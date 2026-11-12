//! Native Windows sign-in: a disposable `WebView2` worker, a private pipe to the same request guard as Linux,
//! and a host-gated proxy with no direct bypass. The worker never receives tokens or credential-store handles.
//! All descendants belong to a private kill-on-close job before their first instruction. Its profile directory
//! is private and removed only after the complete tree exits, including on a failed setup or page crash.
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader};
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsHandle as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use teams_auth::signin::SignInAnswer;
use teams_platform::{
    private,
    windows_job::Job,
    windows_process::{Child, Transfer},
};

mod browser;
mod policy;
mod profile;
mod refusals;
mod session;
mod wire;

#[cfg(feature = "test-support")]
pub use session::is_local_page;
pub use session::{Bound, GuardSetup, WindowConfig, Windows};

/// The engine's name, as a load failure names it.
const ENGINE: &str = "WebView2";

pub const TITLE: &str = "Sign in to Microsoft Teams";
pub const EXTENSION_FILE: &str = "ngteams-webview.exe";
pub const SIZE: (u16, u16) = (520, 720);

/// Windows selects Microsoft's installed evergreen runtime; no Linux runtime archive is loaded here.
pub struct EngineSource;

struct Pinned {
    path: PathBuf,
    _file: File,
}
static WORKER: OnceLock<Pinned> = OnceLock::new();

/// Locate and pin the native worker against replacement for this application's lifetime.
#[must_use]
pub fn extension_beside(exe: &Path) -> Option<PathBuf> {
    if let Some(worker) = WORKER.get() {
        return Some(worker.path.clone());
    }
    let path = exe.parent()?.join(EXTENSION_FILE);
    // FILE_SHARE_READ only: another process cannot replace, delete or write the worker while it is pinned.
    let file = OpenOptions::new().read(true).share_mode(1).open(&path).ok()?;
    let _ = WORKER.set(Pinned { path, _file: file });
    WORKER.get().map(|worker| worker.path.clone())
}

/// Hold the worker from app startup, so an update cannot replace it underneath this run.
///
/// # Errors
/// The installed native worker is missing or cannot be pinned.
pub fn pin_worker() -> io::Result<()> {
    extension_beside(&std::env::current_exe()?)
        .map(|_| ())
        .ok_or_else(|| io::Error::other("native sign-in worker missing"))
}

/// The installed `WebView2` runtime, or what to do without it.
fn engine_source() -> Result<EngineSource, SignInAnswer> {
    browser::runtime().map(|()| EngineSource).map_err(|_| {
        SignInAnswer::Failed("Install or repair the Microsoft Edge WebView2 Runtime, then sign in again.".into())
    })
}

struct Worker {
    job: Job,
    child: Child,
    input: std::io::PipeWriter,
    output: BufReader<std::io::PipeReader>,
    // Must outlive every browser subprocess; dropped after explicit tree cleanup.
    directory: tempfile::TempDir,
}
impl Worker {
    fn start(path: &Path, authorize: String, proxy: String, app_id: &str) -> io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix(&format!("ngteams-signin-{}-", std::process::id()))
            .tempdir()?;
        private::restrict_dir(directory.path())?;
        let (input, to_worker) = io::pipe()?;
        let (output, from_worker) = io::pipe()?;
        let null = OpenOptions::new().read(true).write(true).open("NUL")?;
        let job = Job::new()?;
        let transfer = Transfer::with_standard_streams([input.as_handle(), from_worker.as_handle(), null.as_handle()])?;
        let child = transfer.spawn_in_job(path, &["--guarded-signin".into()], &job)?;
        drop((transfer, input, from_worker));
        let mut worker = Self {
            job,
            child,
            input: to_worker,
            output: BufReader::new(output),
            directory,
        };
        wire::send(
            &mut worker.input,
            &wire::Start {
                identity: wire::identity(),
                app_id: app_id.to_owned(),
                authorize,
                proxy,
                directory: worker.directory.path().to_owned(),
            },
        )?;
        Ok(worker)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.job.terminate(0);
        let _ = self.child.wait(Duration::from_secs(5));
        // Retain the directory rather than deleting browser state while a native query is failing.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.job.active_processes().ok() != Some(0) {
            if std::time::Instant::now() >= deadline {
                let path = self.directory.path().to_owned();
                if let Ok(replacement) = tempfile::tempdir() {
                    let _ = std::mem::replace(&mut self.directory, replacement).keep();
                }
                log::warn!("sign-in cleanup retained a private profile: {}", path.display());
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Err(error) = profile::remove(self.directory.path()) {
            log::warn!(
                "sign-in cleanup retained a private profile (OS error {:?})",
                error.raw_os_error()
            );
        }
    }
}

/// The worker's entry point (`ngteams-webview --guarded-signin`).
pub fn worker() {
    session::worker(browser::run);
}
