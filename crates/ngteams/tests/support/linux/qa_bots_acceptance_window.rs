//! Bots on an own server as an administrator and a user use them through the app's windows, each app in a UI sandbox
//! of its own (`harness = false`: GPUI runs on the process's main thread). It is the acceptance test of task 234.
//!
//! The parent deploys the real `ngteams-server` binary on two API nodes and a media node on one Postgres and one Redis
//! behind one front (`own_server_nodes.rs`, as `own_server_nodes_window.rs` deploys it), its `[bots]` open to
//! loopback. `srv-a` (an administrator) and `srv-b` sign in from the first sign-in screen by typing, each served by an
//! API node of its own ([`instance::Instance`]). Two sample bots of `packaging/server/bots/` run in containers on the
//! host's network: the Teams SDK v2 (TypeScript) one, or the Agents SDK (JavaScript) one with
//! `QA_BOTS_MAINTAINED=agents`, and the botbuilder (Python) one. For each bot:
//!
//! 1. `srv-a` uploads the sample's app package in Settings → Manage apps and sets its bot's endpoint; the bot runs
//!    with the secret the window showed.
//! 2. `srv-b` adds the app in the Store and opens its chat from the app's details; writes to the bot and reads its
//!    answer; fills the bot's card (a click in its field, typed words) and presses its button (a click), and reads
//!    the bot's answer to the submit; runs the search command from the compose box's "+" (typed words, the first
//!    result with Enter, sent) and the order command's dialog (its field and its button clicked); adds the app to a
//!    group chat and to a team, and mentions the bot there (the picker from `@`, Enter): the bot answers in each, and
//!    `srv-a`'s window, on the other node, shows the answers.
//! 3. The bot posts on its own: asked for `proactive`, it posts to the 1:1 chat it stored.
//!
//! A second run deploys the binary on one node with SQLite and runs the chat and the card of the maintained SDK's
//! bot. `QA_BOTS_ONLY` (`two` or `one`) runs only that deployment.
//!
//! The parent asks each child what its window holds, and does the steps that have no fixed place on the screen as
//! their controls do (`qa_bots_acceptance_child.rs`): the package chosen in the file dialog, the store's buttons, the
//! order command's tab. A card's and a dialog's fields and buttons are found on the Teams Light screen by their
//! colours. Every person, app, id and text is made up; the bots' secrets go only to their containers.
//!
//! Screenshots under `target/ui-shots/qa-bots-acceptance-*.png`, in Teams Light and Dark. Without weston 15, niri,
//! `dbus-daemon`, the server binary (`NGTEAMS_QA_SERVER_BIN`, else beside the test's target directory), the container
//! engine and the sample images, or Postgres and Redis for the two nodes (`NGTEAMS_SERVER_PG_URL`,
//! `NGTEAMS_SERVER_REDIS_URL`), it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use teams_ctl::protocol::{OpenTarget, Request, Response};
use ui_sandbox::SandboxResult;
use ui_sandbox::input::{VirtualInput, keys};

#[path = "qa_bots_acceptance_bots.rs"]
mod bots;
#[path = "qa_bots_acceptance_child.rs"]
mod child;
#[path = "own_server_nodes_app.rs"]
#[expect(
    dead_code,
    reason = "the two-node tests share the instance; this one uses a part of it"
)]
mod instance;
#[path = "own_server_nodes_mixed.rs"]
mod mixed;
#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one uses a part of it"
)]
mod own_server_binary;
#[path = "../own_server_nodes.rs"]
#[expect(
    dead_code,
    reason = "the two-node tests share the deployment; this one uses a part of it"
)]
mod own_server_nodes;
#[path = "qa_bots_acceptance_pixels.rs"]
mod pixels;
#[path = "../window.rs"]
mod support;

use bots::{Container, Place, Sample};
use instance::{Instance, Who, ensure, say};
use own_server_binary::BinaryServer;
use own_server_nodes::{Needs, Nodes, PG_ENV, REDIS_ENV, S3_ENV};

