//! The user's sticker packs: one store for all the app's accounts ([`store`], beside the settings file), the import
//! of a folder or a `.zip` of pictures as a pack ([`import`]), and the [`Shelf`] the compose box's picker shows them
//! from.
//!
//! The shelf reads the store once, then keeps it in step with what the app does to it: an import (run on a background
//! thread, its progress and its outcome kept on the shelf, so the picker shows them when it opens again), a rename, a
//! removal, a sticker put in a message (Recent). Each sticker's picture is decoded once, on a background thread, as
//! it is first drawn. A shelf with no directory (settings kept in memory) holds nothing and imports nothing.

pub mod import;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{App, AppContext as _, Context, Entity, Global, RenderImage};
use teams_proto::PastedPicture;

pub use import::{ImportError, ImportKind, Imported, Progress, Skip, Skipped};
pub use store::{Pack, Sticker, StickerRef, StoreError};

/// How the last import stands, while the shelf keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportState {
    Running(Progress),
    Done(Imported),
    Failed(ImportError),
}

/// The shelf of each stickers directory, for the app.
#[derive(Default)]
struct Shelves(HashMap<Option<PathBuf>, Entity<Shelf>>);

impl Global for Shelves {}

/// A sticker's picture as the picker draws it.
#[derive(Clone)]
pub enum Thumb {
    Loading,
    Ready(Arc<RenderImage>),
    Broken,
}

pub struct Shelf {
    dir: Option<PathBuf>,
    packs: Vec<Pack>,
    recent: Vec<StickerRef>,
    import: Option<ImportState>,
    /// Another import was asked for while one ran, and was refused.
    refused: bool,
    thumbs: HashMap<StickerRef, Thumb>,
    /// The thumbs asked for since the last batch was read.
    wanted: HashSet<StickerRef>,
}

impl Shelf {
    /// The shelf of the stickers kept in `dir` (`None`: none kept), made and read the first time it is asked for.
    pub fn of(dir: Option<PathBuf>, cx: &mut App) -> Entity<Self> {
        if let Some(shelf) = cx.default_global::<Shelves>().0.get(&dir) {
            return shelf.clone();
        }
        let shelf = cx.new(|_| Self {
            packs: dir.as_deref().map(store::list).unwrap_or_default(),
            recent: dir.as_deref().map(store::recent).unwrap_or_default(),
            dir: dir.clone(),
            import: None,
            refused: false,
            thumbs: HashMap::new(),
            wanted: HashSet::new(),
        });
        cx.default_global::<Shelves>().0.insert(dir, shelf.clone());
        shelf
    }

    /// Where the stickers are kept; `None` when nothing can be kept.
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The packs, the first added first.
    #[must_use]
    pub fn packs(&self) -> &[Pack] {
        &self.packs
    }

    /// Pack `id`.
    #[must_use]
    pub fn pack(&self, id: &str) -> Option<&Pack> {
        self.packs.iter().find(|pack| pack.id == id)
    }

    /// The sticker `sticker` names, while it is kept.
    #[must_use]
    pub fn sticker(&self, sticker: &StickerRef) -> Option<&Sticker> {
        self.pack(&sticker.pack)?
            .stickers
            .iter()
            .find(|held| held.file == sticker.file)
    }

    /// The stickers put in a message most recently, the last first.
    #[must_use]
    pub fn recent(&self) -> &[StickerRef] {
        &self.recent
    }

