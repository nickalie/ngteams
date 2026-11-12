//! A client of the sandbox's private PipeWire for the sandbox's own tests: opens `teams-media`'s PipeWire backend
//! (selecting sandbox reach explicitly) and prints what it measured as `key=value` lines.
//!
//! - `mic <seconds> <hz>...`: the microphone's level at each frequency.
//! - `speaker <seconds> <hz> <amplitude>`: plays a tone.
//! - `camera <frames> <seconds>`: counts the camera's frames.
//! - `echo <hz> <amplitude>`: plays a half-second burst while listening, and reports when and how loud it came back.
//! - `camera-late <milliseconds>`: leaves the camera unread that long, then reports the index of the first frame
//!   read, how many were queued, and the index of the next fresh one.
//! - `ended <seconds>`: opens the microphone, the speaker and the camera, prints `opened=1`, then keeps using them
//!   and reports, for each, whether it said `closed` within that time (the test stops the server meanwhile); then
//!   opens each again and reports how that open failed and how long it took (`reopened_<device>`, `reopen_ms_<device>`).
//!
//! The devices there are and the user's choice ([`choice`]):
//!
//! - `devices`: the devices and each kind's default.
//! - `watch-node <name> <seconds>`: says when the node comes, becomes its kind's default, and goes.
//! - `follow-mic <system hz> <added hz> <seconds>`: the microphone following the system's default as the test adds
//!   and removes a node above the system's.
//! - `switch-mic <name> <system hz> <chosen hz>`: the microphone moved to `name` and back while it runs.
//! - `pinned-mic <chosen> <chosen hz> <added> <added hz> <seconds>`: the microphone on `chosen` (the system's
//!   default) by choice, kept there when the test plugs in `added` above it; then on `added` by choice, falling back
//!   to the system's when the test unplugs it.
//! - `switch-speaker <name> <hz> <amplitude> <seconds each>`: the speaker moved to `name` and back while it plays.
//! - `portal-camera <frames> <seconds>`: the camera through the camera portal on the sandbox's bus.
//! - `portal-screen <frames> <seconds>`: a shared screen through the screen-cast portal on the sandbox's bus.

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use teams_media::devices::file::FrameCounter;
use teams_media::devices::pattern::index_of;
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
use teams_media::devices::{
    AudioFrame, AudioSink, AudioSource, DeviceError, Devices, FRAME_DURATION, VideoFrame, VideoSink,
};
use teams_media::tone::{Tone, ToneGenerator, amplitude_at};

#[path = "choice.rs"]
mod choice;

type ProbeResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Frames the microphone gets to settle before it is measured.
const SETTLE_FRAMES: usize = 20;
const WAIT: Duration = Duration::from_millis(500);

pub(super) fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ui-sandbox-media-probe: {error}");
            ExitCode::FAILURE
        }
    }
}

fn number<T: std::str::FromStr>(args: &[String], at: usize, what: &str) -> ProbeResult<T> {
    args.get(at)
        .and_then(|arg| arg.parse().ok())
        .ok_or_else(|| format!("expected {what} at argument {at}").into())
}

fn run(args: &[String]) -> ProbeResult {
    if args.first().map(String::as_str) == Some("portal-camera") {
        return choice::portal_camera(number(args, 1, "frames")?, number(args, 2, "seconds")?);
    }
    if args.first().map(String::as_str) == Some("portal-screen") {
        return choice::portal_screen(number(args, 1, "frames")?, number(args, 2, "seconds")?);
    }
    let devices = PipeWireDevices::connect(Reach::Sandbox)?;
    match args.first().map(String::as_str) {
        Some("mic") => mic(&devices, number(args, 1, "seconds")?, &args[2..]),
        Some("speaker") => speaker(
            &devices,
            number(args, 1, "seconds")?,
            Tone {
                hz: number(args, 2, "a frequency")?,
                amplitude: number(args, 3, "an amplitude")?,
            },
        ),
        Some("camera") => camera(&devices, number(args, 1, "frames")?, number(args, 2, "seconds")?),
        Some("echo") => echo(
            &devices,
            Tone {
                hz: number(args, 1, "a frequency")?,
                amplitude: number(args, 2, "an amplitude")?,
            },
        ),
        Some("camera-late") => camera_late(&devices, Duration::from_millis(number(args, 1, "milliseconds")?)),
        Some("ended") => ended(&devices, Duration::from_secs(number(args, 1, "seconds")?)),
        Some(_) => choose(&devices, args),
        None => Err(USAGE.into()),
    }
}

