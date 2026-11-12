//! The UI sandbox's own tests: its tools, its spawn's descriptors, its private bus and fakes, its opener, and one
//! run of everything together (a GPUI window, virtual input, `open_url`, the bus) with canaries planted on every
//! address of the desktop session it inherits, which must stay silent.
//!
//! Without weston 15, niri or `dbus-daemon` the tests that need them say so and pass, unless `GPUI_SMOKE_REQUIRE=1`.

#![cfg(target_os = "linux")]

use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    io::Read as _,
    os::{
        fd::{AsFd as _, AsRawFd},
        unix::{
            fs::PermissionsExt as _,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use tempfile::TempDir;
use ui_sandbox::{
    LineError, NIRI_FULL_WINDOW, Running, SANDBOX_ENV, Sandbox, SandboxOptions, SandboxResult, Tools,
    bus::{NOTIFICATIONS_NAME, NOTIFICATIONS_PATH, PrivateBus, WATCHER_NAME, WATCHER_PATH},
    clean_command,
    input::{Modifier, keys},
    opener::{self, Opened},
    private_dir, required, spawn, unavailable,
};
use zbus::zvariant::Value;

const PROBE: &str = env!("CARGO_BIN_EXE_ui-sandbox-probe");
/// The probe window's Wayland app id and colour (`src/bin/ui-sandbox-probe.rs`).
const PROBE_APP_ID: &str = "ngteams-ui-sandbox-probe";
const PROBE_COLOR: [u8; 3] = [0x20, 0xc0, 0x60];
/// From launch to the ready line (a cold dGPU wakes up within it).
const STARTUP: Duration = Duration::from_secs(30);
/// From the compositor's close request to the process exit.
const CLOSE: Duration = Duration::from_secs(5);
/// How long an event the probe was sent gets to come back on its stdout.
const ECHO: Duration = Duration::from_secs(10);
/// The child run of [`the_desktop_session_is_never_reached`], and the variable naming its canary dir.
const CANARY_CHILD: &str = "everything_together_with_canaries_planted";
const CANARY_ENV: &str = "UI_SANDBOX_CANARY_DIR";

fn script(dir: &Path, name: &str, body: &str) -> SandboxResult<PathBuf> {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    Ok(path)
}

#[test]
fn missing_tools_are_named() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().as_os_str();
    let error = Tools::find_on(path).unwrap_err();
    assert!(error.contains("weston"), "{error}");
    // A weston older than 15 has no --fake-seat.
    script(dir.path(), "weston", "echo '--backend=headless'").unwrap();
    let error = Tools::find_on(path).unwrap_err();
    assert!(error.contains("weston 15"), "{error}");
    script(dir.path(), "weston", "echo '  --fake-seat  Use a fake seat'").unwrap();
    let error = Tools::find_on(path).unwrap_err();
    assert!(error.contains("niri"), "{error}");
    script(dir.path(), "niri", "exit 0").unwrap();
    let error = Tools::find_on(path).unwrap_err();
    assert!(error.contains("dbus-daemon"), "{error}");
    script(dir.path(), "dbus-daemon", "exit 0").unwrap();
    let tools = Tools::find_on(path).unwrap();
    assert_eq!(tools.dbus_daemon, dir.path().join("dbus-daemon"));
}

#[test]
fn a_missing_sandbox_skips_or_fails() {
    assert_eq!(
        required(),
        std::env::var_os(ui_sandbox::REQUIRE_ENV).is_some_and(|value| value == "1")
    );
    assert!(
        unavailable::<()>("some_test", "no niri on PATH", false)
            .unwrap()
            .is_none()
    );
    let error = unavailable::<()>("some_test", "no niri on PATH", true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("some_test: no niri on PATH"), "{error}");
    assert!(error.contains("GPUI_SMOKE_REQUIRE=1"), "{error}");
}

/// The subprocess supplies its own environment, avoiding changes to this test process's shared environment.
#[test]
fn clean_commands_keep_only_the_profile_path_and_the_allowed_environment() {
    const CHILD: &str = "UI_SANDBOX_CHECK_PROFILE_ENV";
    const PARENT_ONLY: [&str; 5] = [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "UI_SANDBOX_SECRET_CANARY",
    ];
    if std::env::var_os(CHILD).is_some() {
        let output = clean_command("/usr/bin/env").output().unwrap();
        assert!(output.status.success());
        let environment: HashMap<_, _> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        assert_eq!(
            environment.get("LLVM_PROFILE_FILE").map(OsStr::new),
            std::env::var_os("LLVM_PROFILE_FILE").as_deref()
        );
        assert_eq!(environment.get("WGPU_BACKEND").map(String::as_str), Some("vulkan"));
        for name in PARENT_ONLY.into_iter().chain([CHILD]) {
            assert!(
                !environment.contains_key(name),
                "parent-only variable inherited: {name}"
            );
        }
        assert!(
            environment
                .keys()
                .all(|name| ui_sandbox::PASSED_ENV.contains(&name.as_str()))
        );
        return;
    }

    let dir = TempDir::new().unwrap();
    let profile = std::env::var_os("LLVM_PROFILE_FILE")
        .unwrap_or_else(|| dir.path().join("child-%p-%m.profraw").into_os_string());
    for present in [false, true] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "clean_commands_keep_only_the_profile_path_and_the_allowed_environment",
            ])
            .current_dir(dir.path())
            .env(CHILD, "1")
            .env("WGPU_BACKEND", "vulkan")
            .env_remove("LLVM_PROFILE_FILE");
        for name in PARENT_ONLY {
            command.env(name, "synthetic-parent-canary");
        }
        if present {
            command.env("LLVM_PROFILE_FILE", &profile);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "profile present={present}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed; 0 failed;"),
            "child check did not run with profile present={present}: {output:?}"
        );
    }
}

