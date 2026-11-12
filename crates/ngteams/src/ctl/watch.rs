//! The events the app's own state makes, beside the engine's ([`super::events::tap`]): each one observed where it
//! changes, and told only when it did.
//!
//! - per account ([`account`]): `unread` from its tray count ([`Tray`], the badge's: a chat's count and the account's
//!   total, a line per chat whose count changed, and one without a chat for a total that changed alone, the first
//!   count's), `own_status` from its own presence ([`OwnPresence`]: what it reports, [`reporting_name`]), `signed_out`
//!   from its models' status, `notification` from its notifier (each one shown, with its title and chat);
//! - for the app ([`app`]): `ui` once what the window shows changed ([`Desktop::ui_state`]: the account shown, the view,
//!   the chat, where the window is, a call's window), told as the shown account's, and `call_window` once a call's
//!   window opened or closed, told as the account the call's window was open for (the shown one's, else the first
//!   account's, when that account isn't known). The desktop, its accounts, its call windows, every shell made and
//!   its chat list (which names the chat) are observed for them;
//! - `account_added` and `account_removed`, told by the app as it adds and removes one ([`crate::app`]).
//!
//! What was last seen is kept whether or not anyone listens, so a subscriber that comes later is told only what
//! changed after it came; an event is built only while someone listens.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::hash::BuildHasher;
use std::rc::Rc;

use gpui::{App, Entity};
use teams_core::Reporting;
use teams_ctl::protocol::{CtlEvent, UiState, View, WindowState};
use teams_proto::ManualAvailability;
use teams_ui::desktop::{UiShown, WindowShown};
use teams_ui::own_presence::OwnPresence;
use teams_ui::shell::Main;
use teams_ui::tray::Tray;
use teams_ui::{Account, Bridge, Desktop, NotifierEvent, Shell};

use super::events::{Accounts, Hub, Origin};

/// What the account's own presence reports, as the wire names it: `connecting` (no registration of the push channel
/// yet), `available`, `busy`, `do-not-disturb`, `away`, `not-set` (a manual status that failed), `inactive` (idle),
/// `halted` (refused before sending) or `stopped` (the quit, or the engine stopped).
#[must_use]
pub fn reporting_name(reporting: Reporting) -> &'static str {
    match reporting {
        Reporting::Connecting => "connecting",
        Reporting::Available | Reporting::Manual(ManualAvailability::Available) => "available",
        Reporting::Manual(ManualAvailability::Busy) => "busy",
        Reporting::Manual(ManualAvailability::DoNotDisturb) => "do-not-disturb",
        Reporting::Manual(ManualAvailability::Away) => "away",
        Reporting::NotSet(_) => "not-set",
        Reporting::Inactive => "inactive",
        Reporting::Halted => "halted",
        Reporting::Quit => "stopped",
    }
}

/// An account's unread counts as its tray last counted them: each chat with anything unread, and the total.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub chats: BTreeMap<String, u64>,
    pub total: Option<u64>,
}

impl Counts {
    /// `tray`'s counts now.
    #[must_use]
    pub fn of(tray: &Tray) -> Self {
        Self {
            chats: tray
                .unread_chats()
                .map(|(chat, count)| (chat.to_owned(), count))
                .collect(),
            total: tray.total(),
        }
    }
}

/// The `unread` events from `before` to `now` (see the module docs).
#[must_use]
pub fn unread_changes(before: &Counts, now: &Counts) -> Vec<CtlEvent> {
    let total = now.total.unwrap_or(0);
    let line = |chat: &str, count: u64| CtlEvent::Unread {
        chat: Some(chat.to_owned()),
        count,
        total,
    };
    let mut changes: Vec<CtlEvent> = now
        .chats
        .iter()
        .filter(|(chat, count)| before.chats.get(*chat) != Some(*count))
        .map(|(chat, count)| line(chat, *count))
        .collect();
    changes.extend(
        before
            .chats
            .keys()
            .filter(|chat| !now.chats.contains_key(*chat))
            .map(|chat| line(chat, 0)),
    );
    if changes.is_empty() && now.total.is_some() && now.total != before.total {
        changes.push(CtlEvent::Unread {
            chat: None,
            count: 0,
            total,
        });
    }
    changes
}

