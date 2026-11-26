//! A chat's system messages as Teams draws them ([`super::ChatView`]'s rows, no author row): a line with its icon in
//! the avatars' column (a recording started in the brand colour), its time first for a call's, a meeting's or a
//! recording's, a meeting's length in bold, Teams' unverified mark after an anonymous member's name, and a recording's
//! expiry once its item says it; under a meeting chat's meeting end its recap card ([`crate::meeting_recap`]); in a
//! 1:1 or group chat, a recording's recap card under its "saved" line and a transcript's on its own, each opening the
//! chat's Recap tab as Teams' do; a kind this build doesn't show as its placeholder.

use chrono::{DateTime, Local};
use gpui::{
    AnyElement, ClipboardItem, Context, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, StyledText, div,
    img, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::DropdownMenu as _;
use gpui_component::tooltip::Tooltip;
use std::collections::HashMap;

use teams_proto::chat::{ChatEvent, RecordingItem, chat_event, meeting_times, web_chat_link};
use teams_store::Message;

use super::{
    AVATAR, AVATAR_GAP, Body, ChatView, ChatViewEvent, MeetingCall, MessageItem, RecapOpen, UNSUPPORTED, VIEW_RECAP,
    web_line,
};
use crate::clock::HourCycle;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::inline::{PictureSource, PictureState, thumbnail};
use crate::meeting_recap::{CONTENT, RecapCard, TRANSCRIPT};
use crate::rich_text::tint;
use crate::system_message::{EventIcon, EventLine, Span, expiry_clause, is_meeting_chat};
use crate::theme::Palette;

/// The widest a recap card is drawn, px.
const RECAP_WIDTH: f32 = 680.0;
/// The recap card's thumbnail, px.
const THUMB_WIDTH: f32 = 142.0;
const THUMB_HEIGHT: f32 = 80.0;

/// The recap thumbnail's name for a screen reader: a click or Enter plays the recording.
pub const PLAY_RECORDING: &str = "Play recording";
/// What Teams' unverified mark says.
pub const UNVERIFIED: &str = "Unverified";

/// How a recording's expiry stands, as the view asked its item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Expiry {
    Reading,
    /// Read: when it expires (ms), `None` when it never does.
    Known(Option<i64>),
    /// It couldn't be read: nothing is said.
    Unread,
}

/// The time of day of `ms` on `cycle`'s clock: a system message's day is its separator's.
pub(super) fn time_of_day(ms: i64, cycle: HourCycle) -> String {
    DateTime::from_timestamp_millis(ms).map_or_else(String::new, |at| {
        at.with_timezone(&Local).format(cycle.time_of_day()).to_string()
    })
}

/// `icon` as Teams draws it before its line: muted, but a recording's start in the brand colour.
fn line_icon(icon: EventIcon, palette: Palette) -> AnyElement {
    let glyph = match icon {
        EventIcon::Joined => Glyph::USER_PLUS,
        EventIcon::Invited => Glyph::CALENDAR_ADD,
        EventIcon::Left => Glyph::USER_LEFT,
        EventIcon::Removed => Glyph::USER_MINUS,
        EventIcon::Named => Glyph::PENCIL,
        EventIcon::Info => Glyph::CIRCLE_ALERT,
        EventIcon::Meeting => Glyph::VIDEO,
        EventIcon::Call => Glyph::PHONE,
        EventIcon::Missed => Glyph::PHONE_MISSED,
        EventIcon::RecordingStarted => Glyph::RECORD,
        EventIcon::RecordingStopped => Glyph::RECORD_STOP,
        EventIcon::RecordingSaved => Glyph::CLOUD_UPLOAD,
    };
    let colour = if icon == EventIcon::RecordingStarted {
        palette.brand
    } else {
        palette.muted_foreground
    };
    let name = icon.name();
    div()
        .debug_selector(move || format!("event-icon {name}"))
        .w(px(AVATAR))
        .mr(px(AVATAR_GAP))
        .flex_none()
        .flex()
        .justify_center()
        .text_color(rgb(colour))
        .child(glyph.icon().size(px(18.0)))
        .into_any_element()
}

