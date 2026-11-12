//! The worker's window: an `NSWindow` holding a `WKWebView` on a non-persistent website data store whose only way
//! out is the host gate (`proxyConfigurations`, an HTTP CONNECT proxy, macOS 14 or later). One delegate object takes
//! the window's close, the view's navigation and UI callbacks and the page's guard questions, all on the main thread,
//! and asks the app over the private pipe before anything starts.
//!
//! The document-start script, in every frame: the Windows script's removal of the unguarded network constructors
//! (`WebRTC`, `WebTransport`, `WebSocket`, workers, `sendBeacon`), and `fetch` and `XMLHttpRequest` wrapped to ask the
//! guard (method and absolute URL) through a script message with a reply before the request is made; a refused one
//! fails as a network error would.
use super::wire::{self, Event, Start};
use crate::webview_policy::INIT;
use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSMenu, NSMenuItem, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSError, NSNotification, NSNumber, NSObject, NSObjectNSKeyValueCoding as _, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, NSURL, NSURLAuthenticationChallenge, NSURLAuthenticationMethodServerTrust,
    NSURLCredential, NSURLRequest, NSURLSessionAuthChallengeDisposition, ns_string,
};
use objc2_web_kit::{
    WKContentWorld, WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationResponse, WKNavigationResponsePolicy, WKScriptMessage, WKScriptMessageHandlerWithReply, WKUIDelegate,
    WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore, WKWindowFeatures,
};
use std::ffi::{CString, c_char};

/// The script message handler's name: `window.webkit.messageHandlers.ngteamsGuard`.
const HANDLER: &str = "ngteamsGuard";

/// `fetch` and `XMLHttpRequest` ask the guard first. Kept in a closure: page script can't reach the originals.
const ASK: &str = r"(() => {
    const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ngteamsGuard;
    if (!handler) { return; }
    const ask = (method, url) => {
        let absolute;
        try { absolute = new URL(String(url), document.baseURI).href; } catch (_) { return Promise.resolve(false); }
        return handler.postMessage(JSON.stringify({ method: String(method || 'GET').toUpperCase(), url: absolute }))
            .then((answer) => answer === true, () => false);
    };
    const originalFetch = window.fetch;
    const guardedFetch = function (input, init) {
        const request = input instanceof Request ? input : null;
        const method = (init && init.method) || (request ? request.method : 'GET');
        const url = request ? request.url : input;
        return ask(method, url).then((allowed) => allowed
            ? originalFetch.call(this, input, init)
            : Promise.reject(new TypeError('Refused by the sign-in guard')));
    };
    Object.defineProperty(window, 'fetch', { value: guardedFetch, writable: false, configurable: false });
    const open = XMLHttpRequest.prototype.open;
    const send = XMLHttpRequest.prototype.send;
    const asked = new WeakMap();
    Object.defineProperty(XMLHttpRequest.prototype, 'open', { writable: false, configurable: false,
        value: function (method, url, ...rest) { asked.set(this, [method, url]); return open.call(this, method, url, ...rest); } });
    Object.defineProperty(XMLHttpRequest.prototype, 'send', { writable: false, configurable: false,
        value: function (body) {
            const [method, url] = asked.get(this) || ['GET', ''];
            ask(method, url).then((allowed) => {
                if (allowed) { send.call(this, body); } else { this.abort(); }
            });
        } });
})()";

#[repr(C)]
struct NwObject {
    _private: [u8; 0],
}

#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_endpoint_create_host(host: *const c_char, port: *const c_char) -> *mut NwObject;
    fn nw_proxy_config_create_http_connect(endpoint: *mut NwObject, tls: *mut NwObject) -> *mut NwObject;
}

/// Write `event` to the app and read its answer; a broken pipe answers no.
fn query(event: &Event) -> bool {
    let result = wire::send(&mut std::io::stdout().lock(), event)
        .and_then(|()| wire::read::<bool>(&mut std::io::stdin().lock()));
    result.unwrap_or(false)
}

