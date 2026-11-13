//! The logged native transport: HTTPS-only service bases, per-service credentials and no write retries.

use std::fmt;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::{AFD_HOSTS, Mri, chat_service_path, path_shape};
use percent_encoding::percent_decode_str;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION, RANGE,
};
use serde_json::Value;
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::{Secret, SecretStore};
use teams_auth::session::Session;
use teams_auth::token::{SPACES_RESOURCE, TokenError, retry_after};
use url::Url;

use crate::app_picture::{APP_PICTURES_HOST, APP_PICTURES_PATH, STATIC_APP_PICTURES_HOST};
use crate::client::{Response, Transport};
use crate::endpoint::{Endpoint, Service};
use crate::file_content::{FileContentLink, MAX_RANGE, MAX_STREAM, StreamStart, is_content_link};
use crate::loki::{CONFIGURATION_PATH, LOKI_AUDIENCE, LOKI_GLOBAL_HOST, parse_regional_base};
use crate::own::OwnState;
use crate::post_read::PostRead;
use crate::read::Read;
use crate::request::Request;
use crate::search::{SUBSTRATE_AUDIENCE, SUBSTRATE_HOST, SubstrateSearch};
use crate::thumbnail::{ThumbnailLink, is_thumbnail_link};
use crate::write::on_calling_base;

mod captions;
mod drive;
mod identity;
mod workflows;

/// The Entra audience of the token the chat service and the registrar take.
pub const IC3_AUDIENCE: &str = "https://ic3.teams.office.com";

/// The Entra audience of the token the middle tier takes.
pub const MIDDLE_TIER_AUDIENCE: &str = SPACES_RESOURCE;

/// The Entra audience of the token Microsoft Graph takes.
pub const GRAPH_AUDIENCE: &str = "https://graph.microsoft.com";

/// The Entra audience of the token CSA takes.
pub const CSA_AUDIENCE: &str = "https://chatsvcagg.teams.microsoft.com";

/// The audience of the token for the teams' provisioning service ([`Service::Provisioning`]).
pub const PROVISIONING_AUDIENCE: &str = "https://templates.teams.microsoft.com";

/// The Entra audience of the token UPS (presence) takes: the presence origin with its slash, as the web client asks for
/// it (`docs/protocol/auth.md`, question 4).
pub const PRESENCE_AUDIENCE: &str = "https://presence.teams.microsoft.com/";

/// Microsoft Graph's host.
pub const GRAPH_HOST: &str = "graph.microsoft.com";

/// Trouter's HTTP host.
pub const TROUTER_HOST: &str = "go.trouter.teams.microsoft.com";

/// The largest body read, in bytes; a longer one is [`HttpError::BodyTooLarge`].
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// The largest body read from AMS ([`Service::Ams`]) or Giphy ([`Service::Giphy`]), in bytes: a picture pasted into a
/// message, a photo's original included, or a GIF at its full size; a longer one is [`HttpError::BodyTooLarge`].
pub const MAX_PICTURE_BODY: usize = 24 * 1024 * 1024;

/// The largest part of a meeting recording's presentation read ([`HttpTransport::media_segment`]), in bytes: a few
/// seconds of a recording at a high bit rate; a longer one is [`HttpError::BodyTooLarge`].
pub const MAX_SEGMENT_BODY: usize = 32 * 1024 * 1024;

/// How long connecting may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one request may take, end to end.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one streamed read of a file may take, end to end ([`HttpTransport::file_stream`]): its body comes at the
/// network's pace, which [`REQUEST_TIMEOUT`] would cut short; its reader gives up on a stalled one sooner.
pub const STREAM_TIMEOUT: Duration = Duration::from_mins(10);

/// The longest a throttled read, plain or own-state request waits before it is sent again, by default.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// The wait before a throttled request is sent again when the answer gives no `Retry-After` (capped like any other).
pub const DEFAULT_RETRY_WAIT: Duration = Duration::from_secs(1);

/// The account's tokens, as the transport asks for them: a bearer token for an audience, and a new one after a service
/// refused one. The account's [`Session`] is the one a client uses; any type may implement it (a fake in tests), since
/// a token decides nothing about where a request goes or what it writes.
pub trait Credentials: Send + Sync {
    /// A bearer token for `audience`.
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send;

    /// A new bearer token for `audience` after a service refused `rejected` (a 401).
    fn bearer_rejected(
        &self,
        audience: &str,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send;

    /// The account's skypetoken, which the calling service takes (`X-Skypetoken`). `Ok(None)` (the default) for a
    /// source that has none: the transport then refuses a call write ([`Refusal::NoSkypetoken`]) and sends nothing.
    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        async { Ok(None) }
    }

    /// A new skypetoken after the calling service refused `rejected` (a 401), for a later request; `Ok(None)` (the
    /// default) for a source that has none.
    fn skypetoken_rejected(
        &self,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        let _ = rejected;
        async { Ok(None) }
    }
}

impl<S: SecretStore> Credentials for Session<S> {
    async fn bearer(&self, audience: &str) -> Result<Secret, TokenError> {
        Ok(self.access_token(audience).await?.token)
    }

    async fn bearer_rejected(&self, audience: &str, rejected: &Secret) -> Result<Secret, TokenError> {
        Ok(self.refresh_rejected(audience, rejected).await?.token)
    }

    async fn skypetoken(&self) -> Result<Option<Secret>, TokenError> {
        Ok(Some(Session::skypetoken(self).await?.token))
    }

    async fn skypetoken_rejected(&self, rejected: &Secret) -> Result<Option<Secret>, TokenError> {
        Ok(Some(Session::skypetoken_rejected(self, rejected).await?.token))
    }
}

/// Why a request didn't produce an answer. No variant carries a token or a body.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// Refused before anything was sent.
    #[error("refused before sending: {0}")]
    Refused(Refusal),
    /// The write has no wire form the native transport has a route for (a call); nothing was sent.
    #[error("the native transport has no route for this write")]
    NoRoute,
    /// A read by `POST` whose request isn't exactly its entry's captured shape, or names other MRIs than the read;
    /// nothing was sent, logged or asked of the session.
    #[error("the read by POST isn't its entry's captured shape; nothing was sent")]
    Unknown,
    /// The session couldn't give a token; nothing (more) was sent.
    #[error("no token for the request: {0}")]
    Credentials(#[source] TokenError),
    /// The request or its answer failed on the way.
    #[error("the request failed: {0}")]
    Failed(Failure),
    /// The answer's body is longer than [`MAX_BODY`] (AMS's: [`MAX_PICTURE_BODY`]).
    #[error("the answer's body is over the largest read")]
    BodyTooLarge,
    /// A read, plain or own-state request answered with a status other than a success, after the retry the status
    /// allows.
    #[error("the service answered HTTP {}", .0.status())]
    Status(Box<Response>),
}

/// Why a request was refused before sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the attachment owner or expected direct recipients do not match")]
    Attachment,
    #[error("the operation does not belong to the current call")]
    CallState,
    /// An endpoint with an empty, `.` or `..` segment.
    #[error("an empty, `.` or `..` path segment")]
    Segment,
    /// A chat service endpoint that doesn't start with `v1`.
    #[error("a chat service endpoint that doesn't start with `v1`")]
    ChatPath,
    /// A middle-tier request on a transport that wasn't given the account's middle-tier base.
    #[error("no middle-tier base")]
    NoMiddleTier,
    /// A CSA request on a transport that wasn't given the account's CSA base.
    #[error("no CSA base")]
    NoCsa,
    /// A provisioning request on a transport that wasn't given the account's base for it.
    #[error("no provisioning base")]
    NoProvisioning,
    /// A user profile request on a transport that wasn't given the account's base for it.
    #[error("no user profile base")]
    NoUserProfile,
    /// A presence request on a transport that wasn't given the account's presence base.
    #[error("no presence base")]
    NoPresence,
    /// An AMS read on a transport that wasn't given the account's AMS base.
    #[error("no AMS base")]
    NoAms,
    /// An MCPS read on a transport that wasn't given the account's MCPS base.
    #[error("no MCPS base")]
    NoMcps,
    /// A Loki read on a transport that wasn't given Loki ([`HttpTransport::with_loki`]), or a regional one before the
    /// account's region was set ([`HttpTransport::set_loki_region`]).
    #[error("no Loki base")]
    NoLoki,
    /// A request to Loki that isn't a typed read, or a read on its global host of anything but its configuration.
    #[error("only typed reads go to Loki")]
    Loki,
    /// The URL built isn't on its service's base: another host, scheme or port, or a path or query other than the
    /// base's and the endpoint's exactly.
    #[error("the URL isn't on its service's base")]
    NotAllowed,
    /// A token that can't be sent as a header value.
    #[error("a token that can't be sent in a header")]
    Token,
    /// A request without a checked calling link: only typed call writes go to the calling service.
    #[error("only typed call writes go to the calling service")]
    Calling,
    /// A read or a plain request to a file thumbnail's host: only a link Graph handed out is read there
    /// ([`HttpTransport::file_thumbnail`]).
    #[error("only a thumbnail link Graph handed out is read from a thumbnail's host")]
    FileThumbnail,
    /// A read or a plain request to a file's download host, or a ranged read of nothing or of more than
    /// [`MAX_RANGE`]: only a download link Graph handed out is read there ([`HttpTransport::file_range`]).
    #[error("only a download link Graph handed out is read from a file's host, a part at a time")]
    FileContent,
    /// A read or a plain request to Substrate: only its search reads go there ([`HttpTransport::search`]).
    #[error("only the search's reads go to Substrate")]
    Substrate,
    /// A call write while the credentials give no skypetoken.
    #[error("no skypetoken for the calling service")]
    NoSkypetoken,
    /// A write, an own-state request or a mutating plain request on a read-only transport
    /// ([`HttpTransport::read_only`], the personal build's read-only mode): logged as `refused_read_only`, never sent.
    #[error("read-only mode: nothing that writes is sent")]
    ReadOnly,
}

