//! What the call's services send back, read as the captures show it (`docs/protocol/calling.md`,
//! `docs/protocol/calling-echo.md`; fixtures `tests/fixtures/calling/`): an incoming call's invitation, the
//! conversation a start or a join opens, an `attach`'s and an `accept`'s answers, the acceptance of a call the account
//! placed, and a call's end. Only the parts the machine acts on are read; a link is kept only when it reads as a
//! [`CallLink`] (exactly as handed out), and a value in a form no capture showed is left out, never guessed.

use base64::Engine as _;
use serde_json::Value;
use teams_proto::{CallLink, MediaContent};
use teams_proto::{INVITATION_EVENTS, Mri};

/// An incoming call's invitation (`{"evt": 107 | 109, "gp": <base64 JSON>}` at the calling endpoint's root), read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invitation {
    pub(crate) caller: Mri,
    pub(crate) caller_name: Option<String>,
    pub(crate) callee: Mri,
    /// The group chat a group call (`evt` 109) is in.
    pub(crate) group_chat: Option<String>,
    /// `conversationInvitation.conversationController`: the call's conversation.
    pub(crate) conversation: CallLink,
    /// `callNotification.links.attach`.
    pub(crate) attach: CallLink,
    /// The caller's offer (a 1:1 call's) or the call's mixer's (a group call's).
    pub(crate) offer: Option<MediaContent>,
    /// The participant id the caller gave the callee (`callNotification.to.participantId`): the callee's in the call,
    /// which the web client names itself by (its progress, its answer, its endpoint state); the service keys the
    /// callee's endpoint state by it (seen 2026-11-06: a mute under another id never showed).
    pub(crate) callee_participant: Option<String>,
}

/// The invitation `body` is; `None` for any other body or one that doesn't read.
#[cfg(test)]
fn invitation(body: &[u8]) -> Option<Invitation> {
    invitation_in(&teams_proto::Cloud::Microsoft, body)
}

pub(crate) fn invitation_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<Invitation> {
    let body: Value = serde_json::from_slice(body).ok()?;
    if !INVITATION_EVENTS.contains(&body.get("evt")?.as_u64()?) {
        return None;
    }
    let gp = base64::engine::general_purpose::STANDARD
        .decode(body.get("gp")?.as_str()?)
        .ok()?;
    let gp: Value = serde_json::from_slice(&gp).ok()?;
    let notification = gp.get("callNotification")?;
    let text = |at: &Value, pointer: &str| at.pointer(pointer).and_then(Value::as_str).map(str::to_owned);
    Some(Invitation {
        caller: Mri::new(text(notification, "/from/id")?),
        caller_name: text(notification, "/from/displayName"),
        callee: Mri::new(text(notification, "/to/id")?),
        group_chat: text(&gp, "/groupChat/threadId"),
        conversation: link(cloud, &gp, "/conversationInvitation/conversationController")?,
        attach: link(cloud, notification, "/links/attach")?,
        offer: notification.get("mediaContent").and_then(media),
        callee_participant: text(notification, "/to/participantId").filter(|id| !id.is_empty()),
    })
}

/// The link at `pointer`, if it reads as a [`CallLink`].
fn link(cloud: &teams_proto::Cloud, at: &Value, pointer: &str) -> Option<CallLink> {
    CallLink::parse_in(cloud, at.pointer(pointer)?.as_str()?)
}

/// An offer or an answer: its blob and content type, its media leg when given.
pub(crate) fn media(content: &Value) -> Option<MediaContent> {
    let blob = content.get("blob")?.as_str()?;
    let content_type = content.get("contentType")?.as_str()?;
    let mut media = MediaContent::new(blob, content_type);
    media.new_mixer_session = content.get("newOffer").and_then(Value::as_bool) == Some(true)
        && content.get("fromMixer").and_then(Value::as_bool) == Some(true);
    media.media_leg_id = content.get("mediaLegId").and_then(Value::as_str).map(str::to_owned);
    Some(media)
}

/// The links of a call's conversation the machine sends to, as an answer hands them out (each where it reads).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Conversation {
    pub(crate) leave: Option<CallLink>,
    pub(crate) caption_add: Option<CallLink>,
    pub(crate) endpoint_metadata: Option<CallLink>,
    pub(crate) endpoint_state: Option<CallLink>,
    /// `addParticipantAndModality`: the `…/add` an `add` posts to.
    pub(crate) add: Option<CallLink>,
    /// A meeting's `admit` and `removeParticipant`: where its lobby's Admit and Deny go.
    pub(crate) admit: Option<CallLink>,
    pub(crate) remove: Option<CallLink>,
    /// `publishState` and `removeState`: where the account's raised hand goes up and comes down.
    pub(crate) publish_state: Option<CallLink>,
    pub(crate) remove_state: Option<CallLink>,
    /// `sendMessage`: where the account's live reactions go.
    pub(crate) send_message: Option<CallLink>,
    /// `updateParticipantProperties`: where the account tells it shares its screen.
    pub(crate) participant_properties: Option<CallLink>,
    /// The conversation itself (its `conversationController`): where a meeting's organizer ends it for everyone.
    pub(crate) controller: Option<CallLink>,
    /// `updateMeetingStates` and `updateMeetingSettings`: where the meeting's timer and its lock change.
    pub(crate) meeting_states: Option<CallLink>,
    pub(crate) meeting_settings: Option<CallLink>,
    /// `mute`: where the organizer mutes everyone else.
    pub(crate) mute: Option<CallLink>,
}

