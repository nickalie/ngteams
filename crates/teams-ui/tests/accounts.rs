//! Several accounts on one desktop, over two fake engines (`common`): the window keeps a shell per account and shows
//! the one the rail's accounts show; every account notifies, its notifications never naming it, and activating one
//! shows its account first; the tray's total sums the accounts' unread counts; a call on one account
//! keeps the others from entering one, and has the app's one call window while the window shows another account
//! with that call's in-call strip above it (the app staying while the call window is open); an account removed takes its shell and its count with it. Every id and name is
//! made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
#[path = "common/notices.rs"]
mod notices;
#[path = "common/parts.rs"]
mod parts;

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use common::{FakeCore, FakeEvents, LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, VisualTestContext, WindowHandle};
use notices::{Asked, FakeServer, chat, message};
use teams_core::{CallId, CallStatus, End, Event, IncomingCall, MessageChange, Source};
use teams_proto::Mri;
use teams_store::{Applied, ConversationKind, Member};
use teams_ui::accounts::AccountParts;
use teams_ui::calls::Ask;
use teams_ui::desktop::{Desktop, DesktopEvent, DesktopParts, WindowShown};
use teams_ui::notify::{GATHER, NotificationServer};
use teams_ui::settings::Settings;
use teams_ui::settings_view::Page;
use teams_ui::shell::Main;
use teams_ui::tray::COUNT_DELAY;
use teams_ui::window::{self, KitRoot};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
/// a's chat with c, and b's.
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
const BC: &str = "19:00000000-0000-4000-8000-00000000000b_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";

/// The second account: its fake engine, its event sender and its models, on the rig's app.
struct Second {
    core: Arc<FakeCore>,
    events: mpsc::UnboundedSender<Event>,
    models: Models,
    _bridge: Entity<Bridge>,
}

/// a's desktop with b added, a fake notification server given to the desktop's accounts.
struct TwoAccounts {
    rig: Rig,
    second: Second,
    desktop: Entity<Desktop>,
    server: Arc<FakeServer>,
    asked: Arc<Mutex<Vec<DesktopEvent>>>,
    written: Written,
}

fn store(core: &FakeCore, id: &str, me: &str, unread: u64) {
    core.conversations
        .lock()
        .unwrap()
        .push(chat(id, ConversationKind::OneToOne, None));
    core.members
        .lock()
        .unwrap()
        .insert(id.to_owned(), vec![Member::new(me, None), Member::new(LAB_C, None)]);
    core.names.lock().unwrap().insert(LAB_C.to_owned(), "Cee".to_owned());
    core.unread.lock().unwrap().insert(id.to_owned(), unread);
}

impl TwoAccounts {
    fn start() -> Self {
        let rig = Rig::start_with(|cx| Models::with_calls(None, cx));
        store(&rig.core, AC, LAB_A, 2);
        let written = Written::default();
        let shell = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::new(written.clone())));
        let (events, received) = mpsc::unbounded_channel();
        let core = Arc::new(FakeCore {
            me: LAB_B.to_owned().into(),
            ..FakeCore::default()
        });
        store(&core, BC, LAB_B, 3);
        let models = rig.cx.update(|cx| Models::with_calls(None, cx));
        let source = FakeEvents {
            events: received,
            shutdowns: Arc::new(AtomicUsize::new(0)),
        };
        let (shared, drained): (Arc<dyn Core>, Models) = (core.clone(), models.clone());
        let bridge = rig
            .cx
            .update(|cx| cx.new(|cx| Bridge::start(source, shared, drained, cx)));
        let server = Arc::new(FakeServer::default());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&asked);
        let b = AccountParts {
            core: rig.cx.read(|cx| bridge.read(cx).core()),
            models: models.clone(),
            name: "Bee Work".into(),
            detail: "bee@example.test".into(),
        };
        let given: Arc<dyn NotificationServer> = server.clone();
        let desktop = rig.cx.update(|cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    DesktopParts {
                        app_id: "ngteams-test",
                        app_name: "Teams (test)".into(),
                        title: "Teams (test)".into(),
                        shell,
                        account_name: "Aye Work".into(),
                        account_detail: "aye@example.test".into(),
                        can_add: true,
                        ringer: None,
                    },
                    cx,
                )
            });
            desktop.update(cx, |desktop, cx| {
                desktop.add_account(b, cx);
                desktop.use_server(given, cx);
            });
            cx.subscribe(&desktop, move |_, event, _| {
                recorded.lock().unwrap().push(event.clone());
            })
            .detach();
            desktop
        });
        rig.cx.run_until_parked();
        Self {
            rig,
            second: Second {
                core,
                events,
                models,
                _bridge: bridge,
            },
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

    /// The account the window shows and what its shell shows.
    fn shown(&self, window: WindowHandle<KitRoot>) -> (String, Main) {
        self.rig.cx.update(|cx| {
            let root = window::content(window, cx).unwrap();
            let shell = root.read(cx).shell_in(cx).unwrap().clone();
            let shell = shell.read(cx);
            (shell.account().to_owned(), shell.main().clone())
        })
    }

    fn switch(&self, key: &str) {
        self.rig
            .cx
            .update(|cx| self.desktop.update(cx, |desktop, cx| desktop.switch(key, cx)));
        self.rig.cx.run_until_parked();
    }

    /// A message from c delivered to b.
    fn deliver_to_b(&self, id: &str) {
        let event = Event::Message {
            conversation: BC.to_owned(),
            id: id.to_owned(),
            change: MessageChange::New,
            applied: Applied::Inserted,
            message: Some(Box::new(message(BC, id, LAB_C, "<p>hello</p>", 5_000))),
            source: Source::Delivery,
        };
        self.second.events.send(event).unwrap();
        self.rig.cx.run_until_parked();
        self.rig.cx.executor().advance_clock(GATHER);
        self.rig.cx.run_until_parked();
    }

    fn traced(&self, view: &str, change: &str) -> Vec<serde_json::Value> {
        self.written
            .lines()
            .into_iter()
            .filter(|line| line["view"] == view && line["change"] == change)
            .collect()
    }
}

