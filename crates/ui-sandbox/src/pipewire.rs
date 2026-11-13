//! The sandbox's private PipeWire: its own server and session manager, with nothing of the desktop's.
//!
//! - **The server** (`pipewire`) runs on a config of our own: its socket in the sandbox's own runtime dir
//!   ([`SANDBOX_SOCKET`]), no D-Bus, no realtime portal, no device monitor, at 48 kHz. Its nodes: the speaker (a null
//!   sink, the default output), and the microphone (the default input): a filter graph mixing a given tone with the
//!   speaker's output fed back through a set delay and attenuation (the loopback the echo-cancellation check uses; off
//!   unless [`PipeWireOptions::echo`] is set).
//! - **The session manager** (`wireplumber`) links streams to those defaults, as the desktop's would, on a profile of
//!   our own: the policy only, with every hardware monitor (ALSA, Bluetooth, V4L2, libcamera) and every D-Bus feature
//!   (portal, reservation, logind, MPRIS) off.
//! - **The camera and the recording** ([`media`]): a thread of the process that owns the sandbox shows the test
//!   pattern as a video source (`sandbox-camera`, the default camera) and records the speaker to
//!   [`PrivatePipeWire::recording`].
//! - **Devices plugged in and out** ([`nodes`]): the same thread adds another microphone or speaker while the sandbox
//!   runs ([`PrivatePipeWire::add_microphone`], [`PrivatePipeWire::add_speaker`]) and removes it again.
//!
//! Both processes get an environment built from scratch: a home, config dirs and a runtime dir of the sandbox's, no
//! session bus, and the plugin and module dirs cut down to links to the few the sandbox needs (no ALSA, Bluetooth,
//! V4L2 or libcamera plugin, no D-Bus support library, no portal or realtime module), so neither can reach a device or
//! a service of the desktop even if a config asked. A client of the sandbox ([`PrivatePipeWire::client_env`]) gets
//! the same server, config and plugin dirs; `teams-media`'s PipeWire backend with sandbox reach opens only this
//! server, at its fixed place inside [`SANDBOX_ENV`](crate::SANDBOX_ENV)'s dir.

pub mod media;
pub mod nodes;
mod screen;
pub mod timeline;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use teams_media::devices::file::PatternCamera;
use teams_media::devices::pipewire::{SANDBOX_CLIENT_CONF, SANDBOX_SOCKET};
use teams_media::tone::Tone;

use crate::{SANDBOX_ENV, SOCKET_WAIT, SandboxResult, private_dir, spawn, wait_until};
use nodes::{Added, AddedNode, NodeSpec, Sound};
use timeline::Timeline;

/// The speaker's node (the default output).
pub const SPEAKER_NODE: &str = "sandbox-speaker";
/// The microphone's node (the default input).
pub const MICROPHONE_NODE: &str = "sandbox-microphone";
/// The camera's node (the default camera).
pub const CAMERA_NODE: &str = "sandbox-camera";
/// The screen's node: what the fake screen-cast portal shares.
pub const SCREEN_NODE: &str = "sandbox-screen";
/// The speaker's recording, relative to the sandbox dir.
pub const SPEAKER_RECORDING: &str = "recordings/speaker.wav";
/// The `priority.session` of the sandbox's own speaker, microphone and camera: a device added above it becomes the
/// system's default as it comes.
pub const DEVICE_PRIORITY: u32 = 2000;
/// WirePlumber's profile, defined by the sandbox's drop-in.
const PROFILE: &str = "ngteams-sandbox";
/// How long the server and the session manager get to stop on `SIGTERM` before they are killed.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// The SPA plugins the server and its clients may load (dir, library), by their system names.
const SPA_PLUGINS: [(&str, &str); 6] = [
    ("support", "libspa-support.so"),
    ("audioconvert", "libspa-audioconvert.so"),
    ("videoconvert", "libspa-videoconvert.so"),
    ("audiomixer", "libspa-audiomixer.so"),
    ("control", "libspa-control.so"),
    ("filter-graph", "libspa-filter-graph.so"),
];
/// The filter graph's built-in nodes (sine, delay, mixer), beside its library.
const FILTER_GRAPH_BUILTIN: (&str, &str) = ("filter-graph", "libspa-filter-graph-plugin-builtin.so");
/// The PipeWire modules the server and its clients may load.
const MODULES: [&str; 9] = [
    "protocol-native",
    "access",
    "metadata",
    "spa-node-factory",
    "client-node",
    "client-device",
    "adapter",
    "link-factory",
    "filter-chain",
];
/// WirePlumber's modules the profile uses.
const WIREPLUMBER_MODULES: [&str; 9] = [
    "default-nodes-api",
    "log-settings",
    "lua-scripting",
    "mixer-api",
    "settings",
    "si-audio-adapter",
    "si-node",
    "si-standard-link",
    "standard-event-source",
];

