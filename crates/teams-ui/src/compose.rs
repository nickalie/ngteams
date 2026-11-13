//! The compose box under a chat's messages (M2 Task 13): what the user types becomes a message's text, and every
//! write goes out through the engine ([`Core::write`]), its outbox; the box builds no request.
//!
//! - **Text**: the box is our own rich-text editor ([`Editor`], WYSIWYG), and what it holds is sent as Teams' editor
//!   sends a message (`docs/protocol/chat.md`, "Formatted text": `RichText/Html`, its paragraphs, marks, colours,
//!   lists, quotes, headings, links and blocks of code as the web client writes them; `crate::editor::convert`).
//!   Blank lines at either end aren't sent. A URL typed stays text, as the capture's link inserted with its text went
//!   out (`lines.json`'s `unlinked_content`); a link is one the format bar's "Insert link" made. A mention is an atom of
//!   the editor naming its MRI, completed after `@` from the chat's members ([`Core::members`], their names from
//!   [`Core::person`]) and only from them: a mention is never read from typed text, only by the MRI the picker put in
//!   it; the picker shows each member's presence dot (M2 Task 17: the engine is asked about the people offered each
//!   time they are shown). In a group chat it offers Everyone after the members ([`offered`]), as the web client does:
//!   it names the chat's id, and the message mentions everyone in the chat (`docs/protocol/chat.md`, "A mention of
//!   everyone"). `teams-proto`'s [`MessageText`] renders the HTML, the mentions and the reply's quote from that one
//!   value. A send the engine refuses before it makes a row gives the box its draft back.
//! - **Keys** (the editor's, `crate::editor`): Enter sends and Shift+Enter starts a new paragraph, as Teams' box
//!   first shows; Ctrl+Shift+X (or the Format button) expands the box with its format bar, as Teams' does, and then
//!   Enter starts a new paragraph, Shift+Enter breaks the line and Ctrl+Enter sends. With the mention picker open, Up
//!   and Down move in it, Enter or Tab chooses, Escape closes it; Escape cancels a reply or an edit, and with nothing
//!   to cancel is handed on (the chat view moves the focus to its message list); Up in an empty box asks to edit the
//!   account's last message ([`ComposeEvent::EditLast`], which the chat view carries out).
//! - **Reply** ([`Compose::reply_to`]): the quoted message's id, sender, sender's name, time and a one-line preview of
//!   its text, sent as the captured reply's quote (`reply.json`). **Edit** ([`Compose::edit`]): the message's rich
//!   text loaded back into the editor ([`load`]) with its formats, its mentions as atoms again by their MRI, its quote
//!   kept as it was; Enter (Ctrl+Enter expanded) sends the edit.
//! - **Typing** ([`Core::transient`], never a stored row): sent while the box has the focus and holds text, at most once
//!   per [`TYPING_EVERY`] per chat (the web client's throttle, M2 Task 10; kept for the app, so a chat opened again
//!   doesn't start it over), never while editing, and only with the "send typing" setting on. Nothing is sent when
//!   typing stops: the web client never sent `Control/ClearTyping`.
//!
//! - **The box** is drawn as Teams' compose box: the editor on a raised, rounded surface with a hairline border
//!   ([`BOX_HEIGHT`] at least), Teams' accent line in the brand colour along its foot while it has the focus, and its
//!   controls inside it on the right: Format, Emoji, Attach, Loop and Actions and apps (the last two shown disabled,
//!   saying the app doesn't have them yet), Correct, Translate and its language, a thin divider, then Send. Its
//!   placeholder stays on one line, cut short with an ellipsis; a narrow box (a thread's pane) shows the language as
//!   a caret alone, naming it in its tooltip. Expanded, the format bar ([`crate::editor::toolbar::FormatBar`]) runs
//!   along its top, the editor is taller, and the controls sit along its foot; Discard on the bar empties the box (and
//!   ends a reply or an edit).
//! - **Emoji** ([`Compose::toggle_emoji`], the button or Ctrl+.): every emoji Teams offers ([`ReactionPicker::for_emoji`],
//!   searchable) over the box; the one picked goes in at the cursor, in place of the text selected, as if typed. It
//!   is only text in the box: nothing is sent until the message is.
//!
//! - **A channel's box** ([`Compose::in_channel`]): "Message in <channel>" posting to the channel, or a post's
//!   thread's "Reply in thread" posting to the post's chain, with Teams' "Send to: thread only" over its text. It
//!   offers Format, Emoji, Correct, Translate and Send, no more: it sends no typing (no capture showed a channel's)
//!   and takes no file (a channel's files go to its team's site, which this app doesn't upload to).
//! - **New message's box** ([`Compose::for_new_chat`]): a chat's box for a chat not made yet. It sends no typing and
//!   takes no pasted picture; its first message is handed to New message ([`ComposeEvent::Start`]), which makes the
//!   chat and sends it there, and comes back to the box if that fails ([`Compose::started`]).
//! - **Read-only** ([`Compose::is_read_only`]): in the app's read-only mode every box ([`READ_ONLY_APP`]), else the
//!   account's own notes ([`teams_core::NOTES`]), which take no write from
//!   this app (no write type is mapped to a `48:` stream), so their box shows [`READ_ONLY_NOTES`] in place of the text
//!   area and its send: nothing can be typed, sent, replied to or edited there, and no typing goes out.
//!
//! - **Correct** ([`correction`], the button or Ctrl+Shift+G): the draft's spelling, grammar and style checked by the AI
//!   tool the settings name, the proposal shown over the box word by word; nothing changes or goes out until the user
//!   takes it.
//! - **Translate** ([`draft_translation`], the button or Ctrl+Shift+Y): the draft translated by the AI tool the settings
//!   name into the language every box shares (the button beside it), in place, as one edit Ctrl+Z takes back.
//!
//! What the box does is traced (`compose opened` when the box is made, `sent`, `replying`, `editing`, `cancelled`, and
//! `changed` each time what it holds changes), never its text.

pub mod attachments;
mod correction;
mod draft_translation;
mod pictures;
mod translated;

use std::cell::Cell;
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Bounds, Context, Entity, EventEmitter, Global, InteractiveElement as _,
    IntoElement, KeyBinding, ParentElement as _, PathPromptOptions, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, actions, anchored, deferred, div,
    prelude::FluentBuilder as _, px, relative, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Selectable as _, Sizable as _, Size};
use teams_core::{Action, Availability, NOTES, Transient};
use teams_proto::chat::{ComposeError, Marks, MessageText, MessageType, Node, Quote, RichText, parse_html};
use teams_proto::is_group_chat_id;
use teams_store::Message;

pub use correction::{CorrectDraft, CorrectionStatus, KeepOriginal, SendCorrection, UseCorrection};
pub use draft_translation::{DraftTranslationStatus, OpenTranslationSettings, TranslateDraft};

pub use crate::editor::Editor;
use crate::editor::convert::{self, CantLoad};
use crate::editor::doc::{Block, Doc, Kind};
use crate::editor::toolbar::{FormatBar, FormatBarEvent, unavailable};
use crate::editor::{self, EditorEvent, EnterMode};

use crate::bridge::Core;
use crate::chat_view::measured;
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::presence::{self, Presences};
use crate::reaction_picker::{ReactionPicker, ReactionPickerEvent};
use crate::reactions;
use crate::rich_text;
use crate::settings::SettingsModel;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The compose box's key context.
pub const CONTEXT: &str = "Compose";
/// The box's least height, px: Teams' compose box, one line of text with its toolbar beside it.
pub const BOX_HEIGHT: f32 = 56.0;
/// The box's padding above and below its text area and toolbar, px.
const BOX_PADDING: f32 = 6.0;
/// A toolbar button in the box, px (its icon three quarters of it).
const TOOL: f32 = 28.0;
/// Below this width, px, the box's toolbar is compacted (the translation language an icon alone), as in a thread's pane.
const NARROW_BOX: f32 = 560.0;
/// The editor's least height expanded, px: Teams' expanded box's writing area.
const EXPANDED_MIN: f32 = 96.0;
/// The editor's most height expanded, px, before it scrolls.
const EXPANDED_MAX: f32 = 320.0;
/// The editor's most height in the box as first shown, px: [`MAX_ROWS`] lines.
const COLLAPSED_MAX: f32 = 8.0 * 22.0;

actions!(
    compose,
    [
        /// Opens or closes the emoji picker.
        ToggleEmoji,
        /// Chooses local files to attach to the message.
        AttachFiles
    ]
);

/// Marks the box's key bindings as installed.
struct Bound;

impl Global for Bound {}

/// The emoji picker's key context, over the box: its key closes it from inside it.
const EMOJI_CONTEXT: &str = "ComposeEmoji";

