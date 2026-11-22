//! QA of task 712 (round 1), Linux compositor/input harness: the user chooses the buttons of the message hover bar,
//! in the app's window over the mock.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_bar_customize_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_bar_customize_window: Linux compositor harness");
}
