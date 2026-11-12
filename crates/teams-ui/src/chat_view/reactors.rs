//! Who reacted: a card that names everyone who gave one of a message's reactions, in the order they did, as Teams'
//! own does.
//!
//! - **Pointer**: the card opens once the pointer has rested on a reaction pill for [`DELAY`]; while one is shown,
//!   another pill shows its own at once. It stays while the pointer is on the pill or the card and closes [`GRACE`]
//!   after it left both.
//! - **Keyboard**: on the selected message, Right and Left move through its pills ([`NextReaction`],
//!   [`PreviousReaction`]); the pill under the keys is drawn selected and its card shows at once, with no timeout.
//!   Escape, or another message selected, leaves the pills. Space ([`PressReaction`]) sets or takes back the account's
//!   own reaction, only on a pill still shown on the selected message.
//! - **Where**: under its pill, or over it when the window has no room below, never on it (the card would take the
//!   pill's clicks). It is drawn only while its pill is: a pill scrolled out of the list takes its card with it.
//! - A card whose pill has gone (everyone took the reaction back, or the message went) closes, its keyboard focus too.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AsyncApp, Bounds, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, actions, anchored, canvas, deferred, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Size;

use super::ChatView;
use crate::people::Face;
use crate::reactions;
use crate::theme::Palette;

/// How long the pointer rests on a pill before its card opens.
pub const DELAY: Duration = Duration::from_millis(500);
/// How long a card waits for the pointer to come back to its pill or itself.
pub const GRACE: Duration = Duration::from_millis(150);
const FACE: f32 = 20.0;
const MIN_WIDTH: f32 = 200.0;
const MAX_WIDTH: f32 = 320.0;
const MAX_HEIGHT: f32 = 320.0;
/// The gap between the pill and its card, held by the card so the pointer crosses it without leaving.
const GAP: f32 = 4.0;
/// The room the card keeps from the window's edges.
const MARGIN: f32 = 8.0;
/// The card's border, top and bottom.
const BORDER: f32 = 2.0;

actions!(teams_ui, [NextReaction, PreviousReaction, PressReaction]);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Opened {
    ByPointer,
    ByKeys,
}

struct Hovered {
    message: String,
    key: String,
    shown: bool,
    by: Opened,
    on_pill: bool,
    on_card: bool,
    wait: Option<Task<()>>,
}

/// What the last frame drew: the pill hovered or focused, and its card's content.
#[derive(Default)]
struct Drawn {
    /// Where the pill was drawn; `None` until it is, after it changed and once it isn't drawn any more.
    pill: Cell<Option<Bounds<Pixels>>>,
    /// The pill was drawn since the card last looked: the card (drawn after the list's rows) checks it every frame.
    pill_now: Cell<bool>,
    /// The height the card's content takes, unclipped; `None` until the card was first drawn.
    content: Cell<Option<Pixels>>,
}

#[derive(Default)]
pub(super) struct Reactors {
    hovered: Option<Hovered>,
    drawn: Rc<Drawn>,
}

impl Reactors {
    fn is(&self, message: &str, key: &str) -> bool {
        self.hovered
            .as_ref()
            .is_some_and(|hovered| hovered.message == message && hovered.key == key)
    }

    /// Hovers or focuses `hovered`: another pill's card forgets where the last one was drawn, so it never shows there.
    fn aim(&mut self, hovered: Option<Hovered>) {
        let same = match (&self.hovered, &hovered) {
            (Some(was), Some(now)) => was.message == now.message && was.key == now.key,
            _ => false,
        };
        if !same {
            self.forget();
        }
        self.hovered = hovered;
    }

    fn forget(&self) {
        self.drawn.pill.set(None);
        self.drawn.content.set(None);
    }

