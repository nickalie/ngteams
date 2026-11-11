//! The chat view: one conversation's messages, the newest at the bottom.
//!
//! - **From the store only.** The view reads the store's newest page when it opens ([`Core::messages`], [`PAGE`]
//!   messages), and again, down to the oldest message it holds, each time the models say its chat changed (a
//!   delivery, a catch-up, an outbox row). A message's reactions come from the store too, read again only for a new
//!   version of it.
//! - **Older pages** load when fewer than a page of rows ([`PAGE`]) is above the view: the store's next page, and once
//!   the store has none, the service's next older page ([`Core::load_older`]) read into the store first. The view
//!   never holds more than the pages it was scrolled through. Nothing older from the service is the chat's beginning
//!   only when the store says its whole history is stored ([`Core::history_complete`]); a chat never synced is asked
//!   again once it changes, and after a few older loads that brought nothing the top offers to load again.
//! - **The list** is a bottom-aligned GPUI `list` that follows the tail while it is at the bottom, so a new message
//!   scrolls in; scrolled up, it stays where it is and a "Jump to newest" pill takes it back; a selection moved with
//!   the keyboard stops following. Rows are laid out only as they come into view. When the rows change, the list is
//!   told only which rows were added, removed or changed, and the message at the top of the view keeps its place:
//!   rows added above it (an older page) or below it never move it.
//! - **Rows**: a day separator before a new day; a "New messages" divider before the first message from someone else
//!   after the account's read horizon as it stood when the chat opened ([`Core::read_horizon`]), as the store counts
//!   unread; a sender's messages within 5 minutes of each other grouped under one name and time, someone else's with
//!   their avatar (M2 Task 12: their picture from the engine, else their initials). The name is their profile's
//!   ([`Core::person`]: Graph's, else the middle tier's profile lookup's), else the one their message carried, else
//!   their newest message's or one the binary knew, else their MRI; a name or a picture read later shows once the
//!   engine tells it (`Event::Profile`). Laid out as Teams does: someone else's avatar in a column of its own, level
//!   with the bubble's top, with their presence's badge when the presence model knows it (after every load the view
//!   asks the engine about the newest [`SENDERS_WATCHED`] senders it shows: [`crate::presence`]); the name and the
//!   time ([`clock::message_time`]) muted over the bubble; an "@" beside a bubble that mentions the account. A message shows its rich text through our own renderer ([`crate::rich_text`],
//!   parsed and laid out once per message id and version), "Edited" once edited, "This message was deleted" once
//!   deleted, its reactions as pills over the bubble's bottom edge with their counts (the account's own ringed) and
//!   "Add reaction". A forwarded message shows its content in Teams' forward card under the arrow, the original's
//!   sender (asked of the engine as any sender) and the original's time, a link to the original ([`crate::forwarded`]:
//!   in the app when the shell has its chat, told as [`ChatViewEvent::OpenOriginal`]). A system message
//!   ([`crate::system_message`]) is drawn with no author row, in Teams' words (its `events` module): a line after its
//!   icon (members added, joined, left or removed, the chat's name, a meeting chat's policy), a line after its icon and
//!   time (a call's or a meeting's start or end, a recording's state), a meeting chat's meeting end with its recap card
//!   ([`crate::meeting_recap`]: View recap, the transcript, the recording's thumbnail and expiry, Share), or a 1:1 or
//!   group chat's recording or transcript card with the link to Teams on the web; one Teams draws nothing for (a
//!   call's end with no length, the transcript's half of a recording, a meeting chat's transcript, a meeting's start
//!   followed at once by its end, a lone bot added) isn't shown. A message type or shape this build
//!   doesn't show is a placeholder that names no content (the thread's own centred, never named by its id);
//!   `Control/…` messages (typing) are signals, never shown.
//! - **Typing**: who is typing now ([`crate::Chats::typing`]), under the messages; it goes off with their end of
//!   typing, their message, or [`crate::models::TYPING_TIMEOUT`] after their last signal.
//! - **Actions** on a message, under the pointer or selected with the keyboard (Up and Down, once the list has the
//!   focus: Escape in the compose box with nothing to cancel there (no reply, edit or open picker) moves it there,
//!   whatever the box holds, and Escape in the list with nothing open or selected moves it back): reply, react and
//!   copy text, and for the account's own message edit (not a forward's) and delete. They are on Teams' hover bar over
//!   the bubble's top right ([`ChatView::hover_bar`]): the quick reactions (the account's most used,
//!   [`Core::frequent_reactions`], then Teams' own: [`quick_reactions`]), react, edit, reply, and "More options" (copy
//!   text, delete). Each is told as
//!   [`ChatViewEvent::Action`] and carried out (M2 Task 13): a reply and an edit go to the [`Compose`] box under the
//!   messages; react's key numbers the bar's quick reactions (keys 1–4) and "more reactions" (key 5, or react's
//!   button: [`ReactionPicker`], every reaction Teams offers, over the view), and a quick reaction or a reaction chip
//!   clicked sets or takes back the account's own (a chip of a key Teams doesn't offer only shows:
//!   [`reactions::Catalog::offers`]); react and delete select the message they act on, and moving the selection
//!   closes them; delete asks first (Enter or the button confirms, Escape cancels). Every write goes out through the
//!   engine ([`Core::write`]), never from the view.
//! - **The account's writes as they go out** ([`crate::sending`]): a message post shows at once as a pending message of
//!   its own (the engine tells its row as it is made, and the rows are built again then, before any store read),
//!   with its state, until its message is held or found in the store ([`Core::message`], by the id the row was sent
//!   with: a message older than the pages held); an edit, a delete or a reaction shows its state on the message it
//!   acts on, unless a newer write on the same thing makes it out of date ([`sending::superseded`]). "Send again" on
//!   a failed row or an earlier session's is the engine's explicit [`Core::resend`], after a warning when its message
//!   may have arrived, or when it changes a message (a later change may have gone out since).
//! - **Read receipts**: with the "send read receipts" setting on, while the chat is shown ([`ChatView::set_shown`]:
//!   the shell hides it behind the settings), the window is active and the list is at the newest message, the
//!   account's read horizon moves to the newest message from someone else that is after it, [`READ_DEBOUNCE`] after
//!   the view settles, one at a time.
//! - **The account's own notes** ([`NOTES`], "<name> (You)" under "Favorites"): read-only here. No conversation
//!   list brings them, so the view asks the engine to sync them from the service each time it opens
//!   ([`Core::sync_conversation`]); their messages offer no action and send no read receipt, and the compose box says
//!   they can't be written ([`compose::READ_ONLY_NOTES`]).
//! - **Opened at a message** ([`ChatView::reveal`], a search's hit, M2 Task 15): the view reads the store back to the
//!   message, scrolls to it and selects it; the list stops following the newest. A message older than the store holds
//!   (the service's search finds what was never synced) is read for through the service's older pages, up to
//!   [`MAX_SEEK`] of them. Until the message is shown no read
//!   receipt goes, and one already waiting is dropped.
//! - **A reply's quote** goes to the message it quotes on a click ([`ChatView::go_to_quoted`]), as Teams' does: read
//!   back to as a message opened at is (its time asked of the store, else the quote's), scrolled to the middle of the
//!   view and ringed in Teams' orange for a moment ([`FLASH`]), never selected; a deleted one's placeholder likewise;
//!   one found nowhere leaves the view where it is.
//!
//! - **A team's channel** ([`Place::Channel`], [`ChatView::in_place`]) is drawn as Teams' channel: its posts as a
//!   chat's messages, each post's replies summed up under it ([`Replies`]), its compose box "Message in <channel>"
//!   posting to the channel; a post's "Reply in thread" (on the hover bar, in place of a chat's
//!   quote) or its summary opens its thread beside it ([`ChatViewEvent::OpenThread`]), a view of its own
//!   ([`Place::Thread`]: the post and its replies, its box "Reply in thread" posting to the post's chain). Reactions,
//!   edits, deletes, copying and workflows work as in a chat; a channel sends no read receipt and no typing, and its
//!   box takes no file (a channel's files go to its team's site). What a channel's messages are besides a chat's
//!   (meeting cards, a channel meeting's call, the channel's own events) is [`channel`]'s.
//!
//! What the view shows is traced (`chat-view shown`: the conversation, its newest message ids and their kinds;
//! `chat-view revealed`: the message opened at; `quoted shown`: the message a quote went to; `typing shown`/`cleared`;
//! `outbox shown`: the rows shown and their states), never text or a name.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crate::editor::{Copy as ComposeCopy, Escape as ComposeEscape};
use chrono::{DateTime, Datelike as _, Local, NaiveDate, TimeZone, Utc};
use gpui::ClipboardItem;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Bounds, Context, ElementId, Entity, EventEmitter, FocusHandle,
    FollowMode, Global, InteractiveElement as _, IntoElement, KeyBinding, ListAlignment, ListOffset, ListState,
    MouseButton, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, actions, anchored, canvas, deferred, div, list, px, relative, rems, rgb,
};
use gpui_component::Selectable as _;
use gpui_component::button::{Button, ButtonRounded, ButtonVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::shimmer::ShimmerText;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable as _, Size};
use teams_core::{Action, Command, EngineError, NOTES};
use teams_proto::chat::{
    LinkPreview, MessageType, RichText, SharedFile, Unshown, parse_html, shared_files, unshown_parts, web_chat_link,
};
use teams_store::{Message, OutboxEntry, OutboxKind, OutboxState, PageCursor, Reaction, counts_unread};

use crate::bridge::Core;
use crate::card_view::{CardState, CardsElement, ShownCards};
use crate::chat_translate::{self, ChatTranslation};
use crate::clock::{self, HourCycle};
use crate::compose::{self, Compose, ComposeEvent, ComposeParts, Quoted};
use crate::files;
use crate::forwarded::{self, ForwardOrigin, Original};
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::link_preview;
use crate::meeting_recap::{self, RecapCard};
use crate::models::Models;
use crate::people::{Face, Shown};
use crate::picture_viewer::{self, OpenPicture, Viewed, Viewing};
use crate::profile_card;
use crate::reaction_picker::{ReactionPicker, ReactionPickerEvent};
use crate::reactions;
use crate::rich_text::{self, Mentions, Rendered};
use crate::selection::{self, MessageKey, Scope, TextSelection};
use crate::sending::{self, Again, Note, RowState};
use crate::settings::SettingsModel;
use crate::shell::ShellParts;
use crate::system_message::{self, EventIcon, EventLine, Naming, Shown as SystemShown};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::{presence, presence_badge};

pub mod channel;
mod events;
pub mod reactors;
mod saved;

use channel::{ChannelLooks, THREAD_EVENT};
pub use channel::{MeetingCard, Place, Replies};
use events::Expiry;
pub use events::UNVERIFIED;

actions!(
    teams_ui,
    [
        SelectPrevious,
        SelectNext,
        ReplyToSelected,
        ReactToSelected,
        EditSelected,
        DeleteSelected,
        ClearSelection,
        ConfirmDelete,
        PickFirst,
        PickSecond,
        PickThird,
        PickFourth,
        PickMore,
        CopySelected
    ]
);

/// The message list's key context: its keys apply only while the list itself has the focus (never while compose's
/// text area has it).
pub const CONTEXT: &str = "ChatMessages";
/// A reaction drawn in the hover bar's quick reactions, px.
const QUICK_EMOJI: f32 = 20.0;
/// A button of the hover bar, px: a quick reaction's or an action's square.
const BAR_BUTTON: f32 = 28.0;
/// The more-options menu's width while it lists workflows, as Teams' widens.
const WORKFLOW_MENU_WIDTH: f32 = 300.0;
/// The hover bar's padding and the gap between its buttons, px.
const BAR_GAP: f32 = 2.0;
/// The hover bar's more-options menu's width, px.
const MENU_WIDTH: f32 = 200.0;
/// A reaction drawn in its pill under a message, px.
const CHIP_EMOJI: f32 = 18.0;
/// A reaction's pill's height, px.
const PILL: f32 = 28.0;
/// How far the reactions reach up over their bubble's bottom edge, px.
const REACTIONS_OVERLAP: f32 = 6.0;
/// The avatar beside someone else's messages, px: Teams' 32.
const AVATAR: f32 = 32.0;

/// Where each sender's face was drawn, by message: kept in a test build only ([`ChatView::face_bounds`]).
#[cfg(feature = "test-support")]
type FacesPlaced = Rc<std::cell::RefCell<HashMap<String, Bounds<Pixels>>>>;
#[cfg(not(feature = "test-support"))]
type FacesPlaced = ();
/// Between the avatar's column and the bubble, px.
const AVATAR_GAP: f32 = 12.0;
/// A bubble's text inset, px: the reactions start there too.
const BUBBLE_INSET: f32 = 16.0;
/// The column right of someone else's bubble that holds the "mentions you" mark, px.
const MARK_COLUMN: f32 = 28.0;
/// A message's header's height, rem: the avatar beside it starts below it, level with the bubble.
const HEADER_REMS: f32 = 1.5;
/// Messages read from the store at a time; older ones load when fewer rows than this are above the view.
pub const PAGE: usize = 50;
/// A sender's messages within this long of the one before are grouped under one name and time, ms.
pub const GROUP_MS: i64 = 5 * 60 * 1_000;
/// How far beyond the view rows are laid out, px.
pub const OVERDRAW: f32 = 400.0;
/// The most message ids a trace line names (the newest).
pub const TRACED: usize = 50;
/// The most senders whose presence the view asks the engine about ([`ChatView::watch_senders`]): the mention picker's
/// [`PICKER_MAX`](compose::PICKER_MAX), whose share of the engine's watch it takes (both show the open chat's people),
/// so the chat list's own asks ([`LIST_WATCHED`](crate::chat_list::LIST_WATCHED)) still fit beside them.
pub const SENDERS_WATCHED: usize = compose::PICKER_MAX;
/// What a deleted message shows.
pub const DELETED: &str = "This message was deleted";
/// What a message of a type this build doesn't show shows.
pub const UNSUPPORTED: &str = "This message can't be shown here";
/// What a message shows under its text when it carries a part this build can't show (a file entry it can't read, a
/// card).
pub const UNSHOWN_CONTENT: &str = "This message has content that can't be shown here";
/// The button beside [`UNSHOWN_CONTENT`] and [`UNSUPPORTED`] in a chat: it opens the chat in the web client
/// (`teams_proto::chat::web_chat_link`). The channel view shows neither with it: no channel deep link was captured.
pub const OPEN_ON_THE_WEB: &str = "Open in Teams on the web";

/// A meeting chat's recap card's button: the meeting's Recap tab, as Teams' card opens it.
pub const VIEW_RECAP: &str = "View recap";
/// The divider at the read horizon.
pub const NEW_MESSAGES: &str = "New messages";
/// Older loads in a row that may bring nothing before rows near the top stop asking for more by themselves.
pub const MAX_FRUITLESS: u32 = 3;
/// How many older pages the view reads at most for a message it opens at that the store didn't hold
/// ([`ChatView::reveal`]).
pub const MAX_SEEK: u32 = 20;
/// How long the message a reply's quote was clicked to stays ringed, as Teams' 4 s animation (the ring fades out over
/// its first three quarters).
pub const FLASH: Duration = Duration::from_secs(4);
/// How many quick reactions the hover bar shows, as Teams' does; keys 1–4 pick them once react is chosen, and key 5
/// opens every other.
pub const QUICK_COUNT: usize = 4;
/// Teams' own quick reactions, in its order (the web client's hover bar offered the first four to an account that
/// had set none: M2 Task 10, `docs/protocol/chat.md`, "Reactions (chat-shapes)"): they fill the hover bar after the
/// account's most used ([`quick_reactions`]).
pub const DEFAULT_QUICK_REACTIONS: [&str; 6] = ["like", "heart", "laugh", "surprised", "sad", "angry"];
/// How long the view waits, once a read receipt is due, before it sends it (and looks again whether it still is).
pub const READ_DEBOUNCE: Duration = Duration::from_millis(800);
/// What the delete's confirmation asks.
pub const CONFIRM_DELETE: &str = "Delete this message?";
/// What "send again" warns of on a row whose message may have arrived.
pub const MAY_HAVE_ARRIVED: &str = "This message may have arrived already. Send it again anyway?";
/// What "send again" warns of on an edit, a delete or a reaction: sent again, it would undo a later change.
pub const MAY_BE_OUT_OF_DATE: &str =
    "A later change to this message may have gone out since. Send this one again anyway?";

/// Marks the view's key bindings as installed.
struct Bound;

impl Global for Bound {}

/// Binds the message list's keys, once per app.
pub fn init(cx: &mut App) {
    compose::init(cx);
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("r", ReplyToSelected, context),
        KeyBinding::new("a", ReactToSelected, context),
        KeyBinding::new("e", EditSelected, context),
        KeyBinding::new("delete", DeleteSelected, context),
        KeyBinding::new("escape", ClearSelection, context),
        KeyBinding::new("enter", ConfirmDelete, context),
        KeyBinding::new("1", PickFirst, context),
        KeyBinding::new("2", PickSecond, context),
        KeyBinding::new("3", PickThird, context),
        KeyBinding::new("4", PickFourth, context),
        KeyBinding::new("5", PickMore, context),
        KeyBinding::new("ctrl-c", CopySelected, context),
    ]);
}

/// What can be done with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageAction {
    Reply,
    /// A channel's post: opens its thread, where a reply goes ([`Place::Thread`]).
    ReplyInThread,
    React,
    /// The account's own message only.
    Edit,
    /// The account's own message only.
    Delete,
    /// Its text to the clipboard.
    Copy,
}

impl MessageAction {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Reply => "Reply",
            Self::ReplyInThread => "Reply in thread",
            Self::React => "React",
            Self::Edit => "Edit",
            Self::Delete => "Delete",
            Self::Copy => "Copy text",
        }
    }

    /// Its button's icon.
    #[must_use]
    pub fn glyph(self) -> Glyph {
        match self {
            // Teams' quote mark: a reply quotes the message
            Self::Reply => Glyph::QUOTE,
            Self::ReplyInThread => Glyph::THREAD,
            Self::React => Glyph::REACT,
            Self::Edit => Glyph::PENCIL,
            Self::Delete => Glyph::TRASH,
            Self::Copy => Glyph::COPY,
        }
    }

    /// The action its key asks for on the selected message, for its button's tooltip.
    fn key_action(self) -> &'static dyn gpui::Action {
        match self {
            Self::Reply | Self::ReplyInThread => &ReplyToSelected,
            Self::React => &ReactToSelected,
            Self::Edit => &EditSelected,
            Self::Delete => &DeleteSelected,
            Self::Copy => &CopySelected,
        }
    }

    /// The name of its button's test selector.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::ReplyInThread => "reply-in-thread",
            Self::React => "react",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Copy => "copy",
        }
    }
}

/// What a meeting chat's recap card asks the Recap tab to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecapOpen {
    /// The recap (View recap).
    Recap,
    /// Its transcript (the Transcript chip).
    Transcript,
    /// Its recording, playing (the thumbnail).
    Play,
}

/// What the chat view tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatViewEvent {
    /// An action was chosen on a message (M2 Task 13 carries it out).
    Action { action: MessageAction, message: String },
    /// A forwarded message's original time was clicked: the original is opened in the app when the account has its
    /// chat, else in Teams on the web.
    OpenOriginal(Original),
    /// A workflow was chosen in a message's "More options" (or "More actions"): it runs on that message
    /// ([`crate::workflow_run`]).
    RunWorkflow {
        flow: crate::workflow_dialog::FlowRef,
        message: crate::workflow_run::MessageRef,
    },
    /// "More actions" → "Create workflows" on a message: the Workflows dialog for it.
    CreateWorkflows(crate::workflow_run::MessageRef),
    /// A recap card's View recap, Transcript or thumbnail: the chat's Recap tab, at the card's recording (its sharing
    /// link) when the meeting was recorded, showing `open`. A call transcribed alone names its transcript's item's
    /// sharing link as `recording`, and `transcribed` is the part it stands for, shown when the recap's read lists none
    /// of its item.
    OpenRecap {
        recording: Option<String>,
        transcribed: Option<teams_proto::recap::RecapPart>,
        open: RecapOpen,
    },
    /// A call's end row's Transcript chip: the chat's call captions saved on this computer by that id
    /// ([`crate::call_transcript`]), shown in the chat's Recap tab.
    OpenSavedTranscript(i64),
    /// A channel post's "Reply in thread" or its replies' summary: its thread, opened beside the channel.
    OpenThread { post: String },
    /// A channel meeting card's Edit: the meeting's details, read from the team's calendar by its id (`ical_uid`),
    /// opened to change or cancel it (the shell's to open, as it knows the channel's team).
    EditMeeting { ical_uid: String },
}

/// How far the conversation's older history is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum History {
    /// More may be older than what the view holds.
    More,
    /// The service has nothing older: the view holds the chat's beginning.
    Complete,
    /// The service's older page couldn't be read.
    Failed,
    /// The conversation was never synced, so nothing older can be read yet: asked again once the chat changes.
    Unsynced,
    /// The last [`MAX_FRUITLESS`] older loads brought nothing: asked again only on demand ([`ChatView::load_older`]).
    Stalled,
}

impl History {
    /// The top row's text.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::More => "Loading older messages…",
            Self::Complete => "This is the beginning of the chat",
            Self::Failed => "Older messages couldn't be loaded",
            Self::Unsynced => "Older messages aren't synced yet",
            Self::Stalled => "No older messages came",
        }
    }

    /// The top row's button, which asks for older messages again: none while a load may run or at the beginning.
    #[must_use]
    pub fn again(self) -> Option<&'static str> {
        match self {
            Self::More | Self::Complete => None,
            Self::Failed => Some("Try again"),
            Self::Unsynced | Self::Stalled => Some("Load older"),
        }
    }
}

/// A message's rendered rich text, compared by identity: the cache hands out one per message id and version.
#[derive(Clone, Debug)]
pub struct RenderedText(pub Arc<Rendered>);

impl PartialEq for RenderedText {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What a message row shows as its body.
#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    Text(RenderedText),
    Deleted,
    /// A system message as a line of its own after its icon, with no author (members added, joined or left, the
    /// chat's name).
    Event(EventLine),
    /// A system message as a line after its icon and time (a call's or a meeting's start or end, a recording's state).
    TimedEvent(EventLine),
    /// A meeting chat's meeting end, or any other chat's recording saved: its line, and the recap card under it (the
    /// meeting's when it left a recording or a transcript, the recording's) ([`crate::meeting_recap`]).
    MeetingEnded(Box<MeetingEnd>),
    /// A 1:1 or group chat's transcript that no recording of its call holds: its recap card alone
    /// ([`crate::meeting_recap::call_cards`]).
    Card(Box<RecapCard>),
    /// A card a bot or a workflow posted, drawn as Teams draws it ([`crate::card_view`]).
    Cards(ShownCards),
    /// A meeting scheduled in a channel, drawn as Teams' meeting card ([`channel::meeting_card`]).
    Meeting(Box<MeetingCard>),
    /// A type this build doesn't show, by its name.
    Unsupported(String),
    /// A type this build doesn't show that the thread itself sent: its placeholder as a centred line, with no author
    /// (a thread is never shown as a sender).
    UnsupportedEvent(String),
}

