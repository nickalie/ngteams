//! The sandbox's private D-Bus session bus and the fakes on it.
//!
//! `dbus-daemon` runs on a config of our own rather than `--session`: the stock session config activates services
//! from the desktop's service dirs (a portal, a notification daemon, a keyring), which would start them on this bus.
//! Ours lists no service dir, so a name nobody owns stays unowned and a call to it fails at once
//! (`ServiceUnknown`); its socket lies in the client's runtime dir as `bus`, where a client that falls back from a
//! missing `DBUS_SESSION_BUS_ADDRESS` to `$XDG_RUNTIME_DIR/bus` finds it too.
//!
//! On it, from this process (each over a connection to this bus's address, never one taken from the environment):
//! a fake notification server (`org.freedesktop.Notifications`) and a fake `StatusNotifierItem` watcher
//! (`org.kde.StatusNotifierWatcher`), each recording every call with its sender. The watcher can be stopped and
//! started again ([`PrivateBus::stop_watcher`], [`PrivateBus::start_watcher`]), as a panel that restarts. With the
//! sandbox's PipeWire, a fake camera portal too (`org.freedesktop.portal.Desktop`'s `org.freedesktop.portal.Camera`,
//! [`PrivateBus::start_camera_portal`]): it answers `AccessCamera` with the user's answer the test sets
//! ([`PrivateBus::set_camera_allowed`]), and `OpenPipeWireRemote`, once access was granted to that caller, with a
//! connection to the sandbox's PipeWire (the whole server, where the real portal's remote shows only the cameras).
//! Beside it on the same object, a fake screen-cast portal (`org.freedesktop.portal.ScreenCast`): `CreateSession`,
//! `SelectSources` and `Start` each answered at once (the user's pick: the sandbox's synthetic screen, its node as
//! `Start`'s one stream), and `OpenPipeWireRemote` on a started session with a connection to the sandbox's PipeWire.
//! Every call is recorded as the camera portal's are.

use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
};

use zbus::{
    blocking::{Connection, connection::Builder, fdo::DBusProxy},
    interface,
    message::Header,
    names::BusName,
    zvariant::{ObjectPath, OwnedFd, OwnedObjectPath, Value},
};

use crate::{SOCKET_WAIT, SandboxResult, clean_command, spawn, wait_until};

/// The daemon's log's name in the sandbox dir.
pub const LOG: &str = "dbus.log";

/// The notification server's bus name and object path.
pub const NOTIFICATIONS_NAME: &str = "org.freedesktop.Notifications";
pub const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
/// The `StatusNotifierItem` watcher's bus name and object path.
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// The desktop portal's bus name and object path, and the request objects' interface.
pub const PORTAL_NAME: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";

/// One call the fake camera portal got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalCall {
    /// The caller's unique bus name.
    pub sender: String,
    /// `AccessCamera` or `OpenPipeWireRemote` (the camera's); `CreateSession`, `SelectSources`, `Start`,
    /// `OpenPipeWireRemote` or `Close` (the screen cast's, each its method name with `ScreenCast.` before it).
    pub method: &'static str,
    /// What the portal answered: `granted`, `denied`, `remote`, `refused`, `session`, `selected`, `screen`, `none` or
    /// `closed`.
    pub answer: &'static str,
}

/// One `Notify` call the fake server got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The caller's unique bus name.
    pub sender: String,
    /// The id the server answered with.
    pub id: u32,
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<String>,
    /// The hints' keys, sorted (their values aren't kept but the picture's).
    pub hints: Vec<String>,
    /// The `image-data` hint's picture, when it is one: its width, height and pixels' bytes (RGBA, row after row).
    pub image: Option<(i32, i32, Vec<u8>)>,
    pub expire_timeout: i32,
}

/// One `RegisterStatusNotifierItem` call the fake watcher got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayItem {
    /// The caller's unique bus name.
    pub sender: String,
    /// The service it named: a bus name, or an object path on the caller's connection.
    pub service: String,
}

/// What the fakes recorded, shared with them.
#[derive(Default)]
struct Records {
    notifications: Vec<Notification>,
    closed: Vec<u32>,
    tray_items: Vec<TrayItem>,
    tray_hosts: Vec<String>,
    portal_calls: Vec<PortalCall>,
    /// The user's answer to `AccessCamera`.
    camera_denied: bool,
    /// The callers granted the camera.
    camera_granted: Vec<String>,
    /// The screen-cast sessions started, by caller and session path.
    screen_cast_started: Vec<(String, String)>,
}

