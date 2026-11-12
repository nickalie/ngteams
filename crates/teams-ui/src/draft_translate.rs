//! A draft translated in its box, on this computer, by the AI command-line tool the user picked ([`crate::local_ai`]:
//! Claude Code or the Codex CLI, the model [`Work::Draft`]), into the one language the settings keep for every box and
//! account ([`crate::settings::Settings::message_translate_to`]).
//!
//! - **Only when asked**: the compose box's Translate ([`crate::compose`]) starts one, and its result replaces the
//!   draft as one edit, so Ctrl+Z brings the draft as written back.
//! - **Only the draft's words and the language go to the tool**: no chat, history, names or tokens. The draft keeps
//!   its shape ([`crate::message_correct::draft`]): only the words of its text blocks go, between numbered markers;
//!   mentions, links that show their own address, inline code, line breaks, blocks of code and pictures never do, and
//!   an answer that doesn't keep every marker is not used.
//! - **Behind [`Engine`]**: the app's is the local tool ([`LocalTools`]); tests put a fake in its place
//!   ([`set_engine`]), so no test runs a real tool.
//! - **One run per request** ([`Translating`]), on a thread of its own, each wait on the tool bounded ([`Limits`]).
//!   Dropping it stops it and ends the tool's process.
//! - **Private**: no draft, translation or tool output is logged or traced.

pub mod menu;
pub mod prompt;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use gpui::{App, Global};

use crate::editor::doc::Doc;
use crate::local_ai::{Backend, Failure, Launch, Model, Tool, Work};
use crate::message_correct::draft::Encoded;
use crate::translate::TARGET_LANGUAGES;

/// The longest draft sent to the tool (its encoded text).
pub const MAX_DRAFT: usize = crate::message_correct::MAX_DRAFT;
/// The language a draft is translated into until the user picks one.
pub const DEFAULT_LANGUAGE: &str = "English";
/// The longest language name kept, in bytes.
pub const MAX_LANGUAGE: usize = 60;

/// The settings' description of where a draft goes.
pub const SETTINGS_NOTE: &str = "When you translate a message you are writing, its text and the language go to this \
                                 tool on your computer and its provider; nothing else of the chat does.";

/// The languages the picker lists: every language captions and chats are translated into, and more that many people
/// write in. Any other is typed.
pub const COMMON_LANGUAGES: &[&str] = &[
    "Afrikaans",
    "Albanian",
    "Amharic",
    "Arabic",
    "Armenian",
    "Azerbaijani",
    "Basque",
    "Belarusian",
    "Bengali",
    "Bosnian",
    "Bulgarian",
    "Burmese",
    "Catalan",
    "Chinese (Simplified)",
    "Chinese (Traditional)",
    "Croatian",
    "Czech",
    "Danish",
    "Dutch",
    "English",
    "Esperanto",
    "Estonian",
    "Filipino",
    "Finnish",
    "French",
    "Galician",
    "Georgian",
    "German",
    "Greek",
    "Gujarati",
    "Hausa",
    "Hebrew",
    "Hindi",
    "Hungarian",
    "Icelandic",
    "Indonesian",
    "Irish",
    "Italian",
    "Japanese",
    "Kannada",
    "Kazakh",
    "Khmer",
    "Korean",
    "Kurdish",
    "Kyrgyz",
    "Lao",
    "Latin",
    "Latvian",
    "Lithuanian",
    "Luxembourgish",
    "Macedonian",
    "Malay",
    "Malayalam",
    "Maltese",
    "Marathi",
    "Mongolian",
    "Nepali",
    "Norwegian",
    "Pashto",
    "Persian",
    "Polish",
    "Portuguese (Brazil)",
    "Portuguese (Portugal)",
    "Punjabi",
    "Romanian",
    "Russian",
    "Serbian",
    "Sinhala",
    "Slovak",
    "Slovenian",
    "Somali",
    "Spanish",
    "Swahili",
    "Swedish",
    "Tajik",
    "Tamil",
    "Telugu",
    "Thai",
    "Turkish",
    "Turkmen",
    "Ukrainian",
    "Urdu",
    "Uzbek",
    "Vietnamese",
    "Welsh",
    "Yiddish",
    "Yoruba",
    "Zulu",
];

