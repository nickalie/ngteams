//! The call window's header, as Teams' call window has it: who the call is with and its state on the left; on the
//! right its controls in Teams' order (Chat, People, Raise and React in a group call or a meeting, View, More; the
//! camera and the microphone with their devices, and Share (Stop sharing while the account shares: [`super::share`]);
//! Leave, with End meeting beside it for a meeting's organizer).
use super::*;

/// Teams' live reactions in React's order: the name the call sends, the reaction's picture's key, and its name.
pub(super) const REACTIONS: [(&str, &str, &str); 5] = [
    ("like", "like", "Like"),
    ("heart", "heart", "Love"),
    ("applause", "clap", "Applause"),
    ("laugh", "laugh", "Laugh"),
    ("surprised", "surprised", "Surprised"),
];

impl CallStage {
    /// The header: who the call is with and its state, and the controls while it is up.
    pub(super) fn header(
        &self,
        live: Option<&ActiveCall>,
        title: SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let state: SharedString = match (live, self.ended(cx)) {
            (Some(active), _) => call_view::status_with_duration(active).into(),
            (None, Some(ended)) if ended.check.is_none() => {
                let heading = ended.reason(self.core.me()).heading();
                match ended.duration {
                    Some(lasted) => format!("{heading} · {}", calls::duration_text(lasted.as_secs())).into(),
                    None => heading.into(),
                }
            }
            (None, Some(ended)) => calls::end_text(&ended.end, ended.voicemail).into(),
            (None, None) => "Call ended".into(),
        };
        div()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            // on the frame, as the main window's title bar is
            .text_color(rgb(palette.on_frame))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(live.and_then(|active| self.duration_mark(active, palette, cx)))
                            .child(
                                div()
                                    .debug_selector(|| "call-window-status".to_owned())
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(palette.on_frame).opacity(0.75))
                                    .child(state),
                            ),
                    ),
            )
            .children(live.map(|active| self.toolbar(active, palette, cx)))
    }

    /// A control's caption, as Teams words it under its icon; none in a narrow window, where the icons stand alone
    /// (their tooltips name them).
    pub(super) fn caption(&self, caption: &'static str) -> &'static str {
        if self.paging.window.0 < COMPACT_BELOW {
            ""
        } else {
            caption
        }
    }

    /// Whether the call takes Hold (and Transfer): a 1:1 call's that is up (Teams offers Hold there, not in a group call or a
    /// meeting), never the test call's or in the read-only mode.
    pub(super) fn takes_hold(&self, active: &ActiveCall) -> bool {
        let one_to_one = matches!(active.kind, CallKind::Chat | CallKind::Incoming)
            && active.view.participants.len() <= 1
            && active
                .chat
                .as_deref()
                .is_none_or(|chat| teams_proto::one_to_one_pair(chat).is_some());
        one_to_one && active.status == CallStatus::Connected && !self.core.read_only()
    }

    /// Hold, or Resume while the account holds the call: Teams' outlined Resume with its play mark in Hold's place.
    fn hold(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.takes_hold(active) {
            return None;
        }
        if active.held.is_some() {
            return Some(
                selected_by(
                    "call-resume",
                    Button::new("call-resume")
                        .outline()
                        .icon(Glyph::PLAY.icon())
                        .label("Resume")
                        .text_color(rgb(palette.on_frame))
                        .tooltip("Resume the call")
                        .on_click(cx.listener(|this, _, _, cx| this.ask(Ask::ToggleHold, cx))),
                )
                .into_any_element(),
            );
        }
        Some(tool(
            ("call-hold", Glyph::PAUSE, self.caption("Hold")),
            ("Put the call on hold", None),
            false,
            palette,
            cx,
            |this, _, cx| this.ask(Ask::ToggleHold, cx),
        ))
    }

    /// A 1:1 call's Transfer, first in Teams' toolbar: it holds the call and asks whom (its dialog); greyed out while
    /// the call is held, as Teams has it.
    fn transfer_tools(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Option<Vec<AnyElement>> {
        if !self.takes_transfer(active) {
            return None;
        }
        let transfer = tool(
            ("call-transfer", Glyph::PHONE_FORWARDED, self.caption("Transfer")),
            ("Transfer", None),
            false,
            palette,
            cx,
            Self::open_transfer,
        );
        let held = active.held.is_some();
        Some(vec![on_hold(div().child(transfer), held)])
    }

    /// Whether the call takes Raise and React: a group call's or a meeting's that is up, as Teams offers them (a 1:1
    /// call has neither), never in the read-only mode.
    fn takes_reactions(&self, active: &ActiveCall) -> bool {
        let group = active.kind == CallKind::Meeting
            || active.view.participants.len() > 1
            || active
                .chat
                .as_deref()
                .is_some_and(|chat| teams_proto::one_to_one_pair(chat).is_none());
        group && !self.core.read_only() && active.kind != CallKind::TestCall
    }

    /// Teams' call controls (see the module docs).
    fn toolbar(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let camera = self
            .calls
            .read(cx)
            .video()
            .is_some()
            .then(|| self.camera(active, palette, cx));
        let (mic, mute) = if active.muted {
            (Glyph::MIC_OFF, "Unmute")
        } else {
            (Glyph::MIC, "Mute")
        };
        let microphone = controls::blocked(
            tool(
                ("call-mute", mic, self.caption("Mic")),
                (mute, Some(&ToggleMuteCall)),
                active.muted,
                palette,
                cx,
                |this, _, cx| this.ask(Ask::ToggleMute, cx),
            ),
            active.muted && active.meeting.unmute_blocked(),
            "call-mute-blocked",
        );
        let hold_or_resume = self.hold(active, palette, cx);
        let held = active.held.is_some();
        let share = self.share_tool(active, palette, cx).map(|share| {
            controls::blocked(
                share,
                !active.sharing && active.meeting.share_blocked(),
                "call-share-blocked",
            )
        });
        let media = div()
            .flex()
            .items_center()
            .gap_1()
            .children(camera)
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(microphone)
                    .children(self.devices_menu("call-audio-devices", AUDIO, "Choose microphone and speaker", cx)),
            )
            .children(share);
        let transfer = self.transfer_tools(active, palette, cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .children(transfer.into_iter().flatten())
            .when_some(hold_or_resume, |bar, hold| bar.child(hold).child(divider(palette)))
            .children(self.meeting_tools(active, palette, cx))
            .child(self.view_menu(active.remote_share, palette, cx))
            .children(self.controls_menu(active, palette, cx))
            .child(self.more(palette, cx))
            .child(divider(palette))
            // on hold, Teams greys the camera and the microphone out until the call resumes
            .child(on_hold(media, held))
            .child(div().w_2())
            .child(self.leave(active, cx))
    }

    /// The toolbar's first controls: Chat (a call with a chat), People with how many are in the call, and Raise and
    /// React where the call takes them.
    fn meeting_tools(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let reactions = self.takes_reactions(active);
        let chat = self.call_chat(active).map(|_| {
            let shown = self.pane == Pane::Chat;
            tool(
                ("call-chat", Glyph::MESSAGE, self.caption("Chat")),
                (if shown { "Hide chat" } else { "Show chat" }, None),
                shown,
                palette,
                cx,
                |this, window, cx| {
                    let open = this.pane != Pane::Chat;
                    this.show_chat(open, window, cx);
                },
            )
        });
        let shown = self.pane == Pane::People;
        let roster = with_count(
            tool(
                ("call-roster", Glyph::USERS, self.caption("People")),
                (
                    if shown {
                        "Hide participants"
                    } else {
                        "Show participants"
                    },
                    None,
                ),
                shown,
                palette,
                cx,
                |this, _, cx| {
                    let open = this.pane != Pane::People;
                    this.show_roster(open, cx);
                },
            ),
            people_count(active),
            attention(active),
            palette,
        );
        let raise = reactions.then(|| {
            let (tip, caption) = if active.hand_raised {
                ("Lower hand", "Lower")
            } else {
                ("Raise hand", "Raise")
            };
            tool(
                ("call-raise-hand", Glyph::HAND, self.caption(caption)),
                (tip, Some(&ToggleHandCall)),
                active.hand_raised,
                palette,
                cx,
                |this, _, cx| this.toggle_hand(cx),
            )
        });
        let react = reactions.then(|| Self::react_menu(self.caption("React"), palette, cx));
        chat.into_iter()
            .chain(std::iter::once(roster))
            .chain(raise)
            .chain(react)
            .collect()
    }

    /// Leave in Teams' red, with End meeting in a menu beside it for the meeting's organizer.
    fn leave(&self, active: &ActiveCall, cx: &mut Context<Self>) -> gpui::Div {
        let leave = control(
            "call-hang-up",
            Control::Labelled(Glyph::PHONE_OFF, "Leave"),
            ("Leave call", &HangUpCall),
            Look::Danger,
            cx,
            |this, cx| this.ask(Ask::HangUp, cx),
        );
        let stage = cx.weak_entity();
        let end = (active.organizer && active.kind == CallKind::Meeting && !self.core.read_only()).then(|| {
            selected_by(
                "call-leave-options",
                Button::new("call-leave-options")
                    .ghost()
                    .xsmall()
                    .icon(Glyph::CHEVRON_DOWN.icon())
                    .text_color(rgb(theme::current(cx).on_frame))
                    .tooltip("Leave options")
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                        let (leave, end) = (stage.clone(), stage.clone());
                        menu.min_w(px(180.0))
                            .item(menu_item("call-leave", None, "Leave").on_click(move |_, _, cx| {
                                let _ = leave.update(cx, |stage, cx| stage.ask(Ask::HangUp, cx));
                            }))
                            .item(
                                menu_item("call-end-meeting", None, "End meeting").on_click(move |_, _, cx| {
                                    let _ = end.update(cx, |stage, cx| stage.confirm_end_meeting(true, cx));
                                }),
                            )
                    }),
            )
        });
        div().flex().items_center().gap_0p5().child(leave).children(end)
    }

    /// End meeting's confirmation shown (`true`, as End meeting asks first) or put away (Cancel).
    pub fn confirm_end_meeting(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.confirm_end = shown;
        cx.notify();
    }

    /// End meeting's confirmation over the whole window while it is asked, as Teams words it: End ends the meeting for
    /// everyone, Cancel leaves it running.
    pub(super) fn end_confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.confirm_end {
            return None;
        }
        let cancel = Button::new("call-end-meeting-cancel")
            .label("Cancel")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.confirm_end_meeting(false, cx)));
        let end = Button::new("call-end-meeting-end")
            .label("End")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| {
                this.confirm_end_meeting(false, cx);
                this.ask(Ask::EndMeeting, cx);
            }));
        let dialog = div()
            .debug_selector(|| "call-end-meeting-confirm".to_owned())
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
                    .child("End the meeting?"),
            )
            .child(div().text_sm().child("You'll end the meeting for everyone."))
            .child(
                div()
                    .pt_2()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by("call-end-meeting-cancel", cancel))
                    .child(selected_by("call-end-meeting-end", end)),
            );
        Some(
            div()
                .id("call-end-meeting-scrim")
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

    /// Raise hand, or Lower hand while it is up (its button or Ctrl+Shift+K); nothing where the call takes neither.
    pub(super) fn toggle_hand(&mut self, cx: &mut Context<Self>) {
        if self.live(cx).is_some_and(|active| self.takes_reactions(&active)) {
            self.ask(Ask::ToggleHand, cx);
        }
    }

    /// React: Teams' five live reactions in a row, and Raise hand under them, as its menu has it.
    fn react_menu(caption: &'static str, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let stage = cx.weak_entity();
        selected_by(
            "call-react",
            Button::new("call-react")
                .ghost()
                .h(px(48.0))
                .text_color(rgb(palette.on_frame))
                .child(tool_face(Glyph::REACT, caption))
                .tooltip("Send a reaction")
                .dropdown_menu_with_anchor(Anchor::TopCenter, move |menu: PopupMenu, _, _| {
                    let stage = stage.clone();
                    menu.item(PopupMenuItem::element(move |_, cx| reaction_row(&stage, cx)))
                }),
        )
        .into_any_element()
    }

    /// The camera's control, with its devices.
    fn camera(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let (glyph, tip) = if active.camera {
            (Glyph::VIDEO, "Turn camera off")
        } else {
            (Glyph::VIDEO_OFF, "Turn camera on")
        };
        let camera = tool(
            ("call-camera", glyph, self.caption("Camera")),
            (tip, Some(&ToggleCameraCall)),
            !active.camera,
            palette,
            cx,
            |this, _, cx| this.ask(Ask::ToggleCamera, cx),
        );
        div()
            .flex()
            .items_center()
            .child(controls::blocked(
                camera,
                !active.camera && active.meeting.camera_blocked(),
                "call-camera-blocked",
            ))
            .children(self.devices_menu("call-camera-devices", CAMERAS, "Choose camera and video effects", cx))
    }

    /// View, as Teams' has it: Gallery or Speaker (a check on the one shown), Focus on content while a share shows
    /// (`shared`), Show videos first, Max gallery size (4, 9, 16 or 49 to a page), and More options (Turn off other
    /// videos or Turn on, Hide me or Show me, Full screen).
    fn view_menu(&self, shared: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let stage = cx.weak_entity();
        let (layout, videos_first, size) = (self.layout, self.videos_first, self.paging.size);
        let options = ViewOptions {
            self_hidden: self.self_hidden,
            others_off: self.others_off,
        };
        selected_by(
            "call-view",
            Button::new("call-view")
                .ghost()
                .h(px(48.0))
                .text_color(rgb(palette.on_frame))
                .child(tool_face(Glyph::GALLERY, self.caption("View")))
                .tooltip("Change view")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, window, cx| {
                    let pick = |chosen: Layout| {
                        let stage = stage.clone();
                        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                            let _ = stage.update(cx, |stage, cx| stage.set_layout(chosen, cx));
                        }
                    };
                    let first = stage.clone();
                    let more = stage.clone();
                    let sizes = stage.clone();
                    menu.min_w(px(220.0))
                        .item(
                            menu_item("call-view-gallery", Some(Glyph::GALLERY), "Gallery")
                                .checked(layout == Layout::Gallery)
                                .on_click(pick(Layout::Gallery)),
                        )
                        .item(
                            menu_item("call-view-speaker", Some(Glyph::SPEAKER_VIEW), "Speaker")
                                .checked(layout == Layout::Speaker)
                                .on_click(pick(Layout::Speaker)),
                        )
                        .when(shared, |menu| {
                            menu.item(
                                menu_item("call-view-focus", Some(Glyph::FOCUS_CONTENT), "Focus on content")
                                    .checked(layout == Layout::Focus)
                                    .on_click(pick(Layout::Focus)),
                            )
                        })
                        .separator()
                        .item(
                            menu_item("call-view-videos-first", Some(Glyph::VIDEO), "Show videos first")
                                .checked(videos_first)
                                .on_click(move |_, _, cx| {
                                    let _ = first.update(cx, |stage, cx| stage.show_videos_first(!videos_first, cx));
                                }),
                        )
                        .submenu("Max gallery size", window, cx, move |menu, _, _| {
                            gallery_sizes(menu, &sizes, size)
                        })
                        .submenu("More options", window, cx, move |menu, window, _| {
                            view_options(menu, &more, options, window.is_fullscreen())
                        })
                }),
        )
        .into_any_element()
    }

    /// More: Record and transcribe, Language and speech (captions), Video effects and settings (a build that shows video), and Audio settings
    /// (the main window's settings on their Calls page).
    fn more(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let stage = cx.weak_entity();
        let video = self.calls.read(cx).video().is_some();
        let caption_visible = self.calls.read(cx).captions_shown();
        let caption_available = !self.core.read_only()
            && self
                .live(cx)
                .is_some_and(|call| call.status == CallStatus::Connected && call.kind != CallKind::TestCall);
        let tools = self.more_tools(cx);
        let record = self.record_tools(cx);
        selected_by(
            "call-more",
            Button::new("call-more")
                .ghost()
                .h(px(48.0))
                .text_color(rgb(palette.on_frame))
                .child(tool_face(Glyph::MORE, self.caption("More")))
                .tooltip("More actions")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, window, cx| {
                    let menu = recording::record_menu(menu, &stage, record, window, cx);
                    let menu = meeting::more_items(menu, &stage, tools);
                    let menu = if video {
                        menu.item(effects_item("call-more-video-effects", stage.clone()))
                    } else {
                        menu
                    };
                    let settings = stage.clone();
                    let menu = menu.item(
                        menu_item("call-more-device-settings", Some(Glyph::VOLUME), "Audio settings")
                            .on_click(move |_, _, cx| emit(&settings, CallStageEvent::DeviceSettings, cx)),
                    );
                    let menu = captions::more(menu, stage.clone(), caption_visible, caption_available, window, cx);
                    let menu = meeting::settings_submenu(menu, &stage, tools, window, cx);
                    meeting::help_item(menu, tools).min_w(px(240.0))
                }),
        )
        .into_any_element()
    }

    /// The menu of the devices of `kinds` (each under its name), chosen as the settings choose them; none while the
    /// devices aren't listed.
    fn devices_menu(
        &self,
        id: &'static str,
        kinds: &'static [DeviceKind],
        tip: &'static str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        self.settings.read(cx).devices()?;
        let on_frame = theme::current(cx).on_frame;
        let model = self.settings.clone();
        let stage = cx.weak_entity();
        let effects = kinds == CAMERAS;
        Some(
            selected_by(
                id,
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(Glyph::CHEVRON_DOWN.icon())
                    .text_color(rgb(on_frame))
                    .tooltip(tip)
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, cx| {
                        let menu = device_items(menu, kinds, &model, cx).separator();
                        if effects {
                            menu.item(effects_item("call-camera-video-effects", stage.clone()))
                        } else {
                            // as Teams' microphone flyout ends: the full audio settings
                            let settings = stage.clone();
                            menu.item(
                                menu_item("call-audio-more-settings", None, "More audio settings")
                                    .on_click(move |_, _, cx| emit(&settings, CallStageEvent::DeviceSettings, cx)),
                            )
                        }
                    }),
            )
            .into_any_element(),
        )
    }
}

