//! Sound packs the user imports: a folder, or a `.zip` of one, holding a small manifest and sound files (WAV, MP3 or
//! Ogg).
//!
//! ```toml
//! # pack.toml
//! name = "Office chimes"
//! author = "Someone"        # optional
//!
//! [sounds]
//! incoming-call = "ring.ogg"
//! message = "sounds/ping.mp3"
//! ```
//!
//! A pack needn't have every event: the others fall back to the default pack's ([`super::board`]). An event this build
//! doesn't know is ignored, so a pack made for a later build still imports.
//!
//! **A pack is untrusted.** Nothing it names is ever used as a path to write to, and only what its manifest names is
//! read:
//! - a sound's path is relative, `/`-separated, with no `.`, `..`, `:` or `\` ([`check_relative`]), and in a folder no
//!   part of it may be a symbolic link;
//! - the manifest, each file, an archive's entry count and the total read are capped ([`MAX_MANIFEST`], [`MAX_SOUND`],
//!   [`MAX_ENTRIES`], [`MAX_TOTAL`]), by what is read rather than what an archive declares;
//! - every sound must decode ([`teams_media::sound_file::decode`]; its kind told from its bytes), and last at most
//!   [`MAX_SECONDS`];
//! - what is kept is our own: each sound re-encoded as 16-bit mono at the device's rate, its peak brought down to
//!   [`MAX_PEAK`], under a name made from its event, beside a manifest written anew, in a directory named by an id made
//!   here ([`SoundSource::Imported`](super::SoundSource::Imported)).
//!
//! An import builds a hidden directory and renames it into place, so a pack is there whole or not at all. Its errors
//! ([`PackError`]) say what is wrong in the app's own words, and never quote the pack.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read as _, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use teams_media::devices::wav::{self, WavWriter};
use teams_media::sound_file;
use toml::{Table, Value};
use zip::ZipArchive;

use super::SoundEvent;
use crate::settings::write_private;

/// The manifest's file name.
pub const MANIFEST: &str = "pack.toml";
/// The largest manifest read, in bytes.
pub const MAX_MANIFEST: u64 = 16 * 1024;
/// The largest sound file read, in bytes.
pub const MAX_SOUND: u64 = 16 * 1024 * 1024;
/// The most read of one pack, in bytes.
pub const MAX_TOTAL: u64 = 64 * 1024 * 1024;
/// The most entries an archive may hold.
pub const MAX_ENTRIES: usize = 256;
/// The largest archive opened, in bytes.
pub const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
/// The longest a sound may last, in seconds.
pub const MAX_SECONDS: f64 = 30.0;
/// The shortest a sound may last, in seconds.
pub const MIN_SECONDS: f64 = 0.01;
/// The loudest peak a sound is kept at: an imported file never blasts next to the app's own.
pub const MAX_PEAK: f32 = 0.5;
/// The most packs kept.
pub const MAX_PACKS: usize = 64;
/// The longest pack name or author, in bytes.
pub const MAX_NAME: usize = 64;
pub use teams_app::settings::sounds::{MAX_PACK_ID as MAX_ID, is_pack_id as is_id};
/// The id of the pack that holds the single files the user chose for an event ([`import_single`]).
pub const OWN_PACK: &str = "my-sounds";
/// That pack's name.
pub const OWN_PACK_NAME: &str = "My sounds";

/// The largest stored sound read back, in bytes: [`MAX_SECONDS`] of 16-bit mono, with room for its header.
const MAX_STORED: u64 = 4 * 1024 * 1024;

/// How the user handed a pack over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportKind {
    Folder,
    Archive,
}

/// Why a pack can't be imported, as the settings say it. It names events only, never what the pack holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PackError {
    #[error("That pack has no {MANIFEST} file.")]
    NoManifest,
    #[error("That pack's {MANIFEST} can't be read. Give it a name and a [sounds] table.")]
    BadManifest,
    #[error("That pack's name is missing or too long.")]
    BadName,
    #[error("That pack names no sounds this app plays.")]
    NoSounds,
    #[error("The file for \"{}\" has an unsafe path. Use a path inside the pack.", .0.label())]
    UnsafePath(SoundEvent),
    #[error("The file for \"{}\" is missing.", .0.label())]
    Missing(SoundEvent),
    #[error("The file for \"{}\" is too large.", .0.label())]
    TooLarge(SoundEvent),
    #[error("The file for \"{}\" isn't a WAV, OGG or MP3 file this app can play.", .0.label())]
    NotSound(SoundEvent),
    #[error("The sound for \"{}\" has to last between 10 milliseconds and 30 seconds.", .0.label())]
    BadLength(SoundEvent),
    #[error("That archive can't be read. Use a .zip file of up to 64 MB.")]
    BadArchive,
    #[error("That pack is too large.")]
    PackTooLarge,
    #[error("There are too many sound packs. Remove one first.")]
    TooManyPacks,
    #[error("The sound pack can't be saved on this computer.")]
    Storage,
}