/// The microphone's echo: the speaker's output fed back into it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Echo {
    pub delay: Duration,
    /// How much quieter the echo is than what was played, in dB.
    pub attenuation_db: f32,
}

/// What the private PipeWire plays and shows.
#[derive(Clone, Debug, PartialEq)]
pub struct PipeWireOptions {
    /// The microphone's tone.
    pub tone: Tone,
    /// The loopback from the speaker into the microphone; `None` leaves the microphone the tone alone.
    pub echo: Option<Echo>,
    pub camera: PatternCamera,
    /// How the screen the fake screen-cast portal shares is cast.
    pub screen: ScreenCast,
}

/// How the sandbox's screen is cast: as niri does (the default), or in plain memory, as compositors that offer no
/// DMA-BUF do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenCast {
    /// `BGRx` with a mandatory linear modifier, DMA-BUFs only, allocated by the screen (see `screen`'s module docs).
    #[default]
    Niri,
    /// `BGRx` with no modifier, in memory the consumer maps.
    Memory,
}

impl Default for PipeWireOptions {
    fn default() -> Self {
        Self {
            tone: Tone {
                hz: 440.0,
                amplitude: 0.25,
            },
            echo: None,
            camera: PatternCamera::default(),
            screen: ScreenCast::default(),
        }
    }
}

/// The programs and system libraries the private PipeWire is made of.
#[derive(Clone, Debug)]
pub struct PipeWireTools {
    pub pipewire: PathBuf,
    pub wireplumber: PathBuf,
    /// The system's SPA plugin dir (`spa-0.2`), PipeWire module dir (`pipewire-0.3`), WirePlumber module dir
    /// (`wireplumber-0.5`) and WirePlumber's data dir (holding `wireplumber.conf`).
    pub spa_dir: PathBuf,
    pub module_dir: PathBuf,
    pub wireplumber_module_dir: PathBuf,
    pub wireplumber_data_dir: PathBuf,
}

/// Where distributions put the libraries.
const LIB_ROOTS: [&str; 5] = [
    "/usr/lib",
    "/usr/lib64",
    "/usr/lib/x86_64-linux-gnu",
    "/usr/lib/aarch64-linux-gnu",
    "/usr/local/lib",
];
/// Where distributions put WirePlumber's data.
const DATA_ROOTS: [&str; 2] = ["/usr/share", "/usr/local/share"];

impl PipeWireTools {
    /// The tools on this process's `PATH` and the system's library dirs.
    ///
    /// # Errors
    /// See [`PipeWireTools::find_on`].
    pub fn find() -> Result<Self, String> {
        Self::find_on(&std::env::var_os("PATH").unwrap_or_default(), &LIB_ROOTS, &DATA_ROOTS)
    }

