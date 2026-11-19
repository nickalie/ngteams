//! The chat header's language picker ([`super::header_button`]): Teams' translation mark, lit while the chat is
//! translated and spinning while its messages are being translated (its tooltip says how far it has come),
//! opening a menu of "Show as written" and every language ([`TARGET_LANGUAGES`]), the chosen one checked.
//! The list is long, so the menu opens with a search field on top, as Teams' long menus have: typing filters the
//! languages by name (or code), Up and Down move through what is left, Enter picks the one highlighted (the first
//! when none is), Escape closes it. Under the list, "Translate every message" ([`EVERY_MESSAGE`]) is a switch, on by
//! default, whatever the search leaves: off, the chat is translated a message at a time
//! ([`super::ChatTranslation::ask`]). It is the last row Up and Down reach, Enter turns it over, and the menu stays
//! open. The menu opens from its button, or from a message's Translate with no language picked: its own open state
//! drives its popover ([`LanguagePicker::set_open`]), so a thread's view uses its channel's
//! ([`super::ChatTranslation::share_picker`]).

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{Animation, AnimationExt as _};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Transformation, WeakEntity, Window, div, percentage, px, rgb,
};
use gpui_component::Selectable;
use gpui_component::button::Button;
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::popover::Popover;
use gpui_component::switch::Switch;
use gpui_component::{Sizable as _, Size};

use super::{ChatTranslation, Progress};
use crate::icons::{Glyph, icon_button};
use crate::theme::{self, Palette};
use crate::translate::{TARGET_LANGUAGES, language};

/// The search field's hint.
pub const SEARCH_HINT: &str = "Search languages";
/// What the menu says when nothing matches.
pub const NO_MATCH: &str = "No languages found";
/// The first row: the chat read as written.
pub const AS_WRITTEN: &str = "Show as written";
/// The switch under the languages: every message translated, or a message at a time.
pub const EVERY_MESSAGE: &str = "Translate every message";

/// One of the menu's rows: a language by its code, or `None` for "Show as written".
type Row = (Option<&'static str>, &'static str);

/// The rows `query` leaves: "Show as written" and the languages whose name or code it matches, ignoring case.
#[must_use]
pub fn matching(query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    let off = std::iter::once((None, AS_WRITTEN));
    let languages = TARGET_LANGUAGES.iter().map(|(code, name)| (Some(*code), *name));
    off.chain(languages)
        .filter(|(code, name)| {
            query.is_empty()
                || name.to_lowercase().contains(&query)
                || code.is_some_and(|code| code.starts_with(query.as_str()))
        })
        .collect()
}

/// The menu (see the module docs).
pub struct LanguagePicker {
    model: WeakEntity<ChatTranslation>,
    input: Entity<InputState>,
    open: bool,
    /// The row the keyboard is on, of those the query leaves.
    highlighted: Option<usize>,
    _input: Subscription,
}

