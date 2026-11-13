//! The PipeWire backend: the microphone, the speaker and the camera as streams on one PipeWire server.
//!
//! The caller selects [`Reach`] when opening the devices:
//!
//! - [`Reach::Sandbox`]: only the UI sandbox's private server, at [`SANDBOX_SOCKET`] inside the dir [`SANDBOX_ENV`]
//!   names, with the client config the sandbox wrote there ([`SANDBOX_CLIENT_CONF`]). Anything else is refused: no
//!   sandbox, a sandbox dir that is (or holds) the desktop's runtime dir, a socket that leads out of the sandbox dir,
//!   the desktop's PipeWire or Pulse sockets, and a process whose environment sets one of [`CONFIG_OVERRIDES`]
//!   (libpipewire would load that config instead of the sandbox's).
//! - [`Reach::Desktop`]: the desktop session's server, `pipewire-0` in `PIPEWIRE_RUNTIME_DIR` or else `XDG_RUNTIME_DIR`.
//!
//! The socket is connected here and handed to libpipewire as a descriptor (`pw_context_connect_fd`), so libpipewire
//! never looks for a server by itself (it would fall back from one runtime dir to another, and to `/run/pipewire`).
//!
//! **The camera** ([`CameraRoute`], selected by the reach too): the sandbox's is a node of its server, like
//! its microphone and speaker; the desktop's comes through the camera portal ([`super::portal`]),
//! which asks the user and hands over a PipeWire remote showing only the cameras, one remote for each camera opened.
//! [`PipeWireDevices::connect_with_camera_portal`] names a portal's bus outright; sandbox reach takes only one inside
//! the UI sandbox's dir (the sandbox's own bus, where its fake portal answers), never the desktop's.
//!
//! **A shared screen** ([`Devices::screen`], [`screen_route`]): always through the screen-cast portal
//! ([`super::portal::open_screen_cast`]) on the session bus, which asks the user what to share and hands over the
//! node and a remote to reach it on; the screen's stream runs on that remote, for as long as the share. Sandbox reach
//! asks only the UI sandbox's bus (its fake portal answers with the sandbox's synthetic screen), never the desktop's.
//!
//! **The devices there are and the user's choice** ([`DeviceDirectory`]): the loop thread watches the server's
//! registry and the session manager's defaults ([`listing`]); a choice moves the open streams of its kind to the
//! device chosen, or back to the system's default, without the devices noticing (`serve`). With no device chosen, a
//! stream follows the system's default as the session manager moves it (a device plugged in or out).
//!
//! **Threads**: libpipewire's objects live on one thread of this backend's own, running PipeWire's main loop
//! ([`PipeWireDevices`] starts it and stops it when the last device and handle are gone); libpipewire adds its own
//! data thread. Frames cross to and from the caller over bounded queues that drop their oldest entries when full: the
//! microphone keeps the newest [`QUEUED_FRAMES`] and the camera the newest [`QUEUED_VIDEO`] when nobody reads, the
//! speaker keeps [`SPEAKER_QUEUE`] of audio. A device whose stream ends (it errs or is unlinked, or the server goes
//! away) reports [`DeviceError::Closed`]: a source once its queue is drained, the speaker at once. Once the server
//! went away and the loop thread ended, opening a device reports [`DeviceError::Closed`] too. The crate has no
//! logger: libpipewire's own warnings go to stderr.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::catalog::{Choices, DeviceDirectory, DeviceKind, DeviceList, DeviceWatcher};
use super::{AudioFrame, AudioSink, AudioSource, DeviceError, Devices, VideoFrame, VideoSource, portal};

mod dmabuf;
mod listing;
mod serve;
mod streams;

use crate::queue::Queue;
use listing::Listing;
use serve::Command;

/// Which PipeWire server the caller selects when opening devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Only the UI sandbox's private server.
    Sandbox,
    /// Only the desktop session's server.
    Desktop,
}

