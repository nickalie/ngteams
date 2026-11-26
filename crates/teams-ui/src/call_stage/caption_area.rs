//! The live captions area's scroll and size. Its lines scroll by the wheel, the scroll bar and the keys (a tab stop:
//! Up, Down, Page Up, Page Down, Home, End). The area follows the newest line while it shows the end, and a scroll up
//! stops that until the user goes back to the end, as Teams does.
//!
//! The edge between the area and the stage resizes it, by a drag or by the keys on that edge (a tab stop: Up, Down,
//! Page Up, Page Down, Home, End). The height is kept for each account in the settings
//! ([`crate::settings::captions_height`]); a drag saves it once, when it ends.
use super::*;
use crate::settings::captions_height;
use gpui::{KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, point};
use gpui_component::scroll::{Scrollbar, ScrollbarMode};

/// How far one Up or Down key scrolls the lines, px: about one caption line.
const LINE_STEP: f32 = 40.0;
/// How far one Up or Down key on the edge resizes the area, px.
const SIZE_STEP: f32 = 20.0;
/// How far one Page Up or Page Down key on the edge resizes the area, px.
const SIZE_PAGE: f32 = 100.0;
/// How near its end (px) the lines must be to follow: each frame follows, so only the end itself counts.
const FOLLOW_WITHIN: f32 = 0.01;
/// The edge's height, px: a thin strip inside the area that the pointer can catch.
const EDGE_HEIGHT: f32 = 6.0;
/// The area's border, px: the measure inside it does not see it.
pub(super) const BORDER: f32 = 1.0;
/// The scroll bar's width, px.
const SCROLLBAR_WIDTH: f32 = 12.0;

/// A drag of the edge: where the pointer was when it started, and the heights that a shrink and a growth start from.
#[derive(Clone, Copy, Debug)]
pub(super) struct Drag {
    from: Pixels,
    height: f32,
    drawn: f32,
}

/// The captions area's scroll and size state.
pub(super) struct CaptionArea {
    pub(super) scroll: gpui::ScrollHandle,
    /// The list of lines: a tab stop whose keys scroll it.
    lines_focus: FocusHandle,
    /// The edge: a tab stop whose keys resize the area.
    edge_focus: FocusHandle,
    drag: Option<Drag>,
    /// The height while a drag goes on, saved when it ends.
    dragged: Option<f32>,
    /// The area's and the stage's heights as last laid out, px.
    laid: Rc<std::cell::Cell<Option<(f32, f32)>>>,
}

impl CaptionArea {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            scroll: gpui::ScrollHandle::new(),
            lines_focus: cx.focus_handle(),
            edge_focus: cx.focus_handle(),
            drag: None,
            dragged: None,
            laid: Rc::default(),
        }
    }

    /// An empty element over the area (`pane`) or the stage that keeps its height as it is laid out.
    pub(super) fn measure(&self, pane: bool) -> AnyElement {
        let laid = Rc::clone(&self.laid);
        gpui::canvas(
            move |bounds, _, _| {
                let height = f32::from(bounds.size.height);
                let (area, stage) = laid.get().unwrap_or((height, height));
                laid.set(Some(if pane { (height + BORDER, stage) } else { (area, height) }));
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }

    /// The lines stay at their end while they were there at the last frame. A hidden area starts at the end again.
    pub(super) fn follow(&self, shown: bool) {
        let scroll = &self.scroll;
        if !shown || scroll.max_offset().y + scroll.offset().y <= px(FOLLOW_WITHIN) {
            scroll.scroll_to_bottom();
        }
    }
}

/// Where a key of the lines goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum LineKey {
    /// By this many px, up when negative.
    By(f32),
    /// A page up or down.
    Page {
        up: bool,
    },
    Top,
    Bottom,
}

