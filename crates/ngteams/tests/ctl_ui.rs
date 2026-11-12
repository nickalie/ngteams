//! The control socket's UI commands (`ctl::ui`): the window, the views it opens, the account it shows and the actions
//! dispatched to it, through the real protocol and server, over two accounts and a store seeded before the app starts:
//! a 1:1 chat with Bee, the group chat "Project" and a team "Design" with its channels. Every id, name and text is made
//! up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ctl_support::{ME, UPN, ask, launched_with_others, pump_until, quit, socket_in};
use gpui::{Entity, TestAppContext};
use ngteams::ctl::ui::{NAMESPACE, action_named};
use teams_ctl::ClientError;
use teams_ctl::protocol::{CtlError, ErrorCode, OpenTarget, Request, Response, UiState, View, WindowOp, WindowState};
use teams_store::{
    ChannelAbout, ChannelKind, Conversation, ConversationKind, Member, NameSource, PictureState, Profile, Store, Team,
    TeamAbout, TeamChannel,
};
use teams_ui::Desktop;
use teams_ui::shell::Main;

const BEE: &str = "8:orgid:00000000-0000-4000-8000-0000000000b1";
const ONE_TO_ONE: &str = "19:00000000-0000-4000-8000-0000000000b1_00000000-0000-4000-8000-0000000000ff@unq.gbl.spaces";
const PROJECT: &str = "19:00000000000000000000000000project@thread.v2";
const GENERAL: &str = "19:0000000000000000000000000general@thread.tacv2";
const PLANS: &str = "19:00000000000000000000000000plans@thread.tacv2";
const OPS_GENERAL: &str = "19:0000000000000000000000000000ops@thread.tacv2";
/// The second account.
const OTHER_UPN: &str = "other@example.test";
const OTHER: &str = "8:orgid:00000000-0000-4000-8000-0000000000fe";
const STEP: Duration = Duration::from_millis(50);

fn now_ms() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

fn conversation(store: &mut Store, id: &str, kind: ConversationKind, topic: Option<&str>) {
    store
        .upsert_conversation(&Conversation {
            id: id.to_owned(),
            kind,
            topic: topic.map(str::to_owned),
            version: 1,
            last_activity: None,
        })
        .unwrap();
    store
        .replace_members(id, 1, &[Member::new(ME, None), Member::new(BEE, None)])
        .unwrap();
}

fn channel(id: &str, name: &str, general: bool) -> TeamChannel {
    TeamChannel {
        id: id.to_owned(),
        name: name.to_owned(),
        general,
        kind: ChannelKind::Standard,
        shown: true,
        about: ChannelAbout::default(),
    }
}

/// The seeded store (see the module docs): everyone's profile and the teams fresh, so nothing is read (no token to read
/// with).
fn seed(store: &mut Store) {
    for (mri, name) in [(ME, "Someone"), (OTHER, "Other"), (BEE, "Bee Example")] {
        store
            .put_profile(&Profile {
                mri: mri.to_owned(),
                name: Some(name.to_owned()),
                name_source: Some(NameSource::Graph),
                picture: PictureState::None,
                read_at: now_ms(),
            })
            .unwrap();
    }
    conversation(store, ONE_TO_ONE, ConversationKind::OneToOne, None);
    conversation(store, PROJECT, ConversationKind::Group, Some("Project"));
    let design = team(
        "Design",
        vec![channel(GENERAL, "General", true), channel(PLANS, "Plans", false)],
    );
    let ops = team("Ops", vec![channel(OPS_GENERAL, "General", true)]);
    store.replace_teams(&[design, ops], now_ms()).unwrap();
}

/// A team named `name`, its id its first channel's.
fn team(name: &str, channels: Vec<TeamChannel>) -> Team {
    Team {
        id: channels[0].id.clone(),
        name: name.to_owned(),
        archived: false,
        group_id: None,
        mailbox: None,
        owner: false,
        can_add_channel: false,
        can_add_private_channel: false,
        about: TeamAbout::default(),
        channels,
    }
}

struct App {
    cx: TestAppContext,
    socket: PathBuf,
    desktop: Entity<Desktop>,
    _dir: tempfile::TempDir,
}