/// Binds the box's own keys, once per app: Ctrl+. (and Ctrl+;, as GTK's text fields) opens the emoji picker from the
/// box and closes it from the picker.
pub fn init(cx: &mut App) {
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    // the last bound is the one a tooltip names
    cx.bind_keys([CONTEXT, EMOJI_CONTEXT].into_iter().flat_map(|context| {
        [
            KeyBinding::new("ctrl-;", ToggleEmoji, Some(context)),
            KeyBinding::new("ctrl-.", ToggleEmoji, Some(context)),
        ]
    }));
    cx.bind_keys([KeyBinding::new("ctrl-shift-u", AttachFiles, Some(CONTEXT))]);
    correction::bind_keys(cx);
    draft_translation::bind_keys(cx);
}

/// The shortest gap between two typing signals: the web client's throttle (`typingIndicator.notificationFrequency`,
/// 6 000 ms; M2 Task 10, `docs/protocol/chat.md`, "Typing").
pub const TYPING_EVERY: Duration = Duration::from_secs(6);
/// The most people the mention picker lists.
pub const PICKER_MAX: usize = 8;
/// How far back from the cursor an `@` still opens the picker, in characters.
pub const MENTION_QUERY_MAX: usize = 40;
/// The longest preview a reply's quote carries, in characters.
pub const PREVIEW_MAX: usize = 300;
/// The most rows the box grows to.
pub const MAX_ROWS: usize = 8;

/// The message `doc` makes, after `quotes` (a reply's, or the quote an edited message keeps); `None` when it shows
/// nothing.
///
/// # Errors
/// What `teams-proto` refuses in a message's text ([`ComposeError`]): a mention of anyone but a work account or
/// everyone in a group chat, a text too long, …
pub fn message_of(doc: &Doc, quotes: &[Node]) -> Result<Option<MessageText>, ComposeError> {
    message_of_with(doc, quotes, &|_| None)
}

/// [`message_of`], each picture in `doc` as `pictures` gives it by its id (the editor's, once sent:
/// [`Editor::picture_node`]).
///
/// # Errors
/// As [`message_of`].
pub fn message_of_with(
    doc: &Doc,
    quotes: &[Node],
    pictures: &dyn Fn(u64) -> Option<teams_proto::chat::Image>,
) -> Result<Option<MessageText>, ComposeError> {
    let (nodes, mentioned) = convert::to_nodes_with(doc, pictures);
    if nodes.is_empty() {
        return Ok(None);
    }
    let mut all = quotes.to_vec();
    all.extend(nodes);
    MessageText::new(
        RichText {
            nodes: all,
            truncated: false,
        },
        mentioned,
    )
    .map(Some)
}

/// Plain text as the editor's document: each line a paragraph (`\n` or `\r\n` ending it), as typed with Shift+Enter
/// between them. Nothing in it is read as a format or a mention.
#[must_use]
pub fn plain_doc(text: &str) -> Doc {
    Doc::new(text.lines().map(Block::paragraph).collect())
}

/// A message's top-level quotes of another message (a reply's, a forward's): what an edit keeps as it was and sends
/// again before the edited text.
#[must_use]
pub fn quotes_of(text: &RichText) -> Vec<Node> {
    text.nodes.iter().filter(|node| is_kept_quote(node)).cloned().collect()
}

fn is_kept_quote(node: &Node) -> bool {
    matches!(node, Node::Quote(quote) if quote.message_id.is_some() || quote.forward)
}

/// A text run with no marks.
fn text_node(text: &str) -> Node {
    Node::Text {
        text: text.to_owned(),
        marks: Marks::NONE,
    }
}

/// The quote a reply carries: the quoted message, its sender and the name shown for them, its time and a preview of
/// its text, as the captured reply quoted (`docs/protocol/chat.md`, "Quoted reply").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quoted {
    pub message_id: String,
    pub sender: String,
    pub sender_name: String,
    /// The quoted message's time, ms: its id, as the captured reply named it.
    pub time: String,
    /// Its text on one line, at most [`PREVIEW_MAX`] characters.
    pub preview: String,
    /// In a translated chat, its text as the chat shows it (its translation), on one line: what the box shows over
    /// it, and the account's own reply keeps as written. The quote sent keeps `preview`, as the others read it.
    pub shown: Option<String>,
}

impl Quoted {
    /// The quote of `message`, its sender shown as `sender_name`; `None` for a message without a sender.
    #[must_use]
    pub fn of(message: &Message, sender_name: &str) -> Option<Self> {
        let sender = message.sender.clone()?;
        let numeric = !message.id.is_empty() && message.id.bytes().all(|byte| byte.is_ascii_digit());
        let time = if numeric {
            message.id.clone()
        } else {
            message.arrival_time.to_string()
        };
        Some(Self {
            message_id: message.id.clone(),
            sender,
            sender_name: sender_name.to_owned(),
            time,
            preview: preview(message),
            shown: None,
        })
    }

    /// The quote, shown in a translated chat as `text` (the quoted message's translation).
    #[must_use]
    pub fn shown_as(self, text: &RichText) -> Self {
        Self {
            shown: Some(one_line_of(text.clone())),
            ..self
        }
    }

    /// The quote as the box shows it: in the chat's language when the chat is translated.
    #[must_use]
    pub fn shown_preview(&self) -> &str {
        self.shown.as_deref().unwrap_or(&self.preview)
    }

    /// The account's reply as written (`text`, its quote as sent): its quote as the chat shows it.
    #[must_use]
    pub fn as_shown_in(&self, text: &RichText) -> RichText {
        let Some(shown) = &self.shown else {
            return text.clone();
        };
        let nodes = text
            .nodes
            .iter()
            .map(|node| match node {
                Node::Quote(quote) if quote.message_id.as_deref() == Some(self.message_id.as_str()) => {
                    Node::Quote(Quote {
                        body: vec![text_node(shown)],
                        ..quote.clone()
                    })
                }
                other => other.clone(),
            })
            .collect();
        RichText {
            nodes,
            truncated: text.truncated,
        }
    }

    /// The quote as rich text.
    #[must_use]
    pub fn node(&self) -> Node {
        Node::Quote(Quote {
            message_id: Some(self.message_id.clone()),
            sender: Some(self.sender.clone()),
            sender_name: Some(self.sender_name.clone()),
            time: Some(self.time.clone()),
            body: vec![text_node(&self.preview)],
            forward: false,
        })
    }
}

/// A message's content as rich text.
#[must_use]
pub fn rich_text_of(message: &Message) -> RichText {
    if message.message_type == MessageType::RichTextHtml.as_str() {
        parse_html(&message.content)
    } else {
        RichText::plain(&message.content)
    }
}

/// A message's text on one line (what it quotes left out; a forward's content is the message's own, kept), at most
/// [`PREVIEW_MAX`] characters.
fn preview(message: &Message) -> String {
    one_line_of(rich_text_of(message))
}

/// A text on one line (what it quotes left out; a forward's content kept), at most [`PREVIEW_MAX`] characters.
fn one_line_of(text: RichText) -> String {
    let nodes = text
        .nodes
        .into_iter()
        .filter(|node| !matches!(node, Node::Quote(quote) if !quote.forward))
        .collect();
    let plain = RichText {
        nodes,
        truncated: false,
    }
    .plain_text();
    let line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= PREVIEW_MAX {
        return line;
    }
    let mut cut: String = line.chars().take(PREVIEW_MAX).collect();
    cut.push('…');
    cut
}

/// A message's text loaded back into the box for an edit ([`load`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    /// What the editor holds: the message's text with its formats, its mentions as atoms by their MRI.
    pub doc: Doc,
    /// Its quotes (a reply's, a forward's), kept as they were and sent again before the edited text.
    pub quotes: Vec<Node>,
}

/// A message's rich text as the box edits it: its top-level quotes of another message (a reply's, a forward's) kept
/// aside, the rest loaded into the editor with its formats and its mentions (those `mentioned` names by `itemid`)
/// as atoms again.
///
/// # Errors
/// [`CantLoad`]: the text shows a picture (the box sends none), has formatting the box would send back changed (a
/// block of code in a language, a numbered list from another number), or was read only in part.
pub fn load<S: BuildHasher>(text: &RichText, mentioned: &HashMap<String, String, S>) -> Result<Loaded, CantLoad> {
    if text.truncated {
        return Err(CantLoad::Truncated);
    }
    let (quotes, rest): (Vec<Node>, Vec<Node>) = text.nodes.iter().cloned().partition(is_kept_quote);
    Ok(Loaded {
        doc: convert::from_nodes(&rest, mentioned, convert::Load::Edit)?,
        quotes,
    })
}

