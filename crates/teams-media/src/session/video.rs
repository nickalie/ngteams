//! The session's video: the camera thread (`teams-media-cam`) and the decoding threads (`teams-media-vid`).
//!
//! - **The camera thread** runs while the camera is on ([`super::MediaSession::set_camera`]): each frame turned into
//!   I420 at the size the call sends ([`crate::video::convert::to_i420`]), its background blurred or replaced as the
//!   choice says at that frame ([`crate::video::effects`]), shown to the self view, and handed to
//!   libwebrtc's video source, which encodes it (an IDR whenever the far end asks) at the rate its bandwidth estimate
//!   allows. A camera that fails (`DeviceClosed(Camera)`) ends the thread and turns the camera off; the rest of the
//!   session goes on.
//! - **The screen thread** runs while the account shares its screen ([`super::MediaSession::share_screen`]): each
//!   frame turned into I420 at most [`SCREEN_MAX`] and handed to the sharing line's source. A screen cast sends a frame
//!   only when the screen changes. For [`SCREEN_SETTLE`] after a change or a keyframe the last picture goes again every
//!   [`SCREEN_REPEAT`], so the encoder refines it; then every [`SCREEN_IDLE_REPEAT`] only. The encoder makes a keyframe
//!   only of a frame it gets: when the far end asks for one (a viewer that joins, the viewers of a share that starts
//!   again, a loss) the last picture goes again at once, and a request that libwebrtc leaves unanswered gets a
//!   keyframe forced; so does each second of the share's first seconds, unasked ([`super::keyframes`]). A screen whose stream ends (the user
//!   stopped it at the desktop) ends the thread, told as `DeviceClosed(Screen)`.
//! - **A decoding thread** runs for each video track the far end sends on: libwebrtc decodes, and each picture goes to
//!   its video line's sink (made the first time the line brings a frame, [`RemoteSinks`]) once the session's video
//!   started; a sharing line's to [`crate::rtc::SHARING_LINE`]'s; a line the negotiated SDP doesn't name (a group
//!   offer's sharing line) shows nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use libwebrtc::rtp_transceiver::RtpTransceiver;
use libwebrtc::video_frame::{VideoFrame as RtcVideoFrame, VideoRotation};
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_stream::native::NativeVideoStream;
use libwebrtc::video_track::RtcVideoTrack;

use super::keyframes::{KEYFRAME_POLL, KeyframeWatch, SenderCounts, StartKeyframes, Step};
use crate::devices::{DeviceError, VideoFrame, VideoSink, VideoSource};
use crate::rtc::{LineMap, Next, Peer, next_within};
use crate::video::codec::EncoderSettings;
use crate::video::convert::{MaxSize, from_buffer, to_buffer, to_i420};
use crate::video::effects::{BackgroundChoice, Effects};

use super::{SessionDevice, SessionEvent, Shared};

/// How long the camera and decoding threads wait for a frame before they check whether to stop.
const FRAME_WAIT: Duration = Duration::from_millis(100);
/// How long a shared screen goes without a new frame before its last picture goes again (see the module docs).
pub const SCREEN_REPEAT: Duration = Duration::from_millis(200);
/// How long after a change or a keyframe the last picture of a shared screen goes again every [`SCREEN_REPEAT`].
pub const SCREEN_SETTLE: Duration = Duration::from_secs(2);
/// How often the last picture of a shared screen that settled goes again: within libwebrtc's receivers' wait for a
/// frame (3 s, after which they ask for a keyframe), as Chrome's screen share repeats a still picture.
pub const SCREEN_IDLE_REPEAT: Duration = Duration::from_secs(1);
/// The largest picture of a screen sent: a bigger screen is scaled down to fit.
pub const SCREEN_MAX: MaxSize = MaxSize {
    width: 1920,
    height: 1080,
};

/// Where the far end's decoded frames (I420) go: the sink of each video line, made the first time the line brings a
/// frame (0 the main video line, 1, 2, … a group call's receiving lines, [`crate::rtc::SHARING_LINE`] a share).
pub type RemoteSinks = Box<dyn FnMut(usize) -> Box<dyn VideoSink> + Send>;