impl App {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        let cx = TestAppContext::single();
        let others = [(OTHER_UPN, "00000000-0000-4000-8000-0000000000fe")];
        let (launched, _) = launched_with_others(&cx, dir.path(), &socket, &Rc::new(Cell::new(0)), seed, &others);
        Self {
            cx,
            socket,
            desktop: launched.desktop,
            _dir: dir,
        }
    }

    fn ask(&self, account: Option<&str>, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, account, request)
    }

    /// What the window shows once `request` was done.
    fn done(&self, account: Option<&str>, request: Request) -> UiState {
        match self.ask(account, request) {
            Ok(Response::UiState(state)) => state,
            other => panic!("not what the window shows: {other:?}"),
        }
    }

    fn refused(&self, account: Option<&str>, request: Request) -> CtlError {
        match self.ask(account, request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("not refused: {other:?}"),
        }
    }

    fn state(&self) -> UiState {
        self.done(None, Request::UiState)
    }

    fn open(&self, account: Option<&str>, target: OpenTarget) -> UiState {
        self.done(account, Request::Open { target })
    }

    fn window(&self, op: WindowOp) -> Result<Response, ClientError> {
        self.ask(None, Request::Window { op })
    }

    fn set_in_tray(&self, in_tray: bool) {
        self.cx.update(|cx| {
            self.desktop.update(cx, |desktop, cx| desktop.set_in_tray(in_tray, cx));
        });
    }

    /// The shell the window shows' main view, once its chat list has loaded `chat` (it names the chat).
    fn list_loaded(&self, chat: &str) {
        pump_until(&self.cx, STEP, |cx| {
            let Some(window) = self.desktop.read(cx).window() else {
                return false;
            };
            let Ok(root) = window.read(cx) else { return false };
            let Ok(root) = root.view().clone().downcast::<teams_ui::Root>() else {
                return false;
            };
            root.read(cx)
                .shell_in(cx)
                .is_some_and(|shell| shell.read(cx).list().read(cx).row(chat).is_some())
        });
    }

    /// `ShowCalendar` dispatched, and the shell the window shows showing the calendar once it ran.
    fn action_shows_the_calendar(&self) {
        let dispatched = self.ask(
            None,
            Request::Action {
                name: "ShowCalendar".to_owned(),
            },
        );
        assert!(matches!(dispatched, Ok(Response::Dispatched { .. })), "{dispatched:?}");
        pump_until(&self.cx, STEP, |cx| {
            let Some(window) = self.desktop.read(cx).window() else {
                return false;
            };
            let Ok(root) = window.read(cx) else { return false };
            let Ok(root) = root.view().clone().downcast::<teams_ui::Root>() else {
                return false;
            };
            root.read(cx)
                .shell_in(cx)
                .is_some_and(|shell| *shell.read(cx).main() == Main::Calendar)
        });
    }

    /// The settings page the window shows.
    fn settings_page(&self) -> teams_ui::settings_view::Page {
        self.cx.read(|cx| {
            let window = self.desktop.read(cx).window().unwrap();
            let root = window
                .read(cx)
                .unwrap()
                .view()
                .clone()
                .downcast::<teams_ui::Root>()
                .unwrap();
            let shell = root.read(cx).shell_in(cx).unwrap().clone();
            shell.read(cx).settings_view().read(cx).page()
        })
    }
}

// A chat or channel opened reads its messages from the service, which finds no token here and stops that account's
// engine a moment later: each account opens one, after which only views that need no engine are opened for it.
#[test]
fn each_open_moves_what_the_window_shows() {
    let app = App::start();
    app.list_loaded(PROJECT);
    let start = app.state();
    assert_eq!((start.shown.as_deref(), start.window), (Some(UPN), WindowState::Shown));

    // a 1:1 chat by the other person's name
    let bee = app.open(
        None,
        OpenTarget::Chat {
            chat: "bee example".to_owned(),
        },
    );
    assert_eq!((bee.view, bee.chat.as_deref()), (View::Chat, Some(ONE_TO_ONE)));

    let settings = app.open(
        None,
        OpenTarget::Settings {
            page: Some("NOTIFICATIONS".to_owned()),
        },
    );
    assert_eq!(settings.view, View::Settings);
    assert_eq!(app.settings_page(), teams_ui::settings_view::Page::Notifications);
    let calendar = app.open(None, OpenTarget::Calendar);
    assert_eq!((calendar.view, calendar.chat), (View::Calendar, None));
    // without a page: where the settings were left
    let again = app.open(None, OpenTarget::Settings { page: None });
    assert_eq!(again.view, View::Settings);
    assert_eq!(app.settings_page(), teams_ui::settings_view::Page::Notifications);

    // another account's chat: that account shown
    let chat = app.open(
        Some(OTHER_UPN),
        OpenTarget::Chat {
            chat: "project".to_owned(),
        },
    );
    assert_eq!(
        (
            chat.shown.as_deref(),
            chat.view,
            chat.chat.as_deref(),
            chat.chat_title.as_deref()
        ),
        (Some(OTHER_UPN), View::Chat, Some(PROJECT), Some("Project"))
    );
    let other = app.open(Some(OTHER_UPN), OpenTarget::Calendar);
    assert_eq!((other.shown.as_deref(), other.view), (Some(OTHER_UPN), View::Calendar));
    assert_eq!(app.state(), other);
    quit(&app.cx);
}

