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
use teams_auth::admin::{AdminUser, UserChanges, UserPage};
use teams_auth::secrets::Secret;
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
use teams_ui::user_admin::{AdminProblem, NewAccount, UserAdmin};
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

/// What a [`TwoAccounts`] starts on: its settings file (`None`: in memory, so no picture can be chosen) and the
/// profile picture both accounts have.
#[derive(Default)]
struct Start<'a> {
    settings: Option<&'a std::path::Path>,
    profile: Option<teams_store::StoredPicture>,
}

/// `account`'s profile, with `picture`.
fn profile(account: &str, picture: Option<&teams_store::StoredPicture>) -> teams_core::Person {
    teams_core::Person {
        mri: account.to_owned(),
        name: None,
        from: None,
        picture: picture.cloned(),
        looking: false,
    }
}

/// The settings kept in the file at `path`.
fn on_file(rig: &Rig, path: &std::path::Path) -> Entity<teams_ui::settings::SettingsModel> {
    let file = teams_ui::settings::SettingsFile::new(path);
    rig.cx
        .update(|cx| cx.new(|_| teams_ui::settings::SettingsModel::load(file)))
}

/// Each fake engine's account with the profile `picture`.
fn give_profiles(fakes: [(&Arc<FakeCore>, &str); 2], picture: Option<&teams_store::StoredPicture>) {
    for (fake, account) in fakes {
        fake.people
            .lock()
            .unwrap()
            .insert(account.to_owned(), profile(account, picture));
    }
}

impl TwoAccounts {
    fn start() -> Self {
        Self::start_on(&Start::default())
    }

    fn start_on(start: &Start<'_>) -> Self {
        let rig = Rig::start_with(|cx| Models::with_calls(None, cx));
        store(&rig.core, AC, LAB_A, 2);
        let written = Written::default();
        let mut shell = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::new(written.clone())));
        if let Some(path) = start.settings {
            shell.settings = on_file(&rig, path);
        }
        let (events, received) = mpsc::unbounded_channel();
        let core = Arc::new(FakeCore {
            me: LAB_B.to_owned().into(),
            ..FakeCore::default()
        });
        store(&core, BC, LAB_B, 3);
        give_profiles([(&rig.core, LAB_A), (&core, LAB_B)], start.profile.as_ref());
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
            // b is on an own server, a on Microsoft 365
            server: Some("chat.example.test:8443".into()),
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
                        account_server: None,
                        can_add: true,
                        ringer: None,
                        pictures_dir: None,
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
        meeting: None,
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
                    account_server: None,
                    can_add: true,
                    ringer: None,
                    pictures_dir: None,
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

#[test]
fn an_activity_open_waiting_for_the_feed_is_dropped_when_its_account_is_switched_away() {
    use teams_core::activity::{ActivityFeed, FEED};
    let d = TwoAccounts::start();
    let window = d.open();
    let shell = d
        .rig
        .cx
        .update(|cx| window::content(window, cx).unwrap().read(cx).shell().unwrap().clone());
    let pane = d.rig.cx.update(|cx| shell.read(cx).activity_view().clone());
    let mut activity = message(FEED, "4000", LAB_C, "", 4000);
    activity.properties = Some(
        serde_json::json!({"activity": {
        "activityType": "mentionInChat", "activitySubtype": "person", "activityId": 4000,
        "sourceThreadId": AC, "sourceMessageId": 4000, "sourceUserId": LAB_C
    }, "isread": false})
        .to_string(),
    );
    *d.rig.core.activity.lock().unwrap() = ActivityFeed::new(&[activity], None, None);
    d.rig.core.actions.lock().unwrap().clear();
    // a's banner clicked while a's feed is being read, then b shown before the read comes back
    d.rig.cx.update(|cx| {
        pane.update(cx, teams_ui::activity_view::ActivityView::reload);
        d.desktop.update(cx, |desktop, cx| {
            desktop.open_activity_item_of(LAB_A, "4000", cx);
            desktop.switch(LAB_B, cx);
        });
    });
    d.rig.cx.run_until_parked();
    assert_eq!(d.shown(window).0, LAB_B);
    assert_eq!(d.rig.cx.read(|cx| pane.read(cx).selected().map(str::to_owned)), None);
    let actions = d.rig.core.actions.lock().unwrap();
    assert!(
        !actions.contains(&teams_core::Action::ActivityRead {
            item: "4000".to_owned(),
            read: true
        }),
        "the hidden account opens and reads nothing: {actions:?}"
    );
}

