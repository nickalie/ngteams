//! `ngteamsctl`'s human-readable output for each answer and each event kind. Times are local, so the checks here leave
//! them out (`tests/cli.rs` runs the binary in UTC and checks them).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::BTreeSet;

use teams_ctl::protocol::{
    AccountRow, CalendarRow, CallRow, ChatRow, Connection, CtlError, CtlEvent, ErrorCode, EventLine, Hello,
    ManualStatus, MessageRow, MessageSource, OutboxRow, OutboxState, OwnStatus, PersonRow, ReactionRow, Response,
    SyncReport, UiState, View, WindowState,
};
use teams_ctl::render::{error, event, kinds, text};

fn s(text: &str) -> String {
    text.to_owned()
}

/// `line` without its leading time.
fn untimed(line: &str) -> &str {
    line.split_once(' ').unwrap().1
}

fn call_row(ringing: bool) -> CallRow {
    CallRow {
        id: s("call-1"),
        account: s("a@lab.invalid"),
        chat: Some(s("19:lab@thread.v2")),
        state: s("connected"),
        ringing,
        caller: ringing.then(|| s("Lab B")),
        muted: true,
        camera: true,
    }
}

fn calendar_row(online: bool) -> CalendarRow {
    CalendarRow {
        id: s("AAMk-synthetic"),
        subject: s("Standup"),
        start: 1_700_000_000_000,
        end: 1_700_001_800_000,
        all_day: false,
        cancelled: false,
        online,
        joinable: online,
        location: None,
        organizer: Some(s("Lab B")),
    }
}

#[test]
fn the_answers_of_later_commands_read_plainly() {
    let outbox = OutboxRow {
        id: 3,
        write: s("send"),
        chat: Some(s("19:lab@thread.v2")),
        state: OutboxState::Sent,
        message: Some(s("17")),
        error: None,
    };
    assert_eq!(text(&Response::Outbox(outbox)), "#3 send 19:lab@thread.v2 sent 17\n");
    assert_eq!(
        text(&Response::OwnStatus(OwnStatus {
            reported: s("Busy"),
            manual: ManualStatus::Dnd,
        })),
        "reported: Busy\nmanual:   dnd\n"
    );
    let synced = text(&Response::Synced(SyncReport {
        conversations: 4,
        messages: 20,
        failed: 0,
        unreadable: 1,
        caught_up: true,
    }));
    assert!(
        synced.contains("conversations: 4\n") && synced.ends_with("caught up:     yes\n"),
        "{synced}"
    );
    assert_eq!(
        text(&Response::Calls {
            calls: vec![call_row(true)],
        }),
        "ID      ACCOUNT        CHAT              STATE\n\
         call-1  a@lab.invalid  19:lab@thread.v2  connected (ringing from Lab B) muted camera\n"
    );
    assert!(text(&Response::Call(call_row(false))).ends_with("connected muted camera\n"));
    let calendar = text(&Response::Calendar {
        events: vec![calendar_row(true), calendar_row(false)],
    });
    let rows: Vec<&str> = calendar.lines().collect();
    assert!(
        rows[0].starts_with("ID") && rows[0].ends_with("FLAGS            SUBJECT"),
        "{calendar}"
    );
    assert!(rows[1].ends_with("online,joinable  Standup"), "{calendar}");
    assert!(rows[2].ends_with("-                Standup"), "{calendar}");
    let event = text(&Response::CalendarEvent(calendar_row(true)));
    assert!(
        event.contains("subject:   Standup\n") && event.contains("location:  -\n"),
        "{event}"
    );
    assert!(event.ends_with("organizer: Lab B\n"), "{event}");
    assert_eq!(text(&Response::Done), "");
    assert_eq!(text(&Response::Subscribed), "");
}

#[test]
fn an_app_status_tells_the_call_up() {
    let status = text(&Response::Status(teams_ctl::protocol::AppStatus {
        app: s("9.9.9"),
        accounts: vec![],
        window: WindowState::Closed,
        call: Some(call_row(false)),
    }));
    assert_eq!(
        status,
        "ngteams 9.9.9  window: closed\n#  ACCOUNT  STATE  NAME\ncall: call-1 on a@lab.invalid in 19:lab@thread.v2 \
         connected muted camera\n"
    );
}

