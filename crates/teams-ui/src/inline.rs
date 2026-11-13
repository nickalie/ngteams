//! What a message draws besides its text, as Teams draws it: the pictures pasted into it and its emoticons
//! (`rich_text` lays them out, this module draws them). Their bytes come only from the engine ([`Core`]), never from a
//! request of a view's own.
//!
//! - **A picture** ([`picture`]) is the view of an AMS object the message names, or a GIF on Giphy's media host (one
//!   picked from Teams' GIF picker, playing as Teams plays it): asked of the engine once a run per account and `src`
//!   ([`Core::message_picture`]), decoded here off the foreground (every frame scaled down to at most
//!   [`MAX_DECODED_SIDE`] on its longer side; a GIF with its frames up to the first that doesn't decode, at most
//!   [`MAX_GIF_FRAMES`] and [`MAX_GIF_BYTES`] of them, its canvas at most [`MAX_GIF_SIDE`]; a still picture's source
//!   at most [`MAX_STILL_SIDE`] per side and 256 MiB across overlapping pixel buffers and codec scratch) and kept for
//!   the run. It is
//!   drawn as Teams draws a picture in a message ([`Shape::of`]): sized by the size the message gives it (else its
//!   own), never upscaled, its aspect ratio kept, in a frame of the border colour (1 px, rounded) at the start of its
//!   line, never wider than the width there is. A tall one (twice as high as wide) or a small one (lower than
//!   [`PICTURE_BOX`]) sits in a square frame of [`PICTURE_BOX`], fitted (a small one at its own size); any other one is
//!   [`PICTURE_BOX`] high; a wide one (twice as wide as high) keeps its own size, at most [`WIDE_MAX_WIDTH`] wide and,
//!   once larger than Teams' bounds, [`PICTURE_BOX`] high. The empty frame stands in while it is read. One the engine
//!   has none of, or none can be asked of (no engine), is drawn as the message's fallback (its placeholder text). Once
//!   shown, a click on it opens it in the picture viewer when its view opens pictures ([`Picture::opening`]:
//!   `crate::picture_viewer`), a pointing hand over it; a press on it starts no text selection.
//! - **An emoticon** ([`emoticon`]) is drawn from Teams' own animation of it (its CDN's strip of frames, read by the
//!   engine as a reaction's is, [`reactions::animation`]): playing when `animated` (an emoticon-only message's, drawn
//!   large), else standing on its first frame (one inside text, at the text's height, as Teams draws it until hovered).
//!   Until it arrives, or when there is none, the reaction pack's picture of it stands in, else its emoji as text.
//! - **A file's thumbnail** ([`thumbnail`]) is asked of the engine by the file's sharing link
//!   ([`Core::file_thumbnail`]) and decoded and kept as a picture is; `crate::files` draws it on the file's card.
//! - **Inline** ([`InlineText`]): an emoticon inside a paragraph keeps the place of a few spaces in the text, and is
//!   drawn over them where the text's layout put them, so it flows and wraps with the text. It is drawn as Teams draws
//!   one inside text: [`INLINE_EMOTICON_REMS`] square (Teams' 20 px beside its 14 px text, so it follows the app's text
//!   size), standing on the bottom of the text's font, its descent below the baseline (Teams' `vertical-align:
//!   text-bottom`), however high the line is ([`inline_emoticon_origin`]); the line keeps its height.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    InteractiveElement, IntoElement, LayoutId, ParentElement as _, Pixels, Point, RenderImage, RenderOnce,
    SharedString, Size, StatefulInteractiveElement as _, Style, Styled as _, TextLayout, Window, div, img, px, size,
};
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder as _, Frame, ImageDecoder as _, Limits, RgbaImage, imageops};
use teams_core::{PictureBytes, Reply, Strip};
use teams_proto::PastedFormat;

use crate::bridge::Core;
use crate::reactions;

/// The longer side a decoded picture is scaled down to, at most: no message is drawn wider.
pub const MAX_DECODED_SIDE: u32 = 2048;

/// The largest source side of a still picture, before downscaling; the pixel budget also bounds its area.
pub const MAX_STILL_SIDE: u32 = 16384;

/// The decoder's allocation budget and the peak of codec scratch, conversion and scaling buffers.
const MAX_STILL_ALLOC: u64 = 256 << 20;

/// App tiles keep the smaller source bounds, regardless of the size they are drawn at.
const MAX_TILE_SIDE: u32 = 4096;
const MAX_TILE_ALLOC: u64 = 8 * (MAX_TILE_SIDE as u64) * (MAX_TILE_SIDE as u64);

/// The most frames of a GIF kept: the frames past it are left out, the animation looping over those before.
pub const MAX_GIF_FRAMES: usize = 300;

