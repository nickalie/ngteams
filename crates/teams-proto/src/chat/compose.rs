//! A message as the native client writes it ([`MessageText`]): one typed rich-text value ([`RichText`], the model reads
//! give) and the MRI of each mention in it, from which both the HTML `content` and `properties.mentions` (and a reply's
//! `properties.qtdMsgs`) are built, so a mention can't be in the markup without being in the request's target.
//!
//! Raw HTML can't be passed: `content` is always rendered here, from the model, in the forms `docs/protocol/chat.md`
//! records. What a message renders to:
//!
//! - text runs escaped, their marks as `b`, `i`, `u`, `s`, `code` (outermost first); `br`; a paragraph as `p`;
//! - a link as `<a href>`, its `href` only `http`, `https` or `mailto`;
//! - a reply's quote as the captured reply (`reply.json`): the `blockquote` of itemtype Reply naming the quoted message,
//!   its sender and its time, with a `preview` paragraph of the quote's text, plus a `qtdMsgs` entry; a quote that
//!   names no message as a plain `blockquote`;
//! - a mention as the web client sent it (`mention.json`, captured in `ab-chat-full` on 2026-10-21): the span of
//!   itemtype Mention inside a `readonly` element, and its `properties.mentions` entry ([`mention_span`],
//!   [`mention_entry`], the only place it is built); a no-break space (U+00A0) as `&nbsp;`, as the web client writes
//!   the one after a mention; a mention of everyone in a group chat in the same form, its MRI the chat's id and its
//!   `mentionType` `everyone` (`mention-everyone.json`, captured in `abc-everyone` on 2026-10-29);
//! - a picture sent to the media service (`crate::ams::sent_picture`: itemtype `AMSImage`, its `imgo` view on an AMS
//!   host, `itemid` its object) as the web client writes a pasted picture, its object in the post's `amsreferences`
//!   ([`MessageText::pictures`]); any other image is refused.
//!
//! Every mentioned MRI is exposed by [`MessageText::participants`]; a quote adds nobody
//! (the quoted message is the chat's own), nor does a mention of everyone in a group chat, named by the chat's id
//! ([`MessageText::everyone`]: it reaches the chat's members, and a write sends it only to that chat).

use std::collections::{BTreeMap, BTreeSet};

use crate::{Mri, is_group_chat_id};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::chat::MentionKind;
use crate::chat::code::{Code, language_of};
use crate::chat::html::{
    INDENT_STEP, List, MAX_CONTENT, MAX_DEPTH, MAX_INDENT, MENTION_ITEMTYPE, Marks, Node, Quote, REPLY_ITEMTYPE,
    RichText, SpanStyle,
};

/// Why a [`MessageText`] can't be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    #[error("a file is stored outside this account's cloud")]
    FileCloud,
    /// A mention in the text whose `item_id` names no mentioned MRI.
    #[error("the mention {0:?} names nobody")]
    UnknownMention(String),
    /// A mentioned MRI whose `item_id` no mention in the text has.
    #[error("the mention {0:?} isn't in the text")]
    UnusedMention(String),
    /// An `item_id` given twice, or held by two mentions in the text.
    #[error("the mention {0:?} is there twice")]
    RepeatedMention(String),
    /// A mention of anyone but a work account (`8:orgid:<object id>`) or everyone in a group chat (the chat's id): a
    /// bot's mention needs its app id, which isn't modelled.
    #[error("a mention of {0}, which isn't a work account or a group chat")]
    NotAPerson(Mri),
    /// An image that isn't a picture sent to the media service for this message ([`crate::ams::sent_picture`]).
    #[error("an image can't be sent in a message's text")]
    Image,
    /// A link whose `href` isn't `http`, `https` or `mailto`.
    #[error("a link that isn't http, https or mailto")]
    LinkScheme,
    /// A quote that names a message but not all of its sender, the sender's name and a time in milliseconds.
    #[error("a reply's quote without its sender, the sender's name or its time")]
    Quote,
    /// A text read only in part ([`RichText::truncated`]).
    #[error("a text that was read only in part")]
    Truncated,
    /// Containers nested deeper than [`MAX_DEPTH`].
    #[error("containers nested deeper than {MAX_DEPTH}")]
    TooDeep,
    /// HTML longer than [`MAX_CONTENT`] bytes.
    #[error("content longer than {MAX_CONTENT} bytes")]
    TooLong,
}

