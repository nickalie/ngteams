//! A real compositor file drop between two application processes, not a synthetic handler call. All files and
//! accounts are made up; uploads are mocked. The source offers actual text/uri-list data over Wayland.
//!
//! Then the drops of the file managers people use, each from the sandbox's drag source ([`ui_sandbox::drag`]) onto
//! the message box with what that file manager offers: Dolphin's URI lists, Nautilus's portal key before its list,
//! Thunar's GTK 3 types, a sandboxed (Flatpak) program's portal key alone, and a source that answers late, so the
//! drop comes before the files were read. A drag of text alone, or of a web link, is refused: no highlight, and the
//! source cancels.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared synthetic UI-test core"
)]
#[path = "../../common/mod.rs"]
mod common;

use gpui::StatefulInteractiveElement as _;
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, ExternalDragPayload, ExternalPaths, FileDragPaths,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, Styled as _, Window, WindowBounds,
    WindowOptions, WindowingModes, actions, div, px, rgb, size,
};
use std::{
    cell::Cell,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::{Arc, PoisonError},
    thread,
    time::Duration,
};
use teams_ui::{
    Core, Models, ShellParts, ViewTrace,
    chat_view::ChatView,
    compose::Compose,
    settings::{Settings, SettingsModel},
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
};
use ui_sandbox::drag::{DndAction, DragEvent, DragSource};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, input::keys, sandbox_or_skip};

const CHILD: &str = "TEAMS_ATTACHMENT_WINDOW";
const FILES: &str = "TEAMS_ATTACHMENT_FILES";
/// Names the file managers whose drops run (comma-separated); all of them when unset.
const ONLY: &str = "TEAMS_ATTACHMENT_ONLY";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const WAIT: Duration = Duration::from_secs(30);
const LAYOUT: &str = "layout { gaps 0; default-column-width { proportion 0.75; }; focus-ring { off; }; border { off; }; }\nwindow-rule { match app-id=\"ngteams-file-source\"; default-column-width { fixed 360; }; }\n";

actions!(attachment_test, [SendFiles]);

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

pub(super) fn main() -> ExitCode {
    let result = match std::env::var(CHILD) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("attachment window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn expect(running: &mut Running, wanted: &str) -> SandboxResult {
    running
        .line_where(WAIT, |line| line == wanted)
        .map(|_| ())
        .map_err(|error| running.fail(&format!("expected {wanted}: {error:?}")))
}

fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
    let png = sandbox.screenshot(name, false)?;
    let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .ok_or("target")?
        .join("ui-shots");
    std::fs::create_dir_all(&target)?;
    std::fs::copy(png, target.join(name))?;
    say(&format!("shot {}", target.join(name).display()));
    Ok(())
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip(
        "native attachment drop",
        &SandboxOptions {
            width: 1440,
            height: 900,
            niri_extra: LAYOUT,
            ..SandboxOptions::default()
        },
    )?
    else {
        return Ok(());
    };
    let paths = [sandbox.path("made-up notes.txt"), sandbox.path("made-up-report.pdf")];
    for path in &paths {
        std::fs::write(path, b"A synthetic attachment, not a real document.\n")?;
    }
    let files = paths
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        let mut source =
            Running::new(sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, "source"), (FILES, &files)])?)?;
        expect(&mut source, "ready")?;
        let mut target = Running::new(sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, theme)])?)?;
        expect(&mut target, "ready")?;
        let mut input = sandbox.input()?;
        input.move_to(100, 400)?;
        input.button_down(keys::BTN_LEFT)?;
        input.move_to(150, 400)?;
        input.move_to(390, 400)?;
        thread::sleep(Duration::from_millis(200));
        input.move_to(900, 400)?;
        expect(&mut target, "hover true")?;
        // the highlight is drawn on the next frames
        thread::sleep(Duration::from_millis(500));
        shot(&sandbox, &format!("attachments-hover-{name}.png"))?;
        // Leaving the destination must remove the highlight before anything is attached.
        input.move_to(100, 400)?;
        expect(&mut target, "hover false")?;
        input.move_to(900, 400)?;
        thread::sleep(Duration::from_millis(200));
        input.move_to(902, 400)?;
        expect(&mut target, "hover true")?;
        input.button_up(keys::BTN_LEFT)?;
        expect(&mut target, "files ready 2")?;
        thread::sleep(Duration::from_millis(200));
        shot(&sandbox, &format!("attachments-staged-{name}.png"))?;
        input.click(900, 300, keys::BTN_LEFT)?;
        input.key(keys::F1)?;
        expect(&mut target, "sent 2 draft preserved")?;
        sandbox.niri_msg(&["action", "close-window"])?;
        let (status, stderr) = target.finish(WAIT)?;
        if !status.success() {
            return Err(format!("target {status}: {stderr}").into());
        }
        sandbox.niri_msg(&["action", "close-window"])?;
        let (status, stderr) = source.finish(WAIT)?;
        if !status.success() {
            return Err(format!("source {status}: {stderr}").into());
        }
    }
    say("real Wayland file drop: ok");
    let only = std::env::var(ONLY).unwrap_or_default();
    for source in file_managers().filter(|source| only.is_empty() || only.split(',').any(|name| name == source.name)) {
        file_manager_drop(&sandbox, &source, &paths)?;
        say(&format!("{} drop: ok", source.name));
    }
    Ok(())
}