type Shared = Arc<Mutex<Records>>;

fn records(shared: &Shared) -> std::sync::MutexGuard<'_, Records> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

fn sender(header: &Header<'_>) -> String {
    header.sender().map(ToString::to_string).unwrap_or_default()
}

/// The fake `org.freedesktop.Notifications`.
struct Notifications {
    records: Shared,
    next_id: u32,
}

/// What a `Notify` call carries apart from its sender.
struct Notify<'a> {
    app_name: &'a str,
    replaces_id: u32,
    app_icon: &'a str,
    summary: &'a str,
    body: &'a str,
    actions: Vec<&'a str>,
    hints: Vec<String>,
    image: Option<(i32, i32, Vec<u8>)>,
    expire_timeout: i32,
}

impl Notifications {
    fn record(&mut self, sender: String, call: Notify<'_>) -> u32 {
        let id = if call.replaces_id == 0 {
            self.next_id = self.next_id.wrapping_add(1).max(1);
            self.next_id
        } else {
            call.replaces_id
        };
        let mut hints = call.hints;
        hints.sort();
        records(&self.records).notifications.push(Notification {
            sender,
            id,
            app_name: call.app_name.to_owned(),
            replaces_id: call.replaces_id,
            app_icon: call.app_icon.to_owned(),
            summary: call.summary.to_owned(),
            body: call.body.to_owned(),
            actions: call.actions.iter().map(|&action| action.to_owned()).collect(),
            hints,
            image: call.image,
            expire_timeout: call.expire_timeout,
        });
        id
    }
}

#[interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[expect(
        clippy::too_many_arguments,
        reason = "the D-Bus method's own signature (susssasa{sv}i), which zbus maps to one argument each"
    )]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands a method its message header by value"
    )]
    fn notify(
        &mut self,
        #[zbus(header)] header: Header<'_>,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<&str>,
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> u32 {
        let image = hints.get("image-data").and_then(image_data);
        let call = Notify {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints: hints.into_keys().map(str::to_owned).collect(),
            image,
            expire_timeout,
        };
        self.record(sender(&header), call)
    }

    fn close_notification(&self, id: u32) {
        records(&self.records).closed.push(id);
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn get_capabilities(&self) -> Vec<&'static str> {
        vec!["body", "actions"]
    }

    #[expect(clippy::unused_self, reason = "a D-Bus method zbus calls on the object")]
    fn get_server_information(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        ("ui-sandbox", "ngteams", "0", "1.2")
    }
}

/// An `image-data` hint's picture, `(iiibiiay)`: its width, height and pixels, when it is 8-bit RGBA whose rows are
/// as wide as it says.
fn image_data(value: &Value<'_>) -> Option<(i32, i32, Vec<u8>)> {
    let (width, height, stride, alpha, bits, channels, pixels): (i32, i32, i32, bool, i32, i32, Vec<u8>) =
        value.try_clone().ok()?.try_into().ok()?;
    let rows = usize::try_from(stride)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    (alpha && bits == 8 && channels == 4 && stride == width.checked_mul(4)? && pixels.len() == rows)
        .then_some((width, height, pixels))
}

/// The fake `org.kde.StatusNotifierWatcher`: it says a host is there, so an app registers its item.
struct Watcher {
    records: Shared,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands a method its message header by value"
    )]
    fn register_status_notifier_item(&self, #[zbus(header)] header: Header<'_>, service: &str) {
        records(&self.records).tray_items.push(TrayItem {
            sender: sender(&header),
            service: service.to_owned(),
        });
    }

    fn register_status_notifier_host(&self, service: &str) {
        records(&self.records).tray_hosts.push(service.to_owned());
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        records(&self.records)
            .tray_items
            .iter()
            .map(|item| item.service.clone())
            .collect()
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn protocol_version(&self) -> i32 {
        0
    }
}

/// The fake `org.freedesktop.portal.Camera` (see the module docs).
struct CameraPortal {
    records: Shared,
    /// The sandbox's PipeWire socket, which `OpenPipeWireRemote` connects to.
    pipewire: PathBuf,
}

