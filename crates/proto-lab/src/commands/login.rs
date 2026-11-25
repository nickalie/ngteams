//! `proto-lab login <a|b|c>`: signs a lab account into the Teams web client in its persistent profile, headless, under
//! an observed browser session, and records the account's MRI in `lab.toml`.
//!
//! The browser opens Teams. A profile that is still signed in to Microsoft is passed straight through, and the web
//! client loads. If Microsoft shows its sign-in page instead, the sign-in is automatic, following the proven
//! `tools/lab-login/teams-lab-login.mjs`:
//! - the email and the password come from lab-automation's account file ([`crate::accounts`]) and are typed only into
//!   a field a person could type into (shown, sized, enabled);
//! - the password is sent once; a rejection stops the run;
//! - "Stay signed in?" is answered yes;
//! - a button is pressed once per page: the same click isn't repeated until the main frame has committed another
//!   document ([`Clicks`]), so a form whose post is still on its way isn't submitted twice, and the page is looked at
//!   at most every [`TICK`];
//! - a request to register an MFA method stops the run, and so does any other multi-factor prompt: one to pick a
//!   method, to approve in an app, or to enter a one-time code;
//! - when Microsoft says it knows no account for the email, the run stops;
//! - when the main frame loads Entra's `/reprocess` page again and again before anything was typed, the cookies and
//!   site data of the sign-in hosts are deleted one time and Teams is opened again; a later loop stops the run.
//!
//! The run is recorded like a scenario's, under
//! `captures/login/<ts>/` ([`LOGIN_CAPTURE`]), so the sign-in form's queries can be read for the sign-in window's own
//! rules (`teams-signin`), but without any request body ([`crate::capture::Capture::without_request_bodies`]): the
//! form's posts carry the account's password. Multi-factor sign-in (Entra's `SAS/*` requests) is never driven: no such
//! exchange has been captured, so what its requests set off (a code sent by text or call, a push to a phone, as soon as
//! the page shows the prompt) and whether verifying a method can be told apart from registering one are unknown, and
//! the run stops instead of asking for a one-time code.
//!
//! Once the web client has loaded, the account's MRI is made from the object id in the claims of a bearer token the
//! page sent for that account's UPN, and recorded in `lab.toml` ([`record_account_mri`]). An account that is signed
//! in but that Teams refuses (no Teams license; the web client then offers a trial, which is a tenant-visible change and
//! is never started here) gets its MRI recorded too, and the report says Teams refused it. Each state goes to
//! `status-<alias>.json` (mode `0600`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::accounts::{AccountsError, AccountsFile, Credentials, accounts_file};
use crate::cdp::{Cdp, CdpError, SessionId};
use crate::config::{Alias, ConfigError, LabConfig, LabLocations, MriRecord, record_account_mri};
use crate::scenario::dsl::js_string;
use crate::session::{
    LabSession, NotAGuid, SIGN_IN_HOSTS, SessionError, SessionOptions, SessionReport, SessionState, Settling, TEAMS_URL,
};

/// What a sign-in page shows, as [`probe`] finds it. A field is the selector of the one element of its kind a person
/// could type into (the probe marks it), or `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignInProbe {
    pub email: Option<String>,
    pub password: Option<String>,
    /// A one-time code.
    pub otc: Option<String>,
    /// "Use another account".
    pub other_tile: Option<String>,
    /// The "Stay signed in?" / "Don't show this again" checkbox.
    pub kmsi: Option<String>,
    pub signals: BTreeSet<Signal>,
}

/// Something else a sign-in page shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    /// A primary button (Next, Sign in, Yes).
    Next,
    /// The password error under the field.
    PasswordError,
    /// The page says the password is wrong or the account is locked.
    Rejected,
    /// The page asks to register an MFA method or more security info.
    Registration,
    /// The page asks to approve the sign-in in an app.
    Approval,
    /// The page asks to pick a verification method.
    Proofs,
    /// The page says Microsoft knows no account of that username.
    UnknownAccount,
}

impl Signal {
    const ALL: [(Self, &str); 7] = [
        (Self::Next, "next"),
        (Self::PasswordError, "passwordError"),
        (Self::Rejected, "rejected"),
        (Self::Registration, "registration"),
        (Self::Approval, "approval"),
        (Self::Proofs, "proofs"),
        (Self::UnknownAccount, "unknownAccount"),
    ];
}

