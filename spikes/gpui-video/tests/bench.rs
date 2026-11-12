//! M0 Task 16: the video grid benchmark, run in the private headless weston + nested niri of
//! [`gpui_video::sandbox`], never on the desktop session. Both upload paths: `image` (approach (a), stock GPUI's
//! `RenderImage`) and `surface` (approach (b), the patched GPUI's external surface).
//!
//! - `bench_mode_runs_and_reads_back` (runs with the suite, on the default GPU): a short `--bench` run per upload
//!   path. The 9 tiles' burned-in counters are read back from two window screenshots taken a second apart (after the
//!   measured phase, while the binary holds), so the frames really reach the screen and keep changing; the summary
//!   line and GPUI's `frame duration` lines are there.
//! - `video_grid_benchmark` (`#[ignore]`, run by hand with a release build): the measurement, on every GPU or on
//!   `GPUI_BENCH_GPU=<pci device id>`, each upload path or `GPUI_BENCH_UPLOAD=image|surface`, for
//!   `GPUI_BENCH_SECONDS` (20) after a 3 s warm-up. It reports per run: draws
//!   and video frames per second, frames skipped, latency from source to draw, draw-to-draw intervals, GPUI's own
//!   frame duration (draw + present on the CPU, `ZED_MEASUREMENTS=1`), CPU per thread group from schedstat, GPU memory
//!   (DRM fdinfo, and `nvidia-smi` for the NVIDIA driver), the CPUs the process may run on and the load average. The
//!   report goes to stderr and is appended to `GPUI_BENCH_OUT` when set.
//!
//!   `cargo test --release -p gpui-video --test bench -- --ignored --nocapture`
//!
//! Frame pacing: weston's headless output repaints on a 60 Hz timer (no vblank), niri draws on weston's frame
//! callback, GPUI on niri's. So draws are capped at 60 per second and a frame's latency includes waiting for the next
//! callback (up to 16.7 ms), as on a 60 Hz display; what the headless setup can't show is scan-out and a real
//! display's vblank jitter.
#![cfg(target_os = "linux")]

use std::{
    collections::{BTreeMap, HashSet},
    fmt::Write as _,
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use gpui_video::{
    bench::{
        Fields, Stats, cpu_percent, fdinfo_gpu_memory, nvidia_smi_memory, parse_fields, parse_frame_duration,
        parse_schedstat_runtime, per_second,
    },
    frames::{Rect, read_counter},
    grid::{TILES, tile_rect},
    sandbox::{
        Gpu, NIRI_FULL_WINDOW, Ready, Running, Sandbox, SandboxOptions, SandboxResult, check_selected_gpu,
        render_node_gpus, sandbox_or_skip, wait_ready,
    },
};

const BINARY: &str = env!("CARGO_BIN_EXE_gpui-video");
/// From launch to the ready line (a cold dGPU wakes up within it).
const STARTUP: Duration = Duration::from_secs(30);
/// Slack on top of a phase's own length before a missing line is an error.
const SLACK: Duration = Duration::from_secs(15);
/// How long the binary keeps the streams running after its summary: the read-back screenshots are taken then, so
/// they don't load the measured phase.
const HOLD: Duration = Duration::from_secs(8);

fn bench_sandbox_options() -> SandboxOptions {
    SandboxOptions {
        width: 1920,
        height: 1080,
        refresh_mhz: Some(60_000),
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    }
}

/// The screenshot's size and the counters of the 9 tiles in a window screenshot, the grid laid out over `viewport`
/// (the window's size as the binary saw it) from the top-left. With `GPUI_BENCH_SHOTS=<dir>` the screenshot is kept
/// there.
fn screenshot_counters(png: &Path, viewport: Size) -> SandboxResult<(Size, Counters)> {
    if let (Some(dir), Some(name)) = (std::env::var_os("GPUI_BENCH_SHOTS"), png.file_name()) {
        fs::copy(png, Path::new(&dir).join(name))?;
    }
    let shot = image::open(png)?.to_luma8();
    let area = Rect::new(0, 0, viewport.0, viewport.1);
    let counters = (0..TILES)
        .map(|index| {
            read_counter(tile_rect(index, area), |x, y| {
                (x < shot.width() && y < shot.height()).then(|| shot.get_pixel(x, y).0[0])
            })
        })
        .collect();
    Ok((shot.dimensions(), counters))
}

/// A size in pixels, (width, height).
type Size = (u32, u32);
/// The counter read from each tile, `None` where it couldn't be read.
type Counters = Vec<Option<u32>>;

/// Two window screenshots a second apart, read back; and the screenshot's size.
fn read_back(sandbox: &Sandbox, label: &str, viewport: Size) -> SandboxResult<(Size, [Counters; 2])> {
    let (size, first) = screenshot_counters(&sandbox.screenshot(&format!("{label}-1.png"), true)?, viewport)?;
    thread::sleep(Duration::from_secs(1));
    let (_, second) = screenshot_counters(&sandbox.screenshot(&format!("{label}-2.png"), true)?, viewport)?;
    Ok((size, [first, second]))
}

/// `<width>x<height>`.
fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

/// CPU time of each live thread of `pid`, by thread id, with its name.
fn thread_cpu(pid: u32) -> BTreeMap<u32, (String, Duration)> {
    let mut threads = BTreeMap::new();
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return threads;
    };
    for task in tasks.flatten() {
        let Some(tid) = task.file_name().to_str().and_then(|name| name.parse().ok()) else {
            continue;
        };
        let name = fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        let runtime = fs::read_to_string(task.path().join("schedstat"))
            .ok()
            .and_then(|text| parse_schedstat_runtime(&text));
        if let Some(runtime) = runtime {
            threads.insert(tid, (name.trim().to_owned(), runtime));
        }
    }
    threads
}

