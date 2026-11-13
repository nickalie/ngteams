//! The desktop on GPUI's test context, over the fake engine (`common`), a fake notification server and a fake tray
//! item: the unread total the tray shows (the listed chats unread, counted again as chats change); closing the window
//! quits without a tray item and keeps the app with one, which shows the window again; a tray item gone (its
//! watcher left, the bus lost) while the window is closed opens it again, and closing it then quits; activating a
//! notification
//! opens its chat, in a window opened again if it was closed; the chat shown in a focused window never notifies; the
//! tray's "Quit". The same item on the sandbox's real private bus is `tests/desktop_bus.rs`. Every id and name is made
//! up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
#[path = "common/notices.rs"]
mod notices;
#[path = "common/parts.rs"]
mod parts;

use std::sync::{Arc, Mutex, PoisonError};

use common::{LAB_A, LAB_B, Rig, Written};
use futures_util::future::BoxFuture;
use gpui::{AppContext as _, Entity, WindowHandle};
use notices::{Asked, FakeServer, chat, message};
use teams_core::{AVAILABLE_REFRESH, Event, MessageChange, OwnStatus, RegistrationReport, Reporting, Source};
use teams_store::{Applied, ConversationKind, Member};
use teams_ui::ViewTrace;
use teams_ui::desktop::{Desktop, DesktopEvent, DesktopParts, WindowShown};
use teams_ui::notify::{GATHER, NoticeKind};
use teams_ui::settings::Settings;
use teams_ui::shell::Main;
use teams_ui::tray::{COUNT_DELAY, Tray, TrayEvent, TrayIcon};
use teams_ui::window::{self, KitRoot};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const CHANNEL: &str = "19:channel0001@thread.tacv2";
const NOTES: &str = "48:notes";

/// A tray item that records every total it was shown.
#[derive(Default)]
struct FakeIcon {
    shown: Mutex<Vec<u64>>,
}

impl FakeIcon {
    fn shown(&self) -> Vec<u64> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl TrayIcon for FakeIcon {
    fn set_unread(&self, unread: u64) -> BoxFuture<'static, ()> {
        self.shown.lock().unwrap_or_else(PoisonError::into_inner).push(unread);
        Box::pin(async {})
    }
}

/// a's store: the a–b chat (two unread), the trio (one), a team's channel (five) and a's notes (three).
fn seeded() -> Rig {
    let rig = Rig::start();
    rig.core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None),
        chat(TRIO, ConversationKind::Group, Some("Lab trio")),
        chat(CHANNEL, ConversationKind::Channel, Some("General")),
        chat(NOTES, ConversationKind::Other("notes".to_owned()), None),
    ]);
    rig.core.members.lock().unwrap().extend([
        (AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]),
        (
            TRIO.to_owned(),
            vec![
                Member::new(LAB_A, None),
                Member::new(LAB_B, None),
                Member::new(LAB_C, None),
            ],
        ),
    ]);
    rig.core.names.lock().unwrap().extend([
        (LAB_B.to_owned(), "Bee".to_owned()),
        (LAB_C.to_owned(), "Cee".to_owned()),
    ]);
    rig.core.unread.lock().unwrap().extend([
        (AB.to_owned(), 2),
        (TRIO.to_owned(), 1),
        (CHANNEL.to_owned(), 5),
        (NOTES.to_owned(), 3),
    ]);
    rig
}

fn count(rig: &Rig) {
    rig.cx.executor().advance_clock(COUNT_DELAY);
    rig.cx.run_until_parked();
}

/// The tray's unread chats with their counts, by id.
fn unread_chats(rig: &Rig, tray: &Entity<Tray>) -> Vec<(String, u64)> {
    let mut chats: Vec<(String, u64)> = rig.cx.read(|cx| {
        tray.read(cx)
            .unread_chats()
            .map(|(chat, count)| (chat.to_owned(), count))
            .collect()
    });
    chats.sort();
    chats
}

