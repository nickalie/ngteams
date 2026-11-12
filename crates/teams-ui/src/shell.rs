//! The shell: the window's three panes, Teams-like and compact. On the left the rail (Chat, and Settings at its foot),
//! then the list pane (the [`Sidebar`]: the [`ChatList`]'s chats and the [`TeamsList`]'s teams with their channels,
//! M2 Task 16, in sections of one list), resizable by the handle on its right edge, then the conversation pane (the
//! chat opened in a [`ChatView`] under its avatar and name, and a 1:1 chat's other member's presence, M2 Task 17; a
//! channel opened in a [`ChannelView`] under its team's tile and its name; with nothing open, what to do).
//! Settings take the place of both the list and the conversation pane, as in Teams (their pages are their own list);
//! the rail's Chat leaves them for what they hid.
//!
//! The account menu (M2 Task 20): the rail's status button, above Settings, shows what the account's own presence
//! reports ([`OwnPresence`], handed in by the desktop) and opens a menu of the statuses the user may set (Automatic and
//! the captured manual ones, the same setting the settings view changes) with the last failed report; a status chosen
//! there goes to the own presence too, so choosing again one whose report failed sends it again. The shell's mouse
//! input and every keystroke in its window (the ones a key binding takes included) tell the own presence the window is
//! used.
//!
//! The title bar ([`title_bar`]) runs across the top, as Teams': the launcher, the list pane's toggle, Back and Forward
//! (Alt+Left, Alt+Right), the search box ([`GlobalSearch`]), "Settings and more" and the account's avatar, which opens
//! the account menu under it.
//!
//! The keyboard: Ctrl+Alt+Down and Ctrl+Alt+Up show the app's next and previous account ([`crate::accounts`]); Ctrl+K
//! opens the [`Switcher`] over the panes (and closes it again); a chat chosen there or clicked in
//! the list opens in the conversation pane and is kept as the selected chat in the settings, so the app opens it again
//! at its next start; a channel chosen there opens as a channel, and a "Set status: …" command chosen there sets the
//! account's status as the account menu does. Ctrl+E (and Ctrl+Shift+F) puts the keyboard in the title bar's search,
//! Ctrl+Alt+G in its "go to" mode; its results take the place of the panes (`Main::Search`), and a message found opens
//! its chat at the message ([`ChatView::reveal`]). What the
//! shell shows is traced (`chat opened`, `channel opened`, `settings opened`, `switcher opened`/`closed`, the search's
//! own lines, `status-menu opened`/`closed`/`chosen` with the status's key, and `header shown`: the chat's
//! header's presence and whether its name and avatar came from people's names and a picture, never the name).
//!
//! New message (Ctrl+N, the list header's pen; [`crate::new_chat`]) takes the conversation pane with its row at the
//! top of the chats (`Main::NewChat`); a chat it names opens in its place.
//!
//! Calls (M3 Task 17), in a build that shows them (in one that doesn't, the call keys do nothing here): the chat's
//! header offers Call once the chat identifies whom it calls, or Join when an invitation named a running group call.
//! Both wait until the calling endpoint is registered and no call is up, ringing or being set up; the [`CallBar`]
//! above the panes shows the incoming call's banner (Answer, Decline) and, while the call runs in its own window
//! ([`crate::call_window`]), the in-call strip
//! (Return to call, Mute, the camera, Leave). The keys: Ctrl+Shift+C calls the chat shown,
//! Ctrl+Shift+J joins its running call, Ctrl+Shift+S answers and Ctrl+Shift+D declines the call that rings,
//! Ctrl+Shift+M mutes or unmutes, Ctrl+Shift+O turns the camera on or off (a build that shows video) and Ctrl+Shift+H
//! leaves the call; the switcher offers the same as commands, with "Make a test call", which the settings' Calls page
//! offers too, as Teams' does ([`Shell::test_call_offer`]: not while a call is up or rings on any account); a test
//! call's results (in its window) end with "Device settings", which opens that page ([`Shell::open_settings_page`]).
//! Traced: `call-button shown` (the chat, and `call`, `join` or `none`).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, Global,
    InteractiveElement as _, IntoElement, KeyBinding, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity,
    Window, actions, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::avatar::Avatar;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, ResizableState, Size, h_resizable, resizable_panel};
use teams_core::{Availability, ChannelMeetup, Reporting};
use teams_proto::ManualAvailability;

use crate::accounts::{AccountTile, Accounts};
use crate::activity_view::{ActivityParts, ActivityView, ActivityViewEvent};
use crate::bridge::Core;
use crate::calendar_view::{CalendarView, CalendarViewEvent};
use crate::call_view::CallBar;
use crate::calls::{self, Ask, MeetingToJoin};
use crate::channel_dialog::{ChannelDialog, ChannelDialogEvent};
use crate::channel_view::{ChannelView, ChannelViewEvent};
use crate::chat_list::{ChatList, ChatListEvent, RowKind};
use crate::chat_view::{self, ChatView, ChatViewEvent};
use crate::create_team_more::{CreateTeamMore, CreateTeamMoreEvent};
use crate::first_run::FirstRun;
use crate::forwarded::Original;
use crate::global_search::{self, GlobalSearch, GlobalSearchEvent, Mode as SearchMode};
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::join_team::{JoinTeam, JoinTeamEvent};
use crate::manage_team::{ManageTab, ManageTeam, ManageTeamEvent};
use crate::models::{Account, Models};
use crate::own_presence::{self, OwnPresence};
use crate::people;
use crate::presence;
use crate::search::HitTarget;
use crate::settings::{ManualStatus, SettingsModel};
use crate::settings_view::{self, Page, SettingsView, SettingsViewEvent, TestCallOffer, TestCallState};
use crate::sidebar::Sidebar;
use crate::switcher::{CallCommand, Switcher, SwitcherEvent};
use crate::team_app_dialogs::{AddAppDialog, TeamPictureDialog};
use crate::team_dialogs::{
    AddMembersDialog, Confirm, ConfirmDialog, CreateTeamDialog, EditTeamDialog, TagDialog, TeamDialogEvent,
};
use crate::team_menu::{TeamChoice, team_link};
use crate::teams_list::{ChannelChosen, TeamsList, TeamsListEvent};
use crate::teams_page::{TeamsPage, TeamsPageEvent};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::view::status_text;

mod channel_management;
mod chat_people;
mod meeting_tabs;
mod new_chat;
mod profile_card;
mod title_bar;
mod workflows;

pub use meeting_tabs::ChatTab;

pub use title_bar::{FocusSearch, GoBack, GoForward, History, RIGHT_WIDTH, SEARCH_MAX, TITLE_HEIGHT, ToggleListPane};

actions!(teams_ui, [ToggleSwitcher, NextAccount, PreviousAccount, GoToChat]);
actions!(
    teams_ui,
    [
        /// Opens New message (Teams' Ctrl+N).
        NewMessage
    ]
);
actions!(teams_ui, [FontLarger, FontSmaller, FontReset]);
actions!(teams_ui, [ShowActivity, ShowChats, ShowCalendar]);
actions!(
    teams_ui,
    [
        CallChat,
        JoinCall,
        AnswerCall,
        DeclineCall,
        HangUpCall,
        ToggleMuteCall,
        ToggleCaptionsCall,
        ToggleCameraCall,
        ToggleHandCall
    ]
);

/// The shell's key context.
pub const CONTEXT: &str = "Shell";
/// The list pane's width when the window opens, px.
pub const LIST_WIDTH: f32 = 320.0;
/// The narrowest the list pane gets, px.
pub const LIST_MIN: f32 = 220.0;
/// The widest the list pane gets, px.
pub const LIST_MAX: f32 = 560.0;
/// An account's avatar on the rail, px.
const ACCOUNT_AVATAR: f32 = 36.0;
/// How often a channel shown is read again for a meeting that runs in it (its header's Join): nothing the native
/// client hears tells it.
pub const CHANNEL_MEETUP_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);
/// The rail's width, px.
pub const RAIL_WIDTH: f32 = 76.0;
/// A rail item's height, px.
const RAIL_ITEM_HEIGHT: f32 = 52.0;
/// A rail item's icon's size, px.
const RAIL_ICON: f32 = 22.0;

/// A rail item: its icon (Lucide's, ISC: `assets/icons/LICENSE-LUCIDE`) and its caption.
#[derive(Clone, Copy)]
struct RailItem {
    icon: &'static [u8],
    label: &'static str,
}

impl RailItem {
    const ACTIVITY: Self = Self {
        icon: include_bytes!("../assets/icons/bell.svg"),
        label: "Activity",
    };
    const CHAT: Self = Self {
        icon: include_bytes!("../assets/icons/chat.svg"),
        label: "Chat",
    };
    const SETTINGS: Self = Self {
        icon: include_bytes!("../assets/icons/settings.svg"),
        label: "Settings",
    };
    const CALENDAR: Self = Self {
        icon: include_bytes!("../assets/icons/calendar.svg"),
        label: "Calendar",
    };
}

/// Marks the shell's key bindings as installed.
struct Bound;

impl Global for Bound {}

/// Binds the shell's keys (Ctrl+K: the switcher; Ctrl+E, Ctrl+Shift+F: the search; Ctrl+Alt+G: its go-to mode; Alt+Left
/// and Alt+Right: Back and Forward) and the chat view's, once per app.
pub fn init(cx: &mut App) {
    chat_view::init(cx);
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    global_search::init(cx);
    crate::activity_view::init(cx);
    cx.bind_keys([
        KeyBinding::new("ctrl-k", ToggleSwitcher, None),
        // Teams' desktop key for New message
        KeyBinding::new("ctrl-n", NewMessage, None),
        KeyBinding::new("ctrl-e", FocusSearch, None),
        KeyBinding::new("ctrl-shift-f", FocusSearch, None),
        KeyBinding::new("ctrl-alt-g", GoToChat, None),
        KeyBinding::new("alt-left", GoBack, None),
        KeyBinding::new("alt-right", GoForward, None),
        KeyBinding::new("ctrl-alt-down", NextAccount, None),
        KeyBinding::new("ctrl-alt-up", PreviousAccount, None),
        // Teams' zoom keys, on the base font size
        KeyBinding::new("ctrl-=", FontLarger, None),
        KeyBinding::new("ctrl-+", FontLarger, None),
        KeyBinding::new("ctrl--", FontSmaller, None),
        KeyBinding::new("ctrl-0", FontReset, None),
        // Teams' keys for its apps on the rail
        KeyBinding::new("ctrl-1", ShowActivity, None),
        KeyBinding::new("ctrl-2", ShowChats, None),
        KeyBinding::new("ctrl-4", ShowCalendar, None),
        KeyBinding::new("ctrl-shift-c", CallChat, None),
        KeyBinding::new("ctrl-shift-j", JoinCall, None),
        KeyBinding::new("ctrl-shift-s", AnswerCall, None),
        KeyBinding::new("ctrl-shift-d", DeclineCall, None),
        KeyBinding::new("ctrl-shift-h", HangUpCall, None),
        KeyBinding::new("ctrl-shift-m", ToggleMuteCall, None),
        KeyBinding::new("alt-shift-c", ToggleCaptionsCall, None),
        // Teams' own key for the camera
        KeyBinding::new("ctrl-shift-o", ToggleCameraCall, None),
        // Teams' own key for Raise and Lower hand
        KeyBinding::new("ctrl-shift-k", ToggleHandCall, None),
    ]);
}

/// What a button of the shell does.
type ShellAction = fn(&mut Shell, &mut Context<Shell>);

/// What the shell shows right of the list: a chat, a channel or nothing in the conversation pane, or the settings
/// across both panes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Main {
    Nothing,
    Chat(String),
    /// A team's channel.
    Channel(String),
    Settings,
    /// The calendar, across both panes.
    Calendar,
    /// The search's results, across both panes.
    Search,
    /// "Your teams and channels", in the conversation pane.
    TeamsPage,
    /// A team's Manage team, in the conversation pane.
    ManageTeam(String),
    /// "Join a team", across both panes.
    JoinTeam,
    /// "More create team options", across both panes.
    CreateTeamMore,
    /// The Workflows app, across both panes.
    Workflows,
    /// New message, in the conversation pane.
    NewChat,
}

/// What the list pane shows: the chats and teams, or the Activity pane (the rail's Activity).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListPlace {
    #[default]
    Chats,
    Activity,
}

/// Where the account menu opens: over the rail's account, or under the title bar's avatar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAnchor {
    Rail,
    TitleBar,
}

/// What the shell is made of: the engine's request side, the models, the settings and the view trace.
#[derive(Clone)]
pub struct ShellParts {
    pub core: Arc<dyn Core>,
    pub models: Models,
    pub settings: Entity<SettingsModel>,
    pub trace: Arc<ViewTrace>,
}

/// The shell (see the module docs).
pub struct Shell {
    core: Arc<dyn Core>,
    models: Models,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    /// The list pane: the chats and the teams.
    sidebar: Entity<Sidebar>,
    /// The Activity pane, with what it tells; shown in the list pane's place while [`ListPlace::Activity`].
    activity: (Entity<ActivityView>, Subscription),
    list_place: ListPlace,
    /// The chat opened in the conversation pane.
    chat: Option<Entity<ChatView>>,
    /// A meeting chat's tabs: the one shown, its Recap and Shared views.
    meeting_tabs: meeting_tabs::MeetingTabs,
    /// Unsaved composers keep their text, reply and in-flight uploads when another chat is opened.
    drafts: std::collections::HashMap<String, Entity<crate::compose::Compose>>,
    /// The channel opened in the conversation pane, with the names its header shows.
    channel: Option<(Entity<ChannelView>, ChannelChosen)>,
    /// This open channel has appeared in a successful team list, so a later absence can establish removal.
    channel_listed: bool,
    /// The meeting that runs in the channel shown, as last read ([`Self::read_channel_meetup`]): its header's Join.
    channel_meetup: Option<(String, ChannelMeetup)>,
    settings_view: Entity<SettingsView>,
    /// The calendar, shown across both panes from the rail's Calendar.
    calendar: Entity<CalendarView>,
    /// The first run's screen, shown over everything while it waits ([`SettingsModel::awaiting_first_run`]).
    first_run: Entity<FirstRun>,
    switcher: Option<(Entity<Switcher>, Subscription)>,
    /// A team's "Create a channel" dialog, while it is open.
    channel_dialog: Option<(Entity<ChannelDialog>, Subscription)>,
    /// "Your teams and channels", once it was opened.
    teams_page: Option<(Entity<TeamsPage>, Subscription)>,
    /// Manage team, while it is shown.
    manage_team: Option<(Entity<ManageTeam>, Subscription)>,
    /// "Join a team", while it is shown.
    join_team: Option<(Entity<JoinTeam>, Subscription)>,
    create_team_more: Option<(Entity<CreateTeamMore>, Subscription)>,
    /// The Workflows app, once it was opened: it keeps its tab and pages while another place is shown.
    workflows: workflows::Place,
    /// A dialog of the team pages, while one is open.
    team_dialog: Option<(gpui::AnyView, Subscription)>,
    /// Where the keyboard was when a dialog opened (a team's line, its "..." menu's row, a page's row): it goes back
    /// there when the dialog closes, as Teams gives it back.
    dialog_return: crate::keyboard::Return,
    /// A toast, while one shows.
    toast: Option<(SharedString, Task<()>)>,
    /// The title bar's search: its box, its suggestions and its results.
    search_box: Entity<GlobalSearch>,
    /// The places shown, for Back and Forward.
    history: History,
    /// The list pane is shown (the title bar's toggle hides it).
    list_shown: bool,
    main: Main,
    /// What the settings, the calendar or the search results hid, shown again when the rail's Chat leaves them.
    before_settings: Main,
    panes: Entity<ResizableState>,
    focus: FocusHandle,
    /// The account's own presence, once the desktop handed it in, with the shell's observation of it.
    own_presence: Option<(Entity<OwnPresence>, Subscription)>,
    /// The account menu is open.
    status_menu: bool,
    /// Where it opened.
    status_anchor: MenuAnchor,
    /// What the chat's header was last traced showing.
    header_traced: Option<HeaderShown>,
    /// The incoming call's banner and the in-call strip.
    call_bar: Entity<CallBar>,
    /// The chat whose call button was last traced, and which it showed.
    call_button_traced: Option<(String, &'static str)>,
    /// The app's accounts, once the desktop handed them in ([`Shell::set_accounts`]): the rail shows their avatars.
    accounts: Option<(Entity<Accounts>, Subscription)>,
    /// What the chat view shown tells ([`Shell::follow_chat`]), while there is one.
    chat_events: Option<Subscription>,
    /// What the channel view tells ([`Shell::follow_channel`]), once there is one.
    channel_events: Option<Subscription>,
    /// New message, while it is open.
    new_chat: new_chat::Open,
    /// The chat header's people flyout, while it is open, and where its button was last drawn.
    people: chat_people::State,
    /// The window's profile card, while one is open, and the pointer's way over faces and names.
    cards: profile_card::State,
    _subscriptions: [Subscription; 12],
}

/// What the chat's header shows, as the trace tells it: the chat, its other member's presence (a 1:1 chat's), and
/// whether its name came from people's names (or its topic) and its avatar from a picture. Never the name itself.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeaderShown {
    chat: String,
    presence: &'static str,
    named: bool,
    picture: bool,
}

