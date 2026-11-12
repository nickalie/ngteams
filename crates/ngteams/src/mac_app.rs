//! The running app on macOS's menu bar and Dock, as Teams is there.
//!
//! - **The menu bar** has the app's menu (Hide ngteams ⌘H, Hide Others ⌥⌘H, Show All, Quit ngteams ⌘Q) and a Window
//!   menu (Minimize ⌘M, Close Window ⌘W). Close and Minimize act on the key window as its own buttons do, so a call's
//!   window asks before it closes as it does when its close button is clicked.
//! - **Quit, from anywhere, is the app's quit** ([`crate::app::quit`]: every account's engine shut down first): ⌘Q and
//!   the menu's Quit ask it directly; the Dock's Quit and a logout or shutdown, which `AppKit` asks of the app's
//!   delegate (`applicationShouldTerminate:`), are answered "later" and the app's quit begins; once it has ended,
//!   [`crate::run_end::end`] lets `AppKit` terminate the process ([`answer_terminate`]).
//! - **The bundle's `GStreamer`** ([`bundled_gstreamer`]): `ngteams.app` carries `GStreamer`'s runtime in
//!   `Contents/Frameworks/gstreamer/` as `lib/`, `plugins/` and `libexec/` (a dotted folder such as `gstreamer-1.0`
//!   reads as a bundle to `codesign`), so the app names its plugins, scanner and registry to `GStreamer` itself.
//! - **The Dock** shows the main window again when its icon is clicked while the app has no window open
//!   ([`reopened`]: closing the window keeps the app in the Dock, `teams_ui::desktop`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, AsyncApp, KeyBinding, Menu, MenuItem, actions};
use objc2::MainThreadMarker;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::sel;
use objc2_app_kit::NSApplication;

actions!(ngteams_app, [Quit, Hide, HideOthers, ShowAll, Minimize, CloseWindow]);

/// `AppKit`'s answers to `applicationShouldTerminate:` (`NSApplicationTerminateReply`).
const TERMINATE_NOW: usize = 1;
const TERMINATE_LATER: usize = 2;

/// What asks the app's quit for `AppKit` (the running app's, replaced by each launch).
static ASK_QUIT: Mutex<Option<async_channel::Sender<()>>> = Mutex::new(None);
/// `AppKit` waits for the answer to its terminate.
static TERMINATING: AtomicBool = AtomicBool::new(false);

/// The menu bar, its keys and the delegate's terminate, for the app that runs now.
pub fn install(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
    cx.on_action(|_: &Quit, cx| crate::app::quit(cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, _| key_window(|window| window.performMiniaturize(None)));
    cx.on_action(|_: &CloseWindow, _| key_window(|window| window.performClose(None)));
    cx.set_menus([
        Menu {
            name: "ngteams".into(),
            items: vec![
                MenuItem::action("Hide ngteams", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit ngteams", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Close Window", CloseWindow),
            ],
            disabled: false,
        },
    ]);
    route_terminate(cx);
}

/// Points `GStreamer` at the runtime `ngteams.app` carries, when this program runs from a bundle that has one: its
/// plugins, its plugin scanner, and a registry of the app's own in `~/Library/Caches/ngteams`. What the environment
/// already names is kept. Called first thing in `main`, before any thread starts.
pub fn bundled_gstreamer() {
    let Some(runtime) = std::env::current_exe()
        .ok()
        .and_then(|program| Some(program.parent()?.parent()?.join("Frameworks/gstreamer")))
        .filter(|runtime| runtime.join("plugins").is_dir())
    else {
        return;
    };
    let registry = std::env::var_os("HOME")
        .map(|home| std::path::PathBuf::from(home).join("Library/Caches/ngteams/gstreamer-registry.bin"));
    let names = [
        ("GST_PLUGIN_SYSTEM_PATH_1_0", Some(runtime.join("plugins"))),
        (
            "GST_PLUGIN_SCANNER_1_0",
            Some(runtime.join("libexec/gst-plugin-scanner")),
        ),
        ("GST_REGISTRY_1_0", registry),
    ];
    for (name, value) in names {
        if let Some(value) = value
            && std::env::var_os(name).is_none()
        {
            // SAFETY: `main` calls this before it starts any thread, so nothing reads the environment meanwhile
            unsafe { std::env::set_var(name, value) };
        }
    }
}

/// The Dock's icon was clicked with no window open: the main window is shown again (opened over the same models).
pub fn reopened(cx: &mut App) {
    if let Some(desktop) = crate::app::desktop(cx) {
        desktop.update(cx, teams_ui::desktop::Desktop::show);
    }
}

/// Does `then` with the key window, if there is one.
fn key_window(then: impl FnOnce(&objc2_app_kit::NSWindow)) {
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    if let Some(window) = NSApplication::sharedApplication(main_thread).keyWindow() {
        then(&window);
    }
}

/// `AppKit`'s terminate (the Dock's Quit, a logout) asks the app's quit from now on.
fn route_terminate(cx: &mut App) {
    let (ask, asked) = async_channel::bounded(1);
    *ASK_QUIT.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ask);
    cx.spawn(async move |cx: &mut AsyncApp| {
        while asked.recv().await.is_ok() {
            log::info!("the app quits: macOS asked it to (the Dock's Quit, a logout)");
            cx.update(crate::app::quit);
        }
    })
    .detach();
    let Some(delegate) = AnyClass::get(c"GPUIApplicationDelegate") else {
        log::warn!("the Dock's Quit ends the app at once: GPUI's app delegate wasn't found");
        return;
    };
    let should_terminate: unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize = should_terminate;
    // SAFETY: the two signatures differ only in their arguments and result; the runtime calls the method through
    // `Imp` with the types it is added with below, which are `should_terminate`'s.
    let imp = unsafe {
        std::mem::transmute::<unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize, Imp>(
            should_terminate,
        )
    };
    // SAFETY: the types (`NSApplicationTerminateReply` is an `NSUInteger`, then self, _cmd and the sender) are
    // `should_terminate`'s; the class is GPUI's registered delegate class, alive for the process. Added once: a second
    // add answers NO and changes nothing.
    unsafe {
        objc2::ffi::class_addMethod(
            std::ptr::from_ref(delegate).cast_mut(),
            sel!(applicationShouldTerminate:),
            imp,
            c"Q@:@".as_ptr(),
        );
    }
}

/// `applicationShouldTerminate:`: "later", and the app's quit asked; "now" when no app runs to ask it of.
unsafe extern "C-unwind" fn should_terminate(_: *mut AnyObject, _: Sel, _: *mut AnyObject) -> usize {
    let asked = ASK_QUIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        // a quit already asked (the channel full) ends the same way
        .is_some_and(|ask| !matches!(ask.try_send(()), Err(async_channel::TrySendError::Closed(()))));
    if asked {
        TERMINATING.store(true, Ordering::SeqCst);
        TERMINATE_LATER
    } else {
        TERMINATE_NOW
    }
}

/// When `AppKit` waits for the answer to its terminate, the app's quit has ended: it is told to go on, and the
/// process ends inside `AppKit` (its terminate). Answers whether it was waiting.
pub(crate) fn answer_terminate(cx: &mut App) -> bool {
    if !TERMINATING.swap(false, Ordering::SeqCst) {
        return false;
    }
    // answered outside the app's update: GPUI's own quit, which `AppKit`'s terminate calls, takes the app
    cx.spawn(async move |_: &mut AsyncApp| {
        if let Some(main_thread) = MainThreadMarker::new() {
            NSApplication::sharedApplication(main_thread).replyToApplicationShouldTerminate(true);
        }
    })
    .detach();
    true
}
