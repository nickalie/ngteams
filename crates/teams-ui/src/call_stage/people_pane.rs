//! The call window's People pane ("Participants"), as Teams shows it: a meeting's Share invite (its join link
//! copied), who waits in its lobby (Admit all, and Admit or Deny each), and who is in the call (the account first),
//! each with whether they speak, their microphone muted and their hand raised, the organizer's Mute all beside them;
//! then in a group call "People added to the chat", in a meeting "Others invited": its chat's members who aren't in
//! it, each with Request to join as the pointer or the keyboard is on them (they are called into the call, as an add
//! is: in a meeting, the add names the meeting's chat and organizer). A meeting's chat members are its invitees: only the roster says who joined. The keyboard
//! reaches each section's heading (Enter or Space folds the section) and each row, in the drawn order.
use super::*;
use crate::keyboard;
use teams_core::LobbyPerson;

/// A section of the People pane, which its heading folds and unfolds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    Lobby,
    InCall,
    Others,
}

impl Section {
    /// Its heading's selector, and the key of its focus handle.
    fn key(self) -> &'static str {
        match self {
            Self::Lobby => "call-roster-section-lobby",
            Self::InCall => "call-roster-section-in",
            Self::Others => "call-roster-section-others",
        }
    }
}

/// What the keyboard reaches in the People pane, as in Teams: each section's heading (Enter or Space folds it) and each
/// row, in the order they are drawn.
#[derive(Default)]
pub(super) struct PeopleKeys {
    handles: keyboard::Handles,
    folded: Vec<Section>,
}

impl PeopleKeys {
    fn folded(&self, section: Section) -> bool {
        self.folded.contains(&section)
    }
}

