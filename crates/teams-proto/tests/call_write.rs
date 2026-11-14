//! Typed call requests against synthetic HTTP and socket mocks: captured bodies, service credentials, retained tracker link binding, and typed delivery responses.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::Mri;
use teams_proto::{
    AcceptCall, AcceptParts, AcceptanceAcknowledgement, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AddParts,
    AddToCall, AnswerMedia, AnswerMediaParts, ApplyChannelParameters, AttachCall, AttachParts, CALL_START_URL,
    CALLING_BASE, CALLING_HOST, CallContext, CallLink, CallParticipant, CallWriteError, ChannelParameters, ClientInfo,
    Credentials, DecideLobby, DeclineCall, FrameSink, FrameText, FrameWriter, HttpError, HttpTransport,
    InvitationReceipt, Invitee, JoinCall, JoinMedia, JoinParts, LeaveCall, LeaveReason, Links, MediaContent,
    MediaDescription, MediaDescriptions, MeetingInfo, Refusal, RelayAnswerError, RelayTokens, RenegotiateMedia,
    ReportRinging, RespondError, Response, StartCall, StartParts, Transport, UpdateEndpointMetadata,
    UpdateEndpointState, UpdateMediaDescriptions, UpdateParticipantProperties, VoiceConsent,
};
use teams_proto::{AcceptTransfer, HoldCall, ResumeCall, TransferCall, Transferor};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const MEETING: &str = "19:meeting_MadeUpMeeting0001@thread.v2";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const OUTSIDER: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const GROUP: &str = "19:syntheticgroupchat0abc@thread.v2";
/// A channel of a team of the lab's own, other than its first.
const CHANNEL: &str = "19:SyntheticChannelThread@thread.tacv2";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const SKYPETOKEN: &str = "placeholder-skypetoken";
const CHAIN: &str = "00000000-0000-4000-8000-0000000000c1";
const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
/// A's and b's calling endpoints' push URLs.
const PUSH_A: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/";
const PUSH_B: &str = "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00001/";
const ENDPOINT_B: &str = "/v4/f/SyntheticEndpoint00001/";
const FOLLOW_UPS_FIXTURE: &str = include_str!("../../../tests/fixtures/calling/chat/follow_ups.json");

/// The flight proxy's link to `rest` on the conversation service.
fn conv(id: &str, rest: &str) -> String {
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!("/{rest}")
    };
    format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/{id}{rest}?i=0-0-0-0&e=1")
}

/// The flight proxy's link to the call controller's `<kind>/<call>/…/<op>`.
fn cc(kind: &str, call: &str, op: &str) -> String {
    format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/{kind}/{call}/1/x1/1/{op}?i=0-0-0-0")
}

fn link(url: &str) -> CallLink {
    CallLink::parse(url).unwrap_or_else(|| panic!("not a call link: {url}"))
}

/// The object id of a work account's or a bot's MRI.
fn object_id(mri: &str) -> &str {
    mri.strip_prefix("8:orgid:")
        .or_else(|| mri.strip_prefix("28:"))
        .unwrap()
}

/// What a's `add` of `invitee` into its call sends.
fn add_parts(invitee: &str) -> AddParts {
    AddParts {
        from: participant(LAB_A),
        invitee: Invitee {
            mri: Mri::new(invitee),
            participant_id: Some("00000000-0000-4000-8000-0000000000e4".into()),
        },
        chat_thread: format!("19:{}_{}@unq.gbl.spaces", object_id(LAB_A), object_id(invitee)),
        tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        links: Links::new().with(
            "addParticipantSuccess",
            callback(PUSH_A, "07070707", "conversation", "addParticipantSuccess"),
        ),
    }
}

/// A callback of `push`'s call agent: `<push>callAgent/<agent>/<segment>/<kind>/<name>/`.
fn callback(push: &str, segment: &str, kind: &str, name: &str) -> String {
    format!("{push}callAgent/00000000-0000-4000-8000-0000000000a1/{segment}/{kind}/{name}/")
}

fn participant(mri: &str) -> CallParticipant {
    CallParticipant {
        mri: Mri::new(mri),
        display_name: Some("Lab".into()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
        language_id: Some("en-us".into()),
    }
}

fn context() -> CallContext {
    CallContext::new(CHAIN).with_region(Some("amer".into()), Some("amer01".into()))
}

fn client() -> ClientInfo {
    ClientInfo {
        endpoint_capabilities: Some(1),
        client_endpoint_capabilities: Some(1),
        platform_name: Some("SyntheticNative/1".into()),
    }
}

/// An offer in Teams' dialect (its content type only matters here).
fn media(required_features: Option<&str>) -> MediaContent {
    MediaContent {
        required_features: required_features.map(str::to_owned),
        client_location: Some("ZZ".into()),
        media_leg_id: Some("SYNTHETICMEDIALEG0000000000000000".into()),
        ..MediaContent::new("v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\n", "application/sdp-ngc-1.0")
    }
}

fn start_parts(push: &str, from: &str, required_features: Option<&str>) -> StartParts {
    StartParts {
        from: participant(from),
        roster_update: callback(push, "0a0a0a0a", "conversation", "rosterUpdate"),
        conversation_links: Links::new()
            .with(
                "conversationEnd",
                callback(push, "0b0b0b0b", "conversation", "conversationEnd"),
            )
            .with(
                "conversationUpdate",
                callback(push, "0c0c0c0c", "conversation", "conversationUpdate"),
            ),
        call_links: Links::new()
            .with("progress", callback(push, "01010101", "call", "progress"))
            .with("mediaAnswer", callback(push, "02020202", "call", "mediaAnswer"))
            .with("acceptance", callback(push, "03030303", "call", "acceptance"))
            .with("end", callback(push, "05050505", "call", "end")),
        media_controller_links: Links::new().with(
            "controlVideoStreaming",
            callback(push, "06060606", "call", "controlVideoStreaming"),
        ),
        modalities: vec!["Audio".into()],
        media: media(required_features),
        client: client(),
    }
}

/// Placeholder tokens, with a skypetoken or without one.
struct Tokens {
    skypetoken: bool,
    rejected: Mutex<Vec<String>>,
}

impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(self.skypetoken.then(|| Secret::new(SKYPETOKEN))))
    }

    fn skypetoken_rejected(
        &self,
        rejected: &Secret,
    ) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        self.rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(rejected.expose().to_owned());
        future::ready(Ok(Some(Secret::new(SKYPETOKEN))))
    }
}

/// One account's call tracker and native transport, its calling service a mock.
struct Lab {
    server: MockServer,
    tracker: Arc<teams_call::Tracker>,
    tokens: Arc<Tokens>,
    transport: HttpTransport<Tokens>,
}

/// The tracker receives deliveries and the answers to this account's actual sends.
async fn lab(own: &str) -> Lab {
    lab_with(own, true).await
}

async fn lab_with(own: &str, skypetoken: bool) -> Lab {
    lab_of(own, skypetoken).await
}

async fn lab_of(own: &str, skypetoken: bool) -> Lab {
    let server = MockServer::start().await;
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(own)));
    let tokens = Arc::new(Tokens {
        skypetoken,
        rejected: Mutex::default(),
    });
    let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    Lab {
        server,
        tracker,
        tokens,
        transport,
    }
}

impl Lab {
    /// The mock answers `verb` at the path of `url` with `answer`.
    async fn answers(&self, verb: &str, url: &str, answer: Value) {
        let at = url::Url::parse(url).unwrap();
        Mock::given(method(verb))
            .and(path(at.path()))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer))
            .mount(&self.server)
            .await;
    }

    /// Sends `write` and gives the request the mock received.
    async fn send<R: teams_proto::Request + Send>(&self, write: R) -> (Response, Request) {
        let attempt = self.tracker.before(&write);
        let answer = self.transport.deliver(write).await.unwrap();
        self.tracker.answered(attempt, &answer);
        let received = self.server.received_requests().await.unwrap();
        (answer, received.last().cloned().unwrap())
    }

    /// Feeds a synthetic callback delivery to the tracker.
    fn deliver_trouter(&self, at: &str, body: &Value) {
        let path = url::Url::parse(at).unwrap().path().to_owned();
        self.tracker.observe_delivery(&delivery(7, &path, body));
    }
}

/// A Trouter delivery on the connection `calling` at `path`.
fn delivery(id: i64, path: &str, body: &Value) -> teams_call::Delivery {
    teams_call::Delivery {
        connection: "calling".into(),
        id,
        path: path.into(),
        trouter_request: Some(format!("request-{id}")),
        chain_id: Some("chain".into()),
        message_id: Some(format!("message-{id}")),
        body: serde_json::to_vec(body).unwrap(),
    }
}

/// The request's body as JSON.
fn body_of(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|value| value.to_str().ok())
}

/// The captured body of the fixture case named `name`.
fn captured(fixture: &str, name: &str) -> Value {
    let fixture: Value = serde_json::from_str(fixture).unwrap();
    fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("no case {name}"))["body"]
        .clone()
}

/// The conversation a start's answer gives, on `id`.
fn conversation_answer(id: &str) -> Value {
    json!({
        "conversationController": conv(id, ""),
        "links": {
            "updateEndpointMetadata": conv(id, "updateEndpointMetadata"),
            "leave": conv(id, "leave"),
            "addParticipantAndModality": conv(id, "add"),
        },
    })
}

