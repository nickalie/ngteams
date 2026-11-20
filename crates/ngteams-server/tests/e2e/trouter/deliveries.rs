//! The registrar and the deliveries: once the client registered its endpoint, what another account does in a chat
//! reaches its socket as the typed events the client reads, and its engine through them.

use std::sync::Arc;
use std::time::Duration;

use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{Outbox, Transient};
use teams_proto::{Author, Client, Mri};
use teams_store::{OutboxState, Store};
use teams_trouter::{Change, ChatEvent, PushEvent, RegistrationReport};

use super::{Account, ORIGIN, delivery, registered_push, server};
use crate::chatsvc::messages::Pair;
use crate::chatsvc::properties::PropertyWrite;
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn the_client_registers_its_endpoint_with_the_registrar() {
    for backend in backends() {
        let server = server(backend).await;
        let id = server.add_user("a@own.test", "Ada").await;
        let account = Account::sign_in(&server, "a@own.test", &id).await;
        let trouter = account.trouter(&server);
        let epid = trouter.endpoint_id().to_owned();
        let mut push = super::Push(trouter.spawn());
        let registered = push
            .until(|event| match event {
                PushEvent::Registered(endpoint) => Some(endpoint),
                _ => None,
            })
            .await;
        assert_eq!(registered.id(), epid);
        assert!(registered.is_for(&Mri::new(account.mri.clone())));
        let report = push
            .until(|event| match event {
                PushEvent::Registration(report) => Some(report),
                _ => None,
            })
            .await;
        assert_eq!(report, RegistrationReport::Registered);
        let until = ngteams_server::registrar::until(&server.app.db, &id, &epid, "TeamsCDLWebWorker")
            .await
            .unwrap()
            .unwrap();
        let ttl = until - ngteams_server::db::now();
        assert!((3590..=3600).contains(&ttl), "{ttl}");
        assert_eq!(
            ngteams_server::registrar::until(&server.app.db, &id, &epid, "SkypeSpacesWeb")
                .await
                .unwrap(),
            None
        );
        push.stop().await;
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_another_account_posts_reaches_the_socket_with_its_edit_reaction_and_delete() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        let mut push = registered_push(&b, &pair.server).await;
        let id = pair.send("pushed to the other member").await;
        let new = delivery(&mut push).await;
        assert!(!new.stored);
        assert!(new.message_id.is_some());
        let ChatEvent::Message { conversation, message } = new.event else {
            panic!("{:?}", new.event);
        };
        assert_eq!(conversation, pair.chat);
        assert_eq!(message.id, id);
        assert_eq!(message.sender(), Some(format!("8:orgid:{}", pair.a_id).as_str()));

        let edited = pair
            .a
            .write(Action::Edit {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                text: teams_proto::chat::MessageText::plain("edited after the push").unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(edited.state, OutboxState::Sent, "{edited:?}");
        let ChatEvent::MessageUpdate { change, message, .. } = delivery(&mut push).await.event else {
            panic!("not a message update");
        };
        assert_eq!(change, Change::Edit);
        assert_eq!(message.id, id);

        let liked = pair
            .a
            .write(Action::React {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: "like".into(),
            })
            .await
            .unwrap();
        assert_eq!(liked.state, OutboxState::Sent, "{liked:?}");
        let ChatEvent::MessageUpdate { change, .. } = delivery(&mut push).await.event else {
            panic!("not a message update");
        };
        assert_eq!(change, Change::Reaction);

        let deleted = pair
            .a
            .write(Action::Delete {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(deleted.state, OutboxState::Sent, "{deleted:?}");
        let ChatEvent::MessageUpdate { change, message, .. } = delivery(&mut push).await.event else {
            panic!("not a message update");
        };
        assert_eq!(change, Change::Delete);
        assert_eq!(message.id, id);
        push.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_reaches_the_other_member_and_never_the_one_typing() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        pair.a.sync().await.unwrap();
        let a = Account::sign_in(&pair.server, "a@own.test", &pair.a_id).await;
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        let mut own = registered_push(&a, &pair.server).await;
        let mut push = registered_push(&b, &pair.server).await;
        pair.a
            .transient(Transient::Typing {
                conversation: pair.chat.clone(),
            })
            .await
            .unwrap();
        let typing = delivery(&mut push).await;
        assert!(!typing.stored);
        assert_eq!(
            typing.event,
            ChatEvent::Typing {
                conversation: pair.chat.clone(),
                from: Some(format!("8:orgid:{}", pair.a_id)),
            }
        );
        assert!(
            own.quiet(Duration::from_millis(300), |event| matches!(
                event,
                PushEvent::Delivery(_)
            ))
            .await
        );
        own.stop().await;
        push.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_accounts_own_read_horizon_and_preferences_reach_only_its_own_sockets() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let id = pair.send("read me").await;
        assert!(pair.b.sync().await.unwrap().failed.is_empty());
        let a = Account::sign_in(&pair.server, "a@own.test", &pair.a_id).await;
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        let mut other = registered_push(&a, &pair.server).await;
        let mut push = registered_push(&b, &pair.server).await;
        let read = pair
            .b
            .write(Action::Read {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(read.state, OutboxState::Sent, "{read:?}");
        let ChatEvent::ReadHorizon { conversation, horizon } = delivery(&mut push).await.event else {
            panic!("not the read horizon");
        };
        assert_eq!(conversation, pair.chat);
        assert_eq!(horizon.last_read_arrival, Some(id.parse().unwrap()));

        let marked = b
            .client(&pair.server)
            .write(PropertyWrite {
                chat: None,
                name: "appConsumptionHorizon".into(),
                value: serde_json::json!({"chat": 1_700_000_000_000_u64, "channel": 0})
                    .to_string()
                    .into(),
            })
            .await
            .unwrap();
        assert_eq!(marked.status(), 200);
        let event = delivery(&mut push).await.event;
        assert!(matches!(event, ChatEvent::AppReadHorizon(_)), "{event:?}");
        // the other member's sockets hear of neither: a peer's read receipt has no delivery in Teams
        assert!(
            other
                .quiet(Duration::from_millis(300), |event| matches!(
                    event,
                    PushEvent::Delivery(_)
                ))
                .await
        );
        other.stop().await;
        push.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_chat_is_delivered_to_its_members_as_a_thread_update() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let c_id = pair.server.add_user("c@own.test", "Cora").await;
        let c = Account::sign_in(&pair.server, "c@own.test", &c_id).await;
        let mut push = registered_push(&c, &pair.server).await;
        let made = pair
            .a
            .write(Action::CreateChat {
                others: vec![
                    Mri::new(format!("8:orgid:{}", pair.b_id)),
                    Mri::new(format!("8:orgid:{c_id}")),
                ],
                topic: Some("Pushed room".into()),
            })
            .await
            .unwrap();
        assert_eq!(made.state, OutboxState::Sent, "{made:?}");
        let ChatEvent::ThreadUpdate {
            thread, roster_version, ..
        } = delivery(&mut push).await.event
        else {
            panic!("a thread update");
        };
        assert_eq!(Some(thread), made.message_id);
        assert!(roster_version.is_some());
        push.stop().await;
        pair.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_with_the_push_channel_hears_typing_and_stores_a_pushed_message() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        pair.a.sync().await.unwrap();
        let b = Account::sign_in(&pair.server, "b@own.test", &pair.b_id).await;
        let dir = tempfile::tempdir().unwrap();
        let mut listening = engine::start(Parts {
            store: Store::open(&dir.path().join("account.sqlite")).unwrap(),
            client: Arc::new(Client::new(b.transport(&pair.server))),
            outbox: Outbox::new(
                Arc::new(b.transport(&pair.server)),
                Author::new(Mri::new(b.mri.clone()), "b@own.test"),
            ),
            calls: None,
            push: Some(engine::Push::new(b.trouter(&pair.server))),
            options: EngineOptions {
                tick: Duration::from_millis(20),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        assert!(listening.sync().await.unwrap().failed.is_empty());
        // typing is only ever pushed: it comes once the engine's endpoint is registered
        let heard = tokio::time::timeout(Duration::from_secs(30), heard_typing(&pair, &mut listening))
            .await
            .unwrap();
        assert_eq!(heard, pair.chat);
        let id = pair.send("stored from the push").await;
        let stored = tokio::time::timeout(Duration::from_secs(30), stored_message(&listening, &pair.chat, &id))
            .await
            .unwrap();
        assert_eq!(stored.id, id);
        listening.shutdown();
        pair.stop().await;
    }
}

/// Has a type in the pair's chat until `listening` hears it; the conversation it heard typing in.
async fn heard_typing(pair: &Pair, listening: &mut EngineHandle) -> String {
    loop {
        pair.a
            .transient(Transient::Typing {
                conversation: pair.chat.clone(),
            })
            .await
            .unwrap();
        let heard = tokio::time::timeout(Duration::from_millis(300), typing_event(listening)).await;
        if let Ok(conversation) = heard {
            return conversation;
        }
    }
}

async fn typing_event(listening: &mut EngineHandle) -> String {
    loop {
        if let Some(Event::Typing {
            conversation,
            typing: true,
            ..
        }) = listening.next_event().await
        {
            return conversation;
        }
    }
}

/// The message once `listening`'s store holds it.
async fn stored_message(listening: &EngineHandle, chat: &str, id: &str) -> teams_store::Message {
    loop {
        if let Some(message) = listening.message(chat, id).await.unwrap() {
            return message;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[test]
fn the_origin_the_tests_serve_is_the_clients_own_cloud() {
    assert!(teams_proto::Cloud::own(ORIGIN).is_ok());
}