/// The step that a key takes over the lines; `None` for any other key.
pub(super) fn line_key(event: &KeyDownEvent) -> Option<LineKey> {
    if event.keystroke.modifiers.modified() {
        return None;
    }
    Some(match event.keystroke.key.as_str() {
        "up" => LineKey::By(-LINE_STEP),
        "down" => LineKey::By(LINE_STEP),
        "pageup" => LineKey::Page { up: true },
        "pagedown" => LineKey::Page { up: false },
        "home" => LineKey::Top,
        "end" => LineKey::Bottom,
        _ => return None,
    })
}

/// The height that a key on the edge gives the area, from `height`, inside `MIN` and `room`; `None` for any other
/// key. Up moves the edge up: it makes the area at the bottom taller, and the area at the top (`at_top`) shorter.
pub(super) fn size_key(event: &KeyDownEvent, height: f32, room: f32, at_top: bool) -> Option<f32> {
    if event.keystroke.modifiers.modified() {
        return None;
    }
    let grow = |by: f32| if at_top { height - by } else { height + by };
    let min = min_height();
    let new = match event.keystroke.key.as_str() {
        "up" => grow(SIZE_STEP),
        "down" => grow(-SIZE_STEP),
        "pageup" => grow(SIZE_PAGE),
        "pagedown" => grow(-SIZE_PAGE),
        "home" => min,
        "end" => room,
        _ => return None,
    };
    Some(new.clamp(min, room.max(min)))
}

#[expect(clippy::cast_precision_loss, reason = "the limits are a few hundred px")]
pub(super) fn min_height() -> f32 {
    captions_height::MIN as f32
}

#[expect(clippy::cast_precision_loss, reason = "the limits are a few hundred px")]
fn max_height() -> f32 {
    captions_height::MAX as f32
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a height inside the limits, a few hundred px"
)]
fn whole_px(height: f32) -> u32 {
    height.round().max(0.0) as u32
}

impl CallStage {
    /// The area's height now: the drag's while one goes on, else the account's.
    #[expect(clippy::cast_precision_loss, reason = "the limits are a few hundred px")]
    pub(super) fn caption_height(&self, cx: &App) -> f32 {
        self.caption_area
            .dragged
            .unwrap_or_else(|| self.settings.read(cx).settings().caption_height_of(self.core.me()) as f32)
    }

    /// The height that a shrink starts from: the saved height, as a short window shrinks it, so a shrink never saves
    /// the language confirmation's taller floor.
    fn caption_start_height(&self, cx: &App) -> f32 {
        self.caption_height(cx).min(self.caption_room())
    }

    /// The height that a growth starts from: the drawn height when the language confirmation makes it taller, so the
    /// first step moves the edge.
    fn caption_grow_height(&self, cx: &App) -> f32 {
        let start = self.caption_start_height(cx);
        // the laid height can be one frame old: use it only for the confirmation's taller floor
        if self.caption_language.is_none() {
            return start;
        }
        self.caption_area.laid.get().map_or(start, |(area, _)| area.max(start))
    }

    /// The tallest the area gets in this window: the area and the stage as laid out, less the stage's least room.
    fn caption_room(&self) -> f32 {
        let room = self.caption_area.laid.get().map_or(
            self.paging.window.1 - HEADER_HEIGHT - MIN_STAGE_HEIGHT,
            |(area, stage)| area + stage - MIN_STAGE_HEIGHT,
        );
        room.min(max_height())
    }

