//! The app on the desktop (M2 Task 14): its main window, its notifications ([`Notifier`]) and its tray item
//! ([`Tray`]), held together.
//!
//! - The window opens over the shell's parts ([`Desktop::open_window`]); closed, it can be opened again from the tray
//!   or a notification, over the same models ([`Desktop::show`]).
//! - **Closing the window keeps the app in the tray** while its tray item is up (a watcher took it); without one,
//!   closing the window quits ([`DesktopEvent::Quit`], which the app acts on: it shuts the engine down, then quits).
//!   The item follows the watcher ([`Hosted`]): a watcher that leaves (a panel restarting, or gone for good) or the
//!   bus lost takes the item down, and a new watcher brings it up again. Taken down while the window is closed, the
//!   window opens again: the app is never left running with no window and no item to bring it back.
//! - A notification activated opens its chat in the window, opened again if it was closed (an app's Activity item's
//!   opens the Activity pane at the item); the tray's "Open" (or a
//!   click on the item) shows the window, its "Quit" quits.
//! - The notifier is told the chat shown in a focused window, which never notifies.
//! - The account's own presence ([`OwnPresence`], M2 Task 20) lives as long as the app, not the window: its refresh goes
//!   on while the app stays in the tray, the shell of each window it opens tells it of the window's use and shows it
//!   in the account menu, and the quit stops it before the app asks for its shutdown (nothing is sent on quit).
//! - An incoming call (M3 Task 17) rings through the [`CallAlert`]: its ringtone on the default output and its
//!   notification with Answer and Decline on the same bus; its body shows the window, where the banner is.
//! - **A call has a window of its own** ([`CallWindows`]), any account's, one at a time: its "Device settings" shows
//!   the main window's settings on their Calls page for its account. The app stays while it is open: the main window
//!   closed without a tray item to stay in quits only once the call window is closed too.
//! - [`Desktop::connect`] connects to the bus the app is given ([`crate::bus`]) and puts the notifications and the
//!   tray item on it; without one, neither is there and nothing else is tried. On Windows it instead connects to
//!   the native shell in the current interactive session, without reading or reaching a D-Bus address.
//!
//! - **Several accounts** ([`Desktop::add_account`], [`Desktop::remove_account`]): each account has its own notifier,
//!   call alert and own presence, all live at once, so every account notifies and rings whichever one the window
//!   shows. A notification never names its account (the accounts may well share one name): activating one shows its
//!   account in the window first, found by the notification's id, never its text. The tray item's total sums every
//!   account's unread count. The window holds a shell per account and shows the one the [`Accounts`] show; the rail's
//!   avatars switch between them, and its "+" and an account's "Sign out and remove" are asked of the app
//!   ([`DesktopEvent::AddAccount`], [`DesktopEvent::RemoveAccount`]), which starts or stops the account's engine and
//!   tells the desktop.
//!
//! What it shows (the account, the view, the window, a call's window) reads as one [`UiShown`]
//! ([`Desktop::ui_state`]); the desktop tells its observers when its window opens or closes or its tray item comes or
//! goes.
//!
//! What it does is traced: `window opened` (the root view's), `window closed`, `tray open`/`quit`, `tray
//! registered`/`gone` (the item up or down), and the notifier's and the tray's own lines.

use std::rc::Rc;
use std::sync::Arc;

#[cfg(unix)]
use futures_util::StreamExt as _;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, SharedString, Subscription, Task, WeakEntity,
    Window, WindowHandle, WindowId,
};
#[cfg(unix)]
use tokio::sync::mpsc;
#[cfg(unix)]
use zbus::Connection;

use crate::accounts::{AccountParts, Accounts, AccountsEvent};
#[cfg(unix)]
use crate::bus;
use crate::bus::BusError;
use crate::call_alert::{CallAlert, CallAlertEvent};
use crate::call_sounds::CallSounds;
use crate::call_window::{AccountNamer, CallWindows, CallWindowsEvent};
#[cfg(unix)]
use crate::notify::BusNotifications;
use crate::notify::{DEFAULT_ACTION, NotificationServer, Notifier, NotifierEvent};
use crate::own_presence::OwnPresence;
use crate::ringtone::Ringer;
use crate::settings_view::Page;
use crate::shell::{ListPlace, Main, Shell, ShellParts};
use crate::trace::{TraceLine, ViewTrace};
#[cfg(unix)]
use crate::tray::{BusTray, Hosted};
use crate::tray::{Tray, TrayEvent};

#[cfg(windows)]
mod windows;
use crate::view::Root;
use crate::window::{self, KitRoot};

