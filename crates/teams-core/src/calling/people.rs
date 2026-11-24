//! Someone added to the account's call, as Teams' People pane adds them ("Invite someone", seen
//! 2026-11-06): in a 1:1 call a new group chat of the three is made first (`POST v1/threads`, the outbox's typed
//! [`CreateChat`], no topic), then the conversation's `add` names it and the call becomes a group call in it; in a
//! group call the call's own chat is named at once. The chat's making is one write of its own beside the call's. In a
//! meeting, Request to join on someone it invited is the same `add`, naming the meeting's chat and organizer (seen
//! 2026-11-24).
//!
//! A transfer the far end asked for is taken by the machine (`teams-call`); the call to the transfer's target is placed
//! here once the transferred call is over and its media gone, as any start is (its media set up first), as the web
//! client calls the target at once (seen 2026-11-06). Nobody but the far end's transfer asks for that call.

use std::future::pending;

use teams_call::Placement;
use teams_proto::{CreateChat, Response, Transferor};
use tokio::sync::oneshot;

use super::{
    Act, Attempt, CallActionError, CallError, CallId, CallTried, Calling, Credentials, Event, HttpError, Mri, Transport,
};

/// A 1:1 call's new group chat being made for `invitee`.
pub(super) struct Making {
    call: CallId,
    invitee: Mri,
    attempt: Attempt,
}

/// The chat being made, once its write came back; never while none is.
pub(super) async fn made(making: Option<&mut Making>) -> (CallId, Mri, CallTried) {
    match making {
        Some(making) => {
            let tried = (&mut making.attempt).await;
            (making.call, making.invitee.clone(), tried)
        }
        None => pending().await,
    }
}

/// The new chat's thread id, as its answer's `Location` names it (`…/v1/threads/<id>`).
fn thread_of(answer: &Response) -> Option<String> {
    let location = answer.location()?;
    let path = location.split(['?', '#']).next()?;
    let id = path.rsplit('/').next()?;
    let id = id.replace("%3A", ":").replace("%3a", ":").replace("%40", "@");
    teams_proto::is_group_chat_id(&id).then_some(id)
}

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// Someone added to the call, or its far end transferred.
    pub(super) fn people_act(&mut self, act: Act) -> Result<CallId, CallActionError> {
        match act {
            Act::Add(call, invitee) => self.add_person(call, invitee),
            Act::Transfer(call, target) => self
                .calls
                .transfer(call, &target)
                .map(|()| call)
                .map_err(CallActionError::from),
            _ => Err(CallError::NotNow.into()),
        }
    }

    /// `invitee` added to `call`: at once in a group call's chat or a meeting's (Request to join); a 1:1 call's new group
    /// chat of the three made first.
    pub(super) fn add_person(&mut self, call: CallId, invitee: Mri) -> Result<CallId, CallActionError> {
        if let Some(chat) = self.calls.group_chat(call) {
            self.calls.add(call, &invitee, &chat)?;
            return Ok(call);
        }
        if self.calls.meeting_chat(call).is_some() {
            self.calls.add_to_meeting(call, &invitee)?;
            return Ok(call);
        }
        if self.making.is_some() {
            return Err(CallError::NotNow.into());
        }
        let far_end = self.calls.far_end(call).ok_or(CallError::NotNow)?;
        if invitee == far_end || invitee == *self.calls.me() {
            return Err(CallActionError::NotAChat);
        }
        let chat =
            CreateChat::group(self.calls.me().clone(), [far_end, invitee.clone()]).ok_or(CallActionError::NotAChat)?;
        let outbox = std::sync::Arc::clone(&self.outbox);
        log::info!("call {}: a group chat is made for someone added to it", call.get());
        self.making = Some(Making {
            call,
            invitee,
            attempt: Box::pin(async move { outbox.try_call(chat).await }),
        });
        Ok(call)
    }

    /// The new group chat for someone added to `call` made (`tried`): the `add` goes, naming it; a chat not made
    /// brings nobody in, told as the add's failure.
    pub(super) fn chat_made(&mut self, call: CallId, invitee: Mri, tried: &CallTried) {
        self.making = None;
        let chat = match tried {
            CallTried::Answered(answer) if (200..300).contains(&answer.status()) => thread_of(answer),
            _ => None,
        };
        let added = chat
            .ok_or(CallError::NotNow)
            .and_then(|chat| self.calls.add(call, &invitee, &chat));
        if let Err(error) = added {
            log::warn!("call {}: someone couldn't be added: {error}", call.get());
            self.emit(Event::CallAddFailed {
                call,
                who: invitee,
                declined: false,
            });
        }
    }
}

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// The far end transferred `call` to `target` and the machine took it: told, and the call to the target goes once
    /// the transferred call's end is handled (its media closed).
    pub(super) fn transferred(&mut self, call: CallId, target: Mri, transferor: Transferor) {
        log::info!("call {}: transferred by its far end", call.get());
        self.emit(Event::CallTransferred {
            call,
            target: target.clone(),
            by: transferor.display_name().map(str::to_owned),
            chat: teams_proto::one_to_one_id(self.calls.me().as_str(), target.as_str()),
        });
        self.transfer_to = Some((call, Placement::Transferred { target, transferor }));
    }

    /// The transferred `call` is over here: the call to its transfer's target goes now, set up as any start is.
    pub(super) fn call_transfer_target(&mut self, call: CallId) {
        let Some((_, placement)) = self.transfer_to.take_if(|(of, _)| *of == call) else {
            return;
        };
        let (reply, _) = oneshot::channel();
        self.set_up(Act::Place(placement), reply);
    }
}
