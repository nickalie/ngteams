//! The child of the group chat rename QA: the real app (`ngteams::app::launch`) in the sandbox.
//!
//! - `mock:<accounts>[:ro]`: mock accounts (`ab`: a, then b) in one window, on the parent's mock chat service and
//!   Trouter. Their stores are in the parent's directory, so a next run of the app sees what the run before kept.
//!   `:ro` starts the app in the read-only mode.
//! - `own`: the made-up account a of the parent's own server, signed in with its password.
//!
//! The child answers the parent's questions through a file in the sandbox dir (`<n> <verb>`; the answer is
//! `re <n> <answer>` on stdout): the state of the shown account's shell, a theme, a font size, a chat to open, quit.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{self, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_mock::{CHAT_BASE, LAB_A, LAB_B, LAB_C};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::Body;
use teams_ui::desktop::Desktop;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::SandboxResult;

#[path = "../window.rs"]
#[expect(
    dead_code,
    reason = "the GUI tests share this support; this one uses its account and runner"
)]
mod support;

/// The child's mode (see the module docs).
pub const CHILD_ENV: &str = "QA_RGC_CHILD";
/// The directory of the app's files, kept by the parent from one run to the next.
pub const ROOT_ENV: &str = "QA_RGC_ROOT";
/// The mock's chat service origin, or the own server's.
pub const ORIGIN_ENV: &str = "QA_RGC_ORIGIN";
/// The mock's Trouter origin.
pub const TROUTER_ENV: &str = "QA_RGC_TROUTER";
/// The own server's certificate authority (PEM file).
pub const CA_ENV: &str = "QA_RGC_CA";
/// The own server's made-up user a signs in as.
pub const OWN_UPN_ENV: &str = "QA_RGC_OWN_UPN";
/// The file in the sandbox dir the parent writes a question to.
pub const ASK: &str = "qa-rgc-ask";

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} is not set"))?)
}

fn mri_of(letter: char) -> Option<&'static str> {
    match letter {
        'a' => Some(LAB_A),
        'b' => Some(LAB_B),
        _ => None,
    }
}

fn alias_of(letter: char) -> Option<support::Alias> {
    match letter {
        'a' => Some(support::Alias::A),
        'b' => Some(support::Alias::B),
        _ => None,
    }
}

/// The letter of the mock account `mri`, or the key itself.
fn letter_of(key: &str) -> String {
    match key {
        LAB_A => "a".to_owned(),
        LAB_B => "b".to_owned(),
        LAB_C => "c".to_owned(),
        other => other.to_owned(),
    }
}

pub fn child(mode: &str) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let root = PathBuf::from(env(ROOT_ENV)?);
    if mode == "own" {
        return own(&root);
    }
    let rest = mode.strip_prefix("mock:").ok_or("unknown mode")?;
    let (accounts, read_only) = match rest.strip_suffix(":ro") {
        Some(accounts) => (accounts, true),
        None => (rest, false),
    };
    let lab = MockLab::start(&root)?;
    let mut started = Vec::new();
    for letter in accounts.chars() {
        let mri = mri_of(letter).ok_or("unknown account")?;
        let alias = alias_of(letter).ok_or("unknown account")?;
        started.push(support::started(alias, lab.engine(letter, mri, read_only)?)?);
    }
    if started.is_empty() {
        return Err("no account".into());
    }
    let first = started.remove(0);
    let runner = Runner {
        setup: Setup::new(PersonalPaths::new(
            root.join("ngteams-config"),
            root.join("ngteams-data"),
        ))?
        .with_secrets(AppSecrets::in_memory()),
        floor: CallFloor::new(),
        devices: None,
    };
    let outcome = run_app(&root, runner, (first, started), read_only);
    drop(lab);
    outcome
}

/// The own server's account a, signed in with its password, as the app starts it.
fn own(root: &Path) -> SandboxResult {
    let (origin, ca, upn) = (env(ORIGIN_ENV)?, env(CA_ENV)?, env(OWN_UPN_ENV)?);
    let paths = PersonalPaths::new(root.join("account").join("config"), root.join("account").join("data"));
    let setup = Setup::new(paths)?;
    let server = accounts_file::OwnServer::parse(&origin, &ca)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let password = Secret::new(super::own_password());
    let account = runtime.block_on(ngteams::signin::add_own(&setup, &upn, &server, &password, &store))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, None)?;
    let first = Started {
        account,
        engine,
        calls: None,
    };
    let runner = support::runner(root)?;
    run_app(root, runner, (first, Vec::new()), false)
}

type Accounts = (Started<MemorySecretStore>, Vec<Started<MemorySecretStore>>);

