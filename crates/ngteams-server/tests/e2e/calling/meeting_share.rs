//! A screen shared in a meeting, as a desktop shares it: 1080p. The client's engines (`teams-core`, `teams-media` on the
//! file device backend) join a meeting scheduled on the server's calendar; one shares a 1080p screen, and each other
//! one shows the sharer sharing and decodes the screen's frames at its size on its sharing line until the share stops.
//! The server's mixer forwards a stream unscaled, as Teams' does, so a viewer that asks the sharing line for less than
//! 1080p gets no frame. Every account and meeting is synthetic.

use teams_core::{CallAction, CallId, CallView};
use teams_media::session::SHARING_LINE;

use super::engines::{A_TONE, B_TONE, FRAMES, FULL_HD, Screen, wait_until};
use super::group_media::{C_TONE, Mixed};
use super::meetings::{Person, join_of, joined, leave_all, schedule, shut_down};
use crate::support::backends;

/// Whether `view` shows `mri` sharing (`on`) or not.
fn sharing(view: &CallView, mri: &str, on: bool) -> bool {
    view.participants
        .iter()
        .any(|party| party.mri == mri && party.sharing == on)
}

/// Waits until `viewer` shows `sharer` sharing in `call`, and [`FRAMES`] more 1080p frames on its sharing line.
async fn sees_screen(viewer: &mut Person, call: CallId, sharer: &str) {
    viewer.engine.view(call, |view| sharing(view, sharer, true)).await;
    let engine = &viewer.engine;
    let since = engine.seen(SHARING_LINE).frames;
    let what = format!("{}: {sharer}'s 1080p screen", viewer.name);
    wait_until(&what, || {
        let seen = engine.seen(SHARING_LINE);
        seen.frames >= since + FRAMES && (seen.width, seen.height) == (FULL_HD.width, FULL_HD.height)
    })
    .await;
    eprintln!("{what}: {:?}", engine.seen(SHARING_LINE));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_1080p_screen_shared_in_a_meeting_reaches_everyone_else_until_it_stops() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::sharing(&mixed, "Cora", C_TONE, Screen::FullHd).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        let c_call = joined(&mut cora, &join).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        // a guest shares, not the organizer: the others see it as the meeting's sharer
        cora.engine.call(CallAction::Share { call: c_call, on: true }).await;
        let sharer = cora.engine.mri.clone();
        sees_screen(&mut ada, a_call, &sharer).await;
        sees_screen(&mut bert, b_call, &sharer).await;
        cora.engine
            .call(CallAction::Share {
                call: c_call,
                on: false,
            })
            .await;
        for (viewer, call) in [(&mut ada, a_call), (&mut bert, b_call)] {
            viewer.engine.view(call, |view| sharing(view, &sharer, false)).await;
        }
        leave_all(&mut [&mut ada, &mut bert, &mut cora], &[a_call, b_call, c_call]).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}
