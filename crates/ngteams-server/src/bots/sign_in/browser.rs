//! The person's browser in a sign-in: the link a sign-in card opens sends it on to the identity provider, and the
//! provider's callback ends at a page that shows the person their code. The pages run no script, load nothing,
//! are never cached, framed or sent a referrer.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use super::connections;
use super::store::{self, SignIn};
use super::{Sealer, provider, redirect_uri, tokens};
use crate::app::App;
use crate::db::DbError;

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/api/oauth/signin", get(start))
        .route("/api/oauth/callback", get(callback))
}

#[derive(Deserialize)]
struct Start {
    signin: Option<String>,
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

type Running = (SignIn, connections::Connection, Sealer);

/// The sign-in a link or a callback names, with its connection and the sealer, while it runs and has no token yet.
async fn running(app: &App, id: Option<&str>) -> Result<Option<Running>, DbError> {
    let (Some(sealer), Some(id)) = (Sealer::of(&app.config), id) else {
        return Ok(None);
    };
    let found = store::running(&app.db, id).await?;
    Ok(found.map(|(found, connection)| (found, connection, sealer)))
}

/// The page for a sign-in that doesn't run, or that the database couldn't find.
fn not_running(error: Option<DbError>) -> Response {
    match error {
        Some(error) => unavailable(&error),
        None => page(StatusCode::NOT_FOUND, GONE_TITLE, GONE_TEXT, None),
    }
}

fn unavailable(error: &DbError) -> Response {
    tracing::error!(%error, "bot sign-in database failure");
    page(StatusCode::SERVICE_UNAVAILABLE, FAILED_TITLE, BUSY_TEXT, None)
}

/// `GET /api/oauth/signin?signin=<id>`: on to the provider.
async fn start(State(app): State<Arc<App>>, Query(query): Query<Start>) -> Response {
    let (found, connection, sealer) = match running(&app, query.signin.as_deref()).await {
        Ok(Some(running)) => running,
        other => return not_running(other.err()),
    };
    let opened = (
        sealer.open(&verifier_context(&found), &found.verifier),
        connection.secret(&sealer),
    );
    let (Ok(verifier), Some(secret)) = opened else {
        tracing::warn!(connection = %connection.name, "a sign-in's sealed values didn't open");
        return page(StatusCode::INTERNAL_SERVER_ERROR, FAILED_TITLE, FAILED_TEXT, None);
    };
    let url = provider::authorize_url(
        &connection,
        secret,
        &redirect_uri(&app.config),
        &found.id,
        &String::from_utf8_lossy(&verifier),
    );
    match url.ok().and_then(|url| HeaderValue::from_str(&url).ok()) {
        Some(location) => {
            let mut response = StatusCode::FOUND.into_response();
            let headers = response.headers_mut();
            headers.insert(header::LOCATION, location);
            guarded(headers);
            response
        }
        None => page(StatusCode::INTERNAL_SERVER_ERROR, FAILED_TITLE, FAILED_TEXT, None),
    }
}

/// `GET /api/oauth/callback?code&state`: the provider's code redeemed, the person's code shown.
async fn callback(State(app): State<Arc<App>>, Query(query): Query<Callback>) -> Response {
    let (found, connection, sealer) = match running(&app, query.state.as_deref()).await {
        Ok(Some(running)) => running,
        other => return not_running(other.err()),
    };
    let (Some(code), None) = (query.code, query.error) else {
        return page(StatusCode::BAD_REQUEST, FAILED_TITLE, FAILED_TEXT, None);
    };
    match finish(&app, &found, &connection, &sealer, code).await {
        Ok(Some(shown)) => page(StatusCode::OK, DONE_TITLE, DONE_TEXT, Some(&shown)),
        Ok(None) => page(StatusCode::BAD_GATEWAY, FAILED_TITLE, FAILED_TEXT, None),
        Err(error) => unavailable(&error),
    }
}

/// Redeems the provider's `code` and gives the sign-in its token and a new code: the code to show.
async fn finish(
    app: &App,
    found: &SignIn,
    connection: &connections::Connection,
    sealer: &Sealer,
    code: String,
) -> Result<Option<String>, DbError> {
    let Some((locked, shown)) = redeemed(app, found, connection, sealer, code).await else {
        return Ok(None);
    };
    let taken = store::signed_in(&app.db, &found.id, &tokens::code_hash(&found.id, &shown), &locked).await?;
    Ok(taken.then_some(shown))
}

/// The provider's token for `code`, sealed for the sign-in, and the code to show for it.
async fn redeemed(
    app: &App,
    found: &SignIn,
    connection: &connections::Connection,
    sealer: &Sealer,
    code: String,
) -> Option<(Vec<u8>, String)> {
    let verifier = sealer.open(&verifier_context(found), &found.verifier).ok()?;
    let secret = connection.secret(sealer)?;
    let held = provider::redeem(
        &app.outbound,
        connection,
        secret,
        &redirect_uri(&app.config),
        code,
        String::from_utf8_lossy(&verifier).into_owned(),
    )
    .await
    .inspect_err(|error| tracing::warn!(%error, connection = %connection.name, "a sign-in got no token"))
    .ok()?;
    let plain = serde_json::to_vec(&held).ok()?;
    let locked = sealer.seal(&pending_context(found), &plain).ok()?;
    Some((locked, new_code()?))
}

/// What a sign-in's verifier is sealed for: its bot, connection and person with it.
pub(super) fn verifier_context(sign_in: &SignIn) -> String {
    sign_in_context("sign-in-verifier", sign_in)
}

/// What a sign-in's token is sealed for until its code is redeemed.
pub(super) fn pending_context(sign_in: &SignIn) -> String {
    sign_in_context("sign-in-token", sign_in)
}

fn sign_in_context(kind: &str, sign_in: &SignIn) -> String {
    format!(
        "{kind}|{}|{}|{}|{}",
        sign_in.id, sign_in.bot_id, sign_in.connection, sign_in.user_id
    )
}

/// Six random digits.
fn new_code() -> Option<String> {
    let mut bytes = [0_u8; 4];
    aws_lc_rs::rand::fill(&mut bytes).ok()?;
    Some(format!("{:06}", u32::from_le_bytes(bytes) % 1_000_000))
}

const DONE_TITLE: &str = "You're signed in";
const DONE_TEXT: &str = "Enter this code in the app's card in your chat to finish signing in. It works once, for the \
                         next 15 minutes.";
const GONE_TITLE: &str = "This sign-in link doesn't work";
const GONE_TEXT: &str = "It ran out or was used already. Ask the app to sign you in again.";
const FAILED_TITLE: &str = "Sign-in didn't finish";
const FAILED_TEXT: &str = "The sign-in service didn't complete it. Ask the app to sign you in again.";
const BUSY_TEXT: &str = "The server can't complete it now. Try the link again in a moment.";

fn guarded(headers: &mut axum::http::HeaderMap) {
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'"),
    );
}

