//! The sign-in window's network session goes only through its host gate. The gate opens HTTPS tunnels to the
//! configured sign-in hosts and logs each connection without headers or bodies, through the window's [`GateLog`] (the
//! desktop's guard). The desktop's offline tests use a refusing proxy of `teams-signin` instead.

use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::hosts::SignInHosts;

/// How long the gate (and a refusing proxy) waits for a connection's request line.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest request line it reads.
pub const LINE_LIMIT: u64 = 8 * 1024;
/// The answer to a connection the gate (or a refusing proxy) refuses.
pub const REFUSAL: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// The method and target of a proxy request line, `?` for what isn't there.
#[must_use]
pub fn request_line(line: &str) -> (&str, &str) {
    let mut fields = line.split_whitespace();
    (fields.next().unwrap_or("?"), fields.next().unwrap_or("?"))
}

/// Where the gate's decisions go: the sign-in window's diagnostic log, without headers or bodies.
pub trait GateLog {
    /// Logs a connection the gate refused: what it asked for, `?` for what it did not say.
    fn refuse(&self, method: &str, target: &str);
    /// Logs a tunnel the gate opens, before it opens.
    fn tunnel(&self, host: &str, port: u16);
}

/// The personal sign-in window's network bound (`docs/personal-build.md` Choice C): a proxy on loopback
/// that opens a `CONNECT` tunnel only to a host on the window's bound ([`SignInHosts`]) on 443, logged
/// before it opens ([`GateLog::tunnel`]), and refuses and logs every other connection, a plain `http` request included.
/// It is the network session's only way out (no direct hosts), so the requests the guard can't see (the redirects
/// `WebKit` follows on its own, a web process that never armed) reach no other host either: a *host* bound, not a
/// method or path bound. DNS lookups happen here, for the hosts on the bound only.
#[derive(Debug)]
pub struct HostGate {
    port: u16,
}

/// How many header lines the gate reads after a `CONNECT` line before it gives up.
const HEADER_LIMIT: usize = 64;

const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";

const UNREACHABLE: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

impl HostGate {
    /// Binds a port on `127.0.0.1` and serves the gate over `hosts`, each decision logged through `guard`, until the
    /// process exits.
    ///
    /// # Errors
    /// The port can't be bound.
    pub fn start<L: GateLog + Send + Sync + 'static>(guard: Arc<L>, hosts: Arc<SignInHosts>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        thread::Builder::new().name("signin-gate".into()).spawn(move || {
            for stream in listener.incoming().map_while(Result::ok) {
                let (guard, hosts) = (Arc::clone(&guard), Arc::clone(&hosts));
                let _ = thread::Builder::new()
                    .name("signin-gate-conn".into())
                    .spawn(move || gate(guard.as_ref(), &hosts, &stream));
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

/// One connection to the gate: a `CONNECT` to a host on the bound gets its tunnel, anything else is refused.
fn gate(guard: &impl GateLog, hosts: &SignInHosts, stream: &TcpStream) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let Ok(reader_stream) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader_stream);
    let mut line = Vec::new();
    let _ = reader.by_ref().take(LINE_LIMIT).read_until(b'\n', &mut line);
    let text = String::from_utf8_lossy(&line).into_owned();
    let (method, target) = request_line(&text);
    let wanted = (method == "CONNECT").then(|| authority(target)).flatten();
    let Some((host, port)) = wanted.filter(|(host, port)| hosts.admits(host, *port)) else {
        guard.refuse(method, target);
        let mut stream = stream;
        let _ = stream.write_all(REFUSAL);
        return;
    };
    // the rest of the CONNECT request: its headers, up to the blank line
    for _ in 0..HEADER_LIMIT {
        let mut header = Vec::new();
        match reader.by_ref().take(LINE_LIMIT).read_until(b'\n', &mut header) {
            Ok(0) | Err(_) => return,
            Ok(_) if header == b"\r\n" || header == b"\n" => break,
            Ok(_) => {}
        }
    }
    guard.tunnel(&host, port);
    let Ok(upstream) = TcpStream::connect((host.as_str(), port)) else {
        let mut stream = stream;
        let _ = stream.write_all(UNREACHABLE);
        return;
    };
    let _ = stream.set_read_timeout(None);
    let mut client = stream;
    if client.write_all(ESTABLISHED).is_err() {
        return;
    }
    // whatever the client sent after its headers is already in the reader's buffer
    let buffered = reader.buffer().to_vec();
    splice(reader.into_inner(), &buffered, upstream);
}

/// Copies both ways between the client and `upstream` until either side closes, `buffered` (what the client sent
/// after its request) first.
fn splice(client: TcpStream, buffered: &[u8], upstream: TcpStream) {
    let (Ok(mut client_out), Ok(mut upstream_in)) = (client.try_clone(), upstream.try_clone()) else {
        return;
    };
    let mut upstream_out = upstream;
    if upstream_out.write_all(buffered).is_err() {
        return;
    }
    let mut client_in = client;
    let up = thread::Builder::new().name("signin-gate-up".into()).spawn(move || {
        let _ = io::copy(&mut client_in, &mut upstream_out);
        let _ = upstream_out.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut upstream_in, &mut client_out);
    let _ = client_out.shutdown(std::net::Shutdown::Write);
    if let Ok(up) = up {
        let _ = up.join();
    }
}

/// A `CONNECT` target's host (lowercased) and port: `host:port`, a name (no brackets, no user info).
fn authority(target: &str) -> Option<(String, u16)> {
    let (host, port) = target.rsplit_once(':')?;
    let port = port.parse().ok()?;
    let valid = !host.is_empty()
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
    valid.then(|| (host.to_ascii_lowercase(), port))
}
