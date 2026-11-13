//! What a user sees of the call window, read from its screenshots (Teams Light): the stage's panel, the tiles on it,
//! the account's own tile (white while its camera is off), the pager `< n/N >`, and what changed between two shots.
//! The app tells no screen positions, so the harness finds what it clicks the way a user's eye does.

use image::{Rgb, RgbImage};

/// A rectangle of the screen or of a shot, px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(self) -> i32 {
        self.y + self.h
    }

    pub fn center(self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn area(self) -> i64 {
        i64::from(self.w) * i64::from(self.h)
    }

    /// `inner` lies in this one, give or take `slack` px.
    pub fn holds(self, inner: Self, slack: i32) -> bool {
        inner.x >= self.x - slack
            && inner.y >= self.y - slack
            && inner.right() <= self.right() + slack
            && inner.bottom() <= self.bottom() + slack
    }

    pub fn union(self, other: Self) -> Self {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Self {
            x,
            y,
            w: self.right().max(other.right()) - x,
            h: self.bottom().max(other.bottom()) - y,
        }
    }
}

/// A connected blob of marked pixels: its bounds and how many pixels it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blob {
    pub bounds: Rect,
    pub pixels: i64,
}

fn distance(a: Rgb<u8>, b: Rgb<u8>) -> i32 {
    a.0.iter()
        .zip(b.0.iter())
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs())
        .sum()
}

fn luminance(pixel: Rgb<u8>) -> i32 {
    (i32::from(pixel.0[0]) * 299 + i32::from(pixel.0[1]) * 587 + i32::from(pixel.0[2]) * 114) / 1000
}

fn pixel(image: &RgbImage, x: i32, y: i32) -> Option<Rgb<u8>> {
    let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
    (x < image.width() && y < image.height()).then(|| *image.get_pixel(x, y))
}

const NEIGHBOURS: [(isize, isize); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

/// The 8-connected blobs of the pixels in `within` that `marked` takes.
pub fn blobs(within: Rect, marked: impl Fn(i32, i32) -> bool) -> Vec<Blob> {
    let (w, h) = (
        usize::try_from(within.w).unwrap_or(0),
        usize::try_from(within.h).unwrap_or(0),
    );
    let mut mask = vec![false; w * h];
    for (index, cell) in mask.iter_mut().enumerate() {
        let (x, y) = (index % w, index / w);
        *cell = marked(within.x + to_i32(x), within.y + to_i32(y));
    }
    let mut found = Vec::new();
    let mut stack = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] {
            continue;
        }
        mask[start] = false;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1, mut pixels) = (usize::MAX, usize::MAX, 0, 0, 0_i64);
        while let Some(at) = stack.pop() {
            let (x, y) = (at % w, at / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            pixels += 1;
            for (dx, dy) in NEIGHBOURS {
                let next = x
                    .checked_add_signed(dx)
                    .zip(y.checked_add_signed(dy))
                    .filter(|(nx, ny)| *nx < w && *ny < h)
                    .map(|(nx, ny)| ny * w + nx);
                if let Some(next) = next.filter(|next| mask[*next]) {
                    mask[next] = false;
                    stack.push(next);
                }
            }
        }
        found.push(Blob {
            bounds: Rect {
                x: within.x + to_i32(x0),
                y: within.y + to_i32(y0),
                w: to_i32(x1 - x0 + 1),
                h: to_i32(y1 - y0 + 1),
            },
            pixels,
        });
    }
    found
}

fn to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

/// The blobs of the pixels in `within` that differ between `before` and `after`.
pub fn changes(before: &RgbImage, after: &RgbImage, within: Rect) -> Vec<Blob> {
    blobs(within, |x, y| match (pixel(before, x, y), pixel(after, x, y)) {
        (Some(a), Some(b)) => distance(a, b) > 48,
        _ => false,
    })
}

/// How many pixels in `within` differ between `before` and `after`.
pub fn changed_pixels(before: &RgbImage, after: &RgbImage, within: Rect) -> i64 {
    changes(before, after, within).iter().map(|blob| blob.pixels).sum()
}

