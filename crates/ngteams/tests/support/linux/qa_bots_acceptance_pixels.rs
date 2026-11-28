//! Where a card's field and buttons, and an app's dialog, are on a Teams Light screenshot: by their colours, as the
//! cards' and the message extensions' QA tests find them.

use std::path::Path;

use ui_sandbox::SandboxResult;

const SURFACE: [u8; 3] = [255, 255, 255];
const BORDER: [u8; 3] = [224, 224, 224];
const FILL: [u8; 3] = [243, 244, 250];
const OFF_FILL: [u8; 3] = [243, 243, 243];
/// The focused field's ring (Teams Light's brand).
const RING: [u8; 3] = [91, 95, 199];
/// Where the chat's pane starts on the 1280×800 screen.
const PANE_X: u32 = 380;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    pub const fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    pub const fn centre(&self) -> (u32, u32) {
        (self.x0 + self.width() / 2, self.y0 + (self.y1 - self.y0) / 2)
    }

    const fn holds(&self, other: &Self) -> bool {
        other.x0 + 10 >= self.x0 && other.x1 <= self.x1 + 10 && other.y0 >= self.y0 && other.y1 <= self.y1
    }
}

pub fn load(shot: &Path) -> SandboxResult<image::RgbImage> {
    Ok(image::open(shot)?.to_rgb8())
}

fn near(pixel: [u8; 3], wanted: [u8; 3], within: u8) -> bool {
    pixel.iter().zip(wanted).all(|(got, want)| got.abs_diff(want) <= within)
}

fn at(shot: &image::RgbImage, x: u32, y: u32) -> [u8; 3] {
    if x < shot.width() && y < shot.height() {
        shot.get_pixel(x, y).0
    } else {
        [0, 0, 0]
    }
}

/// The runs of row `y` from `from` to `to` whose pixels `taken` takes.
fn runs(shot: &image::RgbImage, y: u32, from: u32, to: u32, taken: impl Fn([u8; 3]) -> bool) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    let mut x = from;
    let to = to.min(shot.width());
    while x < to {
        if !taken(at(shot, x, y)) {
            x += 1;
            continue;
        }
        let start = x;
        while x < to && taken(at(shot, x, y)) {
            x += 1;
        }
        found.push((start, x));
    }
    found
}

/// The lowest card in the chat's pane: a top border, a run of [`BORDER`] 120 to 470 px long over [`SURFACE`], down
/// to its bottom border; none inside another.
pub fn lowest_card(shot: &image::RgbImage) -> Option<Rect> {
    let mut found = Vec::new();
    for y in 0..shot.height().saturating_sub(4) {
        for (x0, x1) in runs(shot, y, PANE_X, shot.width(), |pixel| near(pixel, BORDER, 3)) {
            let mid = x0 + (x1 - x0) / 2;
            if !(120..=470).contains(&(x1 - x0)) || at(shot, mid, y + 2) != SURFACE {
                continue;
            }
            if let Some(bottom) = (y + 2..shot.height()).find(|&below| near(at(shot, x0 + 4, below), BORDER, 3)) {
                found.push(Rect {
                    x0: x0.saturating_sub(8),
                    y0: y,
                    x1: x1 + 8,
                    y1: bottom + 1,
                });
            }
        }
    }
    found
        .iter()
        .copied()
        .filter(|card| !found.iter().any(|other| other != card && other.holds(card)))
        .max_by_key(|card| card.y0)
}

/// A field's edge: its [`BORDER`], or the [`RING`] of the focused one.
fn edge(pixel: [u8; 3]) -> bool {
    near(pixel, BORDER, 4) || near(pixel, RING, 4)
}

/// The fields and buttons in `region`: boxes with an [`edge`] and a [`FILL`] (or an [`OFF_FILL`]) inside, in order
/// down the screen, then across.
pub fn boxes_in(shot: &image::RgbImage, region: Rect) -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    for y in region.y0..region.y1.min(shot.height()).saturating_sub(3) {
        for (x0, x1) in runs(shot, y, region.x0, region.x1, edge) {
            let mid = x0 + (x1 - x0) / 2;
            let inside = at(shot, mid, y + 2);
            if x1 - x0 < 30 || !(near(inside, FILL, 3) || near(inside, OFF_FILL, 2)) {
                continue;
            }
            let known = |it: &Rect| it.x0 <= mid && mid < it.x1 && it.y0 <= y && y < it.y1;
            if found.iter().any(known) {
                continue;
            }
            let Some(bottom) = (y + 2..region.y1.min(shot.height())).find(|&below| edge(at(shot, mid, below))) else {
                continue;
            };
            found.push(Rect {
                x0,
                y0: y,
                x1,
                y1: bottom + 1,
            });
        }
    }
    found.sort_by_key(|found| (found.y0, found.x0));
    found
}

/// The bounds of the Teams Light surface: an open dialog over the dimmed window.
pub fn surface_box(shot: &image::RgbImage) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in 0..shot.height() {
        for (x0, x1) in runs(shot, y, 0, shot.width(), |pixel| pixel == SURFACE) {
            if x1 - x0 < 100 {
                continue;
            }
            bounds = Some(bounds.map_or(
                Rect {
                    x0,
                    y0: y,
                    x1,
                    y1: y + 1,
                },
                |known| Rect {
                    x0: known.x0.min(x0),
                    y0: known.y0,
                    x1: known.x1.max(x1),
                    y1: y + 1,
                },
            ));
        }
    }
    bounds
}
