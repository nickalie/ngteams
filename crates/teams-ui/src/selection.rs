//! Selecting a message's text with the mouse and copying it, as Teams allows: drag over the text (across its runs,
//! lines and blocks, and on into the messages above or below), double-click a word, triple-click a paragraph (a line
//! of code), Shift+click to extend; Ctrl+C or the right-click menu's "Copy" puts it on the clipboard as plain text.
//!
//! - **One selection per view** ([`TextSelection`]): a chat's or a channel's. Its ends are [`Spot`]s, a place in one
//!   unit of text: the message ([`MessageKey`], ordered as the view shows its messages), the unit's path among the
//!   message's blocks (as `rich_text` draws them, a paragraph's or a line of code's) and a byte offset in the unit's
//!   drawn text. Everything between the two ends, in that order, is selected.
//! - **Drawn by our own text element** ([`SelectableText`]): the unit's styled text (`rich_text`'s highlights) with
//!   the selected range on Teams' selection colour ([`colour`]), an I-beam over text and a pointing hand over a link.
//!   A press on it starts or extends the selection, a drag extends it into every unit the pointer passes level with,
//!   and a click that selected nothing on a link opens the link (through `open_url`, only an `http`, `https` or
//!   `mailto` one). The view clears the selection on a press anywhere else in it.
//! - **Copied from the model, not the screen** ([`TextSelection::text`]): the view hands every message it holds, and
//!   the selected units' text is read from their laid-out blocks (`rich_text::units`), an emoticon as its emoji, a
//!   unit a line, a blank line between messages. So a selection reaching a message scrolled out of the list still
//!   copies whole.
//! - **The right-click menu** ([`menu`]): over a selection, "Copy" with its key, as Teams' own menu over selected text.
//! - **A link's right-click menu** ([`open_link_menu`]): a right-click on a link opens, at the pointer, "Copy link"
//!   (its `href` on the clipboard, not the text shown) and "Open link" (as a click opens it, only an `http`, `https` or
//!   `mailto` one), whether or not the text is selectable (a card's text too). One is open at a time, app-wide; the
//!   unit it was opened on draws it, over everything, and a press outside it or a choice closes it.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ClipboardItem, CursorStyle, DispatchPhase, Div, Element, ElementId,
    Global, GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, Hsla, InspectorElementId, InteractiveElement as _,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels,
    Point, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, StyledText, TextLayout, Window,
    WindowId, anchored, deferred, div, point, px, rgb,
};
use gpui_component::kbd::Kbd;

use crate::icons::Glyph;
use crate::picture_viewer::OpenPicture;
use crate::rich_text::{self, LinkRange, Rendered};
use crate::theme::Palette;

/// The right-click menu's entry.
pub const COPY: &str = "Copy";

/// A link's right-click menu's entry that puts the link on the clipboard.
pub const COPY_LINK: &str = "Copy link";

/// A link's right-click menu's entry that opens the link.
pub const OPEN_LINK: &str = "Open link";

/// How strongly the brand colour tints selected text's background (readable on every theme's bubbles, a selected
/// bubble's too).
const SELECTION_ALPHA: f32 = 0.35;

/// The right-click menu's width, px.
const MENU_WIDTH: f32 = 160.0;

/// Selected text's background: the brand colour, tinted.
#[must_use]
pub fn colour(palette: Palette) -> Hsla {
    rich_text::tint(palette.brand, SELECTION_ALPHA)
}

/// Where a message stands among a view's messages: ordered as the view shows them (`order`, then its id).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageKey {
    pub order: (i64, i64),
    pub id: SharedString,
}

impl MessageKey {
    #[must_use]
    pub fn new(order: (i64, i64), id: impl Into<SharedString>) -> Self {
        Self { order, id: id.into() }
    }
}

/// A place in a message's text: its message, the unit's path among the message's blocks, and a byte offset in the
/// unit's drawn text. Ordered as read.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Spot {
    pub message: MessageKey,
    pub path: Vec<usize>,
    pub offset: usize,
}

/// How a drag extends the selection: by character, by word (after a double-click) or by unit (after a triple-click).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granularity {
    Char,
    Word,
    Unit,
}

#[derive(Debug, Default)]
struct State {
    anchor: Option<Spot>,
    head: Option<Spot>,
    /// While the left button drags: how it extends, and the range the press selected.
    drag: Option<(Granularity, Spot, Spot)>,
    /// Where the right-click menu is open, in the window.
    menu: Option<Point<Pixels>>,
}

