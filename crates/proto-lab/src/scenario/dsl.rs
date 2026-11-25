//! The scenario steps: what a scenario does to an armed tab and waits for, each step named so that a failure says
//! which one.
//!
//! A [`Driver`] drives one tab over the armed connection and reads a copy of the connection's events
//! ([`crate::arming::ArmedBrowser::watch`]): every request any target of the browser sends (`Network.requestWillBeSent`)
//! and how it ended. It drives and observes; capture holds selectively stop matching requests.
//!
//! - [`Driver::wait_for`]: an element is in the page, found by its `data-tid` (the attribute the Teams web client marks
//!   its controls with, [`Selector::Tid`]) or a CSS selector ([`Selector::Css`]);
//! - [`Driver::wait_for_request`]: a request matching a [`RequestPattern`] was sent, since the driver began or since
//!   the request the last wait matched (so a request sent just before the wait isn't missed, and none is matched twice);
//! - [`Driver::wait_for_failure`]: that request failed, and how (a held one with `net::ERR_BLOCKED_BY_CLIENT`);
//! - [`Driver::wait_for_text`]: the page shows a text;
//! - [`Driver::wait_for_answer`]: a request matching a pattern was answered (or failed), and how;
//! - [`Driver::navigate`], [`Driver::click`], [`Driver::type_into`], [`Driver::insert_text`] (into the focused
//!   element), [`Driver::press_enter`], [`Driver::press`] (any
//!   [`Key`]) and [`Driver::hover`] (a real mouse event, for what shows only under the pointer).
//!
//! A wait goes on across a navigation of its page: an evaluation the browser refuses because the page navigated
//! ([`navigated_away`]) is a look that found nothing yet. A wait that runs out fails with [`StepError::Timeout`],
//! naming the step, how long it waited and what for. So does
//! any call to the page that gets no answer within the driver's call timeout ([`CALL_TIMEOUT`] unless
//! [`Driver::set_call_timeout`] sets another): a page that stops answering (a busy renderer, a modal dialog) fails the
//! step instead of hanging the run before its teardown.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use regex::Regex;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::action_targets::ActionTargets;
use crate::arming::ArmedTarget;
use crate::capture_hold::without_query;
use crate::cdp::{Cdp, CdpError, Event, SessionId};

/// How often a wait on the page looks again.
pub const POLL: Duration = Duration::from_millis(200);

/// How long one call to the page (an evaluation, an input, a navigation, a screenshot) may go unanswered.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a navigation waits for its answer before it looks for a dialog holding it ([`navigate_tab`]).
pub const DIALOG_WAIT: Duration = Duration::from_secs(5);
/// How long a recipients' check waits for a page to make a chat read of its own account before it gives up.
pub const CREDENTIAL_WAIT: Duration = Duration::from_secs(15);

/// Why [`navigate_tab`] failed.
#[derive(Debug, thiserror::Error)]
pub enum NavigateError {
    #[error(transparent)]
    Cdp(#[from] CdpError),
    #[error("the navigation got no answer within {0:?}")]
    Timeout(Duration),
}

/// Navigates the tab `page` to `url`, within `timeout` in all.
///
/// A page that asks before it is left (a `beforeunload` handler, which the web client sets while a message of the
/// compose box is still being sent, such as an image whose upload a capture hold stops) holds `Page.navigate` unanswered
/// until its dialog is answered, and a headless browser shows that dialog to nobody: live, 2026-10-21, an `ab-image`
/// run hung there for two hours. So when the navigation isn't answered within [`DIALOG_WAIT`], the dialog is accepted
/// (`Page.handleJavaScriptDialog`, the page is left, as the step asked; an error there means no dialog is showing) and
/// the navigation gets the rest of `timeout`.
///
/// # Errors
/// [`NavigateError::Timeout`] when it isn't answered within `timeout`; [`NavigateError::Cdp`] when the browser refuses
/// it or the connection goes.
pub async fn navigate_tab(cdp: &Cdp, page: &SessionId, url: &str, timeout: Duration) -> Result<(), NavigateError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let navigate = cdp.call(Some(page), "Page.navigate", json!({"url": url}));
    tokio::pin!(navigate);
    if let Ok(answer) = tokio::time::timeout(DIALOG_WAIT.min(timeout), &mut navigate).await {
        return answer.map(drop).map_err(NavigateError::from);
    }
    let accept = cdp.call(Some(page), "Page.handleJavaScriptDialog", json!({"accept": true}));
    // no dialog showing is an error answer, and the wait for the navigation goes on either way
    let _ = tokio::time::timeout_at(deadline, accept).await;
    match tokio::time::timeout_at(deadline, navigate).await {
        Ok(answer) => answer.map(drop).map_err(NavigateError::from),
        Err(_) => Err(NavigateError::Timeout(timeout)),
    }
}

/// Why a step failed.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("step `{step}`: timed out after {waited:?} waiting for {waiting_for}")]
    Timeout {
        step: String,
        waited: Duration,
        waiting_for: String,
    },
    #[error("step `{step}`: {source}")]
    Cdp {
        step: String,
        #[source]
        source: CdpError,
    },
    #[error("step `{step}`: the browser connection is gone")]
    Closed { step: String },
    #[error("step `{step}`: {reason}")]
    Failed { step: String, reason: String },
}

