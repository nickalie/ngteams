//! The chat list: the account's chats by last activity, each row with its avatar, name, last message's first line,
//! time and unread count, in the [`crate::sidebar`]'s "Chats" section, which draws the rows its [`ChatWindow`] shows
//! and See more under them; the account's favourite chats above them, in the "Favorites" section, in the order Teams
//! keeps them, and not again under "Chats".
//!
//! - **The order** is Teams' own for its recent chats, `MostRecent`: the latest message first. A chat with something
//!   unread comes first because its message is new; one unread for long stays in its place, and past the rows shown
//!   it makes See more bold, as in Teams (`docs/protocol/chat.md`, "The chat list's order and its See more").
//!
//! - **From the store only.** The list asks the engine's store ([`Core::conversations`], then per chat its members,
//!   newest messages, unread count and people's names) when it opens and again each time the models say a chat
//!   changed ([`crate::Chats::revision`]): a delivery, a catch-up, a read horizon, an outbox row. A row is built again
//!   only when its chat changed since it was built, or when a message that carried a name came, in any chat, from
//!   someone the row is named by, or their profile changed ([`crate::Chats::named_at`]); every row again after a
//!   catch-up.
//! - **Which chats**: 1:1, group and meeting chats, but not one the chat service calls empty while the store holds no
//!   text of it and none unread (`isemptyconversation`: a chat nobody wrote in yet, as a test call's chat with the
//!   echo service, a meeting nobody wrote in, or a group chat made by a call's Add participants that holds only the
//!   call; Teams lists none of them). Teams finds them all the same, by search, by the switcher and by New message's
//!   To: ([`ChatList::findable_rows`]), and one opened is listed from then on, after the other chats, as browser Teams
//!   lists it (2026-11-06: a call-only a–b–c chat opened from To: stayed at the list's foot). A team's channels belong to the Teams list
//!   (Task 16), but for the favourite ones (below), and the account's own streams (`48:notifications`, …) aren't
//!   chats: neither is listed under "Chats".
//! - **Favorites** ([`Core::chat_listing`]: the `Favorites` folder of CSA's teams read, in its order): each one the
//!   list has a row for moves from "Chats" to "Favorites"; the account's own notes (`48:notes`, its chat with itself)
//!   is a row of its own there, [`NOTES`], named as Teams names it, "<name> (You)", with the account's own picture and
//!   presence. A team's channel is a favourite too, as Teams lists one ([`RowKind::Channel`]): named by the channel's
//!   name, its team's initial on a rounded square for its avatar (as the Teams list marks the team), its newest post
//!   for its preview, bold while it has unread posts (no count: Teams counts a channel's mentions, which the store
//!   doesn't tell apart), and opened as a channel. A favourite that is neither a stored chat nor a channel of
//!   the stored teams is left out.
//! - **Names** (M2 Task 12): a 1:1 chat by the other member's name as the engine has it ([`Core::person`]: their
//!   profile's, else their newest message's, else one the binary knew); a group or meeting chat by its topic, else its
//!   other members' first names as Teams joins them ("Evgenia and Sean"). Someone without a name is never shown by
//!   MRI: "Unknown user", a bot "Bot" (the test call's echo service "Test call").
//! - **Avatars** ([`AVATAR`] across): a 1:1 chat's other member's picture, else the initials of a name, else nothing;
//!   a group chat's two of its other members, half each, as Teams splits it; a meeting chat Teams' calendar.
//! - **Presence** (M2 Task 17): a 1:1 chat's avatar carries its other member's presence badge
//!   ([`crate::presence_badge`], nothing while it isn't known), never a bot's (it has none to read).
//!   After each load the engine is asked about the other members of the top [`LIST_WATCHED`] 1:1 chats, the lowest
//!   first so the top ones are the latest asked, then the selected chat's (its header's), again when the selection
//!   changes; the list draws what the presence model holds, again on each change of it. A row below them shows
//!   unknown unless another view asked about its person.
//! - **Unread** is the store's count (`unread_count`), by Teams' rules for what makes a chat unread.
//! - **A translated chat** ([`crate::chat_translate`]) shows its preview in its language once its translation is known
//!   ([`Previews`]): a row drawn asks for it, so a chat scrolled past or below See more asks nothing; the account's own
//!   message shows as it was written.
//!
//! A row clicked is told as [`ChatListEvent::Opened`]; the shell shows it. The switcher tells the shell itself
//! ([`crate::switcher::SwitcherEvent::Open`]), and the shell only highlights the row ([`ChatList::select`]). What
//! the list shows is traced (`chat-list shown`: the chats' ids in order, their kinds and unread counts, never a name
//! or text).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Local;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AsyncApp, Context, Div, ElementId, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Subscription, Task, WeakEntity, div, img, px, rgb,
};
use gpui_component::Size;
use teams_core::{Availability, ChatListing, MAX_WATCHED, has_presence};
use teams_proto::TEAMS_ECHO_BOT_MRI;
use teams_proto::chat::parse_html;
use teams_store::{Conversation, ConversationKind, Member, Message, Team, TeamChannel};

use crate::bridge::Core;
use crate::chat_translate::{Lookup, Previews};
use crate::chat_window::{ChatWindow, SeeMore};
use crate::clock::{self, HourCycle};
use crate::compose::PICKER_MAX;
use crate::icons::Glyph;
use crate::models::Models;
use crate::people::{self, Picture, Shown};
use crate::presence;
use crate::presence_badge;
use crate::system_message::{self, Preview};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The most chats the tray counts and the notifications look through.
pub const LIST_LIMIT: usize = 500;
/// The most chats the list holds: See more reaches them all, as do the switcher and the search.
pub const ALL_CHATS: usize = 10_000;
/// How many rows a first load builds before it shows them: the rest of a long list follows when it is built.
const FIRST_ROWS: usize = 50;
/// How many of a chat's newest messages are looked through for its preview line.
pub const PREVIEW_PAGE: usize = 20;
/// The longest preview line, in characters.
pub const PREVIEW_CHARS: usize = 120;
/// The most members a group chat without a topic is named by.
pub const NAMED_MEMBERS: usize = 4;
/// A row's avatar's diameter, px: twice the kit's small avatar (as the user asked), its initials and marks with it.
pub const AVATAR: f32 = 48.0;
/// The presence badge on a row's avatar, px across, its ring included: scaled with [`AVATAR`].
pub const AVATAR_BADGE: f32 = 20.0;
/// A row's height, px: the avatar and room around it.
pub const ROW_HEIGHT: f32 = AVATAR + 16.0;
/// The account's own notes: its chat with itself, which Teams lists under "Favorites" as "<name> (You)".
pub const NOTES: &str = "48:notes";
/// What a member without a name is called.
pub const UNKNOWN_USER: &str = "Unknown user";
/// What a bot or a service (a `28:` id) without a name is called.
pub const BOT: &str = "Bot";
/// What the test call's echo service is called (the name the app's own "Test call" gives it).
pub const ECHO: &str = "Test call";
/// What a deleted message's preview says.
pub const DELETED: &str = "This message was deleted";
/// How many of the top 1:1 chats' other members the list asks the engine about: with the open chat's (the header's)
/// and the mention picker's or the chat view's senders' [`PICKER_MAX`], never more than the engine watches ([`MAX_WATCHED`]), so the list's own
/// asks never drop the people it shows from the watch.
pub const LIST_WATCHED: usize = MAX_WATCHED - PICKER_MAX - 1;

