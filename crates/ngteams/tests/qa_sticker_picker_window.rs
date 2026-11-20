//! QA of task 653 in the UI sandbox: the compose box's picker with its Stickers tab, the sticker packs the user
//! imports from a folder or a `.zip` file, and the stickers a sends and b sees.
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_sticker_picker_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_sticker_picker_window: Linux compositor harness; the teams-ui view tests cover the picker elsewhere");
}
