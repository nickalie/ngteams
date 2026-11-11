use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, AsyncApp, Bounds, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, actions, anchored, deferred, div, px, rgb,
};
use gpui_component::Size;

use super::{ChatView, measured};
use crate::people::Face;
use crate::reactions;
use crate::theme::Palette;

pub const DELAY: Duration = Duration::from_millis(500);
pub const GRACE: Duration = Duration::from_millis(150);
const FACE: f32 = 20.0;
const MIN_WIDTH: f32 = 200.0;
const MAX_WIDTH: f32 = 320.0;
const MAX_HEIGHT: f32 = 320.0;

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

#[derive(Default)]
pub(super) struct Reactors {
    hovered: Option<Hovered>,
    at: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Reactors {
    fn is(&self, message: &str, key: &str) -> bool {
        self.hovered
            .as_ref()
            .is_some_and(|hovered| hovered.message == message && hovered.key == key)
    }

    pub(super) fn measure(&self, message: &str, key: &str) -> Option<AnyElement> {
        self.is(message, key).then(|| measured(&self.at))
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
            self.hovered = None;
        }
        focused
    }
}

pub(super) fn reaction_keys(list: gpui::Div, cx: &mut Context<ChatView>) -> gpui::Div {
    list.on_action(cx.listener(|this, _: &NextReaction, _, cx| this.step_reaction(true, cx)))
        .on_action(cx.listener(|this, _: &PreviousReaction, _, cx| this.step_reaction(false, cx)))
        .on_action(cx.listener(|this, _: &PressReaction, _, cx| this.press_reaction(cx)))
}

impl ChatView {
    #[must_use]
    pub fn reactors_shown(&self) -> Option<(&str, &str)> {
        self.reactors
            .hovered
            .as_ref()
            .filter(|hovered| hovered.shown)
            .map(|hovered| (hovered.message.as_str(), hovered.key.as_str()))
    }

    #[must_use]
    pub fn reaction_focus(&self) -> Option<(&str, &str)> {
        self.reactors
            .hovered
            .as_ref()
            .filter(|hovered| hovered.by == Opened::ByKeys)
            .map(|hovered| (hovered.message.as_str(), hovered.key.as_str()))
    }

    pub fn show_reactors(&mut self, of: Option<(&str, &str)>, cx: &mut Context<Self>) {
        self.reactors.hovered = of.map(|(message, key)| Hovered {
            message: message.to_owned(),
            key: key.to_owned(),
            shown: true,
            by: Opened::ByPointer,
            on_pill: true,
            on_card: false,
            wait: None,
        });
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
                self.reactors.hovered = Some(Hovered {
                    key: key.clone(),
                    message,
                    shown: true,
                    by: Opened::ByKeys,
                    on_pill: false,
                    on_card: false,
                    wait: None,
                });
            }
            None => {
                self.reactors.unfocus();
            }
        }
        cx.notify();
    }

    fn press_reaction(&mut self, cx: &mut Context<Self>) {
        let Some((message, key)) = self
            .reaction_focus()
            .map(|(message, key)| (message.to_owned(), key.to_owned()))
        else {
            return;
        };
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
        self.reactors.hovered = Some(Hovered {
            message: message.to_owned(),
            key: key.to_owned(),
            shown,
            by: Opened::ByPointer,
            on_pill: true,
            on_card: false,
            wait,
        });
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
            self.reactors.hovered = None;
            cx.notify();
            return;
        }
        hovered.wait = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(GRACE).await;
            let _ = this.update(cx, |view, cx| {
                view.reactors.hovered = None;
                cx.notify();
            });
        }));
    }

    pub(super) fn reactors_card(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (message, key) = self.reactors_shown()?;
        let at = self.reactors.at.get()?;
        let people = self.reactors_of(message, key);
        if people.is_empty() {
            return None;
        }
        let rows = people.iter().map(|mri| self.reactor_row(mri)).collect::<Vec<_>>();
        let selector = format!("reactors-{key}");
        let card = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector)
            .min_w(px(MIN_WIDTH))
            .max_w(px(MAX_WIDTH))
            .max_h(px(MAX_HEIGHT))
            .overflow_y_scroll()
            .py_3()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .px_4()
                    .pb_2()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(SharedString::from(reactions::description(key).to_owned())),
            )
            .children(rows);
        let held = div()
            .id("reactors")
            .pt_1()
            .occlude()
            .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover_reactors_card(*on, cx)))
            .child(card);
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::TopCenter)
                    .position(at.bottom_center())
                    .snap_to_window_with_margin(px(8.0))
                    .child(held),
            )
            .with_priority(1)
            .into_any_element(),
        )
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
