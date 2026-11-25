//! A group call's video through the server's own media node (`ngteams_server::mixer`): the client's engines turn
//! their cameras on, the roster tells it, each client asks the node for the others' video on its receiving lines
//! (source requests), and the node forwards each camera onto the lines that asked for it; a shared screen comes on
//! the others' sharing line. Each camera's picture has a shape of its own, so a line shows whose video it carries.

use std::time::{Duration, Instant};

use teams_core::{CallAction, CallId, CallParticipant, CallStatus, CallView};
use teams_media::devices::file::PatternCamera;
use teams_media::session::SHARING_LINE;

use super::engines::{A_TONE, B_TONE, Engine, FRAMES, Screen, wait_until};
use super::group_media::{C_TONE, Mixed, connect_all, group_chat, hang_up_all};
use crate::support::backends;

/// The cameras' shapes: wide, four by three, square.
const WIDE: PatternCamera = PatternCamera {
    width: 640,
    height: 360,
    fps: 15,
};
const FOUR_THREE: PatternCamera = PatternCamera {
    width: 480,
    height: 360,
    fps: 15,
};
const SQUARE: PatternCamera = PatternCamera {
    width: 360,
    height: 360,
    fps: 15,
};

/// How long a line shows no frame before its video counts as stopped: many frame times at the cameras' 15 fps.
const QUIET: Duration = Duration::from_secs(1);

/// How long after the roster told a stop the frames sent before it can still come: the media path's delay and jitter
/// buffer. Under load they came up to 693 ms late, so this has room, but a video that goes on does not fit in it.
const SLACK: Duration = Duration::from_secs(3);

/// A picture's shape: its width over its height.
pub(super) fn shape(width: u32, height: u32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a picture's sides are a few thousand at most")]
    let (width, height) = (width as f32, height as f32);
    width / height.max(1.0)
}

/// The participant `mri` as `view` shows it.
fn party<'a>(view: &'a CallView, mri: &str) -> Option<&'a CallParticipant> {
    view.participants.iter().find(|party| party.mri == mri)
}

/// Waits until `engine` shows each of `others` with its camera on, on a receiving line of its own, and that line has
/// shown at least [`FRAMES`] frames of the shape the other's camera has. Gives the line of each other, in order.
async fn sees(engine: &mut Engine, call: CallId, others: &[(&str, PatternCamera)]) -> Vec<usize> {
    let view = engine
        .shown(call, |view| {
            others.iter().all(|(mri, _)| {
                party(view, mri).is_some_and(|party| party.video_on && party.line.is_some_and(|line| line >= 1))
            })
        })
        .await;
    let mut lines = Vec::new();
    for (mri, camera) in others {
        let line = party(&view, mri).and_then(|party| party.line).unwrap();
        lines.push(line);
        let wanted = shape(camera.width, camera.height);
        let what = format!("{}: {mri}'s video on line {line}", engine.mri);
        wait_until(&what, || {
            let seen = engine.seen(line);
            seen.frames >= FRAMES && (shape(seen.width, seen.height) - wanted).abs() < 0.1
        })
        .await;
        eprintln!("{what}: {:?}", engine.seen(line));
    }
    lines
}

