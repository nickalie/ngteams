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
    h: 300,
};
const BELOW: Rect = Rect {
    y: 58,
    h: 242,
    ..WINDOW
};

fn fill(image: &mut RgbImage, area: Rect, colour: [u8; 3]) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            image.put_pixel(x.cast_unsigned(), y.cast_unsigned(), Rgb(colour));
        }
    }
}

fn shared_frame(bar: i32, menu: Option<u8>) -> RgbImage {
    let mut image = RgbImage::from_pixel(400, 300, Rgb([40; 3]));
    fill(
        &mut image,
        Rect {
            x: 10,
            y: 58,
            w: 380,
            h: 232,
        },
        [243, 244, 250],
    );
    fill(
        &mut image,
        Rect {
            x: 18,
            y: 106,
            w: 252,
            h: 142,
        },
        [64; 3],
    );
    fill(
        &mut image,
        Rect {
            x: 18 + bar,
            y: 106,
            w: 16,
            h: 142,
        },
        [255; 3],
    );
    fill(
        &mut image,
        Rect {
            x: 280,
            y: 135,
            w: 100,
            h: 56,
        },
        [120; 3],
    );
    fill(
        &mut image,
        Rect {
            x: 278,
            y: 198,
            w: 104,
            h: 60,
        },
        [120; 3],
    );
    fill(
        &mut image,
        Rect {
            x: 280,
            y: 200,
            w: 100,
            h: 56,
        },
        [255; 3],
    );
    if let Some(colour) = menu {
        fill(
            &mut image,
            Rect {
                x: 100,
                y: 62,
                w: 220,
                h: 100,
            },
            [colour; 3],
        );
    }
    image
}

#[test]
fn moving_share_cannot_open_a_menu_and_escape_must_restore_the_still_band() {
    for (start, end) in [(0, 32), (236, 204), (32, 0), (204, 236)] {
        let before = shared_frame(start, None);
        let picture = pixels::shared_picture(&before, WINDOW).unwrap();
        let band = pixels::still_below_toolbar(BELOW, Some(picture)).unwrap();
        assert!(band.bottom() < 106);
        assert_eq!(
            pixels::changed_surface_pixels(&before, &shared_frame(end, None), band),
            0
        );
        for colour in [255, 30, 41] {
            let open = shared_frame(end, Some(colour));
            assert!(pixels::changed_surface_pixels(&before, &open, band) >= 1_000);
            let closed = shared_frame(end, None);
            assert!(pixels::changed_surface_pixels(&open, &closed, band) >= 1_000);
            assert_eq!(pixels::changed_surface_pixels(&before, &closed, band), 0);
            assert!(pixels::changed_surface_pixels(&before, &open, band) > 40);
        }
    }
}

#[test]
fn a_missing_share_or_own_tile_is_not_ready_for_toolbar_checks() {
    let mut image = shared_frame(32, None);
    fill(
        &mut image,
        Rect {
            x: 18,
            y: 106,
            w: 252,
            h: 142,
        },
        [243, 244, 250],
    );
    assert!(pixels::shared_picture(&image, WINDOW).is_none());
    let mut image = shared_frame(32, None);
    fill(
        &mut image,
        Rect {
            x: 280,
            y: 200,
            w: 100,
            h: 56,
        },
        [120; 3],
    );
    assert!(pixels::shared_picture(&image, WINDOW).is_none());
    assert!(pixels::still_below_toolbar(BELOW, Some(Rect { y: 60, ..WINDOW })).is_none());
    assert_eq!(pixels::still_below_toolbar(BELOW, None), Some(BELOW));
}

#[test]
fn a_delayed_title_redraw_cannot_identify_the_wrong_toolbar_button() {
    let band = Rect { h: 58, ..WINDOW };
    let button = Rect {
        x: 300,
        y: 4,
        w: 32,
        h: 44,
    };
    let before = RgbImage::from_pixel(400, 300, Rgb([30, 40, 150]));
    let mut on = before.clone();
    fill(
        &mut on,
        Rect {
            x: 10,
            y: 10,
            w: 200,
            h: 18,
        },
        [255; 3],
    );
    let off = on.clone();
    assert!(pixels::toggled_button(&before, &on, &off, band).is_none());
    fill(&mut on, button, [60, 90, 200]);
    assert_eq!(pixels::toggled_button(&before, &on, &off, band), Some(button));
}

#[test]
fn the_share_readiness_wait_also_recognizes_the_dark_theme_picture() {
    let mut image = shared_frame(0, None);
    for pixel in image.pixels_mut() {
        if pixel.0 == [243, 244, 250] {
            *pixel = Rgb([31; 3]);
        }
    }
    fill(
        &mut image,
        Rect {
            x: 278,
            y: 198,
            w: 104,
            h: 60,
        },
        [61; 3],
    );
    fill(
        &mut image,
        Rect {
            x: 280,
            y: 200,
            w: 100,
            h: 56,
        },
        [41; 3],
    );
    assert!(pixels::shared_picture(&image, WINDOW).is_some());
    fill(
        &mut image,
        Rect {
            x: 18,
            y: 106,
            w: 252,
            h: 142,
        },
        [31; 3],
    );
    assert!(pixels::shared_picture(&image, WINDOW).is_none());
}

fn full_hd_frame(picture: Rect, bar: i32) -> RgbImage {
    let mut image = RgbImage::from_pixel(1920, 1080, Rgb([40; 3]));
    fill(
        &mut image,
        Rect {
            x: 10,
            y: 58,
            w: 1900,
            h: 1012,
        },
        [243, 244, 250],
    );
    fill(&mut image, picture, [64; 3]);
    fill(
        &mut image,
        Rect {
            x: picture.x + bar,
            w: picture.w * 16 / 320,
            ..picture
        },
        [255; 3],
    );
    fill(
        &mut image,
        Rect {
            x: 1688,
            y: 798,
            w: 216,
            h: 124,
        },
        [120; 3],
    );
    fill(
        &mut image,
        Rect {
            x: 1690,
            y: 800,
            w: 212,
            h: 120,
        },
        [255; 3],
    );
    image
}

#[test]
fn the_mixers_180p_bar_scaled_to_full_hd_cannot_leave_a_menu_residual() {
    let window = Rect {
        w: 1920,
        h: 1080,
        ..WINDOW
    };
    let picture = Rect {
        x: 22,
        y: 96,
        w: 1660,
        h: 934,
    };
    let bar_width = picture.w * 16 / 320;
    for start in [0, picture.w - bar_width] {
        let before = full_hd_frame(picture, start);
        let detected = pixels::shared_picture(&before, window).unwrap();
        let below = Rect {
            y: 58,
            h: 1022,
            ..window
        };
        let band = pixels::still_below_toolbar(below, Some(detected)).unwrap();
        let closed = full_hd_frame(picture, 320);
        assert!(pixels::changed_surface_pixels(&before, &closed, below) > 100_000);
        assert_eq!(pixels::changed_surface_pixels(&before, &closed, band), 0);
        let mut open = closed.clone();
        fill(
            &mut open,
            Rect {
                x: 1300,
                y: 62,
                w: 280,
                h: 100,
            },
            [255; 3],
        );
        assert!(pixels::changed_surface_pixels(&before, &open, band) >= 1_000);
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
