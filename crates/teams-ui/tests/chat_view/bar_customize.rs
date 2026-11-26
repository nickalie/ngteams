//! The buttons of a message's hover bar, as the user chooses them, on GPUI's test context over the fake engine: the
//! bar of Teams until the user changes it; a right click on the bar or "Customize" in "More options" lists the
//! buttons; a hidden reaction and a hidden reply are rows of "More options" that still act, and their keys still act;
//! an action of "More options" shown on the bar acts from there; the choice is the account's, in its settings; a
//! button that does not apply to a message does not show on it. The messages and ids are made up.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, VisualTestContext, px, size};
use teams_core::Action;
use teams_ui::ViewTrace;
use teams_ui::chat_view::{CUSTOMIZE, ChatView, ChatViewEvent, MessageAction, SHOW_IN_BAR};
use teams_ui::settings::{BarItem, Settings, SettingsModel};

use super::common::{LAB_A, LAB_B, Rig};
use super::{AB, MINUTE, NOON, draw, message, parts, reaction, rig_with};

/// Teams' own quick reactions, the bar's until the account uses others more.
const TEAMS_QUICK: [&str; 4] = ["like", "heart", "laugh", "surprised"];

fn quick(key: &str) -> BarItem {
    BarItem::reaction(key).unwrap_or_else(|| panic!("{key} is a reaction"))
}

/// A chat of someone else's message ("1") and the account's own ("2").
fn two() -> Rig {
    rig_with(vec![
        message("1", LAB_B, "<p>theirs</p>", NOON),
        message("2", LAB_A, "<p>mine</p>", NOON + MINUTE),
    ])
}

/// The chat view over `rig` with `settings`, and its settings model.
fn window_with(
    rig: &Rig,
    settings: Settings,
) -> (Entity<ChatView>, Entity<SettingsModel>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let model = parts.settings.clone();
    let app: &'static mut gpui::TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    (view, model, cx)
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// A key down and up: a focused row's click comes on the key's release.
fn press(cx: &mut VisualTestContext, key: &'static str) {
    let keystroke = gpui::Keystroke::parse(key).unwrap();
    cx.simulate_keystrokes(key);
    cx.simulate_event(gpui::KeyUpEvent { keystroke });
    draw(cx);
}

fn hover(cx: &mut VisualTestContext, row: &'static str) {
    let row = cx.debug_bounds(row).unwrap();
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is shown"));
    cx.simulate_click(at.center(), Modifiers::none());
    draw(cx);
}

/// A right click on the hover bar of the message `row`.
fn right_click_bar(cx: &mut VisualTestContext, row: &'static str) {
    hover(cx, row);
    let bar = cx.debug_bounds("message-actions").unwrap();
    cx.simulate_mouse_down(bar.center(), gpui::MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(bar.center(), gpui::MouseButton::Right, Modifiers::none());
    draw(cx);
}

/// The actions the view tells from now on.
fn told(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<(MessageAction, String)>>> {
    let told: Rc<RefCell<Vec<(MessageAction, String)>>> = Rc::default();
    let seen = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &ChatViewEvent, _| {
            if let ChatViewEvent::Action { action, message } = event {
                seen.borrow_mut().push((*action, message.clone()));
            }
        })
        .detach();
    });
    told
}

