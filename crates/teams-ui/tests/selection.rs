//! A message's text selected with the mouse and copied, as Teams allows: a drag across its runs and paragraphs (bold, a
//! link and inline code among them) and on into the next message, a double-click on a word, a triple-click on a
//! paragraph, a click on a link still opening it, and the selection copied as plain text; a right-click on a link
//! offering "Copy link" (its href, not the text shown) and "Open link". Drawn on GPUI's test context; every id and link
//! is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::Arc;

use gpui::{
    Context, InteractiveElement as _, IntoElement, Modifiers, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Point, Render, SharedString, Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};
use teams_proto::chat::parse_html;
use teams_ui::rich_text::{self, Mentions, Rendered};
use teams_ui::selection::{MessageKey, Scope, TextSelection, close_link_menu, link_menu, open_link_menu};
use teams_ui::theme::LIGHT_PALETTE;

fn laid_out(content: &str) -> Arc<Rendered> {
    Arc::new(rich_text::layout(&parse_html(content), &Mentions::default()))
}

/// Two messages drawn one under the other, their text selectable in one selection, as the chat view draws them.
struct Messages {
    selection: TextSelection,
    messages: Vec<(MessageKey, Arc<Rendered>)>,
}

impl Render for Messages {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self.selection.clone();
        div()
            .w(px(600.0))
            .flex()
            .flex_col()
            .gap_4()
            // as the views do: a press anywhere lets the selection go before the text takes it
            .capture_any_mouse_down(move |event, _, _| {
                if event.button == MouseButton::Left && !event.modifiers.shift {
                    let _ = selection.clear();
                }
            })
            .children(self.messages.iter().map(|(key, rendered)| {
                let id = SharedString::from(format!("message-text-{}", key.id));
                let scope = Scope::new(&self.selection, key.clone());
                let selector = format!("message-{}", key.id);
                div()
                    .debug_selector(move || selector)
                    .child(rich_text::render_selectable(
                        &id,
                        rendered,
                        LIGHT_PALETTE,
                        None,
                        Some(&scope),
                    ))
            }))
    }
}

const FIRST: &str = "<p>plain <b>bold</b> <a href=\"https://example.test/opened\">link</a> <code>x = 1</code></p>\
                     <p>second line</p>";
const SECOND: &str = "<p>next message</p>";

fn drawn() -> (
    TextSelection,
    Vec<(MessageKey, Arc<Rendered>)>,
    &'static mut VisualTestContext,
) {
    drawn_of(&[FIRST, SECOND])
}

