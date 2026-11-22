//! QA of task 696 (round 1): Teams' 3D emoji on a live reaction's tile, as a user sees it in the app's own call window
//! on the mock. The mock serves made-up 3D emoji on the path of Teams' CDN (`skype/v2/<id>V4-3D/<side>a.png`, 102
//! frames). Each frame is a code of 12 rows: each row is magenta on one half and green on the other. The rows carry a
//! marker at the top and the bottom, the reaction (3 bits) and the frame's number (7 bits). The test reads a row from
//! the difference of its two halves, so it reads the frame while the emoji fades in and out too.
//!
//! - Size: 56 px on a tile of the 9-gallery, 112 on one of the 4-gallery, 140 on the speaker's tile, less on a small
//!   tile; the app reads the CDN's side that is not smaller.
//! - Play: the frames come in order at about 24 a second, at the start, the middle and the end, and never again from
//!   the first; the tile is as before after 4.25 s. In Teams Light and Dark.
//! - Several in a row: a newer reaction of the same person takes the place of the older and plays from its own start;
//!   two people's reactions play at the same time, each on its own tile.
//! - A 3D emoji the CDN has not (404): the picker's strip plays. A slow one: the strip plays until it comes, then the
//!   3D emoji at the frame due (Applause, and Surprised at 112 px as a second chance when too few looks land).
//! - "Play animations" off: the still picture, no 3D frame.
//! - A sender not on the stage: the small float, as before; the app reads no 3D emoji for it.

use std::thread;
use std::time::Duration;

use image::RgbImage;
use teams_ui::calls::REACTION_SHOWN as ON_TILE;
use ui_sandbox::SandboxResult;

use super::pixels::Rect;
use super::qa_order::{PEOPLE, Page, Seen, gallery, qa_call, speak};
use super::qa_reactions::{
    Look, STRIP_FRAMES, check_window_shots, no_top_line, react, strip_frame_faded, strip_png, watch,
};
use super::{Rig, SETTLE, View, save_shot, say};

/// How many frames Teams' 3D emoji have, at 24 a second: 4.25 s.
const FRAMES: u32 = 102;
const FPS: f64 = 24.0;
/// How long a reaction's tile is watched: its 4.25 s and the time after it.
const WATCH: Duration = Duration::from_millis(6_800);
const ROWS: u32 = 12;
const MAGENTA: [u8; 3] = [255, 0, 255];
const GREEN: [u8; 3] = [0, 255, 0];
const CDN: &str = "/evergreen-assets";
/// How long the slow CDN holds back its answer: two slow shots of the strip before it, one of the 3D emoji after.
const SLOW: Duration = Duration::from_millis(2_200);
/// A look this long after a reaction's end shows the tile as before.
const AFTER_END: Duration = Duration::from_millis(800);
/// The reactions as the mock sends them, their ids on the CDN, and their code in the made-up emoji.
const REACTIONS: [(&str, &str, u32); 5] = [
    ("like", "yes", 1),
    ("heart", "heart", 2),
    ("clap", "clap", 3),
    ("laugh", "laugh", 4),
    ("surprised", "surprised", 5),
];

fn cdn_id(reaction: &str) -> &'static str {
    REACTIONS
        .iter()
        .find(|(name, _, _)| *name == reaction)
        .map_or("none", |(_, id, _)| id)
}

fn code_of(reaction: &str) -> u32 {
    REACTIONS
        .iter()
        .find(|(name, _, _)| *name == reaction)
        .map_or(0, |(_, _, code)| *code)
}

/// The bits of frame `index` of reaction `code`, top to bottom.
fn bits(code: u32, index: u32) -> [bool; ROWS as usize] {
    let mut rows = [false; ROWS as usize];
    rows[0] = true;
    for bit in 0..3 {
        rows[1 + bit] = code >> (2 - bit) & 1 == 1;
    }
    for bit in 0..7 {
        rows[4 + bit] = index >> (6 - bit) & 1 == 1;
    }
    rows
}