/// A run of a line's words: plain and bold spans as one text, the bold ones in the foreground colour.
fn words(spans: &[(String, bool)], palette: Palette) -> Option<AnyElement> {
    let mut text = String::new();
    let mut bold = Vec::new();
    for (words, strong) in spans {
        let start = text.len();
        text.push_str(words);
        if *strong {
            bold.push((
                start..text.len(),
                HighlightStyle {
                    font_weight: Some(FontWeight::BOLD),
                    color: Some(rgb(palette.foreground).into()),
                    ..HighlightStyle::default()
                },
            ));
        }
    }
    // a run beside a mark starts or ends with the space before or after it: the row's gap stands for it
    let lead = text.len() - text.trim_start().len();
    let text = text.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    let bold: Vec<_> = bold
        .into_iter()
        .filter_map(|(range, style)| {
            let (start, end) = (
                range.start.checked_sub(lead)?,
                range.end.checked_sub(lead)?.min(text.len()),
            );
            (start < end).then_some((start..end, style))
        })
        .collect();
    Some(
        div()
            .min_w_0()
            .child(StyledText::new(text).with_highlights(bold))
            .into_any_element(),
    )
}

/// Teams' unverified mark after a name, with its tooltip.
fn unverified_mark(id: String, palette: Palette) -> AnyElement {
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || id.clone())
        .flex_none()
        .text_color(rgb(palette.muted_foreground))
        .child(Glyph::UNVERIFIED.icon().size(px(14.0)))
        .tooltip(|window, cx| Tooltip::new(UNVERIFIED).build(window, cx))
        .into_any_element()
}

impl ChatView {
    /// A system message's row (see the module docs).
    pub(super) fn system_row(&self, item: &MessageItem, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let cycle = crate::clock::cycle(cx);
        let id = item.id.clone();
        let selector = move || format!("event-{id}");
        match &item.body {
            Body::Event(line) => self
                .event_line(item, line, None, palette)
                .debug_selector(selector)
                .into_any_element(),
            Body::TimedEvent(line) => {
                let line = self
                    .event_line(item, line, Some(time_of_day(item.arrival_time, cycle)), palette)
                    .debug_selector(selector);
                match self.saved_chip(item, palette, cx) {
                    Some(chip) => div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .child(line)
                        .child(chip)
                        .into_any_element(),
                    None => line.into_any_element(),
                }
            }
            Body::MeetingEnded(end) => {
                let line = self
                    .event_line(item, &end.line, Some(time_of_day(item.arrival_time, cycle)), palette)
                    .debug_selector(selector);
                let recap = end
                    .recap
                    .as_ref()
                    .map(|recap| self.recap_card(item, recap, palette, cx));
                let saved = self.saved_chip(item, palette, cx);
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(line)
                    .children(recap)
                    .children(saved)
                    .into_any_element()
            }
            Body::Card(card) => div()
                .w_full()
                .debug_selector(selector)
                .child(self.recap_card(item, card, palette, cx))
                .into_any_element(),
            Body::MeetingCall(call) => div()
                .w_full()
                .py_1p5()
                .debug_selector(selector)
                .child(super::channel::meeting_call_card(
                    &item.id,
                    call,
                    &time_of_day(item.arrival_time, cycle),
                    palette,
                    self.meeting_join(&item.id, call, cx),
                ))
                .into_any_element(),
            Body::UnsupportedEvent(_) => div()
                .w_full()
                .py_2()
                .flex()
                .justify_center()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .debug_selector(selector)
                .child(web_line(
                    &format!("unsupported-{}", item.id),
                    UNSUPPORTED,
                    &item.id,
                    web_chat_link(&self.conversation),
                    palette,
                ))
                .into_any_element(),
            Body::Text(_) | Body::Deleted | Body::Cards(_) | Body::Meeting(_) | Body::Unsupported(_) => {
                div().into_any_element()
            }
        }
    }

