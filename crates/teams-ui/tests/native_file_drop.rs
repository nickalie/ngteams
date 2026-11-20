//! Files dropped from File Explorer onto a chat's message box in the authorized Windows VM console: the real GPUI
//! window and its OLE drop target over made-up data. `scripts/test-windows-file-drop.ps1` drives it.
#![cfg(test)]
#![cfg_attr(windows, windows_subsystem = "windows")]
#[cfg(windows)]
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine supplies more operations than this native fixture uses"
)]
mod common;
#[cfg(windows)]
#[path = "support/native_file_drop.rs"]
mod native;
#[cfg(windows)]
fn main() -> std::process::ExitCode {
    native::main()
}
#[cfg(not(windows))]
fn main() {}
