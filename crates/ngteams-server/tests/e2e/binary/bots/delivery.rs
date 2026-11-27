//! QA of task 229: bot chats and delivery as a user, an administrator and a bot developer meet them, on the real
//! `ngteams-server` binary over TLS. A test bot is a small HTTPS endpoint on loopback (its certificate from the test
//! CA the server trusts through `SSL_CERT_FILE`); the people write through the client's own engine; the bot answers
//! through the connector routes with its own token. Every account, bot and text is synthetic.
//!
//! - a user and a signed bot: the bot's chat made by the first message, text, Markdown and card answers, edit,
//!   delete and typing both ways, the roster, a 1:1 chat the bot starts; who may not use it, a chat the bot is not
//!   in; an unsigned bot, its secret rotated, its app turned off;
//! - the address guard as an administrator sees it: private, loopback, link-local, shared and metadata addresses,
//!   a redirect, a slow and a huge answer, refused with a reason in the log and nothing sent; a CIDR opens one;
//! - a dead endpoint: tried again, then failed; after 20 failures the bot pauses, then gets its activities again;
//! - two API nodes on Postgres and Redis, one killed and the other restarted while activities go out, then both
//!   stopped with activities pending: none lost, none sent twice except under the same id after a killed node's lease.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use ngteams_server::bots::{Delivery, FAILED, SENT, deliveries};
use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_core::Outbox;
use teams_core::account::AMS_GTM;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_core::{Transient, TransientSent};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri};
use teams_store::{Message, OutboxEntry, OutboxState, Store};

use super::super::support::{Deployment, Node, TLS_DIR, tls_parent};
use super::support::{APPS, app_path, call, code, first_admin, tenant};
use crate::support::{Backend, PASSWORD, PG_ENV, backends, redis_url};

mod invoke;

const ADA: &str = "ada@bots.own.test";
const BEA: &str = "bea@bots.own.test";
const CORA: &str = "cora@bots.own.test";
const ECHO: &str = "Echo QA (synthetic)";
/// Where a run keeps the nodes' logs, when set.
const EVIDENCE: &str = "NGTS_QA_BOTS_EVIDENCE";

// --- the test bot ---

#[derive(Clone, Debug)]
enum Mode {
    /// Answers each activity with this status at once.
    Status(u16),
    /// Answers 200 after this delay.
    Slow(Duration),
    /// Answers 200 with a body bigger than the server reads.
    Huge,
    /// Answers 307 to this address.
    Redirect(String),
}

/// One activity as the bot got it.
#[derive(Clone, Debug)]
struct Got {
    at: Instant,
    activity: Value,
    authorization: Option<String>,
}

impl Got {
    fn kind(&self) -> &str {
        self.activity["type"].as_str().unwrap_or_default()
    }

    fn id(&self) -> &str {
        self.activity["id"].as_str().unwrap_or_default()
    }

    fn service_url(&self) -> String {
        self.activity["serviceUrl"].as_str().unwrap().to_owned()
    }
}

struct BotState {
    mode: Mutex<Mode>,
    got: Mutex<Vec<Got>>,
}

/// A bot endpoint over HTTPS on loopback, with the test CA's certificate for `127.0.0.1`.
struct QaBot {
    url: String,
    state: Arc<BotState>,
    handle: axum_server::Handle<std::net::SocketAddr>,
}

