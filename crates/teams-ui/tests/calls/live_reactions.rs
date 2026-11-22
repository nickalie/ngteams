//! Live reactions as Teams shows them in every layout (the gallery, the speaker view, a share with its strip): on the
//! sender's tile while it is on the stage (the account's own on its own tile), its 3D emoji played once (the picker's
//! strip in its place while there is none), gone after its time; floating up from the stage's bottom while the sender's tile is not on it (another page, Focus on
//! content); the window draws no more frames for them once the last is gone. Every id and name is made up.

use std::sync::Arc;
use std::time::Duration;

use gpui::{Bounds, Pixels, VisualTestContext};
use teams_core::{CallStatus, Event};
use teams_proto::Mri;
use teams_ui::call_stage::CallStage;
use teams_ui::call_window::CallWindows;
use teams_ui::calls::REACTION_SHOWN;

use super::{LAB_A, LAB_C, Rig, call, call_windows, click, draw, drawn, registered, shell_window, stage, status};
use super::{TRIO, in_group_call, trace, view};

/// How long a reaction floats when its sender's tile is not on the stage.
const FLOAT_FOR: Duration = Duration::from_millis(2_000);

fn react(rig: &Rig, from: &str, reaction: &str) {
    rig.send(Event::CallReaction {
        call: call(1),
        from: Mri::new(from),
        name: Some("Someone".to_owned()),
        reaction: reaction.to_owned(),
    });
    rig.cx.run_until_parked();
}

fn bounds(stage: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    draw(stage);
    stage.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn later(rig: &Rig, stage: &mut VisualTestContext, by: Duration) {
    stage.executor().advance_clock(by);
    rig.cx.run_until_parked();
    draw(stage);
}

fn shown_now(rig: &Rig, windows: &gpui::Entity<CallWindows>) -> bool {
    let handle = rig.cx.read(|cx| windows.read(cx).window()).unwrap();
    rig.cx.read(|cx| {
        let stage = handle.read(cx).unwrap().view().clone().downcast::<CallStage>().unwrap();
        stage.read(cx).reactions_shown_now()
    })
}

fn mri(n: usize) -> String {
    format!("8:orgid:00000000-0000-4000-8000-000000000{n:03}")
}

/// A group call of 25 others, the window a laptop's size: the rig, its windows and the stage.
fn large_call(rig: &Rig) -> (gpui::Entity<CallWindows>, &'static mut VisualTestContext) {
    let (trace, _) = trace();
    let (shell, cx) = shell_window(rig, Some(TRIO), Arc::clone(&trace));
    let (windows, _) = call_windows(rig, &shell, &trace);
    registered(rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(rig, 1, CallStatus::Connected);
    let ids: Vec<String> = (1..=25).map(mri).collect();
    let who: Vec<(&str, &str, bool, Option<usize>, bool)> = ids
        .iter()
        .map(|mri| (mri.as_str(), "Someone", false, None, false))
        .collect();
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&who, false),
    });
    let stage = stage(rig, &windows);
    stage.simulate_resize(gpui::size(gpui::px(1280.0), gpui::px(800.0)));
    draw(stage);
    draw(stage);
    (windows, stage)
}

/// A reaction of someone whose tile is not on the stage floats up from the stage's bottom, rising as time goes, and
/// goes after its time; then nothing shows and the window asks for no frame.
fn floats_up_and_ends(rig: &Rig, windows: &gpui::Entity<CallWindows>, stage: &mut VisualTestContext, case: &str) {
    react(rig, &mri(25), "heart");
    let start = bounds(stage, "call-reaction-0").unwrap_or_else(|| panic!("{case}: it floats"));
    let area = bounds(stage, "call-stage").unwrap();
    assert!(
        start.bottom() < area.bottom() && start.origin.y > area.origin.y + area.size.height / 2.0,
        "{case}: from the stage's bottom: {start:?} in {area:?}"
    );
    assert!(shown_now(rig, windows), "{case}: the window draws the next frame");
    later(rig, stage, Duration::from_millis(500));
    let up = bounds(stage, "call-reaction-0").unwrap();
    assert!(
        up.origin.y < start.origin.y - gpui::px(10.0),
        "{case}: it rose: {start:?} → {up:?}"
    );
    later(rig, stage, Duration::from_millis(800));
    let higher = bounds(stage, "call-reaction-0").unwrap();
    assert!(
        higher.origin.y < up.origin.y,
        "{case}: still rising: {up:?} → {higher:?}"
    );
    later(rig, stage, FLOAT_FOR.saturating_sub(Duration::from_millis(1_300)));
    assert!(!drawn(stage, "call-reactions"), "{case}: gone after its time");
    assert!(!shown_now(rig, windows), "{case}: no frame asked any more");
}

#[test]
fn a_reaction_of_someone_out_of_sight_floats_up_in_the_gallery_the_speaker_view_and_beside_a_share() {
    let rig = super::calling();
    let (windows, stage) = large_call(&rig);
    // the gallery at 9 to a page: the 25th is on its third page
    let handle = rig.cx.read(|cx| windows.read(cx).window()).unwrap();
    rig.cx.update(|cx| {
        let call_stage = handle.read(cx).unwrap().view().clone().downcast::<CallStage>().unwrap();
        call_stage.update(cx, |call_stage, cx| call_stage.set_gallery_size(9, cx));
    });
    assert!(drawn(stage, "call-gallery-pager") && !drawn(stage, "call-participant-24"));
    floats_up_and_ends(&rig, &windows, stage, "gallery");
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    assert!(drawn(stage, "call-speaker-main"));
    floats_up_and_ends(&rig, &windows, stage, "speaker");
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-share-strip"));
    floats_up_and_ends(&rig, &windows, stage, "share");
    click(stage, "call-view");
    click(stage, "call-view-focus");
    assert!(drawn(stage, "call-share-focus"));
    floats_up_and_ends(&rig, &windows, stage, "focus on content");
}

