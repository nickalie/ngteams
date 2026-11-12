//! Call failures preserve invitation and link binding, retry only explicit rejections, cancel pending retries on
//! hang-up, and shut down with a leave. Media setup fails before signaling; device opening follows admission.

use std::sync::Arc;
use std::time::Duration;

use crate::support::{A_TONE, B_TONE, Lab, TALK, tokens, wait_until};
use serde_json::json;
use teams_core::engine::EngineError;
use teams_core::{CallAction, CallActionError, CallStatus, CallTried, End, Event, Outbox};
use teams_mock::{CHAT_BASE, ECHO, LAB_A, LAB_B, LAB_C, TENANT};
use teams_proto::Mri;
use teams_proto::{AcknowledgeMediaAnswer, Author, CallContext, CallLink, HttpTransport};

/// A media answer's acknowledgement on a call no start of this run registered.
const UNREGISTERED: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/mediaAnswers/MockCallNobodyStarted/1/t/5/acknowledge?i=10-0-0-1";

fn test_call() -> CallAction {
    CallAction::TestCall { echo: Mri::new(ECHO) }
}

/// A's plain outbox on the mock.
fn outbox_of(lab: &Lab) -> Outbox<HttpTransport<crate::support::Tokens>> {
    let tokens = Arc::new(tokens(lab, LAB_A));
    let transport = Arc::new(HttpTransport::with_mock_origin(tokens, CHAT_BASE, &lab.mock.origin()).unwrap());
    Outbox::new(transport, Author::new(Mri::new(LAB_A), "Lab A"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_is_sent_without_policy_or_registry_authorization() {
    let lab = Lab::start().await;
    let outbox = outbox_of(&lab);
    let write =
        AcknowledgeMediaAnswer::new(&CallContext::fresh().unwrap(), &CallLink::parse(UNREGISTERED).unwrap()).unwrap();
    let tried = outbox.try_call(write).await;
    assert!(matches!(tried, CallTried::Answered(_)), "{tried:?}");
    let sent = lab.mock.received_requests().await;
    assert!(
        sent.iter()
            .any(|request| request.url.path().contains("MockCallNobodyStarted")),
        "it went out"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_whose_answer_doesn_t_read_registers_nothing_and_fails_the_call() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.answer_next("POST", "/epconv", 200, &json!({})).await;
    let call = a.handle.requests().call(test_call()).await.unwrap();
    assert_eq!(a.ended(call).await, End::Failed("start"));
    assert_eq!(lab.attempts("/epconv").await, 1, "the start went once");
    assert_eq!(lab.attempts("/leave").await, 0, "no leave without a link");
    assert!(lab.mock.calls().is_empty(), "the world never saw it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_acceptance_that_binds_nothing_is_never_acknowledged_and_the_call_leaves() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.unbind_next_acceptance();
    let call = a.handle.requests().call(test_call()).await.unwrap();
    assert_eq!(a.ended(call).await, End::Failed("acknowledge"));
    wait_until(|| lab.sent_by(LAB_A).contains(&"leave".to_owned())).await;
    assert_eq!(
        lab.sent_by(LAB_A),
        ["start", "leave"],
        "the leave on the conversation the start registered; nothing on the acceptance's links"
    );
    assert_eq!(lab.responses_of(LAB_A), 0, "no acknowledgement");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_media_setup_that_fails_refuses_the_action_ends_its_told_call_and_sends_nothing() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.refuse_next("GET", "/trap/tokens", 500).await;
    let refused = a.handle.requests().call(test_call()).await;
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::Media(_)))),
        "{refused:?}"
    );
    // told placing as its setup began (its window shows it at once), then ended where it shows
    let told = a
        .next(|event| match event {
            Event::Call {
                call,
                status: CallStatus::Placing,
            } => Some(*call),
            _ => None,
        })
        .await;
    assert_eq!(a.ended(told).await, End::Failed("setup"));
    assert!(lab.mock.calls().is_empty());
    assert_eq!(lab.attempts("/epconv").await, 0);
    // and the next one goes, told placing once
    let call = a.handle.requests().call(test_call()).await.unwrap();
    assert_ne!(call, told);
    a.status(call, &CallStatus::Connected).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_hung_up_while_its_media_is_set_up_ends_at_once_and_sends_nothing() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock
        .refuse_next_after("GET", "/trap/tokens", 500, Duration::from_secs(2))
        .await;
    let requests = a.handle.requests();
    let start = requests.call(test_call());
    let told = a
        .next(|event| match event {
            Event::Call {
                call,
                status: CallStatus::Placing,
            } => Some(*call),
            _ => None,
        })
        .await;
    // a mute meanwhile is held for its media
    assert_eq!(
        requests
            .call(CallAction::Mute {
                call: told,
                muted: true
            })
            .await
            .unwrap(),
        told
    );
    assert_eq!(requests.call(CallAction::HangUp { call: told }).await.unwrap(), told);
    assert_eq!(start.await.unwrap(), told, "the start's call, ended");
    assert_eq!(a.ended(told).await, End::HungUp);
    let quiet = a.events_within(Duration::from_millis(2500)).await;
    assert!(quiet.is_empty(), "nothing after its end: {quiet:#?}");
    assert_eq!(lab.attempts("/epconv").await, 0);
    assert!(lab.mock.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_asked_while_a_catch_up_waits_on_the_service_starts_at_once() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    // the service is slow to answer the catch-up's read of the conversations, as a long catch-up holds the engine
    lab.mock
        .refuse_next_after("GET", "/conversations", 500, Duration::from_secs(4))
        .await;
    let sync = tokio::spawn(a.handle.requests().sync());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let start = a.handle.requests().call(test_call());
    let told = tokio::time::timeout(
        Duration::from_secs(2),
        a.next(|event| match event {
            Event::Call {
                call,
                status: CallStatus::Placing,
            } => Some(*call),
            _ => None,
        }),
    )
    .await
    .expect("the call waited for the catch-up");
    assert!(!sync.is_finished(), "the catch-up was over before the call started");
    assert_eq!(start.await.unwrap(), told);
    a.status(told, &CallStatus::Connected).await;
    drop(sync.await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_whose_media_fails_keeps_the_invitation_ringing_until_it_is_declined() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let _ = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    lab.mock.refuse_next("GET", "/trap/tokens", 500).await;
    let requests = b.handle.requests();
    let refused = requests.call(CallAction::Answer { call: incoming.call }).await;
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::Media(_)))),
        "{refused:?}"
    );
    let quiet = b.events_within(Duration::from_millis(300)).await;
    assert!(quiet.is_empty(), "it still rings: {quiet:#?}");
    requests
        .call(CallAction::Decline { call: incoming.call })
        .await
        .unwrap();
    assert_eq!(b.ended(incoming.call).await, End::Declined);
    assert_eq!(lab.sent_by(LAB_B), ["attach", "progress", "reject"]);
}

