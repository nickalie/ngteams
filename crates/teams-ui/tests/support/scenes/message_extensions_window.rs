//! Message extensions and apps' dialogs in a chat, rendered in the private UI
//! sandbox over synthetic data (`tests/fixtures/bots/extension-apps.json`) in Teams Light and Dark: the compose box's
//! Actions and apps, an app's search and its results, a result's card in the compose box, an action command's card
//! dialog and its submit, a dialog that is a web page, a message's "More actions" with the apps' actions, and a card's
//! `task/fetch` dialog. Each page checks what was asked of the bot before it is shot. Screenshots go to
//! `target/ui-shots/message-extensions-*.png`. Never reaches a tenant.
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
use serde_json::json;
use teams_proto::{InvokeAnswer, InvokeKind, TaskDialog};
use teams_store::Message;
use teams_ui::{
    Core, Models, ShellParts, ViewTrace,
    app_dialog::AppDialog,
    chat_view::{ChatView, ChatViewEvent},
    settings::{Settings, SettingsModel},
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
    window::KitRoot,
};
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "this scene does not request fixture diagnostics")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

const CHILD: &str = "TEAMS_UI_MESSAGE_EXTENSIONS_WINDOW";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const BOT: &str = "28:00000000-0000-4000-8000-00000000eb01";
const FINDER: &str = "00000000-0000-4000-8000-00000000e001";
const PRAISE: &str = "00000000-0000-4000-8000-00000000e002";
const ID: &str = "1";
const PAGES: [&str; 8] = [
    "flyout",
    "search",
    "card-in-box",
    "dialog",
    "submitted",
    "web",
    "more-actions",
    "task-fetch",
];

pub(super) fn main() -> ExitCode {
    let result = std::env::var(CHILD).map_or_else(|_| parent(), |mode| child(&mode));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("message extensions window: {error}");
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
    let Some(sandbox) = sandbox_or_skip("message extensions")? else {
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
        .map_err(|error| running.fail(&format!("no page {page} ready: {error:?}")))?;
    let name = format!("message-extensions-{label}-{page}.png");
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

/// The chat in its frame, and an app's dialog over it on a backdrop while one is open, as the shell shows it.
struct Stage {
    chat: AnyView,
    dialog: Option<AnyView>,
}

impl Render for Stage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let mut stage = div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(palette.background))
            .child(self.chat.clone());
        if let Some(dialog) = &self.dialog {
            stage = stage.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x0000_0080))
                    .child(dialog.clone()),
            );
        }
        stage
    }
}

/// A plain message from b.
fn text_message() -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: ID.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some("8:orgid:00000000-0000-4000-8000-00000000000b".to_owned()),
        sender_name: Some("Synthetic Bee".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: "<p>Can someone file the made-up order 1042 for Friday?</p>".to_owned(),
        properties: None,
        arrival_time: 1_767_268_800_000,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

/// A card from the Finder's bot with a `task/fetch` button.
fn task_card_message() -> Message {
    let activity = json!({"type": "message/card", "attachments": [{
        "contentType": "application/vnd.microsoft.card.adaptive",
        "content": {"type": "AdaptiveCard", "version": "1.5", "body": [
            {"type": "TextBlock", "text": "Record 1042", "weight": "bolder", "size": "medium"},
            {"type": "TextBlock", "text": "Made-up order for Friday, waiting for a reviewer.", "wrap": true}
        ], "actions": [
            {"type": "Action.Submit", "title": "Review", "data": {"msteams": {"type": "task/fetch"}, "record": 1042}}
        ]}
    }]});
    let b64 = STANDARD.encode(serde_json::to_vec(&activity).unwrap_or_default());
    Message {
        sender: Some(BOT.to_owned()),
        sender_name: Some("Made-up Finder".to_owned()),
        message_type: "RichText/Media_Card".to_owned(),
        content: format!(
            "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
             https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
        ),
        ..text_message()
    }
}

/// The Finder's search results: three made-up records.
fn results() -> InvokeAnswer {
    let item = |n: u32, line: &str| {
        json!({
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "version": "1.5", "body": [
                {"type": "TextBlock", "text": format!("Record {n}"), "weight": "bolder", "size": "medium"},
                {"type": "TextBlock", "text": line, "wrap": true},
                {"type": "FactSet", "facts": [{"title": "Status", "value": "Open"}, {"title": "Owner", "value": "Synthetic Ay"}]}
            ]},
            "preview": {"contentType": "application/vnd.microsoft.card.thumbnail",
                        "content": {"title": format!("Record {n}"), "text": line}}
        })
    };
    InvokeAnswer::Results {
        grid: false,
        attachments: vec![
            item(1042, "Made-up order for Friday"),
            item(1043, "Made-up order for the lab trio"),
            item(1044, "Made-up return, waiting for parts"),
        ],
    }
}

