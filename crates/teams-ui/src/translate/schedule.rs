//! Which captions go to the tool and when, so a live call's stream of interim updates never turns into a process or a
//! request per fragment:
//!
//! - **one caption at a time**: the tool answers in order, so the next goes when the last came back;
//! - **finals first**, in order, every one (a queue of at most [`MAX_QUEUE`]: the older ones beyond it wait in a
//!   backlog, sent newest first when nothing newer is due, so a transcript's earlier lines get their translation too);
//! - **interim text debounced**: an utterance still being spoken goes once its text held for [`SETTLE`], or every
//!   [`REFRESH`] while it keeps changing, only its latest text, and never when its final is queued;
//! - **a cache** of the last [`CACHE`] texts translated (a final repeating its last interim text costs nothing), kept
//!   when the translation stops and starts again into the same language, so a line keeps its translation;
//! - **the call as context**: the newest [`MAX_SAID`] final captions not sent for translation (already in the target
//!   language, answered from the cache) go with the next one sent, and the call's brief (title and people) whenever it
//!   changed, so the tool's conversation holds the call.
//!
//! Translations are kept per utterance ([`LineKey`]) for the captions shown, in memory only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use teams_core::Caption;

use super::same_language;
use super::session::{Job, Said};

/// The most final captions waiting.
pub const MAX_QUEUE: usize = 6;
/// How long an interim text holds before it goes.
pub const SETTLE: Duration = Duration::from_millis(450);
/// How often an interim text that keeps changing goes.
pub const REFRESH: Duration = Duration::from_millis(1200);
/// How many translations the cache keeps.
pub const CACHE: usize = 2048;
/// The most final captions one caption sent carries as context.
pub const MAX_SAID: usize = 24;

/// An utterance, as the caption service updates it in place: its speaker, track and audio timestamp (or its id when
/// it has no timestamp).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LineKey {
    speaker: Option<String>,
    track: String,
    stamp: u64,
    id: Option<String>,
}

impl LineKey {
    #[must_use]
    pub fn of(line: &Caption) -> Self {
        Self {
            speaker: line.speaker.clone(),
            track: line.track_language.clone(),
            stamp: line.audio_timestamp,
            id: (line.audio_timestamp == 0).then(|| line.id.clone()),
        }
    }
}

/// A translation shown for an utterance.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    source: String,
    text: String,
    final_result: bool,
}

/// An interim text waiting to go.
#[derive(Clone, Debug)]
struct Pending {
    key: LineKey,
    source: String,
    speaker: Option<String>,
    /// When the utterance began waiting, and when its text last changed.
    since: Instant,
    changed: Instant,
}

/// What to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Send(Job),
    /// Nothing until then (an interim text settling).
    At(Instant),
    Idle,
}

/// See the module docs.
#[derive(Debug)]
pub struct Schedule {
    target: &'static str,
    shown: HashMap<LineKey, Shown>,
    finals: VecDeque<Job>,
    /// Finals beyond [`MAX_QUEUE`], oldest first.
    backlog: VecDeque<Job>,
    interim: Option<Pending>,
    busy: bool,
    /// Final captions already accounted for (sent, queued or said), so none goes twice.
    noted: HashSet<(LineKey, String)>,
    said: Vec<Said>,
    cache: HashMap<String, String>,
    cached: VecDeque<String>,
    brief: String,
    brief_sent: Option<String>,
}

impl Schedule {
    /// A schedule translating into language `target` (a code of [`super::TARGET_LANGUAGES`]).
    #[must_use]
    pub fn new(target: &'static str) -> Self {
        Self {
            target,
            shown: HashMap::new(),
            finals: VecDeque::new(),
            backlog: VecDeque::new(),
            interim: None,
            busy: false,
            noted: HashSet::new(),
            said: Vec::new(),
            cache: HashMap::new(),
            cached: VecDeque::new(),
            brief: String::new(),
            brief_sent: None,
        }
    }

    /// Starts from the translations `stopped` got, when it translated into the same language.
    pub fn resume(&mut self, stopped: Schedule) {
        if stopped.target == self.target {
            self.cache = stopped.cache;
            self.cached = stopped.cached;
        }
    }

    /// The call's title and people, as the next caption sent carries them when they changed.
    pub fn set_brief(&mut self, brief: String) {
        self.brief = brief;
    }

    /// Takes the captions shown now (oldest first).
    pub fn observe(&mut self, lines: &[Caption], now: Instant) {
        for line in lines.iter().filter(|line| !line.text.trim().is_empty()) {
            let key = LineKey::of(line);
            if same_language(self.target, &line.track_language) {
                self.note_said(&key, line);
            } else if !self.translated(&key, line) && !self.shown_from_cache(&key, line) {
                if line.final_result {
                    self.queue_final(key, line);
                } else {
                    self.wait_interim(key, line, now);
                }
            }
        }
        let keys: HashSet<LineKey> = lines.iter().map(LineKey::of).collect();
        self.shown.retain(|key, _| keys.contains(key));
        self.noted.retain(|(key, _)| keys.contains(key));
        self.backlog.retain(|job| keys.contains(&job.key));
    }

