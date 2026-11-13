//! The session's video: the camera thread (`teams-media-cam`) and the decoding threads (`teams-media-vid`).
//!
//! - **The camera thread** runs while the camera is on ([`super::MediaSession::set_camera`]): each frame turned into
//!   I420 at the size the call sends ([`crate::video::convert::to_i420`]), its background blurred or replaced as the
//!   choice says at that frame ([`crate::video::effects`]), shown to the self view, and handed to
//!   libwebrtc's video source, which encodes it (an IDR whenever the far end asks) at the rate its bandwidth estimate
//!   allows. A camera that fails (`DeviceClosed(Camera)`) ends the thread and turns the camera off; the rest of the
//!   session goes on.
//! - **The screen thread** runs while the account shares its screen ([`super::MediaSession::share_screen`]): each
//!   frame turned into I420 at most [`SCREEN_MAX`] and handed to the sharing line's source. A screen whose stream ends
//!   (the user stopped it at the desktop) ends the thread, told as `DeviceClosed(Screen)`.
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
use libwebrtc::video_frame::{I420Buffer, VideoBuffer as _, VideoFrame as RtcVideoFrame, VideoRotation};
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_stream::native::NativeVideoStream;
use libwebrtc::video_track::RtcVideoTrack;

use crate::devices::{PixelFormat, VideoFrame, VideoSink, VideoSource};
use crate::rtc::{LineMap, Next, Peer, next_within};
use crate::video::codec::EncoderSettings;
use crate::video::convert::{MaxSize, to_i420};
use crate::video::effects::{BackgroundChoice, Effects};

use super::{SessionDevice, SessionEvent, Shared};

/// How long the camera and decoding threads wait for a frame before they check whether to stop.
const FRAME_WAIT: Duration = Duration::from_millis(100);
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

/// The session's camera: what it runs with once the video started, and the camera thread running now.
#[derive(Default)]
pub(super) struct SessionVideo {
    max_size: Option<MaxSize>,
    background: BackgroundChoice,
    preview: SelfView,
    pub(super) camera: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    /// The screen thread, while the account shares its screen.
    pub(super) screen: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
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
        if let Some((stop, handle)) = self.screen.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
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
    pub(super) source: NativeVideoSource,
    pub(super) screen: Box<dyn VideoSource>,
    pub(super) stop: Arc<AtomicBool>,
}

impl ScreenLoop {
    pub(super) fn run(mut self) {
        let mut first = true;
        while !self.stop.load(Ordering::Relaxed) && !self.shared.stopping() {
            let frame = match self.screen.next_frame(FRAME_WAIT) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(error) => {
                    log::warn!("the shared screen stopped: {error}");
                    self.shared.report(SessionEvent::DeviceClosed(SessionDevice::Screen));
                    return;
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
            // stamped by the bindings on the wall clock, as the source's black keep-alive frames are: a frame stamped
            // earlier than the last one the encoder saw is dropped, and a group call's sharing line has sent those
            // keep-alive frames since the call began
            let sent = RtcVideoFrame::new(VideoRotation::VideoRotation0, to_buffer(&i420));
            if self.source.capture_frame(&sent) {
                VideoShared::count(&self.shared.video.screen_sent);
            }
        }
    }
}

/// An I420 frame of ours (each plane's stride its width) as libwebrtc's buffer.
pub(crate) fn to_buffer(frame: &VideoFrame) -> I420Buffer {
    let mut buffer = I420Buffer::new(frame.width, frame.height);
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (width, height) = (to_usize(frame.width), to_usize(frame.height));
    let (chroma_width, chroma_height) = (width / 2, height / 2);
    let luma_size = width * height;
    let (y, u, v) = buffer.data_mut();
    copy_plane(&frame.data, width, y, to_usize(stride_y), width, height);
    copy_plane(
        frame.data.get(luma_size..).unwrap_or_default(),
        chroma_width,
        u,
        to_usize(stride_u),
        chroma_width,
        chroma_height,
    );
    copy_plane(
        frame.data.get(luma_size + luma_size / 4..).unwrap_or_default(),
        chroma_width,
        v,
        to_usize(stride_v),
        chroma_width,
        chroma_height,
    );
    buffer
}

/// Copies `rows` rows of `columns` bytes from `from` (rows `from_stride` apart) to `to` (rows `to_stride` apart).
fn copy_plane(from: &[u8], from_stride: usize, to: &mut [u8], to_stride: usize, columns: usize, rows: usize) {
    for row in 0..rows {
        let (Some(source), Some(target)) = (
            from.get(row * from_stride..row * from_stride + columns),
            to.get_mut(row * to_stride..row * to_stride + columns),
        ) else {
            return;
        };
        target.copy_from_slice(source);
    }
}

/// A decoded picture as our I420 (even sides, each plane's stride its width).
fn from_buffer(buffer: &I420Buffer) -> Option<VideoFrame> {
    let (width, height) = (buffer.width() & !1, buffer.height() & !1);
    if width == 0 || height == 0 {
        return None;
    }
    let (stride_y, stride_u, stride_v) = buffer.strides();
    let (from_y, from_u, from_v) = buffer.data();
    let (columns, rows) = (to_usize(width), to_usize(height));
    let mut data = vec![0_u8; columns * rows * 3 / 2];
    let (luma, chroma) = data.split_at_mut(columns * rows);
    let (u_plane, v_plane) = chroma.split_at_mut(columns * rows / 4);
    let (half_columns, half_rows) = (columns / 2, rows / 2);
    copy_plane(from_y, to_usize(stride_y), luma, columns, columns, rows);
    copy_plane(
        from_u,
        to_usize(stride_u),
        u_plane,
        half_columns,
        half_columns,
        half_rows,
    );
    copy_plane(
        from_v,
        to_usize(stride_v),
        v_plane,
        half_columns,
        half_columns,
        half_rows,
    );
    Some(VideoFrame {
        width,
        height,
        format: PixelFormat::I420,
        stride: width,
        data,
    })
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
            if self.shared.video.show(line, &picture) {
                VideoShared::count(&self.shared.video.shown);
            }
        }
        stream.close();
    }
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_i420_frame_goes_to_libwebrtc_and_back_unchanged() {
        let (width, height) = (8, 4);
        let data: Vec<u8> = (0..width * height * 3 / 2)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let frame = VideoFrame {
            width: 8,
            height: 4,
            format: PixelFormat::I420,
            stride: 8,
            data,
        };
        assert_eq!(from_buffer(&to_buffer(&frame)), Some(frame));
    }
}
