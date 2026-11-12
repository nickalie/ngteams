//! A chat read and written in a language of the user's choosing, translated on this computer by the AI command-line
//! tool the user has ([`crate::local_ai`]: Claude Code or the Codex CLI, the chat model [`Work::Chat`]). Teams' own
//! message translation needs Microsoft's service and works a message at a time; this one translates the whole chat,
//! both ways, with the chat as context.
//!
//! - **Per chat, off by default**: the chat's header has a language picker ([`header_button`]); the choice is kept per
//!   account and chat in the settings ([`crate::settings::Settings::chat_languages`]), so it holds across restarts.
//!   Nothing goes to a tool until a language is picked; the tool and its model are the General settings' Chat
//!   translation ([`crate::settings::Settings::chat_translator`]: Automatic, Claude Code when it is installed, else
//!   Codex; by default the fast model, as a send waits on it).
//! - **Reading**: every text message the view holds (history, new ones, edits, replies with their quotes) is shown in
//!   the language, as Teams shows a translated message: its translation, with Teams' translation mark beside its time
//!   ([`Mark`]: "See original message" shows it as written, and back). One already in the language is shown as it is,
//!   unmarked. Until its translation comes it shows as written, faded, "Translating…" beside its time; the view never waits.
//! - **Kept in the store** (`teams-store`'s schema 20): each translation by the message's id, version and the
//!   language, so reopening the chat, or a later run, shows it at once and asks the tool only for what is new, edited
//!   or never translated. The chat view reads them for the messages it holds before anything goes to the tool.
//! - **The chat as context, a batch at a time** ([`session`], [`prompt`]): the tool's process stays up for the chat and
//!   keeps the conversation; the messages without a translation go in batches of up to [`BATCH_ITEMS`] messages or
//!   [`BATCH_CHARS`] characters, the newest first (what the view shows), each with the messages before it (who said
//!   what, up to [`CONTEXT_LINES`] and [`CONTEXT_CHARS`]: the session gives a process each line once, and a running
//!   one only what is new to it) and the whole of each message one of them replies to (up to [`QUOTE_LINE`]),
//!   wherever it is in the chat: read from the store when the view hasn't loaded it ([`window`]). Formatting, mentions, links, emoji and code are kept as markers the tool must keep ([`codec`]): the
//!   translation is put back together as the message's own rich text, drawn only through the sanitized [`RichText`].
//! - **What the tool is told of the chat** ([`brief`]): its title, who is who (the account is the member translated
//!   for, the others by name), and its glossary: the renderings the tool chose for the chat's terms, kept in the store
//!   per chat and language and grown from each batch's answer, so a new process, or a later run, keeps them. Gender
//!   and forms of address are the tool's to read from the chat itself; nothing here guesses them.
//! - **Sending** ([`ChatTranslation::outgoing`]): the user writes in the chat's chosen language; the message (a post,
//!   a reply, an edit) is translated into the chat's language, the one the others write in (the source the tool named
//!   for their messages, the most frequent; else the tool judges it from the context), before it goes to the outbox,
//!   the one write path. The user keeps seeing it as written: kept in memory at once and in the store by its
//!   `clientmessageid` (or `edit:<id>`), and bound to the message as its translation once it is stored. A send whose
//!   translation fails sends nothing: the box keeps it and says why, with Try again and Send without translating.
//! - **Progress**: from the moment a language is picked until every message the view holds has its translation, the
//!   header's button spins and says how far it has come ([`ChatTranslation::progress`]), and each message still
//!   waiting says "Translating…" in place ([`Mark::Waiting`]). Translations show a message at a time as the tool writes
//!   them ([`session::Outcome::Partial`]), the newest batch first and, within a batch, the newest message first (the
//!   end of the chat the view shows fills in first). Turning translation off (or picking another language) mid-run
//!   stops the tool at once and drops what it was still writing.
//! - **Failures** stop the reading ([`Status::Failed`]: the tool missing, signed out, too slow, or stopped): the chat
//!   shows a notice saying why, with Try again, and its messages as written.
//! - **Outside the view** ([`Previews`]): the chat list's preview, a new message's notification and the search's cards
//!   show a translated chat's messages in its language too, through the same store, tool and prompts; the view tells it
//!   what it translates, so nothing is asked twice. A reply's quote shows in the box as the chat shows the message.
//! - **The picker** ([`header_button`], [`picker`]): the header's menu of languages, with a search field that filters it.
//! - **Private**: no message, translation or tool output is logged or traced.

pub mod brief;
pub mod codec;
pub mod picker;
pub mod previews;
pub mod prompt;
pub mod session;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{
    AnyElement, App, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Subscription, Task, div, px, rgb,
};
use gpui_component::button::Button;
use teams_proto::chat::{MessageType, RichText, parse_html};
use teams_store::{Message, SentOriginal, StoredTranslation, TranslationKind};

pub use picker::header_button;
pub use previews::{Lookup, Previews};
pub use session::{Limits, Outcome, Session, Spec};

use crate::bridge::Core;
use crate::icons::{Glyph, selected_by};
use crate::local_ai::{AiChoice, Backend, Failure, Tools};
use crate::settings::SettingsModel;
use crate::theme::{self, Palette};
use crate::translate::{TARGET_LANGUAGES, language, same_language};
use codec::{Shells, decode, encode, one_line};
use prompt::{Answer, Brief, Item, Said, Term};

/// The most messages a batch holds.
pub const BATCH_ITEMS: usize = 12;
/// The most characters of messages a batch holds (a longer message goes alone).
pub const BATCH_CHARS: usize = 6000;
/// The most messages before a batch given as its context, and their most characters: what a new tool process is
/// given; a running one is given only what is new to it, at most [`session::WARM_LINES`].
pub const CONTEXT_LINES: usize = 40;
pub const CONTEXT_CHARS: usize = 8000;
/// The longest a context line is.
const CONTEXT_LINE: usize = 400;
/// The longest a message replied to is, as a labelled context line (`[q1]`): what the reply answers, told whole
/// within reason.
const QUOTE_LINE: usize = 2000;
/// The most lines and characters of the chat's latest messages a draft's correction is given
/// ([`ChatTranslation::recent_context`]): it starts a process anew for each draft, so it is given less than a batch.
pub const CORRECTION_LINES: usize = 15;
pub const CORRECTION_CHARS: usize = 3000;
/// How many times a message the tool's answer left out is asked again before it shows as written for good (one held
/// back as an echo isn't counted: the session takes its next echo).
const ATTEMPTS: u8 = 2;
/// The longest a message's text sent to the tool may be (its encoded text); a longer one shows as written.
pub const MAX_MESSAGE: usize = 24_000;

