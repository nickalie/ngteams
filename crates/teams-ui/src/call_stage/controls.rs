//! The organizer's controls of a group call or a meeting, and what binds everyone else, as Teams' call window shows
//! them (seen 2026-11-06, `docs/protocol/calling.md`, "More of a running call"):
//!
//! - **Controls** (the toolbar's shield, the organizer's): Lock meeting; Enable lobby; "Allow all participants to:"
//!   Present, Chat, Unmute themselves and Turn on their cameras (the last two greyed while everyone may present: the
//!   restrictions bind attendees only); View more options where the meeting's options page is known. Each check is the
//!   meeting's option as the service read it when the call became the organizer's, then as the conversation told it.
//!   A change shows at once ([`Ask::MeetingOption`]) and the options are read again shortly after. The value the user
//!   set stays shown until a read shows the same, the conversation tells that option anew, or the change wasn't
//!   taken: the middle tier's read can lag its write by seconds (live 2026-11-06, Present turned back on read back
//!   off 3 s later), so a read that disagrees doesn't flip the check back.
//! - **Mute all** (People, beside "Currently in this call", the organizer's while anyone else isn't muted): "Mute
//!   everyone?" asks first ([`Ask::MuteAll`]).
//! - **"You've been muted"**: the organizer muted the account ([`teams_core::MeetingState::muted_by_organizer`]): its
//!   microphone is muted, and a card over the stage says so with Unmute.
//! - **An attendee's controls**: the microphone, the camera and Share greyed while the organizer's options keep the
//!   account from them ([`teams_core::MeetingState::unmute_blocked`] and the others).

use super::*;
use std::time::Duration;
use teams_proto::{MeetingOption, MeetingOptions};

/// How long after a change of the meeting's options they are read again (a change not taken shows as it is).
const READ_AGAIN_AFTER: Duration = Duration::from_secs(3);

/// The organizer's controls' state in the call window.
#[derive(Default)]
pub(super) struct ControlsUi {
    /// The Controls menu is open.
    open: bool,
    /// "Mute everyone?" is asked.
    confirm_mute_all: bool,
    /// The organizer's mute "You've been muted" shows for, and the newest one seen.
    toast: Option<u64>,
    toast_seen: Option<u64>,
    /// The meeting's options as the service last had them (read, then as the conversation told them), for the call
    /// they were read for.
    options: Option<(CallId, MeetingOptions)>,
    /// What the user set in Controls that the service hasn't shown yet, for its call: shown over `options` (see the
    /// module docs).
    set: Option<(CallId, MeetingOptions)>,
    /// The options as the conversation last told them.
    told: MeetingOptions,
    reading: Option<Task<()>>,
    /// The row of "People added to the chat" the pointer is on: its Request to join shows.
    hovered: Option<usize>,
}

/// A toolbar control greyed out and not taking the pointer while `blocked` (an attendee kept from it), as Teams shows
/// it.
pub(super) fn blocked(control: AnyElement, blocked: bool, selector: &'static str) -> AnyElement {
    if !blocked {
        return control;
    }
    div()
        .debug_selector(move || selector.to_owned())
        .id(selector)
        .opacity(0.4)
        .occlude()
        .child(control)
        .into_any_element()
}

impl CallStage {
    /// Follows the call: the organizer's mute's card shown when a new one came (gone once unmuted), and the meeting's
    /// options read once the account organizes a group call or a meeting that is up.
    pub(super) fn controls_follow(&mut self, active: &ActiveCall, cx: &mut Context<Self>) {
        let controls = &mut self.controls;
        if let Some(version) = active.meeting.muted_by_organizer
            && controls.toast_seen.is_none_or(|seen| seen < version)
        {
            controls.toast_seen = Some(version);
            controls.toast = Some(version);
        }
        if !active.muted {
            controls.toast = None;
        }
        if active.meeting.options != controls.told {
            let before = std::mem::replace(&mut controls.told, active.meeting.options);
            if let Some((_, options)) = controls.options.as_mut() {
                *options = options.updated(controls.told);
            }
            // told after the change, so newer than it: what it tells stands
            if let Some((_, set)) = controls.set.as_mut() {
                told_anew(set, before, controls.told);
            }
        }
        let read = controls.options.as_ref().is_some_and(|(call, _)| *call == active.id);
        if active.organizer && self.takes_meeting_tools(active) && !read && self.controls.reading.is_none() {
            self.read_options(active.id, Duration::ZERO, cx);
        }
    }

