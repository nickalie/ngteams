//! What the call window shows of a recording or a transcription someone runs, as the roster's recording bot tells it
//! (`docs/protocol/calling.md`, "More of a running call"), as Teams shows it:
//!
//! - **Notices** over the stage's top: "Recording and transcription" ("Audio recording and transcription" for the
//!   audio alone) or "Transcription has started" (a transcription alone), each with who started it — "Started by You.
//!   Let everyone know they're being included." to its starter, "Started by <name>. By attending this meeting, you
//!   agree to being included." to everyone else, with the privacy policy; then "Recording and transcription have
//!   stopped." or "Transcription has stopped" once they stop.
//! - **The header's mark** left of the duration: the red dot while it records, the microphone's while it records the
//!   audio alone, the transcript's while it only transcribes.
//! - **The Transcript pane** (Show transcript; it opens by itself for whoever started a transcription alone): its
//!   translation's language and Close, a bar offering live captions instead ("Switch to live captions"), "AI-generated
//!   content may be incorrect", a row per start and stop of the transcription, as the roster told them, and what is
//!   said: each speaker's turn (their initials, name and time from the transcription's start) over the final words of
//!   each of their utterances, live as they come, for the whole call ([`crate::calls::Calls::spoken`]).
//! - **What is said** comes from the call's live captions: no capture showed the transcript's own speech reaching the
//!   call. While a transcription runs, the captions run for the transcript ([`Ask::RecordCaptions`]), their pane left
//!   as the user has it, and stop with it unless the user shows them or the call's captions are saved.
//! - **While the account's own start is pending** (the bot invited and told to start, 15 to 27 s in the lab): a
//!   one-line notice, "Starting transcription…" or "Starting recording and transcription…" ([`Pending`]), with a
//!   Close; a recording's mark left of the duration, greyed; and a spinner atop the open Transcript pane. A start that
//!   fails says why ([`super::recording`]); one that takes longer than Teams waits says "Couldn't start recording and
//!   transcription." ([`teams_core::RecordingProblem`]).

use super::recording::{Start, Stopped, TranscriptRow};
use super::*;
use crate::calls::SpokenLine;
use crate::translate::{self, CaptionTranslation};
use std::collections::HashSet;
use teams_core::RecordingPhase;

/// What the meeting's privacy policy link opens: Microsoft's privacy statement, as Teams links it when the
/// organization names none.
const PRIVACY_POLICY: &str = "https://go.microsoft.com/fwlink/?LinkId=521839";

impl CallStage {
    /// What the roster told since last seen of the recording and the transcription: a start or a stop shows its notice,
    /// the transcript gets its row, and the account's own transcription alone opens the transcript.
    pub(super) fn recorder_seen(&mut self, active: &ActiveCall, cx: &mut App) {
        let state = &active.meeting;
        let was_recording = self.recording_seen;
        let recording = state.recording.is_some();
        if recording && !was_recording {
            self.recording_notice = true;
            self.record.stopped = None;
            self.record.transcription_notice = None;
        }
        if !recording {
            self.recording_notice = false;
            if was_recording {
                // it stopped: whoever stopped it, the notice says so
                self.record.stopped = Some(Stopped {
                    by_me: std::mem::take(&mut self.record.stopping),
                    audio: self.record.audio_seen,
                });
                self.record.transcription_notice = None;
            }
        }
        self.recording_seen = recording;
        if let Some(recording) = &state.recording {
            self.record.audio_seen = recording.audio_only;
        }
        let transcribing = state.transcribing.clone();
        match (self.record.transcribing_seen.take(), &transcribing) {
            (None, Some(teams_core::Transcription { by })) => {
                self.record.transcript.push(TranscriptRow {
                    by: by.clone(),
                    started: true,
                    at: chrono::Utc::now().timestamp_millis(),
                });
                if !recording {
                    self.record.transcription_notice = Some(true);
                    self.record.stopped = None;
                    if by.as_ref().is_some_and(|by| by.as_str() == self.core.me()) {
                        self.open_pane(Pane::Transcript, cx);
                        self.effects = None;
                    }
                }
            }
            (Some(teams_core::Transcription { by }), None) => {
                self.record.transcript.push(TranscriptRow {
                    by,
                    started: false,
                    at: chrono::Utc::now().timestamp_millis(),
                });
                // with a recording, the recording's stop says it
                if !recording && !was_recording {
                    self.record.transcription_notice = Some(false);
                }
            }
            _ => {}
        }
        self.record.transcribing_seen = transcribing;
    }

