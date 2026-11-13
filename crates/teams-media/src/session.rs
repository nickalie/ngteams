//! A call's media session: the microphone through the audio processing to libwebrtc, libwebrtc's audio to the
//! speaker, and once its video started, the camera to libwebrtc and libwebrtc's video to a sink ([`video`]).
//!
//! libwebrtc ([`crate::rtc`]) carries the media: ICE through the relay, DTLS-SRTP, Opus and H.264, the jitter buffer,
//! bandwidth estimation and A/V sync. The session moves frames between it and the device seam, on threads of its own,
//! none of them the caller's (and never an async runtime's):
//!
//! - **the capture thread** (`teams-media-mic`) reads the microphone, runs each 10 ms frame through the audio
//!   processing (echo cancellation, noise suppression, gain control: [`SessionConfig::processing`]) and hands it to
//!   libwebrtc's audio source (silence while muted);
//! - **the playout thread** (`teams-media-spk`) takes the far end's audio from libwebrtc, 10 ms at a time at
//!   libwebrtc's own pace, and plays it; each frame it hands the speaker is the one it hands the audio processing as the
//!   far end, so the echo canceller hears exactly what is played;
//! - **the event thread** (`teams-media-net`) takes what libwebrtc tells (connected, lost, failed, a track the far end
//!   sends on) and runs a group call's control plane ([`control`]) on the data channel;
//! - **the decoding threads** (`teams-media-vid`, one per video track the far end sends on) show each decoded picture
//!   on its video line's sink, once the session's video started ([`MediaSession::start_video`]);
//! - **the camera thread** (`teams-media-cam`), while the camera is on ([`MediaSession::set_camera`]), hands the
//!   camera's frames to libwebrtc, which encodes and sends them while our video line is negotiated to send;
//! - **the screen thread** (`teams-media-scr`), while the account shares its screen ([`MediaSession::share_screen`]),
//!   hands the screen's frames to libwebrtc on a sharing line of their own (a group call's: on the mixer's sharing
//!   line, [`MediaSession::share_group_screen`]); the far end's share comes on its sharing line, told to the sinks as
//!   [`SHARING_LINE`] ([`MediaSession::set_sharing_lines`]).
//!
//! No thread waits on another's device: a stuck speaker only stops what is played, a stuck microphone only what is
//! captured. A missing audio device is reported independently and never prevents the other streams or the call's
//! connection. Without a microphone nothing is generated, and the session stays muted; without a speaker received
//! audio is decoded but is not counted as played or passed to the echo canceller. Retrying devices preserves already
//! running streams and never unmutes by itself.
//!
//! A session may start without its devices ([`MediaSession::start_without_devices`]): libwebrtc makes and takes the
//! SDP and connects the media, and the capture and playout threads start only once [`MediaSession::open_devices`]
//! attempted its audio devices (a call's devices open only once the call went).
//!
//! SDP crosses as libwebrtc's standard SDP ([`SdpOffer`], [`SdpAnswer`]; `teams_media::sdp` converts it to and from
//! Teams' dialect). [`MediaSession::close`] (or dropping the session) hangs up the peer and joins every thread. The
//! connection failing reports [`SessionEvent::Failed`]; the camera failing (its device gone:
//! [`SessionEvent::DeviceClosed`]) stops only the camera thread and turns the camera off; the session goes on.
//!
//! A group call's media also runs its control plane ([`MediaSession::start_control`], [`control`]): the source
//! requests the caller wants ([`MediaSession::request_sources`]) go to the call's mixer over the data channel, and what
//! the mixer tells comes back as events (the dominant speakers). The levels of what the microphone hears and of what
//! the speaker plays are kept ([`MediaSession::levels`]): who speaks in a 1:1 call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use futures_executor::block_on;
use libwebrtc::audio_frame::AudioFrame as RtcAudioFrame;
use libwebrtc::audio_stream::native::NativeAudioStream;
use libwebrtc::audio_track::RtcAudioTrack;

use crate::Error;
use crate::devices::VideoSource;
use crate::devices::{AudioFrame, AudioSink, AudioSource, Devices, FRAME_DURATION, FRAME_SAMPLES, SAMPLE_RATE};
use crate::processing::{AudioProcessing, ProcessingConfig, from_i16, to_i16};
use crate::relay::RelayConfig;
use crate::rtc::{Next, Peer, PeerEvent, next_within};
pub use crate::rtc::{SHARING_LINE, SdpAnswer, SdpOffer};
use control::Control;
pub use control::SourceRequest;
#[cfg(feature = "test-support")]
pub(crate) use video::to_buffer as to_rtc_buffer;
use video::{CameraLoop, ScreenLoop, VideoShared};
pub use video::{RemoteSinks, VideoParts, VideoStats};

mod control;
mod video;

/// How long the capture thread waits for the microphone before it checks whether to stop.
const MIC_WAIT: Duration = Duration::from_millis(100);
/// How long the event thread waits for an event before it turns the control plane.
const NET_WAIT: Duration = Duration::from_millis(50);
/// Events kept for a caller that doesn't read them; beyond it the newest are dropped.
const EVENT_QUEUE: usize = 64;

/// How the session runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    /// What the microphone's frames go through before they are sent ([`ProcessingConfig::CALL`] by default;
    /// [`ProcessingConfig::OFF`] leaves them as they are).
    pub processing: ProcessingConfig,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            processing: ProcessingConfig::CALL,
        }
    }
}

