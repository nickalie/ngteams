//! Account and conversation state is owned by the authenticated account and survives client catch-up.

use teams_core::engine::Action;
use teams_proto::chat::ChatReads;
use teams_store::OutboxState;

use super::{client, messages::Pair};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_reads_a_message_and_its_own_horizon_syncs_without_becoming_the_other_members() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let id = pair.send("read this message").await;
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let row = pair
            .b
            .write(Action::Read {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let horizon = pair.b.read_horizon(&pair.chat).await.unwrap().unwrap();
        assert_eq!(horizon.last_read_arrival.to_string(), id);
        let b_client = client(&pair.server, "b@own.test", &pair.b_id).await;
        let list = ChatReads::new(&b_client).conversations().await.unwrap();
        let own = list.conversations.iter().find(|chat| chat.id == pair.chat).unwrap();
        assert_eq!(
            own.consumption_horizon().unwrap().last_read_arrival,
            Some(id.parse().unwrap())
        );
        let a_client = client(&pair.server, "a@own.test", &pair.a_id).await;
        let list = ChatReads::new(&a_client).conversations().await.unwrap();
        assert!(
            list.conversations
                .iter()
                .find(|chat| chat.id == pair.chat)
                .unwrap()
                .consumption_horizon()
                .is_none()
        );
        pair.stop().await;
    }
}
