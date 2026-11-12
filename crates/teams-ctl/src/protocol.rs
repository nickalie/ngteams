//! What crosses the control socket.
//!
//! - A request is one line: `{"id":N,"cmd":"<cmd>","account":"<upn|mri|index>"?,…}` ([`RequestLine`]). `account`
//!   addresses one of the app's accounts; without it the server takes the account shown in the window (a call command:
//!   the account that rings or is in the call).
//! - Its response is one line with the same id: `{"id":N,"ok":{"kind":"<kind>",…}}` or
//!   `{"id":N,"error":{"code":"<code>","message":…}}` ([`ResponseLine`]). A line too malformed to carry an id is
//!   answered with id 0; a client numbers its requests from 1.
//! - `subscribe` turns the connection into a stream of `{"event":"<kind>","account":…,"at":<ms>,…}` lines
//!   ([`EventLine`]) after its `subscribed` response. A subscriber that falls behind gets a last `lagged` line, and the
//!   connection closes.
//! - The first exchange is `hello` ([`Request::Hello`]); a server of another [`PROTOCOL_VERSION`] answers
//!   [`ErrorCode::VersionMismatch`].
//!
//! Every type here is the wire's own, mapped from the engine's and the store's by the server: open-ended values the
//! engine names (a presence's availability, a call's state, an engine's state) cross as its words in plain strings.

use serde::{Deserialize, Serialize};

/// The protocol's version: a `hello` from a client of another version is refused.
pub const PROTOCOL_VERSION: u32 = 1;

/// How long a write asked to `wait` waits for its outbox row to settle before it answers [`ErrorCode::Timeout`]: past
/// the outbox's own retries of a send that keeps failing for a while.
pub const WRITE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the app takes at most to answer a write before it waits for the row ([`WRITE_WAIT`] more with `wait`): the
/// row's first attempt and the chat's sync after it, each request bounded by the transport's 30 s, behind whatever the
/// engine was doing first. Past it the app answers [`ErrorCode::Timeout`], and the write may still go out
/// ([`UNANSWERED_WRITE`]).
pub const WRITE_ANSWER: std::time::Duration = std::time::Duration::from_secs(90);

/// How long the app takes at most to answer a `sync`: a catch-up reads every conversation changed since the last one,
/// each read bounded by the transport's 30 s. Past it the app answers [`ErrorCode::Timeout`], and the catch-up goes
/// on.
pub const SYNC_ANSWER: std::time::Duration = std::time::Duration::from_secs(300);

/// How long the app takes at most to answer a call command: the engine's answer to its call action (a start is taken
/// within the engine's 15 s or refused, a leave's request bounded by the transport's 30 s) and the call as the app
/// then shows it. Past it the app answers [`ErrorCode::Timeout`], and the action goes on (`ngteamsctl calls` shows
/// where the call stands).
pub const CALL_ANSWER: std::time::Duration = std::time::Duration::from_secs(60);

/// How long the app takes at most to answer a calendar read (`calendar`, `calendar show`): one read of the service
/// takes up to the transport's 30 s, and up to twice that and a throttled service's wait (at most 30 s) more when it
/// is tried again (a token renewed, or the service throttling); a range over 62 days is read 62 days at a time. Past
/// it the app answers [`ErrorCode::Timeout`].
pub const CALENDAR_ANSWER: std::time::Duration = std::time::Duration::from_secs(90);

/// What to do about a write whose answer didn't come in time: it may still go out, so it is looked for before it is
/// written again.
pub const UNANSWERED_WRITE: &str = "`ngteamsctl events --follow --kind outbox` shows its row as it changes, and \
                                    `ngteamsctl messages <chat>` what the chat holds: look there before writing it \
                                    again";

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's `skip_serializing_if` passes a reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// A request with its id and the account it addresses: one line from the client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestLine {
    /// Pairs the response with the request; a client numbers its requests from 1.
    pub id: u64,
    /// The account addressed, by UPN, MRI or index; the shown account (or the call's) without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(flatten)]
    pub request: Request,
}

