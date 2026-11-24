//! A draft proofread before it is sent, on this computer, by the AI command-line tool the user picked
//! ([`crate::local_ai`]: Claude Code or the Codex CLI, the model [`Work::Correction`]): its spelling, grammar and style
//! in whatever language it is written, the recent messages of its chat as context so that their names and terms stay.
//!
//! - **Only when asked, and never sent**: the compose box starts one on the user's request and shows what comes back
//!   beside the draft as written ([`crate::compose`]'s correction); the draft stays as it was until the user takes it.
//! - **The draft keeps its shape** ([`draft`]): only the words of its text blocks go to the tool, between numbered
//!   markers; blocks of code, pictures, rules, mentions and inline code never do, and an answer that doesn't keep every
//!   marker is not used.
//! - **Chat style**: a line or a message that ends with no final dot gets none ([`final_dot`]).
//! - **One run per request** ([`Correcting`]), on a thread of its own so the window never waits on the tool. Dropping it
//!   stops it and ends the tool's process.
//! - **Private**: no draft, message, correction or tool output is logged or traced.

pub mod diff;
pub mod draft;
pub mod final_dot;
pub mod prompt;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::chat_translate::prompt::Said;
use crate::editor::doc::Doc;
use crate::local_ai::{Backend, Failure, Launch, Model, Target, Tool, Work};
use diff::Preview;
use draft::Encoded;
use prompt::Answer;

/// The longest draft sent to the tool (its encoded text).
pub const MAX_DRAFT: usize = crate::chat_translate::MAX_MESSAGE;

/// The settings' description of where a draft goes.
pub const SETTINGS_NOTE: &str = "When you ask for a correction, your message, the chat's recent messages and the \
                                 message a reply quotes, with the names of their writers, go to this tool on your \
                                 computer and its provider.";

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
            answer: Duration::from_secs(60),
        }
    }
}

/// What to proofread, with which tool and model.
#[derive(Clone, Debug)]
pub struct Request {
    pub backend: Backend,
    pub target: Target,
    pub model: Model,
    /// The chat's recent messages, oldest first.
    pub context: Vec<Said>,
    pub draft: Encoded,
    pub limits: Limits,
}

/// A correction the tool proposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    /// The draft as corrected, its shape as it was.
    pub corrected: Doc,
    /// What changed, as the box shows it.
    pub preview: Preview,
}

/// How a correction ended: what it proposes, or `None` when there is nothing to correct.
pub type Outcome = Result<Option<Proposal>, Failure>;

/// A correction being made: dropping it stops its thread and its tool.
pub struct Correcting {
    stop: Arc<AtomicBool>,
    done: async_channel::Receiver<Outcome>,
}

impl Correcting {
    /// Starts `request`'s correction on a thread of its own.
    #[must_use]
    pub fn start(request: Request) -> Self {
        let (out, done) = async_channel::bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let backend = request.backend;
        let worker_stop = Arc::clone(&stop);
        let worker_out = out.clone();
        let spawned = thread::Builder::new()
            .name("message-correct".into())
            .spawn(move || correct(&request, &worker_stop, &worker_out));
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

impl Drop for Correcting {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Tells `out` what came of the correction, unless it was stopped.
fn tell(out: &async_channel::Sender<Outcome>, stop: &AtomicBool, outcome: Outcome) {
    if !stop.load(Ordering::Relaxed) {
        let _ = out.try_send(outcome);
    }
}

/// Starts the tool, asks it, tells what came of it (the box doesn't wait for the process to leave) and ends it.
fn correct(request: &Request, stop: &AtomicBool, out: &async_channel::Sender<Outcome>) {
    let launch = Launch {
        backend: request.backend,
        target: request.target.clone(),
        work: Work::Correction,
        model: request.model.clone(),
        instructions: prompt::instructions(),
        first: request.limits.start,
    };
    let mut tool = match Tool::start(&launch, stop) {
        Ok(tool) => tool,
        Err(failure) => return tell(out, stop, Err(failure)),
    };
    let outcome = ask(&mut tool, request, stop);
    tell(out, stop, outcome);
    tool.close();
}

/// Asks for the correction, once more when the answer can't be used.
fn ask(tool: &mut Tool, request: &Request, stop: &AtomicBool) -> Outcome {
    // the first answer also waits for the tool to start
    let wait = request.limits.answer + request.limits.start;
    let message = prompt::request(&request.context, &request.draft.text);
    if let Ok(outcome) = read(&tool.ask(&message, wait, stop)?, &request.draft) {
        return Ok(outcome);
    }
    read(
        &tool.ask(prompt::FIX_REQUEST, request.limits.answer, stop)?,
        &request.draft,
    )
    .map_err(|Unusable| {
        request.target.named(Failure::Stopped(
            request.backend,
            Some("its answer didn't keep the message's formatting.".to_owned()),
        ))
    })
}

/// An answer that can't be used: it isn't one of the forms, or it doesn't keep the draft's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unusable;

/// What `answer` proposes for `draft`, `None` when it proposes nothing: a proposal that changes no word, or only adds a
/// final dot that the draft left out, is none.
fn read(answer: &str, draft: &Encoded) -> Result<Option<Proposal>, Unusable> {
    let mut corrected = match prompt::read(answer).ok_or(Unusable)? {
        Answer::Unchanged => return Ok(None),
        Answer::Corrected(text) => draft.decode(&text).ok_or(Unusable)?,
    };
    final_dot::keep_missing(draft.doc(), &mut corrected);
    let preview = diff::preview(draft.doc(), &corrected);
    Ok((!preview.marks.is_empty()).then_some(Proposal { corrected, preview }))
}

#[cfg(test)]
#[path = "../tests/support/message_correct_units.rs"]
mod tests;
