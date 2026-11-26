//! Telegram's animated stickers (`.tgs`): a gzipped Lottie animation (512 by 512, at most 3 s, often 60 fps), made
//! frames for a GIF.
//!
//! - **The renderer** is behind [`Renderer`] and [`Animation`], so the import's steps are tested with a made-up one.
//!   The app's is `rasterlottie` (pure Rust, `tiny-skia`): no native library, so each platform builds it as it builds
//!   the app. A build without the `lottie` feature has none ([`renderer`] is `None`), and a `.tgs` is skipped with
//!   [`Skip::NoRenderer`].
//! - **The frames** are rendered at the size a sticker is kept at ([`super::import::KEPT_SIDE`]), never larger than the
//!   animation, at most [`MAX_FPS`] frames a second: a GIF's frame time is in hundredths of a second, and browsers slow
//!   a faster frame down. Each frame's time is cut from the animation's own clock, so the whole loop keeps its length.
//! - **What is read is capped**: the unpacked animation at [`MAX_JSON`], its canvas at
//!   [`super::import::MAX_SOURCE_SIDE`], its frames at [`super::import::MAX_FRAMES`], and what `rasterlottie` is given
//!   to draw by the bounds of `lottie/bounds.rs`.

use std::io::Read as _;

use flate2::read::GzDecoder;
use image::RgbaImage;

use super::import::{KEPT_SIDE, MAX_FPS, MAX_FRAMES, MAX_SOURCE_SIDE, MIN_DELAY_MS, Skip, fit};

/// The largest unpacked animation read, in bytes.
pub const MAX_JSON: u64 = 8 * 1024 * 1024;
/// The highest frame rate an animation may declare.
const MAX_DECLARED_FPS: f64 = 240.0;

/// An animation opened by a [`Renderer`].
pub trait Animation {
    /// Its canvas, in pixels.
    fn size(&self) -> (u32, u32);
    /// Its frames a second.
    fn frame_rate(&self) -> f64;
    /// Its first frame and the frame after its last (Lottie's `ip` and `op`).
    fn span(&self) -> (f64, f64);
    /// Frame `frame` (on the animation's clock) drawn `scale` times its canvas, its pixels not premultiplied.
    ///
    /// # Errors
    /// The frame cannot be drawn.
    fn render(&self, frame: f64, scale: f64) -> Result<RgbaImage, Skip>;
}

/// What opens a Lottie animation.
pub trait Renderer: Sync {
    /// The animation `json` holds.
    ///
    /// # Errors
    /// It is not an animation, or it uses what the renderer cannot draw.
    fn open(&self, json: &str) -> Result<Box<dyn Animation>, Skip>;
}

/// Whether `bytes` are gzipped, as a `.tgs` is.
#[must_use]
pub fn is_tgs(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

/// The app's renderer; `None` in a build without one.
#[must_use]
pub fn renderer() -> Option<&'static dyn Renderer> {
    #[cfg(feature = "lottie")]
    {
        Some(&raster::Raster)
    }
    #[cfg(not(feature = "lottie"))]
    {
        None
    }
}

/// The animation of the `.tgs` `bytes`, unpacked.
fn unpacked(bytes: &[u8]) -> Result<String, Skip> {
    let mut json = String::new();
    GzDecoder::new(bytes)
        .take(MAX_JSON.saturating_add(1))
        .read_to_string(&mut json)
        .map_err(|_| Skip::Unreadable)?;
    if u64::try_from(json.len()).unwrap_or(u64::MAX) > MAX_JSON {
        return Err(Skip::TooLarge);
    }
    Ok(json)
}

/// The hundredths of a second at `index` frames of `rate` a second, rounded.
fn hundredths(index: usize, rate: f64) -> u32 {
    #[expect(clippy::cast_precision_loss, reason = "at most MAX_FRAMES frames")]
    let seconds = index as f64 / rate;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "at most MAX_FRAMES frames at a positive rate"
    )]
    let hundredths = (seconds * 100.0).round() as u32;
    hundredths
}