/// The language `typed` names, as the settings keep it: a listed one by its own name (whatever its case, or by its
/// code), else what was typed, trimmed. `None` when it holds no letter, is longer than [`MAX_LANGUAGE`] bytes, or has a
/// character that could be read as markup or a line of its own.
#[must_use]
pub fn language_name(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if let Some(listed) = listed(typed) {
        return Some(listed.to_owned());
    }
    let shaped = typed.len() <= MAX_LANGUAGE
        && typed.chars().any(char::is_alphabetic)
        && !typed
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '&' | '"' | '`' | '{' | '}'));
    shaped.then(|| typed.to_owned())
}

/// The listed language `typed` names, by its name or (one of [`TARGET_LANGUAGES`]) its code, ignoring case.
#[must_use]
pub fn listed(typed: &str) -> Option<&'static str> {
    let typed = typed.trim();
    COMMON_LANGUAGES
        .iter()
        .copied()
        .find(|name| name.eq_ignore_ascii_case(typed))
        .or_else(|| {
            TARGET_LANGUAGES
                .iter()
                .find(|(code, _)| code.eq_ignore_ascii_case(typed))
                .map(|(_, name)| *name)
        })
}

/// The listed languages whose name has `query` in it, ignoring case (all of them for an empty query).
#[must_use]
pub fn matching(query: &str) -> Vec<&'static str> {
    let query = query.trim().to_lowercase();
    let coded = listed(&query);
    COMMON_LANGUAGES
        .iter()
        .copied()
        .filter(|name| query.is_empty() || name.to_lowercase().contains(&query) || Some(*name) == coded)
        .collect()
}

/// How long the tool may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// How long the tool may take to start.
    pub start: Duration,
    /// How long one answer may take.
    pub answer: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            start: Duration::from_secs(60),
            answer: Duration::from_secs(90),
        }
    }
}

/// What to translate, into which language, with which tool and model.
#[derive(Clone, Debug)]
pub struct Request {
    pub backend: Backend,
    pub program: PathBuf,
    pub model: Model,
    /// The language's name, as the settings keep it.
    pub language: String,
    pub draft: Encoded,
    pub limits: Limits,
}

/// How a translation ended: the draft translated, its shape as it was.
pub type Outcome = Result<Doc, Failure>;

/// What runs the AI tool: the local one in the app ([`LocalTools`]), a fake in tests.
pub trait Engine: Send + Sync {
    /// Starts `launch`'s tool, giving up once `stop` is set.
    ///
    /// # Errors
    /// The tool couldn't start, refused, or took too long.
    fn start(&self, launch: &Launch, stop: &AtomicBool) -> Result<Box<dyn Session>, Failure>;
}

/// A started tool's conversation.
pub trait Session {
    /// Sends `message` and waits up to `wait` for the answer.
    ///
    /// # Errors
    /// The tool refused, stopped, took longer than `wait`, or `stop` was set.
    fn ask(&mut self, message: &str, wait: Duration, stop: &AtomicBool) -> Result<String, Failure>;

    /// Ends the tool.
    fn close(self: Box<Self>);
}

/// The app's engine: Claude Code or the Codex CLI, started on this computer ([`Tool`]).
pub struct LocalTools;

impl Engine for LocalTools {
    fn start(&self, launch: &Launch, stop: &AtomicBool) -> Result<Box<dyn Session>, Failure> {
        Ok(Box::new(Tool::start(launch, stop)?))
    }
}

impl Session for Tool {
    fn ask(&mut self, message: &str, wait: Duration, stop: &AtomicBool) -> Result<String, Failure> {
        Self::ask(self, message, wait, stop)
    }

