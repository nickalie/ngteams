//! The sign-in window's WPE `WebKit` web process extension.
//!
//! `WebKit` loads this module into every web process of the sign-in engine (`teams-signin` over `teams-wpe`, its web
//! context's extensions directory) and calls [`webkit_web_process_extension_initialize_with_user_data`] with the path
//! of the guard's socket. The extension connects there ([`client::GuardClient`]), connects each new page's
//! `send-request` signal, tells the guard the page is armed, and from then on asks the guard about every request the
//! page sends, redirects that come back to the page included; it drops each one the guard doesn't allow (the handler
//! returns `TRUE`), and every one when it has no guard. The guard classifies and logs; nothing is decided here.
//!
//! What this can't see: a synchronous load's redirects and a kept-alive load's redirects after its page is gone are
//! followed by the network process without a second `send-request`, and `WebSockets` never pass it
//! (`docs/backlog/webkit-sign-in-guard-redirects.md`). The window therefore disables unsupported features and
//! routes its network session through the sign-in host gate (`teams_signin::egress`), which also handles redirects.
//!
//! The FFI is by hand: no Rust binding has `WebKit`'s web process extension API. Only the three `WebKit` functions
//! below and two signals are used; they live in `libWPEWebKit-2.0`, which `build.rs` links.
#![cfg(target_os = "linux")]

pub mod client;
pub use teams_signin_protocol::{ENTRY_POINT, protocol};

#[cfg(test)]
#[path = "../tests/support/ffi.rs"]
mod ffi_tests;

use std::ffi::{CStr, c_char};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use glib_sys::{GVariant, gboolean, gpointer};
use gobject_sys::{GCallback, GObject};

use crate::client::GuardClient;

/// `WebKitWebProcessExtension`, `WebKitWebPage`, `WebKitURIRequest`, `WebKitURIResponse`: opaque here.
#[repr(C)]
pub struct Opaque {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn webkit_web_page_get_id(page: *mut Opaque) -> u64;
    fn webkit_uri_request_get_uri(request: *mut Opaque) -> *const c_char;
    fn webkit_uri_request_get_http_method(request: *mut Opaque) -> *const c_char;
}

/// The process's one connection to the guard; `None` until initialized (which denies, as a closed client does).
static GUARD: Mutex<Option<GuardClient>> = Mutex::new(None);

fn with_guard<T>(decide: impl FnOnce(&mut GuardClient) -> T) -> T {
    let mut guard = GUARD.lock().unwrap_or_else(PoisonError::into_inner);
    decide(guard.get_or_insert_with(GuardClient::closed))
}

/// `WebKit`'s entry point. `user_data` is the guard socket's path as a `GVariant` string; anything else leaves the
/// process without a guard, so every request of its pages is dropped.
///
/// # Safety
/// Called by `WebKit` only, with its extension object and the initialization data it was given.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn webkit_web_process_extension_initialize_with_user_data(
    extension: *mut Opaque,
    user_data: *const GVariant,
) {
    // SAFETY: WebKit hands the initialization data it was given, a valid GVariant or null.
    let path = unsafe { socket_path(user_data.cast_mut()) };
    let client = path.map_or_else(GuardClient::closed, |path| GuardClient::connect(Path::new(&path)));
    *GUARD.lock().unwrap_or_else(PoisonError::into_inner) = Some(client);
    // SAFETY: `extension` is WebKit's live extension object (a GObject); the handler matches `page-created`'s
    // signature, (WebKitWebProcessExtension*, WebKitWebPage*, gpointer) → void.
    unsafe { connect(extension.cast(), c"page-created", Some(page_created_handler())) };
}