impl CallStage {
    /// The People pane.
    pub(super) fn roster_pane(
        &self,
        active: &ActiveCall,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let people = self.people.of(active.id);
        let me = self.core.me();
        let own = self_row(
            active,
            (self.own_name.clone(), self.faces.picture(me)),
            self.hand_rank(me),
            palette,
        );
        let mut rows = vec![self.person_stop("call-roster-self", own.id("call-roster-self"), palette, cx)];
        if active.people_stand_in() {
            rows.extend(people.iter().enumerate().map(|(index, (mri, name))| {
                let row = format!("call-roster-{index}");
                let face = faces::small_face(&self.faces, (&row, mri), name);
                let drawn = roster_row(row.clone(), (face, name.clone()), Signs::default(), palette)
                    .id(SharedString::from(row.clone()));
                self.person_stop(&row, drawn, palette, cx)
            }));
        } else {
            rows.extend(active.view.participants.iter().enumerate().map(|(index, participant)| {
                let name = participant_name(participant, people);
                let signs = Signs {
                    speaking: participant.speaking,
                    muted: participant.muted,
                    hand: participant
                        .hand_raised
                        .then(|| self.hand_rank(&participant.mri).unwrap_or(0)),
                    organizer: participant.organizer && controls::group_or_meeting(active),
                };
                let row = format!("call-roster-{index}");
                let face = faces::small_face(&self.faces, (&row, &participant.mri), &name);
                let drawn = roster_row(row.clone(), (face, name), signs, palette).id(SharedString::from(row.clone()));
                self.person_stop(&row, drawn, palette, cx)
            }));
        }
        rows.extend(
            active
                .invited
                .iter()
                .enumerate()
                .map(|(index, invited)| self.invited_row(index, invited, palette, cx)),
        );
        let heading = if active.is_meeting() {
            format!("In this meeting ({})", rows.len())
        } else {
            format!("Currently in this call ({})", rows.len())
        };
        let close = selected_by(
            "call-roster-close",
            icon_button("call-roster-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.show_roster(false, cx))),
        );
        let lobby = (!active.lobby.is_empty()).then(|| self.lobby_section(&active.lobby, palette, cx));
        let mute_all = self.mute_all_button(active, cx);
        let in_call = self.section_heading(Section::InCall, heading, mute_all, palette, cx);
        if self.people_keys.folded(Section::InCall) {
            rows.clear();
        }
        let added = self.added_section(active, palette, window, cx);
        let more = self.roster_more(active, cx);
        let close = div().flex().items_center().gap_1().children(more).child(close);
        side_pane("call-roster-pane", "Participants", close, palette)
            .children(self.invite(active, palette, cx))
            .children(self.invite_box(palette, cx))
            .child(
                div()
                    .id("call-roster-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(lobby)
                    .child(in_call)
                    .children(rows)
                    .children(added),
            )
            .into_any_element()
    }

    /// A meeting's Share invite, once its join link is known: a menu whose Copy meeting link puts it on the clipboard.
    fn invite(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let link = self.join_link(active)?;
        let copied = self.meeting.as_ref().is_some_and(|meeting| meeting.copied);
        let stage = cx.weak_entity();
        let button = selected_by(
            "call-share-invite",
            Button::new("call-share-invite")
                .outline()
                .small()
                .icon(Glyph::USER_PLUS.icon())
                .label("Share invite")
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu: PopupMenu, _, _| {
                    let (stage, link) = (stage.clone(), link.clone());
                    menu.min_w(px(220.0)).item(
                        PopupMenuItem::element(|_, _| {
                            div()
                                .debug_selector(|| "call-copy-link".to_owned())
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Glyph::LINK.icon().size(px(14.0)))
                                .child("Copy meeting link")
                        })
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.clone()));
                            let _ = stage.update(cx, CallStage::link_copied);
                        }),
                    )
                }),
        );
        Some(
            div()
                .px_4()
                .pb_2()
                .flex()
                .items_center()
                .gap_2()
                .child(button)
                .when(copied, |row| {
                    row.child(
                        div()
                            .debug_selector(|| "call-link-copied".to_owned())
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child("Link copied"),
                    )
                })
                .into_any_element(),
        )
    }

    /// Someone the account added who isn't in the call yet, as Teams lists them: ringing ("Calling…"), or "No response"
    /// (or "Declined") with its call-again once they didn't come in.
    fn invited_row(
        &self,
        index: usize,
        invited: &calls::Invited,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = SharedString::from(invited.name.clone());
        let again = (invited.no_response && !self.core.read_only()).then(|| {
            let ask = Ask::Add {
                invitee: invited.mri.clone(),
                name: invited.name.clone(),
            };
            let id = format!("call-roster-call-again-{index}");
            selected_by(
                id.clone(),
                icon_button(SharedString::from(id), Glyph::REFRESH, "Call again")
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| this.ask(ask.clone(), cx))),
            )
        });
        let state = if invited.declined {
            "Declined"
        } else if invited.no_response {
            "No response"
        } else {
            "Calling…"
        };
        let row = format!("call-roster-invited-{index}");
        let selector = row.clone();
        let drawn = div()
            .id(SharedString::from(row.clone()))
            .debug_selector(move || selector)
            .h(px(48.0))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .child(hand_ring(
                faces::small_face(
                    &self.faces,
                    (&format!("call-roster-invited-{index}"), invited.mri.as_str()),
                    &name,
                ),
                false,
                palette,
            ))
            .child(div().flex_1().min_w_0().truncate().child(name))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(state),
            )
            .children(again);
        self.person_stop(&row, drawn, palette, cx)
    }

    /// "People added to the chat (n)" in a group call, "Others invited (n)" in a meeting: its chat's members who aren't
    /// in it, waiting in its lobby or being called, each with its avatar and name, and Request to join while the
    /// pointer or the keyboard is on it (not in the read-only mode).
    fn added_section(
        &self,
        active: &ActiveCall,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let meeting = active.is_meeting();
        if !controls::group_or_meeting(active)
            || active.status != CallStatus::Connected
            || (!meeting && active.view.participants.is_empty())
        {
            return None;
        }
        let me = self.core.me().to_owned();
        let out: Vec<(String, SharedString)> = self
            .people
            .of(active.id)
            .iter()
            .filter(|(mri, _)| {
                // an app in the chat is nobody to ask in; a roster may write an MRI's GUID in another case
                let same = |other: &str| other.eq_ignore_ascii_case(mri);
                !same(&me)
                    && !crate::bot_face::is_bot(mri)
                    && !active
                        .view
                        .participants
                        .iter()
                        .any(|participant| same(&participant.mri))
                    && !active.invited.iter().any(|invited| same(invited.mri.as_str()))
                    && !active.lobby.iter().any(|person| same(person.mri.as_str()))
            })
            .cloned()
            .collect();
        if out.is_empty() {
            return None;
        }
        let heading = if meeting {
            format!("Others invited ({})", out.len())
        } else {
            format!("People added to the chat ({})", out.len())
        };
        let folded = self.people_keys.folded(Section::Others);
        let rows: Vec<AnyElement> = out
            .into_iter()
            .enumerate()
            .filter(|_| !folded)
            .map(|(index, person)| {
                // Request to join shows as the pointer or the keyboard is on the row, as Teams shows it
                let reached = self.hovered_added() == Some(index)
                    || self
                        .people_keys
                        .handles
                        .get(&added_key(index), cx)
                        .contains_focused(window, cx);
                self.added_row(index, person, reached, palette, cx)
            })
            .collect();
        let heading = self.section_heading(Section::Others, heading, None, palette, cx);
        Some(
            div()
                .debug_selector(|| "call-roster-added".to_owned())
                .pt_2()
                .child(heading)
                .children(rows)
                .into_any_element(),
        )
    }

    /// One of "People added to the chat": their avatar and name, and Request to join (when `request`).
    fn added_row(
        &self,
        index: usize,
        (mri, name): (String, SharedString),
        request: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let request = (request && !self.core.read_only()).then(|| {
            let ask = Ask::Add {
                invitee: Mri::new(&mri),
                name: name.to_string(),
            };
            let id = format!("call-roster-request-{index}");
            selected_by(
                id.clone(),
                Button::new(SharedString::from(id))
                    .outline()
                    .xsmall()
                    .label("Request to join")
                    .on_click(cx.listener(move |this, _, _, cx| this.ask(ask.clone(), cx))),
            )
        });
        let row = added_key(index);
        let selector = row.clone();
        let drawn = div()
            .id(SharedString::from(row.clone()))
            .debug_selector(move || selector)
            .h(px(48.0))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.hover_added(hovered.then_some(index), cx);
            }))
            .child(hand_ring(
                faces::small_face(&self.faces, (&format!("call-roster-added-{index}"), &mri), &name),
                false,
                palette,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(palette.foreground))
                    .child(name),
            )
            .children(request);
        self.person_stop(&row, drawn, palette, cx)
    }

    /// Copy meeting link put the meeting's link on the clipboard: Share invite says so.
    fn link_copied(&mut self, cx: &mut Context<Self>) {
        if let Some(meeting) = self.meeting.as_mut() {
            meeting.copied = true;
        }
        cx.notify();
    }

    /// The meeting's join link, when this call is a meeting's and its thread named one.
    pub(super) fn join_link(&self, active: &ActiveCall) -> Option<String> {
        if active.kind != CallKind::Meeting {
            return None;
        }
        self.meeting
            .as_ref()
            .filter(|meeting| meeting.call == active.id)
            .and_then(|meeting| meeting.details.as_ref()?.join_url.clone())
    }

    /// "Waiting in the lobby": Admit all, then each person waiting with Deny and Admit.
    fn lobby_section(&self, waiting: &[LobbyPerson], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let everyone: Vec<Mri> = waiting.iter().map(|person| person.mri.clone()).collect();
        let admit_all = (!self.core.read_only()).then(|| {
            selected_by(
                "call-lobby-admit-all",
                Button::new("call-lobby-admit-all")
                    .ghost()
                    .xsmall()
                    .label("Admit all")
                    .text_color(rgb(palette.brand))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        for waiting in &everyone {
                            this.ask(
                                Ask::Lobby {
                                    waiting: waiting.clone(),
                                    admit: true,
                                },
                                cx,
                            );
                        }
                    })),
            )
            .into_any_element()
        });
        let heading = format!("Waiting in lobby ({})", waiting.len());
        let heading = self.section_heading(Section::Lobby, heading, admit_all, palette, cx);
        let shown = !self.people_keys.folded(Section::Lobby);
        div()
            .debug_selector(|| "call-roster-lobby".to_owned())
            .pb_2()
            .child(heading)
            .children(
                waiting
                    .iter()
                    .enumerate()
                    .filter(|_| shown)
                    .map(|(index, person)| self.lobby_row(index, person, palette, cx)),
            )
            .into_any_element()
    }

    /// One person waiting in the lobby: their avatar and name, then Deny and Admit as Teams' icons.
    fn lobby_row(&self, index: usize, person: &LobbyPerson, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let name: SharedString = person.name.clone().unwrap_or_else(|| SOMEONE.to_owned()).into();
        let decide = |admit: bool, cx: &mut Context<Self>| {
            let (verb, glyph, tip) = if admit {
                ("admit", Glyph::CHECK, "Admit")
            } else {
                ("deny", Glyph::X, "Deny")
            };
            let id = format!("call-roster-{verb}-{index}");
            let waiting = person.mri.clone();
            selected_by(
                id.clone(),
                icon_button(SharedString::from(id), glyph, tip)
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ask(
                            Ask::Lobby {
                                waiting: waiting.clone(),
                                admit,
                            },
                            cx,
                        );
                    })),
            )
        };
        let buttons =
            (!self.core.read_only()).then(|| div().flex().gap_1().child(decide(false, cx)).child(decide(true, cx)));
        let row = format!("call-roster-waiting-{index}");
        let selector = row.clone();
        let drawn = div()
            .id(SharedString::from(row.clone()))
            .debug_selector(move || selector)
            .h(px(44.0))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .child(faces::small_face(
                &self.faces,
                (&format!("call-roster-waiting-{index}"), person.mri.as_str()),
                &name,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(palette.foreground))
                    .child(name),
            )
            .children(buttons);
        self.person_stop(&row, drawn, palette, cx)
    }

    /// A row of the People pane as a tab stop keyed by its selector `key`, lit while the keyboard is on it.
    fn person_stop(&self, key: &str, row: gpui::Stateful<gpui::Div>, palette: Palette, cx: &App) -> AnyElement {
        keyboard::stop(SharedString::from(key.to_owned()), row, palette)
            .handle(&self.people_keys.handles.get(key, cx))
            .into_any_element()
    }

    /// A section's heading, as Teams draws it: a chevron and its title, which a click, Enter or Space folds or unfolds;
    /// an action on its right.
    fn section_heading(
        &self,
        section: Section,
        text: String,
        action: Option<AnyElement>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let key = section.key();
        let chevron = if self.people_keys.folded(section) {
            Glyph::CHEVRON_RIGHT
        } else {
            Glyph::CHEVRON_DOWN
        };
        let toggle = div()
            .id(key)
            .debug_selector(move || key.to_owned())
            .flex_1()
            .min_w_0()
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(palette.muted_foreground))
            .on_click(cx.listener(move |this, _, _, cx| this.fold_section(section, cx)))
            .child(chevron.icon().size(px(12.0)))
            .child(SharedString::from(text));
        div()
            .h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(keyboard::stop(key, toggle, palette).handle(&self.people_keys.handles.get(key, cx)))
            .children(action)
    }

    /// Folds `section` of the People pane, or unfolds it.
    fn fold_section(&mut self, section: Section, cx: &mut Context<Self>) {
        let folded = &mut self.people_keys.folded;
        if let Some(at) = folded.iter().position(|kept| *kept == section) {
            folded.remove(at);
        } else {
            folded.push(section);
        }
        cx.notify();
    }

    /// The key of the People pane's row or heading the keyboard is on, if any.
    #[must_use]
    pub fn people_focus(&self, window: &Window) -> Option<String> {
        self.people_keys.handles.focused(window)
    }
}

