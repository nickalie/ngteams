use std::time::Duration;

use reqwest::Method;
use serde_json::json;
use teams_proto::bot::InvokeAnswer;

use super::super::{Action, app_path, backends, call, id_of, synced, tls_parent};
use super::support::{Case, fetch};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_nonmember_and_a_disabled_or_removed_bot_get_no_invoke() {
    if tls_parent("binary::bots::delivery::invoke::access::a_nonmember_and_a_disabled_or_removed_bot_get_no_invoke") {
        return;
    }
    for backend in backends() {
        let case = Case::open(backend, "access").await;
        assert_eq!(case.invoke(&case.cora, fetch()).await, InvokeAnswer::Refused(403));
        case.bot.quiet().await;
        let body = json!({"name": "composeExtension/fetchCommands", "appId": case.bot_id,
            "conversation": {"id": case.chat}, "imdisplayname": "spoofed name",
            "userAadToken": "synthetic-user-token-that-must-not-reach-the-bot",
            "from": {"id": format!("29:{}", id_of(&case.cora))}});
        assert_eq!(case.raw(&case.ada, &body).await, 200);
        let got = case.got(&case.ada, "composeExtension/fetchCommands").await;
        assert!(got.activity.get("userAadToken").is_none());
        assert!(
            !got.activity
                .to_string()
                .contains("synthetic-user-token-that-must-not-reach-the-bot")
        );
        assert_ne!(got.activity["from"]["name"], "spoofed name");
        let path = app_path(&case.bot_id);
        assert_eq!(
            call(
                &case.node,
                Method::PATCH,
                &path,
                Some(&case.root),
                Some(json!({"enabled": false}))
            )
            .await
            .0,
            200
        );
        assert_eq!(case.invoke(&case.ada, fetch()).await, InvokeAnswer::Refused(403));
        case.bot.quiet().await;
        assert_eq!(
            call(
                &case.node,
                Method::PATCH,
                &path,
                Some(&case.root),
                Some(json!({"enabled": true}))
            )
            .await
            .0,
            200
        );
        assert!(matches!(
            case.invoke(&case.bea, fetch()).await,
            InvokeAnswer::Commands(_)
        ));
        case.got(&case.bea, "composeExtension/fetchCommands").await;
        assert_eq!(
            call(&case.node, Method::DELETE, &path, Some(&case.root), None).await.0,
            204
        );
        assert_eq!(case.invoke(&case.ada, fetch()).await, InvokeAnswer::Refused(404));
        case.bot.quiet().await;
        case.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_members_add_and_remove_reactions_on_the_bots_card() {
    if tls_parent("binary::bots::delivery::invoke::access::both_members_add_and_remove_reactions_on_the_bots_card") {
        return;
    }
    for backend in backends() {
        let case = Case::open(backend, "reactions").await;
        let card = case.card().await;
        for (actor, key) in [(&case.ada, "like"), (&case.bea, "heart")] {
            actor
                .write(Action::React {
                    conversation: case.chat.clone(),
                    message_id: card.clone(),
                    key: key.into(),
                })
                .await;
            let got = case.got(actor, "messageReaction").await;
            assert_eq!(got.activity["replyToId"], card);
            assert_eq!(got.activity["reactionsAdded"], json!([{"type": key}]));
            assert!(got.activity.get("reactionsRemoved").is_none());
            actor
                .write(Action::Unreact {
                    conversation: case.chat.clone(),
                    message_id: card.clone(),
                    key: key.into(),
                })
                .await;
            let got = case.got(actor, "messageReaction").await;
            assert_eq!(got.activity["replyToId"], card);
            assert_eq!(got.activity["reactionsRemoved"], json!([{"type": key}]));
            assert!(got.activity.get("reactionsAdded").is_none());
        }
        let messages = synced(&case.bea, &case.chat).await;
        assert!(messages.iter().any(|message| message.id == card));
        let own = case.ada.send(&case.chat, "a person's message").await;
        case.ada
            .write(Action::React {
                conversation: case.chat.clone(),
                message_id: own,
                key: "like".into(),
            })
            .await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        case.bot.quiet().await;
        case.close().await;
    }
}
