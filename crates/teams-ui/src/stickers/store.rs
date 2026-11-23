//! The sticker packs kept on this computer, for every account of the app: [`DIR`] beside the settings file
//! ([`crate::SettingsModel::stickers_dir`]), so a lab alias keeps its own.
//!
//! ```text
//! stickers/
//!   recent.toml           # recent = ["<pack>/<file>", …], the last sent first ("@teams/<id>": one of Teams')
//!   <pack id>/
//!     pack.toml           # name, added, and a [[stickers]] table per sticker: file, label, width, height
//!     001.png             # a still sticker, as it is sent
//!     002.gif             # an animated one
//! ```
//!
//! Every file is the app's own: written by [`super::import`] (a pack id and a file name made here, never taken from what
//! was imported), read back only when its name is one the app makes ([`is_id`], [`is_file_name`]) and its size is one
//! it keeps. A pack directory that isn't whole (a hidden one an import is still filling, or a damaged one) is skipped.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::settings::write_private;

/// The directory of the packs, beside the settings file.
pub const DIR: &str = "stickers";
/// A pack's manifest.
pub const MANIFEST: &str = "pack.toml";
/// The list of recently sent stickers.
pub const RECENT_FILE: &str = "recent.toml";
/// The stickers Recent holds: two rows of four, as Teams' Stickers tab shows.
pub const RECENT: usize = 8;
/// The stickers [`RECENT_FILE`] keeps: more than Recent shows, as each account's picker shows only the ones it can
/// send, and all accounts share the file.
pub const RECENT_KEPT: usize = 4 * RECENT;
/// The longest pack name, in characters.
pub const MAX_NAME: usize = 64;
/// The longest sticker label, in characters.
pub const MAX_LABEL: usize = 120;
/// The longest pack id, in bytes.
pub const MAX_ID: usize = 72;
/// The largest manifest read, in bytes.
pub const MAX_MANIFEST: u64 = 256 * 1024;
/// The largest sticker file kept or read, in bytes.
pub const MAX_STICKER: u64 = 8 * 1024 * 1024;
/// The most stickers in a pack.
pub const MAX_STICKERS: usize = 200;
/// The most packs kept.
pub const MAX_PACKS: usize = 100;

/// Why the store can't do what was asked, in the user's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("Give the pack a name of up to {MAX_NAME} characters.")]
    BadName,
    #[error("That sticker pack isn't there anymore.")]
    NoPack,
    #[error("The stickers can't be saved on this computer.")]
    Storage,
}

/// A sticker of a pack, as its manifest lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sticker {
    /// Its file in the pack's directory: `<number>.png` or `<number>.gif`.
    pub file: String,
    /// What it shows, in words: its alt text and its name in a search.
    pub label: String,
    /// The size it is shown at, in CSS pixels.
    pub size: (u32, u32),
}

impl Sticker {
    /// Whether it moves (a GIF).
    #[must_use]
    pub fn animated(&self) -> bool {
        Path::new(&self.file)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
    }
}

/// A pack the user added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    pub id: String,
    pub name: String,
    /// When it was added, in seconds since 1970: the packs are listed in that order.
    pub added: u64,
    pub stickers: Vec<Sticker>,
}

/// A sticker by its pack and file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StickerRef {
    pub pack: String,
    pub file: String,
}

/// The pack of every sticker of Teams' own ([`super::teams`]): no pack the user adds has this id ([`is_id`]).
pub const TEAMS_PACK: &str = "@teams";

impl StickerRef {
    /// Teams' sticker `id` ([`super::teams::Catalog::sticker`]).
    #[must_use]
    pub fn teams(id: &str) -> Self {
        Self {
            pack: TEAMS_PACK.to_owned(),
            file: id.to_owned(),
        }
    }

    /// Whether it is one of Teams' stickers.
    #[must_use]
    pub fn is_teams(&self) -> bool {
        self.pack == TEAMS_PACK
    }

    /// `<pack>/<file>`, as [`RECENT_FILE`] keeps it.
    fn key(&self) -> String {
        format!("{}/{}", self.pack, self.file)
    }

    fn of_key(key: &str) -> Option<Self> {
        let (pack, file) = key.split_once('/')?;
        if pack == TEAMS_PACK {
            return super::teams::catalog().sticker(file).map(|_| Self::teams(file));
        }
        (is_id(pack) && is_file_name(file)).then(|| Self {
            pack: pack.to_owned(),
            file: file.to_owned(),
        })
    }
}