impl CameraPortal {
    fn record(&self, sender: String, method: &'static str, answer: &'static str) {
        records(&self.records)
            .portal_calls
            .push(PortalCall { sender, method, answer });
    }
}

/// The request object's path for `sender` and its `token`: the portal's own rule.
fn request_path(sender: &str, token: &str) -> String {
    format!(
        "{PORTAL_PATH}/request/{}/{token}",
        sender.trim_start_matches(':').replace('.', "_")
    )
}

fn failed(error: impl std::fmt::Display) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}

#[interface(name = "org.freedesktop.portal.Camera")]
impl CameraPortal {
    /// Answers at once, with the answer set ([`PrivateBus::set_camera_allowed`]), as the `Response` of the request
    /// object the caller's token names.
    async fn access_camera(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = sender(&header);
        let token = match options.get("handle_token") {
            Some(Value::Str(token)) => token.as_str().to_owned(),
            _ => "t".to_owned(),
        };
        let path = request_path(&sender, &token);
        let denied = records(&self.records).camera_denied;
        let (code, answer) = if denied { (1_u32, "denied") } else { (0, "granted") };
        if !denied {
            records(&self.records).camera_granted.push(sender.clone());
        }
        self.record(sender.clone(), "AccessCamera", answer);
        let results: HashMap<&str, Value<'_>> = HashMap::new();
        connection
            .emit_signal(
                Some(sender.as_str()),
                path.as_str(),
                REQUEST_INTERFACE,
                "Response",
                &(code, results),
            )
            .await?;
        OwnedObjectPath::try_from(path).map_err(failed)
    }

    /// A connection to the sandbox's PipeWire, for a caller granted the camera.
    #[zbus(name = "OpenPipeWireRemote")]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands a method its message header by value"
    )]
    fn open_pipewire_remote(
        &self,
        #[zbus(header)] header: Header<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<OwnedFd> {
        // the portal's version 1 names no option
        drop(options);
        let sender = sender(&header);
        if !records(&self.records).camera_granted.contains(&sender) {
            self.record(sender, "OpenPipeWireRemote", "refused");
            return Err(zbus::fdo::Error::AccessDenied("the camera wasn't granted".to_owned()));
        }
        let remote = UnixStream::connect(&self.pipewire).map_err(failed)?;
        self.record(sender, "OpenPipeWireRemote", "remote");
        Ok(OwnedFd::from(std::os::fd::OwnedFd::from(remote)))
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn is_camera_present(&self) -> bool {
        true
    }

    #[zbus(property, name = "version")]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn version(&self) -> u32 {
        1
    }
}

/// The fake `org.freedesktop.portal.ScreenCast` (see the module docs).
struct ScreenCastPortal {
    records: Shared,
    /// The sandbox's PipeWire socket, which `OpenPipeWireRemote` connects to.
    pipewire: PathBuf,
    /// The sandbox's screen's node (`0` until the server made it).
    node: Arc<AtomicU32>,
}

impl ScreenCastPortal {
    fn record(&self, sender: String, method: &'static str, answer: &'static str) {
        records(&self.records)
            .portal_calls
            .push(PortalCall { sender, method, answer });
    }

    /// Answers a request at once: its `Response` (`code`, `results`) at the request object the caller's token names.
    async fn respond(
        connection: &zbus::Connection,
        sender: &str,
        options: &HashMap<&str, Value<'_>>,
        (code, results): (u32, HashMap<&str, Value<'_>>),
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let path = request_path(sender, &token_of(options, "handle_token"));
        connection
            .emit_signal(
                Some(sender),
                path.as_str(),
                REQUEST_INTERFACE,
                "Response",
                &(code, results),
            )
            .await?;
        OwnedObjectPath::try_from(path).map_err(failed)
    }
}

/// The token an option names (`handle_token`, `session_handle_token`), else `t`.
fn token_of(options: &HashMap<&str, Value<'_>>, name: &str) -> String {
    match options.get(name) {
        Some(Value::Str(token)) => token.as_str().to_owned(),
        _ => "t".to_owned(),
    }
}

#[interface(name = "org.freedesktop.portal.ScreenCast")]
impl ScreenCastPortal {
    /// A session, at the path its token names (as the real portal derives it).
    async fn create_session(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = sender(&header);
        let session = format!(
            "{PORTAL_PATH}/session/{}/{}",
            sender.trim_start_matches(':').replace('.', "_"),
            token_of(&options, "session_handle_token")
        );
        self.record(sender.clone(), "ScreenCast.CreateSession", "session");
        let mut results = HashMap::new();
        results.insert("session_handle", Value::from(session.as_str()));
        Self::respond(connection, &sender, &options, (0, results)).await
    }