impl Shell {
    /// The chat the settings kept as chosen for the account `me`.
    fn selected_chat(settings: &Entity<SettingsModel>, me: &str, cx: &App) -> Option<String> {
        settings.read(cx).settings().selected_chat_of(me).map(str::to_owned)
    }

    /// The shell over `parts`, focused; the chat the settings kept is opened again.
    pub fn new(parts: ShellParts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat_parts = parts.clone();
        let ShellParts {
            core,
            models,
            settings,
            trace,
        } = parts;
        let selected = Self::selected_chat(&settings, core.me(), cx);
        let (list, previews) = Self::chat_list(&chat_parts, selected.clone(), cx);
        let opened = Self::opened(&list, window, cx);
        let (teams, [channel_opened, kept_channel]) = Self::teams_list(&core, &models, &trace, window, cx);
        let (sidebar, new_message_asked) = Self::make_sidebar(&list, &teams, &models, &trace, window, cx);
        let (settings_view, test_call_asked) = Self::make_settings_view(&core, &settings, cx);
        let first_run = cx.new(|cx| FirstRun::new(settings.clone(), cx));
        let (calendar, calendar_events) = Self::calendar(&core, &models, &trace, &settings, window, cx);
        let [settled, presence, renamed] = Self::observe_header(&settings, &models, &list, cx);
        let (call_bar, calling) = Self::calls(&core, &models, &list, &trace, &settings, cx);
        let (focus, chat, chat_events) = Self::kept_chat(chat_parts, selected.clone(), window, cx);
        let (search_box, searched) = Self::search_box(&core, &list, &teams, &trace, window, cx);
        let activity = Self::activity_pane(&core, &models, (&list, &teams), &trace, window, cx);
        Self {
            activity,
            list_place: ListPlace::Chats,
            cards: Self::card_host(&models, (&search_box, previews), window, cx),
            core,
            models,
            settings,
            trace,
            list,
            teams,
            sidebar,
            chat,
            drafts: std::collections::HashMap::new(),
            meeting_tabs: meeting_tabs::MeetingTabs::default(),
            channel: None,
            channel_listed: false,
            channel_meetup: None,
            settings_view,
            calendar,
            first_run,
            switcher: None,
            channel_dialog: None,
            teams_page: None,
            manage_team: None,
            join_team: None,
            create_team_more: None,
            workflows: workflows::Place::default(),
            team_dialog: None,
            dialog_return: crate::keyboard::Return::default(),
            toast: None,
            search_box,
            history: History::default(),
            list_shown: true,
            main: selected.map_or(Main::Nothing, Main::Chat),
            before_settings: Main::Nothing,
            panes: cx.new(|_| ResizableState::default()),
            focus,
            own_presence: None,
            status_menu: false,
            status_anchor: MenuAnchor::Rail,
            header_traced: None,
            call_bar,
            call_button_traced: None,
            accounts: None,
            chat_events,
            channel_events: None,
            new_chat: None,
            people: chat_people::State::default(),
            _subscriptions: [
                opened,
                channel_opened,
                kept_channel,
                presence,
                Self::keystrokes_are_use(window, cx),
                renamed,
                settled,
                calling,
                test_call_asked,
                calendar_events,
                searched,
                new_message_asked,
            ],
        }
    }

    /// The Activity pane, and what it tells followed: an item opens its conversation at its message, a missed call's
    /// Chat and Call reach the caller's 1:1 chat.
    fn activity_pane(
        core: &Arc<dyn Core>,
        models: &Models,
        (chats, teams): (&Entity<ChatList>, &Entity<TeamsList>),
        trace: &Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<ActivityView>, Subscription) {
        let parts = ActivityParts {
            core: Arc::clone(core),
            models: models.clone(),
            chats: chats.clone(),
            teams: teams.clone(),
            trace: Arc::clone(trace),
        };
        let view = cx.new(|cx| ActivityView::new(parts, window, cx));
        // the rail's count follows the feed
        let events = cx.subscribe_in(&view, window, |this: &mut Self, _, event, window, cx| match event {
            ActivityViewEvent::Open(target) => this.open_message(target.clone(), window, cx),
            ActivityViewEvent::ChatWith(mri) => {
                this.chat_with(mri, window, cx);
            }
            ActivityViewEvent::CallBack(mri) => {
                if this.chat_with(mri, window, cx) {
                    this.call_chat(cx);
                }
            }
            ActivityViewEvent::Toast(_) => this.show_toast(crate::activity_view::REMOVED, cx),
        });
        (view, events)
    }

    /// Opens the 1:1 chat with `mri`, when the list has one; whether it did.
    fn chat_with(&mut self, mri: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let found = self
            .list
            .read(cx)
            .findable_rows()
            .find(|row| row.partner.as_deref() == Some(mri) && row.id.ends_with("@unq.gbl.spaces"))
            .map(|row| row.id.clone());
        match found {
            Some(id) => {
                self.open_chat(id, window, cx);
                true
            }
            None => {
                self.show_toast("There's no chat with them yet", cx);
                false
            }
        }
    }

    /// The Activity pane.
    #[must_use]
    pub fn activity_view(&self) -> &Entity<ActivityView> {
        &self.activity.0
    }

    /// What the list pane shows.
    #[must_use]
    pub fn list_place(&self) -> ListPlace {
        self.list_place
    }