const USAGE: &str = "usage: mic <seconds> <hz>... | speaker <seconds> <hz> <amplitude> | camera <frames> <seconds> | echo <hz> <amplitude> | camera-late <milliseconds> | ended <seconds> | devices | watch-node <name> <seconds> | follow-mic <system hz> <added hz> <seconds> | switch-mic <name> <system hz> <chosen hz> | pinned-mic <chosen> <chosen hz> <added> <added hz> <seconds> | switch-speaker <name> <hz> <amplitude> <seconds each> | portal-camera <frames> <seconds>";

/// The commands about the devices there are and the user's choice.
fn choose(devices: &PipeWireDevices, args: &[String]) -> ProbeResult {
    let name = || {
        args.get(1)
            .map(String::as_str)
            .ok_or("expected a node's name at argument 1")
    };
    match args.first().map(String::as_str) {
        Some("devices") => {
            choice::devices(devices);
            Ok(())
        }
        Some("watch-node") => choice::watch_node(devices, name()?, number(args, 2, "seconds")?),
        Some("follow-mic") => choice::follow_mic(
            devices,
            number(args, 1, "a frequency")?,
            number(args, 2, "a frequency")?,
            number(args, 3, "seconds")?,
        ),
        Some("switch-mic") => choice::switch_mic(
            devices,
            name()?,
            number(args, 2, "a frequency")?,
            number(args, 3, "a frequency")?,
        ),
        Some("pinned-mic") => choice::pinned_mic(
            devices,
            (name()?, number(args, 2, "a frequency")?),
            (
                args.get(3)
                    .map(String::as_str)
                    .ok_or("expected a node's name at argument 3")?,
                number(args, 4, "a frequency")?,
            ),
            number(args, 5, "seconds")?,
        ),
        Some("switch-speaker") => choice::switch_speaker(
            devices,
            name()?,
            Tone {
                hz: number(args, 2, "a frequency")?,
                amplitude: number(args, 3, "an amplitude")?,
            },
            Duration::from_secs_f32(number(args, 4, "seconds")?),
        ),
        _ => Err(USAGE.into()),
    }
}

/// Reads `count` microphone frames; fails if they stop coming.
fn listen(microphone: &mut dyn AudioSource, count: usize) -> ProbeResult<Vec<f32>> {
    let mut samples = Vec::new();
    for _ in 0..count {
        let frame = microphone.next_frame(WAIT)?.ok_or("the microphone stopped")?;
        samples.extend_from_slice(frame.samples());
    }
    Ok(samples)
}

fn mic(devices: &PipeWireDevices, seconds: f32, frequencies: &[String]) -> ProbeResult {
    let mut microphone = devices.microphone()?;
    listen(microphone.as_mut(), SETTLE_FRAMES)?;
    let samples = listen(microphone.as_mut(), frames_in(seconds))?;
    println!("frames={}", samples.len() / teams_media::devices::FRAME_SAMPLES);
    for hz in frequencies {
        let level = amplitude_at(&samples, hz.parse()?);
        println!("level_{hz}={level:.4}");
    }
    Ok(())
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a probe's few seconds, at least one frame"
)]
fn frames_in(seconds: f32) -> usize {
    (seconds * 100.0).round().max(1.0) as usize
}

/// Plays `tone` for `seconds` at the device's pace.
fn speaker(devices: &PipeWireDevices, seconds: f32, tone: Tone) -> ProbeResult {
    let mut speaker = devices.speaker()?;
    let played = play(speaker.as_mut(), &mut ToneGenerator::new(tone), frames_in(seconds))?;
    // let the queue drain before the stream closes
    thread::sleep(Duration::from_millis(300));
    println!("played={played}");
    Ok(())
}

