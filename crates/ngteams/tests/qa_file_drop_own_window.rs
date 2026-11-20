//! Linux compositor/input harness: files dropped from a file manager into the app's window, sent to an own server's
//! chat, and read by the other member.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_file_drop_own_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_file_drop_own_window: Linux compositor harness; Windows has scripts/test-windows-file-drop.ps1");
}
