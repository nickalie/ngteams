//! Every call's live captions saved on this computer ([`teams_ui::call_transcript`]) on GPUI's test context, over the
//! fake engine (`common`): the setting (off by default, read and written); a connected call's captions turned on for
//! the saving while their pane stays hidden, shown and hidden again without the captions stopping; each final result
//! kept by utterance as it comes (an interim one not), the call's end kept, a rejoin going on in the same transcript;
//! a 1:1 invitation's call kept in its 1:1 chat, the test call not at all; a start that failed tried again; the setting
//! turned off mid-call stopping the captions it alone ran. Afterwards the call's end row in its chat links the
//! transcript ("Saved on this computer"), which opens in the chat's Recap tab with its speakers and the note that it is
//! the user's own, and is deleted from there. The pure parts: locations, parts merged with Teams' (Teams' transcript
//! wins), the `WebVTT` written and read back, the end rows linked. Every id, name and word is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;
#[path = "common/notices.rs"]
#[expect(
    dead_code,
    reason = "the notification server of the shared notices helper; these tests use its chats alone"
)]
mod notices;

use std::sync::Arc;
use std::time::Duration;

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use notices::chat;
use teams_core::{CallAction, CallCaptions, CallId, CallStatus, Caption, CaptionPhase, End, Event, RegistrationReport};
use teams_proto::Mri;
use teams_proto::recap::RecapPart;
use teams_proto::transcript::{read_vtt, turns};
use teams_store::{CallTranscript, ConversationKind, Member, Message, TranscriptLine};
use teams_ui::ViewTrace;
use teams_ui::call_transcript::{self, attach, cues, local_id, location, merged, utterance, vtt};
use teams_ui::call_window::CallWindows;
use teams_ui::calls::Ask;
use teams_ui::recap_view::RecapView;
use teams_ui::settings::{Settings, SettingsModel, parse, render};
use teams_ui::shell::{ChatTab, Shell, ShellParts};

const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const WORDS: &str = "Private synthetic caption words.";

// --- the pure parts ---

#[test]
fn the_setting_is_off_by_default_and_read_and_written() {
    assert!(
        !Settings::default().save_call_captions,
        "nothing is saved until the user asks"
    );
    let (read, problems) = parse("save_call_captions = true").expect("TOML");
    assert!(read.save_call_captions);
    assert!(problems.is_empty());
    assert!(render(&read).contains("save_call_captions = true"));
    let (read, problems) = parse("save_call_captions = \"yes\"").expect("TOML");
    assert!(!read.save_call_captions, "a bad value keeps the default");
    assert_eq!(problems.len(), 1);
}

#[test]
fn a_saved_transcript_s_location_names_its_account_and_id() {
    let at = location(LAB_A, 42);
    assert!(at.starts_with(call_transcript::LOCAL));
    assert_eq!(local_id(&at), Some(42));
    assert_eq!(local_id("https://example.test/transcripts/42/content"), None);
    assert_ne!(
        location(LAB_A, 42),
        location(LAB_B, 42),
        "another account's is another summary"
    );
}

fn saved(id: i64, start: i64, end: i64) -> CallTranscript {
    CallTranscript {
        id,
        conversation_id: AB.to_owned(),
        title: None,
        started_at: start,
        ended_at: end,
        lines: 1,
    }
}

fn teams_part(transcript: bool, span: Option<(i64, i64)>) -> RecapPart {
    RecapPart {
        recording: Some("https://example.test/recording".to_owned()),
        transcript: transcript.then(|| "https://example.test/transcript".to_owned()),
        sharing_link: None,
        call_id: None,
        drive_id: None,
        item_id: None,
        start_ms: span.map(|(start, _)| start),
        end_ms: span.map(|(_, end)| end),
        organizer_id: None,
        tenant_id: None,
    }
}

