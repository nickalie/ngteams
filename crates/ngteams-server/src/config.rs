//! The server's configuration: defaults, then a TOML file, then `NGTEAMS_SERVER_*` environment variables (figment).
//! A nested key takes a double underscore in the environment (`NGTEAMS_SERVER_TLS__CERT`).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};
use url::Url;

/// The environment variables' prefix.
pub const ENV_PREFIX: &str = "NGTEAMS_SERVER_";
/// Variables under the prefix that aren't configuration keys: the command line's file and log filter, and the tests'
/// Postgres.
const NOT_KEYS: [&str; 3] = ["config", "log", "pg_url"];

/// Audiences the client asks tokens for (`teams-proto`'s and `teams-auth`'s resources): a token request for any other
/// resource is refused.
pub const DEFAULT_AUDIENCES: [&str; 8] = [
    "https://api.spaces.skype.com",
    "https://ic3.teams.office.com",
    "https://chatsvcagg.teams.microsoft.com",
    "https://presence.teams.microsoft.com/",
    "https://templates.teams.microsoft.com",
    "https://graph.microsoft.com",
    "https://loki.delve.office.com",
    "https://outlook.office.com/search",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The address the server listens on.
    pub listen: SocketAddr,
    /// The origin clients reach the server at (`https://teams.example.com`): every base authz hands out is under it.
    /// Plain `http` only on a loopback host.
    pub public_origin: Url,
    /// `postgres://…` (several nodes) or `sqlite://<path>` (one node).
    pub database_url: String,
    /// The most database connections a node keeps (SQLite always takes one: its writes serialize anyway).
    pub database_pool: u32,
    /// `redis://…` or `rediss://…`: fan-out and login counters shared between nodes. Without it, both are in-process.
    pub redis_url: Option<String>,
    /// Where files go: `file:///<dir>`, `memory:///` or `s3://<bucket>`, with `storage_options` (S3's region,
    /// endpoint, keys) as `object_store` names them.
    pub storage_url: String,
    pub storage_options: BTreeMap<String, String>,
    /// TLS on the listener; without it the server speaks plain HTTP for a reverse proxy in front.
    pub tls: Option<TlsConfig>,
    /// The region and partition the client's bases and tokens name.
    pub region: String,
    pub partition: String,
    /// The resources a token may be asked for.
    pub audiences: Vec<String>,
    pub tokens: TokenConfig,
    pub login: LoginConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain.
    pub cert: PathBuf,
    /// PEM private key.
    pub key: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TokenConfig {
    /// An access token's lifetime, in seconds.
    pub access_lifetime: u64,
    /// A refresh token's lifetime, in seconds: how long an account stays signed in without using it.
    pub refresh_lifetime: u64,
    /// How long a signing key signs before a new one replaces it, in seconds.
    pub key_rotation: u64,
    /// Seconds a used refresh token may still be redeemed (a client that lost the answer to a refresh retries with
    /// it); after that, presenting it again revokes its whole sign-in.
    pub reuse_grace: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoginConfig {
    /// Failed password sign-ins an account may have in `window` before it is held off.
    pub max_failures: u32,
    /// Seconds the failures are counted over, and the longest hold.
    pub window: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            public_origin: Url::parse("http://127.0.0.1:8080").unwrap_or_else(|_| unreachable!()),
            database_url: "sqlite://ngteams-server.sqlite".to_owned(),
            database_pool: 16,
            redis_url: None,
            storage_url: "memory:///".to_owned(),
            storage_options: BTreeMap::new(),
            tls: None,
            region: "own".to_owned(),
            partition: "own01".to_owned(),
            audiences: DEFAULT_AUDIENCES.iter().map(|&audience| audience.to_owned()).collect(),
            tokens: TokenConfig::default(),
            login: LoginConfig::default(),
        }
    }
}

impl Default for TokenConfig {
    fn default() -> Self {
        Self {
            access_lifetime: 3600,
            refresh_lifetime: 90 * 86_400,
            key_rotation: 30 * 86_400,
            reuse_grace: 60,
        }
    }
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            max_failures: 10,
            window: 900,
        }
    }
}

/// Why the configuration can't be used.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error(transparent)]
    Load(#[from] Box<figment::Error>),
    #[error("public_origin must be an https origin (http only on a loopback host), with nothing after the host")]
    PublicOrigin,
    #[error("region and partition must be short lowercase names (letters, digits, '-')")]
    Region,
    #[error("database_url must start with postgres:// or sqlite://")]
    Database,
    #[error("tokens' lifetimes and the login window must be above zero")]
    Lifetime,
}

impl Config {
    /// Defaults, then `file` (when given), then the environment; checked.
    ///
    /// # Errors
    /// A file or variable that doesn't parse, or a value [`Self::check`] refuses.
    pub fn load(file: Option<&Path>) -> Result<Self, ConfigError> {
        let mut figment = Figment::from(Serialized::defaults(Self::default()));
        if let Some(file) = file {
            figment = figment.merge(Toml::file_exact(file));
        }
        let config: Self = figment
            .merge(Env::prefixed(ENV_PREFIX).ignore(&NOT_KEYS).split("__"))
            .extract()
            .map_err(Box::new)?;
        config.check()?;
        Ok(config)
    }

    /// Refuses an origin a client wouldn't accept, a region that can't stand in a path, an unknown database and a
    /// zero lifetime.
    ///
    /// # Errors
    /// The first value that is wrong.
    pub fn check(&self) -> Result<(), ConfigError> {
        let origin = &self.public_origin;
        let loopback = origin.host().is_some_and(|host| match host {
            url::Host::Domain(name) => name == "localhost",
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
        });
        let scheme = origin.scheme() == "https" || (origin.scheme() == "http" && loopback);
        let bare = origin.path() == "/"
            && origin.query().is_none()
            && origin.fragment().is_none()
            && origin.username().is_empty()
            && origin.password().is_none();
        if !(scheme && bare) {
            return Err(ConfigError::PublicOrigin);
        }
        if !is_name(&self.region) || !is_name(&self.partition) {
            return Err(ConfigError::Region);
        }
        if !self.database_url.starts_with("postgres://")
            && !self.database_url.starts_with("postgresql://")
            && !self.database_url.starts_with("sqlite://")
        {
            return Err(ConfigError::Database);
        }
        if self.tokens.access_lifetime == 0
            || self.tokens.refresh_lifetime == 0
            || self.tokens.key_rotation == 0
            || self.login.window == 0
        {
            return Err(ConfigError::Lifetime);
        }
        Ok(())
    }

    /// The public origin without its trailing slash, the form bases are built on.
    #[must_use]
    pub fn origin(&self) -> String {
        self.public_origin.as_str().trim_end_matches('/').to_owned()
    }
}

impl TokenConfig {
    #[must_use]
    pub fn access(&self) -> Duration {
        Duration::from_secs(self.access_lifetime)
    }

    #[must_use]
    pub fn refresh(&self) -> Duration {
        Duration::from_secs(self.refresh_lifetime)
    }

    #[must_use]
    pub fn rotation(&self) -> Duration {
        Duration::from_secs(self.key_rotation)
    }
}

fn is_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