/// Hanging up an invitation whose answer still sets its media up: the answer never goes (it is refused as hung up
/// before its media), and the invitation is declined instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_hung_up_while_its_media_is_set_up_declines_the_invitation() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let _ = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    lab.mock
        .refuse_next_after("GET", "/trap/tokens", 500, Duration::from_secs(2))
        .await;
    let requests = b.handle.requests();
    let answer = tokio::spawn(requests.call(CallAction::Answer { call: incoming.call }));
    // the answer waits on its media's relay credentials
    lab.wait_attempts("/trap/tokens", 1).await;
    assert_eq!(
        requests.call(CallAction::HangUp { call: incoming.call }).await.unwrap(),
        incoming.call
    );
    let answered = answer.await.unwrap();
    assert!(
        matches!(answered, Err(EngineError::Call(CallActionError::Media(_)))),
        "{answered:?}"
    );
    assert_eq!(b.ended(incoming.call).await, End::Declined);
    let sent = lab.sent_by(LAB_B);
    assert!(sent.contains(&"reject".to_owned()), "{sent:?}");
    assert!(!sent.contains(&"accept".to_owned()), "{sent:?}");
}

/// An explicitly rejected `attach` renews its token and retries against the same still-ringing invitation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_attach_refused_with_a_401_goes_again_and_its_invitation_is_answered() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    lab.mock.refuse_next("POST", "/attach", 401).await;
    let _ = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
    assert_eq!(lab.attempts("/attach").await, 2, "the refused attach and its retry");
    let sent = lab.sent_by(LAB_B);
    assert_eq!(sent.first().map(String::as_str), Some("attach"), "{sent:?}");
    assert!(sent.contains(&"accept".to_owned()), "{sent:?}");
}

