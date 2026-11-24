//! The call's writes, built from what the call knows: its callbacks (made here, on the call's agent), the links its
//! services handed out, and the account. Each is a typed `teams-proto` request validated against its captured shape
//! when built; an unsupported shape returns [`teams_proto::CallWriteError`].

use std::time::Instant;

use teams_proto::Mri;
use teams_proto::UpdateParticipantProperties;
use teams_proto::{
    AcceptCall, AcceptParts, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AddParts, AddToCall, AnswerMedia,
    AnswerMediaParts, ApplyChannelParameters, AttachCall, AttachParts, CallLink, ChannelParameters, DecideLobby,
    DeclineCall, Invitee, JoinCall, JoinMedia, JoinParts, KeepAliveCall, LeaveCall, LeaveReason, Links, MediaContent,
    MediaDescriptions, MeetingInfo, RenegotiateMedia, ReportRinging, StartCall, StartParts, UpdateEndpointMetadata,
    UpdateEndpointState, UpdateMediaDescriptions,
};
use teams_proto::{
    AcceptTransfer, EndConversation, HoldCall, PublishState, RemoveState, ResumeCall, SendCallReaction, TransferCall,
    Transferor,
};

use crate::agent::{Kind, guid};
use crate::call::{
    ANSWERED_CALL_LINKS, CONVERSATION_LINKS, Call, MEDIA_CONTROLLER_LINKS, Phase, Reoffer, START_CALL_LINKS, Side, Step,
};
use crate::{CallError, CallWrite, Identity, Placement};

/// The features a 1:1 call's offer requires, as every captured 1:1 start and invitation names them.
const ONE_TO_ONE_FEATURES: &str = "nonByPass";

/// A call's modalities: a group call's as captured (`Audio`, `ScreenViewer`), any other's `Audio`.
fn modalities(group: bool) -> Vec<String> {
    if group {
        vec!["Audio".to_owned(), "ScreenViewer".to_owned()]
    } else {
        vec!["Audio".to_owned()]
    }
}

/// An invitee with a fresh participant id, as the web client names one.
fn invitee(mri: &Mri) -> Result<Invitee, getrandom::Error> {
    Ok(Invitee {
        mri: mri.clone(),
        participant_id: Some(guid()?),
    })
}

/// The start of `placement` with `offer`.
pub(crate) fn start(
    call: &mut Call,
    identity: &Identity,
    placement: &Placement,
    mut offer: MediaContent,
) -> Result<CallWrite, CallError> {
    let group = matches!(
        placement,
        Placement::Chat {
            group_chat: Some(_),
            ..
        } | Placement::Meeting { .. }
            | Placement::MeetingCode(_)
            | Placement::Channel { .. }
    );
    match placement {
        Placement::TestCall { .. } => offer.required_features = None,
        Placement::Chat { group_chat: None, .. } | Placement::Transferred { .. } => {
            offer.required_features = Some(ONE_TO_ONE_FEATURES.to_owned());
        }
        Placement::Chat { .. } | Placement::Meeting { .. } | Placement::MeetingCode(_) | Placement::Channel { .. } => {}
    }
    let parts = start_parts(call, identity, group, offer)?;
    let start = match placement {
        Placement::TestCall { echo } => StartCall::echo_test(&call.context, &parts, echo)?,
        Placement::Transferred { target, transferor } => transferred(call, &parts, target, transferor)?,
        Placement::Chat { callees, group_chat } => {
            let callees = callees.iter().map(invitee).collect::<Result<Vec<_>, _>>()?;
            StartCall::in_chat(&call.context, &parts, &callees, group_chat.as_deref())?
        }
        Placement::Meeting { chat, meeting } => StartCall::in_meeting(&call.context, &parts, chat, meeting)?,
        Placement::MeetingCode(code) => StartCall::by_code(&call.context, &parts, code)?,
        // a new meeting is subscribed to first: its entry, with the offer, goes once its conversation is known
        Placement::Channel { channel } => StartCall::meet_now(&call.context, &parts, channel)?,
    };
    Ok(CallWrite::Start(start))
}

/// The transferred party's call to `target`, naming its `transferor`.
fn transferred(call: &Call, parts: &StartParts, target: &Mri, transferor: &Transferor) -> Result<StartCall, CallError> {
    Ok(StartCall::transferred(
        &call.context,
        parts,
        &invitee(target)?,
        transferor,
    )?)
}

