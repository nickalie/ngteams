//! QA of task 1041: a person finds people, reads a profile card and reads presence with `ngteamsctl`, against the
//! running app. Each `ngteamsctl` run is a process of its own: this test binary started again, which runs
//! `teams_ctl::cli` as the `ngteamsctl` binary does (its arguments, its environment, its stdout, its stderr and its
//! exit code). The app runs in this process, lab account a's engine on the mock (`teams-mock`, loopback only): the
//! people search and UPS are made-up answers that this test serves, and the card is the mock's Loki.
//!
//! - **people**: several matches, no match and `--limit`, as the table and as `--json`.
//! - **person**: by MRI, UPN, email and name in any case; the card's presence, last active, out of office and
//!   calendar; an ambiguous name (exit 1, the candidates) and an unknown person (exit 1).
//! - **presence**: several people, a bot that has no presence (not waited for), and 21 people (refused).
//! - **read-only**: the app in the read-only mode serves all three.
//!
//! Every name, address and id is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]
#![cfg(target_os = "linux")]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

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
use teams_mock::{CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, LokiCard, PRESENCE_BASE, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};
use wiremock::matchers::{method, path};
use wiremock::{Mock, Request as Asked, ResponseTemplate};

/// The JSON array of arguments that makes this binary run as `ngteamsctl`.
const CHILD_ARGS: &str = "CTL_PEOPLE_CLI_ARGS";
/// The files the child writes its stdout and its stderr in.
const CHILD_OUT: &str = "CTL_PEOPLE_CLI_OUT";
const CHILD_ERR: &str = "CTL_PEOPLE_CLI_ERR";
/// The name of the test that is the child.
const CHILD_TEST: &str = "ngteamsctl_child";

const UPN: &str = "bot-a@lab.example.test";
const B_MAIL: &str = "bee@lab.example.test";
const C_MAIL: &str = "cee@lab.example.test";
/// c's UPN is not c's mail, as a guest's is.
const C_UPN: &str = "cee_guest.test#EXT#@lab.example.test";
const DEE_ONE: &str = "8:orgid:00000000-0000-4000-8000-0000000000d1";
const DEE_TWO: &str = "8:orgid:00000000-0000-4000-8000-0000000000d2";
const BEE_TWO: &str = "8:orgid:00000000-0000-4000-8000-0000000000b2";
const DEE_OTHER: &str = "8:orgid:00000000-0000-4000-8000-0000000000d3";
const NOBODY_ELSE: &str = "8:orgid:00000000-0000-4000-8000-0000000000e1";

/// The presence that UPS gives, and the time last active that it gives for each person.
const PRESENCE: &[(&str, &str)] = &[(LAB_B, "Busy"), (LAB_C, "Away")];
const LAST_ACTIVE: &str = "2026-11-20T08:30:00Z";
const LAST_ACTIVE_MS: i64 = 1_795_163_400_000;

/// A made-up directory entry: `(MRI, name, UPN, mail, job title, department)`.
type Entry = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