    /// The captions run while a transcription runs, for the transcript's words: asked as it starts (and again a few
    /// times, apart, after they failed or ended unseen), and stopped as it stops unless the user shows them or they are
    /// saved. A reconnect keeps the hold; the captions are asked only while the call is connected.
    pub(super) fn hold_transcript_captions(&mut self, active: &ActiveCall, cx: &mut Context<Self>) {
        let connected = active.status == CallStatus::Connected;
        let want = active.meeting.transcribing.is_some()
            && (connected || active.status == CallStatus::Reconnecting)
            && active.kind != CallKind::TestCall
            && !self.core.read_only()
            && self.core.capabilities().captions;
        let calls = self.calls.read(cx);
        let phase = calls.captions().phase;
        let running = matches!(phase, teams_core::CaptionPhase::On | teams_core::CaptionPhase::Starting);
        if calls.transcript_captions() == want {
            if want && connected {
                self.retry_transcript_captions(running, cx);
            }
            return;
        }
        // the hold starts once connected: the captions can be asked for only then
        if want && !connected {
            return;
        }
        self.record.transcript_retries = 0;
        let kept = calls.caption_pane_asked() || calls.saving_captions();
        self.calls
            .update(cx, |calls, cx| calls.set_transcript_captions(want, cx));
        if want && !running {
            self.ask(Ask::RecordCaptions(true), cx);
        } else if !want && phase != teams_core::CaptionPhase::Off && !kept {
            self.ask(Ask::RecordCaptions(false), cx);
        }
    }

