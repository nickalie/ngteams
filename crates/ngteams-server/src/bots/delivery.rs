//! The senders: every API node takes due rows of the outbox under a lease and posts each to its bot. A wake-up on
//! the fan-out (`bots:wake`, every node's) starts a round at once after a write; a poll covers a lost one.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::outbox::{self, Delivery, Outcome};
use crate::app::App;
use crate::outbound::OutboundError;

/// The fan-out topic a write that queued something wakes the senders on.
const WAKE: &str = "bots:wake";
/// How often a sender looks for due rows without a wake-up.
const POLL: Duration = Duration::from_secs(2);
/// The most rows one node sends at once.
const BATCH: i64 = 32;

/// Wakes every node's sender.
pub(crate) async fn wake(app: &App) {
    if app.fanout.publish(WAKE, bytes::Bytes::new()).await.is_err() {
        tracing::warn!("bot delivery wake-up unavailable; the senders' poll will find it");
    }
}

/// The sender of a node: started with its API, stopped before the node exits.
pub struct Senders {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Senders {
    /// Starts to send what the outbox holds.
    #[must_use]
    pub fn start(app: Arc<App>) -> Self {
        let (stop, stopping) = watch::channel(false);
        let task = tokio::spawn(keep_delivering(app, stopping));
        Self { stop, task }
    }

    /// A planned stop: the sender takes no more rows, and the sends under way end and their results are recorded.
    /// It waits for them at most `within` (each send ends in `bots.send_timeout`). After that it drops them, and
    /// their leases run out: another node sends them again, as after a crash.
    pub async fn stop(mut self, within: Duration) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(within, &mut self.task).await.is_err() {
            tracing::warn!("bot deliveries under way didn't end in time; another node sends them again");
        }
    }
}

