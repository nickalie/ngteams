//! The UI sandbox: everything a GUI test or a lab run of the app needs, none of it the desktop's.
//!
//! - **A private compositor**: a headless weston (GL renderer, `--fake-seat`: GPUI needs a `wl_seat`) with a niri
//!   nested in it, each with its own `XDG_RUNTIME_DIR` in the sandbox's temp dir. A client gets the nested niri's
//!   socket as an absolute `WAYLAND_DISPLAY`; `niri msg` (screenshots, window list, close) always gets its
//!   `NIRI_SOCKET`.
//! - **A private D-Bus session bus** ([`bus`]): a `dbus-daemon` on a socket in the client's runtime dir, with a
//!   config of our own (no service activation, so nothing of the desktop is started on it), holding a fake
//!   notification server and a fake `StatusNotifierItem` watcher that record what they get, and with the private
//!   PipeWire a fake camera portal handing out remotes on it.
//! - **Virtual input** ([`input`]): a keyboard and a pointer in the nested niri, through its
//!   `zwp_virtual_keyboard_manager_v1` and `zwlr_virtual_pointer_manager_v1`.
//! - **A clipboard of its own** ([`clipboard`]): content put on the nested niri's selection through its data-control
//!   protocol for the app to paste, and what the selection holds read back; the desktop's clipboard is never touched.
//! - **A no-op URI opener** ([`opener`]): `xdg-open` (and the other launchers GPUI tries) first on the client's
//!   `PATH`, recording what it was asked to open and opening nothing.
//! - **A private PipeWire** ([`pipewire`], when [`SandboxOptions::pipewire`] asks): its own server and session
//!   manager, a microphone playing a given tone (with the speaker's echo fed back when asked), a speaker recorded to a
//!   file, the test pattern as the camera; no device plugin, no D-Bus.
//!
//! Every child's environment is built from scratch ([`clean_command`]): no `WAYLAND_DISPLAY`, `DISPLAY`, `NIRI_SOCKET`,
//! D-Bus address or `XDG_RUNTIME_DIR` of the session leaks in, and every descriptor it would inherit is closed but the
//! ones its caller names ([`spawn`]). So a sandboxed process reaches the sandbox's compositor and bus only, and the
//! lab's credentials reach it only on a socket the caller hands it.
//!
//! Frame pacing: weston's headless output repaints on a timer at its refresh rate (60 Hz unless set), the nested niri
//! draws when weston's frame callback arrives, and GPUI draws when niri's frame callback arrives. Nothing scans out
//! to a display, so there's no vblank: the cadence is weston's timer.
#![cfg(target_os = "linux")]

pub mod bus;
pub mod clipboard;
pub mod input;
pub mod opener;
pub mod pipewire;
pub mod spawn;

use std::{
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs,
    io::{BufRead as _, BufReader, Read as _},
    os::{fd::BorrowedFd, unix::fs::PermissionsExt as _},
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

use crate::{
    bus::PrivateBus,
    clipboard::{Clipboard, Offers},
    input::VirtualInput,
    opener::Opened,
    pipewire::{PipeWireOptions, PipeWireTools, PrivatePipeWire},
};

/// Errors of the harness: anything goes, with a readable message.
pub type SandboxResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The opt-in a sandboxed GUI binary needs before it opens a window: the absolute path of the sandbox's temp dir,
/// which its `WAYLAND_DISPLAY` must lie in ([`window_allowed`]). [`Sandbox::spawn`] sets it; a desktop session never
/// has it.
pub const SANDBOX_ENV: &str = "UI_SANDBOX_DIR";

/// Set to `1`, a missing weston 15, niri or `dbus-daemon` fails a sandbox test instead of skipping it.
pub const REQUIRE_ENV: &str = "GPUI_SMOKE_REQUIRE";

/// How long a compositor or the bus gets to create its sockets, and niri to write a screenshot.
pub const SOCKET_WAIT: Duration = Duration::from_secs(15);

const WESTON_CONFIG: &str =
    "[core]\nidle-time=0\n\n[shell]\npanel-position=none\nlocking=false\n\n[input-method]\npath=\n";
/// A private niri: no Xwayland, no hotkey overlay, no animations.
const NIRI_CONFIG: &str = "hotkey-overlay {\n    skip-at-startup\n}\nxwayland-satellite {\n    off\n}\n\
animations {\n    off\n}\nprefer-no-csd\n";
/// A niri layout where a window takes the whole output: no gaps, no focus ring, no border, full-width column.
pub const NIRI_FULL_WINDOW: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n\
    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\n";

/// The weston output, the niri config, and the private PipeWire if one is wanted.
#[derive(Clone, Debug, PartialEq)]
pub struct SandboxOptions {
    pub width: u32,
    pub height: u32,
    /// weston's headless refresh rate in mHz; `None` keeps weston's default (60 Hz).
    pub refresh_mhz: Option<u32>,
    /// Appended to the niri config.
    pub niri_extra: &'static str,
    /// A private PipeWire ([`pipewire`]) with these sounds and camera; `None` runs none.
    pub pipewire: Option<PipeWireOptions>,
    /// Whether niri fills weston's output (weston's kiosk shell) rather than opening at its own size: a screen other
    /// than the default's 1280×800.
    pub fill_output: bool,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            refresh_mhz: None,
            niri_extra: "",
            pipewire: None,
            fill_output: false,
        }
    }
}

