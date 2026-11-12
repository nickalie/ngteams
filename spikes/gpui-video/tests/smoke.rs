//! M0 Task 15 smoke test: the `gpui-video` window opens on niri, renders text and an image, and closes, within
//! deadlines and without an error in GPUI's log, on every GPU of the machine.
//!
//! Nothing reaches the desktop the tests run in. Each sandbox ([`gpui_video::sandbox`]) starts a private headless
//! weston (GL renderer, `--fake-seat`: GPUI needs a `wl_seat`) and a niri nested in it, each with its own
//! `XDG_RUNTIME_DIR` in a temp dir, a private `dbus-daemon` with fake notification and tray servers, and a no-op URI
//! opener first on the client's `PATH`. Every child's environment is built from scratch: no `WAYLAND_DISPLAY`,
//! `DISPLAY`, `NIRI_SOCKET` or D-Bus address of the session leaks in, the smoke binary gets the nested niri's socket
//! as an absolute `WAYLAND_DISPLAY` and the private bus's address, and `niri msg` always gets the nested niri's
//! `NIRI_SOCKET`.
//!
//! Without a weston that takes `--fake-seat` (15.0 or later), a niri or a `dbus-daemon` on `PATH`, the compositor
//! tests say so on stderr and pass, unless `GPUI_SMOKE_REQUIRE=1`, which makes them fail instead.
#![cfg(target_os = "linux")]

