//! The live captions area: its lines scroll by the wheel and the keys and follow the newest line while the user is at
//! the end; its edge resizes it by a drag or by the keys, and the height is kept for the account.

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{Modifiers, MouseButton, VisualTestContext, point, px};
use teams_core::{CallStatus, Event};
use teams_ui::settings::Settings;
use teams_ui::shell::Shell;

use super::{
    AB, Rig, call, call_windows, calling, click, draw, drawn, registered, shell_window_with, stage, status, trace,
};

/// The call window with its captions area shown, and the shell whose settings it keeps its height in.
fn captions_shown(rig: &Rig, settings: Settings) -> (gpui::Entity<Shell>, &'static mut VisualTestContext) {
    let (trace, _) = trace();
    let settings = Settings {
        selected_chat: Some(AB.to_owned()),
        ..settings
    };
    let (shell, cx) = shell_window_with(rig, settings, Arc::clone(&trace));
    let (windows, _) = call_windows(rig, &shell, &trace);
    registered(rig);
    click(cx, "chat-call");
    status(rig, 1, CallStatus::Connected);
    let window = stage(rig, &windows);
    window.simulate_keystrokes("alt-shift-c");
    window.run_until_parked();
    lines(rig, 1);
    assert!(drawn(window, "call-captions"));
    (shell, window)
}

/// The captions' first `count` lines, as the engine sends them: all of them each time.
fn lines(rig: &Rig, count: usize) {
    let lines = (0..count)
        .map(|index| teams_core::Caption {
            id: format!("synthetic/{index}"),
            speaker: None,
            display_name: Some("Lab Speaker".into()),
            text: format!("Synthetic caption line {index}."),
            final_result: true,
            spoken_language: "en-us".into(),
            track_language: "en-us".into(),
            audio_timestamp: u64::try_from(index).unwrap() * 100,
            duration: 10,
        })
        .collect();
    rig.send(Event::CallCaptions {
        call: call(1),
        captions: teams_core::CallCaptions {
            phase: teams_core::CaptionPhase::On,
            spoken_language: "en-us".into(),
            error: None,
            lines,
        },
    });
    rig.cx.run_until_parked();
}

/// The captions stop, as the engine says when the user hides them.
fn captions_off(rig: &Rig) {
    rig.send(Event::CallCaptions {
        call: call(1),
        captions: teams_core::CallCaptions {
            phase: teams_core::CaptionPhase::Off,
            spoken_language: "en-us".into(),
            error: None,
            lines: Vec::new(),
        },
    });
    rig.cx.run_until_parked();
}

fn bounds(window: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    draw(window);
    window.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

/// Line `index` ends inside the area's list: the area shows its end.
fn at_end(window: &mut VisualTestContext, index: usize) -> bool {
    let list = bounds(window, "call-caption-text").unwrap();
    bounds(window, &format!("call-caption-line-{index}"))
        .is_some_and(|line| line.bottom() <= list.bottom() + px(1.0) && line.bottom() > list.top())
}

fn wheel(window: &mut VisualTestContext, dy: f32) {
    let list = bounds(window, "call-caption-text").unwrap();
    window.simulate_event(gpui::ScrollEvent {
        position: list.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.0), px(dy))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    });
    draw(window);
}

fn height(window: &mut VisualTestContext) -> f32 {
    f32::from(bounds(window, "call-captions").unwrap().size.height)
}

fn saved_height(shell: &gpui::Entity<Shell>, rig: &Rig) -> u32 {
    let me = rig.cx.read(|cx| rig.bridge.read(cx).core().me().to_owned());
    rig.cx
        .read(|cx| shell.read(cx).settings().read(cx).settings().caption_height_of(&me))
}

/// Tab goes on until the element `selector` marks as the keyboard's is drawn.
fn tab_to(window: &mut VisualTestContext, selector: &str) {
    let reached = (0..60).any(|_| {
        window.simulate_keystrokes("tab");
        drawn(window, selector)
    });
    assert!(reached, "Tab reaches {selector}");
}

#[test]
fn the_captions_follow_the_newest_line_until_scrolled_up() {
    let rig = calling();
    let (_, window) = captions_shown(&rig, Settings::default());
    for count in 2..=40 {
        lines(&rig, count);
        draw(window);
    }
    assert!(at_end(window, 39), "follows each new line");
    assert!(
        drawn(window, "call-caption-scrollbar"),
        "a scroll bar shows the lines are cut"
    );
    wheel(window, 400.0);
    assert!(!at_end(window, 39), "the wheel scrolls up");
    lines(&rig, 41);
    assert!(
        !at_end(window, 40),
        "scrolled up: a new line does not pull the view down"
    );
    wheel(window, -100_000.0);
    assert!(at_end(window, 40), "the wheel goes back to the end");
    lines(&rig, 42);
    assert!(at_end(window, 41), "at the end again: it follows");
    wheel(window, 400.0);
    assert!(!at_end(window, 41));
    captions_off(&rig);
    assert!(!drawn(window, "call-captions"), "hidden");
    lines(&rig, 43);
    assert!(at_end(window, 42), "shown again: at the newest line");
}

