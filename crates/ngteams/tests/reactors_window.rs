//! Linux compositor/input harness: who reacted to a message, reached with the sandbox's own pointer and keyboard.
#[cfg(target_os = "linux")]
#[path = "support/linux/reactors_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("reactors_window: Linux compositor harness; the view tests cover the card elsewhere");
}