/// A view's text selection (see the module docs). Cloned, it is the same selection.
#[derive(Clone, Debug, Default)]
pub struct TextSelection(Rc<RefCell<State>>);

impl TextSelection {
    /// The selected range, in reading order, when it holds anything.
    #[must_use]
    pub fn range(&self) -> Option<(Spot, Spot)> {
        let state = self.0.borrow();
        let (anchor, head) = (state.anchor.clone()?, state.head.clone()?);
        match anchor.cmp(&head) {
            std::cmp::Ordering::Less => Some((anchor, head)),
            std::cmp::Ordering::Greater => Some((head, anchor)),
            std::cmp::Ordering::Equal => None,
        }
    }

    /// Whether nothing is selected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.range().is_none()
    }

    /// Selects from `anchor` to `head`.
    pub fn select(&self, anchor: Spot, head: Spot) {
        let mut state = self.0.borrow_mut();
        state.anchor = Some(anchor);
        state.head = Some(head);
        state.drag = None;
    }

    /// Selects nothing (and closes the menu); whether anything was selected or open.
    #[must_use]
    pub fn clear(&self) -> bool {
        let mut state = self.0.borrow_mut();
        let had = state.anchor.is_some() || state.menu.is_some();
        *state = State::default();
        had
    }

    /// Where the right-click menu is open, while it is and something is selected.
    #[must_use]
    pub fn menu(&self) -> Option<Point<Pixels>> {
        let at = self.0.borrow().menu?;
        (!self.is_empty()).then_some(at)
    }

    /// Closes the right-click menu.
    pub fn close_menu(&self) {
        self.0.borrow_mut().menu = None;
    }

    /// The selected part of a unit of `len` bytes at `path` in `message`, when any of it is selected.
    #[must_use]
    pub fn range_in(&self, message: &MessageKey, path: &[usize], len: usize) -> Option<Range<usize>> {
        let (start, end) = self.range()?;
        let unit = (message, path);
        if unit < (&start.message, start.path.as_slice()) || unit > (&end.message, end.path.as_slice()) {
            return None;
        }
        let from = if unit == (&start.message, start.path.as_slice()) {
            start.offset.min(len)
        } else {
            0
        };
        let to = if unit == (&end.message, end.path.as_slice()) {
            end.offset.min(len)
        } else {
            len
        };
        (from < to).then_some(from..to)
    }

    /// The selected text as plain text, from `messages` (every message the view holds, each with its laid-out text,
    /// in any order): each unit's selected part a line, a blank line between messages. `None` when nothing is
    /// selected.
    pub fn text<'r>(&self, messages: impl IntoIterator<Item = (MessageKey, &'r Rendered)>) -> Option<String> {
        let (start, end) = self.range()?;
        let mut messages: Vec<(MessageKey, &Rendered)> = messages
            .into_iter()
            .filter(|(key, _)| *key >= start.message && *key <= end.message)
            .collect();
        messages.sort_by(|a, b| a.0.cmp(&b.0));
        let parts: Vec<String> = messages
            .into_iter()
            .filter_map(|(key, rendered)| self.message_text(&(start.clone(), end.clone()), &key, rendered))
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    /// The selected part of one message, between `ends` (the selection's, in reading order): each unit's a line.
    fn message_text(&self, ends: &(Spot, Spot), key: &MessageKey, rendered: &Rendered) -> Option<String> {
        let (start, end) = ends;
        let lines: Vec<String> = rich_text::units(rendered)
            .iter()
            .filter_map(|unit| {
                let here = (key, unit.path.as_slice());
                // an empty unit inside the selection (a blank line) keeps its line
                let blank = unit.is_empty()
                    && here > (&start.message, start.path.as_slice())
                    && here < (&end.message, end.path.as_slice());
                let range = self.range_in(key, &unit.path, unit.len()).or(blank.then_some(0..0))?;
                Some(unit.copied(range))
            })
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    /// A press at `spot` in a unit whose drawn text is `text`: a single click starts a selection there (or, with
    /// Shift, extends the one there is), a double-click selects the word, a triple-click the unit.
    fn press(&self, spot: Spot, text: &str, click_count: usize, shift: bool) {
        let mut state = self.0.borrow_mut();
        state.menu = None;
        let (granularity, from, to) = match click_count {
            0 | 1 => (Granularity::Char, spot.clone(), spot.clone()),
            2 => {
                let word = word_at(text, spot.offset);
                (Granularity::Word, at(&spot, word.start), at(&spot, word.end))
            }
            _ => (Granularity::Unit, at(&spot, 0), at(&spot, text.len())),
        };
        if shift && click_count <= 1 && state.anchor.is_some() {
            let anchor = state.anchor.clone().unwrap_or_else(|| spot.clone());
            state.head = Some(spot);
            state.drag = Some((Granularity::Char, anchor.clone(), anchor));
            return;
        }
        state.anchor = Some(from.clone());
        state.head = Some(to.clone());
        state.drag = Some((granularity, from, to));
    }

    /// The pointer dragged to `spot` in a unit whose drawn text is `text`: the selection's moving end follows it, by
    /// the press's granularity. Whether the selection changed.
    fn drag_to(&self, spot: Spot, text: &str) -> bool {
        let mut state = self.0.borrow_mut();
        let Some((granularity, from, to)) = state.drag.clone() else {
            return false;
        };
        let (start, end) = match granularity {
            Granularity::Char => (spot.clone(), spot),
            Granularity::Word => {
                let word = word_at(text, spot.offset);
                (at(&spot, word.start), at(&spot, word.end))
            }
            Granularity::Unit => (at(&spot, 0), at(&spot, text.len())),
        };
        let (anchor, head) = if start < from { (to, start) } else { (from, end) };
        let changed = state.anchor.as_ref() != Some(&anchor) || state.head.as_ref() != Some(&head);
        state.anchor = Some(anchor);
        state.head = Some(head);
        changed
    }

    /// The left button went up: the drag is over.
    fn release(&self) {
        self.0.borrow_mut().drag = None;
    }

    fn dragging(&self) -> bool {
        self.0.borrow().drag.is_some()
    }

    /// Opens the right-click menu at `position` in the window (shown while something is selected).
    pub fn open_menu(&self, position: Point<Pixels>) {
        self.0.borrow_mut().menu = Some(position);
    }
}

/// `spot`'s unit at `offset`.
fn at(spot: &Spot, offset: usize) -> Spot {
    Spot { offset, ..spot.clone() }
}

/// What a character is, for a word's ends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Other
    }
}