/// The variables a sandboxed child takes from this process: only these, never the session's display, bus or
/// runtime dir. `LLVM_PROFILE_FILE` keeps instrumented children in the coverage run's output directory;
/// `WGPU_BACKEND` selects graphics APIs, so CPU-only tests can use Vulkan's software renderer without probing GL.
pub const PASSED_ENV: [&str; 5] = ["PATH", "HOME", "LANG", "LLVM_PROFILE_FILE", "WGPU_BACKEND"];

/// A command whose environment holds only [`PASSED_ENV`] of this process.
pub fn clean_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for name in PASSED_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

pub(crate) fn on_path(path: &OsStr, program: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name).extension().is_some_and(|actual| actual == extension)
}

fn file_name_matches(path: &Path, wanted: impl Fn(&str) -> bool) -> bool {
    path.file_name().is_some_and(|name| wanted(&name.to_string_lossy()))
}

/// Mesa's EGL vendor file, so weston and niri render through Mesa rather than another vendor's EGL.
fn mesa_egl_vendor() -> Option<PathBuf> {
    ["/usr/share/glvnd/egl_vendor.d", "/etc/glvnd/egl_vendor.d"]
        .iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| file_name_matches(path, |name| name.contains("mesa") && has_extension(name, "json")))
}

/// Polls `probe` every 50 ms until it gives a value or `deadline` passes.
pub fn wait_until<T>(deadline: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if start.elapsed() > deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The first entry of `dir` whose name `wanted` accepts, once there is one.
fn wait_for_entry(dir: &Path, wanted: fn(&str) -> bool) -> Option<PathBuf> {
    wait_until(SOCKET_WAIT, || {
        fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| file_name_matches(path, wanted))
    })
}