impl Body {
    /// The trace's name for it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Deleted => "deleted",
            Self::Event(_) => "event",
            Self::TimedEvent(_) => "timed-event",
            Self::MeetingEnded(_) => "meeting-ended",
            Self::Card(_) => "transcript",
            Self::Cards(_) => "cards",
            Self::Meeting(_) => "meeting",
            Self::Unsupported(_) => "unsupported",
            Self::UnsupportedEvent(_) => "unsupported-event",
        }
    }

    /// A system message: drawn centred, with no author row, never grouped with a message beside it.
    #[must_use]
    pub const fn is_system(&self) -> bool {
        matches!(
            self,
            Self::Event(_) | Self::TimedEvent(_) | Self::MeetingEnded(_) | Self::Card(_) | Self::UnsupportedEvent(_)
        )
    }
}

/// A meeting chat's meeting end, or any other chat's recording saved, as shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingEnd {
    /// "Meeting ended: 1h 1m 37s", "Recording has been saved to …".
    pub line: EventLine,
    /// The meeting's recap card, when it left a recording or a transcript (or one is being saved); the recording's.
    pub recap: Option<RecapCard>,
}

/// A reaction key on a message: how many reacted with it, and whether the account did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionCount {
    pub key: String,
    pub users: Vec<String>,
    pub mine: bool,
}

impl ReactionCount {
    #[must_use]
    pub fn count(&self) -> usize {
        self.users.len()
    }
}

/// A message's row.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageItem {
    pub id: String,
    pub version: i64,
    pub sender: Option<String>,
    pub sender_name: SharedString,
    /// The sender's avatar beside `sender_name`: their picture from the engine, else the initials of their name.
    pub face: Face,
    pub own: bool,
    /// The first of its group: its name and time are shown.
    pub header: bool,
    pub arrival_time: i64,
    pub body: Body,
    /// Its `messagetype`: whether the store counts it unread ([`teams_store::counts_unread`]), whether or not this build
    /// shows it.
    pub message_type: String,
    pub edited: bool,
    pub reactions: Vec<ReactionCount>,
    /// The account's writes on it that aren't sent (an edit, a delete, a reaction) and that no newer write on the same
    /// thing made out of date, as they stand, the oldest first.
    pub outbox: Vec<Note>,
    /// The files it carries ([`files_of`]), each drawn as its card under its text (`crate::files`). Behind one
    /// pointer: a row stays small whether or not it has any.
    pub files: Arc<Vec<SharedFile>>,
    /// The previews of the links it carries (`crate::link_preview::previews_of`), each drawn as its card under its text.
    pub links: Arc<Vec<LinkPreview>>,
    /// What it carries beside its text that this build can't show ([`unshown_of`]): a file entry of a shape no message
    /// showed, a card. With any, it shows [`UNSHOWN_CONTENT`] and a way to open the chat in Teams on the web, never a
    /// guess at the part.
    pub unshown: UnshownParts,
    /// Whether its chat takes writes: not the account's own notes ([`NOTES`]), where it offers no action.
    pub writes: ChatWrites,
    /// Where it was forwarded from, when it is a forwarded message ([`crate::forwarded`]): its forward card's header.
    pub forwarded: Option<Box<ForwardOrigin>>,
    /// Where it stands in a channel's threads: none in a chat.
    pub thread: Threaded,
    /// A channel post's replies, summed up under it ([`Place::Channel`]); none without any.
    pub replies: Option<Box<Replies>>,
    /// How its translation is marked beside its time, while the chat is translated ([`crate::chat_translate`]).
    pub translation: Option<chat_translate::Mark>,
}

/// Where a message stands in a channel's threads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Threaded {
    /// A chat's message: it is replied to with a quote ([`MessageAction::Reply`]).
    #[default]
    No,
    /// A channel's post, in the channel's list: replied to in its thread ([`MessageAction::ReplyInThread`]).
    Post,
    /// A message in a post's thread (the post at its top, or a reply): what is written there is a reply already.
    InThread,
}

/// Whether a chat takes the account's writes from this app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatWrites {
    Taken,
    /// The account's own notes ([`NOTES`]): read-only here.
    ReadOnly,
    /// The app's read-only mode ([`crate::Core::read_only`]): no chat takes a write; a message's text can still be
    /// copied.
    AppReadOnly,
}

/// The parts of a stored message this build can't show, by kind ([`unshown_of`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnshownParts {
    /// `properties.files` entries this build can't read as a file (`teams_proto::chat::SharedFiles::unread`).
    pub files: bool,
    /// `properties.cards` other than an empty list.
    pub cards: bool,
    /// `properties` that aren't a JSON object, so nothing in them can be told apart.
    pub unreadable: bool,
}

impl UnshownParts {
    /// Whether there is any.
    #[must_use]
    pub const fn any(self) -> bool {
        self.files || self.cards || self.unreadable
    }
}

/// The parts of a stored message this build can't show (`teams_proto::chat::unshown_parts`: `properties.files`
/// entries it can't read as a file, `properties.cards` other than an empty list); `properties` that aren't a JSON
/// object are one too, never read as nothing. None for a message without `properties`. (The store keeps no `amsreferences`: an image shows from the
/// content's markup, as a placeholder.)
#[must_use]
pub fn unshown_of(message: &Message) -> UnshownParts {
    let Some(text) = message.properties.as_deref() else {
        return UnshownParts::default();
    };
    let Ok(properties) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text) else {
        return UnshownParts {
            unreadable: true,
            ..UnshownParts::default()
        };
    };
    unshown_parts(&properties)
        .iter()
        .fold(UnshownParts::default(), |parts, part| match part {
            Unshown::Files(_) => UnshownParts { files: true, ..parts },
            Unshown::Cards(_) => UnshownParts { cards: true, ..parts },
            Unshown::AmsReferences(_) => parts,
        })
}

/// The files a stored message carries (`teams_proto::chat::shared_files`, its `properties.files` entries read as
/// files), in their order; none for a message without `properties` or with `properties` that aren't a JSON object.
#[must_use]
pub fn files_of(message: &Message) -> Vec<SharedFile> {
    message
        .properties
        .as_deref()
        .and_then(|text| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text).ok())
        .map(|properties| shared_files(properties.get("files")).files)
        .unwrap_or_default()
}

/// A message post of the account's whose message isn't in the store yet: its row as it stands, and its text.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingItem {
    pub note: Note,
    pub created_at: i64,
    /// Its text as the row carries it; `None` when the row can't be read back.
    pub body: Option<RenderedText>,
    pub files: Vec<SharedFile>,
}

/// Whether a message of `message_type` is a text message the view lays out: `Text` or `RichText/Html`.
fn is_text_type(message_type: &str) -> bool {
    message_type == MessageType::RichTextHtml.as_str() || message_type == MessageType::Text.as_str()
}

/// The chat view's system messages: each as shown, per message id, with the version and the names' revision it was
/// worked out at (`None`: a type or shape this build doesn't show), and what they are worked out from beside the
/// message.
#[derive(Default)]
struct Systems {
    shown: HashMap<String, (i64, u64, Option<SystemShown>)>,
    /// Raised whenever a name the view knows changes: system messages name people.
    names_revision: u64,
    /// The chat's name, as the chat list has it: a meeting's recap card is titled by it.
    title: Option<SharedString>,
    /// A meeting chat's recap cards, by the call each is of, as its messages last said them.
    recaps: HashMap<Option<String>, RecapCard>,
    /// Any other chat's recap cards of its calls' recordings and transcripts, by the message each is drawn at.
    calls: HashMap<String, RecapCard>,
    /// When each recording a row names expires, as asked of its item.
    expiries: HashMap<teams_proto::chat::RecordingItem, Expiry>,
}

/// How a system message names people in this view: their name as the engine has it, else the one the event gives,
/// else a placeholder; never an MRI.
struct ViewNames<'v>(&'v ChatView);

impl Naming for ViewNames<'_> {
    fn is_me(&self, mri: &str) -> bool {
        mri == self.0.core.me()
    }

    fn name(&self, mri: &str, carried: Option<&str>) -> String {
        self.0
            .shown(mri)
            .and_then(|shown| shown.name.as_ref())
            .map(ToString::to_string)
            .or_else(|| carried.map(str::to_owned))
            .unwrap_or_else(|| crate::chat_list::unnamed(mri).to_owned())
    }
}

impl MessageItem {
    /// The actions it offers: none on an event, a deleted message, one this build doesn't show or one in a chat that
    /// takes no write; edit and delete only on the account's own.
    #[must_use]
    pub fn actions(&self) -> Vec<MessageAction> {
        if self.writes == ChatWrites::ReadOnly
            || !matches!(self.body, Body::Text(_) | Body::Cards(_) | Body::Meeting(_))
        {
            return Vec::new();
        }
        if self.writes == ChatWrites::AppReadOnly {
            return vec![MessageAction::Copy];
        }
        // a channel's post is replied to in its thread, any post Teams lets the account react to
        let reply = match self.thread {
            Threaded::No => None,
            Threaded::Post => Some(MessageAction::ReplyInThread),
            Threaded::InThread => return self.thread_actions(),
        };
        // a card (or a channel's meeting) is reacted to and copied; a reply's quote of one was never captured
        if matches!(self.body, Body::Cards(_) | Body::Meeting(_)) {
            return reply
                .into_iter()
                .chain([MessageAction::React, MessageAction::Copy])
                .collect();
        }
        let mut actions = vec![
            reply.unwrap_or(MessageAction::Reply),
            MessageAction::React,
            MessageAction::Copy,
        ];
        if self.own {
            // a forward is the original's content: Teams offers no edit of it, and a write of one was never captured
            if !self.is_forward() {
                actions.push(MessageAction::Edit);
            }
            actions.push(MessageAction::Delete);
        }
        actions
    }

    /// The actions of a message in a post's thread: no reply of its own (what is written there is one), else as a
    /// chat's.
    fn thread_actions(&self) -> Vec<MessageAction> {
        let mut actions = vec![MessageAction::React, MessageAction::Copy];
        if self.own && matches!(self.body, Body::Text(_)) {
            if !self.is_forward() {
                actions.push(MessageAction::Edit);
            }
            actions.push(MessageAction::Delete);
        }
        actions
    }

    /// Whether its "More options" lists the account's workflows that run from a message: a text of a conversation
    /// that takes the account's writes, as Teams' menu offers them.
    #[must_use]
    pub fn offers_workflows(&self) -> bool {
        self.writes == ChatWrites::Taken && matches!(self.body, Body::Text(_))
    }

    /// Whether its text holds a forwarded message's content (`rich_text::Block::Forward`).
    #[must_use]
    pub fn is_forward(&self) -> bool {
        matches!(&self.body, Body::Text(text)
            if text.0.blocks.iter().any(|block| matches!(block, rich_text::Block::Forward(_))))
    }
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// The top: how far the history is loaded.
    Top(History),
    Day(NaiveDate),
    NewMessages,
    Message(Box<MessageItem>),
    /// A message post on its way, after every message.
    Pending(PendingItem),
}

/// The day separator's text.
#[must_use]
pub fn day_label(day: NaiveDate, today: NaiveDate) -> String {
    match (today - day).num_days() {
        0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        _ if day.year() == today.year() => day.format("%A, %-d %B").to_string(),
        _ => day.format("%A, %-d %B %Y").to_string(),
    }
}

/// The local day of `ms` in `tz`.
#[must_use]
pub fn day_of<Tz: TimeZone>(ms: i64, tz: &Tz) -> Option<NaiveDate> {
    DateTime::from_timestamp_millis(ms).map(|at| at.with_timezone(tz).date_naive())
}

/// Whether `item` continues the group of `before`: the same sender, within [`GROUP_MS`], neither an event.
fn continues(before: &MessageItem, item: &MessageItem) -> bool {
    let events = before.body.is_system() || item.body.is_system();
    !events
        && before.sender.is_some()
        && before.sender == item.sender
        && (0..=GROUP_MS).contains(&(item.arrival_time - before.arrival_time))
}

/// The rows of `conversation`'s `messages` (oldest first): the top, day separators, the "New messages" divider before
/// the first message from someone else after `horizon` that the store counts unread, and each message grouped with the
/// one before when it continues it.
#[must_use]
pub fn arrange<Tz: TimeZone>(
    conversation: &str,
    messages: Vec<MessageItem>,
    history: History,
    horizon: Option<i64>,
    tz: &Tz,
) -> Vec<Item> {
    let mut items = vec![Item::Top(history)];
    let mut day = None;
    let mut placed = horizon.is_none();
    let mut before: Option<MessageItem> = None;
    for mut item in messages {
        let today = day_of(item.arrival_time, tz);
        let new_day = today.is_some() && today != day;
        if new_day {
            day = today;
            items.extend(today.map(Item::Day));
        }
        // what the store counts unread: someone else's message, not deleted, after the horizon; without a horizon the
        // store counts every one, but nothing marks where reading stopped: no divider
        let unread = !item.own
            && counts_unread(conversation, &item.message_type)
            && item.body != Body::Deleted
            && horizon.is_some_and(|at| item.arrival_time > at);
        let divider = !placed && unread;
        if divider {
            placed = true;
            items.push(Item::NewMessages);
        }
        item.header = new_day || divider || !before.as_ref().is_some_and(|before| continues(before, &item));
        before = Some(item.clone());
        items.push(Item::Message(Box::new(item)));
    }
    items
}

/// The reactions of a message by key, in the order they were first given; the account's own marked.
#[must_use]
pub fn count_reactions(reactions: &[Reaction], me: &str) -> Vec<ReactionCount> {
    let mut first: BTreeMap<&str, i64> = BTreeMap::new();
    for reaction in reactions {
        let at = first.entry(&reaction.key).or_insert(reaction.time);
        *at = (*at).min(reaction.time);
    }
    let mut keys: Vec<(&str, i64)> = first.into_iter().collect();
    keys.sort_by_key(|&(key, at)| (at, key));
    keys.into_iter()
        .map(|(key, _)| {
            let mut given: Vec<&Reaction> = reactions.iter().filter(|reaction| reaction.key == key).collect();
            given.sort_by_key(|reaction| (reaction.time, reaction.user.as_str()));
            ReactionCount {
                key: key.to_owned(),
                users: given.iter().map(|reaction| reaction.user.clone()).collect(),
                mine: given.iter().any(|reaction| reaction.user == me),
            }
        })
        .collect()
}

/// A reaction key as text: the emoji of a key Teams offers, else the key itself.
#[must_use]
pub fn reaction_label(key: &str) -> String {
    reactions::glyph(key)
}

/// A message's position in the store's order (arrival time, then id).
fn position(message: &Message) -> (i64, &str) {
    (message.arrival_time, message.id.as_str())
}

/// The store's messages of a conversation from the newest down to `oldest` (one page when `None`), the newest first,
/// and the cursor for the next older page.
async fn read_window(
    core: &dyn Core,
    conversation: &str,
    oldest: Option<(i64, String)>,
) -> Result<(Vec<Message>, Option<PageCursor>), EngineError> {
    let mut read = Vec::new();
    let mut before = None;
    loop {
        let page = core.messages(conversation.to_owned(), before, PAGE).await?;
        let reached = match (&oldest, page.messages.last()) {
            (Some((at, id)), Some(last)) => position(last) <= (*at, id.as_str()),
            _ => true,
        };
        read.extend(page.messages);
        before = page.next;
        if reached || before.is_none() {
            return Ok((read, before));
        }
    }
}

/// What a load read: the messages it brings (the newest first), the reactions of the versions not held yet, and
/// where the store's next older page starts.
struct Loaded {
    messages: Vec<Message>,
    reactions: HashMap<String, (i64, Vec<Reaction>)>,
    next: Option<PageCursor>,
}

/// The reactions of the messages whose version the view doesn't hold yet; a deleted message has none (the store drops
/// them with it), so none is read for it and any held before are replaced.
async fn new_reactions(
    core: &dyn Core,
    conversation: &str,
    messages: &[Message],
    held: &HashMap<String, i64>,
) -> HashMap<String, (i64, Vec<Reaction>)> {
    let mut read = HashMap::new();
    for message in messages {
        if held.get(&message.id) == Some(&message.version) {
            continue;
        }
        let reactions = if message.delete_time.is_some() {
            Vec::new()
        } else {
            core.reactions(conversation.to_owned(), message.id.clone())
                .await
                .unwrap_or_default()
        };
        read.insert(message.id.clone(), (message.version, reactions));
    }
    read
}

/// Which load runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    /// The newest messages down to the oldest held.
    Window,
    /// The next older page.
    Older,
}

/// The chat view (see the module docs).
pub struct ChatView {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    conversation: String,
    /// A chat, a team's channel or a post's thread in one ([`Place`]).
    place: Place,
    /// A channel's messages' posts, by message id, as the store had them at the last load (a message without one is a
    /// post of its own).
    roots: HashMap<String, String>,
    /// What a channel's messages say of each other, and where the channel is.
    looks: ChannelLooks,
    /// Where its compose box writes: a post's chain in its thread ([`Place::Thread`]), else the conversation. Its
    /// message posts' rows name it.
    writes_to: String,
    /// The messages held, the oldest first.
    messages: Vec<Message>,
    /// Per message id: the version its reactions were read at, and them.
    reactions: HashMap<String, (i64, Vec<Reaction>)>,
    /// Where the store's next older page starts; `None` once the store has no older message.
    next: Option<PageCursor>,
    history: History,
    /// The account's read horizon when the chat opened.
    horizon: Option<i64>,
    /// People as the engine has them, by MRI: the ask whose answer the entry takes, and that answer (`None`: asked,
    /// no answer yet or none).
    people: HashMap<String, (u64, Option<Shown>)>,
    /// The last ask's number: an answer to an earlier ask of someone asked again since is dropped.
    asks: u64,
    /// The models' revision the people were last asked at: someone renamed since is asked again.
    people_seen: u64,
    /// Rendered rich text per message id, with the version it was rendered from and the translation it shows (its
    /// text's address; `None`: the message's own).
    rendered: HashMap<String, (i64, Option<usize>, Arc<Rendered>)>,
    /// The chat's translation into the language picked for it ([`crate::chat_translate`]).
    translation: Entity<ChatTranslation>,
    /// What it showed at the last rebuild, and the observation of its changes.
    translated: (chat_translate::Shown, Subscription),
    /// The system messages as shown, and what they are worked out from.
    systems: Systems,
    /// The chat's calls' captions saved on this computer, and the call end rows linking them.
    saved: saved::Saved,
    /// How many times rich text was laid out.
    layouts: u64,
    /// How many times a message's content was parsed: a text message's HTML, or a system message's record (worked out
    /// again whenever a name the view knows changes).
    parses: u64,
    items: Vec<Item>,
    list: ListState,
    loading: Option<Task<()>>,
    window_due: bool,
    older_due: bool,
    /// Loads that ended, and of those the older pages.
    loads: u64,
    older_loads: u64,
    /// Older loads in a row that brought no message: at [`MAX_FRUITLESS`] the history is [`History::Stalled`].
    fruitless: u32,
    /// The models' revision of the chat the view last read at.
    seen: u64,
    /// The rows laid out in the last frame.
    drawn: Vec<usize>,
    hovered: Option<String>,
    /// The message whose hover bar is under the pointer: the bar floats over the message above its own, outside its
    /// row, so it keeps itself shown.
    bar_hovered: Option<String>,
    /// Where each sender's face was drawn in the last frame, by message, in a test build only: the UI tour opens a
    /// profile card beside one ([`ChatView::face_bounds`]). The card itself is placed by its anchor's own bounds.
    #[cfg_attr(not(feature = "test-support"), expect(dead_code, reason = "a test build's hook"))]
    faces_placed: FacesPlaced,
    selected: Option<String>,
    focus: FocusHandle,
    traced: Option<Vec<Traced>>,
    typing_traced: bool,
    settings: Entity<SettingsModel>,
    compose: Entity<Compose>,
    /// A supported operating-system file drag hovering this conversation.
    file_drop: FileDrop,
    /// The message the quick reactions are open on.
    reacting: Option<String>,
    /// "More reactions", open on the message the quick reactions are open on.
    more: Option<(Entity<ReactionPicker>, Subscription)>,
    /// Where the open quick reactions were last drawn, in the window: "more reactions" opens under them.
    quick_bar: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The hover bar's quick reactions: the account's most used, then Teams' own ([`quick_reactions`]).
    quick: Vec<String>,
    /// How many reactions of the account's the rows held when its most used were last asked for.
    own_reactions: Option<usize>,
    reactors: reactors::Reactors,
    /// The text selected with the mouse, in any of the messages ([`crate::selection`]).
    text_selection: TextSelection,
    /// The message whose more-options menu is open.
    options: Option<String>,
    /// Where the open more-options menu's button was last drawn, in the window: the menu opens under it.
    options_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The message whose more-options menu shows its "More actions" list.
    more_actions: Option<String>,
    /// Where that list was last drawn: a click in it is not a click outside the menu.
    more_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The account's workflows that run from a message, read each time a message's menu opens.
    workflows: Option<Entity<crate::workflow_run::menu::MessageWorkflows>>,
    /// The message whose delete waits for its confirmation.
    deleting: Option<String>,
    /// The row whose "send again" waits for its confirmation (its message may have arrived).
    resending: Option<i64>,
    /// The conversation's outbox rows that aren't sent, as the store had them at the last load.
    unsettled: Vec<OutboxEntry>,
    /// The rows earlier sessions left, by id.
    earlier: HashSet<i64>,
    /// This session's message posts told sent whose message was found in the store, by row id: never shown pending.
    stored_posts: HashSet<i64>,
    /// Whether the chat is shown (not hidden behind the settings): read receipts go only while it is.
    shown: Visibility,
    /// Rendered text of the pending message posts, per row id.
    pending_rendered: HashMap<i64, Arc<Rendered>>,
    /// What each message's cards show besides what they say: toggled elements, opened cards ([`CardState`]).
    card_states: HashMap<String, CardState>,
    /// The newest message the account has read up to (its arrival time): the store's horizon, or the last receipt.
    read_upto: Option<i64>,
    /// The read receipt's wait, while one is due.
    receipt: Option<Task<()>>,
    /// Read receipts sent.
    receipts: u64,
    /// Whether the window is active (the user's).
    active: Activation,
    outbox_traced: Option<Vec<(String, &'static str)>>,
    /// The message to open the view at (its arrival time and id), until a window load reads back to it.
    reveal: Option<(i64, String)>,
    /// The message the running window load reads back to, shown once it ends.
    revealing: Option<(i64, String)>,
    /// A message to open at that the store didn't hold (a search finds what was never synced), with the older pages
    /// read for it so far: shown once an older page brings it, given up after [`MAX_SEEK`] pages or at the beginning.
    seeking: Option<((i64, String), u32)>,
    /// Where a click on a reply's quote is going ([`ChatView::go_to_quoted`]).
    quote: QuoteJump,
    /// The models' changes, the compose box's events, the window's activation and the presence model's changes.
    _subscriptions: [Subscription; 4],
}

/// A click on a reply's quote on its way to the quoted message, and the message it went to.
#[derive(Default)]
struct QuoteJump {
    /// The quoted message it is going to, until it is shown or found nowhere, and the store's look for it while that
    /// runs.
    going_to: Option<(String, Option<Task<()>>)>,
    /// The message it went to, ringed for a moment ([`FLASH`]), and the wait that ends it.
    flashed: Option<(String, Task<()>)>,
    /// Quote clicks shown so far: each ring's animation is its own, so a second click on a quote plays it again.
    flashes: u64,
    /// The row it scrolled to the top, to be centred once the list has laid it out there, and whether a frame has laid
    /// it out yet ([`ChatView::centre`]).
    centring: Option<(usize, bool)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileDrop {
    Idle,
    Hovering,
}

impl EventEmitter<ChatViewEvent> for ChatView {}

impl ChatView {
    /// The view of `conversation` with its compose box, loading from the store at once.
    pub fn new(parts: ShellParts, conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_compose(parts, conversation, None, window, cx)
    }