#[test]
fn an_own_servers_account_names_its_server_in_its_own_settings_alone() {
    let d = TwoAccounts::start();
    let window = d.open();
    let servers = d.rig.cx.read(|cx| {
        let accounts = d.desktop.read(cx).accounts().read(cx);
        accounts
            .tiles()
            .iter()
            .map(|tile| tile.server.as_ref().map(ToString::to_string))
            .collect::<Vec<_>>()
    });
    assert_eq!(servers, [None, Some("chat.example.test:8443".to_owned())]);
    let show_settings = || {
        d.rig.cx.update(|cx| {
            let shell = window::content(window, cx)
                .unwrap()
                .read(cx)
                .shell_in(cx)
                .unwrap()
                .clone();
            shell.update(cx, teams_ui::Shell::show_settings);
        });
        d.rig.cx.run_until_parked();
    };
    let mut cx = gpui::VisualTestContext::from_window(window.into(), &d.rig.cx);
    // a, on Microsoft 365: its settings name no server
    show_settings();
    cx.run_until_parked();
    assert!(cx.debug_bounds("send-presence").is_some(), "the General page shows");
    assert!(cx.debug_bounds("account-server").is_none());
    // b, on its own server
    d.switch(LAB_B);
    show_settings();
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-server").is_some());
}

#[test]
fn an_own_servers_account_names_its_server_on_its_rail_tile_and_account_menu() {
    let d = TwoAccounts::start();
    let window = d.open();
    let tips = d.rig.cx.read(|cx| {
        let accounts = d.desktop.read(cx).accounts().read(cx);
        accounts
            .tiles()
            .iter()
            .map(|tile| teams_ui::shell::account_tip(tile).to_string())
            .collect::<Vec<_>>()
    });
    // a, on Microsoft 365: its name and what tells it apart, as before own servers
    assert_eq!(tips[0].lines().count(), 2, "{tips:?}");
    // b, on its own server: its server under them
    assert_eq!(tips[1].lines().nth(2), Some("chat.example.test:8443"), "{tips:?}");
    assert_eq!(tips[1].lines().count(), 3, "{tips:?}");

    let toggle_menu = || {
        d.rig.cx.update(|cx| {
            let shell = window::content(window, cx)
                .unwrap()
                .read(cx)
                .shell_in(cx)
                .unwrap()
                .clone();
            shell.update(cx, teams_ui::Shell::toggle_status_menu);
        });
        d.rig.cx.run_until_parked();
    };
    let mut cx = gpui::VisualTestContext::from_window(window.into(), &d.rig.cx);
    toggle_menu();
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-menu-account").is_some(), "a's account menu");
    assert!(cx.debug_bounds("status-menu-account-server").is_none());
    toggle_menu();
    d.switch(LAB_B);
    toggle_menu();
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-menu-account-server").is_some());
}

/// An own server's account list of two made-up users, read-only.
struct TwoUsers;

impl UserAdmin for TwoUsers {
    fn list(&self, _: &str, _: Option<&str>) -> Result<UserPage, AdminProblem> {
        let user = |n: u8, upn: &str, name: &str| AdminUser {
            id: format!("00000000-0000-4000-8000-0000000000b{n}"),
            upn: upn.to_owned(),
            display_name: name.to_owned(),
            enabled: true,
            admin: false,
            must_change_password: false,
        };
        Ok(UserPage {
            users: vec![
                user(1, "dana@example.test", "Dana Doe"),
                user(2, "evan@example.test", "Evan Roe"),
            ],
            next: None,
        })
    }