/// What the desktop is made of.
#[derive(Clone)]
pub struct DesktopParts {
    /// The window's Wayland app id, and the tray item's id.
    pub app_id: &'static str,
    /// The app's name on the notifications and the tray item.
    pub app_name: SharedString,
    /// The window's title.
    pub title: SharedString,
    /// The first account's shell; more come with [`Desktop::add_account`].
    pub shell: ShellParts,
    /// The first account's name and what tells it apart (its UPN, the lab's alias), as the rail shows them.
    pub account_name: SharedString,
    pub account_detail: SharedString,
    /// Whether the app adds and removes accounts ([`crate::accounts::Accounts::can_add`]).
    pub can_add: bool,
    /// What plays an incoming call's ringtone (the app's speaker); `None` plays none.
    pub ringer: Option<Arc<dyn Ringer>>,
}

/// What the desktop asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Quit: the tray's "Quit", or the window closed without a tray item to stay in.
    Quit,
    /// The user asked to add an account (the rail's "+").
    AddAccount,
    /// The user asked to sign out of the account of this MRI and remove it (its account menu).
    RemoveAccount(String),
}

/// Where the main window is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowShown {
    /// Open.
    Shown,
    /// Closed while the tray item is up: the app runs on.
    Tray,
    /// Closed, with no tray item (a call's window keeps the app).
    Closed,
}

/// What the app shows, as [`Desktop::ui_state`] reads it (the control socket's `ui state` and `ui` event). The
/// observers of the desktop, its accounts, its call windows and the window's shells are told when it may have changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiShown {
    /// The account the window shows (or would, opened), by its MRI.
    pub account: Option<String>,
    /// What its shell shows right of the list, while the window is open.
    pub main: Option<Main>,
    /// The chat shown's name as the chat list gives it.
    pub title: Option<String>,
    pub window: WindowShown,
    /// A call's window is open.
    pub call_window: bool,
    /// The list pane shows the Activity pane.
    pub activity: bool,
}

/// One account's part of the desktop: its shell's parts, its notifications, its incoming calls' alert and its own
/// presence.
struct Slot {
    key: String,
    shell: ShellParts,
    notifier: Entity<Notifier>,
    call_alert: Entity<CallAlert>,
    /// The sounds of its calls: held for as long as the account is.
    _call_sounds: Entity<CallSounds>,
    own_presence: Entity<OwnPresence>,
    tray: Entity<Tray>,
    _subscriptions: [Subscription; 2],
}

/// The app on the desktop (see the module docs). A GPUI entity.
pub struct Desktop {
    parts: DesktopParts,
    window: Option<WindowHandle<KitRoot>>,
    /// The window's root view, read without the window: a click's handler runs while GPUI holds its window.
    root: Option<WeakEntity<Root>>,
    /// The accounts, the first one's slot first.
    slots: Vec<Slot>,
    accounts: Entity<Accounts>,
    /// The tray item's total: the first account's count, with the others' summed in.
    tray: Entity<Tray>,
    in_tray: bool,
    /// The connection to the bus the app is given, kept for the notifications and the tray item on it.
    #[cfg(unix)]
    bus: Option<Connection>,
    #[cfg(windows)]
    native_tray: Option<Arc<windows::tray::NativeTray>>,
    /// The bus's notification server, for an account added once it is up.
    server: Option<Arc<dyn NotificationServer>>,
    /// The call's own window.
    call_windows: Entity<CallWindows>,
    _subscriptions: [Subscription; 3],
}

// Public only in the library's test image: the integration fixture compiles the same transport source and uses
// these hooks. Reachability keeps the complete lib-test target lintable without suppressing unused test code.
#[cfg(all(test, windows))]
pub use windows::tray::NativeTray as NativeTrayTest;

impl EventEmitter<DesktopEvent> for Desktop {}

/// The shell `window` shows.
fn shown_shell(window: WindowHandle<KitRoot>, cx: &mut App) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// The chat shown in `window` for account `key`, if the window has the focus and shows that account.
fn chat_in_focus(window: WindowHandle<KitRoot>, key: &str, cx: &mut App) -> Option<String> {
    window
        .update(cx, |kit, window, cx| {
            if !window.is_window_active() {
                return None;
            }
            let root = kit.view().clone().downcast::<Root>().ok()?;
            let shell = root.read(cx).shell_in(cx)?.clone();
            let shell = shell.read(cx);
            if shell.account() != key {
                return None;
            }
            match shell.main() {
                Main::Chat(id) | Main::Channel(id) => Some(id.clone()),
                Main::Nothing
                | Main::Settings
                | Main::Calendar
                | Main::Search
                | Main::TeamsPage
                | Main::ManageTeam(_)
                | Main::JoinTeam
                | Main::CreateTeamMore
                | Main::Workflows
                | Main::NewChat => None,
            }
        })
        .ok()
        .flatten()
}

