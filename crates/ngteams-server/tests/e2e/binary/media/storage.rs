//! The blob store, chosen by configuration alone: pictures, files and thumbnails are objects under the configured
//! prefix, and a store that can't be reached fails uploads without taking the chat down.

use std::time::{Duration, Instant};

use teams_core::engine::MessageChange;
use teams_proto::{PastedFormat, PastedPicture};

use super::super::push::support::Rig;
use super::support::{Stage, card, paste, people, same, send_file, try_upload};
use crate::media::png;
use crate::support::{Backend, backends};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pictures_files_and_thumbnails_are_objects_of_the_configured_store() {
    for backend in backends() {
        let stage = Stage::start(backend, "").await;
        let (a, mut b, c, chat) = stage.people().await;
        let (picture, photo) = (png(64, 48), png(1200, 600));
        paste(&a, &stage.cloud, &chat, &picture, (64, 48)).await;
        let id = send_file(&a, &stage.cloud, &chat, "photo.png", &photo).await;
        let share = card(&mut b, &stage.cloud, &chat, &id).await.share_url.unwrap();
        assert!(b.engine().file_thumbnail(share).await.unwrap().is_some());

        let kept = stage.blobs.keys().await;
        let of = |prefix: &str| {
            kept.iter()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect::<Vec<_>>()
        };
        let (pictures, files, thumbnails) = (of("ams/"), of("drive/"), of("drive-thumbnails/"));
        assert_eq!((pictures.len(), files.len()), (1, 1), "{kept:?}");
        assert!(!thumbnails.is_empty(), "{kept:?}");
        assert_eq!(kept.len(), 2 + thumbnails.len(), "{kept:?}");
        assert!(same(&stage.blobs.bytes(&pictures[0]).await, &picture));
        assert!(same(&stage.blobs.bytes(&files[0]).await, &photo));
        drop((a, b, c));
        stage.stop().await;
    }
}

fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_the_bucket_out_of_reach_uploads_fail_and_the_chat_goes_on() {
    let root = "storage_url = \"s3://ngts-nowhere/qa\"\n";
    let tables = format!(
        "[storage_options]\naws_endpoint = \"http://127.0.0.1:{}\"\naws_region = \"us-east-1\"\naws_access_key_id = \
         \"none\"\naws_secret_access_key = \"none\"\naws_allow_http = \"true\"\naws_virtual_hosted_style_request = \
         \"false\"\n",
        closed_port()
    );
    let rig = Rig::start_stored(Backend::Sqlite, "", None, Some((root, &tables))).await;
    let (a, mut b, c, chat) = people(&rig).await;

    let asked = Instant::now();
    let picture = PastedPicture {
        format: PastedFormat::Png,
        bytes: png(8, 8),
    };
    let pasted = a.engine().upload_picture(chat.clone(), picture).await;
    let pasted = format!("{:?}", pasted.unwrap_err());
    let uploaded = try_upload(&a, "lost.bin", b"bytes").await.map(|_| ()).unwrap_err();
    eprintln!(
        "bucket out of reach: {:?}\n  picture: {pasted}\n  file: {uploaded}",
        asked.elapsed()
    );
    assert!(pasted.contains("503"), "{pasted}");
    assert!(uploaded.contains("isn't available now"), "{uploaded}");
    assert!(asked.elapsed() < Duration::from_secs(8), "{:?}", asked.elapsed());

    let id = a.send(&chat, "the chat goes on").await;
    b.pushed(0, &chat, &id, MessageChange::New).await;
    b.shows(&chat, &id, "the chat goes on").await;
    assert_eq!(rig.first.status("/health/live").await, Some(204));
    drop((a, b, c));
    rig.stop().await;
}
