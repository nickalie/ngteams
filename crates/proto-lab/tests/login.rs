//! `proto-lab login` and `status`: the lab accounts' credentials file, telling from recorded (synthetic) CDP events
//! whether the web client loaded or the session has expired, the identity the page signs in as, the sign-in steps, and
//! the status file. With Chrome installed, the sign-in driver runs on a local stand-in for Microsoft's
//! sign-in page, in a headless browser on a throwaway profile with the fence armed.
//!
//! Nothing here touches a lab profile, the real accounts file or the network beyond 127.0.0.1.

mod common;
#[path = "../../teams-platform/tests/support/privacy.rs"]
mod privacy;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use proto_lab::accounts::{AccountsError, AccountsFile};
use proto_lab::arming::ArmedBrowser;
use proto_lab::browser::{Browser, Display, LaunchOptions};
use proto_lab::capture_hold::CaptureHold;
use proto_lab::cdp::{Event, SessionId};
use proto_lab::commands::login::{
    Action, Clicks, LOGIN_CAPTURE, Progress, SignInProbe, SignInStop, StatusFile, next_action, perform, probe,
    session_options,
};
#[cfg(unix)]
use proto_lab::commands::status::{AccountStatus, account_status};
use proto_lab::config::Alias;
use proto_lab::session::{Identity, REFUSAL_GRACE, SessionState, SessionWatch, Settling};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The password every test file holds; no output may contain it.
const SECRET: &str = "Planted-Pa55word!";
const UPN_A: &str = "bot-a@lab.example.com";

// --- the accounts file ---

fn no_env(_: &str) -> Option<String> {
    None
}

fn accounts_json(password_field: &str) -> String {
    format!(
        r#"{{"accounts": [
            {{"profile": "a", "email": "{UPN_A}", {password_field}}},
            {{"profile": "b", "email": "bot-b@lab.example.com", "password": "other-b"}},
            {{"profile": "c", "email": "bot-c@lab.example.com", "password": "other-c"}}
        ], "azureDevOps": {{"organization": "x"}}}}"#
    )
}

#[cfg(unix)]
fn write_file(dir: &Path, text: &str, mode: u32) -> TestResult<std::path::PathBuf> {
    let file = dir.join("accounts.json");
    std::fs::write(&file, text)?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode))?;
    Ok(file)
}

/// Every way an error or a value can be shown.
fn shown(error: &AccountsError) -> String {
    format!("{error} {error:?}")
}

