//! What the call writes are built from: the links the call's services hand out ([`CallLink`]), the call's own headers
//! ([`CallContext`]), who sends ([`CallParticipant`]), the endpoint's callbacks ([`Links`]), an offer or an answer
//! ([`MediaContent`]) and what the client says of itself ([`ClientInfo`]). Each turns into exactly the JSON the captures
//! show for it (`docs/protocol/calling-echo.md`, `docs/protocol/calling.md`).

use std::collections::BTreeMap;
use std::fmt;

use crate::{Cloud, Mri};
use percent_encoding::percent_decode_str;
use serde_json::{Map, Value, json};
use url::Url;

/// The flight proxy every call request goes through: the call start, and every link the call's services hand out.
pub const CALLING_HOST: &str = crate::CALL_PROXY_HOST;

/// The calling service's base (`Service::Calling`): every call request goes under it, the call start to `epconv`
/// and every other to a link the call's services handed out, `ep/<service host>/…`.
pub const CALLING_BASE: &str = "https://api.flightproxy.teams.microsoft.com/api/v2";

/// Where a call starts: [`CALLING_BASE`] plus `epconv`, no query.
pub use crate::ids::CALL_START_URL;

/// A link a call's services handed out (a conversation, its `leave`, the acceptance's `acknowledgement`, an
/// invitation's `attach`, …), kept exactly as it was given, its routing query (`?i=…&e=…`) included: the call registry
/// knows each by its exact text, and the query may be signed, so it is never decoded and built again.
///
/// Only a link under [`CALLING_BASE`]'s `ep/<host>/…` reads: `https`, [`CALLING_HOST`] with no port, no user info or
/// fragment, the path `/api/v2/ep/<host>/<at least one more segment>`, no segment empty, `.` or `..` once decoded.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CallLink(Url);

impl CallLink {
    /// `None` for anything but a link in the form above.
    #[must_use]
    pub fn parse(link: &str) -> Option<Self> {
        Self::parse_in(&Cloud::Microsoft, link)
    }

    /// Reads an exact operation link within the account's calling service.
    #[must_use]
    pub fn parse_in(cloud: &Cloud, link: &str) -> Option<Self> {
        let url = Url::parse(link).ok()?;
        let valid = if cloud.is_microsoft() {
            is_ep_link(&url)
        } else {
            cloud
                .project(&url, "/flightproxy", "https://api.flightproxy.teams.microsoft.com")
                .is_some_and(|url| is_ep_link(&url))
        };
        (url.as_str() == link && valid).then_some(Self(url))
    }

    /// The link as it was given.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub(crate) fn url(&self) -> &Url {
        &self.0
    }
}

/// `Debug` shows the link without its query: the routing query identifies the call's leg.
impl fmt::Debug for CallLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CallLink")
            .field(&format_args!(
                "{}{}",
                self.0.host_str().unwrap_or_default(),
                self.0.path()
            ))
            .finish()
    }
}

/// Whether `url` is on [`CALLING_BASE`]: `https`, [`CALLING_HOST`] with no port, no user info or fragment, and a path
/// that is `/api/v2/epconv` with no query (the call start) or `/api/v2/ep/<host>/<one or more segments>`, no segment
/// empty, `.` or `..` once decoded.
pub(crate) fn on_calling_base(url: &Url) -> bool {
    let start = url.path() == "/api/v2/epconv" && url.query().is_none();
    plain_proxy_url(url) && (start || is_ep_link(url))
}

/// `https`, [`CALLING_HOST`] with no port, no user info or fragment.
fn plain_proxy_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(CALLING_HOST)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

/// [`plain_proxy_url`] with the path `/api/v2/ep/<host>/<one or more segments>`, none empty, `.` or `..`.
fn is_ep_link(url: &Url) -> bool {
    let Some(segments) = url.path_segments() else {
        return false;
    };
    let decoded: Option<Vec<String>> = segments
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect();
    let Some(decoded) = decoded else {
        return false;
    };
    let sound = decoded
        .iter()
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    plain_proxy_url(url)
        && sound
        && decoded.len() >= 5
        && decoded[..3].iter().map(String::as_str).eq(["api", "v2", "ep"])
}

/// The headers every request of one call carries besides the credential, as the web client sends them
/// (`docs/protocol/calling-echo.md`, "The call, step by step"): the call's chain id (`X-Microsoft-Skype-Chain-ID`, one
/// GUID for the call), and the account's region and partition (`MS-Teams-Region`, `MS-Teams-Partition`, as authz
/// named them) when known. Each request adds its own message id (`X-Microsoft-Skype-Message-ID`, a fresh GUID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    cloud: Cloud,
    chain_id: String,
    region: Option<String>,
    partition: Option<String>,
}

impl CallContext {
    /// The context of a call whose chain id is `chain_id`.
    #[must_use]
    pub fn new(chain_id: impl Into<String>) -> Self {
        Self {
            cloud: Cloud::Microsoft,
            chain_id: chain_id.into(),
            region: None,
            partition: None,
        }
    }

