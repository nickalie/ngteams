//! Private diagnostic formatting contracts; synthetic ids and payloads only.
#![cfg(test)]

use std::time::Instant;

use log::{Level, LevelFilter};
use teams_call::{CallId, Calls, Identity, Placement, RemoteEnd};
use teams_proto::Mri;
use teams_proto::{ClientInfo, MediaContent};

use super::super::{EngineError, Event};
use super::{call, logged};
use crate::{CallAction, CallParticipant, CallStatus, CallView, End, IncomingCall};

const PRIVATE_NAME: &str = "PRIVATE NAME MUST NOT BE LOGGED";

/// Allocate a real machine id without sending any of the effects it queued.
fn call_id() -> CallId {
    let mut machine = Calls::new(Identity {
        mri: Mri::new(teams_mock::LAB_A),
        display_name: Some(PRIVATE_NAME.to_owned()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".to_owned(),
        language_id: None,
        tenant_id: teams_mock::TENANT.to_owned(),
        client: ClientInfo::default(),
        region: Some("amer".to_owned()),
        partition: None,
    });
    machine
        .set_push_url("https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000/")
        .unwrap();
    machine
        .place(
            &Placement::TestCall {
                echo: Mri::new(teams_mock::ECHO),
            },
            MediaContent::new("v=0\r\n", "application/sdp-ngc-1.0"),
            Instant::now(),
        )
        .unwrap()
}

fn remote_end() -> RemoteEnd {
    RemoteEnd {
        code: Some(200),
        sub_code: Some(1),
        phrase: Some("CallEndReasonLocalUserInitiated".to_owned()),
        conversation: None,
        by: Some(Mri::new(teams_mock::LAB_B)),
        by_name: Some(PRIVATE_NAME.to_owned()),
        categories: vec!["Success".to_owned()],
    }
}

fn failed() -> Event {
    Event::Failed {
        during: "indexing stored messages for search",
        error: EngineError::Deleted {
            message: "1700000000000".to_owned(),
        },
    }
}

#[test]
fn a_failure_is_logged_at_warn_and_an_info_event_isn_t() {
    let (level, line) = logged(&failed(), LevelFilter::Warn).expect("a warning at warn");
    assert_eq!(level, Level::Warn);
    assert!(line.starts_with("failed indexing stored messages for search"), "{line}");
    assert!(logged(&Event::Teams, LevelFilter::Warn).is_none());
    assert_eq!(
        logged(&Event::Teams, LevelFilter::Info).map(|(level, _)| level),
        Some(Level::Info)
    );
}

#[test]
fn a_presence_change_is_logged_at_info_by_mri_and_availability() {
    let event = Event::Presence {
        mri: "8:orgid:00000000-0000-0000-0000-00000000000b".to_owned(),
        availability: crate::presence::Availability::Away,
    };
    let (level, line) = logged(&event, LevelFilter::Info).expect("a line at info");
    assert_eq!(level, Level::Info);
    assert_eq!(line, "presence of 8:orgid:00000000-0000-0000-0000-00000000000b: Away");
    assert!(logged(&event, LevelFilter::Warn).is_none());
}

#[test]
fn nothing_is_logged_below_the_event_s_level() {
    assert!(logged(&failed(), LevelFilter::Error).is_none());
    assert!(logged(&failed(), LevelFilter::Off).is_none());
}

#[test]
fn every_call_action_is_identified_without_a_name_or_media_payload() {
    let id = call_id();
    let number = id.get();
    let actions = [
        (
            CallAction::Call {
                chat: "synthetic-chat".to_owned(),
            },
            "a call from synthetic-chat".to_owned(),
        ),
        (
            CallAction::TestCall {
                echo: Mri::new(teams_mock::ECHO),
            },
            format!("the test call of {}", teams_mock::ECHO),
        ),
        (
            CallAction::Join {
                chat: "synthetic-group".to_owned(),
            },
            "a join of the call in synthetic-group".to_owned(),
        ),
        (CallAction::Answer { call: id }, format!("the answer of call {number}")),
        (
            CallAction::Decline { call: id },
            format!("the decline of call {number}"),
        ),
        (CallAction::HangUp { call: id }, format!("a hang-up of call {number}")),
        (
            CallAction::Mute { call: id, muted: true },
            format!("call {number} muted true"),
        ),
        (
            CallAction::Renegotiate { call: id },
            format!("a renegotiation of call {number}"),
        ),
        (
            CallAction::Camera { call: id, on: true },
            format!("call {number} camera on"),
        ),
        (
            CallAction::Camera { call: id, on: false },
            format!("call {number} camera off"),
        ),
    ];
    for (action, expected) in actions {
        assert_eq!(call(&action), expected);
    }
}

#[test]
fn incoming_and_remote_end_diagnostics_keep_ids_and_codes_but_omit_display_names() {
    let id = call_id();
    for group_chat in [None, Some("synthetic-group".to_owned())] {
        let event = Event::IncomingCall(IncomingCall {
            call: id,
            caller: Mri::new(teams_mock::LAB_B),
            caller_name: Some(PRIVATE_NAME.to_owned()),
            group_chat: group_chat.clone(),
            meeting: None,
        });
        let (_, line) = logged(&event, LevelFilter::Info).unwrap();
        assert!(line.contains(teams_mock::LAB_B));
        assert!(!line.contains(PRIVATE_NAME));
        assert_eq!(line.contains(" in synthetic-group"), group_chat.is_some());
    }
    for event in [
        Event::RingEnded {
            call: id,
            end: remote_end(),
        },
        Event::Call {
            call: id,
            status: CallStatus::Ended(End::Remote(remote_end())),
        },
    ] {
        let (_, line) = logged(&event, LevelFilter::Info).unwrap();
        assert!(line.contains("200"));
        assert!(line.contains("CallEndReasonLocalUserInitiated"));
        assert!(!line.contains(PRIVATE_NAME));
    }
    let mut anonymous = remote_end();
    anonymous.by = None;
    let event = Event::Call {
        call: id,
        status: CallStatus::Ended(End::Remote(anonymous)),
    };
    assert!(logged(&event, LevelFilter::Info).unwrap().1.contains("by nobody"));
}

#[test]
fn device_and_gallery_diagnostics_report_counts_and_lines_without_participant_names() {
    let id = call_id();
    let number = id.get();
    for (event, expected) in [
        (
            Event::Voicemail { call: id },
            format!("call {number}: taken by the callee's voicemail"),
        ),
        (
            Event::CameraOff { call: id },
            format!("call {number}: its camera went off"),
        ),
        (
            Event::AudioClosed {
                call: id,
                microphone: true,
            },
            format!("call {number}: its microphone went away"),
        ),
        (
            Event::AudioClosed {
                call: id,
                microphone: false,
            },
            format!("call {number}: its speaker went away"),
        ),
        (
            Event::Call {
                call: id,
                status: CallStatus::Connected,
            },
            format!("call {number}: Connected"),
        ),
        (
            Event::CallingRegistration(teams_trouter::RegistrationReport::Registered),
            "calling registration: Registered".to_owned(),
        ),
    ] {
        assert_eq!(logged(&event, LevelFilter::Info).unwrap().1, expected);
    }
    for speaking in [false, true] {
        let event = Event::CallParticipants {
            call: id,
            view: CallView {
                participants: vec![CallParticipant {
                    mri: teams_mock::LAB_B.to_owned(),
                    name: Some(PRIVATE_NAME.to_owned()),
                    video_on: true,
                    line: Some(2),
                    speaking: true,
                    muted: false,
                    hand_raised: false,
                    organizer: false,
                    joined: None,
                    sharing: false,
                }],
                speaking,
                hearing: true,
            },
        };
        let (_, line) = logged(&event, LevelFilter::Info).unwrap();
        assert!(line.contains("1 participant(s), cameras on 1, lines [Some(2)], speaking 1"));
        assert!(!line.contains(PRIVATE_NAME));
        assert!(
            !line.contains(teams_mock::LAB_B),
            "the gallery reports counts rather than people"
        );
        // the account speaking is told after the others who speak, before the muted and the hands up
        assert_eq!(
            line.contains("speaking 1 and the account, muted 0, hands up 0"),
            speaking
        );
        assert!(line.contains(", muted 0, hands up 0"));
    }
}

#[test]
fn message_diagnostics_distinguish_bulk_reads_from_deliveries_and_never_need_a_payload() {
    for (source, expected_level) in [
        (super::Source::Delivery, Level::Info),
        (super::Source::Replay, Level::Info),
        (super::Source::CatchUp, Level::Debug),
        (super::Source::History, Level::Debug),
    ] {
        let event = Event::Message {
            conversation: "synthetic-chat".to_owned(),
            id: "123".to_owned(),
            change: super::super::MessageChange::New,
            applied: teams_store::Applied::Inserted,
            message: None,
            source,
        };
        let (level, line) = logged(&event, LevelFilter::Debug).unwrap();
        assert_eq!(level, expected_level);
        assert!(line.starts_with("message 123 in synthetic-chat:"));
        assert_eq!(
            logged(&event, LevelFilter::Info).is_some(),
            expected_level == Level::Info
        );
    }
}

#[test]
fn meeting_and_lobby_actions_and_events_name_ids_and_counts_only() {
    let id = call_id();
    let number = id.get();
    let actions = [
        (
            CallAction::JoinMeeting {
                chat: "synthetic-meeting".to_owned(),
                meeting: teams_proto::MeetingInfo {
                    organizer_id: "00000000-0000-4000-8000-00000000000a".to_owned(),
                    tenant_id: teams_mock::TENANT.to_owned(),
                },
            },
            "a join of the meeting in synthetic-meeting".to_owned(),
        ),
        (
            CallAction::MeetNow {
                channel: "synthetic-channel".to_owned(),
            },
            "a new meeting in synthetic-channel".to_owned(),
        ),
        (
            CallAction::JoinChannel {
                channel: "synthetic-channel".to_owned(),
                post: None,
            },
            "a join of the meeting running in synthetic-channel".to_owned(),
        ),
        (
            CallAction::JoinChannel {
                channel: "synthetic-channel".to_owned(),
                post: Some("1700000000001".to_owned()),
            },
            "a join of the meeting of post 1700000000001 in synthetic-channel".to_owned(),
        ),
        (
            CallAction::Lobby {
                call: id,
                waiting: Mri::new(teams_mock::LAB_B),
                admit: true,
            },
            format!("call {number}: admit {} from the lobby", teams_mock::LAB_B),
        ),
        (
            CallAction::Lobby {
                call: id,
                waiting: Mri::new(teams_mock::LAB_B),
                admit: false,
            },
            format!("call {number}: deny {} from the lobby", teams_mock::LAB_B),
        ),
    ];
    for (action, expected) in actions {
        assert_eq!(call(&action), expected);
    }
    let lobby = Event::Lobby {
        call: id,
        waiting: vec![crate::LobbyPerson {
            mri: Mri::new(teams_mock::LAB_B),
            name: Some(PRIVATE_NAME.to_owned()),
        }],
    };
    assert_eq!(
        logged(&lobby, LevelFilter::Info).unwrap().1,
        format!("call {number}: 1 waiting in the lobby")
    );
}

#[test]
fn the_plain_events_name_ids_and_counts() {
    for (event, expected) in [
        (
            Event::Connected {
                endpoint: "synthetic-endpoint".to_owned(),
            },
            "push channel up (endpoint synthetic-endpoint)".to_owned(),
        ),
        (
            Event::ReadHorizon {
                conversation: "synthetic-chat".to_owned(),
            },
            "read horizon moved in synthetic-chat".to_owned(),
        ),
        (
            Event::ConversationChanged {
                conversation: "synthetic-chat".to_owned(),
            },
            "conversation synthetic-chat changed".to_owned(),
        ),
        (
            Event::Profile {
                mri: teams_mock::LAB_B.to_owned(),
            },
            format!("profile of {} changed", teams_mock::LAB_B),
        ),
        (
            Event::Indexed { messages: 3 },
            "search backfill over: 3 message(s) indexed".to_owned(),
        ),
        (Event::Teams, "the teams changed".to_owned()),
    ] {
        assert_eq!(logged(&event, LevelFilter::Info).unwrap().1, expected);
    }
    let typing = Event::Typing {
        conversation: "synthetic-chat".to_owned(),
        from: Some(teams_mock::LAB_B.to_owned()),
        typing: true,
    };
    assert!(logged(&typing, LevelFilter::Info).is_none(), "typing is at debug");
    assert_eq!(
        logged(&typing, LevelFilter::Debug).unwrap(),
        (Level::Debug, "typing true in synthetic-chat".to_owned())
    );
}

#[test]
fn the_push_channel_and_its_catch_ups_are_told_by_their_kind_and_counts_alone() {
    let down = Event::Disconnected {
        down: teams_trouter::Down::Closed,
        heard: None,
    };
    assert_eq!(
        logged(&down, LevelFilter::Info),
        Some((Level::Info, "push channel down: Closed".to_owned()))
    );
    let caught_up = Event::CaughtUp {
        cause: crate::Cause::Retry,
        report: crate::CatchUpReport {
            conversations: 3,
            ..crate::CatchUpReport::default()
        },
    };
    assert_eq!(
        logged(&caught_up, LevelFilter::Info),
        Some((
            Level::Info,
            "caught up (Retry): 3 conversation(s), 0 message(s) changed, 0 failed".to_owned()
        ))
    );
    let failed = Event::CatchUpFailed {
        cause: crate::Cause::Retry,
        error: crate::CatchUpError::TooManyPages,
    };
    let (level, line) = logged(&failed, LevelFilter::Warn).expect("a warning");
    assert_eq!(level, Level::Warn);
    assert!(line.starts_with("catch-up (Retry) failed: "), "{line}");
    let calling = Event::CallingRegistration(teams_trouter::RegistrationReport::Registered);
    assert_eq!(
        logged(&calling, LevelFilter::Info),
        Some((Level::Info, "calling registration: Registered".to_owned()))
    );
}

#[test]
fn a_transferred_call_is_told_without_who_transferred_it() {
    let id = call_id();
    let transferred = Event::CallTransferred {
        call: id,
        target: Mri::new(teams_mock::LAB_C),
        by: Some(PRIVATE_NAME.to_owned()),
        chat: None,
    };
    let (_, line) = logged(&transferred, LevelFilter::Info).expect("a line at info");
    assert_eq!(line, format!("call {}: transferred by its far end", id.get()));
    assert!(!line.contains(PRIVATE_NAME) && !line.contains(teams_mock::LAB_C));
}
