//! The sandbox's own test window: a GPUI window filled with one colour that reports on stdout what it gets, so the
//! sandbox's tests can check the compositor, the virtual input, the opener and the bus from outside.
//!
//! - `ui-sandbox-probe [--open-url URL]`: opens the window (only inside a sandbox: `ui_sandbox::window_allowed`) and
//!   prints `ready` once it has presented a few frames, `active true|false` as it gains and loses focus,
//!   `key ctrl=<b> shift=<b> alt=<b> name=<key> char=<Some("x")|None>` per key down and
//!   `down x=<x> y=<y> button=<button>` per mouse button down (window pixels); on Ctrl+V, what GPUI reads from the
//!   clipboard, `clipboard empty` or `clipboard` and an entry each (`text="…"`, `image=<mime>:<bytes>`,
//!   `paths=[…]`). With `--open-url`, it asks GPUI's
//!   `open_url` for `URL` once ready and prints `open-url`. It quits when the window closes.
//! - `ui-sandbox-probe --list-fds [--write-fd N]`: opens no window; prints `fds <n> <n> ...`, the descriptors it
//!   inherited, then writes `hello from the sandbox` to descriptor `N` (a pipe) and exits.
//!
//! GPUI's records go to stderr as `[LEVEL target] message`: GPUI's own from info up, everyone else's from warn up.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use gpui::{
    App, AppContext as _, Bounds, ClipboardEntry, ClipboardItem, Context, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, Render, Styled as _, Window, WindowBounds, WindowOptions,
    WindowingModes, div, px, rgb, size,
};

/// The probe window's Wayland app id.
const APP_ID: &str = "ngteams-ui-sandbox-probe";
/// The window's colour, 0xRRGGBB: a green no compositor background uses.
const COLOR: u32 = 0x0020_c060;
/// Frames presented before the window reports ready.
const READY_FRAMES: usize = 3;
/// What `--write-fd` writes.
const HELLO: &str = "hello from the sandbox\n";
const USAGE: &str = "usage: ui-sandbox-probe [--open-url URL] | ui-sandbox-probe --list-fds [--write-fd N]";

/// Command-line options.
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    open_url: Option<String>,
    list_fds: bool,
    write_fd: Option<u32>,
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--open-url" => options.open_url = Some(args.next().ok_or("--open-url needs a URL")?),
            "--list-fds" => options.list_fds = true,
            "--write-fd" => {
                let fd = args.next().ok_or("--write-fd needs a descriptor")?;
                options.write_fd = Some(fd.parse().map_err(|error| format!("--write-fd {fd}: {error}"))?);
            }
            other => return Err(format!("unknown argument: {other} ({USAGE})")),
        }
    }
    if options.write_fd.is_some() && !options.list_fds {
        return Err(format!("--write-fd needs --list-fds ({USAGE})"));
    }
    Ok(options)
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// The descriptors this process holds, but the one reading `/proc/self/fd`.
fn open_fds() -> Result<Vec<u32>, String> {
    let dir = Path::new("/proc/self/fd");
    let entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| format!("{}: {error}", dir.display()))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    let mut fds: Vec<u32> = entries
        .iter()
        .filter(|path| fs::read_link(path).is_ok())
        .filter_map(|path| path.file_name()?.to_str()?.parse().ok())
        .collect();
    fds.sort_unstable();
    Ok(fds)
}