/// A page of `title` and `text`, with the code when there is one. Only fixed words and digits are written in it.
fn page(status: StatusCode, title: &str, text: &str, code: Option<&str>) -> Response {
    let code = code.map_or_else(String::new, |code| {
        format!("<p><output id=\"code\">{code}</output></p>")
    });
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{title}</title>\
         <meta name=\"viewport\" content=\"width=device-width\"><style>body{{font-family:sans-serif;max-width:32em;\
         margin:4em auto;padding:0 1em}}output{{font-size:2.5em;letter-spacing:.2em;font-weight:600}}</style></head>\
         <body><h1>{title}</h1>{code}<p>{text}</p></body></html>"
    );
    let mut response = (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    guarded(response.headers_mut());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_verifier_or_pending_token_opens_only_for_its_bot_connection_and_person() {
        let key = super::super::SealKey::new(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            [3_u8; 32],
        ));
        let sealer = Sealer::new(&key).unwrap();
        let sign_in = SignIn {
            id: "s".to_owned(),
            bot_id: "b".to_owned(),
            connection: "github".to_owned(),
            user_id: "ada".to_owned(),
            verifier: Vec::new(),
            code_hash: None,
            token: None,
            attempts: 0,
            expires_at: 0,
            created_at: 0,
        };
        for context in [verifier_context, pending_context] {
            let closed = sealer.seal(&context(&sign_in), b"synthetic").unwrap();
            assert!(sealer.open(&context(&sign_in), &closed).is_ok());
            let moved = [
                SignIn {
                    user_id: "bob".to_owned(),
                    ..sign_in.clone()
                },
                SignIn {
                    bot_id: "c".to_owned(),
                    ..sign_in.clone()
                },
                SignIn {
                    connection: "gitlab".to_owned(),
                    ..sign_in.clone()
                },
                SignIn {
                    id: "t".to_owned(),
                    ..sign_in.clone()
                },
            ];
            for other in moved {
                assert!(sealer.open(&context(&other), &closed).is_err());
            }
        }
        assert_ne!(verifier_context(&sign_in), pending_context(&sign_in));
    }

    #[test]
    fn a_code_is_six_digits_and_a_page_shows_it_unframed_and_uncached() {
        for _ in 0..50 {
            let code = new_code().unwrap();
            assert_eq!(code.len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
        let response = page(StatusCode::OK, DONE_TITLE, DONE_TEXT, Some("012345"));
        let headers = response.headers();
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert!(
            headers[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("default-src 'none'")
        );
    }
}
