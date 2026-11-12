//! The read-only mode (`ngteams::read_only`) on the mock chat service (`teams-mock`, through the `test-support` mock
//! origins) and in temporary directories, never a real account: every write kind the engine is asked for is refused
//! below the views and recorded in the diagnostic log as `refused_read_only` (method, host, path
//! shape), with nothing sent; reads go through; the transport refuses a write, a call write, a registration and a
//! mutating plain request on its own too; no push channel and no calling is opened; the broker serves `get` only; the
//! window refuses anywhere but the UI sandbox; the snapshot leaves its source as it was. Every id and token here is
//! made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use teams_auth::broker::private_pair;

use clap::Parser as _;
use ngteams::accounts_file::PersonalPaths;
use ngteams::cli::{Cli, ReadOnlyMode};
use ngteams::read_only;
#[cfg(target_os = "linux")]
use ngteams::read_only::window_allowed;
use ngteams::snapshot;
use serde_json::{Value, json};
use teams_auth::broker::BrokerSecretStore;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, OverlaySecretStore, Secret, SecretKey, SecretStore};
use teams_auth::token::TokenError;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{CallAction, CallActionError, EngineError, OwnStatus, Transient, TransientSent};
use teams_core::{Outbox, account};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_mock::{AX_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, OUTSIDER};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, CallContext, CallLink, CallParticipant, Client, Credentials, Endpoint, EndpointRegistration, HttpError,
    HttpTransport, LeaveCall, LeaveReason, Method, Refusal, Request as PlainRequest, SendMessage, Service, Transport,
    register,
};
use teams_store::{OutboxState, Store};

/// One account's token, as its session would give it.
struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// `who`'s read-only transport to the mock.
fn read_only_http(mock: &ChatServiceMock, who: &str) -> HttpTransport<Tokens> {
    let tokens = Arc::new(Tokens(mock.token(who).unwrap()));
    HttpTransport::with_mock_origin(tokens, CHAT_BASE, &mock.origin())
        .unwrap()
        .read_only()
}

/// Every request the mock got that wasn't a `GET`.
async fn not_reads(mock: &ChatServiceMock) -> Vec<String> {
    mock.received_requests()
        .await
        .iter()
        .filter(|request| request.method.as_str() != "GET")
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .collect()
}