    fn add(&self, _: &NewAccount) -> Result<AdminUser, AdminProblem> {
        Err(AdminProblem::NotAdmin)
    }

    fn change(&self, _: &str, _: &UserChanges) -> Result<AdminUser, AdminProblem> {
        Err(AdminProblem::NotAdmin)
    }

    fn delete(&self, _: &str) -> Result<(), AdminProblem> {
        Err(AdminProblem::NotAdmin)
    }

    fn reset_password(&self, _: &str, _: &Secret, _: bool) -> Result<(), AdminProblem> {
        Err(AdminProblem::NotAdmin)
    }
}

#[test]
fn an_account_menu_item_over_a_user_row_takes_the_click_alone() {
    let d = TwoAccounts::start();
    let window = d.open();
    d.rig.cx.update(|cx| {
        let accounts = d.desktop.read(cx).accounts().clone();
        accounts.update(cx, |accounts, cx| {
            accounts.set_admin(LAB_A, Some(Arc::new(TwoUsers)), cx);
        });
    });
    let shell = d.rig.cx.update(|cx| {
        window::content(window, cx)
            .unwrap()
            .read(cx)
            .shell_in(cx)
            .unwrap()
            .clone()
    });
    d.rig.cx.update(|cx| shell.update(cx, teams_ui::Shell::show_settings));
    d.rig.cx.run_until_parked();
    let mut cx = gpui::VisualTestContext::from_window(window.into(), &d.rig.cx);
    cx.run_until_parked();
    let page = cx.debug_bounds("settings-page-users").expect("Manage users is offered");
    cx.simulate_click(page.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    d.rig
        .cx
        .update(|cx| shell.update(cx, teams_ui::Shell::toggle_status_menu));
    cx.run_until_parked();

    let rows = ["user-row-dana@example.test", "user-row-evan@example.test"]
        .map(|row| cx.debug_bounds(row).expect("the users are listed"));
    let over_a_row = [
        "status-menu-available",
        "status-menu-busy",
        "status-menu-do-not-disturb",
        "status-menu-away",
    ]
    .into_iter()
    .filter_map(|item| cx.debug_bounds(item))
    .map(|item| item.center())
    .find(|point| rows.iter().any(|row| row.contains(point)))
    .expect("a menu item lies over a user row");
    cx.simulate_click(over_a_row, gpui::Modifiers::none());
    cx.run_until_parked();

    assert_eq!(d.traced("status-menu", "chosen").len(), 1, "the menu took the click");
    assert!(
        d.traced("user-admin", "dialog").is_empty(),
        "the row under the menu got the click too"
    );
}

#[test]
fn a_press_outside_the_account_menu_closes_it_and_its_avatar_still_toggles_it() {
    let d = TwoAccounts::start();
    let window = d.open();
    let shell = d.rig.cx.update(|cx| {
        window::content(window, cx)
            .unwrap()
            .read(cx)
            .shell_in(cx)
            .unwrap()
            .clone()
    });
    let mut cx = gpui::VisualTestContext::from_window(window.into(), &d.rig.cx);
    cx.run_until_parked();
    let open = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| shell.read(cx).status_menu_open());
    for avatar in ["rail-status", "title-account"] {
        let Some(at) = cx.debug_bounds(avatar).map(|bounds| bounds.center()) else {
            continue;
        };
        // the avatar opens the menu and closes it again: its own press doesn't close it first
        cx.simulate_click(at, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(open(&mut cx), "{avatar} opens the menu");
        cx.simulate_click(at, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(!open(&mut cx), "{avatar} closes it");
        // a press anywhere else closes it, as in Teams
        cx.simulate_click(at, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(open(&mut cx));
        let menu = cx.debug_bounds("status-menu").expect("the menu is drawn");
        let elsewhere = cx.update(|window, _| {
            let viewport = window.viewport_size();
            gpui::point(viewport.width - gpui::px(24.0), viewport.height * 0.6)
        });
        assert!(!menu.contains(&elsewhere));
        cx.simulate_click(elsewhere, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(!open(&mut cx), "a press beside the menu closes it");
    }
    assert!(
        d.traced("status-menu", "closed").len() >= 2,
        "the avatar's and the outside press's closing are traced"
    );
}

#[test]
fn the_account_menu_takes_the_keyboard_and_gives_it_back() {
    let d = TwoAccounts::start();
    let window = d.open();
    let shell = d.rig.cx.update(|cx| {
        window::content(window, cx)
            .unwrap()
            .read(cx)
            .shell_in(cx)
            .unwrap()
            .clone()
    });
    let mut cx = gpui::VisualTestContext::from_window(window.into(), &d.rig.cx);
    cx.run_until_parked();
    let open = |cx: &mut gpui::VisualTestContext| cx.update(|_, cx| shell.read(cx).status_menu_open());
    let focused = |cx: &mut gpui::VisualTestContext| cx.update(|window, cx| window.focused(cx));
    let at = cx
        .debug_bounds("title-account")
        .or_else(|| cx.debug_bounds("rail-status"))
        .expect("an avatar opens the account menu")
        .center();
    // the keyboard is in the search box, which takes Enter as a draft would
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_search(teams_ui::global_search::Mode::Search, window, cx);
        });
    });
    cx.run_until_parked();
    let typing = focused(&mut cx);
    // the press on the avatar puts the keyboard on the shell (its root is focusable), the click opens the menu
    cx.simulate_mouse_down(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    let before = focused(&mut cx);
    assert!(before.is_some());
    cx.simulate_mouse_up(at, gpui::MouseButton::Left, gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(open(&mut cx));
    assert_ne!(
        focused(&mut cx),
        typing,
        "the open menu, not the search box, has the keyboard"
    );
    assert_ne!(focused(&mut cx), before, "the open menu has the keyboard");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!open(&mut cx), "Escape closes it");
    assert_eq!(focused(&mut cx), before, "the keyboard is back where it was");
    // Down reaches its first status, Enter picks it, and the keyboard goes back
    cx.simulate_click(at, gpui::Modifiers::none());
    cx.run_until_parked();
    for key in ["down", "enter"] {
        // down and up, as a person presses it: a focused row is clicked on the key's release
        let keystroke = gpui::Keystroke::parse(key).unwrap();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        cx.run_until_parked();
    }
    assert!(!open(&mut cx), "a picked status closes it");
    assert_eq!(d.traced("status-menu", "chosen").len(), 1, "Enter picked a status");
    assert_eq!(focused(&mut cx), before, "the keyboard is back after a pick");
}

/// One made-up profile picture, the same for both accounts: a grey PNG.
fn same_profile_picture() -> teams_store::StoredPicture {
    teams_store::StoredPicture {
        media_type: "image/png".to_owned(),
        bytes: png_of([128, 128, 128, 255], 8),
    }
}

fn png_of(colour: [u8; 4], side: u32) -> Vec<u8> {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(side, side, image::Rgba(colour))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    png.into_inner()
}

/// The two accounts on a settings file in `dir`, both with the same profile picture; a picture file to choose.
struct Pictured {
    d: TwoAccounts,
    chosen: std::path::PathBuf,
}

impl Pictured {
    fn start(dir: &std::path::Path) -> Self {
        Self::start_with(dir, Some(same_profile_picture()))
    }

    /// [`Self::start`] with `profile` as the profile picture of both accounts.
    fn start_with(dir: &std::path::Path, profile: Option<teams_store::StoredPicture>) -> Self {
        let chosen = dir.join("chosen.png");
        std::fs::write(&chosen, png_of([30, 120, 220, 255], 64)).unwrap();
        let d = TwoAccounts::start_on(&Start {
            settings: Some(&dir.join("settings.toml")),
            profile,
        });
        d.rig.cx.run_until_parked();
        Self { d, chosen }
    }

    fn accounts(&self) -> Entity<teams_ui::accounts::Accounts> {
        self.d.rig.cx.read(|cx| self.d.desktop.read(cx).accounts().clone())
    }

    /// Account `key`'s profile picture, the picture chosen for it, and the one that shows which account it is.
    fn pictures(&self, key: &str) -> [Option<teams_ui::people::Picture>; 3] {
        let accounts = self.accounts();
        self.d.rig.cx.read(|cx| {
            let tile = accounts.read(cx).tile(key).unwrap();
            [tile.picture.clone(), tile.chosen_picture.clone(), tile.face().cloned()]
        })
    }

    fn file_of(dir: &std::path::Path, key: &str) -> std::path::PathBuf {
        teams_ui::account_picture::AccountPictures::new(dir.join(teams_ui::account_picture::DIR)).path(key)
    }

    /// Keeps the picture file as account `key`'s picture, as the file chooser does.
    fn choose(&self, key: &str) {
        let using = self.start_choosing(key, &self.chosen);
        self.d.rig.cx.run_until_parked();
        drop(using);
    }

    /// Starts to keep the picture file at `path` as account `key`'s picture; it is saved when the test runs the
    /// background work.
    fn start_choosing(&self, key: &str, path: &std::path::Path) -> gpui::Task<Option<gpui::SharedString>> {
        let (path, accounts) = (path.to_owned(), self.accounts());
        self.d
            .rig
            .cx
            .update(|cx| accounts.update(cx, |accounts, cx| accounts.use_picture(key, path, cx)))
    }

    /// Account `key`'s "Use profile picture".
    fn use_profile_picture(&self, key: &str) {
        let accounts = self.accounts();
        self.d
            .rig
            .cx
            .update(|cx| accounts.update(cx, |accounts, cx| accounts.use_profile_picture(key, cx)));
    }

    /// The user picks `path` in the file chooser that is open.
    fn pick(&self, path: &std::path::Path) {
        assert!(self.d.rig.cx.did_prompt_for_paths(), "a file chooser is open");
        let picked = path.to_owned();
        self.d.rig.cx.simulate_path_prompt_response(move |options| {
            assert!(options.files && !options.multiple && !options.directories);
            assert_eq!(options.title.as_deref(), Some("Choose a picture"));
            let names: Vec<&str> = options.filters.iter().map(|filter| filter.name.as_str()).collect();
            assert_eq!(names, ["Pictures (PNG, JPEG, GIF, WebP)", "All files"]);
            let pictures: Vec<&str> = options.filters[0]
                .extensions
                .iter()
                .map(gpui::SharedString::as_str)
                .collect();
            assert_eq!(pictures, ["png", "jpg", "jpeg", "gif", "webp"]);
            assert!(options.filters[1].extensions.is_empty());
            Some(vec![picked])
        });
        self.d.rig.cx.run_until_parked();
    }

    /// A window open on Settings > Accounts.
    fn accounts_page(&self) -> VisualTestContext {
        let window = self.d.open();
        let shell = self.shell(window);
        self.d
            .rig
            .cx
            .update(|cx| shell.update(cx, teams_ui::Shell::show_settings));
        let mut cx = VisualTestContext::from_window(window.into(), &self.d.rig.cx);
        cx.run_until_parked();
        let page = cx.debug_bounds("settings-page-accounts").expect("Accounts is offered");
        cx.simulate_click(page.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        cx
    }

    /// The shell the window shows.
    fn shell(&self, window: WindowHandle<KitRoot>) -> Entity<teams_ui::Shell> {
        self.d.rig.cx.update(|cx| {
            window::content(window, cx)
                .unwrap()
                .read(cx)
                .shell_in(cx)
                .unwrap()
                .clone()
        })
    }
}

#[test]
fn a_picture_chosen_for_an_account_shows_for_it_alone_and_stays_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());
    let [a_profile, a_chosen, a_face] = p.pictures(LAB_A);
    let [b_profile, _, b_face] = p.pictures(LAB_B);
    assert!(a_profile.is_some() && a_chosen.is_none());
    assert_eq!(a_profile, b_profile, "both accounts have the same profile picture");
    assert_eq!(a_face, b_face, "so they can't be told apart");

    p.choose(LAB_B);
    std::fs::remove_file(&p.chosen).unwrap();

    let [b_profile, b_chosen, b_face] = p.pictures(LAB_B);
    assert!(b_chosen.is_some());
    assert_eq!(b_face, b_chosen, "b shows the picture chosen for it");
    assert_eq!(b_profile, a_profile, "its profile picture stays as it was");
    assert_eq!(p.pictures(LAB_A)[2], a_face, "a shows its profile picture");
    assert_ne!(b_face, a_face, "the accounts can be told apart");
    assert!(Pictured::file_of(dir.path(), LAB_B).is_file(), "kept in the app's data");
    let chosen = p.d.traced("account", "picture-chosen");
    assert_eq!(chosen.len(), 1);
    assert_eq!(chosen[0]["people"], serde_json::json!([LAB_B]));
    let asked = p.d.second.core.asked.lock().unwrap().clone();
    assert!(
        asked.iter().all(|asked| !asked.contains("picture")),
        "nothing is asked of the engine: {asked:?}"
    );

    // the app started again on the same files shows it again
    drop(p);
    let again = Pictured::start(dir.path());
    assert_eq!(again.pictures(LAB_B)[1], b_chosen);
    assert!(again.pictures(LAB_A)[1].is_none());
}