#[test]
fn teams_own_transcript_of_the_same_time_wins_over_a_saved_one() {
    let ours = |id, start, end| call_transcript::part(LAB_A, &saved(id, start, end));
    // a recording without a transcript leaves ours, by its start
    let parts = merged(
        vec![teams_part(false, Some((5_000_000, 6_000_000)))],
        vec![ours(1, 1_000_000, 2_000_000)],
    );
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].transcript.as_deref().and_then(local_id), Some(1));
    // Teams transcribed the same call: its own wins
    let parts = merged(
        vec![teams_part(true, Some((1_100_000, 1_900_000)))],
        vec![ours(1, 1_000_000, 2_000_000), ours(2, 9_000_000, 9_500_000)],
    );
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].transcript.as_deref().and_then(local_id), None);
    assert_eq!(parts[1].transcript.as_deref().and_then(local_id), Some(2));
    // a Teams transcript that doesn't say its time wins over every one of ours
    let parts = merged(vec![teams_part(true, None)], vec![ours(1, 1_000_000, 2_000_000)]);
    assert_eq!(parts.len(), 1);
}

fn line(utterance: &str, name: Option<&str>, at: i64, text: &str) -> TranscriptLine {
    TranscriptLine {
        utterance: utterance.to_owned(),
        speaker: Some(LAB_B.to_owned()),
        speaker_name: name.map(str::to_owned),
        started_at: at,
        ended_at: at + 2_500,
        text: text.to_owned(),
    }
}

#[test]
fn saved_lines_are_written_as_teams_transcripts_are_and_read_back_by_speaker() {
    let lines = [
        line("u1", Some("Lab <Bee>"), 1_010_000, "Fish & chips < 5."),
        line("u2", Some("Lab <Bee>"), 1_015_000, "Second."),
        line("u3", None, 1_020_000, "  "),
        line("u4", Some("Lab Sea"), 3_725_000, "Third."),
    ];
    let cues = cues(1_000_000, &lines);
    assert_eq!(cues.len(), 3, "a line without words is left out");
    let written = vtt(&cues);
    assert!(written.starts_with("WEBVTT\n\n1\n00:00:10.000 --> 00:00:12.500\n<v Lab &lt;Bee&gt;>"));
    assert!(written.contains("00:45:25.000 --> 00:45:27.500"));
    let read = read_vtt(&written).expect("a WebVTT file");
    assert_eq!(read[0].speaker.as_deref(), Some("Lab <Bee>"));
    assert_eq!(read[0].text, "Fish & chips < 5.");
    let turns = turns(&read);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].paragraphs.len(), 2);
    assert_eq!(turns[1].start, Duration::from_secs(2725));
    // a line said before the call's start (a rejoin's clock) starts at its start
    let early = cues_of_early();
    assert_eq!(early[0].start, Duration::ZERO);
    assert!(vtt(&[]).starts_with("WEBVTT"));
}

fn cues_of_early() -> Vec<teams_proto::transcript::Cue> {
    let mut early = line("u1", None, 900_000, "Early.");
    early.speaker = None;
    let cues = cues(1_000_000, &[early]);
    assert_eq!(cues[0].speaker, None);
    assert!(!vtt(&cues).contains("<v"));
    cues
}

#[test]
fn each_saved_transcript_is_linked_from_the_call_end_row_nearest_its_end() {
    let transcripts = [saved(1, 1_000_000, 2_000_000), saved(2, 1_500_000, 2_006_000)];
    let ends = [
        ("far".to_owned(), 2_000_000 + 10 * 60 * 1000),
        ("near".to_owned(), 2_005_000),
        ("other".to_owned(), 2_100_000),
    ];
    let linked = attach(&transcripts, &ends);
    // both are nearest "near": the nearer one keeps it
    assert_eq!(linked.get("near"), Some(&2));
    assert!(!linked.contains_key("far"));
    assert_eq!(attach(&transcripts[..1], &ends[..1]).len(), 0, "too far from its end");
    assert_eq!(attach(&[], &ends).len(), 0);
}

