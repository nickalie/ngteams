//! The child of the captions area QA: the real app (`ngteams::app::launch`) in the sandbox as mock account a or b, on
//! the mock's chat service and Trouter. Its settings file is in the parent's directory, so the next run of the app
//! there is a restart on the same files.
//!
//! The child answers the parent's questions through a file in the sandbox dir (`<n> <verb>`; the answer is
//! `re <n> <answer>` on stdout). It tells the app a made-up call and its made-up caption lines as the engine tells
//! them, and it says what the settings and the call hold. Every name and text is made up.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal, Session};
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_mock::{ABC_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::call_stage::CallStage;
use teams_ui::desktop::Desktop;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::SandboxResult;

/// The child's account: `a` or `b`.
pub const CHILD_ENV: &str = "QA867_CHILD";
/// The directory of the app's files, kept by the parent from one run to the next.
pub const ROOT_ENV: &str = "QA867_ROOT";
/// The file in the sandbox dir the parent writes a question to.
pub const ASK: &str = "qa867-ask";
/// The made-up call's number.
const CALL: u64 = 867;

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn mri_of(letter: char) -> Option<&'static str> {
    match letter {
        'a' => Some(LAB_A),
        'b' => Some(LAB_B),
        _ => None,
    }
}

/// The mock account `letter` as the app starts an account. Its session reaches only loopback.
fn started(letter: char, mri: &str, handle: EngineHandle) -> SandboxResult<Started<MemorySecretStore>> {
    let upn = format!("bot-{letter}@lab.example.test");
    let mut account = PersonalAccount::new(
        &upn,
        mri.strip_prefix("8:orgid:").ok_or("mock orgid")?,
        teams_mock::TENANT,
    )?;
    account.name = Some(format!("bot-{letter}"));
    let session = Session::new(
        upn,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/"))?,
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz")?,
        Region::default(),
    );
    Ok(Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    })
}

/// The settings file of the app in `root`.
pub fn settings_file(root: &Path) -> PathBuf {
    root.join("ui").join("settings.toml")
}

pub fn child(letter: &str) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let letter = letter.chars().next().ok_or("no account")?;
    let mri = mri_of(letter).ok_or("unknown account")?;
    let root = PathBuf::from(std::env::var(ROOT_ENV)?);
    let lab = MockLab::start(&root)?;
    let first = started(letter, mri, lab.engine(letter, mri)?)?;
    run_app(&lab, first)
}

