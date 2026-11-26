//! QA of task 867 (round 1), in the UI sandbox: the live captions area in the call window scrolls and resizes, as a
//! user meets it. The real app runs as a child in the sandbox, as mock account a or b, its settings file kept by the
//! parent, so a later run is a restart of the same app ([`child`]). The child tells the app a made-up group call and
//! its made-up caption lines. The parent turns the wheel, presses keys and drags with the sandbox's pointer. It finds
//! the list and the edge by the focus that Tab gives them, and it sees a follow of the newest line as a change of the
//! lines on the screen when a new line comes.
//!
//! - **follow**: many lines; a new line moves the lines (they follow); a wheel scroll up stops that; a scroll back to
//!   the end starts it again.
//! - **tab**: Tab reaches the list (its ring) and the edge (its grab bar turns brand).
//! - **list keys**: Home, Up and Page Up stop the follow; End starts it again.
//! - **edge keys**: Up and Down by 20 px, Page Up and Page Down by 100 px, Home the least (100 px), End the most (the
//!   room that leaves the stage 180 px, at most 1200 px), each saved at once.
//! - **drag**: up and down; a drag saves once, when it ends; it stops at the least and the most. The tooltip on hover.
//! - **window**: a shorter window keeps the stage's least room, and the saved height stays.
//! - **hidden**: the captions hidden at the top of their lines and shown again start at the end. The transcript pane
//!   still opens and draws its new lines.
//! - **themes**: shots of a short and a tall area in Teams Light and Dark, with the list's ring.
//! - **restart**: the height survives a restart for a; b starts at the default and keeps its own.
//!
//! `QA867_EVIDENCE` names a directory that gets the screenshots and the notes too; the shots also go to
//! `target/ui-shots/qa867-*.png`. Without weston 15, niri or `dbus-daemon` it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use image::RgbImage;
use serde_json::Value;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

#[path = "qa_captions_area_child.rs"]
mod child;

