//! The configured limits: a picture's and a file's size and what one account may store. What is refused, or
//! given up half-way, leaves nothing in the store.

use teams_core::engine::Action;
use teams_proto::{CreatePicture, PastedFormat, PastedPicture, UploadPicture};

use super::super::push::support::User;
use reqwest::Method;

use super::support::{
    IC3, MIB, Stage, abandon_upload, card, drive, get, item, noise, paste, raw, send_file, streamed_upload, token,
    try_upload, upload_path,
};
use crate::media::png;
use crate::support::backends;

fn picture(bytes: Vec<u8>) -> PastedPicture {
    PastedPicture {
        format: PastedFormat::Png,
        bytes,
    }
}

/// The status the engine's upload of a file was refused with.
async fn refused(user: &User, name: &str, bytes: &[u8]) -> String {
    try_upload(user, name, bytes).await.map(|_| ()).unwrap_err()
}

async fn pasting_fails_with(user: &User, chat: &str, bytes: Vec<u8>, status: &str) {
    let failed = user.engine().upload_picture(chat.to_owned(), picture(bytes)).await;
    let failed = format!("{:?}", failed.unwrap_err());
    assert!(failed.contains(status), "{failed}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_picture_or_file_over_its_size_and_an_upload_given_up_leave_nothing_in_the_store() {
    let large = png(120, 120);
    let config = format!(
        "[media]\nmax_picture_bytes = {}\nmax_file_bytes = {}\n",
        large.len() - 1,
        8 * MIB
    );
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, b, c, chat) = stage.people().await;
        let node = &stage.rig.first;
        assert_eq!(stage.blobs.keys().await, Vec::<String>::new());

        pasting_fails_with(&a, &chat, large.clone(), "413").await;
        let writer = a.client();
        let made = writer.write(CreatePicture::new(&chat, PastedFormat::Png).unwrap());
        let object = CreatePicture::read_answer(made.await.unwrap().body()).unwrap();
        let text = UploadPicture::new(&object, &picture(b"<html>not a picture</html>".to_vec())).unwrap();
        assert_eq!(writer.write(text).await.unwrap().status(), 415);
        assert_eq!(
            stage.blobs.keys().await,
            Vec::<String>::new(),
            "a refused picture was kept"
        );

        let over = noise(8 * MIB + 1, 1);
        let said = refused(&a, "over.bin", &over).await;
        assert!(said.contains("too large to upload"), "{said}");
        let own = token(&a, &stage.rig.origin).await;
        let status = streamed_upload(node, &upload_path(&a, "streamed.bin"), &own, &over).await;
        assert_eq!(status, 413);
        abandon_upload(node, &upload_path(&a, "given-up.bin"), &own, 7 * MIB, &over[..6 * MIB]).await;
        assert_eq!(
            stage.blobs.keys().await,
            Vec::<String>::new(),
            "a refused file was kept"
        );

        let fits = try_upload(&a, "fits.bin", &over[..8 * MIB]).await.unwrap();
        assert_eq!(fits.size(), 8 * MIB as u64);
        let kept = stage.blobs.keys().await;
        assert!(kept.len() == 1 && kept[0].starts_with("drive/"), "{kept:?}");
        drop((a, b, c));
        stage.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_at_its_quota_is_refused_the_next_upload_and_the_others_keep_theirs() {
    let small = png(8, 8);
    let config = format!("[media]\nuser_quota_bytes = {}\n", small.len() + 3000);
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, b, c, chat) = stage.people().await;
        let node = &stage.rig.first;
        paste(&a, &stage.cloud, &chat, &small, (8, 8)).await;

        let said = refused(&a, "large.bin", &[7; 3001]).await;
        assert!(said.contains("enough storage space"), "{said}");
        let own = token(&a, &stage.rig.origin).await;
        let status = streamed_upload(node, &upload_path(&a, "streamed.bin"), &own, &[7; 3001]).await;
        assert_eq!(status, 507);
        abandon_upload(node, &upload_path(&a, "given-up.bin"), &own, 2500, &[7; 1000]).await;
        assert_eq!(stage.blobs.keys().await.len(), 1, "a refused upload was kept");

        assert_eq!(try_upload(&a, "fits.bin", &[7; 2000]).await.unwrap().size(), 2000);
        pasting_fails_with(&a, &chat, png(64, 64), "507").await;
        assert_eq!(try_upload(&a, "rest.bin", &[7; 1000]).await.unwrap().size(), 1000);
        let said = refused(&a, "one-more.bin", b"x").await;
        assert!(said.contains("enough storage space"), "{said}");

        assert_eq!(try_upload(&b, "theirs.bin", &[7; 2000]).await.unwrap().size(), 2000);
        let kept = stage.blobs.keys().await;
        let (pictures, files) = kept.iter().partition::<Vec<_>, _>(|key| key.starts_with("ams/"));
        assert_eq!((pictures.len(), files.len()), (1, 3), "{kept:?}");
        drop((a, b, c));
        stage.stop().await;
    }
}

