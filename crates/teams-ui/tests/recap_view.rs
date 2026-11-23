//! A meeting chat's tabs on GPUI's test context, over the fake engine (`common`) and a made-up meeting: the header's
//! Chat, Shared and Recap; "View recap" on a recording's card showing the Recap; the recap's parts and
//! transcript (read when shown, searched, a paragraph clicked playing the recording from there); a recap or transcript
//! that can't be read and its Try again; another chat opening on Chat. Every id, name and word is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared fake engine of the view tests: these use part of it")]
mod common;
#[path = "support/recap_fixtures.rs"]
mod fixtures;
#[path = "support/recap_summary_view.rs"]
mod summary;
#[path = "support/recap_summary_qa.rs"]
mod summary_qa;

use std::sync::Arc;
use std::time::Duration;

use common::Rig;
use gpui::{
    AppContext as _, Entity, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext,
    point, px, size,
};
use teams_ui::ViewTrace;
use teams_ui::recap_view::RecapView;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{ChatTab, Main, Shell, ShellParts, ShowCalendar};
use teams_ui::window::KitRoot;

fn locked<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The rig with the made-up meeting chats; the recorded one's recap and transcripts answered unless `unread`.
fn rig(unread: bool) -> Rig {
    let rig = Rig::start();
    let me = rig.core.me.to_string();
    for id in [fixtures::MEETING, fixtures::TRANSCRIPT_ONLY] {
        locked(&rig.core.conversations).push(fixtures::conversation(id));
    }
    locked(&rig.core.messages).insert(fixtures::MEETING.to_owned(), fixtures::messages(&me));
    locked(&rig.core.names).insert(format!("8:orgid:{}", fixtures::ORGANIZER), "Ada Lab".to_owned());
    if !unread {
        locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
        for item in ["ITEM1", "ITEM2"] {
            locked(&rig.core.transcripts).insert(fixtures::transcript_location(item), fixtures::VTT.to_owned());
        }
    }
    locked(&rig.core.meeting_contents).insert(fixtures::TRANSCRIPT_ONLY.to_owned(), fixtures::transcribed());
    rig
}

/// The shell's parts, `chat` the chat kept selected (opened as the shell is made).
fn parts(rig: &Rig, chat: &str) -> ShellParts {
    let settings = Settings {
        selected_chat: Some(chat.to_owned()),
        ..Settings::default()
    };
    rig.cx.update(teams_ui::window::init);
    ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace: Arc::new(ViewTrace::off()),
    }
}

fn shell_window(rig: &Rig, chat: &str) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts(rig, chat);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1600.0), px(1000.0)));
    draw(cx);
    (shell, cx)
}

/// The shell in the kit's root, as the app opens it: an action dispatched while the focus is on nothing drawn starts
/// from that root, which handles none of the shell's.
fn kit_window(rig: &Rig, chat: &str) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts(rig, chat);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let shell = cx.new(|cx| Shell::new(parts, window, cx));
        KitRoot::new(shell, window, cx)
    });
    cx.simulate_resize(size(px(1600.0), px(1000.0)));
    draw(cx);
    let shell = cx.update(|_, cx| root.read(cx).view().clone().downcast::<Shell>().unwrap());
    (shell, cx)
}

/// `ShowCalendar` dispatched as Ctrl+4 or `ngteamsctl action` sends it (to the focus as last drawn) shows the calendar.
fn an_action_runs(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    draw(cx);
    cx.dispatch_action(ShowCalendar);
    draw(cx);
    assert!(
        cx.update(|_, cx| *shell.read(cx).main() == Main::Calendar),
        "the action reached the shell"
    );
}

/// Whether the meeting chat's compose box has the keyboard.
fn compose_focused(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        let chat = shell.read(cx).chat().unwrap().read(cx);
        chat.compose().read(cx).focused(window, cx)
    })
}

/// A meeting chat's Recap and Shared tabs replace its chat, compose box and all: the keyboard moves off the box as one
/// shows, comes back to it on Chat, and is given to the shell (not the box) on the chat opened again on such a tab or
/// handed the keyboard on an account switched back to. An action sent then runs, not lost on a box not drawn.
#[test]
fn the_keyboard_is_never_left_on_a_compose_box_a_meeting_tab_replaces() {
    let rig = rig(false);
    let (shell, cx) = kit_window(&rig, fixtures::MEETING);
    assert!(compose_focused(&shell, cx), "a meeting chat opens on Chat, typed into");
    click(cx, "chat-tab-shared");
    assert!(!compose_focused(&shell, cx));
    an_action_runs(&shell, cx);
    // opened again, on the Shared tab it was left on
    let open = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            shell.update(cx, |shell, cx| {
                shell.open_chat(fixtures::MEETING.to_owned(), window, cx);
            });
        });
        draw(cx);
        assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Shared);
    };
    open(cx);
    an_action_runs(&shell, cx);
    // the keyboard handed to the shell (an account switched back to) while the box had it
    open(cx);
    cx.update(|window, cx| {
        let chat = shell.read(cx).chat().unwrap().read(cx).compose().clone();
        chat.update(cx, |compose, cx| compose.focus(window, cx));
        shell.update(cx, |shell, cx| shell.take_focus(window, cx));
    });
    assert!(!compose_focused(&shell, cx));
    an_action_runs(&shell, cx);
    // back on Chat: typed into again
    open(cx);
    click(cx, "chat-tab-chat");
    assert!(compose_focused(&shell, cx));
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

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(at.center(), Modifiers::none());
    draw(cx);
}

