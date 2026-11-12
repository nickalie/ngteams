//! The sandbox's private PipeWire: its tone at the microphone, its recorded speaker, its camera and its loopback,
//! each through `teams-media`'s PipeWire backend in a client process of the sandbox (`ui-sandbox-media-probe`); a
//! camera left unread handing over its newest frames; every device saying it is closed once the server goes away,
//! and opening one then failing at once with `Closed`;
//! and a run with canaries planted on every PipeWire, Pulse and bus address of the session it inherits, which must
//! stay silent.
//!
//! Without `pipewire` and `wireplumber` the tests say so and pass, unless `GPUI_SMOKE_REQUIRE=1`. Nothing here plays
//! aloud or records a real device: the server is a private one with no device plugin to open.

#![cfg(target_os = "linux")]
// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use teams_media::devices::DeviceError;
use teams_media::devices::pipewire::{PipeWireDevices, QUEUED_VIDEO, Reach, SANDBOX_CLIENT_CONF, SANDBOX_SOCKET};
use teams_media::devices::wav;
use teams_media::tone::{Tone, ToneGenerator, amplitude_at, decibels};
use tempfile::TempDir;
use ui_sandbox::pipewire::nodes::NodeSpec;
use ui_sandbox::pipewire::timeline::now_us;
use ui_sandbox::pipewire::{DEVICE_PRIORITY, Echo, PipeWireOptions, PipeWireTools, PrivatePipeWire};
use ui_sandbox::{
    LineError, Running, SANDBOX_ENV, SandboxOptions, SandboxResult, clean_command, required, spawn, unavailable,
};

const PROBE: &str = env!("CARGO_BIN_EXE_ui-sandbox-media-probe");
/// How long one probe run may take (a few seconds of audio, a connect, a cold start under load).
const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// The child run of [`the_session_s_pipewire_pulse_and_bus_are_never_reached`], and the variable naming its canary
/// dir.
const CANARY_CHILD: &str = "a_private_pipewire_with_canaries_planted";
const CANARY_ENV: &str = "UI_SANDBOX_PIPEWIRE_CANARY_DIR";
const TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.25,
};
const ECHO: Echo = Echo {
    delay: Duration::from_millis(250),
    attenuation_db: 12.0,
};

/// A private PipeWire in a temp dir of its own; stopped before the dir goes.
struct Private {
    pipewire: PrivatePipeWire,
    _dir: TempDir,
}

fn retain_synthetic_recording(pipewire: &PrivatePipeWire) {
    if let Some(dir) = std::env::var_os("UI_SANDBOX_AUDIO_DIAGNOSTICS") {
        let dir = PathBuf::from(dir);
        let _ = fs::create_dir_all(&dir);
        let _ = fs::copy(pipewire.recording(), dir.join("synthetic-speaker.wav"));
        let _ = fs::write(dir.join("private-pipewire.log"), pipewire.logs());
    }
}

fn private_or_skip(test: &str, options: &PipeWireOptions) -> Option<Private> {
    let tools = match PipeWireTools::find() {
        Ok(tools) => tools,
        Err(reason) => {
            unavailable::<()>(test, &reason, required()).unwrap();
            return None;
        }
    };
    let dir = TempDir::new().unwrap();
    let pipewire = PrivatePipeWire::start(&tools, dir.path(), options).unwrap_or_else(|error| panic!("{error}"));
    Some(Private { pipewire, _dir: dir })
}

/// Runs the probe with `env` and `args`; its `key=value` lines.
fn probe(env: &[(&str, OsString)], args: &[&str]) -> SandboxResult<HashMap<String, String>> {
    probe_values(start_probe(env, args)?, args)
}

