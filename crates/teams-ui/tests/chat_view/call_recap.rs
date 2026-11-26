//! A 1:1 call's recording and transcript in its chat as Teams shows them, on GPUI's test context over the fake engine
//! (`common`): the recording saved as its line with the OneDrive that holds it, its recap card under it (the call's
//! title and the recording's span, View recap, its Transcript chip when it was transcribed, the thumbnail with its
//! length and expiry, Share) opening the chat's Recap tab; a transcript no recording holds as a card of its own; one a
//! recording holds told by that recording's card. Every id, name and value here is made up.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::common::{LAB_A, LAB_B, Rig};
use super::parts;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_view::{Body, ChatView, ChatViewEvent, MessageItem, RecapOpen};
use teams_ui::settings::Settings;
use teams_ui::system_message::EventIcon;

const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const CALL: &str = "00000000-0000-4000-8000-0000000000c1";
const OTHER_CALL: &str = "00000000-0000-4000-8000-0000000000c2";
const LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/MadeUpCallRecording";
/// The other call's transcript file, once saved.
const TRANSCRIPT_LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_a/MadeUpCallTranscript";
/// 2026-01-01 12:00 UTC.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;

fn message(id: &str, kind: &str, content: &str, minute: i64) -> Message {
    Message {
        conversation_id: CHAT.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(LAB_A.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: NOON + minute * MINUTE,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

/// A recording's message of `call` in `status` holding `contents`, started by a at 12:01 and kept in a's OneDrive (no
/// organizer named, as a call's recording names none); once ready its link and its file.
fn recording(call: &str, status: &str, code: &str, contents: &str) -> String {
    let ready = status == "Success";
    let link = if ready { LINK } else { "" };
    let file = if ready {
        format!("<item type=\"onedriveForBusinessVideo\" uri=\"{LINK}\" driveId=\"b!drive1\" driveItemId=\"ITEM1\" />")
    } else {
        String::new()
    };
    let owner = LAB_A.trim_start_matches("8:orgid:");
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
         <Title>Call with Lab Bee</Title><a href=\"{link}\">Play</a>\
         <OriginalName v=\"Call with Lab Bee-20260101_120100-Meeting Recording.mp4\" /><MeetingOrganizerId value=\"\" />\
         <RecordingInitiatorId value=\"{LAB_A}\" isAutoRecording=\"False\" />\
         <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{owner}\" />\
         <Identifiers><Id type=\"callId\" value=\"{call}\" /></Identifiers>\
         <RecordingContent contentTypes=\"{contents}\" timestamp=\"2026-01-01T12:01:00.0000000Z\" duration=\"0:00:51.5\">\
         {file}</RecordingContent></URIObject>"
    )
}

/// The transcript alone of `call` saved: its own file (`…-Meeting Transcript.mp4`) in a's OneDrive, as the bot's
/// last state names it.
fn transcript_saved(call: &str) -> String {
    let owner = LAB_A.trim_start_matches("8:orgid:");
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"Success\" code=\"200\" />\
         <Title>Call with Lab Bee</Title><a href=\"{TRANSCRIPT_LINK}\">Play</a>\
         <OriginalName v=\"Call with Lab Bee-20260101_123000-Meeting Transcript.mp4\" /><MeetingOrganizerId value=\"\" />\
         <RecordingInitiatorId value=\"{LAB_A}\" isAutoRecording=\"False\" />\
         <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{owner}\" />\
         <Identifiers><Id type=\"callId\" value=\"{call}\" /></Identifiers>\
         <RecordingContent contentTypes=\"Transcript\" timestamp=\"2026-01-01T12:30:00.0000000Z\" duration=\"0:00:58\">\
         <item type=\"onedriveForBusinessVideo\" uri=\"{TRANSCRIPT_LINK}\" driveId=\"b!drive1\" driveItemId=\"ITEM7\" />\
         </RecordingContent></URIObject>"
    )
}

fn transcript(call: &str) -> String {
    format!(r#"{{\"callId\":\"{call}\",\"isDeleted\":false}}"#)
}

/// The call recorded and transcribed, its transcript's message, the recording ready; then another call transcribed
/// alone.
fn call_messages() -> Vec<Message> {
    let kind = "RichText/Media_CallRecording";
    vec![
        message("1", kind, &recording(CALL, "Initial", "0", "Recording"), 1),
        message("2", kind, &recording(CALL, "Initial", "0", "Transcript"), 1),
        message("3", "RichText/Media_CallTranscript", &transcript(CALL), 1),
        message("4", kind, &recording(CALL, "ChunkFinished", "202", "Recording"), 2),
        message("5", kind, &recording(CALL, "Success", "200", "Recording+Transcript"), 3),
        message(
            "6",
            kind,
            &recording(OTHER_CALL, "ChunkFinished", "202", "Transcript"),
            30,
        ),
        message("7", "RichText/Media_CallTranscript", &transcript(OTHER_CALL), 30),
        message("8", kind, &transcript_saved(OTHER_CALL), 32),
    ]
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn chat_window(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, CHAT.to_owned(), window, cx));
    cx.simulate_resize(size(px(900.0), px(1600.0)));
    draw(cx);
    (view, cx)
}

fn items(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<MessageItem> {
    cx.update(|_, cx| view.read(cx).message_items().cloned().collect())
}

fn rig_with(messages: Vec<Message>) -> Rig {
    let rig = Rig::start();
    rig.core.messages.lock().unwrap().insert(CHAT.to_owned(), messages);
    let mut names = rig.core.names.lock().unwrap();
    names.insert(LAB_A.to_owned(), "Lab Ay".to_owned());
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    drop(names);
    rig.core.expirations.lock().unwrap().insert(
        "ITEM1".to_owned(),
        chrono::Utc::now().timestamp_millis() + 119 * DAY + DAY / 2,
    );
    rig.cx.update(teams_ui::window::init);
    rig
}

#[test]
fn a_calls_recording_is_its_recap_card_under_its_saved_line() {
    let rig = rig_with(call_messages());
    let (view, cx) = chat_window(&rig);
    let shown = items(&view, cx);
    let kinds: Vec<(&str, &str)> = shown.iter().map(|item| (item.id.as_str(), item.body.kind())).collect();
    // "2" and "6", the transcript's halves, show nothing; "3", the transcript the recording holds, is told by its card
    assert_eq!(
        kinds,
        [
            ("1", "timed-event"),
            ("4", "timed-event"),
            ("5", "meeting-ended"),
            ("7", "transcript")
        ]
    );
    the_cards(&view, &shown, cx);
    drawn(cx);
    the_cards_open_the_recap(&view, cx);
}

/// The recording saved: its line (the account's own OneDrive, by its name, as Teams' 1:1 chat says it) and its card;
/// the transcript alone of the other call: its card, its span its own.
fn the_cards(view: &Entity<ChatView>, shown: &[MessageItem], cx: &mut VisualTestContext) {
    let Some(Body::MeetingEnded(saved)) = shown.iter().find(|item| item.id == "5").map(|item| &item.body) else {
        panic!("no recording saved");
    };
    assert_eq!(saved.line.icon, EventIcon::RecordingSaved);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).drawn_text(&saved.line)),
        "Recording has been saved to Lab Ay's OneDrive and will expire in 120 days."
    );
    let card = saved.recap.as_ref().expect("the recording's recap card");
    assert_eq!(card.title.as_deref(), Some("Call with Lab Bee"));
    assert_eq!(card.recordings.len(), 1);
    assert_eq!(card.recordings[0].link, LINK);
    assert!(card.transcript);
    assert_eq!(card.badge().as_deref(), Some("51s"));
    let start = NOON + MINUTE;
    assert_eq!(card.span, Some((start, start + 51_000)));
    // Share → Copy link copies the card's own link, as browser Teams' 1:1 chat copies it
    let link = card.share_link(CHAT).expect("a recap link");
    for part in [
        "threadType=OneOnOneChat",
        "meetingType=Unknown",
        "subType=RecapSharingLink_RecapChiclet",
        "recapType=RecordingAndTranscript",
        "fileUrl=",
    ] {
        assert!(link.contains(part), "{part} in {link}");
    }
    assert!(!link.contains("organizerId"), "{link}");
    let Some(Body::Card(alone)) = shown.iter().find(|item| item.id == "7").map(|item| &item.body) else {
        panic!("no transcript card");
    };
    assert!(alone.transcript && alone.recordings.is_empty());
    assert_eq!(alone.span, Some((start, start + 51_000)));
    the_transcript_alone(alone);
    assert!(cx.update(|_, cx| view.read(cx).has_call_recaps()));
}

