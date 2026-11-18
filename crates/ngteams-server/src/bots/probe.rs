//! The administrator's test activity (Manage apps, "Send a test activity"): one `event` activity named
//! `ngteams/testActivity` to a bot's registered endpoint, from the administrator in their 1:1 chat with it, as any
//! activity goes: through the address guard ([`crate::outbound`]), signed with the server's activity token when the
//! bot is signed, within the bot's send limit. It is no chat event: no outbox row, and the bot's health stays as its
//! deliveries left it. No account's token is in it.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Bot;
use crate::app::App;
use crate::db::DbError;
use crate::db::users::User;

/// The name of the test activity's `event`.
pub(crate) const NAME: &str = "ngteams/testActivity";

/// How the test activity went.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Probe {
    /// The bot answered 2xx.
    pub delivered: bool,
    /// The bot's status, when it answered.
    pub status: Option<u16>,
    /// Why it wasn't delivered.
    pub error: Option<String>,
    pub elapsed: Duration,
}

impl Probe {
    pub(crate) fn json(&self) -> Value {
        json!({
            "delivered": self.delivered,
            "status": self.status,
            "error": self.error,
            "elapsed_ms": u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
        })
    }
}

/// Why no test activity was sent.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProbeError {
    #[error("no such app")]
    NoSuchApp,
    #[error("the app has no bot")]
    NoBot,
    #[error("the bot has no messaging endpoint")]
    NoEndpoint,
    #[error("the app is turned off")]
    Disabled,
    /// The bot's send limit: the wait.
    #[error("the bot's send limit holds it")]
    Limited(Duration),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("the activity's token can't be made: {0}")]
    Token(String),
}

/// Sends the test activity of `admin` to the bot of app `id`.
///
/// # Errors
/// See [`ProbeError`]; a bot that answers an error, or that the guard refuses, is a [`Probe`] not delivered.
pub(crate) async fn test_activity(app: &App, id: &str, admin: &User) -> Result<Probe, ProbeError> {
    let record = crate::apps::repo::get(&app.db, id)
        .await?
        .ok_or(ProbeError::NoSuchApp)?;
    let row = record.bot.ok_or(ProbeError::NoBot)?;
    let bot = super::bot(&app.db, &row.id).await?.ok_or(ProbeError::NoBot)?;
    let endpoint = bot.endpoint.clone().ok_or(ProbeError::NoEndpoint)?;
    if !bot.enabled {
        return Err(ProbeError::Disabled);
    }
    if let Some(wait) = app.bot_sends.take_now(&bot.id) {
        return Err(ProbeError::Limited(wait));
    }
    let service_url = super::service_url(app, &bot);
    let activity = activity(app, &bot, admin, &service_url);
    let token = if bot.signed {
        let token = app
            .bot_keys
            .activity_token(&bot.id, &service_url)
            .map_err(|error| ProbeError::Token(error.to_string()))?;
        Some(token)
    } else {
        None
    };
    let started = Instant::now();
    let sent = app
        .outbound
        .post_json(&endpoint, token.as_deref(), activity.to_string().into_bytes())
        .await;
    let probe = match sent {
        Ok(answer) if (200..300).contains(&answer.status) => Probe {
            delivered: true,
            status: Some(answer.status),
            error: None,
            elapsed: started.elapsed(),
        },
        Ok(answer) => Probe {
            delivered: false,
            status: Some(answer.status),
            error: Some(format!("the bot answered {}", answer.status)),
            elapsed: started.elapsed(),
        },
        Err(error) => Probe {
            delivered: false,
            status: None,
            error: Some(error.to_string()),
            elapsed: started.elapsed(),
        },
    };
    tracing::info!(bot = %bot.id, admin = %admin.id, delivered = probe.delivered, status = ?probe.status,
        "a test activity went to a bot");
    Ok(probe)
}

/// The test activity, addressed.
fn activity(app: &App, bot: &Bot, admin: &User, service_url: &str) -> Value {
    let now = crate::chatsvc::now();
    let time = crate::chatsvc::iso(now);
    let mut activity = json!({
        "type": "event", "name": NAME,
        "id": format!("test-{}", uuid::Uuid::new_v4().simple()),
        "timestamp": time, "localTimestamp": time, "channelId": crate::apps::keys::CHANNEL,
        "locale": super::activity::LOCALE,
        "from": {"id": format!("29:{}", admin.id), "name": admin.display_name, "aadObjectId": admin.id},
        "recipient": {"id": format!("28:{}", bot.id), "name": bot.name},
        "conversation": {"id": super::personal_chat(&admin.id, &bot.id), "conversationType": "personal",
            "isGroup": false},
        "channelData": {},
        "value": {"sentAt": time},
    });
    super::activity::addressed(&mut activity, service_url, app.issuer.tenant());
    activity
}
