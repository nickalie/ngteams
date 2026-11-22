//! One page: a `WebKitWebView` on the engine's display, its `WebKit` signals turned into [`Event`]s, and the
//! commands that drive it.
//!
//! What a page may not do is refused here, as the HTML overlay refuses it (`agterm-gtk`'s `html_overlay/page.rs`): no
//! new windows (`create` is left unconnected, so `WebKit` makes none), script dialogs answered silently, no file
//! chooser, no permission, no full screen, no notification. The context menu is the consumer's to draw
//! ([`super::interaction`]), so its download and new-window items never exist. Downloads are refused per store
//! ([`super::create_session`]).

use std::cell::Cell;
use std::rc::Rc;

use async_channel::Sender;
use glib::prelude::*;
use glib::translate::ToGlibPtr;

use super::platform::{Display, Link, View};
use super::webkit::{
    JscValue, NavigationPolicyDecision, NetworkSession, PolicyDecision, ResponsePolicyDecision, ScriptReply, Settings,
    UserContentManager, WebView, enum_arg, error_is, handled, object_arg, pointer_arg,
};
use crate::event::{
    Event, LoadError, LoadErrorKind, LoadEvent, NavigationType, PageState, PolicyKind, ScriptMessage, Termination,
};
use crate::host::{Evaluation, Load, ViewCommand};
use crate::input::PointerEventType as _;
use crate::input::{Input, Key, Pointer, PointerKind, Scroll};
use crate::platform_sys as platform;
use crate::spec::{Geometry, MessageHandler, Rgba, ViewId, ViewSpec};
use crate::webkit_sys as webkit;

/// A page's web view and what it was built with.
pub(crate) struct Page {
    pub(crate) id: ViewId,
    pub(crate) web_view: WebView,
    view: View,
    settings: Settings,
    content: UserContentManager,
    handlers: Vec<MessageHandler>,
    events: Sender<Event>,
    /// The last state reported, so an unchanged one is not reported again.
    reported: std::cell::RefCell<PageState>,
    closed: Cell<bool>,
    /// The page renders dmabufs (the engine's mode, until [`Page::fall_back_to_shm`]).
    dmabuf: Cell<bool>,
    /// Where the last pointer press was, in logical pixels: a context menu that names no position opens there.
    pressed_at: Rc<Cell<(i32, i32)>>,
    /// The web process is being ended for the fall-back to SHM: its termination is not reported, and the page
    /// reloads.
    falling_back: Cell<bool>,
    /// The reply channels of the evaluations `WebKit` has not completed yet.
    evaluations: std::cell::RefCell<Vec<async_channel::WeakSender<Evaluation>>>,
}

impl Page {
    /// Builds page `id` from `spec` on `display` and `session`.
    pub(crate) fn new(
        id: ViewId,
        spec: &ViewSpec,
        display: &Display,
        session: &NetworkSession,
        events: &Sender<Event>,
    ) -> Result<Rc<Self>, String> {
        let settings = Settings::new(spec.settings);
        if let Some(missing) = spec
            .features_off
            .iter()
            .find(|feature| !settings.set_feature(feature, false))
        {
            return Err(format!("WebKit has no feature {missing} to switch off"));
        }
        let content = UserContentManager::new();
        content.set_scripts(&spec.scripts);
        for handler in &spec.message_handlers {
            if !content.register_handler(&handler.name, handler.world.as_deref()) {
                return Err(format!("the message handler {} is registered twice", handler.name));
            }
        }
        let web_view: WebView = glib::Object::builder()
            .property("display", display)
            .property("network-session", session)
            .property("settings", &settings)
            .property("user-content-manager", &content)
            .build();
        let view = view_of(&web_view).ok_or("the web view has no view of the engine's display")?;
        view.attach(Link {
            id,
            events: events.clone(),
        });
        let page = Rc::new(Self {
            id,
            web_view,
            view,
            settings,
            content,
            handlers: spec.message_handlers.clone(),
            events: events.clone(),
            reported: std::cell::RefCell::default(),
            closed: Cell::new(false),
            dmabuf: Cell::new(display.renders_dmabuf()),
            pressed_at: Rc::new(Cell::new((0, 0))),
            falling_back: Cell::new(false),
            evaluations: std::cell::RefCell::default(),
        });
        if let Some(background) = spec.background {
            page.web_view.set_background_color(background);
        }
        page.web_view.set_zoom_level(spec.zoom);
        page.set_geometry(spec.geometry);
        page.connect_loads();
        page.connect_reports();
        page.connect_policy();
        page.connect_messages();
        page.connect_refusals();
        super::interaction::connect_option_menu(&page.web_view, id);
        let pressed_at = Rc::clone(&page.pressed_at);
        super::interaction::connect_context_menu(&page.web_view, id, events.clone(), move || pressed_at.get());
        Ok(page)
    }

