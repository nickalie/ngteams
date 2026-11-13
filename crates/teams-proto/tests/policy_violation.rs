//! A message a tenant's policy acted on: the verdict (`properties.policyViolation`) read from the synthetic
//! `policy-blocked.json` shapes and from every form the web client reads, the notice each side is told, and the
//! service's stream of blocked messages' copies told apart from a chat.

use std::path::Path;

use serde_json::{Value, json};
use teams_mock::{LAB_A, LAB_B};
use teams_proto::chat::{
    Conversation, Field, GENERIC_STREAM, Message, PolicyNotice, PolicyState, PolicyViolation, ViolationKind,
    is_generic_stream,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc");

fn fixture() -> TestResult<Value> {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join("policy-blocked.json"))?
        .replace("<region>", "amer")
        .replace("<guid:a>", LAB_A.trim_start_matches("8:orgid:"))
        .replace("<guid:b>", LAB_B.trim_start_matches("8:orgid:"));
    Ok(serde_json::from_str(&text)?)
}

fn message(value: &Value) -> TestResult<Message> {
    Ok(serde_json::from_value(value.clone())?)
}

fn verdict(value: &Value) -> Field<PolicyViolation> {
    PolicyViolation::of(Some(value))
}

#[test]
fn a_blocked_update_reads_its_verdict_naming_the_copy() -> TestResult {
    let shapes = fixture()?;
    let blocked = message(&shapes["blocked"])?;
    let Field::Known(verdict) = blocked.policy_violation() else {
        return Err("no verdict".into());
    };
    assert_eq!(verdict.state, PolicyState::Blocked);
    assert_eq!(verdict.kind, Some(ViolationKind::FileType));
    assert_eq!(
        (verdict.verdict_details, verdict.dlp_action, verdict.user_action),
        (0, 2, 0)
    );
    assert_eq!(verdict.justification.as_deref(), Some("LinkScannerViolationHandler"));
    let copy = message(&shapes["copy"])?;
    assert_eq!(verdict.generic_stream_item.as_deref(), Some(copy.id.as_str()));
    assert!(verdict.blocks());
    // the verdict's own update carries the edit time, at its version
    assert_eq!(blocked.edit_time(), blocked.version);
    assert!(blocked.content.is_empty() && blocked.files().files.is_empty());
    Ok(())
}

#[test]
fn the_copy_and_the_later_update_read_as_their_shapes() -> TestResult {
    let shapes = fixture()?;
    let copy = message(&shapes["copy"])?;
    assert_eq!(copy.sender(), Some(LAB_A));
    assert_eq!(copy.files().files.len(), 1);
    assert!(copy.policy_violation().is_absent());
    assert_eq!(copy.extra["to"], GENERIC_STREAM);
    let later = message(&shapes["blocked_atp"])?;
    assert!(matches!(later.policy_violation(), Field::Known(verdict) if verdict.blocks()));
    let stream: Conversation = serde_json::from_value(shapes["stream"].clone())?;
    assert!(is_generic_stream(&stream.id));
    assert!(!is_generic_stream("48:notifications"));
    assert!(stream.is_read_only());
    // the message as sent has none
    assert!(message(&shapes["sent"])?.policy_violation().is_absent());
    Ok(())
}

#[test]
fn a_verdict_reads_as_an_object_or_its_json_text_and_anything_else_is_kept() {
    let object = json!({"state": 2, "violationType": "mu", "genericStreamItemId": "17", "userAction": null});
    let Field::Known(read) = verdict(&object) else {
        panic!("an object reads");
    };
    assert_eq!(read.state, PolicyState::Warning);
    assert_eq!(read.kind, Some(ViolationKind::LinkReputation));
    assert_eq!(read.generic_stream_item.as_deref(), Some("17"));
    assert_eq!(verdict(&Value::String(object.to_string())), Field::Known(read));
    for odd in [
        Value::Null,
        json!(1),
        json!("not json"),
        json!({}),
        json!({"state": "1"}),
        json!({"state": 1, "violationType": 3}),
        json!({"state": 1, "genericStreamItemId": "x1"}),
        json!({"state": 1, "genericStreamItemId": true}),
        json!({"state": 1, "dlpAction": -1}),
    ] {
        assert_eq!(verdict(&odd), Field::Unknown(odd.clone()), "{odd}");
    }
    assert!(PolicyViolation::of(None).is_absent());
}

#[test]
fn each_state_and_kind_reads_as_the_web_client_names_it() {
    let read = |state: u64, kind: &str| match verdict(&json!({"state": state, "violationType": kind})) {
        Field::Known(read) => (read.state, read.kind),
        other => panic!("{other:?}"),
    };
    assert_eq!(read(0, "dlp"), (PolicyState::None, Some(ViolationKind::Dlp)));
    assert_eq!(read(1, "zap"), (PolicyState::Blocked, Some(ViolationKind::Zap)));
    assert_eq!(read(2, "pd"), (PolicyState::Warning, Some(ViolationKind::Phishing)));
    assert_eq!(
        read(7, "cc"),
        (PolicyState::Other(7), Some(ViolationKind::Other("cc".to_owned())))
    );
}

#[test]
fn each_verdict_tells_its_sender_and_everyone_else_as_teams_does() {
    let notice = |value: Value, own: bool| match verdict(&value) {
        Field::Known(read) => read.notice(own),
        other => panic!("{other:?}"),
    };
    for own in [true, false] {
        assert_eq!(
            notice(json!({"state": 1, "violationType": "wft"}), own),
            Some(PolicyNotice::FileTypeBlocked)
        );
        assert_eq!(
            notice(json!({"state": 1, "violationType": "zap"}), own),
            Some(PolicyNotice::HarmfulContentBlocked)
        );
        assert_eq!(
            notice(json!({"state": 2, "violationType": "mu"}), own),
            Some(PolicyNotice::HarmfulLink)
        );
        assert_eq!(
            notice(json!({"state": 2, "violationType": "pd"}), own),
            Some(PolicyNotice::Phishing)
        );
        assert_eq!(
            notice(json!({"state": 1, "violationType": "dlp", "userAction": 2}), own),
            Some(PolicyNotice::Blocked { reported: true })
        );
        assert_eq!(notice(json!({"state": 0, "violationType": "wft"}), own), None);
        assert_eq!(notice(json!({"state": 9}), own), None);
    }
    // a flag is the sender's alone; a file type is a block only while it blocks
    assert_eq!(
        notice(json!({"state": 2, "violationType": "dlp"}), true),
        Some(PolicyNotice::Flagged { reported: false })
    );
    assert_eq!(notice(json!({"state": 2, "violationType": "dlp"}), false), None);
    assert_eq!(
        notice(json!({"state": 2, "violationType": "wft"}), true),
        Some(PolicyNotice::Flagged { reported: false })
    );
    assert_eq!(
        notice(json!({"state": 1}), false),
        Some(PolicyNotice::Blocked { reported: false })
    );
}