impl Conversation {
    fn of(cloud: &teams_proto::Cloud, links: &Value) -> Self {
        let at = |name: &str| {
            links
                .get(name)
                .and_then(Value::as_str)
                .and_then(|link| CallLink::parse_in(cloud, link))
        };
        Self {
            leave: at("leave"),
            caption_add: at("addParticipant"),
            endpoint_metadata: at("updateEndpointMetadata"),
            endpoint_state: at("updateEndpointState"),
            add: at("addParticipantAndModality"),
            admit: at("admit"),
            remove: at("removeParticipant"),
            publish_state: at("publishState"),
            remove_state: at("removeState"),
            send_message: at("sendMessage"),
            participant_properties: at("updateParticipantProperties"),
            controller: None,
            meeting_states: at("updateMeetingStates"),
            meeting_settings: at("updateMeetingSettings"),
            mute: at("mute"),
        }
    }
}

/// The conversation a start's or a subscription's answer `body` opened: its `conversationController`.
pub(crate) fn conversation_controller_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<CallLink> {
    let body: Value = serde_json::from_slice(body).ok()?;
    link(cloud, &body, "/conversationController")
}

/// The conversation a start's or a join's answer is (`{"conversationController", "links": {…}}`); `None` unless it
/// names a conversation controller.
#[cfg(test)]
fn conversation(body: &[u8]) -> Option<Conversation> {
    conversation_in(&teams_proto::Cloud::Microsoft, body)
}

pub(crate) fn conversation_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<Conversation> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let controller = link(cloud, &body, "/conversationController")?;
    let mut conversation = Conversation::of(cloud, body.get("links")?);
    conversation.controller = Some(controller);
    Some(conversation)
}

/// What an `attach`'s answer hands out: the call leg's `progress`, `acceptance` and `callLeg` (the decline's) links,
/// and the links of the conversation its `join` subscribed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attached {
    pub(crate) progress: CallLink,
    pub(crate) acceptance: CallLink,
    pub(crate) call_leg: CallLink,
    pub(crate) conversation: Conversation,
}

/// The `attach` answer `body`, its `join` the subscription to `conversation`; `None` unless the leg's three links read.
#[cfg(test)]
fn attached(body: &[u8], conversation: &CallLink) -> Option<Attached> {
    attached_in(&teams_proto::Cloud::Microsoft, body, conversation)
}

pub(crate) fn attached_in(cloud: &teams_proto::Cloud, body: &[u8], conversation: &CallLink) -> Option<Attached> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let links = body.pointer("/callInvitation/links")?;
    let subscription = body
        .get("additionalActionResponses")?
        .as_array()?
        .iter()
        .find(|response| response.get("url").and_then(Value::as_str) == Some(conversation.as_str()))
        .and_then(|response| response.pointer("/output/links"))
        .map(|links| Conversation {
            controller: Some(conversation.clone()),
            ..Conversation::of(cloud, links)
        })
        .unwrap_or_default();
    Some(Attached {
        progress: link(cloud, links, "/progress")?,
        acceptance: link(cloud, links, "/acceptance")?,
        call_leg: link(cloud, links, "/callLeg")?,
        conversation: subscription,
    })
}

/// How often the call's services want a keep-alive, in seconds (`callKeepAliveInterval`), from an acceptance (`{"callAcceptance":
/// {…}}`) or an `accept`'s answer (`{"callAcceptanceAcknowledgement": {…}}`); `None` when it isn't a positive count.
fn keep_alive(at: &Value) -> Option<u64> {
    at.get("callKeepAliveInterval")?.as_u64().filter(|seconds| *seconds > 0)
}

/// What an `accept`'s answer (`{"callAcceptanceAcknowledgement": {…}}`) hands out: the keep-alive interval and the
/// call controller's `mediaRenegotiation` link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AcceptAnswer {
    pub(crate) keep_alive: Option<u64>,
    pub(crate) renegotiation: Option<CallLink>,
    /// The call leg's own link: where the keep-alive goes.
    pub(crate) call_leg: Option<CallLink>,
    /// Where a group call's media lines are described again (`updateMediaDescriptions`) and its send capabilities
    /// given (`applyChannelParameters`): the camera turned on or off.
    pub(crate) group_media: GroupMediaLinks,
    /// The call leg's `hold` link: where the account puts the call on hold.
    pub(crate) hold: Option<CallLink>,
    /// The call leg's `transfer` link: where the account transfers the call's far end.
    pub(crate) transfer: Option<CallLink>,
}

/// Where a group call's media lines are described (`updateMediaDescriptions`) and its send capabilities given
/// (`applyChannelParameters`), as an acceptance or an `accept`'s answer hands them out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GroupMediaLinks {
    pub(crate) descriptions: Option<CallLink>,
    pub(crate) channel_parameters: Option<CallLink>,
}

impl GroupMediaLinks {
    fn of(cloud: &teams_proto::Cloud, at: &Value) -> Self {
        Self {
            descriptions: link(cloud, at, "/links/updateMediaDescriptions"),
            channel_parameters: link(cloud, at, "/links/applyChannelParameters"),
        }
    }
}

/// An `accept`'s answer, as far as it reads.
#[cfg(test)]
fn accept_answer(body: &[u8]) -> AcceptAnswer {
    accept_answer_in(&teams_proto::Cloud::Microsoft, body)
}

pub(crate) fn accept_answer_in(cloud: &teams_proto::Cloud, body: &[u8]) -> AcceptAnswer {
    let Some(acknowledgement) = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body.get("callAcceptanceAcknowledgement").cloned())
    else {
        return AcceptAnswer::default();
    };
    AcceptAnswer {
        keep_alive: keep_alive(&acknowledgement),
        renegotiation: link(cloud, &acknowledgement, "/links/mediaRenegotiation"),
        call_leg: link(cloud, &acknowledgement, "/links/callLeg"),
        group_media: GroupMediaLinks::of(cloud, &acknowledgement),
        hold: link(cloud, &acknowledgement, "/links/hold"),
        transfer: link(cloud, &acknowledgement, "/links/transfer"),
    }
}