    fn send(&self, event: Event) {
        if !self.closed.get() && self.events.try_send(event).is_err() {
            log::debug!("nobody listens to the WPE engine's events any more");
        }
    }

    /// Runs one command.
    pub(crate) fn apply(self: &Rc<Self>, command: ViewCommand) {
        match command {
            ViewCommand::Load(Load::Uri(uri)) => self.web_view.load_uri(&uri),
            ViewCommand::Load(Load::Html { html, base_uri }) => self.web_view.load_html(&html, base_uri.as_deref()),
            ViewCommand::Reload => self.web_view.reload(),
            ViewCommand::Stop => self.web_view.stop_loading(),
            ViewCommand::GoBack => self.web_view.go_back(),
            ViewCommand::GoForward => self.web_view.go_forward(),
            ViewCommand::Zoom(zoom) => self.web_view.set_zoom_level(zoom),
            ViewCommand::Scripts(scripts) => self.content.set_scripts(&scripts),
            ViewCommand::Background(background) => {
                self.web_view.set_background_color(background.unwrap_or(Rgba::WHITE));
            }
            ViewCommand::Geometry(geometry) => self.set_geometry(geometry),
            ViewCommand::Visible(visible) => self.set_visible(visible),
            ViewCommand::Focused(focused) => self.set_focused(focused),
            ViewCommand::Input(input) => self.input(input),
            ViewCommand::Presented(buffer) => self.view.presented(buffer),
            ViewCommand::FallBackToShm => self.fall_back_to_shm(),
            ViewCommand::Editing(command) => super::interaction::editing_command(&self.web_view, command),
            ViewCommand::ImePreedit { text, cursor } => self.with_input_method(|method| method.preedit(&text, cursor)),
            ViewCommand::ImeCommit(text) => self.with_input_method(|method| method.commit(&text)),
            ViewCommand::Evaluate { script, world, reply } => self.evaluate(&script, world.as_deref(), reply),
        }
    }

    /// Runs `change` on the view's input method; `WebKit` asks the display for one once a field takes the keyboard.
    fn with_input_method(&self, change: impl FnOnce(&super::ime::InputMethod)) {
        if let Some(method) = self.view.input_method() {
            change(&method);
        } else {
            log::debug!("input method text for a page whose field has no input method yet");
        }
    }

    /// Draws on the CPU into SHM buffers from now on (the consumer could not import its dmabufs): `WebKit`'s
    /// `HardwareAcceleration` feature goes off in the page's settings, which a web process reads only when it builds
    /// the page's surface, so the page's web process is ended and the page reloads in a new one, whose surface
    /// hands over SHM buffers. A page that renders SHM already is left alone.
    fn fall_back_to_shm(&self) {
        if !self.dmabuf.replace(false) {
            return;
        }
        if !self.settings.set_hardware_acceleration(false) {
            log::warn!("this WPE WebKit has no HardwareAcceleration feature: the page cannot fall back to SHM");
            return;
        }
        self.falling_back.set(true);
        self.web_view.terminate_web_process();
    }

