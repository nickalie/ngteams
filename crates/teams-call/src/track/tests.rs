//! Kept call-registry cases moved from teams-fence: offers, ended rings and callback bindings.
use super::*;
use base64::Engine as _;
use serde_json::{Value, json};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const CHAT_DELIVERIES: &str = include_str!("../../../../tests/fixtures/calling/chat/deliveries.json");
const CHAT_FOLLOW_UPS: &str = include_str!("../../../../tests/fixtures/calling/chat/follow_ups.json");
use teams_proto::{CALL_START_URL, TEST_CALL_SCENARIO};
const LAB_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000e0";
const COWORKER: &str = "8:orgid:00000000-0000-4000-8000-0000000000c0";

const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
const CONV_HOST: &str = "conv-zz-01-prod-aks.conv.skype.com";
const CC_HOST: &str = "cc-zz-01-prod-aks.cc.skype.com";
const ROUTE: &str = "i=10-0-0-1&e=1";
const CALLBACKS: &str =
    "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/callAgent/agent-1";
const ACCEPTANCE_PATH: &str = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/03030303/call/acceptance/";
const END_PATH: &str = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/04040404/call/end/";
const CONVERSATION_END_PATH: &str =
    "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/05050505/conversation/conversationEnd/";
const CONNECTION: &str = "session-1/socket-1";
const TROUTER_REQUEST: &str = r#"{"id":"request-1","port":3443,"src":"198.51.100.7"}"#;
/// The acceptance delivery's chain and message ids, which its response echoes.
const CHAIN_ID: &str = "00000000-0000-4000-8000-00000000c4a1";
const MESSAGE_ID: &str = "00000000-0000-4000-8000-00000000e55a";

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GROUP: &str = "0123456789abcdef0123456789abcdef";
fn start_body() -> Vec<u8> {
    json!({
        "conversationRequest": {
            "conversationType": null,
            "suppressDialout": true,
            "links": {
                "conversationEnd": format!("{CALLBACKS}/05050505/conversation/conversationEnd/"),
                "conversationUpdate": format!("{CALLBACKS}/06060606/conversation/conversationUpdate/"),
            },
            "scenario": TEST_CALL_SCENARIO,
        },
        "participants": {"from": {"id": LAB_A, "displayName": "Lab A"}, "to": []},
        "groupChat": null,
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": {
                "acceptance": format!("{CALLBACKS}/03030303/call/acceptance/"),
                "end": format!("{CALLBACKS}/04040404/call/end/"),
            },
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
        },
    })
    .to_string()
    .into_bytes()
}
fn conversation(id: &str) -> String {
    format!("{PROXY}/{CONV_HOST}/conv/{id}")
}
fn answer(id: &str) -> Vec<u8> {
    let at = |op: &str| format!("{}/{op}?{ROUTE}", conversation(id));
    json!({
        "conversationController": format!("{}?{ROUTE}", conversation(id)),
        "links": {
            "leave": at("leave"),
            "addParticipant": at("addParticipant"),
            "addParticipantAndModality": at("add"),
            "updateEndpointMetadata": at("updateEndpointMetadata"),
            "removeParticipant": at("removeParticipant"),
        },
    })
    .to_string()
    .into_bytes()
}
fn acknowledge_url(call: &str) -> String {
    format!("{PROXY}/{CC_HOST}/cc/v1/callAcceptance/{call}/1/a1/1/acknowledge?i=10-0-0-2")
}
fn channel_url(call: &str) -> String {
    format!("{PROXY}/{CC_HOST}/cc/v1/mcProxy/{call}/1/k1/1/applyChannelParameters?i=10-0-0-2&e=1")
}
fn acceptance(call: &str) -> Vec<u8> {
    json!({
        "callAcceptance": {
            "acceptedCallModalities": [],
            "links": {
                "acknowledgement": acknowledge_url(call),
                "callLeg": format!("{PROXY}/{CC_HOST}/cc/v1/active/{call}/1/a1/1?i=10-0-0-2&e=1"),
                "applyChannelParameters": channel_url(call),
            },
            "callKeepAliveInterval": 2700,
        }
    })
    .to_string()
    .into_bytes()
}
fn delivery(id: i64, path: &str, body: Vec<u8>) -> Delivery {
    Delivery {
        connection: CONNECTION.to_owned(),
        id,
        path: path.to_owned(),
        trouter_request: Some(TROUTER_REQUEST.to_owned()),
        chain_id: Some(CHAIN_ID.to_owned()),
        message_id: Some(MESSAGE_ID.to_owned()),
        body,
    }
}
fn fill(text: &str) -> String {
    let mut filled = text
        .replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<group>", GROUP);
    for (n, placeholder) in ["tenant", "leg", "call", "aa", "bb", "3", "4", "agentb"]
        .iter()
        .enumerate()
    {
        filled = filled.replace(
            &format!("<guid:{placeholder}>"),
            &format!("00000000-0000-4000-8000-0000000003{n:02}"),
        );
    }
    filled
}
fn invitation(index: usize, id: i64) -> TestResult<(Delivery, Value)> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_DELIVERIES))?;
    let case = &fixture["invitations"][index];
    let gp = case["body"]["gp"].clone();
    Ok((invitation_delivery(case["path"].as_str().ok_or("path")?, id, &gp), gp))
}
fn invitation_delivery(path: &str, id: i64, gp: &Value) -> Delivery {
    let evt = if gp.get("groupChat").is_some_and(|chat| !chat.is_null()) {
        109
    } else {
        107
    };
    invitation_delivery_as(path, id, evt, gp)
}
fn invitation_delivery_as(path: &str, id: i64, evt: u64, gp: &Value) -> Delivery {
    let body = json!({
        "evt": evt,
        "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string()),
    });
    Delivery {
        connection: CONNECTION.to_owned(),
        id,
        path: path.to_owned(),
        trouter_request: Some(TROUTER_REQUEST.to_owned()),
        chain_id: None,
        message_id: None,
        body: body.to_string().into_bytes(),
    }
}
fn attach_case() -> TestResult<(String, Value)> {
    let fixture: Value = serde_json::from_str(&fill(CHAT_FOLLOW_UPS))?;
    let case = &fixture["cases"][1];
    Ok((case["url"].as_str().ok_or("url")?.to_owned(), case["body"].clone()))
}

