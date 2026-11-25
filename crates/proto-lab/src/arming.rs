//! CDP setup for captures: attach targets before their scripts run, enable Network events and install capture hooks.
//! Selective holds are capture experiments; every unmatched HTTP request and WebSocket frame goes through.
use crate::browser::{Browser, BrowserError};
use crate::capture::{Capture, RTC_BINDING};
use crate::capture_hold::{CaptureHold, Handled, HeldRequest, Hold};
use crate::cdp::{Cdp, CdpError, Event, Events, SessionId};
use crate::frame_hold::{self, FrameHold};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

pub const STEP_TIMEOUT: Duration = Duration::from_secs(10);
pub const DECISION_GRACE: Duration = Duration::from_secs(15);
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Page,
    Iframe,
    Worker,
    SharedWorker,
    ServiceWorker,
    Worklet,
    Other(String),
}
impl TargetKind {
    #[must_use]
    pub fn from_type(name: &str) -> Self {
        match name {
            "page" => Self::Page,
            "iframe" => Self::Iframe,
            "worker" => Self::Worker,
            "shared_worker" => Self::SharedWorker,
            "service_worker" => Self::ServiceWorker,
            "worklet" => Self::Worklet,
            other => Self::Other(other.into()),
        }
    }
    /// The target's CDP type, as `Target.attachedToTarget` names it.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Page => "page",
            Self::Iframe => "iframe",
            Self::Worker => "worker",
            Self::SharedWorker => "shared_worker",
            Self::ServiceWorker => "service_worker",
            Self::Worklet => "worklet",
            Self::Other(name) => name,
        }
    }
    #[must_use]
    pub fn steps(&self) -> &'static [Step] {
        match self {
            Self::Page | Self::Iframe => &[
                Step::Network,
                Step::BypassServiceWorker,
                Step::Runtime,
                Step::Page,
                Step::CaptureBinding,
                Step::CaptureHooks,
                Step::FrameBinding,
                Step::FrameHooks,
                Step::AutoAttach,
                Step::Resume,
            ],
            Self::Worklet => &[Step::Resume],
            _ => &[
                Step::Network,
                Step::Runtime,
                Step::FrameBinding,
                Step::FrameHooks,
                Step::AutoAttach,
                Step::Resume,
            ],
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Network,
    BypassServiceWorker,
    Runtime,
    Page,
    CaptureBinding,
    CaptureHooks,
    FrameBinding,
    FrameHooks,
    AutoAttach,
    Resume,
}
impl Step {
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::Network => "Network.enable",
            Self::BypassServiceWorker => "Network.setBypassServiceWorker",
            Self::Runtime => "Runtime.enable",
            Self::Page => "Page.enable",
            Self::CaptureBinding | Self::FrameBinding => "Runtime.addBinding",
            Self::CaptureHooks => "Page.addScriptToEvaluateOnNewDocument",
            Self::FrameHooks => "Runtime.evaluate",
            Self::AutoAttach => "Target.setAutoAttach",
            Self::Resume => "Runtime.runIfWaitingForDebugger",
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ArmError {
    #[error("capture setup step {step:?} failed: {source}")]
    Step {
        step: Step,
        #[source]
        source: CdpError,
    },
    #[error("capture setup step {0:?} got no answer within {STEP_TIMEOUT:?}")]
    StepTimeout(Step),
    #[error("an attach event without {0}")]
    Malformed(&'static str),
    #[error("no capture-ready page within {0:?}")]
    Timeout(Duration),
    #[error("the capture connection closed; the browser was stopped")]
    ConnectionLost,
    #[error(transparent)]
    Cdp(#[from] CdpError),
}
impl ArmError {
    #[must_use]
    pub fn session_gone(&self) -> bool {
        matches!(self,Self::Step{source:CdpError::Remote{error,..},..} if error.code == -32001)
            || matches!(
                self,
                Self::Step {
                    source: CdpError::Detached { .. },
                    ..
                }
            )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmedTarget {
    pub target_id: String,
    pub session: SessionId,
    pub kind: TargetKind,
    pub parent: Option<SessionId>,
}
#[derive(Default)]
struct Targets {
    attached: HashMap<SessionId, TargetKind>,
    ready: HashMap<SessionId, ArmedTarget>,
    failures: Vec<String>,
}
struct Shared {
    http: CaptureHold,
    capture: Option<Capture>,
    targets: Mutex<Targets>,
    changed: Notify,
    browser: Mutex<Option<Browser>>,
    lost: AtomicBool,
    closing: AtomicBool,
    working: AtomicUsize,
    settled: Notify,
    taps: Mutex<Vec<mpsc::UnboundedSender<Event>>>,
    frames: Mutex<Vec<FrameHold>>,
}
impl Shared {
    fn targets(&self) -> MutexGuard<'_, Targets> {
        self.targets.lock().unwrap_or_else(PoisonError::into_inner)
    }
    fn take_browser(&self) -> Option<Browser> {
        self.browser.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
    fn kill_browser(&self) {
        drop(self.take_browser());
    }
    fn tap(&self, event: &Event) {
        self.taps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|tap| tap.send(event.clone()).is_ok());
    }
    async fn settle(&self, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, async {
            loop {
                let notified = self.settled.notified();
                if self.working.load(Ordering::SeqCst) == 0 {
                    return;
                }
                notified.await;
            }
        })
        .await;
    }
}
struct Working(Arc<Shared>);
impl Working {
    fn start(shared: &Arc<Shared>) -> Self {
        shared.working.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(shared))
    }
}
impl Drop for Working {
    fn drop(&mut self) {
        if self.0.working.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.settled.notify_waiters();
        }
    }
}
/// The browser's capture setup status, also readable after close.
#[derive(Clone)]
pub struct CaptureStatus(Arc<Shared>);
impl CaptureStatus {
    #[must_use]
    pub fn held_requests(&self) -> Vec<HeldRequest> {
        self.0.http.held_requests()
    }
    #[must_use]
    pub fn failures(&self) -> Vec<String> {
        self.0.targets().failures.clone()
    }
    #[must_use]
    pub fn pending(&self) -> usize {
        self.0.working.load(Ordering::SeqCst)
    }
}
pub struct ArmedBrowser {
    cdp: Cdp,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}
impl Drop for ArmedBrowser {
    fn drop(&mut self) {
        self.shared.kill_browser();
        self.task.abort();
    }
}
impl ArmedBrowser {
    /// Takes ownership of the browser and attaches its targets for observation.
    /// # Errors
    /// If CDP setup fails; the owned browser is stopped on failure.
    pub async fn arm(browser: Browser, http: CaptureHold) -> Result<Self, ArmError> {
        Self::arm_inner(browser, http, None).await
    }
    /// # Errors
    /// As [`Self::arm`].
    pub async fn arm_capturing(browser: Browser, http: CaptureHold, capture: Capture) -> Result<Self, ArmError> {
        Self::arm_inner(browser, http, Some(capture)).await
    }
    async fn arm_inner(browser: Browser, http: CaptureHold, capture: Option<Capture>) -> Result<Self, ArmError> {
        let (cdp, events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
        let armed = Self::managed(cdp, events, http, capture, Some(browser));
        armed
            .cdp
            .call(None, "Fetch.enable", armed.shared.http.fetch_patterns())
            .await?;
        armed.cdp.call(None, "Target.setAutoAttach", attach(false)).await?;
        Ok(armed)
    }
    fn managed(
        cdp: Cdp,
        events: Events,
        http: CaptureHold,
        capture: Option<Capture>,
        browser: Option<Browser>,
    ) -> Self {
        let shared = Arc::new(Shared {
            http,
            capture,
            targets: Mutex::new(Targets::default()),
            changed: Notify::new(),
            browser: Mutex::new(browser),
            lost: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            working: AtomicUsize::new(0),
            settled: Notify::new(),
            taps: Mutex::new(Vec::new()),
            frames: Mutex::new(Vec::new()),
        });
        let task = tokio::spawn(handle_events(cdp.clone(), events, Arc::clone(&shared)));
        Self { cdp, shared, task }
    }
    /// # Errors
    /// When Chrome doesn't confirm closing or the process cannot be stopped.
    pub async fn close(self, grace: Duration) -> Result<(), BrowserError> {
        self.shared.closing.store(true, Ordering::SeqCst);
        let reply = tokio::time::timeout(grace, self.cdp.call(None, "Browser.close", Value::Null)).await;
        let result = match self.shared.take_browser() {
            Some(browser) => match reply {
                Ok(Ok(_)) => browser.wait_or_kill(grace).await.map(drop),
                other => {
                    browser.kill().await?;
                    Err(BrowserError::CloseUnconfirmed(format!("{other:?}")))
                }
            },
            None => Err(BrowserError::CloseUnconfirmed(
                "the capture browser is already gone".into(),
            )),
        };
        self.shared.settle(DECISION_GRACE).await;
        result
    }
    #[must_use]
    pub fn status(&self) -> CaptureStatus {
        CaptureStatus(Arc::clone(&self.shared))
    }
    #[must_use]
    pub fn cdp(&self) -> &Cdp {
        &self.cdp
    }
    #[must_use]
    pub fn watch(&self) -> mpsc::UnboundedReceiver<Event> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut taps = self.shared.taps.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.shared.lost.load(Ordering::SeqCst) {
            taps.push(tx);
        }
        rx
    }
    #[must_use]
    pub fn capture(&self) -> Option<&Capture> {
        self.shared.capture.as_ref()
    }
    /// # Errors
    /// When Chrome cannot create a tab or capture setup doesn't finish within the bound.
    pub async fn open_page(&self, timeout: Duration) -> Result<ArmedTarget, ArmError> {
        let created = self
            .cdp
            .call(None, "Target.createTarget", json!({"url":"about:blank"}))
            .await?;
        let id = created["targetId"].as_str().ok_or(ArmError::Malformed("targetId"))?;
        tokio::time::timeout(timeout, async {
            loop {
                let changed = self.shared.changed.notified();
                if let Some(target) = self
                    .shared
                    .targets()
                    .ready
                    .values()
                    .find(|target| target.target_id == id && target.parent.is_none())
                    .cloned()
                {
                    return Ok(target);
                }
                if self.shared.lost.load(Ordering::SeqCst) {
                    return Err(ArmError::ConnectionLost);
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| ArmError::Timeout(timeout))?
    }
    #[must_use]
    pub fn targets(&self) -> Vec<ArmedTarget> {
        self.shared.targets().ready.values().cloned().collect()
    }
    /// The session and kind of each attached target, armed yet or not.
    #[must_use]
    pub fn attached(&self) -> Vec<(SessionId, TargetKind)> {
        let targets = self.shared.targets();
        targets
            .attached
            .iter()
            .map(|(session, kind)| (session.clone(), kind.clone()))
            .collect()
    }
    pub fn hold(&self, rule: Hold) {
        self.shared.http.hold(rule);
    }
    pub fn clear_holds(&self) {
        self.shared.http.clear();
    }
    #[must_use]
    pub fn held_requests(&self) -> Vec<HeldRequest> {
        self.shared.http.held_requests()
    }
    /// Installs a selective frame hold in already attached contexts and all future contexts.
    /// # Errors
    /// If a current context cannot be updated.
    pub async fn hold_frames(&self, rule: FrameHold) -> Result<(), CdpError> {
        let script = {
            let mut frames = self.shared.frames.lock().unwrap_or_else(PoisonError::into_inner);
            frames.push(rule);
            frame_hold::script(&frames)
        };
        for target in self.targets() {
            if target.kind != TargetKind::Worklet {
                self.cdp
                    .call(
                        Some(&target.session),
                        "Runtime.addBinding",
                        json!({"name":frame_hold::BINDING}),
                    )
                    .await?;
                if matches!(target.kind, TargetKind::Page | TargetKind::Iframe) {
                    self.cdp
                        .call(
                            Some(&target.session),
                            "Page.addScriptToEvaluateOnNewDocument",
                            json!({"source":script,"runImmediately":true}),
                        )
                        .await?;
                } else {
                    self.cdp
                        .call(Some(&target.session), "Runtime.evaluate", json!({"expression":script}))
                        .await?;
                }
            }
        }
        Ok(())
    }
}
fn attach(children: bool) -> Value {
    let mut params = json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true});
    if children {
        params["filter"] = json!([{"type":"service_worker","exclude":true},{"type":"browser","exclude":true},{"type":"tab","exclude":true},{}]);
    }
    params
}
async fn handle_events(cdp: Cdp, mut events: Events, shared: Arc<Shared>) {
    while let Some(event) = events.recv().await {
        let attached = (event.method == "Target.attachedToTarget").then(|| attached_target(&event));
        // known before the tap: a watcher that subscribes later finds the target in its snapshot, not in its events
        if let Some(Some(target)) = &attached {
            shared
                .targets()
                .attached
                .insert(target.session.clone(), target.kind.clone());
        }
        shared.tap(&event);
        if let Some(capture) = &shared.capture {
            capture.observe(&cdp, &event);
        }
        match event.method.as_str() {
            "Fetch.requestPaused" => {
                let cdp = cdp.clone();
                let work = Working::start(&shared);
                tokio::spawn(async move {
                    let result = work.0.http.handle(&cdp, &event).await;
                    match (&work.0.capture, result) {
                        (Some(capture), Ok(Handled::Held)) => capture.held(&event),
                        (Some(capture), Ok(Handled::ResponseContinued)) => capture.continued(&event),
                        (Some(capture), Err(error)) => capture.unanswered(&event, &error.to_string()),
                        _ => {}
                    }
                });
            }
            "Target.attachedToTarget" => {
                let cdp = cdp.clone();
                let work = Working::start(&shared);
                let target = attached.flatten();
                tokio::spawn(async move {
                    on_attached(&cdp, &work.0, target).await;
                });
            }
            "Target.detachedFromTarget" => {
                if let Some(id) = event.params["sessionId"].as_str() {
                    let id = SessionId::new(id);
                    let mut targets = shared.targets();
                    targets.attached.remove(&id);
                    targets.ready.remove(&id);
                }
            }
            _ => {}
        }
    }
    let mut taps = shared.taps.lock().unwrap_or_else(PoisonError::into_inner);
    shared.lost.store(true, Ordering::SeqCst);
    taps.clear();
    drop(taps);
    if !shared.closing.load(Ordering::SeqCst) {
        shared.targets().failures.push("capture connection lost".into());
        shared.kill_browser();
    }
    shared.changed.notify_waiters();
}
fn attached_target(event: &Event) -> Option<ArmedTarget> {
    Some(ArmedTarget {
        target_id: event.params["targetInfo"]["targetId"].as_str()?.into(),
        session: SessionId::new(event.params["sessionId"].as_str()?),
        kind: TargetKind::from_type(event.params["targetInfo"]["type"].as_str().unwrap_or("?")),
        parent: event.session_id.clone(),
    })
}
async fn on_attached(cdp: &Cdp, shared: &Shared, target: Option<ArmedTarget>) {
    let Some(target) = target else {
        shared.targets().failures.push("malformed target attach event".into());
        shared.kill_browser();
        shared.changed.notify_waiters();
        return;
    };
    match setup_target(cdp, shared, &target).await {
        Ok(()) => {
            let mut targets = shared.targets();
            if targets.attached.contains_key(&target.session) {
                targets.ready.insert(target.session.clone(), target);
            }
        }
        Err(error) if error.session_gone() => {}
        Err(error) => {
            shared.targets().failures.push(error.to_string());
            shared.kill_browser();
        }
    }
    shared.changed.notify_waiters();
}
async fn setup_target(cdp: &Cdp, shared: &Shared, target: &ArmedTarget) -> Result<(), ArmError> {
    for &step in target.kind.steps() {
        let (frames, has_frames) = {
            let holds = shared.frames.lock().unwrap_or_else(PoisonError::into_inner);
            (frame_hold::script(&holds), !holds.is_empty())
        };
        if matches!(step, Step::FrameBinding | Step::FrameHooks) && !has_frames {
            continue;
        }
        let page = matches!(target.kind, TargetKind::Page | TargetKind::Iframe);
        let (method, params) = match step {
            Step::CaptureBinding => {
                if shared.capture.is_none() {
                    continue;
                }
                (step.method(), json!({"name":RTC_BINDING}))
            }
            Step::CaptureHooks => {
                let Some(capture) = &shared.capture else {
                    continue;
                };
                (
                    step.method(),
                    json!({"source":capture.hooks().source(),"runImmediately":true}),
                )
            }
            Step::FrameBinding => (step.method(), json!({"name":frame_hold::BINDING})),
            Step::FrameHooks if page => (
                "Page.addScriptToEvaluateOnNewDocument",
                json!({"source":frames,"runImmediately":true}),
            ),
            Step::FrameHooks => (step.method(), json!({"expression":frames})),
            Step::BypassServiceWorker => (step.method(), json!({"bypass":true})),
            Step::AutoAttach => (step.method(), attach(true)),
            _ => (step.method(), Value::Null),
        };
        tokio::time::timeout(STEP_TIMEOUT, cdp.call(Some(&target.session), method, params))
            .await
            .map_err(|_| ArmError::StepTimeout(step))?
            .map_err(|source| ArmError::Step { step, source })?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "../tests/support/arming_process.rs"]
mod test_process;