/// The mention being typed before `cursor`: the `@`'s byte offset and the query after it. An `@` opens one when it
/// starts a word (at the text's start or after whitespace), on the cursor's line, at most [`MENTION_QUERY_MAX`]
/// characters back, outside a mention and with no mention between it and the cursor.
#[must_use]
pub fn mention_query<'a>(text: &'a str, cursor: usize, mentions: &[Range<usize>]) -> Option<(usize, &'a str)> {
    let before = text.get(..cursor)?;
    let (at, _) = before
        .char_indices()
        .rev()
        .take(MENTION_QUERY_MAX + 1)
        .take_while(|(_, character)| *character != '\n')
        .find(|(_, character)| *character == '@')?;
    let starts_word = before[..at].chars().next_back().is_none_or(char::is_whitespace);
    let query = &before[at + 1..];
    let crossed = mentions
        .iter()
        .any(|mention| mention.end > at && mention.start < cursor);
    (starts_word && !crossed && !query.starts_with(char::is_whitespace)).then_some((at, query))
}

/// Someone the mention picker offers: a member of the chat, by MRI, with the name shown; or everyone in a group chat
/// ([`Candidate::everyone`]), by the chat's id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub mri: String,
    pub name: SharedString,
}

/// What the picker's Everyone says under its name, as the web client's does (`compose_mention_everyone_tag_label`).
pub const EVERYONE_LABEL: &str = "Notify everyone in the chat";

/// The most members a chat may have for the picker to offer Everyone: the web client offers it in no chat of more
/// than 1,000 (`compose_mention_everyone_disabled_large_roster_tooltip`).
pub const EVERYONE_MAX_MEMBERS: usize = 1000;

impl Candidate {
    /// Everyone in the group chat `conversation`: a mention of its id, shown as [`rich_text::EVERYONE`].
    #[must_use]
    pub fn everyone(conversation: &str) -> Self {
        Self {
            mri: conversation.to_owned(),
            name: rich_text::EVERYONE.into(),
        }
    }

    /// Whether it is everyone in a chat rather than a person.
    #[must_use]
    pub fn is_everyone(&self) -> bool {
        is_group_chat_id(&self.mri)
    }
}

/// The candidates whose name holds `query` (any case), at most [`PICKER_MAX`], in their order.
#[must_use]
pub fn matching(candidates: &[Candidate], query: &str) -> Vec<Candidate> {
    let query = query.to_lowercase();
    candidates
        .iter()
        .filter(|candidate| candidate.name.to_lowercase().contains(&query))
        .take(PICKER_MAX)
        .cloned()
        .collect()
}

/// What the picker offers in `conversation` for `query`, its `members` (the account's own left out) read: the members
/// [`matching`] it, then, in a group chat of at most [`EVERYONE_MAX_MEMBERS`], Everyone when its name holds the query
/// (any case), as the web client's picker lists them (`abc-everyone`, `docs/protocol/chat.md`).
#[must_use]
pub fn offered(conversation: &str, members: &[Candidate], query: &str) -> Vec<Candidate> {
    let everyone = (is_group_chat_id(conversation) && members.len() < EVERYONE_MAX_MEMBERS)
        .then(|| Candidate::everyone(conversation))
        .filter(|everyone| everyone.name.to_lowercase().contains(&query.to_lowercase()));
    matching(members, query).into_iter().chain(everyone).collect()
}

/// When typing may be sent again: at most once per [`TYPING_EVERY`], the first at once (the web client's leading-edge
/// throttle).
#[derive(Clone, Copy, Debug, Default)]
pub struct TypingThrottle {
    last: Option<Instant>,
}

impl TypingThrottle {
    /// Whether a typing signal may go out at `now`; if so, `now` is the last one's time.
    pub fn take(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < TYPING_EVERY)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// The typing throttles of the app's chats, by conversation: a global, so a chat view built again (another chat opened
/// and this one again) doesn't start its chat's throttle over.
#[derive(Default)]
struct TypingSent(HashMap<String, TypingThrottle>);

impl Global for TypingSent {}

/// What the box is writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A new message.
    New,
    /// A reply, with its quote.
    Reply(Quoted),
    /// An edit of the account's message `message_id`, which keeps `quotes`.
    Edit { message_id: String, quotes: Vec<Node> },
}

/// The mention picker while it is open: where the `@` is, the cursor it was read at, who matches, who is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    pub at: usize,
    pub end: usize,
    pub matches: Vec<Candidate>,
    pub selected: usize,
    /// The chat's members are being read: nobody to offer yet.
    pub loading: bool,
}

/// What a read-only box says in place of its text area: the account's own notes take no write from this app.
pub const READ_ONLY_NOTES: &str = "Notes can't be written from this app yet";

/// What Loop says: Loop components aren't in this app.
pub const NO_LOOP: &str = "Loop components aren't available in this app";

/// What Actions and apps says: apps aren't in this app.
pub const NO_APPS: &str = "Actions and apps aren't available in this app";

/// What every box says in the app's read-only mode ([`crate::Core::read_only`]): nothing is sent from it.
pub const READ_ONLY_APP: &str = "Read-only: this window sends nothing";
/// What the box says when a reply or an edit is started while the message sent is translated.
pub const TRANSLATING_FIRST: &str = "Wait for your message to be translated and sent first.";

/// What a channel's box says while it is empty ([`Compose::in_channel`]): "Message in <channel>", as Teams' does.
#[must_use]
pub fn channel_placeholder(channel: &str) -> String {
    if channel.is_empty() {
        "Message in this channel".to_owned()
    } else {
        format!("Message in {channel}")
    }
}

/// What a thread's box says while it is empty: Teams' words.
pub const THREAD_PLACEHOLDER: &str = "Reply in thread";

/// Where a thread's reply goes, over its box: Teams' words. A reply goes only to its thread here (Teams' "Send to"
/// also offers the channel, a write no capture showed).
pub const SEND_TO_THREAD: &str = "thread only";

/// Where a box writes: a chat, a team's channel (its posts), or one post's thread in a channel (its replies, written
/// to the post's chain, `teams_proto::reply_chain_id`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Writes {
    #[default]
    Chat,
    Channel,
    Thread,
    /// New message's box, before its chat is made ([`Compose::for_new_chat`]): its first message is handed to the
    /// view that makes the chat ([`ComposeEvent::Start`]), never written by the box.
    NewChat,
}

/// What the box tells its chat view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeEvent {
    /// Up in an empty box: the account's last message is to be edited.
    EditLast,
    /// A message went to the outbox (a post, a reply or an edit).
    Sent,
    /// New message's first message ([`Writes::NewChat`]): the chat is to be made, and the message sent to it. The box
    /// keeps it until [`Compose::started`] says how that went.
    Start(Box<MessageText>),
}

fn empty_file_text() -> Result<MessageText, ComposeError> {
    MessageText::new(
        RichText {
            nodes: Vec::new(),
            truncated: false,
        },
        [],
    )
}

/// What a send took from the box: given back when the engine refuses the write. A send held for its translation
/// writes with its own (the box may start an edit or a reply meanwhile).
struct Draft {
    doc: Doc,
    mode: Mode,
    attachments: Vec<attachments::Attachment>,
}

/// What a compose box is made of.
#[derive(Clone)]
pub struct ComposeParts {
    pub core: Arc<dyn Core>,
    pub settings: Entity<SettingsModel>,
    pub trace: Arc<ViewTrace>,
    /// Others' presence as the engine told it, for the mention picker's dots.
    pub presence: Entity<Presences>,
}

/// The compose box (see the module docs).
pub struct Compose {
    /// The lit Send button's own focus (it is a solid face of ours, not the kit's button, so it keeps its own).
    send_focus: gpui::FocusHandle,
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    presence: Entity<Presences>,
    conversation: String,
    editor: Entity<Editor>,
    format_bar: Entity<FormatBar>,
    /// The box expanded with its format bar (Ctrl+Shift+X, the Format button).
    expanded: bool,
    mode: Mode,
    attachments: Vec<attachments::Attachment>,
    next_attachment: u64,
    /// Kept across navigation until the engine has queued or refused the file send.
    sending_files: usize,
    picker: Option<Picker>,
    /// The chat's members the picker offers, once read.
    candidates: Option<Vec<Candidate>>,
    reading: Option<Task<()>>,
    /// What the last send couldn't do, shown over the box.
    problem: Option<SharedString>,
    /// A translated chat's sends ([`translated`]).
    translated: translated::Translated,
    /// The account's own notes: nothing is written there ([`READ_ONLY_NOTES`]).
    /// What a read-only box says in place of its text area; `None` for a box that writes.
    read_only: Option<&'static str>,
    /// Where it writes ([`Compose::in_channel`]): a channel's box sends no typing and takes no file.
    writes: Writes,
    /// New message's first message while its chat is being made ([`ComposeEvent::Start`]): given back if that fails.
    starting: Option<Draft>,
    /// The AI's correction of the draft ([`correction`]).
    correction: correction::Correction,
    /// The AI's translation of the draft, in place ([`draft_translation`]).
    draft_translation: draft_translation::DraftTranslation,
    /// The emoji picker, while it is open.
    emoji: Option<(Entity<ReactionPicker>, Subscription)>,
    /// Where the emoji button was last drawn, for the picker to open by.
    emoji_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Where the box was last drawn: a narrow one compacts its toolbar.
    box_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Whether the picker was open as the emoji button was pressed: its click then closes it.
    emoji_on_press: bool,
    _editor_events: Subscription,
    _bar_events: Subscription,
    _presence: Subscription,
}

