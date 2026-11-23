//! Imports a folder, a `.zip` or chosen files of pictures as a sticker pack: each PNG, WebP or GIF file, and each
//! Telegram animated sticker (`.tgs`, [`super::lottie`]), becomes a sticker, still or animated, made ready to send. A
//! still sticker is kept (and sent) as a PNG, an animated one (an animated GIF, WebP or PNG, or a `.tgs`) as a GIF,
//! which every Teams client plays. A Telegram video sticker (`.webm`) is skipped and said to be one.
//!
//! - **What is read**: the files in the folder and its folders (at most [`MAX_DEPTH`] deep, no symbolic link), or the
//!   archive's files, in the order of their names. Hidden files and folders (and a Mac archive's `__MACOSX`) are passed
//!   over. A file is told by its bytes, not its name. Each file, the archive, its entry count and the total read are
//!   capped ([`MAX_FILE`], [`MAX_ARCHIVE`], [`MAX_ENTRIES`], [`MAX_TOTAL`]), by what is read rather than what an archive
//!   declares.
//! - **What is kept**: each picture scaled down to [`KEPT_SIDE`] (never up), shown at most [`SHOWN_SIDE`] CSS pixels
//!   on its longer side, as Teams' own stickers. An animated one keeps its frames ([`MAX_FRAMES`] at most) and their
//!   times, and loops; GIF has no partial transparency, so a pixel at least half covered is kept whole and any other is
//!   clear: the edge stays hard in Teams Light and Dark, with no matte of one theme's colour.
//! - **What is skipped**, and why, is told file by file ([`Skipped`]); the others are imported. A pack with no picture
//!   is not made.
//! - Other sources (a Telegram pack's link, [`super::telegram`]) give their files through [`Source`] and are made the
//!   same way.
//!
//! An import builds a hidden directory and renames it into place, so a pack is there whole or not at all.

use std::fs::{self, File};
use std::io::{Cursor, Read as _, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::codecs::png::{PngDecoder, PngEncoder};
use image::codecs::webp::WebPDecoder;
use image::imageops::FilterType;
use image::{AnimationDecoder, Delay, ExtendedColorType, Frame, ImageDecoder, ImageEncoder as _, Limits, RgbaImage};
use teams_proto::PastedFormat;
use zip::ZipArchive;

use super::store::{self, MANIFEST, MAX_LABEL, MAX_NAME, MAX_PACKS, MAX_STICKER, MAX_STICKERS, Pack, Sticker};

/// The longest side a sticker is shown at, in CSS pixels: Teams' stickers are 180 by 180.
pub const SHOWN_SIDE: u32 = 180;
/// The longest side a sticker is kept at, in pixels: Teams' sticker art is 250 by 250.
pub const KEPT_SIDE: u32 = 250;
/// The largest picture file read, in bytes.
pub const MAX_FILE: u64 = 10 * 1024 * 1024;
/// The most frames an animated sticker keeps.
pub const MAX_FRAMES: usize = 200;
/// The longest side of a picture read, in pixels.
pub const MAX_SOURCE_SIDE: u32 = 4096;
/// The largest archive opened, in bytes.
pub const MAX_ARCHIVE: u64 = 200 * 1024 * 1024;
/// The most entries an archive may hold, and the most files a folder's import reads.
pub const MAX_ENTRIES: usize = 1000;
/// The most read of one import, in bytes.
pub const MAX_TOTAL: u64 = 256 * 1024 * 1024;
/// How deep in a folder's folders files are read.
pub const MAX_DEPTH: usize = 4;
/// A sticker's label when its file's name gives none.
pub const DEFAULT_LABEL: &str = "Sticker";
/// A pack's name when its folder's or archive's name gives none.
pub const DEFAULT_PACK_NAME: &str = "My stickers";

/// The memory a picture's decoder may take, in bytes.
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;
/// A frame's shortest time, in milliseconds: browsers slow a faster GIF frame to a tenth of a second.
const MIN_DELAY_MS: u32 = 20;
/// A frame's time when its file gives none, in milliseconds.
const DEFAULT_DELAY_MS: u32 = 100;
/// The GIF encoder's speed (1 best, 30 fastest): good colours at a sticker's size.
const GIF_SPEED: i32 = 10;

/// How the user handed the pictures over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportKind {
    Folder,
    Archive,
}

