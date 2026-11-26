//! A recording or a transcription the account starts or stops (More → Record and transcribe,
//! `docs/protocol/calling.md`, "More of a running call"): Microsoft's recording bot is invited into the call when it
//! isn't there yet (as live captions invite it), then its command link is told to start recording (with its
//! transcript unless a transcription runs already), to start the transcription alone, or to stop either. Each write is
//! one attempt, never resent: a start whose outcome is unknown is told as failed, and the roster shows what runs
//! ([`crate::MeetingState::recording`], [`crate::MeetingState::transcribing`]). Nothing here enters the persistent
//! outbox.
use teams_proto::captions::{RecordCommand, RecordStart};

use super::{Act, CallAction, CallActionError};
#[cfg(feature = "calls")]
use {
    super::{CallId, CallTried, Calling},
    crate::engine::Event,
    std::future::pending,
    std::time::{Duration, Instant},
    teams_proto::captions::CaptionWrite,
    teams_proto::{Credentials, HttpError, Transport},
};

/// How long a start may take, the recording bot's joining included, before it is told failed: about as long as the
/// web client keeps trying (it tried a start whose request went unanswered every 20 s for 3 min 20 s before it gave up,
/// `docs/protocol/calling.md`), as the bot took 15 to 27 s to join in the lab.
#[cfg(feature = "calls")]
const JOIN_WITHIN: Duration = Duration::from_secs(200);

/// What the account's own recording or transcription action is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordingPhase {
    /// Nothing asked, or what was asked is done (the roster tells what runs).
    #[default]
    Idle,
    /// Start recording or Start transcription asked: the bot being invited, or its start sent.
    Starting,
    /// Stop recording or Stop transcription asked and sent.
    Stopping,
}

/// Why the account's start or stop didn't happen, in Teams' words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingProblem {
    pub heading: &'static str,
    pub detail: &'static str,
}

/// The account's recording action in its call ([`Event::CallRecording`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallRecording {
    pub phase: RecordingPhase,
    /// The last start or stop that failed, until the next is asked.
    pub problem: Option<RecordingProblem>,
}

/// Why a start or a stop failed, for a recording and for a transcription alone.
#[cfg(feature = "calls")]
struct Problems {
    not_for_this_call: RecordingProblem,
    something_wrong: RecordingProblem,
    /// The start took longer than [`JOIN_WITHIN`].
    timed_out: RecordingProblem,
    policy: RecordingProblem,
    pending: RecordingProblem,
    stop_failed: RecordingProblem,
}

#[cfg(feature = "calls")]
const START_FAILED: &str = "Couldn't start recording";
/// What the web client says once it gave up on a start that went unanswered.
#[cfg(feature = "calls")]
const TIMED_OUT: &str = "Please try again. Contact your IT admin if this keeps happening.";
#[cfg(feature = "calls")]
const RECORDING: Problems = Problems {
    not_for_this_call: RecordingProblem {
        heading: START_FAILED,
        detail: "Recording cannot be started for this call.",
    },
    something_wrong: RecordingProblem {
        heading: START_FAILED,
        detail: "Something went wrong. Try to start recording again.",
    },
    timed_out: RecordingProblem {
        heading: "Couldn't start recording and transcription.",
        detail: TIMED_OUT,
    },
    policy: RecordingProblem {
        heading: START_FAILED,
        detail: "Recording not started. Your IT admin has disabled Recording.",
    },
    pending: RecordingProblem {
        heading: START_FAILED,
        detail: "Recording couldn't be started because another recording is pending.",
    },
    stop_failed: RecordingProblem {
        heading: "Couldn't stop recording and transcription.",
        detail: "Recording cannot be stopped for this call.",
    },
};