const ONLY_ENV: &str = "QA_BOTS_ONLY";
/// The `[bots]` table of every node: bots on loopback, over plain `http` (the containers'), retried soon.
const BOTS_TABLE: &str =
    "\n[bots]\nallow_http_endpoints = true\nallowed_networks = [\"127.0.0.0/8\"]\nretry_delays = [1, 1]\n";
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(300);
const SETTLE: Duration = Duration::from_millis(900);
/// The open chat's compose box, and its "+", in the sandbox's 1280×800 output.
const COMPOSE: (u32, u32) = (830, 757);
const PLUS: (u32, u32) = (1194, 749);
/// Where the pointer rests for a shot: the empty title bar.
const REST: (u32, u32) = (200, 24);
const COMMENT: &str = "because (synthetic)";

const A: Who = Who {
    name: "srv-a",
    upn: "srv-a@own.test",
    node: 0,
    tone: 440.0,
};
const B: Who = Who {
    name: "srv-b",
    upn: "srv-b@own.test",
    node: 1,
    tone: 660.0,
};
const C_UPN: &str = "srv-c@own.test";
const NAMES: [&str; 3] = ["Ada Made-Up", "Bea Made-Up", "Cy Made-Up"];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(instance::CHILD_ENV).as_deref() {
        Ok("app") => instance::child_with("app", Some(child::hook)),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa bots acceptance: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Says why the test can't run here; an error under `GPUI_SMOKE_REQUIRE=1`.
fn skip(why: &str) -> SandboxResult {
    if std::env::var("GPUI_SMOKE_REQUIRE").as_deref() == Ok("1") {
        return Err(format!("required, but {why}").into());
    }
    say(&format!("skipped: {why}"));
    Ok(())
}

fn parent() -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        return skip("no ngteams-server binary (cargo build -p ngteams-server, or NGTEAMS_QA_SERVER_BIN)");
    };
    let samples = match bots::ready() {
        Ok(samples) => samples,
        Err(why) => return skip(&why),
    };
    let only = std::env::var(ONLY_ENV).unwrap_or_default();
    if only != "one" {
        two_nodes(&binary, &samples)?;
    }
    if only != "two" {
        one_node(&binary, &samples[0])?;
    }
    say("qa bots acceptance: ok");
    Ok(())
}

// --- the deployments ---

