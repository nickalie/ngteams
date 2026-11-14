//! Typed POST reads. The transport validates each captured request shape and sends it once.

use std::fmt;

use crate::Mri;
use serde::Deserialize;
use serde_json::Value;

use crate::endpoint::{Endpoint, Method, Service};
use crate::own::{CLIENT_BUILD, RegisteredEndpoint, is_guid, is_push_url_in};
use crate::request::Request;

/// A reviewed read by `POST` (see the module docs). Sealed: no other crate can implement it.
pub trait PostRead: sealed::Sealed {
    /// The entry's name for request validation and diagnostics.
    const KIND: &'static str;

    /// The MRIs it asks about, in the order its body lists them.
    fn names(&self) -> &[Mri];
}

pub(crate) mod sealed {
    use super::{PostReadWire, Shape};

    /// The seal on [`PostRead`](super::PostRead): a public trait no other crate can name, implemented for this crate's
    /// entries only. It carries the entry's captured shape and its wire form, built from the same fields as its names.
    pub trait Sealed {
        /// The shape the capture showed, which the transport checks the request it sends against.
        fn shape(&self) -> &'static Shape;

        /// What the transport sends.
        fn wire(&self) -> PostReadWire;
    }

    impl Sealed for super::ShortProfileLookup {
        fn shape(&self) -> &'static Shape {
            &super::SHORT_PROFILE
        }

        fn wire(&self) -> PostReadWire {
            PostReadWire::lookup(&super::SHORT_PROFILE, &self.names)
        }
    }

    impl Sealed for super::PresenceRead {
        fn shape(&self) -> &'static Shape {
            &super::PRESENCE
        }

        fn wire(&self) -> PostReadWire {
            PostReadWire::lookup(&super::PRESENCE, &self.names)
        }
    }

    impl Sealed for super::PresenceSubscription {
        fn shape(&self) -> &'static Shape {
            &super::PRESENCE_SUBSCRIPTION
        }

        fn wire(&self) -> PostReadWire {
            PostReadWire {
                endpoint: self.wire_endpoint(),
                headers: self.wire_headers(),
                body: self.wire_body(),
            }
        }
    }

    #[cfg(feature = "test-support")]
    impl Sealed for super::ProbePostRead {
        fn shape(&self) -> &'static Shape {
            self.shape
        }

        fn wire(&self) -> PostReadWire {
            let mut wire = PostReadWire::lookup(self.shape, &self.names);
            for (name, value) in &self.query {
                wire.endpoint = wire.endpoint.with_query(name.clone(), value.clone());
            }
            wire.headers.extend(self.headers.iter().cloned());
            if let Some(body) = &self.body {
                wire.body = body.clone();
            }
            wire
        }
    }
}

/// An entry's shape as the capture showed it: the service, the path under its base (and whether the account's own
/// endpoint ends it), the query (each pair once, nothing else, in any order), the headers it is sent with besides the
/// token (each once, exactly these values, all of them; and each of its id headers once) and its body's schema. Only
/// this crate makes or reads one.
#[derive(Debug)]
pub struct Shape {
    service: Service,
    path: &'static [&'static str],
    /// The path ends with one more segment, the account's Trouter endpoint (its `epid`, a lowercase GUID).
    endpoint: bool,
    query: &'static [(&'static str, &'static str)],
    headers: &'static [(&'static str, &'static str)],
    /// Headers that carry an id rather than a fixed value, each sent once (see [`IdHeader`]).
    ids: &'static [(&'static str, IdHeader)],
    body: Body,
}

/// What an id header of a [`Shape`] carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdHeader {
    /// A fresh lowercase GUID for each request, naming nothing of the account (a correlation id).
    Fresh,
    /// The account's Trouter endpoint, the one the path ends with.
    Endpoint,
    /// The client build the endpoint's registration names ([`CLIENT_BUILD`]).
    Build,
}

/// The schema of an entry's body, which names the MRIs it asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Body {
    /// A non-empty JSON list of MRIs: `["8:orgid:…", …]` (the profile lookups).
    Mris,
    /// A non-empty JSON list of `{"mri": "8:orgid:…"}` objects, the key exactly so and nothing else beside it (the
    /// presence read; the web client's own `"source": "ups"` beside the account's own MRI isn't sent).
    MriObjects,
    /// A presence subscription: exactly `{"trouterUri", "shouldPurgePreviousSubscriptions", "subscriptionsToAdd",
    /// "subscriptionsToRemove"}`, the URI the account's own Trouter endpoint's presence path (a push URL as the
    /// registration takes one, then `unifiedPresenceService`), the flag a boolean, each list of `{"mri": …, "source":
    /// "ups"}` objects, both keys exactly so; the names it gives are the additions, then the removals.
    Subscription,
}

