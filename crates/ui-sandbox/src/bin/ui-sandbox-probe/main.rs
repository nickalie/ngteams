//! `ui-sandbox-probe` belongs to the Linux UI sandbox (`linux.rs` says what it does); on other systems it only says so.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("ui-sandbox-probe runs only in the Linux UI sandbox");
    std::process::ExitCode::from(2)
}
