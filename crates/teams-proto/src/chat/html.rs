//! A message's HTML content read into a small typed model ([`RichText`]): text runs with their marks, line breaks,
//! paragraphs, mentions, quotes, links and images. Parsed by html5ever (through `scraper`), as a browser would, and
//! then walked without recursion.
//!
//! - **Kept**: text, `br`; `p`, `div`, `li` and `h1`–`h6` as paragraphs; `ol` and `ul` as lists, each `li` (any
//!   paragraph) directly in one an item, numbered from `ol`'s `start`; `pre` as a block of code ([`Code`]: its text,
//!   its language's id and its tokens' kinds, every other element in it giving its text, a `br` a line feed), the code
//!   block editor's own paragraph before it dropped; `b`/`strong`, `i`/`em`, `u`/`ins`,
//!   `s`/`strike`/`del`, `code` as marks; a mention (`<span itemtype="http://schema.skype.com/Mention" itemid="<n>">`,
//!   as captured: inside a `readonly` element as sent, which is unknown markup and so gives its text, bare as read
//!   back); a quote (`<blockquote>`; a reply's, `itemtype="http://schema.skype.com/Reply"`,
//!   with the quoted message's id, its sender's MRI and name and its time, as `docs/protocol/chat.md` shows it; a
//!   forwarded message's, `itemtype="http://schema.skype.com/Forward"`, marked as one, its content the forwarded
//!   message's own: where it came from is in `properties.originalMessageContext`); a link
//!   with an `http`, `https` or `mailto` `href`; an image with an `https` `src`, its `alt`, `itemtype` and `itemid`
//!   (an emoticon's id, an AMS picture's object) and the size it is shown at (`width`/`height`, else its `style`'s).
//! - **Whitespace as a browser sets it**: outside a `pre`, a run of spaces, tabs and line feeds in the markup is one
//!   space, and a space at the start or end of a line (beside a `br` or a paragraph's edge) is none. A line break is a
//!   `br` or a paragraph, never a line feed in the markup: the web client sends `<br>` followed by a line feed, and a
//!   paragraph's text with line feeds in it reads as one line. A `pre` (a block of code) keeps its text as it came.
//! - **Unknown markup is kept as text**: an element this model doesn't name, a link of another scheme or an image of
//!   another source gives its text (an image its `alt`), never its tag or attributes.
//! - **Nothing active survives**: `script`, `style`, `template`, frames, `object`/`embed` and the document's head are
//!   dropped with everything in them, and the model holds no attribute but the few above, so no event handler, style
//!   or `javascript:` URL can reach it.
//! - **Bounded**: containers nested deeper than [`MAX_DEPTH`] give their content to the one above, so the model's depth
//!   (and every walk over it) stays bounded whatever the input; content over [`MAX_CONTENT`] is read up to it.

use ego_tree::iter::Edge;
use scraper::node::Element;
use scraper::{Html, Node as HtmlNode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::chat::code::{CODE_BLOCK_ITEMTYPE, Code, Token, language_of};
use crate::chat::model::Field;

/// The deepest the model nests containers (paragraphs, links, quotes, mentions).
pub const MAX_DEPTH: usize = 32;

/// The longest content read, in bytes; a longer one is read up to it ([`RichText::truncated`]). The parse's cost grows
/// with the square of the markup's depth, and a chat message is far shorter than this.
pub const MAX_CONTENT: usize = 64 * 1024;

/// The `itemtype` of a mention's span.
pub const MENTION_ITEMTYPE: &str = "http://schema.skype.com/Mention";

/// The `itemtype` of a reply's quote.
pub const REPLY_ITEMTYPE: &str = "http://schema.skype.com/Reply";

/// The `itemtype` of a forwarded message's quote: what it holds is the forwarded message's content.
pub const FORWARD_ITEMTYPE: &str = "http://schema.skype.com/Forward";

/// The `itemtype` of an emoticon's `<img>` (its `itemid` the emoticon's id, its `alt` the emoji).
pub const EMOJI_ITEMTYPE: &str = "http://schema.skype.com/Emoji";

/// The `itemtype` of a picture pasted into a message (an `<img>` whose `src` is a view of an AMS object).
pub const AMS_IMAGE_ITEMTYPE: &str = "http://schema.skype.com/AMSImage";

/// The largest number a list's `start` may give; a larger one, or one that isn't digits, is 1.
pub const MAX_LIST_START: u32 = 1_000_000;

/// The largest side an image's given size may have, in CSS pixels; a larger one is no size.
const MAX_GIVEN_SIDE: u32 = 100_000;

/// Elements dropped with everything in them.
const DROPPED: &[&str] = &[
    "script", "style", "template", "iframe", "frame", "frameset", "object", "embed", "noembed", "noframes", "head",
    "title", "meta", "link", "base",
];

/// Elements read as a paragraph.
const PARAGRAPHS: &[&str] = &["p", "div", "li"];

/// How far one step of indentation moves a paragraph, in CSS pixels: Teams' editor indents by 40 px
/// (`<p style="margin-left:40px">`).
pub const INDENT_STEP: u32 = 40;

/// The deepest a paragraph is indented, in steps.
pub const MAX_INDENT: u8 = 8;

/// A message's content as rich text. It is also what a message is written from
/// ([`MessageText`](crate::chat::MessageText)), and serializes for the outbox's stored commands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichText {
    pub nodes: Vec<Node>,
    /// The content was longer than [`MAX_CONTENT`]: only its first part was read.
    pub truncated: bool,
}

