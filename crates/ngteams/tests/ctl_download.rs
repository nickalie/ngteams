//! The control socket's `download` and the attachments `messages` lists (`ctl::download`), through the real protocol
//! and server, over lab account a's engine on wiremock as Graph, SharePoint and AMS (the transport's `test-support`
//! mock origin): a message with a SharePoint file and a picture in its text, and one whose file name tries to leave
//! the folder, one with two files of one name, the second not there, and one with two files whose names differ only in
//! case. Two more messages each have a file whose download is held: one while a file comes in at the next target, one
//! past the app's answer bound. The files are read by the app's engine, as the chat view reads them, and saved where
//! the client asks. The read-only mode downloads too. Every id, link, name and byte is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ctl_support::{ask, connect, quit};
use gpui::TestAppContext;
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::setup::Setup;
use serde_json::json;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineOptions, Parts};
use teams_core::{CallFloor, Outbox};
use teams_ctl::ClientError;
use teams_ctl::protocol::{
    AttachmentKind, AttachmentRow, CtlError, DOWNLOAD_ANSWER, ErrorCode, Request, Response, SavedFile,
};
use teams_mock::{CHAT_BASE, LAB_A, LAB_B, TENANT};
use teams_proto::{Author, Client, Credentials, FileDownload, HttpTransport, Mri};
use teams_store::{Conversation, ConversationKind, Member, Message, Store};
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const UPN: &str = "bot-a@lab.example.test";
const CHAT: &str = "19:00000000000000000000000000download@thread.v2";
const AMS_BASE: &str = "https://us-prod.asyncgw.teams.microsoft.com";
const SHARE: &str = "https://contoso-my.sharepoint.com/:b:/g/personal/made_up_user_contoso_test/MadeUpCtlDownload01";
const DOWNLOAD: &str = "/personal/made_up_user_contoso_test/_layouts/15/download.aspx";
const OBJECT: &str = "0-eus-d9-0000000000000000000000000000c71d";
const FILE_BYTES: &[u8] = b"made-up report bytes";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nmade-up picture";
/// The message with a file and a picture.
const BOTH: &str = "1700000000000";
/// The message whose file name tries to leave the folder.
const ESCAPE: &str = "1700000001000";
/// The message with two files of one name, the second not on the service.
const TWINS: &str = "1700000002000";
/// The message with two files whose names differ only in case, both on the service.
const CASES: &str = "1700000003000";
/// The message with a picture and a file whose download puts a file at the picture's target first.
const RACE: &str = "1700000004000";
/// The message with a slow file, then a file.
const SLOW: &str = "1700000005000";
const RACE_SHARE: &str = "https://contoso-my.sharepoint.com/:b:/g/personal/made_up_user_contoso_test/MadeUpCtlRace01";
const RACE_DOWNLOAD: &str = "/personal/made_up_user_contoso_test/_layouts/15/race.aspx";
const SLOW_SHARE: &str = "https://contoso-my.sharepoint.com/:b:/g/personal/made_up_user_contoso_test/MadeUpCtlSlow01";
const SLOW_DOWNLOAD: &str = "/personal/made_up_user_contoso_test/_layouts/15/slow.aspx";
/// How long the test waits in real time for a step of the held download.
const WAIT: Duration = Duration::from_secs(30);
const ANOTHER_APPS: &[u8] = b"another app's picture";
const MISSING: &str = "https://contoso-my.sharepoint.com/:b:/g/personal/made_up_user_contoso_test/MadeUpCtlMissing01";

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

/// The chat with its two messages (see the module docs).
fn seed(store: &mut Store) {
    store
        .upsert_conversation(&Conversation {
            id: CHAT.to_owned(),
            kind: ConversationKind::Group,
            topic: Some("Downloads".to_owned()),
            version: 1,
            last_activity: None,
        })
        .unwrap();
    store
        .replace_members(CHAT, 1, &[Member::new(LAB_A, None), Member::new(LAB_B, None)])
        .unwrap();
    let picture = picture_html();
    message(store, BOTH, &picture, &json!([entry("report.pdf", SHARE)]));
    message(store, RACE, &picture, &json!([entry("report.pdf", RACE_SHARE)]));
    let slow = json!([entry("slow.pdf", SLOW_SHARE), entry("after.pdf", SHARE)]);
    message(store, SLOW, "<p>slow</p>", &slow);

    message(
        store,
        ESCAPE,
        "<p>escape</p>",
        &json!([entry("../../escape.txt", SHARE)]),
    );
    let twins = json!([entry("report.pdf", SHARE), entry("report.pdf", MISSING)]);
    message(store, TWINS, "<p>twins</p>", &twins);
    let cases = json!([entry("report.pdf", SHARE), entry("Report.PDF", SHARE)]);
    message(store, CASES, "<p>cases</p>", &cases);
}

