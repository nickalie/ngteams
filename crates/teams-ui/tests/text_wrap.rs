//! Text wrapping as GPUI does it, by UAX #14 (`icu_segmenter`): no line ends right after opening punctuation (LB14),
//! as in Teams, in the drawn wrap, the line wrapper and the clamped label's truncation alike.

use gpui::{IndentAdjustment, LineFragment, ParentElement as _, Styled as _, TestAppContext, TruncateFrom, px};

const TEXT: &str = "Add it to an [[account]] entry: 彼は「こんにちは」と言った, 他说‘你好’, 【【注意】】 （ｘ） \
    „Zitat“ and ((nested (brackets))) as ((((((((((((((((((((((((((((((((((((((((";

/// Whether the line that ends at byte `at` of `text` ends on an opening mark: a mark of the set with its content after
/// it. A `“` with a space after it closes a quote („Zitat“), and a line can end there.
fn ends_opening(text: &str, at: usize) -> bool {
    let opens = text[..at]
        .chars()
        .next_back()
        .is_some_and(|ends| "([{‘“«„‚（［｛｟「『【〔〈《〖〘〚｢".contains(ends));
    opens && text[at..].chars().next().is_some_and(|next| !next.is_whitespace())
}

/// Where each line of `text` ends when drawn `width` wide: shaped, then wrapped, as a text element does.
fn drawn_breaks(cx: &mut TestAppContext, text: &str, width: f32) -> gpui::Result<Vec<usize>> {
    let cx = cx.add_empty_window();
    cx.update(|window, _| {
        let run = window.text_style().to_run(text.len());
        let lines = window.text_system().shape_text(
            text.to_owned().into(),
            px(14.0),
            std::slice::from_ref(&run),
            Some(px(width)),
            None,
        )?;
        Ok(lines
            .iter()
            .flat_map(|line| {
                line.wrap_boundaries()
                    .iter()
                    .map(|at| line.unwrapped_layout.runs[at.run_ix].glyphs[at.glyph_ix].index)
                    .collect::<Vec<_>>()
            })
            .collect())
    })
}

#[gpui::test]
fn no_line_ends_on_an_opening_bracket_or_quote_cjk_ones_too(cx: &mut TestAppContext) {
    let run = TEXT.find(" ((((").unwrap() + 1;
    for width in (120..=720u16).step_by(8) {
        for at in drawn_breaks(cx, TEXT, f32::from(width)).unwrap() {
            // a run of brackets longer than the line breaks inside itself rather than overflow it
            let in_the_run = at > run + 4;
            assert!(
                !ends_opening(TEXT, at) || in_the_run,
                "a line ends on an opening mark at byte {at}, {width} px wide"
            );
        }
    }
    // the long run itself does break, somewhere, so it never runs off the line
    let breaks = drawn_breaks(cx, TEXT, 160.0).unwrap();
    assert!(
        breaks.iter().any(|&at| at > run + 4),
        "the bracket run is broken: {breaks:?}"
    );
}

fn system_font_wrapper(cx: &TestAppContext) -> gpui::LineWrapperHandle {
    cx.text_system().line_wrapper(gpui::font(".SystemUIFont"), px(14.0))
}

#[gpui::test]
fn the_line_wrapper_ends_no_line_on_an_opening_bracket_or_quote(cx: &mut TestAppContext) {
    let run = TEXT.find(" ((((").unwrap() + 1;
    let mut wrapper = system_font_wrapper(cx);
    for width in (60..=720u16).step_by(2) {
        let breaks = wrapper
            .wrap_line(
                &[LineFragment::text(TEXT)],
                px(f32::from(width)),
                IndentAdjustment::default(),
            )
            .map(|boundary| boundary.ix)
            .collect::<Vec<_>>();
        for at in breaks {
            assert!(
                !ends_opening(TEXT, at) || at > run + 4,
                "a line ends on an opening mark at byte {at}, {width} px wide"
            );
        }
    }
}

#[gpui::test]
fn a_clamped_label_is_cut_where_its_lines_wrap(cx: &mut TestAppContext) {
    // `line_clamp` with an ellipsis: what the truncation keeps must wrap into the allowed lines, or its last line
    // overflows unmarked
    let mut wrapper = system_font_wrapper(cx);
    let run = gpui::TextRun {
        len: TEXT.len(),
        font: gpui::font(".SystemUIFont"),
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    for max_lines in 2..=3 {
        for width in (60..=400u16).step_by(2) {
            let width = px(f32::from(width));
            let (kept, _) = wrapper.truncate_wrapped_line(
                TEXT.into(),
                width,
                max_lines,
                "\u{2026}",
                std::slice::from_ref(&run),
                TruncateFrom::End,
            );
            let lines = 1 + wrapper
                .wrap_line(&[LineFragment::text(&kept)], width, IndentAdjustment::default())
                .count();
            assert!(
                lines <= max_lines,
                "'{kept}' wraps into {lines} lines at {width:?}, {max_lines} allowed"
            );
        }
    }
}

const TITLE: &str = "A made-up video title, much longer than any card has room for on four lines, so that the card \
    must cut it where its last line ends and mark the cut, as Teams does with every text that has maxLines";

/// The text a card's text block draws `width` wide, with `wrap` and `max_lines` as the card gives them.
fn clamped_text(cx: &mut TestAppContext, wrap: bool, max_lines: Option<u32>, width: f32) -> String {
    let text = gpui::StyledText::new(TITLE);
    let layout = text.layout().clone();
    let cx = cx.add_empty_window();
    cx.draw(
        gpui::point(px(0.0), px(0.0)),
        gpui::size(
            gpui::AvailableSpace::Definite(px(width)),
            gpui::AvailableSpace::Definite(px(600.0)),
        ),
        |_, _| teams_ui::card_view::clamped(gpui::div().w(px(width)), wrap, max_lines).child(text),
    );
    layout.text()
}

#[gpui::test]
fn a_card_text_cut_by_its_max_lines_ends_with_an_ellipsis(cx: &mut TestAppContext) {
    for width in [180.0, 273.0, 416.0] {
        let shown = clamped_text(cx, true, Some(4), width);
        assert!(
            shown.ends_with('\u{2026}'),
            "four lines {width} px wide end with '…': {shown:?}"
        );
        assert!(shown.len() < TITLE.len(), "cut: {shown:?}");
        let one = clamped_text(cx, false, None, width);
        assert!(
            one.ends_with('\u{2026}'),
            "a text that does not wrap ends with '…': {one:?}"
        );
    }
    assert_eq!(
        clamped_text(cx, true, Some(40), 416.0),
        TITLE,
        "a text that fits its lines is not cut"
    );
}
