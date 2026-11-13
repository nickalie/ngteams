//! A 1:1 call's shared screen, on the mock and its loopback peer (`docs/protocol/calling.md`, "More of a running
//! call"): the far end's share answered receiving with `["Audio", "ScreenViewer"]` and shown on the sharing line's sink
//! until its offer rejects that line; the account's own Share telling its conversation it shares and renegotiating
//! with `["Audio", "ScreenSharer"]`, the far end shown its screen, and Stop sharing renegotiating it away. A group
//! call's share describes its sharing line instead, renegotiating nothing.

use serde_json::Value;

use crate::support::{A_TONE, Account, B_TONE, Lab, PEER_TONE, wait_until};
use teams_core::{CallAction, CallId, CallStatus, End, Event};
use teams_mock::{AB_CHAT, ABC_CHAT, LAB_A, LAB_B, LAB_C};

/// Frames of a shared screen a side must have shown.
const FRAMES: u64 = 15;

/// The next share state `account` is told for `call`: its own (`own`), or the far end's.
async fn share_told(account: &mut Account, call: CallId, own: bool) -> bool {
    account
        .next(|event| match event {
            Event::CallSharing { call: of, on } if own && *of == call => Some(*on),
            Event::CallRemoteShare { call: of, on } if !own && *of == call => Some(*on),
            _ => None,
        })
        .await
}

