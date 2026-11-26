//! QA of task 868 (round 1): the call's Transcript pane fills with what is said while a transcription runs, in the
//! app's own call window on the mock. The steps run after the tour's own (`NGTEAMS_TOUR_FILTER` picks them by their
//! `qa-transcript-` names). Each step tells the made-up call's events to a's models as the engine would, with made-up
//! speech as final caption results; the child then says what the models and the stage hold ([`said`]) and the parent
//! checks it ([`check`]), with the view trace's caption asks ([`Asks`]). The parent also presses keys and turns the
//! wheel over the pane ([`after_shot`]); the shots show the pane.

use std::fs;
use std::io::BufRead;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use gpui::{App, Entity, Window};
use teams_mock::{ABC_CHAT, LAB_A, LAB_B, LAB_C};
use teams_proto::Mri;
use teams_ui::call_stage::CallStage;
use teams_ui::shell::Shell;
use ui_sandbox::input::keys;
use ui_sandbox::{Running, Sandbox, SandboxResult};

use super::Step;

/// The start of each line the child says of a step.
pub(super) const PREFIX: &str = "qa-transcript ";
/// The start of the steps' names.
const STEP_PREFIX: &str = "qa-transcript-";
/// The QA group call's number, and the meeting's after it.
const GROUP: u64 = 21;
const MEETING: u64 = 22;
/// A made-up meeting chat.
const MEETING_CHAT: &str = "19:meeting_c3ludGhldGljLXFhLTg2OA@thread.v2";
/// How many lines the long run says: more than the 50 the captions keep.
const MANY: u64 = 60;

/// The step after which the parent presses keys, and the one after which it turns the wheel over the pane.
const KEYS: &str = "qa-transcript-keys";
const WHEEL: &str = "qa-transcript-keys-after";

pub(super) const STEPS: [Step; 17] = [
    // a new group call; the account starts a transcription alone: its pane opens empty, the captions held, hidden
    ("qa-transcript-group", |shell, _, cx| {
        fixed_theme(shell, Some(teams_ui::theme::LIGHT_KEY), cx);
        for old in 1..=6 {
            ended(shell, old, cx);
        }
        call(shell, GROUP, ABC_CHAT, cx);
        transcribing(shell, GROUP, Some(LAB_A), cx);
    }),
    // 60 lines of three people, some runs of one speaker: all kept, the pane at its newest line
    ("qa-transcript-many", |shell, _, cx| {
        for index in 1..=MANY {
            say_line(shell, GROUP, index, cx);
        }
    }),
    // the pane closed, five more lines said meanwhile
    ("qa-transcript-closed", |shell, _, cx| {
        with_stage(cx, |stage, cx| stage.show_transcript(false, cx));
        for index in MANY + 1..=MANY + 5 {
            say_line(shell, GROUP, index, cx);
        }
    }),
    // opened again: every line, the newest at its end
    ("qa-transcript-reopened", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_transcript(true, cx));
    }),
    // the call window focused, for the keys the parent presses after the shot
    (KEYS, |_, _, cx| {
        with_stage_window(cx, |_, window, _| window.activate_window());
    }),
    // after Tab and Page Up (the parent turns the wheel over the pane after this shot)
    (WHEEL, |_, _, _| {}),
    // after the wheel
    ("qa-transcript-wheel", |_, _, _| {}),
    // the user shows the live captions (Alt+Shift+C): they show, the transcript keeps filling
    ("qa-transcript-captions-shown", |shell, window, cx| {
        toggle_captions(window, cx);
        say_line(shell, GROUP, MANY + 6, cx);
    }),
    // the user hides them again: they keep running for the transcript, which keeps filling
    ("qa-transcript-captions-hidden", |shell, window, cx| {
        toggle_captions(window, cx);
        say_line(shell, GROUP, MANY + 7, cx);
    }),
    // the transcription stops: its captions are asked to stop, their pane still hidden; the lines stay
    ("qa-transcript-stopped", |shell, _, cx| {
        transcribing(shell, GROUP, None, cx);
    }),
    // a meeting next, transcribed by someone else: the transcript starts empty, the captions held again
    ("qa-transcript-meeting", |shell, _, cx| {
        captions_off(shell, GROUP, cx);
        ended(shell, GROUP, cx);
        call(shell, MEETING, MEETING_CHAT, cx);
        transcribing(shell, MEETING, Some(LAB_B), cx);
    }),
    // speech in the meeting, its pane opened: Teams Light
    ("qa-transcript-meeting-speech", |shell, _, cx| {
        for index in 1..=6 {
            say_line(shell, MEETING, index, cx);
        }
        with_stage(cx, |stage, cx| stage.show_transcript(true, cx));
    }),
    // and in Teams Dark
    ("qa-transcript-meeting-dark", |shell, _, cx| {
        fixed_theme(shell, Some(teams_ui::theme::DARK_KEY), cx);
    }),
    // the pane translated into German (a fake `claude`): the lines said after show their German
    ("qa-transcript-translated", |shell, _, cx| {
        fixed_theme(shell, Some(teams_ui::theme::LIGHT_KEY), cx);
        translate_to(shell, Some("de"), cx);
        say_text(
            shell,
            MEETING,
            7,
            (LAB_B, "Lab Bee"),
            "Good morning, everyone. Let's go through the release plan.",
            cx,
        );
    }),
    // the user shows the captions, then the transcription stops: the captions stay, the lines stay
    ("qa-transcript-kept-shown", |shell, window, cx| {
        translate_to(shell, None, cx);
        toggle_captions(window, cx);
        transcribing(shell, MEETING, None, cx);
    }),
    // "Save live captions" on, the captions hidden, a transcription again and its stop: the saving keeps them
    ("qa-transcript-kept-saved", |shell, window, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.save_call_captions = true, cx);
        });
        toggle_captions(window, cx);
        transcribing(shell, MEETING, Some(LAB_B), cx);
        say_line(shell, MEETING, 8, cx);
    }),
    // that transcription stops: the saving keeps the captions, so nothing asks them to stop; the lines stay
    ("qa-transcript-saved-stopped", |shell, _, cx| {
        transcribing(shell, MEETING, None, cx);
    }),
];

