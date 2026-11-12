//! How many tiles the stage shows at once and how big, as Teams' gallery sizes them: every tile the same 16:9 size,
//! as large as the stage lets them be, no more of them than View's "Max gallery size" (4, 9, 16 or 49) and no more
//! than fit at a readable size; the rest on further pages, turned with the `<` `>` at the gallery's bottom. A strip
//! (beside a share, under the speaker) shows as many as fit along it and pages the same way. Nothing the stage draws
//! leaves it: the call's controls above stay clear and take every click.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement as _, Styled as _};

use super::CallStage;

/// Teams' tile's width over its height.
pub(super) const TILE_RATIO: f32 = 16.0 / 9.0;
/// The smallest tile the gallery lays out, px: below it Teams shows fewer to a page.
pub(super) const MIN_TILE_WIDTH: f32 = 160.0;
/// The shortest the speaker view's strip lets its tiles get beside the floating self view, px: a stage too narrow for
/// one of them and the pager besides takes the self view into the strip.
pub(super) const MIN_STRIP_TILE: f32 = 64.0;
/// The space between tiles, px.
pub(super) const TILE_GAP: f32 = 8.0;
/// The height of the pager under a gallery or a strip that pages, px.
pub(super) const PAGER_HEIGHT: f32 = 36.0;
/// The width of the pager, px: its `<`, which page shows, and its `>`.
pub(super) const PAGER_WIDTH: f32 = 120.0;
/// View's "Max gallery size" choices, as Teams' desktop offers them.
pub(super) const GALLERY_SIZES: [usize; 4] = [4, 9, 16, 49];
/// The gallery size until the user picks one: Teams' largest on the desktop.
const DEFAULT_GALLERY_SIZE: usize = 49;

/// `count` tiles of 16:9 laid out in `area` (width, height, px): the columns, and the tiles' width and height, the
/// columns picked so the tiles come out largest.
#[must_use]
pub(super) fn arrange(count: usize, (width, height): (f32, f32)) -> (usize, (f32, f32)) {
    let count = count.max(1);
    let mut best = (1, (0.0, 0.0));
    for columns in 1..=count {
        let rows = count.div_ceil(columns);
        let (cols, rows_f) = (as_f32(columns), as_f32(rows));
        let fit_width = (width - TILE_GAP * (cols - 1.0)) / cols;
        let fit_height = (height - TILE_GAP * (rows_f - 1.0)) / rows_f;
        let tile_width = fit_width.min(fit_height * TILE_RATIO).max(0.0);
        if tile_width > best.1.0 {
            best = (columns, (tile_width, tile_width / TILE_RATIO));
        }
    }
    best
}

/// How many tiles a page of the gallery holds in `area` (width, height, px) at most `max` to a page: the most whose
/// tiles stay at least [`MIN_TILE_WIDTH`] wide, never fewer than one.
#[must_use]
pub(super) fn page_size(max: usize, area: (f32, f32)) -> usize {
    (1..=max.max(1))
        .rev()
        .find(|&count| arrange(count, area).1.0 >= MIN_TILE_WIDTH)
        .unwrap_or(1)
}

/// How many tiles of `tile` px (along the strip) a strip `length` px long holds, never fewer than one.
#[must_use]
pub(super) fn strip_size(length: f32, tile: f32) -> usize {
    let fits = ((length + TILE_GAP) / (tile + TILE_GAP)).floor();
    if fits >= 1.0 {
        // a strip's length is a window's: far below usize's range
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole, positive count"
        )]
        let fits = fits as usize;
        fits
    } else {
        1
    }
}

/// How long each of `shown` tiles of a strip `length` px long is along it, at most `tile` px: shorter when even the
/// fewest a page shows wouldn't fit at `tile` (a small window), the `pager` px for the pager kept first, so the tiles
/// and the pager never run past the strip.
#[must_use]
pub(super) fn strip_tile(length: f32, tile: f32, shown: usize, pager: f32) -> f32 {
    let shown = as_f32(shown.max(1));
    let room = length - pager - TILE_GAP * (shown - 1.0);
    (room / shown).min(tile).max(0.0)
}

/// The page `asked` of `count` items `per` to a page, kept in range: its first item's index, the page, and how many
/// pages there are.
#[must_use]
pub(super) fn page_of(count: usize, per: usize, asked: usize) -> Page {
    let per = per.max(1);
    let pages = count.div_ceil(per).max(1);
    let page = asked.min(pages - 1);
    Page {
        first: page * per,
        per,
        index: page,
        pages,
    }
}

/// One page of tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Page {
    /// The index of its first item.
    pub(super) first: usize,
    /// How many items a page holds.
    pub(super) per: usize,
    /// Which page it is, from 0.
    pub(super) index: usize,
    /// How many pages there are.
    pub(super) pages: usize,
}

/// A strip of tiles that pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Strip {
    /// The people beside a share.
    Share,
    /// The others under the speaker view's large tile.
    Speaker,
}

