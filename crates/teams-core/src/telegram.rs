//! Telegram's Bot API, as far as a sticker pack's import needs it: a pack (`getStickerSet`), a file's path
//! (`getFile`), and the file's bytes. The user gives the bot token (from `@BotFather`); the app crates never read it
//! out of its `Secret`, so the requests are made here, as the AI APIs' are ([`crate::ai_api`]).
//!
//! - **Where the token goes**: only to `https://api.telegram.org`, and only in the path, as the Bot API takes it
//!   (`/bot<token>/<method>`, `/file/bot<token>/<path>`). The client is `https` only, follows no redirect and uses no
//!   proxy, so nothing else ever gets it. No Teams or own-server token goes here.
//! - **Nothing logs a request**: no URL, no answer, no token. A failure is a fixed category ([`BotError`]); a
//!   `reqwest` error, whose text holds the URL and so the token, is never shown.
//! - **What is read is capped**: an answer at [`MAX_ANSWER`], a file at what the caller asks for. A pack's name, a
//!   token and a file's path are checked before they go in a URL.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use teams_auth::secrets::Secret;

/// The Bot API's origin.
pub const BOT_API_ORIGIN: &str = "https://api.telegram.org";
/// The most of an API answer read, in bytes: a pack of 120 stickers is about 100 KB.
pub const MAX_ANSWER: usize = 4 << 20;
/// The longest name of a pack, as Telegram takes it.
pub const MAX_SET_NAME: usize = 64;
/// How long a connection may take to open.
const CONNECT: Duration = Duration::from_secs(15);
/// How long one request may take.
const REQUEST: Duration = Duration::from_secs(60);

/// Why the Bot API gave nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BotError {
    /// The token is not of the form `@BotFather` gives (`<digits>:<letters>`).
    #[error("the bot token is not of the form @BotFather gives")]
    TokenForm,
    /// The name is not a pack's name (letters, digits and `_`, at most [`MAX_SET_NAME`]).
    #[error("that is not the name of a sticker pack")]
    NameForm,
    /// Telegram refused the token.
    #[error("Telegram refused the bot token")]
    BadToken,
    /// Telegram has no pack of that name.
    #[error("Telegram has no sticker pack of that name")]
    NoSuchSet,
    /// Telegram limits the rate of the requests.
    #[error("Telegram asks to wait before the next request")]
    RateLimited,
    /// The file is larger than the caller takes.
    #[error("the file is too large")]
    TooLarge,
    /// No answer in time.
    #[error("Telegram did not answer in time")]
    TimedOut,
    /// No connection to Telegram.
    #[error("Telegram cannot be reached")]
    Unreachable,
    /// An answer that is not what the Bot API documents, or another failure.
    #[error("Telegram's answer cannot be read")]
    BadAnswer,
}

/// A sticker pack, as `getStickerSet` gives it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct StickerSet {
    pub name: String,
    pub title: String,
    pub stickers: Vec<SetSticker>,
}

/// A sticker of a pack.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SetSticker {
    pub file_id: String,
    /// An animated (`.tgs`) sticker.
    #[serde(default)]
    pub is_animated: bool,
    /// A video (`.webm`) sticker.
    #[serde(default)]
    pub is_video: bool,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Deserialize)]
