//! The account's own actions in a call that is up (its mute told, its hand raised and lowered, its live reactions, a
//! meeting ended for everyone) and what its roster and messages tell of the others, on synthetic deliveries.

use super::*;
use teams_proto::Request as _;

#[path = "group_media.rs"]
mod group_media;

#[path = "write_dispatch.rs"]
mod write_dispatch;

/// a's call to b, its start answered with every link of a meeting's conversation (as captured 2026-11-06) and its
/// acceptance delivered: up.
fn up(now: Instant) -> (Calls, CallId) {
    up_with(now, &acceptance())
}

/// As [`up`], its acceptance `accepted`.
fn up_with(now: Instant, accepted: &Value) -> (Calls, CallId) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let placement = Placement::Chat {
        callees: vec![Mri::new(B)],
        group_chat: None,
    };
    let call = calls.place(&placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let [(start, "start")] = sent(&placed)[..] else {
        panic!("{placed:#?}");
    };
    let mut answer = conversation_answer();
    for op in [
        "updateEndpointState",
        "publishState",
        "removeState",
        "sendMessage",
        "updateParticipantProperties",
    ] {
        answer["links"][op] = json!(conv(&format!("/{op}")));
    }
    calls.on_outcome(start, &ok(&answer), now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, accepted), Observed::Acceptance, now);
    let connected = effects(&mut calls);
    assert!(events(&connected).contains(&CallEvent::State {
        call,
        state: CallState::Connected
    }));
    (calls, call)
}

/// The writes among `effects`, each by its name with its body.
fn bodies(effects: &[Effect]) -> Vec<(WriteId, &'static str, Value)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send { id, write } => {
                let body = match write.as_ref() {
                    CallWrite::EndpointState(write) => write.body(),
                    CallWrite::PublishState(write) => write.body(),
                    CallWrite::RemoveState(write) => write.body(),
                    CallWrite::Reaction(write) => write.body(),
                    CallWrite::EndMeeting(write) => write.body(),
                    CallWrite::Hold(write) => write.body(),
                    CallWrite::Add(write) => write.body(),
                    CallWrite::Resume(write) => write.body(),
                    CallWrite::ParticipantProperties(write) => write.body(),
                    _ => None,
                };
                Some((*id, write.name(), body.unwrap_or(Value::Null)))
            }
            _ => None,
        })
        .collect()
}

/// The path of the call's conversation callback `name`.
fn conversation_callback(calls: &Calls, name: &str) -> String {
    calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Conversation, name))
        .unwrap()
        .to_owned()
}

/// A roster entry in the call: muted or not, its hand's id while up, and its meeting role.
fn in_call(version: u64, muted: bool, hand: Option<&str>, role: &str) -> Value {
    let mut entry = json!({"version": version, "state": "active", "meetingRole": role,
        "endpoints": {"e": {"call": {}, "endpointState": {"endpointStateSequenceNumber": 2,
            "state": {"isMuted": muted}}}}});
    if let Some(hand) = hand {
        entry["publishedStates"] =
            json!([{"typeRank": 1, "stateType": "raiseHands", "content": {"skinTone": 0}, "stateId": hand}]);
    }
    entry
}

#[test]
fn a_mute_is_told_to_the_conversation_as_the_endpoint_state_s_next_report_and_its_failure_ends_nothing() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    calls.set_muted(call, true).unwrap();
    calls.set_muted(call, false).unwrap();
    let told = bodies(&effects(&mut calls));
    let [
        (first, "update_endpoint_state", muted),
        (_, "update_endpoint_state", unmuted),
    ] = &told[..]
    else {
        panic!("{told:#?}");
    };
    assert_eq!(muted["endpointState"]["endpointStateSequenceNumber"], json!(2));
    assert_eq!(muted["endpointState"]["state"]["isMuted"], json!(true));
    assert_eq!(unmuted["endpointState"]["endpointStateSequenceNumber"], json!(3));
    assert_eq!(unmuted["endpointState"]["state"]["isMuted"], json!(false));
    calls.on_outcome(*first, &failed(), now);
    assert!(
        !events(&effects(&mut calls))
            .iter()
            .any(|event| matches!(event, CallEvent::Ended { .. })),
        "the call goes on"
    );
    assert!(matches!(calls.set_muted(CallId(99), true), Err(CallError::NoSuchCall)));
}