/// The call's links once the call controller moved it (a retarget), as the acknowledgement of the account's media
/// answer hands them out: `{"mediaAcknowledgement": {"code": 0, "subCode": 10109, "phrase": "Participant retarget was
/// successful.", "resultCategories": ["Success"]}, "links": {"callLeg", "mediaRenegotiation", …}}` (seen 2026-11-03,
/// `meeting-steps`: b's page let into a's meeting from its lobby, the mixer's first offer answered, the call moved to
/// another call id). An acknowledgement that hands out no call leg (`"links": {"callLeg": null}`, every later one) is
/// none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Retarget {
    pub(crate) renegotiation: Option<CallLink>,
    pub(crate) call_leg: CallLink,
    pub(crate) group_media: GroupMediaLinks,
    pub(crate) hold: Option<CallLink>,
    pub(crate) transfer: Option<CallLink>,
}

/// The retarget a media answer's acknowledgement `body` tells, if it is one ([`Retarget`]): a success naming the new
/// call leg.
pub(crate) fn retarget_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<Retarget> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let succeeded = body
        .pointer("/mediaAcknowledgement/resultCategories")?
        .as_array()?
        .iter()
        .any(|category| category.as_str() == Some("Success"));
    if !succeeded {
        return None;
    }
    Some(Retarget {
        renegotiation: link(cloud, &body, "/links/mediaRenegotiation"),
        call_leg: link(cloud, &body, "/links/callLeg")?,
        group_media: GroupMediaLinks::of(cloud, &body),
        hold: link(cloud, &body, "/links/hold"),
        transfer: link(cloud, &body, "/links/transfer"),
    })
}

/// The acceptance of a call the account placed or joined: its `acknowledgement` link (for an acknowledgement by
/// request), the answer to the account's offer, the keep-alive interval, the call controller's `mediaRenegotiation`
/// link, and whether the callee's voicemail took the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Acceptance {
    pub(crate) acknowledgement: Option<CallLink>,
    pub(crate) answer: Option<MediaContent>,
    pub(crate) keep_alive: Option<u64>,
    pub(crate) renegotiation: Option<CallLink>,
    /// The call leg's own link: where the keep-alive goes.
    pub(crate) call_leg: Option<CallLink>,
    /// The callee's voicemail took the call (`acceptedBy.endpointType` `voicemail`, as captured for a 1:1 call the
    /// callee declined or didn't answer).
    pub(crate) voicemail: bool,
    /// A group call's media links.
    pub(crate) group_media: GroupMediaLinks,
    /// The call leg's `hold` link: where the account puts the call on hold.
    pub(crate) hold: Option<CallLink>,
    /// The call leg's `transfer` link: where the account transfers the call's far end.
    pub(crate) transfer: Option<CallLink>,
}

/// The acceptance delivery `body`; `None` unless it is a `callAcceptance`.
#[cfg(test)]
fn acceptance(body: &[u8]) -> Option<Acceptance> {
    acceptance_in(&teams_proto::Cloud::Microsoft, body)
}

pub(crate) fn acceptance_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<Acceptance> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let acceptance = body.get("callAcceptance")?;
    Some(Acceptance {
        acknowledgement: link(cloud, acceptance, "/links/acknowledgement"),
        answer: acceptance.get("mediaContent").and_then(media),
        keep_alive: keep_alive(acceptance),
        renegotiation: link(cloud, acceptance, "/links/mediaRenegotiation"),
        call_leg: link(cloud, acceptance, "/links/callLeg"),
        voicemail: acceptance.pointer("/acceptedBy/endpointType").and_then(Value::as_str) == Some("voicemail"),
        group_media: GroupMediaLinks::of(cloud, acceptance),
        hold: link(cloud, acceptance, "/links/hold"),
        transfer: link(cloud, acceptance, "/links/transfer"),
    })
}

/// Where a hold is taken back, as the hold's answer `body` hands it out: `{"holdResponse": {"links": {"resume"}}}`
/// (seen 2026-11-06, `ab-call --call steps`).
pub(crate) fn resume_link_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<CallLink> {
    let body: Value = serde_json::from_slice(body).ok()?;
    link(cloud, &body, "/holdResponse/links/resume")
}

/// One participant of a call as its roster lists them (a `rosterUpdate` delivered at the conversation's callback:
/// `{"participants": {<MRI>: {"version", "state", "details": {"displayName"}, "endpoints": {<id>: {"call":
/// {"mediaStreams": [{"type", "label", "sourceId", "direction"}]}}}}}, "type": "Delta", "sequenceNumber"}`). A 1:1
/// call's endpoints list no media streams (its media goes between the two ends, through no mixer): their `call`
/// holds only `serverMuteVersion`.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is the roster's own word for its entry, read from a field of its own"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RosterEntry {
    pub(crate) mri: Mri,
    /// Counts up per participant: an entry delivered again, or older, changes nothing.
    pub(crate) version: u64,
    /// `active` with an endpoint in the call (one with a `call`, its media streams listed or not); `inactive` once
    /// gone (its endpoints empty).
    pub(crate) active: bool,
    /// `active` with an endpoint waiting in a meeting's lobby (one with a `lobby` and none with a `call`, its
    /// `modalityJoined` `Lobby`: seen 2026-11-03, `meeting-steps`, at the organizer's roster).
    pub(crate) in_lobby: bool,
    pub(crate) name: Option<String>,
    /// The main audio's and the main video's source ids, and whether the main video goes out (`sendrecv` or
    /// `sendonly`): of the first endpoint in the call that lists its media.
    pub(crate) audio_source: Option<i64>,
    pub(crate) video_source: Option<i64>,
    pub(crate) video_on: bool,
    /// The source id of its screen while it shares it (its `applicationsharing-video` stream going out: `sendonly` or
    /// `sendrecv`, seen 2026-11-06 in a group call's roster): what a source request names to show it.
    pub(crate) sharing_source: Option<i64>,
    /// The endpoint in the call says its microphone is muted (`endpointState.state.isMuted`, seen 2026-11-06: each
    /// participant's client reports it with `updateEndpointState` as it mutes and unmutes), or the organizer muted it
    /// (its main audio `serverMuted`, seen 2026-11-06 after Mute all: the muted clients reported nothing).
    pub(crate) muted: bool,
    /// The organizer muted its main audio (`serverMuted`): the stream's `serverMuteVersion` then, which counts up with
    /// each mute or unmute the service applied to the endpoint.
    pub(crate) server_mute: Option<u64>,
    /// An attendee of the meeting (`meetingRole` `attendee`): what its organizer's Controls restrict binds it (seen
    /// 2026-11-06: Present off made everyone but the organizer an attendee, back to `presenter` once on).
    pub(crate) attendee: bool,
    /// The id of the participant's raised hand (its `publishedStates` entry of `stateType` `raiseHands`, seen
    /// 2026-11-06), while it is up.
    pub(crate) hand: Option<String>,
    /// The participant organizes the meeting (`meetingRole` `organizer`).
    pub(crate) organizer: bool,
    /// When its endpoint in the call joined (`endpointJoinTime`).
    pub(crate) joined: Option<String>,
}

