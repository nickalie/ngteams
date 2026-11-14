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

#[tokio::test]
async fn presence_subscriptions_validate_the_push_origin_at_construction_and_send() {
    use crate::{Client, EndpointRegistration, Mri, PresenceSubscription, register};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    let transport = HttpTransport::for_cloud_with_mock_origin(
        Arc::new(Tokens),
        "https://chat.example:8443/api/chatsvc/amer",
        cloud(),
        &server.uri(),
    )
    .unwrap()
    .with_presence("https://chat.example:8443/ups/noam")
    .unwrap();
    let push = "https://chat.example:8443/trouter/v4/f/SyntheticEndpoint/";
    let id = "00000000-0000-4000-8000-000000000001";
    let registration = EndpointRegistration::messaging_in(&cloud(), id, push).unwrap();
    let (_, endpoint) = register(&transport, &registration).await.unwrap();
    let endpoint = endpoint.unwrap();
    let named = || [Mri::new("8:orgid:synthetic-b")];
    refuse_push_origins(&endpoint, push);
    Mock::given(method("POST"))
        .and(path(format!("/ups/noam/v1/pubsub/subscriptions/{id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"addStatuses": [], "removeStatuses": []})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    let client = Client::new(transport);
    let own = PresenceSubscription::new_in(&cloud(), &endpoint, push, true, named(), []).unwrap();
    assert_eq!(client.lookup(own.clone()).await.unwrap().status(), 200);
    let foreign = PresenceSubscription::new(
        &endpoint,
        "https://go.trouter.teams.microsoft.com/v4/f/SyntheticEndpoint/",
        true,
        named(),
        [],
    )
    .unwrap();
    assert!(matches!(client.lookup(foreign).await, Err(HttpError::Unknown)));
    let wrong_cloud = Cloud::own("https://other.example:8443").unwrap();
    let foreign = PresenceSubscription::new_in(
        &wrong_cloud,
        &endpoint,
        "https://other.example:8443/trouter/v4/f/SyntheticEndpoint/",
        true,
        named(),
        [],
    )
    .unwrap();
    assert!(matches!(client.lookup(foreign).await, Err(HttpError::Unknown)));
    let microsoft = Client::new(
        HttpTransport::with_mock_origin(
            Arc::new(Tokens),
            "https://teams.microsoft.com/api/chatsvc/amer",
            &server.uri(),
        )
        .unwrap()
        .with_presence("https://teams.microsoft.com/ups/noam")
        .unwrap(),
    );
    assert!(matches!(microsoft.lookup(own).await, Err(HttpError::Unknown)));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

fn refuse_push_origins(endpoint: &crate::RegisteredEndpoint, push: &str) {
    use crate::{Mri, PresenceSubscription};
    let named = || [Mri::new("8:orgid:synthetic-b")];
    assert!(PresenceSubscription::new(endpoint, push, true, named(), []).is_none());
    for bad in [
        "https://go.trouter.teams.microsoft.com/v4/f/SyntheticEndpoint/",
        "https://other.example:8443/trouter/v4/f/SyntheticEndpoint/",
        "https://chat.example/trouter/v4/f/SyntheticEndpoint/",
        "https://chat.example:8443/v4/f/SyntheticEndpoint/",
        "https://user@chat.example:8443/trouter/v4/f/SyntheticEndpoint/",
        "https://chat.example:8443/trouter/v4/f/SyntheticEndpoint/?q=x",
        "https://chat.example:8443/trouter/v4/f/SyntheticEndpoint/#fragment",
        "http://chat.example:8443/trouter/v4/f/SyntheticEndpoint/",
    ] {
        assert!(
            PresenceSubscription::new_in(&cloud(), endpoint, bad, true, named(), []).is_none(),
            "{bad}"
        );
    }
}

#[test]
fn message_media_links_are_scoped_to_the_cloud_and_keep_microsoft_defaults() {
    use crate::{LinkPreviewPicture, MessagePicture, sent_picture, sent_picture_in};

    let cloud = cloud();
    let image = sent_picture_in(&cloud, "synthetic-object", (32, 24)).unwrap();
    assert_eq!(
        image.src,
        "https://chat.example:8443/ams/v1/objects/synthetic-object/views/imgo"
    );
    assert_eq!(
        MessagePicture::of_in(&cloud, &image.src).unwrap().object(),
        "synthetic-object"
    );
    assert!(MessagePicture::of(&image.src).is_none());
    assert_eq!(
        sent_picture("x", (1, 1)),
        sent_picture_in(&Cloud::Microsoft, "x", (1, 1))
    );
    let preview = LinkPreviewPicture::card_picture_src_in(&cloud, "https://page.example/picture.png").unwrap();
    assert!(preview.starts_with("https://chat.example:8443/ams/urlp/"));
    assert_eq!(
        LinkPreviewPicture::of_in(&cloud, &preview).unwrap().picture(),
        "https://page.example/picture.png"
    );
    for src in [
        image.src.replace(":8443", ""),
        image.src.replace("https:", "http:"),
        image.src.replace("chat.example", "outside.example"),
        image.src.replace("/ams/", "/amsx/"),
        format!("{}?extra=1", image.src),
        format!("{}#fragment", image.src),
        "https://us-api.asm.skype.com/v1/objects/x/views/imgo".into(),
    ] {
        assert!(MessagePicture::of_in(&cloud, &src).is_none(), "{src}");
    }
    for src in [
        preview.replace(":8443", ""),
        preview.replace("/ams/", "/sp/"),
        preview.replace("chat.example", "outside.example"),
        preview.replace("chat.example:8443/ams", "urlp.asyncgw.teams.microsoft.com"),
        format!("{preview}&url=https://second.example"),
    ] {
        assert!(LinkPreviewPicture::of_in(&cloud, &src).is_none(), "{src}");
    }
}

#[test]
fn own_picture_compose_round_trips_and_refuses_a_different_account_origin() {
    use crate::chat::{MessageText, Node, RichText};

    let cloud = cloud();
    let image = crate::sent_picture_in(&cloud, "synthetic-object", (32, 24)).unwrap();
    let rich = RichText {
        nodes: vec![Node::Image(image)],
        truncated: false,
    };
    assert!(MessageText::new(rich.clone(), []).is_err());
    let text = MessageText::new_in(&cloud, rich, []).unwrap();
    let stored = serde_json::to_vec(&text).unwrap();
    let restored: MessageText = serde_json::from_slice(&stored).unwrap();
    assert_eq!(restored, text);
    assert!(
        restored
            .for_cloud(&Cloud::own("https://other.example").unwrap())
            .is_err()
    );
    assert!(
        !serde_json::to_string(&MessageText::plain("unchanged").unwrap())
            .unwrap()
            .contains("origin")
    );
}

#[test]
fn own_files_accept_only_the_selected_origin_and_file_prefix() {
    use crate::chat::{is_file_link_in, shared_files_in};
    use crate::{DriveSite, DriveWrite, FileDownload, FileThumbnails, UploadedFile};

    let cloud = cloud();
    let base = "https://chat.example:8443/sp/personal/lab_a/";
    let site = DriveSite::from_web_url_in(&cloud, base).unwrap();
    let upload = serde_json::json!({"id":"ABC1","name":"demo.txt","size":3,
        "webUrl":format!("{base}Documents/demo.txt"),"parentReference":{"driveId":"drive1"},
        "sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}});
    let file = UploadedFile::from_upload(&site, &serde_json::to_vec(&upload).unwrap()).unwrap();
    assert_eq!(
        UploadedFile::from_entry_in(&cloud, &file.entry()).unwrap().entry(),
        file.entry()
    );
    assert!(UploadedFile::from_entry(&file.entry()).is_none());
    assert_eq!(file.site_in(&cloud).unwrap(), site);
    assert!(
        DriveWrite::upload(
            &site,
            "demo.txt",
            b"abc".to_vec(),
            "8:orgid:00000000-0000-4000-8000-00000000000a"
        )
        .is_some()
    );
    assert!(
        DriveWrite::share_in(
            &cloud,
            &file,
            "chat",
            &[("peer".into(), "peer@example.test".into())],
            "8:orgid:00000000-0000-4000-8000-00000000000a"
        )
        .is_some()
    );
    let files = shared_files_in(&cloud, Some(&serde_json::json!([file.entry()])));
    assert_eq!(
        files.files[0].open_link(),
        Some("https://chat.example:8443/sp/personal/lab_a/Documents/demo.txt")
    );
    for link in [
        "https://chat.example/sp/personal/lab_a/demo.txt",
        "https://outside.example:8443/sp/personal/lab_a/demo.txt",
        "https://chat.example:8443/spx/personal/lab_a/demo.txt",
        "https://u@chat.example:8443/sp/personal/lab_a/demo.txt",
        "https://chat.example:8443/sp/personal/lab_a/demo.txt#fragment",
        "https://madeup-my.sharepoint.com/personal/lab_a/demo.txt",
    ] {
        assert!(!is_file_link_in(&cloud, link), "{link}");
        assert!(FileDownload::of_in(&cloud, link).is_none(), "{link}");
        assert!(FileThumbnails::of_in(&cloud, link).is_none(), "{link}");
        assert!(DriveSite::from_web_url_in(&cloud, link).is_none(), "{link}");
    }
    let entry = file.entry();
    let text = crate::chat::MessageText::plain("file")
        .unwrap()
        .with_files(vec![file])
        .for_cloud(&cloud)
        .unwrap();
    let restored: crate::chat::MessageText = serde_json::from_slice(&serde_json::to_vec(&text).unwrap()).unwrap();
    assert_eq!(restored.files()[0].entry(), entry);
}

#[tokio::test]
async fn own_graph_answers_and_transport_refuse_foreign_file_credentials() {
    use crate::{FileDownload, FileThumbnails};

    let transport = transport();
    let cloud = cloud();
    let share = "https://chat.example:8443/sp/personal/lab_a/demo.mp4";
    let download = FileDownload::of_in(&cloud, share).unwrap();
    let thumbnails = FileThumbnails::of_in(&cloud, share).unwrap();
    for url in [
        "https://outside.example/sp/file?tempauth=synthetic",
        "https://chat.example/sp/file?tempauth=synthetic",
        "https://chat.example:8443/graph/file?tempauth=synthetic",
        "https://madeup-my.sharepoint.com/file?tempauth=synthetic",
        "https://media.svc.ms/file?tempauth=synthetic",
    ] {
        let answer = Response::new(
            200,
            serde_json::to_vec(&serde_json::json!({"size":3,"@microsoft.graph.downloadUrl":url})).unwrap(),
        );
        assert!(download.read_answer(&answer).is_none());
        let answer = Response::new(
            200,
            serde_json::to_vec(&serde_json::json!({"value":[{"large":{"url":url}}]})).unwrap(),
        );
        assert!(thumbnails.read_answer(&answer).is_none());
    }
    let foreign = crate::FileContentLink::parse("https://madeup-my.sharepoint.com/file?tempauth=synthetic").unwrap();
    assert!(matches!(
        transport.file_range(&foreign, 0, 1).await,
        Err(HttpError::Refused(Refusal::NotAllowed))
    ));
    assert!(matches!(
        transport.file_stream(&foreign, 0, 1).await,
        Err(HttpError::Refused(Refusal::NotAllowed))
    ));
    let foreign = crate::ThumbnailLink::parse("https://media.svc.ms/picture?tempauth=synthetic").unwrap();
    assert!(matches!(
        transport.file_thumbnail(&foreign).await,
        Err(HttpError::Refused(Refusal::NotAllowed))
    ));
}

#[test]
fn own_uploaded_file_answers_and_shared_previews_cannot_escape_the_account() {
    let cloud = cloud();
    let base = "https://chat.example:8443/sp/personal/lab_a/";
    let site = crate::DriveSite::from_web_url_in(&cloud, base).unwrap();
    let mut upload = serde_json::json!({"id":"ABC1","name":"demo.png","size":3,
        "webUrl":format!("{base}Documents/demo.png"),"parentReference":{"driveId":"drive1"},
        "sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}});
    let file = crate::UploadedFile::from_upload(&site, &serde_json::to_vec(&upload).unwrap()).unwrap();
    let mut entry = file.entry();
    let image = crate::sent_picture_in(&cloud, "object", (32, 24)).unwrap();
    entry["filePreview"] = serde_json::json!({"previewUrl":image.src,"previewWidth":32,"previewHeight":24});
    let files = crate::chat::shared_files_in(&cloud, Some(&serde_json::json!([entry.clone()])));
    assert_eq!(files.files[0].preview.as_ref().unwrap().size, Some((32, 24)));
    for url in [
        "https://outside.example/sp/personal/lab_a/Documents/demo.png",
        "https://chat.example/sp/personal/lab_a/Documents/demo.png",
        "https://chat.example:8443/sp/personal/lab_b/Documents/demo.png",
        "https://chat.example:8443/sp/personal/lab_a/Documents/demo.png?share=anyone",
    ] {
        upload["webUrl"] = url.into();
        assert!(crate::UploadedFile::from_upload(&site, &serde_json::to_vec(&upload).unwrap()).is_none());
    }
    entry["filePreview"]["previewUrl"] = "https://us-api.asm.skype.com/v1/objects/object/views/imgo".into();
    let files = crate::chat::shared_files_in(&cloud, Some(&serde_json::json!([entry])));
    assert!(files.files[0].preview.is_none());
}
