//! A Telegram sticker pack imported from its link (`t.me/addstickers/<name>`, `tg://addstickers?set=<name>`), through
//! Telegram's Bot API with the user's bot token ([`teams_core::telegram`], `docs/protocol/stickers.md`).
//!
//! - **The token** is kept only in the app's keyring ([`BotToken`], under [`TOKEN_NAME`]); a lab alias and the read-only
//!   mode keep it in the memory. It is read on the import's thread, handed to the Bot API's client, and never drawn,
//!   logged or written to a file.
//! - **The pack**: its title names it; each sticker's emoji is its label. A still sticker (`.webp`) and an animated one
//!   (`.tgs`) are made stickers as a file's are ([`super::import`]); a video sticker (`.webm`) is skipped, said to be
//!   one, and not downloaded.

use std::sync::Arc;

use gpui::{App, Global};
use teams_auth::secrets::Secret;
use teams_core::telegram::{BotApi, BotError, SetSticker, StickerSet, set_name_form};

use super::import::{self, ImportError, Imported, MAX_FILE, Progress, Skip, Source};
use crate::local_ai::KeyVault;
use crate::local_ai::providers::MemoryKeys;

/// The name the token is kept under in the vault.
pub const TOKEN_NAME: &str = "telegram-bot";

/// The app's Telegram bot token: an app global, the memory's until the app gives its keyring ([`BotToken::new`]).
#[derive(Clone)]
pub struct BotToken(Arc<dyn KeyVault>);

impl Global for BotToken {}

impl std::fmt::Debug for BotToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BotToken(<vault>)")
    }
}

impl Default for BotToken {
    fn default() -> Self {
        Self::new(Arc::new(MemoryKeys::default()))
    }
}

impl BotToken {
    #[must_use]
    pub fn new(vault: Arc<dyn KeyVault>) -> Self {
        Self(vault)
    }

    /// The app's.
    pub fn of(cx: &mut App) -> Self {
        cx.default_global::<Self>().clone()
    }

    /// The token, if one is kept.
    ///
    /// # Errors
    /// The keyring cannot be read.
    pub fn get(&self) -> Result<Option<Secret>, String> {
        self.0.get(TOKEN_NAME)
    }

    /// Keeps `token`, in place of the one kept.
    ///
    /// # Errors
    /// The keyring cannot be written.
    pub fn put(&self, token: &Secret) -> Result<(), String> {
        self.0.put(TOKEN_NAME, token)
    }

    /// Forgets the token.
    ///
    /// # Errors
    /// The keyring cannot be written.
    pub fn delete(&self) -> Result<(), String> {
        self.0.delete(TOKEN_NAME)
    }
}

/// What makes the Bot API's client from a token: an app global, Telegram's own ([`BotApi::new`]) unless a test gives
/// its mock's.
#[derive(Clone)]
pub struct BotApis(Arc<dyn Fn(Secret) -> Result<BotApi, BotError> + Send + Sync>);

impl Global for BotApis {}

impl Default for BotApis {
    fn default() -> Self {
        Self::new(Arc::new(BotApi::new))
    }
}

impl BotApis {
    #[must_use]
    pub fn new(make: Arc<dyn Fn(Secret) -> Result<BotApi, BotError> + Send + Sync>) -> Self {
        Self(make)
    }

    /// The app's.
    pub fn of(cx: &mut App) -> Self {
        cx.default_global::<Self>().clone()
    }

    fn make(&self, token: Secret) -> Result<BotApi, BotError> {
        (self.0)(token)
    }
}