/// A device of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDevice {
    Microphone,
    Speaker,
    Camera,
    /// The screen being shared (the user stopped it at the desktop, or its stream ended).
    Screen,
}

/// What the session reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// ICE and DTLS are up (again).
    Connected,
    /// ICE lost the far end; it may come back.
    Disconnected,
    /// The far end closed the media (it went away once the media was up). Final.
    Closed,
    /// The connection or the audio processing failed, and the session stopped. Final.
    Failed(String),
    /// A device went away (or failed); its side of the media stopped, the rest goes on.
    DeviceClosed(SessionDevice),
    /// A group call's control plane is up: the mixer answered its handshake, naming what it sends and what it takes.
    ControlActive { send: Vec<String>, receive: Vec<String> },
    /// A source request went: numbered `sequence`, the line by its stream id, the source (`-1` for none).
    SourceRequested { sequence: u64, stream: u32, source: i64 },
    /// The mixer's answer to the source request numbered `sequence`.
    SourceResult { sequence: u64, result: String },
    /// A group call's mixer didn't answer the control plane's handshake: no source request goes.
    ControlTimedOut,
    /// Who spoke last in a group call, as its mixer tells it: the dominant speakers' audio source ids.
    DominantSpeakers(Vec<i64>),
    /// Ephemeral service captions, with debug output redacting recognized speech.
    Captions(Vec<crate::captions::Caption>),
}

/// How loud the call is on each side now: the RMS level (0 to 1) of the last 10 ms the microphone heard (after its
/// processing; 0 while muted) and of the last 10 ms the speaker played.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioLevels {
    pub near: f32,
    pub far: f32,
    /// The last 10 ms the microphone delivered, before its processing, muted or not (0 once it delivered nothing for
    /// 100 ms): whether it hears anything at all (an echo the processing takes out is still here).
    pub captured: f32,
}

/// The RMS level of a frame's samples.
fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
    #[expect(clippy::cast_precision_loss, reason = "a frame's sample count is small")]
    let count = samples.len() as f32;
    (energy / count).sqrt()
}

/// The session's counts since it started, in 10 ms audio frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    /// Frames the microphone delivered.
    pub encoded: u64,
    /// Frames handed to libwebrtc.
    pub sent: u64,
    /// Frames libwebrtc didn't take.
    pub unsent: u64,
    /// Frames of the far end's audio libwebrtc handed over.
    pub received: u64,
    /// Frames of the far end's audio in a shape the speaker doesn't take.
    pub malformed: u64,
    /// Frames handed to the speaker.
    pub frames_played: u64,
    /// The video's counts.
    pub video: VideoStats,
}

#[derive(Default)]
struct Counters {
    encoded: AtomicU64,
    sent: AtomicU64,
    unsent: AtomicU64,
    received: AtomicU64,
    malformed: AtomicU64,
    frames_played: AtomicU64,
}

/// What the threads share.
struct Shared {
    stop: AtomicBool,
    /// The microphone is muted: the capture thread sends silence in its place.
    muted: AtomicBool,
    microphone_missing: AtomicBool,
    speaker_missing: AtomicBool,
    counters: Counters,
    events: SyncSender<SessionEvent>,
    video: VideoShared,
    /// The far end's audio track, once it came.
    far_audio: Mutex<Option<RtcAudioTrack>>,
    /// [`AudioLevels`], each an `f32`'s bits.
    near_level: AtomicU32,
    far_level: AtomicU32,
    captured_level: AtomicU32,
}

impl Shared {
    fn new(events: SyncSender<SessionEvent>) -> Self {
        Self {
            stop: AtomicBool::new(false),
            muted: AtomicBool::new(false),
            microphone_missing: AtomicBool::new(false),
            speaker_missing: AtomicBool::new(false),
            counters: Counters::default(),
            events,
            video: VideoShared::default(),
            far_audio: Mutex::new(None),
            near_level: AtomicU32::new(0),
            far_level: AtomicU32::new(0),
            captured_level: AtomicU32::new(0),
        }
    }

    fn levels(&self) -> AudioLevels {
        AudioLevels {
            near: f32::from_bits(self.near_level.load(Ordering::Relaxed)),
            far: f32::from_bits(self.far_level.load(Ordering::Relaxed)),
            captured: f32::from_bits(self.captured_level.load(Ordering::Relaxed)),
        }
    }

    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Reports `event`; dropped when nobody has read the last [`EVENT_QUEUE`] or the session is gone.
    fn report(&self, event: SessionEvent) {
        match self.events.try_send(event) {
            Ok(()) | Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
        }
    }

    /// The speaker in `slot` failed: emptied and marked missing before it is reported, as the microphone is. The
    /// engine tells [`SessionEvent::DeviceClosed`] only while [`Session::speaker_missing`] holds, so a report read
    /// before the mark would be dropped.
    fn speaker_lost(&self, slot: &mut Option<Box<dyn AudioSink>>) {
        *slot = None;
        self.speaker_missing.store(true, Ordering::Relaxed);
        self.report(SessionEvent::DeviceClosed(SessionDevice::Speaker));
    }

    /// Reports that the session failed with `why` and stops it: the other threads end at their next turn.
    fn fail(&self, why: String) {
        self.report(SessionEvent::Failed(why));
        self.stop.store(true, Ordering::Relaxed);
    }

