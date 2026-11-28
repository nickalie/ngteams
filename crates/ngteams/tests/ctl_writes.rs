//! The control socket's message writes (`ctl::writes`), through the real protocol and server, over lab account a's
//! engine on the mock chat service (`teams-mock`, loopback only, through the `test-support` mock origins): each write
//! goes out through a's outbox, its row answered, and the mock tells b what it did; a's own notes (`48:notes`) take
//! the same writes as a chat, and nobody else is told. The read-only mode refuses them all and sends nothing. `status
//! set` goes out as the status menu's choice does, through a's and b's engines with their push channels on the mock's
//! Trouter, to the mock's UPS; `sync` is a's catch-up. Every id and text is made up or the mock's own.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

use ctl_support::{ask, pump_until, quit};
use gpui::{App, AsyncApp, Entity, TestAppContext};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::ctl::writes::{self, is_settled};
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal, Session};
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{Action, CallFloor, EngineRequests, Event, Outbox, Reporting};
use teams_ctl::ClientError;
use teams_ctl::protocol::{
    CtlError, ErrorCode, ManualStatus, OutboxRow, OutboxState, OwnStatus, Request, Response, StatusChoice, SyncReport,
    WRITE_ANSWER, WRITE_WAIT,
};
use teams_mock::{
    AB_CHAT, AvailabilityChange, CHANNEL, CHAT_BASE, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C, NOTES,
    NOTIFICATIONS, PRESENCE_BASE, TENANT, TrouterMock,
};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::{OutboxEntry, OutboxKind, Store};
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::bus::BusError;
use teams_ui::{Bridge, Desktop, SettingsFile, ViewTrace, compose};
use tokio::sync::broadcast;

const UPN: &str = "bot-a@lab.example.test";

/// a's token, as its session would give it.
struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// The mock, and the app of lab account a on it serving its control socket.
struct Lab {
    mock: ChatServiceMock,
    /// a's engine, beside the app's: to sync a chat the way a delivery would have.
    requests: EngineRequests,
    socket: PathBuf,
    cx: TestAppContext,
    _dir: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Lab {
    /// The app of a on the mock, its store caught up first; `read_only` as `ngteams --read-only` runs.
    fn start(read_only: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let http = || {
            let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
            let http = HttpTransport::with_mock_origin(tokens, CHAT_BASE, &mock.origin()).unwrap();
            if read_only { http.read_only() } else { http }
        };
        let handle = engine::start(Parts {
            calls: None,
            store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
            client: Arc::new(Client::new(http())),
            outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
            push: None,
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })
        .unwrap();
        let requests = handle.requests();
        requests.sync().blocking_wait().unwrap();
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let cx = TestAppContext::single();
        let launch = Launch {
            first: started(handle),
            others: Vec::new(),
            settings: {
                let settings = app::settings(SettingsFile::new(dir.path().join("settings.toml")), None);
                if read_only { settings.read_only() } else { settings }
            },
            bus: Err(BusError::NotGiven),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap(),
                floor: CallFloor::new(),
                devices: None,
            },
            read_only,
            trace: Arc::new(ViewTrace::off()),
            ctl: Some(socket.clone()),
        };
        assert!(cx.update(|cx| app::launch(cx, launch)).is_ok(), "the window opens");
        cx.run_until_parked();
        Self {
            mock,
            requests,
            socket,
            cx,
            _dir: dir,
            runtime,
        }
    }

