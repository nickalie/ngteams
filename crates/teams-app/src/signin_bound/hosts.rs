//! The personal sign-in window's host bound (`docs/personal-build.md`, "Its sign-in", Choice C, approved on
//! 2026-10-23): Entra's sign-in host, its static content hosts, and the federated identity provider's hosts the user
//! names in the account's `accounts.toml` entry (`idp_hosts`), `https` on 443 only. No Teams, Office or Graph host is on it: the window can
//! only sign in. Every production sign-in window uses this bound.

use url::Url;

/// Entra's sign-in host and the static content hosts its sign-in page loads from.
pub const ENTRA_HOSTS: [&str; 7] = [
    "login.microsoftonline.com",
    "aadcdn.msftauth.net",
    "aadcdn.msauth.net",
    "logincdn.msftauth.net",
    "logincdn.msauth.net",
    "aadcdn.msftauthimages.net",
    "aadcdn.msauthimages.net",
];

/// The one port the gate opens a tunnel to.
pub const HTTPS_PORT: u16 = 443;

/// Microsoft's service domains no identity provider host may be or lie under: a host the user names that would let
/// the window reach a Teams, Office, Graph or other service with the user's session is refused.
pub const SERVICE_DOMAINS: [&str; 10] = [
    "microsoft.com",
    "cloud.microsoft",
    "office.com",
    "office.net",
    "office365.com",
    "microsoftonline.com",
    "sharepoint.com",
    "skype.com",
    "live.com",
    "windows.net",
];

/// The hosts the window may reach (see the module docs), each exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignInHosts {
    hosts: Vec<String>,
    /// The identity provider's hosts, which also take a `POST` on any path.
    idp: Vec<String>,
    port: u16,
}

/// Why an identity provider host was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{host:?} can't be an identity provider host: {why}")]
pub struct HostRefused {
    pub host: String,
    pub why: &'static str,
}

impl SignInHosts {
    /// Entra's hosts and the identity provider's `idp_hosts`, each a lowercase DNS name that isn't one of Microsoft's
    /// service domains or under one ([`SERVICE_DOMAINS`]).
    ///
    /// # Errors
    /// [`HostRefused`] for the first host that isn't one.
    pub fn entra_with(idp_hosts: &[String]) -> Result<Self, HostRefused> {
        for host in idp_hosts {
            let refused = |why| HostRefused {
                host: host.clone(),
                why,
            };
            if !is_dns_name(host) {
                return Err(refused("not a lowercase DNS name"));
            }
            if SERVICE_DOMAINS
                .iter()
                .any(|domain| host == domain || host.ends_with(&format!(".{domain}")))
            {
                return Err(refused("one of Microsoft's service domains"));
            }
        }
        let mut hosts: Vec<String> = ENTRA_HOSTS.iter().map(|host| (*host).to_owned()).collect();
        hosts.extend(idp_hosts.iter().cloned());
        Ok(Self {
            hosts,
            idp: idp_hosts.to_vec(),
            port: HTTPS_PORT,
        })
    }

    /// Any hosts on `port`, as given. Tests only (`test-support`): a local server stands in for Entra.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn for_tests(hosts: &[&str], port: u16) -> Self {
        Self {
            hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            idp: Vec::new(),
            port,
        }
    }

    /// Whether the gate opens a tunnel to `host`:`port`.
    #[must_use]
    pub fn admits(&self, host: &str, port: u16) -> bool {
        port == self.port && self.hosts.iter().any(|allowed| allowed == host)
    }

    /// Whether `url` is a page on the bound: `https` (or, in a test's bound, the scheme its port implies), a host on
    /// the list, the gate's port, no user info.
    #[must_use]
    pub fn admits_url(&self, url: &str) -> bool {
        let Ok(url) = Url::parse(url) else {
            return false;
        };
        let scheme_ok = url.scheme() == "https" || (self.port != HTTPS_PORT && url.scheme() == "http");
        let host = url.host_str().unwrap_or_default();
        scheme_ok
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default().is_some_and(|port| self.admits(host, port))
    }

    /// The identity provider's hosts (each also takes a `POST` on any path, `super::classify::Classifier::personal`).
    #[must_use]
    pub fn idp_hosts(&self) -> &[String] {
        &self.idp
    }

    /// The host of `url`, a navigation the window refused, when the user could name it in the account's `idp_hosts`:
    /// an `https` page whose host is a DNS name off the bound that [`SignInHosts::entra_with`] takes (so never one of
    /// Microsoft's service domains, an address or a host on the bound already); `None` otherwise. The host alone: never
    /// the URL's path or query.
    #[must_use]
    pub fn idp_candidate(&self, url: &str) -> Option<String> {
        let url = Url::parse(url).ok()?;
        let Some(url::Host::Domain(host)) = url.host() else {
            return None;
        };
        let host = host.to_owned();
        let off_bound = url.scheme() == "https" && !self.hosts.contains(&host);
        (off_bound && Self::entra_with(std::slice::from_ref(&host)).is_ok()).then_some(host)
    }
}

/// A lowercase DNS name of at least two labels.
fn is_dns_name(text: &str) -> bool {
    let labels: Vec<&str> = text.split('.').collect();
    labels.len() >= 2
        && text.len() <= 253
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}