fn line(event: CtlEvent) -> EventLine {
    EventLine {
        account: Some(s("a@lab.invalid")),
        at: 1_700_000_000_000,
        event,
    }
}

fn person(mri: &str, name: Option<&str>) -> PersonRow {
    PersonRow {
        mri: s(mri),
        name: name.map(s),
    }
}

/// Each event kind with the line it reads as (its time left out).
fn samples() -> Vec<(CtlEvent, &'static str)> {
    let mut samples = message_samples();
    samples.extend(chat_samples());
    samples.extend(people_samples());
    samples.extend(call_samples());
    samples.extend(app_samples());
    samples
}

fn message_samples() -> Vec<(CtlEvent, &'static str)> {
    let chat = || s("19:x");
    vec![
        (
            CtlEvent::Message {
                chat: chat(),
                message: s("1"),
                sender: Some(s("8:orgid:b")),
                sender_name: None,
                text: s("a\nb\n"),
                from_me: true,
                mentions_me: true,
                source: MessageSource::CatchUp,
            },
            "message 19:x 1 8:orgid:b: a ↵ b [me] [@me] [catch-up]",
        ),
        (
            CtlEvent::Message {
                chat: chat(),
                message: s("2"),
                sender: None,
                sender_name: None,
                text: s("again"),
                from_me: false,
                mentions_me: false,
                source: MessageSource::Replay,
            },
            "message 19:x 2 ?: again [replay]",
        ),
    ]
}

fn chat_samples() -> Vec<(CtlEvent, &'static str)> {
    let chat = || s("19:x");
    vec![
        (
            CtlEvent::MessageEdited {
                chat: chat(),
                message: s("1"),
            },
            "message_edited 19:x 1",
        ),
        (
            CtlEvent::MessageDeleted {
                chat: chat(),
                message: s("1"),
            },
            "message_deleted 19:x 1",
        ),
        (
            CtlEvent::Reaction {
                chat: chat(),
                message: s("1"),
                reactions: Some(vec![
                    ReactionRow {
                        reaction: s("like"),
                        count: 2,
                        mine: true,
                    },
                    ReactionRow {
                        reaction: s("heart"),
                        count: 1,
                        mine: false,
                    },
                ]),
            },
            "reaction 19:x 1 likex2* heartx1",
        ),
        (
            CtlEvent::Reaction {
                chat: chat(),
                message: s("1"),
                reactions: None,
            },
            "reaction 19:x 1",
        ),
        (
            CtlEvent::Typing {
                chat: chat(),
                who: None,
                on: false,
            },
            "typing 19:x ? off",
        ),
        (CtlEvent::Read { chat: chat() }, "read 19:x"),
        (CtlEvent::ChatChanged { chat: chat() }, "chat_changed 19:x"),
        (CtlEvent::TeamsChanged, "teams_changed"),
        (
            CtlEvent::Unread {
                chat: Some(chat()),
                count: 2,
                total: 5,
            },
            "unread 19:x count=2 total=5",
        ),
        (
            CtlEvent::Unread {
                chat: None,
                count: 0,
                total: 0,
            },
            "unread - count=0 total=0",
        ),
    ]
}

fn people_samples() -> Vec<(CtlEvent, &'static str)> {
    vec![
        (
            CtlEvent::Presence {
                mri: s("8:orgid:b"),
                availability: s("Busy"),
            },
            "presence 8:orgid:b Busy",
        ),
        (
            CtlEvent::OwnStatus {
                availability: s("away"),
            },
            "own_status away",
        ),
        (
            CtlEvent::Outbox(OutboxRow {
                id: 4,
                write: s("edit"),
                chat: None,
                state: OutboxState::Unknown,
                message: None,
                error: Some(s("timed out")),
            }),
            "outbox #4 edit - unknown (timed out)",
        ),
    ]
}