/// Uses the same canonical tracker as the native calling transport.
async fn native_lab() -> Lab {
    use teams_proto::CallState as _;
    let mut lab = lab(LAB_A).await;
    assert!(lab.tracker.connected("calling", PUSH_A));
    lab.transport = lab
        .transport
        .for_account(Mri::new(LAB_A))
        .with_call_state(lab.tracker.clone());
    lab
}

fn invitee(mri: &str) -> Invitee {
    Invitee {
        mri: Mri::new(mri),
        participant_id: Some("00000000-0000-4000-8000-0000000000e3".into()),
    }
}

#[tokio::test]
async fn a_bound_call_cleanup_needs_no_identity_or_membership_lookup() {
    let lab = native_lab().await;
    lab.answers("POST", CALL_START_URL, conversation_answer("NativeProof"))
        .await;
    let start = StartCall::in_chat(&context(), &start_parts(PUSH_A, LAB_A, None), &[invitee(LAB_B)], None).unwrap();
    assert_eq!(lab.send(start).await.0.status(), 200);
    lab.server.reset().await;
    let leave = conv("NativeProof", "leave");
    Mock::given(method("POST"))
        .and(path(url::Url::parse(&leave).unwrap().path()))
        .respond_with(ResponseTemplate::new(204))
        .mount(&lab.server)
        .await;
    let write = LeaveCall::new(&context(), &link(&leave), &participant(LAB_A), LeaveReason::HungUp).unwrap();
    assert_eq!(lab.send(write).await.0.status(), 204);
    let requests = lab.server.received_requests().await.unwrap();
    assert_eq!(
        requests.len(),
        1,
        "cleanup must not depend on an identity or membership lookup"
    );
    assert_eq!(requests[0].method.as_str(), "POST");
}

#[tokio::test]
async fn the_canonical_tracker_refuses_unbound_cleanup() {
    let lab = native_lab().await;
    let write = LeaveCall::new(
        &context(),
        &link(&conv("Unrelated", "leave")),
        &participant(LAB_A),
        LeaveReason::HungUp,
    )
    .unwrap();
    assert!(matches!(
        lab.transport.deliver(write).await,
        Err(HttpError::Refused(Refusal::CallState))
    ));
    assert!(lab.server.received_requests().await.unwrap().is_empty());
}

/// A's test call, started and registered on the conversation `Synthetic0`.
async fn started_test_call(lab: &Lab) -> (Response, Request) {
    lab.answers("POST", CALL_START_URL, conversation_answer("Synthetic0"))
        .await;
    let start = StartCall::echo_test(&context(), &start_parts(PUSH_A, LAB_A, None), &Mri::new(ECHO)).unwrap();
    lab.send(start).await
}

#[tokio::test]
async fn the_test_call_start_goes_to_the_flight_proxy_with_the_skypetoken_and_registers_its_call() {
    let lab = lab(LAB_A).await;
    let metadata = UpdateEndpointMetadata::new(
        &context(),
        &link(&conv("Synthetic0", "updateEndpointMetadata")),
        &participant(LAB_A),
    )
    .unwrap();
    assert_eq!(
        teams_proto::Request::body(&metadata).unwrap()["participants"]["from"]["id"],
        LAB_A
    );

    let (answer, request) = started_test_call(&lab).await;

    assert_eq!(answer.status(), 200);
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/api/v2/epconv");
    assert_eq!(request.url.query(), None);
    assert_eq!(header(&request, "x-skypetoken"), Some(SKYPETOKEN));
    assert_eq!(
        header(&request, "authorization"),
        None,
        "the calling service takes no bearer token"
    );
    assert_eq!(header(&request, "x-microsoft-skype-chain-id"), Some(CHAIN));
    let message = header(&request, "x-microsoft-skype-message-id").unwrap();
    assert_eq!(message.len(), 36, "a fresh GUID: {message}");
    assert_eq!(header(&request, "ms-teams-region"), Some("amer"));
    assert_eq!(header(&request, "ms-teams-partition"), Some("amer01"));

    let body = body_of(&request);
    assert_eq!(body["conversationRequest"]["scenario"], "UserInitiatedTestCall");
    assert_eq!(body["participants"]["to"], json!([]));
    assert_eq!(body["groupChat"], Value::Null);
    assert_eq!(
        body["callInvitation"]["mediaContent"]["contentType"],
        "application/sdp-ngc-1.0"
    );
}

#[tokio::test]
async fn a_meetings_start_targets_its_chat_names_nobody_and_carries_its_organizer() {
    let meeting = MeetingInfo {
        organizer_id: "00000000-0000-4000-8000-00000000000b".into(),
        tenant_id: "00000000-0000-4000-8000-0000000000f1".into(),
    };
    let parts = start_parts(PUSH_A, LAB_A, None);
    let start = StartCall::in_meeting(&context(), &parts, MEETING, &meeting).unwrap();

    let lab = lab(LAB_A).await;
    lab.answers("POST", CALL_START_URL, conversation_answer("Synthetic1"))
        .await;
    let (_, request) = lab.send(start).await;

    let sent: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(sent["participants"]["to"], json!([]), "nobody rings");
    assert_eq!(sent["conversationRequest"]["suppressDialout"], json!(true));
    assert_eq!(sent["groupChat"], json!({"threadId": MEETING, "messageId": "0"}));
    assert_eq!(
        sent["meetingInfo"],
        json!({"tenantId": meeting.tenant_id, "organizerId": meeting.organizer_id, "meetingType": 1})
    );

    let group = StartCall::in_meeting(&context(), &parts, GROUP, &meeting);
    assert!(group.is_err(), "a group chat's isn't a meeting's start");
}

#[tokio::test]
async fn a_meeting_code_start_resolves_the_invitation_without_inventing_a_chat() {
    // Synthetic invitation; this test sends only to the local mock.
    let code = teams_proto::MeetingCode::typed("123456789012", "MadeUp").unwrap();
    let parts = start_parts(PUSH_A, LAB_A, None);
    let start = StartCall::by_code(&context(), &parts, &code).unwrap();
    let lab = lab(LAB_A).await;
    lab.answers("POST", CALL_START_URL, conversation_answer("SyntheticCode"))
        .await;
    let (_, request) = lab.send(start).await;
    let sent: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(sent["participants"]["to"], json!([]));
    assert_eq!(sent["conversationRequest"]["suppressDialout"], true);
    assert_eq!(sent["groupChat"], Value::Null);
    assert_eq!(sent["meetingInfo"], Value::Null);
    assert_eq!(
        sent["meetingData"],
        json!({
            "meetingCode": "123456789012", "passcode": "MadeUp",
            "meetingUrl": "https://teams.microsoft.com/meet/123456789012?p=MadeUp",
        })
    );
    assert_eq!(sent["meetingPreferences"], json!({"shouldResurrect":"resurrect"}));
    assert_eq!(
        sent["callInvitation"]["mediaContent"]["contentType"],
        "application/sdp-ngc-1.0"
    );
    for invalid in [
        teams_proto::MeetingCode {
            id: "wrong".into(),
            passcode: "MadeUp".into(),
        },
        teams_proto::MeetingCode {
            id: code.id,
            passcode: "a&b".into(),
        },
    ] {
        assert!(StartCall::by_code(&context(), &parts, &invalid).is_err());
    }
}

#[tokio::test]
async fn a_channels_meet_now_subscribes_on_its_team_then_enters_as_its_organizer() {
    let parts = start_parts(PUSH_A, LAB_A, None);
    let subscribe = StartCall::meet_now(&context(), &parts, CHANNEL).unwrap();

    let lab = lab(LAB_A).await;
    lab.answers("POST", CALL_START_URL, conversation_answer("Synthetic1"))
        .await;
    let (_, request) = lab.send(subscribe).await;

    let sent: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(sent.get("callInvitation"), None, "a subscription enters no call");
    assert_eq!(
        sent["conversationRequest"]["properties"]["enableGroupCallMeetupGeneration"],
        json!(true),
        "the service makes the meeting and posts it to the channel"
    );
    assert_eq!(sent["groupChat"], json!({"threadId": CHANNEL, "messageId": null}));
    assert_eq!(
        sent["meetingInfo"],
        Value::Null,
        "the meeting is new: nobody organizes it yet"
    );
    assert_eq!(sent["endpointState"]["endpointStateSequenceNumber"], json!(0));

    // the entry into the meeting it made: the account its organizer, no post named yet
    let entry = JoinParts {
        from: participant(LAB_A),
        roster_update: callback(PUSH_A, "40404040", "conversation", "rosterUpdate"),
        conversation_links: Links::new(),
        group_chat: CHANNEL.into(),
        message_id: None,
        meeting: Some(MeetingInfo {
            organizer_id: LAB_A["8:orgid:".len()..].into(),
            tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        }),
        posts_meetup: true,
        client: client(),
    };
    let join_media = JoinMedia {
        call_links: Links::new().with("acceptance", callback(PUSH_A, "43434343", "call", "acceptance")),
        media_controller_links: Links::new(),
        modalities: vec!["Audio".into(), "ScreenViewer".into()],
        media: media(None),
    };
    let enter = JoinCall::enter(&context(), &link(&conv("Synthetic1", "")), &entry, &join_media).unwrap();

    lab.answers("POST", &conv("Synthetic1", ""), conversation_answer("Synthetic1"))
        .await;
    let (_, request) = lab.send(enter).await;
    let sent: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(sent["groupChat"], json!({"threadId": CHANNEL, "messageId": null}));
    assert_eq!(
        sent["conversationRequest"]["properties"]["enableGroupCallMeetupGeneration"],
        json!(true)
    );

    for chat in [GROUP, MEETING] {
        assert!(
            StartCall::meet_now(&context(), &parts, chat).is_err(),
            "{chat} is no channel"
        );
    }
}

