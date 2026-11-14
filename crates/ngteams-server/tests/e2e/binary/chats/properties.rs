use serde_json::{Value, json};
use teams_core::engine::Action;
use teams_proto::{Endpoint, Method, Request, Service};

use super::support::Rig;
use crate::support::backends;

struct Property {
    chat: Option<String>,
    name: &'static str,
    value: Value,
}

impl Request for Property {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Put
    }
    fn endpoint(&self) -> Endpoint {
        let mut segments = vec!["v1", "users", "ME"];
        if let Some(chat) = &self.chat {
            segments.extend(["conversations", chat]);
        }
        segments.push("properties");
        Endpoint::new(segments).with_query("name", self.name)
    }
    fn body(&self) -> Option<Value> {
        Some(json!({self.name: self.value}))
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reading_clears_only_own_unread_and_mark_all_read_can_be_undone() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let [a, b, c] = &rig.actors;
        let chat = a.create(&[b, c], None).await;
        let id = c.send(&chat, "unread for a and b").await;
        a.sync().await;
        b.sync().await;
        let a_unread = a.engine().unread_count(&chat).await.unwrap();
        assert!(a_unread > 0);
        assert!(b.engine().unread_count(&chat).await.unwrap() > 0);
        b.write(Action::Read {
            conversation: chat.clone(),
            message_id: id.clone(),
        })
        .await;
        a.sync().await;
        b.sync().await;
        assert_eq!(b.engine().unread_count(&chat).await.unwrap(), 0);
        assert_eq!(a.engine().unread_count(&chat).await.unwrap(), a_unread);
        let latest = c.send(&chat, "mark all read later").await;
        b.sync().await;
        a.sync().await;
        let a_unread = a.engine().unread_count(&chat).await.unwrap();
        assert!(a_unread > 0);
        assert!(b.engine().unread_count(&chat).await.unwrap() > 0);
        for (horizon, unread) in [(latest.parse::<u64>().unwrap(), false), (0, true)] {
            let property = Property {
                chat: None,
                name: "appConsumptionHorizon",
                value: json!({"chat": horizon, "channel": 0}).to_string().into(),
            };
            assert_eq!(b.client.write(property).await.unwrap().status(), 200);
            b.sync().await;
            a.sync().await;
            assert_eq!(b.engine().unread_count(&chat).await.unwrap() > 0, unread);
            assert_eq!(a.engine().unread_count(&chat).await.unwrap(), a_unread);
        }
        assert!(b.engine().message(&chat, id).await.unwrap().is_some());
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn favorite_and_mute_stay_private_after_restarting_the_server_nodes() {
    for backend in backends() {
        let mut rig = Rig::start(backend).await;
        rig.actors[0].send(&rig.chat, "preference persistence").await;
        rig.actors[0].sync().await;
        rig.actors[1].sync().await;
        let values = [
            ("favorite", "true"),
            ("alerts", "false"),
            ("channelNotificationSettings", "{\"followedPosts\":{\"banner\":false}}"),
        ];
        for (name, value) in values {
            let property = Property {
                chat: Some(rig.chat.clone()),
                name,
                value: value.into(),
            };
            assert_eq!(rig.actors[0].client.write(property).await.unwrap().status(), 200);
        }
        for restarted in [false, true] {
            if restarted {
                rig.restart_nodes().await;
            }
            rig.actors[0].sync().await;
            rig.actors[1].sync().await;
            let own = rig.actors[0].properties(&rig.chat).await;
            let peer = rig.actors[1].properties(&rig.chat).await;
            for (name, value) in values {
                assert_eq!(own[name], value, "{name}, restarted={restarted}");
                assert!(!peer.contains_key(name), "{name} leaked to peer");
            }
        }
        assert!(
            rig.actors[0]
                .engine()
                .conversation(rig.chat.clone())
                .await
                .unwrap()
                .is_some()
        );
        rig.stop().await;
    }
}