/// Starts the probe with `env` and `args`.
fn start_probe(env: &[(&str, OsString)], args: &[&str]) -> SandboxResult<Running> {
    let mut command = clean_command(PROBE);
    command
        .args(args)
        .envs(env.iter().map(|(name, value)| (*name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn::pass_only(&mut command, &[]);
    Running::new(command.spawn()?)
}

/// The running probe's `key=value` lines from here to its exit, which must be a success.
fn probe_values(mut running: Running, args: &[&str]) -> SandboxResult<HashMap<String, String>> {
    let mut values = HashMap::new();
    loop {
        match running.line(PROBE_DEADLINE) {
            Ok(line) => {
                if let Some((key, value)) = line.split_once('=') {
                    values.insert(key.to_owned(), value.to_owned());
                }
            }
            Err(LineError::Closed) => break,
            Err(LineError::Timeout) => return Err(running.fail(&format!("probe {args:?} gave no output in time"))),
        }
    }
    let (status, stderr) = running.finish(PROBE_DEADLINE)?;
    if !status.success() {
        return Err(format!("probe {args:?}: {status}\n{stderr}").into());
    }
    Ok(values)
}

fn value<T: std::str::FromStr>(values: &HashMap<String, String>, key: &str) -> T {
    values
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {values:?}"))
}

#[test]
fn missing_pipewire_tools_are_named() {
    let dir = TempDir::new().unwrap();
    let error = PipeWireTools::find_on(dir.path().as_os_str(), &[], &[]).unwrap_err();
    assert!(error.contains("pipewire"), "{error}");
    for name in ["pipewire", "wireplumber"] {
        let path = dir.path().join(name);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let libs = dir.path().join("lib");
    let error = PipeWireTools::find_on(dir.path().as_os_str(), &[libs.to_str().unwrap()], &[]).unwrap_err();
    assert!(error.contains("spa-0.2"), "{error}");
}

#[test]
fn the_private_pipewire_hears_a_tone_records_the_speaker_shows_a_camera_and_loops_back() {
    let options = PipeWireOptions {
        tone: TONE,
        echo: Some(ECHO),
        ..PipeWireOptions::default()
    };
    let Some(private) = private_or_skip("the_private_pipewire", &options) else {
        return;
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_everything(&private.pipewire))) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            retain_synthetic_recording(&private.pipewire);
            panic!("{error}{}", private.pipewire.logs());
        }
        Err(panic) => {
            retain_synthetic_recording(&private.pipewire);
            std::panic::resume_unwind(panic);
        }
    }
    // a failure's message carries both servers' logs
    let logs = private.pipewire.logs();
    assert!(
        logs.contains("--- pipewire.log\n") && logs.contains("--- wireplumber.log\n"),
        "{logs}"
    );
    let pids = private.pipewire.pids();
    drop(private);
    for pid in pids {
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "{pid} still runs");
    }
}

fn check_everything(pipewire: &PrivatePipeWire) -> SandboxResult {
    let env = pipewire.client_env();
    // the microphone: the tone, nothing at 1 kHz (the echo carries only what the speaker plays)
    let heard = probe(&env, &["mic", "1", "440", "1000"])?;
    let tone: f32 = value(&heard, "level_440");
    assert!((tone - TONE.amplitude).abs() < 0.03, "{heard:?}");
    assert!(value::<f32>(&heard, "level_1000") < 0.02, "{heard:?}");

    // the camera: the moving test pattern, every frame carrying its index
    let seen = probe(&env, &["camera", "20", "20"])?;
    let frames: u64 = value(&seen, "frames");
    assert!(frames >= 20, "{seen:?}");
    assert!(
        value::<u64>(&seen, "changed") + 1 >= frames,
        "the pattern moves: {seen:?}"
    );
    assert_eq!(value::<u64>(&seen, "indexed"), frames, "{seen:?}");
    assert!(value::<u64>(&seen, "gaps") * 10 <= frames, "{seen:?}");
    assert_eq!(seen["format"], "I420");
    assert_eq!(seen["size"], "640x360");
    assert!(pipewire.camera_frames() >= frames);
    check_late_camera(&env)?;

    // the speaker: what is played lands in the recording
    probe(&env, &["speaker", "1", "1000", "0.5"])?;
    let recorded = recording_level(pipewire, 1_000.0)?;
    assert!((recorded - 0.5).abs() < 0.05, "recorded at {recorded}");

    // the loopback: a burst played comes back at the microphone after the echo's delay, attenuated by its dB
    let echoed = probe(&env, &["echo", "1000", "0.5"])?;
    eprintln!("the loopback: {echoed:?}");
    let delay: u64 = value(&echoed, "delay_ms");
    let level = decibels(value(&echoed, "level"), 0.5);
    assert!((250..750).contains(&delay), "{echoed:?}");
    assert!((level + ECHO.attenuation_db).abs() < 2.0, "{level} dB: {echoed:?}");
    Ok(())
}

