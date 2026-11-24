//! The call controller's wire (`docs/protocol/calling.md`): the links it hands out, all under
//! `<origin>/flightproxy/api/v2/ep/`, the answers of its requests and the bodies it delivers to the parties'
//! callbacks, in the shapes the client reads (`tests/fixtures/calling/`).

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use super::state::{Call, KEEP_ALIVE, Kind, Party, Seat, Showing, Status};
use teams_sdp::sdp::SHARING_LABEL;

/// A party's callbacks, by the name its request gave each.
pub(crate) type Links = BTreeMap<String, String>;

/// The published state of a raised hand.
pub(crate) const RAISE_HANDS: &str = "raiseHands";

/// The names the conversation service and the call controller have behind the flight proxy.
pub(crate) const CONVERSATION_HOST: &str = "conv.own";
pub(crate) const CONTROLLER_HOST: &str = "cc.own";

/// A time as Teams writes it: seven fractional digits.
pub(crate) fn iso(ms: i64) -> String {
    let time = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    format!("{}0Z", time.format("%Y-%m-%dT%H:%M:%S%.6f"))
}

/// A callback's endpoint and its path under the endpoint's push URL, when `link` is a call agent's callback under
/// this server's Trouter.
pub(crate) fn callback<'a>(origin: &str, link: &'a str) -> Option<(&'a str, &'a str)> {
    let rest = link.strip_prefix(origin)?.strip_prefix("/trouter/v4/f/")?;
    let (endpoint, path) = rest.split_once('/')?;
    (!endpoint.is_empty() && path.starts_with("callAgent/") && !path.contains(['?', '#'])).then_some((endpoint, path))
}

/// Every callback `body` hands out, wherever in it, by its name (the first of a name).
pub(crate) fn links_in(origin: &str, body: &Value) -> Links {
    let mut links = Links::new();
    collect(origin, body, &mut links);
    links
}

fn collect(origin: &str, value: &Value, links: &mut Links) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                match value {
                    Value::String(link) if callback(origin, link).is_some() => {
                        links.entry(name.clone()).or_insert_with(|| link.clone());
                    }
                    other => collect(origin, other, links),
                }
            }
        }
        Value::Array(values) => values.iter().for_each(|value| collect(origin, value, links)),
        _ => {}
    }
}

/// Whether `body` names a URL that is no callback of this server's Trouter where a callback belongs: under `links`,
/// or as a roster's `rosterUpdate`.
pub(crate) fn foreign_callback(origin: &str, body: &Value) -> bool {
    fn links_foreign(origin: &str, links: &Value) -> bool {
        links.as_object().is_some_and(|links| {
            links
                .values()
                .filter_map(Value::as_str)
                .any(|link| callback(origin, link).is_none())
        })
    }
    match body {
        Value::Object(fields) => fields.iter().any(|(name, value)| match name.as_str() {
            "links" => links_foreign(origin, value),
            "rosterUpdate" => value.as_str().is_some_and(|link| callback(origin, link).is_none()),
            _ => foreign_callback(origin, value),
        }),
        Value::Array(values) => values.iter().any(|value| foreign_callback(origin, value)),
        _ => false,
    }
}

pub(crate) fn conversation_url(origin: &str, conversation: &str, op: Option<&str>) -> String {
    let op = op.map(|op| format!("/{op}")).unwrap_or_default();
    format!("{origin}/flightproxy/api/v2/ep/{CONVERSATION_HOST}/conv/{conversation}{op}")
}

/// The `endpointId` of an endpoint nobody named.
pub(crate) const ZERO_ENDPOINT: &str = "00000000-0000-0000-0000-000000000000";

/// The leg a call controller link names: a 1:1 call's are all `1`, a group call's the party's seat.
pub(crate) fn leg_of(call: &Call, seat: Seat) -> usize {
    match call.kind {
        Kind::OneToOne => 1,
        Kind::Group => seat.0,
    }
}

/// A call controller link of the party at `seat`: `…/cc/v1/<kind>/<call>/<leg>/<at>/<number>[/<op>]`.
fn controller_url(
    origin: &str,
    (call, seat): (&Call, Seat),
    (kind, at, number): (&str, &str, u64),
    op: Option<&str>,
) -> String {
    let op = op.map(|op| format!("/{op}")).unwrap_or_default();
    let (id, leg) = (&call.id, leg_of(call, seat));
    format!("{origin}/flightproxy/api/v2/ep/{CONTROLLER_HOST}/cc/v1/{kind}/{id}/{leg}/{at}/{number}{op}")
}

