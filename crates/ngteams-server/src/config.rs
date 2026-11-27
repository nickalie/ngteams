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
/// Postgres, S3 store, the Redis they stop and their SDK bot containers.
const NOT_KEYS: [&str; 8] = [
    "config",
    "log",
    "pg_url",
    "s3_url",
    "stop_redis",
    "bot_images",
    "bot_engine",
    "bot_network",
];

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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The deployment's part of that Redis (`crate::fanout::keyspace`): every key and channel is under it. Without
    /// it, the tenant id in the database, which the nodes of one deployment share and no other deployment has. Set
    /// it where two deployments on one Redis have the same tenant id (a database restored from another's).
    pub redis_namespace: Option<String>,
    /// The longest a Redis command or connection attempt may take, in milliseconds. Every use of Redis goes on
    /// without it, so a request waits about this long for a Redis that is down or doesn't answer, then the commands
    /// right after fail at once for as long.
    pub redis_timeout_ms: u64,
    /// Where files go: `file:///<dir>`, `memory:///` or `s3://<bucket>`, with `storage_options` (S3's region,
    /// endpoint, keys) as `object_store` names them.
    pub storage_url: String,
    pub storage_options: BTreeMap<String, String>,
    pub storage: StorageConfig,
    /// TLS on the listener; without it the server speaks plain HTTP for a reverse proxy in front.
    pub tls: Option<TlsConfig>,
    /// The region and partition the client's bases and tokens name.
    pub region: String,
    pub partition: String,
    /// The resources a token may be asked for.
    pub audiences: Vec<String>,
    pub tokens: TokenConfig,
    pub login: LoginConfig,
    pub setup: SetupConfig,
    pub trouter: TrouterConfig,
    pub presence: PresenceConfig,
    pub media: MediaConfig,
    pub calling: crate::calling::config::CallingConfig,
    /// The media node of group calls (`serve --role all|media`).
    pub mixer: crate::mixer::config::MixerConfig,
    /// The meeting rooms the scheduling form's room finder lists (`[[rooms]]`).
    pub rooms: Vec<RoomConfig>,
    pub workflows: crate::workflows::WorkflowsConfig,
    pub apps: crate::apps::AppsConfig,
    pub bots: crate::apps::BotsConfig,
    pub outgoing_webhooks: crate::outgoing::OutgoingConfig,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("region", &self.region)
            .field("partition", &self.partition)
            .field("tokens", &self.tokens)
            .field("login", &self.login)
            .field("setup", &self.setup)
            .field("trouter", &self.trouter)
            .field("presence", &self.presence)
            .field("media", &self.media)
            .field("storage", &self.storage)
            .field("calling", &self.calling)
            .field("workflows", &self.workflows)
            .field("mixer", &self.mixer)
            .field("apps", &self.apps)
            .field("bots", &self.bots)
            .field("outgoing_webhooks", &self.outgoing_webhooks)
            .finish_non_exhaustive()
    }
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
    /// Password attempts an account may have in `window` before it is held off; success clears the count.
    pub max_failures: u32,
    /// Seconds the failures are counted over, and the longest hold.
    pub window: u64,
}

/// The first-admin setup (`crate::admin::setup`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SetupConfig {
    /// Whether creating the first admin takes the setup code the server logs. Off only for an install nobody else
    /// can reach before its admin exists.
    pub require_code: bool,
    /// Setup attempts a client address may make in `window` seconds.
    pub max_attempts: u32,
    pub window: u64,
}

/// How long a node waits for an S3 store (`storage_url`) before it answers 503, in seconds. `read_timeout` and
/// `connect_timeout` in `storage_options` take the place of the two timeouts here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StorageConfig {
    /// The longest wait for a connection to the store.
    pub connect_timeout: u64,
    /// The longest the store may take to answer one request (one picture, or one 5 MiB part of a file, sent
    /// whole first), and the longest a download may then stand still. A download's whole time has no limit: it
    /// goes at its reader's pace. Raise it when the link between the server and its store is slow.
    pub request_timeout: u64,
    /// How often a request the store didn't answer, or answered with a server error, is sent again.
    pub max_retries: usize,
    /// No request is sent again later than this after its first attempt.
    pub retry_timeout: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            connect_timeout: 3,
            request_timeout: 30,
            max_retries: 3,
            retry_timeout: 8,
        }
    }
}