struct Write(Url, Value);
impl Request for Write {
    fn service(&self) -> teams_proto::Service {
        teams_proto::Service::Calling
    }
    fn method(&self) -> teams_proto::Method {
        teams_proto::Method::Post
    }
    fn endpoint(&self) -> teams_proto::Endpoint {
        teams_proto::Endpoint::new([] as [&str; 0])
    }
    fn body(&self) -> Option<Value> {
        Some(self.1.clone())
    }
    fn call_url(&self) -> Option<&Url> {
        Some(&self.0)
    }
}
fn write(url: &str, body: Value) -> Write {
    Write(Url::parse(url).unwrap(), body)
}
fn starting(tracker: &Tracker) -> Attempt {
    tracker.before(&write(CALL_START_URL, serde_json::from_slice(&start_body()).unwrap()))
}
fn attached(tracker: &Tracker) -> (Delivery, Value, String) {
    let (invited, gp) = invitation(0, 1).unwrap();
    assert_eq!(tracker.observe_delivery(&invited), Observed::Offered);
    let (url, body) = attach_case().unwrap();
    tracker.before(&write(&url, body.clone()));
    (invited, gp, url)
}
#[test]
fn an_invitation_is_offered_only_to_its_account_and_not_from_itself() {
    let (invited, mut gp) = invitation(0, 1).unwrap();
    assert_eq!(
        Tracker::new(Mri::new(LAB_B)).observe_delivery(&invited),
        Observed::Offered
    );
    assert_eq!(
        Tracker::new(Mri::new(LAB_A)).observe_delivery(&invited),
        Observed::Ignored
    );
    gp["callNotification"]["from"]["id"] = json!(LAB_B);
    let self_call = invitation_delivery(&invited.path, 2, &gp);
    assert_eq!(
        Tracker::new(Mri::new(LAB_B)).observe_delivery(&self_call),
        Observed::Ignored
    );
    gp["callNotification"]["from"]["id"] = json!(COWORKER);
    assert_eq!(
        Tracker::new(Mri::new(LAB_B)).observe_delivery(&invitation_delivery(&invited.path, 3, &gp)),
        Observed::Offered,
        "caller is not a lab policy decision"
    );
}
#[test]
fn unreadable_or_wrong_kind_invitations_stay_ignored() {
    let (invited, gp) = invitation(0, 1).unwrap();
    let tracker = Tracker::new(Mri::new(LAB_B));
    for body in [b"not JSON".to_vec(), br#"{"evt":107,"gp":"not base64"}"#.to_vec()] {
        assert_eq!(
            tracker.observe_delivery(&Delivery {
                body,
                ..invited.clone()
            }),
            Observed::Ignored
        );
    }
    assert_eq!(
        tracker.observe_delivery(&invitation_delivery_as(&invited.path, 2, 109, &gp)),
        Observed::Ignored
    );
    let (group, gp) = invitation(1, 3).unwrap();
    assert_eq!(
        tracker.observe_delivery(&invitation_delivery_as(&group.path, 4, 107, &gp)),
        Observed::Ignored
    );
}
/// `gp` encoded as an invitation's with `text` spliced into its call notification's opening, so a key can be named
/// twice (a JSON value can't hold that).
fn invitation_with_twice(path: &str, gp: &Value, text: &str) -> Delivery {
    let raw = gp.to_string().replacen(
        r#""callNotification":{"#,
        &format!(r#""callNotification":{{{text},"#),
        1,
    );
    let body = json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(raw)});
    Delivery {
        body: body.to_string().into_bytes(),
        ..invitation_delivery(path, 9, gp)
    }
}
#[test]
fn a_transferred_call_s_invitation_naming_its_context_twice_is_offered_and_any_other_key_twice_is_not() {
    let (invited, mut gp) = invitation(0, 1).unwrap();
    // as a transferred call's invitation names it (seen 2026-11-06): `null`, then the service's own context
    gp["callNotification"]["customContext"] = json!({"callDetails": {}});
    let twice = invitation_with_twice(&invited.path, &gp, r#""customContext":null"#);
    assert_eq!(
        Tracker::new(Mri::new(LAB_B)).observe_delivery(&twice),
        Observed::Offered
    );
    // any other key twice stays ambiguous, and the log says which
    let from = invitation_with_twice(&invited.path, &gp, &format!(r#""from":{{"id":"{LAB_A}"}}"#));
    assert_eq!(Tracker::new(Mri::new(LAB_B)).observe_delivery(&from), Observed::Ignored);
    assert_eq!(unread_invitation(&from), "its notification names `from` twice");
    // and a delivery that isn't an invitation at all says why too
    let mut odd = invited.clone();
    odd.body = br#"{"evt":107,"gp":"not base64"}"#.to_vec();
    assert_eq!(unread_invitation(&odd), "its notification doesn't decode");
    odd.body = b"not JSON".to_vec();
    assert_eq!(unread_invitation(&odd), "its body isn't JSON");
}
#[test]
fn redelivery_is_kept_but_an_ended_ring_never_rings_again() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (invited, _, _) = attached(&tracker);
    assert!(tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&Delivery {
            id: 2,
            ..invited.clone()
        }),
        Observed::Offered
    );
    let (_, body) = attach_case().unwrap();
    let end = callbacks(&body)
        .into_iter()
        .find(|url| url.ends_with("/call/end/"))
        .unwrap();
    assert_eq!(
        tracker.observe_delivery(&delivery(3, Url::parse(&end).unwrap().path(), b"{}".to_vec())),
        Observed::RingEnded
    );
    assert!(!tracker.holds_floor());
    assert_eq!(tracker.observe_delivery(&invited), Observed::Ignored);
}
#[test]
fn conversation_end_and_machine_release_retire_the_offer() {
    for conversation_end in [false, true] {
        let tracker = Tracker::new(Mri::new(LAB_B));
        let (invited, _, attach) = attached(&tracker);
        if conversation_end {
            let (_, body) = attach_case().unwrap();
            let end = callbacks(&body)
                .into_iter()
                .find(|url| url.ends_with("/conversation/conversationEnd/"))
                .unwrap();
            assert_eq!(
                tracker.observe_delivery(&delivery(2, Url::parse(&end).unwrap().path(), b"{}".to_vec())),
                Observed::RingEnded
            );
        } else {
            tracker.let_go(&attach);
        }
        assert!(!tracker.holds_floor());
        assert_eq!(tracker.observe_delivery(&invited), Observed::Ignored);
    }
}
#[test]
fn the_account_offers_one_ringing_invitation_at_a_time() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (_, _, attach) = attached(&tracker);
    let (mut later, mut gp) = invitation(0, 2).unwrap();
    let text = gp.to_string().replace("/forked/", "/forked/next");
    gp = serde_json::from_str(&text).unwrap();
    later = invitation_delivery(&later.path, 2, &gp);
    assert_eq!(tracker.observe_delivery(&later), Observed::Ignored);
    tracker.let_go(&attach);
    assert_eq!(tracker.observe_delivery(&later), Observed::Offered);
}
#[test]
fn a_start_acceptance_may_arrive_before_its_successful_http_answer() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let pending = starting(&tracker);
    assert!(
        !tracker.holds_floor(),
        "pending start has the existing occupancy semantics"
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Acceptance
    );
    tracker.answered(pending, &Response::new(200, answer("conversation-1")));
    assert!(tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert!(!tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("call-1"))),
        Observed::Nothing
    );
}
#[test]
fn explicit_failed_or_unreadable_start_answers_bind_no_live_call() {
    for (status, body) in [(401, answer("c")), (200, b"{}".to_vec())] {
        let tracker = Tracker::new(Mri::new(LAB_A));
        tracker.answered(starting(&tracker), &Response::new(status, body));
        assert!(!tracker.holds_floor());
        assert_eq!(
            tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("call"))),
            Observed::Nothing
        );
    }
}
#[test]
fn acceptance_links_must_name_one_call_and_the_bound_call() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(200, answer("c")));
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, b"{}".to_vec())),
        Observed::AcceptanceUnbound
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(2, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("two"))),
        Observed::AcceptanceUnbound
    );
}
#[test]
fn a_live_calls_media_deliveries_bind_only_their_own_call_links() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(200, answer("c")));
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    let at = format!(
        "{}/08080808/call/mediaRenegotiation/",
        Url::parse(CALLBACKS).unwrap().path()
    );
    let body = |call: &str| {
        json!({"mediaNegotiation":{"links":{"mediaAnswer":format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/{call}/1/a1/1/answer?i=1")}}}).to_string().into_bytes()
    };
    assert_eq!(
        tracker.observe_delivery(&delivery(2, &at, body("other"))),
        Observed::Nothing
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(3, &at, body("one"))),
        Observed::Renegotiation
    );
}

fn registered() -> Tracker {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(200, answer("conversation-1")));
    tracker
}

fn media_delivery(call: &str) -> Delivery {
    let path = format!(
        "{}/08080808/call/mediaRenegotiation/",
        Url::parse(CALLBACKS).unwrap().path()
    );
    let body = json!({"mediaNegotiation": {"links": {
        "mediaAnswer": format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/{call}/1/a1/1/answer?i=1")
    }}});
    delivery(20, &path, body.to_string().into_bytes())
}

#[test]
fn an_acceptance_off_the_callback_or_naming_two_calls_cannot_bind() {
    let tracker = registered();
    let elsewhere = ACCEPTANCE_PATH.replace("agent-1", "agent-2");
    assert_eq!(
        tracker.observe_delivery(&delivery(1, &elsewhere, acceptance("one"))),
        Observed::Nothing
    );
    let split = String::from_utf8(acceptance("one"))
        .unwrap()
        .replace("mcProxy/one", "mcProxy/two");
    assert_eq!(
        tracker.observe_delivery(&delivery(2, ACCEPTANCE_PATH, split.into_bytes())),
        Observed::AcceptanceUnbound
    );
    assert_eq!(tracker.observe_delivery(&media_delivery("one")), Observed::Nothing);
    for id in [3, 4] {
        assert_eq!(
            tracker.observe_delivery(&delivery(id, ACCEPTANCE_PATH, acceptance("one"))),
            Observed::Acceptance
        );
    }
    assert_eq!(
        tracker.observe_delivery(&media_delivery("one")),
        Observed::Renegotiation
    );
}

#[test]
fn an_end_before_the_start_answer_cannot_be_revived_by_the_late_answer() {
    for path in [END_PATH, CONVERSATION_END_PATH] {
        let tracker = Tracker::new(Mri::new(LAB_A));
        let pending = starting(&tracker);
        assert_eq!(
            tracker.observe_delivery(&delivery(1, path, b"{}".to_vec())),
            Observed::Ended
        );
        tracker.answered(pending, &Response::new(200, answer("conversation-1")));
        assert!(!tracker.holds_floor());
        assert_eq!(
            tracker.observe_delivery(&delivery(2, ACCEPTANCE_PATH, acceptance("one"))),
            Observed::Nothing
        );
    }
}

#[test]
fn a_retry_after_an_explicit_rejection_binds_the_new_start() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    tracker.answered(starting(&tracker), &Response::new(503, answer("not-taken")));
    tracker.answered(starting(&tracker), &Response::new(200, answer("taken")));
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert!(tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(2, CONVERSATION_END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert!(!tracker.holds_floor());
}

#[test]
fn an_unknown_start_outcome_keeps_its_callbacks_until_the_end() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let _unknown = starting(&tracker);
    // No answered hook: the request went out but no HTTP outcome is known.
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec())),
        Observed::Ended
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(3, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Nothing
    );
}

