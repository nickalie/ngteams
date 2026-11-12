//! The notifications and the tray item on a real bus: the UI sandbox's private `dbus-daemon` with its fake
//! notification server and tray watcher (`ui_sandbox::bus`), never the desktop's.
//!
//! - [`BusNotifications`]: a notice reaches the server with the app's name, its summary and body, the `default`
//!   action and its hints; one replaces another by id; a close is the server's `CloseNotification`; the server's
//!   `ActionInvoked` for a body comes back as the notification's id, any other action doesn't.
//! - [`BusTray`]: the item registers its own name with the watcher; its title, tooltip, icon (the count drawn on it,
//!   its attention icon too) and status say the unread total, and change with it (`NewTitle` told), as does the Unity
//!   launcher entry's `Update` for the app's desktop entry; a click and the menu's "Open" ask for the window, the menu's "Quit"
//!   for the quit; its menu lays out "Open Teams" and "Quit". It follows its watcher: the watcher stopped is told
//!   `Gone`, started again `Registered` (the item registered with it again), and the bus killed ends the stream.
//! - The bus the app is given is the only one: a child process whose `DBUS_SESSION_BUS_ADDRESS` is unset (or names
//!   a bus that isn't there) and whose `$XDG_RUNTIME_DIR/bus` is a listening canary takes the app's own way to a bus
//!   (`bus::connect_given` on `bus::address_from_env`, which `Desktop::connect` takes) and connects nowhere (the
//!   canary is never reached), and one whose address is the private bus reaches it and nothing else.
//!
//! Without `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use futures_util::StreamExt as _;
use teams_ui::notify::{BusNotifications, Notice, NoticeKind, NotificationServer as _};
use teams_ui::tray::{
    BusTray, Hosted, ITEM_PATH, LAUNCHER_ENTRY, MENU_OPEN, MENU_PATH, MENU_QUIT, TrayEvent, TrayIcon as _,
};
use teams_ui::{app_icon, bus};
use tempfile::TempDir;
use tokio::sync::mpsc;
use ui_sandbox::bus::PrivateBus;
use ui_sandbox::{Tools, required, unavailable};
use zbus::zvariant::OwnedValue;

/// The child's mode, and the address it is handed.
const CHILD_ENV: &str = "TEAMS_UI_DESKTOP_BUS_CHILD";
/// How long a wait on the bus may take.
const WAIT: Duration = Duration::from_secs(10);

