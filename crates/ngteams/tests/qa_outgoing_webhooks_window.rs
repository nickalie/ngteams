//! Linux compositor/input harness: a team owner on an own server makes an outgoing webhook in the app's window and a
//! post that mentions it reaches its service, through the real `ngteams-server` binary.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_outgoing_webhooks_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_outgoing_webhooks_window: Linux compositor harness; the own server runs on Linux only");
}