    fn count(counter: &AtomicU64, by: u64) {
        counter.fetch_add(by, Ordering::Relaxed);
    }

    fn stats(&self) -> SessionStats {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        let counters = &self.counters;
        SessionStats {
            encoded: read(&counters.encoded),
            sent: read(&counters.sent),
            unsent: read(&counters.unsent),
            received: read(&counters.received),
            malformed: read(&counters.malformed),
            frames_played: read(&counters.frames_played),
            video: self.video.stats(),
        }
    }

    fn far_audio(&self) -> MutexGuard<'_, Option<RtcAudioTrack>> {
        self.far_audio.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Audio devices open independently: a missing microphone must not discard the speaker or end the call.
struct AudioDevices {
    processing: Option<Arc<AudioProcessing>>,
    microphone: bool,
    playout: bool,
    speaker: Arc<Mutex<Option<Box<dyn AudioSink>>>>,
}

/// What the event thread takes: libwebrtc's events and the caller's control plane commands.
enum Input {
    Peer(PeerEvent),
    StartControl(Option<u32>),
    RequestSources(Vec<SourceRequest>),
}

/// A running media session.
pub struct MediaSession {
    shared: Arc<Shared>,
    peer: Arc<Peer>,
    inputs: Sender<Input>,
    /// Behind a lock so a session is shared between threads (`Sync`): an engine asks it from one thread and runs its
    /// SDP steps from another.
    events: Mutex<Receiver<SessionEvent>>,
    /// Behind a lock so the devices open on a shared session ([`MediaSession::open_devices`]).
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// Which audio streams run and the shared speaker slot; missing devices can be retried.
    audio_devices: Mutex<AudioDevices>,
    /// The camera running now, and what it runs with once the video started.
    video: Mutex<video::SessionVideo>,
    /// One SDP step at a time: libwebrtc takes its descriptions in the order they are applied.
    negotiation: Mutex<()>,
    /// The call's health as last read, for the next read's rates ([`MediaSession::health`]).
    health: Mutex<crate::health::HealthReader>,
}

impl MediaSession {
    /// Opens the microphone and the speaker of `devices` and starts the session relayed through `relay`: as
    /// [`MediaSession::start_without_devices`], then [`MediaSession::open_devices`]. Nothing is sent until the SDP is
    /// exchanged and the media connects.
    ///
    /// # Errors
    /// As [`MediaSession::start_without_devices`] and [`MediaSession::open_devices`].
    pub fn start(relay: &RelayConfig, devices: &dyn Devices, config: SessionConfig) -> Result<Self, Error> {
        let session = Self::start_without_devices(relay, config)?;
        session.open_devices(devices)?;
        Ok(session)
    }

    /// Starts the session relayed through `relay` with no device open: its peer makes and takes the SDP and connects
    /// the media, and [`MediaSession::open_devices`] later starts what the devices carry.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc refuses the peer, [`Error::Processing`] when the audio processing can't be set
    /// up, [`Error::Io`] when a thread can't be spawned.
    pub fn start_without_devices(relay: &RelayConfig, config: SessionConfig) -> Result<Self, Error> {
        let processing = if config.processing.any() {
            Some(Arc::new(AudioProcessing::new(config.processing)?))
        } else {
            None
        };
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_QUEUE);
        let shared = Arc::new(Shared::new(event_tx));
        let (inputs, input_rx) = mpsc::channel();
        let peer_inputs = Mutex::new(inputs.clone());
        let events: crate::rtc::Events = Arc::new(move |event| {
            let _ = peer_inputs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(Input::Peer(event));
        });
        let peer = Arc::new(Peer::new(relay, events)?);
        let net = Network {
            shared: Arc::clone(&shared),
            peer: Arc::clone(&peer),
            inputs: input_rx,
            control: None,
            captions: crate::captions::Receiver::default(),
            decoding: HashMap::new(),
        };
        let handle = thread::Builder::new()
            .name("teams-media-net".into())
            .spawn(move || net.run())?;
        Ok(Self {
            shared,
            peer,
            inputs,
            events: Mutex::new(event_rx),
            threads: Mutex::new(vec![handle]),
            audio_devices: Mutex::new(AudioDevices {
                processing,
                microphone: false,
                playout: false,
                speaker: Arc::new(Mutex::new(None)),
            }),
            video: Mutex::new(video::SessionVideo::default()),
            negotiation: Mutex::new(()),
            health: Mutex::new(crate::health::HealthReader::default()),
        })
    }

    /// Opens each available audio device, retaining the call when either cannot open. Missing devices are reported
    /// as [`SessionEvent::DeviceClosed`]; a missing microphone stays muted and produces no substitute audio.
    /// Calling again retries missing devices without duplicating running streams.
    ///
    /// # Errors
    /// [`Error::Io`] when a thread can't be spawned.
    pub fn open_devices(&self, devices: &dyn Devices) -> Result<(), Error> {
        let mut audio_devices = self.audio_devices.lock().unwrap_or_else(PoisonError::into_inner);
        if self.shared.stopping() {
            return Ok(());
        }
        if !audio_devices.microphone || self.microphone_missing() {
            self.open_microphone(devices, &mut audio_devices)?;
        }
        self.open_speaker(devices, &audio_devices);
        let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
        if !audio_devices.playout && !self.shared.stopping() {
            let playout = Playout {
                shared: Arc::clone(&self.shared),
                speaker: Arc::clone(&audio_devices.speaker),
                processing: audio_devices.processing.clone(),
            };
            threads.push(
                thread::Builder::new()
                    .name("teams-media-spk".into())
                    .spawn(move || playout.run())?,
            );
            audio_devices.playout = true;
        }
        Ok(())
    }

