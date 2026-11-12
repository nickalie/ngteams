//! A large account's first sign-in, for the tests of what the client shows while it catches up: many chats
//! ([`ChatServiceMock::add_synthetic_chats`]), a conversation list handed out in pages
//! ([`ChatServiceMock::page_conversation_list`]), and a read held open until the test lets it go
//! ([`ChatServiceMock::hold`]).
//!
//! Holds apply to the requests sent through the mock's front ([`ChatServiceMock::start_front`], `front.rs`): a held
//! request waits there alone while the others go on, as one slow read of the service's. A hold lets its requests go
//! once it is released or dropped, and after [`MAX_HOLD`] in any case, so a test that fails while holding never hangs
//! the client.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::world::{self, Kind, State};
use crate::{CHAT_BASE, ChatServiceMock, LAB_A, LAB_B};

/// The longest a request is held, released or not: less than a client's request timeout would let it wait.
pub const MAX_HOLD: Duration = Duration::from_secs(25);

/// When the newest synthetic chat's message arrived: after every message of the fixtures, so the chats sort before
/// the lab's by their last message as they do by their version.
const SYNTHETIC_NEWEST: u64 = 1_767_312_000_000;

/// The id of the synthetic chat `n` ([`ChatServiceMock::add_synthetic_chats`]).
#[must_use]
pub fn synthetic_chat(n: usize) -> String {
    format!("19:synthetic-{n:04}@thread.v2")
}

/// The topic of the synthetic chat `n`.
#[must_use]
pub fn synthetic_topic(n: usize) -> String {
    format!("Synthetic chat {n:04}")
}

/// The text of the one message the synthetic chat `n` holds.
#[must_use]
pub fn synthetic_text(n: usize) -> String {
    format!("Synthetic message {n:04}")
}

/// What a held request is let go with.
#[derive(Clone, Copy)]
enum Outcome {
    /// Answered as the world would.
    Answer,
    /// Refused with this status and no body.
    Refuse(u16),
}

#[derive(Default)]
struct Waiting {
    entered: usize,
    outcome: Option<Outcome>,
    /// A request went at [`MAX_HOLD`], before the hold let it go.
    expired: bool,
}

/// The requests a [`Hold`] matches, and whether they may go.
pub(crate) struct Gate {
    matches: Box<dyn Fn(&str) -> bool + Send + Sync>,
    waiting: Mutex<Waiting>,
    changed: Condvar,
}

impl Gate {
    /// Whether it holds a request for `target` (its path and query, as sent).
    pub(crate) fn matches(&self, target: &str) -> bool {
        (self.matches)(target)
    }

    /// Waits until the hold lets requests go ([`MAX_HOLD`] at most); the status to refuse with, if any.
    pub(crate) fn pass(&self) -> Option<u16> {
        let deadline = Instant::now() + MAX_HOLD;
        let mut waiting = self.lock();
        waiting.entered += 1;
        self.changed.notify_all();
        while waiting.outcome.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                waiting.expired = true;
                return None;
            }
            waiting = self
                .changed
                .wait_timeout(waiting, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        match waiting.outcome {
            Some(Outcome::Refuse(code)) => Some(code),
            _ => None,
        }
    }

