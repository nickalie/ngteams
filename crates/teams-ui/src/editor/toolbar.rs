//! The editor's format bar, as Teams' message box shows it expanded: Bold, Italic, Underline, Strikethrough | bulleted
//! and numbered lists | highlight, font colour, font size | quote, link, code | More (paragraph styles, indent,
//! outdent, a horizontal line, a table, clear formatting, undo, redo), and Discard on the right. Each button acts on
//! the editor it was made for and gives it the focus back; a button whose format is on where the cursor is shows
//! selected. As Teams' bar does, a bar too narrow for all its tools keeps the first that fit and puts the others at the
//! top of its More options, each row doing what its button does; More options and Discard always stay on the bar.
//!
//! The colours are Teams' own, from its web client: the font colours its palette names (Red, Tuscan orange,
//! Sunglow, Pear, Eucalyptus, Faded jade, Fun blue, Tyrian purple) and the highlights (Cranberry, Pumpkin, Marigold,
//! Forest, Seafoam, Light teal, Blue, Lilac), each sent as the hex Teams writes; the sizes Small, Medium and Large.
//! A table isn't written by this editor: its entry shows, disabled, saying so.

use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
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
use crate::keyboard;
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

/// A tool of the bar that goes into its More options when the bar is too narrow, in the bar's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    Bullets,
    Numbers,
    Highlight,
    FontColour,
    FontSize,
    Quote,
    Link,
    Code,
}

/// The bar's tools, in order; More options and Discard follow them.
const TOOLS: [Tool; 12] = [
    Tool::Bold,
    Tool::Italic,
    Tool::Underline,
    Tool::Strikethrough,
    Tool::Bullets,
    Tool::Numbers,
    Tool::Highlight,
    Tool::FontColour,
    Tool::FontSize,
    Tool::Quote,
    Tool::Link,
    Tool::Code,
];

/// The gap between the bar's controls, px.
const GAP: f32 = 2.0;

impl Tool {
    /// Its button's selector; its row in More options has `-row` after it.
    fn id(self) -> &'static str {
        match self {
            Self::Bold => "format-bold",
            Self::Italic => "format-italic",
            Self::Underline => "format-underline",
            Self::Strikethrough => "format-strikethrough",
            Self::Bullets => "format-bullets",
            Self::Numbers => "format-numbers",
            Self::Highlight => "format-highlight",
            Self::FontColour => "format-colour",
            Self::FontSize => "format-size",
            Self::Quote => "format-quote",
            Self::Link => "format-link",
            Self::Code => "format-code",
        }
    }

    fn row_id(self) -> &'static str {
        match self {
            Self::Bold => "format-bold-row",
            Self::Italic => "format-italic-row",
            Self::Underline => "format-underline-row",
            Self::Strikethrough => "format-strikethrough-row",
            Self::Bullets => "format-bullets-row",
            Self::Numbers => "format-numbers-row",
            Self::Highlight => "format-highlight-row",
            Self::FontColour => "format-colour-row",
            Self::FontSize => "format-size-row",
            Self::Quote => "format-quote-row",
            Self::Link => "format-link-row",
            Self::Code => "format-code-row",
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Self::Bold => Glyph::BOLD,
            Self::Italic => Glyph::ITALIC,
            Self::Underline => Glyph::UNDERLINE,
            Self::Strikethrough => Glyph::STRIKETHROUGH,
            Self::Bullets => Glyph::LIST,
            Self::Numbers => Glyph::LIST_ORDERED,
            Self::Highlight => Glyph::HIGHLIGHTER,
            Self::FontColour => Glyph::FONT_COLOUR,
            Self::FontSize => Glyph::FONT_SIZE,
            Self::Quote => Glyph::QUOTE,
            Self::Link => Glyph::LINK,
            Self::Code => Glyph::CODE,
        }
    }

    /// Its name in its row of More options.
    fn label(self) -> &'static str {
        match self {
            Self::Bold => "Bold",
            Self::Italic => "Italic",
            Self::Underline => "Underline",
            Self::Strikethrough => "Strikethrough",
            Self::Bullets => "Bulleted list",
            Self::Numbers => "Numbered list",
            Self::Highlight => "Highlight",
            Self::FontColour => "Font colour",
            Self::FontSize => "Font size",
            Self::Quote => "Quote",
            Self::Link => "Insert link",
            Self::Code => "Code",
        }
    }

    /// Its button's tooltip: its name, and its key when it has one.
    fn tip(self) -> &'static str {
        match self {
            Self::Code => concat!("Code (", ctrl!(), "+Shift+B block)"),
            other => other.label(),
        }
    }

    /// The action of its key, which its tooltip names.
    fn action(self) -> Option<&'static dyn gpui::Action> {
        match self {
            Self::Bold => Some(&Bold),
            Self::Italic => Some(&Italic),
            Self::Underline => Some(&Underline),
            Self::Strikethrough => Some(&Strikethrough),
            Self::Bullets => Some(&BulletList),
            Self::Numbers => Some(&NumberedList),
            Self::Quote => Some(&Quote),
            _ => None,
        }
    }

    /// The menu it opens, if it opens one.
    fn menu(self) -> Option<Menu> {
        match self {
            Self::Highlight => Some(Menu::Highlight),
            Self::FontColour => Some(Menu::FontColour),
            Self::FontSize => Some(Menu::FontSize),
            Self::Link => Some(Menu::Link),
            _ => None,
        }
    }

    /// Its group on the bar: a divider stands between two groups.
    fn group(self) -> u8 {
        match self {
            Self::Bold | Self::Italic | Self::Underline | Self::Strikethrough => 0,
            Self::Bullets | Self::Numbers => 1,
            Self::Highlight | Self::FontColour | Self::FontSize => 2,
            Self::Quote | Self::Link | Self::Code => 3,
        }
    }

    /// Whether a divider stands before the tool at `index` of [`TOOLS`].
    fn after_divider(index: usize) -> bool {
        index > 0 && TOOLS[index - 1].group() != TOOLS[index].group()
    }
}

