//! The machine on its own, fed by hand: what each delivery, outcome and action makes it do (the whole flows on the
//! mock are `tests/mock.rs`).

#[path = "../../tests/support/caption_logging.rs"]
mod caption_logging;

#[path = "../../tests/support/controls.rs"]
mod controls;

#[path = "../../tests/support/decline_ordering.rs"]
mod decline_ordering;

#[path = "../../tests/support/dynamic_callbacks.rs"]
mod dynamic_callbacks;

#[path = "../../tests/support/in_call.rs"]
mod in_call;

#[path = "../../tests/support/meeting_state.rs"]
mod meeting_state;

use std::time::{Duration, Instant};

use crate::{Delivery, Observed};
use base64::Engine as _;
use serde_json::{Value, json};
use teams_proto::Mri;
use teams_proto::{ClientInfo, MediaContent};

use super::{
    CallError, CallEvent, CallId, CallState, Calls, DeliveryKey, DeliveryReply, Effect, End, Identity, Outcome,
    Placement, RENEGOTIATION_RETRY,
};
use crate::CallWrite;
use crate::WriteId;
use crate::agent::Kind;

const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const PUSH: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/";
const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";

fn identity(mri: &str) -> Identity {
    Identity {
        mri: Mri::new(mri),
        display_name: Some("Lab".into()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        language_id: Some("en-us".into()),
        tenant_id: "00000000-0000-4000-8000-0000000000d0".into(),
        client: ClientInfo {
            endpoint_capabilities: Some(1),
            client_endpoint_capabilities: Some(1),
            platform_name: Some("SyntheticNative/1".into()),
        },
        region: Some("amer".into()),
        partition: None,
    }
}

fn media() -> MediaContent {
    MediaContent::new("v=0\r\n", "application/sdp-ngc-1.0")
}

fn conv(rest: &str) -> String {
    format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/Conversation1{rest}?i=10-0-0-2&e=1")
}

fn cc(kind: &str, op: &str) -> String {
    format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/{kind}/Call1/1/x1/1/{op}?i=10-0-0-1")
}

fn delivery(path: &str, body: &Value) -> Delivery {
    Delivery {
        connection: "calling/1".into(),
        id: 7,
        path: path.into(),
        trouter_request: Some("request-7".into()),
        chain_id: Some("chain".into()),
        message_id: Some("message".into()),
        body: body.to_string().into_bytes(),
    }
}

fn effects(calls: &mut Calls) -> Vec<Effect> {
    std::iter::from_fn(|| calls.poll()).collect()
}

/// The effects but the receipts: what a delivery changed (every renegotiation delivered at the call's own callback
/// gets its receipt, a replay's too).
fn changes(calls: &mut Calls) -> Vec<Effect> {
    effects(calls)
        .into_iter()
        .filter(|effect| {
            !matches!(
                effect,
                Effect::Respond {
                    reply: DeliveryReply::Receipt(_),
                    ..
                }
            )
        })
        .collect()
}

/// The deliveries `effects` answer with a receipt.
fn receipts(effects: &[Effect]) -> Vec<DeliveryKey> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Respond {
                delivery,
                reply: DeliveryReply::Receipt(_),
            } => Some(delivery.clone()),
            _ => None,
        })
        .collect()
}

/// The names of the writes among `effects`, with their ids.
fn sent(effects: &[Effect]) -> Vec<(WriteId, &'static str)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send { id, write } => Some((*id, write.name())),
            _ => None,
        })
        .collect()
}

fn events(effects: &[Effect]) -> Vec<CallEvent> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Event(event) => Some(event.clone()),
            _ => None,
        })
        .collect()
}

fn ok(body: &Value) -> Outcome {
    Outcome::Answered {
        status: 200,
        body: body.to_string().into_bytes(),
    }
}

fn conversation_answer() -> Value {
    json!({"conversationController": conv(""), "links": {
        "leave": conv("/leave"), "updateEndpointMetadata": conv("/updateEndpointMetadata"),
        "addParticipantAndModality": conv("/add")}})
}

/// a's machine with a 1:1 call to b placed and its start answered: ringing.
fn ringing_call(now: Instant) -> (Calls, CallId) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let answered = effects(&mut calls);
    assert_eq!(
        events(&answered),
        [CallEvent::State {
            call,
            state: CallState::Ringing
        }]
    );
    (calls, call)
}

fn acceptance_path(calls: &Calls) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "acceptance"))
        .unwrap()
        .to_owned()
}

#[test]
fn call_target_cleanup_matches_only_the_current_calls_callback() {
    let (mut calls, id) = ringing_call(Instant::now());
    let path = acceptance_path(&calls);
    assert_eq!(calls.call_at(&path), Some(id));
    assert_eq!(
        calls.call_at("/v4/f/another/callAgent/old/conversation/rosterUpdate/"),
        None
    );
    calls.call = None;
    assert_eq!(calls.call_at(&path), None);
}

fn acceptance() -> Value {
    json!({"callAcceptance": {
        "links": {"acknowledgement": cc("callAcceptance", "acknowledge")},
        "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        "callKeepAliveInterval": 2700,
    }})
}

#[test]
fn an_action_needs_the_endpoint_and_no_call_up() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    let echo = Placement::TestCall {
        echo: Mri::new("28:00000000-0000-4000-8000-0000000000ec"),
    };
    assert!(matches!(calls.place(&echo, media(), now), Err(CallError::NoEndpoint)));
    assert!(matches!(
        calls.join("19:x@thread.v2", media(), now),
        Err(CallError::NotJoinable)
    ));
    let (mut calls, call) = ringing_call(now);
    assert!(matches!(calls.place(&echo, media(), now), Err(CallError::Busy)));
    assert!(
        matches!(calls.answer(call, media()), Err(CallError::NotNow)),
        "not an invitation"
    );
    assert!(matches!(calls.decline(CallId(99)), Err(CallError::NoSuchCall)));
}

#[test]
fn an_acceptance_is_acknowledged_by_response_and_by_request_only_when_that_failed() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let accepted = effects(&mut calls);
    let Some(Effect::Respond {
        delivery,
        reply: DeliveryReply::Acknowledgement(_),
    }) = accepted.first()
    else {
        panic!("{accepted:#?}");
    };
    assert_eq!((delivery.connection.as_str(), delivery.id), ("calling/1", 7));
    assert!(events(&accepted).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
    assert_eq!(
        sent(&accepted).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["update_endpoint_metadata"]
    );
    let key = delivery.clone();
    calls.on_response_failed(&key);
    let by_request = effects(&mut calls);
    assert_eq!(
        sent(&by_request).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["acknowledge"]
    );
    calls.on_response_failed(&key);
    assert!(effects(&mut calls).is_empty(), "once");
}

/// The keep-alive's period: 90 % of the acceptance's `callKeepAliveInterval` (2700 s), as the web client sends it.
const PERIOD: Duration = Duration::from_secs(2_430);

/// A placed call up by an acceptance whose links name the call leg's own (the keep-alive's).
fn up_with_a_call_leg(now: Instant) -> (Calls, CallId) {
    let (mut calls, call) = ringing_call(now);
    assert_eq!(calls.deadline(), None);
    let path = acceptance_path(&calls);
    let mut accepting = acceptance();
    accepting["callAcceptance"]["links"]["callLeg"] = json!(call_leg());
    calls.on_delivery(&delivery(&path, &accepting), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    (calls, call)
}

#[test]
fn the_keep_alive_goes_to_the_call_leg_every_90_percent_of_its_interval() {
    let now = Instant::now();
    let (mut calls, call) = up_with_a_call_leg(now);
    assert_eq!(calls.keep_alive_interval(), Some(Duration::from_mins(45)));
    assert_eq!(calls.deadline(), Some(now + PERIOD));
    calls.tick(now + Duration::from_secs(2_429));
    assert!(effects(&mut calls).is_empty());
    calls.tick(now + PERIOD);
    let due = effects(&mut calls);
    assert_eq!(events(&due), [CallEvent::KeepAliveDue { call, sent: true }]);
    let first = sent(&due);
    assert_eq!(first.iter().map(|(_, name)| *name).collect::<Vec<_>>(), ["keep_alive"]);
    let Some(Effect::Send { write, .. }) = due.iter().find(|effect| matches!(effect, Effect::Send { .. })) else {
        panic!("{due:#?}");
    };
    assert!(matches!(**write, CallWrite::KeepAlive(_)), "{write:?}");
    // its failing ends nothing; the next one falls due a period later, again and again
    calls.on_outcome(first[0].0, &Outcome::Failed, now + PERIOD);
    assert!(events(&effects(&mut calls)).is_empty());
    assert_eq!(calls.current().map(|(id, _)| id), Some(call));
    assert_eq!(calls.deadline(), Some(now + PERIOD * 2));
    calls.tick(now + PERIOD * 2);
    let again = effects(&mut calls);
    assert_eq!(events(&again), [CallEvent::KeepAliveDue { call, sent: true }]);
    calls.on_outcome(sent(&again)[0].0, &ok(&json!({})), now + PERIOD * 2);
    calls.tick(now + PERIOD * 3);
    assert_eq!(sent(&effects(&mut calls)).len(), 1, "a third one");
}

#[test]
fn no_keep_alive_falls_due_once_the_call_ended() {
    let now = Instant::now();
    let (mut calls, _) = up_with_a_call_leg(now);
    let end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "end"))
        .unwrap()
        .to_owned();
    calls.on_delivery(
        &delivery(&end, &json!({"callEnd": {"code": 0, "phrase": "Done"}})),
        Observed::Ended,
        now,
    );
    let ended = effects(&mut calls);
    let ended_events = events(&ended);
    let [
        CallEvent::Ended {
            end: End::Remote(remote),
            ..
        },
    ] = ended_events.as_slice()
    else {
        panic!("{ended:#?}");
    };
    assert_eq!((remote.code, remote.phrase.as_deref()), (Some(0), Some("Done")));
    assert_eq!(calls.deadline(), None);
    assert_eq!(calls.current(), None);
    for period in 1..=3 {
        calls.tick(now + PERIOD * period);
        assert!(effects(&mut calls).is_empty(), "none after the call ended");
    }
}

#[test]
fn a_call_without_a_call_leg_link_tells_its_keep_alive_due_and_sends_nothing() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    calls.tick(now + PERIOD);
    let due = effects(&mut calls);
    assert_eq!(events(&due), [CallEvent::KeepAliveDue { call, sent: false }]);
    assert!(sent(&due).is_empty());
    assert_eq!(calls.deadline(), Some(now + PERIOD * 2));
}

/// A call leg's own link, the keep-alive's.
fn call_leg() -> String {
    format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/active/Call1/1/a1/3?i=10-0-0-1&e=1")
}

#[test]
fn an_acceptance_the_tracker_bound_nothing_of_ends_the_call_with_a_cancel() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::AcceptanceUnbound, now);
    let failed = effects(&mut calls);
    assert_eq!(
        sent(&failed).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["leave"]
    );
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("acknowledge")
    }));
    assert!(!failed.iter().any(|effect| matches!(effect, Effect::Respond { .. })));
}

