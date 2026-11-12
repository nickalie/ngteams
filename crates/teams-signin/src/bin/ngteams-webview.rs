//! The Windows and macOS sign-in worker. Its only protocol is the private standard streams supplied by the parent.
#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(any(windows, target_os = "macos"))]
fn main() {
    teams_signin::window::worker();
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("ngteams-webview is the Windows and macOS sign-in worker");
    std::process::exit(2);
}