const EVIDENCE_ENV: &str = "QA867_EVIDENCE";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(150);
const SETTLE: Duration = Duration::from_millis(900);
const STEP: Duration = Duration::from_millis(250);
/// The call window's header, px.
const HEADER: u32 = 56;
/// The call window's frame around the stage and the area, px.
const FRAME: u32 = 8;
/// The least room of the stage, px.
const STAGE_MIN: u32 = 180;
/// The least height of the area, px.
const MIN: u32 = 100;
/// The most height of the area, px.
const MAX: u32 = 1200;
/// The default height of the area, px.
const DEFAULT: u32 = 244;
/// The output's size.
const SCREEN: (u32, u32) = (1280, 800);
/// The most Tab presses that look for the list and the edge.
const TABS: usize = 40;
/// The transcript pane's width, px.
const PANE_WIDTH: u32 = 320;
/// How many pixels of the lines must change to say that they moved.
const MOVED: usize = 200;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(child::CHILD_ENV) {
        Ok(letter) => child::child(&letter),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_captions_area_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn evidence() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

/// Adds `line` to the evidence notes and says it.
fn note(line: &str) {
    say(&format!("note {line}"));
    if let Some(dir) = evidence() {
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("notes.txt"))
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// The checks: each passed or failed one is noted; a failed one fails the test at the end.
static CHECKS: Mutex<(usize, Vec<String>)> = Mutex::new((0, Vec::new()));

fn check(name: &str, holds: bool, detail: impl FnOnce() -> String) {
    let detail = detail();
    let mut checks = CHECKS.lock().unwrap_or_else(PoisonError::into_inner);
    if holds {
        checks.0 += 1;
        note(&format!("PASS {name}: {detail}"));
    } else {
        checks.1.push(format!("{name}: {detail}"));
        note(&format!("FAIL {name}: {detail}"));
    }
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_captions_area_window", &options)? else {
        return Ok(());
    };
    let root = tempfile::tempdir()?;
    let outcome = walk(&sandbox, root.path());
    let (passed, failed) = {
        let checks = CHECKS.lock().unwrap_or_else(PoisonError::into_inner);
        (checks.0, checks.1.clone())
    };
    note(&format!("checks: {passed} passed, {} failed", failed.len()));
    outcome.map_err(|error| format!("{error}{}", sandbox.logs()))?;
    if failed.is_empty() {
        say("qa_captions_area_window: ok");
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

fn walk(sandbox: &Sandbox, root: &Path) -> SandboxResult {
    let mut user = User::start(sandbox, root, "a")?;
    let outcome = first_run(&mut user);
    user.finish(outcome)?;
    let saved = height_of(&User::settings_text(root), "a");
    let mut user = User::start(sandbox, root, "a")?;
    let outcome = restarted(&mut user, saved);
    user.finish(outcome)?;
    let mut user = User::start(sandbox, root, "b")?;
    let outcome = other_account(&mut user, saved);
    user.finish(outcome)?;
    let mut user = User::start(sandbox, root, "a")?;
    let outcome = a_again(&mut user, saved);
    user.finish(outcome)?;
    note(&format!("settings file at the end:\n{}", User::settings_text(root)));
    Ok(())
}

/// The height the settings file holds for `letter`, as the text shows it: `None` when it keeps none (the default).
fn height_of(text: &str, letter: &str) -> Option<u32> {
    let mri = match letter {
        "a" => teams_mock::LAB_A,
        _ => teams_mock::LAB_B,
    };
    text.lines()
        .skip_while(|line| line.trim() != "[caption_height]")
        .take_while(|line| !line.trim().is_empty())
        .find(|line| line.contains(mri))
        .and_then(|line| line.rsplit('=').next())
        .and_then(|value| value.trim().parse().ok())
}

// --- the runs ---

fn first_run(user: &mut User<'_>) -> SandboxResult {
    user.open_call(40)?;
    user.shot("a-call-default")?;
    follow(user)?;
    let found = tab_walk(user, "a")?;
    list_keys(user, &found)?;
    edge_keys(user, &found)?;
    drag(user, &found)?;
    themes(user, &found)?;
    window_resize(user, &found)?;
    hidden_and_shown(user, &found)?;
    transcript(user)?;
    // a known height for the next runs: the least, then up twice by a page
    let found = tab_walk(user, "a-after-transcript")?;
    user.focus_edge(&found)?;
    user.keys(&["home", "pageup", "pageup"])?;
    let state = user.state()?;
    check(
        "a's height before the restart",
        state["height_a"].as_u64() == Some(u64::from(MIN + 200)),
        || format!("a's height {}", state["height_a"]),
    );
    Ok(())
}

fn restarted(user: &mut User<'_>, saved: Option<u32>) -> SandboxResult {
    let state = user.state()?;
    check(
        "restart keeps a's height",
        saved == Some(MIN + 200) && state["height_a"].as_u64() == saved.map(u64::from),
        || format!("the file had {saved:?}, the app has {}", state["height_a"]),
    );
    user.open_call(12)?;
    let found = tab_walk(user, "a-restarted")?;
    user.focus_edge(&found)?;
    let drawn = user.drawn_edge()?;
    let window = user.window_height()?;
    check(
        "restart draws a's height",
        drawn.is_some_and(|edge| (window - FRAME - edge).abs_diff(MIN + 200) <= 6),
        || format!("the edge at y {drawn:?}, the window {window} px tall"),
    );
    user.shot("a-restarted")
}

fn other_account(user: &mut User<'_>, saved: Option<u32>) -> SandboxResult {
    let state = user.state()?;
    check(
        "b starts at the default",
        state["height_b"].as_u64() == Some(u64::from(DEFAULT)),
        || format!("b's height {}", state["height_b"]),
    );
    user.open_call(12)?;
    user.shot("b-default")?;
    let found = tab_walk(user, "b")?;
    user.focus_edge(&found)?;
    user.keys(&["home"])?;
    let state = user.state()?;
    check(
        "b's own height, a's kept",
        state["height_b"].as_u64() == Some(u64::from(MIN)) && state["height_a"].as_u64() == saved.map(u64::from),
        || format!("b {} a {} (a's saved {saved:?})", state["height_b"], state["height_a"]),
    );
    user.shot("b-least")
}

fn a_again(user: &mut User<'_>, saved: Option<u32>) -> SandboxResult {
    let state = user.state()?;
    check(
        "a again keeps its height, b keeps its own",
        state["height_a"].as_u64() == saved.map(u64::from) && state["height_b"].as_u64() == Some(u64::from(MIN)),
        || format!("a {} b {}", state["height_a"], state["height_b"]),
    );
    Ok(())
}

/// The lines follow the newest; a wheel up stops that; a wheel back down to the end starts it again.
fn follow(user: &mut User<'_>) -> SandboxResult {
    let moved = user.moved_by_new_line()?;
    check("many lines: a new line moves them (follow)", moved >= MOVED, || {
        format!("{moved} px changed")
    });
    let (x, y) = user.lines_point()?;
    user.input.scroll(x, y, -5)?;
    thread::sleep(SETTLE);
    user.park()?;
    user.shot("a-wheel-up")?;
    let moved = user.moved_by_new_line()?;
    check("wheel up: a new line leaves them (no follow)", moved < MOVED, || {
        format!("{moved} px changed")
    });
    user.input.scroll(x, y, 60)?;
    thread::sleep(SETTLE);
    user.park()?;
    let moved = user.moved_by_new_line()?;
    check(
        "wheel back to the end: a new line moves them (follow)",
        moved >= MOVED,
        || format!("{moved} px changed"),
    );
    Ok(())
}

/// Where Tab puts the focus on the area: the list's ring and the edge's grab bar.
#[derive(Debug, Default)]
struct Found {
    /// The number of Tabs to the list, and a point on its ring.
    list: Option<(usize, (u32, u32))>,
    /// The number of Tabs to the edge, and the grab bar's centre.
    edge: Option<(usize, (u32, u32))>,
}

/// Presses Tab from the window's first focus and finds the list and the edge by what changes in the area.
fn tab_walk(user: &mut User<'_>, prefix: &str) -> SandboxResult<Found> {
    user.ask("activate")?;
    user.park()?;
    thread::sleep(SETTLE);
    let base = user.image()?;
    let height = user.window_height()?;
    let area_top = height.saturating_sub(FRAME + user.height()?);
    let mut found = Found::default();
    for tab in 1..=TABS {
        user.keys(&["tab"])?;
        thread::sleep(STEP);
        let now = user.image()?;
        let Some(changed) = changed_box(&base, &now, area_top.saturating_sub(4), height) else {
            continue;
        };
        let (width, tall) = (changed.2 - changed.0 + 1, changed.3 - changed.1 + 1);
        if found.list.is_none() && width > 300 && tall > 40 {
            let ring = (changed.0, changed.1.midpoint(changed.3));
            found.list = Some((tab, ring));
            check(
                &format!("{prefix}: the list's ring shows on all four sides"),
                ring_whole(&now, changed),
                || format!("ring box {changed:?}"),
            );
            user.shot(&format!("{prefix}-tab-list"))?;
        } else if found.edge.is_none() && width <= 48 && tall <= 10 {
            let centre = (changed.0.midpoint(changed.2), changed.1.midpoint(changed.3));
            found.edge = Some((tab, centre));
            user.shot(&format!("{prefix}-tab-edge"))?;
        }
        if found.list.is_some() && found.edge.is_some() {
            break;
        }
    }
    check(&format!("{prefix}: Tab reaches the list"), found.list.is_some(), || {
        format!("{:?}", found.list)
    });
    check(&format!("{prefix}: Tab reaches the edge"), found.edge.is_some(), || {
        format!("{:?}", found.edge)
    });
    Ok(found)
}

/// The list's keys: Home, Up and Page Up stop the follow; End starts it again.
fn list_keys(user: &mut User<'_>, found: &Found) -> SandboxResult {
    if found.list.is_none() {
        return Ok(());
    }
    user.focus_list(found)?;
    user.keys(&["home"])?;
    user.shot("a-list-home")?;
    let moved = user.moved_by_new_line()?;
    check("list Home: a new line leaves the lines", moved < MOVED, || {
        format!("{moved} px changed")
    });
    user.keys(&["end"])?;
    let moved = user.moved_by_new_line()?;
    check("list End: a new line moves them (follow)", moved >= MOVED, || {
        format!("{moved} px changed")
    });
    user.keys(&["up"])?;
    let moved = user.moved_by_new_line()?;
    check("list Up: a new line leaves them", moved < MOVED, || {
        format!("{moved} px changed")
    });
    user.keys(&["end", "pageup"])?;
    let moved = user.moved_by_new_line()?;
    check("list Page Up: a new line leaves them", moved < MOVED, || {
        format!("{moved} px changed")
    });
    user.keys(&["pagedown", "pagedown", "pagedown", "pagedown"])?;
    let moved = user.moved_by_new_line()?;
    check(
        "list Page Down to the end: a new line moves them",
        moved >= MOVED,
        || format!("{moved} px changed"),
    );
    user.keys(&["down"])?;
    let moved = user.moved_by_new_line()?;
    check("list Down at the end: still follows", moved >= MOVED, || {
        format!("{moved} px changed")
    });
    Ok(())
}

/// The most height in a window `window` px tall.
fn most(window: u32) -> u32 {
    window.saturating_sub(HEADER + FRAME + STAGE_MIN).min(MAX)
}

/// The edge's keys: each saves at once, inside the least and the most.
fn edge_keys(user: &mut User<'_>, found: &Found) -> SandboxResult {
    if found.edge.is_none() {
        return Ok(());
    }
    user.focus_edge(found)?;
    let start = user.height()?;
    let most = most(user.window_height()?);
    let steps: [(&str, i32); 4] = [("up", 20), ("down", -20), ("pageup", 100), ("pagedown", -100)];
    let mut height = start;
    for (key, by) in steps {
        user.keys(&[key])?;
        let now = user.height()?;
        let want = height.saturating_add_signed(by);
        check(&format!("edge {key}"), now == want, || {
            format!("{height} -> {now}, expected {want}")
        });
        height = now;
    }
    user.keys(&["home"])?;
    let now = user.height()?;
    check("edge Home: the least", now == MIN, || format!("{now}"));
    user.keys(&["down", "pagedown"])?;
    let now = user.height()?;
    check("edge Down at the least: stays", now == MIN, || format!("{now}"));
    user.shot("a-edge-least")?;
    user.keys(&["end"])?;
    let now = user.height()?;
    check("edge End: the most", now.abs_diff(most) <= 2, || {
        format!("{now}, expected {most} (window less header and stage)")
    });
    user.keys(&["up", "pageup"])?;
    let after = user.height()?;
    check("edge Up at the most: stays", after == now, || {
        format!("{now} -> {after}")
    });
    user.shot("a-edge-most")?;
    let drawn = user.drawn_edge()?;
    let window = user.window_height()?;
    check(
        "the most leaves the stage its least room",
        drawn.is_some_and(|edge| edge + 4 >= HEADER + STAGE_MIN),
        || format!("the edge at y {drawn:?} in a window {window} px tall"),
    );
    user.keys(&["home", "pageup"])?;
    Ok(())
}

/// A drag of the edge up and down: a drag saves once, when it ends, inside the least and the most.
fn drag(user: &mut User<'_>, found: &Found) -> SandboxResult {
    let Some((_, (x, _))) = found.edge else {
        return Ok(());
    };
    let window = user.window_height()?;
    let start = user.height()?;
    let edge = window - FRAME - start + 3;
    user.input.move_to(x, edge)?;
    thread::sleep(Duration::from_millis(1500));
    user.shot("a-edge-hover")?;
    user.input.button_down(keys::BTN_LEFT)?;
    for step in 1..=15 {
        user.input.move_to(x, edge - step * 10)?;
        thread::sleep(Duration::from_millis(30));
    }
    thread::sleep(STEP);
    let during = user.height()?;
    check("a drag saves nothing while it goes on", during == start, || {
        format!("{start} -> {during} during the drag")
    });
    user.shot("a-drag-during")?;
    user.input.button_up(keys::BTN_LEFT)?;
    thread::sleep(STEP);
    let now = user.height()?;
    check("drag up 150 px", now.abs_diff(start + 150) <= 2, || {
        format!("{start} -> {now}, expected {}", start + 150)
    });
    let edge = window - FRAME - now + 3;
    user.drag_to(x, edge, window - 1)?;
    let now = user.height()?;
    check("drag down to the window's foot: the least", now == MIN, || {
        format!("{now}")
    });
    let edge = window - FRAME - now + 3;
    user.drag_to(x, edge, 1)?;
    let now = user.height()?;
    let most = most(window);
    check("drag up to the window's top: the most", now.abs_diff(most) <= 2, || {
        format!("{now}, expected {most}")
    });
    Ok(())
}

/// A short and a tall area in Teams Light and Dark, the list's ring and the edge on hover too.
fn themes(user: &mut User<'_>, found: &Found) -> SandboxResult {
    let Some((_, (x, _))) = found.edge else {
        return Ok(());
    };
    for theme in ["dark", "light"] {
        user.ask(&format!("theme {theme}"))?;
        user.focus_edge(found)?;
        user.keys(&["end"])?;
        user.park()?;
        user.shot(&format!("theme-{theme}-tall"))?;
        user.focus_list(found)?;
        user.shot(&format!("theme-{theme}-ring"))?;
        user.focus_edge(found)?;
        user.keys(&["home"])?;
        user.shot(&format!("theme-{theme}-short"))?;
        let edge = user.window_height()? - FRAME - MIN + 3;
        user.input.move_to(x, edge)?;
        thread::sleep(Duration::from_millis(1500));
        user.shot(&format!("theme-{theme}-hover"))?;
        user.park()?;
    }
    user.focus_edge(found)?;
    user.keys(&["end"])?;
    Ok(())
}

/// A window that gets shorter while the area is at its most keeps the stage's least room; the saved height stays.
fn window_resize(user: &mut User<'_>, found: &Found) -> SandboxResult {
    if found.edge.is_none() {
        return Ok(());
    }
    user.focus_edge(found)?;
    user.keys(&["end"])?;
    let saved = user.height()?;
    user.sandbox.niri_msg(&["action", "set-window-height", "520"])?;
    let short = user.until_window(|height| height <= 530)?;
    thread::sleep(SETTLE);
    let png = user.sandbox.screenshot("qa867-window-short.png", true)?;
    User::keep(&png, "a-window-short")?;
    let image = image::open(&png)?.to_rgb8();
    let drawn = brand_bar(&image);
    check(
        "a shorter window keeps the stage's least room",
        drawn.is_some_and(|edge| edge + 4 >= HEADER + STAGE_MIN),
        || format!("the edge at y {drawn:?} in a window {short} px tall"),
    );
    let now = user.height()?;
    check("a shorter window keeps the saved height", now == saved, || {
        format!("{saved} -> {now}")
    });
    user.sandbox.niri_msg(&["action", "set-window-height", "100%"])?;
    user.until_window(|height| height >= SCREEN.1 - 10)?;
    thread::sleep(SETTLE);
    let drawn = user.drawn_edge()?;
    let window = user.window_height()?;
    check(
        "the full window again draws the saved height",
        drawn.is_some_and(|edge| (window - FRAME - edge).abs_diff(saved) <= 6),
        || format!("the edge at y {drawn:?}, the window {window}, saved {saved}"),
    );
    user.keys(&["home", "pageup"])?;
    Ok(())
}

/// The captions hidden while at the top of their lines (Alt+Shift+C; the engine tells them off), then shown again
/// (Alt+Shift+C; the engine tells new lines): the area starts at the end.
fn hidden_and_shown(user: &mut User<'_>, found: &Found) -> SandboxResult {
    user.focus_list(found)?;
    user.keys(&["home"])?;
    user.park()?;
    user.keys(&["alt+shift+c"])?;
    user.ask("captions-off")?;
    let state = user.until("captions hidden", |state| state["shown"] == Value::Bool(false))?;
    note(&format!("hidden: {state}"));
    user.shot("a-hidden")?;
    user.keys(&["alt+shift+c"])?;
    // fewer lines than the engine keeps: a new line then drops no old one, so only a scroll to the end moves them
    user.say_more(40)?;
    user.until("captions shown", |state| state["shown"] == Value::Bool(true))?;
    thread::sleep(SETTLE);
    user.shot("a-shown-again")?;
    let moved = user.moved_by_new_line()?;
    check("shown again: at the end, a new line moves them", moved >= MOVED, || {
        format!("{moved} px changed")
    });
    Ok(())
}

/// The transcript pane of task 868 still opens and fills beside the area: its new lines draw in the pane.
fn transcript(user: &mut User<'_>) -> SandboxResult {
    user.ask("transcribe")?;
    let state = user.until("the transcript pane", |state| state["transcript"] == Value::Bool(true))?;
    let before = state["spoken"].as_u64().unwrap_or(0);
    thread::sleep(SETTLE);
    let pane = user.pane_box()?;
    let drawn_before = user.image()?;
    user.say_more(3)?;
    thread::sleep(SETTLE);
    let drawn_after = user.image()?;
    let drawn = changed_in(&drawn_before, &drawn_after, pane);
    let state = user.state()?;
    let after = state["spoken"].as_u64().unwrap_or(0);
    check("the transcript pane opens and fills", after >= before + 3, || {
        format!("spoken {before} -> {after}, {state}")
    });
    check("the transcript pane draws its new lines", drawn >= MOVED, || {
        format!("{drawn} px changed in the pane {pane:?}")
    });
    user.shot("a-transcript")?;
    // the transcription stops: its captions are asked to stop, the engine tells them off, and new ones show again
    user.ask("transcript-off")?;
    user.ask("captions-off")?;
    user.say_more(20)?;
    user.until("captions shown after the transcript", |state| {
        state["shown"] == Value::Bool(true)
    })?;
    thread::sleep(SETTLE);
    Ok(())
}

// --- pixels ---

/// The box (left, top, right, bottom) of the pixels that differ between `a` and `b` in the rows `top..bottom`.
fn changed_box(a: &RgbImage, b: &RgbImage, top: u32, bottom: u32) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in top..bottom.min(a.height()).min(b.height()) {
        for x in 0..a.width().min(b.width()) {
            if differs(a.get_pixel(x, y).0, b.get_pixel(x, y).0) {
                found = Some(found.map_or((x, y, x, y), |(l, t, r, bt)| (l.min(x), t.min(y), r.max(x), bt.max(y))));
            }
        }
    }
    found
}

fn differs(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).any(|(a, b)| a.abs_diff(b) > 24)
}

/// The count of pixels that differ between `a` and `b` in the box (left, top, right, bottom).
fn changed_in(a: &RgbImage, b: &RgbImage, (left, top, right, bottom): (u32, u32, u32, u32)) -> usize {
    let mut count = 0;
    for y in top..bottom.min(a.height()) {
        for x in left..right.min(a.width()) {
            if differs(a.get_pixel(x, y).0, b.get_pixel(x, y).0) {
                count += 1;
            }
        }
    }
    count
}

/// Whether the box's four sides are brand lines, clear of the window's frame on the left and right.
fn ring_whole(image: &RgbImage, (left, top, right, bottom): (u32, u32, u32, u32)) -> bool {
    let (middle_x, middle_y) = (left.midpoint(right), top.midpoint(bottom));
    let sides = [(left, middle_y), (right, middle_y), (middle_x, top), (middle_x, bottom)];
    left > FRAME
        && right + FRAME < image.width().saturating_sub(1)
        && sides.iter().all(|&(x, y)| brandish(image.get_pixel(x, y).0))
}

fn brandish([r, g, b]: [u8; 3]) -> bool {
    u16::from(b) > u16::from(g) + 60 && u16::from(b) > u16::from(r) + 40
}

/// The y of the focused edge's grab bar: the first row below the header with one run of brand pixels about as wide as
/// the bar (32 px) in the window's middle columns; a letter or an avatar gives no such run.
fn brand_bar(image: &RgbImage) -> Option<u32> {
    let middle = image.width() / 2;
    (HEADER + FRAME..image.height().saturating_sub(FRAME)).find(|&y| {
        let mut longest = 0;
        let mut run = 0;
        for x in middle.saturating_sub(30)..middle + 30 {
            run = if brandish(image.get_pixel(x, y).0) { run + 1 } else { 0 };
            longest = longest.max(run);
        }
        (26..=36).contains(&longest)
    })
}

// --- the user ---

struct User<'a> {
    sandbox: &'a Sandbox,
    running: Option<Running>,
    input: VirtualInput,
    asked: u32,
    letter: &'static str,
    said: u64,
}

