//! The socket's side of the server: the bind, a listener thread accepting connections, and a thread per connection
//! reading its request lines, handing each to GPUI's foreground ([`Job`]) and writing back its answer.
//!
//! **The Unix bind.** The socket's directory is made `0700` when missing ([`teams_ctl::prepare_dir`]) and the socket is
//! `0600`: only the user's own processes reach it. A socket file already there is asked whether anyone is behind it:
//! one that answers the connect is another app running ([`BindError::AlreadyRunning`]); one that refuses it was left by
//! an app that didn't quit (a crash, a kill), so it is removed and bound afresh. Anything there that isn't a socket is
//! left alone ([`BindError::NotASocket`]).
//!
//! Windows uses a local-only named pipe with a protected current-user DACL, integrity label and peer identity
//! checks (`teams_ctl::transport`); its logical path names no filesystem socket, and closing it leaves no stale file.
//!
//! **The stop** ([`Listener::stop`]): every connection is shut down (each thread ends at its next read or write), the
//! listening socket too (its blocked `accept` returns), the listener thread is joined, and the socket file is removed
//! when it is still the one bound here.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use teams_ctl::transport::Stream as UnixStream;

use teams_ctl::codec::{self, CodecError, LineReader};
use teams_ctl::protocol::{CtlError, CtlEvent, ErrorCode, EventLine, Outcome, RequestLine, ResponseLine};
use tokio::sync::oneshot;

#[cfg(unix)]
#[path = "socket/unix.rs"]
mod socket;
#[cfg(windows)]
#[path = "socket/windows.rs"]
mod socket;
use socket::Socket;

/// How long an answer's write may wait on a client that doesn't read.
const WRITE: Duration = Duration::from_secs(10);

/// A request for GPUI's foreground, and where its answer goes.
pub struct Job {
    pub line: RequestLine,
    pub reply: oneshot::Sender<Answer>,
}

/// A request's answer: its line, and for a `subscribe` taken the events the connection writes from then on.
pub struct Answer {
    pub line: ResponseLine,
    pub events: Option<async_channel::Receiver<EventLine>>,
}

impl From<ResponseLine> for Answer {
    fn from(line: ResponseLine) -> Self {
        Self { line, events: None }
    }
}

/// Why the socket wasn't bound.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// Another app answers on the socket.
    #[error("another ngteams already serves {}", .0.display())]
    AlreadyRunning(PathBuf),
    /// Something other than a socket is at the path.
    #[error("{} is there and isn't a socket: it is left as it is", .0.display())]
    NotASocket(PathBuf),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// The private socket bound, not served yet ([`Bound::serve`]).
#[derive(Debug)]
pub struct Bound {
    socket: Socket,
}

/// Bind a native private endpoint at `path`.
///
/// # Errors
/// Another app serves there, a non-socket file occupies a Unix path, or native binding failed.
pub fn bind(path: &Path) -> Result<Bound, BindError> {
    socket::bind(path).map(|socket| Bound { socket })
}

impl Bound {
    /// Accept connections on a thread of its own, each served on a thread of its own, their requests sent to `jobs`.
    ///
    /// # Errors
    /// The listener's thread couldn't be started (the socket is closed again and its file removed).
    pub fn serve(self, jobs: async_channel::Sender<Job>) -> io::Result<Listener> {
        let socket = Arc::new(self.socket);
        let connections = Arc::new(Mutex::new(Connections::default()));
        let mut stopping = Listener {
            path: socket.path.clone(),
            socket: Arc::clone(&socket),
            connections: Arc::clone(&connections),
            thread: None,
        };
        let thread = thread::Builder::new()
            .name("ctl-listener".to_owned())
            .spawn(move || accept(&socket, &jobs, &connections));
        match thread {
            Ok(thread) => {
                stopping.thread = Some(thread);
                Ok(stopping)
            }
            Err(error) => {
                stopping.stop();
                Err(error)
            }
        }
    }
}

/// The connections open, each by a number of its own, so the stop can shut them down.
#[derive(Default)]
struct Connections {
    stopped: bool,
    next: u64,
    open: HashMap<u64, UnixStream>,
}

