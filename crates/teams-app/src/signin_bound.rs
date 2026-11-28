//! What bounds a Microsoft sign-in page, on every platform: the hosts it may reach ([`hosts`]), the rules that sort
//! its requests ([`classify`]) and the host gate that is its network's only way out ([`egress`]). The desktop's
//! window (`teams-signin`) and the phone app's `WebView` use the same bound.

pub mod classify;
pub mod egress;
pub mod hosts;

/// Whether the offline test bounds were compiled in (`SignInHosts::for_tests`). Production ngteams refuses such a
/// build.
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}