/// What a start sends besides its kind: the call's callbacks, modalities and `offer`.
fn start_parts(
    call: &mut Call,
    identity: &Identity,
    group: bool,
    offer: MediaContent,
) -> Result<StartParts, CallError> {
    call.modalities = modalities(group);
    let parts = StartParts {
        from: call.me.clone(),
        roster_update: call.callback(Kind::Conversation, "rosterUpdate")?,
        conversation_links: call.links(Kind::Conversation, &CONVERSATION_LINKS)?,
        call_links: call.links(Kind::Call, &START_CALL_LINKS)?,
        media_controller_links: call.links(Kind::Call, &MEDIA_CONTROLLER_LINKS)?,
        modalities: modalities(group),
        media: offer,
        client: identity.client.clone(),
    };
    Ok(parts)
}

/// What a join goes into: the chat (a group chat, or a channel), its message (`"0"`, a channel meeting's post, or
/// none for the organizer's entry into the meeting it started), the meeting it is, if any, and whether the service
/// posts the meeting to its channel (Meet now's entry).
pub(crate) struct JoinInto<'a> {
    pub(crate) chat: &'a str,
    pub(crate) message_id: Option<&'a str>,
    pub(crate) meeting: Option<MeetingInfo>,
    pub(crate) posts_meetup: bool,
}

/// The join of the running call at `conversation` in `into`, with `offer`.
pub(crate) fn join(
    call: &mut Call,
    identity: &Identity,
    conversation: &CallLink,
    into: JoinInto<'_>,
    offer: MediaContent,
) -> Result<CallWrite, CallError> {
    let parts = JoinParts {
        from: call.me.clone(),
        roster_update: call.callback(Kind::Conversation, "rosterUpdate")?,
        conversation_links: call.links(Kind::Conversation, &CONVERSATION_LINKS)?,
        group_chat: into.chat.to_owned(),
        message_id: into.message_id.map(str::to_owned),
        meeting: into.meeting,
        posts_meetup: into.posts_meetup,
        client: identity.client.clone(),
    };
    call.modalities = modalities(true);
    let media = JoinMedia {
        call_links: call.links(Kind::Call, &START_CALL_LINKS)?,
        media_controller_links: call.links(Kind::Call, &MEDIA_CONTROLLER_LINKS)?,
        modalities: call.modalities.clone(),
        media: offer,
    };
    Ok(CallWrite::Join(JoinCall::enter(
        &call.context,
        conversation,
        &parts,
        &media,
    )?))
}

/// The `attach` to the invitation the call answers.
pub(crate) fn attach(call: &mut Call, identity: &Identity) -> Result<CallWrite, CallError> {
    let Some(invitation) = call.invitation().cloned() else {
        return Err(CallError::NotNow);
    };
    let parts = AttachParts {
        from: call.me.clone(),
        end: call.callback(Kind::Call, "end")?,
        roster_update: call.callback(Kind::Conversation, "rosterUpdate")?,
        conversation_links: call.links(Kind::Conversation, &CONVERSATION_LINKS)?,
        conversation: invitation.conversation.clone(),
        client: identity.client.clone(),
    };
    Ok(CallWrite::Attach(AttachCall::new(
        &call.context,
        &invitation.attach,
        &parts,
    )?))
}

/// The leg rings here: `progress` at `link`.
pub(crate) fn ringing(call: &Call, link: &CallLink) -> Result<CallWrite, CallError> {
    Ok(CallWrite::Ringing(ReportRinging::new(&call.context, link, &call.me)?))
}

