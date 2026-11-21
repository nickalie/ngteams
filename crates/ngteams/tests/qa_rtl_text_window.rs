//! QA of task 699 (round 1), Linux compositor/input harness: Arabic, Hebrew and mixed right-to-left and left-to-right
//! text in messages, the compose box, a copy, the chat list and search, as a user sees them in the app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_rtl_text_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_rtl_text_window: Linux compositor harness");
}