    /// Over the pill hovered or focused: keeps where it is drawn, for its card.
    pub(super) fn measure(&self, message: &str, key: &str) -> Option<AnyElement> {
        self.is(message, key).then(|| {
            let drawn = Rc::clone(&self.drawn);
            canvas(
                move |bounds, window, _| {
                    drawn.pill_now.set(true);
                    if drawn.pill.replace(Some(bounds)) != Some(bounds) {
                        // the card is drawn before the list's rows: it moves with its pill on the next frame
                        window.on_next_frame(|window, _| window.refresh());
                    }
                },
                |_, (), _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element()
        })
    }

    pub(super) fn focused(&self, message: &str, key: &str) -> bool {
        self.is(message, key)
            && self
                .hovered
                .as_ref()
                .is_some_and(|hovered| hovered.by == Opened::ByKeys)
    }

    pub(super) fn unfocus(&mut self) -> bool {
        let focused = self
            .hovered
            .as_ref()
            .is_some_and(|hovered| hovered.by == Opened::ByKeys);
        if focused {
            self.aim(None);
        }
        focused
    }
}

/// Inside the card's content, drawn after the list's rows: keeps the content's height and, when the pill wasn't
/// drawn this frame (scrolled out of the list), forgets where it was so the next frame draws no card.
fn card_measure(drawn: &Rc<Drawn>) -> AnyElement {
    let drawn = Rc::clone(drawn);
    canvas(
        move |bounds, window, _| {
            let gone = !drawn.pill_now.replace(false);
            if gone {
                drawn.pill.set(None);
            }
            if gone || drawn.content.replace(Some(bounds.size.height)) != Some(bounds.size.height) {
                // a refresh while drawing asks for no frame: ask for the next one, which draws or hides the card
                window.on_next_frame(|window, _| window.refresh());
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// Where a card goes for a pill drawn at `pill`: under it when the card's `content` fits there in a window `high`
/// tall, else on the side with more room (`true`: over it), never on it; and the card's height there at most.
fn place(pill: Bounds<Pixels>, content: Pixels, high: Pixels) -> (bool, Pixels) {
    let card = (content + px(BORDER)).min(px(MAX_HEIGHT));
    let below = high - px(MARGIN) - pill.bottom() - px(GAP);
    let above = pill.top() - px(GAP) - px(MARGIN);
    if card <= below || below >= above {
        (false, below.max(px(0.0)).min(px(MAX_HEIGHT)))
    } else {
        (true, above.max(px(0.0)).min(px(MAX_HEIGHT)))
    }
}

pub(super) fn reaction_keys(list: gpui::Div, cx: &mut Context<ChatView>) -> gpui::Div {
    list.on_action(cx.listener(|this, _: &NextReaction, _, cx| this.step_reaction(true, cx)))
        .on_action(cx.listener(|this, _: &PreviousReaction, _, cx| this.step_reaction(false, cx)))
        .on_action(cx.listener(|this, _: &PressReaction, _, cx| this.press_reaction(cx)))
}

impl ChatView {
    /// The pill (message, key) whose card is shown.
    #[must_use]
    pub fn reactors_shown(&self) -> Option<(&str, &str)> {
        self.reactors
            .hovered
            .as_ref()
            .filter(|hovered| hovered.shown)
            .map(|hovered| (hovered.message.as_str(), hovered.key.as_str()))
    }

    /// The pill (message, key) under the keys.
    #[must_use]
    pub fn reaction_focus(&self) -> Option<(&str, &str)> {
        self.reactors
            .hovered
            .as_ref()
            .filter(|hovered| hovered.by == Opened::ByKeys)
            .map(|hovered| (hovered.message.as_str(), hovered.key.as_str()))
    }

    /// Shows the card of `of`'s pill (message, key) as the pointer does once it rested there, or none.
    pub fn show_reactors(&mut self, of: Option<(&str, &str)>, cx: &mut Context<Self>) {
        let hovered = of.map(|(message, key)| Hovered {
            message: message.to_owned(),
            key: key.to_owned(),
            shown: true,
            by: Opened::ByPointer,
            on_pill: true,
            on_card: false,
            wait: None,
        });
        self.reactors.aim(hovered);
        cx.notify();
    }

    fn reactors_of(&self, message: &str, key: &str) -> &[String] {
        self.message_items()
            .find(|item| item.id == message)
            .and_then(|item| item.reactions.iter().find(|reaction| reaction.key == key))
            .map_or(&[], |reaction| reaction.users.as_slice())
    }

    fn step_reaction(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(message) = self.selected.clone() else {
            return;
        };
        let keys: Vec<String> = self
            .message_items()
            .find(|item| item.id == message)
            .map(|item| item.reactions.iter().map(|reaction| reaction.key.clone()).collect())
            .unwrap_or_default();
        let at = self
            .reaction_focus()
            .filter(|(focused, _)| *focused == message)
            .and_then(|(_, focused)| keys.iter().position(|key| key == focused));
        let next = match (at, forward) {
            (None, true) => Some(0),
            (Some(at), true) => Some((at + 1).min(keys.len().saturating_sub(1))),
            (Some(at), false) => at.checked_sub(1),
            (None, false) => None,
        };
        match next.and_then(|at| keys.get(at)) {
            Some(key) => {
                self.reactors.aim(Some(Hovered {
                    key: key.clone(),
                    message,
                    shown: true,
                    by: Opened::ByKeys,
                    on_pill: false,
                    on_card: false,
                    wait: None,
                }));
            }
            None => {
                self.reactors.unfocus();
            }
        }
        cx.notify();
    }

    /// Space sets or takes back the account's own reaction on the pill under the keys: only one still shown on the
    /// message selected. A pill whose last reaction was taken back since has gone, so Space sets nothing the user
    /// can't see.
    fn press_reaction(&mut self, cx: &mut Context<Self>) {
        let Some((message, key)) = self
            .reaction_focus()
            .filter(|(message, _)| self.selected.as_deref() == Some(*message))
            .map(|(message, key)| (message.to_owned(), key.to_owned()))
        else {
            return;
        };
        if self.reactors_of(&message, &key).is_empty() {
            return;
        }
        self.toggle_reaction(&message, &key, cx);
    }

    pub(super) fn hover_pill(&mut self, message: &str, key: &str, hovered: bool, cx: &mut Context<Self>) {
        let same = self.reactors.is(message, key);
        if !hovered {
            if same && let Some(hovered) = &mut self.reactors.hovered {
                hovered.on_pill = false;
                self.leave_reactors(cx);
            }
            return;
        }
        if same {
            if let Some(hovered) = &mut self.reactors.hovered {
                hovered.on_pill = true;
                hovered.wait = None;
            }
            return;
        }
        let shown = self.reactors_shown().is_some();
        let wait = (!shown).then(|| {
            cx.spawn(async move |this, cx: &mut AsyncApp| {
                cx.background_executor().timer(DELAY).await;
                let _ = this.update(cx, ChatView::reactors_due);
            })
        });
        self.reactors.aim(Some(Hovered {
            message: message.to_owned(),
            key: key.to_owned(),
            shown,
            by: Opened::ByPointer,
            on_pill: true,
            on_card: false,
            wait,
        }));
        cx.notify();
    }

    fn reactors_due(&mut self, cx: &mut Context<Self>) {
        if let Some(hovered) = &mut self.reactors.hovered {
            hovered.shown = true;
            cx.notify();
        }
    }

    fn hover_reactors_card(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(hovered) = &mut self.reactors.hovered else {
            return;
        };
        hovered.on_card = on;
        if on {
            hovered.wait = None;
        } else {
            self.leave_reactors(cx);
        }
    }

    fn leave_reactors(&mut self, cx: &mut Context<Self>) {
        let Some(hovered) = &mut self.reactors.hovered else {
            return;
        };
        if hovered.on_pill || hovered.on_card || hovered.by == Opened::ByKeys {
            return;
        }
        if !hovered.shown {
            self.reactors.aim(None);
            cx.notify();
            return;
        }
        hovered.wait = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(GRACE).await;
            let _ = this.update(cx, |view, cx| {
                view.reactors.aim(None);
                cx.notify();
            });
        }));
    }

    /// The card of the pill shown, where its pill was drawn this frame (see the module docs); none while that isn't
    /// known yet, and none once nobody gave that reaction any more.
    pub(super) fn reactors_card(
        &self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Some((message, key)) = self.reactors_shown() else {
            self.reactors.forget();
            return None;
        };
        let at = self.reactors.drawn.pill.get()?;
        let people = self.reactors_of(message, key);
        if people.is_empty() {
            return None;
        }
        let content = self.reactors.drawn.content.get();
        let (over, high) = place(at, content.unwrap_or_default(), window.viewport_size().height);
        let rows = people.iter().map(|mri| self.reactor_row(mri)).collect::<Vec<_>>();
        let selector = format!("reactors-{key}");
        let card = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector)
            .min_w(px(MIN_WIDTH))
            .max_w(px(MAX_WIDTH))
            .max_h(high)
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .py_3()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .px_4()
                            .pb_2()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .child(SharedString::from(reactions::description(key).to_owned())),
                    )
                    .children(rows)
                    .child(card_measure(&self.reactors.drawn)),
            );
        let held = div()
            .id("reactors")
            .map(|held| if over { held.pb(px(GAP)) } else { held.pt(px(GAP)) })
            .occlude()
            .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover_reactors_card(*on, cx)))
            // its side is known once its content was measured: not drawn before, so it never shows over the pill
            .when(content.is_none(), |held| held.opacity(0.0))
            .child(card);
        let (anchor, position) = if over {
            (gpui::Anchor::BottomCenter, at.top_center())
        } else {
            (gpui::Anchor::TopCenter, at.bottom_center())
        };
        Some(
            deferred(
                anchored()
                    .anchor(anchor)
                    .position(position)
                    .snap_to_window_with_margin(px(MARGIN))
                    .child(held),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// Closes the card, and leaves the pill under the keys, of a reaction no longer shown: everyone took it back, or its
    /// message went. No pointer leaves a pill that isn't drawn any more, and an Escape shouldn't be spent on it.
    pub(super) fn close_gone_reactors(&mut self) {
        let gone = self
            .reactors
            .hovered
            .as_ref()
            .is_some_and(|hovered| self.reactors_of(&hovered.message, &hovered.key).is_empty());
        if gone {
            self.reactors.aim(None);
        }
    }

    fn reactor_row(&self, mri: &str) -> AnyElement {
        let shown = self.shown(mri);
        let name = shown.and_then(|shown| shown.name.clone());
        let face = Face {
            named: name.is_some(),
            picture: shown.and_then(|shown| shown.picture.clone()),
        };
        let name = name.unwrap_or_else(|| SharedString::from(mri.to_owned()));
        let selector = format!("reactor-{mri}");
        div()
            .debug_selector(move || selector)
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_1p5()
            .child(face.avatar(&name, Size::Small).size(px(FACE)))
            .child(div().min_w_0().truncate().child(name))
            .into_any_element()
    }
}
