//! The message box's toolbar, less crowded, on GPUI's test context over the fake engine (`common`): no AI button; the
//! AI tools (Correct, Translate and its language) are rows of "+" until the user shows them, and buttons the user
//! shows or hides, by a right click on the toolbar or "Customize toolbar" in "+", or in Settings. A hidden button's
//! action stays in "+", with its key. Send and "+" cannot hide. With the AI features off, nothing of AI shows. The
//! keys move as in Teams: the text, a staged file's buttons, the tools as one stop of Tab (Left and Right between
//! them), then Send; expanded, the format bar and Discard come before the text. Every id, name and text is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, px, size};
use teams_ui::ViewTrace;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::{Compose, MenuKind, Writes};
use teams_ui::local_ai::Tools;
use teams_ui::settings::{ComposeTool, Settings, SettingsModel};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// The AI tools' rows in "+".
const AI_ROWS: [&str; 3] = ["compose-correct-row", "compose-translate-row", "compose-language-row"];

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

struct Opened {
    compose: Entity<Compose>,
    settings: Entity<SettingsModel>,
    cx: &'static mut VisualTestContext,
}

/// The a–b chat in a window `width` px wide over `settings`, its box focused.
fn chat_with(rig: &Rig, settings: Settings, width: f32) -> Opened {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let model = parts.settings.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(width), px(640.0)));
    draw(cx);
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
    Opened {
        compose,
        settings: model,
        cx,
    }
}

fn hiding(tools: &[ComposeTool]) -> Settings {
    Settings {
        compose_hidden: tools.to_vec(),
        ..Settings::default()
    }
}

fn showing_ai() -> Settings {
    Settings {
        compose_shown: vec![ComposeTool::Correct, ComposeTool::Translate],
        ..Settings::default()
    }
}

fn shown_tools(settings: &Entity<SettingsModel>, cx: &mut VisualTestContext) -> Vec<ComposeTool> {
    settings.read_with(cx, |model, _| model.settings().compose_shown.clone())
}

/// Whether the customize list's row of each of `tools` shows its check.
fn checks(cx: &mut VisualTestContext, tools: &[&str]) -> Vec<bool> {
    tools
        .iter()
        .map(|tool| {
            cx.debug_bounds(Box::leak(format!("compose-customize-{tool}-check").into_boxed_str()))
                .is_some()
        })
        .collect()
}

fn top(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
    f32::from(
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not shown"))
            .top(),
    )
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
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

fn right_click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not shown"))
        .center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    draw(cx);
}

fn hidden_tools(settings: &Entity<SettingsModel>, cx: &mut VisualTestContext) -> Vec<ComposeTool> {
    settings.read_with(cx, |model, _| model.settings().compose_hidden.clone())
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
fn there_is_no_ai_button_and_the_ai_tools_are_rows_of_plus() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, Settings::default(), 900.0);
    for gone in ["compose-ai", "compose-correct", "compose-translate", "compose-language"] {
        assert!(!shown(cx, gone), "{gone} is not on the bar");
    }
    type_in(&compose, cx, "A made-up draft");
    click(cx, "compose-apps");
    assert!(shown(cx, "compose-apps-menu"));
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    for row in AI_ROWS {
        assert!(shown(cx, row), "{row} is in \"+\"");
    }
    for key in ["compose-correct-row-key", "compose-translate-row-key"] {
        assert!(shown(cx, key), "{key} shows in \"+\"");
    }
    // the AI tools come before Customize toolbar, at the foot
    let order = AI_ROWS
        .into_iter()
        .chain(["compose-customize-row"])
        .map(|row| top(cx, row));
    let order: Vec<f32> = order.collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
    // the language row opens the language menu by "+", and "+" closes
    click(cx, "compose-language-row");
    assert!(!shown(cx, "compose-apps-menu"));
    assert!(shown(cx, "compose-language-menu"));
    let plus = cx.debug_bounds("compose-apps").unwrap();
    let menu = cx.debug_bounds("compose-language-menu").unwrap();
    assert!(
        menu.bottom() <= plus.top() + px(1.0),
        "the menu opens over the button: {menu:?}, {plus:?}"
    );
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-language-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn the_ai_tools_come_after_teams_own_rows_in_plus() {
    let rig = Rig::start();
    let Opened { cx, .. } = chat_with(&rig, hiding(&[ComposeTool::Format, ComposeTool::Emoji]), 900.0);
    click(cx, "compose-apps");
    let rows = [
        "compose-format-row",
        "compose-emoji-row",
        "compose-correct-row",
        "compose-translate-row",
        "compose-language-row",
        "compose-customize-row",
    ];
    let order: Vec<f32> = rows.into_iter().map(|row| top(cx, row)).collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{order:?}");
}

#[test]
fn customize_shows_and_hides_each_ai_tool_on_the_toolbar() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, Settings::default(), 900.0);
    right_click(cx, "compose-attach");
    assert_eq!(
        checks(cx, &["correct", "translate"]),
        [false, false],
        "the AI tools are in \"+\" by default: their rows are unchecked"
    );
    // Correct on the bar: its button shows, its row leaves "+"
    click(cx, "compose-customize-correct");
    assert_eq!(shown_tools(&settings, cx), [ComposeTool::Correct]);
    assert!(shown(cx, "compose-customize-correct-check"));
    assert!(shown(cx, "compose-correct") && !shown(cx, "compose-translate"));
    // Translate on the bar too, with its language's chevron
    click(cx, "compose-customize-translate");
    assert_eq!(
        shown_tools(&settings, cx),
        [ComposeTool::Correct, ComposeTool::Translate]
    );
    assert!(shown(cx, "compose-translate") && shown(cx, "compose-language"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    click(cx, "compose-apps");
    for row in AI_ROWS {
        assert!(!shown(cx, row), "{row} has a button on the bar: no row in \"+\"");
    }
    assert!(shown(cx, "compose-customize-row"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    // the AI tools sit after "+", before Send
    let plus = cx.debug_bounds("compose-apps").unwrap().right();
    let correct = cx.debug_bounds("compose-correct").unwrap();
    let send = cx.debug_bounds("compose-send").unwrap().left();
    assert!(correct.left() >= plus && correct.right() <= send);
    // and back into "+"
    right_click(cx, "compose-attach");
    click(cx, "compose-customize-translate");
    click(cx, "compose-customize-correct");
    assert!(shown_tools(&settings, cx).is_empty());
    assert!(!shown(cx, "compose-correct") && !shown(cx, "compose-translate") && !shown(cx, "compose-language"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn a_shown_translate_reaches_its_language_by_its_chevron() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, showing_ai(), 900.0);
    type_in(&compose, cx, "A made-up draft");
    let translate = cx.debug_bounds("compose-translate").unwrap();
    let chevron = cx.debug_bounds("compose-language").unwrap();
    assert!(
        chevron.left() >= translate.right() - px(1.0),
        "the chevron is beside Translate"
    );
    click(cx, "compose-language");
    assert!(shown(cx, "compose-language-menu"));
    let menu = cx.debug_bounds("compose-language-menu").unwrap();
    assert!(
        menu.bottom() <= chevron.top() + px(1.0),
        "the menu opens over the chevron: {menu:?}, {chevron:?}"
    );
    // the search field has the keyboard: a language typed, then Enter
    cx.simulate_input("German");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(!shown(cx, "compose-language-menu"));
    let languages = settings.read_with(cx, |model, _| {
        let settings = model.settings();
        (
            settings.draft_translate_to.clone(),
            settings.message_translate_to.clone(),
        )
    });
    assert_eq!(languages, ("German".to_owned(), "English".to_owned()));
    // "+" has no language row while Translate is on the bar
    click(cx, "compose-apps");
    assert!(!shown(cx, "compose-language-row"));
}

#[test]
fn the_ai_keys_reach_the_box_whether_their_buttons_show_or_not() {
    for settings in [Settings::default(), showing_ai()] {
        let rig = Rig::start();
        // no AI tool is installed, so that nothing runs
        rig.cx.update(|cx| cx.set_global(Tools::default()));
        let Opened { compose, cx, .. } = chat_with(&rig, settings, 900.0);
        type_in(&compose, cx, "A made-up draft");
        // no tool is installed here: the key's run says so, which shows that the key reached the box
        cx.simulate_keystrokes("ctrl-shift-y");
        draw(cx);
        assert!(
            !matches!(
                compose.read_with(cx, |compose, _| compose.draft_translation_status()),
                teams_ui::compose::DraftTranslationStatus::Idle
            ),
            "Ctrl+Shift+Y started a translation"
        );
        cx.simulate_keystrokes("ctrl-shift-g");
        draw(cx);
        assert!(
            !matches!(
                compose.read_with(cx, |compose, _| compose.correction_status()),
                teams_ui::compose::CorrectionStatus::Idle
            ),
            "Ctrl+Shift+G started a correction"
        );
    }
}

#[test]
fn a_right_click_on_the_toolbar_lists_its_buttons_and_a_click_hides_one() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, Settings::default(), 900.0);
    right_click(cx, "compose-attach");
    assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
    assert!(shown(cx, "compose-customize-menu"));
    // the buttons that show are checked, the AI tools in "+" are not
    assert_eq!(
        checks(
            cx,
            &[
                "format",
                "emoji",
                "attach",
                "loop",
                "apps",
                "send",
                "correct",
                "translate"
            ]
        ),
        [true, true, true, true, true, true, false, false]
    );
    click(cx, "compose-customize-emoji");
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Emoji]);
    assert!(!shown(cx, "compose-emoji"), "the button leaves the bar");
    // the list stays open, the row unchecked; a second click brings the button back
    assert!(shown(cx, "compose-customize-menu"));
    assert!(!shown(cx, "compose-customize-emoji-check"));
    click(cx, "compose-customize-emoji");
    assert!(hidden_tools(&settings, cx).is_empty());
    assert!(shown(cx, "compose-emoji"));
    // Send and "+" cannot hide
    click(cx, "compose-customize-send");
    click(cx, "compose-customize-apps");
    assert!(hidden_tools(&settings, cx).is_empty());
    assert!(shown(cx, "compose-send") && shown(cx, "compose-apps"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-customize-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn a_right_click_opens_the_list_over_the_toolbar_s_buttons() {
    let rig = Rig::start();
    let Opened { cx, .. } = chat_with(&rig, showing_ai(), 1280.0);
    right_click(cx, "compose-emoji");
    let list = cx.debug_bounds("compose-customize-menu").unwrap();
    for tool in ["compose-format", "compose-emoji", "compose-translate", "compose-send"] {
        let button = cx.debug_bounds(tool).unwrap();
        assert!(
            list.bottom() <= button.top() + px(1.0),
            "the list leaves {tool} visible: {list:?}, {button:?}"
        );
    }
}

#[test]
fn the_list_s_keys_pass_over_its_title_and_go_round() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, Settings::default(), 900.0);
    right_click(cx, "compose-attach");
    // the first Down selects Format, not the title
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Format]);
    assert!(shown(cx, "compose-customize-menu"), "the list stays open");
    // the row stays selected: Enter shows Format again
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(hidden_tools(&settings, cx).is_empty());
    // Up from Format goes round to Translate, the last row that can show or hide; Down from it to Format
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert_eq!(shown_tools(&settings, cx), [ComposeTool::Translate]);
    assert!(hidden_tools(&settings, cx).is_empty());
    cx.simulate_keystrokes("enter down enter");
    draw(cx);
    assert!(shown_tools(&settings, cx).is_empty());
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Format]);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!compose.read_with(cx, |compose, _| compose.customize_open()));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn customize_toolbar_in_plus_opens_the_same_list() {
    let rig = Rig::start();
    let Opened { settings, cx, .. } = chat_with(&rig, Settings::default(), 900.0);
    click(cx, "compose-apps");
    assert!(shown(cx, "compose-apps-menu"), "\"+\" opens its menu");
    click(cx, "compose-customize-row");
    assert!(shown(cx, "compose-customize-menu"));
    click(cx, "compose-customize-format");
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Format]);
    assert!(!shown(cx, "compose-format"));
}

