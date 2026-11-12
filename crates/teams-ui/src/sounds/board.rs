//! Playing the app's sounds, on the speaker of the device seam ([`Devices`]): the one the calls use, so a choice of
//! speaker in the settings moves these too.
//!
//! - **Which sound** ([`SoundBoard::clip`]): the event's own pick, else the chosen pack's sound, else the default pack's,
//!   which is made by code and can't fail. So a damaged or missing file never silences anything, a ringing call least
//!   of all; it is logged, without the pack's name.
//! - **A ringtone** ([`SoundPlayer::looped`]) plays on a thread of its own, ahead of real time by at most [`LEAD`],
//!   until it is dropped. It follows the settings as it plays: turned off, it goes quiet; changed, the new sound starts.
//! - **Everything else** plays once, on one worker thread: the newest sound takes the place of one still playing, and
//!   the same event within [`SAME_EVENT`] plays once (a catch-up's messages, a roster's first news).
//! - **A preview** is one more of those, whatever the event's switch says; pressed again it stops. The settings are told
//!   when it ends ([`SoundPlayer::watch_previewing`]).
//!
//! Opening the speaker can wait on the audio server, and reading a pack's file on the disk, so neither happens on the
//! caller's thread. A speaker that can't be opened or goes away ends the sound quietly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use teams_media::devices::{AudioFrame, AudioSink, Devices, FRAME_DURATION, FRAME_SAMPLES};

use super::{MAX_VOLUME, SoundEvent, SoundSource, pack, synth};
use crate::ringtone::{Ringer, Ringtone};
use crate::settings::Settings;

/// How far ahead of real time a sound is queued on the speaker.
pub const LEAD: Duration = Duration::from_millis(100);
/// The same event again within this plays once.
pub const SAME_EVENT: Duration = Duration::from_millis(800);
/// A looping sound shorter than this is padded with silence to it: a short file is no machine gun.
pub const MIN_CYCLE: usize = 96_000;
/// How long the speaker is kept once a sound is queued: dropped sooner, its end would be cut.
const TAIL: Duration = Duration::from_millis(250);

/// A sound's samples at the device's rate.
pub type Clip = Arc<[f32]>;

/// What plays the app's sounds: in the app the [`SoundBoard`]; in a view test a fake that counts.
pub trait SoundPlayer: Send + Sync + 'static {
    /// The settings changed (`packs_dir`: where imported packs are kept, `None` for settings in memory).
    fn apply(&self, settings: &Settings, packs_dir: Option<PathBuf>);
    /// Plays `event`'s sound once, if the settings let it for `account` now (`in_call`: it is in a call).
    fn play(&self, event: SoundEvent, account: &str, in_call: bool);
    /// Plays `event`'s sound over and over until the [`Ringtone`] is dropped, following the settings as it plays.
    fn looped(&self, event: SoundEvent) -> Ringtone;
    /// Previews `event`'s sound as it is now set; one that plays stops first.
    fn preview(&self, event: SoundEvent);
    /// Stops the preview, if one plays.
    fn stop_preview(&self);
    /// The event a preview plays for, if one does.
    fn previewing(&self) -> Option<SoundEvent>;
    /// `tell` is called when a preview starts or ends, on another thread.
    fn watch_previewing(&self, tell: Box<dyn Fn() + Send + Sync>);
}

/// What the worker thread is asked.
enum Job {
    Once(SoundEvent),
    Preview(SoundEvent),
    Stop,
}

/// The settings and sounds as last told.
#[derive(Default)]
struct State {
    settings: Settings,
    packs_dir: Option<PathBuf>,
    /// The sounds made or read so far (every change of the settings starts again).
    clips: HashMap<(SoundSource, SoundEvent), Option<Clip>>,
}

/// What a playing sound takes from the settings.
#[derive(Clone)]
struct Playing {
    clip: Clip,
    gain: f32,
    on: bool,
}

