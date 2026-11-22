//! The compose box at the width of a narrow pane (the call's Chat pane, a narrow window), on GPUI's test context over
//! the fake engine (`common`): the text field keeps a usable width, the tools that do not fit go into the box's "More
//! options" menu, and Send stays in the box. Each tool in the menu keeps its action, its key and its state. A wide box
//! keeps all its tools. The user here shows the AI tools (Correct, Translate) on the toolbar, so that the menu holds
//! them when they do not fit. Every id, name and text is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::Rig;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_ui::ViewTrace;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::{Compose, Writes};
use teams_ui::settings::{ComposeTool, Settings};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// The width of the call's Chat pane, px: the box in it is the narrowest the app draws.
const PANE: f32 = 360.0;

/// The least width of the text field that a person can still type in and read, px.
const USABLE: f32 = 180.0;

/// The tools of a chat's box, in their order, Send apart.
const TOOLS: [&str; 7] = [
    "compose-format",
    "compose-emoji",
    "compose-attach",
    "compose-loop",
    "compose-apps",
    "compose-correct",
    "compose-translate",
];

/// The AI tools' rows, in a menu in place of their buttons.
const AI_ROWS: [&str; 3] = ["compose-correct", "compose-translate", "compose-language"];

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

/// The a–b chat in a window `width` px wide, its box focused.
fn chat(rig: &Rig, width: f32) -> (Entity<Compose>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    let mut settings = Settings::default();
    settings.show_compose_tool(ComposeTool::Correct, true);
    settings.show_compose_tool(ComposeTool::Translate, true);
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(width), px(640.0)));
    draw(cx);
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
    (compose, cx)
}

/// The selector of a tool's row in More options.
fn row(tool: &str) -> &'static str {
    Box::leak(format!("{tool}-row").into_boxed_str())
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

fn width(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
    f32::from(
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not shown"))
            .size
            .width,
    )
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not shown"))
        .center();
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
}

fn type_in(compose: &Entity<Compose>, cx: &mut VisualTestContext, text: &str) {
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, |editor, cx| editor.insert(text, cx));
        });
    });
    draw(cx);
}

#[test]
fn a_narrow_box_keeps_its_text_field_usable_and_its_tools_in_more() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    type_in(&compose, cx, "A made-up message to see the text field");

    let text = width(cx, "rich-editor");
    assert!(text >= USABLE, "the text field is {text} px wide");
    assert!(
        shown(cx, "compose-more"),
        "the tools that do not fit go into More options"
    );
    assert!(shown(cx, "compose-send"), "Send stays in the box");
    assert!(compose.read_with(cx, Compose::is_stacked));
    let hidden: Vec<_> = TOOLS.iter().filter(|tool| cx.debug_bounds(tool).is_none()).collect();
    assert!(!hidden.is_empty(), "some tools go into the menu");
    // the box stays inside its pane
    let compose_box = cx.debug_bounds("compose-box").unwrap();
    assert!(f32::from(compose_box.right()) <= PANE, "{compose_box:?}");
}

#[test]
fn a_wide_box_keeps_all_its_tools_and_no_more_button() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, 900.0);
    for tool in TOOLS {
        assert!(shown(cx, tool), "{tool} is in a wide box");
    }
    assert!(!compose.read_with(cx, Compose::is_stacked));
    assert!(!shown(cx, "compose-more"));
    assert!(shown(cx, "compose-send"));
}

#[test]
fn the_same_box_follows_its_own_width_both_ways() {
    let rig = Rig::start();
    let (_compose, cx) = chat(&rig, 900.0);
    cx.simulate_resize(size(px(PANE), px(640.0)));
    draw(cx);
    assert!(shown(cx, "compose-more"));
    assert!(width(cx, "rich-editor") >= USABLE);
    cx.simulate_resize(size(px(900.0), px(640.0)));
    draw(cx);
    assert!(!shown(cx, "compose-more"));
    for tool in TOOLS {
        assert!(shown(cx, tool), "{tool} comes back in a wide box");
    }
}