    fn ask(&self, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, None, request)
    }

    /// The row `request` answers with.
    fn row(&self, request: Request) -> OutboxRow {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Ok(Response::Outbox(row)) => row,
            other => panic!("{asked}: {other:?}"),
        }
    }

    /// The error `request` answers with.
    fn refused(&self, request: Request) -> CtlError {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("{asked}: {other:?}"),
        }
    }

    /// What the mock told b since the last time, in a–b, as `(kind, content)`.
    fn told_b(&self) -> Vec<(EventKind, String)> {
        self.mock
            .take_events(LAB_B)
            .into_iter()
            .filter(|event| event.conversation == AB_CHAT && event.from == LAB_A)
            .map(|event| {
                let content = event.resource["content"].as_str().unwrap_or_default().to_owned();
                (event.kind, content)
            })
            .collect()
    }

    fn bridge(&self) -> Entity<Bridge> {
        self.cx.read(app::bridges)[0].clone()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing (an empty
/// in-memory store, a loopback port nothing listens on).
fn started(handle: engine::EngineHandle) -> Started<MemorySecretStore> {
    started_as(handle, UPN, LAB_A, "bot-a")
}

/// [`started`] for the account `upn`, `mri`, named `name`.
fn started_as(handle: engine::EngineHandle, upn: &str, mri: &str, name: &str) -> Started<MemorySecretStore> {
    let mut account = PersonalAccount::new(upn, mri.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
    account.name = Some(name.to_owned());
    let session = Session::new(
        upn,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/")).unwrap(),
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz").unwrap(),
        Region::default(),
    );
    Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    }
}

fn send(text: &str, wait: bool) -> Request {
    Request::Send {
        chat: AB_CHAT.to_owned(),
        text: text.to_owned(),
        wait,
    }
}

fn sent(row: &OutboxRow, write: &str) -> String {
    assert_eq!((row.write.as_str(), row.state), (write, OutboxState::Sent), "{row:?}");
    assert_eq!(row.chat.as_deref(), Some(AB_CHAT), "{row:?}");
    row.message.clone().expect("the message it wrote")
}

#[test]
fn send_edit_react_unreact_and_delete_go_out_through_the_outbox() {
    let lab = Lab::start(false);

    // line breaks kept: each line a paragraph, as the compose box sends them
    let row = lab.row(send("one\ntwo", false));
    let id = sent(&row, "send");
    let told = lab.told_b();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, EventKind::Message);
    assert!(told[0].1.contains("<p>one</p><p>two</p>"), "{told:?}");

    let row = lab.row(Request::Edit {
        chat: AB_CHAT.to_owned(),
        message: id.clone(),
        text: "edited".to_owned(),
        wait: true,
    });
    assert_eq!(sent(&row, "edit"), id);
    assert!(matches!(&lab.told_b()[..], [(EventKind::Edit, content)] if content.contains("edited")));

    let reaction = |react: bool| {
        let (chat, message, reaction, wait) = (AB_CHAT.to_owned(), id.clone(), " like ".to_owned(), false);
        if react {
            Request::React {
                chat,
                message,
                reaction,
                wait,
            }
        } else {
            Request::Unreact {
                chat,
                message,
                reaction,
                wait,
            }
        }
    };
    assert_eq!(sent(&lab.row(reaction(true)), "add_reaction"), id);
    assert_eq!(sent(&lab.row(reaction(false)), "remove_reaction"), id);
    let kinds: Vec<EventKind> = lab.told_b().into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, [EventKind::Reaction, EventKind::Reaction]);

    let row = lab.row(Request::Delete {
        chat: AB_CHAT.to_owned(),
        message: id.clone(),
        wait: false,
    });
    assert_eq!(sent(&row, "delete"), id);
    assert!(matches!(&lab.told_b()[..], [(EventKind::Delete, _)]));
}

#[test]
fn the_account_s_own_notes_take_a_send_an_edit_a_reaction_and_a_delete_as_a_chat_does() {
    let lab = Lab::start(false);
    // the notes are in no conversation list: stored as the chat view stores them when it opens them
    lab.requests.sync_conversation(NOTES).blocking_wait().unwrap();
    let wrote = |row: &OutboxRow, write: &str| {
        assert_eq!(
            (row.write.as_str(), row.state, row.chat.as_deref()),
            (write, OutboxState::Sent, Some(NOTES)),
            "{row:?}"
        );
        row.message.clone().expect("the note it wrote")
    };
    let id = wrote(
        &lab.row(Request::Send {
            chat: NOTES.to_owned(),
            text: "a note to self".to_owned(),
            wait: false,
        }),
        "send",
    );
    let note = |lab: &Lab| {
        let notes = lab.mock.notes(LAB_A);
        notes
            .into_iter()
            .find(|note| note["id"] == id.as_str())
            .expect("the note")
    };
    assert!(note(&lab)["content"].as_str().unwrap().contains("a note to self"));

    let row = lab.row(Request::Edit {
        chat: NOTES.to_owned(),
        message: id.clone(),
        text: "a better note".to_owned(),
        wait: true,
    });
    assert_eq!(wrote(&row, "edit"), id);
    assert!(note(&lab)["content"].as_str().unwrap().contains("a better note"));

    let row = lab.row(Request::React {
        chat: NOTES.to_owned(),
        message: id.clone(),
        reaction: "like".to_owned(),
        wait: false,
    });
    assert_eq!(wrote(&row, "add_reaction"), id);
    assert!(note(&lab)["properties"]["emotions"].to_string().contains("like"));

    let row = lab.row(Request::Delete {
        chat: NOTES.to_owned(),
        message: id.clone(),
        wait: false,
    });
    assert_eq!(wrote(&row, "delete"), id);
    assert_eq!(note(&lab)["content"], "");
    assert!(lab.mock.notes(LAB_B).is_empty(), "the notes are a's alone");
    assert!(lab.mock.take_events(LAB_B).is_empty(), "nobody else is told");
}