/// A message's text: a [`RichText`] and the MRI each of its mentions names, by the mention's `item_id`. Everything a
/// post or an edit sends of it (`content`, `properties.mentions`, `properties.qtdMsgs`) and the participants its target
/// adds are built once, when it is made, from that one value.
///
/// Mentions are numbered in the order they appear (`itemid` `0`, `1`, …, as the web client numbers them), whatever the
/// `item_id`s given; each mention appears once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Stored", into = "Stored")]
pub struct MessageText {
    origin: Option<String>,
    files: Vec<crate::UploadedFile>,
    text: RichText,
    mentioned: BTreeMap<String, Mri>,
    pictures: Vec<String>,
    html: String,
    mentions: Vec<Value>,
    quoted: Vec<Value>,
    participants: BTreeSet<Mri>,
    everyone: BTreeSet<String>,
}

impl MessageText {
    /// `text`, whose mentions name the MRIs of `mentioned` by their `item_id`.
    ///
    /// # Errors
    /// See [`ComposeError`]: every mention in `text` must name one entry of `mentioned` and every entry must be named
    /// once; only work accounts and everyone in a group chat (by the chat's id) are mentioned; no image, no link but `http`, `https` and `mailto`, a reply's quote
    /// complete; the text read in full, at most [`MAX_DEPTH`] deep and its HTML at most [`MAX_CONTENT`] bytes.
    pub fn new(text: RichText, mentioned: impl IntoIterator<Item = (String, Mri)>) -> Result<Self, ComposeError> {
        Self::new_in(&crate::Cloud::Microsoft, text, mentioned)
    }

    /// Composes a message whose pictures belong to the account's media service.
    ///
    /// # Errors
    /// Returns the same validation errors as [`Self::new`], including off-cloud pictures.
    pub fn new_in(
        cloud: &crate::Cloud,
        text: RichText,
        mentioned: impl IntoIterator<Item = (String, Mri)>,
    ) -> Result<Self, ComposeError> {
        let mut table = BTreeMap::new();
        for (item_id, mri) in mentioned {
            if !is_work_account(&mri) && !is_group_chat_id(mri.as_str()) {
                return Err(ComposeError::NotAPerson(mri));
            }
            if table.insert(item_id.clone(), mri).is_some() {
                return Err(ComposeError::RepeatedMention(item_id));
            }
        }
        if text.truncated {
            return Err(ComposeError::Truncated);
        }
        let mut render = Render::new(cloud, &table);
        render.nodes(&text.nodes, 0)?;
        if let Some(unused) = table.keys().find(|item_id| !render.used.contains(item_id.as_str())) {
            return Err(ComposeError::UnusedMention(unused.clone()));
        }
        if render.html.len() > MAX_CONTENT {
            return Err(ComposeError::TooLong);
        }
        let Render {
            html,
            mentions,
            quoted,
            participants,
            everyone,
            pictures,
            ..
        } = render;
        Ok(Self {
            origin: cloud.origin().map(ToString::to_string),
            files: Vec::new(),
            text,
            mentioned: table,
            pictures,
            html,
            mentions,
            quoted,
            participants,
            everyone,
        })
    }

    /// Checks the composed text and staged files against the sending account's cloud.
    ///
    /// # Errors
    /// Returns [`ComposeError::FileCloud`] for an off-cloud file, or a text validation error.
    pub fn for_cloud(self, cloud: &crate::Cloud) -> Result<Self, ComposeError> {
        if self.files.iter().any(|file| file.site_in(cloud).is_none()) {
            return Err(ComposeError::FileCloud);
        }
        Self::new_in(cloud, self.text, self.mentioned).map(|text| text.with_files(self.files))
    }

    /// Plain text as one paragraph, no mention.
    ///
    /// # Errors
    /// [`ComposeError::TooLong`] when its HTML is longer than [`MAX_CONTENT`].
    pub fn plain(text: &str) -> Result<Self, ComposeError> {
        let paragraph = RichText {
            nodes: vec![Node::Paragraph(RichText::plain(text).nodes)],
            truncated: false,
        };
        Self::new(paragraph, [])
    }