#[test]
fn the_menu_holds_each_tool_that_does_not_fit_and_its_rows_act() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    click(cx, "compose-more");
    assert!(shown(cx, "compose-more-menu"));
    for tool in TOOLS {
        let in_box = shown(cx, tool);
        let in_menu = cx.debug_bounds(row(tool)).is_some();
        assert!(in_box != in_menu, "{tool}: in the box {in_box}, in the menu {in_menu}");
    }
    for tool in ["compose-correct", "compose-translate", "compose-language"] {
        assert!(!shown(cx, tool), "{tool} is in the menu, not on the row");
    }
    for tool in AI_ROWS {
        assert!(shown(cx, row(tool)), "{tool} is a row of the menu");
    }
    // the language row opens the language menu, as its button does, and the click closes More options
    click(cx, "compose-language-row");
    assert!(!shown(cx, "compose-more-menu"), "a row's click closes the menu");
    assert!(shown(cx, "compose-language-menu"));
    assert!(!compose.read_with(cx, |compose, _| compose.more_open()));
}

#[test]
fn escape_closes_the_menu_and_gives_the_keyboard_back_to_the_text() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    click(cx, "compose-more");
    assert!(shown(cx, "compose-more-menu"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-more-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn the_keyboard_moves_in_the_menu_and_enter_takes_a_row() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    click(cx, "compose-more");
    // Up goes to the last row that can be taken: the translation language, which opens the language menu
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert!(!shown(cx, "compose-more-menu"));
    assert!(
        shown(cx, "compose-language-menu"),
        "Enter takes the row the keyboard chose"
    );
    // Escape closes the language menu and the keyboard goes back to the text
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-language-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn a_channel_box_moves_only_the_tools_it_has() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    compose.update(cx, |compose, cx| {
        compose.in_channel(Writes::Channel, "Message in A made-up channel", cx);
    });
    draw(cx);
    assert!(width(cx, "rich-editor") >= USABLE);
    // its text cannot keep a usable width beside its tools: it stacks, and its own tools are in More options
    assert!(compose.read_with(cx, Compose::is_stacked));
    assert!(shown(cx, "compose-more"), "a narrow channel box has More options");
    click(cx, "compose-more");
    assert!(shown(cx, "compose-more-menu"));
    for tool in AI_ROWS {
        assert!(shown(cx, row(tool)), "{tool} is in the menu of a channel's box");
    }
    for tool in ["compose-attach", "compose-loop"] {
        assert!(
            shown(cx, row(tool)) || shown(cx, tool),
            "{tool} is in a channel's box, as in Teams"
        );
    }
    assert!(
        shown(cx, "compose-apps"),
        "a channel's box has its \"+\", as Teams' does"
    );
}

#[test]
fn an_open_menu_follows_the_state_of_its_tools() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    type_in(&compose, cx, "A made-up draft");
    click(cx, "compose-more");
    // the draft goes while the menu is open: Correct and Translate have nothing to work on now. Down selects the first
    // row, and the next Down skips the rows that are off and selects the language row
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, teams_ui::compose::Editor::clear);
        });
    });
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.more_open()));
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    assert!(
        shown(cx, "compose-language-menu"),
        "the rows show the state of their tools now"
    );
}

#[test]
fn in_a_very_narrow_box_every_tool_is_in_the_menu_and_keeps_its_action_and_key() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, 150.0);
    for tool in TOOLS {
        assert!(!shown(cx, tool), "{tool} is in the menu");
    }
    assert!(shown(cx, "compose-more") && shown(cx, "compose-send"));

    // the keys work as before while their buttons are in the menu
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(!compose.read_with(cx, |compose, _| compose.is_expanded()));

    // each row does what its button does (by the keyboard: the menu is wider than this window)
    click(cx, "compose-more");
    assert!(shown(cx, "compose-format-row"));
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
    click(cx, "compose-more");
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()));
    assert!(compose.read_with(cx, |compose, _| !compose.more_open()));
}

fn right(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
    f32::from(
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not shown"))
            .right(),
    )
}