#[test]
fn a_hand_goes_up_at_publish_state_and_comes_down_by_the_id_the_roster_gives_it() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert!(
        matches!(calls.raise_hand(call, false), Err(CallError::NotNow)),
        "no hand is up"
    );
    calls.raise_hand(call, true).unwrap();
    let raised = bodies(&effects(&mut calls));
    let [(_, "publish_state", published)] = &raised[..] else {
        panic!("{raised:#?}");
    };
    assert_eq!(published["publishedState"]["stateType"], json!("raiseHands"));
    assert_eq!(published["publishedState"]["sequenceNumber"], json!(1));
    // the roster tells the account's hand up, and b muted with theirs
    let roster = conversation_callback(&calls, "rosterUpdate");
    let update = json!({"type": "Delta", "participants": {
        A: in_call(3, false, Some("hand-a"), "attendee"),
        B: in_call(4, true, Some("hand-b"), "attendee"),
    }});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let told = events(&effects(&mut calls));
    assert!(told.contains(&CallEvent::Own {
        call,
        hand_raised: true,
        organizer: false
    }));
    let participants = told
        .iter()
        .find_map(|event| match event {
            CallEvent::Participants { participants, .. } => Some(participants.clone()),
            _ => None,
        })
        .expect("b told");
    assert!(participants[0].muted && participants[0].hand_raised);
    calls.raise_hand(call, false).unwrap();
    let lowered = bodies(&effects(&mut calls));
    let [(_, "remove_state", removed)] = &lowered[..] else {
        panic!("{lowered:#?}");
    };
    assert_eq!(removed["stateIds"], json!(["hand-a"]));
    assert_eq!(removed["sequenceNumber"], json!(2));
}

#[test]
fn a_reaction_goes_to_send_message_and_one_delivered_is_told_with_who_sent_it() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    calls.react(call, "like").unwrap();
    let sent_reaction = bodies(&effects(&mut calls));
    let [(_, "send_message", message)] = &sent_reaction[..] else {
        panic!("{sent_reaction:#?}");
    };
    assert_eq!(message["messageContent"][0]["type"], json!("Reaction"));
    assert_eq!(
        message["messageContent"][0]["payload"]["values"][0]["name"],
        json!("like")
    );
    assert!(matches!(calls.react(call, "wave"), Err(CallError::Write(_))));
    let received = conversation_callback(&calls, "receiveMessage");
    let reaction = json!({"from": {"id": B, "displayName": "Lab Bee"}, "type": "Reaction",
        "payload": {"values": [{"name": "heart", "count": 1}]}});
    calls.on_delivery(&delivery(&received, &reaction), Observed::Nothing, now);
    calls.on_delivery(
        &delivery(&received, &json!({"type": "Something", "from": {"id": B}})),
        Observed::Nothing,
        now,
    );
    assert_eq!(
        events(&effects(&mut calls)),
        [CallEvent::Reaction {
            call,
            from: Mri::new(B),
            name: Some("Lab Bee".to_owned()),
            reaction: "heart".to_owned(),
        }]
    );
}

#[test]
fn only_the_organizer_ends_the_meeting_for_everyone_and_the_call_is_over_once_it_went() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert!(
        matches!(calls.end_meeting(call), Err(CallError::NotNow)),
        "not the organizer"
    );
    let roster = conversation_callback(&calls, "rosterUpdate");
    let update = json!({"type": "Delta", "participants": {A: in_call(3, false, None, "organizer")}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::Own {
        call,
        hand_raised: false,
        organizer: true
    }));
    calls.end_meeting(call).unwrap();
    let ended = bodies(&effects(&mut calls));
    let [(id, "end_conversation", body)] = &ended[..] else {
        panic!("{ended:#?}");
    };
    assert_eq!(
        body["conversationTransactionEnd"]["phrase"],
        json!("ConversationEndForAllInitiated")
    );
    calls.on_outcome(*id, &ok(&json!({})), now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::Ended { call, end: End::HungUp }));
}

