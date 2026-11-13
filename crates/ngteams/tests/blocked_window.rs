//! Linux compositor/input harness: messages a tenant's policy blocked, as a user sees them in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/blocked_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("blocked_window: Linux compositor harness; the view tests cover the notice elsewhere");
}
