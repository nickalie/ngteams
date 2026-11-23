//! Signal's sticker packs, as far as an import needs them: the pack a link names, its manifest and its stickers on
//! Signal's CDN (`docs/protocol/stickers.md`). The requests are made here, not in the app crates, as the AI APIs' are
//! ([`crate::ai_api`]).
//!
//! - **The link** (`https://signal.art/addstickers/#pack_id=<32 hex>&pack_key=<64 hex>`, or
//!   `sgnl://addstickers/?pack_id=…&pack_key=…`) is checked strictly before anything is sent ([`PackLink::parse`]).
//!   The pack key never leaves the app: it only decrypts what the CDN gives.
//! - **The keys** are HKDF-SHA256 of the pack key, with a zero salt and the info `Sticker Pack`: 64 bytes, the AES key
//!   first, then the HMAC key ([`PackKeys::derive`]).
//! - **Each blob** (the manifest, each sticker) is IV (16) | AES-256-CBC with PKCS#7 | HMAC-SHA256 of the IV and the
//!   ciphertext (32). The HMAC is checked first, in constant time; a blob that fails it is never decrypted.
//! - **The manifest** is a small protobuf: title, author, cover and the stickers (id, emoji, content type), read by
//!   hand ([`parse_manifest`]).
//! - **Where requests go**: only to [`CDN_ORIGIN`] (`cdn-ca.signal.org`, which a public CA signs, not
//!   `cdn.signal.org`). No credential, no Teams or own-server token, no cookie. The client is `https` only, follows no
//!   redirect, uses no proxy and tries each request once.
//! - **Nothing logs a request**, and each answer read is capped ([`MAX_MANIFEST`], the caller's size for a sticker).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use aes::Aes256;
use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeDecrypt as _, KeyIvInit as _};
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;

/// Signal's sticker CDN, with a certificate of a public CA.
pub const CDN_ORIGIN: &str = "https://cdn-ca.signal.org";
/// The most of a manifest read, in bytes: a pack of 200 stickers is a few kilobytes.
pub const MAX_MANIFEST: usize = 1 << 20;
/// The most stickers a pack's manifest gives: Signal's packs hold at most 200.
pub const MAX_PACK_STICKERS: usize = 200;
/// The most characters kept of a title, an author or an emoji.
const MAX_TEXT: usize = 200;
/// The info string of the keys' HKDF.
const KEY_INFO: &[u8] = b"Sticker Pack";
const IV_LEN: usize = 16;
const BLOCK_LEN: usize = 16;
const MAC_LEN: usize = 32;
/// How long a connection may take to open.
const CONNECT: Duration = Duration::from_secs(15);
/// How long one request may take.
const REQUEST: Duration = Duration::from_secs(60);

/// Why Signal's CDN or a pack gave nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignalError {
    /// The CDN has no pack (or no sticker) of that id: it answers 403.
    #[error("Signal has no sticker pack of that id")]
    NoSuchPack,
    /// A blob's HMAC does not match: the key is wrong or the blob was changed.
    #[error("the pack's data failed its integrity check")]
    BadMac,
    /// A blob too short to hold an IV, a block and an HMAC, or whose padding is wrong after a good HMAC.
    #[error("the pack's data is not of Signal's form")]
    BadBlob,
    /// The manifest is not the protobuf Signal writes.
    #[error("the pack's manifest cannot be read")]
    BadManifest,
    /// The answer is larger than the caller takes.
    #[error("the answer is too large")]
    TooLarge,
    /// No answer in time.
    #[error("Signal did not answer in time")]
    TimedOut,
    /// No connection to Signal.
    #[error("Signal cannot be reached")]
    Unreachable,
    /// An answer that is not what the CDN gives (another status, a redirect).
    #[error("Signal's answer cannot be read")]
    BadAnswer,
}

/// A pack that a link names: its id (32 hex digits, lower case) and its key (32 bytes).
#[derive(Clone, PartialEq, Eq)]
pub struct PackLink {
    id: String,
    key: [u8; 32],
}

