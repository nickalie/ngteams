//! The reactions Teams offers and how they are drawn.
//!
//! - **The catalog** ([`catalog`]) is every reaction Teams' picker offers, in its categories and order, built by
//!   `scripts/reaction-pack.py` from the web client's emoticon metadata into `assets/reactions/catalog.tsv`: each
//!   reaction's id (the key Teams sends: `1f440_eyes`, or an older name such as `like`, `cool`, `hearteyes`), its code
//!   points, whether it takes a skin tone, and its description. A key of a reaction that takes one may carry the tone
//!   Teams appends (`yes-tone2`, tones 1–5): it is that reaction, drawn in the default tone.
//! - **The pictures** are Fluent Emoji 3D (Microsoft's, MIT: `assets/reactions/LICENSE-FLUENT-EMOJI`), the art Teams
//!   draws its reactions in, packed as WebP into `assets/reactions/emoji.bin`; each is decoded once and kept, so every
//!   chip of one reaction shares one GPUI image. A reaction without a picture draws its code points as text, and a key
//!   the catalog doesn't hold draws as itself.
//! - **Animations**: under the pointer the picker plays a reaction's animation as Teams does, its strip of frames read
//!   from Teams' CDN by the engine ([`Core::reaction_animation`], once a run) and cut into frames here ([`animation`]);
//!   until it arrives, or when there is none, the picture stands still. A call reaction on a tile plays its 3D emoji
//!   once, as Teams does ([`call_emoji_3d`], [`Core::call_reaction_animation`], [`frame_at`]).

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gpui::{
    AnyElement, App, ElementId, Global, Image, ImageFormat, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Pixels, RenderImage, Styled as _, div, img,
};
use image::{Delay, Frame, imageops};

use crate::bridge::Core;

const CATALOG: &str = include_str!("../assets/reactions/catalog.tsv");
const PICTURES: &[u8] = include_bytes!("../assets/reactions/emoji.bin");
/// The skin tones a key may carry (`-tone1` … `-tone5`).
const TONES: u8 = 5;
/// The most reactions a search shows.
pub const MOST_FOUND: usize = 240;
/// The frames a second Teams' reaction animations play at (every one in its catalog).
const ANIMATION_FPS: u32 = 24;
/// How many frames Teams' 3D emoji of a call reaction have: 4.25 s at [`ANIMATION_FPS`].
pub const EMOJI_3D_FRAMES: u32 = 102;
/// How long an animation whose read failed is drawn without it before it is asked for again: a CDN that fails for a
/// while isn't asked on each frame a tile draws.
pub const ANIMATION_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// A reaction Teams offers.
#[derive(Debug)]
pub struct Reaction {
    /// The key Teams sends for it.
    pub id: &'static str,
    /// What it is ("Grinning face with big eyes").
    pub description: &'static str,
    /// Its emoji as text.
    pub glyph: String,
    /// Whether it takes a skin tone.
    pub diverse: bool,
    /// Its category's index in [`Catalog::categories`].
    pub category: usize,
    /// Its picture's index among the pictures, when it has one.
    picture: Option<usize>,
}

/// One of the picker's categories: its title and its reactions' indices.
#[derive(Debug)]
pub struct Category {
    pub title: &'static str,
    pub reactions: Range<usize>,
}

/// Every reaction Teams offers (see the module docs).
#[derive(Debug)]
pub struct Catalog {
    reactions: Vec<Reaction>,
    categories: Vec<Category>,
    by_id: HashMap<&'static str, usize>,
    /// Each picture's bytes in [`PICTURES`], and its image once drawn.
    pictures: Vec<(Range<usize>, OnceLock<Arc<Image>>)>,
}

/// The catalog, read once.
#[must_use]
pub fn catalog() -> &'static Catalog {
    static CATALOG_READ: OnceLock<Catalog> = OnceLock::new();
    CATALOG_READ.get_or_init(|| Catalog::parse(CATALOG))
}

