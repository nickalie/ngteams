//! The `ngteamsctl` binary against an in-test fake server on a temporary socket: each subcommand's request and output,
//! `--json`, the socket each mode chooses, the exit codes, and `events` / `wait`.
//!
//! Every run of the binary names its socket (`$NGTEAMS_CTL_SOCKET`) in a temporary directory, with its runtime and
//! state dirs and `HOME` there too, so no run can reach an app of the user's.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::ffi::OsString;
use std::io::{BufReader, Read, Write as _};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
#[cfg(windows)]
use teams_ctl::transport::Listener as UnixListener;

use clap::Parser as _;
use serde_json::json;
use teams_ctl::ClientError;
use teams_ctl::cli::{Cli, Exit, ShellArg, answer_time, ask_failed, completions, run, socket};
use teams_ctl::client::Timeouts;
use teams_ctl::codec::{LineReader, decode_request};
use teams_ctl::path::Env;
use teams_ctl::protocol::{
    AccountRow, ActivityRow, AppStatus, CALENDAR_ANSWER, CALL_ANSWER, CalendarRow, CallRow, ChatRef, ChatRow, CtlError,
    CtlEvent, ErrorCode, EventFilter, EventLine, Hello, ManualStatus, MemberRow, MessageRow, MessageSource, OpenTarget,
    OutboxRow, OutboxState, Outcome, OwnStatus, PROTOCOL_VERSION, Request, RequestLine, Response, ResponseLine,
    SYNC_ANSWER, StatusChoice, SyncReport, Toggle, UiState, Unread, UnreadRow, View, WRITE_ANSWER, WRITE_WAIT,
    WindowOp, WindowState,
};

fn accept(listener: &UnixListener) -> teams_ctl::transport::Stream {
    #[cfg(unix)]
    {
        listener.accept().unwrap().0
    }
    #[cfg(windows)]
    loop {
        match listener.accept() {
            Ok(stream) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(5)),
            Err(error) => panic!("accept: {error}"),
        }
    }
}

/// Windows uses the system time zone even with TZ=UTC; compute the expected local display of our one fixture time.
fn expected_clock(text: &str) -> String {
    #[cfg(windows)]
    {
        let time = chrono::DateTime::from_timestamp_millis(1_700_000_000_000)
            .unwrap()
            .with_timezone(&chrono::Local);
        text.replace("2023-11-14 22:13", &time.format("%Y-%m-%d %H:%M").to_string())
            .replace("22:13:20", &time.format("%H:%M:%S").to_string())
    }
    #[cfg(unix)]
    {
        text.to_owned()
    }
}

/// What the fake server does with a request after the `hello`: lines to send back, then whether to read on.
type Handler = dyn FnMut(&RequestLine) -> (Vec<String>, bool) + Send;

fn ok(id: u64, response: Response) -> String {
    serde_json::to_string(&ResponseLine {
        id,
        outcome: Outcome::Ok(response),
    })
    .unwrap()
}

fn event(event: CtlEvent) -> String {
    serde_json::to_string(&EventLine {
        account: Some("a@lab.invalid".to_owned()),
        at: 1_700_000_000_000,
        event,
    })
    .unwrap()
}

/// A fake app on a socket in a fresh directory: one connection, its `hello` answered, then `handler`.
struct Server {
    dir: tempfile::TempDir,
    path: PathBuf,
    thread: Option<JoinHandle<Vec<RequestLine>>>,
}

impl Server {
    fn new(mut handler: Box<Handler>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctl.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let thread = thread::spawn(move || serve(&listener, &mut handler));
        Self {
            dir,
            path,
            thread: Some(thread),
        }
    }

    /// A server answering the one request after `hello` with `response`.
    fn answering(response: Response) -> Self {
        Self::new(Box::new(move |request| (vec![ok(request.id, response.clone())], false)))
    }

    /// The requests the server read, the `hello` left out.
    fn requests(mut self) -> Vec<RequestLine> {
        let seen = self.thread.take().unwrap().join().unwrap();
        assert!(matches!(
            seen.first().map(|line| &line.request),
            Some(Request::Hello { .. })
        ));
        seen.into_iter().skip(1).collect()
    }
}

/// Answer one connection on `listener`: its `hello`, then `handler`; the requests read.
fn serve(listener: &UnixListener, handler: &mut Handler) -> Vec<RequestLine> {
    let stream = accept(listener);
    let mut writer = stream.try_clone().unwrap();
    let mut lines = LineReader::new(BufReader::new(stream));
    let mut seen = Vec::new();
    while let Ok(Some(line)) = lines.read_line() {
        let request = decode_request(&line).unwrap();
        let (answers, go_on) = match request.request {
            Request::Hello { .. } => (vec![hello(request.id)], true),
            _ => handler(&request),
        };
        seen.push(request);
        let text = answers.iter().fold(String::new(), |text, answer| text + answer + "\n");
        // the client may be gone already (a wait that matched)
        if writer.write_all(text.as_bytes()).is_err() || !go_on {
            break;
        }
    }
    seen
}

fn hello(id: u64) -> String {
    ok(
        id,
        Response::Hello(Hello {
            protocol: PROTOCOL_VERSION,
            app: "9.9.9".to_owned(),
        }),
    )
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run `ngteamsctl args` against the socket at `socket`, everything else of its environment in `home`; killed past
/// ten seconds.
fn ctl(home: &Path, socket: &Path, args: &[&str]) -> Run {
    ctl_fed(home, socket, args, None)
}

/// [`ctl`], `input` (when given) on its stdin.
fn ctl_fed(home: &Path, socket: &Path, args: &[&str], input: Option<&str>) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ngteamsctl"))
        .env_clear()
        // a coverage run counts the binary's runs too
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env("NGTEAMS_CTL_SOCKET", socket)
        .env("XDG_RUNTIME_DIR", home)
        .env("XDG_STATE_HOME", home)
        .env("HOME", home)
        .env("TZ", "UTC")
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        // dropped once written: its end of file
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    }
    // read as it comes, so an output past the pipe's buffer (a completion script) doesn't hold the binary up
    let stdout = drain(child.stdout.take().unwrap());
    let stderr = drain(child.stderr.take().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("ngteamsctl {args:?} didn't end");
        }
        thread::sleep(Duration::from_millis(10));
    };
    Run {
        code: status.code().unwrap(),
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// Read `pipe` to its end on a thread of its own.
fn drain(mut pipe: impl Read + Send + 'static) -> JoinHandle<String> {
    thread::spawn(move || {
        let mut text = String::new();
        pipe.read_to_string(&mut text).unwrap();
        text
    })
}

/// Run `args` against `server` and hand back the run and the one request it asked.
fn run_one(server: Server, args: &[&str]) -> (Run, RequestLine) {
    let run = ctl(server.dir.path(), &server.path, args);
    let mut requests = server.requests();
    assert_eq!(requests.len(), 1, "{args:?}: {requests:?}");
    (run, requests.remove(0))
}

fn message(id: &str, text: &str) -> MessageRow {
    MessageRow {
        id: id.to_owned(),
        chat: "19:lab@thread.v2".to_owned(),
        sender: Some("8:orgid:b".to_owned()),
        sender_name: Some("Lab B".to_owned()),
        at: 1_700_000_000_000,
        text: text.to_owned(),
        edited: true,
        deleted: false,
        from_me: false,
        mentions_me: true,
    }
}

/// One case of the table below: the arguments, the request they ask, the answer, and lines the output shows.
struct Case {
    args: &'static [&'static str],
    account: Option<&'static str>,
    request: Request,
    response: Response,
    shows: &'static [&'static str],
}

