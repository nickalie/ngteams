//! Native macOS sign-in: a disposable `ngteams-webview` worker showing Microsoft's page in a `WKWebView`, on the same
//! private standard-stream protocol and the same request guard as Windows, and behind the same host-gated proxy with
//! no direct way out.
//!
//! - The worker's website data store is non-persistent (nothing of the page outlives it) and routes every connection
//!   through the host gate (`proxyConfigurations`, macOS 14 or later): a host the account's sign-in doesn't admit is
//!   refused at the tunnel, whatever asked for it.
//! - Every navigation, main frame or not, redirects included, is asked of this process with its method before it
//!   starts ([`wire::Event::Navigation`], then [`wire::Event::Request`]); the redirect to the app's redirect URI is
//!   caught there and never loaded. Every `fetch` and `XMLHttpRequest` the page makes is asked the same way first
//!   (`worker.rs`'s document-start script, in every frame); `WKWebView` offers no hook for its other subresource loads,
//!   which are reads and reach only the gate's hosts.
//! - New windows, downloads, ambient HTTP authentication and client certificates are refused; TLS is the system's,
//!   with no click-through.
//!
//! The worker never receives tokens or credential-store handles: the code in the caught redirect is redeemed here.
use crate::guard::{Decision, Guard, GuardEvent};
use gpui::App;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, OnceLock, mpsc};
use teams_auth::redirect::RedirectUri;
use teams_auth::signin::{SignInAnswer, SignInRequest};

#[path = "../native/wire.rs"]
mod wire;
mod worker;

const PAGE: u64 = 1;

pub const TITLE: &str = "Sign in to Microsoft Teams";
pub const EXTENSION_FILE: &str = "ngteams-webview";
pub const SIZE: (u16, u16) = (520, 720);
pub type GuardSetup = Box<dyn FnOnce(Guard) -> Guard + Send>;

/// macOS signs in with the system's `WebKit`; there is no engine to choose or download.
pub struct EngineSource;

#[derive(Clone, Debug)]
pub enum Bound {
    Hosts(Arc<crate::hosts::SignInHosts>),
}

pub struct WindowConfig {
    pub request: SignInRequest,
    pub bound: Bound,
    /// The native worker beside the app; the common config keeps the Linux field's name.
    pub extension: PathBuf,
    pub engine: EngineSource,
    pub setup: GuardSetup,
}

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

/// Sign-in is a native worker window; it never blocks GPUI's foreground.
#[derive(Clone)]
pub struct Windows {
    app_id: String,
}

impl Windows {
    #[must_use]
    pub fn serve(app_id: &str, _cx: &mut App) -> Self {
        Self::new(app_id)
    }

    #[must_use]
    pub fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
        }
    }

    /// # Errors
    /// Never: `WebKit` is part of macOS.
    pub fn engine_source(&self, _data_dir: &Path) -> Result<EngineSource, SignInAnswer> {
        Ok(EngineSource)
    }

    #[must_use]
    pub fn ask(&self, config: WindowConfig) -> SignInAnswer {
        ask(config, &self.app_id).unwrap_or_else(|error| {
            log::warn!("the sign-in worker failed: {error}");
            SignInAnswer::Failed("The sign-in window could not finish. Close it and try again.".into())
        })
    }
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

fn ask(config: WindowConfig, app_id: &str) -> io::Result<SignInAnswer> {
    let Bound::Hosts(hosts) = config.bound;
    let redirect =
        RedirectUri::parse(&config.request.redirect_uri).map_err(|_| io::Error::other("invalid redirect"))?;
    if !hosts.admits_url(&config.request.authorize) {
        return Err(io::Error::other("authorize host refused"));
    }
    let (events, hearing) = mpsc::channel();
    let guard = Arc::new((config.setup)(Guard::new(Arc::clone(&hosts), redirect.clone(), events)));
    guard.arm(PAGE);
    let proxy = crate::egress::HostGate::start(Arc::clone(&guard), Arc::clone(&hosts))?.uri();
    let mut worker = Worker::start(&config.extension, config.request.authorize, proxy, app_id)?;
    let mut refused = None;
    loop {
        let event: wire::Event = wire::read(&mut worker.output)?;
        let allow = match event {
            wire::Event::Request { method, url } => guard.decide(PAGE, &method, &url) == Decision::Allow,
            wire::Event::Navigation(url) => {
                if redirect.matches(&url) {
                    guard.caught_navigation(&url);
                    wire::send(&mut worker.input, &false)?;
                    return Ok(SignInAnswer::Redirect(teams_auth::secrets::Secret::new(url)));
                }
                let allowed = hosts.admits_url(&url);
                if !allowed {
                    guard.refuse("GET", &url);
                    refused = hosts.idp_candidate(&url).or(refused);
                }
                allowed
            }
            wire::Event::NetworkFailure(code) => {
                log::warn!("sign-in network load failed: {code}");
                continue;
            }
            wire::Event::LoadFailed(code) => {
                return Ok(SignInAnswer::Failed(format!(
                    "The sign-in page could not load (WebKit error {code}). Check your connection and try again."
                )));
            }
            wire::Event::Closed => return Ok(refused.map_or(SignInAnswer::Closed, SignInAnswer::Refused)),
            wire::Event::Failed(reason) => return Ok(SignInAnswer::Failed(reason)),
        };
        wire::send(&mut worker.input, &allow)?;
        while let Ok(event) = hearing.try_recv() {
            if let GuardEvent::Redirect(url) = event {
                return Ok(SignInAnswer::Redirect(url));
            }
        }
    }
}

/// Whether an offline test URL uses HTTP(S) on a numerical loopback address.
#[cfg(feature = "test-support")]
#[must_use]
pub fn is_local_page(text: &str) -> bool {
    url::Url::parse(text).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && match url.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            }
    })
}

/// The worker's entry point (`ngteams-webview --guarded-signin`): stdout carries the protocol only.
pub fn worker() {
    let result = (|| {
        if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--guarded-signin")) {
            return Err("the sign-in worker must be started by ngteams");
        }
        let start: wire::Start =
            wire::read(&mut io::stdin().lock()).map_err(|_| "the sign-in worker's private pipe failed")?;
        if start.identity != wire::identity() {
            return Err("the sign-in worker and app versions differ; restart ngteams after updating");
        }
        worker::run(&start)
    })();
    if let Err(reason) = result {
        let _ = wire::send(&mut io::stdout().lock(), &wire::Event::Failed(reason.into()));
    }
}
