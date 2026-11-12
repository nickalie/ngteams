//! Synthetic large-tenant startup: the service is paused between reads while views query the engine.

#![cfg(test)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::{EngineOptions, Event, Outbox, Parts, Source, engine};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use tokio::sync::{Semaphore, watch};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const FIRST: &str = "19:synthetic-0@thread.v2";
const BASE: &str = teams_mock::CHAT_BASE;
const WAIT: Duration = Duration::from_secs(5);

struct Stepped {
    reads: AtomicUsize,
    entered: watch::Sender<usize>,
    permits: Semaphore,
}

impl Credentials for Stepped {
    async fn bearer(&self, _audience: &str) -> Result<Secret, TokenError> {
        self.entered.send_replace(self.reads.fetch_add(1, Ordering::SeqCst) + 1);
        self.permits.acquire().await.unwrap().forget();
        Ok(Secret::new("synthetic-startup-token".to_owned()))
    }

    fn bearer_rejected(
        &self,
        audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

impl Stepped {
    async fn waiting_at(&self, count: usize) {
        tokio::time::timeout(WAIT, self.entered.subscribe().wait_for(|entered| *entered >= count))
            .await
            .expect("the next service read did not start")
            .unwrap();
    }
}

async fn large_tenant() -> MockServer {
    let server = MockServer::start().await;
    let list_path = "/api/chatsvc/amer/v1/users/ME/conversations";
    for (start, query) in [(0, false), (200, true)] {
        let conversations: Vec<_> = (start..start + 200)
            .map(|n| {
                json!({
                    "id": format!("19:synthetic-{n}@thread.v2"), "version": 7,
                    "threadProperties": {"threadType": "chat", "topic": format!("Synthetic chat {n}")}
                })
            })
            .collect();
        let metadata = if query {
            json!({"syncState": format!("{BASE}/v1/users/ME/conversations?syncState=done")})
        } else {
            json!({"forwardLink": format!("{BASE}/v1/users/ME/conversations?syncState=next")})
        };
        Mock::given(wiremock::matchers::path(list_path))
            .and(move |request: &wiremock::Request| {
                request.url.query().is_some_and(|q| q.contains("syncState=next")) == query
            })
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "conversations": conversations, "_metadata": metadata,
            })))
            .mount(&server)
            .await;
    }
    Mock::given(wiremock::matchers::path_regex("/conversations/.*/messages$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages": []})))
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::path_regex("/threads/.*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": FIRST, "members": [], "properties": {"rosterVersion": "1"}
        })))
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::path(format!("{list_path}/{FIRST}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages": [{
            "id": "1760000010000", "version": "1760000010000", "conversationid": FIRST,
            "messagetype": "RichText/Html", "content": "<p>Synthetic first message</p>",
            "from": format!("{BASE}/v1/users/ME/contacts/{ME}"),
            "originalarrivaltime": "2025-10-09T08:43:20.0000000Z"
        }]})))
        .with_priority(1)
        .mount(&server)
        .await;
    server
}