fn call_samples() -> Vec<(CtlEvent, &'static str)> {
    let chat = || s("19:x");
    vec![
        (
            CtlEvent::IncomingCall {
                call: s("c1"),
                caller: s("8:orgid:b"),
                caller_name: Some(s("Lab B")),
                chat: Some(chat()),
            },
            "incoming_call c1 from Lab B in 19:x",
        ),
        (
            CtlEvent::IncomingCall {
                call: s("c1"),
                caller: s("8:orgid:b"),
                caller_name: None,
                chat: None,
            },
            "incoming_call c1 from 8:orgid:b",
        ),
        (
            CtlEvent::RingEnded {
                call: s("c1"),
                how: s("declined"),
            },
            "ring_ended c1 declined",
        ),
        (
            CtlEvent::Call {
                call: s("c1"),
                state: s("ended"),
                reason: Some(s("hung up")),
            },
            "call c1 ended (hung up)",
        ),
        (
            CtlEvent::Call {
                call: s("c1"),
                state: s("connected"),
                reason: None,
            },
            "call c1 connected",
        ),
        (
            CtlEvent::CallParticipants {
                call: s("c1"),
                joined: vec![person("8:orgid:b", None), person("8:orgid:c", Some("Lab C"))],
                left: vec![person("8:orgid:d", None)],
            },
            "call_participants c1 joined: 8:orgid:b, Lab C left: 8:orgid:d",
        ),
        (
            CtlEvent::Lobby {
                call: s("c1"),
                waiting: vec![person("8:orgid:c", Some("Lab C"))],
            },
            "lobby c1 waiting: Lab C",
        ),
        (CtlEvent::Voicemail { call: s("c1") }, "voicemail c1"),
        (CtlEvent::CallWindow { open: false }, "call_window closed"),
        (
            CtlEvent::MeetingStarting {
                id: s("m1"),
                subject: s("Standup"),
                start: 0,
                minutes: 5,
                joinable: true,
            },
            "meeting_starting Standup in 5 min (m1) [joinable]",
        ),
    ]
}

fn app_samples() -> Vec<(CtlEvent, &'static str)> {
    let chat = || s("19:x");
    let ui = UiState {
        shown: Some(s("a@lab.invalid")),
        view: View::Calendar,
        chat: None,
        chat_title: None,
        window: WindowState::Shown,
        call_window: true,
        activity: false,
    };
    vec![
        (
            CtlEvent::Connection {
                state: Connection::Disconnected,
            },
            "connection disconnected",
        ),
        (CtlEvent::CaughtUp, "caught_up"),
        (
            CtlEvent::CatchUpFailed { error: s("offline") },
            "catch_up_failed offline",
        ),
        (
            CtlEvent::EngineFailed {
                during: s("sync"),
                error: s("refused"),
            },
            "engine_failed sync: refused",
        ),
        (CtlEvent::SignedOut, "signed_out"),
        (CtlEvent::AccountAdded, "account_added"),
        (CtlEvent::AccountRemoved, "account_removed"),
        (
            CtlEvent::Notification {
                title: s("Lab B"),
                chat: Some(chat()),
            },
            "notification Lab B (19:x)",
        ),
        (
            CtlEvent::Notification {
                title: s("Lab B"),
                chat: None,
            },
            "notification Lab B",
        ),
        (
            CtlEvent::Ui { state: ui },
            "ui a@lab.invalid calendar - window=shown call-window",
        ),
        (CtlEvent::Lagged, "lagged events were dropped: the stream ends"),
    ]
}

#[test]
fn each_event_kind_reads_as_one_line() {
    let mut seen = BTreeSet::new();
    for (sample, expected) in samples() {
        seen.insert(sample.kind());
        let shown = event(&line(sample));
        assert_eq!(untimed(&shown), format!("a@lab.invalid {expected}"));
        assert!(!shown.contains('\n'), "{shown}");
    }
    assert_eq!(
        seen,
        CtlEvent::KINDS.iter().copied().collect(),
        "an event kind without a sample"
    );
    let own = event(&EventLine {
        account: None,
        at: 0,
        event: CtlEvent::Lagged,
    });
    assert!(untimed(&own).starts_with("- lagged"), "{own}");
}

#[test]
fn kinds_lists_the_kinds_a_subscriber_may_ask_for() {
    let mut out = Vec::new();
    kinds(&mut out).unwrap();
    let listed = String::from_utf8(out).unwrap();
    assert_eq!(listed.lines().count(), CtlEvent::KINDS.len() - 1);
    assert!(!listed.lines().any(|kind| kind == "lagged"));
}