fn s(text: &str) -> String {
    text.to_owned()
}

fn read_cases() -> Vec<Case> {
    let accounts = vec![AccountRow {
        index: 0,
        upn: s("a@lab.invalid"),
        mri: s("8:orgid:a"),
        name: Some(s("Lab A")),
        shown: true,
        state: s("connected"),
    }];
    vec![
        Case {
            args: &["status"],
            account: None,
            request: Request::Status,
            response: Response::Status(AppStatus {
                app: s("9.9.9"),
                accounts: accounts.clone(),
                window: WindowState::Tray,
                call: None,
            }),
            shows: &["ngteams 9.9.9  window: tray", "0*  a@lab.invalid  connected  Lab A"],
        },
        Case {
            args: &["accounts"],
            account: None,
            request: Request::Accounts,
            response: Response::Accounts { accounts },
            shows: &[
                "#   ACCOUNT        STATE      NAME",
                "0*  a@lab.invalid  connected  Lab A",
            ],
        },
        Case {
            args: &["--account", "1", "chats", "--unread", "--limit", "5"],
            account: Some("1"),
            request: Request::Chats {
                unread_only: true,
                limit: Some(5),
            },
            response: Response::Chats {
                chats: vec![ChatRow {
                    id: s("19:lab@thread.v2"),
                    title: s("Lab chat"),
                    chat_type: s("group"),
                    unread: 3,
                    last_activity: Some(1_700_000_000_000),
                    preview: Some(s("hi")),
                }],
            },
            shows: &[
                "ID                UNREAD  LAST              TYPE   TITLE",
                "19:lab@thread.v2  3       2023-11-14 22:13  group  Lab chat",
            ],
        },
        Case {
            args: &["messages", "Lab chat", "--limit", "2", "--before", "9"],
            account: None,
            request: Request::Messages {
                chat: s("Lab chat"),
                limit: Some(2),
                before: Some(s("9")),
            },
            response: Response::Messages {
                messages: vec![message("7", "two\nlines")],
            },
            shows: &[
                "ID  TIME              FROM   TEXT",
                "7   2023-11-14 22:13  Lab B  (edited) (@me) two ↵ lines",
            ],
        },
    ]
}

fn list_cases() -> Vec<Case> {
    vec![
        Case {
            args: &["unread"],
            account: None,
            request: Request::Unread,
            response: Response::Unread(Unread {
                total: 3,
                chats: vec![UnreadRow {
                    chat: s("19:lab@thread.v2"),
                    title: s("Lab chat"),
                    count: 3,
                }],
            }),
            shows: &["total: 3", "3      19:lab@thread.v2  Lab chat"],
        },
        Case {
            args: &["search", "budget", "report", "--limit", "1"],
            account: None,
            request: Request::Search {
                query: s("budget report"),
                limit: Some(1),
            },
            response: Response::Search {
                messages: vec![message("7", "the budget report")],
            },
            shows: &["7   19:lab@thread.v2  2023-11-14 22:13  Lab B  (edited) (@me) the budget report"],
        },
        Case {
            args: &["members", "19:lab@thread.v2"],
            account: None,
            request: Request::Members {
                chat: s("19:lab@thread.v2"),
            },
            response: Response::Members {
                members: vec![MemberRow {
                    mri: s("8:orgid:b"),
                    name: Some(s("Lab B")),
                    role: Some(s("Admin")),
                }],
            },
            shows: &["MRI        ROLE   NAME", "8:orgid:b  Admin  Lab B"],
        },
    ]
}

fn activity_cases() -> Vec<Case> {
    vec![
        Case {
            args: &["activity", "--filter", "unread", "--search", "lab b", "--limit", "4"],
            account: None,
            request: Request::Activity {
                filter: Some(s("unread")),
                search: Some(s("lab b")),
                limit: Some(4),
            },
            response: Response::Activity {
                items: vec![
                    ActivityRow {
                        id: s("7"),
                        kind: s("reaction"),
                        title: s("Lab B +1 reacted to your message"),
                        preview: Some(s("two\nlines")),
                        place: Some(s("In chat with you")),
                        chat: Some(s("19:lab@unq.gbl.spaces")),
                        message: Some(s("6")),
                        at: 1_700_000_000_000,
                        read: false,
                    },
                    ActivityRow {
                        id: s("5"),
                        kind: s("missed-call"),
                        title: s("Missed call from Lab B"),
                        preview: None,
                        place: None,
                        chat: None,
                        message: None,
                        at: 1_700_000_000_000,
                        read: true,
                    },
                ],
            },
            shows: &[
                "ID  STATE   TIME              KIND         TITLE                             PLACE             PREVIEW",
                "7   unread  2023-11-14 22:13  reaction     Lab B +1 reacted to your message  In chat with you  two ↵ lines",
                "5           2023-11-14 22:13  missed-call  Missed call from Lab B",
            ],
        },
        Case {
            args: &["activity"],
            account: None,
            request: Request::Activity {
                filter: None,
                search: None,
                limit: None,
            },
            response: Response::Activity { items: Vec::new() },
            shows: &["ID  STATE  TIME  KIND  TITLE  PLACE  PREVIEW"],
        },
        Case {
            args: &["ui", "state"],
            account: None,
            request: Request::UiState,
            response: Response::UiState(UiState {
                shown: Some(s("a@lab.invalid")),
                view: View::Chat,
                chat: Some(s("19:lab@thread.v2")),
                chat_title: Some(s("Lab chat")),
                window: WindowState::Shown,
                call_window: false,
                activity: true,
            }),
            shows: &[
                "account:     a@lab.invalid",
                "view:        chat",
                "list:        activity",
                "chat:        19:lab@thread.v2",
                "title:       Lab chat",
                "window:      shown",
                "call window: closed",
            ],
        },
    ]
}

