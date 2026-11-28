//! `ngteamsctl` reads a team's channels (`ctl::reads`, `ctl::address`): the command line run in this process against
//! the app of lab account a, its engine on the mock chat service (`teams-mock`, loopback only, through the
//! `test-support` mock origins). The mock's team has two channels: the general one, in a's conversation list, and the
//! other one, in no list, which holds the posts and replies of `channel-messages.json`. The test adds a post with a
//! subject that came by email, and a reply to it. The store holds nothing of the other channel before `messages`
//! syncs it. Every id, name and text is made up or the mock's own.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser as _;
use ctl_support::{pump_until, pumped, pumped_advancing, quit};
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
use teams_core::engine::{self, EngineOptions, Parts};
use teams_core::{CallFloor, EngineRequests, Outbox};
use teams_ctl::cli::{self, Cli};
use teams_ctl::path::Env;
use teams_mock::{CHANNEL, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, TEAM_CHANNEL, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};

const UPN: &str = "bot-a@lab.example.test";
const TEAM: &str = "A made-up team";
const OTHER: &str = "A made-up channel";
/// The post of `channel-messages.json` with two replies.
const POST: &str = "1760000300000";
const REPLY: &str = "1760000300100";
const PLAIN_REPLY: &str = "1760000300200";
/// The post of `channel-messages.json` without a reply.
const SECOND_POST: &str = "1760000400000";
const SUBJECT: &str = "Made-up weekly alert";

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
    requests: EngineRequests,
    socket: PathBuf,
    cx: TestAppContext,
    _dir: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Lab {
    /// The app of a on the mock, its store caught up first.
    fn start() -> Self {
        Self::start_with(|_, _| {})
    }

    /// [`Self::start`], `prepare` given the mock before a's engine starts.
    fn start_with(prepare: impl FnOnce(&ChatServiceMock, &tokio::runtime::Runtime)) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        prepare(&mock, &runtime);
        let dir = tempfile::tempdir().unwrap();
        let http = || {
            let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
            HttpTransport::with_mock_origin(tokens, CHAT_BASE, &mock.origin()).unwrap()
        };
        let handle = engine::start(Parts {
            calls: None,
            store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
            client: Arc::new(Client::new(http().with_csa(CSA_BASE).unwrap())),
            outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
            push: None,
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })
        .unwrap();
        let requests = handle.requests();
        requests.sync().blocking_wait().unwrap();
        let socket = dir.path().join("run").join("ngteams").join("ctl.sock");
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
            requests,
            socket,
            cx,
            _dir: dir,
            runtime,
        }
    }

    /// `ngteamsctl <args>` as the binary runs it: its exit code, its stdout and its stderr.
    fn ctl(&self, args: &[&str]) -> (u8, String, String) {
        pumped(&self.cx, self.ctl_run(args))
    }

    /// The run of `ngteamsctl <args>` on the client's own thread.
    fn ctl_run(&self, args: &[&str]) -> impl FnOnce() -> (u8, String, String) + Send + 'static {
        let args: Vec<String> = ["ngteamsctl", "--socket", self.socket.to_str().unwrap()]
            .into_iter()
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect();
        move || {
            let parsed = Cli::try_parse_from(&args).unwrap();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let exit = cli::run(&parsed, &Env::default(), &mut out, &mut err) as u8;
            (exit, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
        }
    }

    /// The JSON answer of `ngteamsctl --json <args>`, which must succeed.
    fn json(&self, args: &[&str]) -> Value {
        let args: Vec<&str> = std::iter::once("--json").chain(args.iter().copied()).collect();
        let (exit, out, err) = self.ctl(&args);
        assert_eq!(exit, 0, "{args:?}: {out}{err}");
        serde_json::from_str(&out).unwrap()
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

/// The rows of a `messages` or `thread` answer, by id.
fn rows(answer: &Value) -> Vec<&Value> {
    answer["ok"]["messages"].as_array().unwrap().iter().collect()
}

fn row<'a>(answer: &'a Value, id: &str) -> &'a Value {
    rows(answer)
        .into_iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("no row {id} in {answer}"))
}

fn ids(answer: &Value) -> Vec<&str> {
    rows(answer).iter().map(|row| row["id"].as_str().unwrap()).collect()
}

