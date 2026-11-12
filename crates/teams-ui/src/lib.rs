//! `teams-ui`: the native Teams client's GPUI views and models, over `teams-core`'s engine.
//!
//! - [`bridge`]: the [`Bridge`](bridge::Bridge) owns the engine's [`EngineHandle`](teams_core::EngineHandle) (its
//!   events and its shutdown); one foreground task drains its events into the [`models`]; views share only its
//!   request side, behind the [`Core`](bridge::Core) trait, and ask for data with a query's
//!   [`Reply`](teams_core::Reply). Shutdown ends the drain and shuts the engine down off the foreground, joining its
//!   thread.
//! - [`models`]: GPUI entities the views observe: the account's [`Status`](models::Status) (the push channel, the
//!   engine, signed in or out) and what changed in the [`Chats`](models::Chats). Everything shown comes from the store,
//!   through a query; the models say when to ask again.
//! - [`shell`]: the window's three panes: the rail (Chat, Settings), the list pane, resizable, and the conversation
//!   pane; Ctrl+K opens the [`switcher`] (the chat switcher, fuzzy over the chats' names). The list pane is the
//!   [`sidebar`]: the chats and the teams with their channels, in sections of one list, each folded by its header.
//! - [`chat_list`]: the chats by last activity, from the store, kept up to date by the models' changes.
//! - [`chat_window`]: which of them the "Chats" section shows, and what See more adds.
//! - [`people`]: people as the views show them, a name and a picture from the engine (M2 Task 12); [`presence`] their
//!   presence, a dot in the chat list, the chat's header and the mention picker (M2 Task 17).
//! - [`chat_view`]: a chat's messages, the newest at the bottom, paged from the store (and the service's older
//!   history), grouped by sender, with day separators, the "New messages" divider, typing and the messages' actions;
//!   [`rich_text`] is our own renderer of a message's sanitized rich text (never an HTML or markdown view), its blocks
//!   of code drawn as Teams' code card by [`code`]; the
//!   account's writes show there as they go out ([`sending`]: each outbox row's state, "send again"); reactions are
//!   drawn from [`reactions`] (every reaction Teams offers, in Fluent Emoji's pictures) and picked in
//!   [`reaction_picker`]; a forwarded message is drawn as Teams draws one, its content in a card under where it came
//!   from ([`forwarded`]).
//! - [`compose`]: the compose box under a chat's messages: text with line breaks, links and mentions (by MRI, from the
//!   chat's members) into a message, Enter sends, replies and edits, typing; every write through the engine.
//! - [`global_search`]: the title bar's search (Ctrl+E, Ctrl+Shift+F; Ctrl+Alt+G to go right to a chat or channel):
//!   suggestions as the user types, from the lists at once and the service's search, and the results with Teams' tabs
//!   across the panes; a message found opens its chat at the message. [`search`] is its local part: the messages the
//!   store holds.
//! - [`teams_list`]: the sidebar's teams section, the account's teams and their channels (M2 Task 16); [`channel_view`] a
//!   channel as Teams shows it: its posts in a chat view of the channel ([`chat_view::Place`]), a post's thread beside
//!   them, both written to as a chat is.
//! - [`calendar_view`]: the rail's Calendar, Teams' calendar app: the month and the account's calendars in its own
//!   pane, the toolbar (Today, the arrows, the views: Day, Work week, Week, Month), the time grid or the month with the
//!   account's events from the engine's calendar read, and an event's card with Join for a meeting whose call runs;
//!   [`calendar`] its dates and layout.
//! - [`own_presence`]: the account's own presence as the app reports it (M2 Task 20): `teams-core`'s policy driven by
//!   the window's use, the push channel's registration, the settings' manual status (the shell's account menu) and idle
//!   time, held by the desktop for the app's life.
//! - [`settings`]: the settings and their file (`0600` in a `0700` directory; defaults for what can't be read);
//!   [`settings_view`] changes them.
//! - [`clock`]: every time a view shows, in Teams' words, on the 12-hour or 24-hour clock the settings' "Time format"
//!   gives (the system's locale by default), changed live.
//! - [`trace`]: the view trace, one JSON line per view change (ids, kinds and times, never text) on a descriptor the
//!   caller hands in, read by the live test.
//! - [`desktop`]: the app on the desktop: the main window (opened again from the tray or a notification), the
//!   [`notify`] notifications of messages that come (as the "notifications" setting says; a chat shown in a focused
//!   window never notifies; activating one opens its chat) and the [`tray`] item (the unread chats, open, quit;
//!   closing the window keeps the app in the tray; the launcher entry's count), both on the bus the app is given
//!   ([`bus`]) and nowhere else. Both carry the app's icon ([`app_icon`]: the tray item its pixels, with Teams' unread
//!   badge and its count).
//! - [`calls`] (M3 Task 17): the account's calls as the views show them, in a build that shows them (the lab app and the
//!   personal build), and the user's asks; [`call_view`] the incoming call's banner and the in-call strip of the
//!   main window, [`call_window`] the call's own window (as Teams opens one) showing [`call_stage`]'s stage and
//!   controls, [`effects`] the camera's background effects (blur, a picture) and their self-preview, [`call_alert`] the incoming call's ringtone ([`ringtone`], on the calls’ speaker) and its notification
//!   with Answer and Decline.
//! - [`account_prompt`]: the personal build's first sign-in screen, a window of its own asking for the work account
//!   before anything is signed in.
//! - [`view`]: the window's root view; [`window`] opens it inside the kit's root, which applies the theme to the
//!   window (its font size as the rem size, its font family and colours) and hosts the kit's dialogs and tooltips.
//! - [`theme`]: the UI kit's (`gpui-component`) theme in Teams-like colours and compact sizes, light and dark
//!   following the window's appearance.
//!
//! No view builds a request: the UI has no HTTP or socket client of its own, and GPUI's HTTP client stays the null
//! one (nothing here hands GPUI another), so an image URL loads nothing. Pictures reach a view only as bytes from
//! the engine ([`people`]).
//!
//! M2 Tasks 6–17 and 20 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