    /// Reads `call`'s meeting options after `wait`.
    fn read_options(&mut self, call: CallId, wait: Duration, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.controls.reading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            if !wait.is_zero() {
                cx.background_executor().timer(wait).await;
            }
            let read = core.call_meeting_options(call).await.ok().flatten();
            let _ = this.update(cx, |stage, cx| {
                stage.controls.reading = None;
                let told = stage.controls.told;
                let kept = stage.controls.options.take().filter(|(of, _)| *of == call);
                let options = match (read, kept) {
                    (Some(read), _) => read.updated(told),
                    (None, Some((_, kept))) => kept,
                    (None, None) => MeetingOptions::default().updated(told),
                };
                stage.controls.options = Some((call, options));
                // a read that shows what the user set confirms it; one that doesn't may lag the change, and is kept under it
                if read.is_some()
                    && let Some((_, set)) = stage.controls.set.as_mut().filter(|(of, _)| *of == call)
                {
                    drop_same(set, options);
                }
                cx.notify();
            });
        }));
    }

    /// Opens Controls' menu (or closes it).
    pub fn open_controls(&mut self, open: bool, cx: &mut Context<Self>) {
        self.controls.open = open;
        cx.notify();
    }

    /// Asks "Mute everyone?" (or takes the question away).
    pub fn confirm_mute_all(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.controls.confirm_mute_all = shown;
        cx.notify();
    }

    /// The pointer is on the row `index` of "People added to the chat" (or on none).
    pub fn hover_added(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if self.controls.hovered != index {
            self.controls.hovered = index;
            cx.notify();
        }
    }

    /// The row of "People added to the chat" the pointer is on.
    pub(super) fn hovered_added(&self) -> Option<usize> {
        self.controls.hovered
    }

    /// The meeting's options as Controls show them: what the user set over what the service has, Teams' defaults where
    /// nothing was read or told.
    fn shown_options(&self, call: CallId) -> MeetingOptions {
        let had = self
            .controls
            .options
            .filter(|(of, _)| *of == call)
            .map_or(self.controls.told, |(_, options)| options);
        self.controls
            .set
            .filter(|(of, _)| *of == call)
            .map_or(had, |(_, set)| had.updated(set))
    }

    /// One of Controls' options changed: shown at once, sent, and read again shortly after.
    fn change_option(&mut self, option: MeetingOption, cx: &mut Context<Self>) {
        let Some(call) = self.live(cx).map(|active| active.id) else {
            return;
        };
        let set = match self.controls.set.take() {
            Some((of, set)) if of == call => set,
            _ => MeetingOptions::default(),
        };
        self.controls.set = Some((call, set.updated(changed(option))));
        let stage = cx.weak_entity();
        calls::ask_then(
            &self.calls,
            &self.core,
            &self.trace,
            Ask::MeetingOption(option),
            cx,
            move |answered, cx| {
                if answered != calls::Answered::Taken {
                    let _ = stage.update(cx, |stage, cx| stage.option_not_taken(call, option, cx));
                }
            },
        );
        self.read_options(call, READ_AGAIN_AFTER, cx);
    }

    /// The change `option` of `call`'s options wasn't taken: the check shows what the service has again, unless the
    /// user has since set it otherwise.
    fn option_not_taken(&mut self, call: CallId, option: MeetingOption, cx: &mut Context<Self>) {
        if let Some((_, set)) = self.controls.set.as_mut().filter(|(of, _)| *of == call) {
            drop_same(set, changed(option));
            cx.notify();
        }
    }

    /// The toolbar's Controls (the organizer's, in a group call or a meeting that is up), its menu under it while open.
    pub(super) fn controls_tool(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !(active.organizer && self.takes_meeting_tools(active)) {
            return None;
        }
        let open = self.controls.open;
        let button = toolbar::tool(
            ("call-controls", Glyph::SHIELD, self.caption("Controls")),
            ("Meeting controls", None),
            open,
            palette,
            cx,
            |this, _, cx| {
                let open = !this.controls.open;
                this.open_controls(open, cx);
            },
        );
        let menu = open.then(|| {
            div().absolute().top_full().right_0().child(gpui::deferred(
                gpui::anchored()
                    .anchor(Anchor::TopRight)
                    .snap_to_window_with_margin(px(8.0))
                    .child(self.controls_menu_panel(active, palette, cx)),
            ))
        });
        Some(div().relative().child(button).children(menu).into_any_element())
    }

    /// Controls' menu, as Teams' has it.
    fn controls_menu_panel(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let options = self.shown_options(active.id);
        let locked = active.meeting.locked;
        let rows = option_rows(options, palette, cx);
        let lock = menu_row(
            "call-controls-lock",
            if locked { "Unlock meeting" } else { "Lock meeting" },
            palette,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_controls(false, cx);
            this.ask_lock(!locked, cx);
        }));
        let more = self.meeting_options_url(active, cx).map(|url| {
            menu_row("call-controls-options", "View more options", palette).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.open_controls(false, cx);
                    cx.open_url(&url);
                },
            ))
        });
        div()
            .debug_selector(|| "call-controls-menu".to_owned())
            .id("call-controls-menu")
            .occlude()
            .mt_1()
            .w(px(264.0))
            .py_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .text_sm()
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.open_controls(false, cx)))
            .child(lock)
            .children(rows)
            .when_some(more, |menu, more| menu.child(separator(palette)).child(more))
            .into_any_element()
    }

    /// People's Mute all, beside "Currently in this call": the organizer's while anyone else in the call isn't muted.
    pub(super) fn mute_all_button(&self, active: &ActiveCall, cx: &mut Context<Self>) -> Option<AnyElement> {
        let unmuted = active.view.participants.iter().any(|participant| !participant.muted)
            || active.invited.iter().any(|invited| !invited.no_response);
        if !(active.organizer && unmuted && self.takes_meeting_tools(active)) {
            return None;
        }
        Some(
            selected_by(
                "call-mute-all",
                Button::new("call-mute-all")
                    .outline()
                    .xsmall()
                    .label("Mute all")
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_mute_all(true, cx))),
            )
            .into_any_element(),
        )
    }

    /// "Mute everyone?" over the window while it is asked, as Teams words it: Mute mutes everyone else, Cancel nobody.
    pub(super) fn mute_all_confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.controls.confirm_mute_all {
            return None;
        }
        let cancel = Button::new("call-mute-all-cancel")
            .label("Cancel")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.confirm_mute_all(false, cx)));
        let mute = Button::new("call-mute-all-mute")
            .label("Mute")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| {
                this.confirm_mute_all(false, cx);
                this.ask(Ask::MuteAll, cx);
            }));
        let dialog = div()
            .debug_selector(|| "call-mute-all-confirm".to_owned())
            .occlude()
            .w(px(400.0))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_6()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Mute everyone?"),
            )
            .child(div().text_sm().child("This will mute everyone in the meeting but you."))
            .child(
                div()
                    .pt_2()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by("call-mute-all-cancel", cancel))
                    .child(selected_by("call-mute-all-mute", mute)),
            );
        Some(
            div()
                .id("call-mute-all-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .bg(gpui::black().opacity(0.4))
                .occlude()
                .child(dialog)
                .into_any_element(),
        )
    }

    /// "You've been muted" among the notices over the stage while it shows: the organizer muted the account; Unmute unmutes it (where
    /// the organizer lets attendees), the close button takes the card away.
    pub(super) fn muted_toast(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.controls.toast?;
        let unmute = (!active.meeting.unmute_blocked() && !self.core.read_only()).then(|| {
            selected_by(
                "call-muted-unmute",
                Button::new("call-muted-unmute")
                    .label("Unmute")
                    .primary()
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.controls.toast = None;
                        if this.live(cx).is_some_and(|active| active.muted) {
                            this.ask(Ask::ToggleMute, cx);
                        }
                    })),
            )
        });
        let close = selected_by(
            "call-muted-close",
            icon_button("call-muted-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.controls.toast = None;
                    cx.notify();
                })),
        );
        let card = div()
            .debug_selector(|| "call-muted-toast".to_owned())
            .occlude()
            .w(px(320.0))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Glyph::MIC_OFF.icon().size(px(16.0)))
                    .child(div().flex_1().text_sm().child("You've been muted"))
                    .child(close),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("You have been muted."),
            )
            .children(unmute);
        Some(card.into_any_element())
    }
}

