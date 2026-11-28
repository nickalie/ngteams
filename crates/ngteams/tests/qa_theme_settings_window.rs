//! QA of task 929 in the UI sandbox: the theme picker in Settings, and the settings file across restarts.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_theme_settings_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_theme_settings_window: Linux compositor harness");
}