#[test]
fn hello_and_an_error_read_plainly() {
    assert_eq!(
        text(&Response::Hello(Hello {
            protocol: 1,
            app: s("9.9.9"),
        })),
        "ngteams 9.9.9 (protocol 1)\n"
    );
    let mut err = Vec::new();
    error(&mut err, &CtlError::new(ErrorCode::NoWindow, "the window is closed")).unwrap();
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "ngteamsctl: no_window: the window is closed\n"
    );
}

#[test]
fn control_characters_from_the_app_are_shown_escaped() {
    let message = MessageRow {
        id: s("7"),
        chat: s("19:lab@thread.v2"),
        sender: Some(s("8:orgid:b")),
        sender_name: Some(s("Lab\u{1b}]0;x\u{7}B")),
        at: 1_700_000_000_000,
        text: s("one\u{1b}[2J\ttwo\u{9b}\nthree"),
        edited: false,
        deleted: false,
        from_me: false,
        mentions_me: false,
    };
    let table = text(&Response::Messages {
        messages: vec![message],
    });
    let row = table.lines().nth(1).unwrap();
    assert!(
        row.ends_with("Lab\\u{1b}]0;x\\u{7}B  one\\u{1b}[2J\\ttwo\\u{9b} ↵ three"),
        "{table}"
    );
    // the columns are measured on what is shown
    assert_eq!(table.lines().next().unwrap().find("TEXT"), row.find("one"));
    assert!(!table.chars().any(|c| c.is_control() && c != '\n'), "{table:?}");

    let said = event(&line(CtlEvent::Message {
        chat: s("19:x\r\n@thread.v2"),
        message: s("1"),
        sender: None,
        sender_name: Some(s("B")),
        text: s("hi\u{1b}[2J"),
        from_me: false,
        mentions_me: false,
        source: MessageSource::Push,
    }));
    assert_eq!(
        untimed(&said),
        "a@lab.invalid message 19:x\\r\\n@thread.v2 1 B: hi\\u{1b}[2J"
    );

    let state = text(&Response::UiState(UiState {
        shown: None,
        view: View::Chat,
        chat: None,
        chat_title: Some(s("x\u{1b}[31mred")),
        window: WindowState::Shown,
        call_window: false,
        activity: false,
    }));
    assert!(state.contains("title:       x\\u{1b}[31mred\n"), "{state}");

    let hello = text(&Response::Hello(Hello {
        protocol: 1,
        app: s("9\u{1b}[2J"),
    }));
    assert_eq!(hello, "ngteams 9\\u{1b}[2J (protocol 1)\n");

    let mut err = Vec::new();
    let refused = CtlError {
        candidates: vec![teams_ctl::protocol::ChatRef {
            id: s("19:a\u{1b}@thread.v2"),
            title: s("L\u{1b}[2Jab"),
        }],
        ..CtlError::new(ErrorCode::AmbiguousChat, "chats called L\u{1b}[2Jab")
    };
    error(&mut err, &refused).unwrap();
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "ngteamsctl: ambiguous_chat: chats called L\\u{1b}[2Jab\n  19:a\\u{1b}@thread.v2  L\\u{1b}[2Jab\n"
    );
}

#[test]
fn rows_without_their_optional_values_read_plainly() {
    let accounts = text(&Response::Accounts {
        accounts: vec![AccountRow {
            index: 1,
            upn: s("b@lab.invalid"),
            mri: s("8:orgid:b"),
            name: None,
            shown: false,
            state: s("down"),
        }],
    });
    assert!(accounts.ends_with("\n1  b@lab.invalid  down\n"), "{accounts}");
    let chats = text(&Response::Chats {
        chats: vec![ChatRow {
            id: s("19:x"),
            title: s("X"),
            chat_type: s("one_on_one"),
            unread: 0,
            last_activity: None,
            preview: None,
        }],
    });
    assert!(chats.ends_with("\n19:x  0       -     one_on_one  X\n"), "{chats}");
    // a time out of the calendar's range reads as its number
    let mine = MessageRow {
        id: s("1"),
        chat: s("19:x"),
        sender: None,
        sender_name: None,
        at: i64::MAX,
        text: s("gone"),
        edited: false,
        deleted: true,
        from_me: true,
        mentions_me: false,
    };
    let messages = text(&Response::Messages { messages: vec![mine] });
    assert!(
        messages.ends_with(&format!("\n1   {}  - (me)  (deleted) gone\n", i64::MAX)),
        "{messages}"
    );
    let far = event(&EventLine {
        account: None,
        at: i64::MIN,
        event: CtlEvent::TeamsChanged,
    });
    assert_eq!(far, format!("{} - teams_changed", i64::MIN));
}