#[test]
fn the_opener_records_and_opens_nothing() {
    let dir = TempDir::new().unwrap();
    let bin = private_dir(dir.path().join("bin")).unwrap();
    let log = dir.path().join("it's opened.log");
    opener::install(&bin, &log).unwrap();
    assert!(opener::read_log(&log).unwrap().is_empty());
    let calls: [&[&str]; 2] = [
        &["xdg-open", "https://example.invalid/a?b=c&d=e f"],
        &["gio", "open", "file:///tmp/x"],
    ];
    for call in calls {
        let mut command = clean_command(bin.join(call[0]));
        command.args(&call[1..]);
        let status = spawn::output(&mut command).unwrap().status;
        assert!(status.success(), "{call:?}: {status}");
    }
    assert_eq!(
        opener::read_log(&log).unwrap(),
        [
            Opened {
                launcher: "xdg-open".to_owned(),
                args: vec!["https://example.invalid/a?b=c&d=e f".to_owned()],
            },
            Opened {
                launcher: "gio".to_owned(),
                args: vec!["open".to_owned(), "file:///tmp/x".to_owned()],
            },
        ]
    );
}

/// The descriptors a probe started by `command` (with `--list-fds`) inherited.
fn listed_fds(command: &mut Command) -> SandboxResult<Vec<u32>> {
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!("{}: {stdout}{}", output.status, String::from_utf8_lossy(&output.stderr)).into());
    }
    let fds = stdout
        .lines()
        .find_map(|line| line.strip_prefix("fds "))
        .ok_or_else(|| format!("no fds line: {stdout}"))?;
    Ok(fds.split(' ').map(str::parse).collect::<Result<_, _>>()?)
}

fn raw(fd: &impl AsRawFd) -> u32 {
    u32::try_from(fd.as_raw_fd()).unwrap_or(u32::MAX)
}

