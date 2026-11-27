//! Requests the server makes to an address an administrator or a team set: a bot's messaging endpoint, an outgoing
//! webhook's callback URL. The address guard
//! (server-side request forgery) holds for every one:
//!
//! - `https` only, unless `bots.allow_http_endpoints`; no user info;
//! - public addresses only, unless an address is in `bots.allowed_networks` (a self-hosted bot on a private network);
//!   a cloud metadata address is never reached, whatever the list says;
//! - the connection goes only to the addresses the guard's own resolver passed, so a name can't answer one way for a
//!   check and another for the connection (DNS rebinding); an address written in the URL is checked the same way;
//! - no redirects, no proxy, a deadline for the whole exchange and a cap on the answer's body.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ipnet::IpNet;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// The most of an answer's body read.
pub const MAX_ANSWER: usize = 256 * 1024;
/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a request wasn't made or had no answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OutboundError {
    #[error("the address isn't one the server sends to")]
    Url,
    #[error("the address is on a network the server doesn't reach")]
    Refused,
    #[error("the address is a cloud metadata address, which the server never sends to")]
    Metadata,
    #[error("the name did not resolve")]
    Unresolved,
    #[error("the connection was refused")]
    ConnectionRefused,
    #[error("the bot's host is not reachable")]
    Unreachable,
    #[error("the connection was closed")]
    Closed,
    #[error("the bot's TLS certificate is not trusted")]
    Certificate,
    #[error("the TLS handshake failed")]
    Tls,
    #[error("the request timed out")]
    Timeout,
    #[error("the answer is bigger than the server reads")]
    TooLarge,
    #[error("the request failed")]
    Failed,
}

/// Which addresses may be reached.
#[derive(Clone, Debug, Default)]
pub struct Guard {
    pub allow_http: bool,
    pub networks: Vec<IpNet>,
}

impl Guard {
    /// The guard of the sends to bots, as `bots.*` sets it; one for the check at save and at each send.
    #[must_use]
    pub fn for_bots(config: &crate::apps::BotsConfig) -> Self {
        Self {
            allow_http: config.allow_http_endpoints,
            networks: config.allowed_networks.clone(),
        }
    }

    /// Whether the server may connect to `address`.
    #[must_use]
    pub fn allows(&self, address: IpAddr) -> bool {
        let address = canonical(address);
        if is_metadata(address) {
            return false;
        }
        crate::media::is_public(address) || self.networks.iter().any(|network| network.contains(&address))
    }

    /// Refuses `url` unless its scheme and form may be sent to and an address written in it is allowed.
    ///
    /// # Errors
    /// [`OutboundError::Url`], [`OutboundError::Metadata`] or [`OutboundError::Refused`].
    pub fn check(&self, url: &Url) -> Result<(), OutboundError> {
        let scheme = url.scheme() == "https" || (self.allow_http && url.scheme() == "http");
        if !scheme || !url.username().is_empty() || url.password().is_some() {
            return Err(OutboundError::Url);
        }
        let literal = match url.host().ok_or(OutboundError::Url)? {
            Host::Ipv4(address) => Some(IpAddr::V4(address)),
            Host::Ipv6(address) => Some(IpAddr::V6(address)),
            Host::Domain(_) => None,
        };
        match literal {
            Some(address) if is_metadata(canonical(address)) => Err(OutboundError::Metadata),
            Some(address) if !self.allows(address) => Err(OutboundError::Refused),
            _ => Ok(()),
        }
    }
}

/// An IPv4-mapped IPv6 address as its IPv4 address, so a network list of IPv4 ranges holds it.
fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        IpAddr::V4(_) => address,
    }
}

/// The cloud providers' instance metadata services, also in any IPv6 form that carries their IPv4 address.
fn is_metadata(address: IpAddr) -> bool {
    const AWS_V6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
    let v4 = match address {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(v6) => crate::media::embedded_v4(v6),
    };
    v4.is_some_and(|v4| v4 == Ipv4Addr::new(169, 254, 169, 254) || v4 == Ipv4Addr::new(100, 100, 100, 200))
        || address == IpAddr::V6(AWS_V6)
}

struct Guarded(Guard);

