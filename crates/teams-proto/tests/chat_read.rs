//! The chat reads and the message model: the Task 7/8 synthetic fixtures read into the model (versions, edits, deletes,
//! reactions, mentions, quotes, threads, members; unknown fields and kinds kept), the message HTML read into rich text,
//! and the reads sent through the native transport to the mock chat service (`teams-mock`, through the `test-support`
//! mock origin): pages, the service's own sync and backward links followed only on the same chat base, a region hop
//! not followed.
//!
//! Every id and token here is made up (the mock's, or a placeholder), never a real one.

use std::future::{self, Future};
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_mock::{AB_CHAT, ABC_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, NOTIFICATIONS, TENANT};
use teams_proto::chat::{
    AppConsumptionHorizon, ChatReadError, ChatReads, Code, CodeRun, Conversation, ConversationKind, ConversationList,
    Field, FileKind, FilePreview, LinkPermission, List, MAX_CONTENT, MAX_DEPTH, Marks, MemberList, MentionKind,
    Message, MessagePage, MessageText, MessageType, Node, Quote, RichText, STICKER_ITEMTYPE, SharedFile, SharedFiles,
    Sticker, Table, TableCell, Thread, Token, Unshown, WEB_CLIENT, forwarded, link_endpoint, onedrive_page, parse_html,
    read_link_permission, share_entry, share_token, shared_files, unshown_parts, web_chat_link,
};
use teams_proto::{Client, Cloud, Credentials, HttpError, HttpTransport};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc");

fn oid(mri: &str) -> &str {
    mri.trim_start_matches("8:orgid:")
}

/// A fixture with its placeholders filled in with the mock's made-up ids.
fn fixture(name: &str) -> TestResult<Value> {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(name))?;
    let group = ABC_CHAT.trim_start_matches("19:").trim_end_matches("@thread.v2");
    let text = text
        .replace("<region>", "amer")
        .replace("<guid:a>", oid(LAB_A))
        .replace("<guid:b>", oid(LAB_B))
        .replace("<guid:c>", oid(LAB_C))
        .replace("<guid:tenant>", TENANT)
        .replace("<guid:group>", "00000000-0000-4000-8000-000000000002")
        .replace("<guid:context>", "00000000-0000-4000-8000-000000000003")
        .replace("<group>", group)
        .replace("<token:1>", "fixture-token-1")
        .replace("<token:2>", "fixture-token-2");
    Ok(serde_json::from_str(&text)?)
}

fn body(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

fn message(value: &Value) -> TestResult<Message> {
    Ok(serde_json::from_value(value.clone())?)
}

fn text(text: &str) -> Node {
    Node::Text {
        text: text.to_owned(),
        marks: Marks::NONE,
    }
}

// ---- the fixtures read into the model ----

fn conversation_list(value: &Value) -> TestResult<ConversationList> {
    Ok(ConversationList::from_body(&body(value))?)
}

fn page_of(value: &Value) -> TestResult<MessagePage> {
    Ok(MessagePage::from_body(&body(value))?)
}

#[test]
fn the_conversation_list_reads_each_kind_and_marks_channels_read_only() -> TestResult {
    let fixture = fixture("conversations.json")?;
    let list = conversation_list(&fixture["response"]["body"])?;
    assert!(list.unreadable.is_empty());
    let kinds: Vec<ConversationKind> = list.conversations.iter().map(Conversation::kind).collect();
    assert_eq!(
        kinds,
        [
            ConversationKind::OneToOne,
            ConversationKind::OneToOne,
            ConversationKind::Channel,
            ConversationKind::Notifications
        ]
    );
    let read_only: Vec<bool> = list.conversations.iter().map(Conversation::is_read_only).collect();
    assert_eq!(read_only, [false, false, true, true]);
    assert_eq!(list.total_count(), Field::Known(4));
    assert!(
        list.sync_state()
            .is_some_and(|link| link.contains("syncState=fixture-token-1"))
    );
    assert_eq!(list.forward_link(), None, "an empty forwardLink is no link");
    assert_eq!(
        list.conversations[2].thread_property("spaceThreadTopic"),
        Some("A made-up team")
    );
    Ok(())
}

#[test]
fn a_conversation_entry_reads_its_horizon_and_last_message() -> TestResult {
    let list = conversation_list(&fixture("conversations.json")?["response"]["body"])?;
    let ab = &list.conversations[0];
    assert_eq!(ab.version, Field::Known(1_760_000_300_100));
    let horizon = ab.consumption_horizon().ok_or("no horizon")?;
    assert_eq!(horizon.last_read_arrival, Some(1_760_000_300_000));
    assert_eq!(horizon.updated, Some(1_760_000_300_100));
    assert_eq!(horizon.client_message_id.as_deref(), Some("3333333333333333333"));
    let newest = ab.last_message().ok_or("no last message")?;
    assert_eq!(newest.id, "1760000300000");
    assert_eq!(newest.sender(), Some(LAB_A));
    assert!(ab.extra.contains_key("targetLink"), "an unnamed field is kept");
    assert_eq!(ab.member_properties["role"], "Admin");

    let empty = &list.conversations[1];
    assert_eq!(empty.last_message(), None, "an empty chat's lastMessage is {{}}");
    assert_eq!(empty.consumption_horizon(), None);
    Ok(())
}

#[test]
fn a_conversations_roster_version_is_absent_known_or_kept_as_it_came() -> TestResult {
    let roster = |properties: Value| -> TestResult<Field<u64>> {
        let listed: Conversation = serde_json::from_value(json!({"id": ABC_CHAT, "threadProperties": properties}))?;
        Ok(listed.roster_version())
    };
    assert_eq!(roster(json!({}))?, Field::Absent);
    assert_eq!(
        roster(json!({"rosterVersion": "1760000000000"}))?,
        Field::Known(1_760_000_000_000)
    );
    assert_eq!(roster(json!({"rosterVersion": 7}))?, Field::Known(7));
    assert_eq!(roster(json!({"rosterVersion": "r1"}))?, Field::Unknown(json!("r1")));
    assert_eq!(roster(json!({"rosterVersion": null}))?, Field::Unknown(Value::Null));
    Ok(())
}

#[test]
fn a_conversations_thread_version_is_its_thread_properties_version() -> TestResult {
    let thread = |properties: Value| -> TestResult<Field<u64>> {
        let listed: Conversation = serde_json::from_value(json!({"id": ABC_CHAT, "threadProperties": properties}))?;
        Ok(listed.thread_version())
    };
    assert_eq!(thread(json!({}))?, Field::Absent);
    // a string of digits, as the list gives it (a lab account's debug log, 2026-11): the `version` of the thread's
    // `ThreadUpdate`
    assert_eq!(
        thread(json!({"version": "1793988484830", "rosterVersion": 1_793_955_884_788_u64}))?,
        Field::Known(1_793_988_484_830)
    );
    assert_eq!(thread(json!({"version": "v1"}))?, Field::Unknown(json!("v1")));
    Ok(())
}

#[test]
fn a_conversation_is_empty_only_as_the_service_says_it_in_its_captured_form() -> TestResult {
    let empty = |properties: Value| -> TestResult<Option<bool>> {
        let listed: Conversation = serde_json::from_value(json!({"id": AB_CHAT, "properties": properties}))?;
        Ok(listed.is_empty_conversation())
    };
    assert_eq!(empty(json!({"isemptyconversation": "True"}))?, Some(true));
    assert_eq!(empty(json!({"isemptyconversation": "False"}))?, Some(false));
    // absent, or a form no capture showed: not known
    for other in [
        json!({}),
        json!({"isemptyconversation": true}),
        json!({"isemptyconversation": "true"}),
    ] {
        assert_eq!(empty(other.clone())?, None, "{other}");
    }
    Ok(())
}

#[test]
fn a_conversation_kind_this_build_does_not_name_is_kept_and_read_only() {
    let kind = ConversationKind::of("19:meeting_abc@thread.v2", Some("chat"));
    assert_eq!(kind, ConversationKind::Unknown("chat".to_owned()));
    assert!(kind.is_read_only());
    assert_eq!(
        ConversationKind::of(ABC_CHAT, Some("meeting")),
        ConversationKind::Unknown("meeting".to_owned())
    );
    assert_eq!(
        ConversationKind::of(AB_CHAT, None),
        ConversationKind::Unknown(String::new())
    );
    assert_eq!(ConversationKind::of(ABC_CHAT, Some("chat")), ConversationKind::Group);
    assert!(!ConversationKind::Group.is_read_only());
}

#[test]
fn a_sync_page_reads_its_links_and_a_deleted_message() -> TestResult {
    let fixture = fixture("messages.json")?;
    let page: MessagePage = page_of(&fixture["sync"]["response"]["body"])?;
    assert!(page.unreadable.is_empty());
    assert_eq!(page.messages.len(), 3);
    assert!(
        page.sync_state()
            .is_some_and(|link| link.contains("syncState=fixture-token-2"))
    );
    assert_eq!(page.last_complete_segment_end(), Field::Known(1_760_000_068_000));
    assert_eq!(page.backward_link(), None);
    assert_eq!(page.extra["tenantId"], TENANT);

    let deleted = &page.messages[0];
    assert!(deleted.is_deleted());
    assert_eq!(deleted.delete_time(), Field::Known(1_760_000_068_000));
    assert_eq!(deleted.skype_edited_id.as_ref(), deleted.client_message_id.as_known());
    assert_eq!(deleted.version, Field::Known(1_760_000_068_000));
    assert_eq!(deleted.rich_text(), Some(RichText::default()));
    Ok(())
}

fn sync_page() -> TestResult<MessagePage> {
    page_of(&fixture("messages.json")?["sync"]["response"]["body"])
}

#[test]
fn a_quoted_reply_reads_its_quote_from_content_and_properties() -> TestResult {
    let page = sync_page()?;
    let reply = &page.messages[1];
    let quotes = reply.quotes().known().ok_or("qtdMsgs as read back")?;
    assert_eq!(quotes.len(), 1);
    assert_eq!(quotes[0].message_id, "1760000010000", "a number as read back");
    assert_eq!(quotes[0].sender.as_deref(), Some(LAB_A));
    assert_eq!(quotes[0].validation_result.as_deref(), Some("Valid"));
    let rich = reply.rich_text().ok_or("no rich text")?;
    let quote = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        sender_name: Some("Lab A".to_owned()),
        time: Some("1760000010000".to_owned()),
        body: vec![Node::Paragraph(vec![text("a made-up message, edited")])],
        forward: false,
    };
    // the line break between the quote and the reply is markup, not text (`chat-shapes`, 2026-10-21)
    assert_eq!(
        rich.nodes,
        [Node::Quote(quote), Node::Paragraph(vec![text("a made-up reply")])]
    );
    assert_eq!(reply.mentions(), Field::Known(Vec::new()));
    assert_eq!(reply.files(), SharedFiles::default());
    assert_eq!(reply.ams_references, Some(Vec::new()));
    Ok(())
}

#[test]
fn a_forwarded_message_reads_its_content_as_a_forward_and_where_it_came_from() -> TestResult {
    let fixture = fixture("forward.json")?;
    let note = message(&fixture["read_back"][0])?;
    let rich = note.rich_text().ok_or("no rich text")?;
    let link = "https://example.com/made-up-folder";
    let forward = Quote {
        body: vec![
            Node::Paragraph(vec![text("A made-up note about the new assets, see the folder below")]),
            Node::Paragraph(vec![Node::Link {
                href: link.to_owned(),
                children: vec![text(link)],
            }]),
        ],
        forward: true,
        ..Quote::default()
    };
    assert_eq!(rich.nodes, [Node::Quote(forward)], "the markup's CRLFs are no text");
    let from = note.forwarded().known().ok_or("originalMessageContext")?;
    assert_eq!(from.sender.as_deref(), Some(LAB_C));
    assert_eq!(
        from.message_id.as_deref(),
        Some("1760000040123"),
        "a number, read as its digits"
    );
    assert_eq!(from.thread_id.as_deref(), Some(ABC_CHAT));
    assert_eq!(from.sent_time_ms(), Some(1_767_225_640_123));
    assert_eq!(from.extra["threadType"], "chat");
    assert_eq!(note.unshown(), Vec::new(), "nothing of a forward goes unshown");

    // a plain quote and a reply's are no forward; a message never forwarded names no origin
    let plain = parse_html("<blockquote><p>q</p></blockquote>");
    assert!(matches!(&plain.nodes[..], [Node::Quote(quote)] if !quote.forward));
    let reply = &sync_page()?.messages[1];
    let replied = reply.rich_text().ok_or("no rich text")?;
    assert!(matches!(&replied.nodes[0], Node::Quote(quote) if !quote.forward));
    assert_eq!(reply.forwarded(), Field::Absent);
    Ok(())
}

#[test]
fn a_forwarded_picture_is_the_forwards_own_with_its_object_and_size() -> TestResult {
    let fixture = fixture("forward.json")?;
    let picture = message(&fixture["read_back"][1])?;
    let rich = picture.rich_text().ok_or("no rich text")?;
    let [Node::Quote(quote)] = &rich.nodes[..] else {
        return Err(format!("{rich:?}").into());
    };
    assert!(quote.forward && quote.message_id.is_none() && quote.sender.is_none());
    let [Node::Paragraph(first), Node::Paragraph(second)] = &quote.body[..] else {
        return Err(format!("{quote:?}").into());
    };
    let [Node::Image(image)] = &first[..] else {
        return Err(format!("{first:?}").into());
    };
    assert_eq!(
        image.item_id.as_deref(),
        Some("0-eus-d9-0000000000000000000000000000f0d0")
    );
    assert_eq!(image.size, Some((127, 250)));
    assert_eq!(second, &[text("A made-up remark on the picture above")]);
    Ok(())
}

