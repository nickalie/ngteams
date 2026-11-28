//! Diagnostic classifications remain independent of the private ids, paths and payloads held by an error.

#![cfg(test)]

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use teams_core::{
    CallActionError, CallMedia, CallTried, CatchUpError, EngineError, OutboxError, Relay, TeamsReadError,
};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::session::SessionConfig;
use teams_proto::Response;
use teams_store::StoreError;

const PRIVATE: &str = "private diagnostic sentinel";

#[test]
fn startup_and_wrapped_errors_are_named_without_their_private_details() {
    for (error, kind) in [
        (
            EngineError::Store(StoreError::UnknownConversation(PRIVATE.to_owned())),
            "store",
        ),
        (
            EngineError::Outbox(OutboxError::OtherRun { id: 7, run_id: 8 }),
            "outbox",
        ),
        (EngineError::CatchUp(CatchUpError::CheckFellShort), "catch-up"),
        (EngineError::Teams(TeamsReadError::Lost), "teams"),
        (EngineError::Runtime(io::Error::other(PRIVATE)), "runtime"),
        (EngineError::Thread(io::Error::other(PRIVATE)), "thread"),
        (
            EngineError::AlreadyRunning {
                lock: PathBuf::from(PRIVATE),
            },
            "already running",
        ),
        (EngineError::Call(CallActionError::NoCalling), "call"),
    ] {
        assert_eq!(error.kind(), kind);
        assert!(!error.kind().contains(PRIVATE));
    }
}

#[test]
fn request_errors_are_named_without_message_ids_or_failure_text() {
    for (error, kind) in [
        (
            EngineError::UnknownMessage {
                conversation: PRIVATE.to_owned(),
                message: PRIVATE.to_owned(),
            },
            "unknown message",
        ),
        (
            EngineError::NotOwn {
                message: PRIVATE.to_owned(),
                action: "edited",
            },
            "not the account's own",
        ),
        (
            EngineError::Deleted {
                message: PRIVATE.to_owned(),
            },
            "deleted",
        ),
        (
            EngineError::Unknown {
                message: PRIVATE.to_owned(),
                missing: "compose time",
                action: "edited",
            },
            "unknown field",
        ),
        (EngineError::NotAChat, "not a chat"),
        (
            EngineError::NotAGroupChat {
                conversation: PRIVATE.to_owned(),
            },
            "not a group chat",
        ),
        (
            EngineError::NotAddable {
                conversation: PRIVATE.to_owned(),
            },
            "not addable",
        ),
        (EngineError::NoEndpoint, "no endpoint"),
        (EngineError::Stopped, "stopped"),
        (EngineError::AnimationUnread, "animation unread"),
        (EngineError::PictureUnread, "picture unread"),
        (EngineError::FileUnavailable, "file unavailable"),
        (EngineError::FileUnread, "file unread"),
        (EngineError::CalendarUnread, "calendar unread"),
        (EngineError::NoSuchEvent, "no such event"),
        (
            EngineError::EventNotSaved {
                kind: "creation",
                why: PRIVATE.to_owned(),
            },
            "event not saved",
        ),
        (EngineError::SearchFailed, "search failed"),
    ] {
        assert_eq!(error.kind(), kind);
        assert!(!error.kind().contains(PRIVATE));
    }
}

#[test]
fn a_call_write_summary_distinguishes_the_outcome_and_never_contains_an_answer_body() {
    for (tried, summary) in [
        (
            CallTried::Answered(Response::new(403, PRIVATE.as_bytes())),
            "admitted, HTTP 403",
        ),
        (
            CallTried::Denied("another account is in a call".to_owned()),
            "call admission refused: another account is in a call",
        ),
        (
            CallTried::NotSent("credential unavailable".to_owned()),
            "not sent: credential unavailable",
        ),
        (CallTried::Throttled(Duration::from_millis(7)), "throttled for 7 ms"),
        (
            CallTried::Uncertain("connection closed".to_owned()),
            "outcome unknown: connection closed",
        ),
    ] {
        assert_eq!(tried.summary(), summary);
        assert!(!tried.summary().contains(PRIVATE));
    }
}

#[test]
fn media_debug_information_omits_the_devices_private_file_names() {
    let devices = FileDevices {
        input: AudioInput::Wav(PathBuf::from(PRIVATE)),
        recording: Some(PathBuf::from(PRIVATE)),
        camera: PatternCamera::default(),
        pacing: Pacing::Unpaced,
    };
    let media = CallMedia {
        devices: Arc::new(devices),
        relay: Relay::Teams,
        session: SessionConfig::default(),
        video: None,
    };
    let debug = format!("{media:?}");
    assert!(debug.contains("Teams"));
    assert!(!debug.contains(PRIVATE));
}
