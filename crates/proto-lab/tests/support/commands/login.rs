//! Private login orchestration on an armed synthetic CDP session; no account file, browser, keyring or tenant.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::*;
use crate::scenario::test_page::{Page, evaluated};

const UPN: &str = "synthetic@example.test";
const PASSWORD: &str = "made-up-password-do-not-log";
const OID: &str = "00000000-0000-4000-8000-00000000000a";

fn accounts() -> AccountsFile {
    AccountsFile::parse(
        &json!({"_synthetic":true,"accounts":[{"profile":"a","email":UPN,"password":PASSWORD}]}).to_string(),
        |_| None,
    )
    .unwrap()
}

async fn page(probe: Value) -> Page {
    Page::new(move |command| match command["method"].as_str().unwrap() {
        "Runtime.evaluate" => {
            let value = if command["params"]["expression"] == PROBE {
                probe.clone()
            } else {
                json!(true)
            };
            Ok(evaluated(value))
        }
        "Page.navigate" | "Input.insertText" | "Browser.close" => Ok(json!({})),
        method => Err(format!("unexpected synthetic login command {method}")),
    })
    .await
}

#[tokio::test]
async fn selecting_another_account_tile_requires_the_observed_element_to_still_exist() {
    let root = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let credentials = accounts.for_alias(Alias::A, UPN).unwrap();
    for present in [true, false] {
        let mut page = Page::new(move |command| match command["method"].as_str().unwrap() {
            "Runtime.evaluate" => Ok(evaluated(json!(present))),
            "Browser.close" => Ok(json!({})),
            method => Err(format!("unexpected synthetic account-tile command {method}")),
        })
        .await;
        let (session, pid) = page.managed_session(root.path(), false);
        let mut progress = Progress::default();
        let result = perform(
            session.cdp(),
            session.page(),
            &Action::ClickOtherTile("#synthetic-tile".into()),
            credentials,
            &mut progress,
        )
        .await;
        if present {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(LoginError::FieldGone)));
        }
        assert!(!progress.email_sent && !progress.password_sent);
        session.close().await.unwrap();
        assert!(crate::scenario::test_process::exited(pid));
    }
}

fn identity(page: &Page, oid: &str) {
    let encode = |value: Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let token = format!(
        "{}.{}.c2ln",
        encode(json!({"alg":"none"})),
        encode(json!({"oid":oid,"tid":"00000000-0000-4000-8000-000000000001","upn":UPN}))
    );
    page.send("Network.requestWillBeSent", json!({"requestId":"synthetic-identity","request":{"method":"GET","url":"https://teams.microsoft.com/api/synthetic","headers":{"Authorization":format!("Bearer {token}")}}}));
}

#[tokio::test]
async fn an_already_signed_in_page_records_loaded_and_returns_only_the_matching_identity() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let status = StatusFile::new(dir.path(), Alias::A);
    for oid in [OID, "invalid-object-id"] {
        let mut page = page(json!({})).await;
        let mut session = page.loaded_session(dir.path(), false).await;
        identity(&page, oid);
        assert!(session.pump(Duration::from_secs(1)).await);
        let run = Run {
            upn: UPN,
            credentials: accounts.for_alias(Alias::A, UPN).unwrap(),
            status: &status,
            options: LoginOptions {
                timeout: Duration::from_secs(2),
            },
        };
        let result = run.drive(&mut session).await;
        if oid == OID {
            let outcome = result.unwrap();
            assert_eq!(outcome.mri, format!("8:orgid:{OID}"));
            assert!(!outcome.signed_in);
            assert_eq!(outcome.refused, None);
        } else {
            assert!(matches!(result, Err(LoginError::BadIdentity(_))));
        }
        let written = fs::read_to_string(&status.path).unwrap();
        assert!(!written.contains(PASSWORD) && !written.contains(UPN));
        let written: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(written["state"], "loaded");
        assert_eq!(written["signed_in"], false);
        assert!(
            page.commands()
                .iter()
                .all(|command| command["method"] != "Input.insertText")
        );
    }
}