impl SignInProbe {
    /// Reads what the probe script returned; anything missing or of the wrong type counts as absent.
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        let field = |name: &str| {
            value[name]
                .as_str()
                .filter(|selector| !selector.is_empty())
                .map(str::to_owned)
        };
        Self {
            email: field("email"),
            password: field("password"),
            otc: field("otc"),
            other_tile: field("otherTile"),
            kmsi: field("kmsi"),
            signals: Signal::ALL
                .iter()
                .filter(|(_, name)| value[*name].as_bool() == Some(true))
                .map(|(signal, _)| *signal)
                .collect(),
        }
    }

    #[must_use]
    pub fn shows(&self, signal: Signal) -> bool {
        self.signals.contains(&signal)
    }
}

/// What has been sent so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub email_sent: bool,
    pub password_sent: bool,
}

/// Why a sign-in stops for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignInStop {
    #[error("the password was rejected")]
    PasswordRejected,
    #[error("Microsoft knows no account of this username")]
    UnknownAccount,
    #[error("Microsoft asks to register an MFA method; not proceeding")]
    MfaRegistration,
    #[error("Microsoft asks to pick an MFA method; not proceeding")]
    MfaChoice,
    #[error("Microsoft asks to approve the sign-in in an app; not proceeding")]
    MfaApproval,
    #[error("Microsoft asks for a one-time code; not proceeding")]
    MfaCode,
}

/// The next step on a sign-in page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do yet.
    Wait,
    TypeEmail(String),
    TypePassword(String),
    ClickOtherTile(String),
    /// Tick the checkbox and press the primary button.
    AcceptKmsi(String),
    ClickNext,
    Stop(SignInStop),
}

impl Action {
    /// Whether the step presses something on the page (and so may submit a form).
    fn clicks(&self) -> bool {
        matches!(self, Self::ClickOtherTile(_) | Self::AcceptKmsi(_) | Self::ClickNext)
    }
}

/// Keeps the sign-in from pressing the same thing twice on one page. A click sends the form's post, and until the reply
/// commits the next document the page still shows what was clicked; pressing it again would submit the form a second
/// time while the first post is under way. The email and the password are kept to once each by [`Progress`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Clicks {
    /// The last click and the main-frame document ([`crate::session::SessionWatch::document`]) it was made on.
    last: Option<(Action, u64)>,
}

impl Clicks {
    /// `action` as it may be carried out on main-frame document `document`: [`Action::Wait`] for the click made last,
    /// on the same document; the action itself otherwise, a click then counting as made.
    pub fn admit(&mut self, action: Action, document: u64) -> Action {
        if !action.clicks() {
            return action;
        }
        if self
            .last
            .as_ref()
            .is_some_and(|(last, on)| *last == action && *on == document)
        {
            return Action::Wait;
        }
        self.last = Some((action.clone(), document));
        action
    }
}

/// The next step for what the page shows, in the order the proven script takes them. The password goes in once; after
/// it, a password field still showing means waiting, or a stop if the page says it was wrong. Buttons are pressed
/// only once the password went in.
#[must_use]
pub fn next_action(probe: &SignInProbe, progress: Progress) -> Action {
    if let Some(password) = &probe.password {
        if !progress.password_sent {
            return Action::TypePassword(password.clone());
        }
        if probe.shows(Signal::PasswordError) || probe.shows(Signal::Rejected) {
            return Action::Stop(SignInStop::PasswordRejected);
        }
        return Action::Wait;
    }
    if let Some(email) = &probe.email {
        // still the email page after the email went in: wait. Its "Sign-in options" tile is a `div.table` button, so
        // reading on would take it for a verification-method choice.
        if progress.email_sent {
            if probe.shows(Signal::UnknownAccount) {
                return Action::Stop(SignInStop::UnknownAccount);
            }
            return Action::Wait;
        }
        return Action::TypeEmail(email.clone());
    }
    mfa_or_confirm(probe, progress)
}

