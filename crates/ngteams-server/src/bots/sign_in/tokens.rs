//! The token service a bot calls (`api/usertoken/*`, `api/botsignin/*`), in the Bot Framework's shapes: a person's
//! token for a connection (`TokenResponse`), a sign-in link (`SignInResource`, or the link as text), the person's
//! connections (`TokenStatus`), sign-out. The caller is the bot its credential proved; it reads and makes only its
//! own connections' tokens. Single sign-on's exchange is refused with `412`, so an SDK falls back to its card.

use std::collections::HashMap;

use axum::Json;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Map, Value, json};

use super::browser::{pending_context, verifier_context};
use super::connections::{self, Connection};
use super::now_ms as now;
use super::provider::{self, Held};
use super::store::{self, Owner, SignIn};
use super::{SIGN_IN_LIFETIME_MS, Sealer, redirect_uri, sign_in_link};
use crate::app::App;
use crate::bots::Bot;
use crate::db::DbError;

/// How long before its end a token is renewed rather than handed out.
const RENEW_BEFORE_MS: i64 = 60_000;
/// The longest `state` a sign-in link is asked with.
const MAX_STATE: usize = 16 * 1024;

/// A refusal in the Bot Framework's `ErrorResponse` shape.
fn refused(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": {"code": code, "message": message}}))).into_response()
}

fn not_found() -> Response {
    refused(
        StatusCode::NOT_FOUND,
        "NotFound",
        "No token for this user and connection.",
    )
}

fn bad(message: &str) -> Response {
    refused(StatusCode::BAD_REQUEST, "BadArgument", message)
}

fn unavailable(error: &DbError) -> Response {
    tracing::error!(%error, "bot token service database failure");
    refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "ServiceError",
        "The service can't answer now.",
    )
}

/// What the request asks: its query's parameters, their names in lower case.
struct Asked(HashMap<String, String>);

impl Asked {
    fn of(query: Option<&str>) -> Self {
        Self(
            url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
                .map(|(name, value)| (name.to_ascii_lowercase(), value.into_owned()))
                .collect(),
        )
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.0
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    /// The person `userId` names (`29:<id>`, or the id).
    fn user(&self) -> Option<&str> {
        let user = self.get("userId")?;
        Some(user.strip_prefix("29:").unwrap_or(user))
    }

    fn channel(&self) -> &str {
        self.get("channelId").unwrap_or("msteams")
    }
}

/// A request of `bot` under `api/` (`path` after it), with its query.
pub(crate) async fn serve(app: &App, bot: &Bot, method: Method, path: Vec<String>, query: Option<String>) -> Response {
    if !bot.enabled {
        return refused(StatusCode::FORBIDDEN, "Forbidden", "The bot may not do this.");
    }
    let asked = Asked::of(query.as_deref());
    let named = |at: usize, name: &str| path.get(at).is_some_and(|segment| segment.eq_ignore_ascii_case(name));
    if path.len() != 2 {
        return not_found();
    }
    let answer = match method.as_str() {
        "GET" if named(0, "usertoken") && named(1, "GetToken") => get_token(app, bot, &asked).await,
        "GET" if named(0, "usertoken") && named(1, "GetTokenStatus") => statuses(app, bot, &asked).await,
        "DELETE" if named(0, "usertoken") && named(1, "SignOut") => sign_out(app, bot, &asked).await,
        "POST" if named(0, "usertoken") && named(1, "GetAadTokens") => Ok(Json(json!({})).into_response()),
        "POST" if named(0, "usertoken") && named(1, "exchange") => Ok(refused(
            StatusCode::PRECONDITION_FAILED,
            "PreconditionFailed",
            "This server exchanges no single sign-on token; sign the user in with the card.",
        )),
        "GET" if named(0, "botsignin") && named(1, "GetSignInResource") => {
            sign_in_resource(app, bot, &asked, false).await
        }
        "GET" if named(0, "botsignin") && named(1, "GetSignInUrl") => sign_in_resource(app, bot, &asked, true).await,
        _ => Ok(not_found()),
    };
    answer.unwrap_or_else(|error| unavailable(&error))
}

/// The digest a sign-in's code is kept as.
pub(super) fn code_hash(id: &str, code: &str) -> String {
    crate::apps::digest(&format!("sign-in-code|{id}|{code}"))
}

/// What a person's token is sealed for.
fn token_context(owner: Owner<'_>) -> String {
    format!("token|{}|{}|{}", owner.bot, owner.connection, owner.user)
}

/// `GET api/usertoken/GetToken?userId&connectionName&channelId[&code]`.
async fn get_token(app: &App, bot: &Bot, asked: &Asked) -> Result<Response, DbError> {
    let (Some(user), Some(name)) = (asked.user(), asked.get("connectionName")) else {
        return Ok(bad("userId and connectionName are required."));
    };
    let (Some(sealer), Some(connection)) = (
        Sealer::of(&app.config),
        connections::find(&app.db, &bot.id, name).await?,
    ) else {
        return Ok(not_found());
    };
    let owner = Owner {
        bot: &bot.id,
        connection: &connection.name,
        user,
    };
    if let Some(code) = asked.get("code")
        && let Some(held) = redeem_code(app, &sealer, owner, code).await?
    {
        return Ok(token_response(asked.channel(), &connection.name, &held));
    }
    Ok(match held(app, &sealer, &connection, owner).await? {
        Lookup::Token(held) => token_response(asked.channel(), &connection.name, &held),
        Lookup::None => not_found(),
        Lookup::Later => refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "ServiceError",
            "The identity provider didn't renew the token now; try again later.",
        ),
    })
}

