//! Task 696: a live reaction plays Teams' 3D emoji once on the sender's tile, as a user sees it in the app's own call
//! window on the mock. The mock serves made-up 3D emoji on the path of Teams' CDN (`skype/v2/<id>V4-3D/<side>a.png`,
//! 102 frames, each a disk of its own colour), so the colour of the emoji tells which frame shows.
//!
//! - A made-up person's Like on their gallery tile: the frames come in order at about 24 a second, never from the
//!   first again (played once), and the tile is as before after 4.25 s.
//! - A reaction the CDN has no 3D emoji for (Surprised: the mock answers 404) plays the picker's strip in its place.
//! - Shots in Teams Light and Dark.

use std::thread;
use std::time::Duration;

use image::RgbImage;
use teams_ui::calls::REACTION_SHOWN as ON_TILE;
use ui_sandbox::SandboxResult;

use super::pixels::Rect;
use super::qa_order::{gallery, qa_call, speak};
use super::qa_reactions::{Look, first_person, on_tile, react, strip_png, watch_tile};
use super::{Rig, SETTLE, save_shot, say};

/// How many frames Teams' 3D emoji have, at 24 a second: 4.25 s.
const FRAMES: u32 = 102;
const FPS: f64 = 24.0;
const CDN: &str = "/evergreen-assets";

/// Frame `index`'s colour: a hue round over the frames, each far enough from the next to tell apart. No channel is
/// below 25, so no colour is one of the made-up strip's (each of those has a channel at 0).
fn frame_colour(index: u32) -> [u8; 3] {
    let hue = f64::from(index) / f64::from(FRAMES) * 6.0;
    let channel = |at: f64| {
        let x = (hue - at).rem_euclid(6.0);
        let level = if x < 1.0 {
            x
        } else if x < 3.0 {
            1.0
        } else if x < 4.0 {
            4.0 - x
        } else {
            0.0
        };
        (25_u8..=230)
            .find(|step| f64::from(*step) >= 25.0 + level * 205.0)
            .unwrap_or(230)
    };
    [channel(4.0), channel(2.0), channel(0.0)]
}

