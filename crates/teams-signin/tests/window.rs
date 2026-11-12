#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
#[path = "support/window.rs"]
mod linux;
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}
