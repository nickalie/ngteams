//! The recap's AI summary pane ([`crate::recap_summary`]): offered with what it sends where, made with its progress and
//! Cancel, shown as Fathom lays a summary out (each moment a click away in the recording), copied, and sent to the
//! meeting's chat after a preview the user confirms.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local};
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Sizable as _};
use teams_core::Action;
use teams_proto::chat::MessageText;
use teams_proto::recap::RecapPart;

use super::{Pane, Post, Read, RecapView, centred, event_row, spinner};
use crate::clock;
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::local_ai::{AiChoice, Backend, Failure, Tools, Work};
use crate::recap::local_line_of;
use crate::recap_summary::{self, Input, Made, Point, RecapSummary, State, Step, Topic, cache_key, stamp};
use crate::rich_text::{self, Mentions};
use crate::theme::Palette;

/// What the pane says of a part without a transcript.
pub const NO_TRANSCRIPT: &str = "There's nothing to summarize: this recording has no transcript.";
/// What the pane says of a call transcribed alone whose transcript Teams has nothing of (`docs/protocol/recap.md`).
pub const NO_TEAMS_TRANSCRIPT: &str = "There's nothing to summarize: Teams has no transcript of this call to read.";
/// What the pane says of a transcript in which nobody spoke.
pub const EMPTY_TRANSCRIPT: &str = "There's nothing to summarize: the transcript is empty.";
/// What the pane says when the transcript couldn't be read.
pub const TRANSCRIPT_UNREAD: &str = "The transcript couldn't be loaded, so there's nothing to summarize yet.";
/// What a summary that couldn't be made says first.
pub const SUMMARY_FAILED: &str = "The summary couldn't be made.";
/// The note once the summary was sent.
pub const SENT: &str = "Sent the summary to the meeting chat.";
/// The note once the summary was copied.
pub const COPIED: &str = "Copied the summary.";

impl RecapView {
    /// The shown part's summary, kept by its recording (made when first asked for, followed while the view lives).
    pub fn summary(&mut self, cx: &mut Context<Self>) -> Option<Entity<RecapSummary>> {
        let key = cache_key(self.part()?)?;
        if let Some((summary, _)) = self.summaries.get(&key) {
            return Some(summary.clone());
        }
        let dir = self
            .settings
            .as_ref()
            .and_then(|settings| settings.read(cx).summaries_dir());
        let summary = RecapSummary::of(&key, dir, cx);
        let watched = cx.observe(&summary, |_, _, cx| cx.notify());
        self.summaries.insert(key, (summary.clone(), watched));
        Some(summary)
    }

    /// The pane shown beside the recording: the one picked, else the AI summary once one was asked for (made, being
    /// made or failed) or when the part has no transcript, else the transcript.
    pub fn pane(&mut self, cx: &mut Context<Self>) -> Pane {
        if let Some(pane) = self.pane {
            return pane;
        }
        if self.part().is_some_and(|part| part.transcript.is_none()) {
            return Pane::AiSummary;
        }
        let asked = self
            .summary(cx)
            .is_some_and(|summary| !matches!(summary.read(cx).state(), State::Idle));
        if asked { Pane::AiSummary } else { Pane::Transcript }
    }

