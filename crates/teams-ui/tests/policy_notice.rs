//! What a message a tenant's policy acted on says, in Teams' words: the line and link each side reads, the colours, and
//! the one line the chat list, a notification and a reply's quote read of a message whose text was withheld.

use teams_store::Message;
use teams_ui::chat_list;
use teams_ui::compose::Quoted;
use teams_ui::policy_notice::{self, PolicyNotice};

fn message(content: &str, properties: &str) -> Message {
    Message {
        conversation_id: "19:synthetic-blocked@thread.v2".to_owned(),
        id: "1760000100000".to_owned(),
        version: 2,
        client_message_id: None,
        sender: Some("8:orgid:synthetic-peer".to_owned()),
        sender_name: Some("Synthetic".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: Some(properties.to_owned()),
        arrival_time: 1_760_000_100_000,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

const FILE_TYPE: &str = r#"{"policyViolation":{"state":1,"violationType":"wft","genericStreamItemId":900}}"#;

#[test]
fn a_file_type_block_reads_the_same_to_both_sides_with_the_safety_page() {
    for own in [true, false] {
        let notice = policy_notice::of(Some(FILE_TYPE), own).unwrap();
        assert_eq!(notice, PolicyNotice::FileTypeBlocked);
        assert_eq!(
            policy_notice::words(notice, own),
            "This message was blocked because it contains a file type that is not permitted."
        );
        assert_eq!(
            policy_notice::link(notice, own),
            Some((
                "Learn about file and link safety",
                "https://go.microsoft.com/fwlink/?linkid=2293545"
            ))
        );
        assert!(policy_notice::is_error(notice, own));
    }
}

#[test]
fn a_policy_block_reads_to_its_sender_and_to_everyone_else_as_teams_words_each() {
    let blocked = PolicyNotice::Blocked { reported: false };
    assert_eq!(policy_notice::words(blocked, true), "This message was blocked.");
    assert_eq!(
        policy_notice::link(blocked, true),
        Some(("Learn more.", policy_notice::POLICY_PAGE))
    );
    assert_eq!(
        policy_notice::words(blocked, false),
        "This message was blocked due to organization policy."
    );
    assert_eq!(
        policy_notice::link(blocked, false),
        Some(("What's this?", policy_notice::POLICY_PAGE))
    );
    assert!(policy_notice::is_error(blocked, true) && !policy_notice::is_error(blocked, false));
    let reported = PolicyNotice::Blocked { reported: true };
    assert_eq!(
        policy_notice::words(reported, true),
        "Blocked. You've reported this to your admin."
    );
    assert_eq!(policy_notice::link(reported, true), None);
}

#[test]
fn warnings_flags_and_harmful_content_read_as_teams_words_them() {
    let cases = [
        (
            PolicyNotice::HarmfulContentBlocked,
            "This message was blocked due to potentially harmful content.",
            Some(policy_notice::HARMFUL_CONTENT_PAGE),
        ),
        (
            PolicyNotice::HarmfulLink,
            "This message contains a link that might be harmful.",
            Some(policy_notice::FILE_AND_LINK_SAFETY_PAGE),
        ),
        (
            PolicyNotice::Phishing,
            "This message could be a phishing attempt.",
            None,
        ),
        (
            PolicyNotice::Flagged { reported: false },
            "This message was flagged.",
            Some(policy_notice::POLICY_PAGE),
        ),
        (
            PolicyNotice::Flagged { reported: true },
            "Flagged. You've reported this to your admin.",
            None,
        ),
    ];
    for (notice, words, page) in cases {
        assert_eq!(policy_notice::words(notice, true), words);
        assert_eq!(
            policy_notice::link(notice, true).map(|(_, page)| page),
            page,
            "{notice:?}"
        );
    }
    assert!(!policy_notice::is_error(PolicyNotice::HarmfulContentBlocked, false));
    assert!(policy_notice::is_error(PolicyNotice::Phishing, false));
}

#[test]
fn a_message_without_a_verdict_or_with_one_lifted_tells_nothing() {
    assert_eq!(policy_notice::of(None, true), None);
    assert_eq!(policy_notice::of(Some("not json"), true), None);
    assert_eq!(policy_notice::of(Some("{}"), true), None);
    assert_eq!(policy_notice::of(Some(r#"{"policyViolation":null}"#), true), None);
    assert_eq!(
        policy_notice::of(Some(r#"{"policyViolation":{"state":0}}"#), true),
        None
    );
}

#[test]
fn a_withheld_message_reads_as_its_notice_in_the_list_a_notification_and_a_quote() {
    let withheld = message("", FILE_TYPE);
    let line = "This message was blocked because it contains a file type that is not permitted.";
    assert_eq!(chat_list::message_line(&withheld), line);
    assert_eq!(Quoted::of(&withheld, "Synthetic").unwrap().preview, line);
    // a sender's own, its text put back, reads as its text
    let kept = message("<p>synthetic text</p>", FILE_TYPE);
    assert_eq!(chat_list::message_line(&kept), "synthetic text");
    assert_eq!(Quoted::of(&kept, "Synthetic").unwrap().preview, "synthetic text");
}