/// How many are in the call, the account with them: People's count, as Teams shows it beside the icon.
fn people_count(active: &ActiveCall) -> Option<usize> {
    let others = active.view.participants.len();
    (others > 0).then_some(others + 1)
}

/// How many ask for attention, as Teams' People shows it in a red badge: hands up and people waiting in the lobby.
fn attention(active: &ActiveCall) -> usize {
    let hands = active
        .view
        .participants
        .iter()
        .filter(|participant| participant.hand_raised)
        .count()
        + usize::from(active.hand_raised);
    hands + active.lobby.len()
}

/// `tool` with `count` raised at its icon's top right, as Teams' People shows how many are in the call; while some ask
/// for attention (`attention`), their number in a red badge in its place.
fn with_count(tool: AnyElement, count: Option<usize>, attention: usize, palette: Palette) -> AnyElement {
    if attention > 0 {
        return div()
            .relative()
            .child(tool)
            .child(
                div()
                    .debug_selector(|| "call-roster-attention".to_owned())
                    .absolute()
                    .top(px(4.0))
                    .right(px(8.0))
                    .min_w(px(16.0))
                    .h(px(16.0))
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(theme::DANGER))
                    .text_color(gpui::white())
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(attention.to_string())),
            )
            .into_any_element();
    }
    let Some(count) = count else { return tool };
    div()
        .relative()
        .child(tool)
        .child(
            div()
                .debug_selector(|| "call-roster-count".to_owned())
                .absolute()
                .top(px(4.0))
                .right(px(6.0))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(palette.on_frame))
                .child(SharedString::from(count.to_string())),
        )
        .into_any_element()
}