#[test]
fn the_tray_shows_the_unread_total_of_the_listed_chats_as_they_change() {
    let rig = seeded();
    let written = Written::default();
    let trace = Arc::new(ViewTrace::new(written.clone()));
    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
    let models = rig.models.clone();
    let tray = rig.cx.update(|cx| cx.new(|cx| Tray::new(core, models, trace, cx)));
    count(&rig);
    // a's unread chats, each once however many of its messages are unread (two in the a–b chat), not the channel nor
    // the notes
    assert_eq!(rig.cx.read(|cx| tray.read(cx).total()), Some(2));
    assert_eq!(unread_chats(&rig, &tray), [(AB.to_owned(), 2), (TRIO.to_owned(), 1)]);
    let icon = Arc::new(FakeIcon::default());
    let given = Arc::clone(&icon);
    rig.cx.update(|cx| tray.update(cx, |tray, cx| tray.set_icon(given, cx)));
    rig.cx.run_until_parked();
    assert_eq!(icon.shown(), [2]);
    assert!(rig.cx.read(|cx| tray.read(cx).has_icon()));

    // a's horizon in the a–b chat moved: only that chat is asked again
    rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    let asked_before = rig
        .core
        .asked()
        .iter()
        .filter(|asked| **asked == "unread_count")
        .count();
    rig.send(Event::ReadHorizon {
        conversation: AB.to_owned(),
    });
    count(&rig);
    let asked_after = rig
        .core
        .asked()
        .iter()
        .filter(|asked| **asked == "unread_count")
        .count();
    assert_eq!(asked_after - asked_before, 1);
    assert_eq!(icon.shown(), [2, 1]);
    // more unread in a chat already unread leaves the total as it was: nothing new shown
    rig.core.unread.lock().unwrap().insert(TRIO.to_owned(), 4);
    rig.send(Event::ConversationChanged {
        conversation: TRIO.to_owned(),
    });
    count(&rig);
    assert_eq!(icon.shown(), [2, 1]);
    assert_eq!(rig.cx.read(|cx| tray.read(cx).total()), Some(1));
    // each chat's own count: the read chat is gone, the other holds its four
    assert_eq!(unread_chats(&rig, &tray), [(TRIO.to_owned(), 4)]);
    let totals: Vec<serde_json::Value> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "tray" && line["change"] == "unread")
        .map(|line| line["unread"].clone())
        .collect();
    assert_eq!(totals, [serde_json::json!([2]), serde_json::json!([1])]);
    assert_eq!(rig.shutdowns(), 0);
}

/// The desktop over a seeded rig, with a fake server given to its notifier; what it asked of the app.
struct OnDesktop {
    rig: Rig,
    desktop: Entity<Desktop>,
    server: Arc<FakeServer>,
    asked: Arc<Mutex<Vec<DesktopEvent>>>,
    written: Written,
}

impl OnDesktop {
    fn start() -> Self {
        Self::start_on(seeded())
    }