fn reactions(rig: &Rig) -> Vec<(String, String)> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::React { message_id, key, .. } => Some((message_id.clone(), key.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn the_bar_of_teams_shows_until_the_user_changes_it_and_the_list_says_so() {
    let rig = two();
    let (view, _, cx) = window_with(&rig, Settings::default());
    hover(cx, "message-1");
    for selector in [
        "pick-like",
        "pick-heart",
        "pick-laugh",
        "pick-surprised",
        "action-react",
        "action-reply",
        "message-more",
    ] {
        assert!(shown(cx, selector), "{selector} on the bar");
    }
    assert!(!shown(cx, "action-copy") && !shown(cx, "action-delete") && !shown(cx, "action-mark-unread"));
    right_click_bar(cx, "message-1");
    assert!(cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert!(shown(cx, "message-customize-menu"));
    let checked = |cx: &mut VisualTestContext, key: &str| {
        cx.debug_bounds(Box::leak(format!("message-customize-{key}-check").into_boxed_str()))
            .is_some()
    };
    for item in TEAMS_QUICK.map(quick).into_iter().chain(BarItem::ACTIONS) {
        assert!(
            shown(
                cx,
                Box::leak(format!("message-customize-{}", item.key()).into_boxed_str())
            ),
            "{item:?} listed"
        );
        assert_eq!(checked(cx, &item.key()), item.shown_by_default(), "{item:?}");
    }
    assert!(checked(cx, "more"), "More options always shows");
    // the bar stays shown while the list is open, also with the pointer away
    cx.simulate_mouse_move(gpui::point(px(5.0), px(5.0)), None, Modifiers::none());
    draw(cx);
    assert!(shown(cx, "message-actions"));
    // its title is the list's; Escape closes it
    assert_eq!(SHOW_IN_BAR, "Show in the message bar");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert!(!shown(cx, "message-customize-menu"));
}

#[test]
fn a_hidden_reaction_and_reply_are_rows_of_more_options_that_still_act() {
    let rig = two();
    let (view, model, cx) = window_with(&rig, Settings::default());
    let told = told(&view, cx);
    right_click_bar(cx, "message-1");
    click(cx, "message-customize-reaction-heart");
    click(cx, "message-customize-reply");
    // the list stays open, the bar shows the change at once
    assert!(shown(cx, "message-customize-menu"));
    assert!(!shown(cx, "message-customize-reply-check"));
    assert!(!shown(cx, "pick-heart") && !shown(cx, "action-reply"));
    assert!(shown(cx, "pick-like") && shown(cx, "pick-laugh"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    // the choice is the account's
    let kept = cx.update(|_, cx| model.read(cx).settings().message_bar.get(LAB_A).cloned());
    assert_eq!(
        kept.map(|choice| choice.hidden),
        Some(vec![quick("heart"), BarItem::Reply])
    );
    // in "More options": the reaction, a hairline, then reply
    hover(cx, "message-1");
    click(cx, "message-more");
    let heart = cx.debug_bounds("menu-pick-heart").unwrap();
    let reply = cx.debug_bounds("action-reply").unwrap();
    assert!(
        heart.bottom() <= reply.top(),
        "the reactions first: {heart:?} over {reply:?}"
    );
    click(cx, "menu-pick-heart");
    assert_eq!(reactions(&rig), [("1".to_owned(), "heart".to_owned())]);
    assert_eq!(cx.update(|_, cx| view.read(cx).options().map(str::to_owned)), None);
    hover(cx, "message-1");
    click(cx, "message-more");
    click(cx, "action-reply");
    assert_eq!(*told.borrow(), [(MessageAction::Reply, "1".to_owned())]);
}

#[test]
fn a_hidden_quick_reaction_stays_hidden_when_the_quick_reactions_change_their_order() {
    let rig = two();
    let (_, model, cx) = window_with(&rig, Settings::default());
    right_click_bar(cx, "message-1");
    click(cx, "message-customize-reaction-heart");
    cx.simulate_keystrokes("escape");
    draw(cx);
    laugh_comes_first(&rig, cx);
    hover(cx, "message-1");
    let laugh = cx.debug_bounds("pick-laugh").unwrap();
    let like = cx.debug_bounds("pick-like").unwrap();
    assert!(laugh.right() <= like.left(), "laugh comes first: {laugh:?} {like:?}");
    assert!(!shown(cx, "pick-heart"), "the hidden heart stays hidden");
    assert!(shown(cx, "pick-surprised"), "no other reaction hides");
    // the list names the reactions on the bar now, with their checks
    right_click_bar(cx, "message-1");
    let first = cx.debug_bounds("message-customize-reaction-laugh").unwrap();
    let second = cx.debug_bounds("message-customize-reaction-like").unwrap();
    assert!(first.bottom() <= second.top(), "the list keeps the bar's order");
    for (key, checked) in [("laugh", true), ("like", true), ("heart", false), ("surprised", true)] {
        let check = Box::leak(format!("message-customize-reaction-{key}-check").into_boxed_str());
        assert_eq!(shown(cx, check), checked, "{key}");
    }
    let kept = cx.update(|_, cx| model.read(cx).settings().message_bar.get(LAB_A).cloned());
    assert_eq!(kept.map(|choice| choice.hidden), Some(vec![quick("heart")]));
}

/// The account reacts with laugh, and the change comes in: laugh is first of the quick reactions.
fn laugh_comes_first(rig: &Rig, cx: &mut VisualTestContext) {
    *rig.core.frequent.lock().unwrap() = vec!["laugh".to_owned()];
    rig.core
        .reactions
        .lock()
        .unwrap()
        .insert("1".to_owned(), vec![reaction("laugh", LAB_A, NOON + 10_000)]);
    if let Some(stored) = rig.core.messages.lock().unwrap().get_mut(AB) {
        stored[0].version += 1;
    }
    rig.send(teams_core::Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    draw(cx);
}

#[test]
fn a_click_on_a_row_hides_that_row_when_the_quick_reactions_change_their_order_under_it() {
    let rig = two();
    let (_, model, cx) = window_with(&rig, Settings::default());
    right_click_bar(cx, "message-1");
    let heart = cx.debug_bounds("message-customize-reaction-heart").unwrap().center();
    cx.simulate_mouse_down(heart, gpui::MouseButton::Left, Modifiers::none());
    laugh_comes_first(&rig, cx);
    assert_eq!(
        cx.debug_bounds("message-customize-reaction-heart")
            .map(|row| row.center()),
        Some(heart)
    );
    cx.simulate_mouse_up(heart, gpui::MouseButton::Left, Modifiers::none());
    draw(cx);
    let kept = cx.update(|_, cx| model.read(cx).settings().message_bar.get(LAB_A).cloned());
    assert_eq!(kept.map(|choice| choice.hidden), Some(vec![quick("heart")]));
}

#[test]
fn the_keys_reach_a_hidden_reaction_and_a_hidden_reply() {
    let rig = two();
    let mut settings = Settings::default();
    settings.show_bar_item(LAB_A, quick("like"), false);
    settings.show_bar_item(LAB_A, BarItem::Reply, false);
    settings.show_bar_item(LAB_A, BarItem::React, false);
    let (view, _, cx) = window_with(&rig, settings);
    let told = told(&view, cx);
    hover(cx, "message-1");
    assert!(!shown(cx, "pick-like") && !shown(cx, "action-react") && !shown(cx, "action-reply"));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select(Some("1".to_owned()), cx);
            view.focus_handle().clone().focus(window, cx);
        });
    });
    draw(cx);
    // react's key, then the first place's key: the hidden reaction
    cx.simulate_keystrokes("a 1");
    draw(cx);
    assert_eq!(reactions(&rig), [("1".to_owned(), "like".to_owned())]);
    // reply's key
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    cx.simulate_keystrokes("r");
    draw(cx);
    assert_eq!(
        *told.borrow(),
        [
            (MessageAction::React, "1".to_owned()),
            (MessageAction::Reply, "1".to_owned())
        ]
    );
}

#[test]
fn the_menu_keys_reach_the_hidden_rows_and_customize() {
    let rig = two();
    let mut settings = Settings::default();
    settings.show_bar_item(LAB_A, quick("like"), false);
    let (view, model, cx) = window_with(&rig, settings);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select(Some("1".to_owned()), cx);
            view.focus_handle().clone().focus(window, cx);
        });
    });
    draw(cx);
    cx.simulate_keystrokes("shift-f10");
    draw(cx);
    draw(cx);
    assert!(shown(cx, "message-menu") && shown(cx, "menu-pick-like") && shown(cx, "action-customize"));
    // End: the last row, "Customize"; Enter opens the list, its keys go down to the first row and show it again
    cx.simulate_keystrokes("end");
    draw(cx);
    press(cx, "enter");
    assert!(cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert!(!shown(cx, "message-menu"), "the menu closes for the list");
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert!(cx.update(|_, cx| model.read(cx).settings().shows_bar_item(LAB_A, quick("like"))));
    // Up from the first row goes round to the last that takes a key: Translate
    cx.simulate_keystrokes("up enter escape");
    draw(cx);
    assert!(cx.update(|_, cx| model.read(cx).settings().shows_bar_item(LAB_A, BarItem::Translate)));
    assert!(!cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert_eq!(CUSTOMIZE, "Customize");
}

#[test]
fn an_action_of_more_options_shown_on_the_bar_acts_from_there() {
    let rig = two();
    let (view, _, cx) = window_with(&rig, Settings::default());
    hover(cx, "message-1");
    click(cx, "message-more");
    click(cx, "action-customize");
    assert!(cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert!(!shown(cx, "message-menu"));
    click(cx, "message-customize-copy");
    click(cx, "message-customize-mark-unread");
    cx.simulate_keystrokes("escape");
    draw(cx);
    hover(cx, "message-1");
    let x = |cx: &mut VisualTestContext, selector: &'static str| cx.debug_bounds(selector).unwrap().center().x;
    assert!(x(cx, "action-reply") < x(cx, "action-copy") && x(cx, "action-copy") < x(cx, "message-more"));
    assert!(x(cx, "action-copy") < x(cx, "action-mark-unread"));
    click(cx, "action-copy");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("theirs")
    );
    // "More options" no longer holds them
    hover(cx, "message-1");
    click(cx, "message-more");
    assert!(shown(cx, "message-menu"));
    let menu = cx.debug_bounds("message-menu").unwrap();
    let copy = cx.debug_bounds("action-copy").unwrap();
    assert!(!menu.contains(&copy.center()), "copy is on the bar, not in the menu");
    let unread = cx.debug_bounds("action-mark-unread").unwrap();
    assert!(
        !menu.contains(&unread.center()),
        "mark as unread is on the bar, not in the menu"
    );
}

#[test]
fn a_button_shown_that_does_not_apply_to_a_message_does_not_show_on_it() {
    let rig = two();
    let mut settings = Settings::default();
    settings.show_bar_item(LAB_A, BarItem::Delete, true);
    settings.show_bar_item(LAB_A, BarItem::Translate, true);
    let (view, _, cx) = window_with(&rig, settings);
    // someone else's: no delete, no edit; it offers a translation
    hover(cx, "message-1");
    assert!(!shown(cx, "action-delete") && !shown(cx, "action-edit"));
    assert!(shown(cx, "action-translate"));
    // the account's own: edit and delete
    hover(cx, "message-2");
    assert!(shown(cx, "action-edit") && shown(cx, "action-delete"));
    click(cx, "action-delete");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).deleting().map(str::to_owned))
            .as_deref(),
        Some("2")
    );
}