impl fmt::Debug for PackLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PackLink")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// The bytes of `hex` (an even count of hex digits, either case).
fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The query or fragment of a link to a pack: `signal.art/addstickers/#…` (with or without `https://`), or
/// `sgnl://addstickers/?…`.
fn link_params(link: &str) -> Option<&str> {
    let lower = link.to_ascii_lowercase();
    if lower.starts_with("sgnl://") {
        let rest = link.get("sgnl://".len()..)?;
        let (path, query) = rest.split_once('?')?;
        if !path.eq_ignore_ascii_case("addstickers") && !path.eq_ignore_ascii_case("addstickers/") {
            return None;
        }
        return Some(query.split('#').next().unwrap_or_default());
    }
    let rest = match ["https://", "http://"]
        .iter()
        .find(|scheme| lower.starts_with(**scheme))
    {
        Some(scheme) => link.get(scheme.len()..)?,
        None => link,
    };
    let (before, fragment) = rest.split_once('#')?;
    let (host, path) = before.split_once('/').unwrap_or((before, ""));
    let host = host.to_ascii_lowercase();
    let path = path.trim_end_matches('/');
    let host_ok = host == "signal.art" || host == "www.signal.art";
    (host_ok && path.eq_ignore_ascii_case("addstickers")).then_some(fragment)
}

impl PackLink {
    /// The pack `link` names (see the module docs), or `None`: another host, a missing or repeated `pack_id` or
    /// `pack_key`, an id that is not 32 hex digits, or a key that is not 64.
    #[must_use]
    pub fn parse(link: &str) -> Option<Self> {
        let params = link_params(link.trim())?;
        let (mut id, mut key) = (None, None);
        for pair in params.split('&').filter(|pair| !pair.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let slot = match name {
                "pack_id" => &mut id,
                "pack_key" => &mut key,
                _ => continue,
            };
            if slot.replace(value).is_some() {
                return None;
            }
        }
        let (id, key) = (id?, key?);
        if id.len() != 32 || key.len() != 64 {
            return None;
        }
        hex_bytes(id)?;
        let key: [u8; 32] = hex_bytes(key)?.try_into().ok()?;
        Some(Self {
            id: id.to_ascii_lowercase(),
            key,
        })
    }

    /// The pack's id, as the CDN's paths take it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The keys of the pack's blobs.
    #[must_use]
    pub fn keys(&self) -> PackKeys {
        PackKeys::derive(&self.key)
    }
}

/// The keys of a pack's blobs, from its pack key.
#[derive(Clone, PartialEq, Eq)]
pub struct PackKeys {
    cipher: [u8; 32],
    mac: [u8; 32],
}

impl fmt::Debug for PackKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PackKeys(<keys>)")
    }
}

impl PackKeys {
    /// The AES and HMAC keys of `pack_key`: HKDF-SHA256 with a zero salt and the info `Sticker Pack`, 64 bytes.
    #[must_use]
    pub fn derive(pack_key: &[u8; 32]) -> Self {
        let mut okm = [0_u8; 64];
        // 64 bytes are far below HKDF-SHA256's limit of 8160, so the expand cannot fail
        let _ = Hkdf::<Sha256>::new(Some(&[0_u8; 32]), pack_key).expand(KEY_INFO, &mut okm);
        let (mut cipher, mut mac) = ([0_u8; 32], [0_u8; 32]);
        cipher.copy_from_slice(&okm[..32]);
        mac.copy_from_slice(&okm[32..]);
        Self { cipher, mac }
    }

    /// The AES key.
    #[must_use]
    pub fn cipher_key(&self) -> &[u8; 32] {
        &self.cipher
    }

    /// The HMAC key.
    #[must_use]
    pub fn mac_key(&self) -> &[u8; 32] {
        &self.mac
    }

    /// The plain bytes of `blob` (see the module docs): its HMAC checked first, then decrypted.
    ///
    /// # Errors
    /// [`SignalError::BadBlob`] for a blob of the wrong size or padding, [`SignalError::BadMac`] for one whose HMAC
    /// does not match.
    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>, SignalError> {
        let Some(body_len) = blob.len().checked_sub(MAC_LEN) else {
            return Err(SignalError::BadBlob);
        };
        if body_len < IV_LEN + BLOCK_LEN || !(body_len - IV_LEN).is_multiple_of(BLOCK_LEN) {
            return Err(SignalError::BadBlob);
        }
        let (body, tag) = blob.split_at(body_len);
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.mac).map_err(|_| SignalError::BadBlob)?;
        mac.update(body);
        mac.verify_slice(tag).map_err(|_| SignalError::BadMac)?;
        let (iv, ciphertext) = body.split_at(IV_LEN);
        let mut plain = ciphertext.to_vec();
        let decryptor =
            cbc::Decryptor::<Aes256>::new_from_slices(&self.cipher, iv).map_err(|_| SignalError::BadBlob)?;
        let len = decryptor
            .decrypt_padded::<Pkcs7>(&mut plain)
            .map_err(|_| SignalError::BadBlob)?
            .len();
        plain.truncate(len);
        Ok(plain)
    }
}