#[test]
fn a_hidden_button_s_action_stays_in_plus_with_its_key() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, hiding(&[ComposeTool::Emoji, ComposeTool::Format]), 900.0);
    for gone in [
        "compose-emoji",
        "compose-format",
        "compose-correct",
        "compose-translate",
    ] {
        assert!(!shown(cx, gone), "{gone} is hidden");
    }
    assert!(shown(cx, "compose-attach") && shown(cx, "compose-send"));
    click(cx, "compose-apps");
    for row in [
        "compose-format-row",
        "compose-emoji-row",
        "compose-correct-row",
        "compose-translate-row",
        "compose-language-row",
        "compose-customize-row",
    ] {
        assert!(shown(cx, row), "{row} is in \"+\"");
    }
    // each row of an action with a key shows that key
    for key in [
        "compose-format-row-key",
        "compose-emoji-row-key",
        "compose-correct-row-key",
        "compose-translate-row-key",
    ] {
        assert!(shown(cx, key), "{key} shows in \"+\"");
    }
    assert!(
        !shown(cx, "compose-attach-row"),
        "a button on the bar has no row in \"+\""
    );
    // a row does what its button did: the emoji picker opens by "+"
    click(cx, "compose-emoji-row");
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()));
    cx.simulate_keystrokes("escape");
    draw(cx);
    // the keys work as before while their buttons are hidden
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
}

#[test]
fn the_choice_applies_to_every_box() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, hiding(&[ComposeTool::Format]), 900.0);
    assert!(!shown(cx, "compose-format"));
    // an edit's box
    let loaded = teams_ui::compose::Loaded {
        doc: teams_ui::editor::doc::Doc::new(vec![teams_ui::editor::doc::Block::paragraph("hello")]),
        quotes: Vec::new(),
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1".to_owned(), loaded, window, cx)));
    draw(cx);
    assert!(shown(cx, "compose-mode"));
    assert!(!shown(cx, "compose-format"));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.cancel(window, cx)));
    draw(cx);
    // a channel's post and a thread's reply
    for writes in [Writes::Channel, Writes::Thread] {
        compose.update(cx, |compose, cx| {
            compose.in_channel(writes, "Message in A made-up channel", cx);
        });
        draw(cx);
        assert!(!shown(cx, "compose-format"), "{writes:?}");
        assert!(shown(cx, "compose-apps"), "{writes:?}: its \"+\" holds Format");
        click(cx, "compose-apps");
        assert!(shown(cx, "compose-format-row"), "{writes:?}");
        cx.simulate_keystrokes("escape");
        draw(cx);
    }
}

#[test]
fn a_channel_s_boxes_show_the_shown_ai_tools_and_keep_them_out_of_plus() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, showing_ai(), 900.0);
    let layout = |compose: &Entity<Compose>, cx: &mut VisualTestContext| {
        compose.read_with(cx, |compose, cx| {
            (compose.row_tools(cx), compose.menu_selectors(MenuKind::Plus, cx))
        })
    };
    let (row, plus) = layout(&compose, cx);
    assert!(
        row.ends_with(&["compose-apps", "compose-correct", "compose-translate"]),
        "{row:?}"
    );
    assert_eq!(plus, ["compose-customize"]);
    for writes in [Writes::Channel, Writes::Thread] {
        compose.update(cx, |compose, cx| {
            compose.in_channel(writes, "Message in A made-up channel", cx);
        });
        draw(cx);
        let (row, plus) = layout(&compose, cx);
        assert_eq!(
            row,
            [
                "compose-format",
                "compose-emoji",
                "compose-attach",
                "compose-loop",
                "compose-apps",
                "compose-correct",
                "compose-translate"
            ],
            "{writes:?}"
        );
        assert_eq!(plus, ["compose-customize"], "{writes:?}");
    }
    let settings = compose.read_with(cx, |compose, _| compose.settings().clone());
    cx.update(|_, cx| {
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.compose_shown.clear(), cx);
        });
    });
    draw(cx);
    let (row, plus) = layout(&compose, cx);
    assert_eq!(
        row,
        [
            "compose-format",
            "compose-emoji",
            "compose-attach",
            "compose-loop",
            "compose-apps"
        ]
    );
    assert_eq!(
        plus,
        [
            "compose-correct",
            "compose-translate",
            "compose-language",
            "compose-customize"
        ]
    );
}

#[test]
fn with_the_ai_features_off_nothing_of_ai_shows() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, showing_ai(), 900.0);
    assert!(shown(cx, "compose-correct") && shown(cx, "compose-translate"));
    cx.update(|_, cx| settings.update(cx, |model, cx| model.set_ai_features(false, cx)));
    draw(cx);
    for gone in ["compose-correct", "compose-translate", "compose-language"] {
        assert!(
            !shown(cx, gone),
            "{gone} is not on the bar while the AI features are off"
        );
    }
    click(cx, "compose-apps");
    for row in AI_ROWS {
        assert!(!shown(cx, row), "{row} is not in \"+\" while the AI features are off");
    }
    cx.simulate_keystrokes("escape");
    draw(cx);
    right_click(cx, "compose-attach");
    assert!(shown(cx, "compose-customize-menu"));
    for gone in ["compose-customize-correct", "compose-customize-translate"] {
        assert!(!shown(cx, gone), "no AI row in the customize list");
    }
    cx.simulate_keystrokes("escape");
    draw(cx);
    // on again: the AI tools are where the user put them, on the bar
    cx.update(|_, cx| settings.update(cx, |model, cx| model.set_ai_features(true, cx)));
    draw(cx);
    assert!(shown(cx, "compose-correct") && shown(cx, "compose-translate"));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    click(cx, "compose-apps");
    assert!(!shown(cx, "compose-correct-row"));
}

