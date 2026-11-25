//! A screen shared in a meeting, as a desktop shares it: 1080p. The client's engines (`teams-core`, `teams-media` on the
//! file device backend) join a meeting scheduled on the server's calendar; one shares a 1080p screen, and each other
//! one shows the sharer sharing and decodes the screen's frames at its size on its sharing line until the share stops.
//! The server's mixer forwards a stream unscaled, as Teams' does, so a viewer that asks the sharing line for less than
//! 1080p gets no frame. A 720p share in a meeting and a 1080p share in a group call (no meeting) go the same way, and
//! a camera beside a 1080p share still comes on its camera line. Every account, chat and meeting is synthetic.

use teams_core::{CallAction, CallId, CallView};
use teams_media::devices::file::{PatternCamera, SCREEN};
use teams_media::session::SHARING_LINE;

use super::engines::{A_TONE, B_TONE, Engine, FRAMES, FULL_HD, Screen, wait_until};
use super::group_media::{C_TONE, Mixed, connect_all, group_chat, hang_up_all};
use super::group_video::shape;
use super::meetings::{Person, join_of, joined, leave_all, schedule, shut_down};
use crate::support::backends;

/// Whether `view` shows `mri` sharing (`on`) or not.
fn sharing(view: &CallView, mri: &str, on: bool) -> bool {
    view.participants
        .iter()
        .any(|party| party.mri == mri && party.sharing == on)
}

/// Waits until `viewer` shows `sharer` sharing in `call`, and [`FRAMES`] more frames of the size `size` on its sharing
/// line.
async fn sees_screen(viewer: &mut Engine, name: &str, call: CallId, sharer: &str, size: PatternCamera) {
    viewer.view(call, |view| sharing(view, sharer, true)).await;
    let since = viewer.seen(SHARING_LINE).frames;
    let what = format!("{name}: {sharer}'s {}x{} screen", size.width, size.height);
    wait_until(&what, || {
        let seen = viewer.seen(SHARING_LINE);
        seen.frames >= since + FRAMES && (seen.width, seen.height) == (size.width, size.height)
    })
    .await;
    eprintln!("{what}: {:?}", viewer.seen(SHARING_LINE));
}

/// Ada (organizer), Bert and Cora, Cora's screen `screen`, in a meeting of theirs: the three and their calls.
async fn in_a_meeting(mixed: &Mixed, screen: Screen) -> ([Person; 3], [CallId; 3]) {
    let mut ada = Person::new(mixed, "Ada", A_TONE).await;
    let mut bert = Person::new(mixed, "Bert", B_TONE).await;
    let mut cora = Person::sharing(mixed, "Cora", C_TONE, screen).await;
    let event = schedule(&ada, &[&bert, &cora]).await;
    let join = join_of(&event);
    let calls = [
        joined(&mut ada, &join).await,
        joined(&mut bert, &join).await,
        joined(&mut cora, &join).await,
    ];
    mixed.connected(&mixed.open_call().await, 3).await;
    ([ada, bert, cora], calls)
}