#[test]
fn use_profile_picture_shows_the_profile_picture_again_and_deletes_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());
    p.choose(LAB_B);
    assert!(p.pictures(LAB_B)[1].is_some());

    p.use_profile_picture(LAB_B);
    p.d.rig.cx.run_until_parked();

    let [b_profile, b_chosen, b_face] = p.pictures(LAB_B);
    assert!(b_chosen.is_none());
    assert_eq!(b_face, b_profile);
    assert!(!Pictured::file_of(dir.path(), LAB_B).exists());
    assert_eq!(p.d.traced("account", "picture-reset").len(), 1);
}

#[test]
fn an_account_removed_takes_its_chosen_picture_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());
    p.choose(LAB_A);
    p.choose(LAB_B);
    assert!(Pictured::file_of(dir.path(), LAB_B).is_file());

    // as the app removes an account: its own settings are forgotten, then it leaves the window
    let settings = p.d.rig.cx.read(|cx| p.d.desktop.read(cx).settings().clone());
    p.d.rig.cx.update(|cx| {
        settings.update(cx, |settings, cx| settings.forget_account(LAB_B, Some(LAB_A), cx));
        p.d.desktop.update(cx, |desktop, cx| desktop.remove_account(LAB_B, cx));
    });
    p.d.rig.cx.run_until_parked();

    assert!(!Pictured::file_of(dir.path(), LAB_B).exists());
    assert!(
        Pictured::file_of(dir.path(), LAB_A).is_file(),
        "the other account keeps its picture"
    );
}