struct Shared {
    devices: Arc<dyn Devices + Send + Sync>,
    state: Mutex<State>,
    /// Counted up on every change of the settings: a ringtone looks again when it moved.
    changes: AtomicU64,
    previewing: Mutex<Option<SoundEvent>>,
    watcher: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    /// `event`'s sound and how it plays now (see the module docs).
    fn playing(&self, event: SoundEvent) -> Playing {
        let (sound, pack, master, packs_dir) = {
            let state = locked(&self.state);
            let sounds = &state.settings.sounds;
            (
                sounds.event(event),
                sounds.pack.clone(),
                sounds.volume,
                state.packs_dir.clone(),
            )
        };
        let default = SoundSource::default();
        let clip = sound
            .sound
            .iter()
            .chain([&pack])
            .find_map(|source| self.clip(source, event, packs_dir.as_ref()))
            .or_else(|| self.clip(&default, event, None))
            .unwrap_or_else(|| Arc::from(synth::render(synth::DEFAULT_PACK, event)));
        Playing {
            clip,
            gain: f32::from(sound.volume) * f32::from(master) / f32::from(MAX_VOLUME) / f32::from(MAX_VOLUME),
            on: sound.on,
        }
    }

    /// `source`'s sound for `event`, made or read once.
    fn clip(&self, source: &SoundSource, event: SoundEvent, packs_dir: Option<&PathBuf>) -> Option<Clip> {
        let key = (source.clone(), event);
        if let Some(known) = locked(&self.state).clips.get(&key) {
            return known.clone();
        }
        let clip: Option<Clip> = match source {
            SoundSource::Builtin(pack) => Some(Arc::from(synth::render(pack, event))),
            SoundSource::Imported(id) => packs_dir
                .and_then(|dir| pack::read_sound(dir, id, event))
                .map(Arc::from),
        };
        locked(&self.state).clips.insert(key, clip.clone());
        clip
    }

    fn set_previewing(&self, event: Option<SoundEvent>) {
        *locked(&self.previewing) = event;
        if let Some(tell) = locked(&self.watcher).as_ref() {
            tell();
        }
    }

    /// Opens the speaker for `what` (the log's word for it); `None` when it can't be (it is told why).
    fn speaker(&self, what: &str) -> Option<Box<dyn AudioSink>> {
        match self.devices.speaker() {
            Ok(speaker) => Some(speaker),
            Err(error) => {
                log::warn!("the {what}'s speaker can't be opened: {error}");
                None
            }
        }
    }
}

/// The frame at `at` of `clip` at `gain`, silence past its end.
fn frame_at(clip: &[f32], at: usize, gain: f32) -> AudioFrame {
    let mut frame = AudioFrame::silence();
    let part = clip.get(at..).unwrap_or(&[]);
    for (out, sample) in frame.samples_mut().iter_mut().zip(part) {
        *out = level(*sample, gain);
    }
    frame
}

/// The frame at `at` of a `cycle` of samples: `clip`, then silence to the cycle's end, over and over. A frame that runs
/// past the cycle's end goes on from its start, so a loop of any length repeats with no gap: a recording's length is
/// seldom a whole number of frames.
fn cycle_frame(clip: &[f32], at: usize, cycle: usize, gain: f32) -> AudioFrame {
    let mut frame = AudioFrame::silence();
    for (index, out) in frame.samples_mut().iter_mut().enumerate() {
        let sample = clip.get((at + index) % cycle.max(1)).copied().unwrap_or(0.0);
        *out = level(sample, gain);
    }
    frame
}

