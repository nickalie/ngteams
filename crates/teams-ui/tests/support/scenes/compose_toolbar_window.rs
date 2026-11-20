//! The message box's toolbar, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the process's
//! main thread), for its screenshots: the app's window over the fake engine (`common`), a chat with a made-up draft in
//! its box. Each surface — the bar with no AI button, "+" with the AI tools' rows, the bar with Correct and Translate
//! shown, the language menu by Translate's chevron, the customize list from "+" and from a right click on the bar (its
//! first row selected by Down), "+" with hidden buttons, the settings' list, a narrow window's stacked box with the AI
//! tools in its More options and its "+", and a large font — runs the test again inside the sandbox in Teams Light and
//! Teams Dark; the parent screenshots each window into `target/ui-shots/compose-toolbar-<surface>-<theme>.png`. No AI
//! tool runs.
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
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents, LAB_A, LAB_B};
use gpui::{App, AppContext as _, Entity, Window};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::compose::{Compose, CustomizeDown, MenuKind};
use teams_ui::settings::{ComposeTool, Settings, SettingsModel};
use teams_ui::settings_view::Page;
use teams_ui::shell::{Shell, ShellParts};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "the shared fixture's unsized sandbox serves the other scenes")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sized_sandbox_or_skip};

/// Which mode the child runs in: `<theme>:<surface>`.
const CHILD_ENV: &str = "TEAMS_UI_COMPOSE_TOOLBAR_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-compose-toolbar-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before each step, and before the child says `ready`.
const FRAMES: usize = 30;
/// The surfaces shot in a wide window.
const WIDE: [&str; 9] = [
    "bar",
    "ai-plus",
    "ai-shown",
    "ai-language",
    "customize",
    "customize-click",
    "hidden",
    "settings",
    "large-font",
];
/// The surfaces shot in a narrow window, its box stacked.
const NARROW: [&str; 3] = ["narrow", "narrow-more", "narrow-hidden"];
/// The screens, px: a desktop's, and a narrow window's.
const WIDE_SCREEN: (u32, u32) = (1280, 800);
const NARROW_SCREEN: (u32, u32) = (720, 800);
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
            eprintln!("compose toolbar window test: {error}");
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
    for (screen, surfaces) in [(WIDE_SCREEN, &WIDE[..]), (NARROW_SCREEN, &NARROW[..])] {
        let Some(sandbox) = sized_sandbox_or_skip("teams-ui compose toolbar window", Some(screen))? else {
            return Ok(());
        };
        for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
            for surface in surfaces {
                shot(&sandbox, theme, name, surface).map_err(|error| format!("{error}{}", sandbox.logs()))?;
            }
        }
    }
    say("teams-ui compose toolbar window: ok");
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
    let file = format!("compose-toolbar-{surface}-{name}.png");
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

/// The AI tools `surface` shows on the bar.
fn shown_in(surface: &str) -> Vec<ComposeTool> {
    match surface {
        "ai-shown" | "ai-language" | "narrow" | "narrow-more" => vec![ComposeTool::Correct, ComposeTool::Translate],
        _ => Vec::new(),
    }
}

/// The buttons `surface` hides.
fn hidden_in(surface: &str) -> Vec<ComposeTool> {
    match surface {
        "hidden" => vec![ComposeTool::Format, ComposeTool::Emoji, ComposeTool::Loop],
        "narrow-hidden" => vec![ComposeTool::Emoji],
        _ => Vec::new(),
    }
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
            font_size: if surface == "large-font" {
                20
            } else {
                Appearance::default().font_size
            },
            ..Appearance::default()
        },
        selected_chat: Some(AB.to_owned()),
        draft_translate_to: "German".to_owned(),
        compose_hidden: hidden_in(&surface),
        compose_shown: shown_in(&surface),
        ai_features: Some(true),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, APP_ID, "Message box toolbar".into(), parts)?;
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

/// The customize list as a right click on the bar's Emoji opens it, its first row selected by Down.
fn right_click_list(compose: &mut Compose, window: &mut Window, cx: &mut gpui::Context<Compose>) {
    let Some(bounds) = compose.box_bounds() else {
        eprintln!("the box was not drawn");
        return;
    };
    let emoji = gpui::point(bounds.right() - gpui::px(170.0), bounds.bottom() - gpui::px(22.0));
    compose.open_customize(emoji, window, cx);
    after_frames(window, 2, |window, cx| {
        window.dispatch_action(Box::new(CustomizeDown), cx);
    });
}

/// Puts the window in `surface`'s state.
fn show(shell: &Entity<Shell>, surface: &str, window: &mut Window, cx: &mut App) {
    if surface == "settings" {
        shell.update(cx, |shell, cx| shell.open_settings_page(Page::Appearance, window, cx));
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
    });
    // the menus open once the box was drawn with the draft: they read where its buttons are
    after_frames(window, 2, {
        let surface = surface.to_owned();
        move |window, cx| {
            compose.update(cx, |compose, cx| match surface.as_str() {
                "ai-language" => compose.open_language_menu(window, cx),
                "customize" => compose.customize_toolbar(window, cx),
                "customize-click" => right_click_list(compose, window, cx),
                "ai-plus" | "large-font" | "hidden" | "narrow-hidden" => {
                    compose.open_menu(MenuKind::Plus, window, cx);
                }
                "narrow-more" => compose.open_more(window, cx),
                _ => {}
            });
        }
    });
}