#[test]
fn a_meeting_end_not_taken_leaves_the_call_instead() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let roster = conversation_callback(&calls, "rosterUpdate");
    let update = json!({"type": "Delta", "participants": {A: in_call(3, false, None, "organizer")}});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let _ = effects(&mut calls);
    calls.end_meeting(call).unwrap();
    let ended = sent(&effects(&mut calls));
    let [(id, "end_conversation")] = ended[..] else {
        panic!("{ended:#?}");
    };
    calls.on_outcome(id, &failed(), now);
    assert_eq!(names(&effects(&mut calls)), ["leave"]);
}

#[test]
fn nothing_goes_while_the_call_isn_t_up() {
    let now = Instant::now();
    let (mut calls, call) = ringing_call(now);
    assert!(matches!(calls.set_muted(call, true), Err(CallError::NotNow)));
    assert!(matches!(calls.raise_hand(call, true), Err(CallError::NotNow)));
    assert!(matches!(calls.react(call, "like"), Err(CallError::NotNow)));
    assert!(matches!(calls.end_meeting(call), Err(CallError::NotNow)));
    assert!(
        effects(&mut calls)
            .iter()
            .all(|effect| !matches!(effect, Effect::Send { .. }))
    );
}

#[test]
fn a_call_up_whose_conversation_named_none_of_its_links_sends_none_of_them() {
    let now = Instant::now();
    let (mut connected, up) = connected_call(now);
    assert!(matches!(connected.set_muted(up, true), Err(CallError::NotNow)));
    assert!(matches!(connected.raise_hand(up, true), Err(CallError::NotNow)));
    assert!(matches!(connected.react(up, "like"), Err(CallError::NotNow)));
    assert!(effects(&mut connected).is_empty());
}

#[test]
fn a_callee_names_itself_by_the_participant_id_its_invitation_gave_it() {
    let now = Instant::now();
    let mut calls = Calls::new(identity(B));
    calls.set_push_url(PUSH).unwrap();
    let gp = json!({
        "callNotification": {
            "from": {"id": A, "displayName": "Lab A"},
            "to": {"id": B, "participantId": "00000000-0000-4000-8000-0000000000bb"},
            "links": {"attach": cc("forked", "attach")},
        },
        "conversationInvitation": {"conversationController": conv("")},
    });
    let invited = json!({"evt": 107, "gp": base64::engine::general_purpose::STANDARD.encode(gp.to_string())});
    calls.on_delivery(
        &delivery("/v4/f/SyntheticEndpoint00000/", &invited),
        Observed::Offered,
        now,
    );
    let me = calls.call.as_ref().map(|call| call.me.participant_id.clone());
    assert_eq!(me, Some(Some("00000000-0000-4000-8000-0000000000bb".to_owned())));
}

