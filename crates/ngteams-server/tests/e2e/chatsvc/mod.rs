//! The real own-cloud engine and typed reads against persisted local chat service accounts.

mod blocked;
mod contract;
mod messages;
mod mutations;
mod noops;
mod paging;
mod properties;
mod reactions;
mod security;
mod typing;

use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_proto::chat::ChatReads;
use teams_proto::{Author, Client, Cloud, HttpTransport, Mri};
use teams_store::{OutboxState, Store};

use crate::support::{TestServer, backends};

type ChatClient = Client<HttpTransport<Session<MemorySecretStore>>>;

async fn transport(server: &TestServer, upn: &str, id: &str) -> HttpTransport<Session<MemorySecretStore>> {
    transport_with(Arc::new(server.session(upn).await), server, id)
}

fn transport_with(
    session: Arc<Session<MemorySecretStore>>,
    server: &TestServer,
    id: &str,
) -> HttpTransport<Session<MemorySecretStore>> {
    let cloud = Cloud::own("https://own.example.test").unwrap();
    HttpTransport::for_cloud_with_mock_origin(
        session,
        "https://own.example.test/api/chatsvc/own",
        cloud,
        &server.origin,
    )
    .unwrap()
    .for_account(Mri::new(format!("8:orgid:{id}")))
}

async fn client(server: &TestServer, upn: &str, id: &str) -> ChatClient {
    Client::new(transport(server, upn, id).await)
}

async fn engine(server: &TestServer, upn: &str, id: &str, dir: &std::path::Path) -> EngineHandle {
    let session = Arc::new(server.session(upn).await);
    let client = Arc::new(Client::new(transport_with(Arc::clone(&session), server, id)));
    let transport = Arc::new(transport_with(session, server, id));
    engine::start(Parts {
        store: Store::open(&dir.join("account.sqlite")).unwrap(),
        client,
        outbox: Outbox::new(transport, Author::new(Mri::new(format!("8:orgid:{id}")), upn)),
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

#[tokio::test(flavor = "multi_thread")]
async fn engines_create_unique_and_named_chats_and_another_member_syncs_them() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let b = server.add_user("b@own.test", "Bert").await;
        let c = server.add_user("c@own.test", "Cora").await;
        let dir = tempfile::tempdir().unwrap();
        let handle = engine(&server, "a@own.test", &a, dir.path()).await;
        let create = || Action::CreateChat {
            others: vec![Mri::new(format!("8:orgid:{b}"))],
            topic: None,
        };
        let first = handle.write(create()).await.unwrap();
        assert_eq!(first.state, OutboxState::Sent, "{first:?}");
        let chat = first.message_id.unwrap();
        let again = handle.write(create()).await.unwrap();
        assert_eq!(again.message_id.as_deref(), Some(chat.as_str()));
        let group = handle
            .write(Action::CreateChat {
                others: vec![Mri::new(format!("8:orgid:{b}")), Mri::new(format!("8:orgid:{c}"))],
                topic: Some("Project room".into()),
            })
            .await
            .unwrap();
        assert_eq!(group.state, OutboxState::Sent, "{group:?}");
        let group = group.message_id.unwrap();
        let reader = client(&server, "b@own.test", &b).await;
        let reads = ChatReads::new(&reader);
        let thread = reads.thread(&group).await.unwrap();
        assert_eq!(thread.members.len(), 3);
        assert_eq!(reads.members(&group).await.unwrap().members.len(), 3);
        let list = reads.conversations().await.unwrap();
        assert!(list.conversations.iter().any(|entry| entry.id == group));
        assert!(list.conversations.iter().any(|entry| entry.id == chat));
        assert!(
            reads
                .conversations_from(list.sync_state().unwrap())
                .await
                .unwrap()
                .conversations
                .is_empty()
        );
        let outsider = client(&server, "c@own.test", &c).await;
        assert_eq!(
            ChatReads::new(&outsider).thread(&chat).await.unwrap_err().status(),
            Some(403)
        );
        let bdir = tempfile::tempdir().unwrap();
        let second = engine(&server, "b@own.test", &b, bdir.path()).await;
        assert!(second.sync().await.unwrap().failed.is_empty());
        assert!(
            second
                .conversations(100)
                .await
                .unwrap()
                .iter()
                .any(|entry| entry.id == group)
        );
        handle.shutdown();
        second.shutdown();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn created_group_thread_and_member_list_match_the_synthetic_golden_shapes() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let b = server.add_user("b@own.test", "Bert").await;
        let c = server.add_user("c@own.test", "Cora").await;
        let mut events = server.app.fanout.subscribe(&format!("push:8:orgid:{b}")).await.unwrap();
        let client = client(&server, "a@own.test", &a).await;
        let request = teams_proto::CreateChat::group(
            Mri::new(format!("8:orgid:{a}")),
            [Mri::new(format!("8:orgid:{b}")), Mri::new(format!("8:orgid:{c}"))],
        )
        .unwrap();
        let response = client.write(request).await.unwrap();
        assert_eq!(response.status(), 201);
        let body: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        let thread = &body["threadResource"];
        let group = thread["id"]
            .as_str()
            .unwrap()
            .trim_start_matches("19:")
            .trim_end_matches("@thread.v2");
        let created = thread["properties"]["createdat"].as_str().unwrap();
        let tenant = ngteams_server::db::meta::tenant(&server.app.db).await.unwrap();
        let replacements = [
            ("<guid:a>", a.as_str()),
            ("<guid:b>", b.as_str()),
            ("<guid:c>", c.as_str()),
            ("<guid:tenant>", tenant.as_str()),
            ("<group>", group),
            (
                "https://<region>.example.test",
                "https://own.example.test/api/chatsvc/own",
            ),
            ("1760000000000", created),
        ];
        let golden = |source: &str| {
            let mut source = source.to_owned();
            for (from, to) in replacements {
                source = source.replace(from, to);
            }
            serde_json::from_str::<serde_json::Value>(&source).unwrap()["response"]["body"].clone()
        };
        assert_eq!(
            *thread,
            golden(include_str!("../../../../../tests/fixtures/chatsvc/thread-group.json"))
        );
        let members = ChatReads::new(&client)
            .members(thread["id"].as_str().unwrap())
            .await
            .unwrap();
        let parsed: teams_proto::chat::MemberList = serde_json::from_value(golden(include_str!(
            "../../../../../tests/fixtures/chatsvc/thread-group-members.json"
        )))
        .unwrap();
        assert_eq!(members, parsed);
        let event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: serde_json::Value = serde_json::from_slice(&event).unwrap();
        assert_eq!(event["resource"], *thread);
        assert_eq!(event["resourceType"], "ThreadUpdate");
        server.stop().await;
    }
}
