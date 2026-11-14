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
