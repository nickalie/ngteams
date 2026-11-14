//! Engine messages and typed page reads against the persistent chat service.

use teams_core::engine::{Action, EngineHandle};
use teams_proto::Mri;
use teams_proto::chat::{ChatReads, MessageText};
use teams_store::OutboxState;
use tempfile::TempDir;

use super::{client, engine};
use crate::support::{Backend, TestServer, backends};

pub(super) struct Pair {
    pub server: TestServer,
    pub a_id: String,
    pub b_id: String,
    pub chat: String,
    pub a: EngineHandle,
    pub b: EngineHandle,
    _dirs: [TempDir; 2],
}

impl Pair {
    pub async fn start(backend: Backend) -> Self {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a_id = server.add_user("a@own.test", "Ada").await;
        let b_id = server.add_user("b@own.test", "Bert").await;
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let a = engine(&server, "a@own.test", &a_id, dirs[0].path()).await;
        let b = engine(&server, "b@own.test", &b_id, dirs[1].path()).await;
        let row = a
            .write(Action::CreateChat {
                others: vec![Mri::new(format!("8:orgid:{b_id}"))],
                topic: None,
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent);
        let chat = row.message_id.unwrap();
        assert!(b.sync().await.unwrap().failed.is_empty());
        Self {
            server,
            a_id,
            b_id,
            chat,
            a,
            b,
            _dirs: dirs,
        }
    }

    pub async fn send(&self, text: &str) -> String {
        let row = self
            .a
            .write(Action::Send {
                conversation: self.chat.clone(),
                text: MessageText::plain(text).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row.message_id.unwrap()
    }

    pub async fn stop(self) {
        self.a.shutdown();
        self.b.shutdown();
        self.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_posts_messages_and_another_engine_syncs_them_with_backward_and_incremental_pages() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let first = pair.send("first persistent message").await;
        let second = pair.send("second persistent message").await;
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let reads = ChatReads::new(&reader);
        let page = reads.messages(&pair.chat, 1).await.unwrap();
        assert_eq!(page.messages.len(), 1);
        assert_eq!(page.messages[0].id, second);
        let older = reads
            .messages_from(&pair.chat, page.backward_link().unwrap())
            .await
            .unwrap();
        assert_eq!(older.messages.len(), 1);
        assert_eq!(older.messages[0].id, first);
        assert_eq!(older.sync_state(), page.sync_state());
        let third = pair.send("third persistent message").await;
        let next = reads
            .messages_from(&pair.chat, page.sync_state().unwrap())
            .await
            .unwrap();
        assert_eq!(next.messages.len(), 1);
        assert_eq!(next.messages[0].id, third);
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let message = pair.b.message(&pair.chat, &third).await.unwrap().unwrap();
        assert!(message.content.contains("third persistent message"));
        assert_eq!(
            message.sender.as_deref(),
            Some(format!("8:orgid:{}", pair.a_id).as_str())
        );
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_around_time_reads_choose_messages_on_both_sides_of_the_requested_time() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let first = pair.send("earliest").await;
        let middle = pair.send("middle").await;
        let last = pair.send("latest").await;
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let reads = ChatReads::new(&reader);
        let page = reads
            .messages_around(&pair.chat, middle.parse().unwrap(), 3)
            .await
            .unwrap();
        assert_eq!(
            page.messages.iter().map(|message| &message.id).collect::<Vec<_>>(),
            vec![&last, &middle, &first]
        );
        let early = reads.messages_around(&pair.chat, 0, 2).await.unwrap();
        assert_eq!(
            early.messages.iter().map(|message| &message.id).collect::<Vec<_>>(),
            vec![&middle, &first]
        );
        let late = reads
            .messages_around(&pair.chat, last.parse::<u64>().unwrap() + 1000, 2)
            .await
            .unwrap();
        assert_eq!(
            late.messages.iter().map(|message| &message.id).collect::<Vec<_>>(),
            vec![&last, &middle]
        );
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn incremental_message_pages_are_newest_first_and_keep_concurrent_posts_for_the_next_sync() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let reads = ChatReads::new(&reader);
        let initial = reads.messages(&pair.chat, 200).await.unwrap();
        let first = pair.send("one").await;
        let second = pair.send("two").await;
        let third = pair.send("three").await;
        let edited = pair
            .a
            .write(Action::Edit {
                conversation: pair.chat.clone(),
                message_id: first.clone(),
                text: MessageText::plain("one, edited last").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(edited.state, OutboxState::Sent);
        let mut page = reads
            .messages_from(&pair.chat, &format!("{}&pageSize=1", initial.sync_state().unwrap()))
            .await
            .unwrap();
        assert_eq!(page.messages[0].id, third);
        let next_sync = page.sync_state().unwrap().to_owned();
        let fourth = pair.send("four, between pages").await;
        let mut seen = Vec::new();
        loop {
            assert_eq!(page.sync_state(), Some(next_sync.as_str()));
            seen.extend(page.messages.iter().map(|message| message.id.clone()));
            let Some(next) = page.backward_link() else {
                break;
            };
            page = reads.messages_from(&pair.chat, next).await.unwrap();
        }
        assert_eq!(seen, vec![third, second, first]);
        let following = reads.messages_from(&pair.chat, &next_sync).await.unwrap();
        assert_eq!(following.messages.len(), 1);
        assert_eq!(following.messages[0].id, fourth);
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_catches_up_every_message_across_a_cut_incremental_answer() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let mut ids = Vec::new();
        for index in 0..205 {
            ids.push(pair.send(&format!("offline message {index}")).await);
        }
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        for id in ids {
            assert!(pair.b.message(&pair.chat, &id).await.unwrap().is_some(), "missing {id}");
        }
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn large_message_pages_stay_within_the_native_transport_budget_and_keep_all_rows() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let mut ids = Vec::new();
        let text = "x".repeat(50 * 1024);
        for _ in 0..90 {
            ids.push(pair.send(&text).await);
        }
        let reader = client(&pair.server, "b@own.test", &pair.b_id).await;
        let reads = ChatReads::new(&reader);
        let around = reads
            .messages_around(&pair.chat, ids.last().unwrap().parse().unwrap(), 200)
            .await
            .unwrap();
        assert!(around.messages.len() < ids.len());
        assert!(around.backward_link().is_some());
        let mut page = reads.messages(&pair.chat, 200).await.unwrap();
        assert!(page.messages.len() < ids.len());
        let mut seen = Vec::new();
        loop {
            seen.extend(page.messages.iter().map(|message| message.id.clone()));
            let Some(next) = page.backward_link() else { break };
            page = reads.messages_from(&pair.chat, next).await.unwrap();
        }
        ids.reverse();
        assert_eq!(seen, ids);
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        for id in &ids {
            assert!(pair.b.message(&pair.chat, id).await.unwrap().is_some());
        }
        pair.stop().await;
    }
}
