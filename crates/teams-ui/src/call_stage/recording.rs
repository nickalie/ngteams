//! Record and transcribe, as Teams' call window offers it in a call or a meeting (`docs/protocol/calling.md`, "More of
//! a running call"):
//!
//! - **More → Record and transcribe** lists Start recording (Stop recording while it records) and Start transcription
//!   (Stop transcription while it transcribes), then Show transcript (Hide transcript, checked, while it shows) once a
//!   transcription ran in the call, set apart by a line.
//! - **Start recording** while nothing runs asks "Start recording and transcription" first: the spoken language
//!   (English (US) unless the captions named another), with "More options" → "Choose what to record" (Video and audio,
//!   or Audio only), Cancel and Confirm ([`Ask::Record`]). While a transcription runs it asks "Start recording" with
//!   "Choose what to record" alone, and the recording starts beside the transcription. The roster's recording then shows
//!   the red dot (the microphone's mark for the audio alone) and "Recording and transcription" (or "Audio recording and
//!   transcription") with who started it, as anyone's does to everyone ([`super::transcript`]).
//! - **Start transcription** asks "What language is everyone speaking?" (every time it starts) and starts the
//!   transcription alone ([`Ask::Transcribe`]).
//! - **Stop recording**, while it records (whoever started it), asks "Stop recording and transcription?" first (or
//!   "Stop recording?" while no transcription runs) and stops both ([`Ask::StopRecording`]); **Stop transcription** asks
//!   "Stop transcription?" and stops it alone ([`Ask::StopTranscription`]).
//! - A start or a stop that failed says why in Teams' words ([`teams_core::RecordingProblem`]): the policy's refusal,
//!   the call's, or that something went wrong.

use super::*;
use teams_core::{RecordingPhase, RecordingProblem};
use teams_proto::captions::SPOKEN_LANGUAGES;

/// The spoken language the start asks in unless the captions named another, as Teams' dialog starts.
const DEFAULT_LANGUAGE: &str = "en-us";

/// What a start dialog starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Start {
    /// "Start recording and transcription": nothing runs yet.
    Both,
    /// "Start recording": the transcription runs already.
    Recording,
    /// "What language is everyone speaking?": Start transcription.
    Transcription,
}

/// A start dialog while it is asked: what it starts and what is picked in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StartDialog {
    start: Start,
    /// The spoken language picked.
    language: String,
    /// "Choose what to record" → Audio only.
    audio_only: bool,
    /// "More options" opened ("Start recording and transcription" only).
    more: bool,
}

/// The recording's stop as its notice tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stopped {
    /// The account stopped it.
    pub(super) by_me: bool,
    /// It recorded the audio alone.
    pub(super) audio: bool,
}

/// A transcription's start or stop, as the transcript lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TranscriptRow {
    /// Who started the transcription (the roster keeps naming them once it stopped).
    pub(super) by: Option<Mri>,
    pub(super) started: bool,
    /// When the roster told it, ms since the epoch: where it goes among the transcript's lines.
    pub(super) at: i64,
}

/// Record and transcribe's dialogs, notices and transcript in the call's window.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a choice of its own in the window (a confirmation, a notice), set by its own control"
)]
pub(super) struct RecordUi {
    /// A start dialog is asked.
    pub(super) dialog: Option<StartDialog>,
    /// "Stop recording and transcription?" is asked.
    pub(super) confirm_stop: bool,
    /// "Stop transcription?" is asked.
    pub(super) confirm_stop_transcription: bool,
    /// The account asked to stop the recording, until it stopped: its notice says "You stopped recording".
    pub(super) stopping: bool,
    /// "Recording and transcription have stopped." shows.
    pub(super) stopped: Option<Stopped>,
    /// The account's start or stop that failed, while its notice shows; and the failure last seen.
    problem: Option<RecordingProblem>,
    problem_seen: Option<RecordingProblem>,
    /// The recording last seen recorded the audio alone.
    pub(super) audio_seen: bool,
    /// The transcription ran when last seen, started by whom.
    pub(super) transcribing_seen: Option<teams_core::Transcription>,
    /// "Transcription has started" (`true`) or "Transcription has stopped" (`false`) shows.
    pub(super) transcription_notice: Option<bool>,
    /// The transcript's rows, as the roster told the transcription's starts and stops.
    pub(super) transcript: Vec<TranscriptRow>,
    /// The transcript's "Switch to live captions" bar was put away.
    pub(super) transcript_tip_closed: bool,
    /// The transcript's scroll: it follows the newest line.
    pub(super) transcript_scroll: gpui::ScrollHandle,
    /// The account's own start last confirmed: its notice shows while the start is pending ([`super::transcript`]).
    pub(super) pending: Option<super::transcript::Pending>,
    /// That notice was put away.
    pub(super) pending_closed: bool,
}