impl Catalog {
    fn parse(text: &'static str) -> Self {
        let mut catalog = Self {
            reactions: Vec::new(),
            categories: Vec::new(),
            by_id: HashMap::new(),
            pictures: Vec::new(),
        };
        let mut placed: HashMap<usize, usize> = HashMap::new();
        for line in text.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
            if let Some(title) = line.strip_prefix('@') {
                let at = catalog.reactions.len();
                catalog.categories.push(Category {
                    title,
                    reactions: at..at,
                });
                continue;
            }
            let mut fields = line.split('\t');
            let (Some(id), Some(points), Some(diverse), Some(offset), Some(length), Some(description)) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            let picture = match (offset.parse::<usize>(), length.parse::<usize>()) {
                (Ok(offset), Ok(length)) if offset + length <= PICTURES.len() => {
                    Some(*placed.entry(offset).or_insert_with(|| {
                        catalog.pictures.push((offset..offset + length, OnceLock::new()));
                        catalog.pictures.len() - 1
                    }))
                }
                _ => None,
            };
            let Some(category) = catalog.categories.len().checked_sub(1) else {
                continue;
            };
            catalog.by_id.insert(id, catalog.reactions.len());
            catalog.reactions.push(Reaction {
                id,
                description,
                glyph: points
                    .split(' ')
                    .filter_map(|point| u32::from_str_radix(point, 16).ok().and_then(char::from_u32))
                    .collect(),
                diverse: diverse == "d",
                category,
                picture,
            });
            catalog.categories[category].reactions.end = catalog.reactions.len();
        }
        catalog
    }

    /// The categories, in the picker's order.
    #[must_use]
    pub fn categories(&self) -> &[Category] {
        &self.categories
    }

    /// The reaction at `index`.
    #[must_use]
    pub fn at(&self, index: usize) -> Option<&Reaction> {
        self.reactions.get(index)
    }

    /// The index of the reaction `id` (a key without a skin tone).
    #[must_use]
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// The reaction a key names, and the skin tone it carries (0 for none).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<(&Reaction, u8)> {
        if let Some(&index) = self.by_id.get(key) {
            return Some((&self.reactions[index], 0));
        }
        let (id, tone) = key.rsplit_once("-tone")?;
        let tone = tone.parse::<u8>().ok().filter(|tone| (1..=TONES).contains(tone))?;
        let reaction = &self.reactions[*self.by_id.get(id)?];
        reaction.diverse.then_some((reaction, tone))
    }

    /// Whether `key` is a reaction Teams offers, in a form Teams sends: these are the keys the account may set.
    #[must_use]
    pub fn offers(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Of `indices`, in their order, those the picker offers: a reaction with a picture (not Teams' brand stickers,
    /// which Fluent Emoji has none of), and of the reactions sharing one picture (an older name and a code point's:
    /// `happyface`, `1f603_grinningfacewithbigeyes`) only the first.
    #[must_use]
    pub fn pickable(&self, indices: impl IntoIterator<Item = usize>) -> Vec<usize> {
        let mut seen = HashSet::new();
        indices
            .into_iter()
            .filter(|&index| {
                self.reactions
                    .get(index)
                    .and_then(|reaction| reaction.picture)
                    .is_some_and(|picture| seen.insert(picture))
            })
            .collect()
    }

    /// The reactions the picker offers ([`Self::pickable`]) whose description or id holds every word of `query`, at
    /// most [`MOST_FOUND`]: those whose description is the query first, then those it begins, then the rest, each in
    /// the catalog's order; nothing for a query without a word.
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<usize> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let whole = words.join(" ");
        let mut found: Vec<(u8, usize)> = self
            .reactions
            .iter()
            .enumerate()
            .filter_map(|(index, reaction)| {
                let description = reaction.description.to_lowercase();
                let holds = words
                    .iter()
                    .all(|word| description.contains(word.as_str()) || reaction.id.contains(word.as_str()));
                let rank = if description == whole {
                    0
                } else if description.starts_with(&whole) {
                    1
                } else {
                    2
                };
                holds.then_some((rank, index))
            })
            .collect();
        found.sort_unstable();
        let mut found = self.pickable(found.into_iter().map(|(_, index)| index));
        found.truncate(MOST_FOUND);
        found
    }

    /// The picture of `reaction`, decoded once.
    fn picture(&self, reaction: &Reaction) -> Option<Arc<Image>> {
        let (bytes, image) = self.pictures.get(reaction.picture?)?;
        let image =
            image.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Webp, PICTURES[bytes.clone()].to_vec())));
        Some(Arc::clone(image))
    }
}