fn mfa_or_confirm(probe: &SignInProbe, progress: Progress) -> Action {
    if let Some(tile) = &probe.other_tile {
        return Action::ClickOtherTile(tile.clone());
    }
    if probe.shows(Signal::Registration) {
        return Action::Stop(SignInStop::MfaRegistration);
    }
    if probe.otc.is_some() {
        return Action::Stop(SignInStop::MfaCode);
    }
    if probe.shows(Signal::Proofs) {
        return Action::Stop(SignInStop::MfaChoice);
    }
    if probe.shows(Signal::Approval) {
        return Action::Stop(SignInStop::MfaApproval);
    }
    match (&probe.kmsi, progress.password_sent) {
        (Some(kmsi), true) => Action::AcceptKmsi(kmsi.clone()),
        (None, true) if probe.shows(Signal::Next) => Action::ClickNext,
        _ => Action::Wait,
    }
}

/// The probe: finds the one element of each kind a person could use, marks it with `data-proto-lab`, and reports the
/// marker selectors and the page's signals.
const PROBE: &str = r#"(() => {
  for (const old of document.querySelectorAll('[data-proto-lab]')) old.removeAttribute('data-proto-lab');
  const shown = (el) => {
    const style = getComputedStyle(el);
    const box = el.getBoundingClientRect();
    return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) > 0
      && box.width > 0 && box.height > 0;
  };
  const typable = (el) => shown(el) && !el.disabled && !el.readOnly
    && el.getBoundingClientRect().width >= 100 && el.getBoundingClientRect().height >= 20;
  const mark = (kind, selectors, usable) => {
    for (const selector of selectors) {
      for (const el of document.querySelectorAll(selector)) {
        if (usable(el)) { el.setAttribute('data-proto-lab', kind); return `[data-proto-lab="${kind}"]`; }
      }
    }
    return null;
  };
  const any = (selectors) => selectors.some((s) => [...document.querySelectorAll(s)].some(shown));
  const text = document.body ? document.body.innerText : '';
  return {
    email: mark('email', ['input[name="loginfmt"]', 'input[type="email"]'], typable),
    password: mark('password', ['input[name="passwd"]', 'input[type="password"]'], typable),
    otc: mark('otc', ['input[name="otc"]', '#idTxtBx_SAOTCC_OTC', 'input[autocomplete="one-time-code"]', 'input[type="tel"]'], typable),
    otherTile: mark('other-tile', ['#otherTile', '[data-test-id="otherTile"]'], shown),
    kmsi: mark('kmsi', ['#KmsiCheckboxField', 'input[name="DontShowAgain"]'], shown),
    next: any(['#idSIButton9', 'input[type="submit"]', 'button[type="submit"]']),
    passwordError: any(['#passwordError']),
    rejected: /password is incorrect|account has been locked/i.test(text),
    registration: /more information required|keep your account secure|set up your account/i.test(text),
    approval: /approve sign.in|open your authenticator|enter the number/i.test(text),
    proofs: any(['[data-value][role="button"]', 'div.table[role="button"]', '#idDiv_SAOTCS_Proofs [role="button"]']),
    unknownAccount: any(['#usernameError']) || /username may be incorrect|couldn.t find an account/i.test(text),
  };
})()"#;

/// Presses the first primary button shown.
const PRESS_NEXT: &str = r#"(() => {
  for (const selector of ['#idSIButton9', 'input[type="submit"]', 'button[type="submit"]']) {
    for (const el of document.querySelectorAll(selector)) {
      const box = el.getBoundingClientRect();
      if (box.width > 0 && box.height > 0 && getComputedStyle(el).visibility !== 'hidden') { el.click(); return true; }
    }
  }
  return false;
})()"#;