    /// Private uploads staged by the composer; the engine grants this chat's readers access before queuing the post.
    #[must_use]
    pub fn with_files(mut self, files: Vec<crate::UploadedFile>) -> Self {
        self.files = files;
        self
    }

    #[must_use]
    pub fn files(&self) -> &[crate::UploadedFile] {
        &self.files
    }

    #[must_use]
    pub fn files_property(&self) -> String {
        Value::Array(self.files.iter().map(crate::UploadedFile::entry).collect()).to_string()
    }

    /// The text as given.
    #[must_use]
    pub fn rich_text(&self) -> &RichText {
        &self.text
    }

    /// The MRI each mention names, by the `item_id` given.
    #[must_use]
    pub fn mentioned(&self) -> &BTreeMap<String, Mri> {
        &self.mentioned
    }

    /// The media service's objects of the pictures it shows, in order: the post's `amsreferences`.
    #[must_use]
    pub fn pictures(&self) -> &[String] {
        &self.pictures
    }

    /// The HTML `content` it is sent as.
    #[must_use]
    pub fn html(&self) -> &str {
        &self.html
    }

    /// Whom the message mentions. A write of it adds them to its target.
    #[must_use]
    pub fn participants(&self) -> &BTreeSet<Mri> {
        &self.participants
    }

    /// The group chats whose everyone the message mentions, by the chat's id. Such a mention adds nobody to a write's
    /// target: it reaches the members of the chat the write goes to, and a write sends it only to that chat
    /// ([`Self::mentions_everyone_only_in`]).
    #[must_use]
    pub fn everyone(&self) -> &BTreeSet<String> {
        &self.everyone
    }

    /// Whether every mention of everyone it holds is of `conversation`, the chat a write of it goes to (true without
    /// one): a mention of everyone in another chat isn't sent.
    #[must_use]
    pub fn mentions_everyone_only_in(&self, conversation: &str) -> bool {
        self.everyone.iter().all(|chat| chat == conversation)
    }

    /// `properties.mentions` as sent: a JSON string of the mention entries.
    pub(crate) fn mentions_property(&self) -> String {
        Value::Array(self.mentions.clone()).to_string()
    }

    /// `properties.qtdMsgs` as sent (a JSON string of the quoted messages), when the text quotes one.
    pub(crate) fn quoted_property(&self) -> Option<String> {
        (!self.quoted.is_empty()).then(|| Value::Array(self.quoted.clone()).to_string())
    }
}

/// How a [`MessageText`] is stored (the outbox's payload): the text and its mention table, checked again when read.
#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<crate::UploadedFile>,
    text: RichText,
    mentioned: BTreeMap<String, String>,
}

impl TryFrom<Stored> for MessageText {
    type Error = ComposeError;

    fn try_from(stored: Stored) -> Result<Self, ComposeError> {
        let cloud = stored
            .origin
            .as_deref()
            .map_or(Ok(crate::Cloud::Microsoft), crate::Cloud::own)
            .map_err(|_| ComposeError::Image)?;
        if stored.files.iter().any(|file| file.site_in(&cloud).is_none()) {
            return Err(ComposeError::FileCloud);
        }
        Self::new_in(
            &cloud,
            stored.text,
            stored
                .mentioned
                .into_iter()
                .map(|(item_id, mri)| (item_id, Mri::new(mri))),
        )
        .map(|text| text.with_files(stored.files))
    }
}

impl From<MessageText> for Stored {
    fn from(text: MessageText) -> Self {
        Self {
            origin: text.origin,
            files: text.files,
            text: text.text,
            mentioned: text
                .mentioned
                .into_iter()
                .map(|(item_id, mri)| (item_id, mri.as_str().to_owned()))
                .collect(),
        }
    }
}

/// A mention's span in `content`, as captured (`mention.json`): the name shown, marked with the mention's index in the
/// message, inside the `readonly` element the web client's editor wraps it in.
fn mention_span(index: usize, name: &str) -> String {
    format!(
        "<readonly class=\"skipProofing\" itemtype=\"{MENTION_ITEMTYPE}\" spellcheck=\"false\"><span \
         itemtype=\"{MENTION_ITEMTYPE}\" itemscope itemid=\"{index}\">{}</span></readonly>",
        escape(name)
    )
}

