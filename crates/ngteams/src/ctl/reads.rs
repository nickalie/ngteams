//! The read commands, each through the addressed account's [`Core`] as the views read, mapped to the wire's rows.
//!
//! - `chats`: the chats the chat list lists, the latest activity first, each named, previewed and counted as its row
//!   is ([`chat_list::build_row`]); `unread_only` keeps those with anything unread.
//! - `messages`: a chat's or a channel's messages, the newest first, as plain text: the text messages and the lines
//!   Teams says in a chat ("Meeting started", …) as the chat list's preview reads them ([`system_message::preview`]);
//!   a card's text is the text on it, one line for each text, fact or button
//!   (`teams_proto::chat::card::card_message_text`); the rest (a thread's events, a call's records) left out. `before`
//!   pages back from a message. The answer stops before a message that would take its line over the wire's cap
//!   ([`MAX_LINE`]): fewer than asked, the client pages on from the last one. When the store holds no older message,
//!   the answer says so (`end`): the beginning once the store holds the whole history. Each row lists the message's
//!   attachments as `download` names them ([`super::download`]).
//! - **A channel** ([`address::conversation`]): synced first, as the channel view syncs it when it opens
//!   ([`Core::sync_conversation`]; a channel other than a team's general one is in no conversation list, so no
//!   catch-up reads it), within [`CHANNEL_SYNC`]; a sync that fails or takes longer leaves the store's rows, with
//!   `synced` false. Its rows name a reply's post, and count a post's stored replies ([`Core::reply_roots`]), and carry
//!   a post's subject and email as the chat view draws them. `thread` reads one post's thread from the store, the post
//!   last.
//! - `teams`: the account's teams and their channels, as the Teams list shows them ([`Core::teams`]). Before their
//!   first read the answer waits for it, up to [`TEAMS_ANSWER`].
//! - `search`: the store's own search over the listed chats, as the window's search box searches; its hits also stop
//!   where the line is full.
//! - `members`: a chat's stored members with their names.
//! - `activity`: the account's activity feed as the Activity pane lists it (the newest first, reactions to one message
//!   folded), each item with its kind, title, preview, place and read state; `filter` keeps what one of the pane's
//!   chips keeps, `search` what its search box finds.
//! - `unread`: the badge's own counts, from the account's [`teams_ui::Tray`]: each unread chat's count of unread
//!   messages, and the total of unread chats the badge shows. Before the badge's first count (a moment after the tile
//!   is built) there is nothing to tell yet: the answer waits for it, up to [`FIRST_COUNT`], then says `timeout`
//!   rather than 0.
//!
//! A sender's name is the engine's ([`Core::person`]), else the one the message carried.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Map, Value};

use gpui::{AsyncApp, Entity};
use teams_core::activity::{ActivityFilter, ActivityItem, ActivityKind};
use teams_ctl::codec::{self, MAX_LINE};
use teams_ctl::protocol::{
    ActivityRow, CHANNEL_SYNC, ChannelRow, ChatRow, CtlError, EmailPerson, EmailRow, ErrorCode, HistoryEnd, MemberRow,
    MessageRow, Outcome, Request, Response, ResponseLine, TEAMS_ANSWER, TeamRow, Unread, UnreadRow,
};
use teams_proto::chat::card::{CARD_TYPE, message_cards_text};
use teams_proto::chat::parse_html;
use teams_proto::chat::{EmailDetails, EmailUser};
use teams_store::{ChannelKind, Message, PageCursor, Team};
use teams_ui::activity_view;
use teams_ui::card_view::SENT_A_CARD;
use teams_ui::chat_list;
use teams_ui::rich_text::Mentions;
use teams_ui::search::{self, MAX_HITS};
use teams_ui::system_message::{self, Preview};
use teams_ui::{Chats, Core, Tray};

use super::address::{self, Addressed, Target, engine_error};

/// How many messages `messages` gives without a limit.
pub const MESSAGES: usize = 50;
/// The most messages one `messages` gives.
pub const MAX_MESSAGES: usize = 1000;
/// How long `unread` waits for the badge's first count: well past its delay, inside a client's read timeout.
pub const FIRST_COUNT: Duration = Duration::from_secs(5);

