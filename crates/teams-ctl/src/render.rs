//! `ngteamsctl`'s human-readable output: tables for rows (accounts, chats, messages, …), `key: value` lines for one
//! record, one line per event, and an error with an ambiguous chat's candidates.
//!
//! Times are local (`YYYY-MM-DD HH:MM` in tables, `HH:MM:SS` on event lines). A message's line breaks are shown as
//! ` ↵ `, so each message keeps to its row. Any other control character is shown escaped (`\u{1b}`), so text from
//! the app never drives the terminal. `--json` skips all of this ([`crate::cli`]): JSON escapes them itself.

use std::fmt::Write as _;
use std::io::{self, Write};

use chrono::{DateTime, Local};

use crate::protocol::{
    AccountRow, ActivityRow, AppStatus, CalendarRow, CallRow, ChatRow, CtlError, CtlEvent, EventLine, MemberRow,
    MessageRow, OutboxRow, OutboxState, OwnStatus, PersonRow, ReactionRow, Response, SyncReport, UiState, Unread, View,
    WindowState,
};

/// Print `response` for a person.
///
/// # Errors
/// The write's own error.
pub fn response(out: &mut dyn Write, response: &Response) -> io::Result<()> {
    out.write_all(text(response).as_bytes())
}

/// `response` as the lines printed for it (empty for a plain `done`), every control character in it escaped but the
/// line ends.
#[must_use]
pub fn text(response: &Response) -> String {
    shown(&unescaped(response), true)
}

fn unescaped(response: &Response) -> String {
    match response {
        Response::Hello(hello) => format!("ngteams {} (protocol {})\n", hello.app, hello.protocol),
        Response::Status(status) => app_status(status),
        Response::Accounts { accounts } => account_table(accounts),
        Response::Chats { chats } => chat_table(chats),
        Response::Messages { messages } => message_table(messages, false),
        Response::Search { messages } => message_table(messages, true),
        Response::Unread(unread) => unread_table(unread),
        Response::Members { members } => member_table(members),
        Response::Activity { items } => activity_table(items),
        Response::UiState(state) => ui_state(state),
        Response::Dispatched { action } => format!("dispatched {action}\n"),
        Response::Actions { actions } => actions.iter().fold(String::new(), |mut text, action| {
            let _ = writeln!(text, "{action}");
            text
        }),
        Response::Subscribed | Response::Done => String::new(),
        Response::Outbox(row) => format!("{}\n", outbox(row)),
        Response::OwnStatus(status) => own_status(status),
        Response::Synced(report) => synced(report),
        Response::Calls { calls } => call_table(calls),
        Response::Call(call) => call_table(std::slice::from_ref(call)),
        Response::Calendar { events } => calendar_table(events),
        Response::CalendarEvent(event) => calendar_event(event),
    }
}

/// Tell an error answer on `err`: its code and message, and an ambiguous chat's candidates.
///
/// # Errors
/// The write's own error.
pub fn error(err: &mut dyn Write, error: &CtlError) -> io::Result<()> {
    writeln!(err, "ngteamsctl: {}: {}", error.code, shown(&error.message, false))?;
    for candidate in &error.candidates {
        writeln!(
            err,
            "  {}  {}",
            shown(&candidate.id, false),
            shown(&candidate.title, false)
        )?;
    }
    Ok(())
}

/// Print the event kinds, one a line (`lagged`, the stream's own, left out).
///
/// # Errors
/// The write's own error.
pub fn kinds(out: &mut dyn Write) -> io::Result<()> {
    for kind in CtlEvent::KINDS.iter().filter(|kind| **kind != "lagged") {
        writeln!(out, "{kind}")?;
    }
    Ok(())
}

/// One event's line: its local time, account, kind and what it carries.
#[must_use]
pub fn event(line: &EventLine) -> String {
    let account = line.account.as_deref().unwrap_or("-");
    let detail = event_detail(&line.event);
    let head = format!("{} {account} {}", clock(line.at), line.event.kind());
    let line = if detail.is_empty() {
        head
    } else {
        format!("{head} {detail}")
    };
    shown(&line, false)
}