/// What kind of chat a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    OneToOne,
    Group,
    Meeting,
    /// The account's own notes ([`NOTES`]).
    Notes,
    /// A team's channel among the favourites.
    Channel,
    /// A kind this build doesn't name.
    Other,
}

impl RowKind {
    #[must_use]
    pub fn of(kind: &ConversationKind) -> Self {
        match kind {
            ConversationKind::OneToOne => Self::OneToOne,
            ConversationKind::Group => Self::Group,
            ConversationKind::Meeting => Self::Meeting,
            // the store keeps a meeting's chat by the thread type the list gave it
            ConversationKind::Other(kind) if kind == "thread_type:meeting" => Self::Meeting,
            ConversationKind::Channel => Self::Channel,
            ConversationKind::Other(_) => Self::Other,
        }
    }

    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::OneToOne => "one-to-one",
            Self::Group => "group",
            Self::Meeting => "meeting",
            Self::Notes => "notes",
            Self::Channel => "channel",
            Self::Other => "other",
        }
    }
}

/// Whether the chat list shows a conversation: not a channel (the Teams list's), not one of the account's own
/// streams.
#[must_use]
pub fn listed(conversation: &Conversation) -> bool {
    !conversation.id.starts_with("48:") && conversation.kind != ConversationKind::Channel
}

/// One row of the list, as built from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRow {
    pub id: String,
    pub kind: RowKind,
    /// What the row is called (see the module docs).
    pub name: SharedString,
    /// Whether `name` came from people's names or the chat's topic, not from MRIs.
    pub named: bool,
    /// A 1:1 chat's other member's picture, from the engine.
    pub picture: Option<Picture>,
    /// A 1:1 chat's other member (the account itself for its notes), whose presence the row shows; never a bot.
    pub partner: Option<String>,
    /// A group chat's two members its avatar shows, half each (none for any other chat, or a group of one other).
    pub faces: Vec<HalfFace>,
    /// How many people the store lists in the chat, the account among them (0 when it lists none: a channel, the
    /// account's notes); a group chat's header counts them.
    pub members: usize,
    /// A group chat without a topic: its other members' first names as Teams' Activity says where an item happened
    /// ("Chat with Dmitry, Nikita"); none for any other chat.
    pub chat_with: Option<SharedString>,
    /// A channel's team's name, whose initial its avatar shows (none for a chat).
    pub team: Option<SharedString>,
    /// The last message's first line.
    pub preview: SharedString,
    /// The text message `preview` reads, when it reads one: a translated chat shows it in its language
    /// ([`crate::chat_translate::Previews`]).
    pub previewed: Option<Arc<Message>>,
    /// Milliseconds.
    pub last_activity: Option<i64>,
    pub unread: u64,
}

/// A member a group chat's avatar shows half of: the initial of their name, or their picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HalfFace {
    pub name: Option<SharedString>,
    pub picture: Option<Picture>,
}

/// The other member of a 1:1 chat its id names (`19:<oid>_<oid>@unq.gbl.spaces`), when one of the two is `me`.
#[must_use]
pub fn one_to_one_partner(id: &str, me: &str) -> Option<String> {
    let pair = id.strip_prefix("19:")?.strip_suffix("@unq.gbl.spaces")?;
    let (first, second) = pair.split_once('_')?;
    let mine = me.strip_prefix("8:orgid:")?;
    let other = match (first == mine, second == mine) {
        (true, false) => second,
        (false, true) => first,
        _ => return None,
    };
    Some(format!("8:orgid:{other}"))
}

/// The chat's members other than `me`; a 1:1 chat without stored members gives the one its id names.
#[must_use]
pub fn others(conversation: &Conversation, members: &[Member], me: &str) -> Vec<String> {
    let mut others: Vec<String> = members
        .iter()
        .map(|member| member.mri.clone())
        .filter(|mri| mri != me)
        .collect();
    if others.is_empty() && conversation.kind == ConversationKind::OneToOne {
        others.extend(one_to_one_partner(&conversation.id, me));
    }
    others
}

/// A message the list previews (and the store counts unread): text, deleted or not.
#[must_use]
pub fn is_text(message: &Message) -> bool {
    message.message_type == "Text" || message.message_type.starts_with("RichText")
}

/// The first line of `text` with something in it, at most [`PREVIEW_CHARS`] characters.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    if line.chars().count() <= PREVIEW_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(PREVIEW_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// A text message's first line with something in it, at most [`PREVIEW_CHARS`] characters: read from its sanitized
/// rich text, or as it came for a `Text` message; [`DELETED`] for a deleted one; Teams' "Sent a card" for a card
/// ([`crate::card_view::SENT_A_CARD`]: its fallback text is never shown).
#[must_use]
pub fn message_line(message: &Message) -> String {
    if message.delete_time.is_some() {
        DELETED.to_owned()
    } else if message.message_type == teams_proto::chat::card::CARD_TYPE {
        crate::card_view::SENT_A_CARD.to_owned()
    } else if message.message_type == "Text" {
        // plain text: its `<` and `&` are the text's own
        first_line(&message.content)
    } else {
        first_line(&parse_html(&message.content).plain_text())
    }
}

/// What a chat is called, as its row in the list names it (see the module docs).
pub async fn chat_name(core: &dyn Core, conversation: &Conversation) -> SharedString {
    let members = core.members(conversation.id.clone()).await.unwrap_or_default();
    name_of(core, conversation, &members).await.name
}

/// The preview line of a chat from its newest messages (newest first), as Teams' list words it: the newest text
/// message's first line, read from its sanitized rich text, or as it came for a `Text` message ("You: " before the
/// account's own); a meeting's start or end, a recording's state or a transcript in Teams' words
/// ([`system_message::preview`]); empty when there is none.
#[must_use]
pub fn preview(page: &[Message], me: &str) -> SharedString {
    previewing(page, me).0
}