    /// The captions held for the transcript stopped unseen (failed, or ended by the service): asked for again after a
    /// while, a few times, unless the user shows them (their failure shows) or they are saved and failed (the saving
    /// tries them again). A try that finds the call reconnecting is not counted.
    fn retry_transcript_captions(&mut self, running: bool, cx: &mut Context<Self>) {
        if running && self.calls.read(cx).captions().phase == teams_core::CaptionPhase::On {
            self.record.transcript_retries = 0;
        }
        let calls = self.calls.read(cx);
        if running
            || calls.captions_shown()
            || (calls.saving_captions() && calls.captions().phase == teams_core::CaptionPhase::Failed)
            || self.record.transcript_retrying
            || self.record.transcript_retries >= crate::call_transcript::RETRIES
        {
            return;
        }
        self.record.transcript_retries += 1;
        self.record.transcript_retrying = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::call_transcript::RETRY_AFTER)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.record.transcript_retrying = false;
                let connected = this
                    .live(cx)
                    .is_some_and(|active| active.status == CallStatus::Connected);
                let calls = this.calls.read(cx);
                let stopped = !matches!(
                    calls.captions().phase,
                    teams_core::CaptionPhase::On | teams_core::CaptionPhase::Starting
                );
                if !calls.transcript_captions() || !stopped {
                    return;
                }
                if connected {
                    this.ask(Ask::RecordCaptions(true), cx);
                } else {
                    this.record.transcript_retries = this.record.transcript_retries.saturating_sub(1);
                }
            });
        })
        .detach();
    }

    /// The transcript stays at its end while it was there at the last frame: a new line, a translation or a longer
    /// line keeps it there, a scroll up from the end stops it.
    fn follow_transcript(&self) {
        let scroll = &self.record.transcript_scroll;
        if scroll.max_offset().y + scroll.offset().y <= px(FOLLOW_WITHIN) {
            scroll.scroll_to_bottom();
        }
    }

    /// Opens or closes the Transcript pane, as Show transcript and Hide transcript do.
    pub fn show_transcript(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.open_pane(Pane::Transcript, cx);
            self.effects = None;
        } else if self.pane == Pane::Transcript {
            self.pane = Pane::None;
        }
        let live = self.live(cx);
        self.translate(live.as_ref(), cx);
        cx.notify();
    }

    /// Whether the Transcript pane is open.
    #[must_use]
    pub fn transcript_shown(&self) -> bool {
        self.pane == Pane::Transcript
    }

    /// Who started a recording or a transcription, as its notice says it: its starter is asked to let everyone know.
    fn started_line(&self, active: &ActiveCall, by: Option<&Mri>, palette: Palette) -> AnyElement {
        let (text, selector) = if by.is_some_and(|by| by.as_str() == self.core.me()) {
            (
                SharedString::from("Started by You. Let everyone know they're being included."),
                "call-recording-privacy",
            )
        } else {
            let who = by.map_or_else(|| SharedString::from(SOMEONE), |mri| self.name_of(active, mri));
            (
                SharedString::from(format!(
                    "Started by {who}. By attending this meeting, you agree to being included."
                )),
                "call-recording-policy",
            )
        };
        meeting::text_with_link(selector, &text, "Privacy policy", PRIVACY_POLICY, palette)
    }

    /// A recording's notice while it shows: "Recording and transcription" (the audio alone: "Audio recording and
    /// transcription") and who started it.
    pub(super) fn recording_notice_view(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let recording = active.meeting.recording.as_ref().filter(|_| self.recording_notice)?;
        let (glyph, heading) = if recording.audio_only {
            (Glyph::MIC, "Audio recording and transcription")
        } else {
            (Glyph::RECORD, "Recording and transcription")
        };
        Some(meeting::notice(
            ("call-recording-notice", glyph, heading),
            self.started_line(active, recording.by.as_ref(), palette),
            palette,
            cx.listener(|this, _, _, cx| {
                this.recording_notice = false;
                cx.notify();
            }),
        ))
    }

    /// A transcription's notice while it shows: "Transcription has started" with who started it, or "Transcription has
    /// stopped".
    pub(super) fn transcription_notice(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let started = self.record.transcription_notice?;
        let close = cx.listener(|this, _, _, cx| {
            this.record.transcription_notice = None;
            cx.notify();
        });
        if !started {
            return Some(meeting::notice(
                (
                    "call-transcription-stopped",
                    Glyph::CALL_TRANSCRIPT,
                    "Transcription has stopped",
                ),
                SharedString::from("If you change your mind, you can start it again anytime."),
                palette,
                close,
            ));
        }
        let by = active
            .meeting
            .transcribing
            .as_ref()
            .and_then(|transcription| transcription.by.as_ref());
        Some(meeting::notice(
            (
                "call-transcription-notice",
                Glyph::CALL_TRANSCRIPT,
                "Transcription has started",
            ),
            self.started_line(active, by, palette),
            palette,
            close,
        ))
    }

    /// The Transcript pane while it is open (see the module docs).
    pub(super) fn transcript_pane(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.pane != Pane::Transcript {
            return None;
        }
        let actions = div()
            .flex()
            .items_center()
            .gap_1()
            .children(crate::local_ai::enabled(cx).then(|| self.transcript_translate(cx)))
            .child(selected_by(
                "call-transcript-close",
                icon_button("call-transcript-close", Glyph::X, "Close transcript")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.show_transcript(false, cx))),
            ));
        let rows = self.transcript_rows(active, palette, cx);
        self.follow_transcript();
        Some(
            people_pane::side_pane("call-transcript-pane", "Transcript", actions, palette)
                .children((!self.record.transcript_tip_closed).then(|| Self::transcript_tip(palette, cx)))
                .child(
                    div()
                        .id("call-transcript-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.record.transcript_scroll)
                        .px_4()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(self.pending_start(cx).map(|_| {
                            div()
                                .debug_selector(|| "call-transcript-pending".to_owned())
                                .flex()
                                .justify_center()
                                .py_2()
                                .text_color(rgb(palette.brand))
                                .child(Glyph::LOADING.icon().size(px(24.0)))
                        }))
                        .child(
                            transcript_line(
                                Glyph::INFO,
                                SharedString::from("AI-generated content may be incorrect"),
                                palette,
                            )
                            .text_color(rgb(palette.muted_foreground)),
                        )
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    /// The transcript's rows: each start and stop of the transcription, and each speaker's turn.
    fn transcript_rows(&self, active: &ActiveCall, palette: Palette, cx: &App) -> Vec<AnyElement> {
        let calls = self.calls.read(cx);
        let entries = entries(&self.record.transcript, calls.spoken());
        let start = self
            .record
            .transcript
            .iter()
            .find(|row| row.started)
            .map(|row| row.at)
            .or_else(|| calls.spoken().first().map(|line| line.at))
            .unwrap_or_default();
        let translation = self.translation.read(cx);
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                Entry::Event(row) => {
                    let who = row
                        .by
                        .as_ref()
                        .map_or_else(|| SharedString::from(SOMEONE), |mri| self.name_of(active, mri));
                    let what = if row.started { "started" } else { "stopped" };
                    transcript_line(
                        Glyph::RECORD,
                        SharedString::from(format!("{who} {what} transcription")),
                        palette,
                    )
                    .debug_selector(|| "call-transcript-event".to_owned())
                    .into_any_element()
                }
                Entry::Turn(lines) => turn(index, lines, start, translation, palette),
            })
            .collect()
    }

    /// The pane's bar offering live captions instead: Switch to live captions shows them and closes the pane.
    fn transcript_tip(palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let switch = Button::new("call-transcript-captions")
            .label("Switch to live captions")
            .outline()
            .small()
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_transcript(false, cx);
                this.ask(Ask::Captions(true), cx);
            }));
        let close = icon_button("call-transcript-tip-close", Glyph::X, "Close")
            .xsmall()
            .on_click(cx.listener(|this, _, _, cx| {
                this.record.transcript_tip_closed = true;
                cx.notify();
            }));
        div()
            .debug_selector(|| "call-transcript-tip".to_owned())
            .flex_none()
            .px_4()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(Glyph::INFO.icon().size(px(16.0)).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .child("Follow the conversation better in real time by using live captions."),
                    )
                    .child(selected_by("call-transcript-tip-close", close)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(selected_by("call-transcript-captions", switch)),
            )
    }

    /// The pane's translation: the language the call's speech is translated into on this computer ("Translate to", as
    /// the live captions' settings have it), or Off.
    fn transcript_translate(&self, cx: &App) -> impl IntoElement + use<> {
        let settings = self.settings.clone();
        let current = settings.read(cx).settings().caption_translate_to;
        selected_by(
            "call-transcript-translate",
            icon_button("call-transcript-translate", Glyph::LANGUAGES, "Translate transcript")
                .small()
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                    let menu = menu.item(translate_item(&settings, (None, "Off"), current));
                    translate::TARGET_LANGUAGES
                        .iter()
                        .fold(menu, |menu, &(code, name)| {
                            menu.item(translate_item(&settings, (Some(code), name), current))
                        })
                        .scrollable(true)
                        .max_h(px(360.0))
                }),
        )
    }
}

