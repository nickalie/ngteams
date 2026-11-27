//! Sign-in cards (`docs/server.md` "Bots", `docs/protocol/bots.md`): the server is a bot's token service, the
//! Bot Framework's `OAuthUrl`, for the OAuth connections an administrator gives the bot ([`connections`]).
//!
//! - **The prompt.** A bot's SDK asks its token service for the person's token (`api/usertoken/GetToken`); with
//!   none, it asks for a sign-in link (`api/botsignin/GetSignInResource`) and posts an OAuth card with it. The link
//!   names a sign-in kept here ([`store::SignIn`]) for that bot, connection and person.
//! - **The browser.** The person opens the link (`/api/oauth/signin`): the server sends them to the identity
//!   provider with a PKCE challenge, redeems the provider's code at its callback (`/api/oauth/callback`) through the
//!   address guard ([`provider`]), and shows them a six-digit code. The token stays with the sign-in.
//! - **The code.** The person's client sends it as a `signin/verifyState` invoke, which reaches the bot as any
//!   invoke; the bot redeems it (`GetToken` with `code`) for the person it names, and from then on gets that
//!   person's token, renewed with its refresh token when it runs out. Wrong codes count; five end the sign-in.
//! - **What never happens.** No token of a person's account is involved: the bot gets only the provider's token
//!   the person signed in for. Single sign-on (`signin/tokenExchange`, `api/usertoken/exchange`) is refused with
//!   `412`, so the SDKs fall back to the card.
//!
//! Every token, verifier and client secret is sealed with `bots.token_key` ([`seal`]); without it no connection is
//! made. A bot reaches its token service with its own token at the origin, or an unsigned bot under its own
//! `serviceUrl` (`/bot/u/<bot>.<credential>/api/…`).

mod browser;
mod connections;
mod provider;
pub(crate) mod schema;
mod seal;
mod store;
mod tokens;

use std::sync::Arc;

use axum::Router;

pub use seal::SealKey;
pub(crate) use seal::Sealer;
pub(crate) use tokens::serve;

use crate::app::App;
use crate::config::Config;

/// How long a sign-in link and its code work.
const SIGN_IN_LIFETIME_MS: i64 = 15 * 60 * 1000;

impl Sealer {
    /// The sealer of `bots.token_key`, when it is set.
    pub(crate) fn of(config: &Config) -> Option<Self> {
        Self::new(config.bots.token_key.as_ref()?)
    }
}

/// Now, in Unix milliseconds: the times of sign-ins and tokens.
fn now_ms() -> i64 {
    crate::chatsvc::now()
}

/// Where the identity provider sends a person back: what an administrator registers with it.
pub(crate) fn redirect_uri(config: &Config) -> String {
    format!("{}/api/oauth/callback", config.origin())
}

/// The link a sign-in card opens.
fn sign_in_link(config: &Config, id: &str) -> String {
    format!("{}/api/oauth/signin?signin={id}", config.origin())
}

/// The administrator's connection routes, within the admin API's apps.
pub(crate) fn admin_router() -> Router<Arc<App>> {
    connections::router()
}

/// The browser's sign-in routes.
pub(crate) fn router() -> Router<Arc<App>> {
    browser::router()
}

/// Drops sign-ins that ran out.
pub(crate) async fn purge(app: &App) {
    if let Err(error) = store::purge(&app.db, now_ms()).await {
        tracing::warn!(%error, "old bot sign-ins weren't dropped");
    }
}