use std::{
    ffi::{OsStr, OsString},
    os::unix::net::UnixListener,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

use gpui_video::sandbox::{
    Gpu, Ready, Running, SANDBOX_ENV, Sandbox, SandboxOptions, SandboxResult, check_selected_gpu, clean_command,
    parse_ready, private_dir, render_node_gpus, sandbox_or_skip, wait_ready, wait_until,
};
use tempfile::TempDir;

const SMOKE: &str = env!("CARGO_BIN_EXE_gpui-video");
/// The smoke window's Wayland app id.
const APP_ID: &str = "ngteams-gpui-smoke";
/// From launch to the ready line (a cold dGPU wakes up within it).
const STARTUP: Duration = Duration::from_secs(30);
/// From the compositor's close request to the process exit.
const CLOSE: Duration = Duration::from_secs(5);

/// Counts of the smoke window's colours in a screenshot: magenta and cyan tiles, and white text.
fn colour_counts(png: &Path) -> SandboxResult<[usize; 3]> {
    let shot = image::open(png)?.to_rgb8();
    let near = |pixel: &image::Rgb<u8>, target: [u8; 3]| {
        pixel
            .0
            .iter()
            .zip(target)
            .all(|(&channel, wanted)| channel.abs_diff(wanted) <= 8)
    };
    let mut counts = [0; 3];
    for pixel in shot.pixels() {
        for (count, target) in counts.iter_mut().zip([[255, 0, 255], [0, 255, 255], [255, 255, 255]]) {
            *count += usize::from(near(pixel, target));
        }
    }
    Ok(counts)
}

/// Waits for the ready line, screenshots the screen, lists the windows and asks niri to close the focused one.
fn show_and_close(sandbox: &Sandbox, running: &Running, shot: &str) -> SandboxResult<(Ready, [usize; 3], String)> {
    let ready = wait_ready(running, STARTUP)?;
    let colours = colour_counts(&sandbox.screenshot(shot, false)?)?;
    let windows = sandbox.niri_msg(&["windows"])?;
    sandbox.niri_msg(&["action", "close-window"])?;
    Ok((ready, colours, windows))
}

/// Runs the smoke binary on the sandbox's niri: ready, screenshot with text and image, listed by niri, closed by
/// niri, clean exit without an error logged. Returns what the ready line said and the binary's stderr. A step that
/// fails kills the binary and reports its stderr.
fn smoke(sandbox: &Sandbox, extra_env: &[(&str, &str)], shot: &str) -> SandboxResult<(Ready, String)> {
    let mut running = Running::new(sandbox.spawn(Path::new(SMOKE), &[], extra_env)?)?;
    let (ready, [magenta, cyan, white], windows) =
        show_and_close(sandbox, &running, shot).map_err(|error| running.fail(&error))?;
    let (status, stderr) = running.finish(CLOSE)?;
    eprintln!("{ready:?}\n{stderr}");
    assert!(windows.contains(APP_ID), "niri does not list the window: {windows}");
    assert!(status.success(), "exit {status}: {stderr}");
    let errors: Vec<_> = stderr.lines().filter(|line| line.starts_with("[ERROR")).collect();
    assert!(errors.is_empty(), "errors logged: {errors:#?}");
    assert!(
        magenta > 2000 && cyan > 2000,
        "image not on screen: magenta {magenta}, cyan {cyan}"
    );
    assert!(white > 50, "text not on screen: {white} white pixels");
    Ok((ready, stderr))
}

#[test]
fn window_opens_renders_and_closes_on_niri() {
    let options = SandboxOptions::default();
    let Some(sandbox) = sandbox_or_skip("window_opens_renders_and_closes_on_niri", &options).unwrap() else {
        return;
    };
    let (ready, _) = smoke(&sandbox, &[], "default.png").unwrap();
    assert_eq!(ready.compositor, "Wayland");
    assert!(!ready.software, "rendered in software: {ready:?}");
}

/// Every GPU in turn (the iGPU and the dGPU on a hybrid laptop), selected with GPUI's `ZED_DEVICE_ID`, which puts
/// that adapter first; GPUI's log must show it rendered on that PCI device, so a silent fallback to another GPU, of
/// the same vendor or not, fails.
#[test]
fn window_renders_on_every_gpu() {
    let options = SandboxOptions::default();
    let Some(sandbox) = sandbox_or_skip("window_renders_on_every_gpu", &options).unwrap() else {
        return;
    };
    let gpus = render_node_gpus();
    assert!(!gpus.is_empty(), "no render nodes in /sys/class/drm");
    for gpu in gpus {
        let shot = format!("gpu-{}.png", gpu.device);
        let (ready, stderr) = smoke(&sandbox, &[("ZED_DEVICE_ID", gpu.device.as_str())], &shot).unwrap();
        assert!(!ready.software, "{}: rendered in software: {ready:?}", gpu.device);
        if let Err(error) = check_selected_gpu(&gpu, &ready, &stderr) {
            panic!("{}: {error}\n--- gpui-video stderr\n{stderr}", gpu.device);
        }
    }
}

/// A GPUI adapter log for `check_selected_gpu`: an Intel iGPU and an Intel dGPU, `selected` chosen.
fn adapter_log(selected: &str) -> String {
    format!(
        "[INFO gpui_wgpu::wgpu_context] ZED_DEVICE_ID filter: 0xa7a0\n\
         [INFO gpui_wgpu::wgpu_context] Found 2 GPU adapter(s):\n\
         [INFO gpui_wgpu::wgpu_context]   - Intel(R) Iris(R) Xe Graphics (RPL-P) (vendor=0x8086, device=0xa7a0, \
         backend=Vulkan, type=IntegratedGpu)\n\
         [INFO gpui_wgpu::wgpu_context]   - Intel(R) Arc(tm) A770 Graphics (DG2) (vendor=0x8086, device=0x56a0, \
         backend=Vulkan, type=DiscreteGpu)\n\
         [INFO gpui_wgpu::wgpu_context] Selected GPU (passed configuration test): {selected} (Vulkan)\n"
    )
}

#[test]
fn selected_gpu_is_checked_by_pci_id() {
    let igpu = Gpu {
        vendor: "0x8086".to_owned(),
        device: "0xa7a0".to_owned(),
    };
    let ready = |gpu: &str| Ready {
        compositor: "Wayland".to_owned(),
        gpu: gpu.to_owned(),
        software: false,
    };
    let iris = "Intel(R) Iris(R) Xe Graphics (RPL-P)";
    let arc = "Intel(R) Arc(tm) A770 Graphics (DG2)";
    check_selected_gpu(&igpu, &ready(iris), &adapter_log(iris)).unwrap();
    // The other GPU of the same vendor rendered.
    let error = check_selected_gpu(&igpu, &ready(arc), &adapter_log(arc)).unwrap_err();
    assert!(error.contains("not one of"), "{error}");
    let fallback = adapter_log(arc).replace(
        "[INFO gpui_wgpu::wgpu_context] Selected",
        "[INFO gpui_wgpu::wgpu_context]   Adapter x (Vulkan) failed: boom, trying next...\n\
         [INFO gpui_wgpu::wgpu_context] Selected",
    );
    let error = check_selected_gpu(&igpu, &ready(arc), &fallback).unwrap_err();
    assert!(error.contains("fell back"), "{error}");
    // The requested id matches no adapter (its driver is missing, or a vendor nobody mapped).
    let virtio = Gpu {
        vendor: "0x1af4".to_owned(),
        device: "0x1050".to_owned(),
    };
    let error = check_selected_gpu(&virtio, &ready(iris), &adapter_log(iris)).unwrap_err();
    assert!(error.contains("no adapter 0x1af4:0x1050"), "{error}");
    // The ready line names another adapter than the selected one.
    let error = check_selected_gpu(&igpu, &ready(arc), &adapter_log(iris)).unwrap_err();
    assert!(error.contains("ready line"), "{error}");
    let error = check_selected_gpu(&igpu, &ready(iris), "").unwrap_err();
    assert!(error.contains("no adapter"), "{error}");
}

/// With no Wayland compositor named (and an X11 display that isn't there), the binary refuses to start instead of
/// falling back to X11 or to running headless.
#[test]
fn refuses_to_run_without_a_wayland_display() {
    let dir = TempDir::new().unwrap();
    let run = private_dir(dir.path().join("run")).unwrap();
    let output = clean_command(SMOKE)
        .env("XDG_RUNTIME_DIR", &run)
        .env("DISPLAY", ":4242")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "started without a compositor: {stderr}");
    assert!(stderr.contains("no Wayland compositor"), "{stderr}");
    assert!(output.stdout.is_empty(), "{}", String::from_utf8_lossy(&output.stdout));
}