/// The most bytes a GIF's frames take once decoded and scaled down: the frames past it are left out (never the first,
/// which [`MAX_DECODED_SIDE`] keeps well below it). Every picture is kept for the run.
pub const MAX_GIF_BYTES: usize = 128 << 20;

/// The longer side a GIF's canvas may have to be decoded at all: each of its frames is composed at the canvas's size
/// before it is scaled down. A larger one is drawn as the message's fallback.
pub const MAX_GIF_SIDE: u32 = 4096;

/// The most the decoding of one GIF frame may allocate: the canvas kept between frames, the frame and its composite,
/// each RGBA at most [`MAX_GIF_SIDE`] square. A frame past it (one larger than the canvas) ends the frames.
const MAX_GIF_FRAME_ALLOC: u64 = 3 * 4 * (MAX_GIF_SIDE as u64) * (MAX_GIF_SIDE as u64);

/// The side an emoticon-only message's emoticons are drawn at (Teams' `animated-emoticon-58`).
pub const LARGE_EMOTICON: Pixels = px(58.0);

/// The side an emoticon inside text is drawn at, in the app's text size (its rem): Teams draws one 20 px square beside
/// its 14 px text.
pub const INLINE_EMOTICON_REMS: f32 = 20.0 / 14.0;

/// The side an emoticon inside text is drawn at in `window` ([`INLINE_EMOTICON_REMS`]).
#[must_use]
pub fn inline_emoticon_side(window: &Window) -> Pixels {
    window.rem_size() * INLINE_EMOTICON_REMS
}

/// The vertical metrics of the line of text an emoticon is drawn on ([`inline_emoticon_origin`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMetrics {
    /// The line's height.
    pub height: Pixels,
    /// The ascent its text was laid out with (the largest of the fonts the line draws with).
    pub ascent: Pixels,
    /// The descent its text was laid out with, as the ascent.
    pub descent: Pixels,
    /// The descent of the text's own font below the baseline: what the emoticon stands on.
    pub font_descent: Pixels,
}

/// Where an emoticon `side` square is drawn in its place on a line of text, as Teams draws one (an inline block aligned
/// `text-bottom`): its bottom the text's font's descent below the line's baseline (the text centred in the line's
/// height by its laid-out ascent and descent, as GPUI draws it), centred in the place's width. `place` is the place's
/// start on its line (at the line's top) and `width` its width there.
#[must_use]
pub fn inline_emoticon_origin(place: Point<Pixels>, width: Pixels, line: LineMetrics, side: Pixels) -> Point<Pixels> {
    let baseline = (line.height - line.ascent - line.descent) / 2.0 + line.ascent;
    let bottom = baseline + line.font_descent;
    gpui::point(place.x + (width - side) / 2.0, place.y + bottom - side)
}

/// How long a picture whose read failed on the way is drawn as its fallback before it is asked for again.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// The size a picture is held at while it is read, when the message gives none.
const UNKNOWN_SIZE: (u32, u32) = (320, 180);

/// The side of the square frame a tall or small picture is drawn in, and the height of any other but a wide one
/// (Teams' `18rem`, its `rem` 10 px).
pub const PICTURE_BOX: f32 = 180.0;

/// The widest a wide picture is drawn (Teams' `62rem`).
pub const WIDE_MAX_WIDTH: f32 = 620.0;

/// The highest a wide picture is drawn at its own size before Teams bounds it (Teams' `48rem`).
const WIDE_OWN_MAX_HEIGHT: f32 = 480.0;

/// The highest a wide picture is ever drawn (Teams' `25rem`).
const WIDE_MAX_HEIGHT: f32 = 250.0;

/// How much higher than wide a picture is to be tall, or wider than high to be wide (Teams' `812 / 406`).
const LONG_RATIO: f32 = 2.0;

/// A picture's frame: its border's width, and its corners' radius (Teams' `borderRadiusLarge`).
const FRAME_BORDER: f32 = 1.0;
const FRAME_RADIUS: f32 = 6.0;

