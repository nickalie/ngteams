//! Own-state requests: requests that aren't reads but touch only the signed-in account's own state (registering its
//! own endpoint, say). They form a sealed list ([`OwnState`]: only this crate adds to it), sent through
//! [`HttpTransport::own_state`](crate::HttpTransport::own_state) with the transport's diagnostic wire logging.
//!
//! The entries:
//! - [`EndpointRegistration`]: the registrar registration of the account's own Trouter endpoint for the **messaging**
//!   stack, in the shape the web client's messaging stack sent (`docs/protocol/trouter.md`), so the account's chat
//!   events reach its push socket.
//! - [`CallingRegistration`]: the same registration of the calling stack's own endpoint (`SkypeSpacesWeb`, its own
//!   socket), which makes the account reachable for calls (`docs/protocol/calling-echo.md`); [`register_calling`]
//!   sends it and registers no [`RegisteredEndpoint`].
//!
//! [`register`] sends the registration (as a plain request) and, once the registrar took it, gives the endpoint it
//! registered as a [`RegisteredEndpoint`]: the only way to get one, and the only id the account's own availability may
//! name (`crate::SetEndpointAvailability`, `crate::ForceAvailability`). It takes only an [`EndpointRegistrar`], the
//! native HTTP transport (any transport under `test-support`), so a fake transport's made-up answer can't mint one,
//! and records the account it was registered for ([`Transport::own_account`]): a write
//! of another account's availability can't name it.

use std::fmt;

use crate::Mri;
use serde_json::{Value, json};
use url::Url;

use crate::client::{Response, Transport};
use crate::endpoint::{Endpoint, Method, Service};
use crate::request::Request;

/// The messaging stack's app on the registrar, exactly (`clientDescription.appId`).
pub const MESSAGING_APP_ID: &str = "TeamsCDLWebWorker";

/// The messaging stack's template (`clientDescription.templateKey`): the app and a version, as captured.
pub const MESSAGING_TEMPLATE: &str = "TeamsCDLWebWorker_2.6";

/// The calling stack's app on the registrar, exactly (`clientDescription.appId`).
pub const CALLING_APP_ID: &str = "SkypeSpacesWeb";

/// The calling stack's template (`clientDescription.templateKey`), as captured.
pub const CALLING_TEMPLATE: &str = "SkypeSpacesWeb_2.6";

/// The client build the registration and the Trouter connect name (`platformUIVersion`, and the connect's `tc` `v`):
/// the web client's in the captures of 2026-10-20.
pub const CLIENT_BUILD: &str = "1415/26091712226";

/// How long a registration lasts, in seconds (`transports.TROUTER[].ttl`), as every captured one asked.
pub const REGISTRATION_TTL: u32 = 3600;

/// The host suffix of every Trouter host (`go.`, `<name>-t.`, `<name>-f.`).
pub const TROUTER_DOMAIN: &str = ".trouter.teams.microsoft.com";

/// An entry of the transport's sealed own-state list (see the module docs). Sealed: no other crate can implement
/// it.
pub trait OwnState: sealed::Sealed {
    /// The entry's name for request validation and diagnostics.
    const KIND: &'static str;

    fn service(&self) -> Service;
    fn method(&self) -> Method;
    fn endpoint(&self) -> Endpoint;
    /// The JSON body, if it has one.
    fn body(&self) -> Option<Value>;
}

mod sealed {
    /// The seal on [`OwnState`](super::OwnState): a public trait no other crate can name. Its implementations are the
    /// reviewed list.
    pub trait Sealed {}

    impl Sealed for super::EndpointRegistration {}
    impl Sealed for super::CallingRegistration {}

    #[cfg(feature = "test-support")]
    impl Sealed for super::ProbeOwnState {}
}

/// `POST <AFD host>/registrar/prod/V2/registrations`: the registration of the account's own Trouter endpoint for the
/// messaging stack (see the module docs). The registrar takes the account from the request's token; the body names
/// only the endpoint (its `registrationId`, the socket's `epid`) and its push URL (the `surl` the endpoint's
/// `trouter.connected` handed over). Answered `202` with no body. Every other field is the captured value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointRegistration {
    registration_id: String,
    push_url: String,
}

impl EndpointRegistration {
    /// The registration of the endpoint `registration_id` (a GUID in lowercase hex with dashes, the socket's `epid`)
    /// at `push_url`; `None` unless `push_url` is a Trouter push URL as written: `https`, a host `<name>` +
    /// [`TROUTER_DOMAIN`] (letters, digits, `-`), an optional port, the path `/v4/f/<endpoint id>/` (base64url: letters,
    /// digits, `-`, `_`), no query, fragment or user info.
    #[must_use]
    pub fn messaging(registration_id: &str, push_url: &str) -> Option<Self> {
        Self::messaging_in(&crate::Cloud::Microsoft, registration_id, push_url)
    }