/// The private bus in a temp dir, or `None` (with a note) without `dbus-daemon`.
fn private_bus(test: &str) -> Option<(TempDir, PrivateBus)> {
    let dbus_daemon = match Tools::find() {
        Ok(tools) => tools.dbus_daemon,
        Err(reason) => {
            unavailable::<()>(test, &reason, required()).unwrap();
            return None;
        }
    };
    let dir = TempDir::new().unwrap();
    let bus = PrivateBus::start(&dbus_daemon, &dir.path().join("bus"), dir.path()).unwrap();
    Some((dir, bus))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn notice(kind: NoticeKind, summary: &str, body: &str) -> Notice {
    Notice {
        conversation: "19:0000000000000000000000000000trio@thread.v2".to_owned(),
        kind,
        summary: summary.to_owned(),
        body: body.to_owned(),
    }
}

#[test]
fn notifications_reach_the_server_on_the_bus_given_and_its_activations_come_back() {
    let Some((_dir, private)) = private_bus("notifications on the private bus") else {
        return;
    };
    runtime().block_on(async {
        let connection = bus::connect(private.address()).await.unwrap();
        let (server, mut activations) = BusNotifications::start(&connection, "ngteams (lab a)", "ngteams-lab")
            .await
            .unwrap();
        let first = server
            .show(&notice(NoticeKind::Message, "Bee", "lunch <at> noon & after"), 0)
            .await
            .unwrap();
        let second = server
            .show(&notice(NoticeKind::Collapsed, "Lab trio", "4 new messages"), first)
            .await
            .unwrap();
        assert_eq!(second, first, "the server replaced it");
        server.close(first).await;

        let got = private.notifications();
        assert_eq!(got.len(), 2, "{got:?}");
        let me = connection.unique_name().unwrap().to_string();
        assert_eq!(got[0].sender, me);
        assert_eq!(got[0].app_name, "ngteams (lab a)");
        assert_eq!(got[0].app_icon, app_icon::NAME);
        assert_eq!((got[0].summary.as_str(), got[0].replaces_id), ("Bee", 0));
        // the fake reads no markup: the text as it is
        assert_eq!(got[0].body, "lunch <at> noon & after");
        assert_eq!(got[0].actions, ["default", "Open"]);
        assert_eq!(got[0].hints, ["category", "desktop-entry", "urgency"]);
        assert_eq!(got[0].expire_timeout, -1);
        assert_eq!((got[1].summary.as_str(), got[1].replaces_id), ("Lab trio", first));
        assert_eq!(private.closed_notifications(), [first]);

        // every action comes back with its name: the body's is `default`
        private.invoke_action(first, "other").unwrap();
        private.invoke_action(first, "default").unwrap();
        let other = tokio::time::timeout(WAIT, activations.next()).await.unwrap();
        assert_eq!(other, Some((first, "other".to_owned())));
        let activated = tokio::time::timeout(WAIT, activations.next()).await.unwrap();
        assert_eq!(activated, Some((first, "default".to_owned())));

        // an incoming call's: Answer and Decline, critical, until it is closed
        let ringing = server
            .show(&notice(NoticeKind::Call, "Bee is calling", "Incoming call"), 0)
            .await
            .unwrap();
        let got = private.notifications();
        let call = got.iter().find(|shown| shown.id == ringing).unwrap();
        assert_eq!(
            call.actions,
            ["default", "Open", "answer", "Answer", "decline", "Decline"]
        );
        assert_eq!(call.hints, ["desktop-entry", "urgency"]);
        assert_eq!(call.expire_timeout, 0);
        private.invoke_action(ringing, "answer").unwrap();
        let answered = tokio::time::timeout(WAIT, activations.next()).await.unwrap();
        assert_eq!(answered, Some((ringing, "answer".to_owned())));
    });
}

/// A property of the item or its menu, read as a host reads it.
async fn property<T>(connection: &zbus::Connection, service: &str, path: &str, interface: &str, name: &str) -> T
where
    T: TryFrom<OwnedValue>,
    T::Error: Into<zbus::Error>,
{
    let proxy = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(connection)
        .destination(service.to_owned())
        .unwrap()
        .path(path.to_owned())
        .unwrap()
        .interface(interface.to_owned())
        .unwrap()
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    proxy.get_property::<T>(name).await.unwrap()
}

const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
/// The item's tooltip: an icon name, its pixmaps, a title and a description.
type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);
/// A menu item: its id, its properties and its children.
type MenuItem = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// The item shows the app's icon by its pixels, badged with the `unread` count while it isn't 0 (its attention icon
/// too), with no theme name over them.
async fn assert_icon(host: &zbus::Connection, name: &str, unread: u64) {
    let icon: String = property(host, name, ITEM_PATH, ITEM, "IconName").await;
    assert_eq!(icon, "");
    let pixmaps: Vec<(i32, i32, Vec<u8>)> = property(host, name, ITEM_PATH, ITEM, "IconPixmap").await;
    assert_eq!(pixmaps, app_icon::pixmaps(unread));
    let (_, tip_icon, _, _): ToolTip = property(host, name, ITEM_PATH, ITEM, "ToolTip").await;
    assert_eq!(tip_icon, app_icon::pixmaps(unread));
    let attention: Vec<(i32, i32, Vec<u8>)> = property(host, name, ITEM_PATH, ITEM, "AttentionIconPixmap").await;
    assert_eq!(attention, app_icon::pixmaps(unread));
}