#[tokio::test]
async fn until_loaded_refuses_timeouts_and_unexpected_hosts_before_typing() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let status = StatusFile::new(dir.path(), Alias::A);
    let credentials = accounts.for_alias(Alias::A, UPN).unwrap();
    let mut page = page(json!({"email":"#synthetic-email"})).await;
    let mut session = page.session(dir.path(), false);
    let timed = Run {
        upn: UPN,
        credentials,
        status: &status,
        options: LoginOptions {
            timeout: Duration::ZERO,
        },
    };
    assert!(matches!(
        timed.until_loaded(&mut session, &mut SignIn::default()).await,
        Err(LoginError::Timeout { .. })
    ));
    page.send(
        "Page.frameNavigated",
        json!({"frame":{"id":"synthetic-frame","url":"https://outside.example.test/"}}),
    );
    page.send("Page.frameStoppedLoading", json!({"frameId":"synthetic-frame"}));
    let run = Run {
        options: LoginOptions {
            timeout: Duration::from_secs(1),
        },
        ..timed
    };
    assert!(
        matches!(run.until_loaded(&mut session, &mut SignIn::default()).await, Err(LoginError::UnexpectedHost(host)) if host == "outside.example.test")
    );
    assert!(page.commands().is_empty());
}

#[tokio::test]
async fn sign_in_steps_keep_their_throttle_and_write_each_completed_stage_without_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let status = StatusFile::new(dir.path(), Alias::A);
    let probes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&probes);
    let mut page = Page::new(move |command| match command["method"].as_str().unwrap() {
        "Runtime.evaluate" if command["params"]["expression"] == PROBE => {
            let value = match counted.fetch_add(1, Ordering::SeqCst) {
                0 => json!({"email":"#synthetic-email","next":true}),
                1 => json!({"password":"#synthetic-password","next":true}),
                2 => json!({"kmsi":"#synthetic-kmsi","next":true}),
                _ => json!({}),
            };
            Ok(evaluated(value))
        }
        "Runtime.evaluate" => Ok(evaluated(json!(true))),
        "Input.insertText" => Ok(json!({})),
        method => Err(format!("unexpected login command {method}")),
    })
    .await;
    let session = page.session(dir.path(), false);
    let run = Run {
        upn: UPN,
        credentials: accounts.for_alias(Alias::A, UPN).unwrap(),
        status: &status,
        options: LoginOptions {
            timeout: Duration::from_secs(2),
        },
    };
    let mut sign_in = SignIn::default();
    for expected in ["email-submitted", "password-submitted", "kmsi-accepted"] {
        sign_in.next_look = None;
        run.sign_in_step(&session, &mut sign_in).await.unwrap();
        let record = fs::read_to_string(&status.path).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&record).unwrap()["state"], expected);
        assert!(!record.contains(PASSWORD) && !record.contains(UPN));
        let count = probes.load(Ordering::SeqCst);
        sign_in.next_look = Some(tokio::time::Instant::now() + TICK);
        run.sign_in_step(&session, &mut sign_in).await.unwrap();
        assert_eq!(
            probes.load(Ordering::SeqCst),
            count,
            "the same document is not polled again early"
        );
    }
    sign_in.next_look = None;
    run.sign_in_step(&session, &mut sign_in).await.unwrap();
    let typed = page
        .commands()
        .into_iter()
        .filter(|command| command["method"] == "Input.insertText")
        .collect::<Vec<_>>();
    assert_eq!(typed.len(), UPN.chars().count() + PASSWORD.chars().count());
    let typed = typed
        .iter()
        .map(|command| command["params"]["text"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(typed, format!("{UPN}{PASSWORD}"));
}

#[tokio::test]
async fn mfa_and_failed_status_writes_stop_the_driver_without_retrying_submission() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let credentials = accounts.for_alias(Alias::A, UPN).unwrap();
    let status = StatusFile::new(dir.path(), Alias::A);
    let run = Run {
        upn: UPN,
        credentials,
        status: &status,
        options: LoginOptions {
            timeout: Duration::from_secs(1),
        },
    };
    let mut mfa = page(json!({"registration":true})).await;
    let session = mfa.session(dir.path(), false);
    assert!(matches!(
        run.sign_in_step(&session, &mut SignIn::default()).await,
        Err(LoginError::SignIn(SignInStop::MfaRegistration))
    ));
    assert_eq!(mfa.commands().len(), 1);
    fs::create_dir(&status.path).unwrap();
    let mut email = page(json!({"email":"#synthetic-email","next":true})).await;
    let session = email.session(dir.path(), false);
    let mut state = SignIn::default();
    assert!(matches!(
        run.sign_in_step(&session, &mut state).await,
        Err(LoginError::Status(_))
    ));
    assert!(state.progress.email_sent);
}