#[test]
fn a_start_takes_the_number_reserved_for_it_and_a_later_call_is_numbered_after_it() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    // a reservation no start took (its media failed) is replaced by the next
    let unused = calls.reserve();
    let reserved = calls.reserve();
    assert_ne!(unused, reserved);
    let call = calls.place(&placement, media(), now).unwrap();
    assert_eq!(call, reserved);
    assert!(
        events(&effects(&mut calls)).contains(&CallEvent::State {
            call,
            state: CallState::Placing
        }),
        "the machine tells the reserved call"
    );
    calls.hang_up(call, now).unwrap();
    drop(effects(&mut calls));
    assert!(calls.reserve() > call, "numbers only go up");
}

#[test]
fn a_failed_start_ends_the_call_and_nothing_is_sent_again() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let [(start, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(
        start,
        &Outcome::Answered {
            status: 500,
            body: Vec::new(),
        },
        now,
    );
    let failed = effects(&mut calls);
    assert!(sent(&failed).is_empty());
    assert_eq!(
        events(&failed),
        [CallEvent::Ended {
            call,
            end: End::Failed("start")
        }]
    );
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    assert!(effects(&mut calls).is_empty(), "an outcome of an ended call is dropped");
}

#[test]
fn a_hang_up_before_the_start_was_answered_cancels_once_it_is() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let [(start, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    let asked = effects(&mut calls);
    assert!(sent(&asked).is_empty(), "no leave link yet");
    calls.on_outcome(start, &ok(&conversation_answer()), now + Duration::from_secs(3));
    let answered = effects(&mut calls);
    let [(leave, "leave")] = sent(&answered)[..] else {
        panic!("{answered:#?}");
    };
    calls.on_outcome(leave, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended { call, end: End::HungUp }]
    );
}

/// a's machine with a 1:1 call to b placed, its start handed out and unanswered.
fn placing_call(now: Instant) -> (Calls, CallId, WriteId) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let [(start, "start")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    (calls, call, start)
}

#[test]
fn a_media_failure_before_the_start_was_answered_leaves_once_it_is() {
    let now = Instant::now();
    let (mut calls, call, start) = placing_call(now);
    calls.fail_media(call, now);
    let failed = effects(&mut calls);
    assert!(sent(&failed).is_empty(), "no leave link yet");
    assert_eq!(
        events(&failed),
        [CallEvent::State {
            call,
            state: CallState::Leaving
        }]
    );
    assert_eq!(
        calls.current(),
        Some((call, Some(CallState::Leaving))),
        "kept for the start's answer"
    );
    assert!(!calls.may_send_again(start), "the call leaves");
    calls.fail_media(call, now);
    assert!(effects(&mut calls).is_empty(), "leaving already");

    // the service took the start: the call it opened is left
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let answered = effects(&mut calls);
    let [(leave, "leave")] = sent(&answered)[..] else {
        panic!("{answered:#?}");
    };
    assert!(
        !events(&answered).iter().any(|event| matches!(
            event,
            CallEvent::State {
                state: CallState::Ringing,
                ..
            }
        )),
        "never told ringing: its devices never open"
    );
    calls.on_outcome(leave, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("media")
        }]
    );
    assert_eq!(calls.current(), None);
}

#[test]
fn a_start_that_fails_after_the_call_began_leaving_ends_it_for_what_it_left_for() {
    let now = Instant::now();
    let refused = Outcome::Failed;
    let (mut calls, call, start) = placing_call(now);
    calls.fail_media(call, now);
    let _ = effects(&mut calls);
    calls.on_outcome(start, &refused, now);
    let ended = effects(&mut calls);
    assert!(sent(&ended).is_empty(), "nothing to leave");
    assert_eq!(
        events(&ended),
        [CallEvent::Ended {
            call,
            end: End::Failed("media")
        }]
    );

    let (mut calls, call, start) = placing_call(now);
    calls.hang_up(call, now).unwrap();
    let _ = effects(&mut calls);
    calls.on_outcome(start, &refused, now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended { call, end: End::HungUp }]
    );
}

/// An invitation to `callee` from a, its attach link on `Call1`.
fn invitation(callee: &str) -> Value {
    let gp = json!({
        "callNotification": {
            "from": {"id": A, "displayName": "Lab A"},
            "to": {"id": callee},
            "links": {"attach": cc("forked", "attach")},
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
        "conversationInvitation": {"conversationController": conv("")},
    });
    json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

#[test]
fn an_invitation_whose_attach_failed_for_good_or_whose_answer_doesn_t_read_is_let_go() {
    let now = Instant::now();
    let root = "/v4/f/SyntheticEndpoint00000/";
    let outcomes = [
        Outcome::Failed,
        failed(),
        Outcome::Answered {
            status: 401,
            body: Vec::new(),
        },
        ok(&json!({})),
    ];
    for outcome in outcomes {
        let mut calls = Calls::new(identity(B));
        calls.set_push_url(PUSH).unwrap();
        calls.on_delivery(&delivery(root, &invitation(B)), Observed::Offered, now);
        let [(attach, "attach")] = sent(&effects(&mut calls))[..] else {
            panic!("{outcome:?}");
        };
        // the engine's last word on the attach: no further attempt goes
        calls.on_outcome(attach, &outcome, now);
        let dropped = effects(&mut calls);
        let let_go: Vec<&str> = dropped
            .iter()
            .filter_map(|effect| match effect {
                Effect::LetGo { attach } => Some(attach.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(let_go, [cc("forked", "attach")], "{outcome:?}");
        assert!(events(&dropped).is_empty(), "never shown: {outcome:?}");
        assert!(sent(&dropped).is_empty(), "nothing sent for it: {outcome:?}");
        assert_eq!(calls.current(), None);
    }
    // the engine stopping while it is attached lets it go too
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    calls.on_delivery(&delivery(root, &invitation(B)), Observed::Offered, now);
    let _ = effects(&mut calls);
    calls.shut_down(now);
    assert!(
        effects(&mut calls)
            .iter()
            .any(|effect| matches!(effect, Effect::LetGo { .. })),
        "the engine stopping while it attaches"
    );
}

#[test]
fn only_an_offered_invitation_to_the_account_gets_its_receipt_and_attach_and_rings_once_attached() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    let root = "/v4/f/SyntheticEndpoint00000/";
    calls.on_delivery(&delivery(root, &invitation(B)), Observed::Ignored, now);
    calls.on_delivery(&delivery(root, &invitation(A)), Observed::Offered, now);
    assert!(
        effects(&mut calls).is_empty(),
        "nothing for one not offered or not the account's"
    );

    calls.on_delivery(&delivery(root, &invitation(B)), Observed::Offered, now);
    let offered = effects(&mut calls);
    assert!(matches!(
        offered.first(),
        Some(Effect::Respond {
            reply: DeliveryReply::Receipt(_),
            ..
        })
    ));
    let [(attach, "attach")] = sent(&offered)[..] else {
        panic!("{offered:#?}");
    };
    assert!(events(&offered).is_empty(), "shown only once attached");
    let (call, _) = calls.current().unwrap();
    assert!(matches!(calls.answer(call, media()), Err(CallError::NotNow)));

    let attached = json!({
        "callInvitation": {"links": {"progress": cc("incoming", "progress"),
            "acceptance": cc("incoming", "accept"), "callLeg": cc("incoming", "reject")}},
        "additionalActionResponses": [{"url": conv(""), "output": {"links": {
            "leave": conv("/leave"), "updateEndpointState": conv("/updateEndpointState")}}}],
    });
    calls.on_outcome(attach, &ok(&attached), now);
    let ringing = effects(&mut calls);
    assert_eq!(
        sent(&ringing).iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        ["progress"]
    );
    let rung = events(&ringing);
    let [CallEvent::Incoming(incoming)] = rung.as_slice() else {
        panic!("{ringing:#?}");
    };
    assert_eq!((incoming.caller.as_str(), incoming.call), (A, call));

    // its redelivery gets a receipt again, nothing more
    calls.on_delivery(&delivery(root, &invitation(B)), Observed::Offered, now);
    let again = effects(&mut calls);
    assert!(sent(&again).is_empty());
    assert_eq!(again.len(), 1);

    calls.answer(call, media()).unwrap();
    let answering = effects(&mut calls);
    let [(state, "update_endpoint_state")] = sent(&answering)[..] else {
        panic!("{answering:#?}");
    };
    calls.on_outcome(state, &ok(&json!({})), now);
    assert_eq!(
        names(&effects(&mut calls)),
        ["accept"],
        "the accept once the state was taken"
    );
    assert!(matches!(calls.hang_up(call, now), Ok(())));
}

fn names(effects: &[Effect]) -> Vec<&'static str> {
    sent(effects).iter().map(|(_, name)| *name).collect()
}

fn failed() -> Outcome {
    Outcome::Answered {
        status: 500,
        body: Vec::new(),
    }
}

#[test]
fn a_meeting_goes_from_its_start_to_joining_rings_nobody_and_connects_on_its_acceptance() {
    let meeting = teams_proto::MeetingInfo {
        organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
        tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
    };
    let placement = Placement::Meeting {
        chat: "19:meeting_MadeUpMeeting0001@thread.v2".to_owned(),
        meeting,
    };
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::State {
            call,
            state: CallState::Joining
        }],
        "nobody rings"
    );
}

#[test]
fn a_meeting_code_start_uses_group_media_and_enters_without_ringing() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let code = teams_proto::MeetingCode::typed("123456789012", "MadeUp").unwrap();
    let call = calls.place(&Placement::MeetingCode(code), media(), now).unwrap();
    let placed = effects(&mut calls);
    let start = placed
        .iter()
        .find_map(|effect| match effect {
            Effect::Send { id, write } => Some((*id, write)),
            _ => None,
        })
        .unwrap();
    let body = body_of(start.1);
    assert_eq!(
        body["callInvitation"]["callModalities"],
        json!(["Audio", "ScreenViewer"])
    );
    assert_eq!(body["meetingData"]["meetingCode"], "123456789012");
    let chat = "19:meeting_SyntheticCode@thread.v2";
    let reference = format!("https://teams.microsoft.com/l/meetup-join/{chat}/0");
    let html = format!(
        r#"<a id="meet_invite_block.action.join_link" href="https://teams.microsoft.com/meet/123456789012?p=MadeUp">Join</a><a id="meet_invite_block.action.join_link_compatibility" href="{reference}">System reference</a>"#
    );
    let invitation: String = url::form_urlencoded::byte_serialize(html.as_bytes()).collect();
    let mut answer = conversation_answer();
    answer["meetingDetails"] = json!({"invitation": invitation});
    calls.on_outcome(start.0, &ok(&answer), now);
    let told = events(&effects(&mut calls));
    assert!(
        told.iter()
            .any(|event| matches!(event, CallEvent::State { call: id, state: CallState::Joining } if *id == call))
    );
    assert!(
        told.iter()
            .any(|event| matches!(event, CallEvent::GroupChat { call: id, chat: at } if *id == call && at == chat))
    );
    assert_eq!(calls.call.as_ref().unwrap().caption_thread.as_deref(), Some(chat));
}

fn body_of(write: &crate::CallWrite) -> serde_json::Value {
    use teams_proto::Request as _;
    match write {
        crate::CallWrite::Start(start) => start.body().unwrap(),
        crate::CallWrite::Join(join) => join.body().unwrap(),
        other => panic!("{}", other.name()),
    }
}

