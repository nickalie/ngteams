//! QA of task 666 (round 1): the order of the tiles beside a share, in the gallery and in the speaker view, as a user
//! sees it in the app's own call window on the mock. Thirteen made-up people join a group call, each with a picture
//! of its own colour (served on Graph's picture path), so a tile's person is found by the colour of its face. The
//! mock's mixer names who speaks ([`ChatServiceMock::speak`]): a made-up person's roster entry lists a made-up audio
//! source for it.
//!
//! - A far-end share: the presenter's tile is the strip's first, the account's own tile its last.
//! - Someone on a later page speaks: after about a second their tile is on the first page, behind the presenter's;
//!   before that nothing moves. A short sound (half a second) moves nothing. A second speaker right after the first
//!   moves only when the order held still for three seconds.
//! - Every strip page ends with the account's own tile, everyone once across the pages.
//! - The page the user turned to stays: a person on it who speaks keeps their place; a person out of sight who speaks
//!   moves to the first page while the page shown stays the same page.
//! - After the share: the gallery and the speaker view keep the same order of speakers.
//! - Shots of the strip in Teams Dark, at a larger font and in the smallest call window.
//!
//! [`ChatServiceMock::speak`]: teams_mock::ChatServiceMock::speak

use std::thread;
use std::time::{Duration, Instant};

use image::{Rgb, RgbImage};
use teams_mock::LAB_A;
use ui_sandbox::SandboxResult;

use super::pixels::{self, Rect, Stage};
use super::{Rig, SETTLE, View, grid_size, say};

/// How many made-up people the QA's call has besides the account (and b, whom the mock plays: it has no tile).
pub(super) const PEOPLE: usize = 13;
/// Each made-up person's picture colour, by their index: far apart, and apart from Teams' brand, gold and red.
const COLOURS: [[u8; 3]; PEOPLE] = [
    [0, 150, 0],
    [0, 90, 255],
    [255, 120, 0],
    [0, 200, 200],
    [200, 0, 200],
    [120, 60, 0],
    [0, 0, 120],
    [150, 220, 0],
    [255, 0, 120],
    [0, 90, 60],
    [255, 230, 0],
    [130, 0, 40],
    [0, 255, 120],
];
/// A pixel is a face's colour within this distance (the sum of the channels' differences).
const NEAR: i32 = 40;
/// A tile shows a face when this many of its sampled pixels have its colour.
const FACE: usize = 30;
/// The order may change this soon after a speaker starts, at the earliest: the app's settle time, less the time
/// a shot takes.
const NOT_BEFORE: Duration = Duration::from_millis(850);
/// A move shows this late after it is due, at the latest (a busy machine, the shot's own time).
const LATE: Duration = Duration::from_millis(1_800);
/// The hold ends this soon after its first speaker's start, at the earliest (the settle time and the hold), less a
/// margin: a read that ends later can show the second speaker moved after a hold that held.
const HOLD_ENDS: Duration = Duration::from_millis(3_900);
/// The rounds of the hold the QA tries when each read ends too late to prove it.
const HOLD_ROUNDS: usize = 3;
/// The wait after a move until the order may change again: the hold and a margin.
const HOLD_GAP: Duration = Duration::from_millis(3_500);

/// The QA's made-up person `index`: another set than the other steps', so its pictures are read fresh.
pub(super) fn qa_mri(index: usize) -> String {
    format!("8:orgid:00000000-0000-4000-8000-00000000c{index:03}")
}

fn qa_name(index: usize) -> String {
    const FIRST: [&str; PEOPLE] = [
        "Ada", "Bo", "Cyd", "Dina", "Ezra", "Flo", "Gil", "Ike", "Jo", "Kai", "Lu", "Mo", "Ned",
    ];
    format!("{} Quinn", FIRST[index % PEOPLE])
}

