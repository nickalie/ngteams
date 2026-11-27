//! QA of task 988: a person lists and downloads the attachments of chat messages with `ngteamsctl`, against the
//! running app. Each `ngteamsctl` run is a process of its own: this test binary started again in a folder of the
//! test's, which runs `teams_ctl::cli` as the `ngteamsctl` binary does (its arguments, its current folder, its
//! environment, its stdout, its stderr and its exit code). The app runs in this process.
//!
//! - **On Microsoft 365**: lab account a's app, its engine on wiremock as Graph, SharePoint and AMS. A message with
//!   one SharePoint file, one with a file and an AMS picture, one with no attachment, and one with a file and a file
//!   that is not on the service. The read-only app downloads too.
//! - **On an own server**: an in-process `ngteams-server`. b uploads a file and sends it to a, as the compose box
//!   does. a's app lists it and downloads it from the server.
//!
//! Every id, link, name and byte is made up. Nothing leaves loopback.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]
#![cfg(target_os = "linux")]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;
#[path = "support/own_server.rs"]
#[expect(
    dead_code,
    reason = "the own server's tests share this support; this one adds users only"
)]
mod own_server;

use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use clap::Parser as _;
use ctl_support::{pump_until_done, quit};
use gpui::TestAppContext;
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::setup::Setup;
use own_server::{OwnServer, PASSWORD};
use serde_json::json;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, Action, EngineOptions, Parts};
use teams_core::{CallFloor, Outbox};
use teams_ctl::cli::{self, Cli};
use teams_ctl::path::Env;
use teams_ctl::protocol::{AttachmentKind, Outcome, Response, ResponseLine};
use teams_mock::{CHAT_BASE, LAB_A, LAB_B, TENANT};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Credentials, FileDownload, HttpTransport, Mri};
use teams_store::{Conversation, ConversationKind, Member, Message, OutboxState, Store};
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The JSON array of arguments that makes this binary run as `ngteamsctl`.
const CHILD_ARGS: &str = "CTL_DOWNLOAD_CLI_ARGS";
/// The files the child writes its stdout and its stderr in.
const CHILD_OUT: &str = "CTL_DOWNLOAD_CLI_OUT";
const CHILD_ERR: &str = "CTL_DOWNLOAD_CLI_ERR";
/// The name of the test that is the child.
const CHILD_TEST: &str = "ngteamsctl_child";

const UPN: &str = "bot-a@lab.example.test";
const CHAT: &str = "19:00000000000000000000000000cli0download@thread.v2";
const TOPIC: &str = "Downloads";
const AMS_BASE: &str = "https://us-prod.asyncgw.teams.microsoft.com";
const SHARE: &str = "https://contoso-my.sharepoint.com/:b:/g/personal/made_up_user_contoso_test/MadeUpCliDownload01";
const DOWNLOAD: &str = "/personal/made_up_user_contoso_test/_layouts/15/cli-download.aspx";
const MISSING: &str = "https://contoso-my.sharepoint.com/:b:/g/personal/made_up_user_contoso_test/MadeUpCliMissing01";
const OBJECT: &str = "0-eus-d9-0000000000000000000000000000c11d";
const FILE_BYTES: &[u8] = b"made-up cli report bytes";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nmade-up cli picture";
/// The message with one SharePoint file.
const ONE: &str = "1700000000000";
/// The message with a SharePoint file and an AMS picture in its text.
const BOTH: &str = "1700000001000";
/// The message with text only.
const BARE: &str = "1700000002000";
/// The message with a file, then a file that is not on the service.
const BROKEN: &str = "1700000003000";

/// When this binary runs as `ngteamsctl` (see the module docs), it does what the `ngteamsctl` binary does.
#[test]
#[ignore = "run by ngteamsctl() as the ngteamsctl binary"]
fn ngteamsctl_child() {
    let args: Vec<String> = serde_json::from_str(&std::env::var(CHILD_ARGS).unwrap()).unwrap();
    let mut out = std::fs::File::create(std::env::var(CHILD_OUT).unwrap()).unwrap();
    let mut err = std::fs::File::create(std::env::var(CHILD_ERR).unwrap()).unwrap();
    let code = match Cli::try_parse_from(std::iter::once("ngteamsctl".to_owned()).chain(args)) {
        Ok(parsed) => cli::run(&parsed, &Env::from_process(), &mut out, &mut err) as i32,
        Err(error) => {
            use std::io::Write as _;
            let _ = write!(err, "{error}");
            error.exit_code()
        }
    };
    std::process::exit(code);
}