/// The party as it names itself in `named`, with the id and name the server knows it by.
pub(crate) fn named_by(named: &Value, party: &Party) -> Value {
    let text = |key: &str| named.get(key).and_then(Value::as_str);
    json!({
        "id": party.mri(),
        "displayName": party.name,
        "endpointId": text("endpointId").unwrap_or(ZERO_ENDPOINT),
        "participantId": text("participantId"),
        "languageId": text("languageId"),
    })
}

/// The conversation's links a party operates on; a group call's also where someone is added to it.
fn conversation_links(origin: &str, call: &Call) -> Value {
    let link = |op: &str| conversation_url(origin, &call.conversation, Some(op));
    let mut links = json!({
        "leave": link("leave"),
        "updateEndpointMetadata": link("updateEndpointMetadata"),
        "updateEndpointState": link("updateEndpointState"),
        "updateParticipantProperties": link("updateParticipantProperties"),
        "notificationLinks": link("notificationLinks"),
    });
    if call.kind == Kind::Group {
        links["addParticipant"] = json!(link("add"));
        links["addParticipantAndModality"] = json!(link("add"));
        for op in ["publishState", "removeState", "sendMessage"] {
            links[op] = json!(link(op));
        }
    }
    if call.meeting.is_some() {
        for op in ["admit", "removeParticipant", "mute"] {
            links[op] = json!(link(op));
        }
    }
    links
}

/// The conversation's state: a group call's is multi-party, started by its caller.
fn conversation_state(call: &Call) -> Value {
    match call.kind {
        Kind::OneToOne => json!({"isMultiParty": false, "groupCallInitiator": null, "isBroadcast": false}),
        Kind::Group => {
            let mut state = json!({
                "isMultiParty": true, "groupCallInitiator": call.party(Seat::CALLER).mri(), "isBroadcast": false,
            });
            if let Some(meeting) = &call.meeting {
                let kind = if meeting.channel {
                    "adHocMeeting"
                } else {
                    "scheduledMeeting"
                };
                state["conversationType"] = json!(kind);
                state["isHostless"] = json!(true);
                state["isMeetingActivated"] = json!(true);
            }
            state
        }
    }
}

/// The roster a conversation's answer gives: a 1:1 call's none, a group call's whole.
fn answered_roster(call: &Call) -> Value {
    match call.kind {
        Kind::OneToOne => json!({"type": "Delta", "sequenceNumber": call.roster_sequence, "participants": {}}),
        Kind::Group => roster(call, &call.rostered()),
    }
}

/// The conversation as a start or a subscription answers it.
pub(crate) fn conversation(origin: &str, region: &str, call: &Call) -> Value {
    json!({
        "conversationController": conversation_url(origin, &call.conversation, None),
        "sequenceNumber": 1,
        "subject": "",
        "region": region,
        "conversationStartTime": iso(call.started_at),
        "state": conversation_state(call),
        "roster": answered_roster(call),
        "links": conversation_links(origin, call),
        "meetingDetails": meeting_details(call),
    })
}

/// A meeting's options in effect, as a conversation tells them (`meetingDetails`); none in a call that is no meeting's.
fn meeting_details(call: &Call) -> Value {
    let Some(meeting) = &call.meeting else {
        return Value::Null;
    };
    let controls = &meeting.controls;
    let or = |value: &str, default: &'static str| {
        if value.is_empty() {
            default.to_owned()
        } else {
            value.to_owned()
        }
    };
    let restrictions = match controls.restrictions.as_str() {
        "DisableAudio,DisableVideo" => "DisableAv".to_owned(),
        other => or(other, "UnRestricted"),
    };
    json!({"meetingCapability": {
        "autoAdmittedUsers": or(&controls.lobby, "EveryoneInCompany"),
        "presenterOption": or(&controls.presenters, "Everyone"),
        "attendeeRestrictions": restrictions,
    }})
}

/// What everyone in a meeting is told at its `conversationUpdate` callback when its options change.
pub(crate) fn conversation_update(call: &Call) -> Value {
    json!({"meetingDetails": meeting_details(call)})
}

/// A conversation that never runs: the answer of a start refused at once at its `conversationEnd` callback.
pub(crate) fn unrun_conversation(origin: &str, region: &str, conversation: &str) -> Value {
    json!({
        "conversationController": conversation_url(origin, conversation, None),
        "sequenceNumber": 1,
        "subject": "",
        "region": region,
        "state": {"isMultiParty": true, "groupCallInitiator": null, "isBroadcast": false},
        "roster": {"type": "Delta", "sequenceNumber": 0, "participants": {}},
        "links": {"leave": conversation_url(origin, conversation, Some("leave"))},
    })
}