/// How a request failed on the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    #[error("network")]
    Network,
    #[error("timed out")]
    Timeout,
    /// The answer's body couldn't be read.
    #[error("the body couldn't be read")]
    Body,
}

/// Why a transport couldn't be built.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Not a chat service base the transport accepts (see [`HttpTransport::new`]).
    #[error("not a chat service base the transport accepts")]
    ChatBase,
    /// Not a middle-tier base the transport accepts (see [`HttpTransport::with_middle_tier`]).
    #[error("not a middle-tier base the transport accepts")]
    MiddleTierBase,
    /// Not a CSA base the transport accepts (see [`HttpTransport::with_csa`]).
    #[error("not a CSA base the transport accepts")]
    CsaBase,
    /// Not a provisioning base the transport accepts (see [`HttpTransport::with_provisioning`]).
    #[error("not a provisioning base the transport accepts")]
    ProvisioningBase,
    /// Not a user profile base the transport accepts (see [`HttpTransport::with_user_profile`]).
    #[error("not a user profile base the transport accepts")]
    UserProfileBase,
    /// Not a presence base the transport accepts (see [`HttpTransport::with_presence`]).
    #[error("not a presence base the transport accepts")]
    PresenceBase,
    /// Not an AMS base the transport accepts (see [`HttpTransport::with_ams`]).
    #[error("not an AMS base the transport accepts")]
    AmsBase,
    /// Not an MCPS base the transport accepts (see [`HttpTransport::with_mcps`]).
    #[error("not an MCPS base the transport accepts")]
    McpsBase,
    /// Not a Loki regional base the transport accepts, another region than the one already set, or a transport not
    /// given Loki (see [`HttpTransport::set_loki_region`]).
    #[error("not a Loki regional base the transport accepts")]
    LokiBase,
    /// Not a loopback `http` origin with a port.
    #[error("not a loopback http origin")]
    MockOrigin,
    #[error("can't build the HTTPS client: {0}")]
    Client(#[source] reqwest::Error),
}

/// The native HTTP transport (see the module docs).
pub struct HttpTransport<C> {
    client: reqwest::Client,
    credentials: Arc<C>,
    account: Option<Mri>,
    /// The chat base: an AFD base (`https://<AFD host>/api/chatsvc/<region>`) without a trailing slash, a regional
    /// base as its origin (`https://<host>/`, the slash `Url` keeps for an empty path).
    chat_base: Url,
    /// The AFD host the registrar is reached on.
    registrar_host: &'static str,
    /// The middle-tier base (`https://<AFD host>/api/mt/part/<partition>`, no trailing slash), once given.
    middle_tier: Option<Url>,
    /// The CSA base (`https://<AFD host>/api/csa/<region>`, no trailing slash), once given.
    csa: Option<Url>,
    /// The provisioning service's base (`https://<AFD host>/fabric/<region>/templates/api`), once given.
    provisioning: Option<Url>,
    /// The user profile service's base (`https://<AFD host>/api/userprofilesvc/<region>`), once given.
    user_profile: Option<Url>,
    /// The presence base (`https://<AFD host>/ups/<geo>`, no trailing slash), once given.
    presence: Option<Url>,
    /// The AMS base (`https://<name>.asyncgw.teams.microsoft.com`, its path empty), once given.
    ams: Option<Url>,
    /// The MCPS base (`https://<AFD host>/api/mcps/<region>`, no trailing slash), once given.
    mcps: Option<Url>,
    /// Loki, once given ([`HttpTransport::with_loki`]): its regional base (`https://<region>.loki.delve.office.com/`),
    /// once the account's configuration named it ([`HttpTransport::set_loki_region`]).
    loki: Option<OnceLock<Url>>,
    max_retry_wait: Duration,
    mock_origin: Option<Url>,
    /// Read-only ([`HttpTransport::read_only`]): only reads (a `GET`, a read by `POST` of the sealed list, a search) go
    /// out.
    read_only: bool,
    call_state: Option<Arc<dyn crate::CallState>>,
}

impl<C> fmt::Debug for HttpTransport<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpTransport")
            .field("chat_base", &self.chat_base.as_str())
            .field("registrar_host", &self.registrar_host)
            .field("middle_tier", &self.middle_tier.as_ref().map(Url::as_str))
            .field("csa", &self.csa.as_ref().map(Url::as_str))
            .field("presence", &self.presence.as_ref().map(Url::as_str))
            .field("loki", &self.loki.as_ref().and_then(OnceLock::get).map(Url::as_str))
            .field("max_retry_wait", &self.max_retry_wait)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

/// A request's immutable wire fields, captured before its asynchronous send.
struct BuiltRequest {
    service: Service,
    method: crate::Method,
    endpoint: Endpoint,
    body: Option<Vec<u8>>,
    headers: Vec<(String, String)>,
    call: Option<Url>,
    routable: bool,
    write: bool,
}

impl BuiltRequest {
    fn of<R: Request>(request: &R) -> Self {
        Self {
            service: request.service(),
            method: request.method(),
            endpoint: request.endpoint(),
            body: request
                .raw_body()
                .map(|body| body.bytes().to_vec())
                .or_else(|| request.body().map(|body| body.to_string().into_bytes())),
            headers: request.headers(),
            call: request.call_url().cloned(),
            routable: request.routable(),
            write: request.is_write(),
        }
    }
    fn writing(mut self) -> Self {
        self.write = true;
        self
    }
}

impl<C: Credentials> HttpTransport<C> {
    async fn send_call(
        &self,
        method: reqwest::Method,
        url: &Url,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<Response, HttpError> {
        let token = self
            .credentials
            .skypetoken()
            .await
            .map_err(HttpError::Credentials)?
            .ok_or(HttpError::Refused(Refusal::NoSkypetoken))?;
        let decoded = body
            .as_deref()
            .filter(|body| !body.is_empty())
            .and_then(|body| serde_json::from_slice::<Value>(body).ok());
        if self
            .call_state
            .as_ref()
            .is_some_and(|state| !state.call_allowed(url, decoded.as_ref()))
        {
            return Err(HttpError::Refused(Refusal::CallState));
        }
        let answer = self
            .send_as(method, url, headers, Auth::Skypetoken(&token), body)
            .await?;
        if answer.status() == 401 {
            let _ = self.credentials.skypetoken_rejected(&token).await;
        }
        Ok(answer)
    }

    async fn send_built(&self, request: BuiltRequest) -> Result<Response, HttpError> {
        if !request.routable {
            return Err(HttpError::NoRoute);
        }
        // Loki takes typed reads only (`Transport::read`)
        if is_loki(request.service) {
            return Err(HttpError::Refused(Refusal::Loki));
        }
        let url = match request.call {
            Some(url) if request.service == Service::Calling && on_calling_base(&url) => url,
            Some(_) => return Err(HttpError::Refused(Refusal::NotAllowed)),
            None => self.url_for(request.service, &request.endpoint)?,
        };
        if self.read_only {
            return Err(Self::refused_read_only(request.method.as_str(), url.as_str()));
        }
        let body = request
            .body
            .or_else(|| (request.service == Service::Calling && request.method == crate::Method::Post).then(Vec::new));
        if !request.write && request.service != Service::Calling {
            if request.service == Service::Substrate {
                return Err(HttpError::Refused(Refusal::Substrate));
            }
            let outgoing = Outgoing {
                method: request.method.to_reqwest(),
                url: &url,
                service: request.service,
                body: body.as_deref(),
                headers: &request.headers,
                range: None,
            };
            return self.exchange(outgoing).await;
        }
        let mut headers = plain_headers(request.service, body.is_some() || request.service == Service::Presence);
        // a write to the media service (a picture's object, its bytes) takes JSON back, as the web client's asked
        if request.service == Service::Ams {
            for (name, value) in &mut headers {
                if name.eq_ignore_ascii_case(ACCEPT.as_str()) {
                    "application/json".clone_into(value);
                }
            }
        }
        // a request naming its own body's type (a picture's bytes) sends that one alone
        if request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()))
        {
            headers.retain(|(name, _)| !name.eq_ignore_ascii_case(CONTENT_TYPE.as_str()));
        }
        headers.extend(request.headers);
        if request.service == Service::Calling && body.as_ref().is_some_and(Vec::is_empty) {
            headers.push(("content-length".to_owned(), "0".to_owned()));
        }
        if request.service == Service::Calling {
            return self.send_call(request.method.to_reqwest(), &url, &headers, body).await;
        }
        let token = self.token(request.service).await?;
        let answer = self
            .send_as(
                request.method.to_reqwest(),
                &url,
                &headers,
                Auth::bearer(token.as_ref()),
                body,
            )
            .await?;
        if let (401, Some(audience), Some(refused)) = (answer.status(), audience(request.service), &token) {
            let _ = self.credentials.bearer_rejected(audience, refused).await;
        }
        if request.write || answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }
}

/// One request, built and checked.
struct Outgoing<'a> {
    method: reqwest::Method,
    url: &'a Url,
    service: Service,
    body: Option<&'a [u8]>,
    /// The request's extra non-credential headers, kept for each attempt.
    headers: &'a [(String, String)],
    /// A ranged read's first and last byte (`Range: bytes=<first>-<last>`).
    range: Option<(u64, u64)>,
}