/// What a client asks, by its `cmd`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// The first exchange: the client's protocol version and its own version.
    Hello { protocol: u32, client: String },
    /// The app's accounts with their engines' state, the window, and the call up.
    Status,
    /// The app's accounts.
    Accounts,
    /// The account's chats, the most recent first.
    Chats {
        /// Only the chats with unread messages.
        #[serde(default)]
        unread_only: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// A chat's messages, the newest first.
    Messages {
        chat: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        /// Only the messages before this message id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<String>,
    },
    /// The unread counts the badge shows: per chat and the account's total.
    Unread,
    /// The account's messages that match the query.
    Search {
        query: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// A chat's members.
    Members { chat: String },
    /// The account's activity feed, the newest first, as the Activity pane lists it.
    Activity {
        /// Only the items one of the pane's chips keeps, by its key (`unread`, `mentions`, `reactions`,
        /// `missed-calls`, `replies`, `voicemail`, `apps`); all of them without it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
        /// Only the items whose title, preview or place holds these words, as the pane's search finds them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        search: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Show, hide (to the tray), toggle or focus the main window.
    Window { op: WindowOp },
    /// Open a view in the window.
    Open { target: OpenTarget },
    /// Show another account in the window.
    SwitchAccount { to: String },
    /// What the window shows.
    UiState,
    /// Dispatch a registered `teams_ui::` action to the main window, by its full or unique short name.
    Action { name: String },
    /// The registered `teams_ui::` actions.
    ActionList,
    /// Turn the connection into an event stream.
    Subscribe {
        #[serde(default)]
        filter: EventFilter,
    },
    /// Bring the window to the front (a second launch hands off with it).
    Activate,
    /// Send plain text (line breaks kept) to a chat.
    ///
    /// Each write answers with its outbox row as its first attempt left it ([`WRITE_ANSWER`] at most), or with `wait`
    /// once the row settled: sent, failed, or `unknown` (it may have arrived, and only a `resend` sends it again);
    /// [`WRITE_WAIT`] more at most. A `mark_read` with nothing to mark answers `done`. Past either bound the answer
    /// is [`ErrorCode::Timeout`], and the write may still go out ([`UNANSWERED_WRITE`]).
    Send {
        chat: String,
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// Replace the text of one of the account's messages.
    Edit {
        chat: String,
        message: String,
        text: String,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// Delete one of the account's messages.
    Delete {
        chat: String,
        message: String,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// Add a reaction to a message.
    React {
        chat: String,
        message: String,
        reaction: String,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// Take the account's reaction off a message.
    Unreact {
        chat: String,
        message: String,
        reaction: String,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// Mark a chat read up to its latest message.
    MarkRead {
        chat: String,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// Start a chat with these people (UPNs or MRIs).
    NewChat {
        people: Vec<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// Send an outbox row again.
    Resend {
        row: i64,
        #[serde(default, skip_serializing_if = "is_false")]
        wait: bool,
    },
    /// The account's reported status and its manual choice.
    GetStatus,
    /// Choose the account's status, as its status menu does.
    SetStatus { status: StatusChoice },
    /// Catch the account up with the service.
    Sync,
    /// The invitation that rings and the call up.
    Calls,
    /// Call a chat: its other member, or a group chat's members.
    Call { chat: String },
    /// Answer the invitation that rings.
    Answer,
    /// Decline the invitation that rings.
    Decline,
    /// Leave the call up.
    HangUp,
    /// Turn the microphone on or off in the call up.
    Mute { op: Toggle },
    /// Turn the camera on or off in the call up.
    Camera { op: Toggle },
    /// Join the running call of a group chat.
    Join { chat: String },
    /// The account's calendar between two local dates (`YYYY-MM-DD`, both included); today without them.
    Calendar {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
    /// One calendar event, by its id (named `event`: the line's own `id` pairs it with its answer).
    CalendarEvent { event: String },
    /// Open a meeting's prejoin, or join it at once (`now`).
    JoinMeeting {
        event: String,
        #[serde(default)]
        now: bool,
    },
    /// Open a Teams link (a meeting's join link) as the app would.
    OpenLink { url: String },
}

/// How a calendar date is spelled on the wire and the command line.
pub const DATE_FORMAT: &str = "%Y-%m-%d";

/// The window [`Request::Calendar`] reads, in ms since the epoch: from the local midnight starting `from` to the one
/// ending `to`, both days included. Without `from` it starts `today`; without `to` it ends with its first day.
///
/// # Errors
/// [`ErrorCode::BadRequest`] for a date that isn't `YYYY-MM-DD`, or a `to` before the first day.
pub fn calendar_window(from: Option<&str>, to: Option<&str>, today: chrono::NaiveDate) -> Result<(i64, i64), CtlError> {
    let date = |text: &str| {
        chrono::NaiveDate::parse_from_str(text.trim(), DATE_FORMAT)
            .map_err(|_| CtlError::new(ErrorCode::BadRequest, format!("{text:?} isn't a date (YYYY-MM-DD)")))
    };
    let first = from.map(date).transpose()?.unwrap_or(today);
    let last = to.map(date).transpose()?.unwrap_or(first);
    if last < first {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("the calendar's last day ({last}) is before its first ({first})"),
        ));
    }
    let Some(after) = last.succ_opt() else {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            format!("{last} is the calendar's last day"),
        ));
    };
    Ok((midnight_in(&chrono::Local, first), midnight_in(&chrono::Local, after)))
}

/// Today in the local time zone, as [`calendar_window`] counts it.
#[must_use]
pub fn local_today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

/// The first instant of `date` in `zone`, in ms since the epoch: its midnight, the earlier one where a change of clock
/// repeats it. Where the clock skips midnight the day starts when it jumps, the first time on the clock that date has
/// (the next day's midnight when the whole date is skipped).
#[must_use]
pub fn midnight_in<Tz: chrono::TimeZone>(zone: &Tz, date: chrono::NaiveDate) -> i64 {
    use chrono::Offset as _;
    let midnight = date.and_time(chrono::NaiveTime::MIN);
    if let Some(time) = zone.from_local_datetime(&midnight).earliest() {
        return time.timestamp_millis();
    }
    // the jump is at midnight on the clock before it: the offset a day earlier, before the change
    let as_utc = midnight.and_utc().timestamp_millis();
    midnight
        .checked_sub_signed(chrono::TimeDelta::days(1))
        .map_or(as_utc, |day_before| {
            let before = zone.offset_from_utc_datetime(&day_before).fix();
            as_utc - i64::from(before.local_minus_utc()) * 1000
        })
}

/// What [`Request::Window`] does to the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowOp {
    Show,
    /// To the tray; refused ([`ErrorCode::NoTray`]) without a tray item, since closing would quit the app.
    Hide,
    Toggle,
    Focus,
}

/// The view [`Request::Open`] opens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum OpenTarget {
    /// A chat, by id or title.
    Chat {
        chat: String,
    },
    /// A channel, by id or name.
    Channel {
        channel: String,
    },
    Calendar,
    /// The Activity pane.
    Activity,
    /// The settings, at a page of them.
    Settings {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page: Option<String>,
    },
    /// The search results of a query.
    Search {
        query: String,
    },
}

/// A switch set on, off, or to the other state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Toggle {
    On,
    Off,
    Toggle,
}

/// The status menu's choices; `reset` hands the status back to the app's idle policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusChoice {
    Available,
    Busy,
    Dnd,
    Away,
    Reset,
}

/// The status the account's user chose: `automatic` while the idle policy decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualStatus {
    Automatic,
    Available,
    Busy,
    Dnd,
    Away,
}

/// One response line: the request's id, and its answer or its error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResponseLine {
    pub id: u64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// A request's answer or its error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Response),
    Error(CtlError),
}