/// The made-up 3D emoji, `side` px: its [`FRAMES`] stacked top to bottom, each a disk of [`frame_colour`].
fn sprite_png(side: u32) -> SandboxResult<Vec<u8>> {
    let mut picture = image::RgbaImage::new(side, side * FRAMES);
    let middle = f64::from(side) / 2.0 - 0.5;
    let radius = f64::from(side) * 0.45;
    for index in 0..FRAMES {
        let [r, g, b] = frame_colour(index);
        for y in 0..side {
            for x in 0..side {
                let (dx, dy) = (f64::from(x) - middle, f64::from(y) - middle);
                if dx * dx + dy * dy <= radius * radius {
                    picture.put_pixel(x, index * side + y, image::Rgba([r, g, b, 255]));
                }
            }
        }
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// The frame a look shows at the middle of the emoji on `tile` (its top at the tile's middle, `side` px), when its
/// colour is one of the frames'.
fn frame_seen(shot: &RgbImage, tile: Rect, side: i32) -> Option<u32> {
    let (x, y) = (tile.center().0, tile.y + tile.h / 2 + side / 2);
    let pixel = shot.get_pixel(u32::try_from(x).ok()?, u32::try_from(y).ok()?).0;
    (0..FRAMES)
        .map(|index| {
            let distance = frame_colour(index)
                .iter()
                .zip(pixel)
                .map(|(a, b)| u32::from(a.abs_diff(b)))
                .max()
                .unwrap_or(u32::MAX);
            (distance, index)
        })
        .min()
        .filter(|(distance, _)| *distance <= 6)
        .map(|(_, index)| index)
}

/// The emoji's side on a tile `h` px tall, as the app sizes it.
fn emoji_side(h: i32) -> i32 {
    if h >= 480 {
        140
    } else if h >= 240 {
        112
    } else {
        (h * 43 / 100).clamp(16, 56)
    }
}

/// The frames seen while the reaction showed fully: in order, at about 24 a second, never the first again.
fn played_once(looks: &[Look], tile: Rect, case: &str) -> SandboxResult {
    let side = emoji_side(tile.h);
    let seen: Vec<(Duration, Duration, u32)> = looks
        .iter()
        .filter_map(|look| frame_seen(&look.shot, tile, side).map(|frame| (look.at, look.until, frame)))
        .collect();
    say(&format!(
        "qa696 {case}: frames seen {:?}",
        seen.iter()
            .map(|(at, _, frame)| format!("{:.2}s:{frame}", at.as_secs_f32()))
            .collect::<Vec<_>>()
    ));
    if seen.len() < 2 {
        return Err(format!("{case}: fewer than two looks show a frame of the 3D emoji").into());
    }
    for pair in seen.windows(2) {
        if pair[1].2 < pair[0].2 {
            return Err(format!(
                "{case}: frame {} came after frame {}: it plays again",
                pair[1].2, pair[0].2
            )
            .into());
        }
    }
    if seen.first().map(|first| first.2) == seen.last().map(|last| last.2) {
        return Err(format!("{case}: the 3D emoji doesn't play").into());
    }
    for (at, until, frame) in &seen {
        // the frame is due by the end of the look's shot, and the reaction came at most 1 s after it was sent
        let latest = until.as_secs_f64() * FPS + 3.0;
        let earliest = (at.as_secs_f64() - 1.0) * FPS - 3.0;
        if f64::from(*frame) > latest || f64::from(*frame) < earliest {
            return Err(format!(
                "{case}: frame {frame} showed in the look {at:?}-{until:?}: not 24 frames a second from the start"
            )
            .into());
        }
    }
    Ok(())
}

/// The step (see the module docs).
pub(super) fn reaction_3d(rig: &mut Rig<'_>) -> SandboxResult {
    qa_call(rig)?;
    speak(rig, &[])?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    for side in teams_proto::EMOJI_3D_SIDES {
        let png = sprite_png(u32::from(side))?;
        for id in ["yes", "heart"] {
            runtime.block_on(rig.mock.serve_bytes(
                &format!("{CDN}/skype/v2/{id}V4-3D/{side}a.png"),
                "image/png",
                png.clone(),
            ));
        }
    }
    runtime.block_on(rig.mock.serve_bytes(
        &format!("{CDN}/personal-expressions/v2/assets/emoticons/surprised/default/50_anim_f.png"),
        "image/png",
        strip_png()?,
    ));
    rig.resize((1280, 800))?;
    rig.gallery_size(9, super::qa_order::PEOPLE)?;
    thread::sleep(SETTLE);
    let page = gallery(rig)?;
    let (person, tile) = first_person(&page)?;
    let looks = watch_tile(rig, tile, person, "like")?;
    if let Some(middle) = looks.iter().find(|look| look.at > Duration::from_millis(2_000)) {
        save_shot(rig, &middle.shot, "qa696-reaction-3d-light")?;
    }
    on_tile(&looks, true, "a 3D emoji on a gallery tile")?;
    played_once(&looks, tile, "a 3D emoji on a gallery tile")?;

    let looks = watch_tile(rig, tile, person, "surprised")?;
    on_tile(&looks, true, "the strip in place of a 3D emoji the CDN has not")?;
    let side = emoji_side(tile.h);
    if looks
        .iter()
        .any(|look| frame_seen(&look.shot, tile, side).is_some_and(|frame| frame > 2))
    {
        return Err("a reaction without a 3D emoji shows one".into());
    }

    rig.theme(teams_ui::theme::DARK_KEY)?;
    rig.park()?;
    thread::sleep(SETTLE);
    react(rig, person, "heart")?;
    thread::sleep(Duration::from_millis(2_000));
    rig.save("qa696-reaction-3d-dark")?;
    thread::sleep(ON_TILE);
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    rig.gallery_size(49, super::qa_order::PEOPLE)
}