/// `request`, a read, answered for `account`.
///
/// # Errors
/// The chat named wasn't found, the request isn't a read, or the engine failed ([`engine_error`]).
pub async fn answer(request: &Request, account: &Addressed, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    let core = &*account.core;
    match request {
        Request::Chats { unread_only, limit } => chats(core, *unread_only, *limit).await,
        Request::Messages { chat, limit, before } => messages(core, chat, *limit, before.as_deref(), cx).await,
        Request::Thread { channel, post, limit } => thread(core, channel, post, *limit, cx).await,
        Request::Teams => teams(account, cx).await,
        Request::Search { query, limit } => found(core, query, *limit).await,
        Request::Members { chat } => members(core, chat).await,
        Request::Unread => unread(account, cx).await,
        Request::Activity { filter, search, limit } => {
            activity(core, filter.as_deref(), search.as_deref(), *limit).await
        }
        _ => Err(CtlError::new(ErrorCode::BadRequest, "not a read")),
    }
}

/// A count from the wire, as a `usize`.
fn count(limit: Option<u32>, default: usize, max: usize) -> usize {
    limit
        .map_or(default, |limit| usize::try_from(limit).unwrap_or(max))
        .min(max)
}

async fn chats(core: &dyn Core, unread_only: bool, limit: Option<u32>) -> Result<Response, CtlError> {
    let limit = count(limit, usize::MAX, usize::MAX);
    let mut chats = Vec::new();
    for conversation in address::listed(core).await? {
        if chats.len() >= limit {
            break;
        }
        let row = chat_list::build_row(core, &conversation).await;
        if unread_only && row.unread == 0 {
            continue;
        }
        chats.push(ChatRow {
            id: row.id,
            title: row.name.to_string(),
            chat_type: row.kind.name().to_owned(),
            unread: row.unread,
            last_activity: row.last_activity,
            preview: Some(row.preview.to_string()).filter(|preview| !preview.is_empty()),
        });
    }
    Ok(Response::Chats { chats })
}

async fn messages(
    core: &dyn Core,
    chat: &str,
    limit: Option<u32>,
    before: Option<&str>,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    let target = address::conversation(core, chat).await?;
    let limit = count(limit, MESSAGES, MAX_MESSAGES);
    let cursor = match before {
        Some(before) => Some(cursor_at(core, &target.id, before).await?),
        None => None,
    };
    let synced = if target.channel && before.is_none() {
        Some(synced(core, &target.id, cx).await)
    } else {
        None
    };
    let posts = Posts::of(core, &target).await?;
    let read = read_back(core, &target.id, cursor, limit, posts.as_ref(), |_| Keep::Row).await?;
    let end = if read.ended {
        Some(match core.history_complete(target.id.clone()).await {
            Ok(Some(true)) => HistoryEnd::Beginning,
            _ => HistoryEnd::Store,
        })
    } else {
        None
    };
    Ok(Response::Messages {
        messages: read.rows,
        synced,
        end,
    })
}

/// `thread`: the post `post` names (its own id, or a reply's) and its replies, the newest first, after a sync of the
/// channel as for `messages`.
async fn thread(
    core: &dyn Core,
    channel: &str,
    post: &str,
    limit: Option<u32>,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    let target = address::conversation(core, channel).await?;
    if !target.channel {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("{channel} is a chat: a thread is a channel's"),
        ));
    }
    let synced = synced(core, &target.id, cx).await;
    let posts = Posts::of(core, &target).await?.unwrap_or_default();
    let post = posts.post_of(post.trim()).to_owned();
    let posted = cursor_at(core, &target.id, &post).await?.arrival_time;
    let limit = count(limit, MESSAGES, MAX_MESSAGES);
    // replies come after their post: read from the newest back, the post is the last row
    let newest = read_back(core, &target.id, None, limit, Some(&posts), |message| {
        if message.id == post {
            Keep::Last
        } else if posts.post_of(&message.id) == post {
            Keep::Row
        } else if message.arrival_time < posted {
            Keep::Stop
        } else {
            Keep::Skip
        }
    })
    .await?;
    Ok(Response::Messages {
        end: newest.reached.then_some(HistoryEnd::Beginning),
        messages: newest.rows,
        synced: Some(synced),
    })
}

