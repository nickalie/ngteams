//! A draft translated in its box, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread), for its screenshots: the app's window over the fake engine (`common`), a chat with a
//! made-up draft in its box, and a fake translator in place of the AI tools (no tool runs). Each surface — the
//! control with its language, the language menu, a language typed, the progress line, the translation in place, a
//! failure, and the settings' Message translation — runs the test again inside the sandbox in Teams Light and Teams
//! Dark; the parent screenshots each window into `target/ui-shots/compose-translate-<surface>-<theme>.png`.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
#[path = "../../common/mod.rs"]
mod common;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents, LAB_A, LAB_B};
use gpui::{App, AppContext as _, Entity, Window};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::compose::Compose;
use teams_ui::draft_translate::{self, Engine, Session};
use teams_ui::local_ai::{Backend, Failure, Launch, Tools};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::settings_view::Page;
use teams_ui::shell::{Shell, ShellParts};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in: `<theme>:<surface>`.
const CHILD_ENV: &str = "TEAMS_UI_COMPOSE_TRANSLATE_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-compose-translate-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before each step, and before the child says `ready`.
const FRAMES: usize = 30;
/// The surfaces shot.
const SURFACES: [&str; 7] = [
    "control",
    "menu",
    "typed",
    "progress",
    "translated",
    "failed",
    "settings",
];
/// The made-up draft.
const DRAFT: &str = "Good morning, can you send me the plan before lunch?";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("compose translate window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("teams-ui compose translate window")? else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for surface in SURFACES {
            shot(&sandbox, theme, name, surface).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    say("teams-ui compose translate window: ok");
    Ok(())
}

/// Runs the child on `surface` in `theme` and screenshots its window.
fn shot(sandbox: &Sandbox, theme: &str, name: &str, surface: &str) -> SandboxResult {
    let mode = format!("{theme}:{surface}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, &mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("compose-translate-{surface}-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots_dir()?.join(&file);
    fs::copy(&png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    Ok(())
}

// --- the child: the app's window over the fake engine ---

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// The fake translator answers only once this is set (never, for the progress shot).
static RELEASED: AtomicBool = AtomicBool::new(true);
/// The fake translator says the tool is signed out while this is set.
static FAILS: AtomicBool = AtomicBool::new(false);

/// A made-up translator: the made-up draft in German.
struct Translator;

impl Engine for Translator {
    fn start(&self, _: &Launch, _: &AtomicBool) -> Result<Box<dyn Session>, Failure> {
        Ok(Box::new(Self))
    }
}

impl Session for Translator {
    fn ask(&mut self, _: &str, wait: Duration, stop: &AtomicBool) -> Result<String, Failure> {
        let deadline = std::time::Instant::now() + wait;
        while !RELEASED.load(Ordering::Relaxed) {
            if stop.load(Ordering::Relaxed) || std::time::Instant::now() > deadline {
                return Err(Failure::TimedOut(Backend::Claude));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if FAILS.load(Ordering::Relaxed) {
            return Err(Failure::SignedOut(Backend::Claude));
        }
        Ok(
            "<translated><x0>Guten Morgen, kannst du mir den Plan vor dem Mittagessen schicken?</x0></translated>"
                .to_owned(),
        )
    }

    fn close(self: Box<Self>) {}
}

fn text(conversation: &str, sender: &str, content: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: arrival.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: format!("<p>{content}</p>"),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The made-up store: a's chat with b.
fn seed(core: &FakeCore) {
    let now = chrono::Utc::now().timestamp_millis();
    let minute = 60_000;
    locked(&core.conversations).push(Conversation {
        id: AB.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(now - 30 * minute),
    });
    locked(&core.messages).insert(
        AB.to_owned(),
        vec![text(AB, LAB_B, "Morgen! Der Plan ist fast fertig.", now - 30 * minute)],
    );
    locked(&core.members).insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    for (mri, name) in [(LAB_A, "Ann Able"), (LAB_B, "Ben Bright")] {
        locked(&core.names).insert(mri.to_owned(), name.to_owned());
    }
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        #[cfg(windows)]
        if !fixture::scene_activated(window) {
            after_frames(window, frames, done);
            window.refresh();
            return;
        }
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn child(mode: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let (theme, surface) = mode.split_once(':').ok_or("bad child mode")?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let surface = surface.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, surface, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

fn open(theme: &'static str, surface: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    seed(&core);
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        selected_chat: Some(AB.to_owned()),
        message_translate_to: "German".to_owned(),
        ..Settings::default()
    };
    // both tools "installed" at paths nothing runs: the made-up translator answers in their place
    cx.set_global(Tools::with(vec![
        (Backend::Claude, "/made-up/claude".into()),
        (Backend::Codex, "/made-up/codex".into()),
    ]));
    draft_translate::set_engine(cx, Arc::new(Translator));
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, APP_ID, "Translate a message".into(), parts)?;
    let shell = teams_ui::window::content(window, cx)
        .and_then(|root| root.read(cx).shell_in(cx).cloned())
        .ok_or_else(|| io::Error::other("missing the window's shell"))?;
    // the bridge and the event channel live as long as the app
    mem::forget((bridge, events));
    window.update(cx, |_, window, _| {
        after_frames(window, FRAMES, move |window, cx| {
            show(&shell, &surface, window, cx);
            after_frames(window, FRAMES, |_, _| say("ready"));
        });
    })?;
    Ok(())
}

/// The open chat's box.
fn compose(shell: &Entity<Shell>, cx: &App) -> Option<Entity<Compose>> {
    shell.read(cx).chat().map(|chat| chat.read(cx).compose().clone())
}

/// Puts the window in `surface`'s state.
fn show(shell: &Entity<Shell>, surface: &str, window: &mut Window, cx: &mut App) {
    if surface == "settings" {
        shell.update(cx, |shell, cx| shell.open_settings_page(Page::General, window, cx));
        shell.read(cx).settings_view().read(cx).scroll_to_end();
        return;
    }
    let Some(compose) = compose(shell, cx) else {
        eprintln!("no chat open");
        return;
    };
    compose.update(cx, |compose, cx| {
        compose.focus(window, cx);
        compose.editor().update(cx, |editor, cx| editor.insert(DRAFT, cx));
        match surface {
            "menu" | "typed" => {
                compose.open_language_menu(window, cx);
                if let (Some(menu), "typed") = (compose.language_menu(), surface) {
                    menu.update(cx, |menu, cx| menu.search("Klingon", window, cx));
                }
            }
            "progress" => {
                RELEASED.store(false, Ordering::Relaxed);
                compose.translate_draft(window, cx);
            }
            "translated" => compose.translate_draft(window, cx),
            "failed" => {
                FAILS.store(true, Ordering::Relaxed);
                compose.translate_draft(window, cx);
            }
            _ => {}
        }
    });
}
