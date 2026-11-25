//! The loopback media peer (the `media` feature, M3 Task 13): the far end of a call's media on loopback, for the
//! calls that have no native client at their other end (the test call's media controller, a caller or a callee the
//! mock plays). It is a `teams-media` session on the file backend relayed through a loopback TURN server
//! (`teams_media::test_turn`), speaking Teams' dialect as the call controller and media controller do: it answers an
//! offer in `sdp-ngc-1.0` and offers in it, and takes the answer to its offer (`teams_media::sdp::NgcSession` keeping
//! the lines' two namings for the call's life). With the feature on, the calling service puts one behind its answers
//! once a test turned its media on ([`crate::ChatServiceMock::start_media`]): the test call's media controller, a
//! caller the mock plays, and the renegotiations of their calls. [`EchoDevices`] make a peer the echo service's
//! stand-in: it plays back what it hears ([`crate::ChatServiceMock::start_echo_media`]).
//!
//! **Video** (M3 Task 18): a peer takes the main video line it is offered and sends its camera (the file backend's
//! test pattern) on it once a video line is in use, its decoded frames counted in its stats; it offers video of its
//! own in a renegotiation ([`LoopbackPeer::video_offer`]), and shares a screen (the file backend's 720p pattern) on a
//! sharing line of its own ([`LoopbackPeer::share_offer`]). Its media is libwebrtc's, as the native client's.

use std::collections::VecDeque;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use teams_media::devices::file::FileDevices;
use teams_media::devices::file::FrameCounter;
use teams_media::devices::{
    AudioFrame, AudioSink, AudioSource, DeviceError, Devices, FRAME_DURATION, FRAME_SAMPLES, VideoSource,
};
use teams_media::processing::ProcessingConfig;
use teams_media::sdp::{NgcSdp, NgcSession, SdpError, VideoLines};
use teams_media::session::{MediaSession, SessionConfig, SessionEvent, SessionStats, VideoParts};
use teams_media::test_turn::TurnServer;
use teams_media::video::codec::EncoderSettings;
use teams_media::video::convert::MaxSize;

/// Why the loopback peer couldn't do it.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error(transparent)]
    Media(#[from] teams_media::Error),
    #[error(transparent)]
    Sdp(#[from] SdpError),
}

/// The far end of a call's media on loopback (see the module docs).
pub struct LoopbackPeer {
    session: MediaSession,
    dialect: NgcSession,
    /// Its camera, until a video line is in use (then it goes to the session).
    camera: Option<Box<dyn VideoSource>>,
}

impl fmt::Debug for LoopbackPeer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackPeer").finish_non_exhaustive()
    }
}

impl LoopbackPeer {
    /// A peer relayed over UDP through `turn`, its media on `devices`, its microphone unprocessed (a test's
    /// steady tone goes out as it is).
    ///
    /// # Errors
    /// [`PeerError::Media`] when the allocation or the session can't be made.
    pub fn start(turn: &TurnServer, devices: &dyn Devices) -> Result<Self, PeerError> {
        Self::start_at(turn.udp, devices)
    }