/// What a start in a chat whose call still runs is told at its `conversationEnd` callback: `409` / `5704`, naming the
/// running call's conversation and the chat (`docs/protocol/calling.md`, "Joining a running call").
pub(crate) fn running_elsewhere(origin: &str, running: &Call) -> Value {
    json!({
        "conversationEnd": {
            "code": 409,
            "subCode": 5704,
            "phrase": "Conversation cannot be created due to a failure to resolve ownership",
            "resultCategories": ["ExpectedError"],
            "conversationUrl": {"Location": conversation_url(origin, &running.conversation, None)},
            "threadId": running.chat,
        },
    })
}

/// A party as an invitation names it.
fn named_in_invitation(party: &Party, tenant: &str) -> Value {
    let mut named = party.participant.clone();
    named["displayNameSource"] = json!("substrate");
    named["hidden"] = json!(false);
    named["tenantId"] = json!(tenant);
    named["propertyBag"] = Value::Null;
    named
}

/// An invitation's `from` (the party at `by` that calls or adds, with its region) and `to` (the party at `seat`, at
/// no endpoint yet).
fn invited(call: &Call, (by, seat): (Seat, Seat), (tenant, region): (&str, &str)) -> (Value, Value) {
    let mut from = named_in_invitation(call.party(by), tenant);
    from["region"] = json!(region);
    let mut to = named_in_invitation(call.party(seat), tenant);
    to["endpointId"] = json!(ZERO_ENDPOINT);
    (from, to)
}