#[tokio::test]
async fn a_chat_call_start_carries_every_callee_and_its_group_chat() {
    let invitee = |mri: &str| Invitee {
        mri: Mri::new(mri),
        participant_id: Some("00000000-0000-4000-8000-0000000000e3".into()),
    };
    let parts = start_parts(PUSH_A, LAB_A, Some("nonByPass"));
    let cases = [
        (vec![invitee(LAB_B)], None),
        (vec![invitee(LAB_B), invitee(LAB_C)], Some(GROUP)),
        (vec![invitee(OUTSIDER)], None),
    ];
    for (callees, group) in cases {
        let start = StartCall::in_chat(&context(), &parts, &callees, group).unwrap();
        assert_eq!(start.echo(), None);
        let lab = lab(LAB_A).await;
        lab.answers("POST", CALL_START_URL, conversation_answer("Synthetic1"))
            .await;
        let (_, request) = lab.send(start).await;
        let sent = body_of(&request);
        let actual: Vec<_> = sent["participants"]["to"]
            .as_array()
            .unwrap()
            .iter()
            .map(|participant| participant["id"].as_str().unwrap())
            .collect();
        let expected: Vec<_> = callees.iter().map(|callee| callee.mri.as_str()).collect();
        assert_eq!(actual, expected);
        match group {
            Some(chat) => assert_eq!(sent["groupChat"]["threadId"], chat),
            None => assert!(sent["groupChat"].is_null()),
        }
    }
}

#[test]
fn a_start_outside_its_captured_shape_is_never_made() {
    let invitee = |mri: &str| Invitee {
        mri: Mri::new(mri),
        participant_id: None,
    };
    let parts = start_parts(PUSH_A, LAB_A, None);
    let unmapped = |result: Result<StartCall, CallWriteError>| matches!(result, Err(CallWriteError::Unmapped));
    assert!(unmapped(StartCall::in_chat(&context(), &parts, &[], None)), "no callee");
    assert!(
        unmapped(StartCall::in_chat(
            &context(),
            &parts,
            &[invitee(LAB_B), invitee(LAB_C)],
            None
        )),
        "two callees and no group chat"
    );
    assert!(
        unmapped(StartCall::in_chat(&context(), &parts, &[invitee(LAB_A)], None)),
        "the caller as its own callee"
    );
    assert!(
        unmapped(StartCall::in_chat(&context(), &parts, &[invitee(ECHO)], None)),
        "a bot as a chat call's callee"
    );
    assert!(
        unmapped(StartCall::in_chat(
            &context(),
            &parts,
            &[invitee(LAB_B)],
            Some("19:abc_def@unq.gbl.spaces")
        )),
        "a group chat id that isn't one"
    );
    let featured = start_parts(PUSH_A, LAB_A, Some("nonByPass"));
    assert!(
        unmapped(StartCall::echo_test(&context(), &featured, &Mri::new(ECHO))),
        "the test call's offer has no required features"
    );
    let mut odd = start_parts(PUSH_A, LAB_A, None);
    odd.call_links = odd
        .call_links
        .with("transfer", callback(PUSH_A, "09090909", "call", "transfer"));
    assert!(
        unmapped(StartCall::echo_test(&context(), &odd, &Mri::new(ECHO))),
        "a callback no capture showed in a start"
    );
}

#[tokio::test]
async fn the_echo_joins_through_an_add_on_the_registered_call_only() {
    let lab = lab(LAB_A).await;
    let add = |invitee: &str| AddToCall::invite(&context(), &link(&conv("Synthetic0", "add")), &add_parts(invitee));
    let echo = add(ECHO).unwrap();

    started_test_call(&lab).await;
    lab.answers("POST", &conv("Synthetic0", "add"), json!({})).await;
    let (_, request) = lab.send(echo).await;
    assert_eq!(
        request.url.path(),
        "/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/Synthetic0/add"
    );
    assert_eq!(
        request.url.query(),
        Some("i=0-0-0-0&e=1"),
        "the link's query as handed out"
    );

    let elsewhere = AddToCall::invite(
        &context(),
        &link(&conv("Synthetic0", "add")),
        &AddParts {
            chat_thread: GROUP.into(),
            ..add_parts(ECHO)
        },
    );
    assert!(
        matches!(elsewhere, Err(CallWriteError::Unmapped)),
        "an add names its own 1:1 chat only"
    );
}

/// A's test call, started, registered and accepted: the acceptance, at the start's own `acceptance` callback, binds
/// the call controller's links; the mock answers every follow-up. Its call controller's call id.
async fn accepted_test_call(lab: &Lab) -> &'static str {
    started_test_call(lab).await;
    // the acceptance, at the start's own `acceptance` callback, binds the call controller's links
    let call = "SyntheticCall0";
    lab.deliver_trouter(
        &callback(PUSH_A, "03030303", "call", "acceptance"),
        &json!({"callAcceptance": {"links": {
            "acknowledgement": cc("callAcceptance", call, "acknowledge"),
            "applyChannelParameters": cc("mcProxy", call, "applyChannelParameters"),
            "mediaRenegotiation": cc("active", call, "renegotiate"),
            "updateMediaDescriptions": cc("active", call, "updateMediaDescriptions"),
        }}}),
    );
    for (verb, url) in [
        ("POST", cc("callAcceptance", call, "acknowledge")),
        ("POST", cc("mcProxy", call, "applyChannelParameters")),
        ("POST", cc("active", call, "renegotiate")),
        ("POST", cc("active", call, "updateMediaDescriptions")),
        ("POST", cc("negotiations", call, "answer")),
        ("POST", cc("mediaAnswers", call, "acknowledge")),
        ("PUT", conv("Synthetic0", "updateEndpointMetadata")),
        ("POST", conv("Synthetic0", "leave")),
    ] {
        lab.answers(verb, &url, json!({})).await;
    }
    call
}

#[tokio::test]
async fn a_started_calls_follow_ups_pass_on_the_links_its_services_handed_out() {
    let lab = lab(LAB_A).await;
    let call = accepted_test_call(&lab).await;
    let ours = |segment: &str, name: &str| callback(PUSH_A, segment, "call", name);
    let acknowledge = AcknowledgeAcceptance::new(
        &context(),
        &link(&cc("callAcceptance", call, "acknowledge")),
        &Links::new().with("mediaRenegotiation", ours("0d0d0d0d", "mediaRenegotiation")),
    )
    .unwrap();
    let (_, request) = lab.send(acknowledge).await;
    assert!(request.url.path().ends_with("/acknowledge"));
    let parameters = ChannelParameters {
        mids: vec!["*".into()],
        media_parameter: "{}".into(),
    };
    lab.send(
        ApplyChannelParameters::new(
            &context(),
            &link(&cc("mcProxy", call, "applyChannelParameters")),
            &parameters,
        )
        .unwrap(),
    )
    .await;
    the_camera_goes_on_by_a_renegotiation_and_media_descriptions(&lab, call).await;
    the_media_negotiation_binds_its_answer_and_acknowledgement(&lab, call).await;
    lab.send(
        UpdateEndpointMetadata::new(
            &context(),
            &link(&conv("Synthetic0", "updateEndpointMetadata")),
            &participant(LAB_A),
        )
        .unwrap(),
    )
    .await;
    let leave = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic0", "leave")),
        &participant(LAB_A),
        LeaveReason::HungUp,
    )
    .unwrap();
    let (_, request) = lab.send(leave).await;
    assert_eq!(body_of(&request)["callTransactionEnd"]["code"], 0);
    let after = UpdateEndpointMetadata::new(
        &context(),
        &link(&conv("Synthetic0", "updateEndpointMetadata")),
        &participant(LAB_A),
    )
    .unwrap();
    assert_eq!(
        teams_proto::Request::body(&after).unwrap()["endpointMetadata"],
        json!({})
    );
}

/// The caller's renegotiation and media descriptions, on the links the acceptance bound.
async fn the_camera_goes_on_by_a_renegotiation_and_media_descriptions(lab: &Lab, call: &str) {
    let ours = |segment: &str, name: &str| callback(PUSH_A, segment, "call", name);
    let mut video = media(Some("nonByPass"));
    video.negotiation_tag = Some("tag;v_1".into());
    let renegotiate = RenegotiateMedia::new(
        &context(),
        &link(&cc("active", call, "renegotiate")),
        &participant(LAB_A),
        &["Audio".into(), "Video".into()],
        &Links::new()
            .with("mediaAnswer", ours("0e0e0e0e", "mediaAnswer"))
            .with("rejection", ours("0e0e0e0e", "rejection")),
        &video,
    )
    .unwrap();
    lab.send(renegotiate).await;
    let descriptions = MediaDescriptions {
        descriptions: vec![MediaDescription {
            mid: "1".into(),
            direction: "sendrecv".into(),
            label: Some("main-video".into()),
        }],
        request_id: Some(2),
    };
    let (_, request) = lab
        .send(
            UpdateMediaDescriptions::new(
                &context(),
                &link(&cc("active", call, "updateMediaDescriptions")),
                &descriptions,
                Some("tag;v_1"),
            )
            .unwrap(),
        )
        .await;
    assert_eq!(
        body_of(&request),
        json!({"UpdateMediaDescriptions": {"mediaDescriptions": {
            "descriptions": [{"mid": "1", "direction": "sendrecv", "label": "main-video"}],
            "negotiationTag": "tag;v_1",
            "requestId": 2,
        }}})
    );
}