/// The New record dialog's card.
fn record_dialog(title: &str) -> InvokeAnswer {
    InvokeAnswer::Dialog(TaskDialog {
        title: Some(title.to_owned()),
        card: Some(json!({"type": "AdaptiveCard", "version": "1.5", "body": [
            {"type": "TextBlock", "text": "New record", "weight": "bolder", "size": "medium"},
            {"type": "Input.Text", "id": "name", "label": "Name", "isRequired": true, "value": "Made-up order 1045",
             "errorMessage": "Give the record a name"},
            {"type": "Input.Date", "id": "due", "label": "Due", "value": "2026-11-20"},
            {"type": "Input.ChoiceSet", "id": "kind", "label": "Kind", "value": "order", "choices": [
                {"title": "Order", "value": "order"}, {"title": "Return", "value": "return"}
            ]},
            {"type": "Input.Text", "id": "note", "label": "Note", "isMultiline": true, "placeholder": "Anything else?"}
        ], "actions": [{"type": "Action.Submit", "title": "Create", "data": {"form": "record"}}]})),
        url: None,
        width: Some(teams_proto::DialogSize::Medium),
        height: None,
    })
}

/// What a page's chat holds, and what the bot answers its asks, in order.
fn page_data(page: &str) -> (Message, Vec<InvokeAnswer>) {
    match page {
        "search" | "card-in-box" => (text_message(), vec![results()]),
        "dialog" => (text_message(), vec![results(), record_dialog("New record")]),
        "submitted" => (
            text_message(),
            vec![
                results(),
                record_dialog("New record"),
                InvokeAnswer::Message("Record 1045 was made. It's in your list.".to_owned()),
            ],
        ),
        "task-fetch" => (task_card_message(), vec![record_dialog("Review record 1042")]),
        _ => (text_message(), Vec::new()),
    }
}

fn child(mode: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let (theme, page) = mode.split_once('|').unwrap_or((LIGHT_KEY, "flyout"));
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let page = page.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        teams_ui::window::init(cx);
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
        let fake = Arc::new(common::FakeCore::default());
        let (message, answers) = page_data(&page);
        fake.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(AB.to_owned(), vec![message]);
        fake.bot_answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(answers.into_iter().map(Ok));
        *fake
            .extension_apps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
        let core: Arc<dyn Core> = Arc::clone(&fake) as Arc<dyn Core>;
        let settings = cx.new(|_| SettingsModel::in_memory(Settings::default()));
        let models = Models::new(cx);
        let options = WindowOptions {
            window_bounds: Some(teams_ui::window::initial_bounds(
                gpui::size(gpui::px(1100.0), gpui::px(900.0)),
                cx,
            )),
            #[cfg(windows)]
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("NGTEAMS SYNTHETIC MESSAGE EXTENSIONS".into()),
                ..Default::default()
            }),
            app_id: Some("ngteams-message-extensions-test".to_owned()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, move |window, cx| {
            let parts = ShellParts {
                core: Arc::clone(&core),
                models,
                settings,
                trace: Arc::new(ViewTrace::off()),
            };
            let chat: Entity<ChatView> = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
            let framed = cx.new(|_| Framed(chat.clone())).into();
            let stage = cx.new(|_| Stage {
                chat: framed,
                dialog: None,
            });
            host_dialogs(&chat, &stage, &core, window, cx);
            step(&page, chat, stage.clone(), fake, window);
            cx.new(|cx| KitRoot::new(stage, window, cx))
        });
        if opened.is_err() {
            cx.quit();
        }
    });
    Ok(())
}

