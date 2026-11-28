//! QA of task 1037: a person reads one event of the calendar with `ngteamsctl calendar show`, against the running
//! app. Each `ngteamsctl` run is a process of its own: this test binary started again, which runs `teams_ctl::cli` as
//! the `ngteamsctl` binary does (its arguments, its environment, its stdout, its stderr and its exit code). The app
//! runs in this process, lab account a's engine on the mock's middle tier (`teams-mock`, loopback only).
//!
//! - **show**: the own series occurrence "Code review" (text, organizer, attendees and their answers, join link,
//!   location, recurrence), its series master, the attendee's copy of a series occurrence "Weekly sync" (no answers
//!   of others), an event with no text, no attendees and no join link, and an occurrence whose series cannot be read
//!   ("Part of a series"). As text and as `--json`.
//! - **calendar**: the list rows have no field of `calendar show`.
//! - **errors**: an unknown event id.
//!
//! Every id, name, address, link and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]
#![cfg(target_os = "linux")]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use clap::Parser as _;
use ctl_support::{pump_until_done, quit};
use gpui::TestAppContext;
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_core::{CallFloor, Outbox};
use teams_ctl::cli::{self, Cli};
use teams_ctl::path::Env;
use teams_ctl::protocol::local_today;
use teams_mock::{CHAT_BASE, ChatServiceMock, LAB_A, MIDDLE_TIER_BASE, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};

/// The JSON array of arguments that makes this binary run as `ngteamsctl`.
const CHILD_ARGS: &str = "CTL_CALENDAR_CLI_ARGS";
/// The files the child writes its stdout and its stderr in.
const CHILD_OUT: &str = "CTL_CALENDAR_CLI_OUT";
const CHILD_ERR: &str = "CTL_CALENDAR_CLI_ERR";
/// The name of the test that is the child.
const CHILD_TEST: &str = "ngteamsctl_child";

const UPN: &str = "bot-a@lab.example.test";
const REVIEW_SERIES: &str = "AAMkMadeUpSeriesreview";
const BARE: &str = "AAMkMadeUpBareEvent";
/// The fields of `calendar show` that a list row does not have.
const DETAILS: [&str; 7] = [
    "organizer_address",
    "response",
    "attendees",
    "join_url",
    "series",
    "recurrence",
    "body",
];

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

impl Run {
    fn json(&self) -> Value {
        assert_eq!(self.code, 0, "{self:?}");
        let line: Value = serde_json::from_str(&self.stdout).unwrap_or_else(|error| panic!("{error}: {self:?}"));
        line["ok"].clone()
    }

    fn text(&self) -> &str {
        assert_eq!(self.code, 0, "{self:?}");
        &self.stdout
    }
}

/// `ngteamsctl <args>` against `socket`, while the test thread steps the app.
fn ngteamsctl(cx: &TestAppContext, socket: &Path, args: &[&str]) -> Run {
    let logs = tempfile::tempdir().unwrap();
    let (out, err) = (logs.path().join("out"), logs.path().join("err"));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD_TEST, "--ignored", "--test-threads=1", "--quiet"])
        .current_dir(logs.path())
        .env_clear()
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
    assert!(
        output.status.code() != Some(101),
        "ngteamsctl {args:?} panicked: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let run = Run {
        code: output.status.code().unwrap_or(-1),
        stdout: std::fs::read_to_string(&out).unwrap_or_default(),
        stderr: std::fs::read_to_string(&err).unwrap_or_default(),
    };
    println!(
        "$ ngteamsctl {}\nexit {}\n{}{}",
        args.join(" "),
        run.code,
        run.stdout,
        run.stderr
    );
    run
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

/// a's engine on the mock without a push channel or calling, its calendar read from the mock's middle tier.
fn a_engine(mock: &ChatServiceMock, store: &Path) -> EngineHandle {
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
        store: Store::open(store).unwrap(),
        client: Arc::new(Client::new(http())),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
        push: None,
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })
    .unwrap()
}

