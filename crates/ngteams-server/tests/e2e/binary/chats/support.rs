use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri};
use teams_store::{OutboxState, Store};
use tempfile::TempDir;

use super::super::support::{Deployment, Node};
use crate::support::{Backend, PASSWORD, redis_url};

pub type ChatClient = Client<HttpTransport<Session<MemorySecretStore>>>;

pub struct Actor {
    pub mri: Mri,
    pub client: Arc<ChatClient>,
    engine: Option<EngineHandle>,
    session: Arc<Session<MemorySecretStore>>,
    origin: String,
    endpoint: String,
    dir: TempDir,
}

impl Actor {
    pub async fn sign_in(node: &Node, origin: &str, upn: &str) -> Self {
        let session = Arc::new(node.sign_in(upn, PASSWORD).await);
        let skype = session.skypetoken().await.unwrap();
        let mri = Mri::new(format!("8:{}", skype.skype_id.as_deref().unwrap()));
        assert_eq!(
            session.service_base("chatServiceAfd"),
            Some(format!("{origin}/api/chatsvc/own"))
        );
        let client = Arc::new(Client::new(transport(&session, origin, &node.endpoint, &mri)));
        let dir = tempfile::tempdir().unwrap();
        let engine = start_engine(&session, &client, origin, &node.endpoint, &mri, dir.path());
        Self {
            mri,
            client,
            engine: Some(engine),
            session,
            origin: origin.into(),
            endpoint: node.endpoint.clone(),
            dir,
        }
    }

    pub fn engine(&self) -> &EngineHandle {
        self.engine.as_ref().unwrap()
    }

    pub async fn sync(&self) {
        let report = self.engine().sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
    }

    pub fn restart(&mut self) {
        self.engine.take().unwrap().shutdown();
        self.engine = Some(start_engine(
            &self.session,
            &self.client,
            &self.origin,
            &self.endpoint,
            &self.mri,
            self.dir.path(),
        ));
    }

    pub async fn write(&self, action: Action) -> teams_store::OutboxEntry {
        let row = self.engine().write(action).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row
    }

    pub async fn send(&self, chat: &str, text: &str) -> String {
        self.write(Action::Send {
            conversation: chat.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .message_id
        .unwrap()
    }

    pub async fn create(&self, others: &[&Self], topic: Option<&str>) -> String {
        self.write(Action::CreateChat {
            others: others.iter().map(|actor| actor.mri.clone()).collect(),
            topic: topic.map(str::to_owned),
        })
        .await
        .message_id
        .unwrap()
    }

    pub async fn assert_message(&self, chat: &str, id: &str, text: &str) {
        let message = self.engine().message(chat, id).await.unwrap().unwrap();
        assert!(message.content.contains(text));
    }

    pub async fn properties(&self, chat: &str) -> serde_json::Map<String, serde_json::Value> {
        ChatReads::new(self.client.as_ref())
            .conversations()
            .await
            .unwrap()
            .conversations
            .into_iter()
            .find(|row| row.id == chat)
            .unwrap()
            .properties
    }
}

impl Drop for Actor {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
    }
}

fn transport(
    session: &Arc<Session<MemorySecretStore>>,
    origin: &str,
    endpoint: &str,
    mri: &Mri,
) -> HttpTransport<Session<MemorySecretStore>> {
    let cloud = Cloud::own(origin).unwrap();
    HttpTransport::for_cloud_with_mock_origin(
        Arc::clone(session),
        &format!("{origin}/api/chatsvc/own"),
        cloud,
        endpoint,
    )
    .unwrap()
    .for_account(mri.clone())
}

fn start_engine(
    session: &Arc<Session<MemorySecretStore>>,
    client: &Arc<ChatClient>,
    origin: &str,
    endpoint: &str,
    mri: &Mri,
    dir: &Path,
) -> EngineHandle {
    engine::start(Parts {
        store: Store::open(&dir.join("account.sqlite")).unwrap(),
        client: Arc::clone(client),
        outbox: Outbox::new(
            Arc::new(transport(session, origin, endpoint, mri)),
            Author::new(mri.clone(), "QA Operator"),
        ),
        calls: None,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(20),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

pub struct Rig {
    pub actors: [Actor; 3],
    pub chat: String,
    pub first: Node,
    pub second: Option<Node>,
    _deployment: Deployment,
}

impl Rig {
    pub async fn start(backend: Backend) -> Self {
        eprintln!("real-binary chats: {backend:?}");
        let redis = (backend == Backend::Postgres).then(redis_url).flatten();
        assert!(
            backend != Backend::Postgres || redis.is_some(),
            "Postgres QA requires Redis"
        );
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let mut first = deployment.node("a");
        first.cli(&["migrate"], "");
        for upn in ["a@own.test", "b@own.test", "c@own.test"] {
            first.add_user(upn, PASSWORD);
        }
        first.start().await;
        let mut second = (backend == Backend::Postgres).then(|| deployment.node("b"));
        if let Some(node) = &mut second {
            node.start().await;
        }
        let a = Actor::sign_in(&first, &deployment.origin, "a@own.test").await;
        let b = Actor::sign_in(second.as_ref().unwrap_or(&first), &deployment.origin, "b@own.test").await;
        let c = Actor::sign_in(&first, &deployment.origin, "c@own.test").await;
        let chat = a.create(&[&b], None).await;
        Self {
            actors: [a, b, c],
            chat,
            first,
            second,
            _deployment: deployment,
        }
    }

    pub async fn stop(mut self) {
        for actor in &mut self.actors {
            actor.engine.take().unwrap().shutdown();
        }
        if let Some(node) = &mut self.second {
            node.stop().await;
        }
        self.first.stop().await;
    }
}