/// The pane's translation's language `code` (Off: `None`), checked while it is `current`.
fn translate_item(
    settings: &Entity<SettingsModel>,
    (code, label): (Option<&'static str>, &'static str),
    current: Option<&'static str>,
) -> PopupMenuItem {
    let settings = settings.clone();
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || format!("call-transcript-translate-{}", code.unwrap_or("off")))
            .child(label)
    })
    .checked(code == current)
    .on_click(move |_, _, cx| {
        settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.caption_translate_to = code, cx);
        });
    })
}

/// The account's own start, from its dialog's Confirm until the roster says it runs (or it fails): what Teams' window
/// shows its starter meanwhile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Pending {
    start: Start,
    audio_only: bool,
}

impl Pending {
    pub(super) fn new(start: Start, audio_only: bool) -> Self {
        Self { start, audio_only }
    }

    /// Whether it starts a recording (whose mark shows greyed meanwhile).
    fn records(self) -> bool {
        self.start != Start::Transcription
    }

    /// Its notice's words and glyph, as Teams words them (the recording's alone and the audio's from Teams' own
    /// strings, not seen shown).
    fn words(self) -> (&'static str, Glyph) {
        match (self.start, self.audio_only) {
            (Start::Transcription, _) => ("Starting transcription\u{2026}", Glyph::CALL_TRANSCRIPT),
            (Start::Both, false) => ("Starting recording and transcription\u{2026}", Glyph::RECORD),
            (Start::Both, true) => ("Starting audio recording and transcription\u{2026}", Glyph::MIC),
            (Start::Recording, false) => ("Starting recording\u{2026}", Glyph::RECORD),
            (Start::Recording, true) => ("Starting audio recording\u{2026}", Glyph::MIC),
        }
    }
}

impl CallStage {
    /// The account's own start while it is pending.
    pub(super) fn pending_start(&self, cx: &App) -> Option<Pending> {
        let starting = self.calls.read(cx).recording().phase == RecordingPhase::Starting;
        self.record.pending.filter(|_| starting)
    }

    /// "Starting transcription…" (or the recording's) while the account's start is pending, until it is closed.
    pub(super) fn pending_notice(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pending = self.pending_start(cx).filter(|_| !self.record.pending_closed)?;
        let (words, glyph) = pending.words();
        let close = icon_button("call-record-pending-close", Glyph::X, "Close")
            .xsmall()
            .on_click(cx.listener(|this, _, _, cx| {
                this.record.pending_closed = true;
                cx.notify();
            }));
        Some(
            div()
                .debug_selector(|| "call-record-pending".to_owned())
                .occlude()
                .w(px(320.0))
                .max_w_full()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .shadow_md()
                .child(glyph.icon().size(px(16.0)).flex_none())
                .child(div().flex_1().min_w_0().child(words))
                .child(selected_by("call-record-pending-close", close))
                .into_any_element(),
        )
    }