/// Two API nodes on Postgres and Redis, both bots, every step.
fn two_nodes(binary: &Path, samples: &[Sample]) -> SandboxResult {
    let (Ok(postgres), Ok(redis)) = (std::env::var(PG_ENV), std::env::var(REDIS_ENV)) else {
        return skip(&format!("no Postgres and Redis for two nodes ({PG_ENV}, {REDIS_ENV})"));
    };
    let needs = Needs {
        postgres,
        redis,
        s3: std::env::var(S3_ENV).ok(),
        // no call is made: the relay is named and never reached
        relay: "127.0.0.1:3478".to_owned(),
        secret: "made-up-relay-secret".to_owned(),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let logs = instance::shots_dir()?.join("qa-bots-acceptance-logs");
    let nodes = Nodes::start_with(binary, &needs, &runtime, &logs, BOTS_TABLE)?;
    say(&format!("two nodes deployed at {}", nodes.origin));
    let outcome = two_nodes_walk(&nodes, samples).map_err(|error| format!("{error}\n--- the nodes:{}", nodes.logs()));
    nodes.stop(&runtime)?;
    outcome?;
    say("two nodes on Postgres and Redis: every step passed");
    Ok(())
}

fn two_nodes_walk(nodes: &Nodes, samples: &[Sample]) -> SandboxResult {
    for (index, upn) in [A.upn, B.upn, C_UPN].into_iter().enumerate() {
        nodes.add_user(upn, NAMES[index], index == 0)?;
    }
    let Some(a) = Instance::start(&A, nodes)? else {
        return Ok(());
    };
    let b = Instance::start(&B, nodes)?.ok_or("no second sandbox")?;
    let mut walk = Walk::new(a, b, "two", &nodes.origin, &nodes.ca_file)?;
    let outcome = walk.everything(samples);
    walk.finish(outcome)
}

/// One node on SQLite, the maintained SDK's bot: its chat and its card.
fn one_node(binary: &Path, sample: &Sample) -> SandboxResult {
    let server = BinaryServer::start_with(binary, BOTS_TABLE)?;
    say(&format!("one node on SQLite at {}", server.origin));
    server.add_admin(A.upn, NAMES[0])?;
    server.add_user(B.upn, NAMES[1])?;
    let Some(a) = Instance::start_at(&A, &server.origin, None, "app")? else {
        return Ok(());
    };
    let b = Instance::start_at(&B, &server.origin, None, "app")?.ok_or("no second sandbox")?;
    let mut walk = Walk::new(a, b, "one", &server.origin, &server.ca_file)?;
    let outcome = walk.chat_and_card(sample);
    let outcome =
        outcome.map_err(|error| format!("{error}\n--- the server's log:\n{}", tail(&server.log(), 30)).into());
    walk.finish(outcome)?;
    say("one node on SQLite: the chat and the card passed");
    Ok(())
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

// --- one app in its sandbox ---

/// An app the parent uses: its instance, its keyboard and pointer, and the shots' names.
struct Seat {
    app: Instance,
    input: VirtualInput,
    label: String,
    asked: u32,
}

impl Seat {
    fn new(mut app: Instance, deploy: &str, origin: &str, ca_file: &Path) -> SandboxResult<Self> {
        app.shots = format!("qa-bots-acceptance-{deploy}");
        app.sign_in(origin, ca_file)?;
        let input = app.sandbox.input()?;
        let label = format!("{deploy}-{}", app.name);
        let mut seat = Self {
            app,
            input,
            label,
            asked: 0,
        };
        seat.app.theme(teams_ui::theme::LIGHT_KEY)?;
        seat.input.move_to(REST.0, REST.1)?;
        Ok(seat)
    }

    /// The child's answer to `verb` with `arg`.
    fn ask(&mut self, verb: &str, arg: &str) -> SandboxResult<Value> {
        self.asked += 1;
        let staged = self.app.sandbox.path("bots-ask.staged");
        std::fs::write(&staged, format!("{} {verb} {arg}", self.asked))?;
        std::fs::rename(&staged, self.app.sandbox.path(child::ASK))?;
        let prefix = format!("bots-answer {} ", self.asked);
        let line = self
            .app
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("{}: no answer to {verb}: {error:?}", self.label))?;
        let answer: Value = serde_json::from_str(&line[prefix.len()..])?;
        if let Some(error) = answer.get("error") {
            return Err(format!("{}: {verb} {arg}: {error}", self.label).into());
        }
        Ok(answer)
    }

    fn state(&mut self) -> SandboxResult<Value> {
        self.ask("state", "")
    }

    /// The window's state once `wanted` takes it, within [`WAIT`].
    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                let shot = self.shot(&format!("failed-{}", slug(what)))?;
                return Err(format!("{}: {what}: never came ({}); last {state}", self.label, shot.display()).into());
            }
            thread::sleep(POLL);
        }
    }

    /// Does `what` as its control does.
    fn act(&mut self, what: &str) -> SandboxResult {
        self.ask("do", what)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn ctl(&mut self, request: Request) -> SandboxResult<Response> {
        self.app.ctl(request)
    }

    /// A screenshot of the screen as `qa-bots-acceptance-<label>-<step>.png`.
    fn shot(&mut self, step: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(REST.0, REST.1)?;
        thread::sleep(SETTLE);
        let file = format!("qa-bots-acceptance-{}-{step}.png", self.label);
        let png = self.app.sandbox.screenshot(&file, false)?;
        let kept = instance::shots_dir()?.join(&file);
        std::fs::copy(&png, &kept)?;
        say(&format!("shot {}", kept.display()));
        Ok(kept)
    }

    /// The shots of `step` in Teams Light, then Teams Dark; Light again after.
    fn shots(&mut self, step: &str) -> SandboxResult<PathBuf> {
        let light = self.shot(step)?;
        self.app.theme(teams_ui::theme::DARK_KEY)?;
        self.shot(&format!("{step}-dark"))?;
        self.app.theme(teams_ui::theme::LIGHT_KEY)?;
        Ok(light)
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(Duration::from_millis(400));
        Ok(())
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.input.key(key)?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// Types `text` in the open chat's box after a click in it, and sends it with Enter.
    fn send(&mut self, text: &str) -> SandboxResult {
        self.click(COMPOSE)?;
        self.type_text(text)?;
        self.key(keys::ENTER)
    }

    /// Opens `chat` as `ngteamsctl open chat` does; once the window shows it.
    fn open_chat(&mut self, chat: &str) -> SandboxResult {
        self.ctl(Request::Open {
            target: OpenTarget::Chat { chat: chat.to_owned() },
        })?;
        self.until("the chat open", |state| state["chat"]["conversation"] == chat)?;
        Ok(())
    }

    /// The window's state once the view `view` (`chat` or `channel`) shows a message of `bot` with `text`.
    fn bot_said(&mut self, view: &str, bot: &str, text: &str) -> SandboxResult<Value> {
        let sender = format!("28:{bot}");
        self.until(&format!("the bot's {text:?}"), |state| {
            items(&state[view])
                .iter()
                .any(|item| item["sender"] == sender.as_str() && text_of(item).contains(text))
        })
    }

    fn mri_id(&self) -> String {
        self.app
            .mri
            .trim_start_matches("8:")
            .trim_start_matches("orgid:")
            .to_owned()
    }
}

fn items(view: &Value) -> Vec<Value> {
    view["items"].as_array().cloned().unwrap_or_default()
}

fn text_of(item: &Value) -> &str {
    item["text"].as_str().unwrap_or_default()
}

fn slug(what: &str) -> String {
    what.chars()
        .map(|char| if char.is_ascii_alphanumeric() { char } else { '-' })
        .collect()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The 1:1 chat of a user and a bot, by their ids, as the server makes it.
fn personal_chat(user: &str, bot: &str) -> String {
    let mut ids = [user, bot];
    ids.sort_unstable();
    format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1])
}