impl Body {
    /// The list naming `names`, in that order, in the schema's form of an item (a subscription's: one list of it).
    fn of(self, names: &[Mri]) -> Value {
        let item = |mri: &Mri| match self {
            Self::Mris => Value::from(mri.as_str()),
            Self::MriObjects => serde_json::json!({ "mri": mri.as_str() }),
            Self::Subscription => serde_json::json!({ "mri": mri.as_str(), "source": UPS_SOURCE }),
        };
        Value::Array(names.iter().map(item).collect())
    }

    /// The MRIs `body` names when it is of this schema and nothing else (one JSON value, nothing after it); `None`
    /// otherwise, or when it names nobody.
    fn read(self, cloud: &crate::Cloud, body: &[u8]) -> Option<Vec<Mri>> {
        let names: Vec<String> = match self {
            Self::Mris => serde_json::from_slice(body).ok()?,
            Self::MriObjects => serde_json::from_slice::<Vec<MriObject>>(body)
                .ok()?
                .into_iter()
                .map(|object| object.mri)
                .collect(),
            Self::Subscription => serde_json::from_slice::<SubscriptionBody>(body).ok()?.names(cloud)?,
        };
        (!names.is_empty()).then(|| names.into_iter().map(Mri::new).collect())
    }
}

/// A [`Body::Subscription`], read strictly: these four keys and no other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SubscriptionBody {
    trouter_uri: String,
    /// Read for its type only: either value is the captured shape.
    #[serde(rename = "shouldPurgePreviousSubscriptions")]
    _purge: bool,
    subscriptions_to_add: Vec<SourcedMri>,
    subscriptions_to_remove: Vec<SourcedMri>,
}

impl SubscriptionBody {
    /// The MRIs it names, the additions first, when its URI is the account's Trouter endpoint's presence path and
    /// every item's `source` UPS's.
    fn names(self, cloud: &crate::Cloud) -> Option<Vec<String>> {
        let push_url = self.trouter_uri.strip_suffix(PRESENCE_PUSH_PATH)?;
        let items = self
            .subscriptions_to_add
            .into_iter()
            .chain(self.subscriptions_to_remove);
        let names: Option<Vec<String>> = items
            .map(|item| (item.source == UPS_SOURCE).then_some(item.mri))
            .collect();
        is_push_url_in(cloud, push_url).then_some(names).flatten()
    }
}

/// One item of a subscription's lists, read strictly: `mri` and `source`, nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcedMri {
    mri: String,
    source: String,
}

/// One item of a [`Body::MriObjects`] list, read strictly: `mri` and no other key.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MriObject {
    mri: String,
}

/// A read by `POST` as the transport sends it: its endpoint (the path and the query), the headers the entry adds to
/// its service's (none for a reviewed entry), and its JSON body. Only this crate makes or reads one.
#[derive(Clone, Debug)]
pub struct PostReadWire {
    endpoint: Endpoint,
    headers: Vec<(String, String)>,
    body: Value,
}

impl PostReadWire {
    /// `shape`'s endpoint, its query in the captured order, with `names` as the body.
    fn lookup(shape: &Shape, names: &[Mri]) -> Self {
        let endpoint = shape
            .query
            .iter()
            .fold(Endpoint::new(shape.path.iter().copied()), |endpoint, (name, value)| {
                endpoint.with_query(*name, *value)
            });
        Self {
            endpoint,
            headers: Vec::new(),
            body: shape.body.of(names),
        }
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub(crate) fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    pub(crate) fn body(&self) -> &Value {
        &self.body
    }
}

impl Shape {
    pub(crate) fn service(&self) -> Service {
        self.service
    }

    /// The MRIs a request names, when the request is exactly this shape: `path` is the shape's (then, when the shape
    /// says so, one lowercase GUID, the account's endpoint), `query` its pairs (each once, nothing else, in any order),
    /// `headers` (by lower-cased name, the token left out) its headers (each once, exactly their values, all of them),
    /// and `body` of the shape's schema, naming someone, and nothing else. `None` for anything else: the request is
    /// unknown.
    pub(crate) fn read_back(
        &self,
        cloud: &crate::Cloud,
        path: &[String],
        query: &[(String, String)],
        headers: &[(String, String)],
        body: &[u8],
    ) -> Option<Vec<Mri>> {
        let (path, endpoint) = if self.endpoint {
            let (endpoint, rest) = path.split_last()?;
            if !is_guid(endpoint) {
                return None;
            }
            (rest, Some(endpoint.as_str()))
        } else {
            (path, None)
        };
        let fixed = self.without_ids(headers, endpoint)?;
        let same_path = path.iter().map(String::as_str).eq(self.path.iter().copied());
        (same_path && same_pairs(query, self.query) && same_pairs(&fixed, self.headers))
            .then(|| self.body.read(cloud, body))
            .flatten()
    }