/// Whether the sync of `channel` worked within [`CHANNEL_SYNC`]; one that takes longer goes on in the engine.
async fn synced(core: &dyn Core, channel: &str, cx: &mut AsyncApp) -> bool {
    let executor = cx.background_executor().clone();
    let sync = async {
        core.sync_conversation(channel.to_owned())
            .await
            .map_err(|error| engine_error(&error))
    };
    super::writes::within(CHANNEL_SYNC, "", sync, &executor).await.is_ok()
}

/// What [`read_back`] does with a stored message.
enum Keep {
    /// A row, when it has a text.
    Row,
    /// Not a row: the read goes on.
    Skip,
    /// The last row: the read reached what it looked for.
    Last,
    /// Not a row, and nothing older is looked at.
    Stop,
}

/// The rows of a read and how it ended.
struct ReadBack {
    rows: Vec<MessageRow>,
    /// The store holds no older message than the ones read.
    ended: bool,
    /// The read reached a [`Keep::Last`] message.
    reached: bool,
}

/// The stored messages of `conversation` before `cursor` (the newest without it), the newest first, kept as `keep`
/// says, up to `limit` rows and while they fit on the answer's line.
async fn read_back(
    core: &dyn Core,
    conversation: &str,
    mut cursor: Option<PageCursor>,
    limit: usize,
    posts: Option<&Posts>,
    keep: impl Fn(&Message) -> Keep,
) -> Result<ReadBack, CtlError> {
    let mut names = Names::default();
    let mut rows = Fitting::new(Response::Messages {
        messages: Vec::new(),
        synced: Some(false),
        end: Some(HistoryEnd::Beginning),
    });
    while rows.rows.len() < limit {
        let page = core
            .messages(conversation.to_owned(), cursor, limit)
            .await
            .map_err(|error| engine_error(&error))?;
        for message in &page.messages {
            if rows.rows.len() == limit {
                return Ok(rows.done(false, false));
            }
            let last = match keep(message) {
                Keep::Skip => continue,
                Keep::Stop => return Ok(rows.done(false, false)),
                Keep::Row => false,
                Keep::Last => true,
            };
            if let Some(text) = text_of(message)
                && !rows.push(names.row(core, message, text, posts).await)
            {
                return Ok(rows.done(false, false));
            }
            if last {
                return Ok(rows.done(false, true));
            }
        }
        cursor = page.next;
        if cursor.is_none() {
            return Ok(rows.done(true, false));
        }
    }
    Ok(rows.done(false, false))
}

/// An answer's rows, kept while they fit on its line: the codec refuses a line over [`MAX_LINE`], and an answer that
/// can't be written leaves the client without one.
struct Fitting {
    rows: Vec<MessageRow>,
    /// The bytes still free on the line.
    room: usize,
}

impl Fitting {
    /// No rows yet, the room on the line of `empty` (the answer without any) less its frame, its id at the longest.
    fn new(empty: Response) -> Self {
        let frame = ResponseLine {
            id: u64::MAX,
            outcome: Outcome::Ok(empty),
        };
        let taken = codec::encode(&frame).map_or(MAX_LINE, |line| line.len());
        Self {
            rows: Vec::new(),
            room: MAX_LINE.saturating_sub(taken),
        }
    }

    fn done(self, ended: bool, reached: bool) -> ReadBack {
        ReadBack {
            rows: self.rows,
            ended,
            reached,
        }
    }

    /// Keeps `row` when it fits, with the comma before it; `false`, keeping nothing, when it doesn't.
    fn push(&mut self, row: MessageRow) -> bool {
        // its own line's break stands for the comma
        let size = codec::encode(&row).map_or(usize::MAX, |line| line.len());
        if size > self.room {
            return false;
        }
        self.room -= size;
        self.rows.push(row);
        true
    }
}