    fn start_on(rig: Rig) -> Self {
        let written = Written::default();
        let shell = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::new(written.clone())));
        let server = Arc::new(FakeServer::default());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (given, recorded) = (Arc::clone(&server), Arc::clone(&asked));
        let desktop = rig.cx.update(|cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    DesktopParts {
                        app_id: "ngteams-test",
                        app_name: "Teams (test)".into(),
                        title: "Teams (test)".into(),
                        shell,
                        account_name: "Lab A".into(),
                        account_detail: "lab a".into(),
                        can_add: false,
                        ringer: None,
                    },
                    cx,
                )
            });
            desktop.update(cx, |desktop, cx| {
                desktop.notifier().update(cx, |notifier, _| notifier.set_server(given));
            });
            cx.subscribe(&desktop, move |_, event, _| {
                recorded.lock().unwrap().push(event.clone());
            })
            .detach();
            desktop
        });
        Self {
            rig,
            desktop,
            server,
            asked,
            written,
        }
    }

    fn open(&self) -> WindowHandle<KitRoot> {
        let window = self
            .rig
            .cx
            .update(|cx| self.desktop.update(cx, Desktop::open_window))
            .unwrap();
        self.rig.cx.run_until_parked();
        window
    }

    fn close(&self, window: WindowHandle<KitRoot>) {
        self.rig
            .cx
            .update(|cx| window.update(cx, |_, window, _| window.remove_window()))
            .unwrap();
        self.rig.cx.run_until_parked();
    }

    fn window(&self) -> Option<WindowHandle<KitRoot>> {
        self.rig.cx.read(|cx| self.desktop.read(cx).window())
    }

    /// What the window's shell shows.
    fn main(&self) -> Option<Main> {
        let window = self.window()?;
        self.rig.cx.update(|cx| {
            let root = window::content(window, cx)?;
            let shell = root.read(cx).shell()?.clone();
            Some(shell.read(cx).main().clone())
        })
    }

    fn deliver(&self, conversation: &str, id: &str, sender: &str) {
        self.rig.send(Event::Message {
            conversation: conversation.to_owned(),
            id: id.to_owned(),
            change: MessageChange::New,
            applied: Applied::Inserted,
            message: Some(Box::new(message(conversation, id, sender, "<p>hello</p>", 5_000))),
            source: Source::Delivery,
        });
        self.rig.cx.executor().advance_clock(GATHER);
        self.rig.cx.run_until_parked();
    }

    /// The tray item asking `event`.
    fn tray(&self, event: TrayEvent) {
        self.rig
            .cx
            .update(|cx| self.desktop.update(cx, |desktop, cx| desktop.tray_asked(event, cx)));
        self.rig.cx.run_until_parked();
    }

    fn traced(&self, view: &str, change: &str) -> usize {
        self.written
            .lines()
            .iter()
            .filter(|line| line["view"] == view && line["change"] == change)
            .count()
    }
}

#[test]
fn closing_the_window_quits_without_a_tray_item() {
    let d = OnDesktop::start();
    let window = d.open();
    assert_eq!(d.traced("window", "opened"), 1);
    d.close(window);
    assert_eq!(d.window(), None);
    let shown = d.rig.cx.read(|cx| d.desktop.read(cx).ui_state(cx));
    assert_eq!((shown.window, shown.main), (WindowShown::Closed, None));
    assert_eq!(*d.asked.lock().unwrap(), [DesktopEvent::Quit]);
    assert_eq!(d.traced("window", "closed"), 1);
}

#[test]
fn with_a_tray_item_closing_the_window_keeps_the_app_and_the_tray_shows_it_again() {
    let d = OnDesktop::start();
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.set_in_tray(true, cx)));
    let window = d.open();
    // what the app shows: the account, its shell's view, the window
    let ui = || d.rig.cx.read(|cx| d.desktop.read(cx).ui_state(cx));
    let open = ui();
    assert_eq!(open.account.as_deref(), Some(LAB_A));
    assert_eq!(
        (open.window, open.main, open.title, open.call_window),
        (WindowShown::Shown, Some(Main::Nothing), None, false)
    );
    d.close(window);
    assert_eq!(d.window(), None);
    assert_eq!((ui().window, ui().main), (WindowShown::Tray, None));
    assert_eq!(*d.asked.lock().unwrap(), []);
    d.tray(TrayEvent::Open);
    d.rig.cx.run_until_parked();
    let again = d.window().expect("the window again");
    assert_ne!(again.window_id(), window.window_id());
    assert_eq!(d.traced("window", "opened"), 2);
    assert_eq!(d.traced("tray", "open"), 1);
    // shown while open: the same window, brought forward
    d.tray(TrayEvent::Open);
    assert_eq!(d.window().map(|window| window.window_id()), Some(again.window_id()));
    assert_eq!(d.traced("window", "opened"), 2);
    d.tray(TrayEvent::Quit);
    assert_eq!(*d.asked.lock().unwrap(), [DesktopEvent::Quit]);
    assert_eq!(d.traced("tray", "quit"), 1);
}