/// What an `ngteamsctl` run printed and how it exited.
#[derive(Debug)]
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// `ngteamsctl <args>` run in `cwd` against `socket`, while the test thread steps the app.
fn ngteamsctl(cx: &TestAppContext, socket: &Path, cwd: &Path, args: &[&str]) -> Run {
    std::fs::create_dir_all(cwd).unwrap();
    let logs = tempfile::tempdir().unwrap();
    let (out, err) = (logs.path().join("out"), logs.path().join("err"));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD_TEST, "--ignored", "--test-threads=1", "--quiet"])
        .current_dir(cwd)
        .env_clear()
        // a coverage run counts the child's runs too, and its profile stays out of the download folder
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env(CHILD_ARGS, serde_json::to_string(args).unwrap())
        .env(CHILD_OUT, &out)
        .env(CHILD_ERR, &err)
        .env("NGTEAMS_CTL_SOCKET", socket)
        .env("HOME", logs.path())
        .env("XDG_RUNTIME_DIR", logs.path())
        .env("TZ", "UTC");
    let thread = std::thread::spawn(move || command.output().unwrap());
    pump_until_done(cx, &thread);
    let output = thread.join().unwrap();
    // the harness of a child that panicked failed its one test; its output holds the panic's message
    assert!(
        output.status.code() != Some(101),
        "ngteamsctl {args:?} panicked: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: std::fs::read_to_string(out).unwrap_or_default(),
        stderr: std::fs::read_to_string(err).unwrap_or_default(),
    }
}

/// Placeholder tokens for every audience.
struct Tokens;

impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(format!("placeholder-token-for-{audience}"))))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

fn entry(name: &str, link: &str) -> serde_json::Value {
    json!({"@type": "http://schema.skype.com/File", "fileName": name, "fileType": name.rsplit('.').next(),
        "objectUrl": link, "fileInfo": {"shareUrl": link, "fileUrl": link}})
}

fn message(store: &mut Store, id: &str, html: &str, files: &serde_json::Value) {
    let at: i64 = id.parse().unwrap();
    store
        .upsert_message(
            &Message {
                conversation_id: CHAT.to_owned(),
                id: id.to_owned(),
                version: at,
                client_message_id: None,
                sender: Some(LAB_B.to_owned()),
                sender_name: Some("bot-b".to_owned()),
                message_type: "RichText/Html".to_owned(),
                content: html.to_owned(),
                properties: Some(json!({ "files": files.to_string() }).to_string()),
                arrival_time: at,
                compose_time: Some(at),
                edit_time: None,
                delete_time: None,
            },
            None,
        )
        .unwrap();
}

fn seed(store: &mut Store) {
    store
        .upsert_conversation(&Conversation {
            id: CHAT.to_owned(),
            kind: ConversationKind::Group,
            topic: Some(TOPIC.to_owned()),
            version: 1,
            last_activity: None,
        })
        .unwrap();
    store
        .replace_members(CHAT, 1, &[Member::new(LAB_A, None), Member::new(LAB_B, None)])
        .unwrap();
    message(store, ONE, "<p>the report</p>", &json!([entry("report.pdf", SHARE)]));
    let picture = format!(
        r#"<p>both<img src="https://us-api.asm.skype.com/v1/objects/{OBJECT}/views/imgo" itemtype="http://schema.skype.com/AMSImage" itemid="{OBJECT}" alt="image"></p>"#
    );
    message(store, BOTH, &picture, &json!([entry("notes.pdf", SHARE)]));
    message(store, BARE, "<p>no files here</p>", &json!([]));
    let broken = json!([entry("first.pdf", SHARE), entry("gone.pdf", MISSING)]);
    message(store, BROKEN, "<p>broken</p>", &broken);
}

