//! Bidirectional text (Unicode Standard Annex #9) for wrapped lines.
//!
//! A platform text system shapes a line in visual order: right-to-left runs come out reversed and the glyph x
//! positions do not follow the text order. Wrapping, painting and hit tests of a wrapped line assume that x grows with
//! the byte index, so a line that holds right-to-left text, or whose paragraph is right to left, is put into logical
//! order first ([`logical_layout`]): each cluster keeps its glyphs and its advance, and the clusters follow each other
//! in text order. The line wraps on that layout. Then each row is put back into visual order on its own
//! ([`BidiRows`]), as a browser does after it breaks the lines of a paragraph.
//!
//! The paragraph direction is the text style's ([`crate::TextDirection`]): left-to-right unless set, as in HTML
//! without a `dir` attribute. Left-to-right text that holds no right-to-left character keeps the layout of the platform
//! unchanged.

use crate::{
    FontRun, LineLayout, Pixels, PlatformTextSystem, ShapedGlyph, ShapedRun, TextDirection,
    WrapBoundary, px,
};
use collections::FxHashMap;
use std::ops::Range;
use unicode_bidi::{BidiClass, BidiInfo, Level, bidi_class};

const LEFT_TO_RIGHT_MARK: &str = "\u{200E}";
const RIGHT_TO_LEFT_MARK: &str = "\u{200F}";

/// If the text holds a character that can show right to left: a right-to-left letter or an embedding, override or
/// isolate that starts right-to-left text.
pub(crate) fn needs_bidi(text: &str) -> bool {
    text.chars().any(|character| {
        character >= '\u{0590}'
            && matches!(
                bidi_class(character),
                BidiClass::R | BidiClass::AL | BidiClass::RLE | BidiClass::RLO | BidiClass::RLI
            )
    })
}

/// The direction of the first strong letter of the text, as HTML's `dir="auto"` takes it (rules P2 and P3 of the
/// Unicode Bidirectional Algorithm, isolates not skipped): `None` when the text has no strong letter.
pub fn first_strong_direction(text: &str) -> Option<TextDirection> {
    text.chars()
        .find_map(|character| match bidi_class(character) {
            BidiClass::L => Some(TextDirection::Ltr),
            BidiClass::R | BidiClass::AL => Some(TextDirection::Rtl),
            _ => None,
        })
}

fn starts_left_to_right(text: &str) -> bool {
    first_strong_direction(text) != Some(TextDirection::Rtl)
}

/// Shapes the line as a paragraph of the given direction. Each platform shaper takes the direction of the paragraph
/// from its first strong character (cosmic-text and CoreText by themselves, DirectWrite through the reading direction
/// that `gpui-pre-windows` sets from [`first_strong_direction`]), so a mark of the direction goes before a line whose
/// first strong character has the other one. Without it, the shaper mirrors brackets and orders the neutral
/// characters for the wrong direction.
pub(crate) fn shape_in_direction(
    platform: &dyn PlatformTextSystem,
    text: &str,
    font_size: Pixels,
    runs: &[FontRun],
    rtl: bool,
) -> LineLayout {
    if runs.is_empty() || starts_left_to_right(text) != rtl {
        return platform.layout_line(text, font_size, runs);
    }
    let mark = if rtl {
        RIGHT_TO_LEFT_MARK
    } else {
        LEFT_TO_RIGHT_MARK
    };
    let mark_len = mark.len();
    let marked = format!("{mark}{text}");
    let mut marked_runs = runs.to_vec();
    marked_runs[0].len += mark_len;
    let mut layout = platform.layout_line(&marked, font_size, &marked_runs);
    for run in &mut layout.runs {
        run.glyphs.retain(|glyph| glyph.index >= mark_len);
        for glyph in &mut run.glyphs {
            glyph.index -= mark_len;
        }
    }
    layout.runs.retain(|run| !run.glyphs.is_empty());
    layout.len = text.len();
    layout
}

struct Placed<'a> {
    run: usize,
    order: usize,
    glyph: &'a ShapedGlyph,
}