#[test]
fn an_origin_in_a_form_no_capture_showed_is_kept_as_it_came() -> TestResult {
    let fixture = fixture("forward.json")?;
    let mut properties = fixture["read_back"][0]["properties"]
        .as_object()
        .ok_or("properties")?
        .clone();
    let as_text = Value::String(properties["originalMessageContext"].to_string());
    properties.insert("originalMessageContext".to_owned(), as_text.clone());
    assert_eq!(
        forwarded(&properties),
        Field::Unknown(as_text),
        "a JSON string: not seen"
    );
    for unread in [
        json!({"originalSender": 7, "messageId": 1}),
        json!({"originalSender": LAB_C, "messageId": null}),
        json!({"originalSender": LAB_C, "messageId": [1]}),
        json!({"originalSender": null, "messageId": 1}),
        json!({"originalSender": LAB_C, "originalSentTime": null}),
        json!({"originalSender": LAB_C, "originalThreadId": null}),
        json!({"originalSender": LAB_C, "originalThreadId": 19}),
    ] {
        properties.insert("originalMessageContext".to_owned(), unread.clone());
        assert_eq!(forwarded(&properties), Field::Unknown(unread));
    }
    properties.insert("originalMessageContext".to_owned(), json!({}));
    let read = forwarded(&properties).known().ok_or("an empty object is read")?;
    assert_eq!((read.sent_time_ms(), read.sender, read.message_id), (None, None, None));
    Ok(())
}

#[test]
fn an_edited_message_reads_its_edit_time_and_taken_back_reaction() -> TestResult {
    let page = sync_page()?;
    let edited = &page.messages[2];
    assert_eq!(edited.edit_time(), Field::Known(1_760_000_020_000));
    assert!(!edited.is_deleted());
    let reactions = edited.reactions().known().ok_or("emotions")?;
    assert_eq!(reactions.len(), 1);
    assert_eq!((reactions[0].key.as_str(), reactions[0].users.len()), ("like", 0));
    assert_eq!(edited.annotations_summary, Some(json!({})));
    assert_eq!(edited.message_type, MessageType::RichTextHtml);
    assert_eq!(edited.sequence_id, Field::Known(3));
    assert_eq!(edited.compose_time_ms(), Some(1_767_225_610_000));
    assert_eq!(edited.arrival_time_ms(), Some(1_767_225_610_000));
    assert!(edited.extra.contains_key("conversationLink"));
    assert_eq!(
        edited.properties["languageStamp"],
        "languages=en:100;length:30;&detector=Bling"
    );
    Ok(())
}

#[test]
fn the_page_around_a_time_reads_the_chats_first_event() -> TestResult {
    let fixture = fixture("messages.json")?;
    let page = page_of(&fixture["around"]["response"]["body"])?;
    assert_eq!(page.last_complete_segment_start(), Field::Known(0));
    assert_eq!(page.sync_state(), None);
    let event = &page.messages[1];
    assert_eq!(event.message_type, MessageType::AddMember);
    assert_eq!(event.client_message_id, Field::Absent);
    assert_eq!(event.sender(), Some(AB_CHAT), "a thread's event comes from the thread");
    assert_eq!(event.rich_text(), None);
    let activity = event.thread_activity().ok_or("no activity")?;
    assert_eq!(activity.kind, "addmember");
    assert_eq!(activity.initiator.as_deref(), Some(LAB_A));
    assert_eq!(activity.targets, [LAB_A, LAB_B]);
    assert_eq!(activity.roster_version, Field::Known(1_760_000_000_000));
    assert_eq!(activity.event_time, Field::Known(1_760_000_000_399));
    assert!(event.extra.contains_key("origincontextid"));
    Ok(())
}

#[test]
fn edits_deletes_and_reactions_read_back_as_their_fixtures_recorded() -> TestResult {
    let edited = message(&fixture("edit.json")?["read_back"])?;
    assert_eq!(edited.edit_time(), Field::Known(1_760_000_020_900));
    assert_eq!(edited.version, Field::Known(1_760_000_020_900));
    assert_eq!(edited.skype_edited_id, None);
    assert_eq!(
        edited.rich_text().map(|rich| rich.plain_text()),
        Some("a made-up message, edited\n".to_owned())
    );

    let deleted = message(&fixture("delete.json")?["read_back"])?;
    assert!(deleted.is_deleted());
    assert_eq!(deleted.content, "");
    assert_eq!(deleted.ams_references, None);

    let reaction = fixture("reaction.json")?;
    let kept = message(&reaction["read_back_kept"])?;
    let reactions = kept.reactions().known().ok_or("emotions")?;
    let user = &reactions[0].users[0];
    assert_eq!(
        (user.mri.as_str(), &user.time, user.value.as_known().map(String::as_str)),
        (LAB_A, &Field::Known(1_760_000_055_100), Some("1760000055000"))
    );
    assert_eq!(kept.annotations_summary, Some(json!({"emotions": {"like": 1}})));
    let taken_back = message(&reaction["read_back_taken_back"])?;
    assert_eq!(taken_back.reactions().known().map(|list| list[0].users.len()), Some(0));
    Ok(())
}

#[test]
fn a_mention_reads_in_the_captured_form_from_content_and_properties_as_sent_and_read_back() -> TestResult {
    let fixture = fixture("mention.json")?;
    // as sent the span is inside a `readonly` element; read back it is bare and the entry's itemid a number
    for form in [&fixture["request"]["body"], &fixture["read_back"]] {
        let read = message(form)?;
        let mentions = read.mentions().known().ok_or("mentions as a JSON string")?;
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].item_id, "0");
        assert_eq!(mentions[0].mri.as_deref(), Some(LAB_B));
        assert_eq!(mentions[0].kind, MentionKind::Person);
        assert_eq!(mentions[0].display_name.as_deref(), Some("Lab B"));
        assert_eq!(mentions[0].extra["@type"], "http://schema.skype.com/Mention");
        let rich = read.rich_text().ok_or("no rich text")?;
        assert_eq!(
            rich.nodes,
            [Node::Paragraph(vec![
                Node::Mention {
                    item_id: "0".to_owned(),
                    text: "Lab B".to_owned()
                },
                text("\u{a0}a made-up mention"),
            ])]
        );
    }
    Ok(())
}

#[test]
fn a_sticker_reads_as_one_with_its_label_as_sent_and_read_back() -> TestResult {
    let fixture = fixture("sticker.json")?;
    let label = "A made-up sticker says \"Hi there\"";
    // as sent its `img` is in a `readonly` element; read back it is bare, without the editor's attributes
    for form in [&fixture["request"]["body"], &fixture["read_back"]] {
        let rich = message(form)?.rich_text().ok_or("no rich text")?;
        let blank = || Node::Paragraph(vec![text("\u{a0}")]);
        let [first, Node::Sticker(sticker), last] = &rich.nodes[..] else {
            panic!("a sticker between two blank paragraphs: {rich:#?}");
        };
        assert_eq!((first, last), (&blank(), &blank()));
        assert_eq!(sticker.label.as_deref(), Some(label));
        assert_eq!(sticker.image.item_type.as_deref(), Some(STICKER_ITEMTYPE));
        assert_eq!(sticker.image.size, Some((180, 180)));
        assert!(sticker.image.src.contains("/stickerassets/madeup-250x250/"));
        assert_eq!(rich.plain_text().trim_matches(['\u{a0}', '\n']), label);
    }
    Ok(())
}

#[test]
fn a_sticker_elements_label_wins_over_its_pictures() {
    // a picture sent to the media service in the element: the element's label, not the picture's `alt`
    let ams = parse_html(
        "<readonly aria-label=\"A made-up caption\" itemtype=\"http://schema.skype.com/Sticker\" title=\"t\"><img \
         src=\"https://us-api.asm.skype.com/v1/objects/0-eus-d1-0000000000000000000000000000000a/views/imgo\" \
         itemtype=\"http://schema.skype.com/AMSImage\" width=\"240\" height=\"180\" alt=\"image\"></readonly>",
    );
    let [Node::Sticker(Sticker { image, label })] = &ams.nodes[..] else {
        panic!("a sticker: {ams:#?}");
    };
    assert_eq!(label.as_deref(), Some("A made-up caption"));
    assert_eq!((image.alt.as_deref(), image.size), (Some("image"), Some((240, 180))));
    // the title when there is no aria-label, the picture's alt when neither says anything
    let titled = parse_html(
        "<readonly title=\"Titled\" itemtype=\"http://schema.skype.com/Sticker\"><img \
         src=\"https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/a-250x250/A.gif\" alt=\"a\"></readonly>",
    );
    assert!(matches!(&titled.nodes[..], [Node::Sticker(Sticker { label: Some(label), .. })] if label == "Titled"));
    let bare = parse_html(
        "<readonly aria-label=\" \" itemtype=\"http://schema.skype.com/Sticker\"><img \
         src=\"https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/a-250x250/A.gif\" alt=\"Alt\"></readonly>",
    );
    assert!(matches!(&bare.nodes[..], [Node::Sticker(Sticker { label: Some(label), .. })] if label == "Alt"));
}

#[test]
fn a_sticker_element_of_no_picture_and_any_other_readonly_element_stay_as_they_were() {
    // an element of no picture keeps its text; a mention's element is unknown markup as before
    let empty = parse_html("<readonly itemtype=\"http://schema.skype.com/Sticker\">made-up words</readonly>");
    assert_eq!(empty.nodes, [text("made-up words")]);
    let other = parse_html(
        "<p><readonly itemtype=\"http://schema.skype.com/Mention\"><img \
         src=\"https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/a-250x250/A.gif\" alt=\"x\"></readonly></p>",
    );
    assert!(
        matches!(&other.nodes[..], [Node::Paragraph(children)] if matches!(&children[..], [Node::Image(_)])),
        "{other:#?}"
    );
    // an emoticon in the element is no sticker
    let emoticon = parse_html(
        "<readonly itemtype=\"http://schema.skype.com/Sticker\"><img itemtype=\"http://schema.skype.com/Emoji\" \
         src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/laugh/default/20_f.png\" \
         itemid=\"laugh\" alt=\"x\"></readonly>",
    );
    assert!(
        matches!(&emoticon.nodes[..], [Node::Image(image)] if image.is_emoji()),
        "{emoticon:#?}"
    );
}

/// Each reaction's key and how many users it lists, in the order read.
fn reaction_counts(read: &Message) -> TestResult<Vec<(String, usize)>> {
    let reactions = read.reactions().known().ok_or("emotions")?.clone();
    Ok(reactions
        .into_iter()
        .map(|reaction| (reaction.key, reaction.users.len()))
        .collect())
}

#[test]
fn every_quick_reaction_reads_back_side_by_side_and_taken_back_one_by_one() -> TestResult {
    let fixture = fixture("reactions.json")?;
    let offered: Vec<&str> = fixture["offered"]
        .as_array()
        .ok_or("offered")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(offered, ["like", "heart", "laugh", "surprised"]);
    let counts = |name: &str| -> TestResult<Vec<(String, usize)>> { reaction_counts(&message(&fixture[name])?) };
    let keyed = |users: [usize; 4]| -> Vec<(String, usize)> {
        offered.iter().map(|key| (*key).to_owned()).zip(users).collect()
    };
    assert_eq!(counts("read_back_all_set")?, keyed([1, 1, 1, 1]));
    assert_eq!(counts("read_back_first_taken_back")?, keyed([0, 1, 1, 1]));
    assert_eq!(counts("read_back_all_taken_back")?, keyed([0, 0, 0, 0]));
    let all = message(&fixture["read_back_all_set"])?;
    let reactions = all.reactions().known().ok_or("emotions")?.clone();
    for (reaction, key) in reactions.iter().zip(&offered) {
        assert_eq!(reaction.users[0].mri, LAB_A, "{key}");
        assert!(reaction.users[0].value.as_known().is_some(), "{key}");
    }
    assert_eq!(
        all.annotations_summary,
        Some(json!({"emotions": {"like": 1, "heart": 1, "laugh": 1, "surprised": 1}}))
    );
    assert_eq!(
        message(&fixture["read_back_all_taken_back"])?.annotations_summary,
        Some(json!({}))
    );
    Ok(())
}

/// The three paragraphs of `lines.json`'s message, the last holding `link` (the link as the editor made it, or its
/// plain text).
fn three_lines(link: Option<Node>) -> Vec<Node> {
    let url = "https://example.com/made-up/path";
    let third = match link {
        Some(link) => vec![text("a link "), link, text(" on the third line")],
        None => vec![text(&format!("a link {url} on the third line"))],
    };
    vec![
        Node::Paragraph(vec![text("a made-up first line")]),
        Node::Paragraph(vec![text("a second line")]),
        Node::Paragraph(third),
    ]
}

#[test]
fn lines_read_as_paragraphs_and_a_link_as_a_link_as_sent_and_read_back() -> TestResult {
    let fixture = fixture("lines.json")?;
    let url = "https://example.com/made-up/path";
    let link = Node::Link {
        href: url.to_owned(),
        children: vec![text(url)],
    };
    // read back, the paragraphs are joined by CRLF: no text between them
    for form in [&fixture["request"]["body"], &fixture["read_back"]] {
        let read = message(form)?;
        let rich = read.rich_text().ok_or("no rich text")?;
        assert_eq!(rich.nodes, three_lines(Some(link.clone())));
        assert_eq!(
            rich.plain_text(),
            format!("a made-up first line\na second line\na link {url} on the third line\n")
        );
        let links: Vec<Value> = serde_json::from_str(read.properties["links"].as_str().ok_or("links")?)?;
        assert_eq!(links[0]["url"], url);
        assert_eq!(links[0]["@type"], "http://schema.skype.com/HyperLink");
    }
    let unlinked = parse_html(fixture["unlinked_content"].as_str().ok_or("unlinked")?);
    assert_eq!(unlinked.nodes, three_lines(None));
    Ok(())
}

