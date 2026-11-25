//! A second launch, or a Teams link, handed to the app already running (`ngteams::handoff`), and `open-link` in its
//! server (`ctl::links`): through the real protocol and server, over lab account a's engine on the mock (`teams-mock`,
//! no push channel and no calling), every socket in a temp dir. The app shows calls, but a meeting link only opens its
//! pre-join: nothing is joined or sent. Every link and id is made up or the mock's own.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::io::BufReader;
#[expect(
    dead_code,
    reason = "this fixture serves control requests instead of connecting a raw client"
)]
#[path = "support/transport.rs"]
mod transport;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use transport::Listener;

use clap::Parser as _;
use ctl_support::{ask, pump_until, pumped, quit};
use gpui::{Entity, TestAppContext};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::cli::Cli;
use ngteams::ctl::links::Link;
use ngteams::handoff::{self, HandOff};
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{CallFloor, Outbox};
use teams_ctl::protocol::{
    ErrorCode, Hello, Outcome as Answered, PROTOCOL_VERSION, Request, Response, ResponseLine, WindowState,
};
use teams_ctl::{ClientError, LineReader, RequestLine, Timeouts, codec};
use teams_mock::{CHAT_BASE, ChatServiceMock, ECHO, LAB_A, MIDDLE_TIER_BASE};
use teams_proto::{Author, Client, Credentials, HttpTransport, MeetingInfo, Mri};
use teams_store::Store;
use teams_ui::bus::BusError;
use teams_ui::calls::Prejoin;
use teams_ui::{Calls, SettingsFile, ViewTrace};

const UPN: &str = "bot-a@lab.example.test";
/// The made-up meeting's chat, its organizer and tenant.
const THREAD: &str = "19:meeting_SyntheticHandoff@thread.v2";
const ORGANIZER: &str = "00000000-0000-4000-8000-00000000000b";
const TENANT: &str = "00000000-0000-4000-8000-000000000001";
/// Its join link's path and `context` (percent-encoded `{"Tid":…,"Oid":…}`).
const PATH: &str = "/l/meetup-join/19%3ameeting_SyntheticHandoff%40thread.v2/0";
const CONTEXT: &str = "?context=%7b%22Tid%22%3a%2200000000-0000-4000-8000-000000000001%22%2c%22Oid%22%3a%2200000000-0000-4000-8000-00000000000b%22%7d";

fn msteams_link() -> String {
    format!("msteams:{PATH}{CONTEXT}")
}

fn https_link() -> String {
    format!("https://teams.microsoft.com{PATH}{CONTEXT}")
}

fn cli(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("ngteams").chain(args.iter().copied())).unwrap()
}

/// Where an app under `dir` serves its control socket.
fn socket_of(dir: &Path) -> PathBuf {
    dir.join("run/ngteams/ctl.sock")
}