    /// The view of `channel` (a team's channel) as `place` shows it: its posts ([`Place::Channel`]) or one post's
    /// thread ([`Place::Thread`]), with `compose` writing there: a [`Self::channel_box`], or one kept with its draft
    /// while another thread or channel was shown.
    pub fn in_place(
        parts: ShellParts,
        channel: String,
        place: Place,
        compose: Entity<Compose>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::build(parts, channel, place, compose, window, cx)
    }

    /// A new compose box for a view of `channel` in `place` ([`Self::in_place`]): the channel's posts' box, or a
    /// post's thread's posting to the post's chain ([`writes_to`]), saying `placeholder` while it is empty.
    pub fn channel_box<T: 'static>(
        parts: &ShellParts,
        channel: &str,
        place: &Place,
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Entity<Compose> {
        let writes = match place {
            Place::Thread { .. } => compose::Writes::Thread,
            Place::Channel | Place::Chat => compose::Writes::Channel,
        };
        let compose = Self::compose_box(parts, &writes_to(channel, place), window, cx);
        compose.update(cx, |compose, cx| compose.in_channel(writes, placeholder, cx));
        compose
    }

    pub(crate) fn with_compose(
        parts: ShellParts,
        conversation: String,
        compose: Option<Entity<Compose>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let compose = compose.unwrap_or_else(|| Self::compose_box(&parts, &conversation, window, cx));
        Self::build(parts, conversation, Place::Chat, compose, window, cx)
    }

    fn build(
        parts: ShellParts,
        conversation: String,
        place: Place,
        compose: Entity<Compose>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = Self::subscriptions(&parts.models, &compose, window, cx);
        let writes_to = writes_to(&conversation, &place);
        let (seen, people_seen) = Self::seen_at(&parts.models, [&conversation, &writes_to], cx);
        let (translation, observing_translation) = Self::translation_of(&parts, &conversation, &compose, cx);
        let mut view = Self {
            core: parts.core,
            models: parts.models,
            trace: parts.trace,
            conversation,
            place,
            roots: HashMap::new(),
            looks: ChannelLooks::default(),
            writes_to,
            messages: Vec::new(),
            reactions: HashMap::new(),
            next: None,
            history: History::More,
            horizon: None,
            people: HashMap::new(),
            asks: 0,
            people_seen,
            rendered: HashMap::new(),
            translation,
            translated: (chat_translate::Shown::default(), observing_translation),
            systems: Systems::default(),
            saved: saved::Saved::follow(cx),
            layouts: 0,
            parses: 0,
            items: Vec::new(),
            list: Self::following_list(cx),
            loading: None,
            window_due: true,
            older_due: false,
            loads: 0,
            older_loads: 0,
            fruitless: 0,
            seen,
            drawn: Vec::new(),
            hovered: None,
            bar_hovered: None,
            faces_placed: FacesPlaced::default(),
            selected: None,
            focus: cx.focus_handle(),
            traced: None,
            typing_traced: false,
            settings: parts.settings,
            compose,
            file_drop: FileDrop::Idle,
            reacting: None,
            more: None,
            quick_bar: Rc::default(),
            quick: quick_reactions(&[]),
            own_reactions: None,
            reactors: reactors::Reactors::default(),
            text_selection: TextSelection::default(),
            options: None,
            options_at: Rc::default(),
            more_actions: None,
            more_at: Rc::default(),
            workflows: None,
            deleting: None,
            resending: None,
            unsettled: Vec::new(),
            earlier: HashSet::new(),
            stored_posts: HashSet::new(),
            shown: Visibility::Shown,
            pending_rendered: HashMap::new(),
            card_states: HashMap::new(),
            read_upto: None,
            receipt: None,
            receipts: 0,
            active: Activation::of(window),
            outbox_traced: None,
            reveal: None,
            revealing: None,
            seeking: None,
            quote: QuoteJump::default(),
            _subscriptions: subscriptions,
        };
        view.start(cx);
        view
    }

    /// A new view's first reads: its messages, who types, its notes' outbox and its saved call transcripts.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.kick(cx);
        self.typing_changed(cx);
        self.sync_notes(cx);
        self.read_saved(cx);
    }

    /// The chat's translation, handed to its compose box too, and its observation: a change builds the rows again.
    fn translation_of(
        parts: &ShellParts,
        conversation: &str,
        compose: &Entity<Compose>,
        cx: &mut Context<Self>,
    ) -> (Entity<ChatTranslation>, Subscription) {
        let (core, settings) = (Arc::clone(&parts.core), parts.settings.clone());
        let translation = cx.new(|cx| ChatTranslation::new(core, settings, conversation, cx));
        compose.update(cx, |compose, _| compose.set_translation(translation.clone()));
        let observing = cx.observe(&translation, |this: &mut Self, _, cx| this.rebuild(cx));
        (translation, observing)
    }

    /// The models' revisions a new view starts at: its conversations' last change (the conversation's, and the chain a
    /// thread writes to), and the last change of any.
    fn seen_at(models: &Models, conversations: [&str; 2], cx: &App) -> (u64, u64) {
        let chats = models.chats.read(cx);
        let seen = conversations.map(|conversation| chats.changed_at(conversation));
        (seen[0].max(seen[1]), chats.revision())
    }