// no capture showed a transcription's failure: its words follow the recording's
#[cfg(feature = "calls")]
const TRANSCRIBE_FAILED: &str = "Couldn't start transcription";
#[cfg(feature = "calls")]
const TRANSCRIPTION: Problems = Problems {
    not_for_this_call: RecordingProblem {
        heading: TRANSCRIBE_FAILED,
        detail: "Transcription cannot be started for this call.",
    },
    something_wrong: RecordingProblem {
        heading: TRANSCRIBE_FAILED,
        detail: "Something went wrong. Try to start transcription again.",
    },
    timed_out: RecordingProblem {
        heading: TRANSCRIBE_FAILED,
        detail: TIMED_OUT,
    },
    policy: RecordingProblem {
        heading: TRANSCRIBE_FAILED,
        detail: "Transcription not started. Your IT admin has disabled Transcription.",
    },
    pending: RecordingProblem {
        heading: TRANSCRIBE_FAILED,
        detail: "Transcription couldn't be started because another transcription is pending.",
    },
    stop_failed: RecordingProblem {
        heading: "Couldn't stop transcription.",
        detail: "Transcription cannot be stopped for this call.",
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "calls")]
pub(super) enum Operation {
    Invite,
    Start,
    Stop,
}

#[derive(Default)]
#[cfg(feature = "calls")]
pub(super) struct Recordings {
    call: Option<CallId>,
    view: CallRecording,
    pub(super) writing: Option<(Operation, super::Attempt)>,
    /// The start asked, until it is sent.
    start: Option<RecordCommand>,
    /// What was asked is about the transcription alone: its failure says so.
    transcription: bool,
    deadline: Option<Instant>,
}

#[cfg(feature = "calls")]
impl Recordings {
    fn problems(&self) -> &'static Problems {
        if self.transcription { &TRANSCRIPTION } else { &RECORDING }
    }
}

#[cfg(feature = "calls")]
pub(super) async fn next(writing: Option<&mut (Operation, super::Attempt)>) -> (Operation, CallTried) {
    match writing {
        Some((operation, attempt)) => (*operation, attempt.await),
        None => pending().await,
    }
}

/// What the calling does for a recording's or a transcription's start or stop: a start's file name has its start in
/// local time, as the web client names it.
///
/// # Errors
/// [`CallActionError::Call`] for any other action (never asked).
pub(crate) fn record_act(action: CallAction) -> Result<Act, CallActionError> {
    let stamp = || chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    Ok(match action {
        CallAction::Record {
            call,
            title,
            organizer,
            language,
            audio_only,
        } => Act::Record(
            call,
            RecordCommand::Record(RecordStart {
                title,
                organizer,
                language,
                stamp: stamp(),
                audio_only,
            }),
        ),
        CallAction::Transcribe {
            call,
            title,
            organizer,
            language,
        } => Act::Record(
            call,
            RecordCommand::Transcribe(RecordStart {
                title,
                organizer,
                language,
                stamp: stamp(),
                audio_only: false,
            }),
        ),
        CallAction::StopRecording { call } => Act::Record(call, RecordCommand::StopRecording),
        CallAction::StopTranscription { call } => Act::Record(call, RecordCommand::StopTranscription),
        _ => return Err(teams_call::CallError::NotNow.into()),
    })
}