/// The frames of the `.tgs` `bytes` (see the module docs), each with its time in milliseconds.
///
/// # Errors
/// It is not an animation, it is too large, or `renderer` is `None` or cannot draw it.
pub fn frames(bytes: &[u8], renderer: Option<&dyn Renderer>) -> Result<Vec<(RgbaImage, u32)>, Skip> {
    let json = unpacked(bytes)?;
    let renderer = renderer.ok_or(Skip::NoRenderer)?;
    let animation = renderer.open(&json)?;
    let (width, height) = animation.size();
    if width == 0 || height == 0 {
        return Err(Skip::Unreadable);
    }
    if width > MAX_SOURCE_SIDE || height > MAX_SOURCE_SIDE {
        return Err(Skip::TooBig);
    }
    let fps = animation.frame_rate();
    let (first, end) = animation.span();
    if !(fps.is_finite() && fps > 0.0 && fps <= MAX_DECLARED_FPS && first.is_finite() && end.is_finite()) {
        return Err(Skip::Unreadable);
    }
    let seconds = (end - first) / fps;
    let rate = fps.min(MAX_FPS);
    let count = (seconds * rate).ceil().max(1.0);
    #[expect(clippy::cast_precision_loss, reason = "a small constant")]
    if count > MAX_FRAMES as f64 {
        return Err(Skip::TooManyFrames);
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "at least 1 and at most MAX_FRAMES"
    )]
    let count = count as usize;
    let (kept_width, _) = fit((width, height), KEPT_SIDE);
    let scale = f64::from(kept_width) / f64::from(width);
    (0..count)
        .map(|index| {
            #[expect(clippy::cast_precision_loss, reason = "at most MAX_FRAMES frames")]
            let at = first + index as f64 / rate * fps;
            let frame = animation.render(at, scale)?;
            let ms = (hundredths(index + 1, rate) - hundredths(index, rate)) * 10;
            Ok((frame, ms.max(MIN_DELAY_MS)))
        })
        .collect()
}

#[cfg(feature = "lottie")]
mod bounds;

#[cfg(feature = "lottie")]
mod raster {
    //! The `rasterlottie` renderer.

    use image::RgbaImage;
    use rasterlottie::{PreparedAnimation, RasterlottieError, RenderConfig, Rgba8};

    use super::{Animation, Renderer, Skip};

    pub struct Raster;

    struct Opened {
        prepared: PreparedAnimation,
        size: (u32, u32),
        rate: f64,
        span: (f64, f64),
    }

    fn skip(error: &RasterlottieError) -> Skip {
        match error {
            RasterlottieError::UnsupportedFeatures { .. } => Skip::Unsupported,
            _ => Skip::Unreadable,
        }
    }

    impl Renderer for Raster {
        fn open(&self, json: &str) -> Result<Box<dyn Animation>, Skip> {
            let animation = rasterlottie::Animation::from_json_str(json).map_err(|error| skip(&error))?;
            let size = (animation.width, animation.height);
            let rate = f64::from(animation.frame_rate);
            let span = (f64::from(animation.in_point), f64::from(animation.out_point));
            super::bounds::check(&animation)?;
            let prepared = rasterlottie::Renderer::default()
                .prepare(&animation)
                .map_err(|error| skip(&error))?;
            Ok(Box::new(Opened {
                prepared,
                size,
                rate,
                span,
            }))
        }
    }

    impl Animation for Opened {
        fn size(&self) -> (u32, u32) {
            self.size
        }

        fn frame_rate(&self) -> f64 {
            self.rate
        }

        fn span(&self) -> (f64, f64) {
            self.span
        }

        #[expect(clippy::cast_possible_truncation, reason = "the renderer takes f32")]
        fn render(&self, frame: f64, scale: f64) -> Result<RgbaImage, Skip> {
            let config = RenderConfig::new(Rgba8::TRANSPARENT, scale as f32);
            let drawn = self
                .prepared
                .render_frame(frame as f32, config)
                .map_err(|error| skip(&error))?;
            let mut pixels = drawn.pixels;
            unpremultiply(&mut pixels);
            RgbaImage::from_raw(drawn.width, drawn.height, pixels).ok_or(Skip::Unreadable)
        }
    }

    /// RGBA `pixels` whose colours are premultiplied by their alpha, made straight.
    pub(super) fn unpremultiply(pixels: &mut [u8]) {
        for pixel in pixels.as_chunks_mut::<4>().0 {
            let alpha = u16::from(pixel[3]);
            if alpha == 0 || alpha == 255 {
                continue;
            }
            for channel in &mut pixel[..3] {
                let straight = (u16::from(*channel) * 255 + alpha / 2) / alpha;
                *channel = u8::try_from(straight.min(255)).unwrap_or(u8::MAX);
            }
        }
    }
}

#[cfg(test)]
#[path = "lottie_tests.rs"]
mod tests;
