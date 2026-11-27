//! The client's own requests for outgoing webhooks against the server: Manage team's create, list, picture, change and
//! remove through the engine (`TeamWrite`, `outgoing_webhooks`), and a channel post the composer makes with the
//! webhook's bot mention (`MessageText::new_with_bots`), whose replies the engine syncs. It is the QA test of task 412.
//! Teams, people, webhooks and messages are synthetic.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::json;
use teams_core::EngineError;
use teams_core::engine::Action;
use teams_proto::chat::{Marks, MessageText, Node, RichText};
use teams_proto::{Mri, OutgoingWebhook, TeamWrite, WebhookForm, WebhookPicture, reply_chain_id};
use teams_store::OutboxState;

use super::receiver::{Receiver, Reply, hmac};
use super::{Crew, key_of};
use crate::support::backends;
use crate::teams::Seat;

/// A made-up PNG: the server reads only its first bytes.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic picture";
/// A made-up JPEG.
const JPEG: &[u8] = b"\xff\xd8\xff\xe0synthetic picture";
/// The server's default picture limit, the one the dialog applies.
const PICTURE_LIMIT: usize = 1024 * 1024;

fn form(name: &str, url: &str, picture: WebhookPicture) -> WebhookForm {
    WebhookForm {
        name: Some(name.to_owned()),
        callback_url: Some(url.to_owned()),
        description: Some(format!("{name} answers")),
        picture,
    }
}

async fn write(seat: &Seat, write: TeamWrite) -> Result<teams_core::TeamWritten, EngineError> {
    seat.engine.team_write(write, None).await
}

/// The refusal code of a team write the server refused.
fn refusal(result: Result<teams_core::TeamWritten, EngineError>) -> Option<String> {
    match result {
        Err(EngineError::TeamNotChanged { code }) => code,
        other => panic!("not refused: {other:?}"),
    }
}

async fn listed(seat: &Seat, team: &str) -> Vec<OutgoingWebhook> {
    seat.engine.outgoing_webhooks(team).await.unwrap()
}

fn only(hooks: &[OutgoingWebhook]) -> &OutgoingWebhook {
    assert_eq!(hooks.len(), 1, "{hooks:?}");
    &hooks[0]
}

async fn picture(seat: &Seat, hook: &OutgoingWebhook) -> Option<Vec<u8>> {
    let read = hook
        .app()
        .definition
        .outgoing_webhook()
        .expect("a webhook's app names its picture");
    seat.engine.outgoing_webhook_picture(read).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_manages_a_webhook_through_the_client_engine_and_a_member_sees_it() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let team = crew.team.clone();
        assert!(listed(&crew.ada, &team).await.is_empty());

        // create: the answer alone carries the token, a key the receiver can sign with
        let made = write(
            &crew.ada,
            TeamWrite::CreateOutgoingWebhook {
                team: team.clone(),
                form: form(
                    "Build bot",
                    "https://hooks.example.test/build",
                    WebhookPicture::Set(PNG.to_vec()),
                ),
            },
        )
        .await
        .unwrap();
        let created = made.created_webhook.expect("the create answer's webhook and token");
        assert_eq!(created.webhook.name, "Build bot");
        assert_eq!(key_of(created.token.as_str()).len(), 32);
        let id = created.webhook.id.clone();

        let hooks = listed(&crew.ada, &team).await;
        let hook = only(&hooks);
        assert_eq!(hook.id, id);
        assert_eq!(hook.mri, format!("28:{id}"));
        assert_eq!(hook.description, "Build bot answers");
        assert!(hook.can_manage);
        assert_eq!(hook.callback_url.as_deref(), Some("https://hooks.example.test/build"));
        assert_eq!(picture(&crew.ada, hook).await.as_deref(), Some(PNG));
        let mentionable = hook.app().definition.mentionable_bot(true);
        assert_eq!(mentionable, Some(Mri::new(&hook.mri)));

        refusals(&crew, &team, &id).await;

        // Edit sends only the changed fields: the rest stays
        let change = TeamWrite::ChangeOutgoingWebhook {
            team: team.clone(),
            webhook: id.clone(),
            form: WebhookForm {
                description: Some("Builds and their tests".to_owned()),
                picture: WebhookPicture::Set(JPEG.to_vec()),
                ..WebhookForm::default()
            },
        };
        write(&crew.ada, change).await.unwrap();
        let hooks = listed(&crew.ada, &team).await;
        let hook = only(&hooks);
        assert_eq!(
            (hook.name.as_str(), hook.description.as_str()),
            ("Build bot", "Builds and their tests")
        );
        assert_eq!(hook.callback_url.as_deref(), Some("https://hooks.example.test/build"));
        assert_eq!(picture(&crew.ada, hook).await.as_deref(), Some(JPEG));

        let removed = member_view(&crew, &team, &id).await;

        // the picture removed, then the webhook
        let change = TeamWrite::ChangeOutgoingWebhook {
            team: team.clone(),
            webhook: id.clone(),
            form: WebhookForm {
                picture: WebhookPicture::Remove,
                ..WebhookForm::default()
            },
        };
        write(&crew.ada, change).await.unwrap();
        let hooks = listed(&crew.ada, &team).await;
        assert_eq!(only(&hooks).picture_url, None);
        assert_eq!(picture(&crew.ada, only(&hooks)).await, None);
        write(&crew.ada, removed).await.unwrap();
        assert!(listed(&crew.ada, &team).await.is_empty());
        assert!(listed(&crew.bert, &team).await.is_empty());
        crew.stop().await;
    }
}