/// Where the page before message `id` of `chat` starts.
async fn cursor_at(core: &dyn Core, chat: &str, id: &str) -> Result<PageCursor, CtlError> {
    match core.message(chat.to_owned(), id.to_owned()).await {
        Ok(Some(message)) => Ok(PageCursor {
            arrival_time: message.arrival_time,
            id: message.id,
        }),
        Ok(None) => Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("the chat holds no message {id}"),
        )),
        Err(error) => Err(engine_error(&error)),
    }
}

/// A message's text as `messages` gives it, and whether it carries cards; `None` for one it leaves out (see the
/// module docs).
#[must_use]
pub fn text_of(message: &Message) -> Option<ReadText> {
    if message.delete_time.is_some() {
        return Some(ReadText::default());
    }
    match system_message::preview(&message.message_type, &message.content, &message.conversation_id) {
        Preview::Skipped => None,
        Preview::Said(text) => Some(ReadText { text, card: false }),
        Preview::Text if !chat_list::is_text(message) => None,
        Preview::Text => Some(plain_text(message)),
    }
}

/// A message's text, and whether it carries cards: a card message, or a text message with a link's app cards.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadText {
    pub text: String,
    pub card: bool,
}

/// A text message's whole text, read from its sanitized rich text (as it came for a `Text` message), without the
/// line breaks that end it, then the text of its cards ([`message_cards_text`]); Teams' "Sent a card" for a card
/// message whose cards give no text.
fn plain_text(message: &Message) -> ReadText {
    let cards = message_cards_text(&message.message_type, &message.content, message.properties.as_deref());
    let own = if message.message_type == CARD_TYPE {
        String::new()
    } else if message.message_type == "Text" {
        message.content.trim_end().to_owned()
    } else {
        parse_html(&message.content).plain_text().trim_end().to_owned()
    };
    let text = match cards.as_deref().filter(|cards| !cards.is_empty()) {
        Some(cards) if own.is_empty() => cards.to_owned(),
        Some(cards) => format!("{own}\n{cards}"),
        None if own.is_empty() && message.message_type == CARD_TYPE => SENT_A_CARD.to_owned(),
        None => own,
    };
    ReadText {
        text,
        card: cards.is_some() || message.message_type == CARD_TYPE,
    }
}

async fn found(core: &dyn Core, query: &str, limit: Option<u32>) -> Result<Response, CtlError> {
    if !search::searchable(query) {
        return Err(CtlError::new(ErrorCode::BadRequest, "the query is too short to search"));
    }
    let within: Vec<String> = address::listed(core)
        .await?
        .into_iter()
        .map(|conversation| conversation.id)
        .collect();
    let hits = core
        .search(query.to_owned(), within, count(limit, MAX_HITS, MAX_MESSAGES))
        .await
        .map_err(|error| engine_error(&error))?;
    let mut names = Names::default();
    let mut messages = Fitting::new(Response::Search { messages: Vec::new() });
    for hit in hits {
        let message = core
            .message(hit.conversation_id.clone(), hit.message_id.clone())
            .await
            .map_err(|error| engine_error(&error))?;
        // a hit is a stored message: one gone since is left out
        if let Some(message) = message
            && let Some(text) = text_of(&message)
            && !messages.push(names.row(core, &message, text, None).await)
        {
            break;
        }
    }
    Ok(Response::Search {
        messages: messages.rows,
    })
}

async fn members(core: &dyn Core, chat: &str) -> Result<Response, CtlError> {
    let chat = address::chat(core, chat).await?;
    let stored = core.members(chat.id).await.map_err(|error| engine_error(&error))?;
    let mut names = Names::default();
    let mut members = Vec::with_capacity(stored.len());
    for member in stored {
        members.push(MemberRow {
            name: names.name(core, &member.mri, None).await,
            mri: member.mri,
            role: member.role,
        });
    }
    Ok(Response::Members { members })
}