async fn serve(server: &MockServer) {
    let item = json!({
        "id": "made-up-cli-file", "size": FILE_BYTES.len(),
        "@microsoft.graph.downloadUrl": format!("https://contoso-my.sharepoint.com{DOWNLOAD}?UniqueId=made-up&tempauth=made-up"),
    });
    Mock::given(method("GET"))
        .and(path(format!(
            "/v1.0/shares/{}/driveItem",
            FileDownload::of(SHARE).unwrap().share()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(item))
        .mount(server)
        .await;
    let range = format!("bytes 0-{}/{}", FILE_BYTES.len() - 1, FILE_BYTES.len());
    Mock::given(method("GET"))
        .and(path(DOWNLOAD))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("Content-Range", range)
                .set_body_bytes(FILE_BYTES),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/objects/{OBJECT}/views/imgo")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "image/png"))
        .mount(server)
        .await;
}

/// a's app with its engine on wiremock, serving its control socket.
struct Lab {
    socket: PathBuf,
    cx: TestAppContext,
    dir: tempfile::TempDir,
    _server: MockServer,
    _runtime: tokio::runtime::Runtime,
}

impl Lab {
    fn start(read_only: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(serve(&server));
        let dir = tempfile::tempdir().unwrap();
        let http = || {
            let http = HttpTransport::with_mock_origin(Arc::new(Tokens), CHAT_BASE, &server.uri()).unwrap();
            if read_only { http.read_only() } else { http }
        };
        let mut store = Store::open(&dir.path().join("a.sqlite")).unwrap();
        seed(&mut store);
        let handle = engine::start(Parts {
            calls: None,
            store,
            client: Arc::new(Client::new(http().with_ams(AMS_BASE).unwrap())),
            outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
            push: None,
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })
        .unwrap();
        let mut account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
        account.name = Some("bot-a".to_owned());
        let session = Session::new(
            UPN,
            MemorySecretStore::new(),
            TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/")).unwrap(),
            AuthzClient::with_endpoint("http://127.0.0.1:9/authz").unwrap(),
            Region::default(),
        );
        let first = Started {
            account,
            engine: AccountEngine {
                handle,
                session: Arc::new(session),
            },
            calls: None,
        };
        let setup = Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap();
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let cx = launch(first, setup, dir.path(), &socket, read_only);
        Self {
            socket,
            cx,
            dir,
            _server: server,
            _runtime: runtime,
        }
    }

    /// `ngteamsctl <args>` run in the folder `cwd` of the test's folder.
    fn ctl(&self, cwd: &str, args: &[&str]) -> Run {
        ngteamsctl(&self.cx, &self.socket, &self.folder(cwd), args)
    }

    fn folder(&self, name: &str) -> PathBuf {
        self.dir.path().join("home").join(name)
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// The app launched with `first`, serving its control socket at `socket`.
fn launch(
    first: Started<MemorySecretStore>,
    setup: Setup,
    dir: &Path,
    socket: &Path,
    read_only: bool,
) -> TestAppContext {
    let cx = TestAppContext::single();
    let settings = app::settings(SettingsFile::new(dir.join("settings.toml")), None);
    let launch = Launch {
        first,
        others: Vec::new(),
        settings: if read_only { settings.read_only() } else { settings },
        bus: Err(BusError::NotGiven),
        sign_in_again: Rc::new(|_| Rc::new(|_| {})),
        runner: Runner {
            setup,
            floor: CallFloor::new(),
            devices: None,
        },
        read_only,
        trace: Arc::new(ViewTrace::off()),
        ctl: Some(socket.to_owned()),
    };
    assert!(cx.update(|cx| app::launch(cx, launch)).is_ok(), "the window opens");
    cx.run_until_parked();
    cx
}

/// The names of the files in `folder`, sorted.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn saved_line(path: &Path, size: usize) -> String {
    format!("saved {} ({size} bytes)", path.display())
}

#[test]
fn a_person_lists_the_attachments_of_the_messages_as_text_and_as_json() {
    let lab = Lab::start(false);
    let run = lab.ctl("list", &["messages", TOPIC]);
    assert_eq!(run.code, 0, "{run:?}");
    let line = |text: &str| {
        run.stdout
            .lines()
            .find(|line| line.contains(text))
            .unwrap_or_default()
            .to_owned()
    };
    assert!(
        line("the report").ends_with("the report [1: report.pdf, pdf]"),
        "{run:?}"
    );
    assert!(
        line("both").ends_with(" [1: notes.pdf, pdf] [2: image-2, picture]"),
        "{run:?}"
    );
    assert!(line("no files here").ends_with("no files here"), "{run:?}");
    assert!(
        line("broken").ends_with("broken [1: first.pdf, pdf] [2: gone.pdf, pdf]"),
        "{run:?}"
    );

    let run = lab.ctl("list", &["--json", "messages", TOPIC]);
    assert_eq!(run.code, 0, "{run:?}");
    let answer: ResponseLine = serde_json::from_str(run.stdout.trim()).unwrap();
    let Outcome::Ok(Response::Messages { messages }) = answer.outcome else {
        panic!("{run:?}");
    };
    let both = messages.iter().find(|row| row.id == BOTH).unwrap();
    let rows: Vec<_> = both
        .attachments
        .iter()
        .map(|row| (row.index, row.name.as_str(), row.kind, row.file_type.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            (1, "notes.pdf", AttachmentKind::File, Some("pdf")),
            (2, "image-2", AttachmentKind::Picture, None),
        ]
    );
    let bare = messages.iter().find(|row| row.id == BARE).unwrap();
    assert!(bare.attachments.is_empty(), "{bare:?}");
}

#[test]
fn a_person_downloads_into_the_current_folder_and_into_the_folders_out_names() {
    let lab = Lab::start(false);
    let here = lab.folder("here");
    let run = lab.ctl("here", &["download", TOPIC, ONE]);
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(
        run.stdout,
        saved_line(&here.join("report.pdf"), FILE_BYTES.len()) + "\n"
    );
    assert_eq!(std::fs::read(here.join("report.pdf")).unwrap(), FILE_BYTES);
    assert_eq!(names_in(&here), ["report.pdf"], "no part file is left");

    let relative = here.join("saved/both");
    let run = lab.ctl("here", &["download", TOPIC, BOTH, "--all", "--out", "saved/both"]);
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(
        run.stdout,
        format!(
            "{}\n{}\n",
            saved_line(&relative.join("notes.pdf"), FILE_BYTES.len()),
            saved_line(&relative.join("image-2.png"), PNG.len())
        )
    );
    assert_eq!(std::fs::read(relative.join("notes.pdf")).unwrap(), FILE_BYTES);
    assert_eq!(std::fs::read(relative.join("image-2.png")).unwrap(), PNG);

    let absolute = lab.folder("elsewhere/new");
    let out = absolute.to_str().unwrap();
    let run = lab.ctl("here", &["download", TOPIC, BOTH, "--name", "notes.pdf", "--out", out]);
    assert_eq!(run.code, 0, "{run:?}");
    let run = lab.ctl("here", &["download", TOPIC, BOTH, "--index", "2", "--out", out]);
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(names_in(&absolute), ["image-2.png", "notes.pdf"]);
    assert_eq!(std::fs::read(absolute.join("image-2.png")).unwrap(), PNG);

    let dotted = lab.folder("here/up");
    let run = lab.ctl("here/sub", &["download", TOPIC, ONE, "--out", "../up"]);
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(std::fs::read(dotted.join("report.pdf")).unwrap(), FILE_BYTES);
}

#[test]
fn a_person_replaces_a_file_only_with_force() {
    let lab = Lab::start(false);
    let here = lab.folder("force");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::write(here.join("report.pdf"), b"mine").unwrap();
    let run = lab.ctl("force", &["download", TOPIC, ONE]);
    assert_eq!(run.code, 1, "{run:?}");
    assert!(run.stderr.contains("file_exists"), "{run:?}");
    assert!(run.stdout.is_empty(), "{run:?}");
    assert_eq!(std::fs::read(here.join("report.pdf")).unwrap(), b"mine");

    std::fs::write(here.join("image-2.png"), b"my picture").unwrap();
    let run = lab.ctl("force", &["download", TOPIC, BOTH, "--all"]);
    assert_eq!(run.code, 1, "{run:?}");
    assert!(run.stderr.contains("file_exists"), "{run:?}");
    assert_eq!(
        names_in(&here),
        ["image-2.png", "report.pdf"],
        "the first file is not written either"
    );

    let run = lab.ctl("force", &["download", TOPIC, ONE, "--force"]);
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(std::fs::read(here.join("report.pdf")).unwrap(), FILE_BYTES);
}

#[test]
fn a_person_gets_a_refusal_for_a_bad_selection_and_nothing_is_written() {
    let lab = Lab::start(false);
    let cases: [(&[&str], i32, &str); 8] = [
        (&["download", TOPIC, ONE, "--name", "../report.pdf"], 1, "no_attachment"),
        (
            &["download", TOPIC, ONE, "--name", "sub/report.pdf"],
            1,
            "no_attachment",
        ),
        (&["download", TOPIC, "1799999999999"], 1, "bad_request"),
        (&["download", TOPIC, BARE], 1, "no_attachment"),
        (&["download", TOPIC, BARE, "--all"], 1, "no_attachment"),
        (&["download", TOPIC, BOTH], 1, "no_attachment"),
        (
            &["download", TOPIC, BOTH, "--index", "1", "--all"],
            2,
            "cannot be used with",
        ),
        (
            &["download", TOPIC, BOTH, "--name", "notes.pdf", "--index", "1"],
            2,
            "cannot be used with",
        ),
    ];
    for (args, code, said) in cases {
        let run = lab.ctl("refused", args);
        assert_eq!(run.code, code, "{args:?}: {run:?}");
        assert!(run.stderr.contains(said), "{args:?}: {run:?}");
        assert!(run.stdout.is_empty(), "{args:?}: {run:?}");
    }
    assert!(names_in(&lab.folder("refused")).is_empty());
    assert!(!lab.dir.path().join("report.pdf").exists());
    assert!(!lab.dir.path().join("home/report.pdf").exists());
}

#[test]
fn a_failure_in_the_middle_names_the_files_saved_before_it() {
    let lab = Lab::start(false);
    let here = lab.folder("broken");
    let run = lab.ctl("broken", &["download", TOPIC, BROKEN, "--all"]);
    assert_eq!(run.code, 1, "{run:?}");
    assert!(run.stderr.contains("gone.pdf"), "{run:?}");
    assert!(
        run.stderr
            .contains(&format!("saved before it: {}", here.join("first.pdf").display())),
        "{run:?}"
    );
    assert_eq!(std::fs::read(here.join("first.pdf")).unwrap(), FILE_BYTES);
    assert_eq!(names_in(&here), ["first.pdf"], "no part file is left");
}

#[test]
fn the_read_only_app_serves_the_listing_and_the_download() {
    let lab = Lab::start(true);
    let run = lab.ctl("ro", &["--read-only", "messages", TOPIC]);
    assert_eq!(run.code, 0, "{run:?}");
    assert!(run.stdout.contains("[2: image-2, picture]"), "{run:?}");
    let run = lab.ctl("ro", &["--read-only", "download", TOPIC, BOTH, "--all"]);
    assert_eq!(run.code, 0, "{run:?}");
    let here = lab.folder("ro");
    assert_eq!(std::fs::read(here.join("notes.pdf")).unwrap(), FILE_BYTES);
    assert_eq!(std::fs::read(here.join("image-2.png")).unwrap(), PNG);
}

// --- an own server ---

const A_UPN: &str = "ada@own.example.test";
const B_UPN: &str = "bert@own.example.test";
const OWN_FILE: &str = "rota.txt";
const OWN_BYTES: &[u8] = b"made-up rota: Monday Ada, Tuesday Bert\n";

/// The account `upn` of `server`, signed in under `dir` with its password, and its engine started as the app starts it.
fn own_account(dir: &Path, server: &OwnServer, upn: &str) -> (Setup, Started<MemorySecretStore>) {
    let setup = Setup::new(PersonalPaths::new(dir.join("config"), dir.join("data"))).unwrap();
    let at = accounts_file::OwnServer::parse(&server.origin, &server.ca_file.display().to_string()).unwrap();
    let secrets = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime().unwrap();
    let account = runtime
        .block_on(ngteams::signin::add_own(
            &setup,
            upn,
            &at,
            &Secret::new(PASSWORD),
            &secrets,
        ))
        .unwrap();
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), secrets, false, None).unwrap();
    let started = Started {
        account,
        engine,
        calls: None,
    };
    (setup, started)
}

/// b makes a chat with a and sends `OWN_FILE` there, as the compose box does; the chat's id.
fn b_sends_a_file(engine: &AccountEngine<MemorySecretStore>, a_oid: &str, dir: &Path) -> String {
    let runtime = ngteams::accounts::runtime().unwrap();
    let made = runtime
        .block_on(engine.handle.write(Action::CreateChat {
            others: vec![Mri::new(format!("8:orgid:{a_oid}"))],
            topic: None,
        }))
        .unwrap();
    let chat = made.message_id.unwrap();
    let file = dir.join(OWN_FILE);
    std::fs::write(&file, OWN_BYTES).unwrap();
    let uploaded = runtime.block_on(engine.handle.requests().upload_file(file)).unwrap();
    let row = runtime
        .block_on(engine.handle.write(Action::Send {
            conversation: chat.clone(),
            text: MessageText::plain("the rota").unwrap().with_files(vec![uploaded]),
        }))
        .unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    chat
}

#[test]
fn a_person_downloads_an_own_server_s_file_from_its_server() {
    let server = OwnServer::start().unwrap();
    let a_oid = server.add_user(A_UPN, "Ada Made-Up").unwrap();
    server.add_user(B_UPN, "Bert Made-Up").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (_, bert) = own_account(&dir.path().join("b"), &server, B_UPN);
    let chat = b_sends_a_file(&bert.engine, &a_oid, dir.path());

    let (setup, ada) = own_account(&dir.path().join("a"), &server, A_UPN);
    let socket = dir.path().join("run/ngteams/ctl.sock");
    let cx = launch(ada, setup, &dir.path().join("a"), &socket, false);
    let home = dir.path().join("home");
    let sync = ngteamsctl(&cx, &socket, &home, &["sync"]);
    assert_eq!(sync.code, 0, "{sync:?}");

    let listed = ngteamsctl(&cx, &socket, &home, &["--json", "messages", &chat]);
    assert_eq!(listed.code, 0, "{listed:?}");
    let answer: ResponseLine = serde_json::from_str(listed.stdout.trim()).unwrap();
    let Outcome::Ok(Response::Messages { messages }) = answer.outcome else {
        panic!("{listed:?}");
    };
    let sent = messages
        .iter()
        .find(|row| row.attachments.iter().any(|attachment| attachment.name == OWN_FILE))
        .unwrap_or_else(|| panic!("no message lists {OWN_FILE}: {listed:?}"));
    let text = ngteamsctl(&cx, &socket, &home, &["messages", &chat]);
    assert!(text.stdout.contains(&format!("[1: {OWN_FILE}, text]")), "{text:?}");

    let run = ngteamsctl(&cx, &socket, &home, &["download", &chat, &sent.id]);
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(run.stdout, saved_line(&home.join(OWN_FILE), OWN_BYTES.len()) + "\n");
    assert_eq!(std::fs::read(home.join(OWN_FILE)).unwrap(), OWN_BYTES);
    assert_eq!(names_in(&home), [OWN_FILE]);

    quit(&cx);
    bert.engine.handle.shutdown();
}

#[test]
fn an_ngteamsctl_of_protocol_1_gets_version_mismatch() {
    use std::io::{BufRead as _, BufReader, Write as _};
    let lab = Lab::start(false);
    let socket = lab.socket.clone();
    let thread = std::thread::spawn(move || {
        let mut stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        stream
            .write_all(b"{\"id\":1,\"cmd\":\"hello\",\"protocol\":1,\"client\":\"0.1.0\"}\n")
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        line
    });
    pump_until_done(&lab.cx, &thread);
    let line = thread.join().unwrap();
    let answer: ResponseLine = serde_json::from_str(&line).unwrap();
    let Outcome::Error(error) = answer.outcome else {
        panic!("{line}");
    };
    assert_eq!(error.code, teams_ctl::protocol::ErrorCode::VersionMismatch, "{line}");
}
