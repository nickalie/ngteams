//! One message translated from its "More options" menu, as a user does it in the app's window in the UI sandbox
//! (`harness = false`: GPUI runs on the process's main thread). It is the QA test of task 575.
//!
//! The parent runs the mock (`teams-mock`) with made-up messages in German and English, and runs a's app as a child in
//! the sandbox with its store and settings in one directory, so a later run is a restart of the same app on the same
//! service. The translating tool is a made-up `claude` ([`FAKE_CLAUDE`]) first on the app's `PATH`: it answers each
//! message of a batch in English from a small dictionary (the language it names is German when it knew the words),
//! logs each question, and waits or fails while the parent's files say so. The parent moves the sandbox's pointer and
//! presses its keys; it finds the hover bar, the menu and its list on the screen, and reads the views' rows (their
//! text, mark and offer, and where the list drew them) from the child.
//!
//! - **The menu**: "Translation" in b's and a's own message's More options, its list ("Translate", "Settings"),
//!   "Translating…" while the tool works, the translation with its mark, "See original message (German)" and back, a
//!   second Translate at once without the tool; the key T on a message selected with the keys; whether a key opens
//!   the menu. "Settings" opens Settings > General.
//! - **Odd messages**: one already in English; one b edits after its translation; one b deletes.
//! - **Failures**: the tool signed out (the red mark, its tip, a click on it tries again); no tool at all.
//! - **The whole chat** read in English, then as written again, around the messages translated alone.
//! - **Other places**: a group chat, a channel's post and a reply in its thread.
//! - **A restart**: the messages translated alone show translated at once, the tool not asked.
//! - **An own server's account** on the real `ngteams-server` binary: the same path.
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa575-*.png`, in Teams Light and
//! Dark. `QA575_ONLY` (`mock` or `own`) runs only those runs. Without weston 15, niri or `dbus-daemon` it says so and
//! passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one starts it and adds users"
)]
mod own_server_binary;
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::AccountEngine;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    PRESENCE_BASE, PROVISIONING_BASE, TEAM_CHANNEL, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_translate::Mark;
use teams_ui::chat_view::{Body, ChatView, Item};
use teams_ui::compose::Mode;
use teams_ui::rich_text::Block;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "QA575_CHILD";
const ORIGIN_ENV: &str = "QA575_ORIGIN";
const TROUTER_ENV: &str = "QA575_TROUTER";
const TOKEN_ENV: &str = "QA575_TOKEN";
const CA_ENV: &str = "QA575_CA";
const DIR_ENV: &str = "QA575_DIR";
/// The made-up tool's directory, the only one on the child's `PATH` with a `claude`.
const TOOLS_ENV: &str = "QA575_TOOLS";
/// `own` (an own server's account); else a's account on the mock.
const MODE_ENV: &str = "QA575_MODE";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa575-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);
/// How long the parent watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(2500);

/// Teams Light's surface and its borders.
const SURFACE: [u8; 3] = [255, 255, 255];
const BORDER: [u8; 3] = [224, 224, 224];
/// Teams' red (`theme::DANGER`).
const DANGER: [u8; 3] = [0xc4, 0x31, 0x4b];
/// The message box of a chat on the 1280×800 screen, a one-line box.
const COMPOSE: (u32, u32) = (700, 749);
/// The evdev codes of F10 and the context-menu key.
const KEY_F10: u32 = 68;
const KEY_MENU: u32 = 127;
/// The mock's team channel's post with replies under it.
const CHANNEL_POST: &str = "1760000300000";

/// The made-up messages, as written and as the made-up tool translates them into English.
const HELLO: (&str, &str) = (
    "Guten Morgen! Das Treffen beginnt heute um drei Uhr.",
    "Good morning! The meeting starts today at three.",
);
const ENGLISH: &str = "Thanks, the report looks good to me.";
const OWN: (&str, &str) = ("Danke, bis morgen!", "Thanks, see you tomorrow!");
const LUNCH: (&str, &str) = ("Das Mittagessen ist um zwei.", "Lunch is at two.");
const LUNCH_EDITED: (&str, &str) = ("Das Mittagessen ist um drei.", "Lunch is at three.");
const GONE: (&str, &str) = ("Diese Nachricht kommt gleich weg.", "This message goes away soon.");
const FOUR: (&str, &str) = ("Wir sehen uns um vier.", "See you at four.");
const WEATHER: (&str, &str) = ("Das Wetter ist heute gut.", "The weather is good today.");
const BUS: (&str, &str) = ("Der Bus kommt um acht.", "The bus comes at eight.");
const GROUP: (&str, &str) = (
    "Bitte schickt mir den Bericht bis Freitag.",
    "Please send me the report by Friday.",
);
const POST: (&str, &str) = (
    "Die neue Version ist fertig zum Testen.",
    "The new version is ready for testing.",
);
const REPLY: (&str, &str) = ("Ich schaue es mir morgen an.", "I will look at it tomorrow.");
const OWN_SERVER: (&str, &str) = ("Hallo Dana, kannst du mir helfen?", "Hello Dana, can you help me?");

/// Every pair the made-up tool knows.
const DICTIONARY: [(&str, &str); 12] = [
    HELLO,
    OWN,
    LUNCH,
    LUNCH_EDITED,
    GONE,
    FOUR,
    WEATHER,
    BUS,
    GROUP,
    POST,
    REPLY,
    OWN_SERVER,
];

/// A made-up Claude Code translating a chat's batches (`__STATE__` its state directory): each message in English from
/// `dictionary.sed`, named German when the dictionary changed it and English when not; it logs each question, waits
/// while `hold` exists and fails as signed out while `fail` does.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
printf 'start pid=%s\n' "$$" >> "$state/calls.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  while [ -e "$state/hold" ]; do sleep 0.05; done
  if [ -e "$state/fail" ]; then echo 'Invalid API key. Please run /login' >&2; exit 1; fi
  echo '{"type":"system","subtype":"init"}'
  body=""
  case "$line" in
    *'<translate>'*)
      inner=$(printf '%s' "$line" | sed 's/.*<translate>\\n//; s/\\n<\/translate>.*//')
      while IFS= read -r m; do
        [ -z "$m" ] && continue
        t=$(printf '%s' "$m" | sed -f "$state/dictionary.sed")
        if [ "$t" != "$m" ]; then lang=de; else lang=en; fi
        t=$(printf '%s' "$t" | sed 's/^\(<m id=\\"[0-9]*\\"\) from=\\"[^\\]*\\"\( re=\\"[^\\]*\\"\)\{0,1\}>/\1 lang=\\"'"$lang"'\\">/')
        body="$body$t\\n"
      done < <(printf '%s\n' "$inner" | sed 's/\\n/\n/g')
      ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$body" | tee -a "$state/answers.log"
done
"#;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_translate_message_window: {error}");
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams translate one message", &options)? else {
        return Ok(());
    };
    let only = std::env::var("QA575_ONLY").unwrap_or_default();
    let tool = FakeTool::install(&sandbox)?;
    if only.is_empty() || only == "mock" {
        let service = Service::start(&runtime)?;
        let dir = tempfile::tempdir()?;
        let outcome = mock_runs(&sandbox, &runtime, &service, &tool, dir.path());
        if let Err(error) = outcome {
            return Err(format!("{error}{}", sandbox.logs()).into());
        }
    }
    if only.is_empty() || only == "own" {
        let dir = tempfile::tempdir()?;
        tool.reinstall()?;
        if let Err(error) = own_server(&sandbox, &tool, dir.path()) {
            return Err(format!("{error}{}", sandbox.logs()).into());
        }
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams translate one message: ok");
    Ok(())
}

/// The runs on the mock, on one store: the walk, a restart, and a start without the tool.
fn mock_runs(
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    service: &Service,
    tool: &FakeTool,
    dir: &Path,
) -> SandboxResult {
    step("--- run walk ---");
    let mut run = AppRun::launch(sandbox, &service.env(), dir, tool, "")?;
    let outcome = (|| -> SandboxResult {
        let mut walk = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service: Some(service),
            tool,
        };
        walk.menu_on_b_s_message()?;
        walk.see_original_and_again()?;
        walk.own_message_by_its_key()?;
        walk.keys_for_the_menu()?;
        walk.other_keys()?;
        walk.already_in_english()?;
        walk.settings_row()?;
        walk.tool_fails()?;
        walk.edited_after()?;
        walk.deleted_after()?;
        walk.whole_chat_around()?;
        walk.group_chat()?;
        walk.channel_post_and_reply()
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit()?;

    step("--- run restart ---");
    let mut run = AppRun::launch(sandbox, &service.env(), dir, tool, "")?;
    let outcome = Walk {
        run: &mut run,
        sandbox,
        runtime,
        service: Some(service),
        tool,
    }
    .after_restart();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit()?;

    step("--- run no-tool ---");
    tool.uninstall()?;
    service
        .mock
        .post_as(LAB_B, AB_CHAT, &format!("<p>{}</p>", BUS.0))
        .ok_or("b's last message refused")?;
    let mut run = AppRun::launch(sandbox, &service.env(), dir, tool, "")?;
    let outcome = Walk {
        run: &mut run,
        sandbox,
        runtime,
        service: Some(service),
        tool,
    }
    .no_tool();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit()
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    runtime: &'a tokio::runtime::Runtime,
    service: Option<&'a Service>,
    tool: &'a FakeTool,
}

/// No made-up messages: the own server's walk.
static NO_IDS: Ids = Ids {
    hello: String::new(),
    english: String::new(),
    own: String::new(),
    lunch: String::new(),
    gone: String::new(),
    four: String::new(),
    weather: String::new(),
    group: String::new(),
    post: String::new(),
    reply: String::new(),
};

/// Where the parts of an open Translation list are on the screen.
#[derive(Clone, Copy, Debug)]
struct ListAt {
    translate: (u32, u32),
    settings: (u32, u32),
}

impl Walk<'_> {
    fn ids(&self) -> &Ids {
        self.service.map_or(&NO_IDS, |service| &service.ids)
    }

    fn service(&self) -> SandboxResult<&Service> {
        self.service.ok_or_else(|| "no mock in this run".into())
    }

    /// The shots of `name` in Teams Light, then Teams Dark, then back to Light.
    fn shots(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let light = self.run.shot(self.sandbox, &format!("qa575-{name}-light.png"))?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa575-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(light)
    }

    /// Opens `chat` as a click on its row does, once its message `id` shows.
    fn open_chat(&mut self, chat: &str, id: &str) -> SandboxResult {
        self.run.question(&format!("open {chat}"))?;
        self.run.until("chat", &format!("the chat {chat} with {id}"), |state| {
            item(state, id).is_some()
        })?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The row of `id` in `view` inside the list's viewport: the list is scrolled with the wheel first when it is not.
    fn row(&mut self, view: &str, id: &str) -> SandboxResult<Rect> {
        let mut last = Value::Null;
        for _ in 0..40 {
            let state = self.run.state(view)?;
            last = json!({"viewport": state["viewport"], "row": item(&state, id)});
            let index = state["items"]
                .as_array()
                .and_then(|items| items.iter().position(|row| row["id"] == id))
                .ok_or_else(|| format!("no message {id} in the {view}: {state}"))?;
            let row = &state["items"][index];
            let viewport = rect_of(&state["viewport"]).ok_or("no viewport")?;
            let rect = rect_of(&row["rect"]);
            if let Some(rect) = rect
                && rect.y0 >= viewport.y0 + 40
                && rect.y1 <= viewport.y1 + 2
            {
                return Ok(rect);
            }
            let placed = state["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|other| !other["rect"].is_null()));
            if !placed && let Some(found) = self.hover_scan(view, id, viewport)? {
                return Ok(found);
            }
            // above the view when it is drawn above it, or when no newer message is drawn above the view's bottom
            let below = rect.map_or_else(
                || {
                    state["items"].as_array().is_some_and(|items| {
                        items[..index]
                            .iter()
                            .filter_map(|other| rect_of(&other["rect"]))
                            .any(|other| other.y0 >= viewport.y1)
                    })
                },
                |rect| rect.y1 > viewport.y1 + 2,
            );
            let (x, y) = viewport.centre();
            self.run.input.scroll(x, y, if below { 3 } else { -3 })?;
            thread::sleep(Duration::from_millis(300));
        }
        Err(format!("the message {id} never came into the {view}'s view: {last}").into())
    }

    /// Where the message `id` is in the view, found as the pointer finds it: moved up the view, the rows it hovers.
    /// (The list tells no row's place while all its rows fit the view.)
    fn hover_scan(&mut self, view: &str, id: &str, viewport: Rect) -> SandboxResult<Option<Rect>> {
        let mut found: Option<Rect> = None;
        let mut y = viewport.y1.saturating_sub(3);
        while y > viewport.y0 + 2 {
            self.run.input.move_to(viewport.x0 + 6, y)?;
            thread::sleep(Duration::from_millis(40));
            let hovered = self.run.state(view)?["hovered"] == id;
            if hovered {
                found = Some(grow(found, viewport.x0, y));
            } else if found.is_some() {
                break;
            }
            y -= 4;
        }
        self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
        Ok(found.map(|rect| Rect {
            x1: viewport.x1,
            y1: rect.y1 + 3,
            ..rect
        }))
    }

    /// Points at the message `id` and finds its hover bar's "More options": its centre.
    fn more_button(&mut self, view: &str, id: &str) -> SandboxResult<(u32, u32)> {
        let row = self.row(view, id)?;
        let still = load(&self.run.shot(self.sandbox, "qa575-still.png")?)?;
        // in the row's left margin: never right of the bar, never on a face (its profile card would open)
        self.run
            .input
            .move_to(row.x0 + 6, row.y0 + (row.height() / 2).min(14))?;
        thread::sleep(SETTLE);
        let hover = load(&self.sandbox.screenshot("qa575-hover.png", false)?)?;
        let region = Rect {
            x0: row.x0,
            y0: row.y0.saturating_sub(48),
            x1: row.x1,
            y1: row.y0 + 36,
        };
        let bar = changed_box(&still, &hover, region).ok_or_else(|| format!("no hover bar over the message {id}"))?;
        let band = Rect {
            x0: bar.x1.saturating_sub(30),
            ..bar
        };
        let button = changed_box(&still, &hover, band).unwrap_or(band);
        Ok((bar.x1.saturating_sub(17), button.y0 + 17))
    }

    /// Opens the More options menu of `id` with a click on its hover bar's button; the button's centre, and the screen
    /// with the bar before the click.
    fn open_menu(&mut self, view: &str, id: &str) -> SandboxResult<((u32, u32), PathBuf)> {
        let (x, y) = self.more_button(view, id)?;
        let before = self.sandbox.screenshot("qa575-before-menu.png", false)?;
        for (dx, dy) in [(0_i32, 0_i32), (-6, -4), (-10, 4)] {
            let at = (x.saturating_add_signed(dx), y.saturating_add_signed(dy));
            self.run.click(at.0, at.1)?;
            thread::sleep(SETTLE);
            let state = self.run.state(view)?;
            if state["options"] == id {
                return Ok((at, before));
            }
            if !state["options"].is_null() {
                self.close_menu(view)?;
            }
        }
        Err(format!("the More options menu of {id} never opened").into())
    }

    /// Opens the menu of `id` and its "Translation" list; where the list's rows are. `own`: the menu also offers
    /// Delete, before Mark as unread and Translation. A thread's reply offers no Mark as unread.
    fn open_list(&mut self, view: &str, id: &str, own: bool) -> SandboxResult<ListAt> {
        let ((x, y), before) = self.open_menu(view, id)?;
        let menu = load(&self.run.shot(self.sandbox, "qa575-menu.png")?)?;
        let column = x.saturating_sub(100);
        // a menu with no room under its button opens over it: its own hairlines are not its top. Over the bar, the
        // bar's surface is the menu's too: there the top is the first row the menu changed.
        let over = Rect {
            x0: column.saturating_sub(4),
            y0: y.saturating_sub(320).max(60),
            x1: column + 4,
            y1: menu.height(),
        };
        let top = panel_top(&menu, column, y + 12..menu.height())
            .or_else(|| panel_top(&menu, column, y.saturating_sub(320).max(60)..y))
            .or_else(|| changed_box(&load(&before).ok()?, &menu, over).map(|changed| changed.y0))
            .ok_or("no menu's top edge under its button")?;
        // the rows of Copy, Delete (the account's own), a hairline (1 px with 4 px over and under it), Mark as unread,
        // then Translation: 32 px with 2 px between, under 1 + 4 px
        let marks = view != "thread";
        let index = u32::from(own) + if marks { 2 } else { 1 };
        let row_y = top + 1 + 4 + 34 * index + if marks { 11 } else { 0 } + 16;
        self.run.click(column, row_y)?;
        thread::sleep(SETTLE);
        let listed = load(&self.run.shot(self.sandbox, "qa575-list.png")?)?;
        // the list is left of the menu (200 px wide, its right edge the button's), 4 px apart, 260 px wide
        let list_x = (x + 14).saturating_sub(200 + 4 + 130);
        let list_top = border_top(&listed, list_x, top.saturating_sub(160).max(60)..listed.height())
            .ok_or_else(|| format!("no Translation list beside the menu of {id}"))?;
        let separator =
            separator_under(&listed, list_x, list_top + 2..listed.height()).ok_or("no line between the list's rows")?;
        Ok(ListAt {
            translate: (list_x, separator.saturating_sub(22)),
            settings: (list_x, separator + 23),
        })
    }

    /// Closes the open More options menu: one Escape, as in Teams; else a second one, else a click outside it.
    fn close_menu(&mut self, view: &str) -> SandboxResult {
        static SAID: std::sync::Once = std::sync::Once::new();
        static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        if self.run.state(view)?["options"].is_null() {
            if !SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
                self.shots("escape-closed")?;
            }
            return Ok(());
        }
        let _ = self.run.shot(self.sandbox, "qa575-escape-menu-light.png");
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let second = self.run.state(view)?["options"].is_null();
        SAID.call_once(|| {
            note(format!(
                "the first Escape does not close a More options menu opened with a click (Teams closes it); \
                 a second Escape closes it: {second}"
            ));
        });
        if second {
            return Ok(());
        }
        self.run.click(self.run.rest.0, self.run.rest.1)?;
        thread::sleep(SETTLE);
        if !self.run.state(view)?["options"].is_null() {
            return Err("a click outside does not close the More options menu".into());
        }
        Ok(())
    }

    /// The message `id`'s offer in its list, by its words.
    fn offer(&mut self, view: &str, id: &str) -> SandboxResult<Value> {
        let state = self.run.state(view)?;
        Ok(item(&state, id).map_or(Value::Null, |row| row["offer"].clone()))
    }

    /// Chooses the first row of the Translation list of `id` (Translate, or See original message).
    fn choose_offer(&mut self, view: &str, id: &str, own: bool) -> SandboxResult {
        let list = self.open_list(view, id, own)?;
        self.run.click(list.translate.0, list.translate.1)?;
        Ok(())
    }

    /// Waits until the message `id` in `view` shows `text` with its mark of `kind` (`none`: unmarked).
    fn until_shows(&mut self, view: &str, id: &str, text: &str, kind: &str) -> SandboxResult<Value> {
        let state = self
            .run
            .until(view, &format!("{id} shows {text:?} marked {kind}"), |state| {
                item(state, id).is_some_and(|row| row["text"] == text && row["mark"]["kind"] == kind)
            })?;
        Ok(item(&state, id).cloned().unwrap_or(Value::Null))
    }

    /// Waits until the message `id` is marked `kind`.
    fn until_mark(&mut self, view: &str, id: &str, kind: &str) -> SandboxResult<Value> {
        let state = self.run.until(view, &format!("{id} marked {kind}"), |state| {
            item(state, id).is_some_and(|row| row["mark"]["kind"] == kind)
        })?;
        Ok(item(&state, id).cloned().unwrap_or(Value::Null))
    }

    // --- the walk ---

    /// b's German message: "Translation" in its menu, its list, Translate, "Translating…", then its translation.
    fn menu_on_b_s_message(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        self.open_chat(AB_CHAT, &hello)?;
        self.run.shot(self.sandbox, "qa575-chat-light.png")?;
        expect(self.offer("chat", &hello)? == "Translate", || {
            "b's German message does not offer Translate".to_owned()
        });
        let list = self.open_list("chat", &hello, false)?;
        self.shots("menu")?;
        let before = self.tool.questions();
        self.tool.hold(true)?;
        self.run.click(list.translate.0, list.translate.1)?;
        self.until_mark("chat", &hello, "waiting")?;
        self.shots("translating")?;
        self.tool.hold(false)?;
        let row = self.until_shows("chat", &hello, HELLO.1, "translated")?;
        expect(row["offer"] == "See original message (German)", || {
            format!("a translated German message offers {}", row["offer"])
        });
        self.shots("translated")?;
        let asked = self.tool.questions();
        expect(asked.len() == before.len() + 1, || {
            format!("one Translate asked the tool {} times", asked.len() - before.len())
        });
        if let Some(question) = asked.last() {
            // the batch names its messages by numbers of its own
            let messages = question.matches("<m id=").count();
            expect(question.contains(HELLO.0) && messages == 1, || {
                format!("the tool was asked for other messages too: {question}")
            });
        }
        let state = self.run.state("chat")?;
        expect(state["notice"].is_null(), || {
            format!("one message translated shows the chat's notice: {}", state["notice"])
        });
        step("b's message: Translate from its menu, Translating…, translated");
        Ok(())
    }

    /// "See original message (German)" shows it as written, unmarked; Translate again shows it at once.
    fn see_original_and_again(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        self.open_list("chat", &hello, false)?;
        self.shots("translated-menu")?;
        self.close_menu("chat")?;
        let asked = self.tool.questions().len();
        self.choose_offer("chat", &hello, false)?;
        let row = self.until_shows("chat", &hello, HELLO.0, "none")?;
        expect(row["offer"] == "Translate", || {
            format!("the original again offers {}", row["offer"])
        });
        self.shots("original")?;
        self.choose_offer("chat", &hello, false)?;
        self.until_shows("chat", &hello, HELLO.1, "translated")?;
        thread::sleep(QUIET);
        expect(self.tool.questions().len() == asked, || {
            "Translate after See original message asked the tool again".to_owned()
        });
        step("See original message, and Translate again at once");
        Ok(())
    }

    /// a's own message, selected with the keys (Escape out of the box, then Up), translated by T.
    fn own_message_by_its_key(&mut self) -> SandboxResult {
        let own = self.ids().own.clone();
        self.select_by_keys(&own)?;
        let key = self.run.letter('t')?;
        self.run.input.key(key)?;
        self.until_shows("chat", &own, OWN.1, "translated")?;
        self.shots("own-translated")?;
        // T again on it: as written again
        self.run.input.key(key)?;
        self.until_shows("chat", &own, OWN.0, "none")?;
        self.run.input.key(key)?;
        self.until_shows("chat", &own, OWN.1, "translated")?;
        step("a's own message: T, T, T");
        Ok(())
    }

    /// Selects the message `id` of the chat as the keys do: a click in the box, Escape, then Up until it is selected.
    fn select_by_keys(&mut self, id: &str) -> SandboxResult {
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(Duration::from_millis(300));
        for _ in 0..40 {
            self.run.input.key(keys::UP)?;
            thread::sleep(Duration::from_millis(150));
            if self.run.state("chat")?["selected"] == id {
                return Ok(());
            }
        }
        Err(format!("Up never selected the message {id}").into())
    }

    /// With a message selected by the keys, whether a key opens its More options menu: Teams opens it from the keys.
    fn keys_for_the_menu(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        self.select_by_keys(&hello)?;
        let tries: [(&str, &[Modifier], u32); 4] = [
            ("Shift+F10", &[Modifier::Shift], KEY_F10),
            ("the menu key", &[], KEY_MENU),
            ("Tab, then Enter", &[], keys::TAB),
            ("Enter", &[], keys::ENTER),
        ];
        let mut opened = Vec::new();
        for (name, modifiers, key) in tries {
            self.select_by_keys(&hello)?;
            self.run.input.key_with(modifiers, key)?;
            if key == keys::TAB {
                self.run.input.key(keys::ENTER)?;
            }
            thread::sleep(SETTLE);
            let state = self.run.state("chat")?;
            if state["options"] == hello.as_str() {
                opened.push(name);
                self.close_menu("chat")?;
            }
            self.run.shot(
                self.sandbox,
                &format!("qa575-keys-{}-light.png", name.replace([' ', ','], "-")),
            )?;
        }
        expect(!opened.is_empty(), || {
            "no key opens a selected message's More options menu (Shift+F10, the menu key, Tab then Enter, \
             Enter): its Translation list and Settings are out of the keyboard's reach"
                .to_owned()
        });
        self.run.input.key(keys::ESC)?;
        step(&format!("the keys that open the menu: {opened:?}"));
        self.menu_by_keys()
    }

    /// The chat's other keys still work with the menu keys there: Up and Down move the selection, also after the menu
    /// closes; Tab leaves the list and opens no menu; Tab in the menu closes it; Escape with a reply or an edit open
    /// in the box cancels it with one press, and with the menu open by a click, closes the menu first.
    fn other_keys(&mut self) -> SandboxResult {
        self.arrows_by_the_menu()?;
        self.tab_keys()?;
        self.escape_with_reply()?;
        self.escape_with_edit()?;
        step("the other keys: Up, Down, Tab, Escape with a reply or an edit");
        Ok(())
    }

    /// Where the keys are in the chat: `list`, `box`, `other` or `none`.
    fn focus(&mut self) -> SandboxResult<String> {
        self.run.question("focus chat")
    }

    /// Up and Down move the selection, also after the menu opened by the keys closes with Escape.
    fn arrows_by_the_menu(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        self.select_by_keys(&hello)?;
        self.run.input.key(keys::DOWN)?;
        thread::sleep(SETTLE);
        let below = self.run.state("chat")?["selected"].clone();
        expect(below != hello.as_str() && !below.is_null(), || {
            format!("Down on a selected message selects {below}")
        });
        self.run.input.key(keys::UP)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["selected"] == hello.as_str(), || {
            "Up after Down does not select the message again".to_owned()
        });
        // the menu by the keys, then Escape: the keys are back on the list, and Up and Down move the selection
        self.menu_key_on("chat", &hello)?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let (state, at) = (self.run.state("chat")?, self.focus()?);
        expect(
            state["options"].is_null() && state["selected"] == hello.as_str() && at == "list",
            || {
                format!(
                    "Escape in the menu: options {}, selected {}, focus {at}",
                    state["options"], state["selected"]
                )
            },
        );
        self.run.input.key(keys::DOWN)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["selected"] == below, || {
            "Down after the menu closed does not move the selection".to_owned()
        });
        self.run.input.key(keys::UP)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["selected"] == hello.as_str(), || {
            "Up after the menu closed does not move the selection".to_owned()
        });
        Ok(())
    }

    /// Tab in the menu closes it, the keys back on the list; Tab in the list leaves it and opens no menu.
    fn tab_keys(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        self.menu_key_on("chat", &hello)?;
        self.run.input.key(keys::TAB)?;
        thread::sleep(SETTLE);
        let (state, at) = (self.run.state("chat")?, self.focus()?);
        expect(state["options"].is_null() && at == "list", || {
            format!("Tab in the menu: options {}, focus {at}", state["options"])
        });
        if !state["options"].is_null() {
            self.key_shot("qa575-tab-in-menu-light.png")?;
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
            let at = self.focus()?;
            if at != "list" {
                self.run.question(&format!("select chat {hello}"))?;
                thread::sleep(SETTLE);
            }
        }
        // Tab in the list: the focus leaves it, and no menu opens
        self.run.input.key(keys::TAB)?;
        thread::sleep(SETTLE);
        let (state, at) = (self.run.state("chat")?, self.focus()?);
        expect(state["options"].is_null() && at != "list", || {
            format!("Tab in the list: options {}, focus {at}", state["options"])
        });
        self.key_shot("qa575-tab-out-light.png")?;
        step(&format!("Tab out of the list: the focus goes to {at}"));
        Ok(())
    }

    /// A reply: one Escape cancels it. With the menu open by a click, Escape closes the menu first, the next the
    /// reply, the next moves the keys to the list.
    fn escape_with_reply(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        // the selected message shows its bar already: a click opens the menu of another one
        let english = self.ids().english.clone();
        let reply = self.run.letter('r')?;
        self.select_by_keys(&hello)?;
        self.run.input.key(reply)?;
        self.run
            .until("chat", "r opens a reply", |state| state["compose"] == "reply")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["compose"] == "new", || {
            "one Escape does not cancel the reply".to_owned()
        });
        self.let_selection_go()?;
        // a reply and the menu open by a click: Escape closes the menu, the next the reply, the next moves the focus
        self.select_by_keys(&hello)?;
        self.run.input.key(reply)?;
        self.run
            .until("chat", "r opens a reply", |state| state["compose"] == "reply")?;
        self.open_menu("chat", &english)?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let state = self.run.state("chat")?;
        expect(state["options"].is_null() && state["compose"] == "reply", || {
            format!(
                "Escape with a reply and the menu open: options {}, box {}",
                state["options"], state["compose"]
            )
        });
        self.shots("reply-kept")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let (state, at) = (self.run.state("chat")?, self.focus()?);
        expect(state["compose"] == "new" && at == "box", || {
            format!("the second Escape: box {}, focus {at}", state["compose"])
        });
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let at = self.focus()?;
        expect(at == "list", || format!("the third Escape: focus {at}"));
        self.let_selection_go()
    }

    /// An edit of the own message: one Escape cancels it. With the menu open by a click, Escape closes the menu first.
    fn escape_with_edit(&mut self) -> SandboxResult {
        let (own, english) = (self.ids().own.clone(), self.ids().english.clone());
        let edit = self.run.letter('e')?;
        self.select_by_keys(&own)?;
        self.run.input.key(edit)?;
        self.run
            .until("chat", "e opens the edit", |state| state["compose"] == "edit")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["compose"] == "new", || {
            "one Escape does not cancel the edit".to_owned()
        });
        self.let_selection_go()?;
        self.select_by_keys(&own)?;
        self.run.input.key(edit)?;
        self.run
            .until("chat", "e opens the edit", |state| state["compose"] == "edit")?;
        self.open_menu("chat", &english)?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let state = self.run.state("chat")?;
        expect(state["options"].is_null() && state["compose"] == "edit", || {
            format!(
                "Escape with an edit and the menu open: options {}, box {}",
                state["options"], state["compose"]
            )
        });
        self.shots("edit-kept")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let (state, at) = (self.run.state("chat")?, self.focus()?);
        expect(state["compose"] == "new" && at == "box", || {
            format!("the second Escape: box {}, focus {at}", state["compose"])
        });
        // let the message go and give the box its keys: the next steps find hover bars with the pointer
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        if !self.run.state("chat")?["selected"].is_null() {
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
        }
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        Ok(())
    }

    /// Lets the selected message go, with Escape in the message list, so that Up starts again from the newest message.
    fn let_selection_go(&mut self) -> SandboxResult {
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        for _ in 0..3 {
            if self.run.state("chat")?["selected"].is_null() {
                return Ok(());
            }
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
        }
        Err("Escape in the message list never lets the selected message go".into())
    }

    /// The menu opened by Shift+F10 works with the keys: Down past "Mark as unread" to "Translation", Right into its
    /// list, Enter on its offer, Left and Escape back out. b's message shows translated when this starts.
    fn menu_by_keys(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        self.select_by_keys(&hello)?;
        self.menu_key_on("chat", &hello)?;
        self.key_shots("keys-menu")?;
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::RIGHT)?;
        thread::sleep(SETTLE);
        self.key_shots("keys-translation-list")?;
        // Left closes the list alone, Right opens it again
        self.run.input.key(keys::LEFT)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["options"] == hello.as_str(), || {
            "Left in the Translation list closes the whole menu".to_owned()
        });
        self.run.input.key(keys::RIGHT)?;
        thread::sleep(SETTLE);
        self.run.input.key(keys::ENTER)?;
        self.until_shows("chat", &hello, HELLO.0, "none")?;
        let state = self.run.state("chat")?;
        expect(
            state["options"].is_null() && state["selected"] == hello.as_str(),
            || format!("Enter in the list leaves the menu open or the message unselected: {state}"),
        );
        // the keys are back on the message: Shift+F10 again, and Translate
        self.menu_key_on("chat", &hello)?;
        for key in [keys::DOWN, keys::DOWN, keys::RIGHT, keys::ENTER] {
            self.run.input.key(key)?;
            thread::sleep(Duration::from_millis(300));
        }
        self.until_shows("chat", &hello, HELLO.1, "translated")?;
        // Escape in the list closes the list, a second one the menu
        self.menu_key_on("chat", &hello)?;
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::RIGHT)?;
        thread::sleep(SETTLE);
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["options"] == hello.as_str(), || {
            "Escape in the Translation list closes the whole menu".to_owned()
        });
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        expect(self.run.state("chat")?["options"].is_null(), || {
            "a second Escape leaves the menu open".to_owned()
        });
        // a third lets the message go: the next steps find its hover bar with the pointer
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        step("the menu by the keys: Shift+F10, Down, Right, Left, Enter, Escape");
        Ok(())
    }

    /// Shift+F10 on the message `id` selected in `view`: its menu opens, and takes the keys once it is drawn.
    fn menu_key_on(&mut self, view: &str, id: &str) -> SandboxResult {
        self.run.input.key_with(&[Modifier::Shift], KEY_F10)?;
        self.run
            .until(view, "Shift+F10 opens the menu", |state| state["options"] == id)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Light and dark screenshots with the pointer where it is: a move of the pointer hides the keys' highlight.
    fn key_shots(&mut self, name: &str) -> SandboxResult {
        self.key_shot(&format!("qa575-{name}-light.png"))?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.key_shot(&format!("qa575-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn key_shot(&mut self, file: &str) -> SandboxResult {
        let png = self.sandbox.screenshot(file, false)?;
        fs::copy(&png, shots_dir()?.join(file))?;
        Ok(())
    }

    /// In `view` (a channel's posts or a thread), the message `id` selected, as Escape and Up select it there: Shift+F10
    /// opens its menu and Escape closes it.
    fn menu_key_in(&mut self, view: &str, id: &str) -> SandboxResult {
        self.run.question(&format!("select {view} {id}"))?;
        thread::sleep(SETTLE);
        self.menu_key_on(view, id)?;
        self.key_shots(&format!("keys-menu-{view}"))?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        expect(self.run.state(view)?["options"].is_null(), || {
            format!("Escape leaves the menu in {view} open")
        });
        step(&format!("Shift+F10 in {view}"));
        Ok(())
    }

    /// b's message already in English: marked as translated, its text as it is, "See original message (English)".
    fn already_in_english(&mut self) -> SandboxResult {
        let english = self.ids().english.clone();
        self.choose_offer("chat", &english, false)?;
        let row = self.until_shows("chat", &english, ENGLISH, "translated")?;
        expect(row["offer"] == "See original message (English)", || {
            format!("a message already in English offers {}", row["offer"])
        });
        self.shots("english")?;
        step("a message already in English");
        Ok(())
    }

    /// "Settings" in the list opens Settings > General.
    fn settings_row(&mut self) -> SandboxResult {
        let hello = self.ids().hello.clone();
        let list = self.open_list("chat", &hello, false)?;
        self.run.click(list.settings.0, list.settings.1)?;
        let state = self.run.until("chat", "Settings > General", |state| {
            state["main"] == "settings:General"
        })?;
        step(&format!("Settings opened {}", state["main"]));
        self.shots("settings")?;
        self.open_chat(AB_CHAT, &hello)?;
        Ok(())
    }

    /// The tool signed out: the red mark and its tip; a click on it tries again (and fails again), then translates once
    /// the tool works.
    fn tool_fails(&mut self) -> SandboxResult {
        let four = self.ids().four.clone();
        self.tool.fail(true)?;
        let before = self.tool.questions().len();
        self.choose_offer("chat", &four, false)?;
        let row = self.until_mark("chat", &four, "stopped")?;
        let why = row["mark"]["tip"].as_str().unwrap_or_default().to_owned();
        expect(why.contains("signed in"), || format!("the red mark says {why:?}"));
        expect(row["text"] == FOUR.0, || {
            format!("a failed message shows {}", row["text"])
        });
        let state = self.run.state("chat")?;
        expect(state["notice"].is_null(), || {
            format!("one failed message shows the chat's notice: {}", state["notice"])
        });
        self.shots("failed")?;
        let mark = self.red_mark(&four)?;
        self.run.input.move_to(mark.0, mark.1)?;
        thread::sleep(Duration::from_millis(1500));
        let tip = self.sandbox.screenshot("qa575-failed-tip-light.png", false)?;
        fs::copy(&tip, shots_dir()?.join("qa575-failed-tip-light.png"))?;
        let tried = self.tool.questions().len();
        expect(tried > before, || {
            "the failed Translate never asked the tool".to_owned()
        });
        // a click on the red mark tries again: the tool asked once more, the mark red again
        self.run.click(mark.0, mark.1)?;
        wait_until(WAIT, || (self.tool.questions().len() > tried).then_some(()))
            .ok_or("a click on the red mark did not ask the tool again")?;
        self.until_mark("chat", &four, "stopped")?;
        self.tool.fail(false)?;
        let mark = self.red_mark(&four)?;
        self.run.click(mark.0, mark.1)?;
        self.until_shows("chat", &four, FOUR.1, "translated")?;
        self.shots("failed-then-translated")?;
        step("the tool failed: the red mark, try again, translated");
        Ok(())
    }

    /// Where the red mark of `id` is on the screen.
    fn red_mark(&mut self, id: &str) -> SandboxResult<(u32, u32)> {
        let row = self.row("chat", id)?;
        let shot = load(&self.run.shot(self.sandbox, "qa575-red.png")?)?;
        let region = Rect {
            x0: row.x0,
            y0: row.y0.saturating_sub(4),
            x1: row.x1,
            y1: row.y1 + 4,
        };
        pixels_box(&shot, region, |pixel| near(pixel, DANGER, 24))
            .map(|found| found.centre())
            .ok_or_else(|| format!("no red mark at the message {id}").into())
    }

    /// b's message translated, then b edits it: it shows the new words translated, not the old translation.
    fn edited_after(&mut self) -> SandboxResult {
        let lunch = self.ids().lunch.clone();
        self.choose_offer("chat", &lunch, false)?;
        self.until_shows("chat", &lunch, LUNCH.1, "translated")?;
        let before = self.tool.questions().len();
        self.service()?
            .edit_as_b(self.runtime, AB_CHAT, &lunch, &format!("<p>{}</p>", LUNCH_EDITED.0))?;
        let state = self.run.until("chat", "b's edit", |state| {
            item(state, &lunch).is_some_and(|row| row["edited"] == true)
        })?;
        step(&format!(
            "b's edit came: {}",
            item(&state, &lunch).cloned().unwrap_or_default()
        ));
        let shown = self.run.until("chat", "the edited message settles", |state| {
            item(state, &lunch).is_some_and(|row| row["mark"]["kind"] != "waiting")
        });
        let row = shown
            .ok()
            .and_then(|state| item(&state, &lunch).cloned())
            .unwrap_or_default();
        expect(row["text"] != LUNCH.1, || {
            "an edited message translated alone still shows its old translation".to_owned()
        });
        expect(
            row["text"] == LUNCH_EDITED.1 && row["mark"]["kind"] == "translated",
            || format!("the edited message shows {} marked {}", row["text"], row["mark"]),
        );
        expect(self.tool.questions().len() > before, || {
            "the edit was not translated anew".to_owned()
        });
        self.shots("edited")?;
        step("an edit after the translation");
        Ok(())
    }

    /// b's message translated, then b deletes it: "This message was deleted", no mark, nothing offered.
    fn deleted_after(&mut self) -> SandboxResult {
        let gone = self.ids().gone.clone();
        self.choose_offer("chat", &gone, false)?;
        self.until_shows("chat", &gone, GONE.1, "translated")?;
        self.service()?.delete_as_b(self.runtime, AB_CHAT, &gone)?;
        let state = self.run.until("chat", "b's delete", |state| {
            item(state, &gone).is_some_and(|row| row["kind"] == "deleted")
        })?;
        let row = item(&state, &gone).cloned().unwrap_or_default();
        expect(row["mark"]["kind"] == "none" && row["offer"].is_null(), || {
            format!(
                "a deleted message is marked {} and offers {}",
                row["mark"], row["offer"]
            )
        });
        self.shots("deleted")?;
        step("a delete after the translation");
        Ok(())
    }

    /// The whole chat read in English (as the header's picker sets it), then as written: the messages translated alone
    /// show translated again, the others as written.
    fn whole_chat_around(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.run.question("language en")?;
        self.until_shows("chat", &ids.weather, WEATHER.1, "translated")?;
        let state = self.run.state("chat")?;
        for (id, text) in [(&ids.hello, HELLO.1), (&ids.four, FOUR.1)] {
            let row = item(&state, id).cloned().unwrap_or_default();
            expect(row["text"] == text, || {
                format!("in the chat read in English, {id} shows {}", row["text"])
            });
        }
        self.shots("whole-chat")?;
        // the list switches the two forms of a chat read in a language
        self.choose_offer("chat", &ids.weather, false)?;
        self.until_shows("chat", &ids.weather, WEATHER.0, "original")?;
        expect(self.offer("chat", &ids.weather)? == "See translation", || {
            "a chat's message seen as written does not offer See translation".to_owned()
        });
        self.choose_offer("chat", &ids.weather, false)?;
        self.until_shows("chat", &ids.weather, WEATHER.1, "translated")?;
        self.run.question("language off")?;
        self.until_shows("chat", &ids.weather, WEATHER.0, "none")?;
        let state = self.run.until("chat", "the picked messages translated again", |state| {
            [(&ids.hello, HELLO.1), (&ids.four, FOUR.1), (&ids.own, OWN.1)]
                .iter()
                .all(|(id, text)| item(state, id).is_some_and(|row| row["text"] == *text))
        });
        if let Err(error) = state {
            note(format!("after the whole chat is read as written again: {error}"));
        }
        self.shots("whole-chat-off")?;
        step("the whole chat on and off");
        Ok(())
    }

    /// b's German message in the a–b–c chat.
    fn group_chat(&mut self) -> SandboxResult {
        let group = self.ids().group.clone();
        self.open_chat(ABC_CHAT, &group)?;
        self.choose_offer("chat", &group, false)?;
        self.until_shows("chat", &group, GROUP.1, "translated")?;
        self.shots("group")?;
        step("a group chat");
        Ok(())
    }

    /// b's post in the team's channel, and b's reply in a post's thread.
    fn channel_post_and_reply(&mut self) -> SandboxResult {
        let (post, reply) = (self.ids().post.clone(), self.ids().reply.clone());
        self.run.question("open-channel")?;
        self.run
            .until("posts", "b's post in the channel", |state| item(state, &post).is_some())?;
        thread::sleep(SETTLE);
        self.choose_offer("posts", &post, false)?;
        self.until_shows("posts", &post, POST.1, "translated")?;
        self.shots("channel-post")?;
        self.menu_key_in("posts", &post)?;
        self.run.question(&format!("open-thread {CHANNEL_POST}"))?;
        self.run.until("thread", "b's reply in the thread", |state| {
            item(state, &reply).is_some()
        })?;
        thread::sleep(SETTLE);
        self.choose_offer("thread", &reply, false)?;
        self.until_shows("thread", &reply, REPLY.1, "translated")?;
        self.shots("thread-reply")?;
        self.menu_key_in("thread", &reply)?;
        step("a channel's post and a reply");
        Ok(())
    }

    /// After a restart: what was translated alone shows translated at once, from the store.
    fn after_restart(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        let asked = self.tool.questions().len();
        self.open_chat(AB_CHAT, &ids.hello)?;
        let state = self.run.until("chat", "the translations after the restart", |state| {
            [
                (&ids.hello, HELLO.1),
                (&ids.own, OWN.1),
                (&ids.four, FOUR.1),
                (&ids.lunch, LUNCH_EDITED.1),
                (&ids.english, ENGLISH),
            ]
            .iter()
            .all(|(id, text)| {
                item(state, id).is_some_and(|row| row["text"] == *text && row["mark"]["kind"] == "translated")
            })
        });
        if let Err(error) = state {
            note(format!("after a restart: {error}"));
        }
        let state = self.run.state("chat")?;
        let weather = item(&state, &ids.weather).cloned().unwrap_or_default();
        expect(
            weather["text"] == WEATHER.0 && weather["mark"]["kind"] == "none",
            || {
                format!(
                    "a message never translated alone shows {} after a restart",
                    weather["text"]
                )
            },
        );
        self.shots("restart")?;
        self.open_chat(ABC_CHAT, &ids.group)?;
        if let Err(error) = self.until_shows("chat", &ids.group, GROUP.1, "translated") {
            note(format!("the group chat after a restart: {error}"));
        }
        self.run.question("open-channel")?;
        if let Err(error) = self.until_shows("posts", &ids.post, POST.1, "translated") {
            note(format!("the channel's post after a restart: {error}"));
        }
        self.run.question(&format!("open-thread {CHANNEL_POST}"))?;
        if let Err(error) = self.until_shows("thread", &ids.reply, REPLY.1, "translated") {
            note(format!("the reply after a restart: {error}"));
        }
        thread::sleep(QUIET);
        let now = self.tool.questions().len();
        expect(now == asked, || {
            format!(
                "a restart asked the tool {} times for what the store keeps",
                now - asked
            )
        });
        step("a restart");
        Ok(())
    }

    /// No tool on this computer: what the store keeps still shows; Translate on a new message says why it can't.
    fn no_tool(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.hello)?;
        if let Err(error) = self.until_shows("chat", &ids.hello, HELLO.1, "translated") {
            note(format!("without the tool, a kept translation: {error}"));
        }
        let state = self.run.until("chat", "b's newest message", |state| {
            state["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|row| row["text"] == BUS.0))
        })?;
        let bus = state["items"]
            .as_array()
            .and_then(|items| items.iter().find(|row| row["text"] == BUS.0))
            .and_then(|row| row["id"].as_str())
            .ok_or("no id of b's newest message")?
            .to_owned();
        self.choose_offer("chat", &bus, false)?;
        let row = self.until_mark("chat", &bus, "stopped")?;
        let why = row["mark"]["tip"].as_str().unwrap_or_default().to_owned();
        step(&format!("without a tool the mark says {why:?}"));
        expect(
            why.to_lowercase().contains("claude") || why.to_lowercase().contains("tool"),
            || format!("without a tool the red mark says {why:?}"),
        );
        self.shots("no-tool")?;
        let mark = self.red_mark(&bus)?;
        self.run.input.move_to(mark.0, mark.1)?;
        thread::sleep(Duration::from_millis(1500));
        let tip = self.sandbox.screenshot("qa575-no-tool-tip-light.png", false)?;
        fs::copy(&tip, shots_dir()?.join("qa575-no-tool-tip-light.png"))?;
        step("no tool");
        Ok(())
    }
}

/// The JSON row of the message `id` in a view's state.
fn item<'v>(state: &'v Value, id: &str) -> Option<&'v Value> {
    state["items"].as_array()?.iter().find(|row| row["id"] == id)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a place on the screen: small, and clamped at 0"
)]
fn rect_of(value: &Value) -> Option<Rect> {
    let list = value.as_array()?;
    let number = |at: usize| list.get(at).and_then(Value::as_f64).map(|n| n.max(0.0).round() as u32);
    let (x, y, w, h) = (number(0)?, number(1)?, number(2)?, number(3)?);
    Some(Rect {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    })
}

// --- the own server ---

const OWN_UPN: &str = "dana@own.example.test";
const OWN_B: &str = "erin@own.example.test";

/// An own server's account on the real binary: Erin writes in German, Dana translates it from its menu.
fn own_server(sandbox: &Sandbox, tool: &FakeTool, dir: &Path) -> SandboxResult {
    step("--- run own ---");
    let Some(binary) = own_server_binary::binary()? else {
        say("skipped own-server checks: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok(());
    };
    let server = own_server_binary::BinaryServer::start(&binary)?;
    let dana = server.add_user(OWN_UPN, "Dana Made-Up")?;
    server.add_user(OWN_B, "Erin Made-Up")?;
    let origin = server.origin.clone();
    let ca = server.ca_file.to_string_lossy().into_owned();
    let erin_dir = tempfile::tempdir()?;
    let (_erin, engine) = own_account_in(erin_dir.path(), (&origin, &ca), OWN_B)?;
    let runtime = ngteams::accounts::runtime()?;
    let made = runtime.block_on(engine.handle.write(Action::CreateChat {
        others: vec![Mri::new(format!("8:orgid:{dana}"))],
        topic: None,
    }))?;
    let chat = made.message_id.ok_or("Erin's chat with Dana was not made")?;
    runtime.block_on(engine.handle.write(Action::Send {
        conversation: chat.clone(),
        text: MessageText::plain(OWN_SERVER.0)?,
    }))?;
    let env = vec![(ORIGIN_ENV, origin), (CA_ENV, ca)];
    let mut run = AppRun::launch(sandbox, &env, dir, tool, "own")?;
    let outcome = (|| -> SandboxResult {
        run.question(&format!("open {chat}"))?;
        let state = run.until("chat", "Erin's message", |state| {
            state["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|row| row["text"] == OWN_SERVER.0))
        })?;
        let id = state["items"]
            .as_array()
            .and_then(|items| items.iter().find(|row| row["text"] == OWN_SERVER.0))
            .and_then(|row| row["id"].as_str())
            .ok_or("no id of Erin's message")?
            .to_owned();
        thread::sleep(SETTLE);
        let mut walk = Walk {
            run: &mut run,
            sandbox,
            runtime: &runtime,
            service: None,
            tool,
        };
        walk.choose_offer("chat", &id, false)?;
        walk.until_shows("chat", &id, OWN_SERVER.1, "translated")?;
        walk.shots("own-server")?;
        walk.choose_offer("chat", &id, false)?;
        walk.until_shows("chat", &id, OWN_SERVER.0, "none")?;
        step("an own server's account: translated and as written again");
        Ok(())
    })();
    if let Err(error) = outcome {
        let _ = fs::write(dir.join("own-server.log"), server.log());
        return Err(run.fail(sandbox, &*error));
    }
    run.quit()
}

/// An own server's account signed in with its password and started as the app starts it, with no calls.
fn own_account_in(
    dir: &Path,
    (origin, ca_file): (&str, &str),
    upn: &str,
) -> SandboxResult<(
    ngteams::accounts_file::PersonalAccount,
    AccountEngine<MemorySecretStore>,
)> {
    let paths = ngteams::accounts_file::PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = ngteams::setup::Setup::new(paths)?;
    let server = ngteams::accounts_file::OwnServer::parse(origin, ca_file)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        upn,
        &server,
        &Secret::new(own_server_binary::PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, None)?;
    Ok((account, engine))
}

// --- the screen ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    const fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    const fn centre(&self) -> (u32, u32) {
        (self.x0 + (self.x1 - self.x0) / 2, self.y0 + (self.y1 - self.y0) / 2)
    }
}

fn load(shot: &Path) -> SandboxResult<image::RgbImage> {
    Ok(image::open(shot)?.to_rgb8())
}

fn near(pixel: [u8; 3], wanted: [u8; 3], within: u8) -> bool {
    pixel.iter().zip(wanted).all(|(got, want)| got.abs_diff(want) <= within)
}

fn at(shot: &image::RgbImage, x: u32, y: u32) -> [u8; 3] {
    if x < shot.width() && y < shot.height() {
        shot.get_pixel(x, y).0
    } else {
        [0, 0, 0]
    }
}

/// The first `y` in `rows` of column `x` that is a panel's top border: [`BORDER`] over two rows of [`SURFACE`].
fn border_top(shot: &image::RgbImage, x: u32, rows: std::ops::Range<u32>) -> Option<u32> {
    rows.into_iter()
        .find(|&y| near(at(shot, x, y), BORDER, 4) && at(shot, x, y + 1) == SURFACE && at(shot, x, y + 2) == SURFACE)
}

/// The first `y` in `rows` of column `x` that is a panel's outer top edge: [`border_top`], not under [`SURFACE`].
fn panel_top(shot: &image::RgbImage, x: u32, rows: std::ops::Range<u32>) -> Option<u32> {
    rows.into_iter()
        .find(|&y| border_top(shot, x, y..y + 1).is_some() && at(shot, x, y.saturating_sub(1)) != SURFACE)
}

/// The first `y` in `rows` of column `x` that is a line inside a panel: [`BORDER`] with [`SURFACE`] above and under.
fn separator_under(shot: &image::RgbImage, x: u32, rows: std::ops::Range<u32>) -> Option<u32> {
    rows.into_iter()
        .find(|&y| near(at(shot, x, y), BORDER, 4) && at(shot, x, y - 1) == SURFACE && at(shot, x, y + 1) == SURFACE)
}

/// The bounds of the pixels in `region` that `taken` takes.
fn pixels_box(shot: &image::RgbImage, region: Rect, taken: impl Fn([u8; 3]) -> bool) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in region.y0..region.y1.min(shot.height()) {
        for x in region.x0..region.x1.min(shot.width()) {
            if taken(at(shot, x, y)) {
                bounds = Some(grow(bounds, x, y));
            }
        }
    }
    bounds
}

fn grow(bounds: Option<Rect>, x: u32, y: u32) -> Rect {
    bounds.map_or(
        Rect {
            x0: x,
            y0: y,
            x1: x + 1,
            y1: y + 1,
        },
        |known| Rect {
            x0: known.x0.min(x),
            y0: known.y0.min(y),
            x1: known.x1.max(x + 1),
            y1: known.y1.max(y + 1),
        },
    )
}

/// The bounds of the pixels that changed in `region`.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in region.y0..region.y1.min(before.height()) {
        for x in region.x0..region.x1.min(before.width()) {
            if !near(at(before, x, y), at(after, x, y), 8) {
                bounds = Some(grow(bounds, x, y));
            }
        }
    }
    bounds
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

// --- the service ---

/// The made-up messages' ids.
#[derive(Clone, Debug, Default)]
struct Ids {
    hello: String,
    english: String,
    own: String,
    lunch: String,
    gone: String,
    four: String,
    weather: String,
    group: String,
    post: String,
    reply: String,
}

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    ids: Ids,
}

impl Service {
    /// The mock with the made-up messages: b's and a's in the a–b chat, b's in the a–b–c chat, b's post in the team's
    /// channel and b's reply under its post.
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let post = |from: &str, chat: &str, text: &str| -> SandboxResult<String> {
            mock.post_as(from, chat, &format!("<p>{text}</p>"))
                .ok_or_else(|| format!("the post {text:?} refused").into())
        };
        let ids = Ids {
            hello: post(LAB_B, AB_CHAT, HELLO.0)?,
            english: post(LAB_B, AB_CHAT, ENGLISH)?,
            own: post(LAB_A, AB_CHAT, OWN.0)?,
            lunch: post(LAB_B, AB_CHAT, LUNCH.0)?,
            gone: post(LAB_B, AB_CHAT, GONE.0)?,
            four: post(LAB_B, AB_CHAT, FOUR.0)?,
            weather: post(LAB_B, AB_CHAT, WEATHER.0)?,
            group: post(LAB_B, ABC_CHAT, GROUP.0)?,
            post: mock
                .add_message(
                    TEAM_CHANNEL,
                    (LAB_B, "Lab B"),
                    "RichText/Html",
                    &format!("<p>{}</p>", POST.0),
                )
                .ok_or("b's channel post refused")?,
            reply: mock
                .add_message(
                    &format!("{TEAM_CHANNEL};messageid={CHANNEL_POST}"),
                    (LAB_B, "Lab B"),
                    "RichText/Html",
                    &format!("<p>{}</p>", REPLY.0),
                )
                .ok_or("b's reply refused")?,
        };
        let _ = mock.take_events(LAB_A);
        Ok(Self { mock, trouter, ids })
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_ENV, self.mock.token(LAB_A).unwrap_or_default()),
        ]
    }

    /// b's write to the mock's chat service, as b's client sends it; its status.
    fn as_b(
        &self,
        runtime: &tokio::runtime::Runtime,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> SandboxResult {
        let url = format!(
            "{}/api/chatsvc/amer/v1/users/ME/conversations/{path}",
            self.mock.origin()
        );
        let token = self.mock.token(LAB_B).ok_or("no token for b")?;
        let status = runtime.block_on(async {
            let request = reqwest::Client::new()
                .request(method, url)
                .bearer_auth(token)
                .header("X-MS-Migration", "True")
                .header("BehaviorOverride", "redirectAs404");
            let request = match body {
                Some(body) => request
                    .header("content-type", "application/json")
                    .body(body.to_string()),
                None => request,
            };
            request.send().await.map(|answer| answer.status().as_u16())
        })?;
        if !(200..300).contains(&status) {
            return Err(format!("b's write to {path} answered {status}").into());
        }
        Ok(())
    }

    fn edit_as_b(&self, runtime: &tokio::runtime::Runtime, chat: &str, id: &str, html: &str) -> SandboxResult {
        let body = json!({"messagetype": "RichText/Html", "contenttype": "Text", "content": html, "properties": {}});
        self.as_b(
            runtime,
            reqwest::Method::PUT,
            &format!("{chat}/messages/{id}"),
            Some(body),
        )
    }

    fn delete_as_b(&self, runtime: &tokio::runtime::Runtime, chat: &str, id: &str) -> SandboxResult {
        self.as_b(
            runtime,
            reqwest::Method::DELETE,
            &format!("{chat}/messages/{id}?behavior=softDelete"),
            None,
        )
    }
}

// --- the made-up tool ---

/// The made-up tool as the parent sees it: where it is, what it was asked, and the files that steer it.
struct FakeTool {
    bin: PathBuf,
    state: PathBuf,
    home: PathBuf,
    /// The child's `PATH`: `bin`, then the sandbox's own, without any directory holding a real `claude` or `codex`.
    path: String,
}

impl FakeTool {
    fn install(sandbox: &Sandbox) -> SandboxResult<Self> {
        let (bin, state, home) = (
            sandbox.path("ai-bin"),
            sandbox.path("ai-state"),
            sandbox.path("ai-home"),
        );
        for dir in [&bin, &state, &home] {
            fs::create_dir_all(dir)?;
        }
        let inherited = sandbox
            .client_env()
            .into_iter()
            .find(|(name, _)| *name == "PATH")
            .map(|(_, path)| path)
            .unwrap_or_default();
        let kept: Vec<PathBuf> = std::env::split_paths(&inherited)
            .filter(|dir| !["claude", "codex"].iter().any(|tool| dir.join(tool).exists()))
            .collect();
        let commands = sandbox.path("ai-commands");
        fs::create_dir_all(&commands)?;
        for command in ["sed", "tee", "sleep"] {
            let found = std::env::split_paths(&inherited)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| format!("no {command} on the PATH"))?;
            std::os::unix::fs::symlink(found, commands.join(command))?;
        }
        let path = std::env::join_paths([bin.clone(), commands].into_iter().chain(kept))?
            .into_string()
            .map_err(|_| "a PATH that isn't UTF-8")?;
        let dictionary: String = DICTIONARY
            .iter()
            .map(|(german, english)| ["s/", german, "/", english, "/g\n"].concat())
            .collect();
        fs::write(state.join("dictionary.sed"), dictionary)?;
        let tool = Self { bin, state, home, path };
        tool.reinstall()?;
        Ok(tool)
    }

    /// Puts `claude` (back) on the child's `PATH`.
    fn reinstall(&self) -> SandboxResult {
        use std::os::unix::fs::PermissionsExt as _;
        let program = self.bin.join("claude");
        let script = FAKE_CLAUDE.replace("__STATE__", &self.state.display().to_string());
        fs::write(&program, script)?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    /// Takes `claude` off the child's `PATH`.
    fn uninstall(&self) -> SandboxResult {
        fs::remove_file(self.bin.join("claude"))?;
        Ok(())
    }

    fn switch(&self, name: &str, on: bool) -> SandboxResult {
        let file = self.state.join(name);
        if on {
            fs::write(file, "")?;
        } else if file.exists() {
            fs::remove_file(file)?;
        }
        Ok(())
    }

    fn hold(&self, on: bool) -> SandboxResult {
        self.switch("hold", on)
    }

    fn fail(&self, on: bool) -> SandboxResult {
        self.switch("fail", on)
    }

    /// Every batch the tool was asked to translate, as its stdin's lines.
    fn questions(&self) -> Vec<String> {
        fs::read_to_string(self.state.join("received.log"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("<translate>"))
            .map(str::to_owned)
            .collect()
    }
}

// --- the app in the sandbox ---

/// The app running in the sandbox: its process, its directory and the user's pointer and keys.
struct AppRun {
    running: Running,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar.
    rest: (u32, u32),
    asked: u32,
}

impl AppRun {
    /// Starts the app with `env` and its store and settings in `dir`, in `mode`; once its window is drawn.
    fn launch(
        sandbox: &Sandbox,
        env: &[(&'static str, String)],
        dir: &Path,
        tool: &FakeTool,
        mode: &str,
    ) -> SandboxResult<Self> {
        let dir_text = dir.to_string_lossy().into_owned();
        let bin = tool.bin.to_string_lossy().into_owned();
        let home = tool.home.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![
            (CHILD_ENV, "app"),
            (DIR_ENV, &dir_text),
            (MODE_ENV, mode),
            (TOOLS_ENV, &bin),
            ("PATH", &tool.path),
            ("CLAUDE_CONFIG_DIR", &home),
            ("CODEX_HOME", &home),
        ];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        let child = sandbox.spawn(&std::env::current_exe()?, &[], &vars)?;
        let running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
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

    /// The state of `view` (`chat`, `posts` or `thread`).
    fn state(&mut self, view: &str) -> SandboxResult<Value> {
        let answer = self.question(&format!("state {view}"))?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state of `view` that `wanted` takes, named `what` in the error.
    fn until(&mut self, view: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state(view)?;
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

    /// A screenshot of the screen, the pointer at rest, kept as `target/ui-shots/<name>`; the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        fs::copy(&png, shots_dir()?.join(name))?;
        Ok(png)
    }

    /// Clicks (`x`, `y`) as a user does.
    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// The key of letter `letter`.
    fn letter(&self, letter: char) -> SandboxResult<u32> {
        self.input
            .keymap()
            .key_for(letter)
            .map(|(key, _)| key)
            .ok_or_else(|| format!("no key for {letter}").into())
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa575-failed-run.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits the app; once it ended.
    fn quit(mut self) -> SandboxResult {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} quit", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

// --- the child ---

/// One account's mock token, for the transport and the push channel alike.
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

/// a's engine on the parent's mock, its store in `dir`.
fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?);
    let trouter = Trouter::with_mock_origin(
        Arc::clone(&transport),
        Arc::clone(&tokens),
        &std::env::var(TROUTER_ENV)?,
    )?
    .with_options(Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(50),
        backoff_jitter_percent: 0,
        ..Options::default()
    });
    let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(
            http()?
                .with_ams(AMS_BASE)?
                .with_presence(PRESENCE_BASE)?
                .with_csa(CSA_BASE)?
                .with_user_profile(USER_PROFILE_BASE)?
                .with_provisioning(PROVISIONING_BASE)?
                .with_middle_tier(MIDDLE_TIER_BASE)?,
        )),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: vec![
            (LAB_B.to_owned(), "Lab B".to_owned()),
            (LAB_C.to_owned(), "Lab C".to_owned()),
        ],
    })?)
}

/// The own server's account, signed in with its password: the app's first account, the runner, and the runtime that
/// signed it in (kept while the app runs).
type OwnStart = (
    ngteams::accounts::Started<MemorySecretStore>,
    ngteams::accounts::Runner,
    tokio::runtime::Runtime,
);

fn own_account(dir: &Path) -> SandboxResult<OwnStart> {
    let paths = ngteams::accounts_file::PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = ngteams::setup::Setup::new(paths)?;
    let server = ngteams::accounts_file::OwnServer::parse(&std::env::var(ORIGIN_ENV)?, &std::env::var(CA_ENV)?)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        OWN_UPN,
        &server,
        &Secret::new(own_server_binary::PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, None)?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    Ok((first, support::runner(dir)?, runtime))
}

/// The child's `PATH` holds no tool but the made-up one.
fn only_the_made_up_tool() -> SandboxResult {
    let made_up = PathBuf::from(std::env::var_os(TOOLS_ENV).ok_or("no made-up tool's directory")?);
    let found = teams_ui::local_ai::Tools::detect();
    for backend in [teams_ui::local_ai::Backend::Claude, teams_ui::local_ai::Backend::Codex] {
        if let Some(program) = found.program(backend)
            && !program.starts_with(&made_up)
        {
            return Err(format!("a real tool is on the PATH: {}", program.display()).into());
        }
    }
    Ok(())
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
    only_the_made_up_tool()?;
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let mode = std::env::var(MODE_ENV).unwrap_or_default();
    let (first, runner, runtime) = if mode == "own" {
        let (first, runner, runtime) = own_account(&dir)?;
        (first, runner, Some(runtime))
    } else {
        let engine = engine_on_the_mock(&dir)?;
        (support::started(Alias::A, engine)?, support::runner(&dir)?, None)
    };
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let theme_file = dir.join(THEME_FILE);
    let _ = fs::remove_file(&theme_file);
    let ask = dir.join(ASK);
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
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, theme_file.clone(), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, ask.clone(), cx);
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

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| cx.quit());
                return;
            }
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
                    Some(on_shell(&shell, verb, window, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

/// The chat view of `view`: the chat's, the channel's posts, or the open thread's.
fn view_of(shell: &Entity<Shell>, view: &str, cx: &App) -> Option<Entity<ChatView>> {
    let shell = shell.read(cx);
    match view {
        "chat" => shell.chat().filter(|_| matches!(shell.main(), Main::Chat(_))).cloned(),
        "posts" => shell.channel().map(|channel| channel.read(cx).posts().clone()),
        "thread" => shell
            .channel()
            .and_then(|channel| channel.read(cx).thread().map(|(_, thread)| thread.clone())),
        _ => None,
    }
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let (verb, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "state" => state_of(shell, rest, cx).to_string(),
        "open" => {
            shell.update(cx, |shell, cx| shell.open_chat(rest.to_owned(), window, cx));
            "ok".to_owned()
        }
        "open-channel" => {
            let list = shell.read(cx).list().clone();
            list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
            "ok".to_owned()
        }
        "open-thread" => {
            let Some(channel) = shell.read(cx).channel().cloned() else {
                return "no channel".to_owned();
            };
            channel.update(cx, |channel, cx| channel.open_thread(rest.to_owned(), window, cx));
            "ok".to_owned()
        }
        "reveal" => {
            let (view, id) = rest.split_once(' ').unwrap_or((rest, ""));
            let Some(chat) = view_of(shell, view, cx) else {
                return "no view".to_owned();
            };
            chat.update(cx, |chat, cx| {
                if let Some(index) = chat
                    .items()
                    .iter()
                    .position(|item| matches!(item, Item::Message(message) if message.id == id))
                {
                    chat.list_state().scroll_to_reveal_item(index);
                    cx.notify();
                }
            });
            "ok".to_owned()
        }
        // the message selected and the list's focus, as Escape in the box and Up give them
        "select" => {
            let (view, id) = rest.split_once(' ').unwrap_or((rest, ""));
            let Some(chat) = view_of(shell, view, cx) else {
                return "no view".to_owned();
            };
            chat.update(cx, |chat, cx| {
                chat.select(Some(id.to_owned()), cx);
                chat.focus_handle().clone().focus(window, cx);
            });
            "ok".to_owned()
        }
        // where the keys go: the message list, the compose box, another element or none
        "focus" => {
            let Some(chat) = view_of(shell, rest, cx) else {
                return "no view".to_owned();
            };
            let chat = chat.read(cx);
            if chat.focus_handle().is_focused(window) {
                "list".to_owned()
            } else if chat.compose().read(cx).focused(window, cx) {
                "box".to_owned()
            } else if window.focused(cx).is_some() {
                "other".to_owned()
            } else {
                "none".to_owned()
            }
        }
        // the whole chat read in a language, as the header's picker sets it
        "language" => {
            let Some(chat) = view_of(shell, "chat", cx) else {
                return "no chat".to_owned();
            };
            let language = match rest {
                "en" => Some("en"),
                "de" => Some("de"),
                _ => None,
            };
            let translation = chat.read(cx).translation().clone();
            translation.update(cx, |translation, cx| translation.set_language(language, cx));
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

/// The main view, and the rows of `view`: each message's words as shown, its mark and offer, and where it is drawn.
fn state_of(shell: &Entity<Shell>, view: &str, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(_) => "chat".to_owned(),
        Main::Channel(_) => "channel".to_owned(),
        Main::Settings => format!("settings:{:?}", shell_ref.settings_view().read(cx).page()),
        other => format!("{other:?}"),
    };
    let Some(chat) = view_of(shell, view, cx) else {
        return json!({"main": main, "items": []});
    };
    let chat = chat.read(cx);
    let list = chat.list_state();
    let pixels = |value: gpui::Pixels| f64::from(value);
    let viewport = list.viewport_bounds();
    let items: Vec<Value> = chat
        .items()
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let Item::Message(message) = item else {
                return None;
            };
            Some(row_json(message, list.bounds_for_item(index)))
        })
        .collect();
    // the chat's notice shows a failure only while the whole chat is read in a language
    let translation = chat.translation().read(cx);
    let notice = match translation.status() {
        teams_ui::chat_translate::Status::Failed(failure) if translation.shown().language.is_some() => {
            json!(failure.message())
        }
        _ => Value::Null,
    };
    json!({
        "main": main,
        "conversation": chat.conversation(),
        "selected": chat.selected(),
        "hovered": chat.hovered(),
        "options": chat.options(),
        "compose": match chat.compose().read(cx).mode() {
            Mode::New => "new",
            Mode::Reply(_) => "reply",
            Mode::Edit { .. } => "edit",
        },
        "notice": notice,
        "viewport": [
            pixels(viewport.origin.x),
            pixels(viewport.origin.y),
            pixels(viewport.size.width),
            pixels(viewport.size.height)
        ],
        "items": items,
    })
}

/// One message's row: its words as shown, its mark and offer, and where the list drew it.
fn row_json(message: &teams_ui::chat_view::MessageItem, bounds: Option<gpui::Bounds<gpui::Pixels>>) -> Value {
    let pixels = |value: gpui::Pixels| f64::from(value);
    let (kind, text) = match &message.body {
        Body::Text(rendered) => (
            "text",
            rendered
                .0
                .blocks
                .iter()
                .flat_map(Block::plain_lines)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Body::Deleted => ("deleted", String::new()),
        _ => ("other", String::new()),
    };
    let mark = match &message.translation {
        None => json!({"kind": "none"}),
        Some(Mark::Translated { tip }) => json!({"kind": "translated", "tip": tip.as_ref()}),
        Some(Mark::Original { tip }) => json!({"kind": "original", "tip": tip.as_ref()}),
        Some(Mark::Waiting) => json!({"kind": "waiting"}),
        Some(Mark::Failed) => json!({"kind": "failed"}),
        Some(Mark::Stopped { why }) => json!({"kind": "stopped", "tip": why.as_ref()}),
    };
    let rect = bounds.map(|bounds| {
        json!([
            pixels(bounds.origin.x),
            pixels(bounds.origin.y),
            pixels(bounds.size.width),
            pixels(bounds.size.height)
        ])
    });
    json!({
        "id": message.id,
        "own": message.own,
        "kind": kind,
        "text": text,
        "edited": message.edited,
        "mark": mark,
        "offer": message.translate.map(|offer| offer.label().to_string()),
        "rect": rect,
    })
}
