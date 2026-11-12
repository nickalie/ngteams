//! Shared synthetic scene on a private Linux compositor or the authorized Windows VM console.
#[cfg(any(target_os = "linux", windows))]
#[path = "support/scenes/call_large_window.rs"]
mod scene;

#[cfg(any(target_os = "linux", windows))]
fn main() -> std::process::ExitCode {
    scene::main()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn main() {
    eprintln!("call_large_window: Linux compositor harness; use the native Windows console fixtures");
}