#[test]
fn meet_now_subscribes_to_a_new_meeting_then_enters_it_as_its_organizer() {
    const CHANNEL: &str = "19:MadeUpChannel0001@thread.tacv2";
    let placement = Placement::Channel {
        channel: CHANNEL.to_owned(),
    };
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(subscription, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    let Some(Effect::Send { write, .. }) = placed.iter().find(|effect| matches!(effect, Effect::Send { .. })) else {
        panic!("{placed:#?}");
    };
    assert_eq!(body_of(write)["groupChat"]["threadId"], CHANNEL);
    // the new meeting's conversation: entered at once, the offer kept for it
    calls.on_outcome(subscription, &ok(&conversation_answer()), now);
    let entered = effects(&mut calls);
    let [(entry, "join")] = sent(&entered)[..] else {
        panic!("{entered:#?}");
    };
    let Some(Effect::Send { write, .. }) = entered.iter().find(|effect| matches!(effect, Effect::Send { .. })) else {
        panic!("{entered:#?}");
    };
    assert_eq!(body_of(write)["groupChat"]["threadId"], CHANNEL);
    calls.on_outcome(entry, &ok(&conversation_answer()), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::State {
            call,
            state: CallState::Joining
        }],
        "nobody rings"
    );
}

#[test]
fn meet_now_whose_subscription_names_no_conversation_ends() {
    let placement = Placement::Channel {
        channel: "19:MadeUpChannel0001@thread.tacv2".to_owned(),
    };
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(subscription, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    calls.on_outcome(subscription, &ok(&json!({})), now);
    let after = effects(&mut calls);
    assert!(sent(&after).iter().all(|(_, name)| *name != "join"), "{after:#?}");
    assert!(
        events(&after)
            .iter()
            .any(|event| matches!(event, CallEvent::Ended { .. })),
        "{after:#?}"
    );
}

#[test]
fn a_channel_s_running_meeting_is_joined_at_its_conversation_by_its_post() {
    const CHANNEL: &str = "19:MadeUpChannel0001@thread.tacv2";
    let meetup = teams_proto::ChannelMeetup {
        message_id: "1793680050510".to_owned(),
        conversation: teams_proto::CallLink::parse(&conv("")).unwrap(),
        meeting: teams_proto::MeetingInfo {
            organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
            tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
        },
    };
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let call = calls.join_channel_meeting(CHANNEL, &meetup, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(entry, "join")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    let Some(Effect::Send { write, .. }) = placed.iter().find(|effect| matches!(effect, Effect::Send { .. })) else {
        panic!("{placed:#?}");
    };
    assert_eq!(body_of(write)["groupChat"]["threadId"], CHANNEL);
    assert!(
        matches!(
            calls.join_channel_meeting(CHANNEL, &meetup, media(), now),
            Err(CallError::Busy)
        ),
        "one call at a time"
    );
    calls.on_outcome(entry, &ok(&conversation_answer()), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::State {
            call,
            state: CallState::Joining
        }]
    );
}

#[test]
fn meet_now_hung_up_before_its_subscription_is_answered_never_enters_the_meeting() {
    let placement = Placement::Channel {
        channel: "19:MadeUpChannel0001@thread.tacv2".to_owned(),
    };
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(subscription, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    calls.hang_up(call, now).unwrap();
    effects(&mut calls);
    // the new meeting's conversation comes after the hang-up: no join (which would post the meeting to the channel)
    calls.on_outcome(subscription, &ok(&conversation_answer()), now);
    let after = effects(&mut calls);
    assert!(sent(&after).is_empty(), "{after:#?}");
    assert!(
        events(&after)
            .iter()
            .any(|event| matches!(event, CallEvent::Ended { call: ended, .. } if *ended == call)),
        "{after:#?}"
    );
}

/// a's machine in a meeting's call, its start answered with `roster` and the meeting conversation's lobby links.
fn meeting_answered(roster: Value, now: Instant) -> (Calls, CallId, Vec<CallEvent>) {
    let placement = Placement::Meeting {
        chat: "19:meeting_MadeUpMeeting0001@thread.v2".to_owned(),
        meeting: teams_proto::MeetingInfo {
            organizer_id: "00000000-0000-4000-8000-00000000000a".to_owned(),
            tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
        },
    };
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    let mut answer = conversation_answer();
    answer["links"]["admit"] = json!(conv("/admit"));
    answer["links"]["removeParticipant"] = json!(conv("/removeParticipant"));
    answer["roster"] = roster;
    calls.on_outcome(start, &ok(&answer), now);
    let told = events(&effects(&mut calls));
    (calls, call, told)
}

/// A roster entry waiting in the lobby: an endpoint with a `lobby`, none with a `call`.
fn in_lobby(version: u64, name: &str) -> Value {
    json!({"version": version, "state": "active", "details": {"displayName": name},
        "endpoints": {"e": {"lobby": {"mediaStreams": []}, "modalityJoined": "Lobby"}}})
}

#[test]
fn a_meeting_tells_who_waits_in_its_lobby_and_admits_or_denies_them_at_its_links() {
    let now = Instant::now();
    let roster = json!({"type": "Delta", "participants": {
        A: {"version": 2, "state": "active", "endpoints": {"e": {"call": {}}}},
        B: in_lobby(4, "Lab Bee"),
    }});
    let (mut calls, call, told) = meeting_answered(roster, now);
    let waiting = told
        .iter()
        .find_map(|event| match event {
            CallEvent::Lobby { call: of, waiting } if *of == call => Some(waiting.clone()),
            _ => None,
        })
        .expect("the lobby told");
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].mri, Mri::new(B));
    assert!(
        !told.iter().any(|event| matches!(event, CallEvent::Participants { .. })),
        "nobody waiting is in the call: {told:#?}"
    );
    // nobody else waits
    assert!(matches!(
        calls.decide_lobby(call, &Mri::new("8:orgid:00000000-0000-4000-8000-00000000000c"), true),
        Err(CallError::NotNow)
    ));
    calls.decide_lobby(call, &Mri::new(B), true).unwrap();
    calls.decide_lobby(call, &Mri::new(B), false).unwrap();
    let decided = effects(&mut calls);
    let names: Vec<&str> = sent(&decided).iter().map(|(_, name)| *name).collect();
    assert_eq!(names, ["lobby_decision", "lobby_decision"]);
    // let in: the lobby is empty again, and b is in the call
    let roster_path = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "rosterUpdate"))
        .unwrap()
        .to_owned();
    let admitted = json!({"participants": {B: {"version": 5, "state": "active",
        "endpoints": {"e": {"call": {}}}}}});
    calls.on_delivery(&delivery(&roster_path, &admitted), Observed::Nothing, now);
    let after = events(&effects(&mut calls));
    assert!(after.contains(&CallEvent::Lobby {
        call,
        waiting: Vec::new()
    }));
    assert!(
        after
            .iter()
            .any(|event| matches!(event, CallEvent::Participants { participants, .. } if participants.len() == 1))
    );
}

#[test]
fn a_meeting_tells_the_account_waits_in_its_lobby_until_it_is_let_in() {
    let now = Instant::now();
    let roster = json!({"type": "Delta", "participants": {A: in_lobby(1, "Lab A")}});
    let (mut calls, call, told) = meeting_answered(roster, now);
    assert!(told.contains(&CallEvent::InLobby { call, waiting: true }), "{told:#?}");
    let roster_path = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "rosterUpdate"))
        .unwrap()
        .to_owned();
    let admitted = json!({"participants": {A: {"version": 2, "state": "active",
        "endpoints": {"e": {"call": {}}}}}});
    calls.on_delivery(&delivery(&roster_path, &admitted), Observed::Nothing, now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::InLobby { call, waiting: false }]
    );
}

#[test]
fn a_meetings_running_call_tells_who_is_in_it_from_the_starts_answer() {
    let meeting = teams_proto::MeetingInfo {
        organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
        tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
    };
    let placement = Placement::Meeting {
        chat: "19:meeting_MadeUpMeeting0001@thread.v2".to_owned(),
        meeting,
    };
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    let mut answer = conversation_answer();
    answer["roster"] = json!({"type": "Delta", "participants": {
        A: {"version": 2, "state": "active", "endpoints": {"e": {"call": {}}}},
        B: {"version": 7, "state": "active", "details": {"displayName": "Lab Bee"}, "endpoints": {"e": {"call": {}}}},
    }});
    calls.on_outcome(start, &ok(&answer), now);
    let told = events(&effects(&mut calls));
    let [
        CallEvent::Participants { call: of, participants },
        CallEvent::State { .. },
    ] = &told[..]
    else {
        panic!("{told:#?}");
    };
    assert_eq!(*of, call);
    assert_eq!(participants.len(), 1, "the account left out");
    assert_eq!(participants[0].mri, Mri::new(B));
    assert_eq!(participants[0].name.as_deref(), Some("Lab Bee"));
}

/// An invitation to `callee` from a in the group chat `chat`, its conversation `Conversation<n>`.
fn group_invitation(callee: &str, chat: &str, n: u8) -> Value {
    let gp = json!({
        "callNotification": {
            "from": {"id": A, "displayName": "Lab A"},
            "to": {"id": callee},
            "links": {"attach": cc("forked", "attach")},
        },
        "conversationInvitation": {"conversationController": conv("").replace("Conversation1", &format!("Conversation{n}"))},
        "groupChat": {"threadId": chat},
    });
    json!({"evt": 109, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

/// b's machine with a's invitation offered (its `attach` answered): it rings here.
fn offered_call(now: Instant) -> (Calls, CallId) {
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &invitation(B)),
        Observed::Offered,
        now,
    );
    let [(attach, "attach")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    let attached = json!({
        "callInvitation": {"links": {"progress": cc("incoming", "progress"),
            "acceptance": cc("incoming", "accept"), "callLeg": cc("incoming", "reject")}},
        "additionalActionResponses": [{"url": conv(""), "output": {"links": {
            "leave": conv("/leave"), "updateEndpointState": conv("/updateEndpointState")}}}],
    });
    calls.on_outcome(attach, &ok(&attached), now);
    let ringing = effects(&mut calls);
    let [CallEvent::Incoming(incoming)] = &events(&ringing)[..] else {
        panic!("{ringing:#?}");
    };
    (calls, incoming.call)
}

#[test]
fn a_failed_endpoint_state_ends_the_answer_and_its_accept_never_goes() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let answering = effects(&mut calls);
    let [(state, "update_endpoint_state")] = sent(&answering)[..] else {
        panic!("{answering:#?}");
    };
    calls.on_outcome(state, &failed(), now);
    let ended = effects(&mut calls);
    assert_eq!(names(&ended), ["leave"], "no accept after the call was over");
    assert_eq!(
        events(&ended).last(),
        Some(&CallEvent::Ended {
            call,
            end: End::Failed("update_endpoint_state")
        })
    );
}

#[test]
fn a_hang_up_before_the_accept_went_declines_and_never_leaves() {
    let now = Instant::now();
    // the endpoint state handed out, the accept held for its answer
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let [(state, "update_endpoint_state")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    let hung_up = effects(&mut calls);
    let [(reject, "reject")] = sent(&hung_up)[..] else {
        panic!("{hung_up:#?}");
    };
    assert_eq!(
        events(&hung_up),
        [CallEvent::State {
            call,
            state: CallState::Declining
        }]
    );
    calls.on_outcome(state, &ok(&json!({})), now);
    assert!(effects(&mut calls).is_empty(), "the held accept never goes");
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );

    // the endpoint state not taken by the engine yet: it never goes, only the decline
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    calls.hang_up(call, now).unwrap();
    let hung_up = effects(&mut calls);
    let [(reject, "reject")] = sent(&hung_up)[..] else {
        panic!("{hung_up:#?}");
    };
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );

    // the endpoint state failing after the hang-up: the decline goes on and ends the call
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let [(state, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    let [(reject, "reject")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(state, &failed(), now);
    assert!(effects(&mut calls).is_empty(), "the decline goes on");
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );
}

#[test]
fn a_hang_up_once_the_accept_went_leaves() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.answer(call, media()).unwrap();
    let [(state, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(state, &ok(&json!({})), now);
    let [(_, "accept")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.hang_up(call, now).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["leave"]);
}

#[test]
fn the_call_s_end_delivered_before_its_leave_s_answer_ends_it_as_hung_up() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let _ = effects(&mut calls);
    calls.hang_up(call, now).unwrap();
    let leaving = effects(&mut calls);
    assert_eq!(names(&leaving), ["leave"]);
    let end = callback(&calls, "end");
    let body = json!({"callEnd": {"code": 0, "phrase": "CallEndReasonLocalUserInitiated", "reason": "noError"}});
    calls.on_delivery(&delivery(&end, &body), Observed::Ended, now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended { call, end: End::HungUp }]
    );

    // a call nobody is leaving that ends that way ended at the far end
    let (mut calls, call) = connected_call(now);
    let end = callback(&calls, "end");
    calls.on_delivery(&delivery(&end, &body), Observed::Ended, now);
    assert!(matches!(
        &events(&effects(&mut calls))[..],
        [.., CallEvent::Ended { call: of, end: End::Remote(_) }] if *of == call
    ));
}

#[test]
fn writes_still_queued_when_the_call_ends_never_go() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    // the engine hasn't taken the endpoint metadata yet when the call's end comes
    let end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "end"))
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&end, &json!({"callEnd": {"code": 0}})), Observed::Ended, now);
    let all = effects(&mut calls);
    assert!(sent(&all).is_empty(), "{all:#?}");
    assert!(matches!(
        events(&all).last(),
        Some(CallEvent::Ended {
            end: End::Remote(_),
            ..
        })
    ));
    assert!(events(&all).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
}

