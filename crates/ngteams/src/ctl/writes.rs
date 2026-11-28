//! The message writes, each through the addressed account's [`Core::write`] as the views write: the outbox makes the
//! row, sends it and applies its retries and its unknown-outcome checks as for the window's own writes. Nothing here
//! builds a request.
//!
//! - `send`: plain text, each line a paragraph, made into the message's text as the compose box makes it
//!   ([`compose::message_of`] of [`compose::plain_doc`]), so its sanitizing and its length limit hold. Text that shows
//!   nothing is refused, as the box sends nothing then; a text `teams-proto` refuses (too long, …) is refused with
//!   its reason.
//! - `edit`: one of the account's messages, its text replaced the same way, its quote of another message (a reply's,
//!   a forward's) kept as the box keeps it.
//! - `delete`, `react`, `unreact`: a message by its id, a reaction by its key (`like`, `heart`, …).
//! - `read`: the chat marked read up to its newest message a read receipt can name (someone else's, counted unread,
//!   not deleted), as the chat view sends one; nothing to do (`done`) when the account read that far already. A
//!   channel is refused, as the chat view sends no receipt there (no capture showed a channel's).
//! - `new-chat`: the people by MRI, or by UPN as the service's people search finds them (New message's directory
//!   suggestions); their 1:1 chat for one, a new group chat for more. The row names the chat made once it went out.
//! - `resend`: an outbox row sent again ([`Core::resend`]), as the window's "Send again" does.
//! - `status get`: what the account's own presence reports ([`reporting_name`]) and the status chosen in the
//!   settings. `status set`: chosen as the account menu's status menu chooses it (`Shell::choose_status`), its own
//!   presence told and the choice saved in the settings, which keep it across the app's runs and re-apply it on any
//!   other change of the settings; `reset` is Automatic again. The report goes out through the engine as the menu's
//!   does, and the answer is the status as the choice left it (a report that fails shows as `not-set`).
//! - `sync`: a catch-up now ([`Core::sync`]), answered with what it did, [`SYNC_ANSWER`] at most. A read, so the
//!   read-only mode runs it too.
//!
//! Each answers with its outbox row as its first attempt left it, [`WRITE_ANSWER`] at most (past it, a timeout saying
//! the write may still go out); asked to `wait`, once the row settled (sent, failed, or one that may have arrived,
//! which nothing sends again on its own), [`WRITE_WAIT`] more at most. The read-only mode
//! refuses every write ([`ErrorCode::RefusedReadOnly`]), as its transport would; `status set` too.

use std::future::{self, Future};
use std::pin::{Pin, pin};
use std::task::Poll;

use gpui::{App, AsyncApp, BackgroundExecutor, Entity};
use teams_core::{Action, CatchUpReport, SearchScope};
use teams_ctl::protocol::{
    self, CtlError, ErrorCode, OutboxRow, OwnStatus, Request, Response, SYNC_ANSWER, StatusChoice, SyncReport,
    UNANSWERED_WRITE, WRITE_ANSWER, WRITE_WAIT,
};
use teams_proto::chat::{MessageText, Node};
use teams_proto::{Mri, SearchKind};
use teams_store::{Conversation, ConversationKind, OutboxEntry, OutboxKind, OutboxState, counts_unread};
use teams_ui::settings::ManualStatus;
use teams_ui::{Chats, Core, compose};

use super::address::{self, Addressed, engine_error};
use super::events::outbox_row;
use super::watch::reporting_name;
use crate::app::Running;

/// How many of a chat's stored messages `read` reads at a time, looking for the one to mark read up to.
pub const READ_PAGE: usize = 50;

/// Whether `request` is a message write (see the module docs).
#[must_use]
pub fn serves(request: &Request) -> bool {
    matches!(
        request,
        Request::Send { .. }
            | Request::Edit { .. }
            | Request::Delete { .. }
            | Request::React { .. }
            | Request::Unreact { .. }
            | Request::MarkRead { .. }
            | Request::NewChat { .. }
            | Request::Resend { .. }
    )
}

