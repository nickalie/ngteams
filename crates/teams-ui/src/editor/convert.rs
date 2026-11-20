//! The editor's document as `teams-proto`'s rich text and back: what a message is sent as, and what an edited message,
//! a paste or a draft loads.
//!
//! Written ([`to_nodes`]) as Teams' editor writes a message (`docs/protocol/chat.md`, "Formatted text"): a paragraph
//! a `p` (an empty one `<p>&nbsp;</p>`), a heading `h1`–`h3`, an indented paragraph `<p style="margin-left:40px">`,
//! quote lines one after the other one `blockquote` of paragraphs, list items one after the other one list (`ul` or
//! `ol`, a deeper item a list nested in the item before it), a block of code a `pre`, a rule `hr`; in a block, runs of
//! one link one `a`, runs of one colour, highlight and size one `span`, the marks inside it, a mention its span. A
//! space after a space, or at a block's edge, is a no-break space, as a browser would otherwise drop it. Blank blocks
//! at either end aren't sent.

use std::collections::HashMap;
use std::hash::BuildHasher;

use teams_proto::Mri;
use teams_proto::chat::{Code, CodeRun, List, Marks, Node, Quote, Table, Token};

use super::doc::{Attrs, Block, Doc, Kind, MAX_INDENT, Run};

/// Why a message's text can't be loaded into the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CantLoad {
    /// A picture a message shows: the editor takes pictures pasted into it, not those a message already has.
    #[error("it shows an image")]
    Image,
    #[error("it was read only in part")]
    Truncated,
    /// What the editor would send back otherwise than it was (a block of code in a language, a numbered list from
    /// another number, a table), though the edit didn't change it.
    #[error("it has formatting the box can't keep")]
    Formatting,
}

/// What a load is for, which sets what is done with what the editor can't send back as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    /// An edit of a message: a picture, a block of code in a language (but `plaintext`), a numbered list from another
    /// number than 1 and a table are refused, so the edit sends back only what the user changed.
    Edit,
    /// A text shown to edit but kept as it was until edited (an event's body): a picture refused; a block of code's
    /// language and a list's first number dropped; a table's rows made lines.
    Body,
    /// A paste: a picture is its `alt`; a block of code's language and a list's first number dropped; a table's rows
    /// made lines.
    Paste,
}

/// The document as rich text, and the MRI each mention names by its `item_id` (`0`, `1`, … in order). Nothing when
/// it shows nothing. A picture is left out (a copy's text); [`to_nodes_with`] writes it.
#[must_use]
pub fn to_nodes(doc: &Doc) -> (Vec<Node>, Vec<(String, Mri)>) {
    to_nodes_with(doc, &|_| None)
}

/// [`to_nodes`], each picture written as `pictures` gives it by its id (a message's `<img>` once it is sent, in a
/// paragraph of its own as Teams writes one; a sticker's element as it is); one it gives nothing for is left out.
#[must_use]
pub fn to_nodes_with(doc: &Doc, pictures: &dyn Fn(u64) -> Option<Node>) -> (Vec<Node>, Vec<(String, Mri)>) {
    let first = doc.blocks.iter().position(|block| !block.is_blank());
    let last = doc.blocks.iter().rposition(|block| !block.is_blank());
    let (Some(first), Some(last)) = (first, last) else {
        return (Vec::new(), Vec::new());
    };
    let mut writer = Writer {
        nodes: Vec::new(),
        mentioned: Vec::new(),
        pictures,
    };
    let blocks = &doc.blocks[first..=last];
    let mut index = 0;
    while index < blocks.len() {
        index = writer.block(blocks, index);
    }
    (writer.nodes, writer.mentioned)
}

struct Writer<'a> {
    nodes: Vec<Node>,
    mentioned: Vec<(String, Mri)>,
    pictures: &'a dyn Fn(u64) -> Option<Node>,
}