/// A meeting of Ada (organizer), Bert and Cora: Cora shares `screen`, Ada and Bert see it at `size` until it stops.
async fn meeting_share(screen: Screen, size: PatternCamera) {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let ([mut ada, mut bert, mut cora], calls) = in_a_meeting(&mixed, screen).await;
        // a guest shares, not the organizer: the others see it as the meeting's sharer
        cora.engine
            .call(CallAction::Share {
                call: calls[2],
                on: true,
            })
            .await;
        let sharer = cora.engine.mri.clone();
        sees_screen(&mut ada.engine, "Ada", calls[0], &sharer, size).await;
        sees_screen(&mut bert.engine, "Bert", calls[1], &sharer, size).await;
        cora.engine
            .call(CallAction::Share {
                call: calls[2],
                on: false,
            })
            .await;
        for (viewer, call) in [(&mut ada, calls[0]), (&mut bert, calls[1])] {
            viewer.engine.view(call, |view| sharing(view, &sharer, false)).await;
        }
        leave_all(&mut [&mut ada, &mut bert, &mut cora], &calls).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_1080p_screen_shared_in_a_meeting_reaches_everyone_else_until_it_stops() {
    meeting_share(Screen::FullHd, FULL_HD).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_720p_screen_shared_in_a_meeting_reaches_everyone_else_until_it_stops() {
    meeting_share(Screen::Moving, SCREEN).await;
}

/// Waits until `viewer` shows `filmed`'s camera on its line in `call`: [`FRAMES`] more frames of the shape of `size`
/// and not larger than it.
async fn sees_camera(viewer: &mut Engine, call: CallId, filmed: &str, size: PatternCamera) {
    // the view that put the camera on its line can be among the views that `sees_screen` read
    let view = viewer
        .shown(call, |view| {
            view.participants
                .iter()
                .any(|party| party.mri == filmed && party.video_on && party.line.is_some())
        })
        .await;
    let line = view
        .participants
        .iter()
        .find_map(|party| (party.mri == filmed).then_some(party.line).flatten())
        .unwrap();
    let since = viewer.seen(line).frames;
    let wanted = shape(size.width, size.height);
    let what = format!("{}: {filmed}'s camera on line {line}", viewer.mri);
    // balanced degradation can only make the camera smaller; the size limit keeps a 16:9 screen off this line
    wait_until(&what, || {
        let seen = viewer.seen(line);
        seen.frames >= since + FRAMES
            && seen.width <= size.width
            && seen.height <= size.height
            && (shape(seen.width, seen.height) - wanted).abs() < 0.1
    })
    .await;
    eprintln!("{what}: {:?}", viewer.seen(line));
}

/// Ada, Bert and Cora in a call of their group chat, Bert's screen 1080p: the three and their calls.
async fn in_a_group_call(mixed: &Mixed) -> ([Engine; 3], Vec<CallId>) {
    let mut a = mixed.engine("Ada", A_TONE).await;
    let mut b = mixed.engine_sharing("Bert", B_TONE, Screen::FullHd).await;
    let mut c = mixed.engine("Cora", C_TONE).await;
    let chat = group_chat(&a, &[&b, &c], &[]).await;
    let calls = connect_all(&chat, &mut [&mut a, &mut b, &mut c]).await;
    mixed.connected(&mixed.open_call().await, 3).await;
    ([a, b, c], calls)
}

/// A group call, not a meeting: Ada's camera is on while Bert shares a 1080p screen. Cora sees both, the camera on
/// its camera line and the screen on the sharing line; Ada sees the screen too. Then the share stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_1080p_screen_shared_in_a_group_call_reaches_the_others_beside_a_camera() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let ([mut a, mut b, mut c], calls) = in_a_group_call(&mixed).await;
        a.call(CallAction::Camera {
            call: calls[0],
            on: true,
        })
        .await;
        let (a_mri, b_mri) = (a.mri.clone(), b.mri.clone());
        b.call(CallAction::Share {
            call: calls[1],
            on: true,
        })
        .await;
        sees_screen(&mut a, "Ada", calls[0], &b_mri, FULL_HD).await;
        sees_screen(&mut c, "Cora", calls[2], &b_mri, FULL_HD).await;
        sees_camera(&mut c, calls[2], &a_mri, PatternCamera::default()).await;
        b.call(CallAction::Share {
            call: calls[1],
            on: false,
        })
        .await;
        for (viewer, call) in [(&mut a, calls[0]), (&mut c, calls[2])] {
            viewer.view(call, |view| sharing(view, &b_mri, false)).await;
        }
        hang_up_all(&mut [&mut a, &mut b, &mut c], &calls).await;
        for engine in [a, b, c] {
            engine.shut_down().await;
        }
        mixed.stop().await;
    }
}
