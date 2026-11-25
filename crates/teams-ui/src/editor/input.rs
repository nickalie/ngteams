//! Text typed and composed through GPUI's input handler, and the editor's rich text on the clipboard.
//!
//! The input handler sees the document as one text, its blocks' texts joined by `\n`, counted in UTF-16 as the
//! platform counts it.

use std::collections::HashMap;
use std::ops::Range;

use gpui::{Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, px, size};
use serde_json::{Value, json};
use teams_proto::Mri;
use teams_proto::chat::{Node, RichText, parse_html};

use super::convert;
use super::doc::{Doc, Pos, Selection};
use super::{Editor, Step};

/// The key the editor's rich text has in a clipboard item's metadata.
const RICH_KEY: &str = "ngteams-rich-text";

/// The key another program's HTML has in a clipboard item's metadata, when the platform reads it.
const HTML_KEY: &str = "text/html";

/// The clipboard metadata of a copy: the rich text and the MRI each mention names, by `itemid`.
pub(super) fn clipboard_metadata(nodes: &[Node], mentioned: &[(String, Mri)]) -> String {
    let rich = RichText {
        nodes: nodes.to_vec(),
        truncated: false,
    };
    let mentions: serde_json::Map<String, Value> = mentioned
        .iter()
        .map(|(item_id, mri)| (item_id.clone(), Value::String(mri.as_str().to_owned())))
        .collect();
    json!({ RICH_KEY: { "text": rich, "mentions": mentions } }).to_string()
}

/// Whether a clipboard item's metadata is an editor's own rich text (a copy within the app).
pub(super) fn is_own_rich_text(metadata: &str) -> bool {
    serde_json::from_str::<Value>(metadata).is_ok_and(|value| value.get(RICH_KEY).is_some())
}

/// The document a clipboard item's metadata holds: an editor's rich text, or another program's HTML (read by
/// `teams-proto`'s sanitizing reader, its pictures as their `alt`).
pub(super) fn doc_of_metadata(metadata: &str) -> Option<Doc> {
    let value: Value = serde_json::from_str(metadata).ok()?;
    if let Some(rich) = value.get(RICH_KEY) {
        let text: RichText = serde_json::from_value(rich.get("text")?.clone()).ok()?;
        let mentioned: HashMap<String, String> = rich
            .get("mentions")
            .and_then(Value::as_object)
            .map(|mentions| {
                mentions
                    .iter()
                    .filter_map(|(item_id, mri)| Some((item_id.clone(), mri.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        return convert::from_nodes(&text.nodes, &mentioned, convert::Load::Paste).ok();
    }
    let html = value.get(HTML_KEY)?.as_str()?;
    convert::from_nodes(&parse_html(html).nodes, &HashMap::new(), convert::Load::Paste).ok()
}

impl Editor {
    /// The document as one text, its blocks joined by `\n`.
    fn flat(&self) -> String {
        self.doc.plain_text()
    }

    /// A place's byte offset in [`Self::flat`].
    fn flat_offset(&self, at: Pos) -> usize {
        self.doc.blocks[..at.block]
            .iter()
            .map(|block| block.len() + 1)
            .sum::<usize>()
            + at.offset
    }

    /// The place at a byte offset of [`Self::flat`].
    fn pos_of_flat(&self, mut offset: usize) -> Pos {
        for (index, block) in self.doc.blocks.iter().enumerate() {
            if offset <= block.len() {
                return Pos::new(index, offset);
            }
            offset -= block.len() + 1;
        }
        self.doc.end()
    }

    fn utf16_of(&self, at: Pos) -> usize {
        let flat = self.flat();
        let offset = flat.floor_char_boundary(self.flat_offset(at).min(flat.len()));
        flat[..offset].encode_utf16().count()
    }

    fn pos_of_utf16(&self, utf16: usize) -> Pos {
        let flat = self.flat();
        let mut count = 0;
        for (offset, character) in flat.char_indices() {
            if count >= utf16 {
                return self.pos_of_flat(offset);
            }
            count += character.len_utf16();
        }
        self.pos_of_flat(flat.len())
    }

    fn range_of_utf16(&self, range: &Range<usize>) -> (Pos, Pos) {
        (self.pos_of_utf16(range.start), self.pos_of_utf16(range.end))
    }

    /// Replaces between two places with `text` typed, its attributes those of the text before; the place after it.
    fn replace_between(&mut self, start: Pos, end: Pos, text: &str, cx: &mut Context<Self>) -> Pos {
        let pending = self.pending.take();
        let text = self.fit(start, end, &text.replace('\n', " "));
        let mut after = start;
        self.edit(Step::Typing, cx, |doc, _| {
            let at = doc.delete(start, end);
            let attrs = pending.unwrap_or_else(|| doc.blocks[at.block].attrs_at(at.offset));
            after = doc.insert_text(at, &text, attrs);
            Selection::caret(after)
        });
        after
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (start, end) = self.range_of_utf16(&range);
        *adjusted = Some(self.utf16_of(start)..self.utf16_of(end));
        Some(self.doc.text_between(start, end))
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let (start, end) = self.selection.range();
        Some(UTF16Selection {
            range: self.utf16_of(start)..self.utf16_of(end),
            reversed: self.selection.head < self.selection.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let (start, end) = self.marked?;
        Some(self.utf16_of(start)..self.utf16_of(end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let (start, end) = match (range, self.marked) {
            (Some(range), _) => self.range_of_utf16(&range),
            (None, Some(marked)) => marked,
            (None, None) => {
                self.marked = None;
                return self.type_text(text, cx);
            }
        };
        self.marked = None;
        self.replace_between(start, end, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        let (start, end) = match (range, self.marked) {
            (Some(range), _) => self.range_of_utf16(&range),
            (None, Some(marked)) => marked,
            (None, None) => self.selection.range(),
        };
        let after = self.replace_between(start, end, text, cx);
        self.marked = (after != start).then_some((start, after));
        if let Some(selected) = selected {
            let base = self.utf16_of(start);
            // a limit can cut the composed text: the selection stays in what went in
            let selection = Selection {
                anchor: self.pos_of_utf16(base + selected.start).min(after),
                head: self.pos_of_utf16(base + selected.end).min(after),
            };
            self.selection = selection;
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        element: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let start = self.pos_of_utf16(range.start);
        let end = self.pos_of_utf16(range.end);
        let line = self.line_height_at(start).unwrap_or(px(20.0));
        let from = self.caret_point(start).unwrap_or(element.origin);
        let to = self.caret_point(end).filter(|to| to.y == from.y).unwrap_or(from);
        Some(Bounds::new(from, size((to.x - from.x).max(px(1.0)), line)))
    }

    fn character_index_for_point(&mut self, at: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        let pos = self.pos_for_point(at)?;
        Some(self.utf16_of(pos))
    }

    fn set_selected_text_range(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) {
        let (anchor, head) = self.range_of_utf16(&range);
        self.select(Selection { anchor, head }, cx);
    }
}
