//! Pictures messages show, read when a view shows one ([`InlinePicture`]): a picture pasted into a message
//! ([`teams_proto::MessagePicture`]: a `GET` of the picture's view from the account's AMS base, the way the web client
//! reads it), a GIF picked from Teams' GIF picker ([`teams_proto::GiphyPicture`]: a `GET` of the GIF from Giphy's
//! media host, no token), or a link preview's picture ([`teams_proto::LinkPreviewPicture`]: a `GET` of the async
//! gateway's thumbnail of it), as [`crate::emoticons`] reads reactions' animations; and an app's tile, the picture its
//! definition's `largeImageUrl` names ([`teams_proto::AppPicture`]: a `GET` from Microsoft's CDN, no token), which
//! Activity draws for the app's items. Each kind is kept apart, by its own key.
//!
//! - **Kept for the run**, in memory, up to [`KEPT_BYTES`] of them (the oldest read let go first: a view keeps what it
//!   decoded, so a picture let go is read again only when a view asks for it anew); a view the service has none of (an
//!   answer that isn't a success, or isn't a picture) is kept as missing. A read that failed on the way isn't kept and
//!   is answered [`EngineError::PictureUnread`], so the next ask reads again.
//! - **Cached across runs** on disk ([`Disk`]: the account's [`PictureCache`] beside its store, up to
//!   [`PICTURE_CACHE_BYTES`], the least recently used let go first): a picture the run doesn't know yet is looked for
//!   there before it is read, and one read is kept there, so a later run shows it without reading it again. An AMS
//!   object's view, a GIF and an app's tile at its version never change, so they are kept for as long as there is
//!   room; a missing one isn't kept.
//!   The cache is opened (when first used), read and written on tokio's blocking pool, never on the engine's
//!   thread; a cache that can't be opened, read or written only means the picture is read, and a cached picture whose
//!   bytes changed or aren't a picture is read again.
//! - **One read per picture at a time**: asks while its read runs wait for that read.
//! - A `src` that names neither ([`InlinePicture::of`]), or a tile's URL off the CDN's app folders
//!   ([`AppPicture::of`]), is answered `None` with nothing read.
//! - Nothing is sent but the reads.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use teams_proto::{
    AppPicture, Client, Credentials, HttpError, HttpTransport, InlinePicture, PastedFormat, PastedPicture, Read,
    Response,
};
use teams_store::pictures::PICTURE_CACHE_BYTES;
use teams_store::{PictureCache, StoreError};

use crate::engine::{EngineError, Responder};

/// A picture's bytes and their format, shared with the views.
pub type PictureBytes = Arc<PastedPicture>;

/// How many bytes of pictures are kept at most.
pub const KEPT_BYTES: usize = 64 * 1024 * 1024;

/// What is known of one picture.
pub(crate) enum Known {
    /// Read: its bytes, or `None` when the service has none.
    Read(Option<PictureBytes>),
    /// Its read runs; these wait for it.
    Reading(Vec<Responder<Option<PictureBytes>>>),
}

/// The pictures kept, by what names each (a message's AMS view or GIF; a shared file, [`crate::thumbnails`]).
pub(crate) struct Kept<K> {
    pub(crate) known: HashMap<K, Known>,
    /// The pictures read, oldest first, and their bytes in all.
    order: VecDeque<K>,
    bytes: usize,
}

impl<K> Default for Kept<K> {
    fn default() -> Self {
        Self {
            known: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }
}

impl<K: Clone + Eq + Hash> Kept<K> {
    /// Keeps `picture`'s answer, letting the oldest go while more than [`KEPT_BYTES`] are kept; the asks it had waiting.
    pub(crate) fn keep(&mut self, picture: K, answer: Option<PictureBytes>) -> Option<Known> {
        self.bytes += answer.as_ref().map_or(0, |bytes| bytes.bytes.len());
        self.order.push_back(picture.clone());
        let waiting = self.known.insert(picture, Known::Read(answer));
        while self.bytes > KEPT_BYTES {
            let Some(oldest) = self.order.pop_front() else { break };
            // a picture read again since keeps its asks
            if !matches!(self.known.get(&oldest), Some(Known::Read(_))) {
                continue;
            }
            if let Some(Known::Read(Some(bytes))) = self.known.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(bytes.bytes.len());
            }
        }
        waiting
    }
}

/// The account's picture cache on disk, shared by the messages' pictures and the files' thumbnails
/// ([`crate::thumbnails`]): opened on the blocking pool by the first picture that asks for it, and every picture read
/// once it can't be opened.
#[derive(Clone)]
pub(crate) struct Disk(Arc<Mutex<Opened>>);