/// The variable naming the UI sandbox's dir (as `ui_sandbox::SANDBOX_ENV`).
pub const SANDBOX_ENV: &str = "UI_SANDBOX_DIR";
/// The sandbox's PipeWire socket, relative to its dir.
pub const SANDBOX_SOCKET: &str = "pipewire/pipewire-0";
/// The client config the sandbox writes for its clients, relative to its dir: libpipewire reads it and nothing of
/// the session's (no `client.conf.d` from the user's config dirs).
pub const SANDBOX_CLIENT_CONF: &str = "pipewire-config/client.conf";

/// How long the server gets to answer the first round trip.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the loop thread gets to create a stream.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
/// How often an open that waits for its stream checks that the loop thread still runs.
const OPEN_CHECK: Duration = Duration::from_millis(50);
/// Audio frames the microphone keeps when nobody reads them (0.5 s), the newest; older ones are dropped.
pub const QUEUED_FRAMES: usize = 50;
/// Video frames the camera keeps when nobody reads them, the newest; older ones are dropped.
pub const QUEUED_VIDEO: usize = 3;
/// Audio the speaker keeps queued, in samples (200 ms); more drops the oldest.
pub const SPEAKER_QUEUE: usize = 9_600;

/// The session's variables this backend reads, and the user's id (for the fixed runtime dir, `/run/user/<uid>`).
#[derive(Clone, Debug, Default)]
pub struct SessionEnv {
    vars: HashMap<String, OsString>,
    uid: u32,
}

/// The variables libpipewire reads ahead of the client config a context names (`config.name`, `config.prefix`): set,
/// they load another config in its place.
pub const CONFIG_OVERRIDES: [&str; 2] = ["PIPEWIRE_CONFIG_NAME", "PIPEWIRE_CONFIG_PREFIX"];

/// The session bus's address, where the desktop's camera portal answers.
const SESSION_BUS_ENV: &str = "DBUS_SESSION_BUS_ADDRESS";

/// The variables [`SessionEnv::current`] takes from the process.
const READ_VARS: [&str; 6] = [
    SANDBOX_ENV,
    "PIPEWIRE_RUNTIME_DIR",
    "XDG_RUNTIME_DIR",
    SESSION_BUS_ENV,
    CONFIG_OVERRIDES[0],
    CONFIG_OVERRIDES[1],
];

impl SessionEnv {
    /// This process's.
    #[must_use]
    pub fn current() -> Self {
        let vars = READ_VARS
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| ((*name).to_owned(), value)))
            .collect();
        Self { vars, uid: own_uid() }
    }

    /// The given variables and user id (tests).
    #[must_use]
    pub fn from_vars<K: Into<String>, V: Into<OsString>>(vars: impl IntoIterator<Item = (K, V)>, uid: u32) -> Self {
        Self {
            vars: vars
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            uid,
        }
    }

    fn var(&self, name: &str) -> Option<&Path> {
        self.vars.get(name).filter(|value| !value.is_empty()).map(Path::new)
    }

    /// The first of [`CONFIG_OVERRIDES`] set at all (even empty: libpipewire reads it as it is).
    fn config_override(&self) -> Option<&str> {
        CONFIG_OVERRIDES.into_iter().find(|name| self.vars.contains_key(*name))
    }

    /// The desktop's fixed runtime dirs: the user's session dir and the system-wide PipeWire dir.
    fn desktop_dirs(&self) -> [PathBuf; 2] {
        [
            PathBuf::from(format!("/run/user/{}", self.uid)),
            PathBuf::from("/run/pipewire"),
        ]
    }
}

/// The user id of this process.
fn own_uid() -> u32 {
    // SAFETY: getuid has no preconditions, can't fail and touches no memory of ours.
    unsafe { libc::getuid() }
}

/// Where the selected devices connect, and with which client config (`None`: libpipewire's default lookup).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub socket: PathBuf,
    pub client_conf: Option<PathBuf>,
}

