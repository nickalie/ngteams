//! One lab account browser session, observed through CDP and optionally recorded.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use tokio::sync::mpsc;
use url::Url;

use crate::action_targets::ActionTargets;
use crate::arming::{ArmError, ArmedBrowser, CaptureStatus};
use crate::browser::{Browser, BrowserError, Display, LaunchOptions, Media};
use crate::capture::{Capture, CaptureError, Recorder, RtcHooks};
use crate::capture_hold::{CaptureHold, HeldRequest, Hold};
use crate::cdp::{Cdp, CdpError, Event, SessionId};
#[cfg(unix)]
use crate::commands::chrome::ChromeBuild;
use crate::config::{Alias, LabPaths};
use crate::frame_hold::FrameHold;
use crate::redirect_catch::RedirectCatch;
use crate::scenario::dsl::{Driver, NavigateError, navigate_tab};

/// Where the web client starts.
pub const TEAMS_URL: &str = "https://teams.microsoft.com/v2/";

/// The hosts of Microsoft's sign-in pages.
pub const SIGN_IN_HOSTS: [&str; 5] = [
    "login.microsoftonline.com",
    "login.microsoft.com",
    "login.live.com",
    "device.login.microsoftonline.com",
    "autologon.microsoftazuread-sso.com",
];

/// The authz answer that means Teams won't serve the signed-in account.
const FORBIDDEN: u16 = 403;
const TEAMS_ORIGINS: [&str; 2] = ["https://teams.microsoft.com", "https://teams.cloud.microsoft"];

/// How long a refusal is given to turn into a session (a retry that works) before it is believed.
pub const REFUSAL_GRACE: Duration = Duration::from_secs(5);

/// Where the session is, as far as the events tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing decided yet.
    Loading,
    /// The web client is on Teams with a working session.
    Loaded,
    /// Signed in, but Teams refused the account: its authz exchange got this status (403).
    Refused(u16),
    /// A Microsoft sign-in page stands in the main frame: the session has expired.
    SignInShown,
    /// The main frame finished loading on this other host.
    Elsewhere(String),
}

/// Who a page is signed in as: claims of a bearer token it sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The Entra object id (`oid`).
    pub oid: String,
    /// `upn`, else `preferred_username`, else `unique_name`.
    pub upn: String,
    /// `tid`.
    pub tenant: String,
}

/// An object id that isn't a GUID, so no MRI can be made of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the object id isn't a GUID")]
pub struct NotAGuid;