/// CPU used between two [`thread_cpu`] snapshots, as (main thread, `video-src-*` threads, every other thread).
fn cpu_split(
    pid: u32,
    before: &BTreeMap<u32, (String, Duration)>,
    after: &BTreeMap<u32, (String, Duration)>,
) -> [Duration; 3] {
    let mut split = [Duration::ZERO; 3];
    for (tid, (name, runtime)) in after {
        let start = before.get(tid).map_or(Duration::ZERO, |(_, runtime)| *runtime);
        let group = if *tid == pid {
            0
        } else if name.starts_with("video-src") {
            1
        } else {
            2
        };
        split[group] += runtime.saturating_sub(start);
    }
    split
}

/// GPU memory of `pid`: DRM fdinfo (summed per driver over distinct clients), and `nvidia-smi`'s figure.
fn gpu_memory(pid: u32) -> BTreeMap<String, u64> {
    let mut memory = BTreeMap::new();
    let mut clients = HashSet::new();
    for fd in fs::read_dir(format!("/proc/{pid}/fdinfo"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Some((driver, client, bytes)) = fs::read_to_string(fd.path())
            .ok()
            .and_then(|text| fdinfo_gpu_memory(&text))
        else {
            continue;
        };
        if clients.insert((driver.clone(), client)) {
            *memory.entry(format!("fdinfo:{driver}")).or_insert(0) += bytes;
        }
    }
    let smi = std::process::Command::new("nvidia-smi")
        .args(["-q", "-d", "PIDS"])
        .output();
    if let Some(bytes) = smi
        .ok()
        .and_then(|output| nvidia_smi_memory(&String::from_utf8_lossy(&output.stdout), pid))
    {
        memory.insert("nvidia-smi".to_owned(), bytes);
    }
    memory
}

fn proc_field(pid: u32, file: &str, key: &str) -> String {
    fs::read_to_string(format!("/proc/{pid}/{file}"))
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix(key).map(|value| value.trim().to_owned()))
        .unwrap_or_else(|| "?".to_owned())
}