fn ui_cases() -> Vec<Case> {
    let open = |target| Request::Open { target };
    let done = |args, request| Case {
        args,
        account: None,
        request,
        response: Response::Done,
        shows: &[],
    };
    vec![
        done(&["window", "hide"], Request::Window { op: WindowOp::Hide }),
        done(&["window", "show"], Request::Window { op: WindowOp::Show }),
        done(&["window", "toggle"], Request::Window { op: WindowOp::Toggle }),
        done(&["window", "focus"], Request::Window { op: WindowOp::Focus }),
        done(
            &["open", "chat", "Lab chat"],
            open(OpenTarget::Chat { chat: s("Lab chat") }),
        ),
        done(
            &["open", "channel", "General"],
            open(OpenTarget::Channel { channel: s("General") }),
        ),
        done(&["open", "calendar"], open(OpenTarget::Calendar)),
        done(&["open", "activity"], open(OpenTarget::Activity)),
        done(&["open", "settings"], open(OpenTarget::Settings { page: None })),
        done(
            &["open", "settings", "appearance"],
            open(OpenTarget::Settings {
                page: Some(s("appearance")),
            }),
        ),
        done(
            &["open", "search", "q1", "q2"],
            open(OpenTarget::Search { query: s("q1 q2") }),
        ),
        done(
            &["account", "switch", "b@lab.invalid"],
            Request::SwitchAccount { to: s("b@lab.invalid") },
        ),
        done(&["activate"], Request::Activate),
        Case {
            args: &["action", "list"],
            account: None,
            request: Request::ActionList,
            response: Response::Actions {
                actions: vec![s("teams_ui::ShowCalendar"), s("teams_ui::FocusSearch")],
            },
            shows: &["teams_ui::ShowCalendar", "teams_ui::FocusSearch"],
        },
        Case {
            args: &["action", "ShowCalendar"],
            account: None,
            request: Request::Action {
                name: s("ShowCalendar"),
            },
            response: Response::Dispatched {
                action: s("teams_ui::ShowCalendar"),
            },
            shows: &["dispatched teams_ui::ShowCalendar"],
        },
    ]
}

const LAB_CHAT: &str = "19:lab@thread.v2";

fn outbox(id: i64, write: &str, state: OutboxState) -> Response {
    Response::Outbox(OutboxRow {
        id,
        write: s(write),
        chat: Some(s(LAB_CHAT)),
        state,
        message: Some(s("1700000000000")),
        error: None,
    })
}

fn message_write_cases() -> Vec<Case> {
    let chat = || s("Lab chat");
    let message = || s("1700000000000");
    vec![
        Case {
            args: &["send", "Lab chat", "one\ntwo"],
            account: None,
            request: Request::Send {
                chat: chat(),
                text: s("one\ntwo"),
                wait: false,
            },
            response: outbox(3, "send", OutboxState::Sent),
            shows: &["#3 send 19:lab@thread.v2 sent 1700000000000"],
        },
        Case {
            args: &["--account", "1", "edit", "Lab chat", "1700000000000", "fixed", "--wait"],
            account: Some("1"),
            request: Request::Edit {
                chat: chat(),
                message: message(),
                text: s("fixed"),
                wait: true,
            },
            response: outbox(4, "edit", OutboxState::Sent),
            shows: &["#4 edit 19:lab@thread.v2 sent 1700000000000"],
        },
        Case {
            args: &["delete", "Lab chat", "1700000000000"],
            account: None,
            request: Request::Delete {
                chat: chat(),
                message: message(),
                wait: false,
            },
            response: outbox(5, "delete", OutboxState::Queued),
            shows: &["#5 delete 19:lab@thread.v2 queued 1700000000000"],
        },
        Case {
            args: &["react", "Lab chat", "1700000000000", "like"],
            account: None,
            request: Request::React {
                chat: chat(),
                message: message(),
                reaction: s("like"),
                wait: false,
            },
            response: outbox(6, "add_reaction", OutboxState::Unknown),
            shows: &["#6 add_reaction 19:lab@thread.v2 unknown 1700000000000"],
        },
        Case {
            args: &["unreact", "Lab chat", "1700000000000", "like", "--wait"],
            account: None,
            request: Request::Unreact {
                chat: chat(),
                message: message(),
                reaction: s("like"),
                wait: true,
            },
            response: outbox(7, "remove_reaction", OutboxState::Sent),
            shows: &["#7 remove_reaction 19:lab@thread.v2 sent 1700000000000"],
        },
    ]
}

fn chat_write_cases() -> Vec<Case> {
    vec![
        Case {
            args: &["read", "Lab chat"],
            account: None,
            request: Request::MarkRead {
                chat: s("Lab chat"),
                wait: false,
            },
            response: Response::Done,
            shows: &[],
        },
        Case {
            args: &["new-chat", "b@lab.invalid", "8:orgid:c"],
            account: None,
            request: Request::NewChat {
                people: vec![s("b@lab.invalid"), s("8:orgid:c")],
                wait: false,
            },
            response: outbox(8, "create_chat", OutboxState::Sent),
            shows: &["#8 create_chat 19:lab@thread.v2 sent 1700000000000"],
        },
        Case {
            args: &["resend", "8", "--wait"],
            account: None,
            request: Request::Resend { row: 8, wait: true },
            response: outbox(8, "send", OutboxState::Sent),
            shows: &["#8 send 19:lab@thread.v2 sent 1700000000000"],
        },
    ]
}

fn own_status(reported: &str, manual: ManualStatus) -> Response {
    Response::OwnStatus(OwnStatus {
        reported: s(reported),
        manual,
    })
}

fn status_cases() -> Vec<Case> {
    let set = |status| Request::SetStatus { status };
    vec![
        Case {
            args: &["status", "get"],
            account: None,
            request: Request::GetStatus,
            response: own_status("available", ManualStatus::Automatic),
            shows: &["reported: available", "manual:   automatic"],
        },
        Case {
            args: &["status", "set", "available"],
            account: None,
            request: set(StatusChoice::Available),
            response: own_status("available", ManualStatus::Available),
            shows: &["manual:   available"],
        },
        Case {
            args: &["status", "set", "busy"],
            account: None,
            request: set(StatusChoice::Busy),
            response: own_status("busy", ManualStatus::Busy),
            shows: &["reported: busy"],
        },
        Case {
            args: &["--account", "b@lab.invalid", "status", "set", "dnd"],
            account: Some("b@lab.invalid"),
            request: set(StatusChoice::Dnd),
            response: own_status("do-not-disturb", ManualStatus::Dnd),
            shows: &["reported: do-not-disturb", "manual:   dnd"],
        },
        Case {
            args: &["status", "set", "away"],
            account: None,
            request: set(StatusChoice::Away),
            response: own_status("away", ManualStatus::Away),
            shows: &["reported: away"],
        },
        Case {
            args: &["status", "set", "reset"],
            account: None,
            request: set(StatusChoice::Reset),
            response: own_status("available", ManualStatus::Automatic),
            shows: &["manual:   automatic"],
        },
        Case {
            args: &["sync"],
            account: None,
            request: Request::Sync,
            response: Response::Synced(SyncReport {
                conversations: 3,
                messages: 12,
                failed: 0,
                unreadable: 0,
                caught_up: true,
            }),
            shows: &["conversations: 3", "messages:      12", "caught up:     yes"],
        },
    ]
}

