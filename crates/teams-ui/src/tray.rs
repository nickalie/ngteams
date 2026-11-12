//! The tray item (M2 Task 14): a `StatusNotifierItem` showing the account's unread total, with "Open" and "Quit".
//!
//! - **The total** ([`Tray`]): the unread chats, as Teams' own badge counts them: a chat the chat list shows (not a
//!   team's channel, not the account's own streams) counts once while the store holds any message of it unread, however
//!   many (Teams keeps one yes or no per chat: `docs/protocol/chat.md`, "Which chats are unread"). Counted again a moment
//!   after the models say a chat changed, each chat asked again only when it changed since it was counted, and summed
//!   over the window's accounts. The item's title and tooltip say it ("ngteams: 3 unread chats"), its status is
//!   `NeedsAttention` while it isn't 0, `Active` otherwise. Its icon is the app's, carried as pixels
//!   ([`crate::app_icon::pixmaps`]) with Teams' red unread badge and the count on it ("99+" past 99) while the total
//!   isn't 0, never a name from the theme: the `StatusNotifierItem` spec has no counter of its own, so the number is
//!   drawn into the icon, and into its attention icon too (a host shows that one while the item needs attention).
//! - **The launcher's count**: the same total goes to the desktop's task manager as the Unity launcher entry's `count`
//!   (`com.canonical.Unity.LauncherEntry.Update` for `application://<app id>.desktop`), which KDE Plasma's task manager
//!   and docks draw on the app's task button; hidden at 0. Nothing hears it on a desktop without one.
//! - **The item** ([`BusTray`]): `org.kde.StatusNotifierItem` at [`ITEM_PATH`] and its menu, `com.canonical.dbusmenu`
//!   at [`MENU_PATH`], on the connection to the bus the app is given ([`crate::bus`]), under the name
//!   `org.kde.StatusNotifierItem-<pid>-1`, registered with the watcher (`org.kde.StatusNotifierWatcher`). Activating
//!   it (a click) or the menu's "Open" asks for the window ([`TrayEvent::Open`]); "Quit" quits the app
//!   ([`TrayEvent::Quit`]). Without a watcher on the bus there is no item, and closing the window quits the app.
//! - **The watcher's comings and goings** ([`Hosted`]): the item follows the owner of the watcher's name. A new owner
//!   (a panel restarted) gets the item registered again; one that leaves with none after it, or the connection to the
//!   bus lost, leaves the item shown nowhere, which the app is told so closing its window quits it again.
//!
//! In a view test the item is a fake [`TrayIcon`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::bridge::Core;
use crate::chat_list::{self, LIST_LIMIT};
use crate::models::Models;
use crate::trace::{TraceLine, ViewTrace};
use futures_util::future::BoxFuture;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Subscription, WeakEntity};

/// The item's object path.
pub const ITEM_PATH: &str = "/StatusNotifierItem";
/// Its menu's object path.
pub const MENU_PATH: &str = "/MenuBar";
/// The watcher's bus name and object path.
pub const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
/// The menu's items, by id (0 is its root).
pub const MENU_OPEN: i32 = 1;
pub const MENU_QUIT: i32 = 2;
/// How long after a change the total is counted again.
pub const COUNT_DELAY: Duration = Duration::from_millis(300);
/// The Unity launcher entry's interface, and the path its signal comes from.
pub const LAUNCHER_ENTRY: &str = "com.canonical.Unity.LauncherEntry";
pub const LAUNCHER_PATH: &str = "/com/canonical/unity/launcherentry/ngteams";

/// What the tray asks the app for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// Show the window (open it again if it was closed).
    Open,
    /// Quit the app.
    Quit,
}

/// What became of the watcher the item is registered with ([`BusTray::start`]'s stream).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hosted {
    /// A new watcher took the name, and the item is registered with it.
    Registered,
    /// The watcher left (or a new one refused the item): the item is shown nowhere until another comes.
    Gone,
}

/// The tray item, as the [`Tray`] sees it.
pub trait TrayIcon: Send + Sync + 'static {
    /// Shows `unread` as the account's unread total.
    fn set_unread(&self, unread: u64) -> BoxFuture<'static, ()>;
}

/// The item's title (and tooltip) for an unread total: the app's name, and the total when it isn't 0.
#[must_use]
pub fn title(app: &str, unread: u64) -> String {
    match unread {
        0 => app.to_owned(),
        1 => format!("{app}: 1 unread chat"),
        _ => format!("{app}: {unread} unread chats"),
    }
}

/// The item's status for an unread total.
#[must_use]
pub fn status(unread: u64) -> &'static str {
    if unread == 0 { "Active" } else { "NeedsAttention" }
}

/// A chat's count, and the models' revision of its change when it was counted.
#[derive(Clone, Copy, Debug)]
struct Counted {
    at: u64,
    unread: u64,
}

/// The unread total and the item that shows it (see the module docs). A GPUI entity.
pub struct Tray {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    icon: Option<Arc<dyn TrayIcon>>,
    counted: HashMap<String, Counted>,
    total: Option<u64>,
    /// The other accounts' counts, whose totals the item shows summed with this one's ([`Tray::add_account`]).
    others: Vec<(Entity<Tray>, Subscription)>,
    /// The total the item was last shown (this account's and the others').
    shown: Option<u64>,
    /// A count is under way, and whether the models changed since it began.
    counting: bool,
    again: bool,
    _observing: Subscription,
}