/// How many pixels in `within` differ between `before` and `after`, leaving out the moving picture `before` shows
/// (a shared screen: its dark grey areas), which changes on its own.
pub fn changed_still(before: &RgbImage, after: &RgbImage, within: Rect) -> i64 {
    let video = picture_bounds(before, within).map(|before| {
        // Recover only the pattern's 16px bar at its 720px source height, never a menu-sized extension.
        let edge_slack = 16.max(before.h * 16 / 720) + 6;
        let after = picture_bounds(after, within).filter(|after| {
            before.y.abs_diff(after.y) <= 6
                && before.bottom().abs_diff(after.bottom()) <= 6
                && before.x.abs_diff(after.x) <= edge_slack.cast_unsigned()
                && before.right().abs_diff(after.right()) <= edge_slack.cast_unsigned()
        });
        // The other frame reveals an edge hidden by the moving bright bar.
        let bounds = after.map_or(before, |after| before.union(after));
        Rect {
            x: bounds.x - 6,
            y: bounds.y - 6,
            w: bounds.w + 12,
            h: bounds.h + 12,
        }
    });
    blobs(within, |x, y| {
        let moving = video.is_some_and(|video| video.holds(Rect { x, y, w: 1, h: 1 }, 0));
        !moving
            && match (pixel(before, x, y), pixel(after, x, y)) {
                (Some(a), Some(b)) => distance(a, b) > 48,
                _ => false,
            }
    })
    .iter()
    .map(|blob| blob.pixels)
    .sum()
}

fn picture_bounds(image: &RgbImage, within: Rect) -> Option<Rect> {
    // dark and grey, inside the frame (the window's frame is a dark blue)
    let grey_dark = |colour: Rgb<u8>| {
        let (low, high) = (colour.0.iter().min(), colour.0.iter().max());
        luminance(colour) < 90 && high.zip(low).is_some_and(|(high, low)| high - low < 30)
    };
    blobs(within, |x, y| pixel(image, x, y).is_some_and(grey_dark))
        .into_iter()
        .filter(|blob| blob.bounds.w >= 12 && blob.bounds.h >= 60)
        .filter(|blob| {
            let inner = Rect {
                x: within.x + 4,
                y: within.y + 4,
                w: within.w - 8,
                h: within.h - 8,
            };
            inner.holds(blob.bounds, 0)
        })
        // its bars split it: all of its dark parts
        .map(|blob| blob.bounds)
        .reduce(Rect::union)
}

/// The pager under the gallery, beside the speaker's strip or under the strip beside a share: `<`, its `n/N`, `>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pager {
    pub previous: Rect,
    pub text: Rect,
    pub next: Rect,
}

/// What the stage shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    /// The stage's light panel, in the window.
    pub panel: Rect,
    /// Every tile drawn on it (the account's own among them), the large speaker's or share's too.
    pub tiles: Vec<Rect>,
    /// The account's own tile: white while its camera is off.
    pub own: Option<Rect>,
    pub pager: Option<Pager>,
}

impl Stage {
    /// The tiles in reading order: by rows (tiles whose tops are within a few px share a row), then left to right.
    pub fn reading_order(&self) -> Vec<Rect> {
        let mut tiles = self.tiles.clone();
        tiles.sort_by_key(|tile| (tile.y / 8, tile.x));
        let mut rows: Vec<Vec<Rect>> = Vec::new();
        for tile in tiles {
            match rows.last_mut() {
                Some(row) if (row[0].y - tile.y).abs() <= 6 => row.push(tile),
                _ => rows.push(vec![tile]),
            }
        }
        rows.into_iter()
            .flat_map(|mut row| {
                row.sort_by_key(|tile| tile.x);
                row
            })
            .collect()
    }
}

/// The stage's light panel inside the window's frame: its sides where a row near the window's bottom turns light, its
/// top and bottom where a column just inside its left side does; with its colour, the most common one along that row.
fn panel(image: &RgbImage, window: Rect) -> Option<(Rect, Rgb<u8>)> {
    let light = |x: i32, y: i32| pixel(image, x, y).is_some_and(|colour| luminance(colour) > 200);
    let row = window.bottom() - 14;
    let left = (window.x..window.right()).find(|x| light(*x, row))?;
    let right = (window.x..window.right()).rev().find(|x| light(*x, row))? + 1;
    let column = left + 4;
    let top = (window.y..=row).rev().take_while(|y| light(column, *y)).last()?;
    let bottom = (row..window.bottom()).take_while(|y| light(column, *y)).last()? + 1;
    let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    for y in row - 3..=row + 3 {
        for x in left + 4..right - 4 {
            if let Some(colour) = pixel(image, x, y) {
                *counts.entry(colour.0).or_default() += 1;
            }
        }
    }
    let colour = counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(colour, _)| Rgb(colour))?;
    let rect = Rect {
        x: left,
        y: top,
        w: right - left,
        h: bottom - top,
    };
    Some((rect, colour))
}