// --- the walk ---

struct Walk<'a> {
    a: Seat,
    b: Seat,
    origin: &'a str,
    ca_file: &'a Path,
    bots: Vec<Container>,
}

impl<'a> Walk<'a> {
    fn new(a: Instance, b: Instance, deploy: &str, origin: &'a str, ca_file: &'a Path) -> SandboxResult<Self> {
        let a = Seat::new(a, deploy, origin, ca_file)?;
        let b = Seat::new(b, deploy, origin, ca_file)?;
        say(&format!(
            "{deploy}: srv-a {} and srv-b {} signed in",
            a.app.mri, b.app.mri
        ));
        Ok(Self {
            a,
            b,
            origin,
            ca_file,
            bots: Vec::new(),
        })
    }

    /// The bots stopped and each app quit; `outcome` with the bots' and the apps' logs when it failed.
    fn finish(self, outcome: SandboxResult) -> SandboxResult {
        let outcome = outcome.map_err(|error| {
            let mut said = error.to_string();
            for bot in &self.bots {
                let _ = write!(said, "\n--- a bot:\n{}", tail(&bot.logs(), 40));
            }
            said
        });
        let Self { a, b, bots, .. } = self;
        drop(bots);
        let quit = a.app.quit().and(b.app.quit());
        outcome?;
        quit
    }

    fn everything(&mut self, samples: &[Sample]) -> SandboxResult {
        for sample in samples {
            self.register(sample)?;
            say(&format!("step 1 passed for {}: uploaded, its bot running", sample.name));
            self.add_and_chat(sample)?;
            self.card(sample)?;
            self.proactive(sample)?;
            self.search(sample)?;
            self.order(sample)?;
            say(&format!(
                "step 2 (1:1 chat, card, extensions) and step 3 passed for {}",
                sample.name
            ));
        }
        self.group_chat(samples)?;
        self.channel(samples)?;
        say("step 2 (mentions in a group chat and a channel) passed");
        Ok(())
    }

    fn chat_and_card(&mut self, sample: &Sample) -> SandboxResult {
        self.register(sample)?;
        self.add_and_chat(sample)?;
        self.card(sample)
    }

