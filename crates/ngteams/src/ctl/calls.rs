//! The call commands, each asked through [`calls::ask_then`] as the call buttons ask: the calls' model decides what it
//! can (one call at a time, an invitation that rings, a call up), the engine checks it again and sends its requests
//! through the outbox. Nothing here builds a request, answers, declines or joins on its own, or turns a camera on: each
//! command is the user's ask.
//!
//! - `calls`: the call up and the invitation that rings, of every account (of the one addressed with `--account`).
//! - `call <chat>`, `join <chat>`: the chat named as the reads name it ([`address::chat`]), called or its running call
//!   joined; `answer` (a meeting's Request to join through its pre-join's Join now), `decline`: the invitation that
//!   rings; `hangup`: the call up left (or its ring cancelled).
//! - `mute on|off|toggle`, `camera on|off|toggle`: the microphone or the camera of the call up, asked only when it
//!   isn't as `on`/`off` says already. They are asked one at a time, each once the one before it was answered: the
//!   call shows a switch as it is only then, so one asked meanwhile would be compared with what it was before.
//!
//! **The account**: the one `--account` names, otherwise the one ringing, in a call or setting one up (calls are one
//! at a time across the accounts), otherwise the one the window shows. A start, a join or an answer while another
//! account is busy is refused ([`ErrorCode::Busy`]), as the shell refuses it.
//!
//! **The answer**: [`ErrorCode::NoCalling`] in an app that shows no calls (its devices couldn't be opened). An ask the
//! calls' model refused, or the engine did, is its error: [`ErrorCode::Busy`], [`ErrorCode::NoCall`],
//! [`ErrorCode::NotRinging`], [`ErrorCode::NoVideo`], or [`ErrorCode::Engine`] with what the call view would say. An
//! unmute a newer mute or unmute overtook while the microphone reopened ([`Answered::Superseded`]) is
//! [`ErrorCode::Engine`] saying so: the newer one decides, and the call view tells nothing of it.
//! Otherwise, once the engine took it and the app shows the call as it moved on (a start's or an answer's call told,
//! a declined or left one gone), the call's row ([`Response::Call`]; one that ended says `ended`). All of it within
//! [`CALL_ANSWER`], past which [`ErrorCode::Timeout`].

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, AsyncApp, Entity, WeakEntity};
use teams_core::CallId;
use teams_ctl::protocol::{CALL_ANSWER, CallRow, CtlError, ErrorCode, Request, Response, Toggle};
use teams_ui::calls::{self, Answered, Ask, Ended, Refused};
use teams_ui::{Accounts, Bridge, Calls, ViewTrace};
use tokio::sync::oneshot;

use super::address::{self, Addressed};
use super::dispatch::alive;
use super::writes::within;
use crate::app::Running;

/// Whether `request` is a call command (see the module docs).
#[must_use]
pub fn serves(request: &Request) -> bool {
    matches!(
        request,
        Request::Calls
            | Request::Call { .. }
            | Request::Answer
            | Request::Decline
            | Request::HangUp
            | Request::Mute { .. }
            | Request::Camera { .. }
            | Request::Join { .. }
    )
}

/// `request`, a call command, answered for `account` (see the module docs).
///
/// # Errors
/// The account or chat isn't one, the app shows no calls, the ask was refused (by the calls' model or the engine), or
/// the answer took longer than [`CALL_ANSWER`].
pub async fn answer(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    if let Request::Calls = request {
        return cx.update(|cx| listed(&alive(running, cx)?, account, cx));
    }
    let target = cx.update(|cx| target(&alive(running, cx)?, account, Fallback::Busy, cx))?;
    let executor = cx.background_executor().clone();
    let late = format!(
        "the call wasn't answered in {}s: `ngteamsctl calls` shows where it stands",
        CALL_ANSWER.as_secs()
    );
    within(CALL_ANSWER, &late, asked(&target, request, cx), &executor).await
}

/// The account a call command acts on, with what its ask needs.
pub(super) struct Target {
    pub account: Addressed,
    pub calls: Entity<Calls>,
    pub trace: Arc<ViewTrace>,
    /// The window's accounts: whether another one rings, is in a call or sets one up, read as the ask is made.
    pub accounts: Entity<Accounts>,
    /// Held by a `mute` or a `camera` from before its switch is read until it is answered.
    switches: Rc<tokio::sync::Mutex<()>>,
}

