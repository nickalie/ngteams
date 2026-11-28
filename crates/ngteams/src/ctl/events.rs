//! The event stream: what `subscribe` turns a connection into (`ngteamsctl events`, `wait`).
//!
//! **The hub** ([`Hub`]) holds each subscriber's filter and a bounded channel to its connection's thread, on GPUI's
//! foreground. An event goes to every subscriber whose filter lets it through without waiting on any: one that has
//! [`BUFFERED`] events not yet written gets a last `lagged` line instead (its channel keeps one place for it) and is
//! dropped, so its connection writes what it had, then `lagged`, and closes. A subscriber whose client hung up has its
//! stream closed by its connection's thread, which watches for that (`listener`), and stops counting as one at once,
//! whatever its filter. With no subscriber the sources build no event at all. The quit closes every stream.
//!
//! The app's own state's events (`unread`, `own_status`, `signed_out`, `notification`, `ui`, `call_window`,
//! `account_added`, `account_removed`) come from [`super::watch`], and `meeting_starting` from [`super::ticker`].
//!
//! **The engine's events** come from each account's [`Bridge`] tap ([`tap`]), set when the account starts (at the
//! launch or added later), which sees each event just before the models apply it, and maps it to the wire's
//! ([`map`]):
//! - `message`: only a message newly stored (`New`, `Inserted`), never older history, and from a catch-up or a
//!   replayed delivery only one that arrived after the cutoff the notifier keeps (the start, then when the push
//!   channel was last heard before it went down, less the clock's skew); with its plain text as `messages` reads it,
//!   whether the account sent it and whether it mentions the account. A message without text (a thread's event, a
//!   call's record) is none;
//! - `message_edited`, `message_deleted`, `reaction` (with `detail`, the reactions the store holds now, read once the
//!   event came), `typing`, `read`, `chat_changed`, `teams_changed`, `presence`, `outbox` (a row as made and as each
//!   attempt left it);
//! - the calls: `incoming_call`, `ring_ended`, `call` (with how it ended), `call_participants` (who joined and who left
//!   since the view the models held before this one), `lobby`, `voicemail`;
//! - the app's link: `connection`, `caught_up`, `catch_up_failed`, `engine_failed`.
//!
//! **A filter** ([`Wanted`]): the kinds (`message*` for every kind it starts), the account (UPN, MRI or index), the
//! chat (its id or title, named when subscribing; an event without a chat isn't one of it), the sender (a message's or a
//! typing's: an MRI, a name in any case, or one of the app's accounts by UPN; anyone else's UPN is refused, as nothing
//! here maps it to an MRI) and messages that mention the account. Each one set narrows the stream; none set lets
//! everything through.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{App, AsyncApp, Entity, WeakEntity};
use teams_core::{Availability, CLOCK_SKEW_MS, Event, MessageChange, Source};
use teams_ctl::protocol::{
    Connection, CtlError, CtlEvent, ErrorCode, EventFilter, EventLine, MessageSource, OutboxRow, OutboxState,
    PersonRow, ReactionRow,
};
use teams_store::{Applied, Message, OutboxEntry, Reaction};
use teams_ui::rich_text::Mentions;
use teams_ui::{ActiveCall, Bridge, Core, Tap};

use super::address;
use super::dispatch::alive;
use super::reads::text_of;
use crate::app::Running;

/// The most events a subscriber may have not yet written before it is dropped as lagging.
pub const BUFFERED: usize = 1024;

/// A subscriber's events, for its connection's thread to write: closed once it is dropped, its last event `lagged`
/// when it fell behind.
pub type Stream = async_channel::Receiver<EventLine>;

/// Milliseconds since the epoch, now.
fn now_ms() -> i64 {
    millis(SystemTime::now())
}

fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// What a subscriber wants (see the module docs), its account and chat named.
#[derive(Clone, Debug, Default)]
pub struct Wanted {
    pub filter: EventFilter,
    /// The account's MRI.
    pub account: Option<String>,
    /// The chat's id.
    pub chat: Option<String>,
    /// The sender.
    pub from: Option<Sender>,
}