fn call_row(state: &str, ringing: bool, muted: bool, camera: bool) -> CallRow {
    CallRow {
        id: s("7"),
        account: s("a@lab.invalid"),
        chat: Some(s(LAB_CHAT)),
        state: s(state),
        ringing,
        caller: ringing.then(|| s("8:orgid:b")),
        muted,
        camera,
    }
}

fn call_cases() -> Vec<Case> {
    let call = |state, muted, camera| Response::Call(call_row(state, false, muted, camera));
    vec![
        Case {
            args: &["calls"],
            account: None,
            request: Request::Calls,
            response: Response::Calls {
                calls: vec![call_row("ringing", true, false, false)],
            },
            shows: &["7   a@lab.invalid  19:lab@thread.v2  ringing (ringing from 8:orgid:b)"],
        },
        Case {
            args: &["call", "Lab chat"],
            account: None,
            request: Request::Call { chat: s("Lab chat") },
            response: call("placing", false, false),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  placing"],
        },
        Case {
            args: &["--account", "1", "answer"],
            account: Some("1"),
            request: Request::Answer,
            response: call("connecting", false, false),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  connecting"],
        },
        Case {
            args: &["decline"],
            account: None,
            request: Request::Decline,
            response: call("ended", false, false),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  ended"],
        },
        Case {
            args: &["hangup"],
            account: None,
            request: Request::HangUp,
            response: Response::Done,
            shows: &[],
        },
        Case {
            args: &["join", "Lab group"],
            account: None,
            request: Request::Join { chat: s("Lab group") },
            response: call("joining", false, false),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  joining"],
        },
        Case {
            args: &["mute", "on"],
            account: None,
            request: Request::Mute { op: Toggle::On },
            response: call("connected", true, false),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  connected muted"],
        },
        Case {
            args: &["mute", "toggle"],
            account: None,
            request: Request::Mute { op: Toggle::Toggle },
            response: call("connected", false, false),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  connected"],
        },
        Case {
            args: &["camera", "off"],
            account: None,
            request: Request::Camera { op: Toggle::Off },
            response: call("connected", false, false),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  connected"],
        },
        Case {
            args: &["camera", "on"],
            account: None,
            request: Request::Camera { op: Toggle::On },
            response: call("connected", false, true),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  connected camera"],
        },
    ]
}

fn calendar_row(id: &str, subject: &str, joinable: bool) -> CalendarRow {
    CalendarRow {
        id: s(id),
        subject: s(subject),
        start: 1_793_000_000_000,
        end: 1_793_001_800_000,
        all_day: false,
        cancelled: false,
        online: joinable,
        joinable,
        location: joinable.then(|| s("Microsoft Teams Meeting")),
        organizer: Some(s("Avery Example")),
    }
}

/// The calendar commands: each one's request, and what its answer shows (its lines hold these).
fn calendar_cases() -> Vec<Case> {
    let listed = || Response::Calendar {
        events: vec![
            calendar_row("AAMk-sync", "Weekly sync", true),
            calendar_row("AAMk-day", "Release day", false),
        ],
    };
    let today = Request::Calendar { from: None, to: None };
    let week = Request::Calendar {
        from: Some(s("2026-11-02")),
        to: Some(s("2026-11-08")),
    };
    vec![
        Case {
            args: &["calendar"],
            account: None,
            request: today.clone(),
            response: listed(),
            shows: &["AAMk-sync", "online,joinable  Weekly sync", "Release day"],
        },
        Case {
            args: &["calendar", "--today"],
            account: None,
            request: today,
            response: listed(),
            shows: &["Weekly sync"],
        },
        Case {
            args: &[
                "--account",
                "1",
                "calendar",
                "--from",
                "2026-11-02",
                "--to",
                "2026-11-08",
            ],
            account: Some("1"),
            request: week,
            response: listed(),
            shows: &["Release day"],
        },
        Case {
            args: &["calendar", "--from", "2026-11-02"],
            account: None,
            request: Request::Calendar {
                from: Some(s("2026-11-02")),
                to: None,
            },
            response: Response::Calendar { events: vec![] },
            shows: &["ID", "START"],
        },
        Case {
            args: &["calendar", "show", "AAMk-sync"],
            account: None,
            request: Request::CalendarEvent { event: s("AAMk-sync") },
            response: Response::CalendarEvent(calendar_row("AAMk-sync", "Weekly sync", true)),
            shows: &[
                "subject:   Weekly sync",
                "location:  Microsoft Teams Meeting",
                "organizer: Avery Example",
            ],
        },
    ]
}

/// `join meeting`, with and without `--now`.
fn join_meeting_cases() -> Vec<Case> {
    let join = |now| Request::JoinMeeting {
        event: s("AAMk-sync"),
        now,
    };
    vec![
        Case {
            args: &["join", "meeting", "AAMk-sync"],
            account: None,
            request: join(false),
            response: Response::Done,
            shows: &[],
        },
        Case {
            args: &["--account", "a@lab.invalid", "join", "meeting", "AAMk-sync", "--now"],
            account: Some("a@lab.invalid"),
            request: join(true),
            response: Response::Call(call_row("connecting", false, false, false)),
            shows: &["7   a@lab.invalid  19:lab@thread.v2  connecting"],
        },
    ]
}

#[test]
fn each_calendar_command_asks_its_request_and_prints_its_answer() {
    for case in calendar_cases().into_iter().chain(join_meeting_cases()) {
        let args = case.args;
        let (run, request) = run_one(Server::answering(case.response), args);
        assert_eq!(run.code, 0, "{args:?}: {}", run.stderr);
        assert_eq!(request.request, case.request, "{args:?}");
        assert_eq!(request.account.as_deref(), case.account, "{args:?}");
        for shown in case.shows {
            assert!(
                run.stdout.contains(&expected_clock(shown)),
                "{args:?}: {shown:?} not in\n{}",
                run.stdout
            );
        }
        if case.shows.is_empty() {
            assert_eq!(run.stdout, "", "{args:?}");
        }
    }
}

