//! A meeting chat's tabs, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the process's
//! main thread), for their screenshots: the app's window over the fake engine (`common`), its store holding a made-up
//! meeting chat whose recording's card offers "View recap"; its Recap (two recordings: Part 1 with its transcript,
//! that transcript searched, Part 2, Part 1's recording playing as the engine streams it and paused; paused with its
//! captions on, its speed menu at 1.5x, its settings and its caption settings open; Part 1's recording downloading,
//! downloaded and failed), the Recap of a meeting transcribed and not recorded, and the Shared tab; Part 1's AI summary
//! offered, being made, failed, made, with the keyboard focus on its first point, and previewed before it is sent to the chat (made by a fake `claude`, or a
//! made-up summary kept for the run). A 1:1 chat's call captions saved on this computer: the Transcript chip under its
//! call's end, its Recap (the transcript, its AI summary made, its Delete transcript confirmation). The same chat's call
//! recorded: its recording's recap card under its saved line, its Recap at the recording (its Transcript saying there is
//! none, as Teams' does). A call of that chat transcribed and not recorded: its card, and its Recap as the card shows
//! it (its line alone, its Transcript saying there is none), and a stretch of the recorded call transcribed alone, a
//! part of its session with Teams' note.
//! The test runs itself again inside the sandbox once per page and theme; the parent screenshots each window into
//! `target/ui-shots/recap-*.png`. `RECAP_PAGES` (a comma list) picks pages.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
#[path = "../../common/mod.rs"]
mod common;
#[path = "../recap_fixtures.rs"]
mod fixtures;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents};
use gpui::{App, AppContext as _, Entity, Keystroke, Window};
use teams_core::ChatListing;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{ChatTab, Shell, ShellParts};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::video_player::{CaptionStyle, Menu, Page, players};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../fake_tool.rs"]
#[expect(
    dead_code,
    reason = "the shared synthetic tool also serves process-cancellation tests"
)]
mod fake_tool;
#[path = "../window_fixture.rs"]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "TEAMS_UI_RECAP_WINDOW_TEST";
/// Another count of Tab presses for the `summary-focus` page.
const FOCUS_TABS_ENV: &str = "RECAP_FOCUS_TABS";

/// Each page and the themes it is shot in.
const PAGES: [(&str, &[&str]); 31] = [
    ("call-chat", &[LIGHT_KEY, DARK_KEY]),
    ("call-transcribed-chat", &[LIGHT_KEY]),
    ("call-transcribed", &[LIGHT_KEY, DARK_KEY]),
    ("call-transcribed-part", &[LIGHT_KEY, DARK_KEY]),
    ("call-recap", &[LIGHT_KEY, DARK_KEY]),
    ("call-transcript", &[LIGHT_KEY]),
    ("saved-chat", &[LIGHT_KEY, DARK_KEY]),
    ("saved", &[LIGHT_KEY, DARK_KEY]),
    ("saved-summary", &[LIGHT_KEY]),
    ("saved-delete", &[LIGHT_KEY]),
    ("summary", &[LIGHT_KEY, DARK_KEY]),
    ("summary-focus", &[LIGHT_KEY, DARK_KEY]),
    ("summary-offer", &[LIGHT_KEY, DARK_KEY]),
    ("summary-running", &[LIGHT_KEY]),
    ("summary-failed", &[LIGHT_KEY]),
    ("summary-preview", &[LIGHT_KEY, DARK_KEY]),
    ("downloading", &[LIGHT_KEY, DARK_KEY]),
    ("downloaded", &[LIGHT_KEY]),
    ("download-failed", &[LIGHT_KEY]),
    ("chat", &[LIGHT_KEY]),
    ("recap", &[LIGHT_KEY, DARK_KEY]),
    ("playing", &[LIGHT_KEY, DARK_KEY]),
    ("paused", &[LIGHT_KEY]),
    ("captions", &[LIGHT_KEY, DARK_KEY]),
    ("speed", &[LIGHT_KEY, DARK_KEY]),
    ("settings", &[LIGHT_KEY, DARK_KEY]),
    ("caption-settings", &[LIGHT_KEY, DARK_KEY]),
    ("transcript", &[LIGHT_KEY, DARK_KEY]),
    ("part-2", &[LIGHT_KEY]),
    ("transcript-only", &[LIGHT_KEY]),
    ("shared", &[LIGHT_KEY, DARK_KEY]),
];

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