/// The two kinds of an account's stored keys: its pictures and its files.
async fn kept(stage: &Stage) -> (usize, usize) {
    let keys = stage.blobs.keys().await;
    let of = |kind: &str| keys.iter().filter(|key| key.starts_with(kind)).count();
    (of("ams/"), of("drive/"))
}

/// A picture's bytes come back to its owner's quota when the last message that shows it is deleted, or its object
/// is; a file stays in its owner's drive when its message is deleted, as in Teams, and comes back when the owner
/// deletes it there. What comes back has left the store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_at_its_quota_uploads_again_after_it_deleted_what_it_sent() {
    let small = png(8, 8);
    let config = format!("[media]\nuser_quota_bytes = {}\n", small.len() + 3000);
    for backend in backends() {
        let stage = Stage::start(backend, &config).await;
        let (a, mut b, c, chat) = stage.people().await;
        let node = &stage.rig.first;
        let (_, pasted) = paste(&a, &stage.cloud, &chat, &small, (8, 8)).await;
        let sent = send_file(&a, &stage.cloud, &chat, "full.bin", &[7; 3000]).await;
        let share = card(&mut b, &stage.cloud, &chat, &sent).await.share_url.unwrap();
        let (file, link) = item(stage.rig.other(), &b, &stage, &share).await.unwrap();
        let said = refused(&a, "one-more.bin", b"x").await;
        assert!(said.contains("enough storage space"), "{said}");

        for message_id in [pasted, sent] {
            a.write(Action::Delete {
                conversation: chat.clone(),
                message_id,
            })
            .await;
        }
        // the picture's bytes leave the store after the delete's answer
        for _ in 0..500 {
            if kept(&stage).await == (0, 1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(kept(&stage).await, (0, 1), "the picture stayed, or the file went");
        let (src, _) = paste(&a, &stage.cloud, &chat, &small, (8, 8)).await;
        let said = refused(&a, "after.bin", &[7; 3000]).await;
        assert!(said.contains("enough storage space"), "{said}");

        let object = src.split_once("/ams/v1/objects/").unwrap().1;
        let object = format!("/ams/v1/objects/{}", object.split('/').next().unwrap());
        let (ic3, theirs) = (token(&a, IC3).await, token(&b, IC3).await);
        let delete = async |path: &str, token: &str| raw(node, Method::DELETE, path, Some(token), &[], None).await.0;
        assert_eq!(delete(&object, &theirs).await, 403);
        assert_eq!(get(node, &format!("{object}/views/imgo"), Some(&theirs)).await, 200);
        assert_eq!(delete(&object, &ic3).await, 204);
        assert_eq!(delete(&object, &ic3).await, 404);
        assert_eq!(get(node, &format!("{object}/views/imgo"), Some(&ic3)).await, 404);
        assert_eq!(kept(&stage).await, (0, 1));

        let (own, outsider) = (token(&a, &stage.rig.origin).await, token(&c, &stage.rig.origin).await);
        let path = format!("{}/drive/items/{file}", drive(&a));
        assert_eq!(delete(&path, &outsider).await, 403);
        assert_eq!(
            delete(&format!("{}/drive/items/{file}", drive(&c)), &outsider).await,
            404
        );
        assert_eq!(get(stage.rig.other(), &link, None).await, 200);
        assert!(b.engine().file_thumbnail(share.clone()).await.unwrap().is_none());
        assert_eq!(stage.blobs.keys().await.len(), 2, "no note of the file's thumbnails");
        assert_eq!(delete(&path, &own).await, 204);
        assert_eq!(delete(&path, &own).await, 404);
        assert_eq!(
            get(stage.rig.other(), &link, None).await,
            404,
            "the link outlived the file"
        );
        assert_eq!(stage.blobs.keys().await, Vec::<String>::new());

        let whole = vec![7; 3000 + small.len()];
        assert_eq!(
            try_upload(&a, "after.bin", &whole).await.unwrap().size(),
            whole.len() as u64
        );
        let said = refused(&a, "one-more.bin", b"x").await;
        assert!(said.contains("enough storage space"), "{said}");
        drop((a, b, c));
        stage.stop().await;
    }
}
