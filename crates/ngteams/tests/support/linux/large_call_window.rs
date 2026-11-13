//! Calls of ten people and more in the app's own call window, as a user meets them (task #21: "the preview ribbon
//! covers the top controls, I can't press the mic button"), in the UI sandbox, on the mock (`harness = false`: GPUI
//! runs on the process's main thread).
//!
//! The parent runs the mock chat service with its calling service, its Trouter and its group video (a mixer behind
//! each party), and one UI sandbox on a full HD screen. In it this test runs again as lab account a's app
//! (`ngteams::app::launch` over an engine on the mock with its calling, its calls' media on file devices), serving its
//! control socket (`ngteamsctl`'s) and its view trace. The mock rings a in a group call b starts in the a-b-c chat;
//! a answers over the control socket; the mock's roster then lists made-up people joining (no client of their own,
//! cameras off), one of them sharing a screen, some leaving. The call window is sized by the compositor (560×400 to
//! 1920×1080), and the sandbox's own pointer and keyboard drive it. The app tells no positions, so what the pointer
//! clicks is found as a user's eye finds it, from screenshots (`call_pixels`): a toolbar button where the window
//! changed when its state changed (Mic, Camera, Raise), the rest by their place among the white icons in Teams' order
//! beside those, Leave by its red; the stage's tiles, the account's own white tile and the pager `< n/N >` from the
//! stage's panel. What a click did is read from the control socket (muted, camera), the mock (the hand's
//! `publishState`, the leave) and the view trace (`call asked`).
//!
//! - In a call of 10, 25 and 50 people, in the gallery, the speaker view and beside a share, at 560×400, 800×560,
//!   1280×800 and 1920×1080: Mic is found and its click mutes and unmutes; every tile lies on the stage's panel.
//! - With 50 people, at 560×400 and 1920×1080, in each view, every toolbar button takes its click: Chat and People open
//!   and close their pane, Raise raises and lowers the hand, React, View and More open their menus, Camera turns on and
//!   off, Mic mutes; Share's menu asks to share; Leave ends the call.
//! - The gallery's pages: equal 16:9 tiles at least 160 px wide, the account's own tile last on each, everyone once
//!   across the pages, `<` doing nothing on the first page and `>` nothing on the last; View > Max gallery size 4, 9,
//!   16 and 49 setting how many a page shows, back on the first page; someone joining and leaving while the last page
//!   shows; the window shrunk while the third page shows.
//! - The share strip and the speaker strip page with their own pagers, the account's own tile last in the share strip,
//!   the speaker staying large; a call of 2-9 shows everyone on one page, no pager; in a 1:1 call the account's own
//!   tile floats at the stage's bottom right.
//! - Each ring's banner on the sandbox's bus carries the caller's (b's) picture, round, as the mock's Graph gives it
//!   (#114).
//! - Screenshots of each under `target/ui-shots/large-call-*.png` (and `$LARGE_CALL_EVIDENCE`, if set), in Teams Light
//!   and Dark.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, WindowingModes};
use image::RgbImage;
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::web_client_info;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_ctl::protocol::{CallRow, Request, Response, Toggle};
use teams_ctl::{Client, Timeouts};
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{AB_CHAT, ABC_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, TENANT};
use teams_proto::Mri;
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::settings::Settings;
use teams_ui::{SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys, parse_key};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};
#[path = "large_call_meet_now.rs"]
mod meet_now;
#[path = "call_pixels.rs"]
mod pixels;
use pixels::{Rect, Stage};

const CHILD_ENV: &str = "NGTEAMS_LARGE_CALL_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_LARGE_CALL_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_LARGE_CALL_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_LARGE_CALL_TROUTER";
const RELAY_ENV: &str = "NGTEAMS_LARGE_CALL_RELAY";
const IC3_ENV: &str = "NGTEAMS_LARGE_CALL_IC3";
const SKYPE_ENV: &str = "NGTEAMS_LARGE_CALL_SKYPE";
const DIR_ENV: &str = "NGTEAMS_LARGE_CALL_DIR";
const CTL_ENV: &str = "NGTEAMS_LARGE_CALL_CTL";
/// Where the screenshots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "LARGE_CALL_EVIDENCE";
/// The steps to run, by name, comma-separated; all of them when unset.
const STEPS_ENV: &str = "LARGE_CALL_STEPS";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
/// The file in the sandbox dir the parent writes a theme's key into to have the settings take it, as the
/// appearance page's tiles do.
const THEME_TRIGGER: &str = "theme";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(120);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// How long the window gets to draw what a click, a key or the mock changed.
const SETTLE: Duration = Duration::from_millis(700);
/// niri's layout: each window takes the whole screen until the test sizes it (`NIRI_FULL_WINDOW`'s), and no hot
/// corner opens the overview under the pointer.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
/// The screen: full HD.
const SCREEN: (u32, u32) = (1920, 1080);
/// The window sizes looked at: the call window's smallest, a small one, a laptop's, a full HD screen's.
const SIZES: [(i32, i32); 4] = [(560, 400), (800, 560), (1280, 800), (1920, 1080)];
/// The call window's toolbar lies within this many px of its top.
const TOOLBAR_BAND: i32 = 58;
/// b's picture as the mock's Graph gives it (#114): 96×96, one colour.
const B_PICTURE: [u8; 3] = [0xd0, 0x40, 0x90];
/// The side of Graph's picture and of a banner's.
const PICTURE_SIDE: u32 = 96;
/// The mock's peers' tone.
const PEER_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("large call window test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams large calls", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), PEER_TONE)?;
    runtime.block_on(mock.serve_bytes(&b_picture_path(), "image/png", b_picture()?));
    let mut app = AppUnderTest::start(&sandbox, &mock, &relay.to_string(), trouter.origin(), dir.path())?;
    // the chat list shows b: the app reads b's picture and keeps it, as the call banner then shows it
    let read = wait_until(WAIT, || {
        let requests = runtime.block_on(mock.received_requests());
        requests
            .iter()
            .any(|request| request.url.path() == b_picture_path())
            .then_some(())
    });
    if read.is_none() {
        return Err(format!("b's picture never read{}", sandbox.logs()).into());
    }
    thread::sleep(Duration::from_secs(1));
    let outcome = Rig::new(&sandbox, &mock, &mut app).and_then(|mut rig| scenarios(&mut rig));
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- the app: {}\n--- the sandbox{}",
            app.running.fail(&"killed"),
            sandbox.logs()
        )
    });
    outcome?;
    app.quit(&sandbox)?;
    drop(trouter);
    say("ngteams large call window: ok");
    Ok(())
}

/// Graph's path of b's picture.
fn b_picture_path() -> String {
    format!(
        "/v1.0/users/{}/photos/96x96/$value",
        LAB_B.trim_start_matches("8:orgid:")
    )
}