/// Where a chat's translation stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// No language picked.
    Off,
    /// Translating into the language (the tool starts with the first message to translate).
    On,
    /// The tool failed: the messages show as written until Try again.
    Failed(Failure),
}

/// A message's translation as known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Its translation, and the language it was written in.
    Translated {
        version: i64,
        text: Arc<RichText>,
        source: Option<String>,
    },
    /// It was in the language already.
    Same { version: i64, source: Option<String> },
    /// The account's own message as it was written, before it was sent translated.
    Original { version: i64, text: Arc<RichText> },
    /// The tool's answers left it out: it shows as written.
    Failed { version: i64 },
}

impl Entry {
    fn version(&self) -> i64 {
        match self {
            Self::Translated { version, .. }
            | Self::Same { version, .. }
            | Self::Original { version, .. }
            | Self::Failed { version } => *version,
        }
    }
}

/// How a message's translation is marked beside its time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Shown translated (or, for the account's own, as written): its tip offers the message as it was sent.
    Translated { tip: SharedString },
    /// Shown as sent, after "See original message": its tip offers the translation back.
    Original { tip: SharedString },
    /// Its translation is on its way.
    Waiting,
    /// It couldn't be translated.
    Failed,
}

/// A held text message, as the translation reads it.
#[derive(Clone, Debug)]
pub struct Source {
    pub conversation_id: String,
    pub id: String,
    pub version: i64,
    pub client_message_id: Option<String>,
    /// The sender's name, as the view shows it.
    pub from: String,
    pub own: bool,
    pub edit_time: Option<i64>,
    pub text: Arc<RichText>,
    /// Its plain text on one line: context, and what an original is matched by.
    pub line: String,
}

/// The account's own message as written, sent translated in this run: matched to its message by the text sent.
#[derive(Clone, Debug)]
struct Sent {
    language: &'static str,
    line: String,
    original: Arc<RichText>,
}

/// What a view draws of the chat's translation, taken at each rebuild.
#[derive(Clone, Debug, Default)]
pub struct Shown {
    pub language: Option<&'static str>,
    entries: Arc<HashMap<String, Entry>>,
    originals: Arc<HashSet<String>>,
    sent: Arc<Vec<Sent>>,
    failed: bool,
}

impl Shown {
    /// The text `message` shows instead of its own, when it shows translated.
    #[must_use]
    pub fn text_of(&self, message: &Message) -> Option<Arc<RichText>> {
        self.language?;
        if self.originals.contains(&message.id) {
            return None;
        }
        match self.entries.get(&message.id) {
            Some(Entry::Translated { version, text, .. } | Entry::Original { version, text })
                if *version == message.version =>
            {
                Some(Arc::clone(text))
            }
            _ => None,
        }
    }

    /// How `message` is marked (`None`: not at all), when it is a text message the view shows.
    #[must_use]
    pub fn mark_of(&self, message: &Message) -> Option<Mark> {
        let target = self.language.and_then(language).map(|(_, name)| name)?;
        let entry = self
            .entries
            .get(&message.id)
            .filter(|entry| entry.version() == message.version);
        let seeing_original = self.originals.contains(&message.id);
        match entry {
            Some(Entry::Translated { source, .. }) if seeing_original => Some(Mark::Original {
                tip: format!("See translation ({target})").into(),
            }),
            Some(Entry::Translated { source, .. }) => Some(Mark::Translated {
                tip: source
                    .as_deref()
                    .and_then(language_name)
                    .map_or_else(
                        || "See original message".to_owned(),
                        |name| format!("See original message ({name})"),
                    )
                    .into(),
            }),
            Some(Entry::Original { .. }) if seeing_original => Some(Mark::Original {
                tip: format!("See what you wrote ({target})").into(),
            }),
            Some(Entry::Original { .. }) => Some(Mark::Translated {
                tip: "See the message as sent".into(),
            }),
            Some(Entry::Same { .. }) => None,
            Some(Entry::Failed { .. }) => Some(Mark::Failed),
            None if self.failed => None,
            None => Some(Mark::Waiting),
        }
    }

    /// The account's own message as written for `sent` (a post's text as it goes out), sent translated in this run.
    #[must_use]
    pub fn original_of_sent(&self, sent: &RichText) -> Option<Arc<RichText>> {
        self.language?;
        let line = one_line(sent, usize::MAX);
        self.sent
            .iter()
            .rev()
            .find(|kept| kept.line == line)
            .map(|kept| Arc::clone(&kept.original))
    }
}

/// The English name of a language code a tool named (`en`, `pt-BR`), when it is one of [`TARGET_LANGUAGES`].
#[must_use]
pub fn language_name(code: &str) -> Option<&'static str> {
    TARGET_LANGUAGES
        .iter()
        .find(|(known, _)| same_language(known, code) || *known == code)
        .map(|(_, name)| *name)
}

/// Why an outgoing message wasn't translated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendFailure {
    /// The tool failed.
    Tool(Failure),
    /// Nobody else wrote in the chat yet, so its language isn't known.
    Unknown,
}

impl SendFailure {
    /// What the compose box says.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Tool(failure) => format!("Your message wasn't translated. {}", failure.message()),
            Self::Unknown => {
                "Your message wasn't translated: nobody else has written here yet, so the chat's language isn't known."
                    .to_owned()
            }
        }
    }
}

/// What a translated send gives back: the text to send, or `None` to send it as written (it is in the chat's language).
pub type Outgoing = Result<Option<RichText>, SendFailure>;

/// A message of a batch the tool is working on: its number, id, version and markers.
type RunningItem = (usize, String, i64, Shells);

/// A batch the tool is working on: its messages, and the answers applied so far by their numbers (a later answer that
/// differs replaces one: the batch's whole answer wins over what streamed before it).
struct Running {
    id: u64,
    items: Vec<RunningItem>,
    applied: HashMap<usize, Answer>,
}

