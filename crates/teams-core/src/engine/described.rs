//! The engine's actions and events as the app's diagnostic log names them (M2 Task 24a): by kind, conversation and
//! message ids, outbox rows by id, state and `clientmessageid`; never a message's text, a name or a body. A message
//! event is at `info` when the push channel brought it, at `debug` when a read did (a catch-up stores many).

use log::{Level, LevelFilter};

use super::{Action, Event, Source};
use crate::calling::CallAction;

/// An action by its kind and ids.
pub(super) fn action(action: &Action) -> String {
    match action {
        Action::Send { conversation, .. } => format!("send to {conversation}"),
        Action::Edit {
            conversation,
            message_id,
            ..
        } => format!("edit of {message_id} in {conversation}"),
        Action::Delete {
            conversation,
            message_id,
        } => format!("delete of {message_id} in {conversation}"),
        Action::React {
            conversation,
            message_id,
            key,
        } => format!("reaction {key} on {message_id} in {conversation}"),
        Action::Unreact {
            conversation,
            message_id,
            key,
        } => format!("reaction {key} taken back on {message_id} in {conversation}"),
        Action::Read {
            conversation,
            message_id,
        } => format!("read receipt up to {message_id} in {conversation}"),
        // the name is the user's text: never logged
        Action::CreateChat { others, .. } => format!("a chat with {} other(s)", others.len()),
        Action::AddMembers {
            conversation, members, ..
        } => format!("{} people added to {conversation}", members.len()),
        Action::ActivityRead { item, read: true } => format!("activity item {item} marked read"),
        Action::ActivityRead { item, read: false } => format!("activity item {item} marked unread"),
        Action::ActivitySeen => "activity seen".to_owned(),
        Action::ActivityAllRead => "all activity marked read".to_owned(),
    }
}

/// A call action by its kind and ids.
pub(super) fn call(action: &CallAction) -> String {
    match action {
        CallAction::Captions { call, enabled } => format!("call {} captions {enabled}", call.get()),
        CallAction::CaptionLanguage { call, .. } => format!("call {} caption spoken language", call.get()),
        CallAction::Record { call, .. } => format!("call {} start recording", call.get()),
        CallAction::StopRecording { call } => format!("call {} stop recording", call.get()),
        CallAction::Transcribe { call, .. } => format!("call {} start transcription", call.get()),
        CallAction::StopTranscription { call } => format!("call {} stop transcription", call.get()),
        CallAction::Call { chat } => format!("a call from {chat}"),
        CallAction::TestCall { echo } => format!("the test call of {echo}"),
        CallAction::Join { chat } => format!("a join of the call in {chat}"),
        CallAction::JoinMeetingCode(_) => "a join by meeting ID".to_owned(),
        CallAction::JoinMeeting { chat, .. } => format!("a join of the meeting in {chat}"),
        CallAction::MeetNow { channel } => format!("a new meeting in {channel}"),
        CallAction::JoinChannel { channel } => format!("a join of the meeting running in {channel}"),
        CallAction::Answer { call } => format!("the answer of call {}", call.get()),
        CallAction::Decline { call } => format!("the decline of call {}", call.get()),
        CallAction::HangUp { call } => format!("a hang-up of call {}", call.get()),
        CallAction::Mute { call, muted } => format!("call {} muted {muted}", call.get()),
        CallAction::Renegotiate { call } => format!("a renegotiation of call {}", call.get()),
        CallAction::Camera { call, on } => format!("call {} camera {}", call.get(), if *on { "on" } else { "off" }),
        CallAction::Lobby { call, waiting, admit } => format!(
            "call {}: {} {waiting} from the lobby",
            call.get(),
            if *admit { "admit" } else { "deny" }
        ),
        CallAction::RaiseHand { call, raised } => format!("call {} hand raised {raised}", call.get()),
        CallAction::React { call, reaction } => format!("call {}: reaction {reaction}", call.get()),
        CallAction::EndMeeting { call } => format!("call {}: the meeting ended for everyone", call.get()),
        CallAction::Hold { call, held } => format!("call {} held {held}", call.get()),
        CallAction::Timer { call, timer } => format!(
            "call {}: timer {} {} ms",
            call.get(),
            timer.state.as_str(),
            timer.remaining_ms
        ),
        CallAction::LockMeeting { call, locked } => format!("call {}: meeting locked {locked}", call.get()),
        CallAction::Share { call, on } => format!("call {} sharing its screen {on}", call.get()),
        // no names: that someone was added
        CallAction::Add { call, .. } => format!("call {}: someone added", call.get()),
        CallAction::Transfer { call, .. } => format!("call {}: transferred", call.get()),
        CallAction::MuteAll { call } => format!("call {}: everyone else muted", call.get()),
        CallAction::MeetingOption { call, option } => format!("call {}: meeting option {option:?}", call.get()),
    }
}