/// The part of the engine's request side a message's pictures and emoticons are asked of ([`Core`]'s, through
/// [`PictureSource::new`]; a view test's own fake through [`PictureSource::of`]).
pub trait PictureEngine: Send + Sync {
    /// The account the message is shown to: a picture is read with its token.
    fn me(&self) -> &str;
    /// As [`Core::message_picture`].
    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>>;
    /// As [`Core::file_thumbnail`]; `None` (there is none) unless the engine reads thumbnails.
    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>> {
        let _ = share_url;
        Reply::ready(Ok(None))
    }
    /// As [`Core::file_stream`]; unavailable unless the engine reads files.
    fn file_stream(&self, share_url: String, offset: u64, len: u64) -> Reply<teams_core::FileStream> {
        let _ = (share_url, offset, len);
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// As [`Core::file_size`]; unavailable unless the engine reads files.
    fn file_size(&self, share_url: String) -> Reply<u64> {
        let _ = share_url;
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// As [`Core::recording_presentation`]; unavailable unless the engine streams recordings.
    fn recording_presentation(
        &self,
        link: String,
        drive: String,
        item: String,
    ) -> Reply<Arc<teams_proto::Presentation>> {
        let _ = (link, drive, item);
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// As [`Core::recording_segment`]; unavailable unless the engine streams recordings.
    fn recording_segment(&self, part: teams_proto::MediaSegmentLink) -> Reply<Vec<u8>> {
        let _ = part;
        Reply::ready(Err(teams_core::EngineError::FileUnavailable))
    }
    /// As [`Core::reaction_animation`].
    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>>;
    /// Which engine it is, for telling two sources apart.
    fn engine(&self) -> *const ();
}

/// An engine's request side as a [`PictureEngine`].
struct CoreEngine(Arc<dyn Core>);

impl PictureEngine for CoreEngine {
    fn me(&self) -> &str {
        self.0.me()
    }

    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>> {
        self.0.message_picture(src)
    }

    fn file_thumbnail(&self, share_url: String) -> Reply<Option<PictureBytes>> {
        self.0.file_thumbnail(share_url)
    }

    fn file_stream(&self, share_url: String, offset: u64, len: u64) -> Reply<teams_core::FileStream> {
        self.0.file_stream(share_url, offset, len)
    }

    fn file_size(&self, share_url: String) -> Reply<u64> {
        self.0.file_size(share_url)
    }

    fn recording_presentation(
        &self,
        link: String,
        drive: String,
        item: String,
    ) -> Reply<Arc<teams_proto::Presentation>> {
        self.0.recording_presentation(link, drive, item)
    }

    fn recording_segment(&self, part: teams_proto::MediaSegmentLink) -> Reply<Vec<u8>> {
        self.0.recording_segment(part)
    }

    fn reaction_animation(&self, id: String) -> Reply<Option<Strip>> {
        self.0.reaction_animation(id)
    }

    fn engine(&self) -> *const () {
        Arc::as_ptr(&self.0).cast()
    }
}

/// Where a message's pictures and emoticons are asked for: the engine of the account the message is shown to.
#[derive(Clone)]
pub struct PictureSource(Arc<dyn PictureEngine>);

impl PictureSource {
    /// The engine `core` is the request side of.
    #[must_use]
    pub fn new(core: Arc<dyn Core>) -> Self {
        Self(Arc::new(CoreEngine(core)))
    }

    /// Any [`PictureEngine`] (a view test's fake).
    #[must_use]
    pub fn of(engine: Arc<dyn PictureEngine>) -> Self {
        Self(engine)
    }

    /// The engine it names.
    #[must_use]
    pub fn engine(&self) -> &dyn PictureEngine {
        self.0.as_ref()
    }
}

impl fmt::Debug for PictureSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PictureSource").field(&self.0.me()).finish()
    }
}

/// Two sources are the same when they are the same engine's.
impl PartialEq for PictureSource {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0.engine(), other.0.engine())
    }
}

impl Eq for PictureSource {}

/// What is known of one picture this run.
enum Known {
    /// Asked; its answer hasn't come.
    Asked,
    /// Decoded, with the size its bytes have.
    Decoded(Arc<RenderImage>, (u32, u32)),
    /// The engine has none, or its bytes can't be decoded.
    Missing,
    /// Its read failed on the way, then: asked again after [`RETRY_AFTER`].
    Failed(Instant),
}

/// What names a picture asked of the engine: a message's `<img>` by its `src`, or a file's thumbnail by its sharing
/// link.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Named {
    Src(String),
    Thumbnail(String),
}

/// The messages' pictures and files' thumbnails this run, by the account they are shown to and what names them.
#[derive(Default)]
struct Pictures {
    known: HashMap<(String, Named), Known>,
}

impl gpui::Global for Pictures {}

/// How a picture stands.
#[derive(Clone)]
pub enum PictureState {
    /// Being read.
    Reading,
    /// Read: its frames and the size its bytes have.
    Shown(Arc<RenderImage>, (u32, u32)),
    /// None to show.
    Missing,
}

/// The picture `src` names as `source`'s engine has it; asks for it once when it wasn't (the windows drawn again once it
/// is decoded).
pub fn picture(src: &str, source: &PictureSource, cx: &mut App) -> PictureState {
    asked(Named::Src(src.to_owned()), source, cx)
}

/// The thumbnail of the file `share_url` is the sharing link of, as `source`'s engine has it; asked for once, as a
/// picture is ([`picture`]).
pub fn thumbnail(share_url: &str, source: &PictureSource, cx: &mut App) -> PictureState {
    asked(Named::Thumbnail(share_url.to_owned()), source, cx)
}

