//! A bot's cards and buttons with the bots shown (`NGTEAMS_BOTS=1`), as a user sees and uses them in the app's window
//! in the UI sandbox (`harness = false`: GPUI runs on the process's main thread). It is the QA test of task 224.
//!
//! The parent runs the mock (`teams-mock`) and scripts made-up bots that answer the chat service's invokes. Each bot
//! posts a card message (`RichText/Media_Card`, the activity in its `Swift`) into a chat of its own. The parent runs
//! a's app as a child in the sandbox with a control socket, and uses it as a user does: it clicks and types with the
//! sandbox's pointer and keyboard, and finds the card's fields and buttons on the screen by their colours (Teams
//! Light). It reads what the bots got from the mock, the views' rows from the child, and the links the sandbox's URI
//! opener got (it opens nothing).
//!
//! - **The form.** Every kind of input. A required input left empty, and a number out of its range, stop the send and
//!   show the card's error. Typed letters and digits stay in the field (no chat shortcut fires). Tab goes to the next
//!   field. A slow bot shows the button's spinner, and a second click sends nothing more. The answers reach the bot
//!   merged into the button's data, with Teams' own token. `Action.Execute` puts the bot's card in the card's place.
//! - **The answers.** A message, an error, a sign-in request and a dialog show as a line under the card; the last two
//!   open the message in Teams on the web. A bot that is not there, and a bot that does not answer in time, show
//!   "The app didn't respond. Try again.".
//! - **`imBack` and `messageBack`.** They post the user's message; `messageBack` also asks the bot.
//! - **Suggested actions.** They stand over the compose box of a 1:1 chat, and a click posts the answer.
//! - **Refresh.** A card whose refresh names the account asks its bot once by itself; "Refresh card" in the message's
//!   More options asks again.
//! - **Many inputs and long labels**, and **a link's app card** (it asks no bot).
//! - **Read-only mode** (a second run): inputs take nothing, and a button opens the message in Teams on the web.
//! - **An account whose cloud serves no apps** (a third run): nothing asks a bot.
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa-bot-cards-*.png`, in Teams
//! Light and Dark. Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{Capabilities, Outbox};
use teams_ctl::protocol::{OpenTarget, Request};
use teams_ctl::{Client, Timeouts};
use teams_mock::{AC_CHAT, AMS_BASE, BotReply, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, TrouterMock};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "NGTEAMS_QA_BOT_CARDS_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_QA_BOT_CARDS_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_BOT_CARDS_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_QA_BOT_CARDS_TROUTER";
const TOKEN_ENV: &str = "NGTEAMS_QA_BOT_CARDS_TOKEN";
const DIR_ENV: &str = "NGTEAMS_QA_BOT_CARDS_DIR";
/// `read-only` (the read-only mode) or `no-apps` (a cloud that serves no apps); else the ordinary app.
const MODE_ENV: &str = "NGTEAMS_QA_BOT_CARDS_MODE";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa-bot-cards-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);
/// How long a click that must send nothing gets to send it anyway.
const QUIET: Duration = Duration::from_millis(1500);
/// How long the slow bot takes to answer.
const SLOW: Duration = Duration::from_secs(4);
/// Longer than the chat service's request timeout ([`teams_proto::http::REQUEST_TIMEOUT`]).
const NO_ANSWER: Duration = Duration::from_secs(36);

/// Teams Light's card surface, its borders, a field's fill and Teams' red.
const SURFACE: [u8; 3] = [255, 255, 255];
const BORDER: [u8; 3] = [224, 224, 224];
const FILL: [u8; 3] = [243, 244, 250];
const DANGER: [u8; 3] = [196, 49, 75];
/// A field's fill while it takes nothing (the read-only mode, an ask on its way).
const OFF_FILL: [u8; 3] = [243, 243, 243];
/// The ring of a focused field, in Teams' brand colour.
const RING: [u8; 3] = [91, 95, 199];
/// Where the chat's pane starts on the 1280-wide screen: the rail and the chat list are left of it.
const PANE_X: u32 = 380;
/// From the centre of the menu's "Customize" row to the centre of the row over it, px: half a row, a 2 px gap, the
/// hairline (1 px with 4 px over and under), a 2 px gap and half a row.
const CUSTOMIZE_FOOT: u32 = 16 + 2 + 9 + 2 + 16;
/// The chat's messages, between its header and its compose box.
const PANE: Rect = Rect {
    x0: PANE_X,
    y0: 96,
    x1: 1280,
    y1: 721,
};

const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
const NAME_TYPED: &str = "Ay are 12345 x";
const NOTE_TABBED: &str = "tabbed";

/// The made-up bots, each with a chat of its own.
const FORM_BOT: &str = "28:00000000-0000-4000-8000-0000000000b1";
const MESSAGE_BOT: &str = "28:00000000-0000-4000-8000-0000000000b2";
const ERROR_BOT: &str = "28:00000000-0000-4000-8000-0000000000b3";
const SIGN_IN_BOT: &str = "28:00000000-0000-4000-8000-0000000000b4";
const DIALOG_BOT: &str = "28:00000000-0000-4000-8000-0000000000b5";
/// Nobody scripted it: the service answers 404.
const GONE_BOT: &str = "28:00000000-0000-4000-8000-0000000000b6";
const LATE_BOT: &str = "28:00000000-0000-4000-8000-0000000000b7";
const BACK_BOT: &str = "28:00000000-0000-4000-8000-0000000000b8";
const SUGGEST_BOT: &str = "28:00000000-0000-4000-8000-0000000000b9";
const REFRESH_BOT: &str = "28:00000000-0000-4000-8000-0000000000ba";
const LONG_BOT: &str = "28:00000000-0000-4000-8000-0000000000bb";

const LONG_LABEL: &str = "A made-up question with a very long label that goes on past the width of the card, so \
    that it must wrap onto a second and maybe a third line without cutting a word";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_bot_cards_window: {error}");
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
    let Some(sandbox) = sandbox_or_skip("ngteams bot cards", &options)? else {
        return Ok(());
    };
    let service = Service::start(&runtime)?;
    for mode in ["", "read-only", "no-apps"] {
        let dir = tempfile::tempdir()?;
        let outcome = match mode {
            "" => walk(&sandbox, &runtime, &service, dir.path()),
            "read-only" => read_only(&sandbox, &runtime, &service, dir.path()),
            _ => no_apps(&sandbox, &runtime, &service, dir.path()),
        };
        if let Err(error) = outcome {
            return Err(format!("{error}{}", sandbox.logs()).into());
        }
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams bot cards: ok");
    Ok(())
}

/// The walk of the ordinary app.
fn walk(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir, "")?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service,
        };
        ctx.form()?;
        ctx.answers()?;
        ctx.backs()?;
        ctx.suggested()?;
        ctx.refresh()?;
        ctx.long()?;
        ctx.app_card()
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    runtime: &'a tokio::runtime::Runtime,
    service: &'a Service,
}

impl Walk<'_> {
    /// The invokes the mock got so far (each request to `v1/agents/<bot>/invoke`, the refused ones too).
    fn invokes(&self) -> Vec<Value> {
        invokes_of(self.runtime, &self.service.mock)
    }

    /// Opens `chat`, waits for message `id` and its card on the screen; the card.
    fn open_card(&mut self, chat: &str, id: &str, what: &str) -> SandboxResult<Rect> {
        self.run.open(chat)?;
        self.run
            .until(what, |state| state.item(id).is_some_and(|item| item.kind == "cards"))?;
        self.run.card(self.sandbox, &format!("qa-bot-cards-{what}-light.png"))
    }

    /// Clicks (`x`, `y`) and waits for one more invoke than `before`; that invoke.
    fn click_invokes(&mut self, (x, y): (u32, u32), before: usize, what: &str) -> SandboxResult<Value> {
        self.run.click(x, y)?;
        let found = wait_until(WAIT, || {
            let all = self.invokes();
            (all.len() > before).then(|| all[before].clone())
        });
        found.ok_or_else(|| format!("{what}: no invoke reached the bot").into())
    }

    /// Clicks (`x`, `y`) and checks that nothing reaches a bot.
    fn click_quiet(&mut self, (x, y): (u32, u32), what: &str) -> SandboxResult {
        let before = self.invokes().len();
        self.run.click(x, y)?;
        thread::sleep(QUIET);
        let after = self.invokes().len();
        if after != before {
            note(format!(
                "{what}: {} invokes reached a bot, expected none",
                after - before
            ));
        }
        Ok(())
    }

    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.run.shot(self.sandbox, name)
    }

    /// The shots of `name` in Teams Dark, then back to Light.
    fn dark(&mut self, name: &str) -> SandboxResult {
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.shot(&format!("qa-bot-cards-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        self.shot(&format!("qa-bot-cards-{name}-light.png"))?;
        Ok(())
    }
}

// --- the form ---

impl Walk<'_> {
    fn form(&mut self) -> SandboxResult {
        let service = self.service;
        let card = self.open_card(&service.chats.form, &service.ids.form, "form")?;
        self.dark("form")?;
        let layout = Form::find(&self.run.last, card)?;
        step(&format!("the form: {layout:?}"));
        self.required()?;
        self.typing()?;
        self.range()?;
        self.choices()?;
        self.send()?;
        self.execute()
    }

    /// The form where it is now: a line under a field moves the card up, as the chat keeps its newest end in view.
    fn form_now(&mut self, name: &str) -> SandboxResult<Form> {
        self.run.card(self.sandbox, &format!("qa-bot-cards-{name}-light.png"))?;
        let card = tallest_card(&self.run.last).ok_or("no form on the screen")?;
        Form::find(&self.run.last, card)
    }

    /// Send with the required name empty: its error under it, and nothing sent.
    fn required(&mut self) -> SandboxResult {
        let form = self.form_now("before-required")?;
        let red = danger_in(&self.run.last, PANE);
        self.click_quiet(form.send.centre(), "Send with the required name empty")?;
        let shot = self.shot("qa-bot-cards-required-light.png")?;
        let after = danger_in(&load(&shot)?, PANE);
        if after <= red + 20 {
            note(format!(
                "Send with the required name empty: no error shows under it ({red} -> {after} red pixels, {})",
                shot.display()
            ));
        }
        self.dark("required")?;
        step("a required input left empty stops the send");
        Ok(())
    }

    /// The name typed (with the chat's single-key letters, digits and a space), then Tab and more text.
    fn typing(&mut self) -> SandboxResult {
        let form = self.form_now("before-typing")?;
        let (x, y) = form.name.centre();
        self.run.click(x, y)?;
        self.run.input.type_text(NAME_TYPED)?;
        thread::sleep(SETTLE);
        let state = self.run.state_in("chat")?;
        if state.reacting.is_some() || state.selected.is_some() {
            note(format!(
                "typing \"{NAME_TYPED}\" in the card's name fired a chat shortcut: selected {:?}, reacting {:?}",
                state.selected, state.reacting
            ));
        }
        self.run.input.key(keys::TAB)?;
        self.run.input.type_text(NOTE_TABBED)?;
        thread::sleep(SETTLE);
        self.shot("qa-bot-cards-typed-light.png")?;
        step("typed the name, then Tab and the note");
        Ok(())
    }

    /// A number out of its range (1 to 9) stops the send and shows its error; then a number in the range.
    fn range(&mut self) -> SandboxResult {
        let form = self.form_now("before-range")?;
        let red = danger_in(&self.run.last, PANE);
        self.replace_text(form.number, "12")?;
        let form = self.form_now("range-typed")?;
        self.click_quiet(form.send.centre(), "Send with the number 12 (max 9)")?;
        let shot = self.shot("qa-bot-cards-range-light.png")?;
        let after = danger_in(&load(&shot)?, PANE);
        if after <= red + 20 {
            note(format!(
                "Send with the number 12 (max 9): no error shows ({red} -> {after} red pixels, {})",
                shot.display()
            ));
        }
        let form = self.form_now("range-error")?;
        self.replace_text(form.number, "2")?;
        step("a number out of its range stops the send");
        Ok(())
    }

    fn replace_text(&mut self, field: Rect, text: &str) -> SandboxResult {
        let (x, y) = field.centre();
        self.run.click(x, y)?;
        let a = self.run.letter('a')?;
        self.run.input.key_with(&[Modifier::Ctrl], a)?;
        self.run.input.type_text(text)?;
        thread::sleep(Duration::from_millis(300));
        Ok(())
    }

    /// Juice and Vegetarian checked with the pointer; Green salad picked from the drop-down with the keyboard, and
    /// Escape closes the drop-down's menu.
    fn choices(&mut self) -> SandboxResult {
        let form = self.form_now("before-choices")?;
        let (juice, vegetarian) = form.checks.ok_or("Juice and Vegetarian are not on the screen")?;
        for check in [juice, vegetarian] {
            let (x, y) = check.centre();
            self.run.click(x, y)?;
        }
        let closed = self.shot("qa-bot-cards-checked-light.png")?;
        let (x, y) = form.dish.centre();
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        let open = self.shot("qa-bot-cards-dish-menu-light.png")?;
        if changed(&load(&closed)?, &load(&open)?) < 500 {
            note(format!(
                "a click on the Dish drop-down opens no menu ({})",
                open.display()
            ));
        }
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::DOWN)?;
        self.run.input.key(keys::ENTER)?;
        thread::sleep(SETTLE);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let escaped = self.shot("qa-bot-cards-dish-escaped-light.png")?;
        if changed(&load(&open)?, &load(&escaped)?) < 500 {
            note(format!(
                "Escape leaves the Dish drop-down's menu open ({})",
                escaped.display()
            ));
        }
        step("checked two boxes, picked a dish with the keyboard, closed the menu with Escape");
        Ok(())
    }

    /// Send to the slow bot: the spinner while it waits, a second click sends nothing, then the answers it got.
    fn send(&mut self) -> SandboxResult {
        let service = self.service;
        let form = self.form_now("before-send")?;
        let errors = danger_in(&self.run.last, PANE);
        if errors > 60 {
            note(format!(
                "before Send, the form still shows errors ({errors} red pixels)"
            ));
        }
        self.runtime
            .block_on(service.mock.answer_next_after("POST", "/invoke", SLOW));
        let before = self.invokes().len();
        let idle = self.shot("qa-bot-cards-before-send-light.png")?;
        let invoke = self.click_invokes(form.send.centre(), before, "Send")?;
        thread::sleep(Duration::from_millis(800));
        let busy = self.shot("qa-bot-cards-spinner-light.png")?;
        if changed_in(&load(&idle)?, &load(&busy)?, form.send.grown(4)) < 10 {
            note(format!(
                "the Send button shows no spinner while the bot waits ({})",
                busy.display()
            ));
        }
        self.run.click(form.send.centre().0, form.send.centre().1)?;
        thread::sleep(SLOW);
        let count = self.invokes().len() - before;
        if count != 1 {
            note(format!(
                "Send, then Send again while the bot waits: {count} invokes, expected 1"
            ));
        }
        expect_submit(&invoke, service);
        self.run.until("the bot's thanks", |state| {
            state
                .items
                .iter()
                .any(|item| item.text.contains("Your lunch order is in"))
        })?;
        self.shot("qa-bot-cards-sent-light.png")?;
        step("Send: the spinner, one invoke, the answers and the bot's thanks");
        Ok(())
    }

    /// Order (`Action.Execute`): its invoke, and the bot's card in the form's place.
    fn execute(&mut self) -> SandboxResult {
        let order = self.form_now("before-order")?.order;
        let before = self.invokes().len();
        let invoke = self.click_invokes(order.centre(), before, "Order")?;
        let body = &invoke["body"];
        if body["name"] != "adaptiveCard/action"
            || body["value"]["action"]["verb"] != "order"
            || body["value"]["trigger"] != "manual"
            || body["value"]["action"]["data"]["form"] != "lunch"
            || body["value"]["action"]["data"]["name"] != NAME_TYPED
        {
            note(format!("Order sent {body}"));
        }
        thread::sleep(SETTLE * 2);
        let shot = self.shot("qa-bot-cards-replaced-light.png")?;
        if boxes_in(&load(&shot)?, PANE_X, 1280)
            .iter()
            .any(|found| PANE.holds(found) && found.width() > 300)
        {
            note(format!(
                "the bot's card does not take the form's place ({})",
                shot.display()
            ));
        }
        self.dark("replaced")?;
        step("Order: the bot's card takes the form's place");
        Ok(())
    }
}

/// The Send's invoke: the answers merged into its data, Teams' token, the chat and the message.
fn expect_submit(invoke: &Value, service: &Service) {
    let body = &invoke["body"];
    let wanted = json!({
        "form": "lunch", "name": NAME_TYPED, "note": NOTE_TABBED, "count": "2", "day": "2026-11-20", "at": "12:30",
        "dish": "salad", "size": "s", "extras": "bread,juice", "veg": "yes",
    });
    if body["name"] != "message" || body["value"] != wanted {
        note(format!(
            "Send sent {} with {}; expected the answers {wanted}",
            body["name"], body["value"]
        ));
    }
    let token = service.mock.token(LAB_A).unwrap_or_default();
    if body["userAadToken"] != token.as_str() {
        note("Send's userAadToken is not the account's Teams token".to_owned());
    }
    if body["conversation"]["id"] != service.chats.form.as_str()
        || body["messageId"] != service.ids.form.as_str()
        || body["appId"] != FORM_BOT.trim_start_matches("28:")
        || invoke["taken"] != true
    {
        note(format!(
            "Send's invoke: taken {}, conversation {}, message {}, app {}",
            invoke["taken"], body["conversation"], body["messageId"], body["appId"]
        ));
    }
}

/// The form's controls, found on the screen.
#[derive(Debug)]
struct Form {
    name: Rect,
    number: Rect,
    dish: Rect,
    /// Juice and Vegetarian, while neither is checked.
    checks: Option<(Rect, Rect)>,
    send: Rect,
    order: Rect,
}

impl Form {
    fn find(shot: &image::RgbImage, card: Rect) -> SandboxResult<Self> {
        let found = boxes_in(shot, card.x0, card.x1);
        let inside: Vec<Rect> = found.into_iter().filter(|it| card.holds(it)).collect();
        let wide: Vec<Rect> = inside.iter().copied().filter(|it| it.width() > 300).collect();
        let thirds: Vec<Rect> = inside
            .iter()
            .copied()
            .filter(|it| (100..=200).contains(&it.width()))
            .collect();
        let small: Vec<Rect> = inside
            .iter()
            .copied()
            .filter(|it| it.width() <= 22 && it.height() <= 22)
            .collect();
        let buttons: Vec<Rect> = inside
            .iter()
            .copied()
            .filter(|it| (30..100).contains(&it.width()) && (18..36).contains(&it.height()))
            .collect();
        let missing =
            || format!("the form's fields: wide {wide:?}, thirds {thirds:?}, small {small:?}, buttons {buttons:?}");
        let (name, dish) = match wide.as_slice() {
            [name, .., dish] => (*name, *dish),
            _ => return Err(missing().into()),
        };
        // a checked box is drawn in the brand colour: only the boxes not checked are found
        let checks = match small.as_slice() {
            [.., juice, vegetarian] => Some((*juice, *vegetarian)),
            _ => None,
        };
        let (send, order) = match buttons.as_slice() {
            [.., send, order] => (*send, *order),
            _ => return Err(missing().into()),
        };
        Ok(Self {
            name,
            number: *thirds.first().ok_or_else(missing)?,
            dish,
            checks,
            send,
            order,
        })
    }
}

// --- the answers ---

impl Walk<'_> {
    fn answers(&mut self) -> SandboxResult {
        let service = self.service;
        let chats = &service.chats;
        self.line(&chats.message, &service.ids.message, "message", Line::Note)?;
        self.line(&chats.error, &service.ids.error, "error", Line::Red)?;
        self.line(&chats.sign_in, &service.ids.sign_in, "sign-in", Line::Opens)?;
        self.line(&chats.dialog, &service.ids.dialog, "dialog", Line::Opens)?;
        self.line(&chats.gone, &service.ids.gone, "gone", Line::Red)?;
        self.late()
    }

    /// Opens the chat of a status card and presses its Confirm; checks the line under the card.
    fn line(&mut self, chat: &str, id: &str, what: &str, kind: Line) -> SandboxResult {
        let card = self.open_card(chat, id, what)?;
        let confirm = only_button(&self.run.last, card)?;
        let before = self.invokes().len();
        self.click_invokes(confirm.centre(), before, what)?;
        thread::sleep(SETTLE);
        let answered = self
            .run
            .card(self.sandbox, &format!("qa-bot-cards-{what}-answered-light.png"))?;
        if answered.width() > card.width() + 20 {
            note(format!(
                "{what}: the card grows from {} to {} px wide when the line shows under it",
                card.width(),
                answered.width()
            ));
        }
        self.expect_line(answered, what, kind)?;
        if matches!(kind, Line::Red | Line::Opens) {
            self.dark(&format!("{what}-answered"))?;
        }
        step(&format!("{what}: the line under the card"));
        Ok(())
    }

    fn expect_line(&mut self, card: Rect, what: &str, kind: Line) -> SandboxResult {
        let band = Rect {
            x0: card.x0.saturating_sub(10),
            y0: card.y1 + 2,
            x1: card.x1,
            y1: card.y1 + 24,
        };
        let shot = &self.run.last;
        let (ink, red) = (dark_in(shot, band), danger_in(shot, band));
        let fits = match kind {
            Line::Note | Line::Opens => ink > 30 && red < 10,
            Line::Red => red > 30,
        };
        if !fits {
            note(format!(
                "{what}: under the card {ink} dark and {red} red pixels; expected a {kind:?} line"
            ));
            return Ok(());
        }
        if kind == Line::Opens {
            let opened = self.run.click_opens(self.sandbox, band.x0 + 60, band.y0 + 8)?;
            if !in_teams_on_the_web(&opened) {
                note(format!(
                    "{what}: the line opens {opened}, expected the message in Teams on the web"
                ));
            }
        }
        Ok(())
    }

    /// The bot that does not answer in time: the spinner, then "The app didn't respond".
    fn late(&mut self) -> SandboxResult {
        let service = self.service;
        let card = self.open_card(&service.chats.late, &service.ids.late, "late")?;
        let confirm = only_button(&self.run.last, card)?;
        self.runtime
            .block_on(service.mock.answer_next_after("POST", "/invoke", NO_ANSWER));
        let before = self.invokes().len();
        self.click_invokes(confirm.centre(), before, "late")?;
        thread::sleep(Duration::from_secs(2));
        self.shot("qa-bot-cards-late-waiting-light.png")?;
        thread::sleep(NO_ANSWER);
        let card = self.run.card(self.sandbox, "qa-bot-cards-late-answered-light.png")?;
        self.expect_line(card, "late", Line::Red)?;
        step("a bot that does not answer in time");
        Ok(())
    }
}

/// A link to a message in Teams on the web.
fn in_teams_on_the_web(link: &str) -> bool {
    [
        "https://teams.cloud.microsoft/l/message/",
        "https://teams.microsoft.com/l/message/",
    ]
    .iter()
    .any(|web| link.starts_with(web))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    /// The bot's message: dark text.
    Note,
    /// An error: Teams' red.
    Red,
    /// Sign-in or dialog: dark text, and a click opens Teams on the web.
    Opens,
}

fn only_button(shot: &image::RgbImage, card: Rect) -> SandboxResult<Rect> {
    boxes_in(shot, card.x0, card.x1)
        .into_iter()
        .rfind(|found| card.holds(found) && (18..36).contains(&found.height()) && found.width() >= 30)
        .ok_or_else(|| format!("no button in the card {card:?}").into())
}

// --- imBack, messageBack, suggested actions ---

impl Walk<'_> {
    fn backs(&mut self) -> SandboxResult {
        let service = self.service;
        let card = self.open_card(&service.chats.back, &service.ids.back, "backs")?;
        let buttons: Vec<Rect> = boxes_in(&self.run.last, card.x0, card.x1)
            .into_iter()
            .filter(|found| card.holds(found) && (18..36).contains(&found.height()) && found.width() >= 30)
            .collect();
        let [im, back] = buttons.as_slice() else {
            return Err(format!("the imBack card's buttons: {buttons:?}").into());
        };
        self.click_quiet(im.centre(), "imBack")?;
        self.run.until("the imBack's message", |state| {
            state
                .items
                .iter()
                .any(|item| item.own && item.text == "I'll take the soup")
        })?;
        let card = self.run.card(self.sandbox, "qa-bot-cards-imback-light.png")?;
        let back = boxes_in(&self.run.last, card.x0, card.x1)
            .into_iter()
            .rfind(|found| card.holds(found) && (18..36).contains(&found.height()) && found.width() >= 30)
            .unwrap_or(*back);
        let before = self.invokes().len();
        let invoke = self.click_invokes(back.centre(), before, "messageBack")?;
        // the data goes whole, its msteams in it (docs/protocol/bots.md, own design)
        if invoke["body"]["text"] != "salad" || invoke["body"]["value"]["msteams"]["value"]["dish"] != "salad" {
            note(format!("messageBack sent {}", invoke["body"]));
        }
        self.run.until("the messageBack's message", |state| {
            state.items.iter().any(|item| item.own && item.text == "Salad, please")
        })?;
        self.shot("qa-bot-cards-messageback-light.png")?;
        step("imBack posts the user's message; messageBack posts its display text and asks the bot");
        Ok(())
    }

    fn suggested(&mut self) -> SandboxResult {
        let service = self.service;
        self.run.open(AC_CHAT)?;
        self.run.until("the suggesting message", |state| {
            state.item(&service.ids.suggest).is_some()
        })?;
        thread::sleep(SETTLE);
        let shot = self.shot("qa-bot-cards-suggested-light.png")?;
        self.dark("suggested")?;
        let image = load(&shot)?;
        let card = find_card(&image).ok_or("no card for the suggested actions")?;
        // from the card down: the header's border over the pane's fill and the box's grey top make one false box
        let below: Vec<Rect> = boxes_below(&image, PANE_X, 1280, card.y1)
            .into_iter()
            .filter(|found| PANE.holds(found) && (16..36).contains(&found.height()) && found.width() >= 40)
            .collect();
        let Some(first) = below.iter().min_by_key(|found| found.x0).copied() else {
            note(format!(
                "no suggested actions over the compose box ({})",
                shot.display()
            ));
            return Ok(());
        };
        self.click_quiet(first.centre(), "a suggested imBack")?;
        self.run.until("the suggested answer", |state| {
            state.items.iter().any(|item| item.own && item.text == "Tomato soup")
        })?;
        thread::sleep(SETTLE);
        let after = self.shot("qa-bot-cards-suggested-answered-light.png")?;
        let left = boxes_below(&load(&after)?, PANE_X, 1280, 600)
            .into_iter()
            .filter(|found| PANE.holds(found) && (16..36).contains(&found.height()) && found.width() >= 40)
            .count();
        if left > 0 {
            note(format!(
                "the suggested actions stay after the user answered ({})",
                after.display()
            ));
        }
        step("suggested actions: a click posts the answer, and they go");
        Ok(())
    }
}

// --- refresh ---

impl Walk<'_> {
    fn refresh(&mut self) -> SandboxResult {
        let service = self.service;
        let before = self.invokes().len();
        let card = self.open_card(&service.chats.refresh, &service.ids.refresh, "refresh")?;
        thread::sleep(Duration::from_secs(3));
        let automatic: Vec<Value> = self.invokes()[before..].to_vec();
        let by_itself = automatic
            .iter()
            .filter(|invoke| invoke["body"]["value"]["trigger"] == "automatic")
            .count();
        if by_itself != 1 || automatic.len() != 1 {
            note(format!(
                "a card whose refresh names the account: {automatic:?}; expected one automatic invoke"
            ));
        }
        let _ = card;
        let card = self.run.card(self.sandbox, "qa-bot-cards-refreshed-light.png")?;
        let before = self.invokes().len();
        self.more_options(card)?;
        let menu = self.shot("qa-bot-cards-refresh-menu-light.png")?;
        self.dark("refresh-menu")?;
        let Some(row) = self.run.menu_last_action_row(&menu)? else {
            note(format!(
                "no More options menu on the card's message ({})",
                menu.display()
            ));
            return Ok(());
        };
        let invoke = self.click_invokes(row, before, "Refresh card")?;
        if invoke["body"]["value"]["trigger"] != "manual" || invoke["body"]["value"]["action"]["verb"] != "status" {
            note(format!("Refresh card sent {}", invoke["body"]));
        }
        step("refresh: once by itself, then by hand");
        Ok(())
    }

    /// Hovers the card's message and clicks the hover bar's More options.
    fn more_options(&mut self, card: Rect) -> SandboxResult {
        let still = self.shot("qa-bot-cards-refresh-still-light.png")?;
        self.run.input.move_to(card.x0 + 40, card.y0 + 20)?;
        thread::sleep(SETTLE);
        let png = self.sandbox.screenshot("qa-bot-cards-hover.png", false)?;
        fs::copy(&png, shots_dir()?.join("qa-bot-cards-refresh-hover-light.png"))?;
        let hover = load(&png)?;
        let region = Rect {
            x0: PANE_X,
            y0: card.y0.saturating_sub(70),
            x1: 1280,
            y1: card.y0 + 20,
        };
        let Some(bar) = changed_box(&load(&still)?, &hover, region) else {
            note(format!("no hover bar over the card's message ({})", png.display()));
            return Ok(());
        };
        self.run
            .input
            .click(bar.x1.saturating_sub(14), bar.y0 + bar.height() / 2, keys::BTN_LEFT)?;
        self.run.last_menu_base = Some(hover);
        thread::sleep(SETTLE);
        Ok(())
    }
}

// --- many inputs, a link's app card ---

impl Walk<'_> {
    fn long(&mut self) -> SandboxResult {
        let service = self.service;
        self.open_card(&service.chats.long, &service.ids.long, "long")?;
        // a form card is at most 448 px wide from the avatar's column: right of it is the chat's empty background,
        // up to the window's frame at x 1272
        let right = Rect {
            x0: 965,
            y0: PANE.y0,
            x1: 1272,
            y1: PANE.y1,
        };
        let outside = dark_in(&self.run.last, right);
        if outside > 50 {
            note(format!(
                "long input labels do not wrap: {outside} text pixels right of the card's edge (qa-bot-cards-long-light.png)"
            ));
        }
        self.dark("long")?;
        let (x, y) = (PANE_X + 300, 400);
        self.run.input.scroll(x, y, -10)?;
        thread::sleep(SETTLE);
        self.shot("qa-bot-cards-long-top-light.png")?;
        self.dark("long-top")?;
        step("many inputs and long labels (shots)");
        Ok(())
    }

    fn app_card(&mut self) -> SandboxResult {
        let service = self.service;
        let before = self.invokes().len();
        self.run.open(&service.chats.app)?;
        self.run.until("the link's app card", |state| {
            state.item(&service.ids.app).is_some_and(|item| item.app_cards > 0)
        })?;
        thread::sleep(SETTLE);
        let shot = self.shot("qa-bot-cards-app-card-light.png")?;
        let image = load(&shot)?;
        if let Some(card) = find_card(&image) {
            let opened = self.run.click_opens(self.sandbox, card.x0 + 100, card.y0 + 120)?;
            step(&format!("the app card's click opened {opened}"));
        } else {
            step(&format!(
                "no app card found on the screen to click ({})",
                shot.display()
            ));
        }
        thread::sleep(QUIET);
        if self.invokes().len() != before {
            note("a link's app card asked a bot".to_owned());
        }
        step("a link's app card asks no bot");
        Ok(())
    }
}

// --- read-only mode, a cloud without apps ---

fn read_only(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir, "read-only")?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service,
        };
        let before = ctx.invokes().len();
        let card = ctx.open_card(&service.chats.form, &service.ids.form, "read-only")?;
        ctx.dark("read-only")?;
        let shot = ctx.run.last.clone();
        let boxes: Vec<Rect> = boxes_in(&shot, card.x0, card.x1)
            .into_iter()
            .filter(|found| card.holds(found))
            .collect();
        if let Some(name) = boxes.iter().find(|found| found.width() > 300).copied() {
            let (x, y) = name.centre();
            ctx.run.click(x, y)?;
            thread::sleep(SETTLE);
            let clicked = load(&ctx.shot("qa-bot-cards-read-only-clicked-light.png")?)?;
            ctx.run.click(x, y)?;
            ctx.run.input.type_text("typed")?;
            thread::sleep(SETTLE);
            let after = ctx.shot("qa-bot-cards-read-only-typed-light.png")?;
            if changed_in(&clicked, &load(&after)?, name) > 40 {
                note(format!(
                    "read-only: typing goes into the card's name field ({})",
                    after.display()
                ));
            }
        }
        if let Some(send) = boxes
            .iter()
            .filter(|found| (18..36).contains(&found.height()) && (30..140).contains(&found.width()))
            .min_by_key(|found| (std::cmp::Reverse(found.y0), found.x0))
            .copied()
        {
            let opened = ctx.run.click_opens(sandbox, send.centre().0, send.centre().1)?;
            if !in_teams_on_the_web(&opened) {
                note(format!(
                    "read-only: Send opens {opened}, expected the message in Teams on the web"
                ));
            }
        } else {
            note("read-only: no Send button found".to_owned());
        }
        ctx.run.open(&service.chats.refresh)?;
        ctx.run.open(AC_CHAT)?;
        thread::sleep(Duration::from_secs(2));
        ctx.shot("qa-bot-cards-read-only-suggested-light.png")?;
        if ctx.invokes().len() != before {
            note("read-only: a bot was asked".to_owned());
        }
        step("read-only: inputs take nothing, buttons open Teams on the web, no bot is asked");
        Ok(())
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

fn no_apps(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir, "no-apps")?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            runtime,
            service,
        };
        let before = ctx.invokes().len();
        let card = ctx.open_card(&service.chats.form, &service.ids.form, "no-apps")?;
        for found in boxes_in(&ctx.run.last.clone(), card.x0, card.x1)
            .into_iter()
            .filter(|found| card.holds(found) && (18..36).contains(&found.height()) && found.width() < 140)
        {
            let (x, y) = found.centre();
            ctx.run.click(x, y)?;
        }
        ctx.open_card(&service.chats.message, &service.ids.message, "no-apps-status")?;
        ctx.run.open(&service.chats.refresh)?;
        ctx.run.open(AC_CHAT)?;
        thread::sleep(Duration::from_secs(2));
        ctx.shot("qa-bot-cards-no-apps-suggested-light.png")?;
        if ctx.invokes().len() != before {
            note("a cloud without apps: a bot was asked".to_owned());
        }
        step("a cloud that serves no apps: no bot is asked");
        Ok(())
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
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
    const fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    const fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    const fn centre(&self) -> (u32, u32) {
        (self.x0 + self.width() / 2, self.y0 + self.height() / 2)
    }

    const fn holds(&self, other: &Self) -> bool {
        other.x0 + 10 >= self.x0 && other.x1 <= self.x1 + 10 && other.y0 >= self.y0 && other.y1 <= self.y1
    }

    fn grown(&self, by: u32) -> Self {
        Self {
            x0: self.x0.saturating_sub(by),
            y0: self.y0.saturating_sub(by),
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
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

/// The lowest card in the chat's pane.
fn find_card(shot: &image::RgbImage) -> Option<Rect> {
    cards_in(shot).into_iter().max_by_key(|card| card.y0)
}

/// The tallest card in the chat's pane.
fn tallest_card(shot: &image::RgbImage) -> Option<Rect> {
    cards_in(shot).into_iter().max_by_key(Rect::height)
}

/// The cards (and white bubbles) in the chat's pane: a top border, a run of [`BORDER`] 120 to 470 px long over
/// [`SURFACE`], down to the bottom border; none inside another.
fn cards_in(shot: &image::RgbImage) -> Vec<Rect> {
    let mut found = Vec::new();
    for y in 0..shot.height().saturating_sub(4) {
        for (x0, x1) in runs(shot, y, PANE_X, shot.width(), |pixel| near(pixel, BORDER, 3)) {
            let mid = x0 + (x1 - x0) / 2;
            if !(120..=470).contains(&(x1 - x0)) || at(shot, mid, y + 2) != SURFACE {
                continue;
            }
            if let Some(bottom) = (y + 2..shot.height()).find(|&below| near(at(shot, x0 + 4, below), BORDER, 3)) {
                found.push(Rect {
                    x0: x0.saturating_sub(8),
                    y0: y,
                    x1: x1 + 8,
                    y1: bottom + 1,
                });
            }
        }
    }
    // a field's bottom edge over the card's surface is no card: keep those inside no other
    let outer: Vec<Rect> = found
        .iter()
        .copied()
        .filter(|card| !found.iter().any(|other| other != card && other.holds(card)))
        .collect();
    outer
}

/// The runs of row `y` from `from` to `to` whose pixels `taken` takes.
fn runs(shot: &image::RgbImage, y: u32, from: u32, to: u32, taken: impl Fn([u8; 3]) -> bool) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    let mut x = from;
    let to = to.min(shot.width());
    while x < to {
        if !taken(at(shot, x, y)) {
            x += 1;
            continue;
        }
        let start = x;
        while x < to && taken(at(shot, x, y)) {
            x += 1;
        }
        found.push((start, x));
    }
    found
}

/// A field's edge: its [`BORDER`], or the [`RING`] of the focused one.
fn edge(pixel: [u8; 3]) -> bool {
    near(pixel, BORDER, 4) || near(pixel, RING, 4)
}

/// The fields, buttons and check boxes between `from` and `to`: boxes with an [`edge`] and a [`FILL`] (or an
/// [`OFF_FILL`]) inside, in
/// order down the screen, then across.
fn boxes_in(shot: &image::RgbImage, from: u32, to: u32) -> Vec<Rect> {
    boxes_below(shot, from, to, 0)
}

/// The [`boxes_in`] whose top edge is at row `top` or below it.
fn boxes_below(shot: &image::RgbImage, from: u32, to: u32, top: u32) -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    for y in top..shot.height().saturating_sub(3) {
        for (x0, x1) in runs(shot, y, from, to, edge) {
            let mid = x0 + (x1 - x0) / 2;
            let inside = at(shot, mid, y + 2);
            if x1 - x0 < 6 || !(near(inside, FILL, 3) || near(inside, OFF_FILL, 2)) {
                continue;
            }
            let known = |it: &Rect| it.x0 <= mid && mid < it.x1 && it.y0 <= y && y < it.y1;
            if found.iter().any(known) {
                continue;
            }
            let Some(bottom) = (y + 2..shot.height()).find(|&below| edge(at(shot, mid, below))) else {
                continue;
            };
            found.push(Rect {
                x0,
                y0: y,
                x1,
                y1: bottom + 1,
            });
        }
    }
    found.sort_by_key(|found| (found.y0, found.x0));
    found
}

fn danger_in(shot: &image::RgbImage, region: Rect) -> usize {
    count_in(shot, region, |pixel| near(pixel, DANGER, 40))
}

/// The pixels of text (dark on Teams Light) in `region`.
fn dark_in(shot: &image::RgbImage, region: Rect) -> usize {
    count_in(shot, region, |pixel| {
        pixel.iter().map(|&value| u32::from(value)).sum::<u32>() < 450
    })
}

fn count_in(shot: &image::RgbImage, region: Rect, taken: impl Fn([u8; 3]) -> bool) -> usize {
    (region.y0..region.y1.min(shot.height()))
        .flat_map(|y| (region.x0..region.x1.min(shot.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| taken(at(shot, x, y)))
        .count()
}

fn changed(before: &image::RgbImage, after: &image::RgbImage) -> usize {
    let whole = Rect {
        x0: 0,
        y0: 0,
        x1: before.width(),
        y1: before.height(),
    };
    changed_in(before, after, whole)
}

fn changed_in(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> usize {
    (region.y0..region.y1.min(before.height()))
        .flat_map(|y| (region.x0..region.x1.min(before.width())).map(move |x| (x, y)))
        .filter(|&(x, y)| !near(at(before, x, y), at(after, x, y), 8))
        .count()
}

/// The bounds of the pixels that changed in `region`.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in region.y0..region.y1.min(before.height()) {
        for x in region.x0..region.x1.min(before.width()) {
            if near(at(before, x, y), at(after, x, y), 8) {
                continue;
            }
            bounds = Some(bounds.map_or(
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
            ));
        }
    }
    bounds
}

// --- the service ---

/// The chats the walk opens.
struct Chats {
    form: String,
    message: String,
    error: String,
    sign_in: String,
    dialog: String,
    gone: String,
    late: String,
    back: String,
    refresh: String,
    long: String,
    app: String,
}

/// The ids of the messages the walk reads.
struct Ids {
    form: String,
    message: String,
    error: String,
    sign_in: String,
    dialog: String,
    gone: String,
    late: String,
    back: String,
    suggest: String,
    refresh: String,
    long: String,
    app: String,
}

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    chats: Chats,
    ids: Ids,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        script(&mock)?;
        let group = |topic: &str| mock.add_group(topic, &[LAB_A, LAB_B]).ok_or("no group made");
        let chats = Chats {
            form: group("Made-up lunch form")?,
            message: group("Made-up bot message")?,
            error: group("Made-up bot error")?,
            sign_in: group("Made-up bot sign-in")?,
            dialog: group("Made-up bot dialog")?,
            gone: group("Made-up bot gone")?,
            late: group("Made-up bot late")?,
            back: group("Made-up quick answers")?,
            refresh: group("Made-up lunch status")?,
            long: group("Made-up long survey")?,
            app: group("Made-up video link")?,
        };
        let post = |chat: &str, bot: &str, activity: &Value| post_card(&mock, chat, bot, activity);
        let ids = Ids {
            form: post(&chats.form, FORM_BOT, &adaptive(&form_card()))?,
            message: post(&chats.message, MESSAGE_BOT, &adaptive(&status_card(None)))?,
            error: post(&chats.error, ERROR_BOT, &adaptive(&status_card(None)))?,
            sign_in: post(&chats.sign_in, SIGN_IN_BOT, &adaptive(&status_card(None)))?,
            dialog: post(&chats.dialog, DIALOG_BOT, &adaptive(&status_card(None)))?,
            gone: post(&chats.gone, GONE_BOT, &adaptive(&status_card(None)))?,
            late: post(&chats.late, LATE_BOT, &adaptive(&status_card(None)))?,
            back: post(&chats.back, BACK_BOT, &adaptive(&backs_card()))?,
            suggest: post(AC_CHAT, SUGGEST_BOT, &suggesting())?,
            refresh: post(&chats.refresh, REFRESH_BOT, &adaptive(&status_card(Some(LAB_A))))?,
            long: post(&chats.long, LONG_BOT, &adaptive(&long_card()))?,
            app: app_card_message(&mock, &chats.app)?,
        };
        let _ = mock.take_events(LAB_A);
        Ok(Self {
            mock,
            trouter,
            chats,
            ids,
        })
    }
}

/// What each made-up bot answers.
fn script(mock: &ChatServiceMock) -> SandboxResult {
    let answer = |name: &str| {
        teams_mock::bot_answer(name)
            .map(BotReply::answer)
            .ok_or_else(|| format!("no fixture answer {name}"))
    };
    let thanks = BotReply::done().posting("RichText/Html", "<p>Thanks! Your lunch order is in.</p>");
    let confirmed = BotReply::answer(json!({
        "statusCode": 200, "type": ADAPTIVE, "value": confirmed_card(),
    }));
    mock.script_bot(
        FORM_BOT,
        "Lunch Bot",
        &[("message", thanks), ("adaptiveCard/action", confirmed)],
    );
    mock.script_bot(
        MESSAGE_BOT,
        "Lunch Bot",
        &[("adaptiveCard/action", answer("executeMessage")?)],
    );
    mock.script_bot(ERROR_BOT, "Lunch Bot", &[("adaptiveCard/action", answer("error")?)]);
    mock.script_bot(SIGN_IN_BOT, "Lunch Bot", &[("adaptiveCard/action", answer("signIn")?)]);
    mock.script_bot(DIALOG_BOT, "Lunch Bot", &[("adaptiveCard/action", answer("dialog")?)]);
    mock.script_bot(
        LATE_BOT,
        "Lunch Bot",
        &[("adaptiveCard/action", answer("executeMessage")?)],
    );
    mock.script_bot(BACK_BOT, "Lunch Bot", &[("message", BotReply::done())]);
    mock.script_bot(SUGGEST_BOT, "Lunch Bot", &[("message", BotReply::done())]);
    let ready = BotReply::answer(json!({
        "statusCode": 200, "type": ADAPTIVE, "value": refreshed_card(),
    }));
    mock.script_bot(REFRESH_BOT, "Lunch Bot", &[("adaptiveCard/action", ready)]);
    Ok(())
}

/// `bot` posts a card message carrying `activity` to `chat`, as the chat service delivers a bot's card; its id.
fn post_card(mock: &ChatServiceMock, chat: &str, bot: &str, activity: &Value) -> SandboxResult<String> {
    let b64 = STANDARD.encode(serde_json::to_vec(activity)?);
    let content = format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
    );
    mock.add_message(chat, (bot, "Lunch Bot"), "RichText/Media_Card", &content)
        .ok_or_else(|| format!("the card was not posted to {chat}").into())
}

fn adaptive(card: &Value) -> Value {
    json!({"type": "message/card", "attachments": [{"contentType": ADAPTIVE, "content": card}]})
}

/// The order form: every kind of input, a plain submit and an `Action.Execute`.
fn form_card() -> Value {
    json!({
        "type": "AdaptiveCard", "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Order lunch", "weight": "bolder", "size": "medium"},
            {"type": "Input.Text", "id": "name", "label": "Your name", "isRequired": true,
             "placeholder": "First and last name", "errorMessage": "Enter your name"},
            {"type": "Input.Text", "id": "note", "label": "Note", "isMultiline": true, "placeholder": "Anything else?"},
            {"type": "ColumnSet", "columns": [
                {"type": "Column", "width": "stretch", "items": [
                    {"type": "Input.Number", "id": "count", "label": "How many", "min": 1, "max": 9, "value": 1,
                     "errorMessage": "Pick 1 to 9"}
                ]},
                {"type": "Column", "width": "stretch", "items": [
                    {"type": "Input.Date", "id": "day", "label": "Day", "value": "2026-11-20"}
                ]},
                {"type": "Column", "width": "stretch", "items": [
                    {"type": "Input.Time", "id": "at", "label": "Time", "value": "12:30"}
                ]}
            ]},
            {"type": "Input.ChoiceSet", "id": "dish", "label": "Dish", "value": "soup", "choices": [
                {"title": "Tomato soup", "value": "soup"}, {"title": "Green salad", "value": "salad"},
                {"title": "Bean stew", "value": "stew"}
            ]},
            {"type": "Input.ChoiceSet", "id": "size", "label": "Size", "style": "expanded", "value": "s", "choices": [
                {"title": "Small", "value": "s"}, {"title": "Large", "value": "l"}
            ]},
            {"type": "Input.ChoiceSet", "id": "extras", "label": "Extras", "style": "expanded",
             "isMultiSelect": true, "value": "bread", "choices": [
                {"title": "Bread", "value": "bread"}, {"title": "Juice", "value": "juice"}
            ]},
            {"type": "Input.Toggle", "id": "veg", "title": "Vegetarian", "valueOn": "yes", "valueOff": "no"}
        ],
        "actions": [
            {"type": "Action.Submit", "title": "Send", "data": {"form": "lunch"}},
            {"type": "Action.Execute", "title": "Order", "verb": "order", "data": {"form": "lunch"}}
        ]
    })
}

/// A status card with one `Action.Execute`; with a `refresh` that names `user`, when given.
fn status_card(user: Option<&str>) -> Value {
    let mut card = json!({
        "type": "AdaptiveCard", "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Lunch order", "weight": "bolder", "size": "medium"},
            {"type": "FactSet", "facts": [{"title": "Dish", "value": "Tomato soup"}, {"title": "Status", "value": "Waiting"}]}
        ],
        "actions": [{"type": "Action.Execute", "title": "Confirm", "verb": "confirm"}]
    });
    if let Some(user) = user {
        card["refresh"] = json!({"action": {"type": "Action.Execute", "verb": "status"}, "userIds": [user]});
    }
    card
}

fn confirmed_card() -> Value {
    json!({
        "type": "AdaptiveCard", "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Lunch order", "weight": "bolder", "size": "medium"},
            {"type": "FactSet", "facts": [{"title": "Dish", "value": "Green salad"}, {"title": "Status", "value": "Confirmed"}]},
            {"type": "TextBlock", "text": "Confirmed by Synthetic Ay", "isSubtle": true, "size": "small"}
        ]
    })
}

/// The status the refresh bot answers: ready, with the same refresh.
fn refreshed_card() -> Value {
    let mut card = status_card(Some(LAB_A));
    card["body"][1]["facts"][1]["value"] = json!("Ready");
    card
}

/// An `imBack` and a `messageBack` button.
fn backs_card() -> Value {
    json!({
        "type": "AdaptiveCard", "version": "1.5",
        "body": [{"type": "TextBlock", "text": "What would you like today?", "wrap": true}],
        "actions": [
            {"type": "Action.Submit", "title": "Soup", "data": {"msteams": {"type": "imBack", "value": "I'll take the soup"}}},
            {"type": "Action.Submit", "title": "Salad", "data": {"msteams": {"type": "messageBack",
                "displayText": "Salad, please", "text": "salad", "value": {"dish": "salad"}}}}
        ]
    })
}

fn suggesting() -> Value {
    json!({
        "type": "message/card",
        "attachments": [{"contentType": ADAPTIVE, "content": {
            "type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Which dish would you like?", "wrap": true}]
        }}],
        "suggestedActions": {"actions": [
            {"type": "imBack", "title": "Tomato soup", "value": "Tomato soup"},
            {"type": "imBack", "title": "Green salad", "value": "Green salad"},
            {"type": "imBack", "title": "Nothing today", "value": "Nothing today"}
        ]}
    })
}

/// Many inputs, long labels and long choices.
fn long_card() -> Value {
    let mut body = vec![json!({"type": "TextBlock", "text": "A made-up survey", "weight": "bolder", "size": "medium"})];
    for n in 1..=8 {
        body.push(
            json!({"type": "Input.Text", "id": format!("q{n}"), "label": format!("{n}. {LONG_LABEL}"),
            "isRequired": n % 2 == 0, "placeholder": "A made-up placeholder that is also rather long for a field"}),
        );
    }
    body.push(json!({"type": "Input.ChoiceSet", "id": "pick", "label": LONG_LABEL, "style": "expanded", "choices": [
        {"title": "A made-up choice whose title is long enough to wrap in the card's width at least once", "value": "1"},
        {"title": "Short", "value": "2"}
    ]}));
    body.push(json!({"type": "Input.Toggle", "id": "agree", "title": LONG_LABEL, "valueOn": "y", "valueOff": "n"}));
    json!({
        "type": "AdaptiveCard", "version": "1.5", "body": body,
        "actions": [{"type": "Action.Submit", "title": "Send the made-up answers", "data": {}}]
    })
}

/// b's message with a YouTube link and its app card (`properties.cards`), as Teams' compose box sends it.
fn app_card_message(mock: &ChatServiceMock, chat: &str) -> SandboxResult<String> {
    let link = "https://youtu.be/Qa1_bCd-Ef9";
    let id = mock
        .post_as(LAB_B, chat, &format!("<p><a href=\"{link}\">{link}</a></p>"))
        .ok_or("a post refused")?;
    let cmid = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let open = json!({"type": "Action.Submit", "data": {"msteams": {"type": "invoke", "value": {
        "type": "tab/tabInfoAction", "tabInfo": {"contentUrl": "https://tabs.teams.microsoft.com/made-up",
        "websiteUrl": "https://www.youtube.com/watch?v=Qa1_bCd-Ef9", "entityId": "00000000-0000-4000-8000-0000000004e9"}}}}});
    let mut button = open.clone();
    button["title"] = json!("Open");
    let cards = json!([{
        "appId": "com.microsoft.teamspace.tab.youtube", "cardClientId": "app-preview-card-made-up-qa",
        "appName": "YouTube", "contentType": ADAPTIVE,
        "content": {"type": "AdaptiveCard", "version": "1.6", "selectAction": open, "body": [
            {"type": "Media", "sources": [{"mimeType": "video/mp4", "url": "https://www.youtube.com/watch?v=Qa1_bCd-Ef9"}]},
            {"type": "TextBlock", "text": "A made-up video", "size": "large", "weight": "bolder", "wrap": true},
            {"type": "ActionSet", "actions": [button]}
        ]},
    }]);
    if mock.change_message(chat, &cmid, |message| {
        message["properties"]["cards"] = json!(cards.to_string());
    }) {
        Ok(id)
    } else {
        Err("the app card's message is not in the mock".into())
    }
}

/// Every request to a bot's invoke the mock got: its path, whether the mock took it (it passed the token and member
/// checks), and its body.
fn invokes_of(runtime: &tokio::runtime::Runtime, mock: &ChatServiceMock) -> Vec<Value> {
    let recorded = mock.bot_invokes();
    runtime
        .block_on(mock.received_requests())
        .iter()
        .filter(|request| request.url.path().contains("/agents/") && request.url.path().ends_with("/invoke"))
        .map(|request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
            let kept = recorded.iter().any(|record| record.body == body);
            json!({"path": request.url.path(), "taken": kept, "body": body})
        })
        .collect()
}

// --- what the app shows ---

/// A message's row as the child tells it.
#[derive(Debug, serde::Deserialize)]
struct Item {
    id: String,
    own: bool,
    kind: String,
    text: String,
    app_cards: usize,
}

/// The open view as the child tells it.
#[derive(Debug, serde::Deserialize)]
struct State {
    items: Vec<Item>,
    selected: Option<String>,
    reacting: Option<String>,
}

impl State {
    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<Value>,
    seen: Vec<Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line, not seen before, that `found` takes, named `what` in the error.
    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; the last seen {:?}", self.seen.last()))?;
            self.seen.push(line.clone());
            if found(&line) {
                return Ok(line);
            }
        }
    }
}

/// The app running in the sandbox: its process, its view trace, its control socket and the user's pointer.
struct AppRun {
    running: Running,
    trace: TraceReader,
    ctl: PathBuf,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar, away from every card and button.
    rest: (u32, u32),
    asked: u32,
    /// The last screenshot.
    last: image::RgbImage,
    /// The screen before a menu opened, to find the menu.
    last_menu_base: Option<image::RgbImage>,
}

impl AppRun {
    /// Starts the app on `service` with its store and settings in `dir`, in `mode`; once its window is drawn.
    fn launch(sandbox: &Sandbox, service: &Service, dir: &Path, mode: &str) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let token = service.mock.token(LAB_A).ok_or("the mock's token for a")?;
        let origin = service.mock.origin();
        let dir_text = dir.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "app"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &origin),
                (TROUTER_ENV, service.trouter.origin()),
                (TOKEN_ENV, &token),
                (DIR_ENV, &dir_text),
                (MODE_ENV, mode),
                (teams_ui::bots_flag::BOTS_ENV, "1"),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let trace = TraceReader::start(reader);
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let ctl = dir.join("ctl.sock");
        wait_until(WAIT, || Client::connect(&ctl, Timeouts::default()).ok()).ok_or("no control socket")?;
        let mut run = Self {
            running,
            trace,
            ctl,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (200, 24),
            asked: 0,
            last: image::RgbImage::new(1, 1),
            last_menu_base: None,
        };
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

    /// `ngteamsctl open chat <chat>`, then the trace's `chat opened`.
    fn open(&mut self, chat: &str) -> SandboxResult {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        client.request(
            None,
            Request::Open {
                target: OpenTarget::Chat { chat: chat.to_owned() },
            },
        )?;
        self.trace.next_where(&format!("chat {chat} opened"), |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
        Ok(())
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

    fn state_in(&mut self, view: &str) -> SandboxResult<State> {
        let answer = self.question(&format!("state {view}"))?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state of the chat that `wanted` takes, named `what` in the error.
    fn until(&mut self, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state_in("chat")?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state:#?}").into());
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
        fs::copy(&png, shots_dir()?.join(name))?;
        self.last = load(&png)?;
        Ok(png)
    }

    /// Waits until a card shows in the chat's pane; the card, the shot kept as `name`.
    fn card(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<Rect> {
        let start = Instant::now();
        loop {
            thread::sleep(SETTLE);
            let shot = self.shot(sandbox, name)?;
            if let Some(card) = find_card(&self.last) {
                return Ok(card);
            }
            if start.elapsed() > WAIT {
                return Err(format!("no card on the screen ({})", shot.display()).into());
            }
        }
    }

    /// Clicks (`x`, `y`) as a user does, then rests the pointer.
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

    /// The last row of the menu that opened since [`Self::last_menu_base`], in `shot`, over its "Customize" (a hairline
    /// and a row at the menu's foot): where to click it.
    fn menu_last_action_row(&mut self, shot: &Path) -> SandboxResult<Option<(u32, u32)>> {
        let Some(base) = self.last_menu_base.take() else {
            return Ok(None);
        };
        let now = load(shot)?;
        let whole = Rect {
            x0: PANE_X.saturating_sub(200),
            y0: 0,
            x1: now.width(),
            y1: now.height(),
        };
        Ok(changed_box(&base, &now, whole)
            .filter(|menu| menu.height() > 40)
            .map(|menu| (menu.x0 + 60, menu.y1.saturating_sub(18 + CUSTOMIZE_FOOT))))
    }

    /// Clicks (`x`, `y`) as a user does; the link the app asked the opener for.
    fn click_opens(&mut self, sandbox: &Sandbox, x: u32, y: u32) -> SandboxResult<String> {
        let before = sandbox.opened()?.len();
        self.input.click(x, y, keys::BTN_LEFT)?;
        let opened = wait_until(WAIT, || sandbox.opened().ok()?.get(before).cloned())
            .ok_or_else(|| format!("a click at ({x}, {y}) opened nothing"))?;
        self.input.move_to(self.rest.0, self.rest.1)?;
        opened
            .args
            .last()
            .cloned()
            .ok_or_else(|| format!("the opener got no address: {opened:?}").into())
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa-bot-cards-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits through the tray menu, as a user does; once the app ended and released its engine. Without a tray item
    /// (the read-only run shows none here) the app is stopped.
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
        self.running
            .line_where(WAIT, |line| line == "engine-released")
            .map_err(|error| self.running.fail(&format!("the engine wasn't released: {error:?}")))?;
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

/// a's engine on the parent's mock, its store in `dir`: read-only, or on a cloud that serves no apps, as `mode` says.
fn engine_on_the_mock(dir: &Path, mode: &str) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || -> SandboxResult<HttpTransport<Tokens>> {
        let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)?;
        Ok(if mode == "read-only" {
            transport.read_only()
        } else {
            transport
        })
    };
    let transport = Arc::new(http()?);
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
    let capabilities = if mode == "no-apps" {
        Capabilities {
            apps: false,
            ..Capabilities::ALL
        }
    } else {
        Capabilities::ALL
    };
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(http()?.with_ams(AMS_BASE)?.with_csa(CSA_BASE)?)),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions {
            capabilities,
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
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

/// Shows the theme `file` names (`light` or `dark`) once it changes, as the user picks one in Settings, and says
/// `theme <name>` once it is applied.
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
            // the frames in the new theme
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
    let mode = std::env::var(MODE_ENV).unwrap_or_default();
    let engine = engine_on_the_mock(&dir, &mode)?;
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let runner = support::runner(&dir)?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ctl = dir.join("ctl.sock");
    let theme_file = dir.join(THEME_FILE);
    let ask = dir.join(ASK);
    let read_only = mode == "read-only";
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
                read_only,
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
    let store = Store::open(&support::store_file(&dir, Alias::A))?;
    if store.engine_lock().is_ok() {
        say("engine-released");
    }
    Ok(())
}

/// Answers the parent's questions as they come.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = window
                .update(cx, |kit, _, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, cx: &App) -> String {
    let (verb, _) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "state" => shell.read(cx).chat().map_or_else(
            || json!({"items": [], "selected": null, "reacting": null}),
            |view| state_of(view.read(cx)),
        ),
        other => json!(format!("unknown {other}")),
    }
    .to_string()
}

/// The view's rows, as [`State`] reads them.
fn state_of(view: &ChatView) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({ "items": items, "selected": view.selected(), "reacting": view.reacting() })
}

fn item_of(item: &MessageItem) -> Value {
    let (kind, text) = match &item.body {
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
        Body::Cards(_) => ("cards", String::new()),
        _ => ("other", String::new()),
    };
    let app_cards = item.app_cards.as_ref().map_or(0, |cards| cards.0.cards.len());
    json!({ "id": item.id, "own": item.own, "kind": kind, "text": text, "app_cards": app_cards })
}