#[test]
fn markup_whitespace_beside_a_paragraph_or_quote_is_no_text_and_other_whitespace_stays() {
    let read = parse_html("\r\n<p>one</p>\r\n <p>two <b>bold</b>\u{a0}</p>\n<blockquote>q</blockquote>\t<p>three</p> ");
    let quote = Node::Quote(Quote {
        body: vec![text("q")],
        ..Quote::default()
    });
    assert_eq!(
        read.nodes,
        [
            Node::Paragraph(vec![text("one")]),
            Node::Paragraph(vec![
                text("two "),
                Node::Text {
                    text: "bold".to_owned(),
                    marks: Marks::BOLD
                },
                text("\u{a0}")
            ]),
            quote,
            Node::Paragraph(vec![text("three")]),
        ]
    );
    // a no-break space between paragraphs is text, not markup
    assert_eq!(
        parse_html("<p>a</p>\u{a0}<p>b</p>").nodes,
        [
            Node::Paragraph(vec![text("a")]),
            text("\u{a0}"),
            Node::Paragraph(vec![text("b")])
        ]
    );
}

#[test]
fn a_mention_in_the_group_chat_reads_as_in_the_one_to_one_chat_and_in_both_streams() -> TestResult {
    let fixture = fixture("mention-group.json")?;
    for form in ["request", "read_back", "notifications_entry", "mentions_entry"] {
        let read = match form {
            "request" => message(&fixture["request"]["body"])?,
            _ => message(&fixture[form])?,
        };
        let mentions = read.mentions().known().ok_or("mentions as a JSON string")?;
        assert_eq!(
            (
                mentions[0].item_id.as_str(),
                mentions[0].mri.as_deref(),
                &mentions[0].kind
            ),
            ("0", Some(LAB_B), &MentionKind::Person),
            "{form}"
        );
        let rich = read.rich_text().ok_or("no rich text")?;
        let mention = Node::Mention {
            item_id: "0".to_owned(),
            text: "Lab B".to_owned(),
        };
        let expected = match form {
            "request" | "read_back" => vec![Node::Paragraph(vec![mention, text("\u{a0}a made-up group mention")])],
            _ => vec![mention, text(" a made-up group mention")],
        };
        assert_eq!(rich.nodes, expected, "{form}");
    }
    let group = message(&fixture["read_back"])?;
    assert_eq!(group.extra["to"], ABC_CHAT);
    assert_eq!(
        fixture["notifications_entry"]["properties"]["activity"]["activityType"],
        "mentionInChat"
    );
    assert_eq!(
        fixture["mentions_entry"]["properties"]["activity"]["activityType"],
        "mentions"
    );
    Ok(())
}

#[test]
fn a_quote_as_sent_names_its_message_as_a_string() -> TestResult {
    let sent = message(&fixture("reply.json")?["request"]["body"])?;
    let quotes = sent.quotes().known().ok_or("qtdMsgs as a JSON string")?;
    assert_eq!(
        (quotes[0].message_id.as_str(), &quotes[0].time),
        ("1760000010000", &Field::Known(1_760_000_010_000))
    );
    Ok(())
}

#[test]
fn the_group_thread_read_and_member_list_read_their_members() -> TestResult {
    let thread: Thread = serde_json::from_value(fixture("thread-group.json")?["response"]["body"].clone())?;
    assert_eq!(thread.kind(), ConversationKind::Group);
    let members: Vec<&str> = thread.members.iter().map(|member| member.id.as_str()).collect();
    assert_eq!(members, [LAB_A, LAB_B, LAB_C]);
    assert_eq!(thread.members[0].role.as_deref(), Some("Admin"));
    assert_eq!(thread.roster_version, Field::Known(1_760_000_000_000));
    assert!(thread.extra.contains_key("eTag"));
    let list: MemberList = serde_json::from_value(fixture("thread-group-members.json")?["response"]["body"].clone())?;
    assert_eq!(
        (list.members.len(), &list.total_member_count, &list.roster_version),
        (3, &Field::Known(3), &Field::Known(0))
    );
    Ok(())
}

#[test]
fn what_this_build_does_not_know_is_kept_never_a_parse_failure() -> TestResult {
    let odd = message(&json!({
        "id": 17,
        "version": "x1",
        "messagetype": "Event/Call",
        "content": "<partlist/>",
        "newField": {"nested": true},
        "properties": {
            "mentions": "{\"not\": \"a list\"}",
            "emotions": "like",
            "edittime": null,
            "qtdMsgs": [{"sender": "no id"}],
            "files": 3
        }
    }))?;
    assert_eq!(odd.id, "17");
    assert_eq!(odd.version, Field::Unknown(json!("x1")), "kept, not dropped");
    assert_eq!(odd.message_type, MessageType::Unknown("Event/Call".to_owned()));
    assert_eq!(odd.message_type.as_str(), "Event/Call");
    assert_eq!(odd.rich_text(), None);
    assert_eq!(odd.thread_activity(), None);
    assert_eq!(odd.extra["newField"], json!({"nested": true}));
    assert_eq!(odd.mentions(), Field::Unknown(json!("{\"not\": \"a list\"}")));
    assert_eq!(odd.reactions(), Field::Unknown(json!("like")));
    assert_eq!(odd.edit_time(), Field::Unknown(Value::Null));
    assert_eq!(odd.quotes(), Field::Unknown(json!([{"sender": "no id"}])));
    assert_eq!(odd.files().unread, Some(json!(3)));
    Ok(())
}

#[test]
fn a_file_card_or_ams_object_no_capture_showed_is_kept_as_it_came_never_read_as_one() -> TestResult {
    // every captured message: the empty lists, as JSON strings, and no AMS object
    let reply = message(&fixture("reply.json")?["read_back"])?;
    assert_eq!(reply.files(), SharedFiles::default());
    assert_eq!(reply.unshown(), Vec::new());
    // an entry of a shape no message showed (no `@type`, a link off SharePoint): kept as it came, its card drawn by
    // its name alone, none of its links kept
    let entry = r#"[{"fileName":"made-up.txt","objectUrl":"https://example.test/x"}]"#;
    let odd = message(&json!({
        "id": "18",
        "version": "18",
        "messagetype": "RichText/Html",
        "content": "<p>a made-up caption</p>",
        "amsreferences": ["0-wus-d1-made-up"],
        "properties": {"files": entry, "cards": [{"made": "up"}], "links": "[]", "mentions": "[]"}
    }))?;
    let unread = json!([{"fileName": "made-up.txt", "objectUrl": "https://example.test/x"}]);
    let files = odd.files();
    let drawn: Vec<_> = files
        .files
        .iter()
        .map(|file| (file.name.as_str(), file.open_link()))
        .collect();
    assert_eq!(
        (drawn, files.unread),
        (vec![("made-up.txt", None)], Some(unread.clone()))
    );
    assert_eq!(
        odd.unshown(),
        [
            Unshown::Files(unread),
            Unshown::Cards(json!([{"made": "up"}])),
            Unshown::AmsReferences(json!(["0-wus-d1-made-up"])),
        ]
    );
    let names: Vec<&str> = odd.unshown().iter().map(Unshown::name).collect();
    assert_eq!(names, ["files", "cards", "amsreferences"]);
    // a form that isn't a list is unshown too; an empty list in either form, or none, is nothing to show
    let parts =
        |properties: Value| -> TestResult<Vec<Unshown>> { Ok(unshown_parts(&serde_json::from_value(properties)?)) };
    assert_eq!(parts(json!({"files": 3}))?, [Unshown::Files(json!(3))]);
    assert_eq!(
        parts(json!({"files": "not json"}))?,
        [Unshown::Files(json!("not json"))]
    );
    assert_eq!(parts(json!({"files": null}))?, [Unshown::Files(Value::Null)]);
    assert_eq!(parts(json!({"files": [], "cards": "[]"}))?, []);
    assert_eq!(parts(json!({}))?, []);
    // the rest of the message reads as ever
    assert!(odd.rich_text().is_some());
    // where it is read in full: the web client, at the chat, its id one encoded segment
    assert_eq!(
        web_chat_link("19:a_b@unq.gbl.spaces"),
        "https://teams.microsoft.com/l/chat/19%3Aa_b%40unq.gbl.spaces/0"
    );
    // as `abc-chat` encodes it: a 1:1 chat's object ids keep their `-`
    assert_eq!(
        web_chat_link("19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces"),
        "https://teams.microsoft.com/l/chat/19%3A00000000-0000-4000-8000-00000000000a_\
         00000000-0000-4000-8000-00000000000b%40unq.gbl.spaces/0"
    );
    assert!(web_chat_link("19:x/../y?z#w").starts_with(&format!("{WEB_CLIENT}/l/chat/19%3Ax%2F..%2Fy%3Fz%23w/")));
    Ok(())
}

#[test]
fn a_messages_named_numbers_and_ids_are_absent_known_or_kept_as_they_came() -> TestResult {
    let absent = message(&json!({"id": "1"}))?;
    assert_eq!(
        (&absent.sequence_id, &absent.version, &absent.client_message_id),
        (&Field::Absent, &Field::Absent, &Field::Absent)
    );
    let known = message(&json!({"id": "1", "sequenceId": 4, "version": "1760000000000", "clientmessageid": 42}))?;
    assert_eq!(
        (&known.sequence_id, &known.version, &known.client_message_id),
        (
            &Field::Known(4),
            &Field::Known(1_760_000_000_000),
            &Field::Known("42".to_owned())
        )
    );
    let odd = json!({"id": "1", "sequenceId": -1, "version": 1.5, "clientmessageid": {}});
    let unknown = message(&odd)?;
    assert_eq!(
        (&unknown.sequence_id, &unknown.version, &unknown.client_message_id),
        (
            &Field::Unknown(json!(-1)),
            &Field::Unknown(json!(1.5)),
            &Field::Unknown(json!({}))
        )
    );
    let nulls = message(&json!({"id": "1", "version": null, "clientmessageid": null}))?;
    assert_eq!(
        (&nulls.version, &nulls.client_message_id),
        (&Field::Unknown(Value::Null), &Field::Unknown(Value::Null)),
        "null is no form a capture showed"
    );

    let page = page_of(&json!({"messages": [odd]}))?;
    assert_eq!(
        (page.messages.len(), page.unreadable.len()),
        (1, 0),
        "read, not unreadable"
    );
    assert_eq!(page.messages[0].version, Field::Unknown(json!(1.5)));

    let back = serde_json::to_value(&unknown)?;
    assert_eq!(
        (&back["sequenceId"], &back["version"], &back["clientmessageid"]),
        (&json!(-1), &json!(1.5), &json!({})),
        "serialized as it came"
    );
    let back = serde_json::to_value(&absent)?;
    assert!(back.get("version").is_none() && back.get("clientmessageid").is_none());
    Ok(())
}

#[test]
fn a_conversations_named_numbers_are_absent_known_or_kept_as_they_came() -> TestResult {
    let list = conversation_list(&json!({"conversations": [
        {"id": AB_CHAT},
        {"id": AB_CHAT, "version": 7, "lastUpdatedMessageId": "1760000000000", "lastUpdatedMessageVersion": 0},
        {"id": AB_CHAT, "version": null, "lastUpdatedMessageId": "17e3", "lastUpdatedMessageVersion": [1]}
    ]}))?;
    assert!(list.unreadable.is_empty());
    let fields = |conversation: &Conversation| {
        (
            conversation.version.clone(),
            conversation.last_updated_message_id.clone(),
            conversation.last_updated_message_version.clone(),
        )
    };
    assert_eq!(
        fields(&list.conversations[0]),
        (Field::Absent, Field::Absent, Field::Absent)
    );
    assert_eq!(
        fields(&list.conversations[1]),
        (Field::Known(7), Field::Known(1_760_000_000_000), Field::Known(0))
    );
    assert_eq!(
        fields(&list.conversations[2]),
        (
            Field::Unknown(Value::Null),
            Field::Unknown(json!("17e3")),
            Field::Unknown(json!([1]))
        )
    );
    Ok(())
}

#[test]
fn reaction_users_and_quotes_keep_a_time_or_value_of_another_form() -> TestResult {
    let read = message(&json!({"id": "1", "properties": {
        "emotions": [{"key": "like", "users": [
            {"mri": "8:orgid:x"},
            {"mri": "8:orgid:y", "time": "5", "value": 6},
            {"mri": "8:orgid:z", "time": "soon", "value": true}
        ]}],
        "qtdMsgs": [{"messageId": "1"}, {"messageId": "2", "time": 3}, {"messageId": "3", "time": -3}]
    }}))?;
    let reactions = read.reactions().known().ok_or("emotions")?;
    let users: Vec<_> = reactions[0]
        .users
        .iter()
        .map(|user| (user.time.clone(), user.value.clone()))
        .collect();
    assert_eq!(
        users,
        [
            (Field::Absent, Field::Absent),
            (Field::Known(5), Field::Known("6".to_owned())),
            (Field::Unknown(json!("soon")), Field::Unknown(json!(true)))
        ]
    );
    let quotes = read.quotes().known().ok_or("qtdMsgs")?;
    let times: Vec<_> = quotes.iter().map(|quote| quote.time.clone()).collect();
    assert_eq!(times, [Field::Absent, Field::Known(3), Field::Unknown(json!(-3))]);
    Ok(())
}

#[test]
fn a_chats_topic_is_its_name_and_a_blank_one_none() -> TestResult {
    let listed = |topic: Value| -> serde_json::Result<Conversation> {
        serde_json::from_value(json!({"id": ABC_CHAT, "threadProperties": {"threadType": "chat", "topic": topic}}))
    };
    assert_eq!(listed(json!("Synthetic group"))?.topic(), Some("Synthetic group"));
    assert_eq!(listed(json!(" "))?.topic(), None);
    assert_eq!(listed(json!(7))?.topic(), None, "a topic that isn't text names nothing");
    let thread: Thread = serde_json::from_value(json!({"id": ABC_CHAT, "properties": {"topic": "Synthetic group"}}))?;
    assert_eq!(thread.topic(), Some("Synthetic group"));
    let bare: Thread = serde_json::from_value(json!({"id": ABC_CHAT}))?;
    assert_eq!(bare.topic(), None);
    Ok(())
}

