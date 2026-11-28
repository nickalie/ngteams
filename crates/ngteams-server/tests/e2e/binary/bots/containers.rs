//! The sample bots (`packaging/server/bots/`) in containers against the real binary over TLS, when
//! `NGTEAMS_SERVER_BOT_IMAGES` names them (`teams`, `agents`, `botbuilder-python`, `botbuilder-dotnet`; CI's Coverage
//! job builds the first two). Each image is one SDK's sample, uploaded as its own Teams app package and run with
//! `podman` (or `NGTEAMS_SERVER_BOT_ENGINE`) on the host's network (or `NGTEAMS_SERVER_BOT_NETWORK`), the test CA given
//! in its environment. A person writes through the client's own engine and syncs what the SDK answered: text,
//! Markdown, a card, an edit, a delete, typing, a member read, a proactive message, a card's submit and
//! `Action.Execute`, a search and an action command, and a mention in a group chat and a channel; an administrator
//! sends the bot a test activity. With `NGTEAMS_SERVER_PG_URL` and `NGTEAMS_SERVER_REDIS_URL`, the same on two `api`
//! nodes: Ada writes on one and reads on the other. Every account, bot and text is synthetic.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_store::Message;

use super::super::support::{Deployment, Node, Port, TLS_DIR};
use super::packages::new_id;
use super::sdk::tls_parent;
use super::support::{APPS, app_path, call, first_admin, tenant, upload};
use crate::support::{Backend, PASSWORD, PG_ENV, redis_url};

mod flows;
pub(super) mod person;

use person::Person;

/// The SDK bots to run, by name (`teams,agents,botbuilder-python,botbuilder-dotnet`).
pub const IMAGES: &str = "NGTEAMS_SERVER_BOT_IMAGES";
/// The container engine: `podman` unless set (CI: `docker`, through the runner's socket).
const ENGINE: &str = "NGTEAMS_SERVER_BOT_ENGINE";
/// The network a bot joins: the host's unless set (CI: the measuring container's, `container:<name>`).
const NETWORK: &str = "NGTEAMS_SERVER_BOT_NETWORK";
const ADA: &str = "ada@sdk.own.test";
const BEA: &str = "bea@sdk.own.test";
const CORA: &str = "cora@sdk.own.test";
/// What every account's name is (`Node::add_user`).
const PERSON: &str = "QA Operator";

/// One SDK's sample bot: its directory under `packaging/server/bots`, its image and the name it signs its answers with.
pub(super) struct Sdk {
    dir: &'static str,
    image: &'static str,
    name: &'static str,
}

fn sdks() -> Option<Vec<Sdk>> {
    let wanted = std::env::var(IMAGES).ok()?;
    let all = [
        ("teams", "localhost/ngteams-bot-teams:1", "teams-sdk"),
        ("agents", "localhost/ngteams-bot-agents:1", "agents-sdk"),
        (
            "botbuilder-python",
            "localhost/ngteams-bot-botbuilder-python:1",
            "botbuilder-python",
        ),
        (
            "botbuilder-dotnet",
            "localhost/ngteams-bot-botbuilder-dotnet:1",
            "botbuilder-dotnet",
        ),
    ];
    Some(
        all.iter()
            .filter(|(key, ..)| wanted.split(',').any(|name| name.trim() == *key))
            .map(|&(dir, image, name)| Sdk { dir, image, name })
            .collect(),
    )
}

fn engine() -> String {
    std::env::var(ENGINE).unwrap_or_else(|_| "podman".to_owned())
}

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/server/bots")
}

/// A bot container, removed when dropped.
pub(super) struct Container {
    name: String,
}

impl Container {
    /// Runs `sdk`'s image as the bot `id` with `secret`, listening on `port`.
    fn run(sdk: &Sdk, node: &Node, tenant: &str, id: &str, secret: &str, port: u16) -> Self {
        let name = format!("ngts524-bot-{}-{port}", sdk.name);
        let ca = std::fs::read_to_string(Path::new(&std::env::var_os(TLS_DIR).unwrap()).join("ca.pem")).unwrap();
        let network = std::env::var(NETWORK).unwrap_or_else(|_| "host".to_owned());
        let port_text = port.to_string();
        let env = [
            ("BOT_ID", id),
            ("BOT_SECRET", secret),
            ("TENANT", tenant),
            ("ORIGIN", node.endpoint.as_str()),
            ("SCOPE", super::support::SCOPE),
            ("PORT", port_text.as_str()),
            ("BOT_CA_PEM", ca.as_str()),
        ];
        let mut command = Command::new(engine());
        command.args(["run", "-d", "--rm", "--name", &name, "--network", &network]);
        // values go through the engine's environment, never its arguments
        for (key, value) in env {
            command.arg("-e").arg(key);
            command.env(key, value);
        }
        let output = command.arg(sdk.image).stderr(Stdio::piped()).output().unwrap();
        assert!(
            output.status.success(),
            "{} run {}: {}",
            engine(),
            sdk.image,
            String::from_utf8_lossy(&output.stderr)
        );
        Self { name }
    }

