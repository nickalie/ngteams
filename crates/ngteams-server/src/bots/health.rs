//! A bot's delivery health as the Manage apps page shows it: its run of failed sends, whether it is paused and
//! until when, the last error and success, and its outbox's rows by state (kept a day, [`super::purge`]).

use std::collections::HashMap;

use diesel::dsl::count_star;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};

use super::outbox::{FAILED, PENDING, SENT};
use super::schema::{bot_deliveries, bot_health};
use crate::db::{Db, DbError, with_conn};

/// One bot's health.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Health {
    failures: i32,
    paused_until: i64,
    last_error: Option<String>,
    last_success: Option<i64>,
    pending: i64,
    sent: i64,
    failed: i64,
}

impl Health {
    /// `count` rows of the outbox in `state`.
    fn count(&mut self, state: &str, count: i64) {
        match state {
            PENDING => self.pending = count,
            SENT => self.sent = count,
            FAILED => self.failed = count,
            _ => {}
        }
    }

    /// As the admin API shows it at `now` (milliseconds): `state` is `paused`, `failing` (its last send failed),
    /// `healthy` (its last send went) or `new` (nothing sent yet).
    pub(crate) fn json(&self, now: i64) -> Value {
        let paused = self.paused_until > now;
        let state = if paused {
            "paused"
        } else if self.last_error.is_some() {
            "failing"
        } else if self.last_success.is_some() {
            "healthy"
        } else {
            "new"
        };
        json!({
            "state": state,
            "failures": self.failures,
            "paused_until": paused.then_some(self.paused_until),
            "last_error": self.last_error,
            "last_success": self.last_success,
            "outbox": {"pending": self.pending, "sent": self.sent, "failed": self.failed},
        })
    }
}

type HealthRow = (String, i32, i64, Option<String>, Option<i64>);

/// The health of each of `bots`; one with nothing kept yet is new.
pub(crate) async fn of(db: &Db, bots: &[String]) -> Result<HashMap<String, Health>, DbError> {
    if bots.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<HealthRow> = with_conn!(db, |conn| bot_health::table
        .filter(bot_health::bot_id.eq_any(bots))
        .select((
            bot_health::bot_id,
            bot_health::failures,
            bot_health::paused_until,
            bot_health::last_error,
            bot_health::last_success,
        ))
        .load(conn)
        .await?);
    let counts: Vec<(String, String, i64)> = with_conn!(db, |conn| bot_deliveries::table
        .filter(bot_deliveries::bot_id.eq_any(bots))
        .group_by((bot_deliveries::bot_id, bot_deliveries::state))
        .select((bot_deliveries::bot_id, bot_deliveries::state, count_star()))
        .load(conn)
        .await?);
    let mut health: HashMap<String, Health> = bots.iter().map(|bot| (bot.clone(), Health::default())).collect();
    for (bot, failures, paused_until, last_error, last_success) in rows {
        if let Some(entry) = health.get_mut(&bot) {
            entry.failures = failures;
            entry.paused_until = paused_until;
            entry.last_error = last_error;
            entry.last_success = last_success;
        }
    }
    for (bot, state, count) in counts {
        if let Some(entry) = health.get_mut(&bot) {
            entry.count(&state, count);
        }
    }
    Ok(health)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_follows_the_pause_then_the_last_outcome() {
        let mut health = Health::default();
        assert_eq!(health.json(10)["state"], "new");
        health.last_success = Some(5);
        assert_eq!(health.json(10)["state"], "healthy");
        health.last_error = Some("the bot answered 500".to_owned());
        assert_eq!(health.json(10)["state"], "failing");
        assert_eq!(health.json(10)["paused_until"], Value::Null);
        health.paused_until = 20;
        assert_eq!(health.json(10)["state"], "paused");
        assert_eq!(health.json(10)["paused_until"], 20);
        assert_eq!(health.json(30)["state"], "failing");
    }
}