fn run_app(lab: &MockLab, first: Started<MemorySecretStore>) -> SandboxResult {
    let paths = PersonalPaths::new(lab.root.join("ngteams-config"), lab.root.join("ngteams-data"));
    let runner = Runner {
        setup: Setup::new(paths)?.with_secrets(AppSecrets::in_memory()),
        floor: CallFloor::new(),
        devices: None,
    };
    let settings = teams_ui::SettingsFile::new(settings_file(&lab.root));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
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
                .update(cx, |kit, _, cx| on_app(kit, &desktop, verb, cx))
                .unwrap_or_else(|_| "no window".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_app(kit: &KitRoot, desktop: &Entity<Desktop>, verb: &str, cx: &mut App) -> String {
    let (head, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    let range = || -> Option<(u64, u64)> {
        let (from, to) = rest.split_once(' ')?;
        Some((from.parse().ok()?, to.parse().ok()?))
    };
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
        "call" => {
            let Some(models) = models_of(kit, cx) else {
                return "no shell".to_owned();
            };
            // the calls shown, as a build with calls shows them
            models.calls.update(cx, |calls, _| {
                *calls = teams_ui::Calls::shown(Some(Mri::new(teams_mock::ECHO)));
            });
            call(&models, cx);
            "ok".to_owned()
        }
        "say" => {
            let (Some((from, to)), Some(models)) = (range(), models_of(kit, cx)) else {
                return "bad say".to_owned();
            };
            for index in from..=to {
                say_line(&models, index, cx);
            }
            "ok".to_owned()
        }
        "captions-off" => {
            let Some(models) = models_of(kit, cx) else {
                return "no shell".to_owned();
            };
            SAID.lock().unwrap_or_else(PoisonError::into_inner).clear();
            models.apply(
                teams_core::Event::CallCaptions {
                    call: teams_core::CallId::for_tests(CALL),
                    captions: teams_core::CallCaptions::default(),
                },
                cx,
            );
            "ok".to_owned()
        }
        "activate" => {
            with_stage_window(cx, |stage, window, cx| {
                window.activate_window();
                // the focus back at the window's start, as a fresh window has it
                window.focus(&stage.focus_handle(), cx);
            });
            "ok".to_owned()
        }
        "transcribe" => {
            let Some(models) = models_of(kit, cx) else {
                return "no shell".to_owned();
            };
            transcribing(&models, Some(LAB_B), cx);
            with_stage_window(cx, |stage, _, cx| stage.show_transcript(true, cx));
            "ok".to_owned()
        }
        "transcript-off" => {
            let Some(models) = models_of(kit, cx) else {
                return "no shell".to_owned();
            };
            with_stage_window(cx, |stage, _, cx| stage.show_transcript(false, cx));
            transcribing(&models, None, cx);
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

fn models_of(kit: &KitRoot, cx: &App) -> Option<teams_ui::Models> {
    let root = kit.view().clone().downcast::<Root>().ok()?;
    let shell = root.read(cx).shell_in(cx)?.clone();
    Some(shell.read(cx).models().clone())
}

fn change_settings(desktop: &Entity<Desktop>, cx: &mut App, change: impl FnOnce(&mut teams_ui::Settings) + 'static) {
    let model = desktop.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(change, cx));
}

/// What the settings and the call hold: the account's saved height of the captions area, the heights in the file,
/// the captions shown, their lines, the transcript, and the call window's size.
fn state_of(kit: &KitRoot, desktop: &Entity<Desktop>, cx: &mut App) -> Value {
    let settings = desktop.read(cx).settings().read(cx).settings();
    let height = |mri: &str| settings.caption_height_of(mri);
    let (heights_a, heights_b) = (height(LAB_A), height(LAB_B));
    let stage = stage_of(cx);
    let window = stage.as_ref().map(|(kit, _)| *kit);
    let size = window.and_then(|window| {
        window
            .update(cx, |_, window, _| {
                let size = window.viewport_size();
                (f32::from(size.width), f32::from(size.height))
            })
            .ok()
    });
    let transcript = stage.as_ref().map(|(_, stage)| stage.read(cx).transcript_shown());
    let calls = models_of(kit, cx).map(|models| {
        let calls = models.calls.read(cx);
        (
            calls.captions_shown(),
            calls.captions().lines.len(),
            calls.spoken().len(),
        )
    });
    json!({
        "height_a": heights_a,
        "height_b": heights_b,
        "stage": stage.is_some(),
        "window": size.map(|(width, height)| [width, height]),
        "transcript": transcript,
        "shown": calls.as_ref().map(|calls| calls.0),
        "lines": calls.as_ref().map(|calls| calls.1),
        "spoken": calls.as_ref().map(|calls| calls.2),
    })
}

fn stage_of(cx: &App) -> Option<(WindowHandle<KitRoot>, Entity<CallStage>)> {
    cx.windows().into_iter().find_map(|window| {
        let kit = window.downcast::<KitRoot>()?;
        let stage = kit.read(cx).ok()?.view().clone().downcast::<CallStage>().ok()?;
        Some((kit, stage))
    })
}

fn with_stage_window(cx: &mut App, change: impl FnOnce(&mut CallStage, &mut Window, &mut gpui::Context<CallStage>)) {
    if let Some((kit, stage)) = stage_of(cx) {
        let _ = kit.update(cx, |_, window, cx| {
            stage.update(cx, |stage, cx| change(stage, window, cx));
        });
    }
}

/// A group call from b in the a, b and c chat, answered and connected, with b and c in its roster.
fn call(models: &teams_ui::Models, cx: &mut App) {
    let call = teams_core::CallId::for_tests(CALL);
    let person = |mri: &str, name: &str| teams_core::CallParticipant {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        video_on: false,
        line: None,
        speaking: false,
        muted: false,
        hand_raised: false,
        organizer: false,
        joined: None,
        sharing: false,
    };
    let events = [
        teams_core::Event::IncomingCall(teams_core::IncomingCall {
            call,
            caller: Mri::new(LAB_B),
            caller_name: Some("Lab Bee".to_owned()),
            group_chat: Some(ABC_CHAT.to_owned()),
            meeting: None,
        }),
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Answering,
        },
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Connected,
        },
        teams_core::Event::CallParticipants {
            call,
            view: teams_core::CallView {
                participants: vec![person(LAB_B, "Lab Bee"), person(LAB_C, "Lab Sea")],
                speaking: false,
                hearing: false,
            },
        },
    ];
    for event in events {
        models.apply(event, cx);
    }
}

/// The roster tells a transcription `by` someone, or none.
fn transcribing(models: &teams_ui::Models, by: Option<&str>, cx: &mut App) {
    models.apply(
        teams_core::Event::CallMeeting {
            call: teams_core::CallId::for_tests(CALL),
            state: teams_core::MeetingState {
                transcribing: by.map(|by| teams_core::Transcription { by: Some(Mri::new(by)) }),
                ..teams_core::MeetingState::default()
            },
        },
        cx,
    );
}

/// The lines the made-up captions hold, as the engine keeps them: its view holds the newest 50.
static SAID: Mutex<Vec<teams_core::Caption>> = Mutex::new(Vec::new());
/// The most lines the engine's view keeps.
const KEPT: usize = 50;

/// Made-up line `index`: b and c in turns, each a final result. The engine tells its whole view of the lines.
fn say_line(models: &teams_ui::Models, index: u64, cx: &mut App) {
    let who = if index.is_multiple_of(2) {
        (LAB_C, "Lab Sea")
    } else {
        (LAB_B, "Lab Bee")
    };
    let line = teams_core::Caption {
        id: format!("synthetic-qa867/{index}"),
        speaker: Some(who.0.to_owned()),
        display_name: Some(who.1.to_owned()),
        text: format!("Synthetic caption line {index} from {}, made up for the test.", who.1),
        final_result: true,
        spoken_language: "en-us".to_owned(),
        track_language: "en-us".to_owned(),
        audio_timestamp: index * 3_000_000,
        duration: 20_000,
    };
    let lines = {
        let mut said = SAID.lock().unwrap_or_else(PoisonError::into_inner);
        said.push(line);
        let extra = said.len().saturating_sub(KEPT);
        said.drain(..extra);
        said.clone()
    };
    models.apply(
        teams_core::Event::CallCaptions {
            call: teams_core::CallId::for_tests(CALL),
            captions: teams_core::CallCaptions {
                phase: teams_core::CaptionPhase::On,
                spoken_language: "en-us".to_owned(),
                error: None,
                lines,
            },
        },
        cx,
    );
}

// --- the mock ---

struct MockLab {
    runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    trouter: TrouterMock,
    root: PathBuf,
}

impl MockLab {
    fn start(root: &Path) -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        Ok(Self {
            runtime,
            mock: Arc::new(mock),
            trouter,
            root: root.to_owned(),
        })
    }

    fn engine(&self, letter: char, mri: &str) -> SandboxResult<EngineHandle> {
        let _entered = self.runtime.enter();
        let tokens = Arc::new(Tokens {
            token: self.mock.token(mri).ok_or("the mock's token")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = self.mock.origin();
        let transport = Arc::new(HttpTransport::with_mock_origin(
            Arc::clone(&tokens),
            CHAT_BASE,
            &origin,
        )?);
        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let store = self.root.join("store").join(format!("{letter}.sqlite"));
        if let Some(dir) = store.parent() {
            fs::create_dir_all(dir)?;
        }
        Ok(engine::start(Parts {
            calls: None,
            store: Store::open(&store)?,
            client: Arc::new(Client::new(HttpTransport::with_mock_origin(
                Arc::clone(&tokens),
                CHAT_BASE,
                &origin,
            )?)),
            outbox: Outbox::new(transport, Author::new(Mri::new(mri), format!("bot-{letter}"))),
            push: Some(Push::new(trouter)),
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
