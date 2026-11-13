//! How the editor is drawn: each block a row of our own text element (`BlockText`: GPUI's styled text, the
//! renderer's highlights, the selection and the cursor), a list item after its marker, a quote line beside its bar, a
//! block of code on its card, a rule a hairline; and where on screen a place is, from the last frame's layouts.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, Context, DispatchPhase, Element, ElementId, ElementInputHandler, Entity, FontWeight,
    GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, Hsla, InspectorElementId, InteractiveElement as _,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels,
    Point, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, TextLayout,
    UnderlineStyle, Window, canvas, div, fill, point, px, rgb, size,
};
use teams_proto::chat::FontSize;

use super::doc::{Block, Kind, Pos};
use super::{CONTEXT, Editor, Laid};
use crate::rich_text::{self, Mentioned, Style};
use crate::selection;
use crate::theme::{self, Palette};

/// The width of a list item's marker column, px.
const MARKER: f32 = 24.0;

/// How far a nested list moves in per level, px.
const LIST_STEP: f32 = 20.0;

/// The cursor's width, px.
const CARET: f32 = 1.5;

/// The size most of a block's text has (by bytes); `None` for medium.
fn block_size(block: &Block) -> Option<FontSize> {
    let mut counts = [0usize; 3];
    for run in &block.runs {
        let slot = match run.attrs.span.size {
            None => 0,
            Some(FontSize::Small) => 1,
            Some(FontSize::Large) => 2,
        };
        counts[slot] += run.text.len();
    }
    match counts
        .iter()
        .enumerate()
        .max_by_key(|(slot, count)| (**count, std::cmp::Reverse(*slot)))
    {
        Some((1, _)) => Some(FontSize::Small),
        Some((2, _)) => Some(FontSize::Large),
        _ => None,
    }
}

/// The highlights a block is drawn with: each run's style as the renderer draws it (a mention in the brand colour).
fn run_highlights(block: &Block, palette: Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    block
        .ranges()
        .map(|(range, run)| {
            let style = Style {
                marks: run.attrs.marks,
                mention: run.attrs.mention.as_ref().map(|_| Mentioned::Other),
                link: run.attrs.link.is_some(),
                image: false,
                span: run.attrs.span,
            };
            (range, rich_text::highlight(style, palette))
        })
        .filter(|(_, style)| *style != HighlightStyle::default())
        .collect()
}

/// The markers of the document's list items, by block: a number counted within its list and level, or a bullet by
/// how deep the item is (disc, circle, square).
fn markers(blocks: &[Block]) -> Vec<Option<String>> {
    let mut counters: Vec<u32> = Vec::new();
    blocks
        .iter()
        .map(|block| {
            if !block.kind.is_list() {
                counters.clear();
                return None;
            }
            let depth = usize::from(block.indent);
            counters.truncate(depth + 1);
            counters.resize(depth + 1, 0);
            counters[depth] += 1;
            Some(match block.kind {
                Kind::Numbered => format!("{}.", counters[depth]),
                _ => match depth % 3 {
                    0 => "\u{2022}",
                    1 => "\u{25e6}",
                    _ => "\u{25aa}",
                }
                .to_owned(),
            })
        })
        .collect()
}

impl Editor {
    /// The selected part of block `index` (`len` bytes), with an empty block inside the selection showing a little.
    fn selected_in(&self, index: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.selection.range();
        if start == end || index < start.block || index > end.block {
            return None;
        }
        let from = if index == start.block { start.offset } else { 0 };
        let to = if index == end.block { end.offset } else { len + 1 };
        (from < to).then_some(from..to)
    }