/// Pictures, files and link previews (`crate::media`). Sizes are in bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MediaConfig {
    /// The largest picture pasted into a message.
    pub max_picture_bytes: u64,
    /// The largest file sent in a chat.
    pub max_file_bytes: u64,
    /// What one account may store, pictures and files together; 0 is no limit.
    pub user_quota_bytes: u64,
    /// Seconds a file's download or thumbnail link works.
    pub link_lifetime: u64,
    /// Whether the server fetches pages and pictures for link previews.
    pub link_previews: bool,
    /// Seconds one preview fetch may take.
    pub preview_timeout: u64,
    /// The most of a page read for its preview, and the largest picture fetched for one.
    pub preview_max_page_bytes: u64,
    pub preview_max_picture_bytes: u64,
    /// Lets previews reach loopback, private and link-local addresses. Only for tests: on a real server it lets
    /// any account read the server's own network.
    pub preview_allow_private_addresses: bool,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            max_picture_bytes: 24 * 1024 * 1024,
            max_file_bytes: 250 * 1024 * 1024,
            user_quota_bytes: 5 * 1024 * 1024 * 1024,
            link_lifetime: 3600,
            link_previews: true,
            preview_timeout: 5,
            preview_max_page_bytes: 1024 * 1024,
            preview_max_picture_bytes: 5 * 1024 * 1024,
            preview_allow_private_addresses: false,
        }
    }
}

impl Default for SetupConfig {
    fn default() -> Self {
        Self {
            require_code: true,
            max_attempts: 5,
            window: 300,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrouterConfig {
    /// Seconds a push socket has to authenticate after it opened.
    pub auth_timeout: u64,
}

impl Default for TrouterConfig {
    fn default() -> Self {
        Self { auth_timeout: 10 }
    }
}

impl TrouterConfig {
    #[must_use]
    pub fn auth_timeout(&self) -> Duration {
        Duration::from_secs(self.auth_timeout)
    }
}

/// Presence (`crate::presence`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PresenceConfig {
    /// Seconds an endpoint shows as it was last reported; the client reports again every two minutes.
    pub endpoint_ttl: u64,
    /// Seconds a presence subscription holds; the client renews its own every 55 minutes.
    pub subscription_ttl: u64,
    /// The people one endpoint may watch.
    pub max_subscriptions: usize,
    /// The endpoints one account may report at once.
    pub max_endpoints: usize,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            endpoint_ttl: 320,
            subscription_ttl: 7200,
            max_subscriptions: 650,
            max_endpoints: 32,
        }
    }
}