#[test]
fn an_utterance_is_kept_by_its_call_speaker_track_and_audio_time() {
    let mut caption = caption("synthetic/1", WORDS, true);
    let call = CallId::for_tests(3);
    assert_eq!(utterance(call, &caption), format!("3/{LAB_B}/en-us/123"));
    caption.audio_timestamp = 0;
    assert_eq!(utterance(call, &caption), "3/synthetic/1");
}

// --- in the app ---

fn caption(id: &str, text: &str, final_result: bool) -> Caption {
    Caption {
        id: id.into(),
        speaker: Some(LAB_B.into()),
        display_name: Some("Lab Bee".into()),
        text: text.into(),
        final_result,
        spoken_language: "en-us".into(),
        track_language: "en-us".into(),
        audio_timestamp: 123,
        duration: 10,
    }
}

/// The rig of a build that shows calls, a's store holding the a–b chat (and its call's end row at `ended_ms` when
/// given), the kit set up.
fn calling(ended_ms: Option<i64>) -> Rig {
    let rig = Rig::start_with(|cx| teams_ui::Models::with_calls(Some(Mri::new(ECHO)), cx));
    rig.core
        .conversations
        .lock()
        .unwrap()
        .push(chat(AB, ConversationKind::OneToOne, None));
    rig.core
        .members
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    if let Some(at) = ended_ms {
        let ended = format!(
            "<ended/><partlist alt=\"\" count=\"1\"><part identity=\"{LAB_B}\"><name>{LAB_B}</name>\
             <duration>30</duration></part></partlist>"
        );
        rig.core
            .messages
            .lock()
            .unwrap()
            .entry(AB.to_owned())
            .or_default()
            .push(Message {
                message_type: "Event/Call".to_owned(),
                ..notices::message(AB, "ended-1", LAB_B, &ended, at)
            });
    }
    rig.cx.update(teams_ui::window::init);
    rig
}

/// The shell in a window of its own on the a–b chat, saving every call's captions as `saving` says, with the call
/// windows; the trace written.
fn app(
    rig: &Rig,
    saving: bool,
) -> (
    Entity<Shell>,
    &'static mut VisualTestContext,
    Entity<CallWindows>,
    Written,
) {
    let written = Written::default();
    let trace = Arc::new(ViewTrace::new(written.clone()));
    let settings = Settings {
        selected_chat: Some(AB.to_owned()),
        save_call_captions: saving,
        ..Settings::default()
    };
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace,
    };
    let windows = rig.cx.update(|cx| {
        let windows = cx.new(|cx| CallWindows::new("ngteams-test", cx));
        windows.update(cx, |windows, cx| windows.add(parts.clone(), cx));
        windows
    });
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    draw(cx);
    rig.send(Event::CallingRegistration(RegistrationReport::Registered));
    (shell, cx, windows, written)
}

fn stage(rig: &Rig, windows: &Entity<CallWindows>) -> &'static mut VisualTestContext {
    rig.cx.run_until_parked();
    let window = rig
        .cx
        .read(|cx| windows.read(cx).window())
        .expect("the call window is open");
    let cx = Box::leak(Box::new(VisualTestContext::from_window(window.into(), &rig.cx)));
    draw(cx);
    cx
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn drawn(cx: &mut VisualTestContext, selector: &str) -> bool {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    draw(cx);
    let found = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn status(rig: &Rig, number: u64, status: CallStatus) {
    rig.send(Event::Call {
        call: CallId::for_tests(number),
        status,
    });
}

fn captions(rig: &Rig, number: u64, phase: CaptionPhase, lines: Vec<Caption>) {
    rig.send(Event::CallCaptions {
        call: CallId::for_tests(number),
        captions: CallCaptions {
            phase,
            spoken_language: "en-us".into(),
            error: None,
            lines,
        },
    });
}

/// The caption actions asked so far: whether each turned them on.
fn caption_asks(rig: &Rig) -> Vec<bool> {
    rig.core
        .call_actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            CallAction::Captions { enabled, .. } => Some(*enabled),
            _ => None,
        })
        .collect()
}

