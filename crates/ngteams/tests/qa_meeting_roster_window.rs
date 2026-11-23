//! QA of task 798 (round 1): who a meeting shows as in it, and who as only invited, in the app's call window over the
//! mock. Linux compositor harness (`harness = false`: GPUI runs on the process's main thread).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_meeting_roster_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_meeting_roster_window: Linux compositor harness");
}