#[test]
fn the_keyboard_reaches_the_captions_and_its_keys_scroll_them() {
    let rig = calling();
    let (_, window) = captions_shown(&rig, Settings::default());
    lines(&rig, 40);
    assert!(at_end(window, 39));
    tab_to(window, "call-caption-keys");
    window.simulate_keystrokes("home");
    let list = bounds(window, "call-caption-text").unwrap();
    let first = bounds(window, "call-caption-line-0").unwrap();
    assert!(first.top() >= list.top(), "Home: the first line is in sight");
    assert!(!at_end(window, 39));
    window.simulate_keystrokes("down");
    let moved = bounds(window, "call-caption-line-0").map(|line| line.top());
    assert!(moved.is_none_or(|top| top < first.top()), "Down scrolls down");
    window.simulate_keystrokes("end");
    assert!(at_end(window, 39), "End: the newest line");
    window.simulate_keystrokes("up");
    assert!(!at_end(window, 39), "Up scrolls up");
    window.simulate_keystrokes("end pageup");
    assert!(!at_end(window, 39), "Page Up scrolls up");
    window.simulate_keystrokes(&["pagedown"; 40].join(" "));
    assert!(at_end(window, 39), "Page Down scrolls back down");
    lines(&rig, 41);
    assert!(at_end(window, 40), "at the end by the keys: it follows");
}

#[test]
fn a_drag_of_the_edge_resizes_the_captions_and_keeps_the_height() {
    let rig = calling();
    let (shell, window) = captions_shown(&rig, Settings::default());
    let before = height(window);
    assert!((before - 244.0).abs() < 1.0, "the default height: {before}");
    let edge = bounds(window, "call-caption-resize").unwrap().center();
    let to = point(edge.x, edge.y - px(120.0));
    window.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
    window.simulate_mouse_move(point(edge.x, edge.y - px(60.0)), MouseButton::Left, Modifiers::none());
    window.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    draw(window);
    assert!(
        (height(window) - (before + 120.0)).abs() < 1.0,
        "the area grows as the edge goes up"
    );
    assert_eq!(saved_height(&shell, &rig), 244, "a drag saves once, as it ends");
    window.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(window);
    assert_eq!(saved_height(&shell, &rig), 364);
    // far down: no shorter than the header and a line
    let edge = bounds(window, "call-caption-resize").unwrap().center();
    window.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
    let bottom = window.update(|window, _| window.viewport_size().height) - px(1.0);
    window.simulate_mouse_move(point(edge.x, bottom), MouseButton::Left, Modifiers::none());
    window.simulate_mouse_up(point(edge.x, bottom), MouseButton::Left, Modifiers::none());
    draw(window);
    assert!((height(window) - 100.0).abs() < 1.0, "the least height");
    assert_eq!(saved_height(&shell, &rig), 100);
    // far up: the stage keeps its room
    let edge = bounds(window, "call-caption-resize").unwrap().center();
    window.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
    window.simulate_mouse_move(point(edge.x, px(0.0)), MouseButton::Left, Modifiers::none());
    window.simulate_mouse_up(point(edge.x, px(0.0)), MouseButton::Left, Modifiers::none());
    draw(window);
    let top = bounds(window, "call-captions").unwrap().top();
    assert!(
        top >= px(56.0 + 180.0),
        "the stage keeps its room under the header: {top:?}"
    );
    assert!(height(window) > 364.0, "taller than before: {}", height(window));
    let saved = f64::from(saved_height(&shell, &rig));
    assert!(
        (saved - f64::from(height(window))).abs() < 1.0,
        "the height saved is the one shown"
    );
}

#[test]
fn the_edge_s_keys_resize_the_captions_and_keep_the_height() {
    let rig = calling();
    let (shell, window) = captions_shown(&rig, Settings::default());
    tab_to(window, "call-caption-resize-keys");
    window.simulate_keystrokes("up");
    assert!((height(window) - 264.0).abs() < 1.0, "Up makes the area taller");
    assert_eq!(saved_height(&shell, &rig), 264);
    window.simulate_keystrokes("pagedown");
    assert!((height(window) - 164.0).abs() < 1.0, "Page Down makes it shorter");
    window.simulate_keystrokes("home");
    assert!((height(window) - 100.0).abs() < 1.0, "Home: the least height");
    assert_eq!(saved_height(&shell, &rig), 100);
    window.simulate_keystrokes("end");
    assert!(height(window) > 264.0, "End: the tallest");
}

