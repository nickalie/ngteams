//! Synthetic users, real operator CLI and server processes, native sessions and persisted client engines.

mod history;
mod properties;
mod support;

use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{Author, DeleteMessage, EditMessage, Original, SendMessage};

use self::support::Rig;
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_exchange_messages_and_a_third_member_syncs_a_named_group() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let [a, b, c] = &rig.actors;
        let first = a.send(&rig.chat, "hello from a").await;
        b.sync().await;
        assert!(b.engine().conversation(rig.chat.clone()).await.unwrap().is_some());
        b.assert_message(&rig.chat, &first, "hello from a").await;
        let reply = b.send(&rig.chat, "reply from b").await;
        a.sync().await;
        a.assert_message(&rig.chat, &reply, "reply from b").await;
        let group = a.create(&[b, c], Some("QA named room")).await;
        let post = a.send(&group, "hello group").await;
        c.sync().await;
        c.assert_message(&group, &post, "hello group").await;
        let stored = c.engine().conversation(group.clone()).await.unwrap().unwrap();
        assert_eq!(stored.topic.as_deref(), Some("QA named room"));
        assert_eq!(
            ChatReads::new(c.client.as_ref())
                .members(&group)
                .await
                .unwrap()
                .members
                .len(),
            3
        );
        assert_eq!(a.create(&[b], None).await, rig.chat);
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_edits_and_deletes_sync_to_peer_and_peer_cannot_edit_the_message() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let [a, b, _] = &rig.actors;
        let id = a.send(&rig.chat, "original").await;
        a.sync().await;
        b.sync().await;
        let before = b.engine().message(&rig.chat, &id).await.unwrap().unwrap();
        assert!(
            b.engine()
                .write(Action::Edit {
                    conversation: rig.chat.clone(),
                    message_id: id.clone(),
                    text: MessageText::plain("forbidden").unwrap(),
                })
                .await
                .is_err()
        );
        let forged = EditMessage::new(
            &rig.chat,
            &id,
            Author::new(b.mri.clone(), "QA Operator"),
            MessageText::plain("forbidden").unwrap(),
            Original {
                client_message_id: before.client_message_id.clone().unwrap(),
                compose_time_ms: before.compose_time.unwrap(),
                arrival_time_ms: before.arrival_time,
            },
        );
        assert_eq!(b.client.write(forged).await.unwrap().status(), 403);
        a.write(Action::Edit {
            conversation: rig.chat.clone(),
            message_id: id.clone(),
            text: MessageText::plain("edited").unwrap(),
        })
        .await;
        b.sync().await;
        b.assert_message(&rig.chat, &id, "edited").await;
        let edited = b.engine().message(&rig.chat, &id).await.unwrap().unwrap();
        assert_eq!(edited.arrival_time, before.arrival_time);
        assert_eq!(edited.client_message_id, before.client_message_id);
        assert!(edited.edit_time.is_some());
        a.write(Action::Delete {
            conversation: rig.chat.clone(),
            message_id: id.clone(),
        })
        .await;
        b.sync().await;
        let deleted = b.engine().message(&rig.chat, &id).await.unwrap().unwrap();
        assert!(deleted.content.is_empty());
        assert!(deleted.delete_time.is_some());
        assert_eq!(deleted.arrival_time, before.arrival_time);
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn taking_back_a_reaction_preserves_the_other_members_same_reaction() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let [a, b, _] = &rig.actors;
        let id = a.send(&rig.chat, "both like this").await;
        a.sync().await;
        b.sync().await;
        for actor in [a, b] {
            actor
                .write(Action::React {
                    conversation: rig.chat.clone(),
                    message_id: id.clone(),
                    key: "like".into(),
                })
                .await;
        }
        b.sync().await;
        assert_eq!(b.engine().reactions(&rig.chat, &id).await.unwrap().len(), 2);
        a.write(Action::Unreact {
            conversation: rig.chat.clone(),
            message_id: id.clone(),
            key: "like".into(),
        })
        .await;
        for actor in [a, b] {
            actor.sync().await;
            let remaining = actor.engine().reactions(&rig.chat, &id).await.unwrap();
            assert_eq!(remaining.len(), 1);
            assert_eq!(remaining[0].key, "like");
            assert_eq!(remaining[0].user, b.mri.as_str());
        }
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outsider_cannot_read_or_write_a_private_chat() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let [a, _, c] = &rig.actors;
        let id = a.send(&rig.chat, "private").await;
        c.sync().await;
        assert!(c.engine().conversation(rig.chat.clone()).await.unwrap().is_none());
        let reads = ChatReads::new(c.client.as_ref());
        assert_eq!(reads.thread(&rig.chat).await.unwrap_err().status(), Some(403));
        assert_eq!(reads.members(&rig.chat).await.unwrap_err().status(), Some(403));
        assert_eq!(reads.messages(&rig.chat, 10).await.unwrap_err().status(), Some(403));
        let send = SendMessage::new(
            &rig.chat,
            Author::new(c.mri.clone(), "QA Operator"),
            MessageText::plain("intrusion").unwrap(),
        );
        assert_eq!(c.client.write(send).await.unwrap().status(), 403);
        assert_eq!(
            c.client
                .write(DeleteMessage::new(&rig.chat, &id))
                .await
                .unwrap()
                .status(),
            403
        );
        a.sync().await;
        a.assert_message(&rig.chat, &id, "private").await;
        assert_eq!(
            ChatReads::new(a.client.as_ref())
                .messages(&rig.chat, 10)
                .await
                .unwrap()
                .messages
                .len(),
            1
        );
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_sync_and_node_restart_preserve_chat_history() {
    for backend in backends() {
        let mut rig = Rig::start(backend).await;
        let first = rig.actors[0].send(&rig.chat, "before node restart").await;
        rig.actors[1].sync().await;
        rig.actors[1]
            .assert_message(&rig.chat, &first, "before node restart")
            .await;
        rig.first.stop().await;
        if rig.second.is_some() {
            let during = rig.actors[1].send(&rig.chat, "while node a is down").await;
            rig.first.start().await;
            rig.actors[0].sync().await;
            rig.actors[0]
                .assert_message(&rig.chat, &during, "while node a is down")
                .await;
        } else {
            rig.first.start().await;
        }
        let persisted = ChatReads::new(rig.actors[0].client.as_ref())
            .messages(&rig.chat, 10)
            .await
            .unwrap();
        assert!(persisted.messages.iter().any(|message| message.id == first));
        rig.actors[1].restart();
        rig.actors[1].sync().await;
        rig.actors[1]
            .assert_message(&rig.chat, &first, "before node restart")
            .await;
        let after = rig.actors[0].send(&rig.chat, "after node restart").await;
        rig.actors[1].sync().await;
        rig.actors[1]
            .assert_message(&rig.chat, &after, "after node restart")
            .await;
        rig.stop().await;
    }
}