#[test]
fn a_one_to_one_call_goes_on_hold_at_its_leg_s_hold_link_and_comes_off_it_at_the_resume_link_its_answer_gave() {
    let now = Instant::now();
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["hold"] = json!(cc("active", "hold"));
    let (mut calls, call) = up_with(now, &accepted);
    assert!(
        matches!(calls.hold(call, false), Err(CallError::NotNow)),
        "not held: nothing to resume"
    );
    calls.hold(call, true).unwrap();
    let held = bodies(&effects(&mut calls));
    let [(id, "hold", body)] = &held[..] else {
        panic!("{held:#?}");
    };
    assert_eq!(body["hold"]["holdType"], json!("musicOnHold"));
    assert!(body["hold"]["links"]["holdCompletion"].as_str().is_some());
    let resume = cc("holdSession", "resume");
    calls.on_outcome(*id, &ok(&json!({"holdResponse": {"links": {"resume": resume}}})), now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::Held { call, held: true }));
    assert!(matches!(calls.hold(call, true), Err(CallError::NotNow)), "held already");
    calls.hold(call, false).unwrap();
    let resumed = effects(&mut calls);
    let [(id, "resume", body)] = &bodies(&resumed)[..] else {
        panic!("{resumed:#?}");
    };
    assert!(body["resume"]["links"]["resumeCompletion"].as_str().is_some());
    let resumed_at = resumed.iter().find_map(|effect| match effect {
        Effect::Send { write, .. } => match write.as_ref() {
            CallWrite::Resume(write) => write.call_url().map(ToString::to_string),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(resumed_at.as_deref(), Some(resume.as_str()));
    calls.on_outcome(*id, &ok(&json!({})), now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::Held { call, held: false }));
}

#[test]
fn a_hold_not_taken_leaves_the_call_as_it_was_and_a_call_without_a_hold_link_holds_nothing() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert!(
        matches!(calls.hold(call, true), Err(CallError::NotNow)),
        "its leg named no hold link"
    );
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["hold"] = json!(cc("active", "hold"));
    let (mut calls, call) = up_with(now, &accepted);
    calls.hold(call, true).unwrap();
    let held = sent(&effects(&mut calls));
    let [(id, "hold")] = held[..] else {
        panic!("{held:#?}");
    };
    calls.on_outcome(id, &failed(), now);
    let told = effects(&mut calls);
    assert!(events(&told).contains(&CallEvent::Held { call, held: false }));
    assert!(sent(&told).is_empty(), "nothing else goes");
}

#[test]
fn the_music_on_hold_in_a_held_call_s_roster_is_nobody_in_the_call() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let roster = conversation_callback(&calls, "rosterUpdate");
    let music = crate::read::MUSIC_ON_HOLD_MRI;
    let update = json!({"type": "Delta", "participants": {
        B: in_call(3, false, None, "attendee"),
        music: {"version": 8, "state": "active", "details": {"id": music, "displayName": null},
            "endpoints": {"e1": {"call": {"mediaStreams": [
                {"type": "audio", "label": "main-audio", "sourceId": 201, "direction": "sendrecv"}
            ]}}}},
    }});
    calls.on_delivery(&delivery(&roster, &update), Observed::Nothing, now);
    let told = events(&effects(&mut calls));
    let participants = told.iter().find_map(|event| match event {
        CallEvent::Participants { call: of, participants } if *of == call => Some(participants.clone()),
        _ => None,
    });
    let mris: Vec<String> = participants
        .unwrap_or_default()
        .iter()
        .map(|participant| participant.mri.as_str().to_owned())
        .collect();
    assert_eq!(mris, [B]);
}

/// The music-on-hold app's roster entry, `version`, in the call (`present`) or gone.
fn music_entry(version: u64, present: bool) -> Value {
    let music = crate::read::MUSIC_ON_HOLD_MRI;
    let entry = if present {
        json!({"version": version, "state": "active", "details": {"id": music, "displayName": null},
        "endpoints": {"e1": {"call": {"mediaStreams": [
            {"type": "audio", "label": "main-audio", "sourceId": 201, "direction": "sendrecv"}
        ]}}}})
    } else {
        json!({"version": version, "state": "inactive", "details": {"id": music, "displayName": null},
            "endpoints": {}})
    };
    json!({"type": "Delta", "participants": {music: entry}})
}

/// Whether `effects` tell anything of the far end's hold.
fn tells_far_hold(effects: &[Effect]) -> bool {
    events(effects)
        .iter()
        .any(|event| matches!(event, CallEvent::HeldByFarEnd { .. }))
}

