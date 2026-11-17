//! The speaker under a call's delivery jitter. libwebrtc hands the far end's audio over in bursts, after gaps and late,
//! never at the device's own pace. A paced device of our own (an audio sink that takes its data in real time, as a
//! WASAPI or Core Audio device does, and records where each sample plays) shows what a user hears: a short gap in
//! delivery may give a short silence, but the sound must go on while frames come.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use gstreamer_audio as gst_audio;

use super::*;
use crate::devices::{AudioFrame, FRAME_DURATION, SAMPLE_RATE};
use crate::tone::{Tone, ToneGenerator, purity};

pub(super) const PACED: &str = "paced-speaker ";

/// 10.5 periods in each 10 ms frame: a dropped or repeated frame turns the tone's phase over, and its window is
/// disturbed.
const TONE: Tone = Tone {
    hz: 1050.0,
    amplitude: 0.5,
};
/// 20 ms at 48 kHz.
const WINDOW: usize = 960;
/// How far ahead of its playback the device takes data: a shared-mode WASAPI buffer.
const AHEAD: Duration = Duration::from_millis(20);
/// A write later than this after its due time finds the device run dry: it played silence in between.
const UNDERRUN: Duration = Duration::from_millis(5);
/// A silent run this long (in windows) is a device stall's: a hold in delivery gives less.
const STALL_RUN: usize = 12;
/// How much longer than its stall a stall's silence may be: the windows around it fall where they fall.
const PAST_STALL: Duration = Duration::from_millis(50);
/// Sound after a stall's silence for less than this (in windows) and then silence again is a second gap from the stall.
const AFTER_STALL: usize = 6;
/// How late a frame plays at most once the device runs again: the speaker's 50 ms playout margin, the device's 40 ms
/// buffer and 20 ms ahead, with room. A backlog left from a stall plays each later frame up to 200 ms later.
const LATE: Duration = Duration::from_millis(200);

/// How the device's clock differs from the system's.
#[derive(Clone, Copy, Debug)]
struct Model {
    /// The device's seconds per second of the system clock.
    speed: f64,
    /// The device's thread stops this long, each `every`: a loaded machine's audio thread.
    stall: Duration,
    every: Option<Duration>,
}

#[derive(Default)]
struct Line {
    start: Option<Instant>,
    /// Seconds of audio the device took.
    taken: f64,
    next_stall: Option<Instant>,
    /// Each write: where it plays on the device's own line, in samples from the start, and its samples.
    played: Vec<(usize, Vec<f32>)>,
    /// Each stall as it really was: where the device's line goes on after it, and how long the thread stopped. A
    /// loaded runner's timers make a stop of 300 ms last up to 430 ms.
    stalls: Vec<(usize, Duration)>,
}

impl Line {
    /// The device's real-time line: what it played, silence where nothing was written.
    fn heard(&self) -> Vec<f32> {
        let end = self
            .played
            .iter()
            .map(|(at, samples)| at + samples.len())
            .max()
            .unwrap_or(0);
        let mut heard = vec![0.0; end];
        for (at, samples) in &self.played {
            heard[*at..at + samples.len()].copy_from_slice(samples);
        }
        heard
    }
}

fn lines() -> &'static Mutex<HashMap<String, Arc<Mutex<Line>>>> {
    static LINES: OnceLock<Mutex<HashMap<String, Arc<Mutex<Line>>>>> = OnceLock::new();
    LINES.get_or_init(Mutex::default)
}

mod imp {
    use gst_audio::subclass::prelude::*;
    use gstreamer_audio::gst_base::subclass::prelude::*;

    use super::*;

