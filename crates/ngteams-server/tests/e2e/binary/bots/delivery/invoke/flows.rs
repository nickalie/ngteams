use reqwest::Method;
use serde_json::json;
use teams_proto::bot::{CommandContext, InvokeAnswer, InvokeKind};

use super::super::{backends, synced, tls_parent};
use super::endpoint::attachment;
use super::support::{Case, action, activity_path, fetch};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn members_submit_execute_search_and_complete_both_dialogs() {
    if tls_parent("binary::bots::delivery::invoke::flows::members_submit_execute_search_and_complete_both_dialogs") {
        return;
    }
    for backend in backends() {
        let case = Case::open(backend, "flows").await;
        let card = case.card().await;
        submit(&case, &card).await;
        execute(&case, &card).await;
        search(&case).await;
        extension_dialog(&case).await;
        card_dialog(&case, &card).await;
        case.bot.quiet().await;
        case.close().await;
    }
}

async fn submit(case: &Case, card: &str) {
    let answer = case
        .invoke(
            &case.ada,
            InvokeKind::Submit(action(card, json!({"poll": 7, "answer": "soup"}))),
        )
        .await;
    assert_eq!(answer, InvokeAnswer::Done);
    let got = case.got(&case.ada, "message").await;
    assert_eq!(got.activity["value"], json!({"poll": 7, "answer": "soup"}));
    assert_eq!(got.activity["replyToId"], card);
    assert!(got.activity.get("name").is_none());
    let (status, reply) = case
        .connector()
        .await
        .reply(&case.chat, card, json!({"type": "message", "text": "Soup selected"}))
        .await;
    assert_eq!(status, 200);
    let messages = synced(&case.bea, &case.chat).await;
    assert!(
        messages
            .iter()
            .any(|m| m.id == reply["id"].as_str().unwrap() && m.content.contains("Soup selected"))
    );
}

async fn execute(case: &Case, card: &str) {
    let answer = case
        .invoke(&case.bea, InvokeKind::Execute(action(card, json!({"answer": "tea"}))))
        .await;
    let InvokeAnswer::Card(updated) = answer else {
        panic!("the bot must return a card")
    };
    assert_eq!(updated["body"][0]["text"], "Voted");
    let got = case.got(&case.bea, "adaptiveCard/action").await;
    assert_eq!(got.activity["replyToId"], card);
    assert_eq!(got.activity["value"]["action"]["verb"], "vote");
    assert_eq!(got.activity["value"]["action"]["data"], json!({"answer": "tea"}));
    let body = json!({"type": "message", "attachments": [{
        "contentType": "application/vnd.microsoft.card.adaptive", "content": updated}]});
    assert_eq!(
        case.connector()
            .await
            .call(Method::PUT, &activity_path(&case.chat, card), Some(body))
            .await
            .0,
        200
    );
    for member in [&case.ada, &case.bea] {
        let messages = synced(member, &case.chat).await;
        let shown = messages.iter().find(|m| m.id == card).unwrap();
        assert!(shown.edit_time.is_some());
        let parsed = teams_proto::chat::card::card_message_with_inputs(&shown.content).unwrap();
        assert!(parsed.drawn());
        let teams_proto::chat::card::Kind::Text(title) = &parsed.cards[0].body[0].kind else {
            panic!("the card must start with its title")
        };
        assert_eq!(title.text, "Voted");
    }
}

async fn search(case: &Case) {
    let InvokeAnswer::Commands(commands) = case.invoke(&case.ada, fetch()).await else {
        panic!("no commands")
    };
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].id, "poll");
    assert_eq!(commands[1].id, "search");
    case.got(&case.ada, "composeExtension/fetchCommands").await;
    let query = InvokeKind::Query {
        command: "search".into(),
        parameters: vec![("q".into(), "lunch".into())],
        skip: 0,
        count: 5,
    };
    let InvokeAnswer::Results { attachments, .. } = case.invoke(&case.bea, query).await else {
        panic!("no results")
    };
    let got = case.got(&case.bea, "composeExtension/query").await;
    assert_eq!(
        got.activity["value"]["parameters"],
        json!([{"name": "q", "value": "lunch"}])
    );
    let value = attachments[0]["preview"]["content"]["tap"]["value"].clone();
    let answer = case
        .invoke(&case.bea, InvokeKind::SelectItem { value: value.clone() })
        .await;
    let InvokeAnswer::Results { attachments, .. } = answer else {
        panic!("no chosen card")
    };
    assert_eq!(attachments[0]["content"]["title"], "Poll 7");
    assert_eq!(
        case.got(&case.bea, "composeExtension/selectItem").await.activity["value"],
        value
    );
}

async fn extension_dialog(case: &Case) {
    let kind = InvokeKind::FetchTask {
        command: "poll".into(),
        context: CommandContext::Compose,
        message: None,
    };
    let InvokeAnswer::Dialog(dialog) = case.invoke(&case.ada, kind).await else {
        panic!("no extension dialog")
    };
    assert_eq!(dialog.title.as_deref(), Some("New poll"));
    assert_eq!(dialog.card.unwrap()["body"][0]["text"], "Ask");
    assert_eq!(
        case.got(&case.ada, "composeExtension/fetchTask").await.activity["value"]["commandId"],
        "poll"
    );
    let data = json!({"answer": "salad", "poll": 7}).as_object().unwrap().clone();
    let kind = InvokeKind::SubmitAction {
        command: "poll".into(),
        context: CommandContext::Compose,
        data,
        message: None,
    };
    assert_eq!(
        case.invoke(&case.ada, kind).await,
        InvokeAnswer::Message("Saved".into())
    );
    assert_eq!(
        case.got(&case.ada, "composeExtension/submitAction").await.activity["value"]["data"]["answer"],
        "salad"
    );
}

async fn card_dialog(case: &Case, card: &str) {
    let InvokeAnswer::Dialog(dialog) = case
        .invoke(&case.bea, InvokeKind::TaskFetch(action(card, json!({"poll": 7}))))
        .await
    else {
        panic!("no card dialog")
    };
    assert_eq!(dialog.card.unwrap()["body"][0]["text"], "Ask");
    case.got(&case.bea, "task/fetch").await;
    assert_eq!(
        case.invoke(
            &case.bea,
            InvokeKind::TaskSubmit(action(card, json!({"answer": "coffee"})))
        )
        .await,
        InvokeAnswer::Message("Saved".into())
    );
    assert_eq!(
        case.got(&case.bea, "task/submit").await.activity["value"]["data"]["answer"],
        "coffee"
    );
    assert_eq!(
        case.connector()
            .await
            .send(
                &case.chat,
                json!({"type": "message", "attachments": [attachment("Coffee poll")]})
            )
            .await
            .0,
        200
    );
    assert!(
        synced(&case.ada, &case.chat)
            .await
            .iter()
            .any(|m| m.message_type == "RichText/Media_Card")
    );
}
