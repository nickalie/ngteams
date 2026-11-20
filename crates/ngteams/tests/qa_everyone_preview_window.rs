//! QA of task 715 (round 1), Linux compositor/input harness: a mention of everyone that the sender's client labelled
//! in its own language reads "Everyone" in the chat list, notifications, the reply box and search.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_everyone_preview_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_everyone_preview_window: Linux compositor harness");
}
