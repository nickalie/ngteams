//! Teams' "Edit sticker": the words a sticker of Teams' own packs takes, drawn on its picture with no words (its
//! template), as Teams' web client draws them (`docs/protocol/chat.md`, "A sticker with a caption").
//!
//! - **A caption** ([`Caption`]) is one block of words in a region of the template, in the catalog's font and colour,
//!   which the editor starts with the sticker's own words. A meme's is a top and a bottom line in a band 50 px high,
//!   20 px in from the edges, in white Impact with a black edge, in upper case; with both lines empty it shows its
//!   hints, "TOP CAPTION" and "BOTTOM CAPTION".
//! - **The fit** ([`fit`]) is Teams' own: the words go on about as many lines as the region's shape asks for, each
//!   break at the space nearest an even split, then the block is scaled, up or down, until it fills the region's
//!   width or its height, and is centred in it.
//! - **The drawing** ([`render`]) puts each line on the template in the font the catalog names when the system has
//!   it, else a free font bundled in its place (Anton for Impact, Comic Neue for Comic Sans MS, a serif italic for
//!   Cochin Italic), and gives a PNG of the template's own size.

use std::sync::{Mutex, OnceLock, PoisonError};

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Wrap};
use image::{Rgba, RgbaImage};
use teams_proto::{PastedFormat, PastedPicture};

/// The editor's hint in the caption field of a sticker.
pub const CAPTION_HINT: &str = "Caption";
/// A meme's hints, which it also shows while both of its lines are empty.
pub const TOP_HINT: &str = "Top Caption";
pub const BOTTOM_HINT: &str = "Bottom caption";
/// The most characters a sticker's caption takes, and each of a meme's lines, as in Teams.
pub const MAX_CAPTION: usize = 150;
pub const MAX_MEME_LINE: usize = 25;
/// The height a captioned sticker is shown at in a message, CSS px; its width keeps the picture's shape (Teams' box
/// showed "High five", 334 by 250, at 240 by 180).
pub const SHOWN_HEIGHT: u32 = 180;

/// The text whose mean character width sets how many lines the words take.
const SAMPLE: &str = "AaBbCcDdEeFfGgHhIiJjKkLlMmNnOoPpQqRrSsTtUuVv.,?!";
/// The size the words are measured at, px; the fit scales from it.
const BASE: f32 = 100.0;
/// The space between two lines, px, at any size.
const LEADING: f32 = 2.0;
/// A meme's line: its inset from the picture's edges and its height, px.
const MEME_INSET: f32 = 20.0;
const MEME_BAND: f32 = 50.0;

/// The free fonts drawn in place of a catalog font the system lacks (SIL Open Font License 1.1, beside them).
const ANTON: &[u8] = include_bytes!("../../assets/fonts/Anton-Regular.ttf");
const COMIC_NEUE: &[u8] = include_bytes!("../../assets/fonts/ComicNeue-Regular.ttf");

/// A rectangle of the template, px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Region {
    fn width(self) -> f32 {
        self.right - self.left
    }

    fn height(self) -> f32 {
        self.bottom - self.top
    }
}

/// Where a caption's words go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Words {
    /// One block in this region of the template (left, top, right, bottom, px), which starts as `words`.
    Block { region: [u32; 4], words: String },
    /// A meme's top and bottom lines.
    Meme,
}

/// The caption a sticker takes (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caption {
    /// The file of its picture with no words.
    pub template: String,
    pub family: String,
    pub color: [u8; 3],
    /// The colour of the edge drawn round each letter, when it has one.
    pub border: Option<[u8; 3]>,
    pub words: Words,
}

impl Caption {
    /// The caption of a `caption` line's fields (template, font, colour, edge, region, words), as
    /// `teams_catalog.txt` holds them; `None` when one is not in that form.
    #[must_use]
    pub fn parse(fields: &[&str]) -> Option<Self> {
        let [template, family, color, border, region, words] = fields else {
            return None;
        };
        let words = match (*region, *words) {
            ("-", "-") => Words::Meme,
            (region, words) if words != "-" => {
                let numbers: Vec<u32> = region.split(' ').map(str::parse).collect::<Result<_, _>>().ok()?;
                let region: [u32; 4] = numbers.try_into().ok()?;
                (region[0] < region[2] && region[1] < region[3]).then_some(())?;
                Words::Block {
                    region,
                    words: (*words).to_owned(),
                }
            }
            _ => return None,
        };
        Some(Self {
            template: (*template).to_owned(),
            family: (*family).to_owned(),
            color: colour(color)?,
            border: match *border {
                "-" => None,
                border => Some(colour(border)?),
            },
            words,
        })
    }