/// The word around `offset` in `text`, as a double-click selects it: a run of letters and digits, a run of spaces, or
/// one other character (the one after `offset`, else the one before at the end of the text).
#[must_use]
pub fn word_at(text: &str, offset: usize) -> Range<usize> {
    let offset = text.floor_char_boundary(offset);
    let after = text[offset..].chars().next();
    let (start, c) = match after {
        Some(c) => (offset, c),
        None => match text[..offset].char_indices().next_back() {
            Some((start, c)) => (start, c),
            None => return offset..offset,
        },
    };
    let kind = class(c);
    if kind == CharClass::Other {
        return start..start + c.len_utf8();
    }
    let from = text[..start]
        .char_indices()
        .rev()
        .take_while(|(_, c)| class(*c) == kind)
        .last()
        .map_or(start, |(at, _)| at);
    let to = text[start..]
        .char_indices()
        .find(|(_, c)| class(*c) != kind)
        .map_or(text.len(), |(at, _)| start + at);
    from..to
}

/// `highlights` (in order, not overlapping) with `selected` drawn on `background` (split where they cross it).
#[must_use]
pub fn with_selection(
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    selected: Option<Range<usize>>,
    background: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(selected) = selected else {
        return highlights;
    };
    let mut out = Vec::with_capacity(highlights.len() + 2);
    let mut push = |range: Range<usize>, style: HighlightStyle| {
        let cuts = [
            range.start,
            selected.start.clamp(range.start, range.end),
            selected.end.clamp(range.start, range.end),
            range.end,
        ];
        for piece in cuts.windows(2) {
            let (from, to) = (piece[0], piece[1]);
            if from >= to {
                continue;
            }
            let mut style = style;
            if from >= selected.start && to <= selected.end {
                style.background_color = Some(background);
            }
            if style != HighlightStyle::default() {
                out.push((from..to, style));
            }
        }
    };
    let mut covered = 0;
    for (range, style) in highlights {
        if covered < range.start {
            push(covered..range.start, HighlightStyle::default());
        }
        covered = range.end;
        push(range, style);
    }
    if covered < selected.end {
        push(covered..selected.end, HighlightStyle::default());
    }
    out
}