fn locations(root: &Path) -> LabLocations {
    let locations = LabLocations {
        config_file: root.join("lab.toml"),
        data_dir: root.join("data"),
        personal_data_dir: root.join("personal"),
    };
    let text = [
        ("a", UPN),
        ("b", "synthetic-b@example.test"),
        ("c", "synthetic-c@example.test"),
    ]
    .map(|(alias, upn)| format!("[[accounts]]\nalias='{alias}'\nupn='{upn}'\n"))
    .join("\n");
    fs::write(&locations.config_file, text).unwrap();
    locations
}

async fn managed_loaded(peer: &mut Page, root: &Path, oid: &str) -> (LabSession, u32) {
    let (mut session, pid) = peer.managed_session(root, true);
    peer.send(
        "Page.frameNavigated",
        json!({"frame":{"id":"synthetic-frame","url":TEAMS_URL}}),
    );
    peer.send("Network.requestWillBeSent",json!({"requestId":"loaded","request":{"method":"GET","url":"https://teams.microsoft.com/api/chatsvc/synthetic","headers":{}}}));
    peer.send(
        "Network.responseReceived",
        json!({"requestId":"loaded","response":{"status":200}}),
    );
    identity(peer, oid);
    assert_eq!(session.settle(Duration::from_secs(1)).await, SessionState::Loaded);
    assert!(session.pump(Duration::from_millis(10)).await);
    (session, pid)
}

#[tokio::test]
async fn login_cleanup_records_identity_only_after_the_real_owned_process_has_closed() {
    let root = tempfile::tempdir().unwrap();
    let locations = locations(root.path());
    let accounts = accounts();
    let status = StatusFile::new(root.path(), Alias::A);
    let run = Run {
        upn: UPN,
        credentials: accounts.for_alias(Alias::A, UPN).unwrap(),
        status: &status,
        options: LoginOptions {
            timeout: Duration::from_secs(2),
        },
    };
    for expected in [MriRecord::Recorded, MriRecord::AlreadyRecorded] {
        let mut peer = page(json!({})).await;
        let (session, pid) = managed_loaded(&mut peer, root.path(), OID).await;
        let report = run.finish(session, &locations, Alias::A).await.unwrap();
        assert_eq!(report.mri, expected);
        assert!(!report.signed_in && report.refused.is_none());
        assert!(report.capture.unwrap().join(crate::capture::EVENTS_FILE).exists());
        assert!(crate::scenario::test_process::exited(pid));
        assert_eq!(peer.commands().last().unwrap()["method"], "Browser.close");
    }
    assert_eq!(
        LabConfig::load(&locations).unwrap().account(Alias::A).mri.as_deref(),
        Some(format!("8:orgid:{OID}").as_str())
    );
}

