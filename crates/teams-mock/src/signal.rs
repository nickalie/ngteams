//! A local Signal sticker CDN for the sticker pack import's tests: `GET /stickers/<pack id>/manifest.proto` and
//! `GET /stickers/<pack id>/full/<sticker id>`, each blob encrypted as Signal encrypts it (`docs/protocol/stickers.md`)
//! with keys of its own derivation, so the client's decryption is checked against a second implementation. An unknown
//! pack gets 403, as Signal's CDN gives it. It runs a plain loopback HTTP server on a thread of its own, one request to
//! a connection, and records each request's path and whether it had a credential (`Authorization` or `Cookie`).

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use aes::Aes256;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeEncrypt as _, KeyIvInit as _};
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::{Digest as _, Sha256};

/// A made-up pack id: 32 hex digits, never a real pack's.
pub const SYNTHETIC_PACK_ID: &str = "5f17e7105f17e7105f17e7105f17e710";
/// A made-up pack key: 64 hex digits, never a real pack's.
pub const SYNTHETIC_PACK_KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

/// A sticker of a mock pack.
#[derive(Clone, Debug)]
pub struct MockSticker {
    pub emoji: String,
    /// As the manifest names it (`image/png`, `image/webp`, `image/apng`).
    pub content_type: String,
    /// Its picture, as the pack's author uploaded it.
    pub bytes: Vec<u8>,
}

/// A pack the mock serves: its stickers' ids are their places (from 0), the first is the cover.
#[derive(Clone, Debug)]
pub struct MockPack {
    /// 32 hex digits.
    pub id: String,
    /// 64 hex digits.
    pub key: String,
    pub title: String,
    pub author: String,
    pub stickers: Vec<MockSticker>,
}

impl MockPack {
    /// A pack of `stickers` at [`SYNTHETIC_PACK_ID`] with [`SYNTHETIC_PACK_KEY`].
    #[must_use]
    pub fn synthetic(title: &str, stickers: Vec<MockSticker>) -> Self {
        Self {
            id: SYNTHETIC_PACK_ID.to_owned(),
            key: SYNTHETIC_PACK_KEY.to_owned(),
            title: title.to_owned(),
            author: "Made-up author".to_owned(),
            stickers,
        }
    }

    /// The pack's `signal.art` link.
    #[must_use]
    pub fn link(&self) -> String {
        format!(
            "https://signal.art/addstickers/#pack_id={}&pack_key={}",
            self.id, self.key
        )
    }
}

/// A request the mock got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockRequest {
    /// The request's target: its path and query.
    pub target: String,
    /// Whether it had an `Authorization` or a `Cookie` header.
    pub credential: bool,
}

/// A wrong answer the mock gives on purpose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MockFault {
    /// The manifest's last byte changed: its HMAC fails.
    TamperedManifest,
    /// Sticker `n`'s last byte changed: its HMAC fails.
    TamperedSticker(u32),
    /// Sticker `n`: 403, as for a sticker the CDN does not hold.
    MissingSticker(u32),
    /// From the `n`th sticker download (from 0) on: the connection closes with no answer.
    DroppedFrom(usize),
    /// Each request: a `302 Found` to `location`.
    RedirectTo(String),
    /// Each request: `500 Internal Server Error`.
    Broken,
}

/// The mock CDN, served while it lives.
pub struct SignalCdnMock {
    origin: String,
    requests: Arc<Mutex<Vec<MockRequest>>>,
    fault: Arc<Mutex<Option<MockFault>>>,
}

