//! QA of task 666 (round 1): live reactions in a group call, as a user sees them in the app's own call window on the
//! mock. The QA's made-up people send reactions as their `sendMessage` does ([`ChatServiceMock::react_made_up`]); the
//! account sends its own from React's menu. The mock serves a made-up animation strip on the path of Teams' CDN
//! (48 frames of 50×50, each a disk of another colour), so a reaction that plays changes its picture frame by frame.
//!
//! - On the sender's tile while it is on the stage (the account's own too): the reaction shows, plays (its picture
//!   changes while it is fully shown), dims the face under it, and is gone after about 4.25 s, in the gallery, the
//!   speaker view and the strip beside a share.
//! - Floating while the sender's tile is not on the stage (another page, Focus on content): it rises from the stage's
//!   bottom middle and is gone after about 2 s; three at once float side by side in lanes about 200 px apart.
//! - "Play animations" off: the reaction stands still, and still goes on time.
//! - With no reaction on the screen the window stays still (no frame changes), and the app's CPU time is no higher
//!   than before the reaction.
//! - Shots in Teams Dark and in a small window.
//!
//! [`ChatServiceMock::react_made_up`]: teams_mock::ChatServiceMock::react_made_up

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use image::RgbImage;
// how long a reaction shows on a tile, as the app has it
use teams_ui::calls::REACTION_SHOWN as ON_TILE;
use ui_sandbox::SandboxResult;

use super::pixels::{self, Rect};
use super::qa_order::{PEOPLE, Page, Seen, colour_count, gallery, qa_call, qa_mri, speak, strip};
use super::{Rig, SETTLE, View, save_shot, say};

/// How long a reaction floats.
const FLOAT: Duration = Duration::from_millis(2_000);
/// How many frames the made-up strip has: at the app's 24 a second it loops every 2 s, so two looks less than 2 s
/// apart never meet the same frame.
const FRAMES: u32 = 48;

/// Frame `index`'s colour: a full hue round over the frames, none white or grey (a float shows over the white own
/// tile too).
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
        // 0 to 230
        (0_u8..=230)
            .find(|step| f64::from(*step) >= level * 230.0)
            .unwrap_or(230)
    };
    [channel(4.0), channel(2.0), channel(0.0)]
}

/// The reactions' ids on Teams' CDN, as the call's five reactions name them.
const IDS: [&str; 5] = ["like", "heart", "clap", "laugh", "surprised"];
const CDN_PATH: &str = "/evergreen-assets/personal-expressions/v2/assets/emoticons";