    fn file_drop_target(cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .debug_selector(|| "chat-view".to_owned())
            .id("chat-file-drop")
            .relative()
            .on_drag_move(
                cx.listener(|this, event: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    let hovered = event.bounds.contains(&event.event.position)
                        && this.compose.read(cx).accepts_files()
                        && compose::attachments::supported(event.drag(cx));
                    let state = if hovered { FileDrop::Hovering } else { FileDrop::Idle };
                    if this.file_drop != state {
                        this.file_drop = state;
                        cx.notify();
                    }
                }),
            )
            .on_file_drop_exit(cx.listener(|this, _, _, cx| {
                this.file_drop = FileDrop::Idle;
                cx.notify();
            }))
            .on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                this.file_drop = FileDrop::Idle;
                this.compose
                    .update(cx, |compose, cx| compose.attach_files(paths.paths(), window, cx));
                cx.notify();
            }))
    }

    /// The compose box of `conversation`, on the shell's parts.
    fn compose_box<T: 'static>(
        parts: &ShellParts,
        conversation: &str,
        window: &mut Window,
        cx: &mut Context<T>,
    ) -> Entity<Compose> {
        let compose_parts = ComposeParts {
            core: Arc::clone(&parts.core),
            settings: parts.settings.clone(),
            trace: Arc::clone(&parts.trace),
            presence: parts.models.presence.clone(),
        };
        let conversation = conversation.to_owned();
        cx.new(|cx| Compose::new(compose_parts, conversation, window, cx))
    }

    /// The account's own notes ([`NOTES`]), which no conversation list brings, synced from the service as the view
    /// opens: what it stores comes as the models' change, and a failed sync leaves what the store holds shown.
    fn sync_notes(&self, cx: &mut Context<Self>) {
        if self.conversation != NOTES {
            return;
        }
        let core = Arc::clone(&self.core);
        cx.spawn(async move |_: WeakEntity<Self>, _: &mut AsyncApp| {
            let _ = core.sync_conversation(NOTES.to_owned()).await;
        })
        .detach();
    }

    /// The message list: bottom-aligned, following the newest; its scroll moves the pill, and back at the newest a
    /// read receipt may be due. The handler runs while the list holds its own state, so what reads the list waits
    /// until the list let it go.
    fn following_list(cx: &mut Context<Self>) -> ListState {
        let list = ListState::new(0, ListAlignment::Bottom, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let view: WeakEntity<Self> = cx.entity().downgrade();
        list.set_scroll_handler(move |_, _, cx| {
            let view = view.clone();
            cx.defer(move |cx| {
                let _ = view.update(cx, |view, cx| {
                    view.want_receipt(cx);
                    cx.notify();
                });
            });
        });
        list
    }

    /// What the view follows: the models (its chat's changes, typing, names, presence), its compose box's events and
    /// the window's activation.
    fn subscriptions(
        models: &Models,
        compose: &Entity<Compose>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> [Subscription; 4] {
        let observing = cx.observe(&models.chats, |this: &mut Self, chats, cx| {
            let at = {
                let chats = chats.read(cx);
                // a thread's message posts are rows of the post's chain
                chats
                    .changed_at(&this.conversation)
                    .max(chats.changed_at(&this.writes_to))
            };
            if at > this.seen {
                this.seen = at;
                this.window_due = true;
                // a row just queued shows at once: the rows are built again from the models before the store is read
                // (the engine answers that read only once the send it is busy with is over)
                this.rebuild(cx);
                this.kick(cx);
            }
            this.typing_changed(cx);
            this.renamed(cx);
            cx.notify();
        });
        let compose_events = cx.subscribe_in(compose, window, |this: &mut Self, _, event, window, cx| match event {
            ComposeEvent::EditLast => this.edit_last(window, cx),
            ComposeEvent::Sent | ComposeEvent::Start(_) => {}
        });
        let activation = cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            this.active = Activation::of(window);
            this.want_receipt(cx);
        });
        // a presence the engine told: the senders' badges are drawn again from the model
        let presence = cx.observe(&models.presence, |_: &mut Self, _, cx| cx.notify());
        [observing, compose_events, activation, presence]
    }

    /// The compose box under the messages.
    #[must_use]
    pub fn compose(&self) -> &Entity<Compose> {
        &self.compose
    }

    /// The chat's translation: its header's language picker drives it.
    #[must_use]
    pub fn translation(&self) -> &Entity<ChatTranslation> {
        &self.translation
    }

    /// Whether the operating-system file drop target is currently highlighted.
    #[must_use]
    pub fn file_drop_active(&self) -> bool {
        self.file_drop == FileDrop::Hovering
    }

    /// The message the quick reactions are open on.
    #[must_use]
    pub fn reacting(&self) -> Option<&str> {
        self.reacting.as_deref()
    }

    /// The message whose delete waits for its confirmation.
    #[must_use]
    pub fn deleting(&self) -> Option<&str> {
        self.deleting.as_deref()
    }

    /// The row whose "send again" waits for its confirmation.
    #[must_use]
    pub fn resending(&self) -> Option<i64> {
        self.resending
    }

    /// The message posts on their way, the oldest first.
    pub fn pending_items(&self) -> impl Iterator<Item = &PendingItem> {
        self.items.iter().filter_map(|item| match item {
            Item::Pending(pending) => Some(pending),
            _ => None,
        })
    }

    /// Read receipts sent.
    #[must_use]
    pub fn receipts(&self) -> u64 {
        self.receipts
    }

    /// Whether the chat is shown.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.shown == Visibility::Shown
    }

    /// The chat is shown, or hidden (the shell shows the settings over it): hidden, no read receipt goes, and one
    /// waiting is dropped; shown again, one may be due.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if self.is_shown() == shown {
            return;
        }
        self.shown = if shown { Visibility::Shown } else { Visibility::Hidden };
        if shown {
            self.want_receipt(cx);
        } else {
            self.receipt = None;
        }
        cx.notify();
    }

    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// What the view shows: a chat, a team's channel or a post's thread in one.
    #[must_use]
    pub fn place(&self) -> &Place {
        &self.place
    }

    /// The team's and the channel's names, as a channel meeting's card and call show them; drawn again when they
    /// change.
    pub fn set_channel_place(&mut self, team: SharedString, channel: SharedString, cx: &mut Context<Self>) {
        let place = Some((team, channel)).filter(|(team, channel)| !team.is_empty() && !channel.is_empty());
        if self.looks.place != place {
            self.looks.place = place;
            self.rebuild(cx);
        }
    }

    /// The post a channel's message belongs to, as the last load read it: its own id for a post.
    fn root_of<'m>(&'m self, message: &'m str) -> &'m str {
        self.roots.get(message).map_or(message, String::as_str)
    }

    /// Whether the view shows `message`: a chat every one; a channel its posts; a thread its post and its replies.
    fn shows(&self, message: &Message) -> bool {
        match &self.place {
            Place::Chat => true,
            Place::Channel => self.root_of(&message.id) == message.id,
            Place::Thread { post } => self.root_of(&message.id) == post,
        }
    }

    /// The trace's name for the view.
    fn trace_view(&self) -> &'static str {
        match self.place {
            Place::Chat => "chat-view",
            Place::Channel => "channel-view",
            Place::Thread { .. } => "thread-view",
        }
    }

    /// Whether the chat shows no message at all once read (a conversation not started yet).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.loading.is_none() && self.loads > 0 && self.messages.is_empty() && self.unsettled.is_empty()
    }

    /// The rows, the top first.
    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// What message `id`'s cards are drawn with: their state, kept by the view, and their pictures' source.
    fn card_hook(&self, id: &str, cx: &Context<Self>) -> CardHook {
        let view = cx.entity().downgrade();
        let key = id.to_owned();
        CardHook {
            state: self.card_states.get(id).cloned().unwrap_or_default(),
            on_state: Rc::new(move |state, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.card_states.insert(key.clone(), state);
                    cx.notify();
                });
            }),
            source: rich_text::PictureSource::new(Arc::clone(&self.core)),
            cycle: clock::cycle(cx),
        }
    }

    /// What a click on a picture of message `id` calls: the viewer opened on it ([`Self::open_picture`]).
    fn picture_opener(id: &str, cx: &Context<Self>) -> OpenPicture {
        let view = cx.entity().downgrade();
        let id = id.to_owned();
        OpenPicture::new(move |src, _, cx| {
            let _ = view.update(cx, |this, cx| this.open_picture(&id, src, cx));
        })
    }

    /// What a click on a reply's quote calls: the view going to the quoted message ([`Self::go_to_quoted`]).
    fn quote_opener(cx: &Context<Self>) -> rich_text::OpenQuoted {
        let view = cx.entity().downgrade();
        rich_text::OpenQuoted::new(move |id, time, _, cx| {
            let _ = view.update(cx, |this, cx| this.go_to_quoted(id.to_owned(), time, cx));
        })
    }

    /// The pictures of the messages the view holds, in its order (`crate::picture_viewer`'s Previous and Next), and
    /// where they are asked for.
    fn pictures(&self) -> (Vec<Viewed>, Option<rich_text::PictureSource>) {
        let mut source = None;
        let mut pictures = Vec::new();
        for item in self.message_items() {
            let Body::Text(text) = &item.body else { continue };
            for picture in text.0.pictures() {
                source = source.or_else(|| text.0.source.clone());
                pictures.push(Viewed {
                    src: picture.src.clone(),
                    message: item.id.clone(),
                    sender: item.sender_name.clone(),
                    face: item.face.clone(),
                    time: item.arrival_time,
                });
            }
        }
        (pictures, source)
    }

    /// Opens the picture `src` of message `message` in the picture viewer, with the chat's other pictures before and
    /// after it.
    pub fn open_picture(&mut self, message: &str, src: &str, cx: &mut Context<Self>) {
        let (pictures, source) = self.pictures();
        let (Some(source), Some(at)) = (
            source,
            pictures
                .iter()
                .position(|picture| picture.message == message && picture.src == src),
        ) else {
            return;
        };
        self.trace.record(
            &TraceLine::new("chat-view", "picture opened")
                .conversation(&self.conversation)
                .messages([message]),
        );
        picture_viewer::open(Viewing { source, pictures, at }, cx);
    }

    /// Opens the chat's `n`th picture (the oldest first) in the picture viewer; whether it has one.
    pub fn open_nth_picture(&mut self, n: usize, cx: &mut Context<Self>) -> bool {
        let (pictures, _) = self.pictures();
        let Some(picture) = pictures.get(n) else { return false };
        let (message, src) = (picture.message.clone(), picture.src.clone());
        self.open_picture(&message, &src, cx);
        true
    }

    /// The message rows, the oldest first.
    pub fn message_items(&self) -> impl Iterator<Item = &MessageItem> {
        self.items.iter().filter_map(|item| match item {
            Item::Message(message) => Some(&**message),
            _ => None,
        })
    }

    /// How many messages the view holds.
    #[must_use]
    pub fn held(&self) -> usize {
        self.messages.len()
    }

    #[must_use]
    pub fn history(&self) -> History {
        self.history
    }

    /// Loads that ended, and of those the older pages.
    #[must_use]
    pub fn loads(&self) -> (u64, u64) {
        (self.loads, self.older_loads)
    }

    /// Whether a load is running.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// How many times rich text was laid out (once per message id and version).
    #[must_use]
    pub fn layouts(&self) -> u64 {
        self.layouts
    }

    /// How many times a message's content was parsed: a text message once per id and version, a system message once
    /// per id, version and names' revision (every change of a name it may show works it out again).
    #[must_use]
    pub fn parses(&self) -> u64 {
        self.parses
    }

    /// The rows laid out in the last frame.
    #[must_use]
    pub fn drawn(&self) -> &[usize] {
        &self.drawn
    }

    #[must_use]
    pub fn list_state(&self) -> &ListState {
        &self.list
    }

    /// Whether the "Jump to newest" pill shows: the list is scrolled up from the newest.
    #[must_use]
    pub fn scrolled_up(&self) -> bool {
        !self.list.is_following_tail()
    }

    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    #[must_use]
    pub fn hovered(&self) -> Option<&str> {
        self.bar_hovered.as_deref().or(self.hovered.as_deref())
    }

    /// The hover bar's quick reactions, in order.
    #[must_use]
    pub fn quick(&self) -> &[String] {
        &self.quick
    }

    /// The message whose more-options menu is open.
    #[must_use]
    pub fn options(&self) -> Option<&str> {
        self.options.as_deref()
    }

    /// Takes `message` as the one under the pointer, as its row's hover does (the UI tour's screenshots show a hover
    /// without moving the pointer).
    pub fn hover(&mut self, message: Option<String>, cx: &mut Context<Self>) {
        self.hovered = message;
        cx.notify();
    }

    /// The message list's focus.
    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Who is typing now (not the account), by name where the store has one.
    #[must_use]
    pub fn typing(&self, cx: &App) -> Vec<String> {
        let me = self.core.me();
        self.models
            .chats
            .read(cx)
            .typing(&self.conversation)
            .filter(|mri| *mri != me)
            .map(|mri| self.name_of(mri))
            .collect()
    }

    /// The typing line's text, if anyone is typing.
    #[must_use]
    pub fn typing_text(&self, cx: &App) -> Option<String> {
        let names = self.typing(cx);
        match names.as_slice() {
            [] => None,
            [one] => Some(format!("{one} is typing…")),
            [one, two] => Some(format!("{one} and {two} are typing…")),
            _ => Some("Several people are typing…".to_owned()),
        }
    }

    /// Opens the view at a message (a search's hit, by its arrival time and id): the next window load reads the store
    /// back to it (once the running load ends), then the list scrolls to it, stops following the newest, and the
    /// message is selected. A message the store no longer has (deleted from it since) leaves the view as it is.
    pub fn reveal(&mut self, arrival_time: i64, message: String, cx: &mut Context<Self>) {
        self.reveal = Some((arrival_time, message));
        self.window_due = true;
        self.kick(cx);
    }

    /// Shows the message a window load read back to ([`ChatView::reveal`]).
    fn revealed(&mut self, target: (i64, String), cx: &mut Context<Self>) {
        self.reveal_at(target, 0, cx);
    }

    /// Shows `target` when it is held; else, when it is older than what is held, reads the next older page for it
    /// (`pages` read so far, [`MAX_SEEK`] at most) and looks again once that page is in.
    fn reveal_at(&mut self, target: (i64, String), pages: u32, cx: &mut Context<Self>) {
        let message = target.1.clone();
        let Some(index) = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Message(item) if item.id == message))
        else {
            let older = self
                .messages
                .first()
                .is_none_or(|oldest| (oldest.arrival_time, &oldest.id) > (target.0, &target.1));
            let more = !matches!(self.history, History::Complete | History::Failed);
            if older && more && pages < MAX_SEEK {
                self.seeking = Some((target, pages + 1));
                self.load_older(cx);
            } else if self.going_to(&message) {
                self.quote_missing();
            }
            return;
        };
        if self.going_to(&message) {
            self.flash(index, message, cx);
            return;
        }
        self.select(Some(message.clone()), cx);
        self.list.pause_following_tail();
        self.list.scroll_to_reveal_item(index);
        self.trace.record(
            &TraceLine::new(self.trace_view(), "revealed")
                .conversation(&self.conversation)
                .messages([message.as_str()]),
        );
        cx.notify();
    }

    /// Goes to the message a reply quotes, as a click on its quote does in Teams: when the view holds it (a deleted
    /// one too, as its placeholder), it is scrolled to the middle of the view and ringed for a moment ([`FLASH`]); else
    /// its arrival time is asked of the store (else taken from the quote: a chat message's id is its arrival time) and
    /// the view reads back to it as it does for a message it opens at ([`ChatView::reveal`]: the store's pages, then
    /// the service's older ones, [`MAX_SEEK`] at most), then shows it so. One found nowhere (gone from the chat, or
    /// older than the pages read) leaves the view where it is, as Teams does.
    pub fn go_to_quoted(&mut self, id: String, time: Option<i64>, cx: &mut Context<Self>) {
        if let Some(index) = self.index_of(&id) {
            self.flash(index, id, cx);
            return;
        }
        let core = Arc::clone(&self.core);
        let conversation = self.conversation.clone();
        let wanted = id.clone();
        let look = cx.spawn(async move |this, cx: &mut AsyncApp| {
            let stored = core.message(conversation, wanted.clone()).await.ok().flatten();
            let _ = this.update(cx, |view, cx| {
                let Some((going, look)) = view.quote.going_to.as_mut() else {
                    return;
                };
                if *going != wanted {
                    return;
                }
                *look = None;
                let at = stored
                    .map(|message| message.arrival_time)
                    .or(time)
                    .or_else(|| wanted.parse().ok());
                match at {
                    Some(at) => view.reveal(at, wanted, cx),
                    None => view.quote_missing(),
                }
            });
        });
        self.quote.going_to = Some((id, Some(look)));
    }

    /// Whether a quote's click is going to `message`.
    fn going_to(&self, message: &str) -> bool {
        self.quote.going_to.as_ref().is_some_and(|(id, _)| id == message)
    }

    /// The row of message `id`, when the view holds it.
    fn index_of(&self, id: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, Item::Message(item) if item.id == id))
    }

    /// Shows the message at row `index` a quote's click went to: scrolled to the middle of the view as Teams scrolls it
    /// (put at the top first, then centred once laid out there: [`Self::centre`]; the list stops following the newest)
    /// and ringed for [`FLASH`].
    fn flash(&mut self, index: usize, id: String, cx: &mut Context<Self>) {
        self.quote.going_to = None;
        self.quote.flashes += 1;
        self.list.scroll_to(ListOffset {
            item_ix: index,
            offset_in_item: px(0.0),
        });
        self.quote.centring = Some((index, false));
        self.trace.record(
            &TraceLine::new(self.trace_view(), "quoted shown")
                .conversation(&self.conversation)
                .messages([id.as_str()]),
        );
        let wait = cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(FLASH).await;
            let _ = this.update(cx, |view, cx| {
                view.quote.flashed = None;
                cx.notify();
            });
        });
        self.quote.flashed = Some((id, wait));
        cx.notify();
    }

    /// A quote's click found its message nowhere: the view stays where it is, as Teams' does.
    fn quote_missing(&mut self) {
        if let Some((id, _)) = self.quote.going_to.take() {
            self.trace.record(
                &TraceLine::new(self.trace_view(), "quoted missing")
                    .conversation(&self.conversation)
                    .messages([id.as_str()]),
            );
        }
    }

    /// Once a frame has laid out the row a quote's click put at the top, scrolls it to the middle of the view (as far
    /// as the list goes: the newest stay at the bottom); until then asks for that frame.
    fn centre(&mut self, cx: &mut Context<Self>) {
        match self.quote.centring {
            Some((index, true)) => {
                self.quote.centring = None;
                let Some(row) = self.list.bounds_for_item(index) else {
                    return;
                };
                let view = self.list.viewport_bounds();
                let wanted = view.top() + ((view.size.height - row.size.height) / 2.0).max(px(0.0));
                self.list.scroll_by(row.top() - wanted);
            }
            Some((index, false)) => {
                self.quote.centring = Some((index, true));
                cx.notify();
            }
            None => {}
        }
    }

    /// The message ringed for a moment after a quote's click went to it.
    #[must_use]
    pub fn flashed(&self) -> Option<&str> {
        self.quote.flashed.as_ref().map(|(id, _)| id.as_str())
    }

    /// Scrolls to the newest message and follows the tail again.
    pub fn jump_to_newest(&mut self, cx: &mut Context<Self>) {
        self.list.scroll_to_end();
        self.list.set_follow_mode(FollowMode::Tail);
        self.want_receipt(cx);
        cx.notify();
    }

    /// Selects a message (the keyboard's actions apply to it).
    pub fn select(&mut self, message: Option<String>, cx: &mut Context<Self>) {
        if self.selected != message {
            // what waits on another message (its quick reactions, its delete's confirmation) closes: a key acts only on
            // the message selected
            if self.reacting.is_some() && self.reacting != message {
                self.reacting = None;
            }
            if self.deleting.is_some() && self.deleting != message {
                self.deleting = None;
            }
            if self.options.is_some() && self.options != message {
                self.options = None;
            }
            self.selected = message;
            cx.notify();
        }
    }

    /// Chooses `action` on `message`, if the message offers it: told as [`ChatViewEvent::Action`], then carried out (a
    /// reply or an edit in the compose box, the quick reactions opened, the delete's confirmation asked).
    pub fn act(&mut self, action: MessageAction, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let actions = self
            .message_items()
            .find(|item| item.id == message)
            .map(MessageItem::actions)
            .unwrap_or_default();
        // reply's key on a channel's post replies in its thread
        let action = match action {
            MessageAction::Reply if actions.contains(&MessageAction::ReplyInThread) => MessageAction::ReplyInThread,
            action => action,
        };
        let offered = actions.contains(&action);
        if !offered {
            return;
        }
        cx.emit(ChatViewEvent::Action {
            action,
            message: message.to_owned(),
        });
        // another action closes what waited for an answer
        self.reacting = None;
        self.deleting = None;
        self.options = None;
        match action {
            MessageAction::Reply => self.reply_to(message, window, cx),
            MessageAction::ReplyInThread => cx.emit(ChatViewEvent::OpenThread {
                post: message.to_owned(),
            }),
            MessageAction::Edit => self.edit(message, window, cx),
            MessageAction::React => {
                // the message the keys 1–4 act on is the one selected
                self.select(Some(message.to_owned()), cx);
                self.reacting = Some(message.to_owned());
                self.focus.focus(window, cx);
            }
            MessageAction::Delete => {
                // the message Enter deletes is the one selected
                self.select(Some(message.to_owned()), cx);
                self.deleting = Some(message.to_owned());
                self.focus.focus(window, cx);
            }
            MessageAction::Copy => self.copy_text(message, cx),
        }
        cx.notify();
    }

    /// A held message, by id.
    fn held_message(&self, id: &str) -> Option<&Message> {
        self.messages.iter().find(|message| message.id == id)
    }

    /// Starts a reply to `message` in the compose box.
    fn reply_to(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(held) = self.held_message(message) else { return };
        let (name, _) = self.sender_of(held);
        let Some(quoted) = Quoted::of(held, &name) else { return };
        // in a translated chat the box shows the quote as the chat shows the message; the quote sent is as written
        let quoted = match self.translated.0.text_of(held) {
            Some(text) => quoted.shown_as(&text),
            None => quoted,
        };
        self.compose
            .update(cx, |compose, cx| compose.reply_to(quoted, window, cx));
    }

    /// Starts editing the account's `message` in the compose box: its text loaded back, its mentions by MRI.
    fn edit(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(held) = self.held_message(message) else { return };
        let mentioned = rich_text::mentioned(held.properties.as_deref());
        // in a translated chat the edit starts from the text shown (what the account wrote), and goes out translated
        let text = self
            .translated
            .0
            .text_of(held)
            .map_or_else(|| compose::rich_text_of(held), |shown| shown.as_ref().clone());
        let loaded = compose::load(&text, &mentioned);
        let id = held.id.clone();
        self.compose.update(cx, |compose, cx| match loaded {
            Ok(loaded) => compose.edit(id, loaded, window, cx),
            Err(why) => compose.show_problem(format!("This message can't be edited here: {why}"), cx),
        });
    }

    /// Up in an empty compose box: the account's newest message that offers an edit.
    fn edit_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let last = self
            .message_items()
            .filter(|item| item.actions().contains(&MessageAction::Edit))
            .last()
            .map(|item| item.id.clone());
        if let Some(id) = last {
            self.act(MessageAction::Edit, &id, window, cx);
        }
    }

    /// Sets the account's reaction `key` on `message`, or takes it back when it is set: only a key Teams offers
    /// ([`reactions::Catalog::offers`]), sent in the shape the capture showed; any other is shown, never sent.
    pub fn toggle_reaction(&mut self, message: &str, key: &str, cx: &mut Context<Self>) {
        self.reacting = None;
        self.more = None;
        if !reactions::catalog().offers(key) {
            return;
        }
        let Some(item) = self.message_items().find(|item| item.id == message) else {
            return;
        };
        if !item.actions().contains(&MessageAction::React) {
            return;
        }
        let mine = item
            .reactions
            .iter()
            .any(|reaction| reaction.key == key && reaction.mine);
        let (conversation, message_id, key) = (self.conversation.clone(), message.to_owned(), key.to_owned());
        let action = if mine {
            Action::Unreact {
                conversation,
                message_id,
                key,
            }
        } else {
            Action::React {
                conversation,
                message_id,
                key,
            }
        };
        // the row comes back as the engine's events; a write it refuses has no row to show
        drop(self.core.write(action));
        cx.notify();
    }

    /// Picks the `index`th quick reaction on the message they are open on.
    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        if let (Some(message), Some(key)) = (self.reacting.clone(), self.quick.get(index).cloned()) {
            self.toggle_reaction(&message, &key, cx);
        }
    }

    /// Puts `message`'s text on the clipboard, as the reader sees it (a quote it carries left out).
    fn copy_text(&self, message: &str, cx: &mut Context<Self>) {
        let text = self
            .message_items()
            .find(|item| item.id == message)
            .and_then(|item| match &item.body {
                Body::Text(text) => Some(plain_text(&text.0)),
                Body::Cards(cards) => Some(
                    teams_proto::chat::card::plain_words(&cards.0)
                        .iter()
                        .map(|words| crate::card_view::expanded(words, clock::cycle(cx)))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Body::Meeting(card) => Some(card.title.to_string()),
                _ => None,
            });
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Puts the text selected with the mouse on the clipboard, as plain text; whether there was any.
    pub fn copy_selection(&self, cx: &mut App) -> bool {
        let messages = self.items.iter().filter_map(|item| match item {
            Item::Message(message) => match &message.body {
                Body::Text(text) => Some((message_key(message), &*text.0)),
                _ => None,
            },
            Item::Pending(pending) => pending.body.as_ref().map(|text| (pending_key(pending), &*text.0)),
            _ => None,
        });
        let Some(text) = self.text_selection.text(messages) else {
            return false;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// The text selected with the mouse.
    #[must_use]
    pub fn text_selection(&self) -> &TextSelection {
        &self.text_selection
    }

    /// Opens the more-options menu on `message`, or closes it when it is open there.
    pub fn toggle_options(&mut self, message: &str, cx: &mut Context<Self>) {
        self.options = if self.options.as_deref() == Some(message) {
            None
        } else {
            Some(message.to_owned())
        };
        self.more_actions = None;
        if self.options.is_some() && !self.core.read_only() {
            self.read_workflows(cx);
        }
        cx.notify();
    }

    /// Reads the account's workflows that run from a message again, as Teams does when a message's menu opens.
    fn read_workflows(&mut self, cx: &mut Context<Self>) {
        let workflows = if let Some(workflows) = &self.workflows {
            workflows.clone()
        } else {
            let core = Arc::clone(&self.core);
            let workflows = cx.new(|_| crate::workflow_run::menu::MessageWorkflows::new(core));
            cx.observe(&workflows, |_, _, cx| cx.notify()).detach();
            self.workflows = Some(workflows.clone());
            workflows
        };
        workflows.update(cx, crate::workflow_run::menu::MessageWorkflows::refresh);
    }

    /// The chat message `message` as a workflow runs on it.
    fn message_ref(&self, message: &str) -> crate::workflow_run::MessageRef {
        // a channel's message runs with its thread's post (its own id for a post), as Teams' channel menu runs it
        crate::workflow_run::MessageRef {
            conversation: self.conversation.clone(),
            id: message.to_owned(),
            root: self.place.in_channel().then(|| self.root_of(message).to_owned()),
            in_channel: self.place.in_channel(),
            team: None,
        }
    }

    /// Runs `flow` on `message`, as a workflow chosen in its menu.
    pub fn run_workflow(&mut self, flow: crate::workflow_dialog::FlowRef, message: &str, cx: &mut Context<Self>) {
        self.options = None;
        self.more_actions = None;
        let message = self.message_ref(message);
        cx.emit(ChatViewEvent::RunWorkflow { flow, message });
        cx.notify();
    }

    /// "Create workflows" on `message`.
    pub fn create_workflows(&mut self, message: &str, cx: &mut Context<Self>) {
        self.options = None;
        self.more_actions = None;
        let message = self.message_ref(message);
        cx.emit(ChatViewEvent::CreateWorkflows(message));
        cx.notify();
    }

    /// Shows the open menu's "More actions" list, as its row does.
    pub fn open_more_actions(&mut self, cx: &mut Context<Self>) {
        if self.options.is_some() {
            self.more_actions.clone_from(&self.options);
            cx.notify();
        }
    }

    /// The account's workflows that run from a message, as last read.
    #[must_use]
    pub fn message_workflows(&self, cx: &App) -> Vec<crate::workflow_dialog::FlowRef> {
        self.workflows
            .as_ref()
            .map_or_else(Vec::new, |workflows| workflows.read(cx).flows().to_vec())
    }

    /// Asks the engine for the account's most used reactions, for the hover bar's quick reactions.
    fn ask_frequent(&mut self, cx: &mut Context<Self>) {
        let reply = self.core.frequent_reactions(2 * QUICK_COUNT);
        cx.spawn(async move |this, cx| {
            let Ok(keys) = reply.await else { return };
            let _ = this.update(cx, |this, cx| {
                let quick = quick_reactions(&keys);
                if this.quick != quick {
                    this.quick = quick;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Opens "more reactions" on the message the quick reactions are open on, its search field focused; a pick sets
    /// or takes back the account's own as a quick reaction does, and Escape or a click outside closes it.
    pub fn open_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.reacting.clone() else { return };
        let mine: HashSet<String> = self
            .message_items()
            .find(|item| item.id == message)
            .map(|item| {
                item.reactions
                    .iter()
                    .filter(|reaction| reaction.mine)
                    .map(|reaction| reaction.key.clone())
                    .collect()
            })
            .unwrap_or_default();
        let core = Arc::clone(&self.core);
        let picker = cx.new(|cx| ReactionPicker::new(mine, core, window, cx));
        let events = cx.subscribe_in(&picker, window, move |this, picker, event, window, cx| {
            // a click outside that took the focus elsewhere (the compose box) keeps it there, and the quick reactions
            // close with the picker: the list's keys must not act on what is typed after it
            let held = picker.read(cx).holds_focus(window, cx);
            match event {
                ReactionPickerEvent::Picked(key) => this.toggle_reaction(&message, key, cx),
                ReactionPickerEvent::Dismissed => {
                    this.more = None;
                    if !held && !this.focus.contains_focused(window, cx) {
                        this.reacting = None;
                    }
                }
            }
            if held {
                this.focus_messages(window, cx);
            } else {
                cx.notify();
            }
        });
        self.more = Some((picker, events));
        cx.notify();
    }

    /// "More reactions", while open.
    #[must_use]
    pub fn more_reactions(&self) -> Option<&Entity<ReactionPicker>> {
        self.more.as_ref().map(|(picker, _)| picker)
    }

    /// Deletes the message whose delete waits for its confirmation.
    pub fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        let Some(message_id) = self.deleting.take() else { return };
        drop(self.core.write(Action::Delete {
            conversation: self.conversation.clone(),
            message_id,
        }));
        cx.notify();
    }

    /// "Send again" on row `row`: the engine's explicit resend, after a warning when its message may have arrived.
    pub fn send_again(&mut self, row: i64, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        let again = self.note_of_row(row).map(|note| note.again);
        match again {
            Some(Again::AfterWarning) if self.resending != Some(row) => self.resending = Some(row),
            Some(Again::AfterWarning | Again::Yes) => {
                self.resending = None;
                drop(self.core.resend(row));
            }
            Some(Again::No) | None => {}
        }
        cx.notify();
    }

    /// The note shown for row `row`.
    fn note_of_row(&self, row: i64) -> Option<&Note> {
        self.items.iter().find_map(|item| match item {
            Item::Pending(pending) if pending.note.row == row => Some(&pending.note),
            Item::Message(message) => message.outbox.iter().find(|note| note.row == row),
            _ => None,
        })
    }

    /// Escape in the list: what is open closes first (the quick reactions, a confirmation), then the selection, then
    /// the focus goes back to the compose box.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.more = None;
        let open = self.reacting.take().is_some()
            | self.deleting.take().is_some()
            | self.resending.take().is_some()
            | self.options.take().is_some()
            | self.text_selection.clear();
        if open {
            cx.notify();
        } else if self.selected.is_some() {
            self.select(None, cx);
        } else {
            let compose = self.compose.clone();
            compose.update(cx, |compose, cx| compose.focus(window, cx));
            cx.notify();
        }
    }

    /// Escape in the compose box with nothing to cancel there (no reply, edit or open picker; the box hands it on,
    /// whatever it holds): the message list takes the focus, so Up and Down select a message and its keys act on it.
    fn focus_messages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Asks for the next older page (once the running load ends), even after loads that brought nothing, one that
    /// failed, or one that found the chat never synced.
    pub fn load_older(&mut self, cx: &mut Context<Self>) {
        self.fruitless = 0;
        if self.history != History::Complete && self.history != History::More {
            self.history = History::More;
            self.rebuild(cx);
        }
        self.want_older(cx);
    }

    /// Asks for the next older page while there may be one.
    fn want_older(&mut self, cx: &mut Context<Self>) {
        if self.history == History::More && !self.older_due {
            self.older_due = true;
            self.kick(cx);
        }
    }

    /// Starts the load that is due, unless one runs: the window first, then an older page.
    fn kick(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            return;
        }
        let load = if self.window_due {
            self.window_due = false;
            Load::Window
        } else if std::mem::take(&mut self.older_due) && self.history == History::More {
            Load::Older
        } else {
            return;
        };
        let core = Arc::clone(&self.core);
        let conversation = self.conversation.clone();
        let held = self
            .messages
            .first()
            .map(|message| (message.arrival_time, message.id.clone()));
        let oldest = if load == Load::Window {
            // a message to open at, older than what is held, is read back to
            self.revealing = self.reveal.take();
            match (held, &self.revealing) {
                (Some(held), Some(target)) if held <= *target => Some(held),
                (_, Some(target)) => Some(target.clone()),
                (held, None) => held,
            }
        } else {
            held
        };
        let next = self.next.clone();
        let first = self.loads == 0;
        let find = if load == Load::Window {
            self.posts_to_find(cx)
        } else {
            Vec::new()
        };
        let held: HashMap<String, i64> = self
            .reactions
            .iter()
            .map(|(id, (version, _))| (id.clone(), *version))
            .collect();
        let (place, writes_to) = (self.place.clone(), self.writes_to.clone());
        self.loading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let read = match load {
                Load::Window => {
                    let oldest = thread_oldest(&*core, &conversation, &place, oldest).await;
                    window(&*core, &conversation, oldest, first, &held, &find).await
                }
                Load::Older => older(&*core, &conversation, oldest, next, &held).await,
            };
            let read = match read {
                Ok(read) if place.in_channel() => Ok(in_channel(&*core, &conversation, &writes_to, read).await),
                read => read,
            };
            let _ = this.update(cx, |view, cx| view.loaded(load, read, cx));
        }));
    }

    /// A load ended: what it read is held and shown.
    fn loaded(&mut self, load: Load, read: Result<Read, EngineError>, cx: &mut Context<Self>) {
        self.loading = None;
        self.loads += 1;
        match (load, read) {
            (_, Ok(read)) => self.take(load, read),
            // the engine stopped: what is shown stays
            (Load::Window, Err(_)) => {}
            (Load::Older, Err(_)) => self.history = History::Failed,
        }
        if load == Load::Older {
            self.older_loads += 1;
        }
        self.want_names(cx);
        self.rebuild(cx);
        self.follow_translation(cx);
        self.watch_senders();
        if load == Load::Window
            && let Some(target) = self.revealing.take()
        {
            self.revealed(target, cx);
        }
        if load == Load::Older
            && let Some((target, pages)) = self.seeking.take()
        {
            self.reveal_at(target, pages, cx);
        }
        self.kick(cx);
        self.want_receipt(cx);
    }

    /// Hands the chat's translation the text messages the view holds now (it translates those it has nothing for).
    fn follow_translation(&self, cx: &mut Context<Self>) {
        let held: Vec<&Message> = self.messages.iter().filter(|message| self.shows(message)).collect();
        let name_of = |message: &Message| self.sender_of(message).0;
        self.translation
            .update(cx, |translation, cx| translation.follow(&held, &name_of, cx));
    }

    fn take(&mut self, load: Load, read: Read) {
        if let Horizon::Read(horizon) = read.horizon {
            // a channel's post or a thread marks no "New messages": no read receipt moves its horizon here
            self.horizon = horizon.filter(|_| !self.place.in_channel());
        }
        if let Some(roots) = read.roots {
            self.roots = roots;
        }
        if let Some(outbox) = read.outbox {
            self.unsettled = outbox.unsettled;
            self.earlier = outbox.earlier;
            self.read_upto = self.read_upto.max(outbox.read_upto);
            self.stored_posts.extend(outbox.stored);
        }
        if let Some(history) = read.history {
            self.history = history;
        }
        if load == Load::Window && self.history == History::Unsynced {
            // the chat changed: a sync may have run since
            self.history = History::More;
        }
        let Some(loaded) = read.loaded else { return };
        self.reactions.extend(loaded.reactions);
        let mut messages = loaded.messages;
        messages.reverse();
        match load {
            Load::Window => {
                // the window read reaches down to the oldest held: anything older stays
                let oldest = messages
                    .first()
                    .map(|message| (message.arrival_time, message.id.clone()));
                let kept: Vec<Message> = match &oldest {
                    Some((at, id)) => self
                        .messages
                        .iter()
                        .filter(|message| position(message) < (*at, id.as_str()))
                        .cloned()
                        .collect(),
                    None => Vec::new(),
                };
                if kept.is_empty() {
                    self.next = loaded.next;
                    if self.next.is_some() && self.history == History::Complete {
                        // the store has an older page: a catch-up landed after an older load found nothing
                        self.history = History::More;
                    }
                }
                self.messages = kept.into_iter().chain(messages).collect();
            }
            Load::Older => {
                self.next = loaded.next;
                self.fruitless = if messages.is_empty() { self.fruitless + 1 } else { 0 };
                if self.fruitless >= MAX_FRUITLESS && self.history == History::More {
                    self.history = History::Stalled;
                }
                messages.extend(std::mem::take(&mut self.messages));
                self.messages = messages;
            }
        }
        // a thread holds its whole history once its post is held: nothing older is its
        if let Place::Thread { post } = &self.place
            && self.messages.iter().any(|message| &message.id == post)
        {
            self.history = History::Complete;
        }
        let held: HashSet<&str> = self.messages.iter().map(|message| message.id.as_str()).collect();
        self.reactions.retain(|id, _| held.contains(id.as_str()));
        self.rendered.retain(|id, _| held.contains(id.as_str()));
        self.systems.shown.retain(|id, _| held.contains(id.as_str()));
    }

    /// Asks the engine for the people the rows name: the senders (their profile's name and their picture), events'
    /// members (the account too in a meeting chat, whose events name it by its name, and as a recording's owner),
    /// forwarded messages' original senders (the account too: a forward names it, never "You") and who is typing.
    fn want_names(&mut self, cx: &mut Context<Self>) {
        let mut wanted: Vec<String> = Vec::new();
        let mut originals: Vec<String> = Vec::new();
        let named_by_name = system_message::is_meeting_chat(&self.conversation);
        let messages = std::mem::take(&mut self.messages);
        for message in &messages {
            if let Some(sender) = &message.sender {
                wanted.push(sender.clone());
            }
            if !is_text_type(&message.message_type) && message.delete_time.is_none() {
                let named = system_message::people(&message.message_type, &message.content);
                if named_by_name {
                    originals.extend(named);
                } else {
                    wanted.extend(named);
                    // a recording's owner is named by name, the account too
                    originals.extend(system_message::named_by_name(&message.message_type, &message.content));
                }
            }
            if is_text_type(&message.message_type)
                && let Some(sender) =
                    forwarded::forwarded_from(message.properties.as_deref()).and_then(|from| from.sender)
            {
                originals.push(sender);
            }
        }
        self.messages = messages;
        originals.extend(
            self.reactions
                .values()
                .flat_map(|(_, reactions)| reactions.iter().map(|reaction| reaction.user.clone())),
        );
        self.ask_people(originals, true, cx);
        let typing: Vec<String> = self
            .models
            .chats
            .read(cx)
            .typing(&self.conversation)
            .map(str::to_owned)
            .collect();
        wanted.extend(typing);
        self.ask_names(wanted, cx);
    }

    /// Asks the engine about the people whose avatars the rows show (see the module docs): the senders of someone
    /// else's messages, the newest [`SENDERS_WATCHED`] of them, the newest asked last so the engine's watch keeps them
    /// longest; never a bot or a service, which has no presence ([`teams_core::has_presence`]). Asked after every
    /// load, as the chat list asks after each of its own.
    fn watch_senders(&self) {
        let mut senders: Vec<&String> = Vec::new();
        let shown = self.items.iter().rev().filter_map(|item| match item {
            Item::Message(item) if !item.own && !item.body.is_system() => item.sender.as_ref(),
            _ => None,
        });
        for sender in shown {
            if senders.len() == SENDERS_WATCHED {
                break;
            }
            if teams_core::has_presence(sender) && !senders.contains(&sender) {
                senders.push(sender);
            }
        }
        presence::watch(self.core.as_ref(), senders.into_iter().rev());
    }

    /// Asks the engine about `wanted`, the account itself left out.
    fn ask_names(&mut self, wanted: Vec<String>, cx: &mut Context<Self>) {
        self.ask_people(wanted, false, cx);
    }

    /// Asks the engine about those of `wanted` the view hasn't asked about; the account itself only with `me_too` (a
    /// forward names the account by its name).
    fn ask_people(&mut self, wanted: Vec<String>, me_too: bool, cx: &mut Context<Self>) {
        let me = self.core.me().to_owned();
        let ask = self.asks + 1;
        let mut asked = Vec::new();
        for mri in wanted {
            if (me_too || mri != me) && !self.people.contains_key(&mri) {
                self.people.insert(mri.clone(), (ask, None));
                asked.push(mri);
            }
        }
        if asked.is_empty() {
            return;
        }
        self.asks = ask;
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut found = Vec::new();
            for mri in asked {
                if let Ok(person) = core.person(mri.clone()).await {
                    found.push((mri, Shown::of(person)));
                }
            }
            if found.is_empty() {
                return;
            }
            let _ = this.update(cx, |view, cx| view.answered(ask, found, cx));
        })
        .detach();
    }

    /// The engine answered ask `ask` for these people: the rows are built again when anyone changed.
    fn answered(&mut self, ask: u64, found: Vec<(String, Shown)>, cx: &mut Context<Self>) {
        let mut changed = false;
        for (mri, shown) in found {
            // someone the view dropped meanwhile isn't added back, and someone asked again since takes only the later
            // ask's answer, whichever comes first
            let Some((asked, entry)) = self.people.get_mut(&mri) else {
                continue;
            };
            if *asked == ask && entry.as_ref() != Some(&shown) {
                *entry = Some(shown);
                changed = true;
            }
        }
        if changed {
            self.systems.names_revision += 1;
            self.rebuild(cx);
        }
    }

    /// Someone the view asked about may have a new name or picture: they are asked again.
    fn renamed(&mut self, cx: &mut Context<Self>) {
        let chats = self.models.chats.read(cx);
        let since = self.people_seen;
        let renamed: Vec<String> = self
            .people
            .keys()
            .filter(|mri| chats.named_at(mri) > since)
            .cloned()
            .collect();
        self.people_seen = chats.revision();
        if !renamed.is_empty() {
            self.systems.names_revision += 1;
        }
        for mri in &renamed {
            self.people.remove(mri);
        }
        // only those asked about before: the account itself too when a forward named it
        self.ask_people(renamed, true, cx);
    }

    /// Someone started or stopped typing.
    fn typing_changed(&mut self, cx: &mut Context<Self>) {
        let typing: Vec<String> = self
            .models
            .chats
            .read(cx)
            .typing(&self.conversation)
            .map(str::to_owned)
            .collect();
        let me = self.core.me();
        let shown = typing.iter().any(|mri| mri != me);
        if shown != self.typing_traced {
            self.typing_traced = shown;
            let change = if shown { "shown" } else { "cleared" };
            self.trace
                .record(&TraceLine::new("typing", change).conversation(&self.conversation));
        }
        self.ask_names(typing, cx);
    }

    /// A person's name: "You" for the account, else the engine's, else their MRI.
    fn name_of(&self, mri: &str) -> String {
        if mri == self.core.me() {
            return "You".to_owned();
        }
        self.shown(mri)
            .and_then(|shown| shown.name.as_ref())
            .map_or_else(|| mri.to_owned(), ToString::to_string)
    }

    /// A person as the engine last answered, if it has.
    fn shown(&self, mri: &str) -> Option<&Shown> {
        self.people.get(mri).and_then(|(_, shown)| shown.as_ref())
    }

    /// A message's sender's name and whether it is a name (not an MRI): their profile's, else the message's own,
    /// else the engine's other names, else their MRI.
    fn sender_of(&self, message: &Message) -> (String, bool) {
        let Some(sender) = message.sender.as_deref() else {
            return ("Unknown".to_owned(), false);
        };
        let shown = self.shown(sender);
        let carried = message
            .sender_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let name = match (
            shown
                .filter(|shown| shown.profiled())
                .and_then(|shown| shown.name.as_ref()),
            carried,
        ) {
            (Some(profiled), _) => Some(profiled.to_string()),
            (None, Some(carried)) => Some(carried.to_owned()),
            (None, None) => shown.and_then(|shown| shown.name.as_ref()).map(ToString::to_string),
        };
        match name {
            Some(name) => (name, true),
            None => (sender.to_owned(), false),
        }
    }

    /// A text message's rich text, parsed and laid out once per id and version: the cache is asked before anything
    /// is parsed.
    fn rendered(&mut self, message: &Message) -> Arc<Rendered> {
        let translated = self.translated.0.text_of(message);
        let shows = translated.as_ref().map(|text| Arc::as_ptr(text) as usize);
        if let Some((version, shown, rendered)) = self.rendered.get(&message.id)
            && *version == message.version
            && *shown == shows
        {
            return Arc::clone(rendered);
        }
        let text = match translated {
            Some(text) => text.as_ref().clone(),
            None => self.parse(message),
        };
        self.layouts += 1;
        let mentions = Mentions::of_message(message, self.core.me());
        let web_link = rich_text::message_web_link(&message.conversation_id, &message.id);
        // its pictures and emoticons asked of this account's engine
        let rendered = Arc::new(
            rich_text::layout_with_web_link(&text, &mentions, web_link.as_deref())
                .with_source(rich_text::PictureSource::new(Arc::clone(&self.core))),
        );
        self.rendered
            .insert(message.id.clone(), (message.version, shows, Arc::clone(&rendered)));
        rendered
    }

    /// A text message's content as rich text (counted in [`Self::parses`]).
    fn parse(&mut self, message: &Message) -> RichText {
        self.parses += 1;
        if message.message_type == MessageType::RichTextHtml.as_str() {
            parse_html(&message.content)
        } else {
            RichText::plain(&message.content)
        }
    }

    /// A system message as shown, worked out once per id, version and names' revision (counted in
    /// [`Self::parses`]).
    fn system(&mut self, message: &Message) -> Option<SystemShown> {
        if let Some((version, revision, shown)) = self.systems.shown.get(&message.id)
            && *version == message.version
            && *revision == self.systems.names_revision
        {
            return shown.clone();
        }
        self.parses += 1;
        let shown = system_message::shown(
            &message.message_type,
            &message.content,
            &self.conversation,
            message.sender.as_deref(),
            &ViewNames(self),
        );
        let revision = self.systems.names_revision;
        self.systems
            .shown
            .insert(message.id.clone(), (message.version, revision, shown.clone()));
        shown
    }

    /// What a message shows; `None` for a system message Teams draws nothing for.
    fn body(&mut self, message: &Message) -> Option<Body> {
        if message.delete_time.is_some() {
            return Some(Body::Deleted);
        }
        if self.place.in_channel() {
            if let Some(card) = self.looks.meeting_card(message, self.core.me(), self.core.read_only()) {
                return Some(Body::Meeting(Box::new(card)));
            }
            if let Some(line) = self.looks.meeting_call_line(message) {
                return Some(Body::TimedEvent(EventLine::plain(EventIcon::Meeting, line)));
            }
            // the channel's own events: Teams' post list has them filtered out (its info pane tells them)
            if message.message_type.starts_with(THREAD_EVENT) {
                return None;
            }
        }
        if is_text_type(&message.message_type) {
            return Some(Body::Text(RenderedText(self.rendered(message))));
        }
        if message.message_type == teams_proto::chat::card::CARD_TYPE
            && let Some(cards) = ShownCards::of(&message.content)
        {
            return Some(Body::Cards(cards));
        }
        let kind = message.message_type.clone();
        Some(match self.system(message) {
            Some(SystemShown::Line(line)) => Body::Event(line),
            Some(SystemShown::TimedLine(line)) if self.is_meeting_end(message) => {
                Body::MeetingEnded(Box::new(MeetingEnd {
                    line,
                    recap: self.recap_of(message),
                }))
            }
            // any other chat's recording saved: its recap card under its line
            Some(SystemShown::TimedLine(line)) if self.systems.calls.contains_key(&message.id) => {
                Body::MeetingEnded(Box::new(MeetingEnd {
                    line,
                    recap: self.systems.calls.get(&message.id).cloned(),
                }))
            }
            Some(SystemShown::TimedLine(line)) => Body::TimedEvent(line),
            // a transcript a recording of its call holds is told by that recording's card
            Some(SystemShown::Transcript) => Body::Card(Box::new(self.systems.calls.get(&message.id).cloned()?)),
            Some(SystemShown::Nothing) => return None,
            // the thread's own: never shown as from a sender
            None if kind.starts_with("ThreadActivity/")
                || message.sender.as_deref().is_some_and(system_message::is_thread) =>
            {
                Body::UnsupportedEvent(kind)
            }
            None => Body::Unsupported(kind),
        })
    }

    /// Whether `message` is a meeting chat's meeting end (not a group chat's meeting's).
    fn is_meeting_end(&self, message: &Message) -> bool {
        system_message::is_meeting_chat(&self.conversation)
            && matches!(
                teams_proto::chat::chat_event(&message.message_type, &message.content),
                Some(teams_proto::chat::ChatEvent::CallEnded(end)) if !end.huddle
            )
    }

    /// The recap card of the call the meeting end `message` ends, when the meeting left something to show.
    fn recap_of(&self, message: &Message) -> Option<RecapCard> {
        let call = teams_proto::chat::meeting_times(&message.content).and_then(|times| times.call_id);
        self.systems.recaps.get(&call).filter(|card| card.shows()).cloned()
    }

    /// Whether the chat (not a meeting's) holds a recap card of a call's recording or transcript: its Recap tab has
    /// something of Teams' to show.
    #[must_use]
    pub fn has_call_recaps(&self) -> bool {
        !self.systems.calls.is_empty()
    }

    /// The chat's name, as the chat list has it: a meeting's recap card is titled by it.
    pub fn set_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>) {
        if self.systems.title != title {
            self.systems.title = title;
            self.rebuild(cx);
        }
    }

    /// A message's row, before it is arranged, with the note `notes` has for it; `None` for a control message (a
    /// signal, never shown) and a system message Teams draws nothing for.
    fn item(&mut self, message: &Message, notes: &HashMap<String, Vec<Note>>) -> Option<MessageItem> {
        if MessageType::from(message.message_type.clone()).is_control() {
            return None;
        }
        let me = self.core.me().to_owned();
        let body = self.body(message)?;
        let (files, links, unshown) = if matches!(body, Body::Text(_)) {
            (
                Arc::new(files_of(message)),
                Arc::new(link_preview::previews_of(message.properties.as_deref())),
                unshown_of(message),
            )
        } else {
            (Arc::default(), Arc::default(), UnshownParts::default())
        };
        let (sender_name, named) = self.sender_of(message);
        let picture = message
            .sender
            .as_deref()
            .and_then(|sender| self.shown(sender))
            .and_then(|shown| shown.picture.clone());
        let reactions = self
            .reactions
            .get(&message.id)
            .filter(|_| body != Body::Deleted)
            .map(|(_, reactions)| count_reactions(reactions, &me))
            .unwrap_or_default();
        let forwarded = matches!(body, Body::Text(_))
            .then(|| forwarded::forwarded_from(message.properties.as_deref()))
            .flatten()
            .map(|from| Box::new(self.forward_origin(&from)));
        let translation = matches!(body, Body::Text(_))
            .then(|| self.translated.0.mark_of(message))
            .flatten();
        Some(MessageItem {
            id: message.id.clone(),
            version: message.version,
            own: message.sender.as_deref() == Some(me.as_str()),
            sender: message.sender.clone(),
            face: Face { named, picture },
            sender_name: sender_name.into(),
            header: true,
            arrival_time: message.arrival_time,
            message_type: message.message_type.clone(),
            edited: message.edit_time.is_some() && !matches!(body, Body::Deleted),
            body,
            reactions,
            outbox: notes.get(&message.id).cloned().unwrap_or_default(),
            files,
            links,
            unshown,
            writes: if self.core.read_only() {
                ChatWrites::AppReadOnly
            } else if self.conversation == NOTES {
                ChatWrites::ReadOnly
            } else {
                ChatWrites::Taken
            },
            forwarded,
            thread: match self.place {
                Place::Chat => Threaded::No,
                Place::Channel => Threaded::Post,
                Place::Thread { .. } => Threaded::InThread,
            },
            replies: None,
            translation,
        })
    }

    /// Where a forwarded message came from, as its card's header shows it ([`ForwardOrigin::of`]).
    fn forward_origin(&self, from: &teams_proto::chat::Forwarded) -> ForwardOrigin {
        ForwardOrigin::of(from, from.sender.as_deref().and_then(|sender| self.shown(sender)))
    }

    /// The conversation's outbox rows the view knows, by id: this session's as last told and the store's that aren't
    /// sent (a row told sent stays sent).
    fn known_rows(&self, cx: &App) -> BTreeMap<i64, OutboxEntry> {
        let chats = self.models.chats.read(cx);
        // a thread's message posts are rows of its post's chain
        let chain = (self.writes_to != self.conversation)
            .then(|| chats.outbox_rows(&self.writes_to))
            .into_iter()
            .flatten();
        let mut rows: BTreeMap<i64, OutboxEntry> = chats
            .outbox_rows(&self.conversation)
            .chain(chain)
            .map(|row| (row.id, row.clone()))
            .collect();
        for row in &self.unsettled {
            let told_sent = rows.get(&row.id).is_some_and(|told| told.state == OutboxState::Sent);
            if !told_sent {
                rows.insert(row.id, row.clone());
            }
        }
        rows
    }

    /// The notes shown on messages: per message id, the account's edits, deletes and reactions on it that aren't sent
    /// and that no newer write on the same thing made out of date, the oldest first.
    fn message_notes(&self, rows: &BTreeMap<i64, OutboxEntry>, now_ms: i64) -> HashMap<String, Vec<Note>> {
        let superseded = sending::superseded(rows.values());
        let mut notes: HashMap<String, Vec<Note>> = HashMap::new();
        let shown = rows.values().filter(|row| {
            sending::shown_on_message(row.kind) && row.state != OutboxState::Sent && !superseded.contains(&row.id)
        });
        for row in shown {
            let target = sending::command_of(row).and_then(|command| command.message_id().map(str::to_owned));
            if let Some(target) = target {
                notes
                    .entry(target)
                    .or_default()
                    .push(Note::of(row, self.earlier.contains(&row.id), now_ms));
            }
        }
        notes
    }

    /// This session's message posts told sent whose message the view doesn't hold and that weren't found in the store
    /// yet: their row ids and the message ids they were sent as, for the window load to look up.
    fn posts_to_find(&self, cx: &App) -> Vec<(i64, String)> {
        let held: HashSet<&str> = self
            .messages
            .iter()
            .filter_map(|message| message.client_message_id.as_deref())
            .collect();
        self.models
            .chats
            .read(cx)
            .outbox_rows(&self.writes_to)
            .filter(|row| {
                row.kind == OutboxKind::Send
                    && row.state == OutboxState::Sent
                    && !self.stored_posts.contains(&row.id)
                    && !held.contains(row.client_message_id.as_str())
            })
            .filter_map(|row| Some((row.id, row.message_id.clone()?)))
            .collect()
    }

    /// The message posts on their way: the rows known ([`Self::known_rows`]), each until its message is held or was
    /// found in the store (a message older than the pages held).
    fn pending(&mut self, rows: &BTreeMap<i64, OutboxEntry>, now_ms: i64) -> Vec<PendingItem> {
        let held: HashSet<&str> = self
            .messages
            .iter()
            .filter_map(|message| message.client_message_id.as_deref())
            .collect();
        let shown: Vec<&OutboxEntry> = rows
            .values()
            .filter(|row| {
                row.kind == OutboxKind::Send
                    && row.conversation_id.as_deref() == Some(self.writes_to.as_str())
                    && !held.contains(row.client_message_id.as_str())
                    && !self.stored_posts.contains(&row.id)
            })
            .collect();
        self.pending_rendered
            .retain(|id, _| shown.iter().any(|row| row.id == *id));
        shown
            .into_iter()
            .map(|row| PendingItem {
                note: Note::of(row, self.earlier.contains(&row.id), now_ms),
                created_at: row.created_at,
                body: self.pending_body(row),
                files: match sending::command_of(row) {
                    Some(Command::Send { text, .. }) => {
                        shared_files(Some(&serde_json::json!(text.files_property()))).files
                    }
                    _ => Vec::new(),
                },
            })
            .collect()
    }

    /// A message post's text as its row carries it, laid out once per row.
    fn pending_body(&mut self, row: &OutboxEntry) -> Option<RenderedText> {
        if let Some(rendered) = self.pending_rendered.get(&row.id) {
            return Some(RenderedText(Arc::clone(rendered)));
        }
        let Some(Command::Send { text, .. }) = sending::command_of(row) else {
            return None;
        };
        // a post sent translated shows as the account wrote it
        let original = self.translated.0.original_of_sent(text.rich_text());
        let shown = original.as_deref().unwrap_or(text.rich_text());
        let rendered = Arc::new(rich_text::layout(shown, &Mentions::default()));
        self.pending_rendered.insert(row.id, Arc::clone(&rendered));
        Some(RenderedText(rendered))
    }

    /// A channel's posts' replies among `messages` (held, the oldest first), by post: how many (a reply Teams shows,
    /// not deleted), who replied, the latest first, and when the last came. None outside a channel's posts.
    fn replies_of(&self, messages: &[Message]) -> HashMap<String, Replies> {
        let mut replies: HashMap<String, Replies> = HashMap::new();
        if self.place != Place::Channel {
            return replies;
        }
        // who each post's summary shows already, by MRI
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        for message in messages.iter().rev() {
            let post = self.root_of(&message.id);
            let shown = post != message.id
                && message.delete_time.is_none()
                && !MessageType::from(message.message_type.clone()).is_control()
                && !message.message_type.starts_with(THREAD_EVENT);
            if !shown {
                continue;
            }
            let summary = replies.entry(post.to_owned()).or_insert_with(|| Replies {
                count: 0,
                people: Vec::new(),
                last: message.arrival_time,
            });
            summary.count += 1;
            let sender = message.sender.as_deref().unwrap_or_default();
            if summary.people.len() < channel::REPLIERS && seen.insert((post, sender)) {
                let (name, named) = self.sender_of(message);
                let name = SharedString::from(name);
                let picture = message
                    .sender
                    .as_deref()
                    .and_then(|sender| self.shown(sender))
                    .and_then(|shown| shown.picture.clone());
                summary.people.push((name, Face { named, picture }));
            }
        }
        replies
    }

    /// Builds the rows again and tells the list which changed.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.translated.0 = self.translation.read(cx).shown();
        let now_ms = Utc::now().timestamp_millis();
        let known = self.known_rows(cx);
        let notes = self.message_notes(&known, now_ms);
        let messages = std::mem::take(&mut self.messages);
        if self.place.in_channel() {
            self.looks.read(&messages);
        }
        let mut replies = self.replies_of(&messages);
        if system_message::is_meeting_chat(&self.conversation) {
            self.systems.recaps = meeting_recap::recap_cards(&messages);
        } else if self.place == Place::Chat {
            self.systems.calls = meeting_recap::call_cards(&messages);
        }
        let saved = self.saved_rows(&messages, cx);
        let shown = saved::with_saved(messages.iter().filter(|message| self.shows(message)).collect(), &saved);
        let mut rows: Vec<MessageItem> = shown
            .into_iter()
            .filter_map(|message| self.item(message, &notes))
            .map(|mut item| {
                item.replies = replies.remove(&item.id).map(Box::new);
                item
            })
            .collect();
        if system_message::is_meeting_chat(&self.conversation) {
            events::drop_bare_starts(&mut rows, &messages);
        }
        self.messages = messages;
        let mut items = arrange(&self.conversation, rows, self.history, self.horizon, &Local);
        items.extend(self.pending(&known, now_ms).into_iter().map(Item::Pending));
        let anchor = Anchor::of(&self.list, &self.items);
        splice(&self.list, &self.items, &items);
        if let Some(anchor) = anchor {
            anchor.restore(&self.list, &items);
        }
        self.items = items;
        if let Some(selected) = &self.selected
            && !self.message_items().any(|item| &item.id == selected)
        {
            self.selected = None;
        }
        self.close_gone();
        self.ask_expiries(cx);
        // the account's reactions changed: its most used may have too
        let own = self
            .message_items()
            .map(|item| item.reactions.iter().filter(|reaction| reaction.mine).count())
            .sum();
        if self.own_reactions != Some(own) {
            self.own_reactions = Some(own);
            self.ask_frequent(cx);
        }
        self.trace_items();
        self.trace_outbox();
        cx.notify();
    }

    /// Closes what waits on a message that no longer offers it (gone from the rows, or deleted meanwhile): its quick
    /// reactions, its delete's confirmation.
    fn close_gone(&mut self) {
        let offers = |view: &Self, id: &str, action: MessageAction| {
            view.message_items()
                .any(|item| item.id == id && item.actions().contains(&action))
        };
        if let Some(id) = self.reacting.clone()
            && !offers(self, &id, MessageAction::React)
        {
            self.reacting = None;
        }
        if let Some(id) = self.deleting.clone()
            && !offers(self, &id, MessageAction::Delete)
        {
            self.deleting = None;
        }
        if let Some(id) = self.options.clone()
            && !offers(self, &id, MessageAction::Copy)
        {
            self.options = None;
        }
    }

    /// Traces the outbox rows shown and their states, when they changed (`outbox shown`: the rows' ids as
    /// `messages`, their states as `kinds`).
    fn trace_outbox(&mut self) {
        let shown: Vec<(String, &'static str)> = self
            .items
            .iter()
            .flat_map(|item| match item {
                Item::Pending(pending) => std::slice::from_ref(&pending.note),
                Item::Message(message) => message.outbox.as_slice(),
                _ => &[],
            })
            .map(|note| (note.row.to_string(), note.state.name()))
            .collect();
        let unchanged = match &self.outbox_traced {
            Some(traced) => *traced == shown,
            None => shown.is_empty(),
        };
        if unchanged {
            return;
        }
        let line = TraceLine::new("outbox", "shown")
            .conversation(&self.conversation)
            .messages(shown.iter().map(|(id, _)| id.as_str()));
        let line = shown.iter().fold(line, |line, (_, kind)| line.kind(kind));
        self.trace.record(&line);
        self.outbox_traced = Some(shown);
    }

    /// The newest message a read receipt would move the account's horizon to now, with its arrival time, if one is
    /// due: the setting on, the chat shown, the window active, the list at the newest message and not on its way to a
    /// message it opens at ([`ChatView::reveal`]: until that load ends the list only seems to follow the newest), and
    /// someone else's message after the horizon that the store counts unread (and that carries the `clientmessageid` a
    /// receipt names).
    fn receipt_target(&self, cx: &App) -> Option<(String, i64)> {
        let wanted = self.shown == Visibility::Shown
            && self.active == Activation::Active
            && self.reveal.is_none()
            && self.revealing.is_none()
            && self.seeking.is_none()
            && self.quote.going_to.is_none()
            && self.list.is_following_tail()
            && self.conversation != NOTES
            // a channel's horizon isn't moved from here: no capture showed a channel's receipt
            && !self.place.in_channel()
            && !self.core.read_only()
            && self.settings.read(cx).sends_read_receipts();
        if !wanted {
            return None;
        }
        let me = self.core.me();
        let newest = self.messages.iter().rev().find(|message| {
            message.sender.as_deref().is_some_and(|sender| sender != me)
                && counts_unread(&self.conversation, &message.message_type)
                && message.delete_time.is_none()
                && message.client_message_id.is_some()
        })?;
        self.read_upto
            .is_none_or(|upto| newest.arrival_time > upto)
            .then(|| (newest.id.clone(), newest.arrival_time))
    }

    /// A read receipt goes out [`READ_DEBOUNCE`] from now when one is due, unless one waits already.
    fn want_receipt(&mut self, cx: &mut Context<Self>) {
        if self.receipt.is_some() || self.receipt_target(cx).is_none() {
            return;
        }
        self.receipt = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(READ_DEBOUNCE).await;
            let _ = this.update(cx, ChatView::send_receipt);
        }));
    }

    /// Sends the read receipt that is due now, if one still is.
    fn send_receipt(&mut self, cx: &mut Context<Self>) {
        self.receipt = None;
        let Some((message_id, arrival)) = self.receipt_target(cx) else {
            return;
        };
        self.read_upto = Some(arrival);
        self.receipts += 1;
        // the row comes back as the engine's events
        drop(self.core.write(Action::Read {
            conversation: self.conversation.clone(),
            message_id,
        }));
        self.want_receipt(cx);
    }

    /// Traces the rows shown, when they changed: the newest [`TRACED`] messages with their kinds, and which of them
    /// carry reactions and which quote another (a reply).
    fn trace_items(&mut self) {
        let shown: Vec<Traced> = self
            .message_items()
            .map(|item| Traced {
                id: item.id.clone(),
                kind: trace_kind(item),
                reacted: !item.reactions.is_empty(),
                reply: is_reply(item),
            })
            .collect();
        let shown: Vec<Traced> = shown[shown.len().saturating_sub(TRACED)..].to_vec();
        if self.traced.as_ref() == Some(&shown) {
            return;
        }
        let line = TraceLine::new(self.trace_view(), "shown")
            .conversation(&self.conversation)
            .messages(shown.iter().map(|traced| traced.id.as_str()))
            .reacted(
                shown
                    .iter()
                    .filter(|traced| traced.reacted)
                    .map(|traced| traced.id.as_str()),
            )
            .replies(
                shown
                    .iter()
                    .filter(|traced| traced.reply)
                    .map(|traced| traced.id.as_str()),
            );
        let line = shown.iter().fold(line, |line, traced| line.kind(traced.kind));
        self.trace.record(&line);
        self.traced = Some(shown);
    }

    /// Moves the selection by `step` messages (from the newest when none is selected).
    fn step_selection(&mut self, step: isize, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.message_items().map(|item| item.id.clone()).collect();
        if ids.is_empty() {
            return;
        }
        let last = ids.len() - 1;
        let at = match self
            .selected
            .as_ref()
            .and_then(|selected| ids.iter().position(|id| id == selected))
        {
            Some(at) => at.saturating_add_signed(step).min(last),
            None => last,
        };
        self.select(Some(ids[at].clone()), cx);
        if let Some(index) = self
            .items
            .iter()
            .position(|item| matches!(item, Item::Message(message) if message.id == ids[at]))
        {
            // a selection moved off the newest stops following the tail, or the next frame would scroll it away
            self.list.pause_following_tail();
            self.list.scroll_to_reveal_item(index);
        }
    }

    fn act_on_selected(&mut self, action: MessageAction, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected.clone() {
            self.act(action, &selected, window, cx);
        }
    }

    /// The row at `index`; a row near the top asks for the next older page.
    fn render_item(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        self.drawn.push(index);
        if index < PAGE {
            self.want_older(cx);
        }
        let palette = theme::current(cx);
        match self.items.get(index) {
            Some(Item::Top(history)) => top_row(*history, &self.place, palette, cx),
            Some(Item::Day(day)) => day_row(*day, palette),
            Some(Item::NewMessages) => new_messages_row(palette),
            Some(Item::Message(item)) => {
                let item = item.clone();
                let row = self.message_row(&item, palette, cx);
                // someone else's newest message rises in (the account's own rose as it was sent, below)
                if index + 1 == self.items.len() && !item.own {
                    let id = SharedString::from(format!("message-in-{}", item.id));
                    crate::motion::rise(div().w_full().child(row), id, 12.0, cx)
                } else {
                    row
                }
            }
            Some(Item::Pending(pending)) => {
                let row = pending_row(pending, &self.text_selection, palette, cx);
                let id = SharedString::from(format!("pending-in-{}", pending.created_at));
                crate::motion::rise(div().w_full().child(row), id, 12.0, cx)
            }
            None => div().into_any_element(),
        }
    }

    fn message_row(&self, item: &MessageItem, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id.clone();
        let hovered_id = id.clone();
        let highlighted = self.selected.as_deref() == Some(id.as_str());
        let row = div()
            .id(ElementId::Name(format!("message-{id}").into()))
            .debug_selector(move || format!("message-{id}"))
            .relative()
            .w_full()
            .px_4()
            // a system line sits close to the rows beside it, as Teams lists them
            .pt(px(if item.header && !item.body.is_system() {
                12.0
            } else {
                2.0
            }))
            .flex();
        // a click on a message selects nothing, as in Teams: its actions show under the pointer, and the keyboard
        // selects (nor does it move the focus: the list's own mouse down keeps it where it was)
        let row = row.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            let now = hovered.then(|| hovered_id.clone());
            if *hovered || this.hovered.as_deref() == Some(hovered_id.as_str()) {
                this.hovered = now;
                cx.notify();
            }
        }));
        let row = if item.body.is_system() {
            row.flex_col().child(self.system_row(item, palette, cx))
        } else {
            // someone else's message beside their avatar's column, Teams' way; the account's own to the right
            let avatar = (!item.own).then(|| self.avatar_column(item, palette, cx));
            row.children(avatar)
                .child(self.message_column(item, highlighted, palette, cx))
        };
        row.into_any_element()
    }

    /// The ring Teams draws inside the bubble of the message a reply's quote was clicked to: a hairline in its orange
    /// (the theme's mention colour) that fades out over the first three quarters of [`FLASH`].
    fn flash_ring(&self, item: &MessageItem, palette: Palette, cx: &App) -> Option<AnyElement> {
        if self.flashed() != Some(item.id.as_str()) {
            return None;
        }
        let id = item.id.clone();
        let ring = div()
            .debug_selector(move || format!("quoted-flash-{id}"))
            .absolute()
            .inset_0()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.mention));
        if !crate::motion::on(cx) {
            return Some(ring.into_any_element());
        }
        Some(
            gpui::AnimationExt::with_animation(
                ring,
                SharedString::from(format!("quoted-flash-{}-{}", item.id, self.quote.flashes)),
                gpui::Animation::new(FLASH),
                |ring, t| ring.opacity((1.0 - t * 4.0 / 3.0).max(0.0)),
            )
            .into_any_element(),
        )
    }

    /// The column left of someone else's messages: their avatar beside the first of a group, level with its bubble's
    /// top, with their presence's badge when the models know it (the view asks the engine about its senders after
    /// each load, [`Self::watch_senders`], and draws what the model holds).
    fn avatar_column(&self, item: &MessageItem, palette: Palette, cx: &App) -> gpui::Div {
        let column = div().w(px(AVATAR)).flex_none().mr(px(AVATAR_GAP));
        if !item.header {
            return column;
        }
        let badge = item
            .sender
            .as_deref()
            .and_then(|sender| {
                presence_badge::badge(
                    &self.models.presence.read(cx).of(sender),
                    presence_badge::MESSAGE,
                    palette,
                )
            })
            .map(|badge| badge.absolute().right(px(-2.0)).bottom(px(-2.0)));
        // an app's avatar is its icon on a tile, as Teams draws the Workflows bot's
        let face = if item
            .sender
            .as_deref()
            .is_some_and(crate::workflows_app::is_workflows_bot)
        {
            crate::workflows_app::bot_avatar(AVATAR, palette).into_any_element()
        } else {
            item.face
                .avatar(&item.sender_name, Size::Medium)
                .size(px(AVATAR))
                .into_any_element()
        };
        let id = item.id.clone();
        let avatar = div()
            .debug_selector(move || format!("avatar-{id}"))
            .relative()
            .size(px(AVATAR))
            // the kit's own sizes: 48 px with small initials, drawn at Teams' 32 (an exact `Size::Size` gives
            // the initials half its width, too narrow for two)
            .child(face)
            .children(badge);
        // a click on the face opens their full card, a rest on it the compact one, as Teams' does
        let avatar = match card_of(item) {
            Some(of) => profile_card::anchor(
                format!("profile-card-from-avatar-{}", item.id),
                of,
                profile_card::Click::Full,
                &self.models.cards,
                avatar,
            ),
            None => avatar.into_any_element(),
        };
        #[cfg(feature = "test-support")]
        let avatar = self.face_placed(item, avatar);
        column.pt(rems(HEADER_REMS)).child(avatar)
    }

    /// `avatar`, its bounds recorded for [`Self::face_bounds`].
    #[cfg(feature = "test-support")]
    fn face_placed(&self, item: &MessageItem, avatar: AnyElement) -> AnyElement {
        let (placed, at) = (Rc::clone(&self.faces_placed), item.id.clone());
        div()
            .on_children_prepainted(move |children, _, _| {
                if let Some(bounds) = children.first() {
                    placed.borrow_mut().insert(at.clone(), *bounds);
                }
            })
            .child(avatar)
            .into_any_element()
    }

    /// Where the face beside message `id` was drawn in the last frame, if it was (the UI tour's).
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn face_bounds(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.faces_placed.borrow().get(id).copied()
    }

    /// A message's column: its header, its bubble (with the "mentions you" mark beside it and its hover bar over its
    /// top while it is shown), its reactions over the bubble's bottom edge and its outbox notes.
    fn message_column(
        &self,
        item: &MessageItem,
        highlighted: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let bar = self.hover_bar(item, highlighted, palette, cx);
        let forward = forward_header(item, palette, cx);
        let column = div().flex_1().min_w_0().flex().flex_col();
        let column = if item.own {
            column.items_end()
        } else {
            column.items_start()
        };
        let cycle = clock::cycle(cx);
        // Teams' translation mark stands beside the time, or under the bubble when there is no header to hold it
        let mut mark = translation_mark(item, palette, cx);
        let cards = &self.models.cards;
        let header = item
            .header
            .then(|| header_line(item, (palette, cycle), mark.take(), cards));
        column
            .children(header)
            .child(bubble_line(
                item,
                InChat {
                    conversation: &self.conversation,
                    selection: &self.text_selection,
                    open: Some(Self::picture_opener(&item.id, cx)),
                    quoted: Some(Self::quote_opener(cx)),
                    ring: self.flash_ring(item, palette, cx),
                    cards: matches!(item.body, Body::Cards(_)).then(|| self.card_hook(&item.id, cx)),
                    root: self.place.in_channel().then(|| self.root_of(&item.id)),
                    meeting: Self::meeting_view(item, palette, cx),
                },
                highlighted,
                palette,
                bar,
                forward,
            ))
            .children(footer_line(item, mark, &self.reactors, palette, cx))
            .children(item.replies.as_ref().map(|replies| {
                let post = item.id.clone();
                let open: forwarded::OnOpen = Box::new(cx.listener(move |_, _: &gpui::ClickEvent, _, cx| {
                    cx.stop_propagation();
                    cx.emit(ChatViewEvent::OpenThread { post: post.clone() });
                }));
                channel::replies_line(&item.id, replies, palette, cycle, open)
            }))
            .children(
                item.outbox
                    .iter()
                    .map(|note| note_line(note, palette, cx))
                    .collect::<Vec<_>>(),
            )
    }

    /// A channel meeting's card, its organizer's Edit telling [`ChatViewEvent::EditMeeting`]; `None` for any other
    /// message.
    fn meeting_view(item: &MessageItem, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Body::Meeting(card) = &item.body else {
            return None;
        };
        let ical_uid = card.ical_uid.clone();
        let edit: forwarded::OnOpen = Box::new(cx.listener(move |_, _: &gpui::ClickEvent, _, cx| {
            cx.stop_propagation();
            cx.emit(ChatViewEvent::EditMeeting {
                ical_uid: ical_uid.clone(),
            });
        }));
        Some(channel::meeting_card(
            &item.id,
            card,
            palette,
            clock::cycle(cx),
            Some(edit),
        ))
    }

    /// What waits for the user's answer under the messages: the delete's confirmation, or the warning before "send
    /// again" on a row whose message may have arrived.
    fn confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Ask {
            question,
            yes,
            label,
            on_yes,
        } = if self.deleting.is_some() {
            Ask {
                question: CONFIRM_DELETE,
                yes: "confirm-delete",
                label: "Delete",
                on_yes: Box::new(ChatView::confirm_delete),
            }
        } else {
            let row = self.resending?;
            let post = self.note_of_row(row).is_none_or(|note| note.kind == OutboxKind::Send);
            Ask {
                question: if post { MAY_HAVE_ARRIVED } else { MAY_BE_OUT_OF_DATE },
                yes: "resend-anyway",
                label: "Send again",
                on_yes: Box::new(move |this, cx| this.send_again(row, cx)),
            }
        };
        let confirm = Button::new(yes)
            .label(label)
            .primary()
            .with_size(Size::XSmall)
            .on_click(cx.listener(move |this, _, _, cx| on_yes(this, cx)));
        let cancel = Button::new("confirm-cancel")
            .label("Cancel")
            .ghost()
            .with_size(Size::XSmall)
            .on_click(cx.listener(|this, _, window, cx| this.escape(window, cx)));
        Some(
            div()
                .debug_selector(|| "confirmation".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .py_1()
                .text_sm()
                .border_t_1()
                .border_color(rgb(palette.border))
                .child(question)
                .child(div().debug_selector(move || yes.to_owned()).child(confirm))
                .child(div().debug_selector(|| "confirm-cancel".to_owned()).child(cancel))
                .into_any_element(),
        )
    }
}