/// Where our camera's frames go as they are sent, handed from one camera thread to the next.
pub(super) type SelfView = Arc<Mutex<Option<Box<dyn VideoSink>>>>;

/// A call's video, as [`super::MediaSession::start_video`] takes it.
pub struct VideoParts {
    /// The bounds the camera is sent within.
    pub encoder: EncoderSettings,
    /// The largest picture sent: a bigger camera frame is scaled down to fit.
    pub max_size: MaxSize,
    /// Where the far end's decoded frames go (I420), by video line.
    pub remote: RemoteSinks,
    /// Where our camera's frames go as they are sent (I420): the self view; `None` for none.
    pub preview: Option<Box<dyn VideoSink>>,
    /// The background the camera's frames are sent with, as chosen at each frame.
    pub background: BackgroundChoice,
}

impl std::fmt::Debug for VideoParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoParts")
            .field("encoder", &self.encoder)
            .field("max_size", &self.max_size)
            .finish_non_exhaustive()
    }
}

/// The video's counts since the session started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoStats {
    /// Frames the camera handed over.
    pub captured: u64,
    /// Frames handed to libwebrtc to encode and send.
    pub sent: u64,
    /// Camera frames that didn't convert, or that libwebrtc didn't take.
    pub unsent: u64,
    /// Pictures libwebrtc decoded.
    pub received: u64,
    /// Pictures shown on a line's sink.
    pub shown: u64,
    /// Frames of our shared screen handed to libwebrtc.
    pub screen_sent: u64,
}

/// The counts and flags the session's threads share.
#[derive(Default)]
pub(super) struct VideoShared {
    pub(super) started: AtomicBool,
    pub(super) camera_on: AtomicBool,
    captured: AtomicU64,
    sent: AtomicU64,
    unsent: AtomicU64,
    received: AtomicU64,
    shown: AtomicU64,
    screen_sent: AtomicU64,
    /// The call's media controller asked for the shared screen: a keyframe is due at the screen thread's next read of
    /// its counts.
    pub(super) screen_asked: AtomicBool,
    /// A shared screen was asked for and none of its frames came since: the first one is logged.
    screen_awaited: AtomicBool,
    /// The sinks of the far end's video, by line, once the video started.
    sinks: Mutex<Option<LineSinks>>,
    /// The decoding threads.
    decoders: Mutex<Vec<JoinHandle<()>>>,
}

/// Each line's sink, made from [`RemoteSinks`] the first time the line brings a frame.
struct LineSinks {
    make: RemoteSinks,
    made: HashMap<usize, Box<dyn VideoSink>>,
}

impl VideoShared {
    pub(super) fn stats(&self) -> VideoStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        VideoStats {
            captured: read(&self.captured),
            sent: read(&self.sent),
            unsent: read(&self.unsent),
            received: read(&self.received),
            shown: read(&self.shown),
            screen_sent: read(&self.screen_sent),
        }
    }

    /// A shared screen was asked for: its first frame is logged when it comes.
    pub(super) fn await_screen(&self) {
        self.screen_awaited.store(true, Ordering::Relaxed);
    }

    /// Whether a frame on `line` is the first of a shared screen asked for: it is taken as come.
    fn first_of_screen(&self, line: usize) -> bool {
        line == crate::rtc::SHARING_LINE && self.screen_awaited.swap(false, Ordering::Relaxed)
    }

    fn count(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Shows `frame` on line `line`'s sink (made now when it is the line's first); whether it was shown.
    fn show(&self, line: usize, frame: &VideoFrame) -> bool {
        let mut sinks = self.sinks.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(sinks) = sinks.as_mut() else {
            return false;
        };
        let LineSinks { make, made } = sinks;
        let sink = made.entry(line).or_insert_with(|| make(line));
        sink.show(frame).is_ok()
    }
}

/// The screen thread: it gives the screen back when it stops, unless the screen's stream ended.
pub(super) type ScreenThread = JoinHandle<Option<Box<dyn VideoSource>>>;

