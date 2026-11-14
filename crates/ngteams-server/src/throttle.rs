//! Failed password sign-ins per account: after `max_failures` within the window, the account's password sign-ins are
//! held off until the window ends. Counted in Redis when the server has one (every node sees the same count), else in
//! the node.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use redis::AsyncCommands as _;
use redis::aio::ConnectionManager;
use sha2::{Digest as _, Sha256};

use crate::config::LoginConfig;

/// The failures counted: per account, the count and when its window started.
type Windows = HashMap<String, (u32, Instant)>;

/// Where the counts live.
pub enum Throttle {
    Local {
        limits: LoginConfig,
        windows: Mutex<Windows>,
    },
    Redis {
        limits: LoginConfig,
        redis: ConnectionManager,
    },
}

impl std::fmt::Debug for Throttle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local { .. } => "Throttle::Local",
            Self::Redis { .. } => "Throttle::Redis",
        })
    }
}

impl Throttle {
    #[must_use]
    pub fn local(limits: LoginConfig) -> Self {
        Self::Local {
            limits,
            windows: Mutex::default(),
        }
    }

    #[must_use]
    pub fn redis(limits: LoginConfig, redis: ConnectionManager) -> Self {
        Self::Redis { limits, redis }
    }

    /// How long `account`'s password sign-ins are still held off, if they are. A Redis failure holds nothing off (a
    /// sign-in still needs the password).
    pub async fn hold(&self, account: &str) -> Option<Duration> {
        match self {
            Self::Local { limits, windows } => {
                let mut windows = windows.lock().unwrap_or_else(PoisonError::into_inner);
                let window = Duration::from_secs(limits.window);
                windows.retain(|_, (_, started)| started.elapsed() < window);
                let (count, started) = windows.get(account)?;
                (*count >= limits.max_failures).then(|| window.saturating_sub(started.elapsed()))
            }
            Self::Redis { limits, redis } => {
                let mut redis = redis.clone();
                let key = key(account);
                let (count, ttl): (Option<u32>, i64) =
                    redis::pipe().get(&key).ttl(&key).query_async(&mut redis).await.ok()?;
                let held = count.is_some_and(|count| count >= limits.max_failures);
                held.then(|| Duration::from_secs(u64::try_from(ttl).unwrap_or(limits.window).max(1)))
            }
        }
    }

    /// Counts a failed sign-in of `account`.
    pub async fn failed(&self, account: &str) {
        match self {
            Self::Local { limits, windows } => {
                let mut windows = windows.lock().unwrap_or_else(PoisonError::into_inner);
                let window = Duration::from_secs(limits.window);
                let entry = windows.entry(account.to_owned()).or_insert((0, Instant::now()));
                if entry.1.elapsed() >= window {
                    *entry = (0, Instant::now());
                }
                entry.0 = entry.0.saturating_add(1);
            }
            Self::Redis { limits, redis } => {
                let mut redis = redis.clone();
                let key = key(account);
                // the window's key is made with its expiry, then counted, in one transaction
                let counted: Result<(), _> = redis::pipe()
                    .atomic()
                    .cmd("SET")
                    .arg(&key)
                    .arg(0)
                    .arg("NX")
                    .arg("EX")
                    .arg(limits.window)
                    .ignore()
                    .incr(&key, 1)
                    .ignore()
                    .query_async(&mut redis)
                    .await;
                if let Err(error) = counted {
                    tracing::warn!(%error, "can't count a failed sign-in");
                }
            }
        }
    }

    /// Forgets `account`'s failures after it signed in.
    pub async fn succeeded(&self, account: &str) {
        match self {
            Self::Local { windows, .. } => {
                windows.lock().unwrap_or_else(PoisonError::into_inner).remove(account);
            }
            Self::Redis { redis, .. } => {
                let mut redis = redis.clone();
                if let Err(error) = redis.del::<_, ()>(key(account)).await {
                    tracing::warn!(%error, "can't clear the login failures");
                }
            }
        }
    }
}

/// The Redis key of an account's count: its UPN hashed, so Redis holds no account names.
fn key(account: &str) -> String {
    format!(
        "ngts:login:{}",
        URL_SAFE_NO_PAD.encode(Sha256::digest(account.as_bytes()))
    )
}