    async fn select_sources(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        session: ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        drop(session);
        let sender = sender(&header);
        self.record(sender.clone(), "ScreenCast.SelectSources", "selected");
        Self::respond(connection, &sender, &options, (0, HashMap::new())).await
    }

    /// The user's pick: the sandbox's screen as the session's one stream; none (the request ended) before its node
    /// was made.
    async fn start(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        session: ObjectPath<'_>,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let _ = parent_window;
        let sender = sender(&header);
        let node = self.node.load(Ordering::Relaxed);
        if node == 0 {
            self.record(sender.clone(), "ScreenCast.Start", "none");
            return Self::respond(connection, &sender, &options, (2, HashMap::new())).await;
        }
        records(&self.records)
            .screen_cast_started
            .push((sender.clone(), session.to_string()));
        self.record(sender.clone(), "ScreenCast.Start", "screen");
        let stream: (u32, HashMap<&str, Value<'_>>) = (node, HashMap::new());
        let mut results = HashMap::new();
        results.insert("streams", Value::from(vec![stream]));
        Self::respond(connection, &sender, &options, (0, results)).await
    }

    /// A connection to the sandbox's PipeWire, for a session its caller started.
    #[zbus(name = "OpenPipeWireRemote")]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "zbus hands a method its message header by value"
    )]
    fn open_pipewire_remote(
        &self,
        #[zbus(header)] header: Header<'_>,
        session: ObjectPath<'_>,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<OwnedFd> {
        drop(options);
        let sender = sender(&header);
        let started = (sender.clone(), session.to_string());
        if !records(&self.records).screen_cast_started.contains(&started) {
            self.record(sender, "ScreenCast.OpenPipeWireRemote", "refused");
            return Err(zbus::fdo::Error::AccessDenied("the session wasn't started".to_owned()));
        }
        let remote = UnixStream::connect(&self.pipewire).map_err(failed)?;
        self.record(sender, "ScreenCast.OpenPipeWireRemote", "remote");
        Ok(OwnedFd::from(std::os::fd::OwnedFd::from(remote)))
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn available_source_types(&self) -> u32 {
        // a monitor or a window
        3
    }

    #[zbus(property)]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn available_cursor_modes(&self) -> u32 {
        // hidden, embedded, metadata
        7
    }

    #[zbus(property, name = "version")]
    #[expect(clippy::unused_self, reason = "a D-Bus property zbus reads on the object")]
    fn version(&self) -> u32 {
        4
    }
}

/// `text` as a D-Bus address value: bytes outside `[-0-9A-Za-z_/.\*]` percent-escaped.
#[must_use]
pub fn escape_address_value(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_/.\\*".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            let _ = write!(escaped, "%{byte:02x}");
        }
    }
    escaped
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The daemon's config: a session bus on `address`, no service activation, everyone may own and call anything.
fn config(address: &str) -> String {
    format!(
        "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
         \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
         <busconfig>\n  <type>session</type>\n  <keep_umask/>\n  <listen>{}</listen>\n  <auth>EXTERNAL</auth>\n  \
         <policy context=\"default\">\n    <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    \
         <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
        escape_xml(address)
    )
}

