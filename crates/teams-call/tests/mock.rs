//! `teams-call` on the mock: native clients of the synthetic accounts, each with its own tracker,
//! transport and calling socket, driven as the engine will drive them (`support`), against `teams-mock`'s calling
//! service and Trouter: the test call through start → acceptance → acknowledgement → connected → leave; a 1:1 call
//! answered and declined only by an action; an outsider's invitation ignored with nothing sent for it; an offered
//! invitation that nobody acts on left unanswered; the ring's ends (cancelled, answered elsewhere, timed out); an
//! invitation during a call not offered; a running group call joined; the keep-alive sent to the call leg's own link
//! every 90 % of its interval until the call ends.
//!
//! Every id, token and MRI here is the mocks' made-up one.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

mod support;

use std::time::{Duration, Instant};

use serde_json::Value;
use support::{Client, answer};
use teams_call::{CallEvent, CallId, CallState, End, Incoming, Placement};
use teams_mock::{
    ABC_CHAT, ANSWERED_ELSEWHERE, CallPhase, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TIMED_OUT, TrouterMock,
};
use teams_proto::Mri;

struct Lab {
    mock: ChatServiceMock,
    trouter: TrouterMock,
}

impl Lab {
    async fn start() -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        Self { mock, trouter }
    }

    async fn client(&self, mri: &'static str) -> Client {
        Client::start(&self.mock, self.trouter.origin(), mri).await
    }

    /// The calls' requests sent with `mri`'s skypetoken.
    async fn calling_requests_of(&self, mri: &str) -> Vec<String> {
        let token = self.mock.skypetoken(mri).unwrap();
        self.mock
            .received_requests()
            .await
            .into_iter()
            .filter(|request| {
                request.url.path().starts_with("/api/v2/")
                    && request
                        .headers
                        .get("x-skypetoken")
                        .is_some_and(|value| value.to_str().ok() == Some(token.as_str()))
            })
            .map(|request| format!("{} {}", request.method, request.url.path()))
            .collect()
    }

    /// `mri`'s calls' requests once one ends with `suffix` (a request the mock got is recorded a moment after its
    /// answer went).
    async fn requests_after(&self, mri: &str, suffix: &str) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + support::WAIT;
        loop {
            let sent = self.calling_requests_of(mri).await;
            if sent.iter().any(|request| request.ends_with(suffix)) {
                return sent;
            }
            assert!(tokio::time::Instant::now() < deadline, "no {suffix}: {sent:#?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// `mri`'s delivery responses once the mock recorded `count` of them (a response written reaches the mock's record a
    /// moment later).
    async fn responses_after(&self, mri: &str, count: usize) -> Vec<Value> {
        let deadline = tokio::time::Instant::now() + support::WAIT;
        loop {
            let responses = self.responses_of(mri);
            if responses.len() >= count {
                return responses;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{count} responses: {responses:#?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Every delivery response (`3:::`) on `mri`'s sockets.
    fn responses_of(&self, mri: &str) -> Vec<Value> {
        self.trouter
            .sockets()
            .iter()
            .filter(|socket| socket.account.as_deref() == Some(mri))
            .flat_map(|socket| socket.frames.clone())
            .filter_map(|frame| frame.strip_prefix("3:::").map(str::to_owned))
            .map(|json| serde_json::from_str(&json).unwrap())
            .collect()
    }
}

async fn state(client: &mut Client, wanted: CallState) -> CallId {
    client
        .next(|event| match event {
            CallEvent::State { call, state } if state == wanted => Some(call),
            _ => None,
        })
        .await
}

async fn incoming(client: &mut Client) -> Incoming {
    client
        .next(|event| match event {
            CallEvent::Incoming(incoming) => Some(incoming),
            _ => None,
        })
        .await
}

async fn ended(client: &mut Client) -> End {
    client
        .next(|event| match event {
            CallEvent::Ended { end, .. } => Some(end),
            CallEvent::RingEnded { end, .. } => panic!("a ring ended ({end:?}), not a call"),
            _ => None,
        })
        .await
}

async fn ring_ended(client: &mut Client) -> teams_call::RemoteEnd {
    client
        .next(|event| match event {
            CallEvent::RingEnded { end, .. } => Some(end),
            CallEvent::Ended { end, .. } => panic!("the call ended ({end:?}) instead of its ring"),
            _ => None,
        })
        .await
}

/// The mock's one call.
fn only_call(lab: &Lab) -> teams_mock::MockCall {
    let calls = lab.mock.calls();
    let [call] = calls.as_slice() else {
        panic!("one call: {calls:#?}");
    };
    call.clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_test_call_starts_is_accepted_acknowledged_and_left() {
    let lab = Lab::start().await;
    let mut a = lab.client(LAB_A).await;
    a.place(Placement::TestCall { echo: Mri::new(ECHO) });
    let call = state(&mut a, CallState::Placing).await;
    state(&mut a, CallState::Ringing).await;
    let media = a
        .next(|event| match event {
            CallEvent::RemoteMedia { media, .. } => Some(media),
            _ => None,
        })
        .await;
    assert_eq!(
        media.content_type, "application/sdp-ngc-1.0",
        "the media controller's answer"
    );
    state(&mut a, CallState::Connected).await;
    // the acceptance is acknowledged by its delivery response, not by request
    let responses = lab.responses_after(LAB_A, 1).await;
    let [acknowledgement] = responses.as_slice() else {
        panic!("one response: {responses:#?}");
    };
    let body: Value = serde_json::from_str(acknowledgement["body"].as_str().unwrap()).unwrap();
    assert!(body["callAcceptanceAcknowledgement"]["links"]["mediaRenegotiation"].is_string());
    a.hang_up(call);
    assert_eq!(ended(&mut a).await, End::HungUp);

    let mock_call = only_call(&lab);
    assert!(mock_call.test_call);
    let ops: Vec<&str> = mock_call.requests.iter().map(|(op, _)| op.as_str()).collect();
    assert_eq!(ops, ["start", "updateEndpointMetadata", "add", "leave"]);
    assert_eq!(mock_call.phase, CallPhase::Ended);
    let sent = lab.calling_requests_of(LAB_A).await;
    assert!(
        !sent.iter().any(|request| request.ends_with("/acknowledge")),
        "no acknowledgement by request: {sent:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_is_answered_only_by_the_callees_action_and_both_ends_connect() {
    let lab = Lab::start().await;
    let mut a = lab.client(LAB_A).await;
    let mut b = lab.client(LAB_B).await;
    a.place(Placement::Chat {
        callees: vec![Mri::new(LAB_B)],
        group_chat: None,
    });
    state(&mut a, CallState::Ringing).await;
    let rings = incoming(&mut b).await;
    assert_eq!(
        (rings.caller.as_str(), rings.caller_name.as_deref(), rings.group_chat),
        (LAB_A, Some("Lab A"), None)
    );
    assert!(rings.offer.is_some(), "the caller's offer");

    // nobody acts: it keeps ringing, unanswered
    let quiet = b.events_within(Duration::from_millis(1_500)).await;
    assert!(quiet.is_empty(), "{quiet:#?}");
    let leg = only_call(&lab).legs[0].clone();
    assert_eq!((leg.attached.len(), leg.ringing.len(), leg.accepted_by), (1, 1, None));
    assert!(
        !lab.calling_requests_of(LAB_B)
            .await
            .iter()
            .any(|request| request.ends_with("/accept")),
        "no answer without the action"
    );

    b.answer(rings.call);
    state(&mut b, CallState::Connected).await;
    let media = a
        .next(|event| match event {
            CallEvent::RemoteMedia { media, .. } => Some(media),
            _ => None,
        })
        .await;
    assert_eq!(media.blob, answer().blob, "the callee's answer reaches the caller");
    state(&mut a, CallState::Connected).await;
    b.hang_up(rings.call);
    assert_eq!(ended(&mut b).await, End::HungUp);
    assert!(matches!(ended(&mut a).await, End::Remote(_)));

    let call = only_call(&lab);
    let b_ops: Vec<&str> = call
        .requests
        .iter()
        .filter(|(_, by)| by == LAB_B)
        .map(|(op, _)| op.as_str())
        .collect();
    assert_eq!(
        b_ops,
        [
            "attach",
            "progress",
            "updateEndpointState",
            "accept",
            "updateEndpointMetadata",
            "leave"
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_is_declined_only_by_the_callees_action() {
    let lab = Lab::start().await;
    let mut a = lab.client(LAB_A).await;
    let mut b = lab.client(LAB_B).await;
    a.place(Placement::Chat {
        callees: vec![Mri::new(LAB_B)],
        group_chat: None,
    });
    let call = state(&mut a, CallState::Ringing).await;
    let rings = incoming(&mut b).await;
    b.decline(rings.call);
    assert_eq!(ended(&mut b).await, End::Declined);
    // as captured: a 1:1 call goes on to the callee's voicemail, which accepts it
    state(&mut a, CallState::Connected).await;
    a.hang_up(call);
    assert_eq!(ended(&mut a).await, End::HungUp);

    let mock_call = only_call(&lab);
    assert!(mock_call.legs[0].declined);
    assert_eq!(mock_call.legs[0].accepted_by, None);
    let sent = lab.calling_requests_of(LAB_B).await;
    assert!(
        sent.iter()
            .any(|request| request.starts_with("DELETE") && request.ends_with("/reject"))
    );
    assert!(!sent.iter().any(|request| request.ends_with("/accept")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_caller_cancelling_ends_the_ring() {
    let lab = Lab::start().await;
    let mut a = lab.client(LAB_A).await;
    let mut b = lab.client(LAB_B).await;
    a.place(Placement::Chat {
        callees: vec![Mri::new(LAB_B)],
        group_chat: None,
    });
    let call = state(&mut a, CallState::Ringing).await;
    incoming(&mut b).await;
    a.hang_up(call);
    assert_eq!(ended(&mut a).await, End::HungUp);
    let end = ring_ended(&mut b).await;
    assert_eq!(end.code, Some(487), "the cancel's code, as the caller's leave sent it");
    assert!(
        !lab.calling_requests_of(LAB_B)
            .await
            .iter()
            .any(|request| request.ends_with("/accept") || request.ends_with("/reject"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn an_answer_on_another_endpoint_ends_the_ring_here() {
    let lab = Lab::start().await;
    let mut here = lab.client(LAB_B).await;
    let mut there = lab.client(LAB_B).await;
    let _ = lab.mock.ring(LAB_A, LAB_B);
    let first_ring = incoming(&mut here).await;
    let second_ring = incoming(&mut there).await;
    there.answer(second_ring.call);
    state(&mut there, CallState::Connected).await;
    let end = ring_ended(&mut here).await;
    assert_eq!(end.phrase.as_deref(), Some(ANSWERED_ELSEWHERE));
    let call = only_call(&lab);
    assert_eq!(call.legs[0].attached.len(), 1, "only the endpoint that answered");
    // the ring that ended here can't be answered any more
    here.answer(first_ring.call);
    assert_eq!(here.refusal().await, "no such call is up");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ring_that_times_out_ends_the_ringing() {
    let lab = Lab::start().await;
    let mut b = lab.client(LAB_B).await;
    let id = lab.mock.ring(LAB_A, LAB_B);
    incoming(&mut b).await;
    assert!(lab.mock.end_ring(&id, 408, TIMED_OUT));
    let end = ring_ended(&mut b).await;
    assert_eq!((end.code, end.phrase.as_deref()), (Some(408), Some(TIMED_OUT)));
    assert!(
        !lab.calling_requests_of(LAB_B)
            .await
            .iter()
            .any(|request| request.ends_with("/accept") || request.ends_with("/reject"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invitation_during_a_call_is_not_offered() {
    let lab = Lab::start().await;
    let mut b = lab.client(LAB_B).await;
    let first = lab.mock.ring(LAB_A, LAB_B);
    let rings = incoming(&mut b).await;
    b.answer(rings.call);
    state(&mut b, CallState::Connected).await;
    let before = lab.requests_after(LAB_B, "/updateEndpointMetadata").await.len();
    let responses_before = lab.responses_after(LAB_B, 1).await.len();

    let second = lab.mock.ring(LAB_C, LAB_B);
    let seen = b.events_within(Duration::from_secs(2)).await;
    assert!(seen.is_empty(), "the second invitation isn't offered: {seen:#?}");
    assert_eq!(
        lab.calling_requests_of(LAB_B).await.len(),
        before,
        "nothing sent for it"
    );
    assert_eq!(lab.responses_of(LAB_B).len(), responses_before, "no receipt for it");
    let calls = lab.mock.calls();
    let second = calls.iter().find(|call| call.id == second).unwrap();
    assert!(second.legs[0].attached.is_empty());
    assert!(lab.mock.hang_up_call(&first));
    assert!(matches!(ended(&mut b).await, End::Remote(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_group_call_is_joined_from_its_chat() {
    let lab = Lab::start().await;
    let mut b = lab.client(LAB_B).await;
    let id = lab.mock.ring_group(LAB_A, ABC_CHAT, &[LAB_B, LAB_C]);
    let rings = incoming(&mut b).await;
    assert_eq!(rings.group_chat.as_deref(), Some(ABC_CHAT));
    assert!(lab.mock.end_ring(&id, 408, TIMED_OUT));
    ring_ended(&mut b).await;

    b.join(ABC_CHAT);
    let call = state(&mut b, CallState::Joining).await;
    state(&mut b, CallState::Connected).await;
    b.hang_up(call);
    assert_eq!(ended(&mut b).await, End::HungUp);
    let ops: Vec<String> = only_call(&lab)
        .requests
        .into_iter()
        .filter(|(_, by)| by == LAB_B)
        .map(|(op, _)| op)
        .collect();
    assert_eq!(ops, ["attach", "progress", "join", "updateEndpointMetadata", "leave"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_keep_alive_goes_to_the_call_leg_every_90_percent_of_its_interval_and_stops_with_the_call() {
    let lab = Lab::start().await;
    let mut b = lab.client(LAB_B).await;
    let _ = lab.mock.ring(LAB_A, LAB_B);
    let rings = incoming(&mut b).await;
    b.answer(rings.call);
    state(&mut b, CallState::Connected).await;
    let _ = lab.requests_after(LAB_B, "/updateEndpointMetadata").await;

    // the accept's answer asked for a keep-alive every 2700 s: it goes every 2430 s
    b.tick(Instant::now() + Duration::from_secs(2_420));
    let early = b.events_within(Duration::from_millis(300)).await;
    assert!(early.is_empty(), "{early:#?}");
    for round in 1..=2_u32 {
        b.tick(Instant::now() + Duration::from_secs(2_431) * round);
        let due = b
            .next(|event| match event {
                CallEvent::KeepAliveDue { call, sent } => Some((call, sent)),
                _ => None,
            })
            .await;
        assert_eq!(due, (rings.call, true));
        let deadline = tokio::time::Instant::now() + support::WAIT;
        while keep_alives(&lab, LAB_B) < round as usize {
            assert!(tokio::time::Instant::now() < deadline, "keep-alive {round} not taken");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    assert_eq!(keep_alives(&lab, LAB_B), 2);

    b.hang_up(rings.call);
    let _ = ended(&mut b).await;
    b.tick(Instant::now() + Duration::from_secs(10_000));
    let after = b.events_within(Duration::from_millis(300)).await;
    assert!(after.is_empty(), "none after the call ended: {after:#?}");
    assert_eq!(keep_alives(&lab, LAB_B), 2);
}

/// The keep-alives `mri` sent on the mock's one call.
fn keep_alives(lab: &Lab, mri: &str) -> usize {
    only_call(lab)
        .requests
        .iter()
        .filter(|(op, by)| op == "keepAlive" && by == mri)
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_own_origin_call_rings_accepts_and_hangs_up_through_native_signaling() {
    let cloud = teams_proto::Cloud::own("https://calls.example:8443").unwrap();
    let mock = ChatServiceMock::start_for_cloud(cloud.clone()).await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let mut a = Client::start_in(&mock, trouter.origin(), LAB_A, cloud.clone()).await;
    let mut b = Client::start_in(&mock, trouter.origin(), LAB_B, cloud.clone()).await;
    a.place(Placement::Chat {
        callees: vec![Mri::new(LAB_B)],
        group_chat: None,
    });
    state(&mut a, CallState::Ringing).await;
    let ring = incoming(&mut b).await;
    assert_eq!(ring.caller.as_str(), LAB_A);
    assert_eq!(ring.offer.unwrap().blob, support::offer().blob);
    assert_eq!(mock.calls()[0].legs[0].accepted_by, None);
    b.answer(ring.call);
    state(&mut b, CallState::Connected).await;
    let media = a
        .next(|event| match event {
            CallEvent::RemoteMedia { media, .. } => Some(media),
            _ => None,
        })
        .await;
    assert_eq!(media.blob, answer().blob);
    state(&mut a, CallState::Connected).await;
    b.hang_up(ring.call);
    assert_eq!(ended(&mut b).await, End::HungUp);
    assert!(matches!(ended(&mut a).await, End::Remote(_)));
    assert_eq!(mock.calls()[0].phase, CallPhase::Ended);
    let sent = mock.received_requests().await;
    assert!(
        sent.iter()
            .any(|request| request.url.path() == "/flightproxy/api/v2/epconv")
    );
    assert!(sent.iter().any(|request| request.url.path().ends_with("/accept")));
    assert!(sent.iter().any(|request| request.url.path().ends_with("/leave")));
    for request in &sent {
        assert!(
            request.url.path().starts_with("/flightproxy/api/v2/")
                || request.url.path() == "/registrar/prod/V2/registrations"
        );
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            !body.contains("teams.microsoft.com"),
            "no Microsoft service URL in an Own request"
        );
        if let Ok(body) = serde_json::from_slice::<Value>(&request.body) {
            assert_own_callbacks(&cloud, &body);
        }
    }
}

fn assert_own_callbacks(cloud: &teams_proto::Cloud, value: &Value) {
    match value {
        Value::String(text) if text.starts_with("https://") => {
            let url = url::Url::parse(text).unwrap();
            assert!(cloud.contains(&url));
            if text.contains("/callAgent/") {
                assert!(url.path().starts_with("/trouter/v4/f/"));
            }
        }
        Value::Array(values) => values.iter().for_each(|value| assert_own_callbacks(cloud, value)),
        Value::Object(fields) => fields.values().for_each(|value| assert_own_callbacks(cloud, value)),
        _ => {}
    }
}