#[test]
fn a_calendar_date_that_does_not_read_or_a_join_without_a_chat_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        &["calendar", "--from", "2026-13-01"][..],
        &["calendar", "--to", "tomorrow"],
        &["calendar", "--today", "--from", "2026-11-02"],
        &["calendar", "show"],
        &["join"],
        &["join", "meeting"],
    ] {
        let run = ctl(home.path(), &home.path().join("nobody.sock"), args);
        assert_eq!(run.code, 2, "{args:?}: {}", run.stderr);
    }
}

#[test]
fn a_meeting_s_join_is_waited_for_as_a_call_command_and_a_calendar_read_past_its_read() {
    let parsed = |args: &[&str]| Cli::try_parse_from([&["ngteamsctl"][..], args].concat()).unwrap();
    let default = Timeouts::default().read;
    for args in [&["join", "meeting", "AAMk"][..], &["join", "meeting", "AAMk", "--now"]] {
        assert_eq!(answer_time(&parsed(args).command), CALL_ANSWER + default, "{args:?}");
    }
    for args in [&["calendar"][..], &["calendar", "show", "AAMk"]] {
        assert_eq!(
            answer_time(&parsed(args).command),
            CALENDAR_ANSWER + default,
            "{args:?}"
        );
    }
}

#[test]
fn an_event_without_a_meeting_exits_1_with_its_code() {
    let server = Server::new(Box::new(|request: &RequestLine| {
        (
            vec![error_line(
                request.id,
                ErrorCode::NoMeeting,
                "Release day isn't a Teams meeting the app can join",
            )],
            true,
        )
    }));
    let run = ctl(
        server.dir.path(),
        &server.path,
        &["join", "meeting", "AAMk-day", "--now"],
    );
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stderr.contains("no_meeting: Release day"), "{}", run.stderr);
}

#[test]
fn a_switch_other_than_on_off_or_toggle_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    for args in [&["mute", "loud"][..], &["camera"]] {
        let run = ctl(home.path(), &home.path().join("nobody.sock"), args);
        assert_eq!(run.code, 2, "{args:?}: {}", run.stderr);
    }
}

#[test]
fn a_call_command_s_answer_is_waited_for_past_the_app_s_own_bound_on_it() {
    let parsed = |args: &[&str]| Cli::try_parse_from([&["ngteamsctl"][..], args].concat()).unwrap();
    let default = Timeouts::default().read;
    for args in [
        &["call", "Lab chat"][..],
        &["answer"],
        &["decline"],
        &["hangup"],
        &["join", "Lab group"],
        &["mute", "on"],
        &["camera", "toggle"],
    ] {
        assert_eq!(answer_time(&parsed(args).command), CALL_ANSWER + default, "{args:?}");
    }
    assert_eq!(answer_time(&parsed(&["calls"]).command), default);
}

#[test]
fn a_refused_call_command_exits_1_with_its_code() {
    let server = Server::new(Box::new(|request: &RequestLine| {
        (
            vec![error_line(request.id, ErrorCode::NotRinging, "no call rings")],
            true,
        )
    }));
    let run = ctl(server.dir.path(), &server.path, &["answer"]);
    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stderr.contains("not_ringing: no call rings"), "{}", run.stderr);
}

#[test]
fn a_status_choice_the_menu_doesn_t_have_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let run = ctl(
        home.path(),
        &home.path().join("nobody.sock"),
        &["status", "set", "offline"],
    );
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(run.stderr.contains("offline"), "{}", run.stderr);
}

