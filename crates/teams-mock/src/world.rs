//! The mock's state: its accounts and their tokens, its conversations with their members and messages, the events
//! queued per member, and its clock. Seeded from the fixtures ([`State::lab`]).

use std::collections::BTreeMap;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::fixtures::{FixtureError, Fixtures, object};
use crate::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AX_CHAT, CHANNEL, CHAT_BASE, ChatEvent, EventKind, LAB_A, LAB_B, LAB_C, NOTIFICATIONS,
    OUTSIDER, TEAM_CHANNEL,
};

/// The mock's clock starts after every time in the fixtures, and moves one millisecond per change.
const CLOCK_START: u64 = 1_760_001_000_000;

/// When the a–x chat (no fixture of its own) was made.
const AX_CREATED: u64 = 1_760_000_500_000;

pub(crate) struct Account {
    pub(crate) mri: String,
    /// The name the service takes from the account's token (`fromDisplayNameInToken`).
    pub(crate) display_name: String,
    pub(crate) token: String,
    pub(crate) skypetoken: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    OneToOne,
    Group,
    /// A team channel (written as a chat is, `Conversation::is_channel`) or a notifications stream (never written).
    ReadOnly,
}

pub(crate) struct Conversation {
    pub(crate) id: String,
    pub(crate) kind: Kind,
    pub(crate) members: Vec<String>,
    /// Who made the chat (empty for a read-only conversation).
    pub(crate) creator: String,
    pub(crate) created: u64,
    /// Its members' `rosterVersion`: when it was made, else when people were last added.
    pub(crate) roster: u64,
    /// The conversation's `version`: the time of its last change.
    pub(crate) version: u64,
    /// Its messages in arrival order, each as a read gives it.
    pub(crate) messages: Vec<Value>,
    /// Each member's own read horizon (`consumptionhorizon`), by MRI.
    pub(crate) horizons: BTreeMap<String, String>,
    /// Its conversation list entry as a fixture gave it; a read fills in what changes.
    pub(crate) entry: Value,
    /// In its members' conversation lists: a team's channel other than its general one isn't (M2 Task 16's probe: the
    /// chat service listed 2 of 10 channels, the teams' general ones), and is read by its id alone.
    pub(crate) listed: bool,
}

impl Conversation {
    pub(crate) fn is_member(&self, mri: &str) -> bool {
        self.members.iter().any(|member| member == mri)
    }

    /// Whether it is a team's channel (its thread a `space` or a `topic`), which its members write in.
    pub(crate) fn is_channel(&self) -> bool {
        matches!(
            self.entry["threadProperties"]["threadType"].as_str(),
            Some("space" | "topic")
        )
    }

    /// The message with `id`, if any.
    pub(crate) fn message_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.messages
            .iter_mut()
            .find(|message| message["id"].as_str() == Some(id))
    }

    /// The next message's `sequenceId`.
    pub(crate) fn next_sequence(&self) -> u64 {
        self.messages
            .last()
            .and_then(|message| message["sequenceId"].as_u64())
            .map_or(1, |last| last + 1)
    }
}

pub(crate) struct State {
    pub(crate) fixtures: Fixtures,
    pub(crate) accounts: Vec<Account>,
    pub(crate) conversations: Vec<Conversation>,
    /// Events queued per member, by MRI.
    pub(crate) events: BTreeMap<String, Vec<ChatEvent>>,
    /// The endpoints registered with the registrar (the mock's Trouter delivers only to those).
    pub(crate) registrations: Vec<crate::trouter::Registration>,
    /// The own-availability writes UPS took, in order (`ups.rs`).
    pub(crate) availability: Vec<crate::ups::OwnAvailability>,
    /// The channels made and the writes taken (`channels.rs`).
    pub(crate) channels: crate::channels::Channels,
    /// The calling service's calls and the deliveries waiting for calling endpoints (`calling.rs`).
    pub(crate) calling: crate::calling::Calling,
    /// The calling service's media, once a test turned it on (`peers.rs`).
    #[cfg(feature = "media")]
    pub(crate) peers: crate::peers::Peers,
    /// Counts the changes the mock's Trouter acts on (a queued event, a registration), so its sockets wake up.
    pub(crate) changes: watch::Sender<u64>,
    /// The account's own conversation properties it set, by (account MRI, conversation): a channel's
    /// `channelNotificationSettings` and `followAllRc` (`docs/protocol/channels.md`).
    pub(crate) user_graph_settings: BTreeMap<String, String>,
    pub(crate) own_properties: BTreeMap<(String, String), serde_json::Map<String, Value>>,
    /// The conversation list's page size, once a test asked for pages (`startup.rs`).
    pub(crate) list_page: Option<usize>,
    /// The requests a test holds (`startup.rs`).
    pub(crate) holds: Vec<std::sync::Arc<crate::startup::Gate>>,
    clock: u64,
    groups_made: u64,
}