/// b's picture: a PNG of [`B_PICTURE`], [`PICTURE_SIDE`] square.
fn b_picture() -> SandboxResult<Vec<u8>> {
    let [r, g, b] = B_PICTURE;
    let picture = image::RgbaImage::from_pixel(PICTURE_SIDE, PICTURE_SIDE, image::Rgba([r, g, b, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(picture).write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// Everything the user does, in order.
fn scenarios(rig: &mut Rig<'_>) -> SandboxResult {
    type Step = fn(&mut Rig<'_>) -> SandboxResult;
    let steps: [(&str, Step); 12] = [
        ("small", small_calls),
        ("mic-10", |rig| every_size(rig, 10)),
        ("mic-25", |rig| every_size(rig, 25)),
        ("sizes", gallery_sizes),
        ("last-page", last_page),
        ("strips", strips),
        ("mic-50", |rig| every_size(rig, 50)),
        ("page-three", resized_on_page_three),
        ("themes", themes),
        ("toolbar", whole_toolbar),
        ("leave", share_and_leave),
        ("one-to-one", one_to_one),
    ];
    // a run of some steps alone (`LARGE_CALL_STEPS=sizes,strips`), each setting up the call it needs
    let only = std::env::var(STEPS_ENV).ok();
    let wanted = |name: &str| {
        only.as_deref()
            .is_none_or(|only| only.split(',').any(|step| step == name))
    };
    // before b's group call, as it joins a call of its own (#125)
    if wanted("meet-now") {
        say("step meet-now");
        meet_now::meet_now(rig)?;
        if only.as_deref() == Some("meet-now") {
            return Ok(());
        }
    }
    rig.group_call()?;
    for (name, step) in steps {
        if only
            .as_deref()
            .is_none_or(|only| only.split(',').any(|wanted| wanted == name))
        {
            say(&format!("step {name}"));
            step(rig)?;
        }
    }
    Ok(())
}

/// 2 to 9 people: everyone on one page, the account's own tile last, no pager, at every size.
fn small_calls(rig: &mut Rig<'_>) -> SandboxResult {
    for others in [3, 8] {
        rig.join(others)?;
        for size in SIZES {
            rig.resize(size)?;
            let stage = rig.stage()?;
            let case = format!("{} people at {size:?}", others + 1);
            gallery_page(&stage, &case)?;
            check(
                stage.tiles.len() == others + 1,
                &case,
                "everyone shows on one page",
                &stage,
            )?;
            check(stage.pager.is_none(), &case, "no pager", &stage)?;
        }
    }
    rig.resize((1280, 800))?;
    rig.save("large-call-gallery-9-laptop")
}

/// In each view at each size, Mic is found and takes its click, and every tile lies on the stage.
fn every_size(rig: &mut Rig<'_>, people: usize) -> SandboxResult {
    rig.join(people - 1)?;
    for view in [View::Gallery, View::Speaker, View::Share] {
        rig.show(view)?;
        for size in SIZES {
            rig.resize(size)?;
            let case = format!("{view:?} of {people} at {size:?}");
            let stage = rig.stage()?;
            check(!stage.tiles.is_empty(), &case, "tiles show", &stage)?;
            let mic = rig.locate_mic(&case)?;
            for tile in &stage.tiles {
                check(tile.y >= mic.bottom(), &case, "no tile reaches the toolbar", &stage)?;
            }
            rig.click_mic(mic, &case)?;
            if people == 50 || (people == 25 && size == (1280, 800)) {
                rig.save(&format!("large-call-{}-{people}-{}x{}", view.name(), size.0, size.1))?;
            }
        }
    }
    rig.show(View::Gallery)
}

/// A gallery page as Teams draws one: equal 16:9 tiles at least 160 px wide, the account's own tile last.
fn gallery_page(stage: &Stage, case: &str) -> SandboxResult {
    let order = stage.reading_order();
    let own = stage
        .own
        .ok_or_else(|| format!("{case}: the account's own tile isn't drawn: {stage:?}"))?;
    let last = order.last().copied().ok_or_else(|| format!("{case}: no tiles"))?;
    check(
        last.holds(own, 3),
        case,
        "the account's own tile is the page's last",
        stage,
    )?;
    let first = order[0];
    for tile in &order {
        check(
            (tile.w - first.w).abs() <= 3 && (tile.h - first.h).abs() <= 3,
            case,
            "the tiles are equal",
            stage,
        )?;
        let ratio = f64::from(tile.w) / f64::from(tile.h);
        check((ratio - 16.0 / 9.0).abs() < 0.08, case, "the tiles are 16:9", stage)?;
        check(tile.w >= 156, case, "the tiles are at least 160 px wide", stage)?;
        check(stage.panel.holds(*tile, 0), case, "the tiles lie on the stage", stage)?;
    }
    Ok(())
}

/// Every page of the gallery (the first shown): each a gallery page, the pager's ends doing nothing; how many others
/// the pages showed in all, and each page's tile count.
fn walk_pages(rig: &mut Rig<'_>, case: &str) -> SandboxResult<Vec<usize>> {
    let mut counts = Vec::new();
    let mut stage = rig.stage()?;
    if let Some(pager) = stage.pager {
        // `<` on the first page: nothing changes
        let before = rig.shot()?;
        rig.click(pager.previous)?;
        rig.unchanged(&before, stage.panel, &format!("{case}: < on the first page"))?;
    }
    loop {
        gallery_page(&stage, &format!("{case}, page {}", counts.len() + 1))?;
        counts.push(stage.tiles.len());
        let Some(pager) = stage.pager else { break };
        let before = rig.shot()?;
        rig.click(pager.next)?;
        let after = rig.shot()?;
        if pixels::changed_pixels(&before, &after, stage.panel) < 40 {
            break;
        }
        stage = rig.stage()?;
        check(counts.len() < 40, case, "the pages end", &stage)?;
    }
    Ok(counts)
}

/// The first page of the gallery again, by `<`.
fn first_page(rig: &mut Rig<'_>) -> SandboxResult {
    for _ in 0..40 {
        let stage = rig.stage()?;
        let Some(pager) = stage.pager else { return Ok(()) };
        let before = rig.shot()?;
        rig.click(pager.previous)?;
        if pixels::changed_pixels(&before, &rig.shot()?, stage.panel) < 40 {
            return Ok(());
        }
    }
    Err("the first page didn't come".into())
}

/// View > Max gallery size, 4, 9, 16 and 49 at full HD with 25 people: a page shows that many (the account's own
/// among them), everyone once across the pages; each choice back on the first page.
fn gallery_sizes(rig: &mut Rig<'_>) -> SandboxResult {
    rig.join(24)?;
    rig.show(View::Gallery)?;
    rig.resize((1920, 1080))?;
    let others = rig.people.len();
    for size in [4_usize, 9, 16, 49] {
        // from the third page (or the last), as the user may be anywhere
        for _ in 0..2 {
            if let Some(pager) = rig.stage()?.pager {
                rig.click(pager.next)?;
            }
        }
        // the pointer picks the larger sizes, the keyboard the smaller, as Teams takes both
        if size <= 9 {
            rig.gallery_size_by_keys(size, others)?;
        } else {
            rig.gallery_size(size, others)?;
        }
        let case = format!("max gallery size {size} with {}", others + 1);
        let stage = rig.stage()?;
        if let Some(pager) = stage.pager {
            let before = rig.shot()?;
            rig.click(pager.previous)?;
            rig.unchanged(&before, stage.panel, &format!("{case}: back on the first page"))?;
        }
        let pages = walk_pages(rig, &case)?;
        let shown: usize = pages.iter().map(|tiles| tiles - 1).sum();
        check_pages(&pages, others, size, &case)?;
        if shown != others {
            return Err(format!("{case}: the pages showed {shown} others of {others}: {pages:?}").into());
        }
        if size == 9 {
            rig.save("large-call-gallery-25-size-9-last-page")?;
        }
        first_page(rig)?;
        rig.view_menu_open_on_sizes()?;
        rig.save(&format!("large-call-view-max-gallery-size-{size}-menu"))?;
        rig.key("escape")?;
        thread::sleep(SETTLE);
    }
    Ok(())
}

fn check_pages(pages: &[usize], others: usize, size: usize, case: &str) -> SandboxResult {
    let full = size.min(others + 1);
    if pages.first() != Some(&full) || pages.iter().any(|tiles| *tiles > size) {
        return Err(format!("{case}: pages of {pages:?}, wanted {full} on the first and no more than {size}").into());
    }
    Ok(())
}

/// Someone joins and leaves while the last page shows (Max gallery size 9, 25 others: pages of 8, 8, 8 and 1).
fn last_page(rig: &mut Rig<'_>) -> SandboxResult {
    rig.resize((1280, 800))?;
    rig.join(25)?;
    rig.gallery_size(9, 25)?;
    let tiles = walk_pages(rig, "25 others at size 9")?;
    if tiles != [9, 9, 9, 2] {
        return Err(format!("25 others at size 9: pages of {tiles:?}").into());
    }
    rig.save("large-call-gallery-last-page")?;
    // the last page's one other leaves: the page before shows, nothing after it
    rig.leave_last()?;
    let stage = rig.stage()?;
    gallery_page(&stage, "the last page's one left")?;
    check(
        stage.tiles.len() == 9,
        "the last page's one left",
        "the third page shows",
        &stage,
    )?;
    let pager = stage.pager.ok_or("the last page's one left: no pager")?;
    let before = rig.shot()?;
    rig.click(pager.next)?;
    rig.unchanged(&before, stage.panel, "the last page's one left: > on the last page")?;
    // someone joins while the last page shows: the page stays, > shows them on their own page
    rig.join(25)?;
    let stage = rig.stage()?;
    check(stage.tiles.len() == 9, "one joined", "the third page stays", &stage)?;
    let pager = stage.pager.ok_or("one joined: no pager")?;
    rig.click(pager.next)?;
    let stage = rig.stage()?;
    gallery_page(&stage, "one joined, the next page")?;
    check(
        stage.tiles.len() == 2,
        "one joined",
        "the new page holds them and the account",
        &stage,
    )?;
    first_page(rig)?;
    rig.leave_last()?;
    rig.gallery_size(49, 24)
}

/// The share strip and the speaker strip page, each with its own pager.
fn strips(rig: &mut Rig<'_>) -> SandboxResult {
    share_strip(rig)?;
    speaker_strip(rig)
}

/// The strip beside a share: everyone once across its pages, the account's own tile last on each.
fn share_strip(rig: &mut Rig<'_>) -> SandboxResult {
    rig.join(24)?;
    rig.show(View::Share)?;
    for size in [(800, 560), (1920, 1080), (1280, 800)] {
        rig.resize(size)?;
        share_named_on_picture(rig, size)?;
    }
    // the strip: the column the account's own tile stands in, right of the shared screen (which plays on)
    let first = rig.stage_with_own()?;
    let own = first
        .own
        .ok_or_else(|| format!("the share strip: no own tile: {first:?}"))?;
    let column = Rect {
        x: own.x - 6,
        w: first.panel.right() - own.x + 6,
        ..first.panel
    };
    let mut shown = 0;
    let mut turned = 0;
    loop {
        let stage = rig.stage_with_own()?;
        let strip: Vec<Rect> = stage
            .reading_order()
            .into_iter()
            .filter(|tile| column.holds(*tile, 0))
            .collect();
        let case = format!("share strip page {}", turned + 1);
        rig.debug(&case)?;
        let Some(own) = stage.own else {
            rig.save("failed-share-strip")?;
            return Err(format!("{case}: no own tile: {stage:?}").into());
        };
        check(
            strip.last().is_some_and(|last| last.holds(own, 3)),
            &case,
            "the account's own tile is last",
            &stage,
        )?;
        shown += strip.len() - 1;
        turned += 1;
        let pager = stage.pager.ok_or_else(|| format!("{case}: no pager"))?;
        check(
            column.holds(pager.previous, 0),
            &case,
            "the pager is the strip's",
            &stage,
        )?;
        if turned == 1 {
            rig.save("large-call-share-strip-first-page")?;
        }
        let before = rig.shot()?;
        rig.click(pager.next)?;
        // the next page, once the strip redraws (a busy app may take a while); the last stays as it was
        let after = rig.shot_until(|after| pixels::changed_pixels(&before, after, column) >= 40)?;
        if pixels::changed_pixels(&before, &after, column) < 40 || turned > 30 {
            break;
        }
    }
    let others = rig.people.len();
    if shown != others {
        return Err(format!("the share strip's {turned} pages showed {shown} others of {others}").into());
    }
    Ok(())
}

/// The sharer's name sits on the shared picture's bottom left, as Teams places it, not under the picture on the stage.
fn share_named_on_picture(rig: &mut Rig<'_>, (width, height): (i32, i32)) -> SandboxResult {
    let case = format!("the share at {width}x{height}");
    let deadline = Instant::now() + WAIT;
    rig.wait_share()?;
    // The moving white bar can merge with the name for a frame.
    loop {
        let shot = rig.shot()?;
        let stage = pixels::stage(&shot, rig.window()?).ok_or_else(|| format!("{case}: no stage"))?;
        let own = stage.own.ok_or_else(|| format!("{case}: no own tile: {stage:?}"))?;
        let room = Rect {
            w: own.x - 6 - stage.panel.x,
            ..stage.panel
        };
        let picture = pixels::dark_bounds(&shot, room).ok_or_else(|| format!("{case}: no shared picture"))?;
        let under = Rect {
            y: picture.bottom() + 2,
            h: (room.bottom() - picture.bottom() - 2).max(0),
            ..room
        };
        if !pixels::labels(&shot, under).is_empty() {
            rig.save("failed-share-name")?;
            return Err(format!("{case}: the sharer's name sits under the picture {picture:?}").into());
        }
        let corner = Rect {
            y: picture.bottom() - 40,
            w: 260,
            h: 40,
            ..picture
        };
        let on_corner = pixels::labels(&shot, corner).into_iter().any(|label| {
            (4..=14).contains(&(label.x - picture.x)) && (4..=14).contains(&(picture.bottom() - label.bottom()))
        });
        if on_corner {
            rig.save(&format!("large-call-share-name-{width}x{height}"))?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    rig.save("failed-share-name")?;
    Err(format!("{case}: no name on the shared picture's bottom left").into())
}

/// The strip under the speaker turns its pages; the speaker stays large.
fn speaker_strip(rig: &mut Rig<'_>) -> SandboxResult {
    rig.show(View::Speaker)?;
    rig.save("large-call-speaker-strip-first-page")?;
    let stage = rig.stage()?;
    // the speaker: the stage above its strip (the account's own tile floats over its corner)
    let strip_top = stage
        .tiles
        .iter()
        .filter(|tile| tile.h < stage.panel.h / 3)
        .map(|tile| tile.y)
        .min()
        .ok_or_else(|| format!("no speaker strip: {stage:?}"))?;
    let speaker = Rect {
        h: strip_top - stage.panel.y - 6,
        ..stage.panel
    };
    let pager = stage
        .pager
        .ok_or_else(|| format!("the speaker strip has no pager: {stage:?}"))?;
    check(
        pager.previous.y > speaker.bottom(),
        "speaker strip",
        "its pager is under the speaker",
        &stage,
    )?;
    let before = rig.shot()?;
    rig.click(pager.next)?;
    let after = rig.shot()?;
    let strip = Rect {
        x: stage.panel.x,
        y: speaker.bottom() + 2,
        w: stage.panel.w,
        h: stage.panel.bottom() - speaker.bottom() - 2,
    };
    if pixels::changed_pixels(&before, &after, strip) < 200 {
        return Err("the speaker strip's > turned no page".into());
    }
    rig.unchanged(&before, speaker, "the speaker stays large while its strip pages")?;
    rig.save("large-call-speaker-strip-second-page")?;
    rig.show(View::Gallery)
}

/// The window shrinks while the third page shows: a page with people on it still shows, and the pager turns.
fn resized_on_page_three(rig: &mut Rig<'_>) -> SandboxResult {
    rig.join(49)?;
    rig.show(View::Gallery)?;
    rig.resize((1920, 1080))?;
    rig.gallery_size(16, rig.people.len())?;
    for _ in 0..2 {
        let pager = rig.stage()?.pager.ok_or("50 at size 16: no pager")?;
        rig.click(pager.next)?;
    }
    rig.save("large-call-gallery-page-3-full")?;
    rig.resize((800, 560))?;
    let stage = rig.stage()?;
    gallery_page(&stage, "page 3 shrunk to 800×560")?;
    check(stage.tiles.len() >= 2, "page 3 shrunk", "people show", &stage)?;
    let pager = stage.pager.ok_or("page 3 shrunk: no pager")?;
    let before = rig.shot()?;
    rig.click(pager.previous)?;
    if pixels::changed_pixels(&before, &rig.shot()?, stage.panel) < 40 {
        return Err("page 3 shrunk: < turned no page".into());
    }
    rig.save("large-call-gallery-page-3-shrunk")?;
    first_page(rig)?;
    rig.resize((1920, 1080))?;
    rig.gallery_size(49, rig.people.len())
}

/// The views in Teams Dark, then back in Teams Light.
fn themes(rig: &mut Rig<'_>) -> SandboxResult {
    rig.join(49)?;
    rig.resize((1280, 800))?;
    rig.theme(teams_ui::theme::DARK_KEY)?;
    for view in [View::Gallery, View::Speaker, View::Share] {
        rig.show(view)?;
        rig.save(&format!("large-call-{}-50-dark", view.name()))?;
    }
    // View on Max gallery size, the keyboard in its sizes (dark on dark: no submenu found by its change)
    rig.show(View::Gallery)?;
    rig.open_view_menu()?;
    for key in ["down", "down", "down", "down", "right"] {
        rig.key(key)?;
    }
    thread::sleep(SETTLE);
    rig.save("large-call-view-max-gallery-size-menu-dark")?;
    rig.key("escape")?;
    rig.key("escape")?;
    thread::sleep(SETTLE);
    rig.theme(teams_ui::theme::LIGHT_KEY)?;
    rig.show(View::Gallery)
}

/// Every toolbar button takes its click, in each view, at the smallest and the largest window.
fn whole_toolbar(rig: &mut Rig<'_>) -> SandboxResult {
    rig.join(49)?;
    for size in [(560, 400), (1920, 1080)] {
        for view in [View::Gallery, View::Speaker, View::Share] {
            rig.show(view)?;
            rig.resize(size)?;
            let case = format!("{view:?} of 50 at {size:?}");
            let bar = rig.toolbar(&case)?;
            rig.toolbar_acts(&bar, &case)?;
        }
    }
    Ok(())
}

/// Share asks to share, then Leave leaves the call.
fn share_and_leave(rig: &mut Rig<'_>) -> SandboxResult {
    rig.join(49)?;
    rig.show(View::Gallery)?;
    rig.resize((1280, 800))?;
    let bar = rig.toolbar("before Leave")?;
    rig.share_asks(&bar)?;
    rig.leave(bar.leave)
}

/// A 1:1 call: the account's own tile floats at the stage's bottom right, over the far end's.
fn one_to_one(rig: &mut Rig<'_>) -> SandboxResult {
    let shown = rig.banners_shown();
    let _ = rig.mock.ring(LAB_B, LAB_A);
    rig.answer(shown)?;
    for size in [(800, 560), (1920, 1080)] {
        rig.resize(size)?;
        let stage = rig.stage()?;
        let case = format!("1:1 call at {size:?}");
        let own = stage.own.ok_or_else(|| format!("{case}: no own tile: {stage:?}"))?;
        check(
            stage.panel.right() - own.right() <= 40 && stage.panel.bottom() - own.bottom() <= 40,
            &case,
            "the account's own tile floats at the bottom right",
            &stage,
        )?;
        check(stage.pager.is_none(), &case, "no pager", &stage)?;
        rig.save(&format!("large-call-one-to-one-{}x{}", size.0, size.1))?;
    }
    let leave = rig.leave_button("1:1 call")?;
    rig.leave(leave)
}

fn check(holds: bool, case: &str, what: &str, stage: &Stage) -> SandboxResult {
    if holds {
        Ok(())
    } else {
        Err(format!("{case}: not so: {what}; the stage read {stage:?}").into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Gallery,
    Speaker,
    Share,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            Self::Gallery => "gallery",
            Self::Speaker => "speaker",
            Self::Share => "share",
        }
    }
}

/// The call window's toolbar buttons, where the pointer finds them.
#[derive(Clone, Copy, Debug)]
struct Toolbar {
    chat: Rect,
    people: Rect,
    raise: Rect,
    react: Rect,
    view: Rect,
    more: Rect,
    camera: Rect,
    mic: Rect,
    share: Rect,
    leave: Rect,
}

/// The user at the sandbox: its screen, pointer and keyboard, the app's control socket, and the mock's call.
struct Rig<'a> {
    sandbox: &'a Sandbox,
    mock: &'a ChatServiceMock,
    app: &'a mut AppUnderTest,
    input: VirtualInput,
    ctl: Client,
    /// The mock's group call.
    call: String,
    /// The made-up people in it, in the order they joined.
    people: Vec<String>,
    /// Whether the mock's made-up sharer shares.
    sharing: bool,
    /// The call window on the screen.
    frame: Option<Rect>,
    /// niri's id of the call window.
    window_id: Option<u64>,
    shots: usize,
}

impl<'a> Rig<'a> {
    fn new(sandbox: &'a Sandbox, mock: &'a ChatServiceMock, app: &'a mut AppUnderTest) -> SandboxResult<Self> {
        let socket = app.socket.clone();
        let ctl = wait_until(WAIT, || Client::connect(&socket, Timeouts::default()).ok())
            .ok_or_else(|| format!("no control socket at {}", socket.display()))?;
        Ok(Self {
            sandbox,
            mock,
            app,
            input: sandbox.input()?,
            ctl,
            call: String::new(),
            people: Vec::new(),
            sharing: false,
            frame: None,
            window_id: None,
            shots: 0,
        })
    }

    fn calls(&mut self) -> SandboxResult<Vec<CallRow>> {
        match self.ctl.request(None, Request::Calls)? {
            Response::Calls { calls } => Ok(calls),
            other => Err(format!("calls: {other:?}").into()),
        }
    }

    /// Waits until the call the app is in (or rings with) satisfies `wanted`.
    fn wait_call(&mut self, what: &str, wanted: impl Fn(&CallRow) -> bool) -> SandboxResult<CallRow> {
        let deadline = Instant::now() + WAIT;
        loop {
            let calls = self.calls()?;
            if let Some(row) = calls.iter().find(|row| wanted(row)) {
                return Ok(row.clone());
            }
            if Instant::now() > deadline {
                return Err(format!("{what}: the calls are {calls:?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// b rings a in the a-b-c chat's group call; a answers.
    fn group_call(&mut self) -> SandboxResult {
        let shown = self.banners_shown();
        self.call = self.mock.ring_group(LAB_B, ABC_CHAT, &[LAB_A]);
        self.answer(shown)
    }

    /// How many banners the sandbox's bus has shown so far (closed ones too).
    fn banners_shown(&self) -> usize {
        self.sandbox.bus().notifications().len()
    }

    /// Answers the ringing call over the control socket, as `ngteamsctl answer` does; the call window shows it. Its
    /// banner is one shown after the first `shown`.
    fn answer(&mut self, shown: usize) -> SandboxResult {
        let opened = |seen: &[serde_json::Value]| {
            seen.iter()
                .filter(|line| line["view"] == "call-window" && line["change"] == "opened")
                .count()
        };
        let before = opened(self.app.trace.catch_up());
        self.wait_call("ringing", |row| row.ringing)?;
        self.caller_picture(shown)?;
        self.window_id = None;
        self.ctl.request(None, Request::Answer)?;
        self.wait_call("connected", |row| row.state == "connected")?;
        self.app
            .trace
            .wait_until("the call window opened", |seen| opened(seen) > before)?;
        self.wait_window(|size| size.0 == i32_of(SCREEN.0))?;
        self.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The ringing call's banner on the sandbox's bus (the newest with Answer after the first `shown`) carries b's
    /// picture, round (#114).
    fn caller_picture(&mut self, shown: usize) -> SandboxResult {
        let banner = wait_until(WAIT, || {
            self.sandbox
                .bus()
                .notifications()
                .into_iter()
                .skip(shown)
                .rfind(|banner| banner.actions.iter().any(|action| action == "answer"))
        })
        .ok_or("no incoming call's banner")?;
        let (width, height, pixels) = banner.image.as_ref().ok_or_else(|| {
            format!(
                "no picture on the call's banner: {:?} (hints {:?})",
                banner.summary, banner.hints
            )
        })?;
        let side = i32::try_from(PICTURE_SIDE)?;
        if (*width, *height) != (side, side) || !banner.hints.iter().any(|hint| hint == "image-data") {
            return Err(format!(
                "the call banner's picture is {width}×{height}, hints {:?}",
                banner.hints
            )
            .into());
        }
        let at = |x: u32, y: u32| {
            let start = usize::try_from((y * PICTURE_SIDE + x) * 4).unwrap_or_default();
            pixels.get(start..start + 4).map(<[u8]>::to_vec).unwrap_or_default()
        };
        let last = PICTURE_SIDE - 1;
        let corners = [at(0, 0), at(last, 0), at(0, last), at(last, last)];
        let middle = at(PICTURE_SIDE / 2, PICTURE_SIDE / 2);
        let b_colour = middle.len() == 4
            && middle[3] == 255
            && middle
                .iter()
                .zip(B_PICTURE)
                .all(|(got, wanted)| got.abs_diff(wanted) <= 8);
        say(&format!(
            "the call banner {:?}: {:?}: picture {width}×{height}, middle {middle:?}, corners {corners:?}",
            banner.summary, banner.body
        ));
        if !b_colour || corners.iter().any(|corner| corner.get(3) != Some(&0)) {
            return Err(format!("not b's round picture: middle {middle:?}, corners {corners:?}").into());
        }
        self.shots += 1;
        let picture = image::RgbaImage::from_raw(PICTURE_SIDE, PICTURE_SIDE, pixels.clone())
            .ok_or("the picture's bytes don't fill it")?;
        let file = format!("large-call-banner-picture-{}.png", self.shots);
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir);
            fs::create_dir_all(&dir)?;
            picture.save(dir.join(&file))?;
        }
        Ok(())
    }

    /// Made-up people join until `others` besides b are in the call, or leave from the last joined.
    fn join(&mut self, others: usize) -> SandboxResult {
        while self.people.len() > others {
            self.leave_last()?;
        }
        let new: Vec<(String, String)> = (self.people.len()..others)
            .map(|index| (made_up_mri(index), made_up_name(index)))
            .collect();
        if !new.is_empty() {
            let people: Vec<(&str, &str)> = new.iter().map(|(mri, name)| (mri.as_str(), name.as_str())).collect();
            if !self.mock.join_made_up(&self.call, &people) {
                return Err("the mock's call took nobody".into());
            }
            self.people.extend(new.into_iter().map(|(mri, _)| mri));
        }
        let total = others;
        self.app
            .trace
            .wait_until(&format!("{total} others in the grid"), |seen| {
                grid_size(seen) == Some(total)
            })?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn leave_last(&mut self) -> SandboxResult {
        let mri = self.people.pop().ok_or("nobody to leave")?;
        if !self.mock.leave_made_up(&self.call, &mri) {
            return Err(format!("{mri} wasn't in the call").into());
        }
        let total = self.people.len();
        self.app
            .trace
            .wait_until(&format!("{total} others in the grid"), |seen| {
                grid_size(seen) == Some(total)
            })?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The call window where it is on the screen, as last found.
    fn window(&self) -> SandboxResult<Rect> {
        self.frame.ok_or_else(|| "the call window wasn't found".into())
    }

    /// The size niri gives the window with the keyboard (the call window).
    #[expect(
        clippy::cast_possible_truncation,
        reason = "niri's logical px are whole numbers on a scale of 1"
    )]
    fn window_size(&mut self) -> SandboxResult<(i32, i32)> {
        let text = self.sandbox.niri_msg(&["-j", "windows"])?;
        let windows: serde_json::Value = serde_json::from_str(&text)?;
        let list = windows.as_array().ok_or_else(|| format!("no windows: {text}"))?;
        let window = match self.window_id {
            Some(id) => list.iter().find(|window| window["id"].as_u64() == Some(id)),
            None => list.iter().find(|window| window["is_focused"] == true),
        }
        .ok_or_else(|| format!("no call window: {text}"))?;
        self.window_id = window["id"].as_u64();
        let size = &window["layout"]["window_size"];
        let at = |index: usize| {
            size[index]
                .as_f64()
                .map(|value| value.round() as i32)
                .ok_or_else(|| format!("no window size: {text}"))
        };
        Ok((at(0)?, at(1)?))
    }

    /// Waits for the call window to take `wanted`'s size, then finds it on the screen: where niri's shot of the window
    /// alone matches the screen's.
    fn wait_window(&mut self, wanted: impl Fn((i32, i32)) -> bool) -> SandboxResult<Rect> {
        let deadline = Instant::now() + WAIT;
        loop {
            let size = self.window_size();
            if size.as_ref().is_ok_and(|size| wanted(*size)) {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("the window didn't take the size wanted: {size:?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.focus()?;
        thread::sleep(SETTLE);
        self.shots += 1;
        let name = format!("window-{}.png", self.shots);
        let png = self.sandbox.screenshot(&name, true)?;
        let alone = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        let screen = self.shot()?;
        let Some(frame) = pixels::find(&screen, &alone) else {
            if let Some(dir) = std::env::var_os(EVIDENCE_ENV).map(PathBuf::from) {
                let _ = screen.save(dir.join("lost-screen.png"));
                let _ = alone.save(dir.join("lost-window.png"));
            }
            return Err(format!(
                "the call window ({}×{}) isn't on the screen",
                alone.width(),
                alone.height()
            )
            .into());
        };
        self.frame = Some(frame);
        Ok(frame)
    }

    /// The compositor sizes the call window to `width`×`height`, as a user's drag does.
    fn resize(&mut self, (width, height): (i32, i32)) -> SandboxResult {
        self.focus()?;
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        self.sandbox
            .niri_msg(&["action", "set-window-height", &height.to_string()])?;
        self.wait_window(|size| size == (width, height))?;
        thread::sleep(SETTLE);
        self.park()?;
        if self.sharing {
            self.wait_share()?;
        }
        Ok(())
    }

    /// The call window takes the keyboard, as a click on it gives it.
    fn focus(&mut self) -> SandboxResult {
        if let Some(id) = self.window_id {
            self.sandbox
                .niri_msg(&["action", "focus-window", "--id", &id.to_string()])?;
        }
        Ok(())
    }

    /// The whole screen as it is now.
    fn shot(&mut self) -> SandboxResult<RgbImage> {
        self.shots += 1;
        let name = format!("screen-{}.png", self.shots);
        let png = self.sandbox.screenshot(&name, false)?;
        let image = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        Ok(image)
    }

    fn stage(&mut self) -> SandboxResult<Stage> {
        self.park()?;
        let window = self.window()?;
        let shot = self.shot()?;
        pixels::stage(&shot, window).ok_or_else(|| format!("no stage in the window {window:?}").into())
    }

    /// The stage once the account's camera-off own tile is drawn in full after a strip redraw or camera toggle.
    fn stage_with_own(&mut self) -> SandboxResult<Stage> {
        self.park()?;
        let window = self.window()?;
        let shot = self.shot_until(|shot| pixels::stage(shot, window).is_some_and(|stage| stage.own.is_some()))?;
        pixels::stage(&shot, window)
            .filter(|stage| stage.own.is_some())
            .ok_or_else(|| "the account's camera-off own tile wasn't drawn".into())
    }

    /// Saves the call window as it is now as `<name>.png` under `target/ui-shots/` and the evidence directory.
    fn save(&mut self, name: &str) -> SandboxResult {
        self.park()?;
        let window = self.window()?;
        let shot = self.shot()?;
        let cut = pixels::crop(&shot, window);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .parent()
            .map_or_else(|| PathBuf::from("target"), Path::to_owned)
            .join("ui-shots");
        let mut dirs = vec![dir];
        dirs.extend(std::env::var_os(EVIDENCE_ENV).map(PathBuf::from));
        for dir in dirs {
            fs::create_dir_all(&dir)?;
            cut.save(dir.join(format!("{name}.png")))?;
        }
        Ok(())
    }

    /// With `LARGE_CALL_DEBUG` set, saves the window as it is now as `debug-<n>-<name>`.
    fn debug(&mut self, name: &str) -> SandboxResult {
        if std::env::var_os("LARGE_CALL_DEBUG").is_some() {
            self.shots += 1;
            let shot = self.shot()?;
            let cut = pixels::crop(&shot, self.window()?);
            if let Some(dir) = std::env::var_os(EVIDENCE_ENV).map(PathBuf::from) {
                cut.save(dir.join(format!("debug-{}-{name}.png", self.shots)))?;
            }
        }
        Ok(())
    }

    /// The pointer rests on the window's frame, off every control.
    fn park(&mut self) -> SandboxResult {
        let window = self.window()?;
        self.input.move_to(u32_of(window.x + 2), u32_of(window.bottom() - 2))?;
        thread::sleep(Duration::from_millis(150));
        Ok(())
    }

    /// A left click in the middle of `target`, then the pointer parked.
    fn click(&mut self, target: Rect) -> SandboxResult {
        let (x, y) = target.center();
        self.input.move_to(u32_of(x), u32_of(y))?;
        thread::sleep(Duration::from_millis(60));
        self.input.click(u32_of(x), u32_of(y), keys::BTN_LEFT)?;
        // off the button before its tooltip's delay ends
        thread::sleep(Duration::from_millis(60));
        self.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn key(&mut self, spec: &str) -> SandboxResult {
        self.focus()?;
        let (modifiers, key) = parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// Fails unless the screen in `within` looks as it did in `before`.
    fn unchanged(&mut self, before: &RgbImage, within: Rect, what: &str) -> SandboxResult {
        let after = self.shot()?;
        let changed = pixels::changed_pixels(before, &after, within);
        if changed > 40 {
            if let Some(dir) = std::env::var_os(EVIDENCE_ENV).map(PathBuf::from) {
                let window = self.window()?;
                pixels::crop(before, window).save(dir.join("changed-before.png"))?;
                pixels::crop(&after, window).save(dir.join("changed-after.png"))?;
                let blobs: Vec<_> = pixels::changes(before, &after, within).into_iter().take(12).collect();
                return Err(format!("{what}: {changed} px changed in {within:?}: {blobs:?}").into());
            }
            return Err(format!("{what}: {changed} px changed in {within:?}").into());
        }
        Ok(())
    }

    fn theme(&mut self, key: &str) -> SandboxResult {
        fs::write(self.sandbox.path(THEME_TRIGGER), key)?;
        thread::sleep(SETTLE * 2);
        Ok(())
    }

    /// The operations a sent on the mock's call so far named `op`.
    fn sent(&self, op: &str) -> usize {
        self.mock
            .calls()
            .iter()
            .flat_map(|call| call.requests.iter())
            .filter(|(sent, by)| sent == op && by == LAB_A)
            .count()
    }

    fn wait_sent(&self, op: &str, count: usize, case: &str) -> SandboxResult {
        wait_until(WAIT, || (self.sent(op) >= count).then_some(()))
            .ok_or_else(|| format!("{case}: no {op} #{count} reached the mock").into())
    }
}

/// The view each scenario asks for, through View's menu (the pointer on View, the keyboard in its menu) and the mock's
/// share.
impl Rig<'_> {
    fn show(&mut self, view: View) -> SandboxResult {
        let share = view == View::Share;
        if share != self.sharing {
            let sharer = self.people.first().cloned().ok_or("nobody to share")?;
            if !self.mock.share_made_up(&self.call, &sharer, share) {
                return Err("the mock's share didn't go".into());
            }
            self.sharing = share;
            thread::sleep(SETTLE * 2);
        }
        if share {
            return self.wait_share();
        }
        let previous = self
            .app
            .trace
            .catch_up()
            .iter()
            .rev()
            .find(|line| line["view"] == "call-window" && line["change"] == "layout")
            .and_then(|line| line["kinds"][0].as_str())
            .unwrap_or("gallery")
            .to_owned();
        let (button, before) = self.open_view_menu()?;
        // the menu: Gallery, Speaker (Focus on content while a share shows), …
        let downs = if view == View::Gallery { 1 } else { 2 };
        for _ in 0..downs {
            self.key("down")?;
        }
        self.key("enter")?;
        if previous != view.name() {
            self.app.trace.wait_until("the chosen call layout", |seen| {
                seen.iter()
                    .rev()
                    .find(|line| line["view"] == "call-window" && line["change"] == "layout")
                    .is_some_and(|line| line["kinds"][0] == view.name())
            })?;
        }
        let closed = self.shot_until(|closed| pixels::changed_surface_pixels(&before, closed, button) <= 40)?;
        if pixels::changed_surface_pixels(&before, &closed, button) > 40 {
            self.keep_failed("view-choice", [&before, &before, &closed])?;
            return Err("View stayed open after choosing a layout".into());
        }
        Ok(())
    }

    /// View's button: two right of Raise among the toolbar's icons.
    fn toolbar_view(&mut self) -> SandboxResult<Rect> {
        let raise = self.locate_raise("View")?;
        let groups = self.white_groups()?;
        groups
            .iter()
            .position(|group| group.x <= raise.center().0 && raise.center().0 < group.right())
            .and_then(|index| groups.get(index + 2).copied())
            .ok_or_else(|| format!("no View two right of Raise: {groups:?}").into())
    }

    fn open_view_menu(&mut self) -> SandboxResult<(Rect, RgbImage)> {
        let view = self.toolbar_view()?;
        let before = self.shot()?;
        let below = self.still_below_toolbar(&before)?;
        self.click(view)?;
        let open = self.shot_until(|open| pixels::changed_surface_pixels(&before, open, below) >= 1_000)?;
        if pixels::changed_surface_pixels(&before, &open, below) < 1_000 {
            return Err("View's click opened no menu".into());
        }
        Ok((view, before))
    }

    /// Opens View > Max gallery size (the pointer on View, the keyboard down to Max gallery size, whose submenu opens
    /// beside it): the submenu's box, found where it appeared.
    fn view_menu_open_on_sizes(&mut self) -> SandboxResult<Rect> {
        let (view, _) = self.open_view_menu()?;
        for _ in 0..3 {
            self.key("down")?;
        }
        thread::sleep(SETTLE);
        let before = self.shot()?;
        self.key("down")?;
        self.debug("menu-sizes")?;
        let window = self.window()?;
        // below the toolbar: its title's timer ticks on
        let below = Rect {
            y: window.y + TOOLBAR_BAND,
            h: window.h - TOOLBAR_BAND,
            ..window
        };
        let menu = Rect {
            x: view.x - 260,
            w: 260 + view.w + 40,
            ..below
        };
        let submenu = |after: &RgbImage| {
            pixels::changes(&before, after, below)
                .into_iter()
                .map(|blob| blob.bounds)
                .filter(|bounds| bounds.w >= 60 && bounds.h >= 80 && !menu.holds(*bounds, 0))
                .max_by_key(|bounds| bounds.area())
        };
        let after = self.shot_until(|after| submenu(after).is_some())?;
        submenu(&after).ok_or_else(|| "View > Max gallery size opened no submenu".into())
    }

    /// Chooses `size` in View > Max gallery size; `others` are in the call, so its first page shows
    /// `min(size, others + 1)` tiles at full HD.
    fn gallery_size(&mut self, size: usize, others: usize) -> SandboxResult {
        let index = [4, 9, 16, 49]
            .iter()
            .position(|choice| *choice == size)
            .ok_or("no such size")?;
        let sizes = self.view_menu_open_on_sizes()?;
        // its four rows, equal, top to bottom
        let row = Rect {
            y: sizes.y + sizes.h * i32::try_from(index)? / 4,
            h: sizes.h / 4,
            ..sizes
        };
        self.click(row)?;
        self.debug("size-enter")?;
        self.first_page_shows(size, others)
    }

    /// Chooses `size` in View > Max gallery size by the keyboard, as Teams takes it: Right into the sizes, Down to
    /// `size`, Enter. Escape in the sizes first goes back to View's rows, View still open.
    fn gallery_size_by_keys(&mut self, size: usize, others: usize) -> SandboxResult {
        let index = [4, 9, 16, 49]
            .iter()
            .position(|choice| *choice == size)
            .ok_or("no such size")?;
        let window = self.window()?;
        let below = Rect {
            y: window.y + TOOLBAR_BAND,
            h: window.h - TOOLBAR_BAND,
            ..window
        };
        self.park()?;
        let closed = self.shot()?;
        self.view_menu_open_on_sizes()?;
        self.key("right")?;
        self.key("escape")?;
        thread::sleep(SETTLE);
        self.debug("sizes-escape")?;
        if pixels::changed_pixels(&closed, &self.shot()?, below) < 1_000 {
            self.save("failed-sizes-escape")?;
            return Err("Escape in Max gallery size closed View too".into());
        }
        self.key("right")?;
        for _ in 0..index {
            self.key("down")?;
        }
        self.save(&format!("large-call-view-max-gallery-size-{size}-by-keys"))?;
        self.key("enter")?;
        thread::sleep(SETTLE);
        self.first_page_shows(size, others)
    }

    /// At full HD the first page shows `min(size, others + 1)` tiles.
    fn first_page_shows(&mut self, size: usize, others: usize) -> SandboxResult {
        let window = self.window()?;
        if window.w == 1920 {
            let wanted = size.min(others + 1);
            let shot =
                self.shot_until(|shot| pixels::stage(shot, window).is_some_and(|stage| stage.tiles.len() == wanted))?;
            let stage = pixels::stage(&shot, window).ok_or("no stage after choosing the gallery size")?;
            if stage.tiles.len() != wanted {
                return Err(format!(
                    "max gallery size {size}: the first page shows {} tiles",
                    stage.tiles.len()
                )
                .into());
            }
        }
        Ok(())
    }
}

/// Finding the toolbar's buttons and clicking them.
impl Rig<'_> {
    /// The button whose look changes with `toggle` in both directions, after the toolbar's layout settles.
    fn locate_toggled(
        &mut self,
        what: &str,
        mut toggle: impl FnMut(&mut Self, bool) -> SandboxResult,
    ) -> SandboxResult<Rect> {
        self.park()?;
        let window = self.window()?;
        let deadline = Instant::now() + WAIT;
        let mut before = self.settled_toolbar()?;
        let band = Rect {
            h: TOOLBAR_BAND,
            ..window
        };
        loop {
            toggle(self, true)?;
            let on = self.shot_until(|on| {
                pixels::changes(&before, on, band)
                    .iter()
                    .any(|blob| blob.bounds.w >= 14 && blob.bounds.h >= 14)
            })?;
            toggle(self, false)?;
            let off = self.settled_toolbar()?;
            if let Some(button) = pixels::toggled_button(&before, &on, &off, band) {
                return Ok(button);
            }
            if Instant::now() >= deadline {
                return Err(format!("{what}: no button changed with its state in both directions").into());
            }
            before = off;
        }
    }

    fn settled_toolbar(&mut self) -> SandboxResult<RgbImage> {
        let band = Rect {
            h: 30,
            ..self.window()?
        };
        let deadline = Instant::now() + WAIT;
        let mut before = self.shot()?;
        loop {
            thread::sleep(Duration::from_millis(100));
            let after = self.shot()?;
            if pixels::changed_surface_pixels(&before, &after, band) <= 40 {
                return Ok(after);
            }
            if Instant::now() >= deadline {
                return Err("the toolbar's title and controls didn't settle".into());
            }
            before = after;
        }
    }

    /// Shots of the screen until `done` holds for one (within the state-change deadline): the last.
    fn shot_until(&mut self, done: impl Fn(&RgbImage) -> bool) -> SandboxResult<RgbImage> {
        let deadline = Instant::now() + WAIT;
        let mut shot = self.shot()?;
        while !done(&shot) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
            shot = self.shot()?;
        }
        Ok(shot)
    }

    fn wait_share(&mut self) -> SandboxResult {
        let window = self.window()?;
        let shot = self.shot_until(|shot| pixels::shared_picture(shot, window).is_some())?;
        if pixels::shared_picture(&shot, window).is_none() {
            self.save("failed-share")?;
            return Err("the shared picture and its people's strip weren't drawn".into());
        }
        Ok(())
    }

    fn mute(&mut self, on: bool) -> SandboxResult {
        let op = if on { Toggle::On } else { Toggle::Off };
        self.ctl.request(None, Request::Mute { op })?;
        self.wait_call("the mute", |row| row.muted == on)?;
        Ok(())
    }

    fn camera(&mut self, on: bool) -> SandboxResult {
        let op = if on { Toggle::On } else { Toggle::Off };
        self.ctl.request(None, Request::Camera { op })?;
        self.wait_call("the camera", |row| row.camera == on)?;
        Ok(())
    }

    fn hand(&mut self, up: bool) -> SandboxResult {
        let op = if up { "publishState" } else { "removeState" };
        let count = self.sent(op) + 1;
        self.key("ctrl+shift+k")?;
        self.wait_sent(op, count, "Ctrl+Shift+K")
    }

    fn locate_mic(&mut self, case: &str) -> SandboxResult<Rect> {
        self.locate_toggled(&format!("{case}: Mic"), Self::mute)
    }

    fn locate_raise(&mut self, case: &str) -> SandboxResult<Rect> {
        self.locate_toggled(&format!("{case}: Raise"), Self::hand)
    }

    /// Mic's click mutes, its next unmutes, each asked as the button asks (`call asked mute`).
    fn click_mic(&mut self, mic: Rect, case: &str) -> SandboxResult {
        for muted in [true, false] {
            let asked = self.app.trace.asked("mute");
            self.click(mic)?;
            self.wait_call(&format!("{case}: Mic's click"), |row| row.muted == muted)?;
            if self.app.trace.asked("mute") <= asked {
                return Err(format!("{case}: Mic's click wasn't asked").into());
            }
        }
        Ok(())
    }

    /// The toolbar's buttons as they are drawn now, left to right.
    fn white_groups(&mut self) -> SandboxResult<Vec<Rect>> {
        self.park()?;
        let window = self.window()?;
        let shot = self.shot()?;
        // under the title's two lines' height, off the window's edge
        let band = Rect {
            x: window.x + 4,
            y: window.y + 6,
            w: window.w - 8,
            h: TOOLBAR_BAND - 12,
        };
        Ok(pixels::white_groups(&shot, band))
    }

    fn toolbar(&mut self, case: &str) -> SandboxResult<Toolbar> {
        let mic = self.locate_mic(case)?;
        let camera = self.locate_toggled(&format!("{case}: Camera"), Self::camera)?;
        let raise = self.locate_raise(case)?;
        let groups = self.white_groups()?;
        let at = |button: Rect| {
            groups
                .iter()
                .position(|group| group.x <= button.center().0 && button.center().0 < group.right())
        };
        let found = |index: Option<usize>, what: &str| {
            index
                .and_then(|index| groups.get(index).copied())
                .ok_or_else(|| format!("{case}: no {what} in the toolbar: {groups:?}"))
        };
        let raised = at(raise);
        let people = found(raised.and_then(|index| index.checked_sub(1)), "People")?;
        let chat = found(raised.and_then(|index| index.checked_sub(2)), "Chat")?;
        let react = found(raised.map(|index| index + 1), "React")?;
        let view = found(raised.map(|index| index + 2), "View")?;
        let more = found(raised.map(|index| index + 3), "More")?;
        // right of Mic: Share (a devices' caret between them only when the app lists devices)
        let share = found(at(mic).map(|index| index + 1), "Share")?;
        let leave = self.leave_button(case)?;
        let bar = Toolbar {
            chat,
            people,
            raise,
            react,
            view,
            more,
            camera,
            mic,
            share,
            leave,
        };
        let order = [chat, people, raise, react, view, more, camera, mic, share, leave];
        if order.windows(2).any(|pair| pair[0].right() > pair[1].x + 2) {
            return Err(format!("{case}: the toolbar's buttons aren't in Teams' order: {bar:?}").into());
        }
        Ok(bar)
    }

    /// Each button takes its click and does what it says.
    fn toolbar_acts(&mut self, bar: &Toolbar, case: &str) -> SandboxResult {
        self.click_mic(bar.mic, case)?;
        for on in [true, false] {
            let asked = self.app.trace.asked("camera");
            self.click(bar.camera)?;
            self.wait_call(&format!("{case}: Camera's click"), |row| row.camera == on)?;
            if self.app.trace.asked("camera") <= asked {
                return Err(format!("{case}: Camera's click wasn't asked").into());
            }
        }
        self.stage_with_own()?;
        for op in ["publishState", "removeState"] {
            let count = self.sent(op) + 1;
            self.click(bar.raise)?;
            self.wait_sent(op, count, &format!("{case}: Raise's click"))?;
        }
        for (pane, what) in [(bar.people, "People"), (bar.chat, "Chat")] {
            self.toggles_pane(pane, &format!("{case}: {what}"))?;
        }
        for (menu, what) in [(bar.react, "React"), (bar.view, "View"), (bar.more, "More")] {
            self.opens_menu(menu, &format!("{case}: {what}"))?;
        }
        Ok(())
    }

    /// Saves the shots before, with and after what failed, as `failed-<what>-*.png` in the evidence directory.
    fn keep_failed(&self, what: &str, [before, open, closed]: [&RgbImage; 3]) -> SandboxResult {
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV).map(PathBuf::from) {
            let window = self.window()?;
            for (name, shot) in [("before", before), ("open", open), ("closed", closed)] {
                pixels::crop(shot, window).save(dir.join(format!("failed-{what}-{name}.png")))?;
            }
        }
        Ok(())
    }

    /// A pane button: its click opens its pane beside the stage (the stage redrawn), the next closes it.
    fn toggles_pane(&mut self, button: Rect, case: &str) -> SandboxResult {
        self.park()?;
        let before = self.shot()?;
        let below = self.still_below_toolbar(&before)?;
        self.click(button)?;
        let open = self.shot_until(|open| pixels::changed_surface_pixels(&before, open, below) >= 3_000)?;
        if pixels::changed_surface_pixels(&before, &open, below) < 3_000 {
            return Err(format!("{case}: its click opened no pane").into());
        }
        self.click(button)?;
        let closed = self.shot_until(|closed| {
            let back = pixels::changed_surface_pixels(&open, closed, below);
            back >= 3_000 && pixels::changed_surface_pixels(&before, closed, below) <= 40
        })?;
        let back = pixels::changed_surface_pixels(&open, &closed, below);
        let left = pixels::changed_surface_pixels(&before, &closed, below);
        if back < 3_000 || left > 40 {
            self.keep_failed("pane", [&before, &open, &closed])?;
            return Err(
                format!("{case}: its second click didn't close the pane ({back} px went, {left} px stay)").into(),
            );
        }
        Ok(())
    }

    /// A menu button: its click opens its menu under the toolbar, Escape closes it.
    fn opens_menu(&mut self, button: Rect, case: &str) -> SandboxResult {
        self.park()?;
        let before = self.shot()?;
        let below = self.still_below_toolbar(&before)?;
        self.click(button)?;
        let open = self.shot_until(|open| pixels::changed_surface_pixels(&before, open, below) >= 1_000)?;
        let opened = pixels::changed_surface_pixels(&before, &open, below);
        if opened < 1_000 {
            return Err(format!("{case}: its click opened no menu").into());
        }
        // the open button stays readable: its white icon and caption on a lit chip, not a pale one
        let contrast = pixels::ink_contrast(&open, button);
        if contrast < 80 {
            self.keep_failed("menu-button", [&before, &open, &open])?;
            return Err(format!("{case}: its button is unreadable while its menu is open ({contrast})").into());
        }
        self.key("escape")?;
        let closed = self.shot_until(|closed| {
            let back = pixels::changed_surface_pixels(&open, closed, below);
            back * 2 >= opened && pixels::changed_surface_pixels(&before, closed, below) <= 40
        })?;
        let back = pixels::changed_surface_pixels(&open, &closed, below);
        let left = pixels::changed_surface_pixels(&before, &closed, below);
        if back * 2 < opened || left > 40 {
            self.keep_failed("menu", [&before, &open, &closed])?;
            return Err(format!("{case}: Escape didn't close its menu ({back} px went, {left} px stay)").into());
        }
        Ok(())
    }

    fn still_below_toolbar(&self, before: &RgbImage) -> SandboxResult<Rect> {
        let window = self.window()?;
        let below = Rect {
            y: window.y + TOOLBAR_BAND,
            h: window.h - TOOLBAR_BAND,
            ..window
        };
        let picture = if self.sharing {
            Some(pixels::shared_picture(before, window).ok_or("no shared picture before the toolbar click")?)
        } else {
            None
        };
        pixels::still_below_toolbar(below, picture).ok_or_else(|| "no still region under the toolbar".into())
    }

    /// Share's click opens its menu, whose "Share content" asks to share (the screen is then picked at the desktop's
    /// portal, which the sandbox lacks).
    fn share_asks(&mut self, bar: &Toolbar) -> SandboxResult {
        let asked = self.app.trace.asked("share");
        let window = self.window()?;
        let below = Rect {
            y: window.y + TOOLBAR_BAND,
            h: window.h - TOOLBAR_BAND,
            ..window
        };
        let before = self.shot()?;
        self.click(bar.share)?;
        let open = self.shot_until(|open| pixels::changed_pixels(&before, open, below) >= 1_000)?;
        if pixels::changed_pixels(&before, &open, below) < 1_000 {
            return Err("Share's click opened no menu".into());
        }
        self.key("down")?;
        self.key("enter")?;
        self.app
            .trace
            .wait_until("Share asked", |seen| count_asked(seen, "share") > asked)?;
        self.key("escape")?;
        Ok(())
    }

    /// Leave: Teams' red button in the toolbar.
    fn leave_button(&mut self, case: &str) -> SandboxResult<Rect> {
        self.park()?;
        let window = self.window()?;
        let shot = self.shot()?;
        let band = Rect {
            h: TOOLBAR_BAND,
            ..window
        };
        pixels::red(&shot, band).ok_or_else(|| format!("{case}: no Leave in the toolbar").into())
    }

    /// Leave's click leaves: the mock gets a's leave and the app's call ends.
    fn leave(&mut self, button: Rect) -> SandboxResult {
        let leaves = self.sent("leave") + self.sent("terminate") + self.sent("delete");
        let asked = self.app.trace.asked("hang-up");
        self.click(button)?;
        self.app
            .trace
            .wait_until("Leave asked", |seen| count_asked(seen, "hang-up") > asked)?;
        wait_until(WAIT, || {
            (self.sent("leave") + self.sent("terminate") + self.sent("delete") > leaves).then_some(())
        })
        .ok_or("Leave's click: the mock got no leave")?;
        wait_until(WAIT, || {
            self.calls()
                .ok()
                .filter(|calls| calls.iter().all(|row| row.state != "connected"))
        })
        .ok_or("Leave's click: the call is still up")?;
        self.app.trace.wait_until("the call window closed", |seen| {
            seen.iter()
                .any(|line| line["view"] == "call-window" && line["change"] == "closed")
        })?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

fn made_up_mri(index: usize) -> String {
    format!("8:orgid:00000000-0000-4000-8000-00000000b{index:03}")
}

/// A made-up name, Teams' kind: a first name and a family name, each person's own.
fn made_up_name(index: usize) -> String {
    const FIRST: [&str; 10] = ["Ann", "Ben", "Cleo", "Dan", "Eva", "Finn", "Gus", "Hana", "Ivo", "Jun"];
    const LAST: [&str; 6] = ["Able", "Bright", "Sea", "Dee", "Eastwood", "Field"];
    format!(
        "{} {}",
        FIRST[index % FIRST.len()],
        LAST[(index / FIRST.len()) % LAST.len()]
    )
}

fn i32_of(value: u32) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn u32_of(value: i32) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(0)
}

/// How many participants the last grid the trace told held (`call-participant shown` lines up to its `self`).
fn grid_size(seen: &[serde_json::Value]) -> Option<usize> {
    let end = seen
        .iter()
        .rposition(|line| line["view"] == "call-participant" && line["change"] == "self")?;
    let start = seen[..end]
        .iter()
        .rposition(|line| line["view"] == "call-participant" && line["change"] == "self")
        .map_or(0, |at| at + 1);
    Some(
        seen[start..end]
            .iter()
            .filter(|line| line["view"] == "call-participant" && line["change"] == "shown")
            .count(),
    )
}

fn count_asked(seen: &[serde_json::Value], ask: &str) -> usize {
    seen.iter()
        .filter(|line| {
            line["view"] == "call"
                && line["change"] == "asked"
                && line["kinds"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == ask))
        })
        .count()
}

/// The child app in its sandbox: its process, its trace, its control socket.
struct AppUnderTest {
    running: Running,
    trace: TraceReader,
    socket: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, mock: &ChatServiceMock, relay: &str, trouter: &str, dir: &Path) -> SandboxResult<Self> {
        let own = dir.join("a");
        fs::create_dir_all(&own)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(LAB_A).ok_or("the mock's token")?;
        let skype = mock.skypetoken(LAB_A).ok_or("the mock's skypetoken")?;
        let socket = sandbox.path("run/ctl.sock");
        let own_text = own.to_string_lossy().into_owned();
        let socket_text = socket.to_string_lossy().into_owned();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "a"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &mock.origin()),
                (TROUTER_ENV, trouter),
                (RELAY_ENV, relay),
                (IC3_ENV, &ic3),
                (SKYPE_ENV, &skype),
                (DIR_ENV, &own_text),
                (CTL_ENV, &socket_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut app = Self {
            running: Running::new(child)?,
            trace: TraceReader::start(reader),
            socket,
        };
        app.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| app.running.fail(&format!("no ready line: {error:?}")))?;
        app.trace.wait_for("status", "connected")?;
        app.trace.wait_for("calling", "registered")?;
        Ok(app)
    }

    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        fs::write(sandbox.path(QUIT_TRIGGER), "")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("didn't quit: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<serde_json::Value>,
    seen: Vec<serde_json::Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(serde_json::Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    fn catch_up(&mut self) -> &[serde_json::Value] {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        &self.seen
    }

    /// How many times `ask` was asked so far.
    fn asked(&mut self, ask: &str) -> usize {
        count_asked(self.catch_up(), ask)
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult {
        self.wait_until(&format!("{view} {change}"), |seen| {
            seen.iter().any(|line| line["view"] == view && line["change"] == change)
        })
    }

    /// Waits until the lines so far satisfy `done`.
    fn wait_until(&mut self, what: &str, done: impl Fn(&[serde_json::Value]) -> bool) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        while !done(self.catch_up()) {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).map_err(|_| {
                let tail: Vec<String> = self.seen.iter().rev().take(40).map(ToString::to_string).collect();
                format!("no trace line for {what}; the last lines, newest first: {tail:#?}")
            })?;
            self.seen.push(line);
        }
        Ok(())
    }
}

// --- the child ---

/// The account's mock tokens; they never run out in a test.
struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skypetoken.clone()))))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.ic3.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

/// a's engine on the mock, its calls' media on file devices (silence, the test pattern's camera), relayed through
/// the mock's TURN server.
fn engine(feed: VideoFeed, dir: &Path) -> SandboxResult<EngineHandle> {
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(LAB_A)));
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter = || -> SandboxResult<_> {
        Ok(
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &trouter_origin)?
                .with_options(options),
        )
    };
    let calling = trouter()?.for_calls(CallStack::new(Arc::clone(&tracker)));
    let name = display_name("bot-a@lab.example.test").to_owned();
    let identity = Identity {
        mri: Mri::new(LAB_A),
        display_name: Some(name.clone()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: web_client_info(),
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    let devices: Arc<dyn Devices + Send + Sync> = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
    let media = CallMedia {
        devices,
        video: Some(teams_core::account::call_video(Arc::new(feed))),
        relay: Relay::Fixed(vec![RelayServer::udp(env(RELAY_ENV)?.parse()?)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
        },
    };
    let push = Push::new(trouter()?);
    Ok(engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(http()?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(LAB_A), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let feed = VideoFeed::new();
    let engine = engine(feed.clone(), &dir)?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    let opened = Settings {
        selected_chat: Some(AB_CHAT.to_owned()),
        ..Settings::default()
    };
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    let ctl = PathBuf::from(env(CTL_ENV)?);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started {
                    calls: Some(PersonalCalls {
                        ringer: None,
                        sounds: None,
                        echo: Mri::new(ECHO),
                        video: Some(feed),
                        camera: None,
                        devices: None,
                    }),
                    ..first
                },
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: Some(ctl),
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                theme_when_asked(launched.desktop, cx);
                quit_when_asked(cx);
                say("ready");
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    say("quit");
    Ok(())
}

/// Each time the parent writes [`THEME_TRIGGER`]: the settings take the theme it names.
fn theme_when_asked(desktop: gpui::Entity<teams_ui::Desktop>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(THEME_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(key) = fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = fs::remove_file(&trigger);
            let theme = if key == teams_ui::theme::DARK_KEY {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            cx.update(|cx| fixed_theme(&desktop, theme, cx));
        }
    })
    .detach();
}

/// The settings show `theme` whatever the desktop's, as the appearance page's tiles choose one.
fn fixed_theme(desktop: &gpui::Entity<teams_ui::Desktop>, theme: &'static str, cx: &mut App) {
    let settings = desktop.read(cx).settings().clone();
    settings.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = theme;
            },
            cx,
        );
    });
}

/// Once the parent writes [`QUIT_TRIGGER`] in the sandbox dir: the app quits.
fn quit_when_asked(cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(QUIT_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        while !trigger.exists() {
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        cx.update(|cx| {
            app::quit(cx);
        });
    })
    .detach();
}
