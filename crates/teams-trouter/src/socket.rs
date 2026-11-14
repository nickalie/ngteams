//! The socket: where it connects (Microsoft's host allowlist or the account's own origin), how (WSS, the system's
//! certificate verifier), and its two halves. The write half is wrapped in [`SocketWriter`], the [`FrameWriter`] that only `teams_proto`'s
//! [`FrameSink`](teams_proto::FrameSink) can feed (it takes nothing but the sink's [`FrameText`]), and it is handed to
//! the sink at once; nothing else in this crate holds it, so the socket has no other way to send a frame. The read
//! half only reads.
//!
//! What the WebSocket layer itself sends on the socket (the masked handshake request, a pong to a WebSocket ping, the
//! reply to a close) carries no data of ours and reaches no one: those are the protocol's own control frames.
//!
//! **No frame reaches a log unredacted.** tungstenite logs every frame it sends and reads at `trace` level, its
//! payload included (`Sending frame: …`, `writing frame …`, `Received message …`), and `user.authenticate` carries the
//! bearer token. This crate takes `log` with `max_level_debug` and `release_max_level_debug` (`Cargo.toml`), so those
//! `trace!` calls compile to nothing in any build with this crate in it, whatever logger a binary installs. At `debug`
//! the write half logs each frame it sends itself, as a `teams_log::wire::Frame`, whose formatting redacts
//! `user.authenticate`'s arguments and every credential field (M2 Task 24a: the app's diagnostic log); the session
//! logs each frame it reads the same way. rustls logs only the handshake's metadata, and hyper and h2 log through
//! `tracing` without its `log` feature (h2's frame `Debug` leaves the header fields out on purpose).

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use teams_proto::Cloud;
use teams_proto::{CLIENT_BUILD, FrameText, FrameWriter, TROUTER_DOMAIN, TROUTER_HOST};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, crypto};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{self, Message};
use url::Url;

/// The connect path on every Trouter host.
pub const CONNECT_PATH: &str = "/v4/c";

/// The ping interval the connect names (`timeout`), in seconds, as the web client's.
pub const PING_SECONDS: u64 = 40;

/// The largest frame the client reads, and the largest message.
pub const MAX_INCOMING: usize = 4 * 1024 * 1024;

/// The most the write half buffers before a write fails: frames are small (an authentication is the largest).
const MAX_WRITE_BUFFER: usize = 256 * 1024;

/// The client version the connect names in `tc` (`cv`), the web client's.
const CLIENT_VERSION: &str = "2026.36.01.1";

/// The `Origin` the web client's handshake carried.
const ORIGIN: &str = "https://teams.microsoft.com";

/// The domain the calling stack's connect names (`dom`): the `Origin`'s host, as the web client's carried it.
const CALLING_DOMAIN: &str = "teams.microsoft.com";

/// Which of the web client's two stacks a socket is: each opens its own socket with its own endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stack {
    /// The messaging stack's (`TeamsCDL`): chat deliveries.
    Messaging,
    /// The calling stack's (`SkypeSpaces`): call deliveries.
    Calling,
}

impl Stack {
    /// The client `tc` names (`ua`).
    fn client(self) -> &'static str {
        match self {
            Self::Messaging => "TeamsCDL",
            Self::Calling => "SkypeSpaces",
        }
    }
}

