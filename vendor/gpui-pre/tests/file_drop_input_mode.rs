//! A file manager's drag is pointer input: typed keys before it must not hide the drop target.

use std::{cell::RefCell, path::PathBuf, rc::Rc};

use gpui::{
    AnyWindowHandle, AppContext as _, Context, ExternalPaths, FileDropEvent, InputEvent as _,
    InteractiveElement as _, IntoElement, KeyDownEvent, Keystroke, PlatformInput, Render,
    Styled as _, TestAppContext, Window, div, point, px,
};

struct DropTarget(Rc<RefCell<Vec<PathBuf>>>);

impl Render for DropTarget {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let drops = self.0.clone();
        div()
            .size_full()
            .on_drop(move |paths: &ExternalPaths, _, _| {
                drops.borrow_mut().extend_from_slice(paths.paths())
            })
    }
}

fn key() -> PlatformInput {
    PlatformInput::KeyDown(KeyDownEvent {
        keystroke: Keystroke::parse("a").expect("valid keystroke"),
        is_held: false,
        prefer_character_input: false,
    })
}

#[gpui::test]
fn file_drop_after_typing_reaches_the_element_under_the_pointer(cx: &mut TestAppContext) {
    let drops = Rc::new(RefCell::new(Vec::new()));
    let window: AnyWindowHandle = cx
        .add_window({
            let drops = drops.clone();
            move |_, _| DropTarget(drops)
        })
        .into();
    let path = PathBuf::from("/tmp/typed-then-dropped");
    let position = point(px(30.), px(30.));
    let update_result = cx.update_window(window, |_, window, cx| {
        window.draw(cx).clear(cx);
        window.dispatch_event(key(), cx);
        assert!(window.last_input_was_keyboard());

        window.dispatch_event(
            FileDropEvent::Entered {
                position,
                paths: ExternalPaths([path.clone()].into_iter().collect()),
            }
            .to_platform_input(),
            cx,
        );
        assert!(!window.last_input_was_keyboard());

        window.dispatch_event(key(), cx);
        window.dispatch_event(FileDropEvent::Pending { position }.to_platform_input(), cx);
        assert!(!window.last_input_was_keyboard());

        window.dispatch_event(key(), cx);
        window.dispatch_event(FileDropEvent::Submit { position }.to_platform_input(), cx);
        assert!(!window.last_input_was_keyboard());
        assert!(!cx.has_active_drag());

        window.dispatch_event(FileDropEvent::Exited.to_platform_input(), cx);
        window.dispatch_event(FileDropEvent::Ended.to_platform_input(), cx);
        assert!(!window.last_input_was_keyboard());

        // keyboard navigation and focus rings come back with the next key
        window.dispatch_event(key(), cx);
        assert!(window.last_input_was_keyboard());
    });
    assert!(
        update_result.is_ok(),
        "failed to drop after typing: {update_result:?}"
    );
    assert_eq!(drops.borrow().as_slice(), [path]);
}
