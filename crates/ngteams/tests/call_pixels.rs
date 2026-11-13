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

#[test]
fn a_menu_inside_the_shares_vertical_span_is_still_counted() {
    for colour in [255, 41] {
        for (left, right) in [(280, 380), (350, 390), (5, 105)] {
            let mut after = shared_frame(32, None);
            for y in 60..180 {
                for x in left..right {
                    after.put_pixel(x, y, Rgb([colour; 3]));
                }
            }
            assert!(
                pixels::changed_still(&shared_frame(0, None), &after, WINDOW) >= 2_500,
                "menu colour {colour}, x {left}..{right}"
            );
        }
    }
}

#[test]
fn a_shared_screens_tall_white_bar_is_not_the_accounts_own_tile() {
    let window = Rect {
        x: 0,
        y: 0,
        w: 1280,
        h: 800,
    };
    let mut image = RgbImage::from_pixel(1280, 800, Rgb([40; 3]));
    let mut fill = |area: Rect, colour: [u8; 3]| {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                image.put_pixel(x.cast_unsigned(), y.cast_unsigned(), Rgb(colour));
            }
        }
    };
    fill(
        Rect {
            x: 10,
            y: 58,
            w: 1260,
            h: 732,
        },
        [235, 235, 240],
    );
    // the share, its bar wider in pixels than the strip's own tile, as at 1280x800 and larger
    fill(
        Rect {
            x: 18,
            y: 143,
            w: 1000,
            h: 562,
        },
        [64; 3],
    );
    fill(
        Rect {
            x: 25,
            y: 143,
            w: 92,
            h: 562,
        },
        [255; 3],
    );
    fill(
        Rect {
            x: 1026,
            y: 406,
            w: 236,
            h: 132,
        },
        [120; 3],
    );
    let own = Rect {
        x: 1026,
        y: 545,
        w: 236,
        h: 132,
    };
    fill(own, [255; 3]);
    let stage = pixels::stage(&image, window).unwrap();
    assert_eq!(stage.own, Some(own));
}
