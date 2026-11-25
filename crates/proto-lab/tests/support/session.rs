//! Session setup and private state checks using only synthetic CDP peers.
use super::*;
use serde_json::json;

impl LabSession {
    /// Installs explicit synthetic identity/membership evidence; never available in a live harness build.
    pub(crate) fn set_action_targets(&mut self, targets: Arc<crate::action_targets::ActionTargets>) {
        self.watch.action_targets = Some(targets);
    }

    /// A session on an offline synthetic CDP peer, never a browser profile or a tenant account.
    #[cfg(test)]
    pub(crate) fn synthetic(armed: ArmedBrowser, page: SessionId, recorder: Option<Arc<Recorder>>) -> Self {
        Self {
            events: armed.watch(),
            watch: SessionWatch::new(page.clone()),
            armed,
            page,
            recorder,
        }
    }
}

fn token(claims: &Value) -> String {
    format!("synthetic.{}.synthetic", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

#[test]
fn identity_headers_reject_malformed_authorization_and_require_complete_claims() {
    for headers in [
        Value::Null,
        json!({}),
        json!({"Authorization":17}),
        json!({"Authorization":"Bearer"}),
        json!({"Authorization":"Basic synthetic"}),
        json!({"Authorization":"Bearer one.two"}),
        json!({"Authorization":"Bearer one.two.three.four"}),
        json!({"Authorization":"Bearer one.%.three"}),
        json!({"Authorization":"Bearer one.bm90LWpzb24.three"}),
    ] {
        assert!(identity_in(&headers).is_none(), "{headers}");
    }
    for claims in [
        json!({}),
        json!({"oid":"synthetic"}),
        json!({"oid":"synthetic","tid":"tenant"}),
        json!({"oid":17,"tid":"tenant","upn":"synthetic@example.com"}),
        json!({"oid":"synthetic","tid":17,"upn":"synthetic@example.com"}),
    ] {
        assert!(identity_in(&json!({"authorization":format!("Bearer {}",token(&claims))})).is_none());
    }
    for name in ["upn", "preferred_username", "unique_name"] {
        let mut claims = json!({"oid":"synthetic","tid":"tenant"});
        claims[name] = json!("synthetic@example.com");
        let found = identity_in(&json!({"AUTHORIZATION":format!(" bEaReR {} ",token(&claims))})).unwrap();
        assert_eq!(found.upn, "synthetic@example.com");
        assert_eq!(found.tenant, "tenant");
    }
}

#[test]
fn identity_consensus_rejects_distinct_objects_or_tenants_and_accepts_case_insensitive_upns() {
    for changed_tenant in [false, true] {
        let identity = Identity {
            oid: "synthetic-a".into(),
            tenant: "synthetic-tenant".into(),
            upn: "synthetic@example.com".into(),
        };
        let mut watch = SessionWatch::new(SessionId::new("synthetic-page"));
        watch.identities = vec![identity.clone(), identity.clone()];
        assert_eq!(watch.identity_for("SYNTHETIC@EXAMPLE.COM"), Some(&identity));
        assert!(watch.identity_for("missing@example.com").is_none());
        if changed_tenant {
            watch.identities[1].tenant = "different-tenant".into();
        } else {
            watch.identities[1].oid = "different-object".into();
        }
        assert!(watch.identity_for("synthetic@example.com").is_none());
    }
}

#[test]
fn only_exact_authz_and_service_endpoints_contribute_to_a_session() {
    for value in [
        "not a url",
        "about:blank",
        "https://teams.microsoft.com/api/authsvc/v1.0/authz?synthetic=1",
        "https://teams.microsoft.com/api/authsvc/v1.0/authz#synthetic",
        "http://teams.microsoft.com/api/authsvc/v1.0/authz",
    ] {
        assert!(!is_authz(value));
    }
    for value in [
        "not a url",
        "https://synthetic.example/api/chatsvc/",
        "https://teams.microsoft.com/api/chatsvc",
        "https://teams.microsoft.com/api/other/",
    ] {
        assert!(!is_service_read(value));
    }
    assert!(matches!(Place::of("not a URL"), Place::Blank));
    assert!(matches!(Place::of("about:blank"), Place::Blank));
    assert!(is_service_read("https://teams.cloud.microsoft/api/csa/synthetic"));
}

#[tokio::test]
async fn session_navigation_pumps_disconnects_and_a_real_confirmed_close_are_reported() {
    let mut page = crate::scenario::test_page::Page::new(|command| match command["method"].as_str().unwrap() {
        "Page.navigate" => Err("synthetic navigation refused".into()),
        "Browser.close" => Ok(json!({})),
        method => Err(format!("unexpected synthetic session command {method}")),
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let (mut session, pid) = page.managed_session(root.path(), false);
    assert!(session.navigate("https://synthetic.example").await.is_err());
    assert_eq!(session.page().as_str(), "synthetic-page");
    assert_eq!(session.settle(Duration::ZERO).await, SessionState::Loading);
    assert!(session.pump(Duration::ZERO).await);
    session.clear_holds();
    assert!(!session.report().failed());
    assert!(session.close().await.is_ok());
    assert!(crate::scenario::test_process::exited(pid));
}

#[test]
fn malformed_or_unsuccessful_responses_never_promote_an_unloaded_session() {
    let id = SessionId::new("synthetic-page");
    let key = (Some(id.clone()), "synthetic-request".to_owned());
    let mut watch = SessionWatch::new(id.clone());
    watch.navigated(&json!({"id":"main","url":TEAMS_URL}));
    watch.authz_requests.insert(key.clone(), "POST".into());
    for params in [
        json!({}),
        json!({"requestId":"synthetic-request"}),
        json!({"requestId":"synthetic-request","response":{"status":"200"}}),
        json!({"requestId":"synthetic-request","response":{"status":65536}}),
        json!({"requestId":"synthetic-request","response":{"status":500}}),
    ] {
        watch.response(&Event {
            session_id: Some(id.clone()),
            method: "Network.responseReceived".into(),
            params,
        });
        assert_eq!(watch.state(), SessionState::Loading);
    }
    assert_eq!(watch.last_authz_status(), Some(500));
    watch.authz_requests.insert(key.clone(), "GET".into());
    watch.response(&Event {
        session_id: Some(id.clone()),
        method: "Network.responseReceived".into(),
        params: json!({"requestId":"synthetic-request","response":{"status":403}}),
    });
    assert_eq!(watch.last_authz_status(), Some(500));
    watch.service_requests.insert(key);
    watch.response(&Event {
        session_id: Some(id),
        method: "Network.responseReceived".into(),
        params: json!({"requestId":"synthetic-request","response":{"status":404}}),
    });
    assert_eq!(watch.state(), SessionState::Loading);
}