impl RichText {
    /// Plain text as one run.
    #[must_use]
    pub fn plain(text: &str) -> Self {
        let nodes = if text.is_empty() {
            Vec::new()
        } else {
            vec![Node::Text {
                text: text.to_owned(),
                marks: Marks::NONE,
            }]
        };
        Self {
            nodes,
            truncated: false,
        }
    }

    /// The text a reader sees: runs, mentions' names and images' `alt`, a line break per `br` and between paragraphs.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        plain_text(&self.nodes, &mut text);
        text
    }

    /// The text for a message preview: quotes left out, except a forwarded message's own content.
    #[must_use]
    pub fn preview_text(&self) -> String {
        let mut text = String::new();
        text_of(&self.nodes, &mut text, false);
        text
    }
}

/// One piece of rich text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    Text {
        text: String,
        marks: Marks,
    },
    Break,
    Paragraph(Vec<Node>),
    /// A mention: its `itemid` (the index of its entry in `properties.mentions`) and the name shown.
    Mention {
        item_id: String,
        text: String,
    },
    Link {
        href: String,
        children: Vec<Node>,
    },
    Image(Image),
    Quote(Quote),
    List(List),
    Code(Code),
    /// A `span` whose style gives its runs a font colour, a highlight or a size, as Teams' editor writes one
    /// (`<span style="background-color:#EBD3E1; color:#1E53A3; font-size:x-large">`).
    Span {
        style: SpanStyle,
        children: Vec<Node>,
    },
    /// A heading (`h1`–`h6`), its level 1 to 6.
    Heading {
        level: u8,
        children: Vec<Node>,
    },
    /// A paragraph indented by `depth` steps of [`INDENT_STEP`] (`<p style="margin-left:40px">`), 1 to
    /// [`MAX_INDENT`].
    Indented {
        depth: u8,
        children: Vec<Node>,
    },
    /// A horizontal rule (`hr`).
    Rule,
}

impl Node {
    /// Whether it is a block: a paragraph, a heading, an indented paragraph, a quote, a list, a block of code or a
    /// rule. A line starts at its edges.
    #[must_use]
    pub const fn is_block(&self) -> bool {
        matches!(
            self,
            Self::Paragraph(_)
                | Self::Heading { .. }
                | Self::Indented { .. }
                | Self::Quote(_)
                | Self::List(_)
                | Self::Code(_)
                | Self::Rule
        )
    }
}

/// A colour as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb(pub u32);

impl Rgb {
    /// `#RRGGBB`, upper case, as Teams' editor writes its colours.
    #[must_use]
    pub fn hex(self) -> String {
        format!("#{:06X}", self.0 & 0x00FF_FFFF)
    }

    /// A CSS colour: `#RRGGBB`, `#RGB`, `rgb(r, g, b)` or `rgba(r, g, b, a)` (a tint taken as its colour); `None` for
    /// one fully transparent (an alpha of 0, `transparent`: a browser's copy of text with no highlight) and anything
    /// else (`inherit`, a name, a variable).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if let Some(hex) = text.strip_prefix('#') {
            if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            return match hex.len() {
                6 => u32::from_str_radix(hex, 16).ok().map(Self),
                3 => {
                    let wide: String = hex.chars().flat_map(|c| [c, c]).collect();
                    u32::from_str_radix(&wide, 16).ok().map(Self)
                }
                _ => None,
            };
        }
        let inner = text
            .strip_prefix("rgb(")
            .or_else(|| text.strip_prefix("rgba("))?
            .strip_suffix(')')?;
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if !(3..=4).contains(&parts.len()) {
            return None;
        }
        if let Some(alpha) = parts.get(3) {
            let alpha: f64 = alpha.strip_suffix('%').unwrap_or(alpha).trim_end().parse().ok()?;
            if alpha.is_nan() || alpha <= 0.0 {
                return None;
            }
        }
        let mut value = 0u32;
        for part in &parts[..3] {
            let channel: u8 = part.parse().ok()?;
            value = value << 8 | u32::from(channel);
        }
        Some(Self(value))
    }
}

