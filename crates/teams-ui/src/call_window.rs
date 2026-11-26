//! The call's own window ([`CallWindows`]), as Teams opens one for a call: the main window's chats stay usable while it
//! runs. One window at most, whichever account the call is (one call at a time across the accounts: the call floor).
//!
//! - **It opens** once a call of any account is told (a start, a join, the answer of an invitation; not a decline),
//!   brought to the front, the keyboard on it; what it shows is [`CallStage`]'s.
//! - **It closes** once its call ended by the user's own hang-up (or decline). A call that ended any other way (the
//!   far end hung up, it dropped or never connected) keeps its window on how it ended ([`CallStage`]: who it was
//!   with, why it ended in plain words, how long it lasted, Call back or Rejoin, Close) until it is closed, by Close,
//!   the window's own close or by itself a few seconds later; a test call's window stays on its results until it is
//!   closed, as Teams' test call does. A new call takes the window of the last call's end.
//! - **Closing it while the call is up leaves the call** (one hang-up, as the window's Leave asks), as closing Teams'
//!   call window does, without asking. It isn't opened again for that call by itself; the main window's "Return to
//!   call" ([`raise`]) brings it back while the call is still up (its leave refused, say), and else brings the open
//!   window to the front.
//! - **The lab app** opens it on the same display as its main window: inside the UI sandbox, where its main window is.
//!
//! - **A meeting's pre-join** ([`PrejoinView`]) has its own window of the same size, open while the pre-join shows and
//!   no call is up: Join now's call takes over in the call window, and closing it is Cancel.
//!
//! What it does is traced on its account's trace: `call-window opened` (the call's number and kind), `call-window
//! raised`, `call-window closed` (the call's number, and `left` when its closing left the call).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, Context, Entity, EventEmitter, Global, Subscription, TitlebarOptions, WeakEntity,
    WindowBounds, WindowHandle, WindowId, WindowOptions, px, size,
};
use teams_core::{CallId, CallStatus};

use crate::call_stage::{CallStage, CallStageEvent, ChatNamer, StageParts};
use crate::calls::{self, Ask, Calls};
use crate::prejoin::{PrejoinParts, PrejoinView};
use crate::shell::ShellParts;
use crate::trace::TraceLine;
use crate::window::KitRoot;

/// The call window's size when it opens.
const DEFAULT_SIZE: (f32, f32) = (1000.0, 680.0);
/// The smallest it gets.
const MIN_SIZE: (f32, f32) = (560.0, 400.0);

/// What names a group chat for an account and draws its avatar: its key (its MRI), the chat, the app; the main
/// window's chat list's row of it.
pub type AccountNamer = Rc<dyn Fn(&str, &str, &App) -> Option<crate::chat_list::ChatRow>>;

/// What the call windows ask of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallWindowsEvent {
    /// "Device settings" in the window of the account of this MRI: the main window's settings on their Calls page.
    DeviceSettings(String),
    /// The window closed.
    Closed,
}

/// One account the windows follow.
struct Followed {
    key: String,
    parts: ShellParts,
    _observing: Subscription,
}

/// The window open, for which account's call.
struct Open {
    key: String,
    call: CallId,
    window: WindowHandle<KitRoot>,
    _events: Subscription,
}

/// The pre-join's window open, for which account.
struct OpenPrejoin {
    key: String,
    window: WindowHandle<KitRoot>,
}

/// The app's call window (see the module docs). A GPUI entity the desktop holds.
pub struct CallWindows {
    app_id: String,
    accounts: Vec<Followed>,
    open: Option<Open>,
    prejoin: Option<OpenPrejoin>,
    /// The accounts whose pre-join showed when the calls last changed: one that shows since is the one opened last.
    prejoins: Vec<String>,
    /// The call whose window the user closed: not opened again by itself.
    closed: Option<(String, CallId)>,
    namer: Option<AccountNamer>,
    _closing: Subscription,
}

/// The app's call windows, for [`raise`].
struct Raiser(WeakEntity<CallWindows>);

impl Global for Raiser {}

impl EventEmitter<CallWindowsEvent> for CallWindows {}

/// Whether the app has call windows: the end of a call is theirs to tell, not the main window's.
#[must_use]
pub fn present(cx: &App) -> bool {
    cx.try_global::<Raiser>()
        .is_some_and(|raiser| raiser.0.upgrade().is_some())
}