    /// Shows `pane`.
    pub fn show_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = Some(pane);
        cx.notify();
    }

    /// The tool the settings pick for summaries.
    fn summary_choice(&self, cx: &Context<Self>) -> AiChoice {
        self.settings.as_ref().map_or_else(AiChoice::default, |settings| {
            settings.read(cx).settings().recap_summarizer.clone()
        })
    }

    /// The session's day and span, as the recap's top row says it.
    fn when(&self, cx: &Context<Self>) -> Option<String> {
        local_line_of(self.parts()?, &self.session(), clock::cycle(cx))
    }

    /// What the summary is made from: the shown part's transcript and the meeting's brief; `None` before the
    /// transcript was read.
    fn summary_input(&self, cx: &Context<Self>) -> Option<Input> {
        let transcript = self.transcript()?;
        // the part's place among its session's, as the parts' row names it
        let session = self.session();
        let position = session.iter().position(|&index| index == self.selected)?;
        let part = (session.len() > 1).then_some((position + 1, session.len()));
        let when = self.when(cx);
        let brief = recap_summary::brief(&self.title, when.as_deref(), part, &transcript.turns);
        Some(Input::of(&transcript.turns, brief))
    }

    /// Generate (or Regenerate): the shown part's summary made by the tool the settings pick.
    pub fn summarize(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.summary_input(cx).filter(|input| !input.is_empty()) else {
            return;
        };
        let choice = self.summary_choice(cx);
        if let Some(summary) = self.summary(cx) {
            summary.update(cx, |summary, cx| summary.start(choice, input, cx));
        }
        self.pane = Some(Pane::AiSummary);
        cx.notify();
    }

    /// Cancel (or, after a failure, Dismiss): the summary being made stopped.
    pub fn cancel_summary(&mut self, cx: &mut Context<Self>) {
        if let Some(summary) = self.summary(cx) {
            summary.update(cx, RecapSummary::cancel);
        }
    }

    /// Try again after a tool was missing: the tools looked for again, then the summary made.
    pub fn find_tools_and_summarize(&mut self, cx: &mut Context<Self>) {
        Tools::look_again(cx);
        self.summarize(cx);
    }

    /// The summary made for the shown part.
    fn made(&mut self, cx: &mut Context<Self>) -> Option<Made> {
        match self.summary(cx)?.read(cx).state() {
            State::Done(made) => Some(made.clone()),
            State::Idle | State::Running { .. } | State::Failed(_) => None,
        }
    }

    /// Copy: the summary as Markdown on the clipboard.
    pub fn copy_summary(&mut self, cx: &mut Context<Self>) {
        let Some(made) = self.made(cx) else { return };
        let when = self.when(cx);
        let text = recap_summary::plain(&made, &self.title, when.as_deref());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.note = Some(COPIED.into());
        cx.notify();
    }

    /// Send to chat: the message previewed, waiting for the user to send it.
    pub fn preview_summary(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || self.made(cx).is_none() {
            return;
        }
        self.sending = Some(Post::Preview);
        cx.notify();
    }

    /// The preview's Cancel.
    pub fn close_preview(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.sending, Some(Post::Sending)) {
            self.sending = None;
            cx.notify();
        }
    }

    /// The summary as the message the meeting's chat gets.
    fn summary_message(&mut self, cx: &mut Context<Self>) -> Option<teams_proto::chat::RichText> {
        let made = self.made(cx)?;
        let when = self.when(cx);
        let link = self.link();
        Some(recap_summary::chat_message(
            &made,
            &self.title,
            when.as_deref(),
            link.as_deref(),
        ))
    }

    /// The preview's Send: the summary posted to the meeting's chat, as one of the account's messages.
    pub fn send_summary(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || !matches!(self.sending, Some(Post::Preview | Post::Failed(_))) {
            return;
        }
        let Some(rich) = self.summary_message(cx) else { return };
        let text = match MessageText::new(rich, std::iter::empty()) {
            Ok(text) => text,
            Err(error) => {
                self.sending = Some(Post::Failed(format!("This summary can't be sent: {error}")));
                cx.notify();
                return;
            }
        };
        let reply = self.core.write(Action::Send {
            conversation: self.conversation.clone(),
            text,
        });
        self.sending = Some(Post::Sending);
        cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(_) => {
                        this.sending = None;
                        this.note = Some(SENT.into());
                    }
                    Err(error) => this.sending = Some(Post::Failed(format!("The summary wasn't sent: {error}"))),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// A moment of the summary clicked: the recording plays from there; a part without one shows its transcript there.
    pub fn seek_summary(&mut self, at: u64, cx: &mut Context<Self>) {
        let at = Duration::from_secs(at);
        if self.part().is_some_and(|part| part.recording.is_some()) {
            self.play_from(at, cx);
            return;
        }
        self.pane = Some(Pane::Transcript);
        if let Some(transcript) = self.transcript() {
            let spoken = transcript.spoken_at(at).or_else(|| {
                transcript
                    .turns
                    .iter()
                    .position(|turn| turn.start >= at)
                    .map(|turn| (turn, 0))
            });
            if let Some(spoken) = spoken {
                self.scroll
                    .scroll_to_item(crate::recap::row_of(&transcript.turns, spoken));
            }
            self.spoken = spoken;
        }
        cx.notify();
    }
}

