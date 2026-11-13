//! A desktop banner's picture (#114), as Teams' banners show one: the sender's or the caller's picture for a banner
//! about a person, cropped to a circle, and the app's tile for an app's Activity item, square.
//!
//! - Only pictures the engine already has are used ([`Core::person`](crate::Core::person)'s stored one, the app tile
//!   the engine keeps or caches): a banner never waits on a network read. Without one the banner shows as before, with
//!   the app's icon alone; no initials are drawn (Linux and macOS take only a picture, and drawing text into one
//!   needs a font rasterizer the app doesn't link outside GPUI).
//! - The bytes are decoded within an app tile's limits ([`crate::inline`]) and scaled to at most [`BANNER_SIDE`]
//!   off the main thread, then handed to the platform as pixels (Linux: the `image-data` hint) or as a PNG file the
//!   app writes ([`BannerFiles`]: Windows' toast and macOS' attachment read a file).
//! - A banner file lives only in the app's own data directory, named by this run's notification key alone (never a
//!   chat, a person or text), and is removed with its banner, at the quit and at the next start.

use std::fmt::{self, Write as _};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use image::{GenericImageView as _, RgbaImage};
use teams_core::PictureBytes;
use teams_proto::{PastedFormat, PastedPicture};
use teams_store::StoredPicture;

use crate::inline::decode_still_rgba;

/// The longest side a banner's picture is handed at: Teams' toasts draw theirs at 48 px, twice that for a scaled
/// display.
pub const BANNER_SIDE: u32 = 96;

/// The folder under the app's data directory that holds the banner files.
pub const BANNER_DIR: &str = "banner-pictures";

/// How a banner's picture is shaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PictureShape {
    /// A person's: a circle, as Teams crops one.
    Circle,
    /// An app's tile: as drawn.
    Square,
}

/// A banner's picture: RGBA pixels, at most [`BANNER_SIDE`] a side. Its `Debug` shows its size and shape only.
#[derive(Clone, PartialEq, Eq)]
pub struct BannerPicture {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
    shape: PictureShape,
}

impl fmt::Debug for BannerPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BannerPicture({}x{} {:?})", self.width, self.height, self.shape)
    }
}

impl BannerPicture {
    /// A person's stored picture, cropped to the circle in its middle; `None` when it doesn't decode.
    #[must_use]
    pub fn of_person(picture: &StoredPicture) -> Option<Self> {
        let format = PastedFormat::sniff(&picture.bytes)?;
        let bytes = Arc::new(PastedPicture {
            format,
            bytes: picture.bytes.clone(),
        });
        // squared before it is brought down to the banner's side, so a wide one still fills it
        let mut rgba = square(decode_still_rgba(&bytes, BANNER_SIDE * 4)?);
        if rgba.width() > BANNER_SIDE {
            rgba = image::imageops::resize(&rgba, BANNER_SIDE, BANNER_SIDE, image::imageops::FilterType::Triangle);
        }
        circle(&mut rgba);
        Some(Self::from_rgba(rgba, PictureShape::Circle))
    }

    /// An app's tile, as drawn; `None` when it doesn't decode.
    #[must_use]
    pub fn of_app(picture: &PictureBytes) -> Option<Self> {
        let rgba = decode_still_rgba(picture, BANNER_SIDE)?;
        Some(Self::from_rgba(rgba, PictureShape::Square))
    }

    fn from_rgba(rgba: RgbaImage, shape: PictureShape) -> Self {
        Self {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw().into(),
            shape,
        }
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixels, row after row, four bytes (R, G, B, A) each.
    #[must_use]
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    #[must_use]
    pub fn shape(&self) -> PictureShape {
        self.shape
    }

    /// The picture as a PNG file's bytes.
    #[must_use]
    pub fn png(&self) -> Option<Vec<u8>> {
        let mut png = Vec::new();
        image::write_buffer_with_format(
            &mut std::io::Cursor::new(&mut png),
            &self.rgba,
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .ok()?;
        Some(png)
    }
}

/// The square in the middle of `rgba`.
fn square(rgba: RgbaImage) -> RgbaImage {
    let (width, height) = rgba.dimensions();
    if width == height {
        return rgba;
    }
    let side = width.min(height);
    rgba.view((width - side) / 2, (height - side) / 2, side, side)
        .to_image()
}

/// Clears what lies outside the circle inscribed in `rgba`, its edge blended over one pixel.
fn circle(rgba: &mut RgbaImage) {
    let (width, height) = rgba.dimensions();
    let radius = f64::from(width.min(height)) / 2.0;
    let (cx, cy) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
    for (x, y, pixel) in rgba.enumerate_pixels_mut() {
        let distance = (f64::from(x) + 0.5 - cx).hypot(f64::from(y) + 0.5 - cy);
        let cover = (radius - distance + 0.5).clamp(0.0, 1.0);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "an alpha times a cover in 0..=1, rounded: always 0..=255"
        )]
        let alpha = (f64::from(pixel[3]) * cover).round() as u8;
        pixel[3] = alpha;
    }
}