impl StepError {
    /// The step that failed.
    #[must_use]
    pub fn step(&self) -> &str {
        match self {
            Self::Timeout { step, .. } | Self::Cdp { step, .. } | Self::Closed { step } | Self::Failed { step, .. } => {
                step
            }
        }
    }
}

/// How a step finds an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector<'a> {
    /// The element with this `data-tid`.
    Tid(&'a str),
    /// The first element this CSS selector matches.
    Css(&'a str),
}

impl Selector<'_> {
    /// The page expression that finds the element (or `null`).
    fn query(self) -> String {
        match self {
            Self::Tid(tid) => format!(
                "document.querySelector('[data-tid=\"' + CSS.escape({}) + '\"]')",
                js_string(tid)
            ),
            Self::Css(css) => format!("document.querySelector({})", js_string(css)),
        }
    }
}

impl fmt::Display for Selector<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tid(tid) => write!(f, "an element with data-tid {tid:?}"),
            Self::Css(css) => write!(f, "an element matching {css:?}"),
        }
    }
}

/// A key [`Driver::press`] presses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// The key's value (`KeyboardEvent.key`).
    pub key: &'static str,
    /// The physical key (`KeyboardEvent.code`).
    pub code: &'static str,
    /// The Windows virtual key code.
    pub virtual_key: u32,
    /// The text the key types, if it types any.
    pub text: Option<&'static str>,
    /// The modifiers held: Alt 1, Ctrl 2, Meta 4, Shift 8.
    pub modifiers: u32,
}

impl Key {
    /// Enter: sends a message from the compose box.
    pub const ENTER: Self = Self {
        key: "Enter",
        code: "Enter",
        virtual_key: 13,
        text: Some("\r"),
        modifiers: 0,
    };
    /// Escape: closes a menu or a popup.
    pub const ESCAPE: Self = Self {
        key: "Escape",
        code: "Escape",
        virtual_key: 27,
        text: None,
        modifiers: 0,
    };
    /// Backspace: deletes the selection, or the character before the caret.
    pub const BACKSPACE: Self = Self {
        key: "Backspace",
        code: "Backspace",
        virtual_key: 8,
        text: None,
        modifiers: 0,
    };
    /// Shift+Enter: a line break in the compose box, where Enter alone sends.
    pub const SHIFT_ENTER: Self = Self {
        key: "Enter",
        code: "Enter",
        virtual_key: 13,
        text: Some("\r"),
        modifiers: 8,
    };
    /// Space, pressed as a key (an editor may act on the key, an autolink, where an inserted space does nothing).
    pub const SPACE: Self = Self {
        key: " ",
        code: "Space",
        virtual_key: 32,
        text: Some(" "),
        modifiers: 0,
    };
    /// Tab: moves the focus to the next control.
    pub const TAB: Self = Self {
        key: "Tab",
        code: "Tab",
        virtual_key: 9,
        text: None,
        modifiers: 0,
    };
    /// Shift+Tab: moves the focus to the previous control.
    pub const SHIFT_TAB: Self = Self {
        key: "Tab",
        code: "Tab",
        virtual_key: 9,
        text: None,
        modifiers: 8,
    };
    /// Ctrl+A: selects everything in the focused editor.
    pub const SELECT_ALL: Self = Self {
        key: "a",
        code: "KeyA",
        virtual_key: 65,
        text: None,
        modifiers: 2,
    };
}

/// Which requests a wait is for: a method (any if `None`), a regular expression found in the URL and, optionally, a
/// text the body holds ([`RequestPattern::with_body_containing`]).
#[derive(Clone, Debug)]
pub struct RequestPattern {
    method: Option<String>,
    url: Regex,
    body: Option<String>,
}

impl RequestPattern {
    /// # Errors
    /// When `url` doesn't compile.
    pub fn new(method: Option<&str>, url: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            method: method.map(str::to_ascii_uppercase),
            url: Regex::new(url)?,
            body: None,
        })
    }

    /// The same pattern, matching only a request whose body (as the `Network` events carry it inline) holds `text`: a
    /// message post rather than the typing posts to the same URL. A request whose body the events left out never
    /// matches.
    #[must_use]
    pub fn with_body_containing(mut self, text: &str) -> Self {
        self.body = Some(text.to_owned());
        self
    }

    /// Whether a request of `method` to `url` matches the method and the URL (a pattern with a body text needs
    /// [`Self::matches_request`]).
    #[must_use]
    pub fn matches(&self, method: &str, url: &str) -> bool {
        self.method
            .as_deref()
            .is_none_or(|wanted| wanted.eq_ignore_ascii_case(method))
            && self.url.is_match(url)
    }

    /// Whether `request` matches: its method, its URL and, for a pattern with a body text, its body.
    #[must_use]
    pub fn matches_request(&self, request: &SeenRequest) -> bool {
        self.matches(&request.method, &request.url)
            && self
                .body
                .as_deref()
                .is_none_or(|text| request.body.as_deref().is_some_and(|body| body.contains(text)))
    }
}

