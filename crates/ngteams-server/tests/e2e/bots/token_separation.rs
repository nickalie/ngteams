//! The token rule across both kinds of account in one process: an own-server account's real engine (every request
//! through a recording front to the in-process server, its bot the loopback test bot) and a Microsoft account's real
//! engine on the mock chat service, whose credentials hand out a distinct synthetic token per audience, both write to
//! a bot, press a card's buttons, search a message extension and read the app catalogue at once. Then no Microsoft
//! token reached the own server, no own-server token reached Microsoft, the bot got only the server's activity token,
//! and only the Microsoft invoke carried `userAadToken`. Accounts, apps, bots and tokens are synthetic.

use std::future::{self, Future};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header::{CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use axum::response::Response;
use reqwest::Method;
use serde_json::{Map, Value, json};
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, SPACES_RESOURCE, TokenClient, TokenError};
use teams_core::Outbox;
use teams_core::account::{CSA_GTM, MIDDLE_TIER_GTM, PROVISIONING_GTM, USER_PROFILE_GTM};
use teams_core::bots::BotAsk;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_mock::{BotReply, CHAT_BASE, ChatServiceMock, LAB_A, MIDDLE_TIER_BASE, bot_answer};
use teams_proto::bot::{CardInvoke, InvokeAnswer, InvokeKind};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Cloud, Credentials, HttpTransport, IC3_AUDIENCE, Mri};
use teams_store::{OutboxState, Store};
use tempfile::TempDir;

use super::connector::{loopback, personal_chat};
use super::endpoint::{Connector, ORIGIN, Scripted, TestBot};
use super::installs::{APP, BOT};
use super::{Registry, claims, good_package};
use crate::support::{Backend, backends};

const UPN: &str = "ada@own.test";
const MS_BOT: &str = "28:00000000-0000-4000-8000-0000000000b1";
const MS_APP: &str = "00000000-0000-4000-8000-0000000000a1";
const MS_TOKEN: &str = "synthetic-ms-";
const MS_TILE: &str =
    "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-updates_largeImage.png?v=1";

/// The Microsoft account's token for `audience`: made up, and different for each audience.
fn ms_token(audience: &str) -> String {
    let audience: String = audience
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{MS_TOKEN}{audience}")
}

struct MsTokens;

impl Credentials for MsTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(ms_token(audience))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// One request through the front and its answer, as text.
#[derive(Clone, Debug)]
struct Exchange {
    uri: String,
    headers: Vec<(String, String)>,
    body: String,
    answer: String,
}

impl Exchange {
    fn sent(&self) -> String {
        let headers: Vec<String> = self
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect();
        format!("{}\n{}\n{}", self.uri, headers.join("\n"), self.body)
    }
}

struct Relay {
    upstream: String,
    client: reqwest::Client,
    log: Mutex<Vec<Exchange>>,
}

/// A loopback front that carries every request to the server unchanged and keeps it.
struct Front {
    origin: String,
    relay: Arc<Relay>,
    task: tokio::task::JoinHandle<()>,
}

