//! The camera's background effects, as Teams offers them: none, the background blurred, or a picture in its place
//! (one of the app's own, [`STOCK`], or one the user added). Each camera frame is cut into the person and the rest by
//! a person segmentation model ([`segment`]), then the rest is blurred or replaced ([`compose`]), before the frame is
//! shown in the self view and sent.
//!
//! - **The choice** ([`BackgroundChoice`]) is shared: the app sets it (the settings' video effects), and the camera's
//!   thread reads it at each frame, so a change shows on the next frame. Choosing an effect never turns a camera on:
//!   it only changes the frames of a camera something else turned on.
//! - **The model** is Google's `MediaPipe` selfie segmenter (landscape, 256 by 144; Apache-2.0), converted to ONNX
//!   once, at development time (`tools/selfie-segmenter/convert.py`), and built into the app. It runs on a thread of
//!   its own beside the camera's ([`segment`]: the camera's thread never waits for it, and takes its newest mask): on
//!   the GPU where there is one ([`gpu`]: Burn on wgpu, from code generated from the ONNX at build time), else on the
//!   CPU (tract), loaded once per process on the first frame that needs it. Until its first mask, and when it can't
//!   load, there is no person in the frame (everything is the background): an effect the user chose never falls back
//!   to showing what it hides.
//! - **The edge** is soft, as Teams draws it ([`edge`]): the model's coarse mask refined against the frame's own
//!   picture so it hugs the person, then feathered over a few pixels, and smoothed over frames where it only
//!   flickers.
//! - **The pictures** are decoded once (a stock one from the bytes built in, the user's from its file), cropped to the
//!   frame's shape and kept as I420 at the frame's size until the choice or the size changes. One that can't be read
//!   is blurred instead, for the same reason.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::devices::{PixelFormat, VideoFrame};

mod compose;
mod edge;
#[cfg(feature = "effects")]
mod gpu;
#[cfg(feature = "effects")]
mod segment;
#[cfg(not(feature = "effects"))]
#[path = "effects/no_model.rs"]
mod segment;

pub use segment::{MODEL_HEIGHT, MODEL_WIDTH};

/// One of the backgrounds the app ships: our own artwork in the spirit of Teams' stock backgrounds
/// (`tools/backgrounds/make.py`), a 1280 by 720 JPEG.
#[derive(PartialEq, Eq)]
pub struct Stock {
    /// Its name in the settings file.
    pub key: &'static str,
    /// What the effects' picker calls it.
    pub name: &'static str,
    pub jpeg: &'static [u8],
}

/// The backgrounds the app ships, in the order the picker shows them.
pub const STOCK: [Stock; 6] = [
    Stock {
        key: "office",
        name: "Bright office",
        jpeg: include_bytes!("../../assets/backgrounds/office.jpg"),
    },
    Stock {
        key: "mountains",
        name: "Mountain dawn",
        jpeg: include_bytes!("../../assets/backgrounds/mountains.jpg"),
    },
    Stock {
        key: "city-lights",
        name: "City lights",
        jpeg: include_bytes!("../../assets/backgrounds/city-lights.jpg"),
    },
    Stock {
        key: "waves",
        name: "Purple waves",
        jpeg: include_bytes!("../../assets/backgrounds/waves.jpg"),
    },
    Stock {
        key: "seaside",
        name: "Seaside",
        jpeg: include_bytes!("../../assets/backgrounds/seaside.jpg"),
    },
    Stock {
        key: "forest",
        name: "Misty forest",
        jpeg: include_bytes!("../../assets/backgrounds/forest.jpg"),
    },
];

impl std::fmt::Debug for Stock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Stock").field(&self.key).finish()
    }
}

/// The stock background named `key`.
#[must_use]
pub fn stock(key: &str) -> Option<&'static Stock> {
    STOCK.iter().find(|stock| stock.key == key)
}

/// A picture put behind the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backdrop {
    /// One the app ships.
    Stock(&'static Stock),
    /// One the user added: a JPEG or PNG file.
    File(PathBuf),
}

/// What the camera's background is sent as.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Background {
    /// As the camera sees it.
    #[default]
    None,
    /// Blurred, the person kept sharp.
    Blur,
    /// Replaced by a picture.
    Picture(Backdrop),
}

/// The background chosen now, shared by whoever chooses it and the camera threads that apply it (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct BackgroundChoice {
    chosen: Arc<Mutex<Background>>,
}

impl BackgroundChoice {
    /// Applies `background` from the next frame on.
    pub fn set(&self, background: Background) {
        *self.chosen.lock().unwrap_or_else(PoisonError::into_inner) = background;
    }