/// What a confirming button does.
type OnYes = Box<dyn Fn(&mut ChatView, &mut Context<ChatView>)>;

/// A question the view asks before it writes: its text, its confirming button's id and label, and what that does.
struct Ask {
    question: &'static str,
    yes: &'static str,
    label: &'static str,
    on_yes: OnYes,
}

/// Whether the chat is shown in its window: read receipts go out only while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Visibility {
    Shown,
    /// Behind the settings.
    Hidden,
}

/// Whether the window is the one the user is using: read receipts go out only while it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activation {
    Active,
    Inactive,
}

impl Activation {
    fn of(window: &Window) -> Self {
        if window.is_window_active() {
            Self::Active
        } else {
            Self::Inactive
        }
    }
}

/// The hover bar's quick reactions: the account's most used (`frequent`, the most used first; a key Teams doesn't
/// offer left out), then Teams' own ([`DEFAULT_QUICK_REACTIONS`]) to fill the row, [`QUICK_COUNT`] in all, none twice.
#[must_use]
pub fn quick_reactions(frequent: &[String]) -> Vec<String> {
    let mut quick: Vec<String> = Vec::with_capacity(QUICK_COUNT);
    let offered = frequent
        .iter()
        .map(String::as_str)
        .filter(|key| reactions::catalog().offers(key));
    for key in offered.chain(DEFAULT_QUICK_REACTIONS) {
        if quick.len() == QUICK_COUNT {
            break;
        }
        if !quick.iter().any(|known| known == key) {
            quick.push(key.to_owned());
        }
    }
    quick
}

