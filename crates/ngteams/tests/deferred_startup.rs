//! The app's call devices start without holding up startup. On a fresh, empty `GStreamer` registry (a first run, when
//! Windows' backend scans every plugin) `CallDevices::open` returns at once and discovery ends on its own thread.
//! On Linux the child runs with a cleared environment, so sandbox reach refuses and discovery ends unavailable; in
//! the win11 console it reaches the native backend. `NGTEAMS_DEFERRED_STARTUP_REQUIRE_READY=1` requires that.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::process::Command;
use std::time::{Duration, Instant};

use ngteams::calling::CallDevices;
use teams_media::devices::Reach;
use teams_media::devices::catalog::DeviceStatus;

const CHILD_ENV: &str = "NGTEAMS_DEFERRED_STARTUP_CHILD";
const REQUIRE_READY_ENV: &str = "NGTEAMS_DEFERRED_STARTUP_REQUIRE_READY";
/// Opening may take no longer than this: it only starts the discovery thread.
const OPEN_BOUND: Duration = Duration::from_secs(1);
/// The longest a first plugin scan was measured to take, with room.
const DISCOVERY_BOUND: Duration = Duration::from_secs(400);

#[test]
fn call_devices_open_at_once_on_an_empty_gstreamer_registry() {
    let directory = tempfile::tempdir().unwrap();
    let registry = directory.path().join("fresh-gst-registry.bin");
    std::fs::write(&registry, []).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "discovery_child", "--nocapture", "--test-threads=1"]);
    if cfg!(target_os = "linux") {
        // never the desktop's PipeWire: without the sandbox's variables, sandbox reach refuses
        child.env_clear().env("HOME", directory.path());
        for key in ["PATH", "LANG", "LD_LIBRARY_PATH", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
    }
    // GST_REGISTRY_1_0 wins over GST_REGISTRY, and the Windows console runner sets it to its run's shared registry
    child
        .env(CHILD_ENV, "1")
        .env("GST_REGISTRY", &registry)
        .env("GST_REGISTRY_1_0", &registry);
    if let Some(value) = std::env::var_os(REQUIRE_READY_ENV) {
        child.env(REQUIRE_READY_ENV, value);
    }
    let output = child.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("deferred-startup "), "the child ran: {stdout}");
}

#[test]
fn discovery_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let started = Instant::now();
    let devices = CallDevices::open(Reach::Sandbox);
    let opened = started.elapsed();
    let (_, calls) = devices.for_account();
    let directory = calls.devices.unwrap();
    let at_open = directory.status();
    while directory.status() == DeviceStatus::Preparing && started.elapsed() < DISCOVERY_BOUND {
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = directory.status();
    println!(
        "deferred-startup open={} ms status-at-open={at_open:?} discovery-ended={} ms status={status:?} devices={}",
        opened.as_millis(),
        started.elapsed().as_millis(),
        directory.devices().all().len()
    );
    assert!(opened < OPEN_BOUND, "opening the call devices took {opened:?}");
    assert_ne!(status, DeviceStatus::Preparing, "discovery never ended");
    if std::env::var_os(REQUIRE_READY_ENV).is_some() {
        assert_eq!(status, DeviceStatus::Ready, "discovery failed in the console");
    }
}