    /// The web process ended: a fall-back reloads the page silently, anything else is reported; nothing for a closed
    /// page (its web process ended with it, [`Page::end_web_process`]).
    fn terminated(&self, reason: Termination) {
        if self.closed.get() {
            return;
        }
        if reason == Termination::TerminatedByApi && self.falling_back.replace(false) {
            self.web_view.reload();
            return;
        }
        self.send(Event::WebProcessTerminated { view: self.id, reason });
    }

    fn set_geometry(&self, geometry: Geometry) {
        let Some(toplevel) = self.view.toplevel() else {
            log::warn!("a WPE view without a toplevel cannot be resized");
            return;
        };
        let width = i32::try_from(geometry.width.max(1)).unwrap_or(i32::MAX);
        let height = i32::try_from(geometry.height.max(1)).unwrap_or(i32::MAX);
        let scale = if geometry.scale.is_finite() && geometry.scale > 0.0 {
            geometry.scale
        } else {
            1.0
        };
        toplevel.set_geometry(width, height, scale);
    }

    fn set_visible(&self, visible: bool) {
        // SAFETY: a live view.
        unsafe { platform::wpe_view_set_visible(self.view.as_wpe(), glib::ffi::gboolean::from(visible)) };
    }

    fn set_focused(&self, focused: bool) {
        if focused {
            // SAFETY: a live view.
            unsafe { platform::wpe_view_focus_in(self.view.as_wpe()) };
        } else {
            // SAFETY: a live view.
            unsafe { platform::wpe_view_focus_out(self.view.as_wpe()) };
        }
    }

    fn evaluate(&self, script: &str, world: Option<&str>, reply: Sender<Evaluation>) {
        {
            let mut pending = self.evaluations.borrow_mut();
            pending.retain(|reply| reply.upgrade().is_some());
            pending.push(reply.downgrade());
        }
        self.web_view.evaluate_javascript(script, world, move |result| {
            let result = result
                .map(|value| {
                    value
                        .filter(|value| !value.is_undefined())
                        .and_then(|value| value.to_json())
                })
                .map_err(|err| err.message().to_owned());
            // the caller may have stopped waiting
            let _ = reply.try_send(result);
        });
    }

    /// Closes the reply channel of each evaluation still open: a closed page answers none, and after the engine's
    /// stop `WebKit` completes none.
    fn abandon_evaluations(&self) {
        for reply in self
            .evaluations
            .take()
            .iter()
            .filter_map(async_channel::WeakSender::upgrade)
        {
            reply.close();
        }
    }

    /// Ends the page: no more events, its handlers and load go, its view stops sending frames.
    pub(crate) fn close(&self) {
        self.closed.set(true);
        self.abandon_evaluations();
        for handler in &self.handlers {
            self.content.unregister_handler(&handler.name, handler.world.as_deref());
        }
        self.web_view.stop_loading();
        self.view.detach();
    }

    /// Ends the web process of the closed page at once (the engine's stop), so it never outlives the engine's
    /// process. Left to itself it would end only once its main thread saw the UI process go, and that path does not
    /// end cleanly: `WebKit`'s `GLib` ports stop the web process's main loop there for a normal exit, a main thread
    /// that never gets back to it (a page spinning in a script) has the process abort with a core dump 10 s on
    /// (`crashAfter10Seconds` in `Source/WebKit/WebProcess/WebProcess.cpp` at `wpewebkit-2.54.1`), long after the app
    /// is gone, and now and then even an idle page's process aborts on a release assertion of its own on the way
    /// (`WTF::releaseAssertIsCurrent`, `Source/WTF/wtf/RunLoop.h`). Ended through `WebKit`
    /// (`webkit_web_view_terminate_web_process`, a `SIGKILL` from its process launcher), it goes at once and dumps
    /// nothing; what it held that lasts (cookies, storage) lives in the network process, which ends on its own. macOS's
    /// `WebKit` ends its web processes as abruptly when the UI process goes (`callExit`, an `_exit` off the main
    /// thread).
    pub(crate) fn end_web_process(&self) {
        self.web_view.terminate_web_process();
    }

    // MARK: - input

