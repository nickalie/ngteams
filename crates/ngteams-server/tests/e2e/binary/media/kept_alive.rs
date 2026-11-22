//! Requests that the server refuses before it reads their bodies, on a connection that a client keeps for its next
//! requests: an upload over its limit, an upload with a wrong token, a message too big to send. The client's next
//! requests on that same connection (a message, a file that fits, a health read) get their answers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::future::join_all;
use reqwest::Method;
use serde_json::json;
use teams_auth::authz::CHAT_BASE_GTM;
use teams_proto::{PastedFormat, PastedPicture};
use tokio::net::{TcpListener, TcpStream};

use super::super::push::support::User;
use super::support::{IC3, MIB, Stage, noise, send_file, token, try_upload, upload_path};
use crate::media::png;
use crate::support::backends;

/// The largest file the nodes here take.
const FILE_LIMIT: usize = MIB;
/// Rounds of refused and good requests on the one connection.
const ROUNDS: usize = 30;
/// How long one request or one round of the client may take before the test calls it a hang.
const HANG: Duration = Duration::from_secs(60);

/// A TCP relay in front of a node. It counts the connections that clients open through it, and the connections that
/// the node closed while the client still had them open.
struct Relay {
    origin: String,
    opened: Arc<AtomicUsize>,
    closed_by_node: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Relay {
    async fn to(endpoint: &str) -> Self {
        let target = endpoint.strip_prefix("http://").unwrap().to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let opened = Arc::new(AtomicUsize::new(0));
        let closed_by_node = Arc::new(AtomicUsize::new(0));
        let (counted, closed) = (Arc::clone(&opened), Arc::clone(&closed_by_node));
        let task = tokio::spawn(async move {
            while let Ok((inbound, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                let Ok(outbound) = TcpStream::connect(&target).await else {
                    continue;
                };
                let closed = Arc::clone(&closed);
                tokio::spawn(relay(inbound, outbound, closed));
            }
        });
        Self {
            origin,
            opened,
            closed_by_node,
            task,
        }
    }

    fn opened(&self) -> usize {
        self.opened.load(Ordering::SeqCst)
    }

    fn closed_by_node(&self) -> usize {
        self.closed_by_node.load(Ordering::SeqCst)
    }
}

/// Copies both ways between a client and the node. If the node ends the connection before the client does, it
/// counts one in `closed`. Only a clean end of the client's stream is the client's end: if a write to the node fails
/// (the node closed the socket during a body), that is a close by the node.
async fn relay(inbound: TcpStream, outbound: TcpStream, closed: Arc<AtomicUsize>) {
    use tokio::io::AsyncWriteExt as _;
    let (mut client_read, mut client_write) = inbound.into_split();
    let (mut node_read, mut node_write) = outbound.into_split();
    let client_left = Arc::new(AtomicBool::new(false));
    let left = Arc::clone(&client_left);
    let up = tokio::spawn(async move {
        if tokio::io::copy(&mut client_read, &mut node_write).await.is_ok() {
            left.store(true, Ordering::SeqCst);
        }
        let _ = node_write.shutdown().await;
    });
    let _ = tokio::io::copy(&mut node_read, &mut client_write).await;
    if !client_left.load(Ordering::SeqCst) {
        closed.fetch_add(1, Ordering::SeqCst);
    }
    let _ = client_write.shutdown().await;
    up.abort();
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One HTTP/1.1 client that keeps one idle connection, as a client's pool does.
fn kept_alive_client() -> reqwest::Client {
    reqwest::Client::builder()
        .http1_only()
        .pool_max_idle_per_host(1)
        .pool_idle_timeout(Duration::from_secs(300))
        .timeout(HANG)
        .build()
        .unwrap()
}

/// One HTTP client through a [`Relay`]: what it sent and where it had to open a new connection.
struct Caller {
    http: reqwest::Client,
    relay: Relay,
    /// The last request and the `connection` header of its answer.
    last: String,
    /// For each connection after the first: the request before it. The client's pool can open one while the
    /// last connection is not back in the pool yet, so this only tells where it happened.
    reopened: Vec<String>,
    /// For each connection that the node closed while the client kept it: the request before it. Also each answer
    /// with `connection: close`.
    closed: Vec<String>,
    closes: usize,
}

impl Caller {
    async fn to(endpoint: &str) -> Self {
        Self {
            http: kept_alive_client(),
            relay: Relay::to(endpoint).await,
            last: "none".to_owned(),
            reopened: Vec::new(),
            closed: Vec::new(),
            closes: 0,
        }
    }

    /// The status of `what`, sent as `method` `path` with `token` and `body` (its content type and bytes); the
    /// answer's body is read to its end.
    async fn status(
        &mut self,
        what: &str,
        method: Method,
        path: &str,
        token: Option<&str>,
        (content_type, body): (&str, Vec<u8>),
    ) -> u16 {
        self.closes_since_last();
        let before = self.relay.opened();
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.relay.origin))
            .header("content-type", content_type)
            .body(body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let answer = request
            .send()
            .await
            .unwrap_or_else(|error| panic!("{what}: the request failed: {error:?}"));
        let status = answer.status().as_u16();
        let connection = answer.headers().get("connection").cloned();
        answer.bytes().await.unwrap();
        if before > 0 && self.relay.opened() > before {
            self.reopened.push(format!("after {}", self.last));
        }
        self.last = format!("{what} ({status}, connection: {connection:?})");
        // an answer that tells the client to close is a close by the node, also when the relay does not see it
        if connection.is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"close")) {
            self.closed.push(format!("at {}", self.last));
        }
        status
    }

