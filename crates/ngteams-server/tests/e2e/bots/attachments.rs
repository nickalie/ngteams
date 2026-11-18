//! A bot's pictures (`docs/server.md` "Bots"): the Bot Connector's attachment routes keep them in the server's
//! storage as media objects of the bot's chat, and a message that carries one shows it as a pasted picture, read
//! by the chat's people alone. Pictures and names are synthetic.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Method;
use serde_json::json;
use teams_proto::{Cloud, MessagePicture};

use super::connector::Setup;
use super::endpoint::{Connector, ORIGIN, TestBot, encode};
use super::png;
use crate::support::backends;

/// A GET under the connector's service URL; the status, the content type and the bytes.
async fn fetch(connector: &Connector, path: &str) -> (u16, String, Vec<u8>) {
    let mut request = reqwest::Client::new().get(format!("{}{path}", connector.base));
    if let Some(token) = &connector.token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    (status, kind, response.bytes().await.unwrap().to_vec())
}

/// The `src` of each pasted picture in `html`.
fn picture_srcs(html: &str) -> Vec<String> {
    html.split("<img src=\"")
        .skip(1)
        .map(|rest| rest[..rest.find('"').unwrap()].to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bots_uploaded_picture_is_kept_and_shown_to_the_chat_alone() {
    for backend in backends() {
        let setup = Setup::open(backend).await;
        let chat = setup.chat();
        setup.send(&chat, "show me the chart").await;
        let got = setup.bot.next_of("message").await;
        let service_url = got.activity["serviceUrl"].as_str().unwrap().to_owned();
        let connector = setup.connector(&service_url).await;
        let bytes = png(40, 30);
        let upload = json!({"type": "image/png", "name": "chart.png", "originalBase64": STANDARD.encode(&bytes)});
        let path = format!("v3/conversations/{}/attachments", encode(&chat));
        let (status, answer) = connector.call(Method::POST, &path, Some(upload.clone())).await;
        assert_eq!(status, 200, "{answer}");
        let id = answer["id"].as_str().unwrap().to_owned();

        let (status, info) = connector.call(Method::GET, &format!("v3/attachments/{id}"), None).await;
        assert_eq!(status, 200, "{info}");
        assert_eq!(info["name"], "chart.png");
        assert_eq!(info["type"], "image/png");
        assert_eq!(info["views"], json!([{"viewId": "original", "size": bytes.len()}]));
        let (status, kind, read) = fetch(&connector, &format!("v3/attachments/{id}/views/original")).await;
        assert_eq!((status, kind.as_str()), (200, "image/png"));
        assert_eq!(read, bytes);

        // the picture in a message, by its connector URL, and another as a data URL
        let inline = png(8, 8);
        let data_url = format!("data:image/png;base64,{}", STANDARD.encode(&inline));
        let message = json!({"type": "message", "text": "Here it is", "attachments": [
            {"contentType": "image/png", "name": "chart.png",
             "contentUrl": format!("{service_url}v3/attachments/{id}/views/original")},
            {"contentType": "image/png", "contentUrl": data_url}]});
        let (status, answer) = connector.send(&chat, message).await;
        assert_eq!(status, 200, "{answer}");
        let messages = setup.synced(&chat).await;
        // the text and both pictures after it, in one message
        let shown = messages.last().unwrap();
        assert!(shown.content.starts_with("<p>Here it is</p>"), "{}", shown.content);
        assert_eq!(shown.content.matches("http://schema.skype.com/AMSImage").count(), 2);
        let srcs = picture_srcs(&shown.content);
        let first = srcs[0].clone();
        assert_eq!(first, format!("{ORIGIN}/ams/v1/objects/{id}/views/imgo"));
        let readable = MessagePicture::of_in(&Cloud::own(ORIGIN).unwrap(), &first);
        assert!(readable.is_some(), "{first}");
        let token = teams_trouter::TrouterCredentials::ic3(&setup.registry.server.session("ada@own.test").await)
            .await
            .unwrap();
        for (src, expected) in srcs.iter().zip([&bytes, &inline]) {
            let src = src.replacen(ORIGIN, &setup.registry.server.origin, 1);
            let read = reqwest::Client::new()
                .get(src)
                .bearer_auth(token.token.expose())
                .send()
                .await
                .unwrap();
            assert_eq!(read.status(), 200);
            assert_eq!(read.bytes().await.unwrap().as_ref(), expected.as_slice());
        }

        refusals(&setup, &connector, &service_url, &id).await;
        setup.stop().await;
    }
}

/// A bot that isn't in the chat neither puts a picture in it nor reads one of it; only pictures are kept.
async fn refusals(setup: &Setup, connector: &Connector, service_url: &str, id: &str) {
    let chat = setup.chat();
    let path = format!("v3/conversations/{}/attachments", encode(&chat));
    let upload = json!({"type": "image/png", "name": "chart.png", "originalBase64": STANDARD.encode(png(4, 4))});
    let other = TestBot::start().await;
    let answer = setup.registry.register("Other (synthetic)", &other.url).await;
    let other = Connector::signed(
        &setup.registry.server,
        service_url,
        answer["id"].as_str().unwrap(),
        answer["secret"].as_str().unwrap(),
    )
    .await;
    assert_eq!(other.call(Method::POST, &path, Some(upload)).await.0, 403);
    assert_eq!(
        other.call(Method::GET, &format!("v3/attachments/{id}"), None).await.0,
        404
    );
    assert_eq!(
        fetch(&other, &format!("v3/attachments/{id}/views/original")).await.0,
        404
    );
    // only pictures are kept, and a picture has no other view
    let text = json!({"type": "text/plain", "name": "notes.txt", "originalBase64": STANDARD.encode(b"plain words")});
    assert_eq!(connector.call(Method::POST, &path, Some(text)).await.0, 400);
    assert_eq!(
        fetch(connector, &format!("v3/attachments/{id}/views/thumbnail"))
            .await
            .0,
        404
    );
}

fn inline_picture() -> serde_json::Value {
    json!({"contentType": "image/png", "contentUrl": format!("data:image/png;base64,{}", STANDARD.encode(png(8, 8)))})
}

async fn picture_setup(backend: crate::support::Backend) -> (Setup, Connector) {
    let setup = Setup::open_with(backend, |config| config.media.user_quota_bytes = png(8, 8).len() as u64).await;
    setup.send(&setup.chat(), "pictures").await;
    let got = setup.bot.next_of("message").await;
    let connector = setup.connector(got.activity["serviceUrl"].as_str().unwrap()).await;
    (setup, connector)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_cannot_be_posted_in_another_chat() {
    for backend in backends() {
        let (setup, connector) = picture_setup(backend).await;
        let upload = json!({"originalBase64": STANDARD.encode(png(8, 8))});
        let path = format!("v3/conversations/{}/attachments", encode(&setup.chat()));
        let (status, answer) = connector.call(Method::POST, &path, Some(upload)).await;
        assert_eq!(status, 200);
        let bob = setup.registry.server.add_user("bob@own.test", "Bob (synthetic)").await;
        let (_, made) = connector
            .call(
                Method::POST,
                "v3/conversations",
                Some(json!({"members": [{"id": format!("29:{bob}")}]})),
            )
            .await;
        let picture = json!({"attachments": [{"contentType": "image/png", "contentUrl": format!("{}v3/attachments/{}/views/original", connector.base, answer["id"].as_str().unwrap())}]});
        assert_eq!(connector.send(made["id"].as_str().unwrap(), picture).await.0, 404);
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_delete_releases_a_picture_only_after_its_last_message() {
    for backend in backends() {
        let (setup, connector) = picture_setup(backend).await;
        let chat = setup.chat();
        let (status, first) = connector.send(&chat, json!({"attachments": [inline_picture()]})).await;
        assert_eq!(status, 200);
        let messages = setup.synced(&chat).await;
        let src = picture_srcs(&messages.last().unwrap().content).remove(0);
        let id = src.split("/objects/").nth(1).unwrap().split('/').next().unwrap();
        let attachment = json!({"attachments": [{"contentType": "image/png", "contentUrl": format!("{}v3/attachments/{id}/views/original", connector.base)}]});
        let (status, second) = connector.send(&chat, attachment).await;
        assert_eq!(status, 200);
        for (message, expected) in [(&first, 200), (&second, 404)] {
            let path = format!(
                "v3/conversations/{}/activities/{}",
                encode(&chat),
                message["id"].as_str().unwrap()
            );
            assert_eq!(connector.call(Method::DELETE, &path, None).await.0, 200);
            assert_eq!(
                fetch(&connector, &format!("v3/attachments/{id}/views/original"))
                    .await
                    .0,
                expected
            );
        }
        assert_eq!(
            connector
                .send(&chat, json!({"attachments": [inline_picture()]}))
                .await
                .0,
            200
        );
        setup.stop().await;
    }
}

async fn send_unused_picture_activity(setup: &Setup, connector: &Connector, case: usize) {
    let chat = setup.chat();
    let mut activity = json!({"attachments": [inline_picture()]});
    let attachments = activity["attachments"].as_array_mut().unwrap();
    match case {
        0 => attachments.push(json!({"contentType": "application/vnd.microsoft.card.adaptive", "content": {"type": "AdaptiveCard", "body": [{"text": "no type"}]}})),
        1 => attachments.push(json!({"contentType": "image/png", "contentUrl": "data:image/png;base64,bad!"})),
        2 => attachments.push(json!({"contentType": "image/png", "contentUrl": format!("{}v3/attachments/missing/views/original", connector.base)})),
        3 => {},
        4 => attachments.push(json!({"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "card"}})),
        _ => attachments.push(inline_picture()),
    }
    if (3..=4).contains(&case) {
        let (_, sent) = connector.send(&chat, json!({"text": "before"})).await;
        let id = if case == 3 { "1" } else { sent["id"].as_str().unwrap() };
        let path = format!("v3/conversations/{}/activities/{id}", encode(&chat));
        assert_eq!(
            connector.call(Method::PUT, &path, Some(activity)).await.0,
            if case == 3 { 404 } else { 200 }
        );
    } else {
        assert_eq!(
            connector.send(&chat, activity).await.0,
            match case {
                2 => 404,
                5 => 507,
                _ => 400,
            }
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_activities_and_unused_update_pictures_do_not_consume_quota() {
    for backend in backends() {
        for case in 0..6 {
            let (setup, connector) = picture_setup(backend).await;
            let chat = setup.chat();
            send_unused_picture_activity(&setup, &connector, case).await;
            assert_eq!(
                connector
                    .send(&chat, json!({"attachments": [inline_picture()]}))
                    .await
                    .0,
                200,
                "case {case}"
            );
            setup.stop().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_edit_releases_the_picture_it_removes() {
    for backend in backends() {
        let (setup, connector) = picture_setup(backend).await;
        let chat = setup.chat();
        let (status, sent) = connector.send(&chat, json!({"attachments": [inline_picture()]})).await;
        assert_eq!(status, 200);
        let path = format!(
            "v3/conversations/{}/activities/{}",
            encode(&chat),
            sent["id"].as_str().unwrap()
        );
        assert_eq!(
            connector
                .call(Method::PUT, &path, Some(json!({"text": "without picture"})))
                .await
                .0,
            200
        );
        assert_eq!(
            connector
                .send(&chat, json!({"attachments": [inline_picture()]}))
                .await
                .0,
            200
        );
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_delete_releases_more_than_one_batch_of_pictures() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| {
            config.media.user_quota_bytes = (65 * png(8, 8).len()) as u64;
        })
        .await;
        let chat = setup.chat();
        setup.send(&chat, "many pictures").await;
        let got = setup.bot.next_of("message").await;
        let connector = setup.connector(got.activity["serviceUrl"].as_str().unwrap()).await;
        let activity = json!({"attachments": vec![inline_picture(); 65]});
        let (status, sent) = connector.send(&chat, activity.clone()).await;
        assert_eq!(status, 200);
        let messages = setup.synced(&chat).await;
        assert_eq!(picture_srcs(&messages.last().unwrap().content).len(), 65);
        let path = format!(
            "v3/conversations/{}/activities/{}",
            encode(&chat),
            sent["id"].as_str().unwrap()
        );
        assert_eq!(connector.call(Method::DELETE, &path, None).await.0, 200);
        assert_eq!(connector.send(&chat, activity).await.0, 200);
        setup.stop().await;
    }
}