    /// `headers` but the shape's id headers, when each of those is there once with what it carries (a lowercase GUID,
    /// `endpoint` or the client build); `None` otherwise.
    fn without_ids(&self, headers: &[(String, String)], endpoint: Option<&str>) -> Option<Vec<(String, String)>> {
        for (name, id) in self.ids {
            let mut values = headers.iter().filter(|(sent, _)| sent == name).map(|(_, value)| value);
            let (Some(value), None) = (values.next(), values.next()) else {
                return None;
            };
            let carries = match id {
                IdHeader::Fresh => is_guid(value),
                IdHeader::Endpoint => endpoint == Some(value.as_str()),
                IdHeader::Build => value == CLIENT_BUILD,
            };
            if !carries {
                return None;
            }
        }
        Some(
            headers
                .iter()
                .filter(|(sent, _)| !self.ids.iter().any(|(name, _)| sent == name))
                .cloned()
                .collect(),
        )
    }
}

/// Whether `pairs` are `wanted`, each once and nothing else, in any order.
fn same_pairs(pairs: &[(String, String)], wanted: &[(&str, &str)]) -> bool {
    let mut pairs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let mut wanted = wanted.to_vec();
    pairs.sort_unstable();
    wanted.sort_unstable();
    pairs == wanted
}

/// The query every captured profile lookup sent (ab-chat-full, ab-chat and load captures of 2026-10-20), in the
/// order it was sent. `isMailAddress=true` (a lookup by address) isn't on it.
const LOOKUP_QUERY: &[(&str, &str)] = &[
    ("isMailAddress", "false"),
    ("enableGuest", "true"),
    ("skypeTeamsInfo", "true"),
    ("canBeSmtpAddress", "false"),
    ("includeIBBarredUsers", "true"),
    ("includeDisabledAccounts", "true"),
];

/// The headers a profile lookup goes out with besides its token: the captured values of those that could change what
/// it does or name someone, each pinned to the one value every captured lookup carried (5869 lookups in the captures
/// of 2026-10-20), and `Accept` (the transport asks every service for JSON). `x-ms-object-id` is sent empty, as it always was: it would name an account by its object id.
/// Nothing that names the client's components or the session (`x-ms-client-caller`, `x-ms-request-id`,
/// `x-ms-session-id`) and no cookie.
const LOOKUP_HEADERS: &[(&str, &str)] = &[
    ("accept", "application/json"),
    ("content-type", "application/json;charset=UTF-8"),
    ("x-ms-client-request-type", "0"),
    ("x-ms-client-type", "cdlworker"),
    ("x-ms-migration", "True"),
    ("x-ms-object-id", ""),
    ("x-ms-test-user", "False"),
];

/// `users/fetchShortProfile` (see [`ShortProfileLookup`]).
static SHORT_PROFILE: Shape = Shape {
    service: Service::MiddleTier,
    path: &["beta", "users", "fetchShortProfile"],
    endpoint: false,
    query: LOOKUP_QUERY,
    headers: LOOKUP_HEADERS,
    ids: &[],
    body: Body::Mris,
};

/// The presence read's path under the presence base (the transport adds the trailing slash).
const PRESENCE_PATH: &[&str] = &["v1", "presence", "getpresence"];

/// The headers a presence read goes out with besides its token: `Accept` (the transport asks every service for
/// JSON), the body's `Content-Type`, and the values every captured presence read carried for those that could change
/// what it does or name someone (5461 reads in the captures of 2026-10-19 to 2026-10-21). `x-ms-object-id` is sent
/// empty, as it always was. No `x-ms-endpoint-id` (it would name the client's endpoint), nothing that names the
/// client's components or the session, and no cookie.
const PRESENCE_HEADERS: &[(&str, &str)] = &[
    ("accept", "application/json"),
    ("content-type", "application/json"),
    ("behavioroverride", "redirectAs404"),
    ("x-ms-client-type", "cdlworker"),
    ("x-ms-client-user-agent", "Teams-V2-Web"),
    ("x-ms-object-id", ""),
];

/// `v1/presence/getpresence/` (see [`PresenceRead`]).
static PRESENCE: Shape = Shape {
    service: Service::Presence,
    path: PRESENCE_PATH,
    endpoint: false,
    query: &[],
    headers: PRESENCE_HEADERS,
    ids: &[],
    body: Body::MriObjects,
};

/// `POST <middle tier>/beta/users/fetchShortProfile?isMailAddress=false&enableGuest=true&skypeTeamsInfo=true&
/// canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true` with a JSON list of MRIs: the short
/// profiles (names, addresses, object ids) of exactly those accounts. Reviewed from the web client's lab-account
/// lookups (ab-chat-full of 2026-10-20: each answer named only the accounts asked
/// for). Its answer is read with [`ShortProfileLookup::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortProfileLookup {
    names: Vec<Mri>,
}