/// A message's text as the reader sees it, its paragraphs a line each (an emoticon as its emoji), a list's items each
/// after its marker; a quote it carries (a reply's) and its pictures left out.
fn plain_text(rendered: &Rendered) -> String {
    rendered
        .blocks
        .iter()
        .flat_map(rich_text::Block::plain_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

impl ChatView {
    /// A message's hover bar, as Teams draws it, while the message is under the pointer or selected (or its bar, its
    /// reactions or its more-options menu is): over its bubble's top, flush with the bubble's right edge and never past
    /// its left, floating over the header line or the message above. In it, Teams' order: the quick reactions (the
    /// account's most used first, each setting or taking back the account's own), "React" (every reaction), then
    /// edit on the account's own, reply (Teams' quote), and "More options" holding the rest. None on a message
    /// that offers no action.
    fn hover_bar(
        &self,
        item: &MessageItem,
        highlighted: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let id = item.id.as_str();
        let reacting = self.reacting.as_deref() == Some(id);
        let shown = highlighted
            || reacting
            || self.hovered.as_deref() == Some(id)
            || self.bar_hovered.as_deref() == Some(id)
            || self.options.as_deref() == Some(id);
        let actions = item.actions();
        if !shown || actions.is_empty() {
            return None;
        }
        let quick = self
            .quick
            .iter()
            .enumerate()
            .map(|(index, key)| quick_button(item, key, reacting.then_some(index + 1), palette, cx))
            .collect::<Vec<_>>();
        let quick = div()
            .debug_selector(|| "reaction-picker".to_owned())
            .flex()
            .items_center()
            .gap(px(BAR_GAP))
            .children(quick);
        let edit = actions
            .contains(&MessageAction::Edit)
            .then(|| bar_button(item, MessageAction::Edit, cx));
        let measure = reacting.then(|| measured(&self.quick_bar));
        let hovered_id = item.id.clone();
        let bar = div()
            .id(ElementId::Name(format!("message-actions-{id}").into()))
            .debug_selector(|| "message-actions".to_owned())
            .relative()
            .flex()
            .items_center()
            .gap(px(BAR_GAP))
            .p(px(BAR_GAP))
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_md()
            // the message under it (the one above this one) is neither hovered nor clicked through the bar
            .block_mouse_except_scroll()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.bar_hovered = Some(hovered_id.clone());
                    cx.notify();
                } else if this.bar_hovered.as_deref() == Some(hovered_id.as_str()) {
                    this.bar_hovered = None;
                    cx.notify();
                }
            }))
            .children(measure)
            .child(quick)
            .child(react_button(item, reacting, palette, cx))
            .children(edit)
            // a chat's quote, a channel post's thread; none in a thread, where what is written is a reply
            .children(
                [MessageAction::Reply, MessageAction::ReplyInThread]
                    .into_iter()
                    .find(|action| actions.contains(action))
                    .map(|action| bar_button(item, action, cx)),
            )
            .child(self.options_button(item, cx));
        // over the bubble: its right edge the bubble's, its left never left of the bubble's
        let place = div()
            .absolute()
            .bottom(relative(1.0))
            .min_w(relative(1.0))
            .flex()
            .justify_end();
        let place = if item.own { place.right_0() } else { place.left_0() };
        Some(place.child(bar).into_any_element())
    }

    /// "More options" on the hover bar: opens or closes the menu of the message's other actions, under it.
    fn options_button(&self, item: &MessageItem, cx: &mut Context<Self>) -> gpui::Div {
        let message = item.id.clone();
        let open = self.options.as_deref() == Some(item.id.as_str());
        let button = icon_button(
            ElementId::Name(format!("message-more-{message}").into()),
            Glyph::MORE,
            "More options",
        )
        .with_size(Size::Size(px(BAR_BUTTON)))
        .selected(open)
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_options(&message, cx);
        }));
        selected_by("message-more", button)
            .relative()
            .children(open.then(|| measured(&self.options_at)))
    }

    /// The more-options menu, over the view under its button as it was last drawn: the message's actions the hover
    /// bar doesn't show, each with its key; a click outside closes it.
    fn options_overlay(&mut self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(message) = self.options.clone() else {
            self.options_at.set(None);
            return None;
        };
        let at = self.options_at.get()?;
        let actions = self.message_items().find(|item| item.id == message)?.actions();
        let mut entries = [MessageAction::Copy, MessageAction::Delete]
            .into_iter()
            .filter(|action| actions.contains(action))
            .map(|action| menu_entry(&message, action, palette, window, cx))
            .collect::<Vec<_>>();
        // Teams' workflows run on any message of a chat that takes the account's writes
        let offers_workflows = self
            .message_items()
            .find(|item| item.id == message)
            .is_some_and(MessageItem::offers_workflows);
        let workflows = if offers_workflows {
            self.message_workflows(cx)
        } else {
            Vec::new()
        };
        let more = offers_workflows.then(|| self.more_actions_list(&message, &workflows, palette, cx));
        if offers_workflows {
            entries.extend(self.workflow_entries(&message, &workflows, palette, cx));
        }
        let menu = div()
            .id("message-menu")
            .debug_selector(|| "message-menu".to_owned())
            // Teams' menu widens for the workflows' names
            .w(px(if workflows.is_empty() {
                MENU_WIDTH
            } else {
                WORKFLOW_MENU_WIDTH
            }))
            .p_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                // a click on its own button toggles it there; one in "More actions"' list is the menu's own
                let in_list = this.more_actions.is_some()
                    && this.more_at.get().is_some_and(|list| list.contains(&event.position));
                if !at.contains(&event.position) && !in_list {
                    this.options = None;
                    cx.notify();
                }
            }))
            .children(entries);
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::TopRight)
                    .position(at.bottom_right())
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .items_end()
                            .gap_1()
                            .children(more.flatten())
                            .child(menu),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