/// A compositor outside an opted-in sandbox (the desktop session's) is refused before the binary connects to it:
/// without `UI_SANDBOX_DIR`, with it naming another dir, and with a bare socket name in `XDG_RUNTIME_DIR`.
#[test]
fn refuses_a_compositor_outside_the_sandbox() {
    let dir = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let run = private_dir(dir.path().join("run")).unwrap();
    let socket = run.join("wayland-1");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let cases: [(&OsStr, Option<&Path>); 3] = [
        (socket.as_os_str(), None),
        (socket.as_os_str(), Some(other.path())),
        (OsStr::new("wayland-1"), Some(dir.path())),
    ];
    for (wayland, sandbox) in cases {
        let mut command = clean_command(SMOKE);
        command
            .env("XDG_RUNTIME_DIR", &run)
            .env("WAYLAND_DISPLAY", wayland)
            .env("DISPLAY", ":4242");
        if let Some(sandbox) = sandbox {
            command.env(SANDBOX_ENV, sandbox);
        }
        let output = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "{wayland:?} in {sandbox:?} accepted: {stderr}"
        );
        assert!(stderr.contains("not a private compositor"), "{stderr}");
        assert!(output.stdout.is_empty(), "{}", String::from_utf8_lossy(&output.stdout));
        let accepted = listener.accept();
        assert!(
            accepted
                .as_ref()
                .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock),
            "{wayland:?} in {sandbox:?} connected to the compositor: {accepted:?}"
        );
    }
}

/// A `WAYLAND_DISPLAY` naming no socket is fatal: no fallback to X11 or headless, and nothing on stdout.
#[test]
fn fails_on_a_wayland_display_with_no_compositor() {
    let dir = TempDir::new().unwrap();
    let run = private_dir(dir.path().join("run")).unwrap();
    let missing: OsString = run.join("wayland-missing").into();
    let mut child = clean_command(SMOKE)
        .env("XDG_RUNTIME_DIR", &run)
        .env("WAYLAND_DISPLAY", missing)
        .env(SANDBOX_ENV, dir.path())
        .env("DISPLAY", ":4242")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = wait_until(STARTUP, || child.try_wait().ok().flatten());
    let output = if status.is_some() {
        child.wait_with_output().unwrap()
    } else {
        child.kill().unwrap();
        panic!("still running {STARTUP:?} after a start without a compositor");
    };
    assert!(!output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stdout.is_empty(), "{}", String::from_utf8_lossy(&output.stdout));
}

/// A binary that dies before its ready line is reported at once as an early exit, with its stderr and status.
#[test]
fn early_exit_is_reported_with_stderr() {
    let child = clean_command("sh")
        .args(["-c", "echo 'adapter exploded' >&2; exit 3"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut running = Running::new(child).unwrap();
    let start = Instant::now();
    let error = wait_ready(&running, STARTUP).unwrap_err();
    assert!(
        start.elapsed() < STARTUP / 2,
        "waited {:?} for a dead process",
        start.elapsed()
    );
    let error = running.fail(&error).to_string();
    assert!(error.contains("stdout closed before the ready line"), "{error}");
    assert!(error.contains("adapter exploded"), "{error}");
    assert!(error.contains("exit status: 3"), "{error}");
}

#[test]
fn ready_line_parses() {
    let line = "ready compositor=Wayland gpu=\"Intel(R) Graphics (RPL-P)\" driver=\"Intel open-source Mesa driver\" \
                software=false";
    let ready = parse_ready(line).unwrap();
    assert_eq!(ready.compositor, "Wayland");
    assert_eq!(ready.gpu, "Intel(R) Graphics (RPL-P)");
    assert!(!ready.software);
    assert!(
        parse_ready("ready compositor=Wayland gpu=\"x\" driver=\"y\" software=true")
            .unwrap()
            .software
    );
    assert!(parse_ready("hello").is_none());
}