/// How far reading a chat in its language has come: of the text messages the view holds, how many show their
/// translation (or are known to need none, or couldn't be translated).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// One chat's translation (see the module docs): a GPUI entity its view feeds with the messages it holds.
pub struct ChatTranslation {
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    /// The chat as the settings name it: where its messages are stored (a channel's post's thread is its channel's).
    chat: String,
    language: Option<&'static str>,
    choice: AiChoice,
    status: Status,
    limits: Limits,
    sources: Vec<Source>,
    /// Parsed texts by message id, with their version.
    parsed: HashMap<String, (i64, Arc<RichText>)>,
    entries: Arc<HashMap<String, Entry>>,
    /// The messages (id and version) the store was asked about in this language.
    asked: HashSet<(String, i64)>,
    /// How many times each message was left out of an answer.
    attempts: HashMap<(String, i64), u8>,
    originals: Arc<HashSet<String>>,
    sent: Arc<Vec<Sent>>,
    session: Option<Session>,
    /// What the tool is told of the chat, once read ([`brief::brief_of`]): no batch goes to the tool before; an
    /// outgoing message sent before it is read goes with the chat alone.
    brief: Option<Arc<Brief>>,
    briefing: Option<Task<()>>,
    /// Messages replied to that the view doesn't hold, read from the store, by id (`None`: the store hasn't it).
    quoted: HashMap<String, Option<Source>>,
    /// Reading them: no batch goes to the tool meanwhile.
    quoting: Option<Task<()>>,
    running: Option<Running>,
    outgoing: HashMap<u64, (Shells, async_channel::Sender<Outgoing>)>,
    next_id: u64,
    reading: Option<Task<()>>,
    read_again: bool,
    receiving: Option<Task<()>>,
    /// Batches asked of the tool so far (tests count them).
    batches: u64,
    /// The arrival of the newest message the view holds.
    newest: i64,
    /// The header's language menu, once drawn.
    picker: Option<Entity<picker::LanguagePicker>>,
    /// The account's translations outside the open views: told what this one translates and knows.
    previews: Entity<Previews>,
    _settings: Subscription,
}

impl ChatTranslation {
    /// The translation of the view of `conversation`, read from the settings: a channel and its posts' threads share
    /// their channel's.
    pub fn new(
        core: Arc<dyn Core>,
        settings: Entity<SettingsModel>,
        conversation: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let chat = teams_proto::stored_conversation(conversation).to_owned();
        let observing = cx.observe(&settings, |this: &mut Self, _, cx| this.reconcile(cx));
        let previews = Previews::of(&core, &settings, cx);
        let view = cx.entity_id();
        cx.on_release(move |this, cx| {
            this.previews
                .update(cx, |previews, cx| previews.from_view(view, None, Vec::new(), None, cx));
        })
        .detach();
        let mut this = Self {
            core,
            settings,
            chat,
            language: None,
            choice: AiChoice::default(),
            status: Status::Off,
            limits: Limits::default(),
            sources: Vec::new(),
            parsed: HashMap::new(),
            entries: Arc::default(),
            asked: HashSet::new(),
            attempts: HashMap::new(),
            originals: Arc::default(),
            sent: Arc::default(),
            session: None,
            brief: None,
            briefing: None,
            quoted: HashMap::new(),
            quoting: None,
            running: None,
            outgoing: HashMap::new(),
            next_id: 1,
            reading: None,
            read_again: false,
            receiving: None,
            batches: 0,
            newest: 0,
            picker: None,
            previews,
            _settings: observing,
        };
        this.reconcile(cx);
        this
    }