fn recap(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<RecapView> {
    cx.update(|_, cx| shell.read(cx).recap_view().cloned())
        .expect("the recap is made")
}

fn asked(rig: &Rig, what: &str) -> usize {
    locked(&rig.core.asked).iter().filter(|asked| **asked == what).count()
}

#[test]
fn view_recap_on_the_meetings_recap_card_shows_the_recap_tab() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    assert!(
        cx.debug_bounds("chat-tab-chat shown").is_some(),
        "a meeting chat opens on Chat"
    );
    assert!(cx.debug_bounds("chat-tab-shared").is_some());
    assert!(cx.debug_bounds("chat-tab-recap").is_some());
    assert_eq!(
        asked(&rig, "meeting_contents"),
        0,
        "nothing read before the Recap shows"
    );
    click(cx, "view-recap-6");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    // the card's recording is the part shown: the first
    let shown = recap(&shell, cx);
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 0);
    click(cx, "recap-part-1");
    click(cx, "recap-part-0");
    assert!(cx.debug_bounds("chat-tab-recap shown").is_some());
    assert!(cx.debug_bounds("recap-view").is_some());
    assert_eq!(asked(&rig, "meeting_contents"), 1);
    // two recordings: Part 1 shown, with Share, Watch in browser and the files the chat shared
    for selector in [
        "recap-part-0",
        "recap-part-1",
        "recap-share",
        "recap-watch",
        "recap-more",
        "recap-file-0",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    assert!(
        cx.debug_bounds("recap-transcript-card").is_some(),
        "the transcript beside the recording"
    );
    assert_eq!(
        asked(&rig, "meeting_transcript"),
        2,
        "each part shown (Part 1 from the card, then Part 2) has its transcript read"
    );
    click(cx, "recap-part-1");
    the_card_s_transcript_and_thumbnail_open_part_1(&shell, &shown, cx);
}

/// Back to Chat: the card's Transcript chip shows Part 1's transcript, its thumbnail plays Part 1's recording.
fn the_card_s_transcript_and_thumbnail_open_part_1(
    shell: &Entity<Shell>,
    shown: &Entity<RecapView>,
    cx: &mut VisualTestContext,
) {
    click(cx, "chat-tab-chat");
    click(cx, "recap-card-transcript-6");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 0);
    assert!(
        cx.debug_bounds("recap-ai-notice").is_some(),
        "the transcript from its top"
    );
    click(cx, "chat-tab-chat");
    click(cx, "recap-card-thumbnail-6");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    assert!(
        cx.debug_bounds("video-player-recap-0").is_some(),
        "the recording's player made to play"
    );
}

#[test]
fn the_transcript_is_read_searched_and_plays_the_recording_from_a_paragraph() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    let view = recap(&shell, cx);
    assert_eq!(asked(&rig, "meeting_transcript"), 1);
    for selector in [
        "recap-ai-notice",
        "recap-started",
        "recap-turn-0",
        "recap-turn-2",
        "recap-download",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    // a transcript with a recording says who started it, not who stopped it
    assert!(cx.debug_bounds("recap-stopped").is_none());
    cx.update(|_, cx| view.update(cx, |view, _| view.find("video")));
    draw(cx);
    assert!(cx.debug_bounds("recap-search-count 1/4").is_some());
    click(cx, "recap-search-next");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search().map(teams_ui::recap::Search::label)),
        Some("2/4".to_owned())
    );
    // a paragraph clicked: the recording's player is made and asked to play from there
    click(cx, "recap-paragraph-1-0");
    assert!(cx.debug_bounds("video-player-recap-0").is_some());
    // Part 2: its own transcript read
    click(cx, "recap-part-1");
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 1);
    assert_eq!(asked(&rig, "meeting_transcript"), 2);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).search().cloned()),
        None,
        "a part shown starts unsearched"
    );
}

#[test]
fn a_transcript_alone_shows_no_player_and_who_stopped_it() {
    let rig = rig(false);
    locked(&rig.core.transcripts).insert(fixtures::transcript_location("ITEM9"), fixtures::VTT.to_owned());
    let (shell, cx) = shell_window(&rig, fixtures::TRANSCRIPT_ONLY);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    assert!(cx.debug_bounds("recap-recording").is_none());
    assert!(cx.debug_bounds("recap-watch").is_none());
    assert!(
        cx.debug_bounds("recap-part-0").is_none(),
        "one part shows no parts' row"
    );
    assert!(cx.debug_bounds("recap-no-files").is_some());
    assert!(cx.debug_bounds("recap-stopped").is_some());
}