/// `controls` greyed out and not taking the pointer while the call is on hold (`held`), as Teams shows them.
fn on_hold(controls: gpui::Div, held: bool) -> AnyElement {
    if !held {
        return controls.into_any_element();
    }
    div()
        .debug_selector(|| "call-held-controls".to_owned())
        .id("call-held-controls")
        .opacity(0.4)
        .occlude()
        .child(controls)
        .into_any_element()
}

/// The thin line between the toolbar's groups.
fn divider(palette: Palette) -> gpui::Div {
    div()
        .w(px(1.0))
        .h(px(32.0))
        .mx_1()
        .bg(rgb(palette.on_frame).opacity(0.25))
}

/// A row of a call menu, its test selector `selector`: its icon (when it has one) and its label.
pub(super) fn menu_item(selector: &'static str, glyph: Option<Glyph>, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .gap_2()
            .children(glyph.map(|glyph| glyph.icon().size(px(14.0))))
            .child(label)
    })
}

/// View's Max gallery size: Teams' choices of how many tiles a page of the gallery shows, a check on `chosen`.
fn gallery_sizes(menu: PopupMenu, stage: &WeakEntity<CallStage>, chosen: usize) -> PopupMenu {
    const ITEMS: [(&str, &str); 4] = [
        ("call-view-gallery-size-4", "4"),
        ("call-view-gallery-size-9", "9"),
        ("call-view-gallery-size-16", "16"),
        ("call-view-gallery-size-49", "49"),
    ];
    ITEMS
        .into_iter()
        .zip(super::pages::GALLERY_SIZES)
        .fold(menu.min_w(px(120.0)), |menu, ((selector, label), size)| {
            let stage = stage.clone();
            menu.item(
                menu_item(selector, None, label)
                    .checked(size == chosen)
                    .on_click(move |_, _, cx| {
                        let _ = stage.update(cx, |stage, cx| stage.set_gallery_size(size, cx));
                    }),
            )
        })
}

