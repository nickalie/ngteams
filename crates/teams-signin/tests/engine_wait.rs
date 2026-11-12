#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
#[path = "support/engine_wait.rs"]
mod linux;
#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}