    #[must_use]
    pub fn is_meme(&self) -> bool {
        self.words == Words::Meme
    }

    /// What the editor's fields start with: the sticker's own words, or a meme's two empty lines.
    #[must_use]
    pub fn first_text(&self) -> Vec<String> {
        match &self.words {
            Words::Block { words, .. } => vec![words.clone()],
            Words::Meme => vec![String::new(), String::new()],
        }
    }

    /// The lines to draw for `typed` (the editor's fields) on a template of `size`, each with its region: a block's
    /// words trimmed, none when empty; a meme's lines in upper case, its hints when both are empty.
    #[must_use]
    pub fn texts(&self, typed: &[String], size: (u32, u32)) -> Vec<(String, Region)> {
        match &self.words {
            Words::Block { region, .. } => {
                let text = typed.first().map_or_else(String::new, |text| one_line(text));
                if text.is_empty() {
                    return Vec::new();
                }
                let [left, top, right, bottom] = region.map(side);
                vec![(
                    text,
                    Region {
                        left,
                        top,
                        right,
                        bottom,
                    },
                )]
            }
            Words::Meme => {
                let lines: Vec<String> = typed.iter().take(2).map(|line| one_line(line)).collect();
                let lines = if lines.iter().all(String::is_empty) {
                    vec![TOP_HINT.to_owned(), BOTTOM_HINT.to_owned()]
                } else {
                    lines
                };
                let (width, height) = (side(size.0), side(size.1));
                let bands = [
                    (MEME_INSET, MEME_INSET + MEME_BAND),
                    (height - MEME_INSET - MEME_BAND, height - MEME_INSET),
                ];
                lines
                    .iter()
                    .zip(bands)
                    .filter(|(line, _)| !line.is_empty())
                    .map(|(line, (top, bottom))| {
                        (
                            line.to_uppercase(),
                            Region {
                                left: MEME_INSET,
                                top,
                                right: width - MEME_INSET,
                                bottom,
                            },
                        )
                    })
                    .collect()
            }
        }
    }

    /// The words a message names the sticker by, as Teams does: what was typed, the lines joined by ", ", after the
    /// sticker's name for a meme.
    #[must_use]
    pub fn label(&self, name: &str, typed: &[String]) -> String {
        let words = typed
            .iter()
            .map(|text| one_line(text))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        match (self.is_meme(), words.is_empty()) {
            (true, true) => name.to_owned(),
            (true, false) => format!("{name} {words}"),
            (false, _) => words,
        }
    }
}

/// `text` with each run of white space, line breaks too, made one space and none at the ends, as Teams' canvas draws it.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `#rrggbb`, `black` or `white`.
fn colour(text: &str) -> Option<[u8; 3]> {
    match text {
        "black" => Some([0, 0, 0]),
        "white" => Some([255, 255, 255]),
        _ => {
            let hex = text.strip_prefix('#').filter(|hex| hex.len() == 6)?;
            let value = u32::from_str_radix(hex, 16).ok()?;
            let [_, red, green, blue] = value.to_be_bytes();
            Some([red, green, blue])
        }
    }
}

/// A line of a fitted caption: its text, where it starts and its baseline, px.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub text: String,
    pub x: f32,
    pub baseline: f32,
}

/// A caption fitted to its region: the font size and the lines.
#[derive(Clone, Debug, PartialEq)]
pub struct Fitted {
    pub size: f32,
    pub lines: Vec<Placed>,
}