    /// The background chosen now.
    #[must_use]
    pub fn get(&self) -> Background {
        self.chosen.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// A picture ready to go behind frames of one size.
struct Prepared {
    backdrop: Backdrop,
    width: u32,
    height: u32,
    /// The picture as I420 at that size; `None` when it couldn't be read (the background is blurred instead).
    frame: Option<VideoFrame>,
}

/// A camera thread's effects: the segmenter, the last mask (frames are smoothed over time) and the picture prepared
/// (see the module docs). One per camera thread; [`Effects::apply`] on each frame.
#[derive(Default)]
pub struct Effects {
    segmenter: segment::Segmenter,
    compositor: compose::Compositor,
    prepared: Option<Prepared>,
}

impl std::fmt::Debug for Effects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Effects").finish_non_exhaustive()
    }
}

impl Effects {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies `background` to `frame` (I420, each plane's stride its width, even sides, as
    /// [`crate::video::convert::to_i420`] makes it) in place; any other frame is left as it is.
    pub fn apply(&mut self, frame: &mut VideoFrame, background: &Background) {
        if *background == Background::None {
            self.segmenter.forget();
            return;
        }
        if !is_plain_i420(frame) {
            return;
        }
        let mask = self.segmenter.mask(frame);
        let picture = match background {
            Background::Picture(backdrop) => prepare(&mut self.prepared, backdrop, frame.width, frame.height),
            _ => None,
        };
        match picture {
            Some(picture) => self.compositor.replace(frame, mask, picture),
            None => self.compositor.blur(frame, mask),
        }
    }
}

/// `backdrop` as I420 at `width` by `height`, prepared into `prepared` now unless it already was; `None` when it
/// can't be read.
fn prepare<'a>(
    prepared: &'a mut Option<Prepared>,
    backdrop: &Backdrop,
    width: u32,
    height: u32,
) -> Option<&'a VideoFrame> {
    let ready = prepared
        .as_ref()
        .is_some_and(|prepared| prepared.backdrop == *backdrop && (prepared.width, prepared.height) == (width, height));
    if !ready {
        let frame = match compose::backdrop_frame(backdrop, width, height) {
            Ok(frame) => Some(frame),
            Err(error) => {
                log::warn!("the background picture can't be used, the background is blurred instead: {error}");
                None
            }
        };
        *prepared = Some(Prepared {
            backdrop: backdrop.clone(),
            width,
            height,
            frame,
        });
    }
    prepared.as_ref()?.frame.as_ref()
}

/// The largest picture kept of one the user adds, px: a larger one is scaled down to fit (a call sends 720p at most).
const KEPT_SIZE: (u32, u32) = (1920, 1080);

/// Adds the picture at `source` to the user's backgrounds in `dir` (made when missing): decoded (JPEG, PNG, GIF or
/// WebP, at most 64 MiB), scaled down to fit 1920 by 1080, and written there as a JPEG of a new name, which is
/// returned. Slow (a decode and an encode): never on a window's thread.
///
/// # Errors
/// The picture can't be read or decoded, or `dir` can't be written.
pub fn import(source: &Path, dir: &Path) -> Result<PathBuf, String> {
    let picture = compose::read_picture(source)?;
    let (width, height) = KEPT_SIZE;
    let picture = if picture.width() > width || picture.height() > height {
        picture.resize(width, height, image::imageops::FilterType::Triangle)
    } else {
        picture
    };
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let mut name = [0_u8; 8];
    getrandom::fill(&mut name).map_err(|error| error.to_string())?;
    let name: String = name.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    // written under a name `added` doesn't list, then renamed: a write that fails leaves no broken picture behind
    let path = dir.join(format!("{name}.jpg"));
    let partial = dir.join(format!("{name}.jpg.part"));
    let written = write_jpeg(&partial, &picture.to_rgb8())
        .and_then(|()| std::fs::rename(&partial, &path).map_err(|error| error.to_string()));
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written.map(|()| path)
}

/// `picture` as a JPEG file at `path`.
fn write_jpeg(path: &Path, picture: &image::RgbImage) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
    let mut writer = std::io::BufWriter::new(file);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, 90)
        .encode_image(picture)
        .map_err(|error| error.to_string())?;
    std::io::Write::flush(&mut writer).map_err(|error| error.to_string())
}

/// The pictures the user added in `dir` ([`import`]), the oldest first; none when it can't be read.
#[must_use]
pub fn added(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pictures: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "jpg"))
        .filter_map(|entry| {
            let modified = entry.metadata().and_then(|metadata| metadata.modified()).ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    pictures.sort();
    pictures.into_iter().map(|(_, path)| path).collect()
}

/// Whether `frame` is I420 laid out as the effects take it.
fn is_plain_i420(frame: &VideoFrame) -> bool {
    let (width, height) = (to_usize(frame.width), to_usize(frame.height));
    frame.format == PixelFormat::I420
        && frame.stride == frame.width
        && width >= 2
        && height >= 2
        && width % 2 == 0
        && height % 2 == 0
        && frame.data.len() >= width * height * 3 / 2
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(all(test, feature = "effects"))]
mod tests;
