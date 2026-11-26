//! One translating tool's process, kept running for the call: started once (so the first caption doesn't wait for a
//! cold start), fed one caption at a time on its stdin and read back from its stdout, on a thread of its own so the
//! window never waits on it.
//!
//! The process is [`crate::local_ai`]'s [`Tool`], with the model picked for captions ([`Work::Captions`]): Claude Code
//! or the Codex CLI, without tools, hooks, MCP servers or saved sessions.
//!
//! **The call's context**: the process keeps the conversation, so each caption is read against the whole call so far
//! (the call's title and people, the captions sent with their speakers, the recent ones that weren't sent, the
//! translations it gave) without that being sent again. The conversation is bounded: after [`Limits::turns`] captions or
//! [`Limits::chars`] characters sent, the tool is asked for the call's glossary (its names, terms and abbreviations
//! with the renderings it used), and a new process starts with the call's brief, that glossary and the recent
//! captions with their translations ([`Memory`]), so a long call stays consistent and each prompt stays small.
//!
//! **Failures** end the session ([`Failure`]): the tool missing, signed out, too slow, or stopped; the model shows the
//! captions as spoken and says why. Nothing here logs a caption, a translation or the tool's output.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use super::{LineKey, instructions};
pub use crate::local_ai::Failure;
use crate::local_ai::{Backend, Launch, Model, Target, Tool, Work, cap};

/// How long a wait for the next caption polls before it looks at whether the session was dropped.
const POLL: Duration = Duration::from_millis(100);
/// The longest answer taken; anything longer isn't a caption's translation.
const MAX_ANSWER: usize = 2000;
/// The longest glossary carried to the next process.
const MAX_GLOSSARY: usize = 2000;
/// The most captions the recent transcript carried to the next process holds, and its most characters.
const RECENT_LINES: usize = 30;
const RECENT_CHARS: usize = 6000;
/// The longest brief (the call's title and people) sent.
const MAX_BRIEF: usize = 1500;
/// What asks the tool for the call's glossary (the instructions say what to answer).
pub const GLOSSARY_REQUEST: &str = "<glossary-request/>";

/// When a session's process is replaced, and how long the tool may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Captions sent to one process before it is replaced.
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
            turns: 150,
            chars: 60_000,
            first: Duration::from_secs(45),
            turn: Duration::from_secs(20),
        }
    }
}

/// What a session runs: the tool, where it is, its model, the language it translates into, and its limits.
#[derive(Clone, Debug)]
pub struct Spec {
    pub backend: Backend,
    pub target: Target,
    pub model: Model,
    /// The target language's English name, as the instructions name it.
    pub language: &'static str,
    pub limits: Limits,
}

/// A line said in the call that goes to the tool as context only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    pub speaker: Option<String>,
    pub text: String,
}

/// One caption to translate, with what goes with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub key: LineKey,
    /// The caption's text as spoken.
    pub source: String,
    pub final_result: bool,
    pub speaker: Option<String>,
    /// What was said since the last caption sent that isn't sent for translation (already in the target language,
    /// answered from the cache, or dropped from a full queue), in order.
    pub said: Vec<Said>,
    /// The call's title and people, when they changed since the last caption sent.
    pub brief: Option<String>,
}

/// What a session tells the model.
#[derive(Debug)]
pub enum Outcome {
    Translated { job: Job, text: String },
    Failed(Failure),
}

/// A running session: dropping it stops its thread and its process.
pub struct Session {
    jobs: mpsc::Sender<Job>,
    stop: Arc<AtomicBool>,
    outcomes: async_channel::Receiver<Outcome>,
}

impl Session {
    /// Starts `spec`'s tool on a thread of its own.
    #[must_use]
    pub fn start(spec: Spec) -> Self {
        let (jobs, queued) = mpsc::channel();
        let (out, outcomes) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let backend = spec.backend;
        let worker = Worker {
            spec,
            jobs: queued,
            out: out.clone(),
            stop: Arc::clone(&stop),
            memory: Memory::default(),
        };
        if let Err(error) = thread::Builder::new()
            .name("caption-translate".into())
            .spawn(move || worker.run())
        {
            let _ = out.try_send(Outcome::Failed(Failure::Stopped(backend, Some(error.to_string()))));
        }
        Self { jobs, stop, outcomes }
    }

    /// Hands `job` to the tool (nothing once the session ended: its failure says so).
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

/// What a session remembers of the call to start a new process with.
#[derive(Debug, Default)]
struct Memory {
    brief: String,
    glossary: Option<String>,
    /// The recent captions: speaker, text as spoken, and its translation (none for one said only).
    recent: VecDeque<(Option<String>, String, Option<String>)>,
}

impl Memory {
    /// The message `job` goes as: on a new process the call's brief, glossary and recent captions first; else the
    /// brief only when it changed.
    fn message(&mut self, job: &Job, fresh: bool) -> String {
        let mut message = String::new();
        let changed = job.brief.as_ref().is_some_and(|brief| *brief != self.brief);
        if let Some(brief) = &job.brief {
            self.brief = cap(brief, MAX_BRIEF);
        }
        if (fresh || changed) && !self.brief.is_empty() {
            let _ = write!(message, "<call>\n{}\n</call>\n", self.brief);
        }
        if fresh {
            self.earlier(&mut message);
        }
        if !job.said.is_empty() {
            message.push_str("<said>\n");
            for said in &job.said {
                let _ = writeln!(message, "{}: {}", who(said.speaker.as_deref()), clean(&said.text));
            }
            message.push_str("</said>\n");
        }
        let unfinished = if job.final_result { "" } else { " unfinished" };
        let _ = write!(
            message,
            "<caption from=\"{}\"{unfinished}>{}</caption>",
            who(job.speaker.as_deref()).replace('"', "'"),
            clean(&job.source)
        );
        message
    }