/// Tell the app without waiting for an answer.
fn tell(event: &Event) {
    let _ = wire::send(&mut std::io::stdout().lock(), event);
}

/// The request's method and URL as the guard reads them.
fn described(request: &NSURLRequest) -> (String, String) {
    let url = request
        .URL()
        .and_then(|url| url.absoluteString())
        .map(|url| url.to_string())
        .unwrap_or_default();
    let method = request
        .HTTPMethod()
        .map_or_else(|| "GET".to_owned(), |method| method.to_string());
    (method, url)
}

/// A navigation starts only when the app's navigation rules and its guard both let it.
fn navigation_allowed(method: &str, url: &str) -> bool {
    // a frame's own empty document: no request is made
    if url == "about:blank" || url == "about:srcdoc" {
        return true;
    }
    query(&Event::Navigation(url.to_owned()))
        && query(&Event::Request {
            method: method.to_owned(),
            url: url.to_owned(),
        })
}

#[derive(serde::Deserialize)]
struct Asked {
    method: String,
    url: String,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the delegate holds no state.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "NgteamsSignInDelegate"]
    struct Delegate;

    // SAFETY: NSObjectProtocol has no extra requirements.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the method signatures match NSWindowDelegate's.
    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            tell(&Event::Closed);
            std::process::exit(0);
        }
    }

    // SAFETY: the method signatures match WKNavigationDelegate's.
    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_navigation(
            &self,
            _view: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: the action's request is read on the main thread, during the callback.
            let request = unsafe { action.request() };
            let (method, url) = described(&request);
            let policy = if navigation_allowed(&method, &url) {
                WKNavigationActionPolicy::Allow
            } else {
                WKNavigationActionPolicy::Cancel
            };
            handler.call((policy,));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _view: &WKWebView,
            response: &WKNavigationResponse,
            handler: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            // a response the view can't show would be a download: refused
            // SAFETY: read on the main thread, during the callback.
            let policy = if unsafe { response.canShowMIMEType() } {
                WKNavigationResponsePolicy::Allow
            } else {
                WKNavigationResponsePolicy::Cancel
            };
            handler.call((policy,));
        }

        #[unsafe(method(webView:didReceiveAuthenticationChallenge:completionHandler:))]
        fn challenged(
            &self,
            _view: &WKWebView,
            challenge: &NSURLAuthenticationChallenge,
            handler: &DynBlock<dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential)>,
        ) {
            // the system checks a server's certificate, with no click-through; ambient HTTP authentication and client
            // certificates are refused
            let method = challenge.protectionSpace().authenticationMethod();
            // SAFETY: an immutable framework constant.
            let trust = method.isEqualToString(unsafe { NSURLAuthenticationMethodServerTrust });
            let disposition = if trust {
                NSURLSessionAuthChallengeDisposition::PerformDefaultHandling
            } else {
                NSURLSessionAuthChallengeDisposition::CancelAuthenticationChallenge
            };
            handler.call((disposition, std::ptr::null_mut()));
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_provisional(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            // a navigation refused above, or replaced by another, is not a network failure
            const CANCELLED: isize = -999;
            const INTERRUPTED_BY_POLICY: isize = 102;
            let code = error.code();
            if code != CANCELLED && code != INTERRUPTED_BY_POLICY {
                tell(&Event::NetworkFailure(format!("{} {code}", error.domain())));
            }
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn content_ended(&self, _view: &WKWebView) {
            tell(&Event::Failed(
                "The sign-in page stopped unexpectedly. Close it and try again.".into(),
            ));
            std::process::exit(0);
        }
    }

    // SAFETY: the method signature matches WKUIDelegate's.
    unsafe impl WKUIDelegate for Delegate {
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn new_window(
            &self,
            _view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            _action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> *mut WKWebView {
            // no new windows: a popup's navigation is refused
            std::ptr::null_mut()
        }
    }

    // SAFETY: the method signature matches WKScriptMessageHandlerWithReply's.
    unsafe impl WKScriptMessageHandlerWithReply for Delegate {
        #[unsafe(method(userContentController:didReceiveScriptMessage:replyHandler:))]
        fn asked(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
            reply: &DynBlock<dyn Fn(*mut AnyObject, *mut NSString)>,
        ) {
            // SAFETY: the message's body is read on the main thread, during the callback.
            let body = unsafe { message.body() };
            let allowed = body
                .downcast::<NSString>()
                .ok()
                .and_then(|text| serde_json::from_str::<Asked>(&text.to_string()).ok())
                .is_some_and(|asked| {
                    query(&Event::Request {
                        method: asked.method,
                        url: asked.url,
                    })
                });
            let answer = NSNumber::new_bool(allowed);
            reply.call((Retained::as_ptr(&answer).cast_mut().cast(), std::ptr::null_mut()));
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// The gate's address from its `http://127.0.0.1:<port>` URI.
fn proxy_parts(uri: &str) -> Option<(CString, CString)> {
    let (host, port) = uri.strip_prefix("http://")?.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    Some((CString::new(host).ok()?, CString::new(port).ok()?))
}

/// The non-persistent store whose connections all go through the gate.
fn gated_store(mtm: MainThreadMarker, proxy: &str) -> Result<Retained<WKWebsiteDataStore>, &'static str> {
    const OLD: &str = "Signing in needs macOS 14 or later";
    // SAFETY: a new non-persistent store, on the main thread.
    let store = unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) };
    if !store.respondsToSelector(sel!(setProxyConfigurations:)) {
        return Err(OLD);
    }
    let (host, port) = proxy_parts(proxy).ok_or("the sign-in proxy's address is invalid")?;
    // SAFETY: both strings are NUL-terminated and live through the call; the endpoint is returned retained (+1).
    let endpoint = unsafe { nw_endpoint_create_host(host.as_ptr(), port.as_ptr()) };
    if endpoint.is_null() {
        return Err("the sign-in proxy's address is invalid");
    }
    // SAFETY: a valid endpoint; no TLS to the loopback gate. The configuration is returned retained (+1).
    let config = unsafe { nw_proxy_config_create_http_connect(endpoint, std::ptr::null_mut()) };
    // SAFETY: the endpoint is a retained OS object the configuration now retains itself.
    unsafe { objc2::ffi::objc_release(endpoint.cast()) };
    // SAFETY: Network.framework's objects are Objective-C objects; this takes over the +1 reference.
    let config = unsafe { Retained::from_raw(config.cast::<NSObject>()) }.ok_or("the sign-in proxy failed")?;
    let proxies = NSArray::from_retained_slice(&[config]);
    // SAFETY: `proxyConfigurations` takes an array of `nw_proxy_config_t` (checked present above).
    unsafe { store.setValue_forKey(Some(&proxies), ns_string!("proxyConfigurations")) };
    Ok(store)
}

/// The view's configuration: the gated store, the document-start scripts and the guard's message handler.
fn configuration(
    mtm: MainThreadMarker,
    start: &Start,
    delegate: &Delegate,
) -> Result<Retained<WKWebViewConfiguration>, &'static str> {
    // SAFETY: a new configuration, on the main thread.
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    let store = gated_store(mtm, &start.proxy)?;
    // SAFETY: as above.
    unsafe { config.setWebsiteDataStore(&store) };
    // SAFETY: as above.
    let preferences = unsafe { config.preferences() };
    // SAFETY: as above.
    unsafe { preferences.setJavaScriptCanOpenWindowsAutomatically(false) };
    // SAFETY: Safe Browsing's lookups would go to hosts outside the gate.
    unsafe { preferences.setFraudulentWebsiteWarningEnabled(false) };
    // SAFETY: as above.
    let content = unsafe { config.userContentController() };
    for source in [INIT, ASK] {
        // SAFETY: a new script, in every frame, before the page's own.
        let script = unsafe {
            WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(mtm),
                &NSString::from_str(source),
                WKUserScriptInjectionTime::AtDocumentStart,
                false,
            )
        };
        // SAFETY: on the main thread.
        unsafe { content.addUserScript(&script) };
    }
    // SAFETY: the page's world, where the script runs.
    let world = unsafe { WKContentWorld::pageWorld(mtm) };
    // SAFETY: the delegate outlives the view (both live until the process exits).
    unsafe {
        content.addScriptMessageHandlerWithReply_contentWorld_name(
            ProtocolObject::from_ref(delegate),
            &world,
            &NSString::from_str(HANDLER),
        );
    }
    Ok(config)
}