impl Tray {
    /// The total over the models, counted at once and after every change; shown once an item is given
    /// ([`Tray::set_icon`]).
    pub fn new(core: Arc<dyn Core>, models: Models, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&models.chats, |this: &mut Self, _, cx| this.schedule(cx));
        let mut tray = Self {
            core,
            models,
            trace,
            icon: None,
            counted: HashMap::new(),
            total: None,
            others: Vec::new(),
            shown: None,
            counting: false,
            again: false,
            _observing: observing,
        };
        tray.schedule(cx);
        tray
    }

    /// The item from now on; it is shown the total at once.
    pub fn set_icon(&mut self, icon: Arc<dyn TrayIcon>, cx: &mut Context<Self>) {
        if let Some(total) = self.combined(cx) {
            cx.background_spawn(icon.set_unread(total)).detach();
        }
        self.icon = Some(icon);
    }

    /// Another account's count (an app of several accounts): the item shows the totals summed, counted again whenever
    /// that one's changes.
    pub fn add_account(&mut self, other: Entity<Tray>, cx: &mut Context<Self>) {
        let observing = cx.observe(&other, |this: &mut Self, _, cx| this.show(cx));
        self.others.push((other, observing));
        self.show(cx);
    }

    /// Takes another account's count out of the sum.
    pub fn remove_account(&mut self, other: &Entity<Tray>, cx: &mut Context<Self>) {
        self.others.retain(|(tray, _)| tray != other);
        self.show(cx);
    }

    /// The total the item shows: this account's and the others', once this one was counted.
    #[must_use]
    pub fn combined(&self, cx: &App) -> Option<u64> {
        let others: u64 = self.others.iter().filter_map(|(other, _)| other.read(cx).total()).sum();
        self.total.map(|total| total + others)
    }

    /// Shows the item the combined total when it changed.
    fn show(&mut self, cx: &mut Context<Self>) {
        let combined = self.combined(cx);
        if combined == self.shown {
            return;
        }
        self.shown = combined;
        if let Some(total) = combined {
            self.trace.record(&TraceLine::new("tray", "unread").unread([total]));
            if let Some(icon) = &self.icon {
                cx.background_spawn(icon.set_unread(total)).detach();
            }
        }
    }

    /// Takes the item away (another account's count shows it from now on).
    pub fn take_icon(&mut self) -> Option<Arc<dyn TrayIcon>> {
        self.shown = None;
        self.icon.take()
    }

    /// Whether an item was given.
    #[must_use]
    pub fn has_icon(&self) -> bool {
        self.icon.is_some()
    }

    /// The unread total (the unread chats) as last counted.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// The counted chats that hold anything unread, each with how many of its messages are, as last counted: the
    /// chats [`Tray::total`] counts (in no order).
    pub fn unread_chats(&self) -> impl Iterator<Item = (&str, u64)> {
        self.counted
            .iter()
            .filter(|(_, counted)| counted.unread > 0)
            .map(|(chat, counted)| (chat.as_str(), counted.unread))
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if self.counting {
            self.again = true;
            return;
        }
        self.counting = true;
        cx.spawn(async move |this, cx: &mut AsyncApp| count(this, cx).await)
            .detach();
    }

    /// A count's result: the chats it counted, as of `revisions`; the total is how many of them hold anything unread.
    /// The observers are told when the total or a chat's count changed (the control socket's `unread` follows both).
    fn counted(&mut self, chats: Vec<(String, Counted)>, cx: &mut Context<Self>) -> bool {
        let counted: HashMap<String, Counted> = chats.into_iter().collect();
        let recounted = counted.len() != self.counted.len()
            || counted
                .iter()
                .any(|(chat, now)| self.counted.get(chat).is_none_or(|before| before.unread != now.unread));
        self.counted = counted;
        let unread = self.counted.values().filter(|counted| counted.unread > 0).count();
        let total = u64::try_from(unread).unwrap_or(u64::MAX);
        if self.total != Some(total) {
            self.total = Some(total);
            self.show(cx);
            cx.notify();
        } else if recounted {
            cx.notify();
        }
        if std::mem::take(&mut self.again) {
            true
        } else {
            self.counting = false;
            false
        }
    }
}

/// Counts the total after [`COUNT_DELAY`], again while the models changed during a count.
async fn count(this: WeakEntity<Tray>, cx: &mut AsyncApp) {
    loop {
        cx.background_executor().timer(COUNT_DELAY).await;
        let Ok((core, models, known)) = this.update(cx, |tray, _| {
            (Arc::clone(&tray.core), tray.models.clone(), tray.counted.clone())
        }) else {
            return;
        };
        let Ok(conversations) = core.conversations(LIST_LIMIT).await else {
            // a stopped engine: the total stays as it was
            let _ = this.update(cx, |tray, _| tray.counting = false);
            return;
        };
        let mut chats = Vec::new();
        for conversation in conversations
            .iter()
            .filter(|conversation| chat_list::listed(conversation))
        {
            let at = cx.update(|cx| models.chats.read(cx).changed_at(&conversation.id));
            let counted = match known.get(&conversation.id) {
                Some(counted) if counted.at == at => *counted,
                _ => Counted {
                    at,
                    unread: core.unread_count(conversation.id.clone()).await.unwrap_or(0),
                },
            };
            chats.push((conversation.id.clone(), counted));
        }
        match this.update(cx, |tray, cx| tray.counted(chats, cx)) {
            Ok(true) => {}
            _ => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_says_the_total() {
        assert_eq!(title("App", 0), "App");
        assert_eq!(title("App", 1), "App: 1 unread chat");
        assert_eq!(title("App", 3), "App: 3 unread chats");
        assert_eq!(title("App", 250), "App: 250 unread chats");
        assert_eq!(status(0), "Active");
        assert_eq!(status(1), "NeedsAttention");
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{BusTray, launcher_properties, launcher_uri};
