//! Calls of ten people and more, as Teams lays them out: whatever the stage shows (the gallery, the speaker view, a
//! share with the people beside it) stays on the stage, under the call's controls, so the microphone and the rest take
//! their clicks however many are in the call and however small the window; the gallery shows equal tiles, as many to
//! a page as View's Max gallery size and the window allow, the account's own among them, the rest on pages turned
//! with `<` `>`; the strips page the same way. Every id and name is made up.

use gpui::{Bounds, Pixels, VisualTestContext, px, size};
use teams_core::{CallAction, Event};

use super::{actions, call, click, draw, drawn, group_call_up, view};

/// The window sizes looked at: the call window's smallest, a small one, a laptop's, a full HD screen's.
const SIZES: [(f32, f32); 4] = [(560.0, 400.0), (800.0, 560.0), (1280.0, 800.0), (1920.0, 1080.0)];

fn mris(count: usize) -> Vec<String> {
    (1..=count)
        .map(|n| format!("8:orgid:00000000-0000-4000-8000-000000000{n:03}"))
        .collect()
}

/// A group call of `count` others, the odd ones' cameras on.
fn large_call(rig: &super::Rig, count: usize) -> &'static mut VisualTestContext {
    let stage = group_call_up(rig);
    let ids = mris(count);
    let who: Vec<(&str, &str, bool, Option<usize>, bool)> = ids
        .iter()
        .enumerate()
        .map(|(index, mri)| (mri.as_str(), "Someone With A Long Name", index % 2 == 0, None, false))
        .collect();
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&who, false),
    });
    rig.cx.run_until_parked();
    stage
}

