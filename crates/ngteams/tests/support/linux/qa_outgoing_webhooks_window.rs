//! A team owner on an own server makes an outgoing webhook in the app's window, and a post that mentions it reaches
//! the webhook's service (`harness = false`: GPUI runs on the process's main thread). It is the QA test of task 412.
//!
//! The parent runs the real `ngteams-server` binary (TLS, SQLite) with `[bots]` set to reach loopback, and the
//! webhook's service: an HTTPS endpoint on 127.0.0.1 whose certificate a made-up certificate authority signed, which
//! checks each request's HMAC under the webhook's token. The server process trusts only that authority. The app runs
//! as a child in the sandbox, with `NGTEAMS_BOTS=1`. Before its window opens, the user makes a team with a member and
//! a channel through the app's own engine.
//!
//! The walk: Manage team > Apps > "Create an outgoing webhook"; the user types the name, the HTTPS address and the
//! description with the keyboard (Tab between the fields), picks a wrong picture, then a right one, and creates it.
//! The token shows once; Copy puts it on the clipboard, and the service trusts that copy. The list shows the webhook;
//! a name the team has and an `http://` address get their words. Edit changes the description. In the channel, the
//! user types `@Ech` in the box, picks the webhook from the picker and posts; the service gets the signed post and
//! answers in text, and a second post gets a card. The thread shows both replies under the webhook's name with the
//! App mark. Remove asks, then takes the webhook off the list.
//!
//! Buttons are pressed through the handlers they call (a real window has no element bounds by selector); the text is
//! typed and the box is clicked with the sandbox's keyboard and pointer. Every name, id and text is made up. The
//! screenshots are kept under `target/ui-shots/qa-webhooks-*.png`, in Teams Light and Dark. Without weston 15, niri,
//! `dbus-daemon` or the built server binary it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::ffi::OsStr;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::net::SocketAddr;
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
use ngteams::descriptors::adopt_trace;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::CallFloor;
use teams_proto::{Mri, NewTeam, TeamRole, TeamWrite};
use teams_ui::chat_view::{Body, ChatView, MessageItem, NameMark};
use teams_ui::manage_team::ManageTab;
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::webhook_dialog::{NAME_TAKEN, NOT_HTTPS, WebhookDialog};
use teams_ui::window::KitRoot;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../own_server_binary.rs"]
#[expect(dead_code, reason = "the shared binary harness has helpers other QA tests use")]
mod own_server_binary;

use own_server_binary::BinaryServer;
#[path = "../window.rs"]
#[expect(dead_code, reason = "the shared window harness has helpers other tests use")]
mod support;

const CHILD_ENV: &str = "NGTEAMS_QA_WEBHOOKS_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_QA_WEBHOOKS_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_WEBHOOKS_ORIGIN";
const CA_ENV: &str = "NGTEAMS_QA_WEBHOOKS_CA";
const DIR_ENV: &str = "NGTEAMS_QA_WEBHOOKS_DIR";
const MEMBER_ENV: &str = "NGTEAMS_QA_WEBHOOKS_MEMBER";
/// The file in the app's directory the parent asks the child through.
const ASK: &str = "qa-webhooks-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
/// The file the child writes the team's and the channel's ids to before its window opens.
const PLACES: &str = "places.json";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
/// How long a post may take to reach the service (a lost wake-up waits for the outbox's poll).
const DELIVERY: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(150);
const SETTLE: Duration = Duration::from_millis(900);
/// A pause after typed text, for the input to take it.
const TYPED: Duration = Duration::from_millis(400);

const USER_UPN: &str = "dana@own.example.test";
const USER_NAME: &str = "Dana Made-Up";
const MEMBER_UPN: &str = "bert@own.example.test";
const MEMBER_NAME: &str = "Bert Made-Up";
const TEAM_NAME: &str = "Rocket (synthetic)";
const CHANNEL_NAME: &str = "Launch";
const HOOK: &str = "Echo bot";
const DESCRIPTION: &str = "Answers a made-up status";
const MORE: &str = " and builds";
const FIRST_POST: &str = "status please";
const SECOND_POST: &str = "and the build?";
const PONG: &str = "Pong from the made-up echo";
const CARD_TEXT: &str = "Build 7 passed (synthetic)";
const CARD_ACTION: &str = "Open the build";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_outgoing_webhooks_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

/// What the walk saw wrong that does not stop it: each is said at the end, and the test fails.
static FINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