/// `sample` at `gain`, within `[-1, 1]`.
fn level(sample: f32, gain: f32) -> f32 {
    // a file changed behind the app's back may hold anything
    if sample.is_finite() {
        (sample * gain).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// Sleeps until `played` frames since `start`, less [`LEAD`].
fn pace(start: Instant, played: usize) {
    let due = start + FRAME_DURATION * u32::try_from(played).unwrap_or(u32::MAX);
    thread::sleep(due.saturating_duration_since(Instant::now() + LEAD));
}

/// Plays `event` round and round until `stop` is set, following the settings.
fn run_loop(shared: &Shared, event: SoundEvent, stop: &AtomicBool) {
    let Some(mut speaker) = shared.speaker("ringtone") else {
        return;
    };
    let mut seen = shared.changes.load(Ordering::Acquire);
    let mut playing = shared.playing(event);
    let mut cycle = playing.clip.len().max(MIN_CYCLE);
    let (start, mut at) = (Instant::now(), 0);
    for played in 1.. {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let now = shared.changes.load(Ordering::Acquire);
        if now != seen {
            seen = now;
            let next = shared.playing(event);
            if *next.clip != *playing.clip {
                at = 0;
            }
            cycle = next.clip.len().max(MIN_CYCLE);
            playing = next;
        }
        let frame = if playing.on {
            cycle_frame(&playing.clip, at, cycle, playing.gain)
        } else {
            AudioFrame::silence()
        };
        if let Err(error) = speaker.play(&frame) {
            log::warn!("the ringtone's speaker went away: {error}");
            return;
        }
        at = (at + FRAME_SAMPLES) % cycle;
        pace(start, played);
    }
}

/// The next job, if one came within `wait`.
fn job_within(jobs: &mpsc::Receiver<Job>, wait: Duration) -> Option<Job> {
    jobs.recv_timeout(wait).ok()
}

/// Plays `event` once, following its settings; a preview ignores only its switch. A newer job takes its place.
fn play_once(shared: &Shared, event: SoundEvent, preview: bool, jobs: &mpsc::Receiver<Job>) -> Option<Job> {
    let mut speaker = shared.speaker("sound")?;
    let mut seen = shared.changes.load(Ordering::Acquire);
    let mut playing = shared.playing(event);
    let (start, mut at, mut played) = (Instant::now(), 0, 0);
    while at < playing.clip.len() {
        if let Ok(next) = jobs.try_recv() {
            return Some(next);
        }
        let now = shared.changes.load(Ordering::Acquire);
        if now != seen {
            seen = now;
            let next = shared.playing(event);
            if *next.clip != *playing.clip {
                at = 0;
            }
            playing = next;
        }
        if !preview && !playing.on {
            break;
        }
        let frame = frame_at(&playing.clip, at, playing.gain);
        if let Err(error) = speaker.play(&frame) {
            log::warn!("the sound's speaker went away: {error}");
            return None;
        }
        at += FRAME_SAMPLES;
        played += 1;
        pace(start, played);
    }
    job_within(jobs, LEAD + TAIL)
}

/// The worker thread: each job in turn, a newer one in place of one still playing.
fn run_worker(shared: &Shared, jobs: &mpsc::Receiver<Job>) {
    let mut next = None;
    while let Some(job) = next.take().or_else(|| jobs.recv().ok()) {
        match job {
            Job::Once(event) => {
                next = play_once(shared, event, false, jobs);
            }
            Job::Preview(event) => {
                shared.set_previewing(Some(event));
                next = play_once(shared, event, true, jobs);
                if !matches!(next, Some(Job::Preview(_))) {
                    shared.set_previewing(None);
                }
            }
            Job::Stop => shared.set_previewing(None),
        }
    }
}

/// The app's sounds on the speaker of `devices` (see the module docs).
pub struct SoundBoard {
    shared: Arc<Shared>,
    worker: Mutex<Option<mpsc::Sender<Job>>>,
    /// When each event last played once.
    last: Mutex<HashMap<SoundEvent, Instant>>,
}

impl std::fmt::Debug for SoundBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundBoard").finish_non_exhaustive()
    }
}

impl SoundBoard {
    /// The board on `devices`, at the default settings until it is told otherwise.
    #[must_use]
    pub fn new(devices: Arc<dyn Devices + Send + Sync>) -> Self {
        Self {
            shared: Arc::new(Shared {
                devices,
                state: Mutex::default(),
                changes: AtomicU64::new(0),
                previewing: Mutex::new(None),
                watcher: Mutex::new(None),
            }),
            worker: Mutex::new(None),
            last: Mutex::new(HashMap::new()),
        }
    }

    /// Hands `job` to the worker, started the first time.
    fn send(&self, job: Job) {
        let mut worker = locked(&self.worker);
        if worker.is_none() {
            let (sender, jobs) = mpsc::channel();
            let shared = Arc::clone(&self.shared);
            let started = thread::Builder::new()
                .name("teams-ui-sounds".to_owned())
                .spawn(move || run_worker(&shared, &jobs));
            match started {
                Ok(_) => *worker = Some(sender),
                Err(error) => {
                    log::warn!("the sounds can't start: {error}");
                    return;
                }
            }
        }
        if let Some(sender) = worker.as_ref() {
            // a worker gone with the app takes nothing more
            let _ = sender.send(job);
        }
    }
}

impl SoundPlayer for SoundBoard {
    fn apply(&self, settings: &Settings, packs_dir: Option<PathBuf>) {
        let mut state = locked(&self.shared.state);
        state.settings = settings.clone();
        state.packs_dir = packs_dir;
        state.clips.clear();
        self.shared.changes.fetch_add(1, Ordering::AcqRel);
    }