    /// The rail's Activity (and Ctrl+1): the Activity pane in the list pane's place, leaving the settings, the
    /// calendar or the search's results for what they hid; the keyboard goes to the pane.
    pub fn show_activity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_chats_pane(window, cx);
        self.list_shown = true;
        if self.list_place != ListPlace::Activity {
            self.trace.record(&TraceLine::new("activity", "shown"));
            self.list_place = ListPlace::Activity;
        }
        self.sync_activity_shown(window, cx);
        cx.notify();
    }

    /// Tells the Activity pane whether it is on screen: the list pane shows it and nothing covers both panes.
    fn sync_activity_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let covered = matches!(
            self.main,
            Main::Settings | Main::Calendar | Main::Search | Main::JoinTeam | Main::CreateTeamMore | Main::Workflows
        );
        let shown = self.list_place == ListPlace::Activity && self.list_shown && !covered;
        if self.activity.0.read(cx).shown() != shown {
            self.activity.0.update(cx, |view, cx| view.set_shown(shown, window, cx));
        }
    }

    /// A chat the chat list opens shows in the conversation pane.
    fn opened(list: &Entity<ChatList>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(list, window, |this: &mut Self, _, event, window, cx| match event {
            ChatListEvent::Opened(id) => this.open_chat(id.clone(), window, cx),
        })
    }

    /// The chat list, `selected` highlighted, and the account's translations outside the open chats
    /// ([`crate::chat_translate::Previews`]) handed to it: a translated chat's preview shows in its language.
    fn chat_list(
        parts: &ShellParts,
        selected: Option<String>,
        cx: &mut Context<Self>,
    ) -> (Entity<ChatList>, Entity<crate::chat_translate::Previews>) {
        let (core, models, trace) = (Arc::clone(&parts.core), parts.models.clone(), Arc::clone(&parts.trace));
        let list = cx.new(|cx| ChatList::new(core, models, trace, selected, cx));
        let previews = crate::chat_translate::Previews::of(&parts.core, &parts.settings, cx);
        list.update(cx, |list, cx| list.set_previews(previews.clone(), cx));
        (list, previews)
    }

    /// The shell's focus, focused now: what is typed goes to the shell until a view takes it.
    fn focused(window: &mut Window, cx: &mut Context<Self>) -> FocusHandle {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        focus
    }

    /// What the shell draws again as it changes: the settings (the first run's screen comes and goes with them), a
    /// presence the engine told (the chat's header draws its dot again) and the list's rows built again (a name or a
    /// picture read: the header shows what its row shows).
    fn observe_header(
        settings: &Entity<SettingsModel>,
        models: &Models,
        list: &Entity<ChatList>,
        cx: &mut Context<Self>,
    ) -> [Subscription; 3] {
        let settled = cx.observe(settings, |_: &mut Self, _, cx| cx.notify());
        let presence = cx.observe(&models.presence, |this: &mut Self, _, cx| {
            this.trace_header(cx);
            cx.notify();
        });
        let renamed = cx.observe(list, |this: &mut Self, _, cx| this.list_changed(cx));
        [settled, presence, renamed]
    }

    /// The title bar's search, and what it tells the shell followed.
    fn search_box(
        core: &Arc<dyn Core>,
        list: &Entity<ChatList>,
        teams: &Entity<TeamsList>,
        trace: &Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<GlobalSearch>, Subscription) {
        let (core, list, teams, trace) = (Arc::clone(core), list.clone(), teams.clone(), Arc::clone(trace));
        let search = cx.new(|cx| GlobalSearch::new(core, list, teams, trace, window, cx));
        let events = cx.subscribe_in(&search, window, |this: &mut Self, _, event, window, cx| match event {
            GlobalSearchEvent::OpenChat(id) => this.open_chat(id.clone(), window, cx),
            GlobalSearchEvent::Toast(words) => this.show_toast(words, cx),
            GlobalSearchEvent::OpenChannel(id) => this.open_channel_id(id.clone(), window, cx),
            GlobalSearchEvent::OpenMessage(target) => this.open_message(target.clone(), window, cx),
            GlobalSearchEvent::ShowResults => this.show_search_results(window, cx),
            GlobalSearchEvent::LeaveResults => this.leave_search_results(window, cx),
            GlobalSearchEvent::Done => {
                if this.search_box.read(cx).holds_focus(window, cx) {
                    this.take_focus(window, cx);
                }
            }
        });
        (search, events)
    }

    /// The Teams list, with the subscriptions that open a channel it chose, and a channel kept as the selected chat.
    fn teams_list(
        core: &Arc<dyn Core>,
        models: &Models,
        trace: &Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TeamsList>, [Subscription; 2]) {
        let (core, models, trace) = (Arc::clone(core), models.clone(), Arc::clone(trace));
        let teams = cx.new(|cx| TeamsList::new(core, models, trace, cx));
        let channel_opened = cx.subscribe_in(&teams, window, |this: &mut Self, _, event, window, cx| match event {
            TeamsListEvent::Opened(chosen) => this.open_channel(chosen.clone(), window, cx),
            other => this.team_list_event(other, window, cx),
        });
        // a channel kept as the selected chat (by a build that opened one in a chat view) opens as a channel once the
        // list knows it
        let mut loaded_at = teams.read(cx).revision();
        let kept_channel = cx.observe_in(&teams, window, move |this: &mut Self, teams, window, cx| {
            let loaded = teams.read(cx).revision();
            let refreshed = loaded != loaded_at;
            loaded_at = loaded;
            if let Main::Chat(id) = &this.main {
                if let Some(chosen) = teams.read(cx).chosen(id) {
                    this.open_channel(chosen, window, cx);
                }
            } else if refreshed {
                // Selection, menus and sections also notify. Only a completed list read can establish removal.
                this.channels_changed(&teams, cx);
            }
        });
        (teams, [channel_opened, kept_channel])
    }

    /// The shell's focus, then the chat view of `selected`, the chat the settings kept, opened again and taking what is
    /// typed from the shell, and what it tells followed ([`Shell::follow_chat`]). In that order: the focus made last
    /// would take the keyboard from the compose box.
    fn kept_chat(
        parts: ShellParts,
        selected: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (FocusHandle, Option<Entity<ChatView>>, Option<Subscription>) {
        let focus = Self::focused(window, cx);
        let Some(id) = selected else {
            return (focus, None, None);
        };
        parts.trace.record(&TraceLine::new("chat", "opened").conversation(&id));
        let chat = cx.new(|cx| ChatView::new(parts, id, window, cx));
        let compose = chat.read(cx).compose().clone();
        compose.update(cx, |compose, cx| compose.focus(window, cx));
        let events = Self::follow_chat(&chat, window, cx);
        (focus, Some(chat), Some(events))
    }

    /// The settings view of the account `core` is, offering the test call as the shell says
    /// ([`Shell::test_call_offer`]), and the subscription that asks for it when the user presses it.
    fn make_settings_view(
        core: &Arc<dyn Core>,
        settings: &Entity<SettingsModel>,
        cx: &mut Context<Self>,
    ) -> (Entity<SettingsView>, Subscription) {
        let account = core.me().to_owned();
        let shell = cx.weak_entity();
        let offer: TestCallState = Rc::new(move |cx: &App| {
            shell
                .upgrade()
                .map_or(TestCallOffer::Hidden, |shell| shell.read(cx).test_call_offer(cx))
        });
        let view = cx.new(|cx| {
            SettingsView::new(settings.clone(), cx)
                .for_account(account)
                .with_test_call(offer)
        });
        let asked = cx.subscribe(&view, |this: &mut Self, _, event, cx| match event {
            SettingsViewEvent::MakeTestCall => this.ask_call(Ask::TestCall, cx),
        });
        (view, asked)
    }

    /// The call bar over `models`' calls, and the observation that keeps the header's call button up to date.
    fn calls(
        core: &Arc<dyn Core>,
        models: &Models,
        list: &Entity<ChatList>,
        trace: &Arc<ViewTrace>,
        settings: &Entity<SettingsModel>,
        cx: &mut Context<Self>,
    ) -> (Entity<CallBar>, Subscription) {
        // the account's calls' captions saved on this computer, as the settings ask
        crate::call_transcript::follow(core, &models.calls, settings, trace, cx);
        let (core, list, trace, calls) = (Arc::clone(core), list.clone(), Arc::clone(trace), models.calls.clone());
        let call_bar = cx.new(|cx| CallBar::new(core, calls, list, trace, cx));
        // a call up, rung or ended: the header's call button follows
        let calling = cx.observe(&models.calls, |this: &mut Self, _, cx| {
            this.trace_call_button(cx);
            cx.notify();
        });
        (call_bar, calling)
    }

    /// The calendar, and what it tells the shell: a meeting's chat to open, or its pre-join to show.
    fn calendar(
        core: &Arc<dyn Core>,
        models: &Models,
        trace: &Arc<ViewTrace>,
        settings: &Entity<SettingsModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<CalendarView>, Subscription) {
        let (core, calls, trace) = (Arc::clone(core), models.calls.clone(), Arc::clone(trace));
        let calendar = cx.new(|cx| CalendarView::new(core, calls, trace, cx));
        // the people's calendars the account added are kept in the settings
        calendar.update(cx, |calendar, cx| calendar.set_settings(settings.clone(), cx));
        let events = cx.subscribe_in(&calendar, window, |this: &mut Self, _, event, window, cx| match event {
            CalendarViewEvent::OpenChat(chat) => this.open_chat(chat.clone(), window, cx),
            CalendarViewEvent::Join(to_join) => this.open_prejoin(to_join.clone(), cx),
            CalendarViewEvent::JoinChat { chat, title, link } => {
                this.join_meeting_chat(chat.clone(), Some(title.clone()), link.clone(), cx);
            }
            CalendarViewEvent::JoinCode(meeting_code) => this.join_meeting_code(meeting_code.clone(), cx),
        });
        (calendar, events)
    }

    /// The calendar.
    #[must_use]
    pub fn calendar_view(&self) -> &Entity<CalendarView> {
        &self.calendar
    }

    /// The rail's Calendar (and Ctrl+4): the calendar across both panes; the chat it hides is kept, but isn't shown.
    pub fn show_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.main == Main::Calendar {
            return;
        }
        self.trace.record(&TraceLine::new("calendar", "opened"));
        let before = std::mem::replace(&mut self.main, Main::Calendar);
        if !matches!(before, Main::Settings | Main::Search) {
            self.before_settings = before;
        }
        self.put_chat_away(cx);
        self.leave_workflows(cx);
        self.close_switcher(window, cx);
        self.close_search(cx);
        self.calendar
            .update(cx, |calendar, cx| calendar.set_shown(true, window, cx));
        cx.notify();
    }

    /// The calendar (or the Workflows app) is left for something else.
    fn hide_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_workflows(cx);
        if self.calendar.read(cx).shown() {
            self.calendar
                .update(cx, |calendar, cx| calendar.set_shown(false, window, cx));
        }
    }

    /// Shows the settings on `page`, the keyboard on their page column, as the rail's Settings does.
    pub fn open_settings_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_view.update(cx, |view, cx| view.show_page(page, cx));
        self.open_settings(window, cx);
    }

    /// The rail's Settings: the settings on the page they were left on, the calendar left, the keyboard on their page
    /// column. The keyboard moves with them: what had it (a compose box, the calendar) leaves the window's tree, and a
    /// focus left there would reach no action's handler.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_calendar(window, cx);
        self.show_settings(cx);
        self.settings_view.update(cx, |view, cx| view.focus_pages(window, cx));
    }

    /// Whether the settings' Calls page offers the test call now: not in a build that shows no calls, without the
    /// echo service or read-only; not while this account or another has a call up, ringing or being set up (one call
    /// at a time across the accounts), nor before the calling endpoint is registered.
    #[must_use]
    pub fn test_call_offer(&self, cx: &App) -> TestCallOffer {
        let calls = self.models.calls.read(cx);
        if !calls.shows() || !calls.has_test_call() || self.core.read_only() {
            TestCallOffer::Hidden
        } else if calls.busy() {
            TestCallOffer::InCall
        } else if self.busy_elsewhere(cx) {
            TestCallOffer::OtherAccount
        } else if !calls.registered() {
            TestCallOffer::Connecting
        } else {
            TestCallOffer::Ready
        }
    }

    /// The incoming call's banner and the in-call strip.
    #[must_use]
    pub fn call_bar(&self) -> &Entity<CallBar> {
        &self.call_bar
    }

    /// The call button the chat shown has: `call`, `join` (a running call there, from an invitation), `meeting` (a
    /// meeting's chat: its Join, started or not, as Teams' header has it), or `none` (no chat shown, calls not shown,
    /// the calling endpoint not registered, a group roster not yet stored, or a call up or ringing).
    #[must_use]
    pub fn call_button(&self, cx: &App) -> &'static str {
        let Main::Chat(id) = &self.main else {
            return "none";
        };
        let calls = self.models.calls.read(cx);
        if !calls.idle() || !calls.registered() || self.busy_elsewhere(cx) {
            "none"
        } else if self.is_meeting_chat(id, cx) {
            "meeting"
        } else if calls.joinable(id) {
            "join"
        } else if self.list.read(cx).can_call(id) {
            "call"
        } else {
            "none"
        }
    }

    /// Traces the chat's call button (`call-button shown`) when it changed; nothing while calls aren't shown.
    fn trace_call_button(&mut self, cx: &App) {
        let Main::Chat(id) = &self.main else {
            return;
        };
        if !self.models.calls.read(cx).shows() {
            return;
        }
        let shown = (id.clone(), self.call_button(cx));
        if self.call_button_traced.as_ref() == Some(&shown) {
            return;
        }
        self.trace.record(
            &TraceLine::new("call-button", "shown")
                .conversation(&shown.0)
                .kind(shown.1),
        );
        self.call_button_traced = Some(shown);
    }

    /// The user asks `ask` of the calls (a key, a button, a command).
    pub fn ask_call(&mut self, ask: Ask, cx: &mut Context<Self>) {
        let enters = matches!(ask, Ask::Call { .. } | Ask::Join { .. } | Ask::TestCall | Ask::Answer);
        if enters && self.busy_elsewhere(cx) {
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "another-account", cx);
            return;
        }
        if let Ask::Call { chat } = &ask
            && !self.models.calls.read(cx).busy()
            && self
                .list
                .read(cx)
                .row(chat)
                .is_some_and(|row| matches!(row.kind, RowKind::Group | RowKind::Meeting) && row.members == 0)
        {
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "no-roster", cx);
            return;
        }
        calls::ask(&self.models.calls, &self.core, &self.trace, ask, cx);
    }

    /// The calendar's Join: the meeting's pre-join, in its own window; while a call is up or rings, its refusal.
    pub fn open_prejoin(&mut self, to_join: MeetingToJoin, cx: &mut Context<Self>) {
        let busy_elsewhere = self.busy_elsewhere(cx);
        calls::show_prejoin(&self.models.calls, &self.trace, to_join, busy_elsewhere, cx);
    }

    /// A meeting's Join where only its chat is known (the meeting chat's header, an event whose link names no
    /// organizer): the meeting read from its chat ([`Core::meeting`]), then its pre-join, titled `title` or as the chat
    /// names it. When the chat names no meeting, `link` opens in the browser, or the join is refused.
    pub fn join_meeting_chat(
        &mut self,
        chat: String,
        title: Option<String>,
        link: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.trace
            .record(&TraceLine::new("call", "meeting-read").conversation(&chat));
        let reply = self.core.meeting(chat.clone());
        cx.spawn(async move |this, cx| {
            let read = reply.await;
            let _ = this.update(cx, |this, cx| match read {
                Ok(Some(details)) => {
                    let title = title.or(details.title).unwrap_or_else(|| "Meeting".to_owned());
                    this.open_prejoin(
                        MeetingToJoin::Chat {
                            chat,
                            meeting: details.meeting,
                            title,
                        },
                        cx,
                    );
                }
                _ => {
                    if let Some(link) = link {
                        cx.open_url(&link);
                    } else {
                        let ask = Ask::Join { chat };
                        let (calls, trace) = (this.models.calls.clone(), Arc::clone(&this.trace));
                        calls::refuse_here(&calls, &trace, &ask, "no-meeting", cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Open the pre-join for an invitation. The service resolves its ID only after Join now; a meeting's
    /// chat may hold a long link and cannot reliably be searched by numeric code.
    pub fn join_meeting_code(&mut self, code: teams_core::MeetingCode, cx: &mut Context<Self>) {
        self.trace.record(&TraceLine::new("call", "meeting-code"));
        self.open_prejoin(MeetingToJoin::Code(code), cx);
    }

    /// The account's models the shell shows.
    #[must_use]
    pub fn models(&self) -> &Models {
        &self.models
    }

    /// The account the shell is: its MRI.
    #[must_use]
    pub fn account(&self) -> &str {
        self.core.me()
    }

    /// Hands in the app's accounts: the rail shows their avatars in place of the status button, and no call is offered
    /// while another account has one ([`Accounts::busy_elsewhere`]).
    pub fn set_accounts(&mut self, accounts: Entity<Accounts>, cx: &mut Context<Self>) {
        let observing = cx.observe(&accounts, |this: &mut Self, _, cx| {
            this.trace_call_button(cx);
            cx.notify();
        });
        self.accounts = Some((accounts, observing));
        cx.notify();
    }

    /// Takes the window's keyboard: the chat's compose box when it is drawn ([`Shell::compose_drawn`]), else the shell
    /// (an account switched to). A focus left on something not drawn reaches no action's handler: GPUI dispatches
    /// from the window's root instead.
    pub fn take_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.compose_drawn(cx) {
            Some(compose) => compose.update(cx, |compose, cx| compose.focus(window, cx)),
            None => self.focus.focus(window, cx),
        }
    }

    /// The compose box of the chat the conversation pane shows, when it is drawn and takes text: not on a meeting's
    /// Recap or Shared tab (they replace the chat), not while the account is signed out (its panes give way to the
    /// status), not the read-only notes' box.
    fn compose_drawn(&self, cx: &App) -> Option<Entity<crate::compose::Compose>> {
        let Main::Chat(id) = &self.main else { return None };
        let chat = self
            .chat
            .as_ref()
            .filter(|chat| chat.read(cx).conversation() == id.as_str())?;
        let compose = chat.read(cx).compose();
        let drawn = self.chat_tab_shown() && !self.signed_out_shown(cx) && !compose.read(cx).is_read_only();
        drawn.then(|| compose.clone())
    }

    /// Whether the account's panes give way to its signed-out status ([`Shell::panes_or_signed_out`]).
    fn signed_out_shown(&self, cx: &App) -> bool {
        self.accounts.is_some() && matches!(self.models.status.read(cx).account, Account::SignedOut { .. })
    }

    /// Shows the next account of the app (`forward`) or the one before, round: Ctrl+Alt+Down and Ctrl+Alt+Up.
    pub fn cycle_account(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some((accounts, _)) = &self.accounts {
            accounts.update(cx, |accounts, cx| accounts.cycle(forward, cx));
        }
    }

    /// Whether another account of the app has a call up, ringing or being set up.
    fn busy_elsewhere(&self, cx: &App) -> bool {
        self.accounts
            .as_ref()
            .is_some_and(|(accounts, _)| accounts.read(cx).busy_elsewhere(self.core.me(), cx))
    }

    /// Calls the chat shown (a 1:1 chat's other member, a group chat's members).
    pub fn call_chat(&mut self, cx: &mut Context<Self>) {
        if let Main::Chat(id) = &self.main {
            let chat = id.clone();
            self.ask_call(Ask::Call { chat }, cx);
        } else {
            let ask = Ask::Call { chat: String::new() };
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "no-chat", cx);
        }
    }

    /// Whether the chat `id` is a meeting's, as the list has it.
    fn is_meeting_chat(&self, id: &str, cx: &App) -> bool {
        self.list
            .read(cx)
            .row(id)
            .is_some_and(|row| row.kind == crate::chat_list::RowKind::Meeting)
    }

    /// The chat `id`'s name, when the list names it.
    fn chat_title(&self, id: &str, cx: &App) -> Option<String> {
        let row = self.list.read(cx).row(id)?;
        row.named.then(|| row.name.to_string())
    }

    /// Joins the running call of the chat shown; in a meeting's chat, goes into the meeting (its pre-join), started or
    /// not.
    pub fn join_call(&mut self, cx: &mut Context<Self>) {
        if let Main::Chat(id) = &self.main {
            let chat = id.clone();
            if self.is_meeting_chat(&chat, cx) {
                let title = self.chat_title(&chat, cx);
                // a call up or ringing refuses it as the pre-join opens
                self.join_meeting_chat(chat, title, None, cx);
                return;
            }
            self.ask_call(Ask::Join { chat }, cx);
        } else {
            let ask = Ask::Join { chat: String::new() };
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "no-chat", cx);
        }
    }

    /// Reads whether a meeting runs in the channel `channel` ([`Core::channel_meetup`]), and again every
    /// [`CHANNEL_MEETUP_REFRESH`] while that channel is shown: its header has Join while one runs, as Teams' does.
    fn read_channel_meetup(&mut self, channel: String, cx: &mut Context<Self>) {
        if self.channel_meetup.as_ref().is_some_and(|(shown, _)| *shown != channel) {
            self.channel_meetup = None;
        }
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx| {
            loop {
                let read = core.channel_meetup(channel.clone()).await;
                let shown = this
                    .update(cx, |this, cx| this.channel_meetup_read(&channel, read, cx))
                    .unwrap_or(false);
                if !shown {
                    return;
                }
                cx.background_executor().timer(CHANNEL_MEETUP_REFRESH).await;
            }
        })
        .detach();
    }

    /// What a read of the channel `channel`'s meeting gave, kept while that channel shows (a read that failed keeps what
    /// was read last); whether it still shows.
    fn channel_meetup_read(
        &mut self,
        channel: &str,
        read: Result<Option<ChannelMeetup>, teams_core::EngineError>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.main != Main::Channel(channel.to_owned()) {
            return false;
        }
        let kept = self.channel_meetup.take().filter(|(shown, _)| shown == channel);
        let now = match read {
            Ok(meetup) => meetup.map(|meetup| (channel.to_owned(), meetup)),
            Err(_) => kept.clone(),
        };
        if now != kept {
            let kind = if now.is_some() { "running" } else { "none" };
            self.trace.record(
                &TraceLine::new("channel-meeting", "shown")
                    .conversation(channel)
                    .kind(kind),
            );
            cx.notify();
        }
        self.channel_meetup = now;
        true
    }

    /// A channel meeting's title, as Teams names one: `Meeting in "<channel>"`.
    fn channel_meeting_title(&self, channel: &str, cx: &App) -> String {
        self.channel
            .as_ref()
            .filter(|(view, _)| view.read(cx).channel() == channel)
            .map(|(_, chosen)| chosen.name.to_string())
            .filter(|name| !name.is_empty())
            .map_or_else(|| "Meeting".to_owned(), |name| format!("Meeting in \"{name}\""))
    }

    /// The channel header's Meet now: a new meeting in the channel shown, its pre-join first (Join now starts it); while
    /// a call is up or rings, its refusal.
    pub fn meet_now(&mut self, cx: &mut Context<Self>) {
        let Main::Channel(channel) = self.main.clone() else {
            return;
        };
        if self.channel_archived(&channel, cx) {
            return;
        }
        let busy = self.models.calls.read(cx).busy();
        if busy || self.busy_elsewhere(cx) {
            let ask = Ask::MeetNow { channel };
            let why = if busy { "busy" } else { "another-account" };
            calls::refuse_here(&self.models.calls, &self.trace, &ask, why, cx);
            return;
        }
        let title = self.channel_meeting_title(&channel, cx);
        self.trace.record(
            &TraceLine::new("call", "prejoin")
                .conversation(&channel)
                .kind("meet-now"),
        );
        // nobody organizes the meeting before it starts: the account will, and the start names no organizer
        let to_join = MeetingToJoin::Chat {
            chat: channel,
            meeting: teams_proto::MeetingInfo {
                organizer_id: String::new(),
                tenant_id: String::new(),
            },
            title,
        };
        self.models
            .calls
            .update(cx, |calls, cx| calls.open_meet_now(to_join, cx));
        crate::call_window::raise(cx);
    }

    /// Where the channel shown would hold a scheduled meeting: a standard channel of a team whose mailbox the teams read
    /// named (a private channel has no meeting of the team's calendar, as in Teams).
    fn meeting_place(&self, cx: &App) -> Option<crate::event_form::MeetingPlace> {
        let Main::Channel(channel) = &self.main else {
            return None;
        };
        let teams = self.teams.read(cx).teams()?;
        let team = teams
            .iter()
            .find(|team| team.channels.iter().any(|known| known.id == *channel))?;
        let known = team.channels.iter().find(|known| known.id == *channel)?;
        if known.kind != teams_store::ChannelKind::Standard || team.archived || known.about.archived {
            return None;
        }
        Some(crate::event_form::MeetingPlace {
            channel: teams_core::MeetingChannel {
                channel: channel.clone(),
                mailbox: team.mailbox.clone()?,
            },
            team: team.name.clone(),
            name: known.name.clone(),
        })
    }

    /// The Meet now menu's Schedule a meeting: the scheduling form on a new meeting in the channel shown.
    pub fn schedule_channel_meeting(&mut self, cx: &mut Context<Self>) {
        let Some(place) = self.meeting_place(cx) else {
            return;
        };
        self.trace
            .record(&TraceLine::new("channel-meeting", "schedule").conversation(&place.channel.channel));
        self.calendar
            .update(cx, |calendar, cx| calendar.schedule_in_channel(place, cx));
    }

    /// A meeting card's Edit in the channel shown: the meeting read whole from its team's calendar, then the scheduling
    /// form opened on it with the channel's row (Send update, Cancel meeting), as Teams' details open.
    pub fn edit_channel_meeting(&mut self, ical_uid: String, cx: &mut Context<Self>) {
        let Some(place) = self.meeting_place(cx) else {
            return;
        };
        self.trace
            .record(&TraceLine::new("channel-meeting", "edit").conversation(&place.channel.channel));
        let reply = self.core.channel_event(place.channel.mailbox.clone(), ical_uid);
        let calendar = self.calendar.downgrade();
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = calendar.update(cx, |calendar, cx| match answer {
                Ok(full) => calendar.edit_in_channel(full, place, cx),
                Err(teams_core::EngineError::Stopped) => {}
                Err(_) => calendar.say(crate::calendar_view::EVENT_UNREAD, cx),
            });
        })
        .detach();
    }

    /// The channel header's Join: the meeting that runs in the channel shown, its pre-join first.
    pub fn join_channel_meeting(&mut self, cx: &mut Context<Self>) {
        let Some((channel, meetup)) = self.channel_meetup.clone() else {
            let ask = Ask::Join { chat: String::new() };
            calls::refuse_here(&self.models.calls, &self.trace, &ask, "no-meeting", cx);
            return;
        };
        let title = self.channel_meeting_title(&channel, cx);
        self.open_prejoin(
            MeetingToJoin::Chat {
                chat: channel,
                meeting: meetup.meeting,
                title,
            },
            cx,
        );
    }

    /// The face of channel `id`'s team, as Teams' channel header leads with it: the team's picture once read, else its
    /// initials on its colour (the Teams list's own [`TeamsList::avatar`]); none for a team the list doesn't hold.
    fn team_face(&self, id: &str, palette: Palette, cx: &App) -> Option<AnyElement> {
        let teams = self.teams.read(cx);
        let team = teams
            .teams()?
            .iter()
            .find(|team| team.channels.iter().any(|channel| channel.id == id))?;
        Some(
            div()
                .debug_selector(|| "channel-header-team".to_owned())
                .flex_none()
                .child(teams.avatar(team, 32.0, false, palette))
                .into_any_element(),
        )
    }

    /// The channel header's meeting button, as Teams' header has it once calls may go: Join while a meeting runs in the
    /// channel shown, else Meet now with its menu (Meet now).
    fn channel_join_button(&self, id: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let calls = self.models.calls.read(cx);
        if !calls.idle() || !calls.registered() || self.busy_elsewhere(cx) || self.core.read_only() {
            return None;
        }
        let runs = self.channel_meetup.as_ref().is_some_and(|(shown, _)| shown == id);
        if !runs {
            if self.channel_archived(id, cx) {
                return None;
            }
            let schedules = self.meeting_place(cx).is_some();
            return Some(Self::meet_now_button(schedules, cx));
        }
        let button = icon_button("channel-join-meeting", Glyph::VIDEO, "Join the channel's meeting")
            .label("Join")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| this.join_channel_meeting(cx)));
        Some(selected_by("channel-join-meeting", button).into_any_element())
    }

    /// Teams' Meet now: the button starts a meeting in the channel, and its caret opens the menu of ways to meet (Meet
    /// now, and Schedule a meeting where the channel can hold one: `schedules`).
    fn meet_now_button(schedules: bool, cx: &mut Context<Self>) -> AnyElement {
        let meet = icon_button("channel-meet-now", Glyph::VIDEO, "Start a meeting in this channel now")
            .label("Meet now")
            .on_click(cx.listener(|this, _, _, cx| this.meet_now(cx)));
        let shell = cx.entity().downgrade();
        let caret = icon_button("channel-meet-menu", Glyph::CHEVRON_DOWN, "More ways to meet").dropdown_menu(
            move |menu, _, _| {
                let now = shell.clone();
                let menu = menu.item(
                    meet_menu_row("now", Glyph::VIDEO, "Meet now").on_click(move |_, _, cx| {
                        let _ = now.update(cx, Self::meet_now);
                    }),
                );
                if !schedules {
                    return menu;
                }
                let later = shell.clone();
                menu.item(
                    meet_menu_row("schedule", Glyph::CALENDAR_ADD, "Schedule a meeting").on_click(move |_, _, cx| {
                        let _ = later.update(cx, Self::schedule_channel_meeting);
                    }),
                )
            },
        );
        div()
            .flex()
            .items_center()
            .child(selected_by("channel-meet-now", meet))
            .child(selected_by("channel-meet-menu", caret))
            .into_any_element()
    }

    /// What a call command of the switcher asks.
    fn call_command(&mut self, command: CallCommand, cx: &mut Context<Self>) {
        match command {
            CallCommand::CallChat => self.call_chat(cx),
            CallCommand::Join => self.join_call(cx),
            CallCommand::TestCall => self.ask_call(Ask::TestCall, cx),
            CallCommand::Answer => self.ask_call(Ask::Answer, cx),
            CallCommand::Decline => self.ask_call(Ask::Decline, cx),
            CallCommand::HangUp => self.ask_call(Ask::HangUp, cx),
            CallCommand::ToggleMute => self.ask_call(Ask::ToggleMute, cx),
            CallCommand::ToggleCamera => self.ask_call(Ask::ToggleCamera, cx),
            CallCommand::ToggleCaptions => self.ask_call(Ask::ToggleCaptions, cx),
        }
    }

    /// The chat header's call button (Call, or Join), when it has one.
    fn call_header_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Teams' header: an audio call is an icon; a running call's Join is the one filled button
        let (id, button, on): (&'static str, Button, ShellAction) = match self.call_button(cx) {
            "call" => (
                "chat-call",
                icon_button_with_key("chat-call", Glyph::PHONE, "Audio call", &CallChat),
                Self::call_chat,
            ),
            "meeting" => (
                "chat-join-meeting",
                icon_button_with_key("chat-join-meeting", Glyph::VIDEO, "Join the meeting", &JoinCall)
                    .label("Join")
                    .primary(),
                Self::join_call,
            ),
            "join" => (
                "chat-join",
                icon_button_with_key("chat-join", Glyph::PHONE, "Join the call", &JoinCall)
                    .label("Join")
                    .primary(),
                Self::join_call,
            ),
            _ => return None,
        };
        let button = button.on_click(cx.listener(move |this, _, _, cx| on(this, cx)));
        Some(selected_by(id, button).into_any_element())
    }

    /// The chat list changed: the header and the chat view's title follow it.
    fn list_changed(&mut self, cx: &mut Context<Self>) {
        self.trace_header(cx);
        self.trace_call_button(cx);
        self.give_title(cx);
    }

    /// Gives the chat view its chat's name as the list has it, when it is a name: a meeting's recap card is titled by
    /// it.
    fn give_title(&mut self, cx: &mut Context<Self>) {
        let (Some(chat), Main::Chat(id)) = (&self.chat, &self.main) else {
            return;
        };
        let title = self
            .list
            .read(cx)
            .row(id)
            .filter(|row| row.named)
            .map(|row| row.name.clone());
        chat.update(cx, |chat, cx| chat.set_title(title.clone(), cx));
        self.retitle_recap(title, cx);
    }

    /// Traces what the chat's header shows (`header shown`: the chat; its other member's presence, `none` for a chat
    /// that isn't 1:1, then `named` or `unnamed`, then `picture` or `no-picture`), when it changed. Nothing while no
    /// chat is shown, or before the list has its row.
    fn trace_header(&mut self, cx: &App) {
        let Main::Chat(id) = &self.main else {
            return;
        };
        let list = self.list.read(cx);
        let Some(row) = list.row(id) else {
            return;
        };
        let shown = HeaderShown {
            chat: id.clone(),
            presence: list
                .presence_of(row, cx)
                .map_or("none", |availability| presence::key(&availability)),
            named: row.named,
            picture: row.picture.is_some(),
        };
        if self.header_traced.as_ref() == Some(&shown) {
            return;
        }
        self.trace.record(
            &TraceLine::new("header", "shown")
                .conversation(&shown.chat)
                .kind(shown.presence)
                .kind(if shown.named { "named" } else { "unnamed" })
                .kind(if shown.picture { "picture" } else { "no-picture" }),
        );
        self.header_traced = Some(shown);
    }

    /// Shows the account's own presence in the account menu, and tells it of the window's use from now on.
    pub fn set_own_presence(&mut self, own_presence: Entity<OwnPresence>, cx: &mut Context<Self>) {
        let observing = cx.observe(&own_presence, |_, _, cx| cx.notify());
        self.own_presence = Some((own_presence, observing));
        cx.notify();
    }

    /// The account's own presence, once the desktop handed it in.
    #[must_use]
    pub fn own_presence(&self) -> Option<&Entity<OwnPresence>> {
        self.own_presence.as_ref().map(|(own, _)| own)
    }

    /// Whether the account menu is open.
    #[must_use]
    pub fn status_menu_open(&self) -> bool {
        self.status_menu
    }

    /// Opens the account menu (under the title bar's avatar), or closes it when it is open.
    pub fn toggle_status_menu(&mut self, cx: &mut Context<Self>) {
        self.toggle_status_menu_at(MenuAnchor::TitleBar, cx);
    }

    /// Opens the account menu at `anchor`, or closes it when it is open.
    pub fn toggle_status_menu_at(&mut self, anchor: MenuAnchor, cx: &mut Context<Self>) {
        self.status_anchor = anchor;
        self.status_menu = !self.status_menu;
        let change = if self.status_menu { "opened" } else { "closed" };
        self.trace.record(&TraceLine::new("status-menu", change));
        cx.notify();
    }

    /// The user chose `status` in the account menu: handed to the own presence (which sends it, or sends it again when
    /// it is the status already chosen and its report failed) and kept in the settings, the menu closed.
    pub fn choose_status(&mut self, status: ManualStatus, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("status-menu", "chosen").kind(status.key()));
        if let Some((own, _)) = &self.own_presence {
            own.update(cx, |own, cx| own.choose(status.choice(), cx));
        }
        // unchanged (the same status chosen again), the settings tell nobody: the own presence was told above
        let account = self.core.me().to_owned();
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.set_manual_status(&account, status), cx);
        });
        self.status_menu = false;
        cx.notify();
    }

    /// The account menu's "Sign out and remove": asked of the app (the menu closed).
    fn ask_remove_account(&mut self, cx: &mut Context<Self>) {
        self.status_menu = false;
        if let Some((accounts, _)) = &self.accounts {
            let key = self.core.me().to_owned();
            accounts.update(cx, |accounts, cx| accounts.ask_remove(&key, cx));
        }
        cx.notify();
    }

    /// Every keystroke in the shell's window counts as its use. Seen before any key binding (an interceptor): a key
    /// listener never hears a key a binding took (Ctrl+K, the chat view's keys), and keyboard-only use would go unseen.
    fn keystrokes_are_use(window: &Window, cx: &mut Context<Self>) -> Subscription {
        let shells_window = window.window_handle();
        let shell = cx.weak_entity();
        cx.intercept_keystrokes(move |_, window, cx| {
            if window.window_handle() == shells_window {
                // the shell gone: its window is going too
                let _ = shell.update(cx, Self::used);
            }
        })
    }

    /// The window was used: the own presence is told (Available again at once after idle).
    fn used(&mut self, cx: &mut Context<Self>) {
        if let Some((own, _)) = &self.own_presence {
            own.update(cx, OwnPresence::active);
        }
    }

    /// The Teams list.
    #[must_use]
    pub fn teams(&self) -> &Entity<TeamsList> {
        &self.teams
    }

    /// The list pane: the chats and the teams.
    #[must_use]
    pub fn sidebar(&self) -> &Entity<Sidebar> {
        &self.sidebar
    }

    /// The channel view of the channel opened, if one is.
    #[must_use]
    pub fn channel(&self) -> Option<&Entity<ChannelView>> {
        self.channel.as_ref().map(|(view, _)| view)
    }

    /// Opens team channel `id` ([`Shell::open_channel`]), named as the Teams list names it, or as
    /// "Channel" when the list doesn't show it (the teams not read yet, or unreadable).
    pub fn open_channel_id(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.teams.read(cx).chosen(&id).unwrap_or(ChannelChosen {
            id,
            name: SharedString::default(),
            team: SharedString::default(),
        });
        self.open_channel(chosen, window, cx);
    }

    /// What the Teams list tells besides a channel chosen.
    fn team_list_event(&mut self, event: &TeamsListEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            TeamsListEvent::AddChannel(team) => self.open_channel_dialog(team.clone(), window, cx),
            TeamsListEvent::Chosen(team, choice) => self.team_chosen(team.clone(), *choice, window, cx),
            TeamsListEvent::ChannelChosen(team, channel, choice) => {
                self.channel_chosen(team, channel, choice.clone(), window, cx);
            }
            TeamsListEvent::SeeAllTeams => self.open_teams_page(window, cx),
            TeamsListEvent::SeeAllChannels(team) => {
                self.open_manage_team(team.clone(), ManageTab::Channels, window, cx);
            }
            TeamsListEvent::SectionFailed => {
                self.show_toast("Couldn't save the section's expanded state. Try again.", cx);
            }
            TeamsListEvent::Opened(_) => {}
        }
    }

    /// "See all your teams": "Your teams and channels" in the conversation pane.
    pub fn open_teams_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.teams_page.is_none() {
            let (core, teams, trace) = (Arc::clone(&self.core), self.teams.clone(), Arc::clone(&self.trace));
            let page = cx.new(|cx| TeamsPage::new(core, teams, trace, cx));
            let events = cx.subscribe_in(
                &page,
                window,
                |this: &mut Self, _, event: &TeamsPageEvent, window, cx| match event {
                    TeamsPageEvent::CreateTeam => this.open_create_team(window, cx),
                    TeamsPageEvent::JoinTeam => this.open_join_team(window, cx),
                    TeamsPageEvent::Manage(team) => {
                        this.open_manage_team(team.clone(), ManageTab::Channels, window, cx);
                    }
                    TeamsPageEvent::Chosen(team, choice) => this.team_chosen((**team).clone(), *choice, window, cx),
                },
            );
            self.teams_page = Some((page, events));
        }
        self.show_main(Main::TeamsPage, window, cx);
        // the keyboard goes to the teams listed, as Teams opens the page on them
        if let Some((page, _)) = &self.teams_page {
            page.update(cx, |page, cx| page.focus_rows(window, cx));
        }
    }

    /// "Your teams and channels", once it was opened.
    #[must_use]
    pub fn teams_page(&self) -> Option<&Entity<TeamsPage>> {
        self.teams_page.as_ref().map(|(page, _)| page)
    }

    /// Manage team, while it is shown.
    #[must_use]
    pub fn manage_team(&self) -> Option<&Entity<ManageTeam>> {
        self.manage_team.as_ref().map(|(page, _)| page)
    }

    /// "Join a team", while it is shown.
    #[must_use]
    pub fn join_team(&self) -> Option<&Entity<JoinTeam>> {
        self.join_team.as_ref().map(|(page, _)| page)
    }

    /// The dialog of the team pages, while one is open.
    #[must_use]
    pub fn team_dialog(&self) -> Option<&gpui::AnyView> {
        self.team_dialog.as_ref().map(|(dialog, _)| dialog)
    }

    /// Closes the dialog of the team pages, if one is open.
    pub fn close_team_dialog(&mut self, cx: &mut Context<Self>) {
        self.team_dialog = None;
        cx.notify();
    }

    /// The toast showing.
    #[must_use]
    pub fn toast(&self) -> Option<&SharedString> {
        self.toast.as_ref().map(|(words, _)| words)
    }

    /// Manage team on `team`, on `tab`, in the conversation pane.
    pub fn open_manage_team(&mut self, team: String, tab: ManageTab, window: &mut Window, cx: &mut Context<Self>) {
        let (core, teams, trace) = (Arc::clone(&self.core), self.teams.clone(), Arc::clone(&self.trace));
        let shown = team.clone();
        let page = cx.new(|cx| ManageTeam::new(core, trace, teams, team, tab, cx));
        let events = cx.subscribe_in(
            &page,
            window,
            |this: &mut Self, _, event: &ManageTeamEvent, window, cx| match event {
                ManageTeamEvent::Chosen(team, choice) => this.team_chosen(team.clone(), *choice, window, cx),
                ManageTeamEvent::EditDetails(team) => {
                    let core = Arc::clone(&this.core);
                    let team = team.clone();
                    let dialog = cx.new(|cx| EditTeamDialog::new(core, team, window, cx));
                    this.show_team_dialog(&dialog, window, cx);
                }
                ManageTeamEvent::EditTag(team, tag) => {
                    let core = Arc::clone(&this.core);
                    let (team, tag) = (team.clone(), tag.clone());
                    let dialog = cx.new(|cx| TagDialog::new(core, team, tag, window, cx));
                    this.show_team_dialog(&dialog, window, cx);
                }
                ManageTeamEvent::OpenChannel(channel) => this.open_channel_id(channel.clone(), window, cx),
                ManageTeamEvent::Toast(words) => this.show_toast(words, cx),
                ManageTeamEvent::ChangePicture(team) => this.open_team_picture(team.clone(), window, cx),
                ManageTeamEvent::AddApp(team) => this.open_add_app(team.clone(), window, cx),
            },
        );
        self.manage_team = Some((page, events));
        self.show_main(Main::ManageTeam(shown), window, cx);
    }

    /// "Join a team", across both panes.
    pub fn open_join_team(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let page = cx.new(|cx| JoinTeam::new(core, window, cx));
        let events = cx.subscribe_in(
            &page,
            window,
            |this: &mut Self, _, event: &JoinTeamEvent, window, cx| match event {
                JoinTeamEvent::Back => this.open_teams_page(window, cx),
                JoinTeamEvent::CreateTeam => this.open_create_team(window, cx),
                JoinTeamEvent::Joined(words) => {
                    this.show_toast(words, cx);
                    this.open_teams_page(window, cx);
                }
            },
        );
        self.join_team = Some((page, events));
        self.show_main(Main::JoinTeam, window, cx);
    }

    /// "More create team options", across both panes.
    pub fn open_create_team_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let owned: Vec<teams_store::Team> = self
            .teams
            .read(cx)
            .teams()
            .unwrap_or_default()
            .iter()
            .filter(|team| team.owner && !team.archived && team.group_id.is_some())
            .cloned()
            .collect();
        let page = cx.new(|cx| CreateTeamMore::new(core, owned, window, cx));
        let events = cx.subscribe_in(
            &page,
            window,
            |this: &mut Self, _, event: &CreateTeamMoreEvent, window, cx| match event {
                CreateTeamMoreEvent::Back => this.open_teams_page(window, cx),
                CreateTeamMoreEvent::FromScratch => this.open_create_team(window, cx),
                CreateTeamMoreEvent::Created(words) => {
                    this.show_toast(words, cx);
                    this.open_teams_page(window, cx);
                }
            },
        );
        self.create_team_more = Some((page, events));
        self.show_main(Main::CreateTeamMore, window, cx);
    }

    /// "Change team picture" on `team`.
    pub fn open_team_picture(&mut self, team: teams_store::Team, window: &mut Window, cx: &mut Context<Self>) {
        let (core, teams) = (Arc::clone(&self.core), self.teams.clone());
        let dialog = cx.new(|cx| TeamPictureDialog::new(core, teams, team, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// "Get more apps" on `team`: the apps Manage team lists count as on it.
    pub fn open_add_app(&mut self, team: teams_store::Team, window: &mut Window, cx: &mut Context<Self>) {
        let installed: Vec<String> = self
            .manage_team
            .as_ref()
            .map(|(page, _)| page.read(cx).installed_apps())
            .unwrap_or_default();
        let core = Arc::clone(&self.core);
        let dialog = cx.new(|cx| AddAppDialog::new(core, team, installed, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// "More create team options", while it is shown.
    #[must_use]
    pub fn create_team_more(&self) -> Option<&Entity<CreateTeamMore>> {
        self.create_team_more.as_ref().map(|(page, _)| page)
    }

    /// Shows `main` in the conversation pane, the chat that was there put away.
    fn show_main(&mut self, main: Main, window: &mut Window, cx: &mut Context<Self>) {
        self.put_chat_away(cx);
        self.teams.update(cx, |teams, cx| teams.select(None, cx));
        self.list.update(cx, |list, cx| list.select(None, cx));
        self.main = main;
        self.hide_calendar(window, cx);
        self.close_switcher(window, cx);
        self.close_search(cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// "Create a team".
    pub fn open_create_team(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (core, trace) = (Arc::clone(&self.core), Arc::clone(&self.trace));
        let dialog = cx.new(|cx| CreateTeamDialog::new(core, trace, window, cx));
        self.show_team_dialog(&dialog, window, cx);
    }

    /// An entry of a team's "..." menu, wherever it was chosen.
    pub fn team_chosen(
        &mut self,
        team: teams_store::Team,
        choice: TeamChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (core, trace) = (Arc::clone(&self.core), Arc::clone(&self.trace));
        let confirm =
            |kind: Confirm, this: &mut Self, team: teams_store::Team, window: &mut Window, cx: &mut Context<Self>| {
                let dialog = cx.new(|cx| ConfirmDialog::new(core.clone(), trace.clone(), team, kind, window, cx));
                this.show_team_dialog(&dialog, window, cx);
            };
        match choice {
            TeamChoice::AddChannel => self.open_channel_dialog(team, window, cx),
            TeamChoice::Manage => self.open_manage_team(team.id, ManageTab::Channels, window, cx),
            TeamChoice::ManageTags => self.open_manage_team(team.id, ManageTab::Tags, window, cx),
            TeamChoice::AddMember => {
                let dialog = cx.new(|cx| AddMembersDialog::new(core.clone(), &trace, team, false, window, cx));
                self.show_team_dialog(&dialog, window, cx);
            }
            TeamChoice::CopyLink => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(team_link(&team)));
                self.show_toast("Link copied", cx);
            }
            TeamChoice::HideAll => confirm(Confirm::HideAll, self, team, window, cx),
            TeamChoice::Archive => confirm(Confirm::Archive, self, team, window, cx),
            TeamChoice::Leave => confirm(Confirm::Leave, self, team, window, cx),
            TeamChoice::Delete => confirm(Confirm::Delete, self, team, window, cx),
            TeamChoice::Show => self.show_team(&team, cx),
            TeamChoice::Restore => self.restore_team(&team, cx),
        }
    }

    /// A hidden team's Show: it comes back into the account's list.
    fn show_team(&mut self, team: &teams_store::Team, cx: &mut Context<Self>) {
        let reply = self.core.own_team_view(
            vec![teams_proto::OwnTeamView::ShowAll { team: team.id.clone() }],
            Some(teams_core::TeamChange::Hidden {
                team: team.id.clone(),
                hidden: false,
            }),
        );
        Self::toast_after(reply, "We couldn't show the team.", None, cx);
    }

    /// An archived team's Restore.
    fn restore_team(&mut self, team: &teams_store::Team, cx: &mut Context<Self>) {
        let reply = self.core.team_write(
            teams_proto::TeamWrite::Restore {
                group: team.group_id.clone().unwrap_or_default(),
            },
            Some(teams_core::TeamChange::Archived {
                team: team.id.clone(),
                archived: false,
            }),
        );
        Self::toast_after(
            reply,
            "We couldn't restore your team.",
            Some("Successfully restored team"),
            cx,
        );
    }

    /// Waits for `reply`, then says what went wrong, or `done`.
    fn toast_after<V: Send + 'static>(
        reply: teams_core::Reply<V>,
        failed: &'static str,
        done: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let words = if reply.await.is_ok() { done } else { Some(failed) };
            if let Some(words) = words {
                let _ = this.update(cx, |this, cx| this.show_toast(words, cx));
            }
        })
        .detach();
    }

    /// A word that shows for a few seconds at the bottom of the window.
    pub fn show_toast(&mut self, words: &'static str, cx: &mut Context<Self>) {
        if words.is_empty() {
            return;
        }
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        });
        self.toast = Some((words.into(), task));
        cx.notify();
    }

    /// Shows `dialog` over everything; what it tells is acted on here.
    pub(super) fn show_team_dialog<D>(&mut self, dialog: &Entity<D>, window: &mut Window, cx: &mut Context<Self>)
    where
        D: Render + EventEmitter<TeamDialogEvent>,
    {
        self.close_switcher(window, cx);
        if self.team_dialog.is_none() {
            self.dialog_return.keep(window, cx);
        }
        let events = cx.subscribe_in(
            dialog,
            window,
            |this: &mut Self, _, event: &TeamDialogEvent, window, cx| {
                this.team_dialog = None;
                this.focus.focus(window, cx);
                if !matches!(event, TeamDialogEvent::Created(_)) {
                    this.dialog_return.give_back(window, cx);
                }
                match event {
                    TeamDialogEvent::Closed => {}
                    TeamDialogEvent::MoreOptions => this.open_create_team_more(window, cx),
                    TeamDialogEvent::NotificationSettings => {
                        this.open_settings_page(Page::Notifications, window, cx);
                    }
                    TeamDialogEvent::Workflows { see_all } => this.open_workflows(*see_all, window, cx),
                    TeamDialogEvent::RunWorkflow { flow, message } => {
                        this.run_workflow(flow.clone(), message.clone(), window, cx);
                    }
                    TeamDialogEvent::Created(team) => {
                        let (core, trace) = (Arc::clone(&this.core), Arc::clone(&this.trace));
                        let team = team.clone();
                        let dialog = cx.new(|cx| AddMembersDialog::new(core, &trace, team, true, window, cx));
                        this.show_team_dialog(&dialog, window, cx);
                    }
                    TeamDialogEvent::Changed { team, words } => {
                        this.teams.update(cx, TeamsList::reload);
                        this.show_toast(words, cx);
                        let team = team.clone();
                        if let Some((page, _)) = &this.manage_team
                            && page.read(cx).team_id() == team
                        {
                            page.update(cx, ManageTeam::refresh);
                        }
                    }
                    TeamDialogEvent::Gone { team, words } => {
                        this.show_toast(words, cx);
                        if this.main == Main::ManageTeam(team.clone()) {
                            this.manage_team = None;
                            this.open_teams_page(window, cx);
                        }
                    }
                }
                cx.notify();
            },
        );
        self.team_dialog = Some((dialog.clone().into(), events));
        cx.notify();
    }

    /// A team page's dialog over everything, while one is open.
    fn team_dialog_view(&self) -> Option<gpui::Stateful<gpui::Div>> {
        self.team_dialog.as_ref().map(|(dialog, _)| {
            div()
                .id("team-dialog-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .bg(gpui::rgba(0x0000_0080))
                .child(dialog.clone())
        })
    }

    /// The toast, at the bottom of the window.
    fn toast_view(&self, palette: Palette) -> Option<gpui::Div> {
        self.toast.as_ref().map(|(words, _)| {
            div()
                .absolute()
                .bottom(px(24.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .debug_selector(|| "toast".to_owned())
                        .px_4()
                        .py_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .text_color(rgb(palette.foreground))
                        .shadow_lg()
                        .child(words.clone()),
                )
        })
    }

    /// Opens the team's "Create a channel" dialog over everything; it tells when it closes, and on which channel.
    pub fn open_channel_dialog(&mut self, team: teams_store::Team, window: &mut Window, cx: &mut Context<Self>) {
        self.close_switcher(window, cx);
        let (core, trace) = (Arc::clone(&self.core), Arc::clone(&self.trace));
        self.dialog_return.keep(window, cx);
        let dialog = cx.new(|cx| ChannelDialog::new(core, trace, team, window, cx));
        let closes = cx.subscribe_in(&dialog, window, |this: &mut Self, _, event, window, cx| {
            this.channel_dialog = None;
            this.focus.focus(window, cx);
            match event {
                ChannelDialogEvent::Open(id) => this.open_channel_id(id.clone(), window, cx),
                ChannelDialogEvent::Closed => this.dialog_return.give_back(window, cx),
            }
            cx.notify();
        });
        self.channel_dialog = Some((dialog, closes));
        cx.notify();
    }

    /// The team's "Create a channel" dialog, while it is open.
    #[must_use]
    pub fn channel_dialog(&self) -> Option<&Entity<ChannelDialog>> {
        self.channel_dialog.as_ref().map(|(dialog, _)| dialog)
    }

    /// The "Create a channel" dialog over everything, while it is open.
    fn channel_dialog_view(&self) -> Option<gpui::Stateful<gpui::Div>> {
        self.channel_dialog().cloned().map(|dialog| {
            div()
                .id("channel-dialog-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .occlude()
                .bg(gpui::rgba(0x0000_0080))
                .child(dialog)
        })
    }

    /// Opens a team's channel in the conversation pane; the chat it hides is kept (its draft stays) but
    /// isn't shown, so no read receipt goes for it. A channel is never kept as the selected chat. The switcher and the
    /// search close.
    pub fn open_channel(&mut self, chosen: ChannelChosen, window: &mut Window, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("channel", "opened").conversation(&chosen.id));
        let shown = self
            .channel
            .as_ref()
            .is_some_and(|(view, _)| view.read(cx).channel() == chosen.id);
        let listed = self.teams.read(cx).chosen(&chosen.id).is_some();
        self.channel_listed = listed || shown && self.channel_listed;
        if shown {
            // opened again: synced again (an earlier sync may have failed, and no catch-up reads a channel)
            if let Some((view, kept)) = &mut self.channel {
                if !chosen.team.is_empty() {
                    *kept = chosen.clone();
                }
                view.update(cx, ChannelView::sync);
            }
        } else {
            let drafts = self.channel_drafts(&chosen.id, cx);
            let (parts, id, name) = (self.parts(), chosen.id.clone(), chosen.name.clone());
            let view = cx.new(|cx| ChannelView::new(parts, id, name, drafts, window, cx));
            self.channel_events = Some(Self::follow_channel(&view, window, cx));
            self.channel = Some((view, chosen.clone()));
        }
        // a meeting card names where it is
        if let Some((view, kept)) = &self.channel {
            let (team, name) = (kept.team.clone(), kept.name.clone());
            view.update(cx, |view, cx| view.set_place(team, name, cx));
        }
        self.read_channel_meetup(chosen.id.clone(), cx);
        self.teams
            .update(cx, |teams, cx| teams.select(Some(chosen.id.clone()), cx));
        // the channel is the list's selection: no chat row is highlighted, and the channel's own row under "Favorites"
        // is, as Teams highlights it, whether it is listed yet or comes with a later load
        self.list
            .update(cx, |list, cx| list.select(Some(chosen.id.clone()), cx));
        // a chat view of the channel itself (kept as the selected chat by an earlier build) is dropped, and the channel
        // isn't the selected chat any more
        if self
            .chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).conversation() == chosen.id)
        {
            self.chat = None;
            self.chat_events = None;
        }
        let account = self.core.me().to_owned();
        if self.settings.read(cx).settings().selected_chat_of(&account) == Some(chosen.id.as_str()) {
            self.settings.update(cx, |settings, cx| {
                settings.change(|settings| settings.set_selected_chat(&account, None), cx);
            });
        }
        self.put_chat_away(cx);
        self.main = Main::Channel(chosen.id);
        self.hide_calendar(window, cx);
        self.close_switcher(window, cx);
        self.close_search(cx);
        self.focus.focus(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn list(&self) -> &Entity<ChatList> {
        &self.list
    }

    /// The settings view the shell shows across both panes when Settings is chosen.
    #[must_use]
    pub fn settings_view(&self) -> &Entity<SettingsView> {
        &self.settings_view
    }

    /// The settings the shell keeps the selected chat in.
    #[must_use]
    pub fn settings(&self) -> &Entity<SettingsModel> {
        &self.settings
    }

    #[must_use]
    pub fn main(&self) -> &Main {
        &self.main
    }

    /// The panes' sizes (the list pane's first).
    #[must_use]
    pub fn panes(&self) -> &Entity<ResizableState> {
        &self.panes
    }

    /// The switcher, while it is open.
    #[must_use]
    pub fn switcher(&self) -> Option<&Entity<Switcher>> {
        self.switcher.as_ref().map(|(switcher, _)| switcher)
    }

    /// The title bar's search.
    #[must_use]
    pub fn search(&self) -> &Entity<GlobalSearch> {
        &self.search_box
    }

    /// Ctrl+E, Ctrl+Shift+F (`mode` search) and Ctrl+Alt+G (`mode` go-to): the keyboard in the title bar's search.
    pub fn focus_search(&mut self, mode: SearchMode, window: &mut Window, cx: &mut Context<Self>) {
        self.close_switcher(window, cx);
        self.status_menu = false;
        self.search_box
            .update(cx, |search, cx| search.focus_box(mode, window, cx));
        cx.notify();
    }

    /// The search's results across both panes, the keyboard on them (Escape leaves them); the chat they hide is kept
    /// but isn't shown.
    pub fn show_search_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_box.read(cx).results().is_none() {
            return;
        }
        self.hide_calendar(window, cx);
        if self.main != Main::Search {
            let before = std::mem::replace(&mut self.main, Main::Search);
            if !matches!(before, Main::Settings | Main::Calendar) {
                self.before_settings = before;
            }
            self.put_chat_away(cx);
        }
        let focus = self.search_box.read(cx).results_focus().clone();
        focus.focus(window, cx);
        cx.notify();
    }

    /// `query` typed in the title bar's search and entered: its results shown ([`Shell::show_search_results`]). An
    /// empty query changes nothing.
    pub fn search_for(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.close_switcher(window, cx);
        self.search_box.update(cx, |search, cx| {
            search.set_query(query, window, cx);
            search.show_results(global_search::Tab::All, cx);
        });
        self.show_search_results(window, cx);
    }

    /// Escape on the results: back where the user was.
    pub fn leave_search_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.main != Main::Search {
            return;
        }
        self.search_box
            .update(cx, |search, cx| search.clear_results(window, cx));
        if self.history.can_go_back() {
            self.go_back(window, cx);
        } else {
            self.main = Main::Nothing;
            self.focus.focus(window, cx);
        }
        if self.main == Main::Search {
            self.main = Main::Nothing;
        }
        // the results are gone: Back and Forward never stop at them again
        self.history.forget(&Main::Search);
        cx.notify();
    }

    /// The chat view of the chat opened, if one is.
    #[must_use]
    pub fn chat(&self) -> Option<&Entity<ChatView>> {
        self.chat.as_ref()
    }

    /// Opens a chat in the conversation pane and keeps it as the selected chat; the switcher closes. A team's channel
    /// the Teams list shows opens as a channel instead ([`Shell::open_channel`]), never in a chat view.
    pub fn open_chat(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chosen) = self.teams.read(cx).chosen(&id) {
            self.open_channel(chosen, window, cx);
            return;
        }
        self.trace.record(&TraceLine::new("chat", "opened").conversation(&id));
        self.people.open = None;
        self.hide_calendar(window, cx);
        self.reset_meeting_tabs(&id, cx);
        let shown = self.chat.as_ref().map(|chat| chat.read(cx).conversation() == id);
        if shown != Some(true) {
            self.keep_draft(cx);
            let parts = self.parts();
            let conversation = id.clone();
            let compose = self.drafts.remove(&id);
            let chat = cx.new(|cx| ChatView::with_compose(parts, conversation, compose, window, cx));
            self.chat_events = Some(Self::follow_chat(&chat, window, cx));
            self.chat = Some(chat);
        }
        self.list.update(cx, |list, cx| list.select(Some(id.clone()), cx));
        self.teams.update(cx, |teams, cx| teams.select(None, cx));
        let (account, kept) = (self.core.me().to_owned(), id.clone());
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.set_selected_chat(&account, Some(kept)), cx);
        });
        self.main = Main::Chat(id);
        self.trace_header(cx);
        self.give_title(cx);
        self.trace_call_button(cx);
        self.close_switcher(window, cx);
        self.close_search(cx);
        // the chat opened takes what is typed next, and is shown (read receipts may go); a meeting chat opened again on
        // its Recap or Shared tab keeps that tab, its chat neither shown nor typed into: the shell takes the keyboard
        // (what had it, the calendar or another view, is no longer drawn)
        if let Some(chat) = &self.chat {
            let shown = self.chat_tab_shown();
            chat.update(cx, |chat, cx| chat.set_shown(shown, cx));
        }
        self.take_focus(window, cx);
        cx.notify();
    }

    /// The chat shown was put away (another view took the pane): it isn't shown (no read receipts go), and a meeting
    /// recap's recording stops playing.
    fn put_chat_away(&self, cx: &mut App) {
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(false, cx));
        }
        self.pause_meeting_tabs(cx);
    }

    /// Keeps the boxes holding a draft of the channel shown (its posts' and its threads'), which another channel's view
    /// is about to replace, and gives back channel `id`'s kept ones, by the conversation each writes to.
    fn channel_drafts(
        &mut self,
        id: &str,
        cx: &App,
    ) -> std::collections::HashMap<String, Entity<crate::compose::Compose>> {
        self.drafts.retain(|_, compose| compose.read(cx).has_draft(cx));
        if let Some((view, _)) = &self.channel {
            self.drafts.extend(view.read(cx).drafts(cx));
        }
        // a channel's posts' box is kept by the channel, a thread's by its post's chain, stored in the channel
        let keys: Vec<String> = self
            .drafts
            .keys()
            .filter(|key| teams_proto::stored_conversation(key) == id)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| self.drafts.remove_entry(&key))
            .collect()
    }

    fn keep_draft(&mut self, cx: &App) {
        self.drafts.retain(|_, compose| compose.read(cx).has_draft(cx));
        if let Some(chat) = &self.chat {
            let chat = chat.read(cx);
            let compose = chat.compose();
            if compose.read(cx).has_draft(cx) {
                self.drafts.insert(chat.conversation().to_owned(), compose.clone());
            } else {
                self.drafts.remove(chat.conversation());
            }
        }
    }

    /// Opens a message's chat ([`Shell::open_chat`]) at the message: the chat view reads back to it, shows it and
    /// selects it ([`ChatView::reveal`]); a channel's opens at the message's thread ([`ChannelView::reveal`]).
    pub fn open_message(&mut self, target: HitTarget, window: &mut Window, cx: &mut Context<Self>) {
        if teams_proto::is_team_thread_id(&target.conversation) {
            self.open_channel_id(target.conversation.clone(), window, cx);
        } else {
            self.open_chat(target.conversation.clone(), window, cx);
        }
        if self.main == Main::Channel(target.conversation.clone()) {
            if let Some((channel, _)) = &self.channel {
                let thread = target.thread.unwrap_or_else(|| target.message.clone());
                channel.update(cx, |channel, cx| {
                    channel.reveal(thread, target.message, target.arrival_time, window, cx);
                });
            }
            return;
        }
        if self.main != Main::Chat(target.conversation.clone()) {
            return;
        }
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.reveal(target.arrival_time, target.message, cx));
        }
    }

    /// Follows what `chat` tells: a forwarded message's original asked for ([`Shell::open_original`]).
    fn follow_chat(chat: &Entity<ChatView>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(chat, window, |this: &mut Self, _, event, window, cx| match event {
            ChatViewEvent::OpenOriginal(original) => this.open_original(original.clone(), window, cx),
            ChatViewEvent::RunWorkflow { flow, message } => {
                this.run_workflow(flow.clone(), message.clone(), window, cx);
            }
            ChatViewEvent::CreateWorkflows(message) => this.open_message_workflows(message.clone(), window, cx),
            ChatViewEvent::OpenRecap {
                recording,
                transcribed,
                open,
            } => this.open_recap(recording.clone(), transcribed.clone(), *open, window, cx),
            ChatViewEvent::OpenSavedTranscript(id) => this.open_saved_transcript(*id, window, cx),
            // a chat has no channel's thread or meeting card
            ChatViewEvent::Action { .. } | ChatViewEvent::OpenThread { .. } | ChatViewEvent::EditMeeting { .. } => {}
        })
    }

    /// Follows what `channel` tells: a forwarded post's original asked for ([`Shell::open_original`]).
    fn follow_channel(channel: &Entity<ChannelView>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(channel, window, |this: &mut Self, _, event, window, cx| match event {
            ChannelViewEvent::OpenOriginal(original) => this.open_original(original.clone(), window, cx),
            ChannelViewEvent::EditMeeting { ical_uid } => this.edit_channel_meeting(ical_uid.clone(), cx),
            ChannelViewEvent::RunWorkflow { flow, message } => {
                this.run_workflow(flow.clone(), message.clone(), window, cx);
            }
            ChannelViewEvent::CreateWorkflows(message) => this.open_message_workflows(message.clone(), window, cx),
        })
    }

    /// Opens a forwarded message's original: in the app at the message ([`Shell::open_message`]) when the chat list
    /// has its chat and the store holds the message (the chat view reads back only as far as the store goes), else in
    /// Teams on the web through GPUI's `open_url` when there is a link to it; without one, its chat in the app when
    /// the list has it.
    pub fn open_original(&mut self, original: Original, window: &mut Window, cx: &mut Context<Self>) {
        let Original { target, web_link } = original;
        if self.list.read(cx).row(&target.conversation).is_none() {
            if let Some(link) = web_link {
                cx.open_url(&link);
            }
            return;
        }
        let stored = self.core.message(target.conversation.clone(), target.message.clone());
        cx.spawn_in(window, async move |this, cx| {
            let stored = matches!(stored.await, Ok(Some(_)));
            let _ = this.update_in(cx, |shell, window, cx| match web_link {
                Some(link) if !stored => cx.open_url(&link),
                _ => shell.open_message(target, window, cx),
            });
        })
        .detach();
    }

    /// The presence the chat's header shows: the chat opened's other member's, when it is a 1:1 chat (the list's row
    /// for it names them), as the presence model holds it.
    #[must_use]
    pub fn header_presence(&self, cx: &App) -> Option<Availability> {
        let Main::Chat(id) = &self.main else {
            return None;
        };
        let list = self.list.read(cx);
        let row = list.row(id)?;
        list.presence_of(row, cx)
    }

    /// What a chat view is made of: the shell's own.
    fn parts(&self) -> ShellParts {
        ShellParts {
            core: Arc::clone(&self.core),
            models: self.models.clone(),
            settings: self.settings.clone(),
            trace: Arc::clone(&self.trace),
        }
    }

    /// Shows the settings in place of the list and the conversation pane; the chat they hide is kept (its draft
    /// stays), but isn't shown, so no read receipt goes for it.
    pub fn show_settings(&mut self, cx: &mut Context<Self>) {
        if self.main != Main::Settings {
            self.trace.record(&TraceLine::new("settings", "opened"));
            let before = std::mem::replace(&mut self.main, Main::Settings);
            if !matches!(before, Main::Calendar | Main::Search) {
                self.before_settings = before;
            }
            self.put_chat_away(cx);
            self.leave_workflows(cx);
            cx.notify();
        }
    }

    /// The rail's Chat (and Ctrl+2): leaves the settings, the calendar, the search results or the Workflows app for
    /// what they hid (the chat or the channel shown before them, opened again), or nothing; elsewhere it does nothing.
    pub fn show_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_chats_pane(window, cx);
        if self.list_place != ListPlace::Chats {
            self.list_place = ListPlace::Chats;
            self.sync_activity_shown(window, cx);
            cx.notify();
        }
        // the keyboard goes to the list, as Teams' Ctrl+2 takes it there
        if self.list_shown {
            self.sidebar.update(cx, |sidebar, cx| sidebar.focus_list(window, cx));
        }
    }

    fn show_chats_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.main,
            Main::Settings | Main::Calendar | Main::Search | Main::Workflows
        ) {
            return;
        }
        self.hide_calendar(window, cx);
        match std::mem::replace(&mut self.before_settings, Main::Nothing) {
            Main::Chat(id) => self.open_chat(id, window, cx),
            Main::Channel(id) => self.open_channel_id(id, window, cx),
            Main::NewChat => self.open_new_chat(window, cx),
            Main::Nothing
            | Main::Settings
            | Main::Calendar
            | Main::Search
            | Main::TeamsPage
            | Main::ManageTeam(_)
            | Main::JoinTeam
            | Main::CreateTeamMore
            | Main::Workflows => {
                self.main = Main::Nothing;
                cx.notify();
            }
        }
    }

    /// The base font size a step larger (`step` 1), smaller (-1) or back to the default (`None`), kept in the settings.
    pub fn step_font(&mut self, step: Option<i32>, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, cx| {
            settings.change(
                |settings| {
                    let appearance = &mut settings.appearance;
                    appearance.font_size = step.map_or(theme::FONT_SIZE, |step| appearance.stepped_font_size(step));
                },
                cx,
            );
        });
    }

    /// The handlers of the shell's own keys (the switcher, the search, the accounts, the zoom) on its root.
    fn shell_keys(shell: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        shell
            .on_action(cx.listener(|this, _: &ToggleSwitcher, window, cx| this.toggle_switcher(window, cx)))
            .on_action(cx.listener(|this, _: &NewMessage, window, cx| this.open_new_chat(window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.focus_search(SearchMode::Search, window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToChat, window, cx| this.focus_search(SearchMode::GoTo, window, cx)))
            .on_action(cx.listener(|this, _: &GoBack, window, cx| this.go_back(window, cx)))
            .on_action(cx.listener(|this, _: &GoForward, window, cx| this.go_forward(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleListPane, _, cx| this.toggle_list_pane(cx)))
            .on_action(cx.listener(|this, _: &NextAccount, _, cx| this.cycle_account(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousAccount, _, cx| this.cycle_account(false, cx)))
            .on_action(cx.listener(|this, _: &FontLarger, _, cx| this.step_font(Some(1), cx)))
            .on_action(cx.listener(|this, _: &FontSmaller, _, cx| this.step_font(Some(-1), cx)))
            .on_action(cx.listener(|this, _: &FontReset, _, cx| this.step_font(None, cx)))
            .on_action(cx.listener(|this, _: &ShowActivity, window, cx| this.show_activity(window, cx)))
            .on_action(cx.listener(|this, _: &ShowChats, window, cx| this.show_chats(window, cx)))
            .on_action(cx.listener(|this, _: &ShowCalendar, window, cx| this.show_calendar(window, cx)))
    }

    /// Opens the switcher, or closes it when it is open.
    pub fn toggle_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() {
            self.close_switcher(window, cx);
            return;
        }
        self.close_search(cx);
        let (list, teams) = (self.list.clone(), self.teams.clone());
        let shown = match &self.main {
            Main::Chat(id) => Some(id.clone()),
            _ => None,
        };
        let calls = (self.models.calls.clone(), shown);
        let switcher = cx.new(|cx| Switcher::new(list, teams, calls, window, cx));
        let events = cx.subscribe_in(&switcher, window, |this: &mut Self, _, event, window, cx| match event {
            SwitcherEvent::Open(id) => this.open_chat(id.clone(), window, cx),
            SwitcherEvent::OpenChannel(id) => this.open_channel_id(id.clone(), window, cx),
            SwitcherEvent::Dismissed => this.close_switcher(window, cx),
            SwitcherEvent::CopyDiagnostics => this.copy_diagnostics(window, cx),
            SwitcherEvent::SeeAllTeams => this.open_teams_page(window, cx),
            SwitcherEvent::CreateTeam => this.open_create_team(window, cx),
            SwitcherEvent::JoinTeam => this.open_join_team(window, cx),
            SwitcherEvent::Team(id, choice) => {
                this.close_switcher(window, cx);
                let team = this
                    .teams
                    .read(cx)
                    .teams()
                    .and_then(|teams| teams.iter().find(|team| team.id == *id).cloned());
                if let Some(team) = team {
                    this.team_chosen(team, *choice, window, cx);
                }
            }
            SwitcherEvent::SeeMoreChats => {
                this.close_switcher(window, cx);
                this.list.update(cx, ChatList::see_more);
            }
            SwitcherEvent::SetStatus(status) => {
                this.choose_status(*status, cx);
                this.close_switcher(window, cx);
            }
            SwitcherEvent::Call(command) => {
                this.close_switcher(window, cx);
                this.call_command(*command, cx);
            }
        });
        switcher.update(cx, |switcher, cx| switcher.focus(window, cx));
        self.switcher = Some((switcher, events));
        self.trace.record(&TraceLine::new("switcher", "opened"));
        cx.notify();
    }

    /// The palette's "Copy diagnostics": the text on the clipboard, the switcher closed, and the settings' Diagnostics
    /// shown, where what was copied is told (said plainly when it holds full requests and responses).
    fn copy_diagnostics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings.update(cx, |settings, cx| {
            settings.copy_diagnostics(cx);
        });
        self.trace.record(&TraceLine::new("diagnostics", "copied"));
        self.close_switcher(window, cx);
        self.open_settings_page(Page::Diagnostics, window, cx);
    }

    /// Closes the switcher; the shell takes the focus back only when the switcher had it (one moved elsewhere, into
    /// the compose box say, stays there).
    fn close_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((switcher, _)) = self.switcher.take() {
            self.trace.record(&TraceLine::new("switcher", "closed"));
            if switcher.read(cx).holds_focus(window, cx) {
                self.focus.focus(window, cx);
            }
            cx.notify();
        }
    }

    /// Closes the search's suggestions (its results stay until left).
    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_box.update(cx, GlobalSearch::close_suggestions);
    }

    /// A rail item on the frame: its icon over a small caption in the frame's ink, faded unless chosen; the chosen one
    /// semibold on a lit chip with a mark on its left edge, both growing in as it is chosen.
    fn rail_button(
        id: &'static str,
        item: RailItem,
        chosen: bool,
        palette: Palette,
        cx: &mut Context<Self>,
        on: fn(&mut Self, &mut Window, &mut Context<Self>),
    ) -> AnyElement {
        let ink = rgb(palette.on_frame);
        // the chosen place: a lit chip behind its icon and a bar at the rail's edge, both growing in as the place is
        // chosen and shrinking away as another is
        let lit = div()
            .absolute()
            .top(px(2.0))
            .bottom(px(2.0))
            .left(px(6.0))
            .right(px(6.0))
            .rounded_lg();
        let lit = crate::motion::toggle(
            lit,
            SharedString::from(format!("{id}-lit")),
            chosen,
            cx,
            move |lit, t| lit.bg(ink.opacity(0.18 * t.clamp(0.0, 1.2))),
        );
        let mark = div()
            .absolute()
            .left_0()
            .top(px(RAIL_ITEM_HEIGHT / 2.0))
            .w(px(3.0))
            .rounded_r_sm()
            .bg(ink);
        let mark = crate::motion::toggle(mark, SharedString::from(format!("{id}-mark")), chosen, cx, |mark, t| {
            let height = RAIL_ITEM_HEIGHT / 2.0 * t.max(0.0);
            mark.h(px(height)).mt(px(-height / 2.0))
        });
        div()
            .debug_selector(move || id.to_owned())
            .id(id)
            .relative()
            .w_full()
            .h(px(RAIL_ITEM_HEIGHT))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_0p5()
            .cursor_pointer()
            .text_color(ink.opacity(if chosen { 1.0 } else { 0.72 }))
            .when(chosen, |button| button.font_weight(gpui::FontWeight::SEMIBOLD))
            .hover(move |style| style.text_color(ink))
            .on_click(cx.listener(move |this, _, window, cx| on(this, window, cx)))
            .child(lit)
            .child(mark)
            .child(Icon::default().data(item.icon).size(px(RAIL_ICON)))
            .child(div().text_size(px(10.0)).child(item.label))
            .into_any_element()
    }

    fn rail_view(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let place = !matches!(self.main, Main::Settings | Main::Calendar | Main::Workflows);
        let chats = place && self.list_place == ListPlace::Chats;
        let activity = place && self.list_place == ListPlace::Activity;
        let unseen = self.activity.0.read(cx).unseen();
        div()
            .debug_selector(|| "rail".to_owned())
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .py_2()
            // on the frame, whose gradient the shell draws behind it
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(Self::rail_button(
                        "rail-activity",
                        RailItem::ACTIVITY,
                        activity,
                        palette,
                        cx,
                        Self::show_activity,
                    ))
                    .children((unseen > 0).then(|| rail_count("rail-activity-count", unseen))),
            )
            .child(Self::rail_button(
                "rail-chats",
                RailItem::CHAT,
                chats,
                palette,
                cx,
                Self::show_chats,
            ))
            .child(Self::rail_button(
                "rail-calendar",
                RailItem::CALENDAR,
                self.main == Main::Calendar,
                palette,
                cx,
                Self::show_calendar,
            ))
            .children(self.app_rail(palette, cx))
            .child(div().flex_1())
            .children(self.account_rail(palette, cx))
            .child(Self::rail_button(
                "rail-settings",
                RailItem::SETTINGS,
                self.main == Main::Settings,
                palette,
                cx,
                Self::open_settings,
            ))
    }

    /// What the account's own presence reports now, as the account menu shows it; `None` before the desktop handed it
    /// in.
    #[must_use]
    pub fn own_reporting(&self, cx: &App) -> Option<Reporting> {
        self.own_presence().map(|own| own.read(cx).reporting(cx))
    }

    /// The rail's account avatars (see [`crate::accounts`]) and its "+", once the accounts were handed in; nothing
    /// before.
    fn account_rail(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (accounts, _) = self.accounts.as_ref()?;
        let accounts = accounts.read(cx);
        let me = self.core.me();
        let faces: Vec<AccountFace> = accounts
            .tiles()
            .iter()
            .enumerate()
            .map(|(index, tile)| AccountFace::of(index, tile, tile.key == me, cx))
            .collect();
        let can_add = accounts.can_add();
        let avatars: Vec<AnyElement> = faces
            .into_iter()
            .map(|face| self.account_avatar(face, palette, cx))
            .collect();
        let add = can_add.then(|| {
            let accounts = self.accounts.as_ref().map(|(accounts, _)| accounts.clone());
            div()
                .debug_selector(|| "rail-add-account".to_owned())
                .id("rail-add-account")
                .size(px(ACCOUNT_AVATAR))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(rgb(palette.on_frame).opacity(0.4))
                .text_color(rgb(palette.on_frame).opacity(0.72))
                .cursor_pointer()
                .hover(move |style| {
                    style
                        .bg(rgb(palette.on_frame).opacity(0.15))
                        .text_color(rgb(palette.on_frame))
                })
                .tooltip(|window, cx| Tooltip::new("Add an account").build(window, cx))
                .on_click(move |_, _, cx| {
                    if let Some(accounts) = &accounts {
                        accounts.update(cx, Accounts::ask_add);
                    }
                })
                .child(Glyph::PLUS.icon().size(px(18.0)))
        });
        Some(
            div()
                .debug_selector(|| "rail-accounts".to_owned())
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .pb_2()
                .children(avatars)
                .children(add)
                .into_any_element(),
        )
    }

    /// One account's avatar on the rail: its picture or initials, its own presence's dot, the account shown marked
    /// (a click opens its account menu: selector `rail-status`), another with its unread total (a click shows it:
    /// selector `rail-account-<n>`), a signed-out one faded.
    fn account_avatar(&self, account: AccountFace, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let AccountFace {
            index,
            key,
            name,
            detail,
            picture,
            reporting,
            unread,
            signed_out,
            shown,
        } = account;
        let selector: SharedString = if shown {
            "rail-status".into()
        } else {
            format!("rail-account-{index}").into()
        };
        let face = people::avatar(Some(&name), picture.as_ref(), Size::Medium);
        let dot = reporting.map(|reporting| {
            let dot = presence::dot(&shown_as(reporting), palette)
                .absolute()
                .right(px(-2.0))
                .bottom(px(-2.0));
            if shown {
                dot.debug_selector(|| "rail-status-dot".to_owned())
            } else {
                dot
            }
        });
        let badge = (!shown && unread > 0).then(|| account_unread(index, unread));
        let tip: SharedString = format!("{name}\n{detail}").into();
        div()
            .debug_selector({
                let selector = selector.clone();
                move || selector.to_string()
            })
            .id(selector)
            .relative()
            .flex_none()
            .p(px(2.0))
            .rounded_full()
            .border_2()
            .border_color(if shown {
                rgb(palette.on_frame).opacity(0.9)
            } else {
                rgb(palette.on_frame).opacity(0.0)
            })
            .cursor_pointer()
            .when(signed_out, |avatar| avatar.opacity(0.45))
            .when(shown && self.status_menu, |avatar| {
                avatar.bg(rgb(palette.on_frame).opacity(0.3))
            })
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.core.me() == key {
                    this.toggle_status_menu_at(MenuAnchor::Rail, cx);
                } else if let Some((accounts, _)) = &this.accounts {
                    accounts.update(cx, |accounts, cx| accounts.switch(&key, cx));
                }
            }))
            .child(face)
            .children(dot)
            .children(badge)
            .into_any_element()
    }

    /// The account menu's account: its name and what tells it apart above the statuses, and its "Sign out and remove"
    /// below them in an app that permits removal; neither before the accounts were handed in.
    fn account_menu_parts(&self, palette: Palette, cx: &mut Context<Self>) -> (Option<gpui::Div>, Option<gpui::Div>) {
        let account = self.accounts.as_ref().and_then(|(accounts, _)| {
            let accounts = accounts.read(cx);
            let tile = accounts.tile(self.core.me())?;
            Some((tile.name.clone(), tile.detail.clone(), accounts.can_remove()))
        });
        let header = account.as_ref().map(|(name, detail, _)| {
            div()
                .debug_selector(|| "status-menu-account".to_owned())
                .px_2()
                .pt_1()
                .pb_2()
                .mb_1()
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(div().font_weight(gpui::FontWeight::BOLD).child(name.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(detail.clone()),
                )
        });
        let remove = account.filter(|(_, _, can_add)| *can_add).map(|_| {
            div()
                .w_full()
                .mt_1()
                .pt_1()
                .border_t_1()
                .border_color(rgb(palette.border))
                .child(
                    menu_row("status-menu-remove-account", palette)
                        .text_color(rgb(theme::DANGER))
                        .child(Glyph::LOG_OUT.icon().size(px(16.0)))
                        .child("Sign out and remove")
                        .on_click(cx.listener(|this, _, _, cx| this.ask_remove_account(cx))),
                )
        });
        (header, remove)
    }

    /// The account menu: the statuses the user may set (the one set marked), what is reported, and the last failed
    /// report.
    fn status_menu_view(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.settings.read(cx).settings().manual_status_of(self.core.me());
        let entries = ManualStatus::ALL.map(|status| {
            let check = (status == chosen).then(|| Glyph::CHECK.icon().size(px(14.0)).text_color(rgb(palette.brand)));
            menu_row(format!("status-menu-{}", status.key()), palette)
                .when(status == chosen, |row| row.font_weight(gpui::FontWeight::SEMIBOLD))
                .child(settings_view::status_mark(status, palette))
                .child(div().flex_1().child(status.label()))
                .children(check)
                .on_click(cx.listener(move |this, _, _, cx| this.choose_status(status, cx)))
        });
        let reported = self.own_reporting(cx).map(|reporting| {
            div()
                .debug_selector(|| "status-menu-reported".to_owned())
                .px_2()
                .pt_1()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(format!(
                    "Reported: {}",
                    own_presence::reporting_label(reporting)
                )))
        });
        let failure = self
            .own_presence()
            .and_then(|own| own.read(cx).failure().cloned())
            .map(|failure| {
                div()
                    .debug_selector(|| "status-menu-failure".to_owned())
                    .px_2()
                    .pt_1()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(SharedString::from(format!(
                        "Not reported ({}): {}",
                        own_presence::kind(failure.status),
                        failure.why
                    )))
            });
        let (header, remove) = self.account_menu_parts(palette, cx);
        let menu = div().debug_selector(|| "status-menu".to_owned()).absolute();
        let menu = match self.status_anchor {
            MenuAnchor::Rail => menu.left(px(RAIL_WIDTH + 4.0)).bottom(px(48.0)),
            MenuAnchor::TitleBar => menu.right(px(8.0)).top(px(TITLE_HEIGHT - 4.0)),
        };
        menu.w(px(260.0))
            .p_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .children(header)
            .children(entries)
            .children(reported)
            .children(failure)
            .children(remove)
            .into_any_element()
    }

    fn list_pane(&self) -> AnyElement {
        let pane = div().debug_selector(|| "list-pane".to_owned()).size_full();
        match self.list_place {
            ListPlace::Chats => pane.child(self.sidebar.clone()),
            ListPlace::Activity => pane.child(self.activity.0.clone()),
        }
        .into_any_element()
    }

    fn main_pane(&self, palette: Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let pane = div()
            .debug_selector(|| "conversation-pane".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background));
        match &self.main {
            Main::Chat(id) => self.chat_pane(id, pane, palette, window, cx),
            Main::Channel(id) => {
                let shown = self
                    .channel
                    .as_ref()
                    .filter(|(view, _)| view.read(cx).channel() == id.as_str());
                // Teams' header: the team's tile, the channel's name; a channel the list doesn't know yet says so plainly
                let (lead, title) = shown.filter(|(_, chosen)| !chosen.team.is_empty()).map_or_else(
                    || (None, SharedString::from("Channel")),
                    |(_, chosen)| (self.team_face(id, palette, cx), chosen.name.clone()),
                );
                let join = self.channel_join_button(id, cx);
                let translate = shown.map(|(view, _)| {
                    let model = view.read(cx).posts().read(cx).translation().clone();
                    crate::chat_translate::header_button(&model, window, cx)
                });
                let header = pane_header_led(lead, title, palette)
                    .child(div().flex_1())
                    .children(translate)
                    .children(join)
                    .debug_selector(|| "channel-header".to_owned());
                let body = div().flex_1().min_h_0().children(shown.map(|(view, _)| view.clone()));
                let body = crate::motion::rise(body, SharedString::from(format!("channel-in-{id}")), 10.0, cx);
                pane.child(header).child(body).into_any_element()
            }
            Main::NewChat => self.new_chat_pane(pane, palette, cx),
            Main::TeamsPage => match self.teams_page() {
                Some(shown) => pane.child(shown.clone()).into_any_element(),
                None => pane.child(no_chat(palette, cx)).into_any_element(),
            },
            Main::ManageTeam(_) => match self.manage_team() {
                Some(shown) => pane.child(shown.clone()).into_any_element(),
                None => pane.child(no_chat(palette, cx)).into_any_element(),
            },
            // the settings, the calendar, the search's results and "Join a team" are drawn across both panes (`body`), never
            // here
            Main::Nothing
            | Main::Settings
            | Main::Calendar
            | Main::Search
            | Main::JoinTeam
            | Main::CreateTeamMore
            | Main::Workflows => pane.child(no_chat(palette, cx)).into_any_element(),
        }
    }

    /// The conversation pane over chat `id`: its header (Teams' chat header: the chat's picture and name, a meeting's
    /// tabs after it, the other member's presence, then the translation, the call and the people on the right) and
    /// the chat, the people flyout over it while open.
    fn chat_pane(
        &self,
        id: &str,
        pane: gpui::Div,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = self.list.read(cx).row(id);
        let name = row.map_or_else(|| SharedString::from("Chat"), |row| row.name.clone());
        let face = row.map(|row| people::avatar(row.named.then_some(&row.name), row.picture.as_ref(), Size::Small));
        let chat = self.chat.clone().filter(|chat| chat.read(cx).conversation() == id);
        let presence = self.header_presence(cx).map(|availability| {
            presence::labelled(&availability, palette).debug_selector(|| "chat-header-presence".to_owned())
        });
        let call = self.call_header_button(cx);
        let people = self.people_header_button(id, cx);
        let flyout = self.people_layer(id, cx);
        // the chat's language: its messages read in it, and the account's sent in the chat's own
        let translate = chat.as_ref().map(|chat| {
            let model = chat.read(cx).translation().clone();
            crate::chat_translate::header_button(&model, window, cx)
        });
        let tabs = self.meeting_tab_bar(id, palette, cx);
        // Teams' chat header: the chat's picture and name (a meeting's tabs after it), the other member's
        // presence, the call on the right
        let header = self
            .partner_header(id, &name, palette, cx)
            .unwrap_or_else(|| pane_header(face, name, palette))
            .children(tabs)
            .children(presence)
            .child(div().flex_1())
            .children(translate)
            .children(call)
            .children(people)
            .debug_selector(|| "chat-header".to_owned());
        let body = self.meeting_tab_body().map_or_else(
            || div().flex_1().min_h_0().children(chat),
            |tab| div().flex_1().min_h_0().child(tab),
        );
        // another chat fades in from a little below
        let body = crate::motion::rise(body, SharedString::from(format!("chat-in-{id}")), 10.0, cx);
        pane.relative()
            .child(header)
            .child(body)
            .children(flyout)
            .into_any_element()
    }
}