/// The owner's token, as a request finds it.
enum Lookup {
    Token(Held),
    None,
    /// It ran out and the provider didn't renew it this time; it stays for the next request.
    Later,
}

/// The token of the owner's sign-in whose code `code` is, which becomes theirs; a wrong code counts against their
/// sign-ins.
async fn redeem_code(app: &App, sealer: &Sealer, owner: Owner<'_>, code: &str) -> Result<Option<Held>, DbError> {
    let waiting = store::with_codes(&app.db, owner).await?;
    let matched = waiting.iter().find(|sign_in| {
        let expected = code_hash(&sign_in.id, code);
        sign_in.code_hash.as_deref().is_some_and(|kept| {
            aws_lc_rs::constant_time::verify_slices_are_equal(kept.as_bytes(), expected.as_bytes()).is_ok()
        })
    });
    let Some(matched) = matched else {
        if !waiting.is_empty() {
            store::missed(&app.db, owner).await?;
        }
        return Ok(None);
    };
    let Some(held) = pending_token(sealer, matched) else {
        return Ok(None);
    };
    let Some(locked) = seal_held(sealer, owner, &held) else {
        return Ok(None);
    };
    Ok(store::redeem(&app.db, &matched.id, owner, &locked)
        .await?
        .then_some(held))
}

fn pending_token(sealer: &Sealer, sign_in: &SignIn) -> Option<Held> {
    let locked = sign_in.token.as_deref()?;
    let plain = sealer.open(&pending_context(sign_in), locked).ok()?;
    serde_json::from_slice(&plain).ok()
}

fn seal_held(sealer: &Sealer, owner: Owner<'_>, held: &Held) -> Option<Vec<u8>> {
    let plain = serde_json::to_vec(held).ok()?;
    sealer.seal(&token_context(owner), &plain).ok()
}

fn open_held(sealer: &Sealer, owner: Owner<'_>, locked: &[u8]) -> Option<Held> {
    let plain = sealer.open(&token_context(owner), locked).ok()?;
    serde_json::from_slice(&plain).ok()
}

