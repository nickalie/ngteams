//! What a tenant's compliance and security policies did to a message (`properties.policyViolation`,
//! `docs/protocol/chat.md`, "Messages a policy blocked"), and the service's own stream that keeps the sender a copy of
//! what it blocked ([`GENERIC_STREAM`]).
//!
//! A verdict in a form no capture or the web client's reading showed is [`Field::Unknown`], and a state or a kind it
//! doesn't name is kept as it came: a view shows a notice only for the verdicts [`PolicyViolation::notice`] names.

use serde_json::{Map, Value};

use crate::chat::model::Field;

/// The account's own stream of the copies the service keeps of messages a policy blocked (`threadtype`
/// `streamofgeneric`): each copy holds the blocked message's text and files, under the id the blocked message's
/// verdict names ([`PolicyViolation::generic_stream_item`]). It is no chat: its thread can't be read.
pub const GENERIC_STREAM: &str = "48:generic";

/// Whether `conversation` is the account's stream of blocked messages' copies ([`GENERIC_STREAM`]).
#[must_use]
pub fn is_generic_stream(conversation: &str) -> bool {
    conversation == GENERIC_STREAM
}

/// What the policy did: `state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyState {
    /// 0: nothing (an override or a report that sent it lifts the block).
    None,
    /// 1: the message's content is withheld from everyone but its sender.
    Blocked,
    /// 2: the message is delivered with a warning (flagged).
    Warning,
    /// Any other number, as it came.
    Other(u64),
}

/// Which check acted: `violationType`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViolationKind {
    /// `dlp`: a data loss prevention policy.
    Dlp,
    /// `zap`: zero-hour auto purge, harmful content found after delivery.
    Zap,
    /// `wft`: a file of a type the tenant doesn't permit.
    FileType,
    /// `mu`: a link with a bad reputation.
    LinkReputation,
    /// `pd`: a phishing attempt.
    Phishing,
    /// Any other, as it came (`cc` among them).
    Other(String),
}

impl ViolationKind {
    fn of(name: &str) -> Self {
        match name {
            "dlp" => Self::Dlp,
            "zap" => Self::Zap,
            "wft" => Self::FileType,
            "mu" => Self::LinkReputation,
            "pd" => Self::Phishing,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// `userAction`'s flag for a sender who reported the verdict as a mistake.
const REPORTED_FALSE_POSITIVE: u64 = 2;

/// A message's policy verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyViolation {
    pub state: PolicyState,
    pub kind: Option<ViolationKind>,
    /// `verdictDetails`: flags of what the sender may do (1 report a mistake, 2 override, 4 override with a reason).
    pub verdict_details: u64,
    /// `dlpAction`: flags of what the policy does (1 notify the sender, 2 block everyone, 4 block outsiders).
    pub dlp_action: u64,
    /// `userAction`: flags of what the sender did (1 overrode it, 2 reported it as a mistake).
    pub user_action: u64,
    /// `justificationText`: the sender's reason for an override, or the check's own name.
    pub justification: Option<String>,
    /// `genericStreamItemId`: the id of the copy in [`GENERIC_STREAM`].
    pub generic_stream_item: Option<String>,
}

/// What a view says of a message a policy acted on, as the web client words each case (the words are the view's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyNotice {
    /// Blocked for a file type the tenant doesn't permit; told to everyone.
    FileTypeBlocked,
    /// Blocked as harmful content found after delivery; told to everyone.
    HarmfulContentBlocked,
    /// Delivered with a warning of a harmful link; told to everyone.
    HarmfulLink,
    /// Delivered with a warning of phishing; told to everyone.
    Phishing,
    /// Blocked by any other check; `reported` once the sender reported it as a mistake.
    Blocked { reported: bool },
    /// Flagged by any other check; told to its sender alone.
    Flagged { reported: bool },
}

impl PolicyViolation {
    /// The verdict in `value`, `properties.policyViolation` as it came: an object (as delivered) or its JSON text, its
    /// `state` a number; absent when there is none, [`Field::Unknown`] in any other form (`null` too).
    #[must_use]
    pub fn of(value: Option<&Value>) -> Field<Self> {
        let Some(value) = value else {
            return Field::Absent;
        };
        let parsed;
        let object = match value {
            Value::Object(object) => Some(object),
            Value::String(text) => {
                parsed = serde_json::from_str::<Value>(text).ok();
                parsed.as_ref().and_then(Value::as_object)
            }
            _ => None,
        };
        object
            .and_then(Self::from_object)
            .map_or_else(|| Field::Unknown(value.clone()), Field::Known)
    }

    fn from_object(object: &Map<String, Value>) -> Option<Self> {
        let state = match flags(object.get("state")).ok()? {
            Some(0) => PolicyState::None,
            Some(1) => PolicyState::Blocked,
            Some(2) => PolicyState::Warning,
            Some(other) => PolicyState::Other(other),
            None => return None,
        };
        let text = |name: &str| match object.get(name) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(text)) => Some(Some(text.clone())),
            Some(_) => None,
        };
        let item = match object.get("genericStreamItemId") {
            None | Some(Value::Null) => None,
            Some(Value::Number(number)) => Some(number.as_u64()?.to_string()),
            Some(Value::String(id)) if !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()) => {
                Some(id.clone())
            }
            Some(_) => return None,
        };
        Some(Self {
            state,
            kind: text("violationType")?.map(|name| ViolationKind::of(&name)),
            verdict_details: flags(object.get("verdictDetails")).ok()?.unwrap_or_default(),
            dlp_action: flags(object.get("dlpAction")).ok()?.unwrap_or_default(),
            user_action: flags(object.get("userAction")).ok()?.unwrap_or_default(),
            justification: text("justificationText")?,
            generic_stream_item: item,
        })
    }

    /// What a view tells of it, to its sender (`own`) or to anyone else, as the web client chooses: `None` when it
    /// tells nothing (no verdict, a state it doesn't name, any other warning to anyone but the sender).
    #[must_use]
    pub fn notice(&self, own: bool) -> Option<PolicyNotice> {
        let reported = self.user_action == REPORTED_FALSE_POSITIVE;
        match (self.state, &self.kind) {
            (PolicyState::Blocked, Some(ViolationKind::FileType)) => Some(PolicyNotice::FileTypeBlocked),
            (PolicyState::Blocked, Some(ViolationKind::Zap)) => Some(PolicyNotice::HarmfulContentBlocked),
            (PolicyState::Warning, Some(ViolationKind::LinkReputation)) => Some(PolicyNotice::HarmfulLink),
            (PolicyState::Warning, Some(ViolationKind::Phishing)) => Some(PolicyNotice::Phishing),
            (PolicyState::Blocked, _) => Some(PolicyNotice::Blocked { reported }),
            (PolicyState::Warning, _) if own => Some(PolicyNotice::Flagged { reported }),
            _ => None,
        }
    }

    /// Whether the message's content is withheld: everyone but its sender sees the notice alone.
    #[must_use]
    pub fn blocks(&self) -> bool {
        self.state == PolicyState::Blocked
    }
}

/// A number field: `Ok(None)` when it is missing or `null`, an error in a form no capture showed.
fn flags(value: Option<&Value>) -> Result<Option<u64>, Unread> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number.as_u64().map(Some).ok_or(Unread),
        Some(_) => Err(Unread),
    }
}

/// A field in a form no capture showed.
struct Unread;