    /// Reopens a missing speaker without opening a microphone or changing its mute state.
    pub fn retry_speaker(&self, devices: &dyn Devices) {
        let audio_devices = self.audio_devices.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.shared.stopping() {
            self.open_speaker(devices, &audio_devices);
        }
    }

    /// Whether playback needs a device retry; querying this never waits on a native device open.
    #[must_use]
    pub fn speaker_missing(&self) -> bool {
        self.shared.speaker_missing.load(Ordering::Relaxed)
    }

    fn open_speaker(&self, devices: &dyn Devices, audio_devices: &AudioDevices) {
        let missing_speaker = audio_devices
            .speaker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none();
        if missing_speaker {
            // A native open can wait. Keep the playout lock free so incoming audio continues to be decoded.
            match devices.speaker() {
                Ok(opened) => {
                    // Publish the slot and its status together. Playout can fail immediately after taking the
                    // slot; an unlocked status store could otherwise overwrite that newer missing indication.
                    let mut speaker = audio_devices.speaker.lock().unwrap_or_else(PoisonError::into_inner);
                    *speaker = Some(opened);
                    self.shared.speaker_missing.store(false, Ordering::Relaxed);
                }
                Err(error) => {
                    if !self.shared.speaker_missing.swap(true, Ordering::Relaxed) {
                        log::warn!("the speaker could not open: {error}");
                        self.shared.report(SessionEvent::DeviceClosed(SessionDevice::Speaker));
                    }
                }
            }
        }
    }

    fn open_microphone(&self, devices: &dyn Devices, audio_devices: &mut AudioDevices) -> Result<(), Error> {
        let microphone = match devices.microphone() {
            Ok(microphone) => microphone,
            Err(error) => {
                log::warn!("the microphone could not open: {error}");
                self.shared.muted.store(true, Ordering::Relaxed);
                if !self.shared.microphone_missing.swap(true, Ordering::Relaxed) {
                    self.shared
                        .report(SessionEvent::DeviceClosed(SessionDevice::Microphone));
                }
                return Ok(());
            }
        };
        let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
        if self.shared.stopping() {
            return Ok(());
        }
        let capture = Capture {
            shared: Arc::clone(&self.shared),
            peer: Arc::clone(&self.peer),
            microphone,
            processing: audio_devices.processing.clone(),
        };
        self.shared.microphone_missing.store(false, Ordering::Relaxed);
        threads.push(
            thread::Builder::new()
                .name("teams-media-mic".into())
                .spawn(move || capture.run())?,
        );
        audio_devices.microphone = true;
        Ok(())
    }