/// The banner files the app hands Windows' toasts and macOS' attachments (see the module docs): a folder of the app's
/// data directory only the app's user reaches, each file named by a banner's key.
#[derive(Clone, Debug)]
pub struct BannerFiles {
    dir: PathBuf,
}

impl BannerFiles {
    /// The folder [`BANNER_DIR`] under `data_dir`, made when missing, with whatever an earlier run left there removed;
    /// `None` when it can't be made (banners then show without pictures).
    #[must_use]
    pub fn open(data_dir: &Path) -> Option<Self> {
        let dir = data_dir.join(BANNER_DIR);
        let made = fs::create_dir_all(&dir);
        if let Err(error) = made {
            log::info!("banner pictures are off this run: {error}");
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        }
        let files = Self { dir };
        files.clear();
        Some(files)
    }

    /// The folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes `picture` as the file of banner `key` (letters, digits and `-` only, at most 128 of them); its path, or
    /// `None` when the key isn't one or the file couldn't be written.
    #[must_use]
    pub fn write(&self, key: &str, picture: &BannerPicture) -> Option<PathBuf> {
        let path = self.path(key)?;
        let png = picture.png()?;
        match fs::write(&path, png) {
            Ok(()) => Some(path),
            Err(error) => {
                log::info!("a banner's picture couldn't be written: {error}");
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    /// Removes banner `key`'s file; one already gone is no matter.
    pub fn remove(&self, key: &str) {
        if let Some(path) = self.path(key) {
            let _ = fs::remove_file(path);
        }
    }

    /// Removes every banner file in the folder (only ours: `.png` files of a key's name).
    pub fn clear(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let ours = name
                .to_str()
                .and_then(|name| name.strip_suffix(".png"))
                .is_some_and(valid_key);
            if ours && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        valid_key(key).then(|| self.dir.join(format!("{key}.png")))
    }
}

/// `path` as a `file:` URI, every byte but an unreserved one, `/` and a drive's `:` percent-encoded; a Windows path's
/// `\` turned to `/`.
#[must_use]
pub fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from(if path.starts_with('/') { "file://" } else { "file:///" });
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            uri.push(char::from(byte));
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri
}

/// A Windows toast's element for the picture at `path`: in the app logo's place, a person's cropped to a circle as
/// Teams' toasts show one.
#[must_use]
pub fn toast_image(path: &Path, shape: PictureShape) -> String {
    let crop = match shape {
        PictureShape::Circle => r#" hint-crop="circle""#,
        PictureShape::Square => "",
    };
    format!(r#"<image placement="appLogoOverride"{crop} src="{}"/>"#, file_uri(path))
}

/// `key` with every character a banner file's name can't hold left out.
#[must_use]
pub fn file_key(key: &str) -> String {
    key.chars()
        .filter(|char| char.is_ascii_alphanumeric() || *char == '-')
        .take(128)
        .collect()
}

/// Whether `key` is a banner file's name: letters, digits and `-`, 1 to 128 of them.
fn valid_key(key: &str) -> bool {
    (1..=128).contains(&key.len()) && key.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(rgba: &RgbaImage) -> Vec<u8> {
        let mut png = Vec::new();
        rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encoded");
        png
    }

    fn stored(rgba: &RgbaImage) -> StoredPicture {
        StoredPicture {
            media_type: "image/png".to_owned(),
            bytes: png_of(rgba),
        }
    }

    #[test]
    fn a_person_s_picture_is_a_circle_at_most_the_banner_side() {
        let photo = RgbaImage::from_pixel(240, 200, image::Rgba([200, 30, 30, 255]));
        let picture = BannerPicture::of_person(&stored(&photo)).expect("decoded");
        assert_eq!((picture.width(), picture.height()), (BANNER_SIDE, BANNER_SIDE));
        assert_eq!(picture.shape(), PictureShape::Circle);
        let alpha = |x: u32, y: u32| picture.rgba()[((y * picture.width() + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "a corner is outside the circle");
        assert_eq!(alpha(BANNER_SIDE - 1, BANNER_SIDE - 1), 0);
        assert_eq!(alpha(BANNER_SIDE / 2, BANNER_SIDE / 2), 255, "the middle is kept");
        assert_eq!(alpha(BANNER_SIDE / 2, 1), 255, "the circle reaches the edge");
        assert_eq!(&picture.rgba()[..3], &[200, 30, 30]);
        assert_eq!(format!("{picture:?}"), "BannerPicture(96x96 Circle)");
    }

    #[test]
    fn an_app_s_tile_stays_square_and_a_small_one_keeps_its_size() {
        let tile = RgbaImage::from_pixel(40, 40, image::Rgba([10, 20, 30, 255]));
        let bytes = Arc::new(PastedPicture {
            format: PastedFormat::Png,
            bytes: png_of(&tile),
        });
        let picture = BannerPicture::of_app(&bytes).expect("decoded");
        assert_eq!(
            (picture.width(), picture.height(), picture.shape()),
            (40, 40, PictureShape::Square)
        );
        assert!(picture.rgba().chunks(4).all(|pixel| pixel == [10, 20, 30, 255]));
    }

    #[test]
    fn what_doesn_t_decode_has_no_picture() {
        let junk = StoredPicture {
            media_type: "image/jpeg".to_owned(),
            bytes: b"not a picture".to_vec(),
        };
        assert!(BannerPicture::of_person(&junk).is_none());
        let truncated = StoredPicture {
            media_type: "image/png".to_owned(),
            bytes: b"\x89PNG\r\n\x1a\nmade-up".to_vec(),
        };
        assert!(BannerPicture::of_person(&truncated).is_none());
        let gif = Arc::new(PastedPicture {
            format: PastedFormat::Gif,
            bytes: b"GIF89a".to_vec(),
        });
        assert!(BannerPicture::of_app(&gif).is_none());
    }

    #[test]
    fn a_picture_round_trips_through_its_png() {
        let tile = image::imageops::resize(
            &RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255])),
            16,
            16,
            image::imageops::FilterType::Triangle,
        );
        let bytes = Arc::new(PastedPicture {
            format: PastedFormat::Png,
            bytes: png_of(&tile),
        });
        let picture = BannerPicture::of_app(&bytes).expect("decoded");
        let png = picture.png().expect("encoded");
        let again = image::load_from_memory(&png).expect("a PNG").into_rgba8();
        assert_eq!(again.dimensions(), (16, 16));
        assert_eq!(again.into_raw(), picture.rgba());
    }

    #[test]
    fn banner_files_are_named_by_key_only_and_cleaned_up() {
        let data = tempfile::tempdir().expect("temp dir");
        let picture = BannerPicture::from_rgba(
            RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255])),
            PictureShape::Square,
        );
        let files = BannerFiles::open(data.path()).expect("opened");
        assert_eq!(files.dir(), data.path().join(BANNER_DIR));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(files.dir()).expect("made").permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let written = files.write("run-1-2", &picture).expect("written");
        assert_eq!(written, files.dir().join("run-1-2.png"));
        assert!(image::open(&written).is_ok());
        for refused in ["", "../escape", "a/b", "a.b", "name with space", &"x".repeat(129)] {
            assert!(files.write(refused, &picture).is_none(), "{refused:?}");
        }
        files.remove("run-1-2");
        assert!(!written.exists());
        files.remove("run-1-2");