    /// Notes a close by the node since the last request, on that request.
    fn closes_since_last(&mut self) {
        let closes = self.relay.closed_by_node();
        if closes > self.closes {
            self.closed.push(format!("after {}", self.last));
            self.closes = closes;
        }
    }
}

/// A message as a client posts it, with `text` as its content.
fn message(text: &str, n: usize) -> Vec<u8> {
    json!({
        "messagetype": "Text", "contenttype": "Text", "content": text,
        "clientmessageid": (7_183_000_000_000_000_000_u64 + u64::try_from(n).unwrap()).to_string(),
    })
    .to_string()
    .into_bytes()
}

const OCTETS: &str = "application/octet-stream";
const JSON: &str = "application/json";

/// What a's requests here need: a's file and chat tokens, and where a posts a message to the chat.
struct Sender<'a> {
    a: &'a User,
    files: String,
    chats: String,
    messages: String,
}

impl<'a> Sender<'a> {
    async fn of(a: &'a User, origin: &str, chat: &str) -> Self {
        let base = a.session().service_base(CHAT_BASE_GTM).unwrap();
        let messages = format!(
            "{}/v1/users/ME/conversations/{chat}/messages",
            base.strip_prefix(origin).unwrap()
        );
        Self {
            a,
            files: token(a, origin).await,
            chats: token(a, IC3).await,
            messages,
        }
    }

    /// Round `round` of [`requests_refused_before_their_bodies_were_read_leave_the_kept_connection_to_the_next_requests`].
    async fn round(&self, caller: &mut Caller, round: usize, too_big: &[u8], fits: &[u8]) {
        let path = upload_path(self.a, &format!("over-{round}.bin"));
        let over = caller.status(
            "a file over the limit",
            Method::PUT,
            &path,
            Some(&self.files),
            (OCTETS, too_big.to_vec()),
        );
        assert_eq!(over.await, 413, "round {round}");
        let body = (OCTETS, noise(512 * 1024, 9));
        let wrong = caller.status(
            "a file with a wrong token",
            Method::PUT,
            &path,
            Some("not-a-token"),
            body,
        );
        assert_eq!(wrong.await, 401, "round {round}");
        let long = (JSON, message(&"x".repeat(100 * 1024), round));
        let refused = caller.status(
            "a message too long",
            Method::POST,
            &self.messages,
            Some(&self.chats),
            long,
        );
        // the chat service refuses a body over its 64 KiB as a bad request
        assert_eq!(refused.await, 400, "round {round}");

        let text = (
            JSON,
            message(&format!("after the refusals, round {round}"), ROUNDS + round),
        );
        let sent = caller.status("a message", Method::POST, &self.messages, Some(&self.chats), text);
        assert_eq!(sent.await, 201, "round {round}");
        let path = upload_path(self.a, &format!("fits-{round}.bin"));
        let kept = caller.status(
            "a file that fits",
            Method::PUT,
            &path,
            Some(&self.files),
            (OCTETS, fits.to_vec()),
        );
        assert_eq!(kept.await, 201, "round {round}");
        let live = caller.status(
            "a health read",
            Method::GET,
            "/health/live",
            None,
            ("text/plain", Vec::new()),
        );
        assert_eq!(live.await, 204, "round {round}");
    }
}

