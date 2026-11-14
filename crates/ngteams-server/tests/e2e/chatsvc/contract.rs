//! Wire-shape and malformed-input regressions use the client's own transport and engine.

use serde_json::{Value, json};
use teams_proto::chat::ChatReads;
use teams_proto::{Endpoint, Method, Request, Service};

use super::{client, messages::Pair};
use crate::support::Backend;

#[tokio::test(flavor = "multi_thread")]
async fn message_previews_follow_the_native_contract_without_full_message_properties() {
    let pair = Pair::start(Backend::Sqlite).await;
    pair.send("the list preview").await;
    let client = client(&pair.server, "b@own.test", &pair.b_id).await;
    let listed = ChatReads::new(&client).conversations().await.unwrap();
    let preview = listed
        .conversations
        .iter()
        .find(|chat| chat.id == pair.chat)
        .unwrap()
        .last_message_raw
        .as_ref()
        .unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../../../../../tests/fixtures/chatsvc/conversations.json")).unwrap();
    let golden = &fixture["response"]["body"]["conversations"][0]["lastMessage"];
    assert_eq!(preview["type"], golden["type"]);
    assert_eq!(preview["messagetype"], golden["messagetype"]);
    assert!(golden.get("properties").is_none());
    assert!(preview.get("properties").is_none());
    pair.stop().await;
}

struct MalformedEdit {
    chat: String,
    id: String,
    body: Value,
}
impl Request for MalformedEdit {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Put
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", &self.chat, "messages", &self.id])
    }
    fn body(&self) -> Option<Value> {
        Some(self.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_edits_get_a_native_contract_bad_request_and_leave_the_message_unchanged() {
    let pair = Pair::start(Backend::Sqlite).await;
    let id = pair.send("unchanged after rejected edits").await;
    let client = client(&pair.server, "a@own.test", &pair.a_id).await;
    for body in [json!([]), json!(false), json!(42), json!("bad"), Value::Null] {
        let response = client
            .write(MalformedEdit {
                chat: pair.chat.clone(),
                id: id.clone(),
                body,
            })
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    pair.a.sync().await.unwrap();
    assert!(
        pair.a
            .message(&pair.chat, &id)
            .await
            .unwrap()
            .unwrap()
            .content
            .contains("unchanged after rejected edits")
    );
    pair.stop().await;
}
