//! Linux compositor/input harness: translating the message being written, reached with the sandbox's own pointer and
//! keyboard.
#[cfg(target_os = "linux")]
#[path = "support/linux/translate_draft_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("translate_draft_window: Linux compositor harness; the view tests cover the box elsewhere");
}