impl EventEmitter<ComposeEvent> for Compose {}

/// The chat's members the picker may offer: work accounts but the account, by name.
async fn read_candidates(core: Arc<dyn Core>, conversation: String) -> Vec<Candidate> {
    let members = core.members(conversation).await.unwrap_or_default();
    let me = core.me().to_owned();
    let mut found = Vec::new();
    for member in members {
        if member.mri == me || !member.mri.starts_with("8:orgid:") {
            continue;
        }
        let name = core
            .person(member.mri.clone())
            .await
            .ok()
            .and_then(|person| person.name);
        found.push(Candidate {
            name: name.unwrap_or_else(|| member.mri.clone()).into(),
            mri: member.mri,
        });
    }
    found.sort_by(|one, other| one.name.cmp(&other.name));
    found
}

impl Compose {
    /// The box of `conversation`; traced as made (`compose opened`), so a live test can tell which conversations a
    /// view made one for.
    pub fn new(parts: ComposeParts, conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        parts
            .trace
            .record(&TraceLine::new("compose", "opened").conversation(&conversation));
        let editor = cx.new(|cx| Editor::new(window, cx).placeholder("Type a message"));
        editor.update(cx, |editor, cx| editor.set_heights(None, Some(px(COLLAPSED_MAX)), cx));
        let editor_events = cx.subscribe_in(&editor, window, |this: &mut Self, _, event, window, cx| match event {
            // the box's accent line follows the focus
            EditorEvent::Focus | EditorEvent::Blur => cx.notify(),
            // moving the cursor changes no text: an `@` the cursor left closes the picker
            EditorEvent::Moved => this.revalidate_picker(cx),
            EditorEvent::Changed => {
                // what the box holds changed: traced without the text, so a test driving the keyboard can tell
                // when the app has taken in what it typed
                this.trace
                    .record(&TraceLine::new("compose", "changed").conversation(&this.conversation));
                this.correction_follows_draft(cx);
                this.draft_translation_follows_draft(cx);
                this.refresh_picker(cx);
                this.fit_pictures(cx);
                this.typed(window, cx);
                // the send button follows whether there is anything to send
                cx.notify();
            }
            EditorEvent::Submit => this.send(window, cx),
            EditorEvent::Picture(id) => this.send_picture(*id, cx),
            EditorEvent::Files(paths) => this.pasted_files(paths, window, cx),
        });
        let format_bar = cx.new(|cx| FormatBar::new(editor.clone(), cx));
        let bar_events = cx.subscribe_in(
            &format_bar,
            window,
            |this: &mut Self, _, event, window, cx| match event {
                FormatBarEvent::Discard => this.discard(window, cx),
            },
        );
        // a presence the engine told: an open picker draws its dots again
        let presence = cx.observe(&parts.presence, |this: &mut Self, _, cx| {
            if this.picker.is_some() {
                cx.notify();
            }
        });
        let note = if parts.core.read_only() {
            Some(READ_ONLY_APP)
        } else {
            (conversation == NOTES).then_some(READ_ONLY_NOTES)
        };
        // a picture pasted goes to the media service for this conversation, so a box that sends takes them
        editor.update(cx, |editor, _| editor.set_takes_pictures(note.is_none()));
        let correction = correction::Correction::new(cx);
        Self {
            send_focus: cx.focus_handle(),
            core: parts.core,
            settings: parts.settings,
            trace: parts.trace,
            presence: parts.presence,
            read_only: note,
            conversation,
            editor,
            format_bar,
            expanded: false,
            mode: Mode::New,
            attachments: Vec::new(),
            next_attachment: 0,
            sending_files: 0,
            picker: None,
            candidates: None,
            reading: None,
            problem: None,
            translated: translated::Translated::default(),
            writes: Writes::Chat,
            starting: None,
            correction,
            draft_translation: draft_translation::DraftTranslation::new(),
            emoji: None,
            emoji_at: Rc::default(),
            box_at: Rc::default(),
            emoji_on_press: false,
            _editor_events: editor_events,
            _bar_events: bar_events,
            _presence: presence,
        }
    }

    /// The box made a channel's (`writes`: [`Writes::Channel`], its posts) or a post's thread's ([`Writes::Thread`],
    /// its replies), saying `placeholder` while it is empty. It sends no typing (no capture showed a channel's) and
    /// offers no file: a channel's files go to its team's site, which this app doesn't upload to.
    pub fn in_channel(&mut self, writes: Writes, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.writes = writes;
        let placeholder = placeholder.into();
        self.editor
            .update(cx, |editor, cx| editor.set_placeholder(placeholder, cx));
        cx.notify();
    }

    /// The box made New message's ([`Writes::NewChat`]): it offers what a chat's box offers, but sends no typing (there
    /// is no chat to type in yet) and takes no pasted picture (a picture goes to the media service for its chat); its
    /// first message is handed on ([`ComposeEvent::Start`]), never written by the box.
    pub fn for_new_chat(&mut self, cx: &mut Context<Self>) {
        self.writes = Writes::NewChat;
        self.editor.update(cx, |editor, _| editor.set_takes_pictures(false));
        cx.notify();
    }

    /// Whether the box writes to a chat (made or still to be made), not a channel's posts or a thread.
    fn writes_a_chat(&self) -> bool {
        matches!(self.writes, Writes::Chat | Writes::NewChat)
    }

    /// Whether New message's first message is out of the box while its chat is being made.
    #[must_use]
    pub fn is_starting(&self) -> bool {
        self.starting.is_some()
    }

    /// How making New message's chat went: on `Err`, the first message comes back to the box (unless something was
    /// typed in it since) with what went wrong over it; else it is let go.
    pub fn started(&mut self, outcome: Result<(), SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.starting.take() else {
            return;
        };
        if let Err(problem) = outcome {
            self.give_back(draft, window, cx);
            self.show_problem(problem, cx);
        }
        cx.notify();
    }

    /// The doc the box holds, and what it is a draft of, moved to `other` (New message's box and an existing chat's
    /// as the To: line finds that chat or leaves it): what was typed stays typed. Its pasted pictures stay behind: each
    /// was uploaded for this box's chat alone (only its members may read it), and `other`'s editor holds no picture of
    /// that id, so a block of one moved there would show nothing and hold every send back as not yet uploaded.
    pub fn hand_text_to(&mut self, other: &Entity<Self>, cx: &mut Context<Self>) {
        if self.editor.read(cx).is_empty() {
            return;
        }
        let mut doc = self.editor.read(cx).doc().clone();
        doc.blocks.retain(|block| !matches!(block.kind, Kind::Picture(_)));
        self.editor.update(cx, |editor, cx| editor.set_doc(Doc::default(), cx));
        if doc
            .blocks
            .iter()
            .all(|block| block.kind == Kind::Paragraph && block.is_empty())
        {
            cx.notify();
            return;
        }
        other.update(cx, |other, cx| {
            other.editor.update(cx, |editor, cx| editor.set_doc(doc, cx));
            cx.notify();
        });
        cx.notify();
    }

    /// A chat keeps its editor alive while a local draft or upload remains, including across chat navigation.
    pub(crate) fn has_draft(&self, cx: &App) -> bool {
        !self.editor.read(cx).is_empty()
            || self.mode != Mode::New
            || !self.attachments.is_empty()
            || self.sending_files > 0
    }

    /// The rich-text editor the box writes with.
    #[must_use]
    pub fn editor(&self) -> &Entity<Editor> {
        &self.editor
    }

    /// The format bar shown while the box is expanded.
    #[must_use]
    pub fn format_bar(&self) -> &Entity<FormatBar> {
        &self.format_bar
    }