/// Creates `path` (mode 0700) and returns it.
///
/// # Errors
/// The directory can't be made.
pub fn private_dir(path: PathBuf) -> SandboxResult<PathBuf> {
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

/// Whether a `WAYLAND_DISPLAY` value names a compositor at all.
#[must_use]
pub fn names_wayland_display(value: Option<&OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// Whether `WAYLAND_DISPLAY` is a socket inside the sandbox dir that [`SANDBOX_ENV`] (`sandbox`) opts in to: both
/// absolute, no `..` in the socket path, the socket under the dir. A bare socket name (the desktop session's
/// `wayland-1`, resolved in its `XDG_RUNTIME_DIR`) never passes.
#[must_use]
pub fn in_sandbox(wayland: Option<&OsStr>, sandbox: Option<&OsStr>) -> bool {
    let (Some(wayland), Some(sandbox)) = (wayland.map(Path::new), sandbox.map(Path::new)) else {
        return false;
    };
    wayland.is_absolute()
        && sandbox.is_absolute()
        && !wayland.components().any(|part| part == Component::ParentDir)
        && wayland.starts_with(sandbox)
        && wayland != sandbox
}

/// Whether this process may open a window: its `WAYLAND_DISPLAY` names a compositor, and that compositor is a
/// sandbox's ([`in_sandbox`]). A GUI binary checks this before GPUI connects to anything.
///
/// # Errors
/// Why not, as the message to print.
pub fn window_allowed() -> Result<(), String> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY");
    if !names_wayland_display(wayland.as_deref()) {
        return Err("no Wayland compositor: WAYLAND_DISPLAY is unset or empty (this build never uses X11)".to_owned());
    }
    if !in_sandbox(wayland.as_deref(), std::env::var_os(SANDBOX_ENV).as_deref()) {
        return Err(format!(
            "not a private compositor: WAYLAND_DISPLAY must be an absolute socket path inside the dir {SANDBOX_ENV} \
             names (the UI sandbox sets both); refusing to open a window on the desktop session"
        ));
    }
    Ok(())
}

/// The programs the sandbox runs.
#[derive(Clone, Debug)]
pub struct Tools {
    pub weston: PathBuf,
    pub niri: PathBuf,
    pub dbus_daemon: PathBuf,
}

impl Tools {
    /// The tools on this process's `PATH`.
    ///
    /// # Errors
    /// See [`Tools::find_on`].
    pub fn find() -> Result<Self, String> {
        Self::find_on(&std::env::var_os("PATH").unwrap_or_default())
    }

    /// The tools on `path`, or why they can't run here.
    ///
    /// # Errors
    /// No weston 15 or later (one that takes `--fake-seat`), no niri or no `dbus-daemon` on `path`.
    pub fn find_on(path: &OsStr) -> Result<Self, String> {
        let weston = on_path(path, "weston")
            .filter(|weston| seated(weston))
            .ok_or("no weston with --fake-seat on PATH (weston 15.0 or later)")?;
        let niri = on_path(path, "niri").ok_or("no niri on PATH")?;
        let dbus_daemon = on_path(path, "dbus-daemon").ok_or("no dbus-daemon on PATH")?;
        Ok(Self {
            weston,
            niri,
            dbus_daemon,
        })
    }
}

/// Whether `weston` takes `--fake-seat` (weston 15.0 and later).
fn seated(weston: &Path) -> bool {
    let mut command = clean_command(weston);
    command.arg("--help");
    spawn::pass_only(&mut command, &[]);
    spawn::output(&mut command).is_ok_and(|help| String::from_utf8_lossy(&help.stdout).contains("--fake-seat"))
}

/// What a sandbox test does when the sandbox can't run here: `None` (with a note on stderr), or an error when
/// `require` (`GPUI_SMOKE_REQUIRE=1`).
///
/// # Errors
/// `require` is set.
pub fn unavailable<T>(test: &str, reason: &str, require: bool) -> SandboxResult<Option<T>> {
    if require {
        return Err(format!("{test}: {reason}, and {REQUIRE_ENV}=1").into());
    }
    eprintln!("skipping {test}: {reason}");
    Ok(None)
}

/// Whether `GPUI_SMOKE_REQUIRE=1`.
#[must_use]
pub fn required() -> bool {
    std::env::var_os(REQUIRE_ENV).is_some_and(|value| value == "1")
}

/// The sandbox, or `None` (with a note) when its tools aren't here and `GPUI_SMOKE_REQUIRE` isn't `1`.
///
/// # Errors
/// The tools are missing and `GPUI_SMOKE_REQUIRE=1`, or they fail to start.
pub fn sandbox_or_skip(test: &str, options: &SandboxOptions) -> SandboxResult<Option<Sandbox>> {
    if options.pipewire.is_some()
        && let Err(reason) = PipeWireTools::find()
    {
        return unavailable(test, &reason, required());
    }
    match Tools::find() {
        Ok(tools) => Ok(Some(Sandbox::start(&tools, options)?)),
        Err(reason) => unavailable(test, &reason, required()),
    }
}

/// A private headless weston with a niri nested in it, a private bus, the opener, and a private PipeWire when asked;
/// all stopped on drop.
pub struct Sandbox {
    /// Dropped (stopped) before the dir they live in.
    bus: PrivateBus,
    pipewire: Option<PrivatePipeWire>,
    /// What [`Sandbox::set_clipboard`] put up, served until replaced or cleared.
    clipboard: Mutex<Option<Clipboard>>,
    helpers: Vec<Child>,
    dir: TempDir,
    options: SandboxOptions,
    /// The nested niri's runtime dir, its Wayland socket and its IPC socket.
    niri_run: PathBuf,
    wayland: PathBuf,
    niri_socket: PathBuf,
    /// The opener's dir, first on a client's `PATH`.
    bin: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // stopped while the compositor is still there to take its offer back
        if let Ok(mut clipboard) = self.clipboard.lock() {
            clipboard.take();
        }
        for child in self.helpers.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Sandbox {
    /// Starts weston, then niri nested in it, then the bus and its fakes, and installs the opener; then the private
    /// PipeWire, its session manager and its camera and recording when [`SandboxOptions::pipewire`] asks.
    ///
    /// # Errors
    /// A compositor or the bus fails to start or make its sockets in time, or (when asked for) the PipeWire tools
    /// aren't here or the private PipeWire fails to start (see [`PrivatePipeWire::start`]).
    pub fn start(tools: &Tools, options: &SandboxOptions) -> SandboxResult<Self> {
        let dir = TempDir::new()?;
        let niri_run = private_dir(dir.path().join("niri-run"))?;
        let bin = private_dir(dir.path().join("bin"))?;
        opener::install(&bin, &dir.path().join(opener::LOG))?;
        let bus = PrivateBus::start(&tools.dbus_daemon, &niri_run.join("bus"), dir.path())?;
        let mut sandbox = Self {
            bus,
            pipewire: None,
            clipboard: Mutex::new(None),
            helpers: Vec::new(),
            dir,
            options: options.clone(),
            niri_run,
            wayland: PathBuf::new(),
            niri_socket: PathBuf::new(),
            bin,
        };
        let parent = sandbox.start_weston(&tools.weston)?;
        sandbox.start_niri(&tools.niri, &parent)?;
        if let Some(pipewire) = &options.pipewire {
            let tools = PipeWireTools::find()?;
            let started = PrivatePipeWire::start(&tools, sandbox.dir.path(), pipewire)?;
            sandbox
                .bus
                .start_camera_portal(&started.socket(), started.screen().node)?;
            sandbox.pipewire = Some(started);
        }
        Ok(sandbox)
    }

    /// The private PipeWire, when [`SandboxOptions::pipewire`] asked for one.
    #[must_use]
    pub fn pipewire(&self) -> Option<&PrivatePipeWire> {
        self.pipewire.as_ref()
    }

    /// The sandbox's temp dir.
    #[must_use]
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// A path in the sandbox's temp dir.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The nested niri's Wayland socket (a client's `WAYLAND_DISPLAY`).
    #[must_use]
    pub fn wayland_socket(&self) -> &Path {
        &self.wayland
    }

    /// The private bus and what its fakes recorded.
    #[must_use]
    pub fn bus(&self) -> &PrivateBus {
        &self.bus
    }

    /// The process ids of the compositors (weston, then niri).
    #[must_use]
    pub fn helper_pids(&self) -> Vec<u32> {
        self.helpers.iter().map(Child::id).collect()
    }

    /// Everything the opener was asked to open so far, in order.
    ///
    /// # Errors
    /// Its log can't be read.
    pub fn opened(&self) -> SandboxResult<Vec<Opened>> {
        opener::read_log(&self.path(opener::LOG))
    }

    /// A virtual keyboard and pointer in the nested niri, the pointer's absolute coordinates over the output.
    ///
    /// # Errors
    /// The compositor refuses the connection or doesn't offer the protocols.
    pub fn input(&self) -> SandboxResult<VirtualInput> {
        VirtualInput::connect(&self.wayland, (self.options.width, self.options.height))
    }

    /// Puts `offers` on the nested niri's clipboard, each MIME type answered with its bytes, as a copy in another
    /// program would: the app can paste them once its window has keyboard focus. They are served until the next
    /// call, [`Sandbox::clear_clipboard`], a copy in the app, or the sandbox's end.
    ///
    /// # Errors
    /// See [`Clipboard::offer`].
    pub fn set_clipboard(&self, offers: Offers) -> SandboxResult {
        let served = Clipboard::offer(&self.wayland, offers)?;
        let replaced = self
            .clipboard
            .lock()
            .map_err(|_| "the clipboard's lock is poisoned")?
            .replace(served);
        // stopped outside the lock; the compositor has cancelled it already, the new offer holding the selection
        drop(replaced);
        Ok(())
    }

    /// Stops serving what [`Sandbox::set_clipboard`] put up; the clipboard is empty then unless something newer took
    /// it.
    ///
    /// # Errors
    /// The clipboard's lock is poisoned.
    pub fn clear_clipboard(&self) -> SandboxResult {
        let cleared = self
            .clipboard
            .lock()
            .map_err(|_| "the clipboard's lock is poisoned")?
            .take();
        drop(cleared);
        Ok(())
    }

    /// What the nested niri's clipboard holds now, whoever put it up ([`Sandbox::set_clipboard`] or a copy in the
    /// app), each MIME type read in full; empty when the clipboard is.
    ///
    /// # Errors
    /// See [`clipboard::read`].
    pub fn read_clipboard(&self) -> SandboxResult<Offers> {
        clipboard::read(&self.wayland)
    }

    fn with_mesa_egl(command: &mut Command) {
        if let Some(vendor) = mesa_egl_vendor() {
            command.env("__EGL_VENDOR_LIBRARY_FILENAMES", vendor);
        }
    }

    fn start_weston(&mut self, weston: &Path) -> SandboxResult<PathBuf> {
        let run = private_dir(self.path("weston-run"))?;
        let config = self.path("weston.ini");
        fs::write(&config, WESTON_CONFIG)?;
        let options = &self.options;
        let mut command = clean_command(weston);
        command
            .args([
                "--backend=headless",
                "--renderer=gl",
                "--socket=wayland-0",
                "--fake-seat",
            ])
            .args(options.fill_output.then_some("--shell=kiosk-shell.so"))
            .arg(format!("--width={}", options.width))
            .arg(format!("--height={}", options.height))
            .args(options.refresh_mhz.map(|rate| format!("--refresh-rate={rate}")))
            .arg(format!("--config={}", config.display()))
            .arg(format!("--log={}", self.path("weston.log").display()))
            .env("XDG_RUNTIME_DIR", &run)
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.path("weston.stderr"))?);
        Self::with_mesa_egl(&mut command);
        spawn::pass_only(&mut command, &[]);
        self.helpers.push(command.spawn()?);
        let socket = wait_for_entry(&run, |name| name == "wayland-0");
        Ok(socket.ok_or_else(|| format!("weston made no socket: {}", self.logs()))?)
    }

    fn start_niri(&mut self, niri: &Path, parent: &Path) -> SandboxResult {
        let config = self.path("niri.kdl");
        fs::write(&config, format!("{NIRI_CONFIG}{}", self.options.niri_extra))?;
        let mut command = clean_command(niri);
        command
            .arg("-c")
            .arg(&config)
            .env("WAYLAND_DISPLAY", parent)
            .env("XDG_RUNTIME_DIR", &self.niri_run)
            .env("NIRI_DISABLE_SYSTEM_MANAGER_NOTIFY", "1")
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.path("niri.log"))?);
        Self::with_mesa_egl(&mut command);
        spawn::pass_only(&mut command, &[]);
        self.helpers.push(command.spawn()?);
        let wayland = wait_for_entry(&self.niri_run, |name| {
            name.starts_with("wayland-") && !has_extension(name, "lock")
        });
        let ipc = wait_for_entry(&self.niri_run, |name| {
            name.starts_with("niri.") && has_extension(name, "sock")
        });
        let (Some(wayland), Some(ipc)) = (wayland, ipc) else {
            return Err(format!("nested niri made no sockets: {}", self.logs()).into());
        };
        self.wayland = wayland;
        self.niri_socket = ipc;
        Ok(())
    }

    /// The compositors' and the bus's logs, for a failure message.
    #[must_use]
    pub fn logs(&self) -> String {
        let mut logs = String::new();
        for name in ["weston.log", "weston.stderr", "niri.log", bus::LOG] {
            let text = fs::read_to_string(self.path(name)).unwrap_or_default();
            let _ = write!(logs, "\n--- {name}\n{text}");
        }
        if let Some(pipewire) = &self.pipewire {
            logs.push_str(&pipewire.logs());
        }
        logs
    }

    /// `niri msg` against the nested niri only; returns its stdout.
    ///
    /// # Errors
    /// `niri msg` can't run or fails.
    pub fn niri_msg(&self, args: &[&str]) -> SandboxResult<String> {
        let mut command = clean_command("niri");
        command
            .arg("msg")
            .args(args)
            .env("NIRI_SOCKET", &self.niri_socket)
            .env("XDG_RUNTIME_DIR", &self.niri_run);
        spawn::pass_only(&mut command, &[]);
        let output = command.output()?;
        if !output.status.success() {
            return Err(format!("niri msg {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Screenshots the focused window (`window: true`) or the whole screen through the nested niri into `name` in
    /// the sandbox dir, and returns its path once the file is readable.
    ///
    /// # Errors
    /// niri refuses, or no image appears in time.
    pub fn screenshot(&self, name: &str, window: bool) -> SandboxResult<PathBuf> {
        let png = self.path(name);
        let png_arg = png.to_string_lossy().into_owned();
        let action = if window {
            "screenshot-window"
        } else {
            "screenshot-screen"
        };
        self.niri_msg(&["action", action, "--show-pointer", "false", "--path", &png_arg])?;
        wait_until(SOCKET_WAIT, || image::open(&png).ok().map(|_| ()))
            .ok_or_else(|| format!("no screenshot at {}", png.display()))?;
        Ok(png)
    }

    /// The environment a client of the sandbox gets, on top of [`PASSED_ENV`]: the nested niri's runtime dir and
    /// socket, the sandbox opt-in, the private bus, the opener first on `PATH`, and the private PipeWire's variables
    /// when there is one ([`PrivatePipeWire::client_env`]).
    #[must_use]
    pub fn client_env(&self) -> Vec<(&'static str, OsString)> {
        let mut path = OsString::from(self.bin.as_os_str());
        if let Some(inherited) = std::env::var_os("PATH") {
            path.push(":");
            path.push(inherited);
        }
        let mut env = vec![
            ("XDG_RUNTIME_DIR", self.niri_run.clone().into_os_string()),
            ("WAYLAND_DISPLAY", self.wayland.clone().into_os_string()),
            (SANDBOX_ENV, self.dir.path().as_os_str().to_owned()),
            ("DBUS_SESSION_BUS_ADDRESS", self.bus.address().into()),
            ("PATH", path),
        ];
        let pipewire = self.pipewire.iter().flat_map(PrivatePipeWire::client_env);
        env.extend(pipewire.filter(|(name, _)| *name != SANDBOX_ENV));
        env
    }

    /// `program` with `args` as a client of the sandbox ([`Sandbox::client_env`]), with `extra_env` on top; stdout
    /// and stderr piped; no inherited descriptor but stdin, stdout and stderr.
    ///
    /// # Errors
    /// The program can't be started.
    pub fn spawn(&self, program: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> SandboxResult<Child> {
        self.spawn_with_fds(program, args, extra_env, &[])
    }

    /// [`Sandbox::spawn`], passing `fds` through under their own numbers (the credential broker's socket, the view
    /// trace): the child inherits these and stdin, stdout and stderr, and nothing else.
    ///
    /// # Errors
    /// The program can't be started, or a named descriptor isn't open.
    pub fn spawn_with_fds(
        &self,
        program: &Path,
        args: &[&str],
        extra_env: &[(&str, &str)],
        fds: &[BorrowedFd<'_>],
    ) -> SandboxResult<Child> {
        let mut command = clean_command(program);
        command
            .args(args)
            .envs(self.client_env())
            .envs(extra_env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        spawn::pass_only(&mut command, fds);
        Ok(command.spawn()?)
    }
}

/// A running client: its stdout lines as they come, its stderr once it exits. Killed on drop.
pub struct Running {
    child: Child,
    lines: mpsc::Receiver<String>,
    stderr: Option<thread::JoinHandle<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Why no stdout line came.
#[derive(Debug, PartialEq, Eq)]
pub enum LineError {
    Timeout,
    /// stdout closed: the process exited (or closed it).
    Closed,
}

impl Running {
    /// Starts reading `child`'s piped stdout and stderr.
    ///
    /// # Errors
    /// The child's stdout or stderr isn't piped.
    pub fn new(mut child: Child) -> SandboxResult<Self> {
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let stderr = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Ok(Self {
            child,
            lines,
            stderr: Some(stderr),
        })
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The next stdout line within `timeout`.
    ///
    /// # Errors
    /// None came in time, or stdout closed.
    pub fn line(&self, timeout: Duration) -> Result<String, LineError> {
        self.lines.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => LineError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => LineError::Closed,
        })
    }

    /// The first stdout line `wanted` accepts within `timeout`; the lines before it are skipped.
    ///
    /// # Errors
    /// None came in time, or stdout closed first.
    pub fn line_where(&self, timeout: Duration, wanted: impl Fn(&str) -> bool) -> Result<String, LineError> {
        let start = Instant::now();
        loop {
            let left = timeout.checked_sub(start.elapsed()).ok_or(LineError::Timeout)?;
            let line = self.line(left)?;
            if wanted(&line) {
                return Ok(line);
            }
        }
    }

    /// Everything the client wrote to stderr; it must have exited or been killed.
    pub fn take_stderr(&mut self) -> String {
        match self.stderr.take().map(thread::JoinHandle::join) {
            Some(Ok(text)) => text,
            Some(Err(_)) => "<stderr reader panicked>".to_owned(),
            None => "<stderr already taken>".to_owned(),
        }
    }

    /// Kills the client and returns `error` with its exit status and stderr attached.
    pub fn fail(&mut self, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let _ = self.child.kill();
        let status = self
            .child
            .wait()
            .map_or_else(|wait| format!("wait failed: {wait}"), |status| status.to_string());
        let stderr = self.take_stderr();
        format!("{error} (client: {status})\n--- client stderr\n{stderr}").into()
    }

    /// Waits up to `deadline` for the exit; returns the status and stderr.
    ///
    /// # Errors
    /// Still running at the deadline (it's killed, and the error carries its stderr).
    pub fn finish(mut self, deadline: Duration) -> SandboxResult<(ExitStatus, String)> {
        let Some(status) = wait_until(deadline, || self.child.try_wait().ok().flatten()) else {
            return Err(self.fail(&format!("still running {deadline:?} after the close request")));
        };
        Ok((status, self.take_stderr()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_non_empty_wayland_display_counts() {
        assert!(!names_wayland_display(None));
        assert!(!names_wayland_display(Some(OsStr::new(""))));
        assert!(names_wayland_display(Some(OsStr::new("wayland-1"))));
        assert!(names_wayland_display(Some(OsStr::new("/run/user/1000/wayland-1"))));
    }

    #[test]
    fn only_a_socket_inside_the_opted_in_sandbox_counts() {
        let sandbox = Some(OsStr::new("/tmp/.tmpAbc"));
        let inside = |wayland: &str| in_sandbox(Some(OsStr::new(wayland)), sandbox);
        assert!(inside("/tmp/.tmpAbc/niri-run/wayland-1"));
        // The desktop session: a bare name, or its runtime dir, or no opt-in at all.
        assert!(!inside("wayland-1"));
        assert!(!inside("/run/user/1000/wayland-1"));
        assert!(!in_sandbox(Some(OsStr::new("/tmp/.tmpAbc/niri-run/wayland-1")), None));
        // Escapes and look-alikes.
        assert!(!inside("/tmp/.tmpAbc/../../run/user/1000/wayland-1"));
        assert!(!inside("/tmp/.tmpAbcd/wayland-1"));
        assert!(!inside("/tmp/.tmpAbc"));
        assert!(!in_sandbox(
            Some(OsStr::new("niri-run/wayland-1")),
            Some(OsStr::new("niri-run"))
        ));
        assert!(!in_sandbox(None, sandbox));
    }
}