impl Desktop {
    /// The desktop over `parts`: its first account's notifier, call alert and own presence, the tray, no window yet
    /// ([`Desktop::open_window`]) and no bus ([`Desktop::connect`]).
    pub fn new(parts: DesktopParts, cx: &mut Context<Self>) -> Self {
        let shell = parts.shell.clone();
        let accounts = cx.new(|_| Accounts::new(parts.can_add, Arc::clone(&shell.trace)));
        let asking = cx.subscribe(&accounts, |_: &mut Self, _, event, cx| match event {
            AccountsEvent::AddAsked => cx.emit(DesktopEvent::AddAccount),
            AccountsEvent::RemoveAsked(key) => cx.emit(DesktopEvent::RemoveAccount(key.clone())),
            AccountsEvent::Switched(_) => {}
        });
        let desktop = cx.entity().downgrade();
        let closing = cx.on_window_closed(move |cx, id| {
            // a desktop that is gone has no window to watch
            let _ = desktop.update(cx, |this, cx| this.closed(id, cx));
        });
        let (call_windows, calling) = Self::make_call_windows(parts.app_id, cx);
        let first = Self::slot(&parts, shell.clone(), Arc::clone(&shell.trace), cx);
        let tray = first.tray.clone();
        let name = parts.account_name.clone();
        let detail = parts.account_detail.clone();
        let mut this = Self {
            parts,
            window: None,
            root: None,
            slots: Vec::new(),
            accounts,
            tray,
            in_tray: false,
            #[cfg(unix)]
            bus: None,
            #[cfg(windows)]
            native_tray: None,
            server: None,
            call_windows,
            _subscriptions: [asking, closing, calling],
        };
        this.push(first, name, detail, cx);
        this
    }

    /// The call windows of the app's windows' app id `app_id`, a group chat's call named as the main window's list
    /// names it, and what they ask followed.
    fn make_call_windows(app_id: &str, cx: &mut Context<Self>) -> (Entity<CallWindows>, Subscription) {
        // read from the windows themselves, never the desktop: a call window can draw while the desktop is updated
        let namer: AccountNamer = Rc::new(|key: &str, chat: &str, cx: &App| {
            cx.windows().into_iter().find_map(|window| {
                let root = window
                    .downcast::<KitRoot>()?
                    .read(cx)
                    .ok()?
                    .view()
                    .clone()
                    .downcast::<Root>()
                    .ok()?;
                let shell = root
                    .read(cx)
                    .shells()
                    .iter()
                    .find(|shell| shell.read(cx).account() == key)?
                    .clone();
                let list = shell.read(cx).list().read(cx);
                // a row named by people's names or a topic: an unnamed one leaves the call its participants' names
                list.row(chat).filter(|row| row.named).map(|row| row.name.clone())
            })
        });
        let windows = cx.new(|cx| {
            let mut windows = CallWindows::new(app_id, cx);
            windows.set_namer(namer);
            windows
        });
        let asked = cx.subscribe(&windows, |this: &mut Self, _, event, cx| match event {
            CallWindowsEvent::DeviceSettings(key) => {
                this.switch(key, cx);
                this.in_shell(cx, |shell, window, cx| {
                    shell.open_settings_page(Page::Calls, window, cx);
                });
            }
            CallWindowsEvent::Closed => {
                // a call window closed for another to open in its place keeps the app
                let open = this.call_windows.read(cx).window().is_some();
                if this.window.is_none() && !this.in_tray && !open {
                    this.quit(cx);
                }
            }
        });
        (windows, asked)
    }