/// The callee's invitation: `{"evt": 107, "gp": <base64 JSON>}`, with the caller's offer.
pub(crate) fn invitation(origin: &str, call: &Call, tenant: &str, region: &str) -> Value {
    let forked = |op: &str| controller_url(origin, (call, Seat::CALLEE), ("forked", "i1", 1), Some(op));
    let (from, to) = invited(call, (Seat::CALLER, Seat::CALLEE), (tenant, region));
    let gp = json!({
        "callNotification": {
            "from": from,
            "to": to,
            "links": {
                "attach": forked("attach"),
                "mediaAnswer": "cc://ma",
                "progress": forked("progress"),
                "reject": forked("reject"),
            },
            "mediaContent": call.offer,
            "replaces": null,
            "voicemailSettings": {},
        },
        "conversationInvitation": {
            "conversationController": conversation_url(origin, &call.conversation, None),
            "isMultiParty": false,
            "isBroadcast": false,
        },
        "debugContent": {
            "callId": call.id,
            "callStartTime": iso(call.started_at),
            "participantId": call.party(Seat::CALLEE).participant["participantId"],
        },
        "groupContext": null,
    });
    json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

/// The answer of an `attach` of the party at `seat`: the ringing leg's links and the conversation its subscription
/// joined.
pub(crate) fn attach_answer(origin: &str, call: &Call, seat: Seat) -> Value {
    let incoming = |op: &str| controller_url(origin, (call, seat), ("incoming", "t", 1), Some(op));
    let controller = conversation_url(origin, &call.conversation, None);
    json!({
        "participants": {"from": call.party(Seat::CALLER).participant, "to": call.party(seat).participant},
        "callInvitation": {
            "callModalities": ["audio"],
            "links": {
                "progress": incoming("progress"),
                "acceptance": incoming("accept"),
                "callLeg": incoming("reject"),
            },
            "mediaContent": null,
            "replaces": null,
            "voicemailSettings": {},
        },
        "additionalActionResponses": [{
            "url": controller,
            "output": {
                "conversationController": controller,
                "sequenceNumber": 1,
                "roster": answered_roster(call),
                "state": conversation_state(call),
                "links": conversation_links(origin, call),
            },
        }],
    })
}

/// The links of the party at `seat` on the call that is up: its call leg (the keep-alive's), renegotiation, hold
/// (a 1:1 call's only) and the media lines' update.
fn leg_links(origin: &str, (call, seat): (&Call, Seat), at: &str) -> Map<String, Value> {
    let active = |op: Option<&str>| controller_url(origin, (call, seat), ("active", at, 1), op);
    let mut links = Map::new();
    links.insert("callLeg".into(), json!(active(None)));
    links.insert("mediaRenegotiation".into(), json!(active(Some("renegotiate"))));
    links.insert(
        "updateMediaDescriptions".into(),
        json!(active(Some("updateMediaDescriptions"))),
    );
    if call.kind == Kind::OneToOne {
        links.insert("hold".into(), json!(active(Some("hold"))));
    }
    links.insert(
        "applyChannelParameters".into(),
        json!(controller_url(
            origin,
            (call, seat),
            ("mcProxy", at, 1),
            Some("applyChannelParameters")
        )),
    );
    links
}

/// The answer of the `accept` of the party at `seat`: its acknowledgement, with the call leg's links.
pub(crate) fn accept_answer(origin: &str, call: &Call, seat: Seat) -> Value {
    json!({
        "callAcceptanceAcknowledgement": {
            "links": leg_links(origin, (call, seat), "a4"),
            "callKeepAliveInterval": KEEP_ALIVE,
        },
        "debugContent": null,
    })
}

/// The caller's acceptance: who answered, its answer (`media`) and the call leg's links.
pub(crate) fn acceptance(origin: &str, call: &Call, media: &Value) -> Value {
    let mut links = leg_links(origin, (call, Seat::CALLER), "k1");
    links.insert(
        "acknowledgement".into(),
        json!(controller_url(
            origin,
            (call, Seat::CALLER),
            ("callAcceptance", "a1", 1),
            Some("acknowledge")
        )),
    );
    json!({
        "callAcceptance": {
            "acceptedBy": call.party(Seat::CALLEE).participant,
            "acceptedCallModalities": ["Audio"],
            "links": links,
            "mediaContent": media,
            "callKeepAliveInterval": KEEP_ALIVE,
        },
        "debugContent": null,
    })
}

/// A call's end as delivered: `sender` whose action ended it, none when the service did.
pub(crate) fn end(code: i64, phrase: &str, sender: Option<&Value>) -> Value {
    json!({
        "callEnd": {
            "code": code, "subCode": 0, "phrase": phrase, "reason": "noError", "resultCategories": ["Success"],
            "sender": sender,
        },
        "debugContent": null,
    })
}

/// The end of a call the service gave up on.
pub(crate) fn failure(code: i64, phrase: &str) -> Value {
    json!({
        "callEnd": {
            "code": code, "subCode": 0, "phrase": phrase, "reason": "clientError",
            "resultCategories": ["UnexpectedServerError"], "sender": null,
        },
        "debugContent": null,
    })
}

/// A renegotiation as the far end is told of it: the offer, and where its answer or its rejection goes.
pub(crate) fn negotiation(origin: &str, call: &Call, sender: &Value, offered: &Value, media: &Value) -> Value {
    let at = |op: &str| {
        let number = call.negotiations;
        controller_url(origin, (call, Seat::CALLER), ("negotiations", "t", number), Some(op))
    };
    json!({
        "mediaNegotiation": {
            "sender": sender,
            "mediaContent": media,
            "callModalities": offered.get("callModalities").cloned().unwrap_or_else(|| json!([])),
            "links": {"mediaAnswer": at("answer"), "rejection": at("reject")},
        },
    })
}

/// A renegotiation's answer as the party that offered is told of it, with where it acknowledges it.
pub(crate) fn media_answer(origin: &str, call: &Call, sender: &str, answer: &Value, media: &Value) -> Value {
    let acknowledge = controller_url(
        origin,
        (call, Seat::CALLER),
        ("mediaAnswers", "t", call.negotiations),
        Some("acknowledge"),
    );
    json!({
        "mediaAnswer": {
            "sender": {"id": sender},
            "mediaContent": media,
            "callModalities": answer.get("callModalities").cloned().unwrap_or_else(|| json!([])),
            "links": {"mediaAcknowledgement": acknowledge},
        },
    })
}

pub(crate) fn media_acknowledgement() -> Value {
    json!({"mediaAcknowledgement": {"code": 0, "subCode": 0, "phrase": "", "resultCategories": ["Success"]}})
}

pub(crate) fn negotiation_failure(code: i64, phrase: &str) -> Value {
    json!({"mediaNegotiationFailure": {"code": code, "phrase": phrase, "resultCategories": ["ExpectedError"]}})
}

/// The answer of a hold: where it is taken back.
pub(crate) fn hold_answer(origin: &str, call: &Call) -> Value {
    let resume = controller_url(origin, (call, Seat::CALLER), ("holdSession", "a6", 1), Some("resume"));
    json!({"holdResponse": {"links": {"resume": resume}}, "debugContent": null})
}

pub(crate) fn hold_completion() -> Value {
    json!({"holdCompletion": {"reason": "noError", "code": 0, "phrase": "Success", "audioBotType": "audioPlayback"}})
}

pub(crate) fn resume_completion() -> Value {
    json!({"resumeCompletion": {"reason": "noError", "code": 0, "phrase": "Success"}})
}

/// The music-on-hold app's roster entry while it plays, or once it left.
pub(crate) fn music_entry(version: u64, playing: bool) -> Value {
    let endpoints = if playing {
        json!({"00000000-0000-4000-8000-0000000000f0": {"call": {
            "mediaStreams": [{"type": "audio", "label": "main-audio", "direction": "sendrecv", "serverMuted": false}],
            "serverMuteVersion": 0,
        }}})
    } else {
        json!({})
    };
    json!({
        "version": version,
        "state": if playing { "active" } else { "inactive" },
        "details": {"id": super::state::MUSIC_ON_HOLD, "displayName": null},
        "endpoints": endpoints,
    })
}

/// What an endpoint in the call sends and takes: a 1:1 call's lists no media streams (its media goes through no
/// mixer); a group call's its main audio, its main video (sent while its camera is on) and, while its share is the
/// call's, its sharing stream, by the party's sources: what the others' source requests name.
fn endpoint_call(call: &Call, party: &Party) -> Value {
    let Some(leg) = party.leg.as_ref().filter(|_| call.kind == Kind::Group) else {
        return json!({"serverMuteVersion": party.mute_version});
    };
    let camera = if party.showing.camera { "sendrecv" } else { "recvonly" };
    let mut streams = vec![
        json!({"type": "audio", "label": "main-audio", "sourceId": leg.sources.audio, "direction": "sendrecv",
            "serverMuted": party.server_muted || call.audio_blocked(party)}),
        json!({"type": "video", "label": "main-video", "sourceId": leg.sources.video, "direction": camera}),
    ];
    if party.presenting {
        streams.push(
            json!({"type": "video", "label": SHARING_LABEL, "sourceId": leg.sources.sharing,
            "direction": "sendonly"}),
        );
    }
    json!({"mediaStreams": streams, "serverMuteVersion": party.mute_version})
}

/// What a group call party sends, as its `updateMediaDescriptions` describes its lines: its camera while a line other
/// than the sharing line goes out, its screen while the sharing line (labelled so) does; none for a body that
/// describes nothing.
pub(crate) fn showing(body: &Value) -> Option<Showing> {
    let described = body
        .get("UpdateMediaDescriptions")
        .or_else(|| body.get("updateMediaDescriptions"))?
        .pointer("/mediaDescriptions/descriptions")?
        .as_array()?;
    let sends = |line: &&Value| {
        line.get("direction")
            .and_then(Value::as_str)
            .is_some_and(|direction| matches!(direction, "sendrecv" | "sendonly"))
    };
    let screen = |line: &&Value| line.get("label").and_then(Value::as_str) == Some(SHARING_LABEL);
    Some(Showing {
        camera: described.iter().filter(sends).any(|line| !screen(&line)),
        sharing: described.iter().filter(sends).any(|line| screen(&line)),
    })
}

/// The bandwidth seed (`sendSideBWSeed`, bits a second) of channel parameters (`applyChannelParameters`, whose
/// `mediaParameter` is a JSON text).
pub(crate) fn seed(parameters: &Value) -> Option<u64> {
    let text = parameters.pointer("/multiChannelParameter/mediaParameter")?.as_str()?;
    let parameter: Value = serde_json::from_str(text).ok()?;
    parameter.pointer("/sendSideBWSeed/seedValueBitsPerSec")?.as_u64()
}

/// One party as the roster lists it: in a meeting's call, with its role there.
fn roster_entry(call: &Call, party: &Party, version: u64) -> Value {
    let endpoint = party.participant["endpointId"].as_str().unwrap_or_default();
    let endpoints = if party.status == Status::Lobby && party.active.is_some() {
        json!({endpoint: {
            "lobby": {"mediaStreams": [
                {"type": "audio", "label": "main-audio", "direction": "sendrecv", "serverMuted": true},
            ]},
            "modalityJoined": "Lobby",
            "participantId": party.participant["participantId"],
        }})
    } else if party.active.is_some() {
        json!({endpoint: {
            "call": endpoint_call(call, party),
            "modalityJoined": "Call",
            "participantId": party.participant["participantId"],
            "endpointJoinTime": party.joined,
            "endpointState": {
                "endpointStateSequenceNumber": party.state_sequence.max(1),
                "state": {"isMuted": party.muted},
            },
        }})
    } else {
        json!({})
    };
    let mut entry = json!({
        "version": version,
        "state": if party.active.is_some() { "active" } else { "inactive" },
        "details": {"id": party.mri(), "displayName": party.name},
        "endpoints": endpoints,
    });
    if let Some(hand) = &party.hand {
        entry["publishedStates"] = json!([{
            "typeRank": hand.rank, "stateType": RAISE_HANDS, "content": hand.content, "stateId": hand.id,
        }]);
    }
    if let Some(role) = call.role_of(party) {
        entry["meetingRole"] = json!(role);
        entry["advancedMeetingRole"] = json!(role);
    }
    entry
}

/// The roster update of `seats`' entries, at the call's current sequence.
pub(crate) fn roster(call: &Call, seats: &[Seat]) -> Value {
    let participants: Map<String, Value> = seats
        .iter()
        .map(|seat| call.party(*seat))
        .map(|party| (party.mri(), roster_entry(call, party, call.roster_sequence)))
        .collect();
    json!({"participants": participants, "type": "Delta", "sequenceNumber": call.roster_sequence})
}

/// A group call's invitation of the party at `seat` from the party at `by`: `{"evt": 109, "gp": <base64 JSON>}`, with
/// the media node's offer to its leg (`deliveries.json`'s group invitation). Its links name the party's ring, so the
/// client doesn't take a later ring of the party for another delivery of an earlier one.
pub(crate) fn group_invitation(
    origin: &str,
    call: &Call,
    (by, seat): (Seat, Seat),
    (tenant, region): (&str, &str),
    offer: &Value,
) -> Value {
    let ring = call.party(seat).rings;
    let forked = |op: &str| controller_url(origin, (call, seat), ("forked", "i1", ring), Some(op));
    let (from, to) = invited(call, (by, seat), (tenant, region));
    let mut media = offer.clone();
    media["fromMixer"] = json!(true);
    media["mediaLegId"] = json!(call.party(seat).leg.as_ref().map(|leg| leg.id.as_str()));
    media["originator"] = json!("mcGvc");
    media["skipIceReinvite"] = json!(true);
    media["newOffer"] = json!(false);
    let gp = json!({
        "callNotification": {
            "from": from,
            "to": to,
            "links": {
                "attach": forked("attach"),
                "mediaAnswer": "cc://ma",
                "progress": forked("progress"),
                "reject": forked("reject"),
            },
            "mediaContent": media,
            "fromMixer": true,
            "conversationType": "groupCall",
            "isDialOut": true,
            "isDialIn": false,
        },
        "groupChat": {"threadId": call.chat, "messageId": "0"},
        "meetingProperties": {"meetingLiveState": {"phase": "staging", "sequenceNumber": 0, "seqNum": 0}},
        "conversationInvitation": {
            "conversationController": conversation_url(origin, &call.conversation, None),
            "isMultiParty": true,
            "isBroadcast": false,
            "groupCallInitiator": call.party(Seat::CALLER).mri(),
            "isHostless": true,
        },
        "debugContent": {
            "callId": call.id,
            "callStartTime": iso(call.started_at),
            "participantId": call.party(seat).participant["participantId"],
        },
        "groupContext": null,
    });
    json!({"evt": 109, "gp": base64::engine::general_purpose::STANDARD.encode(meeting_ask(gp, call, tenant).to_string())})
}

/// A meeting's invitation (its host's Request to join) names the meeting's organizer and tenant and its kind, as Teams
/// sends it (`callNotification.invitationData.meetingInfo`, `conversationType` `scheduledMeeting`; seen 2026-11-24).
fn meeting_ask(mut gp: Value, call: &Call, tenant: &str) -> Value {
    if let Some(meeting) = call.meeting.as_ref() {
        gp["callNotification"]["invitationData"] =
            json!({"meetingInfo": {"organizerId": meeting.organizer, "tenantId": tenant}});
        gp["callNotification"]["conversationType"] = json!("scheduledMeeting");
    }
    gp
}

/// The acceptance the media node gives the party at `seat` on its leg (a group call's caller, a joiner): its `answer`,
/// the call leg's links (`answers.json`'s acceptance callback).
pub(crate) fn mixer_acceptance(origin: &str, call: &Call, seat: Seat, answer: &Value) -> Value {
    let mut links = leg_links(origin, (call, seat), "k1");
    links.insert(
        "acknowledgement".into(),
        json!(controller_url(
            origin,
            (call, seat),
            ("callAcceptance", "a1", 1),
            Some("acknowledge")
        )),
    );
    json!({
        "callAcceptance": {
            "acceptedCallModalities": [],
            "links": links,
            "mediaContent": mixer_media(call, seat, answer),
            "callKeepAliveInterval": KEEP_ALIVE,
        },
        "debugContent": null,
    })
}

/// The media node's offer or answer on the leg of the party at `seat`, as the call's deliveries carry it.
fn mixer_media(call: &Call, seat: Seat, sdp: &Value) -> Value {
    let mut media = sdp.clone();
    media["mediaLegId"] = json!(call.party(seat).leg.as_ref().map(|leg| leg.id.as_str()));
    media["escalationOccurring"] = json!(false);
    media["newOffer"] = json!(false);
    media["fromMixer"] = json!(true);
    media["skipIceReinvite"] = json!(false);
    media
}

/// The media node's answer to the renegotiation of the party at `seat`, with where it acknowledges it.
pub(crate) fn mixer_media_answer(origin: &str, call: &Call, seat: Seat, answer: &Value) -> Value {
    let acknowledge = controller_url(
        origin,
        (call, seat),
        ("mediaAnswers", "t", call.negotiations),
        Some("acknowledge"),
    );
    json!({
        "mediaAnswer": {
            "sender": null,
            "mediaContent": mixer_media(call, seat, answer),
            "callModalities": ["Audio"],
            "links": {"mediaAcknowledgement": acknowledge},
        },
    })
}

/// What an add of `invitee` to a group call is told when it rings no endpoint (as captured for someone with no
/// calling endpoint).
pub(crate) fn add_failure(invitee: &str) -> Value {
    let phrase = "Audio-video modality controller could not invite participant to this conversation since no callee \
        endpoints were found.";
    json!({
        "participants": {invitee: {"code": 580, "subCode": 5205, "phrase": phrase,
            "resultCategories": ["UnexpectedServerError"]}},
        "participantInfos": [{"participant": {"id": invitee}, "transactionEnd": {"code": 580}}],
    })
}

/// `evt` of a delivery at a calling endpoint's root telling a meeting's member that its call started.
pub(crate) const MEETING_STARTED: u64 = 128;

/// A meeting's start told to a member of it (`docs/protocol/calling.md`, "When a meeting starts"): `{"evt": 128, "gp":
/// <base64 JSON>}`, `kind` its `notificationType` (`MeetingStart`, `MeetingStartFromLobby`), its starter the call's
/// first party, `subject` the meeting's and `organizer` and `tenant` its `meetingInfo`.
pub(crate) fn meeting_started(kind: &str, call: &Call, subject: Option<&str>, organizer: &str, tenant: &str) -> Value {
    let starter = call.party(Seat::CALLER);
    let gp = json!({
        "notificationType": kind,
        "callId": call.id,
        "threadId": call.chat,
        "messageId": "0",
        "subject": subject,
        "groupCallInitiator": starter.mri(),
        "groupCallInitiatorDisplayName": starter.name,
        "firstUserToJoinModality": starter.mri(),
        "meetingInfo": {"tenantId": tenant, "organizerId": organizer},
    });
    json!({"evt": MEETING_STARTED, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())})
}