/// Whether `name` is one of these steps.
pub(super) fn ours(name: &str) -> bool {
    name.starts_with(STEP_PREFIX)
}

/// What the child says after the step `name`: the call, the transcript's lines and its texts' last, whether the
/// captions are held, shown, asked for by the user or saved, and whether the pane is open.
pub(super) fn said(name: &str, shell: &Entity<Shell>, cx: &App) -> String {
    let calls = shell.read(cx).models().calls.read(cx);
    let call = calls.call().map(|call| format!("{:?}", call.id)).unwrap_or_default();
    let meeting = calls.call().is_some_and(teams_ui::calls::ActiveCall::is_meeting);
    let last = calls
        .spoken()
        .last()
        .map(|line| line.caption.text.clone())
        .unwrap_or_default();
    let pane = stage_read(cx, CallStage::transcript_shown).unwrap_or(false);
    format!(
        "{PREFIX}{name} call={call} meeting={meeting} spoken={} held={} shown={} asked={} saving={} pane={pane} \
         phase={:?} last={last:?}",
        calls.spoken().len(),
        calls.transcript_captions(),
        calls.captions_shown(),
        calls.caption_pane_asked(),
        calls.saving_captions(),
        calls.captions().phase,
    )
}

/// The caption asks the view trace saw so far: the parent counts them from the trace's lines.
#[derive(Clone, Default)]
pub(super) struct Asks(Arc<Mutex<(usize, usize)>>);

impl Asks {
    /// Counts the asks of the view trace `reader`, on a thread of its own, till the trace ends.
    pub(super) fn read(reader: impl BufRead + Send + 'static) -> Self {
        let asks = Self::default();
        let seen = asks.clone();
        thread::spawn(move || {
            for line in reader.lines().map_while(Result::ok) {
                seen.see(&line);
            }
        });
        asks
    }

    /// Counts one trace line.
    fn see(&self, line: &str) {
        if !line.contains("asked") {
            return;
        }
        let mut counts = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if line.contains("\"record-captions\"") {
            counts.0 += 1;
        } else if line.contains("\"captions\"") {
            counts.1 += 1;
        }
    }

    /// The caption asks of the transcript or the saving (`record-captions`), and the user's (`captions`).
    fn counts(&self) -> (usize, usize) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What a step's said line must hold, and how many more `record-captions` and `captions` asks it makes.
fn expected(name: &str) -> (&'static [&'static str], (usize, usize)) {
    match name {
        "qa-transcript-group" => (
            &[
                "spoken=0 ",
                "held=true ",
                "shown=false ",
                "pane=true ",
                "meeting=false ",
            ],
            (1, 0),
        ),
        "qa-transcript-many" => (&["spoken=60 ", "held=true ", "shown=false ", "pane=true "], (0, 0)),
        "qa-transcript-closed" => (&["spoken=65 ", "held=true ", "pane=false "], (0, 0)),
        "qa-transcript-reopened" => (&["spoken=65 ", "pane=true "], (0, 0)),
        "qa-transcript-captions-shown" => (&["spoken=66 ", "held=true ", "shown=true ", "asked=true "], (0, 1)),
        "qa-transcript-captions-hidden" => (&["spoken=67 ", "held=true ", "shown=false ", "asked=false "], (0, 1)),
        "qa-transcript-stopped" => (&["spoken=67 ", "held=false ", "shown=false "], (1, 0)),
        "qa-transcript-meeting" => (&["spoken=0 ", "held=true ", "shown=false ", "meeting=true "], (1, 0)),
        "qa-transcript-meeting-speech" | "qa-transcript-meeting-dark" => (&["spoken=6 ", "pane=true "], (0, 0)),
        "qa-transcript-translated" => (&["spoken=7 ", "pane=true "], (0, 0)),
        "qa-transcript-kept-shown" => (&["spoken=7 ", "held=false ", "shown=true ", "asked=true "], (0, 1)),
        "qa-transcript-kept-saved" => (&["spoken=8 ", "held=true ", "shown=false ", "saving=true "], (0, 1)),
        "qa-transcript-saved-stopped" => (&["spoken=8 ", "held=false ", "shown=false ", "saving=true "], (0, 0)),
        _ => (&[], (0, 0)),
    }
}

