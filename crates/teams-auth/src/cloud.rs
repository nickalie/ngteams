//! Account service boundaries. An own-server account has one HTTPS origin for every credentialed service, and may
//! trust its deployment's own certificate authority beside the system's (a self-signed deployment's `ca_file`).

use std::sync::Arc;
use std::time::Duration;

use url::Url;

/// How long a sign-in client waits for an own server's connection: a host that is off or out of reach fails a start
/// fast, before anything is sent.
pub const OWN_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The services an account belongs to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cloud {
    #[default]
    Microsoft,
    Own {
        origin: OwnOrigin,
    },
}

/// A validated HTTPS origin without a path, user info, query or fragment.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OwnOrigin {
    url: Url,
    /// The deployment's certificate authorities (PEM), trusted beside the system's for this origin's connections.
    roots: Option<Arc<[u8]>>,
}

impl std::fmt::Debug for OwnOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnOrigin")
            .field("url", &self.url.as_str())
            .field("roots", &self.roots.is_some())
            .finish()
    }
}

/// The certificate authority file holds no PEM certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the certificate authority file holds no PEM certificate")]
pub struct InvalidRoots;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("an own server needs an HTTPS origin without user info, path, query or fragment")]
pub struct InvalidOrigin;

impl Cloud {
    /// Selects an own server. An explicit TLS port is allowed.
    ///
    /// # Errors
    /// Returns [`InvalidOrigin`] unless `origin` is a bare HTTPS origin.
    pub fn own(origin: &str) -> Result<Self, InvalidOrigin> {
        let url = Url::parse(origin).map_err(|_| InvalidOrigin)?;
        let bare = url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && (url.as_str() == origin || url.as_str() == format!("{origin}/"));
        if bare {
            Ok(Self::Own {
                origin: OwnOrigin { url, roots: None },
            })
        } else {
            Err(InvalidOrigin)
        }
    }

    #[must_use]
    pub const fn is_microsoft(&self) -> bool {
        matches!(self, Self::Microsoft)
    }

    #[must_use]
    pub fn origin(&self) -> Option<&Url> {
        match self {
            Self::Microsoft => None,
            Self::Own { origin } => Some(&origin.url),
        }
    }

    /// The same cloud trusting the certificate authorities in `pem` beside the system's, on its own origin's
    /// connections only. Microsoft's cloud never takes one.
    ///
    /// # Errors
    /// [`InvalidRoots`] unless `pem` holds at least one certificate and every one of them parses.
    pub fn trusting(self, pem: &[u8]) -> Result<Self, InvalidRoots> {
        match self {
            Self::Microsoft => Err(InvalidRoots),
            Self::Own { origin } => {
                let parsed = reqwest::Certificate::from_pem_bundle(pem).map_err(|_| InvalidRoots)?;
                if parsed.is_empty() {
                    return Err(InvalidRoots);
                }
                Ok(Self::Own {
                    origin: OwnOrigin {
                        url: origin.url,
                        roots: Some(Arc::from(pem)),
                    },
                })
            }
        }
    }

    /// The extra certificate authorities this cloud trusts (PEM), if any ([`Cloud::trusting`]).
    #[must_use]
    pub fn extra_roots(&self) -> Option<&[u8]> {
        match self {
            Self::Microsoft => None,
            Self::Own { origin } => origin.roots.as_deref(),
        }
    }

    /// `builder` trusting this cloud's extra certificate authorities beside the system's ([`crate::tls`]).
    pub fn trust(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        if self.extra_roots().is_none() {
            return builder;
        }
        // checked when they were taken ([`Cloud::trusting`]): one that no longer parses adds nothing
        match crate::tls::client_config(self) {
            Ok(mut config) => {
                config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
                builder.use_preconfigured_tls(config)
            }
            Err(_) => builder,
        }
    }

    /// The builder of every sign-in client of this cloud: it trusts the cloud's extra certificate authorities
    /// ([`Cloud::trust`]), and for an own server it waits at most [`OWN_CONNECT_TIMEOUT`] for a connection.
    pub fn sign_in_client(&self) -> reqwest::ClientBuilder {
        let builder = self.trust(reqwest::Client::builder());
        match self {
            Self::Microsoft => builder,
            Self::Own { .. } => builder.connect_timeout(OWN_CONNECT_TIMEOUT),
        }
    }

    /// Whether an HTTPS URL belongs to this own server, including its port.
    #[must_use]
    pub fn contains(&self, url: &Url) -> bool {
        self.origin().is_some_and(|origin| {
            url.scheme() == "https"
                && url.origin() == origin.origin()
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none()
        })
    }