    /// With these limits (tests).
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The language picked, by its code.
    #[must_use]
    pub fn language(&self) -> Option<&'static str> {
        self.language
    }

    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// Batches asked of the tool so far.
    #[must_use]
    pub fn batches(&self) -> u64 {
        self.batches
    }

    /// Whether the chat's messages are being translated now: a language is picked, nothing failed, and a message the
    /// view holds still waits for its translation. The header's button shows it from the moment a language is picked.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.progress().is_some_and(|progress| progress.done < progress.total)
    }

    /// How far the reading has come, while the chat is translated (`None`: off or failed).
    #[must_use]
    pub fn progress(&self) -> Option<Progress> {
        if self.status != Status::On || self.language.is_none() {
            return None;
        }
        let done = self
            .sources
            .iter()
            .filter(|source| self.entry_of(source).is_some())
            .count();
        Some(Progress {
            done,
            total: self.sources.len(),
        })
    }

    /// The header's language menu, once drawn ([`picker::picker_of`]).
    #[must_use]
    pub fn picker(&self) -> Option<&Entity<picker::LanguagePicker>> {
        self.picker.as_ref()
    }

    fn keep_picker(&mut self, picker: Entity<picker::LanguagePicker>) {
        self.picker = Some(picker);
    }

    /// What the view draws.
    #[must_use]
    pub fn shown(&self) -> Shown {
        Shown {
            language: self.language,
            entries: Arc::clone(&self.entries),
            originals: Arc::clone(&self.originals),
            sent: Arc::clone(&self.sent),
            failed: matches!(self.status, Status::Failed(_)),
        }
    }

    /// Picks the language the chat is read in (`None`: as written), kept in the settings.
    pub fn set_language(&mut self, language: Option<&'static str>, cx: &mut Context<Self>) {
        let (account, chat) = (self.core.me().to_owned(), self.chat.clone());
        self.settings.update(cx, |model, cx| {
            model.change(|settings| settings.set_chat_language(&account, &chat, language), cx);
        });
        // a model kept in memory only still follows it
        self.reconcile(cx);
    }

    /// Shows `message` as written, or translated again.
    pub fn toggle_original(&mut self, message: &str, cx: &mut Context<Self>) {
        let originals = Arc::make_mut(&mut self.originals);
        if !originals.remove(message) {
            originals.insert(message.to_owned());
        }
        cx.notify();
    }

    /// Try again after a failure: the tool started anew.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, Status::Failed(_)) {
            self.stop_session();
            self.status = Status::On;
            self.attempts.clear();
            self.pump(cx);
            self.share(cx);
            cx.notify();
        }
    }

    /// The text messages the view holds now (others are left out), `name_of` naming their senders as it shows them.
    pub fn follow(&mut self, messages: &[&Message], name_of: &dyn Fn(&Message) -> String, cx: &mut Context<Self>) {
        let me = self.core.me().to_owned();
        let sources: Vec<Source> = messages
            .iter()
            .filter(|message| translatable(message))
            .copied()
            .map(|message| {
                let text = self.parsed_text(message);
                Source {
                    conversation_id: message.conversation_id.clone(),
                    id: message.id.clone(),
                    version: message.version,
                    client_message_id: message.client_message_id.clone(),
                    from: name_of(message),
                    own: message.sender.as_deref() == Some(me.as_str()),
                    edit_time: message.edit_time,
                    line: one_line(&text, CONTEXT_LINE),
                    text,
                }
            })
            .collect();
        self.newest = messages
            .iter()
            .map(|message| message.arrival_time)
            .max()
            .unwrap_or_default();
        let held: HashSet<&str> = sources.iter().map(|source| source.id.as_str()).collect();
        self.parsed.retain(|id, _| held.contains(id.as_str()));
        self.sources = sources;
        if self.language.is_some() {
            self.read_store(cx);
        }
        self.share(cx);
    }

    /// Tells the account's [`Previews`] what this view translates (the messages it holds, while it translates) and what
    /// it knows of them, so they aren't asked of the tool twice and show translated outside the view at once.
    fn share(&self, cx: &mut Context<Self>) {
        let view = cx.entity_id();
        // while it holds nothing yet (loading), all of its chat's
        let followed = self
            .language
            .filter(|_| self.status == Status::On)
            .map(|language| previews::Followed {
                conversation: self
                    .sources
                    .first()
                    .map_or_else(|| self.chat.clone(), |first| first.conversation_id.clone()),
                language,
                held: self.sources.iter().map(|source| source.id.clone()).collect(),
                newest: self.newest,
            });
        let learned = self
            .sources
            .iter()
            .filter_map(|source| {
                let entry = self.entry_of(source)?;
                Some((source.conversation_id.clone(), source.id.clone(), entry.clone()))
            })
            .collect();
        let language = self.language;
        self.previews.update(cx, |previews, cx| {
            previews.from_view(view, followed, learned, language, cx);
        });
    }

    fn parsed_text(&mut self, message: &Message) -> Arc<RichText> {
        if let Some((version, text)) = self.parsed.get(&message.id)
            && *version == message.version
        {
            return Arc::clone(text);
        }
        let text = Arc::new(if message.message_type == MessageType::RichTextHtml.as_str() {
            parse_html(&message.content)
        } else {
            RichText::plain(&message.content)
        });
        self.parsed
            .insert(message.id.clone(), (message.version, Arc::clone(&text)));
        text
    }

    /// Follows the settings: a new language or tool starts over.
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.read(cx).settings();
        let language = settings.chat_language_of(self.core.me(), &self.chat);
        let choice = settings.chat_translator.clone();
        if language == self.language && choice == self.choice {
            return;
        }
        self.language = language;
        self.choice = choice;
        self.stop_session();
        self.reading = None;
        self.read_again = false;
        self.entries = Arc::default();
        self.asked.clear();
        self.attempts.clear();
        self.originals = Arc::default();
        self.brief = None;
        self.briefing = None;
        self.quoted.clear();
        self.quoting = None;
        self.status = if language.is_some() { Status::On } else { Status::Off };
        if let Some(language) = language {
            self.read_brief(language, cx);
            self.read_store(cx);
        }
        self.share(cx);
        cx.notify();
    }

    /// Reads what the tool is told of the chat, then hands the tool what waits.
    fn read_brief(&mut self, language: &'static str, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let chat = self.chat.clone();
        self.briefing = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let read = brief::brief_of(&*core, &chat, language).await;
            let _ = this.update(cx, |this, cx| {
                if this.language == Some(language) {
                    this.briefing = None;
                    this.brief = Some(Arc::new(read));
                    this.pump(cx);
                }
            });
        }));
    }

    /// What the tool is told of the chat now: the brief read, with the senders of the messages at `around` among its
    /// members (a large chat's); the chat alone until it is read.
    fn brief_with(&mut self, around: &[usize]) -> Arc<Brief> {
        let chat = self.chat.clone();
        let brief = self.brief.get_or_insert_with(|| {
            Arc::new(Brief {
                conversation: chat,
                ..Brief::default()
            })
        });
        brief::add_senders(
            brief,
            around
                .iter()
                .filter_map(|at| self.sources.get(*at))
                .map(|source| source.from.as_str()),
        );
        Arc::clone(brief)
    }

    /// Stops the tool's session: a send waiting on it is told it stopped.
    fn stop_session(&mut self) {
        self.session = None;
        self.receiving = None;
        self.running = None;
        for (_, (_, answer)) in self.outgoing.drain() {
            let _ = answer.try_send(Err(SendFailure::Tool(Failure::Stopped(Backend::Claude, None))));
        }
    }

    /// Reads from the store what is kept of the messages it wasn't asked about yet (their translations, and the
    /// account's own messages as written), then hands the tool what is left.
    fn read_store(&mut self, cx: &mut Context<Self>) {
        let Some(language) = self.language else { return };
        if self.reading.is_some() {
            self.read_again = true;
            return;
        }
        let wanted: Vec<Source> = self
            .sources
            .iter()
            .filter(|source| {
                self.entry_of(source).is_none() && !self.asked.contains(&(source.id.clone(), source.version))
            })
            .cloned()
            .collect();
        if wanted.is_empty() {
            self.pump(cx);
            return;
        }
        let core = Arc::clone(&self.core);
        let Some(conversation) = wanted.first().map(|source| source.conversation_id.clone()) else {
            return;
        };
        self.reading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let ids = wanted.iter().map(|source| source.id.clone()).collect();
            let keys = wanted
                .iter()
                .filter(|source| source.own)
                .flat_map(|source| {
                    source
                        .client_message_id
                        .clone()
                        .into_iter()
                        .chain([SentOriginal::edit_key(&source.id)])
                })
                .collect::<Vec<_>>();
            let kept = core
                .translations(conversation.clone(), language.to_owned(), ids)
                .await
                .unwrap_or_default();
            let originals = if keys.is_empty() {
                Vec::new()
            } else {
                core.sent_originals(conversation, keys).await.unwrap_or_default()
            };
            let _ = this.update(cx, |this, cx| this.read(language, &wanted, &kept, &originals, cx));
        }));
    }

    /// What the store had for `wanted`.
    fn read(
        &mut self,
        language: &'static str,
        wanted: &[Source],
        kept: &[StoredTranslation],
        originals: &[SentOriginal],
        cx: &mut Context<Self>,
    ) {
        self.reading = None;
        if self.language != Some(language) {
            return;
        }
        let mut bound = Vec::new();
        {
            let entries = Arc::make_mut(&mut self.entries);
            for source in wanted {
                self.asked.insert((source.id.clone(), source.version));
                let stored = kept
                    .iter()
                    .find(|row| row.message_id == source.id && row.version == source.version)
                    .and_then(entry_of_row);
                let entry = stored.or_else(|| {
                    let original = source
                        .own
                        .then(|| stored_original(source, language, originals))
                        .flatten()?;
                    bound.push(original_row(source, language, &original));
                    Some(Entry::Original {
                        version: source.version,
                        text: original,
                    })
                });
                if let Some(entry) = entry {
                    entries.insert(source.id.clone(), entry);
                }
            }
        }
        if !bound.is_empty() {
            self.keep(bound);
        }
        if std::mem::take(&mut self.read_again) {
            self.read_store(cx);
        } else {
            self.pump(cx);
        }
        self.share(cx);
        cx.notify();
    }

    /// The entry `source` has for its version, if any.
    fn entry_of(&self, source: &Source) -> Option<&Entry> {
        self.entries
            .get(&source.id)
            .filter(|entry| entry.version() == source.version)
    }

    /// Hands the tool the next batch, when none runs and there is one: the newest messages the store had nothing for.
    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.running.is_some()
            || self.reading.is_some()
            || self.briefing.is_some()
            || self.quoting.is_some()
            || self.status != Status::On
        {
            return;
        }
        let Some(language) = self.language else { return };
        self.bind_sent(language);
        let due: Vec<usize> = (0..self.sources.len())
            .rev()
            .filter(|at| {
                let source = &self.sources[*at];
                self.entry_of(source).is_none() && self.asked.contains(&(source.id.clone(), source.version))
            })
            .collect();
        if due.is_empty() {
            return;
        }
        let mut chosen = Vec::new();
        let mut chars = 0;
        let mut too_long = Vec::new();
        for at in due {
            let (text, shells) = encode(&self.sources[at].text);
            if text.len() > MAX_MESSAGE {
                too_long.push(at);
                continue;
            }
            if !chosen.is_empty() && (chosen.len() >= BATCH_ITEMS || chars + text.len() > BATCH_CHARS) {
                break;
            }
            chars += text.len();
            chosen.push((at, text, shells));
        }
        for at in too_long {
            let source = &self.sources[at];
            Arc::make_mut(&mut self.entries).insert(
                source.id.clone(),
                Entry::Failed {
                    version: source.version,
                },
            );
        }
        if chosen.is_empty() {
            return cx.notify();
        }
        chosen.reverse();
        let unread = self.unread_quotes(chosen.iter().map(|(at, _, _)| *at));
        if !unread.is_empty() {
            return self.read_quotes(unread, cx);
        }
        if let Err(failure) = self.ensure_session(language, cx) {
            self.status = Status::Failed(failure);
            return cx.notify();
        }
        let (id, job, running) = self.batch_of(chosen);
        if let Some(session) = &self.session {
            session.send(job);
            self.batches += 1;
            self.running = Some(Running {
                id,
                items: running,
                applied: HashMap::new(),
            });
        }
    }

    /// The messages the sources at `picked` reply to that neither the view holds nor the store was asked for.
    fn unread_quotes(&self, picked: impl Iterator<Item = usize>) -> Vec<String> {
        let mut unread: Vec<String> = picked
            .flat_map(|at| brief::replied_to(&self.sources[at].text))
            .filter(|id| !self.quoted.contains_key(id) && !self.sources.iter().any(|source| source.id == *id))
            .collect();
        unread.sort_unstable();
        unread.dedup();
        unread
    }

    /// Reads the messages `ids` from the store (replied to, older than the view holds), then hands the tool what
    /// waits: the reply's whole question goes along, not only the preview the reply holds.
    fn read_quotes(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let chat = self.chat.clone();
        self.quoting = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut names = HashMap::new();
            let mut read = Vec::with_capacity(ids.len());
            for id in ids {
                let message = core
                    .message(chat.clone(), id.clone())
                    .await
                    .ok()
                    .flatten()
                    .filter(translatable);
                let source = match message {
                    Some(message) => Some(previews::far_source(&*core, &message, &mut names).await),
                    None => None,
                };
                read.push((id, source));
            }
            let _ = this.update(cx, |this, cx| {
                if this.quoting.is_some() {
                    this.quoting = None;
                    this.quoted.extend(read);
                    this.pump(cx);
                }
            });
        }));
    }

    /// The job asking about the messages `chosen` (their places among the sources, oldest first, with their encoded
    /// texts and markers), numbered anew: what the tool is told of the chat, their context and each one's item; and
    /// how their answers are read back, and its number.
    fn batch_of(&mut self, chosen: Vec<(usize, String, Shells)>) -> (u64, session::Job, Vec<RunningItem>) {
        let picked: Vec<usize> = chosen.iter().map(|(at, _, _)| *at).collect();
        let last = chosen.last().map_or(0, |(at, _, _)| *at);
        let replies: Vec<Vec<String>> = picked
            .iter()
            .map(|at| brief::replied_to(&self.sources[*at].text))
            .collect();
        let limits = (CONTEXT_LINES, CONTEXT_CHARS);
        let (context, re) = window(&self.sources, &self.quoted, &picked, &replies, last, limits);
        let around: Vec<usize> = picked
            .iter()
            .copied()
            .chain(context.iter().filter_map(|said| self.at_key(&said.key)))
            .collect();
        let brief = self.brief_with(&around);
        let id = self.next_job_id();
        let mut items = Vec::new();
        let mut running = Vec::new();
        for (n, ((at, text, shells), re)) in chosen.into_iter().zip(re).enumerate() {
            let source = &self.sources[at];
            items.push(Item {
                n: n + 1,
                key: key_of(source),
                from: source.from.clone(),
                text,
                re,
            });
            running.push((n + 1, source.id.clone(), source.version, shells));
        }
        let job = session::Job::Batch {
            id,
            brief,
            context,
            items,
        };
        (id, job, running)
    }

    /// Where the message a context line's `key` names is among the sources.
    fn at_key(&self, key: &str) -> Option<usize> {
        self.sources.iter().position(|source| key_of(source) == key)
    }

    /// The chat's latest messages, oldest first, as context for something new written in it: at most
    /// [`CORRECTION_LINES`] and [`CORRECTION_CHARS`].
    #[must_use]
    pub fn recent_context(&self) -> Vec<Said> {
        let limits = (CORRECTION_LINES, CORRECTION_CHARS);
        window(&self.sources, &HashMap::new(), &[], &[], self.sources.len(), limits).0
    }

    fn next_job_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The session, started when there is none yet.
    fn ensure_session(&mut self, language: &'static str, cx: &mut Context<Self>) -> Result<(), Failure> {
        if self.session.is_some() {
            return Ok(());
        }
        let (backend, program) = Tools::of(cx).pick(self.choice.tool)?;
        let name = crate::translate::language(language).map_or("English", |(_, name)| name);
        let session = Session::start(Spec {
            backend,
            program,
            model: self.choice.model.clone(),
            language: name,
            limits: self.limits,
        });
        let outcomes = session.outcomes();
        self.receiving = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            while let Ok(outcome) = outcomes.recv().await {
                if this.update(cx, |this, cx| this.received(outcome, cx)).is_err() {
                    break;
                }
            }
        }));
        self.session = Some(session);
        Ok(())
    }

    fn received(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        match outcome {
            Outcome::Partial { id, answers } => {
                let Some(mut running) = self.running.take_if(|running| running.id == id) else {
                    return;
                };
                self.answered(&mut running, &answers);
                self.running = Some(running);
            }
            Outcome::Batch {
                id,
                answers,
                terms,
                held,
            } => {
                let Some(mut running) = self.running.take_if(|running| running.id == id) else {
                    return;
                };
                match answers {
                    Ok(answers) => {
                        self.answered(&mut running, &answers);
                        self.left_out(&running, &held);
                        self.learn_terms(&terms);
                    }
                    Err(failure) => self.status = Status::Failed(failure),
                }
                self.pump(cx);
            }
            Outcome::Outgoing { id, answer } => {
                if let Some((shells, reply)) = self.outgoing.remove(&id) {
                    let _ = reply.try_send(outgoing_of(answer, &shells));
                }
            }
        }
        self.share(cx);
        cx.notify();
    }

    /// Answers of the batch `running` (so far, or its whole answer): each one new, or differing from the one applied,
    /// kept in memory and in the store.
    fn answered(&mut self, running: &mut Running, answers: &HashMap<usize, Answer>) {
        let mut kept = Vec::new();
        for (n, id, version, shells) in &running.items {
            let Some(answer) = answers.get(n).filter(|answer| running.applied.get(n) != Some(*answer)) else {
                continue;
            };
            let entry = match answer {
                Answer::Translated { text, source } => Entry::Translated {
                    version: *version,
                    text: Arc::new(decode(text, shells)),
                    source: source.clone(),
                },
                Answer::Same { source } => Entry::Same {
                    version: *version,
                    source: source.clone(),
                },
            };
            running.applied.insert(*n, answer.clone());
            kept.extend(self.learn(id, entry));
        }
        if !kept.is_empty() {
            self.keep(kept);
        }
    }

    /// Terms a batch's answer added to the chat's glossary: told from now on, and kept in the store.
    fn learn_terms(&mut self, terms: &[Term]) {
        let (Some(language), Some(brief)) = (self.language, self.brief.as_mut()) else {
            return;
        };
        if terms.is_empty() {
            return;
        }
        brief::learn(brief, terms);
        drop(self.core.keep_glossary(brief::rows(&self.chat, language, terms)));
    }

    /// The batch `running` is done: a message its answer left out is asked again, up to [`ATTEMPTS`], then marked as
    /// one that couldn't be translated. One `held` back as an echo isn't counted: asked again, its next echo is taken.
    fn left_out(&mut self, running: &Running, held: &[usize]) {
        for (n, id, version, _) in &running.items {
            if running.applied.contains_key(n) || held.contains(n) {
                continue;
            }
            let tries = self.attempts.entry((id.clone(), *version)).or_default();
            *tries += 1;
            if *tries >= ATTEMPTS {
                self.learn(id, Entry::Failed { version: *version });
            }
        }
    }

    /// `entry` for the message `id`, when the view still holds it at the entry's version: kept in memory, and given back
    /// as the store keeps it. An answer for a version since edited is dropped, never taken for the edit's: the edit is
    /// asked for once the batch is done.
    fn learn(&mut self, id: &str, entry: Entry) -> Option<StoredTranslation> {
        let language = self.language?;
        let source = self
            .sources
            .iter()
            .find(|source| source.id == id && source.version == entry.version())?;
        let row = row_of(source, language, &entry);
        Arc::make_mut(&mut self.entries).insert(id.to_owned(), entry);
        row
    }

    /// Keeps `rows` in the store (nothing waits on it).
    fn keep(&self, rows: Vec<StoredTranslation>) {
        drop(self.core.keep_translations(rows));
    }

    /// The account's own messages sent translated in this run, bound to their messages once they are held: by the
    /// text that was sent.
    fn bind_sent(&mut self, language: &'static str) {
        if self.sent.is_empty() {
            return;
        }
        let mut bound = Vec::new();
        for source in self.sources.iter().filter(|source| source.own) {
            if self.entry_of(source).is_some() {
                continue;
            }
            let Some(sent) = self
                .sent
                .iter()
                .rev()
                .find(|sent| sent.language == language && sent.line == one_line(&source.text, usize::MAX))
            else {
                continue;
            };
            bound.push((source.clone(), Arc::clone(&sent.original)));
        }
        if bound.is_empty() {
            return;
        }
        let rows = bound
            .iter()
            .map(|(source, original)| original_row(source, language, original))
            .collect();
        let entries = Arc::make_mut(&mut self.entries);
        for (source, original) in bound {
            entries.insert(
                source.id.clone(),
                Entry::Original {
                    version: source.version,
                    text: original,
                },
            );
        }
        self.keep(rows);
    }

    /// The chat's language: the one the others' messages were written in, the most frequent (`None` when none is
    /// known).
    #[must_use]
    pub fn chat_language(&self) -> Option<String> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for source in self.sources.iter().filter(|source| !source.own) {
            let found = match self.entry_of(source) {
                Some(Entry::Translated { source: Some(code), .. } | Entry::Same { source: Some(code), .. }) => {
                    code.as_str()
                }
                Some(Entry::Same { source: None, .. }) => self.language.unwrap_or_default(),
                _ => continue,
            };
            *counts.entry(found).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(code, _)| code.to_owned())
    }

    /// The account's own message `text` (written in the chat's chosen language) as it is to be sent: translated into
    /// the chat's language, or `None` to send it as written (it is in that language already). `None` for the whole
    /// task when the chat isn't translated: the message goes as written.
    pub fn outgoing(&mut self, text: &RichText, cx: &mut Context<Self>) -> Option<Task<Outgoing>> {
        let language = self.language?;
        let target = self.chat_language();
        if target.as_deref().is_some_and(|target| same_language(language, target)) {
            return Some(Task::ready(Ok(None)));
        }
        let replies = vec![brief::replied_to(text)];
        let limits = (CONTEXT_LINES, CONTEXT_CHARS);
        let (context, mut re) = window(&self.sources, &self.quoted, &[], &replies, self.sources.len(), limits);
        if target.is_none() && !self.sources.iter().any(|source| !source.own) {
            return Some(Task::ready(Err(SendFailure::Unknown)));
        }
        if let Err(failure) = self.ensure_session(language, cx) {
            return Some(Task::ready(Err(SendFailure::Tool(failure))));
        }
        let (encoded, shells) = encode(text);
        let around: Vec<usize> = context.iter().filter_map(|said| self.at_key(&said.key)).collect();
        let brief = self.brief_with(&around);
        let id = self.next_job_id();
        let (reply, answer) = async_channel::bounded(1);
        self.outgoing.insert(id, (shells, reply));
        if let Some(session) = &self.session {
            session.send(session::Job::Outgoing {
                id,
                brief,
                context,
                to: target,
                re: re.pop().flatten(),
                text: encoded,
            });
        }
        Some(cx.background_executor().spawn(async move {
            answer
                .recv()
                .await
                .unwrap_or(Err(SendFailure::Tool(Failure::Stopped(Backend::Claude, None))))
        }))
    }

    /// The account's own message `original` was sent as `sent` (its translation): shown as written from now on. `key`
    /// is how the store keeps it ([`SentOriginal::key`]) when it is known now (an edit's); a post's comes with its row
    /// ([`Self::sent_as`]).
    pub fn sent(&mut self, original: &RichText, sent: &RichText, key: Option<String>, cx: &mut Context<Self>) {
        let Some(language) = self.language else { return };
        let original = Arc::new(original.clone());
        let line = one_line(sent, usize::MAX);
        Arc::make_mut(&mut self.sent).push(Sent {
            language,
            line: line.clone(),
            original: Arc::clone(&original),
        });
        if let Some(key) = key {
            self.keep_original(key, &original, line);
        }
        self.pump(cx);
        self.share(cx);
        cx.notify();
    }

    /// The post sent as `sent` went out as the outbox row with `client_message_id`: its original is kept by it.
    pub fn sent_as(&mut self, client_message_id: String, sent: &RichText) {
        let line = one_line(sent, usize::MAX);
        let Some(original) = self
            .sent
            .iter()
            .rev()
            .find(|kept| kept.line == line)
            .map(|kept| Arc::clone(&kept.original))
        else {
            return;
        };
        self.keep_original(client_message_id, &original, line);
    }

    fn keep_original(&self, key: String, original: &RichText, sent_text: String) {
        let (Some(language), Ok(content)) = (self.language, serde_json::to_string(original)) else {
            return;
        };
        drop(self.core.keep_sent_original(SentOriginal {
            conversation_id: self.chat.clone(),
            key,
            language: language.to_owned(),
            content,
            sent_text,
            created_at: chrono::Utc::now().timestamp_millis(),
        }));
    }

    /// The notice the chat shows while translating stopped: why, with Try again.
    pub fn notice(model: &Entity<Self>, palette: Palette, cx: &App) -> Option<AnyElement> {
        let Status::Failed(failure) = model.read(cx).status() else {
            return None;
        };
        let retry = model.clone();
        let again = Button::new("chat-translate-retry")
            .label("Try again")
            .outline()
            .on_click(move |_, _, cx| retry.update(cx, ChatTranslation::retry));
        Some(
            div()
                .debug_selector(|| "chat-translate-notice".to_owned())
                .mx_4()
                .mb_2()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_sm()
                .child(Glyph::LANGUAGES.icon().size(px(16.0)).text_color(rgb(theme::DANGER)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(format!("This chat isn't being translated. {}", failure.message())),
                )
                .child(selected_by("chat-translate-retry", again))
                .into_any_element(),
        )
    }
}

