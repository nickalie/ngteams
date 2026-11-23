use std::io::Write as _;
use std::sync::Mutex;

use flate2::Compression;
use flate2::write::GzEncoder;
use image::RgbaImage;

use super::{Animation, MAX_JSON, Renderer, frames, is_tgs};
use crate::stickers::import::{KEPT_SIDE, Skip};

/// The synthetic, marked animation of the fixtures, gzipped as a `.tgs`.
pub fn synthetic_tgs() -> Vec<u8> {
    let json = include_str!("../../../../tests/fixtures/telegram/synthetic-sticker.json");
    gzip(json.as_bytes())
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// A made-up animation: what it declares, and the frames and scales it was asked for.
struct Fake {
    size: (u32, u32),
    rate: f64,
    span: (f64, f64),
    asked: Mutex<Vec<(f64, f64)>>,
}

impl Animation for &'static Fake {
    fn size(&self) -> (u32, u32) {
        self.size
    }

    fn frame_rate(&self) -> f64 {
        self.rate
    }

    fn span(&self) -> (f64, f64) {
        self.span
    }

    fn render(&self, frame: f64, scale: f64) -> Result<RgbaImage, Skip> {
        self.asked.lock().unwrap().push((frame, scale));
        Ok(RgbaImage::new(2, 2))
    }
}

struct FakeRenderer(&'static Fake);

impl Renderer for FakeRenderer {
    fn open(&self, json: &str) -> Result<Box<dyn Animation>, Skip> {
        if json == "refused" {
            return Err(Skip::Unsupported);
        }
        Ok(Box::new(self.0))
    }
}

fn fake(size: (u32, u32), rate: f64, span: (f64, f64)) -> &'static Fake {
    Box::leak(Box::new(Fake {
        size,
        rate,
        span,
        asked: Mutex::new(Vec::new()),
    }))
}

#[test]
fn a_telegram_animation_is_rendered_at_30_frames_a_second_at_the_kept_size() {
    let animation = fake((512, 512), 60.0, (0.0, 180.0));
    let made = frames(&gzip(b"{}"), Some(&FakeRenderer(animation))).unwrap();
    assert_eq!(made.len(), 90);
    let total: u32 = made.iter().map(|(_, ms)| ms).sum();
    assert_eq!(total, 3000);
    assert!(made.iter().all(|(_, ms)| *ms == 30 || *ms == 40));
    let asked = animation.asked.lock().unwrap();
    assert_eq!(asked[0], (0.0, f64::from(KEPT_SIDE) / 512.0));
    assert!((asked[1].0 - 2.0).abs() < 1e-9);
    assert!((asked[89].0 - 178.0).abs() < 1e-9);
}

#[test]
fn a_slow_or_small_animation_keeps_its_own_rate_and_size() {
    let animation = fake((100, 50), 10.0, (5.0, 15.0));
    let made = frames(&gzip(b"{}"), Some(&FakeRenderer(animation))).unwrap();
    assert_eq!(made.len(), 10);
    assert!(made.iter().all(|(_, ms)| *ms == 100));
    let asked = animation.asked.lock().unwrap();
    assert_eq!(asked[0], (5.0, 1.0));
    assert!((asked[9].0 - 14.0).abs() < 1e-9);
}

#[test]
fn an_animation_out_of_the_limits_is_skipped_with_its_reason() {
    let one = |animation| frames(&gzip(b"{}"), Some(&FakeRenderer(animation)));
    assert_eq!(one(fake((5000, 512), 60.0, (0.0, 60.0))), Err(Skip::TooBig));
    assert_eq!(one(fake((0, 512), 60.0, (0.0, 60.0))), Err(Skip::Unreadable));
    assert_eq!(one(fake((512, 512), 0.0, (0.0, 60.0))), Err(Skip::Unreadable));
    assert_eq!(one(fake((512, 512), 1000.0, (0.0, 60.0))), Err(Skip::Unreadable));
    assert_eq!(one(fake((512, 512), f64::NAN, (0.0, 60.0))), Err(Skip::Unreadable));
    assert_eq!(one(fake((512, 512), 30.0, (0.0, 300.0))), Err(Skip::TooManyFrames));
    assert_eq!(one(fake((512, 512), 30.0, (0.0, f64::INFINITY))), Err(Skip::Unreadable));
    assert_eq!(one(fake((512, 512), 30.0, (0.0, 0.0))).map(|made| made.len()), Ok(1));
    let renderer = FakeRenderer(fake((512, 512), 60.0, (0.0, 60.0)));
    assert_eq!(frames(&gzip(b"refused"), Some(&renderer)), Err(Skip::Unsupported));
    assert_eq!(frames(&gzip(b"{}"), None), Err(Skip::NoRenderer));
    assert_eq!(frames(b"\x1f\x8bnot gzip", Some(&renderer)), Err(Skip::Unreadable));
    let huge = vec![b' '; usize::try_from(MAX_JSON).unwrap() + 1];
    assert_eq!(frames(&gzip(&huge), Some(&renderer)), Err(Skip::TooLarge));
}

#[test]
fn a_tgs_is_told_by_its_gzip_header() {
    assert!(is_tgs(&synthetic_tgs()));
    assert!(!is_tgs(b"RIFF....WEBP"));
    assert!(!is_tgs(b""));
}

#[cfg(feature = "lottie")]
#[test]
fn the_synthetic_tgs_renders_to_an_animated_gif_sticker() {
    use image::AnimationDecoder as _;

    let made = frames(&synthetic_tgs(), super::renderer()).unwrap();
    assert_eq!(made.len(), 30);
    let (first, _) = &made[0];
    assert_eq!(first.dimensions(), (KEPT_SIDE, KEPT_SIDE));
    let scale = f64::from(KEPT_SIDE) / 512.0;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "inside the frame"
    )]
    let at = |x: f64| (x * scale) as u32;
    assert_eq!(first.get_pixel(at(156.0), at(256.0)).0, [0, 0, 255, 255]);
    assert_eq!(first.get_pixel(at(356.0), at(256.0)).0[3], 0);
    let (last, _) = &made[29];
    assert_eq!(last.get_pixel(at(350.0), at(256.0)).0, [0, 0, 255, 255]);
    assert_eq!(last.get_pixel(at(100.0), at(256.0)).0[3], 0);

    let sticker = crate::stickers::import::convert(&synthetic_tgs()).unwrap();
    assert!(sticker.animated);
    assert_eq!(sticker.own, (KEPT_SIDE, KEPT_SIDE));
    assert_eq!(sticker.shown, (180, 180));
    assert_eq!(
        teams_proto::PastedFormat::sniff(&sticker.bytes),
        Some(teams_proto::PastedFormat::Gif)
    );
    let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(sticker.bytes)).unwrap();
    assert_eq!(decoder.into_frames().count(), 30);
}

#[cfg(feature = "lottie")]
#[test]
fn a_premultiplied_pixel_is_made_straight() {
    let mut pixels = [0, 0, 0, 0, 10, 20, 30, 255, 64, 32, 0, 128, 200, 200, 200, 100];
    super::raster::unpremultiply(&mut pixels);
    assert_eq!(
        pixels,
        [0, 0, 0, 0, 10, 20, 30, 255, 128, 64, 0, 128, 255, 255, 255, 100]
    );
}