/// Whether `request` asks to wait for its row to settle.
fn waits(request: &Request) -> bool {
    match request {
        Request::Send { wait, .. }
        | Request::Edit { wait, .. }
        | Request::Delete { wait, .. }
        | Request::React { wait, .. }
        | Request::Unreact { wait, .. }
        | Request::MarkRead { wait, .. }
        | Request::NewChat { wait, .. }
        | Request::Resend { wait, .. } => *wait,
        _ => false,
    }
}

/// `request`, a message write, answered for `account` (see the module docs).
///
/// # Errors
/// The read-only mode, the chat or message named isn't one, the text shows nothing or isn't one Teams takes, a person
/// isn't found, the engine refused or failed ([`engine_error`]), or the row didn't settle in time.
pub async fn answer(request: &Request, account: &Addressed, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    let core = &*account.core;
    if core.read_only() {
        return Err(CtlError::new(
            ErrorCode::RefusedReadOnly,
            "the read-only app writes nothing",
        ));
    }
    let executor = cx.background_executor().clone();
    let Some(entry) = within_answer(written(core, request), &executor).await? else {
        return Ok(Response::Done);
    };
    let entry = if waits(request) {
        settled(&account.chats, entry, cx).await?
    } else {
        entry
    };
    Ok(Response::Outbox(row_of(&entry)))
}

/// `write`'s answer, unless it takes longer than [`WRITE_ANSWER`] (an engine busy with something else first, a
/// network that answers slowly): then [`ErrorCode::Timeout`], saying the write may still go out. Its engine request
/// isn't taken back: the engine sends the row whether or not anyone still waits for its answer.
///
/// # Errors
/// `write`'s own, or the timeout.
pub async fn within_answer<T>(
    write: impl Future<Output = Result<T, CtlError>>,
    executor: &BackgroundExecutor,
) -> Result<T, CtlError> {
    let late = format!(
        "the write wasn't answered in {}s: it may still go out. {UNANSWERED_WRITE}",
        WRITE_ANSWER.as_secs()
    );
    within(WRITE_ANSWER, &late, write, executor).await
}

/// `work`'s answer, unless it takes longer than `bound`: then [`ErrorCode::Timeout`] saying `late`. The work goes on.
pub(super) async fn within<T>(
    bound: std::time::Duration,
    late: &str,
    work: impl Future<Output = Result<T, CtlError>>,
    executor: &BackgroundExecutor,
) -> Result<T, CtlError> {
    let timer = pin!(executor.timer(bound));
    before(timer, work)
        .await
        .unwrap_or_else(|| Err(CtlError::new(ErrorCode::Timeout, late)))
}

/// `work`'s output, or `None` when `deadline` comes first. The work goes on.
pub(super) async fn before<T>(
    mut deadline: Pin<&mut impl Future<Output = ()>>,
    work: impl Future<Output = T>,
) -> Option<T> {
    let mut work = pin!(work);
    future::poll_fn(|task| {
        if let Poll::Ready(output) = work.as_mut().poll(task) {
            return Poll::Ready(Some(output));
        }
        deadline.as_mut().poll(task).map(|()| None)
    })
    .await
}

/// Whether `request` is `status get` or `status set` (see the module docs).
#[must_use]
pub fn serves_status(request: &Request) -> bool {
    matches!(request, Request::GetStatus | Request::SetStatus { .. })
}

/// `status get` or `status set` for the account `wanted` names (see the module docs).
///
/// # Errors
/// The account isn't one, or its engine stopped ([`address::account`]); `status set` in the read-only mode.
pub fn own_status(
    running: &Entity<Running>,
    wanted: Option<&str>,
    request: &Request,
    cx: &mut App,
) -> Result<Response, CtlError> {
    let (mri, own, settings) = {
        let running = running.read(cx);
        let account = address::account(running, wanted, cx)?;
        if matches!(request, Request::SetStatus { .. }) && account.core.read_only() {
            return Err(CtlError::new(
                ErrorCode::RefusedReadOnly,
                "the read-only app reports no status",
            ));
        }
        let desktop = running.desktop().read(cx);
        let own = desktop.own_presence_of(&account.mri).cloned().ok_or_else(|| {
            CtlError::new(
                ErrorCode::Stopped,
                format!("{} isn't in the app's window yet", account.upn),
            )
        })?;
        (account.mri, own, desktop.settings().clone())
    };
    if let Request::SetStatus { status } = request {
        let status = manual_status(*status);
        own.update(cx, |own, cx| own.choose(status.choice(), cx));
        // unchanged (the same status chosen again), the settings tell nobody: the own presence was told above
        settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.set_manual_status(&mri, status), cx);
        });
    }
    Ok(Response::OwnStatus(OwnStatus {
        reported: reporting_name(own.read(cx).reporting(cx)).to_owned(),
        manual: wire_manual(settings.read(cx).settings().manual_status_of(&mri)),
    }))
}