/// Notes `finding` unless `ok`.
fn expect(ok: bool, finding: impl FnOnce() -> String) {
    if !ok {
        note(finding());
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams outgoing webhooks", &options)? else {
        return Ok(());
    };
    let Some(binary) = own_server_binary::binary()? else {
        if ui_sandbox::required() {
            return Err("no ngteams-server binary: cargo build -p ngteams-server first".into());
        }
        say("skipped: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let dir = tempfile::tempdir()?;
    let service = Service::start(&runtime, dir.path())?;
    let outcome = world(&sandbox, &binary, &service, dir.path());
    service.stop();
    drop(runtime);
    if let Err(error) = outcome {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams outgoing webhooks: ok");
    Ok(())
}

/// The server and its users, the app; then the walk.
fn world(sandbox: &Sandbox, binary: &Path, service: &Service, dir: &Path) -> SandboxResult {
    let server_dir = dir.join("server");
    let server = start_server(binary, &server_dir, &service.ca_file)?;
    let user = server.add_user(USER_UPN, USER_NAME)?;
    let member = server.add_user(MEMBER_UPN, MEMBER_NAME)?;
    step(&format!(
        "own server {}; owner {user}; member {member}; the service at {}",
        server.origin, service.url
    ));
    let app_dir = dir.join("app");
    fs::create_dir_all(&app_dir)?;
    let env = vec![
        (ORIGIN_ENV, server.origin.clone()),
        (CA_ENV, server.ca_file.to_string_lossy().into_owned()),
        (MEMBER_ENV, format!("8:orgid:{member}")),
    ];
    let mut run = AppRun::launch(sandbox, &env, &app_dir)?;
    let outcome = (|| -> SandboxResult {
        let mut walk = Walk {
            run: &mut run,
            sandbox,
            service,
            dir: dir.to_owned(),
            server_db: server_dir.join("server.sqlite"),
            places: app_dir.join(PLACES),
            posts: Vec::new(),
        };
        walk.open_apps()?;
        walk.create()?;
        walk.token()?;
        walk.listed()?;
        walk.refusals()?;
        walk.edit()?;
        walk.mention()?;
        walk.replies()?;
        walk.remove()
    })();
    if let Err(error) = outcome {
        let _ = fs::write(dir.join("own-server.log"), server.log());
        step(&format!("the server's log:\n{}", tail(&server.log(), 60)));
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// The last `lines` lines of `text`.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    service: &'a Service,
    dir: PathBuf,
    /// The own server's database, read beside it to check what it keeps.
    server_db: PathBuf,
    /// The child's [`PLACES`] file.
    places: PathBuf,
    /// The ids of the user's two posts that mention the webhook.
    posts: Vec<String>,
}

impl Walk<'_> {
    /// The shots of `name` in Teams Light, then Teams Dark, then back to Light.
    fn shots(&mut self, name: &str) -> SandboxResult {
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa-webhooks-{name}-light.png"))?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa-webhooks-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The dialog's state once `wanted` takes it.
    fn until_dialog(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.run.until_in("dialog", what, wanted)
    }

    /// The dialog once its fields hold `values`: a key the input has not taken yet would change them after a send.
    fn until_typed(&mut self, values: [&str; 3]) -> SandboxResult {
        self.until_dialog("the typed fields", |dialog| dialog["values"] == json!(values))
            .map(drop)
    }

    /// The Apps tab's webhooks once `wanted` takes them.
    fn until_page(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.run.until_in("page", what, wanted)
    }

    /// The names of the team's webhooks in the server's database now; an error if the read fails or the server has
    /// no such team.
    // the engine's list read gives an empty list for a failed read, so the check reads the server's own store
    fn server_hooks(&self) -> SandboxResult<Vec<String>> {
        let places: Value = serde_json::from_str(&fs::read_to_string(&self.places)?)?;
        let team = places["team"].as_str().ok_or("no team in the places file")?;
        let db = rusqlite::Connection::open_with_flags(&self.server_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(WAIT)?;
        let teams: i64 = db.query_row("SELECT count(*) FROM teams WHERE id = ?1", [team], |row| row.get(0))?;
        if teams != 1 {
            return Err(format!("the server has no team {team}").into());
        }
        let mut names = db.prepare("SELECT name FROM outgoing_webhooks WHERE team_id = ?1 ORDER BY name")?;
        let names = names
            .query_map([team], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(names)
    }

    /// Types `text` with the keyboard.
    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.run.input.type_text(text)?;
        thread::sleep(TYPED);
        Ok(())
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.run.input.key(key)?;
        thread::sleep(Duration::from_millis(200));
        Ok(())
    }

    /// A made-up picture in a file of the walk's directory: its path.
    fn picture_file(&self, name: &str, bytes: &[u8]) -> SandboxResult<String> {
        let path = self.dir.join(name);
        fs::write(&path, bytes)?;
        Ok(path.to_string_lossy().into_owned())
    }
}

// --- Manage team > Apps: create ---

impl Walk<'_> {
    fn open_apps(&mut self) -> SandboxResult {
        self.run
            .until_in("teams", "the team in the list", |state| state["count"] == 1)?;
        self.run.act("manage")?;
        thread::sleep(SETTLE);
        self.until_page("the Apps tab", |page| page["open"] == true)?;
        let hooks = self.server_hooks()?;
        expect(hooks.is_empty(), || {
            format!("the server lists webhooks for a new team: {hooks:?}")
        });
        self.run.shot(self.sandbox, "qa-webhooks-apps-light.png")?;
        step("Manage team > Apps is open");
        Ok(())
    }

    /// "Create an outgoing webhook": the fields typed, a wrong picture refused, a right one taken, then Create.
    fn create(&mut self) -> SandboxResult {
        self.run.act("create")?;
        self.until_dialog("the create dialog", |dialog| dialog["open"] == true)?;
        thread::sleep(SETTLE);
        self.type_text(HOOK)?;
        self.key(keys::TAB)?;
        self.type_text(&self.service.url.clone())?;
        self.key(keys::TAB)?;
        self.type_text(DESCRIPTION)?;
        let dialog = self.run.state_in("dialog")?;
        expect(dialog["can_send"] == true, || {
            format!("after the name, Tab, the address, Tab, the description, Create cannot go: {dialog}")
        });
        let gif = self.picture_file("face.gif", b"GIF89a made-up")?;
        self.run.act(&format!("choose {gif}"))?;
        let dialog = self.run.state_in("dialog")?;
        expect(
            dialog["problem"] == "That file can't be used. Choose a .png, .jpg or .jpeg picture.",
            || format!("a GIF picture is not refused with words: {dialog}"),
        );
        let png = self.picture_file("face.png", &made_up_png()?)?;
        self.run.act(&format!("choose {png}"))?;
        let dialog = self.run.state_in("dialog")?;
        expect(dialog["problem"].is_null(), || {
            format!("a PNG picture is not taken: {dialog}")
        });
        self.shots("create")?;
        self.until_typed([HOOK, &self.service.url.clone(), DESCRIPTION])?;
        self.run.act("send")?;
        self.until_dialog("the token after Create", |dialog| dialog["token_shown"] == true)?;
        step("Create answered with the token");
        Ok(())
    }

    /// The token once, Copy, and the service trusts the copied token.
    fn token(&mut self) -> SandboxResult {
        self.shots("token")?;
        self.run.act("copy")?;
        let same = self.run.state_in("clipboard-is-token")?;
        expect(same == true, || {
            "Copy did not put the token shown on the clipboard".to_owned()
        });
        let copied = self.run.state_in("clipboard")?;
        let key = STANDARD.decode(copied.as_str().unwrap_or_default());
        expect(key.as_ref().is_ok_and(|key| key.len() == 32), || {
            "the copied token is no 32-byte key in base64".to_owned()
        });
        self.service.trust(key.unwrap_or_default());
        self.run.shot(self.sandbox, "qa-webhooks-copied-light.png")?;
        self.run.act("close")?;
        self.until_dialog("the dialog closed", |dialog| dialog["open"] == false)?;
        step("the token was copied and the dialog closed");
        Ok(())
    }

    /// The Apps tab lists the webhook with its description, its address and its picture.
    fn listed(&mut self) -> SandboxResult {
        let page = self.until_page("the new webhook in the list", |page| {
            page["hooks"].as_array().is_some_and(|hooks| hooks.len() == 1)
        })?;
        let hook = &page["hooks"][0];
        for (field, wanted) in [
            ("name", json!(HOOK)),
            ("description", json!(DESCRIPTION)),
            ("can_manage", json!(true)),
        ] {
            expect(hook[field] == wanted, || {
                format!("the listed webhook's {field} is {}", hook[field])
            });
        }
        expect(hook["callback_url"] == self.service.url.as_str(), || {
            format!("the listed webhook's address is {}", hook["callback_url"])
        });
        expect(hook["picture"] == true, || {
            "the listed webhook has no picture".to_owned()
        });
        self.shots("list")?;
        step("the Apps tab lists the webhook");
        Ok(())
    }

    /// A name the team has, in other letters, and an `http://` address: their words, and nothing made.
    fn refusals(&mut self) -> SandboxResult {
        self.run.act("create")?;
        self.until_dialog("the create dialog again", |dialog| {
            dialog["open"] == true && dialog["token_shown"] == false
        })?;
        thread::sleep(SETTLE);
        self.type_text("ECHO BOT")?;
        self.key(keys::TAB)?;
        self.type_text("http://127.0.0.1:9/hook")?;
        self.until_typed(["ECHO BOT", "http://127.0.0.1:9/hook", ""])?;
        self.run.act("send")?;
        let dialog = self.run.state_in("dialog")?;
        expect(dialog["problem"] == NOT_HTTPS, || {
            format!("an http:// address is not refused with words: {dialog}")
        });
        // the address again, now HTTPS
        for _ in 0.."http://127.0.0.1:9/hook".len() {
            self.run.input.key(keys::BACKSPACE)?;
        }
        self.type_text(&self.service.url.clone())?;
        self.until_typed(["ECHO BOT", &self.service.url.clone(), ""])?;
        self.run.act("send")?;
        let dialog = self.until_dialog("the name's conflict", |dialog| !dialog["problem"].is_null())?;
        expect(dialog["problem"] == NAME_TAKEN, || {
            format!("a name the team has is not told in Teams' words: {dialog}")
        });
        self.shots("conflict")?;
        self.run.act("close")?;
        self.until_dialog("the dialog closed", |dialog| dialog["open"] == false)?;
        let hooks = self.server_hooks()?;
        expect(hooks == [HOOK], || {
            format!("a refused create changed the server's list: {hooks:?}")
        });
        Ok(())
    }

    /// Edit: the description gets more words; only that changes.
    fn edit(&mut self) -> SandboxResult {
        self.run.act(&format!("edit {HOOK}"))?;
        self.until_dialog("the edit dialog", |dialog| dialog["open"] == true)?;
        thread::sleep(SETTLE);
        // the name has the focus: Tab twice to the description, then its end
        self.key(keys::TAB)?;
        self.key(keys::TAB)?;
        self.key(keys::END)?;
        self.type_text(MORE)?;
        self.until_typed([HOOK, &self.service.url.clone(), &format!("{DESCRIPTION}{MORE}")])?;
        self.run.shot(self.sandbox, "qa-webhooks-edit-light.png")?;
        self.run.act("send")?;
        self.until_dialog("the edit dialog closed", |dialog| dialog["open"] == false)?;
        let wanted = format!("{DESCRIPTION}{MORE}");
        let page = self.until_page("the changed description", |page| {
            page["hooks"][0]["description"] == wanted.as_str()
        });
        match page {
            Ok(page) => {
                let hook = &page["hooks"][0];
                expect(hook["name"] == HOOK && hook["picture"] == true, || {
                    format!("Edit changed more than the description: {hook}")
                });
            }
            Err(error) => note(format!("Edit did not change the description: {error}")),
        }
        step("Edit changed the description");
        Ok(())
    }
}

// --- the channel: the mention, the replies ---

impl Walk<'_> {
    /// Posts in the channel: `@Ech`, the picker's webhook, the words, Enter. The service gets the signed post.
    fn post_mention(&mut self, words: &str, shot: bool) -> SandboxResult<Received> {
        let from = self.service.received().len();
        let state = self
            .run
            .until_in("posts", "the channel's box", |state| state["box"].is_array())?;
        let at = &state["box"];
        let x = px(&at[0]) + px(&at[2]) / 2;
        let y = px(&at[1]) + px(&at[3]).min(40) / 2;
        self.run.click(x, y)?;
        self.type_text("@Ech")?;
        let state = self.run.until_in("posts", "the picker with the webhook", |state| {
            state["offered"]
                .as_array()
                .is_some_and(|offered| offered.iter().any(|name| name == HOOK))
        })?;
        expect(state["offered"] == json!([HOOK]), || {
            format!("the picker offers {}, expected only the webhook", state["offered"])
        });
        if shot {
            self.shots("picker")?;
        }
        // the picker puts the space after the mention
        self.key(keys::ENTER)?;
        self.type_text(words)?;
        self.key(keys::ENTER)?;
        let wanted = format!("<at>{HOOK}</at> {words}");
        let got = wait_until(DELIVERY, || {
            self.service
                .received()
                .into_iter()
                .skip(from)
                .find(|got| got.activity["text"] == wanted.as_str())
        })
        .ok_or_else(|| {
            format!(
                "no post with {wanted:?} reached the service: {:?}",
                self.service.texts()
            )
        })?;
        expect(got.signed, || {
            format!(
                "the post reached the service with a wrong HMAC: {:?}",
                got.authorization
            )
        });
        expect(got.activity["from"]["name"] == USER_NAME, || {
            format!("the post's sender is {}", got.activity["from"]["name"])
        });
        expect(got.activity["conversation"]["conversationType"] == "channel", || {
            format!("the post's conversation is {}", got.activity["conversation"])
        });
        Ok(got)
    }

    fn mention(&mut self) -> SandboxResult {
        self.run.act("channel")?;
        self.service
            .answer(json!({"type": "message", "text": format!("**{PONG}**")}));
        let first = self.post_mention(FIRST_POST, true)?;
        self.posts
            .push(first.activity["id"].as_str().unwrap_or_default().to_owned());
        self.service.answer(json!({"type": "message", "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "version": "1.4",
                "body": [{"type": "TextBlock", "text": CARD_TEXT, "wrap": true}],
                "actions": [{"type": "Action.OpenUrl", "title": CARD_ACTION, "url": "https://ci.example.test/7"}]},
        }]}));
        let second = self.post_mention(SECOND_POST, false)?;
        self.posts
            .push(second.activity["id"].as_str().unwrap_or_default().to_owned());
        step(&format!("both posts reached the service, signed: {:?}", self.posts));
        Ok(())
    }

    /// Each post's thread shows the webhook's reply under its name with the App mark: text, then a card.
    fn replies(&mut self) -> SandboxResult {
        for (index, post) in self.posts.clone().iter().enumerate() {
            self.run.act(&format!("thread {post}"))?;
            let card = index == 1;
            let found = self
                .run
                .until_in("thread", "the webhook's reply in the thread", |state| {
                    items(state).any(|item| hook_reply(item, card))
                });
            let state = match found {
                Ok(_) => self.until_marked()?,
                Err(error) => {
                    note(format!(
                        "the thread of post {post} does not show the webhook's reply: {error}"
                    ));
                    self.run.state_in("thread")?
                }
            };
            for item in items(&state).filter(|item| item["sender_name"] == HOOK) {
                expect(item["app_mark"] == true, || {
                    format!("the webhook's reply has no App mark: {item}")
                });
                expect(item["own"] == false, || {
                    format!("the webhook's reply shows as the user's: {item}")
                });
            }
            if card {
                let shown = items(&state).find(|item| item["kind"] == "cards");
                expect(
                    shown.is_some_and(|item| {
                        item["card"]
                            .as_str()
                            .is_some_and(|card| card.contains(CARD_TEXT) && card.contains(CARD_ACTION))
                    }),
                    || format!("the card reply does not show its text and action: {state}"),
                );
            }
            self.shots(if card { "card-reply" } else { "reply" })?;
        }
        step("both threads show the webhook's replies");
        Ok(())
    }

    /// The thread once each of the webhook's replies has the App mark, else as it is when the wait ends.
    // the row shows the message's own sender name before the view's read of the webhook's app gives the mark
    fn until_marked(&mut self) -> SandboxResult<Value> {
        let marked = self
            .run
            .until_in("thread", "the App mark on the webhook's reply", |state| {
                items(state)
                    .filter(|item| item["sender_name"] == HOOK)
                    .all(|item| item["app_mark"] == true)
            });
        match marked {
            Ok(state) => Ok(state),
            Err(_) => self.run.state_in("thread"),
        }
    }

    /// Remove asks first; then the webhook is gone from the list, and the server's list.
    fn remove(&mut self) -> SandboxResult {
        self.run.act("manage")?;
        self.until_page("the list again", |page| page["hooks"][0]["name"] == HOOK)?;
        self.run.act(&format!("ask-remove {HOOK}"))?;
        let page = self.run.state_in("page")?;
        expect(page["hooks"][0]["name"] == HOOK, || {
            format!("the question took the webhook away: {page}")
        });
        self.shots("remove-question")?;
        self.run.act("remove")?;
        self.until_page("the webhook gone", |page| {
            page["hooks"].as_array().is_some_and(Vec::is_empty)
        })?;
        // the Remove write may still be on its way, so the server's list is read until it is empty
        let start = Instant::now();
        loop {
            let hooks = self.server_hooks()?;
            if hooks.is_empty() {
                break;
            }
            if start.elapsed() > WAIT {
                return Err(format!("the server still lists the removed webhook: {hooks:?}").into());
            }
            thread::sleep(POLL);
        }
        self.run.shot(self.sandbox, "qa-webhooks-removed-light.png")?;
        step("Remove asked, then took the webhook off");
        Ok(())
    }
}

