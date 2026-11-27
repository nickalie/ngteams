//! Sign-in cards (`docs/server.md` "Bots"): an administrator gives a bot an OAuth connection; the bot, as an SDK's
//! OAuth prompt does, finds no token for the person at its token service (`api/usertoken/*` on the origin), asks for
//! a sign-in link (`api/botsignin/*`) and posts an OAuth card; the person signs in through the test identity
//! provider in a browser, gets a code, and the engine sends it as `signin/verifyState`; the bot redeems it for the
//! provider's token. Sign-out, a wrong code, refused single sign-on, and that no token of the account ever reaches
//! the bot. Accounts, apps, providers and tokens are synthetic.

use std::time::Duration;

use base64::Engine as _;
use reqwest::Method;
use serde_json::{Value, json};
use teams_proto::bot::{InvokeAnswer, InvokeKind};
use teams_proto::chat::card::{ActionKind, Via};

use super::connector::Setup;
use super::endpoint::{Connector, ORIGIN, Scripted};
use super::idp::{self, CLIENT_ID, CLIENT_SECRET, Idp, RefreshFailure};
use super::{app_path, error_code};
use crate::support::{Backend, backends};

const CONNECTION: &str = "synthetic-github";

/// A usable `bots.token_key` (synthetic).
pub fn token_key() -> String {
    base64::engine::general_purpose::STANDARD.encode([42_u8; 32])
}

fn sign_in_config(config: &mut ngteams_server::config::Config) {
    config.bots.token_key = Some(ngteams_server::bots::sign_in::SealKey::new(token_key()));
}

fn connection_path(app: &str, name: &str) -> String {
    format!("{}/connections/{name}", app_path(app))
}

fn connection_body(idp: &Idp) -> Value {
    json!({"authorize_url": idp.authorize_url(), "token_url": idp.token_url(), "client_id": CLIENT_ID,
           "client_secret": CLIENT_SECRET, "scopes": "read:user repo"})
}

