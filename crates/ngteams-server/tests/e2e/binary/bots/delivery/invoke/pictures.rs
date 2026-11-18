use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Method;
use serde_json::{Value, json};
use teams_proto::{Cloud, MessagePicture};

use super::super::{ADA, Actor, BEA, CORA, backends, encode, synced, tls_parent};
use super::support::Case;

fn png(size: u32) -> Vec<u8> {
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        size,
        size,
        image::Rgba([23, 140, 88, 255]),
    ));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_members_read_uploaded_and_inline_pictures_but_a_nonmember_cannot() {
    if tls_parent(
        "binary::bots::delivery::invoke::pictures::both_members_read_uploaded_and_inline_pictures_but_a_nonmember_cannot",
    ) {
        return;
    }
    for backend in backends() {
        let case = Case::open(backend, "pictures").await;
        let uploaded = png(12);
        let inline = png(8);
        let connector = case.connector().await;
        let path = format!("v3/conversations/{}/attachments", encode(&case.chat));
        let upload =
            json!({"type": "image/png", "name": "synthetic.png", "originalBase64": STANDARD.encode(&uploaded)});
        let (status, answer) = connector.call(Method::POST, &path, Some(upload)).await;
        assert_eq!(status, 200);
        let id = answer["id"].as_str().unwrap();
        let (status, info) = connector.call(Method::GET, &format!("v3/attachments/{id}"), None).await;
        assert_eq!(status, 200);
        assert_eq!(info["name"], "synthetic.png");
        assert_eq!(info["views"], json!([{"viewId": "original", "size": uploaded.len()}]));
        let response = case
            .node
            .http
            .get(format!("{}v3/attachments/{id}/views/original", connector.base))
            .bearer_auth(connector.token.as_ref().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().await.unwrap().as_ref(), uploaded);
        let body = json!({"type": "message", "text": "Synthetic pictures", "attachments": [
            {"contentType": "image/png", "contentUrl": format!("{}v3/attachments/{id}/views/original", connector.base)},
            {"contentType": "image/png", "contentUrl": format!("data:image/png;base64,{}", STANDARD.encode(&inline))}]});
        let (status, sent) = connector.send(&case.chat, body).await;
        assert_eq!(status, 200);
        let srcs = picture_sources(&case, &sent).await;
        assert_eq!(
            srcs[0],
            format!("{}/ams/v1/objects/{id}/views/imgo", case.node.endpoint)
        );
        for upn in [ADA, BEA, CORA] {
            read_as(&case, upn, &srcs, [uploaded.as_slice(), inline.as_slice()]).await;
        }
        drop(connector);
        case.close().await;
    }
}

async fn picture_sources(case: &Case, sent: &Value) -> Vec<String> {
    let mut result = Vec::new();
    for actor in [&case.ada, &case.bea] {
        let messages = synced(actor, &case.chat).await;
        let message = messages.iter().find(|m| m.id == sent["id"].as_str().unwrap()).unwrap();
        assert!(message.content.starts_with("<p>Synthetic pictures</p>"));
        assert_eq!(message.content.matches("http://schema.skype.com/AMSImage").count(), 2);
        let fragment = scraper::Html::parse_fragment(&message.content);
        let selector = scraper::Selector::parse("img").unwrap();
        let srcs: Vec<String> = fragment
            .select(&selector)
            .map(|img| img.value().attr("src").unwrap().to_owned())
            .collect();
        assert_eq!(srcs.len(), 2);
        let cloud = Cloud::own(&case.node.endpoint).unwrap();
        assert!(srcs.iter().all(|src| MessagePicture::of_in(&cloud, src).is_some()));
        if result.is_empty() {
            result = srcs;
        } else {
            assert_eq!(result, srcs);
        }
    }
    result
}

async fn read_as(case: &Case, upn: &str, srcs: &[String], expected: [&[u8]; 2]) {
    let actor = match upn {
        ADA => &case.ada,
        BEA => &case.bea,
        _ => &case.cora,
    };
    let token = actor
        .session
        .access_token(teams_proto::IC3_AUDIENCE)
        .await
        .unwrap()
        .token;
    for (src, expected) in srcs.iter().zip(expected) {
        engine_read(actor, src, expected, upn != CORA).await;
        let response = case
            .node
            .http
            .get(src)
            .bearer_auth(token.expose())
            .send()
            .await
            .unwrap();
        if upn == CORA {
            assert_eq!(response.status(), 403);
        } else {
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["content-type"], "image/png");
            assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
        }
    }
}

async fn engine_read(actor: &Actor, src: &str, expected: &[u8], member: bool) {
    let shown = actor.engine().message_picture(src.to_owned()).await;
    if member {
        assert_eq!(shown.unwrap().unwrap().bytes.as_slice(), expected);
    } else {
        assert!(!matches!(shown, Ok(Some(_))));
    }
}