    /// Whether the box is expanded with its format bar.
    #[must_use]
    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Expands the box with its format bar, or collapses it (Ctrl+Shift+X, the Format button): expanded, Enter
    /// starts a new paragraph and Ctrl+Enter sends. The editor keeps the focus.
    pub fn toggle_expanded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only.is_some() {
            return;
        }
        self.set_expanded(!self.expanded, cx);
        self.focus(window, cx);
    }

    fn set_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.expanded = expanded;
        let (mode, min, max) = if expanded {
            (EnterMode::NewLine, Some(px(EXPANDED_MIN)), px(EXPANDED_MAX))
        } else {
            (EnterMode::Send, None, px(COLLAPSED_MAX))
        };
        self.editor.update(cx, |editor, cx| {
            editor.set_enter_mode(mode, cx);
            editor.set_heights(min, Some(max), cx);
        });
        self.trace.record(
            &TraceLine::new("compose", if expanded { "expanded" } else { "collapsed" })
                .conversation(&self.conversation),
        );
        cx.notify();
    }

    /// Discard: empties the box and ends a reply or an edit.
    pub fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attachments.clear();
        self.editor.update(cx, super::editor::Editor::clear);
        if self.mode != Mode::New {
            self.cancel(window, cx);
        }
        self.focus(window, cx);
        cx.notify();
    }

    /// The Attach button and its keyboard shortcut use the same staging path as an external drop.
    pub fn choose_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepts_files() {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = this.update_in(cx, |this, window, cx| this.attach_files(&paths, window, cx));
            }
        })
        .detach();
    }

    #[must_use]
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// The mention picker, while it is open.
    #[must_use]
    pub fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// The presence the open picker shows beside each person it offers, in its order (Everyone has none); empty while
    /// it is closed.
    #[must_use]
    pub fn picker_presence(&self, cx: &App) -> Vec<(String, Availability)> {
        let presence = self.presence.read(cx);
        self.picker
            .iter()
            .flat_map(|picker| &picker.matches)
            .filter(|candidate| !candidate.is_everyone())
            .map(|candidate| (candidate.mri.clone(), presence.of(&candidate.mri)))
            .collect()
    }

    /// What the last send couldn't do.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// The text typed, a line per block.
    #[must_use]
    pub fn value(&self, cx: &App) -> SharedString {
        self.editor.read(cx).text().into()
    }

    /// Whether the box takes no text: the account's own notes ([`READ_ONLY_NOTES`]).
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only.is_some()
    }

    /// Gives the box the focus; a read-only one has nothing to take it.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only.is_some() {
            return;
        }
        let focus = self.editor.read(cx).focus.clone();
        window.focus(&focus, cx);
    }

    /// Whether the box has the focus.
    #[must_use]
    pub fn focused(&self, window: &Window, cx: &App) -> bool {
        self.editor.read(cx).is_focused(window)
    }

    /// Shows `problem` over the box.
    pub fn show_problem(&mut self, problem: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.problem = Some(problem.into());
        cx.notify();
    }

    /// Starts a reply quoting `quoted`; the text typed stays.
    pub fn reply_to(&mut self, quoted: Quoted, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending_files > 0 {
            return self.show_problem("Wait for your files to finish sending before starting a reply.", cx);
        }
        // the message being translated stays in the box as it was sent, to go or, failing, to be sent again as it was
        if self.translating() {
            return self.show_problem(TRANSLATING_FIRST, cx);
        }
        if self.read_only.is_some() {
            return;
        }
        self.trace.record(
            &TraceLine::new("compose", "replying")
                .conversation(&self.conversation)
                .messages([quoted.message_id.as_str()]),
        );
        self.mode = Mode::Reply(quoted);
        self.problem = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// Starts editing the account's message `message_id`: its text loaded into the box.
    pub fn edit(&mut self, message_id: String, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only.is_some() {
            return;
        }
        if self.sending_files > 0 {
            return self.show_problem("Wait for your files to finish sending before editing a message.", cx);
        }
        if self.translating() {
            return self.show_problem(TRANSLATING_FIRST, cx);
        }
        if !self.attachments.is_empty() {
            return self.show_problem("Send or remove your attachments before editing a message.", cx);
        }
        self.trace.record(
            &TraceLine::new("compose", "editing")
                .conversation(&self.conversation)
                .messages([message_id.as_str()]),
        );
        self.editor.update(cx, |editor, cx| editor.set_doc(loaded.doc, cx));
        self.mode = Mode::Edit {
            message_id,
            quotes: loaded.quotes,
        };
        self.picker = None;
        self.problem = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// Cancels a reply (its text stays) or an edit (its text goes).
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::New {
            return;
        }
        if matches!(self.mode, Mode::Edit { .. }) {
            self.editor.update(cx, |editor, cx| editor.set_doc(Doc::default(), cx));
        }
        // the reply or the edit being translated is called off with it: nothing of it is sent
        self.drop_translation_of(&self.mode.clone(), cx);
        let _ = window;
        self.mode = Mode::New;
        self.picker = None;
        self.trace
            .record(&TraceLine::new("compose", "cancelled").conversation(&self.conversation));
        cx.notify();
    }

    /// Puts a mention of `mri`, shown as `name`, at the cursor (the picker's choice puts it over the `@` typed).
    pub fn insert_mention(&mut self, mri: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (_, cursor, _) = self.editor.read(cx).cursor_block();
        self.place_mention(cursor..cursor, mri, name, window, cx);
    }

    /// Puts a mention in place of `range` of the cursor's block, a space after it.
    fn place_mention(&mut self, range: Range<usize>, mri: &str, name: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        if name.trim().is_empty() || name.chars().any(char::is_control) {
            self.problem = Some("That mention can't be added: it has no name to show".into());
            return cx.notify();
        }
        self.editor
            .update(cx, |editor, cx| editor.insert_mention(range, mri, name, cx));
        cx.notify();
    }

    /// Sends what the box holds (see the module docs); the box is emptied once the write is asked for, and given its
    /// draft back (its text, its mentions, the reply or the edit) when the engine refuses it before it makes a row,
    /// unless something was typed in it since.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only.is_some() || self.translating() || self.starting.is_some() {
            return;
        }
        if !self.files_ready() {
            return self.show_problem("Wait for your files to upload, or retry or remove a failed file.", cx);
        }
        if !self.editor.read(cx).pictures_ready() {
            return self.show_problem("Wait for your pictures to upload, or retry or remove a failed one.", cx);
        }
        let doc = self.editor.read(cx).doc().clone();
        let quotes = match &self.mode {
            Mode::New => Vec::new(),
            Mode::Reply(quoted) => vec![quoted.node()],
            Mode::Edit { quotes, .. } => quotes.clone(),
        };
        let editor = self.editor.read(cx);
        let built = match message_of_with(&doc, &quotes, &|id| editor.picture_node(id)) {
            Ok(Some(built)) => built,
            Ok(None) if !self.attachments.is_empty() => match empty_file_text() {
                Ok(text) => text,
                Err(error) => return self.show_problem(format!("This message can't be sent: {error}"), cx),
            },
            Ok(None) => return,
            Err(error) => return self.show_problem(format!("This message can't be sent: {error}"), cx),
        };
        let built = built.with_files(
            self.attachments
                .iter()
                .filter_map(|file| match &file.state {
                    attachments::AttachmentState::Ready(uploaded) => Some(uploaded.clone()),
                    _ => None,
                })
                .collect(),
        );
        let from = Draft {
            doc,
            mode: self.mode.clone(),
            attachments: self.attachments.clone(),
        };
        // what is sent is what the box holds: a correction or translation of it is over
        self.forget_correction();
        self.forget_draft_translation();
        // a translated chat's message is translated before it goes to the outbox
        let Some((built, from)) = self.translate_first(built, from, window, cx) else {
            return;
        };
        self.write_built(
            translated::Sending {
                text: built,
                original: None,
            },
            from,
            false,
            window,
            cx,
        );
    }

    /// Writes `sending` through the outbox as `from`, what the send took from the box, says (a post, a reply, an
    /// edit), and empties the box; a send held for its translation (`held`) leaves the box as it is when anything in it
    /// changed meanwhile (what was typed, files added), taking out only what it sent.
    fn write_built(
        &mut self,
        sending: translated::Sending,
        from: Draft,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let translated::Sending { text: built, original } = sending;
        let paths = |attachments: &[attachments::Attachment]| -> Vec<std::path::PathBuf> {
            attachments.iter().map(|file| file.path.clone()).collect()
        };
        let keep_box = held
            && (*self.editor.read(cx).doc() != from.doc
                || self.mode != from.mode
                || paths(&self.attachments) != paths(&from.attachments));
        if self.writes == Writes::NewChat {
            // the chat is made first, by the view that holds the box; the message waits here until it is
            self.starting = Some(from);
            self.trace.record(&TraceLine::new("compose", "sent").kind("new chat"));
            self.clear(window, cx);
            cx.emit(ComposeEvent::Start(Box::new(built)));
            return;
        }
        let sent = original.as_ref().map(|_| built.rich_text().clone());
        let conversation = self.conversation.clone();
        let edit_of = match &from.mode {
            Mode::Edit { message_id, .. } => Some(message_id.clone()),
            _ => None,
        };
        let (action, kind) = match &from.mode {
            Mode::Edit { message_id, .. } => (
                Action::Edit {
                    // a thread's reply is edited where it is stored, its channel, never the post's chain
                    conversation: teams_proto::stored_conversation(&conversation).to_owned(),
                    message_id: message_id.clone(),
                    text: built,
                },
                "edit",
            ),
            Mode::Reply(_) => (
                Action::Send {
                    conversation,
                    text: built,
                },
                "reply",
            ),
            Mode::New => (
                Action::Send {
                    conversation,
                    text: built,
                },
                "message",
            ),
        };
        if let (Some(original), Some(sent)) = (&original, &sent) {
            // the account's reply keeps its quote as the chat showed it
            let original = match &from.mode {
                Mode::Reply(quoted) => quoted.as_shown_in(original),
                _ => original.clone(),
            };
            self.keep_original(&original, sent, edit_of.as_deref(), cx);
        }
        let reply = self.core.write(action);
        self.trace.record(
            &TraceLine::new("compose", "sent")
                .conversation(&self.conversation)
                .kind(kind),
        );
        cx.emit(ComposeEvent::Sent);
        if keep_box {
            // what was sent leaves the box; what changed in it since stays
            if *self.editor.read(cx).doc() == from.doc {
                self.editor.update(cx, |editor, cx| editor.set_doc(Doc::default(), cx));
            }
            if self.mode == from.mode {
                self.mode = Mode::New;
            }
            let sent_files = paths(&from.attachments);
            self.attachments.retain(|file| !sent_files.contains(&file.path));
            cx.notify();
        } else {
            self.clear(window, cx);
        }
        // a post's original is kept by the row it went out as
        let post = sent.filter(|_| edit_of.is_none());
        self.await_send(reply, from, post, window, cx);
    }

    fn await_send(
        &mut self,
        reply: teams_core::Reply<teams_store::OutboxEntry>,
        draft: Draft,
        post: Option<RichText>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files = !draft.attachments.is_empty();
        self.sending_files += usize::from(files);
        cx.spawn_in(window, async move |this, cx| {
            let answer = reply.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending_files = this.sending_files.saturating_sub(usize::from(files));
                match answer {
                    Err(error) => {
                        this.give_back(draft, window, cx);
                        this.show_problem(format!("Not sent: {error}"), cx);
                    }
                    Ok(row) => this.sent_post(row.client_message_id, post.as_ref(), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Restores a refused file send, retaining any newer text typed while its share was pending.
    fn give_back(&mut self, mut draft: Draft, _: &mut Window, cx: &mut Context<Self>) {
        // files attached since stay beside the text given back (a translated send's text left the box once it went),
        // but never beside an edit, which carries no files
        let edit_beside_files = matches!(draft.mode, Mode::Edit { .. }) && !self.attachments.is_empty();
        if draft.attachments.is_empty()
            && (!self.editor.read(cx).is_empty() || self.mode != Mode::New || edit_beside_files)
        {
            return;
        }
        if !self.editor.read(cx).is_empty() {
            draft.doc.blocks.push(crate::editor::doc::Block::default());
            draft.doc.blocks.extend(self.editor.read(cx).doc().blocks.clone());
        }
        self.editor.update(cx, |editor, cx| editor.set_doc(draft.doc, cx));
        self.mode = draft.mode;
        for file in draft.attachments {
            if !self.attachments.iter().any(|current| current.path == file.path) {
                self.attachments.push(file);
            }
        }
        cx.notify();
    }

    /// Empties the box, ends a reply or an edit, and collapses an expanded box, as Teams' does once it sent.
    fn clear(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.forget_correction();
        self.forget_draft_translation();
        self.editor.update(cx, |editor, cx| editor.set_doc(Doc::default(), cx));
        self.forget_untranslated();
        self.mode = Mode::New;
        self.attachments.clear();
        self.picker = None;
        self.problem = None;
        if self.expanded {
            self.set_expanded(false, cx);
        }
        cx.notify();
    }

    /// Typing goes out (see the module docs) when it may.
    fn typed(&mut self, window: &Window, cx: &mut Context<Self>) {
        let wanted = self.settings.read(cx).sends_typing()
            && self.writes == Writes::Chat
            && !matches!(self.mode, Mode::Edit { .. })
            && self.focused(window, cx)
            && self.holds_text(cx);
        if !wanted {
            return;
        }
        let now = cx.background_executor().now();
        let may = cx
            .default_global::<TypingSent>()
            .0
            .entry(self.conversation.clone())
            .or_default()
            .take(now);
        if may {
            // nothing of it is kept: how it went is the engine's (a dropped typing is never sent again)
            drop(self.core.transient(Transient::Typing {
                conversation: self.conversation.clone(),
            }));
        }
    }

    fn holds_text(&self, cx: &App) -> bool {
        !self.editor.read(cx).is_blank()
    }

    // --- the mention picker ---

    /// Reads the picker again from the text and the cursor as they stand, before a key it would take: moving the
    /// cursor changes no text, so nothing else tells the picker (an `@` the cursor left closes it).
    fn revalidate_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.refresh_picker(cx);
        }
    }

    /// Opens, moves or closes the picker after the text changed, or before a key it would take.
    fn refresh_picker(&mut self, cx: &mut Context<Self>) {
        let (text, cursor, tokens) = self.editor.read(cx).cursor_block();
        let Some((at, query)) = mention_query(&text, cursor, &tokens) else {
            return self.close_picker(cx);
        };
        self.read_candidates(cx);
        let loading = self.candidates.is_none();
        let matches = self
            .candidates
            .as_deref()
            .map(|candidates| offered(&self.conversation, candidates, query))
            .unwrap_or_default();
        if !loading && matches.is_empty() {
            return self.close_picker(cx);
        }
        // the people offered, at most `PICKER_MAX`: asked about each time they are shown (the chat list's budget,
        // `LIST_WATCHED`, leaves them room in the engine's watch)
        presence::watch(
            self.core.as_ref(),
            matches
                .iter()
                .filter(|candidate| !candidate.is_everyone())
                .map(|candidate| &candidate.mri),
        );
        let selected = self
            .picker
            .as_ref()
            .map_or(0, |picker| picker.selected)
            .min(matches.len().saturating_sub(1));
        self.picker = Some(Picker {
            at,
            end: cursor,
            matches,
            selected,
            loading,
        });
        cx.notify();
    }

    /// Reads the chat's members for the picker, once.
    fn read_candidates(&mut self, cx: &mut Context<Self>) {
        if self.candidates.is_some() || self.reading.is_some() {
            return;
        }
        let read = read_candidates(Arc::clone(&self.core), self.conversation.clone());
        self.reading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let found = read.await;
            let _ = this.update(cx, |this, cx| {
                this.candidates = Some(found);
                this.reading = None;
                this.refresh_picker(cx);
            });
        }));
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            cx.notify();
        }
    }

    /// Whether the picker offers anyone.
    fn offering(&self) -> bool {
        self.picker.as_ref().is_some_and(|picker| !picker.matches.is_empty())
    }

    fn step_picker(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.picker {
            let last = picker.matches.len().saturating_sub(1);
            picker.selected = picker.selected.saturating_add_signed(step).min(last);
            cx.notify();
        }
    }

    /// Puts the picker's `index`th match in place of the `@` and the query typed.
    pub fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        let Some(picker) = self.picker.clone() else { return };
        let Some(candidate) = picker.matches.get(index) else {
            return;
        };
        self.place_mention(picker.at..picker.end, &candidate.mri, &candidate.name, window, cx);
    }

    fn choose_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.picker.as_ref().map_or(0, |picker| picker.selected);
        self.choose(selected, window, cx);
    }

    // --- the emoji picker ---

    /// The emoji picker, while it is open.
    #[must_use]
    pub fn emoji_picker(&self) -> Option<&Entity<ReactionPicker>> {
        self.emoji.as_ref().map(|(picker, _)| picker)
    }

    /// Opens the emoji picker over the box, its search field focused, or closes it when it is open; a read-only box
    /// has none.
    pub fn toggle_emoji(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only.is_some() {
            return;
        }
        if self.emoji.take().is_some() {
            self.focus(window, cx);
            cx.notify();
            return;
        }
        self.picker = None;
        let picker = cx.new(|cx| ReactionPicker::for_emoji(Arc::clone(&self.core), window, cx));
        let events = cx.subscribe_in(&picker, window, |this, picker, event, window, cx| match event {
            ReactionPickerEvent::Picked(key) => this.put_emoji(key, window, cx),
            ReactionPickerEvent::Dismissed => {
                // Escape gives the box its focus back; a click elsewhere keeps the focus where it went
                let held = picker.read(cx).holds_focus(window, cx);
                this.emoji = None;
                if held {
                    this.focus(window, cx);
                }
                cx.notify();
            }
        });
        self.emoji = Some((picker, events));
        cx.notify();
    }

    /// The emoji button clicked: the picker opens, or closes when it was open as the button was pressed (the press,
    /// outside the picker, has dismissed it by then).
    fn click_emoji(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.emoji_on_press) {
            self.emoji = None;
            self.focus(window, cx);
            cx.notify();
        } else {
            self.toggle_emoji(window, cx);
        }
    }

    /// Puts the emoji of `key` at the cursor, in place of the text selected, as if typed, and closes the picker.
    fn put_emoji(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji = None;
        self.focus(window, cx);
        let emoji = reactions::glyph(key);
        self.editor.update(cx, |editor, cx| editor.insert(&emoji, cx));
        cx.notify();
    }

    // --- keys ---

    fn capture_up(&mut self, _: &editor::MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.picker.is_some() {
            self.step_picker(-1, cx);
            cx.stop_propagation();
        } else if self.mode == Mode::New && self.editor.read(cx).is_empty() && self.attachments.is_empty() {
            cx.emit(ComposeEvent::EditLast);
            cx.stop_propagation();
        }
    }

    fn capture_down(&mut self, _: &editor::MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.picker.is_some() {
            self.step_picker(1, cx);
            cx.stop_propagation();
        }
    }

    fn capture_enter(&mut self, _: &editor::Enter, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.offering() {
            self.choose_selected(window, cx);
            cx.stop_propagation();
        }
    }

    fn capture_tab(&mut self, _: &editor::Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.offering() {
            self.choose_selected(window, cx);
            cx.stop_propagation();
        }
    }

    fn on_escape(&mut self, _: &editor::Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.picker.is_some() {
            self.close_picker(cx);
        } else if self.correction_on_show() {
            self.end_correction(window, cx);
        } else if self.draft_translation_on_show() {
            self.cancel_draft_translation(window, cx);
        } else if self.mode == Mode::New {
            cx.propagate();
        } else {
            self.cancel(window, cx);
        }
    }

    // --- drawing ---

    fn mode_bar(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, detail) = match &self.mode {
            Mode::New => return None,
            Mode::Reply(quoted) => (
                format!("Replying to {}", quoted.sender_name),
                quoted.shown_preview().to_owned(),
            ),
            Mode::Edit { .. } => ("Editing your message".to_owned(), String::new()),
        };
        let tip = if matches!(self.mode, Mode::Edit { .. }) {
            "Cancel editing"
        } else {
            "Cancel reply"
        };
        let cancel = icon_button("compose-cancel", Glyph::X, tip)
            .with_size(Size::XSmall)
            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx)));
        Some(
            div()
                .debug_selector(|| "compose-mode".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .border_l_2()
                .border_color(rgb(palette.brand))
                .text_xs()
                .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_color(rgb(palette.muted_foreground))
                        .child(detail),
                )
                .child(div().debug_selector(|| "compose-cancel".to_owned()).child(cancel))
                .into_any_element(),
        )
    }

    /// Send (Enter does the same; Ctrl+Enter expanded): the kit's icon button, greyed, while the box holds nothing to
    /// send; once it holds something, a round button lit in the brand gradient that pops in, a button to the keyboard
    /// and to assistive tools as the kit's is.
    fn send_button(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let empty = (self.editor.read(cx).is_blank() && self.attachments.is_empty())
            || !self.files_ready()
            || !self.editor.read(cx).pictures_ready();
        let tip = match (matches!(self.mode, Mode::Edit { .. }), self.expanded) {
            (true, false) => "Save (Enter)",
            (true, true) => "Save (Ctrl+Enter)",
            (false, false) => "Send (Enter)",
            (false, true) => "Send (Ctrl+Enter)",
        };
        if empty {
            return selected_by(
                "compose-send",
                icon_button("compose-send", Glyph::SEND, tip)
                    .with_size(Size::Size(px(TOOL)))
                    .disabled(true),
            )
            .into_any_element();
        }
        // ready to go: lit in the brand gradient, a solid face of our own (the kit's coloured buttons are only tinted),
        // popping in as the first character is typed
        let button = div()
            .debug_selector(|| "compose-send".to_owned())
            .id("compose-send")
            .size(px(TOOL))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .bg(theme::brand_gradient(palette, 135.0))
            .shadow(theme::glow(palette, palette.brand, 0.4, 10.0, 2.0))
            .text_color(rgb(palette.on_brand))
            .hover(move |button| button.bg(theme::gradient(palette.brand_hover, palette.accent, 135.0)))
            .active(move |button| button.bg(rgb(palette.brand_active)))
            // what the kit's button gives its own: a button's role and name, a stop on Tab (Enter or Space on it
            // clicks it), a ring while it has the keyboard
            .role(gpui::accesskit::Role::Button)
            .aria_label(tip)
            .track_focus(&self.send_focus)
            .tab_stop(true)
            .focus(move |button| button.border_2().border_color(rgb(palette.on_brand)))
            .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
            .child(Glyph::SEND.icon().size(px(15.0)));
        crate::motion::toggle(button, "compose-send-ready", true, cx, |button, t| {
            button.opacity(t.clamp(0.0, 1.0)).mt(px(6.0 * (1.0 - t)))
        })
    }

    /// The emoji button, its tooltip naming its key; the picker opens over the box by it.
    fn emoji_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let button = icon_button_with_key("compose-emoji", Glyph::EMOJI, "Emoji", &ToggleEmoji)
            .with_size(Size::Size(px(TOOL)))
            .selected(self.emoji.is_some())
            .on_click(cx.listener(|this, _, window, cx| this.click_emoji(window, cx)));
        selected_by("compose-emoji", button)
            .relative()
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.emoji_on_press = this.emoji.is_some()))
            .child(measured(&self.emoji_at))
            .into_any_element()
    }

    /// Format, as Teams' box has it: expands the box with its format bar, highlighted while it is expanded.
    fn format_button(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let tip = if self.expanded {
            "Hide formatting options"
        } else {
            "Show formatting options"
        };
        let glyph = Glyph::TEXT_FORMAT
            .icon()
            .when(self.expanded, |icon| icon.text_color(rgb(palette.brand)));
        let button = icon_button_with_key("compose-format", Glyph::TEXT_FORMAT, tip, &editor::ToggleExpanded)
            .icon(glyph)
            .with_size(Size::Size(px(TOOL)))
            .selected(self.expanded)
            .on_click(cx.listener(|this, _, window, cx| this.toggle_expanded(window, cx)));
        selected_by("compose-format", button).into_any_element()
    }

    /// The box's controls, inside it on the right as Teams has them: Format, Emoji, Attach, Loop and Actions and
    /// apps (the last two not in this app yet: shown disabled, saying so), then this app's Correct, Translate and its
    /// language, a thin divider, then Send.
    fn toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .debug_selector(|| "compose-toolbar".to_owned())
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            // the box's inside, its padding and border taken off: centred on one line, at the foot of more
            .h(px(BOX_HEIGHT - 2.0 * BOX_PADDING - 2.0))
            .child(self.format_button(palette, cx))
            .child(self.emoji_button(cx))
            .children(self.writes_a_chat().then(|| {
                selected_by(
                    "compose-attach",
                    icon_button_with_key("compose-attach", Glyph::PAPERCLIP, "Attach files", &AttachFiles)
                        .with_size(Size::Size(px(TOOL)))
                        .disabled(!self.accepts_files())
                        .on_click(cx.listener(|this, _, window, cx| this.choose_files(window, cx))),
                )
            }))
            // a channel's box offers only what works there
            .children(
                self.writes_a_chat()
                    .then(|| unavailable("compose-loop", Glyph::REPEAT, NO_LOOP)),
            )
            .children(
                self.writes_a_chat()
                    .then(|| unavailable("compose-apps", Glyph::PLUS, NO_APPS)),
            )
            .child(self.correct_button(cx))
            .child(self.translate_buttons(self.is_narrow(), cx))
            .child(div().w(px(1.0)).h(px(20.0)).mx_1().bg(rgb(palette.border)))
            .child(self.send_button(palette, cx))
            .into_any_element()
    }

    /// The emoji picker over the box, by the emoji button as it was last drawn, opening upwards: out of the box's key
    /// context, whose keys (Enter sends, Up edits) would act on what its search field is given.
    /// Its own key context takes the picker's key back from inside it, so the key closes what it opened.
    fn emoji_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ((picker, _), at) = self.emoji.as_ref().zip(self.emoji_at.get())?;
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::BottomRight)
                    .position(at.top_right())
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .key_context(EMOJI_CONTEXT)
                            .on_action(cx.listener(|this, _: &ToggleEmoji, window, cx| this.toggle_emoji(window, cx)))
                            .pb_1()
                            .child(picker.clone()),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Whether the box was last drawn narrower than [`NARROW_BOX`], its toolbar compacted.
    #[must_use]
    pub fn is_narrow(&self) -> bool {
        self.box_at
            .get()
            .is_some_and(|bounds| bounds.size.width < px(NARROW_BOX))
    }

    /// The box itself: the editor and its controls on a raised, rounded surface with a hairline border, and Teams'
    /// accent line along its foot while it has the focus; expanded, the format bar along its top and the controls
    /// along its foot.
    fn input_box(&self, palette: Palette, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let accent = focused.then(|| {
            div()
                .debug_selector(|| "compose-accent".to_owned())
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(2.0))
                .rounded_b_md()
                .bg(theme::brand_gradient(palette, 90.0))
        });
        // the accent line grows out from the middle as the box takes the focus
        let accent = accent.map(|accent| {
            crate::motion::toggle(accent, "compose-accent-grow", true, cx, |accent, t| {
                let inset = relative((1.0 - t.clamp(0.0, 1.0)) / 2.0);
                accent.left(inset).right(inset)
            })
        });
        let surface = div()
            .debug_selector(|| "compose-box".to_owned())
            .relative()
            .min_h(px(BOX_HEIGHT))
            .rounded_lg()
            .border_1()
            .border_color(rgb(if focused {
                theme::mix(palette.border, palette.brand, 0.45)
            } else {
                palette.border
            }))
            .bg(rgb(palette.surface))
            .shadow(if focused {
                theme::glow(palette, palette.brand, 0.22, 18.0, 4.0)
            } else {
                theme::glow(palette, 0x0000_0000, 0.06, 8.0, 2.0)
            })
            .text_color(rgb(palette.foreground));
        // an edit stays where its reply is: nothing is sent anywhere new
        let send_to =
            (self.writes == Writes::Thread && !matches!(self.mode, Mode::Edit { .. })).then(|| send_to_line(palette));
        if self.expanded || !self.attachments.is_empty() {
            return surface
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .pt_2()
                .pb(px(BOX_PADDING))
                .children(send_to)
                .children(self.expanded.then(|| self.format_bar.clone()))
                .child(div().px_1().pt_1().child(self.editor.clone()))
                .children(self.attachment_cards(palette, cx))
                .child(div().flex().justify_end().child(self.toolbar(palette, cx)))
                .child(measured(&self.box_at))
                .children(accent)
                .into_any_element();
        }
        let line = div()
            .flex()
            .items_end()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // one line sits in the middle of the box; more lines grow it, the toolbar staying at its foot
                    .self_center()
                    .py_1()
                    .child(self.editor.clone()),
            )
            .child(self.toolbar(palette, cx));
        surface
            .flex()
            .flex_col()
            .pl_3()
            .pr_2()
            .py(px(BOX_PADDING))
            .overflow_hidden()
            .children(send_to.map(gpui::Styled::pt_1))
            .child(line)
            .child(measured(&self.box_at))
            .children(accent)
            .into_any_element()
    }

    fn picker_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let presence = self.presence.read(cx);
        let rows = picker.matches.iter().enumerate().map(|(index, candidate)| {
            let chosen = index == picker.selected;
            let row = div()
                .id(("mention", index))
                .debug_selector(move || format!("mention-{index}"))
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .when_chosen(chosen, palette)
                .on_click(cx.listener(move |this, _, window, cx| this.choose(index, window, cx)));
            if candidate.is_everyone() {
                row.child(everyone_row(&candidate.name, palette))
            } else {
                // the dot in a slot as wide as Everyone's icon, so every name starts in one column
                row.child(
                    div()
                        .flex_none()
                        .w(px(EVERYONE_ICON))
                        .flex()
                        .justify_center()
                        .child(presence::dot(&presence.of(&candidate.mri), palette)),
                )
                .child(candidate.name.clone())
            }
        });
        let loading = picker
            .loading
            .then(|| div().px_2().text_color(rgb(palette.muted_foreground)).child("…"));
        Some(
            div()
                .debug_selector(|| "mention-picker".to_owned())
                .flex()
                .flex_col()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_sm()
                .children(rows)
                .children(loading)
                .into_any_element(),
        )
    }

    fn problem_view(&self, palette: Palette) -> Option<AnyElement> {
        let problem = self.problem.clone()?;
        Some(
            div()
                .debug_selector(|| "compose-problem".to_owned())
                .text_xs()
                .italic()
                .text_color(rgb(palette.foreground))
                .child(problem)
                .into_any_element(),
        )
    }
}