/// The session's camera: what it runs with once the video started, and the camera thread running now.
#[derive(Default)]
pub(super) struct SessionVideo {
    max_size: Option<MaxSize>,
    background: BackgroundChoice,
    preview: SelfView,
    pub(super) camera: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    /// The screen thread, while the account shares its screen.
    pub(super) screen: Option<(Arc<AtomicBool>, ScreenThread)>,
    shared: Option<Arc<Shared>>,
}

impl SessionVideo {
    pub(super) fn start(&mut self, parts: VideoParts, shared: &Arc<Shared>) {
        self.max_size = Some(parts.max_size);
        self.background = parts.background;
        *self.preview.lock().unwrap_or_else(PoisonError::into_inner) = parts.preview;
        *shared.video.sinks.lock().unwrap_or_else(PoisonError::into_inner) = Some(LineSinks {
            make: parts.remote,
            made: HashMap::new(),
        });
        self.shared = Some(Arc::clone(shared));
    }

    /// The largest picture sent, the self view and the background chosen, once the video started.
    pub(super) fn camera_parts(&self) -> Option<(MaxSize, SelfView, BackgroundChoice)> {
        Some((self.max_size?, Arc::clone(&self.preview), self.background.clone()))
    }

    /// Stops the camera thread, if one runs, and waits for it.
    pub(super) fn stop_camera(&mut self) {
        if let Some((stop, handle)) = self.camera.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    }

    /// Stops the screen thread, if one runs, and waits for it.
    pub(super) fn stop_screen(&mut self) {
        drop(self.take_screen());
    }

    /// Stops the screen thread, if one runs, waits for it, and gives back its screen (none once its stream ended).
    pub(super) fn take_screen(&mut self) -> Option<Box<dyn VideoSource>> {
        let (stop, handle) = self.screen.take()?;
        stop.store(true, Ordering::Relaxed);
        handle.join().ok().flatten()
    }

    /// Waits for the decoding threads (the session is stopping).
    pub(super) fn join_decoders(&mut self) {
        if let Some(shared) = &self.shared {
            let decoders = std::mem::take(&mut *shared.video.decoders.lock().unwrap_or_else(PoisonError::into_inner));
            for handle in decoders {
                let _ = handle.join();
            }
        }
    }
}

/// The camera thread.
pub(super) struct CameraLoop {
    pub(super) shared: Arc<Shared>,
    pub(super) peer: Arc<Peer>,
    pub(super) source: NativeVideoSource,
    pub(super) camera: Box<dyn VideoSource>,
    pub(super) max_size: MaxSize,
    pub(super) preview: SelfView,
    pub(super) background: BackgroundChoice,
    pub(super) stop: Arc<AtomicBool>,
}

impl CameraLoop {
    pub(super) fn run(mut self) {
        let epoch = Instant::now();
        let mut effects = Effects::new();
        while !self.stop.load(Ordering::Relaxed) && !self.shared.stopping() {
            let frame = match self.camera.next_frame(FRAME_WAIT) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(error) => {
                    log::warn!("the camera stopped: {error}");
                    self.off();
                    self.shared.report(SessionEvent::DeviceClosed(SessionDevice::Camera));
                    return;
                }
            };
            VideoShared::count(&self.shared.video.captured);
            let Some(mut i420) = to_i420(&frame, self.max_size) else {
                VideoShared::count(&self.shared.video.unsent);
                continue;
            };
            effects.apply(&mut i420, &self.background.get());
            if let Some(preview) = self.preview.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
                let _ = preview.show(&i420);
            }
            let mut sent = RtcVideoFrame::new(VideoRotation::VideoRotation0, to_buffer(&i420));
            sent.timestamp_us = i64::try_from(epoch.elapsed().as_micros()).unwrap_or(i64::MAX);
            if self.source.capture_frame(&sent) {
                VideoShared::count(&self.shared.video.sent);
            } else {
                VideoShared::count(&self.shared.video.unsent);
            }
        }
    }

    /// The camera went away: nothing more is sent on our line.
    fn off(&self) {
        self.shared.video.camera_on.store(false, Ordering::Relaxed);
        self.peer.set_sending(false);
    }
}