/// Why a file wasn't made a sticker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Skip {
    #[error("isn't a PNG, WebP or GIF picture or a Telegram sticker")]
    NotPicture,
    #[error("is a video sticker, which can't be imported yet")]
    Video,
    #[error("is an animated Telegram sticker, which this build of the app can't import")]
    NoRenderer,
    #[error("uses an animation feature that can't be imported")]
    Unsupported,
    #[error("couldn't be downloaded")]
    NotDownloaded,
    #[error("is larger than 10 MB")]
    TooLarge,
    #[error("is larger than 8 MB as a sticker")]
    TooLargeSticker,
    #[error("is larger than {MAX_SOURCE_SIDE} pixels")]
    TooBig,
    #[error("has more than {MAX_FRAMES} frames")]
    TooManyFrames,
    #[error("can't be read")]
    Unreadable,
    #[error("is past the {MAX_STICKERS} stickers a pack holds")]
    PackFull,
}

/// A file that wasn't made a sticker: its name (its last part), and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub why: Skip,
}

impl std::fmt::Display for Skipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.name, self.why)
    }
}

/// Why nothing was imported, in the user's words.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("That folder can't be read.")]
    NoFolder,
    #[error("That file can't be read as a .zip archive of up to 200 MB and 1000 files.")]
    BadArchive,
    #[error("No PNG, WebP or GIF picture or Telegram sticker there could be made a sticker.")]
    NoStickers(Vec<Skipped>),
    #[error("Those pictures are larger than 256 MB together.")]
    TooMuch,
    #[error("There are too many sticker packs. Remove one first.")]
    TooManyPacks,
    #[error("The stickers can't be saved on this computer.")]
    Storage,
    #[error("That isn't a link to a Telegram sticker pack. It looks like t.me/addstickers/…")]
    BadLink,
    #[error("Add a Telegram bot token first.")]
    NoToken,
    #[error("The Telegram bot token can't be read from this computer's keyring.")]
    TokenUnreadable,
    #[error("Telegram refused the bot token. Check it, or replace it.")]
    BadToken,
    #[error("Telegram has no sticker pack at that link.")]
    NoSuchPack,
    #[error("Telegram asks to wait. Try again in a minute.")]
    TelegramBusy,
    #[error("Telegram can't be reached. Check your connection and try again.")]
    TelegramDown,
    #[error("Telegram's answer can't be read. Try again later.")]
    TelegramFailed,
}

/// How far an import is: `done` of `total` files read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// What an import made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub pack: Pack,
    pub skipped: Vec<Skipped>,
}

/// Where an import's files come from: a folder, an archive, chosen files or a Telegram pack.
pub trait Source {
    /// The pack's name it suggests.
    fn name(&self) -> String;
    /// How many files it has.
    fn len(&self) -> usize;
    /// Whether it has none.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The name of file `index` (its last part), as a skip names it, and its label unless [`Self::label`] gives one.
    fn file_name(&self, index: usize) -> String;
    /// The label of file `index`'s sticker, when the source knows a better one than its file's name gives.
    fn label(&self, _index: usize) -> Option<String> {
        None
    }
    /// The bytes of file `index`, at most [`MAX_FILE`].
    ///
    /// # Errors
    /// It is too large or can't be read, or (a Telegram pack's) it is a video or couldn't be downloaded.
    fn read(&mut self, index: usize) -> Result<Vec<u8>, Skip>;
    /// Why the source can give no more files, after a read that it stopped on; the import then keeps nothing.
    fn stopped(&mut self) -> Option<ImportError> {
        None
    }
}

/// The last part of a `/`-separated name.
fn last_part(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Whether a path's part is hidden (or a Mac archive's resource folder): passed over.
fn hidden(part: &str) -> bool {
    part.starts_with('.') || part == "__MACOSX"
}

/// Reads at most [`MAX_FILE`] bytes of `reader`.
fn read_limited(reader: impl std::io::Read) -> Result<Vec<u8>, Skip> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_FILE.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| Skip::Unreadable)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE {
        return Err(Skip::TooLarge);
    }
    Ok(bytes)
}

/// A folder's files.
pub struct Folder {
    name: String,
    files: Vec<PathBuf>,
}

impl Folder {
    /// The files of the folder at `root` (see the module docs).
    ///
    /// # Errors
    /// It isn't a folder that can be read.
    pub fn open(root: &Path) -> Result<Self, ImportError> {
        let root = fs::canonicalize(root).map_err(|_| ImportError::NoFolder)?;
        if !root.is_dir() {
            return Err(ImportError::NoFolder);
        }
        let mut files = Vec::new();
        walk(&root, 0, &mut files).map_err(|()| ImportError::NoFolder)?;
        files.sort();
        files.truncate(MAX_ENTRIES);
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        Ok(Self { name, files })
    }
}