pub mod account_prompt;
pub mod accounts;
pub mod activity_view;
pub mod app_icon;
pub mod assets;
pub mod bridge;
pub mod bus;
pub mod calendar;
pub mod calendar_view;
pub mod call_alert;
mod call_lobby;
pub mod call_sounds;
pub mod call_stage;
pub mod call_transcript;
pub mod call_view;
pub mod call_window;
pub mod calls;
pub mod card_view;
pub mod channel_dialog;
pub mod channel_management;
pub mod channel_menu;
pub mod channel_view;
pub mod chat_list;
pub mod chat_people;
pub mod chat_translate;
pub mod chat_view;
pub mod chat_window;
pub mod clock;
pub mod code;
pub mod compose;
pub mod create_team_more;
pub mod desktop;
pub mod editor;
pub mod effects;
pub mod event_form;
pub mod files;
pub mod first_run;
pub mod forwarded;
pub mod global_search;
pub mod icons;
pub mod inline;
pub mod join_team;
pub mod keyboard;
pub mod link_preview;
pub mod local_ai;
pub mod manage_team;
pub mod meeting_recap;
pub mod message_correct;
pub mod models;
pub mod motion;
pub mod new_chat;
pub mod notify;
pub mod own_presence;
pub mod people;
pub mod picture_viewer;
pub mod prejoin;
pub mod presence;
pub mod presence_badge;
pub mod profile_card;
pub mod reaction_picker;
pub mod reactions;
pub mod recap;
pub mod recap_summary;
pub mod recap_view;
pub mod recording_download;
pub mod rich_text;
pub mod ringtone;
pub mod search;
pub mod selection;
pub mod sending;
pub mod settings;
pub mod settings_view;
pub mod shared_view;
pub mod sheet;
pub mod shell;
pub mod sidebar;
pub mod sounds;
pub mod switcher;
pub mod system_message;
pub mod team_app_dialogs;
pub mod team_dialogs;
pub mod team_menu;
pub mod teams_list;
pub mod teams_page;
pub mod theme;
pub mod trace;
pub mod translate;
pub mod tray;
pub mod update;
pub mod video;
pub mod video_player;
mod video_surface;
pub mod view;
pub mod window;
pub mod workflow_dialog;
pub mod workflow_run;
pub mod workflows_app;

pub use account_prompt::{AccountPrompt, AccountPromptEvent};
pub use accounts::{AccountParts, AccountTile, Accounts, AccountsEvent};
pub use assets::AppAssets;
pub use bridge::{Bridge, Core, EventSource, StoppedCore, Tap};
pub use call_alert::{CallAlert, CallAlertEvent};
pub use call_view::CallBar;
pub use calls::{ActiveCall, Ask, CallKind, Calls};
pub use channel_view::{ChannelView, ChannelViewEvent};
pub use chat_list::{ChatList, ChatListEvent, ChatRow};
pub use chat_view::{ChatView, ChatViewEvent, MessageAction};
pub use compose::{Compose, ComposeEvent, ComposeParts};
pub use desktop::{Desktop, DesktopEvent, DesktopParts};
pub use effects::{CameraPreview, EffectsPicker, PickerPlace};
pub use first_run::FirstRun;
pub use global_search::{GlobalSearch, GlobalSearchEvent};
pub use models::{Account, Chats, Engine, Feed, FeedEvent, Link, Models, Status};
pub use notify::{Notice, NoticeKind, NotificationServer, Notifier, NotifierEvent};
pub use own_presence::OwnPresence;
pub use people::Shown;
pub use presence::Presences;
pub use ringtone::{Ringer, Ringtone};
pub use search::{Hit, HitTarget};
pub use settings::{Copied, FirstRunState, LogLevel, Settings, SettingsFile, SettingsModel};
pub use shell::{Shell, ShellParts};
pub use switcher::Switcher;
pub use teams_list::{ChannelChosen, TeamsList, TeamsListEvent};
pub use trace::{TraceId, TraceLine, ViewTrace};
pub use tray::{Tray, TrayEvent, TrayIcon};
pub use video::{Tiles, VideoFeed};
pub use view::Root;

#[cfg(test)]
#[path = "../tests/support/fake_tool.rs"]
mod fake_tool;