/// The character boundary in `layout` nearest `position` (the start of the text above it, the end below it), on the
/// rows where its alignment draws them.
pub(crate) fn closest_index(layout: &TextLayout, position: Point<Pixels>) -> usize {
    let (Ok(index) | Err(index)) = layout.closest_index_for_position(position);
    index
}

/// Where a selectable unit is: its selection, its message and its path among the message's blocks.
#[derive(Clone, Debug)]
pub struct Place {
    pub selection: TextSelection,
    pub message: MessageKey,
    pub path: Vec<usize>,
}

impl Place {
    /// The unit at `index` under this one (a line of a block of code).
    #[must_use]
    pub fn child(&self, index: usize) -> Self {
        let mut path = self.path.clone();
        path.push(index);
        Self { path, ..self.clone() }
    }

    fn spot(&self, offset: usize) -> Spot {
        Spot {
            message: self.message.clone(),
            path: self.path.clone(),
            offset,
        }
    }
}

/// A message's units' selection: the view's selection and the message's key, for `rich_text` to place each unit;
/// what a click on one of the message's pictures opens (the picture viewer), when the view opens them; and where a
/// click on a reply's quote goes (the quoted message, in the view), when the view goes there.
#[derive(Clone, Debug)]
pub struct Scope {
    pub selection: TextSelection,
    pub message: MessageKey,
    pub open: Option<OpenPicture>,
    pub quoted: Option<rich_text::OpenQuoted>,
}

impl Scope {
    #[must_use]
    pub fn new(selection: &TextSelection, message: MessageKey) -> Self {
        Self {
            selection: selection.clone(),
            message,
            open: None,
            quoted: None,
        }
    }

    /// It, a click on one of the message's pictures calling `open` with the picture's `src`.
    #[must_use]
    pub fn opening(mut self, open: OpenPicture) -> Self {
        self.open = Some(open);
        self
    }

    /// It, a click on one of the message's reply quotes calling `quoted` with the quoted message.
    #[must_use]
    pub fn quoting(mut self, quoted: rich_text::OpenQuoted) -> Self {
        self.quoted = Some(quoted);
        self
    }

    /// The unit at `path`.
    #[must_use]
    pub fn at(&self, path: &[usize]) -> Place {
        Place {
            selection: self.selection.clone(),
            message: self.message.clone(),
            path: path.to_vec(),
        }
    }
}

/// A unit of styled text, selectable when it has a [`Place`], its links clickable (see the module docs).
pub struct SelectableText {
    id: ElementId,
    content: SharedString,
    text: StyledText,
    links: Vec<LinkRange>,
    place: Option<Place>,
}

impl SelectableText {
    /// `content` drawn with `highlights` (in order, not overlapping), its selected part (when `place` has one) on
    /// `background`; `links` open on a click.
    #[must_use]
    pub fn new(
        id: impl Into<ElementId>,
        content: SharedString,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        links: Vec<LinkRange>,
        place: Option<Place>,
        background: Hsla,
    ) -> Self {
        // a selection made before the text changed (an edit came) may fall inside a character
        let selected = place
            .as_ref()
            .and_then(|place| place.selection.range_in(&place.message, &place.path, content.len()))
            .map(|range| content.floor_char_boundary(range.start)..content.floor_char_boundary(range.end))
            .filter(|range| !range.is_empty());
        let text = StyledText::new(content.clone()).with_highlights(with_selection(highlights, selected, background));
        Self {
            id: id.into(),
            content,
            text,
            links,
            place,
        }
    }

    /// The text's layout, once laid out: where each index is drawn.
    #[must_use]
    pub fn layout(&self) -> &TextLayout {
        self.text.layout()
    }

    fn link_at(links: &[LinkRange], index: usize) -> Option<&LinkRange> {
        links.iter().find(|link| link.range.contains(&index))
    }

    /// The cursor over it: a pointing hand over a link, an I-beam over selectable text.
    fn cursor(&self, hitbox: &Hitbox, window: &mut Window) {
        if !hitbox.is_hovered(window) {
            return;
        }
        let layout = self.text.layout();
        let on_link = layout
            .index_for_position(window.mouse_position())
            .is_ok_and(|index| Self::link_at(&self.links, index).is_some());
        if on_link {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        } else if self.place.is_some() {
            window.set_cursor_style(CursorStyle::IBeam, hitbox);
        }
    }

