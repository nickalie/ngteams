//! The sign-in window's network session goes only through its host gate (`teams_app::signin_bound::egress`), which
//! logs through the window's [`Guard`]. Offline tests can instead use the refusing proxy
//! (`BlackHole`) with numeric loopback bypass; that mode is absent from production builds.

#[cfg(feature = "test-support")]
pub use teams_app::signin_bound::egress::BlackHole;
pub use teams_app::signin_bound::egress::{GateLog, HostGate};

use crate::guard::Guard;

/// The hosts the network session reaches directly: numeric loopback only. A name (even `localhost`) goes to the
/// black hole: GIO's ignore-hosts entries for a name match its subdomains too, and a name is resolved by DNS.
#[cfg(feature = "test-support")]
pub const BYPASS: [&str; 2] = ["127.0.0.0/8", "::1"];

impl GateLog for Guard {
    fn refuse(&self, method: &str, target: &str) {
        Self::refuse(self, method, target);
    }

    fn tunnel(&self, host: &str, port: u16) {
        Self::tunnel(self, host, port);
    }
}
