//! A group chat's name: any of its people names it, renames it or takes the name off, and every one of them is told.
//! Only a group chat takes a name.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use teams_core::Outbox;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_proto::chat::{ChatEvent as ThreadEvent, ChatReads, TopicUpdate, chat_event};
use teams_proto::{Author, Client, Endpoint, MIDDLE_TIER_AUDIENCE, Method, Mri, Request, Service, SetChatTopic};
use teams_store::{OutboxState, Store};
use teams_trouter::{ChatEvent, ThreadTopic};

use super::activity::Trio;
use super::{ChatClient, client};
use crate::support::{TestServer, backends};
use crate::teams::security::{call, token};
use crate::trouter::{Account, Push, delivery, registered_push};

const MEETING: &str = "/api/mt/part/own-01/beta/me/calendarEvents/privateMeeting/schedulingService/create";

/// `PUT v1/threads/<chat>/properties?name=topic` with any body: what the typed request never sends.
struct Naming {
    chat: String,
    body: Value,
}

impl Request for Naming {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Put
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", &self.chat, "properties"]).with_query("name", "topic")
    }
    fn body(&self) -> Option<Value> {
        Some(self.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}

/// An engine of `account` with its push channel.
fn listening(server: &TestServer, account: &Account, upn: &str, dir: &Path) -> EngineHandle {
    engine::start(Parts {
        store: Store::open(&dir.join("account.sqlite")).unwrap(),
        client: Arc::new(Client::new(account.transport(server))),
        outbox: Outbox::new(
            Arc::new(account.transport(server)),
            Author::new(Mri::new(account.mri.clone()), upn),
        ),
        calls: None,
        push: Some(engine::Push::new(account.trouter(server))),
        options: EngineOptions {
            tick: Duration::from_millis(20),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

async fn rename(engine: &EngineHandle, chat: &str, topic: &str) {
    let row = engine
        .write(Action::RenameChat {
            conversation: chat.into(),
            topic: topic.into(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
}

async fn stored_topic(engine: &EngineHandle, chat: &str) -> Option<String> {
    engine.conversation(chat.into()).await.unwrap().unwrap().topic
}

/// Waits until `engine`'s store calls `chat` by `topic`.
async fn stored_until(engine: &EngineHandle, chat: &str, topic: Option<&str>) {
    let wait = async {
        while stored_topic(engine, chat).await.as_deref() != topic {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait)
        .await
        .unwrap_or_else(|_| panic!("the engine never stored the name {topic:?}"));
}

/// The newest message `reader` reads in `chat`, as the name's change it must be.
async fn newest_update(reader: &ChatClient, chat: &str) -> TopicUpdate {
    let page = ChatReads::new(reader).messages(chat, 1).await.unwrap();
    let message = &page.messages[0];
    assert_eq!(message.message_type.as_str(), "ThreadActivity/TopicUpdate");
    match chat_event(message.message_type.as_str(), &message.content) {
        Some(ThreadEvent::TopicUpdated(update)) => update,
        other => panic!("not a name's change: {other:?}"),
    }
}

/// The name's change and then the thread pushed to `push`: the change, and the name the thread update carries.
async fn pushed(push: &mut Push, chat: &str) -> (TopicUpdate, ThreadTopic) {
    let mut told = None;
    loop {
        match delivery(push).await.event {
            ChatEvent::Message { conversation, message } if conversation == chat => {
                if let Some(ThreadEvent::TopicUpdated(update)) =
                    chat_event(message.message_type.as_str(), &message.content)
                {
                    told = Some(update);
                }
            }
            ChatEvent::ThreadUpdate { thread, topic, .. } if thread == chat => {
                return (told.expect("the name's change before the thread"), topic);
            }
            _ => {}
        }
    }
}

/// The kinds of what the fan-out tells one account, until it told a `ThreadUpdate`; the thread's `topic` then.
async fn heard(events: &mut ngteams_server::fanout::Subscription) -> (Vec<String>, Value) {
    let mut kinds = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        let event: Value = serde_json::from_slice(&event).unwrap();
        let kind = event["resourceType"].as_str().unwrap().to_owned();
        if kind == "NewMessage" {
            kinds.push(event["resource"]["messagetype"].as_str().unwrap().to_owned());
        } else if kind == "ThreadUpdate" {
            kinds.push(kind);
            return (kinds, event["resource"]["properties"]["topic"].clone());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_group_chat_renamed_by_a_member_is_named_for_everyone() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let server = &trio.pair.server;
        let (a, b) = (&trio.pair.a, &trio.pair.b);
        let group = trio.group.as_str();
        let a_mri = format!("8:orgid:{}", trio.pair.a_id);
        let b_mri = format!("8:orgid:{}", trio.pair.b_id);
        assert!(a.sync().await.unwrap().failed.is_empty());
        assert!(b.sync().await.unwrap().failed.is_empty());
        let bert = client(server, "b@own.test", &trio.pair.b_id).await;
        let mut push = registered_push(&Account::sign_in(server, "b@own.test", &trio.pair.b_id).await, server).await;
        let cora = Account::sign_in(server, "c@own.test", &trio.c_id).await;
        let dir = tempfile::tempdir().unwrap();
        let cora = listening(server, &cora, "c@own.test", dir.path());
        assert!(cora.sync().await.unwrap().failed.is_empty());
        assert_eq!(stored_topic(&cora, group).await.as_deref(), Some("Launch"));
        let mut own = server.app.fanout.subscribe(&format!("push:{a_mri}")).await.unwrap();

        // a names it; the name goes trimmed, and its XML is kept as text
        let name = "R&D <crew>";
        rename(a, group, &format!("  {name}  ")).await;
        assert_eq!(stored_topic(a, group).await.as_deref(), Some(name));
        assert_eq!(ChatReads::new(&bert).thread(group).await.unwrap().topic(), Some(name));
        let named = TopicUpdate {
            initiator: a_mri.clone(),
            topic: Some(name.to_owned()),
        };
        assert_eq!(newest_update(&bert, group).await, named);
        assert_eq!(
            pushed(&mut push, group).await,
            (named, ThreadTopic::Named(name.to_owned()))
        );
        let kinds = ["ThreadActivity/TopicUpdate".to_owned(), "ThreadUpdate".to_owned()];
        assert_eq!(
            heard(&mut own).await,
            (kinds.to_vec(), json!(name)),
            "the caller hears both"
        );
        stored_until(&cora, group, Some(name)).await;

        // b takes the name off: the chat has none, for everyone
        rename(b, group, "").await;
        assert_eq!(stored_topic(b, group).await, None);
        assert_eq!(ChatReads::new(&bert).thread(group).await.unwrap().topic(), None);
        let listed = ChatReads::new(&bert).conversations().await.unwrap();
        let entry = listed.conversations.iter().find(|entry| entry.id == group).unwrap();
        assert!(entry.thread_properties.get("topic").is_none(), "{entry:?}");
        let unnamed = TopicUpdate {
            initiator: b_mri,
            topic: None,
        };
        assert_eq!(newest_update(&bert, group).await, unnamed);
        assert_eq!(pushed(&mut push, group).await, (unnamed, ThreadTopic::Unnamed));
        assert_eq!(heard(&mut own).await, (kinds.to_vec(), Value::Null));
        stored_until(&cora, group, None).await;

        cora.shutdown();
        push.stop().await;
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_group_chats_own_people_name_it_and_no_longer_than_250_characters() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let server = &trio.pair.server;
        let group = trio.group.as_str();
        let own = client(server, "a@own.test", &trio.pair.a_id).await;
        let dan_id = server.add_user("dan@own.test", "Dan").await;
        let dan = client(server, "dan@own.test", &dan_id).await;
        let typed = |chat: &str, topic: &str| SetChatTopic::new(chat, topic).unwrap();
        let naming = |chat: &str, body: Value| Naming {
            chat: chat.into(),
            body,
        };

        assert_eq!(dan.write(typed(group, "Mine now")).await.unwrap().status(), 403);
        let solo = own.write(typed(&trio.pair.chat, "Just us")).await.unwrap();
        assert_eq!(solo.status(), 400, "a 1:1 chat takes no name");
        assert_eq!(own.write(typed("48:notes", "Notes")).await.unwrap().status(), 404);
        let mt = token(server, "a@own.test", MIDDLE_TIER_AUDIENCE).await;
        let body = json!({"meetingType": "MeetNow", "subject": "Standup"});
        let (status, meeting) = call(server, &mt, reqwest::Method::POST, MEETING, Some(body)).await;
        assert_eq!(status, 201, "{meeting}");
        let meeting = meeting["value"]["groupContext"]["threadId"].as_str().unwrap();
        let named = own.write(typed(meeting, "Ours")).await.unwrap();
        assert_eq!(named.status(), 400, "a meeting's chat takes no name");

        let long = "x".repeat(251);
        assert_eq!(
            own.write(naming(group, json!({"topic": long}))).await.unwrap().status(),
            400
        );
        assert_eq!(
            own.write(naming(group, json!({"topic": 7}))).await.unwrap().status(),
            400
        );
        assert_eq!(own.write(naming(group, json!({}))).await.unwrap().status(), 400);
        let reads = ChatReads::new(&own);
        assert_eq!(reads.thread(group).await.unwrap().topic(), Some("Launch"));
        assert!(reads.messages(group, 200).await.unwrap().messages.is_empty());

        // the longest name is taken, a name with the same name is no change
        let longest = "é".repeat(250);
        assert_eq!(own.write(typed(group, &longest)).await.unwrap().status(), 200);
        assert_eq!(own.write(typed(group, &longest)).await.unwrap().status(), 200);
        assert_eq!(reads.thread(group).await.unwrap().topic(), Some(longest.as_str()));
        assert_eq!(reads.messages(group, 200).await.unwrap().messages.len(), 1);
        trio.stop().await;
    }
}