async fn activity(
    core: &dyn Core,
    filter: Option<&str>,
    search: Option<&str>,
    limit: Option<u32>,
) -> Result<Response, CtlError> {
    let filter = match filter.map(str::trim) {
        None | Some("") => ActivityFilter::All,
        Some(key) => ActivityFilter::of_key(key).ok_or_else(|| {
            let keys: Vec<&str> = ActivityFilter::CHIPS.iter().map(|chip| chip.key()).collect();
            CtlError::new(
                ErrorCode::BadRequest,
                format!("no activity filter is {key}: one of all, {}", keys.join(", ")),
            )
        })?,
    };
    let words = search.unwrap_or_default();
    let limit = count(limit, usize::MAX, usize::MAX);
    let feed = core.activity().await.map_err(|error| engine_error(&error))?;
    let mut rows = HashMap::new();
    let mut items = Vec::new();
    for item in feed.items.iter().filter(|item| feed.keeps(filter, item)) {
        if items.len() >= limit {
            break;
        }
        let place = place(core, item, &mut rows).await;
        if !item.matches(words)
            && !place
                .as_deref()
                .is_some_and(|place| activity_view::place_matches(place, words))
        {
            continue;
        }
        items.push(ActivityRow {
            id: item.id.clone(),
            kind: item.kind.key().to_owned(),
            title: item.title(),
            preview: Some(item.preview.clone()).filter(|preview| !preview.is_empty()),
            place,
            chat: item.thread.clone(),
            message: item.message.clone().or_else(|| item.reply_chain.clone()),
            at: item.at_ms,
            read: feed.is_read(item),
        });
    }
    Ok(Response::Activity { items })
}

/// Where an item happened, as the pane's third line says it ([`activity_view::ActivityView::place`]): a channel by the
/// name the item carries, a chat as [`activity_view::chat_place`] says it from the chat list's row of it (`rows`
/// keeps each one built).
async fn place(
    core: &dyn Core,
    item: &ActivityItem,
    rows: &mut HashMap<String, Option<chat_list::ChatRow>>,
) -> Option<String> {
    if matches!(item.kind, ActivityKind::MissedCall | ActivityKind::Voicemail) {
        return Some(activity_view::TEAMS_CALL.to_owned());
    }
    let Some(thread) = item.thread.as_deref().filter(|_| !item.in_channel()) else {
        return item.topic.clone();
    };
    if !rows.contains_key(thread) && !thread.ends_with("@unq.gbl.spaces") {
        let row = match core.conversation(thread.to_owned()).await {
            Ok(Some(conversation)) => Some(chat_list::build_row(core, &conversation).await),
            _ => None,
        };
        rows.insert(thread.to_owned(), row);
    }
    let place = activity_view::chat_place(item, rows.get(thread).and_then(Option::as_ref));
    Some(place).filter(|place| !place.is_empty())
}

async fn unread(account: &Addressed, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    let Some(tray) = &account.tray else {
        return Err(CtlError::new(
            ErrorCode::Stopped,
            "the window holds no tile of the account",
        ));
    };
    counted(tray, cx).await?;
    let (total, counts): (Option<u64>, HashMap<String, u64>) = cx.update(|cx| {
        let tray = tray.read(cx);
        let counts = tray
            .unread_chats()
            .map(|(chat, count)| (chat.to_owned(), count))
            .collect();
        (tray.total(), counts)
    });
    let total = total.ok_or_else(not_counted)?;
    let core = &*account.core;
    let mut chats = Vec::with_capacity(counts.len());
    // the chat list's order: the latest activity first
    for conversation in address::listed(core).await? {
        if let Some(count) = counts.get(&conversation.id) {
            chats.push(UnreadRow {
                title: chat_list::chat_name(core, &conversation).await.to_string(),
                chat: conversation.id,
                count: *count,
            });
        }
    }
    Ok(Response::Unread(Unread { total, chats }))
}