/// The bot's call to its token service at the origin, with its own token; the status and JSON (or text).
async fn token_service(setup: &Setup, connector: &Connector, method: Method, path: &str) -> (u16, Value) {
    let base = if connector.token.is_some() {
        setup.registry.server.origin.clone()
    } else {
        connector.base.trim_end_matches('/').to_owned()
    };
    let mut request = reqwest::Client::new().request(method, format!("{base}/{path}"));
    if let Some(token) = &connector.token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

fn query(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// The state an SDK's prompt sends for a sign-in link: the connection and the conversation reference, in base64.
fn state(setup: &Setup, user: &str, pascal: bool) -> String {
    let reference = json!({"user": {"id": format!("29:{user}")}, "bot": {"id": format!("28:{}", setup.bot_id)},
        "conversation": {"id": setup.chat()}, "channelId": "msteams", "serviceUrl": format!("{ORIGIN}/bot/")});
    let state = if pascal {
        json!({"ConnectionName": CONNECTION, "Conversation": reference, "MsAppId": setup.bot_id})
    } else {
        json!({"connectionName": CONNECTION, "conversation": reference, "msAppId": setup.bot_id})
    };
    base64::engine::general_purpose::STANDARD.encode(state.to_string())
}

/// What the bot asks its token service for a person's token, with the sign-in code when it has one.
fn get_token(user: &str, code: Option<&str>) -> String {
    let user = format!("29:{user}");
    let mut pairs = vec![
        ("userId", user.as_str()),
        ("connectionName", CONNECTION),
        ("channelId", "msteams"),
    ];
    if let Some(code) = code {
        pairs.push(("code", code));
    }
    format!("api/usertoken/GetToken?{}", query(&pairs))
}

/// The bot's OAuth prompt: no token, a sign-in link, an OAuth card in Ada's chat. The link.
async fn prompt(setup: &Setup, connector: &Connector) -> String {
    let (status, answer) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!(status, 404, "{answer}");
    let link = link_for(setup, connector).await;
    let card = json!({"text": "Sign in to Synthetic GitHub", "connectionName": CONNECTION,
        "buttons": [{"type": "signin", "title": "Sign in", "value": link}]});
    let activity = json!({"type": "message", "attachments": [
        {"contentType": "application/vnd.microsoft.card.oauth", "content": card}]});
    let (status, answer) = connector.send(&setup.chat(), activity).await;
    assert_eq!(status, 200, "{answer}");
    link
}

/// A sign-in link for Ada, from the token service.
async fn link_for(setup: &Setup, connector: &Connector) -> String {
    let path = format!(
        "api/botsignin/GetSignInResource?{}",
        query(&[("state", &state(setup, &setup.ada, false))])
    );
    let (status, resource) = token_service(setup, connector, Method::GET, &path).await;
    assert_eq!(status, 200, "{resource}");
    let link = resource["signInLink"].as_str().unwrap().to_owned();
    assert!(link.starts_with(&format!("{ORIGIN}/api/oauth/signin?")), "{link}");
    link
}

/// The link of the OAuth card Ada synced, as the client reads it.
async fn synced_link(setup: &Setup) -> String {
    let messages = setup.synced(&setup.chat()).await;
    let card = messages
        .iter()
        .rev()
        .find(|message| message.message_type == "RichText/Media_Card")
        .expect("the OAuth card reached Ada");
    let parsed = teams_proto::chat::card::card_message(&card.content).unwrap();
    let action = parsed.cards[0]
        .actions
        .iter()
        .find_map(|action| match &action.kind {
            ActionKind::Submit(bot) => match &bot.via {
                Via::SignIn(Some(link)) => Some((action.title.clone(), link.clone())),
                _ => None,
            },
            _ => None,
        })
        .expect("the card has its sign-in button");
    assert_eq!(action.0, "Sign in");
    action.1
}

/// The bot's connector after Ada's first message.
async fn started(setup: &Setup) -> Connector {
    setup.send(&setup.chat(), "start").await;
    let got = setup.bot.next_of("message").await;
    setup.connector(got.activity["serviceUrl"].as_str().unwrap()).await
}

async fn add_connection(setup: &Setup, idp: &Idp) {
    let (status, answer) = setup
        .registry
        .call(
            Method::PUT,
            &connection_path(&setup.bot_id, CONNECTION),
            Some(connection_body(idp)),
        )
        .await;
    assert_eq!(status, 201, "{answer}");
}

/// Whether any string in `value` holds `secret`.
fn holds(value: &Value, secret: &str) -> bool {
    value.to_string().contains(secret)
}

#[tokio::test]
async fn an_oauth_prompt_signs_a_person_in_through_the_browser_and_the_engines_verify_state() {
    for backend in backends() {
        sign_in_and_out(backend).await;
    }
}

async fn sign_in_and_out(backend: Backend) {
    let setup = Setup::open_with(backend, sign_in_config).await;
    let idp = Idp::start().await;
    add_connection(&setup, &idp).await;
    let connector = started(&setup).await;
    let sent_link = prompt(&setup, &connector).await;
    let link = synced_link(&setup).await;
    assert_eq!(link, sent_link);

    let code = browsed(&setup, &idp, &link).await;
    verified(&setup, &code).await;
    redeemed(&setup, &connector, &code).await;
    renewed(&setup, &connector, &idp).await;
    signed_out(&setup, &connector, &code).await;
    setup.stop().await;
}

/// Ada signs in in her browser; the code the page shows, which holds no token.
async fn browsed(setup: &Setup, idp: &Idp, link: &str) -> String {
    let (code, page) = idp::sign_in_in_browser(link, ORIGIN, &setup.registry.server.origin).await;
    assert_eq!(code.len(), 6, "{page}");
    assert!(!page.contains("idp-access"), "{page}");
    assert_eq!(idp.scopes(), ["read:user", "repo"]);
    code
}

/// Ada's engine sends the code; the bot gets it as `signin/verifyState`, with no token of Ada's.
async fn verified(setup: &Setup, code: &str) {
    setup.bot.script(|activity| match activity["name"].as_str() {
        Some("signin/verifyState") => Scripted::empty(200),
        _ => Scripted::empty(404),
    });
    let ada_token = setup.registry.server.session("ada@own.test").await;
    let ada_token = teams_trouter::TrouterCredentials::ic3(&ada_token).await.unwrap();
    let answer = setup
        .invoke(Some(&setup.chat()), InvokeKind::VerifyState { code: code.to_owned() })
        .await;
    assert_eq!(answer, InvokeAnswer::Done);
    let got = setup.bot.next_of("invoke").await;
    assert_eq!(got.activity["name"], "signin/verifyState");
    assert_eq!(got.activity["value"], json!({"state": code}));
    assert_eq!(got.activity["from"]["id"], format!("29:{}", setup.ada));
    assert!(!holds(&got.activity, ada_token.token.expose()), "{}", got.activity);
    assert_eq!(got.token_claims().unwrap()["aud"], setup.bot_id.as_str());
}

/// The bot redeems the code once, then the token is the person's.
async fn redeemed(setup: &Setup, connector: &Connector, code: &str) {
    let (status, token) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, Some(code))).await;
    assert_eq!(status, 200, "{token}");
    assert_eq!(token["token"], "idp-access-1");
    assert_eq!(token["connectionName"], CONNECTION);
    assert_eq!(token["channelId"], "msteams");
    assert!(
        token["expiration"].as_str().is_some_and(|at| at.ends_with('Z')),
        "{token}"
    );
    let (status, again) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!((status, &again["token"]), (200, &json!("idp-access-1")));
    assert!(has_token(setup, connector).await);
}

