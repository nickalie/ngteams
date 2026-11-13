//! Real devices in the sandbox's private PipeWire, through `teams-media`'s PipeWire backend in a client process of
//! the sandbox (`ui-sandbox-media-probe`): the microphones, speakers and cameras listed with the system's defaults; a
//! device plugged in and out (the sandbox adds and removes a node) seen coming and going; a running microphone and
//! speaker moved to the device the user chose and back without ever saying they closed (the tone goes on, from the
//! other device or into the other recording); a microphone chosen while it is the default kept when a device plugged
//! in above it becomes the default, and falling back when a chosen device is unplugged; following the system, both moved with a device plugged in above the
//! system's and back once it is unplugged; the personal build's camera route, through the sandbox's fake camera
//! portal on the sandbox's bus: granted, denied, and refused outright for a bus outside the sandbox; and a shared
//! screen's, through its fake screen-cast portal there, cast as niri casts (DMA-BUFs only) or in plain memory.
//!
//! Without `pipewire` and `wireplumber` (and for the portal, the compositors and `dbus-daemon`) the tests say so and
//! pass, unless `GPUI_SMOKE_REQUIRE=1`. Nothing here plays aloud or opens a real device: the server is a private one
//! with no device plugin to open, and the portal is the sandbox's fake.

#![cfg(target_os = "linux")]
// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::Duration;

use teams_media::devices::wav;
use teams_media::tone::{Tone, amplitude_at};
use tempfile::TempDir;
use ui_sandbox::pipewire::nodes::{AddedNode, NodeSpec};
use ui_sandbox::pipewire::{
    CAMERA_NODE, DEVICE_PRIORITY, MICROPHONE_NODE, PipeWireOptions, PipeWireTools, PrivatePipeWire, SPEAKER_NODE,
    ScreenCast,
};
use ui_sandbox::{LineError, Running, SandboxOptions, SandboxResult, clean_command, required, spawn, unavailable};

const PROBE: &str = env!("CARGO_BIN_EXE_ui-sandbox-media-probe");
/// How long one probe run may take.
const PROBE_DEADLINE: Duration = Duration::from_secs(60);
/// The sandbox microphone's tone.
const SYSTEM_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.25,
};
/// How long a speaker's recording is waited for after a move: its header is brought up to date every second.
const SETTLE: Duration = Duration::from_millis(2_200);
/// The stretch of a recording's end that is measured.
const TAIL: Duration = Duration::from_millis(600);
/// A tone this loud is there, one this quiet isn't.
const THERE: f32 = 0.3;
const GONE: f32 = 0.05;
/// The longest a running microphone may go without a frame while it is moved.
const MAX_GAP_MS: u64 = 300;

/// A private PipeWire in a temp dir of its own; stopped before the dir goes.
struct Private {
    pipewire: PrivatePipeWire,
    _dir: TempDir,
}

fn private_or_skip(test: &str) -> Option<Private> {
    let tools = match PipeWireTools::find() {
        Ok(tools) => tools,
        Err(reason) => {
            unavailable::<()>(test, &reason, required()).unwrap();
            return None;
        }
    };
    let dir = TempDir::new().unwrap();
    let options = PipeWireOptions {
        tone: SYSTEM_TONE,
        ..PipeWireOptions::default()
    };
    let pipewire = PrivatePipeWire::start(&tools, dir.path(), &options).unwrap_or_else(|error| panic!("{error}"));
    Some(Private { pipewire, _dir: dir })
}

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
    let mut lines = Vec::new();
    loop {
        match running.line(PROBE_DEADLINE) {
            Ok(line) => {
                if let Some((key, value)) = line.split_once('=') {
                    values.insert(key.to_owned(), value.to_owned());
                }
                lines.push(line);
            }
            Err(LineError::Closed) => break,
            Err(LineError::Timeout) => return Err(running.fail(&format!("probe {args:?} gave no output in time"))),
        }
    }
    let (status, stderr) = running.finish(PROBE_DEADLINE)?;
    if !status.success() {
        return Err(format!("probe {args:?}: {status}\n{stderr}").into());
    }
    values.insert("lines".to_owned(), lines.join("\n"));
    Ok(values)
}

