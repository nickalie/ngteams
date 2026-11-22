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
//!   `mentionType` `everyone` (`mention-everyone.json`, captured in `abc-everyone` on 2026-10-29); a bot's mention in
//!   the same form, its MRI `28:<bot id>`, its `mentionType` `bot` and its app's id as `appId` (the web bundle's
//!   shape, no capture yet: `docs/protocol/chat.md`, "Mentions");
//! - a picture sent to the media service (`crate::ams::sent_picture`: itemtype `AMSImage`, its `imgo` view on an AMS
//!   host, `itemid` its object) as the web client writes a pasted picture, its object in the post's `amsreferences`
//!   ([`MessageText::pictures`]); any other image is refused.
//! - a sticker ([`Sticker`]) in the `readonly` element of itemtype Sticker the web client sends around it, labelled by
//!   its `aria-label` and `title`: one of Teams' own stickers as captured in `team-manage` on 2026-11-19 (its `<img>` of
//!   itemtype Sticker on Teams' sticker CDN, with the editor's `data-image-*` attributes), a picture sent to the media
//!   service as a pasted picture's `<img>` (that form inside the element was not captured yet); [`sticker_message`]
//!   is the message of a sticker alone, between the two blank paragraphs the web client sends around it.
//!
//! - an app's card (a message extension's result, `docs/protocol/chat.md`, "App cards") as the web client sends a
//!   link's app card: after the text, a span of itemtype `InputExtension` naming the card's client id, and the card in
//!   `properties.cards` ([`MessageText::with_cards`], [`AppCard`]); a message with none sends `cards` `[]` as before.
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
    INDENT_STEP, Image, List, MAX_CONTENT, MAX_DEPTH, MAX_INDENT, MENTION_ITEMTYPE, Marks, Node, Quote, REPLY_ITEMTYPE,
    RichText, STICKER_ITEMTYPE, SpanStyle, Sticker, Table,
};