/// Whether the cache was opened.
enum Opened {
    /// Not yet: it is opened at this path when first used.
    Not(PathBuf),
    Open(PictureCache),
    /// It couldn't be opened: this run doesn't use it.
    Unusable,
}

impl Disk {
    /// The cache beside the store at `store` ([`PictureCache::file_beside`]), opened when first used.
    pub(crate) fn beside(store: &Path) -> Self {
        Self(Arc::new(Mutex::new(Opened::Not(PictureCache::file_beside(store)))))
    }

    /// The picture `key` names, if the cache holds one stored less than `max_age` ago (any age when `None`) whose
    /// bytes are a picture.
    async fn get(&self, key: String, max_age: Option<Duration>) -> Option<PictureBytes> {
        let opened = Arc::clone(&self.0);
        let found = tokio::task::spawn_blocking(move || {
            using(&opened, "a cached picture couldn't be read", |cache| {
                cache.get(&key, max_age)
            })
        })
        .await;
        let bytes = found.ok().flatten().flatten()?;
        let format = PastedFormat::sniff(&bytes)?;
        Some(Arc::new(PastedPicture { format, bytes }))
    }

    /// The picture `key` names: the cache's ([`Self::get`]), else what `fetch` reads (`Some(None)` when the service
    /// has none, `None` when the read failed on the way), its bytes then cached ([`Self::put`]). `fetch` runs only on
    /// a miss.
    pub(crate) async fn or_fetch(
        &self,
        key: String,
        max_age: Option<Duration>,
        fetch: impl Future<Output = Option<Option<PictureBytes>>>,
    ) -> Option<Option<PictureBytes>> {
        if let Some(cached) = self.get(key.clone(), max_age).await {
            return Some(Some(cached));
        }
        let answer = fetch.await;
        if let Some(Some(bytes)) = &answer {
            self.put(key, bytes);
        }
        answer
    }

    /// Keeps `picture` as the one `key` names, written on the blocking pool while the engine goes on.
    fn put(&self, key: String, picture: &PictureBytes) {
        let opened = Arc::clone(&self.0);
        let picture = Arc::clone(picture);
        // not awaited: the asks are answered already; the runtime waits for it before the engine's lock goes
        drop(tokio::task::spawn_blocking(move || {
            using(&opened, "a picture couldn't be cached", |cache| {
                cache.put(&key, &picture.bytes)
            })
        }));
    }
}

/// Runs `with` on the cache, opening it first when it wasn't yet (on the blocking pool, as every caller runs it);
/// `None` when it can't be opened or `with` failed (logged as `failed`, without the key).
fn using<T>(
    opened: &Mutex<Opened>,
    failed: &str,
    with: impl FnOnce(&mut PictureCache) -> Result<T, StoreError>,
) -> Option<T> {
    let mut opened = opened.lock().unwrap_or_else(PoisonError::into_inner);
    if let Opened::Not(path) = &*opened {
        let next = match PictureCache::open(path, PICTURE_CACHE_BYTES) {
            Ok(cache) => Opened::Open(cache),
            Err(error) => {
                log::info!("the picture cache isn't used this run: {error}");
                Opened::Unusable
            }
        };
        *opened = next;
    }
    let Opened::Open(cache) = &mut *opened else {
        return None;
    };
    match with(cache) {
        Ok(done) => Some(done),
        Err(error) => {
            log::info!("{failed}: {error}");
            None
        }
    }
}

/// A picture read by what names it, kept for the run under that and cached on disk under [`Self::cache_key`].
pub(crate) trait PictureRead: Read + Clone + Eq + Hash + 'static {
    fn cache_key(&self) -> String;
    fn picture_of(&self, answer: &Response) -> Option<PastedPicture>;
}

impl PictureRead for InlinePicture {
    /// The AMS object and its view, the GIF's id and file, or the page's picture a link preview's thumbnail is of
    /// (never the link the message names: the account's AMS base reads it, whichever host the message names).
    fn cache_key(&self) -> String {
        match self {
            Self::Ams(picture) => format!("ams/{}/{}", picture.object(), picture.view()),
            Self::Giphy(picture) => format!("giphy/{}/{}", picture.id(), picture.file()),
            Self::LinkPreview(picture) => format!("urlp/thumbnail/{}", picture.picture()),
        }
    }

    fn picture_of(&self, answer: &Response) -> Option<PastedPicture> {
        self.read_answer(answer)
    }
}

impl PictureRead for AppPicture {
    fn cache_key(&self) -> String {
        self.key()
    }

    fn picture_of(&self, answer: &Response) -> Option<PastedPicture> {
        self.read_answer(answer)
    }
}

pub(crate) struct Pictures<C> {
    client: Arc<Client<HttpTransport<C>>>,
    disk: Disk,
    kept: Rc<RefCell<Kept<InlinePicture>>>,
    apps: Rc<RefCell<Kept<AppPicture>>>,
}