/// A sender a filter names: by these MRIs, or by this name in any case.
#[derive(Clone, Debug, Default)]
pub struct Sender {
    pub mris: Vec<String>,
    /// Lowercase.
    pub name: String,
}

impl Sender {
    fn is(&self, mri: Option<&str>, name: Option<&str>) -> bool {
        mri.is_some_and(|mri| self.mris.iter().any(|wanted| wanted == mri))
            || name.is_some_and(|name| name.trim().to_lowercase() == self.name)
    }
}

impl Wanted {
    /// Whether `event`, on the account `mri`, gets through.
    #[must_use]
    pub fn lets(&self, mri: &str, event: &CtlEvent) -> bool {
        if !self.filter.wants_kind(event.kind()) || self.account.as_deref().is_some_and(|wanted| wanted != mri) {
            return false;
        }
        if let Some(chat) = &self.chat
            && chat_of(event) != Some(chat.as_str())
        {
            return false;
        }
        if let Some(from) = &self.from {
            let sent = match event {
                CtlEvent::Message {
                    sender, sender_name, ..
                } => from.is(sender.as_deref(), sender_name.as_deref()),
                CtlEvent::Typing { who, .. } => from.is(who.as_deref(), None),
                _ => false,
            };
            if !sent {
                return false;
            }
        }
        !self.filter.mentions_me || matches!(event, CtlEvent::Message { mentions_me: true, .. })
    }
}

/// The chat an event is about, if any.
#[must_use]
pub fn chat_of(event: &CtlEvent) -> Option<&str> {
    match event {
        CtlEvent::Message { chat, .. }
        | CtlEvent::MessageEdited { chat, .. }
        | CtlEvent::MessageDeleted { chat, .. }
        | CtlEvent::Reaction { chat, .. }
        | CtlEvent::Typing { chat, .. }
        | CtlEvent::Read { chat }
        | CtlEvent::ChatChanged { chat } => Some(chat),
        CtlEvent::Unread { chat, .. } | CtlEvent::IncomingCall { chat, .. } | CtlEvent::Notification { chat, .. } => {
            chat.as_deref()
        }
        CtlEvent::Outbox(row) => row.chat.as_deref(),
        _ => None,
    }
}

struct Subscriber {
    id: u64,
    wanted: Wanted,
    sender: async_channel::Sender<EventLine>,
    /// The meetings told it, each by its id, its start and the lead it was told at ([`Hub::meetings`]).
    told: HashSet<(String, i64, u32)>,
}

impl Subscriber {
    /// The `meetings` of `account` whose lead came at `now` (see [`Hub::meetings`]); `false` once it is to be dropped.
    fn tell_meetings(&mut self, account: &Origin, meetings: &[Meeting], now: i64) -> bool {
        let leads = leads(&self.wanted.filter).to_vec();
        // what can't come again is forgotten
        self.told
            .retain(|(_, start, lead)| moment(*start, *lead) + super::ticker::GRACE_MS > now);
        let due = meetings.iter().flat_map(|meeting| {
            leads.iter().filter_map(move |&lead| {
                let at = moment(meeting.start, lead);
                (at..at + super::ticker::GRACE_MS)
                    .contains(&now)
                    .then_some((meeting, lead))
            })
        });
        for (meeting, lead) in due {
            let event = CtlEvent::MeetingStarting {
                id: meeting.id.clone(),
                subject: meeting.subject.clone(),
                start: meeting.start,
                minutes: i64::from(lead),
                joinable: meeting.joinable,
            };
            let key = (meeting.id.clone(), meeting.start, lead);
            if self.told.contains(&key) || !self.wanted.lets(&account.mri, &event) {
                continue;
            }
            self.told.insert(key);
            let line = EventLine {
                account: Some(account.upn.clone()),
                at: now,
                event,
            };
            if !deliver(&self.sender, line) {
                return false;
            }
        }
        true
    }
}

