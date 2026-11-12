//! The read commands, each through the addressed account's [`Core`] as the views read, mapped to the wire's rows.
//!
//! - `chats`: the chats the chat list lists, the latest activity first, each named, previewed and counted as its row
//!   is ([`chat_list::build_row`]); `unread_only` keeps those with anything unread.
//! - `messages`: a chat's messages, the newest first, as plain text: the text messages and the lines Teams says in a
//!   chat ("Meeting started", …) as the chat list's preview reads them ([`system_message::preview`]); the rest
//!   (a thread's events, a call's records) left out. `before` pages back from a message. The answer stops before a
//!   message that would take its line over the wire's cap ([`MAX_LINE`]): fewer than asked, the client pages on from
//!   the last one.
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

use gpui::{AsyncApp, Entity};
use teams_core::activity::{ActivityFilter, ActivityItem, ActivityKind};
use teams_ctl::codec::{self, MAX_LINE};
use teams_ctl::protocol::{
    ActivityRow, ChatRow, CtlError, ErrorCode, MemberRow, MessageRow, Outcome, Request, Response, ResponseLine, Unread,
    UnreadRow,
};
use teams_proto::chat::parse_html;
use teams_store::{Conversation, Message, PageCursor};
use teams_ui::activity_view;
use teams_ui::card_view::SENT_A_CARD;
use teams_ui::chat_list;
use teams_ui::rich_text::Mentions;
use teams_ui::search::{self, MAX_HITS};
use teams_ui::system_message::{self, Preview};
use teams_ui::{Core, Tray};

use super::address::{self, Addressed, engine_error};

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
        Request::Messages { chat, limit, before } => messages(core, chat, *limit, before.as_deref()).await,
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

async fn messages(core: &dyn Core, chat: &str, limit: Option<u32>, before: Option<&str>) -> Result<Response, CtlError> {
    let chat = address::chat(core, chat).await?;
    let limit = count(limit, MESSAGES, MAX_MESSAGES);
    let mut cursor = match before {
        Some(before) => Some(cursor_at(core, &chat, before).await?),
        None => None,
    };
    let mut names = Names::default();
    let mut messages = Fitting::new(Response::Messages { messages: Vec::new() });
    'pages: while messages.rows.len() < limit {
        let page = core
            .messages(chat.id.clone(), cursor, limit)
            .await
            .map_err(|error| engine_error(&error))?;
        for message in &page.messages {
            if messages.rows.len() == limit {
                break 'pages;
            }
            if let Some(text) = text_of(message)
                && !messages.push(names.row(core, message, text).await)
            {
                break 'pages;
            }
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(Response::Messages {
        messages: messages.rows,
    })
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
async fn cursor_at(core: &dyn Core, chat: &Conversation, id: &str) -> Result<PageCursor, CtlError> {
    match core.message(chat.id.clone(), id.to_owned()).await {
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

/// A message's text as `messages` gives it, or `None` for one it leaves out (see the module docs).
#[must_use]
pub fn text_of(message: &Message) -> Option<String> {
    if message.delete_time.is_some() {
        return Some(String::new());
    }
    match system_message::preview(&message.message_type, &message.content, &message.conversation_id) {
        Preview::Skipped => None,
        Preview::Said(said) => Some(said),
        Preview::Text if !chat_list::is_text(message) => None,
        Preview::Text => Some(plain_text(message)),
    }
}

/// A text message's whole text, read from its sanitized rich text (as it came for a `Text` message), without the
/// line breaks that end it; Teams' "Sent a card" for a card.
fn plain_text(message: &Message) -> String {
    if message.message_type == teams_proto::chat::card::CARD_TYPE {
        SENT_A_CARD.to_owned()
    } else if message.message_type == "Text" {
        message.content.trim_end().to_owned()
    } else {
        parse_html(&message.content).plain_text().trim_end().to_owned()
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
            && !messages.push(names.row(core, &message, text).await)
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
    let mut places = HashMap::new();
    let mut items = Vec::new();
    for item in feed.items.iter().filter(|item| feed.keeps(filter, item)) {
        if items.len() >= limit {
            break;
        }
        let place = place(core, item, &mut places).await;
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

/// Where an item happened, as the pane's third line says it ([`activity_view::ActivityView::place`]): a channel by the name the item
/// carries, a group chat by the chat list's name of it (`places` keeps each one found).
async fn place(core: &dyn Core, item: &ActivityItem, places: &mut HashMap<String, Option<String>>) -> Option<String> {
    if matches!(item.kind, ActivityKind::MissedCall | ActivityKind::Voicemail) {
        return Some(activity_view::TEAMS_CALL.to_owned());
    }
    let Some(thread) = item.thread.as_deref().filter(|_| !item.in_channel()) else {
        return item.topic.clone();
    };
    if thread.ends_with("@unq.gbl.spaces") {
        return Some(activity_view::IN_CHAT_WITH_YOU.to_owned());
    }
    if !places.contains_key(thread) {
        let name = match core.conversation(thread.to_owned()).await {
            Ok(Some(conversation)) => Some(chat_list::build_row(core, &conversation).await.name.to_string()),
            _ => None,
        };
        places.insert(thread.to_owned(), name.filter(|name| !name.is_empty()));
    }
    places.get(thread).cloned().flatten().or_else(|| item.topic.clone())
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

    /// `message` as a row, its text `text`.
    async fn row(&mut self, core: &dyn Core, message: &Message, text: String) -> MessageRow {
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
            text,
            edited: message.edit_time.is_some(),
            deleted: message.delete_time.is_some(),
            from_me: message.sender.as_deref() == Some(me),
            mentions_me: Mentions::of_message(message, me).names_me(),
        }
    }
}