/// An event's level and line, when a log at `max` writes it: the line is formatted only then, and each event is
/// judged at its own level (a failure is logged at `warn` too, a message only at `info` or `debug`).
pub(super) fn logged(event: &Event, max: LevelFilter) -> Option<(Level, String)> {
    let level = level(event);
    (level <= max).then(|| (level, line(event)))
}

/// An event's level: `warn` for a failure, `debug` for what a read stores in bulk and for typing, `info` otherwise.
fn level(event: &Event) -> Level {
    match event {
        Event::Message { source, .. } => match source {
            Source::Delivery | Source::Replay => Level::Info,
            Source::CatchUp | Source::History => Level::Debug,
        },
        Event::Typing { .. } => Level::Debug,
        Event::Failed { .. } | Event::CatchUpFailed { .. } => Level::Warn,
        _ => Level::Info,
    }
}

/// An event's line.
fn line(event: &Event) -> String {
    match event {
        Event::Message {
            conversation,
            id,
            change,
            applied,
            source,
            ..
        } => format!("message {id} in {conversation}: {change:?}, {applied:?}, from {source:?}"),
        Event::Typing {
            conversation, typing, ..
        } => format!("typing {typing} in {conversation}"),
        Event::Queued(row) | Event::Outbox(row) => format!(
            "outbox row {} ({:?}, clientmessageid {}): {:?}",
            row.id, row.kind, row.client_message_id, row.state
        ),
        Event::Failed { during, error } => format!("failed {during}: {error}"),
        Event::CatchUpFailed { cause, error } => format!("catch-up ({cause:?}) failed: {error}"),
        other => plain(other),
    }
}

