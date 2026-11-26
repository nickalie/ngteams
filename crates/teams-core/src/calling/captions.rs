//! Per-viewer ephemeral caption state. No speech or service credentials enter the persistent outbox.
#[cfg(feature = "calls")]
use {
    super::{CallActionError, CallId, CallTried, Calling},
    crate::engine::Event,
    std::future::pending,
    std::time::{Duration, Instant},
    teams_proto::captions::CaptionWrite,
    teams_proto::{Credentials, HttpError, Transport},
};

#[cfg(not(feature = "calls"))]
use super::Caption;
#[cfg(feature = "calls")]
pub use teams_media::captions::Caption;

/// Local caption visibility and service readiness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptionPhase {
    #[default]
    Off,
    Starting,
    On,
    Failed,
}

/// The current viewer's captions; reset when hidden or when the call ends.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallCaptions {
    pub phase: CaptionPhase,
    pub spoken_language: String,
    /// A short user-facing service failure, containing no response bodies, URLs or speech.
    pub error: Option<String>,
    pub lines: Vec<Caption>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "calls")]
pub(super) enum Operation {
    Invite,
    Start,
    Language,
}

#[derive(Default)]
#[cfg(feature = "calls")]
pub(super) struct Captions {
    call: Option<CallId>,
    view: CallCaptions,
    pub(super) writing: Option<(Operation, super::Attempt)>,
    waiting: bool,
    deadline: Option<Instant>,
}

#[cfg(feature = "calls")]
pub(super) async fn next(writing: Option<&mut (Operation, super::Attempt)>) -> (Operation, CallTried) {
    match writing {
        Some((operation, attempt)) => (*operation, attempt.await),
        None => pending().await,
    }
}

#[cfg(feature = "calls")]
impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    fn caption_emit(&self) {
        if let Some(call) = self.captions.call {
            self.emit(Event::CallCaptions {
                call,
                captions: self.captions.view.clone(),
            });
        }
    }

    pub(super) fn caption_toggle(&mut self, call: CallId, enabled: bool) -> Result<CallId, CallActionError> {
        if self.up != Some(call) {
            return Err(teams_call::CallError::NotNow.into());
        }
        // Hiding is local: other viewers' caption service must keep running.
        if !enabled {
            self.captions = Captions {
                call: Some(call),
                ..Captions::default()
            };
            self.caption_emit();
            return Ok(call);
        }
        if self.captions.call == Some(call)
            && matches!(self.captions.view.phase, CaptionPhase::Starting | CaptionPhase::On)
        {
            return Ok(call);
        }
        self.captions = Captions {
            call: Some(call),
            view: CallCaptions {
                phase: CaptionPhase::Starting,
                ..CallCaptions::default()
            },
            deadline: Some(Instant::now() + Duration::from_secs(60)),
            ..Captions::default()
        };
        self.caption_emit();
        if self.calls.caption_service(call).is_some() {
            self.caption_service_changed(call);
        } else {
            match self.calls.caption_invite(call) {
                Ok(write) => self.caption_send(Operation::Invite, write),
                Err(_) => self.caption_fail("Live captions are unavailable for this call. Try again."),
            }
        }
        Ok(call)
    }

    pub(super) fn caption_language(&mut self, call: CallId, language: &str) -> Result<CallId, CallActionError> {
        if self.captions.call != Some(call)
            || self.captions.view.phase != CaptionPhase::On
            || self.captions.writing.is_some()
        {
            return Err(teams_call::CallError::NotNow.into());
        }
        let write = self.calls.caption_command(call, Some(language))?;
        self.captions.view.error = None;
        self.caption_send(Operation::Language, write);
        Ok(call)
    }

    fn caption_send(&mut self, operation: Operation, write: CaptionWrite) {
        let outbox = self.outbox.clone();
        self.captions.writing = Some((operation, Box::pin(async move { outbox.try_caption(write).await })));
    }

    pub(super) fn caption_service_changed(&mut self, call: CallId) {
        if self.captions.call != Some(call) || self.captions.view.phase == CaptionPhase::Off {
            return;
        }
        let Some(service) = self.calls.caption_service(call) else {
            self.caption_fail("The live caption service stopped. Try again.");
            return;
        };
        self.captions.view.spoken_language.clone_from(&service.spoken_language);
        if service.state == "Active" {
            self.captions.view.phase = CaptionPhase::On;
            self.captions.view.error = None;
            self.captions.deadline = None;
            self.captions.waiting = false;
        } else if self.captions.view.phase == CaptionPhase::On {
            self.caption_fail("The live caption service stopped. Try again.");
        } else if self.captions.view.phase == CaptionPhase::Starting
            && self.captions.writing.is_none()
            && !self.captions.waiting
        {
            match self.calls.caption_command(call, None) {
                Ok(write) => self.caption_send(Operation::Start, write),
                Err(_) => self.caption_fail("Live captions could not start. Try again."),
            }
        }
        self.caption_emit();
    }

    pub(super) fn caption_written(&mut self, operation: Operation, tried: &CallTried) {
        self.captions.writing = None;
        let success = matches!(tried, CallTried::Answered(answer) if (200..300).contains(&answer.status()));
        if !success {
            if operation == Operation::Language {
                self.captions.view.error = Some("The meeting spoken language could not be changed. Try again.".into());
                self.caption_emit();
            } else {
                let message = match tried {
                    CallTried::Answered(answer) if matches!(answer.status(), 401 | 403) => {
                        "Live captions are not allowed by this account or meeting policy."
                    }
                    CallTried::Answered(answer) if matches!(answer.status(), 429 | 503) => {
                        "The live caption service is busy. Try again."
                    }
                    _ => "Live captions could not start. Check your connection and try again.",
                };
                self.caption_fail(message);
            }
            return;
        }
        if operation == Operation::Start {
            self.captions.waiting = true;
        }
        if operation == Operation::Invite {
            // A roster callback can arrive before the HTTP response.
            if let Some(call) = self
                .captions
                .call
                .filter(|call| self.calls.caption_service(*call).is_some())
            {
                self.caption_service_changed(call);
            }
        }
    }

    pub(super) fn caption_fail(&mut self, message: &str) {
        if self.captions.view.phase == CaptionPhase::Off {
            return;
        }
        self.captions.writing = None;
        self.captions.waiting = false;
        self.captions.deadline = None;
        self.captions.view.phase = CaptionPhase::Failed;
        self.captions.view.error = Some(message.into());
        self.caption_emit();
    }

    pub(super) fn caption_check(&mut self, now: Instant) {
        if self.captions.deadline.is_some_and(|deadline| deadline <= now) {
            // what it waited for, so that an `info` log tells a timeout's cause
            let waiting = match (self.captions.writing.as_ref(), self.captions.call) {
                (Some((operation, _)), _) => format!("the answer to its {operation:?}"),
                (None, Some(call)) => match self.calls.caption_service(call) {
                    None => "the caption service to join the call".to_owned(),
                    Some(service) => format!("the service's captions to turn on (they are {})", service.state),
                },
                (None, None) => "nothing".to_owned(),
            };
            log::info!("captions took too long to start: waited for {waiting}");
            self.caption_fail("Live captions took too long to start. Try again.");
        }
    }

    pub(super) fn caption_received(&mut self, call: CallId, lines: Vec<Caption>) {
        if self.captions.call != Some(call)
            || !matches!(self.captions.view.phase, CaptionPhase::On | CaptionPhase::Starting)
        {
            return;
        }
        if self.calls.caption_service(call).is_none() {
            return;
        }
        if self.captions.view.phase == CaptionPhase::Starting {
            self.captions.view.phase = CaptionPhase::On;
            self.captions.deadline = None;
        }
        for line in lines {
            update(&mut self.captions.view.lines, line);
        }
        self.caption_emit();
    }

    pub(super) fn caption_ended(&mut self, call: CallId) {
        if self.captions.call == Some(call) {
            self.captions = Captions {
                call: Some(call),
                ..Captions::default()
            };
            self.caption_emit();
            self.captions.call = None;
        }
    }
}