    /// A press on it: a link's press is kept for its click; a selectable unit starts or extends the selection, or
    /// opens the right-click menu over one; a right-click on a link opens its menu (on this unit, `owner`).
    fn on_press(
        &self,
        hitbox: &Hitbox,
        pressed: &Rc<Cell<Option<usize>>>,
        owner: Option<GlobalElementId>,
        window: &mut Window,
    ) {
        let layout = self.text.layout().clone();
        let content = self.content.clone();
        let links = self.links.clone();
        let place = self.place.clone();
        let hitbox = hitbox.clone();
        let pressed = Rc::clone(pressed);
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            let link = layout
                .index_for_position(event.position)
                .ok()
                .and_then(|index| Self::link_at(&links, index));
            if event.button == MouseButton::Right
                && let Some(link) = link
            {
                if let Some(place) = &place {
                    place.selection.close_menu();
                }
                set_link_menu(
                    LinkMenu {
                        window: window.window_handle().window_id(),
                        owner: owner.clone(),
                        at: Some(event.position),
                        href: link.href.clone(),
                    },
                    cx,
                );
                window.refresh();
                return;
            }
            match event.button {
                MouseButton::Left => {
                    pressed.set(layout.index_for_position(event.position).ok());
                    if let Some(place) = &place {
                        let offset = closest_index(&layout, event.position);
                        place
                            .selection
                            .press(place.spot(offset), &content, event.click_count, event.modifiers.shift);
                        window.refresh();
                    }
                }
                MouseButton::Right => {
                    if let Some(place) = place.as_ref().filter(|place| !place.selection.is_empty()) {
                        place.selection.open_menu(event.position);
                        window.refresh();
                    }
                }
                _ => {}
            }
        });
    }

    /// A drag with the left button: the selection's moving end follows the pointer while it is level with this unit
    /// (wherever it is across).
    fn on_drag(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let Some(place) = self.place.clone() else {
            return;
        };
        let layout = self.text.layout().clone();
        let content = self.content.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
            let level = event.position.y >= bounds.top() && event.position.y < bounds.bottom();
            if phase != DispatchPhase::Bubble
                || event.pressed_button != Some(MouseButton::Left)
                || !level
                || !place.selection.dragging()
            {
                return;
            }
            let offset = closest_index(&layout, event.position);
            if place.selection.drag_to(place.spot(offset), &content) {
                window.refresh();
            }
        });
    }

    /// The left button up: the drag ends; a click that selected nothing on a link (pressed and let go on it) opens it.
    fn on_release(&self, hitbox: &Hitbox, pressed: &Rc<Cell<Option<usize>>>, window: &mut Window) {
        let layout = self.text.layout().clone();
        let links = self.links.clone();
        let place = self.place.clone();
        let hitbox = hitbox.clone();
        let pressed = Rc::clone(pressed);
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if let Some(place) = &place {
                place.selection.release();
            }
            let Some(down) = pressed.take() else { return };
            let selected = place.as_ref().is_some_and(|place| !place.selection.is_empty());
            if selected || !hitbox.is_hovered(window) {
                return;
            }
            let Ok(up) = layout.index_for_position(event.position) else {
                return;
            };
            if let Some(link) = Self::link_at(&links, down).filter(|link| link.range.contains(&up))
                && rich_text::opens(&link.href)
            {
                cx.open_url(&link.href);
            }
        });
    }
}

