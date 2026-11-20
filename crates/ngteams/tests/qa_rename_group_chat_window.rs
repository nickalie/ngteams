//! QA of task 661 in the UI sandbox: a user names, renames and unnames a group chat from its header.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_rename_group_chat_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_rename_group_chat_window: Linux compositor harness");
}
