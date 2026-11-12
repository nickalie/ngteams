//! The box's side of correcting a draft ([`crate::message_correct`]): Correct (the toolbar's button or Ctrl+Shift+G) sends
//! what the box holds to the tool the settings name, and shows what comes back over the box, word by word, beside the
//! draft as written.
//!
//! - **Nothing is sent, and the draft isn't touched, until the user says so**: "Use correction" (Enter) puts it in the box
//!   as one edit, so Ctrl+Z takes it back; "Send corrected" (Ctrl+Enter) puts it there and sends it; "Keep original"
//!   (Escape) goes back to the draft.
//!   A result leaves focus in the editor: press Correct again to focus the preview and use its keys. An arriving
//!   proposal must never turn a key meant to send the original draft into sending an unread rewrite.
//! - **A correction is of the draft it read**: the box notices when the draft changes, while the tool works or over the
//!   proposal, and drops the correction, saying so; a result that comes after that is thrown away, and "Use correction"
//!   checks again before it writes.
//! - **Per box**: a chat's box stays alive with its draft across navigation, so a correction running while another chat is
//!   open is waiting there when the user comes back, and it can't land in any other box.
//! - Stopping it (the button again, Escape, or sending the draft) ends the tool's process.

use std::mem;

use gpui::{
    AnyElement, App, Context, FocusHandle, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement as _,
    KeyBinding, ParentElement as _, SharedString, StatefulInteractiveElement as _, StrikethroughStyle, Styled as _,
    StyledText, Window, actions, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, Size};

use super::{Compose, Mode, TOOL};
use crate::chat_translate::prompt::Said;
use crate::editor::doc::Doc;
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::local_ai::{Failure, Tools, Work};
use crate::message_correct::diff::Change;
use crate::message_correct::draft::{self, Encoded};
use crate::message_correct::{Correcting, Limits, MAX_DRAFT, Outcome, Proposal, Request};
use crate::theme::{self, Palette};
use crate::trace::TraceLine;

actions!(
    compose,
    [
        /// Asks the AI for a correction of the draft, or stops the one running.
        CorrectDraft,
        /// Puts the proposed correction in the box.
        UseCorrection,
        /// Puts the proposed correction in the box and sends it.
        SendCorrection,
        /// Dismisses the proposed correction, keeping the draft as written.
        KeepOriginal
    ]
);

/// The proposal's key context, inside the box's.
const PROPOSAL_CONTEXT: &str = "ComposeCorrection";
/// The tallest the proposal's text gets before it scrolls, px.
const PROPOSAL_MAX: f32 = 200.0;
/// What the button says while it can start a correction.
const CORRECT_TIP: &str = "Correct spelling and style";

/// Binds the keys of correcting a draft.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-shift-g", CorrectDraft, Some(super::CONTEXT)),
        KeyBinding::new("enter", UseCorrection, Some(PROPOSAL_CONTEXT)),
        KeyBinding::new("ctrl-enter", SendCorrection, Some(PROPOSAL_CONTEXT)),
        KeyBinding::new("escape", KeepOriginal, Some(PROPOSAL_CONTEXT)),
    ]);
}

/// Where a box's correction stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorrectionStatus {
    Idle,
    /// The tool is working on the draft.
    Running,
    /// A correction is shown for the user to take or leave.
    Proposed,
    /// The tool found nothing to correct.
    NothingToCorrect,
    /// The draft changed, so its correction was dropped.
    DraftChanged,
    /// Why the correction failed.
    Failed(SharedString),
}

/// A proposal shown over the box.
struct Shown {
    draft: Doc,
    proposal: Proposal,
    /// Showing the words changed, not the text as it would be.
    changes: bool,
}

impl Shown {
    fn text(&self) -> &str {
        if self.changes {
            &self.proposal.preview.changes
        } else {
            &self.proposal.preview.result
        }
    }
}

/// Where a box's correction stands, with what that needs.
#[derive(Default)]
enum State {
    #[default]
    Idle,
    Running {
        draft: Doc,
        /// What the box calls the tool.
        tool: SharedString,
        _waiting: gpui::Task<()>,
        _correcting: Correcting,
    },
    Proposed(Box<Shown>),
    Nothing {
        draft: Doc,
    },
    Changed,
    Failed {
        draft: Doc,
        why: SharedString,
        /// The tool wasn't found: Try again looks for it again.
        missing: bool,
    },
}