#[test]
fn a_threads_and_member_lists_named_numbers_are_absent_known_or_kept_as_they_came() -> TestResult {
    let absent: Thread = serde_json::from_value(json!({"id": ABC_CHAT}))?;
    assert_eq!(
        (&absent.roster_version, &absent.version),
        (&Field::Absent, &Field::Absent)
    );
    let read: Thread = serde_json::from_value(json!({"id": ABC_CHAT, "rosterVersion": "r1", "version": "7"}))?;
    assert_eq!(
        (&read.roster_version, &read.version),
        (&Field::Unknown(json!("r1")), &Field::Known(7))
    );
    let list: MemberList = serde_json::from_value(json!({"totalMemberCount": [3], "rosterVersion": 2}))?;
    assert_eq!(
        (&list.total_member_count, &list.roster_version),
        (&Field::Unknown(json!([3])), &Field::Known(2))
    );
    let empty: MemberList = serde_json::from_value(json!({}))?;
    assert_eq!(
        (&empty.total_member_count, &empty.roster_version),
        (&Field::Absent, &Field::Absent)
    );
    Ok(())
}

#[test]
fn a_thread_events_numbers_and_a_messages_edit_and_delete_times_keep_another_form() -> TestResult {
    let event = message(&json!({
        "id": "1",
        "messagetype": "ThreadActivity/AddMember",
        "content": "<addmember><eventtime>soon</eventtime><rosterVersion>+5</rosterVersion></addmember>"
    }))?;
    let activity = event.thread_activity().ok_or("no activity")?;
    assert_eq!(
        (&activity.event_time, &activity.roster_version),
        (&Field::Unknown(json!("soon")), &Field::Unknown(json!("+5")))
    );
    let bare = message(&json!({"id": "1", "messagetype": "ThreadActivity/AddMember", "content": "<addmember/>"}))?;
    let activity = bare.thread_activity().ok_or("no activity")?;
    assert_eq!(
        (&activity.event_time, &activity.roster_version),
        (&Field::Absent, &Field::Absent)
    );

    let odd = message(&json!({"id": "1", "properties": {"deletetime": "yesterday", "edittime": "5"}}))?;
    assert_eq!(odd.delete_time(), Field::Unknown(json!("yesterday")));
    assert!(
        !odd.is_deleted(),
        "a deletetime no capture showed isn't read as a delete"
    );
    assert_eq!(odd.edit_time(), Field::Known(5));
    let plain = message(&json!({"id": "1"}))?;
    assert_eq!((plain.edit_time(), plain.delete_time()), (Field::Absent, Field::Absent));
    Ok(())
}

#[test]
fn a_mention_kind_or_message_type_this_build_does_not_name_is_kept() -> TestResult {
    let unknown_kind = message(&json!({
        "id": "1",
        "properties": {"mentions": [{"itemid": 0, "mri": "8:orgid:x", "mentionType": "nearby"}]}
    }))?;
    assert_eq!(unknown_kind.message_type, MessageType::Unknown(String::new()));
    let mentions = unknown_kind.mentions().known().ok_or("a list of mentions")?;
    assert_eq!(mentions[0].kind, MentionKind::Unknown("nearby".to_owned()));
    assert_eq!(mentions[0].item_id, "0");
    Ok(())
}

#[test]
fn a_segment_time_in_a_form_no_capture_showed_is_unknown() -> TestResult {
    let page = page_of(&json!({"messages": [], "_metadata": {"lastCompleteSegmentEndTime": null}}))?;
    assert_eq!(page.last_complete_segment_end(), Field::Unknown(Value::Null));
    assert_eq!(page.last_complete_segment_start(), Field::Absent);
    Ok(())
}

// ---- rich text ----

#[test]
fn html_preview_omits_quotes_while_plain_text_keeps_them() {
    for quote in [
        "<blockquote><p>Earlier text</p></blockquote>",
        concat!(
            "<blockquote itemtype=\"http://schema.skype.com/Reply\" itemid=\"42\">",
            "<p>This message was blocked due to organization policy.</p></blockquote>"
        ),
    ] {
        let rich = parse_html(&format!("{quote}<p>My <b>reply</b> &amp; more</p><p>Second line</p>"));
        assert_eq!(rich.preview_text(), "My reply & more\nSecond line\n");
        assert_eq!(
            rich.plain_text(),
            format!("{}My reply & more\nSecond line\n", parse_html(quote).plain_text())
        );
        assert!(parse_html(quote).preview_text().is_empty());
    }
}

#[test]
fn html_preview_omits_nested_quotes_and_keeps_forwarded_content_and_formatting() {
    let rich = parse_html(concat!(
        "<div><blockquote>Quoted</blockquote><h2>Heading</h2></div>",
        "<ul><li><blockquote>Also quoted</blockquote><span>Item</span></li></ul>",
        "<blockquote itemtype=\"http://schema.skype.com/Forward\">",
        "<blockquote>Original quote</blockquote><p>Forwarded text</p></blockquote>",
        "<pre>Code</pre><hr><p><a href=\"https://example.test\">Link</a><br>",
        "<img src=\"https://example.test/a.png\" alt=\"Picture\"></p>"
    ));
    assert_eq!(
        rich.preview_text(),
        "Heading\n\nItem\nForwarded text\nCode\nLink\nPicture\n"
    );
    for quoted in ["Quoted", "Also quoted", "Original quote"] {
        assert!(rich.plain_text().contains(quoted));
    }
}

#[test]
fn no_script_style_or_event_attribute_survives() {
    let rich = parse_html(concat!(
        "<p onclick=\"alert(1)\" style=\"color:red\">hi<script>alert(2)</script><style>p{}</style></p>",
        "<a href=\"javascript:alert(3)\" onmouseover=\"alert(4)\">click</a>",
        "<img src=\"data:image/png;base64,AAAA\" alt=\"inline\" onerror=\"alert(5)\">",
        "<img src=\"javascript:alert(6)\">",
        "<iframe src=\"https://example.test/\">framed</iframe><template><b>inert</b></template>",
    ));
    assert_eq!(rich.nodes, [Node::Paragraph(vec![text("hi")]), text("clickinline")]);
    let debug = format!("{rich:?}");
    for active in ["alert", "onclick", "color", "javascript", "framed", "inert", "data:"] {
        assert!(!debug.contains(active), "{active} survived: {debug}");
    }
}

#[test]
fn unknown_markup_is_kept_as_text_and_known_markup_typed() {
    let rich = parse_html(concat!(
        "<div><marquee>moving <b>bold <i>both</i></b></marquee><br><code>x</code></div>",
        "<a href=\"https://example.test/a\"><u>link</u></a>",
        "<img src=\"https://example.test/i.png\" alt=\"pic\" itemtype=\"http://schema.skype.com/AMSImage\">",
        "<blockquote><p>plain quote</p></blockquote>",
        "<!-- a comment -->",
    ));
    let bold_italic = Marks::BOLD.with(Marks::ITALIC);
    assert!(bold_italic.contains(Marks::ITALIC) && !bold_italic.contains(Marks::CODE));
    assert_eq!(
        rich.nodes,
        [
            Node::Paragraph(vec![
                text("moving "),
                Node::Text {
                    text: "bold ".to_owned(),
                    marks: Marks::BOLD
                },
                Node::Text {
                    text: "both".to_owned(),
                    marks: bold_italic
                },
                Node::Break,
                Node::Text {
                    text: "x".to_owned(),
                    marks: Marks::CODE
                },
            ]),
            Node::Link {
                href: "https://example.test/a".to_owned(),
                children: vec![Node::Text {
                    text: "link".to_owned(),
                    marks: Marks::UNDERLINE
                }],
            },
            Node::Image(teams_proto::chat::Image {
                src: "https://example.test/i.png".to_owned(),
                alt: Some("pic".to_owned()),
                item_type: Some("http://schema.skype.com/AMSImage".to_owned()),
                item_id: None,
                size: None,
            }),
            Node::Quote(Quote {
                body: vec![Node::Paragraph(vec![text("plain quote")])],
                ..Quote::default()
            }),
        ]
    );
    assert_eq!(rich.plain_text(), "moving bold both\nx\nlinkpic\nplain quote\n");
}

/// A pasted picture and an emoticon in the shapes the captures and the user's chats showed (made-up ids): the picture
/// with its `itemid` and its size from its attributes (a fraction rounded) or its `style`, the emoticon with its id.
#[test]
fn a_pasted_picture_and_an_emoticon_keep_their_ids_and_size() {
    let object = "0-eus-d9-00000000000000000000000000000abc";
    let rich = parse_html(&format!(
        "<p><img src=\"https://us-api.asm.skype.com/v1/objects/{object}/views/imgo\" \
         itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" width=\"884\" height=\"130.64889918887602\" \
         alt=\"image\" id=\"x_{object}\" itemid=\"{object}\"></p>\
         <p><img src=\"https://us-api.asm.skype.com/v1/objects/{object}/views/imgo\" itemscope=\"\" \
         itemtype=\"http://schema.skype.com/AMSImage\" style=\"width:2082px; height:2776px\"></p>\
         <p>done<span title=\"Speechless\" type=\"(speechless)\" class=\"animated-emoticon-20-speechless\" \
         itemscope=\"\"><img itemscope=\"\" itemtype=\"http://schema.skype.com/Emoji\" itemid=\"speechless\" \
         src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/\
         speechless/default/20_f.png\" title=\"Speechless\" alt=\"\u{1f610}\" style=\"width:20px; height:20px\">\
         </span></p>"
    ));
    let images: Vec<&teams_proto::chat::Image> = rich
        .nodes
        .iter()
        .flat_map(|node| match node {
            Node::Paragraph(children) => children.iter().collect(),
            other => vec![other],
        })
        .filter_map(|node| match node {
            Node::Image(image) => Some(image),
            _ => None,
        })
        .collect();
    assert_eq!(images.len(), 3, "{rich:?}");
    assert_eq!(images[0].item_id.as_deref(), Some(object));
    assert_eq!(images[0].size, Some((884, 131)));
    assert!(!images[0].is_emoji());
    assert_eq!(
        (images[1].item_id.as_deref(), images[1].size),
        (None, Some((2082, 2776)))
    );
    assert!(images[2].is_emoji());
    assert_eq!(images[2].item_id.as_deref(), Some("speechless"));
    assert_eq!(images[2].alt.as_deref(), Some("\u{1f610}"));
    assert_eq!(images[2].size, Some((20, 20)));
    assert_eq!(rich.plain_text(), "image\n\ndone\u{1f610}\n");

    // a size that isn't plain pixels, or only half of one, is none
    for img in [
        "<img src=\"https://example.test/i.png\" width=\"50%\" height=\"10\">",
        "<img src=\"https://example.test/i.png\" width=\"10\">",
        "<img src=\"https://example.test/i.png\" width=\"0\" height=\"10\">",
        "<img src=\"https://example.test/i.png\" style=\"width:10em; height:10px\">",
        "<img src=\"https://example.test/i.png\" width=\"-4\" height=\"10\">",
        "<img src=\"https://example.test/i.png\" width=\"9999999\" height=\"10\">",
    ] {
        let nodes = parse_html(img).nodes;
        assert!(
            matches!(&nodes[..], [Node::Image(image)] if image.size.is_none()),
            "{img}: {nodes:?}"
        );
    }
}

#[test]
fn deep_nesting_is_bounded_and_keeps_its_text() {
    // `div`s and `blockquote`s nest as written (a nested `a` or `p` would close the one before it)
    let depth = 1_000;
    let content = format!(
        "{}deep{}",
        "<div><blockquote><span>".repeat(depth),
        "</span></blockquote></div>".repeat(depth)
    );
    let rich = parse_html(&content);
    assert!(rich.plain_text().contains("deep"));
    let mut deepest = 0;
    let mut level: Vec<(&Node, usize)> = rich.nodes.iter().map(|node| (node, 1)).collect();
    while let Some((node, at)) = level.pop() {
        deepest = deepest.max(at);
        let children = match node {
            Node::Paragraph(children) | Node::Link { children, .. } => children.as_slice(),
            Node::Quote(quote) => quote.body.as_slice(),
            _ => &[],
        };
        level.extend(children.iter().map(|child| (child, at + 1)));
    }
    assert!(deepest <= MAX_DEPTH + 1, "nested {deepest} deep");
}

#[test]
fn content_over_the_cap_is_read_up_to_it() {
    // two bytes a character, so the cap falls inside one unless the cut moves to a boundary
    let long = format!("<p>a{}</p>", "é".repeat(MAX_CONTENT / 2));
    let rich = parse_html(&long);
    assert!(rich.truncated);
    let shown = rich.plain_text();
    assert!(shown.starts_with("aé") && shown.len() < MAX_CONTENT, "{}", shown.len());
    assert!(!parse_html("<p>short</p>").truncated);
}

#[test]
fn a_mention_elsewhere_than_a_span_is_text() {
    let rich = parse_html("<b itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab B</b>");
    assert_eq!(
        rich.nodes,
        [Node::Text {
            text: "Lab B".to_owned(),
            marks: Marks::BOLD
        }]
    );
}

// ---- reads through the native transport, against the mock ----

/// The account's token as the transport asks for it.
struct MockToken(String);

impl Credentials for MockToken {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

fn client_for(origin: &str, token: &str) -> TestResult<Client<HttpTransport<MockToken>>> {
    let transport = HttpTransport::with_mock_origin(Arc::new(MockToken(token.to_owned())), CHAT_BASE, origin)?;
    Ok(Client::new(transport))
}

fn lab_client(mock: &ChatServiceMock, who: &str) -> TestResult<Client<HttpTransport<MockToken>>> {
    client_for(&mock.origin(), &mock.token(who).ok_or("no such account")?)
}

#[tokio::test]
async fn the_conversation_list_and_its_sync_link_through_the_transport() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_A)?;
    let reads = ChatReads::new(&client);
    let list = reads.conversations().await?;
    let kind = |id: &str| {
        list.conversations
            .iter()
            .find(|conversation| conversation.id == id)
            .map(Conversation::kind)
    };
    assert_eq!(list.conversations.len(), 6);
    assert_eq!(kind(AB_CHAT), Some(ConversationKind::OneToOne));
    assert_eq!(kind(ABC_CHAT), Some(ConversationKind::Group));
    assert_eq!(kind(CHANNEL), Some(ConversationKind::Channel));
    assert_eq!(kind(NOTIFICATIONS), Some(ConversationKind::Notifications));
    let sync = list.sync_state().ok_or("no sync link")?;
    let since = reads.conversations_from(sync).await?;
    assert!(since.conversations.is_empty(), "nothing changed since");
    assert!(since.sync_state().is_some());
    Ok(())
}