/// Why `login` failed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Accounts(#[from] AccountsError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("the sign-in page: {0}")]
    Cdp(#[from] CdpError),
    #[error("{0}")]
    SignIn(#[from] SignInStop),
    #[error("the sign-in page went away from its field before it was filled")]
    FieldGone,
    #[error("the page stopped on an unexpected host: {0}")]
    UnexpectedHost(String),
    #[error("the web client didn't load within {timeout:?} ({})", authz_text(*authz))]
    Timeout { timeout: Duration, authz: Option<u16> },
    #[error("the web client didn't load within {timeout:?}: Microsoft's sign-in answered {answer} ({})", authz_text(*authz))]
    SignInAnswer {
        timeout: Duration,
        answer: String,
        authz: Option<u16>,
    },
    #[error("Microsoft's sign-in page reloads its reprocess page again and again")]
    ReprocessLoop,
    #[error("the web client loaded, but no request showed the account's identity")]
    NoIdentity,
    #[error("the account's {0}")]
    BadIdentity(#[from] NotAGuid),
    #[error("can't write the status file: {0}")]
    Status(#[source] std::io::Error),
    #[error("capture setup failed: {0}")]
    CaptureFailed(String),
}

fn authz_text(authz: Option<u16>) -> String {
    authz.map_or_else(
        || "no authz answer".to_owned(),
        |status| format!("authz answered {status}"),
    )
}

/// Reads the page: what a person would see to act on.
///
/// # Errors
/// When the page can't be evaluated.
pub async fn probe(cdp: &Cdp, page: &SessionId) -> Result<SignInProbe, CdpError> {
    let result = evaluate(cdp, page, PROBE).await?;
    Ok(SignInProbe::from_value(&result))
}

async fn evaluate(cdp: &Cdp, page: &SessionId, expression: &str) -> Result<Value, CdpError> {
    let params = json!({"expression": expression, "returnByValue": true});
    let result = cdp.call(Some(page), "Runtime.evaluate", params).await?;
    Ok(result["result"]["value"].clone())
}

/// Carries out one step on the page. [`Action::Wait`] and [`Action::Stop`] do nothing here.
///
/// # Errors
/// When the page can't be driven, or a field it named is gone.
pub async fn perform(
    cdp: &Cdp,
    page: &SessionId,
    action: &Action,
    credentials: &Credentials,
    progress: &mut Progress,
) -> Result<(), LoginError> {
    match action {
        Action::TypeEmail(field) => {
            type_into(cdp, page, field, credentials.email()).await?;
            press_next(cdp, page).await?;
            progress.email_sent = true;
        }
        Action::TypePassword(field) => {
            // counted as sent before anything is typed: a failure halfway must not lead to a second try
            progress.password_sent = true;
            type_into(cdp, page, field, credentials.password().expose()).await?;
            press_next(cdp, page).await?;
        }
        Action::ClickOtherTile(tile) => click(cdp, page, tile).await?,
        Action::AcceptKmsi(checkbox) => {
            let tick = format!(
                "(() => {{ const box = document.querySelector({}); if (!box) return false; if (!box.checked) box.click(); return true; }})()",
                js_string(checkbox)
            );
            if evaluate(cdp, page, &tick).await? != json!(true) {
                return Err(LoginError::FieldGone);
            }
            press_next(cdp, page).await?;
        }
        Action::ClickNext => press_next(cdp, page).await?,
        Action::Wait | Action::Stop(_) => {}
    }
    Ok(())
}

/// Focuses the field, selects what's in it, and types `text` a character at a time, as a person would.
async fn type_into(cdp: &Cdp, page: &SessionId, field: &str, text: &str) -> Result<(), LoginError> {
    let focus = format!(
        "(() => {{ const el = document.querySelector({}); if (!el) return false; el.focus(); el.select(); return true; }})()",
        js_string(field)
    );
    if evaluate(cdp, page, &focus).await? != json!(true) {
        return Err(LoginError::FieldGone);
    }
    let mut buffer = [0u8; 4];
    for (index, character) in text.chars().enumerate() {
        let params = json!({"text": character.encode_utf8(&mut buffer)});
        cdp.call(Some(page), "Input.insertText", params).await?;
        tokio::time::sleep(keystroke_gap(index)).await;
    }
    Ok(())
}

/// 40–120 ms between keystrokes, varied.
fn keystroke_gap(index: usize) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |now| now.subsec_nanos());
    let spread = (u64::from(nanos) / 7 + index as u64 * 37) % 80;
    Duration::from_millis(40 + spread)
}

async fn click(cdp: &Cdp, page: &SessionId, selector: &str) -> Result<(), LoginError> {
    let script = format!(
        "(() => {{ const el = document.querySelector({}); if (!el) return false; el.click(); return true; }})()",
        js_string(selector)
    );
    if evaluate(cdp, page, &script).await? != json!(true) {
        return Err(LoginError::FieldGone);
    }
    Ok(())
}

async fn press_next(cdp: &Cdp, page: &SessionId) -> Result<(), LoginError> {
    evaluate(cdp, page, PRESS_NEXT).await?;
    Ok(())
}

/// `status-<alias>.json` in the lab's data dir: the login's current state, for whoever runs it.
pub struct StatusFile {
    path: PathBuf,
    alias: Alias,
}

impl StatusFile {
    #[must_use]
    pub fn new(data_dir: &Path, alias: Alias) -> Self {
        Self {
            path: data_dir.join(format!("status-{alias}.json")),
            alias,
        }
    }

    /// Replaces the file with `{alias, state, at_unix_ms, ...extra}`, readable by the owner only.
    ///
    /// # Errors
    /// When the file can't be written.
    pub fn write(&self, state: &str, extra: &Value) -> std::io::Result<()> {
        use std::io::Write as _;
        #[cfg(unix)]
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |now| u64::try_from(now.as_millis()).unwrap_or(u64::MAX));
        let mut record = json!({"alias": self.alias.as_str(), "state": state, "at_unix_ms": at});
        if let (Some(record), Some(extra)) = (record.as_object_mut(), extra.as_object()) {
            record.extend(extra.iter().map(|(key, value)| (key.clone(), value.clone())));
        }
        #[cfg(unix)]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)?;
        // a file left by another tool may be wider
        #[cfg(unix)]
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        #[cfg(windows)]
        let mut file = crate::private::replace(&self.path)?;
        file.write_all(format!("{record:#}\n").as_bytes())
    }
}