fn probe(env: &[(&str, OsString)], args: &[&str]) -> SandboxResult<HashMap<String, String>> {
    probe_values(start_probe(env, args)?, args)
}

/// Waits for the probe's line `line`.
fn wait_line(running: &mut Running, line: &str) -> SandboxResult {
    running
        .line_where(PROBE_DEADLINE, |seen| seen == line)
        .map_err(|error| running.fail(&format!("no {line} line: {error:?}")))?;
    Ok(())
}

fn value<T: std::str::FromStr>(values: &HashMap<String, String>, key: &str) -> T {
    values
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {values:?}"))
}

fn spec(name: &str, description: &str, priority: u32) -> NodeSpec {
    NodeSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        priority,
    }
}

/// The level at `hz` over the last [`TAIL`] of the recording at `path`.
fn tail_level(path: &Path, hz: f32) -> f32 {
    let samples = wav::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let wanted = usize::try_from(TAIL.as_millis()).unwrap() * 48;
    assert!(
        samples.len() >= wanted,
        "{} holds {} samples",
        path.display(),
        samples.len()
    );
    amplitude_at(&samples[samples.len() - wanted..], hz)
}

fn added_recording(node: &AddedNode) -> &Path {
    node.recording().expect("a speaker's recording")
}

#[test]
fn the_devices_are_listed_with_the_system_s_defaults_and_a_device_plugged_in_comes_and_goes() {
    let Some(private) = private_or_skip("the_devices_are_listed") else {
        return;
    };
    let env = private.pipewire.client_env();
    let listed = probe(&env, &["devices"]).unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    let lines = &listed["lines"];
    for device in [
        format!("device=microphone:{MICROPHONE_NODE}:Sandbox microphone"),
        format!("device=speaker:{SPEAKER_NODE}:Sandbox speaker"),
        format!("device=camera:{CAMERA_NODE}:Sandbox camera"),
    ] {
        assert!(lines.lines().any(|line| line == device), "{device} in {lines}");
    }
    assert!(
        !lines.contains("sandbox-echo") && !lines.contains("sandbox-recorder"),
        "a stream is no device: {lines}"
    );
    assert_eq!(listed["default_microphone"], MICROPHONE_NODE);
    assert_eq!(listed["default_speaker"], SPEAKER_NODE);
    assert_eq!(listed["default_camera"], CAMERA_NODE);

    // a headset plugged in above the system's devices: listed, made the default; unplugged: gone
    let args = ["watch-node", "sandbox-headset-mic", "30"];
    let mut running = start_probe(&env, &args).unwrap();
    wait_line(&mut running, "listening=1").unwrap();
    let headset = private
        .pipewire
        .add_microphone(
            spec("sandbox-headset-mic", "Sandbox headset", DEVICE_PRIORITY + 1_000),
            Tone {
                hz: 880.0,
                amplitude: 0.25,
            },
        )
        .unwrap();
    wait_line(&mut running, "added=microphone").unwrap();
    wait_line(&mut running, "default=1").unwrap();
    headset.remove().unwrap();
    let said = probe_values(running, &args).unwrap();
    assert_eq!(said.get("removed").map(String::as_str), Some("1"), "{said:?}");
}

#[test]
fn a_running_microphone_moves_to_the_device_chosen_and_back_and_never_closes() {
    let Some(private) = private_or_skip("a_running_microphone_moves") else {
        return;
    };
    // a second microphone below the system's: there to be chosen, not the default
    let _usb = private
        .pipewire
        .add_microphone(
            spec("sandbox-usb-mic", "USB microphone", DEVICE_PRIORITY - 1_000),
            Tone {
                hz: 660.0,
                amplitude: 0.25,
            },
        )
        .unwrap();
    let args = ["switch-mic", "sandbox-usb-mic", "440", "660"];
    let moved = probe(&private.pipewire.client_env(), &args)
        .unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    eprintln!("the microphone moved: {moved:?}");
    assert_eq!(moved["chosen"], "sandbox-usb-mic");
    let gap: u64 = value(&moved, "max_gap_ms");
    assert!(
        gap < MAX_GAP_MS,
        "a frame came {gap} ms after the one before: {moved:?}"
    );
}

