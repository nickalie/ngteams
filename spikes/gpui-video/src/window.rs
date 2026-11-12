//! M0 Tasks 15–16: GPUI on niri/Wayland, the smoke window and the video grid benchmark.
//!
//! Without arguments (Task 15's smoke check) it opens one window on the Wayland compositor `WAYLAND_DISPLAY` names,
//! draws a line of text and a checkerboard image uploaded as a GPUI `RenderImage`, and once a few frames have been
//! presented prints one line to stdout:
//!
//! ```text
//! ready compositor=<name> gpu=<device> driver=<driver> software=<bool>
//! ```
//!
//! (`compositor` is what GPUI calls the display server: `Wayland`.) It quits when its window is closed, or after
//! `--exit-after-ms <N>`.
//!
//! With `--bench` (Task 16) the window shows a 3×3 grid of synthetic 720p streams at `--fps` (30), each frame handed
//! to GPUI by `--upload surface` (approach (b), the default: a `PixelFrame` painted with the patched GPUI's
//! `Window::paint_external_surface`) or `--upload image` (approach (a): a fresh `RenderImage`, stock GPUI's only way);
//! see `gpui_video::grid`. After the same ready line it warms up for
//! `--warmup-ms`, prints `measuring`, measures for `--duration-ms`, prints one `summary key=value ...` line, keeps
//! the streams running for `--hold-ms` (0; the benchmark runner takes its screenshots then, outside the measured
//! window) and quits. The markers `bench-phase measuring` and `bench-phase done` go to stderr too, so GPUI's own
//! `frame duration: ...` lines (printed with `ZED_MEASUREMENTS=1`) can be cut to the measured window.
//!
//! It refuses to start (exit 1) when `WAYLAND_DISPLAY` is unset or empty. The build has no X11 backend and allows only
//! Wayland (`WindowingModes::WAYLAND`), so it never falls back to X11 or to running headless: a `WAYLAND_DISPLAY` it
//! can't connect to makes GPUI panic at startup. GPUI's log goes to stderr as `[LEVEL target] message`: GPUI's own
//! records from info up, everyone else's from warn up.
//!
//! It also refuses (exit 1) any compositor but a private one: `WAYLAND_DISPLAY` must be an absolute socket path
//! inside the dir `UI_SANDBOX_DIR` names (`ui_sandbox::window_allowed`). The UI sandbox (`ui_sandbox`, a headless
//! weston with a nested niri) sets both; a desktop session has no `UI_SANDBOX_DIR` and names its socket bare
//! (`wayland-1`), so the binary doesn't open a window there. The GPU is
//! chosen by GPUI itself: `ZED_DEVICE_ID=<pci device id>` puts that adapter first, otherwise the compositor's GPU, then
//! a discrete one.

use std::{
    cell::Cell,
    io::Write as _,
    process::ExitCode,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    App, AppContext as _, AsyncApp, Bounds, Context, GpuSpecs, IntoElement, ParentElement as _, Render, RenderImage,
    SharedString, Styled as _, Window, WindowBounds, WindowHandle, WindowOptions, WindowingModes, div, img, px, rgb,
    size,
};
use gpui_video::grid::{Grid, Source, TILES, Upload, VideoFrame};
use image::{Frame, ImageBuffer, Rgba};

/// The smoke window's Wayland app id (the window has no title).
const APP_ID: &str = "ngteams-gpui-smoke";
/// The benchmark window's Wayland app id.
const BENCH_APP_ID: &str = "ngteams-gpui-video";
/// The text line the window draws.
const TEXT: &str = "ngteams GPUI smoke";
/// Window background, 0xRRGGBB.
const BACKGROUND: u32 = 0x0020_2830;
/// Text colour, 0xRRGGBB: white, which the checkerboard never uses.
const TEXT_COLOR: u32 = 0x00ff_ffff;
/// The checkerboard's two colours, 0xRRGGBB: magenta and cyan, which neither the background nor the text uses.
const TILE_COLORS: [u32; 2] = [0x00ff_00ff, 0x0000_ffff];
/// The checkerboard's side and tile side, in pixels.
const IMAGE_SIDE: u32 = 128;
const TILE_SIDE: u32 = 16;
/// Frames presented before the window reports ready.
const READY_FRAMES: usize = 3;
const USAGE: &str = "usage: gpui-video [--exit-after-ms N] | gpui-video --bench [--upload image|surface] \
                     [--warmup-ms N] [--duration-ms N] [--hold-ms N] [--fps N]";