#[test]
fn the_window_keeps_a_shell_per_account_and_shows_the_one_switched_to() {
    let d = TwoAccounts::start();
    let window = d.open();
    let tiles = d.rig.cx.read(|cx| {
        let accounts = d.desktop.read(cx).accounts().read(cx);
        accounts
            .tiles()
            .iter()
            .map(|tile| (tile.key.clone(), tile.name.to_string()))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        tiles,
        [
            (LAB_A.to_owned(), "Aye Work".to_owned()),
            (LAB_B.to_owned(), "Bee Work".to_owned())
        ]
    );
    let shells = d
        .rig
        .cx
        .update(|cx| window::content(window, cx).unwrap().read(cx).shells().len());
    assert_eq!(shells, 2);
    assert_eq!(d.shown(window).0, LAB_A);
    d.switch(LAB_B);
    assert_eq!(d.shown(window).0, LAB_B);
    let switched = d.traced("account", "switched");
    assert_eq!(switched.len(), 1);
    assert_eq!(switched[0]["people"], serde_json::json!([LAB_B]));
    // a chat opened in b's shell stays there when a is shown and b again
    d.rig.cx.update(|cx| {
        d.desktop
            .update(cx, |desktop, cx| desktop.open_chat_of(LAB_B, BC.to_owned(), cx));
    });
    d.rig.cx.run_until_parked();
    d.switch(LAB_A);
    assert_eq!(d.shown(window), (LAB_A.to_owned(), Main::Nothing));
    d.switch(LAB_B);
    assert_eq!(d.shown(window), (LAB_B.to_owned(), Main::Chat(BC.to_owned())));
    // Ctrl+Alt+Down and Up go round the accounts
    let cycle = |forward: bool| {
        d.rig.cx.update(|cx| {
            let shell = window::content(window, cx)
                .unwrap()
                .read(cx)
                .shell_in(cx)
                .unwrap()
                .clone();
            shell.update(cx, |shell, cx| shell.cycle_account(forward, cx));
        });
        d.rig.cx.run_until_parked();
    };
    cycle(true);
    assert_eq!(d.shown(window).0, LAB_A);
    cycle(false);
    assert_eq!(d.shown(window).0, LAB_B);
}

#[test]
fn every_account_notifies_naming_no_account_and_opening_one_shows_its_account() {
    let d = TwoAccounts::start();
    let window = d.open();
    assert_eq!(d.shown(window).0, LAB_A);
    d.deliver_to_b("1");
    let asked = d.server.asked();
    let [Asked::Show { notice, id, .. }] = asked.as_slice() else {
        panic!("not one notification shown: {asked:?}");
    };
    assert_eq!(notice.summary, "Cee");
    assert_eq!(notice.conversation, BC);
    let id = *id;
    let notifier = d
        .rig
        .cx
        .read(|cx| d.desktop.read(cx).notifier_of(LAB_B).unwrap().clone());
    d.rig
        .cx
        .update(|cx| notifier.update(cx, |notifier, cx| notifier.activated(id, cx)));
    d.rig.cx.run_until_parked();
    assert_eq!(d.shown(window), (LAB_B.to_owned(), Main::Chat(BC.to_owned())));
}

#[test]
fn the_tray_sums_the_accounts_and_an_account_removed_leaves_the_sum_and_the_window() {
    let d = TwoAccounts::start();
    let window = d.open();
    d.rig.cx.executor().advance_clock(COUNT_DELAY);
    d.rig.cx.run_until_parked();
    let total = |d: &TwoAccounts| d.rig.cx.read(|cx| d.desktop.read(cx).tray().read(cx).combined(cx));
    // a's one unread chat (two messages) and b's (three): two chats
    assert_eq!(total(&d), Some(2));
    let unread_b = d.rig.cx.read(|cx| {
        let accounts = d.desktop.read(cx).accounts().read(cx);
        accounts.tile(LAB_B).unwrap().unread(cx)
    });
    assert_eq!(unread_b, 1);

    // the user asks to remove b from its account menu: the app is asked, and does it
    d.switch(LAB_B);
    d.rig.cx.update(|cx| {
        let accounts = d.desktop.read(cx).accounts().clone();
        accounts.update(cx, |accounts, cx| accounts.ask_remove(LAB_B, cx));
    });
    assert_eq!(
        *d.asked.lock().unwrap(),
        [DesktopEvent::RemoveAccount(LAB_B.to_owned())]
    );
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.remove_account(LAB_B, cx)));
    d.rig.cx.run_until_parked();
    assert_eq!(total(&d), Some(1));
    assert_eq!(d.shown(window).0, LAB_A);
    let shells = d
        .rig
        .cx
        .update(|cx| window::content(window, cx).unwrap().read(cx).shells().len());
    assert_eq!(shells, 1);
    // the last account isn't removed
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.remove_account(LAB_A, cx)));
    let left = d.rig.cx.read(|cx| d.desktop.read(cx).accounts().read(cx).tiles().len());
    assert_eq!(left, 1);
    assert!(d.second.core.asked().contains(&"conversations"));
}

