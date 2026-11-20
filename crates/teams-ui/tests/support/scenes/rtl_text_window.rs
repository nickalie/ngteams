//! Right-to-left text in the main window over synthetic data, rendered in the private UI sandbox in Teams Light and
//! Dark, as Teams shows it: an Arabic sentence and its English continuation in one paragraph of an English message, a
//! pure Arabic and a pure Hebrew message, digits, brackets, a link, bold and italic runs and a list item in mixed text,
//! a reply's quote of an Arabic message, an Arabic line over an English one, and the chat list's preview of an Arabic
//! message ("chat"); the compose box with mixed text ("compose"); a drag across the mixed line, its selection drawn on
//! the clusters it covers ("select"). A message takes the direction of the first strong
//! letter of its text, as Teams' `dir="auto"` does; the compose box and the preview stay left to right. Screenshots go
//! to `target/ui-shots/rtl-text-*.png`. All text is made up. Never reaches a tenant.
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
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
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

const CHILD: &str = "TEAMS_UI_RTL_TEXT_WINDOW";
const PAGES: [&str; 3] = ["chat", "compose", "select"];
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const GROUP: &str = "19:00000000000000000000000000000group@thread.v2";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AT: i64 = 1_767_268_800_000;
/// The pure Arabic message, which the reply quotes.
const QUOTED: i64 = AT + 120_000;
/// What the compose box holds on its page.
const DRAFT: &str = "\"أوافق على الشروط والأحكام. I agree to the terms.\" Please confirm (2 items).";

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
            eprintln!("rtl text window: {error}");
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
    let Some(sandbox) = sandbox_or_skip("rtl text")? else {
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
    let file = format!("rtl-text-{page}-{name}.png");
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

/// The messages of the 1:1 chat: every shape of mixed text Teams draws, oldest first.
fn messages() -> Vec<Message> {
    let reply = format!(
        "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"{QUOTED}\">\
         <strong itemprop=\"mri\" itemid=\"{LAB_B}\">Synthetic Bee</strong>\
         <span itemprop=\"time\" itemid=\"{QUOTED}\"></span>\
         <p itemprop=\"preview\">مرحبا بكم في الفريق، هل أنتم جاهزون للاجتماع غدا؟</p></blockquote>\
         <p>نعم، أنا جاهز تماما.</p>"
    );
    [
        (
            LAB_B,
            "Synthetic Bee",
            "<p>Each option is kept in a column named after its id. Both use the same wording:</p>\
             <p>\"أوافق على تلقي العروض الترويجية المستقبلية والأخبار والعروض من النادي. I agree to receive future \
             promotions, news and offers from the club.\"</p>\
             <p>The column is empty when nobody chose it.</p>"
                .to_owned(),
        ),
        (
            LAB_B,
            "Synthetic Bee",
            "<p>مرحبا بكم في الفريق، هل أنتم جاهزون للاجتماع غدا؟</p>".to_owned(),
        ),
        (
            LAB_B,
            "Synthetic Bee",
            "<p>שלום לכולם, הפגישה מחר בשעה 10.</p>".to_owned(),
        ),
        (
            LAB_A,
            "Synthetic Ay",
            "<p>Order 42: تم شحن الطلب 123 اليوم (2 items). See <a href=\"https://example.com/order\">صفحة \
             الطلب</a>, <b>نص عريض</b> and <i>نص مائل</i> too.</p>\
             <ul><li>البند الأول: the first item</li><li>The second item, عربي في النهاية</li></ul>"
                .to_owned(),
        ),
        (LAB_A, "Synthetic Ay", reply),
        (
            LAB_B,
            "Synthetic Bee",
            "<p>شكرا على المساعدة اليوم.</p><p>Thanks for the help today, see you on Monday.</p>".to_owned(),
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(at, (sender, name, content))| {
        let at = i64::try_from(at).unwrap_or_default();
        message(AB, AT + at * 60_000, sender, name, &content)
    })
    .collect()
}

fn seed(core: &FakeCore) {
    core.conversations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend([
            chat(AB, ConversationKind::OneToOne, None, AT + 300_000),
            chat(GROUP, ConversationKind::Group, Some("Lunch crew"), AT),
        ]);
    let mut messages = core.messages.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    messages.insert(AB.to_owned(), self::messages());
    messages.insert(
        GROUP.to_owned(),
        vec![message(GROUP, AT, LAB_C, "Synthetic Sea", "<p>See you at noon</p>")],
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
            eprintln!("rtl text open: {error}");
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
    let window = teams_ui::window::open(cx, "ngteams-rtl-text-test", "Right-to-left text".into(), parts)?;
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

/// Opens the 1:1 chat.
fn step(shell: &Entity<Shell>, _page: &str, window: &mut Window, cx: &mut App) {
    shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx));
}

/// Where the drag of the "select" page starts and ends: in the mixed line of the first message, from the middle of
/// its Arabic to the middle of its English, logical px as the 1280 by 800 window lays it out.
const DRAG: [(f32, f32); 2] = [(640.0, 254.0), (1000.0, 254.0)];

fn drag(window: &mut Window, cx: &mut App) {
    let [from, to] = DRAG.map(|(x, y)| gpui::point(gpui::px(x), gpui::px(y)));
    let events = [
        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: from,
            button: gpui::MouseButton::Left,
            modifiers: gpui::Modifiers::none(),
            click_count: 1,
            first_mouse: false,
        }),
        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
            position: to,
            pressed_button: Some(gpui::MouseButton::Left),
            modifiers: gpui::Modifiers::none(),
        }),
        gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: to,
            button: gpui::MouseButton::Left,
            modifiers: gpui::Modifiers::none(),
            click_count: 1,
        }),
    ];
    for event in events {
        window.dispatch_event(event, cx);
    }
}

/// The page's step once its chat shows: the draft typed into the compose box, or the drag.
fn then(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    if page == "select" {
        drag(window, cx);
        return;
    }
    if page != "compose" {
        return;
    }
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let compose = chat.read(cx).compose().clone();
    compose.update(cx, |compose, cx| {
        compose.focus(window, cx);
        compose.editor().update(cx, |editor, cx| editor.insert(DRAFT, cx));
    });
}