    /// One account's slot over `shell`; its count traced to `trace` (the first account's: the item's total is traced
    /// from it).
    fn slot(parts: &DesktopParts, shell: ShellParts, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Slot {
        let key = shell.core.me().to_owned();
        let notifier = cx.new(|cx| {
            Notifier::new(
                Arc::clone(&shell.core),
                &shell.models,
                shell.settings.clone(),
                Arc::clone(&shell.trace),
                cx,
            )
        });
        let desktop = cx.entity().downgrade();
        let viewing = key.clone();
        notifier.update(cx, |notifier, _| {
            notifier.set_viewing(Box::new(move |cx| {
                let window = desktop.upgrade()?.read(cx).window?;
                chat_in_focus(window, &viewing, cx)
            }));
        });
        let opened = key.clone();
        let opening = cx.subscribe(&notifier, move |this: &mut Self, _, event, cx| match event {
            NotifierEvent::Open(id) => this.open_chat_of(&opened, id.clone(), cx),
            NotifierEvent::OpenChannel(id) => this.open_channel_of(&opened, id.clone(), cx),
            NotifierEvent::OpenActivity(item) => this.open_activity_item_of(&opened, item, cx),
            NotifierEvent::Shown { .. } => {}
        });
        let call_alert = cx.new(|cx| {
            CallAlert::new(
                Arc::clone(&shell.core),
                shell.models.calls.clone(),
                Arc::clone(&shell.trace),
                parts.ringer.clone(),
                cx,
            )
        });
        let shown = key.clone();
        let alerted = cx.subscribe(&call_alert, move |this: &mut Self, _, event, cx| match event {
            CallAlertEvent::ShowWindow => {
                this.switch(&shown, cx);
                this.show(cx);
            }
        });
        let own_presence = cx.new(|cx| {
            OwnPresence::new(
                Arc::clone(&shell.core),
                &shell.models,
                &shell.settings,
                Arc::clone(&shell.trace),
                cx,
            )
        });
        let tray = cx.new(|cx| Tray::new(Arc::clone(&shell.core), shell.models.clone(), trace, cx));
        let call_sounds =
            cx.new(|cx| CallSounds::new(key.clone(), shell.models.calls.clone(), shell.settings.clone(), cx));
        Slot {
            key,
            shell,
            notifier,
            call_alert,
            _call_sounds: call_sounds,
            own_presence,
            tray,
            _subscriptions: [opening, alerted],
        }
    }

    /// Adds `slot` to the desktop: to the accounts, to the item's total, to the window's shells and to the bus's
    /// notifications when they are up.
    fn push(&mut self, slot: Slot, name: SharedString, detail: SharedString, cx: &mut Context<Self>) {
        let account = AccountParts {
            core: Arc::clone(&slot.shell.core),
            models: slot.shell.models.clone(),
            name,
            detail,
        };
        let (tray, own, key) = (slot.tray.clone(), slot.own_presence.clone(), slot.key.clone());
        self.accounts.update(cx, |accounts, cx| {
            accounts.add(account, tray.clone(), cx);
            accounts.set_own_presence(&key, own, cx);
        });
        if tray != self.tray {
            self.tray.update(cx, |main, cx| main.add_account(tray, cx));
        }
        if let Some(server) = self.server.clone() {
            hand_server(&slot, &server, cx);
        }
        let parts = slot.shell.clone();
        self.call_windows.update(cx, |windows, cx| windows.add(parts, cx));
        if let Some(window) = self.window {
            let parts = slot.shell.clone();
            let (accounts, own) = (self.accounts.clone(), slot.own_presence.clone());
            let _ = window.update(cx, |kit, window, cx| {
                let root = kit.view().clone().downcast::<Root>().ok()?;
                let shell = cx.new(|cx| Shell::new(parts, window, cx));
                shell.update(cx, |shell, cx| {
                    shell.set_own_presence(own, cx);
                    shell.set_accounts(accounts, cx);
                });
                root.update(cx, |root, cx| root.add_shell(shell, cx));
                // the new shell took the keyboard as it was made: the one shown takes it back
                let shown = root.read(cx).shell_in(cx).cloned()?;
                shown.update(cx, |shell, cx| shell.take_focus(window, cx));
                Some(())
            });
        }
        self.slots.push(slot);
    }

    /// Adds an account the app started (its engine's request side, models, name and what tells it apart), sharing the
    /// first account's settings and trace. One already there is left as it is.
    pub fn add_account(&mut self, account: AccountParts, cx: &mut Context<Self>) {
        let key = account.core.me().to_owned();
        if self.slots.iter().any(|slot| slot.key == key) {
            return;
        }
        let first = &self.parts.shell;
        let shell = ShellParts {
            core: account.core,
            models: account.models,
            settings: first.settings.clone(),
            trace: Arc::clone(&first.trace),
        };
        let slot = Self::slot(&self.parts, shell, Arc::new(ViewTrace::off()), cx);
        self.push(slot, account.name, account.detail, cx);
    }

    /// Takes account `key` out (the app stops its engine): its own presence stopped (nothing more is reported), its
    /// shell out of the window, its count out of the item's total. Notifications it showed stay until they are
    /// dismissed; activating one then does nothing. The first account
    /// can't be removed while it is the only one.
    pub fn remove_account(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(index) = self.slots.iter().position(|slot| slot.key == key) else {
            return;
        };
        if self.slots.len() == 1 {
            return;
        }
        let slot = self.slots.remove(index);
        slot.own_presence.update(cx, OwnPresence::quit);
        self.accounts.update(cx, |accounts, cx| accounts.remove(key, cx));
        self.call_windows.update(cx, |windows, cx| windows.remove(key, cx));
        if slot.tray == self.tray {
            // the item's total moves to the next account's count, with the others' summed in
            let icon = self.tray.update(cx, |tray, _| tray.take_icon());
            self.tray = self.slots[0].tray.clone();
            let others: Vec<Entity<Tray>> = self.slots[1..].iter().map(|slot| slot.tray.clone()).collect();
            self.tray.update(cx, |tray, cx| {
                for other in others {
                    tray.add_account(other, cx);
                }
                if let Some(icon) = icon {
                    tray.set_icon(icon, cx);
                }
            });
        } else {
            self.tray.update(cx, |tray, cx| tray.remove_account(&slot.tray, cx));
        }
        if let Some(window) = self.window {
            let _ = window.update(cx, |kit, _, cx| {
                let root = kit.view().clone().downcast::<Root>().ok()?;
                root.update(cx, |root, cx| root.remove_shell(key, cx));
                Some(())
            });
        }
    }

    /// The accounts the rail shows.
    #[must_use]
    pub fn accounts(&self) -> &Entity<Accounts> {
        &self.accounts
    }

    /// The first account's notifier.
    #[must_use]
    pub fn notifier(&self) -> &Entity<Notifier> {
        &self.slots[0].notifier
    }

    /// Account `key`'s notifier.
    #[must_use]
    pub fn notifier_of(&self, key: &str) -> Option<&Entity<Notifier>> {
        self.slots
            .iter()
            .find(|slot| slot.key == key)
            .map(|slot| &slot.notifier)
    }

    /// The first account's incoming call's ringtone and notification.
    #[must_use]
    pub fn call_alert(&self) -> &Entity<CallAlert> {
        &self.slots[0].call_alert
    }

    /// What holds an update's restart up now ([`crate::update::Waiting`]): any account's call, up or ringing, then a
    /// message any account's box holds unsent in the window.
    #[must_use]
    pub fn update_waiting(&self, cx: &App) -> Option<crate::update::Waiting> {
        if self.slots.iter().any(|slot| slot.shell.models.calls.read(cx).busy()) {
            return Some(crate::update::Waiting::Call);
        }
        let unsent = self
            .root
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .is_some_and(|root| root.read(cx).shells().iter().any(|shell| shell.read(cx).has_unsent(cx)));
        unsent.then_some(crate::update::Waiting::Unsent)
    }

    /// The tray item's total.
    #[must_use]
    pub fn tray(&self) -> &Entity<Tray> {
        &self.tray
    }

    /// The first account's own presence as the app reports it.
    #[must_use]
    pub fn own_presence(&self) -> &Entity<OwnPresence> {
        &self.slots[0].own_presence
    }

    /// Account `key`'s own presence as the app reports it.
    #[must_use]
    pub fn own_presence_of(&self, key: &str) -> Option<&Entity<OwnPresence>> {
        self.slots
            .iter()
            .find(|slot| slot.key == key)
            .map(|slot| &slot.own_presence)
    }

    /// The settings every window of the app shows.
    #[must_use]
    pub fn settings(&self) -> &Entity<crate::SettingsModel> {
        &self.parts.shell.settings
    }

    /// The window, while it is open.
    #[must_use]
    pub fn window(&self) -> Option<WindowHandle<KitRoot>> {
        self.window
    }

    /// The call's own window.
    #[must_use]
    pub fn call_windows(&self) -> &Entity<CallWindows> {
        &self.call_windows
    }

    /// Whether the tray item is up, so closing the window keeps the app.
    #[must_use]
    pub fn in_tray(&self) -> bool {
        self.in_tray
    }

    /// What the app shows now ([`UiShown`]).
    #[must_use]
    pub fn ui_state(&self, cx: &App) -> UiShown {
        let account = self.accounts.read(cx).active_key().map(str::to_owned);
        let shell = self.window.and_then(|window| {
            let root = window.read(cx).ok()?.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        });
        let (main, title, activity) = shell.map_or((None, None, false), |shell| {
            let shell = shell.read(cx);
            let main = shell.main().clone();
            let title = match &main {
                Main::Chat(id) => shell.list().read(cx).row(id).map(|row| row.name.to_string()),
                _ => None,
            };
            (Some(main), title, shell.list_place() == ListPlace::Activity)
        });
        let window = match (self.window, self.in_tray) {
            (Some(_), _) => WindowShown::Shown,
            (None, true) => WindowShown::Tray,
            (None, false) => WindowShown::Closed,
        };
        UiShown {
            account,
            main,
            title,
            window,
            call_window: self.call_windows.read(cx).window().is_some(),
            activity,
        }
    }

    /// The tray item is up (a watcher has it), or down again (its watcher left, or the bus was lost): closing the
    /// window keeps the app, or quits it. Down while the window is closed, the window opens again.
    pub fn set_in_tray(&mut self, in_tray: bool, cx: &mut Context<Self>) {
        if self.in_tray == in_tray {
            return;
        }
        self.in_tray = in_tray;
        let change = if in_tray { "registered" } else { "gone" };
        self.parts.shell.trace.record(&TraceLine::new("tray", change));
        cx.notify();
        if !in_tray && self.window.is_none() {
            self.show(cx);
        }
    }

    /// Opens the main window ([`window::open_accounts`]) with every account's shell, the one the accounts show in
    /// front; the one open already, if it is.
    ///
    /// # Errors
    /// The platform can't open a window.
    pub fn open_window(&mut self, cx: &mut Context<Self>) -> gpui::Result<WindowHandle<KitRoot>> {
        if let Some(window) = self.window {
            return Ok(window);
        }
        let DesktopParts { app_id, title, .. } = self.parts.clone();
        let mut shells = self
            .slots
            .iter()
            .map(|slot| (slot.shell.clone(), slot.own_presence.clone()));
        // the first account's slot is never removed while it is the only one: there is always a first
        let window = match shells.next() {
            Some(first) => {
                let rest = shells.collect();
                window::open_accounts(cx, app_id, title, first, rest, &self.accounts)?
            }
            None => window::open(cx, app_id, title, self.parts.shell.clone())?,
        };
        self.window = Some(window);
        self.root = window
            .update(cx, |kit, _, _| kit.view().clone().downcast::<Root>().ok())
            .ok()
            .flatten()
            .map(|root| root.downgrade());
        #[cfg(windows)]
        self.sync_native_window(cx);
        cx.notify();
        Ok(window)
    }

    /// Shows the window: brought to the front, or opened again if it was closed. A window that can't be opened
    /// leaves the app as it is (still in the tray).
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if let Some(window) = self.window {
            // a window closing as it is asked for is opened again by the next ask
            let _ = window.update(cx, |_, window, _| window.activate_window());
            return;
        }
        let _ = self.open_window(cx);
    }