/// A message's key in what the tool is given: its conversation, id and version, so an edit is given anew.
pub(crate) fn key_of(source: &Source) -> String {
    format!("{} {} {}", source.conversation_id, source.id, source.version)
}

/// The context of what is asked about `sources`' messages at `picked` (oldest first), which reply to the messages
/// `replies` names (each picked one's, or what an outgoing message replies to): the messages before the one at
/// `before` but the picked, the nearest first, at most `limits` (lines, characters), and every message replied to that
/// isn't picked, among the sources or read from the store (`far`, by id: older than the sources), labelled (`q1`) and
/// told whole up to [`QUOTE_LINE`]; oldest first. With it, what each of `replies` replies to: a context line's label,
/// or a picked message's number in the batch.
pub(crate) fn window(
    sources: &[Source],
    far: &HashMap<String, Option<Source>>,
    picked: &[usize],
    replies: &[Vec<String>],
    before: usize,
    (max_lines, max_chars): (usize, usize),
) -> (Vec<Said>, Vec<Option<String>>) {
    let far_of = |id: &str| far.get(id).and_then(Option::as_ref);
    // each message replied to: its id and its label
    let mut labels: Vec<(&str, String)> = Vec::new();
    let re = replies
        .iter()
        .map(|ids| {
            ids.iter().find_map(|id| {
                if let Some(n) = picked.iter().position(|at| sources[*at].id == *id) {
                    return Some((n + 1).to_string());
                }
                if let Some((_, label)) = labels.iter().find(|(known, _)| *known == id) {
                    return Some(label.clone());
                }
                if !sources.iter().any(|source| source.id == *id) && far_of(id).is_none() {
                    return None;
                }
                let label = format!("q{}", labels.len() + 1);
                labels.push((id.as_str(), label.clone()));
                Some(label)
            })
        })
        .collect();
    let label_of = |id: &str| {
        labels
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, label)| label.clone())
    };
    let mut chosen: Vec<usize> = Vec::new();
    let mut chars = 0;
    for index in (0..before.min(sources.len()))
        .rev()
        .filter(|index| !picked.contains(index))
    {
        chars += sources[index].line.len();
        if chosen.len() >= max_lines || chars > max_chars {
            break;
        }
        chosen.push(index);
    }
    chosen.extend((0..sources.len()).filter(|at| !picked.contains(at) && label_of(&sources[*at].id).is_some()));
    chosen.sort_unstable();
    chosen.dedup();
    let said = |source: &Source| {
        let label = label_of(&source.id);
        Said {
            key: key_of(source),
            from: source.from.clone(),
            text: if label.is_some() {
                quote_line(&source.text)
            } else {
                source.line.clone()
            },
            label,
        }
    };
    let context = labels
        .iter()
        .filter_map(|(id, _)| far_of(id).filter(|_| !sources.iter().any(|source| source.id == *id)))
        .map(said)
        .chain(chosen.into_iter().map(|index| said(&sources[index])))
        .collect();
    (context, re)
}