impl Resolve for Guarded {
    fn resolve(&self, name: Name) -> Resolving {
        let guard = self.0.clone();
        Box::pin(async move {
            let failed = |error: OutboundError| Box::new(error) as Box<dyn std::error::Error + Send + Sync>;
            let found: Vec<SocketAddr> = tokio::net::lookup_host((name.as_str(), 0))
                .await
                .map_err(|_| failed(OutboundError::Unresolved))?
                .collect();
            if found.is_empty() {
                return Err(failed(OutboundError::Unresolved));
            }
            let addresses: Vec<SocketAddr> = found.into_iter().filter(|address| guard.allows(address.ip())).collect();
            if addresses.is_empty() {
                return Err(failed(OutboundError::Refused));
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

/// The client for requests to bots.
#[derive(Clone, Debug)]
pub struct Outbound {
    client: reqwest::Client,
    guard: Arc<Guard>,
    deadline: Duration,
}

/// An answer: its status and at most [`MAX_ANSWER`] bytes of its body.
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub body: Bytes,
}

impl Outbound {
    /// # Errors
    /// The HTTP client can't be built.
    pub fn new(guard: Guard, deadline: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT.min(deadline))
            .dns_resolver(Arc::new(Guarded(guard.clone())))
            .user_agent("ngteams-server bot connector")
            .build()?;
        Ok(Self {
            client,
            guard: Arc::new(guard),
            deadline,
        })
    }

    /// Whether `url` may be sent to, as far as the URL alone tells: its scheme, its form and an address written in
    /// it. A name is checked when it is resolved.
    ///
    /// # Errors
    /// [`OutboundError::Url`] or [`OutboundError::Refused`].
    pub fn check(&self, url: &Url) -> Result<(), OutboundError> {
        self.guard.check(url)
    }

    /// Posts `body` as JSON to `url`, with `bearer` when given.
    ///
    /// # Errors
    /// See [`OutboundError`].
    pub async fn post_json(&self, url: &str, bearer: Option<&str>, body: Vec<u8>) -> Result<Answer, OutboundError> {
        let authorization = bearer.map(|bearer| format!("Bearer {bearer}"));
        self.post(url, authorization.as_deref(), body, self.deadline).await
    }

    /// Posts `body` as JSON to `url` with `authorization` as its `Authorization` (an outgoing webhook's HMAC), and
    /// `deadline` for the whole exchange.
    ///
    /// # Errors
    /// See [`OutboundError`].
    pub async fn post_signed(
        &self,
        url: &str,
        authorization: &str,
        body: Vec<u8>,
        deadline: Duration,
    ) -> Result<Answer, OutboundError> {
        self.post(url, Some(authorization), body, deadline).await
    }

    async fn post(
        &self,
        url: &str,
        authorization: Option<&str>,
        body: Vec<u8>,
        deadline: Duration,
    ) -> Result<Answer, OutboundError> {
        let url = Url::parse(url).map_err(|_| OutboundError::Url)?;
        self.guard.check(&url)?;
        let mut request = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(body);
        if let Some(authorization) = authorization {
            let value = sensitive_authorization(authorization)?;
            request = request.header(reqwest::header::AUTHORIZATION, value);
        }
        let exchange = async {
            let mut answer = request.send().await.map_err(|error| refusal(&error))?;
            let status = answer.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = answer.chunk().await.map_err(|_| OutboundError::Failed)? {
                if body.len() + chunk.len() > MAX_ANSWER {
                    return Err(OutboundError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Answer {
                status,
                body: body.into(),
            })
        };
        tokio::time::timeout(deadline, exchange)
            .await
            .map_err(|_| OutboundError::Timeout)?
    }
}

fn sensitive_authorization(authorization: &str) -> Result<reqwest::header::HeaderValue, OutboundError> {
    let mut value = reqwest::header::HeaderValue::from_str(authorization).map_err(|_| OutboundError::Failed)?;
    value.set_sensitive(true);
    Ok(value)
}

/// What a failed request was, in words for a person: the guard's resolver, the connection, TLS or a timeout.
fn refusal(error: &reqwest::Error) -> OutboundError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(cause) = source {
        if let Some(known) = cause.downcast_ref::<OutboundError>() {
            return known.clone();
        }
        if let Some(found) = cause.downcast_ref::<rustls::Error>().map(tls) {
            return found;
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if let Some(found) = connection(io.kind()) {
                return found;
            }
            // an io::Error's source() skips its own inner error, where the TLS layer puts the rustls error
            if let Some(inner) = io.get_ref() {
                source = Some(inner);
                continue;
            }
        }
        source = cause.source();
    }
    if error.is_timeout() {
        OutboundError::Timeout
    } else {
        OutboundError::Failed
    }
}

fn tls(error: &rustls::Error) -> OutboundError {
    match error {
        rustls::Error::InvalidCertificate(_) => OutboundError::Certificate,
        _ => OutboundError::Tls,
    }
}

fn connection(kind: std::io::ErrorKind) -> Option<OutboundError> {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::ConnectionRefused => Some(OutboundError::ConnectionRefused),
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable | ErrorKind::AddrNotAvailable => {
            Some(OutboundError::Unreachable)
        }
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::UnexpectedEof => {
            Some(OutboundError::Closed)
        }
        ErrorKind::TimedOut => Some(OutboundError::Timeout),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_and_hmac_headers_are_sensitive() {
        for text in ["Bearer synthetic-token", "HMAC synthetic-signature"] {
            let value = sensitive_authorization(text).unwrap();
            assert!(value.is_sensitive());
            assert_eq!(value.to_str().unwrap(), text);
            assert!(!format!("{value:?}").contains(text));
        }
        assert!(sensitive_authorization("Bearer invalid\nheader").is_err());
    }

    fn guard(networks: &[&str]) -> Guard {
        Guard {
            allow_http: true,
            networks: networks.iter().map(|network| network.parse().unwrap()).collect(),
        }
    }

    #[test]
    fn private_loopback_and_link_local_addresses_pass_only_when_listed_and_metadata_never() {
        let closed = guard(&[]);
        let open = guard(&[
            "10.0.0.0/8",
            "127.0.0.0/8",
            "169.254.0.0/16",
            "fd00::/8",
            "100.64.0.0/10",
            "::/96",
            "64:ff9b::/96",
            "64:ff9b:1::/48",
            "2002::/16",
        ]);
        for address in [
            "10.1.2.3",
            "127.0.0.1",
            "169.254.10.1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "100.100.1.1",
        ] {
            let address: IpAddr = address.parse().unwrap();
            assert!(!closed.allows(address), "{address}");
            assert!(open.allows(address), "{address}");
        }
        for address in [
            "169.254.169.254",
            "::ffff:169.254.169.254",
            "::169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::a9fe:a9fe",
            "2002:a9fe:a9fe::1",
            "fd00:ec2::254",
            "100.100.100.200",
            "64:ff9b::6464:64c8",
        ] {
            let address: IpAddr = address.parse().unwrap();
            assert!(!open.allows(address), "{address}");
        }
        for address in [
            "192.168.1.1",
            "172.16.0.1",
            "0.0.0.0",
            "::",
            "::1",
            "224.0.0.1",
            "fe80::1",
            "64:ff9b:1::808:808",
        ] {
            assert!(!closed.allows(address.parse().unwrap()), "{address}");
        }
        assert!(closed.allows("93.184.216.34".parse().unwrap()));
        assert!(closed.allows("2606:4700::1111".parse().unwrap()));
        assert!(open.allows("64:ff9b::a00:1".parse().unwrap()));
    }

    #[test]
    fn a_url_is_checked_for_its_scheme_credentials_and_written_address() {
        let https = Guard::default();
        let check = |guard: &Guard, url: &str| guard.check(&Url::parse(url).unwrap());
        assert_eq!(check(&https, "http://bot.example.test/"), Err(OutboundError::Url));
        assert_eq!(
            check(&https, "https://user:pw@bot.example.test/"),
            Err(OutboundError::Url)
        );
        assert_eq!(check(&https, "ftp://bot.example.test/"), Err(OutboundError::Url));
        assert_eq!(check(&https, "https://127.0.0.1/"), Err(OutboundError::Refused));
        assert_eq!(check(&https, "https://[::ffff:10.0.0.1]/"), Err(OutboundError::Refused));
        assert_eq!(check(&https, "https://[fd00:ec2::254]/"), Err(OutboundError::Metadata));
        assert_eq!(
            check(&https, "https://169.254.169.254/latest"),
            Err(OutboundError::Metadata)
        );
        assert_eq!(
            check(&guard(&["169.254.0.0/16"]), "https://[::ffff:169.254.169.254]/"),
            Err(OutboundError::Metadata)
        );
        assert_eq!(check(&https, "https://169.254.10.10/"), Err(OutboundError::Refused));
        assert_eq!(check(&https, "https://bot.example.test/api/messages"), Ok(()));
        assert_eq!(check(&guard(&["127.0.0.0/8"]), "http://127.0.0.1:3978/"), Ok(()));
    }

    #[tokio::test]
    async fn a_name_that_resolves_only_to_refused_addresses_is_never_connected() {
        let outbound = Outbound::new(Guard::default(), Duration::from_secs(5)).unwrap();
        let refused = outbound
            .post_json("https://localhost:9/api/messages", None, b"{}".to_vec())
            .await;
        assert_eq!(refused.unwrap_err(), OutboundError::Refused);
    }

    #[tokio::test]
    async fn a_failed_connection_says_why_in_words_for_a_person() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let plain = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let plain_port = plain.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut stream, _)) = plain.accept().await {
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            }
        });
        let outbound = Outbound::new(guard(&["127.0.0.0/8"]), Duration::from_secs(5)).unwrap();
        let failure = |url: String| {
            let outbound = outbound.clone();
            async move { outbound.post_json(&url, None, b"{}".to_vec()).await.unwrap_err() }
        };
        let refused = failure(format!("http://127.0.0.1:{closed}/")).await;
        assert_eq!(refused, OutboundError::ConnectionRefused);
        assert_eq!(refused.to_string(), "the connection was refused");
        let tls = failure(format!("https://127.0.0.1:{plain_port}/")).await;
        assert_eq!(tls, OutboundError::Tls);
        assert_eq!(tls.to_string(), "the TLS handshake failed");
        let unresolved = failure("https://bot.ngteams.invalid/".to_owned()).await;
        assert_eq!(unresolved, OutboundError::Unresolved);
        assert_eq!(unresolved.to_string(), "the name did not resolve");
        task.abort();
    }

    #[test]
    fn a_tls_error_and_an_io_kind_have_their_own_words() {
        use std::io::ErrorKind;
        let untrusted = rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer);
        assert_eq!(tls(&untrusted), OutboundError::Certificate);
        assert_eq!(tls(&rustls::Error::DecryptError), OutboundError::Tls);
        assert_eq!(connection(ErrorKind::HostUnreachable), Some(OutboundError::Unreachable));
        assert_eq!(connection(ErrorKind::ConnectionReset), Some(OutboundError::Closed));
        assert_eq!(connection(ErrorKind::TimedOut), Some(OutboundError::Timeout));
        assert_eq!(connection(ErrorKind::Other), None);
    }

    #[tokio::test]
    async fn redirects_are_not_followed_and_long_answers_are_cut_off() {
        use axum::Router;
        use axum::http::StatusCode;
        use axum::routing::post;
        let router = Router::new()
            .route(
                "/redirect",
                post(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [("location", "http://169.254.169.254/")],
                    )
                }),
            )
            .route("/large", post(|| async { vec![b'x'; MAX_ANSWER + 1] }))
            .route(
                "/slow",
                post(|| async { tokio::time::sleep(Duration::from_secs(5)).await }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let outbound = Outbound::new(guard(&["127.0.0.0/8"]), Duration::from_millis(500)).unwrap();
        let answer = outbound
            .post_json(&format!("{origin}/redirect"), Some("t"), b"{}".to_vec())
            .await
            .unwrap();
        assert_eq!(answer.status, 307);
        let large = outbound.post_json(&format!("{origin}/large"), None, Vec::new()).await;
        assert_eq!(large.unwrap_err(), OutboundError::TooLarge);
        let slow = outbound.post_json(&format!("{origin}/slow"), None, Vec::new()).await;
        assert_eq!(slow.unwrap_err(), OutboundError::Timeout);
        task.abort();
    }
}