fn load_average() -> String {
    fs::read_to_string("/proc/loadavg")
        .map(|text| text.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

/// Everything one run measured.
struct Run {
    ready: Ready,
    stderr: String,
    /// The window's size, from its screenshot.
    window: Size,
    read_back: [Counters; 2],
    summary: Fields,
    frame_durations: Vec<Duration>,
    cpu: [Duration; 3],
    cpu_window: Duration,
    memory: BTreeMap<String, u64>,
    cpus_allowed: String,
    load: [String; 2],
}

/// GPUI's `frame duration` lines between the `bench-phase measuring` and `bench-phase done` markers.
fn measured_frame_durations(stderr: &str) -> Vec<Duration> {
    stderr
        .lines()
        .skip_while(|line| *line != "bench-phase measuring")
        .take_while(|line| *line != "bench-phase done")
        .filter_map(parse_frame_duration)
        .collect()
}

/// Waits for stdout line `wanted` (by prefix) within `timeout`.
fn expect_line(running: &Running, wanted: &str, timeout: Duration) -> SandboxResult<String> {
    let line = running
        .line(timeout)
        .map_err(|error| format!("waiting for {wanted:?}: {error:?}"))?;
    if line.starts_with(wanted) {
        Ok(line)
    } else {
        Err(format!("expected {wanted:?}, got {line:?}").into())
    }
}

/// One benchmark run: the GPU (`None`: GPUI's choice), the upload path (`image` or `surface`), the phases.
struct Case<'a> {
    gpu: Option<&'a Gpu>,
    upload: &'a str,
    warmup: Duration,
    duration: Duration,
}

/// Runs the benchmark binary once in `sandbox`.
fn run_bench(sandbox: &Sandbox, case: &Case<'_>) -> SandboxResult<Run> {
    let (warmup, duration) = (case.warmup, case.duration);
    let warmup_ms = warmup.as_millis().to_string();
    let duration_ms = duration.as_millis().to_string();
    let mut env = vec![("ZED_MEASUREMENTS", "1")];
    if let Some(gpu) = case.gpu {
        env.push(("ZED_DEVICE_ID", gpu.device.as_str()));
    }
    let hold_ms = HOLD.as_millis().to_string();
    let args = [
        "--bench",
        "--upload",
        case.upload,
        "--warmup-ms",
        &warmup_ms,
        "--duration-ms",
        &duration_ms,
        "--hold-ms",
        &hold_ms,
    ];
    let mut running = Running::new(sandbox.spawn(Path::new(BINARY), &args, &env)?)?;
    let measured = measure_run(sandbox, &running, warmup, duration).map_err(|error| running.fail(&error))?;
    let (status, stderr) = running.finish(HOLD + SLACK)?;
    if !status.success() {
        return Err(format!("gpui-video exited with {status}\n{stderr}").into());
    }
    let frame_durations = measured_frame_durations(&stderr);
    Ok(Run {
        stderr,
        frame_durations,
        ..measured
    })
}

/// Ready line, the measured phase and the summary line, then the read-back while the binary holds.
fn measure_run(sandbox: &Sandbox, running: &Running, warmup: Duration, duration: Duration) -> SandboxResult<Run> {
    let pid = running.pid();
    let ready = wait_ready(running, STARTUP)?;
    expect_line(running, "measuring", warmup + SLACK)?;
    let load_start = load_average();
    let cpus_allowed = proc_field(pid, "status", "Cpus_allowed_list:");
    let start = Instant::now();
    let before = thread_cpu(pid);
    let mut memory: BTreeMap<String, u64> = BTreeMap::new();
    for quarter in 1..=3 {
        thread::sleep((start + duration * quarter / 4).saturating_duration_since(Instant::now()));
        for (source, bytes) in gpu_memory(pid) {
            let entry = memory.entry(source).or_insert(0);
            *entry = (*entry).max(bytes);
        }
    }
    thread::sleep((start + duration * 19 / 20).saturating_duration_since(Instant::now()));
    let after = thread_cpu(pid);
    let cpu_window = start.elapsed();
    let summary_line = expect_line(running, "summary ", duration + SLACK)?;
    let summary = parse_fields(&summary_line).ok_or_else(|| format!("unreadable summary: {summary_line}"))?;
    let load_end = load_average();
    let viewport = summary
        .get("viewport")
        .and_then(|size| parse_size(size))
        .ok_or_else(|| format!("no viewport in the summary: {summary_line}"))?;
    let (window, read_back) = read_back(sandbox, &format!("grid-{pid}"), viewport)?;
    Ok(Run {
        window,
        ready,
        stderr: String::new(),
        read_back,
        summary,
        frame_durations: Vec::new(),
        cpu: cpu_split(pid, &before, &after),
        cpu_window,
        memory,
        cpus_allowed,
        load: [load_start, load_end],
    })
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn stats_line(name: &str, stats: Option<Stats>) -> String {
    stats.map_or_else(
        || format!("{name}: no samples"),
        |stats| {
            format!(
                "{name}: n={} p50={:.2} ms p95={:.2} ms max={:.2} ms mean={:.2} ms",
                stats.count,
                millis(stats.p50),
                millis(stats.p95),
                millis(stats.max),
                millis(stats.mean)
            )
        },
    )
}

fn summary_stats_line(summary: &Fields, name: &str) -> String {
    let us = |suffix: &str| {
        summary
            .number::<u64>(&format!("{name}_{suffix}_us"))
            .map_or(f64::NAN, |micros| millis(Duration::from_micros(micros)))
    };
    format!(
        "{name}: p50={:.2} ms p95={:.2} ms max={:.2} ms",
        us("p50"),
        us("p95"),
        us("max")
    )
}

/// The report of one run, as text.
fn report(label: &str, run: &Run) -> SandboxResult<String> {
    let summary = &run.summary;
    let elapsed = Duration::from_millis(summary.number("elapsed_ms")?);
    let draws: usize = summary.number("draws")?;
    let shown: usize = summary.number("shown")?;
    let received: usize = summary.number("received")?;
    let skipped: usize = summary.number("skipped")?;
    let [main, sources, other] = run.cpu;
    let total = main + sources + other;
    let mut text = String::new();
    writeln!(text, "=== {label}: {} ({})", run.ready.gpu, run.ready.compositor)?;
    writeln!(
        text,
        "cpus allowed: {}; load average at start {} / end {}",
        run.cpus_allowed, run.load[0], run.load[1]
    )?;
    writeln!(
        text,
        "measured {:.1} s; draws {draws} = {:.1}/s",
        elapsed.as_secs_f64(),
        per_second(draws, elapsed)
    )?;
    writeln!(
        text,
        "video: received {received} ({:.1}/s), shown {shown} = {:.1} fps per tile (target 30), skipped {skipped}",
        per_second(received, elapsed),
        per_second(shown, elapsed) / 9.0
    )?;
    writeln!(
        text,
        "{}",
        stats_line(
            "gpui frame duration (draw+present, CPU)",
            Stats::of(&run.frame_durations)
        )
    )?;
    writeln!(text, "{}", summary_stats_line(summary, "interval"))?;
    writeln!(text, "{}", summary_stats_line(summary, "latency"))?;
    writeln!(text, "{}", summary_stats_line(summary, "convert"))?;
    writeln!(
        text,
        "cpu over {:.1} s: total {:.0}% = main {:.0}% + video-src {:.0}% + other {:.0}% (100% = one core)",
        run.cpu_window.as_secs_f64(),
        cpu_percent(total, run.cpu_window),
        cpu_percent(main, run.cpu_window),
        cpu_percent(sources, run.cpu_window),
        cpu_percent(other, run.cpu_window)
    )?;
    let memory: Vec<String> = run
        .memory
        .iter()
        .map(|(source, bytes)| {
            format!(
                "{source}={:.1} MiB",
                f64::from(u32::try_from(bytes >> 10).unwrap_or(u32::MAX)) / 1024.0
            )
        })
        .collect();
    writeln!(text, "gpu memory (max of 3 samples): {}", memory.join(" "))?;
    writeln!(
        text,
        "viewport {}, window screenshot {}x{}; read-back after the summary, 1 s apart: {:?} then {:?}",
        summary.get("viewport").map_or("?", String::as_str),
        run.window.0,
        run.window.1,
        run.read_back[0],
        run.read_back[1]
    )?;
    Ok(text)
}

/// All 9 tiles read back in both screenshots, and each advanced between them.
fn check_read_back(read_back: &[Counters; 2]) -> Result<(), String> {
    let [first, second] = read_back;
    for (tile, (before, after)) in first.iter().zip(second).enumerate() {
        let (Some(before), Some(after)) = (before, after) else {
            return Err(format!("tile {tile} unreadable: {first:?} / {second:?}"));
        };
        if after <= before {
            return Err(format!("tile {tile} did not advance: {before} -> {after}"));
        }
    }
    Ok(())
}

#[test]
fn bench_mode_runs_and_reads_back() {
    let Some(sandbox) = sandbox_or_skip("bench_mode_runs_and_reads_back", &bench_sandbox_options()).unwrap() else {
        return;
    };
    for upload in ["image", "surface"] {
        let case = Case {
            gpu: None,
            upload,
            warmup: Duration::from_millis(3000),
            duration: Duration::from_millis(2000),
        };
        let run = run_bench(&sandbox, &case).unwrap();
        eprintln!(
            "{}",
            report(&format!("short run, {upload}, default GPU"), &run).unwrap()
        );
        assert!(!run.ready.software, "rendered in software: {:?}", run.ready);
        assert_eq!(run.summary.get("upload").map(String::as_str), Some(upload));
        check_read_back(&run.read_back).unwrap();
        assert!(run.summary.number::<usize>("shown").unwrap() > 0, "{:?}", run.summary);
        assert!(run.summary.number::<usize>("draws").unwrap() > 0, "{:?}", run.summary);
        assert!(
            !run.frame_durations.is_empty(),
            "no frame duration lines:\n{}",
            run.stderr
        );
        let errors: Vec<_> = run.stderr.lines().filter(|line| line.starts_with("[ERROR")).collect();
        assert!(errors.is_empty(), "errors logged: {errors:#?}");
    }
}

#[test]
#[ignore = "the measurement: run by hand with --release (see the module docs)"]
fn video_grid_benchmark() {
    let Some(sandbox) = sandbox_or_skip("video_grid_benchmark", &bench_sandbox_options()).unwrap() else {
        return;
    };
    let seconds: u64 = std::env::var("GPUI_BENCH_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20);
    let only = std::env::var("GPUI_BENCH_GPU").ok();
    let gpus: Vec<Gpu> = render_node_gpus()
        .into_iter()
        .filter(|gpu| only.as_ref().is_none_or(|device| *device == gpu.device))
        .collect();
    assert!(!gpus.is_empty(), "no GPU to measure (GPUI_BENCH_GPU={only:?})");
    let uploads = std::env::var("GPUI_BENCH_UPLOAD").unwrap_or_else(|_| "image,surface".to_owned());
    for gpu in &gpus {
        for upload in uploads.split(',') {
            let case = Case {
                gpu: Some(gpu),
                upload,
                warmup: Duration::from_secs(3),
                duration: Duration::from_secs(seconds),
            };
            measure_and_report(&sandbox, &case).unwrap();
        }
    }
}

/// One measured run, reported to stderr and appended to `GPUI_BENCH_OUT`; then its GPU and read-back checked.
fn measure_and_report(sandbox: &Sandbox, case: &Case<'_>) -> SandboxResult {
    let run = run_bench(sandbox, case)?;
    let gpu = case
        .gpu
        .map_or_else(|| "default".to_owned(), |gpu| format!("{} {}", gpu.vendor, gpu.device));
    let text = report(&format!("{gpu}, upload {}", case.upload), &run)?;
    eprintln!("{text}");
    if let Some(out) = std::env::var_os("GPUI_BENCH_OUT") {
        let mut file = fs::OpenOptions::new().create(true).append(true).open(out)?;
        std::io::Write::write_all(&mut file, format!("{text}\n").as_bytes())?;
    }
    if let Some(gpu) = case.gpu {
        check_selected_gpu(gpu, &run.ready, &run.stderr)?;
    }
    check_read_back(&run.read_back)?;
    Ok(())
}

#[test]
fn read_back_must_advance_on_every_tile() {
    let ok = [vec![Some(1); 9], vec![Some(30); 9]];
    check_read_back(&ok).unwrap();
    let mut stuck = ok.clone();
    stuck[1][4] = Some(1);
    assert!(check_read_back(&stuck).unwrap_err().contains("tile 4 did not advance"));
    let mut unreadable = ok;
    unreadable[0][8] = None;
    assert!(check_read_back(&unreadable).unwrap_err().contains("tile 8 unreadable"));
}

#[test]
fn frame_durations_are_cut_to_the_measured_phase() {
    let stderr = "frame duration: 9ms\n[INFO gpui] x\nbench-phase measuring\nframe duration: 1ms\n\
                  something\nframe duration: 2ms\nbench-phase done\nframe duration: 8ms\n";
    assert_eq!(
        measured_frame_durations(stderr),
        vec![Duration::from_millis(1), Duration::from_millis(2)]
    );
    assert!(measured_frame_durations("frame duration: 1ms\n").is_empty());
}

#[test]
fn cpu_is_split_into_main_sources_and_the_rest() {
    let snapshot = |entries: &[(u32, &str, u64)]| {
        entries
            .iter()
            .map(|(tid, name, ms)| (*tid, ((*name).to_owned(), Duration::from_millis(*ms))))
            .collect::<BTreeMap<_, _>>()
    };
    let before = snapshot(&[(100, "gpui-video", 10), (101, "video-src-0", 5), (102, "wgpu", 1)]);
    let after = snapshot(&[
        (100, "gpui-video", 40),
        (101, "video-src-0", 25),
        (102, "wgpu", 2),
        (103, "video-src-1", 7),
    ]);
    assert_eq!(
        cpu_split(100, &before, &after),
        [
            Duration::from_millis(30),
            Duration::from_millis(27),
            Duration::from_millis(1)
        ]
    );
}