/// Controls' options under Lock meeting: Enable lobby, then "Allow all participants to:" and its four, each as
/// `options` say (Teams' defaults where they say nothing).
fn option_rows(options: MeetingOptions, palette: Palette, cx: &mut Context<CallStage>) -> [AnyElement; 7] {
    let (lobby, present, chat) = (
        options.lobby.unwrap_or(true),
        options.present.unwrap_or(true),
        options.chat.unwrap_or(true),
    );
    let (unmute, camera) = (options.unmute.unwrap_or(true), options.camera.unwrap_or(true));
    [
        option_row(
            ("call-controls-lobby", "Enable lobby"),
            (lobby, false),
            MeetingOption::Lobby(!lobby),
            palette,
            cx,
        ),
        separator(palette),
        div()
            .px_3()
            .pt_2()
            .pb_1()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(palette.foreground))
            .child("Allow all participants to:")
            .into_any_element(),
        option_row(
            ("call-controls-present", "Present"),
            (present, false),
            MeetingOption::Present(!present),
            palette,
            cx,
        ),
        option_row(
            ("call-controls-chat", "Chat"),
            (chat, false),
            MeetingOption::Chat(!chat),
            palette,
            cx,
        ),
        // while everyone may present nobody is an attendee: the restrictions bind nobody, greyed as Teams has them
        option_row(
            ("call-controls-unmute", "Unmute themselves"),
            (unmute, present),
            MeetingOption::Attendees {
                unmute: !unmute,
                camera,
            },
            palette,
            cx,
        ),
        option_row(
            ("call-controls-cameras", "Turn on their cameras"),
            (camera, present),
            MeetingOption::Attendees {
                unmute,
                camera: !camera,
            },
            palette,
            cx,
        ),
    ]
}