/// What the control socket opens is opened on the account it names, switched to first: the calendar, the settings
/// (their first page or one named), a search's results; `ui_state` tells the account, the view and the window as they
/// stand, and each account's own presence and unread chats are its own.
#[test]
fn the_views_opened_for_an_account_show_it_first_and_the_ui_state_tells_them() {
    let d = TwoAccounts::start();
    let window = d.open();
    let state = |d: &TwoAccounts| d.rig.cx.read(|cx| d.desktop.read(cx).ui_state(cx));
    let first = state(&d);
    assert_eq!(first.account.as_deref(), Some(LAB_A));
    assert_eq!(first.window, WindowShown::Shown);
    assert!(!first.call_window);

    let on = |d: &TwoAccounts, open: &dyn Fn(&mut Desktop, &mut gpui::Context<Desktop>)| {
        d.rig
            .cx
            .update(|cx| d.desktop.update(cx, |desktop, cx| open(desktop, cx)));
        d.rig.cx.run_until_parked();
    };
    on(&d, &|desktop, cx| desktop.show_calendar_of(LAB_B, cx));
    assert_eq!(d.shown(window), (LAB_B.to_owned(), Main::Calendar));
    let calendar = state(&d);
    assert_eq!(calendar.account.as_deref(), Some(LAB_B));
    assert_eq!(calendar.main, Some(Main::Calendar));
    assert_eq!(calendar.title, None);

    on(&d, &|desktop, cx| desktop.open_settings_of(LAB_A, None, cx));
    assert_eq!(d.shown(window), (LAB_A.to_owned(), Main::Settings));
    on(&d, &|desktop, cx| {
        desktop.open_settings_of(LAB_B, Some(Page::Calls), cx);
    });
    assert_eq!(d.shown(window), (LAB_B.to_owned(), Main::Settings));
    on(&d, &|desktop, cx| desktop.search_of(LAB_A, "hello", cx));
    assert_eq!(d.shown(window), (LAB_A.to_owned(), Main::Search));

    d.rig.cx.read(|cx| {
        let desktop = d.desktop.read(cx);
        assert!(desktop.own_presence_of(LAB_A).is_some());
        assert!(desktop.own_presence_of(LAB_B).is_some());
        assert!(desktop.own_presence_of(LAB_C).is_none(), "no account is c");
    });
    d.rig.cx.executor().advance_clock(COUNT_DELAY);
    d.rig.cx.run_until_parked();
    let unread = |key: &str| {
        d.rig.cx.read(|cx| {
            let accounts = d.desktop.read(cx).accounts().read(cx);
            let tray = accounts.tile(key).unwrap().tray.read(cx);
            tray.unread_chats()
                .map(|(chat, count)| (chat.to_owned(), count))
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(unread(LAB_A), [(AC.to_owned(), 2)]);
    assert_eq!(unread(LAB_B), [(BC.to_owned(), 3)]);
}

#[test]
fn the_rail_s_add_asks_the_app() {
    let d = TwoAccounts::start();
    d.rig.cx.update(|cx| {
        let accounts = d.desktop.read(cx).accounts().clone();
        accounts.update(cx, teams_ui::Accounts::ask_add);
    });
    assert_eq!(*d.asked.lock().unwrap(), [DesktopEvent::AddAccount]);
    assert_eq!(d.traced("account", "add-asked").len(), 1);
}

#[test]
fn a_call_on_one_account_keeps_the_others_from_entering_one() {
    let d = TwoAccounts::start();
    let busy = |d: &TwoAccounts, key: &str| {
        d.rig
            .cx
            .read(|cx| d.desktop.read(cx).accounts().read(cx).busy_elsewhere(key, cx))
    };
    assert!(!busy(&d, LAB_A));
    // an invitation rings for b: a offers no call, and an ask to enter one is refused here
    let invitation = IncomingCall {
        call: CallId::for_tests(1),
        caller: Mri::new(LAB_C),
        caller_name: Some("Cee".to_owned()),
        group_chat: None,
    };
    d.second.events.send(Event::IncomingCall(invitation)).unwrap();
    d.rig.cx.run_until_parked();
    assert!(d.rig.cx.read(|cx| d.second.models.calls.read(cx).ringing().is_some()));
    assert!(busy(&d, LAB_A));
    assert!(!busy(&d, LAB_B));
    let window = d.open();
    d.rig.cx.update(|cx| {
        let shell = window::content(window, cx)
            .unwrap()
            .read(cx)
            .shell_in(cx)
            .unwrap()
            .clone();
        assert_eq!(shell.read(cx).account(), LAB_A);
        shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx));
    });
    d.rig.cx.run_until_parked();
    assert!(!d.rig.core.asked().contains(&"call"));
    let refused = d.rig.cx.read(|cx| d.rig.models.calls.read(cx).refused().cloned());
    assert_eq!(refused.map(|refused| refused.why), Some("another-account"));
}

#[test]
fn another_account_s_call_has_the_call_window_and_its_strip_and_keeps_the_app_while_it_is_open() {
    let d = TwoAccounts::start();
    let window = d.open();
    let call = CallId::for_tests(2);
    for status in [CallStatus::Placing, CallStatus::Connected] {
        d.second.events.send(Event::Call { call, status }).unwrap();
    }
    d.rig.cx.run_until_parked();
    let showing = d.rig.cx.read(|cx| {
        let windows = d.desktop.read(cx).call_windows().read(cx);
        windows.showing().map(|(key, call)| (key.to_owned(), call))
    });
    assert_eq!(showing, Some((LAB_B.to_owned(), call)), "one window, b's call's");
    // the main window shows a, with b's in-call strip above a's panes: the way back to b's call
    assert_eq!(d.shown(window).0, LAB_A);
    let cx = Box::leak(Box::new(VisualTestContext::from_window(window.into(), &d.rig.cx)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(cx.debug_bounds("in-call").is_some() && cx.debug_bounds("call-return").is_some());

    // the main window closed without a tray item: the call's window keeps the app
    d.rig.cx.update(|cx| {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    });
    d.rig.cx.run_until_parked();
    assert!(!d.asked.lock().unwrap().contains(&DesktopEvent::Quit));
    // the call ends: its window closes, and with no window left the app quits
    let ended = Event::Call {
        call,
        status: CallStatus::Ended(End::HungUp),
    };
    d.second.events.send(ended).unwrap();
    d.rig.cx.run_until_parked();
    assert!(
        d.rig
            .cx
            .read(|cx| d.desktop.read(cx).call_windows().read(cx).window().is_none())
    );
    assert!(d.asked.lock().unwrap().contains(&DesktopEvent::Quit));
}

#[test]
fn a_lone_account_s_notifications_name_no_account() {
    let rig = Rig::start();
    store(&rig.core, AC, LAB_A, 1);
    let shell = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let server = Arc::new(FakeServer::default());
    let given: Arc<dyn NotificationServer> = server.clone();
    let desktop = rig.cx.update(|cx| {
        let desktop = cx.new(|cx| {
            Desktop::new(
                DesktopParts {
                    app_id: "ngteams-test",
                    app_name: "Teams (test)".into(),
                    title: "Teams (test)".into(),
                    shell,
                    account_name: "Aye Work".into(),
                    account_detail: "aye@example.test".into(),
                    can_add: true,
                    ringer: None,
                },
                cx,
            )
        });
        desktop.update(cx, |desktop, cx| desktop.use_server(given, cx));
        desktop
    });
    rig.send(Event::Message {
        conversation: AC.to_owned(),
        id: "1".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message(AC, "1", LAB_C, "<p>hello</p>", 5_000))),
        source: Source::Delivery,
    });
    rig.cx.executor().advance_clock(GATHER);
    rig.cx.run_until_parked();
    let shown = server.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].summary, "Cee");
    assert!(server.closed().is_empty());
    assert_eq!(rig.cx.read(|cx| desktop.read(cx).accounts().read(cx).tiles().len()), 1);
    assert_eq!(rig.shutdowns(), 0);
}