impl ShortProfileLookup {
    /// The lookup of `names`, in that order; `None` when it names nobody.
    #[must_use]
    pub fn new(names: impl IntoIterator<Item = Mri>) -> Option<Self> {
        let names: Vec<Mri> = names.into_iter().collect();
        (!names.is_empty()).then_some(Self { names })
    }

    /// The profiles the answer `body` gives, each of an MRI this lookup asked for.
    ///
    /// # Errors
    /// [`AnswerError::Malformed`] unless the body is `{"type": "…", "value": [profile, …]}` (both keys, as every
    /// captured answer has them) with an `mri` in each profile; [`AnswerError::Unasked`] when any profile is of an MRI the lookup didn't ask for (the answer is
    /// dropped whole: an entry's answer names only the accounts asked for).
    pub fn read_answer(&self, body: &[u8]) -> Result<Vec<ShortProfile>, AnswerError> {
        let answer: Answer = serde_json::from_slice(body).map_err(|_| AnswerError::Malformed)?;
        let unasked = answer
            .value
            .iter()
            .filter(|profile| !self.names.contains(&profile.mri))
            .count();
        if unasked > 0 {
            return Err(AnswerError::Unasked { count: unasked });
        }
        Ok(answer.value.into_iter().map(ShortProfile::from).collect())
    }
}

impl Request for ShortProfileLookup {
    fn service(&self) -> Service {
        SHORT_PROFILE.service
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        PostReadWire::lookup(&SHORT_PROFILE, &self.names).endpoint
    }

    fn body(&self) -> Option<Value> {
        Some(SHORT_PROFILE.body.of(&self.names))
    }
}

impl PostRead for ShortProfileLookup {
    const KIND: &'static str = "short_profile";

    fn names(&self) -> &[Mri] {
        &self.names
    }
}

/// The middle tier's answer to a profile lookup, as captured: `{"type": "…IUserIdentity", "value": [profile, …]}`.
#[derive(Deserialize)]
struct Answer {
    /// Required, as every captured answer carries it; what it names isn't read.
    #[serde(rename = "type")]
    _kind: String,
    value: Vec<RawProfile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProfile {
    #[serde(deserialize_with = "mri")]
    mri: Mri,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    given_name: Option<String>,
    #[serde(default)]
    surname: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    user_principal_name: Option<String>,
    #[serde(default)]
    object_id: Option<String>,
    #[serde(default)]
    user_type: Option<String>,
    #[serde(default)]
    job_title: Option<String>,
    #[serde(default)]
    office_location: Option<String>,
}

fn mri<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Mri, D::Error> {
    String::deserialize(deserializer).map(Mri::new)
}

/// One account's short profile as the middle tier gave it. A field the answer left out is `None` (the captures show
/// `givenName` and `surname` left out for an account without them).
#[derive(Clone, PartialEq, Eq)]
pub struct ShortProfile {
    pub mri: Mri,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub email: Option<String>,
    pub user_principal_name: Option<String>,
    pub object_id: Option<String>,
    /// `Member`, or another directory user type as given.
    pub user_type: Option<String>,
    /// Their job title (Manage team's Title).
    pub job_title: Option<String>,
    /// Their office (Manage team's Location).
    pub office_location: Option<String>,
}

impl From<RawProfile> for ShortProfile {
    fn from(raw: RawProfile) -> Self {
        Self {
            mri: raw.mri,
            display_name: raw.display_name,
            given_name: raw.given_name,
            surname: raw.surname,
            email: raw.email,
            user_principal_name: raw.user_principal_name,
            object_id: raw.object_id,
            user_type: raw.user_type,
            job_title: raw.job_title,
            office_location: raw.office_location,
        }
    }
}

/// `Debug` names the profile by its MRI only: the rest is someone's name and addresses.
impl fmt::Debug for ShortProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShortProfile")
            .field("mri", &self.mri)
            .finish_non_exhaustive()
    }
}

/// Why a profile lookup's answer wasn't read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnswerError {
    /// Not the captured answer's shape.
    #[error("not a profile lookup's answer")]
    Malformed,
    /// Profiles of MRIs the lookup didn't ask for.
    #[error("{count} profile(s) of MRIs the lookup didn't ask for")]
    Unasked { count: usize },
}