/// The size Teams' editor gives a run: its Small (`font-size:xx-small`) and Large (`font-size:x-large`), as its
/// font size menu writes them; Medium is no size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FontSize {
    Small,
    Large,
}

impl FontSize {
    /// The CSS value Teams' editor writes for it.
    #[must_use]
    pub const fn css(self) -> &'static str {
        match self {
            Self::Small => "xx-small",
            Self::Large => "x-large",
        }
    }

    /// The size a CSS `font-size` gives: a keyword below `medium` small, above it large; a length under 12 px small
    /// and over 18 px large; `None` for `medium`, `inherit`, `0`, a length between and anything unread.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        match text.as_str() {
            "xx-small" | "x-small" | "small" | "smaller" => return Some(Self::Small),
            "large" | "x-large" | "xx-large" | "xxx-large" | "larger" => return Some(Self::Large),
            _ => {}
        }
        // in hundredths of a pixel: a point is 4/3 of one
        let (number, per_unit) = if let Some(number) = text.strip_suffix("px") {
            (number, 300)
        } else {
            (text.strip_suffix("pt")?, 400)
        };
        let scaled = hundredths(number)?.checked_mul(per_unit)? / 300;
        // 0, and a size between the two, is no size of its own
        match scaled {
            1..1200 => Some(Self::Small),
            1801.. => Some(Self::Large),
            _ => None,
        }
    }
}

/// A non-negative decimal number in hundredths (`14.6667` is 1466); `None` for anything else.
fn hundredths(number: &str) -> Option<u32> {
    let number = number.trim();
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || whole.len() > 6 || !digits(whole) || !digits(fraction) {
        return None;
    }
    let cents = fraction
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(2)
        .fold(0, |sum, digit| sum * 10 + u32::from(digit - b'0'));
    whole.parse::<u32>().ok()?.checked_mul(100)?.checked_add(cents)
}

/// What a `span`'s style gives its runs (see [`Node::Span`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpanStyle {
    /// The font colour (`color`).
    pub color: Option<Rgb>,
    /// The highlight (`background-color`).
    pub highlight: Option<Rgb>,
    pub size: Option<FontSize>,
}

impl SpanStyle {
    /// Whether it gives nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.color.is_none() && self.highlight.is_none() && self.size.is_none()
    }

    /// The style a `style` attribute gives: its `color`, `background-color` (or a `background` that is just a
    /// colour) and `font-size`.
    #[must_use]
    pub fn of_css(style: &str) -> Self {
        let mut found = Self::default();
        for (name, value) in declarations(style) {
            match name.as_str() {
                "color" => found.color = Rgb::parse(value),
                "background-color" | "background" => found.highlight = Rgb::parse(value),
                "font-size" => found.size = FontSize::parse(value),
                _ => {}
            }
        }
        found
    }

    /// It, with what `inner` sets taking its place: a span's style inside this one's.
    #[must_use]
    pub fn under(self, inner: Self) -> Self {
        Self {
            color: inner.color.or(self.color),
            highlight: inner.highlight.or(self.highlight),
            size: inner.size.or(self.size),
        }
    }
}

/// A `style` attribute's declarations: each property's name (lower case) and its value.
fn declarations(style: &str) -> impl Iterator<Item = (String, &str)> {
    style.split(';').filter_map(|declaration| {
        let (name, value) = declaration.split_once(':')?;
        Some((name.trim().to_ascii_lowercase(), value.trim()))
    })
}

/// How many steps a paragraph's `style` indents it: its `margin-left` in pixels, [`INDENT_STEP`] a step (rounded),
/// at most [`MAX_INDENT`]; `0` without one.
fn indent_of(style: &str) -> u8 {
    let step = INDENT_STEP * 100;
    declarations(style)
        .filter(|(name, _)| name == "margin-left")
        .filter_map(|(_, value)| hundredths(value.strip_suffix("px")?))
        .filter(|cents| *cents > 0)
        .map(|cents| {
            let steps = (cents + step / 2) / step;
            u8::try_from(steps.max(1)).unwrap_or(MAX_INDENT).min(MAX_INDENT)
        })
        .last()
        .unwrap_or(0)
}

/// A list (`ol` or `ul`): its items, each what one `li` holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct List {
    /// An `ol`: its items are numbered, from [`Self::start`]; else bulleted.
    pub ordered: bool,
    /// The first item's number: an `ol`'s `start` (digits, at most [`MAX_LIST_START`]), else 1.
    pub start: u32,
    pub items: Vec<Vec<Node>>,
}

