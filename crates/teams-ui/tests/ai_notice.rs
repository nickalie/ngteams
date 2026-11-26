//! The AI notice as a modal dialog, in the kit's root (which takes Tab), as the app opens it over the shell: it takes
//! the keyboard when it shows, Tab and Shift+Tab stay in it, Enter or Space on a button answers it, nothing behind it
//! takes a key while it waits except the keys of the call that rings or is up, the banner of a call that rings shows
//! over it and takes a click, and the whole notice fits the window at the largest font, a scroll bar beside its points
//! whenever they do not fit.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::{LAB_B, Rig};
use gpui::{
    AppContext as _, Bounds, Entity, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton, Pixels, Point,
    ScrollDelta, ScrollEvent, TestAppContext, TouchPhase, VisualTestContext, point, px, size,
};
use teams_core::{CallAction, CallId, CallStatus, Event, IncomingCall, RegistrationReport};
use teams_proto::Mri;
use teams_ui::ViewTrace;
use teams_ui::settings::{FirstRunState, Settings, SettingsModel};
use teams_ui::shell::{Shell, ShellParts};
use teams_ui::window::KitRoot;

/// The settings of a start that asks about the AI features: the first run's screen confirmed, no answer yet.
fn waiting(font_size: u16) -> SettingsModel {
    let mut settings = Settings {
        first_run: FirstRunState::Confirmed,
        ..Settings::default()
    };
    settings.appearance.font_size = font_size;
    SettingsModel::in_memory(settings).requiring_first_run()
}

/// The shell in the kit's root, as the app opens it: the window follows the settings' font size, and the shell is
/// told to take the keyboard.
fn notice_window(rig: &Rig, settings: SettingsModel) -> (Entity<Shell>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    let settings = rig.cx.update(|cx| cx.new(|_| settings));
    rig.cx
        .update(|cx| teams_ui::theme::follow_settings(&settings, cx))
        .detach();
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: settings.clone(),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let shell = cx.new(|cx| Shell::new(parts, window, cx));
        KitRoot::new(shell, window, cx)
    });
    let shell = cx.update(|_, cx| root.read(cx).view().clone().downcast::<Shell>().unwrap());
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.take_focus(window, cx)));
    draw(cx);
    (shell, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

/// Presses each key of `keys` (down and up, as a person does: a focused button is clicked on the key's release).
fn press(cx: &mut VisualTestContext, keys: &'static str) {
    for spec in keys.split(' ') {
        let keystroke = Keystroke::parse(spec).unwrap();
        draw(cx);
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.run_until_parked();
    }
}

fn theme_font() -> u16 {
    teams_ui::theme::FONT_SIZE
}

fn answer(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<bool> {
    cx.update(|_, cx| shell.read(cx).settings().read(cx).settings().ai_features)
}

fn inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.left() >= outer.left()
        && inner.right() <= outer.right()
        && inner.top() >= outer.top()
        && inner.bottom() <= outer.bottom()
}

#[test]
fn the_notice_takes_the_keyboard_and_tab_and_shift_tab_stay_in_it() {
    // Tab from the notice: "Turn AI features off", then "Keep AI features on", then round again; Shift+Tab backwards
    let cases = [
        ("tab enter", false),
        ("tab tab enter", true),
        ("tab tab tab space", false),
        ("shift-tab enter", true),
        ("shift-tab shift-tab space", false),
        ("tab tab tab tab tab shift-tab enter", true),
    ];
    for (keys, on) in cases {
        let rig = Rig::start();
        let (shell, cx) = notice_window(&rig, waiting(theme_font()));
        assert!(bounds(cx, "ai-notice").is_some(), "the notice waits");
        press(cx, keys);
        assert_eq!(answer(&shell, cx), Some(on), "{keys} answers {on}");
        assert!(bounds(cx, "ai-notice").is_none(), "{keys}: the notice is gone");
    }
}

#[test]
fn nothing_behind_the_notice_takes_a_key_and_the_keys_come_back_after_the_answer() {
    let rig = Rig::start();
    let (shell, cx) = notice_window(&rig, waiting(theme_font()));
    press(cx, "ctrl-k");
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_none()),
        "Ctrl+K opens nothing behind the notice"
    );
    assert!(bounds(cx, "ai-notice").is_some());
    // the zoom works over the notice: the user can make it readable
    press(cx, "ctrl-=");
    let font = cx.update(|_, cx| shell.read(cx).settings().read(cx).settings().appearance.font_size);
    assert_eq!(font, theme_font() + 1);
    press(cx, "tab enter");
    assert_eq!(answer(&shell, cx), Some(false));
    press(cx, "ctrl-k");
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_some()),
        "the shell has the keyboard again after the answer"
    );
}