/// The picture `named` names as `source`'s engine has it; asks for it once when it wasn't (the windows drawn again
/// once it is decoded).
fn asked(named: Named, source: &PictureSource, cx: &mut App) -> PictureState {
    let key = (source.engine().me().to_owned(), named);
    let pictures = cx.default_global::<Pictures>();
    match pictures.known.get(&key) {
        Some(Known::Asked) => return PictureState::Reading,
        Some(Known::Decoded(image, natural)) => return PictureState::Shown(Arc::clone(image), *natural),
        Some(Known::Missing) => return PictureState::Missing,
        Some(Known::Failed(at)) if at.elapsed() < RETRY_AFTER => return PictureState::Missing,
        Some(Known::Failed(_)) | None => {}
    }
    pictures.known.insert(key.clone(), Known::Asked);
    let reply = match &key.1 {
        Named::Src(src) => source.engine().message_picture(src.clone()),
        Named::Thumbnail(share_url) => source.engine().file_thumbnail(share_url.clone()),
    };
    cx.spawn(async move |cx| {
        let known = match reply.await {
            Ok(Some(bytes)) => cx
                .background_executor()
                .spawn(async move { decode(&bytes) })
                .await
                .map_or(Known::Missing, |(image, natural)| {
                    Known::Decoded(Arc::new(image), natural)
                }),
            Ok(None) => Known::Missing,
            Err(_) => Known::Failed(Instant::now()),
        };
        cx.update(|cx| {
            cx.default_global::<Pictures>().known.insert(key, known);
            cx.refresh_windows();
        });
    })
    .detach();
    PictureState::Reading
}

/// A picture's bytes as the frames GPUI draws (BGRA), each scaled down to at most [`MAX_DECODED_SIDE`] on its longer
/// side, and the size the bytes have; `None` when they don't decode or exceed the source side or allocation limits.
#[must_use]
pub fn decode(picture: &PictureBytes) -> Option<(RenderImage, (u32, u32))> {
    decode_within(picture, MAX_DECODED_SIDE)
}

/// [`decode`], each frame scaled down to at most `max_side` on its longer side instead (the picture viewer's, which
/// draws a picture at its own size).
#[must_use]
pub fn decode_within(picture: &PictureBytes, max_side: u32) -> Option<(RenderImage, (u32, u32))> {
    decode_limited(picture, max_side, MAX_STILL_SIDE, MAX_STILL_ALLOC)
}

/// An app tile, with still sources capped at 4096 pixels per side before scaling.
pub(crate) fn decode_app_tile(picture: &PictureBytes, max_side: u32) -> Option<(RenderImage, (u32, u32))> {
    decode_limited(picture, max_side, MAX_TILE_SIDE, MAX_TILE_ALLOC)
}

/// A still picture (a desktop banner's: a person's or an app's) as RGBA, scaled down to at most `max_side` on its
/// longer side, within an app tile's limits; `None` for a GIF, or bytes that don't decode or exceed the limits.
pub(crate) fn decode_still_rgba(picture: &PictureBytes, max_side: u32) -> Option<RgbaImage> {
    if picture.format == PastedFormat::Gif {
        return None;
    }
    decode_still(picture, max_side, MAX_TILE_SIDE, MAX_TILE_ALLOC).map(|(rgba, _)| rgba)
}

fn decode_limited(
    picture: &PictureBytes,
    max_side: u32,
    source_side: u32,
    max_alloc: u64,
) -> Option<(RenderImage, (u32, u32))> {
    if picture.format == PastedFormat::Gif {
        return decode_gif(&picture.bytes, max_side);
    }
    let (mut shown, natural) = decode_still(picture, max_side, source_side, max_alloc)?;
    to_bgra(&mut shown);
    Some((RenderImage::new(vec![Frame::new(shown)]), natural))
}