/// The server `reach` allows in the session `env` describes.
///
/// # Errors
/// [`DeviceError::Refused`], with the reason: see the module's notes.
pub fn endpoint(reach: Reach, env: &SessionEnv) -> Result<Endpoint, DeviceError> {
    match reach {
        Reach::Sandbox => sandbox_endpoint(env),
        Reach::Desktop => desktop_endpoint(env),
    }
}

fn refused(reason: impl Into<String>) -> DeviceError {
    DeviceError::Refused(reason.into())
}

fn sandbox_endpoint(env: &SessionEnv) -> Result<Endpoint, DeviceError> {
    let dir = env.var(SANDBOX_ENV).ok_or_else(|| {
        refused(format!(
            "sandbox reach opens only the UI sandbox's PipeWire, and {SANDBOX_ENV} is unset (the sandbox sets it)"
        ))
    })?;
    if let Some(name) = env.config_override() {
        return Err(refused(format!(
            "{name} is set: libpipewire would load that config instead of the sandbox's client config"
        )));
    }
    if !dir.is_absolute() || dir.components().any(|part| part == Component::ParentDir) {
        return Err(refused(format!("{SANDBOX_ENV} must be an absolute path without `..`")));
    }
    let dir = dir.canonicalize()?;
    for desktop in env.desktop_dirs() {
        let desktop = desktop.canonicalize().unwrap_or(desktop);
        if desktop.starts_with(&dir) {
            return Err(refused(format!(
                "the sandbox dir {} is or holds the desktop's runtime dir {}",
                dir.display(),
                desktop.display()
            )));
        }
    }
    let socket = dir.join(SANDBOX_SOCKET).canonicalize().map_err(|error| {
        refused(format!(
            "the sandbox has no PipeWire at {}: {error}",
            dir.join(SANDBOX_SOCKET).display()
        ))
    })?;
    if !socket.starts_with(&dir) {
        return Err(refused(format!(
            "the sandbox's PipeWire socket leads out of the sandbox dir, to {}",
            socket.display()
        )));
    }
    if desktop_sockets(env).contains(&socket) {
        return Err(refused(format!("{} is the desktop's", socket.display())));
    }
    check_socket(&socket)?;
    let client_conf = dir.join(SANDBOX_CLIENT_CONF).canonicalize().map_err(|error| {
        refused(format!(
            "the sandbox wrote no PipeWire client config ({SANDBOX_CLIENT_CONF}): {error}"
        ))
    })?;
    if !client_conf.starts_with(&dir) {
        return Err(refused(
            "the sandbox's PipeWire client config leads out of the sandbox dir",
        ));
    }
    Ok(Endpoint {
        socket,
        client_conf: Some(client_conf),
    })
}

/// The desktop's PipeWire and Pulse sockets at their fixed places, as they resolve.
fn desktop_sockets(env: &SessionEnv) -> Vec<PathBuf> {
    let mut sockets = Vec::new();
    for dir in env.desktop_dirs() {
        for name in ["pipewire-0", "pipewire-0-manager", "pulse/native", "native"] {
            let path = dir.join(name);
            sockets.push(path.canonicalize().unwrap_or(path));
        }
    }
    sockets
}

fn check_socket(socket: &Path) -> Result<(), DeviceError> {
    let metadata = socket.metadata()?;
    if !metadata.file_type().is_socket() {
        return Err(refused(format!("{} isn't a socket", socket.display())));
    }
    if metadata.uid() != own_uid() {
        return Err(refused(format!("{} belongs to another user", socket.display())));
    }
    Ok(())
}

fn desktop_endpoint(env: &SessionEnv) -> Result<Endpoint, DeviceError> {
    let dir = env
        .var("PIPEWIRE_RUNTIME_DIR")
        .or_else(|| env.var("XDG_RUNTIME_DIR"))
        .ok_or_else(|| refused("neither PIPEWIRE_RUNTIME_DIR nor XDG_RUNTIME_DIR is set: no desktop session"))?;
    if !dir.is_absolute() {
        return Err(refused("the desktop's runtime dir must be an absolute path"));
    }
    let socket = dir.join("pipewire-0");
    check_socket(&socket)?;
    Ok(Endpoint {
        socket,
        client_conf: None,
    })
}

