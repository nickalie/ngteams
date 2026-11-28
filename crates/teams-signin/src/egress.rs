//! The sign-in window's network session goes only through its host gate (`teams_app::signin_bound::egress`), which
//! logs through the window's [`Guard`]. Offline tests can instead use a refusing proxy with numeric loopback bypass;
//! that mode is absent from production builds.

pub use teams_app::signin_bound::egress::{GateLog, HostGate, READ_TIMEOUT};

#[cfg(feature = "test-support")]
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
#[cfg(feature = "test-support")]
use std::net::{Ipv4Addr, TcpListener, TcpStream};
#[cfg(feature = "test-support")]
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::thread;

use crate::guard::Guard;
#[cfg(feature = "test-support")]
use teams_app::signin_bound::egress::{LINE_LIMIT, REFUSAL, request_line};

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

/// The refusing proxy, on `127.0.0.1`.
#[cfg(feature = "test-support")]
#[derive(Debug)]
pub struct BlackHole {
    port: u16,
}

#[cfg(feature = "test-support")]
impl BlackHole {
    /// Binds a port on `127.0.0.1` and refuses every connection to it, each logged through `guard`, until the
    /// process exits.
    ///
    /// # Errors
    /// The port can't be bound.
    pub fn start(guard: Arc<Guard>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        thread::Builder::new().name("signin-egress".into()).spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                let guard = Arc::clone(&guard);
                let _ = thread::Builder::new()
                    .name("signin-egress-conn".into())
                    .spawn(move || refuse(&guard, &stream));
            }
        })?;
        Ok(Self { port })
    }

    /// The proxy's URI, for the network session's proxy settings.
    #[must_use]
    pub fn uri(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Logs what the connection asked for and refuses it; a line that can't be read is logged as `?`.
#[cfg(feature = "test-support")]
fn refuse(guard: &Guard, stream: &TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut line = Vec::new();
    let _ = BufReader::new(stream).take(LINE_LIMIT).read_until(b'\n', &mut line);
    let line = String::from_utf8_lossy(&line);
    let (method, target) = request_line(&line);
    guard.refuse(method, target);
    let mut stream = stream;
    let _ = stream.write_all(REFUSAL);
    let _ = stream.flush();
}