/// The owner's token, renewed first when it runs out within [`RENEW_BEFORE_MS`]. When the renewal fails, a token
/// that still works is given, unless the provider refused the grant. Otherwise the provider's availability gives
/// `Later` and the token stays; any other failure signs the owner out of the connection, so the bot shows its card
/// again. A token that another request changed meanwhile is read again.
async fn held(app: &App, sealer: &Sealer, connection: &Connection, owner: Owner<'_>) -> Result<Lookup, DbError> {
    let Some(locked) = store::token(&app.db, owner).await? else {
        return Ok(Lookup::None);
    };
    let Some(held) = open_held(sealer, owner, &locked) else {
        return Ok(Lookup::None);
    };
    if held.expires_at.is_none_or(|at| at > now() + RENEW_BEFORE_MS) {
        return Ok(Lookup::Token(held));
    }
    let Some(secret) = connection.secret(sealer) else {
        tracing::warn!(connection = %connection.name, "a connection's secret didn't open");
        return Ok(Lookup::None);
    };
    let renewed = provider::renew(&app.outbound, connection, secret, &redirect_uri(&app.config), &held)
        .await
        .inspect_err(|error| tracing::info!(%error, connection = %connection.name, "a token wasn't renewed"));
    let error = match renewed {
        Ok(renewed) => {
            let Some(kept) = seal_held(sealer, owner, &renewed) else {
                return Ok(Lookup::None);
            };
            if store::keep(&app.db, owner, &locked, &kept).await? {
                return Ok(Lookup::Token(renewed));
            }
            return held_now(app, sealer, owner).await;
        }
        Err(error) => error,
    };
    if !error.refused() && held.expires_at.is_some_and(|at| at > now()) {
        return Ok(Lookup::Token(held));
    }
    if error.transient() {
        return Ok(Lookup::Later);
    }
    if store::refused(&app.db, owner, &locked).await? {
        return Ok(Lookup::None);
    }
    held_now(app, sealer, owner).await
}

/// The owner's token as another request left it while this one renewed theirs.
async fn held_now(app: &App, sealer: &Sealer, owner: Owner<'_>) -> Result<Lookup, DbError> {
    let now_held = store::token(&app.db, owner)
        .await?
        .and_then(|locked| open_held(sealer, owner, &locked));
    Ok(now_held.map_or(Lookup::None, Lookup::Token))
}

/// A `TokenResponse`.
fn token_response(channel: &str, connection: &str, held: &Held) -> Response {
    let mut answer = json!({"channelId": channel, "connectionName": connection, "token": held.access_token});
    if let Some(at) = held.expires_at.and_then(chrono::DateTime::from_timestamp_millis) {
        answer["expiration"] = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true).into();
    }
    let mut response = Json(answer).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    response
}

/// `GET api/usertoken/GetTokenStatus?userId&channelId[&include]`: each of the bot's connections (those `include`
/// names, when it names any) and whether the person has a token for it.
async fn statuses(app: &App, bot: &Bot, asked: &Asked) -> Result<Response, DbError> {
    let Some(user) = asked.user() else {
        return Ok(bad("userId is required."));
    };
    let included: Vec<String> = asked
        .get("include")
        .map(|names| {
            names
                .split(',')
                .map(|name| name.trim().to_ascii_lowercase())
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let channel = asked.channel().to_owned();
    let found = store::statuses(&app.db, &bot.id, user).await?;
    let mut listed = Vec::new();
    for (name, has) in found {
        if included.is_empty() || included.contains(&name.to_ascii_lowercase()) {
            listed.push(json!({"channelId": channel, "connectionName": name, "hasToken": has,
                               "serviceProviderDisplayName": name}));
        }
    }
    Ok(Json(listed).into_response())
}

/// `DELETE api/usertoken/SignOut?userId[&connectionName]&channelId`.
async fn sign_out(app: &App, bot: &Bot, asked: &Asked) -> Result<Response, DbError> {
    let Some(user) = asked.user() else {
        return Ok(bad("userId is required."));
    };
    store::sign_out(&app.db, &bot.id, asked.get("connectionName"), user).await?;
    Ok(Json(json!({})).into_response())
}

/// A field of `object` by its name in any letter case (the SDKs write `connectionName` or `ConnectionName`).
fn field<'a>(object: &'a Value, name: &str) -> Option<&'a Value> {
    object
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

/// What a link's `state` names: the connection, the person, and the app when it names one.
struct State {
    connection: String,
    user: String,
    app: Option<String>,
}

fn state_of(text: &str) -> Option<State> {
    if text.len() > MAX_STATE {
        return None;
    }
    let trimmed = text.trim().trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed)
        .or_else(|_| STANDARD.decode(text.trim()))
        .ok()?;
    let state: Value = serde_json::from_slice(&bytes).ok()?;
    let text_of = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
    let user = field(field(field(&state, "conversation")?, "user")?, "id").and_then(Value::as_str)?;
    Some(State {
        connection: text_of(field(&state, "connectionName"))?,
        user: user.strip_prefix("29:").unwrap_or(user).to_owned(),
        app: text_of(field(&state, "msAppId")).filter(|app| !app.is_empty()),
    })
}