        // what an earlier run left is removed at the next start; anything else stays
        let left = files.write("run-0-9", &picture).expect("written");
        let other = files.dir().join("keep.txt");
        fs::write(&other, b"not ours").expect("written");
        let again = BannerFiles::open(data.path()).expect("opened");
        assert!(!left.exists());
        assert!(other.exists());
        again.write("run-2-1", &picture).expect("written");
        again.clear();
        assert_eq!(fs::read_dir(again.dir()).expect("listed").count(), 1);
    }

    #[test]
    fn a_toast_names_its_picture_by_a_file_uri() {
        assert_eq!(
            file_uri(Path::new(
                r"C:\Users\Ada Lab\AppData\Local\ngteams\data\banner-pictures\run-1-2.png"
            )),
            "file:///C:/Users/Ada%20Lab/AppData/Local/ngteams/data/banner-pictures/run-1-2.png"
        );
        assert_eq!(
            file_uri(Path::new("/home/a/x&y\"<z>.png")),
            "file:///home/a/x%26y%22%3Cz%3E.png"
        );
        assert_eq!(
            toast_image(Path::new("/d/run-1.png"), PictureShape::Circle),
            r#"<image placement="appLogoOverride" hint-crop="circle" src="file:///d/run-1.png"/>"#
        );
        assert_eq!(
            toast_image(Path::new("/d/run-1.png"), PictureShape::Square),
            r#"<image placement="appLogoOverride" src="file:///d/run-1.png"/>"#
        );
        assert_eq!(file_key("run-{6B29FC40-CA47}-3-4"), "run-6B29FC40-CA47-3-4");
        assert_eq!(file_key(&"x".repeat(200)).len(), 128);
    }

    #[test]
    fn without_a_folder_there_are_no_banner_files() {
        let data = tempfile::tempdir().expect("temp dir");
        let blocked = data.path().join("file");
        fs::write(&blocked, b"").expect("written");
        assert!(BannerFiles::open(&blocked).is_none());
    }
}
