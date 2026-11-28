//! Linux compositor/input harness: an own server's administrator registers sample bots and a user uses them, through
//! the app's windows, on two API nodes with Postgres and Redis and on one node with SQLite.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_bots_acceptance_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_bots_acceptance_window: Linux compositor harness; the own server runs on Linux only");
}
