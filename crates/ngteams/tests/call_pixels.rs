#![cfg(target_os = "linux")]

#[expect(
    dead_code,
    reason = "the shared screenshot reader exposes the other call-window measurements too"
)]
#[path = "support/linux/call_pixels.rs"]
mod pixels;

use image::{Rgb, RgbImage};
use pixels::Rect;

const WINDOW: Rect = Rect {
    x: 0,
    y: 0,
    w: 400,
    h: 240,
};

fn shared_frame(bar: u32, menu: Option<u8>) -> RgbImage {
    let mut image = RgbImage::from_pixel(400, 240, Rgb([220, 230, 245]));
    for y in 40..220 {
        for x in 20..340 {
            let light = (bar..bar + 16).contains(&(x - 20));
            image.put_pixel(x, y, Rgb([if light { 235 } else { 64 }; 3]));
        }
    }
    if let Some(colour) = menu {
        for y in 20..120 {
            for x in 280..380 {
                image.put_pixel(x, y, Rgb([colour; 3]));
            }
        }
    }
    image
}

#[test]
fn a_shared_images_moving_bar_at_either_edge_is_not_an_unclosed_menu() {
    for (before, after) in [(0, 32), (304, 272), (32, 0), (272, 304)] {
        assert_eq!(
            pixels::changed_still(&shared_frame(before, None), &shared_frame(after, None), WINDOW),
            0,
            "bar from {before} to {after}"
        );
    }
}

#[test]
fn a_menu_left_open_is_still_counted_beside_the_moving_shared_image() {
    for colour in [255, 30] {
        assert!(pixels::changed_still(&shared_frame(0, None), &shared_frame(32, Some(colour)), WINDOW) >= 4_000);
    }
}