/// The portal key the sandboxed sources hand over; the fake portal names the files for it.
const TRANSFER_KEY: &str = "made-up-transfer-key";

/// One file manager's drag, as it offers the files.
struct FileManager {
    name: &'static str,
    /// What it offers, from the files' URI list, their plain paths and the portal key.
    offers: fn(&str, &str) -> ui_sandbox::clipboard::Offers,
    actions: DndAction,
    /// How long it takes to answer a read.
    delay: Duration,
    /// The type the app reads the files from; `None`: it takes nothing from this drag.
    takes: Option<&'static str>,
}

fn offer(mime: &str, text: &str) -> (String, Vec<u8>) {
    (mime.to_owned(), text.as_bytes().to_vec())
}

fn file_managers() -> impl Iterator<Item = FileManager> {
    let all = DndAction::Copy | DndAction::Move | DndAction::Ask;
    [
        FileManager {
            name: "dolphin",
            offers: |uris, plain| {
                vec![
                    offer("text/uri-list", uris),
                    offer("application/x-kde4-urilist", uris),
                    offer("text/plain;charset=utf-8", plain),
                ]
            },
            actions: DndAction::Copy | DndAction::Move,
            delay: Duration::ZERO,
            takes: Some("text/uri-list"),
        },
        FileManager {
            name: "nautilus",
            offers: |uris, _| {
                vec![
                    offer("application/vnd.portal.filetransfer", TRANSFER_KEY),
                    offer("application/vnd.portal.files", TRANSFER_KEY),
                    offer("text/uri-list", uris),
                    offer("text/plain;charset=utf-8", uris),
                ]
            },
            actions: all,
            delay: Duration::ZERO,
            takes: Some("text/uri-list"),
        },
        FileManager {
            name: "thunar",
            offers: |uris, plain| {
                vec![
                    offer("text/uri-list", uris),
                    offer("x-special/gnome-copied-files", &format!("copy\n{uris}")),
                    offer("UTF8_STRING", plain),
                    offer("text/plain;charset=utf-8", plain),
                ]
            },
            actions: all,
            delay: Duration::ZERO,
            takes: Some("text/uri-list"),
        },
        FileManager {
            name: "flatpak",
            offers: |_, _| vec![offer("application/vnd.portal.filetransfer", TRANSFER_KEY)],
            actions: DndAction::Copy | DndAction::Move,
            delay: Duration::ZERO,
            takes: Some("application/vnd.portal.filetransfer"),
        },
        FileManager {
            name: "slow",
            offers: |uris, _| vec![offer("text/uri-list", uris)],
            actions: DndAction::Copy | DndAction::Move,
            delay: Duration::from_millis(1500),
            takes: Some("text/uri-list"),
        },
    ]
    .into_iter()
    .chain(drags_of_no_files())
}