#[tokio::test]
async fn message_pages_follow_the_services_backward_and_sync_links() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_B)?;
    let reads = ChatReads::new(&client);

    let first = reads.messages(AB_CHAT, 2).await?;
    let mut ids: Vec<String> = first.messages.iter().map(|message| message.id.clone()).collect();
    let mut backward = first.backward_link().map(str::to_owned);
    while let Some(link) = backward {
        let page = reads.messages_from(AB_CHAT, &link).await?;
        ids.extend(page.messages.iter().map(|message| message.id.clone()));
        backward = page.backward_link().map(str::to_owned);
    }
    assert_eq!(
        ids,
        [
            "1760000060000",
            "1760000050000",
            "1760000010000",
            "1760000005000",
            "1760000000400"
        ],
        "newest first, every message once"
    );

    let whole = reads.messages(AB_CHAT, 200).await?;
    assert_eq!(whole.messages.len(), 5);
    assert!(whole.unreadable.is_empty());
    assert!(whole.messages[0].is_deleted());
    assert_eq!(whole.messages[2].edit_time(), Field::Known(1_760_000_020_000));
    assert_eq!(whole.messages[4].message_type, MessageType::AddMember);
    let sync = whole.sync_state().ok_or("no sync link")?;
    let since = reads.messages_from(AB_CHAT, sync).await?;
    assert!(since.messages.is_empty(), "nothing changed since");

    let around = reads.messages_around(AB_CHAT, 1_760_000_000_000, 40).await?;
    assert_eq!(around.messages.len(), 5);
    assert_eq!(around.sync_state(), None);
    Ok(())
}

#[tokio::test]
async fn threads_and_member_lists_through_the_transport() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_A)?;
    let reads = ChatReads::new(&client);
    let one_to_one = reads.thread(AB_CHAT).await?;
    let members: Vec<&str> = one_to_one.members.iter().map(|member| member.id.as_str()).collect();
    assert_eq!(members, [LAB_A, LAB_B]);
    let group = reads.thread(ABC_CHAT).await?;
    assert_eq!(group.kind(), ConversationKind::Group);
    assert_eq!(group.members.len(), 3);
    let list = reads.members(ABC_CHAT).await?;
    assert_eq!((list.members.len(), &list.total_member_count), (3, &Field::Known(3)));
    // a chat the caller isn't in is the service's refusal, as the transport reports it
    let outside = lab_client(&mock, LAB_C)?;
    let refused = ChatReads::new(&outside).thread(AB_CHAT).await;
    assert!(
        matches!(&refused, Err(ChatReadError::Http(HttpError::Status(answer))) if answer.status() == 403),
        "{refused:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_link_off_the_chat_base_or_of_another_read_is_never_followed() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_A)?;
    let reads = ChatReads::new(&client);
    let messages = format!("v1/users/ME/conversations/{AB_CHAT}/messages?syncState=mock-since-0");
    let foreign = [
        format!("https://elsewhere.example.test/api/chatsvc/amer/{messages}"),
        format!("https://teams.microsoft.com/api/chatsvc/emea/{messages}"),
        format!("https://amer.ng.msg.teams.microsoft.com/{messages}"),
        format!("http://teams.microsoft.com/api/chatsvc/amer/{messages}"),
        format!("https://teams.microsoft.com:8443/api/chatsvc/amer/{messages}"),
        format!("https://user@teams.microsoft.com/api/chatsvc/amer/{messages}"),
        format!("https://teams.microsoft.com/api/chatsvc/amer/{messages}#fragment"),
        format!("https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/{ABC_CHAT}/messages"),
        format!("https://teams.microsoft.com/api/chatsvc/amer/v1/threads/{AB_CHAT}"),
        format!("{}/api/chatsvc/amer/{messages}", mock.origin()),
        "not a url".to_owned(),
    ];
    for link in &foreign {
        let refused = reads.messages_from(AB_CHAT, link).await;
        assert!(
            matches!(refused, Err(ChatReadError::Link { .. })),
            "{link}: {refused:?}"
        );
    }
    let list_link = format!("https://teams.microsoft.com/api/chatsvc/amer/{messages}");
    assert!(matches!(
        reads.conversations_from(&list_link).await,
        Err(ChatReadError::Link { .. })
    ));
    assert!(mock.received_requests().await.is_empty(), "nothing was sent");
    // the same link on the chat base is followed
    assert!(reads.messages_from(AB_CHAT, &list_link).await.is_ok());
    assert_eq!(mock.received_requests().await.len(), 1);
    Ok(())
}

#[test]
fn a_link_on_the_chat_base_becomes_its_endpoint_with_its_query() -> TestResult {
    let endpoint = link_endpoint(
        CHAT_BASE,
        "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19%3Ax%40thread.v2/messages?view=a|b&syncState=t%2Bu&pageSize=200",
    )
    .ok_or("refused")?;
    assert_eq!(
        endpoint.segments(),
        ["v1", "users", "ME", "conversations", "19:x@thread.v2", "messages"]
    );
    let query: Vec<(&str, &str)> = endpoint
        .query()
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    assert_eq!(query, [("view", "a|b"), ("syncState", "t+u"), ("pageSize", "200")]);
    let regional = "https://amer.ng.msg.teams.microsoft.com/";
    assert!(link_endpoint(regional, "https://amer.ng.msg.teams.microsoft.com/v1/threads/x").is_some());
    assert!(link_endpoint(regional, "https://emea.ng.msg.teams.microsoft.com/v1/threads/x").is_none());
    assert!(link_endpoint(CHAT_BASE, "https://teams.microsoft.com/api/chatsvc/amer/v1x/threads").is_none());
    Ok(())
}

#[tokio::test]
async fn a_404_with_a_location_is_a_region_hop_not_followed() -> TestResult {
    let server = MockServer::start().await;
    let hop = format!("{}/api/chatsvc/emea/v1/users/ME/conversations", server.uri());
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).insert_header("Location", hop.as_str()))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "placeholder-token")?;
    let reads = ChatReads::new(&client);
    let moved = reads.conversations().await;
    assert!(
        matches!(moved, Err(ChatReadError::RegionMoved { status: 404 })),
        "{moved:?}"
    );
    let error = moved.err().ok_or("no error")?;
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains("emea") && !shown.contains("127.0.0.1"), "{shown}");
    }
    assert_eq!(server.received_requests().await.map(|requests| requests.len()), Some(1));
    Ok(())
}

#[test]
fn the_accounts_mark_all_read_horizon_reads_as_the_web_client_reads_it() {
    let read = |properties: Value| AppConsumptionHorizon::from_properties(&properties);
    let both = Some(AppConsumptionHorizon { chat: 5, channel: 7 });
    assert_eq!(read(json!({"appConsumptionHorizon": {"chat": 5, "channel": 7}})), both);
    assert_eq!(
        read(json!({"appConsumptionHorizon": "{\"chat\":5,\"channel\":7,\"engage_community\":0}", "locale": "en"})),
        both,
        "as a property's JSON text"
    );
    assert_eq!(
        read(json!({"appConsumptionHorizon": {"chat": 5}})),
        Some(AppConsumptionHorizon { chat: 5, channel: 0 })
    );
    for not_one in [
        json!({"recentReactions": "[]"}),
        json!({"appConsumptionHorizon": {}}),
        json!({"appConsumptionHorizon": {"engage_community": 5}}),
        json!({"appConsumptionHorizon": {"chat": -1}}),
        json!({"appConsumptionHorizon": {"chat": "5"}}),
        json!({"appConsumptionHorizon": "not json"}),
        json!({"appConsumptionHorizon": 5}),
        json!([]),
    ] {
        assert_eq!(read(not_one.clone()), None, "{not_one}");
    }
}

#[tokio::test]
async fn the_accounts_properties_read_gives_its_horizon_and_none_is_no_error() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex("/v1/users/ME/properties$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"appConsumptionHorizon": "{\"chat\":5}"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "placeholder-token")?;
    let reads = ChatReads::new(&client);
    assert_eq!(
        reads.app_consumption_horizon().await?,
        Some(AppConsumptionHorizon { chat: 5, channel: 0 })
    );
    assert_eq!(reads.app_consumption_horizon().await?, None, "an answer it can't read");
    assert_eq!(
        reads.app_consumption_horizon().await?,
        None,
        "an account without properties"
    );
    assert!(reads.app_consumption_horizon().await.is_err(), "a refusal");
    Ok(())
}

#[tokio::test]
async fn a_404_without_a_location_and_a_malformed_answer_are_errors() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "placeholder-token")?;
    let reads = ChatReads::new(&client);
    let missing = reads.thread(AB_CHAT).await;
    assert!(
        matches!(&missing, Err(ChatReadError::Http(HttpError::Status(answer))) if answer.status() == 404),
        "{missing:?}"
    );
    assert!(matches!(reads.conversations().await, Err(ChatReadError::Malformed(_))));
    assert!(matches!(
        reads.messages(AB_CHAT, 10).await,
        Err(ChatReadError::Malformed(_))
    ));
    Ok(())
}

#[tokio::test]
async fn an_entry_that_is_not_a_message_is_kept_apart_and_the_page_read() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "1", "messagetype": "Text", "content": "kept"}, {"no": "id"}, {"id": "2", "content": 5}],
            "_metadata": {"syncState": ""}
        })))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "placeholder-token")?;
    let page = ChatReads::new(&client).messages(AB_CHAT, 10).await?;
    assert_eq!(page.messages.len(), 1);
    assert_eq!(
        page.messages[0].rich_text().map(|rich| rich.plain_text()),
        Some("kept".to_owned())
    );
    assert_eq!(page.unreadable, [json!({"no": "id"}), json!({"id": "2", "content": 5})]);
    assert_eq!(page.sync_state(), None, "an empty link is no link");
    Ok(())
}

/// `files.json`'s message (synthetic, in the shape of a received message with two files).
fn files_message() -> TestResult<Message> {
    message(&fixture("files.json")?["read_back"])
}

fn text_node(text: &str) -> Node {
    Node::Text {
        text: text.to_owned(),
        marks: Marks::NONE,
    }
}

#[test]
fn a_messages_lines_broken_by_br_and_a_line_feed_read_as_a_browser_sets_them() -> TestResult {
    // each `<br>` followed by a line feed: the line feed is markup whitespace, never a second line break
    let text = files_message()?.rich_text().ok_or("no rich text")?;
    assert_eq!(
        text.plain_text(),
        "A made-up test: three cameras on one rehearsal, each from its own spot\n\nThe cut shows:\n1. Where each \
         camera stood.\n2. The made-up timeline.\n3. The final edit.\n\nThe stage plan is attached too\n"
    );
    // runs of whitespace are one space, none at a line's start or end; a `pre` keeps its own
    assert_eq!(
        parse_html("<p>  one\n  two\t three  <br>\n   four </p>\r\n<p>five</p>").plain_text(),
        "one two three\nfour\nfive\n"
    );
    assert_eq!(
        parse_html("<p>a <b> bold</b> c</p>").nodes,
        [Node::Paragraph(vec![
            text_node("a "),
            Node::Text {
                text: "bold".to_owned(),
                marks: Marks::BOLD,
            },
            text_node(" c"),
        ])]
    );
    assert_eq!(
        parse_html("<pre>  keep\n   this</pre>").plain_text(),
        "  keep\n   this\n"
    );
    // a run of whitespace is one space across a link's or a mention's edge too, and none where a line starts
    for (html, shown) in [
        ("<p><a href=\"https://example.test\">x </a> y</p>", "x y\n"),
        ("<p>x <a href=\"https://example.test\"> \n y </a> z</p>", "x y z\n"),
        ("<p>a<br>\n <a href=\"https://example.test\"> b</a></p>", "a\nb\n"),
        (
            "<p>hi <span itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\"> Lab A</span> there</p>",
            "hi Lab A there\n",
        ),
    ] {
        assert_eq!(parse_html(html).plain_text(), shown, "{html}");
    }
    // a no-break space is text, never collapsed
    assert_eq!(parse_html("<p>a\u{a0}\u{a0}b</p>").plain_text(), "a\u{a0}\u{a0}b\n");
    Ok(())
}

#[test]
fn a_message_of_five_files_and_no_text_reads_every_file_with_where_it_is_kept() -> TestResult {
    let message = message(&fixture("files-grid.json")?["read_back"])?;
    let SharedFiles { files, unread } = message.files();
    assert_eq!(unread, None, "every entry read");
    assert_eq!(message.unshown(), Vec::new());
    let kinds: Vec<FileKind> = files.iter().map(SharedFile::kind).collect();
    assert_eq!(
        kinds,
        [
            FileKind::Archive,
            FileKind::Video,
            FileKind::Video,
            FileKind::Video,
            FileKind::Video
        ]
    );
    for file in &files {
        assert_eq!(file.location.as_deref(), Some("personal > made_up_user_contoso_test"));
        assert!(file.share_url.is_some() && file.object_url.is_some(), "{file:?}");
    }
    Ok(())
}