impl fmt::Display for RequestPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a {} request to /{}/",
            self.method.as_deref().unwrap_or("any"),
            self.url
        )?;
        match &self.body {
            Some(text) => write!(f, " holding {text:?}"),
            None => Ok(()),
        }
    }
}

/// A request a target sent, as the `Network` events tell it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenRequest {
    /// The session it was seen on (`None`: the browser's).
    pub session: Option<SessionId>,
    /// The `Network` request id, on that session.
    pub request_id: String,
    pub method: String,
    pub url: String,
    /// The response status, once one came.
    pub status: Option<u16>,
    /// Why it failed (`Network.loadingFailed`'s `errorText`), if it did.
    pub failed: Option<String>,
    /// Its body, when the event carried it inline (`postData`); kept in memory only, for
    /// [`RequestPattern::with_body_containing`].
    pub body: Option<String>,
    /// When it was sent, in ms since the Unix epoch (the event's `wallTime`), if the event said: what a scenario times
    /// a signal by (`chat-shapes`' typing posts).
    pub sent_ms: Option<u64>,
}

/// Drives one armed tab (see the module docs).
pub struct Driver {
    cdp: Cdp,
    page: SessionId,
    events: mpsc::UnboundedReceiver<Event>,
    requests: Vec<SeenRequest>,
    /// The CDP type (`worker`, `shared_worker`, `service_worker`, `iframe`, …) of each target session seen.
    kinds: HashMap<SessionId, String>,
    /// The first request a request wait may match.
    cursor: usize,
    closed: bool,
    /// How long one call to the page may go unanswered.
    call_timeout: Duration,
    /// Synthetic peers can omit observation pauses while retaining all request deadlines.
    #[cfg(test)]
    #[cfg(unix)]
    pause_override: Option<Duration>,
    action_targets: Option<Arc<ActionTargets>>,
}

impl Driver {
    /// A driver of the tab `page`, reading `events` (a watch of the armed connection).
    #[must_use]
    pub fn new(cdp: Cdp, page: SessionId, events: mpsc::UnboundedReceiver<Event>) -> Self {
        Self {
            cdp,
            page,
            events,
            requests: Vec::new(),
            kinds: HashMap::new(),
            cursor: 0,
            closed: false,
            call_timeout: CALL_TIMEOUT,
            #[cfg(test)]
            #[cfg(unix)]
            pause_override: None,
            action_targets: None,
        }
    }

    /// The same driver, knowing the kind of each target attached before it began.
    #[must_use]
    pub fn with_targets(mut self, targets: impl IntoIterator<Item = ArmedTarget>) -> Self {
        for target in targets {
            self.kinds.insert(target.session, target.kind.name().to_owned());
        }
        self
    }

    /// Which target sent `request`: `page` (the driven tab), the CDP type of another target (`worker`, `iframe`,
    /// `shared_worker`, `service_worker`, …), `browser` for an event of the browser itself, or `unknown target`.
    #[must_use]
    pub fn origin(&self, request: &SeenRequest) -> &str {
        match &request.session {
            None => "browser",
            Some(session) if *session == self.page => "page",
            Some(session) => self.kinds.get(session).map_or("unknown target", String::as_str),
        }
    }

    pub(crate) fn with_action_targets(mut self, targets: Option<Arc<ActionTargets>>) -> Self {
        self.action_targets = targets;
        self
    }

    /// Reads full recipients fresh and verifies the actual actor before an explicit call scenario action.
    ///
    /// # Errors
    /// Fails on unknown actor evidence, an unavailable or incomplete read, or a mismatch with the scenario's
    /// declared participants.
    pub async fn preflight_recipients(&mut self, step: &str, expected_chat: &str) -> Result<(), StepError> {
        // a page that made no chat read of its own yet (a callee's on the Calls app) gets a while to make one
        let deadline = tokio::time::Instant::now() + CREDENTIAL_WAIT;
        loop {
            self.drain();
            let targets = self.action_targets.as_ref().ok_or_else(|| StepError::Failed {
                step: step.to_owned(),
                reason: "no explicit lab action target context".to_owned(),
            })?;
            match targets.recipients(expected_chat).await {
                Err(reason)
                    if reason == crate::action_targets::NO_CREDENTIAL && tokio::time::Instant::now() < deadline =>
                {
                    // the deadline passing is the read's own failure, said below on the next round
                    let _ = self
                        .wait_for_events(step, deadline, CREDENTIAL_WAIT, "a chat read of the page's account")
                        .await;
                }
                answer => {
                    return answer.map_err(|reason| StepError::Failed {
                        step: step.to_owned(),
                        reason,
                    });
                }
            }
        }
    }