/// a's machine with the test call placed, its start answered and its acceptance delivered: connected.
fn connected_test_call(now: Instant) -> (Calls, CallId, Vec<Effect>) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let echo = Placement::TestCall {
        echo: Mri::new("28:00000000-0000-4000-8000-0000000000ec"),
    };
    let call = calls.place(&echo, media(), now).unwrap();
    let [(start, "start")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let _ = effects(&mut calls);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let accepted = effects(&mut calls);
    (calls, call, accepted)
}

#[test]
fn the_echo_is_added_only_once_the_endpoint_metadata_was_taken() {
    let now = Instant::now();
    let (mut calls, _, accepted) = connected_test_call(now);
    let [(metadata, "update_endpoint_metadata")] = sent(&accepted)[..] else {
        panic!("{accepted:#?}");
    };
    calls.on_outcome(metadata, &ok(&json!({})), now);
    assert_eq!(names(&effects(&mut calls)), ["add"]);

    let (mut calls, call, accepted) = connected_test_call(now);
    let [(metadata, _)] = sent(&accepted)[..] else { panic!() };
    calls.on_outcome(metadata, &Outcome::Failed, now);
    let ended = effects(&mut calls);
    assert_eq!(names(&ended), ["leave"], "no add to a call that is over");
    assert_eq!(
        events(&ended).last(),
        Some(&CallEvent::Ended {
            call,
            end: End::Failed("update_endpoint_metadata")
        })
    );
}

/// A roster update naming the echo `state` (`active` with an endpoint in the call, or `inactive`), at `version`.
fn echo_roster(state: &str, version: u64) -> Value {
    json!({"participants": {"28:00000000-0000-4000-8000-0000000000ec": {
        "version": version,
        "state": state,
        "details": {"displayName": ""},
        "endpoints": {"made-up-endpoint": {"call": {"mediaStreams": [{"label": "main-audio", "sourceId": 1}]}}},
    }}})
}

#[test]
fn the_test_call_ends_by_itself_once_the_echo_left() {
    let now = Instant::now();
    let (mut calls, call, _) = connected_test_call(now);
    let roster = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "rosterUpdate"))
        .unwrap()
        .to_owned();
    // a roster without the echo before it joined ends nothing
    calls.on_delivery(&delivery(&roster, &json!({"participants": {}})), Observed::Nothing, now);
    assert!(names(&effects(&mut calls)).is_empty());
    calls.on_delivery(&delivery(&roster, &echo_roster("active", 1)), Observed::Nothing, now);
    let joined = effects(&mut calls);
    assert!(names(&joined).is_empty(), "{joined:#?}");
    // the echo played the recording back and left: the call leaves, and ends as the far end's once its leave is taken
    calls.on_delivery(&delivery(&roster, &echo_roster("inactive", 2)), Observed::Nothing, now);
    let left = effects(&mut calls);
    let [(leave, "leave")] = sent(&left)[..] else {
        panic!("{left:#?}");
    };
    calls.on_outcome(leave, &ok(&json!({})), now);
    assert!(matches!(
        events(&effects(&mut calls)).last(),
        Some(CallEvent::Ended {
            call: ended,
            end: End::Remote(_),
        }) if *ended == call
    ));
}

#[test]
fn a_chat_s_call_doesn_t_end_when_someone_leaves_its_roster() {
    let now = Instant::now();
    let (mut calls, _) = ringing_call(now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    let roster = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "rosterUpdate"))
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&roster, &echo_roster("active", 1)), Observed::Nothing, now);
    calls.on_delivery(&delivery(&roster, &echo_roster("inactive", 2)), Observed::Nothing, now);
    assert!(!names(&effects(&mut calls)).contains(&"leave"));
}

#[test]
fn an_acceptance_before_the_start_s_answer_is_applied_once_it_came() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let [(start, "start")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    assert!(effects(&mut calls).is_empty(), "held until the start's answer");

    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let answered = effects(&mut calls);
    assert!(
        answered.iter().any(|effect| matches!(
            effect,
            Effect::Respond {
                reply: DeliveryReply::Acknowledgement(_),
                ..
            }
        )),
        "{answered:#?}"
    );
    let told = events(&answered);
    assert_eq!(
        told.first(),
        Some(&CallEvent::State {
            call,
            state: CallState::Ringing
        })
    );
    assert!(told.iter().any(|event| matches!(event, CallEvent::RemoteMedia { .. })));
    assert_eq!(
        told.last(),
        Some(&CallEvent::State {
            call,
            state: CallState::Connected
        })
    );
    assert_eq!(names(&answered), ["update_endpoint_metadata"]);
}

#[test]
fn a_decline_or_a_leave_that_wasn_t_taken_ends_the_call_failed() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.decline(call).unwrap();
    let [(reject, "reject")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(reject, &Outcome::Failed, now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("reject")
        }]
    );

    let (mut calls, call) = offered_call(now);
    calls.decline(call).unwrap();
    let [(reject, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(reject, &ok(&json!({})), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Declined
        }]
    );

    let (mut calls, call) = ringing_call(now);
    calls.hang_up(call, now).unwrap();
    let [(leave, "leave")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(leave, &failed(), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("leave")
        }]
    );
}

const CHAT: &str = "19:00000000000000000000000000000001@thread.v2";

/// b's machine told of a's group call in [`CHAT`] (its ring ended): joinable.
fn told_of_a_group_call(now: Instant) -> Calls {
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &group_invitation(B, CHAT, 1)),
        Observed::Offered,
        now,
    );
    let [(attach, "attach")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(attach, &failed(), now);
    let _ = effects(&mut calls);
    assert_eq!(calls.current(), None);
    assert!(calls.joinable(CHAT));
    calls
}

#[test]
fn a_running_call_is_forgotten_once_a_join_to_it_fails_or_its_conversation_ended() {
    let now = Instant::now();
    let mut calls = told_of_a_group_call(now);
    let call = calls.join(CHAT, media(), now).unwrap();
    let [(join, "join")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(join, &failed(), now);
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Ended {
            call,
            end: End::Failed("join")
        }]
    );
    assert!(!calls.joinable(CHAT), "a failed join forgets it");
    assert!(matches!(calls.join(CHAT, media(), now), Err(CallError::NotJoinable)));

    let mut calls = told_of_a_group_call(now);
    calls.join(CHAT, media(), now).unwrap();
    let [(join, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(join, &ok(&conversation_answer()), now);
    let _ = effects(&mut calls);
    let conversation_end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "conversationEnd"))
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&conversation_end, &json!({})), Observed::Ended, now);
    let _ = effects(&mut calls);
    assert_eq!(calls.current(), None);
    assert!(!calls.joinable(CHAT), "its conversation's end forgets it");

    // a newer invitation in the chat isn't forgotten by the older call's end
    let mut calls = told_of_a_group_call(now);
    calls.join(CHAT, media(), now).unwrap();
    let [(join, _)] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    calls.on_outcome(join, &ok(&conversation_answer()), now);
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &group_invitation(B, CHAT, 2)),
        Observed::Offered,
        now,
    );
    let _ = effects(&mut calls);
    let conversation_end = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, "conversationEnd"))
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&conversation_end, &json!({})), Observed::Ended, now);
    let _ = effects(&mut calls);
    assert!(calls.joinable(CHAT), "the newer call stays joinable");
}

/// a's 1:1 call to b, placed and accepted (its acceptance naming the call controller's `mediaRenegotiation` link):
/// connected, its effects so far taken.
fn connected_call(now: Instant) -> (Calls, CallId) {
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["mediaRenegotiation"] = json!(cc("active", "renegotiate"));
    calls.on_delivery(&delivery(&path, &accepted), Observed::Acceptance, now);
    let connected = effects(&mut calls);
    assert!(events(&connected).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
    (calls, call)
}

/// The path of the call's callback `name` (`call/<name>/`), the first made.
fn callback(calls: &Calls, name: &str) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, name))
        .unwrap()
        .to_owned()
}

#[test]
fn the_account_s_renegotiation_goes_to_the_controller_s_link_and_its_answer_is_told_and_acknowledged() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    let asked = effects(&mut calls);
    let [(renegotiation, "renegotiate")] = sent(&asked)[..] else {
        panic!("{asked:#?}");
    };
    assert!(
        matches!(calls.renegotiate(call, &media()), Err(CallError::NotNow)),
        "one at a time"
    );
    calls.on_outcome(renegotiation, &ok(&json!({})), now);
    assert!(effects(&mut calls).is_empty());

    let answer = json!({"mediaAnswer": {
        "mediaContent": {"blob": "v=1\r\n", "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAcknowledgement": cc("mediaAnswers", "acknowledge")},
    }});
    let at = latest_callback(&calls, "mediaAnswer");
    calls.on_delivery(&delivery(&at, &answer), Observed::Renegotiation, now);
    let answered = effects(&mut calls);
    assert_eq!(names(&answered), ["media_acknowledge"]);
    let told = events(&answered);
    let [
        CallEvent::RemoteMedia {
            call: of,
            media: told_media,
        },
    ] = told.as_slice()
    else {
        panic!("{answered:#?}");
    };
    assert_eq!((*of, told_media.blob.as_str()), (call, "v=1\r\n"));
    calls.on_delivery(&delivery(&at, &answer), Observed::Renegotiation, now);
    assert!(changes(&mut calls).is_empty(), "no renegotiation waits for it");
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"], "and another may go");
}