    /// `srv-a` uploads the sample's package and sets its bot's endpoint; the bot runs with the secret shown.
    fn register(&mut self, sample: &Sample) -> SandboxResult {
        let a = &mut self.a;
        let first = self.bots.is_empty();
        a.act("manage-apps")?;
        a.until("Manage apps read", |state| {
            state["admin"]["page"] == "apps" && state["admin"]["loaded"] == true
        })?;
        if first {
            a.shots("manage-apps")?;
        }
        a.act("upload")?;
        a.until("the upload step", |state| state["admin"]["dialog"]["kind"] == "upload")?;
        let package = a.app.sandbox.path(&format!("{}.zip", sample.dir));
        std::fs::write(&package, sample.package()?)?;
        a.act(&format!("package {}", package.display()))?;
        a.shots(&format!("upload-{}", sample.name))?;
        a.act("confirm")?;
        a.until("the app made", |state| state["admin"]["dialog"]["kind"] == "made")?;
        let secret_file = a.app.sandbox.path(&format!("secret-{}", sample.name));
        a.ask("secret", &secret_file.to_string_lossy())?;
        let secret = std::fs::read_to_string(&secret_file)?;
        std::fs::remove_file(&secret_file)?;
        a.shot(&format!("uploaded-{}", sample.name))?;
        a.act("admin-close")?;
        let state = a.until("the app listed", |state| {
            state["admin"]["dialog"].is_null() && admin_app(state, sample.bot).is_some()
        })?;
        let name = admin_app(&state, sample.bot)
            .and_then(|app| app["name"].as_str().map(str::to_owned))
            .ok_or("the app has no name")?;
        let port = own_server_binary::free_port()?;
        let endpoint = format!("http://127.0.0.1:{port}/api/messages");
        a.act(&format!("admin-open {name}"))?;
        a.until("the app's details", |state| {
            state["admin"]["dialog"]["kind"] == "details"
        })?;
        a.act(&format!("endpoint {endpoint}"))?;
        a.act("confirm")?;
        a.until("the endpoint saved", |state| {
            state["admin"]["dialog"].is_null()
                && admin_app(state, sample.bot).is_some_and(|app| app["endpoint"] == endpoint.as_str())
        })?;
        a.shots(&format!("registered-{}", sample.name))?;
        let tenant = bots::tenant(self.origin, self.ca_file, sample.bot, &secret)?;
        let place = Place {
            origin: self.origin,
            ca_file: self.ca_file,
            tenant: &tenant,
            secret: &secret,
            port,
        };
        self.bots.push(Container::run(sample, &place)?);
        Ok(())
    }

    /// `srv-b` adds the app in the Store, opens its chat from the details, writes and reads the bot's answer.
    fn add_and_chat(&mut self, sample: &Sample) -> SandboxResult {
        let b = &mut self.b;
        b.act("store")?;
        b.until("the store lists the app", |state| {
            state["store"]["loaded"] == true && strings(&state["store"]["apps"]).iter().any(|id| id == sample.app)
        })?;
        b.shots(&format!("store-{}", sample.name))?;
        b.act(&format!("details {}", sample.app))?;
        b.until("the app's details", |state| state["dialog"]["kind"] == "details")?;
        b.shots(&format!("details-{}", sample.name))?;
        b.act("add")?;
        b.until("the app added", |state| state["dialog"]["added"] == true)?;
        b.shots(&format!("details-added-{}", sample.name))?;
        let chat = personal_chat(&b.mri_id(), sample.bot);
        b.act("close")?;
        // the details offer Open once the chat list has the bot's chat
        let start = Instant::now();
        loop {
            let listed = match b.ctl(Request::Chats {
                unread_only: false,
                limit: None,
            })? {
                Response::Chats { chats } => chats.into_iter().map(|row| row.id).collect(),
                _ => Vec::new(),
            };
            if listed.contains(&chat) {
                break;
            }
            ensure(start.elapsed() < WAIT, || {
                format!("the bot's chat {chat} is never listed: {listed:?}")
            })?;
            b.ctl(Request::Sync)?;
            thread::sleep(POLL);
        }
        b.act(&format!("details {}", sample.app))?;
        b.until("the app's details", |state| state["dialog"]["kind"] == "details")?;
        b.shots(&format!("details-open-{}", sample.name))?;
        b.act("open")?;
        b.until("the bot's chat open", |state| {
            state["chat"]["conversation"] == chat.as_str()
        })?;
        b.shots(&format!("bot-chat-{}", sample.name))?;
        let hello = format!("hello {} (synthetic)", sample.name);
        b.send(&hello)?;
        b.bot_said("chat", sample.bot, &format!("echo from {}: {hello}", sample.name))?;
        b.shots(&format!("chat-{}", sample.name))?;
        say(&format!(
            "{}: srv-b added the app and the bot answered in its chat",
            sample.name
        ));
        Ok(())
    }

