//! The mock's front: a loopback HTTP/1.1 relay to the mock's own server, on a thread of its own, where the requests a
//! test holds ([`ChatServiceMock::hold`], `startup.rs`) wait. Each connection is relayed one request at a time, as a
//! client's HTTP/1.1 connection sends them; a held request waits alone, the other connections go on. It reads only
//! what the clients send and the mock answers: a head, then a `Content-Length` or chunked body (none for a 1xx, 204
//! or 304 answer).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::ChatServiceMock;
use crate::world::State;

/// The running front; dropping it stops it.
pub struct Front {
    origin: String,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Front {
    /// The origin clients send to (`http://127.0.0.1:<port>`), in place of [`ChatServiceMock::origin`].
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl ChatServiceMock {
    /// Starts the mock's front on a free loopback port: every request sent to its origin reaches the mock, but for
    /// those a hold keeps until it lets them go.
    ///
    /// # Errors
    /// When no port can be bound or its thread's runtime can't be built.
    pub fn start_front(&self) -> std::io::Result<Front> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let origin = format!("http://{}", listener.local_addr()?);
        let upstream = *self.server.address();
        let state = Arc::clone(&self.state);
        let (shutdown, stopped) = oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let Ok(listener) = TcpListener::from_std(listener) else {
                    return;
                };
                tokio::select! {
                    () = accept(listener, state, upstream) => {}
                    _ = stopped => {}
                }
            });
        });
        Ok(Front {
            origin,
            shutdown: Some(shutdown),
            thread: Some(thread),
        })
    }
}

async fn accept(listener: TcpListener, state: Arc<Mutex<State>>, upstream: SocketAddr) {
    while let Ok((client, _)) = listener.accept().await {
        tokio::spawn(relay(client, Arc::clone(&state), upstream));
    }
}

/// Relays `client`'s requests to the mock one at a time, each held first while a hold matches it.
async fn relay(client: TcpStream, state: Arc<Mutex<State>>, upstream: SocketAddr) -> std::io::Result<()> {
    let (client_read, mut client_write) = client.into_split();
    let mut client_read = BufReader::new(client_read);
    let mut server: Option<(
        BufReader<tokio::net::tcp::OwnedReadHalf>,
        tokio::net::tcp::OwnedWriteHalf,
    )> = None;
    while let Some(request) = message(&mut client_read, false).await? {
        let target = request_target(&request).unwrap_or_default();
        let gate = {
            let state = state.lock().unwrap_or_else(PoisonError::into_inner);
            state.holds.iter().find(|gate| gate.matches(&target)).cloned()
        };
        if let Some(gate) = gate {
            let refused = tokio::task::spawn_blocking(move || gate.pass()).await.ok().flatten();
            if let Some(code) = refused {
                let answer = format!("HTTP/1.1 {code} Held\r\ncontent-length: 0\r\n\r\n");
                client_write.write_all(answer.as_bytes()).await?;
                continue;
            }
        }
        if server.is_none() {
            let (read, write) = TcpStream::connect(upstream).await?.into_split();
            server = Some((BufReader::new(read), write));
        }
        let Some((server_read, server_write)) = server.as_mut() else {
            return Ok(());
        };
        server_write.write_all(&request).await?;
        let Some(answer) = message(server_read, true).await? else {
            return Ok(());
        };
        client_write.write_all(&answer).await?;
    }
    Ok(())
}

/// The request line's target: `GET <target> HTTP/1.1`.
fn request_target(request: &[u8]) -> Option<String> {
    let line = request.split(|&byte| byte == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    line.split(' ').nth(1).map(str::to_owned)
}

/// The next whole message on `from` (its head and body as they came); `None` at the end of the stream.
async fn message(from: &mut (impl AsyncBufRead + Unpin), answer: bool) -> std::io::Result<Option<Vec<u8>>> {
    let mut whole = Vec::new();
    let mut length = None;
    let mut chunked = false;
    let mut status = 0_u16;
    loop {
        let start = whole.len();
        if from.read_until(b'\n', &mut whole).await? == 0 {
            return Ok(None);
        }
        let line = String::from_utf8_lossy(&whole[start..]).trim_end().to_ascii_lowercase();
        if start == 0 && answer {
            status = line.split(' ').nth(1).and_then(|code| code.parse().ok()).unwrap_or(0);
        }
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().ok();
        }
        if line.starts_with("transfer-encoding:") && line.contains("chunked") {
            chunked = true;
        }
    }
    if answer && (status < 200 || status == 204 || status == 304) {
        return Ok(Some(whole));
    }
    if chunked {
        chunks(from, &mut whole).await?;
        return Ok(Some(whole));
    }
    let start = whole.len();
    whole.resize(start + length.unwrap_or(0), 0);
    from.read_exact(&mut whole[start..]).await?;
    Ok(Some(whole))
}

/// A chunked body's chunks and trailers, up to its empty line, onto `whole`.
async fn chunks(from: &mut (impl AsyncBufRead + Unpin), whole: &mut Vec<u8>) -> std::io::Result<()> {
    loop {
        let start = whole.len();
        from.read_until(b'\n', whole).await?;
        let size = String::from_utf8_lossy(&whole[start..]);
        let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or_default(), 16)
            .map_err(|_| std::io::Error::other("a chunk without its size"))?;
        if size == 0 {
            break;
        }
        let start = whole.len();
        whole.resize(start + size + 2, 0);
        from.read_exact(&mut whole[start..]).await?;
    }
    loop {
        let start = whole.len();
        from.read_until(b'\n', whole).await?;
        if whole[start..].iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
    }
}