#[test]
fn the_last_choice_stays_when_a_picture_still_being_saved_ends_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());
    let red = dir.path().join("red.png");
    std::fs::write(&red, png_of([220, 30, 30, 255], 64)).unwrap();

    // a picture chosen, then "Use profile picture" before it was saved
    let saving = p.start_choosing(LAB_B, &p.chosen);
    p.use_profile_picture(LAB_B);
    p.d.rig.cx.run_until_parked();
    drop(saving);
    assert!(p.pictures(LAB_B)[1].is_none(), "the profile picture shows");
    assert!(
        !Pictured::file_of(dir.path(), LAB_B).exists(),
        "and stays after a restart"
    );
    assert!(p.d.traced("account", "picture-chosen").is_empty());

    // two pictures chosen, one after the other, both saved at the same time: the second one stays
    let first = p.start_choosing(LAB_B, &p.chosen);
    let second = p.start_choosing(LAB_B, &red);
    p.d.rig.cx.run_until_parked();
    drop((first, second));
    let shown = p.pictures(LAB_B)[1].clone().expect("a picture chosen");
    let kept = std::fs::read(Pictured::file_of(dir.path(), LAB_B)).unwrap();
    assert_eq!(
        shown,
        teams_ui::people::Picture::png(kept.clone()),
        "the window shows what is kept"
    );
    let kept = image::load_from_memory(&kept).unwrap().to_rgba8();
    assert_eq!(kept.get_pixel(8, 8).0, [220, 30, 30, 255], "the second picture is kept");
    let files = std::fs::read_dir(dir.path().join(teams_ui::account_picture::DIR))
        .unwrap()
        .count();
    assert_eq!(files, 1, "no part file left");
}