impl Writer<'_> {
    /// Writes the block at `index` (and those that go with it: a quote's lines, a list's items); the index after them.
    fn block(&mut self, blocks: &[Block], index: usize) -> usize {
        let block = &blocks[index];
        match block.kind {
            Kind::Paragraph if block.indent > 0 => {
                let children = self.inline(block);
                self.nodes.push(Node::Indented {
                    depth: block.indent,
                    children,
                });
                index + 1
            }
            Kind::Paragraph => {
                let children = self.inline(block);
                self.nodes.push(Node::Paragraph(children));
                index + 1
            }
            Kind::Heading(level) => {
                let children = self.inline(block);
                self.nodes.push(Node::Heading { level, children });
                index + 1
            }
            Kind::Rule => {
                self.nodes.push(Node::Rule);
                index + 1
            }
            Kind::Picture(id) => {
                match (self.pictures)(id) {
                    Some(Node::Image(picture)) => self.nodes.push(Node::Paragraph(vec![Node::Image(picture)])),
                    Some(node) => self.nodes.push(node),
                    None => {}
                }
                index + 1
            }
            Kind::Code => {
                self.nodes.push(Node::Code(Code {
                    language: Some("plaintext".to_owned()),
                    runs: vec![CodeRun {
                        text: block.text(),
                        token: Token::Plain,
                    }],
                }));
                index + 1
            }
            Kind::Quote => {
                let end = blocks[index..]
                    .iter()
                    .position(|block| block.kind != Kind::Quote)
                    .map_or(blocks.len(), |offset| index + offset);
                let body = blocks[index..end]
                    .iter()
                    .map(|line| Node::Paragraph(self.inline(line)))
                    .collect();
                self.nodes.push(Node::Quote(Quote {
                    body,
                    ..Quote::default()
                }));
                end
            }
            Kind::Bullet | Kind::Numbered => {
                let (list, end) = self.list(blocks, index, block.indent.min(MAX_INDENT));
                self.nodes.push(list);
                end
            }
        }
    }

    /// The list starting at `index`, its items at `depth`: items of its kind at that depth, each with the deeper items
    /// after it as lists nested in it; the index after it.
    fn list(&mut self, blocks: &[Block], index: usize, depth: u8) -> (Node, usize) {
        let kind = blocks[index].kind;
        let mut items: Vec<Vec<Node>> = Vec::new();
        let mut at = index;
        while let Some(block) = blocks.get(at) {
            if !block.kind.is_list() || block.indent < depth {
                break;
            }
            if block.indent > depth {
                let (nested, end) = self.list(blocks, at, depth + 1);
                match items.last_mut() {
                    Some(item) => item.push(nested),
                    None => items.push(vec![nested]),
                }
                at = end;
                continue;
            }
            if block.kind != kind {
                break;
            }
            items.push(self.inline(block));
            at += 1;
        }
        let list = Node::List(List {
            ordered: kind == Kind::Numbered,
            start: 1,
            items,
        });
        (list, at)
    }

    /// A block's runs as inline nodes: runs of one link in one link, runs of one span style in one span.
    fn inline(&mut self, block: &Block) -> Vec<Node> {
        let runs = spaced(&block.runs);
        if runs.iter().all(|run| run.text.is_empty()) {
            // an empty line, as the web client writes one
            return vec![Node::Text {
                text: "\u{a0}".to_owned(),
                marks: Marks::NONE,
            }];
        }
        let mut nodes = Vec::new();
        for group in runs.chunk_by(|one, other| one.attrs.link == other.attrs.link) {
            let children = self.spans(group);
            match group[0].attrs.link.clone() {
                Some(href) => nodes.push(Node::Link { href, children }),
                None => nodes.extend(children),
            }
        }
        nodes
    }

    fn spans(&mut self, runs: &[Run]) -> Vec<Node> {
        let mut nodes = Vec::new();
        for group in runs.chunk_by(|one, other| one.attrs.span == other.attrs.span) {
            let mut children = Vec::new();
            for run in group {
                self.leaf(run, &mut children);
            }
            let style = group[0].attrs.span;
            if style.is_empty() {
                nodes.extend(children);
            } else {
                nodes.push(Node::Span { style, children });
            }
        }
        nodes
    }

    fn leaf(&mut self, run: &Run, into: &mut Vec<Node>) {
        if let Some(mri) = &run.attrs.mention {
            let item_id = self.mentioned.len().to_string();
            self.mentioned.push((item_id.clone(), Mri::new(mri.clone())));
            into.push(Node::Mention {
                item_id,
                text: run.text.clone(),
            });
            return;
        }
        for (at, line) in run.text.split('\n').enumerate() {
            if at > 0 {
                into.push(Node::Break);
            }
            if !line.is_empty() {
                into.push(Node::Text {
                    text: line.to_owned(),
                    marks: run.attrs.marks,
                });
            }
        }
    }
}