/// How many of [`TOOLS`] a bar last drawn `width` wide (`None`: not drawn yet, so all) keeps, at the rem size `rem`,
/// with or without Discard; the others go into its More options.
fn kept(width: Option<Pixels>, rem: Pixels, discard: bool) -> usize {
    let Some(width) = width.map(f32::from) else {
        return TOOLS.len();
    };
    let divider = 1.0 + 0.5 * f32::from(rem);
    // More options, the space that pushes Discard to the right, and Discard
    let mut used = BUTTON + GAP + if discard { GAP + BUTTON } else { 0.0 };
    let mut kept = 0;
    for index in 0..TOOLS.len() {
        if Tool::after_divider(index) {
            used += divider + GAP;
        }
        used += BUTTON + GAP;
        if used > width {
            break;
        }
        kept = index + 1;
    }
    kept
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
    /// Where the bar was last drawn: its width decides which tools it keeps.
    drawn_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// How many of [`TOOLS`] the bar kept as it was last drawn; the others are in its More options.
    kept: usize,
    /// Its tools and More options: one stop of Tab, as Teams' bar is; Discard is a stop of its own.
    row: keyboard::Row,
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
            drawn_at: Rc::default(),
            kept: TOOLS.len(),
            row: keyboard::Row::new(cx),
            _editor: observing,
        }
    }

    /// Its row of tools, which Tab passes as one stop.
    #[must_use]
    pub fn row(&self) -> &keyboard::Row {
        &self.row
    }

    /// It without Discard.
    #[must_use]
    pub fn without_discard(mut self) -> Self {
        self.discard = false;
        self
    }

    /// The selectors of the tools that the bar put into its More options as it was last drawn (tests, the tour).
    #[must_use]
    pub fn in_more_options(&self) -> Vec<&'static str> {
        TOOLS.iter().skip(self.kept).map(|tool| tool.id()).collect()
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

    /// Whether `tool` shows lit: its format is on where the cursor is, or its menu is open.
    fn on(&self, tool: Tool, cx: &App) -> bool {
        if let Some(menu) = tool.menu() {
            return self.open == Some(menu);
        }
        let editor = self.editor.read(cx);
        let kind = editor.block_kind();
        match tool {
            Tool::Bold => editor.has_mark(Marks::BOLD),
            Tool::Italic => editor.has_mark(Marks::ITALIC),
            Tool::Underline => editor.has_mark(Marks::UNDERLINE),
            Tool::Strikethrough => editor.has_mark(Marks::STRIKE),
            Tool::Bullets => kind == Kind::Bullet,
            Tool::Numbers => kind == Kind::Numbered,
            Tool::Quote => kind == Kind::Quote,
            Tool::Code => kind == Kind::Code || editor.has_mark(Marks::CODE),
            Tool::Highlight | Tool::FontColour | Tool::FontSize | Tool::Link => false,
        }
    }

    /// Does what `tool`'s button does: opens its menu, or acts on the editor.
    fn press(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(menu) = tool.menu() {
            return self.toggle(menu, window, cx);
        }
        self.act(window, cx, |editor, cx| match tool {
            Tool::Bold => editor.toggle_mark(Marks::BOLD, cx),
            Tool::Italic => editor.toggle_mark(Marks::ITALIC, cx),
            Tool::Underline => editor.toggle_mark(Marks::UNDERLINE, cx),
            Tool::Strikethrough => editor.toggle_mark(Marks::STRIKE, cx),
            Tool::Bullets => editor.toggle_kind(Kind::Bullet, cx),
            Tool::Numbers => editor.toggle_kind(Kind::Numbered, cx),
            Tool::Quote => editor.toggle_kind(Kind::Quote, cx),
            Tool::Code => editor.code(cx),
            Tool::Highlight | Tool::FontColour | Tool::FontSize | Tool::Link => {}
        });
    }

    /// `tool`'s button on the bar: its tooltip names its key, it is selected while it is on, and a menu opens under it.
    fn tool_button(&self, tool: Tool, cx: &mut Context<Self>) -> AnyElement {
        let button = match tool.action() {
            Some(action) => icon_button_with_key(tool.id(), tool.glyph(), tool.tip(), action),
            None => icon_button(tool.id(), tool.glyph(), tool.tip()),
        }
        .with_size(Size::Size(px(BUTTON)))
        .selected(self.on(tool, cx))
        .on_click(cx.listener(move |this, _, window, cx| this.press(tool, window, cx)));
        let button = selected_by(tool.id(), button);
        match tool.menu() {
            Some(menu) => button
                .relative()
                .child(measured(&self.anchors[menu as usize]))
                .into_any_element(),
            None => button.into_any_element(),
        }
    }

    /// More options' button; its menu opens under it, as do the menus of the tools in it.
    fn more_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let button = icon_button("format-more", Glyph::MORE, "More options")
            .with_size(Size::Size(px(BUTTON)))
            .selected(self.open == Some(Menu::More))
            .on_click(cx.listener(|this, _, window, cx| this.toggle(Menu::More, window, cx)));
        selected_by("format-more", button)
            .relative()
            .child(measured(&self.anchors[Menu::More as usize]))
            .into_any_element()
    }

    /// The tools the bar keeps, with a divider between two groups.
    fn kept_tools(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut row = Vec::new();
        for (index, tool) in TOOLS.iter().enumerate().take(self.kept) {
            if Tool::after_divider(index) {
                row.push(Self::divider(palette));
            }
            row.push(self.tool_button(*tool, cx));
        }
        row
    }

    /// The tools in More options, each a row that does what its button does, a check on the rows that are on.
    fn overflow_rows(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Stateful<gpui::Div>> {
        TOOLS
            .iter()
            .skip(self.kept)
            .map(|tool| {
                let tool = *tool;
                Self::row_face(
                    tool.row_id(),
                    Some(tool.glyph()),
                    tool.label(),
                    self.on(tool, cx),
                    palette,
                )
                .on_click(cx.listener(move |this, _, window, cx| this.press(tool, window, cx)))
            })
            .collect()
    }

    /// Where the menu of `menu` opens: under its button, or under More options while its tool is in that menu.
    fn anchor_of(&self, menu: Menu) -> Option<Bounds<Pixels>> {
        let in_more = TOOLS.iter().skip(self.kept).any(|tool| tool.menu() == Some(menu));
        let anchor = if in_more { Menu::More } else { menu };
        self.anchors[anchor as usize].get()
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

    /// A row of a menu that acts on the editor: an icon, a label, a check when chosen.
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
        Self::row_face(id, glyph, label, chosen, palette)
            .on_click(cx.listener(move |this, _, window, cx| this.act(window, cx, &choose)))
    }

    /// A menu row's face: an icon, a label and a check when chosen.
    fn row_face(
        id: &'static str,
        glyph: Option<Glyph>,
        label: &'static str,
        chosen: bool,
        palette: Palette,
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
        let overflow = self.overflow_rows(palette, cx);
        let overflow_separator = (!overflow.is_empty()).then(separator);
        div()
            .w(px(220.0))
            .flex()
            .flex_col()
            .children(overflow)
            .children(overflow_separator)
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
        let at = self.anchor_of(menu)?;
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.row.note(window, cx);
        self.kept = kept(
            self.drawn_at.get().map(|bounds| bounds.size.width),
            window.rem_size(),
            self.discard,
        );
        let discard = self.discard.then(|| {
            let button = icon_button("format-discard", Glyph::TRASH, "Discard")
                .with_size(Size::Size(px(BUTTON)))
                .on_click(cx.listener(|_, _, _, cx| cx.emit(FormatBarEvent::Discard)));
            selected_by("format-discard", button)
        });
        let read_only = self.editor.read(cx).read_only;
        div()
            .debug_selector(|| "format-bar".to_owned())
            .relative()
            .w_full()
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(
                self.row
                    .wrap(div().flex().items_center().gap(px(GAP)))
                    .children(self.kept_tools(palette, cx))
                    .child(self.more_button(cx)),
            )
            .child(div().flex_1())
            .children(discard.map(|discard| self.row.after(div().flex()).child(discard)))
            .child(measured(&self.drawn_at))
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

    fn rem() -> Pixels {
        px(f32::from(theme::FONT_SIZE))
    }

    #[test]
    fn a_bar_not_drawn_yet_or_wide_keeps_every_tool() {
        assert_eq!(kept(None, rem(), true), TOOLS.len());
        assert_eq!(kept(Some(px(800.0)), rem(), true), TOOLS.len());
    }

    #[test]
    fn a_bar_in_a_call_s_chat_pane_keeps_the_first_tools_that_fit() {
        // the expanded box in the 360 px Chat pane gives its bar 302 px
        assert_eq!(kept(Some(px(302.0)), rem(), true), 7);
        assert_eq!(TOOLS[6], Tool::Highlight);
        // without Discard, one more fits
        assert_eq!(kept(Some(px(302.0)), rem(), false), 8);
    }

    #[test]
    fn a_bar_with_no_room_keeps_no_tool() {
        assert_eq!(kept(Some(px(40.0)), rem(), true), 0);
    }

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