/// A request's answer, by its `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Hello(Hello),
    Status(AppStatus),
    Accounts {
        accounts: Vec<AccountRow>,
    },
    Chats {
        chats: Vec<ChatRow>,
    },
    Messages {
        messages: Vec<MessageRow>,
    },
    Unread(Unread),
    Search {
        messages: Vec<MessageRow>,
    },
    Members {
        members: Vec<MemberRow>,
    },
    Activity {
        items: Vec<ActivityRow>,
    },
    UiState(UiState),
    /// The action was handed to the window; GPUI runs it later.
    Dispatched {
        action: String,
    },
    Actions {
        actions: Vec<String>,
    },
    /// Event lines follow.
    Subscribed,
    /// Done, with nothing to tell.
    Done,
    Outbox(OutboxRow),
    OwnStatus(OwnStatus),
    Synced(SyncReport),
    Calls {
        calls: Vec<CallRow>,
    },
    Call(CallRow),
    Calendar {
        events: Vec<CalendarRow>,
    },
    CalendarEvent(CalendarRow),
}

/// The server's half of `hello`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// The app's version.
    pub app: String,
}

/// The app as `status` sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppStatus {
    pub app: String,
    pub accounts: Vec<AccountRow>,
    pub window: WindowState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<CallRow>,
}

/// Where the main window is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowState {
    Shown,
    /// Closed to the tray: the app runs on.
    Tray,
    /// Closed, with no tray item (a call window may keep the app running).
    Closed,
}