/// `text` fitted to `region` as Teams fits it (see the module docs); `measure` gives a text's width at [`BASE`] px.
/// `None` when nothing in it has a width, or its line spacing alone fills the region's height.
pub fn fit(text: &str, region: Region, measure: &mut dyn FnMut(&str) -> f32) -> Option<Fitted> {
    let (width, height) = (region.width(), region.height());
    let mean = measure(SAMPLE) / count(SAMPLE.chars().count());
    if mean <= 0.0 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let shape = height / width;
    let chars: Vec<char> = text.chars().collect();
    let lines = whole((shape * count(chars.len()) * mean / BASE).sqrt());
    let chunks = split(&chars, &breaks(&chars, lines));
    let widths: Vec<f32> = chunks.iter().map(|chunk| measure(chunk)).collect();
    let widest = widths.iter().copied().fold(0.0, f32::max);
    if widest <= 0.0 {
        return None;
    }
    let spacing = count(chunks.len().saturating_sub(1)) * LEADING;
    let scale = (width / widest).min((height - spacing).max(0.0) / (count(chunks.len()) * BASE));
    if scale <= 0.0 {
        return None;
    }
    let size = BASE * scale;
    let block = count(chunks.len()) * size + spacing;
    let first = region.top + (height - block) / 2.0 + size;
    let lines = chunks
        .into_iter()
        .zip(widths)
        .enumerate()
        .map(|(index, (text, line_width))| Placed {
            text,
            x: region.left + (width - scale * line_width) / 2.0,
            baseline: first + count(index) * (size + LEADING),
        })
        .collect();
    Some(Fitted { size, lines })
}

/// Where `chars` breaks into `lines` lines: for each even split, the space nearest it (the one before on a tie), kept
/// when it is inside the text and after the last break.
fn breaks(chars: &[char], lines: usize) -> Vec<usize> {
    if lines < 2 {
        return Vec::new();
    }
    let step = chars.len() / lines;
    let mut found = Vec::new();
    for split in (1..lines).map(|index| step * index) {
        let ahead = (split..chars.len()).find(|at| chars[*at] == ' ').unwrap_or(chars.len());
        let behind = (0..=split).rev().find(|at| chars.get(*at) == Some(&' '));
        let at = match behind {
            Some(behind) if split - behind <= ahead - split => behind,
            Some(_) => ahead,
            None if ahead - split < split + 1 => ahead,
            None => continue,
        };
        if at > 0 && at < chars.len() && found.last().is_none_or(|last| at > *last) {
            found.push(at);
        }
    }
    found
}

/// `chars` cut at `breaks`, each break's space left out.
fn split(chars: &[char], breaks: &[usize]) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    for at in breaks {
        chunks.push(chars[start..*at].iter().collect());
        start = at + 1;
    }
    chunks.push(chars[start.min(chars.len())..].iter().collect());
    chunks
}

/// The fonts the captions are drawn with, and the glyphs drawn so far.
pub struct Fonts {
    system: FontSystem,
    cache: SwashCache,
}

impl Fonts {
    /// The system's fonts and the bundled ones.
    fn with_system() -> Self {
        let mut system = FontSystem::new();
        Self::bundle(&mut system);
        Self {
            system,
            cache: SwashCache::new(),
        }
    }

    /// The bundled fonts alone: the same drawing on every system.
    #[must_use]
    pub fn bundled() -> Self {
        let mut database = cosmic_text::fontdb::Database::new();
        database.load_font_data(ANTON.to_vec());
        database.load_font_data(COMIC_NEUE.to_vec());
        Self {
            system: FontSystem::new_with_locale_and_db("en-US".to_owned(), database),
            cache: SwashCache::new(),
        }
    }

    fn bundle(system: &mut FontSystem) {
        system.db_mut().load_font_data(ANTON.to_vec());
        system.db_mut().load_font_data(COMIC_NEUE.to_vec());
    }

    /// Whether a font of `family` is there, in any case.
    fn has(&self, family: &str) -> bool {
        self.system
            .db()
            .faces()
            .any(|face| face.families.iter().any(|(name, _)| name.eq_ignore_ascii_case(family)))
    }