    /// The tools on `path`, the libraries under `lib_roots` and WirePlumber's data under `data_roots`.
    ///
    /// # Errors
    /// `pipewire` or `wireplumber` isn't on `path`, or a library dir (or a plugin or module the sandbox needs in it)
    /// isn't there.
    pub fn find_on(path: &std::ffi::OsStr, lib_roots: &[&str], data_roots: &[&str]) -> Result<Self, String> {
        let pipewire = crate::on_path(path, "pipewire").ok_or("no pipewire on PATH")?;
        let wireplumber = crate::on_path(path, "wireplumber").ok_or("no wireplumber on PATH")?;
        let lib = |dir: &str, needs: &str| {
            lib_roots
                .iter()
                .map(|root| Path::new(root).join(dir))
                .find(|candidate| candidate.join(needs).is_file())
                .ok_or_else(|| format!("no {dir}/{needs} under {lib_roots:?}"))
        };
        let spa_dir = lib("spa-0.2", "support/libspa-support.so")?;
        let module_dir = lib("pipewire-0.3", "libpipewire-module-protocol-native.so")?;
        let wireplumber_module_dir = lib("wireplumber-0.5", "libwireplumber-module-settings.so")?;
        let wireplumber_data_dir = data_roots
            .iter()
            .map(|root| Path::new(root).join("wireplumber"))
            .find(|candidate| candidate.join("wireplumber.conf").is_file())
            .ok_or_else(|| format!("no wireplumber/wireplumber.conf under {data_roots:?}"))?;
        let tools = Self {
            pipewire,
            wireplumber,
            spa_dir,
            module_dir,
            wireplumber_module_dir,
            wireplumber_data_dir,
        };
        tools.missing_libraries().map_or(Ok(tools), Err)
    }

    /// Every plugin and module the sandbox links that isn't there, as a message.
    fn missing_libraries(&self) -> Option<String> {
        let mut wanted: Vec<PathBuf> = SPA_PLUGINS
            .iter()
            .chain([&FILTER_GRAPH_BUILTIN])
            .map(|(dir, file)| self.spa_dir.join(dir).join(file))
            .collect();
        wanted.extend(MODULES.iter().map(|name| self.module_dir.join(module_file(name))));
        wanted.extend(
            WIREPLUMBER_MODULES
                .iter()
                .map(|name| self.wireplumber_module_dir.join(wireplumber_module_file(name))),
        );
        let missing: Vec<String> = wanted
            .iter()
            .filter(|path| !path.is_file())
            .map(|path| path.display().to_string())
            .collect();
        (!missing.is_empty()).then(|| format!("missing PipeWire libraries: {}", missing.join(", ")))
    }
}

fn module_file(name: &str) -> String {
    format!("libpipewire-module-{name}.so")
}

fn wireplumber_module_file(name: &str) -> String {
    format!("libwireplumber-module-{name}.so")
}

/// The sandbox's dirs for its PipeWire, all inside its own dir.
#[derive(Clone, Debug)]
struct Layout {
    root: PathBuf,
    /// The runtime dir: the server's socket (`SANDBOX_SOCKET`'s dir).
    run: PathBuf,
    /// `PIPEWIRE_CONFIG_DIR`: the server's and the clients' config (`SANDBOX_CLIENT_CONF`'s dir), and WirePlumber's
    /// drop-in under `wireplumber/`.
    config: PathBuf,
    /// The home and XDG dirs of the server and the session manager.
    home: PathBuf,
    /// Links to the allowed plugins and modules.
    lib: PathBuf,
    /// WirePlumber's system data dir (its main config and scripts), searched after the sandbox's drop-in.
    wireplumber_data: PathBuf,
}

impl Layout {
    fn new(root: &Path, tools: &PipeWireTools) -> SandboxResult<Self> {
        let parent = |relative: &str| -> SandboxResult<PathBuf> {
            let parent = Path::new(relative).parent().ok_or("a relative path without a dir")?;
            Ok(root.join(parent))
        };
        Ok(Self {
            root: root.to_owned(),
            run: parent(SANDBOX_SOCKET)?,
            config: parent(SANDBOX_CLIENT_CONF)?,
            home: root.join("pipewire-home"),
            lib: root.join("pipewire-lib"),
            wireplumber_data: tools.wireplumber_data_dir.clone(),
        })
    }

    fn spa(&self) -> PathBuf {
        self.lib.join("spa-0.2")
    }

    fn modules(&self) -> PathBuf {
        self.lib.join("pipewire-0.3")
    }