fn picture_html() -> String {
    format!(
        r#"<p>the report<img src="https://us-api.asm.skype.com/v1/objects/{OBJECT}/views/imgo" itemtype="http://schema.skype.com/AMSImage" itemid="{OBJECT}" alt="image"></p>"#
    )
}

/// The file's bytes, once another app put its file at `there`.
struct PutsAFileFirst {
    there: PathBuf,
}

impl Respond for PutsAFileFirst {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        std::fs::create_dir_all(self.there.parent().unwrap()).unwrap();
        std::fs::write(&self.there, ANOTHER_APPS).unwrap();
        file_bytes()
    }
}

/// The slow file's content, held until the test opens the gate.
#[derive(Default)]
struct Gate {
    reached: AtomicBool,
    open: AtomicBool,
}

struct Held(Arc<Gate>);

impl Respond for Held {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        self.0.reached.store(true, Ordering::SeqCst);
        let start = Instant::now();
        while !self.0.open.load(Ordering::SeqCst) && start.elapsed() < WAIT {
            std::thread::sleep(Duration::from_millis(5));
        }
        file_bytes()
    }
}

fn file_bytes() -> ResponseTemplate {
    let range = format!("bytes 0-{}/{}", FILE_BYTES.len() - 1, FILE_BYTES.len());
    ResponseTemplate::new(206)
        .insert_header("Content-Range", range)
        .set_body_bytes(FILE_BYTES)
}

/// The Graph item of `share`, its content at `download`.
async fn serve_item(server: &MockServer, share: &str, download: &str) {
    let item = json!({
        "id": "made-up-file", "size": FILE_BYTES.len(),
        "@microsoft.graph.downloadUrl": format!("https://contoso-my.sharepoint.com{download}?UniqueId=made-up&tempauth=made-up"),
    });
    Mock::given(method("GET"))
        .and(path(format!(
            "/v1.0/shares/{}/driveItem",
            FileDownload::of(share).unwrap().share()
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(item))
        .mount(server)
        .await;
}

/// The race message's file content puts a picture at `race_picture` first.
async fn serve_held(server: &MockServer, race_picture: PathBuf, gate: Arc<Gate>) {
    serve_item(server, RACE_SHARE, RACE_DOWNLOAD).await;
    Mock::given(method("GET"))
        .and(path(RACE_DOWNLOAD))
        .respond_with(PutsAFileFirst { there: race_picture })
        .mount(server)
        .await;
    serve_item(server, SLOW_SHARE, SLOW_DOWNLOAD).await;
    Mock::given(method("GET"))
        .and(path(SLOW_DOWNLOAD))
        .respond_with(Held(gate))
        .mount(server)
        .await;
}

async fn serve_files(server: &MockServer) {
    serve_item(server, SHARE, DOWNLOAD).await;
    Mock::given(method("GET"))
        .and(path(DOWNLOAD))
        .respond_with(file_bytes())
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/objects/{OBJECT}/views/imgo")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "image/png"))
        .mount(server)
        .await;
}

/// The app of lab account a, its engine's reads on wiremock, serving its control socket.
struct Lab {
    socket: PathBuf,
    cx: TestAppContext,
    dir: tempfile::TempDir,
    gate: Arc<Gate>,
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
        let dir = tempfile::tempdir().unwrap();
        runtime.block_on(serve_files(&server));
        let gate = Arc::new(Gate::default());
        let race_picture = dir.path().join("out/race/image-2.png");
        runtime.block_on(serve_held(&server, race_picture, Arc::clone(&gate)));
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
            socket,
            cx,
            dir,
            gate,
            _server: server,
            _runtime: runtime,
        }
    }

    fn ask(&self, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, None, request)
    }

    fn saved(&self, request: Request) -> Vec<SavedFile> {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Ok(Response::Downloaded { files }) => files,
            other => panic!("{asked}: {other:?}"),
        }
    }

    fn refused(&self, request: Request) -> CtlError {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("{asked}: {other:?}"),
        }
    }

    /// A new folder in the test's folder, not made yet.
    fn out(&self, name: &str) -> PathBuf {
        self.dir.path().join("out").join(name)
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing.
fn started(handle: engine::EngineHandle) -> Started<MemorySecretStore> {
    let mut account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
    account.name = Some("bot-a".to_owned());
    let session = Session::new(
        UPN,
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

/// What `download` selects: `--name`, `--index`, `--all`.
#[derive(Clone, Copy, Default)]
struct Pick<'a> {
    name: Option<&'a str>,
    index: Option<u32>,
    all: bool,
    force: bool,
}

const ALL: Pick<'static> = Pick {
    name: None,
    index: None,
    all: true,
    force: false,
};

fn index(index: u32) -> Pick<'static> {
    Pick {
        index: Some(index),
        ..Pick::default()
    }
}