/// The camera left unread for a second (15 frames) hands over its newest frames, not the first ones it queued.
fn check_late_camera(env: &[(&str, OsString)]) -> SandboxResult {
    let late = probe(env, &["camera-late", "1000"])?;
    let (started, first, fresh): (u64, u64, u64) =
        (value(&late, "started"), value(&late, "first"), value(&late, "fresh"));
    assert!(value::<usize>(&late, "queued") <= QUEUED_VIDEO, "{late:?}");
    assert!(
        first >= started + 10,
        "the first frame read after the wait is a recent one: {late:?}"
    );
    assert!(
        fresh - first <= u64::try_from(QUEUED_VIDEO)? + 1,
        "no lasting lag: {late:?}"
    );
    Ok(())
}

/// The loudest complete half-second's mean local tone level, once the header holds the last second played.
fn recording_level(pipewire: &PrivatePipeWire, hz: f32) -> SandboxResult<f32> {
    std::thread::sleep(Duration::from_millis(1_500));
    let samples = wav::read(&pipewire.recording())?;
    if std::env::var_os("UI_SANDBOX_AUDIO_DIAGNOSTICS").is_some() {
        eprintln!(
            "synthetic recording: samples={}, half-second levels={:?}",
            samples.len(),
            samples
                .chunks(24_000)
                .map(|window| amplitude_at(window, hz))
                .collect::<Vec<_>>()
        );
    }
    Ok(strongest_recorded_tone(&samples, hz))
}

/// Keep the full 500 ms amplitude requirement, counting silence in its average. A private graph may insert a few
/// silent samples when the probe's independently paced writer briefly underruns. Those samples shift phase, so a
/// coherent Fourier measurement over the whole 500 ms cancels an otherwise full-level tone. Measure its component
/// in each 10 ms device frame, then average every frame in the same complete half-second; a fragment cannot pass.
fn strongest_recorded_tone(samples: &[f32], hz: f32) -> f32 {
    samples
        .as_chunks::<24_000>()
        .0
        .iter()
        .map(|window| {
            window
                .as_chunks::<480>()
                .0
                .iter()
                .map(|frame| amplitude_at(frame, hz))
                .sum::<f32>()
                / 50.0
        })
        .fold(0.0, f32::max)
}

#[test]
fn the_recorded_level_counts_every_frame_without_cancelling_a_tone_after_a_tiny_silent_gap() {
    let mut tone = vec![0.0; 24_000];
    ToneGenerator::new(Tone {
        hz: 1_000.0,
        amplitude: 0.5,
    })
    .fill(&mut tone);
    tone.splice(12_000..12_000, [0.0; 128]);
    tone.resize(48_000, 0.0);
    let coherent = tone
        .as_chunks::<24_000>()
        .0
        .iter()
        .map(|window| amplitude_at(window, 1_000.0))
        .fold(0.0, f32::max);
    assert!(
        coherent < 0.45,
        "the old coherent estimator must reproduce cancellation: {coherent}"
    );
    let recorded = strongest_recorded_tone(&tone, 1_000.0);
    assert!((recorded - 0.5).abs() < 0.05, "recorded at {recorded}");
}

#[test]
fn the_recorded_level_rejects_silence_wrong_frequency_weak_tones_fragments_and_sustained_dropouts() {
    let make_tone = |hz, amplitude| {
        let mut samples = vec![0.0; 24_000];
        ToneGenerator::new(Tone { hz, amplitude }).fill(&mut samples);
        samples
    };
    assert!((strongest_recorded_tone(&make_tone(1_000.0, 0.5), 1_000.0) - 0.5).abs() < 0.05);
    let mut brief = make_tone(1_000.0, 0.5);
    brief[9_600..].fill(0.0);
    let mut dropouts = make_tone(1_000.0, 0.5);
    for frame in dropouts.as_chunks_mut::<480>().0.iter_mut().step_by(5) {
        frame.fill(0.0);
    }
    for samples in [
        vec![0.0; 24_000],
        make_tone(440.0, 0.5),
        make_tone(1_000.0, 0.44),
        make_tone(1_000.0, 0.5)[..480].to_vec(),
        brief,
        dropouts,
    ] {
        let recorded = strongest_recorded_tone(&samples, 1_000.0);
        assert!(recorded < 0.45, "a sustained full-level tone was absent: {recorded}");
    }
}