/// What the web client percent-encoded in `tc`: quotes and braces (and what a query can't carry); `:`, `,` and `/`
/// stayed as they were.
const TC_ENCODED: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'{')
    .add(b'}')
    .add(b'%')
    .add(b'#')
    .add(b'&')
    .add(b'+')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`');

/// A stream the socket runs over: TLS to the service, or (tests only) plain TCP to a loopback mock.
pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub(crate) type Stream = WebSocketStream<Box<dyn Io>>;

/// Where a socket connects: the global host, or a regional one a `trouter.connected` named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectBase(Url);

impl ConnectBase {
    /// The cloud's global connect URL: `wss://go.trouter.teams.microsoft.com/v4/c` for Microsoft,
    /// or `wss://<own origin>/trouter/v4/c`.
    pub(crate) fn global_in(cloud: &Cloud) -> Option<Self> {
        let mut url = cloud
            .service_url(&format!("https://{TROUTER_HOST}{CONNECT_PATH}"), "/trouter/v4/c")
            .ok()?;
        url.set_scheme("wss").ok()?;
        Some(Self(url))
    }

    /// A `reconnectUrl` as `trouter.connected` handed it over, if it is on the allowlist ([`allowed`]); its port `443`
    /// is dropped, as the web client's connect URL dropped it.
    pub(crate) fn regional(reconnect_url: &str) -> Option<Self> {
        let url = Url::parse(reconnect_url).ok()?;
        let regional = url
            .host_str()
            .and_then(|host| host.strip_suffix(TROUTER_DOMAIN))
            .and_then(|name| name.strip_suffix("-t"))
            .is_some_and(|name| {
                !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
        let plain = url.scheme() == "wss"
            && url.port().is_none_or(|port| port == 443)
            && url.path() == CONNECT_PATH
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none();
        (regional && plain).then_some(Self(url))
    }

    pub(crate) fn regional_in(cloud: &Cloud, reconnect_url: &str) -> Option<Self> {
        if cloud.is_microsoft() {
            return Self::regional(reconnect_url);
        }
        let url = Url::parse(reconnect_url).ok()?;
        (allowed_in(cloud, &url) && url.query().is_none()).then_some(Self(url))
    }

    /// The connect URL of `stack`'s socket: the base and the web client's query (`tc`, `timeout`, `epid`, the calling
    /// stack's `dom`, `cor_id`, `con_num`), `tc` naming the stack's client.
    pub(crate) fn connect_url(&self, stack: Stack, epid: &str, cor_id: &str, con_num: &str) -> Url {
        let client = stack.client();
        let tc = format!(r#"{{"cv":"{CLIENT_VERSION}","ua":"{client}","hr":"","v":"{CLIENT_BUILD}"}}"#);
        let dom = match stack {
            Stack::Messaging => String::new(),
            Stack::Calling => {
                let domain = if self.0.path() == CONNECT_PATH {
                    CALLING_DOMAIN
                } else {
                    self.0.host_str().unwrap_or_default()
                };
                format!("&dom={domain}")
            }
        };
        let query = format!(
            "tc={}&timeout={PING_SECONDS}&epid={epid}{dom}&cor_id={cor_id}&con_num={con_num}",
            utf8_percent_encode(&tc, TC_ENCODED)
        );
        let mut url = self.0.clone();
        url.set_query(Some(&query));
        url
    }
}

/// Whether `url` is a connect URL the client may open: `wss`, the global host ([`TROUTER_HOST`]) or a regional
/// `<name>-t` one under [`TROUTER_DOMAIN`], no port but `443`, the path [`CONNECT_PATH`], no user info or fragment.
#[must_use]
pub fn allowed(url: &Url) -> bool {
    let mut base = url.clone();
    base.set_query(None);
    let host_ok = url.host_str() == Some(TROUTER_HOST) || ConnectBase::regional(base.as_str()).is_some();
    host_ok
        && url.scheme() == "wss"
        && url.port().is_none_or(|port| port == 443)
        && url.path() == CONNECT_PATH
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

/// Whether a socket URL belongs to this account's cloud.
#[must_use]
pub fn allowed_in(cloud: &Cloud, url: &Url) -> bool {
    if cloud.is_microsoft() {
        return allowed(url);
    }
    if url.scheme() != "wss" || url.path() != "/trouter/v4/c" {
        return false;
    }
    let mut https = url.clone();
    if https.set_scheme("https").is_err() {
        return false;
    }
    cloud.contains(&https)
}

/// Why a socket couldn't be opened. Nothing in it carries a token (the handshake carries none) or a body.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("the connect URL isn't on the allowlist")]
    NotAllowed,
    #[error("connecting timed out")]
    Timeout,
    #[error("the network failed: {0:?}")]
    Network(io::ErrorKind),
    #[error("the TLS handshake failed")]
    Tls,
    #[error("the WebSocket handshake was refused (HTTP {0:?})")]
    Refused(Option<u16>),
}

/// The TLS client config: the system's certificate verifier on `aws-lc-rs`, as the HTTP transport's (reqwest's).
///
/// # Errors
/// When the system verifier can't be set up.
pub(crate) fn tls_config() -> Result<Arc<ClientConfig>, tokio_rustls::rustls::Error> {
    let provider = Arc::new(crypto::aws_lc_rs::default_provider());
    let verifier = rustls_platform_verifier::Verifier::new(Arc::clone(&provider))?;
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Every URL a client on a mock origin was asked to open, as it was built and checked (before the mock took its
/// path and query), in order. Under a mock origin the handshake carries the mock's address, so this is how a test sees
/// which host a connect chose. Tests only (`test-support`).
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, Default)]
pub struct OpenedUrls(Arc<std::sync::Mutex<Vec<Url>>>);

#[cfg(feature = "test-support")]
impl OpenedUrls {
    /// The URLs asked for so far, oldest first.
    #[must_use]
    pub fn urls(&self) -> Vec<Url> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn push(&self, url: &Url) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(url.clone());
    }
}

/// How a socket is opened: TLS to the URL's host, or (`test-support` only) plain TCP to a loopback mock.
#[derive(Clone)]
pub(crate) struct Opener {
    tls: Arc<ClientConfig>,
    cloud: Cloud,
    #[cfg(feature = "test-support")]
    mock_origin: Option<Url>,
    #[cfg(feature = "test-support")]
    opened: OpenedUrls,
}

impl Opener {
    pub(crate) fn new(tls: Arc<ClientConfig>) -> Self {
        Self {
            tls,
            cloud: Cloud::Microsoft,
            #[cfg(feature = "test-support")]
            mock_origin: None,
            #[cfg(feature = "test-support")]
            opened: OpenedUrls::default(),
        }
    }

    pub(crate) fn for_cloud(mut self, cloud: Cloud) -> Self {
        self.cloud = cloud;
        self
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn with_mock_origin(mut self, origin: Url) -> Self {
        self.mock_origin = Some(origin);
        self
    }

    /// The URLs this opener was asked to open on its mock origin.
    #[cfg(feature = "test-support")]
    pub(crate) fn opened(&self) -> OpenedUrls {
        self.opened.clone()
    }

    /// Opens the socket at `url` (checked against the allowlist first) within `limit`: TCP, TLS, the WebSocket
    /// handshake with the cloud's `Origin` (the web client's for Microsoft, the own origin otherwise).
    pub(crate) async fn open(&self, url: &Url, limit: Duration) -> Result<Stream, ConnectError> {
        if !allowed_in(&self.cloud, url) {
            return Err(ConnectError::NotAllowed);
        }
        tokio::time::timeout(limit, self.open_checked(url))
            .await
            .map_err(|_| ConnectError::Timeout)?
    }

    async fn open_checked(&self, url: &Url) -> Result<Stream, ConnectError> {
        #[cfg(feature = "test-support")]
        if let Some(origin) = &self.mock_origin {
            self.opened.push(url);
            let mut mocked = origin.clone();
            mocked.set_path(url.path());
            mocked.set_query(url.query());
            let address = (origin.host_str().unwrap_or_default(), origin.port().unwrap_or_default());
            let tcp = TcpStream::connect(address).await.map_err(|error| network(&error))?;
            return handshake(mocked, Box::new(tcp), &self.cloud).await;
        }
        let host = url
            .host_str()
            .ok_or(ConnectError::NotAllowed)?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let tcp = TcpStream::connect((host.as_str(), url.port().unwrap_or(443)))
            .await
            .map_err(|error| network(&error))?;
        let name = ServerName::try_from(host).map_err(|_| ConnectError::NotAllowed)?;
        let tls = TlsConnector::from(Arc::clone(&self.tls))
            .connect(name, tcp)
            .await
            .map_err(|_| ConnectError::Tls)?;
        handshake(url.clone(), Box::new(tls), &self.cloud).await
    }
}

fn network(error: &io::Error) -> ConnectError {
    ConnectError::Network(error.kind())
}

async fn handshake(url: Url, stream: Box<dyn Io>, cloud: &Cloud) -> Result<Stream, ConnectError> {
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| ConnectError::NotAllowed)?;
    let origin = match cloud.origin() {
        None => HeaderValue::from_static(ORIGIN),
        Some(origin) => {
            HeaderValue::from_str(origin.as_str().trim_end_matches('/')).map_err(|_| ConnectError::NotAllowed)?
        }
    };
    request.headers_mut().insert("origin", origin);
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_INCOMING))
        .max_frame_size(Some(MAX_INCOMING))
        .max_write_buffer_size(MAX_WRITE_BUFFER);
    let (socket, _response) = tokio_tungstenite::client_async_with_config(request, stream, Some(config))
        .await
        .map_err(|error| match error {
            tungstenite::Error::Http(response) => ConnectError::Refused(Some(response.status().as_u16())),
            tungstenite::Error::Io(error) => network(&error),
            _ => ConnectError::Refused(None),
        })?;
    Ok(socket)
}