impl SelectableText {
    /// The link menu open on this unit (`id`) in this window, drawn over everything at the pointer it was opened at;
    /// one opened for a link without a unit ([`open_link_menu`]) is taken by the first unit drawn that holds the link,
    /// under the link's first letter.
    fn link_menu(&self, id: Option<&GlobalElementId>, window: &mut Window, cx: &mut App) {
        let Some(id) = id else { return };
        let Some(mut open) = cx.try_global::<LinkMenus>().and_then(|menus| menus.0.clone()) else {
            return;
        };
        if open.window != window.window_handle().window_id() {
            return;
        }
        match &open.owner {
            Some(owner) if owner == id => {}
            None => {
                let layout = self.text.layout();
                let Some(under) = self
                    .links
                    .iter()
                    .find(|link| link.href == open.href)
                    .and_then(|link| layout.position_for_index(link.range.start))
                else {
                    return;
                };
                open.owner = Some(id.clone());
                open.at = open.at.or(Some(under + point(px(0.0), layout.line_height())));
                set_link_menu(open.clone(), cx);
            }
            Some(_) => return,
        }
        let Some(at) = open.at else { return };
        let menu = link_menu_element(open.href, crate::theme::current(cx));
        let mut menu = anchored()
            .position(at)
            .snap_to_window_with_margin(px(8.0))
            .child(menu)
            .into_any_element();
        // laid out on its own (it is no child of the tree's layout), then drawn over everything once the tree is
        menu.layout_as_root(AvailableSpace::min_size(), window, cx);
        window.defer_draw(menu, window.element_offset(), MENU_PRIORITY, None);
    }
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(None, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.text.prepaint(None, inspector_id, bounds, state, window, cx);
        self.link_menu(id, window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        // the index a press landed on, kept across frames for its release
        let pressed = match id {
            Some(id) => window.with_element_state::<Rc<Cell<Option<usize>>>, _>(id, |state, _| {
                let state = state.unwrap_or_default();
                (Rc::clone(&state), state)
            }),
            None => Rc::default(),
        };
        self.cursor(hitbox, window);
        self.on_press(hitbox, &pressed, id.cloned(), window);
        self.on_drag(bounds, window);
        self.on_release(hitbox, &pressed, window);
        self.text
            .paint(None, inspector_id, bounds, &mut (), &mut (), window, cx);
    }
}

/// The right-click menu over a selection, at the pointer, while it is open: "Copy" (with `key`, the view's binding),
/// which runs `copy` and closes it; a press outside closes it.
pub fn menu(
    selection: &TextSelection,
    palette: Palette,
    key: Option<Kbd>,
    copy: impl Fn(&mut Window, &mut App) + 'static,
) -> Option<AnyElement> {
    let at = selection.menu()?;
    let closing = selection.clone();
    let copying = selection.clone();
    let entry = menu_entry("selection-copy", Glyph::COPY, COPY, palette)
        .on_click(move |_, window, cx| {
            copy(window, cx);
            copying.close_menu();
            window.refresh();
        })
        .children(key);
    let menu = menu_surface("selection-menu", palette)
        .on_mouse_down_out(move |_, window, _| {
            closing.close_menu();
            window.refresh();
        })
        .child(entry);
    Some(
        deferred(anchored().position(at).snap_to_window_with_margin(px(8.0)).child(menu))
            .with_priority(MENU_PRIORITY)
            .into_any_element(),
    )
}

/// Where a right-click menu is drawn among the deferred elements: over the view's popups.
const MENU_PRIORITY: usize = 2;

/// A right-click menu's surface: raised, bordered, rounded, its entries in a column.
fn menu_surface(id: &'static str, palette: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w(px(MENU_WIDTH))
        .p_1()
        .flex()
        .flex_col()
        .bg(rgb(palette.surface))
        .border_1()
        .border_color(rgb(palette.border))
        .rounded_lg()
        .shadow_lg()
        .occlude()
}

/// A right-click menu's entry: its icon and its label, the hover's colour under it.
fn menu_entry(id: &'static str, glyph: Glyph, label: &'static str, palette: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
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
        .hover(move |row| row.bg(rgb(palette.hover)))
        .child(glyph.icon().size(px(14.0)))
        .child(div().flex_1().child(label))
}

/// The open link menu, app-wide: one right-click menu is open at a time.
#[derive(Default)]
struct LinkMenus(Option<LinkMenu>);

impl Global for LinkMenus {}

/// A link menu: the window it is in, the unit it was opened on (none until one holding the link is drawn), where it
/// is drawn (where the pointer was; none: under the link) and the link's `href`.
#[derive(Clone, Debug)]
struct LinkMenu {
    window: WindowId,
    owner: Option<GlobalElementId>,
    at: Option<Point<Pixels>>,
    href: String,
}

fn set_link_menu(menu: LinkMenu, cx: &mut App) {
    cx.default_global::<LinkMenus>().0 = Some(menu);
}

/// Opens the right-click menu of the link to `href` in `window`, as a right-click on it would, under the link in the
/// first text drawn that holds it.
pub fn open_link_menu(href: &str, window: &mut Window, cx: &mut App) {
    set_link_menu(
        LinkMenu {
            window: window.window_handle().window_id(),
            owner: None,
            at: None,
            href: href.to_owned(),
        },
        cx,
    );
    window.refresh();
}

/// The `href` of the link whose right-click menu is open in `window`, while one is.
#[must_use]
pub fn link_menu(window: &Window, cx: &App) -> Option<String> {
    cx.try_global::<LinkMenus>()
        .and_then(|menus| menus.0.as_ref())
        .filter(|menu| menu.window == window.window_handle().window_id())
        .map(|menu| menu.href.clone())
}

/// Closes the link menu, wherever it is open.
pub fn close_link_menu(cx: &mut App) {
    if cx.try_global::<LinkMenus>().is_some_and(|menus| menus.0.is_some()) {
        cx.global_mut::<LinkMenus>().0 = None;
    }
}

/// A link's right-click menu: "Copy link" puts `href` on the clipboard, "Open link" opens it (only an `http`, `https`
/// or `mailto` one, as a click does); either closes the menu, as a press outside it does.
fn link_menu_element(href: String, palette: Palette) -> AnyElement {
    let copied = href.clone();
    let copy = menu_entry("link-menu-copy", Glyph::LINK, COPY_LINK, palette).on_click(move |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
        close_link_menu(cx);
        window.refresh();
    });
    let open = rich_text::opens(&href).then(|| {
        menu_entry("link-menu-open", Glyph::OPEN_OUT, OPEN_LINK, palette).on_click(move |_, window, cx| {
            cx.open_url(&href);
            close_link_menu(cx);
            window.refresh();
        })
    });
    menu_surface("link-menu", palette)
        .on_mouse_down_out(|_, window, cx| {
            close_link_menu(cx);
            window.refresh();
        })
        .child(copy)
        .children(open)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(order: i64, path: &[usize], offset: usize) -> Spot {
        Spot {
            message: MessageKey::new((order, 0), format!("m{order}")),
            path: path.to_vec(),
            offset,
        }
    }

