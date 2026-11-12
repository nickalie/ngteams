//! A translated chat's text outside its open view: the chat list's last-message preview, a new message's desktop
//! notification and the search's message cards show it in the chat's language too, through the same pipeline as the
//! chat's view ([`super::ChatTranslation`]): the same store, the same tool and the same prompts, a message's
//! translation asked with what the tool is told of its chat ([`super::brief`]: its title, who is who, its glossary),
//! the messages before it and the whole of each message it replies to as context.
//!
//! - **One per account** ([`Previews::of`]), shared by the list, the notifications, the search and every open chat's
//!   translation.
//! - **What is shown** ([`Previews::lookup`]): a message of a chat with a language picked shows its kept translation;
//!   the account's own message, sent translated, shows as it was written ([`SentOriginal`]), never as it went out. The
//!   account's own message kept as nothing else shows as written: it is the account's own wording, and never goes to
//!   the tool from here.
//! - **The tool is asked only for what is shown and new** ([`Previews::want`]): a surface asks for the messages it
//!   shows (a list row on screen, a notification, a search card); the store is read first, so a message the chat's view
//!   (or an earlier run) translated costs nothing. What is left waits [`GATHER`] so the rows drawn together go together,
//!   then goes to the tool a chat at a time: up to [`BATCH_ITEMS`] messages in one batch, the messages before them
//!   as context, through one tool process per language that is closed once idle for [`IDLE`]. Each message is asked at
//!   most once a run: one the answer left out shows as written.
//! - **An open chat's view translates its own** ([`Previews::from_view`]): a message the view holds waits for the
//!   view's translation rather than asking the tool twice, and what the view translates is shown here at once.
//! - **Failures** pause the tool for [`PAUSE`]: until then, what is left shows as written.
//! - **Notifications don't wait long** ([`Previews::translated_within`]): one waits at most its budget for the
//!   translation, then shows as written.
//! - **Private**: no message, translation or tool output is logged or traced.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use futures_util::future;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EntityId, Global, Subscription, Task, WeakEntity};
use teams_proto::chat::RichText;
use teams_store::{Message, PageCursor, SentOriginal, StoredTranslation, TranslationKind};

use super::brief::{self, add_senders, brief_of, replied_to};
use super::codec::{Shells, decode, encode, one_line};
use super::prompt::{Answer, Brief, Item, Said, Term};
use super::session::{Job, Limits, Outcome, Session, Spec};
use super::{
    BATCH_CHARS, BATCH_ITEMS, CONTEXT_CHARS, CONTEXT_LINE, CONTEXT_LINES, Entry, MAX_MESSAGE, Source, entry_of_row,
    key_of, original_row, quote_line, stored_original, translatable,
};
use crate::bridge::Core;
use crate::local_ai::{AiChoice, Failure, Tools};
use crate::settings::SettingsModel;
use crate::translate::language;

/// How long the messages wanted are gathered before the store is read for them.
pub const GATHER: Duration = Duration::from_millis(200);
/// How long a tool process stays up with nothing to translate.
pub const IDLE: Duration = Duration::from_secs(60);
/// How long the tool is left alone after it failed.
pub const PAUSE: Duration = Duration::from_secs(120);
/// The longest a message waits for an open view's translation before the tool is asked for it.
pub const DEFER_LIMIT: Duration = Duration::from_secs(10);
/// How long a notification waits for its message's translation.
pub const NOTIFY_BUDGET: Duration = Duration::from_secs(3);
/// What a sender without a name is called in what the tool is given.
const SOMEONE: &str = "Someone";

/// A message's translation into a language: by its conversation, id and version.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    conversation: String,
    id: String,
    version: i64,
    language: &'static str,
}

/// What is known of a message's translation.
#[derive(Clone, Debug)]
enum Known {
    /// The text it shows: its translation, or the account's own as written.
    Text(Arc<RichText>),
    /// It shows as written (in the language already, left out of an answer, or the account's own); until `until`
    /// when the tool failed and is asked again after its pause.
    AsWritten { until: Option<Instant> },
}