/// Plays `count` frames of `generator`, one per frame time.
fn play(speaker: &mut dyn AudioSink, generator: &mut ToneGenerator, count: usize) -> ProbeResult<usize> {
    let start = Instant::now();
    let mut frame = AudioFrame::silence();
    for index in 0..count {
        generator.fill(frame.samples_mut());
        speaker.play(&frame)?;
        let due = start + FRAME_DURATION * u32::try_from(index + 1)?;
        thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    Ok(count)
}

fn camera(devices: &PipeWireDevices, wanted: u64, seconds: u64) -> ProbeResult {
    let mut camera = devices.camera()?;
    let counter = FrameCounter::new();
    let mut sink = counter.clone();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut first = None;
    while counter.counts().frames < wanted && Instant::now() < deadline {
        if let Some(frame) = camera.next_frame(WAIT)? {
            first.get_or_insert((frame.format, frame.width, frame.height));
            sink.show(&frame)?;
        }
    }
    let counts = counter.counts();
    let (format, width, height) = first.ok_or("no frame came")?;
    println!("frames={}", counts.frames);
    println!("changed={}", counts.changed);
    println!("indexed={}", counts.indexed);
    println!("gaps={}", counts.gaps);
    println!("format={}", format.name());
    println!("size={width}x{height}");
    Ok(())
}

/// Plays half a second of silence, half a second of `tone`, then silence, while the microphone listens on this
/// thread; reports the delay from the burst's first frame handed to the speaker to its onset at the microphone, and
/// the burst's level there.
fn echo(devices: &PipeWireDevices, tone: Tone) -> ProbeResult {
    let mut microphone = devices.microphone()?;
    listen(microphone.as_mut(), SETTLE_FRAMES)?;
    let mut speaker = devices.speaker()?;
    let player = thread::spawn(move || -> Result<Instant, String> {
        let mut silence = ToneGenerator::new(Tone {
            hz: tone.hz,
            amplitude: 0.0,
        });
        play(speaker.as_mut(), &mut silence, 50).map_err(|error| error.to_string())?;
        let burst_start = Instant::now();
        play(speaker.as_mut(), &mut ToneGenerator::new(tone), 50).map_err(|error| error.to_string())?;
        play(speaker.as_mut(), &mut silence, 150).map_err(|error| error.to_string())?;
        Ok(burst_start)
    });
    let mut heard = Vec::new();
    for _ in 0..250 {
        let frame = microphone.next_frame(WAIT)?.ok_or("the microphone stopped")?;
        heard.push((Instant::now(), amplitude_at(frame.samples(), tone.hz)));
    }
    let burst_start = player.join().map_err(|_| "the player panicked")??;
    // what the microphone hears at the burst's frequency before the burst (the tone's leakage into a 10 ms window)
    let floor = heard
        .iter()
        .filter(|(at, _)| *at <= burst_start)
        .map(|(_, level)| *level)
        .fold(0.0, f32::max);
    let threshold = (tone.amplitude * 0.05).max(4.0 * floor);
    let onset = heard
        .iter()
        .position(|(at, level)| *at > burst_start && *level > threshold)
        .ok_or("the burst never came back")?;
    let mut levels: Vec<f32> = heard.iter().skip(onset + 5).take(30).map(|(_, level)| *level).collect();
    levels.sort_by(f32::total_cmp);
    let level = levels.get(levels.len() / 2).copied().unwrap_or_default();
    println!("delay_ms={}", heard[onset].0.duration_since(burst_start).as_millis());
    println!("level={level:.4}");
    Ok(())
}

fn index(frame: &VideoFrame) -> ProbeResult<u32> {
    index_of(frame).ok_or_else(|| "a camera frame without its index".into())
}

/// Reads one frame (the stream runs), leaves the camera unread for `wait`, then drains what was queued and reads the
/// next frame to come.
fn camera_late(devices: &PipeWireDevices, wait: Duration) -> ProbeResult {
    let mut camera = devices.camera()?;
    let started = index(&camera.next_frame(Duration::from_secs(5))?.ok_or("no frame came")?)?;
    thread::sleep(wait);
    let mut queued = Vec::new();
    while let Some(frame) = camera.next_frame(Duration::ZERO)? {
        queued.push(index(&frame)?);
    }
    let fresh = index(&camera.next_frame(WAIT)?.ok_or("the camera stopped")?)?;
    println!("started={started}");
    println!("first={}", queued.first().copied().unwrap_or(fresh));
    println!("queued={}", queued.len());
    println!("fresh={fresh}");
    Ok(())
}

/// Keeps the first failure a device reported: `closed`, or another error.
fn verdict<T>(result: Result<T, DeviceError>, said: &mut Option<String>) {
    match result {
        Ok(_) => {}
        Err(DeviceError::Closed) => {
            said.get_or_insert_with(|| "closed".to_owned());
        }
        Err(error) => {
            said.get_or_insert_with(|| format!("error: {error}"));
        }
    }
}

/// Drains `source` without waiting; the first failure goes to `said`.
fn drain<T>(mut next: impl FnMut() -> Result<Option<T>, DeviceError>, said: &mut Option<String>) {
    while said.is_none() {
        match next() {
            Ok(Some(_)) => {}
            Ok(None) => return,
            other => verdict(other, said),
        }
    }
}

/// Opens all three devices, says so, then plays silence and drains the sources until each has failed or `within`
/// is over; prints each one's verdict.
fn ended(devices: &PipeWireDevices, within: Duration) -> ProbeResult {
    let mut microphone = devices.microphone()?;
    let mut speaker = devices.speaker()?;
    let mut camera = devices.camera()?;
    microphone
        .next_frame(Duration::from_secs(5))?
        .ok_or("the microphone gave nothing")?;
    camera
        .next_frame(Duration::from_secs(5))?
        .ok_or("the camera gave nothing")?;
    println!("opened=1");
    let deadline = Instant::now() + within;
    let (mut heard, mut played, mut seen) = (None, None, None);
    let silence = AudioFrame::silence();
    while Instant::now() < deadline && (heard.is_none() || played.is_none() || seen.is_none()) {
        if played.is_none() {
            verdict(speaker.play(&silence), &mut played);
        }
        drain(|| microphone.next_frame(Duration::ZERO), &mut heard);
        drain(|| camera.next_frame(Duration::ZERO), &mut seen);
        thread::sleep(FRAME_DURATION);
    }
    let open = || "open".to_owned();
    println!("microphone={}", heard.unwrap_or_else(open));
    println!("speaker={}", played.unwrap_or_else(open));
    println!("camera={}", seen.unwrap_or_else(open));
    reopen("microphone", || devices.microphone());
    reopen("speaker", || devices.speaker());
    reopen("camera", || devices.camera());
    Ok(())
}

/// How long a device that keeps opening gets to fail (a stream asked for while the loop thread still winds down
/// opens, and says `closed` later).
const REOPEN_WITHIN: Duration = Duration::from_secs(3);

/// Opens a device after its server went away, again while it opens, until an open fails or [`REOPEN_WITHIN`] is
/// over; prints how the failing open ended and how long that one call took.
fn reopen<T>(name: &str, mut open: impl FnMut() -> Result<T, DeviceError>) {
    let deadline = Instant::now() + REOPEN_WITHIN;
    loop {
        let started = Instant::now();
        let result = open();
        let took = started.elapsed();
        if result.is_ok() && Instant::now() < deadline {
            thread::sleep(FRAME_DURATION * 10);
            continue;
        }
        let mut said = None;
        verdict(result, &mut said);
        println!("reopened_{name}={}", said.unwrap_or_else(|| "open".to_owned()));
        println!("reopen_ms_{name}={}", took.as_millis());
        return;
    }
}
