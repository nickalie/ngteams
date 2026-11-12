//! Native WPE hosts and workers against generated pages in a disposable UI sandbox.

#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "support/engine/mod.rs"]
mod harness;

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
fn main() {
    // the real worker binary, as the app starts it
    let pinned = teams_wpe_client::worker::pin_worker_path(std::path::Path::new(env!("CARGO_BIN_EXE_ngteams-web")));
    if let Err(error) = pinned {
        eprintln!("engine test: {error}");
        std::process::exit(1);
    }
    let code = match harness::entry() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("WPE engine test: {error}");
            1
        }
    };
    if std::env::var_os(harness::CHILD).is_some() {
        teams_wpe::exit_process(code);
    }
    std::process::exit(code);
}