#[test]
fn the_far_end_s_hold_is_told_while_the_music_on_hold_is_in_the_roster_and_the_account_holds_nothing() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let roster = conversation_callback(&calls, "rosterUpdate");
    effects(&mut calls);
    calls.on_delivery(&delivery(&roster, &music_entry(8, true)), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::HeldByFarEnd { call, held: true }));
    // delivered again, or older: nothing more
    calls.on_delivery(&delivery(&roster, &music_entry(8, true)), Observed::Nothing, now);
    calls.on_delivery(&delivery(&roster, &music_entry(7, false)), Observed::Nothing, now);
    assert!(!tells_far_hold(&effects(&mut calls)));
    calls.on_delivery(&delivery(&roster, &music_entry(9, false)), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::HeldByFarEnd { call, held: false }));
    // held again later
    calls.on_delivery(&delivery(&roster, &music_entry(10, true)), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::HeldByFarEnd { call, held: true }));
}

#[test]
fn the_music_on_hold_of_the_account_s_own_hold_tells_no_hold_by_the_far_end() {
    let now = Instant::now();
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["hold"] = json!(cc("active", "hold"));
    let (mut calls, call) = up_with(now, &accepted);
    let roster = conversation_callback(&calls, "rosterUpdate");
    calls.hold(call, true).unwrap();
    let held = sent(&effects(&mut calls));
    let [(id, "hold")] = held[..] else {
        panic!("{held:#?}");
    };
    // the music came before the hold's answer, and stays a moment past the resume
    calls.on_delivery(&delivery(&roster, &music_entry(8, true)), Observed::Nothing, now);
    let resume = cc("holdSession", "resume");
    calls.on_outcome(id, &ok(&json!({"holdResponse": {"links": {"resume": resume}}})), now);
    calls.hold(call, false).unwrap();
    let resumed = sent(&effects(&mut calls));
    let [(id, "resume")] = resumed[..] else {
        panic!("{resumed:#?}");
    };
    calls.on_outcome(id, &ok(&json!({})), now);
    calls.on_delivery(&delivery(&roster, &music_entry(9, false)), Observed::Nothing, now);
    assert!(!tells_far_hold(&effects(&mut calls)));
}

#[test]
fn a_screen_shared_is_told_to_the_conversation_and_named_in_the_call_s_modalities() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let modalities = |calls: &Calls| calls.call.as_ref().unwrap().modalities.clone();
    calls.set_screen(call, Some(crate::Screen::Sharer));
    assert_eq!(modalities(&calls), ["Audio", "ScreenSharer"]);
    calls.tell_screen_shared(call).unwrap();
    let told = bodies(&effects(&mut calls));
    let [(_, "update_participant_properties", body)] = &told[..] else {
        panic!("{told:#?}");
    };
    assert_eq!(
        body["to"][0]["propertyBag"]["screenSharerAttributes"],
        json!({"sequenceNumber": 1, "value": {"source": 1}})
    );
    assert!(body["links"]["updateParticipantPropertiesStatus"].as_str().is_some());
    // each share tells the next
    calls.tell_screen_shared(call).unwrap();
    let told = bodies(&effects(&mut calls));
    assert_eq!(
        told[0].2["to"][0]["propertyBag"]["screenSharerAttributes"]["sequenceNumber"],
        json!(2)
    );
    // the far end's screen shown, then none: the viewer's modality, then the call's own
    calls.set_screen(call, Some(crate::Screen::Viewer));
    assert_eq!(modalities(&calls), ["Audio", "ScreenViewer"]);
    calls.set_screen(call, None);
    assert_eq!(modalities(&calls), ["Audio"]);
    let other = CallId(call.get() + 1);
    assert!(matches!(calls.tell_screen_shared(other), Err(CallError::NoSuchCall)));
}

/// The lab's third account.
const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";