/// One of the app's accounts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountRow {
    /// Its place in the app's accounts, from 0: what `--account <index>` names.
    pub index: u32,
    pub upn: String,
    pub mri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The account the window shows.
    pub shown: bool,
    /// Its engine's state, as the app names it.
    pub state: String,
}

/// A chat in the chat list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRow {
    pub id: String,
    /// As the chat list shows it: the topic, otherwise the other members' names.
    pub title: String,
    /// What it is (one-to-one, group, meeting, channel, …), as the app names it.
    pub chat_type: String,
    pub unread: u64,
    /// When its last message came (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<i64>,
    /// Its last message as plain text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// A chat named by id and title: an ambiguous title's candidates.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRef {
    pub id: String,
    pub title: String,
}

/// A message as plain text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag of its own the message carries: edited, deleted, sent by the account"
)]
pub struct MessageRow {
    pub id: String,
    pub chat: String,
    /// The sender's MRI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    /// When it arrived (ms).
    pub at: i64,
    pub text: String,
    #[serde(default)]
    pub edited: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub from_me: bool,
    #[serde(default)]
    pub mentions_me: bool,
}

/// The badge's unread counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unread {
    pub total: u64,
    /// The chats with unread messages.
    pub chats: Vec<UnreadRow>,
}

/// One chat's unread count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadRow {
    pub chat: String,
    pub title: String,
    pub count: u64,
}

/// A chat's member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRow {
    pub mri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its role in the chat, as the service names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// An item of the activity feed, as the Activity pane shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityRow {
    /// The feed message's id (the newest one's, for reactions folded into one item).
    pub id: String,
    /// What it tells (`mention`, `everyone`, `reaction`, `reply`, `missed-call`, `meeting-updated`, `app`, …), as the
    /// app names it.
    pub kind: String,
    /// As the pane titles it: "Lab B +1 reacted to your message".
    pub title: String,
    /// The message's text as the service previews it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Where it happened, as the pane's third line says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The chat or channel it opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// The message it opens at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// When it happened (ms).
    pub at: i64,
    pub read: bool,
}

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiState {
    /// The UPN of the account shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<String>,
    pub view: View,
    /// The chat or channel open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_title: Option<String>,
    pub window: WindowState,
    pub call_window: bool,
    /// The list pane shows the Activity pane (left of the main view).
    #[serde(default, skip_serializing_if = "is_false")]
    pub activity: bool,
}

/// The window's main view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Chat,
    Channel,
    Calendar,
    Settings,
    Teams,
    Search,
    /// Another view of the app's (activity, sign-in, …).
    Other,
}

/// An outbox row: one write and where it stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRow {
    pub id: i64,
    /// The write it is (send, edit, delete, reaction, …), as the app names it.
    pub write: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    pub state: OutboxState,
    /// The service's id for the message, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Why the last attempt didn't send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where an outbox row stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxState {
    /// Waiting for its first attempt or a retry.
    Queued,
    Sent,
    /// Refused, for good: nothing more is tried without a resend.
    Failed,
    /// An attempt began and whether it arrived isn't known yet: the outbox checks, never resends blindly.
    Unknown,
}

/// The account's own status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnStatus {
    /// The availability reported for the account, as the app names it.
    pub reported: String,
    pub manual: ManualStatus,
}

/// What a catch-up did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub conversations: u64,
    pub messages: u64,
    /// Conversations that couldn't be synced (synced again by a later catch-up).
    pub failed: u64,
    /// Conversations the account can't read.
    pub unreadable: u64,
    /// Nothing failed that trying again soon may change.
    pub caught_up: bool,
}

/// An invitation that rings, or a call up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRow {
    pub id: String,
    /// The UPN of the account it rings at or is up on.
    pub account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// Its state, as the app names it.
    pub state: String,
    /// An invitation that rings here, not answered yet.
    #[serde(default)]
    pub ringing: bool,
    /// Who calls, when it rings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub camera: bool,
}

/// A calendar event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag of its own the event carries: all day, cancelled, a Teams meeting, joinable"
)]
pub struct CalendarRow {
    pub id: String,
    pub subject: String,
    /// When it starts and ends (ms), a time with no offset taken as local.
    pub start: i64,
    pub end: i64,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub cancelled: bool,
    /// A Teams meeting.
    #[serde(default)]
    pub online: bool,
    /// It can be joined from the app.
    #[serde(default)]
    pub joinable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organizer: Option<String>,
}