#[test]
fn a_renegotiation_begins_before_its_offer_and_takes_no_other_meanwhile() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.begin_renegotiation(call).unwrap();
    assert!(
        matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)),
        "one at a time, before its offer is made"
    );
    calls.give_up_renegotiation(call);
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"]);
    assert!(
        matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)),
        "nor while its answer waits"
    );

    // the far end's renegotiation while the account's is being made waits for it: told once the account's is given
    // up
    let (mut calls, call) = connected_call(now);
    calls.begin_renegotiation(call).unwrap();
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let waits = effects(&mut calls);
    assert!(sent(&waits).is_empty() && events(&waits).is_empty(), "{waits:#?}");
    calls.give_up_renegotiation(call);
    assert!(matches!(
        events(&effects(&mut calls))[..],
        [CallEvent::MediaOffer { call: of, .. }] if of == call
    ));
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
}

#[test]
fn the_far_end_s_renegotiation_while_the_account_s_waits_for_its_answer_is_told_after_that_answer() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    drop(effects(&mut calls));
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let waits = effects(&mut calls);
    assert!(
        events(&waits).is_empty(),
        "the call goes on, the far end's waits: {waits:#?}"
    );
    // delivered again meanwhile: a replay
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    assert!(events(&effects(&mut calls)).is_empty());
    // the account's answer comes: applied first, then the far end's is told
    let answered = latest_callback(&calls, "mediaAnswer");
    calls.on_delivery(
        &delivery(&answered, &media_answer("v=1\r\n")),
        Observed::Renegotiation,
        now,
    );
    let told = events(&effects(&mut calls));
    assert!(
        matches!(
            told[..],
            [CallEvent::RemoteMedia { .. }, CallEvent::MediaOffer { call: of, .. }] if of == call
        ),
        "{told:#?}"
    );
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
}

#[test]
fn the_far_end_s_renegotiation_before_the_account_s_is_turned_away_goes_first_and_the_account_s_gives_way() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    let asked = effects(&mut calls);
    let [(first, "renegotiate")] = sent(&asked)[..] else {
        panic!("{asked:#?}");
    };
    // both ends turned their cameras on together: the far end's reached the call controller first, which delivers
    // it, then turns the account's away
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    assert!(events(&effects(&mut calls)).is_empty(), "the call goes on");
    calls.on_outcome(first, &ok(&json!({})), now);
    drop(effects(&mut calls));
    let rejection = latest_callback(&calls, "rejection");
    calls.on_delivery(&delivery(&rejection, &in_progress()), Observed::Nothing, now);
    let gave_way = events(&effects(&mut calls));
    assert!(
        matches!(
            gave_way[..],
            [CallEvent::RenegotiationGaveWay { call: a }, CallEvent::MediaOffer { call: b, .. }] if a == call && b == call
        ),
        "{gave_way:#?}"
    );
    calls.tick(now + RENEGOTIATION_RETRY);
    assert!(sent(&effects(&mut calls)).is_empty(), "the account's doesn't go again");
    assert!(matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)));
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
    // once the far end's is answered, the account's media offers again
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"]);
}

#[test]
fn no_renegotiation_of_the_account_s_begins_while_one_turned_away_waits_to_go_again() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    drop(effects(&mut calls));
    let rejection = latest_callback(&calls, "rejection");
    calls.on_delivery(&delivery(&rejection, &in_progress()), Observed::Nothing, now);
    assert!(changes(&mut calls).is_empty());
    // the camera turned off and on again before the offer turned away goes again: refused, the media untouched
    assert!(matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)));
    assert!(matches!(calls.renegotiate(call, &media()), Err(CallError::NotNow)));
    calls.tick(now + RENEGOTIATION_RETRY);
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"]);
    assert!(matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)));
    let at = latest_callback(&calls, "mediaAnswer");
    calls.on_delivery(&delivery(&at, &media_answer("v=1\r\n")), Observed::Renegotiation, now);
    drop(effects(&mut calls));
    calls.begin_renegotiation(call).unwrap();
}

#[test]
fn the_account_s_renegotiation_waits_while_the_far_end_s_waits_for_its_answer() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let _ = effects(&mut calls);
    assert!(matches!(calls.begin_renegotiation(call), Err(CallError::NotNow)));
    assert!(matches!(calls.renegotiate(call, &media()), Err(CallError::NotNow)));
    // the far end's is answered: the call goes on, and the account's may go now
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
    calls.begin_renegotiation(call).unwrap();
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"]);
}

#[test]
fn a_rejected_renegotiation_or_one_whose_answer_doesn_t_read_fails_the_call() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    let _ = effects(&mut calls);
    let rejection = latest_callback(&calls, "rejection");
    calls.on_delivery(&delivery(&rejection, &json!({})), Observed::Nothing, now);
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("renegotiate")
    }));

    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    let _ = effects(&mut calls);
    let at = latest_callback(&calls, "mediaAnswer");
    calls.on_delivery(
        &delivery(&at, &json!({"mediaAnswer": {}})),
        Observed::Renegotiation,
        now,
    );
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("renegotiate")
    }));
}

/// The call controller's rejection of a renegotiation while another negotiation of the call is in progress, as
/// captured (`ab-call --call video`, both pages' cameras turned on together).
fn in_progress() -> Value {
    json!({"mediaNegotiationFailure": {
        "reason": "clientError", "sender": null, "code": 491, "subCode": 10029,
        "phrase": "Negotiation is in progress.", "resultCategories": ["ExpectedError"],
    }, "debugContent": null})
}

#[test]
fn a_renegotiation_turned_away_while_another_is_in_progress_goes_again_with_its_offer() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    let asked = effects(&mut calls);
    let [(first, "renegotiate")] = sent(&asked)[..] else {
        panic!("{asked:#?}");
    };
    calls.on_outcome(first, &ok(&json!({})), now);
    let rejection = latest_callback(&calls, "rejection");
    calls.on_delivery(&delivery(&rejection, &in_progress()), Observed::Nothing, now);
    assert!(changes(&mut calls).is_empty(), "the call goes on");
    assert_eq!(calls.deadline(), Some(now + RENEGOTIATION_RETRY));
    calls.tick(now + RENEGOTIATION_RETRY / 2);
    assert!(changes(&mut calls).is_empty(), "not yet");
    calls.tick(now + RENEGOTIATION_RETRY);
    let again = effects(&mut calls);
    let [(second, "renegotiate")] = sent(&again)[..] else {
        panic!("{again:#?}");
    };
    calls.on_outcome(second, &ok(&json!({})), now);
    // its answer comes at the new renegotiation's own callback
    let at = latest_callback(&calls, "mediaAnswer");
    calls.on_delivery(&delivery(&at, &media_answer("v=1\r\n")), Observed::Renegotiation, now);
    assert!(
        events(&effects(&mut calls))
            .iter()
            .any(|event| matches!(event, CallEvent::RemoteMedia { .. }))
    );

    // the far end's renegotiation while the account's waits to go again: the far end's goes first, the account's
    // gives way
    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    drop(effects(&mut calls));
    let rejection = latest_callback(&calls, "rejection");
    calls.on_delivery(&delivery(&rejection, &in_progress()), Observed::Nothing, now);
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let told = events(&effects(&mut calls));
    assert!(
        matches!(
            told[..],
            [CallEvent::RenegotiationGaveWay { .. }, CallEvent::MediaOffer { .. }]
        ),
        "{told:#?}"
    );
    calls.tick(now + RENEGOTIATION_RETRY);
    assert!(sent(&effects(&mut calls)).is_empty(), "nothing goes again");

    // turned away too often, the call fails
    let (mut calls, call) = connected_call(now);
    calls.renegotiate(call, &media()).unwrap();
    drop(effects(&mut calls));
    let mut at = now;
    for _ in 0..5 {
        let rejection = latest_callback(&calls, "rejection");
        calls.on_delivery(&delivery(&rejection, &in_progress()), Observed::Nothing, at);
        at += RENEGOTIATION_RETRY;
        calls.tick(at);
        drop(effects(&mut calls));
    }
    let rejection = latest_callback(&calls, "rejection");
    calls.on_delivery(&delivery(&rejection, &in_progress()), Observed::Nothing, at);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::Ended {
        call,
        end: End::Failed("renegotiate")
    }));
}

/// A renegotiation of the call delivered to the account, its answer going to `Call1`'s negotiation link.
fn negotiation() -> Value {
    json!({"mediaNegotiation": {
        "mediaContent": {"blob": "v=2\r\n", "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAnswer": cc("negotiations", "answer"), "rejection": cc("negotiations", "reject")},
    }})
}

#[test]
fn a_renegotiation_and_its_answer_get_their_receipts_and_a_delivery_the_tracker_bound_nothing_of_none() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    // the far end's renegotiation: its receipt at once, beside its telling
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let offered = effects(&mut calls);
    assert_eq!(receipts(&offered), [DeliveryKey::of(&delivery(&at, &negotiation()))]);
    assert_eq!(events(&offered).len(), 1);
    calls.answer_media(call, media()).unwrap();
    drop(effects(&mut calls));
    // the answer to the account's own renegotiation: its receipt too
    let (answer_at, _) = renegotiated(&mut calls, call, now);
    let answer = media_answer("v=1\r\n");
    calls.on_delivery(&delivery(&answer_at, &answer), Observed::Renegotiation, now);
    assert_eq!(receipts(&effects(&mut calls)).len(), 1);
    // a delivery the tracker bound nothing of gets none
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Nothing, now);
    assert!(receipts(&effects(&mut calls)).is_empty());
}

#[test]
fn a_call_whose_media_carries_video_names_the_video_modality_until_it_doesn_t() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let modalities = |calls: &Calls| calls.call.as_ref().unwrap().modalities.clone();
    assert_eq!(modalities(&calls), ["Audio"]);
    calls.set_video(call, true);
    calls.set_video(call, true);
    assert_eq!(modalities(&calls), ["Audio", "Video"], "once, after the call's own");
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"]);
    calls.set_video(call, false);
    assert_eq!(modalities(&calls), ["Audio"]);
    let other = CallId(call.get() + 1);
    calls.set_video(other, true);
    assert_eq!(modalities(&calls), ["Audio"], "another call's changes nothing");
}

#[test]
fn a_renegotiation_delivered_is_told_and_answered_only_by_the_account() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    assert!(matches!(calls.answer_media(call, media()), Err(CallError::NotNow)));
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    let offered = effects(&mut calls);
    assert!(sent(&offered).is_empty(), "nothing goes until the account answers");
    let told = events(&offered);
    let [CallEvent::MediaOffer { call: of, offer }] = told.as_slice() else {
        panic!("{offered:#?}");
    };
    assert_eq!((*of, offer.blob.as_str()), (call, "v=2\r\n"));
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
    assert!(
        matches!(calls.answer_media(call, media()), Err(CallError::NotNow)),
        "answered once"
    );

    // one the tracker bound nothing of (no link its answer may go to) fails the call
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Nothing, now);
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert!(events(&failed).contains(&CallEvent::Ended {
        call,
        end: End::Failed("media_answer")
    }));
}

