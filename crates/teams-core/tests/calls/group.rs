//! A group call's participants and video (M3 Task 19), on the mock with group video on: each party's media is a mixer
//! of its own (the bundled H.264), which offers a callee its group lines, answers a caller's or a joiner's group offer,
//! takes source requests over the data channel and sends a test pattern on every line a request gave a source; the
//! mock delivers the call's roster to everyone in it. Every account is a native engine.
//!
//! - a calls the a–b–c group chat, b and c answer: b's camera, turned on while its answer goes, is refused and never
//!   opens; the start and both answers describe their lines and seed the bandwidth; each sees the other two, each on
//!   a receiving line of its own, and asks its mixer for no video while their cameras are off; b's camera goes on
//!   without an SDP change (its lines described again), the others see it on and b's mixer receives it; with every
//!   camera on each asks its mixer for the other two's video and decodes frames on both lines; b's mixer renegotiates
//!   and b's answer describes its lines and seeds the bandwidth again, its video going on; the mixer's dominant
//!   speaker is told; c leaves and the others see it gone, its line's source taken back.
//! - c's ring runs out, and c joins the running call late (its join describing its lines and seeding the bandwidth):
//!   it sees a and b, and they see it.
//! - a calls without video: it shows b and c on no line, and nobody speaking while it hears the mix.

use std::time::Duration;

use crate::support::{A_TONE, Account, B_TONE, Lab, PEER_TONE, TALK, actual_callback, heard, wait_until};
use serde_json::Value;
use teams_core::{CallAction, CallActionError, CallError, CallId, CallStatus, CallView, End, EngineError, Event};
use teams_mock::{ABC_CHAT, LAB_A, LAB_B, LAB_C, TIMED_OUT};

/// Frames an account must decode on each line of the other participants.
const FRAMES: u64 = 15;

/// The lines `view` shows its participants on, each `Some`.
fn lines(view: &CallView) -> Vec<usize> {
    view.participants.iter().filter_map(|p| p.line).collect()
}

/// `view` shows exactly `who`, each on a receiving line of its own.
fn shows(view: &CallView, who: &[&str]) -> bool {
    let mut lines = lines(view);
    lines.sort_unstable();
    lines.dedup();
    view.participants.len() == who.len()
        && who.iter().all(|mri| view.participants.iter().any(|p| p.mri == *mri))
        && lines.len() == who.len()
        && lines.iter().all(|line| (1..=teams_core::GROUP_SLOTS).contains(line))
}