fn lock(connections: &Mutex<Connections>) -> MutexGuard<'_, Connections> {
    connections.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The socket served (see the module docs); stopped when dropped.
pub struct Listener {
    /// The listening socket, for its shutdown (the thread holds it too).
    socket: Arc<Socket>,
    path: PathBuf,
    connections: Arc<Mutex<Connections>>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    /// Where it serves.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Close every connection and the socket, and remove its file (see the module docs). A second stop does nothing.
    pub fn stop(&mut self) {
        {
            let mut connections = lock(&self.connections);
            if connections.stopped {
                return;
            }
            connections.stopped = true;
            for (_, stream) in connections.open.drain() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        self.socket.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.socket.cleanup();
        log::info!("the control socket is closed");
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The listener thread: each connection accepted is served on a thread of its own, until the stop.
fn accept(listener: &Socket, jobs: &async_channel::Sender<Job>, connections: &Arc<Mutex<Connections>>) {
    loop {
        if lock(connections).stopped {
            return;
        }
        let stream = match listener.accept() {
            Ok(stream) => stream,
            Err(_) if lock(connections).stopped => return,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => {
                // out of descriptors, say: tried again a little later rather than spun on
                log::warn!("the control socket didn't accept a connection: {error}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let number = match register(connections, &stream) {
            Ok(Some(number)) => number,
            Ok(None) => {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
            Err(error) => {
                // out of descriptors, say: this client is turned away, the next one is accepted again
                log::warn!("a control connection wasn't held: {error}");
                let _ = stream.shutdown(std::net::Shutdown::Both);
                continue;
            }
        };
        let (jobs, held) = (jobs.clone(), Arc::clone(connections));
        let served = thread::Builder::new().name("ctl-connection".to_owned()).spawn(move || {
            serve(stream, &jobs);
            lock(&held).open.remove(&number);
        });
        if let Err(error) = served {
            // the closure (and its stream) is gone unrun: the clone held for the stop closes the connection, so the
            // client reads the end rather than waiting on an answer that never comes
            log::warn!("a control connection wasn't served: {error}");
            if let Some(stream) = lock(connections).open.remove(&number) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// `stream` among the connections the stop shuts down: `Ok(None)` once stopped, an error when it can't be held (no
/// descriptor left for its clone).
fn register(connections: &Mutex<Connections>, stream: &UnixStream) -> io::Result<Option<u64>> {
    let held = stream.try_clone()?;
    let mut connections = lock(connections);
    if connections.stopped {
        return Ok(None);
    }
    let number = connections.next;
    connections.next += 1;
    connections.open.insert(number, held);
    Ok(Some(number))
}

/// One connection: each request line answered in turn, until the client closes it, the app stops serving, or a line
/// can't be read on from (one over the size cap leaves the stream mid-line). A `subscribe` taken turns it into its
/// event stream ([`write_events`]).
fn serve(stream: UnixStream, jobs: &async_channel::Sender<Job>) {
    let _ = stream.set_write_timeout(Some(WRITE));
    let Ok(read) = stream.try_clone() else { return };
    let mut reader = LineReader::new(BufReader::new(read));
    let mut writer = stream;
    loop {
        let (answer, go_on) = match reader.read_line() {
            Ok(Some(line)) => match codec::decode_request(&line) {
                Ok(line) => match ask(jobs, line) {
                    Some(answer) => (answer, true),
                    // the app is going: nobody answers any more
                    None => return,
                },
                Err(bad) => (refused(bad.id, bad.error).into(), true),
            },
            Err(CodecError::Utf8) => (bad_request("a line isn't UTF-8").into(), true),
            Err(CodecError::TooLong { limit }) => (bad_request(&format!("a line is over {limit} bytes")).into(), false),
            // the client closed it, or it can't be read on from
            Ok(None) | Err(_) => return,
        };
        if codec::write_line(&mut writer, &answer.line).is_err() || !go_on {
            return;
        }
        if let Some(events) = answer.events {
            write_events(&events, &writer);
            return;
        }
    }
}

/// A subscriber's events written as they come, until its stream closes (the quit, or the client gone), a write fails,
/// or after its `lagged` line.
///
/// The thread waits on the stream, not the socket, so a thread of its own watches the socket for the client's hang-up
/// and closes the stream then: a client gone with a filter no event passes would otherwise hold this thread, its
/// descriptors and its place in the hub for good. Once the writes end the socket is shut down, which ends the watch.
fn write_events(events: &async_channel::Receiver<EventLine>, writer: &UnixStream) {
    thread::scope(|scope| {
        let watching = thread::Builder::new()
            .name("ctl-hang-up".to_owned())
            .spawn_scoped(scope, || {
                if hung_up(writer) {
                    events.close();
                }
            });
        if let Err(error) = watching {
            log::warn!("a subscriber's hang-up isn't watched: its stream ends at its next event: {error}");
        }
        let mut writer = writer;
        let mut deadline = Instant::now() + WRITE;
        while let Ok(line) = events.recv_blocking() {
            let last = line.event == CtlEvent::Lagged;
            deadline = Instant::now() + WRITE;
            if codec::write_line(&mut writer, &line).is_err() || last {
                break;
            }
        }
        // DisconnectNamedPipe discards unread bytes, including the final lagged line. Drain within the last write's
        // existing deadline before disconnecting; stop still forces shutdown and wakes both this and the watch.
        let _ = teams_ctl::transport::finish_writes(writer, deadline);
    });
}

/// Waits until `stream` is hung up (`true`): the client closed it (or shut it down both ways), or it was shut down
/// here. A client that only stops writing hasn't hung up, and what it writes is never read. `false`: the watch failed,
/// and the stream ends at its next event, as without one.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn hung_up(stream: &UnixStream) -> bool {
    use rustix::event::{PollFd, PollFlags, poll};

    // no event asked for: a poll still wakes for a hang-up or an error, and only for those
    let mut watched = [PollFd::new(stream, PollFlags::empty())];
    loop {
        match poll(&mut watched, None) {
            Ok(_) if watched[0].revents().is_empty() => {}
            Ok(_) => return true,
            Err(rustix::io::Errno::INTR) => {}
            Err(error) => {
                log::warn!("a subscriber's hang-up isn't watched: its stream ends at its next event: {error}");
                return false;
            }
        }
    }
}

/// macOS's `poll` reports no hang-up on a Unix socket unless reading is asked about too: the stream is watched for
/// reading, and what a client writes is taken off and dropped (never read as a request), until its end.
#[cfg(target_vendor = "apple")]
fn hung_up(stream: &UnixStream) -> bool {
    use rustix::event::{PollFd, PollFlags, poll};
    use rustix::net::{RecvFlags, recv};

    let mut dropped = [0u8; 4096];
    loop {
        let mut watched = [PollFd::new(stream, PollFlags::IN)];
        match poll(&mut watched, None) {
            Ok(_) if watched[0].revents().intersects(PollFlags::HUP | PollFlags::ERR) => return true,
            Ok(_) if watched[0].revents().contains(PollFlags::IN) => {
                match recv(stream, &mut dropped, RecvFlags::DONTWAIT) {
                    Ok((0, _)) => return true,
                    Ok(_) | Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
                    Err(_) => return true,
                }
            }
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(error) => {
                log::warn!("a subscriber's hang-up isn't watched: its stream ends at its next event: {error}");
                return false;
            }
        }
    }
}

#[cfg(windows)]
fn hung_up(stream: &UnixStream) -> bool {
    loop {
        match stream.peer_closed() {
            Ok(true) => return true,
            Ok(false) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                log::warn!("a subscriber's hang-up isn't watched: {error}");
                return false;
            }
        }
    }
}

/// `line` answered on GPUI's foreground; `None` when the app no longer takes requests.
fn ask(jobs: &async_channel::Sender<Job>, line: RequestLine) -> Option<Answer> {
    let (reply, answer) = oneshot::channel();
    jobs.send_blocking(Job { line, reply }).ok()?;
    answer.blocking_recv().ok()
}

fn refused(id: u64, error: CtlError) -> ResponseLine {
    ResponseLine {
        id,
        outcome: Outcome::Error(error),
    }
}

fn bad_request(message: &str) -> ResponseLine {
    refused(0, CtlError::new(ErrorCode::BadRequest, message))
}