/// `runs` with every space a browser would drop a no-break space: one after a space or a line's start, and one at a
/// line's end.
fn spaced(runs: &[Run]) -> Vec<Run> {
    let mut out: Vec<Run> = runs.to_vec();
    let mut after_space = true;
    for run in &mut out {
        if run.attrs.mention.is_some() {
            after_space = false;
            continue;
        }
        let mut text = String::with_capacity(run.text.len());
        for character in run.text.chars() {
            match character {
                ' ' if after_space => text.push('\u{a0}'),
                ' ' => {
                    text.push(' ');
                    after_space = true;
                    continue;
                }
                '\n' => {
                    text.push('\n');
                    after_space = true;
                    continue;
                }
                other => text.push(other),
            }
            after_space = character == ' ';
        }
        run.text = text;
    }
    // a space ending a line
    let mut line_end = true;
    for run in out.iter_mut().rev() {
        if run.attrs.mention.is_some() {
            line_end = false;
            continue;
        }
        let mut chars: Vec<char> = run.text.chars().collect();
        for character in chars.iter_mut().rev() {
            match *character {
                ' ' if line_end => {
                    *character = '\u{a0}';
                }
                '\n' => line_end = true,
                _ => line_end = false,
            }
        }
        run.text = chars.into_iter().collect();
    }
    out
}

/// The document `nodes` make for `load`, its mentions' MRIs by their `itemid` in `mentioned` (a mention naming nobody
/// there is its text). An emoticon is its emoji; a quote that names a message (a reply's) or a forward's is its text.
///
/// # Errors
/// [`CantLoad::Image`] for a picture, unless `load` is a paste (which takes its `alt`); [`CantLoad::Formatting`],
/// for an edit, what the editor would send back changed ([`Load::Edit`]).
pub fn from_nodes<S: BuildHasher>(
    nodes: &[Node],
    mentioned: &HashMap<String, String, S>,
    load: Load,
) -> Result<Doc, CantLoad> {
    let mut loader = Loader {
        mentioned,
        blocks: Vec::new(),
        current: None,
        load,
    };
    loader.walk(nodes, &Context::default())?;
    loader.flush();
    let mut blocks = loader.blocks;
    // the empty lines a message ends or starts with are none in the box (one block stays)
    while blocks.len() > 1 && blocks.last().is_some_and(Block::is_blank) {
        blocks.pop();
    }
    let leading = blocks
        .iter()
        .take(blocks.len().saturating_sub(1))
        .take_while(|block| block.is_blank())
        .count();
    blocks.drain(..leading);
    Ok(Doc::new(blocks))
}

/// Where the walk is: the kind and indent of the blocks it makes, the attributes of the text it reads.
#[derive(Clone, Default)]
struct Context {
    kind: Kind,
    indent: u8,
    /// How many lists the walk is in.
    lists: u8,
    attrs: Attrs,
}

struct Loader<'m, S> {
    mentioned: &'m HashMap<String, String, S>,
    blocks: Vec<Block>,
    current: Option<Block>,
    load: Load,
}