/// a's token, as the mock hands it out.
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
struct Running {
    socket: PathBuf,
    cx: TestAppContext,
    _mock: ChatServiceMock,
    _dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl Running {
    /// a's app on the mock under a temp dir, showing calls when `calls`.
    fn start(calls: bool) -> Self {
        Self::start_in(tempfile::tempdir().unwrap(), calls)
    }

    /// a's app on the mock under `dir`, serving at [`socket_of`] it.
    fn start_in(dir: tempfile::TempDir, calls: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let handle = {
            let _entered = runtime.enter();
            a_engine(&mock, &dir)
        };
        let socket = socket_of(dir.path());
        let cx = TestAppContext::single();
        let calls = calls.then(|| PersonalCalls {
            ringer: None,
            sounds: None,
            echo: Mri::new(ECHO),
            video: None,
            camera: None,
            devices: None,
            noise: teams_media::processing::NoiseControl::default(),
        });
        let launch = Launch {
            first: Started {
                calls,
                ..started(handle)
            },
            others: Vec::new(),
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
        let Ok(_launched) = cx.update(|cx| app::launch(cx, launch)) else {
            panic!("the window opens");
        };
        cx.run_until_parked();
        Self {
            socket,
            cx,
            _mock: mock,
            _dir: dir,
            _runtime: runtime,
        }
    }

    /// `request` handed off to the app from a thread of its own, the test thread stepping GPUI.
    fn handed(&self, request: Request) -> HandOff {
        let socket = self.socket.clone();
        pumped(&self.cx, move || handoff::hand_off(&socket, request, handoff::TIMEOUTS))
    }

    fn ask(&self, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, None, request)
    }

    /// The meeting pre-join showing, if one does.
    fn prejoin(&self) -> Option<Prejoin> {
        self.cx.read(|cx| calls(cx).read(cx).prejoin().cloned())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// a's calls.
fn calls(cx: &gpui::App) -> Entity<Calls> {
    app::bridges(cx)[0].read(cx).models().calls.clone()
}

/// a's engine on the mock without a push channel or calling.
fn a_engine(mock: &ChatServiceMock, dir: &tempfile::TempDir) -> EngineHandle {
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .with_middle_tier(MIDDLE_TIER_BASE)
            .unwrap()
    };
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing (an empty
/// in-memory store, a loopback port nothing listens on).
fn started(handle: EngineHandle) -> Started<MemorySecretStore> {
    let account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), teams_mock::TENANT).unwrap();
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

#[test]
fn a_plain_launch_hands_off_activate_and_a_link_open_link() {
    assert_eq!(cli(&[]).hand_off(), Some(Request::Activate));
    let link = msteams_link();
    assert_eq!(
        cli(&[link.as_str()]).hand_off(),
        Some(Request::OpenLink { url: link.clone() })
    );
    assert_eq!(
        cli(&["--ctl-socket", "/run/elsewhere.sock"]).hand_off(),
        Some(Request::Activate)
    );
}

#[test]
fn forget_account_and_the_subcommands_are_not_handed_off() {
    assert_eq!(cli(&["--forget", "someone@example.test"]).hand_off(), None);
    assert_eq!(cli(&["--account", "someone@example.test"]).hand_off(), None);
    assert_eq!(cli(&["--lab", "a", "--secrets-fd", "3"]).hand_off(), None);
    // a subcommand's name is the subcommand, not a link
    let snapshot = cli(&["snapshot", "--to", "/nowhere"]);
    assert!(snapshot.command.is_some() && snapshot.url.is_none());
    assert_eq!(snapshot.hand_off(), None);
    assert!(Cli::try_parse_from(["ngteams", "--forget", "someone@example.test", "msteams:/l/x"]).is_err());
}

#[test]
fn the_lab_and_the_read_only_mode_take_no_link() {
    let link = msteams_link();
    let lab = cli(&["--lab", "a", "--secrets-fd", "3", link.as_str()]);
    assert!(lab.lab_mode().unwrap_err().contains("link"));
    let read_only = cli(&[
        "--read-only",
        "--account",
        "someone@example.test",
        "--data",
        "/nowhere",
        "--secrets-fd",
        "3",
        link.as_str(),
    ]);
    assert!(read_only.read_only_mode(None).unwrap_err().contains("link"));
}

#[test]
fn a_second_launch_hands_off_to_the_running_app_and_ends() {
    let running = Running::start(false);
    let request = cli(&["--ctl-socket", running.socket.to_str().unwrap()])
        .hand_off()
        .unwrap();
    assert_eq!(running.handed(request), HandOff::Done { note: None });
    let Ok(Response::Status(status)) = running.ask(Request::Status) else {
        panic!("status");
    };
    assert_eq!(status.window, WindowState::Shown);
}

#[test]
fn a_hung_app_times_out_and_the_second_launch_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ctl.sock");
    // takes connections (the kernel's backlog) and never answers
    let _hung = Listener::bind(&socket).unwrap();
    let timeouts = Timeouts {
        connect: Duration::from_secs(1),
        read: Duration::from_millis(200),
        write: Duration::from_secs(1),
    };
    let start = Instant::now();
    let HandOff::Failed(why) = handoff::hand_off(&socket, Request::Activate, timeouts) else {
        panic!("a hung app isn't handed off to");
    };
    assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
    assert!(why.contains("didn't answer in time"), "{why}");
    assert!(why.contains(&socket.display().to_string()), "{why}");
}

#[test]
fn a_link_is_waited_for_past_the_short_answer_while_its_meeting_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ctl.sock");
    let listener = Listener::bind(&socket).unwrap();
    // an app that greets at once and answers the link only after the short bound, as one reading a slow meeting
    let app = std::thread::spawn(move || {
        let stream = listener.accept().unwrap();
        let mut reader = LineReader::new(BufReader::new(stream.try_clone().unwrap()));
        let mut writer = stream;
        let mut answer = |id: u64, response| {
            let line = ResponseLine {
                id,
                outcome: Answered::Ok(response),
            };
            codec::write_line(&mut writer, &line).unwrap();
        };
        let hello: RequestLine = reader.read().unwrap().unwrap();
        answer(
            hello.id,
            Response::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                app: "synthetic".to_owned(),
            }),
        );
        let asked: RequestLine = reader.read().unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(800));
        answer(asked.id, Response::Done);
        asked.request
    });
    let timeouts = Timeouts {
        connect: Duration::from_secs(1),
        read: Duration::from_millis(200),
        write: Duration::from_secs(1),
    };
    let request = Request::OpenLink { url: msteams_link() };
    assert_eq!(
        handoff::hand_off(&socket, request.clone(), timeouts),
        HandOff::Done { note: None }
    );
    assert_eq!(app.join().unwrap(), request);
}

#[test]
fn nobody_behind_the_socket_is_a_normal_start_that_replaces_a_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_of(dir.path());
    assert_eq!(
        handoff::hand_off(&socket, Request::Activate, handoff::TIMEOUTS),
        HandOff::NotRunning
    );
    teams_ctl::prepare_dir(&socket).unwrap();
    drop(Listener::bind(&socket).unwrap());
    assert_eq!(socket.exists(), cfg!(unix), "only Unix sockets leave a stale file");
    assert_eq!(
        handoff::hand_off(&socket, Request::Activate, handoff::TIMEOUTS),
        HandOff::NotRunning
    );
    // the app started then binds over it
    let running = Running::start_in(dir, false);
    assert_eq!(running.handed(Request::Activate), HandOff::Done { note: None });
}