/// A mention's entry in `properties.mentions`, as captured (`mention.json`; a mention of everyone in a group chat
/// `mention-everyone.json`, its `mri` the chat's id): `itemid` a string as sent.
fn mention_entry(index: usize, mri: &Mri, kind: &MentionKind, name: &str) -> Value {
    json!({
        "@type": MENTION_ITEMTYPE,
        "itemid": index.to_string(),
        "mri": mri.as_str(),
        "mentionType": kind.as_str(),
        "displayName": name,
    })
}

/// A work account's MRI: `8:orgid:` and an object id (hex digits and dashes).
fn is_work_account(mri: &Mri) -> bool {
    mri.as_str()
        .strip_prefix("8:orgid:")
        .is_some_and(|oid| !oid.is_empty() && oid.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-'))
}

/// Renders the model into HTML and collects what the properties and the target need.
struct Render<'a> {
    cloud: &'a crate::Cloud,
    table: &'a BTreeMap<String, Mri>,
    html: String,
    mentions: Vec<Value>,
    used: BTreeSet<&'a str>,
    quoted: Vec<Value>,
    participants: BTreeSet<Mri>,
    everyone: BTreeSet<String>,
    pictures: Vec<String>,
}

/// The marks in the order they open, with their tags: bold as Teams' editor writes it (`strong`), and nested as its
/// output nests them (`<i><strong>…</strong></i>`).
const MARK_TAGS: [(Marks, &str); 5] = [
    (Marks::ITALIC, "i"),
    (Marks::STRIKE, "s"),
    (Marks::BOLD, "strong"),
    (Marks::UNDERLINE, "u"),
    (Marks::CODE, "code"),
];

impl<'a> Render<'a> {
    fn new(cloud: &'a crate::Cloud, table: &'a BTreeMap<String, Mri>) -> Self {
        Self {
            cloud,
            table,
            html: String::new(),
            mentions: Vec::new(),
            used: BTreeSet::new(),
            quoted: Vec::new(),
            participants: BTreeSet::new(),
            everyone: BTreeSet::new(),
            pictures: Vec::new(),
        }
    }

    fn push(&mut self, pieces: &[&str]) {
        for piece in pieces {
            self.html.push_str(piece);
        }
    }

    /// Renders `nodes`, inside `depth` containers.
    fn nodes(&mut self, nodes: &[Node], depth: usize) -> Result<(), ComposeError> {
        if depth > MAX_DEPTH {
            return Err(ComposeError::TooDeep);
        }
        for node in nodes {
            self.node(node, depth)?;
        }
        Ok(())
    }

    fn node(&mut self, node: &Node, depth: usize) -> Result<(), ComposeError> {
        match node {
            Node::Text { text, marks } => self.text(text, *marks),
            Node::Break => self.html.push_str("<br>"),
            Node::Paragraph(children) => {
                self.html.push_str("<p>");
                self.nodes(children, depth + 1)?;
                self.html.push_str("</p>");
            }
            Node::Mention { item_id, text } => self.mention(item_id, text)?,
            Node::Link { href, children } => {
                if !is_link(href) {
                    return Err(ComposeError::LinkScheme);
                }
                // as the web client writes a link (`docs/protocol/chat.md`, "Lines and links")
                let href = escape(href);
                self.push(&[
                    "<a href=\"",
                    &href,
                    "\" rel=\"noreferrer noopener\" title=\"",
                    &href,
                    "\" target=\"_blank\">",
                ]);
                self.nodes(children, depth + 1)?;
                self.html.push_str("</a>");
            }
            Node::Span { style, children } => self.span(*style, children, depth)?,
            Node::Heading { level, children } => {
                let tag = format!("h{}", (*level).clamp(1, 6));
                self.push(&["<", &tag, ">"]);
                self.nodes(children, depth + 1)?;
                self.push(&["</", &tag, ">"]);
            }
            Node::Indented { depth: steps, children } => {
                let margin = u32::from((*steps).clamp(1, MAX_INDENT)) * INDENT_STEP;
                self.push(&["<p style=\"margin-left:", &margin.to_string(), "px\">"]);
                self.nodes(children, depth + 1)?;
                self.html.push_str("</p>");
            }
            Node::Rule => self.html.push_str("<hr>"),
            Node::Image(image) => self.picture(image)?,
            Node::Quote(quote) => self.quote(quote, depth)?,
            Node::List(list) => self.list(list, depth)?,
            Node::Code(code) => self.code(code),
        }
        Ok(())
    }

