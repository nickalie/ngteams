//! A bot's chat in the main window over synthetic data, rendered in the private UI sandbox in Teams Light and Dark,
//!: the chat with its app's name and tile in the list, the header and beside its
//! messages ("chat"); a bot chat nobody wrote in yet, with its app's intro ("intro"); a group chat's mention picker
//! offering the bot of an app added to it after the members ("picker"); the app's About from the bot chat's header
//! ("about"). Screenshots go to `target/ui-shots/bot-chat-*.png`. Never reaches a tenant.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared synthetic view-test engine, no live requests"
)]
#[path = "../../common/mod.rs"]
mod common;
use common::{FakeCore, FakeEvents, LAB_A, LAB_B};
use gpui::{App, AppContext as _, Entity, Window};
use serde_json::json;
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
use teams_proto::{PastedFormat, PastedPicture, StoreApp};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::{
    Bridge, Core, Models, ViewTrace,
    settings::{Settings, SettingsModel},
    shell::{Shell, ShellParts},
    theme::{Appearance, DARK_KEY, LIGHT_KEY},
};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

const CHILD: &str = "TEAMS_UI_BOT_CHAT_WINDOW";
const PAGES: [&str; 4] = ["chat", "intro", "picker", "about"];
/// The made-up apps' bots and their chats with the account.
const HELPER_APP: &str = "00000000-0000-4000-8000-00000000a004";
const HELPER_BOT: &str = "00000000-0000-4000-8000-00000000b004";
const POLLS_APP: &str = "00000000-0000-4000-8000-00000000a005";
const POLLS_BOT: &str = "00000000-0000-4000-8000-00000000b005";
const HELPER_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000b004@unq.gbl.spaces";
const POLLS_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000b005@unq.gbl.spaces";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const GROUP: &str = "19:00000000000000000000000000000group@thread.v2";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const TILE: &str = "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-helper_largeImage.png";
/// A made-up app tile: the app's own icon stands in for it.
const TILE_PNG: &[u8] = include_bytes!("../../../../../packaging/icons/hicolor/64x64/apps/com.nickalie.ngteams.png");

fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

pub(super) fn main() -> ExitCode {
    let result = match std::env::var(CHILD) {
        Ok(value) => child(&value),
        Err(_) => parent(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bot chat window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn shots() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("bot chat")? else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for page in PAGES {
            shot(&sandbox, theme, name, page).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    Ok(())
}

fn shot(sandbox: &Sandbox, theme: &str, name: &str, page: &str) -> SandboxResult {
    let mode = format!("{theme}:{page}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, &mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready: {error:?}")))?;
    let file = format!("bot-chat-{page}-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots()?.join(&file);
    fs::copy(png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}

fn after_frames(window: &mut Window, n: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        #[cfg(windows)]
        if !fixture::scene_activated(window) {
            after_frames(window, n, done);
            window.refresh();
            return;
        }
        if n <= 1 {
            done(window, cx);
        } else {
            after_frames(window, n - 1, done);
            window.refresh();
        }
    });
    window.refresh();
}

/// The made-up apps: a helper to chat with one to one, and polls people mention in a group chat.
fn apps() -> Vec<StoreApp> {
    teams_proto::AppDefinitions::read_answer(
        json!([
            {"id": HELPER_APP, "name": "Made-up Helper", "developerName": "Made-up Maker",
             "shortDescription": "Answers made-up questions about lunch, rooms and the week ahead.",
             "largeImageUrl": TILE, "version": "1.0.0", "accentColor": "#4f52b2",
             "bots": [{"id": HELPER_BOT, "scopes": ["personal"]}]},
            {"id": POLLS_APP, "name": "Made-up Polls", "developerName": "Made-up Maker",
             "shortDescription": "Polls made up", "accentColor": "#038387",
             "bots": [{"id": POLLS_BOT, "scopes": ["groupChat", "team"]}]},
        ])
        .to_string()
        .as_bytes(),
    )
}

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: i64, sender: &str, name: &str, content: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some(name.to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: id,
        compose_time: Some(id),
        edit_time: None,
        delete_time: None,
    }
}

