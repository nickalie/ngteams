//! The sign-in window's WPE `WebKit` engine: the only program that links it, so the app starts on a system without one
//! and picks the engine this runs on (`teams_wpe_client::EngineSource`).

#[cfg(target_os = "linux")]
fn main() {
    // the loader has read it: neither WebKit's helpers nor the host's `bwrap` and `xdg-dbus-proxy` may inherit it
    // SAFETY: first thing in `main`, while the process has one thread.
    unsafe { std::env::remove_var("LD_LIBRARY_PATH") };
    if std::env::args_os().nth(1).is_some_and(|arg| arg == "--probe") {
        let (major, minor, micro) = teams_wpe::worker::engine_version();
        println!("{major}.{minor}.{micro}");
        return;
    }
    // SAFETY: as above, nothing has started a thread.
    unsafe { teams_wpe::worker::serve_if_asked(|| {}) };
    eprintln!("ngteams-web: this worker must be started by ngteams");
    std::process::exit(2);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ngteams-web is the Linux sign-in worker");
    std::process::exit(2);
}