    /// The bot's card: a click in its field, the words typed, a click on its first button; the bot's answer.
    fn card(&mut self, sample: &Sample) -> SandboxResult {
        let b = &mut self.b;
        b.send("card")?;
        let sender = format!("28:{}", sample.bot);
        b.until("the bot's card", |state| {
            items(&state["chat"])
                .last()
                .is_some_and(|item| item["sender"] == sender.as_str() && item["kind"] == "cards")
        })?;
        let shot = b.shot(&format!("card-{}", sample.name))?;
        let screen = pixels::load(&shot)?;
        let card = pixels::lowest_card(&screen).ok_or_else(|| format!("no card on the screen ({})", shot.display()))?;
        let boxes = pixels::boxes_in(&screen, card);
        let field = boxes
            .iter()
            .copied()
            .filter(|found| found.width() > 150)
            .max_by_key(pixels::Rect::width)
            .ok_or_else(|| format!("no field in the card {card:?}: {boxes:?} ({})", shot.display()))?;
        let button = boxes
            .iter()
            .copied()
            .filter(|found| found.y0 > field.y1)
            .min_by_key(|found| (found.y0, found.x0))
            .ok_or_else(|| format!("no button under the card's field: {boxes:?} ({})", shot.display()))?;
        b.click(field.centre())?;
        b.type_text(COMMENT)?;
        b.shots(&format!("card-filled-{}", sample.name))?;
        b.click(button.centre())?;
        let answer = format!(
            "submitted from {name}: choice=yes comment={COMMENT} sdk={name}",
            name = sample.name
        );
        b.bot_said("chat", sample.bot, &answer)?;
        b.shots(&format!("card-answered-{}", sample.name))?;
        say(&format!(
            "{}: the card's submit reached the bot with the typed words",
            sample.name
        ));
        Ok(())
    }

    /// Asked for it, the bot posts later to the chat it stored.
    fn proactive(&mut self, sample: &Sample) -> SandboxResult {
        let b = &mut self.b;
        b.send("proactive")?;
        b.bot_said("chat", sample.bot, &format!("proactive from {}", sample.name))?;
        b.shots(&format!("proactive-{}", sample.name))?;
        say(&format!(
            "{}: the bot posted on its own to its stored chat",
            sample.name
        ));
        Ok(())
    }

    /// The compose box's "+", the app chosen by its typed name; once its pane is open, its commands' titles.
    fn open_app_pane(b: &mut Seat, sample: &Sample) -> SandboxResult<Vec<String>> {
        b.click(PLUS)?;
        b.until("the flyout's apps read", |state| {
            state["chat"]["compose"]["flyout"]["loaded"] == true
        })?;
        b.type_text(&sample.short)?;
        b.until("the flyout lists the app", |state| {
            strings(&state["chat"]["compose"]["flyout"]["listed"]) == [sample.short.clone()]
        })?;
        b.key(keys::ENTER)?;
        let state = b.until("the app's pane", |state| {
            state["chat"]["compose"]["flyout"]["pane"].is_array()
        })?;
        Ok(strings(&state["chat"]["compose"]["flyout"]["pane"][1]))
    }

    /// The bot's own sent message with an app's card, newer than `before` rows.
    fn card_sent(b: &mut Seat, before: usize) -> SandboxResult {
        b.until("the card sent", |state| {
            let compose = &state["chat"]["compose"];
            compose["cards"].as_array().is_some_and(Vec::is_empty)
                && items(&state["chat"])
                    .iter()
                    .skip(before)
                    .any(|item| item["own"] == true && item["app_cards"].as_u64().unwrap_or(0) >= 1)
        })?;
        Ok(())
    }