/// The account's answer of the invitation: its endpoint state (unmuted) where the conversation handed out the link,
/// then its `accept` with `answer`.
pub(crate) fn answer(
    call: &mut Call,
    identity: &Identity,
    answer: MediaContent,
) -> Result<Vec<(Step, CallWrite)>, CallError> {
    let acceptance = call
        .leg
        .as_ref()
        .map(|leg| leg.acceptance.clone())
        .ok_or(CallError::NotNow)?;
    let mut writes = Vec::new();
    if let Some(link) = call.conversation.endpoint_state.clone() {
        let state = UpdateEndpointState::new(&call.context, &link, &call.me, false, 1)?;
        writes.push((Step::EndpointState, CallWrite::EndpointState(state)));
    }
    call.modalities = modalities(call.in_group(None));
    let parts = AcceptParts {
        accepted_by: call.me.clone(),
        modalities: call.modalities.clone(),
        call_links: call.links(Kind::Call, &ANSWERED_CALL_LINKS)?,
        media_controller_links: call.links(Kind::Call, &MEDIA_CONTROLLER_LINKS)?,
        media: answer,
        client: identity.client.clone(),
    };
    let accept = AcceptCall::new(&call.context, &acceptance, &parts)?;
    writes.push((Step::Accept, CallWrite::Accept(accept)));
    Ok(writes)
}

/// The account's decline of the invitation: `DELETE` on its leg.
pub(crate) fn decline(call: &Call) -> Result<CallWrite, CallError> {
    let leg = call.leg.as_ref().ok_or(CallError::NotNow)?;
    Ok(CallWrite::Decline(DeclineCall::new(&call.context, &leg.call_leg)?))
}

/// The account leaves: a cancel while its placed call rings, a hang-up otherwise.
pub(crate) fn leave(call: &Call, now: Instant) -> Result<CallWrite, CallError> {
    let link = call.conversation.leave.as_ref().ok_or(CallError::NotNow)?;
    let ringing = matches!(call.side, Side::Placed { .. }) && call.phase == Phase::Ringing;
    let reason = if ringing {
        LeaveReason::Cancelled {
            rang_seconds: call.rang_seconds(now),
        }
    } else {
        LeaveReason::HungUp
    };
    Ok(CallWrite::Leave(LeaveCall::new(&call.context, link, &call.me, reason)?))
}

/// The account's endpoint metadata in the call's conversation.
pub(crate) fn endpoint_metadata(call: &Call, link: &CallLink) -> Result<CallWrite, CallError> {
    Ok(CallWrite::EndpointMetadata(UpdateEndpointMetadata::new(
        &call.context,
        link,
        &call.me,
    )?))
}

/// The acceptance's acknowledgement by request, handing out `links`.
pub(crate) fn acknowledge(call: &Call, link: &CallLink, links: &Links) -> Result<CallWrite, CallError> {
    Ok(CallWrite::Acknowledge(AcknowledgeAcceptance::new(
        &call.context,
        link,
        links,
    )?))
}

/// The `add` of the test call's echo service at `link`, naming the 1:1 chat of the account and the echo.
pub(crate) fn add(call: &mut Call, identity: &Identity, link: &CallLink, echo: &Mri) -> Result<CallWrite, CallError> {
    let object_id = |mri: &Mri| {
        let mri = mri.as_str();
        mri.strip_prefix("8:orgid:")
            .or_else(|| mri.strip_prefix("28:"))
            .unwrap_or(mri)
            .to_owned()
    };
    let parts = AddParts {
        from: call.me.clone(),
        invitee: invitee(echo)?,
        chat_thread: format!("19:{}_{}@unq.gbl.spaces", object_id(&identity.mri), object_id(echo)),
        tenant_id: identity.tenant_id.clone(),
        links: call.links(Kind::Conversation, &["addParticipantSuccess", "addParticipantFailure"])?,
    };
    Ok(CallWrite::Add(AddToCall::invite(&call.context, link, &parts)?))
}

/// The account's renegotiation of the call's media at the call controller's `link`: its new `offer` and its callbacks
/// for the answer and the rejection, with what the renegotiation waits on once it went (those callbacks' paths).
pub(crate) fn renegotiate(
    call: &mut Call,
    link: &CallLink,
    offer: &MediaContent,
) -> Result<(CallWrite, Reoffer), CallError> {
    let (links, reoffer) = call.reoffer_links()?;
    let write = RenegotiateMedia::new(&call.context, link, &call.me, &call.modalities, &links, offer)?;
    Ok((CallWrite::Renegotiate(write), reoffer))
}