    /// The duration's mark: the recording's or the transcription's ([`indicator`]), else a recording's greyed while
    /// the account's start of it is pending, as Teams greys it.
    pub(super) fn duration_mark(&self, active: &ActiveCall, palette: Palette, cx: &App) -> Option<AnyElement> {
        indicator(active, palette).or_else(|| {
            self.pending_start(cx)
                .filter(|pending| pending.records())
                .map(|pending| {
                    let (_, glyph) = pending.words();
                    div()
                        .debug_selector(|| "call-recording-pending".to_owned())
                        .flex_none()
                        .text_color(rgb(palette.on_frame).opacity(0.5))
                        .child(glyph.icon().size(px(14.0)))
                        .into_any_element()
                })
        })
    }
}

/// How near its end (px) the transcript must be to follow: each frame follows, so only a sub-pixel rest counts.
const FOLLOW_WITHIN: f32 = 1.0;

/// What the translation follows while the Transcript pane shows: the whole call's words, then the captions' lines not
/// final among them, so a line keeps its translation after the captions dropped it.
pub(super) fn translated_lines(spoken: &[SpokenLine], captions: &[teams_core::Caption]) -> Vec<teams_core::Caption> {
    let keys: HashSet<translate::LineKey> = spoken
        .iter()
        .map(|line| translate::LineKey::of(&line.caption))
        .collect();
    spoken
        .iter()
        .map(|line| line.caption.clone())
        .chain(
            captions
                .iter()
                .filter(|caption| !keys.contains(&translate::LineKey::of(caption)))
                .cloned(),
        )
        .collect()
}

/// What the transcript lists, in the order it happened: a start or a stop of the transcription, or a speaker's turn
/// (their utterances one after another, nothing else between).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Entry<'a> {
    Event(&'a TranscriptRow),
    Turn(Vec<&'a SpokenLine>),
}

/// The transcript's entries: its starts and stops and what was said, by time (a start before the words of its
/// moment), each run of one speaker's utterances one turn.
pub(super) fn entries<'a>(events: &'a [TranscriptRow], spoken: &'a [SpokenLine]) -> Vec<Entry<'a>> {
    let mut timed: Vec<(i64, bool, usize)> = events
        .iter()
        .enumerate()
        .map(|(index, row)| (row.at, false, index))
        .chain(spoken.iter().enumerate().map(|(index, line)| (line.at, true, index)))
        .collect();
    timed.sort_by_key(|(at, said, _)| (*at, *said));
    let mut out: Vec<Entry<'a>> = Vec::new();
    for (_, said, index) in timed {
        if !said {
            if let Some(row) = events.get(index) {
                out.push(Entry::Event(row));
            }
            continue;
        }
        let Some(line) = spoken.get(index) else { continue };
        match out.last_mut() {
            Some(Entry::Turn(lines)) if lines.last().is_some_and(|last| same_speaker(last, line)) => lines.push(line),
            _ => out.push(Entry::Turn(vec![line])),
        }
    }
    out
}

/// Whether two utterances are the same speaker's: by their id, else by their name.
fn same_speaker(one: &SpokenLine, other: &SpokenLine) -> bool {
    match (&one.caption.speaker, &other.caption.speaker) {
        (Some(one), Some(other)) => one == other,
        (None, None) => one.caption.display_name == other.caption.display_name,
        _ => false,
    }
}

/// A speaker's turn: their initials, name and time from the transcription's `start` (ms), then each utterance's words
/// (translated when the pane's translation has them).
fn turn(
    index: usize,
    lines: &[&SpokenLine],
    start: i64,
    translation: &CaptionTranslation,
    palette: Palette,
) -> AnyElement {
    let first = lines.first().map(|line| &line.caption);
    let name = SharedString::from(
        first
            .and_then(|caption| caption.display_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "Speaker".to_owned()),
    );
    let since = lines.first().map_or(0, |line| line.at.saturating_sub(start));
    let time = crate::video_player::clock(Duration::from_millis(u64::try_from(since).unwrap_or(0)));
    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .child(people::avatar(Some(&name), None, Size::Small).flex_none())
        .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
        .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(time));
    let said = lines.iter().map(|line| {
        let (text, selector) = match translation.shown(&line.caption) {
            translate::Shown::Translated(text) => (text, "call-transcript-translated"),
            translate::Shown::Waiting(text) | translate::Shown::Spoken(text) => (text, "call-transcript-said"),
        };
        div()
            .debug_selector(move || selector.to_owned())
            .pl(px(32.0))
            .text_sm()
            .text_color(rgb(palette.foreground))
            .child(SharedString::from(text.trim().to_owned()))
    });
    div()
        .debug_selector(move || format!("call-transcript-turn-{index}"))
        .pt_2()
        .flex()
        .flex_col()
        .gap_1()
        .child(header)
        .children(said)
        .into_any_element()
}