/// A renegotiation delivered at the call's own `mediaRenegotiation` callback binds its `answer` link; the answer to
/// ours, at the `mediaAnswer` callback, binds its acknowledgement.
async fn the_media_negotiation_binds_its_answer_and_acknowledgement(lab: &Lab, call: &str) {
    let answer = AnswerMedia::new(
        &context(),
        &link(&cc("negotiations", call, "answer")),
        &AnswerMediaParts {
            sender: participant(LAB_A),
            modalities: vec!["Audio".into(), "Video".into()],
            links: Links::new().with(
                "mediaAcknowledgement",
                callback(PUSH_A, "0f0f0f0f", "call", "mediaAcknowledgement"),
            ),
            media_controller_links: Links::new(),
            media: media(None),
        },
    )
    .unwrap();

    lab.deliver_trouter(
        &callback(PUSH_A, "0d0d0d0d", "call", "mediaRenegotiation"),
        &json!({"mediaNegotiation": {"links": {"mediaAnswer": cc("negotiations", call, "answer")}}}),
    );
    lab.send(answer).await;
    lab.deliver_trouter(
        &callback(PUSH_A, "0e0e0e0e", "call", "mediaAnswer"),
        &json!({"mediaAnswer": {"links": {"mediaAcknowledgement": cc("mediaAnswers", call, "acknowledge")}}}),
    );
    let (_, request) = lab
        .send(AcknowledgeMediaAnswer::new(&context(), &link(&cc("mediaAnswers", call, "acknowledge"))).unwrap())
        .await;
    assert!(request.body.is_empty(), "a media acknowledgement has no body");
    // sent as the web client sends it: an empty JSON body, its length said (without, the call controller took 22 s)
    let header = |name: &str| {
        request
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(header("content-length").as_deref(), Some("0"));
    assert_eq!(header("content-type").as_deref(), Some("application/json"));
}

/// An invitation from a to b, delivered at b's calling endpoint: a 1:1 call's (107), or a group call's (109) in the
/// a–b–c chat.
fn invitation(group: bool, caller: &str) -> Value {
    let mut gp = json!({
        "callNotification": {
            "from": {"id": caller},
            "to": {"id": LAB_B},
            "links": {"attach": cc("forked", "SyntheticCall1", "attach")},
        },
        "conversationInvitation": {"conversationController": conv("Synthetic2", ""), "isMultiParty": group},
    });
    if group {
        gp["groupChat"] = json!({"threadId": GROUP, "messageId": "0"});
    }
    let gp = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&gp).unwrap());
    json!({"evt": if group { 109 } else { 107 }, "gp": gp})
}

fn attach_parts() -> AttachParts {
    AttachParts {
        from: participant(LAB_B),
        end: callback(PUSH_B, "0a0a0a0a", "call", "end"),
        roster_update: callback(PUSH_B, "0b0b0b0b", "conversation", "rosterUpdate"),
        conversation_links: Links::new().with(
            "conversationEnd",
            callback(PUSH_B, "0c0c0c0c", "conversation", "conversationEnd"),
        ),
        conversation: link(&conv("Synthetic2", "")),
        client: client(),
    }
}

/// B's answered-call links, as the attach's answer hands them out.
fn attach_answer() -> Value {
    json!({
        "callInvitation": {"links": {
            "progress": cc("incoming", "SyntheticCall1", "progress"),
            "acceptance": cc("incoming", "SyntheticCall1", "accept"),
            "callLeg": cc("incoming", "SyntheticCall1", "reject"),
        }},
        "additionalActionResponses": [{
            "url": conv("Synthetic2", ""),
            "output": {
                "conversationController": conv("Synthetic2", ""),
                "links": {
                    "updateEndpointState": conv("Synthetic2", "updateEndpointState"),
                    "updateParticipantProperties": conv("Synthetic2", "updateParticipantProperties"),
                    "leave": conv("Synthetic2", "leave"),
                },
            },
        }],
    })
}

/// The answered call's links, as the accept's answer hands them out (inline, as the captured acceptance's answer).
fn accept_answer() -> Value {
    json!({"callAcceptanceAcknowledgement": {"links": {
        "applyChannelParameters": cc("mcProxy", "SyntheticCall1", "applyChannelParameters"),
        "mediaRenegotiation": cc("active", "SyntheticCall1", "renegotiate"),
    }}})
}

/// B, offered a's invitation, attached and told the call it rings.
async fn ringing_callee() -> Lab {
    let lab = lab(LAB_B).await;
    lab.tracker
        .observe_delivery(&delivery(3, ENDPOINT_B, &invitation(false, LAB_A)));
    lab.answers("POST", &cc("forked", "SyntheticCall1", "attach"), attach_answer())
        .await;
    lab.answers("POST", &cc("incoming", "SyntheticCall1", "accept"), accept_answer())
        .await;
    for (verb, op) in [("POST", "progress"), ("DELETE", "reject")] {
        lab.answers(verb, &cc("incoming", "SyntheticCall1", op), json!({}))
            .await;
    }
    let attach = AttachCall::new(
        &context(),
        &link(&cc("forked", "SyntheticCall1", "attach")),
        &attach_parts(),
    )
    .unwrap();
    let (_, request) = lab.send(attach).await;
    let body = body_of(&request);
    assert_eq!(body["attach"]["requireMediaContent"], false, "an attach asks no media");
    assert_eq!(body["additionalActions"][0]["name"], "join");
    assert_eq!(body["additionalActions"][0]["url"], conv("Synthetic2", ""));
    assert_eq!(
        body["additionalActions"][0]["input"].get("callInvitation"),
        None,
        "roster only"
    );
    let ringing = ReportRinging::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "progress")),
        &participant(LAB_B),
    )
    .unwrap();
    lab.send(ringing).await;
    lab
}

/// B accepts the invitation it was offered: the accept's answer binds the answered call's media links (its
/// `applyChannelParameters` and `mediaRenegotiation`).
async fn the_accept_binds_the_answered_calls_media_links(lab: &Lab) {
    lab.answers(
        "POST",
        &cc("mcProxy", "SyntheticCall1", "applyChannelParameters"),
        json!({}),
    )
    .await;
    let parameters = ApplyChannelParameters::new(
        &context(),
        &link(&cc("mcProxy", "SyntheticCall1", "applyChannelParameters")),
        &ChannelParameters {
            mids: vec!["*".into()],
            media_parameter: "{}".into(),
        },
    )
    .unwrap();

    let call_link = |name: &str| callback(PUSH_B, "20202020", "call", name);
    let accept = AcceptCall::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &AcceptParts {
            accepted_by: participant(LAB_B),
            modalities: vec!["Audio".into()],
            call_links: Links::new().with("mediaRenegotiation", call_link("mediaRenegotiation")),
            media_controller_links: Links::new().with("controlVideoStreaming", call_link("controlVideoStreaming")),
            media: media(Some("nonByPass")),
            client: client(),
        },
    )
    .unwrap();
    let (_, request) = lab.send(accept).await;
    let body = body_of(&request);
    assert_eq!(body["callAcceptance"]["acceptedBy"]["id"], LAB_B);
    assert_eq!(body["callAcceptance"]["callKeepAliveInterval"], Value::Null);
    assert_eq!(body["callAcceptance"]["mediaContent"]["requiredFeatures"], "nonByPass");
    // the accept's answer bound the answered call's media links

    let (_, request) = lab.send(parameters).await;
    assert!(request.url.path().ends_with("/applyChannelParameters"));
    let renegotiate = RenegotiateMedia::new(
        &context(),
        &link(&cc("active", "SyntheticCall1", "renegotiate")),
        &participant(LAB_B),
        &["Audio".into(), "Video".into()],
        &Links::new().with("mediaAnswer", call_link("mediaAnswer")),
        &media(Some("nonByPass")),
    )
    .unwrap();
    assert_eq!(
        teams_proto::Request::body(&renegotiate).unwrap()["mediaNegotiation"]["callModalities"],
        json!(["Audio", "Video"])
    );
}