/// How a login runs.
#[derive(Clone, Copy, Debug)]
pub struct LoginOptions {
    /// How long the whole sign-in and load may take.
    pub timeout: Duration,
}

/// The scenario name `login` records its runs under (`captures/login/<ts>/` in the lab's data dir).
pub const LOGIN_CAPTURE: &str = "login";

/// How `login` runs its browser: headless, recorded under
/// [`LOGIN_CAPTURE`] with no request body.
#[must_use]
pub fn session_options() -> SessionOptions {
    SessionOptions {
        capture: Some(LOGIN_CAPTURE.to_owned()),
        capture_request_bodies: false,
        ..SessionOptions::default()
    }
}

/// What a login did.
#[derive(Debug)]
pub struct LoginReport {
    /// It had to sign in (the profile's session had expired).
    pub signed_in: bool,
    /// `None` when the web client loaded; the status of Teams' refused authz exchange when the account is signed in
    /// but Teams won't serve it (no Teams license). Its MRI is recorded either way.
    pub refused: Option<u16>,
    pub mri: MriRecord,
    pub browser: SessionReport,
    /// The run's capture.
    pub capture: Option<PathBuf>,
}

/// What the browser part of a login found.
struct BrowserOutcome {
    signed_in: bool,
    refused: Option<u16>,
    mri: String,
}

/// Signs `alias` in and records its MRI (see the module docs).
///
/// # Errors
/// When the lab config or account file is refused, the browser cannot launch, sign-in stops,
/// the web client does not load in time, the identity is not shown, the MRI cannot be recorded, or capture setup fails. The status file says `failed` then.
pub async fn login(alias: Alias, options: LoginOptions) -> Result<LoginReport, LoginError> {
    let locations = LabLocations::from_env()?;
    let config = LabConfig::load(&locations)?;
    let paths = config.paths().clone();
    let status = StatusFile::new(paths.data_dir(), alias);
    let outcome = async {
        let upn = config.account(alias).upn.clone();
        let accounts = AccountsFile::load(&accounts_file(&locations), |name| std::env::var(name).ok())?;
        let credentials = accounts.for_alias(alias, &upn)?;
        status.write("starting", &json!({})).map_err(LoginError::Status)?;
        let session = LabSession::open_with(&paths, alias, session_options()).await?;
        let run = Run {
            upn: &upn,
            credentials,
            status: &status,
            options,
        };
        run.finish(session, &locations, alias).await
    }
    .await;
    finish_status(&status, outcome)
}

fn finish_status(status: &StatusFile, outcome: Result<LoginReport, LoginError>) -> Result<LoginReport, LoginError> {
    let written = match &outcome {
        Ok(report) => match report.refused {
            None => status.write("done", &json!({"browser": report.browser.summary()})),
            Some(code) => status.write(
                "teams-refused",
                &json!({"authz_status": code, "browser": report.browser.summary()}),
            ),
        },
        Err(error) => status.write("failed", &json!({"error": error.to_string()})),
    };
    let report = outcome?;
    written.map_err(LoginError::Status)?;
    Ok(report)
}