/// The settings' status of a status menu's choice.
#[must_use]
pub fn manual_status(choice: StatusChoice) -> ManualStatus {
    match choice {
        StatusChoice::Available => ManualStatus::Available,
        StatusChoice::Busy => ManualStatus::Busy,
        StatusChoice::Dnd => ManualStatus::DoNotDisturb,
        StatusChoice::Away => ManualStatus::Away,
        StatusChoice::Reset => ManualStatus::Automatic,
    }
}

fn wire_manual(status: ManualStatus) -> protocol::ManualStatus {
    match status {
        ManualStatus::Automatic => protocol::ManualStatus::Automatic,
        ManualStatus::Available => protocol::ManualStatus::Available,
        ManualStatus::Busy => protocol::ManualStatus::Busy,
        ManualStatus::DoNotDisturb => protocol::ManualStatus::Dnd,
        ManualStatus::Away => protocol::ManualStatus::Away,
    }
}

/// `sync`: a catch-up of `account` now, answered with what it did (see the module docs).
///
/// # Errors
/// The engine's ([`engine_error`]), or the catch-up took longer than [`SYNC_ANSWER`] (it goes on).
pub async fn sync(account: &Addressed, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    let executor = cx.background_executor().clone();
    let late = format!(
        "the catch-up didn't end in {}s: it goes on in the app",
        SYNC_ANSWER.as_secs()
    );
    let caught_up = async { account.core.sync().await.map_err(|error| engine_error(&error)) };
    let report = within(SYNC_ANSWER, &late, caught_up, &executor).await?;
    Ok(Response::Synced(sync_report(&report)))
}

/// What a catch-up did, as the wire tells it: its messages are those it stored new or changed.
#[must_use]
pub fn sync_report(report: &CatchUpReport) -> SyncReport {
    let count = |count: usize| u64::try_from(count).unwrap_or(u64::MAX);
    SyncReport {
        conversations: count(report.conversations),
        messages: count(report.messages.inserted.saturating_add(report.messages.updated)),
        failed: count(report.failed.len()),
        unreadable: count(report.unreadable.len()),
        caught_up: report.caught_up(),
    }
}

/// The row `request` made; none for a `read` with nothing to mark.
async fn written(core: &dyn Core, request: &Request) -> Result<Option<OutboxEntry>, CtlError> {
    let action = match request {
        Request::Send { chat, text, .. } => Action::Send {
            conversation: address::chat(core, chat).await?.id,
            text: message_text(text, &[])?,
        },
        Request::Edit {
            chat, message, text, ..
        } => edit(core, chat, message, text).await?,
        Request::Delete { chat, message, .. } => Action::Delete {
            conversation: address::chat(core, chat).await?.id,
            message_id: message.clone(),
        },
        Request::React {
            chat,
            message,
            reaction,
            ..
        } => Action::React {
            conversation: address::chat(core, chat).await?.id,
            message_id: message.clone(),
            key: reaction_key(reaction)?,
        },
        Request::Unreact {
            chat,
            message,
            reaction,
            ..
        } => Action::Unreact {
            conversation: address::chat(core, chat).await?.id,
            message_id: message.clone(),
            key: reaction_key(reaction)?,
        },
        Request::MarkRead { chat, .. } => match read_upto(core, chat).await? {
            Some(action) => action,
            None => return Ok(None),
        },
        Request::NewChat { people, .. } => Action::CreateChat {
            others: others(core, people).await?,
            topic: None,
        },
        Request::Resend { row, .. } => return core.resend(*row).await.map(Some).map_err(|error| engine_error(&error)),
        _ => return Err(CtlError::new(ErrorCode::BadRequest, "not a message write")),
    };
    core.write(action).await.map(Some).map_err(|error| engine_error(&error))
}