impl SignalCdnMock {
    /// Starts the mock, serving `packs`.
    ///
    /// # Errors
    /// The loopback port could not be opened.
    pub fn start(packs: Vec<MockPack>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let origin = format!("http://{}", listener.local_addr()?);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let fault = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&requests);
        let faulty = Arc::clone(&fault);
        let packs = Arc::new(packs);
        thread::Builder::new()
            .name("signal-cdn-mock".to_owned())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let packs = Arc::clone(&packs);
                    let seen = Arc::clone(&seen);
                    let faulty = Arc::clone(&faulty);
                    let _ = thread::Builder::new()
                        .name("signal-cdn-mock-request".to_owned())
                        .spawn(move || serve(stream, &packs, &seen, &faulty));
                }
            })?;
        Ok(Self {
            origin,
            requests,
            fault,
        })
    }

    /// Gives `fault` from now on.
    pub fn fault(&self, fault: MockFault) {
        *self.fault.lock().unwrap_or_else(PoisonError::into_inner) = Some(fault);
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

fn serve(stream: TcpStream, packs: &[MockPack], seen: &Mutex<Vec<MockRequest>>, fault: &Mutex<Option<MockFault>>) {
    let Ok(reader) = stream.try_clone() else { return };
    let mut reader = BufReader::new(reader);
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let mut credential = false;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        let header = header.to_ascii_lowercase();
        credential |= header.starts_with("authorization:") || header.starts_with("cookie:");
    }
    let target = first.split_whitespace().nth(1).unwrap_or_default().to_owned();
    let downloads = {
        let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
        seen.push(MockRequest {
            target: target.clone(),
            credential,
        });
        seen.iter().filter(|request| request.target.contains("/full/")).count()
    };
    let fault = fault.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(MockFault::DroppedFrom(from)) = fault
        && target.contains("/full/")
        && downloads > from
    {
        return;
    }
    let (status, body, location) = match fault {
        Some(MockFault::RedirectTo(location)) => ("302 Found", Vec::new(), format!("Location: {location}\r\n")),
        Some(MockFault::Broken) => ("500 Internal Server Error", Vec::new(), String::new()),
        fault => {
            let (status, body) = answer(&target, packs, fault.as_ref());
            (status, body, String::new())
        }
    };
    let mut stream = stream;
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n{location}Connection: \
         close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
    let _ = reader.read(&mut [0; 1]);
}

/// What the CDN holds at `target`, encrypted, or its 403.
fn answer(target: &str, packs: &[MockPack], fault: Option<&MockFault>) -> (&'static str, Vec<u8>) {
    let forbidden = ("403 Forbidden", b"<Error><Code>AccessDenied</Code></Error>".to_vec());
    let Some(rest) = target.strip_prefix("/stickers/") else {
        return forbidden;
    };
    let Some((id, file)) = rest.split_once('/') else {
        return forbidden;
    };
    let Some(pack) = packs.iter().find(|pack| pack.id == id) else {
        return forbidden;
    };
    let (plain, tampered, salt) = if file == "manifest.proto" {
        (manifest(pack), fault == Some(&MockFault::TamperedManifest), u32::MAX)
    } else {
        let Some(number) = file.strip_prefix("full/").and_then(|number| number.parse::<u32>().ok()) else {
            return forbidden;
        };
        if fault == Some(&MockFault::MissingSticker(number)) {
            return forbidden;
        }
        let Some(sticker) = usize::try_from(number).ok().and_then(|at| pack.stickers.get(at)) else {
            return forbidden;
        };
        (
            sticker.bytes.clone(),
            fault == Some(&MockFault::TamperedSticker(number)),
            number,
        )
    };
    let mut blob = encrypt(&pack.key, salt, &plain);
    if tampered && let Some(last) = blob.last_mut() {
        *last ^= 1;
    }
    ("200 OK", blob)
}

/// `value` as a protobuf varint.
fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(u8::try_from(value & 0x7f).unwrap_or_default() | 0x80);
        value >>= 7;
    }
    out.push(u8::try_from(value).unwrap_or_default());
}

/// A length-delimited field.
fn field(number: u64, bytes: &[u8], out: &mut Vec<u8>) {
    varint(number << 3 | 2, out);
    varint(u64::try_from(bytes.len()).unwrap_or_default(), out);
    out.extend_from_slice(bytes);
}

fn sticker_message(id: usize, sticker: &MockSticker) -> Vec<u8> {
    let mut out = Vec::new();
    varint(1 << 3, &mut out);
    varint(u64::try_from(id).unwrap_or_default(), &mut out);
    field(2, sticker.emoji.as_bytes(), &mut out);
    field(3, sticker.content_type.as_bytes(), &mut out);
    out
}

