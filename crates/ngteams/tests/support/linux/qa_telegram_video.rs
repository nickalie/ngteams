//! QA of task 655 (round 1), in the same sandbox run as task 651's checks: Telegram video stickers (`.webm`).
//!
//! - **The picker**: the folder's video sticker shows, its clear corner shows the picker's ground, in Teams Light and
//!   Dark. Whether it moves there is said (the picker draws the first frame of each animated sticker).
//! - **The chat**: a sends the Telegram pack's video sticker; b sees it bare, it plays, and its clear corner shows the
//!   chat's ground, in Teams Light and Dark.
//! - **Bad files**: a folder with a damaged `.webm`, one cut short, a text file named `.webm`, one with too many frames
//!   and one too wide, beside a good `.webm`, `.tgs`, `.webp`, GIF and PNG: each bad file has its skip line, the import
//!   ends in time, and the good files import.
//! - **No decoder**: a run of the app with every VP8 and VP9 decoder hidden from `GStreamer` skips the `.webm` with its
//!   note, and imports the other files.
//!
//! Every file is made up: the marked synthetic fixtures, and pictures made here.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};
use std::{fs, thread};

use image::{ImageFormat, RgbImage, Rgba, RgbaImage};
use serde_json::Value;
use teams_mock::AB_CHAT;
use ui_sandbox::{SandboxResult, wait_until};

use super::{
    BALL, DISC, FOLDER, GOOD_TITLE, MESSAGES, PICKER, Rect, Rgb, User, VIDEO_EMOJI, WAIT, blobs, differ, disc_webp,
    ensure, faults_to_result, items, near, pack_named, picker_open, pictures, pixel, say, shows_pack_stickers, skipped,
    synthetic_tgs, video,
};

/// The folder of good and bad files, and the folder the run without decoders imports.
pub(super) const BAD: &str = "Qa tg bad";
pub(super) const NO_DECODER: &str = "Qa tg nodecoder";
/// The run of the app with no VP8 or VP9 decoder, and what hides them: each decoder `decodebin` could pick, ranked
/// below every autoplugged element, as on a computer without `GStreamer`'s VP9 plugins.
pub(super) const NO_DECODER_RUN: &str = "nodecoder";
pub(super) const NO_VP9_RANKS: &str = "vp9dec:NONE,vp8dec:NONE,vp9alphadecodebin:NONE,vp8alphadecodebin:NONE,\
avdec_vp9:NONE,avdec_vp8:NONE,nvvp9dec:NONE,nvvp8dec:NONE,vavp9dec:NONE,vavp8dec:NONE,vp9parse:NONE";

/// The fixture's opaque quadrants: red at the left, green at the bottom right.
const RED: Rgb = [255, 0, 0];
const GREEN: Rgb = [0, 255, 0];
/// The made-up GIF's face, which flips between two places.
const MAGENTA: Rgb = [200, 0, 200];

const NO_DECODER_NOTE: &str = "is a video sticker, and this computer has no decoder for it (GStreamer's VP9 plugins)";

/// The marked fixtures of this QA, beside the one of the task.
const LONG: &str = include_str!("../../../../../tests/fixtures/telegram/synthetic-sticker-long.webm.hex");
const WIDE: &str = include_str!("../../../../../tests/fixtures/telegram/synthetic-sticker-wide.webm.hex");

/// The bytes of a hex fixture, its `#` lines left out.
fn hex(text: &str) -> SandboxResult<Vec<u8>> {
    let digits: String = text.lines().filter(|line| !line.starts_with('#')).collect();
    let bytes: Option<Vec<u8>> = digits
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    bytes.ok_or_else(|| "a hex fixture has a pair that is not hex".into())
}

