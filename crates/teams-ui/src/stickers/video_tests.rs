use std::io::Cursor;

use image::AnimationDecoder as _;
use image::codecs::gif::GifDecoder;

use teams_mock::telegram::synthetic_webm;

use super::*;
use crate::stickers::import;

fn made_up(side: u32, at_ms: u64, length_ms: Option<u64>) -> RgbaFrame {
    at_nanos(side, at_ms * 1_000_000, length_ms.map(|ms| ms * 1_000_000))
}

fn at_nanos(side: u32, at: u64, length: Option<u64>) -> RgbaFrame {
    RgbaFrame {
        width: side,
        height: side,
        rgba: vec![9; usize::try_from(side * side * 4).unwrap()],
        at: Duration::from_nanos(at),
        length: length.map(Duration::from_nanos),
    }
}

fn timed(frames: impl IntoIterator<Item = RgbaFrame>) -> Result<Vec<(RgbaImage, u32)>, Skip> {
    let mut timed = Timed::default();
    for frame in frames {
        if timed.take(frame).is_break() {
            break;
        }
    }
    timed.finish(Ok(None))
}

fn delays(frames: &[(RgbaImage, u32)]) -> Vec<u32> {
    frames.iter().map(|(_, ms)| *ms).collect()
}

#[test]
fn the_synthetic_sticker_keeps_its_frames_times_and_clear_corner() {
    let frames = frames(&synthetic_webm().unwrap()).unwrap();
    let mut wanted = vec![30, 40, 30, 30, 40, 30, 30, 40, 30, 30, 40, 30, 30];
    // the alpha decoder often gives no frame for the last block: the frame before it lasts to the declared end
    wanted.extend(if frames.len() == 15 { &[40, 30][..] } else { &[70][..] });
    assert_eq!(delays(&frames), wanted, "30 fps cut on hundredths: 0.5 s in all");
    let (first, _) = &frames[0];
    assert_eq!(first.dimensions(), (64, 64), "never scaled up");
    assert_eq!(first.get_pixel(50, 10).0[3], 0, "the top right is clear");
    assert_eq!(first.get_pixel(10, 50).0[3], 255);
    assert_eq!(first.get_pixel(50, 50).0[3], 255);
}

#[test]
fn the_synthetic_sticker_becomes_a_looping_gif_with_a_clear_corner() {
    let converted = import::convert(&synthetic_webm().unwrap()).unwrap();
    assert!(converted.animated);
    assert_eq!(converted.own, (64, 64));
    let decoder = GifDecoder::new(Cursor::new(&converted.bytes)).unwrap();
    let frames: Vec<_> = decoder.into_frames().collect::<Result<_, _>>().unwrap();
    assert!([14, 15].contains(&frames.len()), "{}", frames.len());
    for frame in &frames {
        let pixels = frame.buffer();
        assert_eq!(pixels.get_pixel(50, 10).0[3], 0, "clear");
        let red = pixels.get_pixel(10, 50).0;
        assert!(red[0] > 240 && red[1] < 16 && red[2] < 16 && red[3] == 255, "{red:?}");
        let green = pixels.get_pixel(50, 50).0;
        assert!(
            green[1] > 240 && green[0] < 16 && green[2] < 16 && green[3] == 255,
            "{green:?}"
        );
    }
    let total: u32 = frames
        .iter()
        .map(|frame| {
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            numerator / denominator
        })
        .sum();
    assert_eq!(total, 500);
}

#[test]
fn a_faster_video_keeps_one_frame_in_each_thirtieth() {
    let sixty = (0..12).map(|index| at_nanos(8, index * 1_000_000_000 / 60, Some(16_666_667)));
    let frames = timed(sixty).unwrap();
    assert_eq!(frames.len(), 6);
    assert_eq!(delays(&frames), [30, 40, 30, 30, 40, 30], "0.2 s in all");
    let thirty_in_milliseconds = (0..6).map(|index| made_up(8, index * 1000 / 30, Some(33)));
    assert_eq!(
        delays(&timed(thirty_in_milliseconds).unwrap()),
        [30, 40, 30, 30, 40, 30],
        "a 30 fps file's times, cut to milliseconds as Matroska keeps them, lose no frame"
    );
}

#[test]
fn a_large_frame_is_scaled_down_and_the_last_frame_lasts_to_the_end() {
    let frames = timed([made_up(512, 0, Some(100)), made_up(512, 100, None)]).unwrap();
    assert_eq!(frames[0].0.dimensions(), (import::KEPT_SIDE, import::KEPT_SIDE));
    assert_eq!(delays(&frames), [100, 30], "a thirtieth when the file gives no end");
    let one = timed([made_up(8, 0, None)]).unwrap();
    assert_eq!(delays(&one), [30]);
    let short = timed([made_up(8, 0, Some(5))]).unwrap();
    assert_eq!(delays(&short), [MIN_DELAY_MS]);

    let declared = |length_ms: u64| {
        let mut timed = Timed::default();
        let _ = timed.take(made_up(8, 0, Some(40)));
        let _ = timed.take(made_up(8, 40, Some(40)));
        delays(&timed.finish(Ok(Some(Duration::from_millis(length_ms)))).unwrap())
    };
    assert_eq!(declared(200), [40, 160], "the last frame lasts to the declared end");
    assert_eq!(
        declared(30),
        [40, 40],
        "a declared end before the frames' own is passed over"
    );
    assert_eq!(declared(60_000), [40, 1000], "at most a second past the last frame");
    let long_last = timed([made_up(8, 0, Some(40)), made_up(8, 40, Some(60_000))]).unwrap();
    assert_eq!(
        delays(&long_last),
        [40, 1000],
        "a frame's own long time is held to a second too"
    );
}

#[test]
fn what_is_too_large_or_damaged_is_skipped() {
    let wide = RgbaFrame {
        width: MAX_SOURCE_SIDE + 1,
        ..made_up(1, 0, None)
    };
    assert_eq!(timed([wide]).unwrap_err(), Skip::TooBig);
    let cut = RgbaFrame {
        rgba: vec![0; 3],
        ..made_up(2, 0, None)
    };
    assert_eq!(timed([cut]).unwrap_err(), Skip::Unreadable);
    let long = (0..=u64::try_from(MAX_FRAMES).unwrap()).map(|index| made_up(1, index * 40, None));
    assert_eq!(timed(long).unwrap_err(), Skip::TooManyFrames);
    assert_eq!(timed([]).unwrap_err(), Skip::Unreadable, "no frame");
    assert_eq!(frames(b"\x1a\x45\xdf\xa3").unwrap_err(), Skip::Unreadable);
    assert_eq!(frames(b"not a video").unwrap_err(), Skip::NotPicture);
}

#[test]
fn each_decode_failure_is_a_skip() {
    for (error, why) in [
        (VideoFileError::NotWebm, Skip::NotPicture),
        (VideoFileError::NoDecoder, Skip::NoVideoDecoder),
        (VideoFileError::Damaged, Skip::Unreadable),
        (VideoFileError::TooSlow, Skip::Unreadable),
        (VideoFileError::TooBig, Skip::TooBig),
    ] {
        let mut timed = Timed::default();
        let _ = timed.take(made_up(1, 0, None));
        assert_eq!(timed.finish(Err(error)).unwrap_err(), why);
    }
    assert_eq!(
        Skip::NoVideoDecoder.to_string(),
        "is a video sticker, and this computer has no decoder for it (GStreamer's VP9 plugins)"
    );
}