fn named(name: &str) -> Pick<'_> {
    Pick {
        name: Some(name),
        ..Pick::default()
    }
}

fn download(message: &str, dir: &str, pick: Pick<'_>) -> Request {
    Request::Download {
        chat: "Downloads".to_owned(),
        message: message.to_owned(),
        name: pick.name.map(str::to_owned),
        index: pick.index,
        all: pick.all,
        dir: dir.to_owned(),
        force: pick.force,
    }
}

fn into(message: &str, dir: &Path, pick: Pick<'_>) -> Request {
    download(message, dir.to_str().unwrap(), pick)
}

fn saved_file(index: u32, name: &str, path: &Path, size: usize) -> SavedFile {
    SavedFile {
        index,
        name: name.to_owned(),
        path: path.to_str().unwrap().to_owned(),
        size: size as u64,
    }
}

#[test]
fn messages_list_a_message_s_file_and_its_picture() {
    let lab = Lab::start(false);
    let Ok(Response::Messages { messages }) = lab.ask(Request::Messages {
        chat: CHAT.to_owned(),
        limit: None,
        before: None,
    }) else {
        panic!("the messages");
    };
    let both = messages.iter().find(|message| message.id == BOTH).unwrap();
    assert_eq!(
        both.attachments,
        vec![
            AttachmentRow {
                index: 1,
                name: "report.pdf".to_owned(),
                kind: AttachmentKind::File,
                file_type: Some("pdf".to_owned()),
            },
            AttachmentRow {
                index: 2,
                name: "image-2".to_owned(),
                kind: AttachmentKind::Picture,
                file_type: None,
            },
        ]
    );
}

#[test]
fn download_saves_the_file_and_the_picture_through_the_engine_and_replaces_a_file_only_with_force() {
    let lab = Lab::start(false);
    let out = lab.out("all");
    let files = lab.saved(into(BOTH, &out, ALL));
    assert_eq!(
        files,
        vec![
            saved_file(1, "report.pdf", &out.join("report.pdf"), FILE_BYTES.len()),
            saved_file(2, "image-2", &out.join("image-2.png"), PNG.len()),
        ]
    );
    assert_eq!(std::fs::read(out.join("report.pdf")).unwrap(), FILE_BYTES);
    assert_eq!(std::fs::read(out.join("image-2.png")).unwrap(), PNG);
    let mut names: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["image-2.png", "report.pdf"], "no part file is left");

    std::fs::write(out.join("report.pdf"), b"mine").unwrap();
    let refused = lab.refused(into(BOTH, &out, named("report.pdf")));
    assert_eq!(refused.code, ErrorCode::FileExists, "{refused:?}");
    assert_eq!(std::fs::read(out.join("report.pdf")).unwrap(), b"mine");

    let forced = into(
        BOTH,
        &out,
        Pick {
            force: true,
            ..index(1)
        },
    );
    assert_eq!(lab.saved(forced).len(), 1);
    assert_eq!(std::fs::read(out.join("report.pdf")).unwrap(), FILE_BYTES);

    std::fs::remove_file(out.join("image-2.png")).unwrap();
    std::fs::write(out.join("image-2.png.part"), b"another app's").unwrap();
    let refused = lab.refused(into(BOTH, &out, index(2)));
    assert_eq!(refused.code, ErrorCode::FileExists, "{refused:?}");
    assert_eq!(std::fs::read(out.join("image-2.png.part")).unwrap(), b"another app's");
    assert!(!out.join("image-2.png").exists());
}

#[test]
fn a_file_name_never_leaves_the_folder_and_a_selection_must_name_one_attachment() {
    let lab = Lab::start(false);
    let out = lab.out("escape");
    let files = lab.saved(into(ESCAPE, &out, Pick::default()));
    assert_eq!(files.len(), 1, "{files:?}");
    let path = PathBuf::from(&files[0].path);
    assert_eq!(path.parent(), Some(out.as_path()), "{path:?}");
    assert_eq!(std::fs::read(&path).unwrap(), FILE_BYTES);

    for request in [
        // two attachments, none selected
        into(BOTH, &out, Pick::default()),
        into(BOTH, &out, named("../report.pdf")),
        into(BOTH, &out, index(3)),
        into(BOTH, &out, index(0)),
    ] {
        assert_eq!(lab.refused(request).code, ErrorCode::NoAttachment);
    }
    assert_eq!(
        lab.refused(download(BOTH, "relative/out", ALL)).code,
        ErrorCode::BadRequest
    );
    assert_eq!(lab.refused(into("1", &out, ALL)).code, ErrorCode::BadRequest);
}