    /// Shows account `key` in the window.
    pub fn switch(&mut self, key: &str, cx: &mut Context<Self>) {
        self.accounts.update(cx, |accounts, cx| accounts.switch(key, cx));
    }

    /// Opens chat `id` of the first account in the window, shown first ([`Desktop::show`]).
    pub fn open_chat(&mut self, id: String, cx: &mut Context<Self>) {
        let key = self.slots[0].key.clone();
        self.open_chat_of(&key, id, cx);
    }

    /// Opens chat `id` of account `key` in the window, that account shown, the window first ([`Desktop::show`]).
    pub fn open_chat_of(&mut self, key: &str, id: String, cx: &mut Context<Self>) {
        self.switch(key, cx);
        self.in_shell(cx, |shell, window, cx| shell.open_chat(id, window, cx));
    }

    /// Opens team channel `id` of the first account in the window, shown first ([`Desktop::show`]).
    pub fn open_channel(&mut self, id: String, cx: &mut Context<Self>) {
        let key = self.slots[0].key.clone();
        self.open_channel_of(&key, id, cx);
    }

    /// Opens team channel `id` of account `key` in the window, that account shown.
    pub fn open_channel_of(&mut self, key: &str, id: String, cx: &mut Context<Self>) {
        self.switch(key, cx);
        self.in_shell(cx, |shell, window, cx| shell.open_channel_id(id, window, cx));
    }