struct Answer<T> {
    ok: bool,
    result: Option<T>,
    #[serde(default)]
    error_code: Option<u16>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct FileInfo {
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    file_size: Option<u64>,
}

/// The Bot API with one bot's token (see the module docs).
#[derive(Clone)]
pub struct BotApi {
    origin: String,
    token: Secret,
    http: reqwest::Client,
    /// The runtime that runs the requests, while the thread that asks blocks.
    runtime: Arc<tokio::runtime::Runtime>,
}

impl fmt::Debug for BotApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BotApi")
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

/// Whether `token` has the form `@BotFather` gives: the bot's number, `:`, then letters, digits, `_` and `-`.
#[must_use]
pub fn token_form(token: &str) -> bool {
    let Some((bot, key)) = token.split_once(':') else {
        return false;
    };
    (1..=20).contains(&bot.len())
        && bot.bytes().all(|byte| byte.is_ascii_digit())
        && (20..=100).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Whether `name` is a pack's name: letters, digits and `_`, at most [`MAX_SET_NAME`].
#[must_use]
pub fn set_name_form(name: &str) -> bool {
    (1..=MAX_SET_NAME).contains(&name.len()) && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Whether `path` is a file path the Bot API gives (`stickers/file_1.webp`): no `..`, no query, nothing to escape.
fn file_path_form(path: &str) -> bool {
    (1..=256).contains(&path.len())
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
}

impl BotApi {
    /// The Bot API at [`BOT_API_ORIGIN`], asked with `token`.
    ///
    /// # Errors
    /// The token is not of `@BotFather`'s form, or the client could not be made.
    pub fn new(token: Secret) -> Result<Self, BotError> {
        Self::build(BOT_API_ORIGIN, token, true)
    }

    /// The Bot API of a local mock at `origin` (`http://127.0.0.1:<port>`). Tests only (`test-support`).
    ///
    /// # Errors
    /// `origin` is not `http` on a loopback host with a port, or as [`Self::new`].
    #[cfg(feature = "test-support")]
    pub fn with_mock_origin(token: Secret, origin: &str) -> Result<Self, BotError> {
        let url = url_of(origin)?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"));
        if url.scheme() != "http" || !loopback || url.port().is_none() || url.path() != "/" {
            return Err(BotError::Unreachable);
        }
        Self::build(origin.trim_end_matches('/'), token, false)
    }

    fn build(origin: &str, token: Secret, https_only: bool) -> Result<Self, BotError> {
        if !token_form(token.expose()) {
            return Err(BotError::TokenForm);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| BotError::Unreachable)?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(https_only)
            .connect_timeout(CONNECT)
            .timeout(REQUEST)
            .user_agent(concat!("ngteams/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| BotError::Unreachable)?;
        Ok(Self {
            origin: origin.to_owned(),
            token,
            http,
            runtime: Arc::new(runtime),
        })
    }

    /// The pack called `name`.
    ///
    /// # Errors
    /// See [`BotError`].
    pub fn sticker_set(&self, name: &str) -> Result<StickerSet, BotError> {
        if !set_name_form(name) {
            return Err(BotError::NameForm);
        }
        let url = format!("{}/bot{}/getStickerSet?name={name}", self.origin, self.token.expose());
        self.runtime.block_on(self.call(&url))
    }

    /// The bytes of file `file_id`, at most `most`.
    ///
    /// # Errors
    /// See [`BotError`]; [`BotError::TooLarge`] past `most`.
    pub fn file(&self, file_id: &str, most: usize) -> Result<Vec<u8>, BotError> {
        if file_id.is_empty()
            || file_id.len() > 256
            || !file_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(BotError::BadAnswer);
        }
        self.runtime.block_on(async {
            let url = format!("{}/bot{}/getFile?file_id={file_id}", self.origin, self.token.expose());
            let info: FileInfo = self.call(&url).await?;
            if info
                .file_size
                .is_some_and(|size| size > u64::try_from(most).unwrap_or(u64::MAX))
            {
                return Err(BotError::TooLarge);
            }
            let path = info
                .file_path
                .filter(|path| file_path_form(path))
                .ok_or(BotError::BadAnswer)?;
            let url = format!("{}/file/bot{}/{path}", self.origin, self.token.expose());
            let response = self.http.get(&url).send().await.map_err(|error| category(&error))?;
            match response.status().as_u16() {
                200 => read_capped(response, most).await,
                401 | 404 => Err(BotError::BadToken),
                429 => Err(BotError::RateLimited),
                _ => Err(BotError::BadAnswer),
            }
        })
    }

    async fn call<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, BotError> {
        let response = self.http.get(url).send().await.map_err(|error| category(&error))?;
        let status = response.status().as_u16();
        let body = read_capped(response, MAX_ANSWER).await.map_err(|error| match error {
            BotError::TooLarge => BotError::BadAnswer,
            other => other,
        })?;
        let answer: Answer<T> = serde_json::from_slice(&body).map_err(|_| match status {
            401 | 404 => BotError::BadToken,
            429 => BotError::RateLimited,
            _ => BotError::BadAnswer,
        })?;
        if answer.ok {
            return answer.result.ok_or(BotError::BadAnswer);
        }
        let description = answer.description.unwrap_or_default();
        Err(match answer.error_code.unwrap_or(status) {
            401 | 404 => BotError::BadToken,
            429 => BotError::RateLimited,
            400 if description.contains("STICKERSET_INVALID") => BotError::NoSuchSet,
            _ => BotError::BadAnswer,
        })
    }
}

#[cfg(feature = "test-support")]
fn url_of(origin: &str) -> Result<reqwest::Url, BotError> {
    reqwest::Url::parse(origin).map_err(|_| BotError::Unreachable)
}

/// A `reqwest` error as a category: its text holds the URL, and so the token, and is never shown.
fn category(error: &reqwest::Error) -> BotError {
    if error.is_timeout() {
        BotError::TimedOut
    } else if error.is_connect() {
        BotError::Unreachable
    } else {
        BotError::BadAnswer
    }
}

/// The body of `response`, at most `most` bytes.
async fn read_capped(mut response: reqwest::Response, most: usize) -> Result<Vec<u8>, BotError> {
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(most).unwrap_or(u64::MAX))
    {
        return Err(BotError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| category(&error))? {
        if body.len() + chunk.len() > most {
            return Err(BotError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
#[path = "telegram_tests.rs"]
mod tests;