    /// Whether `line`'s text already has its translation shown.
    fn translated(&self, key: &LineKey, line: &Caption) -> bool {
        self.shown
            .get(key)
            .is_some_and(|shown| shown.source == line.text && (shown.final_result || !line.final_result))
    }

    /// Shows `line`'s translation from the cache, when it is there.
    fn shown_from_cache(&mut self, key: &LineKey, line: &Caption) -> bool {
        let Some(text) = self.cache.get(&line.text).cloned() else {
            return false;
        };
        self.shown.insert(
            key.clone(),
            Shown {
                source: line.text.clone(),
                text,
                final_result: line.final_result,
            },
        );
        self.note_said(key, line);
        true
    }

    /// A final caption not sent goes as context with the next one sent.
    fn note_said(&mut self, key: &LineKey, line: &Caption) {
        if line.final_result && self.noted.insert((key.clone(), line.text.clone())) {
            self.said.push(Said {
                speaker: line.display_name.clone(),
                text: line.text.clone(),
            });
            if self.said.len() > MAX_SAID {
                self.said.remove(0);
            }
        }
    }

    fn queue_final(&mut self, key: LineKey, line: &Caption) {
        if !self.noted.insert((key.clone(), line.text.clone())) {
            return;
        }
        self.finals.retain(|job| job.key != key);
        self.backlog.retain(|job| job.key != key);
        if self.interim.as_ref().is_some_and(|pending| pending.key == key) {
            self.interim = None;
        }
        self.finals
            .push_back(job(key, line.text.clone(), true, line.display_name.clone()));
        while self.finals.len() > MAX_QUEUE {
            if let Some(older) = self.finals.pop_front() {
                self.backlog.push_back(older);
            }
        }
    }

    fn wait_interim(&mut self, key: LineKey, line: &Caption, now: Instant) {
        if self.finals.iter().any(|job| job.key == key) {
            return;
        }
        match &mut self.interim {
            Some(pending) if pending.key == key => {
                if pending.source != line.text {
                    pending.source.clone_from(&line.text);
                    pending.changed = now;
                }
            }
            _ => {
                self.interim = Some(Pending {
                    key,
                    source: line.text.clone(),
                    speaker: line.display_name.clone(),
                    since: now,
                    changed: now,
                });
            }
        }
    }

    /// The next caption to send, when the last one came back.
    pub fn next(&mut self, now: Instant) -> Next {
        if self.busy {
            return Next::Idle;
        }
        if let Some(job) = self.finals.pop_front() {
            return Next::Send(self.sending(job));
        }
        let Some(pending) = &self.interim else {
            return self
                .backlog
                .pop_back()
                .map_or(Next::Idle, |job| Next::Send(self.sending(job)));
        };
        let due = (pending.changed + SETTLE).min(pending.since + REFRESH);
        if now < due {
            return match self.backlog.pop_back() {
                Some(job) => Next::Send(self.sending(job)),
                None => Next::At(due),
            };
        }
        let Some(pending) = self.interim.take() else {
            return Next::Idle;
        };
        Next::Send(self.sending(job(pending.key, pending.source, false, pending.speaker)))
    }

    /// `job` as it goes: with what was said since the last one and the brief when it changed.
    fn sending(&mut self, mut job: Job) -> Job {
        self.busy = true;
        job.said = std::mem::take(&mut self.said);
        if self.brief_sent.as_ref() != Some(&self.brief) && !self.brief.is_empty() {
            job.brief = Some(self.brief.clone());
            self.brief_sent = Some(self.brief.clone());
        }
        job
    }

    /// `job` came back as `text`.
    pub fn done(&mut self, job: Job, text: String) {
        self.busy = false;
        if !self.cache.contains_key(&job.source) {
            self.cached.push_back(job.source.clone());
            while self.cached.len() > CACHE {
                if let Some(old) = self.cached.pop_front() {
                    self.cache.remove(&old);
                }
            }
        }
        self.cache.insert(job.source.clone(), text.clone());
        // an interim answer coming after the final's never replaces it
        if self
            .shown
            .get(&job.key)
            .is_some_and(|shown| shown.final_result && !job.final_result)
        {
            return;
        }
        self.shown.insert(
            job.key,
            Shown {
                source: job.source,
                text,
                final_result: job.final_result,
            },
        );
    }

    /// The translation shown for `line`: `None` while it has none, or when it is spoken in the target language.
    #[must_use]
    pub fn text(&self, line: &Caption) -> Option<&str> {
        self.shown.get(&LineKey::of(line)).map(|shown| shown.text.as_str())
    }

    /// Whether `line` is spoken in the target language (shown as it is).
    #[must_use]
    pub fn in_target(&self, line: &Caption) -> bool {
        same_language(self.target, &line.track_language)
    }
}

fn job(key: LineKey, source: String, final_result: bool, speaker: Option<String>) -> Job {
    Job {
        key,
        source,
        final_result,
        speaker,
        said: Vec::new(),
        brief: None,
    }
}

#[cfg(test)]
#[path = "../../tests/support/translate_schedule_units.rs"]
mod tests;