    /// A running channel meeting's Join on its post (`post`), telling [`ChatViewEvent::JoinMeeting`]; none in the
    /// read-only mode, as no write is offered there.
    fn meeting_join(&self, post: &str, call: &MeetingCall, cx: &mut Context<Self>) -> Option<crate::forwarded::OnOpen> {
        if self.core.read_only() {
            return None;
        }
        let (post, meeting, title) = (post.to_owned(), call.meeting.clone(), call.title.to_string());
        Some(Box::new(cx.listener(move |_, _: &gpui::ClickEvent, _, cx| {
            cx.stop_propagation();
            cx.emit(ChatViewEvent::JoinMeeting {
                post: post.clone(),
                meeting: meeting.clone(),
                title: title.clone(),
            });
        })))
    }

    /// A line's words as drawn: runs of words (each `(words, bold)`) between Teams' unverified marks (`None`), a
    /// recording's expiry once its item says it.
    fn line_runs(&self, line: &EventLine) -> Vec<Option<Vec<(String, bool)>>> {
        let mut drawn = Vec::new();
        let mut run: Vec<(String, bool)> = Vec::new();
        for span in &line.spans {
            match span {
                Span::Text(text) => run.push((text.clone(), false)),
                Span::Strong(text) => run.push((text.clone(), true)),
                Span::Expiry { file, .. } => {
                    if let Some(Expiry::Known(Some(expires))) = self.systems.expiries.get(file) {
                        run.push((expiry_clause(*expires, chrono::Utc::now().timestamp_millis()), false));
                    }
                }
                Span::Unverified => {
                    drawn.push(Some(std::mem::take(&mut run)));
                    drawn.push(None);
                }
            }
        }
        drawn.push(Some(run));
        drawn
    }

    /// A line's words as the row draws them (a recording's expiry once known), its marks left out.
    #[must_use]
    pub fn drawn_text(&self, line: &EventLine) -> String {
        self.line_runs(line)
            .into_iter()
            .flatten()
            .flatten()
            .map(|(words, _)| words)
            .collect()
    }

    /// A line: its icon, its time when it has one, its words.
    fn event_line(&self, item: &MessageItem, line: &EventLine, time: Option<String>, palette: Palette) -> gpui::Div {
        let mut parts: Vec<AnyElement> = Vec::new();
        let mut marks = 0_usize;
        for drawn in self.line_runs(line) {
            if let Some(run) = drawn {
                parts.extend(words(&run, palette));
            } else {
                parts.push(unverified_mark(format!("unverified-{}-{marks}", item.id), palette));
                marks += 1;
            }
        }
        let time = time.map(|time| div().flex_none().mr_4().child(time));
        div()
            .w_full()
            .py_1p5()
            .flex()
            .items_center()
            .text_sm()
            .text_color(rgb(palette.muted_foreground))
            .child(line_icon(line.icon, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .children(time)
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .children(parts),
                    ),
            )
    }

