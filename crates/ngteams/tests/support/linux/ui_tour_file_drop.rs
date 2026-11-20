//! Two made-up files dragged from a file manager onto the chat's message box in the app's own window, through the
//! compositor ([`ui_sandbox::drag`], what Dolphin offers): the highlight while they are over the chat, then the files
//! in the box beside the draft. The child says where the box is ([`line`]) and what it holds after the drop
//! ([`staged`]); the parent drags ([`hover`]), lets go ([`release`]) and checks the names ([`check`]).

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, thread};

use gpui::{App, Entity, Window};
use teams_ui::shell::{Main, Shell};
use ui_sandbox::drag::{DndAction, DragOptions, DragSource};
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{Sandbox, SandboxResult};

/// The step whose shot shows the highlight while the files are over the chat.
pub(super) const HOVER: &str = "chat-file-drop";
/// The step whose shot shows the files in the box after the drop.
pub(super) const DROPPED: &str = "chat-file-dropped";
/// The start of the lines the child says before those shots.
pub(super) const BOX: &str = "file-drop-box ";
pub(super) const STAGED: &str = "file-drop-staged ";
/// How many moves the pointer takes to the box.
const STEPS: u32 = 30;
/// The made-up files' names.
const NAMES: [&str; 2] = ["made-up notes.txt", "made-up-report.pdf"];

/// Where the shown chat's message box is, as the child says it: its centre (logical px) and the window's scale.
pub(super) fn line(shell: &Entity<Shell>, window: &Window, cx: &App) -> String {
    let shell = shell.read(cx);
    let (Main::Chat(_), Some(chat)) = (shell.main(), shell.chat()) else {
        return format!("{BOX}none");
    };
    let Some(bounds) = chat.read(cx).compose().read(cx).box_bounds() else {
        return format!("{BOX}none");
    };
    let centre = bounds.center();
    format!(
        "{BOX}{} {} {}",
        f32::from(centre.x),
        f32::from(centre.y),
        window.scale_factor()
    )
}

/// What the child says before the shot of step `name`: where the box is, or what it holds after the drop.
pub(super) fn said(name: &str, shell: &Entity<Shell>, window: &Window, cx: &App) -> String {
    if name == HOVER {
        line(shell, window, cx)
    } else {
        staged(shell, cx)
    }
}

/// The names of the files in the shown chat's box, as the child says them.
pub(super) fn staged(shell: &Entity<Shell>, cx: &App) -> String {
    let names = shell
        .read(cx)
        .chat()
        .map(|chat| {
            let compose = chat.read(cx).compose().read(cx);
            compose
                .attachments()
                .iter()
                .map(|file| file.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    format!("{STAGED}{}", names.join("|"))
}

/// A drag the parent holds over the chat until [`release`].
pub(super) struct Held {
    source: DragSource,
    input: VirtualInput,
}

/// Makes the files, shows Dolphin's drag of them at the screen's left, and drags them over the box the child named
/// (`said`), holding the button there.
pub(super) fn hover(sandbox: &Sandbox, said: &str) -> SandboxResult<Held> {
    let mut words = said
        .strip_prefix(BOX)
        .ok_or("not a file-drop-box line")?
        .split(' ')
        .map(str::parse::<f32>);
    let (Some(Ok(x)), Some(Ok(y)), Some(Ok(scale))) = (words.next(), words.next(), words.next()) else {
        return Err(format!("no message box to drop on: {said}").into());
    };
    let to = (device(x, scale), device(y, scale));
    let paths: Vec<PathBuf> = NAMES.iter().map(|name| sandbox.path(name)).collect();
    for path in &paths {
        fs::write(path, b"A synthetic attachment, not a real document.\n")?;
    }
    let uris: String = paths.iter().map(|path| uri(path)).collect();
    let source = sandbox.drag_source(DragOptions {
        offers: vec![
            ("text/uri-list".to_owned(), uris.clone().into_bytes()),
            ("application/x-kde4-urilist".to_owned(), uris.into_bytes()),
        ],
        actions: DndAction::Copy | DndAction::Move,
        at: (20, 560),
        size: (200, 160),
        send_delay: Duration::ZERO,
    })?;
    let mut input = sandbox.input()?;
    let from = (120, 640);
    input.move_to(from.0, from.1)?;
    thread::sleep(Duration::from_millis(200));
    input.button_down(keys::BTN_LEFT)?;
    for step in 1..=STEPS {
        let along = |a: u32, b: u32| {
            let at = i64::from(a) + (i64::from(b) - i64::from(a)) * i64::from(step) / i64::from(STEPS);
            u32::try_from(at).unwrap_or_default()
        };
        input.move_to(along(from.0, to.0), along(from.1, to.1))?;
        thread::sleep(Duration::from_millis(12));
    }
    thread::sleep(Duration::from_millis(400));
    input.move_to(to.0 + 2, to.1)?;
    // the file list is read and the highlight drawn
    thread::sleep(Duration::from_millis(900));
    Ok(Held { source, input })
}

/// Lets the files go on the box; the source is shown until the drop is finished.
pub(super) fn release(mut held: Held) -> SandboxResult {
    held.input.button_up(keys::BTN_LEFT)?;
    let finished = ui_sandbox::wait_until(Duration::from_secs(10), || {
        held.source
            .events()
            .contains(&ui_sandbox::drag::DragEvent::Finished)
            .then_some(())
    });
    if finished.is_none() {
        return Err(format!("the drop was never finished: {:?}", held.source.events()).into());
    }
    Ok(())
}

/// Both files are in the box after the drop, as the child said.
pub(super) fn check(said: &str) -> SandboxResult {
    let mut staged: Vec<&str> = said
        .strip_prefix(STAGED)
        .ok_or("not a file-drop-staged line")?
        .split('|')
        .collect();
    staged.sort_unstable();
    if staged != NAMES {
        return Err(format!("the box holds {staged:?} after the drop, not {NAMES:?}").into());
    }
    Ok(())
}

fn uri(path: &Path) -> String {
    format!("file://{}\r\n", path.display().to_string().replace(' ', "%20"))
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a window's coordinates are small and positive"
)]
fn device(logical: f32, scale: f32) -> u32 {
    (logical * scale).round().max(0.0) as u32
}