/// Adds the plain files under `dir` to `files`, depth first, at most [`MAX_DEPTH`] deep.
fn walk(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) -> Result<(), ()> {
    let entries = fs::read_dir(dir).map_err(|_| ())?;
    for entry in entries.flatten() {
        if files.len() > MAX_ENTRIES {
            return Ok(());
        }
        let name = entry.file_name();
        if name.to_str().is_none_or(hidden) {
            continue;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_file() {
            files.push(entry.path());
        } else if kind.is_dir() && depth < MAX_DEPTH {
            // a folder that can't be read is passed over, as its files would be
            let _ = walk(&entry.path(), depth + 1, files);
        }
    }
    Ok(())
}

impl Source for Folder {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn len(&self) -> usize {
        self.files.len()
    }

    fn file_name(&self, index: usize) -> String {
        file_name_of(self.files.get(index))
    }

    fn read(&mut self, index: usize) -> Result<Vec<u8>, Skip> {
        read_file(self.files.get(index).ok_or(Skip::Unreadable)?)
    }
}

/// Files the user chose, one by one.
pub struct Files {
    name: String,
    files: Vec<PathBuf>,
}

impl Files {
    /// `files` (at most [`MAX_ENTRIES`]), as a pack named after the folder of the first.
    #[must_use]
    pub fn new(mut files: Vec<PathBuf>) -> Self {
        files.sort();
        files.dedup();
        files.truncate(MAX_ENTRIES);
        let name = files
            .first()
            .and_then(|file| file.parent())
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        Self { name, files }
    }
}

impl Source for Files {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn len(&self) -> usize {
        self.files.len()
    }

    fn file_name(&self, index: usize) -> String {
        file_name_of(self.files.get(index))
    }

    fn read(&mut self, index: usize) -> Result<Vec<u8>, Skip> {
        read_file(self.files.get(index).ok_or(Skip::Unreadable)?)
    }
}

/// The last part of `path`, or nothing.
fn file_name_of(path: Option<&PathBuf>) -> String {
    path.and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The bytes of the plain file at `path` (not a link), at most [`MAX_FILE`].
fn read_file(path: &Path) -> Result<Vec<u8>, Skip> {
    let metadata = fs::symlink_metadata(path).map_err(|_| Skip::Unreadable)?;
    if !metadata.is_file() {
        return Err(Skip::Unreadable);
    }
    if metadata.len() > MAX_FILE {
        return Err(Skip::TooLarge);
    }
    read_limited(File::open(path).map_err(|_| Skip::Unreadable)?)
}

/// An archive's files.
pub struct Archive<R> {
    name: String,
    zip: ZipArchive<R>,
    /// The entries read, by their index in the archive, in the order of their names.
    files: Vec<(usize, String)>,
}

impl Archive<File> {
    /// The files of the `.zip` at `path` (see the module docs).
    ///
    /// # Errors
    /// It isn't an archive that can be read, or it is too large.
    pub fn open(path: &Path) -> Result<Self, ImportError> {
        let metadata = fs::metadata(path).map_err(|_| ImportError::BadArchive)?;
        if !metadata.is_file() || metadata.len() > MAX_ARCHIVE {
            return Err(ImportError::BadArchive);
        }
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        Self::over(name, File::open(path).map_err(|_| ImportError::BadArchive)?)
    }
}

impl<R: std::io::Read + Seek> Archive<R> {
    /// The files of the archive `reader` holds, as pack `name`.
    ///
    /// # Errors
    /// It isn't an archive that can be read, or it has too many entries.
    pub fn over(name: String, reader: R) -> Result<Self, ImportError> {
        let zip = ZipArchive::new(reader).map_err(|_| ImportError::BadArchive)?;
        if zip.len() > MAX_ENTRIES {
            return Err(ImportError::BadArchive);
        }
        let mut files: Vec<(usize, String)> = (0..zip.len())
            .filter_map(|index| {
                let entry = zip.name_for_index(index)?.ok()?;
                let plain = !entry.ends_with('/') && !entry.split(['/', '\\']).any(hidden);
                plain.then(|| (index, entry.into_owned()))
            })
            .collect();
        files.sort_by(|one, other| one.1.cmp(&other.1));
        Ok(Self { name, zip, files })
    }
}

impl<R: std::io::Read + Seek> Source for Archive<R> {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn len(&self) -> usize {
        self.files.len()
    }

    fn file_name(&self, index: usize) -> String {
        self.files
            .get(index)
            .map(|(_, name)| last_part(name).to_owned())
            .unwrap_or_default()
    }

    fn read(&mut self, index: usize) -> Result<Vec<u8>, Skip> {
        let (at, _) = self.files.get(index).ok_or(Skip::Unreadable)?;
        let file = self.zip.by_index(*at).map_err(|_| Skip::Unreadable)?;
        if !file.is_file() {
            return Err(Skip::Unreadable);
        }
        if file.size() > MAX_FILE {
            return Err(Skip::TooLarge);
        }
        read_limited(file)
    }
}

/// A picture made ready to send as a sticker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Converted {
    /// A PNG (still) or a GIF (animated).
    pub bytes: Vec<u8>,
    pub animated: bool,
    /// Its size in pixels.
    pub own: (u32, u32),
    /// The size it is shown at, in CSS pixels.
    pub shown: (u32, u32),
}

/// A picture's frames, each with its time in milliseconds, scaled down to [`KEPT_SIDE`].
struct Frames {
    frames: Vec<(RgbaImage, u32)>,
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_SIDE);
    limits.max_image_height = Some(MAX_SOURCE_SIDE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    limits
}

/// Why a decoder failed, as a skip: a picture too large for the limits, or one that doesn't decode.
fn decode_skip(error: &image::ImageError) -> Skip {
    match error {
        image::ImageError::Limits(_) => Skip::TooBig,
        _ => Skip::Unreadable,
    }
}

/// `side` scaled by `scale`, at least 1.
fn scaled(side: u32, scale: f64) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a side is at most MAX_SOURCE_SIDE, scaled by at most 1"
    )]
    let side = (f64::from(side) * scale).round() as u32;
    side.max(1)
}

