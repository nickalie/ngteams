//! Send limits (`docs/server.md` "Bots"), on each node, with governor's GCRA:
//!
//! - the server's sends to one bot keep to `bots.send_rate` (a burst of `bots.send_burst`): a send over it waits for
//!   its turn, as a row of the outbox due later, and the wait is no failed attempt; the held rows of one bot get
//!   turns one after the other, so they don't all come due at once, and a row held once more keeps its turn;
//! - one app's calls to the connector keep to `bots.connector_rate` (a burst of `bots.connector_burst`); more get
//!   the Bot Framework's `429` with `Retry-After` ([`connector`]).

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Json;
use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::response::{IntoResponse, Response};
use governor::clock::{Clock as _, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use serde_json::json;
use tower_governor::GovernorError;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::KeyExtractor;

use crate::apps::BotsConfig;

/// How many bots a limiter tracks before it forgets the ones that are idle again.
const TRACKED: usize = 10_000;

/// A rate and its burst as governor's quota; a zero (refused by the config check) as one.
fn quota(rate: u32, burst: u32) -> Quota {
    let one = |value: u32| NonZeroU32::new(value).unwrap_or(NonZeroU32::MIN);
    Quota::per_second(one(rate)).allow_burst(one(burst))
}

/// The server's sends to each bot.
pub struct SendLimit {
    limiter: DefaultKeyedRateLimiter<String>,
    /// The time between two sends to one bot at its rate.
    interval: Duration,
    /// The turns given to held rows.
    turns: Mutex<Turns>,
}

/// The turns [`SendLimit`] gave.
#[derive(Default)]
struct Turns {
    /// Per bot, when the turn after the last one given to a held row starts.
    next: HashMap<String, Instant>,
    /// Per held row (by its id), the turn it got.
    rows: HashMap<String, Instant>,
}

/// How long a row keeps the turn it got after that turn started: a row its bot's limit holds once more in that time
/// keeps its place in the line.
const KEPT: Duration = Duration::from_secs(60);

impl std::fmt::Debug for SendLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SendLimit")
    }
}

impl SendLimit {
    #[must_use]
    pub fn new(config: &BotsConfig) -> Self {
        Self {
            limiter: RateLimiter::keyed(quota(config.send_rate, config.send_burst)),
            interval: Duration::from_secs(1) / config.send_rate.max(1),
            turns: Mutex::new(Turns::default()),
        }
    }

    /// Takes one send to `bot` now, as a send that can't wait for a turn: `None` when it may go now, else how long
    /// until the limit has room. It gives no turn.
    pub(crate) fn take_now(&self, bot: &str) -> Option<Duration> {
        if self.limiter.len() > TRACKED {
            self.limiter.retain_recent();
        }
        Some(
            self.limiter
                .check_key(&bot.to_owned())
                .err()?
                .wait_time_from(DefaultClock::default().now()),
        )
    }

    /// Takes one send of the outbox's row `row` to `bot`: `None` when it may go now, else how long until its turn. A
    /// row held for the first time gets the turn after the one the last held row got. A row that got a turn before
    /// keeps it: when the limit holds it once more, it waits only until the limit has room.
    pub(crate) fn take(&self, bot: &str, row: &str) -> Option<Duration> {
        let wait = self.take_now(bot);
        let now = Instant::now();
        let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(wait) = wait else {
            turns.rows.remove(row);
            return None;
        };
        if turns.rows.len() > TRACKED {
            turns.rows.retain(|_, turn| *turn + KEPT > now);
        }
        if turns.next.len() > TRACKED {
            turns.next.retain(|_, turn| *turn > now);
        }
        if let Some(turn) = turns.rows.get(row) {
            return Some(turn.saturating_duration_since(now).max(wait));
        }
        let turn = turns
            .next
            .get(bot)
            .copied()
            .map_or(now + wait, |turn| turn.max(now + wait));
        turns.next.insert(bot.to_owned(), turn + self.interval);
        turns.rows.insert(row.to_owned(), turn);
        Some(turn.saturating_duration_since(now))
    }
}

/// Whole seconds a client waits before `wait` is over, as `Retry-After` gives them: never zero.
pub(crate) fn retry_after(wait: Duration) -> u64 {
    wait.as_secs() + u64::from(wait.subsec_nanos() > 0)
}

/// The app a connector request authenticated as ([`connector`]'s key).
#[derive(Clone, Debug)]
pub(super) struct AppKey(pub String);

/// Keys a connector request by the app its credential proved (put in the request by the connector's
/// authentication, which runs first).
#[derive(Clone, Copy, Debug)]
pub(super) struct ByApp;

impl KeyExtractor for ByApp {
    type Key = String;