    /// Shows account `key`'s calendar in the window, that account shown, as the rail's Calendar does.
    pub fn show_calendar_of(&mut self, key: &str, cx: &mut Context<Self>) {
        self.switch(key, cx);
        self.in_shell(cx, Shell::show_calendar);
    }

    /// Shows account `key`'s Activity pane in the window, that account shown, as the rail's Activity shows it.
    pub fn show_activity_of(&mut self, key: &str, cx: &mut Context<Self>) {
        self.switch(key, cx);
        self.in_shell(cx, Shell::show_activity);
    }

    /// Shows account `key`'s Activity pane at its item `item`, opened as a click on it there opens it.
    pub fn open_activity_item_of(&mut self, key: &str, item: &str, cx: &mut Context<Self>) {
        self.switch(key, cx);
        self.in_shell(cx, |shell, window, cx| {
            shell.show_activity(window, cx);
            shell.activity_view().clone().update(cx, |view, cx| view.open(item, cx));
        });
    }

    /// Shows account `key`'s settings in the window on `page`, or where they were left without one, that account shown,
    /// the keyboard on their page column as the rail's Settings puts it.
    pub fn open_settings_of(&mut self, key: &str, page: Option<Page>, cx: &mut Context<Self>) {
        self.switch(key, cx);
        self.in_shell(cx, |shell, window, cx| match page {
            Some(page) => shell.open_settings_page(page, window, cx),
            None => shell.open_settings(window, cx),
        });
    }