/// A made-up 1:1 chat with a's call recorded and transcribed: Teams' chat names its recording, MCPS lists it by the
/// chat's id with no transcript to read (as a 1:1 call's lists it).
const CALL_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

fn call_rig() -> Rig {
    let rig = rig(false);
    let mut chat = fixtures::conversation(CALL_CHAT);
    chat.kind = teams_store::ConversationKind::OneToOne;
    chat.topic = None;
    locked(&rig.core.conversations).push(chat);
    let link = fixtures::sharing_link("ITEM5");
    let ready = format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"Success\" code=\"200\" />\
         <Title>Call with Bo Lab</Title><a href=\"{link}\">Play</a><MeetingOrganizerId value=\"\" />\
         <RecordingInitiatorId value=\"8:orgid:{0}\" isAutoRecording=\"False\" />\
         <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{0}\" />\
         <Identifiers><Id type=\"callId\" value=\"00000000-0000-4000-8000-0000000000c5\" /></Identifiers>\
         <RecordingContent contentTypes=\"Recording+Transcript\" timestamp=\"2026-01-02T10:00:00Z\" duration=\"0:00:51\">\
         <item type=\"onedriveForBusinessVideo\" uri=\"{link}\" driveId=\"b!drive1\" driveItemId=\"ITEM5\" />\
         </RecordingContent></URIObject>",
        fixtures::ORGANIZER
    );
    let mut message = fixtures::messages(&rig.core.me).remove(0);
    message.conversation_id = CALL_CHAT.to_owned();
    message.id = "51".to_owned();
    message.content = ready;
    locked(&rig.core.messages).insert(CALL_CHAT.to_owned(), vec![message]);
    let mut contents = fixtures::recorded();
    contents.retain(|content| content.kind == teams_proto::recap::ContentKind::Recording);
    contents.truncate(1);
    contents[0].location = link;
    contents[0].item_id = Some("ITEM5".to_owned());
    locked(&rig.core.meeting_contents).insert(CALL_CHAT.to_owned(), contents);
    rig
}

#[test]
fn a_calls_recording_in_a_1_1_chat_opens_in_its_recap_tab() {
    let rig = call_rig();
    let (shell, cx) = shell_window(&rig, CALL_CHAT);
    // the chat holds a call's recording: it has the tabs, and its card no web link
    assert!(cx.debug_bounds("chat-tab-recap").is_some());
    assert!(cx.debug_bounds("open-on-the-web-51").is_none());
    click(cx, "view-recap-51");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    assert_eq!(asked(&rig, "meeting_contents"), 1, "read by the chat's own id");
    let shown = recap(&shell, cx);
    let parts = cx
        .update(|_, cx| shown.read(cx).parts().map(<[_]>::to_vec))
        .expect("read");
    assert_eq!(parts.len(), 1);
    assert_eq!(
        parts[0].recording.as_deref(),
        Some(fixtures::sharing_link("ITEM5").as_str())
    );
    for selector in ["recap-recording", "recap-watch", "recap-more"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    // Teams lists no transcript to read for it: its Transcript says there is none, as Teams' Recap does
    click(cx, "recap-pane-transcript");
    assert!(
        cx.debug_bounds(Box::leak(
            format!("recap-state {}", teams_ui::recap_view::NO_TRANSCRIPT_YET).into_boxed_str()
        ))
        .is_some()
    );
    assert_eq!(asked(&rig, "meeting_transcript"), 0);
    // the card's Transcript chip opens the same pane
    click(cx, "chat-tab-chat");
    click(cx, "recap-card-transcript-51");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
}

#[test]
fn a_1_1_chats_calls_are_sessions_of_its_recap_the_latest_shown() {
    let rig = call_rig();
    // a later call recorded too: MCPS lists both by the chat's id
    {
        let mut contents = locked(&rig.core.meeting_contents);
        let calls = contents.get_mut(CALL_CHAT).expect("the call's");
        let mut later = calls[0].clone();
        later.location = fixtures::sharing_link("ITEM6");
        later.item_id = Some("ITEM6".to_owned());
        later.call_id = Some("00000000-0000-4000-8000-0000000000c6".to_owned());
        later.start_ms = later.start_ms.map(|start| start + 3_600_000);
        later.end_ms = later.end_ms.map(|end| end + 3_600_000);
        calls[0].call_id = Some("00000000-0000-4000-8000-0000000000c5".to_owned());
        calls.push(later);
    }
    let (shell, cx) = shell_window(&rig, CALL_CHAT);
    click(cx, "chat-tab-recap");
    let shown = recap(&shell, cx);
    // Teams' Recap opens at the latest call, each call a session of the menu, no parts beside it
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 1);
    assert!(cx.debug_bounds("recap-part-0").is_none());
    assert!(cx.debug_bounds("recap-part-1").is_none());
    let face = cx.debug_bounds("recap-session").expect("the session's box");
    assert!(
        face.size.width <= gpui::px(teams_ui::recap_view::SESSION_WIDTH),
        "hugs its line up to Teams' width: {face:?}"
    );
    click(cx, "recap-session");
    assert!(cx.debug_bounds("recap-session-row-1").is_some(), "the latest");
    click(cx, "recap-session-row-0");
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 0);
}