/// b's post with a subject, which came by email, and a reply of a's to it: their ids.
fn email_post(lab: &Lab) -> (String, String) {
    let details = json!({
        "from": {"name": "Made-up Alerts", "smtp": "alerts@lab.example.test"},
        "to": [{"name": OTHER, "smtp": "channel@lab.example.test"}],
        "cc": [{"smtp": "someone@lab.example.test"}],
        "totalRecipients": 4,
    });
    let post = lab
        .mock
        .add_message_with(
            TEAM_CHANNEL,
            (LAB_B, "bot-b"),
            "RichText/Html",
            "<p>The alert's text.</p>",
            |message| {
                message["properties"] = json!({"subject": SUBJECT, "emaildetails": details.to_string()});
            },
        )
        .unwrap();
    let reply = lab
        .mock
        .add_message(
            &format!("{TEAM_CHANNEL};messageid={post}"),
            (LAB_A, "bot-a"),
            "RichText/Html",
            "<p>Seen.</p>",
        )
        .unwrap();
    (post, reply)
}

#[test]
fn teams_lists_the_teams_and_their_channels_with_their_ids() {
    let lab = Lab::start();
    let answer = lab.json(&["teams"]);
    let teams = answer["ok"]["teams"].as_array().unwrap();
    let team = teams.iter().find(|team| team["name"] == TEAM).unwrap();
    assert_eq!(team["id"], CHANNEL, "{answer}");
    let channels: Vec<(&str, &str)> = team["channels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|channel| (channel["id"].as_str().unwrap(), channel["title"].as_str().unwrap()))
        .collect();
    let general = format!("{TEAM} > General");
    let other = format!("{TEAM} > {OTHER}");
    assert!(channels.contains(&(CHANNEL, general.as_str())), "{answer}");
    assert!(channels.contains(&(TEAM_CHANNEL, other.as_str())), "{answer}");
    let general_row = team["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|channel| channel["id"] == CHANNEL)
        .unwrap();
    assert_eq!(
        (&general_row["general"], &general_row["channel_type"]),
        (&json!(true), &json!("standard"))
    );

    let (exit, out, err) = lab.ctl(&["teams"]);
    assert_eq!(exit, 0, "{err}");
    assert!(out.starts_with("ID "), "{out}");
    assert!(
        out.lines()
            .any(|line| line.starts_with(TEAM_CHANNEL) && line.ends_with(&other)),
        "{out}"
    );
}

/// The rows of `answer` of b's email post `email` and of a's reply `seen` to it.
fn email_row(answer: &Value, email: &str, seen: &str) {
    let post = row(answer, email);
    assert_eq!(
        (&post["subject"], &post["replies"]),
        (&json!(SUBJECT), &json!(1)),
        "{answer}"
    );
    let from = &post["email"]["from"];
    assert_eq!(
        (&from["name"], &from["address"]),
        (&json!("Made-up Alerts"), &json!("alerts@lab.example.test"))
    );
    assert_eq!(post["email"]["recipients"], 4, "{answer}");
    assert_eq!(
        post["email"]["cc"][0]["address"], "someone@lab.example.test",
        "{answer}"
    );
    assert_eq!(row(answer, seen)["post"], json!(email), "{answer}");
}

/// `messages` prints the channel's table for a person, named `<team>/<channel>`.
fn table_of_the_channel(lab: &Lab) {
    let (exit, out, err) = lab.ctl(&["messages", &format!("{TEAM}/{OTHER}")]);
    assert_eq!(exit, 0, "{err}");
    assert!(out.contains(&format!("(reply to {POST}) a made-up reply")), "{out}");
    assert!(out.contains("a made-up post to the channel (2 replies)"), "{out}");
    assert!(
        out.contains(&format!(
            "(email from Made-up Alerts <alerts@lab.example.test> to {OTHER} <channel@lab.example.test>, \
             someone@lab.example.test and 2 more) {SUBJECT} ↵ The alert's text. (1 reply)"
        )),
        "{out}"
    );
    assert!(out.ends_with("(the first message: there is nothing older)\n"), "{out}");
}

#[test]
fn messages_syncs_a_channel_named_by_its_team_and_tells_posts_from_replies() {
    let lab = Lab::start();
    let (email, seen) = email_post(&lab);
    let stored = lab.requests.messages(TEAM_CHANNEL, None, 10).blocking_wait().unwrap();
    assert!(stored.messages.is_empty(), "no catch-up reads the channel: {stored:?}");

    let answer = lab.json(&["messages", &format!("{TEAM} > {OTHER}")]);
    assert_eq!(answer["ok"]["synced"], true, "{answer}");
    assert_eq!(answer["ok"]["end"], "beginning", "{answer}");
    assert_eq!(row(&answer, POST)["replies"], 2, "{answer}");
    assert!(row(&answer, POST).get("post").is_none(), "{answer}");
    assert_eq!(row(&answer, SECOND_POST)["replies"], 0, "{answer}");
    for reply in [REPLY, PLAIN_REPLY] {
        assert_eq!(row(&answer, reply)["post"], POST, "{answer}");
        assert!(row(&answer, reply).get("replies").is_none(), "{answer}");
    }
    email_row(&answer, &email, &seen);

    // by its id and by its name alone, the same rows; one page back, the rest
    assert_eq!(ids(&lab.json(&["messages", TEAM_CHANNEL])), ids(&answer));
    assert_eq!(ids(&lab.json(&["messages", OTHER])), ids(&answer));
    let page = lab.json(&["messages", TEAM_CHANNEL, "--limit", "2"]);
    assert_eq!(page["ok"].get("end"), None, "{page}");
    let older = lab.json(&["messages", TEAM_CHANNEL, "--before", ids(&page)[1]]);
    assert_eq!(older["ok"].get("synced"), None, "a page back reads the store: {older}");
    assert_eq!([ids(&page), ids(&older)].concat(), ids(&answer));

    table_of_the_channel(&lab);
}

#[test]
fn thread_reads_a_post_and_its_replies_by_the_post_or_a_reply() {
    let lab = Lab::start();
    let channel = format!("{TEAM} > {OTHER}");
    for named in [POST, REPLY] {
        let answer = lab.json(&["thread", &channel, named]);
        assert_eq!(ids(&answer), [PLAIN_REPLY, REPLY, POST], "{answer}");
        assert_eq!(
            (&answer["ok"]["synced"], &answer["ok"]["end"]),
            (&json!(true), &json!("beginning"))
        );
    }
    let newest = lab.json(&["thread", &channel, POST, "--limit", "1"]);
    assert_eq!(ids(&newest), [PLAIN_REPLY], "{newest}");
    assert_eq!(newest["ok"].get("end"), None, "{newest}");
    let older = lab.json(&["thread", &channel, POST, "--before", PLAIN_REPLY]);
    assert_eq!(ids(&older), [REPLY, POST], "{older}");
    assert_eq!(
        (older["ok"].get("synced"), &older["ok"]["end"]),
        (None, &json!("beginning")),
        "a page back reads the store: {older}"
    );
    let past_the_post = lab.json(&["thread", &channel, REPLY, "--before", POST]);
    assert_eq!(
        (rows(&past_the_post).len(), &past_the_post["ok"]["end"]),
        (0, &json!("beginning"))
    );

    let (exit, _, err) = lab.ctl(&["thread", &channel, "1"]);
    assert_eq!(exit, 1);
    assert!(err.contains("bad_request: the chat holds no message 1"), "{err}");
}

#[test]
fn a_channel_named_where_a_chat_is_wanted_points_to_teams() {
    let lab = Lab::start();
    let (exit, _, err) = lab.ctl(&["members", OTHER]);
    assert_eq!(exit, 1);
    assert!(
        err.contains(&format!(
            "unknown_chat: {OTHER} is a channel, not a chat: `ngteamsctl teams` lists"
        )),
        "{err}"
    );
    let (exit, _, err) = lab.ctl(&["messages", "No such place"]);
    assert_eq!(exit, 1);
    assert!(
        err.contains("unknown_chat: no chat is No such place: `ngteamsctl chats` lists the chats, `ngteamsctl teams` the channels"),
        "{err}"
    );
    // both teams have a General channel
    let (exit, _, err) = lab.ctl(&["messages", "General"]);
    assert_eq!(exit, 1);
    assert!(err.contains("ambiguous_chat: 2 channels are called General"), "{err}");
    assert!(err.contains(&format!("{CHANNEL}  {TEAM} > General")), "{err}");
    let (exit, _, err) = lab.ctl(&["thread", teams_mock::AB_CHAT, "1"]);
    assert_eq!(exit, 1);
    assert!(
        err.contains("bad_request:") && err.contains("a thread is a channel's"),
        "{err}"
    );
}

#[test]
fn a_channel_whose_sync_fails_answers_the_stored_rows_unsynced() {
    let lab = Lab::start();
    lab.runtime.block_on(
        lab.mock
            .answer_next_containing("GET", &format!("{TEAM_CHANNEL}/messages"), 403, &json!({})),
    );
    let answer = lab.json(&["messages", TEAM_CHANNEL]);
    assert_eq!(
        (&answer["ok"]["synced"], &answer["ok"]["end"]),
        (&json!(false), &json!("store")),
        "{answer}"
    );
    assert!(
        rows(&answer).is_empty(),
        "the store holds nothing of the channel: {answer}"
    );
    let (exit, out, err) = lab.ctl(&["messages", TEAM_CHANNEL]);
    assert_eq!(exit, 0, "{err}");
    assert!(!out.contains("was not synced"), "the next read synced it: {out}");
}

#[test]
fn teams_says_at_once_when_their_read_failed() {
    let lab = Lab::start_with(|mock, runtime| {
        for _ in 0..3 {
            runtime.block_on(mock.refuse_next("GET", "/teams/users/me", 403));
        }
    });
    pump_until(&lab.cx, Duration::from_millis(10), |cx| {
        app::bridges(cx)[0].read(cx).models().chats.read(cx).teams_failed_at() > 0
    });
    let asked = Instant::now();
    let (exit, _, err) = lab.ctl(&["teams"]);
    assert_eq!(exit, 1);
    assert!(err.contains("engine: the teams can't be read right now"), "{err}");
    assert!(
        asked.elapsed() < Duration::from_secs(5),
        "answered at once, not at the timeout"
    );
    // before the teams are read, a thread's id that no stored chat has is no channel to point to
    let unknown = "19:no-such-chat@thread.v2";
    let (exit, _, err) = lab.ctl(&["members", unknown]);
    assert_eq!(exit, 1);
    assert!(err.contains(&format!("unknown_chat: no chat is {unknown}")), "{err}");
}

impl Lab {
    /// [`Self::ctl`] while the app's clock goes on by `step` at each turn: a read that waits on an app timer.
    fn ctl_ticking(&self, args: &[&str], step: Duration) -> (u8, String, String) {
        pumped_advancing(&self.cx, step, self.ctl_run(args))
    }

    /// The posts the mock took: a write that reached the service.
    fn posts_sent(&self) -> usize {
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with("/messages"))
            .count()
    }
}

/// The text of a post that the channel got after its first sync.
const LATE_POST: &str = "A made-up post after the first sync";

/// a's first read syncs the channel; then b posts to it: the store does not hold that post yet.
fn synced_then_posted(lab: &Lab) {
    let first = lab.json(&["messages", TEAM_CHANNEL]);
    assert_eq!(first["ok"]["synced"], true, "{first}");
    lab.mock
        .add_message(
            TEAM_CHANNEL,
            (LAB_B, "bot-b"),
            "RichText/Html",
            &format!("<p>{LATE_POST}</p>"),
        )
        .unwrap();
}

#[test]
fn a_failed_sync_of_a_synced_channel_shows_the_stored_rows_and_a_note() {
    let lab = Lab::start();
    synced_then_posted(&lab);
    lab.runtime.block_on(
        lab.mock
            .answer_next_containing("GET", &format!("{TEAM_CHANNEL}/messages"), 403, &json!({})),
    );
    let (exit, out, err) = lab.ctl(&["messages", &format!("{TEAM} > {OTHER}")]);
    assert_eq!(exit, 0, "{err}");
    assert!(out.contains("a made-up post to the channel (2 replies)"), "{out}");
    assert!(!out.contains(LATE_POST), "the store did not get the late post: {out}");
    assert!(
        out.contains("(the channel was not synced: these are the messages the store held)\n"),
        "{out}"
    );

    let answer = lab.json(&["messages", &format!("{TEAM} > {OTHER}")]);
    assert_eq!(answer["ok"]["synced"], true, "{answer}");
    let late = rows(&answer)
        .into_iter()
        .find(|row| row["text"].as_str().is_some_and(|text| text.contains(LATE_POST)))
        .unwrap_or_else(|| panic!("the next sync got the late post: {answer}"));
    assert_eq!(late["replies"], 0, "{answer}");
    assert!(late.get("post").is_none(), "{answer}");
}

#[test]
fn a_channel_sync_slower_than_its_bound_answers_the_stored_rows_and_goes_on() {
    let lab = Lab::start();
    synced_then_posted(&lab);
    lab.runtime.block_on(lab.mock.answer_next_after(
        "GET",
        &format!("{TEAM_CHANNEL}/messages"),
        Duration::from_secs(4),
    ));
    let asked = Instant::now();
    let (exit, out, err) = lab.ctl_ticking(&["--json", "messages", TEAM_CHANNEL], Duration::from_secs(1));
    assert_eq!(exit, 0, "{err}");
    let answer: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(answer["ok"]["synced"], false, "{answer}");
    assert!(
        asked.elapsed() < Duration::from_secs(4),
        "the app answered at its own bound, before the slow sync: {answer}"
    );
    assert!(!out.contains(LATE_POST), "{answer}");
    assert_eq!(row(&answer, POST)["replies"], 2, "the stored rows: {answer}");

    // the slow sync goes on in the engine and stores the late post; a store read starts no sync of its own
    let start = Instant::now();
    loop {
        let stored = lab.requests.messages(TEAM_CHANNEL, None, 1).blocking_wait().unwrap();
        if stored
            .messages
            .first()
            .is_some_and(|newest| newest.content.contains(LATE_POST))
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the slow sync stored no late post: {stored:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn thread_prints_a_post_and_its_replies_for_a_person() {
    let lab = Lab::start();
    for channel in [format!("{TEAM}/{OTHER}"), format!("{}>{}", TEAM.to_uppercase(), OTHER)] {
        let (exit, out, err) = lab.ctl(&["thread", &channel, REPLY]);
        assert_eq!(exit, 0, "{channel}: {err}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5, "a heading, 3 rows and a note: {out}");
        assert!(
            lines[1].contains(PLAIN_REPLY) && lines[1].contains(&format!("(reply to {POST})")),
            "{out}"
        );
        assert!(
            lines[2].contains(REPLY) && lines[2].contains(&format!("(reply to {POST}) a made-up reply")),
            "{out}"
        );
        assert!(lines[3].contains(POST) && lines[3].contains("(2 replies)"), "{out}");
        assert_eq!(lines[4], "(the first message: there is nothing older)", "{out}");
    }
    let (exit, out, err) = lab.ctl(&["thread", OTHER, SECOND_POST]);
    assert_eq!(exit, 0, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "a heading, the post and a note: {out}");
    assert!(lines[1].contains(SECOND_POST) && !lines[1].contains("repl"), "{out}");
    let (exit, _, err) = lab.ctl(&["thread", OTHER, "1760000999999"]);
    assert_eq!(exit, 1);
    assert!(
        err.contains("bad_request: the chat holds no message 1760000999999"),
        "{err}"
    );
}

#[test]
fn a_channel_named_to_send_or_to_a_chat_filter_points_to_teams_and_sends_nothing() {
    let lab = Lab::start();
    let before = lab.posts_sent();
    for name in [OTHER, &format!("{TEAM} > {OTHER}")] {
        let (exit, out, err) = lab.ctl(&["send", name, "a made-up text"]);
        assert_eq!((exit, out.as_str()), (1, ""), "{err}");
        assert!(
            err.contains("is a channel, not a chat: `ngteamsctl teams` lists"),
            "{name}: {err}"
        );
    }
    for name in ["General", "No such place"] {
        let (exit, _, err) = lab.ctl(&["send", name, "a made-up text"]);
        assert_eq!(exit, 1, "{err}");
        assert!(err.contains(&format!("unknown_chat: no chat is {name}")), "{err}");
        assert!(err.contains("`ngteamsctl teams` the channels"), "{err}");
    }
    assert_eq!(lab.posts_sent(), before, "nothing went to the service");

    for args in [
        vec!["events", "--chat", OTHER],
        vec!["wait", "message", "--chat", OTHER, "--timeout", "1"],
    ] {
        let (exit, _, err) = lab.ctl(&args);
        assert_eq!(exit, 1, "{args:?}: {err}");
        assert!(
            err.contains("is a channel, not a chat: `ngteamsctl teams` lists"),
            "{args:?}: {err}"
        );
    }
}

#[test]
fn a_chat_reads_as_before_beside_the_channels() {
    let lab = Lab::start_with(|mock, _| {
        mock.add_message(
            teams_mock::AB_CHAT,
            (LAB_B, "bot-b"),
            "RichText/Html",
            "<p>A made-up chat text.</p>",
        )
        .unwrap();
    });
    let answer = lab.json(&["messages", teams_mock::AB_CHAT]);
    assert!(
        answer["ok"].get("synced").is_none(),
        "a chat is not synced first: {answer}"
    );
    assert!(
        rows(&answer).iter().any(|row| row["text"] == "A made-up chat text."),
        "{answer}"
    );
    for message in rows(&answer) {
        for field in ["post", "replies", "subject", "email"] {
            assert!(message.get(field).is_none(), "{field} in a chat's row: {answer}");
        }
    }
    let (exit, out, err) = lab.ctl(&["messages", teams_mock::AB_CHAT]);
    assert_eq!(exit, 0, "{err}");
    assert!(!out.contains("(reply to") && !out.contains("not synced"), "{out}");
}