    fn play(&self, event: SoundEvent, account: &str, in_call: bool) {
        if !super::allowed(&locked(&self.shared.state).settings, event, account, in_call) {
            return;
        }
        let now = Instant::now();
        let mut last = locked(&self.last);
        if last.get(&event).is_some_and(|at| now.duration_since(*at) < SAME_EVENT) {
            return;
        }
        last.insert(event, now);
        drop(last);
        self.send(Job::Once(event));
    }

    fn looped(&self, event: SoundEvent) -> Ringtone {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let shared = Arc::clone(&self.shared);
        let started = thread::Builder::new()
            .name("teams-ui-ringtone".to_owned())
            .spawn(move || run_loop(&shared, event, &stopped));
        if let Err(error) = started {
            log::warn!("the ringtone can't start: {error}");
        }
        Ringtone::new(move || stop.store(true, Ordering::Release))
    }

    fn preview(&self, event: SoundEvent) {
        self.send(Job::Preview(event));
    }

    fn stop_preview(&self) {
        self.send(Job::Stop);
    }

    fn previewing(&self) -> Option<SoundEvent> {
        *locked(&self.shared.previewing)
    }

    fn watch_previewing(&self, tell: Box<dyn Fn() + Send + Sync>) {
        *locked(&self.shared.watcher) = Some(tell);
    }
}

impl Ringer for SoundBoard {
    fn start(&self) -> Ringtone {
        self.looped(SoundEvent::IncomingCall)
    }

    fn audible(&self, account: &str, in_call: bool) -> bool {
        super::allowed(
            &locked(&self.shared.state).settings,
            SoundEvent::IncomingCall,
            account,
            in_call,
        )
    }
}

#[cfg(test)]
mod tests {
    use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
    use teams_media::devices::wav;
    use teams_media::tone::amplitude_at;

    use super::*;
    use crate::ringtone::RINGTONE;

    type AfterFrame = Arc<Mutex<Option<Box<dyn FnMut(usize) + Send>>>>;

    /// Records every speaker open and frame, and can change settings exactly between two frames.
    #[derive(Default)]
    struct Heard {
        opens: AtomicU64,
        frames: Arc<Mutex<Vec<AudioFrame>>>,
        after: AfterFrame,
    }

    struct Sink {
        frames: Arc<Mutex<Vec<AudioFrame>>>,
        after: AfterFrame,
    }

    impl AudioSink for Sink {
        fn play(&mut self, frame: &AudioFrame) -> Result<(), teams_media::devices::DeviceError> {
            let count = {
                let mut frames = locked(&self.frames);
                frames.push(frame.clone());
                frames.len()
            };
            if let Some(after) = locked(&self.after).as_mut() {
                after(count);
            }
            Ok(())
        }
    }

    impl Devices for Heard {
        fn speaker(&self) -> Result<Box<dyn AudioSink>, teams_media::devices::DeviceError> {
            self.opens.fetch_add(1, Ordering::Release);
            Ok(Box::new(Sink {
                frames: self.frames.clone(),
                after: self.after.clone(),
            }))
        }

        fn microphone(&self) -> Result<Box<dyn teams_media::devices::AudioSource>, teams_media::devices::DeviceError> {
            Err(teams_media::devices::DeviceError::Closed)
        }

        fn camera(&self) -> Result<Box<dyn teams_media::devices::VideoSource>, teams_media::devices::DeviceError> {
            Err(teams_media::devices::DeviceError::Closed)
        }
    }

    #[test]
    fn a_volume_change_does_not_restart_a_ring() {
        let heard = Arc::new(Heard::default());
        let board = Arc::new(SoundBoard::new(heard.clone()));
        let weak = Arc::downgrade(&board);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        *locked(&heard.after) = Some(Box::new(move |count| {
            if count == 17 {
                let mut settings = Settings::default();
                settings.sounds.volume = 50;
                weak.upgrade().expect("board").apply(&settings, None);
            }
            if count == 30 {
                stopped.store(true, Ordering::Release);
            }
        }));
        run_loop(&board.shared, SoundEvent::IncomingCall, &stop);
        let clip = synth::render(synth::DEFAULT_PACK, SoundEvent::IncomingCall);
        let frames = locked(&heard.frames);
        assert_eq!(frames.len(), 30);
        assert_eq!(frames[17], frame_at(&clip, 17 * FRAME_SAMPLES, 0.5));
        assert_ne!(
            frames[17],
            frame_at(&clip, 0, 0.5),
            "resuming the same clip at its position"
        );
    }

