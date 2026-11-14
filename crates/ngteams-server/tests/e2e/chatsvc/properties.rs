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

#[tokio::test(flavor = "multi_thread")]
async fn native_app_preferences_persist_and_are_visible_only_to_their_own_account() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let a_client = client(&pair.server, "a@own.test", &pair.a_id).await;
        let mut settings = teams_proto::UserGraphSettings::default();
        settings.turn_off("synthetic-app", &["mention".into()]).unwrap();
        let row = a_client
            .write(teams_proto::SetUserGraphSettings(settings.clone()))
            .await
            .unwrap();
        assert_eq!(row.status(), 200);
        let read = a_client.read(&teams_proto::UserGraphSettingsRead).await.unwrap();
        assert_eq!(
            teams_proto::UserGraphSettingsRead::settings_of(read.status(), read.body()).unwrap(),
            settings
        );
        let b_client = client(&pair.server, "b@own.test", &pair.b_id).await;
        let absent = b_client.read(&teams_proto::UserGraphSettingsRead).await.unwrap_err();
        let teams_proto::HttpError::Status(absent) = absent else {
            panic!("expected an empty own-properties account");
        };
        assert!(teams_proto::UserGraphSettingsRead::is_absent_answer(absent.body()));
        let url = pair.server.app.config.database_url.clone();
        let node = crate::support::TestServer::start_with(backend, |config| {
            config.database_url = url;
            config.public_origin = url::Url::parse("https://own.example.test").unwrap();
        })
        .await;
        let reopened = client(&node, "a@own.test", &pair.a_id).await;
        let read = reopened.read(&teams_proto::UserGraphSettingsRead).await.unwrap();
        assert_eq!(
            teams_proto::UserGraphSettingsRead::settings_of(read.status(), read.body()).unwrap(),
            settings
        );
        node.stop().await;
        pair.stop().await;
    }
}

struct PropertyWrite {
    chat: Option<String>,
    name: String,
    value: serde_json::Value,
}

impl teams_proto::Request for PropertyWrite {
    fn service(&self) -> teams_proto::Service {
        teams_proto::Service::ChatService
    }
    fn method(&self) -> teams_proto::Method {
        teams_proto::Method::Put
    }
    fn endpoint(&self) -> teams_proto::Endpoint {
        let mut segments = vec!["v1", "users", "ME"];
        if let Some(chat) = &self.chat {
            segments.extend(["conversations", chat]);
        }
        segments.push("properties");
        teams_proto::Endpoint::new(segments).with_query("name", &self.name)
    }
    fn body(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({self.name.clone(): self.value}))
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_wide_horizon_marks_all_read_and_can_be_taken_back_without_changing_another_account() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let id = pair.send("unread until the account horizon").await;
        pair.b.sync().await.unwrap();
        assert!(pair.b.unread_count(&pair.chat).await.unwrap() > 0);
        let b_client = client(&pair.server, "b@own.test", &pair.b_id).await;
        let set = |chat| PropertyWrite {
            chat: None,
            name: "appConsumptionHorizon".into(),
            value: serde_json::json!({"chat": chat, "channel": 0}).to_string().into(),
        };
        let row = b_client.write(set(id.parse::<u64>().unwrap())).await.unwrap();
        assert_eq!(row.status(), 200);
        pair.b.sync().await.unwrap();
        assert_eq!(pair.b.unread_count(&pair.chat).await.unwrap(), 0);
        let a_client = client(&pair.server, "a@own.test", &pair.a_id).await;
        assert!(
            ChatReads::new(&a_client)
                .app_consumption_horizon()
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(b_client.write(set(0)).await.unwrap().status(), 200);
        pair.b.sync().await.unwrap();
        assert!(pair.b.unread_count(&pair.chat).await.unwrap() > 0);
        pair.stop().await;
    }
}

struct OwnConversation(String);
impl teams_proto::Read for OwnConversation {
    fn service(&self) -> teams_proto::Service {
        teams_proto::Service::ChatService
    }
    fn endpoint(&self) -> teams_proto::Endpoint {
        teams_proto::Endpoint::new(["v1", "users", "ME", "conversations", &self.0])
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_favorite_and_notification_settings_round_trip_without_being_shared() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let a_client = client(&pair.server, "a@own.test", &pair.a_id).await;
        for (name, value) in [
            ("favorite", "true"),
            ("alerts", "false"),
            ("channelNotificationSettings", "{\"followedPosts\":{\"banner\":false}}"),
            ("followAllRc", "false"),
        ] {
            let write = PropertyWrite {
                chat: Some(pair.chat.clone()),
                name: name.into(),
                value: value.into(),
            };
            assert_eq!(a_client.write(write).await.unwrap().status(), 200, "{name}");
        }
        let response = a_client.read(&OwnConversation(pair.chat.clone())).await.unwrap();
        let own: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert_eq!(own["properties"]["favorite"], "true");
        assert_eq!(own["properties"]["alerts"], "false");
        assert_eq!(own["properties"]["followAllRc"], "false");
        assert_eq!(
            own["properties"]["channelNotificationSettings"],
            "{\"followedPosts\":{\"banner\":false}}"
        );
        let b_client = client(&pair.server, "b@own.test", &pair.b_id).await;
        let response = b_client.read(&OwnConversation(pair.chat.clone())).await.unwrap();
        let other: serde_json::Value = serde_json::from_slice(response.body()).unwrap();
        assert!(other["properties"].get("favorite").is_none());
        assert!(other["properties"].get("channelNotificationSettings").is_none());
        pair.stop().await;
    }
}