/// A pack the user imported, as the settings list it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackInfo {
    pub id: String,
    pub name: String,
    pub author: Option<String>,
    /// The events it has a sound for.
    pub events: Vec<SoundEvent>,
}

/// A manifest, once read.
#[derive(Debug)]
struct Manifest {
    name: String,
    author: Option<String>,
    sounds: BTreeMap<SoundEvent, String>,
}

/// Whether `text` is fit to show as a name: there, short enough, and with no control characters.
fn is_name(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_NAME && !text.chars().any(char::is_control)
}

/// Reads a manifest's text. An event this build doesn't know is ignored.
fn parse_manifest(text: &str) -> Result<Manifest, PackError> {
    let table: Table = text.parse().map_err(|_| PackError::BadManifest)?;
    let name = table
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| is_name(name))
        .ok_or(PackError::BadName)?
        .trim()
        .to_owned();
    let author = match table.get("author") {
        None => None,
        Some(author) => Some(
            author
                .as_str()
                .filter(|author| is_name(author))
                .ok_or(PackError::BadManifest)?
                .trim()
                .to_owned(),
        ),
    };
    let listed = table
        .get("sounds")
        .and_then(Value::as_table)
        .ok_or(PackError::BadManifest)?;
    let mut sounds = BTreeMap::new();
    for (key, value) in listed {
        let Some(event) = SoundEvent::from_key(key) else {
            continue;
        };
        let path = value.as_str().ok_or(PackError::BadManifest)?;
        check_relative(path).ok_or(PackError::UnsafePath(event))?;
        sounds.insert(event, path.to_owned());
    }
    if sounds.is_empty() {
        return Err(PackError::NoSounds);
    }
    Ok(Manifest { name, author, sounds })
}

/// A sound's path split into its parts, if it is a plain relative path inside its pack.
fn check_relative(path: &str) -> Option<Vec<&str>> {
    let parts: Vec<&str> = path.split('/').collect();
    let plain = |part: &&str| {
        !part.is_empty()
            && *part != "."
            && *part != ".."
            && !part.chars().any(|c| c == '\\' || c == ':' || c.is_control())
    };
    (path.len() <= 512 && parts.iter().all(plain)).then_some(parts)
}

/// What a pack's files are read from.
trait Files {
    /// The file at `relative` (checked: [`check_relative`]), at most `limit` bytes. `None` when it isn't there.
    fn read(&mut self, parts: &[&str], limit: u64) -> Result<Option<Vec<u8>>, ReadError>;
}

/// Why a file can't be read.
enum ReadError {
    TooLarge,
    Failed,
}

/// Reads at most `limit` bytes of `reader`.
fn read_limited(reader: impl std::io::Read, limit: u64) -> Result<Vec<u8>, ReadError> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ReadError::Failed)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(ReadError::TooLarge);
    }
    Ok(bytes)
}

/// A pack that is a folder.
struct Folder {
    root: PathBuf,
}

impl Files for Folder {
    fn read(&mut self, parts: &[&str], limit: u64) -> Result<Option<Vec<u8>>, ReadError> {
        let mut path = self.root.clone();
        for (index, part) in parts.iter().enumerate() {
            path.push(part);
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                return Ok(None);
            };
            let last = index + 1 == parts.len();
            let fits = if last { metadata.is_file() } else { metadata.is_dir() };
            if !fits {
                return Ok(None);
            }
        }
        read_limited(File::open(&path).map_err(|_| ReadError::Failed)?, limit).map(Some)
    }
}

/// A pack that is a `.zip`: the manifest found at its top or in one folder at its top.
struct Archive<R> {
    zip: ZipArchive<R>,
    prefix: String,
}

impl Archive<File> {
    fn open(path: &Path) -> Result<Self, PackError> {
        let metadata = fs::metadata(path).map_err(|_| PackError::BadArchive)?;
        if !metadata.is_file() || metadata.len() > MAX_ARCHIVE {
            return Err(PackError::BadArchive);
        }
        Self::over(File::open(path).map_err(|_| PackError::BadArchive)?)
    }
}