/// The preview line ([`preview`]) and the text message it reads, when it reads one that isn't deleted or a card: the
/// one a translated chat shows in its language.
#[must_use]
pub fn previewing(page: &[Message], me: &str) -> (SharedString, Option<Arc<Message>>) {
    for message in page {
        let said = if message.delete_time.is_some() {
            Preview::Text
        } else {
            system_message::preview(&message.message_type, &message.content, &message.conversation_id)
        };
        match said {
            Preview::Skipped => {}
            Preview::Said(said) => return (said.into(), None),
            Preview::Text if !is_text(message) => {}
            Preview::Text => {
                let text = own_line(message_line(message), message, me);
                let read = (message.delete_time.is_none()
                    && message.message_type != teams_proto::chat::card::CARD_TYPE)
                    .then(|| Arc::new(message.clone()));
                return (text.into(), read);
            }
        }
    }
    (SharedString::default(), None)
}

/// `text`, a line of `message`, with "You: " before it when the message is the account's own.
fn own_line(text: String, message: &Message, me: &str) -> String {
    if message.sender.as_deref() == Some(me) {
        format!("You: {text}")
    } else {
        text
    }
}

/// The first line with something in it of `text` (a message's translation, or the account's own as written), at most
/// [`PREVIEW_CHARS`] characters.
#[must_use]
pub fn text_line(text: &teams_proto::chat::RichText) -> String {
    first_line(&text.plain_text())
}

/// What a chat is called, whether that is a name, and its picture.
struct Naming {
    name: SharedString,
    named: bool,
    picture: Option<Picture>,
    /// A 1:1 chat's other member, when they have a presence.
    partner: Option<String>,
    faces: Vec<HalfFace>,
    /// The members whose names the name or the avatar read.
    people: Vec<String>,
    /// A group chat without a topic: its members' first names, comma-joined ([`ChatRow::chat_with`]).
    chat_with: Option<SharedString>,
}

/// What someone without a name is called: never their MRI.
#[must_use]
pub fn unnamed(mri: &str) -> &'static str {
    if mri == TEAMS_ECHO_BOT_MRI {
        ECHO
    } else if mri.starts_with("28:") {
        BOT
    } else {
        UNKNOWN_USER
    }
}