/// Whether a call window shows `calls`' call `call`, its end included: the end of a call that no window showed (a
/// ring that failed before its answer) is the main window's to tell.
#[must_use]
pub fn shows(calls: &Entity<Calls>, call: CallId, cx: &App) -> bool {
    let Some(windows) = cx.try_global::<Raiser>().and_then(|raiser| raiser.0.upgrade()) else {
        return false;
    };
    let windows = windows.read(cx);
    windows
        .open
        .as_ref()
        .is_some_and(|open| open.call == call && windows.calls_of(&open.key).is_some_and(|of| of == calls))
}

/// Brings the call window to the front ("Return to call"), opened again when its call is still up and it was closed;
/// nothing in an app without call windows.
pub fn raise(cx: &mut App) {
    let Some(windows) = cx.try_global::<Raiser>().and_then(|raiser| raiser.0.upgrade()) else {
        return;
    };
    windows.update(cx, CallWindows::raise);
}

impl CallWindows {
    /// The call windows of an app whose windows have app id `app_id`, following no account yet; the app's
    /// [`raise`] goes to them.
    pub fn new(app_id: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let raiser = Raiser(cx.weak_entity());
        cx.set_global(raiser);
        let windows = cx.weak_entity();
        let closing = cx.on_window_closed(move |cx, id| {
            // windows that are gone have nothing open to follow
            let _ = windows.update(cx, |this, cx| this.window_gone(id, cx));
        });
        Self {
            app_id: app_id.into(),
            accounts: Vec::new(),
            open: None,
            prejoin: None,
            prejoins: Vec::new(),
            closed: None,
            namer: None,
            _closing: closing,
        }
    }

    /// Names a group chat's call by `namer` from now on (the main window's chat list's row).
    pub fn set_namer(&mut self, namer: AccountNamer) {
        self.namer = Some(namer);
    }

    /// Follows the calls of the account `parts` are; one followed already is left as it is.
    pub fn add(&mut self, parts: ShellParts, cx: &mut Context<Self>) {
        let key = parts.core.me().to_owned();
        if self.accounts.iter().any(|account| account.key == key) {
            return;
        }
        let observing = cx.observe(&parts.models.calls, |this: &mut Self, _, cx| this.follow(cx));
        self.accounts.push(Followed {
            key,
            parts,
            _observing: observing,
        });
        self.follow(cx);
    }

