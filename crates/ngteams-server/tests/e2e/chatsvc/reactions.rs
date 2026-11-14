//! Reactions are per actor and merge into the stored message, including another member's message.

use teams_core::engine::Action;
use teams_store::OutboxState;

use super::messages::Pair;
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn engines_add_and_remove_their_own_reactions_without_removing_another_members() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let id = pair.send("react to this").await;
        pair.a.sync().await.unwrap();
        pair.b.sync().await.unwrap();
        let like = pair
            .b
            .write(Action::React {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: "like".into(),
            })
            .await
            .unwrap();
        assert_eq!(like.state, OutboxState::Sent, "{like:?}");
        pair.a.sync().await.unwrap();
        let reactions = pair.a.reactions(&pair.chat, &id).await.unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].user, format!("8:orgid:{}", pair.b_id));
        let heart = pair
            .a
            .write(Action::React {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: "heart".into(),
            })
            .await
            .unwrap();
        assert_eq!(heart.state, OutboxState::Sent, "{heart:?}");
        pair.b.sync().await.unwrap();
        assert_eq!(pair.b.reactions(&pair.chat, &id).await.unwrap().len(), 2);
        let unliked = pair
            .b
            .write(Action::Unreact {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: "like".into(),
            })
            .await
            .unwrap();
        assert_eq!(unliked.state, OutboxState::Sent, "{unliked:?}");
        pair.a.sync().await.unwrap();
        let remaining = pair.a.reactions(&pair.chat, &id).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key, "heart");
        assert_eq!(remaining[0].user, format!("8:orgid:{}", pair.a_id));
        let unhearted = pair
            .a
            .write(Action::Unreact {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: "heart".into(),
            })
            .await
            .unwrap();
        assert_eq!(unhearted.state, OutboxState::Sent, "{unhearted:?}");
        pair.b.sync().await.unwrap();
        assert!(pair.b.reactions(&pair.chat, &id).await.unwrap().is_empty());
        pair.stop().await;
    }
}