/// The parts of the pane.
impl RecapView {
    /// The AI summary pane for the shown part.
    pub(super) fn summary_pane(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let location = self.part().and_then(|part| part.transcript.clone());
        let Some(location) = location else {
            let unread = self.part().is_some_and(RecapPart::is_unread_transcript);
            let words = if unread { NO_TEAMS_TRANSCRIPT } else { NO_TRANSCRIPT };
            return state(Glyph::SPARKLES, words, None, palette);
        };
        match self.transcripts.get(&location) {
            None | Some(Read::Reading) => return centred(spinner(palette), palette).into_any_element(),
            Some(Read::Failed) => return state(Glyph::CIRCLE_ALERT, TRANSCRIPT_UNREAD, None, palette),
            Some(Read::Shown { transcript, .. }) if transcript.turns.iter().all(|turn| turn.paragraphs.is_empty()) => {
                return state(Glyph::SPARKLES, EMPTY_TRANSCRIPT, None, palette);
            }
            Some(Read::Shown { .. }) => {}
        }
        let Some(summary) = self.summary(cx) else {
            return state(Glyph::SPARKLES, NO_TRANSCRIPT, None, palette);
        };
        match summary.read(cx).state().clone() {
            State::Idle => self.offer(palette, cx),
            State::Running {
                backend,
                label,
                stage,
                started,
            } => running((backend, &label), &stage.words(), started, palette, cx),
            State::Failed(failure) => failed(&failure, palette, cx),
            State::Done(made) => self.summary_shown(&made, palette, cx),
        }
    }