/// A meeting of an account's calendar as the ticker reads it ([`super::ticker`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meeting {
    pub id: String,
    pub subject: String,
    /// Its start, in ms since the epoch.
    pub start: i64,
    /// It can be joined (a Teams meeting).
    pub joinable: bool,
}

/// The subscribers (see the module docs). Foreground only.
#[derive(Default)]
pub struct Hub {
    subscribers: RefCell<Vec<Subscriber>>,
    next: Cell<u64>,
    /// The calendar's ticker runs ([`super::ticker`]).
    ticking: Cell<bool>,
}

impl Hub {
    /// Whether a subscriber wants events of `kind`; those whose client hung up are dropped first.
    #[must_use]
    pub fn wants(&self, kind: &str) -> bool {
        let mut subscribers = self.subscribers.borrow_mut();
        subscribers.retain(|subscriber| !subscriber.sender.is_closed());
        subscribers
            .iter()
            .any(|subscriber| subscriber.wanted.filter.wants_kind(kind))
    }

    /// The longest lead, in minutes, a subscriber wants `meeting_starting` at (0 with none).
    #[must_use]
    pub fn longest_lead(&self) -> u32 {
        self.subscribers
            .borrow()
            .iter()
            .filter(|subscriber| subscriber.wanted.filter.wants_kind(super::ticker::KIND))
            .flat_map(|subscriber| leads(&subscriber.wanted.filter).iter().copied())
            .max()
            .unwrap_or(0)
    }

    /// Marks the ticker running: `false` when it ran already.
    pub fn start_ticking(&self) -> bool {
        !self.ticking.replace(true)
    }

    /// Marks the ticker stopped (nobody wants its events).
    pub fn stop_ticking(&self) {
        self.ticking.set(false);
    }

    /// The `meetings` of `account` at `now` (ms since the epoch) to each subscriber whose lead for one came within the
    /// last [`super::ticker::GRACE_MS`], once per meeting and lead.
    pub fn meetings(&self, account: &Origin, meetings: &[Meeting], now: i64) {
        self.subscribers
            .borrow_mut()
            .retain_mut(|subscriber| subscriber.tell_meetings(account, meetings, now));
    }

    /// Whether anyone listens: without a subscriber no event is built. A subscriber whose client hung up is dropped
    /// here, whatever its filter.
    #[must_use]
    pub fn listening(&self) -> bool {
        let mut subscribers = self.subscribers.borrow_mut();
        subscribers.retain(|subscriber| !subscriber.sender.is_closed());
        !subscribers.is_empty()
    }

    /// A new subscriber's stream of the events `wanted` lets through.
    pub fn subscribe(&self, wanted: Wanted) -> Stream {
        // one place more than the buffer: the `lagged` line always fits
        let (sender, stream) = async_channel::bounded(BUFFERED + 1);
        let id = self.next.get();
        self.next.set(id + 1);
        let mut subscribers = self.subscribers.borrow_mut();
        // those gone meanwhile with no event since: dropped, so they don't pile up
        subscribers.retain(|subscriber| !subscriber.sender.is_closed());
        subscribers.push(Subscriber {
            id,
            wanted,
            sender,
            told: HashSet::new(),
        });
        stream
    }

    /// Every stream closed: the quit.
    pub fn close(&self) {
        self.subscribers.borrow_mut().clear();
    }