/// A private `dbus-daemon` and the fakes on it; the daemon is killed on drop.
pub struct PrivateBus {
    daemon: Child,
    address: String,
    records: Shared,
    /// The notification server's connection, kept open for the bus's life.
    connections: Vec<Connection>,
    /// The watcher's connection, while it runs.
    watcher: Mutex<Option<Connection>>,
    /// The camera portal's connection, once it runs.
    portal: Mutex<Option<Connection>>,
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

impl PrivateBus {
    /// Starts `dbus-daemon` listening on `socket`, with its config and log in `dir`, then the fakes on it.
    ///
    /// # Errors
    /// The daemon fails to start or make its socket in time, or a fake can't take its name.
    pub fn start(dbus_daemon: &Path, socket: &Path, dir: &Path) -> SandboxResult<Self> {
        let address = format!("unix:path={}", escape_address_value(&socket.to_string_lossy()));
        let config_file = dir.join("dbus.conf");
        fs::write(&config_file, config(&address))?;
        let mut command = clean_command(dbus_daemon);
        command
            .arg(format!("--config-file={}", config_file.display()))
            .args(["--nofork", "--nopidfile"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(dir.join(LOG))?);
        spawn::pass_only(&mut command, &[]);
        let daemon = command.spawn()?;
        let mut bus = Self {
            daemon,
            address,
            records: Shared::default(),
            connections: Vec::new(),
            watcher: Mutex::new(None),
            portal: Mutex::new(None),
        };
        if wait_until(SOCKET_WAIT, || socket.exists().then_some(())).is_none() {
            let log = fs::read_to_string(dir.join(LOG)).unwrap_or_default();
            return Err(format!("dbus-daemon made no socket at {}: {log}", socket.display()).into());
        }
        bus.serve()?;
        Ok(bus)
    }

    fn serve(&mut self) -> SandboxResult {
        let notifications = Notifications {
            records: Arc::clone(&self.records),
            next_id: 0,
        };
        let notifications = Builder::address(self.address.as_str())?
            .name(NOTIFICATIONS_NAME)?
            .serve_at(NOTIFICATIONS_PATH, notifications)?
            .build()?;
        self.connections.push(notifications);
        self.start_watcher()
    }

    fn watcher(&self) -> std::sync::MutexGuard<'_, Option<Connection>> {
        self.watcher.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts the fake watcher on a connection of its own, unless it runs: it takes the watcher's name, and what
    /// registers with it is recorded with what registered before.
    ///
    /// # Errors
    /// The connection or the name can't be had.
    pub fn start_watcher(&self) -> SandboxResult {
        let mut running = self.watcher();
        if running.is_none() {
            let watcher = Watcher {
                records: Arc::clone(&self.records),
            };
            let connection = Builder::address(self.address.as_str())?
                .name(WATCHER_NAME)?
                .serve_at(WATCHER_PATH, watcher)?
                .build()?;
            *running = Some(connection);
        }
        Ok(())
    }

    /// Stops the fake watcher, as a panel that quits: its connection closes, so its name has no owner.
    ///
    /// # Errors
    /// The connection can't be closed.
    pub fn stop_watcher(&self) -> SandboxResult {
        if let Some(connection) = self.watcher().take() {
            connection.close()?;
        }
        Ok(())
    }

    /// Starts the fake camera and screen-cast portals, their remotes on the PipeWire at `pipewire` (the sandbox's
    /// socket), the screen cast sharing the node `screen` holds (once the server made it), unless they run.
    ///
    /// # Errors
    /// The connection or the name can't be had.
    pub fn start_camera_portal(&self, pipewire: &Path, screen: Arc<AtomicU32>) -> SandboxResult {
        let mut running = self.portal.lock().unwrap_or_else(PoisonError::into_inner);
        if running.is_none() {
            let portal = CameraPortal {
                records: Arc::clone(&self.records),
                pipewire: pipewire.to_owned(),
            };
            let cast = ScreenCastPortal {
                records: Arc::clone(&self.records),
                pipewire: pipewire.to_owned(),
                node: screen,
            };
            let connection = Builder::address(self.address.as_str())?
                .name(PORTAL_NAME)?
                .serve_at(PORTAL_PATH, portal)?
                .serve_at(PORTAL_PATH, cast)?
                .build()?;
            *running = Some(connection);
        }
        Ok(())
    }

    /// The user's answer the fake camera portal gives from now on (allowed until said otherwise).
    pub fn set_camera_allowed(&self, allowed: bool) {
        records(&self.records).camera_denied = !allowed;
    }

    /// The calls the fake camera portal got, in order.
    #[must_use]
    pub fn portal_calls(&self) -> Vec<PortalCall> {
        records(&self.records).portal_calls.clone()
    }

    /// The bus's address, for `DBUS_SESSION_BUS_ADDRESS`.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The daemon's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.daemon.id()
    }