/// The first word of a name, as Teams names a group chat's members.
pub(crate) fn first_name(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

/// Names joined as Teams joins them (English list form): "A", "A and B", "A, B, and C"; past [`NAMED_MEMBERS`]
/// `more` others counted, "A, B, C, D, +2".
#[must_use]
pub fn joined(names: &[String], more: usize) -> String {
    if more > 0 {
        return format!("{}, +{more}", names.join(", "));
    }
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// What a chat is called (see the module docs).
async fn name_of(core: &dyn Core, conversation: &Conversation, members: &[Member]) -> Naming {
    let topic = conversation
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|topic| !topic.is_empty())
        .filter(|_| conversation.kind != ConversationKind::OneToOne);
    let others = others(conversation, members, core.me());
    let kind = RowKind::of(&conversation.kind);
    // a meeting's chat shows its calendar, never its people: only its topic is read
    if let Some(topic) = topic
        && kind == RowKind::Meeting
    {
        return Naming {
            name: topic.to_owned().into(),
            named: true,
            picture: None,
            partner: None,
            faces: Vec::new(),
            people: Vec::new(),
            chat_with: None,
        };
    }
    if others.is_empty() {
        return Naming {
            name: topic.unwrap_or("Chat").to_owned().into(),
            named: topic.is_some(),
            picture: None,
            partner: None,
            faces: Vec::new(),
            people: Vec::new(),
            chat_with: None,
        };
    }
    let mut shown: Vec<(String, Option<Shown>)> = Vec::new();
    for mri in others.iter().take(NAMED_MEMBERS) {
        shown.push((mri.clone(), core.person(mri.clone()).await.ok().map(Shown::of)));
    }
    let people: Vec<String> = shown.iter().map(|(mri, _)| mri.clone()).collect();
    // a 1:1 chat shows the other member's picture and presence (a bot has none to read)
    if conversation.kind == ConversationKind::OneToOne && others.len() == 1 {
        let (mri, person) = shown.swap_remove(0);
        let name = person.as_ref().and_then(|person| person.name.clone());
        return Naming {
            named: name.is_some(),
            name: name.unwrap_or_else(|| unnamed(&mri).into()),
            picture: person.and_then(|person| person.picture),
            partner: has_presence(&mri).then_some(mri),
            faces: Vec::new(),
            people,
            chat_with: None,
        };
    }
    let faces = if kind == RowKind::Group && others.len() > 1 {
        half_faces(&shown)
    } else {
        Vec::new()
    };
    let (name, named, chat_with) = match topic {
        Some(topic) => (topic.to_owned(), true, None),
        None => members_named(&shown, others.len().saturating_sub(NAMED_MEMBERS)),
    };
    Naming {
        name: name.into(),
        named,
        picture: None,
        partner: None,
        faces,
        people,
        chat_with,
    }
}

/// A group chat's avatar: its first two members `shown`, half each.
fn half_faces(shown: &[(String, Option<Shown>)]) -> Vec<HalfFace> {
    shown
        .iter()
        .take(2)
        .map(|(_, person)| HalfFace {
            name: person.as_ref().and_then(|person| person.name.clone()),
            picture: person.as_ref().and_then(|person| person.picture.clone()),
        })
        .collect()
}

/// A group chat without a topic by the first names of the members `shown` (alphabetically, `more` others counted),
/// whether every one of them had a name, and the names as an activity item's place lists them
/// ([`ChatRow::chat_with`]: "Dmitry, Nikita").
fn members_named(shown: &[(String, Option<Shown>)], more: usize) -> (String, bool, Option<SharedString>) {
    let all_named = shown
        .iter()
        .all(|(_, person)| person.as_ref().is_some_and(|p| p.name.is_some()));
    let mut names: Vec<String> = shown
        .iter()
        .map(
            |(mri, person)| match person.as_ref().and_then(|person| person.name.as_ref()) {
                Some(name) => first_name(name).to_owned(),
                None => unnamed(mri).to_owned(),
            },
        )
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    let listed = if more > 0 {
        joined(&names, more)
    } else {
        names.join(", ")
    };
    (joined(&names, more), all_named, Some(listed.into()))
}

/// The account's own notes' row ([`NOTES`]): "<name> (You)" with its own picture and presence, its preview from the
/// stored notes (if any).
pub async fn notes_row(core: &dyn Core) -> ChatRow {
    let me = core.me().to_owned();
    let person = core.person(me.clone()).await.ok().map(Shown::of);
    let name = person.as_ref().and_then(|person| person.name.clone());
    let page = core
        .messages(NOTES.to_owned(), None, PREVIEW_PAGE)
        .await
        .map(|page| page.messages)
        .unwrap_or_default();
    let (preview, previewed) = previewing(&page, &me);
    ChatRow {
        id: NOTES.to_owned(),
        kind: RowKind::Notes,
        named: name.is_some(),
        name: name.map_or_else(|| "You".into(), |name| format!("{name} (You)").into()),
        picture: person.and_then(|person| person.picture),
        partner: Some(me.clone()),
        faces: Vec::new(),
        members: 0,
        chat_with: None,
        team: None,
        preview,
        previewed,
        last_activity: None,
        unread: 0,
    }
}

/// A favourite channel's row: the channel's name, its team's initial for its avatar, its newest post for its preview,
/// and `last_activity` (the stored conversation's, else its newest stored message's arrival).
pub async fn channel_row(core: &dyn Core, team: &Team, channel: &TeamChannel, last_activity: Option<i64>) -> ChatRow {
    let id = channel.id.clone();
    let page = core
        .messages(id.clone(), None, PREVIEW_PAGE)
        .await
        .map(|page| page.messages)
        .unwrap_or_default();
    let unread = core.unread_count(id.clone()).await.unwrap_or(0);
    let (preview, previewed) = previewing(&page, core.me());
    ChatRow {
        kind: RowKind::Channel,
        name: channel.name.clone().into(),
        named: true,
        picture: None,
        partner: None,
        faces: Vec::new(),
        members: 0,
        chat_with: None,
        team: Some(team.name.clone().into()),
        preview,
        previewed,
        last_activity: last_activity.or_else(|| page.iter().map(|message| message.arrival_time).max()),
        unread,
        id,
    }
}

/// The rows of the favourites that are channels of the stored teams (see the module docs), each with its stored
/// conversation's activity from `activity`; the teams are asked for only when a favourite isn't among the chat `rows`.
async fn channel_rows(
    core: &dyn Core,
    listing: &ChatListing,
    rows: &[ChatRow],
    activity: &HashMap<String, Option<i64>>,
) -> Vec<ChatRow> {
    let wanted: Vec<&String> = listing
        .favorites
        .iter()
        .flatten()
        .filter(|id| id.as_str() != NOTES && !rows.iter().any(|row| &row.id == *id))
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let teams = core.teams().await.ok().flatten().unwrap_or_default();
    let mut channels = Vec::new();
    for id in wanted {
        let found = teams.iter().find_map(|team| {
            team.channels
                .iter()
                .find(|channel| &channel.id == id)
                .map(|channel| (team, channel))
        });
        if let Some((team, channel)) = found {
            let last = activity.get(id).copied().flatten();
            channels.push(channel_row(core, team, channel, last).await);
        }
    }
    channels
}

/// The list's sections as [`arrange`] makes them.
#[derive(Clone, Debug, Default)]
pub struct Arranged {
    /// The favourites, in their order.
    pub favorites: Vec<ChatRow>,
    /// The other chats, the latest activity first, then the empty ones opened this run, in the order they were opened.
    pub rows: Vec<ChatRow>,
    /// The empty chats the list leaves out: found by the search, the switcher and New message all the same.
    pub hidden: Vec<ChatRow>,
}

/// The list's sections from the rows of the stored conversations, the latest activity first (see the module docs):
/// the favourites in their order (`notes` the account's own notes' row, when it is one of them), and the other chats,
/// without the ones the service calls empty that have no text and nothing unread, unless one was opened this run
/// (`opened`, in the order they were): as Teams lists a chat it hides once it is opened, after the others.
#[must_use]
pub fn arrange(rows: Vec<ChatRow>, listing: &ChatListing, mut notes: Option<ChatRow>, opened: &[String]) -> Arranged {
    let empty: HashSet<&str> = listing.empty.iter().map(String::as_str).collect();
    let (mut rows, mut hidden): (Vec<ChatRow>, Vec<ChatRow>) = rows
        .into_iter()
        .partition(|row| !(empty.contains(row.id.as_str()) && row.preview.is_empty() && row.unread == 0));
    for id in opened {
        if let Some(at) = hidden.iter().position(|row| &row.id == id) {
            rows.push(hidden.remove(at));
        }
    }
    let mut favorites = Vec::new();
    for id in listing.favorites.iter().flatten() {
        if id == NOTES {
            favorites.extend(notes.take());
        } else if let Some(at) = rows.iter().position(|row| &row.id == id) {
            favorites.push(rows.remove(at));
        }
    }
    Arranged {
        favorites,
        rows,
        hidden,
    }
}

/// A chat's row, from the store. A query that fails leaves its part empty: the row is still shown.
pub async fn build_row(core: &dyn Core, conversation: &Conversation) -> ChatRow {
    build(core, conversation).await.0
}

/// A chat's row, and the members whose names its name read.
async fn build(core: &dyn Core, conversation: &Conversation) -> (ChatRow, Vec<String>) {
    let id = conversation.id.clone();
    let members = core.members(id.clone()).await.unwrap_or_default();
    let mut page = core
        .messages(id.clone(), None, PREVIEW_PAGE)
        .await
        .map(|page| page.messages)
        .unwrap_or_default();
    if page.is_empty()
        && let Ok(Some(preview)) = core.conversation_preview(id.clone()).await
    {
        page.push(preview);
    }
    let unread = core.unread_count(id.clone()).await.unwrap_or(0);
    let Naming {
        name,
        named,
        picture,
        partner,
        faces,
        people,
        chat_with,
    } = name_of(core, conversation, &members).await;
    let (preview, previewed) = previewing(&page, core.me());
    let row = ChatRow {
        kind: RowKind::of(&conversation.kind),
        name,
        named,
        picture,
        partner,
        faces,
        members: members.len(),
        chat_with,
        team: None,
        preview,
        previewed,
        last_activity: conversation.last_activity,
        unread,
        id,
    };
    (row, people)
}

/// What the chat list tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatListEvent {
    /// A chat was opened from the list.
    Opened(String),
}

/// A row built, and what it was built from.
#[derive(Clone, Debug)]
struct Cached {
    conversation: Conversation,
    /// The models' revision when it was built: a later change of its chat builds it again.
    at: u64,
    /// The members whose names its name read: a later message carrying one's name builds it again.
    people: Vec<String>,
    row: ChatRow,
}

/// A row for a load to keep, or to build.
enum Planned {
    Keep(ChatRow),
    Build(Conversation),
}

/// A row a load built: its conversation, and the members whose names its name read.
type Built = (Conversation, Vec<String>);