    /// `event` of `account` to each subscriber that wants it; one that wants a reaction's detail gets it once the
    /// store's reactions are read through the account's `Core`.
    pub fn publish(self: &Rc<Self>, account: &Origin, event: CtlEvent, cx: &mut App) {
        let line = EventLine {
            account: Some(account.upn.clone()),
            at: now_ms(),
            event,
        };
        let detailing = matches!(line.event, CtlEvent::Reaction { .. });
        let mut detailed = Vec::new();
        self.subscribers.borrow_mut().retain(|subscriber| {
            if !subscriber.wanted.lets(&account.mri, &line.event) {
                return true;
            }
            if detailing && subscriber.wanted.filter.detail {
                detailed.push(subscriber.id);
                return true;
            }
            deliver(&subscriber.sender, line.clone())
        });
        if !detailed.is_empty() {
            self.detail(account, line, detailed, cx);
        }
    }

    /// The reaction `line` with the message's reactions now, to the subscribers `ids`.
    fn detail(self: &Rc<Self>, account: &Origin, mut line: EventLine, ids: Vec<u64>, cx: &mut App) {
        let CtlEvent::Reaction { chat, message, .. } = &line.event else {
            return;
        };
        let reading = account.core.reactions(chat.clone(), message.clone());
        let (hub, me) = (Rc::clone(self), account.mri.clone());
        cx.spawn(async move |cx: &mut AsyncApp| {
            // a read that failed tells no reactions rather than none
            let read = reading.await.ok().map(|reactions| reaction_rows(&reactions, &me));
            if let CtlEvent::Reaction { reactions, .. } = &mut line.event {
                *reactions = read;
            }
            cx.update(|_| hub.send_to(&ids, &line));
        })
        .detach();
    }

    fn send_to(&self, ids: &[u64], line: &EventLine) {
        self.subscribers
            .borrow_mut()
            .retain(|subscriber| !ids.contains(&subscriber.id) || deliver(&subscriber.sender, line.clone()));
    }
}

/// The leads, in minutes, `filter` wants `meeting_starting` at: its own, else [`super::ticker::DEFAULT_LEADS`].
fn leads(filter: &EventFilter) -> &[u32] {
    if filter.leads.is_empty() {
        super::ticker::DEFAULT_LEADS
    } else {
        &filter.leads
    }
}

/// When a meeting starting at `start` is told at `lead` minutes before it.
fn moment(start: i64, lead: u32) -> i64 {
    start.saturating_sub(i64::from(lead) * 60_000)
}

/// `line` to a subscriber, never waiting: `false` when it is to be dropped (its connection went, or it fell behind and
/// was sent `lagged`).
fn deliver(sender: &async_channel::Sender<EventLine>, line: EventLine) -> bool {
    if sender.len() >= BUFFERED {
        let _ = sender.try_send(EventLine {
            account: None,
            at: line.at,
            event: CtlEvent::Lagged,
        });
        return false;
    }
    sender.try_send(line).is_ok()
}

/// A message's reactions as the wire gives them: each reaction with how many gave it, in the order first given.
fn reaction_rows(reactions: &[Reaction], me: &str) -> Vec<ReactionRow> {
    let mut rows: Vec<ReactionRow> = Vec::new();
    for reaction in reactions {
        let mine = reaction.user == me;
        if let Some(row) = rows.iter_mut().find(|row| row.reaction == reaction.key) {
            row.count += 1;
            row.mine |= mine;
        } else {
            rows.push(ReactionRow {
                reaction: reaction.key.clone(),
                count: 1,
                mine,
            });
        }
    }
    rows
}

/// The account an event came on, as its tap holds it.
#[derive(Clone)]
pub struct Origin {
    pub upn: String,
    pub mri: String,
    pub core: Arc<dyn Core>,
}

impl Origin {
    /// The account `upn`, whose bridge is `bridge`.
    #[must_use]
    pub fn of(upn: &str, bridge: &Entity<Bridge>, cx: &App) -> Self {
        let core = bridge.read(cx).core();
        Self {
            upn: upn.to_owned(),
            mri: core.me().to_owned(),
            core,
        }
    }
}

/// The app's accounts, as they run when asked.
pub type Accounts = Rc<dyn Fn(&App) -> Vec<Origin>>;

