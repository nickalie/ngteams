//! The control socket's `download` and the attachments `messages` lists (`ctl::download`), through the real protocol
//! and server, over lab account a's engine on wiremock as Graph, SharePoint and AMS (the transport's `test-support`
//! mock origin): a message with a SharePoint file and a picture in its text, and one whose file name tries to leave
//! the folder, one with two files of one name, the second not there, and one with two files whose names differ only in
//! case. The files are read by the app's engine, as the chat view reads them, and saved where the client asks. The
//! read-only mode downloads too. Every id, link, name and byte is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use ctl_support::{ask, quit};
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
use teams_ctl::protocol::{AttachmentKind, AttachmentRow, CtlError, ErrorCode, Request, Response, SavedFile};
use teams_mock::{CHAT_BASE, LAB_A, LAB_B, TENANT};
use teams_proto::{Author, Client, Credentials, FileDownload, HttpTransport, Mri};
use teams_store::{Conversation, ConversationKind, Member, Message, Store};
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    let picture = format!(
        r#"<p>the report<img src="https://us-api.asm.skype.com/v1/objects/{OBJECT}/views/imgo" itemtype="http://schema.skype.com/AMSImage" itemid="{OBJECT}" alt="image"></p>"#
    );
    message(store, BOTH, &picture, &json!([entry("report.pdf", SHARE)]));
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

async fn serve_files(server: &MockServer) {
    let item = json!({
        "id": "made-up-file", "size": FILE_BYTES.len(),
        "@microsoft.graph.downloadUrl": format!("https://contoso-my.sharepoint.com{DOWNLOAD}?UniqueId=made-up&tempauth=made-up"),
    });
    let item_path = format!("/v1.0/shares/{}/driveItem", FileDownload::of(SHARE).unwrap().share());
    let range = format!("bytes 0-{}/{}", FILE_BYTES.len() - 1, FILE_BYTES.len());
    for (at, response) in [
        (item_path, ResponseTemplate::new(200).set_body_json(item)),
        (
            DOWNLOAD.to_owned(),
            ResponseTemplate::new(206)
                .insert_header("Content-Range", range)
                .set_body_bytes(FILE_BYTES),
        ),
        (
            format!("/v1/objects/{OBJECT}/views/imgo"),
            ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "image/png"),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(response)
            .mount(server)
            .await;
    }
}

/// The app of lab account a, its engine's reads on wiremock, serving its control socket.
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
        runtime.block_on(serve_files(&server));
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