#[test]
fn an_error_answer_fails_the_hand_off_with_its_message() {
    // an app without calls joins no meeting
    let running = Running::start(false);
    let HandOff::Failed(why) = running.handed(Request::OpenLink { url: msteams_link() }) else {
        panic!("a link the app can't open isn't done");
    };
    assert!(
        why.starts_with("the running Teams: ") && why.contains("no calls"),
        "{why}"
    );
}

#[test]
fn a_meetup_join_link_in_either_form_opens_its_pre_join() {
    for link in [msteams_link(), https_link()] {
        let running = Running::start(true);
        let request = Request::OpenLink { url: link.clone() };
        assert_eq!(running.handed(request), HandOff::Done { note: None }, "{link}");
        let prejoin = running.prejoin().unwrap_or_else(|| panic!("{link}: no pre-join"));
        assert_eq!(prejoin.to_join.chat(), Some(THREAD));
        assert_eq!(prejoin.to_join.meeting().unwrap().organizer_id, ORGANIZER);
        assert_eq!(prejoin.to_join.meeting().unwrap().tenant_id, TENANT);
        // the mock has no such thread: the link's organizer, under the pre-join's own title
        assert_eq!(prejoin.to_join.title(), "Meeting");
        assert!(!prejoin.joining, "nothing is joined");
    }
}

#[test]
fn a_link_naming_no_organizer_whose_chat_names_no_meeting_opens_nothing() {
    let running = Running::start(true);
    let answered = running.ask(Request::OpenLink {
        url: format!("msteams:{PATH}"),
    });
    let Err(ClientError::Server(error)) = answered else {
        panic!("{answered:?}");
    };
    assert!(
        matches!(error.code, ErrorCode::Engine | ErrorCode::NoMeeting),
        "{error:?}"
    );
    assert!(running.prejoin().is_none());
}

#[test]
fn an_unknown_link_shows_the_window_and_says_so() {
    let running = Running::start(true);
    for url in [
        "https://example.test/l/meetup-join/19%3ameeting_SyntheticHandoff%40thread.v2/0",
        "msteams:/l/chat/0/0?users=someone@example.test",
        "not a link",
    ] {
        let HandOff::Done { note: Some(note) } = running.handed(Request::OpenLink { url: url.to_owned() }) else {
            panic!("{url}: not shown");
        };
        assert!(note.contains("window is shown"), "{note}");
    }
    assert!(running.prejoin().is_none());
}

#[test]
fn the_link_the_app_started_with_opens_once_it_runs() {
    let running = Running::start(true);
    running.cx.update(|cx| app::open_link(https_link(), cx));
    pump_until(&running.cx, Duration::from_millis(10), |cx| {
        calls(cx).read(cx).prejoin().is_some()
    });
    assert_eq!(running.prejoin().unwrap().to_join.chat(), Some(THREAD));
}

#[test]
fn links_are_read_by_their_shape() {
    let meeting = Some(MeetingInfo {
        organizer_id: ORGANIZER.to_owned(),
        tenant_id: TENANT.to_owned(),
    });
    let of = |chat: &str, meeting: Option<MeetingInfo>| Link::Meeting {
        chat: chat.to_owned(),
        meeting,
    };
    assert_eq!(Link::of(&msteams_link()), of(THREAD, meeting.clone()));
    assert_eq!(Link::of(&https_link()), of(THREAD, meeting.clone()));
    assert_eq!(Link::of(&format!(" MSTEAMS:{PATH}{CONTEXT}\n")), of(THREAD, meeting));
    assert_eq!(Link::of(&format!("msteams:{PATH}")), of(THREAD, None));
    for other in [
        format!("https://elsewhere.example.test{PATH}{CONTEXT}"),
        format!("http://teams.microsoft.com{PATH}{CONTEXT}"),
        "msteams:/l/meetup-join/not-a-thread/0".to_owned(),
        "https://teams.microsoft.com/l/chat/0/0".to_owned(),
        "msteams".to_owned(),
        String::new(),
    ] {
        assert_eq!(Link::of(&other), Link::Other, "{other}");
    }
}

#[test]
fn the_desktop_entry_hands_links_over_and_the_installer_keeps_that() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let entry = std::fs::read_to_string(root.join("packaging/com.nickalie.ngteams.desktop")).unwrap();
    let lines: Vec<&str> = entry.lines().collect();
    assert!(lines.contains(&"Exec=ngteams %u"), "{entry}");
    assert!(lines.contains(&"MimeType=x-scheme-handler/msteams;"), "{entry}");
    let installer = std::fs::read_to_string(root.join("scripts/install.sh")).unwrap();
    assert!(
        installer.contains(r#"sed "s|^Exec=ngteams %u$|Exec=$lib/ngteams %u|""#),
        "the installer's Exec rewrite matches the entry's line"
    );
}
