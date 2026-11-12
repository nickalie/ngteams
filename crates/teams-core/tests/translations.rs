//! A chat's translations made on this computer, kept and read through a running engine (no service write, nothing on
//! the wire), and read back by a later engine on the same store: what a restart of the app shows without translating
//! again. Every id and text here is made up.

#![cfg(test)]

use std::future::{self, Future};
use std::path::Path;
use std::sync::Arc;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_mock::{CHAT_BASE, LAB_A, LAB_B};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::{
    Conversation, ConversationKind, GlossaryTerm, Message, SentOriginal, Store, StoredTranslation, TranslationKind,
};
use wiremock::MockServer;

const CHAT: &str = "19:synthetic-translated@unq.gbl.spaces";

struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-translation-token")))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// The store at `path`, holding the chat and two messages from Lab B.
fn seeded(path: &Path) {
    let mut store = Store::open(path).unwrap();
    store
        .upsert_conversation(&Conversation {
            id: CHAT.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: Some("Synthetic topic".to_owned()),
            version: 1,
            last_activity: None,
        })
        .unwrap();
    for id in ["1001", "1002"] {
        store
            .upsert_message(
                &Message {
                    conversation_id: CHAT.to_owned(),
                    id: id.to_owned(),
                    version: 7,
                    client_message_id: None,
                    sender: Some(LAB_B.to_owned()),
                    sender_name: None,
                    message_type: "RichText/Html".to_owned(),
                    content: "<p>Synthetic text</p>".to_owned(),
                    properties: None,
                    arrival_time: 1_000,
                    compose_time: None,
                    edit_time: None,
                    delete_time: None,
                },
                None,
            )
            .unwrap();
    }
}

fn engine_on(server: &MockServer, path: &Path) -> EngineHandle {
    let tokens = Arc::new(Tokens);
    let transport = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &server.uri()).unwrap();
    let outbox = Outbox::new(Arc::new(transport()), Author::new(Mri::new(LAB_A), "Lab A"));
    engine::start(Parts {
        calls: None,
        store: Store::open(path).unwrap(),
        client: Arc::new(Client::new(transport())),
        outbox,
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

#[tokio::test]
async fn kept_translations_are_read_back_by_a_later_engine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("translations.sqlite");
    seeded(&path);
    let server = MockServer::start().await;
    let ids = vec!["1001".to_owned(), "1002".to_owned()];
    {
        let handle = engine_on(&server, &path);
        assert!(
            handle
                .translations(CHAT.to_owned(), "de".to_owned(), ids.clone())
                .await
                .unwrap()
                .is_empty()
        );
        handle
            .keep_translations(vec![StoredTranslation {
                conversation_id: CHAT.to_owned(),
                message_id: "1001".to_owned(),
                version: 7,
                language: "de".to_owned(),
                kind: TranslationKind::Translated,
                content: "synthetic translation".to_owned(),
                source_language: Some("en".to_owned()),
            }])
            .await
            .unwrap();
        handle
            .keep_sent_original(SentOriginal {
                conversation_id: CHAT.to_owned(),
                key: "synthetic-client-id".to_owned(),
                language: "de".to_owned(),
                content: "synthetic original".to_owned(),
                sent_text: "Synthetic sent".to_owned(),
                created_at: 5,
            })
            .await
            .unwrap();
        handle
            .keep_glossary(vec![GlossaryTerm {
                conversation_id: CHAT.to_owned(),
                language: "de".to_owned(),
                term: "synthetic term".to_owned(),
                rendering: "synthetic rendering".to_owned(),
                used_at: 9,
            }])
            .await
            .unwrap();
        handle.shutdown();
    }
    let handle = engine_on(&server, &path);
    let kept = handle
        .translations(CHAT.to_owned(), "de".to_owned(), ids)
        .await
        .unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].message_id, "1001");
    assert_eq!(kept[0].content, "synthetic translation");
    let originals = handle
        .sent_originals(CHAT.to_owned(), vec!["synthetic-client-id".to_owned()])
        .await
        .unwrap();
    assert_eq!(originals.len(), 1);
    assert_eq!(originals[0].content, "synthetic original");
    let glossary = handle.glossary(CHAT.to_owned(), "de".to_owned()).await.unwrap();
    assert_eq!(glossary.len(), 1);
    assert_eq!(glossary[0].rendering, "synthetic rendering");
    let chat = handle.conversation(CHAT.to_owned()).await.unwrap();
    assert_eq!(chat.and_then(|chat| chat.topic).as_deref(), Some("Synthetic topic"));
    assert!(
        handle
            .conversation("19:synthetic-unknown@unq.gbl.spaces".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    handle.shutdown();
    // nothing of it went to the service
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
}