/// What View's More options shows as it stands.
#[derive(Clone, Copy)]
struct ViewOptions {
    self_hidden: bool,
    others_off: bool,
}

/// View's More options: Turn off other videos (or Turn on), Hide me (or Show me) and Full screen (or Exit).
fn view_options(menu: PopupMenu, stage: &WeakEntity<CallStage>, options: ViewOptions, full: bool) -> PopupMenu {
    let (others, hide) = (stage.clone(), stage.clone());
    let ViewOptions {
        self_hidden,
        others_off,
    } = options;
    let (others_glyph, others_label) = if others_off {
        (Glyph::VIDEO, "Turn on other videos")
    } else {
        (Glyph::VIDEO_OFF, "Turn off other videos")
    };
    let (hide_glyph, hide_label) = if self_hidden {
        (Glyph::EYE, "Show me")
    } else {
        (Glyph::EYE_OFF, "Hide me")
    };
    let (full_glyph, full_label) = if full {
        (Glyph::SHRINK, "Exit full screen")
    } else {
        (Glyph::FULLSCREEN, "Full screen")
    };
    menu.min_w(px(220.0))
        .item(
            menu_item("call-view-other-videos", Some(others_glyph), others_label).on_click(move |_, _, cx| {
                let _ = others.update(cx, |stage, cx| stage.turn_off_other_videos(!others_off, cx));
            }),
        )
        .item(
            menu_item("call-view-hide-me", Some(hide_glyph), hide_label).on_click(move |_, _, cx| {
                let _ = hide.update(cx, |stage, cx| stage.hide_self_view(!self_hidden, cx));
            }),
        )
        .item(
            menu_item("call-view-full-screen", Some(full_glyph), full_label)
                .on_click(|_, window, _| window.toggle_fullscreen()),
        )
}

