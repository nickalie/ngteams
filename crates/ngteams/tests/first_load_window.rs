//! A large account's first sign-in in the UI sandbox: the chat list and chats while the catch-up still runs. Linux
//! only, like the other compositor harnesses.
#[cfg(target_os = "linux")]
#[path = "support/linux/first_load_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("first_load_window: Linux compositor harness");
}