impl<C: Credentials> HttpTransport<C> {
    /// Builds an HTTPS transport on a validated regional chat-service base.
    ///
    /// # Errors
    /// Returns `BuildError` for an invalid base or HTTP client setup failure.
    pub fn new(credentials: Arc<C>, chat_base: &str) -> Result<Self, BuildError> {
        Self::build(credentials, chat_base, true)
    }

    /// A transport whose requests go to `origin` (`http://127.0.0.1:<port>`, a local mock) instead, each with the path
    /// and query it was built and checked with as [`Self::new`]'s. Tests only (`test-support`).
    ///
    /// # Errors
    /// [`BuildError::MockOrigin`] unless `origin` is `http` on a loopback host (`127.0.0.1`, `[::1]`, `localhost`) with
    /// a port and nothing after it; otherwise as [`Self::new`].
    #[cfg(feature = "test-support")]
    pub fn with_mock_origin(credentials: Arc<C>, chat_base: &str, origin: &str) -> Result<Self, BuildError> {
        let origin = parse_mock_origin(origin).ok_or(BuildError::MockOrigin)?;
        let mut transport = Self::build(credentials, chat_base, false)?;
        transport.mock_origin = Some(origin);
        Ok(transport)
    }

    fn build(credentials: Arc<C>, chat_base: &str, https_only: bool) -> Result<Self, BuildError> {
        let chat_base = parse_chat_base(chat_base).ok_or(BuildError::ChatBase)?;
        let registrar_host = chat_base
            .host_str()
            .and_then(|host| AFD_HOSTS.iter().find(|afd| **afd == host))
            .copied()
            .unwrap_or(AFD_HOSTS[0]);
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            // the only retries are `exchange`'s and the outbox's (see the module docs)
            .retry(reqwest::retry::never())
            .https_only(https_only)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(BuildError::Client)?;
        Ok(Self {
            client,
            credentials,
            account: None,
            chat_base,
            registrar_host,
            middle_tier: None,
            csa: None,
            provisioning: None,
            user_profile: None,
            presence: None,
            ams: None,
            mcps: None,
            loki: None,
            max_retry_wait: MAX_RETRY_WAIT,
            mock_origin: None,
            read_only: false,
            call_state: None,
        })
    }

    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Shares the account's canonical call lifecycle with its calling transport.
    #[must_use]
    pub fn with_call_state(mut self, state: Arc<dyn crate::CallState>) -> Self {
        self.call_state = Some(state);
        self
    }

    /// Whether this transport is read-only ([`Self::read_only`]).
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// The refusal of `method` to `url` in read-only mode, logged first (a line that can't be written is said in the
    /// app's log; the request is refused either way).
    fn refused_read_only(method: &str, url: &str) -> HttpError {
        let destination = Url::parse(url).map_or_else(
            |_| "<unparsable>".to_owned(),
            |url| format!("{}{}", url.host_str().unwrap_or_default(), path_shape(url.path())),
        );
        log::info!("refused_read_only {method} {destination}");
        HttpError::Refused(Refusal::ReadOnly)
    }

    /// Assigns the account whose endpoint registrations this transport owns.
    #[must_use]
    pub fn for_account(mut self, account: Mri) -> Self {
        self.account = Some(account);
        self
    }

    /// The refusal of the mutating request `write` in read-only mode, logged with where it would have gone.
    fn refused_write<R: Request>(&self, write: &R) -> HttpError {
        let url = write
            .call_url()
            .cloned()
            .or_else(|| self.url_for(write.service(), &write.endpoint()).ok());
        Self::refused_read_only(write.method().as_str(), url.as_ref().map_or("", Url::as_str))
    }

    /// The transport with the account's middle-tier base, the region's GTM `middleTier` (trimmed): `https`, an AFD host
    /// ([`crate::AFD_HOSTS`]) with no port or user info, the path `/api/mt/part/<partition>` exactly (the
    /// partition a lowercase region, `-` and a number: `amer-03`), one trailing slash allowed, no query or fragment.
    /// Without it, every middle-tier request is refused ([`Refusal::NoMiddleTier`]).
    ///
    /// # Errors
    /// [`BuildError::MiddleTierBase`] for any other base.
    pub fn with_middle_tier(mut self, base: &str) -> Result<Self, BuildError> {
        self.middle_tier = Some(parse_middle_tier_base(base).ok_or(BuildError::MiddleTierBase)?);
        Ok(self)
    }

    /// The transport with the account's CSA base, the region's GTM `chatSvcAggAfd` (trimmed): `https`, an AFD host
    /// ([`crate::AFD_HOSTS`]) with no port or user info, the path `/api/csa/<region>` exactly (the region
    /// lowercase letters: `amer`), one trailing slash allowed, no query or fragment. Without it, every CSA request is
    /// refused ([`Refusal::NoCsa`]).
    ///
    /// # Errors
    /// [`BuildError::CsaBase`] for any other base ([`Self::accepts_csa_base`]).
    pub fn with_csa(mut self, base: &str) -> Result<Self, BuildError> {
        self.csa = Some(parse_csa_base(base).ok_or(BuildError::CsaBase)?);
        Ok(self)
    }

    /// The transport with the account's MCPS base, the region's GTM `mcpService` (trimmed): `https`, an AFD host
    /// ([`crate::AFD_HOSTS`]) with no port or user info, the path `/api/mcps/<region>` exactly (the region lowercase
    /// letters: `us`), one trailing slash allowed, no query or fragment. Without it, every MCPS read is refused
    /// ([`Refusal::NoMcps`]).
    ///
    /// # Errors
    /// [`BuildError::McpsBase`] for any other base ([`Self::accepts_mcps_base`]).
    pub fn with_mcps(mut self, base: &str) -> Result<Self, BuildError> {
        self.mcps = Some(parse_service_base(base, is_mcps_path).ok_or(BuildError::McpsBase)?);
        Ok(self)
    }

    /// The transport with Loki, the persona card service: its configuration read on the fixed global base
    /// (`https://loki.delve.office.com/`), and its person reads on the account's regional base once
    /// [`Self::set_loki_region`] took it from the configuration's answer. Without it, every Loki read is refused
    /// ([`Refusal::NoLoki`]).
    #[must_use]
    pub fn with_loki(mut self) -> Self {
        self.loki = Some(OnceLock::new());
        self
    }

    /// Whether the transport was given Loki ([`Self::with_loki`]).
    #[must_use]
    pub fn has_loki(&self) -> bool {
        self.loki.is_some()
    }

    /// Sets the account's Loki regional base, as its configuration's `LokiUrl` names it
    /// ([`crate::LokiConfiguration::read_answer`]): `https://<one label>.loki.delve.office.com/` exactly (the label
    /// lowercase letters, digits and inner `-`; the trailing slash optional), no port, user info, path, query or
    /// fragment. Taken once: setting the same base again is `Ok`, another one is refused and the first kept.
    ///
    /// # Errors
    /// [`BuildError::LokiBase`] for a base of any other form, another region than the one set, or a transport not given
    /// Loki.
    pub fn set_loki_region(&self, base: &str) -> Result<(), BuildError> {
        let slot = self.loki.as_ref().ok_or(BuildError::LokiBase)?;
        let url = parse_regional_base(base).ok_or(BuildError::LokiBase)?;
        let set = slot.get_or_init(|| url.clone());
        if *set == url { Ok(()) } else { Err(BuildError::LokiBase) }
    }

    /// The account's Loki regional base, once set ([`Self::set_loki_region`]).
    #[must_use]
    pub fn loki_region(&self) -> Option<&str> {
        self.loki.as_ref().and_then(OnceLock::get).map(Url::as_str)
    }

    /// Whether [`Self::with_mcps`] takes `base`.
    #[must_use]
    pub fn accepts_mcps_base(base: &str) -> bool {
        parse_service_base(base, is_mcps_path).is_some()
    }

    /// Whether [`Self::with_csa`] takes `base`.
    #[must_use]
    pub fn accepts_csa_base(base: &str) -> bool {
        parse_csa_base(base).is_some()
    }

    /// Whether the transport was given the account's CSA base ([`Self::with_csa`]): without one, every CSA request is
    /// refused.
    #[must_use]
    pub fn has_csa(&self) -> bool {
        self.csa.is_some()
    }

    /// The transport with the account's provisioning base, the region's GTM `teamsAndChannelsProvisioningService`
    /// (trimmed): `https`, an AFD host ([`crate::AFD_HOSTS`]) with no port or user info, the path
    /// `/fabric/<region>/templates/api` exactly, one trailing slash allowed, no query or fragment. Without it, every
    /// provisioning request is refused ([`Refusal::NoProvisioning`]).
    ///
    /// # Errors
    /// [`BuildError::ProvisioningBase`] for any other base.
    pub fn with_provisioning(mut self, base: &str) -> Result<Self, BuildError> {
        self.provisioning = Some(
            parse_service_base(base, crate::channel_read::is_provisioning_path).ok_or(BuildError::ProvisioningBase)?,
        );
        Ok(self)
    }

    /// Whether [`Self::with_provisioning`] takes `base`.
    #[must_use]
    pub fn accepts_provisioning_base(base: &str) -> bool {
        parse_service_base(base, crate::channel_read::is_provisioning_path).is_some()
    }