#[test]
fn the_format_bar_fits_a_narrow_box_and_its_other_tools_are_in_its_more_options() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    type_in(&compose, cx, "A made-up draft");
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
    let inside = right(cx, "compose-box");
    for tool in ["format-bold", "format-highlight", "format-more", "format-discard"] {
        let at = right(cx, tool);
        assert!(at <= inside, "{tool} ends at {at} px, past the box at {inside} px");
    }
    assert!(
        !shown(cx, "format-code"),
        "Code does not fit the bar: it is in its More options"
    );

    // the tools that do not fit are rows of the bar's More options, and each row does what its button does
    click(cx, "format-more");
    for tool in [
        "format-colour",
        "format-size",
        "format-quote",
        "format-link",
        "format-code",
    ] {
        assert!(shown(cx, row(tool)), "{tool} is a row of the bar's More options");
    }
    for tool in ["format-bold", "format-highlight"] {
        assert!(!shown(cx, row(tool)), "{tool} is on the bar: it has no row");
    }
    cx.simulate_keystrokes("end");
    click(cx, "format-code-row");
    let code = compose.read_with(cx, |compose, cx| {
        compose.editor().read(cx).has_mark(teams_proto::chat::Marks::CODE)
            || compose.editor().read(cx).block_kind() == teams_ui::editor::doc::Kind::Code
    });
    assert!(code, "the Code row turns code on");
    // a row of a tool with a menu opens that menu by the bar's More options, inside the window
    click(cx, "format-more");
    click(cx, "format-link-row");
    assert!(shown(cx, "format-link-insert"), "the Link row opens the link dialog");
    let menu = cx.debug_bounds("format-menu").unwrap();
    assert!(f32::from(menu.right()) <= PANE, "{menu:?}");
}

#[test]
fn a_wide_format_bar_keeps_every_tool() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, 900.0);
    type_in(&compose, cx, "A made-up draft");
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    for tool in [
        "format-bold",
        "format-highlight",
        "format-link",
        "format-code",
        "format-more",
        "format-discard",
    ] {
        assert!(shown(cx, tool), "{tool} is on a wide bar");
    }
    click(cx, "format-more");
    assert!(!shown(cx, "format-code-row"));
}

#[test]
fn a_lit_tool_s_row_in_more_options_shows_a_check() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, 150.0);
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
    click(cx, "compose-more");
    assert!(shown(cx, "compose-format-row"));
    assert!(
        shown(cx, "compose-format-check"),
        "Format is lit: its row shows a check"
    );
    assert!(
        !shown(cx, "compose-emoji-check"),
        "Emoji is not lit: its row shows no check"
    );
}

#[test]
fn tab_from_the_text_reaches_no_hidden_stop() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    type_in(&compose, cx, "A made-up draft");
    // the stops after the text: Format, Emoji, Attach, "+", More options, then Send; the language is in More options,
    // so Enter on no stop opens the language menu
    for stops in 6..=7 {
        for _ in 0..stops {
            cx.update(gpui::Window::focus_next);
            draw(cx);
        }
        cx.simulate_keystrokes("enter");
        draw(cx);
        assert!(
            !shown(cx, "compose-language-menu"),
            "stop {stops} is a hidden one: it opened the language menu"
        );
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
        draw(cx);
    }
    // the sixth stop is Send: Enter on it sends (a focused control clicks as the key goes up)
    for _ in 0..6 {
        cx.update(gpui::Window::focus_next);
        draw(cx);
    }
    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    draw(cx);
    assert!(
        compose.read_with(cx, |compose, cx| compose.value(cx).is_empty()),
        "Enter on the sixth stop sends"
    );
}

#[test]
fn a_click_on_send_keeps_the_keyboard_in_the_text() {
    for width in [PANE, 900.0] {
        let rig = Rig::start();
        let (compose, cx) = chat(&rig, width);
        type_in(&compose, cx, "A made-up message");
        click(cx, "compose-send");
        assert!(
            compose.read_with(cx, |compose, cx| compose.value(cx).is_empty()),
            "the message went"
        );
        assert!(
            cx.update(|window, cx| compose.read(cx).focused(window, cx)),
            "a {width} px box keeps the keyboard in its text after a click on Send"
        );
    }
}

#[test]
fn a_reply_to_a_long_message_shows_a_short_quote() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    let quoted = teams_ui::compose::Quoted {
        message_id: "1700000000000".into(),
        sender: "8:orgid:00000000-0000-4000-8000-00000000000b".into(),
        sender_name: "Lab Bee".into(),
        time: "1700000000000".into(),
        preview: "A made-up long message that goes on and on. "
            .repeat(6)
            .trim()
            .to_owned(),
        shown: None,
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.reply_to(quoted, window, cx)));
    draw(cx);
    let quote = cx.debug_bounds("compose-mode").expect("the reply's quote shows");
    // two lines of small text at most
    assert!(f32::from(quote.size.height) <= 40.0, "the quote is {quote:?}");
    assert!(f32::from(cx.debug_bounds("compose-box").unwrap().size.width) <= PANE);
}