impl Drop for Senders {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Sends what the outbox holds until `stopping` turns true (or its sender goes); a round under way ends first.
async fn keep_delivering(app: Arc<App>, mut stopping: watch::Receiver<bool>) {
    let node = uuid::Uuid::new_v4().simple().to_string();
    let mut wake = None;
    // when the soonest row a bot's send limit held back is due again
    let mut held_until: Option<tokio::time::Instant> = None;
    loop {
        if wake.is_none() {
            // a closed fan-out closes every subscription: subscribe again, and poll meanwhile
            wake = app.fanout.subscribe(WAKE).await.ok();
        }
        while !*stopping.borrow() {
            let done = round(&app, &node).await;
            if let Some(held) = done.held {
                let due = tokio::time::Instant::now() + held;
                held_until = Some(held_until.map_or(due, |soonest| soonest.min(due)));
            }
            if done.sent == 0 {
                break;
            }
        }
        if *stopping.borrow() {
            return;
        }
        let now = tokio::time::Instant::now();
        held_until = held_until.filter(|due| *due > now);
        let nap = held_until.map_or(POLL, |due| (due - now).min(POLL));
        let woken = async {
            match wake.as_mut() {
                Some(subscription) => !matches!(subscription.recv().await, Err(crate::fanout::Missed::Closed)),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            open = woken => {
                if !open {
                    wake = None;
                }
            }
            () = tokio::time::sleep(nap) => {}
            _ = stopping.wait_for(|stopped| *stopped) => return,
        }
    }
}

/// What a round did.
struct Round {
    /// The rows sent (whatever the bot answered).
    sent: usize,
    /// The shortest wait of a row its bot's send limit held back, when one was.
    held: Option<Duration>,
}

/// One round: the rows this node took, each sent unless its bot's send limit holds it back.
async fn round(app: &App, node: &str) -> Round {
    let lease = i64::try_from(app.config.bots.delivery_lease)
        .unwrap_or(i64::MAX / 2_000)
        .saturating_mul(1000);
    let claimed = match outbox::claim(&app.db, node, lease, BATCH).await {
        Ok(claimed) => claimed,
        Err(error) => {
            tracing::warn!(%error, "bot deliveries couldn't be taken");
            return Round { sent: 0, held: None };
        }
    };
    let (due, held) = within_limits(app, node, claimed).await;
    let sent = due.len();
    futures_util::future::join_all(due.into_iter().map(|row| send(app, node, row))).await;
    Round { sent, held }
}

/// The rows whose bots' send limits let them go now; the others go back to the outbox, due when their bot has room,
/// and the shortest of those waits.
async fn within_limits(app: &App, node: &str, claimed: Vec<Delivery>) -> (Vec<Delivery>, Option<Duration>) {
    let mut held: Option<Duration> = None;
    let mut due = Vec::new();
    for row in claimed {
        let Some(wait) = app.bot_sends.take(&row.bot_id) else {
            due.push(row);
            continue;
        };
        held = Some(held.map_or(wait, |shortest| shortest.min(wait)));
        let delay = i64::try_from(wait.as_millis())
            .unwrap_or(i64::MAX / 2)
            .saturating_add(1);
        if let Err(error) = outbox::defer(&app.db, node, &row, delay).await {
            tracing::warn!(%error, "a bot delivery held by its send limit wasn't given back; its lease runs out");
        }
    }
    (due, held)
}

async fn send(app: &App, node: &str, row: Delivery) {
    let outcome = attempt(app, &row).await;
    log(&row, &outcome);
    if let Err(error) = outbox::finish(&app.db, node, &row, &outcome).await {
        tracing::warn!(%error, "a bot delivery's outcome wasn't kept; its lease runs out and it is sent again");
    }
}

fn log(row: &Delivery, outcome: &Outcome) {
    if let Outcome::Failed(error) = outcome {
        tracing::info!(bot = %row.bot_id, kind = activity_kind(row), %error, "an activity to a bot failed for good");
    }
}

fn activity_kind(row: &Delivery) -> String {
    serde_json::from_str::<Value>(&row.activity)
        .ok()
        .and_then(|activity| activity["type"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

async fn attempt(app: &App, row: &Delivery) -> Outcome {
    let bot = match super::bot(&app.db, &row.bot_id).await {
        Ok(Some(bot)) => bot,
        Ok(None) => return Outcome::Failed("the bot is no longer registered".to_owned()),
        Err(error) => return retry(app, row, error.to_string()),
    };
    let Some(endpoint) = bot.endpoint.as_deref().filter(|_| bot.enabled) else {
        return Outcome::Failed("the bot has no endpoint or its app is off".to_owned());
    };
    let Ok(mut activity) = serde_json::from_str::<Value>(&row.activity) else {
        return Outcome::Failed("the stored activity is unreadable".to_owned());
    };
    let service_url = super::service_url(app, &bot);
    super::activity::addressed(&mut activity, &service_url, app.issuer.tenant());
    let token = if bot.signed {
        match app.bot_keys.activity_token(&bot.id, &service_url) {
            Ok(token) => Some(token),
            Err(error) => return retry(app, row, error.to_string()),
        }
    } else {
        None
    };
    let body = activity.to_string().into_bytes();
    match app.outbound.post_json(endpoint, token.as_deref(), body).await {
        Ok(answer) if (200..300).contains(&answer.status) => Outcome::Sent,
        Ok(answer) => retry(app, row, format!("the bot answered {}", answer.status)),
        Err(error @ (OutboundError::Url | OutboundError::Refused)) => Outcome::Failed(error.to_string()),
        Err(error) => retry(app, row, error.to_string()),
    }
}

/// A failed attempt: another after its delay, while attempts are left.
fn retry(app: &App, row: &Delivery, error: String) -> Outcome {
    let delays = &app.config.bots.retry_delays;
    let most = if row.max_attempts > 0 {
        usize::try_from(row.max_attempts).unwrap_or(1)
    } else {
        delays.len() + 1
    };
    let made = usize::try_from(row.attempts).unwrap_or(usize::MAX);
    if made >= most {
        return Outcome::Failed(error);
    }
    let delay = delays.get(made.saturating_sub(1)).copied().unwrap_or(0);
    Outcome::Retry(
        i64::try_from(delay).unwrap_or(i64::MAX / 2_000).saturating_mul(1000),
        error,
    )
}