/// The chat list view (see the module docs).
pub struct ChatList {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    /// The "Favorites" section's rows, in Teams' order.
    favorites: Vec<ChatRow>,
    /// The "Chats" section's rows, shown or not.
    rows: Vec<ChatRow>,
    /// The empty chats the section leaves out ([`Arranged::hidden`]).
    hidden: Vec<ChatRow>,
    /// The empty chats opened this run, in the order they were: listed after the others ([`arrange`]).
    opened_empty: Vec<String>,
    /// Which of them are shown.
    window: ChatWindow,
    cache: HashMap<String, Cached>,
    /// Moves when the cache is dropped: a load that started before keeps none of its rows.
    generation: u64,
    selected: Option<String>,
    loading: Option<Task<()>>,
    /// A change came while a load ran: another follows it.
    dirty: bool,
    /// Loads that ended.
    loads: u64,
    catch_ups: u64,
    traced: Option<Vec<(String, RowKind, u64)>>,
    /// The account's translations: a translated chat's preview shows in its language ([`ChatList::set_previews`]).
    previews: Option<(Entity<Previews>, Subscription)>,
    _observing: [Subscription; 3],
}

impl EventEmitter<ChatListEvent> for ChatList {}

impl ChatList {
    /// The list over the store, loaded at once; `selected` is highlighted.
    pub fn new(
        core: Arc<dyn Core>,
        models: Models,
        trace: Arc<ViewTrace>,
        selected: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let chats = cx.observe(&models.chats, |this: &mut Self, _, cx| this.reload(cx));
        // a presence the engine told: the rows' dots are drawn again from the model
        let presence = cx.observe(&models.presence, |_: &mut Self, _, cx| cx.notify());
        let status = cx.observe(&models.status, |this: &mut Self, status, cx| {
            let catch_ups = status.read(cx).catch_ups;
            if catch_ups != this.catch_ups {
                // a catch-up may have brought names of people in other chats
                this.catch_ups = catch_ups;
                this.cache.clear();
                this.generation += 1;
                this.window.caught_up();
                this.reload(cx);
            }
            cx.notify();
        });
        let catch_ups = models.status.read(cx).catch_ups;
        let mut list = Self {
            core,
            models,
            trace,
            favorites: Vec::new(),
            rows: Vec::new(),
            hidden: Vec::new(),
            opened_empty: Vec::new(),
            window: ChatWindow::default(),
            cache: HashMap::new(),
            generation: 0,
            selected,
            loading: None,
            dirty: false,
            loads: 0,
            catch_ups,
            traced: None,
            previews: None,
            _observing: [chats, status, presence],
        };
        list.reload(cx);
        list
    }

    /// From now on a translated chat's row shows its preview in the chat's language, through `previews`.
    pub fn set_previews(&mut self, previews: Entity<Previews>, cx: &mut Context<Self>) {
        let observing = cx.observe(&previews, |_: &mut Self, _, cx| cx.notify());
        self.previews = Some((previews, observing));
        cx.notify();
    }

    /// The preview line `row` shows: its last message in its chat's language when the chat is translated (the
    /// account's own as written), as the store has it otherwise. A row drawn whose translation isn't known asks for it
    /// (see [`Previews::want`]).
    #[must_use]
    pub fn preview_shown(&self, row: &ChatRow, cx: &mut App) -> SharedString {
        let (Some((previews, _)), Some(message)) = (&self.previews, &row.previewed) else {
            return row.preview.clone();
        };
        match previews.read(cx).lookup(message, cx) {
            Lookup::Text(text) => own_line(text_line(&text), message, self.core.me()).into(),
            Lookup::Unknown => {
                let message = Message::clone(message);
                previews.update(cx, |previews, cx| previews.want([message], cx));
                row.preview.clone()
            }
            Lookup::Off | Lookup::AsWritten => row.preview.clone(),
        }
    }

    /// The "Chats" section's rows, the latest activity first, shown or not.
    #[must_use]
    pub fn rows(&self) -> &[ChatRow] {
        &self.rows
    }

    /// The rows the section shows, as indices of [`Self::rows`], in their order.
    #[must_use]
    pub fn shown_rows(&self) -> Vec<usize> {
        self.window.shown(&self.rows)
    }

    /// What See more would bring, when anything is left below the rows shown.
    #[must_use]
    pub fn see_more_offered(&self) -> Option<SeeMore> {
        self.window.see_more_offered(&self.rows)
    }

    /// See more: more of the rows shown.
    pub fn see_more(&mut self, cx: &mut Context<Self>) {
        self.trace.record(&TraceLine::new("chat-list", "see more"));
        self.window.see_more(&self.rows, self.selected.as_deref());
        cx.notify();
    }