    /// The stickers whose label or pack's name has every word of `query` in it, in the packs' order.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<StickerRef> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        self.packs
            .iter()
            .flat_map(|pack| {
                let name = pack.name.to_lowercase();
                let words = &words;
                pack.stickers.iter().filter_map(move |sticker| {
                    let label = sticker.label.to_lowercase();
                    words
                        .iter()
                        .all(|word| label.contains(word.as_str()) || name.contains(word.as_str()))
                        .then(|| StickerRef {
                            pack: pack.id.clone(),
                            file: sticker.file.clone(),
                        })
                })
            })
            .collect()
    }

    /// How the last import stands.
    #[must_use]
    pub fn import_state(&self) -> Option<&ImportState> {
        self.import.as_ref()
    }

    /// Whether an import is under way.
    #[must_use]
    pub fn importing(&self) -> bool {
        matches!(self.import, Some(ImportState::Running(_)))
    }

    /// Whether another import was asked for while this one runs (and refused: one import at a time).
    #[must_use]
    pub fn refused(&self) -> bool {
        self.refused && self.importing()
    }

    /// Forgets how the last import went (its summary closed).
    pub fn dismiss_import(&mut self, cx: &mut Context<Self>) {
        if !self.importing() {
            self.import = None;
            cx.notify();
        }
    }

    /// Imports the folder or archive at `source` as a new pack, on a background thread; one import at a time
    /// (a second one is refused, [`Self::refused`]).
    pub fn import(&mut self, source: PathBuf, kind: ImportKind, cx: &mut Context<Self>) {
        let Some(dir) = self.dir.clone() else {
            self.import = Some(ImportState::Failed(ImportError::Storage));
            cx.notify();
            return;
        };
        if self.importing() {
            self.refused = true;
            cx.notify();
            return;
        }
        self.refused = false;
        self.import = Some(ImportState::Running(Progress::default()));
        cx.notify();
        let (tell, told) = async_channel::unbounded();
        let work = cx.background_spawn(async move {
            import::import(&source, kind, &dir, &mut |progress| {
                let _ = tell.try_send(progress);
            })
        });
        cx.spawn(async move |this, cx| {
            while let Ok(progress) = told.recv().await {
                let _ = this.update(cx, |this, cx| this.progressed(progress, cx));
            }
            let outcome = work.await;
            let _ = this.update(cx, |this, cx| this.imported(outcome, cx));
        })
        .detach();
    }

    fn progressed(&mut self, progress: Progress, cx: &mut Context<Self>) {
        if self.importing() {
            self.import = Some(ImportState::Running(progress));
            cx.notify();
        }
    }

    fn imported(&mut self, outcome: Result<Imported, ImportError>, cx: &mut Context<Self>) {
        self.refused = false;
        self.import = Some(match outcome {
            Ok(imported) => {
                self.packs.retain(|pack| pack.id != imported.pack.id);
                self.packs.push(imported.pack.clone());
                ImportState::Done(imported)
            }
            Err(error) => ImportState::Failed(error),
        });
        cx.notify();
    }

    /// Renames pack `id`.
    ///
    /// # Errors
    /// As [`store::rename`].
    pub fn rename(&mut self, id: &str, name: &str, cx: &mut Context<Self>) -> Result<(), StoreError> {
        let dir = self.dir.as_deref().ok_or(StoreError::Storage)?;
        let renamed = store::rename(dir, id, name)?;
        if let Some(pack) = self.packs.iter_mut().find(|pack| pack.id == id) {
            pack.name = renamed.name;
        }
        cx.notify();
        Ok(())
    }

    /// Removes pack `id` and its stickers.
    ///
    /// # Errors
    /// As [`store::remove`].
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) -> Result<(), StoreError> {
        let dir = self.dir.as_deref().ok_or(StoreError::Storage)?;
        store::remove(dir, id)?;
        self.packs.retain(|pack| pack.id != id);
        self.recent.retain(|sticker| sticker.pack != id);
        self.thumbs.retain(|sticker, _| sticker.pack != id);
        cx.notify();
        Ok(())
    }

    /// Puts `sticker` first in Recent. A failed write keeps it there for this run.
    pub fn record_recent(&mut self, sticker: &StickerRef, cx: &mut Context<Self>) {
        let written = self
            .dir
            .as_deref()
            .and_then(|dir| store::record_recent(dir, sticker).ok());
        self.recent = written.unwrap_or_else(|| {
            let mut recent = std::mem::take(&mut self.recent);
            recent.retain(|held| held != sticker);
            recent.insert(0, sticker.clone());
            recent.truncate(store::RECENT);
            recent
        });
        cx.notify();
    }

    /// The bytes of `sticker` as it is sent, read now (a sticker is at most [`store::MAX_STICKER`]).
    #[must_use]
    pub fn picture_bytes(&self, sticker: &StickerRef) -> Option<PastedPicture> {
        let bytes = store::read_sticker(self.dir.as_deref()?, sticker)?;
        let format = teams_proto::PastedFormat::sniff(&bytes)?;
        Some(PastedPicture { format, bytes })
    }

    /// `sticker`'s picture as drawn; the first ask reads and decodes it on a background thread.
    pub fn thumb(&mut self, sticker: &StickerRef, cx: &mut Context<Self>) -> Thumb {
        if let Some(thumb) = self.thumbs.get(sticker) {
            return thumb.clone();
        }
        self.thumbs.insert(sticker.clone(), Thumb::Loading);
        let first = self.wanted.is_empty();
        self.wanted.insert(sticker.clone());
        if first {
            // the asks of one frame are read together
            cx.spawn(async move |this, cx| {
                let Ok((dir, wanted)) = this.update(cx, |this, _| (this.dir.clone(), std::mem::take(&mut this.wanted)))
                else {
                    return;
                };
                let read = cx
                    .background_spawn(async move { read_thumbs(dir.as_deref(), wanted) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.thumbs.extend(read);
                    cx.notify();
                });
            })
            .detach();
        }
        Thumb::Loading
    }
}

/// The pictures of `wanted` in `dir`, decoded as drawn.
fn read_thumbs(dir: Option<&Path>, wanted: HashSet<StickerRef>) -> Vec<(StickerRef, Thumb)> {
    wanted
        .into_iter()
        .map(|sticker| {
            let thumb = dir
                .and_then(|dir| store::read_sticker(dir, &sticker))
                .and_then(|bytes| {
                    let format = teams_proto::PastedFormat::sniff(&bytes)?;
                    crate::inline::decode(&Arc::new(PastedPicture { format, bytes }))
                })
                .map_or(Thumb::Broken, |(image, _)| Thumb::Ready(Arc::new(image)));
            (sticker, thumb)
        })
        .collect()
}