/// The roster update `body`'s participants, each where it reads (an MRI and a version); `None` unless it is one.
pub(crate) fn roster(body: &[u8]) -> Option<Vec<RosterEntry>> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let participants = body.get("participants")?.as_object()?;
    Some(
        participants
            .iter()
            .filter_map(|(mri, entry)| roster_entry(mri, entry))
            .collect(),
    )
}

/// The roster a start's answer gives (its `roster`, as an update's): who is in a meeting's call already, which its later
/// updates don't repeat; `None` without one.
pub(crate) fn answered_roster(body: &[u8]) -> Option<Vec<RosterEntry>> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let participants = body.pointer("/roster/participants")?.as_object()?;
    Some(
        participants
            .iter()
            .filter_map(|(mri, entry)| roster_entry(mri, entry))
            .collect(),
    )
}

/// The service's music-on-hold app: in a held 1:1 call's roster (the holder's and the held party's) for as long as the
/// hold lasts, playing the far end its music (seen 2026-11-06, `ab-call --call steps`, and a native hold).
pub(crate) const MUSIC_ON_HOLD_MRI: &str = "28:56aa0a8f-4cd3-446b-8146-0023476a5ff2";

/// The music-on-hold app's entry in the roster update `body`, if it lists one: its version, and whether it is in the
/// call (`active` with an endpoint in the call, one `sendrecv` audio stream; `inactive` with its endpoints empty once
/// the hold ended, seen 2026-11-06 at the held party's roster).
pub(crate) fn music_on_hold(body: &[u8]) -> Option<(u64, bool)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let entry = body.get("participants")?.get(MUSIC_ON_HOLD_MRI)?;
    let version = entry.get("version")?.as_u64()?;
    let in_call = entry.get("state").and_then(Value::as_str) == Some("active")
        && entry
            .get("endpoints")
            .and_then(Value::as_object)
            .is_some_and(|endpoints| {
                endpoints
                    .values()
                    .any(|endpoint| endpoint.get("call").is_some_and(Value::is_object))
            });
    Some((version, in_call))
}

fn roster_entry(mri: &str, entry: &Value) -> Option<RosterEntry> {
    // The built-in caption service (the recording bot, from any region) and the music on hold are hidden in Teams:
    // never a person in the gallery (the music would make a held 1:1 call look like a group call's; a recording
    // shows as its notice).
    if teams_proto::captions::is_caption_service(mri) || mri == MUSIC_ON_HOLD_MRI {
        return None;
    }
    let version = entry.get("version")?.as_u64()?;
    let endpoints = entry.get("endpoints").and_then(Value::as_object);
    let in_call = endpoints.is_some_and(|endpoints| {
        endpoints
            .values()
            .any(|endpoint| endpoint.get("call").is_some_and(Value::is_object))
    });
    let streams = endpoints.and_then(|endpoints| {
        endpoints
            .values()
            .find_map(|endpoint| endpoint.pointer("/call/mediaStreams")?.as_array())
    });
    let stream = |label: &str| {
        streams?
            .iter()
            .find(|stream| stream.get("label").and_then(Value::as_str) == Some(label))
    };
    let video = stream("main-video");
    let sends = |stream: &Value| {
        stream
            .get("direction")
            .and_then(Value::as_str)
            .is_some_and(|direction| matches!(direction, "sendrecv" | "sendonly"))
    };
    let server_mute = server_mute(stream("main-audio"), endpoints);
    let state_active = entry.get("state").and_then(Value::as_str) == Some("active");
    let waiting = endpoints.is_some_and(|endpoints| {
        endpoints
            .values()
            .any(|endpoint| endpoint.get("lobby").is_some_and(Value::is_object))
    });
    Some(RosterEntry {
        mri: Mri::new(mri),
        version,
        active: state_active && in_call,
        in_lobby: state_active && waiting && !in_call,
        // the echo service's entry names nobody: an empty name is none
        name: entry
            .pointer("/details/displayName")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        audio_source: stream("main-audio").and_then(|audio| audio.get("sourceId")?.as_i64()),
        video_source: video.and_then(|video| video.get("sourceId")?.as_i64()),
        video_on: video.is_some_and(sends),
        sharing_source: stream("applicationsharing-video")
            .filter(|sharing| sends(sharing))
            .and_then(|sharing| sharing.get("sourceId")?.as_i64()),
        muted: server_mute.is_some() || reports_muted(endpoints),
        server_mute,
        attendee: entry.get("meetingRole").and_then(Value::as_str) == Some("attendee"),
        hand: entry
            .get("publishedStates")
            .and_then(Value::as_array)
            .and_then(|states| {
                states
                    .iter()
                    .find(|state| state.get("stateType").and_then(Value::as_str) == Some(teams_proto::RAISE_HANDS))
            })
            .and_then(|state| state.get("stateId")?.as_str())
            .map(str::to_owned),
        organizer: entry.get("meetingRole").and_then(Value::as_str) == Some("organizer"),
        joined: endpoints.and_then(|endpoints| {
            endpoints
                .values()
                .filter(|endpoint| endpoint.get("call").is_some_and(Value::is_object))
                .find_map(|endpoint| endpoint.get("endpointJoinTime")?.as_str())
                .map(str::to_owned)
        }),
    })
}