/// An animated GIF of two frames: a magenta square that moves, on a clear ground.
fn gif() -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
        for offset in [20, 70] {
            let mut picture = RgbaImage::new(128, 128);
            for (x, y, pixel) in picture.enumerate_pixels_mut() {
                if (offset..offset + 40).contains(&x) && (40..80).contains(&y) {
                    *pixel = Rgba([MAGENTA[0], MAGENTA[1], MAGENTA[2], 255]);
                }
            }
            let delay = image::Delay::from_numer_denom_ms(200, 1);
            encoder.encode_frame(image::Frame::from_parts(picture, 0, 0, delay))?;
        }
    }
    Ok(bytes)
}

/// A still PNG: a disc as the `.webp` one.
fn png() -> SandboxResult<Vec<u8>> {
    let webp = disc_webp(DISC)?;
    let picture = image::load_from_memory(&webp)?;
    let mut bytes = io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

/// The fixture with its tracks' bytes changed: the header reads, the streams do not.
fn damaged() -> SandboxResult<Vec<u8>> {
    let mut bytes = video()?;
    let tracks = bytes
        .windows(4)
        .position(|id| id == [0x16, 0x54, 0xae, 0x6b])
        .ok_or("the fixture has no tracks")?;
    for byte in bytes.iter_mut().skip(tracks + 8) {
        *byte ^= 0x5a;
    }
    Ok(bytes)
}

/// The fixture cut a little into its first cluster.
fn cut_short() -> SandboxResult<Vec<u8>> {
    let mut bytes = video()?;
    let cluster = bytes
        .windows(4)
        .position(|id| id == [0x1f, 0x43, 0xb6, 0x75])
        .ok_or("the fixture has no cluster")?;
    bytes.truncate(cluster + 40);
    Ok(bytes)
}

/// The bad files, each with the words its skip line must hold.
fn bad_files() -> SandboxResult<Vec<(&'static str, Vec<u8>, &'static str)>> {
    Ok(vec![
        ("bad_damaged.webm", damaged()?, "can't be read"),
        ("bad_cut.webm", cut_short()?, "can't be read"),
        (
            "bad_text.webm",
            b"Made-up words, not a video.\n".to_vec(),
            "isn't a PNG, WebP or GIF picture or a Telegram sticker",
        ),
        ("bad_long.webm", hex(LONG)?, "has more than 200 frames"),
        ("bad_wide.webm", hex(WIDE)?, "is larger than 4096 pixels"),
    ])
}

/// The good files of the bad folder: (name, animated).
const GOOD_FILES: [(&str, bool); 5] = [
    ("good_ball.tgs", true),
    ("good_clip.webm", true),
    ("good_disc.webp", false),
    ("good_flip.gif", true),
    ("good_still.png", false),
];

/// The bad folder, and the folder the run without decoders imports, in `root`.
pub(super) fn make_sources(root: &Path) -> SandboxResult {
    let dir = root.join(BAD);
    fs::create_dir_all(&dir)?;
    for (name, bytes) in [
        ("good_ball.tgs", synthetic_tgs()?),
        ("good_clip.webm", video()?),
        ("good_disc.webp", disc_webp(DISC)?),
        ("good_flip.gif", gif()?),
        ("good_still.png", png()?),
    ] {
        fs::write(dir.join(name), bytes)?;
    }
    for (name, bytes, _) in bad_files()? {
        fs::write(dir.join(name), bytes)?;
    }
    let dir = root.join(NO_DECODER);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("nd_ball.tgs"), synthetic_tgs()?)?;
    fs::write(dir.join("nd_clip.webm"), video()?)?;
    fs::write(dir.join("nd_disc.webp"), disc_webp(DISC)?)?;
    Ok(())
}

/// The video sticker in `area`: its red left half, the green quadrant beside it, and the box of both.
fn video_box(image: &RgbImage, area: Rect) -> Option<Rect> {
    let red = blobs(image, RED, area, 60).into_iter().max_by_key(|blob| blob.y)?;
    let green = blobs(image, GREEN, area, 30)
        .into_iter()
        .find(|blob| blob.x >= red.right() - 4 && blob.x <= red.right() + 8 && blob.y > red.y)?;
    Some(Rect {
        x: red.x,
        y: red.y,
        w: green.right() - red.x,
        h: red.h.max(green.bottom() - red.y),
    })
}