/// View's Max gallery size, which page of the gallery and of a strip shows, and the window's and the stage's size.
pub(super) struct Paging {
    /// The most tiles a page of the gallery shows (View's Max gallery size).
    pub(super) size: usize,
    /// The window's width and height as last drawn, px: a narrow one's toolbar shows its controls' icons alone, and
    /// the stage's size is guessed from it before the stage was laid out.
    pub(super) window: (f32, f32),
    /// The gallery's page, from 0.
    pub(super) gallery: usize,
    /// The strip's page (beside a share, under the speaker), from 0.
    pub(super) strip: usize,
    /// Which strip `strip` is a page of: another strip starts on its first page.
    pub(super) strip_of: Option<Strip>,
    /// The stage's width and height as last laid out, px.
    area: Rc<Cell<Option<(f32, f32)>>>,
}

impl Default for Paging {
    fn default() -> Self {
        Self {
            size: DEFAULT_GALLERY_SIZE,
            window: (f32::MAX, f32::MAX),
            gallery: 0,
            strip: 0,
            strip_of: None,
            area: Rc::default(),
        }
    }
}

impl Paging {
    /// The stage's width and height as last laid out, else `estimate`.
    pub(super) fn area_or(&self, estimate: (f32, f32)) -> (f32, f32) {
        self.area.get().unwrap_or(estimate)
    }

    /// An empty element over the stage that keeps its size as it is laid out, drawing the stage again when it changed.
    pub(super) fn measure(&self, cx: &mut Context<CallStage>) -> AnyElement {
        let area = Rc::clone(&self.area);
        let stage = cx.weak_entity();
        gpui::canvas(
            move |bounds, window, _| {
                let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                if area.replace(Some(size)) != Some(size) {
                    // the tiles take the new size on the next frame
                    window.on_next_frame(move |_, cx| {
                        let _ = stage.update(cx, |_, cx| cx.notify());
                    });
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
}

fn as_f32(count: usize) -> f32 {
    f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_come_out_16_by_9_and_as_large_as_the_area_lets_them() {
        // one tile fills the width of a wide area, or the height of a tall one
        let (columns, (w, h)) = arrange(1, (1600.0, 900.0));
        assert_eq!(columns, 1);
        assert!((w - 1600.0).abs() < 0.1 && (h - 900.0).abs() < 0.1);
        // nine in a 16:9 area: three by three
        assert_eq!(arrange(9, (1600.0, 900.0)).0, 3);
        // four in a wide, short area: one row
        assert_eq!(arrange(4, (2000.0, 250.0)).0, 4);
        let (_, (w, h)) = arrange(10, (1200.0, 700.0));
        assert!((w / h - TILE_RATIO).abs() < 0.01);
    }

    #[test]
    fn a_page_holds_no_more_than_the_gallery_size_or_what_stays_readable() {
        // a full HD stage holds Teams' 49
        assert_eq!(page_size(49, (1880.0, 980.0)), 49);
        assert_eq!(page_size(9, (1880.0, 980.0)), 9);
        // a small window fewer, each tile still readable
        let small = page_size(49, (760.0, 440.0));
        assert!((4..49).contains(&small), "{small}");
        assert!(arrange(small, (760.0, 440.0)).1.0 >= MIN_TILE_WIDTH);
        assert!(arrange(small + 1, (760.0, 440.0)).1.0 < MIN_TILE_WIDTH);
        // a tiny one still one
        assert_eq!(page_size(49, (50.0, 30.0)), 1);
    }

    #[test]
    fn a_strip_s_tiles_shrink_to_fit_a_small_window() {
        // room enough: the tile's own length
        assert!((strip_tile(1000.0, 184.0, 3, 0.0) - 184.0).abs() < 0.01);
        // one tile beside the pager in 280 px: what is left
        assert!((strip_tile(280.0, 184.0, 1, 128.0) - 152.0).abs() < 0.01);
        // two tiles and a gap in 200 px
        assert!((strip_tile(200.0, 132.0, 2, 0.0) - 96.0).abs() < 0.01);
        // a strip too short for two at the floor: the pager keeps its room, the tiles share what is left
        assert!((strip_tile(180.0, 132.0, 2, 44.0) - 64.0).abs() < 0.01);
        assert!((strip_tile(68.0, 132.0, 2, 44.0) - 8.0).abs() < 0.01);
        assert!(strip_tile(50.0, 184.0, 2, 128.0).abs() < 0.01);
    }

    #[test]
    fn a_strip_holds_what_fits_along_it() {
        assert_eq!(strip_size(600.0, 132.0), 4);
        assert_eq!(strip_size(100.0, 132.0), 1);
    }

    #[test]
    fn a_page_asked_past_the_last_is_the_last() {
        assert_eq!(
            page_of(50, 16, 0),
            Page {
                first: 0,
                per: 16,
                index: 0,
                pages: 4
            }
        );
        assert_eq!(
            page_of(50, 16, 9),
            Page {
                first: 48,
                per: 16,
                index: 3,
                pages: 4
            }
        );
        assert_eq!(
            page_of(0, 16, 2),
            Page {
                first: 0,
                per: 16,
                index: 0,
                pages: 1
            }
        );
    }
}