    /// Shows the search results of `query` for account `key` in the window, that account shown, as the title bar's
    /// search shows them once typed and entered.
    pub fn search_of(&mut self, key: &str, query: &str, cx: &mut Context<Self>) {
        self.switch(key, cx);
        self.in_shell(cx, |shell, window, cx| shell.search_for(query, window, cx));
    }

    /// Shows the window and does `then` in the shell it shows.
    fn in_shell(&mut self, cx: &mut Context<Self>, then: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>)) {
        self.show(cx);
        let Some(window) = self.window else { return };
        let Some(shell) = shown_shell(window, cx) else { return };
        let _ = window.update(cx, |_, window, cx| {
            shell.update(cx, |shell, cx| then(shell, window, cx));
        });
    }

    fn closed(&mut self, id: WindowId, cx: &mut Context<Self>) {
        if self.window.is_none_or(|window| window.window_id() != id) {
            return;
        }
        self.window = None;
        self.root = None;
        #[cfg(windows)]
        self.sync_native_window(cx);
        self.parts.shell.trace.record(&TraceLine::new("window", "closed"));
        cx.notify();
        // a call's window keeps the app until it closes too
        let call_window = self.call_windows.read(cx).window().is_some();
        log::info!(
            "the main window closed (in the tray: {}, a call window open: {call_window})",
            self.in_tray
        );
        if !self.in_tray && !call_window {
            log::info!("the app quits: its main window closed");
            self.quit(cx);
        }
    }

    /// Asks the app to quit, every account's own presence stopped first: nothing more is reported (and nothing is
    /// sent for the quit: no captured request reports Offline, M2 Task 20).
    fn quit(&mut self, cx: &mut Context<Self>) {
        for slot in &self.slots {
            slot.own_presence.update(cx, OwnPresence::quit);
        }
        cx.emit(DesktopEvent::Quit);
    }

    /// What the tray item asked: the window shown, or the app quit.
    pub fn tray_asked(&mut self, event: TrayEvent, cx: &mut Context<Self>) {
        match event {
            TrayEvent::Open => {
                self.parts.shell.trace.record(&TraceLine::new("tray", "open"));
                self.show(cx);
            }
            TrayEvent::Quit => {
                self.parts.shell.trace.record(&TraceLine::new("tray", "quit"));
                log::info!("the app quits: the tray's Quit");
                self.quit(cx);
            }
        }
    }

    /// Connects to the bus at `address` (the one the app is given, [`bus::address_from_env`]) and puts the
    /// notifications and the tray item on it. The task ends once both are up or have failed, with what failed (the
    /// app reports it); without an address nothing is connected to.
    #[cfg(unix)]
    pub fn connect(&mut self, address: Result<String, BusError>, cx: &mut Context<Self>) -> Task<Vec<String>> {
        let app_id = self.parts.app_id;
        let app_name = self.parts.app_name.to_string();
        // the read-only mode shows no tray item: closing its window quits it
        let with_tray = !self.parts.shell.core.read_only();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let connection = match bus::connect_given(address).await {
                Ok(connection) => connection,
                Err(error) => return vec![error.to_string()],
            };
            let mut problems = Vec::new();
            if let Err(error) = notifications(&this, &connection, app_id, &app_name, cx).await {
                problems.push(format!("no notifications: {error}"));
            }
            if with_tray && let Err(error) = tray(&this, &connection, app_id, &app_name, cx).await {
                problems.push(format!("no tray item: {error}"));
            }
            let _ = this.update(cx, |this, _| this.bus = Some(connection));
            problems
        })
    }

    /// Connects to Windows' shell in this process's interactive session. D-Bus addresses have no role here.
    #[cfg(windows)]
    pub fn connect(&mut self, address: Result<String, BusError>, cx: &mut Context<Self>) -> Task<Vec<String>> {
        // A deterministic GPUI model test must never post into the machine's real shell. Native fixtures use
        // their explicit VM-console runner and the actual shell transports instead.
        if cx.is_test() {
            let problem = address.err().map_or_else(
                || "native desktop is not opened by a deterministic UI test".to_owned(),
                |error| error.to_string(),
            );
            return cx.spawn(async move |_, _| vec![problem]);
        }
        let (app_id, app_name) = (self.parts.app_id.to_owned(), self.parts.app_name.to_string());
        let with_tray = !self.parts.shell.core.read_only();
        cx.spawn(async move |this, cx: &mut AsyncApp| windows::connect(this, app_id, app_name, with_tray, cx).await)
    }

    /// Keep the native taskbar target current when closing to tray and opening a new GPUI window.
    #[cfg(windows)]
    fn sync_native_window(&self, cx: &mut App) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = self.window.and_then(|handle| {
            handle
                .update(cx, |_, window, _| {
                    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
                        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get().cast_unsigned()),
                        _ => None,
                    }
                })
                .ok()
                .flatten()
        });
        if let Some(tray) = &self.native_tray {
            tray.set_window(hwnd);
        }
    }

    /// Hands a notification server (the bus's, once it is up; a test's fake) to every account's notifier and call
    /// alert, and to the accounts added later.
    pub fn use_server(&mut self, server: Arc<dyn NotificationServer>, cx: &mut Context<Self>) {
        for slot in &self.slots {
            hand_server(slot, &server, cx);
        }
        self.server = Some(server);
    }

    /// Notification `id` had `action` invoked: the call alert's it is, else (its body) the notifier's it is.
    fn activated(&mut self, id: u32, action: &str, cx: &mut Context<Self>) {
        for slot in &self.slots {
            if slot.call_alert.update(cx, |alert, cx| alert.activated(id, action, cx)) {
                return;
            }
        }
        if action != DEFAULT_ACTION {
            return;
        }
        let notifiers: Vec<Entity<Notifier>> = self.slots.iter().map(|slot| slot.notifier.clone()).collect();
        for notifier in notifiers {
            notifier.update(cx, |notifier, cx| notifier.activated(id, cx));
        }
    }
}