#[test]
fn the_read_only_mode_downloads_too() {
    let lab = Lab::start(true);
    let out = lab.out("read-only");
    assert_eq!(
        lab.saved(into(BOTH, &out, index(2))),
        vec![saved_file(2, "image-2", &out.join("image-2.png"), PNG.len())]
    );
}

#[test]
fn a_name_two_files_have_selects_none_and_a_failure_names_what_was_saved_before_it() {
    let lab = Lab::start(false);
    let out = lab.out("twins");
    assert_eq!(
        lab.refused(into(TWINS, &out, named("report.pdf"))).code,
        ErrorCode::NoAttachment
    );
    let failed = lab.refused(into(TWINS, &out, ALL));
    assert_eq!(failed.code, ErrorCode::Engine, "{failed:?}");
    let first = out.join("report.pdf");
    assert!(
        failed.message.contains("report.pdf couldn't be downloaded"),
        "{failed:?}"
    );
    assert!(
        failed
            .message
            .contains(&format!("saved before it: {}", first.display())),
        "{failed:?}"
    );
    assert_eq!(std::fs::read(&first).unwrap(), FILE_BYTES);
    assert!(!out.join("report (2).pdf").exists());
    assert!(!out.join("report (2).pdf.part").exists());
}

#[test]
fn two_files_whose_names_differ_only_in_case_are_both_saved_under_two_names() {
    let lab = Lab::start(false);
    let out = lab.out("cases");
    let files = lab.saved(into(CASES, &out, ALL));
    assert_eq!(
        files,
        vec![
            saved_file(1, "report.pdf", &out.join("report.pdf"), FILE_BYTES.len()),
            saved_file(2, "Report.PDF", &out.join("Report (2).PDF"), FILE_BYTES.len()),
        ]
    );
    assert_eq!(std::fs::read(out.join("report.pdf")).unwrap(), FILE_BYTES);
    assert_eq!(std::fs::read(out.join("Report (2).PDF")).unwrap(), FILE_BYTES);
}

#[test]
fn a_file_that_comes_in_at_a_later_target_during_the_download_stops_it_without_force() {
    let lab = Lab::start(false);
    let out = lab.out("race");
    let refused = lab.refused(into(RACE, &out, ALL));
    assert_eq!(refused.code, ErrorCode::FileExists, "{refused:?}");
    assert!(
        refused
            .message
            .contains(&format!("saved before it: {}", out.join("report.pdf").display())),
        "{refused:?}"
    );
    assert_eq!(std::fs::read(out.join("image-2.png")).unwrap(), ANOTHER_APPS);
    assert_eq!(std::fs::read(out.join("report.pdf")).unwrap(), FILE_BYTES);
}

#[test]
fn past_the_answer_bound_the_app_answers_timeout_and_saves_the_files_after_the_one_it_reads() {
    let lab = Lab::start(false);
    let out = lab.out("slow");
    let socket = lab.socket.clone();
    let request = into(SLOW, &out, ALL);
    let asking = std::thread::spawn(move || connect(&socket).unwrap().request(None, request));
    let start = Instant::now();
    while !lab.gate.reached.load(Ordering::SeqCst) {
        lab.cx.run_until_parked();
        assert!(start.elapsed() < WAIT, "the slow file wasn't asked for");
        std::thread::sleep(Duration::from_millis(5));
    }
    lab.cx.executor().advance_clock(DOWNLOAD_ANSWER);
    while !asking.is_finished() {
        lab.cx.run_until_parked();
        assert!(start.elapsed() < WAIT, "the client wasn't answered");
        std::thread::sleep(Duration::from_millis(2));
    }
    let late = asking.join().unwrap();
    assert!(
        matches!(&late, Err(ClientError::Server(error)) if error.code == ErrorCode::Timeout),
        "{late:?}"
    );
    lab.gate.open.store(true, Ordering::SeqCst);
    while !out.join("after.pdf").exists() {
        lab.cx.run_until_parked();
        assert!(start.elapsed() < WAIT, "the file after the slow one wasn't saved");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read(out.join("slow.pdf")).unwrap(), FILE_BYTES);
    assert_eq!(std::fs::read(out.join("after.pdf")).unwrap(), FILE_BYTES);
}