#[test]
fn a_very_narrow_box_puts_the_hidden_actions_in_more_options() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, hiding(&[ComposeTool::Emoji]), 150.0);
    assert!(!shown(cx, "compose-apps"), "\"+\" is in More options too");
    click(cx, "compose-more");
    assert!(shown(cx, "compose-emoji-row") && shown(cx, "compose-customize-row"));
    // the menu is wider than this window: the keyboard takes the last row, Customize toolbar
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
}

/// The a–b chat in a window `width` px wide under the kit's root, which moves the focus on Tab and Shift+Tab; its box
/// focused.
fn rooted_chat(rig: &Rig, width: f32) -> (Entity<Compose>, &'static mut VisualTestContext) {
    rooted_chat_with(rig, Settings::default(), width)
}

/// As [`rooted_chat`], with `settings`.
fn rooted_chat_with(rig: &Rig, settings: Settings, width: f32) -> (Entity<Compose>, &'static mut VisualTestContext) {
    let (_, compose, cx) = rooted_view(rig, settings, width);
    (compose, cx)
}

/// As [`rooted_chat_with`], with the chat view.
fn rooted_view(
    rig: &Rig,
    settings: Settings,
    width: f32,
) -> (Entity<ChatView>, Entity<Compose>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
        teams_ui::window::KitRoot::new(view, window, cx)
    });
    let view = cx.update(|_, cx| root.read(cx).view().clone().downcast::<ChatView>().unwrap());
    cx.simulate_resize(size(px(width), px(640.0)));
    draw(cx);
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    focus_box(&compose, cx);
    (view, compose, cx)
}

fn focus_box(compose: &Entity<Compose>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
}

fn box_focused(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| compose.read(cx).focused(window, cx))
}

/// Enter on the control the keys are on: it clicks as the key goes up.
fn key_click(cx: &mut VisualTestContext) {
    cx.simulate_keystrokes("enter");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    draw(cx);
}

fn list_focused(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| view.read(cx).focus_handle().is_focused(window))
}

fn tools_focused(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| compose.read(cx).tools_focused(window, cx))
}

fn focused(cx: &mut VisualTestContext) -> gpui::FocusHandle {
    cx.update(|window, cx| window.focused(cx))
        .expect("something has the keys")
}

fn press(cx: &mut VisualTestContext, key: &str) {
    cx.simulate_keystrokes(key);
    draw(cx);
}

/// The stops `key` passes from the box until it is in the box again, the box last; at most `limit`.
fn round_by(key: &str, compose: &Entity<Compose>, cx: &mut VisualTestContext, limit: usize) -> Vec<gpui::FocusHandle> {
    let mut stops = Vec::new();
    for _ in 0..limit {
        press(cx, key);
        stops.push(focused(cx));
        if box_focused(compose, cx) {
            break;
        }
    }
    stops
}

/// Each box the app draws: a chat's, a channel's post, a thread's reply, and a stacked one in a narrow window.
fn each_box(test: impl Fn(&Entity<Compose>, &mut VisualTestContext, &str)) {
    each_box_in(|_, compose, cx, which| test(compose, cx, which));
}

/// As [`each_box`], with the chat view the box is in.
fn each_box_in(test: impl Fn(&Entity<ChatView>, &Entity<Compose>, &mut VisualTestContext, &str)) {
    for (width, writes) in [
        (900.0, None),
        (900.0, Some(Writes::Channel)),
        (900.0, Some(Writes::Thread)),
        (360.0, None),
    ] {
        let rig = Rig::start();
        let (view, compose, cx) = rooted_view(&rig, Settings::default(), width);
        if let Some(writes) = writes {
            compose.update(cx, |compose, cx| {
                compose.in_channel(writes, "Message in A made-up channel", cx);
            });
            draw(cx);
            focus_box(&compose, cx);
        }
        let stacked = compose.read_with(cx, Compose::is_stacked);
        assert_eq!(stacked, width < 900.0, "{width} px");
        test(&view, &compose, cx, &format!("{writes:?} at {width} px"));
    }
}

#[test]
fn shift_tab_from_the_tools_gives_the_box_the_keys_back() {
    each_box(|compose, cx, which| {
        type_in(compose, cx, "a draft");
        press(cx, "tab");
        assert!(
            tools_focused(compose, cx),
            "{which}: Tab goes from the text to the tools"
        );
        press(cx, "shift-tab");
        assert!(box_focused(compose, cx), "{which}: Shift+Tab comes back to the text");
        assert_eq!(
            compose.read_with(cx, |compose, cx| compose.value(cx).to_string()),
            "a draft",
            "{which}"
        );
    });
}

#[test]
fn tab_and_shift_tab_go_round_the_same_stops_the_other_way() {
    each_box(|compose, cx, which| {
        // a draft lights Send: greyed, it is no stop
        type_in(compose, cx, "a draft");
        let start = focused(cx);
        let ahead = round_by("tab", compose, cx, 200);
        assert_eq!(ahead.last(), Some(&start), "{which}: Tab comes round to the text again");
        let back = round_by("shift-tab", compose, cx, 200);
        assert_eq!(
            back.last(),
            Some(&start),
            "{which}: Shift+Tab comes round to the text again"
        );
        let mut reversed = ahead[..ahead.len() - 1].to_vec();
        reversed.reverse();
        assert_eq!(
            &back[..back.len() - 1],
            &reversed[..],
            "{which}: the same stops, the other way"
        );
    });
}

#[test]
fn the_tools_are_one_stop_and_the_arrows_move_between_them() {
    each_box(|compose, cx, which| {
        type_in(compose, cx, "a draft");
        press(cx, "tab");
        let first = focused(cx);
        assert!(tools_focused(compose, cx), "{which}");
        // Left at the first tool stays
        press(cx, "left");
        assert_eq!(focused(cx), first, "{which}: Left stops at the first tool");
        // Tab passes the other tools: one Tab to Send, and Shift+Tab back to the same tool
        press(cx, "tab");
        assert!(
            !tools_focused(compose, cx) && !box_focused(compose, cx),
            "{which}: Send"
        );
        let send = focused(cx);
        press(cx, "shift-tab");
        assert_eq!(
            focused(cx),
            first,
            "{which}: Shift+Tab from Send to the tool the keys were on"
        );
        // Right moves; the tools keep the one the keys were last on
        press(cx, "right");
        let second = focused(cx);
        assert!(
            second != first && tools_focused(compose, cx),
            "{which}: Right goes to the next tool"
        );
        press(cx, "tab");
        assert_eq!(focused(cx), send, "{which}");
        press(cx, "shift-tab");
        assert_eq!(focused(cx), second, "{which}: back on the second tool");
        press(cx, "shift-tab");
        assert!(box_focused(compose, cx), "{which}");
        press(cx, "tab");
        assert_eq!(
            focused(cx),
            second,
            "{which}: Tab from the text lands on the second tool too"
        );
        // End and Right at the last; Home to the first
        press(cx, "end");
        let last = focused(cx);
        assert!(last != second && tools_focused(compose, cx), "{which}: End");
        press(cx, "right");
        assert_eq!(focused(cx), last, "{which}: Right stops at the last tool");
        press(cx, "home");
        assert_eq!(focused(cx), first, "{which}: Home");
        press(cx, "shift-tab");
        assert!(box_focused(compose, cx), "{which}");
    });
}

#[test]
fn a_tool_s_menu_keeps_its_own_arrows_and_gives_the_keys_back_to_its_button() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    press(cx, "tab");
    press(cx, "end");
    let plus = focused(cx);
    press(cx, "enter");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    press(cx, "down");
    press(cx, "right");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus),
        "the menu's own keys"
    );
    press(cx, "escape");
    assert_eq!(compose.read_with(cx, |compose, _| compose.menu_open()), None);
    assert_eq!(focused(cx), plus, "Escape gives the keys back to \"+\"");
    press(cx, "shift-tab");
    assert!(box_focused(&compose, cx));
}

#[test]
fn formatting_taken_in_plus_leaves_the_keys_in_the_text() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat_with(&rig, hiding(&[ComposeTool::Format]), 900.0);
    press(cx, "tab");
    press(cx, "end");
    press(cx, "enter");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    click(cx, "compose-format-row");
    assert!(compose.read_with(cx, |compose, _| compose.menu_open().is_none()));
    assert!(
        box_focused(&compose, cx),
        "the format bar opens for the text, which keeps the keys"
    );
}

