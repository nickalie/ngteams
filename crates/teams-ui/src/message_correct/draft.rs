//! A draft as the proofreading tool reads and writes it: its words between numbered markers ([`encode`]), and back
//! ([`Encoded::decode`]), its shape as it was.
//!
//! - **Blocks**: every block with words (a paragraph, a heading, a line of a quote, a list item) is `<xN>…</xN>`. A block
//!   of code, a picture, a rule and a blank line never go to the tool; they stay where they are.
//! - **Runs**: words with a format (a mark, a colour, a link) are `<xN>…</xN>` inside their block; what must come back
//!   exactly (a mention, inline code, a link that shows its own address, a line break) is `<xN/>`. A link's address is
//!   judged on all its runs together, so one formatted in parts goes out as one `<xN/>` and comes back in order.
//! - **Strict to read**, unlike a translation's: the answer is used only when it has every marker exactly once, each in
//!   its own block, and no block lost its words. A correction the user may send must not lose a mention or a link.

use std::fmt::Write as _;

use teams_proto::chat::Marks;

use crate::chat_translate::codec::{Token, escape, tokens, unescape};
use crate::editor::doc::{Attrs, Block, Doc, Kind, Run};

/// What a marker stands for, and the block it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Slot {
    /// A block's words.
    Block(usize),
    /// Words with these attributes.
    Styled(usize, Attrs),
    /// Runs put back as they were, in order.
    Atom(usize, Vec<Run>),
}

/// A draft encoded for the tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    /// What the tool reads.
    pub text: String,
    doc: Doc,
    slots: Vec<Slot>,
    words: bool,
}

/// `doc` as the tool reads it (see the module docs).
#[must_use]
pub fn encode(doc: &Doc) -> Encoded {
    let mut encoded = Encoded {
        text: String::new(),
        doc: doc.clone(),
        slots: Vec::new(),
        words: false,
    };
    for (index, block) in doc.blocks.iter().enumerate() {
        if has_words_to_read(block) {
            encoded.block(index, block);
        }
    }
    encoded
}

/// Whether `block` goes to the tool.
fn has_words_to_read(block: &Block) -> bool {
    !block.kind.is_atom() && block.kind != Kind::Code && !block.is_blank()
}

/// Whether `run` must come back exactly as it is.
fn kept_whole(run: &Run) -> bool {
    run.attrs.mention.is_some()
        || run.attrs.marks.contains(Marks::CODE)
        || run
            .attrs
            .link
            .as_deref()
            .is_some_and(|href| shows_address(&run.text, href))
}

/// Which of `runs` must come back exactly as they are: each one [`kept_whole`] alone, and every run of a link whose
/// runs, formatted apart, show its address together.
fn kept_whole_runs(runs: &[Run]) -> Vec<bool> {
    let mut whole: Vec<bool> = runs.iter().map(kept_whole).collect();
    let mut start = 0;
    while start < runs.len() {
        let href = runs[start].attrs.link.as_deref();
        let end = runs[start..]
            .iter()
            .position(|run| run.attrs.link.as_deref() != href)
            .map_or(runs.len(), |len| start + len);
        if let Some(href) = href {
            let text: String = runs[start..end].iter().map(|run| run.text.as_str()).collect();
            if shows_address(&text, href) {
                whole[start..end].fill(true);
            }
        }
        start = end;
    }
    whole
}

/// Whether a link's text is its own address (with or without its scheme).
fn shows_address(text: &str, href: &str) -> bool {
    let text = text.trim();
    let bare = href.split_once("://").map_or(href, |(_, rest)| rest);
    text == href || text == bare
}

impl Encoded {
    /// The draft it was made from.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Whether it holds any word for the tool to read.
    #[must_use]
    pub fn has_words(&self) -> bool {
        self.words
    }

    fn add(&mut self, slot: Slot) -> usize {
        self.slots.push(slot);
        self.slots.len() - 1
    }

    fn block(&mut self, index: usize, block: &Block) {
        let n = self.add(Slot::Block(index));
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        let _ = write!(self.text, "<x{n}>");
        let whole = kept_whole_runs(&block.runs);
        let mut at = 0;
        while at < block.runs.len() {
            if !whole[at] {
                self.run(index, &block.runs[at]);
                at += 1;
                continue;
            }
            // The kept runs of one link go as one atom, so an answer can't reorder or split its text.
            let link = block.runs[at].attrs.link.as_deref();
            let len = block.runs[at + 1..]
                .iter()
                .zip(&whole[at + 1..])
                .take_while(|(run, whole)| **whole && link.is_some() && run.attrs.link.as_deref() == link)
                .count();
            self.atom(index, block.runs[at..=at + len].to_vec());
            at += len + 1;
        }
        let _ = write!(self.text, "</x{n}>");
    }

    fn run(&mut self, block: usize, run: &Run) {
        for (at, line) in run.text.split('\n').enumerate() {
            if at > 0 {
                self.atom(block, vec![Run::new("\n", run.attrs.clone())]);
            }
            self.line(block, line, &run.attrs);
        }
    }