/// The path of the call's callback `name` made last: the one the request just built handed out.
fn latest_callback(calls: &Calls, name: &str) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.latest_path_of(Kind::Call, name))
        .unwrap()
        .to_owned()
}

/// The answer to the account's renegotiation, `blob` its SDP.
fn media_answer(blob: &str) -> Value {
    json!({"mediaAnswer": {
        "mediaContent": {"blob": blob, "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAcknowledgement": cc("mediaAnswers", "acknowledge")},
    }})
}

/// The account renegotiates `call` and its renegotiation is taken: the paths of the `mediaAnswer` and `rejection`
/// callbacks this renegotiation's request handed out.
fn renegotiated(calls: &mut Calls, call: CallId, now: Instant) -> (String, String) {
    calls.renegotiate(call, &media()).unwrap();
    let asked = effects(calls);
    let [(renegotiation, "renegotiate")] = sent(&asked)[..] else {
        panic!("{asked:#?}");
    };
    let paths = (
        latest_callback(calls, "mediaAnswer"),
        latest_callback(calls, "rejection"),
    );
    calls.on_outcome(renegotiation, &ok(&json!({})), now);
    assert!(changes(calls).is_empty());
    paths
}

#[test]
fn an_answer_or_a_rejection_counts_only_at_the_waiting_renegotiation_s_own_callbacks() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let (first_answer, first_rejection) = renegotiated(&mut calls, call, now);
    calls.on_delivery(
        &delivery(&first_answer, &media_answer("v=1\r\n")),
        Observed::Renegotiation,
        now,
    );
    assert_eq!(names(&effects(&mut calls)), ["media_acknowledge"]);

    let (second_answer, second_rejection) = renegotiated(&mut calls, call, now);
    assert_ne!(
        (&first_answer, &first_rejection),
        (&second_answer, &second_rejection),
        "each renegotiation hands out callbacks of its own"
    );
    // the first one's answer again, and its rejection: the second still waits, nothing is told, sent or failed
    calls.on_delivery(
        &delivery(&first_answer, &media_answer("v=1\r\n")),
        Observed::Renegotiation,
        now,
    );
    assert!(changes(&mut calls).is_empty(), "an earlier renegotiation's answer");
    calls.on_delivery(&delivery(&first_rejection, &json!({})), Observed::Nothing, now);
    assert!(changes(&mut calls).is_empty(), "an earlier renegotiation's rejection");
    assert_eq!(calls.current(), Some((call, Some(CallState::Connected))));
    assert!(
        matches!(calls.renegotiate(call, &media()), Err(CallError::NotNow)),
        "the second still waits for its answer"
    );

    // the second's own answer: told and acknowledged, once
    calls.on_delivery(
        &delivery(&second_answer, &media_answer("v=3\r\n")),
        Observed::Renegotiation,
        now,
    );
    let answered = effects(&mut calls);
    assert_eq!(names(&answered), ["media_acknowledge"]);
    let told = events(&answered);
    let [CallEvent::RemoteMedia { call: of, media: told }] = told.as_slice() else {
        panic!("{answered:#?}");
    };
    assert_eq!((*of, told.blob.as_str()), (call, "v=3\r\n"));
    calls.on_delivery(
        &delivery(&second_answer, &media_answer("v=3\r\n")),
        Observed::Renegotiation,
        now,
    );
    assert!(changes(&mut calls).is_empty(), "its answer again");
    calls.on_delivery(&delivery(&second_rejection, &json!({})), Observed::Nothing, now);
    assert!(changes(&mut calls).is_empty(), "its rejection once it was answered");
    calls.renegotiate(call, &media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["renegotiate"], "and another may go");
}

/// A renegotiation of the call delivered to the account: the offer `blob`, its answer going to the link `answer`.
fn negotiation_of(blob: &str, answer: &str) -> Value {
    json!({"mediaNegotiation": {
        "mediaContent": {"blob": blob, "contentType": "application/sdp-ngc-1.0"},
        "links": {"mediaAnswer": answer, "rejection": cc("negotiations", "reject")},
    }})
}

/// The offers `effects` tell, by their SDP.
fn offers(effects: &[Effect]) -> Vec<String> {
    events(effects)
        .into_iter()
        .filter_map(|event| match event {
            CallEvent::MediaOffer { offer, .. } => Some(offer.blob),
            _ => None,
        })
        .collect()
}

#[test]
fn a_renegotiation_delivered_again_as_it_was_is_a_replay_and_changes_nothing() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let at = latest_callback(&calls, "mediaRenegotiation");
    let first = negotiation_of("v=2\r\n", &cc("negotiations", "answer"));
    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert_eq!(offers(&effects(&mut calls)), ["v=2\r\n"]);

    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert!(changes(&mut calls).is_empty(), "again while it waits for the answer");
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"], "still answered once");
    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert!(changes(&mut calls).is_empty(), "again once it was answered");
    assert!(
        matches!(calls.answer_media(call, media()), Err(CallError::NotNow)),
        "nothing waits for an answer"
    );

    let other = format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/negotiations/Call1/1/x1/2/answer?i=10-0-0-1");
    let second = negotiation_of("v=4\r\n", &other);
    calls.on_delivery(&delivery(&at, &second), Observed::Renegotiation, now);
    assert_eq!(offers(&effects(&mut calls)), ["v=4\r\n"], "a new one is told");
    calls.on_delivery(&delivery(&at, &first), Observed::Renegotiation, now);
    assert!(
        changes(&mut calls).is_empty(),
        "the answered one again while another waits"
    );
    calls.on_delivery(&delivery(&at, &second), Observed::Renegotiation, now);
    assert!(changes(&mut calls).is_empty(), "the waiting one again");
    assert_eq!(calls.current(), Some((call, Some(CallState::Connected))));
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);

    // a different offer on a link seen before is a new renegotiation
    let third = negotiation_of("v=5\r\n", &cc("negotiations", "answer"));
    calls.on_delivery(&delivery(&at, &third), Observed::Renegotiation, now);
    assert_eq!(offers(&effects(&mut calls)), ["v=5\r\n"]);
    calls.answer_media(call, media()).unwrap();
    assert_eq!(names(&effects(&mut calls)), ["media_answer"]);
}

#[test]
fn media_the_far_end_closed_ends_the_call_as_the_far_end_s_with_a_leave_once() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.media_closed(CallId(99), now);
    assert!(changes(&mut calls).is_empty(), "another call's");
    calls.media_closed(call, now);
    let ended = effects(&mut calls);
    assert_eq!(names(&ended), ["leave"]);
    assert_eq!(
        events(&ended).last(),
        Some(&CallEvent::Ended {
            call,
            end: End::Remote(crate::RemoteEnd::default())
        })
    );
    calls.media_closed(call, now);
    assert!(changes(&mut calls).is_empty(), "it ended already");

    // a call not up yet fails as its media failing does
    let (mut calls, call, _start) = placing_call(now);
    calls.media_closed(call, now);
    let _ = effects(&mut calls);
    assert!(calls.current().is_some(), "its start's answer is awaited first");
}

#[test]
fn a_media_failure_fails_the_call_with_a_leave_once() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.fail_media(CallId(99), now);
    assert!(changes(&mut calls).is_empty(), "another call's");
    calls.fail_media(call, now);
    let failed = effects(&mut calls);
    assert_eq!(names(&failed), ["leave"]);
    assert_eq!(
        events(&failed).last(),
        Some(&CallEvent::Ended {
            call,
            end: End::Failed("media")
        })
    );
    calls.fail_media(call, now);
    assert!(changes(&mut calls).is_empty(), "it ended already");

    // an invitation that rings here goes away unanswered
    let (mut calls, call) = offered_call(now);
    calls.fail_media(call, now);
    let gone = effects(&mut calls);
    assert!(sent(&gone).is_empty());
    assert!(matches!(&events(&gone)[..], [CallEvent::RingEnded { call: of, .. }] if *of == call));
}

#[test]
fn beside_a_write_in_flight_the_engine_takes_everything_but_the_next_write() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    // the start, then its state: the state comes out first, the start stays queued
    assert!(matches!(
        calls.poll_beside_write(),
        Some(Effect::Event(CallEvent::State {
            state: CallState::Placing,
            ..
        }))
    ));
    assert!(calls.poll_beside_write().is_none());
    let Some(Effect::Send { id: start, .. }) = calls.poll() else {
        panic!("the start");
    };
    // a hang-up before the start's answer: the start isn't sent again, the cancel that follows may be
    calls.hang_up(call, now).unwrap();
    let _ = effects(&mut calls);
    assert!(!calls.may_send_again(start), "the call leaves");
    calls.on_outcome(start, &ok(&conversation_answer()), now);
    let [(leave, "leave")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    assert!(calls.may_send_again(leave));
    assert!(!calls.may_send_again(WriteId(99)), "not one of the machine's");
}

#[test]
fn a_cleanup_write_of_a_call_that_ended_may_go_again_until_its_outcome_came() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let metadata = calls
        .call
        .as_ref()
        .and_then(|current| current.writes.keys().next().copied())
        .unwrap();
    assert!(calls.may_send_again(metadata), "the call wants it");
    calls.fail_media(call, now);
    let [(leave, "leave")] = sent(&effects(&mut calls))[..] else {
        panic!()
    };
    assert_eq!(calls.current(), None, "the call ended here at once");
    assert!(!calls.may_send_again(metadata));
    assert!(calls.may_send_again(leave), "its leave is still the engine's to finish");
    calls.on_outcome(leave, &ok(&json!({})), now);
    assert!(!calls.may_send_again(leave));
}

#[test]
fn a_shut_down_lets_an_invitation_go_unanswered_and_hangs_up_a_call() {
    let now = Instant::now();
    let (mut calls, call) = offered_call(now);
    calls.shut_down(now);
    let gone = effects(&mut calls);
    assert!(sent(&gone).is_empty(), "no decline: nothing is sent for it");
    assert!(matches!(&events(&gone)[..], [CallEvent::RingEnded { call: of, .. }] if *of == call));
    assert_eq!(calls.current(), None);

    let (mut calls, _) = connected_call(now);
    calls.shut_down(now);
    assert_eq!(names(&effects(&mut calls)), ["leave"]);
    calls.shut_down(now);
    assert!(effects(&mut calls).is_empty(), "leaving already");
}

#[test]
fn an_acceptance_by_the_callee_s_voicemail_is_told_once_the_call_is_up() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    let mut voicemail = acceptance();
    voicemail["callAcceptance"]["acceptedBy"] = json!({"id": B, "endpointType": "voicemail"});
    calls.on_delivery(&delivery(&path, &voicemail), Observed::Acceptance, now);
    let told = events(&effects(&mut calls));
    let connected = CallEvent::State {
        call,
        state: CallState::Connected,
    };
    let up = told.iter().position(|event| *event == connected).expect("up");
    let forwarded = told
        .iter()
        .position(|event| *event == CallEvent::Voicemail { call })
        .expect("told");
    assert!(up < forwarded, "{told:#?}");

    // the callee's own endpoint taking the call is no voicemail
    let (mut calls, call) = ringing_call(now);
    let path = acceptance_path(&calls);
    let mut answered = acceptance();
    answered["callAcceptance"]["acceptedBy"] = json!({"id": B});
    calls.on_delivery(&delivery(&path, &answered), Observed::Acceptance, now);
    assert!(!events(&effects(&mut calls)).contains(&CallEvent::Voicemail { call }));
}