/// Whether the token service says Ada has a token for the connection.
async fn has_token(setup: &Setup, connector: &Connector) -> bool {
    let status_path = format!(
        "api/usertoken/GetTokenStatus?{}",
        query(&[("userId", &format!("29:{}", setup.ada)), ("channelId", "msteams")])
    );
    let (status, statuses) = token_service(setup, connector, Method::GET, &status_path).await;
    assert_eq!(status, 200, "{statuses}");
    assert_eq!(statuses[0]["connectionName"], CONNECTION);
    statuses[0]["hasToken"].as_bool().unwrap()
}

/// Sign-out: no token for the bot, and the redeemed code doesn't sign in again.
async fn signed_out(setup: &Setup, connector: &Connector, code: &str) {
    let sign_out = format!(
        "api/usertoken/SignOut?{}",
        query(&[
            ("userId", &format!("29:{}", setup.ada)),
            ("connectionName", CONNECTION),
            ("channelId", "msteams"),
        ])
    );
    let (status, _) = token_service(setup, connector, Method::DELETE, &sign_out).await;
    assert_eq!(status, 200);
    let (status, _) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!(status, 404, "signed out, the bot gets no token");
    let (status, _) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, Some(code))).await;
    assert_eq!(status, 404, "a redeemed code doesn't sign in again");
}

/// A token past its time is renewed with its refresh token when the bot asks for it.
async fn renewed(setup: &Setup, connector: &Connector, idp: &Idp) {
    idp.lifetime(1);
    // a second sign-in, whose token lives a second, takes the first one's place
    let link = link_for(setup, connector).await;
    let (code, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
    let (status, token) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, Some(&code))).await;
    assert_eq!((status, &token["token"]), (200, &json!("idp-access-2")), "{token}");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    idp.lifetime(2);
    let (status, token) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!((status, &token["token"]), (200, &json!("idp-access-101")), "{token}");
    assert_eq!(idp.issued(), (2, 1));
    renewal_failed(setup, connector, idp).await;
    idp.lifetime(3600);
}

/// A provider that isn't there keeps the token: it is given while it works, then 503, and the next renewal works.
async fn renewal_failed(setup: &Setup, connector: &Connector, idp: &Idp) {
    let path = get_token(&setup.ada, None);
    let token = || token_service(setup, connector, Method::GET, &path);
    idp.fail_refreshes(Some(RefreshFailure::Unavailable));
    let (status, held) = token().await;
    assert_eq!((status, &held["token"]), (200, &json!("idp-access-101")), "{held}");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let (status, answer) = token().await;
    assert_eq!(status, 503, "{answer}");
    assert!(has_token(setup, connector).await, "a provider outage signs no one out");
    idp.fail_refreshes(None);
    let (status, held) = token().await;
    assert_eq!((status, &held["token"]), (200, &json!("idp-access-102")), "{held}");
    refused_renewal(setup, connector, idp).await;
    unreadable_renewal(setup, connector, idp).await;
}

/// A refused refresh token signs the person out.
async fn refused_renewal(setup: &Setup, connector: &Connector, idp: &Idp) {
    idp.fail_refreshes(Some(RefreshFailure::InvalidGrant));
    let (status, _) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!(status, 404);
    assert!(
        !has_token(setup, connector).await,
        "a refused refresh token signs the person out"
    );
    idp.fail_refreshes(None);
    assert_eq!(idp.issued(), (2, 2));
}