fn bounds(stage: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    stage.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn within(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.origin.x >= outer.origin.x - px(0.5)
        && inner.origin.y >= outer.origin.y - px(0.5)
        && inner.right() <= outer.right() + px(0.5)
        && inner.bottom() <= outer.bottom() + px(0.5)
}

/// Resizes the window and lets the stage take its new size.
fn resize(stage: &mut VisualTestContext, (width, height): (f32, f32)) {
    stage.simulate_resize(size(px(width), px(height)));
    draw(stage);
    draw(stage);
}

/// Every tile drawn sits on the stage, under the controls, and the microphone takes its click.
fn controls_clear(rig: &super::Rig, stage: &mut VisualTestContext, count: usize, case: &str) {
    draw(stage);
    let area = bounds(stage, "call-stage").expect("the stage");
    let mute = bounds(stage, "call-mute").expect("the microphone");
    assert!(
        mute.bottom() <= area.origin.y,
        "{case}: the microphone {mute:?} over the stage {area:?}"
    );
    let mut shown = 0;
    for index in 0..count {
        if let Some(tile) = bounds(stage, &format!("call-participant-{index}")) {
            shown += 1;
            assert!(
                within(tile, area),
                "{case}: tile {index} {tile:?} leaves the stage {area:?}"
            );
        }
    }
    assert!(shown > 0, "{case}: some tiles show");
    let before = actions(rig).len();
    click(stage, "call-mute");
    let asked = actions(rig);
    assert_eq!(asked.len(), before + 1, "{case}: the microphone took the click");
    assert!(
        matches!(asked.last(), Some(CallAction::Mute { .. })),
        "{case}: {asked:?}"
    );
}

#[test]
fn the_microphone_takes_its_click_in_a_gallery_of_10_to_50_at_every_window_size() {
    for count in [10, 16, 25, 50] {
        let rig = super::calling();
        let stage = large_call(&rig, count);
        for window in SIZES {
            resize(stage, window);
            controls_clear(&rig, stage, count, &format!("gallery of {count} at {window:?}"));
        }
    }
}

#[test]
fn the_microphone_takes_its_click_beside_a_share_and_in_the_speaker_view_of_a_large_call() {
    for count in [10, 25, 50] {
        let rig = super::calling();
        let stage = large_call(&rig, count);
        click(stage, "call-view");
        click(stage, "call-view-speaker");
        for window in SIZES {
            resize(stage, window);
            controls_clear(&rig, stage, count, &format!("speaker view of {count} at {window:?}"));
        }
        rig.send(Event::CallRemoteShare {
            call: call(1),
            on: true,
        });
        rig.cx.run_until_parked();
        assert!(drawn(stage, "call-share-strip"));
        for window in SIZES {
            resize(stage, window);
            controls_clear(&rig, stage, count, &format!("share with {count} at {window:?}"));
            let strip = bounds(stage, "call-share-strip").unwrap();
            for index in 0..count {
                if let Some(tile) = bounds(stage, &format!("call-participant-{index}")) {
                    assert!(
                        within(tile, strip),
                        "share of {count} at {window:?}: tile {index} leaves its strip"
                    );
                }
            }
        }
    }
}

/// The participants' tiles the stage shows, by index.
fn shown(stage: &mut VisualTestContext, count: usize) -> Vec<usize> {
    draw(stage);
    (0..count)
        .filter(|index| bounds(stage, &format!("call-participant-{index}")).is_some())
        .collect()
}

#[test]
fn a_gallery_of_more_than_fit_pages_with_the_account_s_own_tile_on_each_page() {
    let rig = super::calling();
    let stage = large_call(&rig, 60);
    resize(stage, (1920.0, 1080.0));
    // Teams' 49 to a page on a full HD screen, the account's own among them
    let first = shown(stage, 60);
    assert_eq!(first, (0..48).collect::<Vec<_>>());
    assert!(drawn(stage, "call-participant-self") && drawn(stage, "call-gallery-pager"));
    let tiles: Vec<Bounds<Pixels>> = first
        .iter()
        .map(|index| bounds(stage, &format!("call-participant-{index}")).unwrap())
        .collect();
    let own = bounds(stage, "call-participant-self").unwrap();
    for tile in &tiles {
        // equal 16:9 tiles
        assert!((tile.size.width - own.size.width).abs() < px(1.0));
        let ratio = f32::from(tile.size.width) / f32::from(tile.size.height);
        assert!((ratio - 16.0 / 9.0).abs() < 0.05, "{tile:?}");
    }
    // the first page has no way back; the next shows the rest and the account again
    click(stage, "call-gallery-previous");
    assert_eq!(shown(stage, 60), first);
    click(stage, "call-gallery-next");
    assert_eq!(shown(stage, 60), (48..60).collect::<Vec<_>>());
    assert!(drawn(stage, "call-participant-self"));
    click(stage, "call-gallery-next");
    assert_eq!(shown(stage, 60), (48..60).collect::<Vec<_>>(), "no page past the last");
    click(stage, "call-gallery-previous");
    assert_eq!(shown(stage, 60), first);

    // a small window shows fewer to a page, each tile still readable
    resize(stage, (800.0, 560.0));
    let small = shown(stage, 60);
    assert!(small.len() < 48 && small.len() >= 4, "{small:?}");
    let tile = bounds(stage, "call-participant-0").unwrap();
    assert!(tile.size.width >= px(150.0), "{tile:?}");
}

/// Picks `size` in View's Max gallery size (a submenu, reached by the keyboard as a user's arrows reach it).
fn pick_gallery_size(stage: &mut VisualTestContext, size: usize) {
    let item = format!("call-view-gallery-size-{size}");
    for downs in 1..=6 {
        click(stage, "call-view");
        stage.simulate_keystrokes(&format!("{} right", vec!["down"; downs].join(" ")));
        if drawn(stage, &item) {
            click(stage, &item);
            return;
        }
        stage.simulate_keystrokes("escape");
        draw(stage);
    }
    panic!("Max gallery size isn't in View");
}

#[test]
fn view_s_max_gallery_size_sets_how_many_a_page_shows() {
    let rig = super::calling();
    let stage = large_call(&rig, 16);
    resize(stage, (1920.0, 1080.0));
    assert_eq!(shown(stage, 16).len(), 16, "all 17 fit Teams' 49");
    assert!(!drawn(stage, "call-gallery-pager"));
    for (size, others) in [(4, 3), (9, 8), (16, 15), (49, 16)] {
        pick_gallery_size(stage, size);
        assert_eq!(shown(stage, 16).len(), others, "size {size}");
        assert_eq!(drawn(stage, "call-gallery-pager"), others < 16, "size {size}");
        assert!(drawn(stage, "call-participant-self"), "size {size}");
    }
}

#[test]
fn a_small_group_call_shows_everyone_on_one_page() {
    let rig = super::calling();
    let stage = large_call(&rig, 3);
    for window in SIZES {
        resize(stage, window);
        assert_eq!(shown(stage, 3), vec![0, 1, 2], "{window:?}");
        assert!(!drawn(stage, "call-gallery-pager"));
        assert!(drawn(stage, "call-participant-self"));
    }
}

#[test]
fn the_strips_page_with_the_account_s_own_tile_beside_the_share() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    resize(stage, (1280.0, 800.0));
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    let first = shown(stage, 25);
    assert!(!first.is_empty() && first.len() < 25, "{first:?}");
    assert!(drawn(stage, "call-strip-pager") && drawn(stage, "call-participant-self"));
    click(stage, "call-strip-next");
    let second = shown(stage, 25);
    assert_eq!(second.first(), Some(&first.len()), "the next ones");
    assert!(drawn(stage, "call-participant-self"));
    // the share ends: the gallery again, on its own first page
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: false,
    });
    rig.cx.run_until_parked();
    assert!(!drawn(stage, "call-strip-pager"));
    assert_eq!(shown(stage, 25), (0..25).collect::<Vec<_>>());
}

#[test]
fn the_speaker_view_s_strip_pages() {
    let rig = super::calling();
    let stage = large_call(&rig, 25);
    resize(stage, (1280.0, 800.0));
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    assert!(drawn(stage, "call-speaker-main") && drawn(stage, "call-strip-pager"));
    let first = shown(stage, 25);
    click(stage, "call-strip-next");
    let second = shown(stage, 25);
    assert_ne!(first, second);
    assert!(second.contains(&0), "the speaker stays large: {second:?}");
}