/// A line of the transcript: its mark and its text.
fn transcript_line(glyph: Glyph, text: SharedString, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(rgb(palette.foreground))
        .child(glyph.icon().size(px(14.0)).flex_none())
        .child(div().flex_1().min_w_0().child(text))
}

/// The mark Teams shows left of the call's duration while it records or transcribes: the red dot for a recording, the
/// microphone's for one of the audio alone, the transcript's for a transcription alone.
pub(super) fn indicator(active: &ActiveCall, palette: Palette) -> Option<AnyElement> {
    let state = &active.meeting;
    match (&state.recording, &state.transcribing) {
        (Some(recording), _) if recording.audio_only => Some(
            div()
                .debug_selector(|| "call-recording-audio".to_owned())
                .relative()
                .flex_none()
                .size(px(16.0))
                .child(Glyph::MIC.icon().size(px(14.0)))
                .child(
                    div()
                        .absolute()
                        .right_0()
                        .bottom_0()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(rgb(palette.on_frame)),
                )
                .into_any_element(),
        ),
        (Some(_), _) => Some(
            div()
                .debug_selector(|| "call-recording-dot".to_owned())
                .flex_none()
                .size(px(10.0))
                .rounded_full()
                .bg(rgb(theme::DANGER))
                .into_any_element(),
        ),
        (None, Some(_)) => Some(
            div()
                .debug_selector(|| "call-transcribing".to_owned())
                .flex_none()
                .child(Glyph::TRANSCRIBE.icon().size(px(14.0)))
                .into_any_element(),
        ),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: i64, started: bool) -> TranscriptRow {
        TranscriptRow { by: None, started, at }
    }

    fn line(at: i64, speaker: Option<&str>, name: &str) -> SpokenLine {
        SpokenLine {
            at,
            caption: teams_core::Caption {
                id: at.to_string(),
                speaker: speaker.map(str::to_owned),
                display_name: Some(name.to_owned()),
                text: "Synthetic words.".to_owned(),
                final_result: true,
                spoken_language: "en-us".to_owned(),
                track_language: "en-us".to_owned(),
                audio_timestamp: 0,
                duration: 0,
            },
        }
    }

    fn shape(entries: &[Entry<'_>]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Event(row) => format!("event {}", row.started),
                Entry::Turn(lines) => format!("turn {}", lines.len()),
            })
            .collect()
    }

    #[test]
    fn the_translation_follows_the_whole_transcript_then_the_captions_not_final_in_it() {
        let spoken = [line(10, Some("a"), "A"), line(20, Some("b"), "B")];
        let mut partial = line(30, Some("a"), "A").caption;
        partial.final_result = false;
        let captions = [spoken[1].caption.clone(), partial.clone()];
        let ids: Vec<String> = translated_lines(&spoken, &captions)
            .into_iter()
            .map(|caption| caption.id)
            .collect();
        assert_eq!(ids, ["10", "20", "30"]);
    }

    #[test]
    fn each_speaker_s_run_of_utterances_is_one_turn_between_the_transcription_s_events() {
        let events = [row(10, true), row(90, false)];
        let spoken = [
            line(10, Some("a"), "A"),
            line(20, Some("a"), "A"),
            line(30, Some("b"), "B"),
            line(40, None, "C"),
            line(50, None, "C"),
            line(60, Some("a"), "A"),
            line(95, Some("a"), "A"),
        ];
        assert_eq!(
            shape(&entries(&events, &spoken)),
            [
                "event true",
                "turn 2",
                "turn 1",
                "turn 2",
                "turn 1",
                "event false",
                "turn 1"
            ]
        );
    }
}