#[test]
fn the_account_s_height_is_kept_for_its_next_call() {
    let rig = calling();
    let me = rig.cx.read(|cx| rig.bridge.read(cx).core().me().to_owned());
    let settings = Settings {
        caption_heights: BTreeMap::from([(me, 400)]),
        ..Settings::default()
    };
    let (_, window) = captions_shown(&rig, settings);
    assert!(
        (height(window) - 400.0).abs() < 1.0,
        "the account's height: {}",
        height(window)
    );
}

#[test]
fn the_edge_at_the_top_resizes_the_other_way() {
    let rig = calling();
    let (shell, window) = captions_shown(&rig, Settings::default());
    click(window, "call-caption-settings");
    window.simulate_keystrokes("down down down down left down enter");
    draw(window);
    let pane = bounds(window, "call-captions").unwrap();
    let edge = bounds(window, "call-caption-resize").unwrap();
    assert!(
        edge.bottom() >= pane.bottom() - px(1.0),
        "the edge is at the area's bottom"
    );
    let from = edge.center();
    let to = point(from.x, from.y + px(80.0));
    window.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    window.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    window.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(window);
    assert_eq!(
        saved_height(&shell, &rig),
        324,
        "a drag down makes the area at the top taller"
    );
}

/// The captions shown with a saved height that the window cannot hold, and the height they are drawn at.
fn too_tall_for_the_window(rig: &Rig) -> (gpui::Entity<Shell>, &'static mut VisualTestContext, f32) {
    let me = rig.cx.read(|cx| rig.bridge.read(cx).core().me().to_owned());
    let settings = Settings {
        caption_heights: BTreeMap::from([(me, 1200)]),
        ..Settings::default()
    };
    let (shell, window) = captions_shown(rig, settings);
    let drawn_height = height(window);
    assert!(
        drawn_height < 1200.0,
        "the window keeps the stage's room: {drawn_height}"
    );
    (shell, window, drawn_height)
}

#[test]
fn a_saved_height_taller_than_the_window_drags_from_the_drawn_height() {
    let rig = calling();
    let (shell, window, drawn_height) = too_tall_for_the_window(&rig);
    drag_edge(window, 40.0);
    assert!(
        (height(window) - (drawn_height - 40.0)).abs() < 1.0,
        "the first drag down makes the drawn area shorter at once: {}",
        height(window)
    );
    let saved = f64::from(saved_height(&shell, &rig));
    assert!((saved - f64::from(height(window))).abs() < 1.0);
}

#[test]
fn a_saved_height_taller_than_the_window_keys_from_the_drawn_height() {
    let rig = calling();
    let (shell, window, drawn_height) = too_tall_for_the_window(&rig);
    tab_to(window, "call-caption-resize-keys");
    window.simulate_keystrokes("down");
    assert!(
        (height(window) - (drawn_height - 20.0)).abs() < 1.0,
        "the first Down makes the drawn area shorter at once: {}",
        height(window)
    );
    let saved = f64::from(saved_height(&shell, &rig));
    assert!((saved - f64::from(height(window))).abs() < 1.0);
}

#[test]
fn the_language_confirmation_s_taller_floor_is_never_saved() {
    let rig = calling();
    let (shell, window) = captions_shown(&rig, Settings::default());
    let start = f64::from(saved_height(&shell, &rig));
    super::caption_language_menu(window);
    click(window, "call-caption-language-sq-al");
    draw(window);
    assert!(drawn(window, "call-caption-language-confirm"));
    assert!(
        f64::from(height(window)) > start + 1.0,
        "the confirmation draws the area taller: {}",
        height(window)
    );
    tab_to(window, "call-caption-resize-keys");
    window.simulate_keystrokes("down");
    assert!(
        (f64::from(saved_height(&shell, &rig)) - (start - 20.0)).abs() < 1.0,
        "a Down saves a shorter height than before"
    );
    drag_edge(window, 40.0);
    assert!(
        (f64::from(saved_height(&shell, &rig)) - (start - 60.0)).abs() < 1.0,
        "a drag down saves a shorter height than before"
    );
    let floor = height(window);
    drag_edge(window, -20.0);
    assert!(
        (height(window) - (floor + 20.0)).abs() < 1.0,
        "a drag up grows the area from its drawn height at once: {} from {floor}",
        height(window)
    );
    window.simulate_keystrokes("down down");
    assert!(
        (height(window) - floor).abs() < 1.0,
        "the area stays drawn at its floor while the saved height goes below it: {}",
        height(window)
    );
    window.simulate_keystrokes("up");
    assert!(
        (height(window) - (floor + 20.0)).abs() < 1.0,
        "an Up grows the area from its drawn height at once: {} from {floor}",
        height(window)
    );
}

/// A drag of the edge down by `dy` px. The edge has its keyboard selector while it has the focus.
fn drag_edge(window: &mut VisualTestContext, dy: f32) {
    let edge = bounds(window, "call-caption-resize-keys")
        .or_else(|| bounds(window, "call-caption-resize"))
        .unwrap()
        .center();
    let to = point(edge.x, edge.y + px(dy));
    window.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
    window.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    window.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    draw(window);
}
