//! Native macOS sign-in: a disposable `ngteams-webview` worker showing Microsoft's page in a `WKWebView`, on the same
//! private standard-stream protocol and the same request guard as Windows, and behind the same host-gated proxy with
//! no direct way out.
//!
//! - The worker's website data store is non-persistent (nothing of the page outlives it) and routes every connection
//!   through the host gate (`proxyConfigurations`, macOS 14 or later): a host the account's sign-in doesn't admit is
//!   refused at the tunnel, whatever asked for it.
//! - Every navigation, main frame or not, redirects included, is asked of this process with its method before it
//!   starts ([`wire::Event::Navigation`], then [`wire::Event::Request`]); the redirect to the app's redirect URI is
//!   caught there and never loaded (the guard's loop is Windows', `native/session.rs`). Every `fetch` and `XMLHttpRequest` the page makes is asked the same way first
//!   (`worker.rs`'s document-start script, in every frame); `WKWebView` offers no hook for its other subresource loads,
//!   which are reads and reach only the gate's hosts.
//! - New windows, downloads, ambient HTTP authentication and client certificates are refused; TLS is the system's,
//!   with no click-through.
//!
//! The worker never receives tokens or credential-store handles: the code in the caught redirect is redeemed here.
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;
use teams_auth::signin::SignInAnswer;

#[path = "../native/session.rs"]
mod session;
#[path = "../native/wire.rs"]
mod wire;
mod worker;

#[cfg(feature = "test-support")]
pub use session::is_local_page;
pub use session::{Bound, GuardSetup, WindowConfig, Windows};

/// The engine's name, as a load failure names it.
const ENGINE: &str = "WebKit";

pub const TITLE: &str = "Sign in to Microsoft Teams";
pub const EXTENSION_FILE: &str = "ngteams-webview";
pub const SIZE: (u16, u16) = (520, 720);

/// macOS signs in with the system's `WebKit`; there is no engine to choose or download.
pub struct EngineSource;

struct Pinned {
    path: PathBuf,
    _file: File,
}
static WORKER: OnceLock<Pinned> = OnceLock::new();

/// The native worker beside `exe`, found once and held open for the app's lifetime.
#[must_use]
pub fn extension_beside(exe: &Path) -> Option<PathBuf> {
    if let Some(worker) = WORKER.get() {
        return Some(worker.path.clone());
    }
    let path = exe.parent()?.join(EXTENSION_FILE);
    let file = File::open(&path).ok()?;
    let _ = WORKER.set(Pinned { path, _file: file });
    WORKER.get().map(|worker| worker.path.clone())
}

/// Find the worker at app startup, so a sign-in later in the run uses the one that came with this app.
///
/// # Errors
/// The worker is missing beside the app.
pub fn pin_worker() -> io::Result<()> {
    extension_beside(&std::env::current_exe()?)
        .map(|_| ())
        .ok_or_else(|| io::Error::other("native sign-in worker missing"))
}

/// `WebKit` is part of macOS: always there.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the sign-in service's engine check, which fails on Windows"
)]
fn engine_source() -> Result<EngineSource, SignInAnswer> {
    Ok(EngineSource)
}

/// The running worker: killed and reaped when dropped, so no sign-in window outlives its ask.
struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn start(path: &Path, authorize: String, proxy: String, app_id: &str) -> io::Result<Self> {
        let mut command = Command::new(path);
        command
            .arg("--guarded-signin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // the loader's overrides would put other code into the page's process
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("DYLD_") {
                command.env_remove(key);
            }
        }
        let mut child = command.spawn()?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("sign-in worker pipes missing"));
        };
        let mut worker = Self {
            child,
            input,
            output: BufReader::new(output),
        };
        wire::send(
            &mut worker.input,
            &wire::Start {
                identity: wire::identity(),
                app_id: app_id.to_owned(),
                authorize,
                proxy,
                directory: PathBuf::new(),
            },
        )?;
        Ok(worker)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The worker's entry point (`ngteams-webview --guarded-signin`).
pub fn worker() {
    session::worker(worker::run);
}