    /// The transport with the account's user profile base, the region's GTM `userProfileService` (trimmed): `https`, an
    /// AFD host ([`crate::AFD_HOSTS`]) with no port or user info, the path `/api/userprofilesvc/<region>` exactly,
    /// one trailing slash allowed, no query or fragment. Without it, every user profile request is refused
    /// ([`Refusal::NoUserProfile`]).
    ///
    /// # Errors
    /// [`BuildError::UserProfileBase`] for any other base.
    pub fn with_user_profile(mut self, base: &str) -> Result<Self, BuildError> {
        self.user_profile = Some(
            parse_service_base(base, crate::channel_read::is_user_profile_path).ok_or(BuildError::UserProfileBase)?,
        );
        Ok(self)
    }

    /// Whether [`Self::with_user_profile`] takes `base`.
    #[must_use]
    pub fn accepts_user_profile_base(base: &str) -> bool {
        parse_service_base(base, crate::channel_read::is_user_profile_path).is_some()
    }

    /// The transport with the account's presence base, the region's GTM `presenceUPS` (trimmed): `https`, an AFD host
    /// ([`crate::AFD_HOSTS`]) with no port or user info, the path `/ups/<geo>` exactly (the geo lowercase
    /// letters: `noam`), one trailing slash allowed, no query or fragment. Without it, every presence request is
    /// refused ([`Refusal::NoPresence`]).
    ///
    /// # Errors
    /// [`BuildError::PresenceBase`] for any other base ([`Self::accepts_presence_base`]).
    pub fn with_presence(mut self, base: &str) -> Result<Self, BuildError> {
        self.presence = Some(parse_presence_base(base).ok_or(BuildError::PresenceBase)?);
        Ok(self)
    }

    /// Whether [`Self::with_presence`] takes `base`.
    #[must_use]
    pub fn accepts_presence_base(base: &str) -> bool {
        parse_presence_base(base).is_some()
    }

    /// Whether the transport was given the account's presence base ([`Self::with_presence`]): without one, every
    /// presence request is refused.
    #[must_use]
    pub fn has_presence(&self) -> bool {
        self.presence.is_some()
    }

    /// The transport with the account's AMS base, the region's GTM `amsV2` (trimmed): `https`, a host
    /// `<name>.asyncgw.teams.microsoft.com` (`<name>` lowercase ASCII letters, digits and `-`: `us-prod`) with no port or
    /// user info, no path but `/`, no query or fragment. Without it, every AMS read is refused ([`Refusal::NoAms`]).
    ///
    /// # Errors
    /// [`BuildError::AmsBase`] for any other base ([`Self::accepts_ams_base`]).
    pub fn with_ams(mut self, base: &str) -> Result<Self, BuildError> {
        self.ams = Some(parse_ams_base(base).ok_or(BuildError::AmsBase)?);
        Ok(self)
    }

    /// Whether [`Self::with_ams`] takes `base`.
    #[must_use]
    pub fn accepts_ams_base(base: &str) -> bool {
        parse_ams_base(base).is_some()
    }

    /// Another cap on the wait before a throttled read, plain or own-state request is sent again.
    #[must_use]
    pub fn with_max_retry_wait(mut self, wait: Duration) -> Self {
        self.max_retry_wait = wait;
        self
    }

    /// The chat base requests are built on.
    #[must_use]
    pub fn chat_base(&self) -> &str {
        self.chat_base.as_str()
    }

    /// The account's tenant id, as its `ic3` token's `tid` claim names it (the test call's `add` names it as the
    /// organizer's); `None` without a token, or one that isn't a JWT with that claim. Nothing of the token leaves here.
    pub async fn account_tenant(&self) -> Option<String> {
        let token = self.credentials.bearer(IC3_AUDIENCE).await.ok()?;
        TokenSummary::read(&token)?.tenant
    }

    /// Whether the transport was given the account's middle-tier base ([`Self::with_middle_tier`]): without one, every
    /// middle-tier request is refused.
    #[must_use]
    pub fn has_middle_tier(&self) -> bool {
        self.middle_tier.is_some()
    }

    /// The URL of `endpoint` on `service`, built on the service's base and checked against it.
    fn url_for(&self, service: Service, endpoint: &Endpoint) -> Result<Url, HttpError> {
        let refused = |refusal| HttpError::Refused(refusal);
        if endpoint
            .segments()
            .iter()
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(refused(Refusal::Segment));
        }
        let base = self.base(service, endpoint)?;
        let mut url = base.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|()| refused(Refusal::NotAllowed))?;
            segments.pop_if_empty().extend(endpoint.segments());
            if trailing_slash(service, endpoint) {
                // UPS's paths end in a slash, as every captured one did (but a subscription's), and MCPS's
                segments.push("");
            }
        }
        if !endpoint.query().is_empty() {
            url.query_pairs_mut().extend_pairs(endpoint.query());
        }
        if on_base(service, &base, &url, endpoint) {
            Ok(url)
        } else {
            Err(refused(Refusal::NotAllowed))
        }
    }

    /// `service`'s base, the one every URL of it is built on.
    fn base(&self, service: Service, endpoint: &Endpoint) -> Result<Url, HttpError> {
        let refused = |refusal| HttpError::Refused(refusal);
        let fixed = |base: String| Url::parse(&base).map_err(|_| refused(Refusal::NotAllowed));
        match service {
            Service::ChatService if endpoint.segments().first().is_none_or(|first| first != "v1") => {
                Err(refused(Refusal::ChatPath))
            }
            Service::ChatService => Ok(self.chat_base.clone()),
            Service::Registrar => fixed(format!("https://{}/registrar", self.registrar_host)),
            Service::Trouter => fixed(format!("https://{TROUTER_HOST}/")),
            Service::Graph => fixed(format!("https://{GRAPH_HOST}/v1.0")),
            Service::Workflows => fixed("https://api.flow.microsoft.com/".to_owned()),
            Service::WorkflowConnections => fixed("https://api.powerapps.com/".to_owned()),
            Service::WorkflowRuntime => Err(refused(Refusal::NotAllowed)),
            Service::MiddleTier => self.middle_tier.clone().ok_or(refused(Refusal::NoMiddleTier)),
            Service::Csa => self.csa.clone().ok_or(refused(Refusal::NoCsa)),
            Service::Provisioning => self.provisioning.clone().ok_or(refused(Refusal::NoProvisioning)),
            Service::UserProfile => self.user_profile.clone().ok_or(refused(Refusal::NoUserProfile)),
            Service::Presence => self.presence.clone().ok_or(refused(Refusal::NoPresence)),
            Service::Calling => Err(refused(Refusal::Calling)),
            Service::FileThumbnail => Err(refused(Refusal::FileThumbnail)),
            Service::FileContent => Err(refused(Refusal::FileContent)),
            Service::Relay => fixed(format!("https://{RELAY_HOST}/{RELAY_PATH}")),
            Service::Emoticons => fixed(format!("https://{EMOTICONS_HOST}/{EMOTICONS_PATH}")),
            Service::Giphy => fixed(format!("https://{GIPHY_HOST}/")),
            Service::AppPictures => fixed(format!("https://{APP_PICTURES_HOST}/{APP_PICTURES_PATH}")),
            Service::StaticAppPictures => fixed(format!("https://{STATIC_APP_PICTURES_HOST}/")),
            Service::Substrate => fixed(format!("https://{SUBSTRATE_HOST}/")),
            Service::Ams => self.ams.clone().ok_or(refused(Refusal::NoAms)),
            Service::Mcps => self.mcps.clone().ok_or(refused(Refusal::NoMcps)),
            Service::LokiGlobal if self.loki.is_none() => Err(refused(Refusal::NoLoki)),
            // the global host answers its configuration only (any other path is refused there)
            Service::LokiGlobal if endpoint.segments() != CONFIGURATION_PATH => Err(refused(Refusal::Loki)),
            Service::LokiGlobal => fixed(format!("https://{LOKI_GLOBAL_HOST}/")),
            Service::Loki => self
                .loki
                .as_ref()
                .and_then(OnceLock::get)
                .cloned()
                .ok_or(refused(Refusal::NoLoki)),
        }
    }

    async fn send_plain(
        &self,
        service: Service,
        method: reqwest::Method,
        endpoint: &Endpoint,
        body: Option<&Value>,
    ) -> Result<Response, HttpError> {
        if service == Service::Substrate {
            return Err(HttpError::Refused(Refusal::Substrate));
        }
        if is_loki(service) {
            return Err(HttpError::Refused(Refusal::Loki));
        }
        // Cloud operations must use the typed route's independent lab/read-only checks and one-attempt policy.
        if matches!(
            service,
            Service::Workflows | Service::WorkflowConnections | Service::WorkflowRuntime
        ) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let url = self.url_for(service, endpoint)?;
        if self.read_only && !matches!(method, reqwest::Method::GET | reqwest::Method::HEAD) {
            return Err(Self::refused_read_only(method.as_str(), url.as_str()));
        }
        let body = body.map(|body| body.to_string().into_bytes());
        let outgoing = Outgoing {
            method,
            url: &url,
            service,
            body: body.as_deref(),
            headers: &[],
            range: None,
        };
        self.exchange(outgoing).await
    }

    /// Where a checked URL is sent: itself, or (`test-support` only) the mock origin with its path and query.
    fn destination(&self, url: &Url) -> Url {
        match &self.mock_origin {
            Some(origin) => {
                let mut mocked = origin.clone();
                mocked.set_path(url.path());
                mocked.set_query(url.query());
                mocked
            }
            None => url.clone(),
        }
    }

    async fn token(&self, service: Service) -> Result<Option<Secret>, HttpError> {
        match audience(service) {
            Some(audience) => Ok(Some(
                self.credentials
                    .bearer(audience)
                    .await
                    .map_err(HttpError::Credentials)?,
            )),
            None => Ok(None),
        }
    }

    /// Sends a read, plain or own-state request, with the one retry a 401,
    /// 429 or 503 allows; any answer but a success is [`HttpError::Status`].
    async fn exchange(&self, outgoing: Outgoing<'_>) -> Result<Response, HttpError> {
        let mut token = self.token(outgoing.service).await?;
        let first = self.attempt(&outgoing, token.as_ref()).await?;
        let retry = match (first.status(), audience(outgoing.service), &token) {
            (401, Some(audience), Some(refused)) => {
                log::info!("{}: HTTP 401, sent once more with a renewed token", shown(&outgoing));
                let renewed = self.credentials.bearer_rejected(audience, refused).await;
                token = Some(renewed.map_err(HttpError::Credentials)?);
                true
            }
            (status @ (429 | 503), ..) => {
                let wait = first.retry_after().unwrap_or(DEFAULT_RETRY_WAIT);
                let wait = wait.min(self.max_retry_wait);
                log::info!(
                    "{}: HTTP {status}, sent once more after {} ms",
                    shown(&outgoing),
                    wait.as_millis()
                );
                tokio::time::sleep(wait).await;
                // the session may have signed out (or renewed) during the wait: the retry takes what it holds now
                token = self.token(outgoing.service).await?;
                true
            }
            _ => false,
        };
        let answer = if retry {
            self.attempt(&outgoing, token.as_ref()).await?
        } else {
            first
        };
        if answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }

    /// One logged attempt of a read, plain or own-state request.
    async fn attempt(&self, outgoing: &Outgoing<'_>, token: Option<&Secret>) -> Result<Response, HttpError> {
        let mut headers = plain_headers(outgoing.service, outgoing.body.is_some());
        headers.extend_from_slice(outgoing.headers);
        if let Some((first, last)) = outgoing.range {
            headers.push((RANGE.as_str().to_owned(), format!("bytes={first}-{last}")));
        }
        let body = outgoing.body.map(<[u8]>::to_vec);
        self.send_as(
            outgoing.method.clone(),
            outgoing.url,
            &headers,
            Auth::bearer(token),
            body,
        )
        .await
    }

    /// One request with exactly `headers` (lower-cased names, none of them a credential) and the credential `auth`
    /// (none, a bearer token, or the skypetoken), and its answer read.
    async fn send_as(
        &self,
        method: reqwest::Method,
        url: &Url,
        headers: &[(String, String)],
        auth: Auth<'_>,
        body: Option<Vec<u8>>,
    ) -> Result<Response, HttpError> {
        let map = header_map(headers, auth)?;
        let full = log_request(&method, url, &map, body.as_deref());
        let shown = format!("{method} {}{}", url.host_str().unwrap_or_default(), request_path(url));
        let started = Instant::now();
        let mut request = self.client.request(method.clone(), self.destination(url)).headers(map);
        if let Some(body) = body {
            request = request.body(body);
        }
        let answered = match request.send().await {
            Ok(answer) => read_answer(answer, full, max_body(url)).await,
            Err(error) => Err(failure(&error)),
        };
        let elapsed = started.elapsed();
        match &answered {
            Ok((answer, headers)) => {
                log::info!("{shown} -> HTTP {} in {} ms", answer.status(), elapsed.as_millis());
                log_response(&method, url, answer, headers, elapsed, full);
            }
            Err(error) => log::info!("{shown} failed after {} ms: {error}", elapsed.as_millis()),
        }
        answered.map(|(answer, _)| answer)
    }

    async fn send_post_read<R: PostRead + Send + Sync>(&self, read: R) -> Result<Response, HttpError> {
        let shape = crate::post_read::sealed::Sealed::shape(&read);
        let wire = crate::post_read::sealed::Sealed::wire(&read);
        let service = shape.service();
        let url = self.url_for(service, wire.endpoint())?;
        let mut headers = plain_headers(service, true);
        headers.extend(wire.headers().iter().cloned());
        let body = serde_json::to_vec(wire.body()).map_err(|_| HttpError::Unknown)?;
        let query: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        if shape
            .read_back(wire.endpoint().segments(), &query, &headers, &body)
            .as_deref()
            != Some(read.names())
        {
            return Err(HttpError::Unknown);
        }
        let token = self.token(service).await?;
        let answer = self
            .send_as(
                reqwest::Method::POST,
                &url,
                &headers,
                Auth::bearer(token.as_ref()),
                Some(body),
            )
            .await?;
        if let (401, Some(audience), Some(refused)) = (answer.status(), audience(service), &token) {
            let _ = self.credentials.bearer_rejected(audience, refused).await;
        }
        if answer.is_success() {
            Ok(answer)
        } else {
            Err(HttpError::Status(Box::new(answer)))
        }
    }
}