    fn negotiating(&self) -> MutexGuard<'_, ()> {
        self.negotiation.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Our offer of one send-and-receive audio line (a 1:1 call's start). Pass the answer to
    /// [`MediaSession::accept_answer`].
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc makes no offer, or the relay allocated nothing.
    pub fn offer_audio(&self) -> Result<SdpOffer, Error> {
        let _step = self.negotiating();
        self.peer.offer_audio()
    }

    /// Offers the session again, its lines as they are.
    ///
    /// # Errors
    /// As [`MediaSession::offer_audio`].
    pub fn reoffer(&self) -> Result<SdpOffer, Error> {
        let _step = self.negotiating();
        self.peer.offer()
    }

    /// A group call's offer, as the web client makes it: audio, the main video line, `slots` receiving video lines,
    /// the sharing line and the data channel.
    ///
    /// # Errors
    /// As [`MediaSession::offer_audio`].
    pub fn offer_group(&self, slots: usize) -> Result<SdpOffer, Error> {
        let _step = self.negotiating();
        self.peer.offer_group(slots)
    }

    /// Starts a group call's control plane, sending as `source` (the data line's stream id, when the far end named
    /// one): the data channel opened on the far end's data line when our offer didn't make it, `syn` sent once it is
    /// open.
    ///
    /// # Errors
    /// [`Error::Rtc`] when the data channel can't be made, [`Error::SessionEnded`] once the session stopped.
    pub fn start_control(&self, source: Option<u32>) -> Result<(), Error> {
        self.peer.open_channel()?;
        self.inputs
            .send(Input::StartControl(source))
            .map_err(|_| Error::SessionEnded)
    }

    /// The sources the caller wants on the receiving lines now; each line's change goes to the mixer once its control
    /// plane is up.
    ///
    /// # Errors
    /// [`Error::SessionEnded`] once the session stopped.
    pub fn request_sources(&self, wanted: Vec<SourceRequest>) -> Result<(), Error> {
        self.inputs
            .send(Input::RequestSources(wanted))
            .map_err(|_| Error::SessionEnded)
    }

    /// How loud the call is on each side now.
    #[must_use]
    pub fn levels(&self) -> AudioLevels {
        self.shared.levels()
    }

    /// Starts the session's video: the far end's video shown on `parts.remote`'s sinks, the camera sent as
    /// `parts.encoder` says once it is on.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc's video can't start.
    pub fn start_video(&self, parts: VideoParts) -> Result<(), Error> {
        self.peer.start_video(parts.encoder)?;
        self.shared.video.started.store(true, Ordering::Relaxed);
        self.video
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .start(parts, &self.shared);
        Ok(())
    }

    /// Turns the camera on (`Some`: its frames sent from now on) or off (`None`); the line's direction goes by the SDP
    /// the caller offers next.
    ///
    /// # Errors
    /// [`Error::Video`] when the video didn't start, [`Error::Io`] when the camera thread can't be spawned.
    pub fn set_camera(&self, camera: Option<Box<dyn VideoSource>>) -> Result<(), Error> {
        let mut video = self.video.lock().unwrap_or_else(PoisonError::into_inner);
        video.stop_camera();
        let Some(camera) = camera else {
            self.peer.set_sending(false);
            self.shared.video.camera_on.store(false, Ordering::Relaxed);
            return Ok(());
        };
        let Some(source) = self.peer.video_source() else {
            return Err(Error::Video("the session's video didn't start".into()));
        };
        let (max_size, preview, background) = video
            .camera_parts()
            .ok_or_else(|| Error::Video("the session's video didn't start".into()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let camera = CameraLoop {
            shared: Arc::clone(&self.shared),
            peer: Arc::clone(&self.peer),
            source,
            camera,
            max_size,
            preview,
            background,
            stop: Arc::clone(&stop),
        };
        // on before the thread runs: a camera that fails at once turns it off again from there
        self.shared.video.camera_on.store(true, Ordering::Relaxed);
        self.peer.set_sending(true);
        let handle = match thread::Builder::new()
            .name("teams-media-cam".into())
            .spawn(move || camera.run())
        {
            Ok(handle) => handle,
            Err(error) => {
                self.shared.video.camera_on.store(false, Ordering::Relaxed);
                self.peer.set_sending(false);
                return Err(error.into());
            }
        };
        video.camera = Some((stop, handle));
        Ok(())
    }

    /// The call's sharing lines by libwebrtc's mid, as the dialect names them (`crate::sdp::NgcSession::sharing_mids`):
    /// their frames go to the sinks as [`SHARING_LINE`], none of them is a camera's line. Set before the SDP that names
    /// them is applied.
    pub fn set_sharing_lines(&self, mids: Vec<String>) {
        self.peer.set_sharing_lines(mids);
    }

    /// Shares `screen`: its frames sent on a line of its own (sending only), with our offer adding it and that line's
    /// mid (for the dialect: `crate::sdp::NgcSession::mark_sharing`). Pass the answer to
    /// [`MediaSession::accept_answer`].
    ///
    /// # Errors
    /// [`Error::Video`] when the video didn't start, [`Error::Rtc`] when libwebrtc refuses the line or makes no offer,
    /// [`Error::Io`] when the screen thread can't be spawned.
    pub fn share_screen(&self, screen: Box<dyn VideoSource>) -> Result<(SdpOffer, String), Error> {
        if self.peer.video_source().is_none() {
            return Err(Error::Video("the session's video didn't start".into()));
        }
        let _step = self.negotiating();
        let source = self.peer.share_screen()?;
        let mut video = self.video.lock().unwrap_or_else(PoisonError::into_inner);
        video.stop_screen();
        let stop = Arc::new(AtomicBool::new(false));
        let looped = ScreenLoop {
            shared: Arc::clone(&self.shared),
            source,
            screen,
            stop: Arc::clone(&stop),
        };
        let handle = match thread::Builder::new()
            .name("teams-media-scr".into())
            .spawn(move || looped.run())
        {
            Ok(handle) => handle,
            Err(error) => {
                self.peer.stop_screen();
                return Err(error.into());
            }
        };
        video.screen = Some((stop, handle));
        drop(video);
        let offer = self.peer.offer()?;
        let mid = self
            .peer
            .screen_mid()
            .ok_or_else(|| Error::Rtc("the screen's line has no mid".into()))?;
        Ok((offer, mid))
    }

    /// Names a group call's sharing line (libwebrtc's mid, the dialect's `crate::sdp::NgcSession::sharing_slot`) before
    /// the mixer's offer of it is answered: the answer sends on it too, so the account's screen can go out on it
    /// ([`MediaSession::share_group_screen`]). `None` for a 1:1 call.
    pub fn set_group_sharing(&self, mid: Option<String>) {
        self.peer.set_group_sharing(mid);
    }

    /// Shares `screen` in a group call: its frames sent on the call's sharing line, which already goes both ways;
    /// nothing is negotiated (the call's services are told by its media descriptions).
    ///
    /// # Errors
    /// [`Error::Rtc`] when the call has no sharing line that sends, [`Error::Io`] when the screen thread can't be
    /// spawned.
    pub fn share_group_screen(&self, screen: Box<dyn VideoSource>) -> Result<(), Error> {
        let source = self
            .peer
            .group_screen_source()
            .ok_or_else(|| Error::Rtc("the group call's sharing line doesn't send".into()))?;
        let mut video = self.video.lock().unwrap_or_else(PoisonError::into_inner);
        video.stop_screen();
        let stop = Arc::new(AtomicBool::new(false));
        let looped = ScreenLoop {
            shared: Arc::clone(&self.shared),
            source,
            screen,
            stop: Arc::clone(&stop),
        };
        let handle = thread::Builder::new()
            .name("teams-media-scr".into())
            .spawn(move || looped.run())?;
        video.screen = Some((stop, handle));
        self.peer.set_group_screen_sending(true);
        Ok(())
    }

    /// Stops sharing the screen in a group call: its thread stopped, the sharing line's encoding off (the line still
    /// receives). Whether it was shared.
    pub fn stop_group_screen(&self) -> bool {
        self.video.lock().unwrap_or_else(PoisonError::into_inner).stop_screen();
        let sharing = self.peer.group_screen_sending();
        self.peer.set_group_screen_sending(false);
        sharing
    }

    /// Stops sharing the screen: its thread stopped and our offer rejecting its line; `None` when none was shared.
    ///
    /// # Errors
    /// As [`MediaSession::offer_audio`].
    pub fn stop_screen(&self) -> Result<Option<SdpOffer>, Error> {
        self.video.lock().unwrap_or_else(PoisonError::into_inner).stop_screen();
        let _step = self.negotiating();
        if !self.peer.stop_screen() {
            return Ok(None);
        }
        self.peer.offer().map(Some)
    }

    /// Whether the account shares its screen.
    #[must_use]
    pub fn sharing(&self) -> bool {
        self.peer.screen_mid().is_some() || self.peer.group_screen_sending()
    }

    /// Whether the camera is on.
    #[must_use]
    pub fn camera_on(&self) -> bool {
        self.shared.video.camera_on.load(Ordering::Relaxed)
    }

    /// Our offer with the main video line on (added when the call has none), for the camera turned on or off
    /// (`sending`: the dialect writes the line's direction as the camera goes).
    ///
    /// # Errors
    /// As [`MediaSession::offer_audio`]; [`Error::Video`] when the video didn't start.
    pub fn offer_video(&self, sending: bool) -> Result<Option<SdpOffer>, Error> {
        if self.peer.video_source().is_none() {
            return Err(Error::Video("the session's video didn't start".into()));
        }
        let _step = self.negotiating();
        self.peer.set_sending(sending && self.camera_on());
        self.peer.offer_video().map(Some)
    }

    /// Mutes or unmutes the microphone: muted, silence is sent in its place.
    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed) || self.microphone_missing()
    }

    /// The microphone failed to open or stopped. An explicit unmute can retry it through [`Self::open_devices`].
    #[must_use]
    pub fn microphone_missing(&self) -> bool {
        self.shared.microphone_missing.load(Ordering::Relaxed)
    }

    /// Applies the far end's answer to our latest offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc refuses it.
    pub fn accept_answer(&self, answer: SdpAnswer) -> Result<(), Error> {
        let _step = self.negotiating();
        self.peer.accept_answer(answer)
    }

    /// Answers the far end's offer.
    ///
    /// # Errors
    /// [`Error::Rtc`] when libwebrtc refuses it, or the relay allocated nothing.
    pub fn accept_offer(&self, offer: SdpOffer) -> Result<SdpAnswer, Error> {
        let _step = self.negotiating();
        self.peer.accept_offer(offer)
    }

    /// The next event, waiting at most `timeout` (`Duration::ZERO`: only one already there).
    pub fn next_event(&self, timeout: Duration) -> Option<SessionEvent> {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        if timeout.is_zero() {
            events.try_recv().ok()
        } else {
            events.recv_timeout(timeout).ok()
        }
    }

    /// The call's health now, as libwebrtc measures it (Teams' Call health): its rates against the last time it was
    /// asked. Waits for libwebrtc on the asking thread; nothing is sent. Statistics libwebrtc can't read show nothing.
    #[must_use]
    pub fn health(&self) -> crate::health::CallHealth {
        let stats = self.peer.stats().unwrap_or_else(|error| {
            log::debug!("the call's statistics can't be read: {error}");
            Vec::new()
        });
        let lines = self.peer.sharing_lines();
        self.health
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .read(&stats, &lines, Instant::now())
    }

    #[must_use]
    pub fn stats(&self) -> SessionStats {
        self.shared.stats()
    }

    /// Hangs up and joins every thread; the counts as they ended.
    pub fn close(mut self) -> SessionStats {
        self.shut_down();
        self.shared.stats()
    }

    fn shut_down(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        {
            let mut video = self.video.lock().unwrap_or_else(PoisonError::into_inner);
            video.stop_camera();
            video.stop_screen();
        }
        self.peer.close();
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        for handle in threads {
            let _ = handle.join();
        }
        self.video
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .join_decoders();
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        self.shut_down();
    }
}

/// The event thread: libwebrtc's events turned into the session's, a group call's control plane turned.
struct Network {
    shared: Arc<Shared>,
    peer: Arc<Peer>,
    inputs: Receiver<Input>,
    control: Option<Control>,
    captions: crate::captions::Receiver,
    /// Each video line's decoder stop flag; a replacement track retires its previous decoder.
    decoding: HashMap<String, Arc<AtomicBool>>,
}

impl Network {
    fn run(mut self) {
        while !self.shared.stopping() {
            match self.inputs.recv_timeout(NET_WAIT) {
                Ok(input) => self.take(input),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if let Some(control) = self.control.as_mut() {
                control.turn(Instant::now(), &self.peer, &self.shared);
            }
        }
    }

    fn take(&mut self, input: Input) {
        match input {
            Input::Peer(event) => self.peer_event(event),
            Input::StartControl(source) => {
                let mut control = Control::new(source);
                control.turn(Instant::now(), &self.peer, &self.shared);
                self.control = Some(control);
            }
            Input::RequestSources(wanted) => {
                if let Some(control) = self.control.as_mut() {
                    control.request(wanted, &self.peer, &self.shared);
                }
            }
        }
    }

    fn peer_event(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::Connected => self.shared.report(SessionEvent::Connected),
            PeerEvent::Disconnected => self.shared.report(SessionEvent::Disconnected),
            PeerEvent::Failed(why) => self.shared.fail(why),
            PeerEvent::Closed => self.shared.report(SessionEvent::Closed),
            PeerEvent::Audio(track) => {
                let mut far = self.shared.far_audio();
                if far.is_none() {
                    *far = Some(track);
                }
            }
            PeerEvent::Video(track, transceiver) => {
                // A rejected m-line can return with a new track before its old stream ends.
                let line = transceiver.mid().unwrap_or_else(|| track.id());
                let stop = Arc::new(AtomicBool::new(false));
                if let Some(previous) = self.decoding.insert(line, Arc::clone(&stop)) {
                    previous.store(true, Ordering::Relaxed);
                }
                let decoder = video::Decoder {
                    shared: Arc::clone(&self.shared),
                    lines: self.peer.lines(),
                    track,
                    transceiver,
                    stop,
                };
                video::spawn_decoder(&self.shared, decoder);
            }
            PeerEvent::ChannelOpen => {
                if let Some(control) = self.control.as_mut() {
                    control.turn(Instant::now(), &self.peer, &self.shared);
                }
            }
            PeerEvent::ChannelMessage(data) => {
                let captions = self.captions.receive(&data);
                if !captions.is_empty() {
                    self.shared.report(SessionEvent::Captions(captions));
                }
                if let Some(control) = self.control.as_mut() {
                    control.receive(&data, &self.peer, &self.shared);
                }
            }
        }
    }
}

/// The capture thread: the microphone through the processing to libwebrtc's audio source.
struct Capture {
    shared: Arc<Shared>,
    peer: Arc<Peer>,
    microphone: Box<dyn AudioSource>,
    processing: Option<Arc<AudioProcessing>>,
}

impl Capture {
    fn run(mut self) {
        let mut quiet_since = Instant::now();
        while !self.shared.stopping() {
            let mut frame = match self.microphone.next_frame(MIC_WAIT) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    if quiet_since.elapsed() >= MIC_WAIT {
                        self.shared.captured_level.store(0, Ordering::Relaxed);
                    }
                    continue;
                }
                Err(error) => {
                    log::warn!("the microphone stopped: {error}");
                    self.shared.microphone_missing.store(true, Ordering::Relaxed);
                    self.shared.muted.store(true, Ordering::Relaxed);
                    self.shared
                        .report(SessionEvent::DeviceClosed(SessionDevice::Microphone));
                    return;
                }
            };
            quiet_since = Instant::now();
            Shared::count(&self.shared.counters.encoded, 1);
            self.shared
                .captured_level
                .store(rms(frame.samples()).to_bits(), Ordering::Relaxed);
            if let Some(processing) = &self.processing
                && let Err(error) = processing.near_end(&mut frame)
            {
                self.shared.fail(error.to_string());
                return;
            }
            if self.shared.muted.load(Ordering::Relaxed) {
                frame = AudioFrame::silence();
            }
            self.shared
                .near_level
                .store(rms(frame.samples()).to_bits(), Ordering::Relaxed);
            let samples = to_i16(&frame);
            let sent = RtcAudioFrame {
                data: samples.as_slice().into(),
                sample_rate: SAMPLE_RATE,
                num_channels: 1,
                samples_per_channel: u32::try_from(FRAME_SAMPLES).unwrap_or(u32::MAX),
            };
            match block_on(self.peer.audio().capture_frame(&sent)) {
                Ok(()) => Shared::count(&self.shared.counters.sent, 1),
                Err(_) => Shared::count(&self.shared.counters.unsent, 1),
            }
        }
    }
}