/// The screen thread.
pub(super) struct ScreenLoop {
    pub(super) shared: Arc<Shared>,
    pub(super) peer: Arc<Peer>,
    pub(super) source: NativeVideoSource,
    pub(super) screen: Box<dyn VideoSource>,
    pub(super) stop: Arc<AtomicBool>,
}

/// The last picture of the shared screen: since when it settles (the screen changed, or a keyframe of it went), and
/// when it last went.
struct Still {
    picture: VideoFrame,
    settles: Instant,
    sent: Instant,
}

impl Still {
    fn new(picture: VideoFrame) -> Self {
        let now = Instant::now();
        Self {
            picture,
            settles: now,
            sent: now,
        }
    }

    /// Whether the picture goes again now, the screen being still (see the module docs).
    fn repeats(&self) -> bool {
        let every = if self.settles.elapsed() < SCREEN_SETTLE {
            SCREEN_REPEAT
        } else {
            SCREEN_IDLE_REPEAT
        };
        self.sent.elapsed() >= every
    }
}

impl ScreenLoop {
    /// Sends the screen's frames until the thread is stopped; the screen, unless its stream ended.
    pub(super) fn run(mut self) -> Option<Box<dyn VideoSource>> {
        let mut first = true;
        let mut last: Option<Still> = None;
        let mut keyframes = KeyframeWatch::default();
        let mut polled = Instant::now();
        let mut start = StartKeyframes::new(polled);
        // an ask from before this share is for a screen no longer sent: the share's first keyframes come anyway
        self.shared.video.screen_asked.store(false, Ordering::Relaxed);
        while !self.stop.load(Ordering::Relaxed) && !self.shared.stopping() {
            if polled.elapsed() >= KEYFRAME_POLL {
                polled = Instant::now();
                let unasked = start.due(polled);
                self.answer(&mut keyframes, unasked, last.as_mut());
            }
            let frame = match self.screen.next_frame(FRAME_WAIT) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    if let Some(still) = last.as_mut().filter(|still| still.repeats()) {
                        self.send(&still.picture);
                        still.sent = Instant::now();
                    }
                    continue;
                }
                Err(error) => {
                    log::warn!("the shared screen stopped: {error}");
                    self.shared.report(SessionEvent::DeviceClosed(SessionDevice::Screen));
                    return None;
                }
            };
            if std::mem::take(&mut first) {
                log::info!(
                    "the shared screen's first frame: {}x{} {:?}",
                    frame.width,
                    frame.height,
                    frame.format
                );
            }
            let Some(i420) = to_i420(&frame, SCREEN_MAX) else {
                log::debug!("a frame of the shared screen doesn't convert");
                continue;
            };
            self.send(&i420);
            last = Some(Still::new(i420));
        }
        Some(match last {
            Some(still) => Box::new(Resumed {
                last: Some(still.picture),
                screen: self.screen,
            }),
            None => self.screen,
        })
    }

    /// Answers the far end's requests for a keyframe, as the sender's counts show them ([`super::keyframes`]): the last
    /// picture goes again, as a keyframe forced when libwebrtc left a request unanswered, or `unasked` in the share's
    /// first seconds.
    fn answer(&self, watch: &mut KeyframeWatch, unasked: bool, last: Option<&mut Still>) {
        let Some(sender) = self.peer.screen_sender() else {
            return;
        };
        let Some(counts) = SenderCounts::read(&sender) else {
            return;
        };
        if watch.last() != Some(counts) {
            log::debug!(
                "the shared screen's sender: FIR {}, PLI {}; {} keyframes",
                counts.firs,
                counts.plis,
                counts.keyframes
            );
        }
        let mut step = watch.read(counts, Instant::now());
        if self.shared.video.screen_asked.swap(false, Ordering::Relaxed) {
            step = Step::Asked;
        } else if unasked && step == Step::Wait {
            step = Step::Start;
        }
        match step {
            Step::Wait => return,
            Step::Resend => log::debug!(
                "the far end asks for a keyframe of the shared screen: FIR {}, PLI {}; {} keyframes",
                counts.firs,
                counts.plis,
                counts.keyframes
            ),
            Step::Start => {
                log::debug!("a keyframe of the shared screen forced in its first seconds");
                sender.generate_key_frame();
            }
            Step::Asked => {
                log::debug!("the media controller asks for the shared screen: a keyframe forced");
                sender.generate_key_frame();
            }
            Step::Force => {
                log::info!(
                    "a keyframe of the shared screen asked for and not made: one forced (FIR {}, PLI {}; {} keyframes)",
                    counts.firs,
                    counts.plis,
                    counts.keyframes
                );
                sender.generate_key_frame();
            }
        }
        if let Some(still) = last {
            self.send(&still.picture);
            // the keyframe is refined as a change is, for the viewer that asked for it
            still.sent = Instant::now();
            still.settles = still.sent;
        }
    }

    /// Hands `picture` (I420) to the sharing line's source.
    fn send(&self, picture: &VideoFrame) {
        // stamped by the bindings on the wall clock, as the source's black keep-alive frames are: a frame stamped
        // earlier than the last one the encoder saw is dropped, and a group call's sharing line has sent those
        // keep-alive frames since the call began
        let sent = RtcVideoFrame::new(VideoRotation::VideoRotation0, to_buffer(picture));
        if self.source.capture_frame(&sent) {
            VideoShared::count(&self.shared.video.screen_sent);
        }
    }
}