    #[test]
    fn a_word_is_letters_and_digits_spaces_or_one_other_character() {
        let text = "Hello, wide world_1!";
        assert_eq!(word_at(text, 2), 0..5);
        assert_eq!(word_at(text, 5), 5..6);
        assert_eq!(word_at(text, 6), 6..7);
        assert_eq!(word_at(text, 14), 12..19);
        assert_eq!(word_at(text, text.len()), 19..20);
        assert_eq!(word_at("", 0), 0..0);
        assert_eq!(word_at("привет мир", 3), 0..12);
    }

    #[test]
    fn a_unit_between_the_ends_is_selected_whole() {
        let selection = TextSelection::default();
        selection.select(spot(2, &[1], 4), spot(1, &[0], 3));
        let first = MessageKey::new((1, 0), "m1");
        let second = MessageKey::new((2, 0), "m2");
        assert_eq!(selection.range_in(&first, &[0], 10), Some(3..10));
        assert_eq!(selection.range_in(&first, &[1, 0], 6), Some(0..6));
        assert_eq!(selection.range_in(&second, &[0], 8), Some(0..8));
        assert_eq!(selection.range_in(&second, &[1], 8), Some(0..4));
        assert_eq!(selection.range_in(&second, &[2], 8), None);
        assert!(!selection.is_empty());
        let _ = selection.clear();
        assert!(selection.is_empty());
    }

    #[test]
    fn the_selection_splits_the_highlights_it_crosses() {
        let bold = HighlightStyle {
            font_weight: Some(gpui::FontWeight::BOLD),
            ..HighlightStyle::default()
        };
        let background = Hsla::default();
        let split = with_selection(vec![(2..6, bold)], Some(4..8), background);
        let selected = HighlightStyle {
            background_color: Some(background),
            ..HighlightStyle::default()
        };
        let both = HighlightStyle {
            background_color: Some(background),
            ..bold
        };
        assert_eq!(split, vec![(2..4, bold), (4..6, both), (6..8, selected)]);
    }

    #[test]
    fn a_drag_after_a_double_click_extends_by_words() {
        let selection = TextSelection::default();
        let text = "one two three";
        selection.press(spot(1, &[0], 5), text, 2, false);
        assert_eq!(selection.range(), Some((spot(1, &[0], 4), spot(1, &[0], 7))));
        assert!(selection.drag_to(spot(1, &[0], 10), text));
        assert_eq!(selection.range(), Some((spot(1, &[0], 4), spot(1, &[0], 13))));
        assert!(selection.drag_to(spot(1, &[0], 1), text));
        assert_eq!(selection.range(), Some((spot(1, &[0], 0), spot(1, &[0], 7))));
        selection.release();
        assert!(!selection.drag_to(spot(1, &[0], 12), text));
    }
}