    fn wireplumber_modules(&self) -> PathBuf {
        self.lib.join("wireplumber-0.5")
    }

    fn wireplumber_config(&self) -> PathBuf {
        self.config.join("wireplumber")
    }
}

/// The private server, its session manager and the camera and recording thread; all stopped on drop.
pub struct PrivatePipeWire {
    /// Stopped first: it is a client of the server.
    media: Option<media::MediaHelper>,
    /// The session manager, then the server.
    children: Vec<Child>,
    layout: Layout,
}

impl Drop for PrivatePipeWire {
    fn drop(&mut self) {
        self.media.take();
        for child in self.children.iter_mut().rev() {
            stop(child);
        }
    }
}

/// `SIGTERM` to a process this sandbox started, then `SIGKILL` if it is still there after [`STOP_WAIT`].
fn stop(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: `pid` is a child this process spawned and hasn't reaped (it holds the `Child`), so the id can't have
        // been reused; `kill` touches no memory of ours.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    if wait_until(STOP_WAIT, || child.try_wait().ok().flatten()).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl PrivatePipeWire {
    /// Starts the server and the session manager in `root` (the sandbox dir) and the camera and recording thread,
    /// and returns once the session manager has linked the recording.
    ///
    /// # Errors
    /// This process sets a PipeWire config override ([`media::check_environment`]), a dir or config can't be written,
    /// a process can't start, or the server or the link doesn't come in time.
    pub fn start(tools: &PipeWireTools, root: &Path, options: &PipeWireOptions) -> SandboxResult<Self> {
        media::check_environment()?;
        let layout = Layout::new(root, tools)?;
        layout.prepare(tools, options)?;
        let mut pipewire = Self {
            media: None,
            children: Vec::new(),
            layout,
        };
        pipewire.spawn(&tools.pipewire, &[], "pipewire.log")?;
        let socket = pipewire.socket();
        if wait_until(SOCKET_WAIT, || socket.exists().then_some(())).is_none() {
            return Err(format!("the private PipeWire made no socket: {}", pipewire.logs()).into());
        }
        pipewire.spawn(&tools.wireplumber, &["-p", PROFILE], "wireplumber.log")?;
        let helper = media::MediaHelper::start(
            &socket,
            &pipewire.layout.config.join(client_conf_name()?),
            &pipewire.recording(),
            options.camera,
            options.screen,
        );
        match helper {
            Ok(helper) => pipewire.media = Some(helper),
            Err(error) => return Err(format!("{error}: {}", pipewire.logs()).into()),
        }
        Ok(pipewire)
    }

    fn spawn(&mut self, program: &Path, args: &[&str], log: &str) -> SandboxResult {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(self.layout.server_env())
            .current_dir(&self.layout.home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.layout.root.join(log))?);
        spawn::pass_only(&mut command, &[]);
        self.children.push(command.spawn()?);
        Ok(())
    }

    /// The server's socket.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.layout.root.join(SANDBOX_SOCKET)
    }

    /// The speaker's recording: 16-bit mono WAV at 48 kHz, its header brought up to date every second.
    #[must_use]
    pub fn recording(&self) -> PathBuf {
        self.layout.root.join(SPEAKER_RECORDING)
    }

    /// Adds a microphone playing `tone` (see [`nodes`]), as a device plugged in; it goes when the node is dropped.
    ///
    /// # Errors
    /// The helper's thread has stopped, or the node isn't there in time.
    pub fn add_microphone(&self, spec: NodeSpec, tone: Tone) -> SandboxResult<AddedNode> {
        AddedNode::add(
            nodes::commands_of(self.media.as_ref())?,
            Added::Microphone(spec, Sound::Tone(tone)),
        )
    }

    /// Adds a microphone playing `samples` over and over (see [`nodes`]), as a device plugged in, each buffer it plays
    /// marked on its [`AddedNode::timeline`]; it goes when the node is dropped.
    ///
    /// # Errors
    /// The helper's thread has stopped, or the node isn't there in time.
    pub fn add_looped_microphone(&self, spec: NodeSpec, samples: Arc<[f32]>) -> SandboxResult<AddedNode> {
        AddedNode::add(
            nodes::commands_of(self.media.as_ref())?,
            Added::Microphone(spec, Sound::Looped(samples, Timeline::new())),
        )
    }

    /// When the speaker's recording's samples were heard ([`timeline`]): sample `n` of the recording is sample `n` of
    /// its timeline. Empty without the recording thread.
    #[must_use]
    pub fn recording_timeline(&self) -> Timeline {
        self.media
            .as_ref()
            .map(media::MediaHelper::recording_timeline)
            .unwrap_or_default()
    }

    /// Adds a speaker recorded to `recordings/<its name>.wav` in the sandbox dir (see [`nodes`]); it goes when the
    /// node is dropped.
    ///
    /// # Errors
    /// The helper's thread has stopped, or the node isn't there in time.
    pub fn add_speaker(&self, spec: NodeSpec) -> SandboxResult<AddedNode> {
        let recording = self
            .recording()
            .with_file_name(format!("{}.wav", spec.name.replace('/', "_")));
        AddedNode::add(
            nodes::commands_of(self.media.as_ref())?,
            Added::Speaker(spec, recording),
        )
    }

    /// Frames the camera has shown so far.
    #[must_use]
    pub fn camera_frames(&self) -> u64 {
        self.media.as_ref().map_or(0, media::MediaHelper::frames_shown)
    }

    /// The sandbox's screen ([`SCREEN_NODE`]): its node once made, and its frames, as the fake screen-cast portal
    /// reads them.
    #[must_use]
    pub fn screen(&self) -> media::ScreenShown {
        self.media.as_ref().map(media::MediaHelper::screen).unwrap_or_default()
    }

    /// The process ids of the server and the session manager.
    #[must_use]
    pub fn pids(&self) -> Vec<u32> {
        self.children.iter().map(Child::id).collect()
    }

    /// What a client of this PipeWire needs on top of its own environment: the sandbox dir (where `teams-media`
    /// finds the server and its client config), the runtime dir, the config dir, the cut-down plugin and module dirs,
    /// and a Pulse runtime dir with no server in it.
    #[must_use]
    pub fn client_env(&self) -> Vec<(&'static str, OsString)> {
        let layout = &self.layout;
        vec![
            (SANDBOX_ENV, layout.root.clone().into_os_string()),
            ("PIPEWIRE_RUNTIME_DIR", layout.run.clone().into_os_string()),
            ("PIPEWIRE_CONFIG_DIR", layout.config.clone().into_os_string()),
            ("SPA_PLUGIN_DIR", layout.spa().into_os_string()),
            ("PIPEWIRE_MODULE_DIR", layout.modules().into_os_string()),
            ("PULSE_RUNTIME_PATH", layout.run.join("pulse").into_os_string()),
            ("PIPEWIRE_LOG_SYSTEMD", "false".into()),
        ]
    }

    /// The server's and the session manager's logs, for a failure message.
    #[must_use]
    pub fn logs(&self) -> String {
        let mut logs = String::new();
        for name in ["pipewire.log", "wireplumber.log"] {
            let text = fs::read_to_string(self.layout.root.join(name)).unwrap_or_default();
            let _ = write!(logs, "\n--- {name}\n{text}");
        }
        logs
    }
}