    #[derive(Default)]
    pub struct Paced {
        pub(super) line: Mutex<Option<(Arc<Mutex<Line>>, Model)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Paced {
        const NAME: &'static str = "NgteamsPacedSpeaker";
        type Type = super::Paced;
        type ParentType = gst_audio::AudioSink;
    }
    impl ObjectImpl for Paced {}
    impl GstObjectImpl for Paced {}
    impl ElementImpl for Paced {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static META: OnceLock<gst::subclass::ElementMetadata> = OnceLock::new();
            Some(META.get_or_init(|| {
                gst::subclass::ElementMetadata::new(
                    "Paced speaker",
                    "Sink/Audio",
                    "A device in real time",
                    "teams-media",
                )
            }))
        }
        fn pad_templates() -> &'static [gst::PadTemplate] {
            static TEMPLATES: OnceLock<Vec<gst::PadTemplate>> = OnceLock::new();
            TEMPLATES.get_or_init(|| {
                let caps = gst_audio::AudioCapsBuilder::new_interleaved()
                    .format(gst_audio::AudioFormat::F32le)
                    .rate(48_000)
                    .channels(1)
                    .build();
                vec![gst::PadTemplate::new("sink", gst::PadDirection::Sink, gst::PadPresence::Always, &caps).unwrap()]
            })
        }
    }
    impl BaseSinkImpl for Paced {}
    impl AudioBaseSinkImpl for Paced {}
    impl AudioSinkImpl for Paced {
        fn open(&self) -> Result<(), gst::LoggableError> {
            Ok(())
        }
        fn prepare(&self, _: &mut gst_audio::AudioRingBufferSpec) -> Result<(), gst::LoggableError> {
            Ok(())
        }
        fn unprepare(&self) -> Result<(), gst::LoggableError> {
            Ok(())
        }
        fn close(&self) -> Result<(), gst::LoggableError> {
            Ok(())
        }
        fn write(&self, data: &[u8]) -> Result<i32, gst::LoggableError> {
            let (line, model) = lock(&self.line).clone().unwrap();
            let samples: Vec<f32> = data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            let mut line = lock(&line);
            let now = Instant::now();
            let start = *line.start.get_or_insert(now);
            let mut stalled = None;
            if let Some(every) = model.every {
                let next = line.next_stall.get_or_insert(start + every);
                if now >= *next {
                    *next += every;
                    wait_until(now + model.stall);
                    stalled = Some(now.elapsed());
                }
            }
            let now = Instant::now();
            if now > start + Duration::from_secs_f64(line.taken / model.speed) + UNDERRUN {
                line.taken = (now - start).as_secs_f64() * model.speed;
            }
            let at = to_samples(line.taken);
            line.stalls.extend(stalled.map(|stall| (at, stall)));
            #[expect(clippy::cast_precision_loss, reason = "a write is a few hundred samples")]
            let taken = samples.len() as f64 / f64::from(SAMPLE_RATE);
            line.played.push((at, samples));
            line.taken += taken;
            let next = start + Duration::from_secs_f64(line.taken / model.speed);
            drop(line);
            if let Some(ready) = next.checked_sub(AHEAD) {
                wait_until(ready);
            }
            Ok(i32::try_from(data.len()).unwrap())
        }
    }
}

glib::wrapper! {
    pub struct Paced(ObjectSubclass<imp::Paced>)
        @extends gst_audio::AudioSink, gst_audio::AudioBaseSink, gst_audio::gst_base::BaseSink, gst::Element, gst::Object;
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a test's line is seconds long"
)]
fn to_samples(seconds: f64) -> usize {
    (seconds * f64::from(SAMPLE_RATE)).round() as usize
}

/// The device of the synthetic entry `label` (`paced-speaker <speed> <stall ms> <every ms> <name>`).
pub(super) fn speaker(label: &str) -> gst::Element {
    let fields: Vec<&str> = label.trim_start_matches(PACED).split(' ').collect();
    let every: u64 = fields[2].parse().unwrap();
    let model = Model {
        speed: fields[0].parse().unwrap(),
        stall: Duration::from_millis(fields[1].parse().unwrap()),
        every: (every > 0).then(|| Duration::from_millis(every)),
    };
    let line = Arc::new(Mutex::new(Line::default()));
    lock(lines()).insert(label.to_owned(), Arc::clone(&line));
    let paced: Paced = glib::Object::builder()
        .property("buffer-time", 40_000_i64)
        .property("latency-time", 10_000_i64)
        .build();
    *lock(&paced.imp().line) = Some((line, model));
    paced.upcast()
}