/// The accounts `running` runs when asked, in the rail's order; none once the app is gone.
#[must_use]
pub fn accounts_of(running: WeakEntity<Running>) -> Accounts {
    Rc::new(move |cx| {
        running.upgrade().map_or_else(Vec::new, |running| {
            running
                .read(cx)
                .bridges()
                .iter()
                .map(|(_, upn, bridge)| Origin::of(upn, bridge, cx))
                .collect()
        })
    })
}

/// The tap of `bridge`, the account `upn`'s, publishing its engine's events on `hub` (see the module docs).
pub fn tap(hub: &Rc<Hub>, upn: &str, bridge: &Entity<Bridge>, cx: &App) {
    let held = bridge.read(cx);
    let account = Origin::of(upn, bridge, cx);
    let calls = held.models().calls.clone();
    let cutoff = Cell::new(now_ms());
    let hub = Rc::clone(hub);
    let tap: Tap = Rc::new(move |event, cx| {
        // kept whether or not anyone listens: a subscriber that comes later skips what the notifier would
        if let Event::Disconnected { heard: Some(heard), .. } = event {
            cutoff.set(millis(*heard).saturating_sub(CLOCK_SKEW_MS));
        }
        if !hub.listening() {
            return;
        }
        if let Some(mapped) = map(event, &account.mri, cutoff.get(), calls.read(cx).call()) {
            hub.publish(&account, mapped, cx);
        }
    });
    held.set_tap(Some(tap));
}

/// `event` as the wire tells it, for the account `me`; `None` for one it doesn't tell (see the module docs). `cutoff`:
/// a catch-up's or a replay's message older than this isn't new to the user; `call`: the call up as the models held it
/// before this event.
#[must_use]
pub fn map(event: &Event, me: &str, cutoff: i64, call: Option<&ActiveCall>) -> Option<CtlEvent> {
    match event {
        Event::Message {
            conversation,
            id,
            change,
            applied,
            message,
            source,
        } => message_event(
            conversation,
            id,
            *change,
            *applied,
            message.as_deref(),
            *source,
            me,
            cutoff,
        ),
        Event::Typing {
            conversation,
            from,
            typing,
        } => Some(CtlEvent::Typing {
            chat: conversation.clone(),
            who: from.clone(),
            on: *typing,
        }),
        Event::ReadHorizon { conversation } => Some(CtlEvent::Read {
            chat: conversation.clone(),
        }),
        Event::ConversationChanged { conversation } => Some(CtlEvent::ChatChanged {
            chat: conversation.clone(),
        }),
        Event::Teams => Some(CtlEvent::TeamsChanged),
        Event::Presence { mri, availability } => Some(CtlEvent::Presence {
            mri: mri.clone(),
            availability: availability_name(availability),
        }),
        Event::Queued(entry) | Event::Outbox(entry) => Some(CtlEvent::Outbox(outbox_row(entry))),
        _ => call_event(event, call).or_else(|| link_event(event)),
    }
}

/// A message event (see the module docs).
#[expect(
    clippy::too_many_arguments,
    reason = "the engine's message event's own fields, with the account and the cutoff it is read against"
)]
fn message_event(
    chat: &str,
    id: &str,
    change: MessageChange,
    applied: Applied,
    message: Option<&Message>,
    source: Source,
    me: &str,
    cutoff: i64,
) -> Option<CtlEvent> {
    let (chat, id) = (chat.to_owned(), id.to_owned());
    match change {
        MessageChange::New => new_message(message?, applied, source, me, cutoff),
        _ if applied == Applied::Ignored => None,
        MessageChange::Edit => Some(CtlEvent::MessageEdited { chat, message: id }),
        MessageChange::Delete => Some(CtlEvent::MessageDeleted { chat, message: id }),
        MessageChange::Reaction => Some(CtlEvent::Reaction {
            chat,
            message: id,
            reactions: None,
        }),
        MessageChange::Other => None,
    }
}