#[test]
fn a_file_shared_on_from_another_chat_reads_as_its_file() -> TestResult {
    // Teams' "Share this file" posts the entry with `state` null
    let message = message(&fixture("file-shared-on.json")?["read_back"])?;
    let SharedFiles { files, unread } = message.files();
    assert_eq!(unread, None, "the shared entry is read");
    assert_eq!(message.unshown(), Vec::new());
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "made-up-camera-take-1.mp4");
    assert_eq!(files[0].kind(), FileKind::Video);
    assert!(files[0].share_url.is_some() && files[0].object_url.is_some());
    // any other state is still not a file's
    let mut entries = grid_entries()?;
    entries[1]["state"] = json!("deleted");
    assert_eq!(shared_files(Some(&Value::Array(entries))).files.len(), 4);
    Ok(())
}

#[test]
fn a_share_posts_the_files_entry_as_teams_does() -> TestResult {
    let read = fixture("file-shared-on.json")?;
    let sent: Vec<Value> = serde_json::from_str(read["sent"]["properties"]["files"].as_str().ok_or("files")?)?;
    let file = shared_files(Some(&Value::Array(grid_entries()?))).files.remove(1);
    let entry = share_entry(&Cloud::Microsoft, &file, Some("anonymous")).ok_or("an entry")?;
    assert_eq!(entry, sent[0]);
    // no scope read: the entry names none; an unknown scope is left out too
    let unscoped = share_entry(&Cloud::Microsoft, &file, None).ok_or("an entry")?;
    assert!(unscoped.get("permissionScope").is_none());
    let odd = share_entry(&Cloud::Microsoft, &file, Some("everyone")).ok_or("an entry")?;
    assert!(odd.get("permissionScope").is_none());
    // a file read with no entry, or on another cloud, is not shared on
    assert_eq!(
        share_entry(
            &Cloud::Microsoft,
            &SharedFile {
                entry: None,
                ..file.clone()
            },
            None
        ),
        None
    );
    let own = Cloud::own("https://chat.example:8443")?;
    assert_eq!(share_entry(&own, &file, None), None);
    Ok(())
}

#[test]
fn a_message_sharing_a_file_on_posts_its_entry_and_keeps_it_when_stored() -> TestResult {
    let read = fixture("file-shared-on.json")?;
    let file = shared_files(Some(&Value::Array(grid_entries()?))).files.remove(1);
    let entry = share_entry(&Cloud::Microsoft, &file, Some("anonymous")).ok_or("an entry")?;
    let text = MessageText::plain("Made-up share note")?.with_shared(vec![entry.clone()]);
    assert_eq!(text.html(), read["sent"]["content"].as_str().ok_or("content")?);
    let files: Value = serde_json::from_str(&text.files_property())?;
    let sent: Value = serde_json::from_str(read["sent"]["properties"]["files"].as_str().ok_or("files")?)?;
    assert_eq!(files, sent);
    let stored = serde_json::to_value(&text)?;
    let restored: MessageText = serde_json::from_value(stored.clone())?;
    assert_eq!(restored, text);
    assert_eq!(restored.shared(), std::slice::from_ref(&entry));
    assert_eq!(text.clone().for_cloud(&Cloud::Microsoft)?, text);
    // an entry that isn't a file on the account's service is refused, stored or sent
    let mut outside = entry;
    outside["fileInfo"]["shareUrl"] = json!("https://files.example/made-up");
    outside["objectUrl"] = json!("https://files.example/made-up");
    outside["fileInfo"]["fileUrl"] = json!("https://files.example/made-up");
    let off = MessageText::plain("Made-up share note")?.with_shared(vec![outside.clone()]);
    assert!(off.for_cloud(&Cloud::Microsoft).is_err());
    let mut stored = stored;
    stored["shared"] = json!([outside]);
    assert!(serde_json::from_value::<MessageText>(stored).is_err());
    Ok(())
}

#[test]
fn a_shared_file_and_an_app_card_survive_storage_and_cloud_conversion() -> TestResult {
    let file = shared_files(Some(&Value::Array(grid_entries()?))).files.remove(1);
    let entry = share_entry(&Cloud::Microsoft, &file, Some("anonymous")).ok_or("an entry")?;
    let card = teams_proto::chat::AppCard::new(
        "00000000-0000-4000-8000-0000000000a0".to_owned(),
        "Made-up Finder".to_owned(),
        None,
        "application/vnd.microsoft.card.adaptive".to_owned(),
        json!({"type": "AdaptiveCard", "body": []}),
    );
    let text = MessageText::plain("Made-up share note")?
        .with_shared(vec![entry.clone()])
        .with_cards(vec![card.clone()]);
    let restored: MessageText = serde_json::from_value(serde_json::to_value(&text)?)?;
    assert_eq!(restored, text);
    assert_eq!(restored.shared(), std::slice::from_ref(&entry));
    assert_eq!(restored.cards(), std::slice::from_ref(&card));
    assert_eq!(restored.for_cloud(&Cloud::Microsoft)?, text);
    Ok(())
}

#[test]
fn a_share_refuses_any_link_that_the_file_reader_discarded() -> TestResult {
    let file = shared_files(Some(&Value::Array(grid_entries()?))).files.remove(1);
    let good = share_entry(&Cloud::Microsoft, &file, Some("anonymous")).ok_or("an entry")?;
    for path in [
        "/objectUrl",
        "/fileInfo/shareUrl",
        "/fileInfo/fileUrl",
        "/filePreview/previewUrl",
    ] {
        for link in [
            "https://outside.example/secret.txt",
            "https://x@contoso-my.sharepoint.com/secret.txt",
        ] {
            let mut mixed = good.clone();
            *mixed.pointer_mut(path).ok_or("link field")? = json!(link);
            let read = shared_files(Some(&json!([mixed.clone()])));
            assert_eq!(read.files.len(), 1, "the other valid link still draws a card");
            assert!(share_entry(&Cloud::Microsoft, &read.files[0], Some("anonymous")).is_none());
            let text = MessageText::plain("test")?.with_shared(vec![mixed]);
            assert!(text.for_cloud(&Cloud::Microsoft).is_err());
        }
    }
    Ok(())
}

#[test]
fn a_sharing_links_permission_is_read_by_its_scope_and_words() {
    let body = json!({"@odata.context": "made-up", "hasPassword": false, "id": "made-up-permission", "roles": ["write"],
        "link": {"scope": "anonymous", "type": "edit", "localizedDescription": "Anyone with the link can edit",
        "webUrl": "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUp"}});
    let read = read_link_permission(body.to_string().as_bytes());
    assert_eq!(
        read,
        Some(LinkPermission {
            scope: "anonymous".to_owned(),
            description: Some("Anyone with the link can edit".to_owned()),
        })
    );
    let no_words = json!({"link": {"scope": "organization"}});
    assert_eq!(
        read_link_permission(no_words.to_string().as_bytes()).and_then(|read| read.description),
        None
    );
    for odd in [
        json!({"link": {"scope": "everyone"}}),
        json!({"roles": ["write"]}),
        json!([]),
    ] {
        assert_eq!(read_link_permission(odd.to_string().as_bytes()), None, "{odd}");
    }
    assert_eq!(
        share_token("https://contoso-my.sharepoint.com/:v:/g/x"),
        "u!aHR0cHM6Ly9jb250b3NvLW15LnNoYXJlcG9pbnQuY29tLzp2Oi9nL3g"
    );
}

/// The five entries of `files-grid.json`, as a list.
fn grid_entries() -> TestResult<Vec<Value>> {
    let read = fixture("files-grid.json")?;
    let files = read["read_back"]["properties"]["files"].as_str().ok_or("files")?;
    Ok(serde_json::from_str(files)?)
}

#[test]
fn a_files_string_with_a_lone_surrogate_escape_is_read_as_a_browser_reads_it() -> TestResult {
    // a name cut in the middle of an emoji: JavaScript's JSON reader keeps the lone half, `serde_json` refuses it
    let text = Value::Array(grid_entries()?).to_string().replacen(
        "made-up-camera-take-1.mp4\"",
        "made-up-camera-take-1 \\ud83c.mp4\"",
        2,
    );
    assert!(serde_json::from_str::<Value>(&text).is_err(), "serde refuses it");
    let read = shared_files(Some(&Value::String(text)));
    assert_eq!(read.unread, None);
    assert_eq!(read.files.len(), 5);
    assert_eq!(read.files[1].name, "made-up-camera-take-1 \u{fffd}.mp4");
    // a pair stays the character it is, an escaped backslash stays text
    let text = Value::Array(grid_entries()?).to_string().replacen(
        "made-up-stage-pack.zip\"",
        "made-up \\ud83c\\udfac \\\\ud83c pack.zip\"",
        2,
    );
    let read = shared_files(Some(&Value::String(text)));
    assert_eq!(read.files[0].name, "made-up \u{1f3ac} \\ud83c pack.zip");
    Ok(())
}

#[test]
fn a_files_value_of_one_entry_or_encoded_twice_is_read() -> TestResult {
    let entries = grid_entries()?;
    let one = shared_files(Some(&Value::String(entries[0].to_string())));
    assert_eq!((one.files.len(), one.unread), (1, None));
    let twice = Value::String(Value::String(Value::Array(entries).to_string()).to_string());
    let read = shared_files(Some(&twice));
    assert_eq!((read.files.len(), read.unread), (5, None));
    Ok(())
}

#[test]
fn file_entries_of_another_file_type_or_none_are_read() -> TestResult {
    let mut entries = grid_entries()?;
    entries[1]["@type"] = json!("http://schema.skype.com/VideoFile");
    entries[2].as_object_mut().ok_or("entry")?.remove("@type");
    entries[3]["@type"] = Value::Null;
    let read = shared_files(Some(&Value::Array(entries.clone())));
    assert_eq!((read.files.len(), read.unread), (5, None));
    // a type that names no file is still not one
    entries[4]["@type"] = json!("http://schema.skype.com/HyperLink");
    let read = shared_files(Some(&Value::Array(entries.clone())));
    assert_eq!(read.files.len(), 4);
    assert_eq!(read.unread, Some(json!([entries[4]])));
    Ok(())
}

#[test]
fn a_file_without_a_sharepoint_link_is_drawn_by_its_name_and_kept_as_unread() -> TestResult {
    let mut entries = grid_entries()?;
    let off = "https://files.example.test/made-up-camera-take-2.mp4";
    for key in ["shareUrl", "fileUrl"] {
        entries[2]["fileInfo"][key] = json!(off);
    }
    entries[2]["objectUrl"] = json!(off);
    let read = shared_files(Some(&Value::Array(entries.clone())));
    assert_eq!(read.files.len(), 5, "every card drawn");
    let linkless = &read.files[2];
    assert_eq!(linkless.name, "made-up-camera-take-2.mp4");
    assert_eq!(linkless.kind(), FileKind::Video);
    assert_eq!(
        (linkless.share_url.as_deref(), linkless.object_url.as_deref()),
        (None, None)
    );
    assert_eq!(linkless.open_link(), None, "its link is never kept");
    assert_eq!(
        read.unread,
        Some(json!([entries[2]])),
        "the message still says what it can't show"
    );
    Ok(())
}

#[test]
fn a_file_without_a_sharepoint_link_keeps_no_preview() -> TestResult {
    let mut entries = grid_entries()?;
    let off = "https://files.example.test/made-up-photo.jpg";
    for key in ["shareUrl", "fileUrl"] {
        entries[0]["fileInfo"][key] = json!(off);
    }
    entries[0]["objectUrl"] = json!(off);
    entries[0]["fileType"] = json!("jpg");
    entries[0]["filePreview"] = json!({
        "previewUrl": "https://us-api.asm.skype.com/v1/objects/0-made-up-photo-0001/views/imgo",
        "previewWidth": 501,
        "previewHeight": 500,
    });
    let read = shared_files(Some(&Value::Array(entries[..1].to_vec())));
    let [file] = read.files.as_slice() else {
        return Err("one file".into());
    };
    assert_eq!(file.kind(), FileKind::Image);
    assert_eq!(
        (file.open_link(), &file.preview),
        (None, &None),
        "never opened or fetched"
    );
    assert_eq!(read.unread, Some(json!([entries[0]])));
    Ok(())
}

#[test]
fn a_files_location_is_the_first_site_link_with_a_path() -> TestResult {
    let mut entries = grid_entries()?;
    entries[0]["fileInfo"]["siteUrl"] = json!("https://contoso-my.sharepoint.com/");
    entries[0]["baseUrl"] = json!("https://contoso-my.sharepoint.com/personal/made_up_other_contoso_test/");
    let read = shared_files(Some(&Value::Array(entries[..1].to_vec())));
    let [file] = read.files.as_slice() else {
        return Err("one file".into());
    };
    assert_eq!(file.location.as_deref(), Some("personal > made_up_other_contoso_test"));
    Ok(())
}

#[test]
fn a_card_beside_five_files_leaves_every_file_read() -> TestResult {
    let mut read = fixture("files-grid.json")?["read_back"].clone();
    read["properties"]["cards"] = json!("[{\"made-up\":\"card\"}]");
    let message = message(&read)?;
    let SharedFiles { files, unread } = message.files();
    assert_eq!((files.len(), unread), (5, None));
    assert!(matches!(message.unshown().as_slice(), [Unshown::Cards(_)]));
    Ok(())
}