/// What More's Record and transcribe offers now.
#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each says what the roster or the window shows now, read once as the menu opens"
)]
pub(super) struct RecordTools {
    /// It may be offered: the call is connected, not the test call, and the app may write.
    pub(super) available: bool,
    /// The call records now (the roster says so).
    pub(super) recording: bool,
    /// The call transcribes now (the roster says so).
    pub(super) transcribing: bool,
    /// Show transcript is offered: a transcription runs or ran in the call.
    pub(super) transcript: bool,
    /// The transcript pane shows.
    pub(super) transcript_shown: bool,
    /// The account's own start or stop under way.
    pub(super) phase: RecordingPhase,
}

/// More's Record and transcribe, at its top (see the module docs).
pub(super) fn record_menu(
    menu: PopupMenu,
    stage: &WeakEntity<CallStage>,
    tools: RecordTools,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if !tools.available {
        return menu;
    }
    let stage = stage.clone();
    // no icon: the kit draws a submenu's in the gutter, out of line with the items' own (Language and speech has none)
    menu.submenu("Record and transcribe", window, cx, move |menu, _, _| {
        let menu = recording_item(menu, &stage, tools);
        let menu = if tools.transcript { menu.separator() } else { menu };
        let menu = transcription_item(menu, &stage, tools);
        if !tools.transcript {
            return menu;
        }
        let stage = stage.clone();
        let shown = tools.transcript_shown;
        menu.item(
            toolbar::menu_item(
                "call-transcript-toggle",
                Some(Glyph::CALL_TRANSCRIPT),
                if shown { "Hide transcript" } else { "Show transcript" },
            )
            .checked(shown)
            .on_click(move |_, _, cx| {
                let _ = stage.update(cx, |stage, cx| stage.show_transcript(!shown, cx));
            }),
        )
    })
    .separator()
}

/// Start recording, or Stop recording while it records.
fn recording_item(menu: PopupMenu, stage: &WeakEntity<CallStage>, tools: RecordTools) -> PopupMenu {
    let stage = stage.clone();
    let busy = tools.phase != RecordingPhase::Idle;
    if tools.recording {
        menu.item(
            toolbar::menu_item("call-record-stop", Some(Glyph::RECORD_STOP), "Stop recording")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = stage.update(cx, |stage, cx| stage.confirm_stop_recording(true, cx));
                }),
        )
    } else {
        menu.item(
            toolbar::menu_item("call-record-start", Some(Glyph::RECORD), "Start recording")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = stage.update(cx, CallStage::open_record_dialog);
                }),
        )
    }
}

/// Start transcription, or Stop transcription while it transcribes.
fn transcription_item(menu: PopupMenu, stage: &WeakEntity<CallStage>, tools: RecordTools) -> PopupMenu {
    let stage = stage.clone();
    let busy = tools.phase != RecordingPhase::Idle;
    if tools.transcribing {
        menu.item(
            toolbar::menu_item("call-transcribe-stop", Some(Glyph::RECORD_STOP), "Stop transcription")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = stage.update(cx, |stage, cx| stage.confirm_stop_transcription(true, cx));
                }),
        )
    } else {
        menu.item(
            toolbar::menu_item("call-transcribe-start", Some(Glyph::TRANSCRIBE), "Start transcription")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = stage.update(cx, CallStage::open_transcribe_dialog);
                }),
        )
    }
}

impl CallStage {
    /// What More's Record and transcribe offers now.
    pub(super) fn record_tools(&self, cx: &App) -> RecordTools {
        let calls = self.calls.read(cx);
        let live = self.live(cx);
        let transcribing = live.as_ref().is_some_and(|call| call.meeting.transcribing.is_some());
        let transcript_shown = self.pane == Pane::Transcript;
        RecordTools {
            available: !self.core.read_only()
                && self.core.capabilities().recording
                && live
                    .as_ref()
                    .is_some_and(|call| call.status == CallStatus::Connected && call.kind != CallKind::TestCall),
            recording: live.is_some_and(|call| call.meeting.recording.is_some()),
            transcribing,
            transcript: transcribing || transcript_shown || !self.record.transcript.is_empty(),
            transcript_shown,
            phase: calls.recording().phase,
        }
    }