    /// A meeting's recap card under its end (see [`crate::meeting_recap`]).
    fn recap_card(&self, item: &MessageItem, card: &RecapCard, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id.clone();
        let header = self.recap_header(item, card, palette, cx);
        let content = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_foreground))
                    .child(CONTENT),
            )
            .children(card.transcript.then(|| Self::transcript_chip(item, card, cx)));
        let picture = card.has_picture().then(|| self.recap_picture(item, card, palette, cx));
        div()
            .w_full()
            .pl(px(AVATAR + AVATAR_GAP))
            .pt_1()
            .pb_3()
            .child(
                div()
                    .debug_selector(move || format!("recap-card-{id}"))
                    .w_full()
                    .max_w(px(RECAP_WIDTH))
                    .px_5()
                    .py_4()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .shadow_sm()
                    .child(header)
                    .child(div().my_3().h(px(1.0)).w_full().bg(rgb(palette.border)))
                    .child(div().flex().items_start().gap_4().child(content).children(picture)),
            )
            .into_any_element()
    }

    /// The card's title and line, Share and View recap.
    fn recap_header(
        &self,
        item: &MessageItem,
        card: &RecapCard,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        // a meeting's card is titled by its chat's name, a call's by its recording's ("Call with …"), as Teams' are
        let chat = self.systems.title.clone();
        let own = card.title.clone().map(SharedString::from);
        let title = if is_meeting_chat(&self.conversation) {
            chat.or(own)
        } else {
            own.or(chat)
        }
        .unwrap_or_else(|| crate::system_message::MEETING.into());
        let line = card.span_line(&Local, crate::clock::cycle(cx));
        let share = card.share_link(&self.conversation).map(|link| {
            let button = icon_button(
                SharedString::from(format!("recap-card-share-{}", item.id)),
                Glyph::SHARE,
                "Share",
            )
            .dropdown_menu(move |menu, _, _| {
                let copied = link.clone();
                menu.item(
                    crate::recap_view::menu_row("recap-card-copy", Glyph::LINK, "Copy link")
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
                )
            });
            selected_by(format!("recap-card-share-{}", item.id), button)
        });
        let view = Button::new(SharedString::from(format!("view-recap-{}", item.id)))
            .label(VIEW_RECAP)
            .primary()
            .with_size(gpui_component::Size::Large)
            .on_click(recap_opener(card, RecapOpen::Recap, cx));
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(palette.foreground))
                            .truncate()
                            .child(title),
                    )
                    .children(line.map(|line| {
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(line)
                    })),
            )
            .children(share)
            .child(selected_by(format!("view-recap-{}", item.id), view))
    }

    /// The Transcript chip: the recap, its transcript shown.
    fn transcript_chip(item: &MessageItem, card: &RecapCard, cx: &mut Context<Self>) -> AnyElement {
        let chip = Button::new(SharedString::from(format!("recap-card-transcript-{}", item.id)))
            .icon(Glyph::TRANSCRIPT.icon())
            .label(TRANSCRIPT)
            .outline()
            .on_click(recap_opener(card, RecapOpen::Transcript, cx));
        div()
            .flex()
            .child(selected_by(format!("recap-card-transcript-{}", item.id), chip))
            .into_any_element()
    }

    /// The first recording's thumbnail with its badge (a click plays it in the recap), and its expiry under it.
    fn recap_picture(
        &self,
        item: &MessageItem,
        card: &RecapCard,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let first = card.first_recording().filter(|_| !card.saving);
        let state = first.map_or(PictureState::Missing, |recording| {
            thumbnail(
                &recording.link,
                &PictureSource::new(std::sync::Arc::clone(&self.core)),
                cx,
            )
        });
        let shown = match state {
            PictureState::Shown(image, _) => img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            PictureState::Reading => div().size_full().into_any_element(),
            PictureState::Missing => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(palette.muted_foreground))
                .child(Glyph::FILE_VIDEO.icon().size(px(28.0)))
                .into_any_element(),
        };
        let badge = card.badge().map(|badge| {
            div()
                .absolute()
                .right(px(6.0))
                .bottom(px(6.0))
                .px_1p5()
                .rounded_sm()
                .bg(gpui::rgba(0x0000_00b3))
                .text_sm()
                .text_color(rgb(0x00ff_ffff))
                .child(badge)
        });
        let id = item.id.clone();
        let mut picture = div()
            .id(SharedString::from(format!("recap-card-thumbnail-{id}")))
            .debug_selector(move || format!("recap-card-thumbnail-{id}"))
            .relative()
            .w(px(THUMB_WIDTH))
            .h(px(THUMB_HEIGHT))
            .rounded_md()
            .overflow_hidden()
            .bg(tint(palette.foreground, 0.08))
            .child(shown)
            .children(badge);
        let picture = if first.is_some() {
            picture = picture
                .cursor_pointer()
                .role(gpui::accesskit::Role::Button)
                .aria_label(PLAY_RECORDING)
                .on_click(recap_opener(card, RecapOpen::Play, cx));
            // a stop on Tab, as Teams' is: Enter or Space plays the recording in the recap, as a click does
            crate::keyboard::stop(
                SharedString::from(format!("recap-card-play-{}", item.id)),
                picture,
                palette,
            )
            .outline(format!("recap-card-ring-{}", item.id))
            .into_any_element()
        } else {
            picture.into_any_element()
        };
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .child(picture)
            .children(self.recap_expiry(item, card, palette))
            .into_any_element()
    }

    /// The line under the thumbnail: when the first recording expires, once its item says it.
    fn recap_expiry(&self, item: &MessageItem, card: &RecapCard, palette: Palette) -> Option<gpui::Div> {
        let file = card.first_recording()?.file.as_ref()?;
        let Some(Expiry::Known(Some(expires))) = self.systems.expiries.get(file) else {
            return None;
        };
        let line = card.expiry_line(*expires, chrono::Utc::now().timestamp_millis());
        let id = item.id.clone();
        Some(
            div()
                .debug_selector(move || format!("recap-card-expiry-{id}"))
                .w(px(THUMB_WIDTH))
                .flex()
                .justify_center()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(Glyph::CLOCK.icon().size(px(12.0)))
                .child(div().truncate().child(line)),
        )
    }

    /// Asks the engine when each recording the shown rows name expires, once each: a recap card's first recording,
    /// a saved recording's line.
    pub(super) fn ask_expiries(&mut self, cx: &mut Context<Self>) {
        let mut wanted: Vec<(String, RecordingItem)> = Vec::new();
        for item in self.message_items() {
            match &item.body {
                Body::TimedEvent(line) => wanted.extend(line.spans.iter().filter_map(|span| match span {
                    Span::Expiry { link, file } => Some((link.clone(), file.clone())),
                    _ => None,
                })),
                Body::MeetingEnded(end) => wanted.extend(
                    end.recap
                        .as_ref()
                        .and_then(RecapCard::first_recording)
                        .and_then(|recording| Some((recording.link.clone(), recording.file.clone()?))),
                ),
                _ => {}
            }
        }
        for (link, file) in wanted {
            if self.systems.expiries.contains_key(&file) {
                continue;
            }
            self.systems.expiries.insert(file.clone(), Expiry::Reading);
            let reply = self
                .core
                .recording_expiration(link, file.drive.clone(), file.item.clone());
            cx.spawn(async move |this, cx| {
                let known = match reply.await {
                    Ok(expires) => Expiry::Known(expires),
                    Err(_) => Expiry::Unread,
                };
                let _ = this.update(cx, |this, cx| {
                    this.systems.expiries.insert(file, known);
                    cx.notify();
                });
            })
            .detach();
        }
    }
}