#[cfg(feature = "calls")]
fn update(lines: &mut Vec<Caption>, line: Caption) {
    if let Some(old) = lines.iter_mut().find(|old| {
        (old.id == line.id || (line.audio_timestamp != 0 && old.audio_timestamp == line.audio_timestamp))
            && old.speaker == line.speaker
            && old.track_language == line.track_language
    }) {
        if (!line.final_result && (old.final_result || old.duration > line.duration)) || older_result(&line.id, &old.id)
        {
            return;
        }
        *old = line;
    } else {
        lines.push(line);
        if lines.len() > 50 {
            lines.remove(0);
        }
    }
}

#[cfg(feature = "calls")]
fn older_result(next: &str, previous: &str) -> bool {
    let numbered = |value: &str| {
        value
            .rsplit_once('/')
            .and_then(|(session, number)| number.parse::<u64>().ok().map(|number| (session.to_owned(), number)))
    };
    matches!((numbered(next), numbered(previous)), (Some((a, next)), Some((b, previous))) if a == b && next < previous)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(id: &str, text: &str, final_result: bool, duration: u64) -> Caption {
        Caption {
            id: id.into(),
            speaker: Some("synthetic-speaker".into()),
            display_name: Some("Lab Speaker".into()),
            text: text.into(),
            final_result,
            duration,
            spoken_language: "en-us".into(),
            track_language: "en-us".into(),
            audio_timestamp: 123,
        }
    }
    #[test]
    fn interim_updates_one_utterance_and_final_never_regresses() {
        let mut lines = Vec::new();
        update(&mut lines, line("synthetic/1", "A", false, 30));
        update(&mut lines, line("synthetic/3", "A synthetic sentence.", true, 20));
        update(&mut lines, line("synthetic/2", "A synthetic", false, 40));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "A synthetic sentence.");
        assert!(lines[0].final_result);
        assert!(!format!("{lines:?}").contains("synthetic sentence"));
    }
    #[test]
    fn speaker_and_track_are_separate_and_history_is_bounded() {
        let mut lines = Vec::new();
        let first = line("one", "Synthetic.", true, 10);
        update(&mut lines, first.clone());
        let mut other = first;
        other.speaker = Some("other-synthetic".into());
        update(&mut lines, other);
        assert_eq!(lines.len(), 2);
        for index in 0..60 {
            let mut next = line(&index.to_string(), "Synthetic.", true, 10);
            next.audio_timestamp = 1000 + index;
            update(&mut lines, next);
        }
        assert_eq!(lines.len(), 50);
        assert_eq!(lines[0].id, "10");
    }
}