/// The clear quadrant (top right) of the sticker in `sticker`: its points that are not `ground` on their row.
fn clear_faults(image: &RgbImage, sticker: Rect, ground_x: i32, what: &str) -> Vec<String> {
    let mut faults = Vec::new();
    let (x0, y0) = (sticker.x + sticker.w / 2 + 3, sticker.y + 3);
    let (x1, y1) = (sticker.right() - 3, sticker.y + sticker.h / 2 - 3);
    let mut differing = Vec::new();
    for y in (y0..y1).step_by(3) {
        let Some(ground) = pixel(image, ground_x, y) else {
            continue;
        };
        for x in (x0..x1).step_by(3) {
            if let Some(here) = pixel(image, x, y)
                && !near(here, ground, 8)
            {
                differing.push(((x, y), here, ground));
            }
        }
    }
    say(&format!(
        "  {what}: the sticker {sticker:?}, clear points not the ground: {}",
        differing.len()
    ));
    if !differing.is_empty() {
        let first: Vec<_> = differing.iter().take(4).collect();
        faults.push(format!(
            "{what}: the clear part does not show the ground: (point, colour, ground) {first:?}"
        ));
    }
    faults
}

/// Whether the sticker in `area` changes between shots within 3 s, and its box in the last shot.
fn moves(user: &mut User<'_>, area: Rect, name: &str) -> SandboxResult<(bool, Option<Rect>)> {
    user.park()?;
    let first = image::open(user.shot(&format!("{name}-0"))?)?.to_rgb8();
    let Some(sticker) = video_box(&first, area) else {
        return Ok((false, None));
    };
    let start = Instant::now();
    let mut frame = 1;
    while start.elapsed() < Duration::from_secs(3) {
        thread::sleep(Duration::from_millis(70));
        let now = image::open(user.shot(&format!("{name}-{}", frame % 6))?)?.to_rgb8();
        frame += 1;
        if differ(&first, &now, sticker) > 4 {
            say(&format!("  {name}: changed after {frame} shots"));
            return Ok((true, Some(sticker)));
        }
    }
    say(&format!("  {name}: no change in {frame} shots"));
    Ok((false, Some(sticker)))
}

/// The folder's pack open in the picker: the video sticker shows, its clear corner shows the picker's ground, in
/// Teams Light and Dark. Whether it moves there is said.
pub(super) fn video_shows_in_picker(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let state = user.open_stickers_by_keys()?;
        let id = pack_named(&state, FOLDER)
            .and_then(|pack| pack["id"].as_str())
            .ok_or("no folder pack")?
            .to_owned();
        user.key_pick(&format!("sticker-pack-{id}"))?;
        user.until("the pack open", |state| shows_pack_stickers(state, &id))?;
        let image = user.image(&format!("video-in-picker-{theme}"))?;
        let Some(sticker) = video_box(&image, PICKER) else {
            faults.push(format!("{theme}: the video sticker does not show in the picker"));
            continue;
        };
        // the sticker's tile, left of its red half
        let ground_x = sticker.x - 3;
        faults.extend(clear_faults(&image, sticker, ground_x, &format!("picker, {theme}")));
        let (moved, _) = moves(user, PICKER, &format!("video-in-picker-{theme}-plays"))?;
        say(&format!("  the video sticker moves in the picker ({theme}): {moved}"));
        user.press("escape")?;
    }
    faults_to_result(&faults)
}