#[test]
fn a_running_speaker_moves_to_the_device_chosen_and_back_and_never_closes() {
    let Some(private) = private_or_skip("a_running_speaker_moves") else {
        return;
    };
    let headset = private
        .pipewire
        .add_speaker(spec("sandbox-headset", "Sandbox headset", DEVICE_PRIORITY - 1_000))
        .unwrap();
    let args = ["switch-speaker", "sandbox-headset", "1000", "0.5", "3"];
    let mut running = start_probe(&private.pipewire.client_env(), &args).unwrap();
    let system = private.pipewire.recording();
    wait_line(&mut running, "phase=chosen").unwrap();
    thread::sleep(SETTLE);
    let (moved_to, left) = (
        tail_level(added_recording(&headset), 1_000.0),
        tail_level(&system, 1_000.0),
    );
    assert!(
        moved_to > THERE && left < GONE,
        "chosen: the headset at {moved_to}, the system's speaker at {left}"
    );
    wait_line(&mut running, "phase=back").unwrap();
    thread::sleep(SETTLE);
    let back = tail_level(&system, 1_000.0);
    assert!(back > THERE, "back: the system's speaker at {back}");
    let said = probe_values(running, &args).unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    assert!(
        said["lines"].contains("phase=done"),
        "the speaker never closed: {said:?}"
    );
}

#[test]
fn following_the_system_the_microphone_and_the_speaker_move_with_a_device_plugged_in_and_out() {
    let Some(private) = private_or_skip("following_the_system") else {
        return;
    };
    let env = private.pipewire.client_env();
    let above = DEVICE_PRIORITY + 1_000;

    // the microphone: a headset plugged in takes over, and the system's comes back once it is unplugged
    let args = ["follow-mic", "440", "880", "20"];
    let mut running = start_probe(&env, &args).unwrap();
    wait_line(&mut running, "listening=1").unwrap();
    let headset = private
        .pipewire
        .add_microphone(
            spec("sandbox-headset-mic", "Sandbox headset", above),
            Tone {
                hz: 880.0,
                amplitude: 0.25,
            },
        )
        .unwrap();
    wait_line(&mut running, "moved=1").unwrap();
    headset.remove().unwrap();
    let followed = probe_values(running, &args).unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    eprintln!("the microphone followed: {followed:?}");
    assert_eq!(followed.get("returned").map(String::as_str), Some("1"));
    let gap: u64 = value(&followed, "max_gap_ms");
    assert!(
        gap < MAX_GAP_MS,
        "a frame came {gap} ms after the one before: {followed:?}"
    );

    // the speaker: a tone played on, into the headset while it is plugged in, into the system's speaker after
    let args = ["speaker", "12", "1000", "0.5"];
    let running = start_probe(&env, &args).unwrap();
    let system = private.pipewire.recording();
    thread::sleep(SETTLE);
    assert!(tail_level(&system, 1_000.0) > THERE, "the system's speaker plays first");
    let headset = private
        .pipewire
        .add_speaker(spec("sandbox-headset", "Sandbox headset", above))
        .unwrap();
    thread::sleep(SETTLE);
    let (moved_to, left) = (
        tail_level(added_recording(&headset), 1_000.0),
        tail_level(&system, 1_000.0),
    );
    assert!(
        moved_to > THERE && left < GONE,
        "plugged in: the headset at {moved_to}, the system's speaker at {left}"
    );
    headset.remove().unwrap();
    thread::sleep(SETTLE);
    let back = tail_level(&system, 1_000.0);
    assert!(back > THERE, "unplugged: the system's speaker at {back}");
    let played = probe_values(running, &args).unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    assert!(played.contains_key("played"), "the speaker never closed: {played:?}");
}