/// Where the selected reach's camera comes from (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CameraRoute {
    /// A node of the same server as the microphone and the speaker: the UI sandbox's.
    Server,
    /// The camera portal on the bus at this address.
    Portal(String),
    /// No way to a camera: why.
    Unavailable(String),
}

/// The camera route `reach` takes in the session `env` describes: the sandbox's server, or the desktop's camera portal
/// on the session bus.
#[must_use]
pub fn camera_route(reach: Reach, env: &SessionEnv) -> CameraRoute {
    match reach {
        Reach::Sandbox => CameraRoute::Server,
        Reach::Desktop => match session_bus(env) {
            Some(address) => CameraRoute::Portal(address),
            None => CameraRoute::Unavailable(format!(
                "no session bus for the camera portal: neither {SESSION_BUS_ENV} nor XDG_RUNTIME_DIR is set"
            )),
        },
    }
}

/// The screen-cast portal's bus `reach` asks in the session `env` describes (see the module docs), or why there is
/// none: the session bus, which sandbox reach takes only when it is the UI sandbox's ([`portal_bus`]).
///
/// # Errors
/// [`DeviceError::Refused`] with the reason.
pub fn screen_route(reach: Reach, env: &SessionEnv) -> Result<String, DeviceError> {
    let address = session_bus(env).ok_or_else(|| {
        refused(format!(
            "no session bus for the screen-cast portal: neither {SESSION_BUS_ENV} nor XDG_RUNTIME_DIR is set"
        ))
    })?;
    portal_bus(reach, env, &address)?;
    Ok(address)
}

/// The session bus's address: `DBUS_SESSION_BUS_ADDRESS`, else the bus in the runtime dir.
fn session_bus(env: &SessionEnv) -> Option<String> {
    if let Some(address) = env.vars.get(SESSION_BUS_ENV).filter(|value| !value.is_empty()) {
        return Some(address.to_string_lossy().into_owned());
    }
    let dir = env.var("XDG_RUNTIME_DIR")?;
    Some(format!("unix:path={}", dir.join("bus").display()))
}

/// Whether `reach` may ask the camera portal on the bus at `address`: sandbox reach only on a bus whose socket lies
/// in the UI sandbox's dir (`unix:path=<a path in it>`, nothing else), so no lab or test process ever asks the
/// desktop's portal; desktop reach on any.
///
/// # Errors
/// [`DeviceError::Refused`], with the reason.
pub fn portal_bus(reach: Reach, env: &SessionEnv, address: &str) -> Result<(), DeviceError> {
    if reach == Reach::Desktop {
        return Ok(());
    }
    let dir = env
        .var(SANDBOX_ENV)
        .ok_or_else(|| {
            refused(format!(
                "sandbox reach asks only the UI sandbox's camera portal, and {SANDBOX_ENV} is unset"
            ))
        })?
        .canonicalize()?;
    let path = address
        .strip_prefix("unix:path=")
        .filter(|rest| !rest.contains([',', ';']))
        .and_then(unescape_address)
        .ok_or_else(|| {
            refused(format!(
                "the camera portal's bus must be a `unix:path=` socket, not {address}"
            ))
        })?;
    let path = Path::new(&path);
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(refused("the camera portal's bus must be an absolute path without `..`"));
    }
    let socket = path.canonicalize()?;
    if !socket.starts_with(&dir) {
        return Err(refused(format!(
            "the camera portal's bus {} isn't the UI sandbox's (in {})",
            socket.display(),
            dir.display()
        )));
    }
    check_socket(&socket)
}