impl<R: std::io::Read + Seek> Archive<R> {
    fn over(reader: R) -> Result<Self, PackError> {
        let zip = ZipArchive::new(reader).map_err(|_| PackError::BadArchive)?;
        if zip.len() > MAX_ENTRIES {
            return Err(PackError::BadArchive);
        }
        let names: Vec<String> = zip.file_names().flatten().map(std::borrow::Cow::into_owned).collect();
        let mut nested = None;
        for name in &names {
            if name == MANIFEST {
                return Ok(Self {
                    zip,
                    prefix: String::new(),
                });
            }
            let inside = name
                .strip_suffix(MANIFEST)
                .and_then(|folder| folder.strip_suffix('/'))
                .filter(|folder| !folder.contains('/') && check_relative(folder).is_some());
            if let Some(folder) = inside {
                if nested.is_some() {
                    return Err(PackError::NoManifest);
                }
                nested = Some(format!("{folder}/"));
            }
        }
        let prefix = nested.ok_or(PackError::NoManifest)?;
        Ok(Self { zip, prefix })
    }
}

impl<R: std::io::Read + Seek> Files for Archive<R> {
    fn read(&mut self, parts: &[&str], limit: u64) -> Result<Option<Vec<u8>>, ReadError> {
        let name = format!("{}{}", self.prefix, parts.join("/"));
        let Ok(file) = self.zip.by_name(&name) else {
            return Ok(None);
        };
        if !file.is_file() {
            return Ok(None);
        }
        if file.size() > limit {
            return Err(ReadError::TooLarge);
        }
        read_limited(file, limit).map(Some)
    }
}

/// How much of a pack has been read so far.
#[derive(Default)]
struct Budget {
    read: u64,
}

impl Budget {
    /// Counts `bytes` read.
    fn spend(&mut self, bytes: &[u8]) -> Result<(), PackError> {
        self.read = self.read.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if self.read > MAX_TOTAL {
            return Err(PackError::PackTooLarge);
        }
        Ok(())
    }
}

/// A hidden directory being filled: removed unless it was renamed into place.
struct Building {
    path: PathBuf,
    done: bool,
}

