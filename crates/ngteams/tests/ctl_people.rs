//! The control socket's people reads (`ctl::people`), through the real protocol and server, over lab account a's
//! engine on the mock: the people search (Substrate's results, a made-up directory that this test serves), the
//! profile card (the mock's Loki) and others' presence (the mock's UPS, read by the engine's own presence reads). Each
//! is a read that reaches nobody. Every name, address and id is made up; the directory's answers are written by hand
//! in the captured shape (`docs/protocol/search.md`).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use ctl_support::{ask, quit};
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
use teams_ctl::ClientError;
use teams_ctl::protocol::{CtlError, ErrorCode, PeopleRow, PersonCard, PresenceRow, Request, Response};
use teams_mock::{CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, LokiCard, PRESENCE_BASE, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};
use wiremock::matchers::{method, path};
use wiremock::{Mock, Request as Asked, ResponseTemplate};

const UPN: &str = "bot-a@lab.example.test";
const B_MAIL: &str = "bee@lab.example.test";
const C_MAIL: &str = "cee@lab.example.test";
/// Two made-up people of one name, found by the search only.
const DEE_ONE: &str = "8:orgid:00000000-0000-4000-8000-0000000000d1";
const DEE_TWO: &str = "8:orgid:00000000-0000-4000-8000-0000000000d2";
/// A made-up guest, whose UPN the search gives, not their mail.
const GUEST: &str = "8:orgid:00000000-0000-4000-8000-0000000000e1";
const GUEST_MAIL: &str = "eve@guest.test";
const GUEST_UPN: &str = "eve_guest.test#EXT#@lab.onmicrosoft.test";

/// The made-up directory: `(MRI, name, UPN, mail, job title, department)`.
const DIRECTORY: &[(&str, &str, &str, &str, &str, &str)] = &[
    (LAB_B, "Bee Example", B_MAIL, B_MAIL, "Tester", "Lab"),
    (LAB_C, "Cee Example", C_MAIL, C_MAIL, "Builder", "Lab"),
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
    (GUEST, "Eve Guest", GUEST_UPN, GUEST_MAIL, "Visitor", "Guests"),
];

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