/// Bee Example Two, Dee Other and Nobody Else match only a part of a name or of a query, so that a part never
/// passes for the whole.
const DIRECTORY: &[Entry] = &[
    (LAB_B, "Bee Example", B_MAIL, B_MAIL, "Tester", "Lab"),
    (LAB_C, "Cee Example", C_UPN, C_MAIL, "Builder", "Lab"),
    (
        DEE_ONE,
        "Dee Example",
        "dee.one@lab.example.test",
        "dee.one@lab.example.test",
        "Writer",
        "Docs",
    ),
    (
        DEE_TWO,
        "Dee Example",
        "dee.two@lab.example.test",
        "dee.two@lab.example.test",
        "Reader",
        "Docs",
    ),
    (
        BEE_TWO,
        "Bee Example Two",
        "bee.two@lab.example.test",
        "bee.two@lab.example.test",
        "Tester",
        "Lab",
    ),
    (
        DEE_OTHER,
        "Dee Other",
        "dee.other@lab.example.test",
        "dee.other@lab.example.test",
        "Writer",
        "Docs",
    ),
    (
        NOBODY_ELSE,
        "Nobody Else",
        "nobody.else@lab.example.test",
        "nobody.else@lab.example.test",
        "Guest",
        "Lab",
    ),
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
    fn ok(&self) -> &str {
        assert_eq!(self.code, 0, "{self:?}");
        &self.stdout
    }

    fn json(&self) -> Value {
        let line: Value = serde_json::from_str(self.ok()).unwrap_or_else(|error| panic!("{error}: {self:?}"));
        line["ok"].clone()
    }

    fn json_error(&self) -> Value {
        assert_eq!(self.code, 1, "{self:?}");
        let line: Value = serde_json::from_str(&self.stdout).unwrap_or_else(|error| panic!("{error}: {self:?}"));
        line["error"].clone()
    }
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

/// Substrate's answer to a results request: the directory's people whose name, UPN or mail holds the query, in any
/// case.
fn directory_answer(request: &Asked) -> ResponseTemplate {
    let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
    let query = body["EntityRequests"][0]["query"]["queryString"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let results: Vec<Value> = DIRECTORY
        .iter()
        .filter(|(_, name, upn, mail, _, _)| {
            [name, upn, mail]
                .iter()
                .any(|text| text.to_lowercase().contains(&query))
        })
        .map(|(mri, name, upn, mail, title, department)| {
            json!({ "Type": "People", "Source": {
                "DisplayName": name, "MRI": mri, "UserPrincipalName": upn, "EmailAddresses": [mail],
                "JobTitle": title, "Department": department,
            }})
        })
        .collect();
    let answer = json!({ "EntitySets": [{ "ResultSets": [{ "Results": results, "MoreResultsAvailable": false }] }] });
    ResponseTemplate::new(200).set_body_raw(answer.to_string(), "application/json")
}

/// UPS's answer to a presence read, as the mock's own, with a time last active that is a real one.
fn presence_answer(request: &Asked) -> ResponseTemplate {
    let asked: Vec<Value> = serde_json::from_slice(&request.body).unwrap_or_default();
    let items: Vec<Value> = asked
        .iter()
        .filter_map(|ask| ask["mri"].as_str())
        .map(|mri| {
            let availability = PRESENCE
                .iter()
                .find(|(known, _)| *known == mri)
                .map_or("Offline", |(_, availability)| availability);
            json!({
                "mri": mri,
                "presence": {
                    "sourceNetwork": "SameEnterprise",
                    "calendarData": {"isOutOfOffice": false},
                    "capabilities": [],
                    "availability": availability,
                    "activity": availability,
                    "lastActiveTime": LAST_ACTIVE,
                },
                "etagMatch": false,
                "etag": "made-up-tag",
                "status": 20000,
            })
        })
        .collect();
    ResponseTemplate::new(200).set_body_raw(Value::Array(items).to_string(), "application/json")
}

/// The mock, and the app of lab account a on it serving its control socket.
struct Lab {
    socket: PathBuf,
    cx: TestAppContext,
    _mock: ChatServiceMock,
    dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl Lab {
    fn start(read_only: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(async {
            let mock = ChatServiceMock::start().await.unwrap();
            mock.mount(
                Mock::given(method("POST"))
                    .and(path(format!(
                        "{}/v1/presence/getpresence/",
                        PRESENCE_BASE.trim_start_matches("https://teams.microsoft.com")
                    )))
                    .respond_with(presence_answer)
                    .with_priority(1),
            )
            .await;
            mock.serve_loki(&[LokiCard {
                mri: LAB_B,
                name: "Bee Example",
                email: B_MAIL,
            }])
            .await;
            mock.mount(
                Mock::given(method("POST"))
                    .and(path("/searchservice/api/v2/query"))
                    .respond_with(directory_answer)
                    .with_priority(1),
            )
            .await;
            mock
        });
        let dir = tempfile::tempdir().unwrap();
        let handle = {
            let _entered = runtime.enter();
            a_engine(&mock, &dir, read_only)
        };
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let cx = TestAppContext::single();
        let settings = app::settings(SettingsFile::new(dir.path().join("settings.toml")), None);
        let launch = Launch {
            first: started(handle),
            others: Vec::new(),
            settings: if read_only { settings.read_only() } else { settings },
            bus: Err(BusError::NotGiven),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap(),
                floor: CallFloor::new(),
                devices: None,
            },
            read_only,
            trace: Arc::new(ViewTrace::new(std::io::sink())),
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
            dir,
            _runtime: runtime,
        }
    }

    /// `ngteamsctl <args>` against the app, while the test thread steps the app.
    fn ngteamsctl(&self, args: &[&str]) -> Run {
        let logs = tempfile::tempdir_in(self.dir.path()).unwrap();
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
            .env("NGTEAMS_CTL_SOCKET", &self.socket)
            .env("HOME", logs.path())
            .env("XDG_RUNTIME_DIR", logs.path())
            .env("TZ", "UTC");
        let thread = std::thread::spawn(move || command.output().unwrap());
        pump_until_done(&self.cx, &thread);
        let output = thread.join().unwrap();
        assert!(
            output.status.code() != Some(101),
            "ngteamsctl {args:?} panicked: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: std::fs::read_to_string(&out).unwrap_or_default(),
            stderr: std::fs::read_to_string(&err).unwrap_or_default(),
        }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// a's engine on the mock without a push channel or calling; its reads find Loki and UPS on the mock. In the
/// read-only mode, its transports send reads only, as `ngteams --read-only` starts them.
fn a_engine(mock: &ChatServiceMock, dir: &tempfile::TempDir, read_only: bool) -> EngineHandle {
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || {
        let transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin).unwrap();
        if read_only { transport.read_only() } else { transport }
    };
    let reads = http().with_presence(PRESENCE_BASE).unwrap().with_loki();
    engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(reads)),
        outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing.
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

/// The MRIs of a `people` or `presence` answer, in order.
fn mris(answer: &Value, key: &str) -> Vec<String> {
    answer[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["mri"].as_str().unwrap_or("-").to_owned())
        .collect()
}

#[test]
fn a_person_finds_people_as_a_table_and_as_json() {
    let lab = Lab::start(false);
    let table = lab.ngteamsctl(&["people", "example"]).ok().to_owned();
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 8, "a heading and seven people:\n{table}");
    assert!(lines[0].starts_with("MRI"), "{table}");
    assert!(
        lines[0].contains("PRESENCE") && lines[0].contains("DEPARTMENT"),
        "{table}"
    );
    let bee: Vec<&str> = lines[1].split_whitespace().collect();
    assert_eq!(bee, [LAB_B, "-", "Bee", "Example", B_MAIL, "Tester", "Lab"], "{table}");

    let found = lab.ngteamsctl(&["--json", "people", "Dee", "example"]).json();
    assert_eq!(
        mris(&found, "people"),
        [DEE_ONE, DEE_TWO],
        "the words are one query, not Dee Other: {found}"
    );
    let limited = lab.ngteamsctl(&["--json", "people", "example", "--limit", "1"]).json();
    assert_eq!(mris(&limited, "people"), [LAB_B], "{limited}");

    let none = lab.ngteamsctl(&["people", "nobody", "at", "all"]);
    assert_eq!(
        none.ok().lines().count(),
        1,
        "the heading only, not Nobody Else: {none:?}"
    );
    let none = lab.ngteamsctl(&["--json", "people", "nobody", "at", "all"]).json();
    assert_eq!(none["people"], json!([]), "{none}");
}

#[test]
fn a_person_reads_one_card_by_each_name_in_any_case() {
    let lab = Lab::start(false);
    for who in [LAB_B, B_MAIL, &B_MAIL.to_uppercase(), "bee example", "BEE EXAMPLE"] {
        let card = lab.ngteamsctl(&["--json", "person", who]).json();
        assert_eq!(card["mri"], LAB_B, "{who}: {card}");
        assert_eq!(card["name"], "Bee Example", "{who}: {card}");
    }

    // the card's presence and calendar: UPS says busy, the mock's Loki free now and next, not out of office
    let card = lab.ngteamsctl(&["--json", "person", B_MAIL]).json();
    assert_eq!(card["presence"], "busy", "{card}");
    assert_eq!(card["last_active"], LAST_ACTIVE_MS, "{card}");
    assert_eq!(card["out_of_office"], false, "{card}");
    assert_eq!(card["free_busy"], "free", "{card}");
    assert_eq!(card["next_free_busy"], "free", "{card}");
    let shown = lab.ngteamsctl(&["person", B_MAIL]).ok().to_owned();
    for line in [
        format!("mri:           {LAB_B}"),
        "name:          Bee Example".to_owned(),
        "presence:      busy".to_owned(),
        "last active:   2026-11-20 08:30".to_owned(),
        "out of office: no".to_owned(),
        "calendar:      free".to_owned(),
        "then:          free".to_owned(),
    ] {
        assert!(shown.contains(&line), "{line:?} not in\n{shown}");
    }

    // c has no card: the search's fields, and the presence read as `presence` reads it
    for who in ["CEE@lab.example.test", &C_UPN.to_uppercase()] {
        let c = lab.ngteamsctl(&["person", who]).ok().to_owned();
        let fields: Vec<(&str, &str)> = c
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key, value.trim()))
            .collect();
        for field in [
            ("mri", LAB_C),
            ("name", "Cee Example"),
            ("upn", C_UPN),
            ("email", C_MAIL),
            ("title", "Builder"),
            ("presence", "away"),
        ] {
            assert!(fields.contains(&field), "{who}: {field:?} not in\n{c}");
        }
    }
}

