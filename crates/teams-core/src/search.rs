//! Local search (M2 Task 15): the core hands the store each message's plain text as it stores it, and indexes the
//! messages stored before the index existed in the background. The query is the store's ([`Store::search`]): the
//! account's own messages, never the service's search (Substrate lists other tenant users, and is never called).
//!
//! - **The text** ([`search_text`]): a rich-text message's sanitized [`RichText`](teams_proto::chat::RichText) read
//!   as the reader sees it (its runs, mentions' names, links' text, images' `alt`, a quote's text), a `Text`
//!   message's content as it came, a card's text as a reader reads it (`teams_proto::chat::card::card_message_text`;
//!   a link's app cards after the text); none for a deleted message or one that isn't text (a thread event, a call's
//!   record, a call's media message: a recording's or a transcript's record, whose ids aren't text). Every message the core stores goes through [`crate::Outbox::receive`], which indexes it in the same
//!   write ([`Store::upsert_message_indexed`]): an edit replaces its text, a delete removes it.
//! - **The backfill** ([`Backfill`]): the stored messages the index doesn't hold at their version, a bounded batch at
//!   a time ([`INDEX_BATCH`]), in the store's fixed order, from the engine's loop between its other work. The store
//!   keeps what is indexed, so a backfill that stopped goes on at the next start where it was.

use teams_proto::chat::card::{CARD_TYPE, card_message_text, message_cards_text};
use teams_proto::chat::{is_call_media, parse_html, with_everyone_shown};
use teams_store::{IndexCursor, IndexText, Message, Store, StoreError};

/// How many messages one step of the backfill indexes at most.
pub const INDEX_BATCH: usize = 200;

/// The text of `message` the search finds it by (see the module docs); empty when it has none.
#[must_use]
pub fn search_text(message: &Message) -> String {
    if message.delete_time.is_some() || is_call_media(&message.message_type) {
        String::new()
    } else if message.message_type == "Text" {
        // plain text: its `<` and `&` are the text's own
        with_cards(message, message.content.clone())
    } else if message.message_type == CARD_TYPE {
        card_message_text(&message.content).unwrap_or_default()
    } else if message.message_type.starts_with("RichText") {
        let text = with_everyone_shown(parse_html(&message.content), message.properties.as_deref()).plain_text();
        with_cards(message, text)
    } else {
        String::new()
    }
}

fn with_cards(message: &Message, text: String) -> String {
    match message_cards_text(&message.message_type, &message.content, message.properties.as_deref()) {
        Some(cards) if !cards.is_empty() => format!("{}\n{cards}", text.trim_end()),
        _ => text,
    }
}

/// What one step of the backfill did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A full batch was indexed (how many of it); more may be left.
    Indexed(usize),
    /// The last batch was indexed (how many of it; none when nothing was left): the backfill is over.
    Done(usize),
}

/// The backfill of the messages the index doesn't hold (see the module docs): one per engine run, from the store's
/// first message on.
#[derive(Debug)]
pub struct Backfill {
    batch: usize,
    /// Where the last batch ended.
    after: Option<IndexCursor>,
    /// How many messages it indexed.
    indexed: usize,
    done: bool,
}

impl Backfill {
    /// A backfill of batches of `batch` messages (at least one).
    #[must_use]
    pub fn new(batch: usize) -> Self {
        Self {
            batch: batch.max(1),
            after: None,
            indexed: 0,
            done: false,
        }
    }

    /// Whether it is over.
    #[must_use]
    pub fn done(&self) -> bool {
        self.done
    }

    /// How many messages it indexed so far.
    #[must_use]
    pub fn indexed(&self) -> usize {
        self.indexed
    }

    /// Indexes the next batch: the next messages after the last batch's that the index doesn't hold at their version,
    /// their text read here and stored in one write. [`Step::Done`] for a batch that comes short (nothing more is left)
    /// and for every step after it, [`Backfill::done`] from then on.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the store can't be read or written: nothing of the batch is indexed, and the next step tries
    /// it again.
    pub fn step(&mut self, store: &mut Store) -> Result<Step, StoreError> {
        if self.done {
            return Ok(Step::Done(0));
        }
        let messages = store.messages_to_index(self.after.as_ref(), self.batch)?;
        let texts: Vec<IndexText> = messages
            .iter()
            .map(|message| IndexText {
                conversation_id: message.conversation_id.clone(),
                message_id: message.id.clone(),
                version: message.version,
                text: search_text(message),
            })
            .collect();
        let indexed = store.index_messages(&texts)?;
        self.indexed += indexed;
        if let Some(last) = messages.last() {
            self.after = Some(IndexCursor {
                conversation_id: last.conversation_id.clone(),
                message_id: last.id.clone(),
            });
        }
        if messages.len() < self.batch {
            self.done = true;
            return Ok(Step::Done(indexed));
        }
        Ok(Step::Indexed(indexed))
    }
}