/// When libwebrtc hands each 10 ms frame over: up to 8 ms late, and now and then held 60-200 ms, then all at once.
fn schedule(frames: u32, seed: u32) -> Vec<Duration> {
    let mut seed = seed;
    let mut next = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(seed >> 8) / f64::from(1_u32 << 24)
    };
    let mut held = Duration::ZERO;
    (0..frames)
        .map(|index| {
            let due = FRAME_DURATION * index;
            if due >= held && next() < 0.004 {
                held = due + Duration::from_millis(60) + Duration::from_millis(140).mul_f64(next());
            }
            (due + Duration::from_millis(8).mul_f64(next())).max(held)
        })
        .collect()
}

/// What a user hears: the windows of the tone after its first second, how many are disturbed (under 30 dB), the
/// longest silent run, how much longer a stall's silence was than the stall, the stalls with a second gap soon after
/// their silence, and how late the last frame played.
struct Heard {
    windows: usize,
    disturbed: usize,
    silent: Duration,
    past_stall: Duration,
    second_gaps: usize,
    late: Duration,
}

/// The line's windows from sample `from` to `to`, against the stalls the device had and the stall its model gives.
fn judge(heard: &[f32], (from, to): (usize, usize), stalls: &[(usize, Duration)], modelled: Duration) -> Heard {
    let fits: Vec<(f32, f32)> = heard
        .get(from..to.min(heard.len()))
        .unwrap_or_default()
        .as_chunks::<WINDOW>()
        .0
        .iter()
        .map(|window| purity(window, TONE.hz))
        .collect();
    let silent: Vec<bool> = fits
        .iter()
        .map(|(amplitude, _)| *amplitude < TONE.amplitude / 4.0)
        .collect();
    let runs = silent_runs(&silent);
    let longest = runs.iter().map(|(_, length)| *length).max().unwrap_or(0);
    let past_stall = runs
        .iter()
        .map(|&(start, length)| {
            // a stall's silence ends where the line goes on after it
            let (first, after) = (from + start * WINDOW, from + (start + length + 1) * WINDOW);
            let stall = stalls
                .iter()
                .filter(|(at, _)| (first..=after).contains(at))
                .map(|(_, stall)| *stall)
                .max()
                .unwrap_or(modelled);
            window_time(length).saturating_sub(stall)
        })
        .max()
        .unwrap_or_default();
    let second_gaps = runs
        .windows(2)
        .filter(|pair| pair[0].1 >= STALL_RUN && pair[1].0 - (pair[0].0 + pair[0].1) < AFTER_STALL)
        .count();
    Heard {
        windows: fits.len(),
        disturbed: fits.iter().filter(|(_, db)| *db < 30.0).count(),
        silent: window_time(longest),
        past_stall,
        second_gaps,
        late: Duration::ZERO,
    }
}

fn window_time(windows: usize) -> Duration {
    Duration::from_millis(20) * u32::try_from(windows).unwrap()
}

/// Each run of silent windows: where it starts and how many windows it holds.
fn silent_runs(silent: &[bool]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (index, quiet) in silent.iter().enumerate() {
        match runs.last_mut() {
            Some((start, length)) if *quiet && *start + *length == index => *length += 1,
            _ if *quiet => runs.push((index, 1)),
            _ => {}
        }
    }
    runs
}

/// How much later than its hand-over the last frame played: where the tone ends on the device's line, against when
/// that frame came.
fn lateness(line: &Line, heard: &[f32], handed_over: Instant) -> Duration {
    let start = line.start.unwrap();
    let end = heard
        .iter()
        .rposition(|sample| sample.abs() > TONE.amplitude / 4.0)
        .map_or(0, |index| index + 1);
    let played = Duration::from_secs_f64(f64::from(u32::try_from(end).unwrap()) / f64::from(SAMPLE_RATE));
    played.saturating_sub(handed_over.saturating_duration_since(start))
}