/// The far end closing the media with no end of the call delivered after it (seen live: a web client's hang-up
/// closed its media and no `call/end` came): the far end left, and the account's leg leaves too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_far_end_closing_the_media_ends_the_call_as_the_far_end_s_with_a_leave() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let call = a.handle.requests().call(test_call()).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    let _ = lab.mock.close_peer(&lab.last_call(), LAB_A);
    assert_eq!(a.ended(call).await, End::Remote(teams_call::RemoteEnd::default()));
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_sent_again_after_a_throttle_connects_and_its_acceptance_is_acknowledged() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.refuse_next("POST", "/epconv", 503).await;
    let requests = a.handle.requests();
    let call = requests.call(test_call()).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    wait_until(|| lab.sent_by(LAB_A).contains(&"add".to_owned())).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
    assert_eq!(lab.attempts("/epconv").await, 2, "the throttled start and its retry");
    assert_acceptance_route(&lab).await;
}

/// The acceptance uses its delivery response, or the documented HTTP fallback when that connection went away.
/// Validate the optional request before removing its one position from the ordered call operations.
async fn assert_acceptance_route(lab: &Lab) {
    let operations = lab.sent_by(LAB_A);
    let acknowledgements: Vec<usize> = operations
        .iter()
        .enumerate()
        .filter_map(|(at, operation)| (operation == "acknowledge").then_some(at))
        .collect();
    assert!(acknowledgements.len() <= 1, "one acknowledgement route: {operations:?}");
    let all = lab.mock.received_requests().await;
    let http: Vec<_> = all
        .iter()
        .filter(|request| {
            request.url.path().contains("/callAcceptance/") && request.url.path().ends_with("/acknowledge")
        })
        .collect();
    assert_eq!(http.len(), acknowledgements.len());
    if let Some(&at) = acknowledgements.first() {
        assert!(at > 1 && at < operations.len() - 1, "{operations:?}");
        let call = lab.mock.calls().pop().unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../../../tests/fixtures/calling/echo/answers.json")).unwrap();
        let expected = fixture["acceptance_callback"]["body"]["callAcceptance"]["links"]["acknowledgement"]
            .as_str()
            .unwrap()
            .replace("<guid:5>", &call.id);
        let url = reqwest::Url::parse(&expected).unwrap();
        assert_eq!(http[0].method.as_str(), "POST");
        assert_eq!(http[0].url.path(), url.path(), "the link belongs to this accepted call");
        assert_eq!(http[0].url.query(), url.query());
        assert_acknowledgement_payload(http[0], &all);
        eprintln!(
            "call {}: acceptance acknowledged once by its admitted HTTP fallback",
            call.id
        );
    } else {
        eprintln!("acceptance acknowledged once by its delivery response");
    }
    let ordered: Vec<_> = operations
        .iter()
        .enumerate()
        .filter(|(at, _)| !acknowledgements.contains(at))
        .map(|(_, operation)| operation.as_str())
        .collect();
    assert_eq!(ordered, ["start", "updateEndpointMetadata", "add", "leave"]);
    assert_eq!(
        lab.responses_of(LAB_A) + http.len(),
        1,
        "one way, with no duplicate acknowledgement"
    );
}