/// The asks counted before the step whose line is checked next.
#[derive(Default)]
pub(super) struct Checker {
    before: (usize, usize),
    log: Vec<String>,
}

impl Checker {
    /// Before the first of these steps runs (`next` after `name`), counts no asks of the tour's other steps.
    pub(super) fn before_next(&mut self, name: &str, next: Option<&str>, asks: &Asks) {
        if !ours(name) && next.is_some_and(ours) {
            // the asks are traced as they are made: they reach the parent's reader a moment later
            thread::sleep(Duration::from_millis(300));
            self.before = asks.counts();
        }
    }

    /// Checks the child's line of step `name` against [`expected`] and the asks made since the last check.
    fn check(&mut self, name: &str, line: &str, asks: &Asks) -> Result<(), String> {
        // the asks are traced as they are made: they reach the parent's reader a moment later
        thread::sleep(Duration::from_millis(300));
        let now = asks.counts();
        let made = (now.0 - self.before.0, now.1 - self.before.1);
        self.before = now;
        self.log
            .push(format!("{line} asks(record-captions,captions)=+{made:?}"));
        let (holds, want) = expected(name);
        let missing: Vec<&str> = holds.iter().copied().filter(|part| !line.contains(part)).collect();
        if !missing.is_empty() {
            return Err(format!("{name}: {line} lacks {missing:?}"));
        }
        if made != want {
            return Err(format!("{name}: asks +{made:?}, expected +{want:?}"));
        }
        Ok(())
    }

    /// Reads the child's line of step `name`, checks it and keeps every line checked in `dir`.
    pub(super) fn step(&mut self, running: &mut Running, name: &str, asks: &Asks, dir: &Path) -> SandboxResult {
        let told = running
            .line_where(super::WAIT, |line| line.starts_with(PREFIX))
            .map_err(|error| running.fail(&format!("no transcript line for {name}: {error:?}")))?;
        let result = self.check(name, &told, asks);
        fs::write(dir.join("tour-qa-transcript.log"), self.log.join("\n"))?;
        result.map_err(|error| running.fail(&error))
    }
}

/// What the parent does after the shot of step `name`: Tab and Page Up in the call window, or the wheel over the pane.
pub(super) fn after_shot(sandbox: &Sandbox, name: &str, png: &Path, dir: &Path) -> SandboxResult {
    if name == KEYS {
        let mut input = sandbox.input()?;
        // a shot after each Tab shows where the focus goes
        for tab in 1..=30 {
            input.key(keys::TAB)?;
            thread::sleep(Duration::from_millis(300));
            let shot = sandbox.screenshot(&format!("tour-qa-transcript-tab-{tab:02}.png"), true)?;
            fs::copy(&shot, dir.join(format!("tour-qa-transcript-tab-{tab:02}.png")))?;
            input.key(keys::PAGE_UP)?;
            input.key(keys::UP)?;
            thread::sleep(Duration::from_millis(150));
        }
    } else if name == WHEEL {
        let (width, height) = image::image_dimensions(png)?;
        let mut input = sandbox.input()?;
        input.move_to(width - 160, height / 2)?;
        thread::sleep(Duration::from_millis(200));
        input.scroll(width - 160, height / 2, -10)?;
    }
    thread::sleep(Duration::from_millis(500));
    Ok(())
}

fn fixed_theme(shell: &Entity<Shell>, key: Option<&'static str>, cx: &mut App) {
    super::fixed_theme(shell, key, cx);
}

fn with_stage(cx: &mut App, change: impl FnOnce(&mut CallStage, &mut gpui::Context<CallStage>)) {
    super::with_stage(cx, change);
}

fn with_stage_window(cx: &mut App, change: impl FnOnce(&mut CallStage, &mut Window, &mut gpui::Context<CallStage>)) {
    super::with_stage_window(cx, change);
}

