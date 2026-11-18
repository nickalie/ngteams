//! The senders: every API node takes due rows of the outbox and posts each to its webhook once. A row goes from
//! `pending` to `sending` by a compare-and-set update, so one node sends it; the node that still holds it when the
//! request ends commits its replies and terminal state together, also by compare-and-set. Thus a mention gets one
//! request and one reply at most. A row whose node died stays `sending` until its lease runs out: the sweep then
//! fails it with Teams' apology and sends nothing again.

use std::sync::Arc;
use std::time::Duration;

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::activity;
use super::schema::outgoing_webhook_deliveries as rows;
use crate::app::App;
use crate::db::{Db, DbError, with_conn};

/// The fan-out topic a post that queued a mention wakes the senders on.
const WAKE: &str = "webhooks:wake";
/// How often a sender looks for due rows without a wake-up.
const POLL: Duration = Duration::from_secs(1);
/// The most rows one node sends at once.
const BATCH: i64 = 32;
/// How long a node holds a row beyond the request's own limit.
const LEASE_MARGIN: i64 = 10_000;
/// A row not sent within this many milliseconds of its message isn't sent, and gets no reply: Teams asks while the
/// person waits, and only stopped senders (webhooks turned off) leave a row so long.
const STALE: i64 = 60_000;

pub(crate) const PENDING: &str = "pending";
const SENDING: &str = "sending";
const DONE: &str = "done";
const FAILED: &str = "failed";

/// What Teams posts as the webhook when its service failed or didn't answer in time.
pub(crate) const APOLOGY: &str = "Sorry, there was a problem encountered with your request";

#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = rows)]
pub(crate) struct Delivery {
    pub id: String,
    pub webhook_id: String,
    pub chat_id: String,
    pub root_id: i64,
    pub activity: String,
    pub state: String,
    pub locked_by: Option<String>,
    pub locked_until: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Delivery {
    pub(crate) fn new(webhook: &str, chat: &str, root: i64, activity: &Value) -> Self {
        let at = crate::chatsvc::now();
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            webhook_id: webhook.to_owned(),
            chat_id: chat.to_owned(),
            root_id: root,
            activity: activity.to_string(),
            state: PENDING.to_owned(),
            locked_by: None,
            locked_until: 0,
            last_error: None,
            created_at: at,
            updated_at: at,
        }
    }
}

/// Wakes every node's sender.
pub(crate) async fn wake(app: &App) {
    if app.fanout.publish(WAKE, bytes::Bytes::new()).await.is_err() {
        tracing::warn!("outgoing webhook wake-up unavailable; the senders' poll will find it");
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

    /// A planned stop: the sender takes no more rows; the requests under way end (each in its limit) and their
    /// replies are posted, for at most `within`. A row dropped after that fails at the sweep, unsent again.
    pub async fn stop(mut self, within: Duration) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(within, &mut self.task).await.is_err() {
            tracing::warn!("outgoing webhook requests under way didn't end in time; the sweep fails them");
        }
    }
}

impl Drop for Senders {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn keep_delivering(app: Arc<App>, mut stopping: watch::Receiver<bool>) {
    let node = uuid::Uuid::new_v4().simple().to_string();
    let mut wake = None;
    loop {
        if wake.is_none() {
            // a closed fan-out closes every subscription: subscribe again, and poll meanwhile
            wake = app.fanout.subscribe(WAKE).await.ok();
        }
        sweep(&app).await;
        while !*stopping.borrow() && round(&app, &node).await > 0 {}
        if *stopping.borrow() {
            return;
        }
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
            () = tokio::time::sleep(POLL) => {}
            _ = stopping.wait_for(|stopped| *stopped) => return,
        }
    }
}

/// One round: the rows this node took, each sent; how many.
async fn round(app: &App, node: &str) -> usize {
    let lease = timeout_ms(app) + LEASE_MARGIN;
    let claimed = match claim(&app.db, node, lease).await {
        Ok(claimed) => claimed,
        Err(error) => {
            tracing::warn!(%error, "outgoing webhook deliveries couldn't be taken");
            return 0;
        }
    };
    let count = claimed.len();
    futures_util::future::join_all(claimed.into_iter().map(|row| send(app, node, row))).await;
    count
}

fn timeout_ms(app: &App) -> i64 {
    i64::try_from(app.config.outgoing_webhooks.timeout_ms).unwrap_or(i64::MAX / 4)
}