#[test]
fn the_own_presence_outlives_the_window_is_shown_in_each_one_and_stops_on_quit() {
    let d = OnDesktop::start();
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.set_in_tray(true, cx)));
    let own = d.rig.cx.read(|cx| d.desktop.read(cx).own_presence().clone());
    let shell_s = |d: &OnDesktop| {
        let window = d.window()?;
        d.rig.cx.update(|cx| {
            let shell = window::content(window, cx)?.read(cx).shell()?.clone();
            shell.read(cx).own_presence().cloned()
        })
    };
    let window = d.open();
    assert_eq!(shell_s(&d).map(|shown| shown.entity_id()), Some(own.entity_id()));
    d.rig.send(Event::Registration(RegistrationReport::Registered));
    assert_eq!(*d.rig.core.own_statuses.lock().unwrap(), [OwnStatus::Available]);
    // the window closed, the app in the tray: still reported
    d.close(window);
    d.rig.cx.executor().advance_clock(AVAILABLE_REFRESH);
    d.rig.cx.run_until_parked();
    assert_eq!(
        *d.rig.core.own_statuses.lock().unwrap(),
        [OwnStatus::Available, OwnStatus::Available]
    );
    // opened again: the same one shown
    d.tray(TrayEvent::Open);
    assert_eq!(shell_s(&d).map(|shown| shown.entity_id()), Some(own.entity_id()));
    // the quit stops it before the app is asked, and sends nothing
    d.tray(TrayEvent::Quit);
    assert_eq!(*d.asked.lock().unwrap(), [DesktopEvent::Quit]);
    assert_eq!(d.rig.cx.read(|cx| own.read(cx).reporting(cx)), Reporting::Quit);
    d.rig.cx.executor().advance_clock(AVAILABLE_REFRESH * 2);
    d.rig.cx.run_until_parked();
    assert_eq!(d.rig.core.own_statuses.lock().unwrap().len(), 2);
}

#[test]
fn a_tray_item_gone_brings_the_window_back_and_closing_it_then_quits() {
    let d = OnDesktop::start();
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.set_in_tray(true, cx)));
    let window = d.open();
    d.close(window);
    assert_eq!(d.window(), None);
    // the watcher leaves (or the bus is lost): nothing would show the app any more
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.set_in_tray(false, cx)));
    d.rig.cx.run_until_parked();
    let back = d.window().expect("the window back");
    assert_eq!(d.traced("tray", "gone"), 1);
    assert_eq!(d.traced("window", "opened"), 2);
    assert_eq!(*d.asked.lock().unwrap(), []);
    // a new watcher has the item again: closing keeps the app
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.set_in_tray(true, cx)));
    assert_eq!(d.traced("tray", "registered"), 2);
    d.close(back);
    assert_eq!(*d.asked.lock().unwrap(), []);
    d.tray(TrayEvent::Open);
    let last = d.window().expect("the window shown from the tray");
    // gone while the window is open: it stays, and closing it quits
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.set_in_tray(false, cx)));
    assert_eq!(d.window().map(|window| window.window_id()), Some(last.window_id()));
    d.close(last);
    assert_eq!(*d.asked.lock().unwrap(), [DesktopEvent::Quit]);
}

