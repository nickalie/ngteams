//! QA of task 710 (round 1): a meeting hosted in another region, joined on that region's flight proxy, for a Microsoft
//! account on the mock. Linux compositor harness (`harness = false`: GPUI runs on the process's main thread).
#[cfg(target_os = "linux")]
#[path = "support/linux/qa_regional_proxy_window.rs"]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("qa_regional_proxy_window: Linux compositor harness");
}