#[test]
fn spawn_passes_only_the_named_descriptors() {
    // A descriptor this process holds without close-on-exec, as an inherited one would be.
    let (leaked, _peer) = UnixStream::pair().unwrap();
    rustix::io::fcntl_setfd(&leaked, rustix::io::FdFlags::empty()).unwrap();

    // Without the sandbox's spawn the child gets it, and the probe sees it.
    let mut plain = clean_command(PROBE);
    plain.arg("--list-fds");
    let fds = listed_fds(&mut plain).unwrap();
    assert!(fds.contains(&raw(&leaked)), "the probe missed the leak: {fds:?}");

    // With it, only stdin, stdout, stderr and the named pipe, which works.
    let (mut reader, writer) = std::io::pipe().unwrap();
    let named = raw(&writer);
    let mut command = clean_command(PROBE);
    command.args(["--list-fds", "--write-fd", &named.to_string()]);
    spawn::pass_only(&mut command, &[writer.as_fd()]);
    let fds = listed_fds(&mut command).unwrap();
    drop(command);
    drop(writer);
    let mut written = String::new();
    reader.read_to_string(&mut written).unwrap();
    assert_eq!(fds, [0, 1, 2, named]);
    assert_eq!(written, "hello from the sandbox\n");
}

/// `Notify` and `CloseNotification` on the fake server, recorded with their sender.
fn check_notifications(bus: &PrivateBus, client: &zbus::blocking::Connection) -> SandboxResult {
    let server = zbus::blocking::Proxy::new(client, NOTIFICATIONS_NAME, NOTIFICATIONS_PATH, NOTIFICATIONS_NAME)?;
    let capabilities: Vec<String> = server.call("GetCapabilities", &())?;
    assert_eq!(capabilities, ["body", "actions"]);
    let information: (String, String, String, String) = server.call("GetServerInformation", &())?;
    assert_eq!(
        information,
        (
            "ui-sandbox".to_owned(),
            "ngteams".to_owned(),
            "0".to_owned(),
            "1.2".to_owned()
        )
    );
    let me = client.unique_name().ok_or("no unique name")?.to_string();
    let hints = HashMap::from([("urgency", Value::U8(1)), ("category", Value::from("im.received"))]);
    let reply = client.call_method(
        Some(NOTIFICATIONS_NAME),
        NOTIFICATIONS_PATH,
        Some(NOTIFICATIONS_NAME),
        "Notify",
        &(
            "teams",
            0_u32,
            "",
            "Ann",
            "hello",
            vec!["default", "Open"],
            hints,
            -1_i32,
        ),
    )?;
    let id: u32 = reply.body().deserialize()?;
    client.call_method(
        Some(NOTIFICATIONS_NAME),
        NOTIFICATIONS_PATH,
        Some(NOTIFICATIONS_NAME),
        "CloseNotification",
        &(id,),
    )?;
    let notifications = bus.notifications();
    let [notification] = notifications.as_slice() else {
        return Err(format!("not one notification: {notifications:?}").into());
    };
    assert_eq!(notification.sender, me);
    assert_eq!(notification.id, id);
    assert_eq!(
        (notification.app_name.as_str(), notification.summary.as_str()),
        ("teams", "Ann")
    );
    assert_eq!(notification.body, "hello");
    assert_eq!(notification.actions, ["default", "Open"]);
    assert_eq!(notification.hints, ["category", "urgency"]);
    assert_eq!(notification.expire_timeout, -1);
    assert_eq!(bus.closed_notifications(), [id]);
    Ok(())
}