/// The string in `user_data`, if it is a `GVariant` of type `s`.
///
/// # Safety
/// `user_data` is null or a valid `GVariant`.
unsafe fn socket_path(user_data: *mut GVariant) -> Option<String> {
    if user_data.is_null() {
        return None;
    }
    // SAFETY: a valid GVariant; the type string it returns lives as long as the variant.
    let kind = unsafe { glib_sys::g_variant_get_type_string(user_data) };
    // SAFETY: `kind` is a NUL-terminated string owned by the variant (or null, checked in `text`).
    if unsafe { text(kind) }.as_deref() != Some("s") {
        return None;
    }
    // SAFETY: a valid GVariant of type `s`; the string it returns lives as long as the variant.
    let value = unsafe { glib_sys::g_variant_get_string(user_data, std::ptr::null_mut()) };
    // SAFETY: as above.
    unsafe { text(value) }
}

/// The UTF-8 text at `pointer`, or `None` for null or invalid UTF-8.
///
/// # Safety
/// `pointer` is null or a NUL-terminated string valid for the call.
unsafe fn text(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: non-null and NUL-terminated by the caller's contract.
    let text = unsafe { CStr::from_ptr(pointer) };
    text.to_str().ok().map(str::to_owned)
}

/// Connects `handler` to `signal` on `instance`.
///
/// # Safety
/// `instance` is a live `GObject` with that signal, and `handler`'s real signature is the signal's.
unsafe fn connect(instance: *mut GObject, signal: &CStr, handler: GCallback) {
    // SAFETY: by the caller's contract; no data and no destroy notify.
    unsafe {
        gobject_sys::g_signal_connect_data(instance, signal.as_ptr(), handler, std::ptr::null_mut(), None, 0);
    }
}

type PageCreated = unsafe extern "C" fn(*mut Opaque, *mut Opaque, gpointer);
type SendRequest = unsafe extern "C" fn(*mut Opaque, *mut Opaque, *mut Opaque, gpointer) -> gboolean;

fn page_created_handler() -> unsafe extern "C" fn() {
    let handler: PageCreated = page_created;
    // SAFETY: GLib calls the handler back with the signal's real signature, which `PageCreated` is; `GCallback` is
    // only the type-erased form it is stored as.
    unsafe { std::mem::transmute::<PageCreated, unsafe extern "C" fn()>(handler) }
}

fn send_request_handler() -> unsafe extern "C" fn() {
    let handler: SendRequest = send_request;
    // SAFETY: as in `page_created_handler`, for `send-request`'s signature.
    unsafe { std::mem::transmute::<SendRequest, unsafe extern "C" fn()>(handler) }
}

/// `page-created`: connects the page's `send-request`, then tells the guard the page is armed.
unsafe extern "C" fn page_created(_extension: *mut Opaque, page: *mut Opaque, _data: gpointer) {
    // SAFETY: `page` is the WebKitWebPage WebKit just created (a GObject); the handler matches `send-request`'s
    // signature, (WebKitWebPage*, WebKitURIRequest*, WebKitURIResponse*, gpointer) → gboolean.
    unsafe { connect(page.cast(), c"send-request", Some(send_request_handler())) };
    // SAFETY: `page` is a live WebKitWebPage.
    let id = unsafe { webkit_web_page_get_id(page) };
    with_guard(|guard| guard.armed(id, std::process::id()));
}

/// `send-request`: `TRUE` (drop the request) unless the guard allows it.
unsafe extern "C" fn send_request(
    page: *mut Opaque,
    request: *mut Opaque,
    _redirected_response: *mut Opaque,
    _data: gpointer,
) -> gboolean {
    // SAFETY: `page` is the live WebKitWebPage the signal was emitted on.
    let id = unsafe { webkit_web_page_get_id(page) };
    // SAFETY: `request` is the live WebKitURIRequest of the signal; its URI is a NUL-terminated string it owns.
    let uri = unsafe { webkit_uri_request_get_uri(request) };
    // SAFETY: as above, for the method (null when WebKit left it out).
    let method = unsafe { webkit_uri_request_get_http_method(request) };
    // SAFETY: points into the request, alive for this call, or is null.
    let uri = unsafe { text(uri) };
    // SAFETY: as above.
    let method = unsafe { text(method) };
    let allowed = uri.is_some_and(|uri| with_guard(|guard| guard.allows(id, method.as_deref(), &uri)));
    gboolean::from(!allowed)
}