/// `POST <presence base>/v1/presence/getpresence/` with a JSON list of `{"mri": …}`: the presence of exactly those
/// accounts. Reviewed from the web client's lab-account reads (ab-chat of
/// 2026-10-22, M2 Task 17: 14 reads of a, b and c, each answered 200 with the presence of the accounts asked for and
/// nobody else). Its answer is read with [`PresenceRead::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceRead {
    names: Vec<Mri>,
}

impl PresenceRead {
    /// The read of `names`' presence, in that order; `None` when it names nobody.
    #[must_use]
    pub fn new(names: impl IntoIterator<Item = Mri>) -> Option<Self> {
        let names: Vec<Mri> = names.into_iter().collect();
        (!names.is_empty()).then_some(Self { names })
    }

    /// The presence the answer `body` gives, each of an MRI this read asked for. An item whose `status` isn't the
    /// captured success (`20000`), or that has no `presence`, gives no availability or activity: never guessed.
    ///
    /// # Errors
    /// [`AnswerError::Malformed`] unless the body is a JSON list of objects, each with an `mri`, as every captured
    /// answer was; [`AnswerError::Unasked`] when any item is of an MRI the read didn't ask for (the answer is dropped
    /// whole: an entry's answer names only the accounts asked for).
    pub fn read_answer(&self, body: &[u8]) -> Result<Vec<UserPresence>, AnswerError> {
        let answer: Vec<RawUserPresence> = serde_json::from_slice(body).map_err(|_| AnswerError::Malformed)?;
        let unasked = answer.iter().filter(|item| !self.names.contains(&item.mri)).count();
        if unasked > 0 {
            return Err(AnswerError::Unasked { count: unasked });
        }
        Ok(answer.into_iter().map(UserPresence::from).collect())
    }
}

impl Request for PresenceRead {
    fn service(&self) -> Service {
        PRESENCE.service
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        PostReadWire::lookup(&PRESENCE, &self.names).endpoint
    }

    fn body(&self) -> Option<Value> {
        Some(PRESENCE.body.of(&self.names))
    }
}

impl PostRead for PresenceRead {
    const KIND: &'static str = "presence";

    fn names(&self) -> &[Mri] {
        &self.names
    }
}

/// The per-item `status` every captured answer's items carried: the account's presence was read.
const PRESENCE_READ: i64 = 20_000;

/// One item of UPS's answer, as captured: `{"mri", "presence": {"availability", "activity", "lastActiveTime",
/// "calendarData": {"isOutOfOffice"}, …}, "status", …}` (also `etag`, `etagMatch`, and `presence`'s `sourceNetwork`
/// and `capabilities`, which aren't read).
#[derive(Deserialize)]
struct RawUserPresence {
    #[serde(deserialize_with = "mri")]
    mri: Mri,
    #[serde(default)]
    presence: Option<RawPresence>,
    #[serde(default)]
    status: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPresence {
    #[serde(default)]
    availability: Option<String>,
    #[serde(default)]
    activity: Option<String>,
    /// Read leniently: a value of another form is none.
    #[serde(default)]
    last_active_time: Option<Value>,
    #[serde(default)]
    calendar_data: Option<Value>,
}

/// One account's presence as UPS gave it, in a read's answer or a push: its availability (`Offline`, `Available`, …)
/// and activity, each a word as given, when they were last active and whether their calendar says they are out of
/// office; each `None` when the answer gave none for it (or one of another form), or gave its item a status other
/// than the captured success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserPresence {
    pub mri: Mri,
    pub availability: Option<String>,
    pub activity: Option<String>,
    /// `lastActiveTime`, ms since the epoch; `None` for `0001-01-01T00:00:00Z` (never: Teams shows "Offline", not
    /// "Last seen").
    pub last_active_ms: Option<i64>,
    /// `calendarData.isOutOfOffice`.
    pub out_of_office: Option<bool>,
}

impl UserPresence {
    /// `mri`'s presence as a push gives it: `presence` is the item's `presence` object (`{"availability",
    /// "activity", "lastActiveTime", "calendarData": {"isOutOfOffice"}, …}`); a field of another form than the
    /// captured one is none.
    #[must_use]
    pub fn of_pushed(mri: Mri, presence: &Value) -> Self {
        let word = |key: &str| presence.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            mri,
            availability: word("availability"),
            activity: word("activity"),
            last_active_ms: last_active(presence.get("lastActiveTime")),
            out_of_office: out_of_office(presence.get("calendarData")),
        }
    }
}

/// A `lastActiveTime` as ms since the epoch (`0001-01-01…`: never).
fn last_active(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_str).and_then(crate::loki::utc_ms)
}