/// One login's moving parts.
struct Run<'a> {
    upn: &'a str,
    credentials: &'a Credentials,
    status: &'a StatusFile,
    options: LoginOptions,
}

/// How often the sign-in page is looked at, at most: a click's post makes events at once, and the page is only looked
/// at again once this has passed (a click isn't repeated on one document anyway: [`Clicks`]).
pub const TICK: Duration = Duration::from_millis(700);
/// How long the identity may take to show once the web client has loaded.
const IDENTITY_WAIT: Duration = Duration::from_secs(30);
/// How many `/reprocess` commits in a row make a loop. A stale Entra session reloads that page about once a second with
/// no field to fill; a normal sign-in passes it at most once.
pub const REPROCESS_LOOP: u32 = 5;
/// How long a blank page stands before the sign-in data is deleted, for the loop's last replies to land.
const SETTLE_BLANK: Duration = Duration::from_secs(2);

/// Where the sign-in is: what was sent, the last click, and when the page may be looked at next.
#[derive(Default)]
struct SignIn {
    progress: Progress,
    clicks: Clicks,
    next_look: Option<tokio::time::Instant>,
    /// The main-frame document on which the sign-in cookies were deleted, which is done once only.
    cookies_cleared_on: Option<u64>,
}

impl Run<'_> {
    async fn finish(
        &self,
        mut session: LabSession,
        locations: &LabLocations,
        alias: Alias,
    ) -> Result<LoginReport, LoginError> {
        let signed = self.drive(&mut session).await;
        // the browser goes down whatever happened, before anything else is reported
        let closed = session.close_recorded().await;
        let signed = signed?;
        let (browser, capture) = closed?;
        if browser.failed() {
            return Err(LoginError::CaptureFailed(browser.summary()));
        }
        let mri = record_account_mri(locations, alias, &signed.mri)?;
        Ok(LoginReport {
            signed_in: signed.signed_in,
            refused: signed.refused,
            mri,
            browser,
            capture,
        })
    }

    /// Opens Teams and signs in where asked, until the web client loads (or Teams refuses the signed-in account); then
    /// waits for the identity.
    async fn drive(&self, session: &mut LabSession) -> Result<BrowserOutcome, LoginError> {
        session.navigate(TEAMS_URL).await?;
        let mut sign_in = SignIn::default();
        let refused = self.until_loaded(session, &mut sign_in).await?;
        let signed_in = sign_in.progress.email_sent || sign_in.progress.password_sent;
        let state = if refused.is_some() {
            "signed-in-teams-refused"
        } else {
            "loaded"
        };
        self.status
            .write(state, &json!({"signed_in": signed_in}))
            .map_err(LoginError::Status)?;
        let identity_deadline = tokio::time::Instant::now() + IDENTITY_WAIT;
        while session.watch().identity_for(self.upn).is_none() && tokio::time::Instant::now() < identity_deadline {
            if !session.pump(TICK).await {
                return Err(LoginError::CaptureFailed(session.report().summary()));
            }
        }
        let identity = session.watch().identity_for(self.upn).ok_or(LoginError::NoIdentity)?;
        Ok(BrowserOutcome {
            signed_in,
            refused,
            mri: identity.mri()?,
        })
    }

    /// Signs in where asked until the web client loads (`None`) or Teams refuses the account (its authz status),
    /// a refusal counting once it has stood for [`crate::session::REFUSAL_GRACE`] ([`Settling`]). A lost capture connection fails the run.
    async fn until_loaded(&self, session: &mut LabSession, sign_in: &mut SignIn) -> Result<Option<u16>, LoginError> {
        let deadline = tokio::time::Instant::now() + self.options.timeout;
        let mut settling = Settling::default();
        loop {
            if tokio::time::Instant::now() >= deadline {
                if let Some(answer) = session.watch().sign_in_error() {
                    return Err(LoginError::SignInAnswer {
                        timeout: self.options.timeout,
                        answer: answer.to_owned(),
                        authz: session.watch().last_authz_status(),
                    });
                }
                return Err(LoginError::Timeout {
                    timeout: self.options.timeout,
                    authz: session.watch().last_authz_status(),
                });
            }
            if !session.pump(TICK).await {
                return Err(LoginError::CaptureFailed(session.report().summary()));
            }
            if in_reprocess_loop(session, sign_in) {
                self.restart_sign_in(session, sign_in).await?;
                continue;
            }
            match settling.judge(session.watch().state(), tokio::time::Instant::now()) {
                SessionState::Loaded => return Ok(None),
                SessionState::Refused(status) => return Ok(Some(status)),
                SessionState::Loading => {}
                SessionState::Elsewhere(host) => return Err(LoginError::UnexpectedHost(host)),
                SessionState::SignInShown => self.sign_in_step(session, sign_in).await?,
            }
        }
    }

    /// Leaves a reprocess loop: once, before anything was typed, the profile's sign-in data is deleted and Teams is
    /// opened again, so that Microsoft asks for the email and the password. Otherwise the loop stops the run.
    async fn restart_sign_in(&self, session: &mut LabSession, sign_in: &mut SignIn) -> Result<(), LoginError> {
        let typed = sign_in.progress.email_sent || sign_in.progress.password_sent;
        if typed || sign_in.cookies_cleared_on.is_some() {
            return Err(LoginError::ReprocessLoop);
        }
        // the loop's replies in flight set the session cookies again: they must end before the cookies go
        session.navigate("about:blank").await?;
        let quiet = tokio::time::Instant::now() + SETTLE_BLANK;
        while tokio::time::Instant::now() < quiet {
            if !session.pump(TICK.min(SETTLE_BLANK)).await {
                return Err(LoginError::CaptureFailed(session.report().summary()));
            }
        }
        sign_in.cookies_cleared_on = Some(session.watch().document());
        let deleted = delete_sign_in_data(session.cdp(), session.page()).await?;
        self.status
            .write("sign-in-cookies-deleted", &json!({"cookies": deleted}))
            .map_err(LoginError::Status)?;
        session.navigate(TEAMS_URL).await?;
        Ok(())
    }

    /// One look at the sign-in page and the step it calls for, unless the page was looked at less than [`TICK`] ago.
    async fn sign_in_step(&self, session: &LabSession, sign_in: &mut SignIn) -> Result<(), LoginError> {
        let now = tokio::time::Instant::now();
        if sign_in.next_look.is_some_and(|next| now < next) {
            return Ok(());
        }
        sign_in.next_look = Some(now + TICK);
        let (cdp, page) = (session.cdp(), session.page());
        let found = probe(cdp, page).await?;
        let action = sign_in
            .clicks
            .admit(next_action(&found, sign_in.progress), session.watch().document());
        let before = sign_in.progress;
        if let Action::Stop(stop) = action {
            return Err(stop.into());
        }
        perform(cdp, page, &action, self.credentials, &mut sign_in.progress).await?;
        let state = match action {
            Action::TypeEmail(_) if !before.email_sent => "email-submitted",
            Action::TypePassword(_) => "password-submitted",
            Action::AcceptKmsi(_) => "kmsi-accepted",
            _ => return Ok(()),
        };
        self.status.write(state, &json!({})).map_err(LoginError::Status)
    }
}