impl Known {
    fn lapsed(&self, now: Instant) -> bool {
        matches!(self, Self::AsWritten { until: Some(until) } if now >= *until)
    }

    fn text(&self) -> Option<Arc<RichText>> {
        match self {
            Self::Text(text) => Some(Arc::clone(text)),
            Self::AsWritten { .. } => None,
        }
    }

    /// What an open view's entry shows.
    fn of_entry(entry: &Entry) -> Self {
        match entry {
            Entry::Translated { text, .. } | Entry::Original { text, .. } => Self::Text(Arc::clone(text)),
            Entry::Same { .. } | Entry::Failed { .. } => Self::AsWritten { until: None },
        }
    }
}

/// What a message shows, as a surface asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// Its chat isn't translated (or it isn't a text message): as written.
    Off,
    /// Not known yet: as written until it is ([`Previews::want`] asks for it).
    Unknown,
    /// As written.
    AsWritten,
    /// This text in its place.
    Text(Arc<RichText>),
}

impl Lookup {
    /// The text shown in the message's place, when it isn't its own.
    #[must_use]
    pub fn text(&self) -> Option<&Arc<RichText>> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// What an open chat's view translates: its conversation, the language, the messages it holds and the arrival of
/// the newest of them. It takes in what is newer as it comes, and all of the conversation's while it holds none yet
/// (it is loading them).
#[derive(Clone, Debug)]
pub struct Followed {
    pub conversation: String,
    pub language: &'static str,
    pub held: HashSet<String>,
    pub newest: i64,
}

impl Followed {
    fn holds(&self, key: &Key, message: &Message) -> bool {
        self.conversation == key.conversation
            && self.language == key.language
            && (self.held.is_empty() || self.held.contains(&key.id) || message.arrival_time > self.newest)
    }
}

/// The messages gathered of one chat, in one language.
type Gathered = (String, &'static str, Vec<(Key, Message)>);

/// A batch the tool works on.
struct Running {
    id: u64,
    items: Vec<(usize, Key, Shells)>,
}

/// The account's translations outside the open views (see the module docs). A GPUI entity.
pub struct Previews {
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    choice: AiChoice,
    limits: Limits,
    known: HashMap<Key, Known>,
    /// Asked for and not known yet: being read, waiting for a view, queued or with the tool.
    pending: HashSet<Key>,
    /// Gathered for the store.
    gathered: Vec<(Key, Message)>,
    gathering: Option<Task<()>>,
    /// The messages an open view translates, by the view.
    followed: HashMap<EntityId, Followed>,
    /// Waiting for an open view's translation, since when.
    deferred: HashMap<Key, (Message, Instant)>,
    sweeping: Option<Task<()>>,
    queue: VecDeque<(Key, Message)>,
    preparing: Option<Task<()>>,
    running: Option<Running>,
    /// One tool process per language, with what reads its outcomes.
    sessions: HashMap<&'static str, (Session, Task<()>)>,
    next_id: u64,
    idle: Option<Task<()>>,
    paused_until: Option<Instant>,
    waiters: HashMap<Key, Vec<async_channel::Sender<()>>>,
    /// Batches asked of the tool so far (tests count them).
    batches: u64,
    _settings: Subscription,
}

/// Each account's [`Previews`], by its engine.
#[derive(Default)]
struct Registry(Vec<(Weak<dyn Core>, WeakEntity<Previews>)>);

impl Global for Registry {}

impl Previews {
    /// The account's of `core` (made the first time), following `settings`.
    pub fn of(core: &Arc<dyn Core>, settings: &Entity<SettingsModel>, cx: &mut App) -> Entity<Self> {
        let weak = Arc::downgrade(core);
        let registry = cx.default_global::<Registry>();
        registry.0.retain(|(_, entity)| entity.upgrade().is_some());
        let found = registry
            .0
            .iter()
            .find(|(kept, _)| Weak::ptr_eq(kept, &weak))
            .and_then(|(_, entity)| entity.upgrade());
        if let Some(found) = found {
            return found;
        }
        let (core, settings) = (Arc::clone(core), settings.clone());
        let previews = cx.new(|cx| Self::new(core, settings, cx));
        cx.default_global::<Registry>().0.push((weak, previews.downgrade()));
        previews
    }

    fn new(core: Arc<dyn Core>, settings: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let choice = settings.read(cx).settings().chat_translator.clone();
        let observing = cx.observe(&settings, |this: &mut Self, _, cx| this.settings_changed(cx));
        Self {
            core,
            settings,
            choice,
            limits: Limits::default(),
            known: HashMap::new(),
            pending: HashSet::new(),
            gathered: Vec::new(),
            gathering: None,
            followed: HashMap::new(),
            deferred: HashMap::new(),
            sweeping: None,
            queue: VecDeque::new(),
            preparing: None,
            running: None,
            sessions: HashMap::new(),
            next_id: 1,
            idle: None,
            paused_until: None,
            waiters: HashMap::new(),
            batches: 0,
            _settings: observing,
        }
    }

    /// With these limits for the tool (tests).
    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// Batches asked of the tool so far.
    #[must_use]
    pub fn batches(&self) -> u64 {
        self.batches
    }

    /// Whether a tool process is up.
    #[must_use]
    pub fn running_tools(&self) -> usize {
        self.sessions.len()
    }

    /// The language `conversation` is read in, if one was picked.
    #[must_use]
    pub fn language_of(&self, conversation: &str, cx: &App) -> Option<&'static str> {
        let chat = teams_proto::stored_conversation(conversation);
        self.settings.read(cx).settings().chat_language_of(self.core.me(), chat)
    }

    fn key_of(&self, message: &Message, cx: &App) -> Option<Key> {
        let language = self.language_of(&message.conversation_id, cx)?;
        translatable(message).then(|| Key {
            conversation: message.conversation_id.clone(),
            id: message.id.clone(),
            version: message.version,
            language,
        })
    }

    /// What `message` shows (see the module docs).
    #[must_use]
    pub fn lookup(&self, message: &Message, cx: &App) -> Lookup {
        let Some(key) = self.key_of(message, cx) else {
            return Lookup::Off;
        };
        match self.known.get(&key) {
            Some(known) if known.lapsed(Instant::now()) => Lookup::Unknown,
            Some(Known::Text(text)) => Lookup::Text(Arc::clone(text)),
            Some(Known::AsWritten { .. }) => Lookup::AsWritten,
            None => Lookup::Unknown,
        }
    }

    /// The surface shows `messages`: those of translated chats not known yet are read from the store and, what it
    /// doesn't keep, translated (see the module docs).
    pub fn want(&mut self, messages: impl IntoIterator<Item = Message>, cx: &mut Context<Self>) {
        let now = Instant::now();
        for message in messages {
            let Some(key) = self.key_of(&message, cx) else { continue };
            if self.pending.contains(&key) || self.known.get(&key).is_some_and(|known| !known.lapsed(now)) {
                continue;
            }
            self.pending.insert(key.clone());
            self.gathered.push((key, message));
        }
        if !self.gathered.is_empty() && self.gathering.is_none() {
            self.gathering = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
                cx.background_executor().timer(GATHER).await;
                let _ = this.update(cx, Self::read_store);
            }));
        }
    }

    /// `message`'s text in its chat's language, waiting at most `budget` for it: `None` when it shows as written, or
    /// isn't ready by then.
    pub fn translated_within(
        &mut self,
        message: Message,
        budget: Duration,
        cx: &mut Context<Self>,
    ) -> Task<Option<Arc<RichText>>> {
        let Some(key) = self.key_of(&message, cx) else {
            return Task::ready(None);
        };
        match self.lookup(&message, cx) {
            Lookup::Text(text) => return Task::ready(Some(text)),
            Lookup::AsWritten | Lookup::Off => return Task::ready(None),
            Lookup::Unknown => {}
        }
        let (told, heard) = async_channel::bounded(1);
        self.waiters.entry(key.clone()).or_default().push(told);
        self.want([message], cx);
        // nothing to gather for: the store is read now
        if self.gathering.take().is_some() {
            self.read_store(cx);
        }
        let timer = cx.background_executor().timer(budget);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let heard = heard.recv();
            futures_util::pin_mut!(heard);
            // told, or out of time: what is known by then is shown
            let _ = future::select(heard, timer).await;
            this.read_with(cx, |this, _| this.known.get(&key).and_then(Known::text))
                .ok()
                .flatten()
        })
    }

    /// An open chat's view (`view`) translates `followed` (`None`: no more) and knows `learned` (its messages' entries,
    /// by conversation, id and version, in `language`).
    pub fn from_view(
        &mut self,
        view: EntityId,
        followed: Option<Followed>,
        learned: Vec<(String, String, Entry)>,
        language: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        match followed {
            Some(followed) => {
                self.followed.insert(view, followed);
            }
            None => {
                self.followed.remove(&view);
            }
        }
        let mut changed = false;
        if let Some(language) = language {
            for (conversation, id, entry) in learned {
                let key = Key {
                    conversation,
                    id,
                    version: entry.version(),
                    language,
                };
                let known = Known::of_entry(&entry);
                let same = matches!(
                    (self.known.get(&key), &known),
                    (Some(Known::Text(kept)), Known::Text(new)) if Arc::ptr_eq(kept, new)
                ) || matches!(
                    (self.known.get(&key), &known),
                    (Some(Known::AsWritten { until: None }), Known::AsWritten { .. })
                );
                if !same {
                    self.settle(key, known);
                    changed = true;
                }
            }
        }
        // what is queued and a view now translates waits for it; what waited for a view no longer translating it goes
        // to the tool
        let (held, queued): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|(key, message)| self.held(key, message));
        self.queue = queued.into();
        for (key, message) in held {
            self.defer(key, message, cx);
        }
        let released: Vec<Key> = self
            .deferred
            .iter()
            .filter(|(key, (message, _))| !self.held(key, message))
            .map(|(key, _)| key.clone())
            .collect();
        self.release(released);
        self.pump(cx);
        if changed {
            cx.notify();
        }
    }

    /// An open view holds `key`'s message (`message`), in its language, or takes it in as it comes.
    fn held(&self, key: &Key, message: &Message) -> bool {
        self.followed.values().any(|followed| followed.holds(key, message))
    }

    /// `key` waits for an open view's translation, at most [`DEFER_LIMIT`].
    fn defer(&mut self, key: Key, message: Message, cx: &mut Context<Self>) {
        self.deferred.entry(key).or_insert((message, Instant::now()));
        if self.sweeping.is_none() {
            self.sweeping = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
                cx.background_executor().timer(DEFER_LIMIT).await;
                let _ = this.update(cx, Self::sweep);
            }));
        }
    }

    /// What waited for a view longer than [`DEFER_LIMIT`] goes to the tool.
    fn sweep(&mut self, cx: &mut Context<Self>) {
        self.sweeping = None;
        let now = Instant::now();
        let late: Vec<Key> = self
            .deferred
            .iter()
            .filter(|(_, (_, since))| now.duration_since(*since) >= DEFER_LIMIT)
            .map(|(key, _)| key.clone())
            .collect();
        self.release(late);
        if let Some(((key, message), _)) = self
            .deferred
            .iter()
            .next()
            .map(|(key, (message, since))| ((key.clone(), message.clone()), since))
        {
            // the rest waits its turn: the sweep comes again
            self.deferred.remove(&key);
            self.defer(key, message, cx);
        }
        self.pump(cx);
    }

    /// `keys` no longer wait for a view: queued for the tool.
    fn release(&mut self, keys: Vec<Key>) {
        for key in keys {
            if let Some((message, _)) = self.deferred.remove(&key) {
                self.queue.push_back((key, message));
            }
        }
    }

    /// `key` is known: no longer pending, and whoever waits for it told.
    fn settle(&mut self, key: Key, known: Known) {
        self.pending.remove(&key);
        self.deferred.remove(&key);
        if let Some(waiters) = self.waiters.remove(&key) {
            for waiter in waiters {
                let _ = waiter.try_send(());
            }
        }
        self.known.insert(key, known);
    }

    /// The settings changed: another tool or model starts over; a language picked or dropped is drawn again.
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let choice = self.settings.read(cx).settings().chat_translator.clone();
        if choice != self.choice {
            self.choice = choice;
            self.stop_tools();
            self.paused_until = None;
        }
        cx.notify();
    }

    /// Closes the tool processes; the batch they worked on is asked again when it is shown again.
    fn stop_tools(&mut self) {
        self.sessions.clear();
        self.preparing = None;
        if let Some(running) = self.running.take() {
            for (_, key, _) in running.items {
                self.pending.remove(&key);
            }
        }
    }

    /// Reads from the store what it keeps of the messages gathered, a chat at a time.
    fn read_store(&mut self, cx: &mut Context<Self>) {
        self.gathering = None;
        let gathered = std::mem::take(&mut self.gathered);
        if gathered.is_empty() {
            return;
        }
        let mut chats: Vec<Gathered> = Vec::new();
        for (key, message) in gathered {
            match chats
                .iter_mut()
                .find(|(conversation, language, _)| *conversation == key.conversation && *language == key.language)
            {
                Some((_, _, items)) => items.push((key, message)),
                None => chats.push((key.conversation.clone(), key.language, vec![(key, message)])),
            }
        }
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let me = core.me().to_owned();
            for (conversation, language, items) in chats {
                let ids = items.iter().map(|(key, _)| key.id.clone()).collect();
                let kept = core
                    .translations(conversation.clone(), language.to_owned(), ids)
                    .await
                    .unwrap_or_default();
                let keys: Vec<String> = items
                    .iter()
                    .filter(|(_, message)| message.sender.as_deref() == Some(me.as_str()))
                    .flat_map(|(_, message)| {
                        message
                            .client_message_id
                            .clone()
                            .into_iter()
                            .chain([SentOriginal::edit_key(&message.id)])
                    })
                    .collect();
                let originals = if keys.is_empty() {
                    Vec::new()
                } else {
                    core.sent_originals(conversation, keys).await.unwrap_or_default()
                };
                if this
                    .update(cx, |this, cx| this.stored(items, &kept, &originals, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
    }

    /// What the store keeps of `items`: each known, or waiting for an open view, or queued for the tool.
    fn stored(
        &mut self,
        items: Vec<(Key, Message)>,
        kept: &[StoredTranslation],
        originals: &[SentOriginal],
        cx: &mut Context<Self>,
    ) {
        let me = self.core.me().to_owned();
        let mut bound = Vec::new();
        let mut changed = false;
        for (key, message) in items {
            if !self.pending.contains(&key) {
                // known meanwhile (an open view's), or the tool was changed
                continue;
            }
            let row = kept
                .iter()
                .find(|row| row.message_id == key.id && row.version == key.version)
                .and_then(entry_of_row);
            if let Some(entry) = row {
                self.settle(key, Known::of_entry(&entry));
                changed = true;
                continue;
            }
            if message.sender.as_deref() == Some(me.as_str()) {
                // the account's own: as written, unless it was sent translated
                let source = source_of(&message);
                let known = match stored_original(&source, key.language, originals) {
                    Some(original) => {
                        bound.push(original_row(&source, key.language, &original));
                        Known::Text(original)
                    }
                    None => Known::AsWritten { until: None },
                };
                self.settle(key, known);
                changed = true;
            } else if self.held(&key, &message) {
                self.defer(key, message, cx);
            } else {
                self.queue.push_back((key, message));
            }
        }
        if !bound.is_empty() {
            drop(self.core.keep_translations(bound));
        }
        self.pump(cx);
        if changed {
            cx.notify();
        }
    }

    /// Hands the tool the next chat's batch, when none runs: up to [`BATCH_ITEMS`] of its messages, the newest first.
    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.running.is_some() || self.preparing.is_some() {
            return;
        }
        let Some((first, _)) = self.queue.front() else {
            self.rest(cx);
            return;
        };
        let (conversation, language) = (first.conversation.clone(), first.language);
        if let Some(until) = self.paused_until {
            if Instant::now() < until {
                self.drop_queue(Some(until), cx);
                return;
            }
            self.paused_until = None;
        }
        let (mut chat, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|(key, _)| key.conversation == conversation && key.language == language);
        self.queue = rest.into();
        // the newest first, as many as a batch holds; the rest wait their turn
        chat.sort_by_key(|(_, message)| std::cmp::Reverse((message.arrival_time, message.id.clone())));
        let mut chosen = Vec::new();
        let mut chars = 0;
        let mut later = Vec::new();
        for (key, message) in chat {
            let (text, shells) = encode(&crate::compose::rich_text_of(&message));
            if text.len() > MAX_MESSAGE {
                self.settle(key, Known::AsWritten { until: None });
                continue;
            }
            if !chosen.is_empty() && (chosen.len() >= BATCH_ITEMS || chars + text.len() > BATCH_CHARS) {
                later.push((key, message));
                continue;
            }
            chars += text.len();
            chosen.push((key, message, text, shells));
        }
        for item in later.into_iter().rev() {
            self.queue.push_front(item);
        }
        if chosen.is_empty() {
            return self.pump(cx);
        }
        chosen.reverse();
        let core = Arc::clone(&self.core);
        self.idle = None;
        self.preparing = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let prepared = prepare(&*core, &conversation, language, &chosen).await;
            let keys = chosen.into_iter().map(|(key, _, _, shells)| (key, shells)).collect();
            let _ = this.update(cx, |this, cx| this.start(language, prepared, keys, cx));
        }));
    }

    /// Hands a prepared batch (what the tool is told of its chat, its context and its items) to the language's tool,
    /// numbered here.
    fn start(
        &mut self,
        language: &'static str,
        (brief, context, items): (Brief, Vec<Said>, Vec<Item>),
        keys: Vec<(Key, Shells)>,
        cx: &mut Context<Self>,
    ) {
        self.preparing = None;
        if self.ensure_session(language, cx).is_err() {
            self.failed(keys.into_iter().map(|(key, _)| key).collect(), cx);
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let running = keys
            .into_iter()
            .enumerate()
            .map(|(n, (key, shells))| (n + 1, key, shells))
            .collect();
        if let Some((session, _)) = self.sessions.get(language) {
            session.send(Job::Batch {
                id,
                brief: Arc::new(brief),
                context,
                items,
            });
            self.batches += 1;
            self.running = Some(Running { id, items: running });
        }
    }

    /// The language's tool process, started when there is none.
    fn ensure_session(&mut self, language: &'static str, cx: &mut Context<Self>) -> Result<(), Failure> {
        if self.sessions.contains_key(language) {
            return Ok(());
        }
        let (backend, program) = Tools::of(cx).pick(self.choice.tool)?;
        let name = self::language(language).map_or("English", |(_, name)| name);
        let session = Session::start(Spec {
            backend,
            program,
            model: self.choice.model.clone(),
            language: name,
            limits: self.limits,
        });
        let outcomes = session.outcomes();
        let receiving = cx.spawn(async move |this, cx: &mut AsyncApp| {
            while let Ok(outcome) = outcomes.recv().await {
                if this.update(cx, |this, cx| this.received(outcome, cx)).is_err() {
                    break;
                }
            }
        });
        self.sessions.insert(language, (session, receiving));
        Ok(())
    }

    fn received(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        let Outcome::Batch { id, answers, terms, .. } = outcome else {
            return;
        };
        let Some(running) = self.running.take_if(|running| running.id == id) else {
            return;
        };
        if let Ok(answers) = answers {
            self.keep_terms(&running, &terms);
            self.answered(running, &answers);
        } else {
            let keys = running.items.into_iter().map(|(_, key, _)| key).collect();
            self.failed(keys, cx);
        }
        self.pump(cx);
        cx.notify();
    }

    /// Terms a batch's answer added to its chat's glossary, kept in the store: its next batch is told them.
    fn keep_terms(&self, running: &Running, terms: &[Term]) {
        let Some((_, key, _)) = running.items.first() else {
            return;
        };
        if !terms.is_empty() {
            drop(
                self.core
                    .keep_glossary(brief::rows(&key.conversation, key.language, terms)),
            );
        }
    }

    /// A batch's answers, each known and kept in the store; one left out shows as written.
    fn answered(&mut self, running: Running, answers: &HashMap<usize, Answer>) {
        let mut rows = Vec::new();
        for (n, key, shells) in running.items {
            let (known, row) = match answers.get(&n) {
                Some(Answer::Translated { text, source }) => {
                    let text = Arc::new(decode(text, &shells));
                    let row = serde_json::to_string(text.as_ref())
                        .ok()
                        .map(|content| row_of(&key, TranslationKind::Translated, content, source.clone()));
                    (Known::Text(text), row)
                }
                Some(Answer::Same { source }) => (
                    Known::AsWritten { until: None },
                    Some(row_of(&key, TranslationKind::Same, String::new(), source.clone())),
                ),
                None => (Known::AsWritten { until: None }, None),
            };
            rows.extend(row);
            self.settle(key, known);
        }
        if !rows.is_empty() {
            drop(self.core.keep_translations(rows));
        }
    }

    /// The tool failed on `keys`: they show as written, and the tool is left alone for [`PAUSE`].
    fn failed(&mut self, keys: Vec<Key>, cx: &mut Context<Self>) {
        let until = Instant::now() + PAUSE;
        self.paused_until = Some(until);
        self.sessions.clear();
        for key in keys {
            self.settle(key, Known::AsWritten { until: Some(until) });
        }
        self.drop_queue(Some(until), cx);
    }

    /// Everything queued shows as written, until `until`.
    fn drop_queue(&mut self, until: Option<Instant>, cx: &mut Context<Self>) {
        for (key, _) in std::mem::take(&mut self.queue) {
            self.settle(key, Known::AsWritten { until });
        }
        cx.notify();
    }

    /// Nothing left to translate: the tool processes close once idle for [`IDLE`].
    fn rest(&mut self, cx: &mut Context<Self>) {
        if self.sessions.is_empty() || self.idle.is_some() {
            return;
        }
        self.idle = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(IDLE).await;
            let _ = this.update(cx, |this, _| {
                this.idle = None;
                if this.running.is_none() && this.preparing.is_none() && this.queue.is_empty() {
                    this.sessions.clear();
                }
            });
        }));
    }
}