/// A `calendarData`'s `isOutOfOffice`.
fn out_of_office(calendar: Option<&Value>) -> Option<bool> {
    calendar?.get("isOutOfOffice")?.as_bool()
}

impl From<RawUserPresence> for UserPresence {
    fn from(raw: RawUserPresence) -> Self {
        let presence = raw.presence.filter(|_| raw.status == Some(PRESENCE_READ));
        let Some(presence) = presence else {
            return Self {
                mri: raw.mri,
                availability: None,
                activity: None,
                last_active_ms: None,
                out_of_office: None,
            };
        };
        Self {
            mri: raw.mri,
            availability: presence.availability,
            activity: presence.activity,
            last_active_ms: last_active(presence.last_active_time.as_ref()),
            out_of_office: out_of_office(presence.calendar_data.as_ref()),
        }
    }
}

/// The path under an endpoint's push URL that UPS delivers presence changes at: the subscription's `trouterUri` is the
/// push URL with it.
pub const PRESENCE_PUSH_PATH: &str = "unifiedPresenceService";

/// The `source` every captured subscription item named.
const UPS_SOURCE: &str = "ups";

/// The status UPS gave every subscription it took in the captures (its web client's "success").
const SUBSCRIBED: i64 = 20_200;

/// `v1/pubsub/subscriptions/<endpoint>` (see [`PresenceSubscription`]): the presence read's headers, with
/// `x-ms-correlation-id` (a fresh GUID each time), `x-ms-client-version` (the client build the registration names) and
/// `x-ms-endpoint-id` (the path's endpoint again), as every captured subscription carried them. UPS answers a
/// subscription without the correlation id or the client version 400 ("Missing mandatory header"). No caller or session
/// id and no cookie. No trailing slash, as no captured one had.
static PRESENCE_SUBSCRIPTION: Shape = Shape {
    service: Service::Presence,
    path: &["v1", "pubsub", "subscriptions"],
    endpoint: true,
    query: &[],
    headers: PRESENCE_HEADERS,
    ids: &[
        (CORRELATION_ID, IdHeader::Fresh),
        (CLIENT_VERSION, IdHeader::Build),
        (ENDPOINT_ID, IdHeader::Endpoint),
    ],
    body: Body::Subscription,
};

/// The header a subscription's correlation id goes in.
const CORRELATION_ID: &str = "x-ms-correlation-id";

/// The header that names the subscribing endpoint again.
const ENDPOINT_ID: &str = "x-ms-endpoint-id";

/// The header that names the client's build.
const CLIENT_VERSION: &str = "x-ms-client-version";

/// `POST <presence base>/v1/pubsub/subscriptions/<endpoint>` with `{"trouterUri": "<push URL>unifiedPresenceService",
/// "shouldPurgePreviousSubscriptions": …, "subscriptionsToAdd": [{"mri": …, "source": "ups"}, …],
/// "subscriptionsToRemove": […]}`: the account's own Trouter endpoint (the registration's `epid`) subscribed to the
/// presence of the MRIs it adds, and no longer to those it removes; with the purge, its earlier subscriptions dropped
/// first. UPS then delivers each change of theirs (a snapshot first) to the socket at that push URL. Reviewed from the
/// web client's 755 subscriptions in the captures of 2026-10-20 to 2026-10-26 (each answered with a status per MRI
/// asked and no other). It names the MRIs it adds or removes ([`PostRead::names`]: the additions, then the removals),
/// never a bot's or a service's `28:` id nor a `48:` stream's. Its answer is read with
/// [`PresenceSubscription::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceSubscription {
    endpoint: String,
    push_url: String,
    purge: bool,
    /// The additions, then the removals.
    names: Vec<Mri>,
    added: usize,
}

impl PresenceSubscription {
    /// The subscription of `endpoint` (a registration of this run) at `push_url` (its socket's push URL, as the
    /// registration takes one: `https://<name>.trouter.teams.microsoft.com[:port]/v4/f/<id>/`), adding `add` and
    /// removing `remove`, its earlier subscriptions dropped first when `purge`. `None` when the push URL isn't one, when
    /// it names nobody or someone twice, or names a `28:` or `48:` id (no presence to subscribe to).
    #[must_use]
    pub fn new(
        endpoint: &RegisteredEndpoint,
        push_url: &str,
        purge: bool,
        add: impl IntoIterator<Item = Mri>,
        remove: impl IntoIterator<Item = Mri>,
    ) -> Option<Self> {
        Self::new_in(&crate::Cloud::Microsoft, endpoint, push_url, purge, add, remove)
    }