/// Account `upn`'s parts watched (see the module docs): its engine's tap, its tray count, its own presence, its
/// sign-out and its notifier. Once the desktop has the account.
pub fn account(hub: &Rc<Hub>, upn: &str, bridge: &Entity<Bridge>, desktop: &Entity<Desktop>, cx: &mut App) {
    super::events::tap(hub, upn, bridge, cx);
    let origin = Origin::of(upn, bridge, cx);
    let desktop = desktop.read(cx);
    let (tray, own) = desktop
        .accounts()
        .read(cx)
        .tile(&origin.mri)
        .map(|tile| (tile.tray.clone(), tile.own_presence.clone()))
        .unzip();
    let notifier = desktop.notifier_of(&origin.mri).cloned();
    if let Some(tray) = tray {
        unread(hub, &origin, &tray, cx);
    }
    if let Some(own) = own.flatten() {
        own_status(hub, &origin, &own, cx);
    }
    signed_out(hub, &origin, bridge, cx);
    if let Some(notifier) = notifier {
        let (hub, origin) = (Rc::clone(hub), origin);
        cx.subscribe(&notifier, move |_, event, cx| {
            if let NotifierEvent::Shown { conversation, title } = event
                && hub.listening()
            {
                let event = CtlEvent::Notification {
                    title: title.clone(),
                    chat: Some(conversation.clone()),
                };
                hub.publish(&origin, event, cx);
            }
        })
        .detach();
    }
}

fn unread(hub: &Rc<Hub>, origin: &Origin, tray: &Entity<Tray>, cx: &mut App) {
    let last = RefCell::new(Counts::of(tray.read(cx)));
    let (hub, origin) = (Rc::clone(hub), origin.clone());
    cx.observe(tray, move |tray, cx| {
        let now = Counts::of(tray.read(cx));
        let before = last.replace(now.clone());
        if before == now || !hub.listening() {
            return;
        }
        for event in unread_changes(&before, &now) {
            hub.publish(&origin, event, cx);
        }
    })
    .detach();
}

fn own_status(hub: &Rc<Hub>, origin: &Origin, own: &Entity<OwnPresence>, cx: &mut App) {
    let last = RefCell::new(reporting_name(own.read(cx).reporting(cx)));
    let (hub, origin) = (Rc::clone(hub), origin.clone());
    cx.observe(own, move |own, cx| {
        let now = reporting_name(own.read(cx).reporting(cx));
        if last.replace(now) == now || !hub.listening() {
            return;
        }
        let event = CtlEvent::OwnStatus {
            availability: now.to_owned(),
        };
        hub.publish(&origin, event, cx);
    })
    .detach();
}

fn signed_out(hub: &Rc<Hub>, origin: &Origin, bridge: &Entity<Bridge>, cx: &mut App) {
    let status = bridge.read(cx).models().status.clone();
    let is_out = |cx: &App| matches!(status.read(cx).account, Account::SignedOut { .. });
    let last = RefCell::new(is_out(cx));
    let (hub, origin) = (Rc::clone(hub), origin.clone());
    cx.observe(&status, move |status, cx| {
        let now = matches!(status.read(cx).account, Account::SignedOut { .. });
        if std::mem::replace(&mut *last.borrow_mut(), now) || !now || !hub.listening() {
            return;
        }
        hub.publish(&origin, CtlEvent::SignedOut, cx);
    })
    .detach();
}