#[test]
fn rarer_answers_read_plainly() {
    for (state, word) in [(OutboxState::Queued, "queued"), (OutboxState::Failed, "failed")] {
        let row = OutboxRow {
            id: 1,
            write: s("react"),
            chat: None,
            state,
            message: None,
            error: None,
        };
        assert_eq!(text(&Response::Outbox(row)), format!("#1 react - {word}\n"));
    }
    let report = SyncReport {
        conversations: 0,
        messages: 0,
        failed: 2,
        unreadable: 0,
        caught_up: false,
    };
    assert!(text(&Response::Synced(report)).ends_with("caught up:     no\n"));
    let quiet = CallRow {
        chat: None,
        muted: false,
        camera: false,
        ..call_row(false)
    };
    assert!(text(&Response::Call(quiet)).ends_with("\ncall-1  a@lab.invalid  -     connected\n"));
    let placed = CalendarRow {
        location: Some(s("Room 1")),
        organizer: None,
        ..calendar_row(false)
    };
    let placed = text(&Response::CalendarEvent(placed));
    assert!(placed.contains("location:  Room 1\norganizer: -\n"), "{placed}");
    let calling = UiState {
        shown: None,
        view: View::Other,
        chat: None,
        chat_title: None,
        window: WindowState::Closed,
        call_window: true,
        activity: false,
    };
    let calling = text(&Response::UiState(calling));
    assert!(
        calling.ends_with("window:      closed\ncall window: open\n"),
        "{calling}"
    );
}

#[test]
fn each_view_and_window_reads_as_its_word() {
    for (view, word) in [
        (View::Chat, "chat"),
        (View::Channel, "channel"),
        (View::Calendar, "calendar"),
        (View::Settings, "settings"),
        (View::Teams, "teams"),
        (View::Search, "search"),
        (View::Other, "other"),
    ] {
        let state = UiState {
            shown: None,
            view,
            chat: None,
            chat_title: None,
            window: WindowState::Tray,
            call_window: false,
            activity: false,
        };
        assert_eq!(
            text(&Response::UiState(state.clone())),
            format!(
                "account:     -\nview:        {word}\nlist:        chats\nchat:        -\ntitle:       -\nwindow:      tray\ncall window: closed\n"
            )
        );
        assert_eq!(
            untimed(&event(&line(CtlEvent::Ui { state: state.clone() }))),
            format!("a@lab.invalid ui - {word} - window=tray")
        );
        let activity = UiState {
            activity: true,
            ..state
        };
        assert!(text(&Response::UiState(activity)).contains("list:        activity\n"));
    }
}

#[test]
fn rarer_event_values_read_plainly() {
    let meeting = CtlEvent::MeetingStarting {
        id: s("m1"),
        subject: s("Review"),
        start: 0,
        minutes: 0,
        joinable: false,
    };
    let joined = CtlEvent::CallParticipants {
        call: s("c1"),
        joined: vec![person("8:orgid:b", None)],
        left: vec![],
    };
    let none = CtlEvent::CallParticipants {
        call: s("c1"),
        joined: vec![],
        left: vec![],
    };
    for (sample, expected) in [
        (CtlEvent::CallWindow { open: true }, "call_window open"),
        (meeting, "meeting_starting Review in 0 min (m1)"),
        (none, "call_participants c1"),
        (joined, "call_participants c1 joined: 8:orgid:b"),
    ] {
        assert_eq!(untimed(&event(&line(sample))), format!("a@lab.invalid {expected}"));
    }
}
