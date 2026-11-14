//! Real engine and outbox operations against an own-origin synthetic service; no tenant is reached.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the shared diagnostic helpers also serve other integration tests"
)]
mod common;

use teams_core::engine::{Action, EngineHandle, Event, MessageChange};
use teams_mock::{AB_CHAT, ChatServiceMock, LAB_A, LAB_B, LAB_C};
use teams_proto::chat::MessageText;
use teams_proto::{Cloud, Mri};
use teams_store::OutboxState;

async fn changed(handle: &mut EngineHandle, chat: &str, id: &str, change: MessageChange) {
    common::event(handle, |event| match event {
        Event::Message {
            conversation,
            id: got,
            change: kind,
            ..
        } if conversation == chat && got == id && *kind == change => Some(()),
        _ => None,
    })
    .await;
}

async fn send(handle: &EngineHandle, chat: &str, text: &str) -> String {
    let row = handle
        .write(Action::Send {
            conversation: chat.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent);
    row.message_id.unwrap()
}

#[tokio::test]
async fn own_engines_sync_send_edit_react_read_and_create_a_chat() {
    let cloud = Cloud::own("https://chat.example:8443").unwrap();
    let mock = ChatServiceMock::start_for_cloud(cloud.clone()).await.unwrap();
    mock.serve_presence(&[]).await;
    mock.serve_presence_subscriptions(&[]).await;
    let trouter = mock.start_trouter().unwrap();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let names = ["Lab A", "Lab B"];
    let mut a = common::native_for_cloud(&mock, &trouter, LAB_A, names, a_dir.path(), &cloud);
    let mut b = common::native_for_cloud(&mock, &trouter, LAB_B, names, b_dir.path(), &cloud);
    common::ready(&mut a).await;
    common::ready(&mut b).await;
    assert!(
        a.conversations(100)
            .await
            .unwrap()
            .iter()
            .any(|chat| chat.id == AB_CHAT)
    );
    assert!(a.sync().await.unwrap().failed.is_empty());

    chat_actions(&mock, &a, &mut b).await;
    create_chat(&a, &mut b).await;
    presence_subscription(&mock, &trouter, &mut a).await;
    a.shutdown();
    b.shutdown();
}

async fn chat_actions(mock: &ChatServiceMock, a: &EngineHandle, b: &mut EngineHandle) {
    let id = send(a, AB_CHAT, "own-server first draft").await;
    changed(b, AB_CHAT, &id, MessageChange::New).await;
    let edit = a
        .write(Action::Edit {
            conversation: AB_CHAT.into(),
            message_id: id.clone(),
            text: MessageText::plain("own-server second draft").unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(edit.state, OutboxState::Sent);
    changed(b, AB_CHAT, &id, MessageChange::Edit).await;
    assert!(
        b.message(AB_CHAT, &id)
            .await
            .unwrap()
            .unwrap()
            .content
            .contains("second draft")
    );
    react_and_read(mock, a, b, &id).await;
}

async fn react_and_read(mock: &ChatServiceMock, a: &EngineHandle, b: &mut EngineHandle, id: &str) {
    let reaction = a
        .write(Action::React {
            conversation: AB_CHAT.into(),
            message_id: id.to_owned(),
            key: "like".into(),
        })
        .await
        .unwrap();
    assert_eq!(reaction.state, OutboxState::Sent);
    changed(b, AB_CHAT, id, MessageChange::Reaction).await;
    assert_eq!(b.reactions(AB_CHAT, id).await.unwrap()[0].user, LAB_A);
    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.into(),
            message_id: id.to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(read.state, OutboxState::Sent);
    assert!(
        mock.take_events(LAB_A)
            .iter()
            .any(|event| event.kind == teams_mock::EventKind::ReadReceipt && event.from == LAB_B)
    );
}

async fn create_chat(a: &EngineHandle, b: &mut EngineHandle) {
    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
            topic: Some("Own chat".into()),
        })
        .await
        .unwrap();
    assert_eq!(created.state, OutboxState::Sent);
    let chat = created.message_id.unwrap();
    let first = send(a, &chat, "new own chat").await;
    changed(b, &chat, &first, MessageChange::New).await;
    assert!(b.conversations(100).await.unwrap().iter().any(|entry| entry.id == chat));
    assert!(b.sync().await.unwrap().failed.is_empty());
}

async fn presence_subscription(mock: &ChatServiceMock, trouter: &teams_mock::TrouterMock, a: &mut EngineHandle) {
    a.presence(LAB_B).await.unwrap();
    let deadline = tokio::time::Instant::now() + common::WAIT;
    loop {
        let requests = mock.received_requests().await;
        if let Some(request) = requests
            .iter()
            .find(|request| request.url.path().contains("/pubsub/subscriptions/"))
        {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert!(
                body["trouterUri"]
                    .as_str()
                    .unwrap()
                    .starts_with("https://chat.example:8443/trouter/v4/f/")
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no own-origin presence subscription"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    trouter.deliver_presence(
        LAB_A,
        &serde_json::json!({"presence": [{
            "mri": LAB_B, "etag": "synthetic-presence-change", "presence": {
                "sourceNetwork": "SameEnterprise", "calendarData": {"isOutOfOffice": false},
                "capabilities": [], "availability": "Busy", "activity": "InACall",
                "lastActiveTime": "2023-11-14T22:13:20.000Z", "deviceType": "Web",
            }
        }]}),
        false,
    );
    common::event(a, |event| match event {
        Event::Presence { mri, availability } if mri == LAB_B && *availability == teams_core::Availability::Busy => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert_eq!(a.presence(LAB_B).await.unwrap(), teams_core::Availability::Busy);
}