#[test]
fn a_literal_password_is_read_for_the_alias() -> TestResult {
    let accounts = AccountsFile::parse(&accounts_json(&format!(r#""password": "{SECRET}""#)), no_env)?;
    let credentials = accounts.for_alias(Alias::A, UPN_A)?;
    assert_eq!(credentials.email(), UPN_A);
    assert_eq!(credentials.password().expose(), SECRET);
    // the claude- prefix is dropped, as loadAccounts does
    assert_eq!(
        accounts
            .for_alias(Alias::B, "bot-b@lab.example.com")?
            .password()
            .expose(),
        "other-b"
    );
    Ok(())
}

#[test]
fn a_password_env_names_the_variable_holding_it() -> TestResult {
    let text = accounts_json(r#""passwordEnv": "LAB_PW_A", "password": "ignored-when-env-is-set""#);
    let env = |name: &str| (name == "LAB_PW_A").then(|| SECRET.to_owned());
    let accounts = AccountsFile::parse(&text, env)?;
    assert_eq!(accounts.for_alias(Alias::A, UPN_A)?.password().expose(), SECRET);
    // an unset (or empty) variable is no password, and the error names the variable, not a value
    let error = AccountsFile::parse(&text, no_env)
        .err()
        .ok_or("parsed without the variable")?;
    assert!(matches!(error, AccountsError::NoPassword { index: 1, ref from } if from.contains("LAB_PW_A")));
    let empty = AccountsFile::parse(&text, |_| Some(String::new()))
        .err()
        .ok_or("parsed an empty variable")?;
    assert!(matches!(empty, AccountsError::NoPassword { index: 1, .. }));
    Ok(())
}

#[test]
fn a_bare_array_is_accepted_too() -> TestResult {
    let text = format!(r#"[{{"profile": "a", "email": "{UPN_A}", "password": "{SECRET}"}}]"#);
    let accounts = AccountsFile::parse(&text, no_env)?;
    assert_eq!(accounts.for_alias(Alias::A, UPN_A)?.email(), UPN_A);
    let missing = accounts
        .for_alias(Alias::B, "bot-b@lab.example.com")
        .err()
        .ok_or("found b")?;
    assert!(matches!(missing, AccountsError::NotListed(Alias::B)));
    Ok(())
}

#[test]
fn the_email_has_to_be_the_lab_accounts_upn() -> TestResult {
    let accounts = AccountsFile::parse(&accounts_json(&format!(r#""password": "{SECRET}""#)), no_env)?;
    // case doesn't matter
    accounts.for_alias(Alias::A, "BOT-A@lab.example.com")?;
    let error = accounts
        .for_alias(Alias::A, "someone@lab.example.com")
        .err()
        .ok_or("matched")?;
    assert!(matches!(error, AccountsError::EmailMismatch(Alias::A)));
    assert!(!shown(&error).contains(SECRET));
    Ok(())
}

#[test]
#[cfg(unix)]
fn the_file_must_not_be_readable_by_group_or_others() -> TestResult {
    let dir = tempfile::tempdir()?;
    let text = accounts_json(&format!(r#""password": "{SECRET}""#));
    for mode in [0o644, 0o640, 0o604, 0o660] {
        let file = write_file(dir.path(), &text, mode)?;
        let error = AccountsFile::load(&file, no_env)
            .err()
            .ok_or("loaded a readable file")?;
        assert!(
            matches!(error, AccountsError::Mode { mode: found, .. } if found == mode),
            "{mode:o}"
        );
        assert!(shown(&error).contains("accounts.json"));
        assert!(!shown(&error).contains(SECRET));
    }
    for mode in [0o600, 0o400] {
        let file = write_file(dir.path(), &text, mode)?;
        let accounts = AccountsFile::load(&file, no_env)?;
        assert_eq!(accounts.for_alias(Alias::A, UPN_A)?.password().expose(), SECRET);
    }
    let missing = AccountsFile::load(&dir.path().join("nope.json"), no_env)
        .err()
        .ok_or("loaded nothing")?;
    assert!(matches!(missing, AccountsError::Read { .. }));
    Ok(())
}

#[test]
#[cfg(windows)]
fn the_account_file_requires_a_private_native_acl() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("accounts.json");
    std::fs::write(&file, accounts_json(&format!(r#""password": "{SECRET}""#)))?;
    teams_platform::private::restrict_file(&file)?;
    privacy::assert_private(&file, false);
    let accounts = AccountsFile::load(&file, no_env)?;
    assert_eq!(accounts.for_alias(Alias::A, UPN_A)?.password().expose(), SECRET);
    // Change only this synthetic file's DACL. Set-Acl can also request SACL privileges unavailable to the normal
    // Limited Interactive console token; broadening the reader grant requires no such privilege.
    let changed = std::process::Command::new("icacls.exe")
        .arg(&file)
        .args(["/grant", "*S-1-5-11:(R)"])
        .output()?;
    assert!(changed.status.success());
    let error = AccountsFile::load(&file, no_env).unwrap_err();
    assert!(
        matches!(error, AccountsError::Read { ref source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied)
    );
    assert!(!shown(&error).contains(SECRET));
    assert!(matches!(
        AccountsFile::load(&dir.path().join("missing"), no_env),
        Err(AccountsError::Read { .. })
    ));
    Ok(())
}

#[test]
fn the_loaders_rules_refuse_a_bad_file() {
    let cases = [
        ("[]".to_owned(), "an empty array"),
        (r#"{"accounts": []}"#.to_owned(), "an empty accounts list"),
        (r#"{"other": 1}"#.to_owned(), "no accounts"),
        (
            format!(r#"[{{"profile": "someone", "email": "{UPN_A}", "password": "{SECRET}"}}]"#),
            "an unsupported profile",
        ),
        (
            format!(
                r#"[{{"profile": "a", "email": "{UPN_A}", "password": "{SECRET}"}},
                    {{"profile": "a", "email": "x@lab.example.com", "password": "{SECRET}"}}]"#
            ),
            "a profile twice",
        ),
        (
            format!(r#"[{{"profile": "a", "email": "not-an-email", "password": "{SECRET}"}}]"#),
            "an invalid email",
        ),
        (
            format!(r#"[{{"profile": "a", "email": "{UPN_A}", "password": ""}}]"#),
            "an empty password",
        ),
        (format!(r#"[{{"profile": "a", "email": "{UPN_A}"}}]"#), "no password"),
        // a password of the wrong type: serde would print the value in a type error
        (
            format!(r#"[{{"profile": "a", "email": "{UPN_A}", "password": ["{SECRET}"]}}]"#),
            "a password that isn't a string",
        ),
        // broken JSON right after the password: a syntax error must not quote it
        (
            format!(r#"[{{"profile": "a", "email": "{UPN_A}", "password": "{SECRET}" "#),
            "broken JSON",
        ),
        (
            format!(r#"[{{"profile": "a", "email": "{UPN_A}", "password": "{SECRET}"}}] x"#),
            "trailing text",
        ),
    ];
    for (text, what) in cases {
        let error = AccountsFile::parse(&text, no_env).err();
        let Some(error) = error else {
            panic!("accepted {what}");
        };
        assert!(!shown(&error).contains(SECRET), "{what}: {error}");
        assert!(!shown(&error).contains("Planted"), "{what}: {error}");
    }
}

#[test]
fn debug_output_never_shows_the_password() -> TestResult {
    let accounts = AccountsFile::parse(&accounts_json(&format!(r#""password": "{SECRET}""#)), no_env)?;
    let credentials = accounts.for_alias(Alias::A, UPN_A)?;
    for text in [
        format!("{accounts:?}"),
        format!("{credentials:?}"),
        format!("{:?}", credentials.password()),
        format!("{credentials:#?}"),
    ] {
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains("other-b"), "{text}");
    }
    assert!(format!("{credentials:?}").contains(UPN_A));
    Ok(())
}

// --- telling where the session is, from recorded CDP events ---

const PAGE: &str = "PAGE-SESSION";
const MAIN: &str = "MAIN-FRAME";
/// A worker's session: its events don't come on the page's.
const WORKER: &str = "WORKER";
const AUTHZ: &str = "https://teams.microsoft.com/api/authsvc/v1.0/authz";
const OID_A: &str = "00000000-0000-4000-8000-00000000000a";
const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";

fn event(session: &str, method: &str, params: Value) -> Event {
    Event {
        session_id: Some(SessionId::new(session)),
        method: method.to_owned(),
        params,
    }
}

fn navigated(session: &str, frame: &str, parent: Option<&str>, url: &str) -> Event {
    let mut frame = json!({"id": frame, "loaderId": "L", "url": url});
    if let Some(parent) = parent {
        frame["parentId"] = json!(parent);
    }
    event(
        session,
        "Page.frameNavigated",
        json!({"frame": frame, "type": "Navigation"}),
    )
}

fn stopped(session: &str, frame: &str) -> Event {
    event(session, "Page.frameStoppedLoading", json!({"frameId": frame}))
}

fn request(session: &str, id: &str, method: &str, url: &str, headers: &Value) -> Event {
    event(
        session,
        "Network.requestWillBeSent",
        json!({"requestId": id, "request": {"url": url, "method": method, "headers": headers}}),
    )
}

fn response(session: &str, id: &str, url: &str, status: u16) -> Event {
    event(
        session,
        "Network.responseReceived",
        json!({"requestId": id, "type": "Fetch", "response": {"url": url, "status": status}}),
    )
}

/// A token-shaped string carrying `claims`; built here so no token-like literal sits in the source.
fn jwt(claims: &Value) -> String {
    let part = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
    format!(
        "{}.{}.{}",
        part(&json!({"alg": "none", "typ": "JWT"})),
        part(claims),
        "c2lnbmF0dXJl"
    )
}

fn bearer(claims: &Value) -> Value {
    json!({"Authorization": format!("Bearer {}", jwt(claims))})
}

fn claims_a() -> Value {
    json!({"aud": "https://api.spaces.skype.com", "oid": OID_A, "tid": TENANT, "upn": UPN_A})
}

fn watch(events: &[Event]) -> SessionWatch {
    let mut watch = SessionWatch::new(SessionId::new(PAGE));
    for event in events {
        watch.observe(event);
    }
    watch
}

/// The web client's main frame on Teams and its authz exchange answered: a signed-in session.
fn loaded_events() -> Vec<Event> {
    vec![
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        request(PAGE, "r1", "POST", AUTHZ, &bearer(&claims_a())),
        response(PAGE, "r1", AUTHZ, 200),
    ]
}

#[test]
fn the_web_client_has_loaded_once_its_authz_exchange_succeeds_on_teams() {
    let events = loaded_events();
    assert_eq!(watch(&events[..1]).state(), SessionState::Loading);
    assert_eq!(watch(&events[..2]).state(), SessionState::Loading);
    assert_eq!(watch(&events).state(), SessionState::Loaded);
    // the new Teams domain counts as Teams too
    let cloud = [
        navigated(PAGE, MAIN, None, "https://teams.cloud.microsoft/"),
        request(PAGE, "r1", "POST", AUTHZ, &json!({})),
        response(PAGE, "r1", AUTHZ, 200),
    ];
    assert_eq!(watch(&cloud).state(), SessionState::Loaded);
}

#[test]
fn a_client_with_a_cached_skypetoken_has_loaded_once_a_service_read_succeeds() {
    // no authz exchange: the client still held a skypetoken from an earlier run and went straight to its services
    const CSA: &str = "https://teams.cloud.microsoft/api/csa/amer/api/v1/teams/users/me/pinnedChannels";
    const CHATSVC: &str = "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/48%3Anotifications";
    for url in [CSA, CHATSVC] {
        let events = [
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
            request(PAGE, "s1", "GET", url, &json!({})),
            response(PAGE, "s1", url, 200),
        ];
        assert_eq!(watch(&events[..2]).state(), SessionState::Loading, "{url}");
        assert_eq!(watch(&events).state(), SessionState::Loaded, "{url}");
    }
    // a failed read, a read of another service, a write, or a read from before the last navigation proves nothing
    let not_loaded = [
        vec![
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
            request(PAGE, "s1", "GET", CHATSVC, &json!({})),
            response(PAGE, "s1", CHATSVC, 404),
        ],
        vec![
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
            request(
                PAGE,
                "s1",
                "GET",
                "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/tenantsv2",
                &json!({}),
            ),
            response(
                PAGE,
                "s1",
                "https://teams.microsoft.com/api/mt/part/amer-03/beta/users/tenantsv2",
                200,
            ),
        ],
        vec![
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
            request(PAGE, "s1", "PUT", CHATSVC, &json!({})),
            response(PAGE, "s1", CHATSVC, 200),
        ],
        vec![
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
            request(PAGE, "s1", "GET", CSA, &json!({})),
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
            response(PAGE, "s1", CSA, 200),
        ],
    ];
    for events in not_loaded {
        assert_eq!(watch(&events).state(), SessionState::Loading);
    }
    // a refused authz still reads as a refusal, whatever else answered
    let refused = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        request(PAGE, "r1", "POST", AUTHZ, &json!({})),
        response(PAGE, "r1", AUTHZ, 403),
        request(PAGE, "s1", "GET", CSA, &json!({})),
        response(PAGE, "s1", CSA, 200),
    ];
    assert_eq!(watch(&refused).state(), SessionState::Refused(403));
}

#[test]
fn an_authz_exchange_from_a_worker_counts_too() {
    let events = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        request("WORKER", "w1", "POST", AUTHZ, &json!({})),
        response("WORKER", "w1", AUTHZ, 200),
    ];
    assert_eq!(watch(&events).state(), SessionState::Loaded);
}

#[test]
fn a_failed_or_preflight_authz_is_not_a_session() {
    let failed = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        request(PAGE, "r1", "POST", AUTHZ, &json!({})),
        response(PAGE, "r1", AUTHZ, 401),
    ];
    assert_eq!(watch(&failed).state(), SessionState::Loading);
    let preflight = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        request(PAGE, "p1", "OPTIONS", AUTHZ, &json!({})),
        response(PAGE, "p1", AUTHZ, 200),
    ];
    assert_eq!(watch(&preflight).state(), SessionState::Loading);
    // a response nobody saw the request of proves nothing either
    let unknown = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        response(PAGE, "x", AUTHZ, 200),
    ];
    assert_eq!(watch(&unknown).state(), SessionState::Loading);
}

/// The web client on Teams, its authz exchange answered with `statuses` in turn.
fn authz_answered(statuses: &[u16]) -> Vec<Event> {
    let mut events = vec![navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/")];
    for (index, status) in statuses.iter().enumerate() {
        let id = format!("a{index}");
        events.push(request(PAGE, &id, "POST", AUTHZ, &bearer(&claims_a())));
        events.push(response(PAGE, &id, AUTHZ, *status));
    }
    events
}

#[test]
fn teams_refusing_the_account_is_told_apart_from_loading() {
    // a signed-in account Teams won't serve (no license): authz is forbidden, maybe again after a fresh token
    assert_eq!(watch(&authz_answered(&[403])).state(), SessionState::Refused(403));
    assert_eq!(watch(&authz_answered(&[403, 403])).state(), SessionState::Refused(403));
    // a retry that works is a session
    assert_eq!(watch(&authz_answered(&[403, 200])).state(), SessionState::Loaded);
    assert_eq!(watch(&authz_answered(&[403, 403, 200])).state(), SessionState::Loaded);
    // an expired token (the client refreshes it) and server errors are not a refusal
    assert_eq!(watch(&authz_answered(&[401, 401])).state(), SessionState::Loading);
    assert_eq!(watch(&authz_answered(&[503, 500])).state(), SessionState::Loading);
    // refusals in an earlier document don't count
    let mut events = authz_answered(&[403]);
    events.push(navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"));
    assert_eq!(watch(&events).state(), SessionState::Loading);
    // the identity is there all the same: the refused request carried the token
    assert!(watch(&authz_answered(&[403])).identity_for(UPN_A).is_some());
}

#[test]
fn the_last_authz_answer_is_kept_for_the_report() {
    assert_eq!(watch(&authz_answered(&[])).last_authz_status(), None);
    assert_eq!(watch(&authz_answered(&[500])).last_authz_status(), Some(500));
    assert_eq!(watch(&authz_answered(&[401, 200])).last_authz_status(), Some(200));
    // only answers to the exchange itself
    let preflight = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        request(PAGE, "p1", "OPTIONS", AUTHZ, &json!({})),
        response(PAGE, "p1", AUTHZ, 204),
    ];
    assert_eq!(watch(&preflight).last_authz_status(), None);
}

#[test]
fn an_authz_before_the_page_reached_teams_does_not_count() {
    // the exchange belongs to the document it was made in; a new main-frame document starts over
    let mut events = loaded_events();
    events.push(navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/?reload"));
    assert_eq!(watch(&events).state(), SessionState::Loading);
}

#[test]
fn an_answer_to_an_authz_sent_by_an_earlier_document_does_not_count() {
    // sent from a worker of the first document, answered after the main frame moved on to a new one
    for status in [200, 403] {
        let events = [
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
            request(WORKER, "w1", "POST", AUTHZ, &bearer(&claims_a())),
            navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/?reload"),
            response(WORKER, "w1", AUTHZ, status),
        ];
        assert_eq!(watch(&events).state(), SessionState::Loading, "{status}");
        // nor is it the last answer of the new document
        assert_eq!(watch(&events).last_authz_status(), None, "{status}");
    }
    // the new document's own exchange counts as ever
    let events = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        request(WORKER, "w1", "POST", AUTHZ, &json!({})),
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/?reload"),
        request(WORKER, "w2", "POST", AUTHZ, &json!({})),
        response(WORKER, "w1", AUTHZ, 403),
        response(WORKER, "w2", AUTHZ, 200),
    ];
    assert_eq!(watch(&events).state(), SessionState::Loaded);
}

#[test]
fn the_document_counts_main_frame_navigations_only() {
    let mut seen = SessionWatch::new(SessionId::new(PAGE));
    assert_eq!(seen.document(), 0);
    seen.observe(&navigated(
        PAGE,
        MAIN,
        None,
        "https://login.microsoftonline.com/common/oauth2/authorize",
    ));
    assert_eq!(seen.document(), 1);
    // a child frame, another tab and the page's own requests leave it alone
    seen.observe(&navigated(PAGE, "child", Some(MAIN), "https://example.com/"));
    seen.observe(&navigated("S-other", "F-other", None, "https://example.com/"));
    seen.observe(&request(PAGE, "r1", "POST", AUTHZ, &json!({})));
    assert_eq!(seen.document(), 1);
    // the form's post commits the next document, on the same URL too
    seen.observe(&navigated(
        PAGE,
        MAIN,
        None,
        "https://login.microsoftonline.com/common/oauth2/authorize",
    ));
    assert_eq!(seen.document(), 2);
}

#[test]
fn a_refusal_is_believed_only_once_it_has_stood_for_its_grace() {
    let start = tokio::time::Instant::now();
    let refused = SessionState::Refused(403);
    let mut settling = Settling::default();
    assert_eq!(settling.judge(refused.clone(), start), SessionState::Loading);
    assert_eq!(
        settling.judge(refused.clone(), start + REFUSAL_GRACE / 2),
        SessionState::Loading
    );
    assert_eq!(settling.judge(refused.clone(), start + REFUSAL_GRACE), refused);
    // a retry in between starts the stand over
    let mut settling = Settling::default();
    settling.judge(refused.clone(), start);
    assert_eq!(
        settling.judge(SessionState::Loading, start + REFUSAL_GRACE / 2),
        SessionState::Loading
    );
    assert_eq!(
        settling.judge(refused.clone(), start + REFUSAL_GRACE),
        SessionState::Loading,
        "the refusal stood only since it came back"
    );
    assert_eq!(settling.judge(refused.clone(), start + REFUSAL_GRACE * 2), refused);
    // every other state counts at once
    for state in [
        SessionState::Loaded,
        SessionState::SignInShown,
        SessionState::Elsewhere("example.com".to_owned()),
        SessionState::Loading,
    ] {
        assert_eq!(Settling::default().judge(state.clone(), start), state);
    }
}

#[test]
fn a_sign_in_page_left_standing_means_the_session_has_expired() {
    let events = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        navigated(
            PAGE,
            MAIN,
            None,
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=x",
        ),
    ];
    // still loading: the sign-in host may only be passing the browser on
    assert_eq!(watch(&events).state(), SessionState::Loading);
    let mut shown = events.to_vec();
    shown.push(stopped(PAGE, MAIN));
    assert_eq!(watch(&shown).state(), SessionState::SignInShown);
    // the same on the other Microsoft sign-in hosts
    for url in [
        "https://login.microsoft.com/common/login",
        "https://login.live.com/login.srf",
    ] {
        let events = [navigated(PAGE, MAIN, None, url), stopped(PAGE, MAIN)];
        assert_eq!(watch(&events).state(), SessionState::SignInShown, "{url}");
    }
}

#[test]
fn passing_through_sign_in_back_to_teams_is_not_an_expired_session() {
    let events = [
        navigated(
            PAGE,
            MAIN,
            None,
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        ),
        stopped(PAGE, MAIN),
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
    ];
    assert_eq!(watch(&events).state(), SessionState::Loading);
}

#[test]
fn only_the_pages_main_frame_says_where_the_session_is() {
    let events = [
        navigated(PAGE, MAIN, None, "https://teams.microsoft.com/v2/"),
        // MSAL's hidden iframe on the sign-in host, in the page and as an out-of-process iframe target
        navigated(
            PAGE,
            "CHILD",
            Some(MAIN),
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        ),
        stopped(PAGE, "CHILD"),
        navigated(
            "IFRAME-SESSION",
            "CHILD",
            None,
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        ),
        stopped("IFRAME-SESSION", "CHILD"),
        // another page
        navigated(
            "OTHER-PAGE",
            "M2",
            None,
            "https://login.microsoftonline.com/common/login",
        ),
        stopped("OTHER-PAGE", "M2"),
    ];
    assert_eq!(watch(&events).state(), SessionState::Loading);
}

#[test]
fn a_main_frame_anywhere_else_is_reported_by_host() {
    let events = [
        navigated(PAGE, MAIN, None, "https://www.office.com/landing?x=1"),
        stopped(PAGE, MAIN),
    ];
    assert_eq!(
        watch(&events).state(),
        SessionState::Elsewhere("www.office.com".to_owned())
    );
    // a blank page is where every run starts
    let blank = [navigated(PAGE, MAIN, None, "about:blank"), stopped(PAGE, MAIN)];
    assert_eq!(watch(&blank).state(), SessionState::Loading);
}

#[test]
#[cfg(unix)]
fn status_reports_each_state_in_words() {
    let timeout = Duration::from_secs(60);
    let words = |state: &SessionState, authz: Option<u16>| account_status(state, timeout, authz).to_string();
    assert_eq!(words(&SessionState::Loaded, Some(200)), "ok");
    assert_eq!(words(&SessionState::SignInShown, None), "login needed");
    assert_eq!(
        words(&SessionState::Refused(403), Some(403)),
        "signed in, but Teams refused the account (authz answered 403)"
    );
    assert_eq!(
        words(&SessionState::Elsewhere("x.example".to_owned()), None),
        "unknown (the page stopped on x.example)"
    );
    assert_eq!(
        words(&SessionState::Loading, Some(500)),
        "unknown (the web client didn't load within 60s; authz answered 500)"
    );
    assert_eq!(
        words(&SessionState::Loading, None),
        "unknown (the web client didn't load within 60s; no authz answer)"
    );
    assert_eq!(account_status(&SessionState::Loaded, timeout, None), AccountStatus::Ok);
}

// --- who the page is signed in as ---

#[test]
fn the_identity_comes_from_the_claims_of_a_bearer_token_the_page_sends() -> TestResult {
    let watch = watch(&loaded_events());
    let identity = watch.identity_for(UPN_A).ok_or("no identity")?;
    assert_eq!(identity.oid, OID_A);
    assert_eq!(identity.tenant, TENANT);
    assert_eq!(identity.mri()?, format!("8:orgid:{OID_A}"));
    // the UPN is matched without case; another account's is not this one
    assert!(watch.identity_for("BOT-A@LAB.EXAMPLE.COM").is_some());
    assert!(watch.identity_for("bot-b@lab.example.com").is_none());
    // nothing of the token is kept: Debug shows the claims only
    let shown = format!("{identity:?} {watch:?}");
    assert!(!shown.contains(&jwt(&claims_a())), "{shown}");
    assert!(!shown.contains("Bearer"), "{shown}");
    Ok(())
}

#[test]
fn the_upn_claim_may_be_named_otherwise() {
    for name in ["preferred_username", "unique_name"] {
        let claims = json!({"oid": OID_A, "tid": TENANT, name: UPN_A});
        let events = [request(
            PAGE,
            "r",
            "GET",
            "https://example.test/",
            &json!({"authorization": format!("bearer {}", jwt(&claims))}),
        )];
        assert!(watch(&events).identity_for(UPN_A).is_some(), "{name}");
    }
}

#[test]
fn a_token_without_the_claims_or_not_a_token_gives_no_identity() {
    let headers = [
        bearer(&json!({"tid": TENANT, "upn": UPN_A})),
        bearer(&json!({"oid": OID_A, "upn": UPN_A})),
        bearer(&json!({"oid": OID_A, "tid": TENANT})),
        bearer(&json!({"oid": 7, "tid": TENANT, "upn": UPN_A})),
        json!({"Authorization": "Bearer not-a-token"}),
        json!({"Authorization": "Bearer a.%%%.c"}),
        json!({"Authorization": format!("Basic {}", jwt(&claims_a()))}),
        json!({"Authentication": format!("skypetoken={}", jwt(&json!({"skypeid": "x"})))}),
    ];
    for headers in headers {
        let events = [request(PAGE, "r", "GET", "https://example.test/", &headers)];
        assert!(watch(&events).identity_for(UPN_A).is_none(), "{headers}");
    }
}

#[test]
fn two_object_ids_for_one_upn_give_no_identity() {
    let other = json!({"oid": "00000000-0000-4000-8000-0000000000bb", "tid": TENANT, "upn": UPN_A});
    let events = [
        request(PAGE, "r1", "GET", "https://example.test/", &bearer(&claims_a())),
        request(PAGE, "r2", "GET", "https://example.test/", &bearer(&other)),
    ];
    assert!(watch(&events).identity_for(UPN_A).is_none());
}

#[test]
fn an_mri_needs_an_object_id_shaped_like_a_guid() {
    for oid in [
        "",
        "x",
        "00000000-0000-4000-8000-00000000000",
        "00000000-0000-4000-8000-00000000000g",
        "8:orgid:x",
    ] {
        let identity = Identity {
            oid: oid.to_owned(),
            upn: UPN_A.to_owned(),
            tenant: TENANT.to_owned(),
        };
        assert!(identity.mri().is_err(), "{oid:?}");
    }
    // upper case is lowered, as Teams writes MRIs
    let identity = Identity {
        oid: OID_A.to_uppercase(),
        upn: UPN_A.to_owned(),
        tenant: TENANT.to_owned(),
    };
    assert_eq!(identity.mri().ok(), Some(format!("8:orgid:{OID_A}")));
}

// --- what login lets through ---

#[test]
fn login_records_its_run_fail_closed_and_without_request_bodies() {
    let options = session_options();
    assert_eq!(options.capture.as_deref(), Some(LOGIN_CAPTURE));
    assert_eq!(LOGIN_CAPTURE, "login");
    assert!(
        !options.capture_request_bodies,
        "the sign-in form's posts carry the password"
    );
    assert_eq!(options.display, Display::Headless);
}

// --- the sign-in steps ---

fn probe_of(value: &Value) -> SignInProbe {
    SignInProbe::from_value(value)
}

const EMAIL: &str = r#"input[name="loginfmt"]"#;
const PASSWD: &str = r#"input[name="passwd"]"#;

#[test]
fn the_email_and_then_the_password_are_typed_once_each() {
    let mut progress = Progress::default();
    let email = probe_of(&json!({"email": EMAIL}));
    assert_eq!(next_action(&email, progress), Action::TypeEmail(EMAIL.to_owned()));
    progress.email_sent = true;
    // the same field again (the page hasn't moved on yet): wait, don't retype
    assert_eq!(next_action(&email, progress), Action::Wait);
    // the email page's "Sign-in options" tile matches the method-choice selectors: still the email page, so wait
    let with_options = probe_of(&json!({"email": EMAIL, "proofs": true, "next": true}));
    assert_eq!(next_action(&with_options, progress), Action::Wait);
    let password = probe_of(&json!({"password": PASSWD, "email": EMAIL}));
    assert_eq!(
        next_action(&password, progress),
        Action::TypePassword(PASSWD.to_owned())
    );
    progress.password_sent = true;
    // the password field still there after sending: wait, never send it twice
    assert_eq!(next_action(&password, progress), Action::Wait);
}

#[test]
fn an_unknown_username_stops_the_run_after_the_email_went_in() {
    let unknown = probe_of(&json!({"email": EMAIL, "unknownAccount": true, "proofs": true, "next": true}));
    assert_eq!(
        next_action(&unknown, Progress::default()),
        Action::TypeEmail(EMAIL.to_owned())
    );
    let sent = Progress {
        email_sent: true,
        password_sent: false,
    };
    assert_eq!(next_action(&unknown, sent), Action::Stop(SignInStop::UnknownAccount));
}

#[test]
fn a_rejected_password_stops_the_run() {
    let progress = Progress {
        email_sent: true,
        password_sent: true,
    };
    for signal in ["passwordError", "rejected"] {
        let probe = probe_of(&json!({"password": PASSWD, signal: true}));
        assert_eq!(
            next_action(&probe, progress),
            Action::Stop(SignInStop::PasswordRejected),
            "{signal}"
        );
    }
}

#[test]
fn stay_signed_in_is_answered_yes_after_the_password() {
    let before = Progress::default();
    let after = Progress {
        email_sent: true,
        password_sent: true,
    };
    let kmsi = probe_of(&json!({"kmsi": "#KmsiCheckboxField", "next": true}));
    assert_eq!(
        next_action(&kmsi, after),
        Action::AcceptKmsi("#KmsiCheckboxField".to_owned())
    );
    // a "next" button is pressed only after the password went in
    assert_eq!(next_action(&kmsi, before), Action::Wait);
    assert_eq!(next_action(&probe_of(&json!({"next": true})), after), Action::ClickNext);
    assert_eq!(next_action(&probe_of(&json!({"next": true})), before), Action::Wait);
}

#[test]
fn another_account_tile_is_picked_before_the_email() {
    let probe = probe_of(&json!({"otherTile": "#otherTile"}));
    assert_eq!(
        next_action(&probe, Progress::default()),
        Action::ClickOtherTile("#otherTile".to_owned())
    );
}

#[test]
fn every_mfa_prompt_stops_the_run() {
    let after = Progress {
        email_sent: true,
        password_sent: true,
    };
    let registration = probe_of(&json!({"registration": true, "next": true}));
    assert_eq!(
        next_action(&registration, after),
        Action::Stop(SignInStop::MfaRegistration)
    );
    // `login` has no step that types a one-time code in: stop, don't ask for one
    let code = probe_of(&json!({"otc": r#"input[name="otc"]"#, "next": true}));
    assert_eq!(next_action(&code, after), Action::Stop(SignInStop::MfaCode));
    assert_eq!(
        next_action(&code, Progress::default()),
        Action::Stop(SignInStop::MfaCode)
    );
    // registration wins when both show
    let both = probe_of(&json!({"otc": r#"input[name="otc"]"#, "registration": true, "next": true}));
    assert_eq!(next_action(&both, after), Action::Stop(SignInStop::MfaRegistration));
    assert!(SignInStop::MfaCode.to_string().contains("one-time code"));
    let choice = probe_of(&json!({"proofs": true, "next": true}));
    assert_eq!(next_action(&choice, after), Action::Stop(SignInStop::MfaChoice));
    let approval = probe_of(&json!({"approval": true, "next": true}));
    assert_eq!(next_action(&approval, after), Action::Stop(SignInStop::MfaApproval));
}

#[test]
fn a_probe_with_nothing_in_it_waits() {
    assert_eq!(next_action(&probe_of(&json!({})), Progress::default()), Action::Wait);
    assert_eq!(next_action(&probe_of(&json!(null)), Progress::default()), Action::Wait);
    // a field given as anything but a selector is no field
    assert_eq!(
        next_action(&probe_of(&json!({"email": true})), Progress::default()),
        Action::Wait
    );
}

#[test]
fn a_click_is_not_repeated_on_the_same_document() {
    let mut clicks = Clicks::default();
    let kmsi = Action::AcceptKmsi("#KmsiCheckboxField".to_owned());
    assert_eq!(clicks.admit(kmsi.clone(), 3), kmsi);
    // the post is on its way and the page still shows the button: don't submit again
    assert_eq!(clicks.admit(kmsi.clone(), 3), Action::Wait);
    assert_eq!(clicks.admit(kmsi.clone(), 3), Action::Wait);
    // the next document may ask again
    assert_eq!(clicks.admit(kmsi.clone(), 4), kmsi);
    // another click on the same document goes ahead, and so does the first one again after it
    assert_eq!(clicks.admit(Action::ClickNext, 4), Action::ClickNext);
    assert_eq!(clicks.admit(Action::ClickNext, 4), Action::Wait);
    let tile = Action::ClickOtherTile("#otherTile".to_owned());
    assert_eq!(clicks.admit(tile.clone(), 4), tile);
    assert_eq!(clicks.admit(tile, 4), Action::Wait);
    // typing is kept to once by the progress, not here; waits and stops pass through
    let email = Action::TypeEmail("#email".to_owned());
    assert_eq!(clicks.admit(email.clone(), 4), email);
    assert_eq!(clicks.admit(email.clone(), 4), email);
    let stop = Action::Stop(SignInStop::MfaCode);
    assert_eq!(clicks.admit(stop.clone(), 4), stop);
    assert_eq!(clicks.admit(Action::Wait, 4), Action::Wait);
}

// --- the status file ---

#[test]
fn the_status_file_is_private_and_replaced_on_each_state() -> TestResult {
    let dir = tempfile::tempdir()?;
    let status = StatusFile::new(dir.path(), Alias::B);
    status.write("starting", &json!({}))?;
    status.write("failed", &json!({"error": "the password was rejected"}))?;
    let file = dir.path().join("status-b.json");
    privacy::assert_private(&file, false);
    let record: Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
    assert_eq!(record["alias"], "b");
    assert_eq!(record["state"], "failed");
    assert_eq!(record["error"], "the password was rejected");
    assert!(record["at_unix_ms"].as_u64().is_some_and(|at| at > 0));
    Ok(())
}

// --- the sign-in driver on a local stand-in page, with Chrome ---

/// A stand-in for Microsoft's sign-in page: an email pane, a password pane and a "Stay signed in?" pane behind one
/// `#idSIButton9`, plus an invisible password field that must never be typed into. `?reject` refuses the password,
/// `?register` asks to register an MFA method after it, `?slow` submits "Stay signed in?" from the page (a GET, which
/// the fail-closed fence lets through) with an answer that takes [`SLOW_ANSWER`] to come, and leaves for `/done` only
/// then. It records what was submitted in `window.lab`.
const SIGN_IN_PAGE: &str = r#"<!doctype html><html><body>
<style>input{width:300px;height:30px} .pane{display:none} .on{display:block}</style>
<input name="passwd" type="password" id="trap" style="opacity:0;position:absolute;left:0;top:0">
<div id="email" class="pane on"><input name="loginfmt" type="email"></div>
<div id="password" class="pane"><input name="passwd" type="password" id="pw"><div id="passwordError" class="pane">Your account or password is incorrect.</div></div>
<div id="kmsi" class="pane">Stay signed in? <input type="checkbox" id="KmsiCheckboxField"></div>
<div id="register" class="pane">Let's keep your account secure</div>
<input type="submit" id="idSIButton9" value="Next">
<script>
window.lab = {email: [], passwords: 0, lastPassword: null, trap: 0};
const q = new URLSearchParams(location.search);
const show = (id) => { for (const p of document.querySelectorAll('.pane')) p.classList.remove('on'); document.getElementById(id).classList.add('on'); };
document.getElementById('trap').addEventListener('input', () => { window.lab.trap += 1; });
let pane = 'email';
document.getElementById('idSIButton9').addEventListener('click', () => {
  if (pane === 'email') { window.lab.email.push(document.querySelector('input[name=loginfmt]').value); pane = 'password'; show('password'); return; }
  if (pane === 'password') {
    window.lab.passwords += 1; window.lab.lastPassword = document.getElementById('pw').value;
    if (q.has('reject')) { document.getElementById('passwordError').classList.add('on'); return; }
    if (q.has('register')) { pane = 'register'; show('register'); return; }
    pane = 'kmsi'; show('kmsi'); return;
  }
  if (pane === 'kmsi') {
    const done = '/done?kmsi=' + document.getElementById('KmsiCheckboxField').checked;
    if (!q.has('slow')) { location.href = done; return; }
    // submitted from the page, which keeps showing "Stay signed in?" until the answer comes
    window.lab.kmsi = (window.lab.kmsi ?? 0) + 1;
    fetch('/kmsi-slow?n=' + window.lab.kmsi).then(() => { location.href = done; });
  }
});
</script></body></html>"#;

struct SignInRun {
    armed: ArmedBrowser,
    page: SessionId,
    _dir: tempfile::TempDir,
}

async fn sign_in_run(chrome: &Path, _server: &MockServer) -> TestResult<SignInRun> {
    let dir = tempfile::tempdir()?;
    let browser = Browser::launch(
        chrome,
        &LaunchOptions::new(dir.path().join("profile")),
        Duration::from_secs(30),
    )
    .await?;

    let http = CaptureHold::new();

    let armed = ArmedBrowser::arm(browser, http).await?;
    let page = armed.open_page(Duration::from_secs(15)).await?.session;
    Ok(SignInRun { armed, page, _dir: dir })
}

async fn serve_sign_in() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/signin"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SIGN_IN_PAGE, "text/html"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/done"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("<!doctype html><p>done</p>", "text/html"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/kmsi-slow"))
        .respond_with(ResponseTemplate::new(204).set_delay(SLOW_ANSWER))
        .mount(&server)
        .await;
    server
}

/// How long the stand-in's slow answer to "Stay signed in?" takes: several of the driver's looks at the page.
const SLOW_ANSWER: Duration = Duration::from_secs(3);

/// Runs the driver on the stand-in page until it stops, the page leaves for `/done`, or 40 steps pass, a look every
/// 250 ms (faster than `login`'s, so a repeated click would show). Returns the last action, the page's record and the
/// page's URL.
async fn drive(run: &SignInRun, url: &str, password: &str) -> TestResult<(Action, Value, String)> {
    let text = format!(r#"[{{"profile": "a", "email": "{UPN_A}", "password": "{password}"}}]"#);
    let accounts = AccountsFile::parse(&text, no_env)?;
    let credentials = accounts.for_alias(Alias::A, UPN_A)?;
    let cdp = run.armed.cdp();
    let mut events = run.armed.watch();
    cdp.call(Some(&run.page), "Page.navigate", json!({"url": url})).await?;
    let mut seen = SessionWatch::new(run.page.clone());
    let mut progress = Progress::default();
    let mut clicks = Clicks::default();
    let mut last = Action::Wait;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        while let Ok(event) = events.try_recv() {
            seen.observe(&event);
        }
        if matches!(seen.state(), SessionState::Elsewhere(_)) && current_url(run).await?.contains("/done") {
            break;
        }
        let found = probe(cdp, &run.page).await?;
        last = clicks.admit(next_action(&found, progress), seen.document());
        assert!(!format!("{found:?} {last:?}").contains(password));
        if matches!(last, Action::Stop(_)) {
            break;
        }
        perform(cdp, &run.page, &last, credentials, &mut progress).await?;
    }
    let record = cdp
        .call(
            Some(&run.page),
            "Runtime.evaluate",
            json!({"expression": "window.lab ?? null", "returnByValue": true}),
        )
        .await?;
    Ok((last, record["result"]["value"].clone(), current_url(run).await?))
}

async fn current_url(run: &SignInRun) -> TestResult<String> {
    let result = run
        .armed
        .cdp()
        .call(
            Some(&run.page),
            "Runtime.evaluate",
            json!({"expression": "location.href", "returnByValue": true}),
        )
        .await?;
    Ok(result["result"]["value"].as_str().unwrap_or_default().to_owned())
}

/// Runs `body` on an armed throwaway browser serving the stand-in page, and closes the browser after. Without Chrome
/// it does nothing.
async fn with_sign_in<F>(body: F) -> TestResult
where
    F: AsyncFnOnce(&SignInRun, &MockServer) -> TestResult,
{
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let server = serve_sign_in().await;
    let run = sign_in_run(&chrome, &server).await?;
    let outcome = tokio::time::timeout(Duration::from_secs(60), body(&run, &server)).await;
    run.armed.close(Duration::from_secs(10)).await?;
    outcome.map_err(|_| "the sign-in took over 60 s")?
}

#[tokio::test]
async fn the_driver_signs_in_through_email_password_and_stay_signed_in() -> TestResult {
    with_sign_in(async |run, server| {
        let (last, record, url) = drive(run, &format!("{}/signin", server.uri()), SECRET).await?;
        assert!(url.ends_with("/done?kmsi=true"), "{url} after {last:?}");
        assert_eq!(record, Value::Null, "the done page has no record");
        // the run never saw a deny of its own: typing and clicking are not requests
        assert!(run.armed.held_requests().is_empty(), "{:?}", run.armed.held_requests());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_driver_answers_stay_signed_in_once_while_its_answer_is_on_the_way() -> TestResult {
    with_sign_in(async |run, server| {
        let (last, _, url) = drive(run, &format!("{}/signin?slow", server.uri()), SECRET).await?;
        assert!(url.ends_with("/done?kmsi=true"), "{url} after {last:?}");
        // the page showed "Stay signed in?" for several looks while the answer was coming: one submit all the same
        let requests = server.received_requests().await.unwrap_or_default();
        let submits = requests
            .iter()
            .filter(|request| request.url.path() == "/kmsi-slow")
            .count();
        assert_eq!(submits, 1, "\"Stay signed in?\" submitted {submits} times");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_driver_types_into_interactive_fields_only_and_sends_the_password_once() -> TestResult {
    with_sign_in(async |run, server| {
        let (last, record, _) = drive(run, &format!("{}/signin?reject", server.uri()), SECRET).await?;
        assert_eq!(last, Action::Stop(SignInStop::PasswordRejected));
        assert_eq!(record["email"], json!([UPN_A]));
        assert_eq!(record["passwords"], 1);
        assert_eq!(record["lastPassword"], SECRET);
        assert_eq!(record["trap"], 0, "typed into an invisible field");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn the_driver_stops_when_asked_to_register_an_mfa_method() -> TestResult {
    with_sign_in(async |run, server| {
        let (last, record, _) = drive(run, &format!("{}/signin?register", server.uri()), SECRET).await?;
        assert_eq!(last, Action::Stop(SignInStop::MfaRegistration));
        assert_eq!(record["passwords"], 1);
        Ok(())
    })
    .await
}