    /// The spoken language a start dialog asks in first: the captions', or English (US).
    fn first_language(&self, cx: &App) -> String {
        let captions = &self.calls.read(cx).captions().spoken_language;
        if SPOKEN_LANGUAGES.iter().any(|(code, _)| code == captions) {
            captions.clone()
        } else {
            DEFAULT_LANGUAGE.to_owned()
        }
    }

    fn open_start(&mut self, start: Start, cx: &mut Context<Self>) {
        self.record.dialog = Some(StartDialog {
            start,
            language: self.first_language(cx),
            audio_only: false,
            more: false,
        });
        cx.notify();
    }

    /// Start recording: "Start recording and transcription" asks the spoken language first; while a transcription runs,
    /// "Start recording" asks what to record.
    pub fn open_record_dialog(&mut self, cx: &mut Context<Self>) {
        let transcribing = self.live(cx).is_some_and(|call| call.meeting.transcribing.is_some());
        self.open_start(if transcribing { Start::Recording } else { Start::Both }, cx);
    }

    /// Start transcription: "What language is everyone speaking?" asks first.
    pub fn open_transcribe_dialog(&mut self, cx: &mut Context<Self>) {
        self.open_start(Start::Transcription, cx);
    }

    /// The start dialog put away, as its Cancel and Close do.
    pub fn close_record_dialog(&mut self, cx: &mut Context<Self>) {
        self.record.dialog = None;
        cx.notify();
    }

    /// "Choose what to record": the audio alone (`true`) or video and audio; it opens "More options" where it is under
    /// them.
    pub fn choose_audio_only(&mut self, audio_only: bool, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.record.dialog {
            dialog.audio_only = audio_only;
            dialog.more = true;
            cx.notify();
        }
    }

    /// "Start recording and transcription"'s More options opened or closed.
    fn toggle_more_options(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.record.dialog {
            dialog.more = !dialog.more;
            cx.notify();
        }
    }

    /// The start dialog's spoken language picked.
    fn pick_record_language(&mut self, code: &str, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.record.dialog {
            code.clone_into(&mut dialog.language);
            cx.notify();
        }
    }

    /// The start dialog's Confirm: the recording or the transcription starts as picked, named as Teams names a call's
    /// ("Call with …") or a meeting's (its title).
    fn confirm_record(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.record.dialog.take() else {
            return;
        };
        let Some(active) = self.live(cx) else { return };
        let title = self.title(&active, cx);
        let (title, organizer) = if active.is_meeting() {
            (
                title.to_string(),
                active.organizer.then(|| self.own_name.clone()).flatten(),
            )
        } else {
            (format!("Call with {title}"), None)
        };
        let organizer = organizer.map(|name| name.to_string());
        self.record.pending = Some(super::transcript::Pending::new(dialog.start, dialog.audio_only));
        self.record.pending_closed = false;
        let ask = match dialog.start {
            Start::Transcription => Ask::Transcribe {
                title,
                organizer,
                language: dialog.language,
            },
            Start::Both | Start::Recording => Ask::Record {
                title,
                organizer,
                language: dialog.language,
                audio_only: dialog.audio_only,
            },
        };
        self.ask(ask, cx);
        cx.notify();
    }

    /// "Stop recording and transcription?" shown (`true`, as Stop recording asks first) or put away (Cancel).
    pub fn confirm_stop_recording(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.record.confirm_stop = shown;
        cx.notify();
    }

    /// "Stop transcription?" shown (`true`, as Stop transcription asks first) or put away (Cancel).
    pub fn confirm_stop_transcription(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.record.confirm_stop_transcription = shown;
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.record.confirm_stop = false;
        self.record.stopping = true;
        self.ask(Ask::StopRecording, cx);
        cx.notify();
    }

    fn stop_transcription(&mut self, cx: &mut Context<Self>) {
        self.record.confirm_stop_transcription = false;
        self.ask(Ask::StopTranscription, cx);
        cx.notify();
    }

