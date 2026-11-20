//! The message box's toolbar, less crowded, on GPUI's test context over the fake engine (`common`): no AI button; the
//! AI tools (Correct, Translate and its language) are rows of "+" until the user shows them, and buttons the user
//! shows or hides, by a right click on the toolbar or "Customize toolbar" in "+", or in Settings. A hidden button's
//! action stays in "+", with its key. Send and "+" cannot hide. With the AI features off, nothing of AI shows. Every
//! id, name and text is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::Rig;
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, px, size};
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
