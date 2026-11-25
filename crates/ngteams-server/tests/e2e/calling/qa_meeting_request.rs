//! QA of task 810 (round 1): Request to join in a meeting on the own server, on SQLite and on Postgres + Redis. The
//! organizer's engine asks an invitee into its meeting. A client's raw socket reads the invitation's shape: the
//! meeting's organizer and tenant and `conversationType` `scheduledMeeting`. A second invitee's engine rings in the
//! meeting's chat, declines (the organizer is told), is asked again and joins. Task 831 adds the invitee's pre-join:
//! its Join now answers the ring, or after the ring joins by the meeting the ring named. Every account and meeting is
//! synthetic.

use std::time::Duration;

use base64::Engine as _;
use serde_json::Value;
use teams_core::engine::Event;
use teams_core::{CallAction, CallStatus, EventWrite};
use teams_proto::Mri;

use super::client::Client;
use super::engines::A_TONE;
use super::group_media::{C_TONE, Mixed};
use super::meetings::{Person, join_of, joined, leave_all, shut_down};
use crate::calendar::meetings::invited;
use crate::calendar::{MONDAY, draft};
use crate::support::backends;
use crate::trouter::Account;

/// How long the organizer waits to be told that an invitee declined.
const TOLD_WITHIN: Duration = Duration::from_secs(15);

/// The decoded `gp` of each call invitation at `client`'s endpoint root (not a meeting's start notice).
fn invitations(client: &Client) -> Vec<Value> {
    client
        .root_deliveries()
        .iter()
        .filter_map(|delivery| delivery["gp"].as_str().map(str::to_owned))
        .filter_map(|gp| base64::engine::general_purpose::STANDARD.decode(gp).ok())
        .filter_map(|gp| serde_json::from_slice::<Value>(&gp).ok())
        .filter(|gp| gp.get("callNotification").is_some())
        .collect()
}

/// The organizer's meeting, with `cora` and Eve (`eve_upn`) invited: its event.
async fn meeting_of(ada: &Person, cora: &Person, eve_upn: &str) -> teams_proto::CalendarEvent {
    let mut meeting = draft("Request check", MONDAY + 9 * crate::calendar::HOUR, 30);
    meeting.online = true;
    meeting.attendees = vec![
        invited(&cora.upn, &cora.name, None, true),
        invited(eve_upn, "Eve", None, true),
    ];
    let event = ada
        .engine
        .handle
        .write_event(EventWrite::Create(meeting))
        .await
        .unwrap()
        .created
        .unwrap();
    for person in [ada, cora] {
        assert!(person.engine.handle.sync().await.unwrap().failed.is_empty());
    }
    event
}

/// The invitation Eve's raw client got for the organizer's Request to join names the meeting, as Teams' does.
async fn eve_asked(ada: &Person, a_call: teams_core::CallId, eve: &Client, eve_mri: &str, chat: &str) {
    ada.engine
        .call(CallAction::Add {
            call: a_call,
            invitee: Mri::new(eve_mri),
        })
        .await;
    super::engines::wait_until("Eve's invitation", || !invitations(eve).is_empty()).await;
    let gp = invitations(eve).remove(0);
    let notification = &gp["callNotification"];
    assert_eq!(gp["groupChat"]["threadId"], chat, "{gp:#}");
    assert_eq!(
        notification["invitationData"]["meetingInfo"]["organizerId"],
        ada.id(),
        "{notification:#}"
    );
    assert!(
        notification["invitationData"]["meetingInfo"]["tenantId"]
            .as_str()
            .is_some_and(|tenant| !tenant.is_empty()),
        "{notification:#}"
    );
    assert_eq!(notification["conversationType"], "scheduledMeeting");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa810_a_hosts_request_to_join_rings_with_the_meeting_and_a_decline_is_told() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let eve_upn = format!("eve-{tag}@own.test");
        let eve_id = mixed.server().add_user(&eve_upn, "Eve").await;
        let eve_account = Account::sign_in(mixed.server(), &eve_upn, &eve_id).await;
        let eve = Client::start(&eve_account, "Eve", mixed.server(), mixed.server()).await;
        let event = meeting_of(&ada, &cora, &eve_upn).await;
        let chat = event.meeting_chat.clone().unwrap();
        let a_call = joined(&mut ada, &join_of(&event)).await;

        eve_asked(&ada, a_call, &eve, &eve_account.mri, &chat).await;
        // the native client rings in the meeting's chat and declines: the organizer is told
        let cora_mri = Mri::new(cora.engine.mri.clone());
        ada.engine
            .call(CallAction::Add {
                call: a_call,
                invitee: cora_mri.clone(),
            })
            .await;
        let ringing = cora.engine.incoming().await;
        assert_eq!(ringing.group_chat.as_deref(), Some(chat.as_str()));
        cora.engine.call(CallAction::Decline { call: ringing.call }).await;
        let told = tokio::time::timeout(
            TOLD_WITHIN,
            ada.engine.next(|event| match event {
                Event::CallAddFailed { who, declined, .. } if *who == cora_mri => Some(*declined),
                _ => None,
            }),
        )
        .await;
        // asked again, Cora joins the meeting
        ada.engine
            .call(CallAction::Add {
                call: a_call,
                invitee: cora_mri.clone(),
            })
            .await;
        let ringing = cora.engine.incoming().await;
        let c_call = cora.engine.call(CallAction::Answer { call: ringing.call }).await;
        cora.engine.status(c_call, &CallStatus::Connected).await;
        let cora_in = |view: &teams_core::CallView| view.participants.iter().any(|one| one.mri == cora_mri.as_str());
        ada.engine.view(a_call, cora_in).await;
        leave_all(&mut [&mut cora, &mut ada], &[c_call, a_call]).await;
        drop(eve);
        shut_down(vec![ada, cora], mixed).await;
        assert_eq!(
            told.ok(),
            Some(true),
            "the organizer was not told that the invitee declined"
        );
    }
}