/// Hands `server` to `slot`'s notifier and call alert.
fn hand_server(slot: &Slot, server: &Arc<dyn NotificationServer>, cx: &mut App) {
    slot.notifier
        .update(cx, |notifier, _| notifier.set_server(Arc::clone(server)));
    slot.call_alert
        .update(cx, |alert, cx| alert.set_server(Arc::clone(server), cx));
}

/// The notifications on `connection`: the server handed to every account's notifier and call alert, each activation
/// to the call alert whose it is, else (its body's) to the notifier whose it is.
#[cfg(unix)]
async fn notifications(
    this: &WeakEntity<Desktop>,
    connection: &Connection,
    app_id: &str,
    app_name: &str,
    cx: &mut AsyncApp,
) -> zbus::Result<()> {
    let (server, mut activations) = BusNotifications::start(connection, app_name, app_id).await?;
    let server: Arc<dyn NotificationServer> = Arc::new(server);
    if this.update(cx, |this, cx| this.use_server(server, cx)).is_err() {
        return Ok(());
    }
    let this = this.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some((id, action)) = activations.next().await {
            if this.update(cx, |this, cx| this.activated(id, &action, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    Ok(())
}

/// The tray item on `connection`: shown the unread total, its asks handed to the desktop.
#[cfg(unix)]
async fn tray(
    this: &WeakEntity<Desktop>,
    connection: &Connection,
    app_id: &str,
    app_name: &str,
    cx: &mut AsyncApp,
) -> zbus::Result<()> {
    let (events, mut asked) = mpsc::unbounded_channel();
    let (item, mut hosted) = BusTray::start(connection, app_id, app_name, events).await?;
    let _ = this.update(cx, |this, cx| {
        this.set_in_tray(true, cx);
        this.tray.update(cx, |tray, cx| tray.set_icon(Arc::new(item), cx));
    });
    let asking = this.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(event) = asked.recv().await {
            if asking.update(cx, |this, cx| this.tray_asked(event, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    let this = this.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(change) = hosted.next().await {
            let up = change == Hosted::Registered;
            if this.update(cx, |this, cx| this.set_in_tray(up, cx)).is_err() {
                return;
            }
        }
        // the bus is lost: the item with it
        let _ = this.update(cx, |this, cx| this.set_in_tray(false, cx));
    })
    .detach();
    Ok(())
}