#[test]
fn send_reads_its_text_from_stdin_with_a_dash() {
    let server = Server::answering(outbox(3, "send", OutboxState::Sent));
    let run = ctl_fed(
        server.dir.path(),
        &server.path,
        &["send", "Lab chat", "-"],
        Some("piped\nlines\n"),
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    let requests = server.requests();
    assert_eq!(
        requests.iter().map(|line| &line.request).collect::<Vec<_>>(),
        [&Request::Send {
            chat: s("Lab chat"),
            text: s("piped\nlines\n"),
            wait: false,
        }]
    );
}

#[test]
fn a_write_whose_row_failed_or_waited_unsent_exits_1() {
    for (args, state, code) in [
        (&["send", "Lab chat", "hi"][..], OutboxState::Failed, 1),
        (&["send", "Lab chat", "hi", "--json"][..], OutboxState::Failed, 1),
        (&["send", "Lab chat", "hi"][..], OutboxState::Queued, 0),
        (&["send", "Lab chat", "hi", "--wait"][..], OutboxState::Unknown, 1),
        (
            &["send", "Lab chat", "hi", "--wait", "--json"][..],
            OutboxState::Unknown,
            1,
        ),
        (&["send", "Lab chat", "hi", "--wait"][..], OutboxState::Sent, 0),
    ] {
        let (run, _) = run_one(Server::answering(outbox(3, "send", state)), args);
        assert_eq!(run.code, code, "{args:?} {state:?}: {}", run.stderr);
        assert!(run.stdout.contains("send"), "the row is printed: {}", run.stdout);
    }
}

#[test]
fn a_write_s_answer_is_waited_for_past_the_app_s_own_bound_on_it() {
    let parsed = |args: &[&str]| Cli::try_parse_from([&["ngteamsctl"][..], args].concat()).unwrap();
    let default = Timeouts::default().read;
    for args in [
        &["send", "Lab chat", "hi"][..],
        &["edit", "Lab chat", "1", "hi"],
        &["read", "Lab chat"],
        &["new-chat", "8:orgid:b"],
        &["resend", "3"],
    ] {
        let waited = answer_time(&parsed(args).command);
        assert!(waited > WRITE_ANSWER, "{args:?}: {waited:?}");
        assert!(waited <= WRITE_ANSWER + default, "{args:?}: {waited:?}");
        let with_wait = answer_time(&parsed(&[args, &["--wait"]].concat()).command);
        assert!(with_wait > WRITE_ANSWER + WRITE_WAIT, "{args:?}: {with_wait:?}");
    }
    assert_eq!(answer_time(&parsed(&["chats"]).command), default);
    assert_eq!(answer_time(&parsed(&["status", "set", "busy"]).command), default);
    assert_eq!(answer_time(&parsed(&["sync"]).command), SYNC_ANSWER + default);
}

#[test]
fn a_write_left_unanswered_says_it_may_still_go_out_and_where_to_look() {
    let told = |error: &ClientError, write: bool| {
        let mut err = Vec::new();
        let exit = ask_failed(&mut err, error, write);
        (exit, String::from_utf8(err).unwrap())
    };
    let (exit, write) = told(&ClientError::Timeout, true);
    assert_eq!(exit, Exit::Error);
    assert!(write.contains("may still go out"), "{write}");
    assert!(write.contains("--kind outbox"), "{write}");
    // a read that timed out wrote nothing; a write the app closed on or refused is told as any other
    for (error, write) in [
        (ClientError::Timeout, false),
        (ClientError::Closed, true),
        (
            ClientError::Server(CtlError::new(ErrorCode::Engine, "the engine stopped")),
            true,
        ),
    ] {
        let (exit, said) = told(&error, write);
        assert_eq!(exit, Exit::Error);
        assert!(said.contains(&error.to_string()), "{said}");
        assert!(!said.contains("may still go out"), "{said}");
    }
}

#[test]
fn each_command_asks_its_request_and_prints_its_answer() {
    for case in read_cases()
        .into_iter()
        .chain(list_cases())
        .chain(activity_cases())
        .chain(ui_cases())
        .chain(message_write_cases())
        .chain(chat_write_cases())
        .chain(status_cases())
        .chain(call_cases())
    {
        let (run, request) = run_one(Server::answering(case.response), case.args);
        assert_eq!(run.code, 0, "{:?}: {}", case.args, run.stderr);
        assert_eq!(request.request, case.request, "{:?}", case.args);
        assert_eq!(request.account.as_deref(), case.account, "{:?}", case.args);
        let lines: Vec<&str> = run.stdout.lines().collect();
        for shown in case.shows {
            assert!(
                lines.contains(&expected_clock(shown).as_str()),
                "{:?}: {shown:?} not in\n{}",
                case.args,
                run.stdout
            );
        }
        if case.shows.is_empty() {
            assert_eq!(run.stdout, "", "{:?}", case.args);
        }
    }
}

#[test]
fn json_prints_the_answer_line_as_it_came() {
    let response = Response::Unread(Unread {
        total: 1,
        chats: vec![UnreadRow {
            chat: s("19:lab@thread.v2"),
            title: s("Lab chat"),
            count: 1,
        }],
    });
    let (run, request) = run_one(Server::answering(response.clone()), &["unread", "--json"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("{}\n", ok(request.id, response)));

    let error = Server::new(Box::new(|request| {
        let line = ResponseLine {
            id: request.id,
            outcome: Outcome::Error(CtlError::new(ErrorCode::NoTray, "no tray item")),
        };
        (vec![serde_json::to_string(&line).unwrap()], false)
    }));
    let (run, _) = run_one(error, &["--json", "window", "hide"]);
    assert_eq!(run.code, 1);
    let line: serde_json::Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(line["error"], json!({"code": "no_tray", "message": "no tray item"}));
}

#[test]
fn an_error_answer_exits_1_and_tells_the_candidates() {
    let server = Server::new(Box::new(|request| {
        let error = CtlError {
            candidates: vec![
                ChatRef {
                    id: s("19:a@thread.v2"),
                    title: s("Lab"),
                },
                ChatRef {
                    id: s("19:b@thread.v2"),
                    title: s("Lab"),
                },
            ],
            ..CtlError::new(ErrorCode::AmbiguousChat, "2 chats are called Lab")
        };
        let line = ResponseLine {
            id: request.id,
            outcome: Outcome::Error(error),
        };
        (vec![serde_json::to_string(&line).unwrap()], false)
    }));
    let (run, _) = run_one(server, &["messages", "Lab"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        "ngteamsctl: ambiguous_chat: 2 chats are called Lab\n  19:a@thread.v2  Lab\n  19:b@thread.v2  Lab\n"
    );
}

#[test]
fn no_app_exits_2() {
    let home = tempfile::tempdir().unwrap();
    let run = ctl(home.path(), &home.path().join("nobody.sock"), &["status"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("no ngteams is running on"), "{}", run.stderr);
}

#[test]
fn a_usage_error_exits_2() {
    let home = tempfile::tempdir().unwrap();
    let nobody = home.path().join("nobody.sock");
    for args in [
        &["frobnicate"][..],
        &["window", "sideways"],
        &["action"],
        &["--lab", "a", "--read-only", "status"],
    ] {
        let run = ctl(home.path(), &nobody, args);
        assert_eq!(run.code, 2, "{args:?}");
        assert!(run.stderr.starts_with("error:"), "{args:?}: {}", run.stderr);
    }
    let run = ctl(home.path(), &nobody, &["--help"]);
    assert_eq!(run.code, 0);
    assert!(run.stdout.contains("events"));
}

#[test]
fn events_kinds_lists_every_kind_without_asking_the_app() {
    let home = tempfile::tempdir().unwrap();
    let run = ctl(home.path(), &home.path().join("nobody.sock"), &["events", "kinds"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let kinds: Vec<&str> = run.stdout.lines().collect();
    let expected: Vec<&str> = CtlEvent::KINDS
        .iter()
        .copied()
        .filter(|kind| *kind != "lagged")
        .collect();
    assert_eq!(kinds, expected);
}

#[test]
fn completions_print_each_shell_s_script_naming_the_subcommands_without_asking_the_app() {
    let home = tempfile::tempdir().unwrap();
    let nobody = home.path().join("nobody.sock");
    for (shell, marker) in [
        ("bash", "complete -F _ngteamsctl"),
        ("fish", "complete -c ngteamsctl"),
        ("zsh", "#compdef ngteamsctl"),
    ] {
        let run = ctl(home.path(), &nobody, &["completions", shell]);
        assert_eq!(run.code, 0, "{shell}: {}", run.stderr);
        assert!(run.stdout.contains(marker), "{shell}: {}", run.stdout);
        for subcommand in [
            "status",
            "chats",
            "messages",
            "send",
            "new-chat",
            "call",
            "hangup",
            "calendar",
            "events",
            "wait",
            "completions",
        ] {
            assert!(run.stdout.contains(subcommand), "{shell} lacks {subcommand}");
        }
    }
    assert!(!nobody.exists());
    let run = ctl(home.path(), &nobody, &["completions", "tcsh"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.starts_with("error:"), "{}", run.stderr);
}

#[test]
fn completions_into_a_writer_that_fails_is_an_error() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(completions(ShellArg::Zsh, &mut Broken).is_err());
    let mut script = Vec::new();
    completions(ShellArg::Fish, &mut script).unwrap();
    assert!(String::from_utf8(script).unwrap().contains("complete -c ngteamsctl"));
}

fn typing(on: bool) -> CtlEvent {
    CtlEvent::Typing {
        chat: s("19:lab@thread.v2"),
        who: Some(s("8:orgid:b")),
        on,
    }
}

/// A server answering the subscribe with `events` and then closing the stream, or holding it open (`hold`).
fn streaming(events: Vec<String>, hold: bool) -> Server {
    Server::new(Box::new(move |request| {
        let mut lines = vec![ok(request.id, Response::Subscribed)];
        lines.extend(events.iter().cloned());
        (lines, hold)
    }))
}

#[test]
fn events_follow_prints_each_event_until_the_app_closes_the_stream() {
    let server = streaming(
        vec![event(typing(true)), event(typing(false)), event(CtlEvent::CaughtUp)],
        false,
    );
    let args = [
        "events",
        "--follow",
        "--account",
        "a@lab.invalid",
        "--kind",
        "typing,caught_up",
        "--kind",
        "message*",
        "--chat",
        "Lab chat",
        "--from",
        "Lab B",
        "--mentions",
        "--detail",
        "--lead",
        "10",
        "--lead",
        "0",
    ];
    let (run, request) = run_one(server, &args);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        expected_clock(
            "22:13:20 a@lab.invalid typing 19:lab@thread.v2 8:orgid:b on\n\
         22:13:20 a@lab.invalid typing 19:lab@thread.v2 8:orgid:b off\n\
         22:13:20 a@lab.invalid caught_up\n"
        )
    );
    assert_eq!(request.account.as_deref(), Some("a@lab.invalid"));
    assert_eq!(
        request.request,
        Request::Subscribe {
            filter: EventFilter {
                kinds: vec![s("typing"), s("caught_up"), s("message*")],
                account: None,
                chat: Some(s("Lab chat")),
                from: Some(s("Lab B")),
                mentions_me: true,
                detail: true,
                leads: vec![10, 0],
            }
        }
    );
}

#[test]
fn events_without_follow_prints_the_first_event_and_json_its_line() {
    let first = event(typing(true));
    let (run, request) = run_one(
        streaming(vec![first.clone(), event(typing(false))], true),
        &["events", "--json"],
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("{first}\n"));
    assert_eq!(
        request.request,
        Request::Subscribe {
            filter: EventFilter::default()
        }
    );
}

#[test]
fn a_lagged_stream_exits_1() {
    let lagged = serde_json::to_string(&EventLine {
        account: None,
        at: 1_700_000_000_000,
        event: CtlEvent::Lagged,
    })
    .unwrap();
    let (run, _) = run_one(
        streaming(vec![event(typing(true)), lagged], true),
        &["events", "--follow"],
    );
    assert_eq!(run.code, 1);
    assert!(
        run.stdout.ends_with("- lagged events were dropped: the stream ends\n"),
        "{}",
        run.stdout
    );
    assert!(run.stderr.contains("fell behind"), "{}", run.stderr);
}

fn said(text: &str) -> CtlEvent {
    CtlEvent::Message {
        chat: s("19:lab@thread.v2"),
        message: s("1"),
        sender: Some(s("8:orgid:b")),
        sender_name: Some(s("Lab B")),
        text: s(text),
        from_me: false,
        mentions_me: false,
        source: MessageSource::Push,
    }
}

#[test]
fn wait_prints_the_first_event_that_matches() {
    let server = streaming(
        vec![event(said("not yet")), event(said("deploy done")), event(said("later"))],
        true,
    );
    let (run, request) = run_one(
        server,
        &["wait", "message", "--match", "deploy (done|failed)", "--timeout", "5"],
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        expected_clock("22:13:20 a@lab.invalid message 19:lab@thread.v2 1 Lab B: deploy done\n")
    );
    let Request::Subscribe { filter } = request.request else {
        panic!("{request:?}");
    };
    assert_eq!(filter.kinds, vec![s("message")]);
}

#[test]
fn wait_times_out_with_exit_3() {
    let server = streaming(vec![event(said("not yet"))], true);
    let started = Instant::now();
    let (run, _) = run_one(server, &["wait", "message", "--match", "never", "--timeout", "0.3"]);
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("no matching event came in time"), "{}", run.stderr);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn wait_on_a_stream_the_app_closes_exits_1() {
    let (run, _) = run_one(streaming(vec![], false), &["wait", "call"]);
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("the app closed the stream"), "{}", run.stderr);
}

#[test]
fn a_bad_timeout_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let run = ctl(
        home.path(),
        &home.path().join("nobody.sock"),
        &["wait", "call", "--timeout", "soon"],
    );
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("isn't a number of seconds"), "{}", run.stderr);
    let run = ctl(
        home.path(),
        &home.path().join("nobody.sock"),
        &["wait", "call", "--timeout=-1"],
    );
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("isn't a number of seconds"), "{}", run.stderr);
}

#[test]
fn an_app_that_closes_before_answering_exits_1() {
    let server = Server::new(Box::new(|_| (vec![], false)));
    let (run, _) = run_one(server, &["status"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.stderr, "ngteamsctl: the app closed the connection\n");
}

fn error_line(id: u64, code: ErrorCode, message: &str) -> String {
    serde_json::to_string(&ResponseLine {
        id,
        outcome: Outcome::Error(CtlError::new(code, message)),
    })
    .unwrap()
}

#[test]
fn a_refused_subscription_exits_1() {
    let server = Server::new(Box::new(|request| {
        (
            vec![error_line(request.id, ErrorCode::UnknownChat, "no chat is Nope")],
            false,
        )
    }));
    let (run, _) = run_one(server, &["events", "--chat", "Nope"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.stderr, "ngteamsctl: unknown_chat: no chat is Nope\n");
}

#[test]
fn a_refused_subscription_prints_its_error_line_with_json() {
    let server = Server::new(Box::new(|request| {
        (
            vec![error_line(request.id, ErrorCode::UnknownAccount, "no account is Nope")],
            false,
        )
    }));
    let (run, request) = run_one(server, &["wait", "message", "--json", "--account", "Nope"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        format!(
            "{}\n",
            error_line(request.id, ErrorCode::UnknownAccount, "no account is Nope")
        )
    );
}

#[test]
fn an_unknown_event_kind_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let nobody = home.path().join("nobody.sock");
    for args in [
        &["wait", "mesage"][..],
        &["wait", "lagged"],
        &["events", "--kind", "typing,typng"],
        &["events", "--follow", "--kind", "nothing*"],
    ] {
        let run = ctl(home.path(), &nobody, args);
        assert_eq!(run.code, 2, "{args:?}");
        assert!(run.stderr.contains("isn't an event kind"), "{args:?}: {}", run.stderr);
    }
    // a known kind or prefix gets as far as the socket
    for args in [&["wait", "call*"][..], &["events", "--kind", "*,ui"]] {
        let run = ctl(home.path(), &nobody, args);
        assert_eq!(run.code, 2, "{args:?}");
        assert!(
            run.stderr.contains("no ngteams is running on"),
            "{args:?}: {}",
            run.stderr
        );
    }
}

/// A fake app that takes the connection and never answers.
fn silent() -> (tempfile::TempDir, PathBuf, JoinHandle<()>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let thread = thread::spawn(move || {
        let mut stream = accept(&listener);
        // read until the client hangs up
        let _ = std::io::copy(&mut stream, &mut std::io::sink());
    });
    (dir, path, thread)
}

#[test]
fn wait_timeout_bounds_a_hello_the_app_never_answers() {
    let (dir, path, thread) = silent();
    let started = Instant::now();
    let run = ctl(dir.path(), &path, &["wait", "message", "--timeout", "0.3"]);
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(run.stderr.contains("no matching event came in time"), "{}", run.stderr);
    assert!(started.elapsed() < Duration::from_secs(5));
    thread.join().unwrap();
}

#[test]
fn wait_timeout_bounds_a_subscribe_the_app_never_answers() {
    let server = Server::new(Box::new(|_| (vec![], true)));
    let started = Instant::now();
    let (run, request) = run_one(server, &["wait", "message", "--timeout", "0.3"]);
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(run.stderr.contains("no matching event came in time"), "{}", run.stderr);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(matches!(request.request, Request::Subscribe { .. }));
}

#[test]
fn wait_with_no_time_left_times_out_before_connecting() {
    let home = tempfile::tempdir().unwrap();
    let run = ctl(
        home.path(),
        &home.path().join("nobody.sock"),
        &["wait", "message", "--timeout", "0"],
    );
    assert_eq!(run.code, 3, "{}", run.stderr);
    assert!(run.stderr.contains("no matching event came in time"), "{}", run.stderr);
}

#[test]
fn a_stream_line_that_is_not_an_event_exits_1() {
    let (run, _) = run_one(
        streaming(vec![s("{\"not\":\"an event\"}")], true),
        &["events", "--follow"],
    );
    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("isn't the JSON expected"), "{}", run.stderr);
}

/// A writer whose every write fails as `kind`.
struct Failing(std::io::ErrorKind);

impl std::io::Write for Failing {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(self.0.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(self.0.into())
    }
}

/// `args` run in this process against `server`, printing into `out`.
fn run_into(server: Server, args: &[&str], out: &mut dyn std::io::Write) -> Exit {
    let cli = Cli::try_parse_from(std::iter::once("ngteamsctl").chain(args.iter().copied())).unwrap();
    let env = Env {
        socket: Some(server.path.clone().into_os_string()),
        ..Env::default()
    };
    let exit = run(&cli, &env, out, &mut Vec::new());
    server.requests();
    exit
}

#[test]
fn a_reader_gone_ends_quietly_and_another_write_error_is_an_error() {
    use std::io::ErrorKind::{BrokenPipe, PermissionDenied};
    let events = || streaming(vec![event(typing(true))], true);
    assert_eq!(
        run_into(events(), &["events", "--follow"], &mut Failing(BrokenPipe)),
        Exit::Ok
    );
    assert_eq!(
        run_into(events(), &["events", "--json"], &mut Failing(PermissionDenied)),
        Exit::Error
    );
    let status = || Server::answering(Response::Actions { actions: vec![s("x")] });
    assert_eq!(
        run_into(status(), &["action", "list"], &mut Failing(BrokenPipe)),
        Exit::Ok
    );
    assert_eq!(
        run_into(status(), &["action", "list", "--json"], &mut Failing(PermissionDenied)),
        Exit::Error
    );
}

fn env(socket: Option<&str>) -> Env {
    Env {
        socket: socket.map(OsString::from),
        runtime_dir: Some(std::env::temp_dir().join("synthetic/runtime").into_os_string()),
        state_home: Some(std::env::temp_dir().join("synthetic/state").into_os_string()),
        home: Some(std::env::temp_dir().join("synthetic/home").into_os_string()),
    }
}

fn chosen(args: &[&str], env: &Env) -> PathBuf {
    let cli = Cli::try_parse_from(std::iter::once("ngteamsctl").chain(args.iter().copied())).unwrap();
    socket(&cli, env).unwrap()
}

#[test]
fn the_socket_is_the_flag_then_the_variable_then_the_mode() {
    let plain = env(None);
    assert_eq!(
        chosen(&["status"], &plain),
        PathBuf::from(plain.runtime_dir.as_ref().unwrap()).join("ngteams/ctl.sock")
    );
    assert_eq!(
        chosen(&["--lab", "a", "status"], &plain),
        PathBuf::from(plain.state_home.as_ref().unwrap()).join("ngteams-lab/a/ctl.sock")
    );
    assert_eq!(
        chosen(&["status", "--read-only"], &plain),
        PathBuf::from(plain.state_home.as_ref().unwrap()).join("ngteams-read-only/run/ctl.sock")
    );
    let overridden = env(Some("/tmp/x/ctl.sock"));
    assert_eq!(
        chosen(&["--lab", "a", "status"], &overridden),
        Path::new("/tmp/x/ctl.sock")
    );
    assert_eq!(
        chosen(&["--socket", "/tmp/y.sock", "--lab", "a", "status"], &overridden),
        Path::new("/tmp/y.sock")
    );
    let cli = Cli::try_parse_from(["ngteamsctl", "--lab", "../x", "status"]).unwrap();
    assert!(socket(&cli, &plain).is_err());
}

#[test]
fn a_mode_without_a_path_exits_2_before_connecting() {
    let cli = Cli::try_parse_from(["ngteamsctl", "status"]).unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(run(&cli, &Env::default(), &mut out, &mut err), Exit::NoApp);
    assert!(out.is_empty());
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "ngteamsctl: no socket to talk to: XDG_RUNTIME_DIR isn't set to an absolute path\n"
    );
}

#[test]
fn regex_wait_matches_the_second_buffered_event_in_process() {
    let server = streaming(
        vec![event(said("not yet")), event(said("deploy done")), event(said("later"))],
        true,
    );
    let mut out = Vec::new();
    assert_eq!(
        run_into(
            server,
            &["wait", "message", "--match", "deploy (done|failed)", "--timeout", "5"],
            &mut out
        ),
        Exit::Ok
    );
    assert!(String::from_utf8(out).unwrap().contains("deploy done"));
}

#[test]
fn buffered_message_events_keep_their_text_across_native_pipe_writes() {
    let server = streaming(
        vec![event(said("not yet")), event(said("deploy done")), event(said("later"))],
        true,
    );
    let client = teams_ctl::Client::connect(&server.path, Timeouts::default()).unwrap();
    let mut events = client.subscribe(None, EventFilter::default()).unwrap();
    events.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    for expected in ["not yet", "deploy done", "later"] {
        let actual = events.next_event().unwrap().unwrap();
        let CtlEvent::Message { text, .. } = actual.event else {
            panic!("not a message");
        };
        assert_eq!(text, expected);
    }
    drop(events);
    server.requests();
}