/// A microphone plugged in that plays given samples over and over is heard as the default and marks when it played
/// them, at the devices' rate on the wall clock; the speaker's recording marks when it heard each sample, so a tone
/// played into the speaker is found in the recording at the time it was played.
#[test]
fn a_looped_microphone_and_the_recording_mark_when_their_samples_went_through() {
    let Some(private) = private_or_skip("the timelines", &PipeWireOptions::default()) else {
        return;
    };
    if let Err(error) = check_timelines(&private.pipewire) {
        panic!("{error}{}", private.pipewire.logs());
    }
}

fn check_timelines(pipewire: &PrivatePipeWire) -> SandboxResult {
    let env = pipewire.client_env();
    let started = now_us();
    // a second of 990 Hz, then a second of silence, over and over
    let mut samples = vec![0.0; 96_000];
    ToneGenerator::new(Tone {
        hz: 990.0,
        amplitude: 0.4,
    })
    .fill(&mut samples[..48_000]);
    let spec = NodeSpec {
        name: "sandbox-looped-mic".to_owned(),
        description: "Looped microphone".to_owned(),
        priority: DEVICE_PRIORITY + 1_000,
    };
    let node = pipewire.add_looped_microphone(spec, Arc::from(samples))?;
    let heard = probe(&env, &["mic", "3", "990", "440"])?;
    // the 3 s heard hold one or two of the loop's seconds of tone, wherever the probe starts in it (its start shifts on
    // a loaded machine): either is far above the silence heard at 440 Hz
    assert!(value::<f32>(&heard, "level_990") > 0.1, "{heard:?}");
    assert!(value::<f32>(&heard, "level_440") < 0.01, "{heard:?}");
    let marks = node.timeline().ok_or("no timeline")?.marks();
    let (first, last) = (marks[0], marks[marks.len() - 1]);
    assert_eq!(first.sample, 0, "{first:?}");
    assert!(first.unix_us >= started, "{first:?} before {started}");
    let played = f64::from(u32::try_from(last.sample - first.sample)?);
    let seconds = Duration::from_micros(u64::try_from(last.unix_us - first.unix_us)?).as_secs_f64();
    assert!(seconds > 2.0, "{seconds} s of marks");
    let rate = played / seconds;
    assert!((rate - 48_000.0).abs() < 48_000.0 * 0.05, "played at {rate} Hz");
    drop(node);

    // a tone played into the speaker: the recording's first sample of it was heard while the probe played it
    let before = now_us();
    probe(&env, &["speaker", "1", "1000", "0.5"])?;
    let after = now_us();
    std::thread::sleep(Duration::from_millis(1_500));
    let recorded = wav::read(&pipewire.recording())?;
    let onset = recorded
        .chunks(480)
        .position(|window| amplitude_at(window, 1_000.0) > 0.25)
        .ok_or("the tone isn't in the recording")?;
    let heard_at = pipewire
        .recording_timeline()
        .time_of(u64::try_from(onset * 480)?)
        .ok_or("the recording marked nothing")?;
    assert!(
        (before..after).contains(&heard_at),
        "heard at {heard_at}, played between {before} and {after}"
    );
    Ok(())
}