impl QaBot {
    async fn start(mode: Mode) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let certs = PathBuf::from(std::env::var_os(TLS_DIR).unwrap());
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(certs.join("leaf.pem"), certs.join("key.pem"))
            .await
            .unwrap();
        let state = Arc::new(BotState {
            mode: Mutex::new(mode),
            got: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/api/messages", post(receive))
            .with_state(Arc::clone(&state));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = axum_server::Handle::new();
        let server = axum_server::from_tcp_rustls(listener, tls)
            .unwrap()
            .handle(handle.clone());
        tokio::spawn(async move {
            let _ = server.serve(router.into_make_service()).await;
        });
        Self {
            url: format!("https://127.0.0.1:{port}/api/messages"),
            state,
            handle,
        }
    }

    fn set(&self, mode: Mode) {
        *self.state.mode.lock().unwrap_or_else(PoisonError::into_inner) = mode;
    }

    fn got(&self) -> Vec<Got> {
        self.state.got.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn hits(&self) -> usize {
        self.got().len()
    }

    fn of(&self, kind: &str) -> Vec<Got> {
        self.got().into_iter().filter(|got| got.kind() == kind).collect()
    }

    /// The first activity of `kind` with `id` (any id when empty); panics after `wait`.
    async fn wait_for(&self, kind: &str, id: &str, wait: Duration) -> Got {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(got) = self.of(kind).into_iter().find(|got| id.is_empty() || got.id() == id) {
                return got;
            }
            assert!(
                Instant::now() < deadline,
                "no {kind} {id} reached the bot: {:#?}",
                self.got()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for QaBot {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

async fn receive(State(state): State<Arc<BotState>>, headers: HeaderMap, body: String) -> Response {
    let got = Got {
        at: Instant::now(),
        activity: serde_json::from_str(&body).unwrap_or(Value::Null),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    };
    state.got.lock().unwrap_or_else(PoisonError::into_inner).push(got);
    let mode = state.mode.lock().unwrap_or_else(PoisonError::into_inner).clone();
    match mode {
        Mode::Status(status) => StatusCode::from_u16(status).unwrap().into_response(),
        Mode::Slow(delay) => {
            tokio::time::sleep(delay).await;
            StatusCode::OK.into_response()
        }
        Mode::Huge => (StatusCode::OK, "x".repeat(400 * 1024)).into_response(),
        Mode::Redirect(to) => (StatusCode::TEMPORARY_REDIRECT, [("location", to)]).into_response(),
    }
}

// --- the bot's side of the connector ---

/// The connector under a `serviceUrl`, with the bot's own token when it is signed.
struct Conn<'a> {
    node: &'a Node,
    base: String,
    token: Option<String>,
}

impl Conn<'_> {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self.node.http.request(method, format!("{}{path}", self.base));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let text = response.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn send(&self, chat: &str, activity: Value) -> (u16, Value) {
        self.call(
            Method::POST,
            &format!("v3/conversations/{}/activities", encode(chat)),
            Some(activity),
        )
        .await
    }

    async fn reply(&self, chat: &str, to: &str, activity: Value) -> (u16, Value) {
        let path = format!("v3/conversations/{}/activities/{to}", encode(chat));
        self.call(Method::POST, &path, Some(activity)).await
    }
}

fn encode(chat: &str) -> String {
    url::form_urlencoded::byte_serialize(chat.as_bytes()).collect()
}

fn text(text: &str) -> Value {
    json!({"type": "message", "text": text})
}

/// The bot's token from the server's `botframework.com` token endpoint, as the Bot Framework SDK asks for it.
async fn bot_token(node: &Node, id: &str, secret: &str) -> String {
    let form = [
        ("grant_type", "client_credentials"),
        ("client_id", id),
        ("client_secret", secret),
        ("scope", "https://api.botframework.com/.default"),
    ];
    let response = node
        .http
        .post(format!("{}/botframework.com/oauth2/v2.0/token", node.endpoint))
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let answer: Value = response.json().await.unwrap();
    answer["access_token"].as_str().unwrap().to_owned()
}

// --- a person in the client ---

/// A person signed in at a node over TLS, with the client's own engine on the HTTPS transport an own server's account
/// gets (no test origin override).
pub(super) struct Actor {
    session: Arc<teams_auth::session::Session<teams_auth::secrets::MemorySecretStore>>,
    pub(super) mri: Mri,
    engine: EngineHandle,
    _dir: tempfile::TempDir,
}

impl Actor {
    pub(super) async fn sign_in(node: &Node, upn: &str) -> Self {
        let session = Arc::new(node.sign_in(upn, PASSWORD).await);
        let skype = session.skypetoken().await.unwrap();
        let mri = Mri::new(format!("8:{}", skype.skype_id.as_deref().unwrap()));
        let ams = session.service_base(AMS_GTM).unwrap();
        // The region names AMS on the shared public origin; this actor is pinned to its own node, as its chat base is.
        let ams = format!("{}{}", node.endpoint, url::Url::parse(&ams).unwrap().path());
        let transport = || {
            HttpTransport::for_cloud(
                Arc::clone(&session),
                &format!("{}/api/chatsvc/own", node.endpoint),
                Cloud::own(&node.endpoint).unwrap(),
            )
            .unwrap()
            .with_ams(&ams)
            .unwrap()
            .for_account(mri.clone())
        };
        let dir = tempfile::tempdir().unwrap();
        let engine = engine::start(Parts {
            store: Store::open(&dir.path().join("account.sqlite")).unwrap(),
            client: Arc::new(Client::new(transport())),
            outbox: Outbox::new(Arc::new(transport()), Author::new(mri.clone(), upn)),
            calls: None,
            push: None,
            options: EngineOptions {
                tick: Duration::from_millis(20),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self {
            session,
            mri,
            engine,
            _dir: dir,
        }
    }

    pub(super) fn engine(&self) -> &EngineHandle {
        &self.engine
    }

    pub(super) async fn sync(&self) {
        let report = self.engine.sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
    }

    async fn write(&self, action: Action) -> OutboxEntry {
        let row = self.engine.write(action).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row
    }

    pub(super) async fn send(&self, chat: &str, text: &str) -> String {
        self.write(Action::Send {
            conversation: chat.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .message_id
        .unwrap()
    }

    async fn create(&self, others: &[&Self], topic: Option<&str>) -> String {
        self.write(Action::CreateChat {
            others: others.iter().map(|actor| actor.mri.clone()).collect(),
            topic: topic.map(str::to_owned),
        })
        .await
        .message_id
        .unwrap()
    }
}

// --- the deployment ---

/// An account's id, as its MRI and a bot chat's id carry it.
pub(super) fn id_of(actor: &Actor) -> String {
    actor
        .mri
        .as_str()
        .trim_start_matches("8:")
        .trim_start_matches("orgid:")
        .to_owned()
}

pub(super) fn personal_chat(user: &str, bot: &str) -> String {
    let mut ids = [user, bot];
    ids.sort_unstable();
    format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1])
}

/// The `[bots]` table and the rest of a node's configuration: `base` is its text before any `[bots]` table.
fn bots_table(node: &Node, base: &str, table: &str) {
    std::fs::write(&node.config, format!("{base}[bots]\n{table}\n")).unwrap();
}

/// The rows of `bot` once `count` are there and each is sent or failed (or `within` is over).
async fn settled(node: &Node, bot: &str, count: usize, within: Duration) -> Vec<Delivery> {
    let config = ngteams_server::config::Config::load(Some(&node.config)).unwrap();
    let db = ngteams_server::db::Db::connect(&config.database_url, 1).await.unwrap();
    let deadline = Instant::now() + within;
    loop {
        let rows = deliveries(&db, bot).await.unwrap();
        let done = rows.len() >= count && rows.iter().all(|row| row.state == SENT || row.state == FAILED);
        if done || Instant::now() > deadline {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn summary(rows: &[Delivery]) -> Vec<(String, i32, Option<String>)> {
    rows.iter()
        .map(|row| (row.state.clone(), row.attempts, row.last_error.clone()))
        .collect()
}

/// Copies a node's log to the evidence directory, when a run names one.
fn keep_log(node: &Node, name: &str) {
    if let Some(dir) = std::env::var_os(EVIDENCE) {
        let dir = Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        let _ = std::fs::copy(node.config.with_extension("log"), dir.join(format!("{name}.log")));
    }
}

fn log_of(node: &Node) -> String {
    std::fs::read_to_string(node.config.with_extension("log")).unwrap_or_default()
}

/// Registers a bot from the admin's form; its id and secret.
async fn register(node: &Node, root: &Secret, name: &str, endpoint: &str, unsigned: bool) -> (String, String) {
    let mut body = json!({"name": name, "endpoint": endpoint});
    if unsigned {
        body["auth_mode"] = json!("unsigned");
    }
    let (status, answer) = call(node, Method::POST, APPS, Some(root), Some(body)).await;
    assert_eq!(status, 201, "{name} {endpoint}: {answer}");
    (
        answer["id"].as_str().unwrap().to_owned(),
        answer["secret"].as_str().unwrap_or_default().to_owned(),
    )
}

/// One `all` node over TLS with the people Ada, Bea and Cora, the `[bots]` table `table`, and its administrator.
async fn one_node(backend: Backend, table: &str) -> (Deployment, Node, String, Secret) {
    let redis = (backend == Backend::Postgres).then(redis_url).flatten();
    let deployment = Deployment::new(backend, true, redis).await;
    let mut node = deployment.node("a");
    // the binary's own default: an administrator reads why a send failed at `info`
    node.log_filter = "info".to_owned();
    let base = std::fs::read_to_string(&node.config).unwrap();
    bots_table(&node, &base, table);
    node.cli(&["migrate"], "");
    for upn in [ADA, BEA, CORA] {
        node.add_user(upn, PASSWORD);
    }
    node.start().await;
    let root = first_admin(&node).await;
    (deployment, node, base, root)
}

pub(super) async fn synced(actor: &Actor, chat: &str) -> Vec<Message> {
    actor.sync().await;
    let mut page = actor.engine().messages(chat, None, 200).await.unwrap().messages;
    page.reverse();
    page
}

// --- a user and a bot ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_user_and_an_https_bot_talk_through_the_binary_over_tls() {
    if tls_parent("binary::bots::delivery::a_user_and_an_https_bot_talk_through_the_binary_over_tls") {
        return;
    }
    for backend in backends() {
        eprintln!("qa bots talk: {backend:?}");
        let table = "allow_unsigned = true\nallowed_networks = [\"127.0.0.0/8\"]\nretry_delays = [1, 1]\n";
        let (deployment, mut node, _, root) = one_node(backend, table).await;
        let bot = QaBot::start(Mode::Status(200)).await;
        let (bot_id, secret) = register(&node, &root, ECHO, &bot.url, false).await;
        let ada = Actor::sign_in(&node, ADA).await;
        let bea = Actor::sign_in(&node, BEA).await;
        let chat = personal_chat(&id_of(&ada), &bot_id);

        let sent = ada.send(&chat, "hello echo, are you there").await;
        let got = bot.wait_for("message", &sent, Duration::from_secs(15)).await;
        assert_eq!(got.activity["text"], "hello echo, are you there");
        assert_eq!(got.activity["from"]["id"], format!("29:{}", id_of(&ada)));
        assert_eq!(got.activity["recipient"]["id"], format!("28:{bot_id}"));
        assert_eq!(got.activity["conversation"]["id"], chat);
        assert_eq!(got.service_url(), format!("{}/bot/", deployment.origin));
        let bearer = got.authorization.as_deref().unwrap().strip_prefix("Bearer ").unwrap();
        assert_eq!(crate::bots::claims(bearer)["aud"], bot_id);

        let conn = Conn {
            node: &node,
            base: got.service_url(),
            token: Some(bot_token(&node, &bot_id, &secret).await),
        };
        answers(&conn, &ada, &chat, &sent, &bot_id).await;
        ada_to_bot(&ada, &bot, &chat, &sent).await;
        bot_to_ada(&conn, &ada, &chat, &sent).await;
        roster_and_first_word(&conn, &root, &bea, &bot_id).await;
        who_may_not(&conn, &node, &root, &ada, &bea, &bot_id).await;
        not_its_chat(&conn, &ada, &bea).await;
        unsigned_rotated_and_off(&node, &root, &ada).await;
        keep_log(&node, &format!("talk-{backend:?}"));
        node.stop().await;
    }
}

/// The bot answers with text, Markdown and a card; Ada syncs them as the bot's.
async fn answers(conn: &Conn<'_>, ada: &Actor, chat: &str, sent: &str, bot_id: &str) {
    let (status, answer) = conn.reply(chat, sent, text("pong from echo")).await;
    assert_eq!(status, 200, "{answer}");
    let markdown = json!({"type": "message", "textFormat": "markdown",
        "text": "**bold** and [a link](https://docs.example.test/a)\n\n<script>alert(1)</script><img src=x onerror=alert(2)>"});
    assert_eq!(conn.send(chat, markdown).await.0, 200);
    let card = json!({"type": "AdaptiveCard", "version": "1.5",
        "body": [{"type": "TextBlock", "text": "Synthetic QA card"}],
        "actions": [{"type": "Action.OpenUrl", "title": "Open", "url": "https://docs.example.test/card"}]});
    let with_card = json!({"type": "message", "text": "a card follows", "attachments": [
        {"contentType": "application/vnd.microsoft.card.adaptive", "content": card}]});
    let (status, answer) = conn.send(chat, with_card).await;
    assert_eq!(status, 200, "{answer}");
    let messages = synced(ada, chat).await;
    let from_bot: Vec<&Message> = messages
        .iter()
        .filter(|message| message.sender.as_deref() == Some(&format!("28:{bot_id}")))
        .collect();
    assert_eq!(from_bot.len(), 4, "{messages:#?}");
    let named = from_bot
        .iter()
        .all(|message| message.sender_name.as_deref() == Some(ECHO));
    let texts = [
        from_bot[0].content.contains("pong from echo"),
        from_bot[1].content.contains("<strong>bold</strong>"),
        !from_bot[1].content.contains("<script") && !from_bot[1].content.contains("onerror"),
        from_bot[2].content.contains("a card follows"),
        from_bot[3].message_type == "RichText/Media_Card",
    ];
    assert!(named && texts.iter().all(|ok| *ok), "{texts:?} {from_bot:#?}");
    let parsed = teams_proto::chat::card::card_message(&from_bot[3].content).unwrap();
    assert!(parsed.drawn(), "{parsed:?}");
}

/// Ada's edit, delete and typing reach the bot.
async fn ada_to_bot(ada: &Actor, bot: &QaBot, chat: &str, sent: &str) {
    ada.write(Action::Edit {
        conversation: chat.to_owned(),
        message_id: sent.to_owned(),
        text: MessageText::plain("hello echo, edited").unwrap(),
    })
    .await;
    let edited = bot.wait_for("messageUpdate", sent, Duration::from_secs(15)).await;
    assert_eq!(edited.activity["text"], "hello echo, edited");
    let gone = ada.send(chat, "a message I take back").await;
    ada.write(Action::Delete {
        conversation: chat.to_owned(),
        message_id: gone.clone(),
    })
    .await;
    bot.wait_for("messageDelete", &gone, Duration::from_secs(15)).await;
    let typed = ada
        .engine()
        .transient(Transient::Typing {
            conversation: chat.to_owned(),
        })
        .await
        .unwrap();
    assert!(matches!(typed, TransientSent::Sent), "{typed:?}");
    bot.wait_for("typing", "", Duration::from_secs(15)).await;
}

/// The bot's typing, and its own message edited and deleted, reach Ada; Ada's message it can't change.
async fn bot_to_ada(conn: &Conn<'_>, ada: &Actor, chat: &str, sent: &str) {
    assert_eq!(conn.send(chat, json!({"type": "typing"})).await.0, 200);
    let (_, draft) = conn.send(chat, text("draft from echo")).await;
    let id = draft["id"].as_str().unwrap().to_owned();
    let path = format!("v3/conversations/{}/activities/{id}", encode(chat));
    let change = json!({"type": "message", "id": id, "text": "final from echo"});
    let (status, answer) = conn.call(Method::PUT, &path, Some(change)).await;
    assert_eq!(status, 200, "{answer}");
    let messages = synced(ada, chat).await;
    let mine = messages.iter().find(|message| message.id == id).unwrap();
    assert!(
        mine.content.contains("final from echo") && mine.edit_time.is_some(),
        "{mine:?}"
    );
    let path_ada = format!("v3/conversations/{}/activities/{sent}", encode(chat));
    let refused = [
        conn.call(Method::PUT, &path_ada, Some(text("not mine"))).await.0,
        conn.call(Method::DELETE, &path_ada, None).await.0,
    ];
    assert_eq!(refused, [403, 403]);
    assert_eq!(conn.call(Method::DELETE, &path, None).await.0, 200);
    let messages = synced(ada, chat).await;
    let mine = messages.iter().find(|message| message.id == id).unwrap();
    assert!(mine.delete_time.is_some(), "{mine:?}");
}

/// The bot reads its chat's roster and starts a 1:1 chat with Bea, who sees its first words.
async fn roster_and_first_word(conn: &Conn<'_>, root: &Secret, bea: &Actor, bot_id: &str) {
    let tenant = tenant(root);
    let create = json!({"bot": {"id": format!("28:{bot_id}")}, "isGroup": false,
        "members": [{"id": format!("29:{}", id_of(bea))}], "channelData": {"tenant": {"id": tenant}},
        "activity": {"type": "message", "text": "Hi Bea, I am the echo bot"}});
    let (status, answer) = conn.call(Method::POST, "v3/conversations", Some(create)).await;
    assert_eq!(status, 201, "{answer}");
    let chat = personal_chat(&id_of(bea), bot_id);
    assert_eq!(answer["id"], chat);
    let messages = synced(bea, &chat).await;
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("Hi Bea, I am the echo bot")),
        "{messages:#?}"
    );
    for path in ["members", "pagedmembers"] {
        let (status, members) = conn
            .call(Method::GET, &format!("v3/conversations/{}/{path}", encode(&chat)), None)
            .await;
        assert_eq!(status, 200, "{members}");
        let list = members.as_array().or_else(|| members["members"].as_array()).unwrap();
        assert_eq!(list.len(), 1, "{members}");
        assert_eq!(list[0]["userPrincipalName"], BEA);
    }
    let one = format!("v3/conversations/{}/members/{BEA}", encode(&chat));
    let (status, member) = conn.call(Method::GET, &one, None).await;
    assert_eq!(status, 200, "{member}");
    assert_eq!(member["id"], format!("29:{}", id_of(bea)));
}

/// Cora may not use the bot: no chat for her, no first word from the bot.
async fn who_may_not(conn: &Conn<'_>, node: &Node, root: &Secret, ada: &Actor, bea: &Actor, bot_id: &str) {
    let body = json!({"everyone": false, "users": [id_of(ada), id_of(bea)]});
    let (status, answer) = call(node, Method::PATCH, &app_path(bot_id), Some(root), Some(body)).await;
    assert_eq!(status, 200, "{answer}");
    let cora = Actor::sign_in(node, CORA).await;
    let cora_chat = personal_chat(&id_of(&cora), bot_id);
    let row = cora
        .engine()
        .write(Action::Send {
            conversation: cora_chat.clone(),
            text: MessageText::plain("let me in").unwrap(),
        })
        .await;
    let wrote = matches!(&row, Ok(row) if row.state == OutboxState::Sent);
    assert!(!wrote, "Cora wrote to a bot she may not use: {row:?}");
    let to_cora = json!({"bot": {"id": format!("28:{bot_id}")},
        "members": [{"id": format!("29:{}", id_of(&cora))}], "channelData": {"tenant": {"id": tenant(root)}},
        "activity": {"type": "message", "text": "uninvited"}});
    assert_eq!(conn.call(Method::POST, "v3/conversations", Some(to_cora)).await.0, 403);
    let status = conn.send(&cora_chat, text("uninvited")).await.0;
    assert!(matches!(status, 403 | 404), "{status}");
}

/// The bot can't write in a people chat of Ada and Bea, which it is not in.
async fn not_its_chat(conn: &Conn<'_>, ada: &Actor, bea: &Actor) {
    let theirs = ada.create(&[bea], None).await;
    let status = conn.send(&theirs, text("intrusion")).await.0;
    assert!(matches!(status, 403 | 404), "{status}");
    let messages = synced(bea, &theirs).await;
    assert!(!messages.iter().any(|message| message.content.contains("intrusion")));
}

fn has(messages: &[Message], text: &str) -> bool {
    messages.iter().any(|message| message.content.contains(text))
}

/// An unsigned bot: it gets no token and answers through the credential in its service URL. Its chat, its connector.
async fn unsigned<'a>(node: &'a Node, root: &Secret, ada: &Actor, plain: &QaBot) -> (String, String, Conn<'a>) {
    let (plain_id, _) = register(node, root, "Plain QA (synthetic)", &plain.url, true).await;
    let chat = personal_chat(&id_of(ada), &plain_id);
    let first = ada.send(&chat, "hello plain bot").await;
    let got = plain.wait_for("message", &first, Duration::from_secs(15)).await;
    assert!(got.authorization.is_none(), "an unsigned bot got a token");
    let old = Conn {
        node,
        base: got.service_url(),
        token: None,
    };
    assert!(old.base.contains(&format!("/bot/u/{plain_id}.")), "{}", old.base);
    assert_eq!(old.reply(&chat, &first, text("plain answer")).await.0, 200);
    (plain_id, chat, old)
}

/// An unsigned bot's place: its node and administrator, Ada, the bot, its chat with Ada and its id.
#[derive(Clone, Copy)]
struct Plain<'a, 'b> {
    node: &'a Node,
    root: &'b Secret,
    ada: &'b Actor,
    bot: &'b QaBot,
    chat: &'b str,
    id: &'b str,
}

/// A new secret closes the old URL; the next activity names the new one. The new connector.
async fn rotated<'a>(at: &Plain<'a, '_>, old: &Conn<'_>) -> Conn<'a> {
    let Plain {
        node,
        root,
        ada,
        bot: plain,
        chat,
        id: plain_id,
    } = *at;
    let path = format!("{}/secret", app_path(plain_id));
    let (status, rotated) = call(node, Method::POST, &path, Some(root), None).await;
    assert!(status == 200 || status == 201, "{rotated}");
    assert_eq!(old.send(chat, text("with the old secret")).await.0, 401);
    let second = ada.send(chat, "after the new secret").await;
    let got = plain.wait_for("message", &second, Duration::from_secs(15)).await;
    assert_ne!(got.service_url(), old.base);
    let new = Conn {
        node,
        base: got.service_url(),
        token: None,
    };
    assert_eq!(new.send(chat, text("with the new secret")).await.0, 200);
    new
}

/// An app turned off: its bot can't post and gets nothing.
async fn turned_off(at: &Plain<'_, '_>, new: &Conn<'_>) {
    let Plain {
        node,
        root,
        ada,
        bot: plain,
        chat,
        id: plain_id,
    } = *at;
    let off = json!({"enabled": false});
    let (status, answer) = call(node, Method::PATCH, &app_path(plain_id), Some(root), Some(off)).await;
    assert_eq!(status, 200, "{answer}");
    let status = new.send(chat, text("while off")).await.0;
    assert!(status == 403 || status == 401, "a turned-off bot posted: {status}");
    let hits = plain.hits();
    let row = ada
        .engine()
        .write(Action::Send {
            conversation: chat.to_owned(),
            text: MessageText::plain("are you off").unwrap(),
        })
        .await;
    eprintln!("talk: Ada writes to a turned-off bot: {:?}", row.map(|row| row.state));
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(plain.hits(), hits, "a turned-off bot got an activity");
    let messages = synced(ada, chat).await;
    let seen = [
        has(&messages, "with the new secret"),
        has(&messages, "with the old secret"),
        has(&messages, "while off"),
    ];
    assert_eq!(seen, [true, false, false]);
}

/// An unsigned bot, its secret rotated, its app turned off.
async fn unsigned_rotated_and_off(node: &Node, root: &Secret, ada: &Actor) {
    let plain = QaBot::start(Mode::Status(200)).await;
    let (plain_id, chat, old) = unsigned(node, root, ada, &plain).await;
    let at = Plain {
        node,
        root,
        ada,
        bot: &plain,
        chat: &chat,
        id: &plain_id,
    };
    let new = rotated(&at, &old).await;
    turned_off(&at, &new).await;
}

// --- the address guard ---

/// What the administrator's form refuses at once: plain `http`, user info, another scheme, not a URL.
async fn the_form_refuses(node: &Node, root: &Secret, port: u16) {
    for endpoint in [
        format!("http://127.0.0.1:{port}/api/messages"),
        format!("https://user:pw@127.0.0.1:{port}/api/messages"),
        "ftp://bots.example.test/x".to_owned(),
        "not a url".to_owned(),
    ] {
        let body = json!({"name": "Refused (synthetic)", "endpoint": endpoint});
        let (status, answer) = call(node, Method::POST, APPS, Some(root), Some(body)).await;
        assert_eq!(status, 400, "{endpoint}: {answer}");
        eprintln!(
            "guard: the form refuses {endpoint}: {} {}",
            code(&answer),
            answer["error"]["message"]
        );
    }
}

/// Whether the only row of a bot failed with `reason`, in `attempts` when given.
fn failed_with(rows: &[Delivery], reason: &str, attempts: Option<i32>) -> bool {
    rows.len() == 1
        && rows[0].state == FAILED
        && attempts.is_none_or(|attempts| rows[0].attempts == attempts)
        && rows[0]
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains(reason))
}

/// With no network allowed: an address written in the endpoint refused when saved, a name at the first send, with
/// its reason in the log; what went wrong.
async fn closed_guard(node: &Node, root: &Secret, ada: &Actor, port: u16) -> Vec<String> {
    let literal = [
        format!("https://127.0.0.1:{port}/api/messages"),
        format!("https://[::1]:{port}/api/messages"),
        format!("https://[::ffff:127.0.0.1]:{port}/api/messages"),
        "https://10.20.30.40/api/messages".to_owned(),
        "https://192.168.1.10/api/messages".to_owned(),
        "https://100.64.0.1/api/messages".to_owned(),
        "https://169.254.10.10/api/messages".to_owned(),
        "https://[fe80::1]/api/messages".to_owned(),
        "https://169.254.169.254/latest/meta-data".to_owned(),
        "https://[::ffff:169.254.169.254]/latest".to_owned(),
        "https://[fd00:ec2::254]/latest".to_owned(),
        "https://100.100.100.200/latest".to_owned(),
        "https://0.0.0.0/api/messages".to_owned(),
    ];
    let mut problems = Vec::new();
    for endpoint in &literal {
        let body = json!({"name": "Guard (synthetic)", "endpoint": endpoint});
        let (status, answer) = call(node, Method::POST, APPS, Some(root), Some(body)).await;
        eprintln!("guard: {endpoint}: saved {status} {}", code(&answer));
        let metadata = ["169.254.169.254", "fd00:ec2::254", "100.100.100.200"];
        let expected = if metadata.iter().any(|address| endpoint.contains(address)) {
            "EndpointMetadata"
        } else {
            "EndpointNotAllowed"
        };
        if (status, code(&answer)) != (400, expected) {
            problems.push(format!("{endpoint}: saved {status} {answer}"));
        }
    }
    let named = [format!("https://localhost:{port}/api/messages")];
    for (n, endpoint) in named.iter().enumerate() {
        let (id, _) = register(node, root, &format!("Guard {n} (synthetic)"), endpoint, false).await;
        ada.send(&personal_chat(&id_of(ada), &id), "is anyone there").await;
        let rows = settled(node, &id, 1, Duration::from_secs(15)).await;
        eprintln!("guard: {endpoint}: {:?}", summary(&rows));
        if !failed_with(&rows, "network", Some(1)) {
            problems.push(format!("{endpoint}: {:?}", summary(&rows)));
        }
    }
    let reasons = log_of(node).matches("failed for good").count();
    eprintln!("guard: {reasons} log lines name a send that failed for good");
    if reasons < named.len() {
        problems.push(format!("the log names {reasons} of {} refused sends", named.len()));
    }
    problems
}

/// Registers a bot for each case and writes to it; their ids.
async fn write_to_each(node: &Node, root: &Secret, ada: &Actor, cases: &[(&str, &str, &str)]) -> Vec<String> {
    let mut ids = Vec::new();
    for (name, endpoint, _) in cases {
        let (id, _) = register(node, root, &format!("{name} (synthetic)"), endpoint, false).await;
        ada.send(&personal_chat(&id_of(ada), &id), &format!("to {name}")).await;
        ids.push(id);
    }
    ids
}

/// The row of `id` failed with `reason`; otherwise what it shows.
async fn refused_for(node: &Node, name: &str, id: &str, reason: &str) -> Option<String> {
    let rows = settled(node, id, 1, Duration::from_secs(40)).await;
    eprintln!("guard: {name}: {:?}", summary(&rows));
    (!failed_with(&rows, reason, None)).then(|| format!("{name}: {:?}", summary(&rows)))
}

/// A metadata address, even with link-local open, refused when saved; otherwise what the server said.
async fn metadata_closed(node: &Node, root: &Secret) -> Option<String> {
    let metadata = json!({"name": "Metadata (synthetic)", "endpoint": "https://169.254.169.254/latest/meta-data"});
    let (status, answer) = call(node, Method::POST, APPS, Some(root), Some(metadata)).await;
    ((status, code(&answer)) != (400, "EndpointMetadata"))
        .then(|| format!("metadata with link-local open: saved {status} {answer}"))
}

/// With loopback and link-local allowed: a redirect, a slow and a huge answer fail with their reasons, the
/// redirect isn't followed, metadata stays closed (refused when saved), and the loopback bot gets its activity; what
/// went wrong.
async fn open_guard(node: &Node, root: &Secret, ada: &Actor, bot: &QaBot) -> Vec<String> {
    let other = QaBot::start(Mode::Status(200)).await;
    let redirect = QaBot::start(Mode::Redirect(other.url.clone())).await;
    let slow = QaBot::start(Mode::Slow(Duration::from_secs(6))).await;
    let huge = QaBot::start(Mode::Huge).await;
    let opened = register(node, root, "Opened (synthetic)", &bot.url, false).await.0;
    let cases: [(&str, &str, &str); 3] = [
        ("Redirect", &redirect.url, "307"),
        ("Slow", &slow.url, "timed out"),
        ("Huge", &huge.url, "bigger"),
    ];
    let ids = write_to_each(node, root, ada, &cases).await;
    let mut problems: Vec<String> = metadata_closed(node, root).await.into_iter().collect();
    let sent = ada
        .send(&personal_chat(&id_of(ada), &opened), "loopback is open now")
        .await;
    bot.wait_for("message", &sent, Duration::from_secs(15)).await;
    for ((name, _, reason), id) in cases.iter().zip(&ids) {
        problems.extend(refused_for(node, name, id, reason).await);
    }
    if other.hits() != 0 {
        problems.push("the redirect was followed".to_owned());
    }
    let rows = settled(node, &opened, 1, Duration::from_secs(5)).await;
    if rows.first().is_none_or(|row| row.state != SENT) {
        problems.push(format!("the opened loopback bot: {:?}", summary(&rows)));
    }
    problems
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_guard_refuses_what_an_admin_may_not_reach_and_a_cidr_opens_one() {
    if tls_parent("binary::bots::delivery::the_guard_refuses_what_an_admin_may_not_reach_and_a_cidr_opens_one") {
        return;
    }
    let table = "retry_delays = [1]\nsend_timeout = 2\ndelivery_lease = 6\n";
    let (_deployment, mut node, base, root) = one_node(Backend::Sqlite, table).await;
    let bot = QaBot::start(Mode::Status(200)).await;
    let port = url::Url::parse(&bot.url).unwrap().port().unwrap();
    let ada = Actor::sign_in(&node, ADA).await;
    the_form_refuses(&node, &root, port).await;
    let mut problems = closed_guard(&node, &root, &ada, port).await;
    assert_eq!(bot.hits(), 0, "a refused address reached the bot");
    keep_log(&node, "guard-closed");

    node.stop().await;
    let open = format!("{table}allowed_networks = [\"127.0.0.0/8\", \"169.254.0.0/16\", \"::1/128\"]\n");
    bots_table(&node, &base, &open);
    node.start().await;
    problems.extend(open_guard(&node, &root, &ada, &bot).await);
    keep_log(&node, "guard-open");
    node.stop().await;
    assert!(problems.is_empty(), "{problems:#?}");
}

// --- a dead endpoint ---

/// More failures until 20 in a row: the bot pauses for a minute, then gets its activities again.
async fn paused_and_back(node: &Node, ada: &Actor, bot: &QaBot, chat: &str, bot_id: &str) {
    // more until 20 failed sends in a row: then the bot pauses
    let mut n = 1;
    while bot.hits() < 20 {
        ada.send(chat, &format!("down {n}")).await;
        settled(node, bot_id, n + 1, Duration::from_secs(20)).await;
        n += 1;
    }
    let paused_at = bot.got()[19].at;
    let hits = bot.hits();
    eprintln!("dead: {hits} failed sends, {n} rows");
    bot.set(Mode::Status(200));
    let during = ada.send(chat, "during the pause").await;
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(bot.hits(), hits, "a paused bot got an activity");
    let back = bot.wait_for("message", &during, Duration::from_secs(70)).await;
    let waited = back.at - paused_at;
    eprintln!("dead: the bot got the next activity {waited:?} after the pause began");
    assert!(waited >= Duration::from_secs(50), "{waited:?}");
    let after = ada.send(chat, "after the pause").await;
    bot.wait_for("message", &after, Duration::from_secs(10)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_bot_is_retried_then_failed_paused_after_twenty_and_back_after_the_pause() {
    if tls_parent(
        "binary::bots::delivery::a_dead_bot_is_retried_then_failed_paused_after_twenty_and_back_after_the_pause",
    ) {
        return;
    }
    let table = "allowed_networks = [\"127.0.0.0/8\"]\nretry_delays = [1, 1]\n";
    let (_deployment, mut node, _, root) = one_node(Backend::Sqlite, table).await;
    let bot = QaBot::start(Mode::Status(503)).await;
    let (bot_id, _) = register(&node, &root, ECHO, &bot.url, false).await;
    let ada = Actor::sign_in(&node, ADA).await;
    let chat = personal_chat(&id_of(&ada), &bot_id);

    let first = ada.send(&chat, "down 0").await;
    let rows = settled(&node, &bot_id, 1, Duration::from_secs(20)).await;
    assert_eq!(
        summary(&rows),
        [(FAILED.to_owned(), 3, Some("the bot answered 503".to_owned()))]
    );
    let tries: Vec<Instant> = bot
        .got()
        .iter()
        .filter(|got| got.id() == first)
        .map(|got| got.at)
        .collect();
    assert_eq!(tries.len(), 3);
    eprintln!(
        "dead: the tries came {:?} and {:?} apart",
        tries[1] - tries[0],
        tries[2] - tries[1]
    );

    paused_and_back(&node, &ada, &bot, &chat, &bot_id).await;
    keep_log(&node, "dead");
    node.stop().await;
}

// --- two nodes and restarts ---

/// Two `api` nodes over TLS on Postgres and Redis, with Ada and Bea and the administrator.
async fn two_nodes(redis: String) -> (Deployment, Node, Node, Secret) {
    let deployment = Deployment::new(Backend::Postgres, true, Some(redis)).await;
    let (mut a, mut b) = (deployment.node("a"), deployment.node("b"));
    let table = "allowed_networks = [\"127.0.0.0/8\"]\nretry_delays = [1, 1]\nsend_timeout = 3\ndelivery_lease = 6\n";
    for node in [&mut a, &mut b] {
        let base = std::fs::read_to_string(&node.config).unwrap();
        bots_table(node, &base, table);
        node.role = Some(("api", None));
    }
    a.cli(&["migrate"], "");
    for upn in [ADA, BEA] {
        a.add_user(upn, PASSWORD);
    }
    a.start().await;
    b.start().await;
    let root = first_admin(&a).await;
    (deployment, a, b, root)
}

/// What the bot got against what was written: problems, when any.
fn verdict(sent: &[String], rows: &[Delivery], bot: &QaBot) -> Vec<String> {
    let not_sent: Vec<_> = rows
        .iter()
        .filter(|row| row.state != SENT)
        .map(|row| (&row.state, row.attempts, &row.last_error))
        .collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for got in bot.of("message") {
        *counts.entry(got.id().to_owned()).or_default() += 1;
    }
    let lost: Vec<&String> = sent.iter().filter(|id| !counts.contains_key(*id)).collect();
    let twice: Vec<(&String, &usize)> = counts.iter().filter(|(_, count)| **count > 1).collect();
    // a killed node's lease runs out and its rows go again under the same id (docs/server.md); a stop is graceful
    let after_a_stop: Vec<&(&String, &usize)> = twice.iter().filter(|(id, _)| sent[30..].contains(id)).collect();
    let strangers: Vec<&String> = counts.keys().filter(|id| !sent.contains(id)).collect();
    eprintln!(
        "nodes: {} written, {} rows, {} got, lost {lost:?}, twice {twice:?}, not sent {not_sent:?}",
        sent.len(),
        rows.len(),
        bot.of("message").len()
    );
    let mut problems = Vec::new();
    for (empty, what) in [
        (lost.is_empty(), format!("activities lost: {lost:?}")),
        (strangers.is_empty(), format!("activities nobody wrote: {strangers:?}")),
        (not_sent.is_empty(), format!("rows not sent: {not_sent:?}")),
        (
            after_a_stop.is_empty(),
            format!("activities sent twice after a graceful stop: {after_a_stop:?}"),
        ),
    ] {
        if !empty {
            problems.push(what);
        }
    }
    problems
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_nodes_and_restarts_lose_no_activity_and_send_none_twice() {
    if tls_parent("binary::bots::delivery::two_nodes_and_restarts_lose_no_activity_and_send_none_twice") {
        return;
    }
    let Some(redis) = redis_url().filter(|_| std::env::var_os(PG_ENV).is_some()) else {
        eprintln!("skipped: needs Postgres and Redis");
        return;
    };
    let (_deployment, mut a, mut b, root) = two_nodes(redis).await;
    let bot = QaBot::start(Mode::Slow(Duration::from_millis(300))).await;
    let (bot_id, _) = register(&a, &root, ECHO, &bot.url, false).await;
    let ada = Actor::sign_in(&a, ADA).await;
    let bea = Actor::sign_in(&b, BEA).await;
    let ada_chat = personal_chat(&id_of(&ada), &bot_id);
    let bea_chat = personal_chat(&id_of(&bea), &bot_id);
    let mut sent = Vec::new();

    // b is killed while activities go out, then comes back
    for n in 0..30 {
        sent.push(ada.send(&ada_chat, &format!("kill {n}")).await);
        if n == 10 {
            b.kill();
            eprintln!("nodes: b killed after {} activities reached the bot", bot.hits());
        }
    }
    b.start().await;
    // a is stopped (SIGTERM) and started while Bea writes through b
    for n in 0..20 {
        sent.push(bea.send(&bea_chat, &format!("restart {n}")).await);
        if n == 5 {
            a.stop().await;
            eprintln!("nodes: a stopped after {} activities reached the bot", bot.hits());
        }
        if n == 12 {
            a.start().await;
        }
    }
    // both stop while activities wait, then start again
    bot.set(Mode::Slow(Duration::from_secs(1)));
    for n in 0..8 {
        sent.push(bea.send(&bea_chat, &format!("stopped {n}")).await);
    }
    a.stop().await;
    b.stop().await;
    eprintln!("nodes: both stopped after {} activities reached the bot", bot.hits());
    bot.set(Mode::Slow(Duration::from_millis(50)));
    a.start().await;
    b.start().await;

    let rows = settled(&a, &bot_id, sent.len(), Duration::from_secs(90)).await;
    let problems = verdict(&sent, &rows, &bot);
    keep_log(&a, "nodes-a");
    keep_log(&b, "nodes-b");
    a.stop().await;
    b.stop().await;
    assert!(problems.is_empty(), "{problems:#?}");
}
