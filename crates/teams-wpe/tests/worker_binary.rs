//! The real `ngteams-web` binary: what the app asks it at startup, that it serves only an app, and how it talks to
//! one (the worker protocol, driven by hand off the user's session).

#![cfg(target_os = "linux")]

use std::process::Command;

const WORKER: &str = env!("CARGO_BIN_EXE_ngteams-web");

#[cfg(test)]
#[path = "support/worker_protocol.rs"]
mod protocol;

#[test]
fn the_probe_prints_the_loaded_engine() {
    let output = Command::new(WORKER).arg("--probe").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let version = String::from_utf8(output.stdout).unwrap();
    let mut parts = version.trim().split('.');
    assert_eq!(parts.next(), Some("2"));
    assert!(parts.next().unwrap().parse::<u32>().unwrap() >= 54);
}

#[test]
fn the_pinned_worker_reports_the_system_engine() {
    protocol::pinned();
    let version = teams_wpe_client::worker::probe_system().unwrap();
    assert!(version.starts_with("2."), "{version}");
}

#[test]
fn started_by_hand_it_says_who_starts_it() {
    let output = Command::new(WORKER).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be started by ngteams"));
}

/// Nothing the worker starts may inherit a runtime's library path: `WebKit`'s helpers would run on it, and so would the
/// host's `bwrap` and `xdg-dbus-proxy`, which the runtime's libraries can break. Those children can't be watched here
/// without a display, so the order in `main` is what's held.
#[test]
fn the_worker_drops_the_loaders_path_before_it_serves() {
    let source = include_str!("../src/bin/ngteams-web.rs");
    let dropped = source.find(r#"remove_var("LD_LIBRARY_PATH")"#).unwrap();
    assert!(dropped < source.find("serve_if_asked").unwrap());
    assert!(dropped < source.find("--probe").unwrap());
}
