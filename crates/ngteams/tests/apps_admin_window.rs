//! Linux compositor/input harness for an own server's Manage apps, driven as an administrator
//! drives it. Cross-platform view tests run in `teams-ui`; this one needs the UI sandbox.
#[cfg(target_os = "linux")]
#[path = "support/linux/apps_admin_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("apps_admin_window: Linux compositor harness");
}