/// `own` scaled down (never up) to fit `most` on its longer side.
#[must_use]
pub fn fit(own: (u32, u32), most: u32) -> (u32, u32) {
    let longer = own.0.max(own.1).max(1);
    if longer <= most {
        return (own.0.max(1), own.1.max(1));
    }
    let scale = f64::from(most) / f64::from(longer);
    (scaled(own.0, scale), scaled(own.1, scale))
}

/// `frame` scaled down to [`KEPT_SIDE`].
fn kept(frame: RgbaImage) -> RgbaImage {
    let (width, height) = fit(frame.dimensions(), KEPT_SIDE);
    if (width, height) == frame.dimensions() {
        frame
    } else {
        image::imageops::resize(&frame, width, height, FilterType::Triangle)
    }
}

/// The frames an animation decoder gives, at most [`MAX_FRAMES`].
fn animation<'a>(decoder: impl AnimationDecoder<'a>) -> Result<Frames, Skip> {
    let mut frames = Vec::new();
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|error| decode_skip(&error))?;
        if frames.len() == MAX_FRAMES {
            return Err(Skip::TooManyFrames);
        }
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let ms = if denominator == 0 || numerator == 0 {
            DEFAULT_DELAY_MS
        } else {
            (numerator / denominator).max(MIN_DELAY_MS)
        };
        frames.push((kept(frame.into_buffer()), ms));
    }
    if frames.is_empty() {
        return Err(Skip::Unreadable);
    }
    Ok(Frames { frames })
}

/// The one frame a still decoder gives.
fn still(decoder: impl ImageDecoder) -> Result<Frames, Skip> {
    let image = image::DynamicImage::from_decoder(decoder).map_err(|error| decode_skip(&error))?;
    Ok(Frames {
        frames: vec![(kept(image.into_rgba8()), 0)],
    })
}

/// Whether `bytes` are a Matroska (`.webm`) file: a Telegram video sticker.
fn is_video(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
}