    /// Whether the engine sends nothing ([`Core::read_only`]): the app's read-only mode, which opens no push channel.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.core.read_only()
    }

    /// The "Favorites" section's rows, in Teams' order.
    #[must_use]
    pub fn favorites(&self) -> &[ChatRow] {
        &self.favorites
    }

    /// Every row, shown or kept below See more: the favourites, then the other chats.
    pub fn all_rows(&self) -> impl Iterator<Item = &ChatRow> {
        self.favorites.iter().chain(&self.rows)
    }

    /// Every chat the account can find, listed or not: [`Self::all_rows`], then the empty chats the list leaves out,
    /// which Teams' search, its switcher and New message's To: find all the same.
    pub fn findable_rows(&self) -> impl Iterator<Item = &ChatRow> {
        self.all_rows().chain(&self.hidden)
    }

    /// A row, shown, kept below See more or left out as empty, by its chat's id.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&ChatRow> {
        self.findable_rows().find(|row| row.id == id)
    }

    /// Whether the stored chat identifies whom a call would ring, even while catch-up is still reading its roster.
    #[must_use]
    pub fn can_call(&self, id: &str) -> bool {
        one_to_one_partner(id, self.core.me()).is_some() || self.row(id).is_some_and(|row| row.members > 1)
    }

    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// How many loads from the store have ended.
    #[must_use]
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// Whether a load is running.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Highlights a chat (the one the shell shows), telling nobody. An empty chat the list left out is listed from now
    /// on, after the others, as Teams lists one once it is opened.
    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if let Some(at) = id
            .as_deref()
            .and_then(|id| self.hidden.iter().position(|row| row.id == id))
        {
            let row = self.hidden.remove(at);
            self.opened_empty.push(row.id.clone());
            self.rows.push(row);
            self.trace_rows();
        }
        if self.selected != id {
            self.selected = id;
            self.window.settle(&self.rows, self.selected.as_deref());
            self.watch_shown();
            cx.notify();
        }
    }

    /// Asks the engine about the people the list shows a presence for (see the module docs): the other members of the
    /// top [`LIST_WATCHED`] 1:1 chats, the lowest first, then the selected chat's, so the watch keeps the top ones and
    /// the header's longest.
    fn watch_shown(&self) {
        let mut partners: Vec<&String> = Vec::new();
        for partner in self.all_rows().filter_map(|row| row.partner.as_ref()) {
            if partners.len() == LIST_WATCHED {
                break;
            }
            if !partners.contains(&partner) {
                partners.push(partner);
            }
        }
        let selected = self
            .selected
            .as_deref()
            .and_then(|id| self.row(id))
            .and_then(|row| row.partner.as_ref());
        presence::watch(self.core.as_ref(), partners.into_iter().rev().chain(selected));
    }

    /// Opens a chat: highlighted, and told to the shell.
    pub fn open(&mut self, id: String, cx: &mut Context<Self>) {
        self.select(Some(id.clone()), cx);
        cx.emit(ChatListEvent::Opened(id));
    }

    /// Reads the list from the store again (or once the running load ends).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        let core = Arc::clone(&self.core);
        let generation = self.generation;
        self.loading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            load(core, generation, this, cx).await;
        }));
    }

    /// Which rows a load keeps and which it builds, of the conversations the store listed.
    fn plan(&self, listed: Vec<Conversation>, cx: &App) -> Vec<Planned> {
        let chats = self.models.chats.read(cx);
        listed
            .into_iter()
            .filter(self::listed)
            .map(|conversation| match self.cache.get(&conversation.id) {
                Some(cached)
                    if cached.at >= chats.changed_at(&conversation.id)
                        && cached.people.iter().all(|mri| cached.at >= chats.named_at(mri))
                        && cached.conversation == conversation =>
                {
                    Planned::Keep(cached.row.clone())
                }
                _ => Planned::Build(conversation),
            })
            .collect()
    }

    /// Keeps the rows a load built, as of the models' revision `at`.
    fn keep_built(&mut self, rows: &[ChatRow], built: Vec<Built>, at: u64) {
        for (conversation, people) in built {
            let Some(row) = rows.iter().find(|row| row.id == conversation.id) else {
                continue;
            };
            let cached = Cached {
                row: row.clone(),
                at,
                people,
                conversation,
            };
            self.cache.insert(cached.row.id.clone(), cached);
        }
    }

    /// A load ended: its rows shown, the ones it built kept (unless the cache was dropped since it started).
    fn loaded(&mut self, generation: u64, at: u64, rows: Option<Loaded>, cx: &mut Context<Self>) {
        self.loading = None;
        self.loads += 1;
        if let Some(Loaded {
            rows,
            built,
            listing,
            notes,
        }) = rows
        {
            if generation == self.generation {
                self.keep_built(&rows, built, at);
            }
            let listed: HashSet<&str> = rows.iter().map(|row| row.id.as_str()).collect();
            self.cache.retain(|id, _| listed.contains(id.as_str()));
            let arranged = arrange(rows, &listing, notes, &self.opened());
            self.show(arranged);
            self.trace_rows();
            self.watch_shown();
        }
        cx.notify();
        if self.dirty {
            self.reload(cx);
        }
    }

    /// The empty chats opened this run, the selected one among them: the ones [`arrange`] lists after the others.
    fn opened(&mut self) -> Vec<String> {
        if let Some(selected) = &self.selected
            && !self.opened_empty.contains(selected)
            && !self.all_rows().any(|row| &row.id == selected)
        {
            self.opened_empty.push(selected.clone());
        }
        self.opened_empty.clone()
    }

    /// The rows a load has, in their sections.
    fn show(&mut self, arranged: Arranged) {
        self.favorites = arranged.favorites;
        self.rows = arranged.rows;
        self.hidden = arranged.hidden;
        self.window.settle(&self.rows, self.selected.as_deref());
    }

    /// The first rows of a first load, shown before the rest are built; a load over rows already shown keeps them.
    fn show_first(&mut self, rows: &[ChatRow], listing: &ChatListing, cx: &mut Context<Self>) {
        if self.all_rows().next().is_none() {
            let opened = self.opened();
            self.show(arrange(rows.to_vec(), listing, None, &opened));
            cx.notify();
        }
    }

    /// Traces what the list shows, when it changed.
    fn trace_rows(&mut self) {
        let shown: Vec<(String, RowKind, u64)> = self
            .all_rows()
            .map(|row| (row.id.clone(), row.kind, row.unread))
            .collect();
        if self.traced.as_ref() == Some(&shown) {
            return;
        }
        let line = TraceLine::new("chat-list", "shown").rows(
            shown
                .iter()
                .map(|(id, kind, unread)| (id.as_str(), kind.name(), *unread)),
        );
        self.trace.record(&line);
        self.traced = Some(shown);
    }

    /// The "Chats" row at `index`; its test selector `chat-row-<index>`.
    pub(crate) fn render_row(&self, index: usize, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        match self.rows.get(index) {
            Some(row) => self.render(row, format!("chat-row-{index}"), palette, None, cx),
            None => div().into_any_element(),
        }
    }

    /// The "Favorites" row at `index`; its test selector `favorite-row-<index>`.
    pub(crate) fn render_favorite(
        &self,
        index: usize,
        palette: Palette,
        more: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.favorites.get(index) {
            Some(row) => self.render(row, format!("favorite-row-{index}"), palette, more, cx),
            None => div().into_any_element(),
        }
    }

    fn render(
        &self,
        row: &ChatRow,
        selector: String,
        palette: Palette,
        more: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected.as_deref() == Some(row.id.as_str());
        let id = row.id.clone();
        let background = if selected { palette.selected } else { palette.surface };
        let element_id = format!("{selector}-{}", row.id);
        // the chosen row's mark: a bar in the brand gradient at its left edge, growing in as the row is chosen
        let mark = div()
            .absolute()
            .left_0()
            .top(px(ROW_HEIGHT / 2.0))
            .w(px(3.0))
            .rounded_r_sm()
            .bg(theme::gradient(palette.brand, palette.accent, 180.0));
        let mark = crate::motion::toggle(
            mark,
            SharedString::from(format!("{element_id}-mark")),
            selected,
            cx,
            |mark, t| {
                let height = (ROW_HEIGHT - 20.0) * t.max(0.0);
                mark.h(px(height)).mt(px(-height / 2.0))
            },
        );
        div()
            .id(ElementId::Name(element_id.into()))
            .debug_selector(move || selector.clone())
            .relative()
            .w_full()
            .h(px(ROW_HEIGHT))
            .when(more.is_none(), gpui::Styled::overflow_hidden)
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .when(selected, |row| {
                row.bg(theme::gradient(
                    palette.selected,
                    theme::mix(palette.selected, palette.surface, 0.6),
                    90.0,
                ))
            })
            .when(!selected, |row| row.bg(rgb(background)))
            .hover(move |style| style.bg(rgb(if selected { palette.selected } else { palette.hover })))
            .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), cx)))
            .child(mark)
            .child(self.avatar(row, background, palette, cx))
            .child(row_text(row, &self.preview_shown(row, cx), palette, clock::cycle(cx)))
            .children(more)
            .into_any_element()
    }

    /// The row's avatar ([`avatar`]), with its person's presence badge at its corner (a 1:1 chat's other member, the
    /// account's own on its notes) when it is known, ringed in the row's `background`.
    fn avatar(&self, row: &ChatRow, background: u32, palette: Palette, cx: &App) -> AnyElement {
        let badge = self
            .presence_of(row, cx)
            .and_then(|availability| presence_badge::badge(&availability, AVATAR_BADGE, palette));
        let Some(badge) = badge else {
            return avatar(row, palette);
        };
        div()
            .relative()
            .flex_none()
            .child(avatar(row, palette))
            .child(
                badge
                    .border_color(rgb(background))
                    .absolute()
                    .right(px(-2.0))
                    .bottom(px(-2.0)),
            )
            .into_any_element()
    }

    /// The presence a row shows: its 1:1 chat's other member's (the account's own on its notes), as the presence
    /// model holds it; `None` for a row that shows none (a group or meeting chat, a bot).
    #[must_use]
    pub fn presence_of(&self, row: &ChatRow, cx: &App) -> Option<Availability> {
        let partner = row.partner.as_deref()?;
        Some(self.models.presence.read(cx).of(partner))
    }
}