impl Shell {
    /// The call bar and the panes on one sheet floating on the frame ([`crate::sheet`]), right of the rail and under
    /// the title bar.
    fn sheet(&self, palette: Palette, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let panes = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .child(self.call_bar.clone())
            .child(div().flex_1().min_h_0().child(self.body(palette, window, cx)));
        let holder = div().flex_1().h_full().min_w_0();
        crate::sheet::float(holder, panes, palette, window, (RAIL_WIDTH, TITLE_HEIGHT), false)
    }
}

/// A row of a popup menu, its test selector `id`: an icon or mark, its words, maybe a check.
pub(crate) fn menu_row(id: impl Into<SharedString>, palette: Palette) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let selector = id.to_string();
    div()
        .debug_selector(move || selector.clone())
        .id(id)
        .w_full()
        .h(px(32.0))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .cursor_pointer()
        .hover(move |row| row.bg(rgb(palette.hover)))
}

/// The conversation pane while no chat is open: what to do, and the key that finds a chat; its mark, lit in the brand
/// gradient, rises in.
fn no_chat(palette: Palette, cx: &App) -> gpui::Div {
    let mark = div()
        .size(px(80.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme::brand_gradient(palette, 135.0))
        .shadow(theme::glow(palette, palette.brand, 0.45, 32.0, 10.0))
        .text_color(rgb(palette.on_brand))
        .child(Glyph::MESSAGE.icon().size(px(34.0)));
    let words = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(palette.foreground))
                .child("No chat open"),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child("Choose one from the list, or press")
                .child(Kbd::new(gpui::Keystroke::parse("ctrl-k").unwrap_or_default()).outline())
                .child("to find one"),
        );
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .text_color(rgb(palette.muted_foreground))
        .child(crate::motion::rise(mark, "no-chat-mark", 14.0, cx))
        .child(crate::motion::rise(words, "no-chat-words", 6.0, cx))
}