#[test]
fn a_new_view_of_the_account_shows_its_choice() {
    let rig = two();
    let mut settings = Settings::default();
    settings.show_bar_item(LAB_A, quick("surprised"), false);
    settings.show_bar_item(LAB_A, BarItem::Copy, true);
    // another account's choice is not this account's
    settings.show_bar_item(LAB_B, BarItem::Reply, false);
    let (_, _, cx) = window_with(&rig, settings);
    hover(cx, "message-1");
    assert!(!shown(cx, "pick-surprised") && shown(cx, "action-copy") && shown(cx, "action-reply"));
}

#[test]
fn translate_on_the_bar_leaves_the_translation_settings_in_more_options() {
    let rig = two();
    let mut settings = Settings::default();
    settings.show_bar_item(LAB_A, BarItem::Translate, true);
    let (_, _, cx) = window_with(&rig, settings);
    hover(cx, "message-1");
    assert!(shown(cx, "action-translate"));
    click(cx, "message-more");
    click(cx, "action-translation");
    let list = cx.debug_bounds("message-translation-menu").unwrap();
    let settings_row = cx.debug_bounds("action-translation-settings").unwrap();
    assert!(list.contains(&settings_row.center()), "the list holds Settings");
    let translate = cx.debug_bounds("action-translate").unwrap();
    assert!(
        !list.contains(&translate.center()),
        "Translate is on the bar, not in the list"
    );
}