#[test]
fn a_call_that_rings_while_the_notice_waits_is_answered_or_declined_from_the_keys() {
    let rig = Rig::start_with(|cx| teams_ui::Models::with_calls(None, cx));
    let (shell, cx) = notice_window(&rig, waiting(theme_font()));
    rig.send(Event::CallingRegistration(RegistrationReport::Registered));
    let incoming = |number: u64| {
        rig.send(Event::IncomingCall(IncomingCall {
            call: CallId::for_tests(number),
            caller: Mri::new(LAB_B),
            caller_name: None,
            group_chat: None,
            meeting: None,
        }));
    };
    let actions = || rig.core.call_actions.lock().unwrap().clone();
    incoming(1);
    press(cx, "ctrl-shift-d");
    assert_eq!(
        actions(),
        [CallAction::Decline {
            call: CallId::for_tests(1)
        }]
    );
    incoming(2);
    press(cx, "ctrl-shift-s");
    assert_eq!(
        actions()[1],
        CallAction::Answer {
            call: CallId::for_tests(2)
        }
    );
    rig.send(Event::Call {
        call: CallId::for_tests(2),
        status: CallStatus::Connected,
    });
    press(cx, "ctrl-shift-h");
    assert_eq!(
        actions()[2],
        CallAction::HangUp {
            call: CallId::for_tests(2)
        }
    );
    // a new call does not start from behind the notice
    press(cx, "ctrl-shift-c");
    assert_eq!(actions().len(), 3);
    assert!(bounds(cx, "ai-notice").is_some(), "the notice still waits");
    assert_eq!(answer(&shell, cx), None);
}

/// A click (down and up) at `at`, on what the last frame drew there.
fn click_at(cx: &mut VisualTestContext, at: Point<Pixels>) {
    draw(cx);
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
}

/// The settings of [`waiting`], in the theme `theme` (not the system's).
fn waiting_in(theme: &'static str) -> SettingsModel {
    let mut settings = Settings {
        first_run: FirstRunState::Confirmed,
        ..Settings::default()
    };
    settings.appearance.follow_system = false;
    settings.appearance.theme = theme;
    SettingsModel::in_memory(settings).requiring_first_run()
}

/// A click at the centre of `button`, which the notice covers: it is not under the dialog, only under its backdrop.
fn click_behind_the_notice(cx: &mut VisualTestContext, button: &str) {
    let found = bounds(cx, button).expect("the button behind the notice");
    let dialog = bounds(cx, "ai-notice").expect("the notice");
    assert!(
        !dialog.contains(&found.center()),
        "{button} {found:?} is beside the notice {dialog:?}"
    );
    click_at(cx, found.center());
}

fn ring(rig: &Rig, number: u64) {
    rig.send(Event::IncomingCall(IncomingCall {
        call: CallId::for_tests(number),
        caller: Mri::new(LAB_B),
        caller_name: None,
        group_chat: None,
        meeting: None,
    }));
}

fn call_actions(rig: &Rig) -> Vec<CallAction> {
    rig.core.call_actions.lock().unwrap().clone()
}

/// Call 1 rings and its banner's Decline is clicked over the notice, then call 2 rings, its Answer is clicked and it
/// connects. The banner is at the call bar's place, the sheet's top left.
fn decline_and_answer_from_the_banner(rig: &Rig, cx: &mut VisualTestContext, theme: &str) {
    for (number, button, action) in [
        (
            1,
            "call-decline",
            CallAction::Decline {
                call: CallId::for_tests(1),
            },
        ),
        (
            2,
            "call-answer",
            CallAction::Answer {
                call: CallId::for_tests(2),
            },
        ),
    ] {
        ring(rig, number);
        let banner = bounds(cx, "call-banner").expect("the banner");
        let bar = bounds(cx, "call-bar").expect("the call bar");
        assert_eq!(
            banner.origin, bar.origin,
            "{theme}: the banner is at the sheet's top left"
        );
        let found = bounds(cx, button).expect("the banner's button");
        click_at(cx, found.center());
        assert_eq!(
            call_actions(rig).last(),
            Some(&action),
            "{theme}: a click on {button} over the notice"
        );
    }
    rig.send(Event::Call {
        call: CallId::for_tests(2),
        status: CallStatus::Connected,
    });
}

/// The strip of the call that is up stays behind the notice, also while call 3 rings over it; the banner of call 3
/// takes its Decline.
fn the_strip_stays_behind(rig: &Rig, cx: &mut VisualTestContext, theme: &str) {
    let taken = call_actions(rig).len();
    click_behind_the_notice(cx, "call-hang-up");
    assert_eq!(
        call_actions(rig).len(),
        taken,
        "{theme}: Leave behind the notice takes no click"
    );
    ring(rig, 3);
    click_behind_the_notice(cx, "call-hang-up");
    assert_eq!(
        call_actions(rig).len(),
        taken,
        "{theme}: Leave behind the notice takes no click while a call rings"
    );
    let found = bounds(cx, "call-decline").expect("the banner of the second call");
    click_at(cx, found.center());
    let declined = CallAction::Decline {
        call: CallId::for_tests(3),
    };
    assert_eq!(
        call_actions(rig).last(),
        Some(&declined),
        "{theme}: the banner takes a click during a call"
    );
}

