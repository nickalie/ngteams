//! Right-to-left text in a left-to-right paragraph, and right-to-left paragraphs (`TextDirection`): the shaper's
//! visual glyph order is put into text order, the line wraps, and each row is put back into visual order (Unicode
//! Bidirectional Algorithm), as a browser shows it. A stand-in shaper gives each character the same advance and orders
//! the glyphs visually with the first strong character's direction, as the platform shapers do. Where painting puts
//! each glyph, and a text element's hit tests in each alignment, are checked against the same hit tests. All text is
//! made up.

use std::{borrow::Cow, cell::RefCell, rc::Rc, sync::Arc};

use gpui::{
    Context, DevicePixels, Font, FontId, FontMetrics, FontRun, GlyphId, IntoElement, LineLayout,
    NoopTextSystem, ParentElement as _, Pixels, PlatformTextSystem, Render, RenderGlyphParams,
    ShapedGlyph, ShapedRun, SharedString, Size, Styled as _, StyledText, TestAppContext, TextAlign,
    TextDirection, TextLayout, TextRenderingMode, TextRun, TextSystem, Window, WindowTextSystem,
    WrappedLine, div, point, px,
};
use unicode_bidi::BidiInfo;

/// Lays out each line as the test platform does, then lets the function change it, with the line's text.
struct StandInShaper(fn(&mut LineLayout, &str));

impl PlatformTextSystem for StandInShaper {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> gpui::Result<()> {
        NoopTextSystem.add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        NoopTextSystem.all_font_names()
    }

    fn font_id(&self, descriptor: &Font) -> gpui::Result<FontId> {
        NoopTextSystem.font_id(descriptor)
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        NoopTextSystem.font_metrics(font_id)
    }