/// A device chosen while it is the system's default is kept when a device plugged in above it becomes the default
/// (WirePlumber would otherwise take such a stream for one following the default), and a chosen device unplugged
/// leaves the microphone on the system's default, never silent or closed.
#[test]
fn a_chosen_microphone_stays_on_its_device_when_another_becomes_the_default_and_falls_back_when_unplugged() {
    let Some(private) = private_or_skip("a_chosen_microphone_stays") else {
        return;
    };
    let headset_name = "sandbox-headset-mic";
    let args = ["pinned-mic", MICROPHONE_NODE, "440", headset_name, "880", "20"];
    let mut running = start_probe(&private.pipewire.client_env(), &args).unwrap();
    wait_line(&mut running, "pinned=1").unwrap();
    let headset = private
        .pipewire
        .add_microphone(
            spec(headset_name, "Sandbox headset", DEVICE_PRIORITY + 1_000),
            Tone {
                hz: 880.0,
                amplitude: 0.25,
            },
        )
        .unwrap();
    wait_line(&mut running, "kept=1").unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    wait_line(&mut running, "moved=1").unwrap();
    headset.remove().unwrap();
    let said = probe_values(running, &args).unwrap_or_else(|error| panic!("{error}{}", private.pipewire.logs()));
    eprintln!("the chosen microphone: {said:?}");
    assert_eq!(said.get("fell_back").map(String::as_str), Some("1"), "{said:?}");
    let gap: u64 = value(&said, "max_gap_ms");
    assert!(gap < MAX_GAP_MS, "a frame came {gap} ms after the one before: {said:?}");
}

/// The personal build's camera route, on the sandbox's fake camera portal: the camera's frames come through the
/// remote it hands over once the user allowed it; a user who doesn't allow it gets no remote; a bus outside the
/// sandbox is refused before anything is asked.
#[test]
fn the_camera_comes_through_the_sandbox_s_camera_portal_only_when_allowed() {
    let options = SandboxOptions {
        pipewire: Some(PipeWireOptions::default()),
        ..SandboxOptions::default()
    };
    let Some(sandbox) = ui_sandbox::sandbox_or_skip("the_camera_portal", &options).unwrap() else {
        return;
    };
    let env = sandbox.client_env();
    let seen = probe(&env, &["portal-camera", "10", "20"]).unwrap_or_else(|error| panic!("{error}{}", sandbox.logs()));
    assert_eq!(seen["camera"], "open", "{seen:?}");
    assert!(value::<u64>(&seen, "frames") >= 10, "{seen:?}");
    let calls = sandbox.bus().portal_calls();
    let asked: Vec<(&str, &str)> = calls.iter().map(|call| (call.method, call.answer)).collect();
    assert_eq!(asked, [("AccessCamera", "granted"), ("OpenPipeWireRemote", "remote")]);
    assert_eq!(calls[0].sender, calls[1].sender, "one caller asked and opened");

    sandbox.bus().set_camera_allowed(false);
    let denied = probe(&env, &["portal-camera", "10", "20"]).unwrap();
    assert_eq!(denied["camera"], "refused", "{denied:?}");
    let calls = sandbox.bus().portal_calls();
    let asked: Vec<(&str, &str)> = calls[2..].iter().map(|call| (call.method, call.answer)).collect();
    assert_eq!(asked, [("AccessCamera", "denied")], "no remote without the user's yes");

    // a bus outside the sandbox (a canary) is refused before anything is asked of it
    let outside = TempDir::new().unwrap();
    let canary = outside.path().join("bus");
    let listener = UnixListener::bind(&canary).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut elsewhere: Vec<(&str, OsString)> = env
        .into_iter()
        .filter(|(name, _)| *name != "DBUS_SESSION_BUS_ADDRESS")
        .collect();
    elsewhere.push((
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", canary.display()).into(),
    ));
    let refused = probe(&elsewhere, &["portal-camera", "10", "20"])
        .unwrap_err()
        .to_string();
    assert!(refused.contains("refused") && refused.contains("sandbox"), "{refused}");
    assert!(listener.accept().is_err(), "the canary bus was reached");
}

