//! A bot's cards in a chat with the bots shown (`NGTEAMS_BOTS=1`), rendered in the private UI sandbox over synthetic
//! data in Teams Light and Dark: a card's inputs, a required one left empty, the bot's card in the card's place, its
//! message, its error and its sign-in request under the card, the suggested actions over the compose box, the card in
//! read-only mode, "Refresh card" in a message's menu, and a sign-in card with its code field. Screenshots go to
//! `target/ui-shots/bot-cards-*.png`. Never reaches a tenant.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared offline test fake; this sandbox harness runs without cfg(test)"
)]
#[path = "../../common/mod.rs"]
mod common;

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gpui::{
    AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowOptions, div, rgb,
};
use serde_json::{Value, json};
use teams_proto::InvokeAnswer;
use teams_store::Message;
use teams_ui::{
    Core, Models, ShellParts, ViewTrace,
    bots_flag::BotsFlag,
    chat_view::ChatView,
    settings::{Settings, SettingsModel},
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
    window::KitRoot,
};
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "this scene does not request fixture diagnostics")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

const CHILD: &str = "TEAMS_UI_BOT_CARDS_WINDOW";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// A made-up bot.
const BOT: &str = "28:00000000-0000-4000-8000-0000000000b0";
const ID: &str = "1";
const PAGES: [&str; 10] = [
    "inputs",
    "invalid",
    "replaced",
    "message",
    "error",
    "sign-in",
    "suggested",
    "read-only",
    "refresh",
    "oauth",
];

pub(super) fn main() -> ExitCode {
    let result = std::env::var(CHILD).map_or_else(|_| parent(), |mode| child(&mode));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bot cards window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn shots() -> io::Result<PathBuf> {
    let p = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .unwrap_or(Path::new("target"))
        .join("ui-shots");
    fs::create_dir_all(&p)?;
    Ok(p)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("bot cards")? else {
        return Ok(());
    };
    for (theme, label) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for page in PAGES {
            shot(&sandbox, theme, label, page)?;
        }
    }
    Ok(())
}

fn shot(sandbox: &Sandbox, theme: &str, label: &str, page: &str) -> SandboxResult {
    let mode = format!("{theme}|{page}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, mode.as_str())])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no page ready: {error:?}")))?;
    let name = format!("bot-cards-{label}-{page}.png");
    let screenshot = sandbox.screenshot(&name, true)?;
    fs::copy(screenshot, shots()?.join(&name))?;
    say(&format!("shot {name}"));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}

/// The page over the theme's background, centred.
struct Stage(AnyView);

impl Render for Stage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(theme::current(cx).background))
            .child(self.0.clone())
    }
}

/// A card message from the bot carrying `activity`.
fn bot_message(activity: &Value) -> Message {
    let b64 = STANDARD.encode(serde_json::to_vec(activity).unwrap_or_default());
    Message {
        conversation_id: AB.to_owned(),
        id: ID.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(BOT.to_owned()),
        sender_name: Some("Lunch Bot".to_owned()),
        message_type: "RichText/Media_Card".to_owned(),
        content: format!(
            "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
             https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
        ),
        properties: None,
        arrival_time: 1_767_268_800_000,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn adaptive(card: &Value) -> Value {
    json!({"type": "message/card", "attachments": [
        {"contentType": "application/vnd.microsoft.card.adaptive", "content": card}
    ]})
}

/// The order form: every kind of input, a plain submit and an `Action.Execute`.
fn form_card() -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Order lunch", "weight": "bolder", "size": "medium"},
            {"type": "Input.Text", "id": "name", "label": "Your name", "isRequired": true,
             "placeholder": "First and last name", "errorMessage": "Enter your name"},
            {"type": "Input.Text", "id": "note", "label": "Note", "isMultiline": true, "placeholder": "Anything else?"},
            {"type": "ColumnSet", "columns": [
                {"type": "Column", "width": "stretch", "items": [
                    {"type": "Input.Number", "id": "count", "label": "How many", "min": 1, "max": 9, "value": 1}
                ]},
                {"type": "Column", "width": "stretch", "items": [
                    {"type": "Input.Date", "id": "day", "label": "Day", "value": "2026-11-20"}
                ]},
                {"type": "Column", "width": "stretch", "items": [
                    {"type": "Input.Time", "id": "at", "label": "Time", "value": "12:30"}
                ]}
            ]},
            {"type": "Input.ChoiceSet", "id": "dish", "label": "Dish", "value": "soup", "choices": [
                {"title": "Tomato soup", "value": "soup"}, {"title": "Green salad", "value": "salad"}
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

/// A status card with one `Action.Execute` and a `refresh` anyone may run by hand.
fn status_card() -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Lunch order", "weight": "bolder", "size": "medium"},
            {"type": "FactSet", "facts": [{"title": "Dish", "value": "Tomato soup"}, {"title": "Status", "value": "Waiting"}]}
        ],
        "actions": [{"type": "Action.Execute", "title": "Confirm", "verb": "confirm"}],
        "refresh": {"action": {"type": "Action.Execute", "verb": "status"}, "userIds": []}
    })
}

