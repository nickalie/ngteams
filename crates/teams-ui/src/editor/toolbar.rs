//! The editor's format bar, as Teams' message box shows it expanded: Bold, Italic, Underline, Strikethrough | bulleted
//! and numbered lists | highlight, font colour, font size | quote, link, code | More (paragraph styles, indent,
//! outdent, a horizontal line, a table, clear formatting, undo, redo), and Discard on the right. Each button acts on
//! the editor it was made for and gives it the focus back; a button whose format is on where the cursor is shows
//! selected.
//!
//! The colours are Teams' own, from its web client: the font colours its palette names (Red, Tuscan orange,
//! Sunglow, Pear, Eucalyptus, Faded jade, Fun blue, Tyrian purple) and the highlights (Cranberry, Pumpkin, Marigold,
//! Forest, Seafoam, Light teal, Blue, Lilac), each sent as the hex Teams writes; the sizes Small, Medium and Large.
//! A table isn't written by this editor: its entry shows, disabled, saying so.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, anchored, deferred, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, Size};
use teams_proto::chat::{FontSize, Marks, Rgb};

use super::doc::Kind;
use super::{Bold, BulletList, Editor, Italic, NumberedList, Quote, Strikethrough, Underline};
use crate::chat_view::measured;
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::rich_text;
use crate::theme::{self, Palette};

/// Teams' font colours: its name for each and the hex it writes.
pub const FONT_COLOURS: [(&str, u32); 8] = [
    ("Red", 0xB6_424C),
    ("Tuscan orange", 0xCD_5937),
    ("Sunglow", 0xFD_C030),
    ("Pear", 0xBD_CB4C),
    ("Eucalyptus", 0x2B_9B62),
    ("Faded jade", 0x37_797B),
    ("Fun blue", 0x1E_53A3),
    ("Tyrian purple", 0xA5_397A),
];

/// Teams' highlights: its name for each and the hex it writes.
pub const HIGHLIGHTS: [(&str, u32); 8] = [
    ("Cranberry", 0xDF_9299),
    ("Pumpkin", 0xF4_A593),
    ("Marigold", 0xFD_D472),
    ("Forest", 0xE5_F18F),
    ("Seafoam", 0x82_CDA8),
    ("Light teal", 0x9D_D9DB),
    ("Blue", 0xC7_D4E8),
    ("Lilac", 0xEB_D3E1),
];

/// What a table entry says: the editor writes none.
pub const NO_TABLES: &str = "Tables can't be written here yet";

/// A toolbar button, px (its icon about half of it).
const BUTTON: f32 = 28.0;

/// A colour swatch, px.
const SWATCH: f32 = 22.0;

/// Which of the bar's menus is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Highlight,
    FontColour,
    FontSize,
    Link,
    More,
}

/// What the bar tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatBarEvent {
    /// Discard was pressed: the draft is to go.
    Discard,
}

/// The link dialog's two fields.
struct LinkForm {
    text: Entity<InputState>,
    address: Entity<InputState>,
    _events: Vec<Subscription>,
}

/// The format bar (see the module docs).
pub struct FormatBar {
    editor: Entity<Editor>,
    open: Option<Menu>,
    /// Where each menu's button was last drawn, for the menu to open under it.
    anchors: [Rc<Cell<Option<Bounds<Pixels>>>>; 5],
    link: Option<LinkForm>,
    /// Whether Discard is offered.
    discard: bool,
    _editor: Subscription,
}

impl EventEmitter<FormatBarEvent> for FormatBar {}