/// `evt` of a group call's delivery at a calling endpoint's root telling that someone came into the call.
pub(crate) const JOINED: u64 = 115;
/// … that someone left it.
pub(crate) const LEFT: u64 = 116;

/// A group call's `evt` 115 or 116 (someone came in, someone left): `{"evt", "cp": base64(gzip(JSON))}` with the
/// conversation, when each party came in and who is in it. The client reads neither; the shape is the minimum the
/// protocol notes name.
pub(crate) fn presence(origin: &str, call: &Call, evt: u64) -> Value {
    let participants: Vec<Value> = call
        .parties
        .iter()
        .filter(|party| party.status == Status::InCall)
        .map(|party| json!({"id": party.mri(), "displayName": party.name, "userJoinTime": party.joined}))
        .collect();
    let content = json!({
        "conversationController": conversation_url(origin, &call.conversation, None),
        "participants": participants,
    });
    json!({"evt": evt, "cp": crate::trouter::gzip_base64(&content.to_string())})
}

/// What a host's mute is told at its request's callback: each party it muted, by MRI, at its new mute version.
pub(crate) fn mute_outcome(call: &Call, muted: &[Seat], operation: &Value) -> Value {
    let parties: Map<String, Value> = muted
        .iter()
        .map(|seat| call.party(*seat))
        .map(|party| {
            let result = json!([{
                "result": "success", "participantId": party.participant["participantId"], "reason": "success",
                "serverMuteVersion": party.mute_version,
            }]);
            (party.mri(), result)
        })
        .collect();
    json!({"muteUnmuteResponseAsync": parties, "operationId": operation, "result": "success"})
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: &str = "https://own.example.test";

    #[test]
    fn only_a_call_agents_callback_under_this_origins_trouter_is_one() {
        let link = "https://own.example.test/trouter/v4/f/Endpoint/callAgent/a/1/call/end/";
        assert_eq!(callback(ORIGIN, link), Some(("Endpoint", "callAgent/a/1/call/end/")));
        for other in [
            "https://other.example.test/trouter/v4/f/Endpoint/callAgent/a/",
            "https://own.example.test/trouter/v4/f/Endpoint/messaging",
            "https://own.example.test/trouter/v4/f//callAgent/a/",
            "https://own.example.test/trouter/v4/f/Endpoint/callAgent/a/?x=1",
            "https://own.example.test.evil/trouter/v4/f/Endpoint/callAgent/a/",
            "cc://ma",
        ] {
            assert_eq!(callback(ORIGIN, other), None, "{other}");
        }
    }

    #[test]
    fn callbacks_are_found_by_name_wherever_a_body_hands_them_out() {
        let end = "https://own.example.test/trouter/v4/f/E/callAgent/a/1/call/end/";
        let roster = "https://own.example.test/trouter/v4/f/E/callAgent/a/2/conversation/rosterUpdate/";
        let body = json!({
            "attach": {"links": {"end": end}},
            "additionalActions": [{"input": {"conversationRequest": {"roster": {"rosterUpdate": roster}}}}],
            "other": "https://elsewhere.example/x",
        });
        let links = links_in(ORIGIN, &body);
        assert_eq!(links.len(), 2);
        assert_eq!(links["end"], end);
        assert_eq!(links["rosterUpdate"], roster);
        assert!(!foreign_callback(ORIGIN, &body));
        let foreign = json!({"callInvitation": {"links": {"end": "https://elsewhere.example/x"}}});
        assert!(foreign_callback(ORIGIN, &foreign));
        let roster = json!([{"roster": {"rosterUpdate": "https://elsewhere.example/x"}}]);
        assert!(foreign_callback(ORIGIN, &roster));
    }

    #[test]
    fn times_are_written_with_seven_fractional_digits() {
        assert_eq!(iso(1_700_000_000_123), "2023-11-14T22:13:20.1230000Z");
    }

    #[test]
    fn the_lines_described_say_what_a_party_sends() {
        let described =
            |lines: Value| json!({"UpdateMediaDescriptions": {"mediaDescriptions": {"descriptions": lines}}});
        let camera_on =
            described(json!([{"mid": "1", "direction": "sendrecv"}, {"mid": "2", "direction": "recvonly"}]));
        assert_eq!(
            showing(&camera_on),
            Some(Showing {
                camera: true,
                sharing: false
            })
        );
        let sharing = described(json!([{"mid": "1", "direction": "recvonly"},
            {"mid": "11", "direction": "sendonly", "label": SHARING_LABEL}]));
        assert_eq!(
            showing(&sharing),
            Some(Showing {
                camera: false,
                sharing: true
            })
        );
        let stopped = json!({"updateMediaDescriptions": {"mediaDescriptions": {"descriptions": [
            {"mid": "1", "direction": "recvonly"}, {"mid": "11", "direction": "recvonly"}]}}});
        assert_eq!(showing(&stopped), Some(Showing::default()));
        assert_eq!(showing(&json!({"other": {}})), None);
    }

    #[test]
    fn the_bandwidth_seed_is_read_from_its_parameter_text() {
        let seeded = json!({"multiChannelParameter": {"mids": ["*"],
            "mediaParameter": "{\"sendSideBWSeed\": {\"seedValueBitsPerSec\": 4000000}}"}});
        assert_eq!(seed(&seeded), Some(4_000_000));
        let capabilities = json!({"multiChannelParameter": {"mids": ["1"],
            "mediaParameter": "{\"maxVideoSendCapabilities\": {}}"}});
        assert_eq!(seed(&capabilities), None);
        let unreadable = json!({"multiChannelParameter": {"mediaParameter": "not json"}});
        assert_eq!(seed(&unreadable), None);
        assert_eq!(seed(&json!({})), None);
    }
}