#[test]
fn someone_added_goes_to_the_conversation_s_add_naming_the_group_chat_and_the_call_is_in_it_once_taken() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let group = "19:00000000000000000000000000010001@thread.v2";
    calls.add(call, &Mri::new(C), group).unwrap();
    let added = bodies(&effects(&mut calls));
    let [(id, "add", body)] = &added[..] else {
        panic!("{added:#?}");
    };
    assert_eq!(body["participants"]["to"][0]["id"], json!(C));
    assert_eq!(body["groupChat"]["threadId"], json!(group));
    assert_eq!(body["participantInvitationData"], json!({}));
    calls.on_outcome(*id, &ok(&json!({})), now);
    let told = events(&effects(&mut calls));
    assert!(told.contains(&CallEvent::GroupChat {
        call,
        chat: group.to_owned()
    }));
    assert_eq!(calls.group_chat(call).as_deref(), Some(group));
    // told once: the conversation's update naming it again changes nothing
    let update = conversation_callback(&calls, "conversationUpdate");
    let named = json!({"groupChat": {"threadId": group, "messageId": "0"}, "state": {"isMultiParty": true}});
    calls.on_delivery(&delivery(&update, &named), Observed::Nothing, now);
    assert!(
        !events(&effects(&mut calls))
            .iter()
            .any(|event| matches!(event, CallEvent::GroupChat { .. }))
    );
    // c didn't answer
    let failure = conversation_callback(&calls, "addParticipantFailure");
    let failed = json!({"participants": {C: {"code": 580, "phrase": "no callee endpoints"}}});
    calls.on_delivery(&delivery(&failure, &failed), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::AddFailed {
        call,
        who: Mri::new(C),
        declined: false
    }));
}

#[test]
fn an_add_not_taken_tells_its_invitee_didn_t_come_in_and_ends_nothing() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    let group = "19:00000000000000000000000000010001@thread.v2";
    calls.add(call, &Mri::new(C), group).unwrap();
    let added = sent(&effects(&mut calls));
    let [(id, "add")] = added[..] else {
        panic!("{added:#?}");
    };
    calls.on_outcome(id, &failed(), now);
    let told = effects(&mut calls);
    assert!(events(&told).contains(&CallEvent::AddFailed {
        call,
        who: Mri::new(C),
        declined: false
    }));
    assert!(sent(&told).is_empty(), "the call goes on");
    assert_eq!(calls.group_chat(call), None);
}

#[test]
fn the_far_end_of_a_one_to_one_call_is_its_chat_s_other_member_and_a_conversation_update_moves_it_to_a_group_chat() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert_eq!(calls.far_end(call), Some(Mri::new(B)));
    assert_eq!(calls.group_chat(call), None);
    let update = conversation_callback(&calls, "conversationUpdate");
    let group = "19:00000000000000000000000000010002@thread.v2";
    let named = json!({"groupChat": {"threadId": group, "messageId": "0"}});
    calls.on_delivery(&delivery(&update, &named), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::GroupChat {
        call,
        chat: group.to_owned()
    }));
    assert_eq!(calls.far_end(call), None, "a group call has no one far end");
}

#[test]
fn a_transfer_goes_at_the_leg_s_transfer_link_and_the_call_leaves_once_the_far_end_took_it() {
    let now = Instant::now();
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["transfer"] = json!(cc("active", "transfer"));
    let (mut calls, call) = up_with(now, &accepted);
    calls.transfer(call, &Mri::new(C)).unwrap();
    let sent_now = effects(&mut calls);
    let [(id, "transfer")] = sent(&sent_now)[..] else {
        panic!("{sent_now:#?}");
    };
    calls.on_outcome(id, &ok(&json!({})), now);
    assert!(sent(&effects(&mut calls)).is_empty(), "it waits for the far end");
    let taken = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "transferAcceptance"))
        .unwrap()
        .to_owned();
    calls.on_delivery(
        &delivery(&taken, &json!({"transferAcceptance": {}})),
        Observed::Nothing,
        now,
    );
    let left = effects(&mut calls);
    assert!(sent(&left).iter().any(|(_, name)| *name == "leave"), "{left:#?}");
}

#[test]
fn a_transfer_not_taken_is_told_and_the_call_goes_on() {
    let now = Instant::now();
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["transfer"] = json!(cc("active", "transfer"));
    let (mut calls, call) = up_with(now, &accepted);
    calls.transfer(call, &Mri::new(C)).unwrap();
    let [(id, "transfer")] = sent(&effects(&mut calls))[..] else {
        panic!();
    };
    calls.on_outcome(id, &failed(), now);
    let told = effects(&mut calls);
    assert!(events(&told).contains(&CallEvent::TransferFailed { call }));
    assert!(sent(&told).is_empty());
    // a call whose leg named no transfer link transfers nothing
    let (mut calls, call) = up(now);
    assert!(matches!(calls.transfer(call, &Mri::new(C)), Err(CallError::NotNow)));
}