/// The organizer's mute of an entry's main `audio`, with the version of its endpoint's mutes it came as.
fn server_mute(audio: Option<&Value>, endpoints: Option<&serde_json::Map<String, Value>>) -> Option<u64> {
    audio
        .filter(|audio| audio.get("serverMuted").and_then(Value::as_bool) == Some(true))
        .map(|_| {
            endpoints
                .and_then(|endpoints| {
                    endpoints
                        .values()
                        .find_map(|endpoint| endpoint.pointer("/call/serverMuteVersion")?.as_u64())
                })
                .unwrap_or(0)
        })
}

/// Whether an endpoint of the entry in the call reports its microphone muted.
fn reports_muted(endpoints: Option<&serde_json::Map<String, Value>>) -> bool {
    endpoints.is_some_and(|endpoints| {
        endpoints
            .values()
            .filter(|endpoint| endpoint.get("call").is_some_and(Value::is_object))
            .any(|endpoint| {
                endpoint
                    .pointer("/endpointState/state/isMuted")
                    .and_then(Value::as_bool)
                    == Some(true)
            })
    })
}

/// A live reaction delivered at the conversation's `receiveMessage` callback (`{"from": {"id", "displayName"},
/// "type": "Reaction", "payload": {"values": [{"name", "count"}]}}`, seen 2026-11-06): who sent it, their name as
/// given, and the reaction's name; `None` for any other message.
pub(crate) fn reaction(body: &[u8]) -> Option<(Mri, Option<String>, String)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    if body.get("type").and_then(Value::as_str) != Some("Reaction") {
        return None;
    }
    let from = body.get("from")?;
    let mri = Mri::new(from.get("id")?.as_str()?);
    let name = from
        .get("displayName")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let reaction = body.pointer("/payload/values/0/name")?.as_str()?.to_owned();
    Some((mri, name, reaction))
}

/// A renegotiation delivered at the account's `mediaRenegotiation` callback (`{"mediaNegotiation": {"mediaContent",
/// "links": {"mediaAnswer", …}}}`): its offer and the link the answer goes to; `None` unless both read.
#[cfg(test)]
fn negotiation(body: &[u8]) -> Option<(MediaContent, CallLink)> {
    negotiation_in(&teams_proto::Cloud::Microsoft, body)
}

pub(crate) fn negotiation_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<(MediaContent, CallLink)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let negotiation = body.get("mediaNegotiation")?;
    Some((
        media(negotiation.get("mediaContent")?)?,
        link(cloud, negotiation, "/links/mediaAnswer")?,
    ))
}

/// The answer to the account's renegotiation, delivered at its `mediaAnswer` callback (`{"mediaAnswer":
/// {"mediaContent", "links": {"mediaAcknowledgement"}}}`): the answer, and the link its acknowledgement goes to where
/// it reads; `None` without a readable answer.
#[cfg(test)]
fn media_answer(body: &[u8]) -> Option<(MediaContent, Option<CallLink>)> {
    media_answer_in(&teams_proto::Cloud::Microsoft, body)
}

pub(crate) fn media_answer_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<(MediaContent, Option<CallLink>)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let answer = body.get("mediaAnswer")?;
    Some((
        media(answer.get("mediaContent")?)?,
        link(cloud, answer, "/links/mediaAcknowledgement"),
    ))
}

/// Whether a rejection of the account's renegotiation (`{"mediaNegotiationFailure": {"code": 491, "phrase":
/// "Negotiation is in progress.", "resultCategories": ["ExpectedError"]}}`, `ab-call --call video`, when both pages
/// turned their cameras on together) says another negotiation of the call was in progress: the renegotiation may go
/// again once it is over.
pub(crate) fn negotiation_in_progress(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body.pointer("/mediaNegotiationFailure/code").and_then(Value::as_i64))
        == Some(491)
}

/// How the call's services ended a call or a ring: the `callEnd` (or a conversation's end) as delivered. Each part only
/// where the delivery gives it; nothing is read into a code no capture named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteEnd {
    pub code: Option<i64>,
    pub sub_code: Option<i64>,
    pub phrase: Option<String>,
    /// The conversation a conversation's end names (`conversationUrl.Location`, its query left out): a start refused
    /// because the chat already has a running call names that call's (code 409, sub-code 5704).
    pub conversation: Option<String>,
    /// Whose action ended it: the end's `sender` (a conversation's end's `callControllerTransactionEnd.sender`), its
    /// MRI. A far end that hung up sends the end as its own, so its phrase reads `CallEndReasonLocalUserInitiated`
    /// ("local" to the sender, not to the account); `None` when the service ended it by itself (`sender: null`).
    pub by: Option<Mri>,
    /// The sender's name as the end gives it (`displayName`).
    pub by_name: Option<String>,
    /// The end's `resultCategories` as delivered: `Success`, or for a call the service gave up on `JoinFailure`,
    /// `MediaNegotiationFailure`, ….
    pub categories: Vec<String>,
}

impl RemoteEnd {
    /// Whether the service ended the call at once because its chat already has a running call: a conversation's end
    /// of code 409, sub-code 5704 ("failure to resolve ownership", seen 2026-10-26).
    #[must_use]
    pub fn chat_has_running_call(&self) -> bool {
        self.code == Some(409) && self.sub_code == Some(5704)
    }

    /// Whether the service gave the call up as a failure: a result category other than `Success`.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.categories.iter().any(|category| category != "Success")
    }
}

