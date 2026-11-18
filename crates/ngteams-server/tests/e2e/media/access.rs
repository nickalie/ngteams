//! Who reaches a drive, an object and a file's links; and two nodes over one database and one store.

use base64::Engine as _;
use reqwest::Method;
use serde_json::{Value, json};
use teams_proto::{CreatePicture, FileDownload, PastedFormat, PastedPicture, UploadPicture};

use super::files::send_file;
use super::{GRAPH, IC3, ORIGIN, Trio, client, cloud, engine, png, raw, status, token};
use crate::support::{Backend, TestServer, backends};

fn site(id: &str) -> String {
    format!("/sp/personal/{}", id.replace('-', ""))
}

/// The file's item as Graph answers `upn`: its id and the path and query of its download link.
async fn item(server: &TestServer, upn: &str, share: &str) -> (String, String) {
    let read = FileDownload::of_in(&cloud(), share).unwrap();
    let graph = token(server, upn, GRAPH).await;
    let path = format!("/graph/v1.0/shares/{}/driveItem", read.share());
    let (status, _, body) = raw(server, Method::GET, &path, Some(&graph), &[], None).await;
    assert_eq!(status, 200);
    let item: Value = serde_json::from_slice(&body).unwrap();
    let link = item["@microsoft.graph.downloadUrl"].as_str().unwrap();
    (
        item["sharepointIds"]["listItemUniqueId"].as_str().unwrap().to_owned(),
        link.strip_prefix(ORIGIN).unwrap().to_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drive_and_an_object_answer_only_their_owner_and_the_people_shared_with() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let [a, b, _] = &trio.ids;
    let share = send_file(&trio, "report.png", &png(1200, 600)).await;
    let (id, _) = item(server, "b@own.test", &share).await;
    let drive = format!("{}/_api/v2.0/drive", site(a));
    let own = token(server, "a@own.test", ORIGIN).await;
    let others = token(server, "b@own.test", ORIGIN).await;
    let get =
        |path: String, token: String| async move { raw(server, Method::GET, &path, Some(&token), &[], None).await.0 };
    assert_eq!(get(drive.clone(), own.clone()).await, 200);
    assert_eq!(get(drive.clone(), others.clone()).await, 403);
    assert_eq!(get(drive.clone(), token(server, "a@own.test", GRAPH).await).await, 401);
    assert_eq!(get(format!("{}/_api/v2.0/drive", site(b)), others.clone()).await, 200);
    assert_eq!(
        get(format!("{}/_api/v2.0/drive/items", site(a)), own.clone()).await,
        404
    );
    let permissions = format!("{}/_api/v2.0/sites/root/items/{id}/driveItem/permissions", site(a));
    assert_eq!(get(permissions.clone(), own.clone()).await, 200);
    assert_eq!(get(permissions.replace(&site(a), &site(b)), others.clone()).await, 404);

    let invite = |body: Value| {
        let path = format!("{}/_api/v2.0/sites/root/items/{id}/driveItem/invite", site(a));
        let own = own.clone();
        async move {
            let headers = [("content-type", "application/json")];
            raw(
                server,
                Method::POST,
                &path,
                Some(&own),
                &headers,
                Some(body.to_string().into_bytes()),
            )
            .await
            .0
        }
    };
    let to = |email: &str, sign_in: bool| json!({"recipients": [{"email": email}], "roles": ["write"], "requireSignIn": sign_in, "sendInvitation": false});
    assert_eq!(invite(to("nobody@own.test", true)).await, 400);
    assert_eq!(invite(to("c@own.test", false)).await, 400);
    assert_eq!(
        invite(json!({"recipients": [], "requireSignIn": true, "sendInvitation": false})).await,
        400
    );
    assert_eq!(invite(to("C@own.test", true)).await, 200);
    let outsider = trio.outsider().await;
    let read = FileDownload::of_in(&cloud(), &share).unwrap();
    assert!(outsider.read(&read).await.is_ok());
    let (_, handed) = item(server, "c@own.test", &share).await;
    let thumbnails = format!("/graph/v1.0/shares/{}/driveItem/thumbnails", read.share());
    let graph = token(server, "c@own.test", GRAPH).await;
    let (_, _, sizes) = raw(server, Method::GET, &thumbnails, Some(&graph), &[], None).await;
    let sizes: Value = serde_json::from_slice(&sizes).unwrap();
    let picture = sizes["value"][0]["large"]["url"].as_str().unwrap();
    let picture = picture.strip_prefix(ORIGIN).unwrap().to_owned();
    let open = |link: String| async move { raw(server, Method::GET, &link, None, &[], None).await.0 };
    assert_eq!((open(handed.clone()).await, open(picture.clone()).await), (200, 200));

    let revoke = format!("{permissions}/{}", trio.ids[2]);
    assert_eq!(raw(server, Method::DELETE, &revoke, Some(&own), &[], None).await.0, 204);
    assert_eq!(raw(server, Method::DELETE, &revoke, Some(&own), &[], None).await.0, 404);
    assert_eq!(status(&outsider.read(&read).await.unwrap_err()), Some(403));
    assert_eq!((open(handed).await, open(picture).await), (403, 403));
    let owner = format!("{permissions}/{a}");
    assert_eq!(raw(server, Method::DELETE, &owner, Some(&own), &[], None).await.0, 404);

    let graph = token(server, "b@own.test", GRAPH).await;
    for (share, expected) in [("u!bm90IGEgbGluaw", 404), ("not-a-token", 400)] {
        let path = format!("/graph/v1.0/shares/{share}/driveItem");
        assert_eq!(
            raw(server, Method::GET, &path, Some(&graph), &[], None).await.0,
            expected
        );
    }
    let elsewhere = FileDownload::of_in(&cloud(), &share.replace("report.png", "other.png")).unwrap();
    let path = format!("/graph/v1.0/shares/{}/driveItem", elsewhere.share());
    assert_eq!(raw(server, Method::GET, &path, Some(&graph), &[], None).await.0, 404);

    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_object_is_made_for_the_makers_chats_filled_by_its_maker_and_read_once_filled() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let [a, b, _] = &trio.ids;
    let outsider = trio.outsider().await;
    let writer = client(server, "a@own.test", a).await;
    let made = writer
        .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
        .await
        .unwrap();
    let object = CreatePicture::read_answer(made.body()).unwrap();
    let fill = || {
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: png(4, 4),
        };
        UploadPicture::new(&object, &picture).unwrap()
    };
    let member = client(server, "b@own.test", b).await;
    assert_eq!(member.write(fill()).await.unwrap().status(), 403);
    let made = outsider
        .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
        .await
        .unwrap();
    assert_eq!(made.status(), 403);
    let ic3 = token(server, "b@own.test", IC3).await;
    let view = format!("/ams/v1/objects/{object}/views/imgo");
    assert_eq!(raw(server, Method::GET, &view, Some(&ic3), &[], None).await.0, 404);
    assert_eq!(writer.write(fill()).await.unwrap().status(), 201);
    assert_eq!(raw(server, Method::GET, &view, Some(&ic3), &[], None).await.0, 200);
    let unknown = view.replace("imgo", "imgt1");
    assert_eq!(raw(server, Method::GET, &unknown, Some(&ic3), &[], None).await.0, 404);
    let (status, _, body) = raw(
        server,
        Method::GET,
        &format!("/ams/v1/objects/{object}"),
        Some(&ic3),
        &[],
        None,
    )
    .await;
    let read: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((status, read["type"].as_str()), (200, Some("pish/image")));
    assert!(read["contents"]["imgpsh"]["length"].as_u64().unwrap() > 0);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_link_serves_ranges_and_only_with_its_own_credential() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let bytes = b"0123456789".to_vec();
    let share = send_file(&trio, "digits.html", &bytes).await;
    let other = send_file(&trio, "other.txt", b"other").await;
    let (id, link) = item(server, "b@own.test", &share).await;
    let (other_id, other_link) = item(server, "b@own.test", &other).await;
    let get = |path: String, range: Option<&'static str>| async move {
        let headers: Vec<(&str, &str)> = range.map(|range| ("range", range)).into_iter().collect();
        raw(server, Method::GET, &path, None, &headers, None).await
    };

    let (status, headers, body) = get(link.clone(), None).await;
    assert_eq!((status, body.as_slice()), (200, bytes.as_slice()));
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["accept-ranges"], "bytes");
    for (range, expected, content_range) in [
        ("bytes=2-5", &b"2345"[..], "bytes 2-5/10"),
        ("bytes=7-", b"789", "bytes 7-9/10"),
        ("bytes=-3", b"789", "bytes 7-9/10"),
        ("bytes=8-200", b"89", "bytes 8-9/10"),
    ] {
        let (status, headers, body) = get(link.clone(), Some(range)).await;
        assert_eq!((status, body.as_slice()), (206, expected), "{range}");
        assert_eq!(headers["content-range"], content_range);
    }
    let (status, headers, _) = get(link.clone(), Some("bytes=10-")).await;
    assert_eq!(
        (status, headers["content-range"].to_str().unwrap()),
        (416, "bytes */10")
    );
    assert_eq!(get(link.clone(), Some("lines=1-2")).await.0, 416);
    let (status, _, body) = get(link.clone(), Some("bytes=0-1,4-5")).await;
    assert_eq!((status, body.len()), (200, 10));

    let bare = format!("/sp/download/{id}");
    assert_eq!(get(bare.clone(), None).await.0, 401);
    assert_eq!(get(format!("{bare}?tempauth=made-up"), None).await.0, 401);
    let swapped = other_link.replace(&other_id, &id);
    assert_eq!(get(swapped, None).await.0, 401);
    let credential = link.split_once("tempauth=").unwrap().1;
    assert_eq!(
        get(format!("/sp/thumbnail/{id}/large?tempauth={credential}"), None)
            .await
            .0,
        401
    );
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_link_ends_with_its_lifetime_and_the_engine_asks_for_another() {
    let trio = Trio::start_with(Backend::Sqlite, |config| config.media.link_lifetime = 1).await;
    let share = send_file(&trio, "short.txt", b"short lived").await;
    let (_, link) = item(&trio.server, "b@own.test", &share).await;
    assert_eq!(trio.b.file_size(share.clone()).await.unwrap(), 11);
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(raw(&trio.server, Method::GET, &link, None, &[], None).await.0, 401);
    assert_eq!(trio.b.file_range(share, 0, 5).await.unwrap().bytes, b"short");
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_account_and_a_token_from_before_a_password_reset_open_no_media_route_and_no_link() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let (app, [_, b, _]) = (&server.app, &trio.ids);
    let share = send_file(&trio, "report.txt", b"0123456789").await;
    let (id, readers) = item(server, "b@own.test", &share).await;
    let (_, senders) = item(server, "a@own.test", &share).await;
    let thumbnail = format!("/sp/thumbnail/{id}/large?tempauth=");
    let routes = [
        (
            "/graph/v1.0/me/drive".to_owned(),
            token(server, "b@own.test", GRAPH).await,
        ),
        (
            format!("{}/_api/v2.0/drive", site(b)),
            token(server, "b@own.test", ORIGIN).await,
        ),
        (
            "/ams/v1/objects/0-none-d1-00000000000000000000000000000000".to_owned(),
            token(server, "b@own.test", IC3).await,
        ),
    ];
    let get = |path: String, token: Option<String>| async move {
        raw(server, Method::GET, &path, token.as_deref(), &[], None).await.0
    };
    let answers = || async {
        let mut answers = vec![get(readers.clone(), None).await, get(senders.clone(), None).await];
        for (path, token) in &routes {
            answers.push(get(path.clone(), Some(token.clone())).await);
        }
        answers
    };
    assert_eq!(answers().await, [200, 200, 200, 200, 404]);

    ngteams_server::admin::reset_password(&app.db, &*app.fanout, "b@own.test", "another horse battery")
        .await
        .unwrap();
    assert_eq!(answers().await, [401, 200, 401, 401, 401]);
    let credential = readers.split_once("tempauth=").unwrap().1;
    assert_ne!(get(format!("{thumbnail}{credential}"), None).await, 200);

    ngteams_server::admin::set_disabled(&app.db, &*app.fanout, "a@own.test", true)
        .await
        .unwrap();
    assert_eq!(get(senders.clone(), None).await, 401);
    ngteams_server::admin::set_disabled(&app.db, &*app.fanout, "a@own.test", false)
        .await
        .unwrap();
    assert_eq!(get(senders, None).await, 200);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_share_files_pictures_and_link_credentials() {
    for backend in backends() {
        let (first, blobs) = super::server(backend, |_| {}).await;
        let shared = first.app.config.clone();
        let second = TestServer::start_with(backend, |config| {
            config.public_origin = shared.public_origin.clone();
            config.database_url = shared.database_url.clone();
            config.storage_url = shared.storage_url.clone();
            config.storage_options = shared.storage_options.clone();
        })
        .await;
        let a = first.add_user("a@own.test", "Ada").await;
        let b = first.add_user("b@own.test", "Bert").await;
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let sender = engine(&first, "a@own.test", &a, dirs[0].path()).await;
        let reader = engine(&second, "b@own.test", &b, dirs[1].path()).await;
        let row = sender
            .write(teams_core::engine::Action::CreateChat {
                others: vec![teams_proto::Mri::new(format!("8:orgid:{b}"))],
                topic: None,
            })
            .await
            .unwrap();
        let chat = row.message_id.unwrap();
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: png(20, 10),
        };
        let object = sender.upload_picture(chat.clone(), picture.clone()).await.unwrap();
        let src = teams_proto::sent_picture_in(&cloud(), &object, (20, 10)).unwrap().src;
        assert_eq!(reader.message_picture(src).await.unwrap().unwrap().bytes, picture.bytes);

        let path = dirs[0].path().join("shared.png");
        std::fs::write(&path, png(300, 200)).unwrap();
        let file = sender.upload_file(path).await.unwrap();
        let entry = file.entry();
        let share = entry["objectUrl"].as_str().unwrap().to_owned();
        let text = teams_proto::chat::MessageText::plain("on two nodes")
            .unwrap()
            .with_files(vec![file])
            .for_cloud(&cloud())
            .unwrap();
        let sent = sender
            .write(teams_core::engine::Action::Send {
                conversation: chat,
                text,
            })
            .await
            .unwrap();
        assert_eq!(sent.state, teams_store::OutboxState::Sent, "{sent:?}");
        assert!(reader.file_thumbnail(share.clone()).await.unwrap().is_some());
        assert_eq!(reader.file_range(share.clone(), 1, 3).await.unwrap().bytes, b"PNG");
        // a link the second node signed opens on the first
        let (_, link) = item(&second, "b@own.test", &share).await;
        assert_eq!(raw(&first, Method::GET, &link, None, &[], None).await.0, 200);
        sender.shutdown();
        reader.shutdown();
        second.stop().await;
        first.stop().await;
        drop(blobs);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_is_no_picture_has_no_thumbnail_and_odd_links_name_nothing() {
    let trio = Trio::start(Backend::Sqlite).await;
    let server = &trio.server;
    let share = send_file(&trio, "notes.txt", b"plain words").await;
    assert!(trio.b.file_thumbnail(share.clone()).await.unwrap().is_none());
    // asked again, the note of what was made answers
    let read = teams_proto::FileThumbnails::of_in(&cloud(), &share).unwrap();
    let graph = token(server, "b@own.test", GRAPH).await;
    let path = format!("/graph/v1.0/shares/{}/driveItem/thumbnails", read.share());
    let (status, _, body) = raw(server, Method::GET, &path, Some(&graph), &[], None).await;
    assert_eq!(
        (status, serde_json::from_slice::<Value>(&body).unwrap()),
        (200, json!({"value": []}))
    );

    let (id, link) = item(server, "b@own.test", &share).await;
    let credential = link.split_once("tempauth=").unwrap().1;
    let thumbnail = format!("/sp/thumbnail/{id}/large?tempauth={credential}");
    assert_eq!(raw(server, Method::GET, &thumbnail, None, &[], None).await.0, 401);

    let token_of = |link: &str| format!("u!{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(link));
    for link in [
        "https://elsewhere.example/sp/personal/x/Documents/Microsoft%20Teams%20Chat%20Files/notes.txt".to_owned(),
        format!("{share}?download=1"),
        format!("{ORIGIN}/sp/personal/not-an-account/Documents/Microsoft%20Teams%20Chat%20Files/notes.txt"),
        share.replace("Documents", "Other"),
        format!("{ORIGIN}/sp/personal"),
    ] {
        let path = format!("/graph/v1.0/shares/{}/driveItem", token_of(&link));
        assert_eq!(
            raw(server, Method::GET, &path, Some(&graph), &[], None).await.0,
            404,
            "{link}"
        );
    }
    let (status, _, body) = raw(server, Method::GET, "/graph/v1.0/me/drive", Some(&graph), &[], None).await;
    let drive: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (status, drive["owner"]["user"]["id"].as_str()),
        (200, Some(trio.ids[1].as_str()))
    );
    let user = format!("/graph/v1.0/users/{}", trio.ids[0]);
    assert_eq!(raw(server, Method::GET, &user, Some(&graph), &[], None).await.0, 200);
    let nobody = "/graph/v1.0/users/00000000-0000-4000-8000-000000000000";
    assert_eq!(raw(server, Method::GET, nobody, Some(&graph), &[], None).await.0, 404);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_who_left_a_chat_cannot_read_its_picture() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let row = trio
            .a
            .write(teams_core::engine::Action::CreateChat {
                others: trio.ids[1..]
                    .iter()
                    .map(|id| teams_proto::Mri::new(format!("8:orgid:{id}")))
                    .collect(),
                topic: Some("Pictures (synthetic)".into()),
            })
            .await
            .unwrap();
        let chat = row.message_id.unwrap();
        let writer = client(&trio.server, "a@own.test", &trio.ids[0]).await;
        let made = writer
            .write(CreatePicture::new(&chat, PastedFormat::Png).unwrap())
            .await
            .unwrap();
        let object = CreatePicture::read_answer(made.body()).unwrap();
        let picture = PastedPicture {
            format: PastedFormat::Png,
            bytes: png(4, 4),
        };
        assert_eq!(
            writer
                .write(UploadPicture::new(&object, &picture).unwrap())
                .await
                .unwrap()
                .status(),
            201
        );
        let ic3 = token(&trio.server, "b@own.test", IC3).await;
        let path = format!("/ams/v1/objects/{object}/views/imgo");
        assert_eq!(
            raw(&trio.server, Method::GET, &path, Some(&ic3), &[], None).await.0,
            200
        );
        let remove = crate::chatsvc::members::RemoveMember {
            chat,
            member: format!("8:orgid:{}", trio.ids[1]),
        };
        assert_eq!(writer.write(remove).await.unwrap().status(), 200);
        assert_eq!(
            raw(&trio.server, Method::GET, &path, Some(&ic3), &[], None).await.0,
            403
        );
        trio.stop().await;
    }
}