/// A pack's manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub title: String,
    pub author: String,
    pub cover: Option<ManifestSticker>,
    /// At most [`MAX_PACK_STICKERS`], each id once, in the manifest's order.
    pub stickers: Vec<ManifestSticker>,
}

/// A sticker of a manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManifestSticker {
    pub id: u32,
    pub emoji: String,
    /// `image/png`, `image/webp`, `image/apng`, or empty in an older pack; the bytes tell the format.
    pub content_type: String,
}

/// A protobuf field's value, as the manifest reads it.
enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// Reads the fields of one protobuf message.
struct Fields<'a> {
    rest: &'a [u8],
}

impl<'a> Fields<'a> {
    fn varint(&mut self) -> Result<u64, SignalError> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.rest.split_first().ok_or(SignalError::BadManifest)?;
            self.rest = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(SignalError::BadManifest)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], SignalError> {
        if len > self.rest.len() {
            return Err(SignalError::BadManifest);
        }
        let (taken, rest) = self.rest.split_at(len);
        self.rest = rest;
        Ok(taken)
    }

    /// The next field's number and value, or `None` at the end.
    fn next_field(&mut self) -> Result<Option<(u64, Value<'a>)>, SignalError> {
        if self.rest.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => self.take(8).map(|_| Value::Fixed)?,
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| SignalError::BadManifest)?;
                Value::Bytes(self.take(len)?)
            }
            5 => self.take(4).map(|_| Value::Fixed)?,
            _ => return Err(SignalError::BadManifest),
        };
        Ok(Some((key >> 3, value)))
    }
}

/// A text field: UTF-8, its control characters dropped, at most [`MAX_TEXT`] characters.
fn text(bytes: &[u8]) -> Result<String, SignalError> {
    let text = std::str::from_utf8(bytes).map_err(|_| SignalError::BadManifest)?;
    Ok(text.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect())
}

fn parse_sticker(bytes: &[u8]) -> Result<ManifestSticker, SignalError> {
    let mut fields = Fields { rest: bytes };
    let mut sticker = ManifestSticker::default();
    while let Some((number, value)) = fields.next_field()? {
        match (number, value) {
            (1, Value::Varint(id)) => sticker.id = u32::try_from(id).map_err(|_| SignalError::BadManifest)?,
            (2, Value::Bytes(emoji)) => sticker.emoji = text(emoji)?,
            (3, Value::Bytes(kind)) => sticker.content_type = text(kind)?,
            (1..=3, _) => return Err(SignalError::BadManifest),
            _ => {}
        }
    }
    Ok(sticker)
}

/// The manifest `bytes` hold: `Pack { title = 1, author = 2, cover = 3, stickers = 4 (repeated) }`, each sticker
/// `Sticker { id = 1, emoji = 2, contentType = 3 }`. Unknown fields are passed over.
///
/// # Errors
/// [`SignalError::BadManifest`]: the bytes are not that protobuf, or a field has the wrong type.
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, SignalError> {
    let mut fields = Fields { rest: bytes };
    let mut manifest = Manifest::default();
    while let Some((number, value)) = fields.next_field()? {
        match (number, value) {
            (1, Value::Bytes(title)) => manifest.title = text(title)?,
            (2, Value::Bytes(author)) => manifest.author = text(author)?,
            (3, Value::Bytes(cover)) => manifest.cover = Some(parse_sticker(cover)?),
            (4, Value::Bytes(sticker)) => {
                let sticker = parse_sticker(sticker)?;
                let known = manifest.stickers.iter().any(|held| held.id == sticker.id);
                if !known && manifest.stickers.len() < MAX_PACK_STICKERS {
                    manifest.stickers.push(sticker);
                }
            }
            (1..=4, _) => return Err(SignalError::BadManifest),
            _ => {}
        }
    }
    Ok(manifest)
}