/// Up to [`BATCH`] pending rows, oldest first, that this node took.
async fn claim(db: &Db, node: &str, lease: i64) -> Result<Vec<Delivery>, DbError> {
    let now = crate::chatsvc::now();
    let candidates: Vec<Delivery> = with_conn!(db, |conn| rows::table
        .filter(rows::state.eq(PENDING))
        .order((rows::created_at.asc(), rows::id.asc()))
        .limit(BATCH)
        .select(Delivery::as_select())
        .load(conn)
        .await?);
    let mut claimed = Vec::new();
    for candidate in candidates {
        if take(db, &candidate.id, PENDING, (SENDING, Some(node), now + lease), now).await? {
            claimed.push(candidate);
        }
    }
    Ok(claimed)
}

/// Moves row `id` from state `from` to `to` (its state, holder and lease) unless another node moved it first;
/// whether this call did.
async fn take(db: &Db, id: &str, from: &str, to: (&str, Option<&str>, i64), now: i64) -> Result<bool, DbError> {
    let (state, holder, until) = to;
    let taken = with_conn!(db, |conn| diesel::update(
        rows::table.find(id).filter(rows::state.eq(from))
    )
    .set((
        rows::state.eq(state),
        rows::locked_by.eq(holder),
        rows::locked_until.eq(until),
        rows::updated_at.eq(now),
    ))
    .execute(conn)
    .await?);
    Ok(taken == 1)
}

/// How a request ended.
#[derive(Debug)]
enum Outcome {
    /// The answer: an activity, or none.
    Answered(Option<Value>),
    Failed(String),
    /// Not sent: it waited too long (the senders were off), and nobody waits for its reply now.
    Stale,
}

async fn send(app: &App, node: &str, row: Delivery) {
    let outcome = if crate::chatsvc::now() - row.created_at > STALE {
        Outcome::Stale
    } else {
        attempt(app, &row).await
    };
    ended(app, node, &row, &outcome).await;
}

/// Ends a delivery in the caller's transaction only if this node still holds it.
macro_rules! finish_in {
    ($conn:expr, $node:expr, $id:expr, $state:expr, $error:expr) => {
        diesel::update(
            rows::table
                .find($id)
                .filter(rows::state.eq(SENDING))
                .filter(rows::locked_by.eq($node)),
        )
        .set((
            rows::state.eq($state),
            rows::locked_until.eq(0),
            rows::last_error.eq($error),
            rows::updated_at.eq(crate::chatsvc::now()),
        ))
        .execute($conn)
        .await?
            == 1
    };
}

/// Commits the replies and the terminal state together, while this node still holds the delivery.
async fn ended(app: &App, node: &str, row: &Delivery, outcome: &Outcome) {
    let (state, error) = match outcome {
        Outcome::Answered(_) => (DONE, None),
        Outcome::Failed(error) => (FAILED, Some(error.as_str())),
        Outcome::Stale => (FAILED, Some("the mention waited too long to be sent")),
    };
    if let Err(error) = complete(app, node, row, state, error, outcome).await {
        unkept(&error);
    }
}

async fn complete(
    app: &App,
    node: &str,
    row: &Delivery,
    state: &str,
    error: Option<&str>,
    outcome: &Outcome,
) -> Result<(), DbError> {
    let made = replies(outcome, &app.config.origin());
    let hook = super::registry::hook(&app.db, &row.webhook_id).await?;
    let posted = with_conn!(&app.db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            if !finish_in!(conn, node, &row.id, state, error) {
                return Ok(None);
            }
            let mut posted = Vec::new();
            if let Some(hook) = &hook {
                for (kind, content) in &made {
                    let post = crate::chatsvc::service::AppPost {
                        thread: &row.chat_id,
                        app: &row.webhook_id,
                        name: &hook.name,
                        kind,
                        content,
                        properties: serde_json::Map::new(),
                    };
                    if let Some(reply) = crate::chatsvc::service::reply_in(conn, app, post, row.root_id).await? {
                        posted.push(reply);
                    }
                }
            }
            Ok(Some(posted))
        })
        .await)?;
    let Some(posted) = posted else {
        return Ok(());
    };
    log_failure(row, error);
    for post in posted {
        crate::chatsvc::service::announce_posted(app, &post).await?;
    }
    Ok(())
}

fn log_failure(row: &Delivery, error: Option<&str>) {
    if let Some(error) = error {
        tracing::info!(webhook = %row.webhook_id, %error, "a request to an outgoing webhook failed");
    }
}

fn unkept(error: &DbError) {
    tracing::warn!(%error, "an outgoing webhook delivery's end wasn't kept; the sweep fails it");
}

#[cfg(test)]
async fn finish(db: &Db, node: &str, id: &str, state: &str, error: Option<&str>) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(finish_in!(conn, node, id, state, error)))
}