/// The fake watcher says a host is there, and records the item registered with its sender.
fn check_watcher(bus: &PrivateBus, client: &zbus::blocking::Connection) -> SandboxResult {
    let me = client.unique_name().ok_or("no unique name")?.to_string();
    let watcher = zbus::blocking::Proxy::new(client, WATCHER_NAME, WATCHER_PATH, WATCHER_NAME)?;
    assert!(watcher.get_property::<bool>("IsStatusNotifierHostRegistered")?);
    watcher.call_method("RegisterStatusNotifierItem", &("/StatusNotifierItem",))?;
    let items = bus.tray_items();
    let [item] = items.as_slice() else {
        return Err(format!("not one item: {items:?}").into());
    };
    assert_eq!(
        (item.sender.as_str(), item.service.as_str()),
        (me.as_str(), "/StatusNotifierItem")
    );
    // A new proxy: the first one caches properties.
    let watcher = zbus::blocking::Proxy::new(client, WATCHER_NAME, WATCHER_PATH, WATCHER_NAME)?;
    let registered: Vec<String> = watcher.get_property("RegisteredStatusNotifierItems")?;
    assert_eq!(registered, ["/StatusNotifierItem"]);
    watcher.call_method("RegisterStatusNotifierHost", &("synthetic-host",))?;
    assert_eq!(bus.tray_hosts(), ["synthetic-host"]);
    bus.stop_watcher()?;
    bus.stop_watcher()?;
    let daemon = zbus::blocking::fdo::DBusProxy::new(client)?;
    assert!(!daemon.name_has_owner(WATCHER_NAME.try_into()?)?);
    bus.start_watcher()?;
    assert!(daemon.name_has_owner(WATCHER_NAME.try_into()?)?);
    Ok(())
}

/// The user activating a notification: the fake server's `ActionInvoked` reaches a client listening for it.
fn check_action_invoked(bus: &PrivateBus, client: &zbus::blocking::Connection) -> SandboxResult {
    let server = zbus::blocking::Proxy::new(client, NOTIFICATIONS_NAME, NOTIFICATIONS_PATH, NOTIFICATIONS_NAME)?;
    // the match rule is in place once the iterator is made, before the signal goes
    let mut invoked = server.receive_signal("ActionInvoked")?;
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let body = invoked
            .next()
            .and_then(|signal| signal.body().deserialize::<(u32, String)>().ok());
        let _ = sender.send(body);
    });
    bus.invoke_action(7, "default")?;
    let got = received.recv_timeout(Duration::from_secs(10))?;
    assert_eq!(got, Some((7, "default".to_owned())));
    Ok(())
}

/// Nothing is activated: a portal call fails at once, as GPUI's start-up calls do.
fn check_no_activation(client: &zbus::blocking::Connection) -> SandboxResult {
    let start = Instant::now();
    let refused = client.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.Settings"),
        "ReadOne",
        &("org.freedesktop.appearance", "color-scheme"),
    );
    let Err(error) = refused else {
        return Err("a portal answered on the private bus".into());
    };
    assert!(error.to_string().contains("ServiceUnknown"), "{error}");
    assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
    Ok(())
}

#[test]
fn the_private_bus_records_what_its_fakes_get() {
    let dbus_daemon = match Tools::find() {
        Ok(tools) => tools.dbus_daemon,
        Err(reason) => {
            unavailable::<()>("the_private_bus_records_what_its_fakes_get", &reason, required()).unwrap();
            return;
        }
    };
    let dir = TempDir::new().unwrap();
    let bus = PrivateBus::start(&dbus_daemon, &dir.path().join("bus"), dir.path()).unwrap();
    let client = bus.connect().unwrap();
    check_notifications(&bus, &client).unwrap();
    check_watcher(&bus, &client).unwrap();
    check_action_invoked(&bus, &client).unwrap();
    check_no_activation(&client).unwrap();
    assert!(bus.client_pids().unwrap().contains(&std::process::id()));
}

/// A probe started outside a sandbox opens no window and never connects to the compositor it was given.
#[test]
fn the_probe_refuses_a_compositor_outside_the_sandbox() {
    let dir = TempDir::new().unwrap();
    let run = private_dir(dir.path().join("run")).unwrap();
    let socket = run.join("wayland-1");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    for sandbox in [None, Some(TempDir::new().unwrap().path().to_owned())] {
        let mut command = clean_command(PROBE);
        command
            .env("XDG_RUNTIME_DIR", &run)
            .env("WAYLAND_DISPLAY", &socket)
            .env("DISPLAY", ":4242");
        if let Some(sandbox) = &sandbox {
            command.env(SANDBOX_ENV, sandbox);
        }
        let output = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{sandbox:?} accepted: {stderr}");
        assert!(stderr.contains("not a private compositor"), "{stderr}");
        assert!(output.stdout.is_empty(), "{}", String::from_utf8_lossy(&output.stdout));
    }
    let accepted = listener.accept();
    assert!(
        accepted
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock),
        "the probe connected to the compositor: {accepted:?}"
    );
}