#[test]
fn the_format_bar_and_discard_come_before_the_text() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_expanded(window, cx)));
    draw(cx);
    focus_box(&compose, cx);
    press(cx, "shift-tab");
    let discard = focused(cx);
    assert!(!box_focused(&compose, cx), "Shift+Tab from the text goes to Discard");
    press(cx, "shift-tab");
    let bold = focused(cx);
    assert!(bold != discard, "then to the bar's tools");
    press(cx, "right");
    let italic = focused(cx);
    assert!(italic != bold, "the bar's arrows");
    press(cx, "tab");
    assert_eq!(focused(cx), discard, "Tab passes the bar's other tools");
    press(cx, "shift-tab");
    assert_eq!(focused(cx), italic, "the bar keeps the tool the keys were on");
    press(cx, "tab");
    press(cx, "tab");
    assert!(box_focused(&compose, cx), "Discard, then the text");
    press(cx, "tab");
    assert!(tools_focused(&compose, cx), "then the box's tools");
    press(cx, "right");
    press(cx, "right");
    let third = focused(cx);
    press(cx, "shift-tab");
    assert!(box_focused(&compose, cx), "Shift+Tab from the tools, the text");
    press(cx, "tab");
    assert_eq!(
        focused(cx),
        third,
        "Tab from the text of an open bar, the tool the keys were on"
    );
}

#[test]
fn a_staged_file_s_buttons_come_between_the_text_and_the_tools() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("made-up-notes.txt");
    std::fs::write(&file, "made-up notes").unwrap();
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.attach_files(&[file], window, cx)));
    draw(cx);
    assert_eq!(compose.read_with(cx, |compose, _| compose.attachments().len()), 1);
    focus_box(&compose, cx);
    press(cx, "tab");
    assert!(
        !box_focused(&compose, cx) && !tools_focused(&compose, cx),
        "Tab goes to the file first"
    );
    press(cx, "shift-tab");
    assert!(
        box_focused(&compose, cx),
        "Shift+Tab from the file gives the text the keys"
    );
    let mut stops = 0;
    while !tools_focused(&compose, cx) && stops < 10 {
        press(cx, "tab");
        stops += 1;
    }
    assert!(tools_focused(&compose, cx), "after the file's buttons, the tools");
    for _ in 0..stops {
        press(cx, "shift-tab");
    }
    assert!(box_focused(&compose, cx), "and back the same way");
    for _ in 0..stops {
        press(cx, "tab");
    }
    press(cx, "right");
    let second = focused(cx);
    press(cx, "shift-tab");
    assert!(!tools_focused(&compose, cx), "Shift+Tab leaves the tools for the file");
    press(cx, "tab");
    assert_eq!(focused(cx), second, "Tab from the file, the tool the keys were on");
    press(cx, "left");
    let first = focused(cx);
    press(cx, "tab");
    assert!(!tools_focused(&compose, cx), "Tab from the tools, Send");
    press(cx, "shift-tab");
    assert_eq!(focused(cx), first, "Shift+Tab from Send, the tool the keys were on");
}

#[test]
fn shift_tab_from_the_text_goes_to_the_message_list_and_tab_comes_back() {
    each_box_in(|view, compose, cx, which| {
        type_in(compose, cx, "a draft");
        press(cx, "shift-tab");
        assert!(
            list_focused(view, cx),
            "{which}: Shift+Tab from the text, the message list"
        );
        assert!(
            cx.debug_bounds("chat-messages-ring").is_some(),
            "{which}: the list ringed"
        );
        cx.simulate_mouse_move(gpui::point(px(3.0), px(3.0)), None, Modifiers::none());
        draw(cx);
        assert!(
            cx.debug_bounds("chat-messages-ring").is_some(),
            "{which}: the ring stays when the pointer moves"
        );
        press(cx, "tab");
        assert!(cx.debug_bounds("chat-messages-ring").is_none(), "{which}: no ring");
        assert!(box_focused(compose, cx), "{which}: Tab from the list, the text");
        press(cx, "shift-tab");
        press(cx, "shift-tab");
        assert!(
            !list_focused(view, cx) && !box_focused(compose, cx),
            "{which}: Shift+Tab from the list leaves it"
        );
    });
}

#[test]
fn shift_tab_from_an_open_format_bar_goes_to_the_message_list() {
    let rig = Rig::start();
    let (view, compose, cx) = rooted_view(&rig, Settings::default(), 900.0);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_expanded(window, cx)));
    draw(cx);
    focus_box(&compose, cx);
    press(cx, "shift-tab");
    press(cx, "shift-tab");
    assert!(!list_focused(&view, cx), "Discard, then the format bar");
    press(cx, "shift-tab");
    assert!(list_focused(&view, cx), "then the message list");
    assert!(cx.debug_bounds("chat-messages-ring").is_some());
    click(cx, "chat-messages");
    assert!(
        list_focused(&view, cx) && cx.debug_bounds("chat-messages-ring").is_none(),
        "a press in the list takes its ring off"
    );
    press(cx, "tab");
    press(cx, "tab");
    press(cx, "tab");
    assert!(box_focused(&compose, cx), "the bar, Discard, then the text");
}

#[test]
fn escape_in_the_emoji_picker_gives_the_keys_back_to_emoji() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    type_in(&compose, cx, "a draft");
    press(cx, "tab");
    press(cx, "right");
    let emoji = focused(cx);
    key_click(cx);
    let picker = compose.read_with(cx, |compose, _| compose.emoji_picker().cloned());
    assert!(picker.is_some(), "Enter on Emoji opens the picker");
    press(cx, "escape");
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_none()));
    assert_eq!(focused(cx), emoji, "Escape gives the keys back to Emoji");
    // a pick puts the emoji in the text, the keys with it
    key_click(cx);
    let picker = compose
        .read_with(cx, |compose, _| compose.emoji_picker().cloned())
        .expect("the picker opens again");
    let first = cx.update(|_, cx| picker.read(cx).rows(cx)[1][0].selector());
    click(cx, first.leak());
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_none()));
    assert!(box_focused(&compose, cx), "a pick gives the text the keys");
    let text = compose.read_with(cx, |compose, cx| compose.value(cx).to_string());
    assert!(text.starts_with("a draft") && text.len() > "a draft".len(), "{text}");
}

/// `count` made-up messages from b in the a–b chat, a minute apart.
fn many(count: i64) -> Vec<teams_store::Message> {
    (1..=count)
        .map(|n| teams_store::Message {
            conversation_id: AB.to_owned(),
            id: n.to_string(),
            version: 1,
            client_message_id: None,
            sender: Some(common::LAB_B.to_owned()),
            sender_name: None,
            message_type: "RichText/Html".to_owned(),
            content: format!("<p>number {n}</p>"),
            properties: None,
            arrival_time: 1_767_268_800_000 + n * 60_000,
            compose_time: None,
            edit_time: None,
            delete_time: None,
        })
        .collect()
}

#[test]
fn the_message_list_is_one_stop_with_a_button_in_it() {
    let rig = Rig::start();
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), many(100));
    let (view, compose, cx) = rooted_view(&rig, Settings::default(), 900.0);
    let list = cx.debug_bounds("chat-messages").expect("the message list is drawn");
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: list.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(1_000.0))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    draw(cx);
    assert!(shown(cx, "jump-to-newest"), "scrolled up, the list holds a button");
    focus_box(&compose, cx);
    press(cx, "shift-tab");
    assert!(
        list_focused(&view, cx),
        "Shift+Tab from the text, the list, not its button"
    );
    press(cx, "tab");
    assert!(
        box_focused(&compose, cx),
        "Tab from the list, past its button to the text"
    );
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_expanded(window, cx)));
    draw(cx);
    focus_box(&compose, cx);
    press(cx, "shift-tab");
    press(cx, "shift-tab");
    press(cx, "shift-tab");
    assert!(list_focused(&view, cx), "from the format bar, the list, not its button");
}

#[test]
fn escape_in_the_emoji_picker_after_emoji_went_into_more_gives_the_text_the_keys() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    type_in(&compose, cx, "a draft");
    press(cx, "tab");
    press(cx, "right");
    key_click(cx);
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()));
    cx.simulate_resize(size(px(150.0), px(640.0)));
    draw(cx);
    assert!(!shown(cx, "compose-emoji"), "Emoji is in More options now");
    if compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()) {
        press(cx, "escape");
    }
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_none()));
    assert!(box_focused(&compose, cx), "Escape gives the text the keys");
}

/// The bot card message `id`: one text input and its Send.
fn card_message(id: &str) -> teams_store::Message {
    card_message_of(
        id,
        &serde_json::json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Text", "id": "said"}],
            "actions": [{"type": "Action.Submit", "title": "Send"}]
        }),
    )
}