    /// The glossary and the recent captions, for a new process.
    fn earlier(&self, message: &mut String) {
        if let Some(glossary) = &self.glossary {
            let _ = write!(message, "<glossary>\n{glossary}\n</glossary>\n");
        }
        if self.recent.is_empty() {
            return;
        }
        message.push_str("<earlier>\n");
        for (speaker, source, text) in &self.recent {
            let _ = writeln!(message, "{}: {}", who(speaker.as_deref()), clean(source));
            if let Some(text) = text {
                let _ = writeln!(message, "=> {}", clean(text));
            }
        }
        message.push_str("</earlier>\n");
    }

    /// Keeps `job` (a final caption) and its translation, and what was said with it, in the recent captions.
    fn remember(&mut self, job: &Job, text: &str) {
        for said in &job.said {
            self.recent.push_back((said.speaker.clone(), said.text.clone(), None));
        }
        if job.final_result {
            self.recent
                .push_back((job.speaker.clone(), job.source.clone(), Some(text.to_owned())));
        }
        let size = |recent: &VecDeque<(Option<String>, String, Option<String>)>| {
            recent
                .iter()
                .map(|(_, source, text)| source.len() + text.as_ref().map_or(0, String::len))
                .sum::<usize>()
        };
        while self.recent.len() > RECENT_LINES || size(&self.recent) > RECENT_CHARS {
            self.recent.pop_front();
        }
    }
}

/// A speaker's name as a message gives it.
fn who(speaker: Option<&str>) -> &str {
    speaker.filter(|name| !name.trim().is_empty()).unwrap_or("Speaker")
}

/// Text as a message carries it: no angle brackets to be read as the message's own tags.
fn clean(text: &str) -> String {
    text.replace('<', "‹").replace('>', "›")
}

/// The session's thread.
struct Worker {
    spec: Spec,
    jobs: Receiver<Job>,
    out: async_channel::Sender<Outcome>,
    stop: Arc<AtomicBool>,
    memory: Memory,
}

impl Worker {
    fn run(mut self) {
        let mut tool = match self.start() {
            Ok(tool) => tool,
            Err(failure) => return self.fail(failure),
        };
        while let Some(job) = self.next_job() {
            if tool.turns() >= self.spec.limits.turns || tool.chars() >= self.spec.limits.chars {
                tool = match self.renew(tool) {
                    Ok(tool) => tool,
                    Err(failure) => return self.fail(failure),
                };
            }
            let message = self.memory.message(&job, tool.turns() == 0);
            let wait = if tool.answered() {
                self.spec.limits.turn
            } else {
                self.spec.limits.first
            };
            match tool.ask(&message, wait, &self.stop).map(|text| answer(&text)) {
                Ok(text) => {
                    self.memory.remember(&job, &text);
                    if self.out.try_send(Outcome::Translated { job, text }).is_err() {
                        break;
                    }
                }
                Err(failure) => {
                    tool.close();
                    return self.fail(failure);
                }
            }
        }
        tool.close();
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

    /// Asks the worn process for the call's glossary (kept when it answers), ends it and starts the next.
    fn renew(&mut self, mut tool: Tool) -> Result<Tool, Failure> {
        if let Ok(glossary) = tool.ask(GLOSSARY_REQUEST, self.spec.limits.turn, &self.stop) {
            let glossary = glossary.trim();
            self.memory.glossary =
                (!glossary.is_empty() && !glossary.eq_ignore_ascii_case("none")).then(|| cap(glossary, MAX_GLOSSARY));
        }
        tool.close();
        self.start()
    }

    /// Starts the tool, with the instructions for the session's language.
    fn start(&self) -> Result<Tool, Failure> {
        let launch = Launch {
            backend: self.spec.backend,
            target: self.spec.target.clone(),
            work: Work::Captions,
            model: self.spec.model.clone(),
            instructions: instructions(self.spec.language),
            first: self.spec.limits.first,
        };
        Tool::start(&launch, &self.stop)
    }

    /// Tells the model, unless the session was dropped (nobody listens then).
    fn fail(&self, failure: Failure) {
        if !self.stop.load(Ordering::Relaxed) {
            let _ = self.out.try_send(Outcome::Failed(failure));
        }
    }
}

/// An answer as shown: trimmed, without a caption tag the tool may have repeated, and capped.
fn answer(text: &str) -> String {
    let mut text = text.trim();
    if let Some(rest) = text.strip_prefix("<caption")
        && let Some((_, rest)) = rest.split_once('>')
    {
        text = rest.trim_start();
    }
    let text = text.strip_suffix("</caption>").unwrap_or(text).trim();
    cap(text, MAX_ANSWER)
}

#[cfg(test)]
#[path = "../../tests/support/translate_session_units.rs"]
mod tests;