/// The selector of the row `index` of "People added to the chat" or "Others invited", and its focus key.
fn added_key(index: usize) -> String {
    format!("call-roster-added-{index}")
}

/// What a row of the People pane shows of its person: whether they speak or are muted, where their raised hand
/// stands in the line of hands up (from 1; 0 when unknown), and whether they organize the meeting.
#[derive(Clone, Copy, Default)]
struct Signs {
    speaking: bool,
    muted: bool,
    hand: Option<usize>,
    organizer: bool,
}

/// A side pane of the call window (People, Chat, Video effects, Meeting info): its title and Close over what it holds.
pub(super) fn side_pane(
    selector: &'static str,
    title: &'static str,
    close: impl IntoElement,
    palette: Palette,
) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .w(px(PANE_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(rgb(palette.surface))
        .border_l_1()
        .border_color(rgb(palette.border))
        .child(
            div()
                .h(px(48.0))
                .flex_none()
                .px_4()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                .child(close),
        )
}

/// The signs on the right of the row `row`: a raised hand with its place in the line (Teams' "1 ✋" chip, named
/// `{row}-hand-{place}`), the speaker while they speak, the microphone (muted or not).
fn signs(row: &str, signs: Signs, palette: Palette) -> gpui::Div {
    let Signs {
        speaking, muted, hand, ..
    } = signs;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .children(hand.map(|rank| {
            let chip = format!("{row}-hand-{rank}");
            div()
                .debug_selector(move || chip)
                .h(px(22.0))
                .px_1p5()
                .flex()
                .items_center()
                .gap_1()
                .rounded_md()
                .bg(rgb(palette.hover))
                .text_xs()
                .when(rank > 0, |chip| chip.child(SharedString::from(rank.to_string())))
                .child(
                    Glyph::HAND
                        .icon()
                        .size(px(14.0))
                        .text_color(rgb(gallery::hand_colour(palette))),
                )
        }))
        .child(if muted {
            Glyph::MIC_OFF
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.muted_foreground))
        } else if speaking {
            Glyph::VOLUME.icon().size(px(16.0)).text_color(rgb(palette.brand))
        } else {
            Glyph::MIC
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.muted_foreground))
        })
}