    pub(super) fn logs(&self) -> String {
        let output = Command::new(engine()).args(["logs", &self.name]).output().unwrap();
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new(engine())
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Waits until the bot listens on `port`.
async fn listening(port: u16, container: &Container) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        assert!(
            Instant::now() < deadline,
            "the bot never listened: {}",
            container.logs()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// `seat`'s stored messages of `chat` after a sync, the oldest first.
pub(super) async fn synced(seat: &Person, chat: &str) -> Vec<Message> {
    seat.sync().await;
    let mut page = seat.engine().messages(chat, None, 200).await.unwrap().messages;
    page.reverse();
    page
}

/// The bot's messages in `chat` once one of them contains `text` (`seat` syncs meanwhile).
pub(super) async fn answered(seat: &Person, chat: &str, bot: &str, text: &str, container: &Container) -> Vec<Message> {
    let sender = format!("28:{bot}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let from_bot: Vec<Message> = synced(seat, chat)
            .await
            .into_iter()
            .filter(|message| message.sender.as_deref() == Some(sender.as_str()))
            .collect();
        if from_bot.iter().any(|message| message.content.contains(text)) {
            return from_bot;
        }
        assert!(
            Instant::now() < deadline,
            "no answer with {text:?}: {:#?}\n{}",
            from_bot.iter().map(|message| &message.content).collect::<Vec<_>>(),
            container.logs()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn find<'a>(messages: &'a [Message], text: &str) -> &'a Message {
    messages.iter().find(|message| message.content.contains(text)).unwrap()
}

/// An account's id, as its MRI and a bot chat's id carry it.
fn id_of(seat: &Person) -> String {
    seat.mri
        .trim_start_matches("8:")
        .trim_start_matches("orgid:")
        .to_owned()
}

fn personal_chat(user: &str, bot: &str) -> String {
    let mut ids = [user, bot];
    ids.sort_unstable();
    format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1])
}

/// The nodes: one `all` node on SQLite, or two `api` nodes on Postgres and Redis.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Nodes {
    One,
    Two,
}

/// A server, its administrator, Ada (writing at the last node, and reading at the first when there are two) and
/// Bea and Cora; the sample of `sdk` uploaded as its package and running. Its ids with what keeps it alive.
pub(super) struct Running {
    _deployment: Deployment,
    nodes: Vec<Node>,
    root: Secret,
    pub(super) ada: Person,
    ada_there: Option<Person>,
    pub(super) bea: Person,
    pub(super) cora: Person,
    /// The app's id (the manifest's `id`) and its bot's.
    pub(super) app: String,
    pub(super) bot: String,
    /// The app's short name, as a mention names its bot.
    pub(super) short_name: String,
    pub(super) chat: String,
    pub(super) container: Container,
    _port: Port,
}

impl Running {
    /// Where Ada reads: at the first node when there are two.
    pub(super) fn reader(&self) -> &Person {
        self.ada_there.as_ref().unwrap_or(&self.ada)
    }

    fn node(&self) -> &Node {
        &self.nodes[0]
    }