    /// The family and style `family` is drawn in: itself when it is there, else what stands in for it.
    #[must_use]
    pub fn face(&self, family: &str) -> (Option<String>, Style) {
        if self.has(family) {
            return (Some(family.to_owned()), Style::Normal);
        }
        let stand_in = match family.to_ascii_lowercase().as_str() {
            "impact" => "Anton",
            "comic sans ms" => "Comic Neue",
            name => {
                let upright = name.strip_suffix(" italic");
                let base = upright.map(|_| &family[..family.len() - " italic".len()]);
                return match base {
                    Some(base) if self.has(base) => (Some(base.to_owned()), Style::Italic),
                    Some(_) => (None, Style::Italic),
                    None => (None, Style::Normal),
                };
            }
        };
        (Some(stand_in.to_owned()), Style::Normal)
    }

    /// `text` shaped on one line at `size` px in `face`.
    fn shaped(&mut self, text: &str, face: &(Option<String>, Style), size: f32) -> Buffer {
        let family = match (face.0.as_deref(), face.1) {
            (Some(name), _) => Family::Name(name),
            (None, Style::Italic) => Family::Serif,
            (None, _) => Family::SansSerif,
        };
        let attrs = Attrs::new().family(family).style(face.1);
        let mut buffer = Buffer::new(&mut self.system, Metrics::new(size, size * 1.25));
        buffer.set_wrap(Wrap::None);
        buffer.set_size(None, None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.system, false);
        buffer
    }

    /// The width of `text` at `size` px in `face`.
    fn width(&mut self, text: &str, face: &(Option<String>, Style), size: f32) -> f32 {
        self.shaped(text, face, size)
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max)
    }

    /// Adds `line`, at `size` px in `face`, to `mask`, the coverage of an image `width` wide.
    fn cover(&mut self, line: &Placed, face: &(Option<String>, Style), size: f32, mask: &mut Mask) {
        let mut buffer = self.shaped(&line.text, face, size);
        let line_y = buffer.layout_runs().next().map_or(0.0, |run| run.line_y);
        let (x, y) = (pixel(line.x), pixel(line.baseline - line_y));
        let white = Color::rgba(255, 255, 255, 255);
        buffer.draw(&mut self.system, &mut self.cache, white, |left, top, w, h, color| {
            let alpha = f32::from(color.a()) / 255.0;
            for dy in 0..h.cast_signed() {
                for dx in 0..w.cast_signed() {
                    mask.add(x + left + dx, y + top + dy, alpha);
                }
            }
        });
    }
}

/// How much of each pixel the letters cover, 0 to 1.
struct Mask {
    width: u32,
    height: u32,
    cover: Vec<f32>,
}

impl Mask {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            cover: vec![0.0; width as usize * height as usize],
        }
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
        (x < self.width && y < self.height).then(|| y as usize * self.width as usize + x as usize)
    }

    fn add(&mut self, x: i32, y: i32, alpha: f32) {
        if let Some(at) = self.index(x, y) {
            self.cover[at] = self.cover[at].max(alpha);
        }
    }

    fn at(&self, x: i32, y: i32) -> f32 {
        self.index(x, y).map_or(0.0, |at| self.cover[at])
    }

    /// Each pixel's highest (`grow`) or lowest cover of the pixels round it: the letters a pixel wider or thinner.
    fn spread(&self, grow: bool) -> Self {
        let mut spread = Self::new(self.width, self.height);
        for y in 0..self.height.cast_signed() {
            for x in 0..self.width.cast_signed() {
                let round = (-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy)));
                let covers = round.map(|(dx, dy)| self.at(x + dx, y + dy));
                let value = if grow {
                    covers.fold(0.0, f32::max)
                } else {
                    covers.fold(1.0, f32::min)
                };
                if let Some(at) = spread.index(x, y) {
                    spread.cover[at] = value;
                }
            }
        }
        spread
    }
}

/// Paints `color` over `image` where `mask` covers it.
fn paint(image: &mut RgbaImage, mask: &Mask, color: [u8; 3]) {
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let alpha = mask.at(x.cast_signed(), y.cast_signed());
        if alpha > 0.0 {
            blend(pixel, color, alpha);
        }
    }
}

fn blend(pixel: &mut Rgba<u8>, color: [u8; 3], alpha: f32) {
    let below = f32::from(pixel[3]) / 255.0;
    let out = alpha + below * (1.0 - alpha);
    for channel in 0..3 {
        let mixed = f32::from(color[channel]).mul_add(alpha, f32::from(pixel[channel]) * below * (1.0 - alpha)) / out;
        pixel[channel] = byte(mixed);
    }
    pixel[3] = byte(out * 255.0);
}