/// How far the speaker may fall behind its clock before silence fills the gap: libwebrtc's own playout runs every
/// 10 ms, a little late at times.
const PLAYOUT_SLACK: Duration = Duration::from_millis(30);
/// How long libwebrtc may hand nothing over before the speaker plays silence in its place: its playout runs every
/// 10 ms once a far end's audio flows, so only its start (or a stall) is this quiet.
const PLAYOUT_QUIET: Duration = Duration::from_millis(100);

/// The speaker's clock: how many frames it played since it started, and when libwebrtc last handed one over.
struct Pace {
    start: Instant,
    played: u32,
    heard: Option<Instant>,
}

impl Pace {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            played: 0,
            heard: None,
        }
    }

    fn played(&mut self) {
        self.played = self.played.saturating_add(1);
    }

    /// libwebrtc handed a frame over now.
    fn heard(&mut self) {
        self.heard = Some(Instant::now());
    }

    /// Whether libwebrtc has handed nothing over for [`PLAYOUT_QUIET`] (or never).
    fn quiet(&self) -> bool {
        self.heard.is_none_or(|heard| heard.elapsed() >= PLAYOUT_QUIET)
    }

    /// Whether the speaker played less than its clock asks, by more than [`PLAYOUT_SLACK`].
    fn behind(&self) -> bool {
        self.start + FRAME_DURATION * self.played + PLAYOUT_SLACK < Instant::now()
    }
}