fn run_app(root: &Path, runner: Runner, (first, others): Accounts, read_only: bool) -> SandboxResult {
    let settings = teams_ui::SettingsFile::new(root.join("ui").join("settings.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others,
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only,
            },
        );
        match launched {
            Ok(launched) => {
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(launched.window, launched.desktop, cx);
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
    say("quit");
    Ok(())
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn shell_of(kit: &KitRoot, cx: &App) -> Option<Entity<Shell>> {
    kit.view()
        .clone()
        .downcast::<Root>()
        .ok()?
        .read(cx)
        .shell_in(cx)
        .cloned()
}

fn answer(window: WindowHandle<KitRoot>, desktop: Entity<Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(20)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| desktop.update(cx, Desktop::quit));
                continue;
            }
            let reply = window
                .update(cx, |kit, window, cx| on_window(kit, &desktop, verb, window, cx))
                .unwrap_or_else(|_| "no window".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_window(kit: &KitRoot, desktop: &Entity<Desktop>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let (head, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match head {
        "state" => state_of(kit, desktop, cx).to_string(),
        "theme" => {
            let key = if rest == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            change_settings(desktop, cx, move |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = key;
            });
            "ok".to_owned()
        }
        "font" => {
            let size: u16 = rest.parse().unwrap_or(teams_ui::theme::FONT_SIZE);
            change_settings(desktop, cx, move |settings| settings.appearance.font_size = size);
            "ok".to_owned()
        }
        "open-chat" => {
            let Some(shell) = shell_of(kit, cx) else {
                return "no shell".to_owned();
            };
            shell.update(cx, |shell, cx| shell.open_chat(rest.to_owned(), window, cx));
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

fn change_settings(desktop: &Entity<Desktop>, cx: &mut App, change: impl FnOnce(&mut teams_ui::Settings) + 'static) {
    let model = desktop.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(change, cx));
}

fn hex(colour: u32) -> String {
    format!("{colour:06x}")
}

fn state_of(kit: &KitRoot, desktop: &Entity<Desktop>, cx: &App) -> Value {
    let accounts = desktop.read(cx).accounts().read(cx);
    let palette = teams_ui::theme::current(cx);
    let shell = shell_of(kit, cx);
    let shell = shell.as_ref().map(|shell| shell.read(cx));
    let rows: Vec<Value> = shell.map_or_else(Vec::new, |shell| {
        shell
            .list()
            .read(cx)
            .rows()
            .iter()
            .map(|row| {
                json!({
                    "id": row.id,
                    "name": row.name.to_string(),
                    "named": row.named,
                    "chat_with": row.chat_with.as_ref().map(ToString::to_string),
                    "kind": format!("{:?}", row.kind),
                })
            })
            .collect()
    });
    let rename = shell.and_then(|shell| shell.rename()).map(|view| {
        let view = view.read(cx);
        json!({
            "chat": view.chat(),
            "value": view.value(cx),
            "too_long": view.too_long(),
            "problem": view.problem().map(ToString::to_string),
            "saving": view.saving(),
        })
    });
    let switcher = shell.and_then(|shell| shell.switcher()).map(|switcher| {
        let switcher = switcher.read(cx);
        json!({
            "query": switcher.query(),
            "matches": switcher.matches().iter().map(|(id, name)| json!([id, name.to_string()])).collect::<Vec<_>>(),
        })
    });
    let chat = shell.and_then(|shell| shell.chat()).map(|chat| chat.read(cx));
    let events: Vec<String> = chat.map_or_else(Vec::new, |chat| {
        chat.message_items()
            .filter_map(|item| match &item.body {
                Body::Event(line) | Body::TimedEvent(line) => Some(line.text()),
                _ => None,
            })
            .collect()
    });
    json!({
        "shown": accounts.active_key().map(letter_of),
        "chat": chat.map(|chat| chat.conversation().to_owned()),
        "rows": rows,
        "rename": rename,
        "switcher": switcher,
        "events": events,
        "toast": shell.and_then(|shell| shell.toast().map(ToString::to_string)),
        "palette": {
            "background": hex(palette.background),
            "surface": hex(palette.surface),
            "border": hex(palette.border),
            "brand": hex(palette.brand),
            "foreground": hex(palette.foreground),
        },
    })
}

// --- the mock's accounts ---

/// The parent's mock, as each account's engine reaches it.
struct MockLab {
    runtime: tokio::runtime::Runtime,
    origin: String,
    trouter: String,
    root: PathBuf,
}

impl MockLab {
    fn start(root: &Path) -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        Ok(Self {
            runtime,
            origin: env(ORIGIN_ENV)?,
            trouter: env(TROUTER_ENV)?,
            root: root.to_owned(),
        })
    }

    /// Mock account `letter`'s engine; in the read-only mode (`read_only`) with read-only transports and no push
    /// channel, as `ngteams::read_only` starts it.
    fn engine(&self, letter: char, mri: &str, read_only: bool) -> SandboxResult<EngineHandle> {
        let _entered = self.runtime.enter();
        let tokens = Arc::new(Tokens {
            token: env(&super::token_env(letter))?,
            renewals: broadcast::channel(4).0,
        });
        let http = || -> SandboxResult<HttpTransport<Tokens>> {
            let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &self.origin)?;
            Ok(if read_only { transport.read_only() } else { transport })
        };
        let transport = Arc::new(http()?);
        let push = if read_only {
            None
        } else {
            let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &self.trouter)?
                .with_options(Options {
                    backoff_base: Duration::from_millis(50),
                    backoff_cap: Duration::from_millis(50),
                    backoff_jitter_percent: 0,
                    ..Options::default()
                });
            Some(Push::new(trouter))
        };
        let store = self.root.join("store").join(format!("{letter}.sqlite"));
        if let Some(dir) = store.parent() {
            fs::create_dir_all(dir)?;
        }
        Ok(engine::start(Parts {
            calls: None,
            store: Store::open(&store)?,
            client: Arc::new(Client::new(http()?)),
            outbox: Outbox::new(transport, Author::new(Mri::new(mri), format!("bot-{letter}"))),
            push,
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?)
    }
}

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}