/// The launcher entry's next `Update`: the desktop entry it names, its count and whether the count is shown.
async fn next_launcher_update(updates: &mut zbus::MessageStream) -> (String, i64, bool) {
    let message = tokio::time::timeout(WAIT, updates.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (uri, mut properties): (String, HashMap<String, OwnedValue>) = message.body().deserialize().unwrap();
    let count = i64::try_from(properties.remove("count").unwrap()).unwrap();
    let visible = bool::try_from(properties.remove("count-visible").unwrap()).unwrap();
    (uri, count, visible)
}

/// The item's title, tooltip, status and icons follow the count `tray` is given, as the launcher entry's `Update` does;
/// the same count again tells nothing.
async fn the_count_shows(host: &zbus::Connection, item: &zbus::Proxy<'_>, tray: &BusTray) {
    let name = tray.name();
    let mut titled = item.receive_signal("NewTitle").await.unwrap();
    let mut attention = item.receive_signal("NewAttentionIcon").await.unwrap();
    let launcher = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(LAUNCHER_ENTRY)
        .unwrap()
        .member("Update")
        .unwrap()
        .build();
    let mut updates = zbus::MessageStream::for_match_rule(launcher, host, None).await.unwrap();
    // the counts given, then the title and status shown
    let steps: [(&[u64], &str, &str); 3] = [
        (&[3], "ngteams (lab a): 3 unread chats", "NeedsAttention"),
        (&[3, 120], "ngteams (lab a): 120 unread chats", "NeedsAttention"),
        (&[0], "ngteams (lab a)", "Active"),
    ];
    for (counts, title, status) in steps {
        for &count in counts {
            tray.set_unread(count).await;
        }
        let shown = counts[counts.len() - 1];
        tokio::time::timeout(WAIT, titled.next()).await.unwrap().unwrap();
        tokio::time::timeout(WAIT, attention.next()).await.unwrap().unwrap();
        assert_titled(host, name, title, status).await;
        assert_icon(host, name, shown).await;
        let update = next_launcher_update(&mut updates).await;
        let uri = "application://ngteams-lab.desktop".to_owned();
        assert_eq!(update, (uri, i64::try_from(shown).unwrap(), shown > 0));
    }
}

/// The item's title, tooltip and status.
async fn assert_titled(host: &zbus::Connection, name: &str, title: &str, status: &str) {
    let shown: String = property(host, name, ITEM_PATH, ITEM, "Title").await;
    assert_eq!(shown, title);
    let (_, _, tip, _): ToolTip = property(host, name, ITEM_PATH, ITEM, "ToolTip").await;
    assert_eq!(tip, title);
    let shown: String = property(host, name, ITEM_PATH, ITEM, "Status").await;
    assert_eq!(shown, status);
}

#[test]
fn the_tray_item_registers_shows_the_total_and_asks_for_the_window_and_the_quit() {
    let Some((_dir, private)) = private_bus("the tray item on the private bus") else {
        return;
    };
    runtime().block_on(async {
        let connection = bus::connect(private.address()).await.unwrap();
        let (events, mut asked) = mpsc::unbounded_channel();
        let (tray, _hosted) = BusTray::start(&connection, "ngteams-lab", "ngteams (lab a)", events)
            .await
            .unwrap();
        let name = tray.name().to_owned();
        assert_eq!(name, format!("org.kde.StatusNotifierItem-{}-1", std::process::id()));
        let items = private.tray_items();
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0].service, name);
        assert_eq!(items[0].sender, connection.unique_name().unwrap().to_string());

        // a host on a connection of its own
        let host = private.connect().unwrap();
        let host: zbus::Connection = host.into();
        let title: String = property(&host, &name, ITEM_PATH, ITEM, "Title").await;
        assert_eq!(title, "ngteams (lab a)");
        let id: String = property(&host, &name, ITEM_PATH, ITEM, "Id").await;
        assert_eq!(id, "ngteams-lab");
        let menu: zbus::zvariant::OwnedObjectPath = property(&host, &name, ITEM_PATH, ITEM, "Menu").await;
        assert_eq!(menu.as_str(), MENU_PATH);

        let item = zbus::Proxy::new(&host, name.clone(), ITEM_PATH, ITEM).await.unwrap();
        the_count_shows(&host, &item, &tray).await;

        item.call::<_, _, ()>("Activate", &(0_i32, 0_i32)).await.unwrap();
        assert_eq!(asked.recv().await, Some(TrayEvent::Open));

        let menu = zbus::Proxy::new(&host, name.clone(), MENU_PATH, MENU).await.unwrap();
        let version: u32 = property(&host, &name, MENU_PATH, MENU, "Version").await;
        assert_eq!(version, 3);
        let (_, (root, _, children)): (u32, MenuItem) = menu
            .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
            .await
            .unwrap();
        assert_eq!((root, children.len()), (0, 2));
        let labels: Vec<(i32, HashMap<String, OwnedValue>)> = menu
            .call("GetGroupProperties", &(vec![MENU_OPEN, MENU_QUIT], vec!["label"]))
            .await
            .unwrap();
        let labels: Vec<(i32, String)> = labels
            .into_iter()
            .map(|(id, mut properties)| {
                let label = String::try_from(properties.remove("label").unwrap()).unwrap();
                (id, label)
            })
            .collect();
        assert_eq!(
            labels,
            [(MENU_OPEN, "Open Teams".to_owned()), (MENU_QUIT, "Quit".to_owned())]
        );
        let data = zbus::zvariant::Value::from("");
        menu.call::<_, _, ()>("Event", &(MENU_OPEN, "clicked", &data, 0_u32))
            .await
            .unwrap();
        menu.call::<_, _, ()>("Event", &(MENU_QUIT, "hovered", &data, 0_u32))
            .await
            .unwrap();
        menu.call::<_, _, ()>("Event", &(MENU_QUIT, "clicked", &data, 0_u32))
            .await
            .unwrap();
        assert_eq!(asked.recv().await, Some(TrayEvent::Open));
        assert_eq!(asked.recv().await, Some(TrayEvent::Quit));
    });
}