/// A link of the call `Call2` the call controller moved a call to, `rest` after its call leg.
fn retargeted_link(rest: &str) -> String {
    format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/active/Call2/1/a4/5{rest}?i=10-0-0-1&e=1")
}

/// The call's own renegotiation link and its keep-alive's call leg, as text.
fn own_links(calls: &Calls) -> (Option<String>, Option<String>) {
    let current = calls.call.as_ref().unwrap();
    (
        current.renegotiation.as_ref().map(|link| link.as_str().to_owned()),
        current
            .keep_alive
            .as_ref()
            .and_then(|keep| keep.link.as_ref())
            .map(|link| link.as_str().to_owned()),
    )
}

#[test]
fn a_retarget_acknowledged_moves_the_call_s_own_links_to_the_new_call() {
    let now = Instant::now();
    let (mut calls, call) = up_with_a_call_leg(now);
    let at = callback(&calls, "mediaRenegotiation");
    calls.on_delivery(&delivery(&at, &negotiation()), Observed::Renegotiation, now);
    drop(effects(&mut calls));
    calls.answer_media(call, media()).unwrap();
    drop(effects(&mut calls));
    let acknowledged = latest_callback(&calls, "mediaAcknowledgement");
    let retarget = json!({
        "mediaAcknowledgement": {"code": 0, "subCode": 10109, "phrase": "Participant retarget was successful.",
            "resultCategories": ["Success"]},
        "links": {"callLeg": retargeted_link(""), "mediaRenegotiation": retargeted_link("/renegotiate")},
    });
    let before = own_links(&calls);
    // one the tracker didn't read moves nothing
    calls.on_delivery(&delivery(&acknowledged, &retarget), Observed::Nothing, now);
    assert_eq!(own_links(&calls), before);
    // nor does a plain acknowledgement, naming no call leg
    let plain =
        json!({"mediaAcknowledgement": {"code": 0, "resultCategories": ["Success"]}, "links": {"callLeg": null}});
    calls.on_delivery(&delivery(&acknowledged, &plain), Observed::Renegotiation, now);
    assert_eq!(own_links(&calls), before);
    drop(effects(&mut calls));
    // as captured, its `callLeg` named twice: `null`, then the new call leg
    let mut twice = delivery(&acknowledged, &retarget);
    twice.body = String::from_utf8(twice.body)
        .unwrap()
        .replacen(r#""links":{"#, r#""links":{"callLeg":null,"#, 1)
        .into_bytes();
    calls.on_delivery(&twice, Observed::Renegotiation, now);
    let moved = effects(&mut calls);
    assert_eq!(receipts(&moved).len(), 1, "its receipt, as any negotiation's");
    assert!(events(&moved).is_empty(), "the call goes on as it was");
    assert_eq!(
        own_links(&calls),
        (Some(retargeted_link("/renegotiate")), Some(retargeted_link("")))
    );
    assert_eq!(calls.current().map(|(id, _)| id), Some(call));
}

#[test]
fn caption_invite_failure_does_not_end_voice_and_only_own_roster_binds_service() {
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    calls.call.as_mut().unwrap().conversation.caption_add = teams_proto::CallLink::parse(&conv("/addParticipant"));
    assert!(calls.caption_invite(CallId(call.get() + 1)).is_err());
    let write = calls.caption_invite(call).unwrap();
    assert!(!format!("{write:?}").contains("Lab"));
    let failed = calls
        .call
        .as_ref()
        .unwrap()
        .latest_path_of(Kind::Conversation, "addParticipantFailure")
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&failed, &json!({"code":403})), Observed::Nothing, now);
    let failed = effects(&mut calls);
    assert!(events(&failed).contains(&CallEvent::CaptionFailed { call }));
    assert_eq!(calls.current(), Some((call, Some(CallState::Connected))));
    assert!(sent(&failed).is_empty());
    let command = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    let body = json!({"participants":{teams_proto::captions::CAPTION_SERVICE_MRI:{"state":"active","version":1,"endpoints":{"synthetic":{"endpointMetadata":{"commandUrl":command,"spokenLanguage":"en-us","processingModes":{"closedCaptions":{"state":"Active"}}}}}}}});
    calls.on_delivery(&delivery("/unknown/rosterUpdate/", &body), Observed::Nothing, now);
    assert!(calls.caption_service(call).is_none());
    let roster = calls
        .call
        .as_ref()
        .unwrap()
        .path_of(Kind::Conversation, "rosterUpdate")
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&roster, &body), Observed::Nothing, now);
    assert_eq!(calls.caption_service(call).unwrap().state, "Active");
    assert!(calls.caption_command(call, None).is_ok());
    assert!(calls.caption_command(call, Some("en-gb")).is_ok());
    assert!(calls.caption_command(call, Some("invented-language")).is_err());
    assert!(
        calls
            .call
            .as_ref()
            .unwrap()
            .roster
            .participants()
            .iter()
            .all(|person| person.mri.as_str() != teams_proto::captions::CAPTION_SERVICE_MRI)
    );
    // Unchanged metadata fields can be omitted in the next roster delta.
    let delta = json!({"participants":{teams_proto::captions::CAPTION_SERVICE_MRI:{"state":"active","version":2,"endpoints":{"synthetic":{"endpointMetadata":{"spokenLanguage":"en-gb"}}}}}});
    calls.on_delivery(&delivery(&roster, &delta), Observed::Nothing, now);
    assert_eq!(calls.caption_service(call).unwrap().spoken_language, "en-gb");
    let stale = json!({"participants":{teams_proto::captions::CAPTION_SERVICE_MRI:{"state":"inactive","version":1}}});
    calls.on_delivery(&delivery(&roster, &stale), Observed::Nothing, now);
    assert_eq!(calls.caption_service(call).unwrap().spoken_language, "en-gb");
    calls.hang_up(call, now).unwrap();
    assert!(calls.caption_command(call, None).is_err());
}

#[test]
fn a_recording_starts_alone_beside_a_running_transcription_and_its_stop_stops_both() {
    use teams_proto::captions::{RecordCommand, RecordStart};
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let start = RecordStart {
        title: "Call with Lab B".into(),
        organizer: None,
        language: "en-us".into(),
        stamp: "20261106_120144".into(),
        audio_only: true,
    };
    assert!(
        calls.record_command(call, &RecordCommand::StopRecording).is_err(),
        "no bot in the call yet"
    );
    let roster = calls
        .call
        .as_ref()
        .unwrap()
        .path_of(Kind::Conversation, "rosterUpdate")
        .unwrap()
        .to_owned();
    let command = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    let bot = |version: u64, transcript: &str| {
        json!({"participants":{teams_proto::captions::CAPTION_SERVICE_MRI:{"state":"active","version":version,
            "endpoints":{"synthetic":{"endpointMetadata":{"commandUrl":command,"spokenLanguage":"en-us",
            "processingModes":{"closedCaptions":{"state":"Inactive"},
            "realTimeTranscript":{"state":transcript,"userId":"8:orgid:synthetic"}}}}}}}})
    };
    let sent = |calls: &Calls, command: &RecordCommand| {
        let write = calls.record_command(call, command).unwrap();
        let body = write.bare_body();
        (
            body.get("mode").and_then(Value::as_str).map(str::to_owned),
            body["processingModes"].clone(),
        )
    };
    let both = json!(["recording", "realTimeTranscript"]);
    calls.on_delivery(&delivery(&roster, &bot(1, "Inactive")), Observed::Nothing, now);
    assert_eq!(
        sent(&calls, &RecordCommand::Record(start.clone())),
        (None, both.clone())
    );
    assert_eq!(
        sent(&calls, &RecordCommand::StopRecording),
        (Some("recording".into()), json!(["recording"]))
    );
    // the transcription runs: a recording starts alone, and its stop stops both
    calls.on_delivery(&delivery(&roster, &bot(2, "Active")), Observed::Nothing, now);
    assert_eq!(
        sent(&calls, &RecordCommand::Record(start.clone())),
        (Some("recording".into()), json!(["recording"]))
    );
    assert_eq!(sent(&calls, &RecordCommand::StopRecording), (None, both));
    for command in [RecordCommand::Transcribe(start), RecordCommand::StopTranscription] {
        assert_eq!(
            sent(&calls, &command),
            (Some("transcription".into()), json!(["realTimeTranscript"]))
        );
    }
}

/// Invited as the commercial recorder, the service can join from another region under Teams' alternate MRI: it is
/// still the call's caption and recording service, and never a person in the call.
#[test]
fn the_caption_service_from_another_region_serves_captions_and_recording_and_stays_hidden() {
    use teams_proto::captions::{CAPTION_SERVICE_ALTERNATE_MRI, RecordCommand};
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let roster = calls
        .call
        .as_ref()
        .unwrap()
        .path_of(Kind::Conversation, "rosterUpdate")
        .unwrap()
        .to_owned();
    let command = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    let bot = |version: u64, captions: &str| {
        json!({"participants":{CAPTION_SERVICE_ALTERNATE_MRI:{"state":"active","version":version,
            "endpoints":{"synthetic":{"endpointMetadata":{"commandUrl":command,"spokenLanguage":"en-us",
            "processingModes":{"closedCaptions":{"state":captions},
            "realTimeTranscript":{"state":"Active","userId":"8:orgid:synthetic"}}}}}}}})
    };
    // Participant versions belong to each MRI: an earlier invited leg must not suppress the alternate leg.
    let invited = json!({"participants": {teams_proto::captions::CAPTION_SERVICE_MRI: {
        "version": 20, "state": "disconnected"
    }}});
    calls.on_delivery(&delivery(&roster, &invited), Observed::Nothing, now);
    let _ = effects(&mut calls);
    calls.on_delivery(&delivery(&roster, &bot(1, "Inactive")), Observed::Nothing, now);
    let told = events(&effects(&mut calls));
    assert!(told.iter().any(|event| matches!(
        event,
        CallEvent::CaptionService { call: of, service: Some(service) }
            if *of == call && service.mri == CAPTION_SERVICE_ALTERNATE_MRI && service.state == "Inactive"
    )));
    assert!(calls.caption_command(call, None).is_ok());
    assert!(calls.record_command(call, &RecordCommand::StopTranscription).is_ok());
    assert!(calls.call.as_ref().unwrap().meeting.state.transcribing.is_some());
    calls.on_delivery(&delivery(&roster, &bot(2, "Active")), Observed::Nothing, now);
    assert_eq!(calls.caption_service(call).unwrap().state, "Active");
    let stale = json!({"participants": {teams_proto::captions::CAPTION_SERVICE_MRI: {
        "version": 19, "state": "disconnected"
    }}});
    calls.on_delivery(&delivery(&roster, &stale), Observed::Nothing, now);
    calls.on_delivery(&delivery(&roster, &bot(1, "Inactive")), Observed::Nothing, now);
    assert_eq!(calls.caption_service(call).unwrap().state, "Active");
    assert!(calls.call.as_ref().unwrap().meeting.state.transcribing.is_some());
    assert!(
        calls
            .call
            .as_ref()
            .unwrap()
            .roster
            .participants()
            .iter()
            .all(|person| !teams_proto::captions::is_caption_service(person.mri.as_str()))
    );
}