/// A meeting room: a place an event can name. It is no account and takes no invitation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoomConfig {
    pub name: String,
    /// Its mailbox, as the room finder names it.
    pub address: String,
    /// The room list (a building) it is in, by name.
    pub list: Option<String>,
    /// How many people it seats.
    pub capacity: Option<u32>,
    pub floor: Option<String>,
    pub street: Option<String>,
    pub audio_device: Option<String>,
    pub video_device: Option<String>,
    pub display_device: Option<String>,
    pub wheelchair: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 8080)),
            public_origin: Url::parse("http://127.0.0.1:8080").unwrap_or_else(|_| unreachable!()),
            database_url: "sqlite://ngteams-server.sqlite".to_owned(),
            database_pool: 16,
            redis_url: None,
            redis_namespace: None,
            redis_timeout_ms: 1000,
            storage_url: "memory:///".to_owned(),
            storage_options: BTreeMap::new(),
            storage: StorageConfig::default(),
            tls: None,
            region: "own".to_owned(),
            partition: "own01".to_owned(),
            audiences: DEFAULT_AUDIENCES.iter().map(|&audience| audience.to_owned()).collect(),
            tokens: TokenConfig::default(),
            login: LoginConfig::default(),
            setup: SetupConfig::default(),
            trouter: TrouterConfig::default(),
            presence: PresenceConfig::default(),
            media: MediaConfig::default(),
            calling: crate::calling::config::CallingConfig::default(),
            mixer: crate::mixer::config::MixerConfig::default(),
            rooms: Vec::new(),
            workflows: crate::workflows::WorkflowsConfig::default(),
            apps: crate::apps::AppsConfig::default(),
            bots: crate::apps::BotsConfig::default(),
            outgoing_webhooks: crate::outgoing::OutgoingConfig::default(),
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
    #[error("redis_namespace must be 1 to 64 letters, digits, '.', '_' or '-'")]
    Namespace,
    #[error("redis_timeout_ms must be above zero")]
    RedisTimeout,
    #[error(
        "tokens' lifetimes, the login and setup windows and the push socket's authentication time must be above zero"
    )]
    Lifetime,
    #[error("media sizes, the link lifetime and the preview timeout must be above zero")]
    Media,
    #[error("the store's connect and request timeouts must be above zero")]
    Storage,
    #[error(
        "calling.ring_timeout and calling.turn.ttl must be above zero, and calling.turn.username and \
         calling.turn.password set together, neither empty"
    )]
    Calling,
    #[error(
        "calling.turn.urls must be turn: or turns: URLs with a host (an optional port, an optional transport of udp \
         or tcp), set together with a credential (calling.turn.secret, or calling.turn.username and password)"
    )]
    Relay,
    #[error(
        "mixer's limits and times must be above zero, mixer.dead_after above mixer.heartbeat, and \
         mixer.internal_url an http or https URL"
    )]
    Mixer,
    #[error(
        "bots.scope must be an absolute URL without a trailing slash that no person's token is issued for, \
         bots.issuer and bots.service_url absolute URLs, the bots' lifetimes, key rotation, send timeout, rates and \
         bursts above zero, bots.delivery_lease longer than bots.send_timeout, and bots.token_key 32 bytes in base64"
    )]
    Bots,
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

    /// Refuses an origin a client wouldn't accept, a region that can't stand in a path, an unknown database, a Redis
    /// namespace that isn't a plain name and a zero lifetime.
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
        let namespace = self.redis_namespace.as_deref();
        if namespace.is_some_and(|namespace| !crate::fanout::keyspace::Keyspace::is_namespace(namespace)) {
            return Err(ConfigError::Namespace);
        }
        if self.redis_timeout_ms == 0 {
            return Err(ConfigError::RedisTimeout);
        }
        if self.tokens.access_lifetime == 0
            || self.tokens.refresh_lifetime == 0
            || self.tokens.key_rotation == 0
            || self.login.window == 0
            || self.setup.window == 0
            || self.trouter.auth_timeout == 0
            || self.presence.endpoint_ttl == 0
            || self.presence.subscription_ttl == 0
        {
            return Err(ConfigError::Lifetime);
        }
        let media = &self.media;
        if media.max_picture_bytes == 0
            || media.max_file_bytes == 0
            || media.link_lifetime == 0
            || media.preview_timeout == 0
            || media.preview_max_page_bytes == 0
            || media.preview_max_picture_bytes == 0
        {
            return Err(ConfigError::Media);
        }
        if self.storage.connect_timeout == 0 || self.storage.request_timeout == 0 {
            return Err(ConfigError::Storage);
        }
        if !self.calling.usable() {
            return Err(ConfigError::Calling);
        }
        if !self.calling.turn.relay_usable() {
            return Err(ConfigError::Relay);
        }
        if !self.mixer.usable() {
            return Err(ConfigError::Mixer);
        }
        if !self.bots.usable(&self.person_audiences()) {
            return Err(ConfigError::Bots);
        }
        Ok(())
    }

    /// [`Self::redis_timeout_ms`].
    #[must_use]
    pub fn redis_timeout(&self) -> Duration {
        Duration::from_millis(self.redis_timeout_ms)
    }

    /// Every audience a person's token may be issued for: the configured services, the server's own origin (the
    /// file service's), and Workflows' while it serves them.
    #[must_use]
    pub fn person_audiences(&self) -> Vec<String> {
        let mut audiences = self.audiences.clone();
        audiences.push(self.origin());
        audiences.extend(crate::workflows::audiences(self));
        audiences
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