#[test]
fn a_signed_out_first_account_removed_leaves_the_other_shown() {
    let d = TwoAccounts::start();
    let window = d.open();
    // a signed out: its shell says so, the rail stays
    d.rig
        .cx
        .update(|cx| d.rig.models.signed_out("sign in again".into(), cx));
    d.rig.cx.run_until_parked();
    d.rig
        .cx
        .update(|cx| d.desktop.update(cx, |desktop, cx| desktop.remove_account(LAB_A, cx)));
    d.rig.cx.run_until_parked();
    let (account, shown) = d.rig.cx.update(|cx| {
        let root = window::content(window, cx).unwrap();
        let root = root.read(cx);
        (root.shell_in(cx).unwrap().read(cx).account().to_owned(), root.shown())
    });
    assert_eq!(account, LAB_B);
    assert_ne!(shown, teams_ui::view::Shown::SignedOut);
}

#[test]
fn switching_accounts_stops_the_hidden_accounts_automatic_activity_writes() {
    use teams_core::activity::{ActivityFeed, FEED};
    let d = TwoAccounts::start();
    let window = d.open();
    let shell = d
        .rig
        .cx
        .update(|cx| window::content(window, cx).unwrap().read(cx).shell().unwrap().clone());
    let pane = d.rig.cx.update(|cx| shell.read(cx).activity_view().clone());
    d.rig.cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                window.activate_window();
                shell.update(cx, |shell, cx| {
                    shell.open_chat(AC.to_owned(), window, cx);
                    shell.show_activity(window, cx);
                });
            })
            .unwrap();
    });
    d.rig.cx.run_until_parked();
    d.switch(LAB_B);
    d.rig.core.actions.lock().unwrap().clear();
    let mut activity = message(FEED, "4000", LAB_C, "", 4000);
    activity.properties = Some(
        serde_json::json!({"activity": {
        "activityType": "mentionInChat", "activitySubtype": "person", "activityId": 4000,
        "sourceThreadId": AC, "sourceMessageId": 4000, "sourceUserId": LAB_C
    }, "isread": false})
        .to_string(),
    );
    *d.rig.core.activity.lock().unwrap() = ActivityFeed::new(&[activity], None, None);
    d.rig
        .cx
        .update(|cx| pane.update(cx, teams_ui::activity_view::ActivityView::reload));
    d.rig.cx.run_until_parked();
    assert!(
        d.rig.core.actions.lock().unwrap().is_empty(),
        "the hidden account marks nothing"
    );
    d.switch(LAB_A);
    let mut visual = VisualTestContext::from_window(window.into(), &d.rig.cx);
    visual.update(|window, cx| {
        let _ = window.draw(cx);
    });
    d.rig.cx.run_until_parked();
    let actions = d.rig.core.actions.lock().unwrap();
    assert!(actions.contains(&teams_core::Action::ActivitySeen));
    assert!(actions.contains(&teams_core::Action::ActivityRead {
        item: "4000".to_owned(),
        read: true
    }));
}