impl Identity {
    /// The account's MRI as Teams writes it: `8:orgid:` and the object id, lowercase.
    ///
    /// # Errors
    /// [`NotAGuid`] when the object id isn't shaped like a GUID.
    pub fn mri(&self) -> Result<String, NotAGuid> {
        let oid = self.oid.to_ascii_lowercase();
        let groups: Vec<&str> = oid.split('-').collect();
        let shaped = groups.len() == 5
            && groups
                .iter()
                .zip([8, 4, 4, 4, 12])
                .all(|(group, length)| group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if shaped {
            Ok(format!("8:orgid:{oid}"))
        } else {
            Err(NotAGuid)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    /// `about:blank`, or no main-frame document yet.
    Blank,
    Teams,
    SignIn,
    Other(String),
}

impl Place {
    fn of(url: &str) -> Self {
        let Ok(url) = Url::parse(url) else {
            return Self::Blank;
        };
        let origin = url.origin().ascii_serialization();
        match url.host_str() {
            None => Self::Blank,
            Some(_) if TEAMS_ORIGINS.contains(&origin.as_str()) => Self::Teams,
            Some(host) if url.scheme() == "https" && SIGN_IN_HOSTS.contains(&host) => Self::SignIn,
            Some(host) => Self::Other(host.to_owned()),
        }
    }
}

/// Tells from a page's events where its session is (see the module docs).
#[derive(Debug)]
pub struct SessionWatch {
    page: SessionId,
    main_frame: Option<String>,
    place: Place,
    /// The main frame finished loading since its last navigation.
    stopped: bool,
    /// An authz exchange succeeded since the main frame's last navigation.
    authz_ok: bool,
    /// Teams forbade an authz exchange since the main frame's last navigation.
    authz_forbidden: bool,
    /// The status of the last answer to an authz exchange.
    last_authz: Option<u16>,
    /// The method of every authz request seen since the main frame's last navigation, by session and request id. An
    /// answer to a request from before it belongs to the document that went away, and doesn't count.
    authz_requests: HashMap<(Option<SessionId>, String), String>,
    /// How many reads of the chat service or CSA ([`is_service_read`]) succeeded since the main frame's last navigation:
    /// any means the client works with tokens it already had (a cached skypetoken skips authz).
    service_reads_ok: usize,
    /// Every such read seen since the main frame's last navigation, by session and request id.
    service_requests: HashSet<(Option<SessionId>, String)>,
    /// How many documents the main frame has committed.
    documents: u64,
    /// How many of the main frame's last commits in a row were Entra's `/reprocess` page.
    reprocess_run: u32,
    /// The last error a sign-in host redirected back with: its `error` and the `AADSTS` code of its description.
    sign_in_error: Option<String>,
    identities: Vec<Identity>,
    action_targets: Option<Arc<ActionTargets>>,
}

impl SessionWatch {
    /// A watch on the tab whose session is `page`.
    #[must_use]
    pub fn new(page: SessionId) -> Self {
        Self {
            page,
            main_frame: None,
            place: Place::Blank,
            stopped: false,
            authz_ok: false,
            authz_forbidden: false,
            last_authz: None,
            authz_requests: HashMap::new(),
            service_reads_ok: 0,
            service_requests: HashSet::new(),
            documents: 0,
            reprocess_run: 0,
            sign_in_error: None,
            identities: Vec::new(),
            action_targets: None,
        }
    }

    /// Takes one event into account.
    pub fn observe(&mut self, event: &Event) {
        if let Some(targets) = &self.action_targets {
            targets.observe(event);
        }
        match event.method.as_str() {
            "Page.frameNavigated" if self.on_page(event) => self.navigated(&event.params["frame"]),
            "Page.frameStoppedLoading" if self.on_page(event) => {
                if self
                    .main_frame
                    .as_deref()
                    .is_some_and(|main| event.params["frameId"] == main)
                {
                    self.stopped = true;
                }
            }
            "Network.requestWillBeSent" | "Fetch.requestPaused" => self.request(event),
            "Network.responseReceived" => self.response(event),
            _ => {}
        }
    }

    fn on_page(&self, event: &Event) -> bool {
        event.session_id.as_ref() == Some(&self.page)
    }

    fn navigated(&mut self, frame: &Value) {
        if frame.get("parentId").is_some() {
            return;
        }
        self.main_frame = frame["id"].as_str().map(str::to_owned);
        let url = frame["url"].as_str().unwrap_or_default();
        self.place = Place::of(url);
        self.reprocess_run = if self.place == Place::SignIn && is_reprocess(url) {
            self.reprocess_run.saturating_add(1)
        } else {
            0
        };
        self.stopped = false;
        self.authz_ok = false;
        self.authz_forbidden = false;
        self.authz_requests.clear();
        self.service_reads_ok = 0;
        self.service_requests.clear();
        self.documents += 1;
    }

    fn request(&mut self, event: &Event) {
        if let Some(error) = sign_in_error(&event.params) {
            self.sign_in_error = Some(error);
        }
        let request = &event.params["request"];
        if let Some(identity) = identity_in(&request["headers"])
            && !self.identities.contains(&identity)
        {
            self.identities.push(identity);
        }
        // `Fetch.requestPaused` ids are the interceptor's, not the network's; the responses carry the network's
        if event.method == "Network.requestWillBeSent"
            && let (Some(id), Some(url), Some(method)) = (
                event.params["requestId"].as_str(),
                request["url"].as_str(),
                request["method"].as_str(),
            )
        {
            let key = (event.session_id.clone(), id.to_owned());
            if is_authz(url) {
                self.authz_requests.insert(key, method.to_ascii_uppercase());
            } else if method.eq_ignore_ascii_case("GET") && is_service_read(url) {
                self.service_requests.insert(key);
            }
        }
    }

    fn response(&mut self, event: &Event) {
        let Some(id) = event.params["requestId"].as_str() else {
            return;
        };
        let key = (event.session_id.clone(), id.to_owned());
        let Some(Ok(status)) = event.params["response"]["status"].as_u64().map(u16::try_from) else {
            return;
        };
        if self.service_requests.contains(&key) {
            if (200..=299).contains(&status) {
                self.service_reads_ok += 1;
            }
            return;
        }
        if self.authz_requests.get(&key).is_none_or(|method| method != "POST") {
            return;
        }
        self.last_authz = Some(status);
        match status {
            200..=299 => self.authz_ok = true,
            FORBIDDEN => self.authz_forbidden = true,
            _ => {}
        }
    }

    /// Where the session is now.
    #[must_use]
    pub fn state(&self) -> SessionState {
        match &self.place {
            Place::Teams if self.authz_ok || (self.service_reads_ok > 0 && !self.authz_forbidden) => {
                SessionState::Loaded
            }
            Place::Teams if self.authz_forbidden => SessionState::Refused(FORBIDDEN),
            Place::SignIn if self.stopped => SessionState::SignInShown,
            Place::Other(host) if self.stopped => SessionState::Elsewhere(host.clone()),
            _ => SessionState::Loading,
        }
    }

    /// Which document the main frame is on: it counts the main frame's navigations (`0` before the first), and goes up
    /// whenever a new document commits there, a reload of the same URL included. Changes within a document (a history
    /// push, the page changing its content) leave it alone.
    #[must_use]
    pub fn document(&self) -> u64 {
        self.documents
    }

    /// The last error a sign-in host answered with, as `<error> <AADSTS code>` (no description: it can name the user).
    #[must_use]
    pub fn sign_in_error(&self) -> Option<&str> {
        self.sign_in_error.as_deref()
    }

    /// How many times in a row the main frame last committed Entra's `/reprocess` page (`0` when it is elsewhere).
    #[must_use]
    pub fn reprocess_run(&self) -> u32 {
        self.reprocess_run
    }

    /// The status of the last answer to the web client's authz exchange, for a report on a session that didn't load.
    #[must_use]
    pub fn last_authz_status(&self) -> Option<u16> {
        self.last_authz
    }

    /// The identity the page has shown for `upn` (case aside). `None` until a request carried it, and when tokens for
    /// that UPN named more than one object id or tenant.
    #[must_use]
    pub fn identity_for(&self, upn: &str) -> Option<&Identity> {
        let mut matching = self
            .identities
            .iter()
            .filter(|identity| identity.upn.eq_ignore_ascii_case(upn));
        let first = matching.next()?;
        matching
            .all(|other| other.oid == first.oid && other.tenant == first.tenant)
            .then_some(first)
    }
}

/// The error of a redirect a sign-in host made (`Network.requestWillBeSent`'s `redirectResponse`), from its
/// `Location`'s fragment or query: `error`, and the `AADSTS` code of `error_description` when it has one.
fn sign_in_error(params: &Value) -> Option<String> {
    let redirect = params.get("redirectResponse")?;
    let from = Url::parse(redirect["url"].as_str()?).ok()?;
    if Place::of(from.as_str()) != Place::SignIn {
        return None;
    }
    let headers = redirect["headers"].as_object()?;
    let location = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("location"))?
        .1
        .as_str()?;
    let location = from.join(location).ok()?;
    let fields = |part: Option<&str>| {
        url::form_urlencoded::parse(part.unwrap_or_default().as_bytes())
            .into_owned()
            .collect::<HashMap<_, _>>()
    };
    let (fragment, query) = (fields(location.fragment()), fields(location.query()));
    let error = fragment.get("error").or_else(|| query.get("error"))?;
    let description = fragment
        .get("error_description")
        .or_else(|| query.get("error_description"));
    let code = description.and_then(|text| {
        let at = text.find("AADSTS")?;
        let digits = text[at + 6..].chars().take_while(char::is_ascii_digit).count();
        (digits > 0).then(|| text[at..at + 6 + digits].to_owned())
    });
    let error: String = error
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    Some(code.map_or_else(|| error.clone(), |code| format!("{error} {code}")))
}

/// Entra's `/<tenant>/reprocess` page: it re-runs a sign-in with the session the browser already has.
fn is_reprocess(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| url.path().ends_with("/reprocess"))
}

/// Holds a refusal back until it has stood for [`REFUSAL_GRACE`]: until then the client may still retry its way into a
/// session.
#[derive(Clone, Copy, Debug, Default)]
pub struct Settling {
    refused_since: Option<tokio::time::Instant>,
}

impl Settling {
    /// What `state`, seen at `now`, counts as: [`SessionState::Loading`] for a refusal that hasn't stood unbroken for
    /// [`REFUSAL_GRACE`] since it was first seen, `state` itself otherwise. Any other state ends the refusal's stand.
    pub fn judge(&mut self, state: SessionState, now: tokio::time::Instant) -> SessionState {
        let SessionState::Refused(_) = state else {
            self.refused_since = None;
            return state;
        };
        let since = *self.refused_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= REFUSAL_GRACE {
            state
        } else {
            SessionState::Loading
        }
    }
}

/// Teams' authz endpoints of the commercial cloud, as the web client names them.
const AUTHZ: [&str; 4] = [
    "https://teams.microsoft.com/api/authsvc/v1.0/authz",
    "https://teams.microsoft.com/api/authsvc/v1.0/authzcore",
    "https://authsvc.teams.microsoft.com/v1.0/authz",
    "https://authsvc.teams.microsoft.com/v1.0/authzcore",
];

/// Whether `url` is one of Teams' authz endpoints ([`AUTHZ`]), with no query.
fn is_authz(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| url.query().is_none() && url.fragment().is_none() && AUTHZ.contains(&url.as_str()))
}