#[tokio::test]
async fn a_failed_identity_or_status_write_still_closes_the_process_before_returning_the_original_failure() {
    let root = tempfile::tempdir().unwrap();
    let locations = locations(root.path());
    let accounts = accounts();
    let status = StatusFile::new(root.path(), Alias::A);
    let run = Run {
        upn: UPN,
        credentials: accounts.for_alias(Alias::A, UPN).unwrap(),
        status: &status,
        options: LoginOptions {
            timeout: Duration::from_secs(2),
        },
    };
    let mut peer = page(json!({})).await;
    let (session, pid) = managed_loaded(&mut peer, root.path(), "not-a-guid").await;
    assert!(matches!(
        run.finish(session, &locations, Alias::A).await,
        Err(LoginError::BadIdentity(_))
    ));
    assert!(crate::scenario::test_process::exited(pid));
    assert!(LabConfig::load(&locations).unwrap().account(Alias::A).mri.is_none());
    fs::remove_file(&status.path).unwrap();
    fs::create_dir(&status.path).unwrap();
    let mut peer = page(json!({})).await;
    let (session, pid) = managed_loaded(&mut peer, root.path(), OID).await;
    assert!(matches!(
        run.finish(session, &locations, Alias::A).await,
        Err(LoginError::Status(_))
    ));
    assert!(crate::scenario::test_process::exited(pid));
}

#[test]
fn final_status_reports_done_refused_or_the_original_error_without_masking_failed_writes() {
    let root = tempfile::tempdir().unwrap();
    let status = StatusFile::new(root.path(), Alias::A);
    for refused in [None, Some(403)] {
        let report = LoginReport {
            signed_in: false,
            refused,
            mri: MriRecord::Recorded,
            browser: SessionReport::default(),
            capture: None,
        };
        assert!(finish_status(&status, Ok(report)).is_ok());
        let record: Value = serde_json::from_str(&fs::read_to_string(&status.path).unwrap()).unwrap();
        assert_eq!(
            record["state"],
            if refused.is_some() { "teams-refused" } else { "done" }
        );
        if refused.is_some() {
            assert_eq!(record["authz_status"], 403);
        }
    }
    assert!(matches!(
        finish_status(&status, Err(LoginError::NoIdentity)),
        Err(LoginError::NoIdentity)
    ));
    assert_eq!(
        serde_json::from_str::<Value>(&fs::read_to_string(&status.path).unwrap()).unwrap()["state"],
        "failed"
    );
    fs::remove_file(&status.path).unwrap();
    fs::create_dir(&status.path).unwrap();
    assert!(matches!(
        finish_status(&status, Err(LoginError::FieldGone)),
        Err(LoginError::FieldGone)
    ));
    let report = LoginReport {
        signed_in: false,
        refused: None,
        mri: MriRecord::Recorded,
        browser: SessionReport::default(),
        capture: None,
    };
    assert!(matches!(finish_status(&status, Ok(report)), Err(LoginError::Status(_))));
}