/// The bot card message `id` that shows `card`.
fn card_message_of(id: &str, card: &serde_json::Value) -> teams_store::Message {
    use base64::Engine as _;
    let activity = serde_json::json!({"type": "message/card", "attachments": [
        {"contentType": "application/vnd.microsoft.card.adaptive", "content": card}
    ]});
    let b64 = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&activity).unwrap());
    let mut message = many(1).remove(0);
    message.id = id.to_owned();
    message.sender = Some("28:00000000-0000-4000-8000-0000000000b0".to_owned());
    message.message_type = "RichText/Media_Card".to_owned();
    message.arrival_time += 30_000;
    message.content =
        format!("<URIObject type=\"SWIFT.1\"><Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>");
    message
}

#[test]
fn enter_on_the_selected_message_goes_to_its_controls_and_escape_comes_back() {
    let rig = Rig::start();
    let mut messages = many(2);
    messages.insert(1, card_message("700"));
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), messages);
    rig.cx.update(|cx| cx.set_global(teams_ui::bots_flag::BotsFlag(true)));
    let (view, compose, cx) = rooted_view(&rig, Settings::default(), 900.0);
    assert!(shown(cx, "card-700-c0-in-said"), "the card's input is drawn");
    press(cx, "shift-tab");
    assert!(list_focused(&view, cx));
    press(cx, "up");
    press(cx, "up");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected().map(str::to_owned))
            .as_deref(),
        Some("700")
    );
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "Enter goes to the card's input");
    cx.simulate_input("lunch");
    draw(cx);
    press(cx, "tab");
    key_click(cx);
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert!(
        matches!(&asks[..], [ask] if matches!(&ask.kind, teams_proto::InvokeKind::Submit(sent)
            if sent.data == serde_json::json!({"said": "lunch"}))),
        "the keyboard alone fills in and sends the card: {asks:?}"
    );
    press(cx, "escape");
    assert!(
        list_focused(&view, cx),
        "Escape on the card's button gives the keys back to the list"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.selected().map(str::to_owned))
            .as_deref(),
        Some("700")
    );
    press(cx, "tab");
    assert!(box_focused(&compose, cx), "Tab from the list still passes its controls");
}

/// The a–b chat with the bot card message 700 of `card` selected from the keyboard, the list with the keys.
fn card_selected(rig: &Rig, card: teams_store::Message) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let mut messages = many(2);
    messages.insert(1, card);
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), messages);
    rig.cx.update(|cx| cx.set_global(teams_ui::bots_flag::BotsFlag(true)));
    let (view, _, cx) = rooted_view(rig, Settings::default(), 900.0);
    press(cx, "shift-tab");
    press(cx, "up");
    press(cx, "up");
    assert_eq!(
        view.read_with(cx, |view, _| view.selected().map(str::to_owned))
            .as_deref(),
        Some("700")
    );
    (view, cx)
}

#[test]
fn escape_in_the_card_s_input_gives_the_keys_back_to_the_list() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "Enter goes to the card's input");
    cx.simulate_input("lunch");
    draw(cx);
    press(cx, "escape");
    assert!(
        list_focused(&view, cx),
        "Escape in the input gives the keys back to the list"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.selected().map(str::to_owned))
            .as_deref(),
        Some("700")
    );
}

#[test]
fn enter_held_on_the_selected_card_does_not_press_its_button() {
    let rig = Rig::start();
    let card = card_message_of(
        "700",
        &serde_json::json!({
            "type": "AdaptiveCard",
            "body": [{"type": "TextBlock", "text": "Ready?"}],
            "actions": [{"type": "Action.Submit", "title": "Yes", "data": {"ok": true}}]
        }),
    );
    let (view, cx) = card_selected(&rig, card);
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "Enter goes to the card's button");
    for _ in 0..3 {
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
            is_held: true,
            prefer_character_input: false,
        });
        draw(cx);
    }
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    draw(cx);
    assert!(
        rig.core.bot_asks.lock().unwrap().is_empty(),
        "the held Enter sends nothing"
    );
    key_click(cx);
    assert_eq!(
        rig.core.bot_asks.lock().unwrap().len(),
        1,
        "a new push of Enter presses the button"
    );
}

fn hold(cx: &mut VisualTestContext, key: &str) {
    for _ in 0..3 {
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
            is_held: true,
            prefer_character_input: false,
        });
        draw(cx);
    }
}

#[test]
fn enter_held_on_the_list_does_not_go_into_the_selected_message() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    hold(cx, "enter");
    assert!(
        list_focused(&view, cx),
        "the repeats of a held Enter leave the keys on the list"
    );
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "a new push of Enter goes into the message");
}

#[test]
fn a_held_key_types_again_in_the_card_s_text() {
    let rig = Rig::start();
    let card = card_message_of(
        "700",
        &serde_json::json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Text", "id": "said", "isMultiline": true}],
            "actions": [{"type": "Action.Submit", "title": "Send"}]
        }),
    );
    let (view, cx) = card_selected(&rig, card);
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "Enter goes to the card's text");
    cx.simulate_input("a");
    press(cx, "enter");
    hold(cx, "enter");
    cx.simulate_input("b");
    click(cx, "card-700-c0a0");
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert!(
        matches!(&asks[..], [ask] if matches!(&ask.kind, teams_proto::InvokeKind::Submit(sent)
            if sent.data == serde_json::json!({"said": "a\n\n\n\nb"}))),
        "each repeat of the held Enter is a new line: {asks:?}"
    );
}

#[test]
fn escape_in_the_card_s_input_with_a_pill_under_the_keys_goes_to_the_list_at_once() {
    let rig = Rig::start();
    rig.core.reactions.lock().unwrap().insert(
        "700".to_owned(),
        vec![teams_store::Reaction {
            key: "like".to_owned(),
            user: "8:orgid:00000000-0000-4000-8000-00000000000b".to_owned(),
            time: 1_700_000_000_000,
        }],
    );
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "right");
    assert!(
        view.read_with(cx, |view, _| view.reaction_focus().is_some()),
        "Right puts the pill under the keys"
    );
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "Enter goes to the card's input");
    press(cx, "escape");
    assert!(list_focused(&view, cx), "one Escape gives the keys back to the list");
}

/// A chat with the bot card `card` (`700`) and the user's own message `701`, whose delete's confirmation is open.
fn deleting_beside_a_card(rig: &Rig, card: teams_store::Message) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let mut messages = many(2);
    messages.insert(1, card);
    let mut mine = many(1).remove(0);
    mine.id = "701".to_owned();
    mine.sender = Some(common::LAB_A.to_owned());
    mine.arrival_time += 40_000;
    messages.push(mine);
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), messages);
    rig.cx.update(|cx| cx.set_global(teams_ui::bots_flag::BotsFlag(true)));
    let (view, _, cx) = rooted_view(rig, Settings::default(), 900.0);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.act(teams_ui::chat_view::MessageAction::Delete, "701", window, cx);
        });
    });
    draw(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.deleting().map(str::to_owned))
            .as_deref(),
        Some("701")
    );
    (view, cx)
}

fn deleted(rig: &Rig) -> bool {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .any(|action| matches!(action, teams_core::Action::Delete { .. }))
}

#[test]
fn enter_on_a_card_s_button_sends_the_card_and_deletes_nothing() {
    let rig = Rig::start();
    let (view, cx) = deleting_beside_a_card(&rig, card_message("700"));
    click(cx, "card-700-c0-in-said");
    cx.simulate_input("lunch");
    draw(cx);
    press(cx, "tab");
    assert!(!list_focused(&view, cx), "Tab goes to the card's button");
    key_click(cx);
    assert!(!deleted(&rig), "Enter on the card's button deletes nothing");
    assert_eq!(rig.core.bot_asks.lock().unwrap().len(), 1, "Enter sends the card");
}

#[test]
fn escape_in_a_card_s_time_list_closes_the_delete_s_confirmation() {
    let rig = Rig::start();
    let card = card_message_of(
        "700",
        &serde_json::json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Time", "id": "at"}],
            "actions": [{"type": "Action.Submit", "title": "Send"}]
        }),
    );
    let (view, cx) = deleting_beside_a_card(&rig, card);
    click(cx, "card-700-c0-in-at");
    press(cx, "tab");
    press(cx, "enter");
    assert!(
        shown(cx, "card-700-c0-in-at-times-1200AM"),
        "Enter on the chevron opens the times"
    );
    press(cx, "escape");
    assert!(!shown(cx, "card-700-c0-in-at-times-1200AM"), "Escape closes the times");
    assert!(
        view.read_with(cx, |view, _| view.deleting().is_none()),
        "and the confirmation"
    );
    press(cx, "enter");
    assert!(!deleted(&rig), "the next Enter deletes nothing");
}

#[test]
fn escape_in_the_card_s_input_closes_the_delete_s_confirmation() {
    let rig = Rig::start();
    let (view, cx) = deleting_beside_a_card(&rig, card_message("700"));
    click(cx, "card-700-c0-in-said");
    assert!(!list_focused(&view, cx), "the click puts the keys in the card's input");
    press(cx, "escape");
    assert!(list_focused(&view, cx), "Escape gives the keys back to the list");
    assert!(
        view.read_with(cx, |view, _| view.deleting().is_none()),
        "and closes the confirmation"
    );
    press(cx, "enter");
    assert!(!deleted(&rig), "the next Enter deletes nothing");
}

