//! Teams participant and conversation identifiers and shared service names.

use std::fmt;

/// The commercial cloud's Azure Front Door hosts for Teams services.
pub const AFD_HOSTS: [&str; 2] = ["teams.microsoft.com", "teams.cloud.microsoft"];

/// The regional chat-service host suffix, matched on a dot boundary.
pub(crate) const REGIONAL_SUFFIX: &str = ".msg.teams.microsoft.com";

/// The main flight proxy host: every call start goes here. A meeting hosted in another region hands out its links on
/// that region's own flight proxy host, `api-<region>.flightproxy.teams.microsoft.com`.
pub const CALL_PROXY_HOST: &str = "api.flightproxy.teams.microsoft.com";

/// The call controller's conversations endpoint, without a query.
pub const CALL_START_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";

/// The conversation scenario of the web client's test call.
pub const TEST_CALL_SCENARIO: &str = "UserInitiatedTestCall";

/// The echo service's MRI in the web client's commercial configuration (`teamsEchoBotMri`).
pub const TEAMS_ECHO_BOT_MRI: &str = "28:cf28171e-fcfd-47e4-a1d6-79460b0b3ca0";

/// Trouter invitation event numbers for 1:1 (107) and group (109) calls.
pub const INVITATION_EVENTS: [u64; 2] = [107, 109];

/// The suffix of a 1:1 conversation id.
const ONE_TO_ONE_SUFFIX: &str = "@unq.gbl.spaces";

/// A participant's MRI (`8:orgid:<guid>`, `28:<bot>`, …). Compared exactly: a lookalike in another case or with extra
/// characters is a different participant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mri(String);

impl Mri {
    #[must_use]
    pub fn new(mri: impl Into<String>) -> Self {
        Self(mri.into())
    }

    /// A participant known by a mail address alone: a calendar event's attendee or organizer, whom Exchange reaches by
    /// address. Kept as `smtp:<address in lower case>`, a form distinct from a Teams MRI.
    #[must_use]
    pub fn mail(address: &str) -> Self {
        Self(format!("{MAIL_PREFIX}{}", address.trim().to_lowercase()))
    }