/// An image the content shows. Its source is kept, never fetched here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub src: String,
    pub alt: Option<String>,
    /// Its `itemtype`, if it has one: [`AMS_IMAGE_ITEMTYPE`] for a picture pasted into a message, [`EMOJI_ITEMTYPE`]
    /// for an emoticon.
    pub item_type: Option<String>,
    /// Its `itemid`, if it has one: an emoticon's id in Teams' catalog (`speechless`, `1f440_eyes`), an AMS image's
    /// object id.
    #[serde(default)]
    pub item_id: Option<String>,
    /// The size it is shown at, in CSS pixels rounded, as its `width` and `height` attributes give it (`884`, or
    /// `130.64889918887602` read as `131`), else its `style`'s `width:<n>px; height:<n>px`; `None` without both.
    #[serde(default)]
    pub size: Option<(u32, u32)>,
}

impl Image {
    /// Whether it is an emoticon (`itemtype` [`EMOJI_ITEMTYPE`]).
    #[must_use]
    pub fn is_emoji(&self) -> bool {
        self.item_type.as_deref() == Some(EMOJI_ITEMTYPE)
    }
}

/// A quote. A reply's names the message it quotes; a forwarded message's ([`Self::forward`]) and a plain
/// `blockquote`'s name nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// The quoted message's id (the quote's `itemid`).
    pub message_id: Option<String>,
    /// The quoted message's sender (`itemprop="mri"`'s `itemid`).
    pub sender: Option<String>,
    /// The name shown for the sender.
    pub sender_name: Option<String>,
    /// The quoted message's time (`itemprop="time"`'s `itemid`).
    pub time: Option<String>,
    /// What the quote shows: the reply's `itemprop="preview"` paragraph, a forwarded message's content, or a plain
    /// quote's content.
    pub body: Vec<Node>,
    /// A forwarded message's (`itemtype` [`FORWARD_ITEMTYPE`]): its body is the forwarded message's own content, and
    /// where it came from is the message's `properties.originalMessageContext` ([`crate::chat::forwarded`]).
    #[serde(default)]
    pub forward: bool,
}

/// The marks on a text run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Marks(u8);

impl Marks {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const STRIKE: Self = Self(1 << 3);
    pub const CODE: Self = Self(1 << 4);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// It without `other`'s marks.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// A thread's event (`ThreadActivity/AddMember`), its XML content read: `<addmember>` with `eventtime`, `initiator`,
/// `rosterVersion` and a `target` per member.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadActivity {
    /// The event's root element, lowercased (`addmember`).
    pub kind: String,
    pub initiator: Option<String>,
    pub targets: Vec<String>,
    /// `rosterVersion`: digits as captured, any other text kept as [`Field::Unknown`] (a JSON string).
    pub roster_version: Field<u64>,
    /// `eventtime` (ms): as `roster_version`.
    pub event_time: Field<u64>,
    /// `sharehistorytime` (ms; `0` for all of it): how far back the chat's history was shared with those added; as
    /// `roster_version`, [`Field::Absent`] when the event names none.
    pub share_history_time: Field<u64>,
}

/// Parses a message's HTML content (see the module docs).
#[must_use]
pub fn parse(content: &str) -> RichText {
    let (content, truncated) = bounded(content);
    let html = Html::parse_fragment(content);
    let mut builder = Builder::new();
    for edge in html.tree.root().traverse() {
        match edge {
            Edge::Open(node) => builder.open(node.value()),
            Edge::Close(node) => builder.close(node.value()),
        }
    }
    let mut nodes = builder.root;
    drop_block_gaps(&mut nodes);
    trim_line_edges(&mut nodes);
    RichText { nodes, truncated }
}

/// Drops the markup whitespace beside a block (a paragraph, a quote or a list): text of ASCII whitespace only, right before
/// or after one, which HTML doesn't render. The service joins a read-back message's paragraphs with CRLF (the
/// `chat-shapes` captures of 2026-10-21, `docs/protocol/chat.md`), which would otherwise read as blank lines. A
/// no-break space is text, and whitespace between inline nodes stays.
fn drop_block_gaps(nodes: &mut Vec<Node>) {
    let is_block = |node: Option<&Node>| node.is_some_and(Node::is_block);
    let is_gap = |node: &Node| matches!(node, Node::Text { text, .. } if text.chars().all(|c| c.is_ascii_whitespace()));
    let keep: Vec<bool> = (0..nodes.len())
        .map(|index| {
            let before = index.checked_sub(1).and_then(|before| nodes.get(before));
            !(is_gap(&nodes[index]) && (is_block(before) || is_block(nodes.get(index + 1))))
        })
        .collect();
    let mut keep = keep.into_iter();
    nodes.retain(|_| keep.next().unwrap_or(true));
}