/// Signal's sticker CDN (see the module docs).
#[derive(Clone)]
pub struct SignalCdn {
    origin: String,
    http: reqwest::Client,
    /// The runtime that runs the requests, while the thread that asks blocks.
    runtime: Arc<tokio::runtime::Runtime>,
}

impl fmt::Debug for SignalCdn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignalCdn")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

impl SignalCdn {
    /// The CDN at [`CDN_ORIGIN`].
    ///
    /// # Errors
    /// The client could not be made.
    pub fn new() -> Result<Self, SignalError> {
        Self::build(CDN_ORIGIN, true)
    }

    /// The CDN of a local mock at `origin` (`http://127.0.0.1:<port>`). Tests only (`test-support`).
    ///
    /// # Errors
    /// `origin` is not `http` on a loopback host with a port, or as [`Self::new`].
    #[cfg(feature = "test-support")]
    pub fn with_mock_origin(origin: &str) -> Result<Self, SignalError> {
        let url = reqwest::Url::parse(origin).map_err(|_| SignalError::Unreachable)?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
        if url.scheme() != "http" || !loopback || url.port().is_none() || url.path() != "/" {
            return Err(SignalError::Unreachable);
        }
        Self::build(origin.trim_end_matches('/'), false)
    }

    fn build(origin: &str, https_only: bool) -> Result<Self, SignalError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| SignalError::Unreachable)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(https_only)
            .connect_timeout(CONNECT)
            .timeout(REQUEST)
            .user_agent(concat!("ngteams/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| SignalError::Unreachable)?;
        Ok(Self {
            origin: origin.to_owned(),
            http,
            runtime: Arc::new(runtime),
        })
    }

    /// The manifest of the pack `link` names, decrypted.
    ///
    /// # Errors
    /// See [`SignalError`]; [`SignalError::NoSuchPack`] when the CDN answers 403 or 404.
    pub fn manifest(&self, link: &PackLink) -> Result<Manifest, SignalError> {
        let url = format!("{}/stickers/{}/manifest.proto", self.origin, link.id());
        let blob = self.fetch(&url, MAX_MANIFEST + IV_LEN + BLOCK_LEN + MAC_LEN)?;
        parse_manifest(&link.keys().decrypt(&blob)?)
    }

    /// The picture of sticker `id` of the pack `link` names, decrypted, at most `most` bytes.
    ///
    /// # Errors
    /// See [`SignalError`]; [`SignalError::TooLarge`] past `most`.
    pub fn sticker(&self, link: &PackLink, id: u32, most: usize) -> Result<Vec<u8>, SignalError> {
        let url = format!("{}/stickers/{}/full/{id}", self.origin, link.id());
        let blob = self.fetch(&url, most.saturating_add(IV_LEN + BLOCK_LEN + MAC_LEN))?;
        let plain = link.keys().decrypt(&blob)?;
        if plain.len() > most {
            return Err(SignalError::TooLarge);
        }
        Ok(plain)
    }

    fn fetch(&self, url: &str, most: usize) -> Result<Vec<u8>, SignalError> {
        self.runtime.block_on(async {
            let response = self.http.get(url).send().await.map_err(|error| category(&error))?;
            match response.status().as_u16() {
                200 => read_capped(response, most).await,
                403 | 404 => Err(SignalError::NoSuchPack),
                _ => Err(SignalError::BadAnswer),
            }
        })
    }
}

/// A `reqwest` error as a category; its text is never shown.
fn category(error: &reqwest::Error) -> SignalError {
    if error.is_timeout() {
        SignalError::TimedOut
    } else if error.is_connect() || error.is_request() {
        SignalError::Unreachable
    } else {
        SignalError::BadAnswer
    }
}

/// The body of `response`, at most `most` bytes.
async fn read_capped(mut response: reqwest::Response, most: usize) -> Result<Vec<u8>, SignalError> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(most).unwrap_or(u64::MAX))
    {
        return Err(SignalError::TooLarge);
    }
    let mut body = Vec::new();
    // `chunk` gives each failure to read the body as a decode error: the connection is lost, or timed out.
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            SignalError::TimedOut
        } else {
            SignalError::Unreachable
        }
    })? {
        if body.len() + chunk.len() > most {
            return Err(SignalError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
#[path = "signal_tests.rs"]
mod tests;