impl State {
    /// The world of the crate docs, from the fixtures.
    pub(crate) fn lab() -> Result<Self, FixtureError> {
        let fixtures = Fixtures::load()?;
        let accounts = [
            (LAB_A, "Lab A"),
            (LAB_B, "Lab B"),
            (LAB_C, "Lab C"),
            (OUTSIDER, "Outsider X"),
        ]
        .into_iter()
        .map(|(mri, name)| Account {
            mri: mri.to_owned(),
            display_name: name.to_owned(),
            token: initial_token(mri),
            skypetoken: None,
        })
        .collect();
        let conversations = seed(&fixtures)?;
        Ok(Self {
            fixtures,
            accounts,
            conversations,
            events: BTreeMap::new(),
            registrations: Vec::new(),
            availability: Vec::new(),
            channels: crate::channels::Channels::default(),
            calling: crate::calling::Calling::default(),
            #[cfg(feature = "media")]
            peers: crate::peers::Peers::default(),
            changes: watch::Sender::new(0),
            own_properties: BTreeMap::new(),
            user_graph_settings: BTreeMap::new(),
            list_page: None,
            holds: Vec::new(),
            clock: CLOCK_START,
            groups_made: 0,
        })
    }

    /// The time of a new change: one millisecond after the last.
    pub(crate) fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub(crate) fn now(&self) -> u64 {
        self.clock
    }

    pub(crate) fn account(&self, mri: &str) -> Option<&Account> {
        self.accounts.iter().find(|account| account.mri == mri)
    }

    pub(crate) fn account_by_token(&self, token: &str) -> Option<&Account> {
        self.accounts.iter().find(|account| account.token == token)
    }

    pub(crate) fn set_token(&mut self, mri: &str, token: String) -> bool {
        match self.accounts.iter_mut().find(|account| account.mri == mri) {
            Some(account) => {
                account.token = token;
                true
            }
            None => false,
        }
    }

    pub(crate) fn conversation(&self, id: &str) -> Option<&Conversation> {
        self.conversations.iter().find(|conversation| conversation.id == id)
    }

    pub(crate) fn conversation_mut(&mut self, id: &str) -> Option<&mut Conversation> {
        self.conversations.iter_mut().find(|conversation| conversation.id == id)
    }

    /// A new group chat's id, `19:<32 hex>@thread.v2`.
    pub(crate) fn new_group_id(&mut self) -> String {
        self.groups_made += 1;
        format!("19:{:032x}@thread.v2", 0x1_0000 + self.groups_made)
    }

    /// Queues `kind` with `resource` for every member of `conversation` but `from`.
    pub(crate) fn queue(&mut self, conversation: &str, from: &str, kind: EventKind, resource: &Value) {
        let Some(members) = self.conversation(conversation).map(|found| found.members.clone()) else {
            return;
        };
        for member in members.into_iter().filter(|member| member != from) {
            self.events.entry(member).or_default().push(ChatEvent {
                conversation: conversation.to_owned(),
                from: from.to_owned(),
                kind,
                resource: resource.clone(),
            });
        }
        self.changed();
    }

    /// Wakes the mock's Trouter sockets up.
    pub(crate) fn changed(&self) {
        self.changes.send_modify(|count| *count += 1);
    }
}

/// The token the mock first hands out for `mri`: token-shaped (letters, digits, `-`), never a real one.
fn initial_token(mri: &str) -> String {
    let oid = mri.trim_start_matches("8:orgid:");
    format!("mock-ic3-token-{oid}-0")
}

/// The conversations of the crate docs' world.
fn seed(fixtures: &Fixtures) -> Result<Vec<Conversation>, FixtureError> {
    let [with_messages, empty, channel, notifications] = fixtures.entries.as_slice() else {
        return Err(FixtureError::from_message("conversations.json: not four entries"));
    };
    let ab = listed(
        with_messages,
        Kind::OneToOne,
        &[LAB_A, LAB_B],
        fixtures.ab_messages.clone(),
    );
    let ac = listed(empty, Kind::OneToOne, &[LAB_A, LAB_C], Vec::new());
    let ax = created(empty, AX_CHAT, Kind::OneToOne, &[LAB_A, OUTSIDER], AX_CREATED);
    let group_created = fixtures.thread["rosterVersion"]
        .as_u64()
        .ok_or_else(|| FixtureError::from_message("thread-group.json: no rosterVersion"))?;
    let abc = created(empty, ABC_CHAT, Kind::Group, &[LAB_A, LAB_B, LAB_C], group_created);
    let channel = listed(channel, Kind::ReadOnly, &[LAB_A], Vec::new());
    let stream = vec![notifications["lastMessage"].clone()];
    let notifications = listed(notifications, Kind::ReadOnly, &[LAB_A], stream);
    let team_channel = unlisted_channel(fixtures.channel_messages.clone());
    let conversations = vec![ab, ac, ax, abc, channel, notifications, team_channel];
    let ids: Vec<&str> = conversations.iter().map(|found| found.id.as_str()).collect();
    if ids
        == [
            AB_CHAT,
            AC_CHAT,
            AX_CHAT,
            ABC_CHAT,
            CHANNEL,
            NOTIFICATIONS,
            TEAM_CHANNEL,
        ]
    {
        Ok(conversations)
    } else {
        Err(FixtureError::from_message("conversations.json: other conversation ids"))
    }
}