#[test]
fn an_edit_of_a_reply_keeps_its_quote_ahead_of_the_new_text() {
    let lab = Lab::start(false);
    let quoted = lab.mock.post_as(LAB_B, AB_CHAT, "<p>from b</p>").unwrap();
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();
    let message = lab
        .requests
        .message(AB_CHAT, quoted.as_str())
        .blocking_wait()
        .unwrap()
        .expect("b's message stored");
    // a's reply to it, as the compose box sends one
    let quote = compose::Quoted::of(&message, "bot-b").unwrap().node();
    let reply = lab
        .requests
        .write(Action::Send {
            conversation: AB_CHAT.to_owned(),
            text: writes::message_text("my reply", &[quote]).unwrap(),
        })
        .blocking_wait()
        .unwrap();
    let id = sent(&writes::row_of(&reply), "send");
    let itemid = format!("itemid=\"{quoted}\"");
    assert!(
        matches!(&lab.told_b()[..], [(EventKind::Message, content)] if content.contains(&itemid)),
        "the reply quotes b's message"
    );

    let row = lab.row(Request::Edit {
        chat: AB_CHAT.to_owned(),
        message: id.clone(),
        text: "edited reply".to_owned(),
        wait: false,
    });
    assert_eq!(sent(&row, "edit"), id);
    let told = lab.told_b();
    let [(EventKind::Edit, content)] = &told[..] else {
        panic!("one edit: {told:?}");
    };
    let quote_at = content.find(&itemid).expect("the quote kept");
    let text_at = content.find("edited reply").expect("the new text");
    assert!(quote_at < text_at, "{content}");
    assert!(!content.contains("my reply"), "{content}");
}