impl LanguagePicker {
    pub fn new(model: WeakEntity<ChatTranslation>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(SEARCH_HINT));
        let events = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.highlighted = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.enter(window, cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        );
        Self {
            model,
            input,
            open: false,
            highlighted: None,
            _input: events,
        }
    }

    /// Whether the menu is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The rows shown now.
    #[must_use]
    pub fn rows(&self, cx: &App) -> Vec<Row> {
        matching(&self.input.read(cx).value())
    }

    /// Opens or closes the menu; it opens with an empty search field, holding the keyboard.
    pub fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if open == self.open {
            return;
        }
        self.open = open;
        self.highlighted = None;
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            if open {
                input.focus(window, cx);
            }
        });
        cx.notify();
    }

    /// Types `query` into the search field (tests, the tour).
    pub fn search(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(query.to_owned(), window, cx));
        self.highlighted = None;
        cx.notify();
    }

    /// Moves the keyboard's row by `step`, within the rows shown and the switch after them.
    pub fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let last = self.rows(cx).len();
        let at = match self.highlighted {
            None if step < 0 => last,
            None => 0,
            Some(at) => at.saturating_add_signed(step).min(last),
        };
        self.highlighted = Some(at);
        cx.notify();
    }

    /// Enter: the row highlighted (the switch turned over), else the first one the query leaves.
    fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let chosen = self.highlighted.or_else(|| (!rows.is_empty()).then_some(0));
        match chosen {
            Some(at) if at == rows.len() => self.toggle_every(window, cx),
            Some(at) => {
                if let Some((code, _)) = rows.get(at).copied() {
                    self.choose(code, window, cx);
                }
            }
            None => {}
        }
    }

    /// Turns "Translate every message" over, with or without a language; the menu stays open, its search field
    /// holding the keyboard.
    pub fn toggle_every(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = self.model.update(cx, |translation, cx| {
            let every = translation.every_message();
            translation.set_every_message(!every, cx);
        });
        self.input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Reads the chat in `language` (`None`: as written), and closes the menu.
    pub fn choose(&mut self, language: Option<&'static str>, window: &mut Window, cx: &mut Context<Self>) {
        // a chat closed meanwhile has nothing left to set
        let _ = self
            .model
            .update(cx, |translation, cx| translation.set_language(language, cx));
        self.set_open(false, window, cx);
    }

    fn row(
        &self,
        at: usize,
        row: Row,
        chosen: Option<&'static str>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (code, label) = row;
        let selector = format!("chat-translate-{}", code.unwrap_or("off"));
        let highlighted = self.highlighted == Some(at);
        let checked = chosen == code;
        let id = selector.clone();
        div()
            .debug_selector(move || selector.clone())
            .id(SharedString::from(id))
            .w_full()
            .h(px(32.0))
            .px_2()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(palette.foreground))
            .when(highlighted, |row| row.bg(rgb(palette.hover)))
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, window, cx| this.choose(code, window, cx)))
            .child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .text_color(rgb(palette.brand))
                    .children(checked.then(|| Glyph::CHECK.icon().size(px(14.0)))),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .into_any_element()
    }

    fn every_row(&self, highlighted: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let every = self.model.upgrade().is_none_or(|model| model.read(cx).every_message());
        let switch = Switch::new("chat-translate-every-switch")
            .checked(every)
            .with_size(Size::Small)
            .tab_stop(false)
            .accessibility_label(EVERY_MESSAGE);
        let row = div()
            .debug_selector(|| "chat-translate-every".to_owned())
            .id("chat-translate-every")
            .w_full()
            .h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(palette.foreground))
            .when(highlighted, |row| row.bg(rgb(palette.hover)))
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_every(window, cx)))
            .child(div().w(px(14.0)).flex_none())
            .child(div().flex_1().min_w_0().truncate().child(EVERY_MESSAGE))
            .child(switch);
        div()
            .pt_1()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(row)
            .into_any_element()
    }
}

impl Focusable for LanguagePicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for LanguagePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let chosen = self.model.upgrade().and_then(|model| model.read(cx).language());
        let rows = self.rows(cx);
        let empty = rows.is_empty();
        let every = self.every_row(self.highlighted == Some(rows.len()), palette, cx);
        let rows: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(at, row)| self.row(at, row, chosen, palette, cx))
            .collect();
        let search = div()
            .debug_selector(|| "chat-translate-search".to_owned())
            .h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.brand))
            .bg(rgb(palette.background))
            .text_sm()
            .text_color(rgb(palette.foreground))
            // Up, Down and Escape are the menu's while the field has the keyboard
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.move_highlight(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.move_highlight(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.set_open(false, window, cx);
                cx.stop_propagation();
            }))
            .child(
                Glyph::SEARCH
                    .icon()
                    .size(px(14.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.input).appearance(false).aria_label(SEARCH_HINT)),
            );
        let list = div()
            .id("chat-translate-languages")
            .debug_selector(|| "chat-translate-languages".to_owned())
            .max_h(px(320.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .children(rows)
            .when(empty, |list| {
                list.child(
                    div()
                        .debug_selector(|| "chat-translate-none".to_owned())
                        .px_2()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child(NO_MATCH),
                )
            });
        div()
            .debug_selector(|| "chat-translate-menu".to_owned())
            .w(px(260.0))
            .flex()
            .flex_col()
            .gap_1()
            .child(search)
            .child(list)
            .child(every)
    }
}