/// Whether `id` could be a pack's id: 1 to [`MAX_ID`] lowercase ASCII letters, digits and dashes, starting with a
/// letter or a digit. Nothing else ever becomes a directory's name.
#[must_use]
pub fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Whether `file` could be a sticker's file: 1 to 6 digits, then `.png` or `.gif`.
#[must_use]
pub fn is_file_name(file: &str) -> bool {
    let Some((number, extension)) = file.split_once('.') else {
        return false;
    };
    (1..=6).contains(&number.len())
        && number.bytes().all(|byte| byte.is_ascii_digit())
        && matches!(extension, "png" | "gif")
}

/// `text` trimmed, if it is fit to show as a name of at most `max` characters: not blank, and with no control
/// characters.
#[must_use]
pub fn clean_name(text: &str, max: usize) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)).then(|| text.to_owned())
}

/// Reads at most `limit` bytes of the plain file at `path` (no symbolic link).
pub(super) fn read_file(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= limit).then_some(bytes)
}

/// A manifest's text for a pack called `name`, added at `added`, holding `stickers`.
pub(super) fn manifest_text(name: &str, added: u64, stickers: &[Sticker]) -> String {
    let mut table = Table::new();
    table.insert("name".to_owned(), Value::String(name.to_owned()));
    table.insert(
        "added".to_owned(),
        Value::Integer(i64::try_from(added).unwrap_or(i64::MAX)),
    );
    let rows = stickers
        .iter()
        .map(|sticker| {
            let mut row = Table::new();
            row.insert("file".to_owned(), Value::String(sticker.file.clone()));
            row.insert("label".to_owned(), Value::String(sticker.label.clone()));
            row.insert("width".to_owned(), Value::Integer(i64::from(sticker.size.0)));
            row.insert("height".to_owned(), Value::Integer(i64::from(sticker.size.1)));
            Value::Table(row)
        })
        .collect();
    table.insert("stickers".to_owned(), Value::Array(rows));
    table.to_string()
}

/// A side of a shown size, if it is one the app writes.
fn side(row: &Table, key: &str) -> Option<u32> {
    row.get(key)
        .and_then(Value::as_integer)
        .and_then(|side| u32::try_from(side).ok())
        .filter(|side| (1..=super::import::SHOWN_SIDE).contains(side))
}

/// The pack a manifest's `text` describes, as pack `id`; `None` when it isn't one the app wrote.
fn parse_manifest(id: &str, text: &str) -> Option<Pack> {
    let table: Table = text.parse().ok()?;
    let name = clean_name(table.get("name")?.as_str()?, MAX_NAME)?;
    let added = table
        .get("added")
        .and_then(Value::as_integer)
        .and_then(|added| u64::try_from(added).ok())
        .unwrap_or_default();
    let rows = table.get("stickers")?.as_array()?;
    let stickers = rows
        .iter()
        .take(MAX_STICKERS)
        .filter_map(|row| {
            let row = row.as_table()?;
            let file = row.get("file")?.as_str().filter(|file| is_file_name(file))?;
            let label = row
                .get("label")
                .and_then(Value::as_str)
                .and_then(|label| clean_name(label, MAX_LABEL))
                .unwrap_or_else(|| super::import::DEFAULT_LABEL.to_owned());
            Some(Sticker {
                file: file.to_owned(),
                label,
                size: (side(row, "width")?, side(row, "height")?),
            })
        })
        .collect();
    Some(Pack {
        id: id.to_owned(),
        name,
        added,
        stickers,
    })
}

/// The pack kept at `dir`, named `id`, if it is one: its stickers those whose file is there.
fn read_pack(dir: &Path, id: &str) -> Option<Pack> {
    if !is_id(id) || !fs::symlink_metadata(dir).ok()?.is_dir() {
        return None;
    }
    let text = read_file(&dir.join(MANIFEST), MAX_MANIFEST)?;
    let mut pack = parse_manifest(id, std::str::from_utf8(&text).ok()?)?;
    pack.stickers.retain(|sticker| {
        fs::symlink_metadata(dir.join(&sticker.file))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= MAX_STICKER)
    });
    Some(pack)
}