/// An answer to a renewal that isn't the provider's outage signs the person out once the token ran out, so the bot
/// shows its card again.
async fn unreadable_renewal(setup: &Setup, connector: &Connector, idp: &Idp) {
    idp.lifetime(1);
    let link = link_for(setup, connector).await;
    let (code, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
    let (status, token) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, Some(&code))).await;
    assert_eq!((status, &token["token"]), (200, &json!("idp-access-3")), "{token}");
    idp.fail_refreshes(Some(RefreshFailure::Unreadable));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (status, answer) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!(status, 404, "{answer}");
    assert!(!has_token(setup, connector).await, "the token that ran out went");
    idp.fail_refreshes(None);
    assert_eq!(idp.issued(), (3, 2));
}

#[tokio::test]
async fn a_wrong_code_signs_no_one_in_and_single_sign_on_is_refused() {
    let setup = Setup::open_with(Backend::Sqlite, sign_in_config).await;
    let idp = Idp::start().await;
    add_connection(&setup, &idp).await;
    let bob = setup.registry.server.add_user("bob@own.test", "Bob Byte").await;
    let connector = started(&setup).await;
    let link = prompt(&setup, &connector).await;
    let (code, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;

    // Ada's code doesn't sign Bob in
    let (status, _) = token_service(&setup, &connector, Method::GET, &get_token(&bob, Some(&code))).await;
    assert_eq!(status, 404);
    // five wrong codes end the sign-in: the right one no longer works
    let wrong = if code == "000000" { "111111" } else { "000000" };
    for _ in 0..5 {
        let (status, _) = token_service(&setup, &connector, Method::GET, &get_token(&setup.ada, Some(wrong))).await;
        assert_eq!(status, 404);
    }
    let (status, _) = token_service(&setup, &connector, Method::GET, &get_token(&setup.ada, Some(&code))).await;
    assert_eq!(status, 404, "the sign-in ended after the wrong codes");

    single_sign_on_is_refused(&setup, &connector).await;
    // a person's token isn't the bot's: no bot token, no token service
    let (status, _) = super::call(
        &setup.registry.server,
        Method::GET,
        &format!("/{}", get_token(&setup.ada, None)),
        Some(&setup.registry.token().await),
        None,
    )
    .await;
    assert_eq!(status, 401);
    // a state that names another bot's app makes no link
    let mut wrong_state: Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(state(&setup, &setup.ada, true))
            .unwrap(),
    )
    .unwrap();
    wrong_state["MsAppId"] = json!("00000000-0000-4000-8000-00000000dead");
    let path = format!(
        "api/botsignin/GetSignInResource?{}",
        query(&[(
            "state",
            &base64::engine::general_purpose::STANDARD.encode(wrong_state.to_string())
        )])
    );
    let (status, _) = token_service(&setup, &connector, Method::GET, &path).await;
    assert_eq!(status, 400);
    // the older link route of botbuilder: the link as text, from the Bot Framework's PascalCase state
    let path = format!(
        "api/botsignin/GetSignInUrl?{}",
        query(&[("state", &state(&setup, &setup.ada, true))])
    );
    let (status, url) = token_service(&setup, &connector, Method::GET, &path).await;
    assert_eq!(status, 200);
    assert!(
        url.as_str()
            .is_some_and(|url| url.starts_with(&format!("{ORIGIN}/api/oauth/signin?"))),
        "{url}"
    );
    // a sign-in link that isn't one
    let (status, _) = super::call(
        &setup.registry.server,
        Method::GET,
        "/api/oauth/signin?signin=nothing",
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
    setup.stop().await;
}

/// Single sign-on: the token service refuses an exchange, and an invoke for it never reaches the bot.
async fn single_sign_on_is_refused(setup: &Setup, connector: &Connector) {
    let exchange = format!(
        "api/usertoken/exchange?{}",
        query(&[
            ("userId", &format!("29:{}", setup.ada)),
            ("connectionName", CONNECTION),
            ("channelId", "msteams"),
        ])
    );
    let status = reqwest::Client::new()
        .post(setup.registry.server.url(&format!("/{exchange}")))
        .bearer_auth(connector.token.as_deref().unwrap())
        .json(&json!({"token": "synthetic-sso-token"}))
        .send()
        .await
        .map(|response| response.status().as_u16())
        .unwrap();
    assert_eq!(status, 412);
    let hits = setup.bot.hits();
    let body = json!({"name": "signin/tokenExchange", "appId": setup.bot_id,
        "conversation": {"id": setup.chat()}, "value": {"id": "x", "connectionName": CONNECTION, "token": "t"}});
    let (status, _) = setup.raw_invoke(&setup.bot_id, &body).await;
    assert_eq!(status, 412);
    assert_eq!(setup.bot.hits(), hits, "the bot got no single sign-on invoke");
}

#[tokio::test]
async fn an_administrator_keeps_a_bots_connections_and_their_secret_stays_unread() {
    let setup = Setup::open_with(Backend::Sqlite, sign_in_config).await;
    let idp = Idp::start().await;
    add_connection(&setup, &idp).await;
    let list = format!("{}/connections", app_path(&setup.bot_id));
    let (status, answer) = setup.registry.call(Method::GET, &list, None).await;
    assert_eq!(status, 200, "{answer}");
    let only = &answer["value"][0];
    assert_eq!(only["name"], CONNECTION);
    assert_eq!(only["client_id"], CLIENT_ID);
    assert_eq!(only["scopes"], "read:user repo");
    assert_eq!(only["redirect_uri"], format!("{ORIGIN}/api/oauth/callback"));
    assert!(!holds(&answer, CLIENT_SECRET), "{answer}");

    // a change without a secret keeps the old one
    let mut changed = connection_body(&idp);
    changed.as_object_mut().unwrap().remove("client_secret");
    changed["scopes"] = json!("read:user");
    let path = connection_path(&setup.bot_id, CONNECTION);
    let (status, answer) = setup.registry.call(Method::PUT, &path, Some(changed)).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["scopes"], "read:user");
    let connector = started(&setup).await;
    let link = prompt(&setup, &connector).await;
    let (code, page) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
    assert_eq!(code.len(), 6, "the old secret still signs in: {page}");

    // what isn't a connection is refused
    let mut no_secret = connection_body(&idp);
    no_secret.as_object_mut().unwrap().remove("client_secret");
    let other = connection_path(&setup.bot_id, "other");
    let (status, answer) = setup.registry.call(Method::PUT, &other, Some(no_secret)).await;
    assert_eq!((status, error_code(&answer)), (400, "InvalidConnection"), "{answer}");
    let mut metadata = connection_body(&idp);
    metadata["token_url"] = json!("http://169.254.169.254/token");
    let (status, answer) = setup.registry.call(Method::PUT, &other, Some(metadata)).await;
    assert_eq!((status, error_code(&answer)), (400, "EndpointMetadata"), "{answer}");
    let mut script = connection_body(&idp);
    script["authorize_url"] = json!("javascript:alert(1)");
    let (status, answer) = setup.registry.call(Method::PUT, &other, Some(script)).await;
    assert_eq!((status, error_code(&answer)), (400, "InvalidConnection"), "{answer}");
    let bad_name = connection_path(&setup.bot_id, "a%20b%2Fc");
    let (status, _) = setup
        .registry
        .call(Method::PUT, &bad_name, Some(connection_body(&idp)))
        .await;
    assert_eq!(status, 400);
    let missing = connection_path("00000000-0000-4000-8000-00000000dead", CONNECTION);
    let (status, _) = setup
        .registry
        .call(Method::PUT, &missing, Some(connection_body(&idp)))
        .await;
    assert_eq!(status, 404);

    // a person who isn't an administrator reads nothing
    setup.registry.server.add_user("eve@own.test", "Eve Synthetic").await;
    let eve = setup.registry.server.session("eve@own.test").await;
    let eve = eve.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    let (status, _) = super::call(
        &setup.registry.server,
        Method::GET,
        &list,
        Some(eve.token.expose()),
        None,
    )
    .await;
    assert_eq!(status, 403);

    a_change_keeps_tokens_and_a_delete_drops_them(&setup, &connector, &idp, &code).await;
    another_provider_drops_tokens(&setup, &connector, &idp).await;
    setup.stop().await;

    // without a key, no connection is made
    let keyless = Setup::open(Backend::Sqlite).await;
    let path = connection_path(&keyless.bot_id, CONNECTION);
    let (status, answer) = keyless
        .registry
        .call(Method::PUT, &path, Some(connection_body(&idp)))
        .await;
    assert_eq!((status, error_code(&answer)), (409, "SignInNotConfigured"), "{answer}");
    keyless.stop().await;
}

