//! The sample bots' message extensions, `Action.Execute` and mentions, through the engine; and every flow on two nodes.

use std::collections::BTreeMap;

use serde_json::json;
use teams_core::engine::Action;
use teams_proto::chat::{Marks, MessageText, Node, RichText};
use teams_proto::{CardInvoke, CommandContext, InvokeAnswer, InvokeKind, Mri, TeamWrite};
use teams_store::OutboxState;

use super::{Nodes, PERSON, Running, Sdk, answered, ask, ask_bot, card_message, ready, running, submit, talk};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_bots_in_containers_answer_extensions_and_an_execute() {
    let test = "flows::sdk_bots_in_containers_answer_extensions_and_an_execute";
    for sdk in ready(test, Nodes::One).unwrap_or_default() {
        eprintln!("sdk bot extensions: {}", sdk.name);
        let run = running(&sdk, Nodes::One).await;
        extensions(&run, &sdk).await;
        execute(&run, &sdk).await;
        run.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_bots_in_containers_answer_mentions_in_a_group_chat_and_a_channel() {
    let test = "flows::sdk_bots_in_containers_answer_mentions_in_a_group_chat_and_a_channel";
    for sdk in ready(test, Nodes::One).unwrap_or_default() {
        eprintln!("sdk bot mentions: {}", sdk.name);
        let run = running(&sdk, Nodes::One).await;
        group_chat(&run, &sdk).await;
        channel(&run, &sdk).await;
        run.stop().await;
    }
}

/// Every flow but sign-in on two `api` nodes on Postgres and Redis: Ada writes at one node and reads at the other,
/// where Bea reads the group chat and the channel.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sdk_bots_in_containers_answer_through_two_nodes_on_postgres_and_redis() {
    let test = "flows::sdk_bots_in_containers_answer_through_two_nodes_on_postgres_and_redis";
    for sdk in ready(test, Nodes::Two).unwrap_or_default() {
        eprintln!("sdk bot on two nodes: {}", sdk.name);
        let run = running(&sdk, Nodes::Two).await;
        talk(&run, &sdk).await;
        submit(&run, &sdk).await;
        extensions(&run, &sdk).await;
        execute(&run, &sdk).await;
        group_chat(&run, &sdk).await;
        channel(&run, &sdk).await;
        run.stop().await;
    }
}

async fn invoke(run: &Running, kind: InvokeKind) -> InvokeAnswer {
    let answer = run.reader().engine().bot_invoke(ask_bot(run, kind)).await.unwrap();
    eprintln!("  the invoke's answer: {answer:?}");
    if matches!(answer, InvokeAnswer::Refused(_)) {
        eprintln!("{}", run.container.logs());
    }
    answer
}

fn titles(answer: &InvokeAnswer, run: &Running) -> Vec<String> {
    let InvokeAnswer::Results {
        grid: false,
        attachments,
    } = answer
    else {
        panic!("no result list: {answer:?}\n{}", run.container.logs());
    };
    attachments
        .iter()
        .map(|attachment| attachment["content"]["title"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The search command `search`, the action command `order`'s dialog and its submit.
async fn extensions(run: &Running, sdk: &Sdk) {
    let name = sdk.name;
    // an invoke names a chat the bot is in: the first message makes the bot's chat
    ask(run, "hi", &format!("echo from {name}: hi")).await;
    let query = InvokeKind::Query {
        command: "search".into(),
        parameters: vec![("query".into(), "pears".into())],
        skip: 0,
        count: 25,
    };
    let found = invoke(run, query).await;
    let InvokeAnswer::Results { attachments, .. } = &found else {
        panic!("{found:?}");
    };
    assert!(
        attachments.iter().all(|found| found["preview"].is_object()),
        "{found:?}"
    );
    assert_eq!(
        titles(&found, run),
        [format!("pears one from {name}"), format!("pears two from {name}")]
    );

    let fetch = InvokeKind::FetchTask {
        command: "order".into(),
        context: CommandContext::Compose,
        message: None,
    };
    let dialog = invoke(run, fetch).await;
    let InvokeAnswer::Dialog(dialog) = dialog else {
        panic!("no dialog: {dialog:?}\n{}", run.container.logs());
    };
    assert_eq!(dialog.title.as_deref(), Some(format!("Order ({name})").as_str()));
    let card = dialog.card.unwrap();
    assert!(
        card["body"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "Input.Text" && item["id"] == "item"),
        "{card}"
    );
    let drawn = format!("{:?}", teams_proto::chat::card::adaptive_card_with_inputs(&card));
    assert!(drawn.contains("Submit(BotAction"), "{drawn}");

    let mut data = serde_json::Map::new();
    data.insert("item".into(), json!("pears"));
    let submit = InvokeKind::SubmitAction {
        command: "order".into(),
        context: CommandContext::Compose,
        data,
        message: None,
    };
    let ordered = invoke(run, submit).await;
    assert_eq!(titles(&ordered, run), [format!("ordered pears from {name}")]);
}

/// The bot's approval card: its `Action.Execute` pressed, and the card that takes its place.
async fn execute(run: &Running, sdk: &Sdk) {
    run.ada.send(&run.chat, "execute").await;
    let card = card_message(run, "Approve the order?").await;
    let parsed = teams_proto::chat::card::card_message(&card.content).unwrap();
    let action = parsed.cards[0]
        .actions
        .iter()
        .find_map(|action| match &action.kind {
            teams_proto::chat::card::ActionKind::Submit(action) => Some(action.clone()),
            _ => None,
        })
        .unwrap();
    let teams_proto::chat::card::Via::Execute { verb } = &action.via else {
        panic!("{action:?}");
    };
    assert_eq!(verb.as_deref(), Some("approve"));
    let pressed = InvokeKind::Execute(CardInvoke {
        message: Some(card.id.clone()),
        id: action.id.clone(),
        verb: verb.clone(),
        data: action.data.clone().unwrap_or_else(|| json!({})),
        ..CardInvoke::default()
    });
    let answer = invoke(run, pressed).await;
    let InvokeAnswer::Card(card) = answer else {
        panic!("no card: {answer:?}\n{}", run.container.logs());
    };
    assert_eq!(
        card["body"][0]["text"],
        format!("approved by {PERSON} from {}", sdk.name),
        "{card}"
    );
}

/// Ada's message to `conversation` that mentions the app's bot, composed as the client composes it.
async fn mention(run: &Running, conversation: &str, words: &str) -> String {
    let bot = Mri::new(format!("28:{}", run.bot));
    let text = RichText {
        nodes: vec![Node::Paragraph(vec![
            Node::Mention {
                item_id: "0".into(),
                text: run.short_name.clone(),
            },
            Node::Text {
                text: format!(" {words}"),
                marks: Marks::NONE,
            },
        ])],
        truncated: false,
    };
    let apps = BTreeMap::from([(bot.clone(), run.app.clone())]);
    let text = MessageText::new_with_bots(text, [("0".to_owned(), bot)], &apps).unwrap();
    let row = run
        .ada
        .engine()
        .write(Action::Send {
            conversation: conversation.to_owned(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

/// The app as Ada's store lists it.
async fn definition(run: &Running) -> teams_proto::AppDefinition {
    let listing = run.reader().engine().app_listing().await.unwrap();
    listing.app(&run.app).unwrap().definition.clone()
}

/// A group chat of Ada, Bea and Cora with the app added: Ada mentions the bot, Bea reads its answer (at the other
/// node, when there are two).
async fn group_chat(run: &Running, sdk: &Sdk) {
    let made = run
        .ada
        .engine()
        .write(Action::CreateChat {
            others: vec![Mri::new(&run.bea.mri), Mri::new(&run.cora.mri)],
            topic: Some(format!("{} room (synthetic)", sdk.name)),
        })
        .await
        .unwrap();
    let chat = made.message_id.unwrap();
    let add = TeamWrite::AddChatApp {
        chat: chat.clone(),
        app: definition(run).await,
    };
    let added = run.ada.engine().team_write(add, None).await;
    assert!(added.is_ok(), "{added:?}");
    mention(run, &chat, "hello group").await;
    let expected = format!("mentioned in groupChat from {}: hello group", sdk.name);
    answered(&run.bea, &chat, &run.bot, &expected, &run.container).await;
}

/// A team with Bea and the app: Ada mentions the bot in a post of its first channel, Bea reads the answer in the
/// post's thread.
async fn channel(run: &Running, sdk: &Sdk) {
    let team = run.ada.make_team(&format!("{} crew", sdk.name), &[&run.bea]).await;
    let add = TeamWrite::AddApp {
        team: team.clone(),
        app: definition(run).await,
    };
    let added = run.ada.engine().team_write(add, None).await;
    assert!(added.is_ok(), "{added:?}");
    let post = mention(run, &team, "hello channel").await;
    let expected = format!("mentioned in channel from {}: hello channel", sdk.name);
    let got = answered(&run.bea, &team, &run.bot, &expected, &run.container).await;
    let reply = got.iter().find(|message| message.content.contains(&expected)).unwrap();
    let roots = run.bea.engine().reply_roots(team.clone()).await.unwrap();
    assert!(roots.contains(&(reply.id.clone(), post.clone())), "{roots:?} {reply:?}");
}