/// Whether the main frame has committed only `/reprocess` for the last [`REPROCESS_LOOP`] documents, all after the
/// sign-in cookies were deleted, if they were.
fn in_reprocess_loop(session: &LabSession, sign_in: &SignIn) -> bool {
    let watch = session.watch();
    watch.reprocess_run() >= REPROCESS_LOOP
        && sign_in
            .cookies_cleared_on
            .is_none_or(|cleared| watch.document() >= cleared + u64::from(REPROCESS_LOOP))
}

/// Deletes the cookies and the other site data of Microsoft's sign-in hosts ([`SIGN_IN_HOSTS`]) from the profile and
/// returns how many cookies went. Cookies of a parent domain that other sites share stay.
async fn delete_sign_in_data(cdp: &Cdp, page: &SessionId) -> Result<usize, CdpError> {
    let urls: Vec<String> = SIGN_IN_HOSTS.iter().map(|host| format!("https://{host}/")).collect();
    let found = cdp
        .call(Some(page), "Network.getCookies", json!({"urls": urls}))
        .await?;
    let mut deleted = 0;
    for cookie in found["cookies"].as_array().into_iter().flatten() {
        let (Some(name), Some(domain), Some(path)) = (
            cookie["name"].as_str(),
            cookie["domain"].as_str(),
            cookie["path"].as_str(),
        ) else {
            continue;
        };
        if !is_sign_in_domain(domain) {
            continue;
        }
        let target = json!({"name": name, "domain": domain, "path": path});
        cdp.call(Some(page), "Network.deleteCookies", target).await?;
        deleted += 1;
    }
    // after the cookies: this clear removes the host-only ones too, which would leave them out of the count
    for host in SIGN_IN_HOSTS {
        let origin = json!({"origin": format!("https://{host}"), "storageTypes": "all"});
        cdp.call(Some(page), "Storage.clearDataForOrigin", origin).await?;
    }
    Ok(deleted)
}