/// A row of the channel header's Meet now menu.
fn meet_menu_row(what: &'static str, glyph: Glyph, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || format!("channel-meet-menu-{what}"))
            .flex()
            .items_center()
            .gap_2()
            .child(glyph.icon().size(px(14.0)))
            .child(label)
    })
}

/// A pane's header line.
pub(crate) fn pane_header(face: Option<Avatar>, title: impl Into<SharedString>, palette: Palette) -> gpui::Div {
    pane_header_led(face.map(IntoElement::into_any_element), title, palette)
}

/// [`pane_header`] led by any element in the face's place: a channel's team tile.
pub(crate) fn pane_header_led(lead: Option<AnyElement>, title: impl Into<SharedString>, palette: Palette) -> gpui::Div {
    pane_header_titled(lead, pane_title(title).into_any_element(), palette)
}

/// A pane header's title: semibold, on one line, ellipsized.
pub(crate) fn pane_title(title: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .child(title.into())
}

/// [`pane_header_led`] with its title drawn by the caller: a 1:1 chat's name, which opens the partner's card.
pub(crate) fn pane_header_titled(lead: Option<AnyElement>, title: AnyElement, palette: Palette) -> gpui::Div {
    div()
        .h(px(48.0))
        .flex_none()
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .bg(rgb(palette.surface))
        .border_b_1()
        .border_color(rgb(palette.border))
        .children(lead)
        .child(title)
}