    /// As [`Self::start`], on the loopback TURN server's UDP port `turn`.
    ///
    /// # Errors
    /// [`PeerError::Media`] when the allocation or the session can't be made.
    pub fn start_at(turn: SocketAddr, devices: &dyn Devices) -> Result<Self, PeerError> {
        let relay = teams_media::test_turn::relay(turn);
        let config = SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        };
        let session = MediaSession::start(&relay, devices, config)?;
        session.start_video(VideoParts {
            encoder: EncoderSettings::default(),
            max_size: MaxSize::HD,
            remote: Box::new(|_| Box::new(FrameCounter::new()) as Box<dyn teams_media::devices::VideoSink>),
            preview: None,
            background: teams_media::video::effects::BackgroundChoice::default(),
        })?;
        let mut dialect = NgcSession::new();
        dialect.set_video(VideoLines::Main { sending: true });
        Ok(Self {
            session,
            dialect,
            camera: devices.camera().ok(),
        })
    }

    /// Answers `offer` (either dialect): its answer in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the offer doesn't convert or the session refuses it.
    pub fn answer(&mut self, offer: &NgcSdp) -> Result<NgcSdp, PeerError> {
        let offer = self.dialect.read_offer(offer)?;
        self.session.set_sharing_lines(self.dialect.sharing_mids());
        let answer = self.session.accept_offer(offer)?;
        let answer = self.dialect.write_answer(&answer)?;
        if answer.carries_video() {
            self.camera_on()?;
        }
        Ok(answer)
    }

    /// Its renegotiation offering its video line (sending its camera), in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the session can't offer or the offer doesn't convert.
    pub fn video_offer(&mut self) -> Result<NgcSdp, PeerError> {
        self.camera_on()?;
        let offer = self
            .session
            .offer_video(true)?
            .ok_or_else(|| teams_media::Error::Rtc("the video line already sends".into()))?;
        Ok(self.dialect.write_offer(&offer)?)
    }

    /// Its renegotiation sharing its screen (`on`: the file backend's screen, a 720p test pattern, on a sharing line of
    /// its own, sending only) or no longer (that line rejected), in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the session can't share or offer (nothing shared to stop), or the offer doesn't convert.
    pub fn share_offer(&mut self, on: bool) -> Result<NgcSdp, PeerError> {
        if !on {
            let offer = self
                .session
                .stop_screen()?
                .ok_or_else(|| teams_media::Error::Rtc("no screen is shared".into()))?;
            return Ok(self.dialect.write_offer(&offer)?);
        }
        let devices = FileDevices {
            input: teams_media::devices::file::AudioInput::Silence,
            recording: None,
            camera: teams_media::devices::file::PatternCamera::default(),
            pacing: teams_media::devices::file::Pacing::RealTime,
        };
        let screen = devices.screen().map_err(teams_media::Error::from)?;
        let (offer, mid) = self.session.share_screen(screen)?;
        self.dialect.mark_sharing(&mid);
        let offer = self.dialect.write_offer(&offer)?;
        self.session.set_sharing_lines(self.dialect.sharing_mids());
        Ok(offer)
    }

    /// Hands its camera to the session, once.
    fn camera_on(&mut self) -> Result<(), PeerError> {
        if let Some(camera) = self.camera.take() {
            self.session.set_camera(Some(camera))?;
        }
        Ok(())
    }

    /// Its offer of the call's audio, in `sdp-ngc-1.0`.
    ///
    /// # Errors
    /// [`PeerError`] when the session can't offer or the offer doesn't convert.
    pub fn offer(&mut self) -> Result<NgcSdp, PeerError> {
        let offer = self.session.offer_audio()?;
        Ok(self.dialect.write_offer(&offer)?)
    }

    /// Its offer of the running call's media again (`MediaSession::reoffer`, its lines as they are), in `sdp-ngc-1.0`: a
    /// renegotiation of its own.
    ///
    /// # Errors
    /// [`PeerError`] when the session can't offer or the offer doesn't convert.
    pub fn reoffer(&mut self) -> Result<NgcSdp, PeerError> {
        let offer = self.session.reoffer()?;
        Ok(self.dialect.write_offer(&offer)?)
    }

    /// Takes the answer to its offer.
    ///
    /// # Errors
    /// [`PeerError`] when the answer doesn't convert or the session refuses it.
    pub fn accept_answer(&mut self, answer: &NgcSdp) -> Result<(), PeerError> {
        let answer = self.dialect.read_answer(answer)?;
        self.session.set_sharing_lines(self.dialect.sharing_mids());
        Ok(self.session.accept_answer(answer)?)
    }

    /// Whether the session reports `wanted` within `wait` (`false` on a final event or when the time is up).
    #[must_use]
    pub fn reports(&self, wanted: &SessionEvent, wait: Duration) -> bool {
        let limit = Instant::now() + wait;
        loop {
            let left = limit.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.session.next_event(left) {
                Some(event) if &event == wanted => return true,
                Some(SessionEvent::Failed(_) | SessionEvent::Closed) => return false,
                _ => {}
            }
        }
    }

    /// The session's counts so far.
    #[must_use]
    pub fn stats(&self) -> SessionStats {
        self.session.stats()
    }

    /// Hangs up and closes the session: its final counts.
    #[must_use]
    pub fn close(self) -> SessionStats {
        self.session.close()
    }
}

/// The echo service's stand-in: a microphone that plays back what the speaker played `delay` earlier, paced as a
/// device is (one 10 ms frame per 10 ms); the speaker also records as `recording`'s does, and the camera is its.
pub struct EchoDevices {
    queue: Arc<Mutex<VecDeque<f32>>>,
    recording: FileDevices,
}

impl fmt::Debug for EchoDevices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EchoDevices").finish_non_exhaustive()
    }
}

impl EchoDevices {
    /// Devices that play back what they hear `delay` later (in whole samples at 48 kHz).
    #[must_use]
    pub fn new(delay: Duration, recording: FileDevices) -> Self {
        let samples = usize::try_from(delay.as_micros() * 48 / 1_000).unwrap_or(usize::MAX);
        Self {
            queue: Arc::new(Mutex::new(VecDeque::from(vec![0.0; samples]))),
            recording,
        }
    }
}

impl Devices for EchoDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        Ok(Box::new(EchoSource {
            queue: Arc::clone(&self.queue),
            start: None,
            handed: 0,
        }))
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        Ok(Box::new(EchoSink {
            queue: Arc::clone(&self.queue),
            recorder: self.recording.speaker()?,
        }))
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.recording.camera()
    }
}

/// What the echo plays: the speaker's samples, `delay` later.
struct EchoSource {
    queue: Arc<Mutex<VecDeque<f32>>>,
    start: Option<Instant>,
    handed: u32,
}

impl AudioSource for EchoSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        let now = Instant::now();
        let due = *self.start.get_or_insert(now) + FRAME_DURATION * self.handed;
        let wait = due.saturating_duration_since(now);
        if wait > timeout {
            thread::sleep(timeout);
            return Ok(None);
        }
        thread::sleep(wait);
        self.handed = self.handed.saturating_add(1);
        let mut frame = AudioFrame::silence();
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.len() >= FRAME_SAMPLES {
            for (sample, heard) in frame.samples_mut().iter_mut().zip(queue.drain(..FRAME_SAMPLES)) {
                *sample = heard;
            }
        }
        Ok(Some(frame))
    }
}

/// The echo's speaker: into the queue its microphone plays back, and recorded.
struct EchoSink {
    queue: Arc<Mutex<VecDeque<f32>>>,
    recorder: Box<dyn AudioSink>,
}

impl AudioSink for EchoSink {
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(frame.samples());
        self.recorder.play(frame)
    }
}