#[test]
fn a_channel_opens_by_its_name_its_team_s_and_its_name_or_its_id() {
    for name in ["plans", "Design/Plans", PLANS] {
        let app = App::start();
        let plans = app.open(
            None,
            OpenTarget::Channel {
                channel: name.to_owned(),
            },
        );
        assert_eq!(
            (plans.view, plans.chat.as_deref()),
            (View::Channel, Some(PLANS)),
            "{name}"
        );
        quit(&app.cx);
    }
}

#[test]
fn an_open_naming_nothing_or_too_much_is_refused() {
    let app = App::start();
    let unknown = app.refused(
        None,
        Request::Open {
            target: OpenTarget::Chat {
                chat: "Nobody".to_owned(),
            },
        },
    );
    assert_eq!(unknown.code, ErrorCode::UnknownChat);
    let channel = app.refused(
        None,
        Request::Open {
            target: OpenTarget::Channel {
                channel: "Budget".to_owned(),
            },
        },
    );
    assert_eq!(channel.code, ErrorCode::UnknownChat);
    let general = app.refused(
        None,
        Request::Open {
            target: OpenTarget::Channel {
                channel: "general".to_owned(),
            },
        },
    );
    assert_eq!(general.code, ErrorCode::AmbiguousChat);
    let titles: Vec<&str> = general.candidates.iter().map(|chat| chat.title.as_str()).collect();
    assert_eq!(titles, ["Design/General", "Ops/General"]);
    let page = app.refused(
        None,
        Request::Open {
            target: OpenTarget::Settings {
                page: Some("Fonts".to_owned()),
            },
        },
    );
    assert_eq!(page.code, ErrorCode::BadRequest);
    assert!(page.message.contains("general"), "{page:?}");
    let search = app.refused(
        None,
        Request::Open {
            target: OpenTarget::Search { query: " ".to_owned() },
        },
    );
    assert_eq!(search.code, ErrorCode::BadRequest);
    let account = app.refused(
        Some("nobody@example.test"),
        Request::Open {
            target: OpenTarget::Calendar,
        },
    );
    assert_eq!(account.code, ErrorCode::UnknownAccount);
    quit(&app.cx);
}

#[test]
fn the_activity_pane_opens_in_the_list_s_place() {
    let app = App::start();
    assert!(!app.state().activity, "the chats listed at first");
    let calendar = app.open(None, OpenTarget::Calendar);
    assert!(!calendar.activity);
    let activity = app.open(None, OpenTarget::Activity);
    assert!(activity.activity, "{activity:?}");
    assert_ne!(activity.view, View::Calendar, "the calendar left for what it hid");
    assert_eq!(app.state(), activity);

    let other = app.open(Some(OTHER_UPN), OpenTarget::Activity);
    assert_eq!((other.shown.as_deref(), other.activity), (Some(OTHER_UPN), true));
    quit(&app.cx);
}

#[test]
fn a_search_shows_its_results() {
    let app = App::start();
    let found = app.open(
        None,
        OpenTarget::Search {
            query: "budget".to_owned(),
        },
    );
    assert_eq!(found.view, View::Search);
    quit(&app.cx);
}

#[test]
fn switching_the_account_shows_it() {
    let app = App::start();
    let other = app.done(None, Request::SwitchAccount { to: OTHER.to_owned() });
    assert_eq!(other.shown.as_deref(), Some(OTHER_UPN));
    let first = app.done(None, Request::SwitchAccount { to: "0".to_owned() });
    assert_eq!(first.shown.as_deref(), Some(UPN));
    let unknown = app.refused(None, Request::SwitchAccount { to: "7".to_owned() });
    assert_eq!(unknown.code, ErrorCode::UnknownAccount);
    quit(&app.cx);
}