/// The client config's file name (`SANDBOX_CLIENT_CONF`'s last part).
fn client_conf_name() -> SandboxResult<&'static std::ffi::OsStr> {
    Ok(Path::new(SANDBOX_CLIENT_CONF)
        .file_name()
        .ok_or("a client config path without a name")?)
}

impl Layout {
    /// Makes the dirs, links the allowed libraries and writes the configs.
    fn prepare(&self, tools: &PipeWireTools, options: &PipeWireOptions) -> SandboxResult {
        for dir in [&self.run, &self.config, &self.home, &self.lib] {
            private_dir(dir.clone())?;
        }
        for dir in [".config", ".local/state", ".local/share", ".cache"] {
            private_dir(self.home.join(dir))?;
        }
        private_dir(
            self.root.join(
                Path::new(SPEAKER_RECORDING)
                    .parent()
                    .ok_or("a recording without a dir")?,
            ),
        )?;
        for (dir, file) in SPA_PLUGINS.iter().chain([&FILTER_GRAPH_BUILTIN]) {
            let target = private_dir(self.spa().join(dir))?;
            link(&tools.spa_dir.join(dir).join(file), &target.join(file))?;
        }
        let modules = private_dir(self.modules())?;
        for name in MODULES {
            link(
                &tools.module_dir.join(module_file(name)),
                &modules.join(module_file(name)),
            )?;
        }
        let wireplumber_modules = private_dir(self.wireplumber_modules())?;
        for name in WIREPLUMBER_MODULES {
            let file = wireplumber_module_file(name);
            link(
                &tools.wireplumber_module_dir.join(&file),
                &wireplumber_modules.join(&file),
            )?;
        }
        fs::write(self.config.join("pipewire.conf"), server_config(options))?;
        fs::write(self.config.join(client_conf_name()?), CLIENT_CONFIG)?;
        let drop_in = private_dir(self.wireplumber_config().join("wireplumber.conf.d"))?;
        fs::write(drop_in.join("90-ngteams-sandbox.conf"), WIREPLUMBER_PROFILE)?;
        Ok(())
    }