    fn input(&self, input: Input) {
        let view = self.view.as_wpe();
        let time = event_time();
        if let Input::Pointer(Pointer {
            kind: PointerKind::Down(_),
            x,
            y,
            ..
        }) = input
        {
            #[expect(clippy::cast_possible_truncation, reason = "a position in a page fits an i32")]
            let at = (x as i32, y as i32);
            self.pressed_at.set(at);
        }
        let event = match input {
            Input::Pointer(pointer) => pointer_event(view, time, pointer),
            Input::Scroll(scroll) => scroll_event(view, time, scroll),
            Input::Key(key) => key_event(view, time, key),
        };
        if event.is_null() {
            return;
        }
        // SAFETY: a live view and an event made for it.
        unsafe { platform::wpe_view_event(view, event) };
        // SAFETY: drops the event's only reference, ours.
        unsafe { platform::wpe_event_unref(event) };
    }

    // MARK: - signals

    fn connect_loads(self: &Rc<Self>) {
        let page = Rc::downgrade(self);
        self.web_view.connect_local("load-changed", false, move |values| {
            let event = match enum_arg(values, 1) {
                webkit::WEBKIT_LOAD_STARTED => LoadEvent::Started,
                webkit::WEBKIT_LOAD_REDIRECTED => LoadEvent::Redirected,
                webkit::WEBKIT_LOAD_COMMITTED => LoadEvent::Committed,
                _ => LoadEvent::Finished,
            };
            if let Some(page) = page.upgrade() {
                page.send(Event::Load { view: page.id, event });
                page.report();
            }
            None
        });
        let page = Rc::downgrade(self);
        // handled, so WebKit shows no error page of its own
        self.web_view.connect_local("load-failed", false, move |values| {
            let uri = values
                .get(2)
                .and_then(|value| value.get::<Option<String>>().ok().flatten());
            let error = values.get(3).and_then(|value| value.get::<glib::Error>().ok());
            if let (Some(page), Some(error)) = (page.upgrade(), error) {
                page.send(Event::LoadFailed {
                    view: page.id,
                    uri: uri.unwrap_or_default(),
                    error: load_error(&error),
                });
            }
            handled(true)
        });
        let page = Rc::downgrade(self);
        self.web_view
            .connect_local("web-process-terminated", false, move |values| {
                let reason = match enum_arg(values, 1) {
                    webkit::WEBKIT_WEB_PROCESS_CRASHED => Termination::Crashed,
                    webkit::WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT => Termination::ExceededMemoryLimit,
                    _ => Termination::TerminatedByApi,
                };
                if let Some(page) = page.upgrade() {
                    page.terminated(reason);
                }
                None
            });
    }

    fn connect_reports(self: &Rc<Self>) {
        for property in ["title", "uri"] {
            let page = Rc::downgrade(self);
            self.web_view.connect_notify_local(Some(property), move |_, _| {
                if let Some(page) = page.upgrade() {
                    page.report();
                }
            });
        }
        if let Some(history) = self.web_view.back_forward_list() {
            let page = Rc::downgrade(self);
            history.connect_changed(move || {
                if let Some(page) = page.upgrade() {
                    page.report();
                }
            });
        }
    }

    /// Reports what the page shows and where its history can go, when that changed.
    fn report(&self) {
        let state = PageState {
            uri: self.web_view.uri(),
            title: self.web_view.title(),
            can_go_back: self.web_view.can_go_back(),
            can_go_forward: self.web_view.can_go_forward(),
        };
        if *self.reported.borrow() == state {
            return;
        }
        self.reported.replace(state.clone());
        self.send(Event::Page {
            view: self.id,
            page: state,
        });
    }

    fn connect_policy(self: &Rc<Self>) {
        let page = Rc::downgrade(self);
        self.web_view.connect_local("decide-policy", false, move |values| {
            let decision = object_arg::<PolicyDecision>(values, 1);
            let kind = enum_arg(values, 2);
            match (page.upgrade(), decision) {
                (Some(page), Some(decision)) => handled(page.decide(&decision, kind)),
                _ => handled(false),
            }
        });
    }