#[test]
fn the_tray_item_follows_its_watcher_and_is_gone_with_the_bus() {
    let Some((_dir, private)) = private_bus("the tray item follows its watcher") else {
        return;
    };
    runtime().block_on(async {
        let connection = bus::connect(private.address()).await.unwrap();
        let (events, _asked) = mpsc::unbounded_channel();
        let (tray, mut hosted) = BusTray::start(&connection, "ngteams-lab", "ngteams (lab a)", events)
            .await
            .unwrap();
        assert_eq!(private.tray_items().len(), 1);
        // the panel quits: nothing shows the item
        private.stop_watcher().unwrap();
        let change = tokio::time::timeout(WAIT, hosted.next()).await.unwrap();
        assert_eq!(change, Some(Hosted::Gone));
        // and starts again: the item registers with the new watcher
        private.start_watcher().unwrap();
        let change = tokio::time::timeout(WAIT, hosted.next()).await.unwrap();
        assert_eq!(change, Some(Hosted::Registered));
        let items = private.tray_items();
        assert_eq!(items.len(), 2, "{items:?}");
        assert_eq!(items[1].service, tray.name());
        assert_eq!(items[1].sender, connection.unique_name().unwrap().to_string());
        // the bus goes away: the item with it
        drop(private);
        let change = tokio::time::timeout(WAIT, hosted.next()).await.unwrap();
        assert_eq!(change, None);
    });
}

/// A canary: a listening socket nobody should connect to; how many did.
struct Canary {
    listener: UnixListener,
}

impl Canary {
    fn plant(path: &Path) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self { listener }
    }

    fn reached(&self) -> usize {
        std::iter::from_fn(|| self.listener.accept().ok()).count()
    }
}

/// Runs this test binary's `the_bus_given_is_the_only_one` again as the child, in `mode`, with `env`.
fn child(mode: &str, env: &[(&str, &str)]) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "the_bus_given_is_the_only_one",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env(CHILD_ENV, mode);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "the child failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// The child: what the app does with the environment it was started with, the way the app does it
/// (`bus::connect_given` on `bus::address_from_env`, as `Desktop::connect`): the notifications and the tray item on
/// that bus, or nothing. A way that fell back elsewhere would reach the canary (and wait there for an answer the
/// canary never gives, so the child gives up after [`WAIT`]).
fn as_the_app(mode: &str) {
    runtime().block_on(async {
        let connected = tokio::time::timeout(WAIT, bus::connect_given(bus::address_from_env())).await;
        let connected = connected.expect("the child's connect never ended: it reached a bus that never answered");
        match mode {
            "unset" => {
                assert!(matches!(connected, Err(bus::BusError::NotGiven)), "{connected:?}");
                println!("child: no bus");
            }
            "gone" => {
                assert!(matches!(connected, Err(bus::BusError::Connect(_))), "{connected:?}");
                println!("child: refused");
            }
            "given" => {
                let connection = connected.unwrap();
                let (server, _) = BusNotifications::start(&connection, "child", "child").await.unwrap();
                server.show(&notice(NoticeKind::Message, "Bee", "hi"), 0).await.unwrap();
                let (events, _asked) = mpsc::unbounded_channel();
                let (_tray, _hosted) = BusTray::start(&connection, "child", "child", events).await.unwrap();
                println!("child: shown");
            }
            other => panic!("unknown mode {other}"),
        }
    });
}

#[test]
fn the_bus_given_is_the_only_one() {
    if let Ok(mode) = std::env::var(CHILD_ENV) {
        as_the_app(&mode);
        return;
    }
    let Some((dir, private)) = private_bus("the bus given is the only one") else {
        return;
    };
    let runtime_dir = dir.path().join("runtime");
    std::fs::create_dir(&runtime_dir).unwrap();
    // where a client falls back to without an address
    let fallback = Canary::plant(&runtime_dir.join("bus"));
    let runtime_dir = runtime_dir.to_string_lossy().into_owned();

    let said = child("unset", &[("XDG_RUNTIME_DIR", &runtime_dir)]);
    assert!(said.contains("child: no bus"), "{said}");
    let gone = format!("unix:path={}", dir.path().join("no-bus-here").display());
    let said = child(
        "gone",
        &[("XDG_RUNTIME_DIR", &runtime_dir), ("DBUS_SESSION_BUS_ADDRESS", &gone)],
    );
    assert!(said.contains("child: refused"), "{said}");
    let said = child(
        "given",
        &[
            ("XDG_RUNTIME_DIR", &runtime_dir),
            ("DBUS_SESSION_BUS_ADDRESS", private.address()),
        ],
    );
    assert!(said.contains("child: shown"), "{said}");
    assert_eq!(fallback.reached(), 0, "a client fell back to $XDG_RUNTIME_DIR/bus");
    assert_eq!(private.notifications().len(), 1);
    assert_eq!(private.tray_items().len(), 1);
}