    fn typographic_bounds(
        &self,
        font_id: FontId,
        glyph_id: GlyphId,
    ) -> gpui::Result<gpui::Bounds<f32>> {
        NoopTextSystem.typographic_bounds(font_id, glyph_id)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> gpui::Result<Size<f32>> {
        NoopTextSystem.advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        NoopTextSystem.glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(
        &self,
        params: &RenderGlyphParams,
    ) -> gpui::Result<gpui::Bounds<DevicePixels>> {
        NoopTextSystem.glyph_raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: gpui::Bounds<DevicePixels>,
    ) -> gpui::Result<(Size<DevicePixels>, Vec<u8>)> {
        NoopTextSystem.rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let mut layout = NoopTextSystem.layout_line(text, font_size, runs);
        (self.0)(&mut layout, text);
        layout
    }

    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        NoopTextSystem.recommended_rendering_mode(font_id, font_size)
    }
}

/// The advance of each character of the stand-in shaper at [`FONT_SIZE`]: 600 of 1000 units.
const ADVANCE: f32 = 6.0;
const FONT_SIZE: f32 = 10.0;
const LINE_HEIGHT: f32 = 20.0;

/// A made-up Arabic sentence and its English continuation, as one paragraph in quotes: the shape of the line that
/// lost its place and its text.
const ARABIC: &str = "أوافق على تلقي العروض والأخبار من النادي";
const ENGLISH: &str = ". I agree to get news and offers from the club.";

/// The glyphs of a layout in text order put in visual order, each with its advance, the paragraph's direction from
/// its first strong character.
fn visual(layout: &mut LineLayout, text: &str) {
    let glyphs: Vec<&ShapedGlyph> = layout.runs.iter().flat_map(|run| &run.glyphs).collect();
    let advances: Vec<Pixels> = glyphs
        .iter()
        .enumerate()
        .map(|(ix, glyph)| {
            glyphs
                .get(ix + 1)
                .map_or(layout.width, |next| next.position.x)
                - glyph.position.x
        })
        .collect();
    let info = BidiInfo::new(text, None);
    let mut placed = Vec::new();
    let mut x = px(0.);
    for paragraph in &info.paragraphs {
        let (levels, runs) = info.visual_runs(paragraph, paragraph.range.clone());
        let mut visual_runs: Vec<_> = runs
            .into_iter()
            .map(|run| (levels[run.start].is_rtl(), run))
            .collect();
        if paragraph.level.is_rtl() {
            visual_runs.reverse();
        }
        for (rtl, run) in visual_runs {
            let mut in_run: Vec<usize> = (0..glyphs.len())
                .filter(|ix| run.contains(&glyphs[*ix].index))
                .collect();
            if rtl {
                in_run.reverse();
            }
            for ix in in_run {
                let mut glyph = glyphs[ix].clone();
                glyph.position = point(x, glyph.position.y);
                x += advances[ix];
                placed.push(glyph);
            }
        }
    }
    layout.runs = vec![ShapedRun {
        font_id: FontId(1),
        glyphs: placed,
    }];
}

fn text_system() -> WindowTextSystem {
    WindowTextSystem::new(Arc::new(TextSystem::new(Arc::new(StandInShaper(visual)))))
}

fn shape_runs_in(
    text: &str,
    runs: &[TextRun],
    width: Option<f32>,
    direction: TextDirection,
) -> WrappedLine {
    let mut lines = text_system()
        .shape_text_in(
            SharedString::from(text.to_owned()),
            px(FONT_SIZE),
            runs,
            width.map(px),
            None,
            direction,
        )
        .unwrap_or_default();
    assert_eq!(lines.len(), 1, "one line: {text:?}");
    lines.remove(0)
}

fn shape_runs(text: &str, runs: &[TextRun], width: Option<f32>) -> WrappedLine {
    shape_runs_in(text, runs, width, TextDirection::Ltr)
}

fn shape_in(text: &str, width: Option<f32>, direction: TextDirection) -> WrappedLine {
    let run = TextRun {
        len: text.len(),
        font: gpui::font("Made-up Sans"),
        ..TextRun::default()
    };
    shape_runs_in(text, &[run], width, direction)
}

fn shape(text: &str, width: Option<f32>) -> WrappedLine {
    shape_in(text, width, TextDirection::Ltr)
}

fn rows(line: &WrappedLine) -> u16 {
    u16::try_from(line.wrap_boundaries().len() + 1).unwrap_or(u16::MAX)
}

/// The characters of each row as they show from left to right: the character under the middle of each advance.
fn shown(line: &WrappedLine, text: &str) -> Vec<String> {
    (0..rows(line))
        .map(|row| {
            let y = px(LINE_HEIGHT * f32::from(row) + LINE_HEIGHT / 2.);
            let mut shown = String::new();
            let mut x = ADVANCE / 2.;
            while let Ok(index) = line.index_for_position(point(px(x), y), px(LINE_HEIGHT)) {
                shown.extend(text[index..].chars().next());
                x += ADVANCE;
            }
            shown
        })
        .collect()
}

fn reversed(text: &str) -> String {
    text.chars().rev().collect()
}

#[test]
fn an_arabic_sentence_and_english_in_one_paragraph_keep_their_column_and_all_their_text() {
    let text = format!("\"{ARABIC}{ENGLISH}\"");
    let width = 200.;
    let line = shape(&text, Some(width));

    for (index, _) in text.char_indices() {
        let at = line.position_for_index(index, px(LINE_HEIGHT));
        let at = at.unwrap_or_else(|| panic!("no position for byte {index}"));
        assert!(
            at.x >= px(0.) && at.x <= px(width),
            "byte {index} at {at:?}, outside the column"
        );
    }
    let shown = shown(&line, &text);
    let all: usize = shown.iter().map(|row| row.chars().count()).sum();
    assert_eq!(
        all,
        text.chars().count(),
        "every character shows once: {shown:?}"
    );
    let first = &shown[0];
    assert!(
        first.starts_with('"'),
        "the quote stays at the start of the row: {first:?}"
    );
    let arabic_shown: String = first.chars().skip(1).collect();
    assert!(
        ARABIC.starts_with(reversed(&arabic_shown).trim()),
        "the first Arabic words read right to left after the quote: {first:?}"
    );
    assert!(
        shown.iter().any(|row| row.contains("I agree")) && shown.concat().contains("club."),
        "the English after the Arabic shows, left to right: {shown:?}"
    );
}

#[test]
fn a_short_arabic_phrase_then_english_shows_as_a_browser_shows_it() {
    let text = "\"ابت. I agree\"";
    let line = shape(text, None);
    assert_eq!(shown(&line, text), ["\"تبا. I agree\""]);
}

#[test]
fn a_paragraph_that_starts_with_arabic_is_still_left_to_right() {
    let text = "ابت abc";
    let line = shape(text, None);
    assert_eq!(shown(&line, text), ["تبا abc"]);
}

#[test]
fn a_pure_arabic_paragraph_reads_right_to_left_its_final_stop_at_the_right() {
    let words = "مرحبا بكم في الفريق";
    let text = format!("{words}.");
    let line = shape(&text, None);
    assert_eq!(shown(&line, &text), [format!("{}.", reversed(words))]);
}

#[test]
fn hebrew_reads_right_to_left_with_its_comma() {
    let text = "שלום, עולם";
    let line = shape(text, None);
    assert_eq!(shown(&line, text), [reversed(text)]);
}

#[test]
fn numbers_inside_arabic_read_left_to_right() {
    let text = "Order 42: ابت 123 ثج (2)";
    let line = shape(text, None);
    assert_eq!(shown(&line, text), ["Order 42: )2( جث 123 تبا"]);
}

#[test]
fn arabic_wraps_between_words_and_each_row_reads_right_to_left() {
    let text = ARABIC;
    let width = 100.;
    let line = shape(text, Some(width));
    let shown = shown(&line, text);
    assert!(shown.len() > 1, "the sentence wraps: {shown:?}");
    let words: Vec<&str> = text.split(' ').collect();
    for row in &shown {
        for word in row.split(' ').filter(|word| !word.is_empty()) {
            assert!(
                words.contains(&reversed(word).as_str()),
                "a row holds whole words, right to left: {row:?} in {shown:?}"
            );
        }
    }
    let rows_in_order: String = shown
        .iter()
        .map(|row| reversed(row.trim_end()))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(rows_in_order.split_whitespace().collect::<Vec<_>>(), words);
}

#[test]
fn a_click_at_the_left_edge_of_an_arabic_word_finds_its_end() {
    let text = "ab ابت cd";
    let line = shape(text, None);
    let word = text.find('ا').unwrap_or_default();
    let word_end = word + "ابت".len();
    let left_edge = point(px(3. * ADVANCE + 1.), px(LINE_HEIGHT / 2.));
    assert_eq!(
        line.closest_index_for_position(left_edge, px(LINE_HEIGHT)),
        Ok(word_end)
    );
    let right_edge = point(px(6. * ADVANCE - 1.), px(LINE_HEIGHT / 2.));
    assert_eq!(
        line.closest_index_for_position(right_edge, px(LINE_HEIGHT)),
        Ok(word)
    );
    let caret = line
        .position_for_index(word, px(LINE_HEIGHT))
        .map(|at| at.x);
    assert_eq!(
        caret,
        Some(px(6. * ADVANCE)),
        "the caret before the word's first letter is at its right"
    );
}

#[test]
fn a_bold_run_inside_arabic_keeps_the_order() {
    let text = "ابت ثجح خدذ";
    let bold_start = text.find('ث').unwrap_or_default();
    let bold_end = text.find(" خ").unwrap_or_default();
    let regular = gpui::font("Made-up Sans");
    let bold = gpui::Font {
        weight: gpui::FontWeight::BOLD,
        ..regular.clone()
    };
    let runs = [
        TextRun {
            len: bold_start,
            font: regular.clone(),
            ..TextRun::default()
        },
        TextRun {
            len: bold_end - bold_start,
            font: bold,
            ..TextRun::default()
        },
        TextRun {
            len: text.len() - bold_end,
            font: regular,
            ..TextRun::default()
        },
    ];
    let line = shape_runs(text, &runs, None);
    assert_eq!(shown(&line, text), [reversed(text)]);
}

#[test]
fn text_without_right_to_left_letters_keeps_its_layout() {
    let text = "Order 42: one, two (three).";
    let line = shape(text, Some(60.));
    assert_eq!(shown(&line, text).concat(), text);
    assert_eq!(
        line.position_for_index(1, px(LINE_HEIGHT)).map(|at| at.x),
        Some(px(ADVANCE))
    );
}

#[test]
fn a_right_to_left_paragraph_puts_its_quote_at_the_right_and_english_after_the_arabic_at_the_left()
{
    let text = "\"ابت. I agree\"";
    let line = shape_in(text, None, TextDirection::Rtl);
    assert_eq!(shown(&line, text), ["\"I agree .تبا\""]);
}

#[test]
fn english_in_a_right_to_left_paragraph_puts_its_final_stop_at_the_left() {
    let text = "Thanks, see you.";
    let line = shape_in(text, None, TextDirection::Rtl);
    assert_eq!(shown(&line, text), [".Thanks, see you"]);
}

#[test]
fn a_right_to_left_paragraph_wraps_with_its_first_words_on_the_first_row() {
    let line = shape_in(ARABIC, Some(100.), TextDirection::Rtl);
    let shown = shown(&line, ARABIC);
    assert!(shown.len() > 1, "the sentence wraps: {shown:?}");
    let first_word = ARABIC.split(' ').next().unwrap_or_default();
    assert!(
        shown[0].trim().ends_with(&reversed(first_word)),
        "the first word is at the right of the first row: {shown:?}"
    );
    let row_end = line.position_for_index(ARABIC.len(), px(LINE_HEIGHT));
    let last_row = f32::from(rows(&line) - 1);
    assert_eq!(row_end.map(|at| at.y), Some(px(LINE_HEIGHT * last_row)));
}

#[test]
fn each_glyph_is_painted_on_the_cluster_that_the_hit_test_finds_there() {
    for (text, direction, width) in [
        (format!("\"{ARABIC}{ENGLISH}\""), TextDirection::Ltr, 200.),
        (ARABIC.to_owned(), TextDirection::Rtl, 100.),
        (
            "Order 42: ابت 123 ثج (2)".to_owned(),
            TextDirection::Ltr,
            90.,
        ),
    ] {
        let line = shape_in(&text, Some(width), direction);
        assert!(rows(&line) > 1, "{text:?} wraps");
        let origins = line
            .bidi_glyph_origins(px(LINE_HEIGHT))
            .unwrap_or_else(|| panic!("{text:?} takes the bidi path"));
        assert_eq!(origins.len(), text.chars().count(), "{text:?}");
        for (index, origin) in origins {
            let middle = origin + point(px(ADVANCE / 2.), px(LINE_HEIGHT / 2.));
            assert_eq!(
                line.index_for_position(middle, px(LINE_HEIGHT)),
                Ok(index),
                "the glyph of byte {index} of {text:?} is painted at {origin:?}"
            );
        }
    }
}

#[test]
fn only_a_line_that_can_read_right_to_left_takes_the_bidi_path() {
    for text in ["Order 42: one, two (three).", "Καλημέρα κόσμε", "Բարեւ ձեզ"]
    {
        assert!(
            shape(text, Some(60.))
                .bidi_glyph_origins(px(LINE_HEIGHT))
                .is_none(),
            "{text:?} keeps the layout of the platform"
        );
    }
    for line in [
        shape("ابت", None),
        shape("abc שלום", None),
        shape_in("Thanks.", None, TextDirection::Rtl),
    ] {
        assert!(
            line.bidi_glyph_origins(px(LINE_HEIGHT)).is_some(),
            "{:?}",
            line.text
        );
    }
}

/// The width a text is drawn in by [`Aligned`].
const ALIGN_WIDTH: f32 = 300.;

/// A text drawn [`ALIGN_WIDTH`] wide in an alignment and a direction, with its layout kept for the hit tests.
struct Aligned {
    text: SharedString,
    align: TextAlign,
    direction: TextDirection,
    layout: Rc<RefCell<Option<TextLayout>>>,
}

impl Render for Aligned {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let text = StyledText::new(self.text.clone());
        self.layout.replace(Some(text.layout().clone()));
        div()
            .w(px(ALIGN_WIDTH))
            .text_align(self.align)
            .text_direction(self.direction)
            .child(text)
    }
}