    /// Follows account `key` no more; its window closes (its engine stopping ends its call).
    pub fn remove(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.key == key) {
            self.close(cx);
        }
        self.accounts.retain(|account| account.key != key);
    }

    /// The window, while it is open.
    #[must_use]
    pub fn window(&self) -> Option<WindowHandle<KitRoot>> {
        self.open.as_ref().map(|open| open.window)
    }

    /// The account and the call the window is open for.
    #[must_use]
    pub fn showing(&self) -> Option<(&str, CallId)> {
        self.open.as_ref().map(|open| (open.key.as_str(), open.call))
    }

    fn calls_of(&self, key: &str) -> Option<&Entity<Calls>> {
        self.accounts
            .iter()
            .find(|account| account.key == key)
            .map(|account| &account.parts.models.calls)
    }

    /// The pre-join's window follows the calls: open while one shows and no call is up. A ring's pre-join comes
    /// first, as its ring is silent while it shows; then the one opened last.
    fn follow_prejoin(&mut self, call_up: bool, cx: &mut Context<Self>) {
        let showing: Vec<String> = self
            .accounts
            .iter()
            .filter(|account| account.parts.models.calls.read(cx).prejoin().is_some())
            .map(|account| account.key.clone())
            .collect();
        let wanted = (!call_up).then(|| self.prejoin_wanted(&showing, cx)).flatten();
        self.prejoins = showing;
        if self.prejoin.as_ref().map(|open| open.key.as_str()) == wanted.as_deref() {
            return;
        }
        if let Some(open) = self.prejoin.take() {
            remove(open.window, cx);
        }
        if let Some(key) = wanted {
            self.open_prejoin(&key, cx);
        }
    }

    /// The account whose pre-join the window shows, of the accounts `showing` one (see [`Self::follow_prejoin`]).
    fn prejoin_wanted(&self, showing: &[String], cx: &App) -> Option<String> {
        let open = self.prejoin.as_ref().map(|open| &open.key);
        self.accounts
            .iter()
            .find(|account| account.parts.models.calls.read(cx).ring_in_prejoin())
            .map(|account| &account.key)
            .or_else(|| showing.iter().find(|key| !self.prejoins.contains(key)))
            .or_else(|| showing.iter().find(|key| Some(*key) == open))
            .or_else(|| showing.first())
            .cloned()
    }

    /// Opens the pre-join's window of account `key`, the keyboard on it.
    fn open_prejoin(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(account) = self.accounts.iter().find(|account| account.key == key) else {
            return;
        };
        let parts = account.parts.clone();
        let view_parts = PrejoinParts {
            core: Arc::clone(&parts.core),
            calls: parts.models.calls.clone(),
            settings: parts.settings.clone(),
            trace: Arc::clone(&parts.trace),
        };
        let options = self.options(cx);
        let calls = parts.models.calls.clone();
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| PrejoinView::new(view_parts, cx));
            view.read(cx).focus_handle().focus(window, cx);
            window.on_window_should_close(cx, move |_, cx| {
                calls.update(cx, Calls::cancel_prejoin);
                true
            });
            cx.new(|cx| KitRoot::new(view, window, cx))
        });
        let Ok(window) = opened else { return };
        self.prejoin = Some(OpenPrejoin {
            key: key.to_owned(),
            window,
        });
    }

    /// The pre-join's window that shows, for a test to read.
    #[must_use]
    pub fn prejoin_window(&self) -> Option<WindowHandle<KitRoot>> {
        self.prejoin.as_ref().map(|open| open.window)
    }

    /// The calls changed: the window opens for a call up (closing one of another call), and closes once its call
    /// ended by the user's hang-up (one that ended otherwise stays on its end, a test call's on its results).
    fn follow(&mut self, cx: &mut Context<Self>) {
        // an invitation being declined is no call to show
        let up = self.accounts.iter().find_map(|account| {
            let active = account.parts.models.calls.read(cx).call()?;
            (active.status != CallStatus::Declining).then(|| (account.key.clone(), active.id))
        });
        self.follow_prejoin(up.is_some(), cx);
        let Some((key, call)) = up else {
            self.closed = None;
            let stays = self.open.as_ref().is_some_and(|open| {
                self.calls_of(&open.key)
                    .and_then(|calls| calls.read(cx).ended())
                    .is_some_and(|ended| ended.id == open.call && ended.stays_in_window())
            });
            if !stays {
                self.close(cx);
            }
            return;
        };
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.key == key && open.call == call)
        {
            return;
        }
        self.close(cx);
        if self.closed.as_ref() != Some(&(key.clone(), call)) {
            self.open_for(&key, call, cx);
        }
    }

    /// Opens the window of account `key`'s call `call`, the keyboard on it.
    fn open_for(&mut self, key: &str, call: CallId, cx: &mut Context<Self>) {
        let Some(account) = self.accounts.iter().find(|account| account.key == key) else {
            return;
        };
        let parts = account.parts.clone();
        let namer: Option<ChatNamer> = self.namer.clone().map(|namer| {
            let key = key.to_owned();
            Rc::new(move |chat: &str, cx: &App| namer(&key, chat, cx)) as ChatNamer
        });
        let kind = parts
            .models
            .calls
            .read(cx)
            .call()
            .map_or("chat", |active| active.kind.name());
        let stage_parts = StageParts {
            core: Arc::clone(&parts.core),
            calls: parts.models.calls.clone(),
            settings: parts.settings.clone(),
            trace: Arc::clone(&parts.trace),
            namer,
            others: self
                .accounts
                .iter()
                .filter(|other| other.key != key)
                .map(|other| other.parts.models.calls.clone())
                .collect(),
            shell: Some(parts.clone()),
        };
        let windows = cx.weak_entity();
        let mut made: Option<Entity<CallStage>> = None;
        let options = self.options(cx);
        let opened = cx.open_window(options, |window, cx| {
            let stage = cx.new(|cx| CallStage::new(stage_parts, call, cx));
            stage.read(cx).focus_handle().focus(window, cx);
            window.on_window_should_close(cx, move |_, cx| {
                // the window's own close leaves the call, as Teams' does; the platform takes the window away
                let _ = windows.update(cx, |this, cx| this.closed_by_user(false, cx));
                true
            });
            made = Some(stage.clone());
            cx.new(|cx| KitRoot::new(stage, window, cx))
        });
        let (Ok(window), Some(stage)) = (opened, made) else {
            return;
        };
        let key = key.to_owned();
        let asked = key.clone();
        let events = cx.subscribe(&stage, move |this: &mut Self, _, event, cx| match event {
            CallStageEvent::DeviceSettings => cx.emit(CallWindowsEvent::DeviceSettings(asked.clone())),
            CallStageEvent::Close => this.closed_by_user(true, cx),
        });
        parts
            .trace
            .record(&TraceLine::new("call-window", "opened").call(call.get()).kind(kind));
        self.open = Some(Open {
            key,
            call,
            window,
            _events: events,
        });
        cx.notify();
    }

    /// How the window opens: centred, Teams' call window's size, the app's app id.
    fn options(&self, cx: &App) -> WindowOptions {
        let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Call".into()),
                ..TitlebarOptions::default()
            }),
            app_id: Some(self.app_id.clone()),
            window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
            ..WindowOptions::default()
        }
    }

    /// Closes the window, if one is open.
    fn close(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open.take() else { return };
        self.trace_closed(&open, false);
        remove(open.window, cx);
        cx.emit(CallWindowsEvent::Closed);
        cx.notify();
    }

    /// The user closed the window (its own close, or the Close of a call's end or a test call's results, which takes
    /// it away: `remove`): the call left while it is up and not opened again for it, how it ended dismissed.
    fn closed_by_user(&mut self, take_away: bool, cx: &mut Context<Self>) {
        let Some(open) = self.open.take() else { return };
        let Some(account) = self.accounts.iter().find(|account| account.key == open.key) else {
            return;
        };
        let parts = account.parts.clone();
        let calls = parts.models.calls.clone();
        let up = calls.read(cx).call().is_some_and(|active| active.id == open.call);
        self.trace_closed(&open, up);
        if up {
            self.closed = Some((open.key.clone(), open.call));
            calls::ask(&calls, &parts.core, &parts.trace, Ask::HangUp, cx);
        } else {
            calls.update(cx, Calls::dismiss);
        }
        if take_away {
            remove(open.window, cx);
        }
        cx.emit(CallWindowsEvent::Closed);
        cx.notify();
    }

    /// A window went away: the call's, without being asked to here, is as the user closing it.
    fn window_gone(&mut self, id: WindowId, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| open.window.window_id() == id) {
            self.closed_by_user(false, cx);
        }
    }

    fn trace_closed(&self, open: &Open, left: bool) {
        let Some(account) = self.accounts.iter().find(|account| account.key == open.key) else {
            return;
        };
        let mut line = TraceLine::new("call-window", "closed").call(open.call.get());
        if left {
            line = line.kind("left");
        }
        account.parts.trace.record(&line);
    }

    /// The pre-join's window to the front, while it is open.
    pub fn raise_prejoin(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = &self.prejoin {
            let _ = open.window.update(cx, |_, window, _| window.activate_window());
        }
    }

    /// "Return to call": the window to the front; opened again when its call is still up and the user closed it.
    fn raise(&mut self, cx: &mut Context<Self>) {
        if let Some(open) = &self.prejoin {
            let _ = open.window.update(cx, |_, window, _| window.activate_window());
            return;
        }
        if let Some(open) = &self.open {
            if let Some(account) = self.accounts.iter().find(|account| account.key == open.key) {
                account
                    .parts
                    .trace
                    .record(&TraceLine::new("call-window", "raised").call(open.call.get()));
            }
            let _ = open.window.update(cx, |_, window, _| window.activate_window());
            return;
        }
        self.closed = None;
        self.follow(cx);
    }
}

/// Takes `window` away once the update under way is over: the platform tells its closing at once, and the windows
/// that close it hear of it.
fn remove(window: WindowHandle<KitRoot>, cx: &mut App) {
    cx.defer(move |cx| {
        // a window gone already has nothing to take away
        let _ = window.update(cx, |_, window, _| window.remove_window());
    });
}