/// A PNG, JPEG or WebP picture as RGBA scaled down to at most `max_side`, and its natural size.
fn decode_still(
    picture: &PictureBytes,
    max_side: u32,
    source_side: u32,
    max_alloc: u64,
) -> Option<(RgbaImage, (u32, u32))> {
    let format = match picture.format {
        PastedFormat::Gif => return None,
        PastedFormat::Png => image::ImageFormat::Png,
        PastedFormat::Jpeg => image::ImageFormat::Jpeg,
        PastedFormat::Webp => image::ImageFormat::WebP,
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&picture.bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(source_side);
    limits.max_image_height = Some(source_side);
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().ok()?;
    let natural = decoder.dimensions();
    if u64::from(natural.0) * u64::from(natural.1) > max_alloc / 4 {
        return None;
    }
    limits.reserve(decoder.total_bytes()).ok()?;
    let codec = codec_peak_bytes(natural, decoder.total_bytes(), format, &picture.bytes);
    if still_peak_bytes(natural, decoder.color_type(), codec, max_side) > max_alloc {
        return None;
    }
    decoder.set_limits(limits).ok()?;
    let rgba = image::DynamicImage::from_decoder(decoder).ok()?.into_rgba8();
    Some((scaled_down(rgba, max_side), natural))
}

fn still_peak_bytes(natural: (u32, u32), color: image::ColorType, codec: u64, max_side: u32) -> u64 {
    let pixels = u64::from(natural.0) * u64::from(natural.1);
    let rgba = pixels * 4;
    let decoded = pixels * u64::from(color.bytes_per_pixel());
    let conversion = if color == image::ColorType::Rgba8 {
        rgba
    } else {
        decoded + rgba
    };
    let peak = codec.max(conversion);
    let shown = scaled_size(natural, max_side);
    if shown == natural {
        return peak;
    }
    // Triangle resizing keeps a full-width float RGBA intermediate beside its source and output.
    let scratch = u64::from(natural.0) * u64::from(shown.1) * 16;
    let output = u64::from(shown.0) * u64::from(shown.1) * 4;
    let weights = u64::from(natural.0.max(natural.1)) * 8;
    peak.max(rgba + scratch + output + weights)
}

fn codec_peak_bytes((width, height): (u32, u32), decoded: u64, format: image::ImageFormat, bytes: &[u8]) -> u64 {
    let width = u64::from(width).div_ceil(32) * 32;
    let height = u64::from(height).div_ceil(32) * 32;
    match format {
        image::ImageFormat::Jpeg if interleaved_baseline_jpeg(bytes) => decoded + width * 1024 + (1 << 20),
        // zune-jpeg keeps i16 coefficients for up to four input components, even when it outputs RGB.
        image::ImageFormat::Jpeg => decoded + width * height * 8 + width * 256 + (1 << 20),
        // WebP can overlap YUV, compressed alpha, lossless transforms and a first-frame canvas.
        // The vendored decoder caps retained Huffman structures at 16 MiB, including compressed alpha.
        image::ImageFormat::WebP => decoded + width * height * 16 + width * 256 + (17 << 20),
        _ => decoded,
    }
}

fn interleaved_baseline_jpeg(mut bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    bytes = &bytes[2..];
    let mut components = None;
    while let Some((&0xff, rest)) = bytes.split_first() {
        bytes = rest;
        while bytes.first() == Some(&0xff) {
            bytes = &bytes[1..];
        }
        let Some((&marker, rest)) = bytes.split_first() else {
            return false;
        };
        let Some(length) = rest.get(..2) else {
            return false;
        };
        let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
        let Some(segment) = rest.get(2..length) else {
            return false;
        };
        match marker {
            0xc0 | 0xc1 if components.is_none() => components = segment.get(5).copied(),
            0x00 | 0x01 | 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf | 0xd0..=0xd9 => return false,
            0xda => return components.is_some_and(|count| segment.first() == Some(&count)),
            _ => {}
        }
        bytes = &rest[length..];
    }
    false
}

/// A GIF's frames, each scaled down as a still picture is, up to the first that doesn't decode (a truncated file's
/// decoder keeps failing rather than ending) and within [`MAX_GIF_FRAMES`] and [`MAX_GIF_BYTES`]; `None` when not even
/// the first decodes or its canvas is larger than [`MAX_GIF_SIDE`].
fn decode_gif(bytes: &[u8], max_side: u32) -> Option<(RenderImage, (u32, u32))> {
    let mut decoder = GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_GIF_SIDE);
    limits.max_image_height = Some(MAX_GIF_SIDE);
    limits.max_alloc = Some(MAX_GIF_FRAME_ALLOC);
    decoder.set_limits(limits).ok()?;
    let natural = decoder.dimensions();
    let mut frames = Vec::new();
    let mut kept = 0_usize;
    for frame in decoder.into_frames().take(MAX_GIF_FRAMES).map_while(Result::ok) {
        let (left, top, delay) = (frame.left(), frame.top(), frame.delay());
        let mut shown = scaled_down(frame.into_buffer(), max_side);
        let size = shown.as_raw().len();
        if !frames.is_empty() && kept.saturating_add(size) > MAX_GIF_BYTES {
            break;
        }
        kept = kept.saturating_add(size);
        to_bgra(&mut shown);
        frames.push(Frame::from_parts(shown, left, top, delay));
    }
    (!frames.is_empty()).then(|| (RenderImage::new(frames), natural))
}

/// `image` scaled down to at most `max_side` on its longer side, keeping its aspect ratio.
fn scaled_down(image: RgbaImage, max_side: u32) -> RgbaImage {
    let shown = scaled_size(image.dimensions(), max_side);
    if shown == image.dimensions() {
        return image;
    }
    imageops::resize(&image, shown.0, shown.1, imageops::FilterType::Triangle)
}