fn play(model: &str, seconds: u32, seed: u32) -> Heard {
    let label = format!("{PACED}{model} {seed}");
    let mut speaker = devices(vec![device(DeviceKind::Speaker, &label, true)])
        .speaker()
        .unwrap();
    let mut tone = ToneGenerator::new(TONE);
    let start = Instant::now();
    let (mut played, mut heard_at) = (0_u32, start);
    for due in schedule(seconds * 100, seed) {
        loop {
            let now = Instant::now();
            let Some(wait) = (start + due).checked_duration_since(now) else {
                break;
            };
            // the session's playout: silence for the time behind while libwebrtc hands nothing over
            if now - heard_at >= Duration::from_millis(100)
                && start + FRAME_DURATION * played + Duration::from_millis(30) < now
            {
                speaker.play(&AudioFrame::silence()).unwrap();
                played += 1;
                continue;
            }
            wait_until(now + wait.min(FRAME_DURATION));
        }
        let mut frame = AudioFrame::silence();
        tone.fill(frame.samples_mut());
        speaker.play(&frame).unwrap();
        played += 1;
        heard_at = Instant::now();
    }
    thread::sleep(Duration::from_millis(300));
    drop(speaker);
    let line = lock(lines()).remove(&label).unwrap();
    let line = lock(&line);
    let heard = line.heard();
    // the device's line starts when it opens, about when the first frame came
    let modelled = Duration::from_millis(model.split(' ').nth(1).unwrap().parse().unwrap());
    let span = (to_samples(1.0), to_samples(f64::from(seconds)));
    let mut judged = judge(&heard, span, &line.stalls, modelled);
    judged.late = lateness(&line, &heard, heard_at);
    let stalls: Vec<u128> = line.stalls.iter().map(|(_, stall)| stall.as_millis()).collect();
    eprintln!(
        "{model}: {} of {} windows disturbed, longest silence {:?} ({:?} past its stall), stalls {stalls:?} ms, {} \
         second gaps, last frame {:?} late",
        judged.disturbed, judged.windows, judged.silent, judged.past_stall, judged.second_gaps, judged.late
    );
    judged
}

/// A device that keeps time: only a hold in delivery longer than the playout margin is heard, as a short gap.
#[test]
fn a_call_s_jitter_gives_only_short_gaps() {
    let heard = play("1.0 0 0", 20, 7);
    assert!(
        heard.silent <= Duration::from_millis(200),
        "silent for {:?}",
        heard.silent
    );
    assert!(
        heard.disturbed * 12 <= heard.windows,
        "{} of {} windows disturbed",
        heard.disturbed,
        heard.windows
    );
}

/// A device whose thread stops for 300 ms every 2 s, as WASAPI's does in a loaded VM: each stop loses that much sound,
/// no more, and the frames after it play in time again. A speaker slaved to the system clock went silent for seconds
/// here. The run ends 1 s after a stop, so its last frame shows the latency after a stall. Each silence is held against
/// the stop the device really had: a loaded runner's timers make it longer than 300 ms.
#[test]
fn a_device_that_stalls_loses_only_the_stall() {
    let heard = play("1.0 300 2000", 19, 11);
    assert!(
        heard.past_stall <= PAST_STALL,
        "silent for {:?}, {:?} longer than its stall",
        heard.silent,
        heard.past_stall
    );
    assert!(
        heard.disturbed * 10 <= heard.windows * 3,
        "{} of {} windows disturbed",
        heard.disturbed,
        heard.windows
    );
    // a delivery hold or a busy machine can fall just after a stop now and then; a dropped backlog does it each time
    assert!(heard.second_gaps <= 2, "{} stalls with a second gap", heard.second_gaps);
    assert!(heard.late <= LATE, "the last frame played {:?} late", heard.late);
}