/// A message replied to as a context line.
pub(crate) fn quote_line(text: &RichText) -> String {
    one_line(text, QUOTE_LINE)
}

/// Whether `message` is one a chat's translation reads: a text message with something in it, not deleted.
pub(crate) fn translatable(message: &Message) -> bool {
    message.delete_time.is_none()
        && !message.content.trim().is_empty()
        && (message.message_type == MessageType::RichTextHtml.as_str()
            || message.message_type == MessageType::Text.as_str())
}

/// A kept row as an entry.
fn entry_of_row(row: &StoredTranslation) -> Option<Entry> {
    let version = row.version;
    let text = || serde_json::from_str::<RichText>(&row.content).ok().map(Arc::new);
    Some(match row.kind {
        TranslationKind::Translated => Entry::Translated {
            version,
            text: text()?,
            source: row.source_language.clone(),
        },
        TranslationKind::Same => Entry::Same {
            version,
            source: row.source_language.clone(),
        },
        TranslationKind::Original => Entry::Original { version, text: text()? },
    })
}

/// An entry as the store keeps it (a failed one isn't kept).
fn row_of(source: &Source, language: &str, entry: &Entry) -> Option<StoredTranslation> {
    let (kind, content, source_language) = match entry {
        Entry::Translated { text, source, .. } => (
            TranslationKind::Translated,
            serde_json::to_string(text.as_ref()).ok()?,
            source.clone(),
        ),
        Entry::Same { source, .. } => (TranslationKind::Same, String::new(), source.clone()),
        Entry::Original { text, .. } => (
            TranslationKind::Original,
            serde_json::to_string(text.as_ref()).ok()?,
            None,
        ),
        Entry::Failed { .. } => return None,
    };
    Some(StoredTranslation {
        conversation_id: source.conversation_id.clone(),
        message_id: source.id.clone(),
        version: source.version,
        language: language.to_owned(),
        kind,
        content,
        source_language,
    })
}