    fn atom(&mut self, block: usize, runs: Vec<Run>) {
        let n = self.add(Slot::Atom(block, runs));
        let _ = write!(self.text, "<x{n}/>");
    }

    /// Words of one line of a run.
    fn line(&mut self, block: usize, line: &str, attrs: &Attrs) {
        if line.is_empty() {
            return;
        }
        self.words |= line.chars().any(char::is_alphanumeric);
        if *attrs == Attrs::default() {
            self.text.push_str(&escape(line));
            return;
        }
        let n = self.add(Slot::Styled(block, attrs.clone()));
        let _ = write!(self.text, "<x{n}>{}</x{n}>", escape(line));
    }

    /// The draft as `answer` corrects it; `None` when it doesn't keep every marker in its place (see the module docs).
    #[must_use]
    pub fn decode(&self, answer: &str) -> Option<Doc> {
        let mut reading = Reading {
            slots: &self.slots,
            seen: vec![false; self.slots.len()],
            block: None,
            styled: None,
            done: Vec::new(),
        };
        for token in tokens(answer) {
            reading.take(&token)?;
        }
        if reading.block.is_some() || reading.seen.iter().any(|seen| !seen) {
            return None;
        }
        let mut doc = self.doc.clone();
        for (index, runs, words) in reading.done {
            let block = doc.blocks.get(index)?;
            let corrected = Block::new(block.kind, block.indent, runs);
            let had_words = block
                .runs
                .iter()
                .zip(kept_whole_runs(&block.runs))
                .any(|(run, whole)| !whole && run.text.chars().any(char::is_alphanumeric));
            if corrected.is_blank() || (had_words && !words) {
                return None;
            }
            doc.blocks[index] = corrected;
        }
        Some(doc)
    }
}

/// A block being read from an answer: its marker, its index in the draft and its runs so far.
struct Open {
    marker: usize,
    index: usize,
    runs: Vec<Run>,
    /// Words read from text, excluding the atoms restored unchanged.
    words: bool,
}

/// A styled marker being read; a link needs a visible label to survive decoding.
struct Styled<'a> {
    marker: usize,
    attrs: &'a Attrs,
    visible: bool,
}

/// An answer being read.
struct Reading<'a> {
    slots: &'a [Slot],
    seen: Vec<bool>,
    block: Option<Open>,
    styled: Option<Styled<'a>>,
    done: Vec<(usize, Vec<Run>, bool)>,
}

impl Reading<'_> {
    fn take(&mut self, token: &Token<'_>) -> Option<()> {
        match token {
            Token::Text(text) => self.text(text),
            Token::Open(n) => self.open(*n),
            Token::Close(n) => self.close(*n),
            Token::Atom(n) => self.atom(*n),
        }
    }

    /// Marks marker `n` as met, the first time only.
    fn first_time(&mut self, n: usize) -> Option<()> {
        let seen = self.seen.get_mut(n)?;
        (!std::mem::replace(seen, true)).then_some(())
    }

    fn text(&mut self, text: &str) -> Option<()> {
        let Some(open) = &mut self.block else {
            return text.trim().is_empty().then_some(());
        };
        let text = unescape(text);
        open.words |= text.chars().any(char::is_alphanumeric);
        let attrs = self.styled.as_mut().map_or_else(Attrs::default, |styled| {
            styled.visible |= !text.trim().is_empty();
            styled.attrs.clone()
        });
        open.runs.push(Run::new(text, attrs));
        Some(())
    }

    fn open(&mut self, n: usize) -> Option<()> {
        self.first_time(n)?;
        match (self.slots.get(n)?, &self.block, &self.styled) {
            (Slot::Block(index), None, _) => {
                self.block = Some(Open {
                    marker: n,
                    index: *index,
                    runs: Vec::new(),
                    words: false,
                });
                Some(())
            }
            (Slot::Styled(block, attrs), Some(open), None) if open.index == *block => {
                self.styled = Some(Styled {
                    marker: n,
                    attrs,
                    visible: false,
                });
                Some(())
            }
            _ => None,
        }
    }

    fn close(&mut self, n: usize) -> Option<()> {
        if let Some(styled) = self.styled.take_if(|styled| styled.marker == n) {
            return (styled.attrs.link.is_none() || styled.visible).then_some(());
        }
        let open = self.block.take_if(|open| open.marker == n && self.styled.is_none())?;
        self.done.push((open.index, open.runs, open.words));
        Some(())
    }

    fn atom(&mut self, n: usize) -> Option<()> {
        self.first_time(n)?;
        let Slot::Atom(block, runs) = self.slots.get(n)? else {
            return None;
        };
        let open = self
            .block
            .as_mut()
            .filter(|open| open.index == *block && self.styled.is_none())?;
        open.runs.extend(runs.iter().cloned());
        Some(())
    }
}
