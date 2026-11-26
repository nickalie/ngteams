//! QA of task 867, Linux compositor/input harness: the live captions area scrolls and resizes, in the app's call
//! window over the mock.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_captions_area_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_captions_area_window: Linux compositor harness");
}