/// Drags the app takes nothing from: text alone, and a web link's URI list.
fn drags_of_no_files() -> [FileManager; 2] {
    [
        FileManager {
            name: "text",
            offers: |_, _| {
                vec![
                    offer("text/plain;charset=utf-8", "Some made-up words"),
                    offer("text/html", "<p>Some made-up words</p>"),
                ]
            },
            actions: DndAction::Copy | DndAction::Move,
            delay: Duration::ZERO,
            takes: None,
        },
        FileManager {
            name: "link",
            offers: |_, _| {
                vec![
                    offer("text/uri-list", "https://example.invalid/made-up-page\r\n"),
                    offer("text/plain;charset=utf-8", "https://example.invalid/made-up-page"),
                ]
            },
            actions: DndAction::Copy | DndAction::Move,
            delay: Duration::ZERO,
            takes: None,
        },
    ]
}

/// Moves the held pointer from `from` to `to` in small steps, as a hand does.
fn glide(input: &mut ui_sandbox::input::VirtualInput, from: (u32, u32), to: (u32, u32)) -> SandboxResult {
    const STEPS: i64 = 30;
    for step in 1..=STEPS {
        let along = |a: u32, b: u32| {
            let at = i64::from(a) + (i64::from(b) - i64::from(a)) * step / STEPS;
            u32::try_from(at).unwrap_or_default()
        };
        input.move_to(along(from.0, to.0), along(from.1, to.1))?;
        thread::sleep(Duration::from_millis(12));
    }
    Ok(())
}

/// The files' URI list (RFC 2483) and their plain paths, a line each.
fn file_lists(paths: &[PathBuf]) -> (String, String) {
    use std::fmt::Write as _;
    paths
        .iter()
        .fold((String::new(), String::new()), |(mut uris, mut plain), path| {
            let shown = path.display().to_string();
            let _ = write!(uris, "file://{}\r\n", shown.replace(' ', "%20"));
            let _ = writeln!(plain, "{shown}");
            (uris, plain)
        })
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window's coordinates are small and positive"
)]
fn pixel(word: Option<&str>) -> u32 {
    word.and_then(|word| word.parse::<f32>().ok())
        .unwrap_or_default()
        .round()
        .max(0.0) as u32
}

/// Where the target says its message box's centre is.
fn box_centre(target: &mut Running, name: &str) -> SandboxResult<(u32, u32)> {
    let said = target
        .line_where(WAIT, |line| line.starts_with("box "))
        .map_err(|error| target.fail(&format!("{name}: no box: {error:?}")))?;
    let mut words = said.split(' ').skip(1);
    Ok((pixel(words.next()), pixel(words.next())))
}

/// `source`'s files dropped on a fresh chat's message box: staged with the draft kept, and the source told the drop
/// was taken (or, for a drag of no files, nothing highlighted and the drag cancelled).
fn file_manager_drop(sandbox: &Sandbox, source: &FileManager, paths: &[PathBuf]) -> SandboxResult {
    let (uris, plain) = file_lists(paths);
    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    sandbox.bus().start_file_transfer(TRANSFER_KEY, &refs)?;
    let mut target = Running::new(sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, LIGHT_KEY)])?)?;
    let to = box_centre(&mut target, source.name)?;
    expect(&mut target, "ready")?;
    let drag = sandbox.drag_source(ui_sandbox::drag::DragOptions {
        offers: (source.offers)(&uris, &plain),
        actions: source.actions,
        at: (40, 300),
        size: (240, 200),
        send_delay: source.delay,
    })?;
    let from = (160, 400);
    let mut input = sandbox.input()?;
    input.move_to(from.0, from.1)?;
    thread::sleep(Duration::from_millis(200));
    input.button_down(keys::BTN_LEFT)?;
    glide(&mut input, from, to)?;
    if source.delay.is_zero() {
        thread::sleep(Duration::from_millis(300));
        input.move_to(to.0 + 2, to.1)?;
        highlighted(&mut target, source, &drag)?;
    }
    // a slow source's drop comes before its files were read
    input.button_up(keys::BTN_LEFT)?;
    if let Some(mime) = source.takes {
        taken(sandbox, &mut target, source, &drag, mime)?;
    } else {
        refused(&mut target, source, &drag)?;
    }
    drop(drag);
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = target.finish(WAIT)?;
    if !status.success() {
        return Err(format!("{} target {status}: {stderr}", source.name).into());
    }
    Ok(())
}