#[test]
fn activating_a_notification_opens_its_chat_in_a_window_opened_again() {
    let d = OnDesktop::start();
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.set_in_tray(true, cx)));
    let window = d.open();
    assert_eq!(d.main(), Some(Main::Nothing));
    d.deliver(TRIO, "1", LAB_C);
    let asked = d.server.asked();
    let [Asked::Show { notice, id, .. }] = asked.as_slice() else {
        panic!("not one notification shown: {asked:?}");
    };
    let id = *id;
    assert_eq!(
        (notice.kind, notice.summary.as_str()),
        (NoticeKind::Message, "Cee in Lab trio")
    );
    let notifier = d.rig.cx.read(|cx| d.desktop.read(cx).notifier().clone());
    d.rig
        .cx
        .update(|cx| notifier.update(cx, |notifier, cx| notifier.activated(id, cx)));
    d.rig.cx.run_until_parked();
    assert_eq!(d.main(), Some(Main::Chat(TRIO.to_owned())));
    assert_eq!(d.server.closed(), [id]);

    // closed, then activated: the window opens again with the chat
    d.deliver(AB, "2", LAB_B);
    let id = d.rig.cx.read(|cx| notifier.read(cx).shown())[0].0;
    d.close(window);
    assert_eq!(d.window(), None);
    d.rig
        .cx
        .update(|cx| notifier.update(cx, |notifier, cx| notifier.activated(id, cx)));
    d.rig.cx.run_until_parked();
    assert_eq!(d.main(), Some(Main::Chat(AB.to_owned())));
    assert_eq!(d.server.shown().len(), 2);
}

#[test]
fn activating_an_app_item_s_notification_shows_the_activity_pane_at_the_item() {
    let d = OnDesktop::start();
    d.open();
    let activity = serde_json::json!({
        "activityType": "msGraph", "activitySubtype": "customNotification", "activityId": 7,
        "sourceAppDisplayName": "Made-up flows", "messagePreview": "a made-up reminder",
        "activityContext": {"teamsAppId": "00000000-0000-4000-8000-00000000a0aa"},
    });
    let mut item = message("48:notifications", "7", LAB_A, "", 5_000);
    item.message_type = "Text".to_owned();
    item.properties = Some(serde_json::json!({"activity": activity, "isread": false}).to_string());
    *d.rig.core.activity.lock().unwrap() =
        teams_core::activity::ActivityFeed::new(std::slice::from_ref(&item), None, None);
    *d.rig.core.app_settings.lock().unwrap() = Some(teams_proto::UserGraphSettings::default());
    d.rig.send(Event::Message {
        conversation: item.conversation_id.clone(),
        id: item.id.clone(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(item)),
        source: Source::Delivery,
    });
    d.rig.cx.run_until_parked();
    let asked = d.server.asked();
    let [Asked::Show { notice, id, .. }] = asked.as_slice() else {
        panic!("not one notification shown: {asked:?}");
    };
    assert_eq!(
        (notice.kind, notice.summary.as_str(), notice.body.as_str()),
        (NoticeKind::Activity, "Made-up flows", "a made-up reminder")
    );
    let notifier = d.rig.cx.read(|cx| d.desktop.read(cx).notifier().clone());
    let id = *id;
    d.rig
        .cx
        .update(|cx| notifier.update(cx, |notifier, cx| notifier.activated(id, cx)));
    d.rig.cx.run_until_parked();
    let window = d.window().unwrap();
    let (place, shown) = d.rig.cx.update(|cx| {
        let root = window::content(window, cx).unwrap();
        let shell = root.read(cx).shell().unwrap().read(cx);
        (
            format!("{:?}", shell.list_place()),
            shell.activity_view().read(cx).shown(),
        )
    });
    assert_eq!((place.as_str(), shown), ("Activity", true));
    let actions = d.rig.core.actions.lock().unwrap().clone();
    assert!(
        actions.iter().any(|action| matches!(
            action,
            teams_core::Action::ActivityRead { item, read: true } if item == "7"
        )),
        "the item opened as a click in the pane opens it: {actions:?}"
    );
}

#[test]
fn the_chat_shown_in_the_focused_window_never_notifies() {
    let d = OnDesktop::start();
    let window = d.open();
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.open_chat(AB.to_owned(), cx)));
    d.rig.cx.run_until_parked();
    d.rig
        .cx
        .update(|cx| window.update(cx, |_, window, _| window.activate_window()))
        .unwrap();
    d.rig.cx.run_until_parked();
    d.deliver(AB, "1", LAB_B);
    assert_eq!(d.server.asked(), []);
    // another chat notifies
    d.deliver(TRIO, "2", LAB_C);
    assert_eq!(d.server.shown().len(), 1);
    // the window without the focus: its chat notifies too
    let mut visual = gpui::VisualTestContext::from_window(window.into(), &d.rig.cx);
    visual.deactivate_window();
    d.deliver(AB, "3", LAB_B);
    let shown = d.server.shown();
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[1].conversation, AB);
}