/// The pack's manifest: `Pack { title = 1, author = 2, cover = 3, stickers = 4 }`.
#[must_use]
pub fn manifest(pack: &MockPack) -> Vec<u8> {
    let mut out = Vec::new();
    field(1, pack.title.as_bytes(), &mut out);
    field(2, pack.author.as_bytes(), &mut out);
    if let Some(cover) = pack.stickers.first() {
        field(3, &sticker_message(0, cover), &mut out);
    }
    for (id, sticker) in pack.stickers.iter().enumerate() {
        field(4, &sticker_message(id, sticker), &mut out);
    }
    out
}

/// The bytes of the hex digits `hex`; a digit that isn't one counts as 0.
fn hex(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .unwrap_or_default()
        })
        .collect()
}

/// `plain` encrypted as Signal encrypts a pack's blob with the pack key `key` (hex): IV | AES-256-CBC | HMAC-SHA256.
/// The IV is made from `salt`, so each blob has its own and a run gives the same bytes.
#[must_use]
pub fn encrypt(key: &str, salt: u32, plain: &[u8]) -> Vec<u8> {
    let mut okm = [0_u8; 64];
    let _ = Hkdf::<Sha256>::new(Some(&[0_u8; 32]), &hex(key)).expand(b"Sticker Pack", &mut okm);
    let (cipher, mac_key) = okm.split_at(32);
    let digest = Sha256::digest(salt.to_be_bytes());
    let iv = &digest[..16];
    let mut out = iv.to_vec();
    if let Ok(encryptor) = cbc::Encryptor::<Aes256>::new_from_slices(cipher, iv) {
        let padded = plain.len() / 16 * 16 + 16;
        let mut buffer = plain.to_vec();
        buffer.resize(padded, 0);
        if let Ok(ciphertext) = encryptor.encrypt_padded::<Pkcs7>(&mut buffer, plain.len()) {
            out.extend_from_slice(ciphertext);
        }
    }
    if let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(mac_key) {
        mac.update(&out);
        out.extend_from_slice(&mac.finalize().into_bytes());
    }
    out
}

/// The CRC-32 of a PNG chunk's type and data.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(kind: [u8; 4], data: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or_default().to_be_bytes());
    let start = out.len();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// A made-up animated PNG (APNG), as a Signal pack's animated sticker is: `side` square, a frame of each of
/// `colours` (RGBA), a tenth of a second each, looping.
#[must_use]
pub fn animated_png(side: u32, colours: &[[u8; 4]]) -> Vec<u8> {
    use std::io::Write as _;
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&side.to_be_bytes());
    header.extend_from_slice(&side.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(*b"IHDR", &header, &mut out);
    let frames = u32::try_from(colours.len()).unwrap_or_default();
    png_chunk(
        *b"acTL",
        &[frames.to_be_bytes(), 0_u32.to_be_bytes()].concat(),
        &mut out,
    );
    let mut sequence = 0_u32;
    for (index, colour) in colours.iter().enumerate() {
        let mut control = sequence.to_be_bytes().to_vec();
        sequence += 1;
        for value in [side, side, 0, 0] {
            control.extend_from_slice(&value.to_be_bytes());
        }
        control.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
        png_chunk(*b"fcTL", &control, &mut out);
        let row: Vec<u8> = std::iter::once(0)
            .chain(std::iter::repeat_n(*colour, usize::try_from(side).unwrap_or_default()).flatten())
            .collect();
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        for _ in 0..side {
            let _ = zlib.write_all(&row);
        }
        let data = zlib.finish().unwrap_or_default();
        if index == 0 {
            png_chunk(*b"IDAT", &data, &mut out);
        } else {
            let mut frame = sequence.to_be_bytes().to_vec();
            sequence += 1;
            frame.extend_from_slice(&data);
            png_chunk(*b"fdAT", &frame, &mut out);
        }
    }
    png_chunk(*b"IEND", &[], &mut out);
    out
}