/// One HTTP client, one kept connection: each round sends a file over the limit (413), a file with a wrong token
/// (401) and a message too big for the chat service (400), each with its body. Then on the same connection it sends
/// a message (201), a file that fits (201) and a health read (204). Nothing fails, and the node never closes the
/// connection that the client keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_refused_before_their_bodies_were_read_leave_the_kept_connection_to_the_next_requests() {
    let config = format!("[media]\nmax_file_bytes = {FILE_LIMIT}\n");
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, b, c, chat) = stage.people().await;
        let mut caller = Caller::to(&stage.rig.first.endpoint).await;
        let sender = Sender::of(&a, &stage.rig.origin, &chat).await;
        let too_big = noise(3 * FILE_LIMIT, 7);
        let fits = noise(FILE_LIMIT / 4, 8);
        for round in 0..ROUNDS {
            sender.round(&mut caller, round, &too_big, &fits).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        caller.closes_since_last();
        assert_eq!(
            caller.closed,
            Vec::<String>::new(),
            "{backend:?}: the node closed a kept connection"
        );
        eprintln!(
            "{backend:?}: new connections of the client's pool: {:?}",
            caller.reopened
        );
        let kept = stage.blobs.keys().await;
        assert_eq!(kept.len(), ROUNDS, "only the files that fit are kept: {kept:?}");
        drop(sender);
        drop((a, b, c, caller));
        stage.stop().await;
    }
}

/// `a`'s client sends a file and pastes a picture, each over its limit, and shows the refusals. At once after that it
/// sends five messages and a file that fits, all together; b's client gets them all. Nothing hangs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_refused_an_oversized_attachment_then_sends_messages_and_a_file_together() {
    let large = png(120, 120);
    let config = format!(
        "[media]\nmax_file_bytes = {FILE_LIMIT}\nmax_picture_bytes = {}\n",
        large.len() - 1
    );
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, mut b, c, chat) = stage.people().await;
        let over = noise(3 * FILE_LIMIT, 11);
        for round in 0..10_u64 {
            let refused = tokio::time::timeout(HANG, try_upload(&a, "over.bin", &over)).await;
            let said = refused.expect("the refused upload hung").map(|_| ()).unwrap_err();
            assert!(said.contains("too large to upload"), "round {round}: {said}");
            let picture = PastedPicture {
                format: PastedFormat::Png,
                bytes: large.clone(),
            };
            let pasted = a.engine().upload_picture(chat.clone(), picture);
            let pasted = tokio::time::timeout(HANG, pasted)
                .await
                .expect("the refused picture hung");
            let said = format!("{:?}", pasted.unwrap_err());
            assert!(said.contains("413"), "round {round}: {said}");

            let texts: Vec<String> = (0..5).map(|n| format!("round {round}, message {n}")).collect();
            let messages = join_all(texts.iter().map(|text| a.send(&chat, text)));
            let name = format!("fits-{round}.bin");
            let fits = noise(FILE_LIMIT / 2, round);
            let file = send_file(&a, &stage.cloud, &chat, &name, &fits);
            let (mut ids, file) = tokio::time::timeout(HANG, futures_util::future::join(messages, file))
                .await
                .unwrap_or_else(|_| panic!("round {round}: the sends after the refusals hung"));
            ids.push(file);
            for id in &ids {
                b.eventually_stored(&chat, id).await;
            }
        }
        drop((a, b, c));
        stage.stop().await;
    }
}
