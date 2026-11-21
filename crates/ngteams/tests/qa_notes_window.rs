//! QA of task 698 (round 1), Linux compositor/input harness: the chat with yourself ("<name> (You)", `48:notes`) takes
//! notes, as a user writes them in the app's window over the mock.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_notes_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_notes_window: Linux compositor harness");
}