    /// Checks the exact selected conversation and fresh full membership immediately before an action.
    /// The action itself must recheck the selected thread atomically with its click when applicable.
    ///
    /// # Errors
    /// As [`Self::preflight_recipients`], or when selected chat evidence is missing/ambiguous/mismatched.
    pub async fn preflight_chat(&mut self, step: &str, expected_chat: &str) -> Result<(), StepError> {
        if self.selected_chat(step).await? != expected_chat {
            return Err(StepError::Failed {
                step: step.to_owned(),
                reason: "selected conversation differs from the intended lab target".to_owned(),
            });
        }
        self.preflight_recipients(step, expected_chat).await?;
        if self.selected_chat(step).await? != expected_chat {
            return Err(StepError::Failed {
                step: step.to_owned(),
                reason: "selected conversation changed during the recipient preflight".to_owned(),
            });
        }
        Ok(())
    }

    async fn selected_chat(&self, step: &str) -> Result<String, StepError> {
        let selected = self.evaluate(step, r#"/* __labSelectedConversation */ (() => {
            const buttons = [...document.querySelectorAll('[data-tid="sendMessageCommands-send"][data-track-thread-id]')]
              .filter(e => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; });
            return buttons.length === 1 ? buttons[0].getAttribute('data-track-thread-id') : null;
        })()"#).await?;
        selected
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| StepError::Failed {
                step: step.to_owned(),
                reason: "no unambiguous selected conversation identity".to_owned(),
            })
    }

    /// Gives each call to the page `timeout` to answer from now on, instead of [`CALL_TIMEOUT`].
    pub fn set_call_timeout(&mut self, timeout: Duration) {
        self.call_timeout = timeout;
    }

    /// Lets a changed page settle for the scenario's observation interval.
    #[cfg(unix)]
    pub(crate) async fn pause(&self, duration: Duration) {
        #[cfg(test)]
        let duration = self.pause_override.unwrap_or(duration);
        tokio::time::sleep(duration).await;
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn omit_observation_pauses(&mut self) {
        self.pause_override = Some(Duration::ZERO);
    }

    /// Every request seen so far.
    #[must_use]
    pub fn requests(&self) -> &[SeenRequest] {
        &self.requests
    }

    /// Navigates the tab ([`navigate_tab`], within the call timeout; a dialog asking before the page is left is
    /// accepted).
    ///
    /// # Errors
    /// When the browser refuses the navigation, or doesn't answer it within the call timeout.
    pub async fn navigate(&mut self, step: &str, url: &str) -> Result<(), StepError> {
        navigate_tab(&self.cdp, &self.page, url, self.call_timeout)
            .await
            .map_err(|error| match error {
                NavigateError::Cdp(source) => StepError::Cdp {
                    step: step.to_owned(),
                    source,
                },
                NavigateError::Timeout(waited) => StepError::Timeout {
                    step: step.to_owned(),
                    waited,
                    waiting_for: format!("the page to answer Page.navigate to {}", without_query(url)),
                },
            })
    }

    /// Waits up to `timeout` for the element `selector` finds to be in the page.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none shows up in time.
    pub async fn wait_for(&mut self, step: &str, selector: Selector<'_>, timeout: Duration) -> Result<(), StepError> {
        let expression = format!("{} !== null", selector.query());
        self.wait_until_true(step, &expression, &selector.to_string(), timeout)
            .await
    }

    /// [`Self::wait_for`] the element with `data-tid` `tid`.
    ///
    /// # Errors
    /// As [`Self::wait_for`].
    pub async fn wait_for_tid(&mut self, step: &str, tid: &str, timeout: Duration) -> Result<(), StepError> {
        self.wait_for(step, Selector::Tid(tid), timeout).await
    }

    /// Waits up to `timeout` for the page's text to hold `text`.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when it doesn't in time.
    pub async fn wait_for_text(&mut self, step: &str, text: &str, timeout: Duration) -> Result<(), StepError> {
        let expression = format!("(document.body?.innerText ?? '').includes({})", js_string(text));
        self.wait_until_true(step, &expression, &format!("the text {text:?}"), timeout)
            .await
    }

    /// Whether the page's text holds `text` now.
    ///
    /// # Errors
    /// When the page can't be evaluated.
    pub async fn shows_text(&mut self, step: &str, text: &str) -> Result<bool, StepError> {
        let expression = format!("(document.body?.innerText ?? '').includes({})", js_string(text));
        Ok(self.evaluate(step, &expression).await? == Value::Bool(true))
    }

    /// Clicks the element `selector` finds.
    ///
    /// # Errors
    /// When there is no such element.
    pub async fn click(&mut self, step: &str, selector: Selector<'_>) -> Result<(), StepError> {
        let expression = format!(
            "(() => {{ const e = {}; if (!e) return false; e.click(); return true; }})()",
            selector.query()
        );
        self.expect_true(step, &expression, &format!("no {selector} to click"))
            .await
    }

    /// Focuses the element `selector` finds and types `text` into it, as a person typing would (`Input.insertText`).
    ///
    /// # Errors
    /// When there is no such element, or the browser refuses the input.
    pub async fn type_into(&mut self, step: &str, selector: Selector<'_>, text: &str) -> Result<(), StepError> {
        let expression = format!(
            "(() => {{ const e = {}; if (!e) return false; e.focus(); return true; }})()",
            selector.query()
        );
        self.expect_true(step, &expression, &format!("no {selector} to type into"))
            .await?;
        self.insert_text(step, text).await
    }

    /// Types `text` into whatever has the focus, as a person typing would (`Input.insertText`): for an editor that
    /// rebuilds its element as it is focused, so that no selector found beforehand still finds it.
    ///
    /// # Errors
    /// When the browser refuses the input.
    pub async fn insert_text(&mut self, step: &str, text: &str) -> Result<(), StepError> {
        self.call(step, "Input.insertText", json!({"text": text}))
            .await
            .map(drop)
    }

    /// Presses Enter in the focused element.
    ///
    /// # Errors
    /// When the browser refuses the input.
    pub async fn press_enter(&mut self, step: &str) -> Result<(), StepError> {
        self.press(step, Key::ENTER).await
    }

    /// Presses `key` (with its modifiers) in the focused element, as a keyboard would: down, then up.
    ///
    /// # Errors
    /// When the browser refuses the input.
    pub async fn press(&mut self, step: &str, key: Key) -> Result<(), StepError> {
        for kind in ["keyDown", "keyUp"] {
            let mut params = json!({
                "type": kind,
                "key": key.key,
                "code": key.code,
                "windowsVirtualKeyCode": key.virtual_key,
                "modifiers": key.modifiers,
            });
            if let (Some(text), "keyDown") = (key.text, kind) {
                params["text"] = json!(text);
            }
            self.call(step, "Input.dispatchKeyEvent", params).await?;
        }
        Ok(())
    }

    /// Moves the mouse onto the middle of the element `selector` finds, scrolled into view first, as a person pointing
    /// at it would: what the page shows only under the pointer (a message's action bar) shows.
    ///
    /// # Errors
    /// When there is no such element, or the browser refuses the input.
    pub async fn hover(&mut self, step: &str, selector: Selector<'_>) -> Result<(), StepError> {
        let (x, y) = self.centre(step, selector).await?;
        let params = json!({"type": "mouseMoved", "x": x, "y": y, "button": "none"});
        self.call(step, "Input.dispatchMouseEvent", params).await.map(drop)
    }

    /// Clicks the element `selector` finds as a pointer would: moved over its middle, pressed and let go.
    ///
    /// # Errors
    /// When there is no such element, or the browser refuses the input.
    pub async fn pointer_click(&mut self, step: &str, selector: Selector<'_>) -> Result<(), StepError> {
        let (x, y) = self.centre(step, selector).await?;
        for (kind, button, clicks) in [
            ("mouseMoved", "none", 0),
            ("mousePressed", "left", 1),
            ("mouseReleased", "left", 1),
        ] {
            let params = json!({"type": kind, "x": x, "y": y, "button": button, "clickCount": clicks});
            self.call(step, "Input.dispatchMouseEvent", params).await?;
        }
        Ok(())
    }

    /// Clicks the viewport point (`x`, `y`) as a pointer would. It reaches what a page selector can't, such as a
    /// cross-origin frame's controls (Teams' Workflows dialog).
    ///
    /// # Errors
    /// When the browser refuses the input.
    pub async fn pointer_click_at(&mut self, step: &str, x: f64, y: f64) -> Result<(), StepError> {
        for (kind, button, clicks) in [
            ("mouseMoved", "none", 0),
            ("mousePressed", "left", 1),
            ("mouseReleased", "left", 1),
        ] {
            let params = json!({"type": kind, "x": x, "y": y, "button": button, "clickCount": clicks});
            self.call(step, "Input.dispatchMouseEvent", params).await?;
        }
        Ok(())
    }

    /// Turns the mouse wheel by `dy` pixels over the viewport point (`x`, `y`).
    ///
    /// # Errors
    /// When the browser refuses the input.
    pub async fn wheel_at(&mut self, step: &str, x: f64, y: f64, dy: f64) -> Result<(), StepError> {
        let params = json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": 0, "deltaY": dy});
        self.call(step, "Input.dispatchMouseEvent", params).await.map(drop)
    }

    /// The middle of the element `selector` finds, in the viewport, once it is scrolled into view.
    async fn centre(&self, step: &str, selector: Selector<'_>) -> Result<(f64, f64), StepError> {
        let expression = format!(
            "(() => {{ const e = {}; if (!e) return null; e.scrollIntoView({{block: 'center'}}); \
             const r = e.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; }})()",
            selector.query()
        );
        let value = self.evaluate(step, &expression).await?;
        match (value[0].as_f64(), value[1].as_f64()) {
            (Some(x), Some(y)) => Ok((x, y)),
            _ => Err(StepError::Failed {
                step: step.to_owned(),
                reason: format!("no {selector} to point at"),
            }),
        }
    }

    /// Passes over every request seen so far: the next request wait matches only a request sent from now on.
    pub fn skip_seen(&mut self) {
        self.drain();
        self.cursor = self.requests.len();
    }

    /// Waits up to `timeout` for the first request matching `pattern`, sent since the driver began or since the request
    /// the last request wait returned (or [`Self::skip_seen`]), to be answered or to fail, and gives it.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none is sent and answered in time, [`StepError::Closed`] when the connection goes.
    pub async fn wait_for_answer(
        &mut self,
        step: &str,
        pattern: &RequestPattern,
        timeout: Duration,
    ) -> Result<SeenRequest, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let waiting_for = format!("{pattern} to be answered");
        loop {
            self.drain();
            let found = self.requests[self.cursor..]
                .iter()
                .position(|request| pattern.matches_request(request));
            if let Some(index) = found.map(|offset| self.cursor + offset) {
                let seen = &self.requests[index];
                if seen.status.is_some() || seen.failed.is_some() {
                    self.cursor = index + 1;
                    return Ok(seen.clone());
                }
            }
            self.wait_for_events(step, deadline, timeout, &waiting_for).await?;
        }
    }

    /// Waits up to `timeout` for a request matching `pattern`, sent since the driver began or since the request the last
    /// such wait returned.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none comes in time, [`StepError::Closed`] when the connection goes.
    pub async fn wait_for_request(
        &mut self,
        step: &str,
        pattern: &RequestPattern,
        timeout: Duration,
    ) -> Result<SeenRequest, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.drain();
            let found = self.requests[self.cursor..]
                .iter()
                .position(|request| pattern.matches_request(request));
            if let Some(offset) = found {
                let index = self.cursor + offset;
                self.cursor = index + 1;
                return Ok(self.requests[index].clone());
            }
            self.wait_for_events(step, deadline, timeout, &pattern.to_string())
                .await?;
        }
    }

    /// Waits up to `timeout` for a request matching `pattern` that failed, sent since the driver began or since the
    /// request the last request wait returned; requests that match but didn't fail are passed over. One that got a
    /// response didn't fail, even when a failure follows the response (as for [`Self::wait_for_failure`]): a request's
    /// response comes in before its failure, so one seen failed without a response never got one.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none fails in time, [`StepError::Closed`] when the connection goes.
    pub async fn wait_for_failed_request(
        &mut self,
        step: &str,
        pattern: &RequestPattern,
        timeout: Duration,
    ) -> Result<SeenRequest, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let waiting_for = format!("{pattern} that fails");
        loop {
            self.drain();
            let found = self.requests[self.cursor..].iter().position(|request| {
                request.status.is_none() && request.failed.is_some() && pattern.matches_request(request)
            });
            if let Some(offset) = found {
                let index = self.cursor + offset;
                self.cursor = index + 1;
                return Ok(self.requests[index].clone());
            }
            self.wait_for_events(step, deadline, timeout, &waiting_for).await?;
        }
    }

    /// Waits up to `timeout` for `request` to fail, and says why (`net::ERR_BLOCKED_BY_CLIENT` for a held request).
    ///
    /// # Errors
    /// [`StepError::Timeout`] when it hasn't failed in time, [`StepError::Failed`] when it got a response instead
    /// (even when it failed after the response, as an answered `fetch` the page drops does).
    pub async fn wait_for_failure(
        &mut self,
        step: &str,
        request: &SeenRequest,
        timeout: Duration,
    ) -> Result<String, StepError> {
        let waiting_for = format!("{} {} to fail", request.method, crate::protocol::log_url(&request.url));
        let this = |seen: &SeenRequest| seen.session == request.session && seen.request_id == request.request_id;
        self.wait_failed(step, &waiting_for, timeout, this)
            .await
            .map(|seen| seen.failed.unwrap_or_default())
    }

    /// Waits up to `timeout` for the request with the `Network` request id `request_id` (on any session) to fail, and
    /// gives it: the request a `Fetch.requestPaused` event names by its `networkId`.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when it hasn't failed in time, [`StepError::Failed`] when it got a response instead
    /// (even when it failed after the response, as an answered `fetch` the page drops does).
    pub async fn wait_for_failure_of(
        &mut self,
        step: &str,
        request_id: &str,
        timeout: Duration,
    ) -> Result<SeenRequest, StepError> {
        let waiting_for = format!("the request {request_id} to fail");
        self.wait_failed(step, &waiting_for, timeout, |seen| seen.request_id == request_id)
            .await
    }

    /// Waits up to `timeout` for the first seen request `this` picks to fail, and gives it. A request that got a
    /// response left the browser, so it didn't fail even when a failure follows the response (a page that drops an
    /// answered `fetch` before its body is read gets `net::ERR_ABORTED` after it): the response is looked at first, or
    /// the verdict would hang on whether a look came between the two events.
    async fn wait_failed(
        &mut self,
        step: &str,
        waiting_for: &str,
        timeout: Duration,
        this: impl Fn(&SeenRequest) -> bool,
    ) -> Result<SeenRequest, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.drain();
            if let Some(seen) = self.requests.iter().find(|seen| this(seen)) {
                if let Some(status) = seen.status {
                    return Err(StepError::Failed {
                        step: step.to_owned(),
                        reason: format!(
                            "{} {} got a response ({status}) instead of failing",
                            seen.method,
                            crate::protocol::log_url(&seen.url)
                        ),
                    });
                }
                if seen.failed.is_some() {
                    return Ok(seen.clone());
                }
            }
            self.wait_for_events(step, deadline, timeout, waiting_for).await?;
        }
    }

    /// Reads every event that has come in and gives how many requests have been seen: a mark for
    /// [`Self::wait_for_seen`], which then passes over every request seen before it (an earlier document's).
    pub fn seen_so_far(&mut self) -> usize {
        self.drain();
        self.requests.len()
    }

    /// Waits up to `timeout` for a seen request, the `since`th or a later one, that `this` picks, and gives the first
    /// one; `waiting_for` says what for, in the error. It looks at every request from `since` on, whatever the request
    /// waits matched, and leaves their place alone: a wait for a request's answer, say, after a wait for a later one.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none is picked in time, [`StepError::Closed`] when the connection goes.
    pub async fn wait_for_seen(
        &mut self,
        step: &str,
        waiting_for: &str,
        since: usize,
        timeout: Duration,
        this: impl Fn(&SeenRequest) -> bool,
    ) -> Result<SeenRequest, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.drain();
            if let Some(seen) = self.requests.iter().skip(since).find(|seen| this(seen)) {
                return Ok(seen.clone());
            }
            self.wait_for_events(step, deadline, timeout, waiting_for).await?;
        }
    }

    /// `request` with its body: the one its `Network` event carried inline or, when it carried none, the one the
    /// browser still holds for it (`Network.getRequestPostData` on the session it was seen on, within the call
    /// timeout), kept for later looks. A body neither gives (a request without one, one the browser no longer holds)
    /// stays `None`. Chrome leaves some bodies out of the event (the registrar's registrations, seen live).
    ///
    /// # Errors
    /// [`StepError::Timeout`] when the browser doesn't answer in time.
    pub async fn read_body(&mut self, step: &str, request: &SeenRequest) -> Result<SeenRequest, StepError> {
        if request.body.is_some() {
            return Ok(request.clone());
        }
        let call = self.cdp.call(
            request.session.as_ref(),
            "Network.getRequestPostData",
            json!({"requestId": request.request_id}),
        );
        let answer = tokio::time::timeout(self.call_timeout, call)
            .await
            .map_err(|_| StepError::Timeout {
                step: step.to_owned(),
                waited: self.call_timeout,
                waiting_for: format!("the body of {} {}", request.method, without_query(&request.url)),
            })?;
        let mut read = request.clone();
        read.body = answer.ok().and_then(|answer| post_data_text(&answer));
        if let Some(seen) = self.seen_mut(request.session.as_ref(), &request.request_id) {
            seen.body.clone_from(&read.body);
        }
        Ok(read)
    }

    /// Reads every event that has come in.
    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.observe(&event),
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }

    /// Waits for the next event, or the deadline.
    async fn wait_for_events(
        &mut self,
        step: &str,
        deadline: tokio::time::Instant,
        timeout: Duration,
        waiting_for: &str,
    ) -> Result<(), StepError> {
        if self.closed {
            return Err(StepError::Closed { step: step.to_owned() });
        }
        match tokio::time::timeout_at(deadline, self.events.recv()).await {
            Ok(Some(event)) => {
                self.observe(&event);
                Ok(())
            }
            Ok(None) => {
                self.closed = true;
                Err(StepError::Closed { step: step.to_owned() })
            }
            Err(_) => Err(StepError::Timeout {
                step: step.to_owned(),
                waited: timeout,
                waiting_for: waiting_for.to_owned(),
            }),
        }
    }

    fn observe(&mut self, event: &Event) {
        if let Some(targets) = &self.action_targets {
            targets.observe(event);
        }
        let params = &event.params;
        if event.method == "Target.attachedToTarget"
            && let (Some(session), Some(kind)) = (params["sessionId"].as_str(), params["targetInfo"]["type"].as_str())
        {
            self.kinds.insert(SessionId::new(session), kind.to_owned());
        }
        let Some(id) = params["requestId"].as_str() else {
            return;
        };
        let session = event.session_id.clone();
        match event.method.as_str() {
            "Network.requestWillBeSent" => {
                let request = &params["request"];
                self.requests.push(SeenRequest {
                    session: session.clone(),
                    request_id: id.to_owned(),
                    method: request["method"].as_str().unwrap_or_default().to_owned(),
                    url: request["url"].as_str().unwrap_or_default().to_owned(),
                    status: None,
                    failed: None,
                    body: request["postData"].as_str().map(str::to_owned),
                    sent_ms: wall_time_ms(&params["wallTime"]),
                });
            }
            "Network.responseReceived" => {
                let status = params["response"]["status"]
                    .as_u64()
                    .and_then(|status| u16::try_from(status).ok());
                if let Some(seen) = self.seen_mut(session.as_ref(), id) {
                    seen.status = status;
                }
            }
            "Network.loadingFailed" => {
                let error = params["errorText"].as_str().unwrap_or("failed").to_owned();
                if let Some(seen) = self.seen_mut(session.as_ref(), id) {
                    seen.failed = Some(error);
                }
            }
            _ => {}
        }
    }

    fn seen_mut(&mut self, session: Option<&SessionId>, id: &str) -> Option<&mut SeenRequest> {
        self.requests
            .iter_mut()
            .rev()
            .find(|seen| seen.session.as_ref() == session && seen.request_id == id)
    }

    /// Waits up to `timeout` for `expression` to be `true` in the page; `waiting_for` says what for, in the error.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when it isn't in time, or the page doesn't answer in time.
    pub async fn wait_until_true(
        &mut self,
        step: &str,
        expression: &str,
        waiting_for: &str,
        timeout: Duration,
    ) -> Result<(), StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let timed_out = || StepError::Timeout {
            step: step.to_owned(),
            waited: timeout,
            waiting_for: waiting_for.to_owned(),
        };
        loop {
            self.drain();
            // a page that doesn't answer (a busy renderer, a modal dialog) runs out the wait too
            let value = match tokio::time::timeout_at(deadline, self.evaluate(step, expression))
                .await
                .map_err(|_| timed_out())?
            {
                Ok(value) => value,
                // the page navigated under the evaluation (a deep link reloads the web client): not there yet
                Err(error) if navigated_away(&error) => Value::Null,
                Err(error) => return Err(error),
            };
            if value == Value::Bool(true) {
                return Ok(());
            }
            if tokio::time::Instant::now() + POLL > deadline {
                return Err(timed_out());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn expect_true(&mut self, step: &str, expression: &str, otherwise: &str) -> Result<(), StepError> {
        if self.evaluate(step, expression).await? == Value::Bool(true) {
            Ok(())
        } else {
            Err(StepError::Failed {
                step: step.to_owned(),
                reason: otherwise.to_owned(),
            })
        }
    }

    /// A PNG of what the tab shows now, for a failed run's diagnosis. It shows the page as the account sees it (names,
    /// messages), so it belongs in the run's capture dir, never in the repository.
    ///
    /// # Errors
    /// When the browser doesn't take it within the call timeout, or its answer isn't base64.
    pub async fn screenshot(&self, step: &str) -> Result<Vec<u8>, StepError> {
        let result = self
            .call(step, "Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        let data = result["data"].as_str().unwrap_or_default();
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| StepError::Failed {
                step: step.to_owned(),
                reason: format!("the screenshot isn't base64: {error}"),
            })
    }

    /// Evaluates `expression` in the tab's page and gives its value. An exception in the page reads as `null`.
    pub(crate) async fn evaluate(&self, step: &str, expression: &str) -> Result<Value, StepError> {
        let result = self
            .call(
                step,
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true}),
            )
            .await?;
        Ok(result["result"]["value"].clone())
    }

    /// Sends `method` to the tab and waits up to the call timeout for its answer.
    async fn call(&self, step: &str, method: &str, params: Value) -> Result<Value, StepError> {
        let call = self.cdp.call(Some(&self.page), method, params);
        tokio::time::timeout(self.call_timeout, call)
            .await
            .map_err(|_| StepError::Timeout {
                step: step.to_owned(),
                waited: self.call_timeout,
                waiting_for: format!("the page to answer {method}"),
            })?
            .map_err(|source| StepError::Cdp {
                step: step.to_owned(),
                source,
            })
    }
}