fn confirmed_card() -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Lunch order", "weight": "bolder", "size": "medium"},
            {"type": "FactSet", "facts": [{"title": "Dish", "value": "Tomato soup"}, {"title": "Status", "value": "Confirmed"}]},
            {"type": "TextBlock", "text": "Confirmed by Synthetic Ay", "isSubtle": true, "size": "small"}
        ]
    })
}

fn suggesting() -> Value {
    json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": {
            "type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Which dish would you like?", "wrap": true}]
        }}],
        "suggestedActions": {"actions": [
            {"type": "imBack", "title": "Tomato soup", "value": "Tomato soup"},
            {"type": "imBack", "title": "Green salad", "value": "Green salad"},
            {"type": "imBack", "title": "Nothing today", "value": "Nothing today"}
        ]}
    })
}

/// A bot's sign-in prompt: an OAuth card with its Sign in button.
fn oauth() -> Value {
    json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.oauth", "content": {
            "text": "Sign in to Synthetic Tools to see your tasks.", "connectionName": "tools",
            "buttons": [{"type": "signin", "title": "Sign in", "value": "https://tools.example.invalid/sign-in"}]
        }}]
    })
}

/// The message a page shows, and what the bot answers its press.
fn page_data(page: &str) -> (Message, Option<InvokeAnswer>) {
    match page {
        "inputs" | "invalid" | "read-only" => (bot_message(&adaptive(&form_card())), None),
        "suggested" => (bot_message(&suggesting()), None),
        "oauth" => (bot_message(&oauth()), None),
        "replaced" => (
            bot_message(&adaptive(&status_card())),
            Some(InvokeAnswer::Card(confirmed_card())),
        ),
        "message" => (
            bot_message(&adaptive(&status_card())),
            Some(InvokeAnswer::Message(
                "Your order is in. We'll tell you when it's ready.".to_owned(),
            )),
        ),
        "error" => (
            bot_message(&adaptive(&status_card())),
            Some(InvokeAnswer::Error {
                code: Some("BadRequest".to_owned()),
                message: "The kitchen is closed today.".to_owned(),
            }),
        ),
        _ => (
            bot_message(&adaptive(&status_card())),
            Some(InvokeAnswer::SignIn(Value::Null)),
        ),
    }
}

fn child(mode: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let (theme, page) = mode.split_once('|').unwrap_or((LIGHT_KEY, "inputs"));
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let page = page.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        teams_ui::window::init(cx);
        cx.set_global(BotsFlag(true));
        theme::set_appearance(
            &Appearance {
                theme,
                follow_system: false,
                ..Appearance::default()
            },
            cx,
        );
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let core = Arc::new(common::FakeCore::default());
        let (message, answer) = page_data(&page);
        core.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(AB.to_owned(), vec![message]);
        core.bot_answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(answer.map(Ok));
        *core.read_only.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = page == "read-only";
        let core: Arc<dyn Core> = core;
        let settings = cx.new(|_| SettingsModel::in_memory(Settings::default()));
        let models = Models::new(cx);
        let options = WindowOptions {
            window_bounds: Some(teams_ui::window::initial_bounds(
                gpui::size(gpui::px(1100.0), gpui::px(900.0)),
                cx,
            )),
            #[cfg(windows)]
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("NGTEAMS SYNTHETIC BOT CARDS".into()),
                ..Default::default()
            }),
            app_id: Some("ngteams-bot-cards-test".to_owned()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, move |window, cx| {
            let parts = ShellParts {
                core,
                models,
                settings,
                trace: Arc::new(ViewTrace::off()),
            };
            let chat: Entity<ChatView> = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
            step(&page, chat.clone(), window);
            let framed = div_view(chat, cx);
            let stage = cx.new(|_| Stage(framed));
            cx.new(|cx| KitRoot::new(stage, window, cx))
        });
        if opened.is_err() {
            cx.quit();
        }
    });
    Ok(())
}

/// The page's step, taken once the chat settled, then "ready".
fn step(page: &str, chat: Entity<ChatView>, window: &mut Window) {
    let page = page.to_owned();
    after(window, 15, move |window, cx| {
        match page.as_str() {
            "invalid" | "replaced" | "message" | "error" | "sign-in" | "oauth" => {
                chat.update(cx, |chat, cx| chat.press_card_action(ID, 0, 0, cx));
            }
            "refresh" => chat.update(cx, |chat, cx| chat.toggle_options(ID, cx)),
            _ => {}
        }
        after(window, 15, |_, _| say("ready"));
    });
}

/// A chat view in a window-sized frame, so its menu has room.
fn div_view(chat: Entity<ChatView>, cx: &mut App) -> AnyView {
    cx.new(|_| Framed(chat)).into()
}

struct Framed(Entity<ChatView>);

impl Render for Framed {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(gpui::px(900.0)).h(gpui::px(860.0)).child(self.0.clone())
    }
}

fn after(window: &mut Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        #[cfg(windows)]
        if !fixture::scene_activated(window) {
            after(window, frames, done);
            window.refresh();
            return;
        }
        if frames == 0 {
            done(window, cx);
        } else {
            after(window, frames - 1, done);
            window.refresh();
        }
    });
    window.refresh();
}