/// A group chat's face `size` across, where a picker lists it (New message's To:): its two members split as the list
/// draws them, else the initials of its name.
#[must_use]
pub fn group_face(row: &ChatRow, palette: Palette, size: f32) -> AnyElement {
    match row.faces.as_slice() {
        [left, right, ..] => split_avatar(left, right, theme::palette_is_dark(palette), size).into_any_element(),
        _ => div()
            .flex_none()
            .child(people::avatar(
                row.named.then_some(&row.name),
                row.picture.as_ref(),
                Size::Size(px(size)),
            ))
            .into_any_element(),
    }
}

/// A row's avatar, [`AVATAR`] across: a meeting's calendar, a group's two members split, else the picture, else the
/// initials of its name (not of an MRI), else a blank one.
fn avatar(row: &ChatRow, palette: Palette) -> AnyElement {
    match (row.kind, row.faces.as_slice()) {
        (RowKind::Meeting, _) => mark_avatar(
            Glyph::CALENDAR,
            theme::brand_gradient(palette, 135.0),
            palette.on_brand,
            "avatar-meeting",
        ),
        (RowKind::Channel, _) => team_avatar(row.team.as_deref().unwrap_or_default(), palette),
        // the Workflows bot's chat: the app's icon on its tile, as Teams draws an app's
        (RowKind::OneToOne, _) if is_workflows_chat(&row.id) => div()
            .debug_selector(|| "avatar-workflows".to_owned())
            .flex_none()
            .child(crate::workflows_app::bot_avatar(AVATAR, palette))
            .into_any_element(),
        // a 1:1 chat whose other member has no presence: a bot or a service
        (RowKind::OneToOne, _) if row.partner.is_none() && row.picture.is_none() => mark_avatar(
            Glyph::BOT,
            rgb(palette.hover).into(),
            palette.muted_foreground,
            "avatar-bot",
        ),
        (RowKind::Group, [left, right, ..]) => {
            split_avatar(left, right, theme::palette_is_dark(palette), AVATAR).into_any_element()
        }
        _ => {
            let name = row.named.then(|| match row.kind {
                // the initials of the account's own name, not of "(You)"
                RowKind::Notes => SharedString::from(row.name.trim_end_matches(" (You)").to_owned()),
                _ => row.name.clone(),
            });
            div()
                .flex_none()
                .child(people::avatar(
                    name.as_ref(),
                    row.picture.as_ref(),
                    Size::Size(px(AVATAR)),
                ))
                .into_any_element()
        }
    }
}

/// Whether `chat` is an account's chat with the Workflows bot (`19:<account>_<bot>@unq.gbl.spaces`).
fn is_workflows_chat(chat: &str) -> bool {
    chat.strip_suffix("@unq.gbl.spaces")
        .and_then(|ids| ids.rsplit_once('_'))
        .is_some_and(|(_, bot)| bot == crate::workflows_app::WORKFLOWS_BOT)
}

/// An avatar of one mark on a colour, its test selector `selector`: Teams' calendar on the brand colour for a
/// meeting's chat, a bot's head on a quiet one for a bot.
fn mark_avatar(mark: Glyph, background: gpui::Background, foreground: u32, selector: &'static str) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .size(px(AVATAR))
        .flex_none()
        .rounded_full()
        .bg(background)
        .flex()
        .items_center()
        .justify_center()
        .child(mark.icon().size(px(AVATAR * 0.5)).text_color(rgb(foreground)))
        .into_any_element()
}

/// A channel's avatar: its team's initial on the brand colour, on a rounded square as Teams draws a team (and as the
/// Teams list marks one).
fn team_avatar(team: &str, palette: Palette) -> AnyElement {
    let initial: String = team
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    div()
        .debug_selector(|| "avatar-team".to_owned())
        .size(px(AVATAR))
        .flex_none()
        .rounded_lg()
        .bg(theme::brand_gradient(palette, 135.0))
        .text_color(rgb(palette.on_brand))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_size(px(AVATAR * 0.4))
        .flex()
        .items_center()
        .justify_center()
        .child(initial)
        .into_any_element()
}

/// The gap between a split avatar's halves, px.
const SPLIT_GAP: f32 = 1.0;

/// A group chat's avatar as Teams splits it: two members, half each, in a `dark` theme's colours or a light one's.
fn split_avatar(left: &HalfFace, right: &HalfFace, dark: bool, size: f32) -> Div {
    div()
        .debug_selector(|| "avatar-split".to_owned())
        .size(px(size))
        .flex_none()
        .flex()
        .gap(px(SPLIT_GAP))
        .child(half(left, true, dark, size))
        .child(half(right, false, dark, size))
}