    /// The server's and the session manager's whole environment.
    fn server_env(&self) -> Vec<(&'static str, OsString)> {
        let mut wireplumber_config = self.wireplumber_config().into_os_string();
        wireplumber_config.push(":");
        wireplumber_config.push(&self.wireplumber_data);
        let mut env = vec![
            ("HOME", self.home.clone().into_os_string()),
            ("XDG_CONFIG_HOME", self.home.join(".config").into_os_string()),
            ("XDG_STATE_HOME", self.home.join(".local/state").into_os_string()),
            ("XDG_DATA_HOME", self.home.join(".local/share").into_os_string()),
            ("XDG_CACHE_HOME", self.home.join(".cache").into_os_string()),
            ("XDG_RUNTIME_DIR", self.run.clone().into_os_string()),
            ("PIPEWIRE_RUNTIME_DIR", self.run.clone().into_os_string()),
            ("PULSE_RUNTIME_PATH", self.run.join("pulse").into_os_string()),
            ("PIPEWIRE_CONFIG_DIR", self.config.clone().into_os_string()),
            ("SPA_PLUGIN_DIR", self.spa().into_os_string()),
            ("PIPEWIRE_MODULE_DIR", self.modules().into_os_string()),
            ("WIREPLUMBER_CONFIG_DIR", wireplumber_config),
            ("WIREPLUMBER_MODULE_DIR", self.wireplumber_modules().into_os_string()),
            ("PIPEWIRE_LOG_SYSTEMD", "false".into()),
        ];
        for name in ["PATH", "LANG"] {
            if let Some(value) = std::env::var_os(name) {
                env.push((name, value));
            }
        }
        env
    }
}

fn link(from: &Path, to: &Path) -> SandboxResult {
    symlink(from, to).map_err(|error| format!("linking {} to {}: {error}", to.display(), from.display()).into())
}

/// The clients' config: the native protocol and what a stream needs, nothing else.
const CLIENT_CONFIG: &str = "\
context.properties = {
    support.dbus = false
    mem.allow-mlock = false
}
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    video.convert.* = videoconvert/libspa-videoconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-client-device }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-metadata }
]
";

/// WirePlumber's profile: the policy alone (linking, default nodes), no hardware monitor, no D-Bus, no saved state.
const WIREPLUMBER_PROFILE: &str = "\
wireplumber.profiles = {
  ngteams-sandbox = {
    inherits = [ policy, mixin.systemwide-session, mixin.stateless ]
    support.dbus = disabled
    support.system-dbus = disabled
    support.portal-permissionstore = disabled
    support.reserve-device = disabled
    support.logind = disabled
    support.modem-manager = disabled
    support.mpris = disabled
    api.notifications = disabled
    api.file-monitor = disabled
    monitor.alsa = disabled
    monitor.alsa-midi = disabled
    monitor.bluez = disabled
    monitor.bluez-midi = disabled
    monitor.v4l2 = disabled
    monitor.libcamera = disabled
  }
}
";