impl Front {
    async fn before(upstream: &str) -> Self {
        let relay = Arc::new(Relay {
            upstream: upstream.to_owned(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            log: Mutex::new(Vec::new()),
        });
        let router = Router::new().fallback(carry).with_state(Arc::clone(&relay));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { origin, relay, task }
    }

    fn exchanges(&self) -> Vec<Exchange> {
        self.relay.log.lock().unwrap().clone()
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn carry(State(relay): State<Arc<Relay>>, request: axum::extract::Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let uri = parts.uri.path_and_query().map_or("/", |path| path.as_str()).to_owned();
    let mut forward = relay
        .client
        .request(parts.method.clone(), format!("{}{uri}", relay.upstream));
    for (name, value) in parts.headers.iter().filter(|(name, _)| **name != HOST) {
        forward = forward.header(name, value);
    }
    let answer = forward.body(body.clone()).send().await.unwrap();
    let status = answer.status();
    let headers = answer.headers().clone();
    let bytes = answer.bytes().await.unwrap();
    relay.log.lock().unwrap().push(Exchange {
        uri: format!("{} {uri}", parts.method),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| (name.to_string(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
            .collect(),
        body: String::from_utf8_lossy(&body).into_owned(),
        answer: String::from_utf8_lossy(&bytes).into_owned(),
    });
    let mut response = Response::builder().status(status);
    for (name, value) in headers
        .iter()
        .filter(|(name, _)| **name != TRANSFER_ENCODING && **name != CONTENT_LENGTH)
    {
        response = response.header(name, value);
    }
    response.body(axum::body::Body::from(bytes)).unwrap()
}

/// The own-server account: the packaged app's bot at the test bot, and Ada's engine whose every request (sign-in
/// refresh, authz, chat service, middle tier, app tiles) goes through the front.
struct Own {
    registry: Registry,
    front: Front,
    bot: TestBot,
    secret: String,
    ada: String,
    engine: EngineHandle,
    _dir: TempDir,
}

impl Own {
    async fn open(backend: Backend) -> Self {
        let registry = Registry::open_with(backend, loopback).await;
        let (status, answer) = registry
            .upload(
                &format!("{}/package", super::APPS),
                Method::POST,
                good_package(APP, BOT, "1.2.0"),
            )
            .await;
        assert_eq!(status, 201, "{answer}");
        let secret = answer["secret"].as_str().unwrap().to_owned();
        let bot = TestBot::start().await;
        let endpoint = json!({"endpoint": bot.url});
        let (status, answer) = registry
            .call(Method::PATCH, &super::app_path(APP), Some(endpoint))
            .await;
        assert_eq!(status, 200, "{answer}");
        let ada = registry.server.add_user(UPN, "Ada Lovelace").await;
        let front = Front::before(&registry.server.origin).await;
        let dir = tempfile::tempdir().unwrap();
        let engine = own_engine(&registry, &front, &ada, &dir).await;
        Self {
            registry,
            front,
            bot,
            secret,
            ada,
            engine,
            _dir: dir,
        }
    }
}

async fn own_session(registry: &Registry, front: &Front) -> Arc<Session<MemorySecretStore>> {
    let store = registry.server.signed_in_store(UPN).await;
    let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(&front.origin)).unwrap();
    let authz = AuthzClient::with_endpoint(&format!("{}/api/authsvc/v1.0/authz", front.origin)).unwrap();
    let session = Arc::new(Session::new(UPN, store, tokens, authz, Region::default()));
    session.skypetoken().await.unwrap();
    session
}

fn own_transport(
    session: &Arc<Session<MemorySecretStore>>,
    front: &Front,
    ada: &str,
) -> HttpTransport<Session<MemorySecretStore>> {
    let base = |gtm: &str| session.service_base(gtm).unwrap();
    HttpTransport::for_cloud_with_mock_origin(
        Arc::clone(session),
        &base(teams_auth::authz::CHAT_BASE_GTM),
        Cloud::own(ORIGIN).unwrap(),
        &front.origin,
    )
    .unwrap()
    .for_account(Mri::new(format!("8:orgid:{ada}")))
    .with_middle_tier(&base(MIDDLE_TIER_GTM))
    .unwrap()
    .with_csa(&base(CSA_GTM))
    .unwrap()
    .with_provisioning(&base(PROVISIONING_GTM))
    .unwrap()
    .with_user_profile(&base(USER_PROFILE_GTM))
    .unwrap()
}

async fn own_engine(registry: &Registry, front: &Front, ada: &str, dir: &TempDir) -> EngineHandle {
    let session = own_session(registry, front).await;
    let author = Author::new(Mri::new(format!("8:orgid:{ada}")), UPN);
    start(
        Client::new(own_transport(&session, front, ada)),
        Outbox::new(Arc::new(own_transport(&session, front, ada)), author),
        dir,
    )
}

fn start<C: Credentials + 'static>(
    client: Client<HttpTransport<C>>,
    outbox: Outbox<HttpTransport<C>>,
    dir: &TempDir,
) -> EngineHandle {
    engine::start(Parts {
        store: Store::open(&dir.path().join("account.sqlite")).unwrap(),
        client: Arc::new(client),
        outbox,
        calls: None,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(20),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

/// The Microsoft account: lab account a of the mock, its engine with [`MsTokens`], and a scripted bot.
struct Microsoft {
    mock: ChatServiceMock,
    engine: EngineHandle,
    _dir: TempDir,
}

impl Microsoft {
    async fn open() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        assert!(mock.set_token(LAB_A, ms_token(IC3_AUDIENCE)));
        let answer = |name: &str| BotReply::answer(bot_answer(name).unwrap());
        mock.script_bot(
            MS_BOT,
            "Synthetic Bot",
            &[
                ("adaptiveCard/action", answer("card")),
                ("composeExtension/query", answer("results")),
                ("message", BotReply::done()),
            ],
        );
        for (definition, state) in teams_mock::extension_apps() {
            let id = definition["id"].as_str().unwrap().to_owned();
            mock.add_app(definition);
            if state != "Featured" {
                mock.install_app(teams_mock::AppPlace::Personal, &id);
            }
        }
        let cloud = Cloud::Microsoft;
        let base = cloud.service_url(CHAT_BASE, "/api/chatsvc/amer").unwrap();
        let origin = mock.origin();
        let http = || {
            HttpTransport::for_cloud_with_mock_origin(Arc::new(MsTokens), base.as_str(), cloud.clone(), &origin)
                .unwrap()
                .for_account(Mri::new(LAB_A))
                .with_middle_tier(MIDDLE_TIER_BASE)
                .unwrap()
        };
        let dir = tempfile::tempdir().unwrap();
        let engine = start(
            Client::new(http()),
            Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Lab A")),
            &dir,
        );
        Self {
            mock,
            engine,
            _dir: dir,
        }
    }
}

fn card(message: &str, verb: Option<&str>, data: Value) -> CardInvoke {
    CardInvoke {
        message: Some(message.to_owned()),
        verb: verb.map(str::to_owned),
        data,
        ..CardInvoke::default()
    }
}

fn search() -> InvokeKind {
    InvokeKind::Query {
        command: "searchPolls".to_owned(),
        parameters: vec![("q".to_owned(), "lunch".to_owned())],
        skip: 0,
        count: 25,
    }
}

async fn send(engine: &EngineHandle, conversation: &str, text: &str) -> String {
    let row = engine
        .write(Action::Send {
            conversation: conversation.to_owned(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

/// What the test bot answers each invoke with.
fn own_answers(activity: &Value) -> Scripted {
    let card = json!({"type": "AdaptiveCard", "version": "1.5", "body": [{"type": "TextBlock", "text": "Voted"}]});
    match activity["name"].as_str() {
        Some("adaptiveCard/action") => {
            Scripted::json(json!({"statusCode": 200, "type": "application/vnd.microsoft.card.adaptive", "value": card}))
        }
        Some("composeExtension/query") => Scripted::json(json!({"composeExtension": {"type": "result",
            "attachmentLayout": "list", "attachments": [{"contentType": "application/vnd.microsoft.card.thumbnail",
            "content": {"title": "Poll 7"}}]}})),
        _ => Scripted::empty(200),
    }
}

/// Ada writes to the bot, presses the bot's card's `Action.Execute` and `Action.Submit`, searches its message
/// extension and reads the catalogue and the app's tile.
async fn own_side(own: &Own) {
    let chat = personal_chat(&own.ada, BOT);
    send(&own.engine, &chat, "hello bot").await;
    let got = own.bot.next_of("message").await;
    let service_url = got.activity["serviceUrl"].as_str().unwrap();
    let connector = Connector::signed(&own.registry.server, service_url, BOT, &own.secret).await;
    let poll = json!({"type": "AdaptiveCard", "version": "1.5", "body": [{"type": "TextBlock", "text": "Lunch?"}],
        "actions": [{"type": "Action.Execute", "title": "Vote", "verb": "vote", "data": {"poll": 7}},
                    {"type": "Action.Submit", "title": "Send", "data": {"poll": 7}}]});
    let attachment = json!({"contentType": "application/vnd.microsoft.card.adaptive", "content": poll});
    let (status, answer) = connector
        .send(&chat, json!({"type": "message", "attachments": [attachment]}))
        .await;
    assert_eq!(status, 200, "{answer}");
    own.bot.script(own_answers);
    let bot = Mri::new(format!("28:{BOT}"));
    press_and_search(&own.engine, &bot, APP, &chat, answer["id"].as_str().unwrap()).await;
    own_catalogue(own).await;
}

async fn own_catalogue(own: &Own) {
    let store = own.engine.app_store().await.unwrap();
    assert!(store.iter().any(|app| app.app.id == APP));
    let definition = own.engine.app_definition(APP.to_owned()).await.unwrap().unwrap();
    assert!(own.engine.bot_app(format!("28:{BOT}")).await.unwrap().is_some());
    assert!(!own.engine.message_extensions().await.unwrap().is_empty());
    let tile = definition.large_image_url().unwrap().to_owned();
    assert!(own.engine.app_picture(tile).await.unwrap().is_some());
    assert!(own.engine.app_picture(MS_TILE).await.unwrap().is_none());
}

/// A card's `Action.Execute` and plain `Action.Submit` on `message`, then a message-extension search.
async fn press_and_search(engine: &EngineHandle, bot: &Mri, app: &str, chat: &str, message: &str) {
    let invoke = |kind| {
        engine.bot_invoke(BotAsk {
            bot: bot.clone(),
            app_id: app.to_owned(),
            conversation: Some(chat.to_owned()),
            kind,
        })
    };
    let voted = invoke(InvokeKind::Execute(card(message, Some("vote"), json!({"poll": 7})))).await;
    assert!(matches!(voted.unwrap(), InvokeAnswer::Card(_)));
    let submitted = invoke(InvokeKind::Submit(card(message, None, json!({"poll": 7})))).await;
    assert_eq!(submitted.unwrap(), InvokeAnswer::Done);
    let found = invoke(search()).await.unwrap();
    assert!(matches!(found, InvokeAnswer::Results { .. }), "{found:?}");
}

/// The Microsoft account does the same on the mock: a message in its bot chat, `Execute`, `Submit`, a search, the
/// catalogue and a tile.
async fn microsoft_side(ms: &Microsoft) {
    let chat = ms.mock.add_bot_chat(MS_BOT).unwrap();
    send(&ms.engine, &chat, "hello bot").await;
    press_and_search(&ms.engine, &Mri::new(MS_BOT), MS_APP, &chat, "1700000000003").await;
    microsoft_catalogue(ms).await;
}

async fn microsoft_catalogue(ms: &Microsoft) {
    let extensions = ms.engine.message_extensions().await.unwrap();
    assert!(!extensions.is_empty());
    let bot = extensions[0].commands_in(teams_proto::bot::CommandContext::Compose)[0]
        .bot
        .clone();
    assert!(ms.engine.bot_app(bot.as_str()).await.unwrap().is_some());
    let _ = ms.engine.app_store().await;
    let _ = ms.engine.app_picture(MS_TILE).await.unwrap();
    let own_tile = format!("{ORIGIN}/teamsappdata/{APP}/color.png");
    assert!(ms.engine.app_picture(own_tile).await.unwrap().is_none());
}

/// Every string of `value`'s that names a token: under a key with "token" in it, or a JWT.
fn tokens_in(value: &Value, key: &str, found: &mut Vec<String>) {
    match value {
        Value::String(text)
            if text.len() >= 16 && (key.to_ascii_lowercase().contains("token") || text.starts_with("eyJ")) =>
        {
            found.push(text.clone());
        }
        Value::Array(items) => items.iter().for_each(|item| tokens_in(item, key, found)),
        Value::Object(map) => map.iter().for_each(|(key, item)| tokens_in(item, key, found)),
        _ => {}
    }
}

/// Every token of the own-server account: each bearer, skypetoken and credential it sent, and each token its server
/// handed it.
fn own_tokens(exchanges: &[Exchange]) -> Vec<String> {
    let mut found = Vec::new();
    for exchange in exchanges {
        for (name, value) in &exchange.headers {
            if matches!(name.as_str(), "authorization" | "authentication" | "x-skypetoken") {
                let token = value.rsplit([' ', '=']).next().unwrap_or_default();
                found.push(token.to_owned());
            }
        }
        for text in [&exchange.body, &exchange.answer] {
            if let Ok(value) = serde_json::from_str::<Value>(text) {
                tokens_in(&value, "", &mut found);
            }
        }
        let form = exchange
            .headers
            .iter()
            .any(|(name, value)| name == "content-type" && value.starts_with("application/x-www-form-urlencoded"));
        if form {
            let fields = url::form_urlencoded::parse(exchange.body.as_bytes());
            found.extend(
                fields
                    .filter(|(key, _)| key.contains("token"))
                    .map(|(_, value)| value.into_owned()),
            );
        }
    }
    found.retain(|token| token.len() >= 16);
    found.sort();
    found.dedup();
    found
}

fn body_of(text: &str) -> Map<String, Value> {
    serde_json::from_str(text).unwrap()
}

/// 1 and 4 for the own server: nothing it got carries a Microsoft token, its invokes no `userAadToken`.
fn the_own_server_saw_no_microsoft_token(exchanges: &[Exchange]) {
    for exchange in exchanges {
        let sent = exchange.sent();
        assert!(
            !sent.contains(MS_TOKEN),
            "a Microsoft token reached the own server: {sent}"
        );
    }
    let invokes: Vec<&Exchange> = exchanges
        .iter()
        .filter(|exchange| exchange.uri.ends_with("/invoke"))
        .collect();
    assert_eq!(invokes.len(), 3, "{invokes:?}");
    for invoke in invokes {
        assert!(!body_of(&invoke.body).contains_key("userAadToken"), "{}", invoke.body);
    }
}

/// 2 and 4 for Microsoft: no own-server token reached the mock, and each invoke carried the account's Teams service
/// token in its body.
async fn microsoft_saw_no_own_token(ms: &Microsoft, own: &[String]) {
    let requests = ms.mock.received_requests().await;
    assert!(!requests.is_empty());
    for request in &requests {
        let headers: Vec<String> = request
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {}", String::from_utf8_lossy(value.as_bytes())))
            .collect();
        let sent = format!(
            "{}\n{}\n{}",
            request.url,
            headers.join("\n"),
            String::from_utf8_lossy(&request.body)
        );
        for token in own {
            assert!(
                !sent.contains(token.as_str()),
                "an own-server token reached Microsoft: {sent}"
            );
        }
        assert!(!sent.contains(ORIGIN), "{sent}");
    }
    let invokes = ms.mock.bot_invokes();
    assert_eq!(invokes.len(), 3, "{invokes:?}");
    for invoke in invokes {
        assert_eq!(
            invoke.body["userAadToken"],
            ms_token(SPACES_RESOURCE),
            "{}",
            invoke.body
        );
    }
}

/// 3: the bot got each activity with the server's own token for it, and no account's token anywhere.
fn the_bot_got_no_account_token(own: &Own, tokens: &[String]) {
    let received = own.bot.received();
    assert!(received.len() >= 4, "{received:?}");
    for got in received {
        let authorization = got.authorization.as_deref().unwrap();
        let bearer = authorization.strip_prefix("Bearer ").unwrap();
        let minted = claims(bearer);
        assert_eq!(minted["aud"], BOT);
        assert_eq!(minted["iss"], format!("{ORIGIN}/bot"));
        let headers: Vec<String> = got
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect();
        let sent = format!("{}\n{}", headers.join("\n"), got.activity);
        assert!(!sent.contains(MS_TOKEN), "a Microsoft token reached the bot: {sent}");
        assert!(!sent.contains("userAadToken"), "{sent}");
        for token in tokens {
            assert!(
                !sent.contains(token.as_str()),
                "an own-server token reached the bot: {sent}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_accounts_tokens_reach_only_its_own_service_and_never_a_bot() {
    for backend in backends() {
        let (own, ms) = tokio::join!(Own::open(backend), Microsoft::open());
        tokio::join!(own_side(&own), microsoft_side(&ms));

        let exchanges = own.front.exchanges();
        let tokens = own_tokens(&exchanges);
        let bearers = exchanges
            .iter()
            .filter(|exchange| exchange.headers.iter().any(|(name, _)| name == "authorization"))
            .count();
        assert!(
            bearers > 0 && !tokens.is_empty(),
            "the front saw the account's requests"
        );
        the_own_server_saw_no_microsoft_token(&exchanges);
        microsoft_saw_no_own_token(&ms, &tokens).await;
        the_bot_got_no_account_token(&own, &tokens);

        ms.engine.shutdown();
        own.engine.shutdown();
        own.registry.server.stop().await;
    }
}