    fn close(self: Box<Self>) {
        Self::close(*self);
    }
}

/// The engine the app's boxes translate with: [`LocalTools`] unless one was set.
#[derive(Clone)]
struct Installed(Arc<dyn Engine>);

impl Global for Installed {}

/// Puts `engine` in place of the local tools for every box (tests, the tour).
pub fn set_engine(cx: &mut App, engine: Arc<dyn Engine>) {
    cx.set_global(Installed(engine));
}

/// The engine the boxes translate with.
#[must_use]
pub fn engine(cx: &App) -> Arc<dyn Engine> {
    cx.try_global::<Installed>().map_or_else(
        || Arc::new(LocalTools) as Arc<dyn Engine>,
        |installed| installed.0.clone(),
    )
}

/// A translation being made: dropping it stops its thread and its tool.
pub struct Translating {
    stop: Arc<AtomicBool>,
    done: async_channel::Receiver<Outcome>,
}

impl Translating {
    /// Starts `request`'s translation with `engine`, on a thread of its own.
    #[must_use]
    pub fn start(request: Request, engine: Arc<dyn Engine>) -> Self {
        let (out, done) = async_channel::bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let backend = request.backend;
        let worker_stop = Arc::clone(&stop);
        let worker_out = out.clone();
        let spawned = thread::Builder::new()
            .name("draft-translate".into())
            .spawn(move || translate(&request, engine.as_ref(), &worker_stop, &worker_out));
        if let Err(error) = spawned {
            let _ = out.try_send(Err(Failure::Stopped(backend, Some(error.to_string()))));
        }
        Self { stop, done }
    }

    /// Where its outcome comes.
    #[must_use]
    pub fn done(&self) -> async_channel::Receiver<Outcome> {
        self.done.clone()
    }
}

impl Drop for Translating {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Tells `out` what came of the translation, unless it was stopped.
fn tell(out: &async_channel::Sender<Outcome>, stop: &AtomicBool, outcome: Outcome) {
    if !stop.load(Ordering::Relaxed) {
        let _ = out.try_send(outcome);
    }
}

/// Starts the tool, asks it, tells what came of it (the box doesn't wait for the process to leave) and ends it.
fn translate(request: &Request, engine: &dyn Engine, stop: &AtomicBool, out: &async_channel::Sender<Outcome>) {
    let launch = Launch {
        backend: request.backend,
        program: request.program.clone(),
        work: Work::Draft,
        model: request.model.clone(),
        instructions: prompt::instructions(),
        first: request.limits.start,
    };
    let mut session = match engine.start(&launch, stop) {
        Ok(session) => session,
        Err(failure) => return tell(out, stop, Err(failure)),
    };
    let outcome = ask(session.as_mut(), request, stop);
    tell(out, stop, outcome);
    session.close();
}

/// Asks for the translation, once more when the answer can't be used.
fn ask(session: &mut dyn Session, request: &Request, stop: &AtomicBool) -> Outcome {
    // the first answer also waits for the tool to start
    let wait = request.limits.answer + request.limits.start;
    let message = prompt::request(&request.language, &request.draft.text);
    if let Some(doc) = read(&session.ask(&message, wait, stop)?, &request.draft) {
        return Ok(doc);
    }
    read(
        &session.ask(prompt::FIX_REQUEST, request.limits.answer, stop)?,
        &request.draft,
    )
    .ok_or_else(|| {
        Failure::Stopped(
            request.backend,
            Some("its answer didn't keep the message's formatting.".to_owned()),
        )
    })
}

/// The draft as `answer` translates it; `None` when it isn't the answer's form or doesn't keep the draft's shape.
fn read(answer: &str, draft: &Encoded) -> Option<Doc> {
    draft.decode(&prompt::read(answer)?)
}

#[cfg(test)]
#[path = "../tests/support/draft_translate_units.rs"]
mod tests;