impl State {
    /// The draft this is about.
    fn draft(&self) -> Option<&Doc> {
        match self {
            Self::Running { draft, .. } | Self::Nothing { draft } | Self::Failed { draft, .. } => Some(draft),
            Self::Proposed(shown) => Some(&shown.draft),
            Self::Idle | Self::Changed => None,
        }
    }
}

/// The box's correction.
pub(super) struct Correction {
    state: State,
    /// The proposal's own focus: its keys after the user deliberately focuses it.
    focus: FocusHandle,
    limits: Limits,
}

impl Correction {
    pub(super) fn new(cx: &mut Context<Compose>) -> Self {
        Self {
            state: State::Idle,
            focus: cx.focus_handle(),
            limits: Limits::default(),
        }
    }
}

impl Compose {
    /// How long the tool may take to correct a draft (tests).
    pub fn set_correction_limits(&mut self, limits: Limits) {
        self.correction.limits = limits;
    }

    /// Where the box's correction stands.
    #[must_use]
    pub fn correction_status(&self) -> CorrectionStatus {
        match &self.correction.state {
            State::Idle => CorrectionStatus::Idle,
            State::Running { .. } => CorrectionStatus::Running,
            State::Proposed(_) => CorrectionStatus::Proposed,
            State::Nothing { .. } => CorrectionStatus::NothingToCorrect,
            State::Changed => CorrectionStatus::DraftChanged,
            State::Failed { why, .. } => CorrectionStatus::Failed(why.clone()),
        }
    }

    /// The correction shown for the user to take or leave.
    #[must_use]
    pub fn correction_proposal(&self) -> Option<&Proposal> {
        match &self.correction.state {
            State::Proposed(shown) => Some(&shown.proposal),
            _ => None,
        }
    }

    /// The text currently displayed in the correction preview.
    #[must_use]
    pub fn correction_preview_text(&self) -> Option<&str> {
        match &self.correction.state {
            State::Proposed(shown) => Some(shown.text()),
            _ => None,
        }
    }