    /// Lets the requests go with `outcome`; `false` when one already went at [`MAX_HOLD`].
    fn open(&self, outcome: Outcome) -> bool {
        let mut waiting = self.lock();
        waiting.outcome.get_or_insert(outcome);
        self.changed.notify_all();
        !waiting.expired
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Waiting> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Requests held in the mock ([`ChatServiceMock::hold`]) until [`Hold::release`] or [`Hold::refuse`], or until it is
/// dropped, which releases them.
pub struct Hold {
    gate: Arc<Gate>,
    state: Arc<Mutex<State>>,
}

impl Hold {
    /// Whether a request it holds came within `timeout`.
    #[must_use]
    pub fn wait_entered(&self, timeout: Duration) -> bool {
        let waiting = self.gate.lock();
        let (waiting, _) = self
            .gate
            .changed
            .wait_timeout_while(waiting, timeout, |waiting| waiting.entered == 0)
            .unwrap_or_else(PoisonError::into_inner);
        waiting.entered > 0
    }

    /// Lets the requests held go on, answered as the world would answer them; later ones aren't held. `false` when
    /// one went at [`MAX_HOLD`] already, before this.
    #[must_use = "false says a request wasn't held until now"]
    pub fn release(self) -> bool {
        self.gate.open(Outcome::Answer)
    }

    /// Answers the requests held with `status` and no body, before the world sees them; later ones aren't held.
    /// `false` when one went at [`MAX_HOLD`] already, before this.
    #[must_use = "false says a request wasn't held until now"]
    pub fn refuse(self, status: u16) -> bool {
        self.gate.open(Outcome::Refuse(status))
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let _ = self.gate.open(Outcome::Answer);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.holds.retain(|gate| !Arc::ptr_eq(gate, &self.gate));
    }
}

impl ChatServiceMock {
    /// Adds `count` group chats of b and a, each named [`synthetic_topic`] and holding one message of b's
    /// ([`synthetic_text`]), chat 0 the newest: a large account's chats, as its first catch-up finds them. Nobody is
    /// told of them. Their ids ([`synthetic_chat`]), the newest first; none when the fixtures have no empty chat to
    /// make their list entries from.
    pub fn add_synthetic_chats(&self, count: usize) -> Vec<String> {
        let mut state = self.lock();
        let Some(empty) = state.fixtures.entries.get(1).cloned() else {
            return Vec::new();
        };
        for n in (0..count).rev() {
            let id = synthetic_chat(n);
            let created = state.tick();
            let mut conversation = world::created(&empty, &id, Kind::Group, &[LAB_B, LAB_A], created);
            conversation.entry["threadProperties"]["topic"] = json!(synthetic_topic(n));
            let arrival = state.tick();
            let sent = SYNTHETIC_NEWEST.saturating_sub(u64::try_from(n).unwrap_or(u64::MAX).saturating_mul(1000));
            conversation.messages.push(json!({
                "sequenceId": 1,
                "conversationid": id,
                "conversationLink": format!("{CHAT_BASE}/v1/users/ME/conversations/{id}"),
                "type": "Message",
                "id": arrival.to_string(),
                "version": arrival.to_string(),
                "clientmessageid": (8_000_000_000_000_000_000_u64 + arrival).to_string(),
                "messagetype": "RichText/Html",
                "contenttype": "Text",
                "content": format!("<p>{}</p>", synthetic_text(n)),
                "from": crate::writes::contact(LAB_B),
                "imdisplayname": "Lab B",
                "composetime": world::iso_time(sent, 7),
                "originalarrivaltime": world::iso_time(sent, 7),
                "properties": {},
            }));
            conversation.version = arrival;
            state.conversations.push(conversation);
        }
        (0..count).map(synthetic_chat).collect()
    }

    /// Edits the one message of the synthetic chat `n` to say `text`, as a newer version of it that every later read
    /// gives (the conversation list's `lastMessage` too) while the chat keeps its place in the list. Nobody is told.
    /// `false` when there is no such chat.
    #[must_use = "false says there was no such chat"]
    pub fn edit_synthetic_message(&self, n: usize, text: &str) -> bool {
        let mut state = self.lock();
        let version = state.tick();
        let Some(message) = state
            .conversation_mut(&synthetic_chat(n))
            .and_then(|conversation| conversation.messages.first_mut())
        else {
            return false;
        };
        message["content"] = json!(format!("<p>{text}</p>"));
        message["version"] = json!(version.to_string());
        true
    }

    /// From now on the conversation list is handed out newest first, `size` conversations a page, each page but the
    /// last with a `forwardLink` to the next.
    pub fn page_conversation_list(&self, size: usize) {
        self.lock().list_page = Some(size.max(1));
    }

    /// Holds every request sent through the front whose target (its path and query, as sent) `matches` takes, from
    /// now on, until the hold returned lets them go (see the module docs).
    pub fn hold(&self, matches: impl Fn(&str) -> bool + Send + Sync + 'static) -> Hold {
        let gate = Arc::new(Gate {
            matches: Box::new(matches),
            waiting: Mutex::default(),
            changed: Condvar::new(),
        });
        self.lock().holds.push(Arc::clone(&gate));
        Hold {
            gate,
            state: Arc::clone(&self.state),
        }
    }
}