/// The saved transcripts: each with its lines' words.
fn kept(rig: &Rig) -> Vec<(CallTranscript, Vec<String>)> {
    let saved = rig.core.saved_calls.lock().unwrap();
    saved
        .transcripts
        .iter()
        .map(|transcript| {
            let words = saved
                .lines
                .get(&transcript.id)
                .map(|lines| lines.iter().map(|line| line.text.clone()).collect())
                .unwrap_or_default();
            (transcript.clone(), words)
        })
        .collect()
}

/// The saved transcript `id` shown in the Recap tab, deleted after its confirmation (not after its Cancel).
fn deleted(rig: &Rig, shell: &Entity<Shell>, cx: &mut VisualTestContext, id: i64) {
    let recap = rig
        .cx
        .read(|cx| shell.read(cx).recap_view().cloned())
        .expect("the recap is made");
    assert!(rig.cx.read(|cx| recap.read(cx).shows_saved()));
    rig.cx.update(|cx| recap.update(cx, RecapView::ask_delete));
    click(cx, "recap-delete-cancel");
    assert_eq!(kept(rig).len(), 1);
    rig.cx.update(|cx| recap.update(cx, RecapView::ask_delete));
    click(cx, "recap-delete-confirm");
    assert!(kept(rig).iter().all(|(transcript, _)| transcript.id != id));
    assert!(!drawn(cx, "recap-saved-here"));
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// The call's captions as they come: a final result kept, an interim one not, another result of the first replacing
/// its words; the pane shown and hidden again without the captions stopping.
fn saved_as_they_come(rig: &Rig, window: &mut VisualTestContext) {
    captions(
        rig,
        1,
        CaptionPhase::On,
        vec![caption("s/1", WORDS, true), caption("s/2", "Still being said", false)],
    );
    assert!(!drawn(window, "call-captions"), "the pane stays hidden");
    let saved = kept(rig);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].0.conversation_id, AB);
    assert_eq!(saved[0].1, [WORDS], "only a final result is kept");
    // the interim one is final now, another result of the first replaces its words
    let mut second = caption("s/3", "Second synthetic sentence.", true);
    second.audio_timestamp = 456;
    captions(
        rig,
        1,
        CaptionPhase::On,
        vec![
            caption("s/4", "Private synthetic caption words, corrected.", true),
            second,
        ],
    );
    assert_eq!(
        kept(rig)[0].1,
        [
            "Private synthetic caption words, corrected.",
            "Second synthetic sentence."
        ]
    );
    // shown and hidden again: only the pane, the captions keep running
    window.simulate_keystrokes("alt-shift-c");
    assert!(drawn(window, "call-captions"));
    window.simulate_keystrokes("alt-shift-c");
    assert!(!drawn(window, "call-captions"));
    assert_eq!(caption_asks(rig), [true]);
}

#[test]
fn a_call_s_captions_are_saved_with_their_pane_hidden_and_shown_after_in_its_chat() {
    let rig = calling(Some(now_ms()));
    let (shell, cx, windows, written) = app(&rig, true);
    assert!(!drawn(cx, "chat-tab-recap"), "nothing saved yet: no tabs");
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    assert_eq!(caption_asks(&rig), [true], "turned on for the saving");
    saved_as_they_come(&rig, stage(&rig, &windows));
    let ended_before = kept(&rig)[0].0.ended_at;
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    assert!(kept(&rig)[0].0.ended_at >= ended_before);
    // the call's end row in the chat links it; its tabs show
    let id = kept(&rig)[0].0.id;
    assert!(drawn(cx, "saved-transcript-mark"));
    assert!(drawn(cx, "chat-tab-recap"));
    click(cx, "saved-transcript-ended-1");
    assert_eq!(rig.cx.read(|cx| shell.read(cx).chat_tab()), ChatTab::Recap);
    assert!(drawn(cx, "recap-saved-here"));
    assert!(drawn(cx, "recap-saved-notice"));
    assert!(drawn(cx, "recap-turn-0"));
    assert!(drawn(cx, "recap-paragraph-0-1"));
    deleted(&rig, &shell, cx, id);
    // nothing said was traced
    assert!(
        !String::from_utf8(written.0.lock().unwrap().clone())
            .unwrap()
            .contains("synthetic caption")
    );
}