    /// A picture sent to the media service, as a received pasted picture is written (`docs/protocol/chat.md`, "Pictures
    /// in messages", its attributes in that order); its object goes in `amsreferences`. The received ones' `itemscope`
    /// (`png`) is left out: its value for another format was never seen.
    fn picture(&mut self, image: &crate::chat::Image) -> Result<(), ComposeError> {
        let view = crate::MessagePicture::of_in(self.cloud, &image.src).ok_or(ComposeError::Image)?;
        let object = view.object().to_owned();
        if image.item_type.as_deref() != Some(crate::chat::html::AMS_IMAGE_ITEMTYPE)
            || image.item_id.as_deref() != Some(object.as_str())
            || view.view() != "imgo"
        {
            return Err(ComposeError::Image);
        }
        let (width, height) = image.size.ok_or(ComposeError::Image)?;
        let src = escape(&image.src);
        let alt = escape(image.alt.as_deref().unwrap_or("image"));
        let object_html = escape(&object);
        self.push(&[
            "<img src=\"",
            &src,
            "\" itemtype=\"",
            crate::chat::html::AMS_IMAGE_ITEMTYPE,
            "\" width=\"",
            &width.to_string(),
            "\" height=\"",
            &height.to_string(),
            "\" alt=\"",
            &alt,
            "\" id=\"x_",
            &object_html,
            "\" itemid=\"",
            &object_html,
            "\">",
        ]);
        if !self.pictures.contains(&object) {
            self.pictures.push(object);
        }
        Ok(())
    }

    /// A block of code as Teams' code block editor writes one: `pre` with its language's class, `code`, a `br` per line
    /// feed and every space a no-break one (the token kinds are the editor's to mark again).
    fn code(&mut self, code: &Code) {
        self.html.push_str("<pre");
        if let Some(language) = code
            .language
            .as_deref()
            .and_then(|id| language_of(&format!("language-{id}")))
        {
            self.push(&[" class=\"language-", &escape(&language), "\""]);
        }
        self.html.push_str("><code>");
        let text = code.text().replace(' ', "\u{a0}");
        for (at, line) in text.split('\n').enumerate() {
            if at > 0 {
                self.html.push_str("<br>");
            }
            self.html.push_str(&escape(line));
        }
        // the reader drops the one line feed a block ends in, as a browser draws no line after it: a code ending in a
        // blank line gives one `br` more, so it reads back the same
        if text.ends_with('\n') {
            self.html.push_str("<br>");
        }
        self.html.push_str("</code></pre>");
    }

    /// A span of Teams' editor: its colours and size in one `style`, its declarations in the order the editor's
    /// output has them (`background-color`, `color`, `font-size`); a span of no style gives just its content.
    fn span(&mut self, style: SpanStyle, children: &[Node], depth: usize) -> Result<(), ComposeError> {
        if style.is_empty() {
            return self.nodes(children, depth + 1);
        }
        let mut declarations = Vec::new();
        if let Some(highlight) = style.highlight {
            declarations.push(format!("background-color:{}", highlight.hex()));
        }
        if let Some(color) = style.color {
            declarations.push(format!("color:{}", color.hex()));
        }
        if let Some(size) = style.size {
            declarations.push(format!("font-size:{}", size.css()));
        }
        self.push(&["<span style=\"", &declarations.join("; "), "\">"]);
        self.nodes(children, depth + 1)?;
        self.html.push_str("</span>");
        Ok(())
    }

    fn text(&mut self, text: &str, marks: Marks) {
        let open: Vec<&str> = MARK_TAGS
            .iter()
            .filter(|(mark, _)| marks.contains(*mark))
            .map(|(_, tag)| *tag)
            .collect();
        for tag in &open {
            self.push(&["<", tag, ">"]);
        }
        self.html.push_str(&escape(text));
        for tag in open.iter().rev() {
            self.push(&["</", tag, ">"]);
        }
    }