    /// A start or a stop that failed since last seen shows its notice.
    pub(super) fn record_problem_seen(&mut self, cx: &App) {
        let problem = self.calls.read(cx).recording().problem;
        if problem != self.record.problem_seen {
            self.record.problem = problem;
            self.record.problem_seen = problem;
            if problem.is_some() {
                self.record.stopping = false;
            }
        }
    }

    /// The start dialog asked, over the whole window (see the module docs).
    pub(super) fn record_dialog_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.record.dialog.clone()?;
        let (prefix, heading) = match dialog.start {
            Start::Both => ("call-record", "Start recording and transcription"),
            Start::Recording => ("call-record", "Start recording"),
            Start::Transcription => ("call-transcribe", "What language is everyone speaking?"),
        };
        let close = icon_button(SharedString::from(format!("{prefix}-close")), Glyph::X, "Close")
            .on_click(cx.listener(|this, _, _, cx| this.close_record_dialog(cx)));
        let cancel = Button::new(SharedString::from(format!("{prefix}-cancel")))
            .label("Cancel")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.close_record_dialog(cx)));
        let confirm = Button::new(SharedString::from(format!("{prefix}-confirm")))
            .label("Confirm")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| this.confirm_record(cx)));
        let body = if dialog.start == Start::Recording {
            what_row(dialog.audio_only, palette, cx)
        } else {
            Self::language_body(&dialog, prefix, palette, cx)
        };
        let dialog = dialog_box(
            if prefix == "call-record" {
                "call-record-dialog"
            } else {
                "call-transcribe-dialog"
            },
            palette,
        )
        .child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_2()
                .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child(heading))
                .child(selected_by(format!("{prefix}-close"), close)),
        )
        .child(body)
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(selected_by(format!("{prefix}-cancel"), cancel))
                .child(selected_by(format!("{prefix}-confirm"), confirm)),
        );
        Some(scrim("call-record-scrim", dialog))
    }

    /// A start dialog's spoken language: what it is for, its picker, and "Start recording and transcription"'s More
    /// options with what to record.
    fn language_body(
        dialog: &StartDialog,
        prefix: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let picker = Self::language_picker(&dialog.language, prefix, cx);
        let more = (dialog.start == Start::Both).then(|| {
            let toggle = Button::new("call-record-more")
                .ghost()
                .small()
                .child(
                    div().flex().items_center().gap_1().child("More options").child(
                        if dialog.more {
                            Glyph::CHEVRON_UP
                        } else {
                            Glyph::CHEVRON_DOWN
                        }
                        .icon()
                        .size(px(14.0)),
                    ),
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_more_options(cx)));
            div()
                .pt_2()
                .flex()
                .flex_col()
                .items_start()
                .gap_2()
                .child(selected_by("call-record-more", toggle))
                .when(dialog.more, |more| more.child(what_row(dialog.audio_only, palette, cx)))
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .child("Select the correct language to ensure the accuracy of features that process audio."),
            )
            .child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Spoken language"),
            )
            .child(selected_by(format!("{prefix}-language"), picker))
            .children(more)
    }

    /// A start dialog's Spoken language: the language picked, its dropdown listing every one.
    fn language_picker(language: &str, prefix: &'static str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let language = language.to_owned();
        let label = SPOKEN_LANGUAGES
            .iter()
            .find(|(code, _)| *code == language)
            .map_or("English (US)", |(_, label)| *label);
        let stage = cx.weak_entity();
        Button::new(SharedString::from(format!("{prefix}-language")))
            .outline()
            .w_full()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label)
                    .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
            )
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu: PopupMenu, _, _| {
                for &(code, label) in SPOKEN_LANGUAGES {
                    let stage = stage.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .debug_selector(move || format!("{prefix}-language-{code}"))
                                .child(label)
                        })
                        .checked(code == language)
                        .on_click(move |_, _, cx| {
                            let _ = stage.update(cx, |stage, cx| stage.pick_record_language(code, cx));
                        }),
                    );
                }
                menu.scrollable(true).max_h(px(360.0))
            })
    }

    /// The stop asked, over the whole window: "Stop recording and transcription?" (or "Stop recording?" while no
    /// transcription runs) or "Stop transcription?"; Stop stops it, Cancel leaves it.
    pub(super) fn stop_recording_confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (prefix, heading) = if self.record.confirm_stop {
            let transcribing = self.live(cx).is_some_and(|call| call.meeting.transcribing.is_some());
            (
                "call-record-stop",
                if transcribing {
                    "Stop recording and transcription?"
                } else {
                    "Stop recording?"
                },
            )
        } else if self.record.confirm_stop_transcription {
            ("call-transcribe-stop", "Stop transcription?")
        } else {
            return None;
        };
        let transcription = prefix == "call-transcribe-stop";
        let cancel = Button::new(SharedString::from(format!("{prefix}-cancel")))
            .label("Cancel")
            .outline()
            .on_click(cx.listener(move |this, _, _, cx| {
                if transcription {
                    this.confirm_stop_transcription(false, cx);
                } else {
                    this.confirm_stop_recording(false, cx);
                }
            }));
        let stop = Button::new(SharedString::from(format!("{prefix}-confirm")))
            .label("Stop")
            .primary()
            .on_click(cx.listener(move |this, _, _, cx| {
                if transcription {
                    this.stop_transcription(cx);
                } else {
                    this.stop_recording(cx);
                }
            }));
        let dialog = dialog_box(
            if transcription {
                "call-transcribe-stop-dialog"
            } else {
                "call-record-stop-dialog"
            },
            palette,
        )
        .w(px(400.0))
        .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child(heading))
        .child(
            div()
                .pt_4()
                .flex()
                .justify_end()
                .gap_2()
                .child(selected_by(format!("{prefix}-cancel"), cancel))
                .child(selected_by(format!("{prefix}-confirm"), stop)),
        );
        Some(scrim("call-record-stop-scrim", dialog))
    }

    /// The recording's notices besides the one of its start: that it stopped, and a start or stop that failed; and the
    /// transcription's start and stop ([`super::transcript`]).
    pub(super) fn recording_notices(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let stopped = self.record.stopped.map(|stopped| {
            let line = if stopped.by_me {
                "You stopped recording. If you change your mind, you can start them again anytime."
            } else {
                "The recording will be saved to the cloud."
            };
            meeting::notice(
                (
                    "call-recording-stopped",
                    if stopped.audio { Glyph::MIC } else { Glyph::RECORD },
                    "Recording and transcription have stopped.",
                ),
                SharedString::from(line),
                palette,
                cx.listener(|this, _, _, cx| {
                    this.record.stopped = None;
                    cx.notify();
                }),
            )
        });
        let problem = self.record.problem.map(|RecordingProblem { heading, detail }| {
            meeting::notice(
                ("call-recording-problem", Glyph::CIRCLE_ALERT, heading),
                SharedString::from(detail),
                palette,
                cx.listener(|this, _, _, cx| {
                    this.record.problem = None;
                    cx.notify();
                }),
            )
        });
        let transcription = self.transcription_notice(active, palette, cx);
        let pending = self.pending_notice(palette, cx);
        problem
            .into_iter()
            .chain(pending)
            .chain(stopped)
            .chain(transcription)
            .collect()
    }
}