/// The call chat's second call, transcribed and not recorded: its transcript's message (`71`) and its file saved in a's
/// OneDrive (`72`); listed by MCPS (`listed`) with nothing to read, as a 1:1 call's transcript is.
fn transcribed_call(rig: &Rig, listed: bool) {
    let call = "00000000-0000-4000-8000-0000000000c7";
    let link = fixtures::sharing_link("ITEM7");
    let saved = format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"Success\" code=\"200\" />\
         <Title>Call with Bo Lab</Title><a href=\"{link}\">Play</a><MeetingOrganizerId value=\"\" />\
         <RecordingInitiatorId value=\"8:orgid:{0}\" isAutoRecording=\"False\" />\
         <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{0}\" />\
         <Identifiers><Id type=\"callId\" value=\"{call}\" /></Identifiers>\
         <RecordingContent contentTypes=\"Transcript\" timestamp=\"2026-01-02T11:00:00Z\" duration=\"0:00:58\">\
         <item type=\"onedriveForBusinessVideo\" uri=\"{link}\" driveId=\"b!drive1\" driveItemId=\"ITEM7\" />\
         </RecordingContent></URIObject>",
        fixtures::ORGANIZER
    );
    {
        let mut messages = locked(&rig.core.messages);
        let chat = messages.get_mut(CALL_CHAT).expect("the call chat's");
        let mut transcript = chat[0].clone();
        transcript.id = "71".to_owned();
        transcript.message_type = "RichText/Media_CallTranscript".to_owned();
        transcript.content = format!(r#"{{\"callId\":\"{call}\",\"isDeleted\":false}}"#);
        transcript.arrival_time += 3_600_000;
        let mut file = chat[0].clone();
        file.id = "72".to_owned();
        file.content = saved;
        file.arrival_time += 3_660_000;
        chat.push(transcript);
        chat.push(file);
    }
    if listed {
        let mut contents = locked(&rig.core.meeting_contents);
        let calls = contents.get_mut(CALL_CHAT).expect("the call's");
        let mut unread = calls[0].clone();
        unread.kind = teams_proto::recap::ContentKind::UnreadTranscript;
        unread.location = String::new();
        unread.web_url = Some(link);
        unread.item_id = Some("ITEM7".to_owned());
        unread.call_id = Some(call.to_owned());
        unread.start_ms = unread.start_ms.map(|start| start + 3_600_000);
        unread.end_ms = unread.end_ms.map(|end| end + 3_600_000);
        calls.push(unread);
    }
}

/// The Recap opened by itself: it lists the recording alone, as Teams' does.
fn the_recording_alone_is_listed(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<RecapView> {
    click(cx, "chat-tab-recap");
    let shown = recap(shell, cx);
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 0);
    click(cx, "recap-session");
    assert!(cx.debug_bounds("recap-session-row-0").is_some());
    assert!(cx.debug_bounds("recap-session-row-1").is_none(), "not listed");
    cx.simulate_keystrokes("escape");
    draw(cx);
    shown
}

#[test]
fn a_call_transcribed_alone_isnt_a_session_until_its_card_shows_it_with_no_transcript() {
    let rig = call_rig();
    transcribed_call(&rig, true);
    let (shell, cx) = shell_window(&rig, CALL_CHAT);
    let shown = the_recording_alone_is_listed(&shell, cx);
    // its card's View recap shows it: its line alone, Share and More options, no player, its Transcript none
    click(cx, "chat-tab-chat");
    click(cx, "view-recap-71");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 1);
    assert!(cx.debug_bounds("recap-session-line").is_some());
    assert!(cx.debug_bounds("recap-session").is_none(), "no menu, as Teams' line");
    for selector in ["recap-share", "recap-more", "recap-files"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    for selector in ["recap-recording", "recap-watch", "recap-transcribed-note"] {
        assert!(cx.debug_bounds(selector).is_none(), "{selector} not drawn");
    }
    click(cx, "recap-pane-transcript");
    assert!(
        cx.debug_bounds(Box::leak(
            format!("recap-state {}", teams_ui::recap_view::NO_TRANSCRIPT_YET).into_boxed_str()
        ))
        .is_some()
    );
    assert_eq!(asked(&rig, "meeting_transcript"), 0);
    // the recording's own card still opens the recording
    click(cx, "chat-tab-chat");
    click(cx, "view-recap-51");
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 0);
    assert!(cx.debug_bounds("recap-session").is_some());
}