    fn extract<T>(&self, request: &axum::http::Request<T>) -> Result<String, GovernorError> {
        request
            .extensions()
            .get::<AppKey>()
            .map(|key| key.0.clone())
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

/// The connector's limit per app. governor gives none for a zero period or burst; the rate and burst are held to at
/// least one and the period to at least a nanosecond, so there is always one.
pub(super) fn connector(
    config: &BotsConfig,
) -> Option<Arc<GovernorConfig<ByApp, governor::middleware::NoOpMiddleware>>> {
    let rate = u64::from(config.connector_rate.max(1));
    GovernorConfigBuilder::default()
        .period(Duration::from_nanos((1_000_000_000 / rate).max(1)))
        .burst_size(config.connector_burst.max(1))
        .key_extractor(ByApp)
        .finish()
        .map(Arc::new)
}

/// A refusal of the governor layer in the Bot Framework's `ErrorResponse` shape, with `Retry-After`.
pub(super) fn refused(error: &GovernorError) -> Response {
    match error {
        GovernorError::TooManyRequests { wait_time, .. } => {
            let body =
                json!({"error": {"code": "TooManyRequests", "message": "The bot sends too fast; wait and try again."}});
            // governor's whole seconds are cut down: one more is never too soon
            let wait = wait_time + 1;
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(RETRY_AFTER, wait.to_string())],
                Json(body),
            )
                .into_response()
        }
        GovernorError::UnableToExtractKey | GovernorError::Other { .. } => {
            let body = json!({"error": {"code": "ServiceError", "message": "The service can't answer now."}});
            (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bot_over_its_burst_waits_and_another_bot_does_not() {
        let config = BotsConfig {
            send_rate: 1,
            send_burst: 2,
            ..BotsConfig::default()
        };
        let limit = SendLimit::new(&config);
        assert_eq!(limit.take("a", "r1"), None);
        assert_eq!(limit.take("a", "r2"), None);
        let wait = limit.take("a", "r3").unwrap();
        assert!(wait <= Duration::from_secs(1), "{wait:?}");
        assert_eq!(limit.take("b", "r4"), None);
        assert!(format!("{limit:?}").contains("SendLimit"));
    }

    /// Asserts that `later` is one interval (100 ms) after `earlier`, less the time the test took between the takes.
    fn one_interval_after(earlier: Duration, later: Duration) {
        let gap = later.saturating_sub(earlier);
        assert!(
            gap > Duration::from_millis(90) && gap <= Duration::from_millis(100),
            "{earlier:?} {later:?}"
        );
    }

    #[test]
    fn the_held_sends_of_a_bot_get_turns_one_after_the_other() {
        let config = BotsConfig {
            send_rate: 10,
            send_burst: 1,
            ..BotsConfig::default()
        };
        let limit = SendLimit::new(&config);
        assert_eq!(limit.take("a", "r0"), None);
        let turns: Vec<Duration> = (1..4).map(|row| limit.take("a", &format!("r{row}")).unwrap()).collect();
        assert!(turns[0] <= Duration::from_millis(100), "{turns:?}");
        for pair in turns.windows(2) {
            one_interval_after(pair[0], pair[1]);
        }
        assert_eq!(limit.take("b", "r4"), None);
    }

    #[test]
    fn a_row_held_once_more_keeps_its_turn() {
        let config = BotsConfig {
            send_rate: 10,
            send_burst: 1,
            ..BotsConfig::default()
        };
        let limit = SendLimit::new(&config);
        assert_eq!(limit.take("a", "r0"), None);
        let first = limit.take("a", "r1").unwrap();
        let second = limit.take("a", "r2").unwrap();
        // r1 again: its own turn, not one after r2's
        let again = limit.take("a", "r1").unwrap();
        assert!(again <= first, "{first:?} {again:?}");
        // and the line didn't move: r3 gets the turn after r2's
        one_interval_after(second, limit.take("a", "r3").unwrap());
    }

    #[test]
    fn a_send_that_cant_wait_gives_no_turn() {
        let config = BotsConfig {
            send_rate: 10,
            send_burst: 1,
            ..BotsConfig::default()
        };
        let limit = SendLimit::new(&config);
        assert_eq!(limit.take_now("a"), None);
        for _ in 0..3 {
            let wait = limit.take_now("a").unwrap();
            assert!(wait <= Duration::from_millis(100), "{wait:?}");
        }
        // the refused sends took no turn: the first held row gets the first one
        let turn = limit.take("a", "r1").unwrap();
        assert!(turn <= Duration::from_millis(100), "{turn:?}");
    }

    #[test]
    fn the_connector_limit_holds_for_a_zero_burst_and_a_rate_past_a_nanosecond() {
        let config = BotsConfig {
            connector_rate: u32::MAX,
            connector_burst: 0,
            ..BotsConfig::default()
        };
        assert!(connector(&config).is_some());
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        assert_eq!(retry_after(Duration::from_millis(1)), 1);
        assert_eq!(retry_after(Duration::from_secs(2)), 2);
        assert_eq!(retry_after(Duration::from_millis(2001)), 3);
    }

    #[test]
    fn a_refusal_without_a_key_is_a_server_failure() {
        let response = refused(&GovernorError::UnableToExtractKey);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let response = refused(&GovernorError::TooManyRequests {
            wait_time: 0,
            headers: None,
        });
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[RETRY_AFTER], "1");
    }
}