/// Reads the stage of the call window at `window` in `image` (Teams Light).
pub fn stage(image: &RgbImage, window: Rect) -> Option<Stage> {
    let (panel, background) = panel(image, window)?;
    let inside = Rect {
        x: panel.x + 2,
        y: panel.y + 2,
        w: panel.w - 4,
        h: panel.h - 4,
    };
    let content = blobs(inside, |x, y| {
        pixel(image, x, y).is_some_and(|colour| distance(colour, background) > 30)
    });
    let tiles: Vec<Rect> = content
        .iter()
        .map(|blob| blob.bounds)
        .filter(|bounds| bounds.w >= 60 && bounds.h >= 34)
        .collect();
    let own = blobs(inside, |x, y| {
        pixel(image, x, y).is_some_and(|colour| colour.0.iter().all(|channel| *channel >= 250))
    })
    .into_iter()
    // landscape, as every own tile is: a shared screen's white bar runs its picture's full height
    .filter(|blob| blob.bounds.w >= 60 && blob.bounds.h >= 34 && blob.bounds.w > blob.bounds.h)
    .max_by_key(|blob| blob.pixels)
    .map(|blob| blob.bounds);
    // marks on the panel itself, not in a tile: the panel's colour round them
    let plain = |x: i32, y: i32| pixel(image, x, y).is_some_and(|colour| distance(colour, background) <= 8);
    let on_panel = |mark: &Rect| {
        let ring = Rect {
            x: mark.x - 3,
            y: mark.y - 3,
            w: mark.w + 6,
            h: mark.h + 6,
        };
        // most of it: the next glyph of the pager's text may come within the ring
        let around: Vec<bool> = (ring.x..ring.right())
            .flat_map(|x| [plain(x, ring.y), plain(x, ring.bottom() - 1)])
            .chain((ring.y..ring.bottom()).flat_map(|y| [plain(ring.x, y), plain(ring.right() - 1, y)]))
            .collect();
        around.iter().filter(|plain| **plain).count() * 5 >= around.len() * 4
    };
    let small: Vec<Rect> = content
        .iter()
        .map(|blob| blob.bounds)
        .filter(|bounds| bounds.w < 60 && bounds.h < 34)
        .filter(on_panel)
        .collect();
    Some(Stage {
        panel,
        own: own.filter(|own| tiles.iter().any(|tile| tile.holds(*own, 2))),
        pager: pager(&small),
        tiles,
    })
}

/// The pager among the small marks on the panel: a row of marks no taller than a line of text, narrow ones (the
/// chevrons) at both ends with text between.
fn pager(small: &[Rect]) -> Option<Pager> {
    let mut marks: Vec<Rect> = small.iter().copied().filter(|mark| mark.h <= 18).collect();
    marks.sort_by_key(|mark| (mark.center().1, mark.x));
    let mut rows: Vec<Vec<Rect>> = Vec::new();
    for mark in marks {
        match rows
            .iter_mut()
            .find(|row| (row[0].center().1 - mark.center().1).abs() <= 5)
        {
            Some(row) => row.push(mark),
            None => rows.push(vec![mark]),
        }
    }
    rows.into_iter().find_map(|mut row| {
        row.sort_by_key(|mark| mark.x);
        // the row's runs of marks no more than 24 px apart
        let mut runs: Vec<Vec<Rect>> = Vec::new();
        for mark in row {
            match runs.last_mut() {
                Some(run) if mark.x - run.last().map_or(0, |last| last.right()) <= 24 => run.push(mark),
                _ => runs.push(vec![mark]),
            }
        }
        runs.into_iter().find_map(|run| {
            let (first, last) = (*run.first()?, *run.last()?);
            let narrow = |mark: Rect| mark.w <= 10 && (5..=14).contains(&mark.h);
            (run.len() >= 3 && narrow(first) && narrow(last) && last.right() - first.x <= 180).then(|| {
                let text = run[1..run.len() - 1]
                    .iter()
                    .copied()
                    .reduce(Rect::union)
                    .unwrap_or(first);
                Pager {
                    previous: first,
                    text,
                    next: last,
                }
            })
        })
    })
}

/// The pixels of Teams' red (Leave) in `within`: their largest blob.
pub fn red(image: &RgbImage, within: Rect) -> Option<Rect> {
    blobs(within, |x, y| {
        pixel(image, x, y).is_some_and(|Rgb([r, g, b])| r > 170 && g < 90 && b < 110)
    })
    .into_iter()
    .max_by_key(|blob| blob.pixels)
    .map(|blob| blob.bounds)
}

