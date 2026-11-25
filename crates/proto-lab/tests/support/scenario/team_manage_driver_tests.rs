//! Real command dispatch and Unix socket framing against an offline CDP peer.

use serde_json::json;
use tokio::io::AsyncReadExt as _;

use super::*;
use crate::scenario::ab_chat_full::tests::page_with;

#[tokio::test]
async fn commands_dispatch_keys_typing_navigation_and_observation() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = page_with(false, false, |command, _| {
        (command["params"]["expression"] == "synthetic failure").then(|| Err("synthetic evaluation failed".into()))
    })
    .await;
    let session = page.session(temp.path(), true);
    let mut drive = session.driver();
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let mut target = StepPage::new(Alias::A, &session, &mut drive);
    for (command, expected) in [
        ("wait 0", "waited"),
        ("wait invalid", "waited"),
        ("press enter", "pressed"),
        ("press backspace", "pressed"),
        ("press space", "pressed"),
        ("press tab", "pressed"),
        ("press shift+tab", "pressed"),
        ("press other", "pressed"),
        ("go https://synthetic.invalid/", "loaded"),
        ("js true", "true"),
        ("click button Synthetic", "true"),
        ("click button", "true"),
        ("point #synthetic", "clicked"),
        ("hover #synthetic", "hovered"),
        ("type #synthetic text with spaces", "typed"),
        ("type #synthetic", "typed"),
        ("insert text with spaces", "inserted"),
        ("xy 12 34", "clicked"),
        ("xy", "clicked"),
        ("wheel 56 78 90", "scrolled"),
        ("wheel many", "scrolled"),
        ("what", "unknown step what"),
    ] {
        assert_eq!(
            step(&mut target, &mut report, command, &TEAM_SERVICES).await.unwrap(),
            expected
        );
    }
    let shot = step(&mut target, &mut report, "shot synthetic.png", &TEAM_SERVICES)
        .await
        .unwrap();
    assert!(shot.contains("synthetic.png"));
    assert!(session.capture_dir().unwrap().join("synthetic.png").exists());
    assert!(
        step(&mut target, &mut report, "js synthetic failure", &TEAM_SERVICES)
            .await
            .is_err()
    );
    let commands = page.commands();
    for key in ["Enter", "Backspace", " ", "Escape"] {
        assert!(commands.iter().any(|command| command["params"]["key"] == key));
    }
    assert!(
        commands
            .iter()
            .any(|command| command["params"]["key"] == "Tab" && command["params"]["modifiers"] == 8)
    );
    assert!(
        commands
            .iter()
            .any(|command| command["params"]["text"] == "text with spaces")
    );
    // a point's missing or unreadable numbers are 0
    let mouse = |kind: &str, x: f64, y: f64| {
        commands.iter().any(|command| {
            command["method"] == "Input.dispatchMouseEvent"
                && command["params"]["type"] == kind
                && command["params"]["x"] == x
                && command["params"]["y"] == y
        })
    };
    assert!(mouse("mousePressed", 12.0, 34.0) && mouse("mouseReleased", 12.0, 34.0));
    assert!(mouse("mousePressed", 0.0, 0.0));
    assert!(commands.iter().any(|command| {
        command["params"]["type"] == "mouseWheel"
            && command["params"]["x"] == 56.0
            && command["params"]["deltaY"] == 90.0
    }));
    assert!(commands.iter().any(|command| {
        command["params"]["type"] == "mouseWheel" && command["params"]["x"] == 0.0 && command["params"]["deltaY"] == 0.0
    }));
}

#[test]
fn the_held_back_team_writes_are_every_method_but_a_read_on_team_urls() {
    let rules = team_rules().unwrap();
    assert_eq!(rules.len(), WRITE_METHODS.len());
    assert!(rules.iter().all(|rule| rule.label() == Some(TEAM_WRITE)));
    let request = |method: &str, url: &str| crate::capture_hold::PausedRequest {
        request_id: "synthetic".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        body: None,
        network_id: None,
    };
    let held = |request: &crate::capture_hold::PausedRequest| rules.iter().any(|rule| rule.matches(request));
    for method in WRITE_METHODS {
        assert!(held(&request(
            method,
            "https://teams.cloud.microsoft/api/mt/part/amer-02/beta/teams/"
        )));
        assert!(held(&request(
            method,
            "https://teams.cloud.microsoft/api/csa/amer/api/v1/teams/channels"
        )));
    }
    assert!(!held(&request(
        "GET",
        "https://teams.cloud.microsoft/api/mt/part/amer-02/beta/teams/"
    )));
    assert!(!held(&request(
        "POST",
        "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations"
    )));
}