/// A screen that a session stopped sending and gave back: its last picture comes first, so that a screen that holds
/// still goes on at once in the next session, then the screen's own frames.
struct Resumed {
    last: Option<VideoFrame>,
    screen: Box<dyn VideoSource>,
}

impl VideoSource for Resumed {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        if let Some(picture) = self.last.take() {
            return Ok(Some(picture));
        }
        self.screen.next_frame(timeout)
    }
}

/// A decoding thread's parts: the far end's video track and the line it came on.
pub(super) struct Decoder {
    pub(super) shared: Arc<Shared>,
    pub(super) lines: Arc<Mutex<LineMap>>,
    pub(super) track: RtcVideoTrack,
    pub(super) transceiver: RtpTransceiver,
    pub(super) stop: Arc<AtomicBool>,
}

/// Starts a decoding thread for the track `decoder` names.
pub(super) fn spawn_decoder(shared: &Arc<Shared>, decoder: Decoder) {
    match thread::Builder::new()
        .name("teams-media-vid".into())
        .spawn(move || decoder.run())
    {
        Ok(handle) => shared
            .video
            .decoders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle),
        Err(error) => log::warn!("the far end's video can't be shown: {error}"),
    }
}

impl Decoder {
    fn run(self) {
        let mut stream = NativeVideoStream::new(self.track.clone());
        while !self.stop.load(Ordering::Relaxed) && !self.shared.stopping() {
            let frame = match next_within(&mut stream, FRAME_WAIT) {
                Ok(Next::Item(frame)) => frame,
                Ok(Next::Ended) | Err(_) => break,
                Ok(Next::Quiet) => continue,
            };
            VideoShared::count(&self.shared.video.received);
            if !self.shared.video.started.load(Ordering::Relaxed) {
                continue;
            }
            let Some(line) = self
                .transceiver
                .mid()
                .and_then(|mid| Peer::video_line(&self.lines, &mid))
            else {
                continue;
            };
            let Some(picture) = from_buffer(&frame.buffer.to_i420()) else {
                continue;
            };
            if self.shared.video.first_of_screen(line) {
                log::info!(
                    "the first frame of a shared screen came: {}x{}",
                    picture.width,
                    picture.height
                );
            }
            if self.shared.video.show(line, &picture) {
                VideoShared::count(&self.shared.video.shown);
            }
        }
        stream.close();
    }
}