/// Where a thread's reply goes, over its text, as Teams' box says it: "Send to:" and the thread, in the brand colour.
fn send_to_line(palette: Palette) -> gpui::Div {
    div()
        .debug_selector(|| "compose-send-to".to_owned())
        .flex()
        .gap_1()
        .text_sm()
        .text_color(rgb(palette.brand))
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Send to:"))
        .child(SEND_TO_THREAD)
}

/// The picker's Everyone, as the web client's: an outlined people icon where a person's picture would be, the name,
/// and under it what it does ([`EVERYONE_LABEL`]).
fn everyone_row(name: &SharedString, palette: Palette) -> impl IntoElement {
    div()
        .debug_selector(|| "mention-everyone".to_owned())
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_none()
                .size(px(EVERYONE_ICON))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(palette.foreground))
                .child(Glyph::USERS.icon().size(px(18.0))),
        )
        .child(
            div().flex().flex_col().child(name.clone()).child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(EVERYONE_LABEL),
            ),
        )
}

/// The circle the picker's Everyone shows its icon in, px.
const EVERYONE_ICON: f32 = 28.0;

/// A picker row's look when it is the one chosen.
trait Chosen {
    fn when_chosen(self, chosen: bool, palette: Palette) -> Self;
}

impl Chosen for gpui::Stateful<gpui::Div> {
    fn when_chosen(self, chosen: bool, palette: Palette) -> Self {
        if chosen { self.bg(rgb(palette.selected)) } else { self }
    }
}