/// An error answer: its code and a message for a person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct CtlError {
    pub code: ErrorCode,
    pub message: String,
    /// An ambiguous chat's candidates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<ChatRef>,
}

impl CtlError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            candidates: Vec::new(),
        }
    }
}

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// No account is the UPN, MRI or index given.
    UnknownAccount,
    /// No chat has the id or title given.
    UnknownChat,
    /// Several chats have the title given ([`CtlError::candidates`]).
    AmbiguousChat,
    /// Another call is up or rings.
    Busy,
    /// The app runs without calling.
    NoCalling,
    /// No call is up.
    NoCall,
    /// No invitation rings.
    NotRinging,
    /// No camera to turn on.
    NoVideo,
    /// The calendar event isn't a Teams meeting the app can join.
    NoMeeting,
    /// No tray item to hide the window to.
    NoTray,
    /// The main window is closed.
    NoWindow,
    /// No registered action has the name given, or several do.
    UnknownAction,
    /// The read-only mode refuses writes.
    RefusedReadOnly,
    /// The account's engine stopped.
    Stopped,
    /// The answer didn't come in time.
    Timeout,
    /// The request didn't read: not JSON, no id, an unknown `cmd` or a missing field.
    BadRequest,
    /// The client speaks another protocol version.
    VersionMismatch,
    /// The engine refused or failed, as its message says.
    Engine,
}

impl ErrorCode {
    /// The code as the wire spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownAccount => "unknown_account",
            Self::UnknownChat => "unknown_chat",
            Self::AmbiguousChat => "ambiguous_chat",
            Self::Busy => "busy",
            Self::NoCalling => "no_calling",
            Self::NoCall => "no_call",
            Self::NotRinging => "not_ringing",
            Self::NoVideo => "no_video",
            Self::NoMeeting => "no_meeting",
            Self::NoTray => "no_tray",
            Self::NoWindow => "no_window",
            Self::UnknownAction => "unknown_action",
            Self::RefusedReadOnly => "refused_read_only",
            Self::Stopped => "stopped",
            Self::Timeout => "timeout",
            Self::BadRequest => "bad_request",
            Self::VersionMismatch => "version_mismatch",
            Self::Engine => "engine",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which events a subscriber wants: each filter set narrows the stream, none set lets every event through.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter {
    /// Event kinds; one ending in `*` names every kind it starts (`message*`). Empty: every kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Only this account's events (UPN, MRI or index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Only this chat's events (id or title).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    /// Only a message or a typing from this person: by MRI, by display name in any case, or one of the app's accounts
    /// by UPN (anyone else's UPN is refused as a bad request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Only messages that mention the account.
    #[serde(default)]
    pub mentions_me: bool,
    /// A `reaction` event carries the message's current reactions.
    #[serde(default)]
    pub detail: bool,
    /// The minutes before a meeting's start at which `meeting_starting` fires; 5 and 0 when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leads: Vec<u32>,
}

impl EventFilter {
    /// Whether the filter's kinds let an event of `kind` through.
    #[must_use]
    pub fn wants_kind(&self, kind: &str) -> bool {
        self.kinds.is_empty()
            || self.kinds.iter().any(|wanted| match wanted.strip_suffix('*') {
                Some(prefix) => kind.starts_with(prefix),
                None => wanted == kind,
            })
    }
}

/// One event line: the event, the account it happened on, and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventLine {
    /// The account's UPN; none for the stream's own events (`lagged`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// When the app saw it (ms).
    pub at: i64,
    #[serde(flatten)]
    pub event: CtlEvent,
}

/// What a message event came by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    /// The push channel.
    Push,
    /// A read that brought the account up to date.
    CatchUp,
    /// A delivery the service made again.
    Replay,
}

/// Where the push channel is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    Connected,
    /// Down, to be connected again.
    Disconnected,
    /// Down for good, until the app starts it again.
    Stopped,
}

/// A reaction on a message, with how many gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionRow {
    pub reaction: String,
    pub count: u64,
    /// The account gave it.
    #[serde(default)]
    pub mine: bool,
}