fn failed(target: &mut Running, source: &FileManager, drag: &DragSource, what: &str) -> Box<dyn std::error::Error> {
    target.fail(&format!("{}: {what}; source saw {:?}", source.name, drag.events()))
}

/// The chat is highlighted while files are over it, and stays plain under a drag of text.
fn highlighted(target: &mut Running, source: &FileManager, drag: &DragSource) -> SandboxResult {
    let hover = target.line_where(
        if source.takes.is_some() {
            WAIT
        } else {
            Duration::from_secs(2)
        },
        |line| line == "hover true",
    );
    match (source.takes, hover) {
        (Some(_), Err(error)) => Err(failed(target, source, drag, &format!("no highlight: {error:?}"))),
        (None, Ok(_)) => Err(failed(target, source, drag, "text was taken for files")),
        _ => Ok(()),
    }
}

/// The files are staged, and the source saw its `mime` read, the drop taken and finished.
fn taken(
    sandbox: &Sandbox,
    target: &mut Running,
    source: &FileManager,
    drag: &DragSource,
    mime: &str,
) -> SandboxResult {
    target
        .line_where(WAIT, |line| line == "files ready 2")
        .map_err(|error| failed(target, source, drag, &format!("no files staged: {error:?}")))?;
    let finished = ui_sandbox::wait_until(WAIT, || drag.events().contains(&DragEvent::Finished).then_some(()));
    let seen = drag.events();
    // niri tells the source no accepted type (`target`); the read, the drop and its finish show the take
    if finished.is_none()
        || !seen.contains(&DragEvent::Sent(mime.to_owned()))
        || !seen.contains(&DragEvent::DropPerformed)
        || seen.contains(&DragEvent::Cancelled)
    {
        return Err(failed(
            target,
            source,
            drag,
            "the source wasn't told the drop was taken",
        ));
    }
    if !copied(&seen) {
        return Err(failed(target, source, drag, "the drop wasn't taken as a copy"));
    }
    if mime.starts_with("application/vnd.portal")
        && !sandbox
            .bus()
            .retrieved_transfers()
            .iter()
            .any(|key| key == TRANSFER_KEY)
    {
        return Err(failed(target, source, drag, "the portal was never asked for the files"));
    }
    Ok(())
}

/// The drop was a copy: a move would make the file manager delete the files it dropped.
fn copied(seen: &[DragEvent]) -> bool {
    let action = |event: &DragEvent| match event {
        DragEvent::Action(bits) => Some(*bits),
        _ => None,
    };
    let moved = seen
        .iter()
        .filter_map(action)
        .any(|bits| bits & DndAction::Move.bits() != 0);
    let last_before_drop = seen
        .iter()
        .take_while(|event| **event != DragEvent::DropPerformed)
        .filter_map(action)
        .last();
    !moved && last_before_drop == Some(DndAction::Copy.bits())
}

/// A drag of no files is refused: nothing read but a URI list, no drop performed, and the drag cancelled.
fn refused(target: &mut Running, source: &FileManager, drag: &DragSource) -> SandboxResult {
    let cancelled = ui_sandbox::wait_until(WAIT, || drag.events().contains(&DragEvent::Cancelled).then_some(()));
    let seen = drag.events();
    let read = seen
        .iter()
        .any(|event| matches!(event, DragEvent::Sent(mime) if mime != "text/uri-list"));
    if cancelled.is_none() || read || seen.contains(&DragEvent::DropPerformed) {
        return Err(failed(target, source, drag, "a drag of no files wasn't refused"));
    }
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

fn child(mode: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let source = mode == "source";
    let dark = mode == DARK_KEY;
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        teams_ui::window::init(cx);
        let opened = if source { open_source(cx) } else { open_target(dark, cx) };
        if let Err(error) = opened {
            eprintln!("open: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

struct FileSource(Vec<PathBuf>);
impl Render for FileSource {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("file-source")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(rgb(0x00dd_dddd))
            .text_color(rgb(0x0022_2222))
            .child("Drag these synthetic files")
            .child("made-up notes.txt")
            .child("made-up-report.pdf")
            .on_drag(self.0.clone(), |paths, _, _, cx| {
                cx.new(|_| ExternalPaths(paths.iter().cloned().collect()))
            })
            .external_drag_payload::<Vec<PathBuf>>(|paths, _, _| {
                Some(ExternalDragPayload::Files(FileDragPaths::new(
                    paths.iter().map(|path| (path.clone(), false)),
                )))
            })
    }
}

fn options(id: &str, cx: &mut App) -> WindowOptions {
    WindowOptions {
        app_id: Some(id.to_owned()),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1080.0), px(900.0)),
            cx,
        ))),
        ..WindowOptions::default()
    }
}

