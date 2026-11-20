//! Linux compositor/input harness: files dragged from a file manager into the app's window, as a user drops them.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_file_drop_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_file_drop_window: Linux compositor harness; Windows has scripts/test-windows-file-drop.ps1");
}