    /// Holds `decision` open and asks the consumer; false leaves it to `WebKit`'s default.
    fn decide(&self, decision: &PolicyDecision, kind: webkit::WebKitPolicyDecisionType) -> bool {
        let question = match kind {
            webkit::WEBKIT_POLICY_DECISION_TYPE_NAVIGATION_ACTION
            | webkit::WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION => {
                let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                    return false;
                };
                let Some(navigation) = navigation.navigation() else {
                    decision.ignore();
                    return true;
                };
                PolicyKind::Navigation {
                    uri: navigation.uri,
                    method: navigation.method,
                    navigation: navigation_type_of(navigation.kind),
                    new_window: kind == webkit::WEBKIT_POLICY_DECISION_TYPE_NEW_WINDOW_ACTION,
                }
            }
            webkit::WEBKIT_POLICY_DECISION_TYPE_RESPONSE => {
                let Some(response) = decision.downcast_ref::<ResponsePolicyDecision>() else {
                    return false;
                };
                PolicyKind::Response {
                    uri: response.uri(),
                    main_frame: response.is_main_frame_main_resource(),
                    mime_supported: response.is_mime_type_supported(),
                }
            }
            _ => return false,
        };
        let held = if self.closed.get() {
            None
        } else {
            super::hold_policy(self.id, decision)
        };
        let Some(token) = held else {
            decision.ignore();
            return true;
        };
        self.send(Event::Policy(crate::event::PolicyRequest {
            token,
            view: self.id,
            kind: question,
        }));
        true
    }

    fn connect_messages(self: &Rc<Self>) {
        for handler in &self.handlers {
            let page = Rc::downgrade(self);
            let name = handler.name.clone();
            let signal = format!("script-message-with-reply-received::{name}");
            self.content.connect_local(&signal, false, move |values| {
                let value = object_arg::<JscValue>(values, 1);
                // SAFETY: the signal's second argument is the reply, a live `WebKitScriptMessageReply`.
                let reply = unsafe { ScriptReply::retain(pointer_arg(values, 2).cast()) };
                if let Some(reply) = reply {
                    match page.upgrade() {
                        Some(page) => page.message(&name, value, reply),
                        None => reply.return_error_message("page closed"),
                    }
                }
                handled(true)
            });
        }
    }

    /// Holds a script message's reply open and reports the message.
    fn message(&self, handler: &str, value: Option<JscValue>, reply: ScriptReply) {
        if self.closed.get() {
            reply.return_error_message("page closed");
            return;
        }
        let Some(value) = value else {
            reply.return_error_message("invalid request");
            return;
        };
        let body = if value.is_object() { value.to_json() } else { None };
        let Some(context) = value.context() else {
            reply.return_error_message("invalid request");
            return;
        };
        let token = match super::hold_reply(self.id, reply, context) {
            Ok(token) => token,
            Err(reply) => {
                reply.return_error_message("page closed");
                return;
            }
        };
        self.send(Event::ScriptMessage(ScriptMessage {
            token,
            view: self.id,
            handler: handler.to_owned(),
            body,
        }));
    }

    fn connect_refusals(self: &Rc<Self>) {
        let page = Rc::downgrade(self);
        // the page's `window.close()`
        self.web_view.connect_local("close", false, move |_| {
            if let Some(page) = page.upgrade() {
                page.send(Event::CloseRequested { view: page.id });
            }
            None
        });
        // script dialogs are answered silently: dismissed, a page leaving still leaves
        self.web_view.connect_local("script-dialog", false, |values| {
            let dialog = pointer_arg(values, 1).cast::<webkit::WebKitScriptDialog>();
            if !dialog.is_null() {
                answer_dialog(dialog);
            }
            handled(true)
        });
        self.web_view.connect_local("run-file-chooser", false, |values| {
            let request = object_arg::<glib::Object>(values, 1);
            if let Some(request) = request {
                let raw: *mut glib::gobject_ffi::GObject = request.to_glib_none().0;
                // SAFETY: the signal's argument is a live `WebKitFileChooserRequest`.
                unsafe { webkit::webkit_file_chooser_request_cancel(raw.cast()) };
            }
            handled(true)
        });
        self.web_view.connect_local("permission-request", false, |values| {
            let request = object_arg::<glib::Object>(values, 1);
            if let Some(request) = request {
                let raw: *mut glib::gobject_ffi::GObject = request.to_glib_none().0;
                // SAFETY: the signal's argument implements `WebKitPermissionRequest`.
                unsafe { webkit::webkit_permission_request_deny(raw.cast()) };
            }
            handled(true)
        });
        for signal in ["enter-fullscreen", "show-notification"] {
            self.web_view.connect_local(signal, false, |_| handled(true));
        }
    }
}