    /// The button or its key: a correction asked for, the one running stopped, or the one shown given the keyboard.
    pub fn correct_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.correction.state {
            State::Running { .. } => self.end_correction(window, cx),
            State::Proposed(_) => window.focus(&self.correction.focus, cx),
            _ => self.start_correction(window, cx),
        }
    }

    /// Whether the box holds what a correction can start from.
    fn can_correct(&self, cx: &App) -> bool {
        !self.editor.read(cx).is_blank() && !self.translating() && self.starting.is_none()
    }

    fn start_correction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only.is_some() || !self.can_correct(cx) {
            return;
        }
        let doc = self.editor.read(cx).doc().clone();
        let encoded = draft::encode(&doc);
        if !encoded.has_words() {
            self.correction.state = State::Nothing { draft: doc };
            return cx.notify();
        }
        if encoded.text.len() > MAX_DRAFT {
            self.correction.state = State::Failed {
                draft: doc,
                why: "This message is too long to correct.".into(),
                missing: false,
            };
            return cx.notify();
        }
        self.run_correction(doc, encoded, window, cx);
    }

    fn run_correction(&mut self, doc: Doc, encoded: Encoded, window: &mut Window, cx: &mut Context<Self>) {
        let choice = self.settings.read(cx).settings().ai(Work::Correction).clone();
        let (backend, program) = match Tools::of(cx).pick(choice.tool) {
            Ok(picked) => picked,
            Err(failure) => return self.correction_failed(doc, &failure, cx),
        };
        let correcting = Correcting::start(Request {
            backend,
            program,
            model: choice.model.clone(),
            context: self.correction_context(cx),
            draft: encoded,
            limits: self.correction.limits,
        });
        let done = correcting.done();
        let waiting = cx.spawn_in(window, async move |this, cx| {
            if let Ok(outcome) = done.recv().await {
                let _ = this.update_in(cx, |this, _, cx| this.correction_came(outcome, cx));
            }
        });
        self.correction.state = State::Running {
            draft: doc,
            tool: backend.label_of(Work::Correction, &choice.model).into(),
            _waiting: waiting,
            _correcting: correcting,
        };
        self.trace
            .record(&TraceLine::new("compose", "correcting").conversation(&self.conversation));
        cx.notify();
    }

    /// What the tool reads around the draft: the chat's recent messages, and the message it replies to.
    fn correction_context(&self, cx: &App) -> Vec<Said> {
        let mut context = self
            .translated
            .model
            .as_ref()
            .map(|model| model.read(cx).recent_context())
            .unwrap_or_default();
        if let Mode::Reply(quoted) = &self.mode {
            context.push(Said::new(quoted.sender_name.clone(), quoted.preview.clone()));
        }
        context
    }

    fn correction_failed(&mut self, draft: Doc, failure: &Failure, cx: &mut Context<Self>) {
        let missing = matches!(failure, Failure::NotInstalled(_));
        let hint = if missing {
            " Install it, or choose another tool in Settings under General."
        } else {
            ""
        };
        self.correction.state = State::Failed {
            draft,
            why: format!("Your message wasn't corrected. {}{hint}", failure.message()).into(),
            missing,
        };
        cx.notify();
    }

    /// The tool's outcome: thrown away unless the box still holds the draft it read. The proposal waits for the user
    /// to focus it deliberately, preserving the meaning of the editor's send key when the answer arrives.
    fn correction_came(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        if !self.correction_holds(cx) {
            return;
        }
        let State::Running { draft, .. } = mem::take(&mut self.correction.state) else {
            return;
        };
        match outcome {
            Err(failure) => return self.correction_failed(draft, &failure, cx),
            Ok(None) => self.correction.state = State::Nothing { draft },
            Ok(Some(proposal)) => {
                self.correction.state = State::Proposed(Box::new(Shown {
                    draft,
                    proposal,
                    changes: true,
                }));
            }
        }
        cx.notify();
    }

    /// Whether what the correction is about is still what the box holds; if not, it is dropped: told when the user could
    /// have been counting on it, unless the box was emptied.
    fn correction_holds(&mut self, cx: &mut Context<Self>) -> bool {
        let editor = self.editor.read(cx);
        if matches!(self.correction.state, State::Changed) && editor.is_blank() {
            self.correction.state = State::Idle;
            cx.notify();
            return true;
        }
        let Some(draft) = self.correction.state.draft() else {
            return true;
        };
        if draft == editor.doc() {
            return true;
        }
        let counted_on = matches!(self.correction.state, State::Running { .. } | State::Proposed(_));
        self.correction.state = if counted_on && !editor.is_blank() {
            State::Changed
        } else {
            State::Idle
        };
        cx.notify();
        false
    }

    /// What the box holds may have changed: a correction of something else goes.
    pub(super) fn correction_follows_draft(&mut self, cx: &mut Context<Self>) {
        self.correction_holds(cx);
    }

    /// Whatever the correction was, it ends (the box sent, or was emptied).
    pub(super) fn forget_correction(&mut self) {
        self.correction.state = State::Idle;
    }

    /// Whether Escape has a correction to end.
    pub(super) fn correction_on_show(&self) -> bool {
        !matches!(self.correction.state, State::Idle)
    }

    /// Ends the correction, its process if it runs; the box takes the keyboard back.
    pub fn end_correction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.forget_correction();
        self.focus(window, cx);
        cx.notify();
    }

    /// "Use correction": the proposal goes in the box as one edit, the draft it read still there.
    pub fn use_correction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.take_correction(window, cx);
    }

    /// Puts the proposal in the box; whether there was one to put.
    fn take_correction(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.correction_holds(cx) || !matches!(self.correction.state, State::Proposed(_)) {
            return false;
        }
        let State::Proposed(shown) = mem::take(&mut self.correction.state) else {
            return false;
        };
        let corrected = shown.proposal.corrected;
        self.editor.update(cx, |editor, cx| editor.replace_doc(corrected, cx));
        self.trace
            .record(&TraceLine::new("compose", "correction used").conversation(&self.conversation));
        self.focus(window, cx);
        cx.notify();
        true
    }

    /// "Send corrected": the proposal goes in the box and is sent as if the user had pressed Send; nothing is sent when
    /// the draft changed under it.
    pub fn send_correction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.take_correction(window, cx) {
            self.send(window, cx);
        }
    }

    /// Try again after a failure, or Check again after the draft changed; tools are looked for again if one was missing.
    pub fn retry_correction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.correction.state, State::Failed { missing: true, .. }) {
            cx.set_global(Tools::detect());
        }
        self.correction.state = State::Idle;
        self.start_correction(window, cx);
    }

    /// Switches the proposal between the words changed and the text as it would be.
    pub fn toggle_correction_view(&mut self, cx: &mut Context<Self>) {
        if let State::Proposed(shown) = &mut self.correction.state {
            shown.changes = !shown.changes;
            cx.notify();
        }
    }

    // --- drawing ---

    /// Correct, in the box's toolbar: lit while a correction runs or is shown, when it stops or takes the keyboard.
    pub(super) fn correct_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = matches!(self.correction.state, State::Running { .. });
        let lit = busy || matches!(self.correction.state, State::Proposed(_));
        let tip = if busy { "Stop correcting" } else { CORRECT_TIP };
        let button = icon_button_with_key("compose-correct", Glyph::SPELL_CHECK, tip, &CorrectDraft)
            .with_size(Size::Size(px(TOOL)))
            .selected(lit)
            .disabled(!lit && !self.can_correct(cx))
            .on_click(cx.listener(|this, _, window, cx| this.correct_draft(window, cx)));
        selected_by("compose-correct", button).into_any_element()
    }

    /// What the box says of its correction, over it.
    pub(super) fn correction_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.correction.state {
            State::Idle => None,
            State::Running { tool, .. } => Some(Self::running_line(tool, palette, cx)),
            State::Proposed(shown) => Some(self.proposal_view(shown, palette, cx)),
            State::Nothing { .. } => Some(Self::clean_line(palette, cx)),
            State::Changed => Some(Self::changed_line(palette, cx)),
            State::Failed { why, .. } => Some(Self::failed_line(why.clone(), cx)),
        }
    }

    fn running_line(tool: &SharedString, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        notice("compose-correcting", Glyph::SPELL_CHECK, palette.muted_foreground)
            .text_color(rgb(palette.muted_foreground))
            .child(words(format!("Checking your message with {tool}…").into()))
            .child(Self::dismiss_button("Stop correcting", cx))
            .into_any_element()
    }

    fn clean_line(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        notice("compose-correction-clean", Glyph::CHECK, theme::SUCCESS)
            .text_color(rgb(palette.muted_foreground))
            .child(words("No corrections needed.".into()))
            .child(Self::dismiss_button("Dismiss", cx))
            .into_any_element()
    }

    fn changed_line(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        notice("compose-correction-changed", Glyph::INFO, palette.muted_foreground)
            .text_color(rgb(palette.muted_foreground))
            .child(words("Your message changed, so its correction was dropped.".into()))
            .child(Self::retry_button("Check again", cx))
            .child(Self::dismiss_button("Dismiss", cx))
            .into_any_element()
    }

    fn failed_line(why: SharedString, cx: &mut Context<Self>) -> AnyElement {
        notice("compose-correction-failed", Glyph::SPELL_CHECK, theme::DANGER)
            .border_l_2()
            .border_color(rgb(theme::DANGER))
            .child(words(why))
            .child(Self::retry_button("Try again", cx))
            .child(Self::dismiss_button("Dismiss", cx))
            .into_any_element()
    }

    fn retry_button(label: &'static str, cx: &mut Context<Self>) -> AnyElement {
        selected_by(
            "compose-correction-retry",
            Button::new("compose-correction-retry")
                .label(label)
                .outline()
                .with_size(Size::Small)
                .on_click(cx.listener(|this, _, window, cx| this.retry_correction(window, cx))),
        )
        .into_any_element()
    }

    fn dismiss_button(tip: &'static str, cx: &mut Context<Self>) -> AnyElement {
        selected_by(
            "compose-correction-dismiss",
            icon_button("compose-correction-dismiss", Glyph::X, tip)
                .with_size(Size::XSmall)
                .on_click(cx.listener(|this, _, window, cx| this.end_correction(window, cx))),
        )
        .into_any_element()
    }

    /// The proposal: what changes, and what the user can do with it.
    fn proposal_view(&self, shown: &Shown, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .debug_selector(|| "compose-correction".to_owned())
            .key_context(PROPOSAL_CONTEXT)
            .track_focus(&self.correction.focus)
            .on_action(cx.listener(|this, _: &UseCorrection, window, cx| this.use_correction(window, cx)))
            .on_action(cx.listener(|this, _: &SendCorrection, window, cx| this.send_correction(window, cx)))
            .on_action(cx.listener(|this, _: &KeepOriginal, window, cx| this.end_correction(window, cx)))
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .child(Self::proposal_header(shown, palette, cx))
            .child(
                div()
                    .id("compose-correction-text")
                    .debug_selector(|| "compose-correction-text".to_owned())
                    .max_h(px(PROPOSAL_MAX))
                    .overflow_y_scroll()
                    .text_sm()
                    .child(proposal_text(shown, palette)),
            )
            .child(self.proposal_actions(cx))
            .into_any_element()
    }

    fn proposal_header(shown: &Shown, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let (glyph, tip) = if shown.changes {
            (Glyph::EYE, "Show the corrected text")
        } else {
            (Glyph::SPELL_CHECK, "Show what changed")
        };
        let toggle = selected_by(
            "compose-correction-view",
            icon_button("compose-correction-view", glyph, tip)
                .with_size(Size::XSmall)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_correction_view(cx))),
        );
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(Glyph::SPELL_CHECK.icon().size(px(14.0)).text_color(rgb(palette.brand)))
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Suggested correction"),
            )
            .child(toggle)
            .into_any_element()
    }

    fn proposal_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let send_label = if matches!(self.mode, Mode::Edit { .. }) {
            "Save corrected"
        } else {
            "Send corrected"
        };
        let use_it = Button::new("compose-correction-use")
            .label("Use correction")
            .primary()
            .with_size(Size::Small)
            .tooltip_with_action("Put it in the box", &UseCorrection, None)
            .on_click(cx.listener(|this, _, window, cx| this.use_correction(window, cx)));
        let send = Button::new("compose-correction-send")
            .label(send_label)
            .outline()
            .with_size(Size::Small)
            .tooltip_with_action(send_label, &SendCorrection, None)
            .on_click(cx.listener(|this, _, window, cx| this.send_correction(window, cx)));
        let keep = Button::new("compose-correction-keep")
            .label("Keep original")
            .ghost()
            .with_size(Size::Small)
            .tooltip_with_action("Dismiss the correction", &KeepOriginal, None)
            .on_click(cx.listener(|this, _, window, cx| this.end_correction(window, cx)));
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(selected_by("compose-correction-use", use_it))
            .child(selected_by("compose-correction-send", send))
            .child(selected_by("compose-correction-keep", keep))
            .into_any_element()
    }
}