/// The end `body` delivers (`{"callEnd": {…}}`, or a conversation's end); every part `None` when it doesn't read.
pub(crate) fn remote_end(body: &[u8]) -> RemoteEnd {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return RemoteEnd::default();
    };
    // a conversation's end may also come bare (`{"code": 409, "subCode", "phrase", "conversationUrl", …}`, a start in a
    // chat whose call still runs: `docs/protocol/calling.md`, "Joining a running call")
    let end = body
        .get("callEnd")
        .or_else(|| body.get("conversationEnd"))
        .unwrap_or(&body);
    // a conversation's end names who ended the call under its call controller's part, its own `sender` null
    let sender = end
        .get("sender")
        .filter(|sender| sender.is_object())
        .or_else(|| end.pointer("/callControllerTransactionEnd/sender"))
        .filter(|sender| sender.is_object());
    let text = |key: &str| {
        sender
            .and_then(|sender| sender.get(key))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    RemoteEnd {
        code: end.get("code").and_then(Value::as_i64),
        sub_code: end.get("subCode").and_then(Value::as_i64),
        phrase: end.get("phrase").and_then(Value::as_str).map(str::to_owned),
        conversation: end
            .pointer("/conversationUrl/Location")
            .and_then(Value::as_str)
            .map(|url| url.split(['?', '#']).next().unwrap_or_default().to_owned()),
        by: text("id").map(Mri::new),
        by_name: text("displayName"),
        categories: end
            .get("resultCategories")
            .and_then(Value::as_array)
            .map(|categories| categories.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use serde_json::json;

    use super::{accept_answer, acceptance, attached, conversation, invitation, remote_end, roster};

    const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
    const CALLER: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const CALLEE: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

    fn conv(rest: &str) -> String {
        format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/Conversation1{rest}?i=10-0-0-2&e=1")
    }

    fn cc(kind: &str, op: &str) -> String {
        format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/{kind}/Call1/27/t/2/{op}?i=10-0-0-1")
    }

    fn invitation_body(evt: u64, gp: &serde_json::Value) -> Vec<u8> {
        let gp = base64::engine::general_purpose::STANDARD.encode(gp.to_string());
        json!({"evt": evt, "gp": gp}).to_string().into_bytes()
    }

    fn gp() -> serde_json::Value {
        json!({
            "callNotification": {
                "from": {"id": CALLER, "displayName": "Lab A"},
                "to": {"id": CALLEE},
                "links": {"attach": cc("forked", "attach")},
                "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0", "mediaLegId": "LEG"},
            },
            "conversationInvitation": {"conversationController": conv("")},
        })
    }

    #[test]
    fn an_invitation_reads_its_parties_links_and_offer() {
        let read = invitation(&invitation_body(107, &gp())).unwrap();
        assert_eq!(read.caller.as_str(), CALLER);
        assert_eq!(read.caller_name.as_deref(), Some("Lab A"));
        assert_eq!(read.callee.as_str(), CALLEE);
        assert_eq!(read.group_chat, None);
        assert_eq!(read.conversation.as_str(), conv(""));
        assert_eq!(read.attach.as_str(), cc("forked", "attach"));
        let offer = read.offer.unwrap();
        assert_eq!(
            (offer.blob.as_str(), offer.media_leg_id.as_deref()),
            ("v=0\r\n", Some("LEG"))
        );
        let mut group = gp();
        group["groupChat"] = json!({"threadId": "19:group@thread.v2", "messageId": "0"});
        let read = invitation(&invitation_body(109, &group)).unwrap();
        assert_eq!(read.group_chat.as_deref(), Some("19:group@thread.v2"));
    }

    #[test]
    fn a_body_that_is_no_readable_invitation_is_none() {
        assert!(
            invitation(&invitation_body(115, &gp())).is_none(),
            "a participant event"
        );
        assert!(invitation(b"{\"evt\":107,\"gp\":\"not base64!\"}").is_none());
        let mut elsewhere = gp();
        elsewhere["callNotification"]["links"]["attach"] = json!("https://cc.example.test/attach");
        assert!(
            invitation(&invitation_body(107, &elsewhere)).is_none(),
            "an attach off the flight proxy"
        );
        let mut nobody = gp();
        nobody["callNotification"]["from"] = json!({});
        assert!(invitation(&invitation_body(107, &nobody)).is_none());
    }

    #[test]
    fn answers_hand_out_the_links_the_machine_sends_to() {
        let answer = json!({
            "conversationController": conv(""),
            "links": {"leave": conv("/leave"), "updateEndpointMetadata": conv("/updateEndpointMetadata"),
                "addParticipantAndModality": conv("/add"), "getAllParticipants": conv("/getAllParticipants")},
        });
        let read = conversation(answer.to_string().as_bytes()).unwrap();
        assert_eq!(read.leave.unwrap().as_str(), conv("/leave"));
        assert_eq!(read.add.unwrap().as_str(), conv("/add"));
        assert!(conversation(b"{\"links\":{}}").is_none(), "no conversation");

        let attach = json!({
            "callInvitation": {"links": {"progress": cc("incoming", "progress"),
                "acceptance": cc("incoming", "accept"), "callLeg": cc("incoming", "reject")}},
            "additionalActionResponses": [{"url": conv(""), "output": {"links": {
                "leave": conv("/leave"), "updateEndpointState": conv("/updateEndpointState")}}}],
        });
        let controller = teams_proto::CallLink::parse(&conv("")).unwrap();
        let read = attached(attach.to_string().as_bytes(), &controller).unwrap();
        assert_eq!(read.acceptance.as_str(), cc("incoming", "accept"));
        assert_eq!(read.call_leg.as_str(), cc("incoming", "reject"));
        assert_eq!(
            read.conversation.endpoint_state.unwrap().as_str(),
            conv("/updateEndpointState")
        );

        let accept = json!({"callAcceptanceAcknowledgement": {
            "links": {"mediaRenegotiation": cc("active", "renegotiate")}, "callKeepAliveInterval": 2700}});
        let read = accept_answer(accept.to_string().as_bytes());
        assert_eq!(read.keep_alive, Some(2700));
        assert_eq!(read.renegotiation.unwrap().as_str(), cc("active", "renegotiate"));
        let none = json!({"callAcceptanceAcknowledgement": {"callKeepAliveInterval": null}});
        assert_eq!(
            accept_answer(none.to_string().as_bytes()),
            super::AcceptAnswer::default()
        );
        assert_eq!(accept_answer(b"not json"), super::AcceptAnswer::default());
    }

    #[test]
    fn a_renegotiation_and_the_answer_to_one_read_their_media_and_links() {
        let offer = json!({"mediaNegotiation": {
            "mediaContent": {"blob": "v=0", "contentType": "application/sdp-ngc-1.0"},
            "links": {"mediaAnswer": cc("negotiations", "answer"), "rejection": cc("negotiations", "reject")},
        }});
        let (media, answer) = super::negotiation(offer.to_string().as_bytes()).unwrap();
        assert_eq!(media.content_type, "application/sdp-ngc-1.0");
        assert_eq!(answer.as_str(), cc("negotiations", "answer"));
        let unlinked = json!({"mediaNegotiation": {"mediaContent": {"blob": "v=0", "contentType": "x"}}});
        assert!(super::negotiation(unlinked.to_string().as_bytes()).is_none());

        let answered = json!({"mediaAnswer": {
            "mediaContent": {"blob": "v=0", "contentType": "application/sdp-ngc-1.0"},
            "links": {"mediaAcknowledgement": cc("mediaAnswers", "acknowledge")},
        }});
        let (_, acknowledgement) = super::media_answer(answered.to_string().as_bytes()).unwrap();
        assert_eq!(acknowledgement.unwrap().as_str(), cc("mediaAnswers", "acknowledge"));
        let empty = json!({"mediaAnswer": {}});
        assert!(super::media_answer(empty.to_string().as_bytes()).is_none());
    }

    #[test]
    fn an_acceptance_reads_its_acknowledgement_answer_and_keep_alive() {
        let body = json!({"callAcceptance": {
            "links": {"acknowledgement": cc("callAcceptance", "acknowledge")},
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
            "callKeepAliveInterval": 2700,
        }});
        let read = acceptance(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            read.acknowledgement.unwrap().as_str(),
            cc("callAcceptance", "acknowledge")
        );
        assert_eq!(read.keep_alive, Some(2700));
        assert!(read.answer.is_some());
        assert_eq!(read.renegotiation, None);
        assert!(!read.voicemail, "taken by the callee");
        assert!(acceptance(b"{}").is_none());
        let voicemail = json!({"callAcceptance": {"acceptedBy": {"endpointType": "voicemail"}}});
        assert!(acceptance(voicemail.to_string().as_bytes()).unwrap().voicemail);
    }

    #[test]
    fn an_end_keeps_what_it_delivers_and_nothing_else() {
        let end = remote_end(br#"{"callEnd":{"code":487,"subCode":0,"phrase":"CallEndReasonLocalUserInitiated"}}"#);
        assert_eq!(
            (end.code, end.sub_code, end.phrase.as_deref()),
            (Some(487), Some(0), Some("CallEndReasonLocalUserInitiated"))
        );
        assert_eq!(remote_end(b"not json"), super::RemoteEnd::default());
        assert_eq!(remote_end(b"{\"callEnd\":{\"code\":\"x\"}}").code, None);
        let bare = remote_end(br#"{"code":409,"subCode":5704,"phrase":"Conversation cannot be created"}"#);
        assert_eq!((bare.code, bare.sub_code), (Some(409), Some(5704)));
        assert!(bare.chat_has_running_call());
        assert_eq!(bare.conversation, None);
        assert!(!end.chat_has_running_call());
        assert_eq!((end.by, end.by_name), (None, None));
    }

    #[test]
    fn an_end_names_who_ended_the_call() {
        let b = "8:orgid:00000000-0000-4000-8000-00000000000b";
        // the far end's hang-up, in the captured shape: its own end, "local" to it
        let hung_up = json!({"callEnd": {"reason": "noError", "sender": {"id": b, "displayName": "Synthetic B",
            "endpointId": "e", "participantId": "p", "hidden": false}, "code": 0, "subCode": 0,
            "phrase": "CallEndReasonLocalUserInitiated", "resultCategories": ["Success"]}});
        let end = remote_end(hung_up.to_string().as_bytes());
        assert_eq!(end.by.as_ref().map(teams_proto::Mri::as_str), Some(b));
        assert_eq!(end.by_name.as_deref(), Some("Synthetic B"));
        assert!(!end.failed());
        // a conversation's end names it under its call controller's part
        let all_left = json!({"code": 0, "subCode": 5002, "phrase": "All left.", "sender": null,
            "resultCategories": ["Success"], "callControllerTransactionEnd": {"sender": {"id": b,
            "displayName": "Synthetic B"}, "phrase": "CallEndReasonLocalUserInitiated"}});
        let end = remote_end(all_left.to_string().as_bytes());
        assert_eq!(
            (end.sub_code, end.by_name.as_deref()),
            (Some(5002), Some("Synthetic B"))
        );
        // the service's own end names nobody; a failure says so by its categories
        let failed = json!({"callEnd": {"reason": "clientError", "sender": null, "code": 0, "subCode": 301_000,
            "phrase": "SdpParsingFailure", "resultCategories": ["JoinFailure", "MediaNegotiationFailure"]}});
        let end = remote_end(failed.to_string().as_bytes());
        assert_eq!((end.by.as_ref(), end.failed()), (None, true));
    }

    #[test]
    fn a_start_refused_by_a_running_call_names_that_call_s_conversation() {
        let conversation = format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/SyntheticConversation9");
        let refused = json!({
            "code": 409,
            "subCode": 5704,
            "phrase": "Conversation cannot be created due to a failure to resolve ownership.",
            "conversationUrl": {"Location": format!("{conversation}?i=10-0-0-1&e=1")},
        });
        let end = remote_end(refused.to_string().as_bytes());
        assert!(end.chat_has_running_call());
        assert_eq!(end.conversation.as_deref(), Some(conversation.as_str()));
    }

    #[test]
    fn a_one_to_one_roster_s_parties_are_in_the_call_without_media_streams() {
        // a 1:1 call's roster, in the captured shape: its endpoints' `call` lists no media streams
        let party = |name: &str| {
            json!({
                "version": 1,
                "state": "active",
                "details": {"displayName": name},
                "endpoints": {"00000000-0000-4000-8000-000000000001": {
                    "call": {"serverMuteVersion": 0},
                    "modalityJoined": "Call",
                }},
            })
        };
        let body = json!({
            "participants": {CALLER: party("Lab A"), CALLEE: party("Lab B")},
            "type": "Delta",
            "sequenceNumber": 1,
        });
        let mut read = roster(body.to_string().as_bytes()).unwrap();
        read.sort_by(|one, other| one.mri.as_str().cmp(other.mri.as_str()));
        assert_eq!(read.len(), 2);
        assert!(read.iter().all(|entry| entry.active), "{read:?}");
        assert_eq!(read[0].name.as_deref(), Some("Lab A"));
        assert_eq!(
            (read[0].audio_source, read[0].video_source, read[0].video_on),
            (None, None, false)
        );
    }

    #[test]
    fn a_group_call_s_sharer_is_read_by_its_sharing_stream_going_out() {
        // a group call's roster in the captured shape (2026-11-06): the sharer's `applicationsharing-video` stream
        // `recvonly` while it doesn't share, `sendonly` while it does
        let sharing = |direction: &str| {
            let body = json!({"participants": {CALLER: {"version": 3, "state": "active", "endpoints": {"e1": {
            "call": {"mediaStreams": [
                {"type": "audio", "label": "main-audio", "sourceId": 201, "direction": "sendrecv"},
                {"type": "video", "label": "main-video", "sourceId": 202, "direction": "recvonly"},
                {"type": "applicationsharing-video", "label": "applicationsharing-video", "sourceId": 212,
                    "direction": direction},
            ]}}}}}});
            roster(body.to_string().as_bytes()).unwrap()[0].sharing_source
        };
        assert_eq!(sharing("recvonly"), None);
        assert_eq!(sharing("sendonly"), Some(212));
        assert_eq!(sharing("sendrecv"), Some(212));
    }

    #[test]
    fn a_party_is_in_the_call_only_while_active_with_an_endpoint_in_it() {
        let active = |state: &str, endpoints: &serde_json::Value| {
            let body = json!({"participants": {CALLER: {"version": 2, "state": state, "endpoints": endpoints}}});
            roster(body.to_string().as_bytes()).unwrap()[0].active
        };
        let in_call = json!({"e1": {"call": {"serverMuteVersion": 0}}});
        let streams = json!({"e1": {"call": {"mediaStreams": [
            {"type": "audio", "label": "main-audio", "sourceId": 10, "direction": "sendrecv"},
        ]}}});
        assert!(active("active", &in_call), "a 1:1 call's");
        assert!(active("active", &streams), "a group call's, its streams listed");
        assert!(!active("inactive", &json!({})), "gone");
        assert!(!active("inactive", &in_call));
        assert!(!active("active", &json!({})), "no endpoint");
        assert!(
            !active("active", &json!({"e1": {"modalityJoined": "Call"}})),
            "no endpoint in the call"
        );
    }
}

/// Who an `add` failed to bring in, as its `addParticipantFailure` delivery names them (`{"participants": {<MRI>:
/// {"code", "phrase", …}}, "participantInfos": […]}`, seen 2026-11-06 when the invitee had no endpoint to ring).
pub(crate) fn add_failed(body: &[u8]) -> Vec<Mri> {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    body.get("participants")
        .and_then(Value::as_object)
        .map(|participants| participants.keys().map(Mri::new).collect())
        .unwrap_or_default()
}

/// The group chat a conversation update names (`groupChat.threadId`): a 1:1 call's new group chat once someone was
/// added to it (seen 2026-11-06, both ends told).
pub(crate) fn update_group_chat(body: &[u8]) -> Option<String> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let chat = body.pointer("/groupChat/threadId")?.as_str()?;
    teams_proto::is_group_chat_id(chat).then(|| chat.to_owned())
}

/// A transfer the far end asked for, delivered at the account's `transfer` callback (`{"callTransfer": {"target":
/// {"id"}, "isConsultative", "transferor": {"details", "authorizationToken"}, "links": {"transferAcceptance",
/// "transferCompletion"}}}`, seen 2026-11-06): whom to call, who transferred, and where the transfer is taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TransferAsked {
    pub(crate) target: Mri,
    pub(crate) transferor: teams_proto::Transferor,
    pub(crate) acceptance: CallLink,
}

/// The transfer `body` asks for; `None` unless it is a blind transfer (a consultative one wasn't captured) whose
/// target, transferor and acceptance link read.
pub(crate) fn transfer_asked_in(cloud: &teams_proto::Cloud, body: &[u8]) -> Option<TransferAsked> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let transfer = body.get("callTransfer")?;
    if transfer.get("isConsultative").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let target = Mri::new(transfer.pointer("/target/id")?.as_str()?);
    let details = transfer.pointer("/transferor/details")?.clone();
    details.get("id")?.as_str()?;
    let authorization = transfer
        .pointer("/transferor/authorizationToken")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(TransferAsked {
        target,
        transferor: teams_proto::Transferor::new(details, authorization),
        acceptance: link(cloud, transfer, "/links/transferAcceptance")?,
    })
}