/// A message as the shared helpers read it.
fn source_of(message: &Message) -> Source {
    let text = Arc::new(crate::compose::rich_text_of(message));
    Source {
        conversation_id: message.conversation_id.clone(),
        id: message.id.clone(),
        version: message.version,
        client_message_id: message.client_message_id.clone(),
        from: String::new(),
        own: true,
        edit_time: message.edit_time,
        line: one_line(&text, CONTEXT_LINE),
        text,
    }
}

/// A message the chat view doesn't hold, read from the store, as a source: named as the engine names its sender.
pub(crate) async fn far_source(core: &dyn Core, message: &Message, names: &mut HashMap<String, String>) -> Source {
    Source {
        from: name_of(core, message, names).await,
        own: message.sender.as_deref() == Some(core.me()),
        ..source_of(message)
    }
}

/// A kept translation of `key`.
fn row_of(key: &Key, kind: TranslationKind, content: String, source_language: Option<String>) -> StoredTranslation {
    StoredTranslation {
        conversation_id: key.conversation.clone(),
        message_id: key.id.clone(),
        version: key.version,
        language: key.language.to_owned(),
        kind,
        content,
        source_language,
    }
}

/// What a batch is told of its chat ([`brief_of`], its senders among its members), its context (the messages before
/// its oldest, oldest first, at most [`CONTEXT_LINES`] and [`CONTEXT_CHARS`], and, labelled, each message one of its
/// messages replies to) and its items, each named by its sender and with what it replies to.
async fn prepare(
    core: &dyn Core,
    conversation: &str,
    language: &'static str,
    chosen: &[(Key, Message, String, Shells)],
) -> (Brief, Vec<Said>, Vec<Item>) {
    let mut brief = Arc::new(brief_of(core, conversation, language).await);
    let mut names: HashMap<String, String> = HashMap::new();
    let mut items = Vec::with_capacity(chosen.len());
    let mut labels: Vec<(String, String)> = Vec::new();
    for (n, (_, message, text, _)) in chosen.iter().enumerate() {
        let re = replied_to(&crate::compose::rich_text_of(message))
            .into_iter()
            .next()
            .map(|id| {
                if let Some(at) = chosen.iter().position(|(_, other, _, _)| other.id == id) {
                    return (at + 1).to_string();
                }
                if let Some((_, label)) = labels.iter().find(|(known, _)| *known == id) {
                    return label.clone();
                }
                let label = format!("q{}", labels.len() + 1);
                labels.push((id, label.clone()));
                label
            });
        items.push(Item {
            n: n + 1,
            key: key_of(&source_of(message)),
            from: name_of(core, message, &mut names).await,
            text: text.clone(),
            re,
        });
    }
    let Some((_, oldest, _, _)) = chosen.first() else {
        return (Arc::unwrap_or_clone(brief), Vec::new(), items);
    };
    let cursor = PageCursor {
        arrival_time: oldest.arrival_time,
        id: oldest.id.clone(),
    };
    let before = core
        .messages(conversation.to_owned(), Some(cursor), CONTEXT_LINES)
        .await
        .map(|page| page.messages)
        .unwrap_or_default();
    let mut context: Vec<(&Message, Said)> = Vec::new();
    let mut chars = 0;
    // the page is the newest first
    for message in before.iter().filter(|message| translatable(message)) {
        let said = said_of(core, message, &mut names).await;
        chars += said.text.len();
        if chars > CONTEXT_CHARS {
            break;
        }
        context.push((message, said));
    }
    context.reverse();
    let quoted = with_replied_to(core, conversation, labels, context, &mut items, &mut names).await;
    add_senders(
        &mut brief,
        items
            .iter()
            .map(|item| item.from.as_str())
            .chain(quoted.iter().map(|said| said.from.as_str())),
    );
    (Arc::unwrap_or_clone(brief), quoted, items)
}