/// The engine's view a web view renders through.
fn view_of(web_view: &WebView) -> Option<View> {
    let raw = web_view.wpe_view();
    if raw.is_null() {
        return None;
    }
    // SAFETY: the web view owns a live view; `from_glib_none` takes a reference.
    let object: glib::Object = unsafe { glib::translate::from_glib_none(raw.cast::<glib::gobject_ffi::GObject>()) };
    object.downcast::<View>().ok()
}

fn answer_dialog(dialog: *mut webkit::WebKitScriptDialog) {
    // SAFETY: the signal's argument is a live dialog for the call.
    let kind = unsafe { webkit::webkit_script_dialog_get_dialog_type(dialog) };
    let confirmed = match kind {
        webkit::WEBKIT_SCRIPT_DIALOG_CONFIRM => Some(false),
        webkit::WEBKIT_SCRIPT_DIALOG_BEFORE_UNLOAD_CONFIRM => Some(true),
        _ => None,
    };
    if let Some(confirmed) = confirmed {
        // SAFETY: a live confirm dialog.
        unsafe { webkit::webkit_script_dialog_confirm_set_confirmed(dialog, glib::ffi::gboolean::from(confirmed)) };
    }
}

/// A failed load as the overlay reads it (upstream's `reportFailure`): a cancel superseded by the next load, a policy
/// interruption, or a failure.
fn load_error(error: &glib::Error) -> LoadError {
    // SAFETY: a plain quark lookup.
    let network = unsafe { webkit::webkit_network_error_quark() };
    // SAFETY: a plain quark lookup.
    let policy = unsafe { webkit::webkit_policy_error_quark() };
    let kind = if error_is(error, network, webkit::WEBKIT_NETWORK_ERROR_CANCELLED) {
        LoadErrorKind::Cancelled
    } else if error_is(
        error,
        policy,
        webkit::WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE,
    ) || error_is(error, policy, webkit::WEBKIT_POLICY_ERROR_CANNOT_SHOW_MIME_TYPE)
    {
        LoadErrorKind::Interrupted
    } else {
        LoadErrorKind::Failed
    };
    LoadError {
        kind,
        message: error.message().to_owned(),
    }
}

const fn navigation_type_of(kind: webkit::WebKitNavigationType) -> NavigationType {
    match kind {
        webkit::WEBKIT_NAVIGATION_TYPE_LINK_CLICKED => NavigationType::LinkClicked,
        webkit::WEBKIT_NAVIGATION_TYPE_FORM_SUBMITTED => NavigationType::FormSubmitted,
        webkit::WEBKIT_NAVIGATION_TYPE_BACK_FORWARD => NavigationType::BackForward,
        webkit::WEBKIT_NAVIGATION_TYPE_RELOAD => NavigationType::Reload,
        webkit::WEBKIT_NAVIGATION_TYPE_FORM_RESUBMITTED => NavigationType::FormResubmitted,
        _ => NavigationType::Other,
    }
}

/// Milliseconds on the monotonic clock, wrapping, as `WPEPlatform` stamps its events.
fn event_time() -> u32 {
    let micros = glib::monotonic_time();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "event times wrap, as WPE's own do"
    )]
    let millis = (micros / 1000) as u32;
    millis
}