    /// Block `index`'s text element.
    pub(super) fn block_text(&self, index: usize, palette: Palette, focused: bool, cx: &App) -> BlockText {
        let block = &self.doc.blocks[index];
        let placeholder = index == 0 && self.is_empty() && !self.placeholder.is_empty();
        let mut content = if placeholder {
            self.placeholder.to_string()
        } else {
            block.text()
        };
        // an empty line keeps its height, and so does the one a line break ends a block with
        if content.is_empty() || content.ends_with('\n') {
            content.push(' ');
        }
        let mut highlights = if placeholder {
            vec![(
                0..content.len(),
                HighlightStyle {
                    color: Some(rgb(palette.muted_foreground).into()),
                    ..HighlightStyle::default()
                },
            )]
        } else {
            run_highlights(block, palette)
        };
        if let Some((start, end)) = self.marked.filter(|(start, _)| start.block == index && !placeholder) {
            let range = start.offset..if end.block == index { end.offset } else { block.len() };
            highlights = underlined(&highlights, &range);
        }
        let selected = self
            .selected_in(index, block.len())
            .map(|range| range.start.min(content.len())..range.end.min(content.len()));
        let highlights = selection::with_selection(highlights, selected, selection::colour(palette));
        let text = StyledText::new(SharedString::from(content)).with_highlights(highlights);
        let caret = (focused && self.selection.is_empty() && self.selection.head.block == index && !self.read_only)
            .then(|| {
                (
                    if placeholder { 0 } else { self.selection.head.offset },
                    rgb(palette.foreground).into(),
                )
            });
        let _ = cx;
        BlockText {
            id: ElementId::Name(format!("rich-editor-block-{index}").into()),
            index,
            text,
            laid: Rc::clone(&self.laid),
            caret,
            reveal: (Rc::clone(&self.reveal), self.scroll.clone()),
        }
    }