#[test]
fn a_text_that_shows_nothing_or_is_too_long_and_a_message_not_there_are_refused_before_any_row() {
    let lab = Lab::start(false);

    for empty in ["", " \n\t\n "] {
        let error = lab.refused(send(empty, false));
        assert_eq!(error.code, ErrorCode::BadRequest, "{empty:?}");
        assert!(error.message.contains("empty"), "{error:?}");
    }
    let error = lab.refused(send(&"long ".repeat(40_000), false));
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.contains("content longer than"), "{error:?}");

    let error = lab.refused(Request::Edit {
        chat: AB_CHAT.to_owned(),
        message: "1".to_owned(),
        text: "edited".to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.contains("no message 1"), "{error:?}");
    let error = lab.refused(Request::Send {
        chat: "no such chat".to_owned(),
        text: "hi".to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::UnknownChat);
    let error = lab.refused(Request::React {
        chat: AB_CHAT.to_owned(),
        message: "1".to_owned(),
        reaction: " ".to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(lab.told_b().is_empty(), "nothing went out");
}

#[test]
fn read_marks_the_chat_read_up_to_b_s_newest_message_once() {
    let lab = Lab::start(false);
    let read = || Request::MarkRead {
        chat: AB_CHAT.to_owned(),
        wait: false,
    };
    lab.mock.post_as(LAB_B, AB_CHAT, "<p>from b</p>").unwrap();
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();

    let row = lab.row(read());
    assert_eq!(
        (row.write.as_str(), row.state),
        ("read_receipt", OutboxState::Sent),
        "{row:?}"
    );
    let receipts: Vec<EventKind> = lab
        .mock
        .take_events(LAB_B)
        .into_iter()
        .filter(|event| event.conversation == AB_CHAT)
        .map(|event| event.kind)
        .collect();
    assert_eq!(receipts, [EventKind::ReadReceipt]);

    // the horizon moved there: nothing more to mark
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();
    assert_eq!(lab.ask(read()).unwrap(), Response::Done);
}

#[test]
fn read_finds_b_s_message_under_more_than_a_page_of_entries_that_don_t_count() {
    let lab = Lab::start(false);
    let id = lab.mock.post_as(LAB_B, AB_CHAT, "<p>from b</p>").unwrap();
    for _ in 0..=writes::READ_PAGE {
        lab.mock
            .add_message(
                AB_CHAT,
                (LAB_B, "bot-b"),
                "ThreadActivity/TopicUpdate",
                "<topicupdate/>",
            )
            .unwrap();
    }
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();

    let row = lab.row(Request::MarkRead {
        chat: AB_CHAT.to_owned(),
        wait: false,
    });
    assert_eq!(
        (row.write.as_str(), row.state),
        ("read_receipt", OutboxState::Sent),
        "{row:?}"
    );
    // read up to b's message, under the entries that came after it
    let message = lab.requests.message(AB_CHAT, id).blocking_wait().unwrap().unwrap();
    let horizon = lab.requests.read_horizon(AB_CHAT).blocking_wait().unwrap().unwrap();
    assert_eq!(horizon.last_read_arrival, message.arrival_time);
}

#[test]
fn read_refuses_a_channel_and_sends_nothing() {
    let lab = Lab::start(false);
    lab.mock
        .add_message(CHANNEL, (LAB_B, "bot-b"), "RichText/Html", "<p>a post from b</p>")
        .unwrap();
    lab.requests.sync_conversation(CHANNEL).blocking_wait().unwrap();
    let writes = || {
        lab.runtime
            .block_on(lab.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() != "GET")
            .count()
    };
    let before = writes();
    let horizon = || lab.requests.read_horizon(CHANNEL).blocking_wait().unwrap();
    let horizon_before = horizon();

    let error = lab.refused(Request::MarkRead {
        chat: CHANNEL.to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::BadRequest, "{error:?}");
    assert!(error.message.contains("channel"), "{error:?}");
    assert_eq!(writes(), before, "no receipt went out");
    assert_eq!(horizon(), horizon_before, "the channel's horizon stays");
}

#[test]
fn new_chat_makes_a_group_chat_and_resend_sends_a_failed_row_again_never_a_sent_one() {
    let lab = Lab::start(false);

    let row = lab.row(Request::NewChat {
        people: vec![LAB_B.to_owned(), format!(" {LAB_C} "), String::new()],
        wait: true,
    });
    assert_eq!(
        (row.write.as_str(), row.state),
        ("create_chat", OutboxState::Sent),
        "{row:?}"
    );
    let made = row.chat.clone().expect("the chat made");
    assert_eq!(row.message.as_deref(), Some(made.as_str()));
    assert!(
        lab.mock
            .take_events(LAB_B)
            .iter()
            .any(|event| event.kind == EventKind::ChatCreated && event.conversation == made),
        "b is told of the chat"
    );
    let nobody = lab.refused(Request::NewChat {
        people: vec![" ".to_owned()],
        wait: false,
    });
    assert_eq!(nobody.code, ErrorCode::BadRequest);
    // a UPN is looked for by the service's people search, which the mock doesn't serve
    let unknown = lab.refused(Request::NewChat {
        people: vec!["nobody@lab.example.test".to_owned()],
        wait: false,
    });
    assert_eq!(unknown.code, ErrorCode::Engine, "{unknown:?}");

    // a's notifications take no message: the send fails for good, its error told; sent again, it fails again
    let failed = lab.row(Request::Send {
        chat: NOTIFICATIONS.to_owned(),
        text: "again".to_owned(),
        wait: true,
    });
    assert_eq!(
        (failed.write.as_str(), failed.state),
        ("send", OutboxState::Failed),
        "{failed:?}"
    );
    assert!(
        failed.error.as_deref().is_some_and(|error| error.contains("403")),
        "{failed:?}"
    );
    let again = lab.row(Request::Resend {
        row: failed.id,
        wait: true,
    });
    assert_eq!((again.id, again.state), (failed.id, OutboxState::Failed), "{again:?}");

    // a row sent is sent once
    let once = lab.row(send("once", false));
    sent(&once, "send");
    let error = lab.refused(Request::Resend {
        row: once.id,
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::Engine, "{error:?}");
}

#[test]
fn the_read_only_app_refuses_every_write_and_sends_nothing() {
    let lab = Lab::start(true);
    let message = || "1".to_owned();
    let chat = || AB_CHAT.to_owned();
    let writes = [
        send("hi", false),
        Request::Edit {
            chat: chat(),
            message: message(),
            text: "hi".to_owned(),
            wait: false,
        },
        Request::Delete {
            chat: chat(),
            message: message(),
            wait: false,
        },
        Request::React {
            chat: chat(),
            message: message(),
            reaction: "like".to_owned(),
            wait: false,
        },
        Request::Unreact {
            chat: chat(),
            message: message(),
            reaction: "like".to_owned(),
            wait: false,
        },
        Request::MarkRead {
            chat: chat(),
            wait: false,
        },
        Request::NewChat {
            people: vec![LAB_B.to_owned()],
            wait: false,
        },
        Request::Resend { row: 1, wait: false },
    ];
    for write in writes {
        assert_eq!(lab.refused(write).code, ErrorCode::RefusedReadOnly);
    }
    let not_reads: Vec<String> = lab
        .runtime
        .block_on(lab.mock.received_requests())
        .iter()
        .filter(|request| request.method.as_str() != "GET")
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .collect();
    assert_eq!(not_reads, Vec::<String>::new());
}

fn entry(id: i64, state: teams_store::OutboxState) -> OutboxEntry {
    OutboxEntry {
        id,
        run_id: 1,
        client_message_id: "1".to_owned(),
        conversation_id: Some(AB_CHAT.to_owned()),
        kind: OutboxKind::Send,
        payload: String::new(),
        state,
        attempts: 1,
        attempts_at_resend: 0,
        failed_checks: 0,
        not_before: None,
        last_error: None,
        message_id: None,
        created_at: 1,
        updated_at: 1,
        unreadable_seen_at: None,
    }
}

/// `writes::settled` of `entry` run on the app's foreground; its outcome once it has one.
fn settling(lab: &Lab, entry: OutboxEntry) -> mpsc::Receiver<Result<OutboxEntry, CtlError>> {
    let bridge = lab.bridge();
    let chats = lab.cx.read(|cx| bridge.read(cx).models().chats.clone());
    let (tell, told) = mpsc::channel();
    lab.cx.update(|cx| {
        cx.spawn(async move |cx: &mut AsyncApp| {
            let _ = tell.send(writes::settled(&chats, entry, cx).await);
        })
        .detach();
    });
    lab.cx.run_until_parked();
    told
}

#[test]
fn a_wait_ends_when_the_engine_tells_the_row_settled_or_at_its_bound() {
    use teams_store::OutboxState as Stored;

    let lab = Lab::start(false);
    let told = settling(&lab, entry(901, Stored::Pending));
    assert!(told.try_recv().is_err(), "a pending row waits");
    let bridge = lab.bridge();
    lab.cx.update(|cx| {
        bridge.update(cx, |bridge, cx| {
            bridge.tell(Event::Outbox(entry(901, Stored::Sending)), cx);
        });
    });
    lab.cx.run_until_parked();
    assert!(told.try_recv().is_err(), "an attempt in flight waits");
    lab.cx.update(|cx| {
        bridge.update(cx, |bridge, cx| {
            bridge.tell(Event::Outbox(entry(901, Stored::Sent)), cx);
        });
    });
    lab.cx.run_until_parked();
    assert_eq!(told.try_recv().unwrap().unwrap().state, Stored::Sent);

    // told already before the wait began
    let told = settling(&lab, entry(901, Stored::Pending));
    assert_eq!(told.try_recv().unwrap().unwrap().state, Stored::Sent);

    let told = settling(&lab, entry(902, Stored::Pending));
    lab.cx.executor().advance_clock(WRITE_WAIT);
    lab.cx.run_until_parked();
    let error = told.try_recv().unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.message.contains("it is queued"), "{error:?}");
}

/// `writes::within_answer` of `write` run on the app's foreground; its outcome once it has one.
fn bounded(
    lab: &Lab,
    write: impl Future<Output = Result<i64, CtlError>> + 'static,
) -> mpsc::Receiver<Result<i64, CtlError>> {
    let (tell, told) = mpsc::channel();
    lab.cx.update(|cx| {
        cx.spawn(async move |cx: &mut AsyncApp| {
            let executor = cx.background_executor().clone();
            let _ = tell.send(writes::within_answer(write, &executor).await);
        })
        .detach();
    });
    lab.cx.run_until_parked();
    told
}

#[test]
fn a_write_the_engine_doesn_t_answer_is_answered_at_its_bound_as_one_that_may_still_go_out() {
    let lab = Lab::start(false);
    assert_eq!(bounded(&lab, future::ready(Ok(7))).try_recv().unwrap().unwrap(), 7);

    let told = bounded(&lab, future::pending());
    lab.cx
        .executor()
        .advance_clock(WRITE_ANSWER.checked_sub(Duration::from_secs(1)).unwrap());
    lab.cx.run_until_parked();
    assert!(told.try_recv().is_err(), "it waits up to its bound");
    lab.cx.executor().advance_clock(Duration::from_secs(1));
    lab.cx.run_until_parked();
    let error = told.try_recv().unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.message.contains("may still go out"), "{error:?}");
    assert!(error.message.contains("--kind outbox"), "{error:?}");
}

#[test]
fn a_row_settles_sent_failed_or_once_it_may_have_arrived() {
    use teams_store::OutboxState as Stored;

    for (state, settled) in [
        (Stored::Pending, false),
        (Stored::Sending, false),
        (Stored::Sent, true),
        (Stored::Denied, true),
        (Stored::Failed, true),
    ] {
        assert_eq!(is_settled(&entry(1, state)), settled, "{state:?}");
    }
    let may_have_arrived = OutboxEntry {
        unreadable_seen_at: Some(1),
        ..entry(1, Stored::Pending)
    };
    assert!(is_settled(&may_have_arrived));
    // a new chat's row names the chat once made
    let made = OutboxEntry {
        kind: OutboxKind::CreateChat,
        conversation_id: None,
        message_id: Some("19:made@thread.v2".to_owned()),
        ..entry(1, Stored::Sent)
    };
    assert_eq!(writes::row_of(&made).chat.as_deref(), Some("19:made@thread.v2"));
}

#[test]
fn sync_answers_what_the_catch_up_stored() {
    let lab = Lab::start(false);
    lab.mock.post_as(LAB_B, AB_CHAT, "<p>while away</p>").unwrap();

    let Response::Synced(report) = lab.ask(Request::Sync).unwrap() else {
        panic!("a sync answers its report");
    };
    assert!(report.conversations >= 1, "{report:?}");
    assert!(report.messages >= 1, "{report:?}");
    assert_eq!(
        (report.failed, report.unreadable, report.caught_up),
        (0, 0, true),
        "{report:?}"
    );
    // stored: the chat holds b's message now
    let Response::Messages { messages, .. } = lab
        .ask(Request::Messages {
            chat: AB_CHAT.to_owned(),
            limit: Some(5),
            before: None,
        })
        .unwrap()
    else {
        panic!("the chat's messages");
    };
    assert!(
        messages.iter().any(|message| message.text == "while away"),
        "{messages:?}"
    );
}

#[test]
fn a_catch_up_report_counts_what_it_did() {
    let mut report = teams_core::CatchUpReport {
        conversations: 4,
        ..teams_core::CatchUpReport::default()
    };
    report.messages.inserted = 2;
    report.messages.updated = 1;
    report.messages.unchanged = 9;
    assert_eq!(
        writes::sync_report(&report),
        SyncReport {
            conversations: 4,
            messages: 3,
            failed: 0,
            unreadable: 0,
            caught_up: true,
        }
    );
}

#[test]
fn the_read_only_app_tells_its_status_and_syncs_but_sets_no_status() {
    let lab = Lab::start(true);
    let Response::OwnStatus(status) = lab.ask(Request::GetStatus).unwrap() else {
        panic!("its status");
    };
    assert_eq!(status.manual, ManualStatus::Automatic);
    let error = lab.refused(Request::SetStatus {
        status: StatusChoice::Busy,
    });
    assert_eq!(error.code, ErrorCode::RefusedReadOnly);
    assert!(matches!(lab.ask(Request::Sync), Ok(Response::Synced(_))));
    let not_reads: Vec<String> = lab
        .runtime
        .block_on(lab.mock.received_requests())
        .iter()
        .filter(|request| request.method.as_str() != "GET")
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .collect();
    assert_eq!(not_reads, Vec::<String>::new());
}

const UPN_B: &str = "bot-b@lab.example.test";

/// One account's mock token, for its transport and its push channel alike.
struct PushTokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for PushTokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

impl TrouterCredentials for PushTokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

/// The app of lab accounts a and b on the mock, each engine with its push channel on the mock's Trouter (so its own
/// presence reports, to the mock's UPS), the first run's screen confirmed.
struct StatusLab {
    mock: ChatServiceMock,
    socket: PathBuf,
    desktop: Entity<Desktop>,
    cx: TestAppContext,
    _trouter: TrouterMock,
    _dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl StatusLab {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let trouter = mock.start_trouter().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let a = started_as(pushing(&mock, &trouter, &dir, LAB_A), UPN, LAB_A, "bot-a");
        let b = started_as(pushing(&mock, &trouter, &dir, LAB_B), UPN_B, LAB_B, "bot-b");
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let cx = TestAppContext::single();
        let launch = Launch {
            first: a,
            others: vec![b],
            settings: app::settings(SettingsFile::new(dir.path().join("settings.toml")), None),
            bus: Err(BusError::NotGiven),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap(),
                floor: CallFloor::new(),
                devices: None,
            },
            read_only: false,
            trace: Arc::new(ViewTrace::off()),
            ctl: Some(socket.clone()),
        };
        let Ok(launched) = cx.update(|cx| app::launch(cx, launch)) else {
            panic!("the window opens");
        };
        let desktop = launched.desktop;
        cx.update(|cx| {
            let settings = desktop.read(cx).settings().clone();
            settings.update(cx, teams_ui::SettingsModel::confirm_first_run);
        });
        let lab = Self {
            mock,
            socket,
            desktop,
            cx,
            _trouter: trouter,
            _dir: dir,
            _runtime: runtime,
        };
        // each push channel registered: each own presence reports, Available first
        lab.until(|lab, cx| {
            [LAB_A, LAB_B]
                .iter()
                .all(|mri| lab.reporting(mri, cx) == Reporting::Available)
        });
        lab
    }

    fn ask(&self, account: Option<&str>, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, account, request)
    }

    /// `status set` of `status` for `account`, answered with the status it left.
    fn set(&self, account: Option<&str>, status: StatusChoice) -> OwnStatus {
        match self.ask(account, Request::SetStatus { status }) {
            Ok(Response::OwnStatus(own)) => own,
            other => panic!("{status:?}: {other:?}"),
        }
    }

    fn get(&self, account: Option<&str>) -> OwnStatus {
        match self.ask(account, Request::GetStatus) {
            Ok(Response::OwnStatus(own)) => own,
            other => panic!("{other:?}"),
        }
    }

    /// What account `mri`'s own presence reports.
    fn reporting(&self, mri: &str, cx: &App) -> Reporting {
        self.desktop
            .read(cx)
            .own_presence_of(mri)
            .unwrap()
            .read(cx)
            .reporting(cx)
    }

    /// The manual statuses UPS took for `mri`, in order (the endpoint's `PUT`s left out).
    fn manual_writes(&self, mri: &str) -> Vec<AvailabilityChange> {
        self.mock
            .own_availability()
            .into_iter()
            .filter(|write| write.account == mri && !matches!(write.change, AvailabilityChange::Endpoint { .. }))
            .map(|write| write.change)
            .collect()
    }

    /// The app stepped until `done`.
    fn until(&self, mut done: impl FnMut(&Self, &App) -> bool) {
        pump_until(&self.cx, Duration::ZERO, |cx| done(self, cx));
    }

    /// The app stepped until UPS took `count` manual statuses for `mri`.
    fn until_written(&self, mri: &str, count: usize) -> Vec<AvailabilityChange> {
        self.until(|lab, _| lab.manual_writes(mri).len() >= count);
        self.manual_writes(mri)
    }
}

impl Drop for StatusLab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// Account `mri`'s engine on the mock, its fence its own, its transport given the presence base, its push channel on
/// the mock's Trouter.
fn pushing(mock: &ChatServiceMock, trouter: &TrouterMock, dir: &tempfile::TempDir, mri: &str) -> EngineHandle {
    let tokens = Arc::new(PushTokens {
        token: mock.token(mri).unwrap(),
        renewals: broadcast::channel(4).0,
    });
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .for_account(Mri::new(mri))
            .with_presence(PRESENCE_BASE)
            .unwrap()
    };
    let transport = Arc::new(http());
    let push = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
        .unwrap()
        .with_options(Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(50),
            backoff_jitter_percent: 0,
            ..Options::default()
        });
    let store = dir.path().join(format!("{}.sqlite", mri.replace(':', "-")));
    engine::start(Parts {
        calls: None,
        store: Store::open(&store).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(transport, Author::new(Mri::new(mri), "lab")),
        push: Some(Push::new(push)),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

fn manual(status: &str) -> AvailabilityChange {
    AvailabilityChange::Manual(status.to_owned())
}

#[test]
fn status_set_reports_each_choice_as_the_status_menu_does_and_reset_hands_it_back() {
    let lab = StatusLab::start();
    assert_eq!(
        lab.get(None),
        OwnStatus {
            reported: "available".to_owned(),
            manual: ManualStatus::Automatic,
        }
    );

    let steps = [
        (StatusChoice::Busy, "busy", ManualStatus::Busy, manual("Busy")),
        (
            StatusChoice::Dnd,
            "do-not-disturb",
            ManualStatus::Dnd,
            manual("DoNotDisturb"),
        ),
        (StatusChoice::Away, "away", ManualStatus::Away, manual("Away")),
        (
            StatusChoice::Available,
            "available",
            ManualStatus::Available,
            manual("Available"),
        ),
        (
            StatusChoice::Reset,
            "available",
            ManualStatus::Automatic,
            AvailabilityChange::Reset,
        ),
    ];
    for (count, (choice, reported, chosen, written)) in steps.into_iter().enumerate() {
        let own = lab.set(None, choice);
        assert_eq!((own.reported.as_str(), own.manual), (reported, chosen), "{choice:?}");
        let writes = lab.until_written(LAB_A, count + 1);
        assert_eq!(writes.last(), Some(&written), "{choice:?}: {writes:?}");
        assert_eq!(writes.len(), count + 1, "{choice:?}: one write each: {writes:?}");
        assert_eq!(lab.get(None), own, "{choice:?}: as the app tells it after");
    }
    assert!(lab.manual_writes(LAB_B).is_empty(), "b's status untouched");
}

#[test]
fn a_status_set_survives_another_setting_s_change_and_b_s_leaves_a_s_alone() {
    let lab = StatusLab::start();
    lab.set(None, StatusChoice::Busy);
    lab.until_written(LAB_A, 1);

    // another setting changed: the saved choice holds, nothing is sent again
    lab.cx.update(|cx| {
        let settings = lab.desktop.read(cx).settings().clone();
        settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.idle_minutes += 1, cx);
        });
    });
    let busy = OwnStatus {
        reported: "busy".to_owned(),
        manual: ManualStatus::Busy,
    };
    assert_eq!(lab.get(None), busy);

    // b's own, by its UPN: a's choice and a's reports as they were
    let b = lab.set(Some(UPN_B), StatusChoice::Dnd);
    assert_eq!((b.reported.as_str(), b.manual), ("do-not-disturb", ManualStatus::Dnd));
    assert_eq!(lab.until_written(LAB_B, 1), [manual("DoNotDisturb")]);
    assert_eq!(lab.get(None), busy);
    assert_eq!(lab.get(Some("1")), b, "b by its index");
    assert_eq!(lab.manual_writes(LAB_A), [manual("Busy")]);
}