/// What `--bench` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bench {
    upload: Upload,
    warmup: Duration,
    duration: Duration,
    /// How long the streams keep running after the summary.
    hold: Duration,
    fps: u32,
}

impl Default for Bench {
    fn default() -> Self {
        Self {
            upload: Upload::Surface,
            warmup: Duration::from_secs(3),
            duration: Duration::from_secs(20),
            hold: Duration::ZERO,
            fps: 30,
        }
    }
}

/// Command-line options.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Options {
    /// Quit this long after the window reports ready, unless it's closed first (smoke window only).
    exit_after: Option<Duration>,
    /// Run the video grid benchmark instead of the smoke window.
    bench: Option<Bench>,
}

/// The number following `flag`.
fn number<T: std::str::FromStr>(flag: &str, value: Option<String>) -> Result<T, String>
where
    T::Err: std::fmt::Display,
{
    let value = value.ok_or_else(|| format!("{flag} needs a value"))?;
    value.parse().map_err(|error| format!("{flag} {value}: {error}"))
}

/// The upload path following `--upload`.
fn upload(value: Option<String>) -> Result<Upload, String> {
    let value = value.ok_or("--upload needs a value")?;
    Upload::from_name(&value).ok_or_else(|| format!("--upload {value}: not image or surface"))
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut bench = Bench::default();
    let (mut wants_bench, mut bench_flags) = (false, false);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--exit-after-ms" => {
                options.exit_after = Some(Duration::from_millis(number(&arg, args.next())?));
            }
            "--bench" => wants_bench = true,
            "--warmup-ms" => bench.warmup = Duration::from_millis(number(&arg, args.next())?),
            "--duration-ms" => bench.duration = Duration::from_millis(number(&arg, args.next())?),
            "--hold-ms" => bench.hold = Duration::from_millis(number(&arg, args.next())?),
            "--fps" => bench.fps = number::<u32>(&arg, args.next())?.max(1),
            "--upload" => bench.upload = upload(args.next())?,
            other => return Err(format!("unknown argument: {other} ({USAGE})")),
        }
        bench_flags |= arg.starts_with("--") && !matches!(arg.as_str(), "--bench" | "--exit-after-ms");
    }
    if wants_bench && options.exit_after.is_some() {
        return Err(format!("--exit-after-ms is for the smoke window ({USAGE})"));
    }
    if bench_flags && !wants_bench {
        return Err(format!(
            "--warmup-ms, --duration-ms, --hold-ms, --fps and --upload need --bench ({USAGE})"
        ));
    }
    options.bench = wants_bench.then_some(bench);
    Ok(options)
}

/// A `side`×`side` checkerboard of `tile`-pixel squares in the two `colors` (0xRRGGBB, top-left first), as the BGRA
/// bytes GPUI's `RenderImage` holds (in an `image` buffer typed RGBA).
fn checkerboard_bgra(side: u32, tile: u32, colors: [u32; 2]) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let bgra = colors.map(|color| {
        let [_, red, green, blue] = color.to_be_bytes();
        Rgba([blue, green, red, u8::MAX])
    });
    ImageBuffer::from_fn(side, side, |x, y| bgra[usize::from((x / tile + y / tile) % 2 == 1)])
}

/// The line printed once the window has presented [`READY_FRAMES`] frames.
fn ready_line(compositor: &str, gpu: Option<&GpuSpecs>) -> String {
    match gpu {
        Some(gpu) => format!(
            "ready compositor={compositor} gpu={:?} driver={:?} software={}",
            gpu.device_name, gpu.driver_name, gpu.is_software_emulated
        ),
        None => format!("ready compositor={compositor} gpu=unknown driver=unknown software=unknown"),
    }
}

/// The window's view: a line of text over a checkerboard image.
struct Smoke {
    text: SharedString,
    image: Arc<RenderImage>,
}

impl Render for Smoke {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let side = px(f32::from(u16::try_from(IMAGE_SIDE).unwrap_or(u16::MAX)));
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_8()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT_COLOR))
            .text_3xl()
            .child(self.text.clone())
            .child(img(self.image.clone()).w(side).h(side))
    }
}

/// Logs GPUI's records to stderr as `[LEVEL target] message`.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log_level(metadata.target())
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