/// Waits until `tray` counted once, up to [`FIRST_COUNT`].
///
/// # Errors
/// [`ErrorCode::Timeout`] when it hasn't counted by then (its first count runs a moment after the tile is built; one
/// that failed is tried again only once a chat changes).
async fn counted(tray: &Entity<Tray>, cx: &mut AsyncApp) -> Result<(), CtlError> {
    let (done, finished) = async_channel::bounded::<bool>(1);
    let observing = cx.update(|cx| {
        if tray.read(cx).total().is_some() {
            return None;
        }
        let done = done.clone();
        Some(cx.observe(tray, move |tray, cx| {
            if tray.read(cx).total().is_some() {
                let _ = done.try_send(true);
            }
        }))
    });
    let Some(_observing) = observing else {
        return Ok(());
    };
    let timer = cx.background_executor().timer(FIRST_COUNT);
    // dropped (cancelled) once the count came first
    let _timeout = cx.background_executor().spawn(async move {
        timer.await;
        let _ = done.try_send(false);
    });
    match finished.recv().await {
        Ok(true) => Ok(()),
        _ => Err(not_counted()),
    }
}

fn not_counted() -> CtlError {
    CtlError::new(ErrorCode::Timeout, "the unread badge hasn't counted yet")
}

/// People's names as the engine has them, each asked once.
#[derive(Default)]
struct Names(HashMap<String, Option<String>>);

impl Names {
    /// `mri`'s name: the engine's, else `carried` (the one a message of theirs carried).
    async fn name(&mut self, core: &dyn Core, mri: &str, carried: Option<&str>) -> Option<String> {
        if !self.0.contains_key(mri) {
            let name = core.person(mri.to_owned()).await.ok().and_then(|person| person.name);
            self.0.insert(mri.to_owned(), name);
        }
        self.0
            .get(mri)
            .cloned()
            .flatten()
            .or_else(|| carried.map(str::to_owned))
            .filter(|name| !name.trim().is_empty())
    }

    /// `message` as a row, its text `text`; a channel's (`posts`) with its post or its replies, its subject and its
    /// email.
    async fn row(&mut self, core: &dyn Core, message: &Message, text: ReadText, posts: Option<&Posts>) -> MessageRow {
        let me = core.me();
        let sender_name = match &message.sender {
            Some(sender) => self.name(core, sender, message.sender_name.as_deref()).await,
            None => message.sender_name.clone(),
        };
        MessageRow {
            id: message.id.clone(),
            chat: message.conversation_id.clone(),
            sender: message.sender.clone(),
            sender_name,
            at: message.arrival_time,
            text: text.text,
            card: text.card,
            edited: message.edit_time.is_some(),
            deleted: message.delete_time.is_some(),
            from_me: message.sender.as_deref() == Some(me),
            mentions_me: Mentions::of_message(message, me).names_me(),
            attachments: super::download::attachments(message)
                .into_iter()
                .map(|attachment| attachment.row)
                .collect(),
            ..posts.map(|posts| posts.parts(message)).unwrap_or_default()
        }
    }
}

/// What a channel's rows carry besides a chat's: the post of each stored reply, and how many replies each post has.
#[derive(Default)]
struct Posts {
    /// Each stored reply's post, by the reply's id.
    roots: HashMap<String, String>,
    replies: HashMap<String, u32>,
}

impl Posts {
    /// The posts of `target`'s messages when it is a channel; `None` for a chat.
    async fn of(core: &dyn Core, target: &Target) -> Result<Option<Self>, CtlError> {
        if !target.channel {
            return Ok(None);
        }
        let roots: HashMap<String, String> = core
            .reply_roots(target.id.clone())
            .await
            .map_err(|error| engine_error(&error))?
            .into_iter()
            .filter(|(message, post)| message != post)
            .collect();
        let mut replies: HashMap<String, u32> = HashMap::new();
        for post in roots.values() {
            *replies.entry(post.clone()).or_default() += 1;
        }
        Ok(Some(Self { roots, replies }))
    }

    /// The post that message `id` belongs to: its own id for a post.
    fn post_of<'a>(&'a self, id: &'a str) -> &'a str {
        self.roots.get(id).map_or(id, String::as_str)
    }

