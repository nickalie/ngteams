//! Typing is a transient engine write and a peer event, with no stored message or conversation change.

use std::time::Duration;

use teams_core::{Transient, TransientSent};
use teams_proto::chat::ChatReads;

use super::{client, messages::Pair};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn engine_typing_reaches_the_other_member_without_entering_the_message_history() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        pair.a.sync().await.unwrap();
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let before = ChatReads::new(&reader).conversations().await.unwrap();
        let mut events = pair
            .server
            .app
            .fanout
            .subscribe(&format!("push:8:orgid:{}", pair.b_id))
            .await
            .unwrap();
        let sent = pair
            .a
            .transient(Transient::Typing {
                conversation: pair.chat.clone(),
            })
            .await
            .unwrap();
        assert!(matches!(sent, TransientSent::Sent), "{sent:?}");
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_slice(&event).unwrap();
        assert_eq!(event["resourceType"], "NewMessage");
        assert_eq!(event["resource"]["messagetype"], "Control/Typing");
        assert_eq!(event["resource"]["conversationid"], pair.chat);
        assert!(
            ChatReads::new(&reader)
                .messages(&pair.chat, 200)
                .await
                .unwrap()
                .messages
                .is_empty()
        );
        let after = ChatReads::new(&reader).conversations().await.unwrap();
        assert_eq!(after.conversations[0].version, before.conversations[0].version);
        pair.stop().await;
    }
}