    /// A fixed service URL for this account, preserving Microsoft's exact URL in Microsoft mode.
    ///
    /// # Errors
    /// Returns a URL parse error for an invalid Microsoft URL.
    pub fn service_url(&self, microsoft: &str, own_path: &str) -> Result<Url, url::ParseError> {
        match self.origin() {
            None => Url::parse(microsoft),
            Some(origin) => {
                let mut url = origin.clone();
                url.set_path(own_path);
                Ok(url)
            }
        }
    }

    /// Projects a URL on this own server's prefix to a Microsoft-shaped URL for existing wire validators.
    /// The original URL remains the destination; the projection is only for checking its service path.
    #[must_use]
    pub fn project(&self, url: &Url, prefix: &str, microsoft: &str) -> Option<Url> {
        if !self.contains(url) {
            return None;
        }
        let rest = url.path().strip_prefix(prefix)?;
        if !rest.is_empty() && !rest.starts_with('/') {
            return None;
        }
        let mut projected = Url::parse(microsoft).ok()?;
        let path = format!("{}{rest}", projected.path().trim_end_matches('/'));
        projected.set_path(&path);
        projected.set_query(url.query());
        Some(projected)
    }

    #[must_use]
    pub fn authority(&self) -> String {
        self.origin().map_or_else(
            || crate::token::ORGANIZATIONS_AUTHORITY.to_owned(),
            |origin| origin.as_str().to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_is_strict_and_port_is_part_of_boundary() {
        let cloud = Cloud::own("https://chat.example:8443").unwrap();
        assert!(cloud.contains(&Url::parse("https://chat.example:8443/graph/v1.0/me").unwrap()));
        for url in [
            "https://chat.example/graph/me",
            "https://teams.microsoft.com/",
            "http://chat.example:8443/",
            "https://u@chat.example:8443/",
        ] {
            assert!(!cloud.contains(&Url::parse(url).unwrap()));
        }
        for origin in [
            "http://chat.example",
            "https://u:p@chat.example",
            "https://chat.example/path",
            "https://chat.example/?q=x",
            "https://chat.example/#x",
            " https://chat.example",
            "https://chat.example/path/..",
        ] {
            assert!(Cloud::own(origin).is_err(), "{origin}");
        }
    }

    #[test]
    fn authentication_endpoints_follow_cloud_and_preserve_microsoft() {
        let microsoft = crate::token::EntraConfig::teams_native();
        let explicit = microsoft.clone().for_cloud(&Cloud::Microsoft);
        assert_eq!(explicit.authorize_endpoint(), microsoft.authorize_endpoint());
        assert_eq!(explicit.token_endpoint(), microsoft.token_endpoint());
        assert_eq!(
            crate::authz::AuthzClient::for_cloud(&Cloud::Microsoft)
                .unwrap()
                .endpoint(),
            crate::authz::AUTHZ_ENDPOINT
        );
        let own = Cloud::own("https://chat.example:8443").unwrap();
        let config = microsoft.for_cloud(&own);
        assert_eq!(
            config.authorize_endpoint(),
            "https://chat.example:8443/oauth2/v2.0/authorize"
        );
        assert_eq!(config.token_endpoint(), "https://chat.example:8443/oauth2/v2.0/token");
        assert_eq!(
            crate::authz::AuthzClient::for_cloud(&own).unwrap().endpoint(),
            "https://chat.example:8443/api/authsvc/v1.0/authz"
        );
    }

    #[test]
    fn service_projection_checks_prefix_boundary() {
        let cloud = Cloud::own("https://chat.example").unwrap();
        let url = cloud
            .service_url("https://unused.example", "/flightproxy/api/v2/ep/service/accept")
            .unwrap();
        assert_eq!(
            cloud
                .project(&url, "/flightproxy", "https://api.flightproxy.teams.microsoft.com")
                .unwrap()
                .as_str(),
            "https://api.flightproxy.teams.microsoft.com/api/v2/ep/service/accept"
        );
        assert!(cloud.project(&url, "/flight", "https://unused.example").is_none());
        assert!(
            Cloud::Microsoft
                .project(&url, "/flightproxy", "https://unused.example")
                .is_none()
        );
    }

    #[test]
    fn only_an_own_servers_sign_in_clients_wait_a_short_time_for_a_connection() {
        let limit = format!("connect_timeout: {OWN_CONNECT_TIMEOUT:?}");
        let own = Cloud::own("https://chat.example").unwrap();
        assert!(format!("{:?}", own.sign_in_client()).contains(&limit));
        assert!(!format!("{:?}", Cloud::Microsoft.sign_in_client()).contains("connect_timeout"));
    }
}