#[test]
fn a_call_transcribed_alone_mcps_doesnt_list_is_shown_from_its_card() {
    let rig = call_rig();
    transcribed_call(&rig, false);
    let (shell, cx) = shell_window(&rig, CALL_CHAT);
    click(cx, "recap-card-transcript-71");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    let shown = recap(&shell, cx);
    let parts = cx
        .update(|_, cx| shown.read(cx).parts().map(<[_]>::to_vec))
        .expect("read");
    assert_eq!(parts.len(), 2, "the card's part beside the recording");
    assert!(parts[1].is_unread_transcript());
    assert_eq!(parts[1].item_id.as_deref(), Some("ITEM7"));
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 1);
    assert!(cx.debug_bounds("recap-session-line").is_some());
    assert!(
        cx.debug_bounds("recap-pane-transcript shown").is_some(),
        "the chip shows its Transcript"
    );
}

#[test]
fn a_part_transcribed_only_among_its_calls_parts_says_so() {
    let rig = call_rig();
    // the recorded call transcribed alone first, then recorded: its card's View recap shows both its parts
    let call = "00000000-0000-4000-8000-0000000000c5".to_owned();
    {
        let mut contents = locked(&rig.core.meeting_contents);
        let calls = contents.get_mut(CALL_CHAT).expect("the call's");
        calls[0].call_id = Some(call.clone());
        let mut unread = calls[0].clone();
        unread.kind = teams_proto::recap::ContentKind::UnreadTranscript;
        unread.location = String::new();
        unread.web_url = Some(fixtures::sharing_link("ITEM8"));
        unread.item_id = Some("ITEM8".to_owned());
        unread.start_ms = unread.start_ms.map(|start| start - 120_000);
        unread.end_ms = unread.end_ms.map(|end| end - 120_000);
        calls.push(unread);
    }
    let (shell, cx) = shell_window(&rig, CALL_CHAT);
    click(cx, "chat-tab-recap");
    let shown = recap(&shell, cx);
    assert!(cx.debug_bounds("recap-part-0").is_none(), "the recording alone");
    cx.update(|_, cx| {
        shown.update(cx, |recap, cx| {
            recap.show_recording(fixtures::sharing_link("ITEM8"), cx);
        });
    });
    draw(cx);
    assert_eq!(cx.update(|_, cx| shown.read(cx).selected()), 0);
    for selector in [
        "recap-part-0",
        "recap-part-1",
        "recap-session",
        "recap-transcribed-note",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    click(cx, "recap-transcribed-close");
    assert!(cx.debug_bounds("recap-transcribed-note").is_none());
    click(cx, "recap-part-1");
    assert!(cx.debug_bounds("recap-recording").is_some());
    assert!(cx.debug_bounds("recap-transcribed-note").is_none());
}

#[test]
fn a_recap_that_cant_be_read_says_so_and_reads_again() {
    let rig = rig(true);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    assert!(cx.debug_bounds("recap-state The recap couldn't be loaded.").is_some());
    assert!(cx.debug_bounds("recap-open-web").is_some());
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
    click(cx, "recap-retry");
    assert_eq!(asked(&rig, "meeting_contents"), 2);
    assert!(cx.debug_bounds("recap-part-0").is_some());
    // the transcript can't be read: said, and read again on Try again
    assert!(
        cx.debug_bounds("recap-state The transcript couldn't be loaded.")
            .is_some()
    );
    locked(&rig.core.transcripts).insert(fixtures::transcript_location("ITEM1"), fixtures::VTT.to_owned());
    click(cx, "recap-transcript-retry");
    assert!(cx.debug_bounds("recap-turn-0").is_some());
}

#[test]
fn the_shared_tab_lists_what_the_chat_shared_and_another_chat_opens_on_chat() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-shared");
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Shared);
    // the recordings, a link and the file
    assert!(cx.debug_bounds("shared-row-3").is_some());
    click(cx, "shared-filter-files");
    assert!(cx.debug_bounds("shared-row-1").is_none(), "only the file");
    click(cx, "shared-filter-links");
    assert!(cx.debug_bounds("shared-row-2").is_some());
    assert!(cx.debug_bounds("shared-row-3").is_none());
    // another chat: Chat, and the other chat's tabs let go
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(fixtures::TRANSCRIPT_ONLY.to_owned(), window, cx);
        });
    });
    draw(cx);
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Chat);
    assert!(cx.update(|_, cx| shell.read(cx).shared_view().is_none()));
}

fn reopen(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_chat(fixtures::MEETING.to_owned(), window, cx);
        });
    });
    draw(cx);
}

fn chat_shown(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| shell.read(cx).chat().map(|chat| chat.read(cx).is_shown()))
        .expect("a chat view")
}

#[test]
fn the_same_chat_opened_again_keeps_its_tab_its_chat_unseen() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    for (tab, selector) in [(ChatTab::Recap, "chat-tab-recap"), (ChatTab::Shared, "chat-tab-shared")] {
        click(cx, selector);
        reopen(&shell, cx);
        assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), tab);
        // the chat under the tab is neither shown (no read receipts go) nor typed into
        assert!(!chat_shown(&shell, cx), "{tab:?}");
        click(cx, "chat-tab-chat");
        assert!(chat_shown(&shell, cx));
    }
}