/// Opens an app's dialog over the chat as the shell does when the chat asks for one.
fn host_dialogs(
    chat: &Entity<ChatView>,
    stage: &Entity<Stage>,
    core: &Arc<dyn Core>,
    window: &mut Window,
    cx: &mut App,
) {
    let (stage, core) = (stage.clone(), Arc::clone(core));
    window
        .subscribe(chat, cx, move |_, event: &ChatViewEvent, window, cx| {
            if let ChatViewEvent::AppDialog(seed) = event {
                let seed = seed.as_ref().clone();
                let core = Arc::clone(&core);
                let dialog = cx.new(|cx| AppDialog::new(core, seed, window, cx));
                stage.update(cx, |stage, cx| {
                    stage.dialog = Some(dialog.into());
                    cx.notify();
                });
            }
        })
        .detach();
}

/// A page's run: what it drives and checks.
#[derive(Clone)]
struct Run {
    page: String,
    chat: Entity<ChatView>,
    stage: Entity<Stage>,
    fake: Arc<common::FakeCore>,
}

impl Run {
    fn flyout(&self, cx: &App) -> Option<Entity<teams_ui::apps_flyout::AppsFlyout>> {
        self.chat.read(cx).compose().read(cx).apps_flyout().cloned()
    }
}

/// The page's steps, taken once the chat settled; then its asks are checked and "ready" said.
fn step(page: &str, chat: Entity<ChatView>, stage: Entity<Stage>, fake: Arc<common::FakeCore>, window: &mut Window) {
    let run = Run {
        page: page.to_owned(),
        chat,
        stage,
        fake,
    };
    after(window, 15, move |window, cx| open(&run, window, cx));
}

/// Actions and apps, a message's menu, or a card's button.
fn open(run: &Run, window: &mut Window, cx: &mut App) {
    let compose = run.chat.read(cx).compose().clone();
    match run.page.as_str() {
        "more-actions" => run.chat.update(cx, |chat, cx| chat.toggle_options(ID, cx)),
        "task-fetch" => run.chat.update(cx, |chat, cx| chat.press_card_action(ID, 0, 0, cx)),
        _ => compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)),
    }
    let run = run.clone();
    after(window, 10, move |window, cx| choose_app(&run, window, cx));
}

/// An app chosen in the flyout, or "More actions" opened.
fn choose_app(run: &Run, window: &mut Window, cx: &mut App) {
    match (run.page.as_str(), run.flyout(cx)) {
        ("search" | "card-in-box" | "dialog" | "submitted", Some(flyout)) => {
            flyout.update(cx, |flyout, cx| flyout.choose_app(FINDER, window, cx));
        }
        ("web", Some(flyout)) => flyout.update(cx, |flyout, cx| flyout.choose_app(PRAISE, window, cx)),
        ("more-actions", _) => run.chat.update(cx, ChatView::open_more_actions),
        _ => {}
    }
    let run = run.clone();
    after(window, 10, move |window, cx| choose_in_pane(&run, window, cx));
}

/// A result or an action command chosen in the app's pane.
fn choose_in_pane(run: &Run, window: &mut Window, cx: &mut App) {
    match (run.page.as_str(), run.flyout(cx)) {
        ("card-in-box", Some(flyout)) => flyout.update(cx, |flyout, cx| flyout.choose_result(0, cx)),
        ("dialog" | "submitted", Some(flyout)) => {
            flyout.update(cx, |flyout, cx| flyout.choose_command(1, window, cx));
        }
        _ => {}
    }
    let run = run.clone();
    after(window, 10, move |window, cx| submit(&run, window, cx));
}