/// The frames of a PNG (an animated one's too), WebP or GIF picture, or of a Telegram animated sticker.
fn frames(bytes: &[u8]) -> Result<Frames, Skip> {
    if super::lottie::is_tgs(bytes) {
        return Ok(Frames {
            frames: super::lottie::frames(bytes, super::lottie::renderer())?,
        });
    }
    if is_video(bytes) {
        return Err(Skip::Video);
    }
    let format = PastedFormat::sniff(bytes).ok_or(Skip::NotPicture)?;
    let skip = |error: image::ImageError| decode_skip(&error);
    match format {
        PastedFormat::Jpeg => Err(Skip::NotPicture),
        PastedFormat::Png => {
            let mut decoder = PngDecoder::new(Cursor::new(bytes)).map_err(skip)?;
            decoder.set_limits(limits()).map_err(skip)?;
            if decoder.is_apng().map_err(skip)? {
                animation(decoder.apng().map_err(skip)?)
            } else {
                still(decoder)
            }
        }
        PastedFormat::Gif => {
            let mut decoder = GifDecoder::new(Cursor::new(bytes)).map_err(skip)?;
            decoder.set_limits(limits()).map_err(skip)?;
            animation(decoder)
        }
        PastedFormat::Webp => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(skip)?;
            decoder.set_limits(limits()).map_err(skip)?;
            if decoder.has_animation() {
                animation(decoder)
            } else {
                still(decoder)
            }
        }
    }
}

/// A pixel at least half covered made whole, any other clear: GIF's one bit of transparency.
fn hard_alpha(frame: &mut RgbaImage) {
    for pixel in frame.pixels_mut() {
        pixel.0 = if pixel.0[3] >= 128 {
            [pixel.0[0], pixel.0[1], pixel.0[2], 255]
        } else {
            [0, 0, 0, 0]
        };
    }
}

fn encode_png(frame: &RgbaImage) -> Result<Vec<u8>, Skip> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(frame.as_raw(), frame.width(), frame.height(), ExtendedColorType::Rgba8)
        .map_err(|_| Skip::Unreadable)?;
    Ok(bytes)
}

fn encode_gif(frames: Vec<(RgbaImage, u32)>) -> Result<Vec<u8>, Skip> {
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new_with_speed(&mut bytes, GIF_SPEED);
        encoder.set_repeat(Repeat::Infinite).map_err(|_| Skip::Unreadable)?;
        for (mut frame, ms) in frames {
            hard_alpha(&mut frame);
            encoder
                .encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(ms, 1)))
                .map_err(|_| Skip::Unreadable)?;
        }
    }
    Ok(bytes)
}

/// The picture `bytes` hold made a sticker (see the module docs).
///
/// # Errors
/// It isn't a picture taken ([`Skip`]), or it is too large once made.
pub fn convert(bytes: &[u8]) -> Result<Converted, Skip> {
    let Frames { mut frames } = frames(bytes)?;
    let own = frames.first().map_or((1, 1), |(frame, _)| frame.dimensions());
    // every frame of a file is the size of its canvas
    let animated = frames.len() > 1;
    let bytes = if animated {
        encode_gif(frames)?
    } else {
        let (frame, _) = frames.pop().ok_or(Skip::Unreadable)?;
        encode_png(&frame)?
    };
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_STICKER {
        return Err(Skip::TooLargeSticker);
    }
    Ok(Converted {
        bytes,
        animated,
        own,
        shown: fit(own, SHOWN_SIDE),
    })
}

/// A sticker's label made from its file's `name`: its stem, `_` and `-` as spaces, the first letter a capital.
#[must_use]
pub fn label_of(name: &str) -> String {
    let stem = last_part(name);
    let stem = stem.rsplit_once('.').map_or(stem, |(stem, _)| stem);
    let words: Vec<&str> = stem.split(['_', '-', ' ']).filter(|word| !word.is_empty()).collect();
    let mut label: String = words
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL)
        .collect();
    if let Some(first) = label.chars().next() {
        let upper: String = first.to_uppercase().collect();
        label.replace_range(..first.len_utf8(), &upper);
    }
    store::clean_name(&label, MAX_LABEL).unwrap_or_else(|| DEFAULT_LABEL.to_owned())
}

