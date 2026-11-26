//! Viewer downloads, unavailable images and pointer zoom/pan against synthetic image bytes on GPUI's test platform.

#![cfg(test)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Entity, Modifiers, MouseButton, MouseDownEvent, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, point,
    px, size,
};
use teams_core::{PictureBytes, Reply, Strip};
use teams_ui::inline::{PictureEngine, PictureSource};
use teams_ui::people::Face;
use teams_ui::picture_viewer::{self, PictureViewer, Viewed, Viewing};

struct Pictures(HashMap<String, PictureBytes>);

impl PictureEngine for Pictures {
    fn me(&self) -> &'static str {
        "8:orgid:00000000-0000-4000-8000-00000000000a"
    }
    fn message_picture(&self, src: String) -> Reply<Option<PictureBytes>> {
        Reply::ready(Ok(self.0.get(&src).cloned()))
    }
    fn reaction_animation(&self, _: String) -> Reply<Option<Strip>> {
        Reply::ready(Ok(None))
    }
    fn engine(&self) -> *const () {
        std::ptr::from_ref(self).cast()
    }
}

fn png(width: u32, height: u32) -> PictureBytes {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba([60, 90, 120, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
}

fn viewing(source: &PictureSource, names: &[&str], at: usize) -> Viewing {
    Viewing {
        source: source.clone(),
        at,
        pictures: names
            .iter()
            .map(|name| Viewed {
                src: (*name).to_owned(),
                message: format!("message-{name}"),
                sender: "Synthetic sender".into(),
                face: Face {
                    named: true,
                    picture: None,
                },
                time: 1_767_225_600_000,
            })
            .collect(),
    }
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

fn open(app: &TestAppContext, viewing: Viewing) -> (Entity<PictureViewer>, VisualTestContext) {
    app.update(|cx| {
        teams_ui::window::init(cx);
        picture_viewer::open(viewing, cx);
    });
    app.run_until_parked();
    let viewer = app.read(picture_viewer::shown).unwrap();
    let window = app.read(picture_viewer::window).unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), app);
    cx.simulate_resize(size(px(900.), px(700.)));
    draw(&mut cx);
    (viewer, cx)
}

#[test]
fn downloads_preserve_the_original_bytes_and_handle_cancel_and_write_failure() {
    let app = TestAppContext::single();
    let bytes = png(32, 24);
    let source = PictureSource::of(Arc::new(Pictures(HashMap::from([("image".to_owned(), bytes.clone())]))));
    let (viewer, mut cx) = open(&app, viewing(&source, &["image"], 0));
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("nested").join("image.png");
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::download));
    assert!(app.did_prompt_for_new_path());
    app.simulate_new_path_selection(|_| Some(destination.clone()));
    draw(&mut cx);
    assert_eq!(std::fs::read(&destination).unwrap(), bytes.bytes);
    app.background_executor.advance_clock(Duration::from_secs(5));
    draw(&mut cx);
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::download));
    app.simulate_new_path_selection(|_| None);
    draw(&mut cx);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    let blocker = directory.path().join("not-a-directory");
    std::fs::write(&blocker, b"synthetic blocker").unwrap();
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::download));
    app.simulate_new_path_selection(|_| Some(blocker.join("image.png")));
    draw(&mut cx);
    assert_eq!(std::fs::read(&blocker).unwrap(), b"synthetic blocker");
    assert!(
        cx.debug_bounds("picture-viewer-image").is_some(),
        "a failed download keeps the image open"
    );
}

#[test]
fn missing_and_corrupt_images_stay_navigable_and_reopening_reuses_the_viewer() {
    let app = TestAppContext::single();
    let corrupt = Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes: b"synthetic invalid PNG".to_vec(),
    });
    let source = PictureSource::of(Arc::new(Pictures(HashMap::from([
        ("good".to_owned(), png(32, 24)),
        ("corrupt".to_owned(), corrupt),
    ]))));
    let (viewer, mut cx) = open(&app, viewing(&source, &["good", "missing", "corrupt"], 0));
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::previous));
    assert_eq!(cx.update(|_, cx| viewer.read(cx).at()), (0, 3));
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::next));
    draw(&mut cx);
    assert!(cx.update(|_, cx| viewer.read(cx).natural().is_none()));
    assert!(cx.debug_bounds("picture-viewer-standing-in").is_some());
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::download));
    assert!(!app.did_prompt_for_new_path());
    cx.simulate_keystrokes("ctrl-= ctrl--");
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::next));
    draw(&mut cx);
    assert!(cx.update(|_, cx| viewer.read(cx).natural().is_none()));
    cx.update(|_, cx| viewer.update(cx, teams_ui::picture_viewer::PictureViewer::next));
    assert_eq!(cx.update(|_, cx| viewer.read(cx).at()), (2, 3));
    app.update(|cx| picture_viewer::open(viewing(&source, &["good"], 0), cx));
    draw(&mut cx);
    assert_eq!(app.read(picture_viewer::shown).unwrap(), viewer);
    assert_eq!(cx.update(|_, cx| viewer.read(cx).natural()), Some((32, 24)));
    app.update(|cx| picture_viewer::open(viewing(&source, &[], 0), cx));
    draw(&mut cx);
    assert_eq!(cx.update(|_, cx| viewer.read(cx).at()), (0, 1));
}

#[test]
fn pointer_zoom_and_drag_preserve_bounds_and_double_click_returns_to_fit() {
    let app = TestAppContext::single();
    let source = PictureSource::of(Arc::new(Pictures(HashMap::from([(
        "large".to_owned(),
        png(1600, 1200),
    )]))));
    let (viewer, mut cx) = open(&app, viewing(&source, &["large"], 0));
    let stage = cx.debug_bounds("picture-viewer-stage").unwrap();
    let centre = stage.center();
    cx.simulate_event(ScrollEvent {
        position: centre,
        delta: ScrollDelta::Lines(point(0., 1.)),
        modifiers: Modifiers {
            control: true,
            ..Modifiers::none()
        },
        ..ScrollEvent::default()
    });
    draw(&mut cx);
    assert!(cx.update(|_, cx| viewer.read(cx).placement().zoom.is_some()));
    cx.simulate_event(MouseDownEvent {
        position: centre,
        button: MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 2,
        first_mouse: false,
    });
    draw(&mut cx);
    cx.simulate_event(MouseDownEvent {
        position: centre,
        button: MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 2,
        first_mouse: false,
    });
    draw(&mut cx);
    assert_eq!(cx.update(|_, cx| viewer.read(cx).placement().zoom), Some(1.));
    cx.simulate_mouse_down(centre, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(centre + point(px(150.), px(80.)), MouseButton::Left, Modifiers::none());
    draw(&mut cx);
    assert_eq!(
        cx.update(|_, cx| viewer.read(cx).placement().pan),
        point(px(150.), px(80.))
    );
    cx.simulate_mouse_up(centre, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(centre, None, Modifiers::none());
    cx.simulate_keystrokes("ctrl-0");
    draw(&mut cx);
    assert!(cx.update(|_, cx| viewer.read(cx).placement().zoom.is_none()));
}