#[cfg(feature = "calls")]
impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    fn record_emit(&self) {
        if let Some(call) = self.recording.call {
            self.emit(Event::CallRecording {
                call,
                recording: self.recording.view,
            });
        }
    }

    /// Starts or stops a recording or a transcription as `command` asks.
    pub(super) fn record(&mut self, call: CallId, command: RecordCommand) -> Result<CallId, CallActionError> {
        if self.up != Some(call)
            || (self.recording.call == Some(call) && self.recording.view.phase != RecordingPhase::Idle)
        {
            return Err(teams_call::CallError::NotNow.into());
        }
        let transcription = command.transcription();
        if !command.starts() {
            let write = self.calls.record_command(call, &command)?;
            self.recording = Recordings {
                call: Some(call),
                view: CallRecording {
                    phase: RecordingPhase::Stopping,
                    problem: None,
                },
                transcription,
                ..Recordings::default()
            };
            self.record_send(Operation::Stop, write);
            self.record_emit();
            return Ok(call);
        }
        self.recording = Recordings {
            call: Some(call),
            view: CallRecording {
                phase: RecordingPhase::Starting,
                problem: None,
            },
            start: Some(command),
            transcription,
            deadline: Some(Instant::now() + JOIN_WITHIN),
            ..Recordings::default()
        };
        self.record_emit();
        if self.calls.caption_service(call).is_some() {
            self.record_service_changed(call);
        } else if self.captions.writing.is_none() {
            match self.calls.caption_invite(call) {
                Ok(write) => self.record_send(Operation::Invite, write),
                Err(_) => self.record_fail(self.recording.problems().not_for_this_call),
            }
        }
        // else live captions are inviting the bot already: its roster entry starts the recording
        Ok(call)
    }

    fn record_send(&mut self, operation: Operation, write: CaptionWrite) {
        let outbox = self.outbox.clone();
        self.recording.writing = Some((operation, Box::pin(async move { outbox.try_caption(write).await })));
    }

    /// The recording bot's roster entry changed: a start waiting for it is sent.
    pub(super) fn record_service_changed(&mut self, call: CallId) {
        if self.recording.call != Some(call)
            || self.recording.view.phase != RecordingPhase::Starting
            || self.recording.writing.is_some()
            || self.calls.caption_service(call).is_none()
        {
            return;
        }
        let Some(start) = self.recording.start.take() else {
            return;
        };
        match self.calls.record_command(call, &start) {
            Ok(write) => self.record_send(Operation::Start, write),
            Err(_) => self.record_fail(self.recording.problems().not_for_this_call),
        }
    }

    pub(super) fn record_written(&mut self, operation: Operation, tried: &CallTried) {
        self.recording.writing = None;
        let status = match tried {
            CallTried::Answered(answer) => Some(answer.status()),
            _ => None,
        };
        if !status.is_some_and(|status| (200..300).contains(&status)) {
            let problems = self.recording.problems();
            let problem = match (operation, status) {
                (Operation::Stop, _) => problems.stop_failed,
                (Operation::Invite, _) => problems.not_for_this_call,
                (Operation::Start, Some(401 | 403)) => problems.policy,
                (Operation::Start, Some(409)) => problems.pending,
                (Operation::Start, _) => problems.something_wrong,
            };
            self.record_fail(problem);
            return;
        }
        match operation {
            Operation::Invite => {
                // its roster entry may have come before the answer
                if let Some(call) = self.recording.call {
                    self.record_service_changed(call);
                }
            }
            Operation::Start | Operation::Stop => {
                self.recording.deadline = None;
                self.recording.view.phase = RecordingPhase::Idle;
                self.record_emit();
            }
        }
    }

    /// The recording bot couldn't join (its `addParticipantFailure`): a start waiting for it fails.
    pub(super) fn record_join_failed(&mut self, call: CallId) {
        if self.recording.call == Some(call) && self.recording.view.phase == RecordingPhase::Starting {
            self.record_fail(self.recording.problems().not_for_this_call);
        }
    }

    fn record_fail(&mut self, problem: RecordingProblem) {
        self.recording.writing = None;
        self.recording.start = None;
        self.recording.deadline = None;
        self.recording.view = CallRecording {
            phase: RecordingPhase::Idle,
            problem: Some(problem),
        };
        self.record_emit();
    }

    pub(super) fn record_check(&mut self, now: Instant) {
        if self.recording.writing.is_none() && self.recording.deadline.is_some_and(|deadline| deadline <= now) {
            self.record_fail(self.recording.problems().timed_out);
        }
    }

    pub(super) fn record_ended(&mut self, call: CallId) {
        if self.recording.call == Some(call) {
            self.recording = Recordings::default();
        }
    }
}