/// The messages of `contents`, drawn as [`Messages`] draws them; the first message's id is "1".
fn drawn_of(
    contents: &[&str],
) -> (
    TextSelection,
    Vec<(MessageKey, Arc<Rendered>)>,
    &'static mut VisualTestContext,
) {
    let selection = TextSelection::default();
    let messages = (1_u32..)
        .zip(contents)
        .map(|(id, content)| (MessageKey::new((i64::from(id), 0), id.to_string()), laid_out(content)))
        .collect::<Vec<_>>();
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    // the kit's theme, which a code card's copy button reads
    app.update(teams_ui::theme::init);
    let (view_selection, view_messages) = (selection.clone(), messages.clone());
    let (_, cx) = app.add_window_view(move |_, _| Messages {
        selection: view_selection,
        messages: view_messages,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (selection, messages, cx)
}

/// Points on the paragraphs' first lines, top to bottom: the first message's two (each one line), the second's one.
fn paragraphs(cx: &mut VisualTestContext) -> Vec<Point<Pixels>> {
    let first = cx.debug_bounds("message-1").expect("the first message is drawn");
    let second = cx.debug_bounds("message-2").expect("the second message is drawn");
    vec![
        first.origin + point(px(1.0), px(4.0)),
        point(first.left() + px(1.0), first.bottom() - px(4.0)),
        second.origin + point(px(1.0), px(4.0)),
    ]
}

fn copied(selection: &TextSelection, messages: &[(MessageKey, Arc<Rendered>)]) -> Option<String> {
    selection.text(messages.iter().map(|(key, rendered)| (key.clone(), &**rendered)))
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

#[test]
fn a_drag_selects_across_runs_paragraphs_and_messages() {
    let (selection, messages, cx) = drawn();
    let drawn = paragraphs(cx);
    let start = drawn[0];
    // past the end of the first message's second line
    let end = drawn[1] + point(px(500.0), px(0.0));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
    assert_eq!(
        copied(&selection, &messages).as_deref(),
        Some("plain bold link x = 1\nsecond line")
    );
    assert!(cx.opened_url().is_none(), "a drag opens no link");
    // on into the next message, from the middle of the first paragraph
    let middle = drawn[0];
    let next = drawn[2] + point(px(500.0), px(0.0));
    cx.simulate_mouse_down(middle, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(next, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(next, MouseButton::Left, Modifiers::none());
    redraw(cx);
    assert_eq!(
        copied(&selection, &messages).as_deref(),
        Some("plain bold link x = 1\nsecond line\n\nnext message")
    );
    // a click elsewhere lets it go
    cx.simulate_click(next, Modifiers::none());
    assert!(selection.is_empty());
}

fn double_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
    for click_count in 1..=2 {
        cx.simulate_event(MouseDownEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    }
}

/// The advance of each character of the test platform: 0.6 em.
fn advance(cx: &mut VisualTestContext) -> Pixels {
    cx.update(|window, _| window.text_style().font_size.to_pixels(window.rem_size()) * 0.6)
}

#[test]
fn a_press_and_a_drag_in_a_right_aligned_message_take_the_letters_where_they_are_drawn() {
    // right to left and aligned right: the first word's three letters at the right, a space, then the second word
    let (selection, messages, cx) = drawn_of(&["<p>ابت ثج</p>"]);
    let advance = advance(cx);
    let line = cx.debug_bounds("rich-text").expect("the line is drawn");
    let y = line.top() + px(4.0);
    double_click(cx, point(line.right() - advance * 5.0, y));
    assert_eq!(copied(&selection, &messages).as_deref(), Some("ثج"));
    let (start, end) = (
        point(line.right() - advance * 0.2, y),
        point(line.right() - advance * 2.8, y),
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    assert_eq!(copied(&selection, &messages).as_deref(), Some("ابت"));
}

#[test]
fn code_in_a_right_to_left_message_reads_left_to_right_from_the_left() {
    let (selection, messages, cx) = drawn_of(&["<p>مرحبا</p><pre>let x = 1;</pre>"]);
    let lines = cx.debug_bounds("rich-code-lines").expect("the code is drawn");
    let rem = cx.update(|window, _| window.rem_size());
    // the lines' padding (1.5 rem), a one-digit number column (0.65 rem) and the gap (1 rem)
    let text_left = lines.left() + rem * 3.15;
    double_click(cx, point(text_left + px(2.0), lines.top() + rem + px(4.0)));
    assert_eq!(copied(&selection, &messages).as_deref(), Some("let"));
}

#[test]
fn a_double_click_selects_a_word_and_a_triple_click_its_paragraph() {
    let (selection, messages, cx) = drawn();
    let drawn = paragraphs(cx);
    let on_second = drawn[1] + point(px(2.0), px(0.0));
    for click_count in 1..=2 {
        cx.simulate_event(MouseDownEvent {
            position: on_second,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        cx.simulate_mouse_up(on_second, MouseButton::Left, Modifiers::none());
    }
    assert_eq!(copied(&selection, &messages).as_deref(), Some("second"));
    cx.simulate_event(MouseDownEvent {
        position: on_second,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 3,
        first_mouse: false,
    });
    cx.simulate_mouse_up(on_second, MouseButton::Left, Modifiers::none());
    assert_eq!(copied(&selection, &messages).as_deref(), Some("second line"));
}

#[test]
fn a_click_on_a_link_still_opens_it_and_selects_nothing() {
    let (selection, _, cx) = drawn();
    let first = paragraphs(cx)[0];
    // "plain bold " is before the link: find a spot on it by walking right until the click opens it
    let opened = (0..60u8).any(|step| {
        let at = first + point(px(4.0 * f32::from(step)), px(0.0));
        cx.simulate_click(at, Modifiers::none());
        cx.opened_url().is_some()
    });
    assert!(opened, "a click on the link opens it");
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.test/opened"));
    assert!(selection.is_empty(), "a click selects nothing");
}

/// A right-click at each spot along `from`'s line, walking right, until one opens a link's menu; where it did.
fn right_click_a_link(cx: &mut VisualTestContext, from: Point<Pixels>) -> Option<Point<Pixels>> {
    (0..60u8).find_map(|step| {
        let at = from + point(px(4.0 * f32::from(step)), px(0.0));
        cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
        cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
        redraw(cx);
        menu_open(cx).map(|_| at)
    })
}

/// The link whose menu is open in the window.
fn menu_open(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|window, cx| link_menu(window, cx))
}

#[test]
fn a_right_click_on_a_link_offers_copy_link_which_copies_its_href() {
    let (selection, _, cx) = drawn();
    let first = paragraphs(cx)[0];
    // a right-click on plain text with nothing selected opens no menu
    cx.simulate_mouse_down(first, MouseButton::Right, Modifiers::none());
    redraw(cx);
    assert_eq!(menu_open(cx), None);
    assert!(
        right_click_a_link(cx, first).is_some(),
        "a right-click on the link opens its menu"
    );
    assert_eq!(menu_open(cx).as_deref(), Some("https://example.test/opened"));
    assert!(cx.debug_bounds("link-menu").is_some(), "the menu is drawn");
    assert!(cx.debug_bounds("link-menu-open").is_some(), "it offers Open link");
    let copy = cx.debug_bounds("link-menu-copy").expect("it offers Copy link");
    cx.simulate_click(copy.center(), Modifiers::none());
    // the link's href, not the text shown ("link")
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("https://example.test/opened")
    );
    assert_eq!(menu_open(cx), None, "a choice closes the menu");
    assert!(cx.opened_url().is_none(), "copying opens nothing");
    assert!(selection.is_empty(), "a right-click selects nothing");
}

#[test]
fn open_link_opens_it_and_a_press_outside_closes_the_menu() {
    let (_, _, cx) = drawn();
    let drawn = paragraphs(cx);
    assert!(right_click_a_link(cx, drawn[0]).is_some());
    cx.simulate_mouse_down(drawn[2], MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(drawn[2], MouseButton::Left, Modifiers::none());
    redraw(cx);
    assert_eq!(menu_open(cx), None, "a press outside closes it");
    assert!(cx.opened_url().is_none());
    assert!(right_click_a_link(cx, drawn[0]).is_some());
    let open = cx.debug_bounds("link-menu-open").expect("it offers Open link");
    cx.simulate_click(open.center(), Modifiers::none());
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.test/opened"));
    assert_eq!(menu_open(cx), None);
}

#[test]
fn a_right_click_on_a_link_over_a_selection_opens_the_link_s_menu_and_keeps_the_selection() {
    let (selection, messages, cx) = drawn();
    let drawn = paragraphs(cx);
    let end = drawn[1] + point(px(500.0), px(0.0));
    cx.simulate_mouse_down(drawn[0], MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
    assert!(right_click_a_link(cx, drawn[0]).is_some());
    assert!(selection.menu().is_none(), "the selection's own menu stays closed");
    assert_eq!(
        copied(&selection, &messages).as_deref(),
        Some("plain bold link x = 1\nsecond line")
    );
    // a right-click on the selected text under the link opens the selection's menu and closes the link's
    cx.simulate_mouse_down(drawn[1], MouseButton::Right, Modifiers::none());
    redraw(cx);
    assert!(selection.menu().is_some());
    assert_eq!(menu_open(cx), None);
}

/// A card's text, drawn without a selection (as `card_view` draws it).
struct Card {
    rendered: Arc<Rendered>,
}

impl Render for Card {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(600.0)).child(rich_text::render(
            &SharedString::from("card-text"),
            &self.rendered,
            LIGHT_PALETTE,
        ))
    }
}

#[test]
fn a_link_in_text_that_isn_t_selectable_has_the_menu_too_and_a_mention_none() {
    let rendered = laid_out(
        "<p><span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab B</span> \
         <a href=\"mailto:lab@example.test\">write</a></p>",
    );
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(move |_, _| Card { rendered });
    redraw(cx);
    let text = cx.debug_bounds("rich-link").expect("the text is drawn");
    let start = text.origin + point(px(2.0), px(4.0));
    // the mention comes first: a right-click on it opens nothing
    cx.simulate_mouse_down(start, MouseButton::Right, Modifiers::none());
    redraw(cx);
    assert_eq!(menu_open(cx), None, "a mention isn't a link");
    assert!(right_click_a_link(cx, start).is_some());
    assert_eq!(menu_open(cx).as_deref(), Some("mailto:lab@example.test"));
    let copy = cx.debug_bounds("link-menu-copy").expect("it offers Copy link");
    cx.simulate_click(copy.center(), Modifiers::none());
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("mailto:lab@example.test")
    );
}

#[test]
fn a_menu_opened_for_a_link_is_drawn_by_the_text_holding_it() {
    let (_, _, cx) = drawn();
    cx.update(|window, cx| open_link_menu("https://example.test/opened", window, cx));
    redraw(cx);
    assert!(cx.debug_bounds("link-menu-copy").is_some());
    cx.update(|_, cx| close_link_menu(cx));
    redraw(cx);
    assert!(cx.debug_bounds("link-menu-copy").is_none());
    // a link no text holds draws nothing
    cx.update(|window, cx| open_link_menu("https://example.test/elsewhere", window, cx));
    redraw(cx);
    assert!(cx.debug_bounds("link-menu").is_none());
}