/// The account a command acts on when `--account` names none.
#[derive(Clone, Copy)]
pub(super) enum Fallback {
    /// The one ringing, in a call or setting one up, otherwise the one shown: a call command's.
    Busy,
    /// The one shown: a calendar event's join, whose event is the account's own.
    Shown,
}

/// The account `wanted` names, otherwise the one `fallback` says (see the module docs).
///
/// # Errors
/// The account isn't one, its engine stopped, or the app shows no calls ([`ErrorCode::NoCalling`]).
pub(super) fn target(
    running: &Entity<Running>,
    wanted: Option<&str>,
    fallback: Fallback,
    cx: &App,
) -> Result<Target, CtlError> {
    let running = running.read(cx);
    let busy = || {
        running
            .bridges()
            .iter()
            .find(|(_, _, bridge)| calls_of(bridge, cx).read(cx).busy())
            .map(|(mri, _, _)| mri.as_str())
    };
    let wanted = match fallback {
        Fallback::Busy => wanted.or_else(busy),
        Fallback::Shown => wanted,
    };
    let (mri, upn, bridge) = address::known(running, wanted, cx)?;
    let calls = calls_of(bridge, cx).clone();
    if !calls.read(cx).shows() {
        return Err(CtlError::new(
            ErrorCode::NoCalling,
            format!("{upn} has no calls here: the app's call devices couldn't be opened"),
        ));
    }
    let account = address::account(running, Some(mri), cx)?;
    Ok(Target {
        account,
        calls,
        trace: Arc::clone(running.trace()),
        accounts: running.desktop().read(cx).accounts().clone(),
        switches: Rc::clone(running.call_switches()),
    })
}

fn calls_of<'a>(bridge: &'a Entity<Bridge>, cx: &'a App) -> &'a Entity<Calls> {
    &bridge.read(cx).models().calls
}

/// `calls`: each account's call up and invitation ringing, or only the account `wanted` names.
fn listed(running: &Entity<Running>, wanted: Option<&str>, cx: &App) -> Result<Response, CtlError> {
    let running = running.read(cx);
    let accounts: Vec<(&String, &Entity<Bridge>)> = match wanted {
        Some(_) => {
            let (_, upn, bridge) = address::known(running, wanted, cx)?;
            vec![(upn, bridge)]
        }
        None => running.bridges().iter().map(|(_, upn, bridge)| (upn, bridge)).collect(),
    };
    let calls = accounts
        .into_iter()
        .flat_map(|(upn, bridge)| rows(upn, calls_of(bridge, cx).read(cx)))
        .collect();
    Ok(Response::Calls { calls })
}

/// Account `upn`'s call up, then the invitation that rings there.
#[must_use]
pub fn rows(upn: &str, calls: &Calls) -> Vec<CallRow> {
    up_row(upn, calls).into_iter().chain(ringing_row(upn, calls)).collect()
}

/// Account `upn`'s call up, or being set up.
#[must_use]
pub fn up_row(upn: &str, calls: &Calls) -> Option<CallRow> {
    calls.call().map(|call| CallRow {
        id: call.id.get().to_string(),
        account: upn.to_owned(),
        chat: call.chat.clone(),
        state: calls::status_name(&call.status).to_owned(),
        ringing: false,
        caller: call.caller.as_ref().map(|caller| caller.as_str().to_owned()),
        muted: call.muted,
        camera: call.camera,
    })
}

/// The invitation that rings at account `upn`.
#[must_use]
pub fn ringing_row(upn: &str, calls: &Calls) -> Option<CallRow> {
    calls.ringing().map(|ringing| CallRow {
        id: ringing.call.get().to_string(),
        account: upn.to_owned(),
        chat: ringing.group_chat.clone(),
        state: "ringing".to_owned(),
        ringing: true,
        caller: Some(ringing.caller.as_str().to_owned()),
        muted: false,
        camera: false,
    })
}

/// A call that ended, as its row tells it.
fn ended_row(upn: &str, ended: &Ended) -> CallRow {
    CallRow {
        id: ended.id.get().to_string(),
        account: upn.to_owned(),
        chat: ended.chat.clone(),
        state: calls::status_name(&teams_core::CallStatus::Ended(ended.end.clone())).to_owned(),
        ringing: false,
        caller: ended.caller.as_ref().map(|caller| caller.as_str().to_owned()),
        muted: false,
        camera: false,
    }
}

