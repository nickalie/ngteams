//! A Signal sticker pack imported from its link (`signal.art/addstickers/#pack_id=…&pack_key=…`, or
//! `sgnl://addstickers/?pack_id=…&pack_key=…`), from Signal's CDN with no account ([`teams_core::signal`],
//! `docs/protocol/stickers.md`).
//!
//! - **The link** is checked before anything is sent; its key only decrypts what the CDN gives, and is never kept,
//!   drawn or logged.
//! - **The pack**: its title names it; each sticker's emoji is its label. A still sticker (PNG, WebP) is kept as a PNG,
//!   an animated one (APNG) as a GIF, as a file's are ([`super::import`]).
//! - **What stops it**: a link that names no pack, a key that doesn't open the manifest, or a CDN that can't be
//!   reached or gives an unexpected answer (a redirect, or a status other than 200 or 403); then nothing is kept. A
//!   sticker that fails its integrity check, or that the CDN doesn't hold (403), is skipped and said to be one.

use std::sync::Arc;

use gpui::{App, Global};
use teams_core::signal::{Manifest, PackLink, SignalCdn, SignalError};

use super::import::{self, ImportError, Imported, MAX_FILE, Progress, Skip, Source};

/// What makes the CDN's client: an app global, Signal's own ([`SignalCdn::new`]) unless a test gives its mock's.
#[derive(Clone)]
pub struct SignalCdns(Arc<dyn Fn() -> Result<SignalCdn, SignalError> + Send + Sync>);

impl Global for SignalCdns {}

impl Default for SignalCdns {
    fn default() -> Self {
        Self::new(Arc::new(SignalCdn::new))
    }
}

impl SignalCdns {
    #[must_use]
    pub fn new(make: Arc<dyn Fn() -> Result<SignalCdn, SignalError> + Send + Sync>) -> Self {
        Self(make)
    }

    /// The app's.
    pub fn of(cx: &mut App) -> Self {
        cx.default_global::<Self>().clone()
    }

    fn make(&self) -> Result<SignalCdn, SignalError> {
        (self.0)()
    }
}

/// Whether `link` names a Signal pack.
#[must_use]
pub fn is_pack_link(link: &str) -> bool {
    PackLink::parse(link).is_some()
}

/// Why the CDN gave no manifest, as an import's failure.
fn failed(error: SignalError) -> ImportError {
    match error {
        SignalError::NoSuchPack => ImportError::SignalNoSuchPack,
        SignalError::BadMac => ImportError::SignalBadKey,
        SignalError::TimedOut | SignalError::Unreachable => ImportError::SignalDown,
        SignalError::BadBlob | SignalError::BadManifest | SignalError::TooLarge | SignalError::BadAnswer => {
            ImportError::SignalFailed
        }
    }
}

/// A linked pack's stickers, downloaded one by one as the import reads them.
struct Linked {
    cdn: SignalCdn,
    link: PackLink,
    manifest: Manifest,
    /// The failure that ends the import: the CDN can't be reached or gives an unexpected answer.
    stopped: Option<ImportError>,
}

impl Linked {
    fn emoji(&self, index: usize) -> &str {
        self.manifest
            .stickers
            .get(index)
            .map_or("", |sticker| sticker.emoji.as_str())
    }
}

impl Source for Linked {
    fn name(&self) -> String {
        self.manifest.title.clone()
    }

    fn len(&self) -> usize {
        self.manifest.stickers.len().min(import::MAX_ENTRIES)
    }

    fn file_name(&self, index: usize) -> String {
        format!("Sticker {} {}", index + 1, self.emoji(index))
            .trim_end()
            .to_owned()
    }

    fn label(&self, index: usize) -> Option<String> {
        Some(self.emoji(index).to_owned()).filter(|emoji| !emoji.is_empty())
    }

    fn read(&mut self, index: usize) -> Result<Vec<u8>, Skip> {
        let id = self.manifest.stickers.get(index).ok_or(Skip::Unreadable)?.id;
        let most = usize::try_from(MAX_FILE).unwrap_or(usize::MAX);
        self.cdn.sticker(&self.link, id, most).map_err(|error| match error {
            SignalError::TooLarge => Skip::TooLarge,
            SignalError::BadMac | SignalError::BadBlob => Skip::Tampered,
            SignalError::TimedOut | SignalError::Unreachable | SignalError::BadAnswer => {
                self.stopped = Some(failed(error));
                Skip::NotDownloaded
            }
            SignalError::NoSuchPack | SignalError::BadManifest => Skip::NotDownloaded,
        })
    }

    fn stopped(&mut self) -> Option<ImportError> {
        self.stopped.take()
    }
}

/// Imports the pack `link` names into `dir`, from the CDN `cdns` makes, telling `progress` after each sticker.
///
/// # Errors
/// Nothing was imported ([`ImportError`]); nothing is left behind.
pub fn import_link(
    link: &str,
    cdns: &SignalCdns,
    dir: &std::path::Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Imported, ImportError> {
    let link = PackLink::parse(link).ok_or(ImportError::SignalBadLink)?;
    let cdn = cdns.make().map_err(failed)?;
    let manifest = cdn.manifest(&link).map_err(failed)?;
    import::import_from(
        &mut Linked {
            cdn,
            link,
            manifest,
            stopped: None,
        },
        dir,
        progress,
    )
}