/// Whether `item` is the webhook's reply: its card, or its text.
fn hook_reply(item: &Value, card: bool) -> bool {
    let shown = if card {
        item["kind"] == "cards"
    } else {
        item["text"] == PONG
    };
    item["sender_name"] == HOOK && shown
}

/// A pixel of a bounds array, as the sandbox's pointer takes it.
fn px(value: &Value) -> u32 {
    let pixels = value.as_f64().unwrap_or(0.0).max(0.0).round();
    // a window coordinate is far below u32::MAX
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a small, positive window coordinate"
    )]
    let pixels = pixels as u32;
    pixels
}

fn items(state: &Value) -> impl Iterator<Item = &Value> {
    state["items"].as_array().into_iter().flatten()
}

/// A made-up square picture: white on blue, as a PNG.
fn made_up_png() -> SandboxResult<Vec<u8>> {
    let face = image::RgbaImage::from_fn(64, 64, |x, y| {
        if (16..48).contains(&x) && (16..48).contains(&y) {
            image::Rgba([255, 255, 255, 255])
        } else {
            image::Rgba([25, 90, 190, 255])
        }
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(face).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

// --- the webhook's service ---

/// One post as the service got it.
#[derive(Clone, Debug)]
struct Received {
    activity: Value,
    authorization: Option<String>,
    /// The HMAC of the exact body under the trusted key matches `Authorization`.
    signed: bool,
}

struct Shared {
    key: Mutex<Vec<u8>>,
    answer: Mutex<Value>,
    inbox: Mutex<Vec<Received>>,
}

/// The webhook's HTTPS service on 127.0.0.1, with the made-up authority that signed its certificate.
struct Service {
    url: String,
    ca_file: PathBuf,
    shared: Arc<Shared>,
    handle: axum_server::Handle<SocketAddr>,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime, dir: &Path) -> SandboxResult<Self> {
        let (ca_pem, cert, key) = certificates()?;
        let ca_file = dir.join("service-ca.pem");
        fs::write(&ca_file, ca_pem)?;
        let mut config =
            rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let url = format!("https://{}/hook", listener.local_addr()?);
        let shared = Arc::new(Shared {
            key: Mutex::new(Vec::new()),
            answer: Mutex::new(json!({"type": "message", "text": "ok"})),
            inbox: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/hook", post(receive))
            .with_state(Arc::clone(&shared));
        let handle = axum_server::Handle::new();
        let _entered = runtime.enter();
        let server = axum_server::from_tcp_rustls(
            listener,
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(config)),
        )?
        .handle(handle.clone());
        runtime.spawn(server.serve(router.into_make_service()));
        Ok(Self {
            url,
            ca_file,
            shared,
            handle,
        })
    }

    fn trust(&self, key: Vec<u8>) {
        *self.shared.key.lock().unwrap_or_else(PoisonError::into_inner) = key;
    }

    fn answer(&self, answer: Value) {
        *self.shared.answer.lock().unwrap_or_else(PoisonError::into_inner) = answer;
    }

    fn received(&self) -> Vec<Received> {
        self.shared.inbox.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn texts(&self) -> Vec<String> {
        self.received()
            .iter()
            .map(|got| got.activity["text"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn stop(&self) {
        self.handle.shutdown();
    }
}

/// `Authorization`'s value for `body` under `key`, as Teams makes it.
fn hmac(key: &[u8], body: &[u8]) -> String {
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key);
    format!("HMAC {}", STANDARD.encode(aws_lc_rs::hmac::sign(&key, body).as_ref()))
}

async fn receive(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let key = shared.key.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let signed = !key.is_empty() && authorization.as_deref() == Some(hmac(&key, &body).as_str());
    let activity: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    step(&format!("the service got {} (signed: {signed})", activity["text"]));
    shared
        .inbox
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Received {
            activity,
            authorization,
            signed,
        });
    if !signed {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let answer = shared.answer.lock().unwrap_or_else(PoisonError::into_inner).clone();
    ([("content-type", "application/json")], answer.to_string()).into_response()
}

/// A made-up authority's PEM, and the service's certificate for 127.0.0.1 it signed with its key.
fn certificates() -> SandboxResult<(String, CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let ca_key = KeyPair::generate()?;
    let mut ca = CertificateParams::new(Vec::new())?;
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(DnType::CommonName, "Made-up webhook certificate authority");
    let ca_cert = ca.self_signed(&ca_key)?;
    let key = KeyPair::generate()?;
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
    let cert = leaf.signed_by(&key, &Issuer::new(ca, ca_key))?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    Ok((ca_cert.pem(), cert.der().clone(), key))
}

// --- the own server ---

/// The real `ngteams-server` binary on loopback, its outbound guard open to loopback and its process trusting only
/// the service's authority; its log and database stay in `dir`.
fn start_server(binary: &Path, dir: &Path, service_ca: &Path) -> SandboxResult<BinaryServer> {
    // the server reaches the service through the system verifier, which reads only these when they are set
    let no_roots = dir.join("no-roots");
    fs::create_dir_all(&no_roots)?;
    BinaryServer::start_in(
        binary,
        Some(dir),
        "\n[bots]\nallowed_networks = [\"127.0.0.0/8\"]\n",
        &[
            ("SSL_CERT_FILE", service_ca.as_os_str()),
            ("SSL_CERT_DIR", no_roots.as_os_str()),
            ("NGTEAMS_SERVER_LOG", OsStr::new("info,tower_http=debug")),
        ],
    )
}

// --- the app in the sandbox ---

/// The app running in the sandbox: its process, the files the parent asks through and the user's pointer.
struct AppRun {
    running: Running,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar.
    rest: (u32, u32),
    asked: u32,
}

impl AppRun {
    /// Starts the app with `env` and its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let dir_text = dir.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![(CHILD_ENV, "app"), (TRACE_FD_ENV, &fd), (DIR_ENV, &dir_text)];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        vars.push((teams_ui::bots_flag::BOTS_ENV, "1"));
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[writer.as_fd()])?;
        drop(writer);
        // the view trace is drained so that the app never waits on a full pipe
        thread::spawn(move || BufReader::new(reader).lines().map_while(Result::ok).for_each(drop));
        let mut running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let mut run = Self {
            running,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (200, 24),
            asked: 0,
        };
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

    /// Asks the child `verb`; its answer.
    fn question(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state_in(&mut self, verb: &str) -> SandboxResult<Value> {
        let answer = self.question(verb)?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Does `verb` in the app, as its control does when clicked; an error unless the app did it.
    fn act(&mut self, verb: &str) -> SandboxResult {
        let answer = self.state_in(verb)?;
        if answer != "done" {
            return Err(format!("{verb}: {answer}").into());
        }
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// Waits up to [`WAIT`] for a state of `verb` that `wanted` takes, named `what` in the error.
    fn until_in(&mut self, verb: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state_in(verb)?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    /// A screenshot of the screen, kept as `target/ui-shots/<name>`; the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        let kept = shots_dir()?.join(name);
        fs::copy(&png, &kept)?;
        step(&format!("shot {}", kept.display()));
        Ok(png)
    }

    /// Clicks (`x`, `y`) as a user does.
    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa-webhooks-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits through the tray menu, as a user does; once the app ended. Without a tray item the app is stopped.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.running.pid());
        let host = sandbox.bus().connect()?;
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
        let quit = || menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32));
        if wait_until(Duration::from_secs(10), || quit().ok()).is_none() {
            step("no tray item to quit through: the app is stopped");
            let _ = self.running.fail(&"stopped by the walk");
            return Ok(());
        }
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the child ---

/// The account, the runner, and the runtime that signed it in (kept while the app runs).
type OwnStart = (
    ngteams::accounts::Started<MemorySecretStore>,
    ngteams::accounts::Runner,
    tokio::runtime::Runtime,
);

/// The own server's account, signed in with its password: the app's first account. Its team, with the member and a
/// channel, is made through its engine, and the ids are written to [`PLACES`].
fn own_account(dir: &Path) -> SandboxResult<OwnStart> {
    let paths = ngteams::accounts_file::PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = ngteams::setup::Setup::new(paths)?;
    let server = ngteams::accounts_file::OwnServer::parse(&std::env::var(ORIGIN_ENV)?, &std::env::var(CA_ENV)?)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        USER_UPN,
        &server,
        &Secret::new(own_server_binary::PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, None)?;
    let (team, channel) = runtime.block_on(make_team(&engine.handle))?;
    fs::write(dir.join(PLACES), json!({"team": team, "channel": channel}).to_string())?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    Ok((first, support::runner(dir)?, runtime))
}

/// The team with the member and a standard channel: the team's and the channel's threads.
async fn make_team(engine: &teams_core::EngineHandle) -> SandboxResult<(String, String)> {
    let create = TeamWrite::Create(NewTeam {
        name: TEAM_NAME.into(),
        description: "A made-up team".into(),
        first_channel: "General".into(),
        public: false,
    });
    let (team, group) = engine.team_write(create, None).await?.created.ok_or("no team")?;
    let group = group.ok_or("no team group")?;
    let add = TeamWrite::AddMembers {
        team: team.clone(),
        group: group.clone(),
        members: vec![(Mri::new(std::env::var(MEMBER_ENV)?), TeamRole::Member)],
    };
    engine.team_write(add, None).await?;
    let made = engine
        .create_channel(teams_core::ChannelDraft {
            channel: teams_proto::NewChannel {
                team: team.clone(),
                group,
                mailbox: None,
                name: CHANNEL_NAME.into(),
                description: "Where the builds are told".into(),
                kind: teams_proto::NewChannelKind::Standard,
                layout: teams_proto::ChannelLayout::Threads,
            },
            show_by_default: true,
        })
        .await?;
    Ok((team, made.thread.ok_or("no channel")?))
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// The settings as the Appearance page leaves them once theme `key` is picked.
fn show_theme(settings: &mut teams_ui::Settings, key: &'static str) {
    settings.appearance.follow_system = false;
    settings.appearance.theme = key;
}

/// Shows the theme `file` names (`light` or `dark`) once it changes, and says `theme <name>` once it is applied.
fn follow_theme_file(settings: &Entity<teams_ui::SettingsModel>, file: PathBuf, cx: &mut App) {
    let settings = settings.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut applied = String::new();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(wanted) = fs::read_to_string(&file) else {
                continue;
            };
            if wanted == applied {
                continue;
            }
            let key = if wanted == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let changed =
                cx.update(|cx| settings.update(cx, |model, cx| model.change(|chosen| show_theme(chosen, key), cx)));
            if changed.is_err() {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(300)).await;
            say(&format!("theme {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let (first, runner, runtime) = own_account(&dir)?;
    let places: Value = serde_json::from_str(&fs::read_to_string(dir.join(PLACES))?)?;
    let places = Places {
        team: places["team"].as_str().unwrap_or_default().to_owned(),
        channel: places["channel"].as_str().unwrap_or_default().to_owned(),
    };
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ctl = dir.join("ctl.sock");
    let theme_file = dir.join(THEME_FILE);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: Some(ctl),
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, theme_file.clone(), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, ask.clone(), places.clone(), cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    drop(runtime);
    Ok(())
}

/// The team's and the channel's ids.
#[derive(Clone)]
struct Places {
    team: String,
    channel: String,
}

/// Answers the parent's questions as they come.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, places: Places, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = window
                .update(cx, |kit, window, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, &places, window, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| json!("no shell"));
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, places: &Places, window: &mut Window, cx: &mut App) -> Value {
    let (verb, argument) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "teams" => json!({"count": shell.read(cx).teams().read(cx).teams().map_or(0, <[_]>::len)}),
        "dialog" => dialog_state(shell, cx),
        "page" => page_state(shell, cx),
        "posts" => posts_state(shell, cx),
        "thread" if argument.is_empty() => thread_state(shell, cx),
        "clipboard" => json!(cx.read_from_clipboard().and_then(|item| item.text())),
        "clipboard-is-token" => json!(clipboard_is_token(shell, cx)),
        _ => done(act(shell, verb, argument, places, window, cx)),
    }
}

fn done(outcome: Result<(), String>) -> Value {
    outcome.map_or_else(|error| json!(error), |()| json!("done"))
}

/// Does what the control named `verb` does when clicked.
fn act(
    shell: &Entity<Shell>,
    verb: &str,
    argument: &str,
    places: &Places,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let team = || {
        shell
            .read(cx)
            .teams()
            .read(cx)
            .teams()
            .and_then(|teams| teams.iter().find(|team| team.id == places.team).cloned())
            .ok_or_else(|| "the team is not listed".to_owned())
    };
    match verb {
        "manage" => shell.update(cx, |shell, cx| {
            shell.open_manage_team(places.team.clone(), ManageTab::Apps, window, cx);
        }),
        "create" => {
            let team = team()?;
            shell.update(cx, |shell, cx| shell.open_webhook(team, None, window, cx));
        }
        "edit" => {
            let team = team()?;
            let hook = webhook(shell, argument, cx)?;
            shell.update(cx, |shell, cx| shell.open_webhook(team, Some(hook), window, cx));
        }
        "choose" => {
            let bytes = fs::read(argument).map_err(|error| error.to_string())?;
            dialog(shell, cx)?.update(cx, |dialog, cx| dialog.choose(bytes, cx));
        }
        "send" => dialog(shell, cx)?.update(cx, WebhookDialog::send),
        "copy" => dialog(shell, cx)?.update(cx, WebhookDialog::copy_token),
        "close" => dialog(shell, cx)?.update(cx, WebhookDialog::close),
        "ask-remove" => {
            let hook = webhook(shell, argument, cx)?;
            page(shell, cx)?.update(cx, |page, cx| page.ask_remove_webhook(hook, cx));
        }
        "remove" => page(shell, cx)?.update(cx, teams_ui::manage_team::ManageTeam::remove_webhook),
        "channel" => {
            shell.update(cx, Shell::close_team_dialog);
            let channel = places.channel.clone();
            shell.update(cx, |shell, cx| shell.open_channel_id(channel, window, cx));
        }
        "thread" => {
            let channel = shell.read(cx).channel().cloned().ok_or("no channel is open")?;
            let post = argument.to_owned();
            channel.update(cx, |channel, cx| channel.open_thread(post, window, cx));
        }
        other => return Err(format!("unknown {other}")),
    }
    Ok(())
}

fn dialog(shell: &Entity<Shell>, cx: &App) -> Result<Entity<WebhookDialog>, String> {
    shell
        .read(cx)
        .team_dialog()
        .cloned()
        .and_then(|view| view.downcast::<WebhookDialog>().ok())
        .ok_or_else(|| "no webhook dialog".to_owned())
}

fn page(shell: &Entity<Shell>, cx: &App) -> Result<Entity<teams_ui::manage_team::ManageTeam>, String> {
    shell
        .read(cx)
        .manage_team()
        .cloned()
        .ok_or_else(|| "Manage team is not open".to_owned())
}

fn webhook(shell: &Entity<Shell>, name: &str, cx: &App) -> Result<teams_proto::OutgoingWebhook, String> {
    page(shell, cx)?
        .read(cx)
        .webhooks()
        .into_iter()
        .find(|hook| hook.name == name)
        .ok_or_else(|| format!("no webhook {name} is listed"))
}

/// Whether the clipboard holds the token the dialog shows.
fn clipboard_is_token(shell: &Entity<Shell>, cx: &App) -> bool {
    let shown = dialog(shell, cx)
        .ok()
        .and_then(|dialog| dialog.read(cx).token().map(str::to_owned));
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    shown.is_some() && shown == copied
}

/// The dialog: open, Create's state, its problem and whether it shows the token (never the token itself).
fn dialog_state(shell: &Entity<Shell>, cx: &mut App) -> Value {
    let Ok(dialog) = dialog(shell, cx) else {
        return json!({"open": false});
    };
    dialog.update(cx, |dialog, cx| {
        json!({
            "open": true,
            "can_send": dialog.can_send(cx),
            "values": dialog.values(cx),
            "problem": dialog.problem(),
            "token_shown": dialog.token().is_some(),
        })
    })
}

/// The Apps tab's webhooks.
fn page_state(shell: &Entity<Shell>, cx: &App) -> Value {
    let Ok(page) = page(shell, cx) else {
        return json!({"open": false, "hooks": []});
    };
    let page = page.read(cx);
    let hooks: Vec<Value> = page
        .webhooks()
        .iter()
        .map(|hook| {
            json!({
                "name": hook.name, "description": hook.description, "can_manage": hook.can_manage,
                "callback_url": hook.callback_url, "picture": hook.picture_url.is_some(),
            })
        })
        .collect();
    json!({"open": true, "hooks": hooks})
}

/// The channel's posts: the box's bounds and what its picker offers.
fn posts_state(shell: &Entity<Shell>, cx: &App) -> Value {
    let Some(channel) = shell.read(cx).channel() else {
        return json!({"box": null});
    };
    let compose = channel.read(cx).posts().read(cx).compose().read(cx);
    let offered: Vec<String> = compose
        .picker()
        .map(|picker| {
            picker
                .matches
                .iter()
                .map(|candidate| candidate.name.to_string())
                .collect()
        })
        .unwrap_or_default();
    json!({
        "box": compose.box_bounds().map(|bounds| {
            [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].map(f32::from)
        }),
        "offered": offered,
    })
}

/// The open thread's messages.
fn thread_state(shell: &Entity<Shell>, cx: &App) -> Value {
    let thread = shell
        .read(cx)
        .channel()
        .and_then(|channel| channel.read(cx).thread().map(|(_, view)| view.clone()));
    thread.map_or_else(|| json!({"items": []}), |view| state_of(view.read(cx)))
}

fn state_of(view: &ChatView) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({"conversation": view.conversation(), "items": items})
}

fn item_of(item: &MessageItem) -> Value {
    let (kind, blocks, card) = match &item.body {
        Body::Text(rendered) => ("text", rendered.0.blocks.as_slice(), String::new()),
        Body::Cards(cards) => ("cards", &[][..], format!("{:?}", cards.0)),
        _ => ("other", &[][..], String::new()),
    };
    let text = blocks
        .iter()
        .flat_map(Block::plain_lines)
        .collect::<Vec<_>>()
        .join("\n");
    json!({
        "id": item.id, "own": item.own, "kind": kind, "text": text, "card": card,
        "sender": item.sender, "sender_name": item.sender_name.as_ref(),
        "app_mark": item.name_mark == NameMark::App,
    })
}
