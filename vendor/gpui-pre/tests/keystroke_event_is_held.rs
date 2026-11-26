//! A keystroke's observers and interceptors know if it is a held key's repeat (`KeystrokeEvent::is_held`), so that a
//! view can drop the repeats before they reach a binding. A replayed pending keystroke is not a repeat.

use std::{cell::RefCell, rc::Rc};

use gpui::{
    AnyWindowHandle, AppContext as _, Context, FocusHandle, InteractiveElement as _, IntoElement,
    KeyBinding, KeyDownEvent, Keystroke, KeystrokeEvent, ParentElement as _, PlatformInput, Render,
    Styled as _, TestAppContext, Window, actions, div,
};

actions!(held_test, [Single, Double]);

/// Each observed keystroke: its key, if it is held, and the name of its action.
type Seen = Rc<RefCell<Vec<(String, bool, Option<&'static str>)>>>;

struct Keys {
    focus: FocusHandle,
}

impl Render for Keys {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("Keys")
            .track_focus(&self.focus)
            .on_action(|_: &Single, _, _| {})
            .on_action(|_: &Double, _, _| {})
            .size_full()
            .child("keys")
    }
}

fn key_down(key: &'static str, is_held: bool) -> PlatformInput {
    PlatformInput::KeyDown(KeyDownEvent {
        keystroke: Keystroke::parse(key).unwrap_or_else(|_| panic!("{key} is a keystroke")),
        is_held,
        prefer_character_input: false,
    })
}

fn record(seen: &Seen) -> impl FnMut(&KeystrokeEvent, &mut Window, &mut gpui::App) + 'static {
    let seen = seen.clone();
    move |event, _, _| {
        seen.borrow_mut().push((
            event.keystroke.key.clone(),
            event.is_held,
            event.action.as_ref().map(|action| action.name()),
        ))
    }
}

/// A focused [`Keys`] window, with the keystrokes that its observers and interceptors see.
fn keys_window(cx: &mut TestAppContext) -> (AnyWindowHandle, Seen, Seen) {
    cx.update(|cx| {
        cx.bind_keys([
            KeyBinding::new("s", Single, Some("Keys")),
            KeyBinding::new("a", Single, Some("Keys")),
            KeyBinding::new("a b", Double, Some("Keys")),
        ])
    });
    let window: AnyWindowHandle = cx
        .add_window(|_, cx| Keys {
            focus: cx.focus_handle(),
        })
        .into();
    let observed = Seen::default();
    let intercepted = Seen::default();
    let (observe, intercept) = (record(&observed), record(&intercepted));
    let subscriptions = cx.update(|cx| {
        (
            cx.observe_keystrokes(observe),
            cx.intercept_keystrokes(intercept),
        )
    });
    std::mem::forget(subscriptions);
    let focused = cx.update_window(window, |view, window, cx| {
        let view = view.downcast::<Keys>().expect("a Keys view");
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        window.draw(cx).clear(cx);
    });
    assert!(focused.is_ok(), "focus the view: {focused:?}");
    (window, observed, intercepted)
}

fn press(cx: &mut TestAppContext, window: AnyWindowHandle, keys: &[(&'static str, bool)]) {
    let pressed = cx.update_window(window, |_, window, cx| {
        for (key, is_held) in keys {
            window.dispatch_event(key_down(key, *is_held), cx);
        }
    });
    assert!(pressed.is_ok(), "press {keys:?}: {pressed:?}");
}

fn seen(
    key: &str,
    is_held: bool,
    action: Option<&'static str>,
) -> (String, bool, Option<&'static str>) {
    (key.to_owned(), is_held, action)
}

#[gpui::test]
fn a_held_key_without_a_binding_is_held_for_observers_and_interceptors(cx: &mut TestAppContext) {
    let (window, observed, intercepted) = keys_window(cx);
    press(cx, window, &[("x", false), ("x", true), ("x", true)]);
    let expected = vec![
        seen("x", false, None),
        seen("x", true, None),
        seen("x", true, None),
    ];
    assert_eq!(*intercepted.borrow(), expected);
    assert_eq!(*observed.borrow(), expected);
}

#[gpui::test]
fn a_held_key_that_runs_an_action_is_held_for_its_observers(cx: &mut TestAppContext) {
    let (window, observed, intercepted) = keys_window(cx);
    press(cx, window, &[("s", false), ("s", true)]);
    assert_eq!(
        *observed.borrow(),
        vec![
            seen("s", false, Some("held_test::Single")),
            seen("s", true, Some("held_test::Single"))
        ]
    );
    assert_eq!(
        *intercepted.borrow(),
        vec![seen("s", false, None), seen("s", true, None)]
    );
}

#[gpui::test]
fn a_replayed_pending_keystroke_is_not_held(cx: &mut TestAppContext) {
    let (window, observed, _) = keys_window(cx);
    press(cx, window, &[("a", true), ("x", true)]);
    assert_eq!(
        *observed.borrow(),
        vec![
            seen("a", false, Some("held_test::Single")),
            seen("x", true, None)
        ]
    );
}
