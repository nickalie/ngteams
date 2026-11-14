//! A bounded conversation scan leaves concurrent writes for the following sync, on either database.

use teams_proto::chat::ChatReads;
use teams_proto::{CreateChat, Mri};

use super::client;
use crate::support::{TestServer, backends};

#[tokio::test(flavor = "multi_thread")]
async fn conversation_pages_keep_their_snapshot_and_survive_another_node_starting() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let b = server.add_user("b@own.test", "Bert").await;
        let c = server.add_user("c@own.test", "Cora").await;
        let client = client(&server, "a@own.test", &a).await;
        let create = || {
            CreateChat::group(
                Mri::new(format!("8:orgid:{a}")),
                [Mri::new(format!("8:orgid:{b}")), Mri::new(format!("8:orgid:{c}"))],
            )
            .unwrap()
        };
        for _ in 0..3 {
            assert_eq!(client.write(create()).await.unwrap().status(), 201);
        }
        let reads = ChatReads::new(&client);
        let mut page = reads
            .conversations_from("https://own.example.test/api/chatsvc/own/v1/users/ME/conversations?pageSize=1")
            .await
            .unwrap();
        let sync = page.sync_state().unwrap().to_owned();
        assert_eq!(page.conversations.len(), 1);
        assert_eq!(client.write(create()).await.unwrap().status(), 201);
        let mut seen = vec![page.conversations[0].id.clone()];
        while let Some(next) = page.forward_link() {
            page = reads.conversations_from(next).await.unwrap();
            assert_eq!(page.sync_state(), Some(sync.as_str()));
            seen.extend(page.conversations.iter().map(|entry| entry.id.clone()));
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(seen.iter().collect::<std::collections::HashSet<_>>().len(), 3);
        let changed = reads.conversations_from(&sync).await.unwrap();
        assert_eq!(changed.conversations.len(), 1);
        assert!(!seen.contains(&changed.conversations[0].id));
        let url = server.app.config.database_url.clone();
        let node = TestServer::start_with(backend, |config| {
            config.database_url = url;
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let reader = super::client(&node, "b@own.test", &b).await;
        assert_eq!(
            ChatReads::new(&reader)
                .conversations()
                .await
                .unwrap()
                .conversations
                .len(),
            4
        );
        node.stop().await;
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_creating_the_same_one_to_one_roster_return_one_persisted_thread() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let b = server.add_user("b@own.test", "Bert").await;
        let db = server.app.config.database_url.clone();
        let node = TestServer::start_with(backend, |config| {
            config.database_url = db;
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let first = client(&server, "a@own.test", &a).await;
        let second = client(&node, "b@own.test", &b).await;
        let a = Mri::new(format!("8:orgid:{a}"));
        let b = Mri::new(format!("8:orgid:{b}"));
        let (one, two) = tokio::join!(
            first.write(CreateChat::one_to_one(a.clone(), b.clone()).unwrap()),
            second.write(CreateChat::one_to_one(b, a).unwrap()),
        );
        let one = one.unwrap();
        let two = two.unwrap();
        assert_eq!(one.status(), 201);
        assert_eq!(two.status(), 201);
        let first: serde_json::Value = serde_json::from_slice(one.body()).unwrap();
        let second: serde_json::Value = serde_json::from_slice(two.body()).unwrap();
        assert_eq!(first["threadResource"], second["threadResource"]);
        node.stop().await;
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn groups_supported_by_the_native_builder_are_bounded_by_request_size_without_an_invented_roster_cap() {
    for backend in backends() {
        let server = TestServer::start_with(backend, |config| {
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let a = server.add_user("a@own.test", "Ada").await;
        let password = ngteams_server::db::users::by_id(&server.app.db, &a)
            .await
            .unwrap()
            .unwrap()
            .password_hash;
        let mut others = Vec::new();
        for index in 0..251 {
            let user = ngteams_server::db::users::add(
                &server.app.db,
                &format!("member-{index}@own.test"),
                "Synthetic member",
                &password,
            )
            .await
            .unwrap();
            others.push(Mri::new(format!("8:orgid:{}", user.id)));
        }
        let dir = tempfile::tempdir().unwrap();
        let engine = super::engine(&server, "a@own.test", &a, dir.path()).await;
        let row = engine
            .write(teams_core::engine::Action::CreateChat {
                others,
                topic: Some("Native roster".into()),
            })
            .await
            .unwrap();
        assert_eq!(row.state, teams_store::OutboxState::Sent, "{:?}", row.last_error);
        let reader = client(&server, "a@own.test", &a).await;
        let members = ChatReads::new(&reader)
            .members(row.message_id.as_deref().unwrap())
            .await
            .unwrap();
        assert_eq!(members.members.len(), 252);
        engine.shutdown();
        server.stop().await;
    }
}