#[tokio::test]
async fn native_authorization_uses_the_dedicated_driver_and_closes_before_returning_a_secret() {
    use teams_auth::secrets::Secret;
    use teams_auth::token::{Authorization, EntraConfig, Scope};
    let root = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let credentials = accounts.for_alias(Alias::A, UPN).unwrap();
    let authorization = Authorization::new(&EntraConfig::teams_native(), UPN, Scope::spaces()).unwrap();
    let status = StatusFile::new(root.path(), Alias::A);
    let (send, mut caught) = tokio::sync::mpsc::unbounded_channel();
    let mut page = Page::new(move |command| match command["method"].as_str().unwrap() {
        "Page.navigate" | "Browser.close" | "Input.insertText" => Ok(json!({})),
        "Runtime.evaluate" => {
            let expression = command["params"]["expression"].as_str().unwrap();
            if expression == PROBE {
                Ok(evaluated(json!({"password":"#synthetic-password"})))
            } else {
                if expression == PRESS_NEXT {
                    send.send(Secret::new("synthetic-redirect-secret")).unwrap();
                }
                Ok(evaluated(json!(true)))
            }
        }
        method => Err(format!("unexpected synthetic authorization command {method}")),
    })
    .await;
    let (session, pid) = page.managed_session(root.path(), false);
    page.send(
        "Page.frameNavigated",
        json!({"frame":{"id":"synthetic-auth","url":authorization.url()}}),
    );
    page.send("Page.frameStoppedLoading", json!({"frameId":"synthetic-auth"}));
    let (redirect, report) = authorization_session(
        session,
        &authorization,
        credentials,
        &status,
        Duration::from_secs(5),
        &mut caught,
    )
    .await
    .unwrap();
    assert_eq!(redirect.expose(), "synthetic-redirect-secret");
    assert!(!report.failed());
    assert!(crate::scenario::test_process::exited(pid));
    let commands = page.commands();
    assert_eq!(
        commands.iter().filter(|c| c["method"] == "Input.insertText").count(),
        credentials.password().expose().chars().count()
    );
    assert_eq!(
        commands
            .iter()
            .filter(|c| c["params"]["expression"] == PRESS_NEXT)
            .count(),
        1
    );
    assert!(
        commands
            .iter()
            .filter(|c| c["method"] == "Page.navigate")
            .all(|c| c["params"]["url"] == authorization.url())
    );
    assert!(
        !std::fs::read_to_string(root.path().join("status-a.json"))
            .unwrap()
            .contains("synthetic-redirect-secret")
    );
}

const REPROCESS: &str = "https://login.microsoftonline.com/common/reprocess?ctx=synthetic";

fn reprocess(page: &Page, times: u32) {
    for _ in 0..times {
        page.send(
            "Page.frameNavigated",
            json!({"frame":{"id":"synthetic-frame","url":REPROCESS}}),
        );
        page.send("Page.frameStoppedLoading", json!({"frameId":"synthetic-frame"}));
    }
}

async fn cookie_page() -> Page {
    Page::new(|command| match command["method"].as_str().unwrap() {
        "Runtime.evaluate" => Ok(evaluated(json!({}))),
        "Network.getCookies" => Ok(json!({"cookies":[
            {"name":"synthetic-ests","domain":".login.microsoftonline.com","path":"/"},
            {"name":"synthetic-live","domain":"login.live.com","path":"/"},
            {"name":"synthetic-shared","domain":".microsoft.com","path":"/"},
            {"name":"synthetic-lookalike","domain":"notlogin.live.com","path":"/"},
            {"name":"synthetic-incomplete"}
        ]})),
        "Network.deleteCookies" | "Page.navigate" | "Storage.clearDataForOrigin" => Ok(json!({})),
        method => Err(format!("unexpected synthetic reprocess command {method}")),
    })
    .await
}

fn sent(page: &Page, method: &str) -> Vec<Value> {
    page.commands()
        .into_iter()
        .filter(|command| command["method"] == method)
        .map(|command| command["params"].clone())
        .collect()
}