/// Drops the spaces at the start and end of each line of `nodes` (a container whose lines they are: the root, a
/// paragraph, a quote): a text run's leading spaces at the start of the container, after a `br` or a block, its
/// trailing ones at the end, before a `br` or a block. A browser draws none of them. Runs left empty go.
fn trim_line_edges(nodes: &mut Vec<Node>) {
    let edge = |node: Option<&Node>| node.is_none_or(|node| matches!(node, Node::Break) || node.is_block());
    for index in 0..nodes.len() {
        let starts = edge(index.checked_sub(1).and_then(|before| nodes.get(before)));
        let ends = edge(nodes.get(index + 1));
        if let Some(Node::Text { text, .. }) = nodes.get_mut(index) {
            if starts {
                let kept = text.trim_start_matches(' ').len();
                text.drain(..text.len() - kept);
            }
            if ends {
                text.truncate(text.trim_end_matches(' ').len());
            }
        }
    }
    nodes.retain(|node| !matches!(node, Node::Text { text, .. } if text.is_empty()));
}

/// `text` with every run of HTML whitespace (space, tab, line feed, form feed, carriage return) made one space, as a
/// browser sets text outside a `pre`; `after_space`: the text before it ends in a space, so a leading one goes too.
fn collapse_whitespace(text: &str, after_space: bool) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut space = after_space;
    for character in text.chars() {
        if matches!(character, ' ' | '\t' | '\n' | '\x0c' | '\r') {
            if !space {
                collapsed.push(' ');
            }
            space = true;
        } else {
            collapsed.push(character);
            space = false;
        }
    }
    collapsed
}

/// `content` up to [`MAX_CONTENT`] bytes (cut on a character boundary), and whether it was cut.
pub(crate) fn bounded(content: &str) -> (&str, bool) {
    if content.len() <= MAX_CONTENT {
        return (content, false);
    }
    let mut end = MAX_CONTENT;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (&content[..end], true)
}

/// Reads a thread event's XML content (up to [`MAX_CONTENT`] bytes, as [`parse`]); `None` when it has no element.
#[must_use]
pub fn thread_activity(content: &str) -> Option<ThreadActivity> {
    let html = Html::parse_fragment(bounded(content).0);
    let mut elements = html.tree.root().descendants().filter(|node| {
        node.value()
            .as_element()
            .is_some_and(|element| element.name() != "html")
    });
    let root = elements.next()?;
    let mut activity = ThreadActivity {
        kind: root.value().as_element()?.name().to_owned(),
        ..ThreadActivity::default()
    };
    for node in root.descendants() {
        let Some(element) = node.value().as_element() else {
            continue;
        };
        let text = || {
            node.descendants()
                .filter_map(|inner| inner.value().as_text().map(|text| &**text))
                .collect::<String>()
                .trim()
                .to_owned()
        };
        match element.name() {
            "initiator" => activity.initiator = Some(text()),
            "target" => activity.targets.push(text()),
            "rosterversion" => activity.roster_version = digits(text()),
            "eventtime" => activity.event_time = digits(text()),
            "sharehistorytime" => activity.share_history_time = digits(text()),
            _ => {}
        }
    }
    Some(activity)
}

/// An element's text as a number: digits only (as captured), anything else kept as it came.
fn digits(text: String) -> Field<u64> {
    match text.parse() {
        Ok(number) if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) => Field::Known(number),
        _ => Field::Unknown(Value::String(text)),
    }
}

/// What an open element became.
enum Frame {
    /// A container: its content is collected on the builder's container stack and made a node when it closes.
    Container,
    /// An element whose content goes to the container above (unknown markup, a mark, a leaf, a container too deep);
    /// the marks it found are restored when it closes.
    Pass { restore: Marks },
    /// An element inside a block of code: its text goes to the code, as `token` while it is open (a `span` of a
    /// highlight class), the token before it restored when it closes.
    InCode { restore: Token },
}

enum Container {
    Paragraph,
    Link(String),
    Mention(String),
    /// A quote, and whether it is a reply's.
    Quote(Quote, bool),
    /// A reply quote's sender, with the MRI its `itemid` names.
    Sender(Option<String>),
    /// A list, its kind and first number: its content is made its items when it closes ([`list_items`]).
    List {
        ordered: bool,
        start: u32,
    },
    /// A block of code (`pre`): its text is collected in it, not as children.
    Code(Code),
    /// A `span` whose style gives something ([`SpanStyle`]).
    Span(SpanStyle),
    /// A heading, its level.
    Heading(u8),
    /// A paragraph indented by that many steps.
    Indented(u8),
}

impl Container {
    /// Whether it is a block: a line starts at its edges.
    const fn is_block(&self) -> bool {
        matches!(
            self,
            Self::Paragraph
                | Self::Quote(..)
                | Self::List { .. }
                | Self::Code(_)
                | Self::Heading(_)
                | Self::Indented(_)
        )
    }
}