#[test]
fn a_received_messages_files_are_read_by_name_type_and_links() -> TestResult {
    let message = files_message()?;
    let SharedFiles { files, unread } = message.files();
    assert_eq!(unread, None);
    assert!(files.iter().all(|file| file.entry.is_some()), "each keeps its entry");
    let read: Vec<SharedFile> = files
        .iter()
        .map(|file| SharedFile {
            entry: None,
            ..file.clone()
        })
        .collect();
    let personal = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test";
    let shared = "https://contoso-my.sharepoint.com/:{kind}:/g/personal/made_up_user_contoso_test";
    assert_eq!(
        read,
        [
            SharedFile {
                name: "02-made-up-rehearsal-cut-v3.mp4".to_owned(),
                extension: "mp4".to_owned(),
                share_url: Some(format!(
                    "{}/MadeUpShareLinkForTheRehearsalCut0001",
                    shared.replace("{kind}", "v")
                )),
                object_url: Some(format!(
                    "{personal}/Documents/Microsoft Teams Chat Files/02-made-up-rehearsal-cut-v3.mp4"
                )),
                preview: None,
                location: Some("personal > made_up_user_contoso_test".to_owned()),
                entry: None,
            },
            SharedFile {
                name: "01-made-up-stage-plan.pdf".to_owned(),
                extension: "pdf".to_owned(),
                share_url: Some(format!(
                    "{}/MadeUpShareLinkForTheStagePlan0002",
                    shared.replace("{kind}", "b")
                )),
                object_url: Some(format!(
                    "{personal}/Documents/Microsoft Teams Chat Files/01-made-up-stage-plan.pdf"
                )),
                preview: None,
                location: Some("personal > made_up_user_contoso_test".to_owned()),
                entry: None,
            },
        ]
    );
    assert_eq!(message.unshown(), Vec::new(), "every file read: nothing unshown");
    let (video, pdf) = (&files[0], &files[1]);
    // a Microsoft site names its owner itself: no own server's account to name
    assert_eq!(video.site_owner(), None);
    assert_eq!((video.kind(), pdf.kind()), (FileKind::Video, FileKind::Pdf));
    assert!(video.kind().has_thumbnail() && !pdf.kind().has_thumbnail());
    // a click opens the sharing link; Download is the same link with `download=1`
    assert_eq!(video.open_link(), video.share_url.as_deref());
    assert_eq!(
        video.download_link(),
        Some(format!(
            "{}/MadeUpShareLinkForTheRehearsalCut0001?download=1",
            shared.replace("{kind}", "v")
        ))
    );
    Ok(())
}

#[test]
fn an_ordered_and_a_bulleted_list_are_read_as_lists_of_items() {
    // as the web client sends one: a line feed after `<ol>`, the items' `li`s back to back, a nested list in an item
    let read = parse_html(
        "<p>Pick one:</p>\n<ol>\n<li>External API</li><li><b>Internal</b> API\n<ul>\n<li>inner</li></ul>\n</li></ol>\n<p>&nbsp;</p>",
    );
    let inner = Node::List(List {
        ordered: false,
        start: 1,
        items: vec![vec![text_node("inner")]],
    });
    assert_eq!(
        read.nodes,
        [
            Node::Paragraph(vec![text_node("Pick one:")]),
            Node::List(List {
                ordered: true,
                start: 1,
                items: vec![
                    vec![text_node("External API")],
                    vec![
                        Node::Text {
                            text: "Internal".to_owned(),
                            marks: Marks::BOLD,
                        },
                        text_node(" API"),
                        inner,
                    ],
                ],
            }),
            Node::Paragraph(vec![text_node("\u{a0}")]),
        ]
    );
    assert_eq!(
        read.plain_text(),
        "Pick one:\nExternal API\nInternal API\ninner\n\u{a0}\n"
    );
    // an `ol`'s `start` numbers its first item; one that isn't digits, or a `ul`'s, is 1; an `li` in no list is a
    // paragraph
    let start = |html: &str| match &parse_html(html).nodes[..] {
        [Node::List(list)] => Some(list.start),
        _ => None,
    };
    assert_eq!(start("<ol start=\"4\"><li>d</li></ol>"), Some(4));
    assert_eq!(start("<ol start=\"-2\"><li>x</li></ol>"), Some(1));
    assert_eq!(start("<ol start=\"99999999\"><li>x</li></ol>"), Some(1));
    assert_eq!(start("<ul start=\"4\"><li>x</li></ul>"), Some(1));
    assert_eq!(
        parse_html("<li>alone</li>").nodes,
        [Node::Paragraph(vec![text_node("alone")])]
    );
}

#[test]
fn a_table_is_read_as_rows_of_cells_a_header_cell_marked() {
    // as an email's body has one: a `thead` and a `tbody` (read through), whitespace between the cells, a cell with
    // more than text
    let read = parse_html(
        "<p>Before</p>\n<table border=\"1\">\n<thead><tr><th>Name</th> <th>State</th></tr></thead>\n<tbody>\n\
         <tr><td>Cell one</td>\n<td><b>Cell</b> two<br>more</td></tr>\n<tr><td></td><td>last</td></tr></tbody></table>",
    );
    let cell = |header: bool, nodes: Vec<Node>| TableCell { header, nodes };
    assert_eq!(
        read.nodes,
        [
            Node::Paragraph(vec![text_node("Before")]),
            Node::Table(Table {
                rows: vec![
                    vec![
                        cell(true, vec![text_node("Name")]),
                        cell(true, vec![text_node("State")]),
                    ],
                    vec![
                        cell(false, vec![text_node("Cell one")]),
                        cell(
                            false,
                            vec![
                                Node::Text {
                                    text: "Cell".to_owned(),
                                    marks: Marks::BOLD,
                                },
                                text_node(" two"),
                                Node::Break,
                                text_node("more"),
                            ],
                        ),
                    ],
                    vec![cell(false, Vec::new()), cell(false, vec![text_node("last")])],
                ],
            }),
        ]
    );
    // a reader sees each row on a line, its cells apart
    assert_eq!(
        read.plain_text(),
        "Before\nName State\nCell one Cell two\nmore\n last\n"
    );
    // a cell or a row outside a table is no table, as a browser reads it
    assert_eq!(parse_html("<td>alone</td>").nodes, [text_node("alone")]);
}

/// A block of code as Teams' code block editor sends one (the shape of a received message, every value made up): the
/// editor's own paragraph, then `pre` with its language's class, its lines split by `<br>`, its spaces `&nbsp;`, its
/// tokens in highlight.js spans.
const CODE_BLOCK: &str = "<p>it shows as this object:</p>\n<p></p>\n\
    <p itemtype=\"http://schema.skype.com/CodeBlockEditor\" id=\"x_codeBlockEditor-made-up\">\n&nbsp;</p>\n\
    <pre class=\"language-json language-was-manually-selected\" itemid=\"codeBlockEditor-made-up\"><code>{<br> \
    &nbsp;&nbsp; <span class=\"hljs-function\">&quot;kind&quot;</span>:&nbsp;<span class=\"hljs-string\">\
    &quot;made&nbsp;up&quot;</span>,<br> &nbsp;&nbsp; <span class=\"hljs-function\">&quot;next&quot;</span>:&nbsp;\
    <span class=\"hljs-keyword\">null</span><br>}</code></pre>\n<p></p>\n<p>and nothing else</p>";

#[test]
fn a_code_block_is_read_with_its_language_lines_and_tokens() {
    let read = parse_html(CODE_BLOCK);
    let run = |text: &str, token| CodeRun {
        text: text.to_owned(),
        token,
    };
    assert_eq!(
        read.nodes,
        [
            Node::Paragraph(vec![text_node("it shows as this object:")]),
            Node::Paragraph(vec![]),
            Node::Code(Code {
                language: Some("json".to_owned()),
                runs: vec![
                    run("{\n    ", Token::Plain),
                    run("\"kind\"", Token::Function),
                    run(": ", Token::Plain),
                    run("\"made up\"", Token::String),
                    run(",\n    ", Token::Plain),
                    run("\"next\"", Token::Function),
                    run(": ", Token::Plain),
                    run("null", Token::Keyword),
                    run("\n}", Token::Plain),
                ],
            }),
            Node::Paragraph(vec![]),
            Node::Paragraph(vec![text_node("and nothing else")]),
        ]
    );
    assert_eq!(
        read.plain_text(),
        "it shows as this object:\n\n{\n    \"kind\": \"made up\",\n    \"next\": null\n}\n\nand nothing else\n"
    );
}

#[test]
fn a_bare_pre_is_a_code_block_and_nothing_in_one_but_its_text_is_kept() {
    // no language, its line feeds kept, a final one dropped; a link, a mark or a mention in it gives its text, a script
    // nothing
    let read =
        parse_html("<pre>  keep\n<b>this</b> <a href=\"https://example.test\">link</a><script>x()</script>\n</pre>");
    assert_eq!(
        read.nodes,
        [Node::Code(Code {
            language: None,
            runs: vec![CodeRun {
                text: "  keep\nthis link".to_owned(),
                token: Token::Plain,
            }],
        })]
    );
    // a language id of another shape is none; an unknown highlight class is plain text
    let language = |class: &str| teams_proto::chat::language_of(class);
    assert_eq!(language("language-C# other"), Some("c#".to_owned()));
    assert_eq!(language("language-\"><x"), None);
    assert_eq!(language("plain"), None);
    assert_eq!(Token::of_class("hljs-made-up"), Some(Token::Plain));
    assert_eq!(Token::of_class("other hljs-number"), Some(Token::Number));
    assert_eq!(Token::of_class("other"), None);
}

#[test]
fn an_images_preview_on_ams_is_read_with_its_size() -> TestResult {
    let read = files_message()?;
    let entries: Value = serde_json::from_str(read.properties["files"].as_str().ok_or("files")?)?;
    let src = "https://us-api.asm.skype.com/v1/objects/0-made-up-photo-0001/views/imgo";
    let with_preview = |preview: Value| {
        let mut entry = entries[0].clone();
        entry["fileName"] = json!("made-up-photo.jpg");
        entry["fileType"] = json!("jpg");
        entry["filePreview"] = preview;
        shared_files(Some(&json!([entry]))).files.remove(0)
    };
    let file = with_preview(json!({"previewUrl": src, "previewWidth": 501, "previewHeight": 500}));
    assert_eq!(file.kind(), FileKind::Image);
    assert_eq!(
        file.preview,
        Some(FilePreview {
            src: src.to_owned(),
            size: Some((501, 500)),
        })
    );
    // a size off its range is none; a link that isn't a view of an AMS object is no preview
    let sizeless = with_preview(json!({"previewUrl": src, "previewWidth": 0, "previewHeight": 500}));
    assert_eq!(sizeless.preview.map(|preview| preview.size), Some(None));
    for link in [
        "",
        "https://example.com/v1/objects/0-made-up-photo-0001/views/imgo",
        "https://us-api.asm.skype.com/v1/objects/0-made-up-photo-0001/views/imgo?x=1",
    ] {
        assert_eq!(with_preview(json!({"previewUrl": link})).preview, None, "{link}");
    }
    Ok(())
}

#[test]
fn a_file_entry_off_its_shape_is_kept_as_it_came_beside_the_ones_read() -> TestResult {
    let read = files_message()?;
    let entries: Value = serde_json::from_str(read.properties["files"].as_str().ok_or("files")?)?;
    let good = entries[0].clone();
    let off = |change: &dyn Fn(&mut Value)| {
        let mut entry = good.clone();
        change(&mut entry);
        entry
    };
    let odd = [
        off(&|entry| entry["@type"] = json!("http://schema.skype.com/Card")),
        off(&|entry| entry["state"] = json!("deleted")),
        off(&|entry| {
            entry["fileName"] = json!("");
            entry["title"] = Value::Null;
        }),
        off(&|entry| {
            entry["fileInfo"]["shareUrl"] = json!("http://contoso-my.sharepoint.com/x");
            entry["objectUrl"] = json!("https://example.com/x");
            entry["fileInfo"]["fileUrl"] = json!("javascript:alert(1)");
        }),
        json!("not an object"),
    ];
    let mut list = vec![good.clone()];
    list.extend(odd.iter().cloned());
    let files = shared_files(Some(&Value::String(Value::Array(list).to_string())));
    // the one with links off its shape is drawn by its name, none of its links kept
    assert_eq!(files.files.len(), 2);
    assert_eq!(files.files[1].open_link(), None);
    assert_eq!(files.unread, Some(Value::Array(odd.to_vec())));
    // a list, not a JSON string of one, reads alike; the extension comes from the name when no type is given
    let mut typeless = good;
    typeless["fileType"] = Value::Null;
    typeless["type"] = Value::Null;
    let files = shared_files(Some(&json!([typeless])));
    assert_eq!(files.files[0].extension, "mp4");
    assert_eq!(files.unread, None);
    Ok(())
}

#[test]
fn a_link_s_preview_is_read_from_its_entry_and_one_turned_off_is_left_out() -> TestResult {
    let message = message(&fixture("link-preview.json")?["read_back"])?;
    let previews = message.link_previews();
    assert_eq!(previews.len(), 1, "the preview turned off is left out: {previews:#?}");
    let preview = &previews[0];
    assert_eq!(preview.url, "http://made-up.example.test");
    assert_eq!(preview.site, "made-up.example.test");
    assert_eq!(preview.title, "Made-up Site, a page invented for the test");
    assert!(
        preview
            .description
            .as_deref()
            .is_some_and(|text| text.starts_with("A made-up page's"))
    );
    assert_eq!(
        preview.picture.as_deref(),
        Some(
            "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image/Thumbnail?url=https%3a%2f%2fcdn.example.\
             test%2fmade-up%2fcard-800x418.png"
        )
    );
    assert_eq!(preview.picture_size, Some((320, 167)));
    // a link the compose box sent before its preview was read draws no card
    let unread = json!(
        r#"[{"@type":"http://schema.skype.com/HyperLink","itemid":"0","url":"https://example.test/","previewenabled":true,"preview":{"previewurl":""}}]"#
    );
    assert!(teams_proto::chat::link_previews(Some(&unread)).is_empty());
    Ok(())
}

