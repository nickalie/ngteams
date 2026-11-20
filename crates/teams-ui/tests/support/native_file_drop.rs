//! The chat window a File Explorer drag drops onto. It says on stdout where its message box is (`box <x> <y> <scale>`,
//! in the window's client area), when the drop highlight shows or goes (`hover <bool>`), and the files staged once
//! all are ready (`files <names>`); the driver checks those lines.
use crate::common::{FakeCore, LAB_A, LAB_B};
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowBounds, WindowOptions, div, px, size,
};
use std::{
    cell::Cell,
    io::Write as _,
    process::ExitCode,
    rc::Rc,
    sync::{Arc, PoisonError},
    time::Duration,
};
use teams_ui::{
    Core, Models, ShellParts, ViewTrace,
    chat_view::ChatView,
    compose::attachments::AttachmentState,
    settings::{Settings, SettingsModel},
};

const TITLE: &str = "NGTEAMS SYNTHETIC FILE DROP";
const CHAT: &str = "19:synthetic-file-drop@thread.v2";
/// How long the fixture waits for the driver's drop.
const DEADLINE: Duration = Duration::from_secs(150);

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

struct Holder(Entity<ChatView>);

impl Render for Holder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.0.clone())
    }
}

pub fn main() -> ExitCode {
    if std::env::var_os("NGTEAMS_NATIVE_FILE_DROP_REQUIRE").is_none() {
        println!("native file drop requires the authorized win11 console helper");
        return ExitCode::SUCCESS;
    }
    teams_platform::windows_sandbox::window_allowed_here().expect("authorized console");
    gpui_platform::application()
        .with_assets(teams_ui::AppAssets)
        .run(|cx: &mut App| {
            teams_ui::window::init(cx);
            if let Err(error) = open(cx) {
                say(&format!("open failed: {error:#}"));
                cx.quit();
            }
            cx.spawn(async |cx| {
                cx.background_executor().timer(DEADLINE).await;
                say("deadline");
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    ExitCode::SUCCESS
}

fn open(cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    core.members.lock().unwrap_or_else(PoisonError::into_inner).insert(
        CHAT.to_owned(),
        vec![
            teams_store::Member::new(LAB_A, None),
            teams_store::Member::new(LAB_B, None),
        ],
    );
    let settings = cx.new(|_| SettingsModel::in_memory(Settings::default()));
    let parts = ShellParts {
        core: core as Arc<dyn Core>,
        models: Models::new(cx),
        settings,
        trace: Arc::new(ViewTrace::off()),
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(900.0), px(700.0)),
            cx,
        ))),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(TITLE.into()),
            ..gpui::TitlebarOptions::default()
        }),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        let chat = cx.new(|cx| ChatView::new(parts, CHAT.to_owned(), window, cx));
        let compose = chat.read(cx).compose().clone();
        let hovering = Rc::new(Cell::new(false));
        cx.observe(&chat, move |chat, cx| {
            let now = chat.read(cx).file_drop_active();
            if hovering.replace(now) != now {
                say(&format!("hover {now}"));
            }
        })
        .detach();
        let told = Rc::new(Cell::new(false));
        cx.observe(&compose, move |compose, cx| {
            let files = compose.read(cx).attachments();
            if !files.is_empty()
                && files.iter().all(|file| matches!(file.state, AttachmentState::Ready(_)))
                && !told.replace(true)
            {
                let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
                say(&format!("files {}", names.join("|")));
            }
        })
        .detach();
        let boxed = compose.clone();
        frames(window, 10, move |window, cx| {
            if let Some(bounds) = boxed.read(cx).box_bounds() {
                let centre = bounds.center();
                say(&format!(
                    "box {} {} {}",
                    f32::from(centre.x),
                    f32::from(centre.y),
                    window.scale_factor()
                ));
            }
            say("ready");
        });
        let holder = cx.new(|_| Holder(chat));
        cx.new(|cx| gpui_component::Root::new(holder, window, cx))
    })?;
    Ok(())
}

fn frames(window: &Window, left: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if left <= 1 {
            done(window, cx);
        } else {
            frames(window, left - 1, done);
        }
    });
}