#[test]
fn a_picture_still_being_saved_when_its_account_is_removed_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());

    // its own settings are forgotten while the picture is saved; the tile stays until the engine stopped (and for the
    // last account, the app quits with it)
    let saving = p.start_choosing(LAB_B, &p.chosen);
    let settings = p.d.rig.cx.read(|cx| p.d.desktop.read(cx).settings().clone());
    p.d.rig.cx.update(|cx| {
        settings.update(cx, |settings, cx| settings.forget_account(LAB_B, Some(LAB_A), cx));
    });
    p.d.rig.cx.run_until_parked();
    drop(saving);

    assert!(!Pictured::file_of(dir.path(), LAB_B).exists(), "no file left behind");
    assert!(p.pictures(LAB_B)[1].is_none());
    p.d.rig
        .cx
        .update(|cx| p.d.desktop.update(cx, |desktop, cx| desktop.remove_account(LAB_B, cx)));
    p.d.rig.cx.run_until_parked();
    assert!(!Pictured::file_of(dir.path(), LAB_B).exists());
}

#[test]
fn the_account_menu_changes_the_picture_and_goes_back_to_the_profile_picture() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());
    let window = p.d.open();
    p.d.switch(LAB_B);
    let shell = p.shell(window);
    let mut cx = gpui::VisualTestContext::from_window(window.into(), &p.d.rig.cx);
    let toggle = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_, cx| shell.update(cx, teams_ui::Shell::toggle_status_menu));
        cx.run_until_parked();
    };
    toggle(&mut cx);
    assert!(
        cx.debug_bounds("status-menu-account-picture").is_some(),
        "the menu shows the account's picture"
    );
    assert!(
        cx.debug_bounds("status-menu-profile-picture").is_none(),
        "no picture chosen: nothing to go back from"
    );
    let change = cx.debug_bounds("status-menu-change-picture").expect("Change picture");
    cx.simulate_click(change.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| shell.read(cx).status_menu_open()), "the menu closes");
    p.pick(&p.chosen);
    assert!(p.pictures(LAB_B)[1].is_some(), "b shows the picture picked");
    assert!(p.pictures(LAB_A)[1].is_none());

    toggle(&mut cx);
    let profile = cx
        .debug_bounds("status-menu-profile-picture")
        .expect("Use profile picture, now that a picture is chosen");
    cx.simulate_click(profile.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(p.pictures(LAB_B)[1].is_none());
    assert!(!Pictured::file_of(dir.path(), LAB_B).exists());

    // a file that is no picture: the menu is closed, so a toast says why
    let text = dir.path().join("notes.png");
    std::fs::write(&text, "not a picture").unwrap();
    toggle(&mut cx);
    let change = cx.debug_bounds("status-menu-change-picture").unwrap();
    cx.simulate_click(change.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    p.pick(&text);
    cx.run_until_parked();
    assert!(p.pictures(LAB_B)[1].is_none());
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).toast().cloned()).as_deref(),
        Some("That file can't be used. Choose a PNG, JPEG, GIF or WebP picture.")
    );
}