/// A D-Bus address value with its `%xx` escapes undone; `None` for a broken escape or bytes that aren't UTF-8.
fn unescape_address(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut rest = value.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(after.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &after[2..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8(bytes).ok()
}

/// The PipeWire backend's devices: one connection, on a thread of its own, to the selected server; the
/// camera on it or through the camera portal ([`CameraRoute`]); the devices there are and the user's choice of each
/// kind ([`DeviceDirectory`]).
#[derive(Clone)]
pub struct PipeWireDevices {
    connection: Arc<Connection>,
    camera: CameraRoute,
    /// The screen-cast portal's bus, or why there is none ([`screen_route`]).
    screen: Result<String, String>,
    /// The client config a connection takes (the sandbox's), for the portal's remotes too.
    client_conf: Option<PathBuf>,
    choices: Arc<Mutex<Choices>>,
    /// The portal's remotes, one per camera open now: a camera choice moves their streams too.
    remotes: Arc<Mutex<Vec<Weak<Connection>>>>,
}

impl std::fmt::Debug for PipeWireDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeWireDevices")
            .field("camera", &self.camera)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl PipeWireDevices {
    /// Connects to the server `reach` selects in this process's session, its camera on the corresponding
    /// [`camera_route`], and returns once the devices there are listed.
    ///
    /// # Errors
    /// The server is refused ([`DeviceError::Refused`]), can't be reached, or doesn't answer in time.
    pub fn connect(reach: Reach) -> Result<Self, DeviceError> {
        let env = SessionEnv::current();
        Self::connect_routed(reach, &env, camera_route(reach, &env))
    }

    /// [`PipeWireDevices::connect`], the camera through the camera portal on the bus at `bus` (with sandbox reach,
    /// only the UI sandbox's: [`portal_bus`]).
    ///
    /// # Errors
    /// As [`PipeWireDevices::connect`]'s, and [`DeviceError::Refused`] for a bus outside the selected reach.
    pub fn connect_with_camera_portal(reach: Reach, bus: &str) -> Result<Self, DeviceError> {
        let env = SessionEnv::current();
        portal_bus(reach, &env, bus)?;
        Self::connect_routed(reach, &env, CameraRoute::Portal(bus.to_owned()))
    }

    fn connect_routed(reach: Reach, env: &SessionEnv, camera: CameraRoute) -> Result<Self, DeviceError> {
        let endpoint = endpoint(reach, env)?;
        let stream = UnixStream::connect(&endpoint.socket)?;
        let connection = Connection::start(stream.into(), endpoint.client_conf.clone(), Choices::default())?;
        Ok(Self {
            connection: Arc::new(connection),
            camera,
            screen: screen_route(reach, env).map_err(|error| error.to_string()),
            client_conf: endpoint.client_conf,
            choices: Arc::default(),
            remotes: Arc::default(),
        })
    }

    /// The camera's route.
    #[must_use]
    pub fn camera_route(&self) -> &CameraRoute {
        &self.camera
    }

    /// Asks the loop thread of `connection` for a stream and waits for it. The guard exists from the moment the
    /// command is sent, so a stream the loop makes after this gave up (a timeout) is closed again right after. A loop
    /// thread that has ended (the server went away), or ends while this waits, makes it [`DeviceError::Closed`]:
    /// nobody would ever take the command.
    fn open(
        connection: &Arc<Connection>,
        make: impl FnOnce(u64, mpsc::Sender<Result<(), String>>) -> Command,
    ) -> Result<StreamGuard, DeviceError> {
        let id = connection.next_id.fetch_add(1, Ordering::Relaxed);
        let (ready_tx, ready_rx) = mpsc::channel();
        connection.send(make(id, ready_tx))?;
        let guard = StreamGuard::new(id, connection);
        let deadline = Instant::now() + OPEN_TIMEOUT;
        loop {
            // read before the wait: the loop thread answers before it ends, so once it has ended its answer, if any,
            // is already in the channel
            let ended = connection.ended();
            let slice = OPEN_CHECK.min(deadline.saturating_duration_since(Instant::now()));
            match ready_rx.recv_timeout(slice) {
                Ok(Ok(())) => return Ok(guard),
                Ok(Err(error)) => return Err(DeviceError::PipeWire(error)),
                Err(RecvTimeoutError::Disconnected) => return Err(DeviceError::Closed),
                Err(RecvTimeoutError::Timeout) if ended => return Err(DeviceError::Closed),
                Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    return Err(DeviceError::PipeWire(format!(
                        "the stream wasn't created within {OPEN_TIMEOUT:?}"
                    )));
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// The connection a camera opens on: this one, or a new remote from the camera portal.
    fn camera_connection(&self) -> Result<Arc<Connection>, DeviceError> {
        match &self.camera {
            CameraRoute::Server => Ok(Arc::clone(&self.connection)),
            CameraRoute::Portal(bus) => {
                let remote = portal::open_camera_remote(bus)?;
                let choices = lock(&self.choices).clone();
                let started_with = choices.get(DeviceKind::Camera).map(str::to_owned);
                let connection = Arc::new(Connection::start(remote, self.client_conf.clone(), choices)?);
                let mut remotes = lock(&self.remotes);
                remotes.retain(|remote| remote.strong_count() > 0);
                remotes.push(Arc::downgrade(&connection));
                // a choice made while the remote started: `choose` set it before walking `remotes`, which held no
                // remote yet; one made from here on reaches it through `choose` (sent twice, the loop takes it once)
                let now = lock(&self.choices).get(DeviceKind::Camera).map(str::to_owned);
                if now != started_with {
                    // a remote whose loop has ended has no stream to move
                    let _ = connection.send(Command::Choose {
                        kind: DeviceKind::Camera,
                        name: now,
                    });
                }
                Ok(connection)
            }
            CameraRoute::Unavailable(why) => Err(refused(why.clone())),
        }
    }
}

impl Devices for PipeWireDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        let frames = Queue::new(QUEUED_FRAMES);
        let shared = frames.clone();
        let stream = Self::open(&self.connection, |id, ready| Command::Microphone {
            id,
            frames: shared,
            ready,
        })?;
        Ok(Box::new(Microphone {
            frames,
            _stream: stream,
        }))
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        let queue = Queue::new(SPEAKER_QUEUE);
        let shared = queue.clone();
        let stream = Self::open(&self.connection, |id, ready| Command::Speaker {
            id,
            queue: shared,
            ready,
        })?;
        Ok(Box::new(Speaker { queue, _stream: stream }))
    }

    /// The screen (or window) the user picks at the screen-cast portal, its stream on the remote the portal hands over
    /// (its session closed once the source goes).
    fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        let bus = self.screen.as_ref().map_err(|why| refused(why.clone()))?;
        let mut cast = portal::open_screen_cast(bus)?;
        let remote = cast
            .remote
            .take()
            .ok_or_else(|| DeviceError::Portal("the screen-cast portal handed over no remote".to_owned()))?;
        let connection = Arc::new(Connection::start(remote, self.client_conf.clone(), Choices::default())?);
        let frames = Queue::new(QUEUED_VIDEO);
        let shared = frames.clone();
        let node = cast.node;
        let stream = Self::open(&connection, |id, ready| Command::Screen {
            id,
            node,
            frames: shared,
            ready,
        })?;
        Ok(Box::new(Screen {
            frames,
            _stream: stream,
            _cast: cast,
        }))
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        let connection = self.camera_connection()?;
        let frames = Queue::new(QUEUED_VIDEO);
        let shared = frames.clone();
        let stream = Self::open(&connection, |id, ready| Command::Camera {
            id,
            frames: shared,
            ready,
        })?;
        Ok(Box::new(Camera {
            frames,
            _stream: stream,
        }))
    }
}

impl DeviceDirectory for PipeWireDevices {
    fn devices(&self) -> DeviceList {
        self.connection.listing.list()
    }

    fn watch(&self, watcher: DeviceWatcher) {
        self.connection.listing.watch(watcher);
    }

    fn choose(&self, kind: DeviceKind, name: Option<&str>) {
        if !lock(&self.choices).set(kind, name) {
            return;
        }
        let choose = || Command::Choose {
            kind,
            name: name.map(str::to_owned),
        };
        // a connection whose loop has ended has no stream left to move
        let _ = self.connection.send(choose());
        if kind == DeviceKind::Camera {
            for remote in lock(&self.remotes).iter().filter_map(Weak::upgrade) {
                let _ = remote.send(choose());
            }
        }
    }

    fn chosen(&self, kind: DeviceKind) -> Option<String> {
        lock(&self.choices).get(kind).map(str::to_owned)
    }
}

/// The loop thread and the way to it.
struct Connection {
    commands: Mutex<pipewire::channel::Sender<Command>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    next_id: AtomicU64,
    /// Set once the loop thread has stopped taking commands (it quit, or the server went away): PipeWire's channel
    /// keeps taking them, so a send can't tell by itself.
    ended: Arc<AtomicBool>,
    /// The devices the server holds, as the loop thread last listed them.
    listing: Arc<Listing>,
}

/// Marks the loop thread's end when dropped, on its way out (a panic too).
struct EndMark(Arc<AtomicBool>);

impl Drop for EndMark {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Connection {
    /// Starts the loop thread on `fd` with the user's `choices`, and waits until the devices are listed.
    fn start(fd: OwnedFd, client_conf: Option<PathBuf>, choices: Choices) -> Result<Self, DeviceError> {
        let (commands, receiver) = pipewire::channel::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let ended = Arc::new(AtomicBool::new(false));
        let mark = EndMark(Arc::clone(&ended));
        let listing = Arc::new(Listing::default());
        let start_listing = Arc::clone(&listing);
        let thread = thread::Builder::new()
            .name("teams-media-pipewire".to_owned())
            .spawn(move || {
                let _mark = mark;
                let start = serve::Start {
                    fd,
                    client_conf: client_conf.as_deref(),
                    commands: receiver,
                    listing: start_listing,
                    choices,
                };
                serve::run(start, &started_tx);
            })?;
        let connection = Self {
            commands: Mutex::new(commands),
            thread: Mutex::new(Some(thread)),
            next_id: AtomicU64::new(1),
            ended,
            listing,
        };
        match started_rx.recv_timeout(CONNECT_TIMEOUT) {
            Ok(Ok(())) => Ok(connection),
            Ok(Err(error)) => Err(DeviceError::PipeWire(error)),
            Err(RecvTimeoutError::Timeout) => Err(DeviceError::PipeWire(format!(
                "the server didn't answer within {CONNECT_TIMEOUT:?}"
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(DeviceError::PipeWire("the loop thread stopped".to_owned())),
        }
    }

    /// Whether the loop thread has stopped taking commands.
    fn ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }

    /// Hands `command` to the loop thread; [`DeviceError::Closed`] once it has ended (the command would stay queued
    /// with nobody to take it).
    fn send(&self, command: Command) -> Result<(), DeviceError> {
        if self.ended() {
            return Err(DeviceError::Closed);
        }
        let sender = lock(&self.commands);
        sender.send(command).map_err(|_| DeviceError::Closed)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.send(Command::Quit);
        let thread = lock(&self.thread).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

/// Closes its stream when the device is dropped, and keeps the connection up while the device lives.
struct StreamGuard {
    id: u64,
    connection: Arc<Connection>,
}

impl StreamGuard {
    fn new(id: u64, connection: &Arc<Connection>) -> Self {
        Self {
            id,
            connection: Arc::clone(connection),
        }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        let _ = self.connection.send(Command::Close(self.id));
    }
}

struct Microphone {
    frames: Queue<AudioFrame>,
    _stream: StreamGuard,
}

impl AudioSource for Microphone {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<AudioFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

struct Camera {
    frames: Queue<VideoFrame>,
    _stream: StreamGuard,
}

impl VideoSource for Camera {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

/// A shared screen: its frames, its stream (on the portal's remote), and the portal's session it lives in.
struct Screen {
    frames: Queue<VideoFrame>,
    _stream: StreamGuard,
    _cast: portal::ScreenCast,
}

impl VideoSource for Screen {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, DeviceError> {
        self.frames.recv(timeout)
    }
}

struct Speaker {
    queue: Queue<f32>,
    _stream: StreamGuard,
}

impl AudioSink for Speaker {
    /// Queues the frame's samples (the oldest dropped beyond [`SPEAKER_QUEUE`]); [`DeviceError::Closed`] once the
    /// playback stream ended or the server went away.
    fn play(&mut self, frame: &AudioFrame) -> Result<(), DeviceError> {
        self.queue.extend(frame.samples().iter().copied()).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_reach_uses_its_server_camera_and_desktop_reach_uses_the_portal() {
        let env = SessionEnv::from_vars([(SESSION_BUS_ENV, "unix:path=/run/user/1000/bus")], 1000);
        assert_eq!(camera_route(Reach::Sandbox, &env), CameraRoute::Server);
        assert_eq!(
            camera_route(Reach::Desktop, &env),
            CameraRoute::Portal("unix:path=/run/user/1000/bus".to_owned())
        );
        let env = SessionEnv::from_vars([("XDG_RUNTIME_DIR", "/run/user/1000")], 1000);
        assert_eq!(
            camera_route(Reach::Desktop, &env),
            CameraRoute::Portal("unix:path=/run/user/1000/bus".to_owned()),
            "the bus in the runtime dir"
        );
        let none = SessionEnv::from_vars(Vec::<(&str, &str)>::new(), 1000);
        assert!(matches!(
            camera_route(Reach::Desktop, &none),
            CameraRoute::Unavailable(_)
        ));
    }

    #[test]
    fn an_address_value_s_escapes_are_undone() {
        assert_eq!(unescape_address("/tmp/a%20b%2cc").as_deref(), Some("/tmp/a b,c"));
        assert_eq!(unescape_address("/tmp/%2").as_deref(), None);
        assert_eq!(unescape_address("/tmp/%zz").as_deref(), None);
    }

    #[test]
    fn sandbox_reach_asks_only_a_portal_on_a_bus_in_the_sandbox() {
        let sandbox = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let bus = sandbox.path().join("bus");
        let elsewhere = outside.path().join("bus");
        let _listening = [&bus, &elsewhere].map(|path| std::os::unix::net::UnixListener::bind(path).unwrap());
        let env = SessionEnv::from_vars([(SANDBOX_ENV, sandbox.path())], own_uid());
        let address = |path: &Path| format!("unix:path={}", path.display());
        assert!(portal_bus(Reach::Sandbox, &env, &address(&bus)).is_ok());
        for refused_address in [
            address(&elsewhere),
            format!("{},guid=00", address(&bus)),
            format!("unix:abstract={}", bus.display()),
            "unix:path=relative/bus".to_owned(),
            format!("unix:path={}/..{}", sandbox.path().display(), elsewhere.display()),
        ] {
            assert!(
                matches!(
                    portal_bus(Reach::Sandbox, &env, &refused_address),
                    Err(DeviceError::Refused(_))
                ),
                "{refused_address}"
            );
        }
        let no_sandbox = SessionEnv::from_vars(Vec::<(&str, &str)>::new(), own_uid());
        assert!(matches!(
            portal_bus(Reach::Sandbox, &no_sandbox, &address(&bus)),
            Err(DeviceError::Refused(_))
        ));
        assert!(portal_bus(Reach::Desktop, &no_sandbox, &address(&elsewhere)).is_ok());
    }
}
