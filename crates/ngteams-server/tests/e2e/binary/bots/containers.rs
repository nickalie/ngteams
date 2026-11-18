//! Real SDK bots in containers (`tests/bots/`) against the real binary over TLS, when `NGTEAMS_SERVER_BOT_IMAGES`
//! names them (`teams`, `agents`; CI's Coverage job builds both). Each image is the SDK's own echo-and-cards bot, run
//! with `podman` (or `NGTEAMS_SERVER_BOT_ENGINE`) on the host's network (or `NGTEAMS_SERVER_BOT_NETWORK`), the test
//! CA given in its environment. A person writes through the client's own engine and syncs what the SDK answered:
//! text, Markdown, a card, an edit, a delete, typing, a member read and a proactive message; an administrator sends
//! the bot a test activity. Every account, bot and text is synthetic.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_store::Message;

use super::super::support::{Deployment, Node, Port, TLS_DIR};
use super::delivery::{Actor, id_of, personal_chat, synced};
use super::sdk::tls_parent;
use super::support::{APPS, app_path, call, first_admin, tenant};
use crate::support::{Backend, PASSWORD};

/// The SDK bots to run, by name (`teams,agents`).
pub const IMAGES: &str = "NGTEAMS_SERVER_BOT_IMAGES";
/// The container engine: `podman` unless set (CI: `docker`, through the runner's socket).
const ENGINE: &str = "NGTEAMS_SERVER_BOT_ENGINE";
/// The network a bot joins: the host's unless set (CI: the measuring container's, `container:<name>`).
const NETWORK: &str = "NGTEAMS_SERVER_BOT_NETWORK";
const ADA: &str = "ada@sdk.own.test";

/// One SDK's bot: its image and the name it signs its answers with.
struct Sdk {
    image: &'static str,
    name: &'static str,
}

fn sdks() -> Option<Vec<Sdk>> {
    let wanted = std::env::var(IMAGES).ok()?;
    let all = [
        ("teams", "localhost/ngteams-bot-teams:1", "teams-sdk"),
        ("agents", "localhost/ngteams-bot-agents:1", "agents-sdk"),
    ];
    Some(
        all.iter()
            .filter(|(key, ..)| wanted.split(',').any(|name| name.trim() == *key))
            .map(|(_, image, name)| Sdk { image, name })
            .collect(),
    )
}

fn engine() -> String {
    std::env::var(ENGINE).unwrap_or_else(|_| "podman".to_owned())
}

/// A bot container, removed when dropped.
struct Container {
    name: String,
}