/// The picture in `template`, a file's bytes; `None` when they hold no picture.
#[must_use]
pub fn decode(template: &[u8]) -> Option<RgbaImage> {
    Some(image::load_from_memory(template).ok()?.to_rgba8())
}

/// `template` with `typed` drawn on it as `caption` says, in `fonts`.
#[must_use]
pub fn draw(template: &RgbaImage, caption: &Caption, typed: &[String], fonts: &Mutex<Fonts>) -> RgbaImage {
    let mut image = template.clone();
    for (text, region) in caption.texts(typed, image.dimensions()) {
        // the fonts are held only to shape the letters: the painting goes on without them
        let Some(mask) = cover(&text, region, caption, image.dimensions(), fonts) else {
            continue;
        };
        match caption.border {
            // Teams strokes each letter 2 px wide on its outline, over the fill
            Some(border) => {
                paint(&mut image, &mask.spread(true), border);
                paint(&mut image, &mask.spread(false), caption.color);
            }
            None => paint(&mut image, &mask, caption.color),
        }
    }
    image
}

/// The cover of `text` fitted to `region`, on a picture of `size`.
fn cover(text: &str, region: Region, caption: &Caption, size: (u32, u32), fonts: &Mutex<Fonts>) -> Option<Mask> {
    let mut fonts = fonts.lock().unwrap_or_else(PoisonError::into_inner);
    let face = fonts.face(&caption.family);
    let fitted = fit(text, region, &mut |text| fonts.width(text, &face, BASE))?;
    let mut mask = Mask::new(size.0, size.1);
    for line in &fitted.lines {
        fonts.cover(line, &face, fitted.size, &mut mask);
    }
    Some(mask)
}

/// The fonts of the app's captions, read the first time they are asked for (the system's take a moment).
pub(crate) fn shared() -> &'static Mutex<Fonts> {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    FONTS.get_or_init(|| Mutex::new(Fonts::with_system()))
}

/// [`draw`] in the system's fonts, as a PNG.
#[must_use]
pub fn render(template: &RgbaImage, caption: &Caption, typed: &[String]) -> Option<PastedPicture> {
    png(&draw(template, caption, typed, shared()))
}

/// `image` as a PNG.
#[must_use]
pub fn png(image: &RgbaImage) -> Option<PastedPicture> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Some(PastedPicture {
        format: PastedFormat::Png,
        bytes: bytes.into_inner(),
    })
}

/// The size a picture of `size` is shown at in a message: [`SHOWN_HEIGHT`] high, its shape kept.
#[must_use]
pub fn shown(size: (u32, u32)) -> (u32, u32) {
    let (width, height) = (size.0.max(1), size.1.max(1));
    let shown = u64::from(width) * u64::from(SHOWN_HEIGHT);
    let rounded = (shown + u64::from(height) / 2) / u64::from(height);
    (u32::try_from(rounded).unwrap_or(u32::MAX).max(1), SHOWN_HEIGHT)
}

/// A side or a place of a picture, px, as a float: at most a few thousand, exact in an `f32`.
#[expect(clippy::cast_precision_loss, reason = "a picture's side is far below 2^24")]
fn side(value: u32) -> f32 {
    value as f32
}

/// A count of characters or lines, as a float: a caption has at most a few hundred.
#[expect(clippy::cast_precision_loss, reason = "a caption's characters are far below 2^24")]
fn count(value: usize) -> f32 {
    value as f32
}

/// The whole part of a number of lines, 0 for anything below 1 (or not a number).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a caption has at most a few hundred lines, never a negative number"
)]
fn whole(value: f32) -> usize {
    value.floor().max(0.0) as usize
}

/// The pixel a place falls in, rounded.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a place on a picture of at most a few thousand px"
)]
fn pixel(value: f32) -> i32 {
    value.round() as i32
}

/// A colour channel or an alpha, 0 to 255.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 first"
)]
fn byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
#[path = "caption_tests.rs"]
mod tests;