/// A PNG of one colour, Graph's 96×96.
fn picture(colour: [u8; 3]) -> SandboxResult<Vec<u8>> {
    let [r, g, b] = colour;
    let picture = image::RgbaImage::from_pixel(96, 96, image::Rgba([r, g, b, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(picture)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// The call of the QA: connected, the QA's people in it (and nobody else made up), each with their picture served.
pub(super) fn qa_call(rig: &mut Rig<'_>) -> SandboxResult {
    let connected = rig.calls()?.iter().any(|row| row.state == "connected");
    if !connected {
        rig.people.clear();
        rig.sharing = false;
        rig.group_call()?;
    }
    if rig.people.first().is_some_and(|first| *first == qa_mri(0)) && rig.people.len() == PEOPLE {
        return Ok(());
    }
    if rig.sharing {
        rig.show(View::Gallery)?;
    }
    if !rig.people.is_empty() {
        rig.join(0)?;
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    for (index, colour) in COLOURS.iter().enumerate() {
        let path = format!(
            "/v1.0/users/{}/photos/96x96/$value",
            qa_mri(index).trim_start_matches("8:orgid:")
        );
        runtime.block_on(rig.mock.serve_bytes(&path, "image/png", picture(*colour)?));
    }
    let people: Vec<(String, String)> = (0..PEOPLE).map(|index| (qa_mri(index), qa_name(index))).collect();
    let listed: Vec<(&str, &str)> = people.iter().map(|(mri, name)| (mri.as_str(), name.as_str())).collect();
    if !rig.mock.join_made_up(&rig.call, &listed) {
        return Err("the mock's call took nobody".into());
    }
    rig.people = people.into_iter().map(|(mri, _)| mri).collect();
    rig.app
        .trace
        .wait_until(&format!("{PEOPLE} others in the grid"), |seen| {
            grid_size(seen) == Some(PEOPLE)
        })?;
    thread::sleep(SETTLE * 2);
    Ok(())
}

/// The mixer names `speakers` (the QA's people, by index) as who speaks now, the newest first; none: nobody.
pub(super) fn speak(rig: &Rig<'_>, speakers: &[usize]) -> SandboxResult {
    let mris: Vec<String> = speakers.iter().map(|index| qa_mri(*index)).collect();
    let listed: Vec<&str> = mris.iter().map(String::as_str).collect();
    if rig.mock.speak(&rig.call, LAB_A, &listed) {
        Ok(())
    } else {
        Err("the mock's mixer named nobody".into())
    }
}

fn distance(a: Rgb<u8>, b: [u8; 3]) -> i32 {
    a.0.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(y)).abs())
        .sum()
}

/// How many px of `within` have `person`'s picture colour.
pub(super) fn colour_count(shot: &RgbImage, within: Rect, person: usize) -> usize {
    let Some(wanted) = COLOURS.get(person) else {
        return 0;
    };
    (within.y.max(0)..within.bottom())
        .flat_map(|y| (within.x.max(0)..within.right()).map(move |x| (x, y)))
        .filter_map(|(x, y)| Some((u32::try_from(x).ok()?, u32::try_from(y).ok()?)))
        .filter(|(x, y)| *x < shot.width() && *y < shot.height())
        .filter(|(x, y)| distance(*shot.get_pixel(*x, *y), *wanted) <= NEAR)
        .count()
}

/// A tile as the QA reads it: the account's own (`Own`), one of the QA's people, or a tile with no known face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Seen {
    Own,
    Person(usize),
    Unknown,
}

impl Seen {
    fn person(self) -> Option<usize> {
        match self {
            Self::Person(index) => Some(index),
            _ => None,
        }
    }
}

/// What a strip or a gallery page shows now: its tiles in reading order.
pub(super) struct Page {
    pub(super) shot: RgbImage,
    pub(super) stage: Stage,
    pub(super) tiles: Vec<(Rect, Seen)>,
}

impl Page {
    pub(super) fn people(&self) -> Vec<Seen> {
        self.tiles.iter().map(|(_, seen)| *seen).collect()
    }

    /// The QA's people on it, in order.
    fn persons(&self) -> Vec<usize> {
        self.tiles.iter().filter_map(|(_, seen)| seen.person()).collect()
    }
}

/// The faces in `within` (one pass over every second px): each found person's centre, by their picture's colour.
fn faces(shot: &RgbImage, within: Rect) -> Vec<(usize, (i32, i32))> {
    let mut sums = [(0_i64, 0_i64, 0_i64); PEOPLE];
    for y in (within.y.max(0)..within.bottom()).step_by(2) {
        for x in (within.x.max(0)..within.right()).step_by(2) {
            let (Ok(px), Ok(py)) = (u32::try_from(x), u32::try_from(y)) else {
                continue;
            };
            if px >= shot.width() || py >= shot.height() {
                continue;
            }
            let colour = *shot.get_pixel(px, py);
            if let Some(index) = COLOURS.iter().position(|wanted| distance(colour, *wanted) <= NEAR) {
                let sum = &mut sums[index];
                *sum = (sum.0 + i64::from(x), sum.1 + i64::from(y), sum.2 + 1);
            }
        }
    }
    sums.iter()
        .enumerate()
        .filter(|(_, (_, _, count))| *count >= i64::try_from(FACE).unwrap_or(i64::MAX))
        .map(|(index, (x, y, count))| {
            let at = |sum: i64| i32::try_from(sum / count).unwrap_or_default();
            (index, (at(*x), at(*y)))
        })
        .collect()
}

/// The tiles in `within` in reading order: each face's tile (a stage tile centred on the face, as a tile centres its
/// face, else a tile of the own tile's size round it: a speaking tile's glow can join tiles into one blob) and the
/// account's own. They are ordered by the faces' places.
fn read_tiles(shot: &RgbImage, stage: &Stage, within: Rect) -> Vec<(Rect, Seen)> {
    let own = stage.own;
    let size = own.map_or((240, 135), |own| (own.w, own.h));
    let mut tiles: Vec<(Rect, Seen)> = faces(shot, within)
        .into_iter()
        .map(|(index, (x, y))| {
            let centred = stage
                .tiles
                .iter()
                .filter(|tile| (tile.center().0 - x).abs() <= 24 && (tile.center().1 - y).abs() <= 24)
                .min_by_key(|tile| tile.area())
                .copied();
            let tile = centred.unwrap_or(Rect {
                x: x - size.0 / 2,
                y: y - size.1 / 2,
                w: size.0,
                h: size.1,
            });
            (tile, Seen::Person(index))
        })
        .collect();
    if let Some(own) = own.filter(|own| within.holds(*own, 8)) {
        tiles.push((own, Seen::Own));
    }
    // reading order: rows of tiles whose middles are within a few px, each left to right
    tiles.sort_by_key(|(tile, _)| tile.center().1);
    let mut rows: Vec<Vec<(Rect, Seen)>> = Vec::new();
    for tile in tiles {
        match rows.last_mut() {
            Some(row) if (row[0].0.center().1 - tile.0.center().1).abs() <= 12 => row.push(tile),
            _ => rows.push(vec![tile]),
        }
    }
    rows.into_iter()
        .flat_map(|mut row| {
            row.sort_by_key(|(tile, _)| tile.center().0);
            row
        })
        .collect()
}

/// The strip beside the share now: the column the account's own tile stands in.
pub(super) fn strip(rig: &mut Rig<'_>) -> SandboxResult<Page> {
    let window = rig.window()?;
    rig.park()?;
    let shot = rig.shot_until(|shot| pixels::stage(shot, window).is_some_and(|stage| stage.own.is_some()))?;
    let stage = pixels::stage(&shot, window).ok_or("no stage")?;
    let own = stage
        .own
        .ok_or_else(|| format!("no own tile beside the share: {stage:?}"))?;
    let column = Rect {
        x: own.x - 6,
        w: stage.panel.right() - own.x + 6,
        ..stage.panel
    };
    let tiles = read_tiles(&shot, &stage, column);
    Ok(Page { shot, stage, tiles })
}

/// The strip now, from one shot, the pointer left where it is (parked already): for reads in quick turns.
fn quick_strip(rig: &mut Rig<'_>) -> SandboxResult<Page> {
    let window = rig.window()?;
    let shot = rig.shot()?;
    let stage = pixels::stage(&shot, window).ok_or("no stage")?;
    let Some(own) = stage.own else {
        return Ok(Page {
            shot,
            stage,
            tiles: Vec::new(),
        });
    };
    let column = Rect {
        x: own.x - 6,
        w: stage.panel.right() - own.x + 6,
        ..stage.panel
    };
    let tiles = read_tiles(&shot, &stage, column);
    Ok(Page { shot, stage, tiles })
}

/// The gallery now, from one shot (see [`quick_strip`]).
fn quick_gallery(rig: &mut Rig<'_>) -> SandboxResult<Page> {
    let window = rig.window()?;
    let shot = rig.shot()?;
    let stage = pixels::stage(&shot, window).ok_or("no stage")?;
    let tiles = read_tiles(&shot, &stage, stage.panel);
    Ok(Page { shot, stage, tiles })
}

/// The gallery's page now: every tile on the stage.
pub(super) fn gallery(rig: &mut Rig<'_>) -> SandboxResult<Page> {
    let window = rig.window()?;
    rig.park()?;
    let shot = rig.shot_until(|shot| pixels::stage(shot, window).is_some_and(|stage| stage.own.is_some()))?;
    let stage = pixels::stage(&shot, window).ok_or("no stage")?;
    let tiles = read_tiles(&shot, &stage, stage.panel);
    Ok(Page { shot, stage, tiles })
}

/// Saves the window as `page`'s shot shows it.
fn save_page(rig: &mut Rig<'_>, page: &Page, name: &str) -> SandboxResult {
    super::save_shot(rig, &page.shot, name)
}

fn fail(rig: &mut Rig<'_>, name: &str, error: String) -> SandboxResult {
    rig.save(&format!("failed-qa666-{name}"))?;
    Err(error.into())
}

/// The step (see the module docs).
pub(super) fn qa_order(rig: &mut Rig<'_>) -> SandboxResult {
    qa_call(rig)?;
    speak(rig, &[])?;
    rig.show(View::Gallery)?;
    rig.resize((1280, 800))?;
    // a far-end share by someone on a later page: Ike..Ned are pages away from the first
    let presenter = 10;
    if !rig.mock.share_made_up(&rig.call, &qa_mri(presenter), true) {
        return Err("the mock's share didn't go".into());
    }
    rig.sharing = true;
    rig.wait_share()?;
    thread::sleep(SETTLE);
    let first = strip(rig)?;
    say(&format!("qa666 share strip: {:?}", first.people()));
    rig.save("qa666-share-presenter-first")?;
    let people = first.people();
    if people.first() != Some(&Seen::Person(presenter)) || people.last() != Some(&Seen::Own) {
        return fail(
            rig,
            "presenter",
            format!("the strip shows {people:?}: wanted the presenter first, own last"),
        );
    }
    let shown = people.len() - 1;
    say(&format!(
        "qa666: the strip shows {shown} others and the own tile on a page"
    ));
    // a speaker pages away comes to the first page, behind the presenter, after about a second
    rig.park()?;
    let start = Instant::now();
    speak(rig, &[8])?;
    let moved = wait_order(
        rig,
        start,
        |page| page.persons().get(1) == Some(&8),
        "Jo to the first page",
    )?;
    say(&format!("qa666: Jo (page 3) came to the first page after {moved:?}"));
    rig.save("qa666-share-speaker-moved-up")?;
    check_window(moved, Duration::from_secs(1), "Jo's move")?;
    holds_still(rig, presenter)?;
    every_page(rig)?;
    page_stays(rig)?;
    looks(rig)?;
    after_share(rig, presenter)
}

/// When a change showed, from the start of a wait: after `lo` (the start of the last read without it) and before `hi`
/// (the end of the first read with it).
#[derive(Clone, Copy, Debug)]
struct Moved {
    lo: Duration,
    hi: Duration,
}

/// A short sound moves nothing; a second speaker right after the first moves only when the order held still 3 s.
fn holds_still(rig: &mut Rig<'_>, presenter: usize) -> SandboxResult {
    let after_first_move = Instant::now();
    // a short sound (0.5 s) of someone pages away moves nothing, well after the hold
    thread::sleep(Duration::from_millis(3_500));
    let before = strip(rig)?.people();
    speak(rig, &[11])?;
    thread::sleep(Duration::from_millis(500));
    speak(rig, &[])?;
    rig.park()?;
    let still = still_for(rig, Duration::from_secs(3), &before)?;
    if let Some(changed) = still {
        return fail(
            rig,
            "short-sound",
            format!("a 0.5 s sound moved the strip: {before:?} -> {changed:?}"),
        );
    }
    say("qa666: a 0.5 s sound of Mo (page 4) moved nothing for 3 s");
    let mut pair = (5, 9);
    for _ in 0..HOLD_ROUNDS {
        match hold_round(rig, presenter, pair, after_first_move)? {
            Hold::Shown => return Ok(()),
            Hold::Late(read_to) => {
                say(&format!(
                    "qa666: the hold read ended {read_to:?} after the first speaker's start, past the hold's earliest \
                     end, and shows the second moved: it proves nothing, so two others speak again"
                ));
                // the second speaker's move starts a new hold
                thread::sleep(HOLD_GAP);
                pair = out_of_sight_pair(rig, presenter)?;
            }
        }
    }
    fail(
        rig,
        "hold-late",
        format!("each of {HOLD_ROUNDS} hold reads ended after {HOLD_ENDS:?}: the hold was never seen"),
    )
}

/// What one round of the hold showed.
enum Hold {
    /// The order held, and the second speaker moved after it.
    Shown,
    /// The read ended after the hold could end, and showed the second speaker moved: no proof either way.
    Late(Duration),
}

/// Two people on a later page of the strip (never the presenter), for a new round of the hold.
fn out_of_sight_pair(rig: &mut Rig<'_>, presenter: usize) -> SandboxResult<(usize, usize)> {
    let first = strip(rig)?.persons();
    let mut away = (0..PEOPLE)
        .rev()
        .filter(|index| *index != presenter && !first.contains(index));
    match (away.next(), away.next()) {
        (Some(second), Some(first_speaker)) => Ok((first_speaker, second)),
        _ => Err(format!("fewer than two people past the first page: {first:?}").into()),
    }
}

/// The hold: `first` speaks and moves up after a second; `second` starts 1.2 s after. Without the hold `second` would
/// move 2.2 s after the first's start; with it only 3 s after the first's move (about 4 s after the start). A read
/// between 2.7 s and [`HOLD_ENDS`] shows the first moved and the second not yet.
fn hold_round(
    rig: &mut Rig<'_>,
    presenter: usize,
    (first, second): (usize, usize),
    after_first_move: Instant,
) -> SandboxResult<Hold> {
    rig.park()?;
    let start = Instant::now();
    speak(rig, &[first])?;
    thread::sleep(Duration::from_millis(1_200).saturating_sub(start.elapsed()));
    speak(rig, &[second])?;
    thread::sleep(Duration::from_millis(2_700).saturating_sub(start.elapsed()));
    let read_from = start.elapsed();
    let between = quick_strip(rig)?;
    let read_to = start.elapsed();
    let middle = between.persons();
    say(&format!(
        "qa666: the hold of {first} and {second}: the read from {read_from:?} to {read_to:?} after the first's start \
         shows {middle:?}"
    ));
    save_page(rig, &between, "qa666-share-hold-middle")?;
    if middle.get(1) == Some(&second) {
        if read_to > HOLD_ENDS {
            speak(rig, &[])?;
            return Ok(Hold::Late(read_to));
        }
        fail(
            rig,
            "hold",
            format!("{second} moved by {read_to:?} after {first}'s start: the order did not hold 3 s after the move"),
        )?;
    }
    if middle.get(1) != Some(&first) {
        fail(
            rig,
            "hold-first",
            format!("{first} did not move up by {read_to:?}: {middle:?}"),
        )?;
    }
    let moved = wait_order(
        rig,
        start,
        |page| page.persons().get(1) == Some(&second),
        "the second speaker to the first page",
    )?;
    say(&format!(
        "qa666: {second} moved {moved:?} after {first}'s start (the hold ends about 4 s after it; {:?} since Jo's)",
        after_first_move.elapsed()
    ));
    rig.save("qa666-share-hold")?;
    check_window(
        moved,
        Duration::from_secs(4),
        "the second speaker's move after the hold",
    )?;
    speak(rig, &[])?;
    let order = strip(rig)?.people();
    say(&format!("qa666: the first page now {order:?}"));
    if order.get(..3) != Some(&[Seen::Person(presenter), Seen::Person(second), Seen::Person(first)][..]) {
        fail(
            rig,
            "speakers-order",
            format!("wanted the presenter, {second}, {first}: {order:?}"),
        )?;
    }
    Ok(Hold::Shown)
}

/// Waits for the strip to satisfy `done`, from `start` (see [`wait_page`]).
fn wait_order(rig: &mut Rig<'_>, start: Instant, done: impl Fn(&Page) -> bool, what: &str) -> SandboxResult<Moved> {
    wait_page(rig, start, done, what, quick_strip)
}

/// Reads the page with `read` until it satisfies `done`: when the change came, as an interval from `start`. Fails
/// when it comes earlier than the settle time allows, or not in 10 s.
fn wait_page(
    rig: &mut Rig<'_>,
    start: Instant,
    done: impl Fn(&Page) -> bool,
    what: &str,
    read: fn(&mut Rig<'_>) -> SandboxResult<Page>,
) -> SandboxResult<Moved> {
    let mut lo = Duration::ZERO;
    loop {
        let taken = start.elapsed();
        let page = read(rig)?;
        if done(&page) {
            let seen = Moved {
                lo,
                hi: start.elapsed(),
            };
            if seen.hi < NOT_BEFORE {
                rig.save(&format!("failed-qa666-early-{}", what.replace(' ', "-")))?;
                return Err(format!("{what}: it moved already {seen:?} after the speech started").into());
            }
            return Ok(seen);
        }
        lo = taken;
        if start.elapsed() > Duration::from_secs(10) {
            rig.save(&format!("failed-qa666-{}", what.replace(' ', "-")))?;
            return Err(format!("{what}: not in 10 s; the page shows {:?}", page.people()).into());
        }
    }
}

/// A move seen in `seen`, due `due` after its start: not too late.
fn check_window(seen: Moved, due: Duration, what: &str) -> SandboxResult {
    if seen.lo > due + LATE {
        return Err(format!("{what}: seen {seen:?} after its start, due at {due:?}").into());
    }
    Ok(())
}

/// The strip's people stay `before` for `time`: `None`, or what it showed instead.
fn still_for(rig: &mut Rig<'_>, time: Duration, before: &[Seen]) -> SandboxResult<Option<Vec<Seen>>> {
    let start = Instant::now();
    while start.elapsed() < time {
        let now = quick_strip(rig)?.people();
        if now != before {
            return Ok(Some(now));
        }
    }
    Ok(None)
}

/// Every strip page ends with the account's own tile, and the pages show everyone once.
fn every_page(rig: &mut Rig<'_>) -> SandboxResult {
    let mut seen: Vec<usize> = Vec::new();
    let mut all_pages = Vec::new();
    for turn in 0..10 {
        let page = strip(rig)?;
        let people = page.people();
        all_pages.push(people.clone());
        if people.last() != Some(&Seen::Own) {
            return fail(
                rig,
                "own-last",
                format!("strip page {}: {people:?}: the own tile is not last", turn + 1),
            );
        }
        if people.contains(&Seen::Unknown) {
            return fail(
                rig,
                "unknown-face",
                format!("strip page {}: a tile with no known face: {people:?}", turn + 1),
            );
        }
        seen.extend(page.persons());
        let Some(pager) = page.stage.pager else { break };
        let column = Rect {
            x: page.stage.own.map_or(page.stage.panel.x, |own| own.x - 6),
            ..page.stage.panel
        };
        rig.click(pager.next)?;
        let after = rig.shot()?;
        if pixels::changed_pixels(&page.shot, &after, column) < 40 {
            break;
        }
    }
    say(&format!("qa666: the strip's pages {all_pages:?}"));
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != seen.len() || seen.len() != PEOPLE {
        return fail(
            rig,
            "pages",
            format!("the pages showed {seen:?}: wanted each of {PEOPLE} once"),
        );
    }
    first_strip_page(rig)
}

/// Back on the strip's first page, by `<`.
fn first_strip_page(rig: &mut Rig<'_>) -> SandboxResult {
    for _ in 0..12 {
        let page = strip(rig)?;
        let Some(pager) = page.stage.pager else { return Ok(()) };
        let column = Rect {
            x: page.stage.own.map_or(page.stage.panel.x, |own| own.x - 6),
            ..page.stage.panel
        };
        rig.click(pager.previous)?;
        let after = rig.shot()?;
        if pixels::changed_pixels(&page.shot, &after, column) < 40 {
            return Ok(());
        }
    }
    Err("the strip's first page didn't come".into())
}

/// The user turns to page 2: one there who speaks stays in place; one out of sight who speaks moves to the first page,
/// and page 2 stays the page shown.
fn page_stays(rig: &mut Rig<'_>) -> SandboxResult {
    thread::sleep(Duration::from_millis(3_200));
    let first = strip(rig)?;
    let pager = first.stage.pager.ok_or("the strip has no pager")?;
    rig.click(pager.next)?;
    let second = strip(rig)?;
    let on_two = second.persons();
    say(&format!("qa666: strip page 2 {:?}", second.people()));
    rig.save("qa666-share-page-2")?;
    let in_sight = *on_two.get(1).ok_or("page 2 holds fewer than two people")?;
    speak(rig, &[in_sight])?;
    let still = still_for(rig, Duration::from_millis(3_500), &second.people())?;
    speak(rig, &[])?;
    if let Some(changed) = still {
        return fail(
            rig,
            "in-sight",
            format!(
                "{in_sight}, in sight on page 2, spoke: the page went {:?} -> {changed:?}",
                second.people()
            ),
        );
    }
    // someone on the last page speaks while page 2 shows
    let all: Vec<usize> = (0..PEOPLE).collect();
    let first_two: Vec<usize> = first.persons().into_iter().chain(on_two.iter().copied()).collect();
    let away = *all
        .iter()
        .rev()
        .find(|index| !first_two.contains(index))
        .ok_or("nobody past page 2")?;
    thread::sleep(Duration::from_millis(3_200));
    let pager_before = strip(rig)?.stage.pager.ok_or("page 2 has no pager")?;
    let before_shot = rig.shot()?;
    rig.park()?;
    let start = Instant::now();
    speak(rig, &[away])?;
    // page 2 now starts with the one the first page gave up
    let pushed = first.persons().last().copied().ok_or("the first page held nobody")?;
    let moved = wait_order(
        rig,
        start,
        |page| page.persons().first() == Some(&pushed),
        "page 2 takes the first page's last",
    )?;
    speak(rig, &[])?;
    let now = strip(rig)?;
    say(&format!(
        "qa666: {away} spoke out of sight; after {moved:?} page 2 shows {:?}",
        now.people()
    ));
    rig.save("qa666-share-page-2-after-move")?;
    let pager_now = now.stage.pager.ok_or("no pager after the move")?;
    let text_changed = pixels::changed_pixels(&before_shot, &now.shot, pager_before.text);
    if pager_now != pager_before || text_changed > 0 {
        return fail(
            rig,
            "page-kept",
            format!("the pager changed: {pager_before:?} -> {pager_now:?}, {text_changed} px of its text"),
        );
    }
    first_strip_page(rig)?;
    let first_now = strip(rig)?.persons();
    say(&format!("qa666: the first page now {first_now:?}"));
    if first_now.get(1) != Some(&away) {
        return fail(
            rig,
            "away-first",
            format!("{away} isn't behind the presenter: {first_now:?}"),
        );
    }
    rig.save("qa666-share-out-of-sight-moved")
}

/// Shots of the strip in Teams Dark, at a font of 18 px and in the smallest window.
fn looks(rig: &mut Rig<'_>) -> SandboxResult {
    rig.theme(teams_ui::theme::DARK_KEY)?;
    rig.save("qa666-share-strip-dark")?;
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    rig.theme(&format!("{}18", super::FONT_PREFIX))?;
    rig.save("qa666-share-strip-font-18")?;
    rig.resize((560, 400))?;
    rig.save("qa666-share-strip-560x400-font-18")?;
    rig.theme(&format!("{}{}", super::FONT_PREFIX, teams_ui::theme::FONT_SIZE))?;
    rig.save("qa666-share-strip-560x400")?;
    rig.resize((800, 560))?;
    let page = strip(rig)?;
    say(&format!("qa666: the strip at 800x560 {:?}", page.people()));
    rig.save("qa666-share-strip-800x560")?;
    rig.resize((1280, 800))
}

/// The share ends: the gallery and the speaker view start with the same speakers, in the same order.
fn after_share(rig: &mut Rig<'_>, presenter: usize) -> SandboxResult {
    let strip_order = strip(rig)?.persons();
    if !rig.mock.share_made_up(&rig.call, &qa_mri(presenter), false) {
        return Err("the mock's share didn't stop".into());
    }
    rig.sharing = false;
    thread::sleep(SETTLE * 2);
    rig.show(View::Gallery)?;
    let page = gallery(rig)?;
    let order = page.persons();
    say(&format!(
        "qa666: the gallery after the share {:?} (the strip was {strip_order:?})",
        page.people()
    ));
    rig.save("qa666-gallery-after-share")?;
    // the speakers kept their order: the strip's after its presenter
    let speakers: Vec<usize> = strip_order.iter().skip(1).copied().collect();
    if order.get(..speakers.len()) != Some(&speakers[..]) {
        return fail(
            rig,
            "gallery-order",
            format!("the gallery starts {order:?}, wanted the speakers {speakers:?}"),
        );
    }
    if page.people().last() != Some(&Seen::Own) {
        return fail(
            rig,
            "gallery-own-last",
            format!("the gallery's own tile is not last: {:?}", page.people()),
        );
    }
    rig.show(View::Speaker)?;
    thread::sleep(SETTLE);
    let speaker = gallery(rig)?;
    say(&format!("qa666: the speaker view {:?}", speaker.people()));
    rig.save("qa666-speaker-after-share")?;
    let shown = speaker.persons();
    let first_speakers: Vec<usize> = speakers.iter().copied().filter(|index| shown.contains(index)).collect();
    if shown
        .iter()
        .copied()
        .filter(|index| speakers.contains(index))
        .collect::<Vec<_>>()
        != first_speakers
    {
        return fail(
            rig,
            "speaker-order",
            format!("the speaker view shows {shown:?}, the speakers' order is {speakers:?}"),
        );
    }
    gallery_move(rig)
}

/// In the gallery (Max gallery size 9): someone on page 2 speaks and comes to the first page after about a second.
fn gallery_move(rig: &mut Rig<'_>) -> SandboxResult {
    rig.show(View::Gallery)?;
    rig.gallery_size(9, PEOPLE)?;
    thread::sleep(Duration::from_millis(3_200));
    let page = gallery(rig)?;
    let on_first = page.persons();
    let away = (0..PEOPLE)
        .rev()
        .find(|index| !on_first.contains(index))
        .ok_or("everyone on the gallery's first page")?;
    rig.park()?;
    let start = Instant::now();
    speak(rig, &[away])?;
    let moved = wait_page(
        rig,
        start,
        |page| page.persons().first() == Some(&away),
        "the gallery's speaker to the first page",
        quick_gallery,
    )?;
    speak(rig, &[])?;
    say(&format!(
        "qa666: in the gallery, {away} (page 2) came first after {moved:?}"
    ));
    rig.save("qa666-gallery-speaker-moved-up")?;
    check_window(moved, Duration::from_secs(1), "the gallery's move")?;
    rig.gallery_size(49, PEOPLE)
}
