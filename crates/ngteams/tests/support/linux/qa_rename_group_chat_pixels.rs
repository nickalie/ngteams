//! What the group chat rename QA reads in screenshots, as a user's eye does: the chat header's marks (its avatar, its
//! name, the pencil after the name, the actions far right), the popup's filled Save button, and a picture of the
//! pencil found again. A border, a 1–2 px line or a focus ring is never taken for a mark or a button: marks narrower
//! than 3 px are dropped, and a button must be a filled box.

use image::RgbImage;

/// An sRGB colour.
pub type Rgb = [u8; 3];

/// How far apart two colours may be (the sum over the channels) and still count as one.
const SAME: u32 = 30;
/// How far a pixel must be from the header's background to be ink.
const INK: u32 = 60;
/// The least gap between two of the header's marks, px: less is a gap inside one name.
const GAP: u32 = 6;
/// The rail's width, px: the conversation pane starts right of it at the least.
const RAIL: u32 = 76;
/// The title bar's least height, px.
const TITLE: u32 = 48;

/// A box on the screen (inclusive edges) and how many of its pixels are taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blob {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub pixels: u32,
}

impl Blob {
    pub fn centre(self) -> (u32, u32) {
        (u32::midpoint(self.x0, self.x1), u32::midpoint(self.y0, self.y1))
    }

    pub fn width(self) -> u32 {
        self.x1 - self.x0 + 1
    }

    pub fn height(self) -> u32 {
        self.y1 - self.y0 + 1
    }
}

/// `"rrggbb"` as a colour.
pub fn rgb_of(hex: &str) -> Option<Rgb> {
    let value = u32::from_str_radix(hex, 16).ok()?;
    let [_, r, g, b] = value.to_be_bytes();
    Some([r, g, b])
}

fn distance(a: Rgb, b: Rgb) -> u32 {
    a.iter().zip(b).map(|(a, b)| u32::from(a.abs_diff(b))).sum()
}

fn at(image: &RgbImage, x: u32, y: u32) -> Rgb {
    image.get_pixel_checked(x, y).map_or([0, 0, 0], |pixel| pixel.0)
}

/// The conversation pane's left edge: right of the list pane's border (a line of `border` down the window), or the
/// rail's edge when no list pane shows.
fn pane_left(image: &RgbImage, border: Rgb) -> u32 {
    let (width, height) = image.dimensions();
    let rows: Vec<u32> = (TITLE + 60..height.saturating_sub(60)).step_by(8).collect();
    (RAIL + 40..width.saturating_sub(100))
        .find(|&x| {
            let on = rows
                .iter()
                .filter(|&&y| distance(at(image, x, y), border) <= 12)
                .count();
            !rows.is_empty() && on * 10 >= rows.len() * 9
        })
        .map_or(RAIL, |x| x + 1)
}

/// The chat header's band (top, bottom) in the column `x`: from under the title bar to over the header's bottom
/// border, both edges left out.
fn header_band(image: &RgbImage, x: u32, border: Rgb) -> Option<(u32, u32)> {
    let bottom = (TITLE + 16..TITLE + 160).find(|&y| distance(at(image, x, y), border) <= 12)?;
    let background = at(image, x, bottom - 3);
    let mut top = bottom - 3;
    while top > 0 && distance(at(image, x, top - 1), background) <= SAME {
        top -= 1;
    }
    (bottom - top >= 24).then_some((top + 3, bottom - 3))
}

/// The header's marks, left to right: runs of ink columns at least [`GAP`] apart, each at least 3 px wide.
fn header_marks(image: &RgbImage, border: Rgb) -> Option<Vec<Blob>> {
    let left = pane_left(image, border);
    let (top, bottom) = header_band(image, left + 4, border)?;
    let background = at(image, left + 4, top);
    let width = image.width();
    let ink = |x: u32| (top..=bottom).any(|y| distance(at(image, x, y), background) > INK);
    let mut marks: Vec<Blob> = Vec::new();
    let mut run: Option<(u32, u32)> = None;
    for x in left + 3..width.saturating_sub(3) {
        if ink(x) {
            run = Some(run.map_or((x, x), |(start, _)| (start, x)));
            continue;
        }
        if let Some((start, end)) = run
            && x - end > GAP
        {
            marks.push(mark(image, (start, end), (top, bottom), background));
            run = None;
        }
    }
    if let Some((start, end)) = run {
        marks.push(mark(image, (start, end), (top, bottom), background));
    }
    Some(marks.into_iter().filter(|mark| mark.width() >= 3).collect())
}