#[test]
fn a_fully_transparent_colour_is_no_colour_and_a_tint_its_colour() {
    use teams_proto::chat::{Rgb, SpanStyle};
    assert_eq!(Rgb::parse("rgba(0, 0, 0, 0)"), None);
    assert_eq!(Rgb::parse("rgba(255,255,255,0.0)"), None);
    assert_eq!(Rgb::parse("rgba(1, 2, 3, 0%)"), None);
    assert_eq!(Rgb::parse("transparent"), None);
    assert_eq!(
        Rgb::parse("rgba(1, 2, 3, x)"),
        None,
        "an alpha unread is a colour unread"
    );
    assert_eq!(Rgb::parse("rgba(1, 2, 3, 0.5)"), Some(Rgb(0x01_0203)));
    assert_eq!(Rgb::parse("rgba(1, 2, 3, 40%)"), Some(Rgb(0x01_0203)));
    assert_eq!(Rgb::parse("rgba(1, 2, 3, 1)"), Some(Rgb(0x01_0203)));
    assert_eq!(Rgb::parse("rgb(1, 2, 3)"), Some(Rgb(0x01_0203)));
    // a browser's copy of plain text: no highlight, its colour kept
    let style = SpanStyle::of_css("color: rgb(36, 36, 36); background-color: rgba(0, 0, 0, 0)");
    assert_eq!((style.color, style.highlight), (Some(Rgb(0x24_2424)), None));
    let read = parse_html("<p><span style=\"background-color: transparent\">plain</span></p>");
    assert_eq!(
        read.nodes,
        vec![Node::Paragraph(vec![Node::Text {
            text: "plain".to_owned(),
            marks: Marks::NONE,
        }])]
    );
}

#[tokio::test]
async fn a_blocked_copy_is_read_by_id_with_properties_and_missing_or_invalid_answers_are_safe() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_A)?;
    let reads = ChatReads::new(&client);
    let copy = fixture("policy-blocked.json")?["copy"].clone();
    let id = copy["id"].as_str().ok_or("copy id")?;
    let path = format!("/conversations/48:generic/messages/{id}");
    mock.answer_next("GET", &path, 200, &copy).await;
    assert_eq!(reads.blocked_copy(id).await?, Some(message(&copy)?));
    let requests = mock.received_requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
    assert_eq!(
        requests[0].url.query_pairs().collect::<Vec<_>>(),
        [("view".into(), "msnp24Equivalent|supportsMessageProperties".into())]
    );
    assert_eq!(reads.blocked_copy(id).await?, None);
    for invalid in [json!({}), json!({"id": "another-copy"})] {
        mock.answer_next("GET", &path, 200, &invalid).await;
        assert!(matches!(reads.blocked_copy(id).await, Err(ChatReadError::Malformed(_))));
    }
    mock.answer_next("GET", &path, 403, &json!({})).await;
    assert_eq!(reads.blocked_copy(id).await.unwrap_err().status(), Some(403));
    Ok(())
}

#[tokio::test]
async fn own_chat_pages_sync_and_backward_links_stay_on_the_account_base() -> TestResult {
    let cloud = teams_proto::Cloud::own("https://chat.example:8443")?;
    let base = "https://chat.example:8443/api/chatsvc/amer";
    let mock = ChatServiceMock::start_for_cloud(cloud.clone()).await?;
    mock.page_conversation_list(1);
    let transport = HttpTransport::for_cloud_with_mock_origin(
        Arc::new(MockToken(mock.token(LAB_A).ok_or("no account")?)),
        base,
        cloud,
        &mock.origin(),
    )?;
    let client = Client::new(transport);
    let reads = ChatReads::new(&client);
    let list = reads.conversations().await?;
    let sync = list.sync_state().ok_or("no sync")?;
    assert!(sync.starts_with(base));
    assert!(reads.conversations_from(sync).await?.conversations.is_empty());
    let forward = list.forward_link().ok_or("no forward link")?;
    assert!(forward.starts_with(base));
    assert_eq!(reads.conversations_from(forward).await?.conversations.len(), 1);
    let first = reads.messages(AB_CHAT, 2).await?;
    let mut ids: Vec<String> = first.messages.iter().map(|message| message.id.clone()).collect();
    let mut backward = first.backward_link().map(str::to_owned);
    while let Some(link) = backward {
        assert!(link.starts_with(base));
        let page = reads.messages_from(AB_CHAT, &link).await?;
        ids.extend(page.messages.iter().map(|message| message.id.clone()));
        backward = page.backward_link().map(str::to_owned);
    }
    assert_eq!(ids.len(), 5);
    assert_eq!(ids.iter().collect::<std::collections::BTreeSet<_>>().len(), 5);
    assert!(
        reads
            .messages_from(AB_CHAT, first.sync_state().ok_or("no message sync")?)
            .await?
            .messages
            .is_empty()
    );
    let before = mock.received_requests().await.len();
    refuse_own_chat_links(&reads, sync, first.sync_state().unwrap(), base).await;
    assert_eq!(mock.received_requests().await.len(), before);
    Ok(())
}

async fn refuse_own_chat_links(reads: &ChatReads<'_, MockToken>, sync: &str, message_sync: &str, base: &str) {
    for bad_base in [
        CHAT_BASE,
        "https://other.example:8443/api/chatsvc/amer",
        "https://chat.example/api/chatsvc/amer",
        "http://chat.example:8443/api/chatsvc/amer",
        "https://user@chat.example:8443/api/chatsvc/amer",
        "https://chat.example:8443/api/chatsvc/emea",
        "https://chat.example:8443/api/mt/part/amer-03",
    ] {
        refuse_own_chat_base(reads, sync, message_sync, base, bad_base).await;
    }
    assert!(matches!(
        reads.messages_from(ABC_CHAT, message_sync).await,
        Err(ChatReadError::Link { .. })
    ));
    assert!(matches!(
        reads.conversations_from(message_sync).await,
        Err(ChatReadError::Link { .. })
    ));
    let link = format!("{message_sync}#fragment");
    assert!(matches!(
        reads.messages_from(AB_CHAT, &link).await,
        Err(ChatReadError::Link { .. })
    ));
}

async fn refuse_own_chat_base(
    reads: &ChatReads<'_, MockToken>,
    sync: &str,
    message_sync: &str,
    base: &str,
    bad_base: &str,
) {
    let link = sync.replacen(base, bad_base, 1);
    assert!(
        matches!(reads.conversations_from(&link).await, Err(ChatReadError::Link { .. })),
        "{link}"
    );
    let link = message_sync.replacen(base, bad_base, 1);
    assert!(
        matches!(
            reads.messages_from(AB_CHAT, &link).await,
            Err(ChatReadError::Link { .. })
        ),
        "{link}"
    );
}

/// The labels of the recipients the "To" line names.
fn shown_labels(details: &teams_proto::chat::EmailDetails) -> Vec<&str> {
    details.shown_recipients().iter().map(|user| user.label()).collect()
}

#[test]
fn a_post_that_came_by_email_has_its_subject_and_its_email_details() -> TestResult {
    use teams_proto::chat::EmailUser;
    let email = message(&fixture("email-post.json")?["read_back"])?;
    assert_eq!(
        email.subject().as_deref(),
        Some("Made-up alert: memory use - test cluster")
    );
    let details = email.email_details().ok_or("the post's email details")?;
    let alerts = EmailUser {
        name: None,
        smtp: Some("alerts-noreply@example.com".to_owned()),
    };
    assert_eq!(details.from.as_ref(), Some(&alerts));
    assert_eq!(details.sender.as_ref(), Some(&alerts));
    assert_eq!(alerts.label(), "alerts-noreply@example.com");
    assert_eq!(shown_labels(&details), ["made-up-channel.example@example.com"]);
    assert_eq!(details.more_recipients(), 0);
    assert!(
        details
            .email_link
            .as_deref()
            .is_some_and(|link| link.ends_with("Made-up alert resolved.eml"))
    );
    // a plain post has no subject and no email
    let plain = message(&fixture("link-preview.json")?["read_back"])?;
    assert_eq!(plain.subject(), None);
    assert_eq!(plain.email_details(), None);
    Ok(())
}

#[test]
fn an_email_s_to_line_names_three_at_most_and_counts_the_others() -> TestResult {
    use teams_proto::chat::EmailDetails;
    let many = json!({
        "to": [{"name": "Ann", "smtp": "ann@example.com"}, {"smtp": "bo@example.com"}, {"name": " "}],
        "cc": [{"smtp": "cy@example.com"}, {"smtp": "di@example.com"}],
        "totalRecipients": 9,
        "emailLink": "https://made-up.example.com/original.eml"
    });
    let details = EmailDetails::of(Some(&many)).ok_or("an object reads too")?;
    assert_eq!(shown_labels(&details), ["Ann", "bo@example.com", "cy@example.com"]);
    assert_eq!(details.more_recipients(), 6);
    assert_eq!(details.email_link, None, "a link off SharePoint is not opened");
    // without totalRecipients, the lists are counted
    let text = json!(r#"{"to":[{"smtp":"a@example.com"}],"cc":[{"smtp":"b@example.com"}]}"#);
    let counted = EmailDetails::of(Some(&text)).ok_or("a JSON string of an object")?;
    assert_eq!(counted.total_recipients, 2);
    assert_eq!(counted.from, None);
    Ok(())
}

#[test]
fn what_is_not_an_email_object_is_no_email_and_only_file_links_are_opened() {
    use teams_proto::chat::{EmailDetails, is_email_link};
    assert_eq!(EmailDetails::of(Some(&json!("not json"))), None);
    assert_eq!(EmailDetails::of(Some(&json!([1]))), None);
    assert_eq!(EmailDetails::of(None), None);
    assert!(is_email_link("https://contoso.sharepoint.com/sites/A/x.eml"));
    assert!(is_email_link("https://teams.own.example.com/sp/files/x.eml"));
    assert!(
        !is_email_link("https://teams.own.example.com/files/x.eml"),
        "off the file service"
    );
    assert!(!is_email_link("http://teams.own.example.com/sp/files/x.eml"));
    assert!(!is_email_link("https://user@teams.own.example.com/sp/files/x.eml"));
    assert!(!is_email_link("http://contoso.sharepoint.com/x.eml"));
    assert!(!is_email_link("not a link"));
}

#[test]
fn a_long_email_link_is_kept_whole_or_dropped() {
    use teams_proto::chat::EmailDetails;
    let link_of = |len: usize| {
        let base = "https://contoso.sharepoint.com/sites/A/";
        format!("{base}{}.eml", "a".repeat(len - base.len() - 4))
    };
    let long = link_of(2000);
    let kept = EmailDetails::of(Some(&json!({ "emailLink": format!(" {long} ") })));
    assert_eq!(kept.and_then(|details| details.email_link), Some(long));
    let too_long = EmailDetails::of(Some(&json!({ "emailLink": link_of(4097) })));
    assert_eq!(too_long.and_then(|details| details.email_link), None);
}

#[test]
fn an_own_servers_drive_names_its_owners_account() {
    let file = |link: &str| SharedFile {
        name: "made-up plan.pdf".to_owned(),
        extension: "pdf".to_owned(),
        share_url: Some(link.to_owned()),
        object_url: None,
        preview: None,
        location: None,
        entry: None,
    };
    let own =
        file("https://chat.example:8443/sp/personal/0000000000004000800000000000000B/Documents/made-up%20plan.pdf");
    assert_eq!(
        own.site_owner().as_deref(),
        Some("8:orgid:00000000-0000-4000-8000-00000000000b")
    );
    // not an account's id: a short one, a name, another path
    for link in [
        "https://chat.example:8443/sp/personal/00000000000040008000/Documents/a.pdf",
        "https://chat.example:8443/sp/personal/made_up_user_contoso_test/Documents/a.pdf",
        "https://chat.example:8443/personal/0000000000004000800000000000000b/Documents/a.pdf",
    ] {
        assert_eq!(file(link).site_owner(), None, "{link}");
    }
}

#[tokio::test]
async fn a_sharing_links_permission_is_read_on_the_files_site_with_its_token() -> TestResult {
    let server = MockServer::start().await;
    let site = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";
    let object = format!("{site}Documents/Microsoft%20Teams%20Chat%20Files/made-up.mp4");
    let share = "https://contoso-my.sharepoint.com/:v:/g/personal/made_up_user_contoso_test/MadeUpLink";
    let token = share_token(share);
    Mock::given(method("GET"))
        .and(path_regex(format!(
            "^/personal/made_up_user_contoso_test/_api/v2.0/shares/{}/permission$",
            regex_escape(&token)
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"roles": ["write"],
            "link": {"scope": "anonymous", "type": "edit", "localizedDescription": "Anyone with the link can edit"}})))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "made-up-site-token")?;
    let answer = client.transport().sharing_link_permission(&object, share).await?;
    let read = read_link_permission(answer.body()).ok_or("a permission")?;
    assert_eq!(read.scope, "anonymous");
    // a link or an address off the account's file service is refused before anything is sent
    for (object, share) in [
        (object.as_str(), "https://files.example/made-up"),
        ("https://files.example/made-up", share),
    ] {
        let refused = client.transport().sharing_link_permission(object, share).await;
        assert!(matches!(refused, Err(HttpError::Refused(_))), "{refused:?}");
    }
    assert_eq!(server.received_requests().await.ok_or("recorded")?.len(), 1);
    Ok(())
}

/// `text` with the characters a regex gives a meaning to escaped.
fn regex_escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            let escaped = matches!(
                c,
                '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
            );
            escaped.then_some('\\').into_iter().chain([c])
        })
        .collect()
}

#[test]
fn share_outside_teams_opens_onedrives_page_of_the_file() {
    let file = shared_files(Some(&Value::Array(grid_entries().unwrap())))
        .files
        .remove(1);
    assert_eq!(
        onedrive_page(&file).as_deref(),
        Some(
            "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/_layouts/15/onedrive.aspx?id=%2Fpersonal%2Fmade_up_user_contoso_test%2FDocuments%2FMicrosoft%20Teams%20Chat%20Files%2Fmade-up-camera-take-1.mp4&parent=%2Fpersonal%2Fmade_up_user_contoso_test%2FDocuments%2FMicrosoft%20Teams%20Chat%20Files"
        )
    );
    // an own server's file, a team site's or one with no address of its own has none
    for object in [
        Some("https://chat.example:8443/sp/personal/0000000000004000800000000000000B/Documents/plan.pdf"),
        Some("https://contoso.sharepoint.com/sites/made-up/Shared%20Documents/plan.pdf"),
        None,
    ] {
        let other = SharedFile {
            object_url: object.map(str::to_owned),
            ..file.clone()
        };
        assert_eq!(onedrive_page(&other), None, "{object:?}");
    }
}