    /// What a summary is, what it sends where, and Generate; without a tool, how to get one.
    fn offer(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let choice = self.summary_choice(cx);
        let picked = Tools::of(cx).pick(choice.tool);
        let action = match &picked {
            Ok(_) => selected_by(
                "recap-summary-generate",
                Button::new("recap-summary-generate")
                    .icon(Glyph::SPARKLES.icon())
                    .label("Generate summary")
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| this.summarize(cx))),
            )
            .into_any_element(),
            Err(_) => selected_by(
                "recap-summary-find-tools",
                Button::new("recap-summary-find-tools")
                    .label("Try again")
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| this.find_tools_and_summarize(cx))),
            )
            .into_any_element(),
        };
        let privacy = match &picked {
            Ok((backend, _)) => format!(
                "The transcript goes to {} ({}) on this computer, which sends it to {}. Pick the tool in Settings \
                 > Calls.",
                backend.tool(),
                backend.label_of(Work::Summary, &choice.model),
                backend.provider()
            ),
            Err(failure) => format!(
                "{} Install Claude Code (claude) or the Codex CLI (codex), then try again.",
                failure.message()
            ),
        };
        div()
            .debug_selector(|| "recap-summary-offer".to_owned())
            .flex_1()
            .p_6()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(badge(Glyph::SPARKLES, palette))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Summarize this meeting"),
            )
            .child(
                div()
                    .max_w(px(340.0))
                    .text_center()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(
                        "Get its purpose, key takeaways, the topics discussed with their moments in the recording, \
                         and next steps with their owners.",
                    ),
            )
            .child(action)
            .child(privacy_note(privacy, palette))
            .into_any_element()
    }

    /// A summary made: what made it and when, Copy, Send to chat and Regenerate, then its sections.
    fn summary_shown(&mut self, made: &Made, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let made_at = DateTime::from_timestamp_millis(made.made_at).map(|at| {
            at.with_timezone(&Local)
                .format(&format!("%b %-d, {}", clock::cycle(cx).time_of_day()))
                .to_string()
        });
        let about = match made_at {
            Some(at) => format!("Made with {} · {at}", made.made_with()),
            None => format!("Made with {}", made.made_with()),
        };
        let mut actions = vec![selected_by(
            "recap-summary-copy",
            icon_button("recap-summary-copy", Glyph::COPY, "Copy summary")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.copy_summary(cx))),
        )];
        if !self.core.read_only() {
            actions.push(selected_by(
                "recap-summary-send",
                icon_button("recap-summary-send", Glyph::SEND, "Send summary to chat")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.preview_summary(cx))),
            ));
        }
        actions.push(selected_by(
            "recap-summary-regenerate",
            icon_button("recap-summary-regenerate", Glyph::REFRESH, "Regenerate summary")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.summarize(cx))),
        ));
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_1()
            .child(
                div()
                    .text_color(rgb(palette.brand))
                    .child(Glyph::SPARKLES.icon().size(px(14.0))),
            )
            .child(
                div()
                    .debug_selector(|| "recap-summary-made".to_owned())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(about),
            )
            .children(actions);
        let body = sections(&made.summary, made.sections, palette, cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .id("recap-summary")
                    .debug_selector(|| "recap-summary".to_owned())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.summary_scroll)
                    .px_4()
                    .pb_4()
                    .children(body),
            )
            .into_any_element()
    }

    /// The preview over the recap before the summary goes to the chat: the message as the chat will show it, Cancel
    /// and Send.
    pub(super) fn send_preview(&mut self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sending = self.sending.clone()?;
        let rich = self.summary_message(cx)?;
        let rendered = Arc::new(rich_text::layout(&rich, &Mentions::default()));
        let message = rich_text::render(&SharedString::from("recap-summary-preview-message"), &rendered, palette);
        let busy = matches!(sending, Post::Sending);
        let problem = match &sending {
            Post::Failed(why) => Some(
                div()
                    .debug_selector(|| "recap-summary-send-failed".to_owned())
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(rgb(crate::theme::DANGER))
                    .child(Glyph::CIRCLE_ALERT.icon().size(px(14.0)))
                    .child(why.clone()),
            ),
            Post::Preview | Post::Sending => None,
        };
        let dialog = preview_dialog(message, problem, busy, palette, cx);
        Some(
            div()
                .id("recap-summary-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .bg(rgb(palette.background).opacity(0.7))
                .occlude()
                .child(dialog)
                .into_any_element(),
        )
    }
}

/// The preview's box: what sending does, the message as the chat will show it, why it wasn't sent, Cancel and Send.
fn preview_dialog(
    message: AnyElement,
    problem: Option<gpui::Div>,
    busy: bool,
    palette: Palette,
    cx: &mut Context<RecapView>,
) -> gpui::Div {
    let cancel = Button::new("recap-summary-send-cancel")
        .label("Cancel")
        .outline()
        .disabled(busy)
        .on_click(cx.listener(|this, _, _, cx| this.close_preview(cx)));
    let send = Button::new("recap-summary-send-confirm")
        .icon(Glyph::SEND.icon())
        .label("Send")
        .primary()
        .spinning(busy)
        .disabled(busy)
        .on_click(cx.listener(|this, _, _, cx| this.send_summary(cx)));
    div()
        .debug_selector(|| "recap-summary-preview".to_owned())
        .occlude()
        .w(px(600.0))
        .max_w_full()
        .max_h(gpui::relative(0.85))
        .flex()
        .flex_col()
        .gap_3()
        .p_5()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .shadow_lg()
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Send the summary to the chat?"),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child("Everyone in this meeting's chat will see this message. Check it before you send it."),
        )
        .child(
            div()
                .id("recap-summary-preview-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.background))
                .text_sm()
                .child(message),
        )
        .children(problem)
        .child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(selected_by("recap-summary-send-cancel", cancel))
                .child(selected_by("recap-summary-send-confirm", send)),
        )
}