#[tokio::test]
async fn a_reprocess_loop_deletes_only_the_sign_in_cookies_once_then_stops_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let status = StatusFile::new(dir.path(), Alias::A);
    let mut page = cookie_page().await;
    let mut session = page.session(dir.path(), false);
    let run = Run {
        upn: UPN,
        credentials: accounts.for_alias(Alias::A, UPN).unwrap(),
        status: &status,
        options: LoginOptions {
            timeout: Duration::from_millis(3500),
        },
    };
    let mut sign_in = SignIn::default();
    reprocess(&page, REPROCESS_LOOP - 1);
    assert!(matches!(
        run.until_loaded(&mut session, &mut sign_in).await,
        Err(LoginError::Timeout { .. })
    ));
    assert!(
        sent(&page, "Network.getCookies").is_empty(),
        "fewer reloads are no loop"
    );

    reprocess(&page, 1);
    assert!(matches!(
        run.until_loaded(&mut session, &mut sign_in).await,
        Err(LoginError::Timeout { .. })
    ));
    let deleted = sent(&page, "Network.deleteCookies");
    assert_eq!(
        deleted,
        [
            json!({"name":"synthetic-ests","domain":".login.microsoftonline.com","path":"/"}),
            json!({"name":"synthetic-live","domain":"login.live.com","path":"/"}),
        ]
    );
    assert_eq!(
        sent(&page, "Page.navigate"),
        [json!({"url": "about:blank"}), json!({"url": TEAMS_URL})]
    );
    let cleared = sent(&page, "Storage.clearDataForOrigin");
    assert_eq!(cleared.len(), SIGN_IN_HOSTS.len());
    let methods: Vec<_> = page
        .commands()
        .into_iter()
        .map(|command| command["method"].clone())
        .collect();
    let last_delete = methods.iter().rposition(|method| method == "Network.deleteCookies");
    let first_clear = methods.iter().position(|method| method == "Storage.clearDataForOrigin");
    assert!(
        last_delete < first_clear,
        "the cookies are counted before the clear removes them"
    );
    assert_eq!(
        cleared[0],
        json!({"origin":"https://login.microsoftonline.com","storageTypes":"all"})
    );
    let record: Value = serde_json::from_str(&fs::read_to_string(&status.path).unwrap()).unwrap();
    assert_eq!(record["state"], "sign-in-cookies-deleted");
    assert_eq!(record["cookies"], 2);

    reprocess(&page, REPROCESS_LOOP);
    assert!(matches!(
        run.until_loaded(&mut session, &mut sign_in).await,
        Err(LoginError::ReprocessLoop)
    ));
    assert_eq!(
        sent(&page, "Network.deleteCookies").len(),
        2,
        "the cookies go once only"
    );
    assert_eq!(sent(&page, "Page.navigate").len(), 2);
}

#[tokio::test]
async fn a_reprocess_loop_after_typing_stops_without_deleting_cookies() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let status = StatusFile::new(dir.path(), Alias::A);
    let mut page = cookie_page().await;
    let mut session = page.session(dir.path(), false);
    let run = Run {
        upn: UPN,
        credentials: accounts.for_alias(Alias::A, UPN).unwrap(),
        status: &status,
        options: LoginOptions {
            timeout: Duration::from_secs(2),
        },
    };
    let mut sign_in = SignIn {
        progress: Progress {
            email_sent: true,
            password_sent: false,
        },
        ..SignIn::default()
    };
    reprocess(&page, REPROCESS_LOOP);
    assert!(matches!(
        run.until_loaded(&mut session, &mut sign_in).await,
        Err(LoginError::ReprocessLoop)
    ));
    assert!(sent(&page, "Network.getCookies").is_empty());
    assert!(sent(&page, "Page.navigate").is_empty());
}

#[test]
fn the_reprocess_run_counts_only_unbroken_main_frame_commits_of_the_reprocess_page() {
    let page = SessionId::new("synthetic-page");
    let mut watch = crate::session::SessionWatch::new(page.clone());
    let commit = |watch: &mut crate::session::SessionWatch, url: &str, parent: bool| {
        let mut frame = json!({"id":"synthetic-frame","url":url});
        if parent {
            frame["parentId"] = json!("synthetic-parent");
        }
        watch.observe(&crate::cdp::Event {
            method: "Page.frameNavigated".into(),
            params: json!({ "frame": frame }),
            session_id: Some(page.clone()),
        });
    };
    commit(&mut watch, REPROCESS, false);
    commit(&mut watch, "https://login.live.com/Me.htm?v=3", true);
    commit(&mut watch, REPROCESS, false);
    assert_eq!(watch.reprocess_run(), 2, "a sub-frame doesn't break the run");
    commit(
        &mut watch,
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        false,
    );
    assert_eq!(watch.reprocess_run(), 0);
    commit(&mut watch, "https://outside.example.test/common/reprocess", false);
    assert_eq!(watch.reprocess_run(), 0, "only a sign-in host's page counts");
}

