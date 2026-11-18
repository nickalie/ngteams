//! Synthetic picture references under concurrent bot requests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use ngteams_server::storage::{Blob, Incoming, Storage, StorageError};
use reqwest::Method;
use serde_json::json;
use tokio::sync::Notify;

use super::connector::Setup;
use super::endpoint::encode;
use crate::support::backends;

#[derive(Debug)]
struct HeldRead {
    inner: Arc<dyn Storage>,
    armed: AtomicBool,
    entered: Notify,
    resume: Notify,
}

#[async_trait]
impl Storage for HeldRead {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, StorageError> {
        let bytes = self.inner.get(key).await?;
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            self.resume.notified().await;
        }
        Ok(bytes)
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }

    async fn put_stream(&self, key: &str, body: Incoming, limit: u64) -> Result<u64, StorageError> {
        self.inner.put_stream(key, body, limit).await
    }

    async fn open(&self, key: &str, range: Option<std::ops::Range<u64>>) -> Result<Option<Blob>, StorageError> {
        self.inner.open(key, range).await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prepared_picture_cannot_commit_after_its_last_reference_is_removed() {
    for backend in backends() {
        for update in [false, true] {
            prepared_picture_after_cleanup(backend, update).await;
        }
    }
}

async fn prepared_picture_after_cleanup(backend: crate::support::Backend, update: bool) {
    use base64::Engine as _;
    let mut held = None;
    let setup = Setup::open_changed(
        backend,
        |_| {},
        |app| {
            let storage = Arc::new(HeldRead {
                inner: Arc::clone(&app.storage),
                armed: AtomicBool::new(false),
                entered: Notify::new(),
                resume: Notify::new(),
            });
            app.storage = storage.clone();
            held = Some(storage);
        },
    )
    .await;
    let held = held.unwrap();
    let chat = setup.chat();
    setup.send(&chat, "pictures").await;
    let got = setup.bot.next_of("message").await;
    let connector = setup.connector(got.activity["serviceUrl"].as_str().unwrap()).await;
    let inline = json!({"attachments": [{"contentType": "image/png", "contentUrl": format!(
        "data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(super::png(8, 8)))}]});
    let (status, sent) = connector.send(&chat, inline).await;
    assert_eq!(status, 200);
    let messages = setup.synced(&chat).await;
    let content = &messages.last().unwrap().content;
    let picture = content.split("/objects/").nth(1).unwrap().split('/').next().unwrap();
    let activity = json!({"attachments": [{"contentType": "image/png", "contentUrl": format!(
        "{}v3/attachments/{picture}/views/original", connector.base)}]});
    let (_, target) = connector.send(&chat, json!({"text": "target"})).await;
    let target_path = format!(
        "v3/conversations/{}/activities/{}",
        encode(&chat),
        target["id"].as_str().unwrap()
    );
    let source_path = format!(
        "v3/conversations/{}/activities/{}",
        encode(&chat),
        sent["id"].as_str().unwrap()
    );
    held.armed.store(true, Ordering::SeqCst);
    let pending = async {
        if update {
            connector.call(Method::PUT, &target_path, Some(activity)).await
        } else {
            connector.send(&chat, activity).await
        }
    };
    let remove = async {
        tokio::time::timeout(Duration::from_secs(10), held.entered.notified())
            .await
            .unwrap();
        let result = if update {
            connector
                .call(Method::PUT, &source_path, Some(json!({"text": "removed"})))
                .await
        } else {
            connector.call(Method::DELETE, &source_path, None).await
        };
        held.resume.notify_one();
        assert_eq!(result.0, 200);
    };
    let (answer, ()) = tokio::join!(pending, remove);
    assert_eq!(answer.0, 404, "{answer:?}");
    let messages = setup.synced(&chat).await;
    assert!(messages.iter().all(|message| !message.content.contains(picture)));
    setup.stop().await;
}