#[test]
fn a_right_click_on_the_open_more_options_closes_it_for_the_list() {
    let rig = two();
    // a short menu, under its button and clear of it
    let mut settings = Settings::default();
    for item in [BarItem::Copy, BarItem::Delete, BarItem::MarkUnread, BarItem::Translate] {
        settings.show_bar_item(LAB_A, item, true);
    }
    let (view, _, cx) = window_with(&rig, settings);
    hover(cx, "message-2");
    click(cx, "message-more");
    let menu = cx.debug_bounds("message-menu").unwrap();
    let more = cx.debug_bounds("message-more").unwrap();
    assert!(
        !menu.contains(&more.center()),
        "the menu is clear of its button: {menu:?} {more:?}"
    );
    cx.simulate_mouse_down(more.center(), gpui::MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(more.center(), gpui::MouseButton::Right, Modifiers::none());
    draw(cx);
    assert!(cx.update(|_, cx| view.read(cx).bar_customize_open()));
    assert!(!shown(cx, "message-menu"), "the menu closes for the list");
}

#[test]
fn more_options_taller_than_the_window_scrolls_to_the_row_of_the_keys() {
    let rig = two();
    let mut settings = Settings::default();
    for item in TEAMS_QUICK
        .map(quick)
        .into_iter()
        .chain([BarItem::React, BarItem::Edit, BarItem::Reply])
    {
        settings.show_bar_item(LAB_A, item, false);
    }
    let (view, _, cx) = window_with(&rig, settings);
    cx.simulate_resize(size(px(800.0), px(260.0)));
    draw(cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select(Some("2".to_owned()), cx);
            view.focus_handle().clone().focus(window, cx);
        });
    });
    draw(cx);
    cx.simulate_keystrokes("shift-f10");
    draw(cx);
    draw(cx);
    let menu = cx.debug_bounds("message-menu").unwrap();
    assert!(menu.size.height <= px(260.0), "the menu fits the window: {menu:?}");
    let customize = cx.debug_bounds("action-customize").unwrap();
    assert!(!menu.contains(&customize.center()), "Customize is under the fold");
    cx.simulate_keystrokes("end");
    draw(cx);
    draw(cx);
    let menu = cx.debug_bounds("message-menu").unwrap();
    let customize = cx.debug_bounds("action-customize").unwrap();
    assert!(
        menu.contains(&customize.center()),
        "End scrolls to Customize: {menu:?} {customize:?}"
    );
    // Down from the last row wraps to the first, Up from the first back to the last: each scrolls to its row
    cx.simulate_keystrokes("down");
    draw(cx);
    draw(cx);
    let menu = cx.debug_bounds("message-menu").unwrap();
    let first = cx.debug_bounds("menu-pick-like").unwrap();
    assert!(
        menu.contains(&first.center()),
        "Down scrolls to the first row: {menu:?} {first:?}"
    );
    cx.simulate_keystrokes("up");
    draw(cx);
    draw(cx);
    let menu = cx.debug_bounds("message-menu").unwrap();
    let customize = cx.debug_bounds("action-customize").unwrap();
    assert!(
        menu.contains(&customize.center()),
        "Up scrolls to Customize: {menu:?} {customize:?}"
    );
}

