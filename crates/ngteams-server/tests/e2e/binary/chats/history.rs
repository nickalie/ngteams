use std::collections::HashSet;

use teams_core::engine::Action;
use teams_proto::chat::{ChatReads, MessageText};

use super::support::{Actor, Rig};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hundreds_of_messages_page_completely_and_old_mutations_reach_incremental_sync() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let [a, b, _] = &rig.actors;
        let mut ids = Vec::new();
        let large = "x".repeat(50 * 1024);
        for index in 0..300 {
            let text = if index < 90 {
                format!("large {index} {large}")
            } else {
                format!("small {index}")
            };
            ids.push(a.send(&rig.chat, &text).await);
        }
        assert_backward_pages(b, &rig.chat, &ids).await;
        b.sync().await;
        for _ in 0..10 {
            if b.engine().history_complete(&rig.chat).await.unwrap() == Some(true) {
                break;
            }
            assert!(b.engine().load_older(&rig.chat).await.unwrap().is_some());
        }
        assert_eq!(b.engine().history_complete(&rig.chat).await.unwrap(), Some(true));
        for id in &ids {
            assert!(b.engine().message(&rig.chat, id).await.unwrap().is_some());
        }
        a.sync().await;
        for index in 0..205 {
            a.send(&rig.chat, &format!("offline {index}")).await;
        }
        mutate_old_messages(a, &rig.chat, &ids).await;
        b.sync().await;
        for id in ids.iter().take(12) {
            b.assert_message(&rig.chat, id, "old message edited offline").await;
        }
        let tombstone = b.engine().message(&rig.chat, &ids[12]).await.unwrap().unwrap();
        assert!(tombstone.delete_time.is_some());
        assert!(tombstone.content.is_empty());
        let reactions = b.engine().reactions(&rig.chat, &ids[13]).await.unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].user, a.mri.as_str());
        assert_eq!(
            b.engine().messages(&rig.chat, None, 1000).await.unwrap().messages.len(),
            505
        );
        rig.stop().await;
    }
}

async fn assert_backward_pages(actor: &Actor, chat: &str, expected: &[String]) {
    let reads = ChatReads::new(actor.client.as_ref());
    let mut page = reads.messages(chat, 200).await.unwrap();
    let watermark = page.sync_state().unwrap().to_owned();
    let mut seen = Vec::new();
    let mut links = HashSet::new();
    loop {
        assert_eq!(page.sync_state(), Some(watermark.as_str()));
        seen.extend(page.messages.into_iter().map(|message| message.id));
        let Some(next) = page.metadata.get("backwardLink").and_then(serde_json::Value::as_str) else {
            break;
        };
        assert!(links.insert(next.to_owned()), "backward page loop");
        page = reads.messages_from(chat, next).await.unwrap();
    }
    assert!(links.len() >= 2, "history must cross both byte and count page cuts");
    assert_eq!(seen, expected.iter().rev().cloned().collect::<Vec<_>>());
}

async fn mutate_old_messages(actor: &Actor, chat: &str, ids: &[String]) {
    for id in ids.iter().take(12) {
        actor
            .write(Action::Edit {
                conversation: chat.into(),
                message_id: id.clone(),
                text: MessageText::plain("old message edited offline").unwrap(),
            })
            .await;
    }
    actor
        .write(Action::Delete {
            conversation: chat.into(),
            message_id: ids[12].clone(),
        })
        .await;
    actor
        .write(Action::React {
            conversation: chat.into(),
            message_id: ids[13].clone(),
            key: "like".into(),
        })
        .await;
}