#[test]
fn the_window_hides_to_the_tray_and_shows_again() {
    let app = App::start();
    app.set_in_tray(true);
    let window = |response: Result<Response, ClientError>| match response {
        Ok(Response::UiState(state)) => state.window,
        other => panic!("not what the window shows: {other:?}"),
    };
    assert_eq!(window(app.window(WindowOp::Hide)), WindowState::Tray);
    assert!(app.cx.read(|cx| app.desktop.read(cx).window().is_none()));
    // hidden already: nothing to do
    assert_eq!(window(app.window(WindowOp::Hide)), WindowState::Tray);
    assert_eq!(window(app.window(WindowOp::Show)), WindowState::Shown);
    assert_eq!(window(app.window(WindowOp::Toggle)), WindowState::Tray);
    assert_eq!(window(app.window(WindowOp::Toggle)), WindowState::Shown);
    assert_eq!(window(app.window(WindowOp::Focus)), WindowState::Shown);
    assert_eq!(window(app.window(WindowOp::Hide)), WindowState::Tray);
    assert_eq!(window(app.ask(None, Request::Activate)), WindowState::Shown);
    // an open from the tray opens the window
    assert_eq!(window(app.window(WindowOp::Hide)), WindowState::Tray);
    let calendar = app.open(None, OpenTarget::Calendar);
    assert_eq!((calendar.window, calendar.view), (WindowState::Shown, View::Calendar));
    quit(&app.cx);
}

#[test]
fn hiding_without_a_tray_item_is_refused_and_the_window_stays() {
    let app = App::start();
    for op in [WindowOp::Hide, WindowOp::Toggle] {
        let refused = app.refused(None, Request::Window { op });
        assert_eq!(refused.code, ErrorCode::NoTray, "{op:?}");
    }
    assert_eq!(app.state().window, WindowState::Shown);
    assert!(app.cx.read(|cx| app.desktop.read(cx).window().is_some()));
    quit(&app.cx);
}

#[test]
fn an_action_named_in_full_or_short_is_dispatched_and_runs() {
    let app = App::start();
    let Ok(Response::Actions { actions }) = app.ask(None, Request::ActionList) else {
        panic!("no action list");
    };
    assert!(actions.iter().all(|name| name.starts_with(NAMESPACE)), "{actions:?}");
    assert!(actions.contains(&"teams_ui::ShowCalendar".to_owned()), "{actions:?}");

    let dispatched = app.ask(
        None,
        Request::Action {
            name: "showcalendar".to_owned(),
        },
    );
    assert_eq!(
        dispatched.unwrap(),
        Response::Dispatched {
            action: "teams_ui::ShowCalendar".to_owned()
        }
    );
    pump_until(&app.cx, STEP, |cx| {
        let window = app.desktop.read(cx).window().unwrap();
        let root = window
            .read(cx)
            .unwrap()
            .view()
            .clone()
            .downcast::<teams_ui::Root>()
            .unwrap();
        root.read(cx)
            .shell_in(cx)
            .is_some_and(|shell| *shell.read(cx).main() == Main::Calendar)
    });
    assert_eq!(app.state().view, View::Calendar);

    let full = app.ask(
        None,
        Request::Action {
            name: "teams_ui::ShowChats".to_owned(),
        },
    );
    assert_eq!(
        full.unwrap(),
        Response::Dispatched {
            action: "teams_ui::ShowChats".to_owned()
        }
    );
    let unknown = app.refused(
        None,
        Request::Action {
            name: "Teleport".to_owned(),
        },
    );
    assert_eq!(unknown.code, ErrorCode::UnknownAction);

    // the window closed: nowhere to dispatch to
    app.set_in_tray(true);
    app.window(WindowOp::Hide).unwrap();
    let closed = app.refused(
        None,
        Request::Action {
            name: "ShowCalendar".to_owned(),
        },
    );
    assert_eq!(closed.code, ErrorCode::NoWindow);
    quit(&app.cx);
}

/// Settings opened without a page over a chat whose compose box has the keyboard take the keyboard into their page
/// column, as the rail's Settings does: an action sent then runs, not lost on the compose box they hide.
#[test]
fn an_action_after_settings_opened_without_a_page_runs() {
    let app = App::start();
    app.list_loaded(PROJECT);
    let chat = app.open(
        None,
        OpenTarget::Chat {
            chat: "project".to_owned(),
        },
    );
    assert_eq!(chat.view, View::Chat);
    let settings = app.open(None, OpenTarget::Settings { page: None });
    assert_eq!(settings.view, View::Settings);
    app.action_shows_the_calendar();
    assert_eq!(app.state().view, View::Calendar);
    quit(&app.cx);
}