/// A connection's change keeps the people's tokens; its deletion drops them, also when it is made again.
async fn a_change_keeps_tokens_and_a_delete_drops_them(setup: &Setup, connector: &Connector, idp: &Idp, code: &str) {
    let path = connection_path(&setup.bot_id, CONNECTION);
    let list = format!("{}/connections", app_path(&setup.bot_id));
    // a change keeps the people's tokens
    let (status, _) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, Some(code))).await;
    assert_eq!(status, 200);
    let mut changed = connection_body(idp);
    changed["scopes"] = json!("repo");
    let (status, answer) = setup.registry.call(Method::PUT, &path, Some(changed)).await;
    assert_eq!(status, 200, "{answer}");
    let (status, token) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!(status, 200, "{token}");

    // deleting the connection signs everyone out of it, also when it is made again
    let (status, _) = setup.registry.call(Method::DELETE, &path, None).await;
    assert_eq!(status, 204);
    let (status, answer) = setup.registry.call(Method::GET, &list, None).await;
    assert_eq!((status, &answer["value"]), (200, &json!([])));
    let (status, _) = setup
        .registry
        .call(Method::PUT, &path, Some(connection_body(idp)))
        .await;
    assert_eq!(status, 201);
    let (status, _) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!(status, 404);
}

/// A change to another provider signs everyone out of the connection: its grants go only to the provider that gave
/// them.
async fn another_provider_drops_tokens(setup: &Setup, connector: &Connector, idp: &Idp) {
    let path = connection_path(&setup.bot_id, CONNECTION);
    // another token URL needs its secret again
    let mut moved = connection_body(idp);
    moved.as_object_mut().unwrap().remove("client_secret");
    moved["token_url"] = json!(format!("{}/other-token", idp.base));
    let (status, answer) = setup.registry.call(Method::PUT, &path, Some(moved)).await;
    assert_eq!((status, error_code(&answer)), (400, "InvalidConnection"), "{answer}");
    sign_in_once(setup, connector).await;
    // sign-ins under way when the provider changes: one with its code, one with only its link
    let link = link_for(setup, connector).await;
    let (waiting, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
    let unopened = link_for(setup, connector).await;
    let other = Idp::start().await;
    let (status, answer) = setup
        .registry
        .call(Method::PUT, &path, Some(connection_body(&other)))
        .await;
    assert_eq!(status, 200, "{answer}");
    old_provider_grants_went(setup, connector, &waiting, &unopened).await;
    assert_eq!(other.issued(), (0, 0));
}

async fn sign_in_once(setup: &Setup, connector: &Connector) {
    let link = link_for(setup, connector).await;
    let (code, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
    let (status, token) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, Some(&code))).await;
    assert_eq!(status, 200, "{token}");
}