/// Requests from the sealed own-state and read lists, plus file-content reads.
impl<C: Credentials> HttpTransport<C> {
    ///
    /// # Errors
    /// Returns an HTTP, credential, or protocol-shape error.
    pub async fn own_state<R: OwnState>(&self, request: &R) -> Result<Response, HttpError> {
        let (service, method, endpoint, body) =
            (request.service(), request.method(), request.endpoint(), request.body());
        self.send_plain(service, method.to_reqwest(), &endpoint, body.as_ref())
            .await
    }

    ///
    /// # Errors
    /// Returns an HTTP, credential, or protocol-shape error.
    pub async fn search(&self, search: &SubstrateSearch) -> Result<Response, HttpError> {
        let service = search.service();
        let endpoint = search.endpoint();
        let url = self.url_for(service, &endpoint)?;
        let body = search.body().to_string().into_bytes();
        let method = reqwest::Method::POST;
        let outgoing = Outgoing {
            method,
            url: &url,
            service,
            body: Some(&body),
            headers: &[],
            range: None,
        };
        self.exchange(outgoing).await
    }

    /// Sends one typed POST read after validating its protocol shape.
    ///
    /// # Errors
    /// Returns an HTTP, credential, or protocol-shape error.
    pub async fn post_read<R: PostRead + Send + Sync>(&self, read: R) -> Result<Response, HttpError> {
        self.send_post_read(read).await
    }

    /// Reads the picture of a file's thumbnail at `link`, the link Graph handed out for it
    /// ([`crate::FileThumbnails::read_answer`]): a `GET` on [`Service::FileThumbnail`] with no token of the account's
    /// (the link carries its own credential), the link checked again before it goes ([`is_thumbnail_link`]), with the
    /// one retry a 429 or 503 allows, as any read.
    ///
    /// # Errors
    /// As a read's: [`Refusal::NotAllowed`] for a link off the thumbnail hosts, [`HttpError::Status`] for an answer that
    /// isn't a success.
    pub async fn file_thumbnail(&self, link: &ThumbnailLink) -> Result<Response, HttpError> {
        let url = link.url();
        if !is_thumbnail_link(url) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let outgoing = Outgoing {
            method: reqwest::Method::GET,
            url,
            service: Service::FileThumbnail,
            body: None,
            headers: &[],
            range: None,
        };
        self.exchange(outgoing).await
    }

    /// Reads `len` bytes of a shared file from `offset`, at the download link Graph handed out for it
    /// ([`crate::FileDownload::read_answer`]): a `GET` on [`Service::FileContent`] with `Range: bytes=<first>-<last>`
    /// and no token of the account's (the link carries its own credential), the link checked again before it goes
    /// ([`is_content_link`]), with the one retry a 429 or 503 allows, as any read. The answer is read as a part of the
    /// file by [`crate::FileRange::read`].
    ///
    /// # Errors
    /// As a read's: [`Refusal::NotAllowed`] for a link off SharePoint's hosts, [`Refusal::FileContent`] for a read of
    /// nothing or of more than [`MAX_RANGE`], [`HttpError::Status`] for an answer that isn't a success (an expired
    /// link's 401 or 403 among them: the caller asks Graph for a new one).
    pub async fn file_range(&self, link: &FileContentLink, offset: u64, len: u32) -> Result<Response, HttpError> {
        let url = link.url();
        if !is_content_link(url) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        if len == 0 || len > MAX_RANGE {
            return Err(HttpError::Refused(Refusal::FileContent));
        }
        let last = offset
            .checked_add(u64::from(len) - 1)
            .ok_or(HttpError::Refused(Refusal::FileContent))?;
        let outgoing = Outgoing {
            method: reqwest::Method::GET,
            url,
            service: Service::FileContent,
            body: None,
            headers: &[],
            range: Some((offset, last)),
        };
        self.exchange(outgoing).await
    }

    /// Reads a part of a meeting recording's presentation (an initialization or a media segment) at its address on
    /// the transform service ([`crate::MediaSegmentLink`]): a `GET` on [`Service::FileContent`] with no token of the
    /// account's (the address carries its own credential), the address checked again before it goes
    /// ([`crate::recording::is_media_link`]), with the one retry a 429 or 503 allows, as any read; its body up to
    /// [`MAX_SEGMENT_BODY`].
    ///
    /// # Errors
    /// As a read's: [`Refusal::NotAllowed`] for an address off the transform service, [`HttpError::Status`] for an
    /// answer that isn't a success (an expired address's 401 or 403 among them: the caller reads the presentation
    /// again).
    pub async fn media_segment(&self, link: &crate::MediaSegmentLink) -> Result<Response, HttpError> {
        let url = link.url();
        if !crate::recording::is_media_link(url) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        let outgoing = Outgoing {
            method: reqwest::Method::GET,
            url,
            service: Service::FileContent,
            body: None,
            headers: &[],
            range: None,
        };
        self.exchange(outgoing).await
    }