/// a picks the Telegram pack's video sticker and sends it; b sees it bare, it plays, and its clear corner shows the
/// chat's ground, in Teams Light and Dark.
pub(super) fn send_video_b_sees_it_play(user: &mut User<'_>) -> SandboxResult {
    let mut faults = Vec::new();
    let state = user.open_stickers_by_keys()?;
    let pack = pack_named(&state, GOOD_TITLE).ok_or("no Telegram pack")?.clone();
    let id = pack["id"].as_str().unwrap_or_default().to_owned();
    let file = pack["stickers"]
        .as_array()
        .and_then(|stickers| stickers.iter().find(|sticker| sticker["label"] == VIDEO_EMOJI))
        .and_then(|sticker| sticker["file"].as_str())
        .ok_or("no video sticker in the Telegram pack")?
        .to_owned();
    user.key_pick(&format!("sticker-pack-{id}"))?;
    user.until("the pack open", |state| shows_pack_stickers(state, &id))?;
    user.serve_upload(&id, &file)?;
    let number = file.split('.').next().unwrap_or_default();
    let before = user.sent_with("POST", "schema.skype.com/Sticker").len();
    user.key_pick(&format!("sticker-{id}-{number}"))?;
    let state = user.until("the video sticker in the box, sent to the media service", |state| {
        let pictures = pictures(state);
        !picker_open(state) && pictures.len() == 1 && pictures[0]["state"] == "sent"
    })?;
    if pictures(&state)[0]["sticker"] != VIDEO_EMOJI {
        faults.push(format!("the sticker in the box: {}", pictures(&state)[0]));
    }
    user.image("video-send-in-box")?;
    user.ask("focus-box")?;
    user.press("enter")?;
    let sent = wait_until(WAIT, || {
        let all = user.sent_with("POST", "schema.skype.com/Sticker");
        (all.len() > before).then(|| all.last().cloned().unwrap_or_default())
    })
    .ok_or("no video sticker message went to the chat service")?;
    if !sent.contains(VIDEO_EMOJI) && !sent.contains("\\ud83d\\ude39") {
        faults.push(format!(
            "the message does not name the video sticker by its emoji: {sent}"
        ));
    }
    user.switch("b")?;
    user.open(AB_CHAT)?;
    let state = user.until("a's video sticker in b's chat", |state| {
        items(state)
            .iter()
            .any(|item| item["label"] == VIDEO_EMOJI && item["own"] == false)
    })?;
    let item: Value = items(&state)
        .into_iter()
        .rfind(|item| item["label"] == VIDEO_EMOJI)
        .unwrap_or_default();
    if item["bare"] != true || item["sticker_alone"] != true {
        faults.push(format!("b's message is not a bare sticker: {item}"));
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let image = user.image(&format!("b-sees-video-sticker-{theme}"))?;
        let (moved, sticker) = moves(user, MESSAGES, &format!("b-sees-video-sticker-{theme}-plays"))?;
        let Some(sticker) = sticker.or_else(|| video_box(&image, MESSAGES)) else {
            faults.push(format!("{theme}: b sees no video sticker"));
            continue;
        };
        if !moved {
            faults.push(format!("{theme}: the video sticker does not play for b"));
        }
        let ground_x = MESSAGES.right() - 80;
        faults.extend(clear_faults(&image, sticker, ground_x, &format!("b's chat, {theme}")));
    }
    faults_to_result(&faults)
}

/// The summary's lines for the bad folder: each bad file's, and none for a good file.
fn skip_faults(lines: &[String]) -> SandboxResult<Vec<String>> {
    let mut faults = Vec::new();
    for (name, _, why) in bad_files()? {
        let wanted = format!("{name} {why}");
        if !lines.iter().any(|line| line == &wanted) {
            faults.push(format!("no skip line {wanted:?}: {lines:?}"));
        }
    }
    for (name, _) in GOOD_FILES {
        if lines.iter().any(|line| line.starts_with(name)) {
            faults.push(format!("the good file {name} was skipped: {lines:?}"));
        }
    }
    Ok(faults)
}

/// The pack of the good files: three animated stickers (`.tgs`, `.webm`, GIF) as GIFs, two still ones as PNGs.
fn good_pack_faults(state: &Value, name: &str) -> Vec<String> {
    let Some(pack) = pack_named(state, name) else {
        return vec![format!("no pack {name:?}: {}", state["shelf"]["packs"])];
    };
    let stickers = pack["stickers"].as_array().cloned().unwrap_or_default();
    let animated = stickers.iter().filter(|sticker| sticker["animated"] == true).count();
    let wanted = GOOD_FILES.iter().filter(|(_, animated)| *animated).count();
    let mut faults = Vec::new();
    if stickers.len() != GOOD_FILES.len() || animated != wanted {
        faults.push(format!("{name}: the stickers {stickers:?}"));
    }
    faults
}