/// The dialog's card submitted.
fn submit(run: &Run, window: &mut Window, cx: &mut App) {
    let dialog = run
        .stage
        .read(cx)
        .dialog
        .clone()
        .and_then(|dialog| dialog.downcast::<AppDialog>().ok());
    if let (true, Some(dialog)) = (run.page == "submitted", dialog) {
        dialog.update(cx, |dialog, cx| dialog.press(0, cx));
    }
    let run = run.clone();
    after(window, 15, move |_, cx| finish(&run, cx));
}

/// The page checked: "ready", or what is wrong and the app ends.
fn finish(run: &Run, cx: &mut App) {
    match check(&run.page, &run.chat, &run.stage, &run.fake, cx) {
        Ok(()) => say("ready"),
        Err(problem) => {
            eprintln!("page {}: {problem}", run.page);
            cx.quit();
        }
    }
}

/// What the page must show and have asked.
fn check(
    page: &str,
    chat: &Entity<ChatView>,
    stage: &Entity<Stage>,
    fake: &common::FakeCore,
    cx: &App,
) -> Result<(), String> {
    let asks: Vec<InvokeKind> = fake
        .bot_asks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|ask| ask.kind.clone())
        .collect();
    let compose = chat.read(cx).compose().clone();
    let flyout = compose.read(cx).apps_flyout().cloned();
    let dialog = stage
        .read(cx)
        .dialog
        .clone()
        .and_then(|dialog| dialog.downcast::<AppDialog>().ok());
    let stage_of = |name: &str| {
        dialog
            .as_ref()
            .is_some_and(|dialog| dialog.read(cx).stage_name() == name)
    };
    let fine = match page {
        "flyout" => flyout.is_some_and(|flyout| {
            let names: Vec<String> = flyout
                .read(cx)
                .listed(cx)
                .iter()
                .map(|app| app.name().to_owned())
                .collect();
            names == ["Made-up Finder", "Made-up Praise"]
        }),
        "search" => {
            matches!(asks.as_slice(), [InvokeKind::Query { command, .. }] if command == "findRecords")
                && flyout
                    .and_then(|flyout| flyout.read(cx).pane())
                    .is_some_and(|(_, tabs, found)| tabs.len() == 2 && found.len() == 3)
        }
        "card-in-box" => flyout.is_none() && compose.read(cx).app_cards().len() == 1,
        "dialog" => {
            matches!(asks.get(1), Some(InvokeKind::FetchTask { command, .. }) if command == "newRecord")
                && stage_of("card")
        }
        "submitted" => {
            matches!(asks.get(2), Some(InvokeKind::SubmitAction { command, data, .. })
                if command == "newRecord" && data.get("form") == Some(&json!("record"))
                    && data.get("name") == Some(&json!("Made-up order 1045"))
                    && data.get("kind") == Some(&json!("order")))
                && stage_of("note")
        }
        "web" => asks.is_empty() && stage_of("web"),
        "more-actions" => {
            let actions: Vec<(String, bool)> = chat
                .read(cx)
                .message_app_actions()
                .iter()
                .map(|(app, command)| (command.title.clone(), app.installed))
                .collect();
            actions
                == [
                    ("New record".to_owned(), true),
                    ("Create task".to_owned(), true),
                    ("File a ticket".to_owned(), false),
                ]
        }
        "task-fetch" => {
            matches!(asks.as_slice(), [InvokeKind::TaskFetch(card)]
                if card.data.get("record") == Some(&json!(1042)) && card.message.as_deref() == Some(ID))
                && stage_of("card")
        }
        _ => true,
    };
    if fine { Ok(()) } else { Err(format!("asks {asks:?}")) }
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