/// Why a [`MessageText`] can't be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    /// The stored account origin is not a bare HTTPS origin.
    #[error("the stored account origin can't be read")]
    Origin,
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
    /// A mention of anyone but a work account (`8:orgid:<object id>`), everyone in a group chat (the chat's id) or a
    /// bot whose app is given (`28:<bot id>`, [`MessageText::new_with_bots`]).
    #[error("a mention of {0}, which isn't a work account, a group chat or a bot with its app")]
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
    /// The apps' cards after the text, and how long `html` is without their spans; boxed, as most messages have none.
    cards: Option<Box<(Vec<AppCard>, usize)>>,
    /// Files shared on from other messages, each its entry as a share posts it ([`crate::chat::share_entry`]).
    shared: Vec<Value>,
    text: RichText,
    mentioned: BTreeMap<String, Mri>,
    /// The app of each bot it mentions, by the bot's MRI.
    bots: BTreeMap<Mri, String>,
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
        Self::build(cloud, text, mentioned, &BTreeMap::new())
    }

    /// [`Self::new`], its mentions of bots (`28:<bot id>`) each written with its app's id as `bots` gives it by the
    /// bot's MRI; an entry no mention names is left out.
    ///
    /// # Errors
    /// As [`Self::new`]; a bot mentioned without its app is [`ComposeError::NotAPerson`].
    pub fn new_with_bots(
        text: RichText,
        mentioned: impl IntoIterator<Item = (String, Mri)>,
        bots: &BTreeMap<Mri, String>,
    ) -> Result<Self, ComposeError> {
        Self::build(&crate::Cloud::Microsoft, text, mentioned, bots)
    }

    fn build(
        cloud: &crate::Cloud,
        text: RichText,
        mentioned: impl IntoIterator<Item = (String, Mri)>,
        apps: &BTreeMap<Mri, String>,
    ) -> Result<Self, ComposeError> {
        let mut table = BTreeMap::new();
        let mut bots = BTreeMap::new();
        for (item_id, mri) in mentioned {
            if is_bot(&mri) {
                let Some(app) = apps.get(&mri).filter(|app| is_app_id(app)) else {
                    return Err(ComposeError::NotAPerson(mri));
                };
                bots.insert(mri.clone(), app.clone());
            } else if !is_work_account(&mri) && !is_group_chat_id(mri.as_str()) {
                return Err(ComposeError::NotAPerson(mri));
            }
            if table.insert(item_id.clone(), mri).is_some() {
                return Err(ComposeError::RepeatedMention(item_id));
            }
        }
        if text.truncated {
            return Err(ComposeError::Truncated);
        }
        let mut render = Render::new(cloud, &table, &bots);
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
            cards: None,
            shared: Vec::new(),
            text,
            mentioned: table,
            bots,
            pictures,
            html,
            mentions,
            quoted,
            participants,
            everyone,
        })
    }

    /// Checks the composed text and staged files against the sending account's cloud. A picture the composer put in
    /// Microsoft's form ([`crate::sent_picture`]: the composer knows no account) goes on the cloud's own media
    /// service, the same object: an own server's upload named it there.
    ///
    /// # Errors
    /// Returns [`ComposeError::FileCloud`] for an off-cloud file, or a text validation error.
    pub fn for_cloud(self, cloud: &crate::Cloud) -> Result<Self, ComposeError> {
        if self.files.iter().any(|file| !file.is_in(cloud))
            || !self.shared.iter().all(|entry| super::is_share_entry_in(cloud, entry))
        {
            return Err(ComposeError::FileCloud);
        }
        let mut text = self.text;
        if !cloud.is_microsoft() {
            pictures_on(cloud, &mut text.nodes);
        }
        let cards = self.cards.map(|cards| cards.0).unwrap_or_default();
        Self::build(cloud, text, self.mentioned, &self.bots)
            .map(|text| text.with_files(self.files).with_cards(cards).with_shared(self.shared))
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

    /// The text with `cards` after it, in place of any it had: each a span after the text and an entry of
    /// `properties.cards` (see the module docs).
    #[must_use]
    pub fn with_cards(mut self, cards: Vec<AppCard>) -> Self {
        if let Some(old) = self.cards.take() {
            self.html.truncate(old.1);
        }
        if cards.is_empty() {
            return self;
        }
        let text_html = self.html.len();
        for card in &cards {
            self.html.push_str("\n<span itemid=\"");
            self.html.push_str(&escape(&card.client_id));
            self.html.push_str("\" itemscope=\"\" itemtype=\"");
            self.html.push_str(INPUT_EXTENSION);
            self.html.push_str("\"><span itemprop=\"cardId\"></span></span>");
        }
        self.cards = Some(Box::new((cards, text_html)));
        self
    }

    /// The apps' cards it carries.
    #[must_use]
    pub fn cards(&self) -> &[AppCard] {
        self.cards.as_ref().map_or(&[], |cards| cards.0.as_slice())
    }

    /// `properties.cards` as sent: a JSON string of the cards' entries, `[]` with none.
    #[must_use]
    pub fn cards_property(&self) -> String {
        Value::Array(self.cards().iter().map(AppCard::entry).collect()).to_string()
    }

    /// Files shared on from other messages, each its entry as a share posts it ([`crate::chat::share_entry`]): the
    /// post names them as they are, with no upload and no grant.
    #[must_use]
    pub fn with_shared(mut self, entries: Vec<Value>) -> Self {
        self.shared = entries;
        self
    }

    /// The entries of the files it shares on ([`Self::with_shared`]).
    #[must_use]
    pub fn shared(&self) -> &[Value] {
        &self.shared
    }

    /// `properties.files` as sent: the uploads' entries, then the entries of the files shared on.
    #[must_use]
    pub fn files_property(&self) -> String {
        Value::Array(
            self.files
                .iter()
                .map(crate::UploadedFile::entry)
                .chain(self.shared.iter().cloned())
                .collect(),
        )
        .to_string()
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

    /// The app of each bot it mentions, by the bot's MRI.
    #[must_use]
    pub fn bots(&self) -> &BTreeMap<Mri, String> {
        &self.bots
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cards: Vec<AppCard>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    shared: Vec<Value>,
    text: RichText,
    mentioned: BTreeMap<String, String>,
    /// The app of each bot mentioned, by the bot's MRI.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    bots: BTreeMap<String, String>,
}

impl TryFrom<Stored> for MessageText {
    type Error = ComposeError;

    fn try_from(stored: Stored) -> Result<Self, ComposeError> {
        let cloud = stored
            .origin
            .as_deref()
            .map_or(Ok(crate::Cloud::Microsoft), crate::Cloud::own)
            .map_err(|_| ComposeError::Origin)?;
        if stored.files.iter().any(|file| !file.is_in(&cloud))
            || !stored
                .shared
                .iter()
                .all(|entry| super::is_share_entry_in(&cloud, entry))
        {
            return Err(ComposeError::FileCloud);
        }
        let bots = stored.bots.into_iter().map(|(bot, app)| (Mri::new(bot), app)).collect();
        Self::build(
            &cloud,
            stored.text,
            stored
                .mentioned
                .into_iter()
                .map(|(item_id, mri)| (item_id, Mri::new(mri))),
            &bots,
        )
        .map(|text| {
            text.with_files(stored.files)
                .with_cards(stored.cards)
                .with_shared(stored.shared)
        })
    }
}

impl From<MessageText> for Stored {
    fn from(text: MessageText) -> Self {
        Self {
            origin: text.origin,
            files: text.files,
            cards: text.cards.map(|cards| cards.0).unwrap_or_default(),
            shared: text.shared,
            text: text.text,
            mentioned: text
                .mentioned
                .into_iter()
                .map(|(item_id, mri)| (item_id, mri.as_str().to_owned()))
                .collect(),
            bots: text
                .bots
                .into_iter()
                .map(|(bot, app)| (bot.as_str().to_owned(), app))
                .collect(),
        }
    }
}

/// The itemtype of an app card's span in `content`.
const INPUT_EXTENSION: &str = "http://schema.skype.com/InputExtension";

/// An app's card a message carries (`properties.cards`, `docs/protocol/chat.md`, "App cards"): the app, the card, and
/// the client id its span in `content` names. Seen for a link's card; a message extension's result goes the same way
/// (unseen).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppCard {
    pub app_id: String,
    pub app_name: String,
    pub app_icon: Option<String>,
    /// The card's `contentType` (an Adaptive Card, a hero or a thumbnail card).
    pub content_type: String,
    pub content: Value,
    /// `app-preview-card-<hex>`, made once ([`AppCard::new`]), so a resend names the same card.
    pub client_id: String,
}

impl AppCard {
    /// The card `content` of type `content_type` from the app `app_id` named `app_name`, with a client id of its own.
    #[must_use]
    pub fn new(
        app_id: String,
        app_name: String,
        app_icon: Option<String>,
        content_type: String,
        content: Value,
    ) -> Self {
        let mut bytes = [0_u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            bytes = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos())
                .to_le_bytes();
        }
        let hex: String = bytes
            .iter()
            .flat_map(|byte| [byte >> 4, byte & 0x0f])
            .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
            .collect();
        Self {
            app_id,
            app_name,
            app_icon,
            content_type,
            content,
            client_id: format!("app-preview-card-{hex}"),
        }
    }

    /// Its entry in `properties.cards`.
    fn entry(&self) -> Value {
        let mut entry = json!({
            "appId": self.app_id,
            "appName": self.app_name,
            "cardClientId": self.client_id,
            "contentType": self.content_type,
            "content": self.content,
        });
        if let (Some(icon), Some(object)) = (&self.app_icon, entry.as_object_mut()) {
            object.insert("appIcon".to_owned(), json!(icon));
        }
        entry
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

/// A bot's mention entry: a person's with `mentionType` `bot` and its app's id (`appId`), as the web bundle names it.
fn bot_entry(index: usize, bot: &Mri, app: &str, name: &str) -> Value {
    let mut entry = mention_entry(index, bot, &MentionKind::Bot, name);
    entry["appId"] = json!(app);
    entry
}

/// A work account's MRI: `8:orgid:` and an object id (hex digits and dashes).
fn is_work_account(mri: &Mri) -> bool {
    mri.as_str().strip_prefix("8:orgid:").is_some_and(is_guid_like)
}

/// A bot's MRI: `28:` and its id (hex digits and dashes).
fn is_bot(mri: &Mri) -> bool {
    mri.as_str().strip_prefix("28:").is_some_and(is_guid_like)
}

fn is_guid_like(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

/// An app's id as the catalogue gives one: a GUID, or a name like `com.microsoft.teamspace.tab.planner`.
fn is_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
}

/// Renders the model into HTML and collects what the properties and the target need.
struct Render<'a> {
    cloud: &'a crate::Cloud,
    table: &'a BTreeMap<String, Mri>,
    bots: &'a BTreeMap<Mri, String>,
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
    fn new(cloud: &'a crate::Cloud, table: &'a BTreeMap<String, Mri>, bots: &'a BTreeMap<Mri, String>) -> Self {
        Self {
            cloud,
            table,
            bots,
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
            Node::Sticker(sticker) => self.sticker(sticker)?,
            Node::Quote(quote) => self.quote(quote, depth)?,
            Node::List(list) => self.list(list, depth)?,
            Node::Table(table) => self.table(table, depth)?,
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

    /// A sticker in the `readonly` element the web client sends around one, its label as the element's `aria-label`
    /// and `title`: one of Teams' own as the capture shows it (its `<img>`'s attributes in that order), any other as a
    /// picture sent to the media service ([`Self::picture`]).
    fn sticker(&mut self, sticker: &Sticker) -> Result<(), ComposeError> {
        let label = sticker
            .label
            .as_deref()
            .or(sticker.image.alt.as_deref())
            .filter(|label| !label.trim().is_empty())
            .map_or_else(|| STICKER_LABEL.to_owned(), escape);
        self.push(&[
            "<readonly aria-label=\"",
            &label,
            "\" contenteditable=\"false\" itemtype=\"",
            STICKER_ITEMTYPE,
            "\" title=\"",
            &label,
            "\">",
        ]);
        if crate::StickerPicture::of(&sticker.image.src).is_some() {
            let (width, height) = sticker.image.size.ok_or(ComposeError::Image)?;
            self.push(&[
                "<img src=\"",
                &escape(&sticker.image.src),
                "\" height=\"",
                &height.to_string(),
                "\" alt=\"",
                &label,
                "\" width=\"",
                &width.to_string(),
                "\" data-image-type=\"standard\" data-image-mode=\"single\" itemtype=\"",
                STICKER_ITEMTYPE,
                "\">",
            ]);
        } else {
            self.picture(&sticker.image)?;
        }
        self.html.push_str("</readonly>");
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
        } else if let Some(app) = self.bots.get(mri) {
            self.mentions.push(bot_entry(index, mri, app, name));
            self.participants.insert(mri.clone());
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

    /// A table, as it was read: its rows, each its cells (a header's a `th`), inside the three containers it was read
    /// as (the table, the row, the cell).
    fn table(&mut self, table: &Table, depth: usize) -> Result<(), ComposeError> {
        self.html.push_str("<table><tbody>");
        for row in &table.rows {
            self.html.push_str("<tr>");
            for cell in row {
                let tag = if cell.header { "th" } else { "td" };
                self.push(&["<", tag, ">"]);
                self.nodes(&cell.nodes, depth + 3)?;
                self.push(&["</", tag, ">"]);
            }
            self.html.push_str("</tr>");
        }
        self.html.push_str("</tbody></table>");
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
            Node::Table(table) => {
                for cell in table.rows.iter().flatten() {
                    within_depth(&cell.nodes, depth + 3)?;
                }
            }
            Node::Text { .. }
            | Node::Break
            | Node::Mention { .. }
            | Node::Image(_)
            | Node::Sticker(_)
            | Node::Code(_)
            | Node::Rule => {}
        }
    }
    Ok(())
}

/// Each picture in `nodes` that is in Microsoft's sent form moved to `cloud`'s media service (see
/// [`MessageText::for_cloud`]); any other picture stays as it is, for the render to check.
fn pictures_on(cloud: &crate::Cloud, nodes: &mut [Node]) {
    for node in nodes {
        match node {
            Node::Image(image) | Node::Sticker(Sticker { image, .. }) => picture_on(cloud, image),
            Node::Paragraph(children)
            | Node::Link { children, .. }
            | Node::Span { children, .. }
            | Node::Heading { children, .. }
            | Node::Indented { children, .. } => pictures_on(cloud, children),
            Node::Quote(quote) => pictures_on(cloud, &mut quote.body),
            Node::List(list) => {
                for item in &mut list.items {
                    pictures_on(cloud, item);
                }
            }
            Node::Table(table) => {
                for cell in table.rows.iter_mut().flatten() {
                    pictures_on(cloud, &mut cell.nodes);
                }
            }
            Node::Text { .. } | Node::Break | Node::Mention { .. } | Node::Code(_) | Node::Rule => {}
        }
    }
}

/// `image` moved to `cloud`'s media service when it is a picture in Microsoft's sent form.
fn picture_on(cloud: &crate::Cloud, image: &mut Image) {
    let moved = crate::MessagePicture::of_in(&crate::Cloud::Microsoft, &image.src)
        .filter(|picture| picture.view() == "imgo")
        .zip(image.size)
        .and_then(|(picture, size)| crate::sent_picture_in(cloud, picture.object(), size));
    if let Some(moved) = moved {
        image.src = moved.src;
    }
}

/// The label of a sticker that has none.
const STICKER_LABEL: &str = "Sticker";

impl Sticker {
    /// One of Teams' own stickers: its picture `src` on Teams' sticker CDN ([`crate::StickerPicture`]), drawn at `size`
    /// CSS pixels (Teams' are 180 by 180), with `label`. `None` for a `src` off that CDN.
    #[must_use]
    pub fn of_teams(src: &str, size: (u32, u32), label: &str) -> Option<Self> {
        crate::StickerPicture::of(src)?;
        Some(Self {
            image: Image {
                src: src.to_owned(),
                alt: Some(label.to_owned()),
                item_type: Some(STICKER_ITEMTYPE.to_owned()),
                item_id: None,
                size: Some((size.0.max(1), size.1.max(1))),
            },
            label: Some(label.to_owned()),
        })
    }

    /// A picture sent to `cloud`'s media service into `object` as a sticker, drawn at `size` CSS pixels, with `label`:
    /// its `<img>` a pasted picture's ([`crate::sent_picture_in`]). `None` for an id that isn't an object's.
    #[must_use]
    pub fn of_picture(cloud: &crate::Cloud, object: &str, size: (u32, u32), label: &str) -> Option<Self> {
        let image = crate::sent_picture_in(cloud, object, size)?;
        Some(Self {
            image: Image {
                alt: Some(label.to_owned()),
                ..image
            },
            label: Some(label.to_owned()),
        })
    }
}

/// A message of `sticker` alone, as the web client sends one: between two blank paragraphs (`<p>&nbsp;</p>`).
#[must_use]
pub fn sticker_message(sticker: Sticker) -> RichText {
    let blank = || {
        Node::Paragraph(vec![Node::Text {
            text: "\u{a0}".to_owned(),
            marks: Marks::NONE,
        }])
    };
    RichText {
        nodes: vec![blank(), Node::Sticker(sticker), blank()],
        truncated: false,
    }
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