fn original_row(source: &Source, language: &str, original: &Arc<RichText>) -> StoredTranslation {
    StoredTranslation {
        conversation_id: source.conversation_id.clone(),
        message_id: source.id.clone(),
        version: source.version,
        language: language.to_owned(),
        kind: TranslationKind::Original,
        content: serde_json::to_string(original.as_ref()).unwrap_or_default(),
        source_language: None,
    }
}

/// How long before an edit's time its original may have been kept and still be its own, ms (the two clocks differ).
const EDIT_SKEW: i64 = 120_000;

/// The account's own message `source` as written, from the originals the store kept: a post's by its
/// `clientmessageid` (unless edited since), an edit's by its message and the text it was sent as.
fn stored_original(source: &Source, language: &str, originals: &[SentOriginal]) -> Option<Arc<RichText>> {
    let line = one_line(&source.text, usize::MAX);
    let edit = SentOriginal::edit_key(&source.id);
    let post = source.client_message_id.as_deref();
    let found = originals.iter().filter(|kept| kept.language == language).find(|kept| {
        let same_text = kept.sent_text == line;
        if Some(kept.key.as_str()) == post {
            return same_text || source.edit_time.is_none();
        }
        kept.key == edit && (same_text || source.edit_time.is_some_and(|at| at + EDIT_SKEW >= kept.created_at))
    })?;
    serde_json::from_str::<RichText>(&found.content).ok().map(Arc::new)
}

/// An outgoing answer as the box takes it: the translation put back together, or the tool's failure.
fn outgoing_of(answer: Result<(Option<String>, Option<String>), Failure>, shells: &Shells) -> Outgoing {
    match answer {
        Ok((Some(text), _)) => Ok(Some(decode(&text, shells))),
        Ok((None, _)) => Ok(None),
        Err(failure) => Err(SendFailure::Tool(failure)),
    }
}

/// The settings' description of where a chat's text goes.
pub const SETTINGS_NOTE: &str = "When you translate a chat, its messages and yours, with its title and its members' \
     names, go to this tool on your computer and its provider.";

#[cfg(test)]
#[path = "../tests/support/chat_translate_units.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/support/chat_translate_eval.rs"]
mod eval;