/// Substrate's answer to a results request: the directory's people whose name or mail holds the query, in any case.
fn directory_answer(request: &Asked) -> ResponseTemplate {
    let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
    let query = body["EntityRequests"][0]["query"]["queryString"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    let results: Vec<Value> = DIRECTORY
        .iter()
        .filter(|(_, name, _, mail, _, _)| name.to_lowercase().contains(&query) || mail.contains(&query))
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

/// The mock, and the app of lab account a on it serving its control socket.
struct Lab {
    socket: PathBuf,
    cx: TestAppContext,
    mock: ChatServiceMock,
    _dir: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Lab {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(async {
            let mock = ChatServiceMock::start().await.unwrap();
            mock.serve_presence(&[(LAB_B, "Busy"), (LAB_C, "Away"), (GUEST, "Available")])
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
            a_engine(&mock, &dir)
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
            mock,
            _dir: dir,
            runtime,
        }
    }

    fn ask(&self, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, None, request)
    }

    /// The error `request` answers with.
    fn refused(&self, request: Request) -> CtlError {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("{asked}: {other:?}"),
        }
    }

    fn people(&self, query: &str, limit: Option<u32>) -> Vec<PeopleRow> {
        let request = Request::People {
            query: query.to_owned(),
            limit,
        };
        match self.ask(request) {
            Ok(Response::People { people }) => people,
            other => panic!("people {query}: {other:?}"),
        }
    }

    fn person(&self, who: &str) -> PersonCard {
        match self.ask(Request::Person { who: who.to_owned() }) {
            Ok(Response::Person(card)) => *card,
            other => panic!("person {who}: {other:?}"),
        }
    }

    fn presence(&self, people: &[&str]) -> Vec<PresenceRow> {
        let request = Request::Presence {
            people: people.iter().map(|person| (*person).to_owned()).collect(),
        };
        match self.ask(request) {
            Ok(Response::Presence { people }) => people,
            other => panic!("presence {people:?}: {other:?}"),
        }
    }

    /// The paths of the requests the mock got that hold `part`.
    fn asked(&self, part: &str) -> usize {
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.url.path().contains(part))
            .count()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// a's engine on the mock without a push channel or calling; its reads find Loki and UPS on the mock, and it reads
/// the presence of whom it watches as soon as it is asked.
fn a_engine(mock: &ChatServiceMock, dir: &tempfile::TempDir) -> EngineHandle {
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin).unwrap();
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

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing (an empty
/// in-memory store, a loopback port nothing listens on).
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

#[test]
fn people_lists_each_match_of_the_search_and_none_for_no_match() {
    let lab = Lab::start();
    let found = lab.people(" example ", None);
    let names: Vec<(&str, Option<&str>)> = found
        .iter()
        .map(|person| (person.name.as_str(), person.mri.as_deref()))
        .collect();
    assert_eq!(
        names,
        [
            ("Bee Example", Some(LAB_B)),
            ("Cee Example", Some(LAB_C)),
            ("Dee Example", Some(DEE_ONE)),
            ("Dee Example", Some(DEE_TWO)),
        ]
    );
    assert_eq!(found[0].email.as_deref(), Some(B_MAIL));
    assert_eq!(
        (found[0].job_title.as_deref(), found[0].department.as_deref()),
        (Some("Tester"), Some("Lab"))
    );
    assert!(
        found.iter().all(|person| person.presence.is_none()),
        "nobody's presence is known before anyone asked: {found:?}"
    );
    assert_eq!(lab.people("example", Some(2)).len(), 2);
    assert!(lab.people("nobody at all", None).is_empty());
    assert_eq!(
        lab.refused(Request::People {
            query: s(" "),
            limit: None
        })
        .code,
        ErrorCode::BadRequest
    );
}

#[test]
fn person_shows_the_card_by_upn_by_mri_and_by_name() {
    let lab = Lab::start();
    let by_upn = lab.person(&B_MAIL.to_uppercase());
    assert_eq!(by_upn.mri, LAB_B);
    assert_eq!(by_upn.name.as_deref(), Some("Bee Example"));
    assert_eq!(by_upn.upn.as_deref(), Some(B_MAIL));
    assert_eq!(by_upn.emails.first().map(String::as_str), Some(B_MAIL));
    assert_eq!(by_upn.presence.as_deref(), Some("busy"), "{by_upn:?}");
    assert_eq!(
        by_upn.job_title.as_deref(),
        Some("Tester"),
        "the search's, which the card lacks"
    );
    let by_mri = lab.person(LAB_B);
    assert_eq!(
        (
            by_mri.mri.as_str(),
            by_mri.name.as_deref(),
            by_mri.upn.as_deref(),
            by_mri.presence.as_deref()
        ),
        (LAB_B, Some("Bee Example"), Some(B_MAIL), Some("busy")),
        "by MRI, the same card"
    );
    assert_eq!(lab.person("bee example").mri, LAB_B, "by name, in any case");
    assert_eq!(
        lab.person("Cee").mri,
        LAB_C,
        "a part of a name, the search's only person"
    );

    // c has no card on the mock: the card has what the search gave, its presence read as `presence` reads it
    let c = lab.person(C_MAIL);
    assert_eq!(
        (c.mri.as_str(), c.name.as_deref(), c.job_title.as_deref()),
        (LAB_C, Some("Cee Example"), Some("Builder"))
    );
    assert_eq!(c.emails, [C_MAIL]);
    assert_eq!(c.presence.as_deref(), Some("away"), "{c:?}");

    let guest = lab.person(GUEST_MAIL);
    assert_eq!(
        (guest.mri.as_str(), guest.name.as_deref()),
        (GUEST, Some("Eve Guest")),
        "by a mail that isn't the UPN the search gives, the search's only person"
    );
    let rows = lab.presence(&[GUEST_MAIL]);
    assert_eq!(
        (rows[0].mri.as_str(), rows[0].availability.as_str()),
        (GUEST, "available"),
        "{rows:?}"
    );
}

#[test]
fn person_names_whom_it_cannot_tell_apart_and_refuses_nobody() {
    let lab = Lab::start();
    let ambiguous = lab.refused(Request::Person { who: s("Dee Example") });
    assert_eq!(ambiguous.code, ErrorCode::AmbiguousPerson, "{ambiguous:?}");
    let candidates: Vec<(&str, &str)> = ambiguous
        .candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate.title.as_str()))
        .collect();
    assert_eq!(
        candidates,
        [
            (DEE_ONE, "Dee Example <dee.one@lab.example.test>"),
            (DEE_TWO, "Dee Example <dee.two@lab.example.test>"),
        ]
    );
    let part = lab.refused(Request::Person { who: s("Example") });
    assert_eq!(
        (part.code, part.candidates.len()),
        (ErrorCode::AmbiguousPerson, 4),
        "{part:?}"
    );
    for nobody in ["nobody@lab.example.test", "Nobody Here"] {
        let error = lab.refused(Request::Person { who: s(nobody) });
        assert_eq!(error.code, ErrorCode::UnknownPerson, "{nobody}: {error:?}");
    }
    assert_eq!(lab.refused(Request::Person { who: s("") }).code, ErrorCode::BadRequest);
}

#[test]
fn presence_reads_several_people_as_the_badges_do() {
    let lab = Lab::start();
    let rows = lab.presence(&[LAB_B, C_MAIL, "Bee Example"]);
    assert_eq!(
        rows,
        [
            PresenceRow {
                mri: s(LAB_B),
                name: Some(s("Bee Example")),
                availability: s("busy"),
            },
            PresenceRow {
                mri: s(LAB_C),
                name: Some(s("Cee Example")),
                availability: s("away"),
            },
        ],
        "b once, asked by MRI and by name, named by the search"
    );
    assert!(lab.asked("/v1/presence/getpresence/") > 0, "the engine read them");
    // known now: the people search shows it
    let found = lab.people("example", None);
    let presence: Vec<Option<&str>> = found.iter().map(|person| person.presence.as_deref()).collect();
    assert_eq!(presence, [Some("busy"), Some("away"), None, None]);

    let too_many: Vec<String> = (0..21).map(|index| format!("8:orgid:made-up-{index}")).collect();
    for people in [vec![s(" ")], too_many] {
        let error = lab.refused(Request::Presence { people });
        assert_eq!(error.code, ErrorCode::BadRequest, "{error:?}");
    }
    let unknown = lab.refused(Request::Presence {
        people: vec![s(LAB_B), s("Nobody Here")],
    });
    assert_eq!(unknown.code, ErrorCode::UnknownPerson);
}

fn s(text: &str) -> String {
    text.to_owned()
}