/// A key as text: its emoji (in the skin tone it carries) when the catalog holds it, else the key itself.
#[must_use]
pub fn glyph(key: &str) -> String {
    let Some((reaction, tone)) = catalog().get(key) else {
        return key.to_owned();
    };
    if tone == 0 {
        return reaction.glyph.clone();
    }
    // the tone modifier follows the first code point
    let modifier = char::from_u32(0x1F3FA + u32::from(tone)).unwrap_or_default();
    let mut chars = reaction.glyph.chars().filter(|c| !matches!(c, '\u{FE0E}' | '\u{FE0F}'));
    chars.next().into_iter().chain([modifier]).chain(chars).collect()
}

/// What a key is, for a tooltip: its description, else the key itself.
#[must_use]
pub fn description(key: &str) -> &str {
    catalog().get(key).map_or(key, |(reaction, _)| reaction.description)
}

/// A key drawn `size` square: its picture, else its emoji as text, else the key itself.
#[must_use]
pub fn emoji(key: &str, size: Pixels) -> AnyElement {
    let catalog = catalog();
    let picture = catalog.get(key).and_then(|(reaction, _)| catalog.picture(reaction));
    match picture {
        Some(picture) => img(picture).size(size).flex_none().into_any_element(),
        None => div()
            .flex_none()
            .line_height(size)
            .text_size(size * 0.8)
            .child(glyph(key))
            .into_any_element(),
    }
}

/// The reactions' and messages' emoticons' animations this run, by id: decoded, or `None` when there is none or its
/// read still runs; an id never asked for, or whose read failed on the way (asked again when next drawn), isn't here.
#[derive(Default)]
struct Animations {
    known: HashMap<Kept, Option<Arc<RenderImage>>>,
}

/// What an animation is kept by: a strip by its id, a call reaction's 3D emoji by its reaction and side, so that no
/// emoticon id a message names can stand for a 3D emoji.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Kept {
    Strip(String),
    Emoji3d(String, u16),
}

impl Global for Animations {}

/// The animation of `key` (a reaction's key, or the id of an emoticon a message shows, which may be one the catalog
/// doesn't hold) when it was read; otherwise asks the engine for it once (the windows drawn again once it arrives; a
/// read that failed on the way is asked again when next drawn) and answers `None` meanwhile, as for a reaction without
/// one.
pub fn animation(key: &str, core: &Arc<dyn Core>, cx: &mut App) -> Option<Arc<RenderImage>> {
    animation_of(key, crate::inline::PictureSource::new(Arc::clone(core)).engine(), cx)
}

/// [`animation`], asked of `core` (a message's emoticon's, through its [`crate::inline::PictureSource`]).
pub fn animation_of(key: &str, core: &dyn crate::inline::PictureEngine, cx: &mut App) -> Option<Arc<RenderImage>> {
    let id = catalog()
        .get(key)
        .map_or_else(|| key.to_owned(), |(reaction, _)| reaction.id.to_owned());
    decoded(Kept::Strip(id.clone()), || core.reaction_animation(id), frames, cx)
}

/// The 3D emoji Teams plays once on the tile of a call reaction (`reaction`: `like`, `heart`, `applause`, `laugh`,
/// `surprised`), with frames of `side` px (one of [`teams_proto::EMOJI_3D_SIDES`]), as [`animation`] keeps it: `None`
/// until it was read, or when Teams' CDN has none or one of another shape.
pub fn call_emoji_3d(reaction: &str, side: u16, core: &Arc<dyn Core>, cx: &mut App) -> Option<Arc<RenderImage>> {
    let known = Kept::Emoji3d(reaction.to_owned(), side);
    decoded(
        known,
        || core.call_reaction_animation(reaction.to_owned(), side),
        move |strip| emoji_3d_frames(strip, side),
        cx,
    )
}