fn event_detail(event: &CtlEvent) -> String {
    match event {
        CtlEvent::MessageEdited { chat, message } | CtlEvent::MessageDeleted { chat, message } => {
            format!("{chat} {message}")
        }
        CtlEvent::Reaction {
            chat,
            message,
            reactions,
        } => match reactions {
            Some(reactions) => format!("{chat} {message} {}", reaction_list(reactions)),
            None => format!("{chat} {message}"),
        },
        CtlEvent::Typing { chat, who, on } => {
            format!(
                "{chat} {} {}",
                who.as_deref().unwrap_or("?"),
                if *on { "on" } else { "off" }
            )
        }
        CtlEvent::Read { chat } | CtlEvent::ChatChanged { chat } => chat.clone(),
        CtlEvent::Unread { chat, count, total } => {
            format!("{} count={count} total={total}", chat.as_deref().unwrap_or("-"))
        }
        CtlEvent::Presence { mri, availability } => format!("{mri} {availability}"),
        CtlEvent::OwnStatus { availability } => availability.clone(),
        CtlEvent::Outbox(row) => outbox(row),
        _ => message_event(event)
            .or_else(|| call_event(event))
            .unwrap_or_else(|| app_event(event)),
    }
}

fn message_event(event: &CtlEvent) -> Option<String> {
    let CtlEvent::Message {
        chat,
        message,
        sender,
        sender_name,
        text,
        from_me,
        mentions_me,
        source,
    } = event
    else {
        return None;
    };
    let who = sender_name.as_deref().or(sender.as_deref()).unwrap_or("?");
    let mut line = format!("{chat} {message} {who}: {}", one_line(text));
    for (on, flag) in [(*from_me, "[me]"), (*mentions_me, "[@me]")] {
        if on {
            line.push(' ');
            line.push_str(flag);
        }
    }
    match source {
        crate::protocol::MessageSource::Push => {}
        crate::protocol::MessageSource::CatchUp => line.push_str(" [catch-up]"),
        crate::protocol::MessageSource::Replay => line.push_str(" [replay]"),
    }
    Some(line)
}

fn call_event(event: &CtlEvent) -> Option<String> {
    Some(match event {
        CtlEvent::IncomingCall {
            call,
            caller,
            caller_name,
            chat,
        } => {
            let who = caller_name.as_deref().unwrap_or(caller);
            format!(
                "{call} from {who}{}",
                chat.as_ref().map(|chat| format!(" in {chat}")).unwrap_or_default()
            )
        }
        CtlEvent::RingEnded { call, how } => format!("{call} {how}"),
        CtlEvent::Call { call, state, reason } => match reason {
            Some(reason) => format!("{call} {state} ({reason})"),
            None => format!("{call} {state}"),
        },
        CtlEvent::CallParticipants { call, joined, left } => {
            let mut line = call.clone();
            if !joined.is_empty() {
                let _ = write!(line, " joined: {}", people(joined));
            }
            if !left.is_empty() {
                let _ = write!(line, " left: {}", people(left));
            }
            line
        }
        CtlEvent::Lobby { call, waiting } => format!("{call} waiting: {}", people(waiting)),
        CtlEvent::Voicemail { call } => call.clone(),
        CtlEvent::CallWindow { open } => (if *open { "open" } else { "closed" }).to_owned(),
        CtlEvent::MeetingStarting {
            id,
            subject,
            minutes,
            joinable,
            ..
        } => format!(
            "{subject} in {minutes} min ({id}){}",
            if *joinable { " [joinable]" } else { "" }
        ),
        _ => return None,
    })
}