/// Puts the glyphs of a line into text order, each cluster with its own advance, and the clusters one after another
/// from x = 0. A cluster is the glyphs that start at one byte index. Its visual left edge is its leftmost glyph, and
/// its advance reaches to the left edge of the next cluster on the screen. Clusters at the same x keep the order of
/// the platform, which is visual.
pub(crate) fn logical_layout(layout: &LineLayout) -> LineLayout {
    let mut placed: Vec<Placed<'_>> = layout
        .runs
        .iter()
        .enumerate()
        .flat_map(|(run, shaped)| shaped.glyphs.iter().map(move |glyph| (run, glyph)))
        .enumerate()
        .map(|(order, (run, glyph))| Placed { run, order, glyph })
        .collect();
    if placed.is_empty() {
        return copy_layout(layout, Vec::new(), layout.width);
    }

    let mut by_index: FxHashMap<usize, (Pixels, usize)> = FxHashMap::default();
    for item in &placed {
        let (left, first) = by_index
            .entry(item.glyph.index)
            .or_insert((item.glyph.position.x, item.order));
        if item.glyph.position.x < *left {
            *left = item.glyph.position.x;
        }
        *first = (*first).min(item.order);
    }
    let mut clusters: Vec<(usize, Pixels, usize)> = by_index
        .into_iter()
        .map(|(index, (left, first))| (index, left, first))
        .collect();
    clusters.sort_by(|a, b| a.1.0.total_cmp(&b.1.0).then(a.2.cmp(&b.2)));
    let right_edge = layout
        .width
        .max(clusters.last().map_or(px(0.), |cluster| cluster.1));
    let mut advances: Vec<(usize, Pixels, Pixels)> = clusters
        .iter()
        .enumerate()
        .map(|(ix, (index, left, _))| {
            let next = clusters.get(ix + 1).map_or(right_edge, |cluster| cluster.1);
            (*index, *left, next - *left)
        })
        .collect();
    advances.sort_by_key(|(index, _, _)| *index);

    placed.sort_by_key(|item| (item.glyph.index, item.order));
    let mut runs: Vec<ShapedRun> = Vec::new();
    let mut pen = px(0.);
    let mut cluster_ix = 0;
    let mut cluster_pen = px(0.);
    for item in &placed {
        while advances[cluster_ix].0 != item.glyph.index {
            pen += advances[cluster_ix].2;
            cluster_ix += 1;
            cluster_pen = pen;
        }
        let (_, left, _) = advances[cluster_ix];
        let mut glyph = item.glyph.clone();
        glyph.position.x = cluster_pen + (item.glyph.position.x - left);
        let font_id = layout.runs[item.run].font_id;
        match runs.last_mut() {
            Some(run) if run.font_id == font_id => run.glyphs.push(glyph),
            _ => runs.push(ShapedRun {
                font_id,
                glyphs: vec![glyph],
            }),
        }
    }
    let width = advances.iter().map(|(_, _, advance)| *advance).sum();
    copy_layout(layout, runs, width)
}

fn copy_layout(layout: &LineLayout, runs: Vec<ShapedRun>, width: Pixels) -> LineLayout {
    LineLayout {
        font_size: layout.font_size,
        width,
        ascent: layout.ascent,
        descent: layout.descent,
        runs,
        len: layout.len,
    }
}

/// A cluster as a row shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VisualCluster {
    /// The bytes of the cluster.
    pub range: Range<usize>,
    /// The left edge, from the left edge of the row.
    pub x: Pixels,
    pub width: Pixels,
    pub rtl: bool,
}

/// The visual order of each row of a wrapped line whose layout is in text order.
#[derive(Debug, Default)]
pub(crate) struct BidiRows {
    /// The x of each glyph, from the left edge of its row, in the order of the layout's runs.
    glyph_x: Vec<Pixels>,
    /// The row of each glyph, in the same order.
    glyph_row: Vec<usize>,
    /// The clusters of all rows, row after row, each row from left to right.
    clusters: Vec<VisualCluster>,
    /// The first cluster of each row, and the end of `clusters`.
    row_starts: Vec<usize>,
    /// The bytes of each row.
    row_ranges: Vec<Range<usize>>,
    /// If the paragraph is right to left.
    rtl: bool,
}

struct LogicalCluster {
    range: Range<usize>,
    x: Pixels,
    width: Pixels,
    glyphs: Range<usize>,
}