/// A line over the box.
fn notice(selector: &'static str, glyph: Glyph, tint: u32) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .child(glyph.icon().size(px(14.0)).text_color(rgb(tint)))
}

fn words(text: SharedString) -> gpui::Div {
    div().flex_1().min_w_0().child(text)
}

/// The proposal's text: the words taken out struck through and those put in highlighted, or the text as it would be.
fn proposal_text(shown: &Shown, palette: Palette) -> StyledText {
    let text = StyledText::new(shown.text().to_owned());
    if !shown.changes {
        return text;
    }
    let highlights = shown
        .proposal
        .preview
        .marks
        .iter()
        .map(|(range, change)| (range.clone(), change_style(*change, palette)))
        .collect::<Vec<_>>();
    text.with_highlights(highlights)
}

/// How a change is marked. Not by colour alone: what goes is struck through, what comes is bold.
fn change_style(change: Change, palette: Palette) -> HighlightStyle {
    match change {
        Change::Removed => {
            let color = theme::mix(palette.foreground, theme::DANGER, 0.7);
            HighlightStyle {
                color: Some(rgb(color).into()),
                background_color: Some(rgb(theme::mix(palette.surface, theme::DANGER, 0.14)).into()),
                strikethrough: Some(StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(rgb(color).into()),
                }),
                ..HighlightStyle::default()
            }
        }
        Change::Added => HighlightStyle {
            color: Some(rgb(palette.foreground).into()),
            background_color: Some(rgb(theme::mix(palette.surface, theme::SUCCESS, 0.28)).into()),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..HighlightStyle::default()
        },
    }
}