/// `image` cut to `rect`.
pub fn crop(image: &RgbImage, rect: Rect) -> RgbImage {
    image::imageops::crop_imm(
        image,
        u32::try_from(rect.x).unwrap_or(0),
        u32::try_from(rect.y).unwrap_or(0),
        u32::try_from(rect.w).unwrap_or(0),
        u32::try_from(rect.h).unwrap_or(0),
    )
    .to_image()
}

/// Where `part` (a call window's own shot) lies in `whole` (the screen's), if anywhere: the offset at which a spread of
/// its frame's and toolbar's pixels match.
pub fn find(whole: &RgbImage, part: &RgbImage) -> Option<Rect> {
    let (w, h) = (i32::try_from(part.width()).ok()?, i32::try_from(part.height()).ok()?);
    let (sw, sh) = (i32::try_from(whole.width()).ok()?, i32::try_from(whole.height()).ok()?);
    // on the window's frame and toolbar: the stage may play video between the two shots
    let samples: Vec<(i32, i32)> = (0..16)
        .flat_map(|step| {
            let (x, y) = (w * step / 16 + 2, h * step / 16 + 2);
            [(x, 3), (x, 18), (x, 28), (x, 40), (x, h - 3), (3, y), (w - 4, y)]
        })
        .collect();
    // a few may differ: the title's call timer ticks between the two shots
    let allowed = samples.len() / 16;
    (0..=sh - h)
        .flat_map(|y| (0..=sw - w).map(move |x| (x, y)))
        .find(|(x, y)| {
            let mut misses = 0;
            samples.iter().all(|(px, py)| {
                let same = match (pixel(whole, x + px, y + py), pixel(part, *px, *py)) {
                    (Some(a), Some(b)) => distance(a, b) <= 12,
                    _ => false,
                };
                misses += usize::from(!same);
                misses <= allowed
            })
        })
        .map(|(x, y)| Rect { x, y, w, h })
}

/// The toolbar's buttons in `band` (white icons and captions on the window's blue frame), left to right: the runs of
/// columns holding white, no more than 6 px apart, each at least 6 px wide.
pub fn white_groups(image: &RgbImage, band: Rect) -> Vec<Rect> {
    let white = |x: i32| {
        (band.y..band.bottom())
            .any(|y| pixel(image, x, y).is_some_and(|colour| colour.0.iter().all(|channel| *channel > 215)))
    };
    let mut groups: Vec<Rect> = Vec::new();
    for x in band.x..band.right() {
        if !white(x) {
            continue;
        }
        match groups.last_mut() {
            Some(last) if x - last.right() <= 6 => last.w = x + 1 - last.x,
            _ => groups.push(Rect {
                x,
                y: band.y,
                w: 1,
                h: band.h,
            }),
        }
    }
    groups.retain(|group| group.w >= 6);
    groups
}

/// How far a control's ink stands out from its ground in `rect`: its brightest pixels' luminance (the 98th
/// percentile, white icons and captions) less the median's (the ground under them).
pub fn ink_contrast(image: &RgbImage, rect: Rect) -> i32 {
    let mut seen: Vec<i32> = (rect.y..rect.bottom())
        .flat_map(|y| (rect.x..rect.right()).map(move |x| (x, y)))
        .filter_map(|(x, y)| pixel(image, x, y).map(luminance))
        .collect();
    if seen.is_empty() {
        return 0;
    }
    seen.sort_unstable();
    let at = |share: usize| seen[(seen.len() - 1) * share / 100];
    at(98) - at(50)
}

/// The bounds of the dark pixels in `within` (a shared screen: the mock's is dark grey and black).
pub fn dark_bounds(image: &RgbImage, within: Rect) -> Option<Rect> {
    blobs(within, |x, y| {
        pixel(image, x, y).is_some_and(|colour| luminance(colour) < 90)
    })
    .into_iter()
    .filter(|blob| blob.bounds.w >= 12 && blob.bounds.h >= 12)
    .map(|blob| blob.bounds)
    .reduce(Rect::union)
}

/// The name labels in `within`: white boxes a line of text tall, as a tile's or a share's name sits in.
pub fn labels(image: &RgbImage, within: Rect) -> Vec<Rect> {
    blobs(within, |x, y| {
        pixel(image, x, y).is_some_and(|colour| luminance(colour) > 245)
    })
    .into_iter()
    .map(|blob| blob.bounds)
    .filter(|bounds| (14..=30).contains(&bounds.h) && (30..=250).contains(&bounds.w))
    .collect()
}