async fn attempt(app: &App, row: &Delivery) -> Outcome {
    let hook = match super::registry::hook(&app.db, &row.webhook_id).await {
        Ok(Some(hook)) => hook,
        Ok(None) => return Outcome::Answered(None),
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    let Ok(mut activity) = serde_json::from_str::<Value>(&row.activity) else {
        return Outcome::Failed("the stored activity is unreadable".to_owned());
    };
    activity::addressed(
        &mut activity,
        &crate::apps::service_url(&app.config),
        app.issuer.tenant(),
    );
    let body = activity.to_string().into_bytes();
    let authorization = activity::sign(&hook.key(), &body);
    let limit = Duration::from_millis(app.config.outgoing_webhooks.timeout_ms);
    match app
        .outbound
        .post_signed(&hook.callback_url, &authorization, body, limit)
        .await
    {
        Ok(answer) if (200..300).contains(&answer.status) => read(&answer.body),
        Ok(answer) => Outcome::Failed(format!("the webhook answered {}", answer.status)),
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

/// An answer's body: nothing, or an activity.
fn read(body: &[u8]) -> Outcome {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Outcome::Answered(None);
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(answer) if answer.is_object() => Outcome::Answered(Some(answer)),
        _ => Outcome::Failed("the answer isn't an activity".to_owned()),
    }
}

/// The messages `outcome` makes: the answer's, Teams' apology, or none.
fn replies(outcome: &Outcome, origin: &str) -> Vec<(&'static str, String)> {
    use crate::bots::activity::{Refused, messages};
    match outcome {
        Outcome::Answered(None) | Outcome::Stale => Vec::new(),
        Outcome::Answered(Some(answer)) => match messages(&activity::kept(answer.clone()), origin, false) {
            Ok(made) => made.into_iter().map(|made| (made.kind, made.content)).collect(),
            Err(Refused::Empty) => Vec::new(),
            Err(Refused::Attachment) => vec![apology()],
        },
        Outcome::Failed(_) => vec![apology()],
    }
}

fn apology() -> (&'static str, String) {
    (crate::bots::activity::HTML, format!("<p>{APOLOGY}</p>"))
}

/// Fails the rows whose node died while it sent them (their lease ran out), with Teams' apology; nothing is sent
/// again.
async fn sweep(app: &App) {
    let now = crate::chatsvc::now();
    let stuck: Vec<Delivery> = match stuck(&app.db, now).await {
        Ok(stuck) => stuck,
        Err(error) => {
            tracing::warn!(%error, "outgoing webhook deliveries couldn't be swept");
            return;
        }
    };
    for row in stuck {
        fail_stuck(app, &row).await;
    }
}

async fn fail_stuck(app: &App, row: &Delivery) {
    let Some(node) = row.locked_by.as_deref() else {
        tracing::warn!(delivery = %row.id, "an outgoing webhook delivery has no sender");
        return;
    };
    ended(app, node, row, &Outcome::Failed("the sender lease expired".to_owned())).await;
}

async fn stuck(db: &Db, now: i64) -> Result<Vec<Delivery>, DbError> {
    with_conn!(db, |conn| Ok(rows::table
        .filter(rows::state.eq(SENDING))
        .filter(rows::locked_until.lt(now))
        .limit(BATCH)
        .select(Delivery::as_select())
        .load(conn)
        .await?))
}

/// Drops sent and failed rows last changed before `before`.
pub(crate) async fn purge(db: &Db, before: i64) -> Result<usize, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        rows::table
            .filter(rows::state.eq_any([DONE, FAILED]))
            .filter(rows::updated_at.lt(before)),
    )
    .execute(conn)
    .await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_answer_is_none_and_one_that_is_no_activity_fails() {
        assert!(matches!(read(b""), Outcome::Answered(None)));
        assert!(matches!(read(b" \n"), Outcome::Answered(None)));
        assert!(matches!(read(br#"{"text":"x"}"#), Outcome::Answered(Some(_))));
        assert!(matches!(read(b"[1]"), Outcome::Failed(_)));
        assert!(matches!(read(b"nope"), Outcome::Failed(_)));
    }

    async fn sqlite() -> Result<(tempfile::TempDir, Db), DbError> {
        use diesel_async::SimpleAsyncConnection as _;
        let dir = tempfile::tempdir().map_err(|error| DbError::Pool(error.to_string()))?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        // the rows' webhooks aren't registered: this tests only the outbox's own updates
        with_conn!(db, |conn| conn.batch_execute("PRAGMA foreign_keys = OFF").await?);
        Ok((dir, db))
    }

    async fn put(db: &Db, row: &Delivery) -> Result<(), DbError> {
        with_conn!(db, |conn| diesel::insert_into(rows::table)
            .values(row)
            .execute(conn)
            .await?);
        Ok(())
    }

    async fn state(db: &Db, id: &str) -> Result<String, DbError> {
        with_conn!(db, |conn| Ok(rows::table
            .find(id)
            .select(rows::state)
            .first(conn)
            .await?))
    }

    #[tokio::test]
    async fn one_node_takes_a_row_and_only_the_holder_ends_it() -> Result<(), DbError> {
        let (_dir, db) = sqlite().await?;
        let row = Delivery::new("w", "19:c@thread.tacv2", 5, &serde_json::json!({"type": "message"}));
        put(&db, &row).await?;
        let first = claim(&db, "a", 60_000).await?;
        let second = claim(&db, "b", 60_000).await?;
        assert_eq!((first.len(), second.len()), (1, 0));
        assert!(!finish(&db, "b", &row.id, DONE, None).await?);
        assert!(finish(&db, "a", &row.id, DONE, None).await?);
        assert_eq!(state(&db, &row.id).await?, DONE);
        let now = crate::chatsvc::now();
        assert_eq!(purge(&db, now + 1).await?, 1);
        Ok(())
    }
    #[tokio::test]
    async fn replies_and_the_terminal_state_commit_together() -> Result<(), Box<dyn std::error::Error>> {
        use diesel_async::SimpleAsyncConnection as _;
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = LogWriter(Arc::clone(&log));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let dir = tempfile::tempdir()?;
        let config = crate::config::Config {
            database_url: format!("sqlite://{}", dir.path().join("server.sqlite").display()),
            database_pool: 1,
            ..crate::config::Config::default()
        };
        let app = App::start(config).await?;
        with_conn!(&app.db, |conn| {
            conn.batch_execute("PRAGMA foreign_keys = OFF;
            INSERT INTO chats (id, creator, unique_roster, created_at, version, roster_version)
                VALUES ('channel', 'author', 0, 0, 0, 0);
            INSERT INTO outgoing_webhooks
                (id, team_id, name, name_key, description, callback_url, security_key, created_by, created_at, updated_at)
                VALUES ('hook', 'team', 'Synthetic hook', 'synthetic hook', '', 'https://example.test', '', 'author', 0, 0);
            CREATE TRIGGER refuse_card BEFORE INSERT ON chat_messages
                WHEN NEW.body LIKE '%RichText/Media_Card%'
                BEGIN SELECT RAISE(ABORT, 'synthetic card failure'); END;").await?;
        });
        let row = Delivery::new("hook", "channel", 1, &serde_json::json!({}));
        put(&app.db, &row).await?;
        claim(&app.db, "node", -1).await?;
        let failure = Outcome::Failed("synthetic failure".to_owned());
        complete(&app, "other", &row, FAILED, Some("synthetic failure"), &failure).await?;
        assert_eq!(failure_logs(&log), 0);
        let answer = Outcome::Answered(Some(serde_json::json!({"type": "message", "text": "text before card",
            "attachments": [{"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "card"}}]})));
        assert!(complete(&app, "node", &row, DONE, None, &answer).await.is_err());
        assert_eq!(state(&app.db, &row.id).await?, SENDING);
        assert_eq!(message_count(&app.db).await?, 0);
        with_conn!(&app.db, |conn| conn.batch_execute("DROP TRIGGER refuse_card").await?);
        sweep(&app).await;
        assert_eq!(state(&app.db, &row.id).await?, FAILED);
        assert_eq!(message_count(&app.db).await?, 1);
        assert_eq!(failure_logs(&log), 1);
        complete(&app, "node", &row, FAILED, Some("synthetic failure"), &failure).await?;
        assert_eq!(failure_logs(&log), 1);
        complete(&app, "node", &row, DONE, None, &answer).await?;
        assert_eq!(message_count(&app.db).await?, 1);
        let next = Delivery::new("hook", "channel", 1, &serde_json::json!({}));
        put(&app.db, &next).await?;
        claim(&app.db, "node", 60_000).await?;
        complete(&app, "node", &next, DONE, None, &answer).await?;
        assert_eq!(state(&app.db, &next.id).await?, DONE);
        assert_eq!(message_count(&app.db).await?, 3);
        Ok(())
    }

    #[derive(Clone)]
    struct LogWriter(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn failure_logs(log: &std::sync::Mutex<Vec<u8>>) -> usize {
        String::from_utf8_lossy(&log.lock().unwrap())
            .matches("a request to an outgoing webhook failed")
            .count()
    }

    async fn message_count(db: &Db) -> Result<i64, DbError> {
        with_conn!(db, |conn| Ok(crate::chatsvc::schema::chat_messages::table
            .count()
            .get_result(conn)
            .await?))
    }
}