#[test]
fn a_refresh_keeps_the_recording_shown_when_an_earlier_one_went() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    click(cx, "recap-part-1");
    let view = recap(&shell, cx);
    let shown = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let view = view.read(cx);
            let part = view.parts().and_then(|parts| parts.get(view.selected()).cloned());
            part.and_then(|part| part.item_id)
        })
    };
    assert_eq!(shown(cx).as_deref(), Some("ITEM2"));
    // the first recording deleted elsewhere, then Refresh: the second is still the one shown, now Part 1
    let left: Vec<_> = fixtures::recorded()
        .into_iter()
        .filter(|content| content.item_id.as_deref() != Some("ITEM1"))
        .collect();
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), left);
    cx.update(|_, cx| view.update(cx, RecapView::read));
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).selected()), 0);
    assert_eq!(shown(cx).as_deref(), Some("ITEM2"));
}

#[test]
fn a_recap_that_held_nothing_is_read_again_when_shown_again() {
    let rig = rig(false);
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), Vec::new());
    let (_shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    assert!(cx.debug_bounds("recap-part-0").is_none());
    assert_eq!(asked(&rig, "meeting_contents"), 1);
    // the recording saved since: the tab shown again reads it
    locked(&rig.core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
    click(cx, "chat-tab-chat");
    click(cx, "chat-tab-recap");
    assert_eq!(asked(&rig, "meeting_contents"), 2);
    assert!(cx.debug_bounds("recap-part-0").is_some());
    // a recap that held something isn't read again
    click(cx, "chat-tab-chat");
    click(cx, "chat-tab-recap");
    assert_eq!(asked(&rig, "meeting_contents"), 2);
}

#[test]
fn a_narrow_recap_keeps_the_player_controls_and_menus_inside_the_picture() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    for width in [1600.0, 1024.0, 1280.0] {
        cx.simulate_resize(size(px(width), px(720.0)));
        draw(cx);
        let picture = cx.debug_bounds("video-screen-recap-0").expect("recording picture");
        let content = cx.debug_bounds("recap-content").expect("recap content");
        let session = cx.debug_bounds("recap-session-face").expect("session label");
        let part = cx.debug_bounds("recap-part-0").expect("first part");
        assert!(!session.intersects(&part), "session label overlaps its part at {width}");
        assert!(content.contains(&picture.origin));
        assert!(picture.right() <= content.right());
        let files = cx.debug_bounds("recap-files").expect("shared files card");
        assert!(files.size.width <= px(400.0), "wide recaps retain Teams' sidebar width");
        for (button, menu) in [
            ("video-speed-recap-0", "video-speed-menu"),
            ("video-settings-recap-0", "video-settings-menu"),
        ] {
            let control = cx.debug_bounds(button).expect("player control");
            assert!(picture.contains(&control.center()), "{button} reachable at {width}");
            click(cx, button);
            let panel = cx.debug_bounds(menu).expect("open player menu");
            assert!(picture.contains(&panel.origin), "{menu} left/top at {width}");
            assert!(panel.right() <= picture.right(), "{menu} right at {width}");
            assert!(panel.bottom() <= picture.bottom(), "{menu} bottom at {width}");
            click(cx, button);
        }
    }
}