/// Builds the model from the parse's open and close edges. No step looks further than the open containers (at most
/// [`MAX_DEPTH`]), so deep input costs no more per element than flat input.
struct Builder {
    /// The content outside every container.
    root: Vec<Node>,
    /// One frame per open element.
    frames: Vec<Frame>,
    /// The open containers, innermost last, each with its content so far.
    containers: Vec<(Container, Vec<Node>)>,
    marks: Marks,
    /// While above 0, the edges are inside a dropped element (the count is how deep).
    skip: usize,
    /// The text set so far ends in a space, or a line starts here (a `br`, a block's edge): a whitespace run next is
    /// none, across any element's edge (a link, a mention, a mark), as a browser sets it.
    after_space: bool,
    /// The kind of token the text inside a block of code is.
    token: Token,
}

impl Builder {
    fn new() -> Self {
        Self {
            root: Vec::new(),
            frames: Vec::new(),
            containers: Vec::new(),
            marks: Marks::NONE,
            skip: 0,
            after_space: true,
            token: Token::Plain,
        }
    }

    fn open(&mut self, node: &HtmlNode) {
        match node {
            HtmlNode::Element(_) if self.skip > 0 => self.skip += 1,
            HtmlNode::Element(element) => self.open_element(element),
            HtmlNode::Text(text) if self.skip == 0 => {
                let token = self.token;
                if let Some(code) = self.code() {
                    code.push(text, token);
                    return;
                }
                let marks = self.marks;
                let text = collapse_whitespace(text, self.after_space);
                if !text.is_empty() {
                    self.after_space = text.ends_with(' ');
                }
                push_text(self.children(), &text, marks);
            }
            _ => {}
        }
    }

    fn close(&mut self, node: &HtmlNode) {
        if !matches!(node, HtmlNode::Element(_)) {
            return;
        }
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        match self.frames.pop() {
            Some(Frame::Pass { restore }) => self.marks = restore,
            Some(Frame::InCode { restore }) => self.token = restore,
            Some(Frame::Container) => {
                if let Some((kind, children)) = self.containers.pop() {
                    self.close_container(kind, children);
                }
            }
            None => {}
        }
    }

    fn open_element(&mut self, element: &Element) {
        let name = element.name();
        if DROPPED.contains(&name) || element.attr("itemtype") == Some(CODE_BLOCK_ITEMTYPE) {
            self.skip = 1;
            return;
        }
        if self.code().is_some() {
            return self.open_in_code(element);
        }
        if self.reply_quote().is_some() {
            match element.attr("itemprop") {
                Some("mri") => return self.open_container(Container::Sender(attr(element, "itemid"))),
                Some("time") => {
                    if let Some(quote) = self.reply_quote() {
                        quote.time = attr(element, "itemid");
                    }
                    self.skip = 1;
                    return;
                }
                _ => {}
            }
        }
        if let Some(mark) = mark(name) {
            let restore = self.marks;
            self.marks = restore.with(mark);
            self.frames.push(Frame::Pass { restore });
            return;
        }
        match container(element) {
            Some(kind) => self.open_container(kind),
            None => self.open_leaf(element),
        }
    }

    fn open_container(&mut self, kind: Container) {
        if self.containers.len() >= MAX_DEPTH {
            self.frames.push(Frame::Pass { restore: self.marks });
            return;
        }
        if kind.is_block() {
            self.after_space = true;
        }
        self.containers.push((kind, Vec::new()));
        self.frames.push(Frame::Container);
    }

    /// An element inside a block of code: a `br` is a line feed, an image its `alt`, a `span` of a highlight class
    /// the kind of the text in it; any other gives its text.
    fn open_in_code(&mut self, element: &Element) {
        let restore = self.token;
        match element.name() {
            "br" => {
                if let Some(code) = self.code() {
                    code.push("\n", restore);
                }
            }
            "img" => {
                if let (Some(alt), Some(code)) = (element.attr("alt"), self.code()) {
                    code.push(alt, restore);
                }
            }
            "span" => {
                if let Some(token) = element.attr("class").and_then(Token::of_class) {
                    self.token = token;
                }
            }
            _ => {}
        }
        self.frames.push(Frame::InCode { restore });
    }

    /// The block of code the walk is in: the innermost container, as no container opens inside one.
    fn code(&mut self) -> Option<&mut Code> {
        match self.containers.last_mut() {
            Some((Container::Code(code), _)) => Some(code),
            _ => None,
        }
    }