impl Drop for Building {
    fn drop(&mut self) {
        if !self.done {
            // nothing more can be done with a leftover; a later list skips it as hidden
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Makes `path` a private directory (and its parents).
fn private_dir(path: &Path) -> Result<(), PackError> {
    teams_platform::private::create_dir_all(path).map_err(|_| PackError::Storage)
}

/// A sound's samples brought to what is kept: at the device's rate, its peak at most [`MAX_PEAK`].
fn normalized(bytes: &[u8], event: SoundEvent) -> Result<Vec<f32>, PackError> {
    let decoded = sound_file::decode(bytes, MAX_SECONDS).map_err(|_| PackError::NotSound(event))?;
    if !(MIN_SECONDS..=MAX_SECONDS).contains(&decoded.seconds()) {
        return Err(PackError::BadLength(event));
    }
    let mut samples = decoded.at_device_rate();
    let peak = samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak > MAX_PEAK {
        let scale = MAX_PEAK / peak;
        for sample in &mut samples {
            *sample *= scale;
        }
    }
    Ok(samples)
}

/// Writes `samples` as `<event>.wav` in `dir`.
fn write_sound(dir: &Path, event: SoundEvent, samples: &[f32]) -> Result<(), PackError> {
    let path = dir.join(format!("{}.wav", event.key()));
    replace_sound(&path, |temporary| {
        let mut writer = WavWriter::create(temporary).map_err(|_| PackError::Storage)?;
        writer.write(samples).map_err(|_| PackError::Storage)?;
        writer.finish().map_err(|_| PackError::Storage)
    })
}

/// Completes a replacement beside the kept file before publishing it. A failed write leaves that file intact.
fn replace_sound(path: &Path, write: impl FnOnce(&Path) -> Result<(), PackError>) -> Result<(), PackError> {
    let number = IMPORTS.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(".sound-{}-{number}.wav", std::process::id()));
    let result = write(&temporary).and_then(|()| fs::rename(&temporary, path).map_err(|_| PackError::Storage));
    // Also removes a partially written file on failure; after a successful rename there is nothing to remove.
    let _ = fs::remove_file(&temporary);
    result
}

/// The manifest kept beside a pack's sounds.
fn stored_manifest(name: &str, author: Option<&str>, events: &[SoundEvent]) -> String {
    let mut table = Table::new();
    table.insert("name".to_owned(), Value::String(name.to_owned()));
    if let Some(author) = author {
        table.insert("author".to_owned(), Value::String(author.to_owned()));
    }
    let sounds: Table = events
        .iter()
        .map(|event| (event.key().to_owned(), Value::String(format!("{}.wav", event.key()))))
        .collect();
    table.insert("sounds".to_owned(), Value::Table(sounds));
    table.to_string()
}

/// An id for a pack called `name`, free among `taken`, and the name it goes by (numbered when another has it).
fn free_id(name: &str, taken: &[PackInfo]) -> (String, String) {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(MAX_ID - 8).collect();
    let slug = if slug.is_empty() { "pack".to_owned() } else { slug };
    let free = |id: &str, name: &str| id != OWN_PACK && !taken.iter().any(|pack| pack.id == id || pack.name == name);
    if free(&slug, name) {
        return (slug, name.to_owned());
    }
    let mut number = 2;
    loop {
        let suffix = format!(" ({number})");
        let mut end = MAX_NAME.saturating_sub(suffix.len()).min(name.len());
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let (id, shown) = (format!("{slug}-{number}"), format!("{}{suffix}", &name[..end]));
        if free(&id, &shown) {
            return (id, shown);
        }
        number += 1;
    }
}

/// Imports the pack `source` into `packs_dir`.
///
/// # Errors
/// It can't be imported ([`PackError`]); nothing is left behind.
pub fn import(source: &Path, kind: ImportKind, packs_dir: &Path) -> Result<PackInfo, PackError> {
    match kind {
        ImportKind::Folder => {
            let root = fs::canonicalize(source).map_err(|_| PackError::NoManifest)?;
            import_from(&mut Folder { root }, packs_dir)
        }
        ImportKind::Archive => import_from(&mut Archive::open(source)?, packs_dir),
    }
}

/// The counter that keeps two imports' hidden directories apart.
static IMPORTS: AtomicU64 = AtomicU64::new(0);

fn import_from(files: &mut impl Files, packs_dir: &Path) -> Result<PackInfo, PackError> {
    let mut budget = Budget::default();
    let text = match files.read(&[MANIFEST], MAX_MANIFEST) {
        Ok(Some(bytes)) => bytes,
        Ok(None) | Err(ReadError::Failed) => return Err(PackError::NoManifest),
        Err(ReadError::TooLarge) => return Err(PackError::BadManifest),
    };
    budget.spend(&text)?;
    let manifest = parse_manifest(std::str::from_utf8(&text).map_err(|_| PackError::BadManifest)?)?;
    let mut sounds = Vec::with_capacity(manifest.sounds.len());
    for (event, path) in &manifest.sounds {
        let parts = check_relative(path).ok_or(PackError::UnsafePath(*event))?;
        let bytes = match files.read(&parts, MAX_SOUND) {
            Ok(Some(bytes)) => bytes,
            Ok(None) | Err(ReadError::Failed) => return Err(PackError::Missing(*event)),
            Err(ReadError::TooLarge) => return Err(PackError::TooLarge(*event)),
        };
        budget.spend(&bytes)?;
        sounds.push((*event, normalized(&bytes, *event)?));
    }
    private_dir(packs_dir)?;
    let taken = list(packs_dir);
    if taken.len() >= MAX_PACKS {
        return Err(PackError::TooManyPacks);
    }
    let (id, name) = free_id(&manifest.name, &taken);
    let number = IMPORTS.fetch_add(1, Ordering::Relaxed);
    let mut building = Building {
        path: packs_dir.join(format!(".import-{}-{number}", std::process::id())),
        done: false,
    };
    private_dir(&building.path)?;
    let events: Vec<SoundEvent> = sounds.iter().map(|(event, _)| *event).collect();
    for (event, samples) in &sounds {
        write_sound(&building.path, *event, samples)?;
    }
    write_private(
        &building.path.join(MANIFEST),
        stored_manifest(&name, manifest.author.as_deref(), &events).as_bytes(),
    )
    .map_err(|_| PackError::Storage)?;
    fs::rename(&building.path, packs_dir.join(&id)).map_err(|_| PackError::Storage)?;
    building.done = true;
    Ok(PackInfo {
        id,
        name,
        author: manifest.author,
        events,
    })
}

/// Keeps the sound file `source` as the user's own sound for `event`, in the pack of those files ([`OWN_PACK`], made the
/// first time), the sound it had before replaced.
///
/// # Errors
/// It can't be used or kept ([`PackError`]); what was kept before stays.
pub fn import_single(source: &Path, event: SoundEvent, packs_dir: &Path) -> Result<PackInfo, PackError> {
    let metadata = fs::metadata(source).map_err(|_| PackError::Missing(event))?;
    if !metadata.is_file() {
        return Err(PackError::Missing(event));
    }
    let file = File::open(source).map_err(|_| PackError::Missing(event))?;
    let bytes = read_limited(file, MAX_SOUND).map_err(|error| match error {
        ReadError::TooLarge => PackError::TooLarge(event),
        ReadError::Failed => PackError::Missing(event),
    })?;
    let samples = normalized(&bytes, event)?;
    let dir = packs_dir.join(OWN_PACK);
    private_dir(&dir)?;
    let mut events = read_pack(&dir, OWN_PACK).map(|pack| pack.events).unwrap_or_default();
    if !events.contains(&event) {
        events.push(event);
        events.sort_unstable();
    }
    write_private(
        &dir.join(MANIFEST),
        stored_manifest(OWN_PACK_NAME, None, &events).as_bytes(),
    )
    .map_err(|_| PackError::Storage)?;
    write_sound(&dir, event, &samples)?;
    Ok(PackInfo {
        id: OWN_PACK.to_owned(),
        name: OWN_PACK_NAME.to_owned(),
        author: None,
        events,
    })
}

/// The packs kept in `packs_dir`, by name. A directory that isn't a pack is skipped.
#[must_use]
pub fn list(packs_dir: &Path) -> Vec<PackInfo> {
    let Ok(entries) = fs::read_dir(packs_dir) else {
        return Vec::new();
    };
    let mut packs: Vec<PackInfo> = entries
        .flatten()
        .filter_map(|entry| read_pack(&entry.path(), entry.file_name().to_str()?))
        .collect();
    packs.sort_by(|one, other| one.name.cmp(&other.name));
    packs
}

/// The pack kept at `dir`, named `id`, if it is one.
fn read_pack(dir: &Path, id: &str) -> Option<PackInfo> {
    if !is_id(id) || !fs::symlink_metadata(dir).ok()?.is_dir() {
        return None;
    }
    let text = read_limited(File::open(dir.join(MANIFEST)).ok()?, MAX_MANIFEST).ok()?;
    let manifest = parse_manifest(std::str::from_utf8(&text).ok()?).ok()?;
    let events = manifest
        .sounds
        .keys()
        .copied()
        .filter(|event| stored_file(dir, *event).is_some())
        .collect();
    Some(PackInfo {
        id: id.to_owned(),
        name: manifest.name,
        author: manifest.author,
        events,
    })
}

/// The path of `event`'s sound in the pack at `dir`, if it is there as a plain file of a size kept.
fn stored_file(dir: &Path, event: SoundEvent) -> Option<PathBuf> {
    let path = dir.join(format!("{}.wav", event.key()));
    let metadata = fs::symlink_metadata(&path).ok()?;
    (metadata.is_file() && metadata.len() <= MAX_STORED).then_some(path)
}

/// `event`'s sound in pack `id` of `packs_dir`, read again from its file; `None` when it can't be had (it is told why,
/// without the pack's name, in the log). An event absent from the manifest needs no fallback warning.
#[must_use]
pub fn read_sound(packs_dir: &Path, id: &str, event: SoundEvent) -> Option<Vec<f32>> {
    if !is_id(id) {
        return None;
    }
    let dir = packs_dir.join(id);
    // Missing events are normal for a partial pack. A promised event that is missing or refused is damage.
    let manifest = (|| {
        let bytes = read_limited(File::open(dir.join(MANIFEST)).ok()?, MAX_MANIFEST).ok()?;
        parse_manifest(std::str::from_utf8(&bytes).ok()?).ok()
    })();
    let Some(manifest) = manifest else {
        log::warn!("a sound pack's manifest is missing or can't be read");
        return None;
    };
    if !manifest.sounds.contains_key(&event) {
        return None;
    }
    let Some(path) = stored_file(&dir, event) else {
        log::warn!(
            "a sound pack's {} sound is missing or isn't a plain file of the permitted size",
            event.key()
        );
        return None;
    };
    let Ok(file) = File::open(&path) else {
        log::warn!("a sound pack's {} sound can't be opened", event.key());
        return None;
    };
    let bytes = match read_limited(file, MAX_STORED) {
        Ok(bytes) => bytes,
        Err(ReadError::TooLarge | ReadError::Failed) => {
            log::warn!("a sound pack's {} sound can't be read", event.key());
            return None;
        }
    };
    match wav::decode(&bytes) {
        Ok(samples) if !samples.is_empty() => Some(samples),
        Ok(_) | Err(_) => {
            log::warn!("a sound pack's {} sound is damaged", event.key());
            None
        }
    }
}

/// Removes pack `id` of `packs_dir`. Nothing outside that directory is touched.
///
/// # Errors
/// It is there and can't be removed.
pub fn remove(packs_dir: &Path, id: &str) -> Result<(), PackError> {
    if !is_id(id) {
        return Err(PackError::Storage);
    }
    let dir = packs_dir.join(id);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&dir).map_err(|_| PackError::Storage),
        Ok(_) | Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write as _};