#[test]
fn escape_in_the_box_keeps_the_delete_s_confirmation() {
    let rig = Rig::start();
    let (view, cx) = deleting_beside_a_card(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    focus_box(&compose, cx);
    press(cx, "escape");
    assert!(
        view.read_with(cx, |view, _| view.deleting().is_some()),
        "Escape outside the list keeps the confirmation"
    );
}

#[test]
fn escape_in_another_view_s_list_keeps_the_delete_s_confirmation() {
    let rig = Rig::start();
    let (view, cx) = deleting_beside_a_card(&rig, card_message("700"));
    let (other, _, other_cx) = rooted_view(&rig, Settings::default(), 900.0);
    other_cx.update(|window, cx| other.read(cx).focus_handle().clone().focus(window, cx));
    draw(other_cx);
    assert!(list_focused(&other, other_cx), "the keys are in the other view's list");
    press(other_cx, "escape");
    draw(cx);
    assert!(
        view.read_with(cx, |view, _| view.deleting().is_some()),
        "Escape in another view's list keeps this view's confirmation"
    );
}

#[test]
fn shift_escape_in_the_list_keeps_the_delete_s_confirmation() {
    let rig = Rig::start();
    let (view, cx) = deleting_beside_a_card(&rig, card_message("700"));
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    draw(cx);
    press(cx, "shift-escape");
    assert!(
        view.read_with(cx, |view, _| view.deleting().is_some()),
        "only a plain Escape closes the confirmation"
    );
}

#[test]
fn enter_held_after_a_time_is_chosen_does_not_open_the_times_again() {
    let rig = Rig::start();
    let card = card_message_of(
        "700",
        &serde_json::json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Time", "id": "at"}],
            "actions": [{"type": "Action.Submit", "title": "Send"}]
        }),
    );
    let (view, cx) = card_selected(&rig, card);
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "Enter goes to the card's time");
    press(cx, "tab");
    press(cx, "enter");
    assert!(
        shown(cx, "card-700-c0-in-at-times-1200AM"),
        "Enter on the chevron opens the times"
    );
    press(cx, "down");
    press(cx, "enter");
    assert!(
        !shown(cx, "card-700-c0-in-at-times-1200AM"),
        "Enter chooses a time and closes the times"
    );
    hold(cx, "enter");
    assert!(
        !shown(cx, "card-700-c0-in-at-times-1200AM"),
        "the held Enter's repeats do not open the times again"
    );
}

#[test]
fn escape_held_in_the_card_s_input_stops_at_the_list() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "enter");
    assert!(!list_focused(&view, cx), "Enter goes to the card's input");
    press(cx, "escape");
    hold(cx, "escape");
    assert!(
        list_focused(&view, cx),
        "the repeats of a held Escape leave the keys on the list"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.selected().map(str::to_owned))
            .as_deref(),
        Some("700"),
        "and keep the selection"
    );
}

#[test]
fn escape_held_on_the_list_with_nothing_selected_stops_at_the_box() {
    let rig = Rig::start();
    let (view, compose, cx) = rooted_view(&rig, Settings::default(), 900.0);
    let focus_the_list = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
        draw(cx);
    };
    focus_the_list(cx);
    press(cx, "escape");
    assert!(box_focused(&compose, cx), "Escape on the list gives the box the keys");
    hold(cx, "escape");
    assert!(
        box_focused(&compose, cx),
        "the repeats of a held Escape leave the keys in the box"
    );
    replying_in(&compose, cx);
    focus_the_list(cx);
    press(cx, "escape");
    assert!(box_focused(&compose, cx));
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn tab_in_a_tool_s_menu_closes_it_and_leaves_the_tools() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    press(cx, "tab");
    press(cx, "end");
    let plus = focused(cx);
    press(cx, "tab");
    let after = focused(cx);
    press(cx, "shift-tab");
    assert_eq!(focused(cx), plus);
    press(cx, "enter");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    press(cx, "tab");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        None,
        "Tab closes the menu"
    );
    assert_eq!(focused(cx), after, "and goes on as Tab from \"+\"");
    press(cx, "shift-tab");
    press(cx, "enter");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    press(cx, "shift-tab");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        None,
        "Shift+Tab closes the menu"
    );
    assert!(box_focused(&compose, cx), "and goes back as Shift+Tab from \"+\"");
}

#[test]
fn shift_tab_in_a_message_s_menu_gives_the_keys_back_to_the_card_s_input() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "enter");
    let input = focused(cx);
    assert!(!list_focused(&view, cx), "Enter goes to the card's input");
    press(cx, "shift-f10");
    assert!(
        view.read_with(cx, |view, _| view.options().is_some()),
        "Shift+F10 opens the message's menu"
    );
    press(cx, "shift-tab");
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Shift+Tab closes the menu"
    );
    assert_eq!(focused(cx), input, "and gives the keys back to the card's input");
}

#[test]
fn tab_out_of_a_clicked_message_menu_gives_the_keys_back_to_the_card_s_input() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "enter");
    let input = focused(cx);
    draw(cx);
    let message = cx.debug_bounds("message-700").expect("the card's message");
    cx.simulate_mouse_move(message.center(), None, Modifiers::none());
    click(cx, "message-more");
    assert!(
        view.read_with(cx, |view, _| view.options().is_some()),
        "the click opens the message's menu"
    );
    for _ in 0..40 {
        if view.read_with(cx, |view, _| view.options().is_none()) {
            break;
        }
        press(cx, "tab");
    }
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Tab in the menu closes it"
    );
    assert_eq!(focused(cx), input, "and gives the keys back to the card's input");
}

#[test]
fn tab_in_a_clicked_tool_s_menu_goes_on_from_its_button() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    type_in(&compose, cx, "draft");
    click(cx, "compose-apps");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    press(cx, "tab");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        None,
        "Tab closes the menu"
    );
    let after = focused(cx);
    assert!(
        !box_focused(&compose, cx),
        "Tab goes past the tools, not back to the text"
    );
    assert!(!tools_focused(&compose, cx), "Tab leaves the tools, not stays on \"+\"");
    press(cx, "shift-tab");
    press(cx, "tab");
    assert_eq!(focused(cx), after, "the stop after the tools, as Tab from \"+\"");
}

/// The a–b chat at a width where Attach and More options are both on the row: the keys on Attach, More options opened
/// by a click, then the box narrower, Attach in More options now.
fn more_opened_on_attach_then_attach_moved_into_it(rig: &Rig) -> (Entity<Compose>, &'static mut VisualTestContext) {
    let (compose, cx) = rooted_chat(rig, 270.0);
    type_in(&compose, cx, "draft");
    press(cx, "tab");
    press(cx, "right");
    press(cx, "right");
    let attach = focused(cx);
    click(cx, "compose-more");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::More)
    );
    cx.simulate_resize(size(px(210.0), px(640.0)));
    draw(cx);
    let row = compose.read_with(cx, Compose::row_tools);
    assert_eq!(
        row,
        ["compose-format", "compose-emoji"],
        "Attach is in More options now"
    );
    assert!(shown(cx, "compose-more"));
    assert_ne!(focused(cx), attach, "the keys are in the menu");
    (compose, cx)
}

#[test]
fn plus_s_menu_closes_when_plus_leaves_the_row_and_the_text_gets_the_keys() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    press(cx, "tab");
    press(cx, "end");
    press(cx, "enter");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    let mut width = 900.0;
    while compose.read_with(cx, Compose::row_tools).contains(&"compose-apps") {
        width -= 20.0;
        assert!(width > 100.0, "\"+\" goes into More options in a narrow box");
        cx.simulate_resize(size(px(width), px(640.0)));
        draw(cx);
    }
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        None,
        "the menu closes with its button gone"
    );
    assert!(box_focused(&compose, cx), "the text has the keys");
    cx.simulate_resize(size(px(900.0), px(640.0)));
    draw(cx);
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        None,
        "the menu does not show again in a wide box"
    );
}

#[test]
fn tab_in_a_menu_whose_tool_left_the_row_goes_on_from_its_button() {
    let rig = Rig::start();
    let (compose, cx) = more_opened_on_attach_then_attach_moved_into_it(&rig);
    press(cx, "tab");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        None,
        "Tab closes the menu"
    );
    assert!(
        !box_focused(&compose, cx),
        "Tab goes past the tools, not back to the text"
    );
    assert!(!tools_focused(&compose, cx), "Tab leaves the tools");
}

