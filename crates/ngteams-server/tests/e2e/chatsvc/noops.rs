//! Retry identities and reaction take-backs must not create duplicate messages or duplicate peer events.

use std::time::Duration;

use ngteams_server::fanout::Subscription;
use serde_json::{Value, json};
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{Author, Mri, RemoveReaction, SendMessage, SetReaction};

use super::{ChatClient, client, messages::Pair};
use crate::support::backends;

async fn next(events: &mut Subscription) -> Value {
    let bytes = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn duplicate_post(pair: &Pair, client: &ChatClient, events: &mut Subscription) -> String {
    let send = SendMessage::new(
        &pair.chat,
        Author::new(Mri::new(format!("8:orgid:{}", pair.a_id)), "Forged display"),
        MessageText::plain("one message despite a repeated request").unwrap(),
    )
    .with_client_message_id("4444444444444444444");
    let first = client.write(send.clone()).await.unwrap();
    assert_eq!(first.status(), 201);
    let event = next(events).await;
    assert_eq!(event["resource"]["imdisplayname"], "Ada");
    let again = client.write(send).await.unwrap();
    assert_eq!(again.status(), 201);
    assert_eq!(again.body(), first.body());
    assert!(
        tokio::time::timeout(Duration::from_millis(25), events.recv())
            .await
            .is_err()
    );
    event["resource"]["id"].as_str().unwrap().to_owned()
}

async fn add_two_reactions(pair: &Pair, a: &ChatClient, b: &ChatClient, id: &str, events: &mut Subscription) {
    assert_eq!(
        a.write(RemoveReaction::new(&pair.chat, id, "like"))
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), events.recv())
            .await
            .is_err()
    );
    assert_eq!(
        a.write(SetReaction::new(&pair.chat, id, "like").at(1_760_000_055_000))
            .await
            .unwrap()
            .status(),
        200
    );
    next(events).await;
    assert_eq!(
        b.write(SetReaction::new(&pair.chat, id, "like"))
            .await
            .unwrap()
            .status(),
        200
    );
}

async fn reaction_noops(pair: &Pair, a: &ChatClient, b: &ChatClient, id: &str, events: &mut Subscription) {
    add_two_reactions(pair, a, b, id, events).await;
    assert_eq!(
        a.write(RemoveReaction::new(&pair.chat, id, "like"))
            .await
            .unwrap()
            .status(),
        200
    );
    let removed = next(events).await;
    assert_eq!(
        removed["resource"]["properties"]["emotions"][0]["users"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        removed["resource"]["properties"]["emotions"][0]["users"][0]["mri"],
        format!("8:orgid:{}", pair.b_id)
    );
    assert_eq!(
        a.write(RemoveReaction::new(&pair.chat, id, "like"))
            .await
            .unwrap()
            .status(),
        200
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(25), events.recv())
            .await
            .is_err()
    );
    assert_eq!(
        b.write(RemoveReaction::new(&pair.chat, id, "like"))
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_posts_and_absent_reaction_removals_have_no_duplicate_effect_or_delivery() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let a = client(&pair.server, "a@own.test", &pair.a_id).await;
        let b = client(&pair.server, "b@own.test", &pair.b_id).await;
        let mut events = pair
            .server
            .app
            .fanout
            .subscribe(&format!("push:8:orgid:{}", pair.b_id))
            .await
            .unwrap();
        let id = duplicate_post(&pair, &a, &mut events).await;
        reaction_noops(&pair, &a, &b, &id, &mut events).await;
        let page = ChatReads::new(&b).messages(&pair.chat, 200).await.unwrap();
        assert_eq!(page.messages.len(), 1);
        let fixture: Value =
            serde_json::from_str(include_str!("../../../../../tests/fixtures/chatsvc/reaction.json")).unwrap();
        assert_eq!(
            page.messages[0].properties["emotions"],
            fixture["read_back_taken_back"]["properties"]["emotions"]
        );
        assert_eq!(page.messages[0].annotations_summary.as_ref(), Some(&json!({})));
        pair.stop().await;
    }
}