#[test]
fn a_narrow_recap_scrolls_to_the_transcript_below_the_recording() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    cx.simulate_resize(size(px(1024.0), px(720.0)));
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    let content = cx.debug_bounds("recap-content").expect("recap content");
    cx.simulate_event(ScrollWheelEvent {
        position: content.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-600.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    draw(cx);
    let search = cx.debug_bounds("recap-search").expect("transcript search");
    assert!(
        content.contains(&search.center()),
        "transcript search remains reachable"
    );
    let paragraph = cx.debug_bounds("recap-turn-0").expect("transcript paragraph");
    assert!(
        content.contains(&paragraph.center()),
        "transcript text remains reachable"
    );
}

#[test]
fn each_part_s_recording_has_its_transcript_as_captions_named_by_its_language() {
    let rig = rig(false);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.show_chat_tab(ChatTab::Recap, window, cx)));
    draw(cx);
    // the transcript and its language read: the player's CC, its menu naming the language
    assert_eq!(asked(&rig, "transcript_language"), 1);
    assert!(cx.debug_bounds("video-captions-recap-0").is_some());
    assert!(cx.debug_bounds("video-speed-recap-0").is_some());
    assert!(cx.debug_bounds("video-settings-recap-0").is_some());
    click(cx, "video-captions-recap-0");
    assert!(cx.debug_bounds("video-captions-menu").is_some());
    let player = |cx: &mut VisualTestContext, id: &str| {
        cx.update(|_, cx| {
            teams_ui::video_player::players(cx)
                .into_iter()
                .find(|player| player.read(cx).id() == id)
        })
        .expect("the part's player")
    };
    let first = player(cx, "recap-0");
    assert!(cx.update(|_, cx| first.read(cx).captions().is_some()));
    assert_eq!(
        cx.update(|_, cx| first.read(cx).captions().map(teams_ui::video_player::Captions::label)),
        Some("English (United States)".to_owned())
    );
    click(cx, "video-captions-on");
    assert!(cx.update(|_, cx| teams_ui::video_player::CaptionStyle::current(cx).on));
    // Part 2: its own transcript (other words), its own captions, still on
    locked(&rig.core.transcripts).insert(
        fixtures::transcript_location("ITEM2"),
        fixtures::VTT.replace("Today.", "Part two."),
    );
    click(cx, "recap-part-1");
    assert_eq!(asked(&rig, "transcript_language"), 2);
    let second = player(cx, "recap-1");
    let said = |cx: &mut VisualTestContext, player: &Entity<teams_ui::video_player::VideoPlayer>| {
        cx.update(|_, cx| {
            player
                .read(cx)
                .captions()
                .and_then(|captions| captions.at(Duration::from_secs(4)).map(str::to_owned))
        })
    };
    assert_eq!(said(cx, &second).as_deref(), Some("Part two."));
    assert_eq!(said(cx, &first).as_deref(), Some("Today."));
    assert!(cx.update(|_, cx| teams_ui::video_player::CaptionStyle::current(cx).on));
}

/// The made-up recording's file as Part 1's item names it.
const RECORDING_NAME: &str = "Weekly sync-20260102_100000-Meeting Recording.mp4";

/// The rig with Part 1's recording downloadable (`may` false: blocked), its bytes `bytes`.
fn downloadable(may: bool, bytes: &[u8]) -> Rig {
    let rig = rig(false);
    locked(&rig.core.recording_files).insert(
        "ITEM1".to_owned(),
        teams_proto::RecordingAccess {
            can_download: may,
            name: Some(RECORDING_NAME.to_owned()),
            size: Some(bytes.len() as u64),
        },
    );
    locked(&rig.core.file_bytes).insert(fixtures::sharing_link("ITEM1"), bytes.to_vec());
    rig
}

/// Made-up bytes standing for a recording.
fn recording_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|at| u8::try_from(at % 251).unwrap_or(0)).collect()
}

#[test]
fn more_options_downloads_the_recording_where_chosen_with_its_progress_and_end() {
    let bytes = recording_bytes(300_000);
    let rig = downloadable(true, &bytes);
    // streams that break off every 70 000 bytes: each opened again where the file stands
    *locked(&rig.core.stream_limit) = Some(70_000);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    assert_eq!(asked(&rig, "recording_file"), 1, "the shown part's file is read");
    let view = recap(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).downloadable(cx)),
        Some((fixtures::sharing_link("ITEM1"), RECORDING_NAME.to_owned()))
    );
    click(cx, "recap-more");
    assert!(cx.debug_bounds("recap-more-download").is_some(), "Download offered");
    click(cx, "recap-more-download");
    assert!(rig.cx.did_prompt_for_new_path());
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("nested").join("recording.mp4");
    rig.cx.simulate_new_path_selection(|_| Some(target.clone()));
    draw(cx);
    assert_eq!(std::fs::read(&target).unwrap(), bytes, "the whole recording, in order");
    assert_eq!(asked(&rig, "file_stream"), 5);
    assert!(cx.debug_bounds("recap-download-0 saved").is_some());
    assert!(cx.debug_bounds("recap-download-show-0").is_some());
    assert!(!teams_ui::recording_download::part_path(&target).exists());
    // the download outlives the tab and the chat: shown again when the recap is
    click(cx, "chat-tab-chat");
    click(cx, "chat-tab-recap");
    assert!(cx.debug_bounds("recap-download-0 saved").is_some());
    click(cx, "recap-download-dismiss-0");
    assert!(cx.debug_bounds("recap-download-0 saved").is_none());
    assert!(target.exists(), "dismissing keeps the file");
}

#[test]
fn a_download_shows_how_far_it_got_and_cancelling_leaves_nothing() {
    let bytes = recording_bytes(200_000);
    let rig = downloadable(true, &bytes);
    // the first 50 000 bytes come, then nothing more
    *locked(&rig.core.stream_limit) = Some(50_000);
    *locked(&rig.core.hold_streams_from) = Some(50_000);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    let view = recap(&shell, cx);
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("recording.mp4");
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.save_recording(fixtures::sharing_link("ITEM1"), target.clone(), cx);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("recap-download-0 running").is_some());
    let download = cx
        .update(|_, cx| teams_ui::recording_download::download_of(&fixtures::sharing_link("ITEM1"), cx))
        .expect("the download");
    assert!(matches!(
        cx.update(|_, cx| download.read(cx).progress().clone()),
        teams_ui::recording_download::Progress::Running { .. }
    ));
    assert!(teams_ui::recording_download::part_path(&target).exists());
    // a running download isn't offered again
    assert_eq!(cx.update(|_, cx| view.read(cx).downloadable(cx)), None);
    click(cx, "recap-download-cancel-0");
    assert!(cx.debug_bounds("recap-download-0 running").is_none());
    assert!(!teams_ui::recording_download::part_path(&target).exists());
    assert!(!target.exists());
    // offered again once cancelled
    assert!(cx.update(|_, cx| view.read(cx).downloadable(cx)).is_some());
}