/// Cut, copy, paste, select all and undo reach the page through the main menu's key equivalents, and Cmd-W closes.
fn edit_menu(mtm: MainThreadMarker, app: &NSApplication) {
    let item = |title: &str, action, key: &str| {
        // SAFETY: a new menu item with a standard responder action.
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            )
        }
    };
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Edit"));
    for (title, action, key) in [
        ("Undo", sel!(undo:), "z"),
        ("Cut", sel!(cut:), "x"),
        ("Copy", sel!(copy:), "c"),
        ("Paste", sel!(paste:), "v"),
        ("Select All", sel!(selectAll:), "a"),
        ("Close", sel!(performClose:), "w"),
    ] {
        edit.addItem(&item(title, action, key));
    }
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&edit));
    let bar = NSMenu::new(mtm);
    bar.addItem(&holder);
    app.setMainMenu(Some(&bar));
}

/// Opens the window on the authorize page and runs until it closes (the process exits there).
pub(super) fn run(start: &Start) -> Result<(), &'static str> {
    let mtm = MainThreadMarker::new().ok_or("the sign-in worker must run on its main thread")?;
    watch_parent();
    let app = NSApplication::sharedApplication(mtm);
    // no Dock icon of its own: the window belongs to ngteams
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    edit_menu(mtm, &app);
    let delegate = Delegate::new(mtm);
    let config = configuration(mtm, start, &delegate)?;
    let size = NSSize::new(f64::from(super::SIZE.0), f64::from(super::SIZE.1));
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
    // SAFETY: a new view with a valid configuration, on the main thread.
    let view = unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config) };
    // SAFETY: the delegate outlives the view.
    unsafe { view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    // SAFETY: as above.
    unsafe { view.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    // SAFETY: a new window, on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is held by this function until the process exits.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(super::TITLE));
    window.setContentView(Some(&view));
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.center();
    let url =
        NSURL::URLWithString(&NSString::from_str(&start.authorize)).ok_or("the sign-in page's address is invalid")?;
    // SAFETY: on the main thread; the request is copied by the view.
    unsafe { view.loadRequest(&NSURLRequest::requestWithURL(&url)) };
    window.makeKeyAndOrderFront(None);
    #[expect(
        deprecated,
        reason = "`activate` alone doesn't bring an accessory process forward over the app"
    )]
    app.activateIgnoringOtherApps(true);
    app.run();
    Ok(())
}

/// The worker ends with its app: started by `ngteams`, it exits once it is reparented (the app died without
/// killing it).
fn watch_parent() {
    let parent = std::os::unix::process::parent_id();
    let _ = std::thread::Builder::new().name("signin-parent".into()).spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if std::os::unix::process::parent_id() != parent {
                std::process::exit(0);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::proxy_parts;

    #[test]
    fn the_gate_uri_gives_a_numeric_host_and_port() {
        let (host, port) = proxy_parts("http://127.0.0.1:43123").unwrap();
        assert_eq!((host.to_str().unwrap(), port.to_str().unwrap()), ("127.0.0.1", "43123"));
        for bad in [
            "https://127.0.0.1:1",
            "http://127.0.0.1",
            "http://127.0.0.1:port",
            "127.0.0.1:1",
        ] {
            assert!(proxy_parts(bad).is_none(), "{bad}");
        }
    }
}