    /// Sets the live captions area's height for the call's account, px, inside [`captions_height::MIN`] and
    /// [`captions_height::MAX`].
    pub fn resize_captions(&mut self, height: u32, cx: &mut Context<Self>) {
        let account = self.core.me().to_owned();
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.set_caption_height(&account, height), cx);
        });
        cx.notify();
    }

    fn save_caption_height(&self, height: f32, cx: &mut Context<Self>) {
        let account = self.core.me().to_owned();
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.set_caption_height(&account, whole_px(height)), cx);
        });
    }

    /// The list of lines: its keys, its scroll and its scroll bar.
    pub(super) fn caption_text(
        &self,
        lines: Vec<AnyElement>,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let keyboard_on = self.caption_area.lines_focus.is_focused(window);
        let cut = self.caption_area.scroll.max_offset().y > px(0.0);
        let list = div()
            .id("call-caption-text")
            .role(gpui::accesskit::Role::List)
            .aria_label("Live captions")
            .track_focus(&self.caption_area.lines_focus.clone().tab_stop(true))
            .on_key_down(cx.listener(|this, event, _, cx| this.caption_key(event, cx)))
            .when(keyboard_on, |list| {
                list.debug_selector(|| "call-caption-keys".to_owned())
            })
            .size_full()
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(rgb(palette.brand)))
            .overflow_y_scroll()
            .track_scroll(&self.caption_area.scroll)
            .pr(px(SCROLLBAR_WIDTH))
            .children(lines);
        let scrollbar = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(SCROLLBAR_WIDTH))
            .child(
                Scrollbar::vertical(&self.caption_area.scroll)
                    .id("call-caption-scrollbar")
                    .mode(ScrollbarMode::Always)
                    .viewport_from_layout()
                    .styles(move |styles| {
                        styles
                            .thumb(move |thumb| thumb.bg(gpui::Hsla::from(rgb(palette.muted_foreground)).alpha(0.6)))
                            .thumb_hover(move |thumb| thumb.bg(gpui::Hsla::from(rgb(palette.muted_foreground))))
                    }),
            )
            .when(cut, |bar| bar.debug_selector(|| "call-caption-scrollbar".to_owned()));
        div()
            .debug_selector(|| "call-caption-text".to_owned())
            .relative()
            .flex_1()
            .min_h_0()
            .child(list)
            .child(scrollbar)
    }

    fn caption_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(key) = line_key(event) else {
            return;
        };
        let scroll = &self.caption_area.scroll;
        let offset = scroll.offset();
        let bottom = -scroll.max_offset().y;
        let y = match key {
            LineKey::By(by) => offset.y - px(by),
            LineKey::Page { up } => {
                let page = scroll.bounds().size.height;
                if up { offset.y + page } else { offset.y - page }
            }
            LineKey::Top => px(0.0),
            LineKey::Bottom => bottom,
        };
        scroll.set_offset(point(offset.x, y.clamp(bottom, px(0.0))));
        cx.stop_propagation();
        cx.notify();
    }

    /// The edge between the area and the stage: a drag or its keys resize the area.
    pub(super) fn caption_edge(
        &self,
        palette: Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let keyboard_on = self.caption_area.edge_focus.is_focused(window);
        let dragging = self.caption_area.drag.is_some();
        let edge = div()
            .id("call-caption-resize")
            .debug_selector(|| "call-caption-resize".to_owned())
            .role(gpui::accesskit::Role::Splitter)
            .aria_label("Resize live captions")
            .tooltip(|window, cx| Tooltip::new("Resize live captions").build(window, cx))
            .track_focus(&self.caption_area.edge_focus.clone().tab_stop(true))
            .on_key_down(cx.listener(|this, event, _, cx| this.caption_edge_key(event, cx)))
            .when(keyboard_on, |edge| {
                edge.debug_selector(|| "call-caption-resize-keys".to_owned())
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.caption_area.drag = Some(Drag {
                        from: event.position.y,
                        height: this.caption_start_height(cx),
                        drawn: this.caption_grow_height(cx),
                    });
                    cx.notify();
                }),
            )
            .absolute()
            .left_0()
            .right_0()
            .h(px(EDGE_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .cursor_ns_resize()
            .child(
                div()
                    .w(px(32.0))
                    .h(px(4.0))
                    .rounded_full()
                    .bg(rgb(if dragging || keyboard_on {
                        palette.brand
                    } else {
                        palette.border
                    })),
            );
        if self.caption_top {
            edge.bottom_0()
        } else {
            edge.top_0()
        }
    }

    fn caption_edge_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let room = self.caption_room();
        let start = self.caption_start_height(cx);
        let Some(mut height) = size_key(event, start, room, self.caption_top) else {
            return;
        };
        if height > start {
            height = size_key(event, self.caption_grow_height(cx), room, self.caption_top).unwrap_or(height);
        }
        self.save_caption_height(height, cx);
        cx.stop_propagation();
        cx.notify();
    }

    /// The pointer moved: a drag of the edge resizes the area; a move with no button down ends the drag.
    pub(super) fn caption_dragged(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.caption_area.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.caption_drag_ended(cx);
            return;
        }
        let moved = f32::from(event.position.y - drag.from);
        let grown = if self.caption_top { moved } else { -moved };
        let height = if grown > 0.0 { drag.drawn } else { drag.height } + grown;
        let min = min_height();
        self.caption_area.dragged = Some(height.clamp(min, self.caption_room().max(min)));
        cx.notify();
    }

    /// The drag ended: the height it left is saved for the account.
    pub(super) fn caption_drag_ended(&mut self, cx: &mut Context<Self>) {
        if self.caption_area.drag.take().is_none() {
            return;
        }
        if let Some(height) = self.caption_area.dragged.take() {
            self.save_caption_height(height, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, modifiers: gpui::Modifiers) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers,
                key: name.to_owned(),
                key_char: None,
            },
            is_held: false,
            prefer_character_input: false,
        }
    }

    #[test]
    fn the_lines_keys_scroll_by_a_line_a_page_or_to_an_end() {
        let none = gpui::Modifiers::none();
        assert_eq!(line_key(&key("up", none)), Some(LineKey::By(-LINE_STEP)));
        assert_eq!(line_key(&key("down", none)), Some(LineKey::By(LINE_STEP)));
        assert_eq!(line_key(&key("pageup", none)), Some(LineKey::Page { up: true }));
        assert_eq!(line_key(&key("pagedown", none)), Some(LineKey::Page { up: false }));
        assert_eq!(line_key(&key("home", none)), Some(LineKey::Top));
        assert_eq!(line_key(&key("end", none)), Some(LineKey::Bottom));
        assert_eq!(line_key(&key("a", none)), None);
        assert_eq!(line_key(&key("up", gpui::Modifiers::shift())), None);
    }

    #[test]
    fn the_edge_s_keys_resize_inside_the_limits_from_either_place() {
        let none = gpui::Modifiers::none();
        let size = |name: &str, height, at_top| size_key(&key(name, none), height, 500.0, at_top);
        assert_eq!(size("up", 244.0, false), Some(264.0));
        assert_eq!(size("down", 244.0, false), Some(224.0));
        assert_eq!(size("up", 244.0, true), Some(224.0));
        assert_eq!(size("pageup", 244.0, false), Some(344.0));
        assert_eq!(size("pagedown", 244.0, true), Some(344.0));
        assert_eq!(size("home", 244.0, false), Some(min_height()));
        assert_eq!(size("end", 244.0, false), Some(500.0));
        assert_eq!(size("pageup", 480.0, false), Some(500.0), "no taller than the room");
        assert_eq!(
            size("pagedown", 120.0, false),
            Some(min_height()),
            "no shorter than the least"
        );
        assert_eq!(
            size_key(&key("end", none), 244.0, 50.0, false),
            Some(min_height()),
            "a short window"
        );
        assert_eq!(size("a", 244.0, false), None);
        assert_eq!(
            size_key(&key("up", gpui::Modifiers::control()), 244.0, 500.0, false),
            None
        );
    }

    #[test]
    fn a_height_is_kept_in_whole_px() {
        assert_eq!(whole_px(243.6), 244);
        assert_eq!(whole_px(-4.0), 0);
    }
}