impl BidiRows {
    /// Lays out the rows of `layout` (in text order) that `wrap_boundaries` cut, in visual order.
    pub fn new(
        text: &str,
        layout: &LineLayout,
        wrap_boundaries: &[WrapBoundary],
        rtl: bool,
    ) -> Self {
        let glyphs: Vec<&ShapedGlyph> = layout.runs.iter().flat_map(|run| &run.glyphs).collect();
        let clusters = logical_clusters(text, layout, &glyphs);
        let row_glyph_starts = row_glyph_starts(layout, wrap_boundaries);
        let level = if rtl { Level::rtl() } else { Level::ltr() };
        let info = BidiInfo::new(text, Some(level));

        let mut rows = BidiRows {
            glyph_x: vec![px(0.); glyphs.len()],
            glyph_row: vec![0; glyphs.len()],
            rtl,
            ..Default::default()
        };
        for cluster in &clusters {
            for glyph in cluster.glyphs.clone() {
                rows.glyph_x[glyph] = glyphs[glyph].position.x - cluster.x;
            }
        }
        let mut cluster_ix = 0;
        for (row, glyph_start) in row_glyph_starts.iter().enumerate() {
            let glyph_end = row_glyph_starts
                .get(row + 1)
                .copied()
                .unwrap_or(glyphs.len());
            let first = cluster_ix;
            while cluster_ix < clusters.len() && clusters[cluster_ix].glyphs.start < glyph_end {
                cluster_ix += 1;
            }
            let row_clusters = &clusters[first..cluster_ix];
            let row_range = match (row_clusters.first(), row_clusters.last()) {
                (Some(first), Some(last)) => first.range.start..last.range.end,
                _ => {
                    let at = glyphs
                        .get(*glyph_start)
                        .map_or(text.len(), |glyph| glyph.index);
                    at..at
                }
            };
            rows.row_starts.push(rows.clusters.len());
            rows.row_ranges.push(row_range.clone());
            rows.place_row(&info, row, row_range, row_clusters);
        }
        rows.row_starts.push(rows.clusters.len());
        rows
    }

    fn place_row(
        &mut self,
        info: &BidiInfo<'_>,
        row: usize,
        row_range: Range<usize>,
        row_clusters: &[LogicalCluster],
    ) {
        for cluster in row_clusters {
            for glyph in cluster.glyphs.clone() {
                self.glyph_row[glyph] = row;
            }
        }
        let mut x = px(0.);
        for (run, rtl) in visual_runs(info, row_range) {
            let in_run = row_clusters
                .iter()
                .filter(|cluster| run.contains(&cluster.range.start));
            let ordered: Vec<&LogicalCluster> = if rtl {
                in_run.rev().collect()
            } else {
                in_run.collect()
            };
            for cluster in ordered {
                for glyph in cluster.glyphs.clone() {
                    self.glyph_x[glyph] += x;
                }
                self.clusters.push(VisualCluster {
                    range: cluster.range.clone(),
                    x,
                    width: cluster.width,
                    rtl,
                });
                x += cluster.width;
            }
        }
    }

    /// The number of rows.
    pub fn row_count(&self) -> usize {
        self.row_ranges.len()
    }

    /// The clusters of a row, from left to right.
    pub fn row(&self, row: usize) -> &[VisualCluster] {
        match (self.row_starts.get(row), self.row_starts.get(row + 1)) {
            (Some(start), Some(end)) => &self.clusters[*start..*end],
            _ => &[],
        }
    }

    /// The width of a row.
    pub fn row_width(&self, row: usize) -> Pixels {
        self.row(row).iter().map(|cluster| cluster.width).sum()
    }

    /// The x of a glyph (its position in the layout's runs, counted over all runs) from the left edge of its row, and
    /// the row.
    pub fn glyph(&self, flat_ix: usize) -> (Pixels, usize) {
        (
            self.glyph_x.get(flat_ix).copied().unwrap_or_default(),
            self.glyph_row.get(flat_ix).copied().unwrap_or_default(),
        )
    }

    /// The byte index for a point in a row, as [`crate::WrappedLineLayout::index_for_position`] gives it.
    pub fn index_for_position(&self, row: usize, x: Pixels, closest: bool) -> Result<usize, usize> {
        let Some(range) = self.row_ranges.get(row) else {
            return Err(self.row_ranges.last().map_or(0, |range| range.end));
        };
        let (left_end, right_end) = if self.rtl {
            (range.end, range.start)
        } else {
            (range.start, range.end)
        };
        if x < px(0.) {
            return Err(left_end);
        }
        let Some(cluster) = self
            .row(row)
            .iter()
            .find(|cluster| x < cluster.x + cluster.width)
        else {
            return Err(right_end);
        };
        if !closest {
            return Ok(cluster.range.start);
        }
        let left_half = x < cluster.x + cluster.width / 2.;
        Ok(if left_half == cluster.rtl {
            cluster.range.end
        } else {
            cluster.range.start
        })
    }