impl<S: BuildHasher> Loader<'_, S> {
    fn flush(&mut self) {
        if let Some(mut block) = self.current.take() {
            block.normalize();
            self.blocks.push(block);
        }
    }

    fn push(&mut self, text: &str, attrs: Attrs, context: &Context) {
        let block = self
            .current
            .get_or_insert_with(|| Block::new(context.kind, context.indent, Vec::new()));
        block.runs.push(Run::new(text.replace('\u{a0}', " "), attrs));
    }

    /// Walks `nodes` in `context`; recursive over a model `teams-proto` caps at its `MAX_DEPTH`.
    fn walk(&mut self, nodes: &[Node], context: &Context) -> Result<(), CantLoad> {
        for node in nodes {
            self.node(node, context)?;
        }
        Ok(())
    }

    fn block(&mut self, nodes: &[Node], context: &Context) -> Result<(), CantLoad> {
        self.flush();
        self.walk(nodes, context)?;
        self.flush();
        Ok(())
    }

    /// An image with `alt`: an emoticon (`emoji`) is edited as its emoji; any other picture (a sticker too) is no text,
    /// so a message with one isn't edited, and a pasted one is its `alt`.
    fn image(&mut self, emoji: bool, alt: Option<&str>, context: &Context) -> Result<(), CantLoad> {
        if emoji {
            // Teams' own editor keeps a message with emoticons editable; this one has no emoticon run, so an
            // emoticon is edited (and goes back) as its emoji
            self.push(alt.unwrap_or_default(), context.attrs.clone(), context);
        } else if self.load != Load::Paste {
            return Err(CantLoad::Image);
        } else if let Some(alt) = alt {
            self.push(alt, context.attrs.clone(), context);
        }
        Ok(())
    }

    fn node(&mut self, node: &Node, context: &Context) -> Result<(), CantLoad> {
        match node {
            Node::Text { text, marks } => {
                let attrs = Attrs {
                    marks: context.attrs.marks.with(*marks),
                    ..context.attrs.clone()
                };
                self.push(text, attrs, context);
            }
            Node::Break => self.push("\n", context.attrs.clone(), context),
            Node::Mention { item_id, text } => self.mention(item_id, text, context),
            Node::Link { href, children } => {
                let mut inner = context.clone();
                inner.attrs.link = Some(href.clone());
                self.walk(children, &inner)?;
            }
            Node::Span { style, children } => {
                let mut inner = context.clone();
                inner.attrs.span = inner.attrs.span.under(*style);
                self.walk(children, &inner)?;
            }
            Node::Image(image) => self.image(image.is_emoji(), image.alt.as_deref(), context)?,
            // a sticker is a picture: a message of one isn't edited, and a pasted one is its label
            Node::Sticker(sticker) => self.image(false, sticker.label.as_deref(), context)?,
            Node::Paragraph(children) => self.block(children, context)?,
            Node::Heading { level, children } => {
                let inner = Context {
                    kind: Kind::Heading((*level).clamp(1, 6)),
                    indent: 0,
                    ..context.clone()
                };
                self.block(children, &inner)?;
            }
            Node::Indented { depth, children } => {
                let inner = Context {
                    indent: if context.kind == Kind::Paragraph {
                        *depth
                    } else {
                        context.indent
                    },
                    ..context.clone()
                };
                self.block(children, &inner)?;
            }
            Node::Quote(quote) => {
                let inner = Context {
                    kind: Kind::Quote,
                    indent: 0,
                    ..context.clone()
                };
                self.block(&quote.body, &inner)?;
            }
            Node::List(list) => self.list(list, context)?,
            Node::Code(code) => {
                let plain = code.language.as_deref().is_none_or(|language| language == "plaintext");
                if self.load == Load::Edit && !plain {
                    return Err(CantLoad::Formatting);
                }
                self.flush();
                self.blocks.push(Block::new(
                    Kind::Code,
                    0,
                    vec![Run::new(code.text().trim_end_matches('\n'), Attrs::default())],
                ));
            }
            Node::Rule => {
                self.flush();
                self.blocks.push(Block::new(Kind::Rule, 0, Vec::new()));
            }
            Node::Table(table) => self.table(table, context)?,
        }
        Ok(())
    }

    /// A table: the box has none, so an edit is refused (it would send the table back as lines); elsewhere each row is
    /// a line, its cells apart by a tab.
    fn table(&mut self, table: &Table, context: &Context) -> Result<(), CantLoad> {
        if self.load == Load::Edit {
            return Err(CantLoad::Formatting);
        }
        for row in &table.rows {
            self.flush();
            for (column, cell) in row.iter().enumerate() {
                if column > 0 {
                    self.push("\t", context.attrs.clone(), context);
                }
                self.walk(&cell.nodes, context)?;
            }
            self.flush();
        }
        Ok(())
    }

    fn mention(&mut self, item_id: &str, text: &str, context: &Context) {
        let tokenable = !text.trim().is_empty() && !text.chars().any(char::is_control);
        let attrs = match self.mentioned.get(item_id).filter(|_| tokenable) {
            Some(mri) => Attrs {
                mention: Some(mri.clone()),
                link: None,
                ..context.attrs.clone()
            },
            None => context.attrs.clone(),
        };
        self.push(text, attrs, context);
    }

    fn list(&mut self, list: &List, context: &Context) -> Result<(), CantLoad> {
        if self.load == Load::Edit && list.ordered && list.start != 1 {
            return Err(CantLoad::Formatting);
        }
        self.flush();
        let inner = Context {
            kind: if list.ordered { Kind::Numbered } else { Kind::Bullet },
            indent: context.lists.min(MAX_INDENT),
            lists: context.lists.saturating_add(1),
            attrs: context.attrs.clone(),
        };
        for item in &list.items {
            self.block(item, &inner)?;
        }
        Ok(())
    }
}