impl Shell {
    fn call_keys(shell: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        shell
            .on_action(cx.listener(|this, _: &CallChat, _, cx| this.call_chat(cx)))
            .on_action(cx.listener(|this, _: &JoinCall, _, cx| this.join_call(cx)))
            .on_action(cx.listener(|this, _: &AnswerCall, _, cx| this.ask_call(Ask::Answer, cx)))
            .on_action(cx.listener(|this, _: &DeclineCall, _, cx| this.ask_call(Ask::Decline, cx)))
            .on_action(cx.listener(|this, _: &HangUpCall, _, cx| this.ask_call(Ask::HangUp, cx)))
            .on_action(cx.listener(|this, _: &ToggleMuteCall, _, cx| this.ask_call(Ask::ToggleMute, cx)))
            .on_action(cx.listener(|this, _: &ToggleCaptionsCall, _, cx| this.ask_call(Ask::ToggleCaptions, cx)))
            .on_action(cx.listener(|this, _: &ToggleCameraCall, _, cx| {
                this.ask_call(Ask::ToggleCamera, cx);
            }))
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.sync_activity_shown(window, cx);
        self.sync_new_message_row(cx);
        self.history.note(&self.main);
        let suggestions = self.search_suggestions(palette, window, cx);
        let overlay = |view: AnyView| {
            div()
                .absolute()
                .top(px(TITLE_HEIGHT + 8.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(view)
        };
        let switcher = self.switcher().map(|switcher| overlay(switcher.clone().into()));
        let status_menu = self.status_menu.then(|| self.status_menu_view(palette, cx));
        let channel_dialog = self.channel_dialog_view();
        let team_dialog = self.team_dialog_view();
        let toast = self.toast_view(palette);
        let more_apps = self.more_apps_view(palette, cx);
        let card = self.card_layer(window, cx);
        let first_run = self.settings.read(cx).awaiting_first_run().then(|| {
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0080))
                .child(self.first_run.clone())
        });
        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            // the window's use, for the account's own presence (its keystrokes: `keystrokes_are_use`)
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| this.used(cx)))
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| this.used(cx)))
            .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| this.used(cx)))
            .map(|shell| Self::shell_keys(shell, cx))
            // the call keys only in a build that shows calls: elsewhere they reach whatever else takes them
            .when(self.models.calls.read(cx).shows(), |shell| Self::call_keys(shell, cx))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::frame(palette))
            .child(self.title_bar(palette, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .child(self.rail_view(palette, cx))
                    .child(self.sheet(palette, window, cx)),
            )
            .children(suggestions)
            .children(switcher)
            .children(status_menu)
            .children(channel_dialog)
            .children(team_dialog)
            .children(more_apps)
            .children(card)
            .children(toast)
            .children(first_run)
    }
}

