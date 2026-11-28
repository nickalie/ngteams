//! The people reads on the wire: the people search's rows ([`PeopleRow`]), one person's profile card
//! ([`PersonCard`]) and others' presence ([`PresenceRow`]).
//!
//! A person is named by their MRI (`8:orgid:<id>`), their UPN or email address, or their display name as the people
//! search finds them. A name or an address that nobody has is [`super::ErrorCode::UnknownPerson`]. A name that
//! several people have is [`super::ErrorCode::AmbiguousPerson`], with each one's MRI and name in
//! [`super::CtlError::candidates`].

use serde::{Deserialize, Serialize};

/// How long the app takes at most to answer `people` or `person`: the people search, then for a card its reads, each
/// bounded by the transport's 30 s. Past it the app answers [`super::ErrorCode::Timeout`]. A card whose presence read
/// failed then waits [`PRESENCE_WAIT`] more.
pub const PEOPLE_ANSWER: std::time::Duration = std::time::Duration::from_secs(60);

/// How long `presence` (and a card whose presence read failed) waits at most for the presence of a person that the
/// app did not know yet: the engine's next presence read (10 s after the one before it at most) and its answer. Past
/// it the person's presence stays `unknown`.
pub const PRESENCE_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// The most people one `presence` asks about: the number of people the engine watches at once. The views share that
/// watch, so someone new that it asks about drops the badge asked about longest ago until its view asks again.
pub const PRESENCE_MOST: usize = 20;

/// A person that the people search found, as the search's People tab lists them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeopleRow {
    /// Their MRI, when the service gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mri: Option<String>,
    pub name: String,
    /// Their UPN or mail address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    /// Their availability as the app knows it now (as [`PresenceRow::availability`]); absent when it is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<String>,
}

/// Someone that a card names: a manager, a direct report, a person they work with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPersonRow {
    pub mri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// One person's profile card, as the app's card shows it. Each field is absent when its read failed or gave nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonCard {
    pub mri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upn: Option<String>,
    /// Their mail addresses, the main one first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emails: Vec<String>,
    /// `Member`, `Guest`, …, as the directory gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub office: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile_phone: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub business_phones: Vec<String>,
    /// Their availability, as the presence badges name it (`available`, `busy`, `do-not-disturb`, `offline`, …, or
    /// the service's own word for one the app does not name); absent when it is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<String>,
    /// When they were last active (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_of_office: Option<bool>,
    /// Their calendar's status now (`free`, `tentative`, `busy`, `out_of_office`, `working_elsewhere`, `unknown`, or
    /// the service's own word).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_busy: Option<String>,
    /// The status that follows it, and when (ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_free_busy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_busy_until: Option<i64>,
    /// Nearest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managers: Vec<CardPersonRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub direct_reports: Vec<CardPersonRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub works_with: Vec<CardPersonRow>,
}

/// One person's presence, as the presence badges show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceRow {
    pub mri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// As the badges name it (`available`, `busy`, `do-not-disturb`, `away`, `offline`, …, or the service's own
    /// word), `unknown` when the app could not read it in [`PRESENCE_WAIT`].
    pub availability: String,
}
