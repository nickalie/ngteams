//! Synthetic catch-up reads held at the HTTP boundary while user reads take the foreground lane.

#![cfg(test)]

use std::future::{Future, ready};
use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::{EngineHandle, EngineOptions, Event, Outbox, Parts, engine};
use teams_mock::{CHAT_BASE, CSA_BASE, ChatServiceMock, Front, Hold, LAB_A, TEAM_CHANNEL, synthetic_chat};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;

const WAIT: Duration = Duration::from_secs(5);

struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

struct Rig {
    handle: EngineHandle,
    mock: ChatServiceMock,
    _front: Front,
    _dir: tempfile::TempDir,
}

impl Rig {
    async fn new() -> Self {
        Self::with_history(0).await
    }

    async fn with_history(messages: usize) -> Self {
        let mock = ChatServiceMock::start().await.unwrap();
        mock.add_synthetic_chats(8);
        mock.page_conversation_list(50);
        for _ in 0..messages {
            mock.post_as(teams_mock::LAB_B, teams_mock::AB_CHAT, "<p>Synthetic history</p>")
                .unwrap();
        }
        let front = mock.start_front().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
        let http = || {
            HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, front.origin())
                .unwrap()
                .for_account(Mri::new(LAB_A))
                .with_csa(CSA_BASE)
                .unwrap()
        };
        let handle = engine::start(Parts {
            store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
            client: Arc::new(Client::new(http())),
            outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "Synthetic")),
            push: None,
            calls: None,
            known_names: Vec::new(),
            options: EngineOptions::default(),
        })
        .unwrap();
        Self {
            handle,
            mock,
            _front: front,
            _dir: dir,
        }
    }

    fn hold_messages(&self, chat: &str) -> Hold {
        let encoded = chat.replace(':', "%3A").replace('@', "%40");
        let plain = format!("/{chat}/messages");
        let encoded = format!("/{encoded}/messages");
        self.mock.hold(move |target| {
            let path = target.split('?').next().unwrap_or_default();
            path.ends_with(&plain) || path.ends_with(&encoded)
        })
    }

    fn hold_thread(&self, chat: &str) -> Hold {
        let plain = format!("/threads/{chat}");
        let encoded = format!("/threads/{}", chat.replace(':', "%3A").replace('@', "%40"));
        self.mock.hold(move |target| {
            let path = target.split('?').next().unwrap_or_default();
            path.ends_with(&plain) || path.ends_with(&encoded)
        })
    }

    async fn reads_of(&self, chat: &str) -> usize {
        self.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| {
                let path = request.url.path().replace("%3A", ":").replace("%40", "@");
                path.ends_with(&format!("/{chat}/messages"))
            })
            .count()
    }

    async fn teams_ready(&mut self) {
        tokio::time::timeout(WAIT, async {
            while let Some(event) = self.handle.next_event().await {
                if matches!(event, Event::Teams) {
                    return;
                }
            }
            panic!("engine stopped before the teams arrived");
        })
        .await
        .expect("teams waited for catch-up");
        assert!(self.handle.teams().await.unwrap().is_some());
    }
}

