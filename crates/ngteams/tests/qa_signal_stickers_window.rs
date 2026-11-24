//! Linux compositor/input harness: Signal sticker packs imported from a link in the picker, as a user sees them in the
//! app's window.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_signal_stickers_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "qa_signal_stickers_window: Linux compositor harness; the teams-ui view tests cover the import elsewhere"
    );
}
