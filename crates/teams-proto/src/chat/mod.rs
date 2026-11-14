//! The chat service's reads and the message model (M1 Task 10), from the shapes `docs/protocol/chat.md` records:
//!
//! - [`ChatReads`]: the conversation list, message pages (newest, around a time, and the service's own sync and
//!   paging links, followed only on the same chat base), the thread read and the member list, each a `GET` through
//!   the native transport;
//! - the model ([`Conversation`], [`Message`], [`Thread`], …): unknown fields kept, unknown kinds kept as their text,
//!   never a parse failure for what this build doesn't know;
//! - [`RichText`]: a message's HTML as a small typed model (text runs, mentions, quotes, links, images, lists, code
//!   blocks), with unknown markup kept as text and nothing active surviving;
//! - [`MessageText`]: what a message is written from (M1 Task 12), a [`RichText`] and the MRI each of its mentions
//!   names, from which a post's or an edit's `content`, `properties.mentions` and target are all built.
//!
//! Received attachments are parsed, never fetched: a file `properties.files` names in the shape a received message
//! showed is read as a [`SharedFile`] (its name, type and links: [`shared_files`]); `amsreferences`, `properties.cards` with
//! an entry and a file entry of another shape are kept as they came ([`Unshown`]: no capture showed one, so a view
//! says the message has content it can't show and never guesses at it), and an image's `src`, `itemtype`, `itemid` and size are kept on [`Image`] (a view asks the
//! engine for a pasted picture's bytes or an emoticon's, never this crate).

pub mod card;
mod code;
mod compose;
mod event;
mod files;
mod html;
mod links;
mod model;
mod policy;
mod read;

pub use code::{CODE_BLOCK_ITEMTYPE, Code, CodeRun, Token, language_of};
pub use compose::{ComposeError, MessageText};
pub use event::{
    CALL_MEDIA_PREFIX, CallEnd, CallStart, ChatEvent, ChatPolicy, MeetingTimes, Member, Members, Recording,
    RecordingContents, RecordingItem, RecordingStatus, Removed, TopicUpdate, Transcript, chat_event, is_call_media,
    meeting_times,
};
pub use files::{
    FILE_TYPE, FileKind, FilePreview, SharedFile, SharedFiles, is_file_link, is_sharepoint_host, shared_files,
};
pub use html::{
    AMS_IMAGE_ITEMTYPE, EMOJI_ITEMTYPE, FORWARD_ITEMTYPE, FontSize, INDENT_STEP, Image, List, MAX_CONTENT, MAX_DEPTH,
    MAX_INDENT, MAX_LIST_START, MENTION_ITEMTYPE, Marks, Node, Quote, REPLY_ITEMTYPE, Rgb, RichText, SpanStyle,
    ThreadActivity, parse as parse_html, thread_activity,
};
pub use links::{HYPERLINK_TYPE, LinkPreview, link_previews};
pub use model::{
    AppConsumptionHorizon, ConsumptionHorizon, Conversation, ConversationKind, ConversationList, Field, Forwarded,
    Malformed, MemberList, Mention, MentionKind, Message, MessagePage, MessageType, QuotedMessage, Reaction,
    ReactionUser, Thread, ThreadMember, Unshown, WEB_CLIENT, forwarded, iso_millis, unshown_parts, web_chat_link,
};
pub use policy::{GENERIC_STREAM, PolicyNotice, PolicyState, PolicyViolation, ViolationKind, is_generic_stream};
pub use read::{ChatReadError, ChatReads, MAX_PAGE_SIZE, link_endpoint, link_endpoint_in};