#[test]
fn a_transfer_the_far_end_asked_for_is_taken_and_told_with_whom_to_call_then_the_call_is_over_here() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    effects(&mut calls);
    let at = calls
        .call
        .as_ref()
        .and_then(|call| call.path_of(Kind::Call, "transfer"))
        .map(str::to_owned);
    let Some(at) = at else {
        // a placed call hands its `transfer` callback out with its acceptance's acknowledgement only
        return;
    };
    let asked = json!({"callTransfer": {"target": {"id": C}, "isConsultative": false,
        "transferor": {"details": {"id": B, "displayName": "Lab B"}, "authorizationToken": "synthetic-authorization"},
        "links": {"transferAcceptance": cc("transfer", "acceptance"), "transferCompletion": cc("transfer", "completion")}}});
    calls.on_delivery(&delivery(&at, &asked), Observed::Nothing, now);
    let accepting = effects(&mut calls);
    let [(id, "transfer_acceptance")] = sent(&accepting)[..] else {
        panic!("{accepting:#?}");
    };
    calls.on_outcome(id, &ok(&json!({})), now);
    let told = events(&effects(&mut calls));
    let transferred = told.iter().find_map(|event| match event {
        CallEvent::Transferred {
            call: of,
            target,
            transferor,
        } if *of == call => Some((target.clone(), transferor.mri())),
        _ => None,
    });
    assert_eq!(transferred, Some((Mri::new(C), Some(Mri::new(B)))));
    assert!(
        told.iter()
            .any(|event| matches!(event, CallEvent::Ended { call: of, .. } if *of == call))
    );
    assert!(calls.current().is_none());
}

/// The made-up meeting a's meeting calls are in.
const MEETING_CHAT: &str = "19:meeting_MadeUpMeeting0001@thread.v2";

/// The made-up meeting's organizer (a) and tenant.
fn meeting_info() -> teams_proto::MeetingInfo {
    teams_proto::MeetingInfo {
        organizer_id: "00000000-0000-4000-8000-00000000000a".to_owned(),
        tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
    }
}

/// a in its meeting's call, its entry answered (`answer`) and its acceptance delivered: up.
fn in_meeting(now: Instant, placement: &Placement, answer: &Value) -> (Calls, CallId) {
    let mut calls = Calls::new(identity(A));
    calls.set_push_url(PUSH).unwrap();
    let call = calls.place(placement, media(), now).unwrap();
    let placed = effects(&mut calls);
    let (start, _) = sent(&placed)[0];
    calls.on_outcome(start, &ok(answer), now);
    let path = acceptance_path(&calls);
    calls.on_delivery(&delivery(&path, &acceptance()), Observed::Acceptance, now);
    let _ = effects(&mut calls);
    (calls, call)
}