/// The `callModalities` of every renegotiation and renegotiation answer `mri` sent, in order.
async fn modalities(lab: &Lab, mri: &str) -> Vec<Vec<String>> {
    lab.mock
        .received_requests()
        .await
        .into_iter()
        .filter_map(|request| {
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            let sent = body
                .pointer("/mediaNegotiation")
                .or_else(|| body.pointer("/mediaAnswer"))?;
            (sent.pointer("/sender/id")?.as_str()? == mri).then(|| {
                sent["callModalities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|modality| modality.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .collect()
}

/// Waits until the last renegotiation or answer `mri` sent named `wanted` (its write may go a moment after the state is
/// told); fails after five seconds.
async fn last_modalities(lab: &Lab, mri: &str, wanted: &[&str]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let last = modalities(lab, mri).await.pop().unwrap_or_default();
        if last == wanted {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{mri} last named {last:?}, not {wanted:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_far_end_s_screen_is_shown_on_its_sharing_line_until_it_stops() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;

    // the peer shares: b answers receiving, tells the share shown, and shows its frames apart from any camera's
    assert!(lab.mock.renegotiate_share(&id, LAB_B, true));
    assert!(share_told(&mut b, call, false).await, "the far end's share shown");
    wait_until(|| b.video.share.counts().indexed >= FRAMES).await;
    assert_eq!(
        b.video.remote.counts().frames,
        0,
        "no camera's frames: the far end's camera is off"
    );
    last_modalities(&lab, LAB_B, &["Audio", "ScreenViewer"]).await;

    // and stops: the line goes, so does the share
    assert!(lab.mock.renegotiate_share(&id, LAB_B, false));
    assert!(!share_told(&mut b, call, false).await, "the far end's share gone");
    last_modalities(&lab, LAB_B, &["Audio"]).await;

    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_end_shares_its_screen_and_the_other_is_shown_it() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: AB_CHAT.into() })
        .await
        .unwrap();
    let incoming = b.incoming().await;
    let answered = b
        .handle
        .requests()
        .call(CallAction::Answer { call: incoming.call })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    b.status(answered, &CallStatus::Connected).await;

    // a shares: its conversation told first, then its renegotiation adds the sharing line, which b is shown
    let share = |on| CallAction::Share { call, on };
    assert_eq!(a.handle.requests().call(share(true)).await.unwrap(), call);
    assert!(share_told(&mut a, call, true).await, "a shares");
    assert!(share_told(&mut b, answered, false).await, "b is shown a's screen");
    wait_until(|| b.video.share.counts().indexed >= FRAMES).await;
    assert_eq!(b.video.remote.counts().frames, 0, "a's camera stays off");
    let sent = lab.sent_by(LAB_A);
    let at = |op: &str| sent.iter().position(|sent| sent == op);
    assert!(
        at("updateParticipantProperties").is_some() && at("updateParticipantProperties") < at("renegotiate"),
        "{sent:?}"
    );
    last_modalities(&lab, LAB_A, &["Audio", "ScreenSharer"]).await;
    last_modalities(&lab, LAB_B, &["Audio", "ScreenViewer"]).await;

    // and stops: the sharing line rejected, b's share gone
    assert_eq!(a.handle.requests().call(share(false)).await.unwrap(), call);
    assert!(!share_told(&mut a, call, true).await, "a no longer shares");
    assert!(!share_told(&mut b, answered, false).await, "b's share gone");
    last_modalities(&lab, LAB_A, &["Audio"]).await;

    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

/// A 1:1 call's native end shares, stops and shares again, as a user does: the far end is shown the second share's
/// frames too, not only told that it shares.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_end_shares_again_after_stopping_and_the_other_is_shown_it_again() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: AB_CHAT.into() })
        .await
        .unwrap();
    let incoming = b.incoming().await;
    let answered = b
        .handle
        .requests()
        .call(CallAction::Answer { call: incoming.call })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    b.status(answered, &CallStatus::Connected).await;

    let share = |on| CallAction::Share { call, on };
    let mut shown = 0;
    for round in 1..=2 {
        // the stop's renegotiation settles first, as it has by the time a user shares again
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match a.handle.requests().call(share(true)).await {
                Ok(shared) => break assert_eq!(shared, call),
                Err(error) if std::time::Instant::now() < deadline => {
                    assert!(format!("{error:?}").contains("NotNow"), "{error:?}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Err(error) => panic!("a can't share again ({round}): {error:?}"),
            }
        }
        assert!(share_told(&mut a, call, true).await, "a shares ({round})");
        assert!(
            share_told(&mut b, answered, false).await,
            "b is told a's share ({round})"
        );
        let wanted = shown + FRAMES;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while b.video.share.counts().indexed < wanted {
            assert!(
                std::time::Instant::now() < deadline,
                "share {round}: b was shown {:?} of a's frames, not {wanted} indexed",
                b.video.share.counts()
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        shown = b.video.share.counts().indexed;
        last_modalities(&lab, LAB_A, &["Audio", "ScreenSharer"]).await;

        assert_eq!(a.handle.requests().call(share(false)).await.unwrap(), call);
        assert!(!share_told(&mut a, call, true).await, "a no longer shares ({round})");
        assert!(!share_told(&mut b, answered, false).await, "b's share gone ({round})");
        last_modalities(&lab, LAB_A, &["Audio"]).await;
    }

    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

/// The bodies of every `updateMediaDescriptions` the mock got, in order.
async fn descriptions(lab: &Lab) -> Vec<Value> {
    lab.mock
        .received_requests()
        .await
        .into_iter()
        .filter(|request| request.url.path().ends_with("/updateMediaDescriptions"))
        .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
        .map(|body| body["UpdateMediaDescriptions"]["mediaDescriptions"].clone())
        .collect()
}

/// The last `updateMediaDescriptions` describes the sharing line going `direction` (labelled while it sends), tagged
/// as a share's; waits for it up to five seconds.
async fn shares_described(lab: &Lab, direction: &str) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let last = descriptions(lab).await.pop().unwrap_or_default();
        let sharing = last["descriptions"]
            .as_array()
            .and_then(|lines| lines.last())
            .cloned()
            .unwrap_or_default();
        let labelled = sharing.get("label").and_then(Value::as_str);
        let tagged = last["negotiationTag"]
            .as_str()
            .is_some_and(|tag| tag.ends_with(";ss_1"));
        let wanted_label = (direction == "sendonly").then_some("applicationsharing-video");
        if sharing["direction"] == direction && labelled == wanted_label && tagged {
            return last;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "not described {direction}: {last}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A group call's share (`docs/protocol/calling.md`, "The native client's share, group call or meeting"): a calls the
/// a–b–c chat (its own group offer), b and c answer the mixer's offers. b shares: its conversation told, then its
/// lines described with the sharing line `sendonly`, labelled and tagged, nothing renegotiated; Stop sharing describes
/// it `recvonly` with the same tag. a, whose sharing line its own offer made, shares the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_call_shares_on_its_sharing_line_by_describing_it_without_a_renegotiation() {
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
    let mut answered = Vec::new();
    for (account, ringing) in [(&mut b, ringing_b.call), (&mut c, ringing_c.call)] {
        let call = account
            .handle
            .requests()
            .call(CallAction::Answer { call: ringing })
            .await
            .unwrap();
        account.status(call, &CallStatus::Connected).await;
        answered.push(call);
    }
    a.status(call_a, &CallStatus::Connected).await;
    let call_b = answered[0];

    let share = |call, on| CallAction::Share { call, on };
    assert_eq!(b.handle.requests().call(share(call_b, true)).await.unwrap(), call_b);
    assert!(share_told(&mut b, call_b, true).await, "b shares");
    shares_described(&lab, "sendonly").await;
    let sent = lab.sent_by(LAB_B);
    let at = |op: &str| sent.iter().position(|sent| sent == op);
    assert!(
        at("updateParticipantProperties").is_some()
            && at("updateParticipantProperties") < at("updateMediaDescriptions"),
        "{sent:?}"
    );
    assert!(!sent.contains(&"renegotiate".to_owned()), "{sent:?}");

    assert_eq!(b.handle.requests().call(share(call_b, false)).await.unwrap(), call_b);
    assert!(!share_told(&mut b, call_b, true).await, "b no longer shares");
    shares_described(&lab, "recvonly").await;

    assert_eq!(a.handle.requests().call(share(call_a, true)).await.unwrap(), call_a);
    assert!(
        share_told(&mut a, call_a, true).await,
        "a shares on its own offer's line"
    );
    shares_described(&lab, "sendonly").await;
    assert!(!lab.sent_by(LAB_A).contains(&"renegotiate".to_owned()));

    for (account, call) in [(&mut a, call_a), (&mut b, call_b), (&mut c, answered[1])] {
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        assert_eq!(account.ended(call).await, End::HungUp);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_without_video_shares_nothing() {
    let lab = Lab::start().await;
    let mut b = lab.account_without_video(LAB_B, B_TONE).await;
    let _id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    assert!(requests.call(CallAction::Share { call, on: true }).await.is_err());
    assert!(!lab.sent_by(LAB_B).contains(&"renegotiate".to_owned()));
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
}