/// A rail item's count of what is new (the Activity's), at its icon's top right, Teams' red; its test selector `id`.
fn rail_count(id: &'static str, count: usize) -> gpui::Div {
    div()
        .debug_selector(move || id.to_owned())
        .absolute()
        .top(px(2.0))
        .left(px(RAIL_WIDTH / 2.0 + 2.0))
        .min_w(px(16.0))
        .h(px(16.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(rgb(theme::DANGER))
        .text_color(rgb(0x00ff_ffff))
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(SharedString::from(if count > 99 {
            "99+".to_owned()
        } else {
            count.to_string()
        }))
}

/// Another account's unread total on its avatar in the rail, its test selector `rail-account-<index>-unread`.
fn account_unread(index: usize, unread: u64) -> gpui::Div {
    div()
        .debug_selector(move || format!("rail-account-{index}-unread"))
        .absolute()
        .top(px(-4.0))
        .right(px(-6.0))
        .min_w(px(16.0))
        .h(px(16.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(rgb(theme::DANGER))
        .text_color(rgb(0x00ff_ffff))
        .text_size(px(10.0))
        .child(SharedString::from(if unread > 99 {
            "99+".to_owned()
        } else {
            unread.to_string()
        }))
}

/// What the rail shows of one account, read before it is drawn.
struct AccountFace {
    index: usize,
    key: String,
    name: SharedString,
    detail: SharedString,
    picture: Option<people::Picture>,
    reporting: Option<Reporting>,
    unread: u64,
    signed_out: bool,
    /// It is the account this shell is.
    shown: bool,
}

impl AccountFace {
    fn of(index: usize, tile: &AccountTile, shown: bool, cx: &App) -> Self {
        Self {
            index,
            key: tile.key.clone(),
            name: tile.name.clone(),
            detail: tile.detail.clone(),
            picture: tile.picture.clone(),
            reporting: tile.own_presence.as_ref().map(|own| own.read(cx).reporting(cx)),
            unread: tile.unread(cx),
            signed_out: tile.signed_out(cx),
            shown,
        }
    }
}

impl Shell {
    /// The panes; in place of them, once the account is signed out in an app of accounts, what to do (the rail stays,
    /// to switch to another account or remove this one).
    /// Under the call bar: the settings across both panes while Settings is chosen (as in Teams, their pages are
    /// their own list), else the list and the conversation panes (or the signed-out notice).
    fn body(&self, palette: Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.main == Main::Settings {
            return self.settings_view.clone().into_any_element();
        }
        // out of sight: the settings' video effects go, and a preview of the camera with them
        self.settings_view.update(cx, |view, _| view.put_away());
        if self.main == Main::Calendar {
            return self.calendar.clone().into_any_element();
        }
        if self.main == Main::Search {
            return global_search::results_page(&self.search_box, palette, cx);
        }
        if self.main == Main::JoinTeam
            && let Some(page) = self.join_team()
        {
            return page.clone().into_any_element();
        }
        if self.main == Main::CreateTeamMore
            && let Some(page) = self.create_team_more()
        {
            return page.clone().into_any_element();
        }
        if self.main == Main::Workflows
            && let Some(page) = &self.workflows.app
        {
            return page.clone().into_any_element();
        }
        if !self.list_shown {
            return self.panes_or_signed_out(self.main_pane(palette, window, cx), palette, cx);
        }
        let panes = h_resizable("panes")
            .with_state(&self.panes)
            .child(
                resizable_panel()
                    .size(px(LIST_WIDTH))
                    .size_range(px(LIST_MIN)..px(LIST_MAX))
                    .child(self.list_pane()),
            )
            .child(resizable_panel().child(self.main_pane(palette, window, cx)));
        self.panes_or_signed_out(panes, palette, cx)
    }

    fn panes_or_signed_out(&self, panes: impl IntoElement, palette: Palette, cx: &App) -> AnyElement {
        if !self.signed_out_shown(cx) {
            return div().size_full().child(panes).into_any_element();
        }
        let status = self.models.status.read(cx);
        div()
            .debug_selector(|| "account-signed-out".to_owned())
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(palette.muted_foreground))
            .child(status_text(status))
            .into_any_element()
    }
}

impl Shell {
    /// The search's suggestions under the title bar's box, lined up with it, while they are shown.
    fn search_suggestions(&self, palette: Palette, window: &Window, cx: &App) -> Option<AnyElement> {
        let suggestions = global_search::dropdown(&self.search_box, palette, window, cx)?;
        let left = self.title_left_width(cx) + 8.0;
        let width = f32::from(window.viewport_size().width) - left - RIGHT_WIDTH - 16.0;
        Some(
            div()
                .absolute()
                .top(px(TITLE_HEIGHT - 6.0))
                .left(px(left))
                .w(px(width.clamp(240.0, SEARCH_MAX)))
                .child(suggestions)
                .into_any_element(),
        )
    }
}

/// The dot the rail's status button shows for what is reported: a manual status's own, Available's green, grey while
/// nothing is reported, none (an empty ring) while it isn't known.
#[must_use]
pub fn shown_as(reporting: Reporting) -> Availability {
    match reporting {
        Reporting::Available | Reporting::Manual(ManualAvailability::Available) => Availability::Available,
        Reporting::Manual(ManualAvailability::Busy) => Availability::Busy,
        Reporting::Manual(ManualAvailability::DoNotDisturb) => Availability::DoNotDisturb,
        Reporting::Manual(ManualAvailability::Away) => Availability::Away,
        Reporting::Inactive | Reporting::Halted | Reporting::Quit => Availability::Offline,
        // a manual status (or reset) that failed: what the account shows isn't known
        Reporting::Connecting | Reporting::NotSet(_) => Availability::Unknown,
    }
}