/// The events whose fields hold no text of anyone's.
fn plain(event: &Event) -> String {
    match event {
        Event::Connected { endpoint } => format!("push channel up (endpoint {endpoint})"),
        Event::Registration(report) => format!("push registration: {report:?}"),
        Event::Disconnected { down, .. } => format!("push channel down: {down:?}"),
        Event::PushStopped(stop) => format!("push channel stopped: {stop:?}"),
        Event::CaughtUp { cause, report } => format!(
            "caught up ({cause:?}): {} conversation(s), {} message(s) changed, {} failed",
            report.conversations,
            report.messages.changed(),
            report.failed.len()
        ),
        Event::ReadHorizon { conversation } => format!("read horizon moved in {conversation}"),
        Event::ConversationChanged { conversation } => format!("conversation {conversation} changed"),
        Event::Transient(transient, sent) => format!("transient {transient:?}: {sent:?}"),
        Event::Profile { mri } => format!("profile of {mri} changed"),
        Event::Presence { mri, availability } => format!("presence of {mri}: {availability:?}"),
        Event::Teams => "the teams changed".to_owned(),
        Event::Indexed { messages } => format!("search backfill over: {messages} message(s) indexed"),
        Event::IncomingCall(incoming) => format!(
            "incoming call {} from {}{}",
            incoming.call.get(),
            incoming.caller,
            incoming
                .group_chat
                .as_ref()
                .map(|chat| format!(" in {chat}"))
                .unwrap_or_default()
        ),
        Event::RingEnded { call, end } => {
            format!("call {}: its ring ended ({:?}, {:?})", call.get(), end.code, end.phrase)
        }
        // an end by its code, phrase, sender's MRI and categories: never the sender's name
        Event::Call {
            call,
            status: crate::CallStatus::Ended(crate::End::Remote(end)),
        } => format!(
            "call {}: ended remotely ({:?}, {:?}, {:?}, by {}, {:?})",
            call.get(),
            end.code,
            end.sub_code,
            end.phrase,
            end.by.as_ref().map_or("nobody", |by| by.as_str()),
            end.categories
        ),
        Event::CallCaptions { call, captions } => format!(
            "call {}: captions {:?}, {} utterances",
            call.get(),
            captions.phase,
            captions.lines.len()
        ),
        Event::CallRecording { call, recording } => format!(
            "call {}: recording {:?}{}",
            call.get(),
            recording.phase,
            if recording.problem.is_some() { ", failed" } else { "" }
        ),
        Event::Call { call, status } => format!("call {}: {status:?}", call.get()),
        // no names: how many, which lines, who of them speaks
        Event::CallParticipants { call, view } => format!(
            "call {}: {} participant(s), cameras on {}, lines {:?}, speaking {}{}, muted {}, hands up {}",
            call.get(),
            view.participants.len(),
            view.participants.iter().filter(|p| p.video_on).count(),
            view.participants.iter().map(|p| p.line).collect::<Vec<_>>(),
            view.participants.iter().filter(|p| p.speaking).count(),
            if view.speaking { " and the account" } else { "" },
            view.participants.iter().filter(|p| p.muted).count(),
            view.participants.iter().filter(|p| p.hand_raised).count(),
        ),
        // no names: how many wait
        Event::Lobby { call, waiting } => format!("call {}: {} waiting in the lobby", call.get(), waiting.len()),
        // the account's own state in its call, a reaction, a hold, an add, a transfer
        _ if !call_state(event).is_empty() => call_state(event),
        Event::CallingRegistration(report) => format!("calling registration: {report:?}"),
        // a message, typing, the outbox and a failed catch-up are told by their own lines
        _ => String::new(),
    }
}

/// What a meeting shares, by kind: never its join link or passcode, nor who records.
fn call_meeting(call: crate::CallId, state: &teams_call::MeetingState) -> String {
    format!(
        "call {}: timer {:?}, locked {}, join info {}, recording {}, transcribing {}",
        call.get(),
        state.timer.as_ref().map(|timer| timer.state),
        state.locked,
        state.join.is_some(),
        state.recording.is_some(),
        state.transcribing.is_some()
    )
}

/// A call's own state (its voicemail, its devices gone, the account's hand, hold and shares) and a live reaction, by
/// MRI and kind: never the sender's name.
fn call_state(event: &Event) -> String {
    match event {
        Event::CallOwn {
            call,
            hand_raised,
            organizer,
        } => format!(
            "call {}: own hand raised {hand_raised}, organizer {organizer}",
            call.get()
        ),
        Event::CallReaction {
            call, from, reaction, ..
        } => format!("call {}: reaction {reaction} from {from}", call.get()),
        Event::CallMeeting { call, state } => call_meeting(*call, state),
        Event::Voicemail { call } => format!("call {}: taken by the callee's voicemail", call.get()),
        Event::CameraOff { call } => format!("call {}: its camera went off", call.get()),
        Event::AudioClosed { call, microphone } => format!(
            "call {}: its {} went away",
            call.get(),
            if *microphone { "microphone" } else { "speaker" }
        ),
        Event::CallHeld { call, held } => format!("call {} held {held}", call.get()),
        Event::CallHeldByFarEnd { call, held } => format!("call {} held by its far end {held}", call.get()),
        Event::CallSharing { call, on } => format!("call {} sharing its screen {on}", call.get()),
        Event::CallRemoteShare { call, on } => format!("call {}: the far end's screen shown {on}", call.get()),
        Event::CallGroupChat { call, .. } => format!("call {}: in a group chat now", call.get()),
        Event::CallAddFailed { call, .. } => format!("call {}: someone added didn't come in", call.get()),
        Event::CallTransferFailed { call } => format!("call {}: its transfer wasn't taken", call.get()),
        Event::CallTransferred { call, .. } => format!("call {}: transferred by its far end", call.get()),
        _ => String::new(),
    }
}

#[cfg(test)]
#[path = "../../tests/support/described.rs"]
mod tests;
