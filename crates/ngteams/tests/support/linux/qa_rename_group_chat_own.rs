//! The `own` scene of the group chat rename QA: the real `ngteams-server` binary (TLS with a made-up certificate
//! authority, SQLite) with the made-up users a, b and c. b, a client in the parent brought up as the app brings an own
//! server's account up, makes a group chat of the three. a's app runs in the sandbox. a renames the chat with the
//! header's pencil: b's store has the new name. b renames it, then takes the name off: a's window shows each at once.

use std::path::Path;
use std::thread;
use std::time::Instant;

use serde_json::Value;
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::CallFloor;
use teams_core::account::AccountEngine;
use teams_core::engine::Action;
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use ui_sandbox::SandboxResult;

use super::{POLL, User, WAIT, child, ensure, note};

#[path = "../own_server_binary.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one starts the server and adds users"
)]
mod own_server_binary;
use own_server_binary::{BinaryServer, PASSWORD};

/// The people: user name, display name. Made up.
const A: (&str, &str) = ("ada@own.example.test", "Ada Made-Up");
const B: (&str, &str) = ("bert@own.example.test", "Bert Made-Up");
const C: (&str, &str) = ("cara@own.example.test", "Cara Made-Up");
/// The name b gives the chat it makes.
const FIRST: &str = "Own made-up group";

pub fn password() -> &'static str {
    PASSWORD
}

/// An own server's account signed in with its password and started as the app starts it, with no calls.
fn own_account(dir: &Path, (origin, ca): (&str, &str), upn: &str) -> SandboxResult<AccountEngine<MemorySecretStore>> {
    let paths = ngteams::accounts_file::PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = ngteams::setup::Setup::new(paths)?;
    let server = ngteams::accounts_file::OwnServer::parse(origin, ca)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        upn,
        &server,
        &Secret::new(PASSWORD),
        &store,
    ))?;
    Ok(ngteams::engine::start(
        &setup.paths,
        &account,
        CallFloor::new(),
        store,
        true,
        None,
    )?)
}

pub fn scene() -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        let why = "no ngteams-server binary (NGTEAMS_QA_SERVER_BIN, or cargo build -p ngteams-server)";
        if ui_sandbox::required() {
            return Err(why.into());
        }
        super::say(&format!("own: skipped: {why}"));
        return Ok(());
    };
    let Some(sandbox) = super::sandbox(1280)? else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let server = BinaryServer::start(&binary)?;
    let a_oid = server.add_user(A.0, A.1)?;
    server.add_user(B.0, B.1)?;
    let c_oid = server.add_user(C.0, C.1)?;
    let ca = server.ca_file.to_string_lossy().into_owned();
    let bert = own_account(&dir.path().join("b"), (&server.origin, &ca), B.0)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let b = Bert {
        engine: &bert,
        runtime: &runtime,
    };
    let chat = b.make_group(&[&a_oid, &c_oid])?;
    let env = vec![
        (child::ORIGIN_ENV.to_owned(), server.origin.clone()),
        (child::CA_ENV.to_owned(), ca.clone()),
        (child::OWN_UPN_ENV.to_owned(), A.0.to_owned()),
    ];
    let outcome = User::start(&sandbox, &dir.path().join("a"), "own", &env).and_then(|mut user| {
        let walked = walk(&mut user, &b, &chat);
        user.finish(walked)
    });
    if let Some(evidence) = super::evidence() {
        let _ = std::fs::create_dir_all(&evidence);
        let _ = std::fs::write(evidence.join("qa661-own-server.log"), server.log());
    }
    bert.handle.shutdown();
    outcome.map_err(|error| format!("{error}{}", sandbox.logs()).into())
}

/// b, a client in the parent.
struct Bert<'a> {
    engine: &'a AccountEngine<MemorySecretStore>,
    runtime: &'a tokio::runtime::Runtime,
}

impl Bert<'_> {
    /// b makes a group chat named [`FIRST`] with the users `others` (object ids) and says hello in it; its id.
    fn make_group(&self, others: &[&str]) -> SandboxResult<String> {
        let made = self.runtime.block_on(self.engine.handle.write(Action::CreateChat {
            others: others.iter().map(|oid| Mri::new(format!("8:orgid:{oid}"))).collect(),
            topic: Some(FIRST.to_owned()),
        }))?;
        let chat = made.message_id.ok_or("b's group chat was not made")?;
        self.runtime.block_on(self.engine.handle.write(Action::Send {
            conversation: chat.clone(),
            text: MessageText::plain("Made-up hello to the group")?,
        }))?;
        note(&format!("own: b made the group chat {chat}"));
        Ok(chat)
    }

    fn rename(&self, chat: &str, topic: &str) -> SandboxResult {
        let row = self.runtime.block_on(self.engine.handle.write(Action::RenameChat {
            conversation: chat.to_owned(),
            topic: topic.to_owned(),
        }))?;
        ensure(row.state == teams_store::OutboxState::Sent, || {
            format!("b's rename to {topic:?} is {:?}", row.state)
        })
    }

    /// The name b's store has for `chat`.
    fn topic(&self, chat: &str) -> SandboxResult<Option<String>> {
        let conversation = self
            .runtime
            .block_on(self.engine.handle.conversation(chat.to_owned()))?;
        Ok(conversation.and_then(|conversation| conversation.topic))
    }

    /// Waits until b's store names `chat` `topic`, with no sync asked.
    fn until_topic(&self, chat: &str, topic: &str) -> SandboxResult {
        let start = Instant::now();
        loop {
            let now = self.topic(chat)?;
            if now.as_deref() == Some(topic) {
                return Ok(());
            }
            if start.elapsed() > WAIT {
                return Err(format!("b's store never named the chat {topic:?}: {now:?}").into());
            }
            thread::sleep(POLL);
        }
    }
}

fn walk(user: &mut User<'_>, b: &Bert<'_>, chat: &str) -> SandboxResult {
    user.until("a's group chat listed", |state| User::row(state, chat).is_some())?;
    user.open_by_switcher("made-up group", chat)?;
    user.shot("30-own-group")?;
    user.open_popup(chat, FIRST)?;
    user.type_text("Own renamed by a")?;
    user.keys(&["enter"])?;
    user.until("a's popup closed", |state| state["rename"].is_null())?;
    user.row_named(chat, "Own renamed by a")?;
    b.until_topic(chat, "Own renamed by a")?;
    note("own: a renamed the chat; b's store has the name");
    b.rename(chat, "Own renamed by b")?;
    user.row_named(chat, "Own renamed by b")?;
    user.until("a's line: b changed the group name", |state| {
        super::events_hold(state, "changed the group name to Own renamed by b")
    })?;
    user.shot("31-own-renamed-by-b")?;
    b.rename(chat, "")?;
    let state = user.until("a's chat called by its people", |state| {
        User::row(state, chat).is_some_and(|row| row["name"] != "Own renamed by b")
    })?;
    let row: Value = User::row(&state, chat).cloned().unwrap_or_default();
    note(&format!("own: b took the name off; a's row: {row}"));
    user.until("a's line: removed the group name", |state| {
        super::events_hold(state, "removed the group name")
    })?;
    user.shot("32-own-unnamed")?;
    Ok(())
}