/// One of Controls' options: its label with a check on the right while it is on (`checked`), greyed and not taking
/// the pointer while `greyed`; a click changes it to `change`.
fn option_row(
    (selector, label): (&'static str, &'static str),
    (checked, greyed): (bool, bool),
    change: MeetingOption,
    palette: Palette,
    cx: &mut Context<CallStage>,
) -> AnyElement {
    let row = menu_row(selector, label, palette).child(div().flex_none().size(px(16.0)).when(checked, |mark| {
        mark.debug_selector(move || format!("{selector}-on"))
            .child(Glyph::CHECK.icon().size(px(16.0)))
    }));
    if greyed {
        return row
            .text_color(rgb(palette.muted_foreground))
            .opacity(0.6)
            .cursor_default()
            .into_any_element();
    }
    row.on_click(cx.listener(move |this, _, _, cx| this.change_option(change, cx)))
        .into_any_element()
}

/// A row of Controls' menu: its label, hovered as a menu's row is.
fn menu_row(selector: &'static str, label: &'static str, palette: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .debug_selector(move || selector.to_owned())
        .id(selector)
        .h(px(32.0))
        .mx_1()
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .cursor_pointer()
        .hover(move |row| row.bg(rgb(palette.hover)))
        .child(div().flex_1().min_w_0().truncate().child(label))
}

/// The options' five checks alike, in Controls' order: lobby, present, chat, unmute, camera.
fn checks(options: &mut MeetingOptions) -> [&mut Option<bool>; 5] {
    [
        &mut options.lobby,
        &mut options.present,
        &mut options.chat,
        &mut options.unmute,
        &mut options.camera,
    ]
}

/// What `option` sets, and nothing else.
fn changed(option: MeetingOption) -> MeetingOptions {
    let mut options = MeetingOptions::default();
    match option {
        MeetingOption::Lobby(on) => options.lobby = Some(on),
        MeetingOption::Present(on) => options.present = Some(on),
        MeetingOption::Chat(on) => options.chat = Some(on),
        MeetingOption::Attendees { unmute, camera } => {
            options.unmute = Some(unmute);
            options.camera = Some(camera);
        }
    }
    options
}

/// Drops each value the user set (`set`) that `same` holds too: a read that shows it, or a change of it that failed.
fn drop_same(set: &mut MeetingOptions, mut same: MeetingOptions) {
    for (set, same) in checks(set).into_iter().zip(checks(&mut same)) {
        if set.is_some() && *set == *same {
            *set = None;
        }
    }
}

/// Drops each value the user set (`set`) of an option the conversation told anew, from `before` to `now`.
fn told_anew(set: &mut MeetingOptions, mut before: MeetingOptions, mut now: MeetingOptions) {
    for ((set, before), now) in checks(set).into_iter().zip(checks(&mut before)).zip(checks(&mut now)) {
        if now.is_some() && *before != *now {
            *set = None;
        }
    }
}

/// The thin line between Controls' groups.
fn separator(palette: Palette) -> AnyElement {
    div().my_1().h(px(1.0)).bg(rgb(palette.border)).into_any_element()
}

/// A group call's or a meeting's (not a 1:1 call's, nor the test call's): where Teams names the organizer and offers
/// its controls.
pub(super) fn group_or_meeting(active: &ActiveCall) -> bool {
    let group = meeting::is_meeting(active)
        || active.view.participants.len() > 1
        || active
            .chat
            .as_deref()
            .is_some_and(|chat| teams_proto::one_to_one_pair(chat).is_none());
    group && active.kind != CallKind::TestCall
}