/// The server's config: the speaker, and the microphone's filter graph (the tone plus the speaker's delayed echo).
fn server_config(options: &PipeWireOptions) -> String {
    format!("{SERVER_HEAD}{}{}", microphone_module(options), server_objects())
}

/// The server's properties, plugins and modules, up to the microphone's module.
const SERVER_HEAD: &str = "\
context.properties = {
    core.daemon = true
    core.name = pipewire-0
    support.dbus = false
    mem.allow-mlock = false
    default.clock.rate = 48000
    default.clock.allowed-rates = [ 48000 ]
}
context.spa-libs = {
    audio.convert.* = audioconvert/libspa-audioconvert
    video.convert.* = videoconvert/libspa-videoconvert
    support.* = support/libspa-support
}
context.modules = [
    { name = libpipewire-module-protocol-native }
    { name = libpipewire-module-access }
    { name = libpipewire-module-metadata }
    { name = libpipewire-module-spa-node-factory }
    { name = libpipewire-module-client-node }
    { name = libpipewire-module-client-device }
    { name = libpipewire-module-adapter }
    { name = libpipewire-module-link-factory }
";

/// The microphone: a filter graph mixing the tone with the speaker's output, delayed and attenuated (gain 0 without
/// an echo), ending the module list.
fn microphone_module(options: &PipeWireOptions) -> String {
    let (delay, gain) = options.echo.map_or((0.0, 0.0), |echo| {
        (echo.delay.as_secs_f32(), 10_f32.powf(-echo.attenuation_db / 20.0))
    });
    let max_delay = delay.ceil() + 1.0;
    let Tone { hz, amplitude } = options.tone;
    format!(
        "\
    {{ name = libpipewire-module-filter-chain
      args = {{
        node.description = \"Sandbox microphone\"
        audio.channels = 1
        audio.position = [ MONO ]
        filter.graph = {{
          nodes = [
            {{ type = builtin name = tone label = sine control = {{ \"Freq\" = {hz} \"Ampl\" = {amplitude} }} }}
            {{ type = builtin name = echo label = delay config = {{ \"max-delay\" = {max_delay} }} control = {{ \"Delay (s)\" = {delay} }} }}
            {{ type = builtin name = mic label = mixer control = {{ \"Gain 1\" = {gain} \"Gain 2\" = 1.0 }} }}
          ]
          links = [
            {{ output = \"echo:Out\" input = \"mic:In 1\" }}
            {{ output = \"tone:Out\" input = \"mic:In 2\" }}
          ]
          inputs = [ \"echo:In\" ]
          outputs = [ \"mic:Out\" ]
        }}
        capture.props = {{
          node.name = sandbox-echo
          stream.capture.sink = true
          target.object = {SPEAKER_NODE}
          node.passive = true
        }}
        playback.props = {{
          node.name = {MICROPHONE_NODE}
          node.description = \"Sandbox microphone\"
          media.class = Audio/Source
          priority.session = {DEVICE_PRIORITY}
        }}
      }}
    }}
]
"
    )
}

/// The driver and the speaker.
fn server_objects() -> String {
    format!(
        "\
context.objects = [
    {{ factory = spa-node-factory
      args = {{
        factory.name = support.node.driver
        node.name = Dummy-Driver
        node.group = pipewire.dummy
        priority.driver = 200000
      }}
    }}
    {{ factory = adapter
      args = {{
        factory.name = support.null-audio-sink
        node.name = {SPEAKER_NODE}
        node.description = \"Sandbox speaker\"
        media.class = Audio/Sink
        audio.position = [ MONO ]
        audio.rate = 48000
        priority.session = {DEVICE_PRIORITY}
      }}
    }}
]
"
    )
}