impl ChatView {
    /// The menu's workflows group (the first [`crate::workflow_run::menu::MAIN_MENU`] workflows that run from a
    /// message) and "More actions", after a hairline.
    fn workflow_entries(
        &self,
        message: &str,
        workflows: &[crate::workflow_dialog::FlowRef],
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        use crate::workflow_run::menu;
        let mut entries = vec![menu::separator(palette)];
        for flow in workflows.iter().take(menu::MAIN_MENU) {
            let (flow_ref, message) = (flow.clone(), message.to_owned());
            let view = cx.entity().downgrade();
            entries.push(menu::workflow_row(flow, palette, move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.run_workflow(flow_ref.clone(), &message, cx));
            }));
        }
        let view = cx.entity().downgrade();
        // one hairline between the groups when there are no workflows to list
        if !workflows.is_empty() {
            entries.push(menu::separator(palette));
        }
        let open = self.more_actions.as_deref() == Some(message);
        let shown = message.to_owned();
        entries.push(menu::more_actions_row(open, palette, move |_, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.more_actions = if this.more_actions.is_some() {
                    None
                } else {
                    Some(shown.clone())
                };
                cx.notify();
            });
        }));
        entries
    }

    /// "More actions"' list beside the menu, while it is open: the other workflows, then "Create workflows".
    fn more_actions_list(
        &self,
        message: &str,
        workflows: &[crate::workflow_dialog::FlowRef],
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        use crate::workflow_run::menu;
        if self.more_actions.as_deref() != Some(message) {
            return None;
        }
        let mut list = menu::panel("message-more-actions-menu", palette)
            .relative()
            .child(measured(&self.more_at));
        let rest: Vec<_> = workflows.iter().skip(menu::MAIN_MENU).collect();
        for flow in &rest {
            let (flow_ref, message) = ((*flow).clone(), message.to_owned());
            let view = cx.entity().downgrade();
            list = list.child(menu::workflow_row(flow, palette, move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.run_workflow(flow_ref.clone(), &message, cx));
            }));
        }
        if !rest.is_empty() {
            list = list.child(menu::separator(palette));
        }
        let (view, message) = (cx.entity().downgrade(), message.to_owned());
        list = list.child(menu::create_row(palette, move |_, _, cx| {
            let _ = view.update(cx, |this, cx| this.create_workflows(&message, cx));
        }));
        Some(list.into_any_element())
    }

    /// The right-click menu over the text selected with the mouse, while it is open: "Copy", with its key.
    fn selection_menu(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = Kbd::binding_for_action(&CopySelected, Some(CONTEXT), window).map(Kbd::outline);
        let view = cx.entity().downgrade();
        selection::menu(&self.text_selection, palette, key, move |_, cx| {
            let _ = view.update(cx, |this, cx| this.copy_selection(cx));
        })
    }
}

/// An empty element over its parent that keeps where the parent was last drawn in `drawn_at`, for a popup to open by.
pub(crate) fn measured(drawn_at: &Rc<Cell<Option<Bounds<Pixels>>>>) -> AnyElement {
    let drawn_at = Rc::clone(drawn_at);
    canvas(
        move |bounds, window, _| {
            if drawn_at.replace(Some(bounds)) != Some(bounds) {
                // the popup is drawn before the list's rows: it moves with its anchor on the next frame
                window.on_next_frame(|window, _| window.refresh());
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// An action's icon button on the hover bar, its tooltip naming it and its key.
fn bar_button(item: &MessageItem, action: MessageAction, cx: &mut Context<ChatView>) -> gpui::Div {
    let message = item.id.clone();
    let name = action.name();
    let id = ElementId::Name(format!("action-{name}-{message}").into());
    let button = icon_button_with_key(id, action.glyph(), action.label(), action.key_action())
        .with_size(Size::Size(px(BAR_BUTTON)))
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.act(action, &message, window, cx);
        }));
    selected_by(format!("action-{name}"), button)
}

/// "React" on the hover bar (Teams' smiley with a plus): every reaction Teams offers, over the view. While the quick
/// reactions are open on the message (react's key), it is the 5th, its number in the corner.
fn react_button(item: &MessageItem, reacting: bool, palette: Palette, cx: &mut Context<ChatView>) -> gpui::Div {
    let message = item.id.clone();
    let action = MessageAction::React;
    let id = ElementId::Name(format!("action-react-{message}").into());
    let button = icon_button_with_key(id, action.glyph(), action.label(), action.key_action())
        .with_size(Size::Size(px(BAR_BUTTON)))
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.act(action, &message, window, cx);
            this.open_more(window, cx);
        }));
    div()
        .debug_selector(|| "pick-more".to_owned())
        .relative()
        .child(selected_by("action-react", button))
        .children(reacting.then(|| key_number(5, palette)))
}

/// A quick reaction on the hover bar: sets the account's own, or takes it back when it is set (then marked). While the
/// quick reactions are open on the message, its key's `number` is in its corner.
fn quick_button(
    item: &MessageItem,
    key: &str,
    number: Option<usize>,
    palette: Palette,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let mine = item
        .reactions
        .iter()
        .any(|reaction| reaction.key == key && reaction.mine);
    let (message, toggled) = (item.id.clone(), key.to_owned());
    let selector = format!("pick-{key}");
    let described = SharedString::from(reactions::description(key).to_owned());
    let button = div()
        .id(ElementId::Name(format!("pick-{key}-{message}").into()))
        .debug_selector(move || selector.clone())
        .relative()
        .size(px(BAR_BUTTON))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(palette.hover)))
        .tooltip(move |window, cx| Tooltip::new(described.clone()).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_reaction(&message, &toggled, cx);
        }))
        .child(reactions::emoji(key, px(QUICK_EMOJI)))
        .children(number.map(|number| key_number(number, palette)));
    let button = if mine { button.bg(rgb(palette.selected)) } else { button };
    button.into_any_element()
}

/// A key's number in the corner of a hover bar button, while the quick reactions are open.
fn key_number(number: usize, palette: Palette) -> gpui::Div {
    div()
        .absolute()
        .top_0()
        .left(px(2.0))
        .text_size(px(9.0))
        .text_color(rgb(palette.muted_foreground))
        .child(number.to_string())
}

/// An entry of the more-options menu: the action's icon, its name and its key; delete in Teams' red.
fn menu_entry(
    message: &str,
    action: MessageAction,
    palette: Palette,
    window: &Window,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let message = message.to_owned();
    let key = Kbd::binding_for_action(action.key_action(), Some(CONTEXT), window).map(Kbd::outline);
    let colour = if action == MessageAction::Delete {
        theme::DANGER
    } else {
        palette.foreground
    };
    let selector = format!("action-{}", action.name());
    div()
        .id(ElementId::Name(selector.clone().into()))
        .debug_selector(move || selector.clone())
        .w_full()
        .h(px(32.0))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .text_color(rgb(colour))
        .hover(move |row| row.bg(rgb(palette.hover)))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.options = None;
            this.act(action, &message, window, cx);
        }))
        .child(action.glyph().icon().size(px(14.0)))
        .child(div().flex_1().child(action.label()))
        .children(key)
        .into_any_element()
}

/// An outbox row's state under the message it concerns, with "send again" when it is offered.
fn note_line(note: &Note, palette: Palette, cx: &mut Context<ChatView>) -> AnyElement {
    let row = note.row;
    let again = (note.again != Again::No).then(|| {
        div().debug_selector(move || format!("send-again-{row}")).child(
            Button::new(ElementId::Name(format!("send-again-{row}").into()))
                .icon(Glyph::REFRESH.icon())
                .label("Send again")
                .ghost()
                .with_size(Size::XSmall)
                .on_click(cx.listener(move |this, _, _, cx| this.send_again(row, cx))),
        )
    });
    let colour = match note.state {
        RowState::Denied | RowState::Failed => palette.foreground,
        _ => palette.muted_foreground,
    };
    div()
        .debug_selector(move || format!("outbox-{row}"))
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(rgb(colour))
        .child(note.text())
        .children(again)
        .into_any_element()
}

/// A message post on its way: the account's bubble, muted, with the row's state.
fn pending_row(
    pending: &PendingItem,
    selection: &TextSelection,
    palette: Palette,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let row = pending.note.row;
    let id: SharedString = format!("pending-text-{row}").into();
    let scope = Scope::new(selection, pending_key(pending));
    let content = match &pending.body {
        Some(text) => rich_text::render_selectable(&id, &text.0, palette, None, Some(&scope)),
        None => muted(UNSUPPORTED, palette),
    };
    div()
        .debug_selector(move || format!("pending-{row}"))
        .w_full()
        .px_4()
        .pt(px(2.0))
        .flex()
        .flex_col()
        .items_end()
        .child(
            div()
                .max_w(relative(0.8))
                .px_3()
                .py_1()
                .rounded_md()
                .opacity(0.7)
                .bg(rgb(palette.selected))
                .child(content),
        )
        .children(files::file_cards(
            &format!("pending-{row}"),
            &pending.files,
            None,
            palette,
        ))
        .child(note_line(&pending.note, palette, cx))
        .into_any_element()
}

/// A message's place among the view's messages for the text selection: as the list orders them.
#[must_use]
pub fn message_key(item: &MessageItem) -> MessageKey {
    MessageKey::new((item.arrival_time, 0), item.id.clone())
}

/// A pending post's place among the view's messages for the text selection: after every message, by its row.
fn pending_key(pending: &PendingItem) -> MessageKey {
    MessageKey::new((i64::MAX, pending.note.row), format!("pending-{}", pending.note.row))
}

/// Whether a load read the account's read horizon, and what it was.
enum Horizon {
    NotRead,
    /// The last read message's arrival time; `None` when the account has none in the chat.
    Read(Option<i64>),
}

/// What a load read.
struct Read {
    loaded: Option<Loaded>,
    /// Read on the first load.
    horizon: Horizon,
    /// The history's new state, when the load learnt it.
    history: Option<History>,
    /// The account's writes and read horizon as the store has them now: read by every window load.
    outbox: Option<OutboxRead>,
    /// A channel's messages' posts, by message id: read by every load of a channel's view ([`in_channel`]).
    roots: Option<HashMap<String, String>>,
}

/// The account's writes in the conversation and its read horizon, as a window load read them.
struct OutboxRead {
    unsettled: Vec<OutboxEntry>,
    earlier: HashSet<i64>,
    /// The arrival time of the last message the account has read.
    read_upto: Option<i64>,
    /// The message posts looked up whose message the store has, by row id.
    stored: Vec<i64>,
}

/// The conversation's rows that aren't sent, the rows earlier sessions left, and the account's read horizon now; what
/// can't be read is none.
async fn outbox_read(core: &dyn Core, conversation: &str) -> OutboxRead {
    let unsettled = core.outbox_unsettled(conversation.to_owned()).await.unwrap_or_default();
    let earlier = core
        .left_by_earlier_runs()
        .await
        .map(|rows| rows.into_iter().map(|row| row.id).collect())
        .unwrap_or_default();
    let read_upto = core
        .read_horizon(conversation.to_owned())
        .await
        .ok()
        .flatten()
        .map(|horizon| horizon.last_read_arrival);
    OutboxRead {
        unsettled,
        earlier,
        read_upto,
        stored: Vec::new(),
    }
}

/// The posts of `find` (row ids, the message ids they were sent as) whose message the store has.
async fn stored_posts(core: &dyn Core, conversation: &str, find: &[(i64, String)]) -> Vec<i64> {
    let mut stored = Vec::new();
    for (row, message_id) in find {
        let found = core.message(conversation.to_owned(), message_id.clone()).await;
        if matches!(found, Ok(Some(_))) {
            stored.push(*row);
        }
    }
    stored
}

/// The window load: the newest messages down to `oldest`, the account's writes and read horizon now, which of the
/// posts of `find` the store has, and on the first load that horizon for the "New messages" divider.
async fn window(
    core: &dyn Core,
    conversation: &str,
    oldest: Option<(i64, String)>,
    first: bool,
    held: &HashMap<String, i64>,
    find: &[(i64, String)],
) -> Result<Read, EngineError> {
    let mut outbox = outbox_read(core, conversation).await;
    outbox.stored = stored_posts(core, conversation, find).await;
    let horizon = if first {
        Horizon::Read(outbox.read_upto)
    } else {
        Horizon::NotRead
    };
    let (messages, next) = read_window(core, conversation, oldest).await?;
    let reactions = new_reactions(core, conversation, &messages, held).await;
    Ok(Read {
        loaded: Some(Loaded {
            messages,
            reactions,
            next,
        }),
        horizon,
        history: None,
        outbox: Some(outbox),
        roots: None,
    })
}

/// Where a view of `conversation` in `place` writes: a thread's post's chain, else the conversation.
pub(crate) fn writes_to(conversation: &str, place: &Place) -> String {
    match place {
        Place::Thread { post } => {
            teams_proto::reply_chain_id(conversation, post).unwrap_or_else(|| conversation.to_owned())
        }
        Place::Chat | Place::Channel => conversation.to_owned(),
    }
}

/// Where a thread's window load reads back to: its post, when the store has it and it is older than `oldest` (what is
/// held); else `oldest`. Any other place's is `oldest`.
async fn thread_oldest(
    core: &dyn Core,
    conversation: &str,
    place: &Place,
    oldest: Option<(i64, String)>,
) -> Option<(i64, String)> {
    let Place::Thread { post } = place else {
        return oldest;
    };
    let Ok(Some(post)) = core.message(conversation.to_owned(), post.clone()).await else {
        return oldest;
    };
    let at = (post.arrival_time, post.id);
    Some(oldest.map_or(at.clone(), |held| held.min(at)))
}

/// What a channel's load reads besides a chat's: its messages' posts, and a thread's message posts not sent yet (rows
/// of the post's chain, `writes_to`).
async fn in_channel(core: &dyn Core, conversation: &str, writes_to: &str, mut read: Read) -> Read {
    read.roots = core
        .reply_roots(conversation.to_owned())
        .await
        .ok()
        .map(|roots| roots.into_iter().collect());
    if writes_to != conversation
        && let Some(outbox) = &mut read.outbox
    {
        outbox
            .unsettled
            .extend(core.outbox_unsettled(writes_to.to_owned()).await.unwrap_or_default());
    }
    read
}

/// The older load: the store's next page; once the store has none, the service's next older page read into the store
/// first.
async fn older(
    core: &dyn Core,
    conversation: &str,
    oldest: Option<(i64, String)>,
    next: Option<PageCursor>,
    held: &HashMap<String, i64>,
) -> Result<Read, EngineError> {
    let before = if let Some(next) = next {
        next
    } else {
        if core.load_older(conversation.to_owned()).await?.is_none() {
            // nothing to read: the beginning only once the store says the whole history is stored
            let history = match core.history_complete(conversation.to_owned()).await? {
                Some(true) => History::Complete,
                Some(false) | None => History::Unsynced,
            };
            return Ok(Read {
                loaded: None,
                horizon: Horizon::NotRead,
                history: Some(history),
                outbox: None,
                roots: None,
            });
        }
        let Some((arrival_time, id)) = oldest else {
            // nothing was held: the newest page now
            return window(core, conversation, None, false, held, &[]).await;
        };
        PageCursor { arrival_time, id }
    };
    let page = core.messages(conversation.to_owned(), Some(before), PAGE).await?;
    let reactions = new_reactions(core, conversation, &page.messages, held).await;
    Ok(Read {
        loaded: Some(Loaded {
            messages: page.messages,
            reactions,
            next: page.next,
        }),
        horizon: Horizon::NotRead,
        history: None,
        outbox: None,
        roots: None,
    })
}

/// A row's identity in the list, for telling which rows changed.
type Key = (u8, String);

fn key(item: &Item) -> Key {
    match item {
        Item::Top(_) => (0, String::new()),
        Item::Day(day) => (1, day.to_string()),
        Item::NewMessages => (2, String::new()),
        Item::Message(message) => (3, message.id.clone()),
        Item::Pending(pending) => (4, pending.note.row.to_string()),
    }
}

/// Tells `list` how `old` became `new`, row by row: rows only in `old` are removed and rows only in `new` added where
/// they stand, and a row in both that changed is measured again. Every other row keeps its place and its measured
/// height. The rows in both keep their order (the messages' order); should they not, the rest is replaced.
fn splice(list: &ListState, old: &[Item], new: &[Item]) {
    let old_keys: Vec<Key> = old.iter().map(key).collect();
    let new_keys: Vec<Key> = new.iter().map(key).collect();
    let in_old: HashSet<&Key> = old_keys.iter().collect();
    let in_new: HashSet<&Key> = new_keys.iter().collect();
    // `at_new` is where `old[at_old]` stands in the list: the rows above it are told already
    let (mut at_old, mut at_new) = (0, 0);
    while at_old < old.len() || at_new < new.len() {
        // a run of rows gone and a run of rows come, replaced together
        let gone = old_keys[at_old..]
            .iter()
            .take_while(|key| !in_new.contains(key))
            .count();
        let came = new_keys[at_new..]
            .iter()
            .take_while(|key| !in_old.contains(key))
            .count();
        if gone + came > 0 {
            list.splice(at_new..at_new + gone, came);
            at_old += gone;
            at_new += came;
            continue;
        }
        if at_old == old.len() || at_new == new.len() || old_keys[at_old] != new_keys[at_new] {
            list.splice(at_new..at_new + (old.len() - at_old), new.len() - at_new);
            return;
        }
        // a run of the same rows: those that changed are measured again
        let run = old_keys[at_old..]
            .iter()
            .zip(&new_keys[at_new..])
            .take_while(|(old, new)| old == new)
            .count();
        remeasure_changed(list, &old[at_old..at_old + run], &new[at_new..at_new + run], at_new);
        at_old += run;
        at_new += run;
    }
}

/// Measures again the rows of `old` that changed in `new` (the same rows, standing in the list from `at`), a run of
/// them at a time.
fn remeasure_changed(list: &ListState, old: &[Item], new: &[Item], at: usize) {
    let mut changed: Option<usize> = None;
    for (offset, (old, new)) in old.iter().zip(new).enumerate() {
        match (old != new, changed) {
            (true, None) => changed = Some(offset),
            (false, Some(from)) => {
                list.remeasure_items(at + from..at + offset);
                changed = None;
            }
            _ => {}
        }
    }
    if let Some(from) = changed {
        list.remeasure_items(at + from..at + old.len());
    }
}

/// Where the view is read from, kept across a change of the rows by the rows' identity, never their index: the first
/// message row at or below the list's top, and the rows above it in the view (the top, a day, the divider).
struct Anchor {
    message: Key,
    /// The rows from the list's top row down to the message row (none when the message row is the top row).
    lead: Vec<Key>,
    /// How far into the list's top row the view starts.
    offset: Pixels,
}

impl Anchor {
    /// Where `list`, showing `items`, is; `None` while it follows the tail (a new row scrolls in) or shows no message.
    fn of(list: &ListState, items: &[Item]) -> Option<Self> {
        if list.is_following_tail() {
            return None;
        }
        let top = list.logical_scroll_top();
        let (at, message) = items
            .iter()
            .enumerate()
            .skip(top.item_ix)
            .find(|(_, item)| matches!(item, Item::Message(_)))?;
        Some(Self {
            message: key(message),
            lead: items[top.item_ix..at].iter().map(key).collect(),
            offset: top.offset_in_item,
        })
    }

    /// Scrolls `list`, now showing `items`, back to the anchor: the same rows at the top, from the same offset. When
    /// the rows above the message row changed (an older page came in below the top row), the message row is put at
    /// the top, so what was read stays in view and the new rows are above it.
    fn restore(self, list: &ListState, items: &[Item]) {
        let Some(at) = items.iter().position(|item| key(item) == self.message) else {
            return;
        };
        let lead = at
            .checked_sub(self.lead.len())
            .filter(|&from| items[from..at].iter().map(key).eq(self.lead.iter().cloned()));
        list.scroll_to(match lead {
            Some(from) => ListOffset {
                item_ix: from,
                offset_in_item: self.offset,
            },
            None => ListOffset {
                item_ix: at,
                offset_in_item: px(0.0),
            },
        });
    }
}

fn centered_line(text: impl Into<SharedString>, palette: Palette) -> gpui::Div {
    div()
        .w_full()
        .py_2()
        .flex()
        .justify_center()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(text.into())
}

/// The top row: how far the history is loaded; when no load will run by itself, a button to load again.
fn top_row(history: History, place: &Place, palette: Palette, cx: &mut Context<ChatView>) -> AnyElement {
    let label = match (place, history) {
        // a thread opens with its post: nothing above it
        (Place::Thread { .. }, _) => return div().into_any_element(),
        (Place::Channel, History::Complete) => "This is the beginning of the channel",
        _ => history.label(),
    };
    let again = history.again().map(|label| {
        div().debug_selector(|| "load-older".to_owned()).child(
            Button::new("load-older-again")
                .label(label)
                .ghost()
                .with_size(Size::XSmall)
                .on_click(cx.listener(|this, _, _, cx| this.load_older(cx))),
        )
    });
    centered_line(label, palette)
        .gap_2()
        .items_center()
        .debug_selector(|| "history-top".to_owned())
        .children(again)
        .into_any_element()
}

fn day_row(day: NaiveDate, palette: Palette) -> AnyElement {
    let label = day_label(day, Local::now().date_naive());
    centered_line(label, palette)
        .debug_selector(move || format!("day-{day}"))
        .into_any_element()
}

fn new_messages_row(palette: Palette) -> AnyElement {
    let rule = || div().flex_1().h(px(1.0)).bg(rgb(palette.brand));
    div()
        .debug_selector(|| "new-messages".to_owned())
        .w_full()
        .px_4()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .text_color(rgb(palette.brand))
        .child(rule())
        .child(NEW_MESSAGES)
        .child(rule())
        .into_any_element()
}

/// Whom a message's face and name open the card of: its sender, as the message shows them; none for the account's
/// own and an app's.
fn card_of(item: &MessageItem) -> Option<profile_card::CardOf> {
    let sender = item.sender.as_deref().filter(|_| !item.own)?;
    let name = item.face.named.then(|| item.sender_name.clone());
    Some(profile_card::CardOf::new(sender, name).with_picture(item.face.picture.clone()))
}

/// A message's header, Teams' way: the sender's name (none on the account's own) and its time (on `cycle`'s clock),
/// both muted, over the bubble's left edge, then "Edited" once edited. The time's test selector is
/// `message-time-<id> <time>`.
fn header_line(
    item: &MessageItem,
    (palette, cycle): (Palette, HourCycle),
    mark: Option<AnyElement>,
    cards: &Entity<profile_card::CardAsks>,
) -> AnyElement {
    let time = clock::message_time(item.arrival_time, &Local::now(), cycle);
    let id = &item.id;
    let time = div().debug_selector(|| format!("message-time-{id} {time}")).child(time);
    // the name opens the sender's compact card, on a click or a rest, as Teams' does
    let name = (!item.own).then(|| {
        let name = div().child(item.sender_name.clone());
        match card_of(item) {
            Some(of) => profile_card::anchor(
                format!("profile-card-from-name-{id}"),
                of,
                profile_card::Click::Compact,
                cards,
                name,
            ),
            None => name.into_any_element(),
        }
    });
    div()
        .h(rems(HEADER_REMS))
        .flex()
        .gap_3()
        .items_center()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .children(name)
        .child(time)
        .children(item.edited.then(edited_mark))
        .children(mark)
        .into_any_element()
}

/// How faded a message's text is while its translation comes.
const WAITING_OPACITY: f32 = 0.6;
/// What a message waiting for its translation says beside its time.
pub const TRANSLATING: &str = "Translating…";

