//! A local Telegram Bot API for the sticker pack import's tests: `getStickerSet`, `getFile` and the file download, for
//! one made-up bot token and the packs a test gives. Its answers have the shapes the Bot API documents
//! (`docs/protocol/stickers.md`). It runs a plain loopback HTTP server on a thread of its own, one request to a
//! connection, and records each request's path and whether it had an `Authorization` header.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use serde_json::{Value, json};

/// The made-up bot token the mock takes: of `@BotFather`'s form, never a real bot's.
pub const SYNTHETIC_BOT_TOKEN: &str = "123456789:SYNTHETIC_bot_token_made_up_000000";

/// How a sticker of a mock pack is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MockKind {
    /// A `.webp` picture.
    Still,
    /// A `.tgs` animation.
    Animated,
    /// A `.webm` video.
    Video,
}

/// A sticker of a mock pack.
#[derive(Clone, Debug)]
pub struct MockSticker {
    pub kind: MockKind,
    pub emoji: String,
    pub bytes: Vec<u8>,
}

/// A pack the mock serves.
#[derive(Clone, Debug)]
pub struct MockSet {
    pub name: String,
    pub title: String,
    pub stickers: Vec<MockSticker>,
}

/// A request the mock got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockRequest {
    /// The request's target: its path and query.
    pub target: String,
    /// Whether it had an `Authorization` header.
    pub authorization: bool,
}

/// The mock Bot API, served while it lives.
pub struct BotApiMock {
    origin: String,
    requests: Arc<Mutex<Vec<MockRequest>>>,
}

impl BotApiMock {
    /// Starts the mock, serving `sets`.
    ///
    /// # Errors
    /// The loopback port could not be opened.
    pub fn start(sets: Vec<MockSet>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let origin = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let sets = Arc::new(sets);
        thread::Builder::new().name("bot-api-mock".to_owned()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let sets = Arc::clone(&sets);
                let seen = Arc::clone(&seen);
                let _ = thread::Builder::new()
                    .name("bot-api-mock-request".to_owned())
                    .spawn(move || serve(stream, &sets, &seen));
            }
        })?;
        Ok(Self { origin, requests })
    }

    /// The mock's origin, `http://127.0.0.1:<port>`.
    #[must_use]
    pub fn origin(&self) -> String {
        self.origin.clone()
    }

    /// The requests so far.
    #[must_use]
    pub fn requests(&self) -> Vec<MockRequest> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

fn serve(stream: TcpStream, sets: &[MockSet], seen: &Mutex<Vec<MockRequest>>) {
    let Ok(reader) = stream.try_clone() else { return };
    let mut reader = BufReader::new(reader);
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut authorization = false;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        authorization |= header.to_ascii_lowercase().starts_with("authorization:");
    }
    let target = first.split_whitespace().nth(1).unwrap_or_default().to_owned();
    seen.lock().unwrap_or_else(PoisonError::into_inner).push(MockRequest {
        target: target.clone(),
        authorization,
    });
    let (status, kind, body) = answer(&target, sets);
    let mut stream = stream;
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
    let _ = reader.read(&mut [0; 1]);
}

fn json_answer(status: &'static str, value: &Value) -> (&'static str, &'static str, Vec<u8>) {
    (status, "application/json", value.to_string().into_bytes())
}

fn refused(code: u16, description: &str) -> Value {
    json!({ "ok": false, "error_code": code, "description": description })
}

fn extension(kind: MockKind) -> &'static str {
    match kind {
        MockKind::Still => "webp",
        MockKind::Animated => "tgs",
        MockKind::Video => "webm",
    }
}

/// The file id of sticker `index` of `set`: `<set>__<index>`.
fn file_id(set: &MockSet, index: usize) -> String {
    format!("{}__{index}", set.name)
}

fn find<'a>(sets: &'a [MockSet], id: &str) -> Option<(&'a MockSet, usize, &'a MockSticker)> {
    let (name, index) = id.rsplit_once("__")?;
    let index: usize = index.parse().ok()?;
    let set = sets.iter().find(|set| set.name == name)?;
    Some((set, index, set.stickers.get(index)?))
}

fn query<'a>(target: &'a str, name: &str) -> Option<&'a str> {
    let (_, query) = target.split_once('?')?;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

fn answer(target: &str, sets: &[MockSet]) -> (&'static str, &'static str, Vec<u8>) {
    let path = target.split('?').next().unwrap_or_default();
    if let Some(rest) = path.strip_prefix("/file/bot") {
        let Some(file) = rest
            .strip_prefix(SYNTHETIC_BOT_TOKEN)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            return ("404 Not Found", "text/plain", b"Not Found".to_vec());
        };
        let id = file
            .strip_prefix("stickers/")
            .and_then(|file| file.rsplit_once('.'))
            .map(|(id, _)| id);
        return match id.and_then(|id| find(sets, id)) {
            Some((_, _, sticker)) => ("200 OK", "application/octet-stream", sticker.bytes.clone()),
            None => ("404 Not Found", "text/plain", b"Not Found".to_vec()),
        };
    }
    let Some(rest) = path.strip_prefix("/bot") else {
        return json_answer("404 Not Found", &refused(404, "Not Found"));
    };
    let Some(method) = rest
        .strip_prefix(SYNTHETIC_BOT_TOKEN)
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return json_answer("401 Unauthorized", &refused(401, "Unauthorized"));
    };
    match method {
        "getStickerSet" => sticker_set(query(target, "name").unwrap_or_default(), sets),
        "getFile" => file_info(query(target, "file_id").unwrap_or_default(), sets),
        _ => json_answer("404 Not Found", &refused(404, "Not Found")),
    }
}

fn sticker_set(name: &str, sets: &[MockSet]) -> (&'static str, &'static str, Vec<u8>) {
    let Some(set) = sets.iter().find(|set| set.name == name) else {
        return json_answer("400 Bad Request", &refused(400, "Bad Request: STICKERSET_INVALID"));
    };
    let stickers: Vec<Value> = set
        .stickers
        .iter()
        .enumerate()
        .map(|(index, sticker)| {
            json!({
                "file_id": file_id(set, index),
                "file_unique_id": format!("u{index}"),
                "type": "regular",
                "width": 512,
                "height": 512,
                "is_animated": sticker.kind == MockKind::Animated,
                "is_video": sticker.kind == MockKind::Video,
                "emoji": sticker.emoji,
                "file_size": sticker.bytes.len(),
            })
        })
        .collect();
    json_answer(
        "200 OK",
        &json!({
            "ok": true,
            "result": { "name": set.name, "title": set.title, "sticker_type": "regular", "stickers": stickers },
        }),
    )
}

fn file_info(id: &str, sets: &[MockSet]) -> (&'static str, &'static str, Vec<u8>) {
    let Some((set, index, sticker)) = find(sets, id) else {
        return json_answer("400 Bad Request", &refused(400, "Bad Request: invalid file_id"));
    };
    json_answer(
        "200 OK",
        &json!({
            "ok": true,
            "result": {
                "file_id": id,
                "file_unique_id": format!("u{index}"),
                "file_size": sticker.bytes.len(),
                "file_path": format!("stickers/{}.{}", file_id(set, index), extension(sticker.kind)),
            },
        }),
    )
}