/// The ink's box in the columns `start..=end` of the band `top..=bottom`.
fn mark(image: &RgbImage, (start, end): (u32, u32), (top, bottom): (u32, u32), background: Rgb) -> Blob {
    let mut blob = Blob {
        x0: start,
        y0: bottom,
        x1: end,
        y1: top,
        pixels: 0,
    };
    for y in top..=bottom {
        for x in start..=end {
            if distance(at(image, x, y), background) > INK {
                blob.y0 = blob.y0.min(y);
                blob.y1 = blob.y1.max(y);
                blob.pixels += 1;
            }
        }
    }
    blob
}

/// The pencil after the chat's name: the mark right before the widest gap of the header (the actions sit far right).
pub fn pencil(image: &RgbImage, border: Rgb) -> Option<Blob> {
    let marks = header_marks(image, border)?;
    if marks.len() < 3 {
        return None;
    }
    let widest = marks
        .windows(2)
        .enumerate()
        .max_by_key(|(_, pair)| pair[1].x0 - pair[0].x1)
        .map(|(index, _)| index)?;
    // past the avatar and the name at the least
    (widest >= 2).then(|| marks[widest])
}

/// `blob`'s pixels.
pub fn crop(image: &RgbImage, blob: Blob) -> RgbImage {
    image::imageops::crop_imm(image, blob.x0, blob.y0, blob.width(), blob.height()).to_image()
}

/// Where `picture` shows in the band under the title bar, right of the rail (within a few levels per channel).
pub fn find_in_header(image: &RgbImage, picture: &RgbImage) -> Option<(u32, u32)> {
    let (width, height) = image.dimensions();
    let (w, h) = picture.dimensions();
    let most = u64::from(w) * u64::from(h) * 3 * 6;
    let bottom = height.min(TITLE + 160).saturating_sub(h);
    for y in TITLE..bottom {
        for x in RAIL..width.saturating_sub(w) {
            if differs_within(image, picture, (x, y), most) {
                return Some((x, y));
            }
        }
    }
    None
}

fn differs_within(image: &RgbImage, picture: &RgbImage, (x, y): (u32, u32), most: u64) -> bool {
    let mut total = 0_u64;
    for (px, py, pixel) in picture.enumerate_pixels() {
        total += u64::from(distance(at(image, x + px, y + py), pixel.0));
        if total > most {
            return false;
        }
    }
    true
}

/// The topmost filled button of colour `brand` below `top`: a box at least `least_width`×20 px that `brand` fills to
/// 80 % (a focus ring, a border or a selected row's mark never does).
pub fn filled_button(image: &RgbImage, brand: Rgb, top: u32, least_width: u32) -> Option<Blob> {
    let (width, height) = image.dimensions();
    let mut seen = vec![false; (width * height) as usize];
    let mut found: Option<Blob> = None;
    for y in top..height {
        for x in RAIL..width {
            let index = (y * width + x) as usize;
            if seen[index] || distance(at(image, x, y), brand) > 40 {
                continue;
            }
            let blob = fill(image, brand, (x, y), &mut seen);
            let filled = u64::from(blob.pixels) * 10 >= u64::from(blob.width() * blob.height()) * 8;
            if blob.width() >= least_width
                && blob.height() >= 20
                && filled
                && found.is_none_or(|best| blob.y0 < best.y0)
            {
                found = Some(blob);
            }
        }
    }
    found
}

/// The connected pixels of colour `brand` from `start`, marked in `seen`.
fn fill(image: &RgbImage, brand: Rgb, start: (u32, u32), seen: &mut [bool]) -> Blob {
    let width = image.width();
    let mut blob = Blob {
        x0: start.0,
        y0: start.1,
        x1: start.0,
        y1: start.1,
        pixels: 0,
    };
    let mut stack = vec![start];
    seen[(start.1 * width + start.0) as usize] = true;
    while let Some((x, y)) = stack.pop() {
        blob.pixels += 1;
        blob.x0 = blob.x0.min(x);
        blob.y0 = blob.y0.min(y);
        blob.x1 = blob.x1.max(x);
        blob.y1 = blob.y1.max(y);
        let next = [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)];
        for (nx, ny) in next {
            let Some(pixel) = image.get_pixel_checked(nx, ny) else {
                continue;
            };
            let index = (ny * width + nx) as usize;
            if !seen[index] && distance(pixel.0, brand) <= 40 {
                seen[index] = true;
                stack.push((nx, ny));
            }
        }
    }
    blob
}