/// `text`, plain, as a message's text after `quotes`, as the compose box makes it.
///
/// # Errors
/// [`ErrorCode::BadRequest`]: the text shows nothing, or `teams-proto` refuses it (too long, …).
pub fn message_text(text: &str, quotes: &[Node]) -> Result<MessageText, CtlError> {
    match compose::message_of(&compose::plain_doc(text), quotes) {
        Ok(Some(text)) => Ok(text),
        Ok(None) => Err(CtlError::new(
            ErrorCode::BadRequest,
            "the text is empty: nothing to send",
        )),
        Err(error) => Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("Teams doesn't take that text: {error}"),
        )),
    }
}

/// The edit of the account's message `id` of `chat` to `text`, its quote kept.
async fn edit(core: &dyn Core, chat: &str, id: &str, text: &str) -> Result<Action, CtlError> {
    let chat = address::chat(core, chat).await?;
    let message = core
        .message(chat.id.clone(), id.to_owned())
        .await
        .map_err(|error| engine_error(&error))?
        .ok_or_else(|| no_message(id))?;
    let quotes = compose::quotes_of(&compose::rich_text_of(&message));
    Ok(Action::Edit {
        conversation: chat.id,
        message_id: message.id,
        text: message_text(text, &quotes)?,
    })
}

fn no_message(id: &str) -> CtlError {
    CtlError::new(ErrorCode::BadRequest, format!("the chat holds no message {id}"))
}

fn reaction_key(reaction: &str) -> Result<String, CtlError> {
    let key = reaction.trim();
    if key.is_empty() {
        return Err(CtlError::new(ErrorCode::BadRequest, "name a reaction (like, heart, …)"));
    }
    Ok(key.to_owned())
}

/// The read receipt that marks `chat` read up to its newest message one can name, unless the account read that far
/// already (see the module docs). A channel's is refused, as the chat view sends none: no capture showed one.
async fn read_upto(core: &dyn Core, chat: &str) -> Result<Option<Action>, CtlError> {
    let chat = address::chat(core, chat).await?;
    if chat.kind == ConversationKind::Channel || teams_proto::is_team_thread_id(&chat.id) {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            "a channel isn't marked read from here, only a chat",
        ));
    }
    let horizon = core
        .read_horizon(chat.id.clone())
        .await
        .map_err(|error| engine_error(&error))?
        .map(|horizon| horizon.last_read_arrival);
    Ok(newest_unread(core, &chat, horizon)
        .await?
        .map(|message_id| Action::Read {
            conversation: chat.id,
            message_id,
        }))
}

/// The newest message of `chat` a read receipt can name: someone else's, counted unread, not deleted, carrying the
/// `clientmessageid` a receipt names, arrived after `horizon`. The stored pages are read down to the horizon, so a run
/// of newer messages that don't count (thread events, deleted ones) doesn't hide an older one that does.
async fn newest_unread(core: &dyn Core, chat: &Conversation, horizon: Option<i64>) -> Result<Option<String>, CtlError> {
    let me = core.me();
    let mut before = None;
    loop {
        let page = core
            .messages(chat.id.clone(), before, READ_PAGE)
            .await
            .map_err(|error| engine_error(&error))?;
        for message in page.messages {
            if horizon.is_some_and(|horizon| message.arrival_time <= horizon) {
                return Ok(None);
            }
            if message.sender.as_deref().is_some_and(|sender| sender != me)
                && counts_unread(&chat.id, &message.message_type)
                && message.delete_time.is_none()
                && message.client_message_id.is_some()
            {
                return Ok(Some(message.id));
            }
        }
        let Some(next) = page.next else {
            return Ok(None);
        };
        before = Some(next);
    }
}