    #[test]
    fn a_loop_that_isnt_whole_frames_wraps_without_a_gap() {
        let clip: Vec<f32> = (0..1_000_u16)
            .map(|index| f32::from(index % 200 + 1) / 1_000.0)
            .collect();
        let played: Vec<f32> = (0..6)
            .flat_map(|frame| {
                cycle_frame(&clip, frame * FRAME_SAMPLES % clip.len(), clip.len(), 1.0)
                    .samples()
                    .to_vec()
            })
            .collect();
        let expected: Vec<f32> = clip.iter().cycle().take(played.len()).copied().collect();
        assert_eq!(played, expected);
    }

    #[test]
    fn a_cycle_longer_than_its_clip_is_silent_after_it() {
        let clip = vec![0.25; 300];
        let frame = cycle_frame(&clip, 0, 400, 2.0);
        assert!(
            frame.samples()[..300]
                .iter()
                .all(|sample| (sample - 0.5).abs() < f32::EPSILON)
        );
        assert!(frame.samples()[300..400].iter().all(|sample| *sample == 0.0));
        assert!(
            frame.samples()[400..]
                .iter()
                .all(|sample| (sample - 0.5).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn one_shots_follow_volume_and_switch_changes_but_previews_ignore_the_switch() {
        for preview in [false, true] {
            let heard = Arc::new(Heard::default());
            let board = Arc::new(SoundBoard::new(heard.clone()));
            let weak = Arc::downgrade(&board);
            *locked(&heard.after) = Some(Box::new(move |count| {
                if count == 5 || count == 10 {
                    let mut settings = Settings::default();
                    settings.sounds.volume = 50;
                    settings
                        .sounds
                        .change_event(SoundEvent::IncomingCall, |sound| sound.on = count != 10);
                    weak.upgrade().expect("board").apply(&settings, None);
                }
            }));
            let (_sender, jobs) = mpsc::channel();
            assert!(play_once(&board.shared, SoundEvent::IncomingCall, preview, &jobs).is_none());
            let frames = locked(&heard.frames);
            let clip = synth::render(synth::DEFAULT_PACK, SoundEvent::IncomingCall);
            assert_eq!(frames[5], frame_at(&clip, 5 * FRAME_SAMPLES, 0.5));
            assert_eq!(
                frames.len(),
                if preview {
                    clip.len().div_ceil(FRAME_SAMPLES)
                } else {
                    10
                }
            );
        }
    }

    fn board(recording: PathBuf) -> SoundBoard {
        let devices = FileDevices {
            input: AudioInput::Silence,
            recording: Some(recording),
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        };
        SoundBoard::new(Arc::new(devices))
    }

    fn waited_for(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_frame_scales_and_cleans_its_samples() {
        let clip = [1.0, f32::NAN, -2.0, 0.5];
        let frame = frame_at(&clip, 0, 0.5);
        assert_eq!(&frame.samples()[..4], &[0.5, 0.0, -1.0, 0.25]);
        assert!(frame.samples()[4..].iter().all(|sample| *sample == 0.0));
        assert!(frame_at(&clip, 900, 1.0).samples().iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn a_missing_sound_falls_back_to_the_pack_then_the_default() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let board = board(dir.path().join("out.wav"));
        let mut settings = Settings::default();
        settings.sounds.pack = SoundSource::Builtin("chimes");
        settings.sounds.change_event(SoundEvent::IncomingCall, |sound| {
            sound.sound = Some(SoundSource::Imported("gone".to_owned()));
        });
        board.apply(&settings, Some(dir.path().to_owned()));
        let clip = board.shared.playing(SoundEvent::IncomingCall).clip;
        assert_eq!(&*clip, &*synth::render("chimes", SoundEvent::IncomingCall));
        settings.sounds.pack = SoundSource::Imported("gone".to_owned());
        board.apply(&settings, Some(dir.path().to_owned()));
        let clip = board.shared.playing(SoundEvent::IncomingCall).clip;
        assert_eq!(&*clip, &*synth::render(synth::DEFAULT_PACK, SoundEvent::IncomingCall));
    }

    #[test]
    fn volume_is_the_events_own() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let board = board(dir.path().join("out.wav"));
        let mut settings = Settings::default();
        settings
            .sounds
            .change_event(SoundEvent::Message, |sound| sound.volume = 25);
        board.apply(&settings, None);
        assert!((board.shared.playing(SoundEvent::Message).gain - 0.25).abs() < 0.001);
        assert!((board.shared.playing(SoundEvent::Mention).gain - 1.0).abs() < 0.001);
        settings.sounds.volume = 50;
        board.apply(&settings, None);
        assert!(
            (board.shared.playing(SoundEvent::Message).gain - 0.125).abs() < 0.001,
            "with the master volume"
        );
    }

    #[test]
    fn a_ringtone_plays_on_the_speaker_until_dropped() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let recording = dir.path().join("ring.wav");
        let board = board(recording.clone());
        let ringtone = board.start();
        assert!(waited_for(
            || wav::read(&recording).is_ok_and(|samples| samples.len() >= 48_000)
        ));
        drop(ringtone);
        let samples = wav::read(&recording).expect("recorded");
        assert!(amplitude_at(&samples[..19_200], RINGTONE[0].hz) > 0.15);
    }

    #[test]
    fn a_sound_plays_once_and_the_same_event_at_once_is_one() {
        let heard = Arc::new(Heard::default());
        let board = SoundBoard::new(heard.clone());
        let told = Arc::new(AtomicBool::new(false));
        let counted = Arc::clone(&told);
        board.watch_previewing(Box::new(move || {
            counted.store(true, Ordering::Release);
        }));
        board.play(SoundEvent::Message, "me", false);
        board.play(SoundEvent::Message, "me", false);
        // A FIFO barrier: Stop is reported only after both play requests have been processed.
        board.stop_preview();
        assert!(waited_for(|| told.load(Ordering::Acquire)));
        assert_eq!(heard.opens.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_sound_turned_off_or_quiet_while_busy_makes_none() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let recording = dir.path().join("off.wav");
        let board = board(recording.clone());
        let mut settings = Settings::default();
        settings
            .sounds
            .change_event(SoundEvent::Message, |sound| sound.on = false);
        settings.sounds.while_busy = false;
        board.apply(&settings, None);
        board.play(SoundEvent::Message, "me", false);
        board.play(SoundEvent::Mention, "me", true);
        assert!(!board.audible("me", true));
        assert!(board.audible("me", false));
        thread::sleep(Duration::from_millis(200));
        assert!(!recording.exists(), "no speaker was opened");
    }

    #[test]
    fn a_preview_plays_whatever_the_switch_says_and_tells_when_it_ends() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let recording = dir.path().join("preview.wav");
        let board = board(recording.clone());
        let told = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&told);
        board.watch_previewing(Box::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        let mut settings = Settings::default();
        settings
            .sounds
            .change_event(SoundEvent::Message, |sound| sound.on = false);
        board.apply(&settings, None);
        board.preview(SoundEvent::Message);
        assert!(waited_for(|| told.load(Ordering::Relaxed) >= 2), "started and ended");
        assert_eq!(board.previewing(), None);
        assert!(wav::read(&recording).is_ok_and(|samples| !samples.is_empty()));
    }

    #[test]
    fn stopping_a_long_preview_ends_it() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let board = board(dir.path().join("stop.wav"));
        board.preview(SoundEvent::IncomingCall);
        assert!(waited_for(|| board.previewing() == Some(SoundEvent::IncomingCall)));
        board.stop_preview();
        let until = Instant::now() + Duration::from_millis(500);
        while board.previewing().is_some() && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(board.previewing().is_none(), "stopped before the two-second clip ends");
    }

    #[test]
    fn a_speaker_that_cant_open_ends_quietly() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let board = board(dir.path().join("gone/out.wav"));
        let told = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&told);
        board.watch_previewing(Box::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        board.preview(SoundEvent::Message);
        assert!(
            waited_for(|| told.load(Ordering::Relaxed) == 2),
            "worker reports start and end"
        );
        assert_eq!(board.previewing(), None);
        // The worker survived the error and can report another preview ending.
        board.preview(SoundEvent::Mention);
        assert!(waited_for(|| told.load(Ordering::Relaxed) == 4));
    }
}