    use zip::write::{FileOptions, ZipWriter};

    use super::*;

    /// A mono 48 kHz WAV of a loud constant, `seconds` long.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a short, positive length"
    )]
    fn wav_bytes(seconds: f32) -> Vec<u8> {
        let count = (seconds * 48_000.0) as usize;
        let data: Vec<u8> = std::iter::repeat_n(i16::MAX.to_le_bytes(), count).flatten().collect();
        let mut bytes = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&48_000_u32.to_le_bytes());
        bytes.extend_from_slice(&96_000_u32.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_le_bytes());
        bytes.extend_from_slice(&data);
        bytes
    }

    type Case<'a> = (&'a str, Vec<(&'a str, Vec<u8>)>, PackError);

    const GOOD: &str =
        "name = \"Office\"\nauthor = \"Me\"\n[sounds]\nmessage = \"sounds/ping.wav\"\nfuture-event = \"x.wav\"\n";

    fn folder(manifest: &str, files: &[(&str, Vec<u8>)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temporary directory");
        fs::write(dir.path().join(MANIFEST), manifest).expect("written");
        for (name, bytes) in files {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().expect("a parent")).expect("made");
            fs::write(path, bytes).expect("written");
        }
        dir
    }

    fn zip_of(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in files {
            out.start_file(*name, FileOptions::<()>::default()).expect("started");
            out.write_all(bytes).expect("written");
        }
        out.finish().expect("finished").into_inner()
    }

    fn imported_zip(bytes: Vec<u8>, packs: &Path) -> Result<PackInfo, PackError> {
        import_from(&mut Archive::over(Cursor::new(bytes))?, packs)
    }

    #[test]
    fn a_folder_imports_and_its_sound_is_ours_afterwards() {
        let pack = folder(GOOD, &[("sounds/ping.wav", wav_bytes(0.5))]);
        let packs = tempfile::tempdir().expect("a temporary directory");
        let info = import(pack.path(), ImportKind::Folder, packs.path()).expect("imported");
        assert_eq!(info.id, "office");
        assert_eq!(info.name, "Office");
        assert_eq!(info.author.as_deref(), Some("Me"));
        assert_eq!(info.events, vec![SoundEvent::Message], "the unknown event is left out");
        assert_eq!(list(packs.path()), vec![info.clone()]);
        let sound = read_sound(packs.path(), "office", SoundEvent::Message).expect("read back");
        assert_eq!(sound.len(), 24_000);
        assert!(
            sound.iter().all(|sample| sample.abs() <= MAX_PEAK + 0.001),
            "the peak is brought down"
        );
        assert_eq!(read_sound(packs.path(), "office", SoundEvent::Mention), None);
        remove(packs.path(), "office").expect("removed");
        assert_eq!(list(packs.path()), Vec::new());
        remove(packs.path(), "office").expect("nothing there is no matter");
    }

    #[test]
    fn a_zip_imports_from_its_top_or_one_folder() {
        let packs = tempfile::tempdir().expect("a temporary directory");
        let top = zip_of(&[(MANIFEST, GOOD.into()), ("sounds/ping.wav", wav_bytes(0.1))]);
        assert_eq!(imported_zip(top, packs.path()).expect("imported").id, "office");
        let nested = zip_of(&[
            ("mine/pack.toml", GOOD.into()),
            ("mine/sounds/ping.wav", wav_bytes(0.1)),
        ]);
        let second = imported_zip(nested, packs.path()).expect("imported");
        assert_eq!((second.id.as_str(), second.name.as_str()), ("office-2", "Office (2)"));
        let file = packs.path().join("pack.zip");
        fs::write(
            &file,
            zip_of(&[(MANIFEST, GOOD.into()), ("sounds/ping.wav", wav_bytes(0.1))]),
        )
        .expect("written");
        let third = import(&file, ImportKind::Archive, packs.path()).expect("imported");
        assert_eq!(third.id, "office-3");
        assert_eq!(list(packs.path()).len(), 3);
    }

    #[test]
    fn path_tricks_are_refused_and_nothing_escapes() {
        let packs = tempfile::tempdir().expect("a temporary directory");
        for bad in [
            "../x.wav",
            "/etc/x.wav",
            "a/../x.wav",
            "./x.wav",
            "a\\x.wav",
            "c:x.wav",
            "",
        ] {
            let manifest = format!("name = \"P\"\n[sounds]\nmessage = {bad:?}\n");
            let pack = folder(&manifest, &[]);
            assert_eq!(
                import(pack.path(), ImportKind::Folder, packs.path()).err(),
                Some(PackError::UnsafePath(SoundEvent::Message)),
                "{bad}"
            );
        }
        assert_eq!(list(packs.path()), Vec::new());
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_in_a_folder_is_not_followed() {
        let outside = tempfile::tempdir().expect("a temporary directory");
        fs::write(outside.path().join("secret.wav"), wav_bytes(0.1)).expect("written");
        let pack = folder(
            "name = \"P\"\n[sounds]\nmessage = \"link.wav\"\nmention = \"dir/x.wav\"\n",
            &[],
        );
        std::os::unix::fs::symlink(outside.path().join("secret.wav"), pack.path().join("link.wav")).expect("linked");
        let packs = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(
            import(pack.path(), ImportKind::Folder, packs.path()).err(),
            Some(PackError::Missing(SoundEvent::Message))
        );
        let pack = folder("name = \"P\"\n[sounds]\nmention = \"dir/secret.wav\"\n", &[]);
        std::os::unix::fs::symlink(outside.path(), pack.path().join("dir")).expect("linked");
        assert_eq!(
            import(pack.path(), ImportKind::Folder, packs.path()).err(),
            Some(PackError::Missing(SoundEvent::Mention))
        );
    }

    #[test]
    fn bad_manifests_and_sounds_say_why_and_leave_nothing() {
        let packs = tempfile::tempdir().expect("a temporary directory");
        let cases: [Case<'_>; 7] = [
            ("not toml [", vec![], PackError::BadManifest),
            ("[sounds]\nmessage = \"a.wav\"", vec![], PackError::BadName),
            ("name = \"\"\n[sounds]\nmessage = \"a.wav\"", vec![], PackError::BadName),
            (
                "name = \"P\"\nauthor = 3\n[sounds]\nmessage = \"a.wav\"",
                vec![],
                PackError::BadManifest,
            ),
            (
                "name = \"P\"\n[sounds]\nunknown = \"a.wav\"",
                vec![],
                PackError::NoSounds,
            ),
            (
                "name = \"P\"\n[sounds]\nmessage = \"a.wav\"",
                vec![],
                PackError::Missing(SoundEvent::Message),
            ),
            (
                "name = \"P\"\n[sounds]\nmessage = \"a.wav\"",
                vec![("a.wav", b"not a wave".to_vec())],
                PackError::NotSound(SoundEvent::Message),
            ),
        ];
        for (manifest, files, why) in cases {
            let pack = folder(manifest, &files);
            assert_eq!(
                import(pack.path(), ImportKind::Folder, packs.path()).err(),
                Some(why),
                "{manifest}"
            );
        }
        let empty = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(
            import(empty.path(), ImportKind::Folder, packs.path()).err(),
            Some(PackError::NoManifest)
        );
        assert_eq!(fs::read_dir(packs.path()).map_or(0, Iterator::count), 0);
    }

    #[test]
    fn a_sound_too_long_or_too_short_is_refused() {
        let packs = tempfile::tempdir().expect("a temporary directory");
        let manifest = "name = \"P\"\n[sounds]\nmessage = \"a.wav\"";
        let long = folder(manifest, &[("a.wav", wav_bytes(31.0))]);
        assert_eq!(
            import(long.path(), ImportKind::Folder, packs.path()).err(),
            Some(PackError::BadLength(SoundEvent::Message))
        );
        let short = folder(manifest, &[("a.wav", wav_bytes(0.001))]);
        assert_eq!(
            import(short.path(), ImportKind::Folder, packs.path()).err(),
            Some(PackError::BadLength(SoundEvent::Message))
        );
    }

    #[test]
    fn size_limits_hold_by_what_is_read() {
        assert!(read_limited(Cursor::new(vec![0; 11]), 10).is_err());
        assert!(read_limited(Cursor::new(vec![0; 10]), 10).is_ok());
        let mut budget = Budget::default();
        assert_eq!(budget.spend(&vec![0; 1024]), Ok(()));
        budget.read = MAX_TOTAL;
        assert_eq!(budget.spend(&[0]), Err(PackError::PackTooLarge));
    }

    #[test]
    fn an_archive_of_too_many_entries_or_no_manifest_is_refused() {
        let many: Vec<(String, Vec<u8>)> = (0..=MAX_ENTRIES)
            .map(|index| (format!("f{index}"), Vec::new()))
            .collect();
        let named: Vec<(&str, Vec<u8>)> = many
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone()))
            .collect();
        let packs = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(
            imported_zip(zip_of(&named), packs.path()).err(),
            Some(PackError::BadArchive)
        );
        assert_eq!(
            imported_zip(zip_of(&[("a/b/pack.toml", GOOD.into())]), packs.path()).err(),
            Some(PackError::NoManifest)
        );
        assert_eq!(
            imported_zip(
                zip_of(&[("a/pack.toml", GOOD.into()), ("b/pack.toml", GOOD.into())]),
                packs.path()
            )
            .err(),
            Some(PackError::NoManifest)
        );
        assert_eq!(
            imported_zip(b"not a zip".to_vec(), packs.path()).err(),
            Some(PackError::BadArchive)
        );
        let missing = packs.path().join("missing.zip");
        assert_eq!(
            import(&missing, ImportKind::Archive, packs.path()).err(),
            Some(PackError::BadArchive)
        );
    }

    #[test]
    fn a_damaged_sound_reads_as_none_and_a_bad_id_is_refused() {
        let pack = folder(GOOD, &[("sounds/ping.wav", wav_bytes(0.1))]);
        let packs = tempfile::tempdir().expect("a temporary directory");
        import(pack.path(), ImportKind::Folder, packs.path()).expect("imported");
        fs::write(packs.path().join("office/message.wav"), b"garbage").expect("written");
        assert_eq!(read_sound(packs.path(), "office", SoundEvent::Message), None);
        assert_eq!(read_sound(packs.path(), "../office", SoundEvent::Message), None);
        assert_eq!(remove(packs.path(), "../office"), Err(PackError::Storage));
        for good in ["a", "my-pack-2"] {
            assert!(is_id(good));
        }
        for bad in ["", "-a", "A", "a/b", "a.b", &"a".repeat(MAX_ID + 1)] {
            assert!(!is_id(bad), "{bad}");
        }
    }

    #[test]
    fn an_unusual_name_still_gets_a_safe_id() {
        let (id, shown) = free_id("../Ünï Pack!", &[]);
        assert!(is_id(&id), "{id}");
        assert_eq!(shown, "../Ünï Pack!");
        assert_eq!(free_id("!!!", &[]).0, "pack");
    }

    #[test]
    fn repeated_long_names_stay_listed_and_removable() {
        for name in ["x".repeat(MAX_NAME), "é".repeat(MAX_NAME / 2)] {
            let manifest = format!("name = \"{name}\"\n[sounds]\nmessage = \"ping.wav\"\n");
            let pack = folder(&manifest, &[("ping.wav", wav_bytes(0.1))]);
            let packs = tempfile::tempdir().expect("a directory");
            for _ in 0..3 {
                let kept = import(pack.path(), ImportKind::Folder, packs.path()).expect("imported");
                assert!(is_name(&kept.name));
                assert!(list(packs.path()).contains(&kept));
            }
            for kept in list(packs.path()) {
                remove(packs.path(), &kept.id).expect("removed");
            }
            assert!(list(packs.path()).is_empty());
        }
    }

    #[test]
    fn a_partial_write_preserves_the_previous_sound_and_cleans_up() {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("message.wav");
        let original = wav_bytes(0.1);
        fs::write(&path, &original).expect("written");
        let result = replace_sound(&path, |temporary| {
            fs::write(temporary, b"partial WAV").expect("partial write");
            Err(PackError::Storage)
        });
        assert_eq!(result, Err(PackError::Storage));
        assert_eq!(fs::read(&path).expect("old sound"), original);
        assert_eq!(fs::read_dir(dir.path()).expect("directory").count(), 1);
    }

    #[test]
    fn a_single_file_becomes_the_events_own_sound() {
        let packs = tempfile::tempdir().expect("a temporary directory");
        let file = packs.path().join("ring.wav");
        fs::write(&file, wav_bytes(0.25)).expect("written");
        let dir = packs.path().join("packs");
        let one = import_single(&file, SoundEvent::Mention, &dir).expect("imported");
        assert_eq!(one.id, OWN_PACK);
        let two = import_single(&file, SoundEvent::Message, &dir).expect("imported");
        assert_eq!(two.events, vec![SoundEvent::Message, SoundEvent::Mention]);
        assert_eq!(
            read_sound(&dir, OWN_PACK, SoundEvent::Message).map(|sound| sound.len()),
            Some(12_000)
        );
        assert_eq!(list(&dir).len(), 1);
        assert_eq!(free_id("My sounds", &[]).0, "my-sounds-2", "a pack never takes that id");
        fs::write(&file, b"nope").expect("written");
        assert_eq!(
            import_single(&file, SoundEvent::Message, &dir).err(),
            Some(PackError::NotSound(SoundEvent::Message))
        );
        assert_eq!(
            read_sound(&dir, OWN_PACK, SoundEvent::Message).map(|sound| sound.len()),
            Some(12_000)
        );
        assert_eq!(
            import_single(&dir, SoundEvent::Message, &dir).err(),
            Some(PackError::Missing(SoundEvent::Message))
        );
    }

    #[test]
    fn too_many_packs_are_refused() {
        let packs = tempfile::tempdir().expect("a temporary directory");
        for index in 0..MAX_PACKS {
            let dir = packs.path().join(format!("p{index}"));
            fs::create_dir(&dir).expect("made");
            fs::write(
                dir.join(MANIFEST),
                format!("name = \"P{index}\"\n[sounds]\nmessage = \"message.wav\"\n"),
            )
            .expect("written");
        }
        let pack = folder(GOOD, &[("sounds/ping.wav", wav_bytes(0.1))]);
        assert_eq!(
            import(pack.path(), ImportKind::Folder, packs.path()).err(),
            Some(PackError::TooManyPacks)
        );
    }
}