    /// The channel's fields of `message`'s row; the rest left as a chat's.
    fn parts(&self, message: &Message) -> MessageRow {
        let post = self.roots.get(&message.id).cloned();
        let replies = post
            .is_none()
            .then(|| self.replies.get(&message.id).copied().unwrap_or_default());
        let properties = message
            .properties
            .as_deref()
            .and_then(|text| serde_json::from_str::<Map<String, Value>>(text).ok())
            .unwrap_or_default();
        MessageRow {
            post,
            replies,
            subject: teams_proto::chat::subject(&properties),
            email: EmailDetails::of_properties(&properties).map(|details| email_row(&details)),
            ..MessageRow::default()
        }
    }
}

/// The email a post came from, as the wire tells it.
fn email_row(details: &EmailDetails) -> EmailRow {
    let people = |users: &[EmailUser]| users.iter().map(email_person).collect();
    EmailRow {
        from: details.from.as_ref().map(email_person),
        sender: details
            .sender
            .as_ref()
            .filter(|sender| Some(*sender) != details.from.as_ref())
            .map(email_person),
        to: people(&details.to),
        cc: people(&details.cc),
        recipients: u32::try_from(details.total_recipients).unwrap_or(u32::MAX),
    }
}

fn email_person(user: &EmailUser) -> EmailPerson {
    EmailPerson {
        name: user.name.clone(),
        address: user.smtp.clone(),
    }
}

/// `teams`: the account's teams as the Teams list shows them, waiting for their first read when the store holds none.
async fn teams(account: &Addressed, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    let core = &*account.core;
    let deadline = std::time::Instant::now() + TEAMS_ANSWER;
    loop {
        let told = cx.update(|cx| told_at(&account.chats, cx));
        if let Some(teams) = core.teams().await.map_err(|error| engine_error(&error))? {
            return Ok(Response::Teams {
                teams: teams.iter().map(team_row).collect(),
            });
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() || !teams_told(&account.chats, told, left, cx).await {
            return Err(CtlError::new(
                ErrorCode::Timeout,
                "the teams aren't read yet: ask again in a moment",
            ));
        }
        if cx.update(|cx| account.chats.read(cx).teams_failed_at() > account.chats.read(cx).teams_at()) {
            return Err(CtlError::new(ErrorCode::Engine, "the teams can't be read right now"));
        }
    }
}

/// The revisions of the models' last teams change and last failed teams read.
fn told_at(chats: &Entity<Chats>, cx: &gpui::App) -> (u64, u64) {
    let chats = chats.read(cx);
    (chats.teams_at(), chats.teams_failed_at())
}

/// Waits until the models tell a teams change or a failed read after `told`, up to `left`: `false` when none came.
async fn teams_told(chats: &Entity<Chats>, told: (u64, u64), left: Duration, cx: &mut AsyncApp) -> bool {
    let (done, finished) = async_channel::bounded::<bool>(1);
    let observing = cx.update(|cx| {
        if told_at(chats, cx) != told {
            return None;
        }
        let done = done.clone();
        Some(cx.observe(chats, move |chats, cx| {
            if told_at(&chats, cx) != told {
                let _ = done.try_send(true);
            }
        }))
    });
    let Some(_observing) = observing else {
        return true;
    };
    let timer = cx.background_executor().timer(left);
    // dropped (cancelled) once the teams were told first
    let _timeout = cx.background_executor().spawn(async move {
        timer.await;
        let _ = done.try_send(false);
    });
    finished.recv().await.unwrap_or(false)
}

fn team_row(team: &Team) -> TeamRow {
    TeamRow {
        id: team.id.clone(),
        name: team.name.clone(),
        archived: team.archived,
        channels: team
            .channels
            .iter()
            .map(|channel| ChannelRow {
                id: channel.id.clone(),
                name: channel.name.clone(),
                title: address::channel_title(&team.name, &channel.name),
                general: channel.general,
                channel_type: match channel.kind {
                    ChannelKind::Standard => "standard",
                    ChannelKind::Private => "private",
                    ChannelKind::Shared => "shared",
                }
                .to_owned(),
                hidden: !channel.shown,
                archived: channel.about.archived,
            })
            .collect(),
    }
}