/// The window shown again from the tray gives the keyboard to the account it shows, not the first one: an action sent
/// then runs in that account.
#[test]
fn an_action_after_the_window_shown_again_runs_in_the_account_shown() {
    let app = App::start();
    app.set_in_tray(true);
    let other = app.done(None, Request::SwitchAccount { to: OTHER.to_owned() });
    assert_eq!(other.shown.as_deref(), Some(OTHER_UPN));
    app.window(WindowOp::Hide).unwrap();
    let shown = app.window(WindowOp::Show).unwrap();
    assert!(
        matches!(&shown, Response::UiState(state) if state.shown.as_deref() == Some(OTHER_UPN)),
        "{shown:?}"
    );
    app.action_shows_the_calendar();
    let after = app.state();
    assert_eq!((after.shown.as_deref(), after.view), (Some(OTHER_UPN), View::Calendar));
    quit(&app.cx);
}

/// An action sent right after an account switch runs in the account switched to (the window is drawn before the
/// dispatch). The test app draws a dirty window as each request's effects end, so the stale frame the draw guards
/// against in the app isn't reached here: this holds the outcome.
#[test]
fn an_action_after_an_account_switch_runs_in_that_account() {
    let app = App::start();
    let before = app.state();
    assert_ne!(before.view, View::Calendar);
    let other = app.done(None, Request::SwitchAccount { to: OTHER.to_owned() });
    assert_eq!(other.shown.as_deref(), Some(OTHER_UPN));
    let dispatched = app.ask(
        None,
        Request::Action {
            name: "ShowCalendar".to_owned(),
        },
    );
    assert!(matches!(dispatched, Ok(Response::Dispatched { .. })), "{dispatched:?}");
    pump_until(&app.cx, STEP, |cx| {
        let window = app.desktop.read(cx).window().unwrap();
        let root = window
            .read(cx)
            .unwrap()
            .view()
            .clone()
            .downcast::<teams_ui::Root>()
            .unwrap();
        root.read(cx)
            .shell_in(cx)
            .is_some_and(|shell| *shell.read(cx).main() == Main::Calendar)
    });
    let after = app.state();
    assert_eq!((after.shown.as_deref(), after.view), (Some(OTHER_UPN), View::Calendar));
    // the first account's view is as it was
    let first = app.done(None, Request::SwitchAccount { to: "0".to_owned() });
    assert_eq!((first.shown.as_deref(), first.view), (Some(UPN), before.view));
    quit(&app.cx);
}

/// An account switched back to on a chat its panes don't draw (here signed out, as the session finding its refresh
/// token gone makes it) gives the keyboard to its shell, not the chat's compose box: an action sent then runs, not lost
/// on a box not drawn.
#[test]
fn an_action_after_switching_back_to_a_signed_out_chat_runs() {
    let app = App::start();
    app.list_loaded(PROJECT);
    let chat = app.open(
        None,
        OpenTarget::Chat {
            chat: "project".to_owned(),
        },
    );
    assert_eq!(chat.view, View::Chat);
    app.cx.update(|cx| {
        let window = app.desktop.read(cx).window().unwrap();
        let root = window
            .read(cx)
            .unwrap()
            .view()
            .clone()
            .downcast::<teams_ui::Root>()
            .unwrap();
        let status = root.read(cx).shell_in(cx).unwrap().read(cx).models().status.clone();
        status.update(cx, |status, cx| {
            status.account = teams_ui::Account::SignedOut { hint: "made up".into() };
            cx.notify();
        });
    });
    let other = app.done(None, Request::SwitchAccount { to: OTHER.to_owned() });
    assert_eq!(other.shown.as_deref(), Some(OTHER_UPN));
    let first = app.done(None, Request::SwitchAccount { to: "0".to_owned() });
    assert_eq!((first.shown.as_deref(), first.view), (Some(UPN), View::Chat));
    app.action_shows_the_calendar();
    let after = app.state();
    assert_eq!((after.shown.as_deref(), after.view), (Some(UPN), View::Calendar));
    quit(&app.cx);
}

#[test]
fn a_short_name_two_actions_share_is_refused() {
    let names = ["teams_ui::Close".to_owned(), "teams_ui::viewer::Close".to_owned()];
    assert_eq!(action_named(&names, "teams_ui::Close").unwrap(), "teams_ui::Close");
    assert_eq!(
        action_named(&names, "teams_ui::viewer::close").unwrap(),
        "teams_ui::viewer::Close"
    );
    let shared = action_named(&names, "close").unwrap_err();
    assert_eq!(shared.code, ErrorCode::UnknownAction);
    assert!(shared.message.contains("teams_ui::viewer::Close"), "{shared:?}");
}