    /// Streams `len` bytes (at most [`MAX_STREAM`]) of a shared file from `offset`, at the download link Graph handed
    /// out for it: the [`HttpTransport::file_range`] `GET` (`Range: bytes=<first>-<last>`, no token of the account's,
    /// the link checked again before it goes), its answer handed back once its headers came and its body read part by
    /// part as it arrives ([`FileBody`]), never held whole; dropping it stops the read. It may take up to
    /// [`STREAM_TIMEOUT`], and nothing retries it here (its reader starts another where this one stopped).
    ///
    /// # Errors
    /// [`Refusal::NotAllowed`] for a link off SharePoint's hosts, [`Refusal::FileContent`] for a read of nothing or of
    /// more than [`MAX_STREAM`], [`HttpError::Status`] for an answer that isn't a success (an expired link's 401 or 403
    /// among them: the caller asks Graph for a new one), [`HttpError::Failed`] when it failed on the way.
    pub async fn file_stream(&self, link: &FileContentLink, offset: u64, len: u64) -> Result<FileBody, HttpError> {
        let url = link.url();
        if !is_content_link(url) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        if len == 0 || len > MAX_STREAM {
            return Err(HttpError::Refused(Refusal::FileContent));
        }
        let last = offset
            .checked_add(len - 1)
            .ok_or(HttpError::Refused(Refusal::FileContent))?;
        let mut headers = plain_headers(Service::FileContent, false);
        headers.push((RANGE.as_str().to_owned(), format!("bytes={offset}-{last}")));
        let map = header_map(&headers, Auth::None)?;
        let method = reqwest::Method::GET;
        let full = log_request(&method, url, &map, None);
        let shown = format!("GET {}{}", url.host_str().unwrap_or_default(), url.path());
        let started = Instant::now();
        let sent = self
            .client
            .get(self.destination(url))
            .headers(map)
            .timeout(STREAM_TIMEOUT)
            .send()
            .await;
        let answer = match sent {
            Ok(answer) => answer,
            Err(error) => {
                let error = failure(&error);
                log::info!("{shown} failed after {} ms: {error}", started.elapsed().as_millis());
                return Err(error);
            }
        };
        let status = answer.status().as_u16();
        log::info!(
            "{shown} -> HTTP {status} in {} ms, streamed",
            started.elapsed().as_millis()
        );
        if !answer.status().is_success() {
            let (response, kept) = read_answer(answer, full, MAX_BODY).await?;
            log_response(&method, url, &response, &kept, started.elapsed(), full);
            return Err(HttpError::Status(Box::new(response)));
        }
        let content_range = answer
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok());
        let start = StreamStart::read(status, content_range, answer.content_length(), offset, len);
        if full {
            let kept = header_pairs(answer.headers());
            log_response(
                &method,
                url,
                &Response::new(status, Vec::new()),
                &kept,
                started.elapsed(),
                full,
            );
        }
        Ok(FileBody {
            start,
            left: start.map_or(0, |start| start.len),
            response: answer,
            shown,
            started,
            streamed: 0,
        })
    }
}

/// A streamed read's answer ([`HttpTransport::file_stream`]): what it holds, and its body part by part as it arrives.
/// Dropped before its end, the read stops (its HTTP/2 stream is reset).
pub struct FileBody {
    start: Option<StreamStart>,
    response: reqwest::Response,
    /// The bytes still to come.
    left: u64,
    shown: String,
    started: Instant,
    streamed: u64,
}

impl fmt::Debug for FileBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileBody")
            .field("start", &self.start)
            .field("left", &self.left)
            .finish_non_exhaustive()
    }
}

impl FileBody {
    /// Where the answer starts and what it holds; `None` when it isn't the part asked for ([`StreamStart::read`]).
    #[must_use]
    pub const fn start(&self) -> Option<StreamStart> {
        self.start
    }

    /// The next part of the body, in order (never past what [`FileBody::start`] holds); `None` once all of it came.
    ///
    /// # Errors
    /// [`HttpError::Failed`] when the body broke off or ended short.
    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, HttpError> {
        if self.left == 0 {
            return Ok(None);
        }
        let Some(chunk) = self.response.chunk().await.map_err(|error| failure(&error))? else {
            return Err(HttpError::Failed(Failure::Body));
        };
        let take = usize::try_from(self.left).unwrap_or(usize::MAX).min(chunk.len());
        let taken = chunk.get(..take).unwrap_or_default().to_vec();
        self.left -= taken.len() as u64;
        self.streamed += taken.len() as u64;
        Ok(Some(taken))
    }
}

impl Drop for FileBody {
    /// Tells the log how much came, and whether it stopped before its end (its reader let it go).
    fn drop(&mut self) {
        log::info!(
            "{}: {} bytes streamed in {} ms{}",
            self.shown,
            self.streamed,
            self.started.elapsed().as_millis(),
            if self.left == 0 { "" } else { ", stopped before its end" }
        );
    }
}

impl<C: Credentials> Transport for HttpTransport<C> {
    type Error = HttpError;

    async fn deliver_caption(&self, write: crate::captions::CaptionWrite) -> Option<Result<Response, HttpError>> {
        Some(self.deliver_caption_write(&write).await)
    }

    async fn deliver_attachment(&self, write: crate::DriveWrite) -> Option<Result<Response, HttpError>> {
        if self.read_only {
            return Some(Err(Self::refused_read_only(
                write.method().as_str(),
                write.url().as_str(),
            )));
        }
        if let Err(error) = self.preflight_attachment(&write).await {
            return Some(Err(error));
        }
        Some(self.deliver_drive(&write).await)
    }