    /// The search command: words typed in its pane, the bot's results, the first into the box with Enter, sent.
    fn search(&mut self, sample: &Sample) -> SandboxResult {
        let b = &mut self.b;
        Self::open_app_pane(b, sample)?;
        b.type_text("pears")?;
        let wanted = [
            format!("pears one from {}", sample.name),
            format!("pears two from {}", sample.name),
        ];
        b.until("the search's results", |state| {
            strings(&state["chat"]["compose"]["flyout"]["pane"][2]) == wanted
        })?;
        b.shots(&format!("search-{}", sample.name))?;
        b.key(keys::ENTER)?;
        b.until("the result's card in the box", |state| {
            state["chat"]["compose"]["cards"]
                .as_array()
                .is_some_and(|cards| cards.len() == 1)
        })?;
        b.shots(&format!("search-card-{}", sample.name))?;
        let before = items(&b.state()?["chat"]).len();
        b.key(keys::ENTER)?;
        Self::card_sent(b, before)?;
        b.shots(&format!("search-sent-{}", sample.name))?;
        say(&format!("{}: the search command's result was sent", sample.name));
        Ok(())
    }

    /// The order command: its dialog's field clicked and typed, its button clicked; the bot's card into the box, sent.
    fn order(&mut self, sample: &Sample) -> SandboxResult {
        let b = &mut self.b;
        let tabs = Self::open_app_pane(b, sample)?;
        let at = tabs
            .iter()
            .position(|tab| tab == "Order")
            .ok_or_else(|| format!("no Order tab in {tabs:?}"))?;
        b.act(&format!("command {at}"))?;
        b.until("the order dialog's card", |state| {
            state["dialog"]["kind"] == "app" && state["dialog"]["stage"] == "card"
        })?;
        let shot = b.shot(&format!("order-{}", sample.name))?;
        let screen = pixels::load(&shot)?;
        let dialog =
            pixels::surface_box(&screen).ok_or_else(|| format!("no dialog on the screen ({})", shot.display()))?;
        let boxes = pixels::boxes_in(&screen, dialog);
        let [field, .., button] = boxes.as_slice() else {
            return Err(format!("the order dialog shows {boxes:?} ({})", shot.display()).into());
        };
        let (field, button) = (*field, *button);
        b.click(field.centre())?;
        b.type_text("pears")?;
        b.shots(&format!("order-filled-{}", sample.name))?;
        b.click(button.centre())?;
        let ordered = format!("ordered pears from {}", sample.name);
        b.until("the ordered card in the box", |state| {
            state["dialog"].is_null()
                && state["chat"]["compose"]["cards"]
                    .as_array()
                    .is_some_and(|cards| cards.iter().any(|card| card["content"].to_string().contains(&ordered)))
        })?;
        b.shots(&format!("order-card-{}", sample.name))?;
        let before = items(&b.state()?["chat"]).len();
        b.click(COMPOSE)?;
        b.key(keys::ENTER)?;
        Self::card_sent(b, before)?;
        b.shots(&format!("order-sent-{}", sample.name))?;
        say(&format!(
            "{}: the order command's dialog was submitted and its card sent",
            sample.name
        ));
        Ok(())
    }

    /// The app added to `place` (a chat's or a team's id) from its details in the Store.
    fn add_to(b: &mut Seat, sample: &Sample, kind: &str, place: &str) -> SandboxResult {
        b.act("store")?;
        b.until("the store read", |state| state["store"]["loaded"] == true)?;
        b.act(&format!("details {}", sample.app))?;
        b.until("the app's details", |state| state["dialog"]["kind"] == "details")?;
        b.act(&format!("add-to {kind}"))?;
        b.until(&format!("the {kind} offered"), |state| {
            state["dialog"]["kind"] == "place" && strings(&state["dialog"]["places"]).iter().any(|id| id == place)
        })?;
        b.act(&format!("choose {place}"))?;
        b.shots(&format!("add-to-{kind}-{}", sample.name))?;
        b.act("go")?;
        b.until(&format!("the app added to the {kind}"), |state| {
            state["dialog"].is_null()
        })?;
        Ok(())
    }

    /// In the open `view` (`chat` or `channel`): `@` and the app's name typed, its bot picked with Enter, `words`
    /// after it, sent.
    fn mention(b: &mut Seat, view: &str, sample: &Sample, words: &str) -> SandboxResult {
        b.click(COMPOSE)?;
        b.type_text(&format!("@{}", sample.short))?;
        b.until("the picker offers the bot", |state| {
            strings(&state[view]["compose"]["picker"]["names"]).contains(&sample.short)
        })?;
        b.shot(&format!("{view}-picker-{}", sample.name))?;
        b.key(keys::ENTER)?;
        b.type_text(&format!(" {words}"))?;
        b.key(keys::ENTER)
    }