/// The most detailed level logged for `target`: info for GPUI (its adapter choice), warn for the rest (zbus's
/// handshake chatter, reaching `log` through tracing's `log` feature).
fn log_level(target: &str) -> log::LevelFilter {
    if target.starts_with("gpui") {
        log::LevelFilter::Info
    } else {
        log::LevelFilter::Warn
    }
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

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// Prints the ready line and arms the `exit_after` timer.
fn report_ready(window: &Window, cx: &mut App, exit_after: Option<Duration>) {
    say(&ready_line(cx.compositor_name(), window.gpu_specs().as_ref()));
    if let Some(delay) = exit_after {
        let timer = cx.background_executor().timer(delay);
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
}

fn open_window(cx: &mut App, exit_after: Option<Duration>) -> Result<WindowHandle<Smoke>, String> {
    let bounds = Bounds::centered(None, size(px(640.0), px(400.0)), cx);
    let image = Arc::new(RenderImage::new(vec![Frame::new(checkerboard_bgra(
        IMAGE_SIDE,
        TILE_SIDE,
        TILE_COLORS,
    ))]));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some(APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    cx.open_window(options, |window, cx| {
        after_frames(window, READY_FRAMES, move |window, cx| {
            report_ready(window, cx, exit_after);
        });
        cx.new(|_| Smoke {
            text: TEXT.into(),
            image,
        })
    })
    .map_err(|error| format!("{error:#}"))
}

/// Hands every frame the sources send to the grid, a batch per wake-up, and asks for a redraw.
async fn feed_grid(handle: WindowHandle<Grid>, frames: async_channel::Receiver<VideoFrame>, cx: &mut AsyncApp) {
    while let Ok(first) = frames.recv().await {
        let mut batch = vec![first];
        while let Ok(frame) = frames.try_recv() {
            batch.push(frame);
        }
        let pushed = handle.update(cx, |grid, window, cx| {
            for frame in batch {
                if let Err(error) = grid.push(frame, window) {
                    log::error!("{error}");
                }
            }
            cx.notify();
        });
        if pushed.is_err() {
            return;
        }
    }
}

/// Warms up, measures, prints the summary, stops the sources and quits.
async fn drive_bench(handle: WindowHandle<Grid>, bench: Bench, source: Source, cx: &mut AsyncApp) {
    cx.background_executor().timer(bench.warmup).await;
    let started = handle.update(cx, |grid, _, _| grid.start_measuring(Instant::now()));
    if started.is_ok() {
        say("measuring");
        eprintln!("bench-phase measuring");
        cx.background_executor().timer(bench.duration).await;
        if let Ok(summary) = handle.update(cx, |grid, _, _| grid.summary(Instant::now())) {
            eprintln!("bench-phase done");
            say(&summary);
        }
        cx.background_executor().timer(bench.hold).await;
    }
    source.stop();
    cx.update(|cx| cx.quit());
}

/// Opens the benchmark window, starts the sources and the driver.
fn open_bench(cx: &mut App, bench: Bench) -> Result<(), String> {
    let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some(BENCH_APP_ID.to_owned()),
        ..WindowOptions::default()
    };
    let handle = cx
        .open_window(options, |window, cx| {
            after_frames(window, READY_FRAMES, |window, cx| report_ready(window, cx, None));
            cx.new(|_| Grid::new(bench.upload))
        })
        .map_err(|error| format!("{error:#}"))?;
    let (sender, frames) = async_channel::unbounded();
    let source = Source::start(TILES, bench.fps, &sender).map_err(|error| format!("starting the sources: {error}"))?;
    cx.spawn(async move |cx| feed_grid(handle, frames, cx).await).detach();
    cx.spawn(async move |cx| drive_bench(handle, bench, source, cx).await)
        .detach();
    Ok(())
}

fn run(options: Options) -> ExitCode {
    let failed = Rc::new(Cell::new(false));
    let failed_in_app = Rc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let opened = match options.bench {
            Some(bench) => open_bench(cx, bench),
            None => open_window(cx, options.exit_after).map(|_| ()),
        };
        if let Err(error) = opened {
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

pub(crate) fn main() -> ExitCode {
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
    let options = match parse_options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            log::error!("{error}");
            return ExitCode::from(2);
        }
    };
    if let Err(error) = ui_sandbox::window_allowed() {
        log::error!("{error}");
        return ExitCode::FAILURE;
    }
    run(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn options_default_to_the_smoke_window_without_an_exit_timer() {
        assert_eq!(parse_options(args(&[])), Ok(Options::default()));
    }

    #[test]
    fn exit_after_takes_milliseconds() {
        let options = parse_options(args(&["--exit-after-ms", "1500"])).unwrap();
        assert_eq!(options.exit_after, Some(Duration::from_millis(1500)));
        assert_eq!(options.bench, None);
    }

    #[test]
    fn bench_takes_its_defaults_and_overrides() {
        let options = parse_options(args(&["--bench"])).unwrap();
        assert_eq!(options.bench, Some(Bench::default()));
        let options = parse_options(args(&[
            "--warmup-ms",
            "500",
            "--bench",
            "--duration-ms",
            "1500",
            "--fps",
            "60",
            "--hold-ms",
            "4000",
            "--upload",
            "image",
        ]))
        .unwrap();
        assert_eq!(
            options.bench,
            Some(Bench {
                upload: Upload::Image,
                warmup: Duration::from_millis(500),
                duration: Duration::from_millis(1500),
                hold: Duration::from_millis(4000),
                fps: 60,
            })
        );
        assert_eq!(
            parse_options(args(&["--bench", "--fps", "0"]))
                .unwrap()
                .bench
                .unwrap()
                .fps,
            1
        );
    }

    #[test]
    fn bad_arguments_are_refused() {
        let refused = |list: &[&str], needle: &str| {
            let error = parse_options(args(list)).unwrap_err();
            assert!(error.contains(needle), "{list:?}: {error}");
        };
        refused(&["--exit-after-ms"], "needs a value");
        refused(&["--exit-after-ms", "soon"], "soon");
        refused(&["--x11"], "unknown argument: --x11");
        refused(&["--duration-ms", "100"], "need --bench");
        refused(&["--bench", "--exit-after-ms", "100"], "smoke window");
        refused(&["--bench", "--fps", "-3"], "--fps -3");
        refused(&["--bench", "--upload", "dmabuf"], "--upload dmabuf");
        refused(&["--bench", "--upload"], "--upload needs a value");
        refused(&["--upload", "image"], "need --bench");
    }

    #[test]
    fn checkerboard_alternates_tiles_in_bgra() {
        let board = checkerboard_bgra(32, 16, [0x0011_2233, 0x0044_5566]);
        assert_eq!(board.dimensions(), (32, 32));
        let first = Rgba([0x33, 0x22, 0x11, 0xff]);
        let second = Rgba([0x66, 0x55, 0x44, 0xff]);
        assert_eq!(*board.get_pixel(0, 0), first);
        assert_eq!(*board.get_pixel(15, 15), first);
        assert_eq!(*board.get_pixel(16, 0), second);
        assert_eq!(*board.get_pixel(0, 16), second);
        assert_eq!(*board.get_pixel(31, 31), first);
    }

    #[test]
    fn smoke_colours_stay_apart() {
        assert!(!TILE_COLORS.contains(&TEXT_COLOR));
        assert!(!TILE_COLORS.contains(&BACKGROUND));
        assert_ne!(TEXT_COLOR, BACKGROUND);
    }

    #[test]
    fn gpui_logs_from_info_and_the_rest_from_warn() {
        assert_eq!(log_level("gpui_wgpu::wgpu_context"), log::LevelFilter::Info);
        assert_eq!(log_level("gpui_linux::linux::platform"), log::LevelFilter::Info);
        assert_eq!(log_level("zbus::connection::handshake::common"), log::LevelFilter::Warn);
        assert_eq!(log_level("tracing::span"), log::LevelFilter::Warn);
    }

    #[test]
    fn ready_line_names_compositor_and_gpu() {
        let gpu = GpuSpecs {
            is_software_emulated: false,
            device_name: "Intel(R) Graphics".to_owned(),
            driver_name: "Intel open-source Mesa driver".to_owned(),
            driver_info: String::new(),
        };
        assert_eq!(
            ready_line("Wayland", Some(&gpu)),
            "ready compositor=Wayland gpu=\"Intel(R) Graphics\" driver=\"Intel open-source Mesa driver\" software=false"
        );
        assert_eq!(
            ready_line("weston", None),
            "ready compositor=weston gpu=unknown driver=unknown software=unknown"
        );
    }
}