fn assert_acknowledgement_payload(request: &wiremock::Request, requests: &[wiremock::Request]) {
    let start = requests
        .iter()
        .find(|request| {
            request.url.path().ends_with("/epconv")
                && serde_json::from_slice::<serde_json::Value>(&request.body).is_ok()
        })
        .unwrap();
    let started: serde_json::Value = serde_json::from_slice(&start.body).unwrap();
    let acceptance = started["callInvitation"]["links"]["acceptance"].as_str().unwrap();
    let callback = acceptance.strip_suffix("/call/acceptance/").unwrap();
    let agent = format!("{}/", callback.rsplit_once('/').unwrap().0);
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body.as_object().unwrap().len(), 1);
    let acknowledgement = body["callAcceptanceAcknowledgement"].as_object().unwrap();
    assert_eq!(acknowledgement.len(), 1);
    let links = acknowledgement["links"].as_object().unwrap();
    assert_eq!(links.len(), 7);
    for name in [
        "mediaRenegotiation",
        "transfer",
        "replacement",
        "balanceUpdate",
        "retargetCompletion",
        "controlVideoStreaming",
        "updateMediaDescriptions",
    ] {
        let link = links[name].as_str().unwrap();
        assert!(
            link.starts_with(&agent),
            "the fallback hands out only this call agent's callbacks"
        );
        assert!(link.ends_with(&format!("/call/{name}/")));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_throttled_start_hung_up_during_its_wait_is_never_sent_again() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    lab.mock.refuse_next("POST", "/epconv", 503).await;
    let requests = a.handle.requests();
    let call = requests.call(test_call()).await.unwrap();
    // the start went and was throttled: its next attempt waits a second (no `Retry-After`); the account hangs up
    // meanwhile
    lab.wait_attempts("/epconv", 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    requests.call(CallAction::HangUp { call }).await.unwrap();
    let _ = a.ended(call).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert_eq!(lab.attempts("/epconv").await, 1, "the start went once");
    assert!(lab.mock.calls().is_empty(), "no call rang anywhere");
    assert_eq!(a.microphones(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_renegotiation_while_the_first_waits_is_refused_and_the_call_goes_on() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    let first = requests.call(CallAction::Renegotiate { call });
    let second = requests.call(CallAction::Renegotiate { call });
    assert_eq!(first.await.unwrap(), call);
    let refused = second.await;
    assert!(
        matches!(
            refused,
            Err(EngineError::Call(CallActionError::Call(teams_call::CallError::NotNow)))
        ),
        "{refused:?}"
    );
    wait_until(|| lab.sent_by(LAB_B).contains(&"acknowledge".to_owned())).await;
    let before = lab.mock.peer_stats(&id, LAB_B).unwrap().received;
    tokio::time::sleep(TALK).await;
    assert!(
        lab.mock.peer_stats(&id, LAB_B).unwrap().received > before + 50,
        "the media goes on after the first's answer"
    );
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
    let sent = lab.sent_by(LAB_B);
    assert_eq!(sent.iter().filter(|op| *op == "renegotiate").count(), 1, "{sent:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_stopping_hangs_the_call_up_with_a_leave() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let call = a.handle.requests().call(test_call()).await.unwrap();
    a.status(call, &CallStatus::Connected).await;
    a.shut_down().await;
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_stopping_while_the_start_is_in_flight_waits_for_it_and_leaves() {
    let lab = Lab::start().await;
    let a = lab.account(LAB_A, A_TONE).await;
    // the action is answered once the start was handed to the outbox: it is in flight as the engine stops
    let _ = a.handle.requests().call(test_call()).await.unwrap();
    a.shut_down().await;
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
    assert_eq!(lab.sent_by(LAB_A).first().map(String::as_str), Some("start"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_answered_after_the_shutdown_grace_still_gets_its_leave() {
    let lab = Lab::start().await;
    let a = lab.account(LAB_A, A_TONE).await;
    // the start's answer comes past `SHUTDOWN_GRACE`, within the request's timeout
    lab.mock
        .delay_next_call_op("epconv", teams_core::calling::SHUTDOWN_GRACE + Duration::from_secs(2));
    let _ = a.handle.requests().call(test_call()).await.unwrap();
    a.shut_down().await;
    wait_until(|| lab.sent_by(LAB_A).last().map(String::as_str) == Some("leave")).await;
    assert_eq!(lab.sent_by(LAB_A).first().map(String::as_str), Some("start"));
}

/// a's token in a JWT's shape (made-up claims, no signature checked): its audience and the mock's tenant.
const A_TOKEN_WITH_TENANT: &str = "eyJhbGciOiJub25lIn0.\
    eyJhdWQiOiJodHRwczovL2ljMy50ZWFtcy5vZmZpY2UuY29tIiwidGlkIjoiMDAwMDAwMDAtMDAwMC00MDAwLTgwMDAtMDAwMDAwMDAwMDAxIn0.\
    c2lnbmF0dXJl";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_call_of_an_engine_without_the_tenant_reads_it_from_the_account_s_token_first() {
    let lab = Lab::start().await;
    assert!(lab.mock.set_token(LAB_A, A_TOKEN_WITH_TENANT));
    let mut a = lab.account_without_tenant(LAB_A, A_TONE).await;
    let requests = a.handle.requests();
    let call = requests
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    wait_until(|| lab.sent_by(LAB_A).contains(&"add".to_owned())).await;
    // the `add` names the tenant read from the token (its `tid`, the mock's tenant)
    let added = lab.mock.calls().last().expect("a call").add_tenants.clone();
    assert_eq!(added, [TENANT]);
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_test_call_whose_tenant_can_t_be_read_is_refused_and_sends_nothing() {
    let lab = Lab::start().await;
    // the mock's own tokens are opaque: no tenant in them
    let a = lab.account_without_tenant(LAB_A, A_TONE).await;
    let refused = a
        .handle
        .requests()
        .call(CallAction::TestCall { echo: Mri::new(ECHO) })
        .await;
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::Media(ref why))) if why.contains("tenant")),
        "{refused:?}"
    );
    assert!(lab.mock.calls().is_empty(), "no start went");
    assert_eq!(lab.attempts("/epconv").await, 0, "no start was sent");
    assert_eq!(a.microphones(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_from_a_chat_that_names_nobody_is_refused_as_not_a_chat_and_sends_nothing() {
    let lab = Lab::start().await;
    let a = lab.account(LAB_A, A_TONE).await;
    // a group chat the store doesn't hold, and a 1:1 chat's id without the account in it
    for chat in ["19:unknown@thread.v2", "19:someone_else-entirely@unq.gbl.spaces"] {
        let refused = a
            .handle
            .requests()
            .call(CallAction::Call { chat: chat.to_owned() })
            .await;
        assert!(
            matches!(refused, Err(EngineError::Call(CallActionError::NotAChat))),
            "{chat}: {refused:?}"
        );
    }
    assert!(lab.mock.calls().is_empty(), "no start went");
    assert_eq!(lab.attempts("/epconv").await, 0, "no start was sent");
    assert_eq!(a.microphones(), 0);
}

struct PendingDevices {
    released: tokio::sync::watch::Receiver<bool>,
    fail: bool,
    files: teams_media::devices::file::FileDevices,
}

impl teams_media::devices::Devices for PendingDevices {
    fn preparing(&self) -> bool {
        !*self.released.borrow()
    }
    fn ready(&self) -> teams_media::devices::DeviceReady<'_> {
        Box::pin(async move {
            let mut released = self.released.clone();
            released
                .wait_for(|released| *released)
                .await
                .map_err(|_| teams_media::devices::DeviceError::Closed)?;
            if self.fail {
                Err(teams_media::devices::DeviceError::Closed)
            } else {
                Ok(())
            }
        })
    }
    fn microphone(&self) -> Result<Box<dyn teams_media::devices::AudioSource>, teams_media::devices::DeviceError> {
        self.files.microphone()
    }
    fn speaker(&self) -> Result<Box<dyn teams_media::devices::AudioSink>, teams_media::devices::DeviceError> {
        self.files.speaker()
    }
    fn camera(&self) -> Result<Box<dyn teams_media::devices::VideoSource>, teams_media::devices::DeviceError> {
        self.files.camera()
    }
}

fn pending_devices(fail: bool) -> (Arc<PendingDevices>, tokio::sync::watch::Sender<bool>) {
    use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
    let (release, released) = tokio::sync::watch::channel(false);
    (
        Arc::new(PendingDevices {
            released,
            fail,
            files: FileDevices {
                input: AudioInput::Silence,
                recording: None,
                camera: PatternCamera::default(),
                pacing: Pacing::Unpaced,
            },
        }),
        release,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_discovery_leaves_engine_running_and_a_call_preparing_until_ready() {
    let lab = Lab::start().await;
    let (devices, release) = pending_devices(false);
    let mut a = lab.account_with_devices(devices).await;
    let start = a.handle.requests().call(test_call());
    let told = a
        .next(|event| match event {
            Event::Call {
                call,
                status: CallStatus::Preparing,
            } => Some(*call),
            _ => None,
        })
        .await;
    tokio::time::timeout(Duration::from_secs(5), a.handle.requests().sync())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lab.attempts("/epconv").await, 0);
    assert_eq!(lab.attempts("/trap/tokens").await, 0, "discovery precedes media setup");
    release.send_replace(true);
    assert_eq!(start.await.unwrap(), told);
    a.status(told, &CallStatus::Placing).await;
    a.status(told, &CallStatus::Connected).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deferred_discovery_failure_refuses_calls_without_sending_and_keeps_sync_working() {
    let lab = Lab::start().await;
    let (devices, release) = pending_devices(true);
    let mut a = lab.account_with_devices(devices).await;
    let start = a.handle.requests().call(test_call());
    let told = a
        .next(|event| match event {
            Event::Call {
                call,
                status: CallStatus::Preparing,
            } => Some(*call),
            _ => None,
        })
        .await;
    release.send_replace(true);
    let refused = start.await;
    assert!(matches!(refused, Err(EngineError::Call(CallActionError::NoCalling))));
    assert_eq!(a.ended(told).await, End::Failed("setup"));
    assert_eq!(lab.attempts("/epconv").await, 0);
    assert_eq!(lab.attempts("/trap/tokens").await, 0);
    a.handle.requests().sync().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_can_be_cancelled_while_discovery_is_pending() {
    let lab = Lab::start().await;
    let (devices, _release) = pending_devices(false);
    let mut a = lab.account_with_devices(devices).await;
    let start = a.handle.requests().call(test_call());
    let told = a
        .next(|event| match event {
            Event::Call {
                call,
                status: CallStatus::Preparing,
            } => Some(*call),
            _ => None,
        })
        .await;
    a.handle
        .requests()
        .call(CallAction::HangUp { call: told })
        .await
        .unwrap();
    assert_eq!(start.await.unwrap(), told);
    assert_eq!(a.ended(told).await, End::HungUp);
    assert_eq!(lab.attempts("/epconv").await, 0);
    a.shut_down().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn answering_waits_for_deferred_discovery_before_sending() {
    let lab = Lab::start().await;
    let (devices, release) = pending_devices(false);
    let mut a = lab.account_with_devices(devices).await;
    let _ = lab.mock.ring(LAB_C, LAB_A);
    let incoming = a.incoming().await;
    let mut answer = Box::pin(a.handle.requests().call(CallAction::Answer { call: incoming.call }));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut answer)
            .await
            .is_err()
    );
    assert_eq!(lab.attempts("/accept").await, 0);
    assert_eq!(lab.attempts("/trap/tokens").await, 0);
    release.send_replace(true);
    assert_eq!(answer.await.unwrap(), incoming.call);
    a.status(incoming.call, &CallStatus::Connected).await;
}