/// What a command waits for once the engine took its ask.
#[derive(Clone, Copy)]
pub(super) enum Settle {
    /// A start's or a join's call told (or gone already).
    Started,
    /// The invitation `0` answered: its call told.
    Answered(CallId),
    /// Call `0` gone: declined, or left.
    Gone(CallId),
    /// Nothing more: the model shows the change once the engine took it (mute, camera).
    Now,
}

/// The ask `request` stands for on `target`, asked and answered.
async fn asked(target: &Target, request: &Request, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    // a switch compared with what the one asked before it made, not what it was while that one was on its way
    let _switching = match request {
        Request::Mute { .. } | Request::Camera { .. } => Some(target.switches.lock().await),
        _ => None,
    };
    let core = &target.account.core;
    let ask = match request {
        Request::Call { chat } => Ask::Call {
            chat: address::chat(&**core, chat).await?.id,
        },
        Request::Join { chat } => Ask::Join {
            chat: address::chat(&**core, chat).await?.id,
        },
        Request::Answer => Ask::Answer,
        Request::Decline => Ask::Decline,
        Request::HangUp => Ask::HangUp,
        Request::Mute { op } => match cx.update(|cx| switched(target, *op, Switch::Microphone, cx)) {
            Some(answer) => return answer,
            None => Ask::ToggleMute,
        },
        Request::Camera { op } => match cx.update(|cx| switched(target, *op, Switch::Camera, cx)) {
            Some(answer) => return answer,
            None => Ask::ToggleCamera,
        },
        _ => return Err(CtlError::new(ErrorCode::BadRequest, "not a call command")),
    };
    let (settle, answered) = cx.update(|cx| ask_now(target, ask, cx))?;
    match answered.await.unwrap_or(Answered::Refused) {
        Answered::Taken => settled(target, settle, cx).await,
        Answered::Superseded => Err(superseded()),
        Answered::Refused => Err(cx.update(|cx| refusal(target.calls.read(cx).refused()))),
    }
}

/// Which switch of the call `mute` and `camera` set.
#[derive(Clone, Copy)]
enum Switch {
    Microphone,
    Camera,
}

/// The call's row when `op` asks for what the switch is already (nothing to ask); `None` when it is to be toggled. With
/// no call up, it is asked anyway, and refused as the button would be.
fn switched(target: &Target, op: Toggle, switch: Switch, cx: &App) -> Option<Result<Response, CtlError>> {
    let calls = target.calls.read(cx);
    let call = calls.call()?;
    let on = match switch {
        Switch::Microphone => call.muted,
        Switch::Camera => call.camera,
    };
    let wanted = match op {
        Toggle::On => true,
        Toggle::Off => false,
        Toggle::Toggle => return None,
    };
    (on == wanted).then(|| {
        up_row(&target.account.upn, calls)
            .map(Response::Call)
            .ok_or_else(no_call)
    })
}

/// `ask` asked of `target`'s calls, as the shell asks it: what its answer waits for, and how the engine answered it.
fn ask_now(target: &Target, ask: Ask, cx: &mut App) -> Result<(Settle, oneshot::Receiver<Answered>), CtlError> {
    let (told, taken) = oneshot::channel();
    let calls = &target.calls;
    let settle = {
        let calls = calls.read(cx);
        match &ask {
            Ask::Call { .. } | Ask::Join { .. } => Settle::Started,
            Ask::Answer => calls
                .ringing()
                .map_or(Settle::Now, |ringing| Settle::Answered(ringing.call)),
            Ask::Decline => calls
                .ringing()
                .map_or(Settle::Now, |ringing| Settle::Gone(ringing.call)),
            Ask::HangUp => calls.call().map_or(Settle::Now, |call| Settle::Gone(call.id)),
            _ => Settle::Now,
        }
    };
    // read now, as the shell reads it right before it asks: a chat's lookup ahead of it may have taken a while
    let enters = matches!(ask, Ask::Call { .. } | Ask::Join { .. } | Ask::Answer);
    if enters && target.accounts.read(cx).busy_elsewhere(&target.account.mri, cx) {
        calls::refuse_here(calls, &target.trace, &ask, "another-account", cx);
        return Err(refusal(calls.read(cx).refused()));
    }
    if ask == Ask::Answer && calls.read(cx).ring_opens_prejoin() {
        // a meeting's Request to join: its Accept opens the pre-join, whose Join now answers, as `--now` joins a meeting
        calls::accept_meeting_ring(calls, &target.account.core, &target.trace, cx);
        calls::join_now_then(calls, &target.account.core, &target.trace, cx, move |taken, _| {
            let _ = told.send(if taken { Answered::Taken } else { Answered::Refused });
        });
        return Ok((settle, taken));
    }
    calls::ask_then(
        calls,
        &target.account.core,
        &target.trace,
        ask,
        cx,
        move |answered, _| {
            // the command's connection may be gone meanwhile: nobody to tell
            let _ = told.send(answered);
        },
    );
    Ok((settle, taken))
}

