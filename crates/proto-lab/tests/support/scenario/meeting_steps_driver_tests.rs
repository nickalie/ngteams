//! Meeting metadata and socket lifecycle with synthetic scheduling requests.

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

use super::*;

use crate::scenario::meeting_page;

fn options() -> RunOptions {
    RunOptions {
        load_timeout: Duration::from_secs(2),
        display: crate::browser::Display::Headless,
        call: crate::scenario::ab_call::CallStep::Answer,
    }
}

#[tokio::test]
async fn scheduled_meeting_metadata_is_saved_only_when_a_capture_exists() {
    for record in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = meeting_page::page(meeting_page::creation(), 200, 200).await;
        let mut session = page.session(temp.path(), record);
        let mut drive = session.driver();
        let mut report = ScenarioReport::new(ScenarioName::MeetingSteps);
        let chat = made_meeting(
            &mut session,
            &mut drive,
            &mut report,
            "Synthetic meeting",
            "peer@example.invalid",
            options(),
        )
        .await
        .unwrap();
        assert_eq!(chat, meeting_page::CHAT);
        if let Some(dir) = session.capture_dir() {
            assert_eq!(
                std::fs::read_to_string(dir.join("meeting.txt")).unwrap(),
                format!("{}\n{}\n", meeting_page::CHAT, meeting_page::LINK)
            );
        }
        assert!(report.notes.iter().any(|note| note.contains("scheduled the meeting")));
    }
}

#[tokio::test]
async fn invalid_metadata_and_capture_io_failures_stop_the_control_phase() {
    for body in [
        json!({}),
        json!({"schedulingServiceEvent":{"links":{"join":meeting_page::LINK}}}),
        meeting_page::creation(),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut page = meeting_page::page(body.clone(), 200, 200).await;
        let mut session = page.session(temp.path(), true);
        std::fs::create_dir(session.capture_dir().unwrap().join("meeting.txt")).unwrap();
        let mut drive = session.driver();
        let mut report = ScenarioReport::new(ScenarioName::MeetingSteps);
        let error = made_meeting(
            &mut session,
            &mut drive,
            &mut report,
            "Synthetic",
            "peer@example.invalid",
            options(),
        )
        .await
        .unwrap_err()
        .to_string();
        if body == meeting_page::creation() {
            assert!(error.contains("can't write meeting.txt"));
        } else if body["schedulingServiceEvent"]["links"]["join"].is_string() {
            assert!(error.contains("no chat"));
        } else {
            assert!(error.contains("no join link"));
        }
    }
}

#[tokio::test]
async fn missing_mris_stop_before_navigation() {
    let temp = tempfile::tempdir().unwrap();
    let mut page = meeting_page::page(meeting_page::creation(), 200, 200).await;
    let mut session = page.session(temp.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::MeetingSteps);
    let who = Who {
        alias: Alias::A,
        me: "",
        them: "",
        peer_upn: "peer@example.invalid",
    };
    assert!(
        steps(
            &who,
            &mut session,
            &mut [],
            &mut report,
            &temp.path().join("steps.sock"),
            options()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("no MRI")
    );
    assert!(page.commands().is_empty());
}

#[test]
fn isolated_environment_covers_aliases_schedule_and_cleanup_choices() {
    for (mode, value, expected) in [
        ("none", "unknown", "b"),
        ("empty", "a", "a"),
        ("schedule", "b", "b"),
        ("cancel", "c", "c"),
        ("cancel-failed", "unknown", "b"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("--exact")
            .arg("scenario::meeting_steps::driver_tests::isolated_control_child")
            .arg("--nocapture")
            .env_clear()
            .env("HOME", temp.path())
            .env("PROTO_SCENARIO_CASE", mode)
            .env("PROTO_TEST_ALIAS", value)
            .env("PROTO_EXPECTED_ALIAS", expected);
        match mode {
            "empty" => {
                command.env("PROTO_LAB_SCHEDULE", "").env("PROTO_LAB_CANCEL", "");
            }
            "schedule" => {
                command.env("PROTO_LAB_SCHEDULE", "Synthetic meeting");
            }
            "cancel" | "cancel-failed" => {
                command.env("PROTO_LAB_CANCEL", meeting_page::CHAT);
            }
            _ => {}
        }
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "mode {mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn isolated_control_child() {
    let Ok(mode) = std::env::var("PROTO_SCENARIO_CASE") else {
        return;
    };
    let temp = tempfile::tempdir().unwrap();
    let status = if mode == "cancel-failed" { 503 } else { 200 };
    let mut page = meeting_page::page(meeting_page::creation(), status, 200).await;
    let mut session = page.session(temp.path(), false);
    let mut report = ScenarioReport::new(ScenarioName::MeetingSteps);
    assert_eq!(alias_of("PROTO_MISSING_ALIAS", Alias::B), Alias::B);
    assert_eq!(
        alias_of("PROTO_TEST_ALIAS", Alias::B).to_string(),
        std::env::var("PROTO_EXPECTED_ALIAS").unwrap()
    );
    let socket = temp.path().join("steps.sock");
    let client = async {
        loop {
            match UnixStream::connect(&socket).await {
                Ok(mut stream) => {
                    stream.write_all(b"quit\n").await.unwrap();
                    stream.shutdown().await.unwrap();
                    let mut answer = String::new();
                    stream.read_to_string(&mut answer).await.unwrap();
                    assert_eq!(answer, "bye\n");
                    return;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
    };
    let who = Who {
        alias: Alias::A,
        me: "8:orgid:00000000-0000-0000-0000-00000000000a",
        them: "8:orgid:00000000-0000-0000-0000-00000000000b",
        peer_upn: "peer@example.invalid",
    };
    let (result, client) = tokio::join!(
        steps(&who, &mut session, &mut [], &mut report, &socket, options()),
        tokio::time::timeout(Duration::from_secs(30), client)
    );
    client.unwrap();
    assert_eq!(result.is_err(), mode == "cancel-failed", "{result:?}");
    if matches!(mode.as_str(), "schedule" | "cancel") {
        assert!(report.notes.iter().any(|note| note.ends_with("was cancelled")));
    }
    if mode == "cancel-failed" {
        assert!(report.notes.iter().any(|note| note.contains("wasn't cancelled")));
    }
}