/// The account's `answer` to a renegotiation delivered to it, at the `mediaAnswer` `link` the renegotiation handed out:
/// its callbacks for the media acknowledgement and the media controller.
pub(crate) fn answer_media(call: &mut Call, link: &CallLink, answer: MediaContent) -> Result<CallWrite, CallError> {
    let parts = AnswerMediaParts {
        sender: call.me.clone(),
        modalities: call.modalities.clone(),
        links: call.links(Kind::Call, &["mediaAcknowledgement"])?,
        media_controller_links: call.links(Kind::Call, &MEDIA_CONTROLLER_LINKS)?,
        media: answer,
    };
    Ok(CallWrite::AnswerMedia(AnswerMedia::new(&call.context, link, &parts)?))
}

/// A meeting lobby's Admit (`admit` true, at the conversation's `admit` link) or Deny (at its `removeParticipant`
/// link) of `waiting`, with new callbacks for how it went (named as the web client's are: its admit's
/// `admitParticipantSuccess` and `admitParticipantFailure`, its removal's `removeParticipantSuccess` and
/// `removeParticipantFailure`).
pub(crate) fn lobby(call: &mut Call, link: &CallLink, waiting: &Mri, admit: bool) -> Result<CallWrite, CallError> {
    let (success, failure) = if admit {
        ("admitParticipantSuccess", "admitParticipantFailure")
    } else {
        ("removeParticipantSuccess", "removeParticipantFailure")
    };
    let (on_success, on_failure) = (
        call.callback(Kind::Conversation, success)?,
        call.callback(Kind::Conversation, failure)?,
    );
    let write = if admit {
        let links = Links::new()
            .with("admitSuccess", on_success)
            .with("admitFailure", on_failure);
        DecideLobby::admit(&call.context, link, (&call.me, waiting), &links)?
    } else {
        let links = Links::new().with(success, on_success).with(failure, on_failure);
        DecideLobby::deny(&call.context, link, (&call.me, waiting), &links)?
    };
    Ok(CallWrite::Lobby(write))
}

/// The account's microphone muted (`muted`) or not, told at the conversation's `updateEndpointState` link as the
/// endpoint state's next report.
pub(crate) fn muted(call: &mut Call, link: &CallLink, muted: bool) -> Result<CallWrite, CallError> {
    let sequence = call.endpoint_states + 1;
    let write = UpdateEndpointState::new(&call.context, link, &call.me, muted, sequence)?;
    call.endpoint_states = sequence;
    Ok(CallWrite::EndpointState(write))
}

/// The account's hand raised at the conversation's `publishState` link, or (`lowered`, its state's id) taken back at
/// its `removeState` link, as the next state the account changed.
pub(crate) fn hand(call: &mut Call, link: &CallLink, lowered: Option<&str>) -> Result<CallWrite, CallError> {
    let sequence = call.published + 1;
    let write = match lowered {
        Some(state) => CallWrite::RemoveState(RemoveState::new(&call.context, link, &call.me, state, sequence)?),
        None => CallWrite::PublishState(PublishState::raise_hand(&call.context, link, &call.me, sequence)?),
    };
    call.published = sequence;
    Ok(write)
}

/// The account sharing its screen, told at the conversation's `updateParticipantProperties` link as its next
/// `screenSharerAttributes`, with a callback for the update's status.
pub(crate) fn screen_sharer(call: &mut Call, link: &CallLink) -> Result<CallWrite, CallError> {
    let sequence = call.screen_shares + 1;
    let status = call.callback(Kind::Conversation, "updateParticipantPropertiesStatus")?;
    let operation = guid()?;
    let write =
        UpdateParticipantProperties::screen_sharer(&call.context, link, &call.me, sequence, &status, &operation)?;
    call.screen_shares = sequence;
    Ok(CallWrite::ParticipantProperties(write))
}

/// A live reaction `reaction` at the conversation's `sendMessage` link.
pub(crate) fn reaction(call: &Call, link: &CallLink, reaction: &str) -> Result<CallWrite, CallError> {
    Ok(CallWrite::Reaction(SendCallReaction::new(
        &call.context,
        link,
        &call.me,
        reaction,
    )?))
}

/// The call put on hold (`held`) at its leg's `hold` link, or taken off hold at the hold's `resume` link, with the
/// callback its completion is told at (`holdCompletion`, `resumeCompletion`).
pub(crate) fn hold(call: &mut Call, link: &CallLink, held: bool) -> Result<CallWrite, CallError> {
    Ok(if held {
        let links = call.links(Kind::Call, &["holdCompletion"])?;
        CallWrite::Hold(HoldCall::new(&call.context, link, &links)?)
    } else {
        let links = call.links(Kind::Call, &["resumeCompletion"])?;
        CallWrite::Resume(ResumeCall::new(&call.context, link, &links)?)
    })
}