    /// A context with a new chain id (a fresh GUID).
    ///
    /// # Errors
    /// When the system's random source fails.
    pub fn fresh() -> Result<Self, getrandom::Error> {
        Ok(Self::new(guid()?))
    }

    /// Binds every request and service link of this call to the account's cloud.
    #[must_use]
    pub fn for_cloud(mut self, cloud: Cloud) -> Self {
        self.cloud = cloud;
        self
    }

    #[must_use]
    pub const fn cloud(&self) -> &Cloud {
        &self.cloud
    }

    /// The same context naming the account's region and partition.
    #[must_use]
    pub fn with_region(mut self, region: Option<String>, partition: Option<String>) -> Self {
        self.region = region;
        self.partition = partition;
        self
    }

    #[must_use]
    pub fn chain_id(&self) -> &str {
        &self.chain_id
    }

    /// The headers of one request: these and a fresh message id.
    pub(super) fn headers(&self) -> Result<Vec<(&'static str, String)>, getrandom::Error> {
        let mut headers = vec![
            ("x-microsoft-skype-chain-id", self.chain_id.clone()),
            ("x-microsoft-skype-message-id", guid()?),
        ];
        if let Some(region) = &self.region {
            headers.push(("ms-teams-region", region.clone()));
        }
        if let Some(partition) = &self.partition {
            headers.push(("ms-teams-partition", partition.clone()));
        }
        Ok(headers)
    }
}

/// The hex digits, lowercase.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// A fresh random GUID, lowercase and hyphenated (a version 4 UUID).
pub(super) fn guid() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0x0f)]));
        hex
    });
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

/// Who sends, as a call request names its sender (`participants.from`, `acceptedBy`, `sender`): the account's MRI, the
/// endpoint it registered (`endpointId`), and what the client knows of itself in the call: its name, its leg's
/// participant id, its language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallParticipant {
    pub mri: Mri,
    pub display_name: Option<String>,
    pub endpoint_id: String,
    pub participant_id: Option<String>,
    pub language_id: Option<String>,
}

impl CallParticipant {
    /// The participant as the captures write it: `id`, then each of the others that is known.
    pub(super) fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("id".to_owned(), json!(self.mri.as_str()));
        if let Some(name) = &self.display_name {
            object.insert("displayName".to_owned(), json!(name));
        }
        object.insert("endpointId".to_owned(), json!(self.endpoint_id));
        if let Some(id) = &self.participant_id {
            object.insert("participantId".to_owned(), json!(id));
        }
        if let Some(language) = &self.language_id {
            object.insert("languageId".to_owned(), json!(language));
        }
        Value::Object(object)
    }

    /// The participant as an operation in the call's conversation names its own (`updateParticipantProperties`'s
    /// `from`): its `id`, `endpointId` and participant id only.
    pub(super) fn short_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("id".to_owned(), json!(self.mri.as_str()));
        object.insert("endpointId".to_owned(), json!(self.endpoint_id));
        if let Some(id) = &self.participant_id {
            object.insert("participantId".to_owned(), json!(id));
        }
        Value::Object(object)
    }
}

/// Someone a call request invites (`participants.to`): an MRI and, as the web client gives one, a fresh participant id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitee {
    pub mri: Mri,
    pub participant_id: Option<String>,
}

impl Invitee {
    pub(super) fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("id".to_owned(), json!(self.mri.as_str()));
        if let Some(id) = &self.participant_id {
            object.insert("participantId".to_owned(), json!(id));
        }
        Value::Object(object)
    }
}

/// The endpoint's callback links a request hands the call's services, by name (`acceptance`, `end`,
/// `conversationEnd`, `mediaRenegotiation`, …): each the endpoint's Trouter push URL, its call agent and a segment of
/// its own. Each typed request constructor validates the names it accepts, and the call tracker holds every link
/// to the endpoint's own call agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Links(BTreeMap<String, String>);

impl Links {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same links with `name` at `url`.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, url: impl Into<String>) -> Self {
        self.0.insert(name.into(), url.into());
        self
    }

    /// The link named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every link, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(name, url)| (name.as_str(), url.as_str()))
    }

    pub(super) fn json(&self) -> Value {
        Value::Object(self.0.iter().map(|(name, url)| (name.clone(), json!(url))).collect())
    }
}

/// One media line as a request describes it (`mediaDescriptions.descriptions`): its mid, its direction and its label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDescription {
    pub mid: String,
    pub direction: String,
    pub label: Option<String>,
}

impl MediaDescription {
    fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("mid".to_owned(), json!(self.mid));
        object.insert("direction".to_owned(), json!(self.direction));
        if let Some(label) = &self.label {
            object.insert("label".to_owned(), json!(label));
        }
        Value::Object(object)
    }
}

/// The media lines a request describes and the request's number (`mediaDescriptions`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDescriptions {
    pub descriptions: Vec<MediaDescription>,
    pub request_id: Option<u64>,
}