#[tokio::test]
async fn an_offered_invitation_is_attached_answered_and_left_on_its_own_links() {
    let lab = ringing_callee().await;
    for op in ["updateEndpointState", "updateParticipantProperties", "leave"] {
        lab.answers("POST", &conv("Synthetic2", op), json!({})).await;
    }
    let state = UpdateEndpointState::new(
        &context(),
        &link(&conv("Synthetic2", "updateEndpointState")),
        &participant(LAB_B),
        false,
        1,
    )
    .unwrap();
    lab.send(state).await;
    the_accept_binds_the_answered_calls_media_links(&lab).await;
    let consent = VoiceConsent {
        value: "0".into(),
        sequence: 1,
    };
    let properties = UpdateParticipantProperties::new(
        &context(),
        &link(&conv("Synthetic2", "updateParticipantProperties")),
        &participant(LAB_B),
        &consent,
        &callback(PUSH_B, "30303030", "conversation", "updateParticipantPropertiesStatus"),
        "00000000-0000-4000-8000-0000000000f0",
    )
    .unwrap();
    lab.send(properties).await;
    let leave = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic2", "leave")),
        &participant(LAB_B),
        LeaveReason::HungUp,
    )
    .unwrap();
    lab.send(leave).await;
}

#[tokio::test]
async fn a_decline_is_the_captured_rejection_of_an_offered_invitation() {
    let lab = ringing_callee().await;
    let decline = DeclineCall::new(&context(), &link(&cc("incoming", "SyntheticCall1", "reject"))).unwrap();

    let (_, request) = lab.send(decline).await;
    assert_eq!(request.method.as_str(), "DELETE");
    assert_eq!(
        body_of(&request),
        captured(
            FOLLOW_UPS_FIXTURE,
            "the callee declines (the attach answer's callLeg link)"
        )
    );
    let again = AcceptCall::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &AcceptParts {
            accepted_by: participant(LAB_B),
            modalities: vec!["Audio".into()],
            call_links: Links::new(),
            media_controller_links: Links::new(),
            media: media(None),
            client: client(),
        },
    )
    .unwrap();
    assert_eq!(
        teams_proto::Request::body(&again).unwrap()["callAcceptance"]["acceptedBy"]["id"],
        LAB_B
    );
}

/// A transport of `lab`'s tokens whose every service is a closed port: each write it sends gets a network
/// error, no answer.
fn unreachable(lab: &Lab) -> HttpTransport<Tokens> {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    HttpTransport::with_mock_origin(
        Arc::clone(&lab.tokens),
        CHAT_BASE,
        &format!("http://127.0.0.1:{closed}"),
    )
    .unwrap()
}

#[tokio::test]
async fn an_attach_its_service_didn_t_take_leaves_its_invitation_to_the_next_attempt() {
    // a 401 (the skypetoken renewed for the next attempt), a throttle, a refusal and no answer at all: the transport
    // binds nothing and retires nothing; whether another attempt goes, or the invitation is let go, is the call
    // machine's to say (`Tracker::let_go`)
    for failure in [401, 503, 404, 0] {
        let lab = lab_of(LAB_B, true).await;
        let invited = delivery(3, ENDPOINT_B, &invitation(false, LAB_A));
        assert_eq!(lab.tracker.observe_delivery(&invited), teams_call::Observed::Offered);
        let attach_at = cc("forked", "SyntheticCall1", "attach");
        let attach = || AttachCall::new(&context(), &link(&attach_at), &attach_parts()).unwrap();
        let request = attach();
        if failure == 0 {
            let error = unreachable(&lab).deliver(request).await.unwrap_err();
            assert!(!matches!(error, HttpError::Refused(_)), "{error:?}");
        } else {
            Mock::given(method("POST"))
                .and(path(url::Url::parse(&attach_at).unwrap().path()))
                .respond_with(ResponseTemplate::new(failure))
                .up_to_n_times(1)
                .with_priority(1)
                .mount(&lab.server)
                .await;
            let answer = lab.transport.deliver(request).await.unwrap();
            assert_eq!(answer.status(), failure);
        }
        // the next attempt goes on the same invitation, and its answer binds the invitation's links: it is answered
        lab.answers("POST", &attach_at, attach_answer()).await;
        let (answer, _) = lab.send(attach()).await;
        assert_eq!(answer.status(), 200, "{failure}");
        let call_link = |name: &str| callback(PUSH_B, "20202020", "call", name);
        let accept = AcceptCall::new(
            &context(),
            &link(&cc("incoming", "SyntheticCall1", "accept")),
            &AcceptParts {
                accepted_by: participant(LAB_B),
                modalities: vec!["Audio".into()],
                call_links: Links::new().with("mediaRenegotiation", call_link("mediaRenegotiation")),
                media_controller_links: Links::new().with("controlVideoStreaming", call_link("controlVideoStreaming")),
                media: media(Some("nonByPass")),
                client: client(),
            },
        )
        .unwrap();
        assert_eq!(
            teams_proto::Request::body(&accept).unwrap()["callAcceptance"]["acceptedBy"]["id"],
            LAB_B
        );
    }
}

#[tokio::test]
async fn a_join_with_media_of_a_running_group_call_registers_it() {
    let lab = lab(LAB_B).await;
    lab.tracker
        .observe_delivery(&delivery(4, ENDPOINT_B, &invitation(true, LAB_A)));
    let parts = JoinParts {
        from: participant(LAB_B),
        roster_update: callback(PUSH_B, "40404040", "conversation", "rosterUpdate"),
        conversation_links: Links::new().with(
            "conversationEnd",
            callback(PUSH_B, "41414141", "conversation", "conversationEnd"),
        ),
        group_chat: GROUP.into(),
        message_id: Some("0".to_owned()),
        meeting: Some(MeetingInfo {
            organizer_id: LAB_A["8:orgid:".len()..].into(),
            tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        }),
        posts_meetup: false,
        client: client(),
    };
    let subscribe = JoinCall::subscribe(&context(), &link(&conv("Synthetic2", "")), &parts).unwrap();
    assert!(
        teams_proto::Request::body(&subscribe)
            .unwrap()
            .get("callInvitation")
            .is_none()
    );

    let join_media = JoinMedia {
        call_links: Links::new().with("acceptance", callback(PUSH_B, "43434343", "call", "acceptance")),
        media_controller_links: Links::new(),
        modalities: vec!["Audio".into(), "ScreenViewer".into()],
        media: media(None),
    };
    let enter = JoinCall::enter(&context(), &link(&conv("Synthetic2", "")), &parts, &join_media).unwrap();

    lab.answers("POST", &conv("Synthetic2", ""), conversation_answer("Synthetic2"))
        .await;
    lab.send(enter).await;
    let metadata = UpdateEndpointMetadata::new(
        &context(),
        &link(&conv("Synthetic2", "updateEndpointMetadata")),
        &participant(LAB_B),
    )
    .unwrap();
    assert_eq!(
        teams_proto::Request::body(&metadata).unwrap()["participants"]["from"]["id"],
        LAB_B
    );
}

#[test]
fn a_join_of_a_channels_meeting_names_the_channel_and_its_post() {
    const CHANNEL: &str = "19:SyntheticTeamThread@thread.tacv2";
    let parts = JoinParts {
        from: participant(LAB_B),
        roster_update: callback(PUSH_B, "40404040", "conversation", "rosterUpdate"),
        conversation_links: Links::new().with(
            "conversationEnd",
            callback(PUSH_B, "41414141", "conversation", "conversationEnd"),
        ),
        group_chat: CHANNEL.into(),
        message_id: Some("1700000000000".to_owned()),
        meeting: Some(MeetingInfo {
            organizer_id: LAB_A["8:orgid:".len()..].into(),
            tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        }),
        posts_meetup: false,
        client: client(),
    };
    let join_media = JoinMedia {
        call_links: Links::new().with("acceptance", callback(PUSH_B, "43434343", "call", "acceptance")),
        media_controller_links: Links::new(),
        modalities: vec!["Audio".into(), "ScreenViewer".into()],
        media: media(None),
    };
    let enter = JoinCall::enter(&context(), &link(&conv("Synthetic3", "")), &parts, &join_media).unwrap();
    assert_eq!(
        teams_proto::Request::body(&enter).unwrap()["groupChat"],
        json!({"threadId": CHANNEL, "messageId": "1700000000000"})
    );

    // a channel's meeting is entered only with the meeting named: without its organizer it doesn't map
    let unnamed = JoinParts { meeting: None, ..parts };
    assert!(JoinCall::enter(&context(), &link(&conv("Synthetic3", "")), &unnamed, &join_media).is_err());
}