/// The error of an unmute a newer mute or unmute overtook while the microphone reopened: not a refusal, so the calls
/// keep none (an earlier one may still be there, about something else).
fn superseded() -> CtlError {
    CtlError::new(
        ErrorCode::Engine,
        "a newer mute or unmute took this unmute's place while the microphone reopened: `ngteamsctl calls` shows the \
         microphone as that one left it",
    )
}

fn no_call() -> CtlError {
    CtlError::new(ErrorCode::NoCall, "no call is up")
}

/// The error of an ask that went nowhere, as the calls' model kept why.
pub(super) fn refusal(refused: Option<&Refused>) -> CtlError {
    let Some(refused) = refused else {
        return CtlError::new(ErrorCode::Engine, "the call action went nowhere");
    };
    let code = match refused.why {
        "busy" | "another-account" => ErrorCode::Busy,
        "no-call" | "not-joinable" => ErrorCode::NoCall,
        "not-ringing" => ErrorCode::NotRinging,
        "no-video" => ErrorCode::NoVideo,
        "no-calls" | "no-calling" => ErrorCode::NoCalling,
        "stopped" => ErrorCode::Stopped,
        "not-told" => ErrorCode::Timeout,
        _ => ErrorCode::Engine,
    };
    CtlError::new(code, refused.text.clone())
}

/// The answer once `target`'s calls show what `settle` waits for (see the module docs).
pub(super) async fn settled(target: &Target, settle: Settle, cx: &mut AsyncApp) -> Result<Response, CtlError> {
    let upn = target.account.upn.clone();
    let shown = move |calls: &Calls| -> Option<Result<Response, CtlError>> {
        let up = || up_row(&upn, calls).map(|row| Ok(Response::Call(row)));
        let ended = |id: CallId| {
            calls
                .ended()
                .filter(|ended| ended.id == id)
                .map_or(Ok(Response::Done), |ended| Ok(Response::Call(ended_row(&upn, ended))))
        };
        match settle {
            Settle::Now => up().or(Some(Ok(Response::Done))),
            Settle::Started => {
                if calls.call().is_some() {
                    return up();
                }
                // the start went nowhere after all, or its call ended before it was read here
                (!calls.busy()).then(|| match calls.ended() {
                    Some(ended) => Ok(Response::Call(ended_row(&upn, ended))),
                    None => Err(refusal(calls.refused())),
                })
            }
            Settle::Answered(id) => {
                if calls.call().is_some_and(|call| call.id == id) {
                    return up();
                }
                calls
                    .ringing()
                    .is_none_or(|ringing| ringing.call != id)
                    .then(|| ended(id))
            }
            Settle::Gone(id) => {
                let in_call = calls.call().is_some_and(|call| call.id == id);
                let rings = calls.ringing().is_some_and(|ringing| ringing.call == id);
                (!in_call && !rings).then(|| ended(id))
            }
        }
    };
    let (done, finished) = async_channel::bounded::<Result<Response, CtlError>>(1);
    let (already, _observing) = cx.update(|cx| {
        if let Some(answer) = shown(target.calls.read(cx)) {
            return (Some(answer), None);
        }
        let shown = shown.clone();
        let observing = cx.observe(&target.calls, move |calls, cx| {
            if let Some(answer) = shown(calls.read(cx)) {
                let _ = done.try_send(answer);
            }
        });
        (None, Some(observing))
    });
    if let Some(answer) = already {
        return answer;
    }
    // bounded by the command's own wait ([`CALL_ANSWER`])
    finished
        .recv()
        .await
        .unwrap_or_else(|_| Err(CtlError::new(ErrorCode::Stopped, "the account's calls are gone")))
}
