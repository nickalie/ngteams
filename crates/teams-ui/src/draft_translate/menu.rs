//! The menu that picks the language drafts are translated into: the compose box's language button and the settings'
//! row open the same menu, and both set the one setting every box reads
//! ([`crate::settings::Settings::message_translate_to`]).
//!
//! A search field on top, as the chat header's language menu has it, and the language chosen first; typing filters
//! the listed languages ([`super::COMMON_LANGUAGES`]) by name or code; a name no listed one starts with is offered
//! after them as "Use “…”", so any language can be typed. Up and Down move through the rows, Enter picks the one
//! highlighted (the first when none is), Escape closes it. Test selectors start with the menu's own prefix:
//! `<prefix>-menu`, `<prefix>-search`, `<prefix>-custom`, and `<prefix>-<language>` for a listed one (its name in lower
//! case, a dash for anything not a letter or digit).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, px, rgb,
};
use gpui_component::Selectable;
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::popover::Popover;

use super::{language_name, listed, matching};
use crate::icons::Glyph;
use crate::settings::SettingsModel;
use crate::theme::{self, Palette};

/// The search field's hint.
pub const SEARCH_HINT: &str = "Search or type a language";
/// What the menu says when nothing matches and nothing typed can be used.
pub const NO_MATCH: &str = "No languages found";

/// One of the menu's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A language typed that isn't listed, by its name as it would be kept.
    Typed(String),
    Listed(&'static str),
}

impl Row {
    /// The language it sets.
    #[must_use]
    pub fn language(&self) -> &str {
        match self {
            Self::Typed(name) => name,
            Self::Listed(name) => name,
        }
    }
}

/// The rows `query` leaves: the listed languages it matches, then what it names, when no listed name is or starts
/// with it (so the start of a listed name offers only that one).
#[must_use]
pub fn rows_for(query: &str) -> Vec<Row> {
    let lower = query.trim().to_lowercase();
    let offered = !lower.is_empty()
        && listed(query).is_none()
        && !super::COMMON_LANGUAGES
            .iter()
            .any(|name| name.to_lowercase().starts_with(&lower));
    let typed = offered.then(|| language_name(query)).flatten().map(Row::Typed);
    matching(query).into_iter().map(Row::Listed).chain(typed).collect()
}

/// A listed language's test selector key.
#[must_use]
pub fn key_of(language: &str) -> String {
    language
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// What the menu tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanguageMenuEvent {
    /// A language was picked and the menu closed.
    Chosen,
    /// Escape closed it.
    Escaped,
}

/// The menu (see the module docs).
pub struct LanguageMenu {
    settings: Entity<SettingsModel>,
    prefix: &'static str,
    input: Entity<InputState>,
    open: bool,
    /// The row the keyboard is on, of those the query leaves.
    highlighted: Option<usize>,
    _input: Subscription,
    _settings: Subscription,
}

impl EventEmitter<LanguageMenuEvent> for LanguageMenu {}

impl LanguageMenu {
    pub fn new(
        settings: Entity<SettingsModel>,
        prefix: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let observing = cx.observe(&settings, |_, _, cx| cx.notify());
        Self {
            settings,
            prefix,
            input,
            open: false,
            highlighted: None,
            _input: events,
            _settings: observing,
        }
    }

    /// The language the settings hold.
    #[must_use]
    pub fn chosen(&self, cx: &App) -> String {
        self.settings.read(cx).settings().message_translate_to.clone()
    }

    /// Whether the menu is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The rows shown now: with nothing typed, the language chosen first, then the others.
    #[must_use]
    pub fn rows(&self, cx: &App) -> Vec<Row> {
        let query = self.input.read(cx).value();
        let mut rows = rows_for(&query);
        if query.trim().is_empty() {
            let chosen = self.chosen(cx);
            rows.retain(|row| row.language() != chosen);
            rows.insert(0, listed(&chosen).map_or(Row::Typed(chosen), Row::Listed));
        }
        rows
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

    /// Moves the keyboard's row by `step`, within the rows shown.
    pub fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.rows(cx).len();
        self.highlighted = (count > 0).then(|| match self.highlighted {
            None if step < 0 => count - 1,
            None => 0,
            Some(at) => at.saturating_add_signed(step).min(count - 1),
        });
        cx.notify();
    }

    /// Enter: the row highlighted, else the first one the query leaves.
    pub fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let at = self.highlighted.unwrap_or(0);
        if let Some(row) = rows.get(at) {
            self.choose(row.language(), window, cx);
        }
    }

    /// Sets `language` for every box, and closes the menu.
    pub fn choose(&mut self, language: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(language) = language_name(language) else {
            return;
        };
        self.settings.update(cx, |model, cx| {
            model.change(|settings| settings.message_translate_to = language, cx);
        });
        self.set_open(false, window, cx);
        cx.emit(LanguageMenuEvent::Chosen);
    }

    fn row(&self, at: usize, row: &Row, chosen: &str, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let (selector, label): (String, SharedString) = match row {
            Row::Typed(name) => (
                format!("{}-custom", self.prefix),
                format!("Use \u{201c}{name}\u{201d}").into(),
            ),
            Row::Listed(name) => (format!("{}-{}", self.prefix, key_of(name)), (*name).into()),
        };
        let highlighted = self.highlighted == Some(at);
        let checked = row.language() == chosen;
        let language = row.language().to_owned();
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
            .on_click(cx.listener(move |this, _, window, cx| this.choose(&language, window, cx)))
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

    fn search_field(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let prefix = self.prefix;
        div()
            .debug_selector(move || format!("{prefix}-search"))
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
                cx.emit(LanguageMenuEvent::Escaped);
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
            )
            .into_any_element()
    }
}

impl Focusable for LanguageMenu {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for LanguageMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let chosen = self.chosen(cx);
        let rows = self.rows(cx);
        let empty = rows.is_empty();
        let rows: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(at, row)| self.row(at, &row, &chosen, palette, cx))
            .collect();
        let prefix = self.prefix;
        let list = div()
            .id(SharedString::from(format!("{prefix}-list")))
            .debug_selector(move || format!("{prefix}-list"))
            .max_h(px(320.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .children(rows)
            .when(empty, |list| {
                list.child(
                    div()
                        .px_2()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child(NO_MATCH),
                )
            });
        div()
            .debug_selector(move || format!("{prefix}-menu"))
            .w(px(260.0))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Translate messages into"),
            )
            .child(self.search_field(palette, cx))
            .child(list)
    }
}

/// `face` opening `menu` in a popover anchored by its own `anchor` corner (see [`Popover::anchor`]).
pub fn popover<T>(menu: &Entity<LanguageMenu>, id: &'static str, anchor: gpui::Anchor, face: T, cx: &App) -> AnyElement
where
    T: Selectable + IntoElement + 'static,
{
    let open = menu.read(cx).is_open();
    let focus = menu.read(cx).focus_handle(cx);
    let toggled = menu.clone();
    let content = menu.clone();
    Popover::new(id)
        .anchor(anchor)
        .trigger(face.selected(open))
        .track_focus(&focus)
        .open(open)
        .on_open_change(move |open, window, cx| {
            toggled.update(cx, |menu, cx| menu.set_open(*open, window, cx));
        })
        .content(move |_, _, _| content.clone())
        .into_any_element()
}
