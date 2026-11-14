//! Real engine and outbox operations against an own-origin synthetic service; no tenant is reached.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the shared diagnostic helpers also serve other integration tests"
)]
mod common;

use teams_core::engine::{Action, EngineHandle, Event, MessageChange};
use teams_mock::{AB_CHAT, ChatServiceMock, LAB_A, LAB_B, LAB_C};
use teams_proto::chat::MessageText;
use teams_proto::{Cloud, Mri};
use teams_store::OutboxState;

async fn changed(handle: &mut EngineHandle, chat: &str, id: &str, change: MessageChange) {
    common::event(handle, |event| match event {
        Event::Message {
            conversation,
            id: got,
            change: kind,
            ..
        } if conversation == chat && got == id && *kind == change => Some(()),
        _ => None,
    })
    .await;
}

async fn send(handle: &EngineHandle, chat: &str, text: &str) -> String {
    let row = handle
        .write(Action::Send {
            conversation: chat.into(),
            text: MessageText::plain(text).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent);
    row.message_id.unwrap()
}

#[tokio::test]
async fn own_engines_sync_send_edit_react_read_and_create_a_chat() {
    let cloud = Cloud::own("https://chat.example:8443").unwrap();
    let mock = ChatServiceMock::start_for_cloud(cloud.clone()).await.unwrap();
    mock.serve_presence(&[]).await;
    mock.serve_presence_subscriptions(&[]).await;
    let trouter = mock.start_trouter().unwrap();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let names = ["Lab A", "Lab B"];
    let mut a = common::native_for_cloud(&mock, &trouter, LAB_A, names, a_dir.path(), &cloud);
    let mut b = common::native_for_cloud(&mock, &trouter, LAB_B, names, b_dir.path(), &cloud);
    common::ready(&mut a).await;
    common::ready(&mut b).await;
    assert!(
        a.conversations(100)
            .await
            .unwrap()
            .iter()
            .any(|chat| chat.id == AB_CHAT)
    );
    assert!(a.sync().await.unwrap().failed.is_empty());

    chat_actions(&mock, &a, &mut b).await;
    create_chat(&a, &mut b).await;
    presence_subscription(&mock, &trouter, &mut a).await;
    a.shutdown();
    b.shutdown();
}

async fn chat_actions(mock: &ChatServiceMock, a: &EngineHandle, b: &mut EngineHandle) {
    let id = send(a, AB_CHAT, "own-server first draft").await;
    changed(b, AB_CHAT, &id, MessageChange::New).await;
    let edit = a
        .write(Action::Edit {
            conversation: AB_CHAT.into(),
            message_id: id.clone(),
            text: MessageText::plain("own-server second draft").unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(edit.state, OutboxState::Sent);
    changed(b, AB_CHAT, &id, MessageChange::Edit).await;
    assert!(
        b.message(AB_CHAT, &id)
            .await
            .unwrap()
            .unwrap()
            .content
            .contains("second draft")
    );
    react_and_read(mock, a, b, &id).await;
}

async fn react_and_read(mock: &ChatServiceMock, a: &EngineHandle, b: &mut EngineHandle, id: &str) {
    let reaction = a
        .write(Action::React {
            conversation: AB_CHAT.into(),
            message_id: id.to_owned(),
            key: "like".into(),
        })
        .await
        .unwrap();
    assert_eq!(reaction.state, OutboxState::Sent);
    changed(b, AB_CHAT, id, MessageChange::Reaction).await;
    assert_eq!(b.reactions(AB_CHAT, id).await.unwrap()[0].user, LAB_A);
    let read = b
        .write(Action::Read {
            conversation: AB_CHAT.into(),
            message_id: id.to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(read.state, OutboxState::Sent);
    assert!(
        mock.take_events(LAB_A)
            .iter()
            .any(|event| event.kind == teams_mock::EventKind::ReadReceipt && event.from == LAB_B)
    );
}

async fn create_chat(a: &EngineHandle, b: &mut EngineHandle) {
    let created = a
        .write(Action::CreateChat {
            others: vec![Mri::new(LAB_B), Mri::new(LAB_C)],
            topic: Some("Own chat".into()),
        })
        .await
        .unwrap();
    assert_eq!(created.state, OutboxState::Sent);
    let chat = created.message_id.unwrap();
    let first = send(a, &chat, "new own chat").await;
    changed(b, &chat, &first, MessageChange::New).await;
    assert!(b.conversations(100).await.unwrap().iter().any(|entry| entry.id == chat));
    assert!(b.sync().await.unwrap().failed.is_empty());
}

async fn presence_subscription(mock: &ChatServiceMock, trouter: &teams_mock::TrouterMock, a: &mut EngineHandle) {
    a.presence(LAB_B).await.unwrap();
    let deadline = tokio::time::Instant::now() + common::WAIT;
    loop {
        let requests = mock.received_requests().await;
        if let Some(request) = requests
            .iter()
            .find(|request| request.url.path().contains("/pubsub/subscriptions/"))
        {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert!(
                body["trouterUri"]
                    .as_str()
                    .unwrap()
                    .starts_with("https://chat.example:8443/trouter/v4/f/")
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no own-origin presence subscription"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    trouter.deliver_presence(
        LAB_A,
        &serde_json::json!({"presence": [{
            "mri": LAB_B, "etag": "synthetic-presence-change", "presence": {
                "sourceNetwork": "SameEnterprise", "calendarData": {"isOutOfOffice": false},
                "capabilities": [], "availability": "Busy", "activity": "InACall",
                "lastActiveTime": "2023-11-14T22:13:20.000Z", "deviceType": "Web",
            }
        }]}),
        false,
    );
    common::event(a, |event| match event {
        Event::Presence { mri, availability } if mri == LAB_B && *availability == teams_core::Availability::Busy => {
            Some(())
        }
        _ => None,
    })
    .await;
    assert_eq!(a.presence(LAB_B).await.unwrap(), teams_core::Availability::Busy);
}

#[tokio::test]
async fn own_engine_sends_and_views_a_picture_and_reads_a_file_link() {
    use teams_proto::chat::{Node, RichText};
    use teams_proto::{PastedFormat, PastedPicture, sent_picture_in};

    let cloud = Cloud::own("https://chat.example:8443").unwrap();
    let mock = ChatServiceMock::start_for_cloud(cloud.clone()).await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut a = common::native_for_cloud(&mock, &trouter, LAB_A, ["Lab A", "Lab B"], dir.path(), &cloud);
    common::ready(&mut a).await;
    let picture = PastedPicture {
        format: PastedFormat::Png,
        bytes: b"\x89PNG\r\n\x1a\nsynthetic picture".to_vec(),
    };
    mock.serve_picture_sends("synthetic-own-picture", picture.bytes.clone())
        .await;
    let object = a.upload_picture(AB_CHAT.into(), picture.clone()).await.unwrap();
    let image = sent_picture_in(&cloud, &object, (32, 24)).unwrap();
    let src = image.src.clone();
    refuse_foreign_picture(&a).await;
    let text = MessageText::new_in(
        &cloud,
        RichText {
            nodes: vec![Node::Image(image)],
            truncated: false,
        },
        [],
    )
    .unwrap();
    let row = a
        .write(Action::Send {
            conversation: AB_CHAT.into(),
            text,
        })
        .await
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent);
    let message = a.message(AB_CHAT, row.message_id.unwrap()).await.unwrap().unwrap();
    assert!(message.content.contains("https://chat.example:8443/ams/v1/objects/"));
    assert_eq!(*a.message_picture(&src).await.unwrap().unwrap(), picture);
    assert!(a.message_picture(src.replace(":8443", "")).await.unwrap().is_none());
    assert!(
        a.message_picture("https://us-api.asm.skype.com/v1/objects/x/views/imgo")
            .await
            .unwrap()
            .is_none()
    );
    let preview =
        teams_proto::LinkPreviewPicture::card_picture_src_in(&cloud, "https://page.example/picture.png").unwrap();
    mock.serve_bytes("/ams/urlp/v1/url/image/Thumbnail", "image/png", picture.bytes.clone())
        .await;
    assert_eq!(*a.message_picture(preview).await.unwrap().unwrap(), picture);
    own_file(&mock, &a, &cloud).await;
    own_upload(&mock, &a, dir.path()).await;
    let requests = mock.received_requests().await;
    assert!(
        requests
            .iter()
            .any(|request| request.url.path().starts_with("/ams/v1/objects/"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.url.path().starts_with("/graph/v1.0/shares/"))
    );
    for request in requests
        .iter()
        .filter(|request| request.url.path().starts_with("/sp/synthetic/"))
    {
        assert!(!request.headers.contains_key("authorization"));
    }
    a.shutdown();
}

async fn own_file(mock: &ChatServiceMock, a: &EngineHandle, cloud: &Cloud) {
    let link = "https://chat.example:8443/sp/personal/lab_a/Documents/demo.mp4";
    let file = teams_proto::FileDownload::of_in(cloud, link).unwrap();
    mock.answer_next("GET", &format!("/graph/v1.0/shares/{}/driveItem", file.share()), 200,
        &serde_json::json!({"size":14,"@microsoft.graph.downloadUrl":"https://chat.example:8443/sp/synthetic/content?tempauth=synthetic"})).await;
    mock.answer_next("GET", &format!("/graph/v1.0/shares/{}/driveItem/thumbnails", file.share()), 200,
        &serde_json::json!({"value":[{"large":{"url":"https://chat.example:8443/sp/synthetic/thumbnail?tempauth=synthetic"}}]})).await;
    mock.serve_ranged(
        "/sp/synthetic/content",
        "application/octet-stream",
        b"synthetic file".to_vec(),
    )
    .await;
    mock.serve_bytes(
        "/sp/synthetic/thumbnail",
        "image/png",
        b"\x89PNG\r\n\x1a\nsynthetic thumbnail".to_vec(),
    )
    .await;
    let files = teams_proto::chat::shared_files_in(
        cloud,
        Some(&serde_json::json!([{
            "@type":"http://schema.skype.com/File", "fileName":"demo.mp4", "state":"active", "objectUrl":link,
            "fileInfo":{"shareUrl":link}
        }])),
    );
    assert!(files.unread.is_none());
    assert_eq!(files.files[0].download_link().unwrap(), format!("{link}?download=1"));
    assert_eq!(a.file_size(link).await.unwrap(), 14);
    assert_eq!(a.file_range(link, 0, 14).await.unwrap().bytes, b"synthetic file");
    assert_eq!(
        a.file_thumbnail(link).await.unwrap().unwrap().format,
        teams_proto::PastedFormat::Png
    );
    assert!(a.file_size("https://outside.example/sp/demo.mp4").await.is_err());
}

async fn refuse_foreign_picture(a: &EngineHandle) {
    use teams_proto::chat::{Node, RichText};
    for cloud in [Cloud::Microsoft, Cloud::own("https://other.example").unwrap()] {
        let image = teams_proto::sent_picture_in(&cloud, "synthetic-picture", (32, 24)).unwrap();
        let text = MessageText::new_in(
            &cloud,
            RichText {
                nodes: vec![Node::Image(image)],
                truncated: false,
            },
            [],
        )
        .unwrap();
        assert!(
            a.write(Action::Send {
                conversation: AB_CHAT.into(),
                text
            })
            .await
            .is_err()
        );
    }
}

async fn own_upload(mock: &ChatServiceMock, a: &EngineHandle, dir: &std::path::Path) {
    use serde_json::json;

    let owner = LAB_A.strip_prefix("8:orgid:").unwrap();
    let site = "https://chat.example:8443/sp/personal/lab_a/";
    let drive = "/sp/personal/lab_a/_api/v2.0/drive";
    let details = json!({"owner":{"user":{"id":owner}}});
    let grants = json!({"value":[{"id":"owner","roles":["owner"],"grantedToV2":{"user":{"id":owner}}}]});
    let reads = [
        ("/graph/v1.0/me/drive".to_owned(), json!({"webUrl":site})),
        (
            "/graph/v1.0/me".to_owned(),
            json!({"id":owner,"userPrincipalName":"a@example.test"}),
        ),
        (drive.to_owned(), details.clone()),
        (drive.to_owned(), details),
        (
            format!("{drive}/root:/Microsoft%20Teams%20Chat%20Files"),
            json!({"id":"folder"}),
        ),
        (
            format!("{drive}/root:/Microsoft%20Teams%20Chat%20Files"),
            json!({"id":"folder"}),
        ),
        (
            format!("{drive}/root:/Microsoft%20Teams%20Chat%20Files:/permissions"),
            grants.clone(),
        ),
        (
            format!("{drive}/root:/Microsoft%20Teams%20Chat%20Files:/permissions"),
            grants,
        ),
    ];
    for (path, answer) in reads {
        mock.answer_next("GET", &path, 200, &answer).await;
    }
    mock.answer_next(
        "PUT",
        "/content",
        201,
        &json!({
            "id":"ABC1","name":"demo.txt","size":3,"webUrl":format!("{site}Documents/demo.txt"),
            "parentReference":{"driveId":"drive1"},
            "sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}
        }),
    )
    .await;
    let path = dir.join("demo.txt");
    std::fs::write(&path, b"abc").unwrap();
    let file = a.upload_file(path).await.unwrap();
    assert_eq!(file.name(), "demo.txt");
    assert_eq!(file.size(), 3);
    assert!(file.item_url().starts_with(site));
}