    /// An element that is no container: a line break, an image, or unknown markup, whose text stays.
    fn open_leaf(&mut self, element: &Element) {
        let marks = self.marks;
        match element.name() {
            "br" => {
                self.after_space = true;
                self.children().push(Node::Break);
            }
            "hr" => {
                self.after_space = true;
                self.children().push(Node::Rule);
            }
            "img" => {
                self.after_space = false;
                match image(element) {
                    Some(image) => self.children().push(Node::Image(image)),
                    None => {
                        if let Some(alt) = element.attr("alt") {
                            push_text(self.children(), alt, marks);
                        }
                    }
                }
            }
            _ => {}
        }
        self.frames.push(Frame::Pass { restore: marks });
    }

    fn close_container(&mut self, kind: Container, mut children: Vec<Node>) {
        drop_block_gaps(&mut children);
        if kind.is_block() {
            // the next text starts a line
            self.after_space = true;
            trim_line_edges(&mut children);
        }
        let node = match kind {
            Container::Paragraph => Node::Paragraph(children),
            Container::Span(style) => Node::Span { style, children },
            Container::Heading(level) => Node::Heading { level, children },
            Container::Indented(depth) => Node::Indented { depth, children },
            Container::Link(href) => Node::Link { href, children },
            Container::Mention(item_id) => {
                let mut text = String::new();
                plain_text(&children, &mut text);
                Node::Mention { item_id, text }
            }
            Container::Quote(mut quote, _) => {
                quote.body = children;
                Node::Quote(quote)
            }
            Container::List { ordered, start } => Node::List(List {
                ordered,
                start,
                items: list_items(children),
            }),
            Container::Code(mut code) => {
                code.finish();
                Node::Code(code)
            }
            Container::Sender(mri) => {
                let mut name = String::new();
                plain_text(&children, &mut name);
                if let Some(quote) = self.reply_quote() {
                    quote.sender = mri;
                    quote.sender_name = Some(name);
                }
                return;
            }
        };
        self.children().push(node);
    }

    /// The content of the innermost open container, or the root's.
    fn children(&mut self) -> &mut Vec<Node> {
        match self.containers.last_mut() {
            Some((_, children)) => children,
            None => &mut self.root,
        }
    }

    /// The innermost open reply quote.
    fn reply_quote(&mut self) -> Option<&mut Quote> {
        self.containers.iter_mut().rev().find_map(|(kind, _)| match kind {
            Container::Quote(quote, true) => Some(quote),
            _ => None,
        })
    }
}

fn mark(name: &str) -> Option<Marks> {
    match name {
        "b" | "strong" => Some(Marks::BOLD),
        "i" | "em" => Some(Marks::ITALIC),
        "u" | "ins" => Some(Marks::UNDERLINE),
        "s" | "strike" | "del" => Some(Marks::STRIKE),
        "code" => Some(Marks::CODE),
        _ => None,
    }
}

/// The container `element` opens, if it is one.
fn container(element: &Element) -> Option<Container> {
    let name = element.name();
    if PARAGRAPHS.contains(&name) {
        let depth = if name == "li" {
            0
        } else {
            element.attr("style").map_or(0, indent_of)
        };
        return Some(if depth > 0 {
            Container::Indented(depth)
        } else {
            Container::Paragraph
        });
    }
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(Container::Heading(name.as_bytes()[1] - b'0')),
        "span" if element.attr("itemtype").is_none() => element
            .attr("style")
            .map(SpanStyle::of_css)
            .filter(|style| !style.is_empty())
            .map(Container::Span),
        "pre" => Some(Container::Code(Code {
            language: element.attr("class").and_then(language_of),
            runs: Vec::new(),
        })),
        "a" => element
            .attr("href")
            .filter(|href| scheme(href).is_some_and(|scheme| matches!(scheme.as_str(), "http" | "https" | "mailto")))
            .map(|href| Container::Link(href.to_owned())),
        "span" if element.attr("itemtype") == Some(MENTION_ITEMTYPE) => {
            Some(Container::Mention(attr(element, "itemid").unwrap_or_default()))
        }
        "ol" | "ul" => Some(Container::List {
            ordered: name == "ol",
            start: element
                .attr("start")
                .filter(|_| name == "ol")
                .and_then(list_start)
                .unwrap_or(1),
        }),
        "blockquote" => {
            let reply = element.attr("itemtype") == Some(REPLY_ITEMTYPE);
            let quote = Quote {
                message_id: if reply { attr(element, "itemid") } else { None },
                forward: element.attr("itemtype") == Some(FORWARD_ITEMTYPE),
                ..Quote::default()
            };
            Some(Container::Quote(quote, reply))
        }
        _ => None,
    }
}

/// An `ol`'s `start`: digits only, 0 to [`MAX_LIST_START`].
fn list_start(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.is_empty() || text.len() > 7 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|start| *start <= MAX_LIST_START)
}

