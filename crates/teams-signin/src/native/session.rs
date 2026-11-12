//! The app's side of a native sign-in worker, the same on Windows (`WebView2`) and macOS (`WKWebView`): the sign-in
//! service, the window's config, the loop that has the guard decide each request the worker asks about, catches the
//! redirect and records refusals, and the worker's entry point. Each platform's module includes this file (as
//! `wire.rs` is) and supplies its engine ([`super::EngineSource`], [`super::engine_source`], [`super::ENGINE`]), its
//! worker process ([`super::Worker`]: `start`, `input`, `output`) and the page loop the worker runs.
use super::{ENGINE, EngineSource, Worker, wire};
use crate::guard::{Decision, Guard, GuardEvent};
use gpui::App;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use teams_auth::redirect::RedirectUri;
use teams_auth::signin::{SignInAnswer, SignInRequest};

/// The one page a sign-in window holds, as the guard knows it.
const PAGE: u64 = 1;

pub type GuardSetup = Box<dyn FnOnce(Guard) -> Guard + Send>;

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
    /// The platform's web engine is missing (`WebView2`'s runtime; never on macOS, whose `WebKit` is the system's).
    pub fn engine_source(&self, _data_dir: &Path) -> Result<EngineSource, SignInAnswer> {
        super::engine_source()
    }

    #[must_use]
    pub fn ask(&self, config: WindowConfig) -> SignInAnswer {
        ask(config, &self.app_id).unwrap_or_else(|error| {
            log::warn!("the sign-in worker failed: {error}");
            SignInAnswer::Failed("The sign-in window could not finish. Close it and try again.".into())
        })
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
                    "The sign-in page could not load ({ENGINE} error {code}). Check your connection and try again."
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

/// The worker's entry point (`ngteams-webview --guarded-signin`): checks it was started by this app's own version,
/// then runs `page` (the platform's page loop). Stdout carries the protocol only; stderr is discarded by the parent.
pub(super) fn worker(page: fn(&wire::Start) -> Result<(), &'static str>) {
    let result = (|| {
        if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("--guarded-signin")) {
            return Err("the sign-in worker must be started by ngteams");
        }
        let start: wire::Start =
            wire::read(&mut io::stdin().lock()).map_err(|_| "the sign-in worker's private pipe failed")?;
        if start.identity != wire::identity() {
            return Err("the sign-in worker and app versions differ; restart ngteams after updating");
        }
        page(&start)
    })();
    if let Err(reason) = result {
        let _ = wire::send(&mut io::stdout().lock(), &wire::Event::Failed(reason.into()));
    }
}