#[tokio::test]
async fn without_a_skypetoken_no_call_write_is_sent() {
    let lab = lab_with(LAB_A, false).await;
    let start = StartCall::echo_test(&context(), &start_parts(PUSH_A, LAB_A, None), &Mri::new(ECHO)).unwrap();
    let request = start;
    let error = lab.transport.deliver(request).await.unwrap_err();
    assert!(matches!(error, HttpError::Refused(Refusal::NoSkypetoken)), "{error:?}");
    assert!(lab.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_skypetoken_is_renewed_for_later_and_the_write_not_resent() {
    let lab = lab(LAB_A).await;
    Mock::given(method("POST"))
        .and(path("/api/v2/epconv"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&lab.server)
        .await;
    let start = StartCall::echo_test(&context(), &start_parts(PUSH_A, LAB_A, None), &Mri::new(ECHO)).unwrap();
    let request = start;
    let answer = lab.transport.deliver(request).await.unwrap();
    assert_eq!(answer.status(), 401);
    assert_eq!(lab.server.received_requests().await.unwrap().len(), 1);
    assert_eq!(
        *lab.tokens.rejected.lock().unwrap_or_else(PoisonError::into_inner),
        [SKYPETOKEN]
    );
}

#[test]
fn a_media_contents_debug_leaves_its_sdp_out() {
    let pwd = "IcePwd+7rT/9qWx3ZpLmN0bV";
    let content = MediaContent::new(
        format!("v=0\r\na=ice-ufrag:Uf9+rAg/\r\na=ice-pwd:{pwd}\r\nc=IN IP4 192.0.2.7\r\n"),
        "application/sdp-ngc-1.0",
    );
    let shown = format!("{content:?}");
    for hidden in [pwd, "Uf9+rAg/", "192.0.2.7", "ice-"] {
        assert!(!shown.contains(hidden), "{shown}");
    }
    assert!(shown.contains("application/sdp-ngc-1.0"), "{shown}");
}

#[test]
fn the_calling_constants_share_the_flight_proxy_base() {
    assert_eq!(CALL_START_URL, format!("{CALLING_BASE}/epconv"));
    assert_eq!(CALLING_HOST, teams_proto::CALL_PROXY_HOST);
    assert_eq!(CALLING_BASE, format!("https://{CALLING_HOST}/api/v2"));
}

#[test]
fn a_call_link_is_a_link_on_the_flight_proxy_kept_as_given() {
    for good in [
        conv("Synthetic0", "leave"),
        conv("Synthetic0", ""),
        cc("forked", "SyntheticCall1", "attach"),
    ] {
        assert_eq!(link(&good).as_str(), good);
    }
    for bad in [
        "https://api.flightproxy.teams.microsoft.com/api/v2/epconv",
        "http://api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x",
        "https://api.flightproxy.teams.microsoft.com:8443/api/v2/ep/host/conv/x",
        "https://user@api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x#frag",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/%2e%2e",
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/host//x",
        "https://api.flightproxy.teams.microsoft.com/api/v3/ep/host/conv/x",
        "https://evil.example/api/v2/ep/host/conv/x",
        "https://API.flightproxy.teams.microsoft.com/api/v2/ep/host/conv/x",
    ] {
        assert!(CallLink::parse(bad).is_none(), "{bad}");
    }
}

#[test]
fn a_write_on_another_operations_link_is_never_made() {
    let progress = ReportRinging::new(
        &context(),
        &link(&cc("incoming", "SyntheticCall1", "accept")),
        &participant(LAB_B),
    );
    assert!(matches!(progress, Err(CallWriteError::Unmapped)));
    let decline = DeclineCall::new(&context(), &link(&cc("forked", "SyntheticCall1", "attach")));
    assert!(matches!(decline, Err(CallWriteError::Unmapped)));
    let leave = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic0", "add")),
        &participant(LAB_A),
        LeaveReason::Cancelled { rang_seconds: 9 },
    );
    assert!(matches!(leave, Err(CallWriteError::Unmapped)));
}

#[tokio::test]
async fn the_cancel_of_a_ringing_call_carries_how_long_it_rang() {
    let lab = lab(LAB_A).await;
    started_test_call(&lab).await;
    lab.answers("POST", &conv("Synthetic0", "leave"), json!({})).await;
    let cancel = LeaveCall::new(
        &context(),
        &link(&conv("Synthetic0", "leave")),
        &participant(LAB_A),
        LeaveReason::Cancelled { rang_seconds: 9 },
    )
    .unwrap();
    let (_, request) = lab.send(cancel).await;
    let body = body_of(&request);
    assert_eq!(body["callTransactionEnd"]["code"], 487);
    assert_eq!(
        body["callTransactionEnd"]["callQualityDiagnosticsInformation"]["cancelationDuration"],
        9
    );
}

#[tokio::test]
async fn a_read_on_the_calling_service_is_refused() {
    struct CallingRead;
    impl teams_proto::Read for CallingRead {
        fn service(&self) -> teams_proto::Service {
            teams_proto::Service::Calling
        }

        fn endpoint(&self) -> teams_proto::Endpoint {
            teams_proto::Endpoint::new(["ep", "host", "x"])
        }
    }
    let lab = lab(LAB_A).await;
    let error = lab.transport.read(&CallingRead).await.unwrap_err();
    assert!(matches!(error, HttpError::Refused(Refusal::Calling)), "{error:?}");
}

/// What the calling socket was handed.
#[derive(Clone, Default)]
struct Socket(Arc<Mutex<Vec<String>>>);

impl Socket {
    fn texts(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the socket is closed")]
struct Closed;

impl FrameWriter for Socket {
    type Error = Closed;

    fn write(&mut self, text: FrameText) -> impl Future<Output = Result<(), Closed>> + Send {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.into_string());
        future::ready(Ok(()))
    }
}

/// The JSON of a `3:::` frame.
fn response_json(text: &str) -> Value {
    serde_json::from_str(text.strip_prefix("3:::").unwrap()).unwrap()
}

/// A's started call's acceptance, delivered with `id` on the connection `calling`.
fn acceptance(lab: &Lab, id: i64) -> teams_call::Delivery {
    let at = callback(PUSH_A, "03030303", "call", "acceptance");
    let body = json!({"callAcceptance": {"links": {"acknowledgement": cc("callAcceptance", "SyntheticCall0", "acknowledge")}}});
    let acceptance = delivery(id, url::Url::parse(&at).unwrap().path(), &body);
    lab.tracker.observe_delivery(&acceptance);
    acceptance
}

#[tokio::test]
async fn the_acceptance_is_acknowledged_as_a_typed_frame() {
    let lab = lab(LAB_A).await;
    started_test_call(&lab).await;
    let acceptance = acceptance(&lab, 7);
    let socket = Socket::default();
    let mut sink = FrameSink::new(socket.clone()).with_call_responses("calling");
    let links = Links::new().with(
        "mediaRenegotiation",
        callback(PUSH_A, "0d0d0d0d", "call", "mediaRenegotiation"),
    );
    let ack = AcceptanceAcknowledgement::new(&acceptance, "cv.1", 5, links).unwrap();

    sink.respond(ack.clone()).await.unwrap();

    let texts = socket.texts();
    assert_eq!(texts.len(), 1);
    let sent = response_json(&texts[0]);
    let acknowledgement = json!({"callAcceptanceAcknowledgement": {"links": {
        "mediaRenegotiation": callback(PUSH_A, "0d0d0d0d", "call", "mediaRenegotiation"),
    }}});
    assert_eq!(
        sent,
        json!({
            "id": 7,
            "status": 200,
            "headers": {
                "MS-CV": "cv.1",
                "trouter-request": "request-7",
                "trouter-client": "{\"cd\":5}",
                "X-Microsoft-Skype-Chain-ID": "chain",
                "X-Microsoft-Skype-Message-ID": "message-7",
            },
            "body": acknowledgement.to_string(),
        })
    );
    assert!(
        texts[0].starts_with("3:::{\"id\":7,\"status\":200,\"headers\":{\"MS-CV\""),
        "the captured key order: {}",
        texts[0]
    );
}

#[tokio::test]
async fn an_offered_invitation_gets_its_receipt_and_no_other_does() {
    let lab = lab(LAB_B).await;
    let offered = delivery(3, ENDPOINT_B, &invitation(false, LAB_A));
    lab.tracker.observe_delivery(&offered);
    let socket = Socket::default();
    let mut sink = FrameSink::new(socket.clone()).with_call_responses("calling");
    sink.respond(InvitationReceipt::new(&offered, 2).unwrap())
        .await
        .unwrap();
    assert_eq!(
        socket.texts(),
        [
            r#"3:::{"id":3,"status":200,"headers":{"trouter-request":"request-3","trouter-client":"{\"cd\":2}"},"body":""}"#
        ]
    );

    let again = delivery(5, ENDPOINT_B, &invitation(false, LAB_A));
    lab.tracker.observe_delivery(&again);
    let mut other = FrameSink::new(socket.clone()).with_call_responses("another connection");
    assert!(matches!(
        other.respond(InvitationReceipt::new(&again, 2).unwrap()).await,
        Err(RespondError::OtherConnection)
    ));
    let mut plain = FrameSink::new(socket.clone());
    assert!(matches!(
        plain.respond(InvitationReceipt::new(&again, 2).unwrap()).await,
        Err(RespondError::NoCallResponses)
    ));
    assert_eq!(socket.texts().len(), 1, "only the offered invitation's receipt went");
}

#[test]
fn a_response_needs_the_headers_its_delivery_carried() {
    let mut bare = delivery(9, ENDPOINT_B, &json!({}));
    bare.chain_id = None;
    assert!(AcceptanceAcknowledgement::new(&bare, "cv", 1, Links::new()).is_none());
    assert!(InvitationReceipt::new(&bare, 1).is_some());
    bare.trouter_request = None;
    assert!(InvitationReceipt::new(&bare, 1).is_none());
    let shown = format!(
        "{:?}",
        InvitationReceipt::new(&delivery(9, ENDPOINT_B, &json!({})), 1).unwrap()
    );
    assert!(!shown.contains("request-9"), "{shown}");
}

#[tokio::test]
async fn the_relay_credentials_are_a_read_with_the_ic3_token() {
    let lab = lab(LAB_A).await;
    Mock::given(method("GET"))
        .and(path("/trap/tokens"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tokens": [{"realm": "\"rtcmedia\"", "username": "placeholder-user", "password": "placeholder-password"}],
            "expires": 604_800,
        })))
        .mount(&lab.server)
        .await;
    let answer = lab.transport.read(&RelayTokens).await.unwrap();
    let request = lab.server.received_requests().await.unwrap().pop().unwrap();
    assert_eq!(request.url.query(), None);
    assert_eq!(
        header(&request, "authorization"),
        Some("Bearer placeholder-token-for-https://ic3.teams.office.com")
    );
    assert_eq!(header(&request, "api-version"), Some("2"));
    assert_eq!(header(&request, "x-ms-migration"), Some("True"));
    assert_eq!(header(&request, "x-skypetoken"), None);
    let relay = RelayTokens::read_answer(answer.body()).unwrap();
    assert_eq!(relay.expires, std::time::Duration::from_hours(168));
    let [credential] = relay.credentials.as_slice() else {
        panic!("one credential")
    };
    assert_eq!(credential.realm, "rtcmedia");
    assert_eq!(credential.username, "placeholder-user");
    assert_eq!(credential.password.expose(), "placeholder-password");
    assert!(!format!("{credential:?}").contains("placeholder-password"));
}

