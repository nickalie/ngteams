//! The buttons of the message hover bar, chosen as a user chooses them in the app's window in the UI sandbox
//! (`harness = false`: GPUI runs on the process's main thread). It is the QA test of task 712.
//!
//! The parent runs the mock (`teams-mock`) with made-up messages, and runs the app as a child in the sandbox with its
//! store and settings in one directory, so a later run is a restart of the same app. A made-up `claude` first on the
//! app's `PATH` translates from a small dictionary. The parent moves the sandbox's pointer and presses its keys. It
//! finds the hover bar, its buttons, the menu and the list on the screen, and reads the views from the child.
//!
//! - **The list**: a right click on the bar opens "Show in the message bar" at the pointer; "Customize" at the foot of
//!   "More options" opens it too, by the pointer and by the keys. Up, Down, Enter, Escape, and a click on a row.
//! - **The bar**: the second quick reaction (Heart) and Reply hidden, Copy text and Mark as unread shown, later Delete
//!   and Translate too. Each button on b's and on a's own message works, and Edit is not on b's message. When the
//!   quick reactions change their order (the most used first), Heart stays hidden and no other reaction hides.
//! - **The menu**: the hidden buttons that apply first, a hairline, the rest, then "Customize"; each row works.
//! - **Other places**: a channel's post and a reply in its thread use the same choice.
//! - **A restart** keeps the choice; **another account** (b, on the same settings file) keeps Teams' bar; the rows
//!   clicked back give Teams' bar again, and the settings file keeps no choice.
//! - **The look**: Teams Light and Dark.
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa712-*.png`. `QA712_ONLY` (`walk`,
//! `restart`, `other`, `default`, a comma list) runs only those runs; `QA712_EVIDENCE` names a directory that gets a
//! copy of each screenshot. Without weston 15, niri or `dbus-daemon` it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, ClipboardItem, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, TEAM_CHANNEL, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_translate::Mark;
use teams_ui::chat_view::{Body, ChatView, Item};
use teams_ui::compose::Mode;
use teams_ui::rich_text::Block;
use teams_ui::settings::BarItem;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "QA712_CHILD";
const ORIGIN_ENV: &str = "QA712_ORIGIN";
const TROUTER_ENV: &str = "QA712_TROUTER";
const TOKEN_ENV: &str = "QA712_TOKEN";
const DIR_ENV: &str = "QA712_DIR";
/// `a` (a's account) or `b` (b's account, on a's settings file).
const ACCOUNT_ENV: &str = "QA712_ACCOUNT";
const ONLY_ENV: &str = "QA712_ONLY";
const EVIDENCE_ENV: &str = "QA712_EVIDENCE";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa712-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);

/// Teams Light's surface and its borders.
const SURFACE: [u8; 3] = [255, 255, 255];
const BORDER: [u8; 3] = [224, 224, 224];
/// The message box of a chat on the 1280×800 screen, a one-line box.
const COMPOSE: (u32, u32) = (700, 749);
const KEY_F10: u32 = 68;
/// The hover bar: its buttons' pitch (28 px and a 2 px gap), and its border and padding before the first button.
const PITCH: u32 = 30;
const BAR_INSET: u32 = 3;
/// The list's rows: under its 1 px border, 4 px padding and its 26 px title with a 2 px gap, 26 px apart by 2 px.
const LIST_FIRST: u32 = 1 + 4 + 26 + 2 + 13;
const LIST_PITCH: u32 = 28;
/// The list's rows in their order: the four quick reactions in their order now, React, Edit, Reply, Copy text,
/// Delete, Mark as unread, Translate, then "More options".
const ROW_DELETE: u32 = 8;
const ROW_TRANSLATE: u32 = 10;
/// The quick reaction that a hides: the second of Teams' own, at the second place when a hides it.
const HIDDEN: &str = "heart";
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
const OWN_GONE: &str = "Made-up own line that I delete from the menu.";
const OWN_BAR_GONE: &str = "Made-up own line that I delete from the bar.";
const FOUR: (&str, &str) = ("Wir sehen uns um vier.", "See you at four.");
const WEATHER: (&str, &str) = ("Das Wetter ist heute gut.", "The weather is good today.");
const POST: (&str, &str) = (
    "Die neue Version ist fertig zum Testen.",
    "The new version is ready for testing.",
);
const REPLY: (&str, &str) = ("Ich schaue es mir morgen an.", "I will look at it tomorrow.");

/// Every pair the made-up tool knows.
const DICTIONARY: [(&str, &str); 7] = [HELLO, OWN, LUNCH, FOUR, WEATHER, POST, REPLY];

/// A made-up Claude Code translating a chat's batches (`__STATE__` its state directory): each message in English from
/// `dictionary.sed`, named German when the dictionary changed it and English when not.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
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
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$body"
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
            eprintln!("qa_bar_customize_window: {error}");
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

fn wanted(run: &str) -> bool {
    let only = std::env::var(ONLY_ENV).unwrap_or_default();
    only.is_empty() || only.split(',').any(|name| name.trim() == run)
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
    let Some(sandbox) = sandbox_or_skip("ngteams hover bar customize", &options)? else {
        return Ok(());
    };
    let tool = FakeTool::install(&sandbox)?;
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    if let Err(error) = runs(&sandbox, &service, &tool, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams hover bar customize: ok");
    Ok(())
}

/// The runs on one store and one settings file: a's walk, a's restart, b's start, a's way back to Teams' bar.
fn runs(sandbox: &Sandbox, service: &Service, tool: &FakeTool, dir: &Path) -> SandboxResult {
    for (name, account) in [("walk", "a"), ("restart", "a"), ("other", "b"), ("default", "a")] {
        if !wanted(name) {
            continue;
        }
        step(&format!("--- run {name} ---"));
        let mut run = AppRun::launch(sandbox, &service.env(account), dir, tool, account)?;
        let outcome = {
            let mut walk = Walk::new(&mut run, sandbox, service, dir);
            match name {
                "walk" => walk.walk(),
                "restart" => walk.after_restart(),
                "other" => walk.other_account(),
                _ => walk.back_to_default(),
            }
        };
        if let Err(error) = outcome {
            return Err(run.fail(sandbox, &*error));
        }
        run.quit()?;
    }
    Ok(())
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    service: &'a Service,
    dir: &'a Path,
}

/// Where the parts of a screen are.
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

/// A hover bar as the screen shows it: its border's left and right columns, and the middle of its buttons.
#[derive(Clone, Copy, Debug)]
struct Bar {
    left: u32,
    right: u32,
    middle: u32,
}

impl Bar {
    /// How many buttons it holds, "More options" too.
    const fn buttons(&self) -> u32 {
        (self.right + 1 - self.left - 2 * BAR_INSET + 2 + PITCH / 2) / PITCH
    }

    /// The centre of its button `index` (0 is the first).
    const fn button(&self, index: u32) -> (u32, u32) {
        (self.left + BAR_INSET + index * PITCH + 14, self.middle)
    }
}

/// An open "More options" menu on the screen: the column its rows are read in, its top and bottom edges, and the
/// hairlines between its groups.
#[derive(Clone, Debug)]
struct Menu {
    column: u32,
    top: u32,
    bottom: u32,
    lines: Vec<u32>,
}

impl Menu {
    /// The number of rows in each group, from the top.
    fn groups(&self) -> Vec<u32> {
        let mut starts = vec![self.top + 5];
        starts.extend(self.lines.iter().map(|line| line + 7));
        let mut ends: Vec<u32> = self.lines.iter().map(|line| line - 6).collect();
        ends.push(self.bottom - 5);
        starts
            .iter()
            .zip(ends)
            .map(|(start, end)| (end.saturating_sub(*start) + 2 + 17) / 34)
            .collect()
    }

    /// The centre of the row `row` of the group `group`.
    fn row(&self, group: usize, row: u32) -> (u32, u32) {
        let first = match group {
            0 => self.top + 21,
            _ => self.lines[group - 1] + 23,
        };
        (self.column + 40, first + 34 * row)
    }
}

impl<'a> Walk<'a> {
    fn new(run: &'a mut AppRun, sandbox: &'a Sandbox, service: &'a Service, dir: &'a Path) -> Self {
        Self {
            run,
            sandbox,
            service,
            dir,
        }
    }
}

// --- the walk's tools ---

impl Walk<'_> {
    fn ids(&self) -> &Ids {
        &self.service.ids
    }

    /// The shots of `name` in Teams Light, then Teams Dark, then back to Light; the pointer where it is.
    fn shots(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let light = self.key_shot(&format!("qa712-{name}-light.png"))?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.key_shot(&format!("qa712-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(light)
    }

    fn key_shot(&mut self, file: &str) -> SandboxResult<PathBuf> {
        let png = self.sandbox.screenshot(file, false)?;
        keep(&png, file)?;
        Ok(png)
    }

    /// Opens `chat` as a click on its row does, once its message `id` shows.
    fn open_chat(&mut self, chat: &str, id: &str) -> SandboxResult {
        self.run.question(&format!("open {chat}"))?;
        self.run.until("chat", &format!("the chat {chat} with {id}"), |state| {
            state["conversation"] == chat && item(state, id).is_some()
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
            let viewport = rect_of(&state["viewport"]).ok_or("no viewport")?;
            let rect = rect_of(&state["items"][index]["rect"]);
            if let Some(rect) = rect
                && rect.y0 >= viewport.y0 + 44
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

    /// Points at the message `id` (no message selected, no menu open) and finds its hover bar on the screen (Teams
    /// Light): the bar's border, read across the middle of its buttons.
    fn bar(&mut self, view: &str, id: &str) -> SandboxResult<Bar> {
        let mut last = String::new();
        for _ in 0..3 {
            match self.try_bar(view, id) {
                Ok(bar) => return Ok(bar),
                Err(error) => last = error.to_string(),
            }
            self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
            thread::sleep(SETTLE);
        }
        Err(last.into())
    }

    fn try_bar(&mut self, view: &str, id: &str) -> SandboxResult<Bar> {
        let row = self.row(view, id)?;
        let still = load(&self.run.shot(self.sandbox, "qa712-still.png")?)?;
        // in the row's left margin: never on the bar, never on a face (its profile card would open)
        self.run
            .input
            .move_to(row.x0 + 6, row.y0 + (row.height() / 2).min(14))?;
        thread::sleep(SETTLE);
        let hover = load(&self.sandbox.screenshot("qa712-hover.png", false)?)?;
        let region = Rect {
            x0: row.x0,
            y0: row.y0.saturating_sub(48),
            x1: row.x1,
            y1: row.y0 + 30,
        };
        let changed = changed_box(&still, &hover, region)
            .ok_or_else(|| format!("no hover bar over the message {id} (its row {row:?})"))?;
        let middle = changed.y0 + 17;
        let left = (changed.x0..changed.x1).find(|&x| near(at(&hover, x, middle), BORDER, 6));
        let right = (changed.x0..changed.x1)
            .rev()
            .find(|&x| near(at(&hover, x, middle), BORDER, 6));
        match (left, right) {
            (Some(left), Some(right)) if right > left + 40 => Ok(Bar { left, right, middle }),
            _ => Err(format!("no border of the hover bar over {id} at y {middle} in {changed:?}").into()),
        }
    }

    /// Points at `id` and clicks its bar's button `index`; the bar.
    fn click_bar(&mut self, view: &str, id: &str, index: u32) -> SandboxResult<Bar> {
        let bar = self.bar(view, id)?;
        let (x, y) = bar.button(index);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        Ok(bar)
    }

    /// Lets the selected message go and closes what is open, so the pointer alone shows a bar.
    fn calm(&mut self, view: &str) -> SandboxResult {
        self.run.question(&format!("unselect {view}"))?;
        self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

// --- the menu and the list ---

impl Walk<'_> {
    /// Opens the More options menu of `id` with a click on its bar's last button; the menu as the screen shows it.
    fn open_menu(&mut self, view: &str, id: &str) -> SandboxResult<Menu> {
        let mut last = String::new();
        for _ in 0..2 {
            let bar = self.bar(view, id)?;
            let before = load(&self.sandbox.screenshot("qa712-before-menu.png", false)?)?;
            let (x, y) = bar.button(bar.buttons() - 1);
            self.run.click(x, y)?;
            thread::sleep(SETTLE);
            let state = self.run.state(view)?;
            if state["options"] == id {
                return self.read_menu(&before, (x, y));
            }
            last = format!("the More options menu of {id} never opened: {}", state["options"]);
            self.close_menu(view)?;
        }
        Err(last.into())
    }

    /// The open menu, its button at `button`: its top edge under the button (or over it, with no room under), its
    /// bottom edge and its hairlines.
    fn read_menu(&mut self, before: &image::RgbImage, button: (u32, u32)) -> SandboxResult<Menu> {
        let png = self.run.shot(self.sandbox, "qa712-menu.png")?;
        let menu = load(&png)?;
        // the menu is 200 px wide, its right edge the button's: the column is in its rows' left padding
        let column = (button.0 + 14).saturating_sub(200) + 8;
        let (x, y) = (column, button.1);
        let over = Rect {
            x0: x.saturating_sub(2),
            y0: y.saturating_sub(420).max(60),
            x1: x + 2,
            y1: menu.height(),
        };
        let top = panel_top(&menu, x, y + 12..menu.height())
            .or_else(|| panel_top(&menu, x, y.saturating_sub(420).max(60)..y))
            .or_else(|| changed_box(before, &menu, over).map(|changed| changed.y0))
            .ok_or("no menu's top edge")?;
        let bottom = panel_bottom(&menu, x, top + 30..menu.height()).ok_or("no menu's bottom edge")?;
        let lines = (top + 3..bottom - 3)
            .filter(|&row| separator_at(&menu, x, row) && separator_at(&menu, x + 1, row))
            .collect();
        Ok(Menu {
            column,
            top,
            bottom,
            lines,
        })
    }

    /// Closes the open More options menu with Escape, else a click outside it.
    fn close_menu(&mut self, view: &str) -> SandboxResult {
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        if self.run.state(view)?["options"].is_null() {
            return Ok(());
        }
        self.run.click(self.run.rest.0, self.run.rest.1)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Opens the menu of `id`, checks its groups against `groups` (`what` names the case), and clicks the row `row` of
    /// the group `group`, when one is given.
    fn menu_row(
        &mut self,
        view: &str,
        id: &str,
        groups: &[u32],
        pick: Option<(usize, u32)>,
        what: &str,
    ) -> SandboxResult {
        let menu = self.open_menu(view, id)?;
        let seen = menu.groups();
        expect(seen == groups, || {
            format!("{what}: More options holds groups of {seen:?} rows, not {groups:?} ({menu:?})")
        });
        if let Some((group, row)) = pick {
            if group >= seen.len() {
                return Err(format!("{what}: no group {group} in the menu").into());
            }
            let (x, y) = menu.row(group, row);
            self.run.click(x, y)?;
            thread::sleep(SETTLE);
        }
        Ok(())
    }

    /// Right-clicks `id`'s bar on its button `index`; where the pointer was.
    fn right_click_bar(&mut self, view: &str, id: &str, index: u32) -> SandboxResult<(u32, u32)> {
        let bar = self.bar(view, id)?;
        let (x, y) = bar.button(index);
        self.run.input.click(x, y, keys::BTN_RIGHT)?;
        thread::sleep(SETTLE);
        Ok((x, y))
    }

    /// Whether the list is open in `view`, as the child reads it; waits up to a short time for `open`.
    fn list_open(&mut self, view: &str, open: bool) -> SandboxResult<bool> {
        let start = Instant::now();
        loop {
            let now = self.run.state(view)?["bar_customize"] == true;
            if now == open || start.elapsed() > Duration::from_secs(5) {
                return Ok(now);
            }
            thread::sleep(POLL);
        }
    }

    /// The account's choice, as the settings hold it: each button's name and whether it shows.
    fn choice(&mut self, view: &str) -> SandboxResult<Value> {
        Ok(self.run.state(view)?["bar"].clone())
    }

    /// Presses `key` in the open list, then waits until the choice of `item` is `shown`; a finding when it is not.
    fn toggle_by_key(&mut self, view: &str, key: u32, item: &str, shown: bool) -> SandboxResult {
        self.run.input.key(key)?;
        let reached = self.run.until(view, &format!("{item} shown: {shown}"), |state| {
            state["bar"][item] == shown
        });
        if let Err(error) = reached {
            note(format!("a key in the list: {error}"));
        }
        Ok(())
    }

    /// Down, then Enter in the open list changes one button; Enter again changes it back. (The pointer can light a row
    /// first: Down goes on from there.)
    fn keys_toggle_one(&mut self, view: &str, what: &str) -> SandboxResult {
        let before = self.choice(view)?;
        self.run.input.key(keys::DOWN)?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.key(keys::ENTER)?;
        let changed = self
            .run
            .until(view, &format!("{what}: Down, Enter changes a button"), |state| {
                state["bar"] != before
            });
        if let Err(error) = changed {
            note(error.to_string());
            return Ok(());
        }
        self.run.input.key(keys::ENTER)?;
        let back = self
            .run
            .until(view, &format!("{what}: Enter again changes it back"), |state| {
                state["bar"] == before
            });
        if let Err(error) = back {
            note(error.to_string());
        }
        Ok(())
    }

    /// Clicks the row `row` of a list whose top-left corner is `corner`, then waits for `item` to be `shown`.
    fn toggle_by_click(&mut self, view: &str, corner: (u32, u32), row: u32, item: &str, shown: bool) -> SandboxResult {
        self.run
            .click(corner.0 + 70, corner.1 + LIST_FIRST + LIST_PITCH * row)?;
        let reached = self.run.until(view, &format!("{item} shown: {shown}"), |state| {
            state["bar"][item] == shown
        });
        if let Err(error) = reached {
            note(format!("a click on the list's row {row}: {error}"));
        }
        Ok(())
    }
}

// --- what a button does ---

/// What the parent knows before a button acts: the account's reactions on the message, and the screen.
struct Before {
    mine: Vec<Value>,
    screen: PathBuf,
}

/// Teams' brand colour in Teams Light, as the confirming button is drawn.
const BRAND: [u8; 3] = [0x5b, 0x5f, 0xc7];

impl Walk<'_> {
    fn before(&mut self, view: &str, id: &str) -> SandboxResult<Before> {
        self.run.question("clear-clipboard")?;
        let state = self.run.state(view)?;
        let mine = item(&state, id)
            .and_then(|row| row["mine"].as_array().cloned())
            .unwrap_or_default();
        let screen = self.sandbox.screenshot("qa712-before-act.png", false)?;
        Ok(Before { mine, screen })
    }

    /// Clicks `id`'s bar button `index`, which must be `name`, and checks what it did.
    fn act_on_bar(&mut self, view: &str, id: &str, index: u32, name: &str) -> SandboxResult {
        let before = self.before(view, id)?;
        self.click_bar(view, id, index)?;
        self.done(
            view,
            id,
            name,
            &before,
            &format!("the bar's {name} on {id} in the {view}"),
        )
    }

    /// Clicks the row `pick` of `id`'s menu, which must be `name`, and checks what it did.
    fn act_in_menu(&mut self, view: &str, id: &str, groups: &[u32], pick: (usize, u32), name: &str) -> SandboxResult {
        let before = self.before(view, id)?;
        let what = format!("the menu's {name} on {id} in the {view}");
        self.menu_row(view, id, groups, Some(pick), &what)?;
        self.done(view, id, name, &before, &what)
    }

    /// Checks what the button `name` did on `id`.
    fn done(&mut self, view: &str, id: &str, name: &str, before: &Before, what: &str) -> SandboxResult {
        let outcome = match name {
            "react" => self.reacted_with_picker(view),
            "reply" => self.compose_became(view, "reply"),
            "edit" => self.compose_became(view, "edit"),
            "reply-in-thread" => self
                .run
                .until(view, "the thread", |state| state["thread"] == id)
                .map(drop),
            "copy" => self.copied(view, id),
            "delete" => self.deleted(view, id, &before.screen),
            "mark-unread" => self
                .run
                .until(view, "the divider before it", |state| state["divider_before"] == id)
                .map(drop),
            "translate" => self
                .run
                .until(view, "its translation", |state| {
                    item(state, id).is_some_and(|row| row["mark"] == "translated")
                })
                .map(drop),
            pick => {
                let key = pick.strip_prefix("pick-").unwrap_or(pick).to_owned();
                let had = before.mine.iter().any(|mine| mine == key.as_str());
                self.run
                    .until(view, &format!("the reaction {key} set: {}", !had), |state| {
                        item(state, id)
                            .and_then(|row| row["mine"].as_array())
                            .is_some_and(|mine| mine.iter().any(|one| one == key.as_str()) != had)
                    })
                    .map(drop)
            }
        };
        if let Err(error) = outcome {
            note(format!("{what}: {error}"));
        }
        self.calm(view)
    }

    /// "React" opened every reaction; Escape closes it.
    fn reacted_with_picker(&mut self, view: &str) -> SandboxResult {
        self.run
            .until(view, "every reaction", |state| state["more_reactions"] == true)?;
        self.run.input.key(keys::ESC)?;
        self.run
            .until(view, "every reaction closed", |state| state["more_reactions"] == false)
            .map(drop)
    }

    /// The box became a reply or an edit (`mode`); Escape in the box ends it.
    fn compose_became(&mut self, view: &str, mode: &str) -> SandboxResult {
        self.run
            .until(view, &format!("the box in {mode}"), |state| state["compose"] == mode)?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        if self.run.state(view)?["compose"] != "new" {
            self.run.click(COMPOSE.0, COMPOSE.1)?;
            self.run.input.key(keys::ESC)?;
        }
        self.run
            .until(view, "the box new again", |state| state["compose"] == "new")
            .map(drop)
    }

    /// The clipboard holds the words of `id`.
    fn copied(&mut self, view: &str, id: &str) -> SandboxResult {
        let state = self.run.state(view)?;
        let text = item(&state, id).map(|row| row["text"].as_str().unwrap_or_default().to_owned());
        let start = Instant::now();
        loop {
            let copied = self.run.question("clipboard")?;
            if text
                .as_deref()
                .is_some_and(|text| !text.is_empty() && copied.trim() == text.trim())
            {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(10) {
                return Err(format!("the clipboard holds {copied:?}, not {text:?}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Delete asked first; its "Delete" (found on the screen, Teams' brand colour) deleted `id`.
    fn deleted(&mut self, view: &str, id: &str, before: &Path) -> SandboxResult {
        self.run
            .until(view, "the delete's question", |state| state["deleting"] == id)?;
        thread::sleep(SETTLE);
        let asked = load(&self.run.shot(self.sandbox, "qa712-delete-asked.png")?)?;
        // the question's row is over the box; "Jump to newest" above it has the brand's colour too
        let region = Rect {
            x0: 460,
            y0: COMPOSE.1 - 60,
            x1: asked.width(),
            y1: COMPOSE.1 - 26,
        };
        let changed = changed_box(&load(before)?, &asked, region).unwrap_or(region);
        let button =
            pixels_box(&asked, changed, |pixel| near(pixel, BRAND, 24)).ok_or("no confirming Delete on the screen")?;
        let (x, y) = button.centre();
        self.run.click(x, y)?;
        self.run
            .until(view, "the message deleted", |state| {
                item(state, id).is_none_or(|row| row["kind"] == "deleted")
            })
            .map(drop)
    }
}

// --- what the bar and the menu must hold ---

/// The names of the buttons the bar of `id` shows by the account's choice, as the task states it: the shown quick
/// reactions, React, then Edit, Reply, Copy text, Delete, Mark as unread when they apply, Translate when the message
/// offers it, then "More options".
fn bar_of(state: &Value, id: &str) -> Vec<String> {
    let shows = |key: &str| state["bar"][key] == true;
    let row = item(state, id).cloned().unwrap_or(Value::Null);
    let applies = |name: &str| {
        row["actions"]
            .as_array()
            .is_some_and(|all| all.iter().any(|one| one == name))
    };
    let mut names: Vec<String> = quick(state)
        .iter()
        .filter(|key| shows(&format!("reaction-{key}")))
        .map(|key| format!("pick-{key}"))
        .collect();
    if shows("react") {
        names.push("react".to_owned());
    }
    for name in ["edit", "reply", "copy", "delete", "mark-unread"] {
        let action = if name == "reply" && applies("reply-in-thread") {
            "reply-in-thread"
        } else {
            name
        };
        if shows(name) && applies(action) {
            names.push(action.to_owned());
        }
    }
    if shows("translate") && !row["offer"].is_null() {
        names.push("translate".to_owned());
    }
    names.push("more".to_owned());
    names
}

/// The groups of rows of `id`'s More options, as the task states them: the hidden quick reactions, a hairline, the
/// other hidden buttons that apply, a hairline, Mark as unread (when hidden) with Translation, the apps' "More
/// actions", then a hairline and "Customize".
fn menu_of(state: &Value, id: &str) -> Vec<u32> {
    let shows = |key: &str| state["bar"][key] == true;
    let row = item(state, id).cloned().unwrap_or(Value::Null);
    let applies = |name: &str| {
        row["actions"]
            .as_array()
            .is_some_and(|all| all.iter().any(|one| one == name))
    };
    let mut groups = Vec::new();
    let reactions = if applies("react") {
        quick(state)
            .iter()
            .filter(|key| !shows(&format!("reaction-{key}")))
            .count()
    } else {
        0
    };
    let hidden = ["react", "reply", "edit", "copy", "delete"]
        .into_iter()
        .filter(|name| !shows(name) && (applies(name) || (*name == "reply" && applies("reply-in-thread"))))
        .count();
    groups.extend([reactions, hidden].into_iter().filter(|count| *count > 0));
    if !shows("mark-unread") && applies("mark-unread") {
        groups.push(1);
    }
    if !row["offer"].is_null() {
        match groups.last_mut() {
            Some(last) => *last += 1,
            None => groups.push(1),
        }
    }
    // the apps' and workflows' "More actions", as on main
    groups.push(1);
    groups.push(1);
    groups
        .into_iter()
        .map(|count| u32::try_from(count).unwrap_or(0))
        .collect()
}

/// The keys of the quick reactions, in their places.
fn quick(state: &Value) -> Vec<String> {
    state["quick"]
        .as_array()
        .map(|keys| keys.iter().filter_map(|key| key.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// `names` with the quick reactions in front, in their order now, except `hidden`.
fn with_quick(state: &Value, hidden: Option<&str>, names: &[&str]) -> Vec<String> {
    quick(state)
        .iter()
        .filter(|key| Some(key.as_str()) != hidden)
        .map(|key| format!("pick-{key}"))
        .chain(names.iter().map(|name| (*name).to_owned()))
        .collect()
}

/// The name of the quick reaction at the place `place` (0 is the first) in the choice, as the list's row has it.
fn place_name(state: &Value, place: usize) -> String {
    format!("reaction-{}", quick(state).get(place).cloned().unwrap_or_default())
}

impl Walk<'_> {
    /// Checks that `id`'s bar holds `wanted` (the bar the task states, by the choice the child reads), and that the
    /// screen shows that many buttons; the bar's names.
    fn check_bar(&mut self, view: &str, id: &str, wanted: &[String], what: &str) -> SandboxResult {
        let state = self.run.state(view)?;
        let stated = bar_of(&state, id);
        expect(stated == wanted, || {
            format!(
                "{what}: by the choice {} the bar of {id} is {stated:?}, not {wanted:?}",
                state["bar"]
            )
        });
        let bar = self.bar(view, id)?;
        let count = usize::try_from(bar.buttons())?;
        expect(count == wanted.len(), || {
            format!(
                "{what}: the bar of {id} shows {count} buttons, not {} ({wanted:?}; {bar:?})",
                wanted.len()
            )
        });
        step(&format!("{what}: the bar of {id} holds {count} buttons: {wanted:?}"));
        Ok(())
    }

    /// The menu of `id` as the task states it.
    fn groups(&mut self, view: &str, id: &str) -> SandboxResult<Vec<u32>> {
        let state = self.run.state(view)?;
        Ok(menu_of(&state, id))
    }

    /// Clicks each of `names` on `id`'s bar, each where the bar holds it now, and checks what it did. `slot:<n>` is
    /// the bar's button `n`, whichever quick reaction is there: the most used reactions come first.
    fn act_each(&mut self, view: &str, id: &str, names: &[&str]) -> SandboxResult {
        for name in names {
            let bar = bar_of(&self.run.state(view)?, id);
            let index = match name.strip_prefix("slot:") {
                Some(slot) => slot.parse::<usize>()?,
                None => bar
                    .iter()
                    .position(|one| one == name)
                    .ok_or_else(|| format!("no {name} on the bar of {id}: {bar:?}"))?,
            };
            let actual = bar
                .get(index)
                .cloned()
                .ok_or_else(|| format!("no button {index} on {bar:?}"))?;
            self.act_on_bar(view, id, u32::try_from(index)?, &actual)?;
        }
        Ok(())
    }

    /// The accounts whose choice the settings file keeps.
    fn kept_accounts(&self) -> SandboxResult<Vec<String>> {
        let text = fs::read_to_string(self.dir.join("ui").join("a.toml")).unwrap_or_default();
        let table: toml::Table = text.parse()?;
        Ok(table
            .get("message_bar")
            .and_then(toml::Value::as_table)
            .map(|accounts| accounts.keys().cloned().collect())
            .unwrap_or_default())
    }

    /// Selects `id` in `view` (its bar shows without the pointer), takes the shots of `name`, and lets it go.
    fn bar_shots(&mut self, view: &str, id: &str, name: &str) -> SandboxResult {
        self.row(view, id)?;
        self.run.question(&format!("select {view} {id}"))?;
        self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
        thread::sleep(SETTLE);
        self.shots(name)?;
        self.calm(view)
    }
}

// --- the walk ---

impl Walk<'_> {
    fn walk(&mut self) -> SandboxResult {
        self.default_bar()?;
        self.default_menu()?;
        self.right_click_list()?;
        self.customized_bar()?;
        self.customized_menu()?;
        self.customize_from_menu()?;
        self.customize_by_keys()?;
        self.delete_and_translate_on_the_bar()?;
        self.channel_post_and_reply()
    }

    /// Teams' bar before any choice: on b's message and on a's own; each of its buttons works.
    fn default_bar(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.english)?;
        let state = self.run.state("chat")?;
        expect(self.kept_accounts()?.is_empty(), || {
            "the settings file keeps a choice at the start".to_owned()
        });
        let others = with_quick(&state, None, &["react", "reply", "more"]);
        self.check_bar("chat", &ids.english, &others, "Teams' bar on b's message")?;
        // a quick reaction set, then taken back
        self.act_each("chat", &ids.english, &["slot:0", "slot:0", "react", "reply"])?;
        let state = self.run.state("chat")?;
        let own = with_quick(&state, None, &["react", "edit", "reply", "more"]);
        self.check_bar("chat", &ids.own, &own, "Teams' bar on a's own message")?;
        self.act_each("chat", &ids.own, &["edit"])?;
        self.bar_shots("chat", &ids.english, "bar-default")?;
        step("Teams' bar");
        Ok(())
    }

    /// Teams' menu: Copy text, Mark as unread, Translation's Translate and the own message's Delete work from it.
    fn default_menu(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        let groups = self.groups("chat", &ids.english)?;
        self.act_in_menu("chat", &ids.english, &groups, (0, 0), "copy")?;
        let groups = self.groups("chat", &ids.lunch)?;
        self.act_in_menu("chat", &ids.lunch, &groups, (1, 0), "mark-unread")?;
        let groups = self.groups("chat", &ids.own_gone)?;
        self.act_in_menu("chat", &ids.own_gone, &groups, (0, 1), "delete")?;
        self.translate_from_menu("chat", &ids.hello)?;
        let menu = self.open_menu("chat", &ids.english)?;
        step(&format!("Teams' menu on b's message: {menu:?}"));
        self.shots("menu-default")?;
        self.close_menu("chat")?;
        self.calm("chat")?;
        step("Teams' menu");
        Ok(())
    }

    /// "Translation" in `id`'s menu (the row after Mark as unread), then "Translate" in its list.
    fn translate_from_menu(&mut self, view: &str, id: &str) -> SandboxResult {
        let groups = self.groups(view, id)?;
        let what = format!("the menu's Translation on {id}");
        let menu = self.open_menu(view, id)?;
        let seen = menu.groups();
        expect(seen == groups, || {
            format!("{what}: groups of {seen:?} rows, not {groups:?}")
        });
        let (x, y) = menu.row(1, 1);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        let listed = load(&self.run.shot(self.sandbox, "qa712-translation-list.png")?)?;
        // the list is left of the menu, 4 px apart, 260 px wide
        let list_x = menu.column.saturating_sub(8 + 4 + 130);
        let top = border_top(&listed, list_x, menu.top.saturating_sub(160).max(60)..listed.height())
            .ok_or_else(|| format!("{what}: no list beside the menu"))?;
        let line = (top + 2..listed.height())
            .find(|&row| separator_at(&listed, list_x, row))
            .ok_or_else(|| format!("{what}: no line between the list's rows"))?;
        self.run.click(list_x, line.saturating_sub(22))?;
        let outcome = self.run.until(view, "its translation", |state| {
            item(state, id).is_some_and(|row| row["mark"] == "translated")
        });
        if let Err(error) = outcome {
            note(format!("{what}: {error}"));
        }
        self.calm(view)
    }

    /// A right click on the bar opens the list at the pointer. The keys: Down, Enter, Up, Escape; a click on a row.
    fn right_click_list(&mut self) -> SandboxResult {
        let english = self.ids().english.clone();
        self.calm("chat")?;
        let focus = self.run.question("focus chat")?;
        let corner = self.open_list_by_right_click(&english, "the right click")?;
        self.shots("list-right-click")?;
        let state = self.run.state("chat")?;
        let (first, second) = (place_name(&state, 0), place_name(&state, 1));
        expect(second == format!("reaction-{HIDDEN}"), || {
            format!("the list's second row is {second}, not {HIDDEN}")
        });
        // Down to the first quick reaction, Enter hides it, Enter shows it again
        self.toggle_by_key("chat", keys::DOWN, &first, true)?;
        self.toggle_by_key("chat", keys::ENTER, &first, false)?;
        self.toggle_by_key("chat", keys::ENTER, &first, true)?;
        self.run.input.key(keys::DOWN)?;
        self.toggle_by_key("chat", keys::ENTER, &second, false)?;
        for _ in 0..5 {
            self.run.input.key(keys::DOWN)?;
        }
        self.toggle_by_key("chat", keys::ENTER, "reply", false)?;
        self.run.input.key(keys::DOWN)?;
        self.toggle_by_key("chat", keys::ENTER, "copy", true)?;
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::DOWN)?;
        self.toggle_by_key("chat", keys::ENTER, "mark-unread", true)?;
        // past Translate the keys go round to the first row, over "More options", which cannot hide; Up goes back
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::DOWN)?;
        self.toggle_by_key("chat", keys::ENTER, &first, false)?;
        self.run.input.key(keys::UP)?;
        self.toggle_by_key("chat", keys::ENTER, "translate", true)?;
        self.toggle_by_key("chat", keys::ENTER, "translate", false)?;
        self.run.input.key(keys::DOWN)?;
        self.toggle_by_key("chat", keys::ENTER, &first, true)?;
        // a click on a row
        self.toggle_by_click("chat", corner, ROW_TRANSLATE, "translate", true)?;
        self.toggle_by_click("chat", corner, ROW_TRANSLATE, "translate", false)?;
        self.shots("list-chosen")?;
        self.run.input.key(keys::ESC)?;
        expect(!self.list_open("chat", false)?, || {
            "Escape leaves the list open".to_owned()
        });
        let back = self.run.question("focus chat")?;
        expect(back == focus, || {
            format!("after Escape in the list the focus is on {back}, not {focus}")
        });
        step("the list by a right click");
        Ok(())
    }

    /// Right-clicks `id`'s bar on "React" and checks that the list opened at the pointer; its top-left corner.
    fn open_list_by_right_click(&mut self, id: &str, what: &str) -> SandboxResult<(u32, u32)> {
        let (x, y) = self.right_click_bar("chat", id, 1)?;
        let state = self.run.state("chat")?;
        expect(state["bar_customize"] == true && state["options"].is_null(), || {
            format!(
                "{what}: a right click on the bar does not open the list alone: {}",
                state["bar_customize"]
            )
        });
        let listed = load(&self.run.shot(self.sandbox, "qa712-list.png")?)?;
        // the list's border is darker than a menu's, and drawn over what is under it
        let top = (y.saturating_sub(4)..y + 14).find(|&row| edge(&listed, (x + 70, row), (0, 1)));
        let left = (x.saturating_sub(6)..x + 8).find(|&column| edge(&listed, (column, y + 40), (1, 0)));
        // a list with no room under the pointer moves up into the window
        if y + 400 < listed.height() {
            expect(top.is_some() && left.is_some(), || {
                format!("{what}: the list does not open at the pointer ({x}, {y}): top {top:?}, left {left:?}")
            });
        }
        Ok((left.unwrap_or(x), top.unwrap_or(y)))
    }
}

impl Walk<'_> {
    /// The chosen bar: the second quick reaction and Reply hidden, Copy text and Mark as unread shown. Edit is not on
    /// b's message. Each button works.
    fn customized_bar(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        let state = self.run.state("chat")?;
        let others = with_quick(&state, Some(HIDDEN), &["react", "copy", "mark-unread", "more"]);
        self.check_bar("chat", &ids.english, &others, "the chosen bar on b's message")?;
        self.act_each(
            "chat",
            &ids.english,
            &["slot:1", "slot:1", "react", "copy", "mark-unread"],
        )?;
        self.hidden_stays_hidden("chat", &ids.english, "after reactions from the bar")?;
        let state = self.run.state("chat")?;
        let own = with_quick(&state, Some(HIDDEN), &["react", "edit", "copy", "mark-unread", "more"]);
        self.check_bar("chat", &ids.own, &own, "the chosen bar on a's own message")?;
        self.act_each("chat", &ids.own, &["edit", "copy"])?;
        self.bar_shots("chat", &ids.english, "bar-chosen")?;
        self.bar_shots("chat", &ids.own, "bar-chosen-own")?;
        expect(self.kept_accounts()? == [LAB_A], || {
            "the settings file does not keep a's choice".to_owned()
        });
        step("the chosen bar");
        Ok(())
    }

    /// Heart stays hidden in any order of the quick reactions, and the bar of `id` shows each other one.
    fn hidden_stays_hidden(&mut self, view: &str, id: &str, what: &str) -> SandboxResult {
        let state = self.run.state(view)?;
        let order = quick(&state);
        step(&format!("{what}: the quick reactions are {order:?}"));
        expect(state["bar"][format!("reaction-{HIDDEN}")] == false, || {
            format!("{what}: {HIDDEN} shows again in the choice {}", state["bar"])
        });
        let others = order.iter().filter(|key| *key != HIDDEN).count();
        let bar = self.bar(view, id)?;
        let shown = bar_of(&state, id);
        let picks = shown.iter().filter(|name| name.starts_with("pick-")).count();
        expect(!shown.contains(&format!("pick-{HIDDEN}")) && picks == others, || {
            format!("{what}: the bar of {id} is {shown:?} with the quick reactions {order:?}")
        });
        let count = usize::try_from(bar.buttons())?;
        expect(count == shown.len(), || {
            format!(
                "{what}: the bar of {id} shows {count} buttons, not {} ({shown:?})",
                shown.len()
            )
        });
        Ok(())
    }

    /// The chosen menu: the hidden quick reaction, a hairline, Reply (with Translation), then "Customize".
    fn customized_menu(&mut self) -> SandboxResult {
        let english = self.ids().english.clone();
        let groups = self.groups("chat", &english)?;
        // the hidden Heart, set and taken back: the row stays Heart while the quick reactions change their order
        let heart = format!("pick-{HIDDEN}");
        self.act_in_menu("chat", &english, &groups, (0, 0), &heart)?;
        self.act_in_menu("chat", &english, &groups, (0, 0), &heart)?;
        self.act_in_menu("chat", &english, &groups, (1, 0), "reply")?;
        self.hidden_stays_hidden("chat", &english, "after reactions from the menu")?;
        let menu = self.open_menu("chat", &english)?;
        step(&format!("the chosen menu on b's message: {menu:?}, {groups:?}"));
        self.shots("menu-chosen")?;
        self.close_menu("chat")?;
        self.calm("chat")?;
        step("the chosen menu");
        Ok(())
    }

    /// "Customize" at the foot of the menu: the menu closes and the list opens; a click on a row there shows Delete and
    /// Translate. Escape gives the focus back.
    fn customize_from_menu(&mut self) -> SandboxResult {
        let english = self.ids().english.clone();
        let focus = self.run.question("focus chat")?;
        let menu = self.open_menu("chat", &english)?;
        let last = menu.lines.len();
        let (x, y) = menu.row(last, 0);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        let state = self.run.state("chat")?;
        expect(state["bar_customize"] == true && state["options"].is_null(), || {
            format!(
                "\"Customize\" does not close the menu and open the list: list {}, menu {}",
                state["bar_customize"], state["options"]
            )
        });
        let keys_on = self.run.question("focus chat")?;
        step(&format!("the list from Customize is open; the keys go to: {keys_on}"));
        self.shots("list-from-menu")?;
        self.keys_toggle_one("chat", "the list from Customize")?;
        self.run.input.key(keys::ESC)?;
        expect(!self.list_open("chat", false)?, || {
            "Escape leaves the list from Customize open".to_owned()
        });
        let back = self.run.question("focus chat")?;
        expect(back == focus, || {
            format!("after Escape in the list from Customize the focus is on {back}, not {focus}")
        });
        self.calm("chat")?;
        let corner = self.open_list_by_right_click(&english, "the second right click")?;
        self.toggle_by_click("chat", corner, ROW_DELETE, "delete", true)?;
        self.toggle_by_click("chat", corner, ROW_TRANSLATE, "translate", true)?;
        self.run.input.key(keys::ESC)?;
        self.list_open("chat", false)?;
        self.calm("chat")?;
        step("the list from Customize");
        Ok(())
    }

    /// The keys alone: a message selected, Shift+F10, End to "Customize", Enter opens the list, Escape gives the keys
    /// back to the message list.
    fn customize_by_keys(&mut self) -> SandboxResult {
        let english = self.ids().english.clone();
        self.row("chat", &english)?;
        self.run.question(&format!("select chat {english}"))?;
        thread::sleep(SETTLE);
        self.run.input.key_with(&[Modifier::Shift], KEY_F10)?;
        self.run.until("chat", "Shift+F10 opens the menu", |state| {
            state["options"] == english.as_str()
        })?;
        thread::sleep(SETTLE);
        self.run.input.key(keys::END)?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.key(keys::ENTER)?;
        let opened = self.list_open("chat", true)?;
        expect(opened, || "End and Enter in the menu do not open the list".to_owned());
        if opened {
            self.shots("list-by-keys")?;
            let first = place_name(&self.run.state("chat")?, 0);
            self.toggle_by_key("chat", keys::DOWN, &first, true)?;
            self.toggle_by_key("chat", keys::ENTER, &first, false)?;
            self.toggle_by_key("chat", keys::ENTER, &first, true)?;
            self.run.input.key(keys::ESC)?;
            expect(!self.list_open("chat", false)?, || {
                "Escape leaves the list by the keys open".to_owned()
            });
            let back = self.run.question("focus chat")?;
            expect(back == "list", || {
                format!("after Escape in the list opened by the keys the focus is on {back}, not the message list")
            });
        }
        self.calm("chat")?;
        step("the list by the keys");
        Ok(())
    }

    /// Delete and Translate on the bar: Delete asks first, Translate translates.
    fn delete_and_translate_on_the_bar(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        let state = self.run.state("chat")?;
        let mut own = with_quick(
            &state,
            Some(HIDDEN),
            &["react", "edit", "copy", "delete", "mark-unread"],
        );
        if item(&state, &ids.own_bar_gone).is_some_and(|row| !row["offer"].is_null()) {
            own.push("translate".to_owned());
        }
        own.push("more".to_owned());
        self.check_bar("chat", &ids.own_bar_gone, &own, "Delete on the bar")?;
        self.bar_shots("chat", &ids.own_bar_gone, "bar-delete")?;
        self.act_each("chat", &ids.own_bar_gone, &["delete"])?;
        let others = with_quick(
            &state,
            Some(HIDDEN),
            &["react", "copy", "mark-unread", "translate", "more"],
        );
        self.check_bar("chat", &ids.four, &others, "Translate on the bar")?;
        self.act_each("chat", &ids.four, &["translate"])?;
        // with Translate on the bar, the menu's Translation keeps its row (its list holds the settings)
        let groups = self.groups("chat", &ids.four)?;
        self.menu_row("chat", &ids.four, &groups, None, "the menu with Translate on the bar")?;
        self.close_menu("chat")?;
        self.calm("chat")?;
        step("Delete and Translate on the bar");
        Ok(())
    }

    /// A channel's post and a reply in its thread: the same choice; Reply in thread is in the post's menu.
    fn channel_post_and_reply(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.run.question("open-channel")?;
        self.run.until("posts", "b's post in the channel", |state| {
            item(state, &ids.post).is_some()
        })?;
        thread::sleep(SETTLE);
        let state = self.run.state("posts")?;
        let post = bar_of(&state, &ids.post);
        expect(!post.iter().any(|name| name.starts_with("reply")), || {
            format!("the post's bar shows a reply though Reply is hidden: {post:?}")
        });
        self.check_bar("posts", &ids.post, &post, "the bar on a channel's post")?;
        self.act_each("posts", &ids.post, &["copy", "mark-unread"])?;
        self.bar_shots("posts", &ids.post, "bar-post")?;
        let groups = self.groups("posts", &ids.post)?;
        self.act_in_menu("posts", &ids.post, &groups, (1, 0), "reply-in-thread")?;
        self.run.question(&format!("open-thread {CHANNEL_POST}"))?;
        self.run.until("thread", "b's reply in the thread", |state| {
            item(state, &ids.reply).is_some()
        })?;
        thread::sleep(SETTLE);
        let state = self.run.state("thread")?;
        let reply = bar_of(&state, &ids.reply);
        expect(
            !reply.iter().any(|name| name == "edit" || name.starts_with("reply")),
            || format!("the thread reply's bar shows Edit or a reply: {reply:?}"),
        );
        self.check_bar("thread", &ids.reply, &reply, "the bar on a thread's reply")?;
        let names: &[&str] = if reply.iter().any(|name| name == "translate") {
            &["copy", "translate"]
        } else {
            &["copy"]
        };
        self.act_each("thread", &ids.reply, names)?;
        self.bar_shots("thread", &ids.reply, "bar-thread")?;
        step("a channel's post and a reply in its thread");
        Ok(())
    }
}

/// a's choice at the end of the walk; every other quick reaction shows.
const CHOSEN: [(&str, bool); 8] = [
    ("reaction-heart", false),
    ("react", true),
    ("edit", true),
    ("reply", false),
    ("copy", true),
    ("delete", true),
    ("mark-unread", true),
    ("translate", true),
];

/// Whether `choice` (the child's names and whether each shows) is `wanted`, else Teams' bar (`None`). A quick
/// reaction that `wanted` does not name shows.
fn choice_is(choice: &Value, wanted: Option<&[(&str, bool)]>) -> bool {
    let wanted = wanted.unwrap_or_default();
    let named = wanted.iter().all(|(key, shown)| choice[*key] == *shown);
    let others = choice.as_object().is_some_and(|choice| {
        choice.iter().all(|(key, shown)| {
            let by_default = key.starts_with("reaction-")
                || BarItem::ACTIONS
                    .iter()
                    .any(|item| item.key() == key.as_str() && item.shown_by_default());
            let wanted = wanted
                .iter()
                .find(|(name, _)| name == key)
                .map_or(by_default, |(_, shown)| *shown);
            *shown == wanted
        })
    });
    named && others
}

impl Walk<'_> {
    /// After a restart a's choice is kept: the bar and its Copy text.
    fn after_restart(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.english)?;
        let choice = self.choice("chat")?;
        expect(choice_is(&choice, Some(&CHOSEN)), || {
            format!("after a restart a's choice is {choice}, not the one made before it")
        });
        let state = self.run.state("chat")?;
        let mut others = with_quick(&state, Some(HIDDEN), &["react", "copy", "mark-unread"]);
        if item(&state, &ids.english).is_some_and(|row| !row["offer"].is_null()) {
            others.push("translate".to_owned());
        }
        others.push("more".to_owned());
        self.check_bar("chat", &ids.english, &others, "the bar after a restart")?;
        self.act_each("chat", &ids.english, &["copy"])?;
        self.bar_shots("chat", &ids.english, "bar-after-restart")?;
        step("the choice after a restart");
        Ok(())
    }

    /// b's account on the same settings file keeps Teams' bar, while the file keeps a's choice.
    fn other_account(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.own)?;
        let state = self.run.state("chat")?;
        expect(state["account"] == LAB_B, || {
            format!("the app shows {}, not b", state["account"])
        });
        expect(choice_is(&state["bar"], None), || {
            format!("b's choice is {}, not Teams' bar", state["bar"])
        });
        // a's message is someone else's for b, b's own is b's
        let others = with_quick(&state, None, &["react", "reply", "more"]);
        self.check_bar("chat", &ids.own, &others, "b's bar on a's message")?;
        self.act_each("chat", &ids.own, &["reply"])?;
        let own = with_quick(&state, None, &["react", "edit", "reply", "more"]);
        self.check_bar("chat", &ids.english, &own, "b's bar on b's own message")?;
        self.bar_shots("chat", &ids.own, "bar-other-account")?;
        let kept = self.kept_accounts()?;
        expect(kept == [LAB_A], || {
            format!("with b's start the settings file keeps the choices of {kept:?}, not a's alone")
        });
        step("another account keeps Teams' bar");
        Ok(())
    }

    /// The rows clicked back give Teams' bar again, and the settings file keeps no choice.
    fn back_to_default(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.english)?;
        let corner = self.open_list_by_right_click(&ids.english, "the right click after the restarts")?;
        let quick = quick(&self.run.state("chat")?);
        let heart = quick.iter().position(|key| key == HIDDEN);
        expect(heart.is_some(), || {
            format!("{HIDDEN} is not a quick reaction now: {quick:?}")
        });
        let heart_row = u32::try_from(heart.unwrap_or(1))?;
        let heart_name = format!("reaction-{HIDDEN}");
        for (row, key, shown) in [
            (heart_row, heart_name.as_str(), true),
            (6, "reply", true),
            (7, "copy", false),
            (ROW_DELETE, "delete", false),
            (9, "mark-unread", false),
            (ROW_TRANSLATE, "translate", false),
        ] {
            self.toggle_by_click("chat", corner, row, key, shown)?;
        }
        self.shots("list-back-to-default")?;
        self.run.input.key(keys::ESC)?;
        self.list_open("chat", false)?;
        self.calm("chat")?;
        let state = self.run.state("chat")?;
        expect(choice_is(&state["bar"], None), || {
            format!(
                "after the rows clicked back a's choice is {}, not Teams' bar",
                state["bar"]
            )
        });
        let others = with_quick(&state, None, &["react", "reply", "more"]);
        self.check_bar("chat", &ids.english, &others, "Teams' bar again")?;
        self.act_each("chat", &ids.english, &["reply"])?;
        let groups = self.groups("chat", &ids.english)?;
        self.menu_row("chat", &ids.english, &groups, None, "Teams' menu again")?;
        self.close_menu("chat")?;
        self.calm("chat")?;
        self.bar_shots("chat", &ids.english, "bar-back-to-default")?;
        let kept = self.kept_accounts()?;
        expect(kept.is_empty(), || {
            format!("Teams' bar again, but the settings file keeps the choices of {kept:?}")
        });
        step("back to Teams' bar");
        Ok(())
    }
}

fn item<'v>(state: &'v Value, id: &str) -> Option<&'v Value> {
    state["items"]
        .as_array()
        .and_then(|items| items.iter().find(|row| row["id"] == id))
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

// --- the screen ---

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

/// The first `y` in `rows` of column `x` that is a panel's bottom border: [`BORDER`] under two rows of [`SURFACE`],
/// not over [`SURFACE`].
fn panel_bottom(shot: &image::RgbImage, x: u32, rows: std::ops::Range<u32>) -> Option<u32> {
    rows.into_iter().find(|&y| {
        near(at(shot, x, y), BORDER, 4)
            && at(shot, x, y - 1) == SURFACE
            && at(shot, x, y - 2) == SURFACE
            && at(shot, x, y + 1) != SURFACE
    })
}

/// Whether `point` is a panel's edge: not [`SURFACE`], with two pixels of [`SURFACE`] after it in the direction `step`.
fn edge(shot: &image::RgbImage, point: (u32, u32), step: (u32, u32)) -> bool {
    let (x, y) = point;
    at(shot, x, y) != SURFACE
        && at(shot, x + step.0, y + step.1) == SURFACE
        && at(shot, x + 2 * step.0, y + 2 * step.1) == SURFACE
}

/// Whether (`x`, `y`) is a line inside a panel: [`BORDER`] with [`SURFACE`] above and under.
fn separator_at(shot: &image::RgbImage, x: u32, y: u32) -> bool {
    near(at(shot, x, y), BORDER, 4) && at(shot, x, y - 1) == SURFACE && at(shot, x, y + 1) == SURFACE
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

/// Keeps the screenshot `png` as `name` under `target/ui-shots/`, and in `QA712_EVIDENCE` when it is set.
fn keep(png: &Path, name: &str) -> SandboxResult {
    fs::copy(png, shots_dir()?.join(name))?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        let evidence = PathBuf::from(evidence);
        fs::create_dir_all(&evidence)?;
        fs::copy(png, evidence.join(name))?;
    }
    Ok(())
}

// --- the service ---

/// The made-up messages' ids.
#[derive(Clone, Debug, Default)]
struct Ids {
    hello: String,
    english: String,
    own: String,
    lunch: String,
    own_gone: String,
    own_bar_gone: String,
    four: String,
    post: String,
    reply: String,
}

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    ids: Ids,
}

impl Service {
    /// The mock with the made-up messages: b's and a's in the a–b chat, b's post in the team's channel and b's reply
    /// under its post.
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let post = |from: &str, text: &str| -> SandboxResult<String> {
            mock.post_as(from, AB_CHAT, &format!("<p>{text}</p>"))
                .ok_or_else(|| format!("the post {text:?} refused").into())
        };
        let ids = Ids {
            hello: post(LAB_B, HELLO.0)?,
            english: post(LAB_B, ENGLISH)?,
            own: post(LAB_A, OWN.0)?,
            lunch: post(LAB_B, LUNCH.0)?,
            own_gone: post(LAB_A, OWN_GONE)?,
            own_bar_gone: post(LAB_A, OWN_BAR_GONE)?,
            four: post(LAB_B, FOUR.0)?,
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
        post(LAB_B, WEATHER.0)?;
        let _ = mock.take_events(LAB_A);
        Ok(Self { mock, trouter, ids })
    }

    fn env(&self, account: &str) -> Vec<(&'static str, String)> {
        let mri = if account == "b" { LAB_B } else { LAB_A };
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_ENV, self.mock.token(mri).unwrap_or_default()),
        ]
    }
}

// --- the made-up tool ---

/// The made-up tool: where it is, and the child's `PATH` with it first and no real `claude` or `codex`.
struct FakeTool {
    home: PathBuf,
    path: String,
}

impl FakeTool {
    fn install(sandbox: &Sandbox) -> SandboxResult<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let (bin, state, home, commands) = (
            sandbox.path("ai-bin"),
            sandbox.path("ai-state"),
            sandbox.path("ai-home"),
            sandbox.path("ai-commands"),
        );
        for dir in [&bin, &state, &home, &commands] {
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
        let program = bin.join("claude");
        fs::write(&program, FAKE_CLAUDE.replace("__STATE__", &state.display().to_string()))?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        Ok(Self { home, path })
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
    /// Starts the app of `account` with `env` and its store and settings in `dir`; once its window is drawn.
    fn launch(
        sandbox: &Sandbox,
        env: &[(&'static str, String)],
        dir: &Path,
        tool: &FakeTool,
        account: &str,
    ) -> SandboxResult<Self> {
        let dir_text = dir.to_string_lossy().into_owned();
        let home = tool.home.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![
            (CHILD_ENV, "app"),
            (DIR_ENV, &dir_text),
            (ACCOUNT_ENV, account),
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
        keep(&png, name)?;
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
            .shot(sandbox, "qa712-failed-run.png")
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

/// The engine of `alias` (`mri`) on the parent's mock, its store in `dir`.
fn engine_on_the_mock(dir: &Path, alias: Alias, mri: &str) -> SandboxResult<EngineHandle> {
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
    let upn = format!("bot-{alias}@lab.example.test");
    let author = Author::new(Mri::new(mri), display_name(&upn));
    let known_names = [(LAB_A, "Lab A"), (LAB_B, "Lab B"), (LAB_C, "Lab C")]
        .into_iter()
        .filter(|(other, _)| *other != mri)
        .map(|(other, name)| (other.to_owned(), name.to_owned()))
        .collect();
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, alias))?,
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
        known_names,
    })?)
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
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let (alias, mri) = if std::env::var(ACCOUNT_ENV).as_deref() == Ok("b") {
        (Alias::B, LAB_B)
    } else {
        (Alias::A, LAB_A)
    };
    let first = support::started(alias, engine_on_the_mock(&dir, alias, mri)?)?;
    let runner = support::runner(&dir)?;
    // one settings file for both accounts: each keeps its choice under its MRI
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
        "select" | "unselect" => select(shell, verb == "select", rest, window, cx),
        "focus" => focus_of(shell, rest, window, cx),
        "clipboard" => cx
            .read_from_clipboard()
            .and_then(|copied| copied.text())
            .unwrap_or_else(|| "<none>".to_owned())
            .replace('\n', "\\n"),
        "clear-clipboard" => {
            cx.write_to_clipboard(ClipboardItem::new_string("<cleared>".to_owned()));
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

/// The message `id` selected in `view` with the list's focus, as Escape in the box and Up give them; or no message
/// selected (`on` false, `rest` the view alone).
fn select(shell: &Entity<Shell>, on: bool, rest: &str, window: &mut Window, cx: &mut App) -> String {
    let (view, id) = rest.split_once(' ').unwrap_or((rest, ""));
    let Some(chat) = view_of(shell, view, cx) else {
        return "no view".to_owned();
    };
    chat.update(cx, |chat, cx| {
        if on {
            chat.select(Some(id.to_owned()), cx);
            chat.focus_handle().clone().focus(window, cx);
        } else {
            chat.select(None, cx);
        }
    });
    "ok".to_owned()
}

/// Where the keys go: the message list, the compose box, another element or none.
fn focus_of(shell: &Entity<Shell>, view: &str, window: &Window, cx: &App) -> String {
    let Some(chat) = view_of(shell, view, cx) else {
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

/// The view `view`: its menu, list, box and question, the account's choice, and each message's row.
fn state_of(shell: &Entity<Shell>, view: &str, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let account = shell_ref.account().to_owned();
    let main = match shell_ref.main() {
        Main::Chat(_) => "chat".to_owned(),
        Main::Channel(_) => "channel".to_owned(),
        other => format!("{other:?}"),
    };
    let thread = shell_ref
        .channel()
        .and_then(|channel| channel.read(cx).thread().map(|(post, _)| post.to_owned()));
    let Some(chat) = view_of(shell, view, cx) else {
        return json!({"main": main, "account": account, "thread": thread, "items": []});
    };
    let chat = chat.read(cx);
    let settings = chat.compose().read(cx).settings().read(cx).settings();
    // the quick reactions now, each other reaction the account hid, and the other buttons
    let hidden = settings
        .message_bar
        .get(&account)
        .map(|choice| choice.hidden.clone())
        .unwrap_or_default();
    let bar: serde_json::Map<String, Value> = chat
        .quick()
        .iter()
        .filter_map(|key| BarItem::reaction(key))
        .chain(hidden)
        .chain(BarItem::ACTIONS)
        .map(|item| (item.key().into_owned(), json!(settings.shows_bar_item(&account, item))))
        .collect();
    let list = chat.list_state();
    let viewport = list.viewport_bounds();
    let mut divider_before = Value::Null;
    let mut after_divider = false;
    let mut items = Vec::new();
    for (index, item) in chat.items().iter().enumerate() {
        match item {
            Item::NewMessages => after_divider = true,
            Item::Message(message) => {
                if after_divider && divider_before.is_null() {
                    divider_before = json!(message.id);
                }
                items.push(row_json(message, list.bounds_for_item(index)));
            }
            _ => {}
        }
    }
    json!({
        "main": main,
        "account": account,
        "thread": thread,
        "conversation": chat.conversation(),
        "selected": chat.selected(),
        "hovered": chat.hovered(),
        "options": chat.options(),
        "bar_customize": chat.bar_customize_open(),
        "deleting": chat.deleting(),
        "more_reactions": chat.more_reactions().is_some(),
        "quick": chat.quick(),
        "bar": bar,
        "compose": match chat.compose().read(cx).mode() {
            Mode::New => "new",
            Mode::Reply(_) => "reply",
            Mode::Edit { .. } => "edit",
        },
        "divider_before": divider_before,
        "viewport": bounds_json(viewport),
        "items": items,
    })
}

fn bounds_json(bounds: gpui::Bounds<gpui::Pixels>) -> Value {
    let pixels = |value: gpui::Pixels| f64::from(value);
    json!([
        pixels(bounds.origin.x),
        pixels(bounds.origin.y),
        pixels(bounds.size.width),
        pixels(bounds.size.height)
    ])
}

/// One message's row: its words as shown, its mark and offer, its actions, the account's reactions, and where the
/// list drew it.
fn row_json(message: &teams_ui::chat_view::MessageItem, bounds: Option<gpui::Bounds<gpui::Pixels>>) -> Value {
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
        Some(Mark::Translated { .. }) => "translated",
        Some(Mark::Original { .. }) => "original",
        Some(Mark::Waiting) => "waiting",
        Some(Mark::Failed | Mark::Stopped { .. }) => "failed",
        None => "none",
    };
    let actions: Vec<&str> = message
        .actions()
        .into_iter()
        .map(teams_ui::chat_view::MessageAction::name)
        .collect();
    let mine: Vec<&str> = message
        .reactions
        .iter()
        .filter(|reaction| reaction.mine)
        .map(|reaction| reaction.key.as_str())
        .collect();
    json!({
        "id": message.id,
        "own": message.own,
        "kind": kind,
        "text": text,
        "mark": mark,
        "offer": message.translate.map(|offer| offer.label().to_string()),
        "actions": actions,
        "mine": mine,
        "rect": bounds.map(bounds_json),
    })
}
