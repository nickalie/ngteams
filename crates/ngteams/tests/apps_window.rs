//! Linux compositor/input harness for the Store, driven as a user drives it. Cross-platform view
//! tests run in `teams-ui`; this one needs the UI sandbox.
#[cfg(target_os = "linux")]
#[path = "support/linux/apps_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("apps_window: Linux compositor harness");
}