/// The layout of `text` drawn by [`Aligned`], and the advance of each character of the test platform (0.6 em).
fn drawn_aligned(text: &str, align: TextAlign, direction: TextDirection) -> (TextLayout, Pixels) {
    let layout = Rc::new(RefCell::new(None));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (view_layout, shown) = (Rc::clone(&layout), SharedString::from(text.to_owned()));
    let (_, cx) = app.add_window_view(move |_, _| Aligned {
        text: shown,
        align,
        direction,
        layout: view_layout,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let advance =
        cx.update(|window, _| window.text_style().font_size.to_pixels(window.rem_size()) * 0.6);
    let layout = layout.borrow().clone();
    (
        layout.unwrap_or_else(|| panic!("{text:?} is drawn")),
        advance,
    )
}

#[test]
fn a_hit_test_finds_each_character_where_its_alignment_draws_it() {
    for (text, align, direction) in [
        (ARABIC.to_owned(), TextAlign::Right, TextDirection::Rtl),
        (
            format!("\"{ARABIC}{ENGLISH}\""),
            TextAlign::Center,
            TextDirection::Ltr,
        ),
        (
            "Order 42: one, two (three), four, five and six.".to_owned(),
            TextAlign::Center,
            TextDirection::Ltr,
        ),
    ] {
        let (layout, advance) = drawn_aligned(&text, align, direction);
        let bounds = layout.bounds();
        let half_row = layout.line_height() / 2.;
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(at, _)| at)
            .chain([text.len()])
            .collect();
        let carets: Vec<_> = boundaries
            .iter()
            .map(|at| {
                layout
                    .position_for_index(*at)
                    .unwrap_or_else(|| panic!("byte {at} of {text:?}"))
            })
            .collect();
        let mut checked = 0;
        for (pair, edges) in boundaries.windows(2).zip(carets.windows(2)) {
            let (start, end) = (edges[0], edges[1]);
            if start.y != end.y || ((end.x - start.x).abs() - advance).abs() > px(0.01) {
                continue;
            }
            let middle = point((start.x + end.x) / 2., start.y + half_row);
            assert_eq!(
                layout.index_for_position(middle),
                Ok(pair[0]),
                "byte {} of {text:?}, drawn at {middle:?}",
                pair[0]
            );
            let closest = layout.closest_index_for_position(middle);
            assert!(
                matches!(closest, Ok(at) if at == pair[0] || at == pair[1]),
                "the nearer side of byte {} of {text:?}: {closest:?}",
                pair[0]
            );
            checked += 1;
        }
        assert!(
            checked * 2 > text.chars().count(),
            "{checked} characters of {text:?}"
        );
        let mut row_ys: Vec<Pixels> = carets.iter().map(|at| at.y).collect();
        row_ys.dedup();
        assert!(row_ys.len() > 1, "{text:?} wraps");
        for y in row_ys {
            let on_row = carets.iter().filter(|at| at.y == y).map(|at| at.x);
            let (left, right) = on_row.fold((bounds.right(), bounds.left()), |(left, right), x| {
                (left.min(x), right.max(x))
            });
            let (space_left, space_right) = (left - bounds.left(), bounds.right() - right);
            let aligned = match align {
                TextAlign::Right => space_right.abs() <= advance + px(0.01),
                _ => (space_left - space_right).abs() <= advance + px(0.01),
            };
            assert!(
                aligned,
                "{align:?} row at {y:?} of {text:?}: {left:?}..{right:?} in {bounds:?}"
            );
        }
    }
}

#[test]
fn a_point_on_the_bottom_edge_of_a_line_is_on_the_next_line() {
    let text = "First line\nSecond line";
    let second = text.find('S').unwrap_or_default();
    let (layout, advance) = drawn_aligned(text, TextAlign::Left, TextDirection::Ltr);
    let bounds = layout.bounds();
    let edge = point(
        bounds.left() + advance / 4.,
        bounds.top() + layout.line_height(),
    );
    assert_eq!(layout.closest_index_for_position(edge), Ok(second));
    assert_eq!(layout.index_for_position(edge), Ok(second));
}