fn new_message(message: &Message, applied: Applied, source: Source, me: &str, cutoff: i64) -> Option<CtlEvent> {
    let source = match source {
        Source::Delivery => MessageSource::Push,
        Source::CatchUp => MessageSource::CatchUp,
        Source::Replay => MessageSource::Replay,
        Source::History => return None,
    };
    if applied != Applied::Inserted || (source != MessageSource::Push && message.arrival_time <= cutoff) {
        return None;
    }
    Some(CtlEvent::Message {
        chat: message.conversation_id.clone(),
        message: message.id.clone(),
        sender: message.sender.clone(),
        sender_name: message.sender_name.clone().filter(|name| !name.trim().is_empty()),
        text: text_of(message)?.text,
        from_me: message.sender.as_deref() == Some(me),
        mentions_me: Mentions::of_message(message, me).names_me(),
        source,
    })
}

/// A call's event, `call` the call up as the models held it before this one.
fn call_event(event: &Event, call: Option<&ActiveCall>) -> Option<CtlEvent> {
    Some(match event {
        Event::IncomingCall(incoming) => CtlEvent::IncomingCall {
            call: incoming.call.get().to_string(),
            caller: incoming.caller.as_str().to_owned(),
            caller_name: incoming.caller_name.clone(),
            chat: incoming.group_chat.clone(),
        },
        Event::RingEnded { call, end } => CtlEvent::RingEnded {
            call: call.get().to_string(),
            how: end
                .phrase
                .clone()
                .or_else(|| end.code.map(|code| code.to_string()))
                .unwrap_or_else(|| "ended".to_owned()),
        },
        Event::Call { call, status } => CtlEvent::Call {
            call: call.get().to_string(),
            state: teams_ui::calls::status_name(status).to_owned(),
            reason: match status {
                teams_core::CallStatus::Ended(end) => Some(teams_ui::calls::end_name(end).to_owned()),
                _ => None,
            },
        },
        Event::CallParticipants { call: id, view } => {
            let before: &[teams_core::CallParticipant] = call
                .filter(|active| active.id == *id)
                .map_or(&[], |active| &active.view.participants);
            let now = &view.participants;
            CtlEvent::CallParticipants {
                call: id.get().to_string(),
                joined: missing(now, before),
                left: missing(before, now),
            }
        }
        Event::Lobby { call, waiting } => CtlEvent::Lobby {
            call: call.get().to_string(),
            waiting: waiting
                .iter()
                .map(|person| PersonRow {
                    mri: person.mri.as_str().to_owned(),
                    name: person.name.clone(),
                })
                .collect(),
        },
        Event::Voicemail { call } => CtlEvent::Voicemail {
            call: call.get().to_string(),
        },
        _ => return None,
    })
}

/// The participants of `listed` that aren't in `against`.
fn missing(listed: &[teams_core::CallParticipant], against: &[teams_core::CallParticipant]) -> Vec<PersonRow> {
    let against: HashSet<&str> = against.iter().map(|participant| participant.mri.as_str()).collect();
    listed
        .iter()
        .filter(|participant| !against.contains(participant.mri.as_str()))
        .map(|participant| PersonRow {
            mri: participant.mri.clone(),
            name: participant.name.clone(),
        })
        .collect()
}

/// The push channel's and the engine's own events.
fn link_event(event: &Event) -> Option<CtlEvent> {
    Some(match event {
        Event::Connected { .. } => CtlEvent::Connection {
            state: Connection::Connected,
        },
        Event::Disconnected { .. } => CtlEvent::Connection {
            state: Connection::Disconnected,
        },
        Event::PushStopped(_) => CtlEvent::Connection {
            state: Connection::Stopped,
        },
        Event::CaughtUp { .. } => CtlEvent::CaughtUp,
        Event::CatchUpFailed { error, .. } => CtlEvent::CatchUpFailed {
            error: error.to_string(),
        },
        Event::Failed { during, error } => CtlEvent::EngineFailed {
            during: (*during).to_owned(),
            error: error.to_string(),
        },
        _ => return None,
    })
}