fn redirect(from: &str, location: &str) -> crate::cdp::Event {
    crate::cdp::Event {
        method: "Network.requestWillBeSent".into(),
        params: json!({
            "requestId":"synthetic-redirect",
            "request":{"method":"GET","url":"https://teams.cloud.microsoft/v2/authv2","headers":{}},
            "redirectResponse":{"url":from,"status":302,"headers":{"Location":location}}
        }),
        session_id: Some(SessionId::new("synthetic-frame-session")),
    }
}

const AUTHORIZE: &str = "https://login.microsoftonline.com/00000000-0000-4000-8000-000000000001/oauth2/v2.0/authorize";

#[test]
fn the_watch_keeps_only_the_error_and_aadsts_code_of_a_sign_in_redirect() {
    let mut watch = crate::session::SessionWatch::new(SessionId::new("synthetic-page"));
    for (from, location, expected) in [
        ("https://teams.cloud.microsoft/", "/v2/#error=login_required", None),
        (
            AUTHORIZE,
            "https://teams.cloud.microsoft/v2/authv2#code=synthetic",
            None,
        ),
        (
            AUTHORIZE,
            "https://teams.cloud.microsoft/v2/authv2?error=access_denied",
            Some("access_denied"),
        ),
        (
            AUTHORIZE,
            "https://teams.cloud.microsoft/v2/authv2#error=login_required&error_description=AADSTS500341%3A+The+user+account+synthetic%40example.test+has+been+deleted",
            Some("login_required AADSTS500341"),
        ),
        (
            AUTHORIZE,
            "/v2/authv2#error=bad%3Cname%3E&error_description=no+code+AADSTS",
            Some("badname"),
        ),
    ] {
        let mut fresh = crate::session::SessionWatch::new(SessionId::new("synthetic-page"));
        fresh.observe(&redirect(from, location));
        assert_eq!(fresh.sign_in_error(), expected, "{location}");
        watch.observe(&redirect(from, location));
    }
    assert_eq!(watch.sign_in_error(), Some("badname"), "the last error stands");
    watch.observe(&crate::cdp::Event {
        method: "Network.requestWillBeSent".into(),
        params: json!({"requestId":"plain","request":{"method":"GET","url":AUTHORIZE,"headers":{}}}),
        session_id: None,
    });
    assert_eq!(watch.sign_in_error(), Some("badname"));
}

#[tokio::test]
async fn a_timeout_names_the_sign_in_error_without_the_description() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = accounts();
    let status = StatusFile::new(dir.path(), Alias::A);
    let mut page = page(json!({})).await;
    let mut session = page.session(dir.path(), false);
    let redirected = redirect(
        AUTHORIZE,
        "https://teams.cloud.microsoft/v2/authv2#error=login_required&error_description=AADSTS500341%3A+synthetic%40example.test+deleted",
    );
    page.send(&redirected.method, redirected.params.clone());
    let run = Run {
        upn: UPN,
        credentials: accounts.for_alias(Alias::A, UPN).unwrap(),
        status: &status,
        options: LoginOptions {
            timeout: Duration::from_millis(800),
        },
    };
    let error = run
        .until_loaded(&mut session, &mut SignIn::default())
        .await
        .unwrap_err();
    assert!(
        matches!(&error, LoginError::SignInAnswer { answer, authz: None, .. } if answer == "login_required AADSTS500341")
    );
    let text = error.to_string();
    assert!(
        text.ends_with("answered login_required AADSTS500341 (no authz answer)"),
        "{text}"
    );
    assert!(!text.contains("example.test"));
}