/// "Choose what to record" with its dropdown: Video and audio, or Audio only.
fn what_row(audio_only: bool, palette: Palette, cx: &mut Context<CallStage>) -> gpui::Div {
    let stage = cx.weak_entity();
    let picker = Button::new("call-record-what")
        .outline()
        .w(px(240.0))
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .child(if audio_only { "Audio only" } else { "Video and audio" })
                .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
        )
        .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu: PopupMenu, _, _| {
            [("video", "Video and audio", false), ("audio", "Audio only", true)]
                .into_iter()
                .fold(menu, |menu, (key, label, audio)| {
                    let stage = stage.clone();
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .debug_selector(move || format!("call-record-what-{key}"))
                                .child(label)
                        })
                        .checked(audio == audio_only)
                        .on_click(move |_, _, cx| {
                            let _ = stage.update(cx, |stage, cx| stage.choose_audio_only(audio, cx));
                        }),
                    )
                })
        });
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .child(div().text_sm().child("Choose what to record"))
        .child(selected_by("call-record-what", picker))
}

/// A dialog's box, as Teams draws one: a raised, rounded surface.
fn dialog_box(selector: &'static str, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .occlude()
        .w(px(520.0))
        .max_w_full()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_color(rgb(palette.foreground))
        .shadow_lg()
}

/// `dialog` centred over the whole window, which it dims.
fn scrim(id: &'static str, dialog: gpui::Div) -> AnyElement {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .bg(gpui::black().opacity(0.4))
        .occlude()
        .child(dialog)
        .into_any_element()
}