/// The menu of `model`'s picker, made the first time it is drawn.
pub fn picker_of(model: &Entity<ChatTranslation>, window: &mut Window, cx: &mut App) -> Entity<LanguagePicker> {
    if let Some(picker) = model.read(cx).picker() {
        return picker.clone();
    }
    let weak = model.downgrade();
    let picker = cx.new(|cx| LanguagePicker::new(weak, window, cx));
    model.update(cx, |model, _| model.share_picker(picker.clone()));
    picker
}

/// The header button's tooltip (and accessible name): what it does, or how far the chat's translation has come.
#[must_use]
pub fn tip_of(translation: &ChatTranslation) -> String {
    let Some((_, name)) = translation.language().and_then(language) else {
        return "Translate chat".to_owned();
    };
    if matches!(translation.status(), super::Status::Failed(_)) {
        return format!("Translating into {name} stopped");
    }
    match translation.progress() {
        Some(Progress { done, total }) if done < total => format!("Translating into {name}… {done} of {total}"),
        _ if !translation.every_message() => format!("Messages translated into {name} one at a time"),
        _ => format!("Chat translated into {name}"),
    }
}

/// The header's button: Teams' translation mark, or a spinning ring while the chat is being translated. It stays a live
/// button while it spins (the kit's loading button takes no clicks), so its menu can still turn translation off.
#[derive(IntoElement)]
struct Trigger {
    button: Button,
    busy: bool,
}

impl Selectable for Trigger {
    fn selected(mut self, selected: bool) -> Self {
        self.button = self.button.selected(selected);
        self
    }

    fn is_selected(&self) -> bool {
        self.button.is_selected()
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        if !self.busy {
            return self.button.into_any_element();
        }
        let ring = || Glyph::LOADING.icon();
        if !crate::motion::on(cx) {
            return self.button.icon(ring()).into_any_element();
        }
        self.button
            .with_animation(
                "chat-translate-busy",
                Animation::new(Duration::from_millis(900)).repeat(),
                move |button, turn| button.icon(ring().transform(Transformation::rotate(percentage(turn)))),
            )
            .into_any_element()
    }
}

/// The chat header's language picker over `model` (see the module docs).
pub fn header_button(model: &Entity<ChatTranslation>, window: &mut Window, cx: &mut App) -> AnyElement {
    let translation = model.read(cx);
    let chosen = translation.language();
    let busy = translation.busy();
    let tip = tip_of(translation);
    let picker = picker_of(model, window, cx);
    let open = picker.read(cx).is_open();
    let focus = picker.read(cx).focus_handle(cx);
    let button = Trigger {
        button: icon_button("chat-translate", Glyph::LANGUAGES, tip),
        busy,
    }
    .selected(chosen.is_some() || open);
    let toggled = picker.clone();
    let content = picker.clone();
    let popover = Popover::new("chat-translate-popover")
        .anchor(gpui::Anchor::TopRight)
        .trigger(button)
        .track_focus(&focus)
        .open(open)
        .on_open_change(move |open, window, cx| {
            toggled.update(cx, |picker, cx| picker.set_open(*open, window, cx));
        })
        .content(move |_, _, _| content.clone());
    let face = div().child(popover);
    let face = if busy {
        face.debug_selector(|| "chat-translate-busy".to_owned())
    } else {
        face
    };
    div()
        .debug_selector(|| "chat-translate".to_owned())
        .child(face)
        .into_any_element()
}
