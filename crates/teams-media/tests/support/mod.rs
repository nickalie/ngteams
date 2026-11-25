//! Two media sessions through the loopback TURN server, on the file backend (or devices of a test's own).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, SAMPLE_RATE, VideoSource, wav};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayConfig;
use teams_media::session::{MediaSession, SdpAnswer, SdpOffer, SessionConfig, SessionEvent};
use teams_media::test_turn::TurnServer;
use teams_media::tone::{Tone, amplitude_at, level_at};

/// How long ICE and DTLS may take through the relay on a loaded machine.
pub const CONNECT_WAIT: Duration = Duration::from_secs(20);

pub const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
pub const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};

/// The loopback TURN server `turn` as a session's relay, over UDP.
pub fn relay(turn: &TurnServer) -> RelayConfig {
    teams_media::test_turn::relay(turn.udp)
}

/// The file backend at the device's pace: `tone` in, a recording at `recording` out.
pub fn file_devices(tone: Tone, recording: &Path) -> FileDevices {
    FileDevices {
        input: AudioInput::Tone(tone),
        recording: Some(recording.to_path_buf()),
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    }
}

/// A's offer and B's answer, each carried as text as signaling would carry it.
pub fn negotiate(a: &MediaSession, b: &MediaSession) {
    let offer = a.offer_audio().unwrap();
    let offer = SdpOffer::from_sdp_string(&offer.to_sdp_string()).unwrap();
    assert!(
        offer.to_sdp_string().contains("typ relay"),
        "only the relayed candidate"
    );
    let answer = b.accept_offer(offer).unwrap();
    let answer = SdpAnswer::from_sdp_string(&answer.to_sdp_string()).unwrap();
    a.accept_answer(answer).unwrap();
}

/// Waits for `session` to report `wanted`, failing on any final event or after `wait`.
pub fn wait_for(session: &MediaSession, wanted: &SessionEvent, wait: Duration) {
    let limit = Instant::now() + wait;
    loop {
        let left = limit.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "no {wanted:?} within {wait:?}");
        match session.next_event(left) {
            Some(event) if &event == wanted => return,
            Some(event @ (SessionEvent::Failed(_) | SessionEvent::Closed)) => panic!("{event:?} before {wanted:?}"),
            _ => {}
        }
    }
}

/// A recording and the stretch of it to judge: from `from` to `to` after the speaker started.
pub struct Heard {
    pub samples: Vec<f32>,
    pub from: Duration,
    pub to: Duration,
}

impl Heard {
    pub fn read(path: &Path, from: Duration, to: Duration) -> Self {
        Self {
            samples: wav::read(path).unwrap(),
            from,
            to,
        }
    }

    fn stretch(&self) -> &[f32] {
        let index = |at: Duration| {
            let samples = at.as_micros() * u128::from(SAMPLE_RATE) / 1_000_000;
            usize::try_from(samples).unwrap().min(self.samples.len())
        };
        &self.samples[index(self.from)..index(self.to)]
    }

    /// The level of `hz` over the whole stretch.
    pub fn level(&self, hz: f32) -> f32 {
        amplitude_at(self.stretch(), hz)
    }

    /// The level of `hz` over the stretch's 100 ms windows ([`level_at`]).
    pub fn windowed_level(&self, hz: f32) -> f32 {
        level_at(self.stretch(), hz)
    }

    /// The RMS level of the whole stretch.
    pub fn rms(&self) -> f32 {
        let stretch = self.stretch();
        let sum: f64 = stretch.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        #[expect(clippy::cast_possible_truncation, reason = "a level")]
        let level = (sum / f64::from(u32::try_from(stretch.len().max(1)).unwrap())).sqrt() as f32;
        level
    }

    /// The share of the stretch's 20 ms windows in which `hz` is heard at `level` or more.
    pub fn continuity(&self, hz: f32, level: f32) -> f64 {
        let (windows, _) = self.stretch().as_chunks::<960>();
        assert!(
            windows.len() >= 25,
            "a stretch of {} windows is too short",
            windows.len()
        );
        let heard = windows
            .iter()
            .filter(|window| amplitude_at(window.as_slice(), hz) >= level)
            .count();
        let count = |n: usize| f64::from(u32::try_from(n).unwrap());
        count(heard) / count(windows.len())
    }
}

/// A gate a stuck device waits on until the test opens it.
#[derive(Clone, Default)]
pub struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    pub fn wait(&self) {
        let (open, changed) = &*self.0;
        let guard = open.lock().unwrap_or_else(PoisonError::into_inner);
        let _open = changed
            .wait_while(guard, |open| !*open)
            .unwrap_or_else(PoisonError::into_inner);
    }

    pub fn open(&self) {
        let (open, changed) = &*self.0;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

/// How a test's own device misbehaves.
#[derive(Clone)]
pub enum Fault {
    /// Its first use blocks until the gate opens.
    Stuck(Gate),
    /// It says it was closed.
    Gone,
}

/// The file backend with the microphone or the speaker replaced by a faulty one.
pub struct FaultyDevices {
    pub files: FileDevices,
    pub microphone: Option<Fault>,
    pub speaker: Option<Fault>,
}

struct FaultySource(Fault, bool);

impl AudioSource for FaultySource {
    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        match &self.0 {
            Fault::Stuck(gate) if !self.1 => {
                self.1 = true;
                gate.wait();
                Ok(None)
            }
            Fault::Stuck(_) => Ok(None),
            Fault::Gone => Err(DeviceError::Closed),
        }
    }
}

struct FaultySink(Fault, bool);