/// `invitee` added to the call at the conversation's `add` `link`, the call's `group_chat` named, its outcome told at
/// new `addParticipantSuccess` and `addParticipantFailure` callbacks.
pub(crate) fn add_person(
    call: &mut Call,
    link: &CallLink,
    invitee_mri: &Mri,
    group_chat: &str,
) -> Result<CallWrite, CallError> {
    let links = call.links(Kind::Conversation, &["addParticipantSuccess", "addParticipantFailure"])?;
    Ok(CallWrite::Add(AddToCall::person(
        &call.context,
        link,
        &call.me,
        &invitee(invitee_mri)?,
        group_chat,
        &links,
    )?))
}

/// `invitee` asked into the meeting's call at the conversation's `add` `link`, the meeting's chat and its organizer
/// named, its outcome told at new callbacks.
pub(crate) fn add_to_meeting(
    call: &mut Call,
    link: &CallLink,
    invitee_mri: &Mri,
    (chat, meeting): (&str, &MeetingInfo),
) -> Result<CallWrite, CallError> {
    let links = call.links(Kind::Conversation, &["addParticipantSuccess", "addParticipantFailure"])?;
    Ok(CallWrite::Add(AddToCall::to_meeting(
        &call.context,
        link,
        (&call.me, &invitee(invitee_mri)?),
        (chat, meeting),
        &links,
    )?))
}

/// The call's far end transferred to `target` at the call leg's `transfer` `link`, its acceptance and completion told
/// at new callbacks.
pub(crate) fn transfer(call: &mut Call, link: &CallLink, target: &Mri) -> Result<CallWrite, CallError> {
    let links = call.links(Kind::Call, &["transferAcceptance", "transferCompletion"])?;
    Ok(CallWrite::Transfer(TransferCall::new(
        &call.context,
        link,
        &call.me,
        target,
        &links,
    )?))
}

/// The transfer the far end asked for, taken at its `transferAcceptance` `link`.
pub(crate) fn accept_transfer(call: &Call, link: &CallLink) -> Result<CallWrite, CallError> {
    Ok(CallWrite::AcceptTransfer(AcceptTransfer::new(&call.context, link)?))
}

/// The meeting ended for everyone, at the conversation's own URL.
pub(crate) fn end_meeting(call: &Call, link: &CallLink) -> Result<CallWrite, CallError> {
    Ok(CallWrite::EndMeeting(EndConversation::new(
        &call.context,
        link,
        &call.me,
    )?))
}

/// The call's keep-alive, at the call leg's own link (the acceptance's or the `accept`'s answer's `callLeg`).
pub(crate) fn keep_alive(call: &Call, link: &CallLink) -> Result<CallWrite, CallError> {
    Ok(CallWrite::KeepAlive(KeepAliveCall::new(&call.context, link)?))
}

/// A group call's media lines described again at the call controller's `updateMediaDescriptions` link, tagged `tag`
/// when given (a share's).
pub(crate) fn describe(
    call: &Call,
    link: &CallLink,
    descriptions: &MediaDescriptions,
    tag: Option<&str>,
) -> Result<CallWrite, CallError> {
    Ok(CallWrite::MediaDescriptions(UpdateMediaDescriptions::new(
        &call.context,
        link,
        descriptions,
        tag,
    )?))
}

/// A group call's send capabilities at the call controller's `applyChannelParameters` link.
pub(crate) fn channel_parameters(
    call: &Call,
    link: &CallLink,
    parameters: &ChannelParameters,
) -> Result<CallWrite, CallError> {
    Ok(CallWrite::ChannelParameters(ApplyChannelParameters::new(
        &call.context,
        link,
        parameters,
    )?))
}

/// The acknowledgement of the answer to the account's renegotiation, at the link that answer handed out.
pub(crate) fn acknowledge_media(call: &Call, link: &CallLink) -> Result<CallWrite, CallError> {
    Ok(CallWrite::AcknowledgeMediaAnswer(AcknowledgeMediaAnswer::new(
        &call.context,
        link,
    )?))
}