#[tokio::test]
async fn socket_accepts_multiple_connections_reports_errors_and_quits_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("steps.sock");
    std::fs::write(&socket, "stale socket").unwrap();
    let mut page = page_with(false, false, |command, _| {
        (command["params"]["expression"] == "fail").then(|| Err("synthetic failure".into()))
    })
    .await;
    let mut session = page.session(temp.path(), false);
    let mut drive = session.driver();
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let client = async {
        loop {
            match UnixStream::connect(&socket).await {
                Ok(mut stream) => {
                    stream.write_all(b"js fail\nunknown\n").await.unwrap();
                    stream.shutdown().await.unwrap();
                    let mut answer = String::new();
                    stream.read_to_string(&mut answer).await.unwrap();
                    assert!(answer.starts_with("failed:"));
                    assert!(answer.contains("unknown step unknown"));
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream.write_all(b"shot absent.png\nquit\n").await.unwrap();
        stream.shutdown().await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        assert_eq!(answer, "no screenshot\nbye\n");
    };
    let options = RunOptions {
        load_timeout: Duration::from_secs(2),
        display: crate::browser::Display::Headless,
        call: crate::scenario::ab_call::CallStep::Answer,
    };
    let (server, ()) = tokio::join!(
        steps(Alias::A, &mut session, &mut drive, &mut report, &socket, options),
        client
    );
    server.unwrap();
    let error = serve(
        Alias::A,
        &mut session,
        &mut drive,
        &mut report,
        &temp.path().join("absent/steps.sock"),
        &TEAM_SERVICES,
    )
    .await;
    assert!(error.is_err());
}

#[tokio::test]
async fn step_holds_apply_body_conditions_and_keep_unmatched_requests_running() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = page_with(false, false, |_, _| None).await;
    let session = page.session(temp.path(), false);
    for rule in [
        "POST /positive +needle",
        "PUT /negative -needle",
        "/default",
        "PATCH /plain",
    ] {
        assert_eq!(hold(&session, rule).unwrap(), "held");
    }
    assert!(hold(&session, "POST [").is_err());
    for (id, method, path, body) in [
        (1, "POST", "positive", "needle"),
        (2, "POST", "positive", "other"),
        (3, "PUT", "negative", "other"),
        (4, "PUT", "negative", "needle"),
        (5, "POST", "default", ""),
        (6, "PATCH", "plain", ""),
    ] {
        page.send("Fetch.requestPaused", json!({"requestId":format!("paused-{id}"),"networkId":format!("synthetic-{id}"),"resourceType":"XHR","request":{"method":method,"url":format!("https://synthetic.invalid/{path}"),"postData":body}}));
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.held_requests().len() < 4
            || page
                .commands()
                .iter()
                .filter(|command| {
                    matches!(
                        command["method"].as_str(),
                        Some("Fetch.failRequest" | "Fetch.continueRequest")
                    )
                })
                .count()
                < 6
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(session.held_requests().len(), 4);
    assert!(
        session
            .held_requests()
            .iter()
            .all(|request| request.label.as_deref() == Some(HELD_BY_A_STEP))
    );
}

fn emit(page: &crate::scenario::test_page::Page, id: usize, method: &str, host: &str, body: Option<&str>) {
    let mut request = json!({"method":method,"url":format!("https://{host}/api/mt/teams/synthetic?private=hidden")});
    if let Some(body) = body {
        request["postData"] = json!(body);
    }
    page.events
        .send(crate::cdp::Event {
            session_id: None,
            method: "Network.requestWillBeSent".into(),
            params: json!({"requestId":format!("seen-{id}"),"request":request}),
        })
        .unwrap();
    page.events
        .send(crate::cdp::Event {
            session_id: None,
            method: "Network.responseReceived".into(),
            params: json!({"requestId":format!("seen-{id}"),"response":{"status":200}}),
        })
        .unwrap();
}

#[tokio::test]
async fn seen_filters_services_and_tokens_advances_its_cursor_and_reads_missing_bodies() {
    let mut page = page_with(false, false, |command, _| {
        (command["method"] == "Network.getRequestPostData").then(|| Ok(json!({"postData":"recovered body"})))
    })
    .await;
    for (id, method, host, body) in [
        (1, "GET", "teams.cloud.microsoft", Some("never show read bodies")),
        (2, "OPTIONS", "teams.cloud.microsoft", None),
        (3, "HEAD", "teams.cloud.microsoft", None),
        (4, "POST", "other.invalid", Some("foreign private body")),
        (5, "POST", "teams.cloud.microsoft", Some("ordinary body")),
        (6, "PUT", "teams.cloud.microsoft", Some("ACCESS_TOKEN synthetic secret")),
        (7, "PATCH", "teams.cloud.microsoft", Some("Bearer synthetic secret")),
        (8, "DELETE", "teams.cloud.microsoft", Some("eyJ synthetic secret")),
        (9, "POST", "teams.cloud.microsoft", None),
    ] {
        emit(&page, id, method, host, body);
    }
    let mut seen = 0;
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let answer = writes_seen(&mut page.driver, &mut report, &mut seen, &TEAM_SERVICES).await;
    assert_eq!(answer.lines().count(), 6);
    assert!(answer.contains("ordinary body"));
    assert!(answer.contains("recovered body"));
    assert_eq!(answer.matches("left out").count(), 3);
    for hidden in ["synthetic secret", "private=hidden", "foreign private", "never show"] {
        assert!(!answer.contains(hidden));
    }
    assert_eq!(report.notes.len(), 5);
    assert!(
        writes_seen(&mut page.driver, &mut report, &mut seen, &TEAM_SERVICES)
            .await
            .is_empty()
    );
    seen = usize::MAX;
    assert!(
        writes_seen(&mut page.driver, &mut report, &mut seen, &TEAM_SERVICES)
            .await
            .is_empty()
    );
}

#[test]
fn account_selection_uses_only_the_child_environment() {
    for (value, expected) in [
        (None, "a"),
        (Some("a"), "a"),
        (Some("b"), "b"),
        (Some("c"), "c"),
        (Some("unknown"), "a"),
    ] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("scenario::team_manage::driver_tests::account_selection_child")
            .env_clear()
            .env("TEAM_EXPECTED_ALIAS", expected);
        if let Some(value) = value {
            command.env("PROTO_LAB_ACCOUNT", value);
        }
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn account_selection_child() {
    let Ok(expected) = std::env::var("TEAM_EXPECTED_ALIAS") else {
        return;
    };
    assert_eq!(account().to_string(), expected);
}

#[tokio::test]
async fn disconnected_socket_writer_and_missing_post_body_do_not_break_other_observations() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = page_with(false, false, |command, _| {
        (command["method"] == "Network.getRequestPostData").then(|| Err("body no longer available".into()))
    })
    .await;
    let session = page.session(temp.path(), false);
    let mut drive = session.driver();
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let mut target = StepPage::new(Alias::A, &session, &mut drive);
    assert_eq!(
        step(&mut target, &mut report, "hold POST /shape +synthetic", &TEAM_SERVICES)
            .await
            .unwrap(),
        "held"
    );
    assert_eq!(
        step(&mut target, &mut report, "seen", &TEAM_SERVICES).await.unwrap(),
        ""
    );
    let (server, mut client) = UnixStream::pair().unwrap();
    client.write_all(b"js true\n").await.unwrap();
    drop(client);
    assert!(!connection(std::slice::from_mut(&mut target), &mut report, server, &TEAM_SERVICES).await);
    emit(&page, 1, "POST", "teams.cloud.microsoft", None);
    let mut at = 0;
    let answer = writes_seen(&mut page.driver, &mut report, &mut at, &TEAM_SERVICES).await;
    assert!(answer.contains("POST"));
    assert!(!answer.contains("unavailable"));
    emit(&page, 2, "POST", "teams.cloud.microsoft", None);
    page.driver.set_call_timeout(Duration::ZERO);
    assert!(
        writes_seen(&mut page.driver, &mut report, &mut at, &TEAM_SERVICES)
            .await
            .contains("POST")
    );
}

#[tokio::test]
async fn a_step_addressed_to_an_account_goes_to_that_account_s_page() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = page_with(false, false, |_, _| None).await;
    let session = page.session(temp.path(), false);
    let (mut first, mut second) = (session.driver(), session.driver());
    let mut pages = [
        StepPage::new(Alias::A, &session, &mut first),
        StepPage::new(Alias::B, &session, &mut second),
    ];
    let (target, line) = addressed(&mut pages, "@b shot b.png");
    assert_eq!(target.map(|page| page.alias), Some(Alias::B));
    assert_eq!(line, "shot b.png");
    let (target, line) = addressed(&mut pages, "shot a.png");
    assert_eq!(target.map(|page| page.alias), Some(Alias::A));
    assert_eq!(line, "shot a.png");
    assert!(addressed(&mut pages, "@c js true").0.is_none());
    assert!(addressed(&mut pages, "@nobody js true").0.is_none());
}

fn on(page: &crate::scenario::test_page::Page, session: Option<&str>, method: &str, params: serde_json::Value) {
    page.events
        .send(crate::cdp::Event {
            session_id: session.map(crate::cdp::SessionId::new),
            method: method.into(),
            params,
        })
        .unwrap();
}

#[tokio::test]
async fn seen_names_the_target_of_each_request_and_how_it_ended() {
    let mut page = page_with(false, false, |_, _| None).await;
    page.driver.know_targets([(
        crate::cdp::SessionId::new("synthetic-frame"),
        crate::arming::TargetKind::Iframe,
    )]);
    on(
        &page,
        None,
        "Target.attachedToTarget",
        json!({"sessionId":"synthetic-worker","targetInfo":{"type":"worker","targetId":"synthetic-worker-target"}}),
    );
    let url = |id: &str| format!("https://teams.cloud.microsoft/api/chatsvc/synthetic/{id}?private=hidden");
    for (session, id, method) in [
        (Some("synthetic-worker"), "sent", "POST"),
        (Some("synthetic-worker"), "held", "POST"),
        (Some("synthetic-page"), "read", "GET"),
        (Some("synthetic-frame"), "framed", "PUT"),
        (Some("synthetic-stranger"), "stranger", "DELETE"),
        (None, "browser", "PATCH"),
    ] {
        on(
            &page,
            session,
            "Network.requestWillBeSent",
            json!({"requestId":id,"request":{"method":method,"url":url(id),"postData":"synthetic body"}}),
        );
    }
    on(
        &page,
        Some("synthetic-worker"),
        "Network.responseReceived",
        json!({"requestId":"sent","response":{"status":201}}),
    );
    on(
        &page,
        Some("synthetic-worker"),
        "Network.loadingFailed",
        json!({"requestId":"held","errorText":"net::ERR_BLOCKED_BY_CLIENT"}),
    );
    on(
        &page,
        Some("synthetic-page"),
        "Network.responseReceived",
        json!({"requestId":"read","response":{"status":200}}),
    );
    let mut seen = 0;
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let answer = writes_seen(&mut page.driver, &mut report, &mut seen, &TEAM_SERVICES).await;
    let lines: Vec<_> = answer.lines().collect();
    let expect = [
        "POST https://teams.cloud.microsoft/api/chatsvc/synthetic/sent?private -> 201 from worker",
        "POST https://teams.cloud.microsoft/api/chatsvc/synthetic/held?private -> failed net::ERR_BLOCKED_BY_CLIENT from worker",
        "GET https://teams.cloud.microsoft/api/chatsvc/synthetic/read?private -> 200 from page",
        "PUT https://teams.cloud.microsoft/api/chatsvc/synthetic/framed?private -> pending from iframe",
        "DELETE https://teams.cloud.microsoft/api/chatsvc/synthetic/stranger?private -> pending from unknown target",
        "PATCH https://teams.cloud.microsoft/api/chatsvc/synthetic/browser?private -> pending from browser",
    ];
    assert_eq!(lines.len(), expect.len(), "{answer}");
    for (line, expected) in lines.iter().zip(expect) {
        assert!(line.starts_with(expected), "{line} / {expected}");
    }
    assert!(!answer.contains("hidden"));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.ends_with("synthetic/sent?private from worker"))
    );
}