#[test]
fn a_relay_answer_outside_its_shape_reads_nothing() {
    for answer in [
        json!({}),
        json!({"tokens": [], "expires": 1}),
        json!({"tokens": [{"realm": "r", "username": "u"}], "expires": 1}),
        json!({"tokens": [{"realm": "r", "username": "", "password": "p"}], "expires": 1}),
        json!({"tokens": [{"realm": "r", "username": "u", "password": "p"}]}),
        json!({"tokens": [{"realm": "r", "username": "u", "password": "p"}], "expires": "1"}),
    ] {
        assert_eq!(
            RelayTokens::read_answer(&serde_json::to_vec(&answer).unwrap()).err(),
            Some(RelayAnswerError::Malformed),
            "{answer}"
        );
    }
}

// --- call payloads ---

#[test]
fn a_lobbys_admit_and_deny_are_built_whole_naming_the_person_waiting() {
    for admit in [true, false] {
        let (segment, success, failure) = if admit {
            ("admit", "admitSuccess", "admitFailure")
        } else {
            (
                "removeParticipant",
                "removeParticipantSuccess",
                "removeParticipantFailure",
            )
        };
        let links = Links::new()
            .with(success, callback(PUSH_A, "08080808", "conversation", success))
            .with(failure, callback(PUSH_A, "09090909", "conversation", failure));
        let at = link(&conv("Synthetic0", segment));
        let from = participant(LAB_A);
        let waiting = Mri::new(LAB_B);
        let write = if admit {
            DecideLobby::admit(&context(), &at, (&from, &waiting), &links)
        } else {
            DecideLobby::deny(&context(), &at, (&from, &waiting), &links)
        }
        .unwrap();
        let body = teams_proto::Request::body(&write).unwrap();
        assert_eq!(body["participants"]["to"][0]["id"], LAB_B);
    }
}

#[test]
fn a_raised_hand_and_a_lowered_one_are_built_in_their_captured_shapes() {
    use teams_proto::{PublishState, RemoveState};
    let from = participant(LAB_A);
    let raised = PublishState::raise_hand(&context(), &link(&conv("Synthetic0", "publishState")), &from, 1).unwrap();
    let body = teams_proto::Request::body(&raised).unwrap();
    assert_eq!(
        body["publishedState"],
        json!({"content": {"skinTone": 0}, "level": "user", "stateType": "raiseHands", "sequenceNumber": 1})
    );
    assert_eq!(body["from"]["id"], LAB_A);
    let lowered = RemoveState::new(
        &context(),
        &link(&conv("Synthetic0", "removeState")),
        &from,
        "hand-a",
        2,
    )
    .unwrap();
    let body = teams_proto::Request::body(&lowered).unwrap();
    assert_eq!(body["stateIds"], json!(["hand-a"]));
    assert_eq!(body["scope"], json!("specified"));
    assert_eq!(body["sequenceNumber"], json!(2));
    // each goes only to its own link
    assert!(matches!(
        PublishState::raise_hand(&context(), &link(&conv("Synthetic0", "removeState")), &from, 1),
        Err(CallWriteError::Unmapped)
    ));
}

#[test]
fn a_live_reaction_and_a_meeting_s_end_are_built_in_their_captured_shapes() {
    use teams_proto::{EndConversation, SendCallReaction};
    let from = participant(LAB_A);
    for name in teams_proto::CALL_REACTIONS {
        let reaction =
            SendCallReaction::new(&context(), &link(&conv("Synthetic0", "sendMessage")), &from, name).unwrap();
        let body = teams_proto::Request::body(&reaction).unwrap();
        let content = &body["messageContent"][0];
        assert_eq!(content["type"], json!("Reaction"));
        assert_eq!(content["scope"], json!("all"));
        assert_eq!(content["payload"]["values"][0]["name"], json!(name));
        assert_eq!(content["payload"]["values"][0]["count"], json!(1));
    }
    assert!(matches!(
        SendCallReaction::new(&context(), &link(&conv("Synthetic0", "sendMessage")), &from, "wave"),
        Err(CallWriteError::Unmapped)
    ));
    let end = EndConversation::new(&context(), &link(&conv("Synthetic0", "")), &from).unwrap();
    assert_eq!(teams_proto::Request::method(&end).as_str(), "DELETE");
    let body = teams_proto::Request::body(&end).unwrap();
    assert_eq!(
        body["conversationTransactionEnd"]["phrase"],
        json!("ConversationEndForAllInitiated")
    );
}

#[test]
fn hold_and_resume_go_at_their_links_in_the_captured_shape() {
    let callback = "https://pub-ent-zz-01-t.trouter.teams.microsoft.com:3443/v4/f/Synthetic/callAgent/Agent1/a1/call/";
    let links = Links::new().with("holdCompletion", format!("{callback}holdCompletion/"));
    let hold = HoldCall::new(&context(), &link(&cc("active", "Call1", "hold")), &links).unwrap();
    assert_eq!(teams_proto::Request::method(&hold).as_str(), "POST");
    let body = teams_proto::Request::body(&hold).unwrap();
    assert_eq!(body["hold"]["holdType"], json!("musicOnHold"));
    assert_eq!(
        body["hold"]["links"]["holdCompletion"],
        json!(format!("{callback}holdCompletion/"))
    );
    assert!(body["debugContent"]["causeId"].as_str().is_some());
    let links = Links::new().with("resumeCompletion", format!("{callback}resumeCompletion/"));
    let resume = ResumeCall::new(&context(), &link(&cc("holdSession", "Call1", "resume")), &links).unwrap();
    let body = teams_proto::Request::body(&resume).unwrap();
    assert_eq!(
        body["resume"]["links"]["resumeCompletion"],
        json!(format!("{callback}resumeCompletion/"))
    );
    // each only at its own link
    assert!(matches!(
        HoldCall::new(&context(), &link(&cc("active", "Call1", "renegotiate")), &links),
        Err(CallWriteError::Unmapped)
    ));
    assert!(matches!(
        ResumeCall::new(&context(), &link(&cc("holdSession", "Call1", "hold")), &links),
        Err(CallWriteError::Unmapped)
    ));
}

#[test]
fn a_screen_sharer_s_participant_properties_go_in_the_captured_shape() {
    let status = callback(PUSH_A, "34343434", "conversation", "updateParticipantPropertiesStatus");
    let properties = UpdateParticipantProperties::screen_sharer(
        &context(),
        &link(&conv("Synthetic1", "updateParticipantProperties")),
        &participant(LAB_A),
        2,
        &status,
        "00000000-0000-4000-8000-0000000000f1",
    )
    .unwrap();
    assert_eq!(teams_proto::Request::method(&properties).as_str(), "POST");
    let body = teams_proto::Request::body(&properties).unwrap();
    assert_eq!(body["to"][0]["id"], json!(LAB_A));
    assert_eq!(
        body["to"][0]["propertyBag"],
        json!({"screenSharerAttributes": {"sequenceNumber": 2, "value": {"source": 1}}})
    );
    assert_eq!(body["links"]["updateParticipantPropertiesStatus"], json!(status));
    assert_eq!(body["operationId"], json!("00000000-0000-4000-8000-0000000000f1"));
    assert_eq!(body["from"]["id"], json!(LAB_A));
    // only at the conversation's own link
    assert!(matches!(
        UpdateParticipantProperties::screen_sharer(
            &context(),
            &link(&conv("Synthetic1", "updateEndpointState")),
            &participant(LAB_A),
            1,
            &status,
            "00000000-0000-4000-8000-0000000000f2",
        ),
        Err(CallWriteError::Unmapped)
    ));
}