/// Whether `url` is a read the web client makes only with a working session: the chat service or the chat service
/// aggregator (CSA) on a Teams origin, `https://<teams origin>/api/{chatsvc,csa}/…`. An account Teams refuses never
/// gets that far (no skypetoken, no service tokens' use).
fn is_service_read(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    let origin = url.origin().ascii_serialization();
    TEAMS_ORIGINS.contains(&origin.as_str())
        && (url.path().starts_with("/api/chatsvc/") || url.path().starts_with("/api/csa/"))
}

/// The identity in a request's `Authorization: Bearer` token, if it is a JWT with the claims.
pub(crate) fn identity_in(headers: &Value) -> Option<Identity> {
    let value = headers
        .as_object()?
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))?
        .1
        .as_str()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut parts = token.trim().split('.');
    let (_, payload, _, None) = (parts.next()?, parts.next()?, parts.next()?, parts.next()) else {
        return None;
    };
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    let text = |name: &str| claims[name].as_str().map(str::to_owned);
    Some(Identity {
        oid: text("oid")?,
        upn: text("upn")
            .or_else(|| text("preferred_username"))
            .or_else(|| text("unique_name"))?,
        tenant: text("tid")?,
    })
}

/// Failures in the capture connection or target setup; selective capture holds are not failures.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionReport {
    pub failures: Vec<String>,
}
impl SessionReport {
    #[must_use]
    pub fn of(status: &CaptureStatus) -> Self {
        Self {
            failures: status.failures(),
        }
    }
    #[must_use]
    pub fn failed(&self) -> bool {
        !self.failures.is_empty()
    }
    #[must_use]
    pub fn summary(&self) -> String {
        if self.failed() {
            self.failures.join("; ")
        } else {
            "capture setup complete".into()
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("browser action target evidence: {0}")]
    ActionTargets(String),
    #[error("no Chrome for Testing at {} (run `proto-lab chrome install`)",.0.display())]
    NoChrome(PathBuf),
    #[cfg(windows)]
    #[error("Microsoft Edge is unavailable: {0}")]
    NoEdge(#[source] std::io::Error),
    #[error(transparent)]
    Browser(#[from] BrowserError),
    #[error(transparent)]
    Arm(#[from] ArmError),
    #[error(transparent)]
    Cdp(#[from] CdpError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("{0} capture setup operation(s) still pending at close")]
    Pending(usize),
    #[error("the tab didn't answer a navigation within {0:?}")]
    Navigate(Duration),
}

/// How long [`LabSession::navigate`] waits for the tab to answer a navigation: as long as a step's call to a busy
/// page may take ([`crate::scenario::ab_chat_full::PAGE_CALL`]).
pub const NAVIGATE_TIMEOUT: Duration = Duration::from_secs(90);

/// How a lab session runs.
#[derive(Clone, Debug)]
pub struct SessionOptions {
    /// Record the run as a capture of this scenario (`captures/<scenario>/<ts>/` in the lab's data dir).
    pub capture: Option<String>,
    /// Whether the capture keeps request bodies; `login` keeps none ([`Capture::without_request_bodies`]).
    pub capture_request_bodies: bool,
    pub display: Display,
    /// The browser's camera and microphone: the system's (and a prompt), or Chrome's fake devices, the microphone
    /// playing a WAV file (`echo-call`).
    pub media: Media,
    /// The `User-Agent` the browser sends instead of its own ([`LaunchOptions::user_agent`]).
    pub user_agent: Option<String>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            capture: None,
            capture_request_bodies: true,
            display: Display::Headless,
            media: Media::System,
            user_agent: None,
        }
    }
}

/// How long the browser gets to put its debugging endpoint up.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the first tab gets to be armed.
const PAGE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the browser gets to exit after closing.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// One lab account's browser, armed, with one tab and its events watched.
pub struct LabSession {
    armed: ArmedBrowser,
    page: SessionId,
    events: mpsc::UnboundedReceiver<Event>,
    watch: SessionWatch,
    /// The run's capture, if it records.
    recorder: Option<Arc<Recorder>>,
}

impl LabSession {
    /// [`Self::open_with`] headless and not recorded ([`SessionOptions::default`]).
    ///
    /// # Errors
    /// As [`Self::open_with`].
    pub async fn open(paths: &LabPaths, alias: Alias) -> Result<Self, SessionError> {
        Self::open_with(paths, alias, SessionOptions::default()).await
    }

    /// Launches the pinned Chrome on the selected lab profile, attaches capture instrumentation, and opens a blank tab.
    /// # Errors
    /// If Chrome is missing, a capture cannot be created, or browser/CDP setup fails.
    pub async fn open_with(paths: &LabPaths, alias: Alias, options: SessionOptions) -> Result<Self, SessionError> {
        Self::open_armed(paths, alias, options, None).await
    }

    /// [`Self::open`], with the HTTP layer also catching the authorization redirect ([`RedirectCatch`]; `proto-lab
    /// auth` only).
    ///
    /// # Errors
    /// As [`Self::open_with`].
    pub async fn open_catching(paths: &LabPaths, alias: Alias, catch: RedirectCatch) -> Result<Self, SessionError> {
        Self::open_armed(paths, alias, SessionOptions::default(), Some(catch)).await
    }

    async fn open_armed(
        paths: &LabPaths,
        alias: Alias,
        options: SessionOptions,
        catch: Option<RedirectCatch>,
    ) -> Result<Self, SessionError> {
        #[cfg(unix)]
        let chrome = ChromeBuild::pinned().executable(&paths.chrome_dir());
        #[cfg(windows)]
        let chrome = crate::browser::installed_edge().map_err(SessionError::NoEdge)?;
        if !chrome.is_file() {
            return Err(SessionError::NoChrome(chrome));
        }
        let action_targets = if matches!(options.capture.as_deref(), Some("ab-call" | "abc-call")) {
            let locations = crate::config::LabLocations::from_env()
                .map_err(|error| SessionError::ActionTargets(error.to_string()))?;
            let config = crate::config::LabConfig::load(&locations)
                .map_err(|error| SessionError::ActionTargets(error.to_string()))?;
            Some(Arc::new(
                ActionTargets::new(&config, alias).map_err(SessionError::ActionTargets)?,
            ))
        } else {
            None
        };
        let http = match catch {
            Some(catch) => CaptureHold::new().with_pending_messages().with_redirect_catch(catch),
            None => CaptureHold::new().with_pending_messages(),
        };
        let recorder = match &options.capture {
            Some(scenario) => Some(Arc::new(Recorder::create(&paths.captures_dir(), scenario)?)),
            None => None,
        };
        let launch = LaunchOptions {
            display: options.display,
            media: options.media.clone(),
            user_agent: options.user_agent.clone(),
            ..LaunchOptions::for_profile(paths, alias)
        };
        let browser = Browser::launch(&chrome, &launch, LAUNCH_TIMEOUT).await?;
        let armed = match &recorder {
            Some(recorder) => {
                let capture = Capture::new(Arc::clone(recorder), RtcHooks::default());
                let capture = if options.capture_request_bodies {
                    capture
                } else {
                    capture.without_request_bodies()
                };
                ArmedBrowser::arm_capturing(browser, http, capture).await?
            }
            None => ArmedBrowser::arm(browser, http).await?,
        };
        let events = armed.watch();
        let page = armed.open_page(PAGE_TIMEOUT).await?.session;
        let mut watch = SessionWatch::new(page.clone());
        watch.action_targets = action_targets;
        Ok(Self {
            armed,
            page,
            events,
            watch,
            recorder,
        })
    }

    /// A scenario driver on the tab, with its own watch of the events from now on.
    #[must_use]
    pub fn driver(&self) -> Driver {
        Driver::new(self.armed.cdp().clone(), self.page.clone(), self.armed.watch())
            .with_targets(self.armed.targets())
            .with_action_targets(self.watch.action_targets.clone())
    }

    /// Declares the exact conversation and participants of this browser call scenario.
    #[cfg(unix)]
    pub(crate) fn declare_target(&self, chat: &str, members: &[String]) -> Result<(), SessionError> {
        self.watch
            .action_targets
            .as_ref()
            .ok_or_else(|| SessionError::ActionTargets("no explicit scenario identity evidence".to_owned()))?
            .declare(chat, members)
            .map_err(SessionError::ActionTargets)
    }

    /// Where the run is recorded, if it is.
    #[must_use]
    pub fn capture_dir(&self) -> Option<&Path> {
        self.recorder.as_deref().map(Recorder::dir)
    }

    /// Holds matching requests for capture without sending them.
    pub fn hold(&self, rule: Hold) {
        self.armed.hold(rule);
    }
    /// # Errors
    /// If a current target cannot install the requested frame hold.
    pub async fn hold_frames(&self, rule: FrameHold) -> Result<(), CdpError> {
        self.armed.hold_frames(rule).await
    }
    pub fn clear_holds(&self) {
        self.armed.clear_holds();
    }
    #[must_use]
    pub fn held_requests(&self) -> Vec<HeldRequest> {
        self.armed.held_requests()
    }

    /// Navigates the tab, within [`NAVIGATE_TIMEOUT`] ([`navigate_tab`]: a dialog asking before the page is left is
    /// accepted). Unbounded, a navigation held by such a dialog hung an `ab-image` run for two hours (2026-10-21).
    ///
    /// # Errors
    /// When the browser refuses the navigation, or doesn't answer it in time.
    pub async fn navigate(&self, url: &str) -> Result<(), SessionError> {
        navigate_tab(self.armed.cdp(), &self.page, url, NAVIGATE_TIMEOUT)
            .await
            .map_err(|error| match error {
                NavigateError::Cdp(error) => SessionError::Cdp(error),
                NavigateError::Timeout(waited) => SessionError::Navigate(waited),
            })
    }

    /// Feeds the watch every event that has come in, waiting up to `wait` for one if none has. `false` once the
    /// connection is gone.
    pub async fn pump(&mut self, wait: Duration) -> bool {
        match tokio::time::timeout(wait, self.events.recv()).await {
            Ok(Some(event)) => self.watch.observe(&event),
            Ok(None) => return false,
            Err(_) => return true,
        }
        loop {
            match self.events.try_recv() {
                Ok(event) => self.watch.observe(&event),
                Err(mpsc::error::TryRecvError::Empty) => return true,
                Err(mpsc::error::TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Pumps until the session is anything but [`SessionState::Loading`] ([`Settling`]: a refusal counts once it has
    /// stood for [`REFUSAL_GRACE`]). `Loading` when `timeout` passes first, or the connection is lost, a refusal that
    /// hasn't stood its grace by then included.
    pub async fn settle(&mut self, timeout: Duration) -> SessionState {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut settling = Settling::default();
        loop {
            let state = settling.judge(self.watch.state(), tokio::time::Instant::now());
            if state != SessionState::Loading || tokio::time::Instant::now() >= deadline {
                return state;
            }
            if !self.pump(Duration::from_millis(500)).await {
                return SessionState::Loading;
            }
        }
    }

    #[must_use]
    pub fn watch(&self) -> &SessionWatch {
        &self.watch
    }

    #[must_use]
    pub fn cdp(&self) -> &Cdp {
        self.armed.cdp()
    }

    /// The tab's session.
    #[must_use]
    pub fn page(&self) -> &SessionId {
        &self.page
    }

    #[must_use]
    pub fn report(&self) -> SessionReport {
        SessionReport::of(&self.armed.status())
    }
    #[must_use]
    pub fn status(&self) -> CaptureStatus {
        self.armed.status()
    }
    /// Closes the owned browser and finishes its capture.
    /// # Errors
    /// If browser shutdown or capture flushing fails.
    pub async fn close(self) -> Result<SessionReport, SessionError> {
        let status = self.armed.status();
        let closed = self.armed.close(CLOSE_GRACE).await;
        if let Some(recorder) = &self.recorder {
            recorder.finish()?;
        }
        closed?;
        if status.pending() > 0 {
            return Err(SessionError::Pending(status.pending()));
        }
        Ok(SessionReport::of(&status))
    }
    /// # Errors
    /// As [`Self::close`].
    pub async fn close_recorded(self) -> Result<(SessionReport, Option<PathBuf>), SessionError> {
        let dir = self.capture_dir().map(Path::to_path_buf);
        Ok((self.close().await?, dir))
    }
}

#[cfg(test)]
#[path = "../tests/support/session.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/support/session_startup.rs"]
mod startup_tests;