    /// Block `index`'s row: its text after its list marker, beside its quote's bar, on its code card, or a rule.
    fn row(&self, index: usize, marker: Option<String>, palette: Palette, focused: bool, cx: &App) -> AnyElement {
        let block = &self.doc.blocks[index];
        let text = self.block_text(index, palette, focused, cx);
        let size = match block.kind {
            Kind::Heading(level) => rich_text::block_text_size(Some(level), None),
            _ => rich_text::block_text_size(None, block_size(block)),
        };
        let placeholder = index == 0 && self.is_empty() && !self.placeholder.is_empty();
        let body = div()
            .flex_1()
            .min_w_0()
            // a placeholder is one line, cut short where the box is narrow
            .when(placeholder, gpui::Styled::truncate)
            .when_some(size, |body, size| body.text_size(px(size)).line_height(px(size * 1.35)))
            .when(matches!(block.kind, Kind::Heading(_)), |body| {
                body.font_weight(FontWeight::SEMIBOLD)
            })
            .child(text);
        let row = div().w_full().flex().items_start();
        match block.kind {
            Kind::Bullet | Kind::Numbered => row
                .pl(px(LIST_STEP * f32::from(block.indent)))
                .child(
                    div()
                        .flex_none()
                        .w(px(MARKER))
                        .pr(px(6.0))
                        .flex()
                        .justify_end()
                        .whitespace_nowrap()
                        .children(marker),
                )
                .child(body)
                .into_any_element(),
            Kind::Quote => row
                .pl_2()
                .border_l_2()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.hover))
                .child(body)
                .into_any_element(),
            Kind::Code => {
                let mono = cx
                    .try_global::<gpui_component::theme::Theme>()
                    .map(|theme| theme.mono_font_family.clone());
                row.my_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.hover))
                    .when_some(mono, gpui::Styled::font_family)
                    .text_sm()
                    .child(body)
                    .into_any_element()
            }
            Kind::Rule => row
                .relative()
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(10.0))
                        .h(px(1.0))
                        .bg(rgb(palette.border)),
                )
                .child(body)
                .into_any_element(),
            Kind::Paragraph | Kind::Heading(_) | Kind::Picture(_) => row
                .pl(px(rich_text::INDENT * f32::from(block.indent)))
                .child(body)
                .into_any_element(),
        }
    }

    // --- where a place is drawn ---

    /// Where the cursor at `at` is drawn: its top left, in the window.
    pub(super) fn caret_point(&self, at: Pos) -> Option<Point<Pixels>> {
        let laid = self.laid.borrow();
        let layout = laid.get(at.block)?.text.as_ref()?;
        layout.position_for_index(at.offset)
    }

    /// The height of the line at `at`.
    pub(super) fn line_height_at(&self, at: Pos) -> Option<Pixels> {
        let laid = self.laid.borrow();
        Some(laid.get(at.block)?.text.as_ref()?.line_height())
    }

    /// The place nearest `at` in the block drawn level with it (the first above them all, the last below).
    pub(super) fn pos_for_point(&self, at: Point<Pixels>) -> Option<Pos> {
        let block = {
            let laid = self.laid.borrow();
            let mut found = None;
            for (index, entry) in laid.iter().enumerate().take(self.doc.blocks.len()) {
                let Some(row) = entry.row else { continue };
                found = Some(index);
                if at.y < row.bottom() {
                    break;
                }
            }
            found?
        };
        self.pos_in_block(block, at)
    }

    /// The place nearest `at` in block `index`.
    fn pos_in_block(&self, index: usize, at: Point<Pixels>) -> Option<Pos> {
        let laid = self.laid.borrow();
        let layout = laid.get(index)?.text.as_ref()?;
        let offset = selection::closest_index(layout, at).min(self.doc.blocks[index].len());
        if self.is_empty() {
            return Some(Pos::default());
        }
        Some(Pos::new(index, self.doc.blocks[index].snap(offset, false)))
    }

    /// The place on the line drawn above or below `from` (`down`), nearest `goal_x`; `None` past the first or last.
    pub(super) fn line_neighbour(&self, from: Pos, down: bool, goal_x: Pixels) -> Option<Pos> {
        let caret = self.caret_point(from)?;
        let line = self.line_height_at(from)?;
        let y = if down {
            caret.y + line * 1.5
        } else {
            caret.y - line * 0.5
        };
        let (top, bottom) = {
            let laid = self.laid.borrow();
            let rows: Vec<Bounds<Pixels>> = laid.iter().filter_map(|entry| entry.row).collect();
            (rows.first()?.top(), rows.last()?.bottom())
        };
        if y < top || y >= bottom {
            return None;
        }
        // between two rows (a quote's padding, a code card's margin), the nearer one
        self.pos_for_point(point(goal_x, y))
    }

    /// The start or end (`end`) of the line `from` is drawn on.
    pub(super) fn line_edge_of(&self, from: Pos, end: bool) -> Option<Pos> {
        let caret = self.caret_point(from)?;
        let line = self.line_height_at(from)?;
        let row = self.laid.borrow().get(from.block)?.row?;
        let x = if end {
            row.right() + px(10_000.0)
        } else {
            row.left() - px(1.0)
        };
        self.pos_in_block(from.block, point(x, caret.y + line / 2.0))
    }
}