/// A shared screen's route, on the sandbox's fake screen-cast portal (sandbox reach asks only the sandbox's bus): its
/// session made, its sources picked, the sandbox's synthetic screen started and its remote handed over, the screen's
/// frames (the 720p pattern, in order) coming through it though the screen casts as niri does (linear DMA-BUFs only,
/// its modifier mandatory), and the session closed only once the screen went; a bus outside the sandbox refused
/// before anything is asked.
#[test]
fn a_shared_screen_comes_through_the_sandbox_s_screen_cast_portal() {
    let options = SandboxOptions {
        pipewire: Some(PipeWireOptions::default()),
        ..SandboxOptions::default()
    };
    let Some(sandbox) = ui_sandbox::sandbox_or_skip("the_screen_cast_portal", &options).unwrap() else {
        return;
    };
    let env = sandbox.client_env();
    let seen = probe(&env, &["portal-screen", "10", "20"]).unwrap_or_else(|error| panic!("{error}{}", sandbox.logs()));
    assert_eq!(seen["screen"], "open", "{seen:?}");
    assert_eq!(seen["size"], "1280x720", "the sandbox's screen: {seen:?}");
    assert!(value::<u64>(&seen, "frames") >= 10, "{seen:?}");
    assert_eq!(seen["in_order"], "1", "{seen:?}");
    let calls = sandbox.bus().portal_calls();
    let asked: Vec<(&str, &str)> = calls.iter().map(|call| (call.method, call.answer)).collect();
    assert_eq!(
        asked,
        [
            ("ScreenCast.CreateSession", "session"),
            ("ScreenCast.SelectSources", "selected"),
            ("ScreenCast.Start", "screen"),
            ("ScreenCast.OpenPipeWireRemote", "remote"),
            ("ScreenCast.Close", "closed"),
        ]
    );
    assert!(calls.iter().all(|call| call.sender == calls[0].sender), "one caller");
    // what the portal says it offers, as xdg-desktop-portal's does: a monitor or a window, every cursor mode
    let client = sandbox.bus().connect().unwrap();
    let cast = zbus::blocking::Proxy::new(
        &client,
        ui_sandbox::bus::PORTAL_NAME,
        ui_sandbox::bus::PORTAL_PATH,
        "org.freedesktop.portal.ScreenCast",
    )
    .unwrap();
    let property = |name: &str| cast.get_property::<u32>(name).unwrap();
    assert_eq!(
        (
            property("AvailableSourceTypes"),
            property("AvailableCursorModes"),
            property("version")
        ),
        (3, 7, 4)
    );

    // a bus outside the sandbox (a canary) is refused before anything is asked of it
    let outside = TempDir::new().unwrap();
    let canary = outside.path().join("bus");
    let listener = UnixListener::bind(&canary).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut elsewhere: Vec<(&str, OsString)> = env
        .into_iter()
        .filter(|(name, _)| *name != "DBUS_SESSION_BUS_ADDRESS")
        .collect();
    elsewhere.push((
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", canary.display()).into(),
    ));
    let refused = probe(&elsewhere, &["portal-screen", "10", "20"]).unwrap();
    assert_eq!(refused["screen"], "refused", "{refused:?}");
    assert!(refused["why"].contains("sandbox"), "{refused:?}");
    assert!(listener.accept().is_err(), "the canary bus was reached");
}

/// A screen cast in plain memory, as compositors that offer no DMA-BUF cast it (the DMA-BUF offer comes first and is
/// turned down): its frames come through the portal in order, and its session is closed once the screen went.
#[test]
fn a_screen_cast_in_plain_memory_comes_through_the_screen_cast_portal_too() {
    let options = SandboxOptions {
        pipewire: Some(PipeWireOptions {
            screen: ScreenCast::Memory,
            ..PipeWireOptions::default()
        }),
        ..SandboxOptions::default()
    };
    let Some(sandbox) = ui_sandbox::sandbox_or_skip("the_screen_cast_portal_in_memory", &options).unwrap() else {
        return;
    };
    let seen = probe(&sandbox.client_env(), &["portal-screen", "10", "20"])
        .unwrap_or_else(|error| panic!("{error}{}", sandbox.logs()));
    assert_eq!(seen["screen"], "open", "{seen:?}");
    assert_eq!(seen["size"], "1280x720", "{seen:?}");
    assert!(value::<u64>(&seen, "frames") >= 10, "{seen:?}");
    assert_eq!(seen["in_order"], "1", "{seen:?}");
    let asked: Vec<&str> = sandbox.bus().portal_calls().iter().map(|call| call.method).collect();
    assert_eq!(
        asked,
        [
            "ScreenCast.CreateSession",
            "ScreenCast.SelectSources",
            "ScreenCast.Start",
            "ScreenCast.OpenPipeWireRemote",
            "ScreenCast.Close",
        ]
    );
}