/// The people a new chat is with, by their MRIs (see the module docs).
///
/// # Errors
/// [`ErrorCode::BadRequest`] for nobody named, or a UPN the people search doesn't find; the engine's.
async fn others(core: &dyn Core, people: &[String]) -> Result<Vec<Mri>, CtlError> {
    if people.iter().all(|person| person.trim().is_empty()) {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            "name the people to start a chat with",
        ));
    }
    let mut others = Vec::with_capacity(people.len());
    for person in people
        .iter()
        .map(|person| person.trim())
        .filter(|person| !person.is_empty())
    {
        others.push(person_mri(core, person).await?);
    }
    Ok(others)
}

/// `person`'s MRI: one given as it is (`8:…`), a UPN found by the service's people search.
async fn person_mri(core: &dyn Core, person: &str) -> Result<Mri, CtlError> {
    if !person.contains('@') {
        return Ok(Mri::new(person));
    }
    let found = core
        .search_service(person.to_owned(), SearchScope::Only(SearchKind::People))
        .await
        .map_err(|error| engine_error(&error))?;
    found
        .people
        .into_iter()
        .find(|found| {
            found
                .email
                .as_deref()
                .is_some_and(|email| email.eq_ignore_ascii_case(person))
        })
        .and_then(|found| found.mri)
        .map(|mri| Mri::new(&mri))
        .ok_or_else(|| {
            CtlError::new(
                ErrorCode::BadRequest,
                format!("no one is {person}: name them by their MRI"),
            )
        })
}

/// Whether nothing more happens to `entry` on its own: sent, failed for good, or one that may have arrived (only an
/// explicit resend sends it again).
#[must_use]
pub fn is_settled(entry: &OutboxEntry) -> bool {
    matches!(
        entry.state,
        OutboxState::Sent | OutboxState::Denied | OutboxState::Failed
    ) || entry.may_have_arrived()
}

/// `entry` once it settled, as the engine tells its row to the account's models, [`WRITE_WAIT`] at most.
///
/// # Errors
/// [`ErrorCode::Timeout`], naming where the row stands then.
pub async fn settled(chats: &Entity<Chats>, entry: OutboxEntry, cx: &mut AsyncApp) -> Result<OutboxEntry, CtlError> {
    if is_settled(&entry) {
        return Ok(entry);
    }
    let id = entry.id;
    let (done, finished) = async_channel::bounded::<Option<OutboxEntry>>(1);
    let told = move |chats: &Chats| chats.outbox_row(id).filter(|row| is_settled(row)).cloned();
    let (already, _observing) = cx.update(|cx| {
        if let Some(row) = told(chats.read(cx)) {
            return (Some(row), None);
        }
        let done = done.clone();
        let observing = cx.observe(chats, move |chats, cx| {
            if let Some(row) = told(chats.read(cx)) {
                let _ = done.try_send(Some(row));
            }
        });
        (None, Some(observing))
    });
    if let Some(row) = already {
        return Ok(row);
    }
    let timer = cx.background_executor().timer(WRITE_WAIT);
    // dropped (cancelled) once the row settled first
    let _timeout = cx.background_executor().spawn(async move {
        timer.await;
        let _ = done.try_send(None);
    });
    if let Ok(Some(row)) = finished.recv().await {
        return Ok(row);
    }
    let last = cx.update(|cx| chats.read(cx).outbox_row(id).cloned()).unwrap_or(entry);
    Err(CtlError::new(
        ErrorCode::Timeout,
        format!(
            "outbox row {id} hasn't settled in {}s: it is {}",
            WRITE_WAIT.as_secs(),
            state_name(&row_of(&last))
        ),
    ))
}

fn state_name(row: &OutboxRow) -> String {
    format!("{:?}", row.state).to_lowercase()
}

/// `entry` as the wire tells it: a new chat's row names the chat once it went out (its answer names the thread).
#[must_use]
pub fn row_of(entry: &OutboxEntry) -> OutboxRow {
    let mut row = outbox_row(entry);
    if entry.kind == OutboxKind::CreateChat && row.chat.is_none() && entry.state == OutboxState::Sent {
        row.chat.clone_from(&row.message);
    }
    row
}