/// A reaction of someone whose tile is on the stage shows on it, the account's own on its own tile, each through its
/// time; then nothing shows and the window asks for no frame.
fn on_tiles(rig: &Rig, windows: &gpui::Entity<CallWindows>, stage: &mut VisualTestContext, case: &str) {
    react(rig, LAB_C, "heart");
    let tile = bounds(stage, "call-participant-1").unwrap();
    let emoji = bounds(stage, "call-participant-1-reaction").unwrap_or_else(|| panic!("{case}: on c's tile"));
    assert!(
        (emoji.center().x - tile.center().x).abs() < gpui::px(1.0)
            && (emoji.origin.y - tile.center().y).abs() < gpui::px(1.0),
        "{case}: centred, its top at the tile's middle: {emoji:?} on {tile:?}"
    );
    assert!(!drawn(stage, "call-reactions"), "{case}: nothing floats");
    later(rig, stage, Duration::from_millis(500));
    react(rig, LAB_A, "like");
    assert!(
        drawn(stage, "call-participant-self-reaction"),
        "{case}: the account's own on its tile"
    );
    assert!(shown_now(rig, windows));
    later(rig, stage, Duration::from_millis(1_500));
    assert!(drawn(stage, "call-participant-1-reaction"), "{case}: still showing");
    later(rig, stage, REACTION_SHOWN.saturating_sub(Duration::from_millis(2_000)));
    assert!(
        !drawn(stage, "call-participant-1-reaction"),
        "{case}: gone after its time"
    );
    assert!(
        drawn(stage, "call-participant-self-reaction"),
        "{case}: the later one stays its time"
    );
    later(rig, stage, REACTION_SHOWN);
    assert!(!drawn(stage, "call-participant-self-reaction"));
    assert!(!shown_now(rig, windows), "{case}: no frame asked any more");
}

#[test]
fn a_reaction_shows_on_its_sender_s_tile_in_the_gallery_the_speaker_view_and_beside_a_share() {
    let rig = super::calling();
    let (trace, _) = trace();
    let (windows, stage) = in_group_call(&rig, &trace);
    assert!(drawn(stage, "call-grid"));
    on_tiles(&rig, &windows, stage, "gallery");
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    assert!(drawn(stage, "call-speaker-main"));
    react(&rig, LAB_C, "heart");
    assert!(drawn(stage, "call-participant-1-reaction"), "speaker view: on c's tile");
    later(&rig, stage, REACTION_SHOWN);
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-share-strip"));
    on_tiles(&rig, &windows, stage, "share");
}

#[test]
fn a_live_reaction_plays_its_animated_emoji() {
    let rig = super::calling();
    let mut strip = image::RgbaImage::new(4, 12);
    strip.put_pixel(0, 0, image::Rgba([1, 2, 3, 255]));
    let mut png = Vec::new();
    strip
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    rig.core
        .animations
        .lock()
        .unwrap()
        .insert("heart".to_owned(), Arc::from(png));
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    react(&rig, LAB_C, "heart");
    draw(stage);
    assert!(
        drawn(stage, "call-participant-1-reaction-playing"),
        "its animation plays"
    );
}

/// A made-up 3D emoji of `frames` frames, `side` px.
fn sprite(side: u32, frames: u32) -> Arc<[u8]> {
    let mut picture = image::RgbaImage::new(side, side * frames);
    picture.put_pixel(0, 0, image::Rgba([1, 2, 3, 255]));
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    Arc::from(png)
}

#[test]
fn a_live_reaction_plays_teams_3d_emoji_on_the_tile_in_place_of_the_strip() {
    let rig = super::calling();
    rig.core
        .animations
        .lock()
        .unwrap()
        .insert("heart".to_owned(), sprite(4, 3));
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    let tile = bounds(stage, "call-participant-1").unwrap();
    let side = if tile.size.height.as_f32() >= 480.0 {
        140
    } else if tile.size.height.as_f32() >= 240.0 {
        112
    } else {
        56
    };
    rig.core
        .animations_3d
        .lock()
        .unwrap()
        .insert(("heart".to_owned(), side), sprite(8, 102));
    react(&rig, LAB_C, "heart");
    // the 3D emoji read and decoded
    later(&rig, stage, Duration::from_millis(1));
    assert!(drawn(stage, "call-participant-1-reaction-3d"), "the 3D emoji plays");
    assert!(!drawn(stage, "call-participant-1-reaction-playing"), "not the strip");
    later(&rig, stage, REACTION_SHOWN / 2);
    assert!(
        drawn(stage, "call-participant-1-reaction-3d"),
        "still on the tile half way"
    );
    later(&rig, stage, REACTION_SHOWN);
    assert!(!drawn(stage, "call-participant-1-reaction"), "gone after its time");
}

#[test]
fn a_live_reaction_without_a_3d_emoji_plays_the_picker_s_strip() {
    let rig = super::calling();
    rig.core
        .animations
        .lock()
        .unwrap()
        .insert("surprised".to_owned(), sprite(4, 3));
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    react(&rig, LAB_C, "surprised");
    later(&rig, stage, Duration::from_millis(1));
    assert!(drawn(stage, "call-participant-1-reaction-playing"), "the strip plays");
    assert!(!drawn(stage, "call-participant-1-reaction-3d"));
}