/// Waits until `engine`'s line `line` stops showing `what`, which the roster told at `told`: no frame for [`QUIET`]
/// by [`SLACK`] after `told`, and then at most 2 frames more in a second. The roster tells a stop on the signaling
/// path, and the frames sent before the stop come on the media path. Teams orders nothing between the two, so frames
/// can still come after the roster told the stop, but not for longer than [`SLACK`].
async fn stopped(engine: &Engine, line: usize, what: &str, told: Instant) {
    let label = format!("{}: {what} stopped on line {line}", engine.mri);
    let deadline = told + SLACK + QUIET;
    while engine.seen(line).last.is_some_and(|last| last.elapsed() < QUIET) {
        assert!(
            Instant::now() < deadline,
            "{label}: frames still came {SLACK:?} after the roster told the stop: {:?}",
            engine.seen(line)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let shown = engine.seen(line).frames;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let seen = engine.seen(line);
    assert!(seen.frames <= shown + 2, "{label}, then came again: {seen:?}");
}

/// When `engine` showed a view of `call` that is `wanted`. Wait for each engine at the same time: a view that waits in
/// the queue while the test waits for another engine would give a later time.
async fn told(engine: &mut Engine, call: CallId, wanted: impl Fn(&CallView) -> bool) -> Instant {
    engine.view(call, wanted).await;
    Instant::now()
}

/// Turns `engine`'s camera in `call` on or off.
async fn camera(engine: &Engine, call: CallId, on: bool) {
    engine.call(CallAction::Camera { call, on }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_clients_see_each_others_video_on_the_lines_they_asked_for() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine_filmed("Ada", A_TONE, WIDE).await;
        let mut b = mixed.engine_filmed("Bert", B_TONE, FOUR_THREE).await;
        let mut c = mixed.engine_filmed("Cora", C_TONE, SQUARE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        for (engine, call) in [(&a, calls[0]), (&b, calls[1]), (&c, calls[2])] {
            camera(engine, call, true).await;
        }
        let (a_mri, b_mri, c_mri) = (a.mri.clone(), b.mri.clone(), c.mri.clone());
        sees(&mut a, calls[0], &[(&b_mri, FOUR_THREE), (&c_mri, SQUARE)]).await;
        let lines = [
            sees(&mut b, calls[1], &[(&a_mri, WIDE), (&c_mri, SQUARE)]).await[0],
            sees(&mut c, calls[2], &[(&a_mri, WIDE), (&b_mri, FOUR_THREE)]).await[0],
        ];
        // a's camera off: the others are told, and its line shows nothing more
        camera(&a, calls[0], false).await;
        let camera_off = |view: &CallView| party(view, &a_mri).is_some_and(|party| !party.video_on);
        let (b_told, c_told) = tokio::join!(told(&mut b, calls[1], camera_off), told(&mut c, calls[2], camera_off));
        for (engine, line, told) in [(&b, lines[0], b_told), (&c, lines[1], c_told)] {
            stopped(engine, line, "a's camera", told).await;
        }
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_joiner_sees_the_cameras_that_were_on_and_is_seen() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine_filmed("Ada", A_TONE, WIDE).await;
        let mut b = mixed.engine_filmed("Bert", B_TONE, FOUR_THREE).await;
        let mut c = mixed.engine_filmed("Cora", C_TONE, SQUARE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let mut calls = connect_all(&chat, &mut [&mut a, &mut b]).await;
        let rung = c.incoming().await;
        c.call(CallAction::Decline { call: rung.call }).await;
        camera(&a, calls[0], true).await;
        camera(&b, calls[1], true).await;
        let (a_mri, b_mri, c_mri) = (a.mri.clone(), b.mri.clone(), c.mri.clone());
        sees(&mut a, calls[0], &[(&b_mri, FOUR_THREE)]).await;
        // c joins the call it finds in the chat: the running cameras come to it from their next keyframe
        assert!(c.handle.sync().await.unwrap().failed.is_empty());
        let joined = c.call(CallAction::Join { chat: chat.clone() }).await;
        c.status(joined, &CallStatus::Connected).await;
        calls.push(joined);
        sees(&mut c, joined, &[(&a_mri, WIDE), (&b_mri, FOUR_THREE)]).await;
        camera(&c, joined, true).await;
        sees(&mut a, calls[0], &[(&c_mri, SQUARE)]).await;
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_screen_reaches_the_others_until_it_stops() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine("Bert", B_TONE).await;
        let mut c = mixed.engine("Cora", C_TONE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        b.call(CallAction::Share {
            call: calls[1],
            on: true,
        })
        .await;
        let sharer = b.mri.clone();
        for (engine, call) in [(&mut a, calls[0]), (&mut c, calls[2])] {
            engine
                .view(call, |view| party(view, &sharer).is_some_and(|party| party.sharing))
                .await;
            let what = format!("{}: b's screen", engine.mri);
            wait_until(&what, || {
                let seen = engine.seen(SHARING_LINE);
                seen.frames >= FRAMES && (shape(seen.width, seen.height) - 16.0 / 9.0).abs() < 0.1
            })
            .await;
            eprintln!("{what}: {:?}", engine.seen(SHARING_LINE));
        }
        b.call(CallAction::Share {
            call: calls[1],
            on: false,
        })
        .await;
        let unshared = |view: &CallView| party(view, &sharer).is_some_and(|party| !party.sharing);
        let (a_told, c_told) = tokio::join!(told(&mut a, calls[0], unshared), told(&mut c, calls[2], unshared));
        for (engine, told) in [(&a, a_told), (&c, c_told)] {
            stopped(engine, SHARING_LINE, "b's screen", told).await;
        }
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

/// How many frames of a still screen a viewer must show of each share: more than the screen gave.
const STILL_SHOWN: u64 = 10;

/// Waits until `engine` shows `sharer` sharing in `call` and its sharing line [`STILL_SHOWN`] frames of a 16:9 screen
/// more than `before`.
async fn sees_screen(engine: &mut Engine, call: CallId, sharer: &str, before: u64) {
    engine
        .view(call, |view| party(view, sharer).is_some_and(|party| party.sharing))
        .await;
    let what = format!("{}: {sharer}'s still screen", engine.mri);
    wait_until(&what, || {
        let seen = engine.seen(SHARING_LINE);
        seen.frames >= before + STILL_SHOWN && (shape(seen.width, seen.height) - 16.0 / 9.0).abs() < 0.1
    })
    .await;
    eprintln!("{what}: {:?}", engine.seen(SHARING_LINE));
}

/// Waits until `engine` shows that `sharer` shares no more in `call`.
async fn sees_share_end(engine: &mut Engine, call: CallId, sharer: &str) {
    engine
        .view(call, |view| party(view, sharer).is_some_and(|party| !party.sharing))
        .await;
}

/// `sharer` shares its screen in `call` (`on`) or stops.
async fn share(sharer: &Engine, call: CallId, on: bool) {
    sharer.call(CallAction::Share { call, on }).await;
}

/// A screen that holds still (as a desktop's does while nothing on it changes) reaches a late joiner: it joins while b
/// shares and sees b's screen too, from a keyframe of b's last picture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_joiner_sees_a_still_screen_that_is_shared() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine_sharing("Bert", B_TONE, Screen::Still).await;
        let mut c = mixed.engine("Cora", C_TONE).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let mut calls = connect_all(&chat, &mut [&mut a, &mut b]).await;
        let rung = c.incoming().await;
        c.call(CallAction::Decline { call: rung.call }).await;
        let sharer = b.mri.clone();
        share(&b, calls[1], true).await;
        sees_screen(&mut a, calls[0], &sharer, 0).await;
        // c joins the call while b shares: b's screen comes to it too
        assert!(c.handle.sync().await.unwrap().failed.is_empty());
        let joined = c.call(CallAction::Join { chat: chat.clone() }).await;
        c.status(joined, &CallStatus::Connected).await;
        calls.push(joined);
        sees_screen(&mut c, joined, &sharer, 0).await;
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}

/// A still screen shared, stopped and shared again reaches the others each time, and so does another's share that
/// starts after it stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_still_screen_shared_again_and_another_s_share_after_it_reach_the_others() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut a = mixed.engine("Ada", A_TONE).await;
        let mut b = mixed.engine_sharing("Bert", B_TONE, Screen::Still).await;
        let mut c = mixed.engine_sharing("Cora", C_TONE, Screen::Still).await;
        let chat = group_chat(&a, &[&b, &c], &[]).await;
        let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        let (b_mri, c_mri) = (b.mri.clone(), c.mri.clone());
        for round in 1..=2 {
            let before = (a.seen(SHARING_LINE).frames, c.seen(SHARING_LINE).frames);
            share(&b, calls[1], true).await;
            eprintln!("b shares, round {round}");
            sees_screen(&mut a, calls[0], &b_mri, before.0).await;
            sees_screen(&mut c, calls[2], &b_mri, before.1).await;
            share(&b, calls[1], false).await;
            sees_share_end(&mut a, calls[0], &b_mri).await;
            sees_share_end(&mut c, calls[2], &b_mri).await;
        }
        // c shares after b stopped: a and b see c's screen
        let before = (a.seen(SHARING_LINE).frames, b.seen(SHARING_LINE).frames);
        share(&c, calls[2], true).await;
        sees_screen(&mut a, calls[0], &c_mri, before.0).await;
        sees_screen(&mut b, calls[1], &c_mri, before.1).await;
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}