impl Container {
    /// Runs `sdk`'s image as the bot `id` with `secret`, listening on `port`.
    fn run(sdk: &Sdk, node: &Node, tenant: &str, id: &str, secret: &str, port: u16) -> Self {
        let name = format!("ngts524-bot-{}-{port}", sdk.name);
        let ca =
            std::fs::read_to_string(std::path::Path::new(&std::env::var_os(TLS_DIR).unwrap()).join("ca.pem")).unwrap();
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

    fn logs(&self) -> String {
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
    let deadline = Instant::now() + Duration::from_secs(60);
    while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
        assert!(
            Instant::now() < deadline,
            "the bot never listened: {}",
            container.logs()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The bot's messages in `chat` once one of them contains `text` (Ada syncs meanwhile).
async fn answered(ada: &Actor, chat: &str, bot: &str, text: &str, container: &Container) -> Vec<Message> {
    let sender = format!("28:{bot}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let from_bot: Vec<Message> = synced(ada, chat)
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

/// A server, its administrator and Ada; the bot `sdk` registered and running. Its id with what keeps it alive.
struct Running {
    _deployment: Deployment,
    node: Node,
    root: Secret,
    ada: Actor,
    bot: String,
    chat: String,
    container: Container,
    _port: Port,
}

async fn running(sdk: &Sdk) -> Running {
    let deployment = Deployment::new(Backend::Sqlite, true, None).await;
    let mut node = deployment.node("a");
    super::support::configure(
        &node,
        "",
        "[bots]\nallow_http_endpoints = true\nallowed_networks = [\"127.0.0.0/8\"]\nretry_delays = [1, 1]\n",
    );
    node.cli(&["migrate"], "");
    node.add_user(ADA, PASSWORD);
    node.start().await;
    let root = first_admin(&node).await;
    let held = Port::take();
    let port = held.number;
    let endpoint = format!("http://127.0.0.1:{port}/api/messages");
    let body = json!({"name": format!("{} echo (synthetic)", sdk.name), "endpoint": endpoint});
    let (status, answer) = call(&node, Method::POST, APPS, Some(&root), Some(body)).await;
    assert_eq!(status, 201, "{answer}");
    let bot = answer["id"].as_str().unwrap().to_owned();
    let secret = answer["secret"].as_str().unwrap();
    let container = Container::run(sdk, &node, &tenant(&root), &bot, secret, port);
    listening(port, &container).await;
    let ada = Actor::sign_in(&node, ADA).await;
    let chat = personal_chat(&id_of(&ada), &bot);
    Running {
        _deployment: deployment,
        node,
        root,
        ada,
        bot,
        chat,
        container,
        _port: held,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_bots_in_containers_answer_a_person_through_the_binary() {
    let Some(sdks) = sdks() else {
        eprintln!("skipped: {IMAGES} is not set");
        return;
    };
    if tls_parent("binary::bots::containers::sdk_bots_in_containers_answer_a_person_through_the_binary") {
        return;
    }
    for sdk in &sdks {
        eprintln!("sdk bot: {}", sdk.name);
        let mut run = running(sdk).await;
        talk(&run, sdk).await;
        test_activity(&run).await;
        run.node.stop().await;
    }
}

/// A word Ada sends and the text of the bot's answer it waits for; the bot's messages then.
async fn ask(run: &Running, word: &str, answer: &str) -> Vec<Message> {
    run.ada.send(&run.chat, word).await;
    answered(&run.ada, &run.chat, &run.bot, answer, &run.container).await
}

async fn talk(run: &Running, sdk: &Sdk) {
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
    assert!(member.content.contains("QA Operator"), "{member:?}");
    ask(run, "proactive", &format!("proactive from {name}")).await;
}

/// The bot's card: its `Action.Submit` with the SDK's data, and a link.
async fn card(run: &Running, name: &str) {
    run.ada.send(&run.chat, "card").await;
    let card = card_message(&run.ada, &run.chat, &run.bot, &run.container).await;
    let parsed = teams_proto::chat::card::card_message(&card.content).unwrap();
    let actions = format!("{parsed:?}");
    assert!(actions.contains("Submit(BotAction"), "{actions}");
    assert!(actions.contains(&format!("\"sdk\": String(\"{name}\")")), "{actions}");
    assert!(
        actions.contains("OpenUrl(\"https://docs.example.test/sdk\")"),
        "{actions}"
    );
}

/// The bot's card message, once it came.
async fn card_message(ada: &Actor, chat: &str, bot: &str, container: &Container) -> Message {
    let sender = format!("28:{bot}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let card = synced(ada, chat).await.into_iter().find(|message| {
            message.sender.as_deref() == Some(sender.as_str()) && message.message_type == "RichText/Media_Card"
        });
        if let Some(card) = card {
            return card;
        }
        assert!(Instant::now() < deadline, "no card came: {}", container.logs());
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// The administrator's test activity: the SDK takes it.
async fn test_activity(run: &Running) {
    let path = format!("{}/test", app_path(&run.bot));
    let (status, answer) = call(&run.node, Method::POST, &path, Some(&run.root), None).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        (&answer["delivered"], &answer["status"]),
        (&Value::Bool(true), &json!(200)),
        "{answer}\n{}",
        run.container.logs()
    );
}

/// A card's plain `Action.Submit`, pressed through the engine's own invoke: the SDK gets a message carrying the
/// card's data and answers, and its answer comes back in the chat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the server's bot invokes (part 8, 230, team/bots-8): remove at the rebase after it lands"]
async fn sdk_bots_in_containers_answer_a_card_submit() {
    let Some(sdks) = sdks() else {
        eprintln!("skipped: {IMAGES} is not set");
        return;
    };
    if tls_parent("binary::bots::containers::sdk_bots_in_containers_answer_a_card_submit") {
        return;
    }
    for sdk in &sdks {
        eprintln!("sdk bot card submit: {}", sdk.name);
        let mut run = running(sdk).await;
        submit(&run, sdk).await;
        run.node.stop().await;
    }
}

async fn submit(run: &Running, sdk: &Sdk) {
    let (ada, chat, bot, container) = (&run.ada, run.chat.as_str(), run.bot.as_str(), &run.container);
    ada.send(chat, "card").await;
    let card = card_message(ada, chat, bot, container).await;
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
    let ask = teams_core::bots::BotAsk {
        bot: teams_proto::Mri::new(format!("28:{bot}")),
        app_id: bot.to_owned(),
        conversation: Some(chat.to_owned()),
        kind: teams_proto::InvokeKind::Submit(teams_proto::CardInvoke {
            message: Some(card.id.clone()),
            id: submit.id,
            data,
            ..teams_proto::CardInvoke::default()
        }),
    };
    let answer = ada.engine().bot_invoke(ask).await.unwrap();
    eprintln!("  the invoke's answer: {answer:?}");
    let expected = format!(
        "submitted from {}: comment=because (synthetic) choice=yes sdk={}",
        sdk.name, sdk.name
    );
    answered(ada, chat, bot, &expected, container).await;
}