fn start(server: &MockServer, credentials: &Arc<Stepped>, directory: &std::path::Path) -> engine::EngineHandle {
    let http = || HttpTransport::with_mock_origin(Arc::clone(credentials), BASE, &server.uri()).unwrap();
    let transport = Arc::new(http());
    let client = Arc::new(Client::new(http()));
    let author = Author::new(Mri::new(ME), "Synthetic");
    engine::start(Parts {
        store: Store::open(&directory.join("store.sqlite")).unwrap(),
        client,
        outbox: Outbox::new(transport, author),
        calls: None,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn first_list_page_and_messages_are_published_before_the_rest_of_a_large_tenant_finishes() {
    let server = large_tenant().await;
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(Stepped {
        reads: AtomicUsize::new(0),
        entered: watch::channel(0).0,
        permits: Semaphore::new(2),
    });
    let mut handle = start(&server, &credentials, directory.path());
    let sync = handle.sync();
    credentials.waiting_at(3).await;
    let first_page = tokio::time::timeout(WAIT, handle.conversations(500))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first_page.len(),
        200,
        "the first list page is usable while the next one waits"
    );
    assert_eq!(
        first_page.iter().find(|c| c.id == FIRST).unwrap().topic.as_deref(),
        Some("Synthetic chat 0")
    );
    let first_change = tokio::time::timeout(WAIT, handle.next_event()).await.unwrap().unwrap();
    assert!(
        matches!(first_change, Event::ConversationChanged { .. }),
        "{first_change:?}"
    );

    credentials.permits.add_permits(1);
    credentials.waiting_at(4).await;
    assert_eq!(handle.conversations(500).await.unwrap().len(), 400);
    credentials.permits.add_permits(2);
    credentials.waiting_at(6).await;
    let messages = handle.messages(FIRST, None, 10).await.unwrap().messages;
    assert_eq!(
        messages.len(),
        1,
        "the first chat opens before the other 399 are synced"
    );
    let mut message_events = 0;
    while let Some(event) = handle.try_next_event() {
        if matches!(
            event,
            Event::Message {
                source: Source::CatchUp,
                ..
            }
        ) {
            message_events += 1;
        }
    }
    assert_eq!(
        message_events, 1,
        "stored messages are published before the full catch-up ends"
    );
    credentials.permits.add_permits(1000);
    let report = tokio::time::timeout(Duration::from_secs(30), sync)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.conversations, 400);
    assert_eq!(report.messages.inserted, 1);
    assert!(report.failed.is_empty(), "{report:?}");
    while let Some(event) = handle.try_next_event() {
        if matches!(
            event,
            Event::Message {
                source: Source::CatchUp,
                ..
            }
        ) {
            message_events += 1;
        }
    }
    assert_eq!(
        message_events, 1,
        "the final publication must not repeat a progress message"
    );
    handle.shutdown();
}

#[tokio::test]
async fn a_failed_list_page_leaves_staged_chats_to_be_synced_on_the_next_run() {
    let server = large_tenant().await;
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(Stepped {
        reads: AtomicUsize::new(0),
        entered: watch::channel(0).0,
        permits: Semaphore::new(2),
    });
    let handle = start(&server, &credentials, directory.path());
    let sync = handle.sync();
    credentials.waiting_at(3).await;
    server.reset().await;
    credentials.permits.add_permits(1000);
    assert!(tokio::time::timeout(WAIT, sync).await.unwrap().is_err());
    handle.shutdown();
    let store = Store::open(&directory.path().join("store.sqlite")).unwrap();
    assert_eq!(store.conversation_list_sync_link().unwrap(), None);
    assert_eq!(store.conversation(FIRST).unwrap().unwrap().version, 0);
    assert!(store.sync_state(FIRST).unwrap().is_none());
    drop(store);

    Mock::given(wiremock::matchers::method("GET"))
        .respond_with(|request: &wiremock::Request| {
            let body = if request.url.path().ends_with("/conversations") {
                json!({"conversations": [{
                    "id": FIRST, "version": 7, "threadProperties": {"threadType": "chat"}
                }], "_metadata": {"syncState": format!("{BASE}/v1/users/ME/conversations?syncState=done")}})
            } else if request.url.path().ends_with("/messages") {
                json!({"messages": []})
            } else {
                json!({"id": FIRST, "members": [], "properties": {"rosterVersion": "1"}})
            };
            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(&server)
        .await;
    let handle = start(&server, &credentials, directory.path());
    let report = tokio::time::timeout(WAIT, handle.sync()).await.unwrap().unwrap();
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(
        report.messages.pages, 1,
        "the staged row must not skip the first message read"
    );
    assert_eq!(handle.conversation(FIRST.to_owned()).await.unwrap().unwrap().version, 7);
    handle.shutdown();
}