/// "Import a folder" of good and bad files: each bad one has its skip line, the import ends in time (no hang), the good
/// ones import (GIF, `.tgs` and still pictures as before the task), and the summary shows in Teams Light and Dark.
pub(super) fn bad_video_files_skip(user: &mut User<'_>) -> SandboxResult {
    user.world.chooser.queue(vec![user.world.sources.join(BAD)]);
    let start = Instant::now();
    user.pick_import("sticker-import-folder")?;
    let (_, state) = user.finish_import("import-bad")?;
    let took = start.elapsed();
    say(&format!("  the import took {took:?}"));
    let lines = skipped(&state);
    say(&format!("  skipped: {lines:?}"));
    let mut faults = skip_faults(&lines)?;
    faults.extend(good_pack_faults(&state, BAD));
    if took > Duration::from_secs(60) {
        faults.push(format!("the import took {took:?}"));
    }
    user.image("import-bad-summary-light")?;
    user.ask("theme-dark")?;
    user.image("import-bad-summary-dark")?;
    user.ask("theme-light")?;
    let id = pack_named(&state, BAD)
        .and_then(|pack| pack["id"].as_str())
        .unwrap_or_default()
        .to_owned();
    // a fifth pack is behind "See all": the summary's Show opens it
    user.key_pick("sticker-import-open")?;
    user.until("the bad folder's pack open", |state| shows_pack_stickers(state, &id))?;
    let faces = [
        (RED, "the .webm"),
        (BALL, "the .tgs"),
        (DISC, "the stills"),
        (MAGENTA, "the GIF"),
    ];
    let missing = |image: &RgbImage| -> Vec<&str> {
        faces
            .iter()
            .filter(|(colour, _)| blobs(image, *colour, PICKER, 40).is_empty())
            .map(|(_, what)| *what)
            .collect()
    };
    // the tiles fill as their pictures load
    let start = Instant::now();
    let mut image = user.image("bad-folder-pack-in-picker")?;
    while !missing(&image).is_empty() && start.elapsed() < WAIT {
        thread::sleep(Duration::from_millis(300));
        image = user.image("bad-folder-pack-in-picker")?;
    }
    say(&format!("  the pack's tiles drew in {:?}", start.elapsed()));
    for what in missing(&image) {
        faults.push(format!("{what} sticker does not show in the picker"));
    }
    faults_to_result(&faults)
}

/// The run with no VP8 or VP9 decoder: the `.webm` is skipped with its note, the other files import.
pub(super) fn no_decoder_skip(user: &mut User<'_>) -> SandboxResult {
    user.world.chooser.queue(vec![user.world.sources.join(NO_DECODER)]);
    user.pick_import("sticker-import-folder")?;
    let (_, state) = user.finish_import("import-nodecoder")?;
    user.image("import-nodecoder-summary-light")?;
    user.ask("theme-dark")?;
    user.image("import-nodecoder-summary-dark")?;
    user.ask("theme-light")?;
    let lines = skipped(&state);
    say(&format!("  skipped: {lines:?}"));
    let mut faults = Vec::new();
    let wanted = format!("nd_clip.webm {NO_DECODER_NOTE}");
    if lines != [wanted.clone()] {
        faults.push(format!("the skipped lines {lines:?}, [{wanted:?}] expected"));
    }
    match pack_named(&state, NO_DECODER) {
        Some(pack) => {
            let count = pack["stickers"].as_array().map_or(0, Vec::len);
            ensure(count == 2, || format!("the pack has {count} stickers, 2 expected"))
                .unwrap_or_else(|error| faults.push(error.to_string()));
        }
        None => faults.push(format!("no pack {NO_DECODER:?}")),
    }
    faults_to_result(&faults)
}