/// What the window shows, as the wire tells it; `upns` names each account's MRI by its UPN.
#[must_use]
pub fn ui_state<S: BuildHasher>(shown: &UiShown, upns: &HashMap<String, String, S>) -> UiState {
    let (view, chat) = match &shown.main {
        Some(Main::Chat(id)) => (View::Chat, Some(id.clone())),
        Some(Main::Channel(id)) => (View::Channel, Some(id.clone())),
        Some(Main::Calendar) => (View::Calendar, None),
        Some(Main::Settings) => (View::Settings, None),
        Some(Main::Search) => (View::Search, None),
        Some(Main::TeamsPage | Main::ManageTeam(_) | Main::JoinTeam | Main::CreateTeamMore) => (View::Teams, None),
        Some(Main::Nothing | Main::Workflows | Main::NewChat) | None => (View::Other, None),
    };
    UiState {
        shown: shown.account.as_ref().and_then(|mri| upns.get(mri).cloned()),
        view,
        chat,
        chat_title: shown.title.clone(),
        window: match shown.window {
            WindowShown::Shown => WindowState::Shown,
            WindowShown::Tray => WindowState::Tray,
            WindowShown::Closed => WindowState::Closed,
        },
        call_window: shown.call_window,
        activity: shown.activity,
    }
}

/// What the window shows, last seen.
#[derive(Clone)]
struct Seen {
    ui: UiShown,
    /// The account the call's window was open for, while it is.
    call: Option<String>,
}

/// The app's `ui` and `call_window` events (see the module docs), over `desktop` and the accounts `accounts` lists.
pub fn app(hub: &Rc<Hub>, desktop: &Entity<Desktop>, accounts: Accounts, cx: &mut App) {
    let seen = Rc::new(RefCell::new(seen(desktop, cx)));
    let check: Rc<dyn Fn(&mut App)> = {
        let (hub, desktop) = (Rc::clone(hub), desktop.downgrade());
        Rc::new(move |cx: &mut App| {
            let Some(desktop) = desktop.upgrade() else { return };
            let now = self::seen(&desktop, cx);
            let before = seen.replace(now.clone());
            if before.ui == now.ui || !hub.listening() {
                return;
            }
            let accounts = accounts(cx);
            told(&hub, &accounts, &before, &now, cx);
        })
    };
    let read = desktop.read(cx);
    let (rail, calls) = (read.accounts().clone(), read.call_windows().clone());
    let on_desktop = Rc::clone(&check);
    cx.observe(desktop, move |_, cx| on_desktop(cx)).detach();
    let on_rail = Rc::clone(&check);
    cx.observe(&rail, move |_, cx| on_rail(cx)).detach();
    let on_calls = Rc::clone(&check);
    cx.observe(&calls, move |_, cx| on_calls(cx)).detach();
    // every shell made (the window's, opened again or for an account added) told too: its view and chat change there,
    // and its chat list names the chat (a rename or a row loaded notifies the list, not the shell)
    cx.observe_new(move |_: &mut Shell, _, cx| {
        let shell = cx.entity();
        let check = Rc::clone(&check);
        cx.defer(move |cx| {
            let list = shell.read(cx).list().clone();
            let on_list = Rc::clone(&check);
            cx.observe(&shell, move |_, cx| check(cx)).detach();
            cx.observe(&list, move |_, cx| on_list(cx)).detach();
        });
    })
    .detach();
}

/// What `desktop` shows now.
fn seen(desktop: &Entity<Desktop>, cx: &App) -> Seen {
    let read = desktop.read(cx);
    Seen {
        ui: read.ui_state(cx),
        call: read.call_windows().read(cx).showing().map(|(key, _)| key.to_owned()),
    }
}

/// The `ui` and `call_window` events from `before` to `now`.
fn told(hub: &Rc<Hub>, origins: &[Origin], before: &Seen, now: &Seen, cx: &mut App) {
    let upns: HashMap<String, String> = origins
        .iter()
        .map(|origin| (origin.mri.clone(), origin.upn.clone()))
        .collect();
    let of = |mri: Option<&String>| {
        mri.and_then(|mri| origins.iter().find(|origin| &origin.mri == mri))
            .or_else(|| origins.first())
    };
    if before.ui.call_window != now.ui.call_window
        && let Some(origin) = of(now.call.as_ref().or(before.call.as_ref()).or(now.ui.account.as_ref()))
    {
        let event = CtlEvent::CallWindow {
            open: now.ui.call_window,
        };
        hub.publish(origin, event, cx);
    }
    if let Some(origin) = of(now.ui.account.as_ref()) {
        let event = CtlEvent::Ui {
            state: ui_state(&now.ui, &upns),
        };
        hub.publish(origin, event, cx);
    }
}