impl MediaDescriptions {
    pub(super) fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "descriptions".to_owned(),
            Value::Array(self.descriptions.iter().map(MediaDescription::json).collect()),
        );
        if let Some(id) = self.request_id {
            object.insert("requestId".to_owned(), json!(id));
        }
        Value::Object(object)
    }
}

/// A media line's channel parameters (`applyChannelParameters.multiChannelParameter`): the lines by mid (`*` for all)
/// and the parameter, a JSON text as the media controller takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelParameters {
    pub mids: Vec<String>,
    pub media_parameter: String,
}

impl ChannelParameters {
    pub(super) fn json(&self) -> Value {
        json!({"multiChannelParameter": {"mids": self.mids, "mediaParameter": self.media_parameter}})
    }
}

/// An offer or an answer as a call request carries it (`mediaContent`): the SDP in Teams' dialect and its content type
/// (`application/sdp-ngc-1.0`, `teams_media::sdp`), and what the captures send beside it. Its `Debug` leaves the
/// blob out: the SDP holds the call's ICE credentials and addresses.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaContent {
    /// Received service marker: a new mixer session (not an ordinary renegotiation). Never emitted in our answers.
    pub new_mixer_session: bool,
    pub blob: String,
    pub content_type: String,
    pub required_features: Option<String>,
    pub client_location: Option<String>,
    pub negotiation_tag: Option<String>,
    pub descriptions: Option<MediaDescriptions>,
    pub channel_parameters: Option<ChannelParameters>,
    pub media_leg_id: Option<String>,
}

impl fmt::Debug for MediaContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the blob holds ICE credentials and addresses: never in a log
        f.debug_struct("MediaContent")
            .field("new_mixer_session", &self.new_mixer_session)
            .field("blob", &format_args!("<{} bytes>", self.blob.len()))
            .field("content_type", &self.content_type)
            .field("required_features", &self.required_features)
            .field("client_location", &self.client_location)
            .field("negotiation_tag", &self.negotiation_tag)
            .field("descriptions", &self.descriptions)
            .field("channel_parameters", &self.channel_parameters)
            .field("media_leg_id", &self.media_leg_id)
            .finish()
    }
}

impl MediaContent {
    /// `blob` of `content_type`, nothing beside it.
    #[must_use]
    pub fn new(blob: impl Into<String>, content_type: impl Into<String>) -> Self {
        Self {
            new_mixer_session: false,
            blob: blob.into(),
            content_type: content_type.into(),
            required_features: None,
            client_location: None,
            negotiation_tag: None,
            descriptions: None,
            channel_parameters: None,
            media_leg_id: None,
        }
    }

    pub(super) fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("blob".to_owned(), json!(self.blob));
        object.insert("contentType".to_owned(), json!(self.content_type));
        let optional = [
            (
                "requiredFeatures",
                self.required_features.as_ref().map(|text| json!(text)),
            ),
            ("clientLocation", self.client_location.as_ref().map(|text| json!(text))),
            ("negotiationTag", self.negotiation_tag.as_ref().map(|text| json!(text))),
            (
                "mediaDescriptions",
                self.descriptions.as_ref().map(MediaDescriptions::json),
            ),
            (
                "applyChannelParameters",
                self.channel_parameters.as_ref().map(ChannelParameters::json),
            ),
            ("mediaLegId", self.media_leg_id.as_ref().map(|text| json!(text))),
        ];
        for (key, value) in optional {
            if let Some(value) = value {
                object.insert(key.to_owned(), value);
            }
        }
        Value::Object(object)
    }
}

/// What the client says of itself in a call request: its endpoint's capability numbers (`endpointCapabilities`,
/// `clientEndpointCapabilities`) and the platform name its `pstnContent` gives. Each is sent only when given: the
/// native client advertises no capability it hasn't got, and which numbers it may send is settled live.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientInfo {
    pub endpoint_capabilities: Option<u64>,
    pub client_endpoint_capabilities: Option<u64>,
    pub platform_name: Option<String>,
}

impl ClientInfo {
    /// Adds the capability numbers that are given to `object`, the client's as `clientEndpointCapabilities` only when
    /// `with_client` (an `attach` carries only the endpoint's).
    pub(super) fn add_capabilities(&self, object: &mut Map<String, Value>, with_client: bool) {
        object.insert("capabilities".to_owned(), Value::Null);
        if let Some(capabilities) = self.endpoint_capabilities {
            object.insert("endpointCapabilities".to_owned(), json!(capabilities));
        }
        if let (true, Some(capabilities)) = (with_client, self.client_endpoint_capabilities) {
            object.insert("clientEndpointCapabilities".to_owned(), json!(capabilities));
        }
    }

    /// `pstnContent` as the captures write it, when a platform name is given.
    pub(super) fn pstn_content(&self) -> Option<Value> {
        self.platform_name
            .as_ref()
            .map(|name| json!({"emergencyCallCountry": "", "platformName": name, "publicApiCall": false}))
    }
}