/// A list's content as its items: each paragraph in it (an `li`) one, and any other content between them (a browser
/// draws it without a marker) an item of its own, in order.
fn list_items(children: Vec<Node>) -> Vec<Vec<Node>> {
    let mut items = Vec::new();
    let mut stray = Vec::new();
    for child in children {
        match child {
            Node::Paragraph(item) => {
                if !stray.is_empty() {
                    items.push(std::mem::take(&mut stray));
                }
                items.push(item);
            }
            other => stray.push(other),
        }
    }
    if !stray.is_empty() {
        items.push(stray);
    }
    items
}

/// An image with an `https` source.
fn image(element: &Element) -> Option<Image> {
    let src = element.attr("src")?;
    (scheme(src)? == "https").then(|| Image {
        src: src.to_owned(),
        alt: attr(element, "alt"),
        item_type: attr(element, "itemtype"),
        item_id: attr(element, "itemid"),
        size: given_size(element),
    })
}

/// The size an image is shown at: its `width` and `height` attributes (CSS pixels, a fraction rounded), else its
/// `style`'s `width:<n>px` and `height:<n>px`; `None` unless both are there, positive and at most
/// [`MAX_GIVEN_SIDE`].
fn given_size(element: &Element) -> Option<(u32, u32)> {
    let attributes = element
        .attr("width")
        .and_then(css_pixels)
        .zip(element.attr("height").and_then(css_pixels));
    attributes.or_else(|| {
        let style = element.attr("style")?;
        let property = |name: &str| {
            style.split(';').find_map(|declaration| {
                let (key, value) = declaration.split_once(':')?;
                (key.trim().eq_ignore_ascii_case(name)).then(|| css_pixels(value.trim().strip_suffix("px")?))?
            })
        };
        property("width").zip(property("height"))
    })
}

/// A length in CSS pixels (`884`, `130.64889918887602`), rounded half up; `None` for anything else, `0` or over
/// [`MAX_GIVEN_SIDE`].
fn css_pixels(text: &str) -> Option<u32> {
    let text = text.trim();
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    if whole.is_empty() || whole.len() > 6 || !digits(whole) || !digits(fraction) {
        return None;
    }
    let up = fraction.bytes().next().is_some_and(|digit| digit >= b'5');
    let value = whole.parse::<u32>().ok()?.checked_add(u32::from(up))?;
    (1..=MAX_GIVEN_SIDE).contains(&value).then_some(value)
}

/// The scheme of an absolute URL.
fn scheme(text: &str) -> Option<String> {
    Url::parse(text.trim()).ok().map(|url| url.scheme().to_owned())
}

fn attr(element: &Element, name: &str) -> Option<String> {
    element.attr(name).map(str::to_owned)
}

/// Adds a text run, joined to the one before when their marks are the same.
fn push_text(children: &mut Vec<Node>, text: &str, marks: Marks) {
    if text.is_empty() {
        return;
    }
    if let Some(Node::Text {
        text: last,
        marks: before,
    }) = children.last_mut()
        && *before == marks
    {
        last.push_str(text);
        return;
    }
    children.push(Node::Text {
        text: text.to_owned(),
        marks,
    });
}

/// The text `nodes` show (see [`RichText::plain_text`]). Recursive, over a model at most [`MAX_DEPTH`] deep.
fn plain_text(nodes: &[Node], out: &mut String) {
    text_of(nodes, out, true);
}

fn text_of(nodes: &[Node], out: &mut String, include_quotes: bool) {
    for node in nodes {
        match node {
            Node::Text { text, .. } | Node::Mention { text, .. } => out.push_str(text),
            Node::Break => out.push('\n'),
            Node::Paragraph(children) | Node::Heading { children, .. } | Node::Indented { children, .. } => {
                paragraph_text(children, out, include_quotes);
            }
            Node::Link { children, .. } | Node::Span { children, .. } => text_of(children, out, include_quotes),
            Node::Rule => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Node::Image(image) => out.push_str(image.alt.as_deref().unwrap_or_default()),
            Node::Quote(quote) if include_quotes || quote.forward => text_of(&quote.body, out, include_quotes),
            Node::Quote(_) => {}
            Node::Code(code) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&code.text());
                out.push('\n');
            }
            Node::List(list) => {
                for item in &list.items {
                    item_text(item, out, include_quotes);
                }
            }
        }
    }
}

/// The text of a paragraph with its `children`: on a line of its own.
fn paragraph_text(children: &[Node], out: &mut String, include_quotes: bool) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    text_of(children, out, include_quotes);
    out.push('\n');
}

/// The text of a list's item: on a line of its own, with no blank line after a list nested at its end.
fn item_text(children: &[Node], out: &mut String, include_quotes: bool) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    text_of(children, out, include_quotes);
    if !out.ends_with('\n') {
        out.push('\n');
    }
}
