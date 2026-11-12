//! Ending the app's run loop so `Application::run` returns to `main`, which then restarts the app after a sign-in,
//! prints why it stopped, or installs an update.
//!
//! GPUI's own quit on macOS is `-[NSApplication terminate:]`, which exits the process inside `AppKit`: `run` never
//! returns there. On macOS this stops `AppKit`'s run loop instead (`-[NSApplication stop:]`, then an event posted so
//! the loop wakes to see it), as GPUI's other platforms return from `run` after their quit.

use gpui::App;

/// End the run: `run` returns once the current event is handled.
pub fn end(cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        stop_app_kit();
    }
    #[cfg(not(target_os = "macos"))]
    cx.quit();
}

#[cfg(target_os = "macos")]
fn stop_app_kit() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::NSPoint;

    // GPUI calls back on the main thread; anywhere else, nothing is stopped rather than AppKit used off it
    let Some(main_thread) = MainThreadMarker::new() else {
        log::warn!("the app's run wasn't ended: asked off the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(main_thread);
    app.stop(None);
    let wake = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(wake) = wake {
        app.postEvent_atStart(&wake, true);
    }
}