#[test]
fn a_channel_opened_from_the_desktop_reuses_the_window_with_its_box_posting_to_it() {
    let d = OnDesktop::start();
    d.rig.cx.update(|cx| {
        d.desktop
            .update(cx, |desktop, cx| desktop.open_channel(CHANNEL.to_owned(), cx));
    });
    d.rig.cx.run_until_parked();
    assert_eq!(d.main(), Some(Main::Channel(CHANNEL.to_owned())));
    let first = d.window().unwrap();
    let again = d.open();
    assert_eq!(first, again);
    assert!(!d.rig.cx.read(|cx| d.desktop.read(cx).in_tray()));
    let mut visual = gpui::VisualTestContext::from_window(first.into(), &d.rig.cx);
    visual.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(visual.debug_bounds("channel-view").is_some());
    assert!(visual.debug_bounds("compose").is_some());
    d.rig.cx.update(|cx| {
        d.desktop.update(cx, |desktop, cx| {
            desktop.open_channel_of(LAB_A, CHANNEL.to_owned(), cx);
            desktop.set_in_tray(false, cx);
        });
    });
    assert_eq!(d.window(), Some(first));
    assert!(d.rig.core.actions.lock().unwrap().is_empty());
}

#[test]
fn an_account_added_to_an_open_window_keeps_the_active_shell_and_inherits_the_notification_server() {
    use teams_ui::accounts::AccountParts;
    use teams_ui::models::Models;

    let d = OnDesktop::start();
    let window = d.open();
    let icon = Arc::new(FakeIcon::default());
    d.rig.cx.update(|cx| {
        d.desktop.update(cx, |desktop, cx| {
            desktop.use_server(d.server.clone(), cx);
            desktop.tray().update(cx, |tray, cx| tray.set_icon(icon.clone(), cx));
        });
    });
    let core = Arc::new(common::FakeCore {
        me: LAB_B.to_owned().into(),
        ..common::FakeCore::default()
    });
    let models = d.rig.cx.update(Models::new);
    let second = AccountParts {
        core,
        models,
        name: "Synthetic second account".into(),
        detail: "second@example.test".into(),
    };
    d.rig.cx.update(|cx| {
        d.desktop.update(cx, |desktop, cx| {
            desktop.add_account(second.clone(), cx);
            desktop.add_account(second, cx);
        });
    });
    d.rig.cx.run_until_parked();
    let active = d.rig.cx.update(|cx| {
        let root = window::content(window, cx).unwrap();
        root.read(cx).shell_in(cx).unwrap().read(cx).account().to_owned()
    });
    assert_eq!(active, LAB_A);
    assert_eq!(
        d.rig.cx.read(|cx| d.desktop.read(cx).accounts().read(cx).tiles().len()),
        2
    );
    assert!(d.rig.cx.read(|cx| d.desktop.read(cx).notifier_of(LAB_B).is_some()));
    // each account's own presence is its own, the first one's the one `own_presence` gives
    d.rig.cx.read(|cx| {
        let desktop = d.desktop.read(cx);
        let (a, b) = (
            desktop.own_presence_of(LAB_A).unwrap(),
            desktop.own_presence_of(LAB_B).unwrap(),
        );
        assert_eq!(a.entity_id(), desktop.own_presence().entity_id());
        assert_ne!(a.entity_id(), b.entity_id());
    });
    d.rig.cx.update(|cx| {
        d.desktop.update(cx, |desktop, cx| {
            desktop.remove_account("missing", cx);
            desktop.remove_account(LAB_A, cx);
        });
    });
    d.rig.cx.run_until_parked();
    assert_eq!(
        d.rig.cx.read(|cx| d.desktop.read(cx).accounts().read(cx).tiles().len()),
        1
    );
    assert!(d.rig.cx.read(|cx| d.desktop.read(cx).tray().read(cx).has_icon()));
    assert!(d.rig.cx.read(|cx| d.desktop.read(cx).notifier_of(LAB_A).is_none()));
    assert!(d.rig.cx.read(|cx| d.desktop.read(cx).own_presence_of(LAB_A).is_none()));
    assert_eq!(d.window(), Some(window));
    assert!(d.asked.lock().unwrap().is_empty());
}