/// A 3D emoji's frames when its PNG is [`EMOJI_3D_FRAMES`] frames of `side` px, read from its header before any pixel
/// is decoded; `None` for another shape.
#[must_use]
pub fn emoji_3d_frames(strip: &[u8], side: u16) -> Option<RenderImage> {
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(strip), image::ImageFormat::Png)
        .into_dimensions()
        .ok()?;
    let side = u32::from(side);
    (width == side && Some(height) == side.checked_mul(EMOJI_3D_FRAMES))
        .then(|| frames(strip))
        .flatten()
}

/// Which frame of an animation of `frames` frames played once shows `elapsed` after it started: one a
/// [`ANIMATION_FPS`]th of a second, the last one held after its end.
#[must_use]
pub fn frame_at(elapsed: std::time::Duration, frames: usize) -> usize {
    let index = elapsed.as_millis() * u128::from(ANIMATION_FPS) / 1_000;
    usize::try_from(index)
        .unwrap_or(usize::MAX)
        .min(frames.saturating_sub(1))
}

/// The animation kept as `id` when it was read; otherwise asks for it once with `ask` and cuts it with `decode` (the
/// windows drawn again once it arrives; a read that failed is asked again when drawn after [`ANIMATION_RETRY_AFTER`])
/// and answers `None` meanwhile.
fn decoded(
    id: Kept,
    ask: impl FnOnce() -> teams_core::Reply<Option<teams_core::Strip>>,
    decode: impl FnOnce(&[u8]) -> Option<RenderImage> + Send + 'static,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let animations = cx.default_global::<Animations>();
    if let Some(known) = animations.known.get(&id) {
        return known.clone();
    }
    // asked: an answer of `None` until it comes
    animations.known.insert(id.clone(), None);
    let reply = ask();
    cx.spawn(async move |cx| {
        let strip = match reply.await {
            Ok(Some(strip)) => strip,
            Ok(None) => return,
            Err(_) => {
                cx.background_executor().timer(ANIMATION_RETRY_AFTER).await;
                cx.update(|cx| cx.default_global::<Animations>().known.remove(&id));
                return;
            }
        };
        let decoded = cx
            .background_executor()
            .spawn(async move { decode(&strip) })
            .await
            .map(Arc::new);
        if decoded.is_some() {
            cx.update(|cx| {
                cx.default_global::<Animations>().known.insert(id, decoded);
                cx.refresh_windows();
            });
        }
    })
    .detach();
    None
}

/// An animation strip (a PNG of square frames stacked top to bottom) as the frames GPUI plays, at
/// [`ANIMATION_FPS`]; `None` for one that isn't a PNG or whose frames aren't square.
#[must_use]
pub fn frames(strip: &[u8]) -> Option<RenderImage> {
    let strip = image::load_from_memory_with_format(strip, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (side, height) = strip.dimensions();
    if side == 0 || height < side || height % side != 0 {
        return None;
    }
    let delay = Delay::from_numer_denom_ms(1_000, ANIMATION_FPS);
    let frames: Vec<Frame> = (0..height / side)
        .map(|index| {
            let mut frame = imageops::crop_imm(&strip, 0, index * side, side, side).to_image();
            // GPUI draws BGRA
            for pixel in frame.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Frame::from_parts(frame, 0, 0, delay)
        })
        .collect();
    Some(RenderImage::new(frames))
}

/// `key` drawn `size` square, playing its animation when `animated` holds one ([`animation`]), else as [`emoji`].
#[must_use]
pub fn emoji_playing(key: &str, size: Pixels, animated: Option<Arc<RenderImage>>) -> AnyElement {
    match animated {
        // an id of its own: GPUI keeps the frame it is on in the element's state, and without one shows only the first
        Some(frames) => img(frames)
            .id(ElementId::Name(format!("playing-{key}").into()))
            .size(size)
            .flex_none()
            .into_any_element(),
        None => emoji(key, size),
    }
}
