//! Wire-shape and malformed-input regressions use the client's own transport and engine.

use serde_json::{Value, json};
use teams_proto::chat::ChatReads;
use teams_proto::{Endpoint, Method, Request, Service};

use super::{client, messages::Pair};
use crate::support::Backend;

#[tokio::test(flavor = "multi_thread")]
async fn message_previews_follow_the_native_contract_without_full_message_properties() {
    let pair = Pair::start(Backend::Sqlite).await;
    pair.send("the list preview").await;
    let client = client(&pair.server, "b@own.test", &pair.b_id).await;
    let listed = ChatReads::new(&client).conversations().await.unwrap();
    let preview = listed
        .conversations
        .iter()
        .find(|chat| chat.id == pair.chat)
        .unwrap()
        .last_message_raw
        .as_ref()
        .unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../../../../../tests/fixtures/chatsvc/conversations.json")).unwrap();
    let golden = &fixture["response"]["body"]["conversations"][0]["lastMessage"];
    assert_eq!(preview["type"], golden["type"]);
    assert_eq!(preview["messagetype"], golden["messagetype"]);
    assert!(golden.get("properties").is_none());
    assert!(preview.get("properties").is_none());
    pair.stop().await;
}

struct MalformedEdit {
    chat: String,
    id: String,
    body: Value,
}
impl Request for MalformedEdit {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Put
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", &self.chat, "messages", &self.id])
    }
    fn body(&self) -> Option<Value> {
        Some(self.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_edits_get_a_native_contract_bad_request_and_leave_the_message_unchanged() {
    let pair = Pair::start(Backend::Sqlite).await;
    let id = pair.send("unchanged after rejected edits").await;
    let client = client(&pair.server, "a@own.test", &pair.a_id).await;
    for body in [json!([]), json!(false), json!(42), json!("bad"), Value::Null] {
        let response = client
            .write(MalformedEdit {
                chat: pair.chat.clone(),
                id: id.clone(),
                body,
            })
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    pair.a.sync().await.unwrap();
    assert!(
        pair.a
            .message(&pair.chat, &id)
            .await
            .unwrap()
            .unwrap()
            .content
            .contains("unchanged after rejected edits")
    );
    pair.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_and_reacting_to_older_messages_keeps_the_latest_arrival_preview() {
    for backend in crate::support::backends() {
        let pair = Pair::start(backend).await;
        let first = pair.send("old arrival").await;
        let last = pair.send("latest arrival").await;
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        for action in [
            teams_core::engine::Action::Edit {
                conversation: pair.chat.clone(),
                message_id: first.clone(),
                text: teams_proto::chat::MessageText::plain("old arrival edited").unwrap(),
            },
            teams_core::engine::Action::React {
                conversation: pair.chat.clone(),
                message_id: first.clone(),
                key: "like".into(),
            },
        ] {
            let row = pair.a.write(action).await.unwrap();
            assert_eq!(row.state, teams_store::OutboxState::Sent);
            let listed = ChatReads::new(&reader).conversations().await.unwrap();
            let entry = listed.conversations.iter().find(|chat| chat.id == pair.chat).unwrap();
            assert_eq!(entry.last_message_raw.as_ref().unwrap()["id"], last);
            assert_eq!(
                entry.last_updated_message_id,
                teams_proto::chat::Field::Known(first.parse().unwrap())
            );
        }
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn message_changes_reach_the_acting_accounts_other_endpoints() {
    for backend in crate::support::backends() {
        let pair = Pair::start(backend).await;
        let mut events = pair
            .server
            .app
            .fanout
            .subscribe(&format!("push:8:orgid:{}", pair.a_id))
            .await
            .unwrap();
        let id = pair.send("from another endpoint").await;
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: Value = serde_json::from_slice(&event).unwrap();
        assert_eq!(event["resourceType"], "NewMessage");
        assert_eq!(event["resource"]["id"], id);
        for action in [
            teams_core::engine::Action::Edit {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                text: teams_proto::chat::MessageText::plain("updated on another endpoint").unwrap(),
            },
            teams_core::engine::Action::React {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: "like".into(),
            },
            teams_core::engine::Action::Delete {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
            },
        ] {
            let row = pair.a.write(action).await.unwrap();
            assert_eq!(row.state, teams_store::OutboxState::Sent);
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            let event: Value = serde_json::from_slice(&event).unwrap();
            assert_eq!(event["resourceType"], "MessageUpdate");
            assert_eq!(event["resource"]["id"], id);
        }
        let c_id = pair.server.add_user("c@own.test", "Cora").await;
        let group = pair
            .a
            .write(teams_core::engine::Action::CreateChat {
                others: vec![
                    teams_proto::Mri::new(format!("8:orgid:{}", pair.b_id)),
                    teams_proto::Mri::new(format!("8:orgid:{c_id}")),
                ],
                topic: Some("Another endpoint's room".into()),
            })
            .await
            .unwrap();
        assert_eq!(group.state, teams_store::OutboxState::Sent);
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: Value = serde_json::from_slice(&event).unwrap();
        assert_eq!(event["resourceType"], "ThreadUpdate");
        assert_eq!(event["resource"]["id"], group.message_id.unwrap());
        pair.stop().await;
    }
}