    fn mention(&mut self, item_id: &str, name: &str) -> Result<(), ComposeError> {
        let Some((key, mri)) = self.table.get_key_value(item_id) else {
            return Err(ComposeError::UnknownMention(item_id.to_owned()));
        };
        if !self.used.insert(key.as_str()) {
            return Err(ComposeError::RepeatedMention(item_id.to_owned()));
        }
        let index = self.mentions.len();
        self.html.push_str(&mention_span(index, name));
        if is_group_chat_id(mri.as_str()) {
            self.mentions
                .push(mention_entry(index, mri, &MentionKind::Everyone, name));
            self.everyone.insert(mri.as_str().to_owned());
        } else {
            self.mentions
                .push(mention_entry(index, mri, &MentionKind::Person, name));
            self.participants.insert(mri.clone());
        }
        Ok(())
    }

    /// A list: `ol` (with its `start` when it isn't 1) or `ul`, an `li` per item.
    fn list(&mut self, list: &List, depth: usize) -> Result<(), ComposeError> {
        let tag = if list.ordered { "ol" } else { "ul" };
        self.push(&["<", tag]);
        if list.ordered && list.start != 1 {
            self.push(&[" start=\"", &list.start.to_string(), "\""]);
        }
        self.html.push('>');
        for item in &list.items {
            self.html.push_str("<li>");
            self.nodes(item, depth + 2)?;
            self.html.push_str("</li>");
        }
        self.push(&["</", tag, ">"]);
        Ok(())
    }

    /// A reply's quote (it names a message) in the captured form, or a plain quote.
    fn quote(&mut self, quote: &Quote, depth: usize) -> Result<(), ComposeError> {
        let Some(message_id) = &quote.message_id else {
            self.html.push_str("<blockquote>");
            self.nodes(&quote.body, depth + 1)?;
            self.html.push_str("</blockquote>");
            return Ok(());
        };
        let (Some(sender), Some(name), Some(time)) = (&quote.sender, &quote.sender_name, &quote.time) else {
            return Err(ComposeError::Quote);
        };
        let time_ms: u64 = time.parse().map_err(|_| ComposeError::Quote)?;
        within_depth(&quote.body, depth + 1)?;
        let preview = RichText {
            nodes: quote.body.clone(),
            truncated: false,
        }
        .plain_text();
        self.push(&[
            "<blockquote itemscope itemtype=\"",
            REPLY_ITEMTYPE,
            "\" itemid=\"",
            &escape(message_id),
            "\"><strong itemprop=\"mri\" itemid=\"",
            &escape(sender),
            "\">",
            &escape(name),
            "</strong><span itemprop=\"time\" itemid=\"",
            &escape(time),
            "\"></span><p itemprop=\"preview\">",
            &escape(preview.trim_end_matches('\n')),
            "</p></blockquote>",
        ]);
        self.quoted
            .push(json!({"messageId": message_id, "sender": sender, "time": time_ms}));
        Ok(())
    }
}

/// Checks that `nodes`, inside `depth` containers, nest no deeper than [`MAX_DEPTH`], as [`Render::nodes`] checks what
/// it renders: for content that is kept in the text but not rendered (a reply's preview, sent as its plain text), so a
/// text too deep to store and read back is refused when it is made.
fn within_depth(nodes: &[Node], depth: usize) -> Result<(), ComposeError> {
    if depth > MAX_DEPTH {
        return Err(ComposeError::TooDeep);
    }
    for node in nodes {
        match node {
            Node::Paragraph(children)
            | Node::Link { children, .. }
            | Node::Span { children, .. }
            | Node::Heading { children, .. }
            | Node::Indented { children, .. } => within_depth(children, depth + 1)?,
            Node::Quote(quote) => within_depth(&quote.body, depth + 1)?,
            Node::List(list) => {
                for item in &list.items {
                    within_depth(item, depth + 2)?;
                }
            }
            Node::Text { .. } | Node::Break | Node::Mention { .. } | Node::Image(_) | Node::Code(_) | Node::Rule => {}
        }
    }
    Ok(())
}

/// An `http`, `https` or `mailto` URL.
fn is_link(href: &str) -> bool {
    Url::parse(href).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
}

/// `text` with `&`, `<`, `>` and `"` escaped, for an element's text or a quoted attribute, and a no-break space as
/// `&nbsp;` (the web client's form of the space after a mention).
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\u{a0}' => escaped.push_str("&nbsp;"),
            other => escaped.push(other),
        }
    }
    escaped
}
