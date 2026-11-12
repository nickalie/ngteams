//! The app's guarded sign-in window: WPE `WebKit` drawn in GPUI on Linux, native `WebView2` on Windows, a native
//! `WKWebView` on macOS.
//! [`hosts::SignInHosts`] binds every production window to Entra and the account's identity provider hosts.
//! [`egress::HostGate`] is its network session's only way out; [`guard`] decides and logs page requests without
//! bodies or credentials. The window catches the redirect and hands it directly to `teams_auth::signin` to verify
//! the request's state and redeem its code. Linux has no standalone sign-in binary. Windows and macOS use the disposable
//! `ngteams-webview` worker and the private stdin/stdout protocol in `native/wire.rs` (`macos/mod.rs` says what macOS
//! guards and how).
//!
//! `WebKit` follows some redirects without asking the page guard; the network host gate still applies to them.
//! Windows uses all-source request filters and response restrictions in a disposable native worker.
//! Offline loopback windows and synthetic TLS rules exist only with `test-support`.

#[cfg(any(windows, target_os = "macos", test))]
mod webview_policy;

pub mod classify;
mod diagnostics;
pub mod egress;
#[cfg(target_os = "linux")]
pub mod engine;
pub mod guard;
pub mod hosts;
#[cfg(target_os = "linux")]
pub mod input;
#[cfg(target_os = "linux")]
mod view;
#[cfg(target_os = "linux")]
pub mod window;
#[cfg(windows)]
#[path = "native/mod.rs"]
pub mod window;
#[cfg(target_os = "macos")]
#[path = "macos/mod.rs"]
pub mod window;

/// Whether offline test guards or window modes were compiled in. Production ngteams refuses such a build.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
