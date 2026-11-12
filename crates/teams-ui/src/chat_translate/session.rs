//! One chat's translating tool, on a thread of its own so the window never waits on it: [`crate::local_ai`]'s
//! [`Tool`] with the chat model ([`Work::Chat`]), Claude Code or the Codex CLI without tools, hooks, MCP servers or
//! saved sessions.
//!
//! - **The chat's context**: the process keeps the conversation, so each batch is read against the batches before it.
//!   Each job carries what the tool should know of the chat ([`Brief`]: its title, who is who, its glossary) and the
//!   messages around its own as `<context>`; the session tells a process each of them once ([`Given`]): the chat on
//!   its first question (again when it changes), each term and each context line once, so a process started anew
//!   (after [`Limits::turns`] questions or [`Limits::chars`] characters, or after a failure) loses nothing that
//!   matters, and a running one isn't told twice what it holds. A process's first question takes the whole context
//!   the job carries; a later one at most [`WARM_LINES`] lines and [`WARM_CHARS`] characters of what is new to it,
//!   besides the messages replied to, which go every time.
//! - **One job at a time**, in the order handed in: the model hands one batch at a time and an outgoing message at
//!   once, so a send waits at most for the batch running.
//! - **As it comes**: a batch's answers are told one message at a time as the tool writes them ([`Outcome::Partial`]),
//!   so the chat shows each translation when it is ready rather than the batch's all at once.
//! - **An echo is no answer**: a "translation" that is the message as written (a fast model sometimes hands a batch
//!   back untranslated) is left out, so the model asks for it again ([`without_echoes`]).
//! - **Failures** are told with the job they ended ([`Outcome`]); the process is closed and the next job starts a new
//!   one. Nothing here logs a message, a translation or the tool's output.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use super::prompt::{self, Answer, Brief, Item, Preface, Said, Term};
pub use crate::local_ai::Failure;
use crate::local_ai::{Backend, Launch, Model, Tool, Work};

/// How long a wait for the next job polls before it looks at whether the session was dropped.
const POLL: Duration = Duration::from_millis(100);
/// The most context lines new to a running process one question gives it (the messages replied to aside), and their
/// most characters: it holds the conversation before them.
pub const WARM_LINES: usize = 15;
pub const WARM_CHARS: usize = 3000;

/// When a session's process is replaced, and how long the tool may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Questions asked of one process before it is replaced.
    pub turns: usize,
    /// Characters sent to one process before it is replaced.
    pub chars: usize,
    /// How long the first answer may take (the process starting and signing in).
    pub first: Duration,
    /// How long any later answer may take.
    pub turn: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            turns: 40,
            chars: 120_000,
            first: Duration::from_secs(90),
            turn: Duration::from_secs(60),
        }
    }
}

/// What a session runs: the tool, where it is, its model, the language the chat is read in, and its limits.
#[derive(Clone, Debug)]
pub struct Spec {
    pub backend: Backend,
    pub program: PathBuf,
    pub model: Model,
    /// The chat's language's English name, as the instructions name it.
    pub language: &'static str,
    pub limits: Limits,
}

/// What the tool is asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    /// A batch of the chat's messages, into the chat's language.
    Batch {
        id: u64,
        brief: Arc<Brief>,
        context: Vec<Said>,
        items: Vec<Item>,
    },
    /// The account's own message (encoded), into the chat's language `to` (`None`: judged from the context), and the
    /// context line it replies to (its label).
    Outgoing {
        id: u64,
        brief: Arc<Brief>,
        context: Vec<Said>,
        to: Option<String>,
        re: Option<String>,
        text: String,
    },
}

/// What a session tells the model.
#[derive(Debug)]
pub enum Outcome {
    /// Answers of a batch still running, as the tool writes them: each message's once (again only when a later agent
    /// message answers it otherwise), before the batch's [`Outcome::Batch`] (which holds them again).
    Partial { id: u64, answers: HashMap<usize, Answer> },
    /// A batch's answers, by the messages' numbers, and the terms the answer added to the chat's glossary; or why it
    /// failed.
    Batch {
        id: u64,
        answers: Result<HashMap<usize, Answer>, Failure>,
        terms: Vec<Term>,
    },
    /// An outgoing message's translation (`None`: already in the chat's language) and the language it named; or why
    /// it failed.
    Outgoing {
        id: u64,
        answer: Result<(Option<String>, Option<String>), Failure>,
    },
}

