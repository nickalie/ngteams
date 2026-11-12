//! Embed the native Windows executable's icon; no resource tool runs for other targets.
#[path = "../../build/windows-icon.rs"]
mod windows_icon;

/// The Windows main (UI) thread's stack reserve, in bytes: Linux's 8 MiB. `scripts/windows-release.py` checks it.
const MAIN_THREAD_STACK: u64 = 8 * 1024 * 1024;

fn main() -> std::process::ExitCode {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // MSVC hits LNK1140's debug-record limit when the full GUI/media/AI image is linked, even with line tables
        // only. Keep the normal feature set and all runtime diagnostics; omit this image's PDB at the final link.
        // Dependency/test debug information and the Linux executable retain their normal settings.
        println!("cargo::rustc-link-arg-bin=ngteams=/DEBUG:NONE");
        // MSVC's default is 1 MiB; overflowing GPUI's main thread aborts the app with no panic or log line.
        println!("cargo::rustc-link-arg-bin=ngteams=/STACK:{MAIN_THREAD_STACK}");
    }
    windows_icon::build("ngteams")
}