/// The made-up animation strip: its [`FRAMES`] stacked top to bottom, each a disk of [`frame_colour`] on a clear
/// ground, 50×50.
pub(super) fn strip_png() -> SandboxResult<Vec<u8>> {
    let side = 50_u32;
    let mut picture = image::RgbaImage::new(side, side * FRAMES);
    for index in 0..FRAMES {
        let [r, g, b] = frame_colour(index);
        let top = index * side;
        for y in 0..side {
            for x in 0..side {
                let (dx, dy) = (f64::from(x) - 24.5, f64::from(y) - 24.5);
                if dx * dx + dy * dy <= 23.0 * 23.0 {
                    picture.put_pixel(x, top + y, image::Rgba([r, g, b, 255]));
                }
            }
        }
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// One look at the watched area: when, how much differs from the shot before the reaction, and from the look before.
#[derive(Clone, Debug)]
pub(super) struct Look {
    /// When its shot started and ended, from the reaction's sending.
    pub(super) at: Duration,
    pub(super) until: Duration,
    pub(super) from_start: i64,
    from_last: i64,
    /// Where it differs from the shot before the reaction.
    pub(super) changed: Option<Rect>,
    pub(super) shot: RgbImage,
}

/// Shots of `area` for `time` after `act` (the shot before it is the start): each look.
pub(super) fn watch(
    rig: &mut Rig<'_>,
    area: Rect,
    time: Duration,
    act: impl FnOnce(&mut Rig<'_>) -> SandboxResult,
) -> SandboxResult<(RgbImage, Vec<Look>)> {
    rig.park()?;
    let before = quick_shot(rig)?;
    let start = Instant::now();
    act(rig)?;
    let mut looks = Vec::new();
    let mut last = before.clone();
    while start.elapsed() < time {
        let at = start.elapsed();
        let shot = quick_shot(rig)?;
        let changes = pixels::changes(&before, &shot, area);
        looks.push(Look {
            at,
            until: start.elapsed(),
            from_start: changes.iter().map(|blob| blob.pixels).sum(),
            from_last: pixels::changed_pixels(&last, &shot, area),
            changed: changes
                .iter()
                .filter(|blob| blob.pixels >= 20)
                .map(|blob| blob.bounds)
                .reduce(Rect::union),
            shot: shot.clone(),
        });
        last = shot;
    }
    Ok((before, looks))
}

/// Whether [`quick_shot`] shows what a shot of the whole screen shows (checked once, at the step's start).
static WINDOW_SHOTS: AtomicBool = AtomicBool::new(false);

/// The screen as a shot of the focused call window alone shows it, the rest black: half the px of a full HD screen to
/// write and read, so a busy machine still takes a few shots a second. A shot of the whole screen while
/// [`WINDOW_SHOTS`] does not hold.
fn quick_shot(rig: &mut Rig<'_>) -> SandboxResult<RgbImage> {
    if !WINDOW_SHOTS.load(Ordering::Relaxed) {
        return rig.shot();
    }
    let window = rig.window()?;
    let png = rig.sandbox.screenshot("qa666-window.png", true)?;
    let part = image::open(&png)?.to_rgb8();
    let _ = std::fs::remove_file(png);
    let mut whole = RgbImage::new(super::SCREEN.0, super::SCREEN.1);
    image::imageops::replace(&mut whole, &part, i64::from(window.x), i64::from(window.y));
    Ok(whole)
}

/// Turns [`quick_shot`]'s window shots on when one matches a shot of the whole screen within the call window.
pub(super) fn check_window_shots(rig: &mut Rig<'_>) -> SandboxResult {
    rig.park()?;
    let window = rig.window()?;
    let full = rig.shot()?;
    WINDOW_SHOTS.store(true, Ordering::Relaxed);
    let quick = quick_shot(rig)?;
    let changed = pixels::changed_pixels(&full, &quick, window);
    let same = changed < 400;
    WINDOW_SHOTS.store(same, Ordering::Relaxed);
    say(&format!(
        "qa666: a shot of the call window alone differs from the screen's in {changed} px: window shots {same}"
    ));
    Ok(())
}

fn summary(looks: &[Look]) -> String {
    looks
        .iter()
        .map(|look| format!("{:.1}s:{}/{}", look.at.as_secs_f32(), look.from_start, look.from_last))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A made-up person's reaction `reaction` on their tile `tile`, watched for 6.8 s; sent again, at most three times
/// in all, while fewer than three looks catch it or no two looks one after the other catch it at its full opacity (a
/// busy machine's shots are slow).
pub(super) fn watch_tile(rig: &mut Rig<'_>, tile: Rect, person: usize, reaction: &str) -> SandboxResult<Vec<Look>> {
    let mut looks = Vec::new();
    for _ in 0..3 {
        (_, looks) = watch(rig, tile, Duration::from_millis(6_800), |rig| {
            react(rig, person, reaction)
        })?;
        let caught = looks.iter().filter(|look| look.from_start >= 400).count();
        let steady = steady_pairs(&looks).count();
        if caught >= 3 && steady >= 1 {
            break;
        }
        say(&format!(
            "qa666: {caught} looks caught {person}'s reaction on the tile, {steady} pairs at its full opacity: sent \
             again"
        ));
    }
    Ok(looks)
}

/// What a reaction on a tile did: shown, played while fully shown, and gone on time.
pub(super) fn on_tile(looks: &[Look], playing: bool, case: &str) -> SandboxResult {
    say(&format!("qa666 {case}: {}", summary(looks)));
    let shown = looks
        .iter()
        .filter(|look| look.at > Duration::from_millis(1_400) && look.at < Duration::from_millis(3_400))
        .collect::<Vec<_>>();
    if shown.is_empty() || shown.iter().any(|look| look.from_start < 400) {
        return Err(format!("{case}: the reaction isn't on the tile while it should be fully shown").into());
    }
    // a change between two looks that showed the reaction at its full opacity: its picture moved, as no fade
    // changes it there (the strip of 48 frames loops every 2 s, so two looks less than 2 s apart show different
    // frames)
    let moving = steady_pairs(looks).filter(|pair| pair[1].from_last >= 150).count();
    if playing && moving < 1 {
        return Err(format!("{case}: the reaction's picture doesn't play ({moving} looks changed)").into());
    }
    if !playing && moving > 0 {
        return Err(format!("{case}: the reaction moves with Play animations off ({moving} looks changed)").into());
    }
    // it ended after the start of the last look that showed it and before the end of the next look
    let (last_on, first_off) = end_of(looks).ok_or_else(|| format!("{case}: no look after the reaction's end"))?;
    if last_on > ON_TILE + Duration::from_millis(700) || first_off < ON_TILE.saturating_sub(Duration::from_millis(900))
    {
        return Err(format!(
            "{case}: the reaction ended between {last_on:?} and {first_off:?} after it came: Teams shows it {ON_TILE:?}"
        )
        .into());
    }
    let after = looks
        .iter()
        .filter(|look| look.at > ON_TILE + Duration::from_millis(800));
    if after.clone().any(|look| look.from_start > 40) {
        return Err(format!("{case}: the tile is not as before the reaction after it ended").into());
    }
    if after.count() == 0 {
        return Err(format!("{case}: no look after the reaction's end").into());
    }
    Ok(())
}

/// The pairs of looks one after the other that both showed a reaction on a tile at its full opacity, not fading in
/// or out: the app's fade-in ends at about 1.34 s and its fade-out starts at about 3.46 s. The first look starts at
/// 1.4 s or later, and the shot of the second look ends at 3.4 s or earlier (a busy machine's shot can take long).
fn steady_pairs(looks: &[Look]) -> impl Iterator<Item = &[Look]> {
    looks.windows(2).filter(|pair| {
        pair[0].at >= Duration::from_millis(1_400)
            && pair[1].until <= Duration::from_millis(3_400)
            && pair[0].from_start >= 400
            && pair[1].from_start >= 400
    })
}

/// When what a reaction showed ended: after the start of the last look that showed it, before the end of the first
/// look after that showed nothing; `None` when no look came after it.
fn end_of(looks: &[Look]) -> Option<(Duration, Duration)> {
    let last = looks.iter().rposition(|look| look.from_start > 40)?;
    let next = looks.get(last + 1)?;
    Some((looks[last].at, next.until))
}

/// What a floating reaction did: shown near the stage's bottom middle, rising (when `playing`), gone after about 2 s.
fn floated(looks: &[Look], stage: Rect, playing: bool, case: &str) -> SandboxResult {
    say(&format!(
        "qa666 {case}: {}; where {:?}",
        summary(looks),
        looks
            .iter()
            .map(|look| look.changed.map(|at| (at.x, at.y)))
            .collect::<Vec<_>>()
    ));
    let seen: Vec<(Duration, Rect)> = looks
        .iter()
        .filter_map(|look| look.changed.filter(|_| look.from_start >= 60).map(|at| (look.at, at)))
        .collect();
    let (first, last) = (
        seen.first().ok_or_else(|| format!("{case}: nothing floated"))?,
        seen.last().ok_or("")?,
    );
    let middle = stage.x + stage.w / 2;
    if (first.1.center().0 - middle).abs() > 330 {
        return Err(format!(
            "{case}: the float at {:?} is not about the stage's middle {middle}",
            first.1
        )
        .into());
    }
    if first.1.bottom() < stage.bottom() - 200 {
        return Err(format!(
            "{case}: the float at {:?} is not near the stage's bottom {}",
            first.1,
            stage.bottom()
        )
        .into());
    }
    if playing && last.1.y >= first.1.y - 10 {
        return Err(format!("{case}: the float didn't rise: {:?} then {:?}", first.1, last.1).into());
    }
    if !playing && (last.1.y - first.1.y).abs() > 4 {
        return Err(format!(
            "{case}: the float moved with Play animations off: {:?} then {:?}",
            first.1, last.1
        )
        .into());
    }
    if let Some((last_on, _)) = end_of(looks)
        && last_on > FLOAT + Duration::from_millis(700)
    {
        return Err(format!("{case}: the float was still seen {last_on:?} after it came").into());
    }
    if looks
        .iter()
        .any(|look| look.at > FLOAT + Duration::from_millis(900) && look.from_start > 40)
    {
        return Err(format!("{case}: the stage is not as before after the float ended").into());
    }
    Ok(())
}

/// A float to check: who sends it, over which stage, and whether it moves ("Play animations").
struct Float {
    stage: Rect,
    person: usize,
    playing: bool,
    /// Over a share that moves under it: only the changes to a colour count (see [`coloured_changes`]).
    over_share: bool,
}

impl Float {
    /// `reaction` sent and watched as it floats (see [`floated`]); sent again, at most five times in all, while
    /// fewer than two looks catch it (a busy machine's shots are slow).
    fn check(&self, rig: &mut Rig<'_>, reaction: &str, shot: &str, case: &str) -> SandboxResult {
        for _ in 0..5 {
            let area = float_area(self.stage);
            let (before, mut looks) = watch(rig, area, Duration::from_millis(4_000), |rig| {
                react(rig, self.person, reaction)
            })?;
            if self.over_share {
                coloured_changes(&before, &mut looks, area);
            }
            if let Some(look) = looks.iter().find(|look| look.at > Duration::from_millis(700)) {
                save_shot(rig, &look.shot, shot)?;
            }
            let caught = looks.iter().filter(|look| look.from_start >= 60).count();
            if caught >= 2 {
                return floated(&looks, self.stage, self.playing, case);
            }
            say(&format!(
                "qa666 {case}: only {caught} look caught the float: sent again"
            ));
        }
        Err(format!("{case}: no two looks caught the float in five tries").into())
    }
}

/// The pixel of `shot` at `x`, `y` (`None` out of the shot), and whether it is a colour: its channels differ by more
/// than 80 (black, white and grey never do).
fn colourful(shot: &RgbImage, x: i32, y: i32) -> Option<([u8; 3], bool)> {
    let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
    (x < shot.width() && y < shot.height()).then(|| {
        let p = shot.get_pixel(x, y).0;
        (p, p.iter().max().unwrap_or(&0) - p.iter().min().unwrap_or(&0) > 80)
    })
}

/// Counts in `looks` only the changes from `before` to a colour, as the float's frames are and the mock's share
/// (black, white and grey) never is: its moves under the float don't count.
fn coloured_changes(before: &RgbImage, looks: &mut [Look], area: Rect) {
    let mut last = before.clone();
    for look in looks.iter_mut() {
        let changes = |from: &RgbImage| {
            pixels::blobs(area, |x, y| {
                match (colourful(from, x, y), colourful(&look.shot, x, y)) {
                    (Some((a, _)), Some((b, true))) => {
                        a.iter().zip(b).map(|(a, b)| a.abs_diff(b)).max().unwrap_or(0) > 48
                    }
                    _ => false,
                }
            })
        };
        let from_start = changes(before);
        look.from_last = changes(&last).iter().map(|blob| blob.pixels).sum();
        look.from_start = from_start.iter().map(|blob| blob.pixels).sum();
        look.changed = from_start
            .iter()
            .filter(|blob| blob.pixels >= 20)
            .map(|blob| blob.bounds)
            .reduce(Rect::union);
        last.clone_from(&look.shot);
    }
}

/// The bottom part of the stage where floats rise.
fn float_area(stage: Rect) -> Rect {
    Rect {
        x: stage.x + 4,
        y: stage.bottom() - 260,
        w: stage.w - 8,
        h: 256,
    }
}

pub(super) fn react(rig: &Rig<'_>, person: usize, reaction: &str) -> SandboxResult {
    if rig.mock.react_made_up(&rig.call, &qa_mri(person), reaction) {
        Ok(())
    } else {
        Err(format!("the mock's call has no {person} to react").into())
    }
}

/// The step (see the module docs).
pub(super) fn qa_reactions(rig: &mut Rig<'_>) -> SandboxResult {
    qa_call(rig)?;
    speak(rig, &[])?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let png = strip_png()?;
    for id in IDS {
        runtime.block_on(rig.mock.serve_bytes(
            &format!("{CDN_PATH}/{id}/default/50_anim_f.png"),
            "image/png",
            png.clone(),
        ));
    }
    rig.show(View::Gallery)?;
    rig.resize((1280, 800))?;
    rig.gallery_size(9, PEOPLE)?;
    thread::sleep(SETTLE);
    check_window_shots(rig)?;
    let idle = cpu_idle(rig, "before any reaction")?;
    gallery_reactions(rig)?;
    let after = cpu_idle(rig, "after the reactions")?;
    say(&format!(
        "qa666: the app's CPU with no reaction: {idle:.1} % before, {after:.1} % after"
    ));
    if after > idle * 1.5 + 3.0 {
        return Err(
            format!("the app's CPU stays up after the reactions: {idle:.1} % before, {after:.1} % after").into(),
        );
    }
    own_reaction(rig)?;
    speaker_reactions(rig)?;
    share_reactions(rig)?;
    still_reactions(rig)?;
    looks(rig)?;
    rig.gallery_size(49, PEOPLE)
}

/// The app's CPU share over 8 s with nothing moving on the stage, in %; the stage must not change meanwhile.
fn cpu_idle(rig: &mut Rig<'_>, case: &str) -> SandboxResult<f64> {
    rig.park()?;
    thread::sleep(SETTLE);
    let stage = rig.stage()?;
    let before = rig.shot()?;
    let pid = rig.app.running.pid();
    let (ticks, start) = (cpu_ticks(pid)?, Instant::now());
    thread::sleep(Duration::from_secs(8));
    let used = cpu_ticks(pid)? - ticks;
    let share = f64::from(u32::try_from(used).unwrap_or(u32::MAX)) / start.elapsed().as_secs_f64();
    let after = rig.shot()?;
    let changed = pixels::changed_pixels(&before, &after, stage.panel);
    say(&format!(
        "qa666 CPU {case}: {share:.1} % over 8 s; {changed} px of the stage changed"
    ));
    if changed > 40 {
        rig.save("failed-qa666-idle-stage")?;
        return Err(format!("{case}: the stage changed with nothing on it ({changed} px)").into());
    }
    Ok(share)
}

/// The process's user and system time so far, in clock ticks (100 a second).
fn cpu_ticks(pid: u32) -> SandboxResult<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest.split_whitespace().collect())
        .unwrap_or_default();
    // after the name: state is field 3, utime 14, stime 15
    let utime: u64 = fields.get(11).ok_or("no utime")?.parse()?;
    let stime: u64 = fields.get(12).ok_or("no stime")?.parse()?;
    Ok(utime + stime)
}

/// The gallery: a reaction on a tile on the page, a float of one on page 2, three floats at once.
fn gallery_reactions(rig: &mut Rig<'_>) -> SandboxResult {
    let page = gallery(rig)?;
    let on_page = page.people();
    say(&format!("qa666 reactions: the gallery's first page {on_page:?}"));
    let (person, tile) = first_person(&page)?;
    let face_before = face_count(&page.shot, tile, person);
    let looks = watch_tile(rig, tile, person, "like")?;
    let middle = looks
        .iter()
        .find(|look| look.at > Duration::from_millis(2_000))
        .ok_or("no look in the middle of the reaction")?;
    rig.save("qa666-reaction-gallery-tile")?;
    save_shot(rig, &middle.shot, "qa666-reaction-gallery-tile-playing")?;
    let face_during = face_count(&middle.shot, tile, person);
    say(&format!(
        "qa666: the face under the reaction: {face_before} px of its colour before, {face_during} during"
    ));
    on_tile(&looks, true, "a reaction on a gallery tile")?;
    if face_during * 2 > face_before {
        return Err(format!(
            "the face under the reaction isn't dimmed: {face_before} px of its colour before, {face_during} during"
        )
        .into());
    }
    let away = (0..PEOPLE)
        .filter(|index| !on_page.contains(&Seen::Person(*index)))
        .collect::<Vec<_>>();
    let float = Float {
        stage: page.stage.panel,
        person: away[0],
        playing: true,
        over_share: false,
    };
    float.check(
        rig,
        "heart",
        "qa666-reaction-gallery-float",
        "a reaction from page 2 in the gallery",
    )?;
    several(rig, page.stage.panel, &away)
}

/// Three people off the page react at once: three floats side by side, about 200 px apart.
fn several(rig: &mut Rig<'_>, stage: Rect, away: &[usize]) -> SandboxResult {
    let area = float_area(stage);
    let (before, looks) = watch(rig, area, Duration::from_millis(4_000), |rig| {
        react(rig, away[0], "laugh")?;
        react(rig, away[1], "clap")?;
        react(rig, away[2], "surprised")
    })?;
    let look = looks
        .iter()
        .find(|look| look.at > Duration::from_millis(800))
        .ok_or("no look of the three floats")?;
    save_shot(rig, &look.shot, "qa666-reaction-three-floats")?;
    let mut floats: Vec<Rect> = pixels::changes(&before, &look.shot, area)
        .into_iter()
        .filter(|blob| blob.bounds.w >= 16 && blob.bounds.h >= 16)
        .map(|blob| blob.bounds)
        .collect();
    floats.sort_by_key(|float| float.x);
    let centres: Vec<i32> = floats.iter().map(|float| float.center().0).collect();
    say(&format!("qa666: three floats at {floats:?}"));
    if floats.len() != 3 {
        return Err(format!("three reactions at once: {} floats show: {floats:?}", floats.len()).into());
    }
    for pair in centres.windows(2) {
        let gap = pair[1] - pair[0];
        let lanes = (f64::from(gap) / 200.0).round();
        if lanes < 1.0 || (f64::from(gap) - lanes * 200.0).abs() > 30.0 {
            return Err(format!("the floats are not in lanes 200 px apart: {centres:?}").into());
        }
    }
    if looks
        .iter()
        .any(|look| look.at > FLOAT + Duration::from_millis(900) && look.from_start > 40)
    {
        return Err("the three floats did not all end on time".into());
    }
    Ok(())
}

/// The first of the QA's people on a page, and their tile.
pub(super) fn first_person(page: &Page) -> SandboxResult<(usize, Rect)> {
    page.tiles
        .iter()
        .find_map(|(tile, seen)| match seen {
            Seen::Person(index) => Some((*index, *tile)),
            _ => None,
        })
        .ok_or_else(|| "no known face on the page".into())
}

/// How many px of the middle of `tile` have `person`'s picture colour (the face, centred on the tile).
fn face_count(shot: &RgbImage, tile: Rect, person: usize) -> usize {
    let middle = Rect {
        x: tile.x + tile.w / 4,
        y: tile.y + tile.h / 8,
        w: tile.w / 2,
        h: tile.h * 3 / 4,
    };
    colour_count(shot, middle, person)
}

/// The account's own reaction from React's menu: on its own tile.
fn own_reaction(rig: &mut Rig<'_>) -> SandboxResult {
    let bar = rig.toolbar("qa-reactions")?;
    let page = gallery(rig)?;
    let own = page.stage.own.ok_or("no own tile")?;
    rig.park()?;
    let closed = rig.shot()?;
    rig.click(bar.react)?;
    let open = rig.shot()?;
    let window = rig.window()?;
    let below = Rect {
        y: window.y + super::TOOLBAR_BAND,
        h: 140,
        ..window
    };
    let menu = pixels::changes(&closed, &open, below)
        .into_iter()
        .map(|blob| blob.bounds)
        .reduce(Rect::union)
        .ok_or("React's click opened no menu")?;
    save_shot(rig, &open, "qa666-react-menu")?;
    // its first picture (Like): the leftmost colourful blob in the menu
    let mut pictures: Vec<Rect> = pixels::blobs(menu, |x, y| colourful(&open, x, y).is_some_and(|(_, colour)| colour))
        .into_iter()
        .map(|blob| blob.bounds)
        .filter(|bounds| bounds.w >= 8 && bounds.h >= 8)
        .collect();
    pictures.sort_by_key(|picture| picture.x);
    let like = *pictures
        .first()
        .ok_or_else(|| format!("no pictures in React's menu at {menu:?}"))?;
    say(&format!("qa666: React's menu at {menu:?}, its pictures {pictures:?}"));
    // sent again, at most three times in all, while no two looks one after the other catch it at its full opacity
    let mut looks = Vec::new();
    for attempt in 0..3 {
        if attempt > 0 {
            rig.click(bar.react)?;
            thread::sleep(SETTLE);
        }
        let sent = rig.sent("sendMessage") + 1;
        (_, looks) = watch(rig, own, Duration::from_millis(6_800), |rig| {
            rig.click(like)?;
            rig.key("escape")
        })?;
        rig.wait_sent("sendMessage", sent, "React's Like")?;
        if steady_pairs(&looks).count() >= 1 {
            break;
        }
        say("qa666: no two looks caught the own reaction at its full opacity: sent again");
    }
    if let Some(look) = looks.iter().find(|look| look.at > Duration::from_millis(2_200)) {
        save_shot(rig, &look.shot, "qa666-reaction-own-tile")?;
    }
    on_tile(&looks, true, "the account's own reaction on its tile")
}

/// The speaker view: a strip tile's reaction on it, someone off the strip's page floats.
fn speaker_reactions(rig: &mut Rig<'_>) -> SandboxResult {
    rig.show(View::Speaker)?;
    thread::sleep(Duration::from_secs(3));
    let page = gallery(rig)?;
    say(&format!("qa666 reactions: the speaker view {:?}", page.people()));
    // a strip tile: the smallest tiles
    let strip_tile = page
        .tiles
        .iter()
        .filter(|(tile, seen)| matches!(seen, Seen::Person(_)) && tile.h < page.stage.panel.h / 3)
        .map(|(tile, seen)| (*tile, *seen))
        .next()
        .ok_or("no person in the speaker view's strip")?;
    let Seen::Person(person) = strip_tile.1 else {
        return Err("no person".into());
    };
    let looks = watch_tile(rig, strip_tile.0, person, "like")?;
    if let Some(look) = looks.iter().find(|look| look.at > Duration::from_millis(2_200)) {
        save_shot(rig, &look.shot, "qa666-reaction-speaker-strip-tile")?;
    }
    on_tile(&looks, true, "a reaction on a speaker strip tile")?;
    let shown: Vec<Seen> = page.people();
    let away = (0..PEOPLE)
        .rev()
        .find(|index| !shown.contains(&Seen::Person(*index)))
        .ok_or("everyone shows in the speaker view")?;
    let float = Float {
        stage: page.stage.panel,
        person: away,
        playing: true,
        over_share: false,
    };
    float.check(
        rig,
        "heart",
        "qa666-reaction-speaker-float",
        "a reaction off the speaker view's strip",
    )?;
    rig.show(View::Gallery)
}

/// A share: a strip tile's reaction on it, someone off the strip's page floats; Focus on content: every one floats.
fn share_reactions(rig: &mut Rig<'_>) -> SandboxResult {
    let presenter = 3;
    if !rig.mock.share_made_up(&rig.call, &qa_mri(presenter), true) {
        return Err("the mock's share didn't go".into());
    }
    rig.sharing = true;
    rig.wait_share()?;
    thread::sleep(Duration::from_secs(3));
    let page = strip(rig)?;
    say(&format!("qa666 reactions: the share's strip {:?}", page.people()));
    let (person, tile) = first_person(&page)?;
    // the share plays on under the strip: only the tile is watched
    let looks = watch_tile(rig, tile, person, "like")?;
    if let Some(look) = looks.iter().find(|look| look.at > Duration::from_millis(2_200)) {
        save_shot(rig, &look.shot, "qa666-reaction-share-strip-tile")?;
    }
    on_tile(&looks, true, "a reaction on a share strip tile")?;
    let shown = page.people();
    let away = (0..PEOPLE)
        .rev()
        .find(|index| !shown.contains(&Seen::Person(*index)))
        .ok_or("everyone shows in the strip")?;
    // the float rises over the share, which moves under it: only the float's colours count
    let float = Float {
        stage: page.stage.panel,
        person: away,
        playing: true,
        over_share: true,
    };
    float.check(rig, "heart", "qa666-reaction-share-float", "a float beside a share")?;
    focus(rig, true)?;
    thread::sleep(SETTLE);
    let float = Float { person, ..float };
    float.check(
        rig,
        "laugh",
        "qa666-reaction-focus-float",
        "a float over Focus on content",
    )?;
    focus(rig, false)?;
    if !rig.mock.share_made_up(&rig.call, &qa_mri(presenter), false) {
        return Err("the mock's share didn't stop".into());
    }
    rig.sharing = false;
    thread::sleep(SETTLE * 2);
    rig.show(View::Gallery)
}

/// View → Focus on content (`on`), or back to Gallery.
fn focus(rig: &mut Rig<'_>, on: bool) -> SandboxResult {
    // under Focus on content the share has no strip beside it, which the menu's check of a share looks for: the menu
    // is checked over the whole stage then
    let sharing = rig.sharing;
    rig.sharing = on;
    let opened = rig.open_view_menu();
    rig.sharing = sharing;
    opened?;
    let downs = if on { 3 } else { 1 };
    for _ in 0..downs {
        rig.key("down")?;
    }
    rig.key("enter")?;
    let wanted = if on { "focus" } else { "gallery" };
    rig.app.trace.wait_until("the chosen call layout", |seen| {
        seen.iter()
            .rev()
            .find(|line| line["view"] == "call-window" && line["change"] == "layout")
            .is_some_and(|line| line["kinds"][0] == wanted)
    })?;
    thread::sleep(SETTLE);
    Ok(())
}

/// "Play animations" off: a reaction on a tile stands still and goes on time; a float stands still and goes.
fn still_reactions(rig: &mut Rig<'_>) -> SandboxResult {
    rig.theme(super::ANIMATIONS_OFF)?;
    let page = gallery(rig)?;
    let (person, tile) = first_person(&page)?;
    let looks = watch_tile(rig, tile, person, "heart")?;
    if let Some(look) = looks.iter().find(|look| look.at > Duration::from_millis(2_200)) {
        save_shot(rig, &look.shot, "qa666-reaction-tile-animations-off")?;
    }
    let outcome = on_tile(&looks, false, "a reaction on a tile with Play animations off");
    let away = (0..PEOPLE)
        .find(|index| !page.people().contains(&Seen::Person(*index)))
        .ok_or("everyone on the first page")?;
    let float = Float {
        stage: page.stage.panel,
        person: away,
        playing: false,
        over_share: false,
    };
    let floated = float.check(
        rig,
        "like",
        "qa666-reaction-float-animations-off",
        "a float with Play animations off",
    );
    rig.theme(super::ANIMATIONS_ON)?;
    outcome?;
    floated
}

/// Shots of a reaction on a tile and a float in Teams Dark and in a small window.
fn looks(rig: &mut Rig<'_>) -> SandboxResult {
    for (name, theme, size) in [
        ("dark", teams_ui::theme::DARK_KEY, (1280, 800)),
        ("560x400", teams_ui::theme::LIGHT_KEY, (560, 400)),
    ] {
        rig.theme(theme)?;
        rig.resize(size)?;
        rig.park()?;
        thread::sleep(SETTLE);
        react(rig, 0, "like")?;
        react(rig, PEOPLE - 1, "heart")?;
        thread::sleep(Duration::from_millis(1_000));
        rig.save(&format!("qa666-reactions-{name}"))?;
        thread::sleep(ON_TILE);
    }
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    rig.resize((1280, 800))
}