#[test]
fn a_person_added_to_a_call_goes_with_its_group_chat_and_no_invitation_data_as_captured() {
    let invitee = Invitee {
        mri: Mri::new(LAB_C),
        participant_id: Some("00000000-0000-4000-8000-0000000000e4".into()),
    };
    let links = Links::new()
        .with(
            "addParticipantSuccess",
            callback(PUSH_A, "07070707", "conversation", "addParticipantSuccess"),
        )
        .with(
            "addParticipantFailure",
            callback(PUSH_A, "08080808", "conversation", "addParticipantFailure"),
        );
    let at = link(&conv("Conv1", "add"));
    let add = AddToCall::person(&context(), &at, &participant(LAB_A), &invitee, GROUP, &links).unwrap();
    let body = teams_proto::Request::body(&add).unwrap();
    assert_eq!(
        body["participants"]["to"],
        json!([{"id": LAB_C, "participantId": "00000000-0000-4000-8000-0000000000e4"}])
    );
    assert_eq!(body["participants"]["from"]["id"], json!(LAB_A));
    assert_eq!(body["participantInvitationData"], json!({}));
    assert_eq!(body["groupChat"], json!({"threadId": GROUP, "messageId": null}));
    assert!(body["links"]["addParticipantFailure"].as_str().is_some());
    assert_eq!(add.invitee().as_str(), LAB_C);
    // never the account itself, a bot, a chat that isn't a group's, or another link
    let me = Invitee {
        mri: Mri::new(LAB_A),
        participant_id: None,
    };
    let bot = Invitee {
        mri: Mri::new(ECHO),
        participant_id: None,
    };
    for (who, chat) in [(&me, GROUP), (&bot, GROUP), (&invitee, MEETING)] {
        assert!(matches!(
            AddToCall::person(&context(), &at, &participant(LAB_A), who, chat, &links),
            Err(CallWriteError::Unmapped)
        ));
    }
    let other = Links::new().with("acceptance", callback(PUSH_A, "09090909", "call", "acceptance"));
    assert!(AddToCall::person(&context(), &at, &participant(LAB_A), &invitee, GROUP, &other).is_err());
}

#[test]
fn a_transfer_goes_at_the_leg_s_transfer_link_and_its_acceptance_at_the_delivered_link_as_captured() {
    let links = Links::new()
        .with(
            "transferAcceptance",
            callback(PUSH_A, "0d0d0d0d", "call", "transferAcceptance"),
        )
        .with(
            "transferCompletion",
            callback(PUSH_A, "0e0e0e0e", "call", "transferCompletion"),
        );
    let at = link(&cc("active", "Call1", "transfer"));
    let transfer = TransferCall::new(&context(), &at, &participant(LAB_A), &Mri::new(LAB_C), &links).unwrap();
    assert_eq!(teams_proto::Request::method(&transfer).as_str(), "POST");
    let body = teams_proto::Request::body(&transfer).unwrap();
    assert_eq!(body["callTransfer"]["target"], json!({"id": LAB_C}));
    assert_eq!(
        body["callTransfer"]["transferor"],
        json!({"details": {"id": LAB_A, "endpointId": "00000000-0000-4000-8000-0000000000e1",
            "participantId": "00000000-0000-4000-8000-0000000000e2", "languageId": "en-us"},
            "authorizationToken": null})
    );
    assert_eq!(
        body["callTransfer"]["transferContext"],
        json!({"customContext": {"callDetails": {}}})
    );
    assert!(body["callTransfer"]["links"]["transferCompletion"].as_str().is_some());
    assert!(matches!(
        TransferCall::new(&context(), &at, &participant(LAB_A), &Mri::new(LAB_A), &links),
        Err(CallWriteError::Unmapped)
    ));
    assert!(matches!(
        TransferCall::new(
            &context(),
            &link(&cc("active", "Call1", "hold")),
            &participant(LAB_A),
            &Mri::new(LAB_C),
            &links
        ),
        Err(CallWriteError::Unmapped)
    ));
    let accepted = AcceptTransfer::new(&context(), &link(&cc("transfer", "Call1", "acceptance"))).unwrap();
    assert_eq!(
        teams_proto::Request::body(&accepted),
        Some(json!({"transferAcceptance": {}}))
    );
    assert!(AcceptTransfer::new(&context(), &link(&cc("transfer", "Call1", "completion"))).is_err());
}

#[test]
fn the_transferred_party_s_call_to_the_target_names_its_transferor_as_delivered() {
    let details = json!({"id": LAB_A, "displayName": "Lab A", "endpointId": "e", "participantId": "p",
        "tenantId": "00000000-0000-4000-8000-0000000000d0", "region": "amer", "propertyBag": null});
    let transferor = Transferor::new(details.clone(), Some("placeholder-transfer-authorization".into()));
    assert_eq!(
        transferor.mri().map(|mri| mri.as_str().to_owned()).as_deref(),
        Some(LAB_A)
    );
    assert_eq!(transferor.display_name(), Some("Lab A"));
    assert!(!format!("{transferor:?}").contains("placeholder-transfer-authorization"));
    let target = Invitee {
        mri: Mri::new(LAB_C),
        participant_id: Some("00000000-0000-4000-8000-0000000000e5".into()),
    };
    let start = StartCall::transferred(&context(), &start_parts(PUSH_B, LAB_B, None), &target, &transferor).unwrap();
    let body = teams_proto::Request::body(&start).unwrap();
    assert_eq!(body["participants"]["to"][0]["id"], json!(LAB_C));
    assert_eq!(body["groupChat"], Value::Null);
    assert_eq!(
        body["callInvitation"]["transferor"],
        json!({"details": details, "authorizationToken": "placeholder-transfer-authorization"})
    );
    assert_eq!(body["callInvitation"]["replaces"], Value::Null);
    let me = Invitee {
        mri: Mri::new(LAB_B),
        participant_id: None,
    };
    assert!(StartCall::transferred(&context(), &start_parts(PUSH_B, LAB_B, None), &me, &transferor).is_err());
}

#[tokio::test]
async fn own_typed_calling_rejects_cross_origin_links_callbacks_and_writes() {
    let cloud = teams_proto::Cloud::own("https://calls.example:8443").unwrap();
    let own = context().for_cloud(cloud.clone());
    let own_push = "https://calls.example:8443/trouter/v4/f/SyntheticEndpoint00000/";
    let own_link = conv("own", "leave").replace(
        "https://api.flightproxy.teams.microsoft.com",
        "https://calls.example:8443/flightproxy",
    );
    let parsed = CallLink::parse_in(&cloud, &own_link).unwrap();
    assert_eq!(parsed.as_str(), own_link);
    for other in [
        conv("own", "leave"),
        own_link.replace(":8443", ":9443"),
        own_link.replace("https://", "http://"),
        own_link.replace("calls.example", "elsewhere.example"),
        own_link.replace("/flightproxy/", "/flightproxy-evil/"),
        format!("{own_link}#fragment"),
    ] {
        assert!(CallLink::parse_in(&cloud, &other).is_none(), "{other}");
    }
    assert!(CallLink::parse(&own_link).is_none());
    assert!(DeclineCall::new(&own, &link(&cc("incoming", "own", "reject"))).is_err());
    assert!(
        DeclineCall::new(
            &context(),
            &CallLink::parse_in(
                &cloud,
                &cc("incoming", "own", "reject").replace(
                    "https://api.flightproxy.teams.microsoft.com",
                    "https://calls.example:8443/flightproxy"
                )
            )
            .unwrap()
        )
        .is_err()
    );
    assert!(StartCall::in_chat(&own, &start_parts(PUSH_A, LAB_A, None), &[invitee(LAB_B)], None).is_err());
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens {
        skypetoken: true,
        rejected: Mutex::default(),
    });
    let transport = HttpTransport::for_cloud_with_mock_origin(
        tokens.clone(),
        "https://calls.example:8443/api/chatsvc/amer",
        cloud.clone(),
        &server.uri(),
    )
    .unwrap();
    let microsoft = StartCall::in_chat(&context(), &start_parts(PUSH_A, LAB_A, None), &[invitee(LAB_B)], None).unwrap();
    assert!(matches!(transport.deliver(microsoft).await, Err(HttpError::Refused(_))));
    let mut own_parts = start_parts(own_push, LAB_A, None);
    own_parts.from.display_name = Some("Lab: A".into());
    let own_start = StartCall::in_chat(&own, &own_parts, &[invitee(LAB_B)], None).unwrap();
    let microsoft_transport = HttpTransport::with_mock_origin(tokens, CHAT_BASE, &server.uri()).unwrap();
    assert!(matches!(
        microsoft_transport.deliver(own_start).await,
        Err(HttpError::Refused(_))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn own_meeting_code_start_retains_the_wire_shape_on_its_origin() {
    let cloud = teams_proto::Cloud::own("https://calls.example:8443").unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/flightproxy/api/v2/epconv"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens {
        skypetoken: true,
        rejected: Mutex::default(),
    });
    let transport = HttpTransport::for_cloud_with_mock_origin(
        tokens,
        "https://calls.example:8443/api/chatsvc/amer",
        cloud.clone(),
        &server.uri(),
    )
    .unwrap();
    let code = teams_proto::MeetingCode::typed("123456789012", "MadeUp").unwrap();
    let start = StartCall::by_code(
        &context().for_cloud(cloud),
        &start_parts(
            "https://calls.example:8443/trouter/v4/f/SyntheticEndpoint00000/",
            LAB_A,
            None,
        ),
        &code,
    )
    .unwrap();
    transport.deliver(start).await.unwrap();
    let sent = server.received_requests().await.unwrap();
    let body = body_of(&sent[0]);
    assert_eq!(
        body["meetingData"]["meetingUrl"],
        "https://calls.example:8443/meet/123456789012?p=MadeUp"
    );
    assert_eq!(header(&sent[0], "x-microsoft-skype-chain-id"), Some(CHAIN));
    assert_eq!(header(&sent[0], "x-skypetoken"), Some(SKYPETOKEN));
}