/// `--list-fds`: prints the descriptors, then writes [`HELLO`] to `write_fd` (reopened through `/proc`).
fn list_fds(write_fd: Option<u32>) -> ExitCode {
    match open_fds() {
        Ok(fds) => {
            let listed: Vec<String> = fds.iter().map(ToString::to_string).collect();
            say(&format!("fds {}", listed.join(" ")));
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    }
    if let Some(fd) = write_fd {
        let path = format!("/proc/self/fd/{fd}");
        let written = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|mut pipe| pipe.write_all(HELLO.as_bytes()));
        if let Err(error) = written {
            eprintln!("{path}: {error}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
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

/// The window's view: one colour, every key and click reported.
struct Probe {
    focus: FocusHandle,
}

/// Prints a key down.
fn report_key(event: &KeyDownEvent) {
    let keystroke = &event.keystroke;
    let modifiers = &keystroke.modifiers;
    say(&format!(
        "key ctrl={} shift={} alt={} name={} char={:?}",
        modifiers.control, modifiers.shift, modifiers.alt, keystroke.key, keystroke.key_char
    ));
}

/// What a clipboard read gave, as the probe prints it.
fn describe_clipboard(item: Option<&ClipboardItem>) -> String {
    let Some(item) = item else {
        return "clipboard empty".to_owned();
    };
    let entries: Vec<String> = item
        .entries()
        .iter()
        .map(|entry| match entry {
            ClipboardEntry::String(text) => format!("text={:?}", text.text()),
            ClipboardEntry::Image(image) => format!("image={}:{}", image.format.mime_type(), image.bytes.len()),
            ClipboardEntry::ExternalPaths(paths) => format!("paths={:?}", paths.paths()),
        })
        .collect();
    format!("clipboard {}", entries.join(" "))
}

/// Prints what the clipboard holds on Ctrl+V.
fn report_paste(event: &KeyDownEvent, cx: &App) {
    let keystroke = &event.keystroke;
    if keystroke.modifiers.control && keystroke.key == "v" {
        say(&describe_clipboard(cx.read_from_clipboard().as_ref()));
    }
}

/// Prints a mouse button down.
fn report_down(event: &MouseDownEvent) {
    say(&format!(
        "down x={:.0} y={:.0} button={:?}",
        event.position.x.as_f32(),
        event.position.y.as_f32(),
        event.button
    ));
}

impl Render for Probe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(COLOR))
            .track_focus(&self.focus)
            .on_key_down(|event, _, cx| {
                report_key(event);
                report_paste(event, cx);
            })
            .on_mouse_down(MouseButton::Left, |event, _, _| report_down(event))
            .on_mouse_down(MouseButton::Right, |event, _, _| report_down(event))
    }
}

/// Prints `ready`, then asks for `open_url` if there is one.
fn report_ready(cx: &App, open_url: Option<&str>) {
    say("ready");
    if let Some(url) = open_url {
        cx.open_url(url);
        say("open-url");
    }
}

fn open_window(cx: &mut App, open_url: Option<String>) -> Result<(), String> {
    let bounds = Bounds::centered(None, size(px(640.0), px(400.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some(APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        after_frames(window, READY_FRAMES, move |_, cx| report_ready(cx, open_url.as_deref()));
        let view = cx.new(|cx| {
            cx.observe_window_activation(window, |_, window, _| {
                say(&format!("active {}", window.is_window_active()));
            })
            .detach();
            Probe {
                focus: cx.focus_handle(),
            }
        });
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        view
    })
    .map(|_| ())
    .map_err(|error| format!("{error:#}"))
}

fn run_window(open_url: Option<String>) -> ExitCode {
    let failed = std::rc::Rc::new(std::cell::Cell::new(false));
    let failed_in_app = std::rc::Rc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open_window(cx, open_url) {
            log::error!("failed to open the window: {error}");
            failed_in_app.set(true);
            cx.quit();
        }
    });
    if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Logs to stderr as `[LEVEL target] message`.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        let most = if metadata.target().starts_with("gpui") {
            log::Level::Info
        } else {
            log::Level::Warn
        };
        metadata.level() <= most
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "[{} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

pub(super) fn main() -> ExitCode {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            log::error!("{error}");
            return ExitCode::from(2);
        }
    };
    if options.list_fds {
        return list_fds(options.write_fd);
    }
    if let Err(error) = ui_sandbox::window_allowed() {
        log::error!("{error}");
        return ExitCode::FAILURE;
    }
    run_window(options.open_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_options(args.iter().map(|&arg| arg.to_owned()))
    }

    #[test]
    fn options_parse() {
        assert_eq!(parse(&[]).unwrap(), Options::default());
        assert_eq!(
            parse(&["--open-url", "https://example.org/"])
                .unwrap()
                .open_url
                .as_deref(),
            Some("https://example.org/")
        );
        let fds = parse(&["--list-fds", "--write-fd", "7"]).unwrap();
        assert!(fds.list_fds);
        assert_eq!(fds.write_fd, Some(7));
        assert!(parse(&["--write-fd", "7"]).is_err());
        assert!(parse(&["--open-url"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }
}