#[test]
fn a_start_that_never_went_out_cannot_bind_late_callbacks_or_be_revived() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let pending = starting(&tracker);
    tracker.not_sent(pending);
    tracker.answered(pending, &Response::new(200, answer("late")));
    assert!(!tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Nothing
    );
    assert_eq!(
        tracker.observe_delivery(&delivery(2, END_PATH, b"{}".to_vec())),
        Observed::Nothing
    );
}

#[test]
fn a_not_sent_notice_cannot_end_a_live_call_or_another_kind_of_attempt() {
    let tracker = Tracker::new(Mri::new(LAB_A));
    let pending = starting(&tracker);
    tracker.answered(pending, &Response::new(200, answer("conversation-1")));
    for attempt in [
        pending,
        Attempt::Nothing,
        Attempt::Answer {
            offer: 0,
            operation: CallOperation::Attach,
        },
    ] {
        tracker.not_sent(attempt);
        assert!(tracker.holds_floor());
    }
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    assert_eq!(
        tracker.observe_delivery(&media_delivery("one")),
        Observed::Renegotiation
    );
}

#[test]
fn echo_updates_keep_the_call_live_and_its_end_after_leave_retires_it() {
    let tracker = registered();
    let fixture = include_str!("../../../../tests/fixtures/calling/echo/deliveries.json")
        .replace("<guid:a>", GUID_A)
        .replace("<guid:echo>", ECHO.trim_start_matches("28:"));
    let fixture: Value = serde_json::from_str(&fixture).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let path = format!(
            "{}/{}",
            Url::parse(CALLBACKS).unwrap().path(),
            case["callback"].as_str().unwrap()
        );
        if case["observed"] == "ended" {
            tracker.before(&write(
                &format!("{}/leave?{ROUTE}", conversation("conversation-1")),
                Value::Null,
            ));
            assert!(!tracker.holds_floor(), "a leaving call releases the floor");
            assert_eq!(
                tracker.observe_delivery(&delivery(10, &path, case["body"].to_string().into_bytes())),
                Observed::Ended
            );
        } else {
            assert_eq!(
                tracker.observe_delivery(&delivery(10, &path, case["body"].to_string().into_bytes())),
                Observed::Nothing
            );
            assert!(tracker.holds_floor());
        }
    }
    assert_eq!(
        tracker.observe_delivery(&delivery(11, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Nothing
    );
}

#[test]
fn a_retarget_moves_media_binding_only_after_a_success_with_one_call_leg() {
    let tracker = registered();
    assert_eq!(
        tracker.observe_delivery(&delivery(1, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    let acknowledged = format!(
        "{}/09090909/call/mediaAcknowledgement/",
        Url::parse(CALLBACKS).unwrap().path()
    );
    let active = |call: &str, suffix: &str| format!("{PROXY}/{CC_HOST}/cc/v1/active/{call}/1/a4/5{suffix}?i=1");
    for body in [
        json!({"mediaAcknowledgement":{"resultCategories":["Failure"]},"links":{"callLeg":active("two", "")}}),
        json!({"mediaAcknowledgement":{"resultCategories":["Success"]},"links":{"callLeg":active("two", ""),"mediaRenegotiation":active("three", "/renegotiate")}}),
        json!({"mediaAcknowledgement":{"resultCategories":["Success"]},"links":{"mediaRenegotiation":active("two", "/renegotiate")}}),
    ] {
        assert_eq!(
            tracker.observe_delivery(&delivery(2, &acknowledged, body.to_string().into_bytes())),
            Observed::Renegotiation
        );
        assert_eq!(tracker.observe_delivery(&media_delivery("two")), Observed::Nothing);
        assert_eq!(
            tracker.observe_delivery(&media_delivery("one")),
            Observed::Renegotiation
        );
    }
    // This synthetic form preserves the capture's duplicate callLeg: JSON.parse keeps the final value.
    let body = json!({"mediaAcknowledgement":{"resultCategories":["Success"]},"links":{
        "callLeg":active("two", ""),"mediaRenegotiation":active("two", "/renegotiate")
    }})
    .to_string()
    .replacen(r#""links":{"#, r#""links":{"callLeg":null,"#, 1);
    assert_eq!(
        tracker.observe_delivery(&delivery(
            3,
            &acknowledged.replace("agent-1", "agent-2"),
            body.clone().into_bytes()
        )),
        Observed::Nothing
    );
    assert_eq!(tracker.observe_delivery(&media_delivery("two")), Observed::Nothing);
    assert_eq!(
        tracker.observe_delivery(&delivery(4, &acknowledged, body.into_bytes())),
        Observed::Renegotiation
    );
    assert_eq!(
        tracker.observe_delivery(&media_delivery("two")),
        Observed::Renegotiation
    );
    assert_eq!(tracker.observe_delivery(&media_delivery("one")), Observed::Nothing);
}

fn follow_ups() -> Value {
    serde_json::from_str(&fill(CHAT_FOLLOW_UPS)).unwrap()
}
fn fixture_write(fixture: &Value, index: usize) -> Write {
    let case = &fixture["cases"][index];
    write(case["url"].as_str().unwrap(), case["body"].clone())
}
fn bound_offer(tracker: &Tracker) -> (Delivery, String, Value) {
    let (invited, _, attach) = attached(tracker);
    let fixture = follow_ups();
    let pending = tracker.before(&fixture_write(&fixture, 1));
    tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
    (invited, attach, fixture)
}
fn callee_negotiation(fixture: &Value, index: usize) -> Delivery {
    let callbacks = callbacks(&fixture["cases"][1]["body"]);
    let agent = callbacks
        .iter()
        .filter_map(|callback| parse_callback(callback))
        .find_map(|callback| agent_of(&callback))
        .unwrap();
    let case = &fixture["deliveries"][index];
    delivery(
        30,
        &format!("{}{}", agent.path, case["callback"].as_str().unwrap()),
        case["body"].to_string().into_bytes(),
    )
}

#[test]
fn a_ring_end_before_the_attach_answer_suppresses_the_late_offer() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (invited, _, _) = attached(&tracker);
    let fixture = follow_ups();
    let pending = tracker.before(&fixture_write(&fixture, 1));
    let end = fixture["cases"][1]["body"]["attach"]["links"]["end"].as_str().unwrap();
    assert_eq!(
        tracker.observe_delivery(&delivery(2, Url::parse(end).unwrap().path(), b"{}".to_vec())),
        Observed::RingEnded
    );
    tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
    assert!(!tracker.holds_floor());
    assert_eq!(tracker.observe_delivery(&invited), Observed::Ignored);
    assert_eq!(
        tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
        Observed::Nothing
    );
}

#[test]
fn a_rejected_attach_can_be_retried_and_an_answered_offer_is_not_let_go() {
    let tracker = Tracker::new(Mri::new(LAB_B));
    let (invited, _, attach) = attached(&tracker);
    let fixture = follow_ups();
    for status in [401, 503] {
        let pending = tracker.before(&fixture_write(&fixture, 1));
        tracker.answered(pending, &Response::new(status, fixture["attach_answer"].to_string()));
        assert_eq!(tracker.observe_delivery(&invited), Observed::Offered);
        assert!(tracker.holds_floor());
    }
    let pending = tracker.before(&fixture_write(&fixture, 1));
    tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
    assert_eq!(
        tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
        Observed::Nothing,
        "not answered yet"
    );
    let pending = tracker.before(&fixture_write(&fixture, 4));
    tracker.answered(pending, &Response::new(200, fixture["accept_answer"].to_string()));
    tracker.let_go(&attach);
    assert!(tracker.holds_floor());
    for index in [0, 1] {
        let negotiated = callee_negotiation(&fixture, index);
        assert_eq!(tracker.observe_delivery(&negotiated), Observed::Renegotiation);
        let elsewhere = Delivery {
            path: negotiated.path.replace("callAgent/", "callAgent/other"),
            ..negotiated.clone()
        };
        assert_eq!(tracker.observe_delivery(&elsewhere), Observed::Nothing);
        let other = Delivery {
            body: String::from_utf8(negotiated.body.clone())
                .unwrap()
                .replace("SyntheticCall1", "SyntheticCall9")
                .into_bytes(),
            ..negotiated
        };
        assert_eq!(tracker.observe_delivery(&other), Observed::Nothing);
    }
    tracker.before(&fixture_write(&fixture, 8));
    assert!(!tracker.holds_floor());
    assert_eq!(
        tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
        Observed::Nothing
    );
}

#[test]
fn a_new_leg_of_the_same_conversation_is_offered_after_decline_or_leave() {
    for leave in [false, true] {
        let tracker = Tracker::new(Mri::new(LAB_B));
        let (_, _, fixture) = bound_offer(&tracker);
        if leave {
            let pending = tracker.before(&fixture_write(&fixture, 4));
            tracker.answered(pending, &Response::new(200, fixture["accept_answer"].to_string()));
            tracker.before(&fixture_write(&fixture, 8));
        } else {
            tracker.before(&fixture_write(&fixture, 5));
        }
        assert!(!tracker.holds_floor());
        let (invited, gp) = invitation(0, 40).unwrap();
        let gp: Value = serde_json::from_str(&gp.to_string().replace("SyntheticCall1/", "SyntheticCall2/")).unwrap();
        let next = invitation_delivery(&invited.path, 41, &gp);
        assert_eq!(tracker.observe_delivery(&next), Observed::Offered);
        let fixture: Value =
            serde_json::from_str(&fixture.to_string().replace("SyntheticCall1/", "SyntheticCall2/")).unwrap();
        let pending = tracker.before(&fixture_write(&fixture, 1));
        tracker.answered(pending, &Response::new(200, fixture["attach_answer"].to_string()));
        let pending = tracker.before(&fixture_write(&fixture, 4));
        tracker.answered(pending, &Response::new(200, fixture["accept_answer"].to_string()));
        assert!(tracker.holds_floor());
        assert_eq!(
            tracker.observe_delivery(&callee_negotiation(&fixture, 0)),
            Observed::Renegotiation
        );
    }
}

const PUSH_ROOT: &str = "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/";
const CAPTION_COMMAND: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
const ROSTER_PATH: &str = "/v4/f/SyntheticEndpoint00000/callAgent/agent-1/07070707/conversation/rosterUpdate/";
fn caption_actor() -> teams_proto::CallParticipant {
    teams_proto::CallParticipant {
        mri: Mri::new(LAB_A),
        display_name: None,
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
        language_id: None,
    }
}
fn caption_seed(tracker: &Tracker) -> Attempt {
    let mut body: Value = serde_json::from_slice(&start_body()).unwrap();
    body["participants"]["from"]["endpointId"] = json!(caption_actor().endpoint_id);
    body["participants"]["from"]["participantId"] = json!(caption_actor().participant_id);
    body["groupChat"] = json!({"threadId":teams_proto::one_to_one_id(LAB_A, LAB_B).unwrap()});
    body["conversationRequest"]["links"]["rosterUpdate"] =
        json!(format!("{}{ROSTER_PATH}", PUSH_ROOT.split("/v4/").next().unwrap()));
    tracker.before(&write(CALL_START_URL, body))
}
fn caption_start(actor: &teams_proto::CallParticipant) -> teams_proto::captions::CaptionWrite {
    teams_proto::captions::CaptionWrite::start(
        &teams_proto::CallContext::new(CHAIN_ID),
        &teams_proto::captions::CaptionCommandLink::parse(CAPTION_COMMAND).unwrap(),
        actor,
        &teams_proto::one_to_one_id(LAB_A, LAB_B).unwrap(),
    )
    .unwrap()
}
fn caption_roster(version: u64, state: &str, command: &str) -> Delivery {
    delivery(
        30,
        ROSTER_PATH,
        json!({"participants":{teams_proto::captions::CAPTION_SERVICE_MRI:{
            "version":version,"state":state,"endpoints":{"recorder":{"endpointMetadata":{
                "commandUrl":command,"spokenLanguage":"en-us","processingModes":{"closedCaptions":{"state":"Inactive"}}
            }}}
        }}})
        .to_string()
        .into_bytes(),
    )
}
fn assert_caption_actor_is_bound(tracker: &Tracker) {
    use teams_proto::CallState as _;
    let mut wrong = caption_actor();
    wrong.mri = Mri::new(LAB_B);
    assert!(!tracker.caption_allowed(&caption_start(&wrong)));
    wrong = caption_actor();
    wrong.endpoint_id = "00000000-0000-4000-8000-0000000000e3".into();
    assert!(!tracker.caption_allowed(&caption_start(&wrong)));
    wrong = caption_actor();
    wrong.participant_id = Some("00000000-0000-4000-8000-0000000000e3".into());
    assert!(!tracker.caption_allowed(&caption_start(&wrong)));
}
fn assert_caption_roster_updates_are_current(tracker: &Tracker, start: &teams_proto::captions::CaptionWrite) {
    use teams_proto::CallState as _;
    tracker.observe_delivery(&caption_roster(1, "disconnected", "invalid"));
    assert!(
        tracker.caption_allowed(start),
        "older roster delta cannot revoke newer service"
    );
    let mut other = caption_roster(3, "disconnected", "invalid");
    other.path = "/v4/f/Other/callAgent/agent/rosterUpdate/".into();
    tracker.observe_delivery(&other);
    assert!(
        tracker.caption_allowed(start),
        "another callback cannot update this service"
    );
    tracker.observe_delivery(&caption_roster(3, "active", "invalid"));
    assert!(
        !tracker.caption_allowed(start),
        "new malformed command revokes old link"
    );
    tracker.observe_delivery(&caption_roster(4, "active", CAPTION_COMMAND));
    assert!(tracker.caption_allowed(start));
}
#[test]
fn captions_follow_current_actor_call_roster_version_and_connection() {
    use teams_proto::CallState as _;
    let tracker = Tracker::new(Mri::new(LAB_A));
    let start = caption_start(&caption_actor());
    assert!(!tracker.caption_allowed(&start));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
    let pending = caption_seed(&tracker);
    assert!(!tracker.caption_allowed(&start));
    tracker.answered(pending, &Response::new(200, answer("caption")));
    let context = teams_proto::CallContext::new(CHAIN_ID);
    let chat = teams_proto::one_to_one_id(LAB_A, LAB_B).unwrap();
    let invite = teams_proto::captions::CaptionWrite::invite(
        &context,
        &teams_proto::CallLink::parse(&format!("{}/addParticipant?{ROUTE}", conversation("caption"))).unwrap(),
        &caption_actor(),
        &chat,
        (
            &format!("{CALLBACKS}/08080808/conversation/addParticipantSuccess/"),
            &format!("{CALLBACKS}/09090909/conversation/addParticipantFailure/"),
        ),
    )
    .unwrap();
    assert!(tracker.caption_allowed(&invite));
    assert!(!tracker.caption_allowed(&start));
    tracker.observe_delivery(&caption_roster(2, "active", CAPTION_COMMAND));
    assert!(tracker.caption_allowed(&start));
    assert_caption_actor_is_bound(&tracker);
    assert_caption_roster_updates_are_current(&tracker, &start);
    tracker.disconnected(CONNECTION);
    assert!(!tracker.caption_allowed(&start));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
    tracker.observe_delivery(&caption_roster(5, "disconnected", CAPTION_COMMAND));
    assert!(!tracker.caption_allowed(&start));
    tracker.observe_delivery(&caption_roster(6, "active", CAPTION_COMMAND));
    assert!(tracker.caption_allowed(&start));
    tracker.observe_delivery(&delivery(31, END_PATH, b"{}".to_vec()));
    assert!(!tracker.caption_allowed(&invite));
    assert!(!tracker.caption_allowed(&start));
}
#[test]
fn an_answered_incoming_call_authorizes_only_its_current_regional_caption_service() {
    use teams_proto::CallState as _;
    let tracker = Tracker::new(Mri::new(LAB_B));
    let root = "https://pub-ent-zz-02-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00001/";
    assert!(tracker.connected(CONNECTION, root));
    let (_, _, fixture) = bound_offer(&tracker);
    let input = &fixture["cases"][1]["body"]["additionalActions"][0]["input"];
    let own = &input["participants"]["from"];
    let actor = teams_proto::CallParticipant {
        mri: Mri::new(LAB_B),
        display_name: None,
        endpoint_id: own["endpointId"].as_str().unwrap().into(),
        participant_id: Some(own["participantId"].as_str().unwrap().into()),
        language_id: None,
    };
    let start = caption_start(&actor);
    assert!(
        !tracker.caption_allowed(&start),
        "a ringing offer cannot start captions"
    );
    let pending = tracker.before(&fixture_write(&fixture, 4));
    tracker.answered(pending, &Response::new(200, fixture["accept_answer"].to_string()));
    assert!(!tracker.caption_allowed(&start), "the service has not joined yet");
    let callback = Url::parse(input["conversationRequest"]["roster"]["rosterUpdate"].as_str().unwrap()).unwrap();
    let mut roster = caption_roster(1, "active", CAPTION_COMMAND);
    roster.path = callback.path().into();
    roster.body = String::from_utf8(roster.body)
        .unwrap()
        .replace(
            teams_proto::captions::CAPTION_SERVICE_MRI,
            teams_proto::captions::CAPTION_SERVICE_ALTERNATE_MRI,
        )
        .into_bytes();
    tracker.observe_delivery(&roster);
    assert!(tracker.caption_allowed(&start));
    let mut departed: Value = serde_json::from_slice(&roster.body).unwrap();
    let service = &mut departed["participants"][teams_proto::captions::CAPTION_SERVICE_ALTERNATE_MRI];
    service["version"] = json!(2);
    service["state"] = json!("disconnected");
    roster.body = departed.to_string().into_bytes();
    tracker.observe_delivery(&roster);
    assert!(!tracker.caption_allowed(&start), "departure revokes the command");
}

/// The service joining from another region, with an older endpoint whose key sorts first: the command allowed is the
/// newest endpoint's, as the call machine follows it.
#[test]
fn captions_allow_the_newest_endpoint_of_the_service_from_another_region() {
    use teams_proto::CallState as _;
    let tracker = Tracker::new(Mri::new(LAB_A));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
    let pending = caption_seed(&tracker);
    tracker.answered(pending, &Response::new(200, answer("caption")));
    tracker.observe_delivery(&caption_roster(20, "disconnected", "invalid"));
    let start = caption_start(&caption_actor());
    let older = CAPTION_COMMAND.replace("000000000001", "000000000002");
    let endpoint = |command: &str, joined: &str| {
        json!({"endpointMetadata": {"commandUrl": command, "joinTime": joined, "spokenLanguage": "en-us",
            "processingModes": {"closedCaptions": {"state": "Inactive"}}}})
    };
    let roster = json!({"participants": {teams_proto::captions::CAPTION_SERVICE_ALTERNATE_MRI: {
        "version": 2, "state": "active", "endpoints": {
            "a-older": endpoint(&older, "2026-01-01T00:00:01.5Z"),
            "b-newer": endpoint(CAPTION_COMMAND, "2026-01-01T00:00:02Z"),
        }
    }}});
    tracker.observe_delivery(&delivery(30, ROSTER_PATH, roster.to_string().into_bytes()));
    assert!(tracker.caption_allowed(&start));
    let stale = teams_proto::captions::CaptionWrite::start(
        &teams_proto::CallContext::new(CHAIN_ID),
        &teams_proto::captions::CaptionCommandLink::parse(&older).unwrap(),
        &caption_actor(),
        &teams_proto::one_to_one_id(LAB_A, LAB_B).unwrap(),
    )
    .unwrap();
    assert!(!tracker.caption_allowed(&stale));
    // A newer participant version for the other MRI cannot rebind commands to an older join.
    let older_entry = json!({"participants": {teams_proto::captions::CAPTION_SERVICE_MRI: {
        "version": 21, "state": "active", "endpoints": {
            "b-newer": endpoint(&older, "2026-01-01T00:00:01.5Z")
        }
    }}});
    tracker.observe_delivery(&delivery(30, ROSTER_PATH, older_entry.to_string().into_bytes()));
    assert!(tracker.caption_allowed(&start));
    assert!(!tracker.caption_allowed(&stale));
    // Switching MRIs does not forget the invited leg's version, or accept older alternate-leg state.
    tracker.observe_delivery(&caption_roster(19, "active", &older));
    let mut stale_roster = roster;
    let entry = &mut stale_roster["participants"][teams_proto::captions::CAPTION_SERVICE_ALTERNATE_MRI];
    entry["version"] = json!(1);
    entry["state"] = json!("disconnected");
    tracker.observe_delivery(&delivery(30, ROSTER_PATH, stale_roster.to_string().into_bytes()));
    assert!(tracker.caption_allowed(&start));
    assert!(!tracker.caption_allowed(&stale));
    // Process the followed MRI's departure even beside the other MRI's older full read.
    let mut both = older_entry;
    both["participants"][teams_proto::captions::CAPTION_SERVICE_MRI]["version"] = json!(22);
    both["participants"][teams_proto::captions::CAPTION_SERVICE_ALTERNATE_MRI] =
        json!({"version": 3, "state": "disconnected"});
    tracker.observe_delivery(&delivery(30, ROSTER_PATH, both.to_string().into_bytes()));
    assert!(
        !tracker.caption_allowed(&start),
        "departed endpoint loses command authorization"
    );
    assert!(tracker.caption_allowed(&stale), "still-active older region takes over");
}
fn assert_invalid_reply_envelopes_are_refused(tracker: &Tracker, envelope: &Value) {
    use teams_proto::CallState as _;
    for field in [
        "trouter-request",
        "X-Microsoft-Skype-Chain-ID",
        "X-Microsoft-Skype-Message-ID",
    ] {
        let mut wrong = envelope.clone();
        wrong["headers"][field] = json!("different");
        assert!(!tracker.response_allowed(CONNECTION, 20, &wrong.to_string()));
    }
    let mut wrong = envelope.clone();
    wrong["status"] = json!(500);
    assert!(!tracker.response_allowed(CONNECTION, 20, &wrong.to_string()));
    wrong = envelope.clone();
    wrong["id"] = json!(21);
    assert!(!tracker.response_allowed(CONNECTION, 20, &wrong.to_string()));
    wrong = envelope.clone();
    wrong["body"] = json!("not json");
    assert!(!tracker.response_allowed(CONNECTION, 20, &wrong.to_string()));
    wrong = envelope.clone();
    wrong["body"]=json!({"callAcceptanceAcknowledgement":{"links":{"end":"https://other.trouter.teams.microsoft.com/v4/f/Other/callAgent/a/end/"}}}).to_string().into();
    assert!(!tracker.response_allowed(CONNECTION, 20, &wrong.to_string()));
}
#[test]
fn only_current_registered_delivery_replies_pass_once_with_exact_echoed_headers() {
    use teams_proto::CallState as _;
    let tracker = Tracker::new(Mri::new(LAB_A));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
    tracker.answered(starting(&tracker), &Response::new(200, answer("c")));
    let accepted = delivery(20, ACCEPTANCE_PATH, acceptance("call"));
    assert_eq!(tracker.observe_delivery(&accepted), Observed::Acceptance);
    let envelope = json!({"id":20,"status":200,"headers":{
        "trouter-request":TROUTER_REQUEST,"X-Microsoft-Skype-Chain-ID":CHAIN_ID,
        "X-Microsoft-Skype-Message-ID":MESSAGE_ID
    },"body":json!({"callAcceptanceAcknowledgement":{"links":{}}}).to_string()});
    assert!(!tracker.response_allowed("other", 20, &envelope.to_string()));
    assert!(!tracker.response_allowed(CONNECTION, 20, "not json"));
    assert_invalid_reply_envelopes_are_refused(&tracker, &envelope);
    assert!(tracker.response_allowed(CONNECTION, 20, &envelope.to_string()));
    assert!(!tracker.response_allowed(CONNECTION, 20, &envelope.to_string()));
    tracker.observe_delivery(&accepted);
    tracker.disconnected(CONNECTION);
    assert!(!tracker.response_allowed(CONNECTION, 20, &envelope.to_string()));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
    tracker.observe_delivery(&accepted);
    tracker.observe_delivery(&delivery(21, END_PATH, b"{}".to_vec()));
    assert!(!tracker.response_allowed(CONNECTION, 20, &envelope.to_string()));
}
#[test]
fn connection_roots_are_first_party_complete_endpoint_roots() {
    use teams_proto::CallState as _;
    let tracker = Tracker::new(Mri::new(LAB_A));
    for root in [
        "invalid",
        "http://pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/f/Endpoint/",
        "https://unrelated.example/v4/f/Endpoint/",
        "https://u:p@pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/f/Endpoint/",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:444/v4/f/Endpoint/",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/f/Endpoint/?token=1",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/f/Endpoint/#part",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/f/Endpoint/callAgent/",
    ] {
        assert!(!tracker.connected(CONNECTION, root), "{root}");
    }
    assert!(!tracker.connected("", PUSH_ROOT));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
}

#[test]
fn current_call_links_keep_their_bound_sender_without_a_recipient_allowlist() {
    use teams_proto::CallState as _;
    let tracker = Tracker::new(Mri::new(LAB_A));
    let start_url = Url::parse(CALL_START_URL).unwrap();
    let request: Value = serde_json::from_slice(&start_body()).unwrap();
    assert!(!tracker.call_allowed(&start_url, Some(&request)));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
    assert!(tracker.call_allowed(&start_url, Some(&request)));
    let pending = caption_seed(&tracker);
    tracker.answered(pending, &Response::new(200, answer("actor")));
    let leave = Url::parse(&format!("{}/leave?{ROUTE}", conversation("actor"))).unwrap();
    let actor =
        json!({"id":LAB_A,"endpointId":caption_actor().endpoint_id,"participantId":caption_actor().participant_id});
    let body = json!({"participants":{"from":actor,"to":[{"id":COWORKER}]}});
    assert!(
        tracker.call_allowed(&leave, Some(&body)),
        "a recipient is no runtime policy decision"
    );
    for pointer in [
        "/participants/from",
        "/from",
        "/sender",
        "/callProgress/sender",
        "/callAcceptance/acceptedBy",
        "/mediaNegotiation/sender",
        "/mediaAnswer/sender",
    ] {
        let fields: Vec<_> = pointer.trim_start_matches('/').split('/').collect();
        let mut foreign = json!({"id":LAB_B});
        for field in fields.into_iter().rev() {
            foreign = json!({field:foreign});
        }
        assert!(!tracker.call_allowed(&leave, Some(&foreign)), "{pointer}");
    }
    assert!(!tracker.call_allowed(
        &leave,
        Some(&json!({"additionalActions":[{"input":{"participants":{"from":{"id":LAB_B}}}}]}))
    ));
    let mut changed = body.clone();
    changed["participants"]["from"]["endpointId"] = json!("different");
    tracker.before(&write(leave.as_str(), changed.clone()));
    assert!(
        !tracker.call_allowed(&leave, Some(&changed)),
        "before cannot rebind an existing call actor"
    );
    assert!(tracker.call_allowed(&leave, Some(&body)));
    let other = Url::parse(&format!("{}/leave?{ROUTE}", conversation("other"))).unwrap();
    assert!(!tracker.call_allowed(&other, Some(&body)));
    let foreign_callback =
        json!({"links":{"end":"https://other.trouter.teams.microsoft.com/v4/f/Other/callAgent/a/end/"}});
    assert!(!tracker.call_allowed(&leave, Some(&foreign_callback)));
    tracker.disconnected(CONNECTION);
    assert!(
        tracker.call_allowed(&leave, Some(&body)),
        "bound cleanup survives socket loss"
    );
}

fn current_reply_tracker() -> Tracker {
    use teams_proto::CallState as _;
    let tracker = Tracker::new(Mri::new(LAB_A));
    assert!(tracker.connected(CONNECTION, PUSH_ROOT));
    tracker.answered(caption_seed(&tracker), &Response::new(200, answer("c")));
    assert_eq!(
        tracker.observe_delivery(&delivery(20, ACCEPTANCE_PATH, acceptance("one"))),
        Observed::Acceptance
    );
    tracker
}

fn plain_receipt(delivery: &Delivery) -> String {
    json!({"id":delivery.id,"status":200,"headers":{"trouter-request":TROUTER_REQUEST},"body":""}).to_string()
}

fn callback_request_body(callback: &str) -> Value {
    let actor = caption_actor();
    json!({"participants":{"from":{
        "id":actor.mri.as_str(),"endpointId":actor.endpoint_id,"participantId":actor.participant_id
    }},"links":{"mediaRenegotiation":callback}})
}

#[test]
fn subsequent_call_requests_register_callbacks_before_their_deliveries() {
    use teams_proto::CallState as _;
    for request_url in [
        acknowledge_url("one"),
        format!("{PROXY}/{CC_HOST}/cc/v1/negotiations/one/1/a1/1/answer?i=1"),
    ] {
        let tracker = current_reply_tracker();
        let mut media = media_delivery("one");
        media.id = 21;
        assert_eq!(tracker.observe_delivery(&media), Observed::Renegotiation);
        assert!(!tracker.response_allowed(CONNECTION, media.id, &plain_receipt(&media)));
        let callback = format!("{CALLBACKS}/08080808/call/mediaRenegotiation/");
        let request = write(&request_url, callback_request_body(&callback));
        assert!(tracker.call_allowed(request.call_url().unwrap(), request.body().as_ref()));
        tracker.before(&request);
        assert_eq!(tracker.observe_delivery(&media), Observed::Renegotiation);
        assert!(tracker.response_allowed(CONNECTION, media.id, &plain_receipt(&media)));
        assert!(!tracker.response_allowed(CONNECTION, media.id, &plain_receipt(&media)));
    }
}

#[test]
fn later_request_callbacks_cannot_rebind_a_foreign_actor_endpoint_or_call() {
    use teams_proto::CallState as _;
    let callback = format!("{CALLBACKS}/08080808/call/mediaRenegotiation/");
    for changed in ["sender", "endpoint", "participant", "root", "call", "ended"] {
        let tracker = current_reply_tracker();
        let mut body = callback_request_body(&callback);
        let mut url = acknowledge_url("one");
        match changed {
            "sender" => body["participants"]["from"]["id"] = json!(LAB_B),
            "endpoint" => body["participants"]["from"]["endpointId"] = json!("another"),
            "participant" => body["participants"]["from"]["participantId"] = json!("another"),
            "root" => body["links"]["other"] = json!(callback.replace("SyntheticEndpoint00000", "Other")),
            "call" => url = acknowledge_url("another"),
            _ => assert_eq!(
                tracker.observe_delivery(&delivery(22, END_PATH, b"{}".to_vec())),
                Observed::Ended
            ),
        }
        tracker.before(&write(&url, body));
        let media = media_delivery("one");
        tracker.observe_delivery(&media);
        assert!(
            !tracker.response_allowed(CONNECTION, media.id, &plain_receipt(&media)),
            "{changed}"
        );
    }
}

fn acceptance_receipt(callback: &str) -> Value {
    json!({"id":20,"status":200,"headers":{
        "trouter-request":TROUTER_REQUEST,"X-Microsoft-Skype-Chain-ID":CHAIN_ID,
        "X-Microsoft-Skype-Message-ID":MESSAGE_ID
    },"body":json!({"callAcceptanceAcknowledgement":{"links":{"mediaRenegotiation":callback}}}).to_string()})
}

#[test]
fn a_valid_acceptance_reply_registers_new_callbacks_even_before_the_start_answer() {
    use teams_proto::CallState as _;
    for before_answer in [false, true] {
        let tracker = Tracker::new(Mri::new(LAB_A));
        assert!(tracker.connected(CONNECTION, PUSH_ROOT));
        let pending = caption_seed(&tracker);
        if !before_answer {
            tracker.answered(pending, &Response::new(200, answer("c")));
        }
        tracker.observe_delivery(&delivery(20, ACCEPTANCE_PATH, acceptance("one")));
        let callback = format!("{CALLBACKS}/08080808/call/mediaRenegotiation/");
        let mut invalid = acceptance_receipt(&callback);
        let mut content: Value = serde_json::from_str(invalid["body"].as_str().unwrap()).unwrap();
        content["unrelated"] = json!(callback.replace("SyntheticEndpoint00000", "Other"));
        invalid["body"] = json!(content.to_string());
        assert!(!tracker.response_allowed(CONNECTION, 20, &invalid.to_string()));
        assert!(tracker.response_allowed(CONNECTION, 20, &acceptance_receipt(&callback).to_string()));
        assert!(!tracker.response_allowed(CONNECTION, 20, &acceptance_receipt(&callback).to_string()));
        if before_answer {
            tracker.answered(pending, &Response::new(200, answer("c")));
        }
        let mut media = media_delivery("one");
        media.id = 23;
        assert_eq!(tracker.observe_delivery(&media), Observed::Renegotiation);
        assert!(tracker.response_allowed(CONNECTION, media.id, &plain_receipt(&media)));
    }
}

#[test]
fn own_tracker_binds_only_own_conversation_and_controller_links() {
    use teams_proto::CallState as _;
    let cloud = Cloud::own("https://calls.example:8443").unwrap();
    let tracker = Tracker::for_cloud(Mri::new(LAB_A), cloud.clone());
    let own = |text: &str| {
        text.replace(
            "https://api.flightproxy.teams.microsoft.com",
            "https://calls.example:8443/flightproxy",
        )
        .replace(
            "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/",
            "https://calls.example:8443/trouter/v4/f/",
        )
    };
    let start: Value = serde_json::from_str(&own(&String::from_utf8(start_body()).unwrap())).unwrap();
    assert!(tracker.connected(
        CONNECTION,
        "https://calls.example:8443/trouter/v4/f/SyntheticEndpoint00000/"
    ));
    let attempt = tracker.before(&write("https://calls.example:8443/flightproxy/api/v2/epconv", start));
    let controller = own(&conversation("own"));
    let body = json!({"conversationController": controller, "links": {"leave": format!("{controller}/leave")}});
    tracker.answered(attempt, &Response::new(200, body.to_string().into_bytes()));
    assert!(tracker.holds_floor());
    assert!(tracker.call_allowed(&Url::parse(&format!("{controller}/leave")).unwrap(), None));
    assert!(!tracker.call_allowed(
        &Url::parse(CALL_START_URL).unwrap(),
        Some(&json!({"conversationRequest": {}}))
    ));
    let acceptance =
        json!({"callAcceptance": {"links": {"callLeg": format!("{PROXY}/{CC_HOST}/cc/v1/active/own/1/a/1")}}});
    let callback = format!("/trouter{ACCEPTANCE_PATH}");
    let mut delivery = delivery(900, &callback, acceptance.to_string().into_bytes());
    assert_eq!(tracker.observe_delivery(&delivery), Observed::AcceptanceUnbound);
    delivery.body = own(&acceptance.to_string()).into_bytes();
    assert_eq!(tracker.observe_delivery(&delivery), Observed::Acceptance);
}

#[test]
fn own_invitations_refuse_foreign_and_mixed_controller_origins() {
    let cloud = Cloud::own("https://calls.example:8443").unwrap();
    let (microsoft, gp) = invitation(0, 1).unwrap();
    let tracker = || Tracker::for_cloud(Mri::new(LAB_B), cloud.clone());
    assert_eq!(tracker().observe_delivery(&microsoft), Observed::Ignored);
    let root = format!("/trouter{}", microsoft.path);
    let mut own_gp: Value = serde_json::from_str(&gp.to_string().replace(
        "https://api.flightproxy.teams.microsoft.com",
        "https://calls.example:8443/flightproxy",
    ))
    .unwrap();
    let own = invitation_delivery(&root, 2, &own_gp);
    assert_eq!(tracker().observe_delivery(&own), Observed::Offered);
    assert_eq!(Tracker::new(Mri::new(LAB_B)).observe_delivery(&own), Observed::Ignored);
    own_gp["conversationInvitation"]["conversationController"] =
        gp["conversationInvitation"]["conversationController"].clone();
    assert_eq!(
        tracker().observe_delivery(&invitation_delivery(&root, 3, &own_gp)),
        Observed::Ignored
    );
}