    fn read<R: Read>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        let service = request.service();
        let endpoint = request.endpoint();
        async move {
            if service == Service::Substrate {
                return Err(HttpError::Refused(Refusal::Substrate));
            }
            let url = self.url_for(service, &endpoint)?;
            let outgoing = Outgoing {
                method: reqwest::Method::GET,
                url: &url,
                service,
                body: None,
                headers: &[],
                range: None,
            };
            self.exchange(outgoing).await
        }
    }

    fn workflow(&self, request: &crate::WorkflowRequest) -> impl Future<Output = crate::WorkflowResponse> + Send {
        self.workflow(request)
    }

    fn send<R: Request>(&self, request: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        let built = BuiltRequest::of(request);
        async move { self.send_built(built).await }
    }

    async fn channel_own<R: crate::ChannelOwn + Sync>(&self, request: &R) -> Result<Response, HttpError> {
        let service = request.service();
        let method = request.method().to_reqwest();
        let url = self.url_for(service, &request.endpoint())?;
        if self.read_only {
            return Err(Self::refused_read_only(method.as_str(), url.as_str()));
        }
        if request
            .interest_account()
            .is_some_and(|requested| self.own_account().is_none_or(|own| requested != own.as_str()))
        {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        self.verify_account_actor().await?;
        let token = self
            .credentials
            .bearer(audience(service).ok_or(HttpError::NoRoute)?)
            .await
            .map_err(HttpError::Credentials)?;
        let body = request.body().map(|value| value.to_string().into_bytes());
        let headers = plain_headers(service, body.is_some());
        self.send_as(method, &url, &headers, Auth::Bearer(&token), body).await
    }

    async fn send_own<R: OwnState + Request + Sync>(&self, request: &R) -> Result<Response, HttpError> {
        self.own_state(request).await
    }

    fn own_account(&self) -> Option<&Mri> {
        self.account.as_ref()
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn read_only_refusal<R: Request>(&self, write: &R) -> Option<HttpError> {
        self.read_only.then(|| self.refused_write(write))
    }

    async fn deliver<R: Request + Send>(&self, write: R) -> Result<Response, HttpError> {
        self.send_built(BuiltRequest::of(&write).writing()).await
    }
}

/// The credential a request carries: none, a bearer token, or the account's skypetoken (the calling service's).
#[derive(Clone, Copy)]
enum Auth<'a> {
    None,
    Bearer(&'a Secret),
    Skypetoken(&'a Secret),
    Recorder { bearer: &'a Secret, skype: &'a Secret },
}

impl<'a> Auth<'a> {
    /// A bearer token when there is one.
    fn bearer(token: Option<&'a Secret>) -> Self {
        token.map_or(Self::None, Self::Bearer)
    }
}

/// The header the calling service takes the skypetoken in.
const SKYPETOKEN_HEADER: &str = "x-skypetoken";

/// The TURN credentials' host and path ([`Service::Relay`]).
pub const RELAY_HOST: &str = "teams.microsoft.com";
const RELAY_PATH: &str = "trap";

/// The async gateway's hosts end in this ([`Service::Ams`]: `us-prod.asyncgw.teams.microsoft.com`).
pub const AMS_HOST_SUFFIX: &str = ".asyncgw.teams.microsoft.com";

/// The `User-Agent` the media service is sent, as a Chrome on Linux running the web client sends it: the service makes
/// the client's platform of it.
const WEB_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";

/// The `Accept` a picture's view is read with, as a browser's `<img>` sends it.
const PICTURE_ACCEPT: &str = "image/avif,image/webp,image/apng,image/*,*/*;q=0.8";

/// Teams' reaction pictures' host and path ([`Service::Emoticons`]).
pub const EMOTICONS_HOST: &str = "statics.teams.cdn.office.net";
const EMOTICONS_PATH: &str = "evergreen-assets/personal-expressions/v2/assets/emoticons";

/// Giphy's media host ([`Service::Giphy`]).
pub const GIPHY_HOST: &str = "media.giphy.com";

/// The headers every Loki request carries, as the web client's persona card sends them (as real headers here, not in
/// its simple request's body).
const LOKI_HEADERS: &[(&str, &str)] = &[
    ("x-clienttype", "Teams"),
    ("x-clientfeature", "LivePersonaCard"),
    ("x-clientarchitectureversion", "v2"),
    ("x-hostappplatform", "Web"),
];

/// Whether `service` is Loki's, either base.
const fn is_loki(service: Service) -> bool {
    matches!(service, Service::Loki | Service::LokiGlobal)
}

/// The `Accept` a GIF is read with: a GIF, whose frames the views play (a browser's `Accept` would take a WebP, of
/// which only the first frame is drawn).
const GIF_ACCEPT: &str = "image/gif";

/// The Entra audience of the bearer token `service` takes, if it takes one.
fn audience(service: Service) -> Option<&'static str> {
    match service {
        Service::ChatService | Service::Registrar | Service::Relay | Service::Ams => Some(IC3_AUDIENCE),
        Service::MiddleTier | Service::UserProfile => Some(MIDDLE_TIER_AUDIENCE),
        Service::Provisioning => Some(PROVISIONING_AUDIENCE),
        Service::Graph => Some(GRAPH_AUDIENCE),
        Service::Csa => Some(CSA_AUDIENCE),
        Service::Mcps => Some(crate::recap::MCPS_AUDIENCE),
        Service::Presence => Some(PRESENCE_AUDIENCE),
        Service::Substrate => Some(SUBSTRATE_AUDIENCE),
        Service::Workflows => Some(crate::workflow::WORKFLOW_AUDIENCE),
        Service::WorkflowConnections => Some(crate::workflow::CONNECTION_AUDIENCE),
        Service::WorkflowRuntime => Some(crate::workflow::RUNTIME_AUDIENCE),
        Service::Loki | Service::LokiGlobal => Some(LOKI_AUDIENCE),
        Service::Trouter
        | Service::Calling
        | Service::Emoticons
        | Service::FileThumbnail
        | Service::FileContent
        | Service::Giphy
        | Service::AppPictures
        | Service::StaticAppPictures => None,
    }
}

/// The headers a request to `service` goes out with besides its token, by lower-cased name: `Accept`, the body's
/// `Content-Type` when it has one, and the service's own.
fn plain_headers(service: Service, body: bool) -> Vec<(String, String)> {
    let content_type = match service {
        Service::MiddleTier | Service::UserProfile => "application/json;charset=UTF-8",
        Service::Provisioning
        | Service::Workflows
        | Service::WorkflowConnections
        | Service::WorkflowRuntime
        | Service::ChatService
        | Service::Registrar
        | Service::Trouter
        | Service::Graph
        | Service::Csa
        | Service::Mcps
        | Service::Presence
        | Service::Calling
        | Service::Relay
        | Service::Emoticons
        | Service::Ams
        | Service::FileThumbnail
        | Service::FileContent
        | Service::Giphy
        | Service::AppPictures
        | Service::StaticAppPictures
        | Service::Substrate
        | Service::Loki
        | Service::LokiGlobal => "application/json",
    };
    // a picture's view and a thumbnail answer an image, as a browser's `<img>` asks for one
    let accept = match service {
        Service::Ams | Service::FileThumbnail | Service::AppPictures | Service::StaticAppPictures => PICTURE_ACCEPT,
        Service::Giphy => GIF_ACCEPT,
        // a file's bytes, whatever it holds, as a browser's media element asks for them
        Service::FileContent => "*/*",
        Service::ChatService
        | Service::Registrar
        | Service::Trouter
        | Service::MiddleTier
        | Service::Provisioning
        | Service::Workflows
        | Service::WorkflowConnections
        | Service::WorkflowRuntime
        | Service::UserProfile
        | Service::Graph
        | Service::Csa
        | Service::Mcps
        | Service::Presence
        | Service::Calling
        | Service::Relay
        | Service::Emoticons
        | Service::Substrate
        | Service::Loki
        | Service::LokiGlobal => "application/json",
    };
    let mut headers = vec![(ACCEPT.as_str().to_owned(), accept.to_owned())];
    if body {
        headers.push((CONTENT_TYPE.as_str().to_owned(), content_type.to_owned()));
    }
    headers.extend(
        service_headers(service)
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
    );
    headers
}

/// The headers `service` takes besides the token, as the web client sends them.
fn service_headers(service: Service) -> &'static [(&'static str, &'static str)] {
    match service {
        Service::ChatService => &[("x-ms-migration", "True"), ("behavioroverride", "redirectAs404")],
        Service::Registrar => &[("x-ms-migration", "True"), ("x-ms-test-user", "False")],
        Service::MiddleTier => &[
            ("x-ms-client-request-type", "0"),
            ("x-ms-client-type", "cdlworker"),
            ("x-ms-migration", "True"),
            ("x-ms-object-id", ""),
            ("x-ms-test-user", "False"),
        ],
        // as the capture's search sent them: without them the service answered 400
        Service::UserProfile => &[
            ("x-client-ui-language", "en-us"),
            ("x-ms-client-type", "cdlworker"),
            ("x-ms-migration", "True"),
            ("x-ms-object-id", ""),
            ("x-ms-test-user", "False"),
        ],
        // the values every captured presence read carried
        Service::Presence => &[
            ("behavioroverride", "redirectAs404"),
            ("x-ms-client-type", "cdlworker"),
            ("x-ms-client-user-agent", "Teams-V2-Web"),
            ("x-ms-object-id", ""),
        ],
        // the Entra form of the web client's read (`docs/protocol/auth.md`)
        Service::Relay => &[("api-version", "2"), ("x-ms-migration", "True")],
        // the object read's form (`proto-lab probe object`), as the web client's object creation sent it
        // the media service makes a platform id of the `User-Agent` and the client's build, and answers 400 without them:
        // the web client's
        Service::Ams => &[
            ("x-ms-migration", "True"),
            ("x-ms-test-user", "False"),
            ("user-agent", WEB_USER_AGENT),
            ("x-ms-client-version", crate::own::CLIENT_BUILD),
        ],
        // the persona card's own: without the client type Loki answers 400 (`ClientTypeMissing`)
        Service::Loki | Service::LokiGlobal => LOKI_HEADERS,
        Service::Trouter
        | Service::Graph
        | Service::Csa
        | Service::Mcps
        | Service::Provisioning
        | Service::Workflows
        | Service::WorkflowConnections
        | Service::WorkflowRuntime
        | Service::Calling
        | Service::Emoticons
        | Service::FileThumbnail
        | Service::FileContent
        | Service::Giphy
        | Service::AppPictures
        | Service::StaticAppPictures
        | Service::Substrate => &[],
    }
}

/// Whether `url`, built for `endpoint` on `service`, is on `base` and nothing else: `https`, no user info, port or
/// fragment, the base's host, a decoded path that is exactly the base's and the endpoint's segments (UPS's with its
/// trailing slash, but a subscription's), and the endpoint's query exactly. The chat service's also as
/// [`chat_service_path`] reads it, and the registrar's on an AFD host.
fn on_base(service: Service, base: &Url, url: &Url, endpoint: &Endpoint) -> bool {
    let plain = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none();
    let segments = endpoint.segments();
    let mut wanted = decoded_path(base).unwrap_or_default();
    wanted.extend(segments.iter().cloned());
    if trailing_slash(service, endpoint) {
        wanted.push(String::new());
    }
    let service_rule = match service {
        Service::ChatService => chat_service_path(url).is_some_and(|rest| segments.get(1..) == Some(&rest[..])),
        Service::Registrar => url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host)),
        Service::Trouter
        | Service::Graph
        | Service::MiddleTier
        | Service::Csa
        | Service::Mcps
        | Service::Provisioning
        | Service::Workflows
        | Service::WorkflowConnections
        | Service::WorkflowRuntime
        | Service::UserProfile
        | Service::Presence
        | Service::Relay
        | Service::Calling
        | Service::Emoticons
        | Service::Ams
        | Service::FileThumbnail
        | Service::FileContent
        | Service::Giphy
        | Service::AppPictures
        | Service::StaticAppPictures
        | Service::Substrate
        | Service::Loki
        | Service::LokiGlobal => true,
    };
    plain
        && url.host_str() == base.host_str()
        && decoded_path(url).is_some_and(|path| path == wanted)
        && query_is(url, endpoint.query())
        && service_rule
}

/// Whether `endpoint`'s path on `service` ends in a slash: UPS's ([`ups_slash`]), MCPS's, and AMS's object creation
/// (`v1/objects/`, as the web client's was captured).
fn trailing_slash(service: Service, endpoint: &Endpoint) -> bool {
    match service {
        Service::Presence => ups_slash(endpoint),
        Service::Mcps => true,
        Service::Ams => endpoint.segments() == ["v1", "objects"],
        _ => false,
    }
}

/// Whether a UPS path ends in a slash: every captured one did but the presence subscription's
/// (`v1/pubsub/subscriptions/<endpoint>`, [`crate::PresenceSubscription`]), which never did.
fn ups_slash(endpoint: &Endpoint) -> bool {
    !matches!(
        endpoint.segments(),
        [v1, pubsub, subscriptions, _] if v1 == "v1" && pubsub == "pubsub" && subscriptions == "subscriptions"
    )
}

/// Whether `url`'s query is exactly `query`, pair by pair in order.
fn query_is(url: &Url, query: &[(String, String)]) -> bool {
    url.query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .eq(query.iter().cloned())
}