/// `highlights` with `range` underlined (as an input method's text being composed is drawn), split where it crosses
/// them.
fn underlined(
    highlights: &[(Range<usize>, HighlightStyle)],
    range: &Range<usize>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let underline = UnderlineStyle {
        thickness: px(1.0),
        ..UnderlineStyle::default()
    };
    let mut cuts = vec![range.start, range.end];
    for (piece, _) in highlights {
        cuts.extend([piece.start, piece.end]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let style_at = |at: usize| {
        highlights
            .iter()
            .find(|(piece, _)| piece.contains(&at))
            .map(|(_, style)| *style)
            .unwrap_or_default()
    };
    cuts.windows(2)
        .filter_map(|pair| {
            let (from, to) = (pair[0], pair[1]);
            let mut style = style_at(from);
            if from >= range.start && to <= range.end {
                style.underline = Some(underline);
            }
            (style != HighlightStyle::default()).then_some((from..to, style))
        })
        .collect()
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let focused = self.focus.is_focused(window);
        {
            let mut laid = self.laid.borrow_mut();
            laid.resize(self.doc.blocks.len(), Laid::default());
        }
        let markers = markers(&self.doc.blocks);
        let rows: Vec<AnyElement> = markers
            .into_iter()
            .enumerate()
            .map(|(index, marker)| match self.doc.blocks[index].kind {
                Kind::Picture(id) => self.picture_row(index, id, palette, focused, cx),
                _ => self.row(index, marker, palette, focused, cx),
            })
            .collect();
        let scroll = div()
            .id("rich-editor-scroll")
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .when_some(self.max_height, gpui::Styled::max_h)
            .when_some(self.min_height, gpui::Styled::min_h)
            .flex()
            .flex_col()
            .children(rows);
        let root = div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .debug_selector(|| "rich-editor".to_owned())
            .relative()
            .w_full()
            .cursor_text()
            // the press-and-drag layer under the rows, so a picture's own buttons over them take their clicks
            .child(input_and_mouse(cx.entity()))
            .child(scroll);
        Self::on_actions(root, cx)
    }
}

/// The input handler and the mouse over the whole editor: a press places the cursor (and gives the editor the focus),
/// a drag extends the selection, wherever the pointer goes while the button is down.
fn input_and_mouse(editor: Entity<Editor>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox: Hitbox, window, cx| {
            let focus = editor.read(cx).focus.clone();
            window.handle_input(&focus, ElementInputHandler::new(bounds, editor.clone()), cx);
            let pressing = editor.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                pressing.update(cx, |this, cx| {
                    this.focus(window, cx);
                    if let Some(at) = this.pos_for_point(event.position) {
                        this.press(at, event.click_count, event.modifiers.shift, cx);
                    }
                });
            });
            let dragging = editor.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble || event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                if !dragging.read(cx).dragging() {
                    return;
                }
                dragging.update(cx, |this, cx| {
                    if let Some(at) = this.pos_for_point(event.position) {
                        this.drag_to(at, cx);
                    }
                });
            });
            let releasing = editor.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && releasing.read(cx).dragging() {
                    releasing.update(cx, |this, _| this.release());
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// A block's text: GPUI's styled text, its layout kept for the editor, the cursor drawn in it.
pub(super) struct BlockText {
    id: ElementId,
    index: usize,
    text: StyledText,
    laid: Rc<RefCell<Vec<Laid>>>,
    /// Where the cursor is drawn, and in what colour.
    caret: Option<(usize, Hsla)>,
    /// Whether the cursor is to be scrolled into view, and the scroll it is in.
    reveal: (Rc<Cell<bool>>, ScrollHandle),
}

impl IntoElement for BlockText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl BlockText {
    fn layout(&self) -> &TextLayout {
        self.text.layout()
    }

    /// Scrolls the editor so `caret` (in the window) is in view, once asked.
    fn reveal(&self, caret: Bounds<Pixels>, window: &mut Window) {
        let (asked, scroll) = &self.reveal;
        if !asked.replace(false) {
            return;
        }
        let view = scroll.bounds();
        let mut offset = scroll.offset();
        if caret.bottom() > view.bottom() {
            offset.y -= caret.bottom() - view.bottom();
        } else if caret.top() < view.top() {
            offset.y += view.top() - caret.top();
        } else {
            return;
        }
        scroll.set_offset(offset);
        window.refresh();
    }
}

impl Element for BlockText {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.prepaint(None, inspector_id, bounds, state, window, cx);
        if let Some(entry) = self.laid.borrow_mut().get_mut(self.index) {
            *entry = Laid {
                text: Some(self.layout().clone()),
                row: Some(bounds),
            };
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .paint(None, inspector_id, bounds, &mut (), &mut (), window, cx);
        let Some((offset, colour)) = self.caret else { return };
        let layout = self.layout().clone();
        let Some(at) = layout.position_for_index(offset) else {
            return;
        };
        let caret = Bounds::new(at, size(px(CARET), layout.line_height()));
        window.paint_quad(fill(caret, colour));
        self.reveal(caret, window);
    }
}