/// One half of a split avatar: the member's picture, else the initial of their name on a colour of its own.
fn half(face: &HalfFace, left: bool, dark: bool, size: f32) -> AnyElement {
    let width = px((size - SPLIT_GAP) / 2.0);
    let radius = px(size / 2.0);
    if let Some(picture) = &face.picture {
        let image = img(Arc::clone(picture.image()))
            .w(width)
            .h(px(size))
            .object_fit(ObjectFit::Cover);
        let image = if left {
            image.rounded_tl(radius).rounded_bl(radius)
        } else {
            image.rounded_tr(radius).rounded_br(radius)
        };
        return image.into_any_element();
    }
    let initial: String = face
        .name
        .as_deref()
        .and_then(|name| name.chars().find(|c| c.is_alphanumeric()))
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    let (background, foreground) = initials_colour(face.name.as_deref().unwrap_or(""), dark);
    let half = div()
        .w(width)
        .h(px(size))
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(background))
        .text_color(rgb(foreground))
        .text_size(px(size * 0.4))
        .child(initial);
    if left {
        half.rounded_tl(radius).rounded_bl(radius).into_any_element()
    } else {
        half.rounded_tr(radius).rounded_br(radius).into_any_element()
    }
}

/// Teams' soft avatar colours (background, initials), one picked by the name.
const INITIALS_COLOURS: [(u32, u32); 6] = [
    (0x00f6_d1d5, 0x00a4_262c),
    (0x00fd_e7d3, 0x008e_562e),
    (0x00d6_e5f6, 0x0000_4e8c),
    (0x00d9_ecd4, 0x0028_6e2c),
    (0x00e4_dcf2, 0x005c_2e91),
    (0x00f7_e5c2, 0x0080_5c00),
];

/// The same hues for a dark theme, as the kit draws a person's initials there: a deep tint, the initials light.
const DARK_INITIALS_COLOURS: [(u32, u32); 6] = [
    (0x004a_2a2e, 0x00f1_a6ad),
    (0x0049_3324, 0x00f3_bd8f),
    (0x001f_3550, 0x009c_c8f5),
    (0x0024_402a, 0x00a6_db9f),
    (0x0037_2a4f, 0x00c9_b1f2),
    (0x0045_391c, 0x00e9_cf7d),
];

/// The colours of `name`'s initials in a `dark` theme or a light one, the same hue for the same name in both.
#[must_use]
pub fn initials_colour(name: &str, dark: bool) -> (u32, u32) {
    let sum = name
        .bytes()
        .fold(0_usize, |sum, byte| sum.wrapping_add(usize::from(byte)));
    let colours = if dark {
        &DARK_INITIALS_COLOURS
    } else {
        &INITIALS_COLOURS
    };
    colours[sum % colours.len()]
}

/// The row's name and time (on `cycle`'s clock) over its preview (`line`, in its chat's language when it is
/// translated) and unread count (when it has either).
fn row_text(row: &ChatRow, line: &SharedString, palette: Palette, cycle: HourCycle) -> impl IntoElement {
    let time = row
        .last_activity
        .map(|ms| clock::list_time(ms, &Local::now(), cycle))
        .unwrap_or_default();
    let name = div().flex_1().min_w_0().truncate().child(row.name.clone());
    let name = if row.unread > 0 {
        name.font_weight(gpui::FontWeight::BOLD)
    } else {
        name
    };
    let top = div()
        .flex()
        .gap_2()
        .child(name)
        .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(time));
    let preview = div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .child(line.clone());
    let mut bottom = div().flex().gap_2().items_center().child(preview);
    // a channel's unread posts make it bold, uncounted (see the module docs)
    if row.unread > 0 && row.kind != RowKind::Channel {
        bottom = bottom.child(
            div()
                .min_w(px(18.0))
                .h(px(18.0))
                .px_1p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .bg(theme::brand_gradient(palette, 135.0))
                .shadow(theme::glow(palette, palette.brand, 0.5, 8.0, 2.0))
                .text_color(rgb(palette.on_brand))
                .child(row.unread.to_string()),
        );
    }
    let text = div().flex_1().min_w_0().flex().flex_col().child(top);
    // a row with nothing under its name (the account's own notes before anything was written) is one line, centred
    if line.is_empty() && row.unread == 0 {
        text
    } else {
        text.child(bottom)
    }
}

/// What a load read: the rows it kept or built, the ones it built, the favourites and the empty chats, and the
/// account's own notes' row when they are one of the favourites.
struct Loaded {
    rows: Vec<ChatRow>,
    built: Vec<Built>,
    listing: ChatListing,
    notes: Option<ChatRow>,
}

/// One load: the conversation list, then each row kept or built, then shown.
async fn load(core: Arc<dyn Core>, generation: u64, this: WeakEntity<ChatList>, cx: &mut AsyncApp) {
    let listed = core.conversations(ALL_CHATS).await;
    // without it (an engine that stopped, a store that failed) every chat is listed under "Chats"
    let listing = core.chat_listing().await.unwrap_or_default();
    // the stored channels' activity, for the favourite ones' rows
    let activity: HashMap<String, Option<i64>> = listed
        .iter()
        .flatten()
        .filter(|conversation| conversation.kind == ConversationKind::Channel)
        .map(|conversation| (conversation.id.clone(), conversation.last_activity))
        .collect();
    let Ok(planned) = this.update(cx, |list, cx| {
        let at = list.models.chats.read(cx).revision();
        (listed.map(|listed| list.plan(listed, cx)), at)
    }) else {
        // the list is gone
        return;
    };
    let (planned, at) = planned;
    let rows = match planned {
        Ok(planned) => {
            let (mut rows, built) = build_rows(&*core, planned, &listing, &this, cx).await;
            let channels = channel_rows(&*core, &listing, &rows, &activity).await;
            // each one a favourite: moved to "Favorites" in its place
            rows.extend(channels);
            let favored = listing.favorites.iter().flatten().any(|id| id == NOTES);
            let notes = if favored { Some(notes_row(&*core).await) } else { None };
            Some(Loaded {
                rows,
                built,
                listing,
                notes,
            })
        }
        // the engine stopped: the rows shown stay
        Err(_) => None,
    };
    let _ = this.update(cx, |list, cx| list.loaded(generation, at, rows, cx));
}

/// The rows of a load, in its order, and the conversations it built rows for; the first [`FIRST_ROWS`] are shown to a
/// list that has none yet, as a long list takes a while to build.
async fn build_rows(
    core: &dyn Core,
    planned: Vec<Planned>,
    listing: &ChatListing,
    this: &WeakEntity<ChatList>,
    cx: &mut AsyncApp,
) -> (Vec<ChatRow>, Vec<Built>) {
    let mut rows = Vec::with_capacity(planned.len());
    let mut built = Vec::new();
    for item in planned {
        match item {
            Planned::Keep(row) => rows.push(row),
            Planned::Build(conversation) => {
                let (row, people) = build(core, &conversation).await;
                rows.push(row);
                built.push((conversation, people));
            }
        }
        if rows.len() == FIRST_ROWS {
            let _ = this.update(cx, |list, cx| list.show_first(&rows, listing, cx));
        }
    }
    (rows, built)
}