/// Someone in a call or its lobby.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonRow {
    pub mri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// An event, by its `event` kind. Each carries only what its source holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CtlEvent {
    /// A new message: never older history, nor a catch-up or replay older than the notifier's cutoff.
    Message {
        chat: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_name: Option<String>,
        text: String,
        from_me: bool,
        mentions_me: bool,
        source: MessageSource,
    },
    MessageEdited {
        chat: String,
        message: String,
    },
    MessageDeleted {
        chat: String,
        message: String,
    },
    /// A message's reactions changed; with `detail`, the reactions it has now.
    Reaction {
        chat: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reactions: Option<Vec<ReactionRow>>,
    },
    Typing {
        chat: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        who: Option<String>,
        on: bool,
    },
    /// The account's read horizon in a chat moved.
    Read {
        chat: String,
    },
    ChatChanged {
        chat: String,
    },
    TeamsChanged,
    /// A chat's unread count or the account's total (its unread chats) changed: a line per chat whose count changed,
    /// or one without a chat (count 0) for the badge's first count.
    Unread {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
        count: u64,
        total: u64,
    },
    Presence {
        mri: String,
        availability: String,
    },
    /// The account's reported status changed (from the menu, ctl or the idle policy): `connecting`, `available`,
    /// `busy`, `do-not-disturb`, `away`, `not-set` (a status chosen whose report failed), `inactive` (idle), `halted`
    /// or `stopped`.
    OwnStatus {
        availability: String,
    },
    Outbox(OutboxRow),
    IncomingCall {
        call: String,
        caller: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caller_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
    },
    RingEnded {
        call: String,
        how: String,
    },
    Call {
        call: String,
        state: String,
        /// How it ended, once ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    CallParticipants {
        call: String,
        joined: Vec<PersonRow>,
        left: Vec<PersonRow>,
    },
    Lobby {
        call: String,
        waiting: Vec<PersonRow>,
    },
    Voicemail {
        call: String,
    },
    CallWindow {
        open: bool,
    },
    /// A meeting starts in `minutes` (a subscriber's lead).
    MeetingStarting {
        id: String,
        subject: String,
        start: i64,
        minutes: i64,
        joinable: bool,
    },
    Connection {
        state: Connection,
    },
    CaughtUp,
    CatchUpFailed {
        error: String,
    },
    EngineFailed {
        during: String,
        error: String,
    },
    SignedOut,
    AccountAdded,
    AccountRemoved,
    /// A desktop notification the app showed.
    Notification {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
    },
    /// The shown account, view, chat or window changed.
    Ui {
        state: UiState,
    },
    /// The subscriber fell behind: events were dropped, and the stream ends.
    Lagged,
}

impl CtlEvent {
    /// Every event kind, as the wire spells it.
    pub const KINDS: &[&str] = &[
        "message",
        "message_edited",
        "message_deleted",
        "reaction",
        "typing",
        "read",
        "chat_changed",
        "teams_changed",
        "unread",
        "presence",
        "own_status",
        "outbox",
        "incoming_call",
        "ring_ended",
        "call",
        "call_participants",
        "lobby",
        "voicemail",
        "call_window",
        "meeting_starting",
        "connection",
        "caught_up",
        "catch_up_failed",
        "engine_failed",
        "signed_out",
        "account_added",
        "account_removed",
        "notification",
        "ui",
        "lagged",
    ];

    /// The event's kind, as the wire spells it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Message { .. } => "message",
            Self::MessageEdited { .. } => "message_edited",
            Self::MessageDeleted { .. } => "message_deleted",
            Self::Reaction { .. } => "reaction",
            Self::Typing { .. } => "typing",
            Self::Read { .. } => "read",
            Self::ChatChanged { .. } => "chat_changed",
            Self::TeamsChanged => "teams_changed",
            Self::Unread { .. } => "unread",
            Self::Presence { .. } => "presence",
            Self::OwnStatus { .. } => "own_status",
            Self::Outbox(_) => "outbox",
            Self::IncomingCall { .. } => "incoming_call",
            Self::RingEnded { .. } => "ring_ended",
            Self::Call { .. } => "call",
            Self::CallParticipants { .. } => "call_participants",
            Self::Lobby { .. } => "lobby",
            Self::Voicemail { .. } => "voicemail",
            Self::CallWindow { .. } => "call_window",
            Self::MeetingStarting { .. } => "meeting_starting",
            Self::Connection { .. } => "connection",
            Self::CaughtUp => "caught_up",
            Self::CatchUpFailed { .. } => "catch_up_failed",
            Self::EngineFailed { .. } => "engine_failed",
            Self::SignedOut => "signed_out",
            Self::AccountAdded => "account_added",
            Self::AccountRemoved => "account_removed",
            Self::Notification { .. } => "notification",
            Self::Ui { .. } => "ui",
            Self::Lagged => "lagged",
        }
    }
}