/// A read-only box: `note` ([`READ_ONLY_NOTES`], [`READ_ONLY_APP`]) where the text area would be, greyed, with nothing
/// to type in or send.
fn read_only_view(palette: Palette, note: &'static str) -> AnyElement {
    div()
        .debug_selector(|| "compose".to_owned())
        .flex_none()
        .px_4()
        .pt_1()
        .pb_4()
        .child(
            div()
                .debug_selector(|| "compose-read-only".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .min_h(px(BOX_HEIGHT))
                .px_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(Glyph::INFO.icon().size(px(14.0)))
                .child(note),
        )
        .into_any_element()
}

impl Render for Compose {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        if let Some(note) = self.read_only {
            return read_only_view(palette, note);
        }
        self.correction_follows_draft(cx);
        self.draft_translation_follows_draft(cx);
        self.prepare_language_menu(window, cx);
        let focused = self.focused(window, cx);
        let keyed = div()
            .key_context(CONTEXT)
            .capture_action(cx.listener(Self::capture_up))
            .capture_action(cx.listener(Self::capture_down))
            .capture_action(cx.listener(Self::capture_enter))
            .capture_action(cx.listener(Self::capture_tab))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(|this, _: &editor::ToggleExpanded, window, cx| this.toggle_expanded(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleEmoji, window, cx| this.toggle_emoji(window, cx)))
            .on_action(cx.listener(|this, _: &AttachFiles, window, cx| this.choose_files(window, cx)))
            .on_action(cx.listener(|this, _: &CorrectDraft, window, cx| this.correct_draft(window, cx)))
            .on_action(cx.listener(|this, _: &TranslateDraft, window, cx| this.translate_draft(window, cx)))
            .flex()
            .flex_col()
            .gap_1()
            .children(self.picker_view(palette, cx))
            .children(self.mode_bar(palette, cx))
            .children(self.translation_view(palette, cx))
            .children(self.problem_view(palette))
            .children(self.correction_view(palette, cx))
            .children(self.draft_translation_view(palette, cx))
            .child(self.input_box(palette, focused, cx));
        div()
            .debug_selector(|| "compose".to_owned())
            .flex_none()
            .px_4()
            .pt_1()
            .pb_4()
            .child(keyed)
            .children(self.emoji_overlay(cx))
            .into_any_element()
    }
}