/// React's row: each live reaction as its picture, sent at a click.
fn reaction_row(stage: &WeakEntity<CallStage>, cx: &mut App) -> AnyElement {
    let _ = cx;
    div()
        .debug_selector(|| "call-react-menu".to_owned())
        .flex()
        .items_center()
        .gap_1()
        .children(REACTIONS.into_iter().map(|(name, key, label)| {
            let stage = stage.clone();
            div()
                .id(SharedString::from(format!("call-react-{name}")))
                .debug_selector(move || format!("call-react-{name}"))
                .size(px(40.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .cursor_pointer()
                .hover(|button| button.bg(gpui::black().opacity(0.06)))
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                .on_click(move |_, _, cx| {
                    let _ = stage.update(cx, |stage, cx| stage.ask(Ask::React(name.to_owned()), cx));
                })
                .child(crate::reactions::emoji(key, px(28.0)))
        }))
        .into_any_element()
}

/// A control of the toolbar, Teams' way: its icon over its caption, its tooltip naming it (with the key bound to its
/// action, when it has one), marked while `marked` (muted, the camera off, the pane open).
pub(super) fn tool(
    (id, glyph, caption): (&'static str, Glyph, &'static str),
    (tip, action): (&'static str, Option<&dyn gpui::Action>),
    marked: bool,
    palette: Palette,
    cx: &mut Context<CallStage>,
    on: fn(&mut CallStage, &mut Window, &mut Context<CallStage>),
) -> AnyElement {
    let action = action.map(gpui::Action::boxed_clone);
    div()
        .debug_selector(move || id.to_owned())
        .id(id)
        .h(px(48.0))
        .min_w(px(if caption.is_empty() { 40.0 } else { 56.0 }))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        // on the frame: its ink, a lit chip while marked (muted, camera off, the people shown)
        .text_color(rgb(palette.on_frame))
        .when(marked, |tool| tool.bg(rgb(palette.on_frame).opacity(0.2)))
        .hover(move |tool| tool.bg(rgb(palette.on_frame).opacity(0.12)))
        .tooltip(move |window, cx| {
            let tooltip = Tooltip::new(tip);
            match &action {
                Some(action) => tooltip.action(action.as_ref(), None).build(window, cx),
                None => tooltip.build(window, cx),
            }
        })
        .on_click(cx.listener(move |this, _, window, cx| on(this, window, cx)))
        .child(tool_face(glyph, caption))
        .into_any_element()
}

/// A toolbar control's face: its icon over its caption (none in a narrow window), in the control's own text colour.
pub(super) fn tool_face(glyph: Glyph, caption: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_0p5()
        .child(glyph.icon().size(px(20.0)))
        .when(!caption.is_empty(), |face| face.child(div().text_xs().child(caption)))
}