/// The sockets planted on the desktop session's addresses, each listening and never accepting.
struct Canaries {
    dir: TempDir,
    listeners: Vec<(PathBuf, UnixListener)>,
}

impl Canaries {
    fn plant() -> SandboxResult<Self> {
        let dir = TempDir::new()?;
        private_dir(dir.path().join("run"))?;
        let mut listeners = Vec::new();
        for name in [
            "bus",
            "wayland-0",
            "niri.sock",
            "run/bus",
            "run/wayland-0",
            "run/wayland-1",
        ] {
            let path = dir.path().join(name);
            let listener = UnixListener::bind(&path)?;
            listener.set_nonblocking(true)?;
            listeners.push((path, listener));
        }
        Ok(Self { dir, listeners })
    }

    /// The session's variables, every one pointing at a canary.
    fn session_env(&self) -> Vec<(&'static str, String)> {
        let path = |name: &str| self.dir.path().join(name).to_string_lossy().into_owned();
        vec![
            ("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", path("bus"))),
            ("WAYLAND_DISPLAY", path("wayland-0")),
            ("NIRI_SOCKET", path("niri.sock")),
            ("XDG_RUNTIME_DIR", path("run")),
            ("DISPLAY", ":4242".to_owned()),
            (CANARY_ENV, self.dir.path().to_string_lossy().into_owned()),
        ]
    }

    /// The canaries something connected to.
    fn reached(&self) -> Vec<&Path> {
        self.listeners
            .iter()
            .filter(|(_, listener)| listener.accept().is_ok())
            .map(|(path, _)| path.as_path())
            .collect()
    }
}