#[tokio::test]
async fn a_redirect_hop_keeps_its_own_status() {
    let mut page = page_with(false, false, |_, _| None).await;
    let url = |id: &str| format!("https://teams.cloud.microsoft/api/chatsvc/synthetic/{id}");
    on(
        &page,
        Some("synthetic-page"),
        "Network.requestWillBeSent",
        json!({"requestId":"hop","request":{"method":"POST","url":url("first")}}),
    );
    on(
        &page,
        Some("synthetic-page"),
        "Network.requestWillBeSent",
        json!({"requestId":"hop","request":{"method":"GET","url":url("second")},"redirectResponse":{"status":307}}),
    );
    on(
        &page,
        Some("synthetic-page"),
        "Network.responseReceived",
        json!({"requestId":"hop","response":{"status":200}}),
    );
    let mut seen = 0;
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let answer = writes_seen(&mut page.driver, &mut report, &mut seen, &TEAM_SERVICES).await;
    let lines: Vec<_> = answer.lines().collect();
    assert_eq!(lines.len(), 2, "{answer}");
    assert!(
        lines[0].starts_with("POST https://teams.cloud.microsoft/api/chatsvc/synthetic/first -> 307 from page"),
        "{answer}"
    );
    assert!(
        lines[1].starts_with("GET https://teams.cloud.microsoft/api/chatsvc/synthetic/second -> 200 from page"),
        "{answer}"
    );
}

#[test]
fn every_target_kind_has_its_cdp_name() {
    use crate::arming::TargetKind;
    for name in [
        "page",
        "iframe",
        "worker",
        "shared_worker",
        "service_worker",
        "worklet",
        "synthetic_other",
    ] {
        assert_eq!(TargetKind::from_type(name).name(), name);
    }
}