#[test]
fn a_person_learns_that_a_name_is_ambiguous_or_names_nobody() {
    let lab = Lab::start(false);
    let ambiguous = lab.ngteamsctl(&["person", "Dee Example"]);
    assert_eq!(ambiguous.code, 1, "{ambiguous:?}");
    assert!(ambiguous.stderr.contains("ambiguous_person"), "{ambiguous:?}");
    for candidate in [
        format!("{DEE_ONE}  Dee Example <dee.one@lab.example.test>"),
        format!("{DEE_TWO}  Dee Example <dee.two@lab.example.test>"),
    ] {
        assert!(
            ambiguous.stderr.contains(&candidate),
            "{candidate:?} not in {ambiguous:?}"
        );
    }
    let error = lab.ngteamsctl(&["--json", "person", "dee example"]).json_error();
    assert_eq!(error["code"], "ambiguous_person", "{error}");
    assert_eq!(error["candidates"].as_array().map(Vec::len), Some(2), "{error}");

    for nobody in ["nobody@lab.example.test", "Nobody Here"] {
        let run = lab.ngteamsctl(&["person", nobody]);
        assert_eq!(run.code, 1, "{run:?}");
        assert!(run.stderr.contains("unknown_person"), "{run:?}");
        let error = lab.ngteamsctl(&["--json", "person", nobody]).json_error();
        assert_eq!(error["code"], "unknown_person", "{error}");
    }
    let run = lab.ngteamsctl(&["presence", LAB_B, "Nobody Here"]);
    assert!(
        run.code == 1 && run.stderr.contains("unknown_person"),
        "one unknown person refuses the whole ask: {run:?}"
    );
}