/// A name the team has, in other letters, and pictures the dialog refuses: the server refuses them, and the picture
/// stays.
async fn refusals(crew: &Crew, team: &str, id: &str) {
    let team = team.to_owned();
    let id = id.to_owned();
    // a second webhook with the same name in other letters: Teams' conflict code, which the dialog words
    let taken = write(
        &crew.ada,
        TeamWrite::CreateOutgoingWebhook {
            team: team.clone(),
            form: form("BUILD BOT", "https://hooks.example.test/other", WebhookPicture::Keep),
        },
    )
    .await;
    assert_eq!(refusal(taken).as_deref(), Some("OutgoingWebhookNameAlreadyExists"));

    // pictures the dialog refuses, the server refuses too
    for wrong in [b"GIF89a synthetic".to_vec(), [PNG, &vec![0; PICTURE_LIMIT]].concat()] {
        let change = TeamWrite::ChangeOutgoingWebhook {
            team: team.clone(),
            webhook: id.clone(),
            form: WebhookForm {
                picture: WebhookPicture::Set(wrong),
                ..WebhookForm::default()
            },
        };
        assert_eq!(refusal(write(&crew.ada, change).await).as_deref(), Some("BadRequest"));
    }
    assert_eq!(
        picture(&crew.ada, only(&listed(&crew.ada, &team).await))
            .await
            .as_deref(),
        Some(PNG)
    );
}

/// What the member sees and may do; the owner's Remove for the webhook.
async fn member_view(crew: &Crew, team: &str, id: &str) -> TeamWrite {
    let team = team.to_owned();
    let id = id.to_owned();
    // the member: the team's default lets members manage apps, so the server offers Edit and Remove
    let seen = listed(&crew.bert, &team).await;
    assert_eq!(only(&seen).name, "Build bot");
    assert!(only(&seen).can_manage);
    // closed to members: no address, no Edit, and the server refuses the member's writes
    crew.close_to_members().await;
    let seen = listed(&crew.bert, &team).await;
    let hook = only(&seen);
    assert!(!hook.can_manage);
    assert_eq!(hook.callback_url, None);
    assert_eq!(picture(&crew.bert, hook).await.as_deref(), Some(JPEG));
    member_refused(crew, &team, &id).await
}

/// The member's Remove and Create, which the server refuses; the owner's Remove for the webhook.
async fn member_refused(crew: &Crew, team: &str, id: &str) -> TeamWrite {
    let team = team.to_owned();
    let removed = TeamWrite::RemoveOutgoingWebhook {
        team: team.clone(),
        webhook: id.to_owned(),
    };
    assert_eq!(
        refusal(write(&crew.bert, removed.clone()).await).as_deref(),
        Some("Forbidden")
    );
    let made = TeamWrite::CreateOutgoingWebhook {
        team: team.clone(),
        form: form("Member bot", "https://hooks.example.test/member", WebhookPicture::Keep),
    };
    assert_eq!(refusal(write(&crew.bert, made).await).as_deref(), Some("Forbidden"));
    // someone on no team: the list is nothing
    assert!(listed(&crew.cora, &team).await.is_empty());
    removed
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archived_team_takes_no_new_webhook_but_still_lists_its_own() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let team = crew.team.clone();
        let made = form("Kept bot", "https://hooks.example.test/kept", WebhookPicture::Keep);
        write(
            &crew.ada,
            TeamWrite::CreateOutgoingWebhook {
                team: team.clone(),
                form: made,
            },
        )
        .await
        .unwrap();
        let archive = TeamWrite::Archive {
            group: crew.group.clone(),
            site_read_only: false,
        };
        write(&crew.ada, archive).await.unwrap();
        let again = TeamWrite::CreateOutgoingWebhook {
            team: team.clone(),
            form: form("Late bot", "https://hooks.example.test/late", WebhookPicture::Keep),
        };
        assert_eq!(
            refusal(write(&crew.ada, again).await).as_deref(),
            Some("OperationNotAllowedOnArchivedTeam")
        );
        assert_eq!(only(&listed(&crew.ada, &team).await).name, "Kept bot");
        crew.stop().await;
    }
}