fn scaled_size((width, height): (u32, u32), max_side: u32) -> (u32, u32) {
    let longer = width.max(height);
    if longer <= max_side {
        return (width, height);
    }
    let scale = |side: u32| {
        u32::try_from(u64::from(side) * u64::from(max_side) / u64::from(longer))
            .unwrap_or(max_side)
            .max(1)
    };
    (scale(width), scale(height))
}

/// GPUI draws BGRA.
fn to_bgra(image: &mut RgbaImage) {
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
}

/// A picture of a message, drawn as the module docs say. `given` is the size the message gives it.
pub struct Picture {
    id: ElementId,
    src: String,
    given: Option<(u32, u32)>,
    source: Option<PictureSource>,
    /// Its frame's border and background, which is all that is drawn while it is read.
    frame: Hsla,
    /// Drawn when there is none.
    fallback: Option<AnyElement>,
    /// What a click on it does once it is shown (the picture viewer opened on it), and the element id (and test
    /// selector) of what takes the click.
    open: Option<(SharedString, OnOpen)>,
    child: Option<AnyElement>,
}

/// What a click on a shown picture calls.
pub type OnOpen = Rc<dyn Fn(&mut Window, &mut App)>;

impl Picture {
    #[must_use]
    pub fn new(
        id: impl Into<ElementId>,
        src: &str,
        given: Option<(u32, u32)>,
        source: Option<PictureSource>,
        frame: Hsla,
        fallback: AnyElement,
    ) -> Self {
        Self {
            id: id.into(),
            src: src.to_owned(),
            given,
            source,
            frame,
            fallback: Some(fallback),
            open: None,
            child: None,
        }
    }

    /// It opened by `open` on a click once it is shown, a pointing hand over it; `id` names the element that takes the
    /// click (its test selector too).
    #[must_use]
    pub fn opening(mut self, id: impl Into<SharedString>, open: OnOpen) -> Self {
        self.open = Some((id.into(), open));
        self
    }
}

/// How a picture was laid out.
pub enum Laid {
    /// As its fallback, laid out as a child.
    Fallback,
    /// Measured: its frames (`None` while it is read), the size it is shaped by (the message's, else its bytes') and
    /// the size of its bytes (the shaping size while it is read).
    Measured(Option<Arc<RenderImage>>, (u32, u32), (u32, u32)),
}