/// An availability as the wire names it: `available`, `busy`, `do-not-disturb`, …, or the service's own word for one
/// this build doesn't name.
#[must_use]
pub fn availability_name(availability: &Availability) -> String {
    match availability {
        Availability::Other(word) => word.clone(),
        known => teams_ui::presence::key(known).to_owned(),
    }
}

/// An outbox row as the wire tells it: a row an attempt began on and didn't finish, or one whose message may have
/// arrived, is `unknown` (the outbox checks it, never sends it blindly).
#[must_use]
pub fn outbox_row(entry: &OutboxEntry) -> OutboxRow {
    let state = match entry.state {
        _ if entry.may_have_arrived() => OutboxState::Unknown,
        teams_store::OutboxState::Pending => OutboxState::Queued,
        teams_store::OutboxState::Sending => OutboxState::Unknown,
        teams_store::OutboxState::Sent => OutboxState::Sent,
        teams_store::OutboxState::Denied | teams_store::OutboxState::Failed => OutboxState::Failed,
    };
    OutboxRow {
        id: entry.id,
        write: entry.kind.as_str().to_owned(),
        chat: entry.conversation_id.clone(),
        state,
        message: entry.message_id.clone(),
        error: entry.last_error.clone(),
    }
}

/// A `subscribe` (see the module docs): its filter's account (else the request's) and chat named, then its stream.
///
/// # Errors
/// The app is quitting, the account or the chat named isn't one ([`address`]), or `from` is the UPN of someone not one
/// of the app's accounts.
pub async fn subscribe(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    filter: EventFilter,
    cx: &mut AsyncApp,
) -> Result<Stream, CtlError> {
    let named = filter.account.as_deref().or(account);
    let (addressed, from) = cx.update(|cx| -> Result<_, CtlError> {
        let running = alive(running, cx)?;
        let running = running.read(cx);
        let addressed = if named.is_some() || filter.chat.is_some() {
            Some(address::account(running, named, cx)?)
        } else {
            None
        };
        let from = filter.from.as_deref().map(|from| sender(running, from)).transpose()?;
        Ok((addressed, from))
    })?;
    let chat = match (&filter.chat, &addressed) {
        (Some(chat), Some(addressed)) => Some(address::chat(&*addressed.core, chat).await?.id),
        _ => None,
    };
    let wanted = Wanted {
        account: named.and(addressed.map(|addressed| addressed.mri)),
        chat,
        from,
        filter,
    };
    let meetings = wanted.filter.wants_kind(super::ticker::KIND);
    cx.update(|cx| {
        let alive = alive(running, cx)?;
        let hub = Rc::clone(alive.read(cx).hub());
        let stream = hub.subscribe(wanted);
        if meetings {
            super::ticker::start(&hub, accounts_of(running.clone()), cx);
        }
        Ok(stream)
    })
}

/// The sender `from` names: an MRI, a name, or one of the app's accounts by its UPN.
///
/// # Errors
/// A UPN (an address, not an MRI) of anyone else: no read here maps it to the person's MRI, and taken as a name it
/// would match nobody, leaving the stream silently empty.
fn sender(running: &Running, from: &str) -> Result<Sender, CtlError> {
    let from = from.trim();
    let accounts: Vec<String> = running
        .bridges()
        .iter()
        .filter(|(_, upn, _)| upn.eq_ignore_ascii_case(from))
        .map(|(mri, _, _)| mri.clone())
        .collect();
    if accounts.is_empty() && from.contains('@') && !from.contains(':') {
        return Err(CtlError::new(
            ErrorCode::BadRequest,
            "`from` takes a UPN only of one of the app's accounts: name anyone else by MRI or display name",
        ));
    }
    let mut mris = vec![from.to_owned()];
    mris.extend(accounts);
    Ok(Sender {
        mris,
        name: from.to_lowercase(),
    })
}
