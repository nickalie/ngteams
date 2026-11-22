//! The storage limits of the configuration: each picture's and file's size, and what one account may store.

use teams_proto::{CreatePicture, PastedFormat, PastedPicture, UploadPicture};

use super::files::send_file;
use super::{Trio, client, png};
use crate::support::Backend;

fn picture(bytes: Vec<u8>) -> PastedPicture {
    PastedPicture {
        format: PastedFormat::Png,
        bytes,
    }
}

async fn upload(trio: &Trio, name: &str, bytes: &[u8]) -> Result<u64, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    trio.a
        .upload_file(path)
        .await
        .map(|file| file.size())
        .map_err(|error| format!("{error:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_stores_no_more_than_its_quota() {
    let small = png(8, 8);
    let quota = small.len() as u64 + 3000;
    let trio = Trio::start_with(Backend::Sqlite, |config| config.media.user_quota_bytes = quota).await;
    trio.a
        .upload_picture(trio.chat.clone(), picture(small.clone()))
        .await
        .unwrap();
    let refused = upload(&trio, "large.bin", &vec![7; 3001]).await.unwrap_err();
    assert!(refused.contains("enough storage space"), "{refused}");
    assert_eq!(upload(&trio, "fits.bin", &vec![7; 2000]).await, Ok(2000));
    let refused = trio
        .a
        .upload_picture(trio.chat.clone(), picture(png(64, 64)))
        .await
        .unwrap_err();
    assert!(format!("{refused:?}").contains("507"), "{refused:?}");
    assert_eq!(upload(&trio, "rest.bin", &vec![7; 1000]).await, Ok(1000));
    let refused = upload(&trio, "one-more.bin", b"x").await.unwrap_err();
    assert!(refused.contains("enough storage space"), "{refused}");
    // the other account's storage is its own
    let share = send_file(&trio, "sent.bin", b"").await;
    assert_eq!(trio.b.file_size(share).await.unwrap(), 0);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_filled_many_times_at_once_counts_its_bytes_once() {
    use reqwest::Method;

    let (large, small) = (png(120, 120), png(8, 8));
    let quota = large.len() as u64;
    for backend in crate::support::backends() {
        let trio = Trio::start_with(backend, |config| config.media.user_quota_bytes = quota).await;
        let writer = client(&trio.server, "a@own.test", &trio.ids[0]).await;
        let made = writer
            .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
            .await
            .unwrap();
        let object = CreatePicture::read_answer(made.body()).unwrap();
        let ic3 = super::token(&trio.server, "a@own.test", super::IC3).await;
        let path = format!("/ams/v1/objects/{object}/content/imgpsh");
        let fill = |bytes: &[u8]| {
            let filled = super::raw(&trio.server, Method::PUT, &path, Some(&ic3), &[], Some(bytes.to_vec()));
            async { filled.await.0 }
        };

        let first = tokio::join!(fill(&large), fill(&large), fill(&large), fill(&large));
        assert_eq!(
            first,
            (201, 201, 201, 201),
            "the same bytes were counted more than once"
        );
        for _ in 0..4 {
            let replaced = tokio::join!(fill(&small), fill(&small), fill(&small), fill(&small));
            assert_eq!(replaced, (201, 201, 201, 201));
            assert_eq!(fill(&large).await, 201);
        }
        assert_eq!(fill(&small).await, 201);
        let rest = large.len() - small.len();
        assert_eq!(upload(&trio, "rest.bin", &vec![7; rest]).await, Ok(rest as u64));
        let refused = upload(&trio, "one-more.bin", b"x").await.unwrap_err();
        assert!(refused.contains("enough storage space"), "{refused}");
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_filled_with_different_bytes_at_once_serves_the_bytes_it_counts() {
    use reqwest::Method;

    let (large, small) = (png(120, 120), png(8, 8));
    let quota = large.len() as u64;
    for backend in crate::support::backends() {
        let trio = Trio::start_with(backend, |config| config.media.user_quota_bytes = quota).await;
        let writer = client(&trio.server, "a@own.test", &trio.ids[0]).await;
        let made = writer
            .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
            .await
            .unwrap();
        let object = CreatePicture::read_answer(made.body()).unwrap();
        let ic3 = super::token(&trio.server, "a@own.test", super::IC3).await;
        let path = format!("/ams/v1/objects/{object}/content/imgpsh");
        let fill = |bytes: &[u8]| {
            let filled = super::raw(&trio.server, Method::PUT, &path, Some(&ic3), &[], Some(bytes.to_vec()));
            async { filled.await.0 }
        };
        let get = async |path: String| {
            super::raw(&trio.server, Method::GET, &path, Some(&ic3), &[], None)
                .await
                .2
        };

        let mut counted = 0;
        for _ in 0..12 {
            let filled = tokio::join!(fill(&large), fill(&small), fill(&small), fill(&large), fill(&small));
            assert_eq!(filled, (201, 201, 201, 201, 201));
            let described: serde_json::Value =
                serde_json::from_slice(&get(format!("/ams/v1/objects/{object}/")).await).unwrap();
            counted = usize::try_from(described["contents"]["imgpsh"]["length"].as_u64().unwrap()).unwrap();
            let served = get(format!("/ams/v1/objects/{object}/views/imgo")).await;
            assert!(
                served == large || served == small,
                "the picture is neither fill's bytes"
            );
            assert_eq!(served.len(), counted, "the object describes other bytes than it serves");
        }
        let rest = large.len() - counted;
        assert_eq!(upload(&trio, "rest.bin", &vec![7; rest]).await, Ok(rest as u64));
        let refused = upload(&trio, "one-more.bin", b"x").await.unwrap_err();
        assert!(refused.contains("enough storage space"), "{refused}");
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pictures_and_files_over_their_size_limits_are_refused() {
    let large = png(120, 120);
    let limit = large.len() as u64 - 1;
    let trio = Trio::start_with(Backend::Sqlite, |config| {
        config.media.max_picture_bytes = limit;
        config.media.max_file_bytes = 3000;
    })
    .await;
    let refused = trio
        .a
        .upload_picture(trio.chat.clone(), picture(large))
        .await
        .unwrap_err();
    assert!(format!("{refused:?}").contains("413"), "{refused:?}");
    let refused = upload(&trio, "large.bin", &vec![1; 3001]).await.unwrap_err();
    assert!(refused.contains("too large to upload"), "{refused}");
    assert_eq!(upload(&trio, "fits.bin", &vec![1; 3000]).await, Ok(3000));

    let writer = client(&trio.server, "a@own.test", &trio.ids[0]).await;
    let made = writer
        .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
        .await
        .unwrap();
    let object = CreatePicture::read_answer(made.body()).unwrap();
    let text = UploadPicture::new(&object, &picture(b"<html>not a picture</html>".to_vec())).unwrap();
    assert_eq!(writer.write(text).await.unwrap().status(), 415);
    trio.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_store_that_fails_answers_unavailable_and_keeps_no_trace_of_the_bytes() {
    use reqwest::Method;

    let blobs = tempfile::tempdir().unwrap();
    let root = blobs.path().join("store");
    let url = format!("file://{}", root.display());
    let trio = Trio::start_with(Backend::Sqlite, |config| {
        config.storage_url = url;
        config.storage_options.clear();
        config.media.user_quota_bytes = 0;
    })
    .await;
    let writer = client(&trio.server, "a@own.test", &trio.ids[0]).await;
    let made = writer
        .write(CreatePicture::new(&trio.chat, PastedFormat::Png).unwrap())
        .await
        .unwrap();
    let object = CreatePicture::read_answer(made.body()).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::write(&root, b"not a directory").unwrap();

    let fill = UploadPicture::new(&object, &picture(png(8, 8))).unwrap();
    assert_eq!(writer.write(fill).await.unwrap().status(), 503);
    let ic3 = super::token(&trio.server, "a@own.test", super::IC3).await;
    let path = format!("/ams/v1/objects/{object}/");
    let (status, _, body) = super::raw(&trio.server, Method::GET, &path, Some(&ic3), &[], None).await;
    let read: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((status, &read["contents"]), (200, &serde_json::json!({})));
    let refused = upload(&trio, "lost.bin", b"bytes").await.unwrap_err();
    assert!(refused.contains("isn't available now"), "{refused}");
    trio.stop().await;
}
