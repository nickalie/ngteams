//! The engine's mutations retain message identity and reach the other member through sync and fan-out.

use std::time::Duration;

use teams_core::engine::Action;
use teams_proto::chat::MessageText;
use teams_store::OutboxState;

use super::messages::Pair;
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_edits_its_message_and_the_other_member_syncs_the_same_identity_and_arrival() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let id = pair.send("before the edit").await;
        assert!(pair.a.sync().await.unwrap().failed.is_empty());
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let before = pair.b.message(&pair.chat, &id).await.unwrap().unwrap();
        let mut events = pair
            .server
            .app
            .fanout
            .subscribe(&format!("push:8:orgid:{}", pair.b_id))
            .await
            .unwrap();
        let row = pair
            .a
            .write(Action::Edit {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                text: MessageText::plain("after the edit").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let after = pair.b.message(&pair.chat, &id).await.unwrap().unwrap();
        assert!(after.content.contains("after the edit"));
        assert!(after.version > before.version);
        assert_eq!(after.arrival_time, before.arrival_time);
        assert_eq!(after.client_message_id, before.client_message_id);
        assert!(after.edit_time.is_some());
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_slice(&event).unwrap();
        assert_eq!(event["resourceType"], "MessageUpdate");
        assert_eq!(event["resource"]["id"], id);
        pair.stop().await;
    }
}