/// A message waiting for its translation: Teams' translation mark and "Translating…", a light sweeping over it while
/// animations play.
fn translating_mark(id: &str, palette: Palette, cx: &App) -> AnyElement {
    let selector = format!("translation-mark-{id}");
    let label: AnyElement = if crate::motion::on(cx) {
        ShimmerText::new(TRANSLATING)
            .id(SharedString::from(format!("translating-{id}")))
            .highlight_color(rgb(palette.brand))
            .into_any_element()
    } else {
        div().child(TRANSLATING).into_any_element()
    };
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_1()
        .p(px(2.0))
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(Glyph::LANGUAGES.icon().size(px(14.0)))
        .child(label)
        .into_any_element()
}

/// Teams' translation mark of a message of a translated chat: a click shows it as sent, and back; while its
/// translation comes, "Translating…" ([`translating_mark`]); one that couldn't be translated, a mark that says so.
fn translation_mark(item: &MessageItem, palette: Palette, cx: &mut Context<ChatView>) -> Option<AnyElement> {
    use chat_translate::Mark;
    let mark = item.translation.as_ref()?;
    let id = item.id.clone();
    let (tip, color, toggles): (SharedString, u32, bool) = match mark {
        Mark::Translated { tip } => (tip.clone(), palette.muted_foreground, true),
        Mark::Original { tip } => (tip.clone(), palette.brand, true),
        Mark::Waiting => return Some(translating_mark(&id, palette, cx)),
        Mark::Failed => ("This message couldn't be translated".into(), theme::DANGER, false),
    };
    let selector = format!("translation-mark-{id}");
    let glyph = Glyph::LANGUAGES.icon().size(px(14.0)).text_color(rgb(color));
    let face = div()
        .id(ElementId::Name(selector.clone().into()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .rounded_sm()
        .p(px(2.0))
        .child(glyph)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    let face = if toggles {
        face.cursor_pointer()
            .hover(move |face| face.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                cx.stop_propagation();
                this.translation
                    .update(cx, |translation, cx| translation.toggle_original(&id, cx));
            }))
    } else {
        face
    };
    Some(face.into_any_element())
}

/// The header of a forwarded message's card ([`forwarded::header`]); a click on the original's time tells
/// [`ChatViewEvent::OpenOriginal`].
fn forward_header(item: &MessageItem, palette: Palette, cx: &mut Context<ChatView>) -> Option<AnyElement> {
    let origin = item.forwarded.as_ref()?;
    let open = origin.original.clone().map(|original| {
        Box::new(cx.listener(move |_, _: &gpui::ClickEvent, _, cx| {
            cx.stop_propagation();
            cx.emit(ChatViewEvent::OpenOriginal(original.clone()));
        })) as forwarded::OnOpen
    });
    Some(forwarded::header(&item.id, origin, palette, clock::cycle(cx), open))
}

/// "Edited", beside a message's time.
fn edited_mark() -> gpui::Div {
    div().debug_selector(|| "edited".to_owned()).child("Edited")
}

/// A message as the trace last told it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Traced {
    id: String,
    kind: &'static str,
    reacted: bool,
    reply: bool,
}

/// Whether a message's text quotes another message (a reply's quote names the message it quotes).
fn is_reply(item: &MessageItem) -> bool {
    match &item.body {
        Body::Text(text) => text
            .0
            .blocks
            .iter()
            .any(|block| matches!(block, rich_text::Block::Quote(quote) if quote.message_id.is_some())),
        _ => false,
    }
}

/// A row's kind in the trace: `edited` once edited, `unshown` for a text with a part no capture showed, else its
/// body's.
fn trace_kind(item: &MessageItem) -> &'static str {
    if item.edited {
        "edited"
    } else if matches!(item.body, Body::Text(_)) && item.unshown.any() {
        "unshown"
    } else {
        item.body.kind()
    }
}

/// Whether someone else's message mentions the account in its own text: Teams marks it beside its bubble.
fn mentions_me(item: &MessageItem) -> bool {
    !item.own && matches!(&item.body, Body::Text(text) if text.0.mentions_me())
}

/// A message's bubble (with its hover bar `bar` over its top, while shown, and `forward`, a forwarded message's card's
/// header) and, for someone else's, the column right of it that holds Teams' "@" when it mentions the account.
fn bubble_line(
    item: &MessageItem,
    chat: InChat<'_>,
    highlighted: bool,
    palette: Palette,
    bar: Option<AnyElement>,
    forward: Option<AnyElement>,
) -> gpui::Div {
    let line = div().w_full().flex().items_start();
    if item.own {
        return line
            .justify_end()
            .child(bubble(item, chat, highlighted, palette, bar, forward));
    }
    let id = item.id.clone();
    let mark = mentions_me(item).then(|| {
        div()
            .debug_selector(move || format!("mentions-me-{id}"))
            .text_lg()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(palette.mention))
            .child("@")
    });
    line.child(bubble(item, chat, highlighted, palette, bar, forward))
        .child(div().w(px(MARK_COLUMN)).flex_none().pt_1().pl_2().children(mark))
}

/// A bubble's face: the account's own in its tint running into the accent's, lifted on a faint glow of the brand
/// colour; someone else's on the surface, lifted a little; the selected one in the selection's colour.
fn bubble_surface(bubble: gpui::Div, own: bool, highlighted: bool, palette: Palette) -> gpui::Div {
    match (highlighted, own) {
        (true, _) => bubble.bg(rgb(palette.selection)),
        (false, true) => bubble
            .bg(theme::gradient(
                palette.selected,
                theme::mix(palette.selected, palette.accent, 0.12),
                120.0,
            ))
            .shadow(theme::glow(palette, palette.brand, 0.14, 8.0, 2.0)),
        (false, false) => bubble
            .bg(rgb(palette.surface))
            .shadow(theme::glow(palette, 0x0000_0000, 0.07, 6.0, 1.0)),
    }
}

/// Where a message's bubble is drawn: its chat (for its links to Teams on the web), the view's text selection, and
/// what a click on one of its pictures opens.
struct InChat<'a> {
    conversation: &'a str,
    selection: &'a TextSelection,
    open: Option<OpenPicture>,
    /// What a click on one of its reply quotes calls: the view going to the quoted message.
    quoted: Option<rich_text::OpenQuoted>,
    /// The ring over the bubble while a quote's click has just gone to it ([`ChatView::flash_ring`]).
    ring: Option<AnyElement>,
    /// A card message's: what its cards show and who is told when a click changes it.
    cards: Option<CardHook>,
    /// A channel's message's thread's post (its own id for a post): what a card's button opens in Teams on the web.
    root: Option<&'a str>,
    /// A channel meeting's card, drawn as the message's content ([`channel::meeting_card`]).
    meeting: Option<AnyElement>,
}

impl InChat<'_> {
    /// Where message `id` opens in Teams on the web: a chat's link, or in a channel the message's own (in its thread,
    /// as a card's button opens it).
    fn web_link(&self, id: &str) -> String {
        match self.root {
            Some(root) => teams_proto::workflow::MessageRun::link_to(self.conversation, id, Some(root)),
            None => web_chat_link(self.conversation),
        }
    }
}

/// What a message's cards are drawn with in the chat ([`crate::card_view`]).
struct CardHook {
    state: CardState,
    on_state: crate::card_view::OnState,
    source: rich_text::PictureSource,
    cycle: HourCycle,
}

/// What `item`'s bubble holds: its text, its cards, its meeting, or the line a message drawn elsewhere leaves.
fn bubble_content(
    item: &MessageItem,
    chat: &mut InChat<'_>,
    id: &SharedString,
    palette: Palette,
    forward: Option<AnyElement>,
) -> AnyElement {
    match &item.body {
        Body::Text(text) => {
            let mut scope = Scope::new(chat.selection, message_key(item));
            if let Some(open) = chat.open.clone() {
                scope = scope.opening(open);
            }
            if let Some(quoted) = chat.quoted.clone() {
                scope = scope.quoting(quoted);
            }
            let drawn = rich_text::render_selectable(id, &text.0, palette, forward, Some(&scope));
            // while its translation comes it shows as written, faded, as Teams fades a message being translated
            if item.translation == Some(chat_translate::Mark::Waiting) {
                div().opacity(WAITING_OPACITY).child(drawn).into_any_element()
            } else {
                drawn
            }
        }
        Body::Deleted => muted(DELETED, palette),
        Body::Cards(cards) => {
            let drawn = CardsElement::new(item.id.clone(), cards.clone(), palette, HourCycle::Twelve)
                // a button that sends to the bot opens the card's message in Teams on the web, where it runs
                .in_teams(Some(teams_proto::workflow::MessageRun::link_to(
                    chat.conversation,
                    &item.id,
                    chat.root,
                )));
            match chat.cards.take() {
                Some(hook) => drawn
                    .state(hook.state.clone(), Rc::clone(&hook.on_state))
                    .source(Some(hook.source.clone()))
                    .clock(hook.cycle),
                None => drawn,
            }
            .into_any_element()
        }
        Body::Meeting(_) => chat.meeting.take().unwrap_or_else(|| div().into_any_element()),
        Body::Unsupported(_) => web_line(
            &format!("unsupported-{}", item.id),
            UNSUPPORTED,
            &item.id,
            chat.web_link(&item.id),
            palette,
        ),
        Body::Event(_) | Body::TimedEvent(_) | Body::MeetingEnded(_) | Body::Card(_) | Body::UnsupportedEvent(_) => {
            div().into_any_element()
        }
    }
}

fn bubble(
    item: &MessageItem,
    mut chat: InChat<'_>,
    highlighted: bool,
    palette: Palette,
    bar: Option<AnyElement>,
    forward: Option<AnyElement>,
) -> AnyElement {
    let id: SharedString = format!("message-text-{}", item.id).into();
    let (files, links) = match &item.body {
        Body::Text(text) => (
            files::file_cards(&item.id, &item.files, text.0.source.as_ref(), palette),
            link_preview::cards(&item.id, &item.links, text.0.source.as_ref(), palette),
        ),
        _ => (None, None),
    };
    let content = bubble_content(item, &mut chat, &id, palette, forward);
    let selector = format!("bubble-{}", item.id);
    let bubble = div()
        .debug_selector(move || selector)
        // the hover bar's anchor
        .relative()
        .min_w_0();
    // a message drawn bare (only an emoticon or a sticker, `Rendered::bare`) or of files alone has no bubble behind
    // it, as in Teams
    let bare = matches!(&item.body, Body::Text(text) if text.0.bare || (text.0.blocks.is_empty() && files.is_some()))
        // a card (a channel meeting's too) is its own surface
        || matches!(&item.body, Body::Cards(_) | Body::Meeting(_));
    let bubble = if bare {
        bubble
    } else {
        bubble_surface(
            bubble.px(px(BUBBLE_INSET)).py_2().rounded_lg(),
            item.own,
            highlighted,
            palette,
        )
    };
    // the account's own keep Teams' margin on their left; someone else's reach the mark's column
    let bubble = if item.own { bubble.max_w(relative(0.8)) } else { bubble };
    bubble
        .child(content)
        .children(links)
        .children(files)
        .children(item.unshown.any().then(|| {
            div()
                .text_xs()
                .child(web_line(
                    &format!("unshown-{}", item.id),
                    UNSHOWN_CONTENT,
                    &item.id,
                    chat.web_link(&item.id),
                    palette,
                ))
                .into_any_element()
        }))
        .children(chat.ring.take())
        // last, so it is drawn over the bubble
        .children(bar)
        .into_any_element()
}

/// `note` (the line under a message's text for a part no capture showed, [`UNSHOWN_CONTENT`], or a type's
/// placeholder, [`UNSUPPORTED`]) and [`OPEN_ON_THE_WEB`], which opens `link` (the chat, or a channel's message itself)
/// in the web client through GPUI's `open_url` (the portal or `xdg-open`; the sandbox's no-op opener in tests). Never
/// for a deleted message.
fn web_line(selector: &str, note: &'static str, id: &str, link: String, palette: Palette) -> AnyElement {
    let selector = selector.to_owned();
    let open = format!("open-on-the-web-{id}");
    let button = Button::new(SharedString::from(open.clone()))
        .label(OPEN_ON_THE_WEB)
        .ghost()
        .with_size(Size::XSmall)
        .on_click(move |_, _, cx| cx.open_url(&link));
    div()
        .debug_selector(move || selector)
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .child(muted(note, palette))
        .child(div().debug_selector(move || open).child(button))
        .into_any_element()
}

fn muted(text: &'static str, palette: Palette) -> AnyElement {
    div()
        .italic()
        .text_color(rgb(palette.muted_foreground))
        .child(text)
        .into_any_element()
}

/// The reactions as Teams draws them, over the bubble's bottom edge from its text's inset: a pill per key (its count
/// once more than one reacted, the account's own ringed in the brand colour; a click sets or takes back the account's
/// own), then "Add reaction", which opens the reactions on the message. "Edited" stands here when there is no header
/// to hold it.
fn footer_line(
    item: &MessageItem,
    mark: Option<AnyElement>,
    reactors: &reactors::Reactors,
    palette: Palette,
    cx: &mut Context<ChatView>,
) -> Option<AnyElement> {
    let edited =
        (item.edited && !item.header).then(|| edited_mark().text_xs().text_color(rgb(palette.muted_foreground)));
    if edited.is_none() && mark.is_none() && item.reactions.is_empty() {
        return None;
    }
    let reactions = item
        .reactions
        .iter()
        .map(|reaction| reaction_pill(item, reaction, reactors.measure(&item.id, &reaction.key), palette, cx))
        .collect::<Vec<_>>();
    let add = (!item.reactions.is_empty() && item.actions().contains(&MessageAction::React)).then(|| {
        let message = item.id.clone();
        pill(ElementId::Name(format!("reactions-add-{}", item.id).into()), palette)
            .debug_selector(|| "reactions-add".to_owned())
            .border_color(rgb(palette.border))
            .tooltip(|window, cx| Tooltip::new("Add reaction").build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.act(MessageAction::React, &message, window, cx);
                this.open_more(window, cx);
            }))
            .child(
                Glyph::REACT
                    .icon()
                    .size(px(CHIP_EMOJI - 2.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
    });
    let line = div().flex().gap_1().items_center().px(px(BUBBLE_INSET));
    // over the bubble's bottom edge, drawn after it
    let line = if item.reactions.is_empty() {
        line.pt_1()
    } else {
        line.mt(px(-REACTIONS_OVERLAP))
    };
    Some(
        line.children(edited)
            .children(mark)
            .children(reactions)
            .children(add)
            .into_any_element(),
    )
}

/// A reaction's pill.
fn reaction_pill(
    item: &MessageItem,
    reaction: &ReactionCount,
    measure: Option<AnyElement>,
    palette: Palette,
    cx: &mut Context<ChatView>,
) -> gpui::Stateful<gpui::Div> {
    let key = reaction.key.clone();
    let (message, toggled) = (item.id.clone(), reaction.key.clone());
    let (hovered_message, hovered_key) = (item.id.clone(), reaction.key.clone());
    let chip = pill(ElementId::Name(format!("reaction-{}-{key}", item.id).into()), palette)
        .debug_selector(move || format!("reaction-{key}"))
        .relative()
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            this.hover_pill(&hovered_message, &hovered_key, *hovered, cx);
        }))
        .children(measure)
        .child(reactions::emoji(&reaction.key, px(CHIP_EMOJI)))
        .children((reaction.count() > 1).then(|| reaction.count().to_string()));
    // a chip of a reaction Teams offers sets or takes back the account's own; any other key only shows
    let chip = if reactions::catalog().offers(&reaction.key) {
        chip.on_click(cx.listener(move |this, _, _, cx| this.toggle_reaction(&message, &toggled, cx)))
    } else {
        chip
    };
    chip.border_color(rgb(if reaction.mine { palette.brand } else { palette.border }))
}

/// A pill under a message: a reaction's, or "Add reaction".
fn pill(id: ElementId, palette: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .h(px(PILL))
        .min_w(px(PILL + 8.0))
        .px_2()
        .rounded_full()
        .border_1()
        .bg(rgb(palette.surface))
        .text_sm()
        .cursor_pointer()
        .hover(move |style| style.bg(rgb(palette.hover)))
}

impl ChatView {
    /// "More reactions" over the view, under the quick reactions as they were last drawn: out of the list's key
    /// context, whose keys would act on what its search field is given. It closes with the quick reactions.
    fn more_overlay(&mut self) -> Option<impl IntoElement + use<>> {
        if self.reacting.is_none() {
            self.more = None;
            self.quick_bar.set(None);
        }
        let ((picker, _), bar) = self.more.as_ref().zip(self.quick_bar.get())?;
        Some(
            deferred(
                anchored()
                    .position(bar.bottom_left())
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().pt_1().child(picker.clone())),
            )
            .with_priority(1),
        )
    }
}

/// The hover bar's keys on the message list: the quick reactions (1–4) and every reaction (5) once react opened
/// them, and copy text on the selected message.
fn hover_bar_keys(list: gpui::Div, cx: &mut Context<ChatView>) -> gpui::Div {
    list.on_action(cx.listener(|this, _: &PickFirst, _, cx| this.pick(0, cx)))
        .on_action(cx.listener(|this, _: &PickSecond, _, cx| this.pick(1, cx)))
        .on_action(cx.listener(|this, _: &PickThird, _, cx| this.pick(2, cx)))
        .on_action(cx.listener(|this, _: &PickFourth, _, cx| this.pick(3, cx)))
        .on_action(cx.listener(|this, _: &PickMore, window, cx| this.open_more(window, cx)))
        .on_action(cx.listener(|this, _: &CopySelected, window, cx| {
            // the text selected with the mouse first, as Teams copies it; else the message selected with the keyboard
            if !this.copy_selection(cx) {
                this.act_on_selected(MessageAction::Copy, window, cx);
            }
        }))
}

impl ChatView {
    /// "Jump to newest" over the list's foot, while it is scrolled up.
    fn jump_pill(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        (self.scrolled_up() && !self.items.is_empty()).then(|| {
            div()
                .debug_selector(|| "jump-to-newest".to_owned())
                .absolute()
                .bottom(px(12.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    Button::new("jump-to-newest")
                        .icon(Glyph::ARROW_DOWN.icon())
                        .label("Jump to newest")
                        .primary()
                        .rounded(ButtonRounded::Large)
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.jump_to_newest(cx))),
                )
        })
    }

    /// Who is typing, under the messages, while anyone is.
    fn typing_line(&self, palette: Palette, cx: &App) -> Option<gpui::Div> {
        self.typing_text(cx).map(|text| {
            div()
                .debug_selector(|| "typing".to_owned())
                .px_4()
                .h(px(20.0))
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .italic()
                .text_color(rgb(palette.muted_foreground))
                .child(typing_dots(palette, cx))
                .child(text)
        })
    }
}

/// Three dots in the brand colours beside "… is typing", bouncing one after another while animations play.
fn typing_dots(palette: Palette, cx: &App) -> gpui::Div {
    use gpui::AnimationExt as _;
    let moving = crate::motion::on(cx);
    let dots = [
        palette.brand,
        theme::mix(palette.brand, palette.accent, 0.5),
        palette.accent,
    ]
    .into_iter()
    .enumerate()
    .map(move |(at, color)| {
        let dot = div().size(px(5.0)).rounded_full().bg(rgb(color));
        if !moving {
            return dot.into_any_element();
        }
        #[expect(clippy::cast_precision_loss, reason = "a dot's place, 0 to 2")]
        let delay = at as f32 * 0.16;
        dot.with_animation(
            SharedString::from(format!("typing-dot-{at}")),
            gpui::Animation::new(std::time::Duration::from_millis(1100)).repeat(),
            move |dot, t| {
                let lift = ((t - delay).rem_euclid(1.0) * std::f32::consts::TAU).sin().max(0.0);
                dot.opacity(0.4 + 0.6 * lift).mb(px(4.0 * lift))
            },
        )
        .into_any_element()
    });
    div().h(px(12.0)).flex().items_end().gap(px(3.0)).children(dots)
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.drawn.clear();
        #[cfg(feature = "test-support")]
        self.faces_placed.borrow_mut().clear();
        let more = self.more_overlay();
        let options = self.options_overlay(palette, window, cx);
        let selection_menu = self.selection_menu(palette, window, cx);
        let reactors = self.reactors_card(palette, cx);
        let rows = list(
            self.list.clone(),
            cx.processor(|this: &mut Self, index: usize, _window, cx| this.render_item(index, cx)),
        )
        .size_full()
        .pb_3();
        self.centre(cx);
        let pill = self.jump_pill(cx);
        let typing = self.typing_line(palette, cx);
        let messages = hover_bar_keys(div(), cx)
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            // a click in the messages leaves the focus where it is (the compose box, as in Teams): the list takes it
            // only from the keyboard (Escape in the box) and from the actions that need its keys
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step_selection(1, cx)))
            .on_action(cx.listener(|this, _: &ReplyToSelected, window, cx| {
                this.act_on_selected(MessageAction::Reply, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ReactToSelected, window, cx| {
                this.act_on_selected(MessageAction::React, window, cx);
            }))
            .on_action(cx.listener(|this, _: &EditSelected, window, cx| {
                this.act_on_selected(MessageAction::Edit, window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                this.act_on_selected(MessageAction::Delete, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ClearSelection, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &ConfirmDelete, _, cx| this.confirm_delete(cx)))
            .debug_selector(|| "chat-messages".to_owned())
            .relative()
            .flex_1()
            .min_h_0()
            .child(rows)
            .children(pill);
        Self::file_drop_target(cx)
            // Escape the compose box handed on (nothing to cancel there): the list takes the focus
            .on_action(cx.listener(|this, _: &ComposeEscape, window, cx| this.focus_messages(window, cx)))
            // a press anywhere in the view lets the selected text go (a press on text then starts a new selection;
            // Shift+click extends it), and brings the focus into the view when it was elsewhere, so Ctrl+C reaches it
            .capture_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                if event.button != MouseButton::Left {
                    return;
                }
                if !this.focus.is_focused(window) && !this.compose.read(cx).focused(window, cx) {
                    this.focus.focus(window, cx);
                }
                if !event.modifiers.shift && this.text_selection.clear() {
                    cx.notify();
                }
            }))
            // Ctrl+C in the compose box copies the messages' selected text while there is any, as a browser copies
            // the page's one selection
            .capture_action(cx.listener(|this, _: &ComposeCopy, _, cx| {
                if this.copy_selection(cx) {
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            // a faint wash of the accent toward the compose box
            .bg(theme::gradient(
                palette.background,
                theme::mix(palette.background, palette.accent, 0.06),
                180.0,
            ))
            .child(messages)
            .children(typing)
            .children(ChatTranslation::notice(&self.translation, palette, cx))
            .children(self.confirmation(palette, cx))
            .child(self.compose.clone())
            .children(more)
            .children(reactors)
            .children(options)
            .children(selection_menu)
            .children((self.file_drop == FileDrop::Hovering).then(|| file_drop_overlay(palette)))
    }
}

/// Teams covers the conversation while a local file hovers it, leaving a clear drop target.
fn file_drop_overlay(palette: Palette) -> impl IntoElement {
    div()
        .debug_selector(|| "chat-file-drop-highlight".to_owned())
        .absolute()
        .inset_0()
        .p_1()
        .bg(rgb(palette.background).opacity(0.94))
        .child(
            div()
                .size_full()
                .border_2()
                .border_color(rgb(palette.brand).opacity(0.4))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .text_color(rgb(palette.foreground))
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Share in this chat"),
                ),
        )
}