    /// A group chat of `srv-b`, `srv-a` and `srv-c` with each app added; `srv-b` mentions each bot, each answers;
    /// `srv-a`'s window shows the answers.
    fn group_chat(&mut self, samples: &[Sample]) -> SandboxResult {
        let people = vec![self.a.app.upn.to_owned(), C_UPN.to_owned()];
        let group = match self.b.ctl(Request::NewChat { people, wait: true })? {
            Response::Outbox(row) => row
                .chat
                .clone()
                .or(row.message.clone())
                .ok_or_else(|| format!("no group: {row:?}"))?,
            other => return Err(format!("new group: {other:?}").into()),
        };
        for sample in samples {
            Self::add_to(&mut self.b, sample, "chat", &group)?;
        }
        self.b.open_chat(&group)?;
        for sample in samples {
            Self::mention(&mut self.b, "chat", sample, "hello group")?;
            let answer = format!("mentioned in groupChat from {}: hello group", sample.name);
            self.b.bot_said("chat", sample.bot, &answer)?;
            self.b.shots(&format!("group-{}", sample.name))?;
        }
        self.a.open_chat(&group)?;
        for sample in samples {
            let answer = format!("mentioned in groupChat from {}: hello group", sample.name);
            self.a.bot_said("chat", sample.bot, &answer)?;
        }
        self.a.shots("group")?;
        say("the bots answered their mentions in the group chat, shown on both nodes' windows");
        Ok(())
    }

    /// A team of `srv-b` with `srv-a` and each app added; `srv-b` mentions each bot in its General channel, each
    /// answers in the post's thread; `srv-a`'s window shows the channel.
    fn channel(&mut self, samples: &[Sample]) -> SandboxResult {
        let made = self
            .b
            .ask("make-team", &format!("Made-up bots crew|{}", self.a.app.mri))?;
        let team = made["team"].as_str().ok_or("no team")?.to_owned();
        for sample in samples {
            Self::add_to(&mut self.b, sample, "team", &team)?;
        }
        for seat in [&mut self.b, &mut self.a] {
            seat.ctl(Request::Open {
                target: OpenTarget::Channel { channel: team.clone() },
            })?;
            seat.until("the channel open", |state| {
                state["channel"]["conversation"] == team.as_str()
            })?;
        }
        for sample in samples {
            Self::mention(&mut self.b, "channel", sample, "hello channel")?;
            let answer = format!("mentioned in channel from {}: hello channel", sample.name);
            channel_answer(&mut self.b, &team, sample.bot, &answer)?;
            self.b.shots(&format!("channel-{}", sample.name))?;
        }
        for sample in samples {
            let answer = format!("mentioned in channel from {}: hello channel", sample.name);
            channel_answer(&mut self.a, &team, sample.bot, &answer)?;
        }
        self.a.shots("channel")?;
        say("the bots answered their mentions in the channel, read on both nodes");
        Ok(())
    }
}

fn admin_app<'s>(state: &'s Value, bot: &str) -> Option<&'s Value> {
    state["admin"]["apps"].as_array()?.iter().find(|app| app["bot"] == bot)
}

/// Waits until the seat's control socket lists the bot's `text` in the channel `team` (a reply in a post's thread).
fn channel_answer(seat: &mut Seat, team: &str, bot: &str, text: &str) -> SandboxResult {
    let sender = format!("28:{bot}");
    let start = Instant::now();
    loop {
        if let Response::Messages { messages, .. } = seat.ctl(Request::Messages {
            chat: team.to_owned(),
            limit: Some(50),
            before: None,
        })? && messages
            .iter()
            .any(|message| message.sender.as_deref() == Some(sender.as_str()) && message.text.contains(text))
        {
            return Ok(());
        }
        if start.elapsed() > WAIT {
            let shot = seat.shot("failed-channel-answer")?;
            return Err(format!("{}: no {text:?} in the channel ({})", seat.label, shot.display()).into());
        }
        thread::sleep(POLL);
    }
}
