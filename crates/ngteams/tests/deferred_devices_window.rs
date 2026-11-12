//! Linux compositor/input harness for calls while call-device discovery is still running or has failed.
#[cfg(target_os = "linux")]
#[path = "support/linux/deferred_devices_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("deferred_devices_window: Linux compositor harness; use the native Windows console fixtures");
}