fn started(handle: EngineHandle) -> Started<MemorySecretStore> {
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

/// a's app on the mock, serving its control socket.
struct Lab {
    mock: ChatServiceMock,
    socket: PathBuf,
    cx: TestAppContext,
    runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

impl Lab {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let handle = {
            let _entered = runtime.enter();
            a_engine(&mock, &dir.path().join("a.sqlite"))
        };
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let cx = TestAppContext::single();
        let launch = Launch {
            first: started(handle),
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
        assert!(cx.update(|cx| app::launch(cx, launch)).is_ok(), "the window opens");
        cx.run_until_parked();
        Self {
            mock,
            socket,
            cx,
            runtime,
            _dir: dir,
        }
    }

    fn ctl(&self, args: &[&str]) -> Run {
        ngteamsctl(&self.cx, &self.socket, args)
    }

    /// The `--json` rows of `calendar` for this week (seven days from today).
    fn week(&self) -> Vec<Value> {
        let today = local_today();
        let last = (0..6).fold(today, |day, _| day.succ_opt().unwrap());
        let (from, to) = (
            today.format("%Y-%m-%d").to_string(),
            last.format("%Y-%m-%d").to_string(),
        );
        let answer = self.ctl(&["--json", "calendar", "--from", &from, "--to", &to]).json();
        answer["events"]
            .as_array()
            .unwrap_or_else(|| panic!("no events: {answer}"))
            .clone()
    }

    /// The id of the first event of this week called `subject`.
    fn id_of(&self, subject: &str) -> String {
        let week = self.week();
        week.iter()
            .find(|event| event["subject"] == subject)
            .and_then(|event| event["id"].as_str())
            .unwrap_or_else(|| panic!("no {subject} this week: {week:?}"))
            .to_owned()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// The lines of a `calendar show` text answer that start with `key:`, their values trimmed.
fn values<'a>(text: &'a str, key: &str) -> Vec<&'a str> {
    let key = format!("{key}:");
    text.lines()
        .filter_map(|line| line.strip_prefix(key.as_str()))
        .map(str::trim)
        .collect()
}

#[test]
fn a_person_reads_their_own_series_occurrence_as_text_and_as_json() {
    let lab = Lab::start();
    let id = lab.id_of("Code review");

    let event = &lab.ctl(&["--json", "calendar", "show", &id]).json();
    for (key, value) in [
        ("kind", "calendar_event"),
        ("id", id.as_str()),
        ("subject", "Code review"),
        ("organizer", "Lab Account A"),
        ("organizer_address", "someone@example.com"),
        ("location", "Microsoft Teams Meeting"),
        ("response", "organizer"),
        ("series", REVIEW_SERIES),
        ("body", "Made-up notes"),
    ] {
        assert_eq!(event[key], value, "{key}: {event}");
    }
    let recurrence = event["recurrence"].as_str().unwrap_or_default();
    assert!(recurrence.starts_with("Occurs every Wednesday starting "), "{event}");
    let join = event["join_url"].as_str().unwrap_or_default();
    assert!(
        join.starts_with("https://teams.microsoft.com/l/meetup-join/19%3ameeting_MadeUpMeeting05"),
        "{event}"
    );
    assert_eq!(
        event["attendees"],
        json!([
            {"name": "Avery Example", "address": "avery@example.com", "optional": false, "response": "accepted"},
            {"name": "Lab Account A", "address": "a@example.com", "optional": false, "response": "accepted"},
            {"name": "Riley Placeholder", "address": "riley@example.com", "optional": false, "response": "tentative"},
            {"name": "Casey Madeup", "address": "casey@example.com", "optional": true, "response": "not_responded"},
        ])
    );

    let run = lab.ctl(&["calendar", "show", &id]);
    let text = run.text();
    for (key, value) in [
        ("id", id.as_str()),
        ("organizer", "Lab Account A <someone@example.com>"),
        ("location", "Microsoft Teams Meeting"),
        ("response", "organizer"),
        ("recurrence", recurrence),
        ("series", REVIEW_SERIES),
        ("join", join),
    ] {
        assert_eq!(values(text, key), [value], "{key}: {text}");
    }
    assert_eq!(
        values(text, "attendee"),
        [
            "Avery Example <avery@example.com> (accepted)",
            "Lab Account A <a@example.com> (accepted)",
            "Riley Placeholder <riley@example.com> (tentative)",
            "Casey Madeup <casey@example.com> (optional, not responded)",
        ],
        "{text}"
    );
    assert!(text.ends_with("(optional, not responded)\n\nMade-up notes\n"), "{text}");
}

#[test]
fn a_person_reads_the_series_master_with_its_own_recurrence() {
    let lab = Lab::start();
    let event = lab.ctl(&["--json", "calendar", "show", REVIEW_SERIES]).json();
    assert_eq!(event["id"], REVIEW_SERIES, "{event}");
    assert_eq!(event["subject"], "Code review");
    assert_eq!(event["body"], "Made-up notes of the series");
    assert_eq!(event["response"], "organizer");
    assert!(
        event.get("series").is_none(),
        "a master is no part of a series: {event}"
    );
    let recurrence = event["recurrence"].as_str().unwrap_or_default();
    assert!(recurrence.starts_with("Occurs every Wednesday starting "), "{event}");

    let run = lab.ctl(&["calendar", "show", REVIEW_SERIES]);
    let text = run.text();
    assert_eq!(values(text, "recurrence"), [recurrence], "{text}");
    assert!(values(text, "series").is_empty(), "{text}");
    assert!(text.ends_with("\n\nMade-up notes of the series\n"), "{text}");
}

#[test]
fn a_person_reads_an_attendee_s_copy_without_the_answers_of_others() {
    let lab = Lab::start();
    let id = lab.id_of("Weekly sync");

    let event = lab.ctl(&["--json", "calendar", "show", &id]).json();
    assert_eq!(event["response"], "tentative", "{event}");
    assert_eq!(event["organizer"], "Avery Example");
    assert_eq!(event["series"], "AAMkMadeUpSeriessync");
    assert!(
        event["recurrence"]
            .as_str()
            .is_some_and(|recurrence| recurrence.starts_with("Occurs every ")),
        "{event}"
    );
    let attendees = event["attendees"].as_array().unwrap_or_else(|| panic!("{event}"));
    assert_eq!(attendees.len(), 4, "{event}");
    assert!(
        attendees.iter().all(|attendee| attendee.get("response").is_none()),
        "{event}"
    );

    let run = lab.ctl(&["calendar", "show", &id]);
    let text = run.text();
    assert_eq!(values(text, "response"), ["tentative"], "{text}");
    assert_eq!(
        values(text, "attendee"),
        [
            "Avery Example <avery@example.com>",
            "Lab Account A <a@example.com>",
            "Riley Placeholder <riley@example.com>",
            "Casey Madeup <casey@example.com> (optional)",
        ],
        "{text}"
    );
}

#[test]
fn a_person_reads_an_event_with_no_text_no_attendees_and_no_join_link() {
    let lab = Lab::start();
    let bare = json!({
        "objectId": BARE,
        "subject": "Quiet time",
        "startTime": "2026-11-02T10:00:00.0000000Z",
        "endTime": "2026-11-02T11:00:00.0000000Z",
        "isAllDayEvent": false,
        "eventType": "Single",
        "isCancelled": false,
        "isOnlineMeeting": false,
        "isOrganizer": true,
        "myResponseType": "Organizer",
        "organizerName": "Lab Account A",
        "organizerAddress": "a@example.com",
        "location": "",
        "attendees": [{"name": "Lab Account A", "address": "a@example.com", "status": {"response": "Organizer"}}],
    });
    let path = format!("/events/{BARE}");
    lab.runtime.block_on(lab.mock.answer_next("GET", &path, 200, &bare));
    let event = lab.ctl(&["--json", "calendar", "show", BARE]).json();
    assert_eq!(event["subject"], "Quiet time", "{event}");
    assert_eq!(event["response"], "organizer", "{event}");
    for absent in ["attendees", "join_url", "series", "recurrence", "body", "location"] {
        assert!(event.get(absent).is_none(), "{absent}: {event}");
    }

    lab.runtime.block_on(lab.mock.answer_next("GET", &path, 200, &bare));
    let run = lab.ctl(&["calendar", "show", BARE]);
    let text = run.text();
    for absent in ["attendee", "join", "series", "recurrence"] {
        assert!(values(text, absent).is_empty(), "{absent}: {text}");
    }
    assert_eq!(values(text, "location"), ["-"], "{text}");
    assert!(!text.contains("\n\n"), "no text below the fields: {text}");
    assert_eq!(values(text, "response"), ["organizer"], "{text}");
    assert!(text.ends_with("organizer\n"), "{text}");
}

#[test]
fn an_occurrence_whose_series_cannot_be_read_is_part_of_a_series() {
    let lab = Lab::start();
    let id = lab.id_of("Code review");
    let path = format!("/events/{REVIEW_SERIES}");

    lab.runtime.block_on(lab.mock.refuse_next("GET", &path, 404));
    let event = lab.ctl(&["--json", "calendar", "show", &id]).json();
    assert_eq!(event["recurrence"], "Part of a series", "{event}");
    assert_eq!(event["series"], REVIEW_SERIES);
    assert_eq!(event["body"], "Made-up notes");
    assert_eq!(event["attendees"].as_array().map(Vec::len), Some(4), "{event}");

    lab.runtime.block_on(lab.mock.refuse_next("GET", &path, 500));
    let run = lab.ctl(&["calendar", "show", &id]);
    assert_eq!(values(run.text(), "recurrence"), ["Part of a series"], "{run:?}");
}

#[test]
fn the_calendar_list_keeps_its_rows_and_an_unknown_event_is_a_clear_error() {
    let lab = Lab::start();
    let week = lab.week();
    assert_eq!(week.len(), 15, "{week:?}");
    for row in &week {
        for detail in DETAILS {
            assert!(row.get(detail).is_none(), "{detail}: {row}");
        }
    }

    let run = lab.ctl(&["calendar", "--from", "2026-11-02", "--to", "2026-11-08"]);
    let table = run.text();
    let mut lines = table.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split_whitespace().collect();
    assert_eq!(header, ["ID", "START", "END", "FLAGS", "SUBJECT"], "{table}");
    assert_eq!(lines.count(), 15, "{table}");
    for detail in [
        "attendee",
        "someone@example.com",
        "meetup-join",
        "Occurs every",
        "Made-up notes",
    ] {
        assert!(!table.contains(detail), "{detail}: {table}");
    }

    let unknown = lab.ctl(&["calendar", "show", "AAMkNoSuchEvent"]);
    assert_eq!(unknown.code, 1, "{unknown:?}");
    assert!(unknown.stdout.is_empty(), "{unknown:?}");
    assert_eq!(
        unknown.stderr,
        "ngteamsctl: unknown_event: no event AAMkNoSuchEvent in the calendar: `ngteamsctl calendar` lists them\n",
        "{unknown:?}"
    );

    let unknown = lab.ctl(&["--json", "calendar", "show", "AAMkNoSuchEvent"]);
    assert_eq!(unknown.code, 1, "{unknown:?}");
    let line: Value = serde_json::from_str(&unknown.stdout).unwrap_or_else(|error| panic!("{error}: {unknown:?}"));
    assert_eq!(line["error"]["code"], "unknown_event", "{line}");
    assert_eq!(
        line["error"]["message"], "no event AAMkNoSuchEvent in the calendar: `ngteamsctl calendar` lists them",
        "{line}"
    );
}