#[test]
fn escape_in_a_menu_whose_tool_left_the_row_gives_the_text_the_keys() {
    let rig = Rig::start();
    let (compose, cx) = more_opened_on_attach_then_attach_moved_into_it(&rig);
    press(cx, "escape");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        None,
        "Escape closes the menu"
    );
    assert!(
        box_focused(&compose, cx),
        "the text has the keys, as Attach is gone from the row"
    );
}

/// The Tabs from the card's input into the menu that a click on "More options" opened, and from there out of it.
fn tabs_out_of_a_clicked_menu(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> usize {
    open_the_card_s_menu_by_a_click(view, cx);
    for tabs in 1..=40 {
        press(cx, "tab");
        if view.read_with(cx, |view, _| view.options().is_none()) {
            return tabs;
        }
    }
    panic!("Tab in the menu closes it");
}

fn open_the_card_s_menu_by_a_click(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    draw(cx);
    let message = cx.debug_bounds("message-700").expect("the card's message");
    cx.simulate_mouse_move(message.center(), None, Modifiers::none());
    click(cx, "message-more");
    assert!(
        view.read_with(cx, |view, _| view.options().is_some()),
        "the click opens the message's menu"
    );
}

#[test]
fn escape_in_customize_chosen_by_the_keys_in_a_clicked_menu_gives_the_keys_back_to_the_card_s_input() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "enter");
    let input = focused(cx);
    let tabs = tabs_out_of_a_clicked_menu(&view, cx);
    assert_eq!(focused(cx), input);
    open_the_card_s_menu_by_a_click(&view, cx);
    for _ in 1..tabs {
        press(cx, "tab");
    }
    press(cx, "end");
    key_click(cx);
    assert!(
        cx.update(|_, cx| view.read(cx).bar_customize_open()),
        "Customize from the keys opens its list"
    );
    press(cx, "escape");
    assert!(!cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert_eq!(focused(cx), input, "Escape gives the keys back to the card's input");
}

#[test]
fn tab_and_shift_tab_in_customize_chosen_in_a_clicked_menu_close_it_and_give_the_keys_back_to_the_card_s_input() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "enter");
    let input = focused(cx);
    let tabs = tabs_out_of_a_clicked_menu(&view, cx);
    for key in ["tab", "shift-tab"] {
        open_the_card_s_menu_by_a_click(&view, cx);
        for _ in 1..tabs {
            press(cx, "tab");
        }
        press(cx, "end");
        key_click(cx);
        assert!(
            cx.update(|_, cx| view.read(cx).bar_customize_open()),
            "Customize from the keys opens its list"
        );
        press(cx, key);
        assert!(
            !cx.update(|_, cx| view.read(cx).bar_customize_open()),
            "{key} closes the list"
        );
        assert_eq!(focused(cx), input, "{key} gives the keys back to the card's input");
    }
}

#[test]
fn tab_and_shift_tab_in_customize_toolbar_close_it_and_give_the_text_the_keys() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    for key in ["tab", "shift-tab"] {
        focus_box(&compose, cx);
        click(cx, "compose-apps");
        click(cx, "compose-customize-row");
        assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
        press(cx, key);
        assert!(
            !compose.read_with(cx, |compose, _| compose.customize_open()),
            "{key} closes the list"
        );
        assert!(box_focused(&compose, cx), "{key} gives the text the keys");
    }
}

#[test]
fn customize_toolbar_taken_from_plus_by_the_keys_gives_them_back_to_plus_when_it_closes() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    for key in ["escape", "tab", "shift-tab"] {
        focus_box(&compose, cx);
        press(cx, "tab");
        press(cx, "end");
        let plus = focused(cx);
        press(cx, "enter");
        assert_eq!(
            compose.read_with(cx, |compose, _| compose.menu_open()),
            Some(MenuKind::Plus)
        );
        press(cx, "up");
        press(cx, "enter");
        assert!(
            compose.read_with(cx, |compose, _| compose.customize_open()),
            "Customize toolbar is the last row"
        );
        press(cx, key);
        assert!(
            !compose.read_with(cx, |compose, _| compose.customize_open()),
            "{key} closes the list"
        );
        assert_eq!(focused(cx), plus, "{key} gives the keys back to \"+\"");
    }
}

#[test]
fn customize_toolbar_taken_from_the_apps_flyout_of_a_focused_plus_gives_the_keys_back_to_plus() {
    let rig = Rig::start();
    rig.cx.update(|cx| cx.set_global(teams_ui::bots_flag::BotsFlag(true)));
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    let (compose, cx) = rooted_chat(&rig, 900.0);
    for key in ["escape", "tab", "shift-tab"] {
        focus_box(&compose, cx);
        press(cx, "tab");
        press(cx, "end");
        let plus = focused(cx);
        key_click(cx);
        let flyout = compose
            .read_with(cx, |compose, _| compose.apps_flyout().cloned())
            .expect("the flyout opens");
        flyout.update(cx, |_, cx| cx.emit(teams_ui::apps_flyout::FlyoutEvent::Customize));
        draw(cx);
        assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
        press(cx, key);
        assert!(
            !compose.read_with(cx, |compose, _| compose.customize_open()),
            "{key} closes the list"
        );
        assert_eq!(focused(cx), plus, "{key} gives the keys back to \"+\"");
    }
}

#[test]
fn escape_in_the_apps_flyout_of_a_focused_plus_gives_the_keys_back_to_plus() {
    let rig = Rig::start();
    rig.cx.update(|cx| cx.set_global(teams_ui::bots_flag::BotsFlag(true)));
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    let (compose, cx) = rooted_chat(&rig, 900.0);
    focus_box(&compose, cx);
    press(cx, "tab");
    press(cx, "end");
    let plus = focused(cx);
    key_click(cx);
    assert!(compose.read_with(cx, |compose, _| compose.apps_flyout().is_some()));
    assert_ne!(focused(cx), plus, "the flyout's search takes the keys");
    press(cx, "escape");
    assert!(
        compose.read_with(cx, |compose, _| compose.apps_flyout().is_none()),
        "Escape closes the flyout"
    );
    assert_eq!(focused(cx), plus, "Escape gives the keys back to \"+\"");
}

#[test]
fn escape_held_in_the_apps_flyout_does_not_cancel_the_reply() {
    let rig = Rig::start();
    rig.cx.update(|cx| cx.set_global(teams_ui::bots_flag::BotsFlag(true)));
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    let (compose, cx) = rooted_chat(&rig, 900.0);
    replying_in(&compose, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)));
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.apps_flyout().is_some()));
    assert!(!box_focused(&compose, cx), "the flyout's search takes the keys");
    press(cx, "escape");
    assert!(
        compose.read_with(cx, |compose, _| compose.apps_flyout().is_none()),
        "Escape closes the flyout"
    );
    assert!(box_focused(&compose, cx), "Escape gives the keys back to the box");
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn escape_held_in_the_emoji_picker_does_not_cancel_the_reply() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    replying_in(&compose, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx)));
    draw(cx);
    assert!(!box_focused(&compose, cx), "the picker's search takes the keys");
    press(cx, "escape");
    assert!(
        compose.read_with(cx, |compose, _| compose.emoji_picker().is_none()),
        "Escape closes the picker"
    );
    assert!(box_focused(&compose, cx), "Escape gives the keys back to the box");
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn shift_escape_held_in_the_apps_flyout_does_not_cancel_the_reply_when_shift_goes_up() {
    let rig = Rig::start();
    rig.cx.update(|cx| cx.set_global(teams_ui::bots_flag::BotsFlag(true)));
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    let (compose, cx) = rooted_chat(&rig, 900.0);
    replying_in(&compose, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)));
    draw(cx);
    press(cx, "shift-escape");
    assert!(
        compose.read_with(cx, |compose, _| compose.apps_flyout().is_none()),
        "Shift+Escape closes the flyout"
    );
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn escape_in_the_apps_flyout_closes_a_message_s_menu_first() {
    let rig = Rig::start();
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    let (view, cx) = card_selected(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    open_the_card_s_menu_by_a_click(&view, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)));
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.apps_flyout().is_some()));
    assert!(view.read_with(cx, |view, _| view.options().is_some()));
    press(cx, "escape");
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Escape closes the message's menu first"
    );
    assert!(
        compose.read_with(cx, |compose, _| compose.apps_flyout().is_some()),
        "the flyout stays open"
    );
    hold(cx, "escape");
    assert!(
        compose.read_with(cx, |compose, _| compose.apps_flyout().is_some()),
        "the held Escape's repeats do not close the flyout"
    );
    press(cx, "escape");
    assert!(compose.read_with(cx, |compose, _| compose.apps_flyout().is_none()));
}