pub(super) fn main() -> ExitCode {
    let result = match std::env::var(CHILD_ENV) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("recap window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn shots() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("recap window")? else {
        return Ok(());
    };
    let only = std::env::var("RECAP_PAGES").ok();
    for (page, themes) in PAGES {
        if only
            .as_deref()
            .is_some_and(|only| !only.split(',').any(|wanted| wanted == page))
        {
            continue;
        }
        for theme in themes {
            shot(&sandbox, theme, page).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    Ok(())
}

fn shot(sandbox: &Sandbox, theme: &str, page: &str) -> SandboxResult {
    let mode = format!("{theme}:{page}");
    let tabs = std::env::var(FOCUS_TABS_ENV).unwrap_or_default();
    let mut env = vec![(CHILD_ENV, mode.as_str())];
    if !tabs.is_empty() {
        env.push((FOCUS_TABS_ENV, tabs.as_str()));
    }
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &env)?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready: {error:?}")))?;
    let name = if theme == DARK_KEY { "dark" } else { "light" };
    let file = format!("recap-{page}-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots()?.join(&file);
    fs::copy(png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}

/// The meeting chats, their messages, the people and the recaps MCPS would answer.
fn seed(core: &FakeCore) {
    let me = core.me().to_owned();
    for id in [fixtures::MEETING, fixtures::TRANSCRIPT_ONLY] {
        locked(&core.conversations).push(fixtures::conversation(id));
    }
    locked(&core.messages).insert(fixtures::MEETING.to_owned(), fixtures::messages(&me));
    let only: Vec<_> = fixtures::messages(&me)
        .into_iter()
        .filter(|message| message.message_type == "Event/Call")
        .map(|mut message| {
            fixtures::TRANSCRIPT_ONLY.clone_into(&mut message.conversation_id);
            message
        })
        .collect();
    locked(&core.messages).insert(fixtures::TRANSCRIPT_ONLY.to_owned(), only);
    locked(&core.names).insert(format!("8:orgid:{}", fixtures::ORGANIZER), "Ada Lab".to_owned());
    locked(&core.names).insert(me, "Ada Lab".to_owned());
    locked(&core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
    locked(&core.meeting_contents).insert(fixtures::TRANSCRIPT_ONLY.to_owned(), fixtures::transcribed());
    locked(&core.recordings).insert("ITEM1".to_owned());
    for item in ["ITEM1", "ITEM2", "ITEM9"] {
        locked(&core.transcripts).insert(fixtures::transcript_location(item), fixtures::VTT.to_owned());
        locked(&core.thumbnails).insert(fixtures::sharing_link(item), poster());
    }
    *locked(&core.listing) = ChatListing {
        favorites: None,
        empty: Vec::new(),
    };
}

/// The a–b chat (a 1:1 with Bo Lab): a few messages, then its call's end, whose captions are saved on this computer.
const SAVED_CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const BO: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

fn seed_saved(core: &FakeCore) {
    let me = core.me().to_owned();
    let minute = 60_000;
    let ended = fixtures::START + 12 * minute;
    locked(&core.conversations).push(teams_store::Conversation {
        id: SAVED_CHAT.to_owned(),
        kind: teams_store::ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(ended + minute),
    });
    locked(&core.members).insert(
        SAVED_CHAT.to_owned(),
        vec![teams_store::Member::new(&me, None), teams_store::Member::new(BO, None)],
    );
    locked(&core.names).insert(BO.to_owned(), "Bo Lab".to_owned());
    let message = |id: &str, sender: &str, kind: &str, content: &str, at: i64| teams_store::Message {
        conversation_id: SAVED_CHAT.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    };
    let call_end = format!(
        "<ended/><partlist alt=\"\" count=\"2\"><part identity=\"{BO}\"><name>{BO}</name>\
         <duration>600</duration></part><part identity=\"{me}\"><name>{me}</name><duration>600</duration></part>\
         </partlist>"
    );
    locked(&core.messages).insert(
        SAVED_CHAT.to_owned(),
        vec![
            message(
                "s1",
                BO,
                "RichText/Html",
                "<p>Got a minute for the release plan?</p>",
                fixtures::START - minute,
            ),
            message(
                "s2",
                &me,
                "RichText/Html",
                "<p>Sure, calling you now.</p>",
                fixtures::START - minute / 2,
            ),
            message("s3", BO, "Event/Call", &call_end, ended),
            message(
                "s4",
                BO,
                "RichText/Html",
                "<p>Thanks, that settles it.</p>",
                ended + minute,
            ),
        ],
    );
    seed_saved_transcript(core, &me);
}

/// The a–b chat's call recorded too (the call pages): its recording's start, stop and save in the chat, a recording
/// kept in a's OneDrive that MCPS lists by the chat's id with no transcript to read, as Teams lists a call's.
fn seed_call(core: &FakeCore) {
    seed_saved(core);
    let me = core.me().to_owned();
    let oid = me.trim_start_matches("8:orgid:").to_owned();
    let minute = 60_000;
    let at = fixtures::START + 3 * minute;
    let link = fixtures::sharing_link(CALL_ITEM);
    let recording = |status: &str, code: &str, file: bool| {
        let (href, item) = if file {
            (
                link.clone(),
                format!(
                    "<item type=\"onedriveForBusinessVideo\" uri=\"{link}\" driveId=\"b!drive1\" driveItemId=\"{CALL_ITEM}\" />"
                ),
            )
        } else {
            (String::new(), String::new())
        };
        format!(
            "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
             <Title>Call with Bo Lab</Title><a href=\"{href}\">Play</a><MeetingOrganizerId value=\"\" />\
             <RecordingInitiatorId value=\"{me}\" isAutoRecording=\"False\" />\
             <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{oid}\" />\
             <Identifiers><Id type=\"callId\" value=\"{CALL_ID}\" /></Identifiers>\
             <RecordingContent contentTypes=\"Recording+Transcript\" timestamp=\"2026-01-02T10:03:00Z\" \
             duration=\"0:00:51\">{item}</RecordingContent></URIObject>"
        )
    };
    let message = |id: &str, content: String, when: i64| teams_store::Message {
        conversation_id: SAVED_CHAT.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(me.clone()),
        sender_name: None,
        message_type: "RichText/Media_CallRecording".to_owned(),
        content,
        properties: None,
        arrival_time: when,
        compose_time: Some(when),
        edit_time: None,
        delete_time: None,
    };
    if let Some(messages) = locked(&core.messages).get_mut(SAVED_CHAT) {
        messages.push(message("c1", recording("Initial", "0", false), at));
        messages.push(message("c2", recording("ChunkFinished", "202", false), at + minute));
        messages.push(message("c3", recording("Success", "200", true), at + 2 * minute));
        messages.sort_by_key(|message| message.arrival_time);
    }
    locked(&core.meeting_contents).insert(
        SAVED_CHAT.to_owned(),
        vec![teams_proto::MeetingContent {
            kind: teams_proto::recap::ContentKind::Recording,
            location: link.clone(),
            call_id: Some(CALL_ID.to_owned()),
            drive_id: Some("b!drive1".to_owned()),
            item_id: Some(CALL_ITEM.to_owned()),
            start_ms: Some(at),
            end_ms: Some(at + 51_000),
            web_url: None,
            organizer_id: Some(oid),
            tenant_id: Some("00000000-0000-4000-8000-0000000000f0".to_owned()),
        }],
    );
    locked(&core.recordings).insert(CALL_ITEM.to_owned());
    locked(&core.thumbnails).insert(link, poster());
    locked(&core.expirations).insert(
        CALL_ITEM.to_owned(),
        chrono::Utc::now().timestamp_millis() + 119 * 24 * 3_600_000 + 3_600_000,
    );
}

/// The a–b chat's second call, transcribed and not recorded: its transcript's message and its file saved in a's
/// OneDrive, MCPS listing it with nothing to read; and a stretch of the recorded call transcribed before its recording
/// (`call-transcribed-part`).
fn seed_transcribed(core: &FakeCore) {
    let me = core.me().to_owned();
    let oid = me.trim_start_matches("8:orgid:").to_owned();
    let minute = 60_000;
    let at = fixtures::START + 40 * minute;
    let link = fixtures::sharing_link(TRANSCRIBED_ITEM);
    let saved = format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"Success\" code=\"200\" />\
         <Title>Call with Bo Lab</Title><a href=\"{link}\">Play</a><MeetingOrganizerId value=\"\" />\
         <RecordingInitiatorId value=\"{me}\" isAutoRecording=\"False\" />\
         <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{oid}\" />\
         <Identifiers><Id type=\"callId\" value=\"{TRANSCRIBED_CALL}\" /></Identifiers>\
         <RecordingContent contentTypes=\"Transcript\" timestamp=\"2026-01-02T10:40:00Z\" duration=\"0:00:58\">\
         <item type=\"onedriveForBusinessVideo\" uri=\"{link}\" driveId=\"b!drive1\" driveItemId=\"{TRANSCRIBED_ITEM}\" />\
         </RecordingContent></URIObject>"
    );
    let message = |id: &str, kind: &str, content: String, when: i64| teams_store::Message {
        conversation_id: SAVED_CHAT.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(me.clone()),
        sender_name: None,
        message_type: kind.to_owned(),
        content,
        properties: None,
        arrival_time: when,
        compose_time: Some(when),
        edit_time: None,
        delete_time: None,
    };
    if let Some(messages) = locked(&core.messages).get_mut(SAVED_CHAT) {
        let transcript = format!(r#"{{\"callId\":\"{TRANSCRIBED_CALL}\",\"isDeleted\":false}}"#);
        messages.push(message("t1", "RichText/Media_CallTranscript", transcript, at));
        messages.push(message("t2", "RichText/Media_CallRecording", saved, at + 2 * minute));
        messages.sort_by_key(|message| message.arrival_time);
    }
    let unread = |item: &str, call: &str, start: i64, end: i64| teams_proto::MeetingContent {
        kind: teams_proto::recap::ContentKind::UnreadTranscript,
        location: String::new(),
        call_id: Some(call.to_owned()),
        drive_id: Some("b!drive1".to_owned()),
        item_id: Some(item.to_owned()),
        start_ms: Some(start),
        end_ms: Some(end),
        web_url: Some(fixtures::sharing_link(item)),
        organizer_id: Some(oid.clone()),
        tenant_id: Some("00000000-0000-4000-8000-0000000000f0".to_owned()),
    };
    if let Some(contents) = locked(&core.meeting_contents).get_mut(SAVED_CHAT) {
        let recorded = fixtures::START + 3 * minute;
        contents.push(unread(TRANSCRIBED_ITEM, TRANSCRIBED_CALL, at, at + 58_000));
        contents.push(unread("ITEM8", CALL_ID, recorded - 2 * minute, recorded - minute));
    }
}

/// The a–b chat's call transcribed alone: its transcript's file and its call.
const TRANSCRIBED_ITEM: &str = "ITEM7";
const TRANSCRIBED_CALL: &str = "00000000-0000-4000-8000-0000000000c7";

/// The a–b call's recording: its item and its call.
const CALL_ITEM: &str = "ITEM5";
const CALL_ID: &str = "00000000-0000-4000-8000-0000000000c5";

/// The a–b call's captions saved on this computer: a's and Bo's lines over a minute.
fn seed_saved_transcript(core: &FakeCore, me: &str) {
    let minute = 60_000;
    let ended = fixtures::START + 12 * minute;
    let mut saved = locked(&core.saved_calls);
    let id = saved.write(teams_core::TranscriptWrite::Open {
        conversation: SAVED_CHAT.to_owned(),
        title: None,
        at: fixtures::START + 2 * minute,
        rejoin_ms: 300_000,
    });
    let said = [
        (
            me,
            "Ada Lab",
            5,
            "Hi Bo. Let's go through the release plan for Thursday.",
        ),
        (
            me,
            "Ada Lab",
            12,
            "The build passed overnight, so the only open item is the search fix.",
        ),
        (BO, "Bo Lab", 25, "I can finish the search fix by Wednesday afternoon."),
        (
            BO,
            "Bo Lab",
            34,
            "Then we test playback on the second recording on Thursday morning.",
        ),
        (
            me,
            "Ada Lab",
            48,
            "Good. I'll write the release notes and send them to the team.",
        ),
        (
            BO,
            "Bo Lab",
            61,
            "Should we keep the old player behind a switch for a week?",
        ),
    ];
    let lines = said
        .iter()
        .enumerate()
        .map(|(index, (speaker, name, second, text))| teams_store::TranscriptLine {
            utterance: format!("u{index}"),
            speaker: Some((*speaker).to_owned()),
            speaker_name: Some((*name).to_owned()),
            started_at: fixtures::START + 2 * minute + second * 1_000,
            ended_at: fixtures::START + 2 * minute + (second + 5) * 1_000,
            text: (*text).to_owned(),
        })
        .collect();
    saved.write(teams_core::TranscriptWrite::Lines { id, lines });
    saved.write(teams_core::TranscriptWrite::End { id, at: ended });
}

/// A recording's poster: a made-up frame, as Graph's thumbnail of the recording would be.
fn poster() -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    let frame = image::RgbaImage::from_fn(640, 360, |x, y| {
        let shade = u8::try_from((x / 4 + y / 4) % 200).unwrap_or(0);
        image::Rgba([20, 60 + shade / 2, 40 + shade / 3, 255])
    });
    if frame
        .write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .is_err()
    {
        bytes.clear();
    }
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        #[cfg(windows)]
        if !fixture::scene_activated(window) {
            after_frames(window, frames, done);
            window.refresh();
            return;
        }
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn child(mode: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let (theme, page) = mode.split_once(':').ok_or("bad child mode")?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let page = page.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, page, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

/// Part 1's transcript with a cue spoken in the made-up recording's 3 s, for its captions.
const CAPTIONED: &str = "WEBVTT\n\n\
    u1/1-0\n00:00:00.300 --> 00:00:03.500\n<v Ada Lab>Hello everyone, this is the weekly sync.\n\
    We look at the video player first.</v>\n\n";

/// The made-up recording's file, as Part 1's item names it.
const RECORDING_NAME: &str = "Weekly sync-20260102_100000-Meeting Recording.mp4";

/// The download pages' recording: Part 1's may be downloaded; downloading, its first 5 MiB of 12 MiB come and then
/// nothing more; downloaded, all of its 4.4 MB; failed, it is refused.
fn seed_download(core: &FakeCore, page: &str) {
    locked(&core.recording_files).insert(
        "ITEM1".to_owned(),
        teams_proto::RecordingAccess {
            can_download: true,
            name: Some(RECORDING_NAME.to_owned()),
            size: None,
        },
    );
    let size = match page {
        "downloading" => 12 * 1024 * 1024,
        "downloaded" => 4_659_019,
        _ => return,
    };
    locked(&core.file_bytes).insert(fixtures::sharing_link("ITEM1"), vec![0x5a; size]);
    if page == "downloading" {
        *locked(&core.stream_limit) = Some(5 * 1024 * 1024);
        *locked(&core.hold_streams_from) = Some(5 * 1024 * 1024);
    }
}

/// The download pages' file, in a folder of the run's own.
fn download_target() -> PathBuf {
    std::env::temp_dir()
        .join(format!("ngteams-recap-download-{}", std::process::id()))
        .join(RECORDING_NAME)
}

/// The summary pages' tool: a fake `claude` that never answers (being made) or is signed out (failed), in a folder of
/// the run's own; a made-up summary of Part 1 kept for the run (made, previewed).
fn seed_summary(page: &str, cx: &mut App) {
    use teams_ui::local_ai::{Backend, Tools};
    let body = match page {
        "summary-running" => "while read -r line; do :; done\n",
        "summary-failed" => {
            "read -r line\necho '{\"type\":\"result\",\"is_error\":true,\"result\":\"Not logged in · Please run /login\"}'\n"
        }
        "summary-offer" => "",
        "summary" | "summary-preview" | "summary-focus" => {
            teams_ui::recap_summary::keep(&fixtures::sharing_link("ITEM1"), &made_summary(), None, cx);
            return;
        }
        _ => return,
    };
    let folder = summary_tool_dir();
    let program = match fs::create_dir_all(&folder).and_then(|()| fake_tool::try_write(&folder, "claude", body)) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("the fake tool: {error}");
            return;
        }
    };
    cx.set_global(Tools::with(vec![(Backend::Claude, program)]));
}

fn summary_tool_dir() -> PathBuf {
    std::env::temp_dir().join(format!("ngteams-recap-summary-{}", std::process::id()))
}

/// A made-up summary of the made-up meeting, as the tool would make it.
fn made_summary() -> teams_ui::recap_summary::Made {
    use teams_ui::recap_summary::{Made, Point, Step, Summary, Topic};
    let point = |text: &str, at: u64| Point {
        text: text.to_owned(),
        at: Some(at),
    };
    Made {
        summary: Summary {
            purpose: "Walk through the new recap's video player and transcript before the release.".to_owned(),
            takeaways: vec![
                "The video player plays the recording, and the transcript follows it as it plays.".to_owned(),
                "Search finds every match in the transcript and counts them.".to_owned(),
                "The release waits for the search's last fixes.".to_owned(),
            ],
            topics: vec![
                Topic {
                    title: "Video player".to_owned(),
                    points: vec![
                        point("Ada opened the weekly sync with the video player first.", 7),
                        point("Bo confirmed the video plays and the transcript follows it.", 15),
                    ],
                },
                Topic {
                    title: "Transcript search".to_owned(),
                    points: vec![point(
                        "Ada showed the search finding every video named in the meeting.",
                        20,
                    )],
                },
            ],
            next_steps: vec![
                Step {
                    owner: Some("Ada Lab".to_owned()),
                    text: "Finish the search fixes by Friday.".to_owned(),
                    at: Some(20),
                },
                Step {
                    owner: Some("Bo Lab".to_owned()),
                    text: "Check playback on the second recording.".to_owned(),
                    at: Some(15),
                },
            ],
            questions: vec![point("Should the transcript follow the video when it is searched?", 22)],
        },
        tool: "claude".to_owned(),
        model: teams_ui::local_ai::Model::Recommended,
        made_at: fixtures::START + 3_600_000,
        sections: 1,
    }
}

/// The pages that play Part 1's recording, then pause it.
const PLAYED: [&str; 6] = ["playing", "paused", "captions", "speed", "settings", "caption-settings"];

fn open(theme: &'static str, page: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    seed(&core);
    if page == "captions" {
        let vtt = format!("{CAPTIONED}{}", fixtures::VTT.trim_start_matches("WEBVTT\n\n"));
        locked(&core.transcripts).insert(fixtures::transcript_location("ITEM1"), vtt);
        cx.set_global(CaptionStyle {
            on: true,
            ..CaptionStyle::default()
        });
    }
    seed_download(&core, &page);
    seed_summary(&page, cx);
    if page.starts_with("call") {
        seed_call(&core);
    }
    if page.starts_with("call-transcribed") {
        seed_transcribed(&core);
    }
    if page.starts_with("saved") {
        seed_saved(&core);
        if page == "saved-summary" {
            let key = teams_ui::call_transcript::location(core.me(), 1);
            teams_ui::recap_summary::keep(&key, &made_summary(), None, cx);
        }
    }
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let chat = if page == "transcript-only" {
        fixtures::TRANSCRIPT_ONLY
    } else if page.starts_with("saved") || page.starts_with("call") {
        SAVED_CHAT
    } else {
        fixtures::MEETING
    };
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        selected_chat: Some(chat.to_owned()),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, "ngteams-recap-test", "Recap".into(), parts)?;
    let root = teams_ui::window::content(window, cx).ok_or_else(|| io::Error::other("missing shell"))?;
    let shell = root
        .read(cx)
        .shell_in(cx)
        .cloned()
        .ok_or_else(|| io::Error::other("missing shell"))?;
    window.update(cx, |_, window, _| {
        after_frames(window, 30, move |window, cx| {
            step(&shell, &page, window, cx);
            after_frames(window, 30, move |window, cx| {
                // the pages with a menu or captions are paused sooner, in its 3 s
                let frames = match page.as_str() {
                    "playing" | "paused" => 60,
                    page if PLAYED.contains(&page) => 15,
                    _ => 30,
                };
                then(&shell, &page, window, cx);
                // a playing recording is shot a second on, its frames drawn as they come; a paused one with its
                // controls, which show while it is paused
                after_frames(window, frames, move |window, cx| {
                    last(&shell, &page, window, cx);
                });
            });
        });
    })?;
    mem::forget((bridge, events));
    Ok(())
}

/// The page's last step: ready to be shot, a recording paused first on the pages that show it paused, with the menu
/// they show open.
fn last(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    if page.starts_with("summary") {
        let _ = fs::remove_dir_all(summary_tool_dir());
    }
    if page.starts_with("download") {
        // the row stays as it is drawn; the made-up file goes
        if let Some(folder) = download_target().parent() {
            let _ = fs::remove_dir_all(folder);
        }
    }
    if !PLAYED.contains(&page) || page == "playing" {
        say("ready");
        return;
    }
    if let Some(recap) = shell.read(cx).recap_view().cloned() {
        recap.update(cx, teams_ui::recap_view::RecapView::pause);
    }
    let menu = match page {
        "speed" => Some(Menu::Speed),
        "settings" => Some(Menu::Settings(Page::Main)),
        "caption-settings" => Some(Menu::Settings(Page::Captions)),
        _ => None,
    };
    if let Some(player) = players(cx).into_iter().find(|player| player.read(cx).id() == "recap-0") {
        player.update(cx, |player, cx| {
            if page == "speed" {
                player.set_rate(1.5, cx);
            }
            player.show_menu(menu, cx);
        });
    }
    after_frames(window, 20, |_, _| say("ready"));
}

/// Opens the page's tab.
fn step(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    let tab = match page {
        "chat" | "saved-chat" | "call-chat" | "call-transcribed-chat" => ChatTab::Chat,
        "shared" => ChatTab::Shared,
        _ => ChatTab::Recap,
    };
    shell.update(cx, |shell, cx| shell.show_chat_tab(tab, window, cx));
}

/// The page's step once its tab shows.
fn then(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    let Some(recap) = shell.read(cx).recap_view().cloned() else {
        return;
    };
    recap.update(cx, |recap, cx| match page {
        "transcript" => {
            recap.find("video");
            recap.step_match(false, cx);
        }
        "part-2" => recap.select_part(1, cx),
        "saved" => recap.show_saved(1, cx),
        "call-recap" => recap.show_recording(fixtures::sharing_link(CALL_ITEM), cx),
        "call-transcript" => {
            recap.show_recording(fixtures::sharing_link(CALL_ITEM), cx);
            recap.show_transcript(cx);
        }
        // as the call transcribed alone's card's Transcript chip shows it
        "call-transcribed" => {
            recap.show_recording(fixtures::sharing_link(TRANSCRIBED_ITEM), cx);
            recap.show_transcript(cx);
        }
        "call-transcribed-part" => recap.show_recording(fixtures::sharing_link("ITEM8"), cx),
        "saved-summary" => {
            recap.show_saved(1, cx);
            recap.show_pane(teams_ui::recap_view::Pane::AiSummary, cx);
        }
        "saved-delete" => {
            recap.show_saved(1, cx);
            recap.ask_delete(cx);
        }
        "summary-offer" => recap.show_pane(teams_ui::recap_view::Pane::AiSummary, cx),
        "summary-running" | "summary-failed" => recap.summarize(cx),
        "summary-preview" => recap.preview_summary(cx),
        "downloading" | "downloaded" | "download-failed" => {
            recap.save_recording(fixtures::sharing_link("ITEM1"), download_target(), cx);
        }
        // the recording streamed and played from half a second in, shot as it plays
        page if PLAYED.contains(&page) => recap.play_from(Duration::from_millis(500), cx),
        _ => {}
    });
    if page == "summary-focus" {
        focus_first_point(window, cx);
    }
}

/// The Tab presses from the window's start to the summary's first point.
const TABS_TO_FIRST_POINT: usize = 16;

/// The focus moved on until it is on the summary's first point, the last step a Tab press, so that the focus shows as
/// the keyboard's; `RECAP_FOCUS_TABS` sets another count.
fn focus_first_point(window: &mut Window, cx: &mut App) {
    let tabs = std::env::var(FOCUS_TABS_ENV)
        .ok()
        .and_then(|tabs| tabs.parse().ok())
        .unwrap_or(TABS_TO_FIRST_POINT);
    for _ in 1..tabs {
        window.focus_next(cx);
    }
    if let Ok(tab) = Keystroke::parse("tab") {
        window.dispatch_keystroke(tab, cx);
    }
}