/// Runs [`everything_together_with_canaries_planted`] in a child test process whose environment names canaries for
/// the session's bus, compositor, niri and runtime dir; the child must pass and no canary may be reached.
#[test]
fn the_desktop_session_is_never_reached() {
    if let Err(reason) = Tools::find() {
        unavailable::<()>("the_desktop_session_is_never_reached", &reason, required()).unwrap();
        return;
    }
    let canaries = Canaries::plant().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([CANARY_CHILD, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .envs(canaries.session_env())
        .env_remove("WAYLAND_SOCKET")
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
    assert!(
        reached.is_empty(),
        "the desktop session was reached: {reached:?}\n{stderr}"
    );
}

/// The variables of process `pid` as it was started.
fn environ(pid: u32) -> SandboxResult<Vec<(String, String)>> {
    let raw = fs::read(format!("/proc/{pid}/environ"))?;
    Ok(raw
        .split(|&byte| byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            entry
                .split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect())
}

/// No session variable that isn't the sandbox's, and nothing naming a canary.
fn check_env(who: &str, env: &[(String, String)], sandbox: &Sandbox, canary: &str) {
    let dir = sandbox.dir();
    for (key, value) in env {
        assert!(!value.contains(canary), "{who}: {key}={value} names a canary");
        assert!(
            !["DISPLAY", "NIRI_SOCKET", "WAYLAND_SOCKET"].contains(&key.as_str()),
            "{who} inherited {key}={value}"
        );
        if ["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS"].contains(&key.as_str()) {
            assert!(
                value.contains(&*dir.to_string_lossy()),
                "{who}: {key}={value} outside the sandbox"
            );
        }
    }
}

/// A client's stdout and what it printed so far.
struct Lines {
    running: Running,
    seen: Vec<String>,
}

impl Lines {
    /// The first line, seen or to come within `timeout`, that `wanted` accepts.
    fn wait_for(&mut self, timeout: Duration, wanted: impl Fn(&str) -> bool) -> SandboxResult<String> {
        if let Some(line) = self.seen.iter().find(|line| wanted(line)) {
            return Ok(line.clone());
        }
        let start = Instant::now();
        loop {
            let left = timeout.checked_sub(start.elapsed()).unwrap_or_default();
            match self.running.line(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if wanted(&line) {
                        return Ok(line);
                    }
                }
                Err(LineError::Timeout) => return Err(format!("not within {timeout:?}; seen {:?}", self.seen).into()),
                Err(LineError::Closed) => return Err(format!("stdout closed; seen {:?}", self.seen).into()),
            }
        }
    }

    /// The characters of every key line so far, in order.
    fn typed(&self) -> String {
        self.seen
            .iter()
            .filter_map(|line| {
                line.strip_prefix("key ")?
                    .split_once(" char=Some(\"")?
                    .1
                    .strip_suffix("\")")
            })
            .collect()
    }
}

fn probe_colour_share(png: &Path) -> SandboxResult<f64> {
    let shot = image::open(png)?.to_rgb8();
    let near = shot
        .pixels()
        .filter(|pixel| {
            pixel
                .0
                .iter()
                .zip(PROBE_COLOR)
                .all(|(&channel, wanted)| channel.abs_diff(wanted) <= 8)
        })
        .count();
    let total = u64::from(shot.width()) * u64::from(shot.height());
    Ok(f64::from(u32::try_from(near)?) / f64::from(u32::try_from(total)?))
}

/// The client's environment and bus: the sandbox's, nothing of the session's.
fn check_client_env(sandbox: &Sandbox, canary: &str) -> SandboxResult {
    let output = sandbox.spawn(Path::new("env"), &[], &[])?.wait_with_output()?;
    let env: Vec<(String, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect();
    check_env("a client", &env, sandbox, canary);
    let value = |key: &str| env.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
    assert_eq!(
        value("WAYLAND_DISPLAY"),
        Some(sandbox.wayland_socket().to_string_lossy().into_owned())
    );
    assert_eq!(
        value("DBUS_SESSION_BUS_ADDRESS").as_deref(),
        Some(sandbox.bus().address())
    );
    for pid in sandbox.helper_pids().into_iter().chain([sandbox.bus().pid()]) {
        check_env(&format!("helper {pid}"), &environ(pid)?, sandbox, canary);
    }
    let names = sandbox
        .spawn(
            Path::new("dbus-send"),
            &[
                "--session",
                "--print-reply",
                "--dest=org.freedesktop.DBus",
                "/",
                "org.freedesktop.DBus.ListNames",
            ],
            &[],
        )?
        .wait_with_output()?;
    let names = String::from_utf8_lossy(&names.stdout);
    assert!(
        names.contains(NOTIFICATIONS_NAME) && names.contains(WATCHER_NAME),
        "{names}"
    );
    Ok(())
}

/// The probe window: it opens and draws, gets typed text, a chord, Enter and a click, and its `open_url` reaches
/// the opener; it holds a connection to the private bus only; it closes in time without an error logged.
fn check_probe(sandbox: &Sandbox) -> SandboxResult {
    let url = "https://example.invalid/ui-sandbox?probe=1";
    let running = Running::new(sandbox.spawn(Path::new(PROBE), &["--open-url", url], &[])?)?;
    let mut lines = Lines {
        running,
        seen: Vec::new(),
    };
    let checked = drive_probe(sandbox, &mut lines, url);
    let Lines { mut running, .. } = lines;
    checked.map_err(|error| running.fail(&error))?;
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = running.finish(CLOSE)?;
    assert!(status.success(), "exit {status}: {stderr}");
    let errors: Vec<_> = stderr.lines().filter(|line| line.starts_with("[ERROR")).collect();
    assert!(errors.is_empty(), "errors logged: {errors:#?}");
    Ok(())
}

fn drive_probe(sandbox: &Sandbox, lines: &mut Lines, url: &str) -> SandboxResult {
    lines.wait_for(STARTUP, |line| line == "ready")?;
    lines.wait_for(ECHO, |line| line == "open-url")?;
    let wanted = Opened {
        launcher: "xdg-open".to_owned(),
        args: vec![url.to_owned()],
    };
    ui_sandbox::wait_until(ECHO, || sandbox.opened().ok().filter(|opened| opened.contains(&wanted)))
        .ok_or_else(|| format!("the opener never got {url}: {:?}", sandbox.opened()))?;
    let windows = sandbox.niri_msg(&["windows"])?;
    assert!(
        windows.contains(PROBE_APP_ID),
        "niri does not list the window: {windows}"
    );
    let share = probe_colour_share(&sandbox.screenshot("probe.png", false)?)?;
    assert!(share > 0.9, "the probe's colour covers {share:.2} of the screen");
    assert!(
        sandbox.bus().client_pids()?.contains(&lines.running.pid()),
        "the probe holds no connection to the private bus"
    );

    lines.wait_for(ECHO, |line| line == "active true")?;
    let mut input = sandbox.input()?;
    let text = "Hello, sandbox! 42";
    input.type_text(text)?;
    lines.wait_for(ECHO, |line| line.ends_with("char=Some(\"2\")"))?;
    assert_eq!(lines.typed(), text);
    let (k, _) = input.keymap().key_for('k').ok_or("no k")?;
    input.key_with(&[Modifier::Ctrl], k)?;
    lines.wait_for(ECHO, |line| {
        line.starts_with("key ctrl=true shift=false alt=false name=k ")
    })?;
    input.key(keys::ENTER)?;
    lines.wait_for(ECHO, |line| line.contains(" name=enter "))?;
    input.click(400, 300, keys::BTN_LEFT)?;
    lines.wait_for(ECHO, |line| line == "down x=400 y=300 button=Left")?;
    input.click(400, 300, keys::BTN_RIGHT)?;
    lines.wait_for(ECHO, |line| line == "down x=400 y=300 button=Right")?;
    // a button held (a drag's start) is a press where the pointer is, and its release lets it go
    input.button_down(keys::BTN_LEFT)?;
    lines.wait_for(ECHO, |line| line == "down x=400 y=300 button=Left")?;
    input.button_up(keys::BTN_LEFT)?;
    input.click(400, 300, keys::BTN_RIGHT)?;
    lines.wait_for(ECHO, |line| line == "down x=400 y=300 button=Right")?;
    assert!(input.type_text("☃").is_err());
    let logs = sandbox.logs();
    assert!(logs.contains("weston.log") && logs.contains("niri.log"));
    Ok(())
}

/// Everything together in one sandbox; run only as [`the_desktop_session_is_never_reached`]'s child, whose
/// environment names canaries for the session.
#[test]
#[ignore = "run by the_desktop_session_is_never_reached, in a child process with canaries for the session"]
fn everything_together_with_canaries_planted() {
    let canary = std::env::var(CANARY_ENV).expect("run by the_desktop_session_is_never_reached");
    assert!(!canary.is_empty() && OsStr::new(&canary) != OsStr::new("/"));
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        refresh_mhz: Some(60_000),
        ..SandboxOptions::default()
    };
    let Some(sandbox) = ui_sandbox::sandbox_or_skip(CANARY_CHILD, &options).unwrap() else {
        return;
    };
    let checked = check_client_env(&sandbox, &canary).and_then(|()| check_probe(&sandbox));
    if let Err(error) = checked {
        panic!("{error}\n{}", sandbox.logs());
    }
}

#[test]
fn a_failed_running_client_is_reaped_and_retains_its_stderr() {
    let child = Command::new("/bin/sh")
        .args([
            "-c",
            "printf 'synthetic stderr\\n' >&2; printf 'ready\\n'; exec /bin/sleep 30",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut running = Running::new(child).unwrap();
    assert_eq!(running.line(Duration::from_secs(5)).unwrap(), "ready");
    let error = running.fail(&"synthetic failure").to_string();
    assert!(error.contains("synthetic failure"));
    assert!(error.contains("synthetic stderr"));
    assert!(error.contains("client:"));
    assert_eq!(running.take_stderr(), "<stderr already taken>");
}