/// The packs kept in `dir`, the first added first (then by id). A directory that isn't a pack is skipped.
#[must_use]
pub fn list(dir: &Path) -> Vec<Pack> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut packs: Vec<Pack> = entries
        .flatten()
        .filter_map(|entry| read_pack(&entry.path(), entry.file_name().to_str()?))
        .collect();
    packs.sort_by(|one, other| one.added.cmp(&other.added).then_with(|| one.id.cmp(&other.id)));
    packs
}

/// The bytes of sticker `sticker` of `dir`, as it is sent; `None` when it isn't there.
#[must_use]
pub fn read_sticker(dir: &Path, sticker: &StickerRef) -> Option<Vec<u8>> {
    if !is_id(&sticker.pack) || !is_file_name(&sticker.file) {
        return None;
    }
    read_file(&dir.join(&sticker.pack).join(&sticker.file), MAX_STICKER)
}

/// Renames pack `id` of `dir` to `name`.
///
/// # Errors
/// The name isn't one ([`StoreError::BadName`]), the pack is gone, or its manifest can't be written.
pub fn rename(dir: &Path, id: &str, name: &str) -> Result<Pack, StoreError> {
    let name = clean_name(name, MAX_NAME).ok_or(StoreError::BadName)?;
    let pack_dir = dir.join(id);
    let mut pack = read_pack(&pack_dir, id).ok_or(StoreError::NoPack)?;
    pack.name = name;
    write_private(
        &pack_dir.join(MANIFEST),
        manifest_text(&pack.name, pack.added, &pack.stickers).as_bytes(),
    )
    .map_err(|_| StoreError::Storage)?;
    Ok(pack)
}

/// Removes pack `id` of `dir`, and its stickers from Recent. Nothing outside that directory is touched.
///
/// # Errors
/// It is there and can't be removed.
pub fn remove(dir: &Path, id: &str) -> Result<(), StoreError> {
    if !is_id(id) {
        return Err(StoreError::NoPack);
    }
    let pack_dir = dir.join(id);
    match fs::symlink_metadata(&pack_dir) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&pack_dir).map_err(|_| StoreError::Storage)?,
        Ok(_) => return Err(StoreError::NoPack),
        Err(_) => {}
    }
    let recent: Vec<StickerRef> = recent(dir).into_iter().filter(|sticker| sticker.pack != id).collect();
    write_recent(dir, &recent)
}

/// The stickers sent most recently, the last first, those still kept only.
#[must_use]
pub fn recent(dir: &Path) -> Vec<StickerRef> {
    let Some(text) = read_file(&dir.join(RECENT_FILE), MAX_MANIFEST) else {
        return Vec::new();
    };
    let Ok(table) = std::str::from_utf8(&text).unwrap_or_default().parse::<Table>() else {
        return Vec::new();
    };
    let mut recent: Vec<StickerRef> = Vec::new();
    for key in table
        .get("recent")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(sticker) = StickerRef::of_key(key).filter(|sticker| !recent.contains(sticker)) {
            recent.push(sticker);
        }
    }
    recent.retain(|sticker| sticker.is_teams() || path_of(dir, sticker).is_some());
    recent.truncate(RECENT_KEPT);
    recent
}

/// Puts `sticker` first in Recent; the list after.
///
/// # Errors
/// It can't be written.
pub fn record_recent(dir: &Path, sticker: &StickerRef) -> Result<Vec<StickerRef>, StoreError> {
    let mut recent = recent(dir);
    recent.retain(|held| held != sticker);
    recent.insert(0, sticker.clone());
    recent.truncate(RECENT_KEPT);
    write_recent(dir, &recent)?;
    Ok(recent)
}

fn write_recent(dir: &Path, recent: &[StickerRef]) -> Result<(), StoreError> {
    if recent.is_empty() && !dir.join(RECENT_FILE).exists() {
        return Ok(());
    }
    let mut table = Table::new();
    table.insert(
        "recent".to_owned(),
        Value::Array(recent.iter().map(|sticker| Value::String(sticker.key())).collect()),
    );
    write_private(&dir.join(RECENT_FILE), table.to_string().as_bytes()).map_err(|_| StoreError::Storage)
}

/// The path of a kept sticker's file, if it is there.
fn path_of(dir: &Path, sticker: &StickerRef) -> Option<PathBuf> {
    if !is_id(&sticker.pack) || !is_file_name(&sticker.file) {
        return None;
    }
    let path = dir.join(&sticker.pack).join(&sticker.file);
    fs::symlink_metadata(&path)
        .is_ok_and(|metadata| metadata.is_file())
        .then_some(path)
}
