use super::*;
use std::future;

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-own-server-token")))
    }

    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}

struct Me;

impl crate::Read for Me {
    fn service(&self) -> Service {
        Service::Graph
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["me"])
    }
}

fn cloud() -> Cloud {
    Cloud::own("https://chat.example:8443").unwrap()
}

fn transport() -> HttpTransport<Tokens> {
    HttpTransport::for_cloud(Arc::new(Tokens), "https://chat.example:8443/api/chatsvc/amer", cloud())
        .unwrap()
        .with_middle_tier("https://chat.example:8443/api/mt/part/amer-03")
        .unwrap()
        .with_csa("https://chat.example:8443/api/csa/amer")
        .unwrap()
        .with_provisioning("https://chat.example:8443/fabric/amer/templates/api")
        .unwrap()
        .with_user_profile("https://chat.example:8443/api/userprofilesvc/amer")
        .unwrap()
        .with_presence("https://chat.example:8443/ups/noam")
        .unwrap()
        .with_mcps("https://chat.example:8443/api/mcps/us")
        .unwrap()
        .with_ams("https://chat.example:8443/ams")
        .unwrap()
        .with_loki()
}

#[test]
fn every_credentialed_service_stays_on_own_origin() {
    let transport = transport();
    transport.set_loki_region("https://chat.example:8443/loki").unwrap();
    let cases = [
        (
            Service::ChatService,
            vec!["v1", "users", "ME", "conversations"],
            "/api/chatsvc/amer/v1/users/ME/conversations",
        ),
        (
            Service::MiddleTier,
            vec!["beta", "users"],
            "/api/mt/part/amer-03/beta/users",
        ),
        (
            Service::Csa,
            vec!["api", "v3", "teams", "users", "me"],
            "/api/csa/amer/api/v3/teams/users/me",
        ),
        (Service::Provisioning, vec!["teams"], "/fabric/amer/templates/api/teams"),
        (Service::UserProfile, vec!["users"], "/api/userprofilesvc/amer/users"),
        (
            Service::Presence,
            vec!["v1", "me", "endpoints"],
            "/ups/noam/v1/me/endpoints/",
        ),
        (Service::Mcps, vec!["contents"], "/api/mcps/us/contents/"),
        (
            Service::Ams,
            vec!["v1", "objects", "picture", "views", "imgo"],
            "/ams/v1/objects/picture/views/imgo",
        ),
        (
            Service::Registrar,
            vec!["prod", "V2", "registrations"],
            "/registrar/prod/V2/registrations",
        ),
        (Service::Trouter, vec!["v4", "c"], "/trouter/v4/c"),
        (Service::Graph, vec!["me"], "/graph/v1.0/me"),
        (Service::Relay, vec!["tokens"], "/trap/tokens"),
        (
            Service::Substrate,
            vec!["searchservice", "api", "v2", "query"],
            "/search/searchservice/api/v2/query",
        ),
        (
            Service::LokiGlobal,
            CONFIGURATION_PATH.to_vec(),
            "/loki/api/v1/livepersonacard/configuration",
        ),
        (Service::Loki, vec!["person"], "/loki/person"),
        (Service::Workflows, vec!["flows"], "/workflows/flows"),
        (
            Service::WorkflowConnections,
            vec!["connections"],
            "/workflow-connections/connections",
        ),
    ];
    for (service, segments, path) in cases {
        let url = transport.url_for(service, &Endpoint::new(segments)).unwrap();
        assert_eq!(url.as_str(), format!("https://chat.example:8443{path}"), "{service:?}");
    }
}

#[test]
fn bases_refuse_other_clouds_ports_and_normalized_paths() {
    let tokens = Arc::new(Tokens);
    assert!(HttpTransport::for_cloud(tokens.clone(), "https://teams.microsoft.com/api/chatsvc/amer", cloud()).is_err());
    assert!(HttpTransport::new(tokens, "https://chat.example:8443/api/chatsvc/amer").is_err());
    let own = transport();
    for base in [
        "https://teams.microsoft.com/api/csa/amer",
        "https://chat.example/api/csa/amer",
        "https://chat.example:8443/api/csa/amer?",
        "https://u@chat.example:8443/api/csa/amer",
        "https://chat.example:8443/api/csa/x/../amer",
        "https://chat.example:8443/api/csa/AMER",
    ] {
        assert!(!own.accepts_csa_base_in(base), "{base}");
    }
    assert!(own.accepts_middle_tier_base_in("https://chat.example:8443/api/mt/part/amer-03/"));
    assert!(!own.accepts_middle_tier_base_in("https://teams.microsoft.com/api/mt/part/amer-03"));
    assert!(!own.accepts_presence_base_in("https://teams.microsoft.com/ups/noam"));
    assert!(!own.accepts_mcps_base_in("https://teams.microsoft.com/api/mcps/us"));
    assert!(!own.accepts_provisioning_base_in("https://teams.microsoft.com/fabric/amer/templates/api"));
    assert!(!own.accepts_user_profile_base_in("https://teams.microsoft.com/api/userprofilesvc/amer"));
    assert!(!own.accepts_ams_base_in("https://us-prod.asyncgw.teams.microsoft.com"));
    assert!(own.set_loki_region("https://region.loki.delve.office.com").is_err());
    assert!(own.set_loki_region("https://chat.example:8443/loki").is_ok());
}

