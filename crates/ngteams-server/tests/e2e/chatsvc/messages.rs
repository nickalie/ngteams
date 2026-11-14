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
        assert_eq!(message.sender.as_deref(), Some(format!("8:orgid:{}", pair.a_id).as_str()));
        pair.stop().await;
    }
}