/// Task 831: the invitee's pre-join, as its actions reach the own server. Accept sends nothing, so the ring rings on
/// while the pre-join shows. Join now answers the ring, and Cora is in the meeting. When the ring ended before Join
/// now (here Cora declines it), Join now is the meeting's own join, by the organizer and tenant the ring named.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qa831_an_invitee_joins_through_the_pre_join_by_the_rings_answer_or_after_the_ring_by_its_meeting() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let eve_upn = format!("eve-{tag}@own.test");
        mixed.server().add_user(&eve_upn, "Eve").await;
        let event = meeting_of(&ada, &cora, &eve_upn).await;
        let chat = event.meeting_chat.clone().unwrap();
        let a_call = joined(&mut ada, &join_of(&event)).await;
        let cora_mri = Mri::new(cora.engine.mri.clone());
        let cora_in = |view: &teams_core::CallView| view.participants.iter().any(|one| one.mri == cora_mri.as_str());
        let ask_cora = CallAction::Add {
            call: a_call,
            invitee: cora_mri.clone(),
        };

        ada.engine.call(ask_cora.clone()).await;
        let ringing = cora.engine.incoming().await;
        assert_eq!(ringing.group_chat.as_deref(), Some(chat.as_str()));
        let meeting = ringing.meeting.clone().expect("the ring names its meeting");
        assert_eq!(meeting.organizer_id, ada.id());
        assert_eq!(Some(&meeting), event.meeting.as_ref(), "the meeting the calendar names");
        // the pre-join shows: nothing goes out, and the ring rings on
        tokio::time::sleep(Duration::from_secs(2)).await;
        let c_call = cora.engine.call(CallAction::Answer { call: ringing.call }).await;
        assert_eq!(c_call, ringing.call, "Join now answers the ring that still rings");
        cora.engine.status(c_call, &CallStatus::Connected).await;
        ada.engine.view(a_call, cora_in).await;
        cora.engine.call(CallAction::HangUp { call: c_call }).await;
        cora.engine.ended(c_call).await;
        ada.engine.view(a_call, |view| !cora_in(view)).await;

        // asked again; the ring ends while the pre-join shows; Join now joins by the meeting the ring named
        ada.engine.call(ask_cora).await;
        let ringing = cora.engine.incoming().await;
        cora.engine.call(CallAction::Decline { call: ringing.call }).await;
        cora.engine.ended(ringing.call).await;
        let by_link = CallAction::JoinMeeting {
            chat: chat.clone(),
            meeting: ringing.meeting.clone().expect("the ring names its meeting"),
        };
        let c_call = joined(&mut cora, &by_link).await;
        ada.engine.view(a_call, cora_in).await;
        leave_all(&mut [&mut cora, &mut ada], &[c_call, a_call]).await;
        shut_down(vec![ada, cora], mixed).await;
    }
}