/// `context` (oldest first, with their messages) with each message `labels` names labelled where it is, else read and
/// put first (it is older), told whole up to [`super::QUOTE_LINE`]; a reply whose message isn't held names nothing
/// (its preview is all there is).
async fn with_replied_to(
    core: &dyn Core,
    conversation: &str,
    labels: Vec<(String, String)>,
    mut context: Vec<(&Message, Said)>,
    items: &mut [Item],
    names: &mut HashMap<String, String>,
) -> Vec<Said> {
    let mut quoted = Vec::new();
    for (id, label) in labels {
        if let Some((message, said)) = context.iter_mut().find(|(held, _)| held.id == id) {
            said.label = Some(label);
            said.text = quote_line(&crate::compose::rich_text_of(message));
            continue;
        }
        let Some(message) = core
            .message(conversation.to_owned(), id)
            .await
            .ok()
            .flatten()
            .filter(translatable)
        else {
            // not held: its preview in the reply is all there is
            items
                .iter_mut()
                .filter(|item| item.re.as_ref() == Some(&label))
                .for_each(|item| item.re = None);
            continue;
        };
        let mut said = said_of(core, &message, names).await;
        said.label = Some(label);
        said.text = quote_line(&crate::compose::rich_text_of(&message));
        quoted.push(said);
    }
    quoted.extend(context.into_iter().map(|(_, said)| said));
    quoted
}

/// `message` as a context line.
async fn said_of(core: &dyn Core, message: &Message, names: &mut HashMap<String, String>) -> Said {
    let source = source_of(message);
    Said {
        key: key_of(&source),
        from: name_of(core, message, names).await,
        text: one_line(&source.text, CONTEXT_LINE),
        label: None,
    }
}

/// A sender's name as the engine has it, else as the message carried it.
async fn name_of(core: &dyn Core, message: &Message, names: &mut HashMap<String, String>) -> String {
    let Some(sender) = message.sender.clone() else {
        return SOMEONE.to_owned();
    };
    if let Some(name) = names.get(&sender) {
        return name.clone();
    }
    let name = core
        .person(sender.clone())
        .await
        .ok()
        .and_then(|person| person.name)
        .or_else(|| message.sender_name.clone())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| SOMEONE.to_owned());
    names.insert(sender, name.clone());
    name
}