#[test]
fn calling_links_are_bound_to_cloud_and_prefix() {
    for suffix in ["epconv", "ep/controller/conv/call/leave?i=one&e=two"] {
        let url = Url::parse(&format!("https://chat.example:8443/flightproxy/api/v2/{suffix}")).unwrap();
        assert!(calling_base_in(&cloud(), &url));
        assert!(!calling_base_in(&Cloud::Microsoft, &url));
    }
    for link in [
        "https://api.flightproxy.teams.microsoft.com/api/v2/epconv",
        "https://chat.example/flightproxy/api/v2/epconv",
        "https://chat.example:8443/api/v2/epconv",
        "https://elsewhere.example:8443/flightproxy/api/v2/epconv",
        "https://chat.example:8443/flightproxy/api/v2/epconv?token=x",
    ] {
        assert!(!calling_base_in(&cloud(), &Url::parse(link).unwrap()), "{link}");
    }
}

#[test]
fn only_public_cdns_may_leave_own_origin_without_credentials() {
    let transport = transport();
    let token = Secret::new("synthetic-token");
    for host in [EMOTICONS_HOST, GIPHY_HOST, APP_PICTURES_HOST, STATIC_APP_PICTURES_HOST] {
        let url = Url::parse(&format!("https://{host}/picture")).unwrap();
        assert!(transport.destination_allowed(&url, Auth::None));
        assert!(!transport.destination_allowed(&url, Auth::Bearer(&token)));
        assert!(!transport.destination_allowed(&url, Auth::Skypetoken(&token)));
    }
    assert!(!transport.destination_allowed(
        &Url::parse("https://teams.microsoft.com/trap/tokens").unwrap(),
        Auth::None
    ));
    assert!(transport.destination_allowed(
        &Url::parse("https://chat.example:8443/trap/tokens").unwrap(),
        Auth::Bearer(&token)
    ));
}

#[tokio::test]
async fn own_reads_use_checked_paths_through_loopback_override() {
    use crate::Transport as _;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/graph/v1.0/me"))
        .and(header("authorization", "Bearer synthetic-own-server-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"synthetic-user"})))
        .expect(1)
        .mount(&server)
        .await;
    let transport = HttpTransport::for_cloud_with_mock_origin(
        Arc::new(Tokens),
        "https://chat.example:8443/api/chatsvc/amer",
        cloud(),
        &server.uri(),
    )
    .unwrap();
    assert_eq!(transport.read(&Me).await.unwrap().status(), 200);
    let token = Secret::new("synthetic-token");
    let error = transport
        .send_as(
            reqwest::Method::GET,
            &Url::parse("https://graph.microsoft.com/v1.0/me").unwrap(),
            &[],
            Auth::Bearer(&token),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, HttpError::Refused(Refusal::NotAllowed)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
fn push_registration_refuses_off_origin_in_both_clouds() {
    use crate::{CallingRegistration, EndpointRegistration};
    let id = "00000000-0000-4000-8000-000000000001";
    let own = "https://chat.example:8443/trouter/v4/f/SyntheticEndpoint/";
    let microsoft = "https://go.trouter.teams.microsoft.com/v4/f/SyntheticEndpoint/";
    assert!(EndpointRegistration::messaging_in(&cloud(), id, own).is_some());
    assert!(CallingRegistration::new_in(&cloud(), id, own).is_some());
    assert!(EndpointRegistration::messaging_in(&cloud(), id, microsoft).is_none());
    assert!(CallingRegistration::new_in(&cloud(), id, microsoft).is_none());
    assert!(EndpointRegistration::messaging(id, own).is_none());
    assert!(CallingRegistration::new(id, own).is_none());
    for link in [
        "https://chat.example/trouter/v4/f/SyntheticEndpoint/",
        "https://chat.example:8443/trouter/v4/f/SyntheticEndpoint/?x=y",
        "https://chat.example:8443/trouter/v4/f/SyntheticEndpoint/#x",
    ] {
        assert!(
            EndpointRegistration::messaging_in(&cloud(), id, link).is_none(),
            "{link}"
        );
    }
}