/// A cancelled download's job can still be in its last write (a long sync to the disk): until the job ends, a new
/// start to its place does not write a second `.part` file beside it, and the recap offers no Try again, which would
/// replace the file of another download without a question.
#[test]
fn a_download_started_again_before_its_cancelled_job_ended_is_busy_and_offers_no_try_again() {
    let bytes = recording_bytes(200_000);
    let rig = downloadable(true, &bytes);
    *locked(&rig.core.stream_limit) = Some(50_000);
    *locked(&rig.core.hold_streams_from) = Some(50_000);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    let view = recap(&shell, cx);
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("recording.mp4");
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.save_recording(fixtures::sharing_link("ITEM1"), target.clone(), cx);
        });
    });
    draw(cx);
    let download = cx
        .update(|_, cx| teams_ui::recording_download::download_of(&fixtures::sharing_link("ITEM1"), cx))
        .expect("the download");
    assert!(teams_ui::recording_download::part_path(&target).exists());
    // cancelled, and started again before its job was dropped
    let progress = cx.update(|_, cx| {
        download.update(cx, |download, cx| {
            download.cancel(cx);
            download.start(cx);
            download.progress().clone()
        })
    });
    assert_eq!(
        progress,
        teams_ui::recording_download::Progress::Failed(teams_ui::recording_download::Failure::Busy)
    );
    draw(cx);
    assert!(cx.debug_bounds("recap-download-0 failed").is_some());
    assert!(cx.debug_bounds("recap-download-retry-0").is_none(), "no Try again");
    // the job removed its `.part` file as it ended
    assert!(!teams_ui::recording_download::part_path(&target).exists());
    assert!(!target.exists());
    // the job ended: the next start runs
    let running = cx.update(|_, cx| {
        download.update(cx, |download, cx| {
            download.start(cx);
            download.progress().running()
        })
    });
    assert!(running);
}

#[test]
fn a_recording_the_account_may_not_download_offers_no_download() {
    let rig = downloadable(false, b"made-up bytes");
    let (_shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    assert_eq!(asked(&rig, "recording_file"), 1);
    click(cx, "recap-more");
    assert!(cx.debug_bounds("recap-more-refresh").is_some(), "the menu is open");
    assert!(cx.debug_bounds("recap-more-download").is_none());
    // Part 2's file can't be read: no Download either, and it is read again with the recap
    cx.simulate_keystrokes("escape");
    click(cx, "recap-part-1");
    assert_eq!(asked(&rig, "recording_file"), 2);
    click(cx, "recap-more");
    assert!(cx.debug_bounds("recap-more-download").is_none());
}

#[test]
fn a_download_that_fails_says_why_and_tries_again() {
    let bytes = recording_bytes(10_000);
    let rig = downloadable(true, &bytes);
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    click(cx, "chat-tab-recap");
    let view = recap(&shell, cx);
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("recording.mp4");
    // the recording refused at its download link
    let bytes_kept = locked(&rig.core.file_bytes).remove(&fixtures::sharing_link("ITEM1"));
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.save_recording(fixtures::sharing_link("ITEM1"), target.clone(), cx);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("recap-download-0 failed").is_some());
    assert!(!teams_ui::recording_download::part_path(&target).exists());
    // there again: Try again saves it
    locked(&rig.core.file_bytes).insert(fixtures::sharing_link("ITEM1"), bytes_kept.unwrap());
    click(cx, "recap-download-retry-0");
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert!(cx.debug_bounds("recap-download-0 saved").is_some());
}

#[test]
fn a_cloud_without_recaps_shows_no_recap_tab_and_its_card_opens_none() {
    let rig = rig(false);
    *locked(&rig.core.capabilities) = Some(teams_core::Capabilities {
        recap: false,
        ..teams_core::Capabilities::ALL
    });
    let (shell, cx) = shell_window(&rig, fixtures::MEETING);
    assert!(cx.debug_bounds("chat-tab-chat shown").is_some());
    assert!(cx.debug_bounds("chat-tab-shared").is_some());
    assert!(cx.debug_bounds("chat-tab-recap").is_none());
    shell.update_in(cx, |shell, window, cx| shell.show_chat_tab(ChatTab::Recap, window, cx));
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| shell.read(cx).chat_tab()), ChatTab::Chat);
    assert_eq!(asked(&rig, "meeting_contents"), 0);
}
