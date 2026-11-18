//! A bot's delivery health and its test activity in the administrator's words (`docs/server.md`, "Bots"): how the
//! server's deliveries to the bot go, the last day's counts, and how the bot answered a test activity.

use teams_auth::admin::{BotHealth, HealthState, TestActivity};

use super::AppProblem;

/// How long ago `at` (ms since the epoch) was at `now`, in rough words.
fn ago(at: i64, now: i64) -> String {
    let minutes = (now - at).max(0) / 60_000;
    match minutes {
        0 => "just now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    }
}

/// How long is left until `until` (ms since the epoch) at `now`, in rough words.
fn left(until: i64, now: i64) -> String {
    let seconds = (until - now).max(0) / 1000;
    if seconds < 60 {
        "less than a minute".to_owned()
    } else {
        let minutes = (seconds + 59) / 60;
        format!("{minutes} min")
    }
}

fn failed_in_a_row(failures: u32) -> String {
    if failures == 1 {
        "1 failed".to_owned()
    } else {
        format!("{failures} failed in a row")
    }
}

/// The health in one sentence at `now` (ms since the epoch), and whether it needs attention.
#[must_use]
pub fn summary(health: Option<&BotHealth>, now: i64) -> (String, bool) {
    let Some(health) = health else {
        return ("This server doesn't report delivery.".to_owned(), false);
    };
    let reason = health
        .last_error
        .as_deref()
        .map(|error| format!(" Last error: {error}."))
        .unwrap_or_default();
    match health.state {
        HealthState::New => ("Nothing sent to the bot yet.".to_owned(), false),
        HealthState::Healthy => {
            let when = health
                .last_success
                .map(|at| format!(" Last delivered {}.", ago(at, now)))
                .unwrap_or_default();
            (format!("Delivering.{when}"), false)
        }
        HealthState::Failing => (
            format!("Not delivering: {}.{reason}", failed_in_a_row(health.failures)),
            true,
        ),
        HealthState::Paused => {
            let pause = health
                .paused_until
                .map(|until| format!(" for {}", left(until, now)))
                .unwrap_or_default();
            (
                format!("Paused{pause} after many failed in a row; the server then tries again.{reason}"),
                true,
            )
        }
        HealthState::Other => ("The server reports a state this app doesn't know.".to_owned(), false),
    }
}

/// The last day's activities to the bot, when there were any.
#[must_use]
pub fn counts(health: Option<&BotHealth>) -> Option<String> {
    let health = health?;
    if health.pending + health.sent + health.failed == 0 {
        return None;
    }
    Some(format!(
        "Last day: {} delivered, {} failed, {} waiting",
        health.sent, health.failed, health.pending
    ))
}

/// How the bot answered a test activity, and whether it took it.
#[must_use]
pub fn went(result: &Result<TestActivity, AppProblem>) -> (String, bool) {
    match result {
        Ok(test) if test.delivered => {
            let status = test.status.map(|status| format!(" ({status})")).unwrap_or_default();
            (format!("The bot took it{status} in {} ms.", test.elapsed_ms), true)
        }
        Ok(test) => {
            let why = test
                .error
                .clone()
                .or_else(|| test.status.map(|status| format!("it answered {status}")))
                .unwrap_or_else(|| "no answer".to_owned());
            (format!("The bot didn't take it: {why}."), false)
        }
        Err(problem) => (problem.words().to_string(), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn health(state: HealthState) -> BotHealth {
        BotHealth {
            state,
            failures: 0,
            paused_until: None,
            last_error: None,
            last_success: None,
            pending: 0,
            sent: 0,
            failed: 0,
        }
    }

    #[test]
    fn each_state_has_its_sentence() {
        assert_eq!(summary(None, NOW).0, "This server doesn't report delivery.");
        assert_eq!(
            summary(Some(&health(HealthState::New)), NOW),
            ("Nothing sent to the bot yet.".to_owned(), false)
        );
        let healthy = BotHealth {
            last_success: Some(NOW - 5 * 60_000),
            ..health(HealthState::Healthy)
        };
        assert_eq!(summary(Some(&healthy), NOW).0, "Delivering. Last delivered 5 min ago.");
        let failing = BotHealth {
            failures: 3,
            last_error: Some("the bot answered 500".to_owned()),
            ..health(HealthState::Failing)
        };
        assert_eq!(
            summary(Some(&failing), NOW),
            (
                "Not delivering: 3 failed in a row. Last error: the bot answered 500.".to_owned(),
                true
            )
        );
        let paused = BotHealth {
            paused_until: Some(NOW + 45_000),
            ..health(HealthState::Paused)
        };
        assert!(
            summary(Some(&paused), NOW)
                .0
                .starts_with("Paused for less than a minute")
        );
        assert!(
            summary(Some(&health(HealthState::Other)), NOW)
                .0
                .contains("doesn't know")
        );
    }

    #[test]
    fn times_and_counts_read_plainly() {
        assert_eq!(ago(NOW, NOW), "just now");
        assert_eq!(ago(NOW - 3 * 3_600_000, NOW), "3 h ago");
        assert_eq!(ago(NOW - 2 * 86_400_000, NOW), "2 days ago");
        assert_eq!(left(NOW + 61_000, NOW), "2 min");
        assert_eq!(failed_in_a_row(1), "1 failed");
        assert_eq!(counts(Some(&health(HealthState::New))), None);
        let busy = BotHealth {
            pending: 1,
            sent: 7,
            failed: 2,
            ..health(HealthState::Healthy)
        };
        assert_eq!(
            counts(Some(&busy)).unwrap(),
            "Last day: 7 delivered, 2 failed, 1 waiting"
        );
    }

    #[test]
    fn a_test_activity_says_whether_the_bot_took_it() {
        let took = TestActivity {
            delivered: true,
            status: Some(200),
            error: None,
            elapsed_ms: 35,
        };
        assert_eq!(
            went(&Ok(took.clone())),
            ("The bot took it (200) in 35 ms.".to_owned(), true)
        );
        let refused = TestActivity {
            delivered: false,
            status: None,
            error: Some("the address is on a network the server doesn't reach".to_owned()),
            ..took
        };
        assert!(!went(&Ok(refused)).1);
        let silent = TestActivity {
            delivered: false,
            status: Some(500),
            error: None,
            elapsed_ms: 3,
        };
        assert_eq!(went(&Ok(silent)).0, "The bot didn't take it: it answered 500.");
        assert_eq!(went(&Err(AppProblem::AppDisabled)).0, "Turn the app on first.");
    }
}