    /// The point of a byte index: the edge of its character where the caret goes before it, or the end edge of the
    /// row's last character for the end of a row.
    pub fn position_for_index(&self, index: usize) -> Option<(usize, Pixels)> {
        let row = self
            .row_ranges
            .iter()
            .position(|range| index <= range.end)?;
        let clusters = self.row(row);
        if let Some(cluster) = clusters
            .iter()
            .find(|cluster| cluster.range.contains(&index))
        {
            let at_start = index == cluster.range.start;
            let x = if cluster.rtl == at_start {
                cluster.x + cluster.width
            } else {
                cluster.x
            };
            return Some((row, x));
        }
        let x = clusters
            .iter()
            .max_by_key(|cluster| cluster.range.start)
            .map_or(px(0.), |last| {
                if last.rtl {
                    last.x
                } else {
                    last.x + last.width
                }
            });
        Some((row, x))
    }
}

fn logical_clusters(
    text: &str,
    layout: &LineLayout,
    glyphs: &[&ShapedGlyph],
) -> Vec<LogicalCluster> {
    let mut clusters: Vec<LogicalCluster> = Vec::new();
    for (ix, glyph) in glyphs.iter().enumerate() {
        match clusters.last_mut() {
            Some(cluster) if cluster.range.start == glyph.index => {
                cluster.glyphs.end = ix + 1;
                cluster.x = cluster.x.min(glyph.position.x);
            }
            _ => clusters.push(LogicalCluster {
                range: glyph.index..glyph.index,
                x: glyph.position.x,
                width: px(0.),
                glyphs: ix..ix + 1,
            }),
        }
    }
    let ends: Vec<(usize, Pixels)> = clusters
        .iter()
        .skip(1)
        .map(|cluster| (cluster.range.start, cluster.x))
        .chain([(text.len(), layout.width)])
        .collect();
    for (cluster, (end, end_x)) in clusters.iter_mut().zip(ends) {
        cluster.range.end = end;
        cluster.width = (end_x - cluster.x).max(px(0.));
    }
    clusters
}

fn row_glyph_starts(layout: &LineLayout, wrap_boundaries: &[WrapBoundary]) -> Vec<usize> {
    let mut run_offsets = Vec::with_capacity(layout.runs.len());
    let mut offset = 0;
    for run in &layout.runs {
        run_offsets.push(offset);
        offset += run.glyphs.len();
    }
    std::iter::once(0)
        .chain(wrap_boundaries.iter().map(|boundary| {
            run_offsets.get(boundary.run_ix).copied().unwrap_or(offset) + boundary.glyph_ix
        }))
        .collect()
}

/// The level runs of a row in visual order, each with its direction.
fn visual_runs(info: &BidiInfo<'_>, row: Range<usize>) -> Vec<(Range<usize>, bool)> {
    let mut runs = Vec::new();
    for paragraph in &info.paragraphs {
        let start = row.start.max(paragraph.range.start);
        let end = row.end.min(paragraph.range.end);
        if start >= end {
            continue;
        }
        let (levels, level_runs) = info.visual_runs(paragraph, start..end);
        runs.extend(level_runs.into_iter().map(|run| {
            let rtl = levels[run.start].is_rtl();
            (run, rtl)
        }));
    }
    runs
}

/// A stretch of a row in one decoration run, from `x` and `width` wide.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Segment {
    pub run: usize,
    pub x: Pixels,
    pub width: Pixels,
}

/// The stretches of a row where each decoration run shows, left to right: neighbour clusters of one run make one
/// stretch.
pub(crate) fn row_segments(clusters: &[VisualCluster], run_ends: &[usize]) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    for cluster in clusters {
        let run = run_ends.partition_point(|end| *end <= cluster.range.start);
        match segments.last_mut() {
            Some(segment)
                if segment.run == run
                    && (segment.x + segment.width - cluster.x).abs() < px(0.01) =>
            {
                segment.width += cluster.width;
            }
            _ => segments.push(Segment {
                run,
                x: cluster.x,
                width: cluster.width,
            }),
        }
    }
    segments
}

/// The decoration run of a byte index, from the ends of the runs.
pub(crate) fn run_at(run_ends: &[usize], index: usize) -> usize {
    run_ends.partition_point(|end| *end <= index)
}
