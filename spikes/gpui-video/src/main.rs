//! The `gpui-video` spike's binary (`window.rs` says what it does): GPUI on niri/Wayland in the Linux UI sandbox; on
//! other systems it only says so.

#[cfg(target_os = "linux")]
mod window;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    window::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("gpui-video runs only in the Linux UI sandbox");
    std::process::ExitCode::from(2)
}