/// The `mediaContent` of each start, join, `accept` and renegotiation answer the mock got, by which it was: each
/// group call's request describes its lines and seeds the bandwidth the mixer may send at (without them the live
/// mixer sent no video).
async fn media_contents(lab: &Lab) -> Vec<(&'static str, Value)> {
    let kinds = [
        ("start or join", "/callInvitation/mediaContent"),
        ("accept", "/callAcceptance/mediaContent"),
        ("answer", "/mediaAnswer/mediaContent"),
    ];
    lab.mock
        .received_requests()
        .await
        .iter()
        .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
        .flat_map(|body| {
            kinds
                .iter()
                .filter_map(|(kind, pointer)| Some((*kind, body.pointer(pointer)?.clone())))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// `content` describes its lines and seeds the bandwidth, as a group call's start, join, `accept` and answers do.
fn described_and_seeded(content: &Value) -> bool {
    let described = content
        .pointer("/mediaDescriptions/descriptions")
        .and_then(Value::as_array)
        .is_some_and(|lines| !lines.is_empty());
    let seeded = content
        .pointer("/applyChannelParameters/multiChannelParameter/mediaParameter")
        .and_then(Value::as_str)
        .is_some_and(|parameter| parameter.contains("sendSideBWSeed"));
    described && seeded
}

/// The line `view` shows `who` on.
fn line_of(view: &CallView, who: &str) -> usize {
    view.participants
        .iter()
        .find(|p| p.mri == who)
        .and_then(|p| p.line)
        .expect("a line")
}

/// `account` turns its camera on in `call`.
async fn camera_on(account: &Account, call: CallId) {
    let requests = account.handle.requests();
    assert_eq!(
        requests.call(CallAction::Camera { call, on: true }).await.unwrap(),
        call
    );
}

/// `account`'s call, up: the view it reaches showing `who`, and frames decoded on each of their lines.
async fn sees(account: &mut Account, call: CallId, who: &[&str]) -> CallView {
    let view = account.view(call, |view| shows(view, who)).await;
    let video = account.video.clone();
    let lines = lines(&view);
    wait_until(|| lines.iter().all(|line| video.line(*line) >= FRAMES)).await;
    view
}

/// `account` answers `ringing` and is connected: its call.
async fn answer(account: &mut Account, ringing: CallId) -> CallId {
    let call = account
        .handle
        .requests()
        .call(CallAction::Answer { call: ringing })
        .await
        .unwrap();
    account.status(call, &CallStatus::Connected).await;
    call
}

/// `account` answers `ringing`, turning its camera on while its answer goes (the call not up yet): the camera is
/// refused, its device never opened and nothing described; the call connects. Its call.
async fn answer_turning_the_camera_on_early(lab: &Lab, account: &mut Account, ringing: CallId) -> CallId {
    lab.mock.delay_next_call_op("accept", Duration::from_millis(1_500));
    let call = account
        .handle
        .requests()
        .call(CallAction::Answer { call: ringing })
        .await
        .unwrap();
    let early = account
        .handle
        .requests()
        .call(CallAction::Camera { call, on: true })
        .await;
    assert!(
        matches!(early, Err(EngineError::Call(CallActionError::Call(CallError::NotNow)))),
        "{early:?}"
    );
    account.status(call, &CallStatus::Connected).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(account.video.preview.counts().frames, 0, "the camera never opened");
    assert!(!lab.sent_by(account.mri).contains(&"updateMediaDescriptions".to_owned()));
    call
}

/// b's mixer renegotiates: b's answer describes its lines and seeds the bandwidth again, and the video on its `line`
/// goes on.
async fn mixer_renegotiates(lab: &Lab, b: &Account, line: usize) {
    let before = b.video.line(line);
    assert!(lab.mock.renegotiate(&lab.last_call(), LAB_B));
    wait_until(|| lab.sent_by(LAB_B).contains(&"answer".to_owned())).await;
    let answers: Vec<Value> = media_contents(lab)
        .await
        .into_iter()
        .filter_map(|(kind, content)| (kind == "answer").then_some(content))
        .collect();
    assert_eq!(answers.len(), 1);
    assert!(described_and_seeded(&answers[0]), "{}", answers[0]);
    let video = b.video.clone();
    wait_until(|| video.line(line) >= before + FRAMES).await;
}

/// `account` hangs up `call`, and it ends.
async fn hang_up(account: &mut Account, call: CallId) {
    account
        .handle
        .requests()
        .call(CallAction::HangUp { call })
        .await
        .unwrap();
    assert_eq!(account.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_call_shows_each_participant_on_a_line_of_its_own() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let mut c = lab.account(LAB_C, PEER_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let (ringing_b, ringing_c) = (b.incoming().await, c.incoming().await);
    let id = lab.last_call();

    let call_b = answer_turning_the_camera_on_early(&lab, &mut b, ringing_b.call).await;
    let call_c = answer(&mut c, ringing_c.call).await;
    a.status(call_a, &CallStatus::Connected).await;

    // the start and both answers describe their lines and seed the bandwidth
    let contents = media_contents(&lab).await;
    let kinds: Vec<&str> = contents.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(kinds, ["start or join", "accept", "accept"], "{kinds:?}");
    for (kind, content) in &contents {
        assert!(described_and_seeded(content), "{kind}: {content}");
    }

    // each sees the other two on lines of its own; with their cameras off, it asks its mixer for no video
    a.view(call_a, |view| shows(view, &[LAB_B, LAB_C])).await;
    b.view(call_b, |view| shows(view, &[LAB_A, LAB_C])).await;
    c.view(call_c, |view| shows(view, &[LAB_A, LAB_B])).await;
    for account in [LAB_A, LAB_B, LAB_C] {
        let stats = lab.mock.mixer_stats(&id, account).unwrap();
        assert!(stats.requests.is_empty(), "{account}: {stats:?}");
    }

    // b's camera on: its lines described again (no renegotiation), the others see it on, its mixer gets its video
    camera_on(&b, call_b).await;
    a.view(call_a, |view| {
        view.participants.iter().any(|p| p.mri == LAB_B && p.video_on)
    })
    .await;
    wait_until(|| lab.mock.mixer_stats(&id, LAB_B).unwrap().received >= FRAMES).await;
    let sent = lab.sent_by(LAB_B);
    assert!(sent.contains(&"updateMediaDescriptions".to_owned()), "{sent:?}");
    assert!(sent.contains(&"applyChannelParameters".to_owned()), "{sent:?}");
    assert!(!sent.contains(&"renegotiate".to_owned()), "{sent:?}");

    // every camera on: each asks its mixer for the other two's video, and decodes it
    camera_on(&a, call_a).await;
    camera_on(&c, call_c).await;
    sees(&mut a, call_a, &[LAB_B, LAB_C]).await;
    let view_b = sees(&mut b, call_b, &[LAB_A, LAB_C]).await;
    sees(&mut c, call_c, &[LAB_A, LAB_B]).await;
    for account in [LAB_A, LAB_B, LAB_C] {
        let stats = lab.mock.mixer_stats(&id, account).unwrap();
        assert_eq!(stats.handshakes, 1, "{account}: one control plane");
        let mut sources: Vec<i64> = stats.requests.iter().map(|(_, source)| *source).collect();
        sources.dedup();
        assert_eq!(
            sources.len(),
            2,
            "{account}: the other two's video, once each: {stats:?}"
        );
    }

    mixer_renegotiates(&lab, &b, line_of(&view_b, LAB_C)).await;

    // who speaks: a's mixer says c
    assert!(lab.mock.speak(&id, LAB_A, &[LAB_C]));
    a.view(call_a, |view| {
        view.participants.iter().any(|p| p.mri == LAB_C && p.speaking)
            && view.participants.iter().all(|p| p.mri == LAB_C || !p.speaking)
    })
    .await;

    // c leaves: a and b see it gone, its line given back
    hang_up(&mut c, call_c).await;
    a.view(call_a, |view| shows(view, &[LAB_B])).await;
    b.view(call_b, |view| shows(view, &[LAB_A])).await;
    wait_until(|| {
        lab.mock
            .mixer_stats(&id, LAB_A)
            .unwrap()
            .requests
            .iter()
            .any(|(_, source)| *source == -1)
    })
    .await;

    // the call runs on for a and b
    hang_up(&mut a, call_a).await;
    hang_up(&mut b, call_b).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_joiner_is_shown_and_sees_everyone() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let mut c = lab.account(LAB_C, PEER_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let ringing_b = b.incoming().await;
    let _ = c.incoming().await;
    let id = lab.last_call();
    let call_b = answer(&mut b, ringing_b.call).await;
    camera_on(&a, call_a).await;
    camera_on(&b, call_b).await;
    sees(&mut a, call_a, &[LAB_B]).await;

    // c's ring runs out; it joins the running call
    assert!(lab.mock.end_ring(&id, 487, TIMED_OUT));
    c.next(|event| matches!(event, teams_core::Event::RingEnded { .. }).then_some(()))
        .await;
    let call_c = c
        .handle
        .requests()
        .call(CallAction::Join { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    c.status(call_c, &CallStatus::Connected).await;
    camera_on(&c, call_c).await;
    sees(&mut c, call_c, &[LAB_A, LAB_B]).await;
    sees(&mut a, call_a, &[LAB_B, LAB_C]).await;
    sees(&mut b, call_b, &[LAB_A, LAB_C]).await;
    assert!(lab.sent_by(LAB_C).contains(&"join".to_owned()));
    let contents = media_contents(&lab).await;
    let starts: Vec<&Value> = contents
        .iter()
        .filter_map(|(kind, content)| (*kind == "start or join").then_some(content))
        .collect();
    assert_eq!(starts.len(), 2, "a's start and c's join");
    assert!(starts.iter().all(|content| described_and_seeded(content)));

    for (account, call) in [(&mut a, call_a), (&mut b, call_b), (&mut c, call_c)] {
        hang_up(account, call).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_call_without_video_shows_nobody_on_a_line_and_nobody_speaking() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account_without_video(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let mut c = lab.account(LAB_C, PEER_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let (ringing_b, ringing_c) = (b.incoming().await, c.incoming().await);
    let call_b = answer(&mut b, ringing_b.call).await;
    let call_c = answer(&mut c, ringing_c.call).await;
    a.status(call_a, &CallStatus::Connected).await;
    let shown = |view: &CallView| view.participants.len() == 2 && view.participants.iter().all(|p| p.line.is_none());
    a.view(call_a, shown).await;

    // the mix plays (a hears its mixer's tone throughout), and still nobody in it is marked speaking: the mix says
    // nothing of who in it speaks, and without video no control plane tells
    let views: Vec<CallView> = a
        .events_within(TALK)
        .await
        .into_iter()
        .filter_map(|event| match event {
            Event::CallParticipants { call, view } if call == call_a => Some(view),
            _ => None,
        })
        .collect();
    for view in &views {
        assert!(shown(view), "{view:?}");
        assert!(view.participants.iter().all(|p| !p.speaking), "{view:?}");
    }
    let camera = a
        .handle
        .requests()
        .call(CallAction::Camera { call: call_a, on: true })
        .await;
    assert!(
        matches!(camera, Err(EngineError::Call(CallActionError::NoVideo))),
        "{camera:?}"
    );

    for (account, call) in [(&mut a, call_a), (&mut b, call_b), (&mut c, call_c)] {
        hang_up(account, call).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mix = heard(&a.recording, PEER_TONE, TALK);
    assert!(mix > 0.25, "a heard its mixer's tone at {mix:.3}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admitted_group_call_reports_lobby_transitions_and_cleans_up_failed_replacement_media() {
    let lab = Lab::start_group_video().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call_id = lab.mock.ring_group(LAB_A, ABC_CHAT, &[LAB_B]);
    let ringing = b.incoming().await;
    let call = answer(&mut b, ringing.call).await;
    let roster = actual_callback(
        &lab,
        "/additionalActions/0/input/conversationRequest/roster/rosterUpdate",
    )
    .await;
    let waiting = serde_json::json!({"participants":{LAB_B:{"version":1000,"state":"active","endpoints":{"synthetic-lobby":{"lobby":{}}}}}});
    let _ = lab.trouter.deliver_call(LAB_B, &roster, &waiting);
    b.status(call, &CallStatus::Lobby).await;
    let admitted = serde_json::json!({"participants":{LAB_B:{"version":1001,"state":"active","endpoints":{"synthetic-call":{"call":{"mediaStreams":[]}}}}}});
    let _ = lab.trouter.deliver_call(LAB_B, &roster, &admitted);
    b.status(call, &CallStatus::Connected).await;
    let callback = actual_callback(&lab, "/callAcceptance/links/mediaRenegotiation").await;
    // A new mixer session uses its own setup; malformed SDP fails it and releases the existing leg once.
    let replacement = serde_json::json!({"mediaNegotiation":{
        "mediaContent":{"blob":"synthetic invalid SDP","contentType":"application/sdp-ngc-1.0","newOffer":true,"fromMixer":true},
        "links":{"mediaAnswer":format!("https://api.flightproxy.teams.microsoft.com/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/negotiations/{call_id}/1/t/4/answer?i=10-0-0-1")}
    }});
    let _ = lab.trouter.deliver_call(LAB_B, &callback, &replacement);
    assert_eq!(b.ended(call).await, End::Failed("media"));
    wait_until(|| lab.sent_by(LAB_B).iter().filter(|op| op.as_str() == "leave").count() == 1).await;
    assert!(
        b.handle
            .requests()
            .call(CallAction::Mute { call, muted: true })
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_real_new_mixer_session_replaces_media_and_keeps_gallery_and_camera() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let ringing = b.incoming().await;
    let call = answer(&mut b, ringing.call).await;
    a.status(call_a, &CallStatus::Connected).await;
    let call_id = lab.last_call();
    let before = b.view(call, |view| !view.participants.is_empty()).await;
    camera_on(&b, call).await;
    b.handle
        .requests()
        .call(CallAction::Mute { call, muted: true })
        .await
        .unwrap();
    let replacement = teams_mock::mixer::MixerPeer::start_at(lab.relay, 80_000, PEER_TONE).unwrap();
    let offer = replacement.offer().unwrap();
    let callback = actual_callback(&lab, "/callAcceptance/links/mediaRenegotiation").await;
    let answer_path = format!("/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/negotiations/{call_id}/1/t/4/answer");
    lab.mock
        .answer_next("POST", &answer_path, 200, &serde_json::json!({}))
        .await;
    let negotiation = serde_json::json!({"mediaNegotiation":{
        "mediaContent":{"blob":offer.as_str(),"contentType":offer.content_type(),"newOffer":true,"fromMixer":true},
        "links":{"mediaAnswer":format!("https://api.flightproxy.teams.microsoft.com{answer_path}?i=10-0-0-1")}
    }});
    let _ = lab.trouter.deliver_call(LAB_B, &callback, &negotiation);
    lab.wait_attempts(&answer_path, 1).await;
    let content = lab
        .mock
        .received_requests()
        .await
        .into_iter()
        .find_map(|request| {
            if request.url.path() != answer_path {
                return None;
            }
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            body.pointer("/mediaAnswer/mediaContent").cloned()
        })
        .expect("Core answered with the replacement session's SDP");
    assert!(described_and_seeded(&content));
    let answer = teams_media::sdp::NgcSdp::new(
        content["contentType"].as_str().unwrap(),
        content["blob"].as_str().unwrap(),
    )
    .unwrap();
    replacement.accept_answer(&answer).unwrap();
    wait_until(|| replacement.stats().handshakes > 0 && replacement.stats().received >= FRAMES).await;
    let after = b
        .view(call, |view| {
            view.participants.iter().map(|p| &p.mri).collect::<Vec<_>>()
                == before.participants.iter().map(|p| &p.mri).collect::<Vec<_>>()
        })
        .await;
    assert_eq!(after.participants.len(), before.participants.len());
    b.handle
        .requests()
        .call(CallAction::Mute { call, muted: false })
        .await
        .unwrap();
    hang_up(&mut b, call).await;
    hang_up(&mut a, call_a).await;
    assert_eq!(lab.attempts(&answer_path).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_meetings_request_to_join_rings_with_its_organizer_and_tenant_and_a_group_ring_with_none() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let chat = "19:meeting_MadeUpRingCheck0001@thread.v2";
    let organizer = LAB_A.trim_start_matches("8:orgid:");
    let info = serde_json::json!({"organizerId": organizer, "tenantId": teams_mock::TENANT});
    let asked = lab.mock.ring_meeting(LAB_A, chat, &[LAB_B], info);
    let ringing = b.incoming().await;
    assert_eq!(ringing.group_chat.as_deref(), Some(chat));
    let meeting = ringing.meeting.clone().expect("the ring names its meeting");
    assert_eq!(
        (meeting.organizer_id.as_str(), meeting.tenant_id.as_str()),
        (organizer, teams_mock::TENANT)
    );
    assert!(lab.mock.end_ring(&asked, 487, TIMED_OUT));
    b.next(|event| matches!(event, Event::RingEnded { .. }).then_some(()))
        .await;

    let _group = lab.mock.ring_group(LAB_A, ABC_CHAT, &[LAB_B]);
    let ringing = b.incoming().await;
    assert_eq!((ringing.group_chat.as_deref(), ringing.meeting), (Some(ABC_CHAT), None));
}