/// `LAB_A`'s engine on the mock, with read-only transports, no push channel and no calls.
fn read_only_engine(mock: &ChatServiceMock, dir: &tempfile::TempDir) -> EngineHandle {
    let transport = Arc::new(read_only_http(mock, LAB_A));

    let client = Arc::new(Client::new(read_only_http(mock, LAB_A)));
    let outbox = Outbox::new(transport, Author::new(Mri::new(LAB_A), "a"));
    assert!(outbox.is_read_only());
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
        client,
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

/// Every chat write the views can ask for, on the account's own message `mine` and someone else's `theirs`.
fn every_write(mine: &str, theirs: &str) -> Vec<Action> {
    let text = || MessageText::plain("made up").unwrap();
    let conversation = || AX_CHAT.to_owned();
    vec![
        Action::Send {
            conversation: conversation(),
            text: text(),
        },
        Action::Edit {
            conversation: conversation(),
            message_id: mine.to_owned(),
            text: text(),
        },
        Action::Delete {
            conversation: conversation(),
            message_id: mine.to_owned(),
        },
        Action::React {
            conversation: conversation(),
            message_id: theirs.to_owned(),
            key: "like".to_owned(),
        },
        Action::Unreact {
            conversation: conversation(),
            message_id: theirs.to_owned(),
            key: "like".to_owned(),
        },
        Action::Read {
            conversation: conversation(),
            message_id: theirs.to_owned(),
        },
        Action::CreateChat {
            others: vec![Mri::new(OUTSIDER)],
            topic: None,
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_write_the_engine_is_asked_for_is_refused_and_never_sent_while_reads_go_through() {
    let mock = ChatServiceMock::start().await.unwrap();
    let mine = mock.post_as(LAB_A, AX_CHAT, "<p>mine</p>").unwrap();
    let theirs = mock.post_as(OUTSIDER, AX_CHAT, "<p>theirs</p>").unwrap();
    // what the posts above told the chat's members: from here on, nothing more is told
    let _ = (mock.take_events(OUTSIDER), mock.take_events(LAB_A));
    let dir = tempfile::tempdir().unwrap();
    let mut handle = read_only_engine(&mock, &dir);
    let core = handle.requests();
    assert!(core.read_only(), "the views are told");

    // reads go through
    core.sync_conversation(AX_CHAT).await.unwrap();
    let page = core.messages(AX_CHAT, None, 10).await.unwrap();
    for id in [&mine, &theirs] {
        assert!(page.messages.iter().any(|message| &message.id == id), "{id} read");
    }

    let conversation = || AX_CHAT.to_owned();
    let writes = every_write(&mine, &theirs);
    let _count = writes.len();
    for action in writes {
        let row = core.write(action.clone()).await.unwrap();
        assert_eq!(row.state, OutboxState::Failed, "{action:?}: {row:?}");
        assert!(
            row.last_error
                .as_deref()
                .is_some_and(|error| error.contains("read-only")),
            "{action:?}: {row:?}"
        );
    }
    // typing: dropped, never sent
    let typing = core
        .transient(Transient::Typing {
            conversation: conversation(),
        })
        .await
        .unwrap();
    assert!(
        matches!(&typing, TransientSent::Dropped { reason } if reason.contains("read-only")),
        "{typing:?}"
    );
    // the account's availability and status: no endpoint is ever registered, so none can be named
    for status in [OwnStatus::Available, OwnStatus::ClearManual] {
        assert!(matches!(
            core.own_availability(status).await,
            Err(EngineError::NoEndpoint)
        ));
    }
    // a call: there is no calling
    let call = core.call(CallAction::Call { chat: conversation() }).await;
    assert!(
        matches!(call, Err(EngineError::Call(CallActionError::NoCalling))),
        "{call:?}"
    );

    // nothing but reads reached the service, and nobody heard of anything
    assert_eq!(not_reads(&mock).await, Vec::<String>::new());
    assert!(mock.take_events(OUTSIDER).is_empty());
    // no row is ever tried again
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(not_reads(&mock).await, Vec::<String>::new());
    while let Some(event) = handle.try_next_event() {
        assert!(!matches!(event, Event::Connected { .. }), "no push channel");
    }
    handle.shutdown();
}

/// A plain `POST` to the chat service: a write's shape.
struct ChatPost;

impl PlainRequest for ChatPost {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations", AX_CHAT, "messages"])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({"content": "made up", "messagetype": "Text", "contenttype": "text", "clientmessageid": "1"}))
    }
}

fn participant() -> CallParticipant {
    CallParticipant {
        mri: Mri::new(LAB_A),
        display_name: Some("a".into()),
        endpoint_id: "00000000-0000-4000-8000-0000000000e1".into(),
        participant_id: Some("00000000-0000-4000-8000-0000000000e2".into()),
        language_id: Some("en-us".into()),
    }
}

#[tokio::test]
async fn the_transport_refuses_a_write_a_call_write_a_registration_and_a_mutating_plain_request_on_its_own() {
    let mock = ChatServiceMock::start().await.unwrap();
    let transport = read_only_http(&mock, LAB_A);
    assert!(Transport::is_read_only(&transport));

    // a typed write handed to the read-only transport
    let post = SendMessage::new(
        AX_CHAT,
        Author::new(Mri::new(LAB_A), "a"),
        MessageText::plain("x").unwrap(),
    );

    let write = post;
    let refused = transport.deliver(write).await;
    assert!(
        matches!(refused, Err(HttpError::Refused(Refusal::ReadOnly))),
        "{refused:?}"
    );

    // a call write: its `leave`
    let link = CallLink::parse(
        "https://api.flightproxy.teams.microsoft.com/api/v2/ep/conv-zz-01-prod-aks.conv.skype.com/conv/Synthetic0/leave?i=0-0-0-0&e=1",
    )
    .unwrap();
    let context = CallContext::new("00000000-0000-4000-8000-0000000000c1");
    let leave = LeaveCall::new(&context, &link, &participant(), LeaveReason::HungUp).unwrap();
    let call = transport.read_only_refusal(&leave);
    assert!(matches!(call, Some(HttpError::Refused(Refusal::ReadOnly))), "{call:?}");

    // the push channel's registration: no endpoint is registered
    let registration = EndpointRegistration::messaging(
        "00000000-0000-4000-8000-0000000000e1",
        "https://pub-ent-euwe-01-t.trouter.teams.microsoft.com/v4/f/made-up_endpoint-id/",
    )
    .unwrap();
    let registered = register(&transport, &registration).await;
    assert!(
        matches!(registered, Err(HttpError::Refused(Refusal::ReadOnly))),
        "{registered:?}"
    );
    // a write's shape as a plain request
    let plain = Client::new(read_only_http(&mock, LAB_A)).send(&ChatPost).await;
    assert!(matches!(plain, Err(HttpError::Refused(Refusal::ReadOnly))), "{plain:?}");

    assert_eq!(not_reads(&mock).await, Vec::<String>::new());
}

#[test]
fn a_read_only_account_opens_no_push_channel_and_no_calling() {
    // calls asked for, on file devices (no PipeWire, no portal)
    let calls = || {
        let devices = Arc::new(FileDevices {
            input: AudioInput::Silence,
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        });
        Some(ngteams::calling::on_devices(devices, false).0)
    };
    let (push, calling) = account::listening(true, true, calls());
    assert!(!push && calling.is_none(), "read-only: neither, whatever was asked");
    let (push, calling) = account::listening(false, true, calls());
    assert!(push && calling.is_some(), "a writing account keeps both");
    assert!(
        account::READ_ONLY_POLL >= Duration::from_secs(30),
        "it catches up by reads, not often"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_window_refuses_anywhere_but_the_ui_sandbox() {
    let sandbox = Some(OsStr::new("/tmp/ui-sandbox-made-up"));
    let inside = Some(OsStr::new("/tmp/ui-sandbox-made-up/niri-run/wayland-1"));
    assert!(window_allowed(inside, sandbox, None).is_ok());
    // the desktop's compositor, by name or by a path outside the sandbox dir
    for wayland in [
        "wayland-1",
        "/run/user/1000/wayland-1",
        "/tmp/ui-sandbox-made-up/../wayland-1",
    ] {
        assert!(
            window_allowed(Some(OsStr::new(wayland)), sandbox, None).is_err(),
            "{wayland}"
        );
    }
    assert!(window_allowed(inside, None, None).is_err(), "no sandbox opt-in");
    assert!(window_allowed(None, sandbox, None).is_err(), "no compositor");
    assert!(
        window_allowed(inside, sandbox, Some(OsStr::new(":0"))).is_err(),
        "DISPLAY set: X11 could be fallen back to"
    );
}

#[test]
fn the_command_line_asks_for_everything_the_read_only_mode_needs() {
    let parse = |args: &[&str]| Cli::try_parse_from(std::iter::once("ngteams").chain(args.iter().copied())).unwrap();
    let full = parse(&[
        "--read-only",
        "--account",
        "a@example.test",
        "--data",
        "/tmp/s",
        "--secrets-fd",
        "3",
    ]);
    assert_eq!(
        full.read_only_mode(None).unwrap(),
        Some(ReadOnlyMode {
            account: "a@example.test".into(),
            data: PathBuf::from("/tmp/s"),
            secrets_fd: 3,
            trace_fd: None,
            ctl_socket: None,
        })
    );
    // the control socket only where its runner names it
    let told = parse(&[
        "--read-only",
        "--account",
        "a@example.test",
        "--data",
        "/tmp/s",
        "--secrets-fd",
        "3",
        "--ctl-socket",
        "/state/ngteams-read-only/run/ctl.sock",
    ]);
    assert_eq!(
        told.read_only_mode(None).unwrap().unwrap().ctl_socket,
        Some("/state/ngteams-read-only/run/ctl.sock".into())
    );
    // the environment asks for it too; `0` and empty don't
    let by_env = parse(&["--account", "a@example.test", "--data", "/tmp/s", "--secrets-fd", "3"]);
    assert!(by_env.read_only_mode(Some(OsStr::new("1"))).unwrap().is_some());
    assert!(
        by_env.read_only_mode(Some(OsStr::new("0"))).is_err(),
        "its arguments alone are refused"
    );
    assert_eq!(parse(&[]).read_only_mode(Some(OsStr::new(""))).unwrap(), None);
    // what's missing is said
    for missing in [
        &["--read-only", "--data", "/tmp/s", "--secrets-fd", "3"][..],
        &["--read-only", "--account", "a@example.test", "--secrets-fd", "3"],
        &["--read-only", "--account", "a@example.test", "--data", "/tmp/s"],
        &[
            "--read-only",
            "--account",
            "a",
            "--data",
            "/s",
            "--secrets-fd",
            "3",
            "--trace-fd",
            "3",
        ],
    ] {
        assert!(parse(missing).read_only_mode(None).is_err(), "{missing:?}");
    }
    assert!(Cli::try_parse_from(["ngteams", "--read-only", "--forget", "a@example.test"]).is_err());
}

#[tokio::test]
async fn the_broker_serves_get_only_and_the_overlay_keeps_renewals_in_memory() {
    let store = Arc::new(MemorySecretStore::new());
    let key = refresh_token_key("a@example.test");
    store.put(&key, &Secret::new("stored-made-up")).await.unwrap();
    let (ours, theirs) = private_pair().unwrap();
    let scope = read_only::broker_scope("a@example.test");
    assert!(scope.is_get_only());
    let served = Arc::clone(&store);
    let mut noted = Vec::new();
    let broker = tokio::spawn(async move {
        let result = read_only::serve_broker(theirs, &scope, &*served, &mut noted).await;
        (result.is_ok(), String::from_utf8(noted).unwrap())
    });
    let client = BrokerSecretStore::new(ours, ngteams::KEYRING_APPLICATION).unwrap();
    assert_eq!(client.get(&key).await.unwrap().unwrap().expose(), "stored-made-up");
    assert!(client.put(&key, &Secret::new("rotated")).await.is_err(), "put refused");
    assert!(client.delete(&key).await.is_err(), "delete refused");
    // through the overlay the app uses, a renewal lives in memory only
    let overlay = OverlaySecretStore::new(client);
    overlay.put(&key, &Secret::new("rotated-made-up")).await.unwrap();
    assert_eq!(overlay.get(&key).await.unwrap().unwrap().expose(), "rotated-made-up");
    let other = SecretKey::new("a@example.test", "https://ic3.teams.office.com");
    overlay.delete(&other).await.unwrap();
    assert!(overlay.get(&other).await.unwrap().is_none());
    drop(overlay);
    let (closed_cleanly, noted) = broker.await.unwrap();
    assert!(closed_cleanly);
    assert!(
        noted.contains("put") && noted.contains("delete") && noted.contains("get only"),
        "{noted}"
    );
    assert!(!noted.contains("made-up"), "no value is noted: {noted}");
    assert_eq!(
        store.get(&key).await.unwrap().unwrap().expose(),
        "stored-made-up",
        "the stored item stays"
    );
}

/// Every file under `dir` with its bytes (the shared-memory file a reader may touch left out).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if !path.to_string_lossy().ends_with("-shm") {
                // Store synchronization files are empty. Windows' mandatory byte locks can refuse even a read
                // past EOF while the source is open; checking zero length establishes their entire contents.
                let bytes = if path.extension().is_some_and(|extension| extension == "lock") {
                    assert_eq!(path.metadata().unwrap().len(), 0, "synthetic lock contents");
                    Vec::new()
                } else {
                    fs::read(&path).unwrap()
                };
                found.insert(path, bytes);
            }
        }
    }
    found
}

#[test]
fn the_snapshot_copies_what_the_mode_needs_and_leaves_its_source_as_it_was() {
    let home = tempfile::tempdir().unwrap();
    let mine = PersonalPaths::new(home.path().join("config/ngteams"), home.path().join("share/ngteams"));
    mine.make_dirs().unwrap();
    let account = ngteams::accounts_file::PersonalAccount::new(
        "someone@example.test",
        "00000000-0000-4000-8000-0000000000ff",
        "00000000-0000-4000-8000-000000000001",
    )
    .unwrap();
    account.save(&mine).unwrap();
    fs::write(mine.settings_file(), "send_typing = false\n").unwrap();
    fs::create_dir_all(mine.data_dir().join("regions")).unwrap();
    fs::write(mine.region_file(&account).unwrap(), r#"{"region": "amer"}"#).unwrap();
    fs::create_dir_all(mine.log_dir()).unwrap();
    fs::write(mine.log_dir().join("app.log"), "synthetic log\n").unwrap();
    let store_file = mine.store_file(&account).unwrap();
    {
        let store = Store::open(&store_file).unwrap();
        drop(store);
    }
    let before = tree(home.path());

    // the store copied while nothing has it open, and again while an engine's store is open on it
    let first = tempfile::tempdir().unwrap();
    let dest = first.path().join("snapshot");
    let paths = snapshot::snapshot(&mine, &dest).unwrap();
    assert_eq!(tree(home.path()), before, "nothing of the source changed");
    assert!(paths.accounts_file().is_file() && paths.settings_file().is_file());
    assert!(paths.region_file(&account).unwrap().is_file());
    assert!(!paths.log_dir().exists(), "diagnostic logs are left out of a snapshot");
    Store::open(&paths.store_file(&account).unwrap()).unwrap();
    assert_eq!(tree(home.path()), before, "the copy is a store of its own");

    let open = Store::open(&store_file).unwrap();
    let before_open = tree(home.path());
    let second = tempfile::tempdir().unwrap();
    snapshot::snapshot(&mine, &second.path().join("snapshot")).unwrap();
    assert_eq!(tree(home.path()), before_open, "read through its WAL, nothing written");
    drop(open);

    // a snapshot inside the user's files, or into a directory holding something, is refused
    assert!(snapshot::snapshot(&mine, &mine.data_dir().join("snapshot")).is_err());
    assert!(snapshot::snapshot(&mine, &dest).is_err(), "not empty");
    // and the mode refuses to run on the user's own files
    assert!(read_only::snapshot_paths(home.path(), &mine).is_err());
    assert!(read_only::snapshot_paths(&dest, &mine).is_ok());
    assert!(
        read_only::snapshot_paths(first.path(), &mine).is_err(),
        "no snapshot there"
    );
}