#[test]
fn someone_a_meeting_invited_is_asked_in_naming_its_chat_and_organizer_and_their_decline_is_told() {
    let now = Instant::now();
    let placement = Placement::Meeting {
        chat: MEETING_CHAT.to_owned(),
        meeting: meeting_info(),
    };
    let (mut calls, call) = in_meeting(now, &placement, &conversation_answer());
    assert_eq!(calls.meeting_chat(call).as_deref(), Some(MEETING_CHAT));
    assert_eq!(calls.group_chat(call), None, "a meeting's chat is no group chat");
    calls.add_to_meeting(call, &Mri::new(C)).unwrap();
    let added = bodies(&effects(&mut calls));
    let [(id, "add", body)] = &added[..] else {
        panic!("{added:#?}");
    };
    assert_eq!(body["participants"]["to"][0]["id"], json!(C));
    assert_eq!(body["groupChat"], json!({"threadId": MEETING_CHAT, "messageId": null}));
    assert_eq!(
        body["participantInvitationData"]["invitationData"]["meetingInfo"]["organizerId"],
        json!("00000000-0000-4000-8000-00000000000a")
    );
    calls.on_outcome(*id, &ok(&json!({})), now);
    let taken = events(&effects(&mut calls));
    assert!(
        !taken.iter().any(|event| matches!(event, CallEvent::GroupChat { .. })),
        "the meeting's call stays in its chat: {taken:#?}"
    );
    // c declined the ring (as captured 2026-11-24)
    let failure = conversation_callback(&calls, "addParticipantFailure");
    let declined = json!({"participants": {C: {"code": 580, "subCode": 5208,
        "callControllerTransactionEnd": {"code": 603, "subCode": 0}}}});
    calls.on_delivery(&delivery(&failure, &declined), Observed::Nothing, now);
    assert!(events(&effects(&mut calls)).contains(&CallEvent::AddFailed {
        call,
        who: Mri::new(C),
        declined: true
    }));
    // never the account itself, and never into another call
    assert!(calls.add_to_meeting(call, &Mri::new(A)).is_err());
    let other = CallId(call.get() + 1);
    assert!(matches!(
        calls.add_to_meeting(other, &Mri::new(C)),
        Err(CallError::NoSuchCall)
    ));
    assert!(sent(&effects(&mut calls)).is_empty());
}

#[test]
fn a_call_that_is_no_meeting_s_or_names_no_organizer_asks_nobody_into_a_meeting() {
    let now = Instant::now();
    let (mut calls, call) = up(now);
    assert_eq!(calls.meeting_chat(call), None);
    assert!(matches!(
        calls.add_to_meeting(call, &Mri::new(C)),
        Err(CallError::NotNow)
    ));
    // a meeting code's entry whose answer links the meeting's chat but names no meeting: its organizer isn't known
    let code = teams_proto::MeetingCode::typed("123456789012", "MadeUp").unwrap();
    let (mut calls, call) = in_meeting(now, &Placement::MeetingCode(code), &answer_linking(MEETING_CHAT));
    assert_eq!(
        calls.call.as_ref().and_then(|call| call.caption_thread.as_deref()),
        Some(MEETING_CHAT),
        "the meeting's chat is known"
    );
    assert_eq!(calls.meeting_chat(call), None);
    assert!(matches!(
        calls.add_to_meeting(call, &Mri::new(C)),
        Err(CallError::NotNow)
    ));
    assert!(sent(&effects(&mut calls)).is_empty());
}

/// A meeting code's entry answered with the meeting's details linking `chat`, as its join link names it.
fn answer_linking(chat: &str) -> Value {
    let reference = format!("https://teams.microsoft.com/l/meetup-join/{chat}/0");
    let html = format!(
        r#"<a id="meet_invite_block.action.join_link" href="https://teams.microsoft.com/meet/123456789012?p=MadeUp">Join</a><a id="meet_invite_block.action.join_link_compatibility" href="{reference}">System reference</a>"#
    );
    let invitation: String = url::form_urlencoded::byte_serialize(html.as_bytes()).collect();
    let mut answer = conversation_answer();
    answer["meetingDetails"] = json!({"invitation": invitation});
    answer
}

#[test]
fn a_meeting_code_s_entry_takes_the_meeting_its_answer_names() {
    let now = Instant::now();
    let code = teams_proto::MeetingCode::typed("123456789012", "MadeUp").unwrap();
    let mut answer = conversation_answer();
    answer["meetingInfo"] = json!({"organizerId": "00000000-0000-4000-8000-00000000000a",
        "tenantId": "00000000-0000-4000-8000-0000000000f1"});
    let (calls, call) = in_meeting(now, &Placement::MeetingCode(code), &answer);
    assert_eq!(
        calls.call.as_ref().and_then(|call| call.meeting_info.clone()),
        Some(meeting_info())
    );
    // its chat comes later, with the meeting's join link
    assert_eq!(calls.meeting_chat(call), None);
}