/// The microphone, the speaker and the camera each say they are closed once the server they stream on goes away, and
/// opening one after that says so at once (not after the open timeout, as another error).
#[test]
fn every_device_says_closed_once_the_server_goes_away() {
    let Some(private) = private_or_skip("every_device_says_closed", &PipeWireOptions::default()) else {
        return;
    };
    let args = ["ended", "20"];
    let running = start_probe(&private.pipewire.client_env(), &args).unwrap();
    if let Err(error) = running.line_where(PROBE_DEADLINE, |line| line == "opened=1") {
        panic!("the probe opened nothing: {error:?}{}", private.pipewire.logs());
    }
    // stops the server (and its session manager and the sandbox's own clients) by the ids it holds; the dir stays
    let Private { pipewire, _dir: dir } = private;
    drop(pipewire);
    let said = probe_values(running, &args).unwrap();
    for device in ["microphone", "speaker", "camera"] {
        assert_eq!(
            said.get(device).map(String::as_str),
            Some("closed"),
            "{device}: {said:?}"
        );
        assert_eq!(
            said.get(&format!("reopened_{device}")).map(String::as_str),
            Some("closed"),
            "{device} opened after the server went away: {said:?}"
        );
        let took: u64 = said
            .get(&format!("reopen_ms_{device}"))
            .and_then(|ms| ms.parse().ok())
            .unwrap_or_else(|| panic!("{device}: {said:?}"));
        assert!(took < 1_000, "{device}'s failing open took {took} ms: {said:?}");
    }
    drop(dir);
}

/// A sandbox (compositor, bus and PipeWire) gives a client its PipeWire with the rest.
#[test]
fn a_sandbox_with_pipewire_hands_it_to_its_clients() {
    let options = SandboxOptions {
        pipewire: Some(PipeWireOptions::default()),
        ..SandboxOptions::default()
    };
    let Some(sandbox) = ui_sandbox::sandbox_or_skip("a_sandbox_with_pipewire", &options).unwrap() else {
        return;
    };
    let env = sandbox.client_env();
    let dir = sandbox.dir().to_owned();
    for name in [
        SANDBOX_ENV,
        "PIPEWIRE_RUNTIME_DIR",
        "PIPEWIRE_CONFIG_DIR",
        "SPA_PLUGIN_DIR",
        "PIPEWIRE_MODULE_DIR",
    ] {
        let values: Vec<&OsString> = env
            .iter()
            .filter(|(key, _)| *key == name)
            .map(|(_, value)| value)
            .collect();
        assert_eq!(values.len(), 1, "{name}: {values:?}");
        assert!(Path::new(values[0]).starts_with(&dir), "{name}={values:?}");
    }
    assert!(dir.join(SANDBOX_SOCKET).exists() && dir.join(SANDBOX_CLIENT_CONF).exists());
    let heard = probe(&env, &["mic", "0.5", "440"]).unwrap_or_else(|error| panic!("{error}{}", sandbox.logs()));
    let pipewire = sandbox.pipewire().expect("asked for");
    assert!(
        (value::<f32>(&heard, "level_440") - 0.25).abs() < 0.03,
        "{heard:?}{}",
        pipewire.logs()
    );
}

/// Listening sockets nobody may connect to, and a client config drop-in nobody may read (it asks for a module that
/// doesn't exist, which fails whoever reads it).
struct Canaries {
    dir: TempDir,
    listeners: Vec<(PathBuf, UnixListener)>,
}

impl Canaries {
    fn plant() -> Self {
        let dir = TempDir::new().unwrap();
        let mut listeners = Vec::new();
        for name in [
            "run/pipewire-0",
            "run/pipewire-0-manager",
            "run/pulse/native",
            "run/bus",
            "pw/pipewire-0",
            "pulse-native",
        ] {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            listeners.push((path, listener));
        }
        for conf in ["client.conf.d", "pipewire.conf.d"] {
            let drop_in = dir.path().join("home/.config/pipewire").join(conf);
            fs::create_dir_all(&drop_in).unwrap();
            fs::write(
                drop_in.join("canary.conf"),
                "context.modules = [ { name = libpipewire-module-canary-never-read } ]\n",
            )
            .unwrap();
        }
        Self { dir, listeners }
    }

