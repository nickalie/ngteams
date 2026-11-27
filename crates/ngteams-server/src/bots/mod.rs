//! The Bot Connector (`docs/server.md` "Bots"): the server is the `msteams` channel for the bots registered in it
//! (`crate::apps`).
//!
//! - **To a bot.** A chat event a bot should get is a row of the outbox ([`outbox`]), written in the chat write's own
//!   transaction; every API node runs its [`Senders`], which takes rows under a lease and posts each as an
//!   activity to the bot's endpoint through the address guard ([`crate::outbound`]). A signed bot's activity carries
//!   the server's RS256 token (`crate::apps::keys`); an unsigned bot's carries none, and its own `serviceUrl` holds
//!   its credential instead. No account's token ever enters an activity.
//! - **From a bot.** The connector routes (`/bot/v3/…`, [`connector`]) take a bot's own token (or its unsigned
//!   `serviceUrl`) and post, change and delete its messages as the chat member it is, through the chat service's own
//!   write path, so every reader and sync sees them as any message.
//! - **Bot chats.** A bot is a principal in chats (a `users` row of kind `app`, `28:<id>`). A person's first message
//!   to `19:<person>_<bot>@unq.gbl.spaces` makes their 1:1 chat when the person may use the bot's app; a bot makes it
//!   with `POST v3/conversations` under the same rule.

pub(crate) mod activity;
mod connector;
mod delivery;
pub(crate) mod health;
mod invoke;
mod limits;
pub(crate) mod outbox;
mod pictures;
mod probe;
pub mod sign_in;

pub use outbox::{Delivery, FAILED, PENDING, SENT, deliveries};
pub(crate) mod schema;

use std::sync::Arc;

use axum::Router;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use crate::app::App;
use crate::apps::schema::{app_users, apps, bots};
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};

pub use delivery::Senders;
pub(crate) use delivery::wake;
pub use limits::SendLimit;
pub(crate) use limits::retry_after;
pub(crate) use probe::{ProbeError, test_activity};

/// A registered bot, as delivery and the connector need it.
#[derive(Clone, Debug)]
pub(crate) struct Bot {
    pub id: String,
    pub app_id: String,
    pub name: String,
    pub endpoint: Option<String>,
    pub signed: bool,
    pub secret_hash: String,
    /// Its app is enabled.
    pub enabled: bool,
}

/// The bot `id`, when one is registered.
pub(crate) async fn bot(db: &Db, id: &str) -> Result<Option<Bot>, DbError> {
    type Row = (String, String, Option<String>, String, String, bool, String);
    let found: Option<Row> = with_conn!(db, |conn| bots::table
        .inner_join(apps::table)
        .filter(bots::id.eq(id))
        .select((
            bots::id,
            bots::app_id,
            bots::endpoint,
            bots::auth_mode,
            bots::secret_hash,
            apps::enabled,
            apps::name,
        ))
        .first(conn)
        .await
        .optional()?);
    let Some((id, app_id, endpoint, mode, secret_hash, enabled, app_name)) = found else {
        return Ok(None);
    };
    let name: Option<String> = with_conn!(db, |conn| users::table
        .find(&id)
        .select(users::display_name)
        .first(conn)
        .await
        .optional()?);
    Ok(Some(Bot {
        name: name.unwrap_or(app_name),
        id,
        app_id,
        endpoint,
        signed: mode != crate::apps::AuthMode::Unsigned.as_str(),
        secret_hash,
        enabled,
    }))
}

/// Whether `user` may use `bot`'s app: it is enabled and for everyone or for them.
pub(crate) async fn may_use(db: &Db, bot: &Bot, user: &str) -> Result<bool, DbError> {
    if !bot.enabled {
        return Ok(false);
    }
    let everyone: bool = with_conn!(db, |conn| apps::table
        .find(&bot.app_id)
        .select(apps::everyone)
        .first(conn)
        .await
        .optional()?)
    .unwrap_or(false);
    if everyone {
        return Ok(true);
    }
    let listed: i64 = with_conn!(db, |conn| app_users::table
        .filter(app_users::app_id.eq(&bot.app_id))
        .filter(app_users::user_id.eq(user))
        .count()
        .get_result(conn)
        .await?);
    Ok(listed > 0)
}

/// The `serviceUrl` a bot is given: the server's, or for an unsigned bot its own, which carries its credential.
pub(crate) fn service_url(app: &App, bot: &Bot) -> String {
    let base = crate::apps::service_url(&app.config);
    if bot.signed {
        base
    } else {
        format!("{base}u/{}.{}/", bot.id, unsigned_credential(bot))
    }
}

/// An unsigned bot's credential: derived from its secret's digest, so a new secret makes a new one.
fn unsigned_credential(bot: &Bot) -> String {
    crate::apps::digest(&format!("unsigned:{}", bot.secret_hash))
}

/// The 1:1 chat of `person` and `bot`, as Teams names it: `19:<a>_<b>@unq.gbl.spaces`, the ids in order.
pub(crate) fn personal_chat(person: &str, bot: &str) -> String {
    let mut ids = [person, bot];
    ids.sort_unstable();
    format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1])
}

/// Before a person's message to `chat`: when it names their 1:1 chat with a bot whose app they may use
/// (`19:<a>_<b>@unq.gbl.spaces`, the ids in order) and the chat isn't there yet, it is made, as Teams makes a bot
/// chat with its first message.
pub(crate) async fn open_chat(app: &App, chat: &str, person: &str) -> Result<(), DbError> {
    let Some(pair) = chat
        .strip_prefix("19:")
        .and_then(|rest| rest.strip_suffix("@unq.gbl.spaces"))
    else {
        return Ok(());
    };
    let Some((first, second)) = pair.split_once('_') else {
        return Ok(());
    };
    let other = match (first == person, second == person) {
        (true, false) => second,
        (false, true) => first,
        _ => return Ok(()),
    };
    if first >= second || !app.config.apps.enabled {
        return Ok(());
    }
    let Some(bot) = bot(&app.db, other).await? else {
        return Ok(());
    };
    if may_use(&app.db, &bot, person).await? {
        crate::chatsvc::service::bot_chat(app, person, &bot.id).await?;
    }
    Ok(())
}

/// The connector routes; nothing while apps are off.
pub(crate) fn router(app: &Arc<App>) -> Router<Arc<App>> {
    if !app.config.apps.enabled {
        return Router::new();
    }
    connector::router(app)
        .merge(invoke::router())
        .merge(sign_in::router())
}

/// Typing by `user` in `chat` at the chat service's moment `at`, for the bots in it.
pub(crate) async fn typing(app: &App, chat: &str, user: &str, at: i64) {
    match queue_typing(&app.db, chat, user, at).await {
        Ok(true) => wake(app).await,
        Ok(false) => {}
        Err(error) => tracing::warn!(%error, "typing wasn't queued for the chat's bots"),
    }
}

async fn queue_typing(db: &Db, chat: &str, user: &str, at: i64) -> Result<bool, DbError> {
    use diesel_async::AsyncConnection as _;
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| Ok(outbox::enqueue!(
            conn,
            chat,
            user,
            activity::Event::Typing { at },
            at
        )))
        .await)
}

/// Drops delivered and failed activities a day old.
pub(crate) async fn purge(app: &App) {
    let before = crate::chatsvc::now() - 86_400_000;
    if let Err(error) = outbox::purge(&app.db, before).await {
        tracing::warn!(%error, "old bot deliveries weren't dropped");
    }
    sign_in::purge(app).await;
}
