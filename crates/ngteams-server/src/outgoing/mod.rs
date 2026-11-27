//! Outgoing webhooks (`docs/server.md` "Outgoing webhooks"): a team's app that its people @mention in its standard
//! channels, served as Teams serves it.
//!
//! - **Registration.** The middle tier's `beta/teams/<team>/outgoingWebhooks` ([`registry`]). Each webhook is an app
//!   account (`users.kind` `app`, `28:<id>`) that is in no chat; its token, an HMAC key, shows once.
//! - **Delivery.** A channel message that mentions a webhook of its team puts a row in the outbox in the message's own
//!   transaction ([`enqueue!`]). Any API node sends a row, once ([`delivery`]): it posts the activity signed with
//!   HMAC-SHA256 of the exact body, through the address guard ([`crate::outbound`]), with Teams' 5 s limit, and posts
//!   the answer (text or cards that only open links) or Teams' apology as the webhook's reply in the post's thread.
//!   No token goes to a webhook, and a webhook can't call the server: Teams' webhooks send nothing proactive.

mod activity;
mod delivery;
mod registry;
pub(crate) mod schema;

use std::sync::Arc;

use axum::Router;
use serde::{Deserialize, Serialize};

use crate::app::App;

pub(crate) use activity::{Author, Mention, mentioned, text as mention_text};
pub use delivery::Senders;
pub(crate) use delivery::wake;

/// The GTM authz names while the server serves outgoing webhooks: the middle tier, where they are registered.
pub const GTM: &str = "outgoingWebhookService";

/// `[outgoing_webhooks]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OutgoingConfig {
    /// Whether the registration routes are served, mentions are sent and authz announces them.
    pub enabled: bool,
    /// Milliseconds a webhook has for its answer, from connecting to its end: Teams gives 5 seconds.
    pub timeout_ms: u64,
    /// The largest picture a webhook may have, in bytes.
    pub max_picture_bytes: usize,
}

impl Default for OutgoingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_ms: 5_000,
            max_picture_bytes: 1024 * 1024,
        }
    }
}

/// The registration routes; nothing while webhooks are off.
pub(crate) fn router(config: &crate::config::Config) -> Router<Arc<App>> {
    if !config.outgoing_webhooks.enabled {
        return Router::new();
    }
    registry::routes(config)
}

/// In a message write's transaction (`$conn`): a row for each webhook of its team that a new message `$id` (body
/// `$body`, thread post `$root`, at `$at`) by the person `$owner` in the standard channel `$chat` mentions. Anything
/// else makes none: a chat, a private channel, another team's webhook, an app's own message. Whether one was made,
/// so the caller wakes the senders after its commit.
macro_rules! enqueue {
    ($conn:expr, $chat:expr, $owner:expr, $id:expr, $root:expr, $body:expr, $at:expr) => {{
        let named = $crate::outgoing::mentioned($body);
        if named.is_empty() {
            false
        } else {
            use $crate::outgoing::schema::{outgoing_webhook_deliveries, outgoing_webhooks};
            let team: Option<String> = $crate::teams::schema::channels::table
                .find($chat)
                .filter($crate::teams::schema::channels::private.eq(false))
                .select($crate::teams::schema::channels::team_id)
                .first($conn)
                .await
                .optional()?;
            let hooks: Vec<String> = match &team {
                None => Vec::new(),
                Some(team) => {
                    outgoing_webhooks::table
                        .filter(outgoing_webhooks::team_id.eq(team))
                        .filter(outgoing_webhooks::id.eq_any(&named))
                        .select(outgoing_webhooks::id)
                        .load($conn)
                        .await?
                }
            };
            let author: Option<String> = if hooks.is_empty() {
                None
            } else {
                $crate::db::schema::users::table
                    .find($owner)
                    .filter($crate::db::users::person())
                    .select($crate::db::schema::users::display_name)
                    .first($conn)
                    .await
                    .optional()?
            };
            let rows = match (&team, &author) {
                (Some(team), Some(name)) => {
                    let mention = $crate::outgoing::Mention {
                        id: $id,
                        channel: $chat,
                        root: $root,
                        team,
                        body: $body,
                        at: $at,
                    };
                    $crate::outgoing::deliveries(&hooks, &mention, &$crate::outgoing::Author { id: $owner, name })
                }
                _ => Vec::new(),
            };
            for row in &rows {
                diesel::insert_into(outgoing_webhook_deliveries::table)
                    .values(row)
                    .execute($conn)
                    .await?;
            }
            !rows.is_empty()
        }
    }};
}
pub(crate) use enqueue;

/// The outbox rows of `mention` by `author` for `hooks`, each with its activity.
pub(crate) fn deliveries(hooks: &[String], mention: &Mention<'_>, author: &Author<'_>) -> Vec<delivery::Delivery> {
    let pending = activity::pending(mention, author);
    hooks
        .iter()
        .map(|hook| delivery::Delivery::new(hook, mention.channel, mention.root, &pending))
        .collect()
}

/// Drops sent and failed deliveries a day old.
pub(crate) async fn purge(app: &App) {
    let before = crate::chatsvc::now() - 86_400_000;
    if let Err(error) = delivery::purge(&app.db, before).await {
        tracing::warn!(%error, "old outgoing webhook deliveries weren't dropped");
    }
}