/// The other call's card, transcribed alone: its own file once saved is what View recap opens, and Share's link is a
/// 1:1 call's transcript file's.
fn the_transcript_alone(alone: &teams_ui::meeting_recap::RecapCard) {
    let saved = alone.transcribed.as_ref().expect("the transcript's file");
    assert_eq!(saved.link, TRANSCRIPT_LINK);
    assert_eq!(alone.opens(), Some(TRANSCRIPT_LINK));
    let part = alone.transcribed_part().expect("the part it stands for");
    assert!(part.is_unread_transcript());
    assert_eq!(part.item_id.as_deref(), Some("ITEM7"));
    assert_eq!(part.call_id.as_deref(), Some(OTHER_CALL));
    let half = NOON + 30 * MINUTE;
    assert_eq!((part.start_ms, part.end_ms), (Some(half), Some(half + 58_000)));
    let shared = alone.share_link(CHAT).expect("a recap link");
    assert!(shared.ends_with("&recapType=ODSPTranscript"), "{shared}");
    assert!(shared.contains("subType=RecapSharingLink_RecapChiclet"), "{shared}");
}

/// Both cards drawn with their controls; the transcript's with no picture, neither with a web link.
fn drawn(cx: &mut VisualTestContext) {
    for selector in [
        "recap-card-5",
        "view-recap-5",
        "recap-card-share-5",
        "recap-card-transcript-5",
        "recap-card-thumbnail-5",
        "recap-card-expiry-5",
        "recap-card-7",
        "view-recap-7",
        "recap-card-share-7",
        "recap-card-transcript-7",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    assert!(cx.debug_bounds("recap-card-thumbnail-7").is_none());
    assert!(
        cx.debug_bounds("open-on-the-web-5").is_none(),
        "the card opens the recap in the app"
    );
}

/// View recap, the Transcript chip and the thumbnail tell the shell to show the chat's Recap tab at the recording; the
/// transcript's card at its own file, with the part it stands for.
fn the_cards_open_the_recap(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let told: Rc<RefCell<Vec<ChatViewEvent>>> = Rc::default();
    let kept = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    for selector in [
        "view-recap-5",
        "recap-card-transcript-5",
        "recap-card-thumbnail-5",
        "view-recap-7",
    ] {
        let button = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked();
    }
    let opened = |recording: &str, open| ChatViewEvent::OpenRecap {
        recording: Some(recording.to_owned()),
        transcribed: None,
        open,
    };
    let told = told.borrow();
    assert_eq!(
        told[..3],
        [
            opened(LINK, RecapOpen::Recap),
            opened(LINK, RecapOpen::Transcript),
            opened(LINK, RecapOpen::Play),
        ]
    );
    let ChatViewEvent::OpenRecap {
        recording,
        transcribed: Some(part),
        open: RecapOpen::Recap,
    } = &told[3]
    else {
        panic!("the transcript's View recap opens its part: {:?}", told[3]);
    };
    assert_eq!(recording.as_deref(), Some(TRANSCRIPT_LINK));
    assert_eq!(part.sharing_link.as_deref(), Some(TRANSCRIPT_LINK));
}

#[test]
fn a_transcript_has_its_card_until_a_recording_of_its_call_holds_it() {
    let mut messages = call_messages();
    messages.truncate(4);
    let rig = rig_with(messages);
    let (view, cx) = chat_window(&rig);
    let kinds: Vec<&str> = items(&view, cx).iter().map(|item| item.body.kind()).collect();
    // the transcript the recording being saved will hold: its card, until the recording is ready
    assert_eq!(kinds, ["timed-event", "transcript", "timed-event"]);
    assert!(cx.update(|_, cx| view.read(cx).has_call_recaps()));
}

#[test]
fn the_keyboard_reaches_a_recaps_thumbnail_and_enter_plays_the_recording() {
    let rig = rig_with(call_messages());
    let (view, cx) = chat_window(&rig);
    let told: Rc<RefCell<Vec<ChatViewEvent>>> = Rc::default();
    let kept = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    cx.simulate_keystrokes("enter");
    draw(cx);
    let mut reached = false;
    for _ in 0..12 {
        if cx.debug_bounds("recap-card-ring-5").is_some() {
            reached = true;
            break;
        }
        // as the kit's root moves the keys in the app's window, which the test's has not
        cx.update(gpui::Window::focus_next);
        draw(cx);
    }
    assert!(reached, "Tab reaches the thumbnail, ringed");
    // down and up, as a person presses it: the thumbnail is clicked on the key's release
    let enter = gpui::Keystroke::parse("enter").unwrap();
    cx.simulate_event(gpui::KeyDownEvent {
        keystroke: enter.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(gpui::KeyUpEvent { keystroke: enter });
    cx.run_until_parked();
    assert_eq!(
        told.borrow().last(),
        Some(&ChatViewEvent::OpenRecap {
            recording: Some(LINK.to_owned()),
            transcribed: None,
            open: RecapOpen::Play,
        })
    );
}