fn app_event(event: &CtlEvent) -> String {
    match event {
        CtlEvent::Connection { state } => format!("{state:?}").to_lowercase(),
        CtlEvent::CatchUpFailed { error } => error.clone(),
        CtlEvent::EngineFailed { during, error } => format!("{during}: {error}"),
        CtlEvent::Notification { title, chat } => match chat {
            Some(chat) => format!("{title} ({chat})"),
            None => title.clone(),
        },
        CtlEvent::Ui { state } => format!(
            "{} {} {} window={}{}",
            state.shown.as_deref().unwrap_or("-"),
            view(state.view),
            state.chat.as_deref().unwrap_or("-"),
            window(state.window),
            if state.call_window { " call-window" } else { "" }
        ),
        CtlEvent::Lagged => "events were dropped: the stream ends".to_owned(),
        _ => String::new(),
    }
}

fn people(people: &[PersonRow]) -> String {
    people
        .iter()
        .map(|person| person.name.clone().unwrap_or_else(|| person.mri.clone()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn reaction_list(reactions: &[ReactionRow]) -> String {
    reactions
        .iter()
        .map(|reaction| {
            let mine = if reaction.mine { "*" } else { "" };
            format!("{}x{}{mine}", reaction.reaction, reaction.count)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn app_status(status: &AppStatus) -> String {
    let mut text = format!("ngteams {}  window: {}\n", status.app, window(status.window));
    text.push_str(&account_table(&status.accounts));
    if let Some(call) = &status.call {
        let _ = writeln!(text, "call: {}", call_line(call));
    }
    text
}

fn account_table(accounts: &[AccountRow]) -> String {
    let rows = accounts.iter().map(|account| {
        vec![
            format!("{}{}", account.index, if account.shown { "*" } else { "" }),
            account.upn.clone(),
            account.state.clone(),
            account.name.clone().unwrap_or_default(),
        ]
    });
    table(&["#", "ACCOUNT", "STATE", "NAME"], rows)
}

fn chat_table(chats: &[ChatRow]) -> String {
    let rows = chats.iter().map(|chat| {
        vec![
            chat.id.clone(),
            chat.unread.to_string(),
            chat.last_activity.map_or_else(|| "-".to_owned(), stamp),
            chat.chat_type.clone(),
            chat.title.clone(),
        ]
    });
    table(&["ID", "UNREAD", "LAST", "TYPE", "TITLE"], rows)
}

fn message_table(messages: &[MessageRow], with_chat: bool) -> String {
    let rows = messages.iter().map(|message| {
        let mut row = vec![message.id.clone()];
        if with_chat {
            row.push(message.chat.clone());
        }
        row.push(stamp(message.at));
        row.push(sender(message));
        row.push(message_text(message));
        row
    });
    if with_chat {
        table(&["ID", "CHAT", "TIME", "FROM", "TEXT"], rows)
    } else {
        table(&["ID", "TIME", "FROM", "TEXT"], rows)
    }
}

fn sender(message: &MessageRow) -> String {
    let who = message
        .sender_name
        .as_deref()
        .or(message.sender.as_deref())
        .unwrap_or("-");
    if message.from_me {
        format!("{who} (me)")
    } else {
        who.to_owned()
    }
}

fn message_text(message: &MessageRow) -> String {
    let mut text = String::new();
    for (on, flag) in [
        (message.deleted, "(deleted) "),
        (message.edited, "(edited) "),
        (message.mentions_me, "(@me) "),
    ] {
        if on {
            text.push_str(flag);
        }
    }
    text.push_str(&one_line(&message.text));
    text
}

fn unread_table(unread: &Unread) -> String {
    let rows = unread
        .chats
        .iter()
        .map(|chat| vec![chat.count.to_string(), chat.chat.clone(), chat.title.clone()]);
    format!("total: {}\n{}", unread.total, table(&["COUNT", "CHAT", "TITLE"], rows))
}

fn member_table(members: &[MemberRow]) -> String {
    let rows = members.iter().map(|member| {
        vec![
            member.mri.clone(),
            member.role.clone().unwrap_or_default(),
            member.name.clone().unwrap_or_default(),
        ]
    });
    table(&["MRI", "ROLE", "NAME"], rows)
}

fn activity_table(items: &[ActivityRow]) -> String {
    let rows = items.iter().map(|item| {
        vec![
            item.id.clone(),
            if item.read { "" } else { "unread" }.to_owned(),
            stamp(item.at),
            item.kind.clone(),
            one_line(&item.title),
            item.place.clone().unwrap_or_default(),
            one_line(item.preview.as_deref().unwrap_or_default()),
        ]
    });
    table(&["ID", "STATE", "TIME", "KIND", "TITLE", "PLACE", "PREVIEW"], rows)
}

fn ui_state(state: &UiState) -> String {
    record(&[
        ("account", state.shown.clone().unwrap_or_else(|| "-".to_owned())),
        ("view", view(state.view).to_owned()),
        ("list", if state.activity { "activity" } else { "chats" }.to_owned()),
        ("chat", state.chat.clone().unwrap_or_else(|| "-".to_owned())),
        ("title", state.chat_title.clone().unwrap_or_else(|| "-".to_owned())),
        ("window", window(state.window).to_owned()),
        (
            "call window",
            if state.call_window { "open" } else { "closed" }.to_owned(),
        ),
    ])
}

fn outbox(row: &OutboxRow) -> String {
    let mut line = format!(
        "#{} {} {} {}",
        row.id,
        row.write,
        row.chat.as_deref().unwrap_or("-"),
        outbox_state(row.state)
    );
    if let Some(message) = &row.message {
        let _ = write!(line, " {message}");
    }
    if let Some(error) = &row.error {
        let _ = write!(line, " ({error})");
    }
    line
}

fn outbox_state(state: OutboxState) -> &'static str {
    match state {
        OutboxState::Queued => "queued",
        OutboxState::Sent => "sent",
        OutboxState::Failed => "failed",
        OutboxState::Unknown => "unknown",
    }
}

fn own_status(status: &OwnStatus) -> String {
    record(&[
        ("reported", status.reported.clone()),
        ("manual", format!("{:?}", status.manual).to_lowercase()),
    ])
}

fn synced(report: &SyncReport) -> String {
    record(&[
        ("conversations", report.conversations.to_string()),
        ("messages", report.messages.to_string()),
        ("failed", report.failed.to_string()),
        ("unreadable", report.unreadable.to_string()),
        ("caught up", if report.caught_up { "yes" } else { "no" }.to_owned()),
    ])
}

fn call_table(calls: &[CallRow]) -> String {
    let rows = calls.iter().map(|call| {
        vec![
            call.id.clone(),
            call.account.clone(),
            call.chat.clone().unwrap_or_else(|| "-".to_owned()),
            call_state(call),
        ]
    });
    table(&["ID", "ACCOUNT", "CHAT", "STATE"], rows)
}

fn call_line(call: &CallRow) -> String {
    format!(
        "{} on {} {}{}",
        call.id,
        call.account,
        call.chat
            .as_deref()
            .map(|chat| format!("in {chat} "))
            .unwrap_or_default(),
        call_state(call)
    )
}

fn call_state(call: &CallRow) -> String {
    let mut state = call.state.clone();
    if call.ringing {
        let caller = call.caller.as_deref().unwrap_or("?");
        let _ = write!(state, " (ringing from {caller})");
    }
    for (on, flag) in [(call.muted, " muted"), (call.camera, " camera")] {
        if on {
            state.push_str(flag);
        }
    }
    state
}

fn calendar_table(events: &[CalendarRow]) -> String {
    let rows = events.iter().map(|event| {
        vec![
            event.id.clone(),
            stamp(event.start),
            stamp(event.end),
            calendar_flags(event),
            event.subject.clone(),
        ]
    });
    table(&["ID", "START", "END", "FLAGS", "SUBJECT"], rows)
}

fn calendar_event(event: &CalendarRow) -> String {
    record(&[
        ("id", event.id.clone()),
        ("subject", event.subject.clone()),
        ("start", stamp(event.start)),
        ("end", stamp(event.end)),
        ("flags", calendar_flags(event)),
        ("location", event.location.clone().unwrap_or_else(|| "-".to_owned())),
        ("organizer", event.organizer.clone().unwrap_or_else(|| "-".to_owned())),
    ])
}

fn calendar_flags(event: &CalendarRow) -> String {
    let flags: Vec<&str> = [
        (event.all_day, "all-day"),
        (event.cancelled, "cancelled"),
        (event.online, "online"),
        (event.joinable, "joinable"),
    ]
    .into_iter()
    .filter_map(|(on, flag)| on.then_some(flag))
    .collect();
    if flags.is_empty() {
        "-".to_owned()
    } else {
        flags.join(",")
    }
}

fn view(view: View) -> &'static str {
    match view {
        View::Chat => "chat",
        View::Channel => "channel",
        View::Calendar => "calendar",
        View::Settings => "settings",
        View::Teams => "teams",
        View::Search => "search",
        View::Other => "other",
    }
}

fn window(state: WindowState) -> &'static str {
    match state {
        WindowState::Shown => "shown",
        WindowState::Tray => "tray",
        WindowState::Closed => "closed",
    }
}

/// Rows under a header, each column as wide as its widest cell, the last one unpadded.
fn table(header: &[&str], rows: impl Iterator<Item = Vec<String>>) -> String {
    let mut cells: Vec<Vec<String>> = vec![header.iter().map(|cell| (*cell).to_owned()).collect()];
    // escaped before measuring, so a cell's control characters neither act nor break the columns
    cells.extend(rows.map(|row| row.iter().map(|cell| shown(cell, false)).collect()));
    let mut widths = vec![0; header.len()];
    for row in &cells {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut text = String::new();
    for row in &cells {
        let last = row.len().saturating_sub(1);
        for (column, cell) in row.iter().enumerate() {
            if column == last {
                text.push_str(cell);
            } else {
                let pad = widths[column].saturating_sub(cell.chars().count());
                let _ = write!(text, "{cell}{}  ", " ".repeat(pad));
            }
        }
        text.truncate(text.trim_end_matches(' ').len());
        text.push('\n');
    }
    text
}

/// `key: value` lines, the values aligned.
fn record(fields: &[(&str, String)]) -> String {
    let width = fields.iter().map(|(key, _)| key.len()).max().unwrap_or(0);
    fields.iter().fold(String::new(), |mut text, (key, value)| {
        let _ = writeln!(text, "{key}:{} {}", " ".repeat(width - key.len()), shown(value, false));
        text
    })
}

/// `text` with its control characters (C0, DEL and C1: a terminal acts on them, `ESC[2J` clears the screen)
/// escaped as Rust writes them (`\u{1b}`, `\t`), line ends too unless `keep_lines`: what the app reports comes from
/// other people's messages and names.
fn shown(text: &str, keep_lines: bool) -> String {
    text.chars().fold(String::with_capacity(text.len()), |mut shown, c| {
        if c.is_control() && !(keep_lines && c == '\n') {
            shown.extend(c.escape_default());
        } else {
            shown.push(c);
        }
        shown
    })
}

/// A message's text on one line: its line breaks as ` ↵ `.
fn one_line(text: &str) -> String {
    text.trim_end().lines().collect::<Vec<_>>().join(" ↵ ")
}

fn local(ms: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp_millis(ms).map(|time| time.with_timezone(&Local))
}

/// A table's time: `YYYY-MM-DD HH:MM`, local.
fn stamp(ms: i64) -> String {
    local(ms).map_or_else(|| ms.to_string(), |time| time.format("%Y-%m-%d %H:%M").to_string())
}

/// An event line's time: `HH:MM:SS`, local.
fn clock(ms: i64) -> String {
    local(ms).map_or_else(|| ms.to_string(), |time| time.format("%H:%M:%S").to_string())
}