/// An id for a pack called `name`, free among `taken`, and the name it goes by (numbered when another has it).
fn free_id(name: &str, taken: &[Pack]) -> (String, String) {
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
    let slug: String = slug.trim_matches('-').chars().take(store::MAX_ID - 8).collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "pack" } else { slug };
    let free = |id: &str, shown: &str| !taken.iter().any(|pack| pack.id == id || pack.name == shown);
    if free(slug, name) {
        return (slug.to_owned(), name.to_owned());
    }
    let mut number = 2;
    loop {
        let suffix = format!(" ({number})");
        let room = MAX_NAME.saturating_sub(suffix.chars().count());
        let shown: String = name.chars().take(room).collect();
        let (id, shown) = (format!("{slug}-{number}"), format!("{shown}{suffix}"));
        if free(&id, &shown) {
            return (id, shown);
        }
        number += 1;
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

/// The counter that keeps two imports' hidden directories apart.
static IMPORTS: AtomicU64 = AtomicU64::new(0);

/// Imports the folder or archive `source` into `dir`, telling `progress` after each file.
///
/// # Errors
/// Nothing was imported ([`ImportError`]); nothing is left behind.
pub fn import(
    source: &Path,
    kind: ImportKind,
    dir: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Imported, ImportError> {
    match kind {
        ImportKind::Folder => import_from(&mut Folder::open(source)?, dir, progress),
        ImportKind::Archive => import_from(&mut Archive::open(source)?, dir, progress),
    }
}

/// Imports the files of `source` into `dir` as a new pack, telling `progress` after each file.
///
/// # Errors
/// Nothing was imported ([`ImportError`]); nothing is left behind.
pub fn import_from(
    source: &mut dyn Source,
    dir: &Path,
    progress: &mut dyn FnMut(Progress),
) -> Result<Imported, ImportError> {
    let total = source.len();
    let mut read = 0_u64;
    let mut made: Vec<(Sticker, Converted)> = Vec::new();
    let mut skipped = Vec::new();
    progress(Progress { done: 0, total });
    for index in 0..total {
        let name = source.file_name(index);
        let outcome = if made.len() == MAX_STICKERS {
            Err(Skip::PackFull)
        } else {
            match source.read(index) {
                Ok(bytes) => {
                    read = read.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                    if read > MAX_TOTAL {
                        return Err(ImportError::TooMuch);
                    }
                    convert(&bytes)
                }
                Err(why) => Err(why),
            }
        };
        if let Some(error) = source.stopped() {
            return Err(error);
        }
        match outcome {
            Ok(converted) => {
                let extension = if converted.animated { "gif" } else { "png" };
                let label = source
                    .label(index)
                    .and_then(|label| store::clean_name(&label, MAX_LABEL))
                    .unwrap_or_else(|| label_of(&name));
                let sticker = Sticker {
                    file: format!("{:03}.{extension}", made.len() + 1),
                    label,
                    size: converted.shown,
                };
                made.push((sticker, converted));
            }
            Err(why) => skipped.push(Skipped { name, why }),
        }
        progress(Progress { done: index + 1, total });
    }
    if made.is_empty() {
        return Err(ImportError::NoStickers(skipped));
    }
    let pack = keep(&source.name(), made, dir)?;
    Ok(Imported { pack, skipped })
}

/// Writes `made` into `dir` as a new pack called `name`.
fn keep(name: &str, made: Vec<(Sticker, Converted)>, dir: &Path) -> Result<Pack, ImportError> {
    teams_platform::private::create_dir_all(dir).map_err(|_| ImportError::Storage)?;
    let taken = store::list(dir);
    if taken.len() >= MAX_PACKS {
        return Err(ImportError::TooManyPacks);
    }
    let name = store::clean_name(name, MAX_NAME).unwrap_or_else(|| {
        let cut: String = name.trim().chars().take(MAX_NAME).collect();
        store::clean_name(&cut, MAX_NAME).unwrap_or_else(|| DEFAULT_PACK_NAME.to_owned())
    });
    let (id, name) = free_id(&name, &taken);
    let number = IMPORTS.fetch_add(1, Ordering::Relaxed);
    let mut building = Building {
        path: dir.join(format!(".import-{}-{number}", std::process::id())),
        done: false,
    };
    teams_platform::private::create_dir_all(&building.path).map_err(|_| ImportError::Storage)?;
    let mut stickers = Vec::with_capacity(made.len());
    for (sticker, converted) in made {
        crate::settings::write_private(&building.path.join(&sticker.file), &converted.bytes)
            .map_err(|_| ImportError::Storage)?;
        stickers.push(sticker);
    }
    let added = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    crate::settings::write_private(
        &building.path.join(MANIFEST),
        store::manifest_text(&name, added, &stickers).as_bytes(),
    )
    .map_err(|_| ImportError::Storage)?;
    fs::rename(&building.path, dir.join(&id)).map_err(|_| ImportError::Storage)?;
    building.done = true;
    Ok(Pack {
        id,
        name,
        added,
        stickers,
    })
}