/// `seat`'s message to `conversation` that mentions `hook`, as the composer makes it from the picker's choice: the
/// webhook's bot with the webhook's id as its app.
async fn mention(seat: &Seat, conversation: &str, hook: &OutgoingWebhook, words: &str) -> String {
    let app = hook.app();
    let bot = app.definition.mentionable_bot(true).expect("a webhook is a team's bot");
    let text = RichText {
        nodes: vec![Node::Paragraph(vec![
            Node::Mention {
                item_id: "0".into(),
                text: hook.name.clone(),
            },
            Node::Text {
                text: format!(" {words}"),
                marks: Marks::NONE,
            },
        ])],
        truncated: false,
    };
    let apps = BTreeMap::from([(bot.clone(), app.app.id.clone())]);
    let text = MessageText::new_with_bots(text, [("0".to_owned(), bot)], &apps).unwrap();
    let row = seat
        .engine
        .write(Action::Send {
            conversation: conversation.to_owned(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    row.message_id.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_post_the_composer_makes_reaches_the_webhook_and_its_replies_show_under_its_name() {
    for backend in backends() {
        let crew = Crew::open(backend).await;
        let receiver = Receiver::start().await;
        let made = write(
            &crew.ada,
            TeamWrite::CreateOutgoingWebhook {
                team: crew.team.clone(),
                form: form("Echo bot", &receiver.url, WebhookPicture::Set(PNG.to_vec())),
            },
        )
        .await
        .unwrap()
        .created_webhook
        .unwrap();
        let key = key_of(made.token.as_str());
        receiver.trust(key.clone());
        // the member's composer offers the webhook from the list it reads
        let hooks = listed(&crew.bert, &crew.team).await;
        let hook = only(&hooks).clone();

        receiver.answer(Reply::json(
            &json!({"type": "message", "text": "**Pong** from the echo"}),
        ));
        let post = mention(&crew.bert, &crew.channel, &hook, "status please").await;
        let got = receiver.next().await;
        assert!(got.signed, "{:?}", got.authorization);
        assert_eq!(got.authorization.as_deref(), Some(hmac(&key, &got.body).as_str()));
        assert_eq!(got.activity["id"], post.as_str());
        assert_eq!(got.activity["text"], "<at>Echo bot</at> status please");
        assert_eq!(got.activity["from"]["name"], "Bert Sample");
        let replies = super::delivery::replies_from(&crew.ada, &crew.channel, &post, &hook.id, 1).await;
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert_eq!(replies[0].sender_name.as_deref(), Some("Echo bot"));
        assert!(
            replies[0].content.contains("<strong>Pong</strong>"),
            "{}",
            replies[0].content
        );

        // a card answer to a mention in the post's thread: a card reply under the same post
        let card = json!({"type": "AdaptiveCard", "version": "1.4",
            "body": [{"type": "TextBlock", "text": "Build 7 passed (synthetic)"}],
            "actions": [{"type": "Action.OpenUrl", "title": "Open the build", "url": "https://ci.example.test/7"}]});
        receiver.answer(Reply::json(&json!({"type": "message", "attachments": [
            {"contentType": "application/vnd.microsoft.card.adaptive", "content": card}]})));
        let chain = reply_chain_id(&crew.channel, &post).unwrap();
        let reply = mention(&crew.ada, &chain, &hook, "and the build?").await;
        let got = receiver.next().await;
        assert!(got.signed);
        assert_eq!(got.activity["id"], reply.as_str());
        assert_eq!(got.activity["from"]["name"], "Ada Lovelace");
        let replies = super::delivery::replies_from(&crew.bert, &crew.channel, &post, &hook.id, 2).await;
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert_eq!(replies[1].sender_name.as_deref(), Some("Echo bot"));
        assert_eq!(replies[1].message_type, "RichText/Media_Card");

        // the webhook removed: a mention reaches nothing more
        write(
            &crew.ada,
            TeamWrite::RemoveOutgoingWebhook {
                team: crew.team.clone(),
                webhook: hook.id.clone(),
            },
        )
        .await
        .unwrap();
        let _ = mention(&crew.bert, &crew.channel, &hook, "still there?").await;
        assert!(!receiver.gets_within(Duration::from_secs(3)).await);
        crew.stop().await;
    }
}