    /// The registration on the account's cloud; refuses off-origin push URLs.
    #[must_use]
    pub fn messaging_in(cloud: &crate::Cloud, registration_id: &str, push_url: &str) -> Option<Self> {
        (is_guid(registration_id) && is_push_url_in(cloud, push_url)).then(|| Self {
            registration_id: registration_id.to_owned(),
            push_url: push_url.to_owned(),
        })
    }

    #[must_use]
    pub fn registration_id(&self) -> &str {
        &self.registration_id
    }

    #[must_use]
    pub fn push_url(&self) -> &str {
        &self.push_url
    }
}

impl Request for EndpointRegistration {
    fn service(&self) -> Service {
        Service::Registrar
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["prod", "V2", "registrations"])
    }

    fn body(&self) -> Option<Value> {
        Some(registration_body(
            MESSAGING_APP_ID,
            MESSAGING_TEMPLATE,
            &self.registration_id,
            &self.push_url,
        ))
    }
}

/// A registration's body: every field the captured value but the app, its template, the endpoint and its push URL.
fn registration_body(app_id: &str, template: &str, registration_id: &str, push_url: &str) -> Value {
    json!({
        "clientDescription": {
            "appId": app_id,
            "aesKey": "",
            "languageId": "en-US",
            "platform": "chrome",
            "templateKey": template,
            "platformUIVersion": CLIENT_BUILD,
        },
        "registrationId": registration_id,
        "nodeId": "",
        "transports": {
            "TROUTER": [{"context": "", "path": push_url, "ttl": REGISTRATION_TTL}],
        },
    })
}

/// `POST <AFD host>/registrar/prod/V2/registrations`: the registration of the account's own Trouter endpoint for the
/// calling stack (`SkypeSpacesWeb`, see the module docs), the calling stack's own socket's endpoint: the messaging
/// registration's body with the calling stack's app and template, as the calling stack sent it on every
/// `trouter.connected` (a reconnect included, `docs/protocol/calling.md`). It makes the account reachable for calls,
/// without joining or answering a call; call actions remain the signaling machine's responsibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallingRegistration {
    registration_id: String,
    push_url: String,
}

impl CallingRegistration {
    /// The calling stack's registration of the endpoint `registration_id` at `push_url`; `None` unless both are as
    /// [`EndpointRegistration::messaging`] takes them.
    #[must_use]
    pub fn new(registration_id: &str, push_url: &str) -> Option<Self> {
        Self::new_in(&crate::Cloud::Microsoft, registration_id, push_url)
    }

    /// The registration on the account's cloud; refuses off-origin push URLs.
    #[must_use]
    pub fn new_in(cloud: &crate::Cloud, registration_id: &str, push_url: &str) -> Option<Self> {
        (is_guid(registration_id) && is_push_url_in(cloud, push_url)).then(|| Self {
            registration_id: registration_id.to_owned(),
            push_url: push_url.to_owned(),
        })
    }

    #[must_use]
    pub fn registration_id(&self) -> &str {
        &self.registration_id
    }

    #[must_use]
    pub fn push_url(&self) -> &str {
        &self.push_url
    }
}

impl Request for CallingRegistration {
    fn service(&self) -> Service {
        Service::Registrar
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["prod", "V2", "registrations"])
    }

    fn body(&self) -> Option<Value> {
        Some(registration_body(
            CALLING_APP_ID,
            CALLING_TEMPLATE,
            &self.registration_id,
            &self.push_url,
        ))
    }
}

impl OwnState for CallingRegistration {
    const KIND: &'static str = "calling_endpoint_registration";

    fn service(&self) -> Service {
        Request::service(self)
    }

    fn method(&self) -> Method {
        Request::method(self)
    }

    fn endpoint(&self) -> Endpoint {
        Request::endpoint(self)
    }

    fn body(&self) -> Option<Value> {
        Request::body(self)
    }
}

/// Sends the calling stack's `registration` through [`Transport::send_own`] and returns the answer. It
/// registers no [`RegisteredEndpoint`]: the account's own availability names its messaging endpoint only.
///
/// # Errors
/// The transport's.
pub async fn register_calling<T: Transport>(
    transport: &T,
    registration: &CallingRegistration,
) -> Result<Response, T::Error> {
    transport.send_own(registration).await
}

impl OwnState for EndpointRegistration {
    const KIND: &'static str = "endpoint_registration";

    fn service(&self) -> Service {
        Request::service(self)
    }

    fn method(&self) -> Method {
        Request::method(self)
    }

    fn endpoint(&self) -> Endpoint {
        Request::endpoint(self)
    }

    fn body(&self) -> Option<Value> {
        Request::body(self)
    }
}

/// The account's own Trouter endpoint, as a registration of this run registered it for the messaging stack: the
/// `registrationId` of an [`EndpointRegistration`] the account's transport sent and the registrar took (a 2xx), and
/// the account it was registered for. It is the endpoint id the account's own availability names. Only [`register`]
/// makes one, over the native HTTP transport ([`EndpointRegistrar`]): it has no
/// public constructor, isn't deserialized from anything, and lives only in memory, so it is always this run's.
#[derive(Clone, PartialEq, Eq)]
pub struct RegisteredEndpoint {
    id: String,
    /// The account the registration was for ([`Transport::own_account`]) when it went through; `None` when the
    /// transport has no account identity.
    account: Option<Mri>,
}