/// A cookie domain of one of [`SIGN_IN_HOSTS`] or below it.
fn is_sign_in_domain(domain: &str) -> bool {
    let domain = domain.trim_start_matches('.');
    SIGN_IN_HOSTS
        .iter()
        .any(|host| domain == *host || domain.strip_suffix(host).is_some_and(|rest| rest.ends_with('.')))
}

#[cfg(test)]
#[path = "../../tests/support/commands/login.rs"]
mod tests;

/// Windows lab authorization uses the same one-submission login driver directly on Microsoft's native authorize
/// page. It never loads the Teams web client (and therefore cannot open an unchecked chat before reach is audited).
///
/// # Errors
/// Browser setup/cleanup, sign-in rejection/MFA, an unexpected host or the authorization deadline fails.
#[cfg(windows)]
pub async fn authorization_profile(
    paths: &crate::config::LabPaths,
    alias: Alias,
    authorization: &teams_auth::token::Authorization,
    credentials: &Credentials,
    timeout: Duration,
) -> Result<(teams_auth::secrets::Secret, SessionReport), LoginError> {
    let entra = teams_auth::token::EntraConfig::teams_native();
    let (catch, mut caught) =
        crate::redirect_catch::RedirectCatch::new(&entra.authorize_endpoint(), entra.redirect_uri())
            .map_err(|_| LoginError::CaptureFailed("cannot create native redirect catcher".into()))?;
    let session = LabSession::open_catching(paths, alias, catch).await?;
    let status = StatusFile::new(paths.data_dir(), alias);
    authorization_session(session, authorization, credentials, &status, timeout, &mut caught).await
}

#[cfg(any(windows, test))]
async fn authorization_session(
    mut session: LabSession,
    authorization: &teams_auth::token::Authorization,
    credentials: &Credentials,
    status: &StatusFile,
    timeout: Duration,
    caught: &mut tokio::sync::mpsc::UnboundedReceiver<teams_auth::secrets::Secret>,
) -> Result<(teams_auth::secrets::Secret, SessionReport), LoginError> {
    let run = Run {
        upn: credentials.email(),
        credentials,
        status,
        options: LoginOptions { timeout },
    };
    let outcome = tokio::time::timeout(timeout, async {
        status.write("starting", &json!({})).map_err(LoginError::Status)?;
        session.navigate(authorization.url()).await?;
        let mut state = SignIn::default();
        loop {
            if let Ok(redirect) = caught.try_recv() {
                return Ok(redirect);
            }
            match session.watch().state() {
                SessionState::SignInShown => run.sign_in_step(&session, &mut state).await?,
                SessionState::Elsewhere(host) => return Err(LoginError::UnexpectedHost(host)),
                _ => {}
            }
            if !session.pump(TICK).await {
                return Err(LoginError::CaptureFailed("authorization browser disconnected".into()));
            }
        }
    })
    .await
    .unwrap_or(Err(LoginError::Timeout { timeout, authz: None }));
    let closed = session.close().await;
    // The status file never carries a third-party browser error or a redirect/code.
    status
        .write(if outcome.is_ok() { "authorized" } else { "failed" }, &json!({}))
        .map_err(LoginError::Status)?;
    let redirect = outcome?;
    let browser = closed?;
    if browser.failed() {
        return Err(LoginError::CaptureFailed(browser.summary()));
    }
    Ok((redirect, browser))
}