fn seed(core: &FakeCore) {
    let helper = format!("28:{HELPER_BOT}");
    let at = 1_767_268_800_000;
    core.conversations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend([
            chat(HELPER_CHAT, ConversationKind::OneToOne, None, at + 120_000),
            chat(GROUP, ConversationKind::Group, Some("Lunch crew"), at + 60_000),
            chat(AB, ConversationKind::OneToOne, None, at),
            chat(POLLS_CHAT, ConversationKind::OneToOne, None, at - 60_000),
        ]);
    let mut messages = core.messages.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    messages.insert(
        HELPER_CHAT.to_owned(),
        vec![
            message(HELPER_CHAT, at, LAB_A, "Synthetic Ay", "<p>What's for lunch today?</p>"),
            message(
                HELPER_CHAT,
                at + 60_000,
                &helper,
                "Made-up Helper",
                "<p>Today there is tomato soup and a green salad.</p>",
            ),
            message(
                HELPER_CHAT,
                at + 120_000,
                &helper,
                "Made-up Helper",
                "<p>Ask me about rooms or the week ahead too.</p>",
            ),
        ],
    );
    messages.insert(
        GROUP.to_owned(),
        vec![message(GROUP, at + 60_000, LAB_B, "Synthetic Bee", "<p>Lunch?</p>")],
    );
    messages.insert(
        AB.to_owned(),
        vec![message(AB, at, LAB_B, "Synthetic Bee", "<p>See you there</p>")],
    );
    drop(messages);
    let mut members = core.members.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    members.insert(
        GROUP.to_owned(),
        [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
    );
    members.insert(
        AB.to_owned(),
        [LAB_A, LAB_B].iter().map(|mri| Member::new(mri, None)).collect(),
    );
    drop(members);
    let mut names = core.names.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    names.insert(LAB_A.to_owned(), "Synthetic Ay".to_owned());
    names.insert(LAB_B.to_owned(), "Synthetic Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Synthetic Sea".to_owned());
    drop(names);
    let apps = apps();
    let mut bots = core.bot_apps.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bots.insert(HELPER_BOT.to_owned(), apps[0].clone());
    bots.insert(POLLS_BOT.to_owned(), apps[1].clone());
    drop(bots);
    core.mentionable_bots
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(GROUP.to_owned(), vec![apps[1].clone()]);
    core.app_pictures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            TILE.to_owned(),
            Arc::new(PastedPicture {
                format: PastedFormat::Png,
                bytes: TILE_PNG.to_vec(),
            }),
        );
}

fn child(value: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let (theme, page) = value.split_once(':').ok_or("bad child mode")?;
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let page = page.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, page, cx) {
            eprintln!("bot chat open: {error}");
            cx.quit();
        }
    });
    Ok(())
}

fn open(theme: &'static str, page: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    seed(&core);
    let (events, received) = mpsc::unbounded_channel();
    let models = Models::new(cx);
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, "ngteams-bot-chat-test", "Bot chat".into(), parts)?;
    let root = teams_ui::window::content(window, cx).ok_or_else(|| std::io::Error::other("missing shell"))?;
    let shell = root
        .read(cx)
        .shell_in(cx)
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing shell"))?;
    window.update(cx, |_, window, _| {
        after_frames(window, 30, move |window, cx| {
            step(&shell, &page, window, cx);
            after_frames(window, 30, move |window, cx| {
                then(&shell, &page, window, cx);
                after_frames(window, 30, |_, _| say("ready"));
            });
        });
    })?;
    std::mem::forget((bridge, events));
    Ok(())
}

/// Opens the page's chat.
fn step(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    let chat = match page {
        "intro" => POLLS_CHAT,
        "picker" => GROUP,
        _ => HELPER_CHAT,
    };
    shell.update(cx, |shell, cx| shell.open_chat(chat.to_owned(), window, cx));
}

/// The page's step once its chat shows: the picker opened by an `@`, or the app's About.
fn then(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    match page {
        "picker" => {
            let Some(chat) = shell.read(cx).chat().cloned() else {
                return;
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| {
                compose.focus(window, cx);
                compose
                    .editor()
                    .update(cx, |editor, cx| editor.insert("Who's in? @", cx));
            });
        }
        "about" => shell.update(cx, |shell, cx| shell.open_bot_about(HELPER_CHAT, window, cx)),
        _ => {}
    }
}