fn pointer_event(view: *mut platform::WPEView, time: u32, pointer: Pointer) -> *mut platform::WPEEvent {
    let modifiers = pointer.modifiers.bits();
    let source = platform::WPE_INPUT_SOURCE_MOUSE;
    match pointer.kind {
        PointerKind::Enter | PointerKind::Leave | PointerKind::Move => {
            // SAFETY: a live view; a move-type event carries no button.
            unsafe {
                platform::wpe_event_pointer_move_new(
                    pointer.kind.event_type(),
                    view,
                    source,
                    time,
                    modifiers,
                    pointer.x,
                    pointer.y,
                    0.0,
                    0.0,
                )
            }
        }
        PointerKind::Down(button) => {
            // SAFETY: a live view; WPE counts the press against its previous ones.
            let presses =
                unsafe { platform::wpe_view_compute_press_count(view, pointer.x, pointer.y, button.number(), time) };
            // SAFETY: a live view.
            unsafe {
                platform::wpe_event_pointer_button_new(
                    pointer.kind.event_type(),
                    view,
                    source,
                    time,
                    modifiers,
                    button.number(),
                    pointer.x,
                    pointer.y,
                    presses,
                )
            }
        }
        // a release carries no press count (WPE asserts it is zero)
        // SAFETY: a live view.
        PointerKind::Up(button) => unsafe {
            platform::wpe_event_pointer_button_new(
                pointer.kind.event_type(),
                view,
                source,
                time,
                modifiers,
                button.number(),
                pointer.x,
                pointer.y,
                0,
            )
        },
    }
}

fn scroll_event(view: *mut platform::WPEView, time: u32, scroll: Scroll) -> *mut platform::WPEEvent {
    // SAFETY: a live view.
    unsafe {
        platform::wpe_event_scroll_new(
            view,
            platform::WPE_INPUT_SOURCE_MOUSE,
            time,
            scroll.modifiers.bits(),
            scroll.delta_x,
            scroll.delta_y,
            glib::ffi::gboolean::from(scroll.precise),
            glib::ffi::gboolean::from(scroll.stop),
            scroll.x,
            scroll.y,
        )
    }
}

fn key_event(view: *mut platform::WPEView, time: u32, key: Key) -> *mut platform::WPEEvent {
    let kind = if key.pressed {
        platform::WPE_EVENT_KEYBOARD_KEY_DOWN
    } else {
        platform::WPE_EVENT_KEYBOARD_KEY_UP
    };
    let keycode = key.keycode.unwrap_or_else(|| keycode_for(view, key.keyval));
    // SAFETY: a live view.
    unsafe {
        platform::wpe_event_keyboard_new(
            kind,
            view,
            platform::WPE_INPUT_SOURCE_KEYBOARD,
            time,
            key.modifiers.bits(),
            keycode,
            key.keyval,
        )
    }
}

/// A hardware keycode that produces `keyval` in the display's keymap, 0 when none does.
fn keycode_for(view: *mut platform::WPEView, keyval: u32) -> u32 {
    // SAFETY: a live view; its display is owned by it.
    let display = unsafe { platform::wpe_view_get_display(view) };
    // SAFETY: a live display; the keymap is owned by it.
    let keymap = unsafe { platform::wpe_display_get_keymap(display) };
    if keymap.is_null() {
        return 0;
    }
    let mut entries: *mut platform::WPEKeymapEntry = std::ptr::null_mut();
    let mut count = 0;
    // SAFETY: a live keymap and valid out-pointers; the entries are ours to free.
    let found =
        unsafe { platform::wpe_keymap_get_entries_for_keyval(keymap, keyval, &raw mut entries, &raw mut count) };
    let keycode = if found != glib::ffi::GFALSE && count > 0 && !entries.is_null() {
        // SAFETY: the call filled `count` entries at `entries`.
        unsafe { (*entries).keycode }
    } else {
        0
    };
    // SAFETY: frees the array the call allocated (null is fine).
    unsafe { glib::ffi::g_free(entries.cast()) };
    keycode
}

#[cfg(test)]
#[path = "../../tests/support/page.rs"]
mod tests;