/// A running session: dropping it stops its thread and its process.
pub struct Session {
    jobs: mpsc::Sender<Job>,
    stop: Arc<AtomicBool>,
    outcomes: async_channel::Receiver<Outcome>,
}

impl Session {
    /// Starts `spec`'s session; its tool starts with the first job.
    #[must_use]
    pub fn start(spec: Spec) -> Self {
        let (jobs, queued) = mpsc::channel();
        let (out, outcomes) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            spec,
            jobs: queued,
            out,
            stop: Arc::clone(&stop),
            tool: None,
            given: Given::default(),
        };
        let _ = thread::Builder::new()
            .name("chat-translate".into())
            .spawn(move || worker.run());
        Self { jobs, stop, outcomes }
    }

    /// Hands `job` to the tool.
    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    /// Where its outcomes come.
    #[must_use]
    pub fn outcomes(&self) -> async_channel::Receiver<Outcome> {
        self.outcomes.clone()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// What the running process was told: the chat, its terms (by the term) and the messages it read (by their keys).
#[derive(Debug, Default)]
pub(crate) struct Given {
    chat: Option<Brief>,
    terms: HashMap<String, String>,
    seen: HashSet<String>,
    /// Whether it answered a question yet.
    warm: bool,
}

impl Given {
    /// What a question with `brief` and `context` (oldest first) starts with, for a process told what this holds.
    pub(crate) fn preface<'a>(&self, brief: &'a Brief, context: &'a [Said]) -> Preface<'a> {
        let new_chat = self.chat.as_ref().is_none_or(|told| !told.same_chat(brief));
        let terms = brief
            .terms
            .iter()
            .filter(|term| new_chat || self.terms.get(&term.term) != Some(&term.rendering))
            .collect();
        let mut lines = Vec::new();
        let (mut count, mut chars) = (0, 0);
        for said in context.iter().rev() {
            let replied_to = said.label.is_some();
            if !replied_to && !said.key.is_empty() && self.seen.contains(&said.key) {
                continue;
            }
            if !replied_to && self.warm {
                count += 1;
                chars += said.text.len();
                if count > WARM_LINES || chars > WARM_CHARS {
                    continue;
                }
            }
            lines.push(said);
        }
        lines.reverse();
        Preface {
            chat: new_chat.then_some(brief),
            terms,
            context: lines,
        }
    }

    /// The process answered a question that started with `preface` and asked about `items`; its answer added `terms`.
    pub(crate) fn told(&mut self, preface: &Preface<'_>, items: &[Item], terms: &[Term]) {
        if let Some(chat) = preface.chat {
            if self
                .chat
                .as_ref()
                .is_some_and(|told| told.conversation != chat.conversation)
            {
                self.terms.clear();
            }
            self.chat = Some(Brief {
                terms: Vec::new(),
                ..chat.clone()
            });
        }
        for term in preface.terms.iter().copied().chain(terms) {
            self.terms.insert(term.term.clone(), term.rendering.clone());
        }
        let keys = preface
            .context
            .iter()
            .map(|said| &said.key)
            .chain(items.iter().map(|item| &item.key));
        self.seen.extend(keys.filter(|key| !key.is_empty()).cloned());
        self.warm = true;
    }
}

/// The session's thread.
struct Worker {
    spec: Spec,
    jobs: Receiver<Job>,
    out: async_channel::Sender<Outcome>,
    stop: Arc<AtomicBool>,
    tool: Option<Tool>,
    given: Given,
}

impl Worker {
    fn run(mut self) {
        while let Some(job) = self.next_job() {
            let outcome = match job {
                Job::Batch {
                    id,
                    brief,
                    context,
                    items,
                } => self.batch(id, &brief, &context, &items),
                Job::Outgoing {
                    id,
                    brief,
                    context,
                    to,
                    re,
                    text,
                } => self.outgoing(id, &brief, &context, (to.as_deref(), re.as_deref()), &text),
            };
            if self.stop.load(Ordering::Relaxed) || self.out.try_send(outcome).is_err() {
                break;
            }
        }
        if let Some(tool) = self.tool.take() {
            tool.close();
        }
    }

    /// A batch asked of the tool: each message's answer told as soon as it is written ([`Outcome::Partial`]), then the
    /// whole answer.
    fn batch(&mut self, id: u64, brief: &Brief, context: &[Said], items: &[Item]) -> Outcome {
        let out = self.out.clone();
        let stop = Arc::clone(&self.stop);
        let mut told = HashMap::new();
        let mut partial = |so_far: &str| {
            let fresh = without_echoes(fresh_answers(so_far, &mut told), items);
            if !fresh.is_empty() && !stop.load(Ordering::Relaxed) {
                let _ = out.try_send(Outcome::Partial { id, answers: fresh });
            }
        };
        let answer = self.ask(brief, context, items, &mut partial, |preface| {
            prompt::batch(preface, items)
        });
        match answer {
            Ok((text, terms)) => Outcome::Batch {
                id,
                answers: Ok(without_echoes(prompt::read_batch(&text), items)),
                terms,
            },
            Err(failure) => Outcome::Batch {
                id,
                answers: Err(failure),
                terms: Vec::new(),
            },
        }
    }

    /// An outgoing message asked of the tool: into `to`, replying to `re`.
    fn outgoing(
        &mut self,
        id: u64,
        brief: &Brief,
        context: &[Said],
        (to, re): (Option<&str>, Option<&str>),
        text: &str,
    ) -> Outcome {
        let backend = self.spec.backend;
        let answer = self
            .ask(brief, context, &[], &mut |_| {}, |preface| {
                prompt::outgoing(preface, to, re, text)
            })
            .and_then(|(text, _)| prompt::read_outgoing(&text).ok_or_else(|| not_a_translation(backend)));
        Outcome::Outgoing { id, answer }
    }

    /// The next job, or `None` once the session was dropped.
    fn next_job(&self) -> Option<Job> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return None;
            }
            match self.jobs.recv_timeout(POLL) {
                Ok(job) => return Some(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Asks the tool (started, or started anew once worn, first) the question `message` makes of what the process
    /// isn't told yet, `partial` told the answer so far as it comes: the answer and the terms it added. A failure
    /// closes the process.
    fn ask(
        &mut self,
        brief: &Brief,
        context: &[Said],
        items: &[Item],
        partial: &mut dyn FnMut(&str),
        message: impl FnOnce(&Preface<'_>) -> String,
    ) -> Result<(String, Vec<Term>), Failure> {
        let limits = self.spec.limits;
        if let Some(tool) = self
            .tool
            .take_if(|tool| tool.turns() >= limits.turns || tool.chars() >= limits.chars)
        {
            tool.close();
        }
        let mut tool = if let Some(tool) = self.tool.take() {
            tool
        } else {
            self.given = Given::default();
            Tool::start(
                &Launch {
                    backend: self.spec.backend,
                    program: self.spec.program.clone(),
                    work: Work::Chat,
                    model: self.spec.model.clone(),
                    instructions: prompt::instructions(self.spec.language),
                    first: limits.first,
                },
                &self.stop,
            )?
        };
        let preface = self.given.preface(brief, context);
        let wait = if tool.answered() { limits.turn } else { limits.first };
        match tool.ask_streaming(&message(&preface), wait, &self.stop, partial) {
            Ok(text) => {
                let terms = if items.is_empty() {
                    Vec::new()
                } else {
                    prompt::read_terms(&text)
                };
                self.given.told(&preface, items, &terms);
                self.tool = Some(tool);
                Ok((text, terms))
            }
            Err(failure) => {
                tool.close();
                Err(failure)
            }
        }
    }
}

/// The answers complete in a batch's answer `so_far` that weren't `told` yet, or were told otherwise (a later agent
/// message correcting one), now told. An answer is complete once its element is closed, so a message cut mid-stream is
/// never taken; a delta may close one and start the next, so every delta is read.
fn fresh_answers(so_far: &str, told: &mut HashMap<usize, Answer>) -> HashMap<usize, Answer> {
    let fresh: HashMap<usize, Answer> = prompt::read_batch(so_far)
        .into_iter()
        .filter(|(n, answer)| told.get(n) != Some(answer))
        .collect();
    told.extend(fresh.iter().map(|(n, answer)| (*n, answer.clone())));
    fresh
}

/// `answers` but those that hand a message of `items` back as it was written.
fn without_echoes(mut answers: HashMap<usize, Answer>, items: &[Item]) -> HashMap<usize, Answer> {
    answers.retain(|n, answer| match answer {
        Answer::Translated { text, .. } => !items.iter().any(|item| item.n == *n && item.text.trim() == text.trim()),
        Answer::Same { .. } => true,
    });
    answers
}

/// What an answer that isn't a translation fails with.
fn not_a_translation(backend: Backend) -> Failure {
    Failure::Stopped(backend, Some("its answer wasn't a translation.".to_owned()))
}

#[cfg(test)]
#[path = "../../tests/support/chat_translate_session_units.rs"]
mod tests;
