//! The organizer's controls of a group call or a meeting, as Teams' People and Controls offer them (seen 2026-11-06,
//! `docs/protocol/calling.md`, "More of a running call"): Mute all ([`CallAction::MuteAll`], the conversation's `mute`
//! through the machine), and the meeting's options read and changed on the middle tier (Controls' Enable lobby and
//! Allow all participants to: [`EngineRequests::call_meeting_options`] reads them, [`CallAction::MeetingOption`]
//! changes one, one attempt, the outbox's retries only). And what binds the account when someone else organizes:
//! Mute all's mute, which the account takes as its own (the microphone silent; nothing told, as the web client tells
//! nothing), and the attendees' restrictions, which the view shows.
//!
//! [`CallAction::MuteAll`]: super::CallAction::MuteAll
//! [`CallAction::MeetingOption`]: super::CallAction::MeetingOption
//! [`EngineRequests::call_meeting_options`]: crate::EngineRequests::call_meeting_options

use teams_proto::{MeetingOption, MeetingOptions};

use crate::engine::Responder;
#[cfg(feature = "calls")]
use {
    super::{Act, CallActionError, CallError, CallRetries, CallTried, Calling, Waiting},
    crate::EngineError,
    std::sync::Arc,
    teams_call::{CallId, MeetingState},
    teams_proto::{Credentials, HttpError, MeetingOptionsRead, SetMeetingOption, Transport},
};

/// What is asked of a call's meeting options: their read (answered to its own responder), or a change.
#[cfg_attr(
    not(feature = "calls"),
    expect(dead_code, reason = "without call media no calling reads a meeting option")
)]
pub(crate) enum Options {
    Read(Responder<Option<MeetingOptions>>),
    Change(MeetingOption),
}

#[cfg(feature = "calls")]
impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// What goes beside the call's signaling, answered once it is done: the call's health read, its meeting options
    /// read, or one of them changed (its answer to `reply`; the reads' to their own responders).
    pub(super) fn aside(&mut self, act: Act, reply: Waiting) {
        match act {
            Act::Health(call, health) => self.health(call, health),
            Act::Options(call, Options::Read(options)) => self.meeting_options(call, options),
            Act::Options(call, Options::Change(option)) => self.set_meeting_option(call, option, reply),
            _ => {
                let _ = reply.send(Err(EngineError::from(CallActionError::Call(CallError::NotNow))));
            }
        }
    }

    /// Answers `reply` with the meeting options of `call`, read from the middle tier now; `None` unless the account
    /// organizes the call (in a group chat or a meeting's) and the read came back.
    pub(super) fn meeting_options(&self, call: CallId, reply: Responder<Option<MeetingOptions>>) {
        let Some(of) = self.calls.options_of(call) else {
            let _ = reply.send(Ok(None));
            return;
        };
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let options = match client.read(&MeetingOptionsRead(of)).await {
                Ok(answer) if (200..300).contains(&answer.status()) => MeetingOptions::read_answer(answer.body()),
                Ok(answer) => {
                    log::info!(
                        "call {}: its meeting options read answered {}",
                        call.get(),
                        answer.status()
                    );
                    None
                }
                Err(error) => {
                    log::info!("call {}: its meeting options weren't read: {error}", call.get());
                    None
                }
            };
            let _ = reply.send(Ok(options));
        });
    }

    /// One of `call`'s meeting options changed (Controls): answered once the middle tier took it, or why not.
    pub(super) fn set_meeting_option(&self, call: CallId, option: MeetingOption, reply: Waiting) {
        let Some(of) = self.calls.options_of(call) else {
            log::info!(
                "call {}: no meeting option changes: {}",
                call.get(),
                self.calls.options_missing(call)
            );
            let _ = reply.send(Err(EngineError::from(CallActionError::Call(CallError::NotNow))));
            return;
        };
        let outbox = Arc::clone(&self.outbox);
        let write = SetMeetingOption::new(of, option);
        tokio::task::spawn_local(async move {
            let mut retries = CallRetries::default();
            let taken = loop {
                let tried = outbox.try_meeting(write.clone()).await;
                if let Some(wait) = retries.again(&tried) {
                    tokio::time::sleep(wait).await;
                    continue;
                }
                break match tried {
                    CallTried::Answered(answer) if (200..300).contains(&answer.status()) => true,
                    CallTried::Answered(answer) => {
                        log::info!(
                            "call {}: its meeting option was refused: {}",
                            call.get(),
                            answer.status()
                        );
                        false
                    }
                    other => {
                        log::info!("call {}: its meeting option didn't go: {}", call.get(), other.summary());
                        false
                    }
                };
            };
            let answer = if taken {
                Ok(call)
            } else {
                Err(EngineError::from(CallActionError::NotChanged))
            };
            let _ = reply.send(answer);
        });
    }

    /// The meeting's state told: when the organizer muted the account anew (Mute all), its microphone goes silent as
    /// its own mute would, and nothing is told (the web client reported nothing).
    pub(super) fn muted_by_organizer(&mut self, call: CallId, state: &MeetingState) {
        let Some(version) = state.muted_by_organizer else {
            return;
        };
        let before = self.organizer_mute.filter(|(of, _)| *of == call).map(|(_, told)| told);
        if before.is_some_and(|told| told >= version) {
            return;
        }
        self.organizer_mute = Some((call, version));
        let Some(live) = self.live.as_ref().filter(|live| live.call == call) else {
            return;
        };
        self.mute_generation = self.mute_generation.wrapping_add(1);
        match self.held_mute.as_mut().filter(|(of, _)| *of == call) {
            Some((_, wanted)) => *wanted = true,
            None => live.session.set_muted(true),
        }
        log::info!("call {}: muted by its organizer", call.get());
    }
}