    /// The session's variables, every one pointing at a canary.
    fn session_env(&self) -> Vec<(&'static str, String)> {
        let path = |name: &str| self.dir.path().join(name).to_string_lossy().into_owned();
        vec![
            ("XDG_RUNTIME_DIR", path("run")),
            ("PIPEWIRE_RUNTIME_DIR", path("pw")),
            ("PIPEWIRE_REMOTE", path("pw/pipewire-0")),
            ("PULSE_SERVER", format!("unix:{}", path("pulse-native"))),
            ("PULSE_RUNTIME_PATH", path("run/pulse")),
            ("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", path("run/bus"))),
            ("HOME", path("home")),
            ("XDG_CONFIG_HOME", path("home/.config")),
            (CANARY_ENV, self.dir.path().to_string_lossy().into_owned()),
        ]
    }

    fn reached(&self) -> Vec<&Path> {
        self.listeners
            .iter()
            .filter(|(_, listener)| listener.accept().is_ok())
            .map(|(path, _)| path.as_path())
            .collect()
    }
}

/// Runs [`a_private_pipewire_with_canaries_planted`] in a child test process whose session points at canaries; the
/// child must pass and no canary may be reached.
#[test]
fn the_session_s_pipewire_pulse_and_bus_are_never_reached() {
    if let Err(reason) = PipeWireTools::find() {
        unavailable::<()>(
            "the_session_s_pipewire_pulse_and_bus_are_never_reached",
            &reason,
            required(),
        )
        .unwrap();
        return;
    }
    let canaries = Canaries::plant();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([CANARY_CHILD, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .envs(canaries.session_env())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "child failed\n--- stdout\n{stdout}\n--- stderr\n{stderr}"
    );
    assert!(stdout.contains("1 passed"), "the child ran no test\n{stdout}\n{stderr}");
    let reached = canaries.reached();
    assert!(reached.is_empty(), "the session was reached: {reached:?}\n{stderr}");
}

/// The variables of process `pid` as it was started.
fn environ(pid: u32) -> Vec<String> {
    fs::read(format!("/proc/{pid}/environ"))
        .unwrap_or_default()
        .split(|&byte| byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect()
}

/// The backend refuses the session's PipeWire in this process, the private server starts and serves a client,
/// and neither the server nor the session manager was handed anything of the session; run only as
/// [`the_session_s_pipewire_pulse_and_bus_are_never_reached`]'s child.
#[test]
#[ignore = "run by the_session_s_pipewire_pulse_and_bus_are_never_reached, in a child process with canaries"]
fn a_private_pipewire_with_canaries_planted() {
    let canary = std::env::var(CANARY_ENV).expect("run by the_session_s_pipewire_pulse_and_bus_are_never_reached");
    assert!(!canary.is_empty() && canary != "/");
    // Sandbox reach opens only the sandbox's PipeWire, and this process's session has none.
    let error = PipeWireDevices::connect(Reach::Sandbox).unwrap_err();
    assert!(
        matches!(&error, DeviceError::Refused(reason) if reason.contains(SANDBOX_ENV)),
        "{error}"
    );

    let Some(private) = private_or_skip(CANARY_CHILD, &PipeWireOptions::default()) else {
        return;
    };
    for pid in private.pipewire.pids() {
        for entry in environ(pid) {
            assert!(!entry.contains(&canary), "{pid} was handed {entry}");
        }
    }
    let env = private.pipewire.client_env();
    for (name, value) in &env {
        assert!(!value.to_string_lossy().contains(&canary), "{name}={value:?}");
    }
    let heard =
        probe(&env, &["mic", "0.5", "440"]).unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    assert!(
        (value::<f32>(&heard, "level_440") - TONE.amplitude).abs() < 0.03,
        "{heard:?}"
    );
    let seen = probe(&env, &["camera", "5", "20"]).unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    assert!(value::<u64>(&seen, "frames") >= 5, "{seen:?}");

    // handed the session's PipeWire and Pulse variables and no sandbox, the probe refuses before connecting to
    // anything. The probe explicitly selects sandbox reach, independent of the features its caller unified.
    let session: Vec<(&str, OsString)> = [
        "XDG_RUNTIME_DIR",
        "PIPEWIRE_RUNTIME_DIR",
        "PIPEWIRE_REMOTE",
        "PULSE_SERVER",
    ]
    .into_iter()
    .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
    .collect();
    assert_eq!(session.len(), 4);
    let refused = probe(&session, &["mic", "0.5", "440"]).unwrap_err().to_string();
    assert!(
        refused.contains("refused") && refused.contains(SANDBOX_ENV),
        "{refused}"
    );
}