/// An answer's roster announces the service immediately; later updates report only changed projections.
#[test]
fn an_answer_roster_reports_caption_and_recording_state_without_a_followup_update() {
    use teams_proto::captions::CAPTION_SERVICE_ALTERNATE_MRI;
    let now = Instant::now();
    let command = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    let initial = json!({"participants": {CAPTION_SERVICE_ALTERNATE_MRI: {
        "version": 1, "state": "active", "endpoints": {"synthetic": {"endpointMetadata": {
            "commandUrl": command, "spokenLanguage": "en-us", "processingModes": {
                "closedCaptions": {"state": "Active"}, "recording": {"state": "Active"},
                "realTimeTranscript": {"state": "Active"}
            }
        }}}
    }}});
    let (mut calls, call, told) = meeting_answered(initial.clone(), now);
    let service = calls.caption_service(call).unwrap();
    assert_eq!(service.mri, CAPTION_SERVICE_ALTERNATE_MRI);
    assert_eq!(service.command.as_str(), command);
    assert_eq!(
        told.iter()
            .filter(|event| matches!(event,
                CallEvent::CaptionService { call: of, service: Some(told) } if *of == call && told == service
            ))
            .count(),
        1
    );
    assert!(told.iter().any(|event| matches!(event,
        CallEvent::Meeting { call: of, state }
            if *of == call && state.recording.is_some() && state.transcribing.is_some()
    )));
    let path = calls
        .call
        .as_ref()
        .unwrap()
        .path_of(Kind::Conversation, "rosterUpdate")
        .unwrap()
        .to_owned();
    calls.on_delivery(&delivery(&path, &initial), Observed::Nothing, now);
    let repeated = events(&effects(&mut calls));
    assert!(
        !repeated
            .iter()
            .any(|event| matches!(event, CallEvent::CaptionService { .. } | CallEvent::Meeting { .. }))
    );
    // Recording can stop without changing the caption binding: it still needs its own meeting event.
    let stopped = json!({"participants": {CAPTION_SERVICE_ALTERNATE_MRI: {
        "version": 2, "endpoints": {"synthetic": {"endpointMetadata": {
            "processingModes": {"closedCaptions": {"state": "Active"}}
        }}}
    }}});
    calls.on_delivery(&delivery(&path, &stopped), Observed::Nothing, now);
    let told = events(&effects(&mut calls));
    assert!(
        !told
            .iter()
            .any(|event| matches!(event, CallEvent::CaptionService { .. }))
    );
    assert!(told.iter().any(|event| matches!(event,
        CallEvent::Meeting { call: of, state }
            if *of == call && state.recording.is_none() && state.transcribing.is_none()
    )));
    let departed = json!({"participants": {CAPTION_SERVICE_ALTERNATE_MRI: {
        "version": 3, "state": "disconnected"
    }}});
    calls.on_delivery(&delivery(&path, &departed), Observed::Nothing, now);
    let told = events(&effects(&mut calls));
    assert!(told.iter().any(|event| matches!(event,
        CallEvent::CaptionService { call: of, service: None } if *of == call
    )));
    assert!(calls.caption_service(call).is_none());
}

/// Both region identities describe one service; unrelated or older entries cannot stop its recording.
#[test]
fn recording_and_captions_follow_the_same_regional_service_and_endpoint() {
    use teams_proto::captions::{CAPTION_SERVICE_ALTERNATE_MRI, CAPTION_SERVICE_MRI};
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let roster = calls
        .call
        .as_ref()
        .unwrap()
        .path_of(Kind::Conversation, "rosterUpdate")
        .unwrap()
        .to_owned();
    let command = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
    let bot = |mri: &str, version: u64, joined: &str, state: &str| {
        json!({"participants": {mri: {"version": version, "state": "active", "endpoints": {
            "synthetic": {"endpointMetadata": {"commandUrl": command, "joinTime": joined,
                "processingModes": {"closedCaptions": {"state": state},
                    "recording": {"state": state}, "realTimeTranscript": {"state": state}}}}
        }}}})
    };
    let newer = bot(CAPTION_SERVICE_ALTERNATE_MRI, 1, "2026-01-01T00:00:02Z", "Active");
    calls.on_delivery(&delivery(&roster, &newer), Observed::Nothing, now);
    let expected = calls.call.as_ref().unwrap().meeting.state.clone();
    assert!(expected.recording.is_some() && expected.transcribing.is_some());
    let service = calls.caption_service(call).unwrap().clone();
    let _ = effects(&mut calls);
    for entry in [
        json!({"version": 21, "state": "disconnected"}),
        json!({"version": 22, "state": "connecting"}),
        json!({"version": 23}),
        json!({"version": 24, "state": "active"}),
    ] {
        let other = json!({"participants": {CAPTION_SERVICE_MRI: entry}});
        calls.on_delivery(&delivery(&roster, &other), Observed::Nothing, now);
        assert_eq!(calls.call.as_ref().unwrap().meeting.state, expected);
        assert_eq!(calls.caption_service(call), Some(&service));
        assert!(
            !events(&effects(&mut calls))
                .iter()
                .any(|event| matches!(event, CallEvent::Meeting { .. }))
        );
    }
    // A full but older active entry must not take either state back, even with the same endpoint key.
    let older = bot(CAPTION_SERVICE_MRI, 25, "2026-01-01T00:00:01Z", "Inactive");
    calls.on_delivery(&delivery(&roster, &older), Observed::Nothing, now);
    assert_eq!(calls.call.as_ref().unwrap().meeting.state, expected);
    assert_eq!(calls.caption_service(call), Some(&service));
    // The same selection applies when both full entries arrive together.
    let mut both = newer;
    both["participants"][CAPTION_SERVICE_ALTERNATE_MRI]["version"] = json!(2);
    both["participants"][CAPTION_SERVICE_MRI] = older["participants"][CAPTION_SERVICE_MRI].clone();
    calls.on_delivery(&delivery(&roster, &both), Observed::Nothing, now);
    assert_eq!(calls.call.as_ref().unwrap().meeting.state, expected);
    assert_eq!(calls.caption_service(call), Some(&service));
    // A newer join hands over all modes; its departure hands back while the other region stays active.
    let newest = bot(CAPTION_SERVICE_MRI, 26, "2026-01-01T00:00:03Z", "Active");
    calls.on_delivery(&delivery(&roster, &newest), Observed::Nothing, now);
    assert_eq!(calls.caption_service(call).unwrap().mri, CAPTION_SERVICE_MRI);
    assert_eq!(calls.call.as_ref().unwrap().meeting.state, expected);
    regional_departures(&mut calls, call, &roster, now, &expected);
}

fn regional_departures(calls: &mut Calls, call: CallId, roster: &str, now: Instant, expected: &crate::MeetingState) {
    use teams_proto::captions::{CAPTION_SERVICE_ALTERNATE_MRI, CAPTION_SERVICE_MRI};
    let departed = json!({"participants": {CAPTION_SERVICE_MRI: {"version": 27, "state": "disconnected"}}});
    calls.on_delivery(&delivery(roster, &departed), Observed::Nothing, now);
    // The alternate endpoint is still active: the derived answer hands back to it.
    assert_eq!(calls.caption_service(call).unwrap().mri, CAPTION_SERVICE_ALTERNATE_MRI);
    assert_eq!(&calls.call.as_ref().unwrap().meeting.state, expected);
    let departed = json!({"participants": {CAPTION_SERVICE_ALTERNATE_MRI: {"version": 3, "state": "disconnected"}}});
    calls.on_delivery(&delivery(roster, &departed), Observed::Nothing, now);
    assert!(calls.caption_service(call).is_none());
    assert!(calls.call.as_ref().unwrap().meeting.state.recording.is_none());
    assert!(calls.call.as_ref().unwrap().meeting.state.transcribing.is_none());
}

#[test]
fn mixer_explicitly_marks_only_the_initial_caption_media_fork_as_a_new_session() {
    let mut content =
        json!({"blob":"v=0\r\n","contentType":"application/sdp-ngc-0.5","newOffer":true,"fromMixer":true});
    assert!(crate::read::media(&content).unwrap().new_mixer_session);
    content["newOffer"] = json!(false);
    assert!(!crate::read::media(&content).unwrap().new_mixer_session);
    content["newOffer"] = json!(true);
    content["fromMixer"] = json!(false);
    assert!(!crate::read::media(&content).unwrap().new_mixer_session);
}

#[test]
fn recording_without_a_caption_link_survives_another_mris_empty_active_entry() {
    use teams_proto::captions::{CAPTION_SERVICE_ALTERNATE_MRI, CAPTION_SERVICE_MRI};
    let now = Instant::now();
    let (mut calls, call) = connected_call(now);
    let roster = calls
        .call
        .as_ref()
        .unwrap()
        .path_of(Kind::Conversation, "rosterUpdate")
        .unwrap()
        .to_owned();
    let recording = json!({"participants": {CAPTION_SERVICE_ALTERNATE_MRI: {
        "version": 1, "state": "active", "endpoints": {"synthetic": {"endpointMetadata": {
            "processingModes": {"recording": {"state": "Active"}, "realTimeTranscript": {"state": "Active"}}
        }}}
    }}});
    calls.on_delivery(&delivery(&roster, &recording), Observed::Nothing, now);
    let expected = calls.call.as_ref().unwrap().meeting.state.clone();
    assert!(expected.recording.is_some() && expected.transcribing.is_some());
    assert!(calls.caption_service(call).is_none());
    let _ = effects(&mut calls);
    for entry in [
        json!({"version": 21, "state": "disconnected"}),
        json!({"version": 22, "state": "connecting"}),
        json!({"version": 23}),
        json!({"version": 24, "state": "active"}),
    ] {
        let other = json!({"participants": {CAPTION_SERVICE_MRI: entry}});
        calls.on_delivery(&delivery(&roster, &other), Observed::Nothing, now);
        assert_eq!(calls.call.as_ref().unwrap().meeting.state, expected);
        assert!(
            !events(&effects(&mut calls))
                .iter()
                .any(|event| matches!(event, CallEvent::Meeting { .. }))
        );
    }
    let departed = json!({"participants": {CAPTION_SERVICE_ALTERNATE_MRI: {"version": 2, "state": "disconnected"}}});
    calls.on_delivery(&delivery(&roster, &departed), Observed::Nothing, now);
    assert!(calls.call.as_ref().unwrap().meeting.state.recording.is_none());
    assert!(calls.call.as_ref().unwrap().meeting.state.transcribing.is_none());
}

#[test]
fn a_group_chat_s_meeting_its_start_named_is_joined_where_it_runs() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    assert!(!calls.joinable(CHAT));
    let link = teams_proto::CallLink::parse(&conv("")).unwrap();
    calls.posted_running(CHAT, link.clone());
    assert!(calls.joinable(CHAT));
    calls.join(CHAT, media(), now).unwrap();
    let [(_, "join")] = sent(&effects(&mut calls))[..] else {
        panic!("one join")
    };

    // only a group chat's: a meeting's chat and a 1:1 chat are joined otherwise
    let mut calls = Calls::new(identity(B));
    calls.posted_running("19:meeting_SyntheticMeeting@thread.v2", link.clone());
    calls.posted_running("19:a_b@unq.gbl.spaces", link);
    assert!(!calls.joinable("19:meeting_SyntheticMeeting@thread.v2"));
    assert!(!calls.joinable("19:a_b@unq.gbl.spaces"));
}