    /// A new connection to this bus (its address only, never the environment's).
    ///
    /// # Errors
    /// The bus refuses it.
    pub fn connect(&self) -> SandboxResult<Connection> {
        Ok(Builder::address(self.address.as_str())?.build()?)
    }

    /// The `Notify` calls the fake server got, in order.
    #[must_use]
    pub fn notifications(&self) -> Vec<Notification> {
        records(&self.records).notifications.clone()
    }

    /// The ids `CloseNotification` was called with, in order.
    #[must_use]
    pub fn closed_notifications(&self) -> Vec<u32> {
        records(&self.records).closed.clone()
    }

    /// The user activating notification `id` with `action` (`default` for its body): the fake server's
    /// `ActionInvoked` signal, sent to every connection on the bus as notification servers send it.
    ///
    /// # Errors
    /// The signal can't be sent.
    pub fn invoke_action(&self, id: u32, action: &str) -> SandboxResult {
        let server = self.connections.first().ok_or("no connection to the bus")?;
        server.emit_signal(
            None::<BusName<'_>>,
            NOTIFICATIONS_PATH,
            NOTIFICATIONS_NAME,
            "ActionInvoked",
            &(id, action),
        )?;
        Ok(())
    }

    /// The items registered with the fake watcher, in order.
    #[must_use]
    pub fn tray_items(&self) -> Vec<TrayItem> {
        records(&self.records).tray_items.clone()
    }

    /// The hosts registered with the fake watcher, in order.
    #[must_use]
    pub fn tray_hosts(&self) -> Vec<String> {
        records(&self.records).tray_hosts.clone()
    }

    /// The process ids of every connection on the bus but the daemon's own (the fakes' included). A connection that
    /// left between the listing and its lookup (a short-lived one, as niri's screenshot notification) is skipped.
    ///
    /// # Errors
    /// The bus can't be asked.
    pub fn client_pids(&self) -> SandboxResult<Vec<u32>> {
        let connection = self.connections.first().ok_or("no connection to the bus")?;
        let proxy = DBusProxy::new(connection)?;
        let mut pids = Vec::new();
        for name in proxy.list_names()? {
            if name.starts_with(':')
                && let Some(pid) = unless_gone(proxy.get_connection_unix_process_id(BusName::from(name)))?
            {
                pids.push(pid);
            }
        }
        pids.sort_unstable();
        pids.dedup();
        Ok(pids)
    }
}

/// A lookup's answer, `None` when the name it asked about has no owner any more.
fn unless_gone<T>(answer: zbus::fdo::Result<T>) -> zbus::fdo::Result<Option<T>> {
    match answer {
        Ok(value) => Ok(Some(value)),
        Err(zbus::fdo::Error::NameHasNoOwner(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lookup_skips_only_a_name_that_left() {
        assert_eq!(unless_gone(Ok(7_u32)), Ok(Some(7)));
        assert_eq!(
            unless_gone::<u32>(Err(zbus::fdo::Error::NameHasNoOwner(":1.9".into()))),
            Ok(None)
        );
        let denied = zbus::fdo::Error::AccessDenied("no".into());
        assert_eq!(unless_gone::<u32>(Err(denied.clone())), Err(denied));
    }

    #[test]
    fn a_request_path_is_the_portal_s_own() {
        assert_eq!(
            request_path(":1.42", "teams_camera_1"),
            "/org/freedesktop/portal/desktop/request/1_42/teams_camera_1"
        );
    }

    #[test]
    fn an_address_value_escapes_what_dbus_reserves() {
        assert_eq!(escape_address_value("/tmp/.tmpAb_c-1/bus"), "/tmp/.tmpAb_c-1/bus");
        assert_eq!(escape_address_value("/tmp/a b,c=d;e"), "/tmp/a%20b%2cc%3dd%3be");
    }

    #[test]
    fn the_config_activates_nothing() {
        let config = config("unix:path=/tmp/x&y/bus");
        assert!(
            config.contains("<listen>unix:path=/tmp/x&amp;y/bus</listen>"),
            "{config}"
        );
        for activation in ["servicedir", "standard_session_servicedirs", "include"] {
            assert!(!config.contains(activation), "{activation} in {config}");
        }
    }
}