/// Splits an open socket: the write half as the [`FrameWriter`] for the sink, the read half to read from.
pub(crate) fn split(socket: Stream) -> (SocketWriter, SplitStream<Stream>) {
    let (write, read) = socket.split();
    (SocketWriter(write), read)
}

/// The socket's write half, as the frame sink uses it: it sends the sink's [`FrameText`] as one text frame, and
/// nothing else.
pub(crate) struct SocketWriter(SplitSink<Stream, Message>);

/// Why the socket couldn't send a frame. Never the frame: tungstenite's `WriteBufferFull` hands the unsent frame
/// back, and it is dropped here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WriteFailed {
    #[error("the socket is closed")]
    Closed,
    #[error("the socket's write buffer is full")]
    BufferFull,
    #[error("the network failed: {0:?}")]
    Network(io::ErrorKind),
    #[error("the WebSocket layer failed")]
    Protocol,
}

impl From<tungstenite::Error> for WriteFailed {
    fn from(error: tungstenite::Error) -> Self {
        match error {
            tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => Self::Closed,
            tungstenite::Error::WriteBufferFull(_) => Self::BufferFull,
            tungstenite::Error::Io(error) => Self::Network(error.kind()),
            _ => Self::Protocol,
        }
    }
}

impl FrameWriter for SocketWriter {
    type Error = WriteFailed;

    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), WriteFailed>> + Send {
        let text = text.into_string();
        // the frame in full at `debug`, `user.authenticate` included: its record redacts the arguments
        log::debug!(
            target: teams_log::wire::FRAME_TARGET,
            "{}",
            teams_log::wire::Frame {
                direction: teams_log::wire::Direction::Sent,
                text: &text,
            }
        );
        let message = Message::text(text);
        async move { self.0.send(message).await.map_err(WriteFailed::from) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_connect_and_regional_urls_stay_on_origin() {
        let cloud = Cloud::own("https://chat.example:8443").unwrap();
        let global = ConnectBase::global_in(&cloud).unwrap();
        assert_eq!(global.0.as_str(), "wss://chat.example:8443/trouter/v4/c");
        let url = global.connect_url(Stack::Calling, "endpoint", "correlation", "connection");
        assert!(
            url.query_pairs()
                .any(|(key, value)| key == "dom" && value == "chat.example")
        );
        assert!(allowed_in(&cloud, &url));
        assert!(!allowed(&url));
        assert!(ConnectBase::regional_in(&cloud, global.0.as_str()).is_some());
        for link in [
            "wss://go.trouter.teams.microsoft.com/v4/c",
            "wss://name-t.trouter.teams.microsoft.com/v4/c",
            "wss://chat.example/trouter/v4/c",
            "ws://chat.example:8443/trouter/v4/c",
            "wss://chat.example:8443/v4/c",
            "wss://chat.example:8443/trouter/v4/c?token=x",
            "wss://u@chat.example:8443/trouter/v4/c",
            "wss://chat.example:8443/trouter/v4/c#x",
        ] {
            assert!(ConnectBase::regional_in(&cloud, link).is_none(), "{link}");
        }
        assert_eq!(
            ConnectBase::global_in(&Cloud::Microsoft).unwrap().0.as_str(),
            "wss://go.trouter.teams.microsoft.com/v4/c"
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    #[expect(
        clippy::result_large_err,
        reason = "tungstenite fixes the server handshake callback's error type"
    )]
    async fn own_handshake_uses_own_origin_and_refuses_off_origin_before_connecting() {
        use tokio::net::TcpListener;
        use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock = Url::parse(&format!("ws://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let socket = tokio_tungstenite::accept_hdr_async(stream, |request: &Request, response: Response| {
                assert_eq!(request.uri().path(), "/trouter/v4/c");
                assert_eq!(request.headers()["origin"], "https://chat.example:8443");
                Ok(response)
            })
            .await
            .unwrap();
            drop(socket);
        });
        let cloud = Cloud::own("https://chat.example:8443").unwrap();
        let opener = Opener::new(tls_config().unwrap())
            .for_cloud(cloud.clone())
            .with_mock_origin(mock);
        let own = ConnectBase::global_in(&cloud).unwrap();
        let socket = opener.open(&own.0, Duration::from_secs(5)).await.unwrap();
        drop(socket);
        server.await.unwrap();
        let microsoft = ConnectBase::global_in(&Cloud::Microsoft).unwrap();
        assert!(matches!(
            opener.open(&microsoft.0, Duration::from_secs(5)).await,
            Err(ConnectError::NotAllowed)
        ));
        assert_eq!(opener.opened().urls(), vec![own.0]);
    }
}