#[test]
fn settings_accounts_lists_each_account_with_its_picture_buttons_and_says_why_a_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());
    let mut cx = p.accounts_page();
    for n in 0..2 {
        assert!(
            cx.debug_bounds(format!("settings-account-{n}").leak()).is_some(),
            "account {n}"
        );
        assert!(cx.debug_bounds(format!("account-picture-change-{n}").leak()).is_some());
        assert!(cx.debug_bounds(format!("account-picture-profile-{n}").leak()).is_none());
    }
    assert!(cx.debug_bounds("account-picture-note").is_some());

    // b's Change picture, a file that is no picture: said under b's row, nothing changed
    let text = dir.path().join("notes.png");
    std::fs::write(&text, "not a picture").unwrap();
    let change = cx.debug_bounds("account-picture-change-1").unwrap();
    cx.simulate_click(change.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    p.pick(&text);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("account-picture-problem-1").is_some(),
        "why it was refused"
    );
    assert!(cx.debug_bounds("account-picture-problem-0").is_none());
    assert!(p.pictures(LAB_B)[1].is_none());
    assert_eq!(p.d.traced("account", "picture-refused").len(), 1);

    // then a picture: b's row offers Use profile picture, the problem goes
    let change = cx.debug_bounds("account-picture-change-1").unwrap();
    cx.simulate_click(change.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    p.pick(&p.chosen);
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-picture-problem-1").is_none());
    assert!(p.pictures(LAB_B)[1].is_some());
    let profile = cx
        .debug_bounds("account-picture-profile-1")
        .expect("Use profile picture on b's row");
    assert!(cx.debug_bounds("account-picture-profile-0").is_none());
    cx.simulate_click(profile.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(p.pictures(LAB_B)[1].is_none());

    // Add account asks the app, as the rail's "+" does
    let add = cx.debug_bounds("settings-add-account").expect("Add account");
    cx.simulate_click(add.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(*p.d.asked.lock().unwrap(), [DesktopEvent::AddAccount]);
}

#[test]
fn settings_accounts_says_which_picture_each_account_shows() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start(dir.path());
    let mut cx = p.accounts_page();
    assert!(cx.debug_bounds("account-picture-shows-1-profile").is_some());

    p.choose(LAB_B);
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-picture-shows-1-chosen").is_some());
    assert!(cx.debug_bounds("account-picture-shows-0-profile").is_some());
}

#[test]
fn settings_accounts_says_the_initials_show_for_an_account_without_a_picture() {
    let dir = tempfile::tempdir().unwrap();
    let p = Pictured::start_with(dir.path(), None);
    let mut cx = p.accounts_page();
    for n in 0..2 {
        assert!(p.pictures([LAB_A, LAB_B][n])[2].is_none(), "account {n} has no picture");
        assert!(
            cx.debug_bounds(format!("account-picture-shows-{n}-initials").leak())
                .is_some(),
            "account {n} says its initials show"
        );
        assert!(
            cx.debug_bounds(format!("account-picture-shows-{n}-profile").leak())
                .is_none()
        );
    }

    p.choose(LAB_B);
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-picture-shows-1-chosen").is_some());
    assert!(cx.debug_bounds("account-picture-shows-0-initials").is_some());
}

#[test]
fn settings_in_memory_offer_no_picture_to_choose() {
    let d = TwoAccounts::start();
    let window = d.open();
    let shell = d.rig.cx.update(|cx| {
        window::content(window, cx)
            .unwrap()
            .read(cx)
            .shell_in(cx)
            .unwrap()
            .clone()
    });
    let mut cx = gpui::VisualTestContext::from_window(window.into(), &d.rig.cx);
    cx.update(|_, cx| shell.update(cx, teams_ui::Shell::toggle_status_menu));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-menu-account").is_some());
    assert!(cx.debug_bounds("status-menu-change-picture").is_none());
    cx.update(|_, cx| shell.update(cx, teams_ui::Shell::toggle_status_menu));
    cx.update(|_, cx| shell.update(cx, teams_ui::Shell::show_settings));
    cx.run_until_parked();
    let page = cx.debug_bounds("settings-page-accounts").expect("Accounts is offered");
    cx.simulate_click(page.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("settings-account-1").is_some());
    assert!(cx.debug_bounds("account-picture-change-1").is_none());
    assert!(cx.debug_bounds("account-picture-note").is_none());
}