/// A state of the pane: its icon in a tinted circle, its words, and what to do.
fn state(glyph: Glyph, words: &'static str, action: Option<AnyElement>, palette: Palette) -> AnyElement {
    div()
        .flex_1()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(badge(glyph, palette))
        .child(
            div()
                .debug_selector(move || format!("recap-summary-state {words}"))
                .max_w(px(340.0))
                .text_center()
                .text_color(rgb(palette.foreground))
                .child(words),
        )
        .children(action)
        .into_any_element()
}

/// An icon large in a tinted circle.
fn badge(glyph: Glyph, palette: Palette) -> gpui::Div {
    div()
        .size(px(56.0))
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(palette.selected))
        .text_color(rgb(palette.brand))
        .child(glyph.icon().size(px(26.0)))
}

/// What goes where, under the offer and the progress.
fn privacy_note(words: String, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "recap-summary-privacy".to_owned())
        .max_w(px(340.0))
        .flex()
        .gap_2()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(div().flex_none().pt_0p5().child(Glyph::SHIELD.icon().size(px(12.0))))
        .child(div().flex_1().min_w_0().child(words))
        .into_any_element()
}

/// The summary being made by `backend` (as the recap says it: `label`): how far it got, for how long, and Cancel.
fn running(
    (backend, label): (Backend, &str),
    stage: &str,
    started: std::time::Instant,
    palette: Palette,
    cx: &mut Context<RecapView>,
) -> AnyElement {
    let elapsed = stamp(started.elapsed().as_secs());
    let cancel = Button::new("recap-summary-cancel")
        .label("Cancel")
        .outline()
        .on_click(cx.listener(|this, _, _, cx| this.cancel_summary(cx)));
    div()
        .debug_selector(|| "recap-summary-running".to_owned())
        .flex_1()
        .p_6()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(spinner(palette))
        .child(div().font_weight(FontWeight::SEMIBOLD).child(stage.to_owned()))
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(format!("{label} · {elapsed}")),
        )
        .child(selected_by("recap-summary-cancel", cancel))
        .child(privacy_note(
            format!(
                "The transcript is with {} on this computer, which sends it to {}.",
                backend.tool(),
                backend.provider()
            ),
            palette,
        ))
        .into_any_element()
}

/// The summary that couldn't be made: why, Try again and Dismiss.
fn failed(failure: &Failure, palette: Palette, cx: &mut Context<RecapView>) -> AnyElement {
    // a tool missing is looked for again; any other failure tries the same tool again
    let missing = matches!(failure, Failure::NotInstalled(_));
    let again = Button::new("recap-summary-retry")
        .label("Try again")
        .primary()
        .on_click(cx.listener(move |this, _, _, cx| {
            if missing {
                this.find_tools_and_summarize(cx);
            } else {
                this.summarize(cx);
            }
        }));
    let dismiss = Button::new("recap-summary-dismiss")
        .label("Dismiss")
        .outline()
        .on_click(cx.listener(|this, _, _, cx| this.cancel_summary(cx)));
    let actions = div()
        .flex()
        .gap_2()
        .child(selected_by("recap-summary-retry", again))
        .child(selected_by("recap-summary-dismiss", dismiss));
    let why = div()
        .debug_selector(|| "recap-summary-failure".to_owned())
        .max_w(px(340.0))
        .text_center()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .child(failure.message());
    let below = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .child(why)
        .child(actions)
        .into_any_element();
    state(Glyph::CIRCLE_ALERT, SUMMARY_FAILED, Some(below), palette)
}