    /// A subscription to this account's cloud; off-origin push URLs are refused.
    #[must_use]
    pub fn new_in(
        cloud: &crate::Cloud,
        endpoint: &RegisteredEndpoint,
        push_url: &str,
        purge: bool,
        add: impl IntoIterator<Item = Mri>,
        remove: impl IntoIterator<Item = Mri>,
    ) -> Option<Self> {
        let mut names: Vec<Mri> = add.into_iter().collect();
        let added = names.len();
        names.extend(remove);
        let unique = names.iter().collect::<std::collections::BTreeSet<_>>().len() == names.len();
        let has_presence = names
            .iter()
            .all(|mri| !mri.as_str().starts_with("28:") && !mri.as_str().starts_with("48:"));
        let shaped = is_guid(endpoint.id()) && is_push_url_in(cloud, push_url);
        (!names.is_empty() && unique && has_presence && shaped).then(|| Self {
            endpoint: endpoint.id().to_owned(),
            push_url: push_url.to_owned(),
            purge,
            names,
            added,
        })
    }

    /// The MRIs it subscribes to.
    #[must_use]
    pub fn added(&self) -> &[Mri] {
        &self.names[..self.added]
    }

    /// The MRIs it no longer subscribes to.
    #[must_use]
    pub fn removed(&self) -> &[Mri] {
        &self.names[self.added..]
    }

    /// What UPS made of each MRI asked, from the answer `body`: one [`SubscriptionStatus`] per item, `taken` only for
    /// the captured success (`20200`); any other status (the web client's polling only, too many subscribers or
    /// subscriptions, a failed snapshot, an error, a status of a form no capture showed) is not taken, and someone
    /// asked about whom the answer says nothing isn't either.
    ///
    /// # Errors
    /// [`AnswerError::Malformed`] unless the body is `{"addStatuses": [...], "removeStatuses": [...]}` (both keys, as
    /// every captured answer has them) with an `mri` in each item; [`AnswerError::Unasked`] when an item is of an MRI
    /// the subscription didn't add (or, among the removals, remove).
    pub fn read_answer(&self, body: &[u8]) -> Result<Vec<SubscriptionStatus>, AnswerError> {
        let answer: RawSubscriptionAnswer = serde_json::from_slice(body).map_err(|_| AnswerError::Malformed)?;
        let unasked = answer
            .add_statuses
            .iter()
            .filter(|item| !self.added().contains(&item.mri))
            .chain(
                answer
                    .remove_statuses
                    .iter()
                    .filter(|item| !self.removed().contains(&item.mri)),
            )
            .count();
        if unasked > 0 {
            return Err(AnswerError::Unasked { count: unasked });
        }
        let status = |removed: bool| {
            move |item: RawSubscriptionStatus| SubscriptionStatus {
                taken: item.status.as_ref().and_then(Value::as_i64) == Some(SUBSCRIBED),
                mri: item.mri,
                removed,
            }
        };
        Ok(answer
            .add_statuses
            .into_iter()
            .map(status(false))
            .chain(answer.remove_statuses.into_iter().map(status(true)))
            .collect())
    }

    fn wire_endpoint(&self) -> Endpoint {
        Endpoint::new(
            PRESENCE_SUBSCRIPTION
                .path
                .iter()
                .copied()
                .chain([self.endpoint.as_str()]),
        )
    }

    /// Its id headers: a fresh correlation id, the client build and its endpoint.
    fn wire_headers(&self) -> Vec<(String, String)> {
        vec![
            (CORRELATION_ID.to_owned(), crate::search::fresh_id()),
            (CLIENT_VERSION.to_owned(), CLIENT_BUILD.to_owned()),
            (ENDPOINT_ID.to_owned(), self.endpoint.clone()),
        ]
    }

    fn wire_body(&self) -> Value {
        serde_json::json!({
            "trouterUri": format!("{}{PRESENCE_PUSH_PATH}", self.push_url),
            "shouldPurgePreviousSubscriptions": self.purge,
            "subscriptionsToAdd": Body::Subscription.of(self.added()),
            "subscriptionsToRemove": Body::Subscription.of(self.removed()),
        })
    }
}

impl Request for PresenceSubscription {
    fn service(&self) -> Service {
        PRESENCE_SUBSCRIPTION.service
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        self.wire_endpoint()
    }

    fn body(&self) -> Option<Value> {
        Some(self.wire_body())
    }

    fn headers(&self) -> Vec<(String, String)> {
        self.wire_headers()
    }
}

impl PostRead for PresenceSubscription {
    const KIND: &'static str = "presence_subscription";

    fn names(&self) -> &[Mri] {
        &self.names
    }
}

/// UPS's answer to a subscription, as captured: `{"addStatuses": [{"mri", "source", "status"}, …],
/// "removeStatuses": […]}`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSubscriptionAnswer {
    add_statuses: Vec<RawSubscriptionStatus>,
    remove_statuses: Vec<RawSubscriptionStatus>,
}