/// One row of the People pane: the person's `face` (their picture, else their initials; ringed in gold while their
/// hand is up), their name with "Organizer" under it for the meeting's, and their signs.
fn roster_row(
    selector: impl Into<String>,
    (avatar, name): (gpui_component::avatar::Avatar, SharedString),
    shown: Signs,
    palette: Palette,
) -> gpui::Div {
    let selector = selector.into();
    let shown_signs = signs(&selector, shown, palette);
    div()
        .debug_selector(move || selector)
        .h(px(48.0))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .child(hand_ring(avatar, shown.hand.is_some(), palette))
        .child(name_and_role(name, shown.organizer, palette))
        .child(shown_signs)
}

/// A row's avatar, ringed in Teams' raised-hand gold while `raised`.
fn hand_ring(avatar: impl IntoElement, raised: bool, palette: Palette) -> gpui::Div {
    div()
        .flex_none()
        .p(px(2.0))
        .rounded_full()
        .border_2()
        .border_color(if raised {
            rgb(gallery::hand_colour(palette))
        } else {
            gpui::transparent_black().into()
        })
        .child(avatar)
}

/// A row's name, with "Organizer" under it for the meeting's organizer, as Teams' People pane says it.
fn name_and_role(name: SharedString, organizer: bool, palette: Palette) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(div().truncate().child(name))
        .when(organizer, |names| {
            names.child(
                div()
                    .debug_selector(|| "call-roster-organizer".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Organizer"),
            )
        })
}