#[test]
fn the_keys_on_a_more_options_the_wheel_scrolled_start_on_its_first_row_in_view() {
    let rig = two();
    let mut settings = Settings::default();
    for item in TEAMS_QUICK
        .map(quick)
        .into_iter()
        .chain([BarItem::React, BarItem::Edit, BarItem::Reply])
    {
        settings.show_bar_item(LAB_A, item, false);
    }
    let (view, _, cx) = window_with(&rig, settings);
    cx.simulate_resize(size(px(800.0), px(260.0)));
    draw(cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.select(Some("2".to_owned()), cx);
            view.focus_handle().clone().focus(window, cx);
        });
    });
    draw(cx);
    hover(cx, "message-2");
    click(cx, "message-more");
    let menu = cx.debug_bounds("message-menu").unwrap();
    cx.simulate_event(gpui::ScrollEvent {
        position: menu.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-3000.0))),
        touch_phase: gpui::TouchPhase::Moved,
        ..gpui::ScrollEvent::default()
    });
    draw(cx);
    let menu = cx.debug_bounds("message-menu").unwrap();
    let first = cx.debug_bounds("menu-pick-like").unwrap();
    assert!(!menu.contains(&first.center()), "the wheel scrolls the first row out");
    cx.simulate_keystrokes("shift-f10");
    draw(cx);
    draw(cx);
    let menu = cx.debug_bounds("message-menu").unwrap();
    let first = cx.debug_bounds("menu-pick-like").unwrap();
    assert!(
        menu.contains(&first.center()),
        "the keys start in view: {menu:?} {first:?}"
    );
}