/// A conversation as its list entry in `conversations.json` gives it.
fn listed(entry: &Value, kind: Kind, members: &[&str], messages: Vec<Value>) -> Conversation {
    let horizons = entry["properties"]["consumptionhorizon"]
        .as_str()
        .map(|horizon| BTreeMap::from([(LAB_A.to_owned(), horizon.to_owned())]))
        .unwrap_or_default();
    Conversation {
        id: entry["id"].as_str().unwrap_or_default().to_owned(),
        kind,
        members: members.iter().map(|&member| member.to_owned()).collect(),
        creator: if kind == Kind::ReadOnly {
            String::new()
        } else {
            LAB_A.to_owned()
        },
        created: entry["threadProperties"]["rosterVersion"].as_u64().unwrap_or_default(),
        roster: entry["threadProperties"]["rosterVersion"].as_u64().unwrap_or_default(),
        version: entry["version"].as_u64().unwrap_or_default(),
        messages,
        horizons,
        entry: entry.clone(),
        listed: true,
    }
}

/// The team's other channel ([`TEAM_CHANNEL`], the `topics` entry of the team in `conversations.json`): a, b and c
/// its members, `messages` its posts and replies, in nobody's conversation list, its thread a `topic`.
fn unlisted_channel(messages: Vec<Value>) -> Conversation {
    let created = messages
        .first()
        .and_then(|message| message["version"].as_str())
        .and_then(|version| version.parse().ok())
        .unwrap_or_default();
    Conversation {
        id: TEAM_CHANNEL.to_owned(),
        kind: Kind::ReadOnly,
        members: [LAB_A, LAB_B, LAB_C].map(str::to_owned).to_vec(),
        creator: String::new(),
        created,
        roster: created,
        version: created,
        messages,
        horizons: BTreeMap::new(),
        entry: json!({"threadProperties": {"threadType": "topic"}}),
        listed: false,
    }
}

/// A chat made by its first member at `created`, with no messages; its list entry is the empty 1:1 chat's of
/// `conversations.json` with this chat's ids and times, and for a group chat without the keys only a 1:1 chat showed
/// (`productThreadType`, `uniquerosterthread`): a group chat's list entry wasn't seen.
pub(crate) fn created(empty: &Value, id: &str, kind: Kind, members: &[&str], created: u64) -> Conversation {
    let creator = members.first().copied().unwrap_or_default();
    let mut entry = empty.clone();
    let fields = object(&mut entry);
    fields.insert("targetLink".to_owned(), json!(format!("{CHAT_BASE}/v1/threads/{id}")));
    fields.insert(
        "messages".to_owned(),
        json!(format!("{CHAT_BASE}/v1/users/ME/conversations/{id}/messages")),
    );
    object(fields.entry("properties").or_insert(Value::Null)).insert("addedBy".to_owned(), json!(creator));
    let thread = object(fields.entry("threadProperties").or_insert(Value::Null));
    thread.insert("originalThreadId".to_owned(), json!(id));
    thread.insert("createdat".to_owned(), json!(iso_time(created, 3)));
    thread.insert("lastjoinat".to_owned(), json!(created.to_string()));
    thread.insert("rosterVersion".to_owned(), json!(created));
    thread.insert("version".to_owned(), json!(created.to_string()));
    if kind == Kind::Group {
        thread.remove("productThreadType");
        thread.remove("uniquerosterthread");
    }
    Conversation {
        id: id.to_owned(),
        kind,
        members: members.iter().map(|&member| member.to_owned()).collect(),
        creator: creator.to_owned(),
        created,
        roster: created,
        version: created,
        messages: Vec::new(),
        horizons: BTreeMap::new(),
        entry,
        listed: true,
    }
}

/// `ms` (since the Unix epoch) as the service writes a time: `2026-01-01T00:00:00.0000000Z`, with `fraction_digits`
/// (up to 7) digits after the second.
pub(crate) fn iso_time(ms: u64, fraction_digits: usize) -> String {
    let seconds = ms / 1000;
    let (days, of_day) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_date(days);
    let fraction = format!("{:03}0000", ms % 1000);
    let fraction = fraction.get(..fraction_digits.min(7)).unwrap_or_default();
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// The Gregorian date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_date(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let of_era = shifted - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * of_year + 2) / 153;
    let day = of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::iso_time;

    #[test]
    fn times_are_written_as_the_service_writes_them() {
        assert_eq!(iso_time(1_767_225_600_000, 7), "2026-01-01T00:00:00.0000000Z");
        assert_eq!(iso_time(1_767_225_650_123, 7), "2026-01-01T00:00:50.1230000Z");
        assert_eq!(iso_time(1_767_225_600_400, 3), "2026-01-01T00:00:00.400Z");
        assert_eq!(iso_time(951_782_400_000, 7), "2000-02-29T00:00:00.0000000Z");
        assert_eq!(iso_time(0, 7), "1970-01-01T00:00:00.0000000Z");
    }
}