async fn old_provider_grants_went(setup: &Setup, connector: &Connector, waiting: &str, unopened: &str) {
    let (status, _) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, None)).await;
    assert_eq!(status, 404, "the old provider's token went with it");
    let (status, _) = token_service(setup, connector, Method::GET, &get_token(&setup.ada, Some(waiting))).await;
    assert_eq!(status, 404, "a code of the old provider signs no one in");
    let status = idp::link_status(unopened, ORIGIN, &setup.registry.server.origin).await;
    assert_eq!(status, 404, "a link made for the old provider doesn't open");
}

#[tokio::test]
async fn an_unsigned_bot_reaches_its_token_service_under_its_own_service_url() {
    let setup = Setup::open_with(Backend::Sqlite, |config| {
        sign_in_config(config);
        config.bots.allow_unsigned = true;
    })
    .await;
    let idp = Idp::start().await;
    let (status, answer) = setup
        .registry
        .call(
            Method::PATCH,
            &app_path(&setup.bot_id),
            Some(json!({"auth_mode": "unsigned"})),
        )
        .await;
    assert_eq!(status, 200, "{answer}");
    add_connection(&setup, &idp).await;
    setup.send(&setup.chat(), "start").await;
    let got = setup.bot.next_of("message").await;
    let service_url = got.activity["serviceUrl"].as_str().unwrap();
    assert!(service_url.contains("/bot/u/"), "{service_url}");
    let connector = Connector::unsigned(&setup.registry.server, service_url);
    let link = prompt(&setup, &connector).await;
    let (code, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
    let (status, token) = token_service(&setup, &connector, Method::GET, &get_token(&setup.ada, Some(&code))).await;
    assert_eq!((status, &token["token"]), (200, &json!("idp-access-1")), "{token}");
    // the same route without its credential is refused
    let (status, _) = super::call(
        &setup.registry.server,
        Method::GET,
        &format!("/{}", get_token(&setup.ada, None)),
        None,
        None,
    )
    .await;
    assert_eq!(status, 401);
    setup.stop().await;
}