impl<'a> User<'a> {
    fn start(sandbox: &'a Sandbox, root: &Path, letter: &'static str) -> SandboxResult<Self> {
        let root = root.to_string_lossy().into_owned();
        let spawned = sandbox.spawn(
            &std::env::current_exe()?,
            &[],
            &[(child::CHILD_ENV, letter), (child::ROOT_ENV, &root)],
        )?;
        let mut running = Running::new(spawned)?;
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
        }
        let user = Self {
            sandbox,
            running: Some(running),
            input: sandbox.input()?,
            asked: 0,
            letter,
            said: 0,
        };
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn settings_text(root: &Path) -> String {
        fs::read_to_string(child::settings_file(root)).unwrap_or_default()
    }

    fn finish(&mut self, outcome: SandboxResult) -> SandboxResult {
        if outcome.is_err() {
            let _ = self.shot("failed");
        }
        let quit = self.quit();
        outcome?;
        quit
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.running.as_ref().ok_or_else(|| "the app has quit".into())
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{}.staged", child::ASK));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(child::ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running()?
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// The account's saved height.
    fn height(&mut self) -> SandboxResult<u32> {
        let key = format!("height_{}", self.letter);
        let height = self.state()?[key.as_str()].as_u64().ok_or("no height")?;
        Ok(u32::try_from(height)?)
    }

    fn window_height(&mut self) -> SandboxResult<u32> {
        let state = self.state()?;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a window's px")]
        state["window"][1]
            .as_f64()
            .map(|height| height.round() as u32)
            .ok_or_else(|| format!("no call window: {state}").into())
    }

    fn until_window(&mut self, wanted: impl Fn(u32) -> bool) -> SandboxResult<u32> {
        let start = Instant::now();
        loop {
            let height = self.window_height()?;
            if wanted(height) {
                return Ok(height);
            }
            if start.elapsed() > WAIT {
                return Err(format!("the window stays {height} px tall").into());
            }
            thread::sleep(POLL);
        }
    }

    /// The made-up call, its first `lines` lines said, the captions shown, the call window active.
    fn open_call(&mut self, lines: u64) -> SandboxResult {
        self.ask("call")?;
        self.until("the call window", |state| state["stage"] == Value::Bool(true))?;
        self.say_more(lines)?;
        self.until("the captions", |state| {
            state["shown"] == Value::Bool(true) && state["lines"].as_u64().is_some_and(|lines| lines > 0)
        })?;
        self.ask("activate")?;
        thread::sleep(SETTLE);
        self.park()?;
        let state = self.state()?;
        note(&format!("{} call open: {state}", self.letter));
        Ok(())
    }

    fn say_more(&mut self, lines: u64) -> SandboxResult {
        let from = self.said + 1;
        self.said += lines;
        let reply = self.ask(&format!("say {from} {}", self.said))?;
        if reply != "ok" {
            return Err(format!("say: {reply}").into());
        }
        thread::sleep(STEP);
        Ok(())
    }

    /// A point over the lines: the middle of the area, left of its scroll bar.
    fn lines_point(&mut self) -> SandboxResult<(u32, u32)> {
        let window = self.window_height()?;
        let height = self.height()?.min(window);
        Ok((SCREEN.0 / 2, window - FRAME - height / 2))
    }

    /// The lines' box: the area under its header, left of its scroll bar.
    fn lines_box(&mut self) -> SandboxResult<(u32, u32, u32, u32)> {
        let window = self.window_height()?;
        let height = self.height()?.min(window);
        Ok((
            FRAME + 4,
            window - FRAME - height + 40,
            SCREEN.0 - FRAME - 24,
            window - FRAME - 4,
        ))
    }

    /// The transcript pane's box: the right column of the stage, under the window's header.
    fn pane_box(&mut self) -> SandboxResult<(u32, u32, u32, u32)> {
        let window = self.window_height()?;
        Ok((
            SCREEN.0 - FRAME - PANE_WIDTH + 4,
            HEADER + FRAME,
            SCREEN.0 - FRAME - 4,
            window - FRAME - 4,
        ))
    }

    /// How many px of the lines change when one more line is said.
    fn moved_by_new_line(&mut self) -> SandboxResult<usize> {
        let area = self.lines_box()?;
        let before = self.image()?;
        self.say_more(1)?;
        thread::sleep(SETTLE);
        let after = self.image()?;
        Ok(changed_in(&before, &after, area))
    }

    /// The y of the area's edge as drawn, found by its grab bar with the focus on it.
    fn drawn_edge(&mut self) -> SandboxResult<Option<u32>> {
        Ok(brand_bar(&self.image()?))
    }

    /// Tab from the window's start until the edge has the focus.
    fn focus_edge(&mut self, found: &Found) -> SandboxResult {
        let Some((tabs, _)) = found.edge else {
            return Err("Tab never reached the edge".into());
        };
        self.focus_by(tabs)
    }

    fn focus_list(&mut self, found: &Found) -> SandboxResult {
        let Some((tabs, _)) = found.list else {
            return Err("Tab never reached the list".into());
        };
        self.focus_by(tabs)
    }

    /// The call window activated again (its focus at its start), then `tabs` Tabs.
    fn focus_by(&mut self, tabs: usize) -> SandboxResult {
        self.ask("activate")?;
        thread::sleep(STEP);
        for _ in 0..tabs {
            self.keys(&["tab"])?;
        }
        Ok(())
    }

    fn drag_to(&mut self, x: u32, from: u32, to: u32) -> SandboxResult {
        self.input.move_to(x, from)?;
        thread::sleep(STEP);
        self.input.button_down(keys::BTN_LEFT)?;
        let steps = 20;
        for step in 1..=steps {
            let y = if to > from {
                from + (to - from) * step / steps
            } else {
                from - (from - to) * step / steps
            };
            self.input.move_to(x, y)?;
            thread::sleep(Duration::from_millis(30));
        }
        thread::sleep(STEP);
        self.input.button_up(keys::BTN_LEFT)?;
        thread::sleep(STEP);
        self.park()
    }

    fn quit(&mut self) -> SandboxResult {
        if self.running.is_none() {
            return Ok(());
        }
        self.ask("quit")?;
        let running = self.running.take().ok_or("the app has quit")?;
        let (status, stderr) = running.finish(WAIT)?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("the app exited {status}: {stderr}").into())
        }
    }

    fn keys(&mut self, specs: &[&str]) -> SandboxResult {
        for spec in specs {
            let (modifiers, key) =
                ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
            self.input.key_with(&modifiers, key)?;
            thread::sleep(STEP);
        }
        Ok(())
    }

    /// The pointer over the stage, away from the area and the header's buttons.
    fn park(&mut self) -> SandboxResult {
        self.input.move_to(SCREEN.0 / 2, HEADER + 90)
    }

    fn image(&mut self) -> SandboxResult<RgbImage> {
        let png = self.sandbox.screenshot("qa867-probe.png", false)?;
        Ok(image::open(&png)?.to_rgb8())
    }

    /// A screenshot of the screen as `qa867-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult {
        thread::sleep(SETTLE);
        let png = self.sandbox.screenshot(&format!("qa867-{name}.png"), false)?;
        Self::keep(&png, name)
    }

    fn keep(png: &Path, name: &str) -> SandboxResult {
        let file = format!("qa867-{name}.png");
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .parent()
            .map_or_else(|| PathBuf::from("target"), Path::to_owned)
            .join("ui-shots");
        fs::create_dir_all(&dir)?;
        fs::copy(png, dir.join(&file))?;
        if let Some(dir) = evidence() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(png, dir.join(&file))?;
        }
        Ok(())
    }
}

impl Drop for User<'_> {
    fn drop(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.fail(&"dropped");
        }
    }
}