#[test]
fn a_person_reads_the_presence_of_several_people_and_of_a_bot() {
    let lab = Lab::start(false);
    let table = lab
        .ngteamsctl(&["presence", LAB_B, C_MAIL, "Bee Example"])
        .ok()
        .to_owned();
    let rows: Vec<Vec<&str>> = table.lines().map(|line| line.split_whitespace().collect()).collect();
    assert_eq!(
        rows,
        [
            vec!["MRI", "PRESENCE", "NAME"],
            vec![LAB_B, "busy", "Bee", "Example"],
            vec![LAB_C, "away", "Cee", "Example"],
        ],
        "b once:\n{table}"
    );

    // a bot has no presence: it is unknown at once; a wait for it would hang the ask, as the test clock stays
    let bot = lab.ngteamsctl(&["--json", "presence", ECHO, LAB_C]).json();
    let rows: Vec<(&str, &str)> = bot["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["mri"].as_str().unwrap(), row["availability"].as_str().unwrap()))
        .collect();
    assert_eq!(rows, [(ECHO, "unknown"), (LAB_C, "away")], "{bot}");
    let card = lab.ngteamsctl(&["--json", "person", ECHO]).json();
    assert_eq!(card["mri"], ECHO, "{card}");
    assert!(card.get("presence").is_none_or(Value::is_null), "{card}");

    let many: Vec<String> = (0..21)
        .map(|index| format!("8:orgid:00000000-0000-4000-8000-{index:012}"))
        .collect();
    let mut args = vec!["presence"];
    args.extend(many.iter().map(String::as_str));
    let refused = lab.ngteamsctl(&args);
    assert_eq!(refused.code, 2, "clap refuses more than 20: {refused:?}");
    assert!(refused.stdout.is_empty(), "{refused:?}");
}

#[test]
fn the_read_only_app_serves_people_person_and_presence() {
    let lab = Lab::start(true);
    let found = lab.ngteamsctl(&["--json", "people", "example"]).json();
    assert_eq!(
        mris(&found, "people"),
        [LAB_B, LAB_C, DEE_ONE, DEE_TWO, BEE_TWO, DEE_OTHER, NOBODY_ELSE],
        "the search is a read: {found}"
    );
    let card = lab.ngteamsctl(&["--json", "person", B_MAIL]).json();
    assert_eq!(
        (&card["mri"], &card["presence"]),
        (&json!(LAB_B), &json!("busy")),
        "{card}"
    );
    let presence = lab.ngteamsctl(&["--json", "presence", LAB_B, LAB_C]).json();
    let rows: Vec<&str> = presence["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["availability"].as_str().unwrap())
        .collect();
    assert_eq!(rows, ["busy", "away"], "{presence}");
}