/// The made-up 3D emoji of reaction `code`, `side` px: its [`FRAMES`] stacked top to bottom, each its code.
fn sprite_png(code: u32, side: u32) -> SandboxResult<Vec<u8>> {
    let mut picture = image::RgbaImage::new(side, side * FRAMES);
    for index in 0..FRAMES {
        let rows = bits(code, index);
        for y in 0..side {
            let row = rows[usize::try_from(y * ROWS / side)?];
            for x in 0..side {
                let left = x < side / 2;
                let [r, g, b] = if left == row { MAGENTA } else { GREEN };
                picture.put_pixel(x, index * side + y, image::Rgba([r, g, b, 255]));
            }
        }
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

fn pixel(shot: &RgbImage, x: i32, y: i32) -> Option<[i32; 3]> {
    let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
    (x < shot.width() && y < shot.height()).then(|| shot.get_pixel(x, y).0.map(i32::from))
}

/// The made-up emoji's box on `tile` in a look at its full opacity: the px of its pure magenta and green.
fn emoji_box(shot: &RgbImage, tile: Rect) -> Option<Rect> {
    let pure = |p: [i32; 3]| {
        [MAGENTA, GREEN].iter().any(|colour| {
            p.iter()
                .zip(colour)
                .all(|(got, wanted)| (got - i32::from(*wanted)).abs() <= 30)
        })
    };
    let mut bounds: Option<Rect> = None;
    let mut count = 0;
    for y in tile.y..tile.bottom() {
        for x in tile.x..tile.right() {
            if pixel(shot, x, y).is_some_and(pure) {
                count += 1;
                let dot = Rect { x, y, w: 1, h: 1 };
                bounds = Some(bounds.map_or(dot, |known| known.union(dot)));
            }
        }
    }
    bounds.filter(|_| count >= 100)
}

/// The reaction and frame the emoji in `area` shows in `shot`, at any opacity of 10 % or more; `None` when its code
/// does not read (no emoji, the strip, the still picture). When the left half does not read (a small tile's name
/// label covers it), the right half alone.
fn read_code(shot: &RgbImage, area: Rect) -> Option<(u32, u32)> {
    read_both_halves(shot, area).or_else(|| read_right_half(shot, area))
}

/// [`read_code`] from the right half of each row alone: green there is a 1, magenta a 0.
fn read_right_half(shot: &RgbImage, area: Rect) -> Option<(u32, u32)> {
    let right = area.x + area.w * 3 / 4;
    let mut rows = [false; ROWS as usize];
    for (row, bit) in rows.iter_mut().enumerate() {
        let row = i32::try_from(row).ok()?;
        let y = area.y + (area.h * (2 * row + 1)) / (2 * i32::try_from(ROWS).ok()?);
        let [red, green, blue] = pixel(shot, right, y)?;
        let lean = green - i32::midpoint(red, blue);
        *bit = if lean >= 40 {
            true
        } else if lean <= -40 {
            false
        } else {
            return None;
        };
    }
    decode(rows)
}

fn decode(rows: [bool; ROWS as usize]) -> Option<(u32, u32)> {
    if !rows[0] || rows[ROWS as usize - 1] {
        return None;
    }
    let number = |from: usize, to: usize| rows[from..to].iter().fold(0, |sum, bit| sum * 2 + u32::from(*bit));
    let (code, frame) = (number(1, 4), number(4, 11));
    (frame < FRAMES).then_some((code, frame))
}

/// [`read_code`] from the difference of the two halves of each row.
fn read_both_halves(shot: &RgbImage, area: Rect) -> Option<(u32, u32)> {
    let (left, right) = (area.x + area.w / 4, area.x + area.w * 3 / 4);
    let mut rows = [false; ROWS as usize];
    for (row, bit) in rows.iter_mut().enumerate() {
        let row = i32::try_from(row).ok()?;
        let y = area.y + (area.h * (2 * row + 1)) / (2 * i32::try_from(ROWS).ok()?);
        let (l, r) = (pixel(shot, left, y)?, pixel(shot, right, y)?);
        let (red, green, blue) = (l[0] - r[0], l[1] - r[1], l[2] - r[2]);
        // magenta on the left: red and blue up, green down, by the same amount
        let level = red.min(blue).min(-green);
        let other = (-red).min(-blue).min(green);
        *bit = if level >= 25 {
            true
        } else if other >= 25 {
            false
        } else {
            return None;
        };
    }
    decode(rows)
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

/// The CDN's side the app reads for an emoji `side` px.
fn cdn_side(side: i32) -> u16 {
    teams_proto::EMOJI_3D_SIDES
        .iter()
        .copied()
        .find(|cdn| i32::from(*cdn) >= side)
        .unwrap_or(140)
}

/// Where the emoji shows on `tile` as the app places it: centred, its top at the tile's middle.
fn expected_area(tile: Rect) -> Rect {
    let side = emoji_side(tile.h);
    Rect {
        x: tile.center().0 - side / 2,
        y: tile.y + tile.h / 2,
        w: side,
        h: side,
    }
}

/// The paths of the 3D emoji the app read so far.
fn reads_3d(rig: &Rig<'_>) -> SandboxResult<Vec<String>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let requests = runtime.block_on(rig.mock.received_requests());
    Ok(requests
        .iter()
        .map(|request| request.url.path().to_owned())
        .filter(|path| path.contains("V4-3D/"))
        .collect())
}

/// A look's code: when its shot started and ended, and the reaction and frame it read.
#[derive(Clone, Copy, Debug)]
struct Seen3d {
    at: Duration,
    until: Duration,
    code: u32,
    frame: u32,
}

fn codes(looks: &[Look], area: Rect) -> Vec<Seen3d> {
    looks
        .iter()
        .filter_map(|look| {
            read_code(&look.shot, area).map(|(code, frame)| Seen3d {
                at: look.at,
                until: look.until,
                code,
                frame,
            })
        })
        .collect()
}

fn listed(seen: &[Seen3d]) -> String {
    seen.iter()
        .map(|seen| {
            format!(
                "{:.2}-{:.2}s:{}/{}",
                seen.at.as_secs_f32(),
                seen.until.as_secs_f32(),
                seen.code,
                seen.frame
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A reaction watched on a tile: its looks, and the codes they read.
struct Watched {
    tile: Rect,
    looks: Vec<Look>,
    seen: Vec<Seen3d>,
    /// The codes of each try so far, this one's last.
    plays: Vec<Vec<Seen3d>>,
    /// The looks of the tries before this one, without their shots.
    earlier: Vec<Glance>,
}

/// When a look was and how much of the tile differed from the shot before the reaction.
#[derive(Clone, Copy, Debug)]
struct Glance {
    at: Duration,
    until: Duration,
    from_start: i64,
}

impl From<&Look> for Glance {
    fn from(look: &Look) -> Self {
        Self {
            at: look.at,
            until: look.until,
            from_start: look.from_start,
        }
    }
}

impl Watched {
    /// A look of any try started after the reaction's end, `ends` after the sending.
    fn ended_seen(&self, ends: Duration) -> bool {
        self.looks
            .iter()
            .map(Glance::from)
            .chain(self.earlier.iter().copied())
            .any(|glance| glance.at > ends + AFTER_END)
    }

    /// The emoji's box measured in a look that shows it at its full opacity.
    fn measured(&self) -> Option<Rect> {
        // frames 32 to 83 show at full opacity
        let area = expected_area(self.tile);
        self.looks
            .iter()
            .filter(|look| read_code(&look.shot, area).is_some_and(|(_, frame)| (34..=81).contains(&frame)))
            .find_map(|look| emoji_box(&look.shot, self.tile))
    }
}

/// `act` sent and the tile watched for `length`; sent again, at most five times in all, while `enough` does not hold
/// or no try has a look after the reaction's end, `ends` after the sending (a busy machine's shots are slow). Each try
/// starts its looks 0.3 s later than the one before, so that slow shots of the tries see different frames (the fade's
/// frames show for less time than one slow shot takes).
fn watch_3d(
    rig: &mut Rig<'_>,
    (tile, length, ends): (Rect, Duration, Duration),
    case: &str,
    act: impl Fn(&mut Rig<'_>) -> SandboxResult,
    enough: impl Fn(&Watched) -> bool,
) -> SandboxResult<Watched> {
    let area = expected_area(tile);
    let mut watched = Watched {
        tile,
        looks: Vec::new(),
        seen: Vec::new(),
        plays: Vec::new(),
        earlier: Vec::new(),
    };
    for attempt in 0..5 {
        if attempt > 0 {
            thread::sleep(SETTLE);
        }
        let later = Duration::from_millis(300) * attempt;
        let (_, looks) = watch(rig, tile, length, |rig| {
            act(rig)?;
            thread::sleep(later);
            Ok(())
        })?;
        let seen = codes(&looks, area);
        say(&format!("qa696 {case}: try {attempt}: {}", listed(&seen)));
        let mut plays = std::mem::take(&mut watched.plays);
        plays.push(seen.clone());
        let mut earlier = std::mem::take(&mut watched.earlier);
        earlier.extend(watched.looks.iter().map(Glance::from));
        watched = Watched {
            tile,
            looks,
            seen,
            plays,
            earlier,
        };
        if enough(&watched) && watched.ended_seen(ends) {
            break;
        }
    }
    Ok(watched)
}

/// The looks caught the start, the middle and the end of the play.
fn start_middle_end(seen: &[Seen3d]) -> bool {
    let caught = |from: u32, to: u32| seen.iter().any(|seen| (from..=to).contains(&seen.frame));
    // the end is the fade after the last frame at full opacity (83), so a play that stops early fails
    caught(16, 40) && caught(41, 70) && caught(84, 101)
}

/// The reaction `code` played once in each play: in order, at about 24 frames a second; the plays together show
/// the start, the middle and the end (each play is the same, and a busy machine's slow shots catch a few frames of
/// each).
fn played_once(plays: &[Vec<Seen3d>], code: u32, case: &str) -> SandboxResult {
    for seen in plays {
        if let Some(other) = seen.iter().find(|seen| seen.code != code) {
            return Err(format!(
                "{case}: a look shows reaction {} in place of {code}: {other:?}",
                other.code
            )
            .into());
        }
        for pair in seen.windows(2) {
            if pair[1].frame < pair[0].frame {
                return Err(format!(
                    "{case}: frame {} came after frame {}: it plays again ({})",
                    pair[1].frame,
                    pair[0].frame,
                    listed(seen)
                )
                .into());
            }
        }
        on_time(seen, Duration::ZERO, case)?;
    }
    let all = plays.concat();
    if !start_middle_end(&all) {
        return Err(format!(
            "{case}: the looks did not see the start, middle and end frames: {}",
            listed(&all)
        )
        .into());
    }
    Ok(())
}

/// The frames of one play came at 24 a second from one start, which came at most 1 s after `sent`: each look
/// limits the start to a window (its frame ± 3, due within its shot), and the windows of all looks overlap.
fn on_time(seen: &[Seen3d], sent: Duration, case: &str) -> SandboxResult {
    let sent = sent.as_secs_f64();
    let (mut earliest, mut latest) = (sent, sent + 1.0);
    for seen in seen {
        let frame = f64::from(seen.frame);
        earliest = earliest.max(seen.at.as_secs_f64() - (frame + 3.0) / FPS);
        latest = latest.min(seen.until.as_secs_f64() - (frame - 3.0) / FPS);
    }
    if earliest > latest {
        return Err(format!(
            "{case}: the frames do not come at 24 a second from one start within 1 s of the sending: {}",
            listed(seen)
        )
        .into());
    }
    Ok(())
}

/// The tile is as before the reaction once it ended, and no code reads there.
fn gone(watched: &Watched, after: Duration, case: &str) -> SandboxResult {
    if !watched.ended_seen(after) {
        return Err(format!("{case}: no look after the reaction's end").into());
    }
    let last: Vec<Glance> = watched.looks.iter().map(Glance::from).collect();
    let late = |glance: &&Glance| glance.at > after + AFTER_END;
    let every = || last.iter().chain(&watched.earlier);
    if every().filter(late).any(|glance| glance.from_start > 40) {
        return Err(format!("{case}: the tile is not as before after the reaction ended").into());
    }
    // the sending reaches the tile within 1.5 s, so from 2.2 s until near its end the reaction is surely on the tile
    let shown_from = after.saturating_sub(ON_TILE) + Duration::from_millis(2_200);
    if let Some(early) = every().find(|glance| {
        glance.at >= shown_from && glance.until + Duration::from_millis(300) <= after && glance.from_start <= 40
    }) {
        return Err(format!("{case}: the reaction left the tile before its end: {early:?}").into());
    }
    Ok(())
}

/// The emoji measured is `side` px (± 3) and where the app places it.
fn sized(watched: &Watched, side: i32, case: &str) -> SandboxResult {
    let measured = watched
        .measured()
        .ok_or_else(|| format!("{case}: no look at full opacity shows the emoji's pure colours"))?;
    let wanted = expected_area(watched.tile);
    say(&format!(
        "qa696 {case}: tile {:?}, emoji {measured:?}, wanted {side} px at {wanted:?}",
        watched.tile
    ));
    if (measured.w - side).abs() > 3 || (measured.h - side).abs() > 3 {
        return Err(format!("{case}: the emoji is {}×{} px, not {side}", measured.w, measured.h).into());
    }
    if (measured.x - wanted.x).abs() > 4 || (measured.y - wanted.y).abs() > 4 {
        return Err(
            format!("{case}: the emoji is at {measured:?}, not centred with its top at the tile's middle").into(),
        );
    }
    Ok(())
}

/// The hue of the px in the middle of `area`, in degrees; `None` when it is grey (no colour to tell).
fn hue_at(shot: &RgbImage, area: Rect) -> Option<f64> {
    let middle = area.center();
    let [r, g, b] = pixel(shot, middle.0, middle.1)?.map(f64::from);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max - min < 30.0 {
        return None;
    }
    let delta = max - min;
    let sector = if (max - r).abs() < f64::EPSILON {
        ((g - b) / delta).rem_euclid(6.0)
    } else if (max - g).abs() < f64::EPSILON {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    Some(sector * 60.0)
}

fn hue_apart(a: f64, b: f64) -> f64 {
    let apart = (a - b).abs() % 360.0;
    apart.min(360.0 - apart)
}

/// A picture with no 3D frame on the tile: shown while it should be, playing (its hue changes, as the strip's
/// frames do) or still (one hue), and gone on time.
fn plain_picture(watched: &Watched, playing: bool, case: &str) -> SandboxResult {
    let area = expected_area(watched.tile);
    let shown: Vec<&Look> = watched
        .looks
        .iter()
        .filter(|look| look.at >= Duration::from_millis(1_000) && look.until <= Duration::from_millis(4_000))
        .collect();
    let hues: Vec<(Duration, Option<f64>, i64)> = shown
        .iter()
        .map(|look| (look.at, hue_at(&look.shot, area), look.from_start))
        .collect();
    say(&format!("qa696 {case}: hue at the emoji's middle {hues:?}"));
    if shown.is_empty() || shown.iter().any(|look| look.from_start < 400) {
        return Err(format!("{case}: the reaction is not on the tile while it should be shown").into());
    }
    let known: Vec<f64> = hues.iter().filter_map(|(_, hue, _)| *hue).collect();
    let spread = known
        .iter()
        .flat_map(|a| known.iter().map(move |b| hue_apart(*a, *b)))
        .fold(0.0, f64::max);
    if playing && (known.len() < 2 || spread < 20.0) {
        return Err(format!("{case}: the picture does not play: hues {hues:?}").into());
    }
    if !playing && spread > 15.0 {
        return Err(format!("{case}: the picture moves with Play animations off: hues {hues:?}").into());
    }
    gone(watched, ON_TILE, case)
}

/// A made-up person's reaction on their tile: it plays once at the size for the tile, from the CDN side for it.
fn one_play(rig: &mut Rig<'_>, (person, tile): (usize, Rect), reaction: &str, shot: &str, case: &str) -> SandboxResult {
    let side = emoji_side(tile.h);
    let watched = watch_3d(
        rig,
        (tile, WATCH, ON_TILE),
        case,
        |rig| react(rig, person, reaction),
        |watched| start_middle_end(&watched.plays.concat()) && watched.measured().is_some(),
    )?;
    if let Some(middle) = watched.looks.iter().find(|look| look.at > Duration::from_millis(2_000)) {
        save_shot(rig, &middle.shot, shot)?;
    }
    sized(&watched, side, case)?;
    played_once(&watched.plays, code_of(reaction), case)?;
    gone(&watched, ON_TILE, case)?;
    let file = format!("{}V4-3D/{}a.png", cdn_id(reaction), cdn_side(side));
    let reads = reads_3d(rig)?;
    if !reads.iter().any(|path| path.ends_with(&file)) {
        return Err(format!("{case}: the app did not read {file}: {reads:?}").into());
    }
    Ok(())
}

/// The first two people on a page, and their tiles.
fn first_two(page: &Page) -> SandboxResult<[(usize, Rect); 2]> {
    let people: Vec<(usize, Rect)> = page
        .tiles
        .iter()
        .filter_map(|(tile, seen)| match seen {
            Seen::Person(index) => Some((*index, *tile)),
            _ => None,
        })
        .collect();
    match people.as_slice() {
        [first, second, ..] => Ok([*first, *second]),
        _ => Err(format!("fewer than two people on the page: {:?}", page.people()).into()),
    }
}

/// Serves the made-up 3D emoji: every reaction at every side, but Surprised at 56 and 140 none (404), and Applause
/// and Surprised at 112 slow (no step before reads those two files).
fn serve(rig: &Rig<'_>) -> SandboxResult {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    for (name, id, code) in REACTIONS {
        runtime.block_on(rig.mock.serve_bytes(
            &format!("{CDN}/personal-expressions/v2/assets/emoticons/{name}/default/50_anim_f.png"),
            "image/png",
            strip_png()?,
        ));
        for side in teams_proto::EMOJI_3D_SIDES {
            if name == "surprised" && side != 112 {
                continue;
            }
            let path = format!("{CDN}/skype/v2/{id}V4-3D/{side}a.png");
            let png = sprite_png(code, u32::from(side))?;
            if SLOW_REACTIONS.contains(&name) && side == 112 {
                runtime.block_on(rig.mock.serve_bytes_after(&path, "image/png", png, SLOW));
            } else {
                runtime.block_on(rig.mock.serve_bytes(&path, "image/png", png));
            }
        }
    }
    // the strip's path names Like and Applause by their keys too
    for key in ["yes", "clap"] {
        runtime.block_on(rig.mock.serve_bytes(
            &format!("{CDN}/personal-expressions/v2/assets/emoticons/{key}/default/50_anim_f.png"),
            "image/png",
            strip_png()?,
        ));
    }
    Ok(())
}

/// The reactions the CDN gives late on the 4-gallery's tiles: each slow case's chance, in order.
const SLOW_REACTIONS: [&str; 2] = ["clap", "surprised"];

/// The step (see the module docs).
pub(super) fn qa_reaction_3d(rig: &mut Rig<'_>) -> SandboxResult {
    qa_call(rig)?;
    speak(rig, &[])?;
    serve(rig)?;
    rig.show(View::Gallery)?;
    rig.resize((1280, 800))?;
    rig.gallery_size(9, PEOPLE)?;
    thread::sleep(SETTLE);
    check_window_shots(rig)?;
    float_unchanged(rig)?;
    let page = gallery(rig)?;
    let first = first_two(&page)?[0];
    // Heart on the 56 and 140 px tiles: qa-reactions runs earlier in the same app and leaves Like's files of those
    // sides missing (404), which the app keeps for the run
    one_play(
        rig,
        first,
        "heart",
        "qa696-r1-3d-56-light",
        "a 3D emoji on a tile of the 9-gallery",
    )?;
    missing(rig, first)?;
    still(rig, first)?;

    rig.gallery_size(4, PEOPLE)?;
    thread::sleep(SETTLE);
    let page = gallery(rig)?;
    let [first, second] = first_two(&page)?;
    one_play(
        rig,
        first,
        "heart",
        "qa696-r1-3d-112-light",
        "a 3D emoji on a tile of the 4-gallery",
    )?;
    slow(rig, second)?;
    in_a_row(rig, first)?;
    together(rig, [first, second])?;
    rig.theme(teams_ui::theme::DARK_KEY)?;
    thread::sleep(SETTLE);
    let dark = one_play(rig, first, "laugh", "qa696-r1-3d-112-dark", "a 3D emoji in Teams Dark");
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    dark?;

    small(rig)?;
    speaker(rig)?;
    rig.gallery_size(49, PEOPLE)
}

/// A reaction from a person off the page floats as before: a small emoji near the stage's bottom, gone after about
/// 2 s, with no 3D frame; the app reads no 3D emoji for it. (Its rise and lanes: `qa-reactions`.)
fn float_unchanged(rig: &mut Rig<'_>) -> SandboxResult {
    let case = "a float of a person off the page";
    let page = gallery(rig)?;
    let away = (0..PEOPLE)
        .find(|index| !page.people().contains(&Seen::Person(*index)))
        .ok_or("everyone on the first page")?;
    let stage = page.stage.panel;
    let area = Rect {
        x: stage.x + 4,
        y: stage.bottom() - 260,
        w: stage.w - 8,
        h: 256,
    };
    let mut looks = Vec::new();
    for _ in 0..5 {
        (_, looks) = watch(rig, area, Duration::from_millis(4_000), |rig| react(rig, away, "heart"))?;
        if looks.iter().any(|look| look.from_start >= 60) {
            break;
        }
        thread::sleep(SETTLE);
    }
    let shown: Vec<(Duration, Option<Rect>)> = looks
        .iter()
        .filter(|look| look.from_start >= 60)
        .map(|look| (look.at, look.changed))
        .collect();
    say(&format!("qa696 {case}: {shown:?}"));
    let first = looks
        .iter()
        .find(|look| look.from_start >= 60)
        .ok_or_else(|| format!("{case}: nothing floated in five tries"))?;
    save_shot(rig, &first.shot, "qa696-r1-float")?;
    for (at, changed) in &shown {
        let Some(changed) = changed else { continue };
        if changed.w > 48 || changed.h > 48 {
            return Err(format!("{case}: at {at:?} the float is {changed:?}, larger than the 36 px float").into());
        }
        if changed.bottom() < stage.bottom() - 200 {
            return Err(format!("{case}: the float at {changed:?} is not near the stage's bottom").into());
        }
    }
    if looks
        .iter()
        .any(|look| look.at > Duration::from_millis(2_900) && look.from_start > 40)
    {
        return Err(format!("{case}: the stage is not as before after the float ended").into());
    }
    let reads = reads_3d(rig)?;
    if reads.iter().any(|path| path.contains("heartV4-3D")) {
        return Err(format!("{case}: a float read a 3D emoji: {reads:?}").into());
    }
    Ok(())
}

/// Two looks or more caught the reaction while it should be shown.
fn shown_twice(watched: &Watched) -> bool {
    watched
        .looks
        .iter()
        .filter(|look| {
            look.at >= Duration::from_millis(1_000)
                && look.until <= Duration::from_millis(4_000)
                && look.from_start >= 400
        })
        .count()
        >= 2
}

/// The CDN has no 3D emoji (404): the picker's strip plays in its place, and goes on time.
fn missing(rig: &mut Rig<'_>, (person, tile): (usize, Rect)) -> SandboxResult {
    let case = "a reaction the CDN has no 3D emoji for (404)";
    let watched = watch_3d(
        rig,
        (tile, WATCH, ON_TILE),
        case,
        |rig| react(rig, person, "surprised"),
        shown_twice,
    )?;
    if let Some(middle) = watched.looks.iter().find(|look| look.at > Duration::from_millis(2_000)) {
        save_shot(rig, &middle.shot, "qa696-r1-404-strip")?;
    }
    if !watched.seen.is_empty() {
        return Err(format!("{case}: a 3D frame shows: {}", listed(&watched.seen)).into());
    }
    for look in watched.looks.iter().filter(|look| {
        look.at >= Duration::from_millis(1_000) && look.until <= Duration::from_millis(4_000) && look.from_start >= 400
    }) {
        no_top_line(&look.shot, tile, case)?;
    }
    let reads = reads_3d(rig)?;
    if !reads.iter().any(|path| path.contains("surprisedV4-3D/56a.png")) {
        return Err(format!("{case}: the app did not ask the CDN: {reads:?}").into());
    }
    plain_picture(&watched, true, case)
}

/// "Play animations" off: the still picture shows, even with the 3D emoji read already, and goes on time.
fn still(rig: &mut Rig<'_>, (person, tile): (usize, Rect)) -> SandboxResult {
    let case = "a reaction with Play animations off";
    rig.theme(super::ANIMATIONS_OFF)?;
    let watched = watch_3d(
        rig,
        (tile, WATCH, ON_TILE),
        case,
        |rig| react(rig, person, "like"),
        shown_twice,
    );
    rig.theme(super::ANIMATIONS_ON)?;
    let watched = watched?;
    if let Some(middle) = watched.looks.iter().find(|look| look.at > Duration::from_millis(2_000)) {
        save_shot(rig, &middle.shot, "qa696-r1-still")?;
    }
    if !watched.seen.is_empty() {
        return Err(format!("{case}: a 3D frame shows: {}", listed(&watched.seen)).into());
    }
    plain_picture(&watched, false, case)
}

/// A slow CDN: the strip plays until the 3D emoji comes, then the 3D emoji at the frame due, never from its start.
/// The app keeps an emoji once read, so a second try is not slow: when too few looks land, the second try sends the
/// other reaction the CDN gives late, its looks 0.6 s later so that slow shots land at other moments.
fn slow(rig: &mut Rig<'_>, person: (usize, Rect)) -> SandboxResult {
    let case = "a 3D emoji the CDN gives 2.2 s late";
    let mut few = Vec::new();
    for (chance, reaction) in (0..).zip(SLOW_REACTIONS) {
        if chance > 0 {
            thread::sleep(SETTLE);
        }
        let later = Duration::from_millis(600) * chance;
        match slow_try(rig, person, (reaction, later), case)? {
            None => return Ok(()),
            Some(why) => {
                say(&format!("qa696 {case}: {reaction}: too few looks: {why}"));
                few.push(format!("{reaction}: {why}"));
            }
        }
    }
    Err(format!("{case}: {}", few.join("; ")).into())
}

/// Logs each look's time, its change on the tile and the code it read.
fn say_looks(looks: &[Look], area: Rect, case: &str) {
    say(&format!(
        "qa696 {case}: looks {:?}",
        looks
            .iter()
            .map(|look| format!(
                "{:.2}-{:.2}s:{}:{:?}",
                look.at.as_secs_f32(),
                look.until.as_secs_f32(),
                look.from_start,
                read_code(&look.shot, area)
            ))
            .collect::<Vec<_>>()
    ));
}

/// One try of [`slow`] with `reaction`, its looks from `later` after the sending: `Some` with the reason when too few looks landed to tell.
fn slow_try(
    rig: &mut Rig<'_>,
    (person, tile): (usize, Rect),
    (reaction, later): (&str, Duration),
    case: &str,
) -> SandboxResult<Option<String>> {
    let area = expected_area(tile);
    let (before, looks) = watch(rig, tile, Duration::from_millis(6_800), |rig| {
        react(rig, person, reaction)?;
        thread::sleep(later);
        Ok(())
    })?;
    let seen = codes(&looks, area);
    say(&format!("qa696 {case}: {}", listed(&seen)));
    let first_3d = looks
        .iter()
        .position(|look| read_code(&look.shot, area).is_some())
        .unwrap_or(looks.len());
    say_looks(&looks, area, case);
    let strip = looks[..first_3d]
        .iter()
        .find(|look| look.at > Duration::from_millis(900) && look.from_start >= 400);
    if let Some(strip) = strip {
        save_shot(rig, &strip.shot, "qa696-r1-slow-strip")?;
    }
    if let Some(late) = looks.get(first_3d) {
        save_shot(rig, &late.shot, "qa696-r1-slow-3d")?;
    }
    let Some(strip) = strip else {
        return Ok(Some(
            "no look before the 3D emoji came shows the strip on the tile".to_owned(),
        ));
    };
    no_top_line(&strip.shot, tile, case)?;
    let strip_frames: Vec<u32> = looks[..first_3d]
        .iter()
        .filter_map(|look| strip_frame_faded(&look.shot, &before, tile))
        .collect();
    say(&format!("qa696 {case}: strip frames {strip_frames:?}"));
    // 3 frames or more apart: a faint look of one frame can read a frame next to it
    let apart = |a: u32, b: u32| a.abs_diff(b).min(STRIP_FRAMES - a.abs_diff(b)) >= 3;
    if !strip_frames.iter().any(|a| strip_frames.iter().any(|b| apart(*a, *b))) {
        return Ok(Some(format!(
            "the strip does not play before the 3D emoji came: frames {strip_frames:?}"
        )));
    }
    let Some(first) = seen.first() else {
        return Ok(Some("the 3D emoji never shows after it came".to_owned()));
    };
    if first.frame < 45 {
        return Err(format!(
            "{case}: the 3D emoji started at frame {}, not at the frame due",
            first.frame
        )
        .into());
    }
    if seen.iter().any(|seen| seen.code != code_of(reaction)) {
        return Err(format!("{case}: another reaction shows: {}", listed(&seen)).into());
    }
    if seen.windows(2).any(|pair| pair[1].frame < pair[0].frame) {
        return Err(format!("{case}: the 3D emoji plays again: {}", listed(&seen)).into());
    }
    on_time(&seen, Duration::ZERO, case)?;
    let plays = vec![seen.clone()];
    let watched = Watched {
        tile,
        looks,
        seen,
        plays,
        earlier: Vec::new(),
    };
    if !watched.ended_seen(ON_TILE) {
        return Ok(Some("no look after the reaction's end".to_owned()));
    }
    gone(&watched, ON_TILE, case)?;
    Ok(None)
}

/// Heart, then Laugh 1.6 s later from the same person: Laugh takes Heart's place, plays from its own start, and goes
/// 4.25 s after it came.
fn in_a_row(rig: &mut Rig<'_>, (person, tile): (usize, Rect)) -> SandboxResult {
    let case = "two reactions in a row from one person";
    let second_after = Duration::from_millis(1_600);
    let watched = watch_3d(
        rig,
        (tile, WATCH + Duration::from_millis(1_800), second_after + ON_TILE),
        case,
        |rig| {
            react(rig, person, "heart")?;
            thread::sleep(second_after);
            react(rig, person, "laugh")
        },
        |watched| {
            watched.seen.iter().any(|seen| seen.code == code_of("heart"))
                && watched
                    .seen
                    .iter()
                    .any(|seen| seen.code == code_of("laugh") && seen.frame >= 41)
        },
    )?;
    if let Some(look) = watched
        .looks
        .iter()
        .find(|look| look.at > second_after + Duration::from_millis(1_500))
    {
        save_shot(rig, &look.shot, "qa696-r1-in-a-row")?;
    }
    let seen = &watched.seen;
    let heart: Vec<Seen3d> = seen
        .iter()
        .copied()
        .filter(|seen| seen.code == code_of("heart"))
        .collect();
    let laugh: Vec<Seen3d> = seen
        .iter()
        .copied()
        .filter(|seen| seen.code == code_of("laugh"))
        .collect();
    let (Some(last_heart), Some(first_laugh)) = (heart.last(), laugh.first()) else {
        return Err(format!("{case}: the looks did not see both reactions: {}", listed(seen)).into());
    };
    if seen
        .iter()
        .any(|seen| seen.code != code_of("heart") && seen.code != code_of("laugh"))
    {
        return Err(format!("{case}: a third reaction shows: {}", listed(seen)).into());
    }
    if heart.iter().any(|seen| seen.at > first_laugh.at) {
        return Err(format!("{case}: Heart shows again after Laugh took its place: {}", listed(seen)).into());
    }
    if first_laugh.frame >= last_heart.frame && first_laugh.at < last_heart.at + Duration::from_millis(1_000) {
        return Err(format!("{case}: Laugh did not start from its own start: {}", listed(seen)).into());
    }
    if laugh.windows(2).any(|pair| pair[1].frame < pair[0].frame) {
        return Err(format!("{case}: Laugh plays again: {}", listed(seen)).into());
    }
    on_time(&laugh, second_after, case)?;
    gone(&watched, second_after + ON_TILE, case)
}

/// Two people react at the same moment: each tile plays its own 3D emoji at the same time.
fn together(rig: &mut Rig<'_>, [(first, first_tile), (second, second_tile)]: [(usize, Rect); 2]) -> SandboxResult {
    let case = "two people's reactions at the same time";
    let union = first_tile.union(second_tile);
    let (first_area, second_area) = (expected_area(first_tile), expected_area(second_tile));
    let both = |look: &Look| {
        let a = read_code(&look.shot, first_area);
        let b = read_code(&look.shot, second_area);
        matches!((a, b), (Some((1, _)), Some((2, _))))
    };
    let mut looks = Vec::new();
    for _ in 0..3 {
        (_, looks) = watch(rig, union, Duration::from_millis(6_800), |rig| {
            react(rig, first, "like")?;
            react(rig, second, "heart")
        })?;
        if looks.iter().any(both) {
            break;
        }
        thread::sleep(SETTLE);
    }
    let shown: Vec<String> = looks
        .iter()
        .map(|look| {
            format!(
                "{:.2}s:{:?}/{:?}",
                look.at.as_secs_f32(),
                read_code(&look.shot, first_area),
                read_code(&look.shot, second_area)
            )
        })
        .collect();
    say(&format!("qa696 {case}: {shown:?}"));
    let look = looks
        .iter()
        .find(|look| both(look))
        .ok_or_else(|| format!("{case}: no look shows both 3D emoji at once: {shown:?}"))?;
    save_shot(rig, &look.shot, "qa696-r1-together")?;
    let (Some((_, a)), Some((_, b))) = (read_code(&look.shot, first_area), read_code(&look.shot, second_area)) else {
        return Err(format!("{case}: no code").into());
    };
    if a.abs_diff(b) > 6 {
        return Err(format!(
            "{case}: the two emoji are {} frames apart: they did not start together",
            a.abs_diff(b)
        )
        .into());
    }
    Ok(())
}

/// The speaker view's large tile: the 140 px emoji, from the CDN's 140 px file.
fn speaker(rig: &mut Rig<'_>) -> SandboxResult {
    rig.show(View::Speaker)?;
    speak(rig, &[0])?;
    thread::sleep(Duration::from_secs(3));
    let page = gallery(rig)?;
    save_shot(rig, &page.shot, "qa696-r1-speaker-view")?;
    // the speaker's face is the highest on the stage; the QA's tile finder knows no tile that large, so the tile is
    // the stage's width and twice the face's height over the stage's top (a tile centres its face)
    let (person, face) = page
        .tiles
        .iter()
        .filter_map(|(tile, seen)| match seen {
            Seen::Person(index) => Some((*index, tile.center())),
            _ => None,
        })
        .min_by_key(|(_, centre)| centre.1)
        .ok_or("no person in the speaker view")?;
    let panel = page.stage.panel;
    let large = (
        person,
        Rect {
            x: face.0 - panel.w / 2,
            y: panel.y,
            w: panel.w,
            h: 2 * (face.1 - panel.y),
        },
    );
    say(&format!(
        "qa696: the speaker view {:?}, its large tile {large:?}",
        page.tiles
    ));
    speak(rig, &[])?;
    let outcome = if large.1.h >= 486 {
        one_play(
            rig,
            large,
            "heart",
            "qa696-r1-3d-140-light",
            "a 3D emoji on the speaker view's large tile",
        )
    } else {
        Err(format!(
            "the speaker view's large tile {:?} is too small for the 140 px emoji",
            large.1
        )
        .into())
    };
    rig.show(View::Gallery)?;
    outcome
}

/// A small window's small tile: an emoji smaller than 56 px, from the CDN's 56 px file.
fn small(rig: &mut Rig<'_>) -> SandboxResult {
    rig.resize((560, 400))?;
    rig.gallery_size(9, PEOPLE)?;
    thread::sleep(SETTLE);
    let page = gallery(rig)?;
    let first = first_two(&page)?[0];
    say(&format!("qa696: a small tile {:?}", first.1));
    let outcome = if emoji_side(first.1.h) < 56 {
        one_play(rig, first, "heart", "qa696-r1-3d-small", "a 3D emoji on a small tile")
    } else {
        Err(format!("the tile {:?} is not small", first.1).into())
    };
    rig.resize((1280, 800))?;
    outcome
}