/// The AMS base, if `base` is one [`HttpTransport::with_ams`] accepts.
fn parse_ams_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let host = url.host_str()?;
    let named = host.strip_suffix(AMS_HOST_SUFFIX).is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    });
    let plain = url.scheme() == "https"
        && named
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        // the base as it was given (with the slash `Url` keeps for an empty path), not one the parser normalized
        && url.as_str() == format!("{base}/");
    plain.then_some(url)
}

/// The presence base, if `base` is one [`HttpTransport::with_presence`] accepts.
fn parse_presence_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the base as it was given, not one the parser normalized
        && url.as_str() == base;
    let path = decoded_path(&url)?;
    let geo = matches!(
        path.as_slice(),
        [ups, geo] if ups == "ups" && !geo.is_empty() && geo.bytes().all(|byte| byte.is_ascii_lowercase())
    );
    (plain && geo).then_some(url)
}

/// The base `base` names when it is `https`, on an AFD host with no port or user info, its path as `path_is` wants, one
/// trailing slash allowed, no query or fragment.
fn parse_service_base(base: &str, path_is: fn(&[String]) -> bool) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the base as it was given, not one the parser normalized
        && url.as_str() == base;
    (plain && path_is(&decoded_path(&url)?)).then_some(url)
}

/// Whether a decoded base path is MCPS's, `/api/mcps/<region>` (the region lowercase letters).
fn is_mcps_path(path: &[String]) -> bool {
    matches!(
        path,
        [api, mcps, region] if api == "api"
            && mcps == "mcps"
            && !region.is_empty()
            && region.bytes().all(|byte| byte.is_ascii_lowercase())
    )
}

/// The CSA base, if `base` is one [`HttpTransport::with_csa`] accepts.
fn parse_csa_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the base as it was given, not one the parser normalized
        && url.as_str() == base;
    let path = decoded_path(&url)?;
    let region = matches!(
        path.as_slice(),
        [api, csa, region] if api == "api"
            && csa == "csa"
            && !region.is_empty()
            && region.bytes().all(|byte| byte.is_ascii_lowercase())
    );
    (plain && region).then_some(url)
}

/// The middle-tier base, if `base` is one [`HttpTransport::with_middle_tier`] accepts.
fn parse_middle_tier_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(base).ok()?;
    let plain = url.scheme() == "https"
        && url.host_str().is_some_and(|host| AFD_HOSTS.contains(&host))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        // the base as it was given, not one the parser normalized
        && url.as_str() == base;
    let path = decoded_path(&url)?;
    let partition = matches!(
        path.as_slice(),
        [api, mt, part, partition] if api == "api" && mt == "mt" && part == "part" && is_partition(partition)
    );
    (plain && partition).then_some(url)
}

/// A middle-tier partition: a lowercase region, `-`, a number (`amer-03`).
fn is_partition(partition: &str) -> bool {
    partition.split_once('-').is_some_and(|(region, number)| {
        !region.is_empty()
            && region.bytes().all(|byte| byte.is_ascii_lowercase())
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// The URL's path segments, percent-decoded; a bare `/` is none.
fn decoded_path(url: &Url) -> Option<Vec<String>> {
    let segments: Vec<String> = url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    Some(if segments == [""] { Vec::new() } else { segments })
}

/// The chat base, if `base` is exactly one [`chat_service_path`] accepts a request under (see [`HttpTransport::new`]).
fn parse_chat_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let base = base.strip_suffix('/').unwrap_or(base);
    let probe = Url::parse(&format!("{base}/v1")).ok()?;
    chat_service_path(&probe).filter(Vec::is_empty)?;
    Url::parse(base).ok()
}

/// A loopback `http` origin with a port and nothing after it.
#[cfg(feature = "test-support")]
fn parse_mock_origin(origin: &str) -> Option<Url> {
    let url = Url::parse(origin).ok()?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
    let bare = url.path() == "/" && url.query().is_none() && url.fragment().is_none() && url.username().is_empty();
    (url.scheme() == "http" && loopback && url.port().is_some() && bare && url.password().is_none()).then_some(url)
}

/// The largest body read for a request to `url`: [`MAX_PICTURE_BODY`] from AMS's async gateway and Giphy's media host,
/// [`MAX_SEGMENT_BODY`] from the media transform service, [`MAX_BODY`] from anywhere else.
fn max_body(url: &Url) -> usize {
    if url.host_str().is_some_and(crate::recording::is_media_host) {
        return MAX_SEGMENT_BODY;
    }
    if url
        .host_str()
        .is_some_and(|host| host.ends_with(AMS_HOST_SUFFIX) || host == GIPHY_HOST)
    {
        MAX_PICTURE_BODY
    } else {
        MAX_BODY
    }
}

fn request_path(url: &Url) -> String {
    const FOLDER: &str = "/root:/Microsoft%20Teams%20Chat%20Files/";
    url.path().split_once(FOLDER).map_or_else(
        || url.path().to_owned(),
        |(prefix, _)| format!("{prefix}{FOLDER}<file>:/content"),
    )
}

/// A request's description in the log at `info`: its method, host and path, never its query.
fn shown(outgoing: &Outgoing<'_>) -> String {
    format!(
        "{} {}{}",
        outgoing.method,
        outgoing.url.host_str().unwrap_or_default(),
        outgoing.url.path()
    )
}

/// `headers` (lower-cased names, none of them a credential) and the credential `auth` as a request's header map.
fn header_map(headers: &[(String, String)], auth: Auth<'_>) -> Result<HeaderMap, HttpError> {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| HttpError::Unknown)?;
        let value = HeaderValue::from_str(value).map_err(|_| HttpError::Unknown)?;
        map.append(name, value);
    }
    let credential = match auth {
        Auth::None => None,
        Auth::Bearer(token) => Some((AUTHORIZATION, format!("Bearer {}", token.expose()))),
        Auth::Skypetoken(token) => Some((HeaderName::from_static(SKYPETOKEN_HEADER), token.expose().to_owned())),
        Auth::Recorder { bearer, skype } => {
            let mut value = HeaderValue::from_str(skype.expose()).map_err(|_| HttpError::Refused(Refusal::Token))?;
            value.set_sensitive(true);
            map.insert(HeaderName::from_static(SKYPETOKEN_HEADER), value);
            Some((AUTHORIZATION, format!("Bearer {}", bearer.expose())))
        }
    };
    if let Some((name, value)) = credential {
        let mut value = HeaderValue::from_str(&value).map_err(|_| HttpError::Refused(Refusal::Token))?;
        value.set_sensitive(true);
        map.insert(name, value);
    }
    Ok(map)
}

/// Writes the request to the app's debug log (its credentials redacted by the logger) when that level is on, and says
/// whether it is.
fn log_request(method: &reqwest::Method, url: &Url, map: &HeaderMap, body: Option<&[u8]>) -> bool {
    let full = log::log_enabled!(log::Level::Debug);
    if full {
        let headers = header_pairs(map);
        let request = teams_log::wire::Request {
            method: method.as_str(),
            url: url.as_str(),
            headers: &headers,
            body,
        };
        log::debug!(target: teams_log::wire::HTTP_TARGET, "{request}");
    }
    full
}

/// Writes an answer to the app's debug log when `full` (its level on).
fn log_response(
    method: &reqwest::Method,
    url: &Url,
    answer: &Response,
    headers: &[(String, String)],
    elapsed: Duration,
    full: bool,
) {
    if full {
        let response = teams_log::wire::Response {
            method: method.as_str(),
            url: url.as_str(),
            status: answer.status(),
            headers,
            body: answer.body(),
            elapsed,
        };
        log::debug!(target: teams_log::wire::HTTP_TARGET, "{response}");
    }
}

/// Every header of `map` as text, for the log's `debug` records (which redact the credentials'): a value that isn't
/// text is taken lossily.
fn header_pairs(map: &HeaderMap) -> Vec<(String, String)> {
    map.iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

/// Reads an answer: its status, `Location`, `Retry-After`, `Content-Type`, `Content-Range`, and its body up to
/// [`MAX_BODY`]; with `all_headers`, every header too, for the log's `debug` record (none otherwise).
async fn read_answer(
    mut answer: reqwest::Response,
    all_headers: bool,
    max_body: usize,
) -> Result<(Response, Vec<(String, String)>), HttpError> {
    let headers = answer.headers();
    let kept = if all_headers { header_pairs(headers) } else { Vec::new() };
    let text = |name| {
        headers
            .get(name)
            .and_then(|value: &HeaderValue| value.to_str().ok())
            .map(str::to_owned)
    };
    let (location, content_type, content_range) = (text(LOCATION), text(CONTENT_TYPE), text(CONTENT_RANGE));
    let wait = retry_after(headers);
    let status = answer.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = answer.chunk().await.map_err(|error| failure(&error))? {
        if body.len() + chunk.len() > max_body {
            return Err(HttpError::BodyTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let mut response = Response::new(status, body);
    if let Some(location) = location {
        response = response.with_location(location);
    }
    if let Some(content_type) = content_type {
        response = response.with_content_type(content_type);
    }
    if let Some(content_range) = content_range {
        response = response.with_content_range(content_range);
    }
    if let Some(wait) = wait {
        response = response.with_retry_after(wait);
    }
    Ok((response, kept))
}

fn failure(error: &reqwest::Error) -> HttpError {
    HttpError::Failed(if error.is_timeout() {
        Failure::Timeout
    } else if error.is_body() || error.is_decode() {
        Failure::Body
    } else {
        Failure::Network
    })
}

#[cfg(test)]
#[path = "http/attachment_tests.rs"]
mod attachment_tests;
