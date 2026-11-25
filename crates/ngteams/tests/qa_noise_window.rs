//! QA of task 844, Linux compositor/input harness: the noise suppression level in Settings and in a call's audio menu,
//! in the app's window over the mock.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_noise_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_noise_window: Linux compositor harness");
}