impl FormatBar {
    /// The bar of `editor`; it draws again whenever the editor changes, so its buttons show what is on.
    pub fn new(editor: Entity<Editor>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&editor, |_, _, cx| cx.notify());
        Self {
            editor,
            open: None,
            anchors: Default::default(),
            link: None,
            discard: true,
            _editor: observing,
        }
    }

    /// It without Discard.
    #[must_use]
    pub fn without_discard(mut self) -> Self {
        self.discard = false;
        self
    }

    /// The menu open, if one is.
    #[must_use]
    pub fn open_menu(&self) -> Option<Menu> {
        self.open
    }

    /// Opens `menu`, or closes it when it is open.
    pub fn toggle(&mut self, menu: Menu, window: &mut Window, cx: &mut Context<Self>) {
        if self.open == Some(menu) {
            return self.close(window, cx);
        }
        self.open = Some(menu);
        if menu == Menu::Link {
            self.open_link(window, cx);
        }
        cx.notify();
    }

    /// Closes the open menu and gives the editor the focus back.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = None;
        self.link = None;
        let focus = self.editor.read(cx).focus.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Runs `act` on the editor, closes any menu and gives the editor the focus.
    fn act(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        act: impl FnOnce(&mut Editor, &mut Context<Editor>),
    ) {
        self.editor.update(cx, act);
        self.close(window, cx);
    }

    fn open_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, address) = {
            let editor = self.editor.read(cx);
            match editor.link_at_cursor() {
                Some((text, href, _)) if editor.selection().is_empty() => (text, href),
                _ => (editor.selected_text(), String::new()),
            }
        };
        let text = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Text to display")
                .default_value(text)
        });
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Address")
                .default_value(address)
        });
        let events = [&text, &address]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this: &mut Self, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.insert_link(window, cx);
                    }
                })
            })
            .collect();
        address.update(cx, |input, cx| input.focus(window, cx));
        self.link = Some(LinkForm {
            text,
            address,
            _events: events,
        });
    }

    /// Inserts the link the dialog holds: its address made `https://` when it names no scheme.
    fn insert_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.link else { return };
        let text = form.text.read(cx).value().to_string();
        let address = link_address(form.address.read(cx).value().trim());
        let Some(address) = address else { return };
        self.act(window, cx, |editor, cx| editor.insert_link(&text, &address, cx));
    }

    // --- drawing ---

    /// A format's button: its tooltip names its key; selected while the format is on.
    #[expect(
        clippy::too_many_arguments,
        reason = "one toolbar button: its id, glyph, tip, key, state and press"
    )]
    fn format_button(
        id: &'static str,
        glyph: Glyph,
        tip: &'static str,
        action: &dyn gpui::Action,
        on: bool,
        cx: &mut Context<Self>,
        press: impl Fn(&mut Editor, &mut Context<Editor>) + 'static,
    ) -> AnyElement {
        let button = icon_button_with_key(id, glyph, tip, action)
            .with_size(Size::Size(px(BUTTON)))
            .selected(on)
            .on_click(cx.listener(move |this, _, window, cx| this.act(window, cx, &press)));
        selected_by(id, button).into_any_element()
    }

    /// A menu's button, the menu opening under it.
    fn menu_button(
        &self,
        id: &'static str,
        glyph: Glyph,
        tip: &'static str,
        menu: Menu,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let anchor = Rc::clone(&self.anchors[menu as usize]);
        let button = icon_button(id, glyph, tip)
            .with_size(Size::Size(px(BUTTON)))
            .selected(self.open == Some(menu))
            .on_click(cx.listener(move |this, _, window, cx| this.toggle(menu, window, cx)));
        selected_by(id, button)
            .relative()
            .child(measured(&anchor))
            .into_any_element()
    }

    fn divider(palette: Palette) -> AnyElement {
        div()
            .flex_none()
            .w(px(1.0))
            .h(px(18.0))
            .mx_1()
            .bg(rgb(palette.border))
            .into_any_element()
    }

    fn marks(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let editor = self.editor.read(cx);
        let on = |mark| editor.has_mark(mark);
        let (bold, italic, underline, strike) = (
            on(Marks::BOLD),
            on(Marks::ITALIC),
            on(Marks::UNDERLINE),
            on(Marks::STRIKE),
        );
        vec![
            Self::format_button("format-bold", Glyph::BOLD, "Bold", &Bold, bold, cx, |e, cx| {
                e.toggle_mark(Marks::BOLD, cx);
            }),
            Self::format_button(
                "format-italic",
                Glyph::ITALIC,
                "Italic",
                &Italic,
                italic,
                cx,
                |e, cx| {
                    e.toggle_mark(Marks::ITALIC, cx);
                },
            ),
            Self::format_button(
                "format-underline",
                Glyph::UNDERLINE,
                "Underline",
                &Underline,
                underline,
                cx,
                |e, cx| {
                    e.toggle_mark(Marks::UNDERLINE, cx);
                },
            ),
            Self::format_button(
                "format-strikethrough",
                Glyph::STRIKETHROUGH,
                "Strikethrough",
                &Strikethrough,
                strike,
                cx,
                |e, cx| e.toggle_mark(Marks::STRIKE, cx),
            ),
        ]
    }

    fn lists(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let kind = self.editor.read(cx).block_kind();
        vec![
            Self::format_button(
                "format-bullets",
                Glyph::LIST,
                "Bulleted list",
                &BulletList,
                kind == Kind::Bullet,
                cx,
                |e, cx| e.toggle_kind(Kind::Bullet, cx),
            ),
            Self::format_button(
                "format-numbers",
                Glyph::LIST_ORDERED,
                "Numbered list",
                &NumberedList,
                kind == Kind::Numbered,
                cx,
                |e, cx| e.toggle_kind(Kind::Numbered, cx),
            ),
        ]
    }

    fn inserts(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let editor = self.editor.read(cx);
        let quote = editor.block_kind() == Kind::Quote;
        let code = editor.block_kind() == Kind::Code || editor.has_mark(Marks::CODE);
        vec![
            Self::format_button("format-quote", Glyph::QUOTE, "Quote", &Quote, quote, cx, |e, cx| {
                e.toggle_kind(Kind::Quote, cx);
            }),
            self.menu_button("format-link", Glyph::LINK, "Insert link", Menu::Link, cx),
            {
                let button = icon_button(
                    "format-code",
                    Glyph::CODE,
                    concat!("Code (", ctrl!(), "+Shift+B block)"),
                )
                .with_size(Size::Size(px(BUTTON)))
                .selected(code)
                .on_click(cx.listener(|this, _, window, cx| this.act(window, cx, super::Editor::code)));
                selected_by("format-code", button).into_any_element()
            },
        ]
    }

    /// A swatch of `colour` named `name`, choosing it; `None` is the swatch that takes the colour off.
    #[expect(
        clippy::too_many_arguments,
        reason = "one swatch: its id, name, colour, state and choice"
    )]
    fn swatch(
        id: String,
        name: &'static str,
        colour: Option<u32>,
        chosen: bool,
        palette: Palette,
        cx: &mut Context<Self>,
        choose: impl Fn(&mut Editor, &mut Context<Editor>) + 'static,
    ) -> AnyElement {
        let face = div()
            .size(px(SWATCH))
            .rounded_full()
            .border_1()
            .border_color(rgb(palette.border))
            .map(|face| match colour {
                Some(colour) => face.bg(rgb(colour)),
                // no colour: the surface crossed out
                None => face
                    .bg(rgb(palette.surface))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Glyph::X.icon().size(px(12.0)).text_color(rgb(palette.muted_foreground))),
            });
        let selector = id.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || selector.clone())
            .p(px(3.0))
            .rounded_full()
            .border_2()
            .border_color(if chosen {
                rgb(palette.brand)
            } else {
                gpui::transparent_black().into()
            })
            .cursor_pointer()
            .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(name).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.act(window, cx, &choose)))
            .child(face)
            .into_any_element()
    }

    fn colour_menu(&self, highlight: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let attrs = self.editor.read(cx).current_attrs();
        let current = if highlight {
            attrs.span.highlight
        } else {
            attrs.span.color
        };
        let (choices, none_name, key) = if highlight {
            (HIGHLIGHTS, "No highlight", "highlight")
        } else {
            (FONT_COLOURS, "Automatic", "colour")
        };
        let mut swatches = vec![Self::swatch(
            format!("format-{key}-none"),
            none_name,
            None,
            current.is_none(),
            palette,
            cx,
            move |e, cx| {
                if highlight {
                    e.set_highlight(None, cx);
                } else {
                    e.set_color(None, cx);
                }
            },
        )];
        for (index, (name, colour)) in choices.into_iter().enumerate() {
            swatches.push(Self::swatch(
                format!("format-{key}-{index}"),
                name,
                Some(colour),
                current == Some(Rgb(colour)),
                palette,
                cx,
                move |e, cx| {
                    if highlight {
                        e.set_highlight(Some(Rgb(colour)), cx);
                    } else {
                        e.set_color(Some(Rgb(colour)), cx);
                    }
                },
            ));
        }
        div()
            .flex()
            .flex_wrap()
            .w(px((SWATCH + 10.0) * 5.0))
            .gap_1()
            .children(swatches)
            .into_any_element()
    }

    /// A row of a menu: an icon, a label, a check when chosen; disabled with `disabled` (its tooltip saying why).
    #[expect(
        clippy::too_many_arguments,
        reason = "one menu row: its id, glyph, label, state and choice"
    )]
    fn menu_row(
        id: &'static str,
        glyph: Option<Glyph>,
        label: &'static str,
        chosen: bool,
        palette: Palette,
        cx: &mut Context<Self>,
        choose: impl Fn(&mut Editor, &mut Context<Editor>) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, window, cx| this.act(window, cx, &choose)))
            .child(
                div()
                    .w(px(16.0))
                    .flex_none()
                    .children(glyph.map(|glyph| glyph.icon().size(px(14.0)))),
            )
            .child(div().flex_1().child(label))
            .when(chosen, |row| row.child(Glyph::CHECK.icon().size(px(14.0))))
    }

    fn size_menu(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let size = self.editor.read(cx).current_attrs().span.size;
        div()
            .w(px(160.0))
            .flex()
            .flex_col()
            .child(Self::menu_row(
                "format-size-small",
                None,
                "Small",
                size == Some(FontSize::Small),
                palette,
                cx,
                |e, cx| {
                    e.set_size(Some(FontSize::Small), cx);
                },
            ))
            .child(Self::menu_row(
                "format-size-medium",
                None,
                "Medium",
                size.is_none(),
                palette,
                cx,
                |e, cx| {
                    e.set_size(None, cx);
                },
            ))
            .child(Self::menu_row(
                "format-size-large",
                None,
                "Large",
                size == Some(FontSize::Large),
                palette,
                cx,
                |e, cx| {
                    e.set_size(Some(FontSize::Large), cx);
                },
            ))
            .into_any_element()
    }

    fn more_menu(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let separator = || div().h(px(1.0)).my_1().bg(rgb(palette.border));
        let table = div()
            .id("format-table")
            .debug_selector(|| "format-table".to_owned())
            .h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .text_color(rgb(palette.muted_foreground))
            .tooltip(|window, cx| gpui_component::tooltip::Tooltip::new(NO_TABLES).build(window, cx))
            .child(div().w(px(16.0)).child(Glyph::TABLE.icon().size(px(14.0))))
            .child("Insert table");
        div()
            .w(px(220.0))
            .flex()
            .flex_col()
            .children(self.kind_rows(palette, cx))
            .child(separator())
            .child(Self::menu_row(
                "format-indent",
                Some(Glyph::INDENT),
                "Increase indent",
                false,
                palette,
                cx,
                |e, cx| e.indent(true, cx),
            ))
            .child(Self::menu_row(
                "format-outdent",
                Some(Glyph::OUTDENT),
                "Decrease indent",
                false,
                palette,
                cx,
                |e, cx| e.indent(false, cx),
            ))
            .child(Self::menu_row(
                "format-rule",
                Some(Glyph::MINUS),
                "Horizontal line",
                false,
                palette,
                cx,
                super::Editor::insert_rule,
            ))
            .child(table)
            .child(separator())
            .children(Self::history_rows(palette, cx))
            .into_any_element()
    }

    /// More's rows that set the block's kind: Paragraph and the headings, the cursor's checked.
    fn kind_rows(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        let current = self.editor.read(cx).block_kind();
        [
            ("format-paragraph", Glyph::PILCROW, "Paragraph", Kind::Paragraph),
            ("format-heading-1", Glyph::HEADING_1, "Heading 1", Kind::Heading(1)),
            ("format-heading-2", Glyph::HEADING_2, "Heading 2", Kind::Heading(2)),
            ("format-heading-3", Glyph::HEADING_3, "Heading 3", Kind::Heading(3)),
        ]
        .into_iter()
        .map(|(id, glyph, label, kind)| {
            Self::menu_row(id, Some(glyph), label, current == kind, palette, cx, move |e, cx| {
                e.set_kind(kind, cx);
            })
        })
        .collect()
    }

    /// More's last rows: Clear formatting, Undo and Redo.
    fn history_rows(palette: Palette, cx: &mut Context<Self>) -> [gpui::Stateful<gpui::Div>; 3] {
        [
            Self::menu_row(
                "format-clear",
                Some(Glyph::REMOVE_FORMATTING),
                "Clear formatting",
                false,
                palette,
                cx,
                super::Editor::clear_formatting,
            ),
            Self::menu_row(
                "format-undo",
                Some(Glyph::UNDO),
                "Undo",
                false,
                palette,
                cx,
                super::Editor::undo,
            ),
            Self::menu_row(
                "format-redo",
                Some(Glyph::REDO),
                "Redo",
                false,
                palette,
                cx,
                super::Editor::redo,
            ),
        ]
    }

    fn link_menu(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let form = self.link.as_ref()?;
        let has_link = self.editor.read(cx).link_at_cursor().is_some();
        let field = |label: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(label))
                .child(Input::new(input).small())
        };
        let insert = Button::new("format-link-insert")
            .primary()
            .small()
            .label("Insert")
            .on_click(cx.listener(|this, _, window, cx| this.insert_link(window, cx)));
        let cancel = Button::new("format-link-cancel")
            .ghost()
            .small()
            .label("Cancel")
            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));
        let remove = has_link.then(|| {
            Button::new("format-link-remove")
                .ghost()
                .small()
                .label("Remove link")
                .on_click(cx.listener(|this, _, window, cx| this.act(window, cx, super::Editor::remove_link)))
        });
        Some(
            div()
                .w(px(300.0))
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Insert link"),
                )
                .child(field("Text to display", &form.text))
                .child(field("Address", &form.address))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .children(remove.map(|remove| selected_by("format-link-remove", remove)))
                        .child(selected_by("format-link-cancel", cancel))
                        .child(selected_by("format-link-insert", insert)),
                )
                .into_any_element(),
        )
    }

    /// The open menu, under its button.
    fn popover(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.open?;
        let at = self.anchors[menu as usize].get()?;
        let content = match menu {
            Menu::Highlight => self.colour_menu(true, palette, cx),
            Menu::FontColour => self.colour_menu(false, palette, cx),
            Menu::FontSize => self.size_menu(palette, cx),
            Menu::More => self.more_menu(palette, cx),
            Menu::Link => self.link_menu(palette, cx)?,
        };
        let panel = div()
            .id("format-menu")
            .debug_selector(|| "format-menu".to_owned())
            .p_2()
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(content);
        Some(
            deferred(
                anchored()
                    .position(at.bottom_left())
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().pt_1().child(panel)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}

/// The address a link is made to: as typed with a scheme the renderer opens (`http`, `https`, `mailto`), an address
/// with `@` and no scheme a `mailto:` one, any other `https://`; `None` when nothing was typed or it names another
/// scheme.
#[must_use]
pub fn link_address(typed: &str) -> Option<String> {
    if typed.is_empty() || typed.chars().any(char::is_whitespace) {
        return None;
    }
    if rich_text::opens(typed) {
        return Some(typed.to_owned());
    }
    if typed.contains("://") || typed.to_ascii_lowercase().starts_with("javascript:") {
        return None;
    }
    if typed.contains('@') && !typed.contains('/') {
        return Some(format!("mailto:{typed}"));
    }
    Some(format!("https://{typed}"))
}

impl Render for FormatBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let discard = self.discard.then(|| {
            let button = icon_button("format-discard", Glyph::TRASH, "Discard")
                .with_size(Size::Size(px(BUTTON)))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(FormatBarEvent::Discard)));
            selected_by("format-discard", button)
        });
        let read_only = self.editor.read(cx).read_only;
        div()
            .debug_selector(|| "format-bar".to_owned())
            .w_full()
            .flex()
            .items_center()
            .gap(px(2.0))
            .children(self.marks(cx))
            .child(Self::divider(palette))
            .children(self.lists(cx))
            .child(Self::divider(palette))
            .child(self.menu_button("format-highlight", Glyph::HIGHLIGHTER, "Highlight", Menu::Highlight, cx))
            .child(self.menu_button("format-colour", Glyph::FONT_COLOUR, "Font colour", Menu::FontColour, cx))
            .child(self.menu_button("format-size", Glyph::FONT_SIZE, "Font size", Menu::FontSize, cx))
            .child(Self::divider(palette))
            .children(self.inserts(cx))
            .child(self.menu_button("format-more", Glyph::MORE, "More options", Menu::More, cx))
            .child(div().flex_1())
            .children(discard)
            .children(self.popover(palette, cx))
            .when(read_only, |bar| bar.opacity(0.5))
    }
}

/// A disabled control of the box, with a tooltip saying why: a feature of Teams this app doesn't have yet.
#[must_use]
pub fn unavailable(id: &'static str, glyph: Glyph, tip: &'static str) -> AnyElement {
    let button = icon_button(id, glyph, tip)
        .with_size(Size::Size(px(BUTTON)))
        .disabled(true);
    selected_by(id, button).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_s_address_gets_a_scheme_the_renderer_opens() {
        assert_eq!(link_address("example.com/a").as_deref(), Some("https://example.com/a"));
        assert_eq!(
            link_address("http://example.com").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(link_address("me@example.com").as_deref(), Some("mailto:me@example.com"));
        assert_eq!(link_address("javascript:alert(1)"), None);
        assert_eq!(link_address("ftp://example.com"), None);
        assert_eq!(link_address(""), None);
    }
}
