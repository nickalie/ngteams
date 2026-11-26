//! QA of task 903 (round 1), Linux compositor/input harness: the app starts on GPUI 0.3.14 and its `secondary!`
//! shortcuts fire from the keyboard.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_secondary_keys_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_secondary_keys_window: Linux compositor harness");
}