/// The summary's sections, as Fathom lays them out.
fn sections(
    summary: &recap_summary::Summary,
    count: usize,
    palette: Palette,
    cx: &mut Context<RecapView>,
) -> Vec<AnyElement> {
    let mut rows = vec![event_row(
        "recap-summary-notice",
        Glyph::INFO,
        recap_summary::AI_NOTICE.to_owned(),
        palette,
    )];
    let mut moments = 0_usize;
    if !summary.purpose.is_empty() {
        rows.push(heading("Meeting purpose", "purpose"));
        rows.push(div().text_sm().child(summary.purpose.clone()).into_any_element());
    }
    if !summary.takeaways.is_empty() {
        rows.push(heading("Key takeaways", "takeaways"));
        for takeaway in &summary.takeaways {
            rows.push(dot(takeaway.clone(), palette));
        }
    }
    if !summary.topics.is_empty() {
        rows.push(heading("Topics", "topics"));
        for Topic { title, points } in &summary.topics {
            rows.push(
                div()
                    .pt_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.clone())
                    .into_any_element(),
            );
            for Point { text, at } in points {
                rows.push(bullet(*at, &mut moments, div().child(text.clone()), palette, cx));
            }
        }
    }
    if !summary.next_steps.is_empty() {
        rows.push(heading("Next steps", "next-steps"));
        for Step { owner, text, at } in &summary.next_steps {
            let words = div()
                .flex()
                .flex_wrap()
                .gap_x_1()
                .children(
                    owner
                        .as_ref()
                        .map(|owner| div().font_weight(FontWeight::SEMIBOLD).child(format!("{owner}:"))),
                )
                .child(text.clone());
            rows.push(bullet(*at, &mut moments, words, palette, cx));
        }
    }
    if !summary.questions.is_empty() {
        rows.push(heading("Questions raised", "questions"));
        for Point { text, at } in &summary.questions {
            rows.push(bullet(*at, &mut moments, div().child(text.clone()), palette, cx));
        }
    }
    if count > 1 {
        rows.push(
            div()
                .debug_selector(|| "recap-summary-sections".to_owned())
                .pt_4()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(format!(
                    "The transcript was too long to read at once: it was read in {count} parts, then summarized."
                ))
                .into_any_element(),
        );
    }
    rows
}

/// A section's heading.
fn heading(words: &'static str, key: &'static str) -> AnyElement {
    div()
        .debug_selector(move || format!("recap-summary-{key}"))
        .pt_4()
        .pb_1()
        .font_weight(FontWeight::SEMIBOLD)
        .child(words)
        .into_any_element()
}

/// A takeaway: a bullet, then its words.
fn dot(words: String, palette: Palette) -> AnyElement {
    div()
        .pt_1()
        .flex()
        .items_start()
        .gap_2()
        .text_sm()
        .child(
            div()
                .flex_none()
                .w(px(12.0))
                .text_color(rgb(palette.muted_foreground))
                .child("•"),
        )
        .child(div().flex_1().min_w_0().child(words))
        .into_any_element()
}

/// A point: its moment (a click plays it) or a bullet under the moments, then its words.
fn bullet(
    at: Option<u64>,
    moments: &mut usize,
    words: gpui::Div,
    palette: Palette,
    cx: &mut Context<RecapView>,
) -> AnyElement {
    let mark = match at {
        Some(at) => {
            let index = *moments;
            *moments += 1;
            div()
                .id(SharedString::from(format!("recap-summary-at-{index}")))
                .debug_selector(move || format!("recap-summary-at-{index}"))
                .flex_none()
                .min_w(px(40.0))
                .px_1()
                .rounded_sm()
                .text_xs()
                .text_color(rgb(palette.brand))
                .cursor_pointer()
                .hover(move |chip| chip.bg(rgb(palette.hover)))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(format!("Play from {}", stamp(at))).build(window, cx)
                })
                .child(stamp(at))
                .on_click(cx.listener(move |this, _, _, cx| this.seek_summary(at, cx)))
                .into_any_element()
        }
        None => div()
            .flex_none()
            .w(px(40.0))
            .flex()
            .justify_end()
            .pr_2()
            .text_color(rgb(palette.muted_foreground))
            .child("•")
            .into_any_element(),
    };
    div()
        .pt_1()
        .flex()
        .items_start()
        .gap_2()
        .text_sm()
        .child(mark)
        .child(words.flex_1().min_w_0())
        .into_any_element()
}