/// The playout thread: libwebrtc's audio of the far end to the speaker, and to the processing as the far end.
struct Playout {
    shared: Arc<Shared>,
    speaker: Arc<Mutex<Option<Box<dyn AudioSink>>>>,
    processing: Option<Arc<AudioProcessing>>,
}

impl Playout {
    fn run(mut self) {
        let rate = i32::try_from(SAMPLE_RATE).unwrap_or(i32::MAX);
        let mut pace = Pace::new();
        let mut stream: Option<NativeAudioStream> = None;
        while !self.shared.stopping() {
            // the far end's audio track comes once its SDP was applied
            if stream.is_none()
                && let Some(track) = self.shared.far_audio().clone()
            {
                stream = Some(NativeAudioStream::new(track, rate, 1));
            }
            let received = match stream.as_mut().map(|stream| next_within(stream, FRAME_DURATION)) {
                Some(Ok(Next::Item(received))) => Some(received),
                Some(Ok(Next::Ended)) => break,
                Some(Ok(Next::Quiet)) => None,
                Some(Err(error)) => {
                    self.shared.fail(error.to_string());
                    break;
                }
                None => {
                    thread::sleep(FRAME_DURATION);
                    None
                }
            };
            if let Some(received) = received {
                pace.heard();
                if !self.play_received(&received, &mut pace) {
                    return;
                }
            }
            // the speaker keeps its pace while libwebrtc hands nothing over (before the far end's track, while the
            // media connects): silence for the time that fell behind, so a speaker that went away is noticed too
            while pace.quiet() && pace.behind() {
                if !self.play(&AudioFrame::silence()) {
                    return;
                }
                pace.played();
            }
        }
        if let Some(mut stream) = stream {
            stream.close();
        }
    }