fn open_source(cx: &mut App) -> gpui::Result<()> {
    let files = std::env::var(FILES)
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .collect();
    let options = options("ngteams-file-source", cx);
    cx.open_window(options, |window, cx| {
        frames(window, 10, |_, _| say("ready"));
        cx.new(|_| FileSource(files))
    })?;
    Ok(())
}

struct Target {
    chat: Entity<ChatView>,
    compose: Entity<Compose>,
    core: Arc<common::FakeCore>,
}
impl Render for Target {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("AttachmentHarness")
            .size_full()
            .child(self.chat.clone())
            .on_action(cx.listener(|this, _: &SendFiles, window, cx| {
                this.compose.update(cx, |compose, cx| compose.send(window, cx));
                let actions = this.core.actions.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(teams_core::Action::Send { text, .. }) = actions.last()
                    && text.html() == "<p>Keep this draft</p>"
                {
                    say(&format!("sent {} draft preserved", text.files().len()));
                }
            }))
    }
}

fn open_target(dark: bool, cx: &mut App) -> gpui::Result<()> {
    cx.bind_keys([KeyBinding::new("f1", SendFiles, Some("AttachmentHarness"))]);
    let core = Arc::new(common::FakeCore::default());
    core.members.lock().unwrap_or_else(PoisonError::into_inner).insert(
        AB.to_owned(),
        vec![
            teams_store::Member::new(common::LAB_A, None),
            teams_store::Member::new(common::LAB_B, None),
        ],
    );
    let settings = cx.new(|_| {
        SettingsModel::in_memory(Settings {
            appearance: Appearance {
                follow_system: false,
                theme: if dark { DARK_KEY } else { LIGHT_KEY },
                ..Appearance::default()
            },
            ..Settings::default()
        })
    });
    let parts = ShellParts {
        core: core.clone() as Arc<dyn Core>,
        models: Models::new(cx),
        settings: settings.clone(),
        trace: Arc::new(ViewTrace::off()),
    };
    let follow = theme::follow_settings(&settings, cx);
    follow.detach();
    let options = options("ngteams-file-target", cx);
    cx.open_window(options, |window, cx| {
        let chat = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
        let compose = chat.read(cx).compose().clone();
        let hovering = Rc::new(Cell::new(false));
        cx.observe(&chat, move |chat, cx| {
            let now = chat.read(cx).file_drop_active();
            if hovering.replace(now) != now {
                say(&format!("hover {now}"));
            }
        })
        .detach();
        cx.observe(&compose, |compose, cx| {
            let files = compose.read(cx).attachments();
            if files.len() == 2
                && files
                    .iter()
                    .all(|file| matches!(file.state, teams_ui::compose::attachments::AttachmentState::Ready(_)))
            {
                say("files ready 2");
            }
        })
        .detach();
        let editor = compose.read(cx).editor().clone();
        editor.update(cx, |editor, cx| {
            editor.set_doc(
                teams_ui::editor::doc::Doc::new(vec![teams_ui::editor::doc::Block::new(
                    teams_ui::editor::doc::Kind::Paragraph,
                    0,
                    vec![teams_ui::editor::doc::Run::new(
                        "Keep this draft",
                        teams_ui::editor::doc::Attrs::default(),
                    )],
                )]),
                cx,
            );
        });
        let boxed = compose.clone();
        let view = cx.new(|_| Target { chat, compose, core });
        frames(window, 10, move |_, cx| {
            if let Some(bounds) = boxed.read(cx).box_bounds() {
                let centre = bounds.center();
                say(&format!("box {} {}", f32::from(centre.x), f32::from(centre.y)));
            }
            say("ready");
        });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;
    Ok(())
}