impl RegisteredEndpoint {
    /// The endpoint's id: a lowercase GUID, the socket's `epid`.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The account it was registered for; `None` when the transport has no account identity, and then no account's
    /// availability can name it.
    #[must_use]
    pub fn account(&self) -> Option<&Mri> {
        self.account.as_ref()
    }

    /// Whether it was registered for `account`: the only account whose availability may name it.
    #[must_use]
    pub fn is_for(&self, account: &Mri) -> bool {
        self.account.as_ref() == Some(account)
    }
}

impl fmt::Debug for RegisteredEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RegisteredEndpoint").field(&self.id).finish()
    }
}

/// A transport whose registration may give a [`RegisteredEndpoint`]: the native [`HttpTransport`](crate::HttpTransport)
/// only, which sends only to its services' bases with diagnostic wire logging. Sealed:
/// [`Transport`] is open (a test's fake implements it, and its answer is whatever the fake says), so a registration
/// "taken" by another transport proves nothing was registered, and no other crate can make a transport one. Under
/// `test-support` (tests only, which ngteams refuses to run with) every transport is one, so a test's fake can
/// register.
pub trait EndpointRegistrar: Transport + registrar::Sealed {}

impl<T: Transport + registrar::Sealed> EndpointRegistrar for T {}

mod registrar {
    /// The seal on [`EndpointRegistrar`](super::EndpointRegistrar): a public trait no other crate can name.
    pub trait Sealed {}

    #[cfg(not(feature = "test-support"))]
    impl<C: crate::Credentials> Sealed for crate::HttpTransport<C> {}

    #[cfg(feature = "test-support")]
    impl<T: crate::Transport> Sealed for T {}
}

/// Sends `registration` through [`Transport::send_own`] and returns the answer, with the endpoint it registered
/// when the answer is a success, recorded for the transport's own account ([`Transport::own_account`], whose token
/// the registrar took).
/// A refusal or a failure registers nothing. Only an [`EndpointRegistrar`] (the native HTTP transport) registers: a
/// fake transport answering a success can't make a [`RegisteredEndpoint`] outside `test-support`.
///
/// # Errors
/// The transport's.
pub async fn register<T: EndpointRegistrar>(
    transport: &T,
    registration: &EndpointRegistration,
) -> Result<(Response, Option<RegisteredEndpoint>), T::Error> {
    let answer = transport.send_own(registration).await?;
    let endpoint = answer.is_success().then(|| RegisteredEndpoint {
        id: registration.registration_id.clone(),
        account: transport.own_account().cloned(),
    });
    Ok((answer, endpoint))
}

/// A GUID as `8-4-4-4-12` lowercase hex digits.
pub(crate) fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// A Trouter push URL (see [`EndpointRegistration::messaging`]).
pub(crate) fn is_push_url(text: &str) -> bool {
    let Ok(url) = Url::parse(text) else {
        return false;
    };
    let name = url
        .host_str()
        .and_then(|host| host.strip_suffix(TROUTER_DOMAIN))
        .unwrap_or_default();
    let endpoint = url
        .path()
        .strip_prefix("/v4/f/")
        .and_then(|rest| rest.strip_suffix('/'))
        .unwrap_or_default();
    url.scheme() == "https"
        && !name.is_empty()
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !endpoint.is_empty()
        // the endpoint id is base64url: letters, digits, `-` and `_` (live ids carry both)
        && endpoint
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the URL as it was given, not one the parser normalized
        && url.as_str() == text
}

pub(crate) fn is_push_url_in(cloud: &crate::Cloud, text: &str) -> bool {
    if cloud.is_microsoft() {
        return is_push_url(text);
    }
    let Ok(url) = Url::parse(text) else { return false };
    url.as_str() == text
        && cloud
            .project(&url, "/trouter", "https://go.trouter.teams.microsoft.com")
            .is_some_and(|url| is_push_url(url.as_str()))
}

/// An own-state request of the test's choosing, `POST`ed: the transport's tests need one beyond the sealed list.
/// Tests only (`test-support`), which ngteams refuses to run with.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOwnState {
    service: Service,
    endpoint: Endpoint,
    body: Option<Value>,
}

#[cfg(feature = "test-support")]
impl ProbeOwnState {
    #[must_use]
    pub fn new(service: Service, endpoint: Endpoint, body: Option<Value>) -> Self {
        Self {
            service,
            endpoint,
            body,
        }
    }
}

#[cfg(feature = "test-support")]
impl OwnState for ProbeOwnState {
    const KIND: &'static str = "test_probe";

    fn service(&self) -> Service {
        self.service
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }

    fn body(&self) -> Option<Value> {
        self.body.clone()
    }
}