impl IntoElement for Picture {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Picture {
    type RequestLayoutState = Laid;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Laid) {
        let state = match &self.source {
            Some(source) => picture(&self.src, source, cx),
            None => PictureState::Missing,
        };
        let (image, given, own) = match state {
            PictureState::Missing => {
                let mut fallback = self.fallback.take().unwrap_or_else(|| div().into_any_element());
                let layout = fallback.request_layout(window, cx);
                self.child = Some(fallback);
                return (layout, Laid::Fallback);
            }
            PictureState::Reading => {
                let given = self.given.unwrap_or(UNKNOWN_SIZE);
                (None, given, given)
            }
            PictureState::Shown(image, own) => (Some(image), self.given.unwrap_or(own), own),
        };
        let layout = window.request_measured_layout(Style::default(), move |known, available, _, _| {
            Shape::of(given, own, room(known, available)).outer()
        });
        (layout, Laid::Measured(image, given, own))
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        laid: &mut Laid,
        window: &mut Window,
        cx: &mut App,
    ) {
        match laid {
            Laid::Fallback => {
                if let Some(child) = &mut self.child {
                    child.prepaint(window, cx);
                }
            }
            Laid::Measured(image, given, own) => {
                // shaped for the width of its block, which a block or flex parent stretches to the width there is,
                // and never taller than its block, should its block be lower
                let shape = Shape::of(*given, *own, Some(bounds.size.width)).within_height(bounds.size.height);
                let drawn = image.as_ref().map(|image| {
                    // an id of its own: GPUI keeps a GIF's frame in the element's state; its size given outright, so
                    // the image's own aspect ratio sets nothing
                    InteractiveElement::id(img(Arc::clone(image)), self.id.clone())
                        .w(shape.image.width)
                        .h(shape.image.height)
                        .flex_none()
                        // within the frame's rounded corners, should it fill the frame
                        .rounded(px(FRAME_RADIUS - FRAME_BORDER))
                });
                let Size { width, height } = shape.outer();
                let frame = div()
                    .debug_selector(|| "rich-picture-drawn".to_owned())
                    .w(width)
                    .h(height)
                    .border(px(FRAME_BORDER))
                    .border_color(self.frame)
                    .rounded(px(FRAME_RADIUS))
                    .bg(self.frame)
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .children(drawn);
                let mut sized = match (&self.open, image.is_some()) {
                    // a click on it opens it; a press on it starts no text selection (it isn't text)
                    (Some((id, open)), true) => {
                        let open = Rc::clone(open);
                        let selector = id.to_string();
                        div()
                            .id(ElementId::Name(id.clone()))
                            .debug_selector(move || selector)
                            .w(width)
                            .h(height)
                            .cursor_pointer()
                            .on_click(move |_, window, cx| open(window, cx))
                            .child(frame)
                            .into_any_element()
                    }
                    _ => frame.into_any_element(),
                };
                sized.prepaint_as_root(
                    bounds.origin,
                    size(width, height).map(AvailableSpace::Definite),
                    window,
                    cx,
                );
                self.child = Some(sized);
            }
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _laid: &mut Laid,
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

/// The width there is for a picture: `known`'s, else a definite `available`; any under a max-content constraint, none
/// under a min-content one (it shrinks with its bubble, as an image of `max-width: 100%` does).
fn room(known: Size<Option<Pixels>>, available: Size<AvailableSpace>) -> Option<Pixels> {
    known.width.or(match available.width {
        AvailableSpace::Definite(room) => Some(room),
        AvailableSpace::MaxContent => None,
        AvailableSpace::MinContent => Some(px(0.0)),
    })
}

/// How Teams sizes a picture in a message, by the size it is given (its `data-image-type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// At least twice as high as wide: fitted into a square frame of [`PICTURE_BOX`].
    Tall,
    /// Lower than [`PICTURE_BOX`] and not long: at its own size in a square frame of [`PICTURE_BOX`] (scaled down only
    /// to fit it).
    Small,
    /// Neither long nor small: [`PICTURE_BOX`] high.
    Standard,
    /// At least twice as wide as high: its own size, or once larger than Teams' bounds at most [`WIDE_MAX_WIDTH`] wide
    /// and [`PICTURE_BOX`] high.
    Wide,
}

impl Kind {
    /// The kind of a picture given `width` by `height`.
    #[must_use]
    pub fn of(width: f32, height: f32) -> Self {
        let (width, height) = (width.max(1.0), height.max(1.0));
        if height / width >= LONG_RATIO {
            Self::Tall
        } else if width / height >= LONG_RATIO {
            Self::Wide
        } else if height < PICTURE_BOX {
            Self::Small
        } else {
            Self::Standard
        }
    }
}

/// The frame a picture is drawn in, inside its border, and the size its bytes are drawn at, centred in the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub frame: Size<Pixels>,
    pub image: Size<Pixels>,
}

impl Shape {
    /// The shape of a picture the message gives `given` (else its bytes' size) and whose bytes are `own`, in `room` (the
    /// width there is; `None` for any), as Teams draws one: its frame by its [`Kind`], no wider than `room` (a wide one
    /// keeping its aspect ratio), its bytes contained in the frame, a small one's never past the size it is given.
    #[must_use]
    pub fn of(given: (u32, u32), own: (u32, u32), room: Option<Pixels>) -> Self {
        #[expect(clippy::cast_precision_loss, reason = "a picture's side is at most 100 000 pixels")]
        let (width, height) = (given.0.max(1) as f32, given.1.max(1) as f32);
        #[expect(clippy::cast_precision_loss, reason = "a picture's side is at most 100 000 pixels")]
        let aspect = own.0.max(1) as f32 / own.1.max(1) as f32;
        let room = room.map_or(f32::INFINITY, |room| (f32::from(room) - 2.0 * FRAME_BORDER).max(0.0));
        let kind = Kind::of(width, height);
        let frame = match kind {
            Kind::Tall | Kind::Small => (PICTURE_BOX.min(room), PICTURE_BOX),
            Kind::Standard => ((PICTURE_BOX * width / height).min(room), PICTURE_BOX),
            Kind::Wide => {
                let bounded = if width > WIDE_MAX_WIDTH || height > WIDE_OWN_MAX_HEIGHT {
                    (WIDE_MAX_WIDTH / width).min(PICTURE_BOX / height)
                } else {
                    1.0
                };
                let scale = bounded.min(WIDE_MAX_HEIGHT / height).min(room / width);
                (width * scale, height * scale)
            }
        };
        let mut image = if frame.1 > 0.0 && frame.0 / frame.1 > aspect {
            (frame.1 * aspect, frame.1)
        } else {
            (frame.0, frame.0 / aspect)
        };
        if kind == Kind::Small && image.0 > width {
            image = (width, width / aspect);
        }
        Self {
            frame: size(px(frame.0), px(frame.1)),
            image: size(px(image.0), px(image.1)),
        }
    }

    /// It scaled down, its aspect ratio kept, so that its frame with its border is at most `height` high.
    #[must_use]
    pub fn within_height(self, height: Pixels) -> Self {
        let outer = self.outer().height;
        if outer <= height || outer <= px(0.0) {
            return self;
        }
        let scale = (height / outer).max(0.0);
        Self {
            frame: size(self.frame.width * scale, self.frame.height * scale),
            image: size(self.image.width * scale, self.image.height * scale),
        }
    }