/// The browser's answers to an evaluation whose page navigated before it ran or while it ran: the document it was for
/// is gone, and a wait asks the new one at its next look. One whose tab closed fails at that look instead.
const NAVIGATED_AWAY: [&str; 3] = [
    "Inspected target navigated or closed",
    "Execution context was destroyed",
    "Cannot find context with specified id",
];

/// Whether `error` is an evaluation refused only because its page navigated ([`NAVIGATED_AWAY`]): seen live on
/// 2026-10-20, when a deep link reloaded the web client while a wait looked for its compose box.
#[must_use]
pub fn navigated_away(error: &StepError) -> bool {
    matches!(
        error,
        StepError::Cdp { source: CdpError::Remote { error, .. }, .. }
            if NAVIGATED_AWAY.iter().any(|text| error.message.contains(text))
    )
}

/// The text of a `Network.getRequestPostData` answer (`postData`, base64 when `base64Encoded`); `None` for a body that
/// isn't UTF-8.
fn post_data_text(answer: &Value) -> Option<String> {
    let data = answer["postData"].as_str()?;
    if answer["base64Encoded"] == true {
        let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
        String::from_utf8(bytes).ok()
    } else {
        Some(data.to_owned())
    }
}

/// A `Network.requestWillBeSent`'s `wallTime` (seconds since the Unix epoch, with a fraction) in whole ms; `None` for
/// anything but a finite, non-negative number.
fn wall_time_ms(wall_time: &Value) -> Option<u64> {
    let since_epoch = Duration::try_from_secs_f64(wall_time.as_f64()?).ok()?;
    u64::try_from(since_epoch.as_millis()).ok()
}

/// A JavaScript string literal of `text`.
pub(crate) fn js_string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}