impl AudioSink for FaultySink {
    fn play(&mut self, _frame: &AudioFrame) -> Result<(), DeviceError> {
        match &self.0 {
            Fault::Stuck(gate) if !self.1 => {
                self.1 = true;
                gate.wait();
                Ok(())
            }
            Fault::Stuck(_) => Ok(()),
            Fault::Gone => Err(DeviceError::Closed),
        }
    }
}

impl Devices for FaultyDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        match &self.microphone {
            Some(fault) => Ok(Box::new(FaultySource(fault.clone(), false))),
            None => self.files.microphone(),
        }
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        match &self.speaker {
            Some(fault) => Ok(Box::new(FaultySink(fault.clone(), false))),
            None => self.files.speaker(),
        }
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.files.camera()
    }
}

/// The speaker looped back into the microphone: each played frame comes back `delay` after it was played,
/// `attenuation_db` down, and the microphone hears nothing else (an echo path with no one talking at its end). The
/// microphone keeps the speaker's pace: it hands over a frame only once its delay has passed, so the echo canceller
/// hears each frame as the far end `delay` before it hears that frame's echo. A microphone thread that falls
/// [`LINE_LIMIT`] behind loses the oldest frames, as a device's queue would, and the echo path's delay changes there;
/// the line notes when.
pub struct LoopbackDevices {
    line: Arc<Line>,
    gain: f32,
    delay: Duration,
    /// Every frame the speaker played, for the test to read.
    pub played: Arc<Mutex<Vec<AudioFrame>>>,
}

/// The frames on their way from the speaker to the microphone, each with when the microphone may hear it.
#[derive(Default)]
struct Line {
    frames: Mutex<VecDeque<(Instant, AudioFrame)>>,
    arrived: Condvar,
    /// When frames were dropped.
    dropped: Mutex<Vec<Instant>>,
}

/// The most the line holds (1 s).
const LINE_LIMIT: usize = 100;

impl LoopbackDevices {
    pub fn new(delay: Duration, attenuation_db: f32) -> Self {
        Self {
            line: Arc::default(),
            gain: 10.0_f32.powf(-attenuation_db / 20.0),
            delay,
            played: Arc::default(),
        }
    }

    /// The gain the line applies.
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// Frames the line dropped since `since`: each one shortened the echo path.
    pub fn dropped_since(&self, since: Instant) -> usize {
        let dropped = self.line.dropped.lock().unwrap();
        dropped.iter().filter(|at| **at >= since).count()
    }
}

struct LoopSource(Arc<Line>);

impl AudioSource for LoopSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        let line = &*self.0;
        let limit = Instant::now() + timeout;
        let mut frames = line.frames.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            let wake = match frames.front() {
                Some((due, _)) if *due <= now => return Ok(frames.pop_front().map(|(_, frame)| frame)),
                Some((due, _)) => (*due).min(limit),
                None => limit,
            };
            if wake <= now {
                return Ok(None);
            }
            frames = line
                .arrived
                .wait_timeout(frames, wake - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

struct LoopSink {
    line: Arc<Line>,
    gain: f32,
    delay: Duration,
    played: Arc<Mutex<Vec<AudioFrame>>>,
}

impl AudioSink for LoopSink {
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.played
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(frame.clone());
        let mut echo = frame.clone();
        for sample in echo.samples_mut() {
            *sample *= self.gain;
        }
        let mut frames = self.line.frames.lock().unwrap_or_else(PoisonError::into_inner);
        frames.push_back((Instant::now() + self.delay, echo));
        while frames.len() > LINE_LIMIT {
            frames.pop_front();
            self.line
                .dropped
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Instant::now());
        }
        self.line.arrived.notify_all();
        Ok(())
    }
}

impl Devices for LoopbackDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        Ok(Box::new(LoopSource(Arc::clone(&self.line))))
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        Ok(Box::new(LoopSink {
            line: Arc::clone(&self.line),
            gain: self.gain,
            delay: self.delay,
            played: Arc::clone(&self.played),
        }))
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        Err(DeviceError::Closed)
    }
}

/// Where a test's recordings go.
///
/// The process's libwebrtc is warmed up first (its factory's threads take most of a second to start, once): a test
/// takes its time base after naming its recordings, and a recording's samples count from that base.
pub fn recording(dir: &Path, name: &str) -> PathBuf {
    warm();
    dir.join(format!("{name}.wav"))
}

/// Starts the process's libwebrtc, once: a session made and dropped.
pub fn warm() {
    static WARM: std::sync::Once = std::sync::Once::new();
    WARM.call_once(|| {
        let turn = TurnServer::start().unwrap();
        drop(MediaSession::start_without_devices(&relay(&turn), SessionConfig::default()).unwrap());
    });
}

/// Starts a session on `devices` behind its own allocation on `turn`, its microphone unprocessed: these tests judge
/// what the media carries with a steady tone, which the call's noise suppression takes for noise and lowers
/// (`tests/processing.rs` judges the processing, `tests/session.rs`'s
/// `the_session_takes_the_far_end_s_echo_out_of_what_it_sends` the session running it).
pub fn start(turn: &TurnServer, devices: &dyn Devices) -> MediaSession {
    start_with(turn, devices, ProcessingConfig::OFF)
}

/// Starts a session on `devices` behind its own allocation on `turn`, its microphone through `processing`.
pub fn start_with(turn: &TurnServer, devices: &dyn Devices, processing: ProcessingConfig) -> MediaSession {
    let config = SessionConfig {
        processing,
        ..Default::default()
    };
    MediaSession::start(&relay(turn), devices, config).unwrap()
}