impl<C: Credentials + 'static> Pictures<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, disk: Disk) -> Self {
        Self {
            client,
            disk,
            kept: Rc::default(),
            apps: Rc::default(),
        }
    }

    /// Answers `reply` with the picture `src` names: at once when it is known, else once its read is over (the read
    /// started now unless one runs). A `src` no read can name is answered `None`.
    pub(crate) fn picture(&self, src: &str, reply: Responder<Option<PictureBytes>>) {
        match InlinePicture::of(src) {
            Some(picture) => self.ask(&self.kept, picture, reply),
            None => {
                let _ = reply.send(Ok(None));
            }
        }
    }

    /// Answers `reply` with the app tile `url` names (a definition's `largeImageUrl`), as [`Self::picture`] does; a
    /// URL off the CDN's app folders is answered `None`.
    pub(crate) fn app_picture(&self, url: &str, reply: Responder<Option<PictureBytes>>) {
        match AppPicture::of(url) {
            Some(picture) => self.ask(&self.apps, picture, reply),
            None => {
                let _ = reply.send(Ok(None));
            }
        }
    }

    fn ask<P: PictureRead>(&self, kept_by: &Rc<RefCell<Kept<P>>>, picture: P, reply: Responder<Option<PictureBytes>>) {
        let mut kept = kept_by.borrow_mut();
        match kept.known.get_mut(&picture) {
            Some(Known::Read(bytes)) => {
                let _ = reply.send(Ok(bytes.clone()));
            }
            Some(Known::Reading(waiting)) => waiting.push(reply),
            None => {
                kept.known.insert(picture.clone(), Known::Reading(vec![reply]));
                // on the engine's `LocalSet`: the read waits on nothing of the engine's loop
                tokio::task::spawn_local(read_picture(
                    Arc::clone(&self.client),
                    self.disk.clone(),
                    picture,
                    Rc::clone(kept_by),
                ));
            }
        }
    }
}

/// Reads `picture` from the service: its bytes, `Some(None)` when the service has none, `None` when the read failed on
/// the way.
async fn fetch<C: Credentials + 'static, P: PictureRead>(
    client: &Client<HttpTransport<C>>,
    picture: &P,
) -> Option<Option<PictureBytes>> {
    match client.read(picture).await {
        Ok(answer) => Some(picture.picture_of(&answer).map(Arc::new)),
        Err(HttpError::Status(_) | HttpError::BodyTooLarge) => Some(None),
        Err(error) => {
            log::info!("a picture couldn't be read: {error}");
            None
        }
    }
}

/// Finds `picture` in the cache on disk, else reads it (and caches what it read); answers every ask that waits for
/// it; keeps it, or that there is none, unless the read failed on the way ([`EngineError::PictureUnread`]).
async fn read_picture<C: Credentials + 'static, P: PictureRead>(
    client: Arc<Client<HttpTransport<C>>>,
    disk: Disk,
    picture: P,
    kept: Rc<RefCell<Kept<P>>>,
) {
    let key = picture.cache_key();
    let answer = disk.or_fetch(key, None, fetch(&client, &picture)).await;
    let waiting = match answer.clone() {
        Some(bytes) => kept.borrow_mut().keep(picture, bytes),
        None => kept.borrow_mut().known.remove(&picture),
    };
    let Some(Known::Reading(waiting)) = waiting else { return };
    for reply in waiting {
        let _ = reply.send(answer.clone().ok_or(EngineError::PictureUnread));
    }
}

#[cfg(test)]
mod tests {
    use teams_proto::PastedFormat;

    use super::*;

    fn view(n: usize) -> InlinePicture {
        InlinePicture::of(&format!(
            "https://us-api.asm.skype.com/v1/objects/0-eus-d9-{n:032x}/views/imgo"
        ))
        .unwrap()
    }

    fn bytes(len: usize) -> PictureBytes {
        Arc::new(PastedPicture {
            format: PastedFormat::Png,
            bytes: vec![0; len],
        })
    }

    #[test]
    fn the_oldest_pictures_are_let_go_past_the_bytes_kept() {
        let mut kept = Kept::default();
        let third = KEPT_BYTES / 3 + 1;
        for n in 0..3 {
            kept.keep(view(n), Some(bytes(third)));
        }
        assert!(!kept.known.contains_key(&view(0)), "the oldest let go");
        assert!(kept.known.contains_key(&view(1)) && kept.known.contains_key(&view(2)));
        assert_eq!(kept.bytes, 2 * third);
        // a view with none costs nothing, and is kept
        kept.keep(view(9), None);
        assert!(matches!(kept.known.get(&view(9)), Some(Known::Read(None))));
        assert_eq!(kept.bytes, 2 * third);
    }
}