#[test]
fn a_refused_bus_connection_reports_why_and_leaves_the_open_window_usable() {
    use futures_util::FutureExt as _;
    let d = OnDesktop::start();
    let window = d.open();
    let task = d.rig.cx.update(|cx| {
        d.desktop.update(cx, |desktop, cx| {
            desktop.connect(Err(teams_ui::bus::BusError::NotGiven), cx)
        })
    });
    d.rig.cx.run_until_parked();
    let problems = task.now_or_never().unwrap();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("no session bus was given"));
    assert_eq!(d.window(), Some(window));
    d.tray(TrayEvent::Open);
    assert!(d.asked.lock().unwrap().is_empty());
}

/// What holds an update's restart up (`teams_ui::update`): a message typed and not sent in any account's box, then a
/// call ringing or up.
#[test]
fn an_updates_restart_waits_for_an_unsent_message_and_for_a_call() {
    use teams_ui::editor::doc::{Block, Doc};
    use teams_ui::update::Waiting;
    let rig = Rig::start_with(|cx| teams_ui::Models::with_calls(None, cx));
    rig.core
        .conversations
        .lock()
        .unwrap()
        .push(chat(AB, ConversationKind::OneToOne, None));
    let d = OnDesktop::start_on(rig);
    let waiting = |d: &OnDesktop| d.rig.cx.read(|cx| d.desktop.read(cx).update_waiting(cx));
    assert_eq!(waiting(&d), None, "no window, nothing typed");
    let window = d.open();
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.open_chat(AB.to_owned(), cx)));
    d.rig.cx.run_until_parked();
    assert_eq!(waiting(&d), None);
    let draft = |text: Option<&str>| {
        d.rig.cx.update(|cx| {
            let root = window::content(window, cx).unwrap();
            let shell = root.read(cx).shell().unwrap().clone();
            let compose = shell.read(cx).chat().unwrap().read(cx).compose().clone();
            let editor = compose.read(cx).editor().clone();
            let doc = text.map_or_else(Doc::default, |text| Doc::new(vec![Block::paragraph(text)]));
            editor.update(cx, |editor, cx| editor.set_doc(doc, cx));
        });
    };
    draft(Some("Lunch at noon?"));
    assert_eq!(waiting(&d), Some(Waiting::Unsent));
    // kept while another place is shown
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.show_calendar_of(LAB_A, cx)));
    d.rig.cx.run_until_parked();
    assert_eq!(waiting(&d), Some(Waiting::Unsent));
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.open_chat(AB.to_owned(), cx)));
    d.rig.cx.run_until_parked();
    draft(None);
    assert_eq!(waiting(&d), None);
    // a call ringing holds it up too, before anything typed
    d.rig.send(Event::CallingRegistration(RegistrationReport::Registered));
    d.rig.send(Event::IncomingCall(teams_core::IncomingCall {
        call: teams_core::CallId::for_tests(1),
        caller: teams_proto::Mri::new(LAB_B),
        caller_name: Some("Bee".to_owned()),
        group_chat: None,
    }));
    d.rig.cx.run_until_parked();
    assert_eq!(waiting(&d), Some(Waiting::Call));
    // the window closed: its boxes are gone with it, the call still holds
    d.close(window);
    assert_eq!(waiting(&d), Some(Waiting::Call));
}