#[test]
fn escape_in_the_emoji_picker_keeps_a_message_s_menu_open_also_when_held() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    open_the_card_s_menu_by_a_click(&view, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_emoji(window, cx)));
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()));
    press(cx, "escape");
    assert!(
        compose.read_with(cx, |compose, _| compose.emoji_picker().is_none()),
        "Escape closes the picker"
    );
    assert!(
        view.read_with(cx, |view, _| view.options().is_some()),
        "one Escape closes only the picker"
    );
    hold(cx, "escape");
    assert!(
        view.read_with(cx, |view, _| view.options().is_some()),
        "the held Escape's repeats do not close the message's menu"
    );
    press(cx, "escape");
    assert!(view.read_with(cx, |view, _| view.options().is_none()));
}

#[test]
fn shift_escape_in_the_apps_flyout_closes_a_message_s_menu_first() {
    let rig = Rig::start();
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    let (view, cx) = card_selected(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    open_the_card_s_menu_by_a_click(&view, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)));
    draw(cx);
    press(cx, "shift-escape");
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Shift+Escape closes the message's menu first"
    );
    assert!(
        compose.read_with(cx, |compose, _| compose.apps_flyout().is_some()),
        "the flyout stays open"
    );
}

/// The box replies to a made-up message, with the keys in it.
fn replying_in(compose: &Entity<Compose>, cx: &mut VisualTestContext) {
    let quoted = teams_ui::compose::Quoted {
        message_id: "1700000000000".into(),
        sender: "8:orgid:00000000-0000-4000-8000-00000000000b".into(),
        sender_name: "Lab Bee".into(),
        time: "1700000000000".into(),
        preview: "A made-up message".into(),
        shown: None,
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.reply_to(quoted, window, cx)));
    focus_box(compose, cx);
    assert!(replying(compose, cx));
}

fn replying(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> bool {
    compose.read_with(cx, |compose, _| {
        matches!(compose.mode(), teams_ui::compose::Mode::Reply(_))
    })
}

/// A held Escape keeps the reply; a new Escape in the box then cancels it.
fn held_escape_keeps_the_reply(compose: &Entity<Compose>, cx: &mut VisualTestContext) {
    hold(cx, "escape");
    assert!(
        replying(compose, cx),
        "the held Escape's repeats do not cancel the reply"
    );
    press(cx, "escape");
    assert!(!replying(compose, cx), "a new Escape in the box cancels the reply");
}

#[test]
fn escape_held_in_customize_chosen_in_a_clicked_menu_does_not_cancel_the_reply() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    replying_in(&compose, cx);
    let tabs = tabs_out_of_a_clicked_menu(&view, cx);
    focus_box(&compose, cx);
    open_the_card_s_menu_by_a_click(&view, cx);
    for _ in 1..tabs {
        press(cx, "tab");
    }
    press(cx, "end");
    key_click(cx);
    assert!(
        cx.update(|_, cx| view.read(cx).bar_customize_open()),
        "Customize from the keys opens its list"
    );
    press(cx, "escape");
    assert!(!cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert!(box_focused(&compose, cx), "Escape gives the keys back to the box");
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn escape_held_in_a_clicked_message_menu_does_not_cancel_the_reply() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    replying_in(&compose, cx);
    let tabs = tabs_out_of_a_clicked_menu(&view, cx);
    focus_box(&compose, cx);
    open_the_card_s_menu_by_a_click(&view, cx);
    for _ in 1..tabs {
        press(cx, "tab");
    }
    assert!(view.read_with(cx, |view, _| view.options().is_some()));
    assert!(
        !box_focused(&compose, cx) && !tools_focused(&compose, cx),
        "the keys are in the menu"
    );
    press(cx, "escape");
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Escape closes the menu"
    );
    assert!(box_focused(&compose, cx), "Escape gives the keys back to the box");
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn escape_held_with_a_clicked_message_menu_over_the_box_does_not_cancel_the_reply() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    replying_in(&compose, cx);
    open_the_card_s_menu_by_a_click(&view, cx);
    assert!(box_focused(&compose, cx), "the click leaves the keys in the box");
    press(cx, "escape");
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Escape closes the menu"
    );
    assert!(
        replying(&compose, cx),
        "the Escape that closed the menu keeps the reply"
    );
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn escape_on_a_tool_closes_a_clicked_message_menu() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    focus_box(&compose, cx);
    press(cx, "tab");
    assert!(tools_focused(&compose, cx), "Tab goes to the tools");
    let tool = focused(cx);
    open_the_card_s_menu_by_a_click(&view, cx);
    assert_eq!(focused(cx), tool, "the click leaves the keys on the tool");
    press(cx, "escape");
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Escape closes the menu"
    );
    assert_eq!(focused(cx), tool, "the tool keeps the keys");
}

#[test]
fn a_click_on_another_message_s_more_options_from_the_keys_in_a_menu_keeps_where_to_give_them_back() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    press(cx, "enter");
    let tabs = tabs_out_of_a_clicked_menu(&view, cx);
    press(cx, "escape");
    assert!(list_focused(&view, cx));
    press(cx, "up");
    let other = view.read_with(cx, |view, _| view.selected().map(str::to_owned));
    assert_ne!(other.as_deref(), Some("700"));
    press(cx, "shift-f10");
    press(cx, "down");
    assert_eq!(view.read_with(cx, |view, _| view.options().map(str::to_owned)), other);
    assert!(!list_focused(&view, cx), "the keys are in the other message's menu");
    open_the_card_s_menu_by_a_click(&view, cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.options().map(str::to_owned))
            .as_deref(),
        Some("700")
    );
    assert!(list_focused(&view, cx), "the click gives the keys back to the list");
    press(cx, "down");
    press(cx, "enter");
    for _ in 1..tabs {
        press(cx, "tab");
    }
    assert!(
        view.read_with(cx, |view, _| view.options().is_some()),
        "the keys are in the card's menu"
    );
    press(cx, "escape");
    assert!(view.read_with(cx, |view, _| view.options().is_none()));
    assert!(
        list_focused(&view, cx),
        "Escape gives the keys back to where the click left them"
    );
}

#[test]
fn escape_held_after_it_closed_plus_does_not_cancel_the_reply() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    replying_in(&compose, cx);
    click(cx, "compose-apps");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Plus)
    );
    press(cx, "escape");
    assert_eq!(compose.read_with(cx, |compose, _| compose.menu_open()), None);
    assert!(
        replying(&compose, cx),
        "the Escape that closed the menu keeps the reply"
    );
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn escape_held_after_it_closed_customize_toolbar_does_not_cancel_the_reply() {
    let rig = Rig::start();
    let (compose, cx) = rooted_chat(&rig, 900.0);
    replying_in(&compose, cx);
    click(cx, "compose-apps");
    click(cx, "compose-customize-row");
    assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
    press(cx, "escape");
    assert!(!compose.read_with(cx, |compose, _| compose.customize_open()));
    assert!(
        replying(&compose, cx),
        "the Escape that closed the list keeps the reply"
    );
    held_escape_keeps_the_reply(&compose, cx);
}

#[test]
fn copy_text_picked_by_the_keys_in_a_clicked_menu_gives_them_back_to_the_list() {
    let rig = Rig::start();
    let (view, cx) = card_selected(&rig, card_message("700"));
    let tabs = tabs_out_of_a_clicked_menu(&view, cx);
    assert!(list_focused(&view, cx));
    open_the_card_s_menu_by_a_click(&view, cx);
    for _ in 1..tabs {
        press(cx, "tab");
    }
    assert!(!list_focused(&view, cx), "the keys are in the menu");
    // the first row the keys reach: Copy text
    press(cx, "home");
    key_click(cx);
    assert!(
        view.read_with(cx, |view, _| view.options().is_none()),
        "Copy text closes the menu"
    );
    assert!(cx.read_from_clipboard().is_some(), "Copy text copies the message");
    assert!(list_focused(&view, cx), "the keys go back to the list");
}

#[test]
fn enter_held_after_a_time_is_clicked_open_does_not_send_the_draft() {
    let rig = Rig::start();
    let card = card_message_of(
        "700",
        &serde_json::json!({
            "type": "AdaptiveCard",
            "body": [{"type": "Input.Time", "id": "at"}],
            "actions": [{"type": "Action.Submit", "title": "Send"}]
        }),
    );
    let (view, cx) = card_selected(&rig, card);
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    focus_box(&compose, cx);
    type_in(&compose, cx, "draft");
    click(cx, "card-700-c0-in-at-times");
    assert!(
        shown(cx, "card-700-c0-in-at-times-1200AM"),
        "a click on the chevron opens the times"
    );
    press(cx, "down");
    press(cx, "enter");
    assert!(
        !shown(cx, "card-700-c0-in-at-times-1200AM"),
        "Enter chooses a time and closes the times"
    );
    hold(cx, "enter");
    assert_eq!(
        compose.read_with(cx, |compose, cx| compose.editor().read(cx).text()),
        "draft",
        "the held Enter's repeats do not send the draft"
    );
}