/// The account's own row of the People pane, as Teams shows it: its name ("You" until its profile names it) and its
/// picture, else its initials (its mark while its profile names it nothing), "Organizer" under it for the meeting's
/// organizer, and its signs.
fn self_row(
    active: &ActiveCall,
    (name, picture): (Option<SharedString>, Option<&people::Picture>),
    rank: Option<usize>,
    palette: Palette,
) -> gpui::Div {
    let shown = Signs {
        speaking: active.view.speaking && !active.muted,
        muted: active.muted,
        hand: active.hand_raised.then(|| rank.unwrap_or(0)),
        organizer: active.organizer && controls::group_or_meeting(active),
    };
    let (mark, name) = match (name, picture) {
        (name, Some(picture)) => (
            people::avatar(name.as_ref(), Some(picture), Size::Small)
                .debug_selector(|| "call-roster-self-picture".to_owned())
                .into_any_element(),
            name.unwrap_or_else(|| SharedString::from("You")),
        ),
        (Some(name), None) => (
            people::avatar(Some(&name), None, Size::Small)
                .debug_selector(|| "call-roster-self-initials".to_owned())
                .into_any_element(),
            name,
        ),
        (None, None) => (
            div()
                .size(px(24.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(Glyph::USER.icon().size(px(14.0)))
                .into_any_element(),
            SharedString::from("You"),
        ),
    };
    div()
        .debug_selector(|| "call-roster-self".to_owned())
        .h(px(48.0))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .child(hand_ring(mark, shown.hand.is_some(), palette))
        .child(name_and_role(name, shown.organizer, palette))
        .child(signs("call-roster-self", shown, palette))
}