async fn soon<T>(reply: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, reply)
        .await
        .expect("foreground read waited for background catch-up")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opened_chat_channel_and_teams_read_while_background_waits_without_duplicate_reads() {
    let mut rig = Rig::new().await;
    let first = synthetic_chat(0);
    let opened = synthetic_chat(7);
    let held = rig.hold_messages(&first);
    let sync = rig.handle.sync();
    assert!(held.wait_entered(WAIT));
    let one = rig.handle.sync_conversation(&opened);
    let two = rig.handle.sync_conversation(&opened);
    assert_eq!(soon(one).await.unwrap().inserted, 1);
    soon(two).await.unwrap();
    assert_eq!(rig.reads_of(&opened).await, 1);
    assert_eq!(rig.handle.messages(&opened, None, 10).await.unwrap().messages.len(), 1);
    assert!(rig.handle.teams().await.unwrap().is_none());
    rig.teams_ready().await;
    rig.handle.chat_listing().await.unwrap();
    rig.handle.teams().await.unwrap();
    assert_eq!(
        rig.mock
            .received_requests()
            .await
            .iter()
            .filter(|request| { request.url.path() == "/api/csa/amer/api/v3/teams/users/me" })
            .count(),
        1
    );
    soon(rig.handle.sync_conversation(TEAM_CHANNEL)).await.unwrap();
    assert!(
        !rig.handle
            .messages(TEAM_CHANNEL, None, 100)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(held.release());
    assert!(soon(sync).await.unwrap().failed.is_empty());
    assert_eq!(rig.reads_of(&first).await, 1);
    assert_eq!(rig.reads_of(&opened).await, 1);
    assert_eq!(rig.reads_of(TEAM_CHANNEL).await, 1);
    rig.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_opened_background_chat_reuses_its_read_and_canceled_requests_keep_queue_order() {
    let rig = Rig::new().await;
    let first = synthetic_chat(0);
    let second = synthetic_chat(6);
    let third = synthetic_chat(7);
    let held = rig.hold_messages(&first);
    let foreground = rig.hold_messages(&second);
    let third_held = rig.hold_messages(&third);
    let sync = rig.handle.sync();
    assert!(held.wait_entered(WAIT));
    let same = rig.handle.sync_conversation(&first);
    let two = rig.handle.sync_conversation(&second);
    assert!(foreground.wait_entered(WAIT));
    drop(rig.handle.sync_conversation(synthetic_chat(5)));
    let three = rig.handle.sync_conversation(&third);
    let duplicate = rig.handle.sync_conversation(&second);
    assert!(
        !third_held.wait_entered(Duration::from_millis(100)),
        "only one foreground read may run"
    );
    assert!(foreground.release());
    soon(two).await.unwrap();
    assert!(third_held.wait_entered(WAIT));
    assert!(third_held.release());
    soon(three).await.unwrap();
    soon(duplicate).await.unwrap();
    assert_eq!(rig.reads_of(&second).await, 1);
    assert_eq!(rig.reads_of(&third).await, 1);
    assert_eq!(rig.reads_of(&synthetic_chat(5)).await, 0);
    assert!(held.release());
    soon(same).await.unwrap();
    soon(sync).await.unwrap();
    assert_eq!(rig.reads_of(&first).await, 1);
    assert_eq!(rig.reads_of(&second).await, 1);
    assert_eq!(rig.reads_of(&third).await, 1);
    let requests = rig.mock.received_requests().await;
    let chats: Vec<_> = requests
        .iter()
        .filter_map(|request| {
            let path = request.url.path().replace("%3A", ":").replace("%40", "@");
            path.ends_with("/messages").then_some(path)
        })
        .collect();
    assert!(chats[0].contains(&second), "{chats:?}");
    assert!(chats[1].contains(&third), "{chats:?}");
    rig.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn older_messages_run_during_a_later_catch_up_and_keep_the_newest_sync_cursor() {
    let rig = Rig::with_history(205).await;
    soon(rig.handle.sync()).await.unwrap();
    let before = rig
        .handle
        .messages(teams_mock::AB_CHAT, None, 1000)
        .await
        .unwrap()
        .messages
        .len();
    let held = rig.mock.hold(|target| {
        target
            .split('?')
            .next()
            .is_some_and(|path| path.ends_with("/conversations"))
    });
    let sync = rig.handle.sync();
    assert!(held.wait_entered(WAIT));
    let older = soon(rig.handle.load_older(teams_mock::AB_CHAT)).await.unwrap();
    assert!(older.is_some());
    let after = rig
        .handle
        .messages(teams_mock::AB_CHAT, None, 1000)
        .await
        .unwrap()
        .messages
        .len();
    assert!(after > before);
    assert!(held.release());
    soon(sync).await.unwrap();
    assert_eq!(
        rig.handle
            .messages(teams_mock::AB_CHAT, None, 1000)
            .await
            .unwrap()
            .messages
            .len(),
        after
    );
    rig.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ready_background_answer_is_stored_before_the_next_queued_foreground_read_finishes() {
    let rig = Rig::new().await;
    let first = synthetic_chat(0);
    let second = synthetic_chat(6);
    let third = synthetic_chat(7);
    let background = rig.hold_messages(&first);
    let foreground = rig.hold_messages(&second);
    let next_foreground = rig.hold_messages(&third);
    let sync = rig.handle.sync();
    assert!(background.wait_entered(WAIT));
    let two = rig.handle.sync_conversation(&second);
    assert!(foreground.wait_entered(WAIT));
    let three = rig.handle.sync_conversation(&third);
    assert!(background.release());
    // Let the released HTTP answer become ready while the foreground still owns the store.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(foreground.release());
    soon(two).await.unwrap();
    assert!(next_foreground.wait_entered(WAIT));
    assert_eq!(
        soon(rig.handle.messages(&first, None, 10))
            .await
            .unwrap()
            .messages
            .len(),
        1
    );
    assert!(next_foreground.release());
    soon(three).await.unwrap();
    soon(sync).await.unwrap();
    rig.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reopened_chat_reads_new_changes_during_the_same_catch_up() {
    let rig = Rig::new().await;
    let opened = synthetic_chat(7);
    let held = rig.hold_messages(&synthetic_chat(0));
    let sync = rig.handle.sync();
    assert!(held.wait_entered(WAIT));
    soon(rig.handle.sync_conversation(&opened)).await.unwrap();
    assert!(rig.mock.edit_synthetic_message(7, "Updated while catch-up waits"));
    let updated = soon(rig.handle.sync_conversation(&opened)).await.unwrap();
    assert_eq!(updated.updated, 1);
    let messages = rig.handle.messages(&opened, None, 10).await.unwrap().messages;
    assert!(messages[0].content.contains("Updated while catch-up waits"));
    assert_eq!(rig.reads_of(&opened).await, 2);
    assert!(held.release());
    soon(sync).await.unwrap();
    assert_eq!(rig.reads_of(&opened).await, 2);
    rig.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_background_read_is_still_sent_while_a_queued_foreground_read_runs() {
    let rig = Rig::new().await;
    let first = synthetic_chat(0);
    let background = rig.hold_messages(&first);
    // what the catch-up reads after chat 0's messages: its members
    let next_background = rig.hold_thread(&first);
    let foreground = rig.hold_messages(&synthetic_chat(6));
    let next_foreground = rig.hold_messages(&synthetic_chat(7));
    let sync = rig.handle.sync();
    assert!(background.wait_entered(WAIT));
    let two = rig.handle.sync_conversation(synthetic_chat(6));
    assert!(foreground.wait_entered(WAIT));
    let three = rig.handle.sync_conversation(synthetic_chat(7));
    assert!(background.release());
    // chat 0's answer comes back while chat 6 still runs, so the catch-up moves on to its members as chat 7 starts
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(foreground.release());
    soon(two).await.unwrap();
    assert!(next_foreground.wait_entered(WAIT));
    // the catch-up's next read is polled (its transport timeout running) beside the queued foreground read
    assert!(next_background.wait_entered(WAIT), "background read left unpolled");
    assert!(next_background.release());
    assert!(next_foreground.release());
    soon(three).await.unwrap();
    assert!(soon(sync).await.unwrap().failed.is_empty());
    rig.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_sync_queued_after_catch_up_is_not_read() {
    let rig = Rig::new().await;
    soon(rig.handle.sync()).await.unwrap();
    let (first, dropped, last) = (synthetic_chat(0), synthetic_chat(1), synthetic_chat(2));
    let reads = rig.reads_of(&dropped).await;
    let held = rig.hold_messages(&first);
    let one = rig.handle.sync_conversation(&first);
    assert!(held.wait_entered(WAIT));
    drop(rig.handle.sync_conversation(&dropped));
    drop(rig.handle.load_older(&dropped));
    let three = rig.handle.sync_conversation(&last);
    assert!(held.release());
    soon(one).await.unwrap();
    soon(three).await.unwrap();
    assert_eq!(rig.reads_of(&dropped).await, reads);
    rig.handle.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_foreground_sync_from_before_the_listing_is_not_reused_over_a_newer_version() {
    let rig = Rig::new().await;
    soon(rig.handle.sync()).await.unwrap();
    let opened = synthetic_chat(7);
    rig.mock
        .post_as(teams_mock::LAB_B, &opened, "<p>Before the listing</p>")
        .unwrap();
    let list = rig
        .mock
        .hold(|target| target.split('?').next().unwrap_or_default().ends_with("/conversations"));
    let sync = rig.handle.sync();
    assert!(list.wait_entered(WAIT));
    soon(rig.handle.sync_conversation(&opened)).await.unwrap();
    rig.mock
        .post_as(teams_mock::LAB_B, &opened, "<p>After the foreground sync</p>")
        .unwrap();
    assert!(list.release());
    assert!(soon(sync).await.unwrap().failed.is_empty());
    let messages = rig.handle.messages(&opened, None, 10).await.unwrap().messages;
    assert!(
        messages
            .iter()
            .any(|message| message.content.contains("After the foreground sync")),
        "{messages:?}"
    );
    rig.handle.shutdown();
}