/// The pack's name a link names: `t.me/addstickers/<name>` (also `telegram.me`, with or without `https://`),
/// `tg://addstickers?set=<name>`, the same for an emoji pack (`addemoji`), or the bare name.
#[must_use]
pub fn set_name(link: &str) -> Option<String> {
    let link = link.trim();
    let lower = link.to_ascii_lowercase();
    let name = if let Some(query) = ["tg://addstickers?", "tg://addemoji?"]
        .iter()
        .find_map(|prefix| lower.starts_with(prefix).then(|| &link[prefix.len()..]))
    {
        query.split('&').find_map(|pair| pair.strip_prefix("set="))?
    } else {
        let rest = ["https://", "http://"]
            .iter()
            .find_map(|scheme| lower.starts_with(scheme).then(|| &link[scheme.len()..]))
            .unwrap_or(link);
        match rest.split_once('/') {
            Some((host, path)) => {
                let host = host.to_ascii_lowercase();
                let host = host.strip_prefix("www.").unwrap_or(&host);
                if !matches!(host, "t.me" | "telegram.me" | "telegram.dog") {
                    return None;
                }
                let path = path.split(['?', '#']).next().unwrap_or_default().trim_end_matches('/');
                let (kind, name) = path.split_once('/')?;
                if !matches!(kind, "addstickers" | "addemoji") {
                    return None;
                }
                name
            }
            None => rest,
        }
    };
    set_name_form(name).then(|| name.to_owned())
}

/// Why the Bot API gave nothing, as an import's failure.
fn failed(error: BotError) -> ImportError {
    match error {
        BotError::TokenForm | BotError::BadToken => ImportError::BadToken,
        BotError::NameForm => ImportError::BadLink,
        BotError::NoSuchSet => ImportError::NoSuchPack,
        BotError::RateLimited => ImportError::TelegramBusy,
        BotError::TimedOut | BotError::Unreachable => ImportError::TelegramDown,
        BotError::TooLarge | BotError::BadAnswer => ImportError::TelegramFailed,
    }
}

/// A linked pack's stickers, downloaded one by one as the import reads them.
struct Linked {
    api: BotApi,
    set: StickerSet,
    /// The failure that ends the import: Telegram refuses, asks to wait or can't be reached.
    stopped: Option<ImportError>,
}

impl Linked {
    fn sticker(&self, index: usize) -> Option<&SetSticker> {
        self.set.stickers.get(index)
    }
}

impl Source for Linked {
    fn name(&self) -> String {
        self.set.title.clone()
    }

    fn len(&self) -> usize {
        self.set.stickers.len().min(import::MAX_ENTRIES)
    }

    fn file_name(&self, index: usize) -> String {
        let emoji = self
            .sticker(index)
            .and_then(|sticker| sticker.emoji.as_deref())
            .unwrap_or_default();
        format!("Sticker {} {emoji}", index + 1).trim_end().to_owned()
    }

    fn label(&self, index: usize) -> Option<String> {
        self.sticker(index)?.emoji.clone()
    }

    fn read(&mut self, index: usize) -> Result<Vec<u8>, Skip> {
        let sticker = self.sticker(index).ok_or(Skip::Unreadable)?;
        if sticker.is_video {
            return Err(Skip::Video);
        }
        if sticker.file_size.is_some_and(|size| size > MAX_FILE) {
            return Err(Skip::TooLarge);
        }
        let most = usize::try_from(MAX_FILE).unwrap_or(usize::MAX);
        self.api.file(&sticker.file_id, most).map_err(|error| match error {
            BotError::TooLarge => Skip::TooLarge,
            BotError::BadToken | BotError::RateLimited | BotError::TimedOut | BotError::Unreachable => {
                self.stopped = Some(failed(error));
                Skip::NotDownloaded
            }
            _ => Skip::NotDownloaded,
        })
    }

    fn stopped(&mut self) -> Option<ImportError> {
        self.stopped.take()
    }
}

/// Imports the pack `link` names into `dir`, asking the Bot API made by `apis` with the token `tokens` keeps, telling
/// `progress` after each sticker.
///
/// # Errors
/// Nothing was imported ([`ImportError`]); nothing is left behind.
pub fn import_link(
    link: &str,
    tokens: &BotToken,
    apis: &BotApis,
    dir: &std::path::Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Imported, ImportError> {
    let name = set_name(link).ok_or(ImportError::BadLink)?;
    let token = tokens
        .get()
        .map_err(|_| ImportError::TokenUnreadable)?
        .ok_or(ImportError::NoToken)?;
    let api = apis.make(token).map_err(failed)?;
    let set = api.sticker_set(&name).map_err(failed)?;
    import::import_from(
        &mut Linked {
            api,
            set,
            stopped: None,
        },
        dir,
        progress,
    )
}