/// What a recap card's control does: tells [`ChatViewEvent::OpenRecap`] with `recording` (its sharing link) and what
/// to show.
fn recap_opener(
    card: &RecapCard,
    open: RecapOpen,
    cx: &mut Context<ChatView>,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    // what the card opens: its first recording, else its transcript alone's item
    let recording = card.opens().map(str::to_owned);
    let transcribed = card.transcribed_part();
    cx.listener(move |_, _: &gpui::ClickEvent, _, cx| {
        cx.stop_propagation();
        cx.emit(ChatViewEvent::OpenRecap {
            recording: recording.clone(),
            transcribed: transcribed.clone(),
            open,
        });
    })
}

/// A meeting chat's rows (oldest first) without a meeting's start that is followed at once by its own end: Teams'
/// meeting chat draws only the end then ("Meeting ended: 2m 6s" and its recap card), and both when anything is shown
/// between them.
pub(super) fn drop_bare_starts(rows: &mut Vec<MessageItem>, messages: &[Message]) {
    let calls: HashMap<&str, (bool, Option<String>)> = messages
        .iter()
        .filter(|message| message.message_type == "Event/Call")
        .filter_map(|message| {
            let ended = match chat_event(&message.message_type, &message.content)? {
                ChatEvent::CallStarted(start) if !start.huddle => false,
                ChatEvent::CallEnded(end) if !end.huddle => true,
                _ => return None,
            };
            let call = meeting_times(&message.content).and_then(|times| times.call_id);
            Some((message.id.as_str(), (ended, call)))
        })
        .collect();
    let mut index = 0;
    while index + 1 < rows.len() {
        let bare = match (
            calls.get(rows[index].id.as_str()),
            calls.get(rows[index + 1].id.as_str()),
        ) {
            (Some((false, started)), Some((true, ended))) => started == ended,
            _ => false,
        };
        if bare {
            rows.remove(index);
        } else {
            index += 1;
        }
    }
}