    /// Plays what libwebrtc handed over, 10 ms at a time; whether the speaker is still there.
    fn play_received(&mut self, received: &RtcAudioFrame<'_>, pace: &mut Pace) -> bool {
        Shared::count(&self.shared.counters.received, 1);
        for samples in received.data.chunks(FRAME_SAMPLES) {
            let floats: Vec<f32> = samples.iter().map(|&sample| from_i16(sample)).collect();
            let Some(frame) = AudioFrame::from_samples(&floats) else {
                Shared::count(&self.shared.counters.malformed, 1);
                continue;
            };
            if !self.play(&frame) {
                return false;
            }
            pace.played();
        }
        true
    }

    /// Plays one frame; whether the speaker is still there.
    fn play(&mut self, frame: &AudioFrame) -> bool {
        self.shared
            .far_level
            .store(rms(frame.samples()).to_bits(), Ordering::Relaxed);
        let mut speaker = self.speaker.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(output) = speaker.as_mut() else {
            // Decode received audio even without an output device; nothing is claimed as played or sent to AEC.
            return true;
        };
        if let Some(processing) = &self.processing
            && let Err(error) = processing.far_end(frame)
        {
            self.shared.fail(error.to_string());
            return false;
        }
        if let Err(error) = output.play(frame) {
            log::warn!("the speaker stopped: {error}");
            self.shared.speaker_lost(&mut speaker);
            return true;
        }
        Shared::count(&self.shared.counters.frames_played, 1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lost speaker whose drop checks whether its loss was reported before the speaker was marked missing.
    struct Watched {
        shared: Arc<Shared>,
        events: Arc<Mutex<Receiver<SessionEvent>>>,
        early: Arc<AtomicBool>,
    }

    impl AudioSink for Watched {
        fn play(&mut self, _: &AudioFrame) -> Result<(), crate::devices::DeviceError> {
            Err(crate::devices::DeviceError::Closed)
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            let reported = self
                .events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .try_recv()
                .is_ok();
            if reported && !self.shared.speaker_missing.load(Ordering::Relaxed) {
                self.early.store(true, Ordering::Relaxed);
            }
        }
    }

    #[test]
    fn a_lost_speaker_is_marked_missing_before_it_is_reported() {
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE);
        let shared = Arc::new(Shared::new(sender));
        let events = Arc::new(Mutex::new(receiver));
        let early = Arc::new(AtomicBool::new(false));
        let mut slot: Option<Box<dyn AudioSink>> = Some(Box::new(Watched {
            shared: Arc::clone(&shared),
            events: Arc::clone(&events),
            early: Arc::clone(&early),
        }));
        shared.speaker_lost(&mut slot);
        assert!(slot.is_none());
        assert!(
            !early.load(Ordering::Relaxed),
            "reported before it was marked missing: the engine would drop it"
        );
        let told = events.lock().unwrap_or_else(PoisonError::into_inner).try_recv();
        assert_eq!(told, Ok(SessionEvent::DeviceClosed(SessionDevice::Speaker)));
        assert!(shared.speaker_missing.load(Ordering::Relaxed));
    }

    #[test]
    fn a_frame_level_is_its_rms() {
        assert!((rms(&[0.5; 480]) - 0.5).abs() < 1e-6);
        assert!(rms(&[]).abs() < f32::EPSILON);
    }
}