/// `GET api/botsignin/GetSignInResource?state` (or `GetSignInUrl`, `as_text`): a new sign-in's link for the person
/// and connection the state names.
async fn sign_in_resource(app: &App, bot: &Bot, asked: &Asked, as_text: bool) -> Result<Response, DbError> {
    let Some(state) = asked.get("state").and_then(state_of) else {
        return Ok(bad("state names no connection and user."));
    };
    if state
        .app
        .as_ref()
        .is_some_and(|named| *named != bot.app_id && *named != bot.id)
    {
        return Ok(bad("state names another app."));
    }
    let Some(sealer) = Sealer::of(&app.config) else {
        return Ok(bad("This server keeps no connection."));
    };
    let Some(connection) = connections::find(&app.db, &bot.id, &state.connection).await? else {
        return Ok(bad("The bot has no such connection."));
    };
    let person = crate::db::users::by_id(&app.db, &state.user)
        .await?
        .filter(|person| !person.disabled && person.deleted_at.is_none());
    let may = match &person {
        Some(person) => crate::bots::may_use(&app.db, bot, &person.id).await?,
        None => false,
    };
    if !may {
        return Ok(bad("state names no one who may use the bot."));
    }
    let Some(sign_in) = new_sign_in(&sealer, bot, &connection, &state.user) else {
        return Ok(refused(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ServiceError",
            "The service can't answer now.",
        ));
    };
    store::begin(&app.db, &sign_in).await?;
    let link = sign_in_link(&app.config, &sign_in.id);
    Ok(if as_text {
        ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], link).into_response()
    } else {
        let mut resource = Map::new();
        resource.insert("signInLink".to_owned(), link.into());
        Json(Value::Object(resource)).into_response()
    })
}

fn new_sign_in(sealer: &Sealer, bot: &Bot, connection: &Connection, user: &str) -> Option<SignIn> {
    let mut bytes = [0_u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).ok()?;
    let id = URL_SAFE_NO_PAD.encode(bytes);
    let at = now();
    let mut sign_in = SignIn {
        bot_id: bot.id.clone(),
        connection: connection.name.clone(),
        user_id: user.to_owned(),
        verifier: Vec::new(),
        code_hash: None,
        token: None,
        attempts: 0,
        expires_at: at + SIGN_IN_LIFETIME_MS,
        created_at: at,
        id,
    };
    sign_in.verifier = sealer
        .seal(&verifier_context(&sign_in), provider::new_verifier().as_bytes())
        .ok()?;
    Some(sign_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_is_read_in_either_letter_case_and_either_base64() {
        let camel = json!({"connectionName": "github", "conversation": {"user": {"id": "29:ada"}}, "msAppId": "b"});
        let read = state_of(&STANDARD.encode(camel.to_string())).unwrap();
        assert_eq!((read.connection.as_str(), read.user.as_str()), ("github", "ada"));
        assert_eq!(read.app.as_deref(), Some("b"));
        let pascal = json!({"ConnectionName": "g", "Conversation": {"User": {"Id": "ada"}}});
        let read = state_of(&URL_SAFE_NO_PAD.encode(pascal.to_string())).unwrap();
        assert_eq!(
            (read.connection.as_str(), read.user.as_str(), read.app),
            ("g", "ada", None)
        );
        assert!(state_of("not base64!").is_none());
        assert!(state_of(&STANDARD.encode("{}")).is_none());
        assert!(state_of(&"A".repeat(MAX_STATE + 1)).is_none());
        let asked = Asked::of(Some("UserId=29%3Aada&connectionName=&channelId=msteams"));
        assert_eq!(asked.user(), Some("ada"));
        assert_eq!(asked.get("connectionName"), None);
        assert_eq!(asked.channel(), "msteams");
        assert_ne!(code_hash("a", "123456"), code_hash("b", "123456"));
    }
}