#[test]
fn a_rejoin_goes_on_in_the_same_transcript_and_an_invitation_s_call_is_kept_in_its_chat() {
    let rig = calling(None);
    let (_shell, cx, _windows, _) = app(&rig, true);
    // b calls a 1:1: the invitation names no chat
    rig.send(Event::IncomingCall(teams_core::IncomingCall {
        call: CallId::for_tests(1),
        caller: Mri::new(LAB_B),
        caller_name: Some("Lab Bee".to_owned()),
        group_chat: None,
        meeting: None,
    }));
    status(&rig, 1, CallStatus::Connected);
    captions(&rig, 1, CaptionPhase::On, vec![caption("s/1", WORDS, true)]);
    status(&rig, 1, CallStatus::Ended(End::Failed("dropped")));
    rig.send(Event::IncomingCall(teams_core::IncomingCall {
        call: CallId::for_tests(2),
        caller: Mri::new(LAB_B),
        caller_name: None,
        group_chat: None,
        meeting: None,
    }));
    status(&rig, 2, CallStatus::Connected);
    assert_eq!(caption_asks(&rig), [true, true], "each call's captions turned on");
    captions(&rig, 2, CaptionPhase::On, vec![caption("s/1", "Back again.", true)]);
    let saved = kept(&rig);
    assert_eq!(saved.len(), 1, "the rejoin goes on in the same transcript");
    assert_eq!(saved[0].0.conversation_id, AB, "the invitation's 1:1 chat");
    assert_eq!(saved[0].1.len(), 2, "each call's utterance kept apart");
    // the chat holds no end row of the call: the app's own shows once the call is over, never while it runs
    let row = format!("saved-transcript-saved-call-{}", saved[0].0.id);
    assert!(drawn(cx, "chat-tab-recap"));
    assert!(!drawn(cx, &row));
    status(&rig, 2, CallStatus::Ended(End::HungUp));
    assert!(drawn(cx, &row));
    assert!(drawn(cx, "saved-transcript-mark"));
}

#[test]
fn the_test_call_and_a_build_with_the_setting_off_save_nothing() {
    let rig = calling(None);
    let (_shell, cx, _windows, _) = app(&rig, false);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    assert!(caption_asks(&rig).is_empty());
    captions(&rig, 1, CaptionPhase::On, vec![caption("s/1", WORDS, true)]);
    assert!(kept(&rig).is_empty());
    assert!(
        rig.cx.read(|cx| rig.models.calls.read(cx).captions_shown()),
        "shown as before"
    );

    let rig = calling(None);
    let (shell, cx, _windows, _) = app(&rig, true);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
    status(&rig, 1, CallStatus::Connected);
    assert!(caption_asks(&rig).is_empty(), "the test call's captions aren't saved");
}

#[test]
fn a_failed_start_is_tried_again_and_the_setting_turned_off_stops_what_it_alone_ran() {
    let rig = calling(None);
    let (shell, cx, _windows, _) = app(&rig, true);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    captions(&rig, 1, CaptionPhase::Failed, Vec::new());
    assert_eq!(caption_asks(&rig), [true]);
    rig.cx.executor().advance_clock(Duration::from_secs(21));
    rig.cx.run_until_parked();
    assert_eq!(caption_asks(&rig), [true, true], "tried again");
    captions(&rig, 1, CaptionPhase::On, Vec::new());
    let settings = rig.cx.read(|cx| shell.read(cx).settings().clone());
    rig.cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.save_call_captions = false, cx);
        });
    });
    rig.cx.run_until_parked();
    assert_eq!(
        caption_asks(&rig),
        [true, true, false],
        "stopped: only the saving ran them"
    );
    assert!(!rig.cx.read(|cx| rig.models.calls.read(cx).saving_captions()));
}