#[derive(Deserialize)]
struct RawSubscriptionStatus {
    #[serde(deserialize_with = "mri")]
    mri: Mri,
    /// A number in every captured answer; another form is no success.
    #[serde(default)]
    status: Option<Value>,
}

/// What UPS made of one MRI a subscription named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionStatus {
    pub mri: Mri,
    /// Named among the removals.
    pub removed: bool,
    /// UPS took it: its changes now come to the endpoint (an addition), or no longer (a removal).
    pub taken: bool,
}

/// A profile lookup or a presence read with something added that its entry doesn't name (a query pair, a header,
/// another body): the transport's tests need one to show it sends no request off its entry's captured shape. Tests
/// only (`test-support`), which ngteams refuses to run with.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug)]
pub struct ProbePostRead {
    shape: &'static Shape,
    names: Vec<Mri>,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

#[cfg(feature = "test-support")]
impl ProbePostRead {
    /// `lookup` as it is, until something is added.
    #[must_use]
    pub fn new(lookup: ShortProfileLookup) -> Self {
        Self::of(&SHORT_PROFILE, lookup.names)
    }

    /// `read` as it is, until something is added.
    #[must_use]
    pub fn presence(read: PresenceRead) -> Self {
        Self::of(&PRESENCE, read.names)
    }

    fn of(shape: &'static Shape, names: Vec<Mri>) -> Self {
        Self {
            shape,
            names,
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// Adds `name=value` to the query.
    #[must_use]
    pub fn with_query(mut self, name: &str, value: &str) -> Self {
        self.query.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Adds a header (its name lower-cased, as the transport sends it).
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    /// Sends `body` instead of the list of MRIs.
    #[must_use]
    pub fn with_body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
}

#[cfg(feature = "test-support")]
impl PostRead for ProbePostRead {
    const KIND: &'static str = "test_probe";

    fn names(&self) -> &[Mri] {
        &self.names
    }
}

#[cfg(test)]
mod tests {
    //! The endpoint, push URL and MRI here are synthetic.

    use super::*;

    const EPID: &str = "00000000-0000-4000-8000-0000000000e1";
    const PUSH_URL: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/";

    /// What the subscription shape reads back from a request to `EPID` with the presence read's headers and `ids`.
    fn read_back(ids: &[(&str, &str)]) -> Option<Vec<Mri>> {
        let path: Vec<String> = ["v1", "pubsub", "subscriptions", EPID].map(str::to_owned).to_vec();
        let headers: Vec<(String, String)> = PRESENCE_HEADERS
            .iter()
            .chain(ids)
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let body = serde_json::json!({
            "trouterUri": format!("{PUSH_URL}{PRESENCE_PUSH_PATH}"),
            "shouldPurgePreviousSubscriptions": true,
            "subscriptionsToAdd": [{"mri": "8:orgid:lab-b", "source": "ups"}],
            "subscriptionsToRemove": [],
        });
        PRESENCE_SUBSCRIPTION.read_back(
            &crate::Cloud::Microsoft,
            &path,
            &[],
            &headers,
            body.to_string().as_bytes(),
        )
    }

    #[test]
    fn a_subscription_goes_only_with_one_correlation_id_and_its_own_endpoint_again() {
        let fresh = crate::search::fresh_id();
        let fresh = fresh.as_str();
        assert_eq!(
            read_back(&[
                (CORRELATION_ID, fresh),
                (CLIENT_VERSION, CLIENT_BUILD),
                (ENDPOINT_ID, EPID)
            ]),
            Some(vec![Mri::new("8:orgid:lab-b")])
        );
        let other = "00000000-0000-4000-8000-0000000000e2";
        let build = (CLIENT_VERSION, CLIENT_BUILD);
        for ids in [
            vec![build, (ENDPOINT_ID, EPID)],
            vec![(CORRELATION_ID, fresh), build],
            vec![(CORRELATION_ID, fresh), (ENDPOINT_ID, EPID)],
            vec![(CORRELATION_ID, fresh), (CLIENT_VERSION, "1415/0"), (ENDPOINT_ID, EPID)],
            vec![(CORRELATION_ID, "not-a-guid"), build, (ENDPOINT_ID, EPID)],
            vec![
                (CORRELATION_ID, fresh),
                (CORRELATION_ID, other),
                build,
                (ENDPOINT_ID, EPID),
            ],
            vec![(CORRELATION_ID, fresh), build, (ENDPOINT_ID, other)],
        ] {
            assert_eq!(read_back(&ids), None, "{ids:?}");
        }
    }
}