    pub(super) async fn stop(mut self) {
        for node in &mut self.nodes {
            node.stop().await;
        }
    }
}

/// A sample's Teams app package with new ids in its manifest; the package, the app's id and short name and its bot's id.
fn sample_package(sdk: &Sdk) -> (bytes::Bytes, String, String, String) {
    let dir = samples().join(sdk.dir);
    let (app, bot) = (new_id(), new_id());
    let text = std::fs::read_to_string(dir.join("manifest.json"))
        .unwrap()
        .replace("${APP_ID}", &app)
        .replace("${BOT_ID}", &bot);
    let manifest: Value = serde_json::from_str(&text).unwrap();
    let short_name = manifest["name"]["short"].as_str().unwrap().to_owned();
    let icon = |name: &str| std::fs::read(dir.join(name)).unwrap();
    let package = crate::bots::package(&manifest, &icon("color.png"), &icon("outline.png"), &[]);
    (package, app, short_name, bot)
}

async fn deployment(nodes: Nodes, extra: &str) -> (Deployment, Vec<Node>) {
    let two = nodes == Nodes::Two;
    let (backend, redis) = if two {
        (Backend::Postgres, redis_url())
    } else {
        (Backend::Sqlite, None)
    };
    let deployment = Deployment::new(backend, true, redis).await;
    let mut started: Vec<Node> = ["a", "b"][..if two { 2 } else { 1 }]
        .iter()
        .map(|name| deployment.node(name))
        .collect();
    for node in &mut started {
        super::support::configure(
            node,
            "",
            &format!(
                "[bots]\nallow_http_endpoints = true\nallowed_networks = [\"127.0.0.0/8\"]\nretry_delays = [1, 1]\n{extra}"
            ),
        );
        if two {
            node.role = Some(("api", None));
        }
    }
    started[0].cli(&["migrate"], "");
    for upn in [ADA, BEA, CORA] {
        started[0].add_user(upn, PASSWORD);
    }
    for node in &mut started {
        node.start().await;
    }
    (deployment, started)
}

pub(super) async fn running(sdk: &Sdk, nodes: Nodes) -> Running {
    running_with(sdk, nodes, "").await
}

/// [`running`], with `bots` holding `extra` too.
async fn running_with(sdk: &Sdk, nodes: Nodes, extra: &str) -> Running {
    let (deployment, nodes) = deployment(nodes, extra).await;
    let node = &nodes[0];
    let root = first_admin(node).await;
    let (package, app, short_name, bot) = sample_package(sdk);
    let (status, answer) = upload(node, &root, Method::POST, &format!("{APPS}/package"), package).await;
    assert_eq!(status, 201, "{answer}");
    let secret = answer["secret"].as_str().unwrap().to_owned();
    let held = Port::take();
    let port = held.number;
    let endpoint = format!("http://127.0.0.1:{port}/api/messages");
    let body = json!({"endpoint": endpoint});
    let (status, answer) = call(node, Method::PATCH, &app_path(&app), Some(&root), Some(body)).await;
    assert_eq!(status, 200, "{answer}");
    let container = Container::run(sdk, node, &tenant(&root), &bot, &secret, port);
    listening(port, &container).await;
    // the public origin is the first node's: the bot calls it, and only an engine there follows the server's links
    let ada = Person::sign_in(nodes.last().unwrap(), ADA).await;
    let ada_there = if nodes.len() > 1 {
        Some(Person::sign_in(node, ADA).await)
    } else {
        None
    };
    let bea = Person::sign_in(node, BEA).await;
    let cora = Person::sign_in(node, CORA).await;
    let chat = personal_chat(&id_of(&ada), &bot);
    Running {
        _deployment: deployment,
        nodes,
        root,
        ada,
        ada_there,
        bea,
        cora,
        app,
        bot,
        short_name,
        chat,
        container,
        _port: held,
    }
}

/// The SDKs the test `test` runs on `nodes`: none when the images, or Postgres and Redis for two nodes, are not named,
/// and none in the parent of the TLS child (its name for the child).
pub(super) fn ready(test: &str, nodes: Nodes) -> Option<Vec<Sdk>> {
    let Some(sdks) = sdks() else {
        eprintln!("skipped: {IMAGES} is not set");
        return None;
    };
    if nodes == Nodes::Two && (std::env::var_os(PG_ENV).is_none() || redis_url().is_none()) {
        eprintln!("skipped: needs Postgres and Redis");
        return None;
    }
    if tls_parent(&format!("binary::bots::containers::{test}")) {
        return None;
    }
    Some(sdks)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_bots_in_containers_answer_a_person_through_the_binary() {
    let test = "sdk_bots_in_containers_answer_a_person_through_the_binary";
    for sdk in ready(test, Nodes::One).unwrap_or_default() {
        eprintln!("sdk bot talk: {}", sdk.name);
        let run = running(&sdk, Nodes::One).await;
        talk(&run, &sdk).await;
        test_activity(&run).await;
        run.stop().await;
    }
}

/// A word Ada sends and the text of the bot's answer she waits for; the bot's messages then.
pub(super) async fn ask(run: &Running, word: &str, answer: &str) -> Vec<Message> {
    run.ada.send(&run.chat, word).await;
    answered(run.reader(), &run.chat, &run.bot, answer, &run.container).await
}

pub(super) async fn talk(run: &Running, sdk: &Sdk) {
    let name = sdk.name;
    ask(
        run,
        &format!("hello {name}"),
        &format!("echo from {name}: hello {name}"),
    )
    .await;
    // the SDK's Markdown, as the server's HTML
    ask(run, "markdown", &format!("<strong>bold</strong> from {name}")).await;
    card(run, name).await;
    let got = ask(run, "edit", &format!("final from {name}")).await;
    let edited = find(&got, &format!("final from {name}"));
    assert!(edited.edit_time.is_some(), "{edited:?}");
    let got = ask(run, "delete", &format!("deleted from {name}")).await;
    assert!(got.iter().any(|message| message.delete_time.is_some()), "{got:#?}");
    ask(run, "typing", &format!("typed from {name}")).await;
    let got = ask(run, "member", &format!("member from {name}: ")).await;
    let member = find(&got, &format!("member from {name}: "));
    assert!(member.content.contains(PERSON), "{member:?}");
    ask(run, "proactive", &format!("proactive from {name}")).await;
}

/// The bot's card: its `Action.Submit` with the SDK's data, and a link.
async fn card(run: &Running, name: &str) {
    run.ada.send(&run.chat, "card").await;
    let card = card_message(run, "Pick one").await;
    let parsed = teams_proto::chat::card::card_message(&card.content).unwrap();
    let actions = format!("{parsed:?}");
    assert!(actions.contains("Submit(BotAction"), "{actions}");
    assert!(actions.contains(&format!("\"sdk\": String(\"{name}\")")), "{actions}");
    assert!(
        actions.contains("OpenUrl(\"https://docs.example.test/sdk\")"),
        "{actions}"
    );
}

/// The bot's card message with `text`, once it came.
pub(super) async fn card_message(run: &Running, text: &str) -> Message {
    let sender = format!("28:{}", run.bot);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let card = synced(run.reader(), &run.chat).await.into_iter().find(|message| {
            message.sender.as_deref() == Some(sender.as_str())
                && message.message_type == "RichText/Media_Card"
                && teams_proto::chat::card::card_message(&message.content)
                    .is_some_and(|cards| format!("{cards:?}").contains(text))
        });
        if let Some(card) = card {
            return card;
        }
        assert!(Instant::now() < deadline, "no card came: {}", run.container.logs());
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// The administrator's test activity: the SDK takes it (botbuilder-python answers 201).
async fn test_activity(run: &Running) {
    let path = format!("{}/test", app_path(&run.app));
    let (status, answer) = call(run.node(), Method::POST, &path, Some(&run.root), None).await;
    assert_eq!(status, 200, "{answer}");
    let took = answer["delivered"] == Value::Bool(true)
        && answer["status"]
            .as_u64()
            .is_some_and(|status| (200..300).contains(&status));
    assert!(took, "{answer}\n{}", run.container.logs());
}

/// A card's plain `Action.Submit`, pressed through the engine's own invoke: the SDK gets a message carrying the
/// card's data and answers, and its answer comes back in the chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_bots_in_containers_answer_a_card_submit() {
    for sdk in ready("sdk_bots_in_containers_answer_a_card_submit", Nodes::One).unwrap_or_default() {
        eprintln!("sdk bot card submit: {}", sdk.name);
        let run = running(&sdk, Nodes::One).await;
        submit(&run, &sdk).await;
        run.stop().await;
    }
}

pub(super) fn ask_bot(run: &Running, kind: teams_proto::InvokeKind) -> teams_core::bots::BotAsk {
    teams_core::bots::BotAsk {
        bot: teams_proto::Mri::new(format!("28:{}", run.bot)),
        app_id: run.app.clone(),
        conversation: Some(run.chat.clone()),
        kind,
    }
}

pub(super) async fn submit(run: &Running, sdk: &Sdk) {
    run.ada.send(&run.chat, "card").await;
    let card = card_message(run, "Pick one").await;
    let parsed = teams_proto::chat::card::card_message(&card.content).unwrap();
    let submit = parsed.cards[0]
        .actions
        .iter()
        .find_map(|action| match &action.kind {
            teams_proto::chat::card::ActionKind::Submit(submit) => Some(submit.clone()),
            _ => None,
        })
        .unwrap();
    let mut data = submit.data.unwrap_or_else(|| json!({}));
    data["comment"] = json!("because (synthetic)");
    let ask = ask_bot(
        run,
        teams_proto::InvokeKind::Submit(teams_proto::CardInvoke {
            message: Some(card.id.clone()),
            id: submit.id,
            data,
            ..teams_proto::CardInvoke::default()
        }),
    );
    let answer = run.reader().engine().bot_invoke(ask).await.unwrap();
    eprintln!("  the invoke's answer: {answer:?}");
    let expected = format!(
        "submitted from {}: choice=yes comment=because (synthetic) sdk={}",
        sdk.name, sdk.name
    );
    answered(run.reader(), &run.chat, &run.bot, &expected, &run.container).await;
}

/// The Teams SDK's OAuth flow against the test identity provider: the bot's card, the person's browser through the
/// provider to the code, the engine's `signin/verifyState`, the bot signed in with the provider's token (it says a
/// digest of it), then sign-out and a new card.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_sdk_bots_sign_in_prompt_completes_from_the_engine() {
    let Some(sdks) = sdks() else {
        eprintln!("skipped: {IMAGES} is not set");
        return;
    };
    let Some(sdk) = sdks.iter().find(|sdk| sdk.name == "teams-sdk") else {
        eprintln!("skipped: {IMAGES} names no teams image");
        return;
    };
    if tls_parent("binary::bots::containers::an_sdk_bots_sign_in_prompt_completes_from_the_engine") {
        return;
    }
    let idp = crate::bots::idp::Idp::start().await;
    let key = crate::bots::sign_in::token_key();
    let run = running_with(sdk, Nodes::One, &format!("token_key = \"{key}\"\n")).await;
    let path = format!("{}/connections/synthetic-idp", app_path(&run.app));
    let body = json!({"authorize_url": idp.authorize_url(), "token_url": idp.token_url(),
        "client_id": crate::bots::idp::CLIENT_ID, "client_secret": crate::bots::idp::CLIENT_SECRET, "scopes": "read"});
    let (status, answer) = call(run.node(), Method::PUT, &path, Some(&run.root), Some(body)).await;
    assert_eq!(status, 201, "{answer}");
    sign_in(&run, sdk).await;
    run.stop().await;
}

/// The sign-in link of the newest OAuth card from the bot, once `count` came.
async fn sign_in_card(run: &Running, count: usize) -> String {
    let sender = format!("28:{}", run.bot);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let links: Vec<String> = synced(&run.ada, &run.chat)
            .await
            .into_iter()
            .filter(|message| message.sender.as_deref() == Some(sender.as_str()))
            .filter_map(|message| teams_proto::chat::card::card_message(&message.content))
            .flat_map(|cards| cards.cards)
            .flat_map(|card| card.actions)
            .filter_map(|action| match action.kind {
                teams_proto::chat::card::ActionKind::Submit(teams_proto::chat::card::BotAction {
                    via: teams_proto::chat::card::Via::SignIn(Some(link)),
                    ..
                }) => Some(link),
                _ => None,
            })
            .collect();
        if links.len() >= count {
            return links.last().unwrap().clone();
        }
        assert!(
            Instant::now() < deadline,
            "no sign-in card came: {}",
            run.container.logs()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn sign_in(run: &Running, sdk: &Sdk) {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    let name = sdk.name;
    run.ada.send(&run.chat, "signin").await;
    let link = sign_in_card(run, 1).await;
    let endpoint = &run.node().endpoint;
    let (code, page) = crate::bots::idp::sign_in_in_browser(&link, endpoint, endpoint).await;
    assert_eq!(code.len(), 6, "{page}");
    let verify = ask_bot(run, teams_proto::InvokeKind::VerifyState { code });
    let answer = run.ada.engine().bot_invoke(verify).await.unwrap();
    assert_eq!(answer, teams_proto::InvokeAnswer::Done, "{}", run.container.logs());
    let hex = sha2::Sha256::digest(b"idp-access-1")
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let digest = &hex[..12];
    answered(
        &run.ada,
        &run.chat,
        &run.bot,
        &format!("signed in to {name}: {digest}"),
        &run.container,
    )
    .await;
    ask(run, "signin", &format!("already signed in to {name}: {digest}")).await;
    ask(run, "signout", &format!("signed out of {name}")).await;
    run.ada.send(&run.chat, "signin").await;
    sign_in_card(run, 2).await;
}