/// Reads the call window's stage, when it is open.
fn stage_read<T>(cx: &App, read: impl FnOnce(&CallStage) -> T) -> Option<T> {
    let stage = cx.windows().into_iter().find_map(|window| {
        let kit = window.downcast::<teams_ui::window::KitRoot>()?;
        kit.read(cx).ok()?.view().clone().downcast::<CallStage>().ok()
    })?;
    Some(read(stage.read(cx)))
}

/// Alt+Shift+C in the call window, as the user presses it to show or hide the live captions.
fn toggle_captions(_: &mut Window, cx: &mut App) {
    with_stage_window(cx, |_, window, cx| {
        window.defer(cx, |window, cx| {
            if let Ok(keystroke) = gpui::Keystroke::parse("alt-shift-c") {
                window.dispatch_keystroke(keystroke, cx);
            }
        });
    });
}

fn ended(shell: &Entity<Shell>, number: u64, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::Call {
            call: teams_core::CallId::for_tests(number),
            status: teams_core::CallStatus::Ended(teams_core::End::HungUp),
        },
        cx,
    );
}

/// A call from b in `chat`, answered and connected, with b and c in its roster, as the engine tells it.
fn call(shell: &Entity<Shell>, number: u64, chat: &str, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    let call = teams_core::CallId::for_tests(number);
    let person = |mri: &str, name: &str| teams_core::CallParticipant {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        video_on: false,
        line: None,
        speaking: false,
        muted: false,
        hand_raised: false,
        organizer: false,
        joined: None,
        sharing: false,
    };
    let events = [
        teams_core::Event::IncomingCall(teams_core::IncomingCall {
            call,
            caller: Mri::new(LAB_B),
            caller_name: Some("Lab Bee".to_owned()),
            group_chat: Some(chat.to_owned()),
            meeting: None,
        }),
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Answering,
        },
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Connected,
        },
        teams_core::Event::CallParticipants {
            call,
            view: teams_core::CallView {
                participants: vec![person(LAB_B, "Lab Bee"), person(LAB_C, "Lab Sea")],
                speaking: false,
                hearing: false,
            },
        },
    ];
    for event in events {
        models.apply(event, cx);
    }
}

/// The roster tells a transcription `by` someone, or none (`None`).
fn transcribing(shell: &Entity<Shell>, number: u64, by: Option<&str>, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallMeeting {
            call: teams_core::CallId::for_tests(number),
            state: teams_core::MeetingState {
                transcribing: by.map(|by| teams_core::Transcription { by: Some(Mri::new(by)) }),
                ..teams_core::MeetingState::default()
            },
        },
        cx,
    );
}

/// The captions off, as the engine tells it once they are asked to stop.
fn captions_off(shell: &Entity<Shell>, number: u64, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    models.apply(
        teams_core::Event::CallCaptions {
            call: teams_core::CallId::for_tests(number),
            captions: teams_core::CallCaptions::default(),
        },
        cx,
    );
}

/// Line `index` of the made-up speech: a, b and c in turns, b twice in a row each fifth line.
fn say_line(shell: &Entity<Shell>, number: u64, index: u64, cx: &mut App) {
    let who = match index % 5 {
        0 | 1 => (LAB_B, "Lab Bee"),
        2 | 3 => (LAB_C, "Lab Sea"),
        _ => (LAB_A, "Lab A"),
    };
    let text = format!("Synthetic line {index} said by {}.", who.1);
    say_text(shell, number, index, who, &text, cx);
}

/// `text` said by `who` as line `index`: first still spoken, then final, as the captions tell it.
fn say_text(shell: &Entity<Shell>, number: u64, index: u64, who: (&str, &str), text: &str, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    for final_result in [false, true] {
        let line = teams_core::Caption {
            id: format!("synthetic-qa/{number}/{index}"),
            speaker: Some(who.0.to_owned()),
            display_name: Some(who.1.to_owned()),
            text: if final_result {
                text.to_owned()
            } else {
                text.split(' ').take(2).collect::<Vec<_>>().join(" ")
            },
            final_result,
            spoken_language: "en-us".to_owned(),
            track_language: "en-us".to_owned(),
            audio_timestamp: index * 3_000_000,
            duration: 20_000,
        };
        models.apply(
            teams_core::Event::CallCaptions {
                call: teams_core::CallId::for_tests(number),
                captions: teams_core::CallCaptions {
                    phase: teams_core::CaptionPhase::On,
                    spoken_language: "en-us".to_owned(),
                    error: None,
                    lines: vec![line],
                },
            },
            cx,
        );
    }
}

/// The captions' "Translate to" `language` (none: off), by the tour's fake `claude`.
fn translate_to(shell: &Entity<Shell>, language: Option<&'static str>, cx: &mut App) {
    if language.is_some() {
        let _ = super::fake_claude_tools(cx);
    }
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.caption_translate_to = language;
                settings.caption_translator = teams_ui::translate::AiChoice::default();
            },
            cx,
        );
    });
}