    /// Its frame's size with its border.
    #[must_use]
    pub fn outer(&self) -> Size<Pixels> {
        let border = px(2.0 * FRAME_BORDER);
        size(self.frame.width + border, self.frame.height + border)
    }
}

/// An emoticon of a message, drawn as the module docs say.
#[derive(IntoElement)]
pub struct Emoticon {
    id: String,
    alt: Option<String>,
    /// Its side; `None` for one inside text ([`inline_emoticon_side`]).
    side: Option<Pixels>,
    animated: bool,
    source: Option<PictureSource>,
}

impl Emoticon {
    #[must_use]
    pub fn new(id: &str, alt: Option<&str>, side: Pixels, animated: bool, source: Option<PictureSource>) -> Self {
        Self {
            id: id.to_owned(),
            alt: alt.map(str::to_owned),
            side: Some(side),
            animated,
            source,
        }
    }

    /// One inside text: standing on its first frame, at the side Teams draws one there ([`inline_emoticon_side`]).
    #[must_use]
    pub fn inline(id: &str, alt: Option<&str>, source: Option<PictureSource>) -> Self {
        Self {
            id: id.to_owned(),
            alt: alt.map(str::to_owned),
            side: None,
            animated: false,
            source,
        }
    }
}

impl RenderOnce for Emoticon {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let side = self.side.unwrap_or_else(|| inline_emoticon_side(window));
        let frames = self
            .source
            .as_ref()
            .and_then(|source| reactions::animation_of(&self.id, source.engine(), cx));
        match frames {
            // with an id GPUI plays it (the frame it is on kept in the element's state); without one it stands on its
            // first frame
            Some(frames) if self.animated => {
                InteractiveElement::id(img(frames), ElementId::Name(format!("emoticon-{}", self.id).into()))
                    .size(side)
                    .flex_none()
                    .into_any_element()
            }
            Some(frames) => img(frames).size(side).flex_none().into_any_element(),
            None if reactions::catalog().get(&self.id).is_some() => reactions::emoji(&self.id, side),
            None => div()
                .flex_none()
                .line_height(side)
                .text_size(side * 0.8)
                .child(self.alt.unwrap_or_default())
                .into_any_element(),
        }
    }
}

/// A paragraph's text with emoticons drawn over the places it keeps for them (see the module docs): `text` is the
/// paragraph's element, `layout` its text's layout, and each emoticon the byte range of its place in the text.
pub struct InlineText {
    text: AnyElement,
    layout: TextLayout,
    emoticons: Vec<(Range<usize>, AnyElement)>,
}

impl InlineText {
    #[must_use]
    pub fn new(text: AnyElement, layout: TextLayout, emoticons: Vec<(Range<usize>, AnyElement)>) -> Self {
        Self {
            text,
            layout,
            emoticons,
        }
    }
}

impl IntoElement for InlineText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InlineText {
    type RequestLayoutState = ();
    /// Which emoticons were placed: one whose place the text's layout doesn't hold (cut off by a line clamp) isn't, and
    /// isn't painted either (GPUI panics on painting an element it didn't prepaint).
    type PrepaintState = Vec<bool>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let text = self.text.request_layout(window, cx);
        let style = Style {
            size: Size {
                width: gpui::relative(1.0).into(),
                height: gpui::Length::Auto,
            },
            ..Style::default()
        };
        (window.request_layout(style, [text], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<bool> {
        self.text.prepaint(window, cx);
        let height = self.layout.line_height();
        let side = inline_emoticon_side(window);
        let font = window.text_system().resolve_font(&window.text_style().font());
        let mut placed = vec![false; self.emoticons.len()];
        for ((place, emoticon), placed) in self.emoticons.iter_mut().zip(&mut placed) {
            let (Some(start), Some(line)) = (
                self.layout.position_for_index(place.start),
                self.layout.line_layout_for_index(place.start),
            ) else {
                continue;
            };
            // the place's width on its line (a place the line wraps in takes the emoticon's side)
            let end = self
                .layout
                .position_for_index(place.end)
                .filter(|end| end.y == start.y && end.x > start.x);
            let width = end.map_or(side, |end| end.x - start.x);
            let laid = &line.unwrapped_layout;
            let metrics = LineMetrics {
                height,
                ascent: laid.ascent,
                descent: laid.descent,
                // GPUI's platforms differ in the sign they give a font's descent
                font_descent: window.text_system().descent(font, laid.font_size).abs(),
            };
            let origin = inline_emoticon_origin(start, width, metrics, side);
            emoticon.prepaint_as_root(origin, size(side, side).map(AvailableSpace::Definite), window, cx);
            *placed = true;
        }
        placed
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _layout: &mut (),
        placed: &mut Vec<bool>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text.paint(window, cx);
        for ((_, emoticon), placed) in self.emoticons.iter_mut().zip(placed.iter()) {
            if *placed {
                emoticon.paint(window, cx);
            }
        }
    }
}