#[test]
fn the_banner_of_a_call_that_rings_while_the_notice_waits_shows_over_it_and_takes_a_click() {
    // the light theme has depth (the sheet's rounded corners), High Contrast is flat
    for (theme, depth) in [(teams_ui::theme::LIGHT_KEY, true), ("high-contrast", false)] {
        let rig = Rig::start_with(|cx| teams_ui::Models::with_calls(None, cx));
        let (shell, cx) = notice_window(&rig, waiting_in(theme));
        assert_eq!(cx.update(|_, cx| teams_ui::theme::current(cx).depth), depth, "{theme}");
        rig.send(Event::CallingRegistration(RegistrationReport::Registered));
        decline_and_answer_from_the_banner(&rig, cx, theme);
        the_strip_stays_behind(&rig, cx, theme);
        assert!(bounds(cx, "ai-notice").is_some(), "{theme}: the notice still waits");
        assert_eq!(answer(&shell, cx), None);
        // the user answers the notice while a call rings: after it, the banner and the strip are as without the notice
        ring(&rig, 4);
        press(cx, "tab enter");
        assert_eq!(
            answer(&shell, cx),
            Some(false),
            "{theme}: the notice keeps the keyboard"
        );
        assert!(bounds(cx, "ai-notice").is_none());
        assert!(bounds(cx, "call-banner").is_some(), "{theme}: the call still rings");
        let found = bounds(cx, "call-hang-up").expect("the strip of the call that is up");
        click_at(cx, found.center());
        let left = CallAction::HangUp {
            call: CallId::for_tests(2),
        };
        assert_eq!(
            call_actions(&rig).last(),
            Some(&left),
            "{theme}: Leave takes a click after the answer"
        );
    }
}

#[test]
fn the_points_show_a_scroll_bar_only_when_they_do_not_fit() {
    let largest = *teams_ui::theme::FONT_SIZES.end();
    for (font, width, height, cut) in [
        (theme_font(), 1280.0, 800.0, false),
        (largest, 1280.0, 800.0, false),
        (largest, 1280.0, 480.0, true),
        (largest, 520.0, 800.0, true),
    ] {
        let rig = Rig::start();
        let (_, cx) = notice_window(&rig, waiting(font));
        cx.simulate_resize(size(px(width), px(height)));
        draw(cx);
        let body = bounds(cx, "ai-notice-body").expect("the points");
        let later = bounds(cx, "ai-notice-later").expect("the last line");
        assert_eq!(
            later.bottom() > body.bottom(),
            cut,
            "font {font}, {width}x{height}: the points are cut"
        );
        let scrollbar = bounds(cx, "ai-notice-scrollbar");
        assert_eq!(
            scrollbar.is_some(),
            cut,
            "font {font}, {width}x{height}: the scroll bar shows only when the points are cut"
        );
        let Some(scrollbar) = scrollbar else { continue };
        let dialog = bounds(cx, "ai-notice").expect("the notice");
        assert!(
            inside(scrollbar, dialog) && scrollbar.left() >= body.right(),
            "font {font}, {width}x{height}: the scroll bar {scrollbar:?} is right of the points {body:?}, in the notice"
        );
        assert_eq!((scrollbar.top(), scrollbar.bottom()), (body.top(), body.bottom()));
        // a click low on the bar's track scrolls the last line into view
        click_at(cx, point(scrollbar.center().x, scrollbar.bottom() - px(2.0)));
        let scrolled = bounds(cx, "ai-notice-later").expect("the last line");
        assert!(
            scrolled.bottom() <= body.bottom(),
            "font {font}, {width}x{height}: the scroll bar brings the last line into the box"
        );
    }
}

#[test]
fn the_whole_notice_fits_the_window_at_the_largest_font_and_its_points_scroll() {
    let largest = *teams_ui::theme::FONT_SIZES.end();
    let rig = Rig::start();
    let (_, cx) = notice_window(&rig, waiting(largest));
    for (width, height) in [(1280.0, 800.0), (720.0, 800.0), (520.0, 800.0), (520.0, 480.0)] {
        cx.simulate_resize(size(px(width), px(height)));
        let window = Bounds::new(point(px(0.0), px(0.0)), size(px(width), px(height)));
        let dialog = bounds(cx, "ai-notice").expect("the notice");
        assert!(
            inside(dialog, window),
            "{width}x{height}: the notice {dialog:?} is inside the window"
        );
        for button in ["ai-notice-off", "ai-notice-on"] {
            let found = bounds(cx, button).expect("the button");
            assert!(
                inside(found, dialog),
                "{width}x{height}: {button} {found:?} is inside {dialog:?}"
            );
        }
    }
    // 520 × 480: the points do not fit, and the wheel scrolls them
    let body = bounds(cx, "ai-notice-body").expect("the points");
    let later = bounds(cx, "ai-notice-later").expect("the last line");
    assert!(later.bottom() > body.bottom(), "the points are higher than their box");
    cx.simulate_event(ScrollEvent {
        position: body.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-2000.0))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    });
    let scrolled = bounds(cx, "ai-notice-later").expect("the last line");
    assert!(
        scrolled.bottom() <= body.bottom(),
        "the wheel brings the last line into the box"
    );
}