    /// The address of a participant made by [`Mri::mail`].
    #[must_use]
    pub fn mail_address(&self) -> Option<&str> {
        self.0.strip_prefix(MAIL_PREFIX)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is an app's or a bot's MRI (`28:…`).
    #[must_use]
    pub fn is_bot(&self) -> bool {
        self.0.starts_with("28:")
    }
}

/// The prefix of a participant known by a mail address ([`Mri::mail`]).
const MAIL_PREFIX: &str = "smtp:";

impl fmt::Display for Mri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether `id` is a recognized conversation id: a 1:1 chat, group chat or meeting chat.
/// Looser `19:…@unq.gbl.spaces` and `19:…@thread.v2` strings are not accepted.
#[must_use]
pub fn is_chat_id(id: &str) -> bool {
    one_to_one_pair(id).is_some() || is_group_chat_id(id) || is_meeting_chat_id(id)
}

/// The two members a 1:1 chat id names: `19:<oid>_<oid>@unq.gbl.spaces` → `8:orgid:<oid>` each, lowercased. `None`
/// for any other id.
#[must_use]
pub fn one_to_one_pair(id: &str) -> Option<[Mri; 2]> {
    let pair = id.strip_prefix("19:")?.strip_suffix(ONE_TO_ONE_SUFFIX)?;
    let (first, second) = pair.split_once('_')?;
    if !is_guid(first) || !is_guid(second) || first.eq_ignore_ascii_case(second) {
        return None;
    }
    let mri = |oid: &str| Mri::new(format!("8:orgid:{}", oid.to_ascii_lowercase()));
    Some([mri(first), mri(second)])
}

/// The 1:1 chat id of two work accounts, as the web client builds it: `19:<oid>_<oid>@unq.gbl.spaces`, the two object
/// ids (lowercase) sorted; [`one_to_one_pair`] reads it back. `None` unless both MRIs are `8:orgid:<oid>` (a GUID) of
/// two different accounts.
#[must_use]
pub fn one_to_one_id(mri_a: &str, mri_b: &str) -> Option<String> {
    let oid = |mri: &str| {
        let oid = mri.strip_prefix("8:orgid:")?;
        is_guid(oid).then(|| oid.to_ascii_lowercase())
    };
    let mut oids = [oid(mri_a)?, oid(mri_b)?];
    oids.sort();
    let [first, second] = oids;
    (first != second).then(|| format!("19:{first}_{second}{ONE_TO_ONE_SUFFIX}"))
}

/// The suffix of a group (or meeting) chat's conversation id.
const GROUP_SUFFIX: &str = "@thread.v2";

/// Whether `id` is a group chat's conversation id: `19:<id>@thread.v2`, the id 1–128 ASCII letters, digits, `_` or
/// `-`, and not a meeting's chat (`19:meeting_…`, [`is_meeting_chat_id`]).
#[must_use]
pub fn is_group_chat_id(id: &str) -> bool {
    thread_v2(id).is_some_and(|inner| !has_meeting_prefix(inner))
}

/// Whether `id` is a meeting chat's conversation id: `19:meeting_<id>@thread.v2` (`meeting_` in any case), the whole
/// id 1–128 ASCII letters, digits, `_` or `-`, with something after `meeting_`. No id is both this and a group chat's
/// ([`is_group_chat_id`]).
#[must_use]
pub fn is_meeting_chat_id(id: &str) -> bool {
    thread_v2(id).is_some_and(|inner| has_meeting_prefix(inner) && inner.len() > MEETING_PREFIX.len())
}

/// Whether `id` is a team's or a channel's thread id: `19:<id>@thread.tacv2` or `19:<id>@thread.skype`, the id 1–128
/// ASCII letters, digits, `_` or `-` (no reply chain).
#[must_use]
pub fn is_team_thread_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("19:") else {
        return false;
    };
    let Some(inner) = rest
        .strip_suffix("@thread.tacv2")
        .or_else(|| rest.strip_suffix("@thread.skype"))
    else {
        return false;
    };
    (1..=128).contains(&inner.len())
        && inner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// What joins a channel's id to a post's in a reply chain's id (`docs/protocol/chat-service.md`, "Ids").
const CHAIN_JOIN: &str = ";messageid=";

/// The reply chain of post `post` (its message id, digits) in channel `channel` (a team's thread):
/// `<channel>;messageid=<post>`, the conversation a reply in the post's thread is posted to, as the web client joins
/// them. `None` for a channel or a post in another form.
#[must_use]
pub fn reply_chain_id(channel: &str, post: &str) -> Option<String> {
    let post_reads = !post.is_empty() && post.bytes().all(|byte| byte.is_ascii_digit());
    (is_team_thread_id(channel) && post_reads).then(|| format!("{channel}{CHAIN_JOIN}{post}"))
}

/// The channel and the post a reply chain's id names ([`reply_chain_id`]); `None` for any other id.
#[must_use]
pub fn reply_chain(id: &str) -> Option<(&str, &str)> {
    let (channel, post) = id.split_once(CHAIN_JOIN)?;
    reply_chain_id(channel, post).map(|_| (channel, post))
}

/// The conversation a message written to `id` is stored in: a reply chain's channel ([`reply_chain`]), else `id`.
#[must_use]
pub fn stored_conversation(id: &str) -> &str {
    reply_chain(id).map_or(id, |(channel, _)| channel)
}

/// The prefix of a meeting chat's inner id.
const MEETING_PREFIX: &str = "meeting_";

/// The id inside `19:<id>@thread.v2`, when it is 1–128 ASCII letters, digits, `_` or `-`.
fn thread_v2(id: &str) -> Option<&str> {
    let inner = id.strip_prefix("19:")?.strip_suffix(GROUP_SUFFIX)?;
    ((1..=128).contains(&inner.len())
        && inner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then_some(inner)
}

/// Whether a `@thread.v2` chat's inner id begins with `meeting_`, in any case.
fn has_meeting_prefix(inner: &str) -> bool {
    inner
        .get(..MEETING_PREFIX.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(MEETING_PREFIX))
}

pub(crate) fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
