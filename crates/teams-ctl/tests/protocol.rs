//! The wire's shapes: every request, response and event round trips, and a line that doesn't read is `bad_request`.
//!
//! The samples are lines as the wire spells them, with every field their type serializes, so each must come back
//! from its type as the same JSON.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::collections::BTreeSet;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use teams_ctl::codec::{BadLine, decode_request};
use teams_ctl::protocol::{
    ChatRef, CtlError, CtlEvent, ErrorCode, EventFilter, EventLine, Hello, OpenTarget, Outcome, Request, RequestLine,
    Response, ResponseLine, calendar_window, local_today, midnight_in,
};

/// The variant's place: exhaustive, so a new request fails to compile here until it has a sample.
fn request_index(request: &Request) -> usize {
    match request {
        Request::Hello { .. } => 0,
        Request::Status => 1,
        Request::Accounts => 2,
        Request::Chats { .. } => 3,
        Request::Messages { .. } => 4,
        Request::Unread => 5,
        Request::Search { .. } => 6,
        Request::Members { .. } => 7,
        Request::Window { .. } => 8,
        Request::Open { .. } => 9,
        Request::SwitchAccount { .. } => 10,
        Request::UiState => 11,
        Request::Action { .. } => 12,
        Request::ActionList => 13,
        Request::Subscribe { .. } => 14,
        Request::Activate => 15,
        Request::Send { .. } => 16,
        Request::Edit { .. } => 17,
        Request::Delete { .. } => 18,
        Request::React { .. } => 19,
        Request::Unreact { .. } => 20,
        Request::MarkRead { .. } => 21,
        Request::NewChat { .. } => 22,
        Request::Resend { .. } => 23,
        Request::GetStatus => 24,
        Request::SetStatus { .. } => 25,
        Request::Sync => 26,
        Request::Calls => 27,
        Request::Call { .. } => 28,
        Request::Answer => 29,
        Request::Decline => 30,
        Request::HangUp => 31,
        Request::Mute { .. } => 32,
        Request::Camera { .. } => 33,
        Request::Join { .. } => 34,
        Request::Calendar { .. } => 35,
        Request::CalendarEvent { .. } => 36,
        Request::JoinMeeting { .. } => 37,
        Request::OpenLink { .. } => 38,
        Request::Activity { .. } => 39,
    }
}
const REQUESTS: usize = 40;

/// One line of each request (and of each value of their enums).
const REQUEST_LINES: &[&str] = &[
    r#"{"id":1,"cmd":"hello","protocol":1,"client":"0.1.0"}"#,
    r#"{"id":2,"cmd":"status"}"#,
    r#"{"id":3,"account":"a@lab.invalid","cmd":"accounts"}"#,
    r#"{"id":4,"cmd":"chats","unread_only":true,"limit":20}"#,
    r#"{"id":5,"cmd":"chats","unread_only":false}"#,
    r#"{"id":6,"account":"1","cmd":"messages","chat":"19:x@thread.v2","limit":5,"before":"1700000000000"}"#,
    r#"{"id":7,"cmd":"messages","chat":"Lab B"}"#,
    r#"{"id":8,"cmd":"unread"}"#,
    r#"{"id":9,"cmd":"search","query":"standup","limit":3}"#,
    r#"{"id":10,"cmd":"members","chat":"19:x@thread.v2"}"#,
    r#"{"id":11,"cmd":"window","op":"show"}"#,
    r#"{"id":12,"cmd":"window","op":"hide"}"#,
    r#"{"id":13,"cmd":"window","op":"toggle"}"#,
    r#"{"id":14,"cmd":"window","op":"focus"}"#,
    r#"{"id":15,"cmd":"open","target":{"view":"chat","chat":"Lab B"}}"#,
    r#"{"id":16,"cmd":"open","target":{"view":"channel","channel":"19:c@thread.tacv2"}}"#,
    r#"{"id":17,"cmd":"open","target":{"view":"calendar"}}"#,
    r#"{"id":18,"cmd":"open","target":{"view":"settings","page":"notifications"}}"#,
    r#"{"id":19,"cmd":"open","target":{"view":"settings"}}"#,
    r#"{"id":20,"cmd":"open","target":{"view":"search","query":"budget"}}"#,
    r#"{"id":21,"cmd":"switch_account","to":"1"}"#,
    r#"{"id":22,"cmd":"ui_state"}"#,
    r#"{"id":23,"cmd":"action","name":"ShowCalendar"}"#,
    r#"{"id":24,"cmd":"action_list"}"#,
    r#"{"id":25,"cmd":"subscribe","filter":{"kinds":["message*","unread"],"account":"0","chat":"19:x@thread.v2","from":"Lab B","mentions_me":true,"detail":true,"leads":[10,0]}}"#,
    r#"{"id":26,"cmd":"subscribe","filter":{"mentions_me":false,"detail":false}}"#,
    r#"{"id":27,"cmd":"activate"}"#,
    r#"{"id":28,"cmd":"send","chat":"19:x@thread.v2","text":"two\nlines"}"#,
    r#"{"id":29,"cmd":"edit","chat":"19:x@thread.v2","message":"1700000000000","text":"fixed","wait":true}"#,
    r#"{"id":30,"cmd":"delete","chat":"19:x@thread.v2","message":"1700000000000"}"#,
    r#"{"id":31,"cmd":"react","chat":"19:x@thread.v2","message":"1700000000000","reaction":"like"}"#,
    r#"{"id":32,"cmd":"unreact","chat":"19:x@thread.v2","message":"1700000000000","reaction":"like"}"#,
    r#"{"id":33,"cmd":"mark_read","chat":"19:x@thread.v2"}"#,
    r#"{"id":34,"cmd":"new_chat","people":["b@lab.invalid","8:orgid:00000000-0000-0000-0000-00000000000c"]}"#,
    r#"{"id":35,"cmd":"resend","row":42,"wait":true}"#,
    r#"{"id":36,"cmd":"get_status"}"#,
    r#"{"id":37,"cmd":"set_status","status":"available"}"#,
    r#"{"id":38,"cmd":"set_status","status":"busy"}"#,
    r#"{"id":39,"cmd":"set_status","status":"dnd"}"#,
    r#"{"id":40,"cmd":"set_status","status":"away"}"#,
    r#"{"id":41,"cmd":"set_status","status":"reset"}"#,
    r#"{"id":42,"cmd":"sync"}"#,
    r#"{"id":43,"cmd":"calls"}"#,
    r#"{"id":44,"cmd":"call","chat":"19:x@thread.v2"}"#,
    r#"{"id":45,"cmd":"answer"}"#,
    r#"{"id":46,"cmd":"decline"}"#,
    r#"{"id":47,"cmd":"hang_up"}"#,
    r#"{"id":48,"cmd":"mute","op":"on"}"#,
    r#"{"id":49,"cmd":"mute","op":"off"}"#,
    r#"{"id":50,"cmd":"camera","op":"toggle"}"#,
    r#"{"id":51,"cmd":"join","chat":"19:x@thread.v2"}"#,
    r#"{"id":52,"cmd":"calendar","from":"2026-11-01","to":"2026-11-07"}"#,
    r#"{"id":53,"cmd":"calendar"}"#,
    r#"{"id":54,"cmd":"calendar_event","event":"AAMk-synthetic"}"#,
    r#"{"id":55,"cmd":"join_meeting","event":"AAMk-synthetic","now":true}"#,
    r#"{"id":56,"cmd":"join_meeting","event":"AAMk-synthetic","now":false}"#,
    r#"{"id":57,"cmd":"open_link","url":"msteams:/l/meetup-join/19%3ameeting_synthetic%40thread.v2/0"}"#,
    r#"{"id":58,"cmd":"activity","filter":"unread","search":"lab b","limit":10}"#,
    r#"{"id":59,"cmd":"activity"}"#,
    r#"{"id":60,"cmd":"open","target":{"view":"activity"}}"#,
];

/// `text` read as a `T` and written again: the same JSON.
fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(text: &str) -> T {
    let value: T = serde_json::from_str(text).unwrap_or_else(|error| panic!("{text}: {error}"));
    let again = serde_json::to_string(&value).unwrap();
    assert!(!again.contains('\n'), "{again}");
    assert_eq!(
        serde_json::from_str::<Value>(&again).unwrap(),
        serde_json::from_str::<Value>(text).unwrap(),
        "{text}"
    );
    assert_eq!(serde_json::from_str::<T>(&again).unwrap(), value);
    value
}

#[test]
fn every_request_round_trips_through_its_line() {
    let mut seen = BTreeSet::new();
    for text in REQUEST_LINES {
        let line: RequestLine = round_trip(text);
        assert_eq!(decode_request(text).unwrap(), line);
        seen.insert(request_index(&line.request));
    }
    assert_eq!(seen, (0..REQUESTS).collect(), "a request kind without a sample");
}

#[test]
fn a_request_line_reads_its_defaults() {
    let parsed = decode_request(r#"{"id":3,"cmd":"chats"}"#).unwrap();
    assert_eq!(
        parsed,
        RequestLine {
            id: 3,
            account: None,
            request: Request::Chats {
                unread_only: false,
                limit: None,
            },
        }
    );
    let parsed = decode_request(r#"{"id":4,"cmd":"open","target":{"view":"calendar"}}"#).unwrap();
    assert_eq!(
        parsed.request,
        Request::Open {
            target: OpenTarget::Calendar
        }
    );
    let parsed = decode_request(r#"{"id":5,"cmd":"subscribe"}"#).unwrap();
    assert_eq!(
        parsed.request,
        Request::Subscribe {
            filter: EventFilter::default()
        }
    );
    let parsed = decode_request(r#"{"id":6,"cmd":"join_meeting","event":"e"}"#).unwrap();
    assert_eq!(
        parsed.request,
        Request::JoinMeeting {
            event: "e".to_owned(),
            now: false
        }
    );
}

fn bad(line: &str) -> BadLine {
    decode_request(line).unwrap_err()
}

#[test]
fn a_line_that_does_not_read_is_a_bad_request_with_its_id_when_it_has_one() {
    let unknown = bad(r#"{"id":9,"cmd":"launch_missiles"}"#);
    assert_eq!(unknown.id, 9);
    assert_eq!(unknown.error.code, ErrorCode::BadRequest);
    assert!(
        unknown.error.message.contains("launch_missiles"),
        "{}",
        unknown.error.message
    );

    let missing = bad(r#"{"id":10,"cmd":"send","chat":"19:x@thread.v2"}"#);
    assert_eq!((missing.id, missing.error.code), (10, ErrorCode::BadRequest));
    assert!(missing.error.message.contains("text"), "{}", missing.error.message);

    let no_cmd = bad(r#"{"id":11}"#);
    assert_eq!((no_cmd.id, no_cmd.error.code), (11, ErrorCode::BadRequest));

    let wrong_value = bad(r#"{"id":12,"cmd":"set_status","status":"brb"}"#);
    assert_eq!((wrong_value.id, wrong_value.error.code), (12, ErrorCode::BadRequest));

    for line in [
        "not json",
        r#"{"cmd":"status"}"#,
        r#"{"id":"1","cmd":"status"}"#,
        r#"{"id":-1,"cmd":"status"}"#,
    ] {
        let error = bad(line);
        assert_eq!((error.id, error.error.code), (0, ErrorCode::BadRequest), "{line}");
    }
}

/// The variant's place: exhaustive, so a new response fails to compile here until it has a sample.
fn response_index(response: &Response) -> usize {
    match response {
        Response::Hello(_) => 0,
        Response::Status(_) => 1,
        Response::Accounts { .. } => 2,
        Response::Chats { .. } => 3,
        Response::Messages { .. } => 4,
        Response::Unread(_) => 5,
        Response::Search { .. } => 6,
        Response::Members { .. } => 7,
        Response::UiState(_) => 8,
        Response::Dispatched { .. } => 9,
        Response::Actions { .. } => 10,
        Response::Subscribed => 11,
        Response::Done => 12,
        Response::Outbox(_) => 13,
        Response::OwnStatus(_) => 14,
        Response::Synced(_) => 15,
        Response::Calls { .. } => 16,
        Response::Call(_) => 17,
        Response::Calendar { .. } => 18,
        Response::CalendarEvent(_) => 19,
        Response::Activity { .. } => 20,
    }
}
const RESPONSES: usize = 21;

/// One `ok` answer of each kind (and of each value of their enums).
const RESPONSE_LINES: &[&str] = &[
    r#"{"id":1,"ok":{"kind":"hello","protocol":1,"app":"0.1.0"}}"#,
    r#"{"id":2,"ok":{"kind":"status","app":"0.1.0","accounts":[{"index":0,"upn":"a@lab.invalid","mri":"8:orgid:a","name":"Lab A","shown":true,"state":"running"}],"window":"shown","call":{"id":"call-1","account":"a@lab.invalid","chat":"19:x@thread.v2","state":"connected","ringing":false,"muted":true,"camera":false}}}"#,
    r#"{"id":3,"ok":{"kind":"status","app":"0.1.0","accounts":[],"window":"tray"}}"#,
    r#"{"id":4,"ok":{"kind":"status","app":"0.1.0","accounts":[],"window":"closed"}}"#,
    r#"{"id":5,"ok":{"kind":"accounts","accounts":[{"index":1,"upn":"b@lab.invalid","mri":"8:orgid:b","shown":false,"state":"stopped"}]}}"#,
    r#"{"id":6,"ok":{"kind":"chats","chats":[{"id":"19:x@thread.v2","title":"Lab B","chat_type":"one_on_one","unread":2,"last_activity":1700000000000,"preview":"hi"},{"id":"19:y@thread.v2","title":"Group","chat_type":"group","unread":0}]}}"#,
    r#"{"id":7,"ok":{"kind":"messages","messages":[{"id":"1700000000000","chat":"19:x@thread.v2","sender":"8:orgid:b","sender_name":"Lab B","at":1700000000000,"text":"hello\nthere","edited":true,"deleted":false,"from_me":false,"mentions_me":true}]}}"#,
    r#"{"id":8,"ok":{"kind":"unread","total":3,"chats":[{"chat":"19:x@thread.v2","title":"Lab B","count":3}]}}"#,
    r#"{"id":9,"ok":{"kind":"search","messages":[{"id":"1","chat":"19:x@thread.v2","at":1,"text":"budget","edited":false,"deleted":false,"from_me":true,"mentions_me":false}]}}"#,
    r#"{"id":10,"ok":{"kind":"members","members":[{"mri":"8:orgid:b","name":"Lab B","role":"Admin"},{"mri":"8:orgid:c"}]}}"#,
    r#"{"id":11,"ok":{"kind":"ui_state","shown":"a@lab.invalid","view":"chat","chat":"19:x@thread.v2","chat_title":"Lab B","window":"shown","call_window":false}}"#,
    r#"{"id":12,"ok":{"kind":"ui_state","view":"channel","window":"tray","call_window":true}}"#,
    r#"{"id":13,"ok":{"kind":"ui_state","view":"calendar","window":"closed","call_window":false}}"#,
    r#"{"id":14,"ok":{"kind":"ui_state","view":"settings","window":"shown","call_window":false}}"#,
    r#"{"id":15,"ok":{"kind":"ui_state","view":"teams","window":"shown","call_window":false}}"#,
    r#"{"id":16,"ok":{"kind":"ui_state","view":"search","window":"shown","call_window":false}}"#,
    r#"{"id":17,"ok":{"kind":"ui_state","view":"other","window":"shown","call_window":false}}"#,
    r#"{"id":18,"ok":{"kind":"dispatched","action":"teams_ui::ShowCalendar"}}"#,
    r#"{"id":19,"ok":{"kind":"actions","actions":["teams_ui::ShowCalendar","teams_ui::ToggleMute"]}}"#,
    r#"{"id":20,"ok":{"kind":"subscribed"}}"#,
    r#"{"id":21,"ok":{"kind":"done"}}"#,
    r#"{"id":22,"ok":{"kind":"outbox","id":3,"write":"send","chat":"19:x@thread.v2","state":"queued"}}"#,
    r#"{"id":23,"ok":{"kind":"outbox","id":3,"write":"send","state":"sent","message":"1700000000000"}}"#,
    r#"{"id":24,"ok":{"kind":"outbox","id":3,"write":"edit","state":"failed","error":"refused"}}"#,
    r#"{"id":25,"ok":{"kind":"outbox","id":3,"write":"delete","state":"unknown"}}"#,
    r#"{"id":26,"ok":{"kind":"own_status","reported":"Available","manual":"automatic"}}"#,
    r#"{"id":27,"ok":{"kind":"own_status","reported":"Available","manual":"available"}}"#,
    r#"{"id":28,"ok":{"kind":"own_status","reported":"Busy","manual":"busy"}}"#,
    r#"{"id":29,"ok":{"kind":"own_status","reported":"DoNotDisturb","manual":"dnd"}}"#,
    r#"{"id":30,"ok":{"kind":"own_status","reported":"Away","manual":"away"}}"#,
    r#"{"id":31,"ok":{"kind":"synced","conversations":4,"messages":10,"failed":0,"unreadable":1,"caught_up":true}}"#,
    r#"{"id":32,"ok":{"kind":"calls","calls":[{"id":"call-2","account":"b@lab.invalid","state":"ringing","ringing":true,"caller":"8:orgid:c","muted":false,"camera":false}]}}"#,
    r#"{"id":33,"ok":{"kind":"call","id":"call-1","account":"a@lab.invalid","state":"connecting","ringing":false,"muted":false,"camera":true}}"#,
    r#"{"id":34,"ok":{"kind":"calendar","events":[{"id":"AAMk-synthetic","subject":"Standup","start":1700000000000,"end":1700000900000,"all_day":false,"cancelled":false,"online":true,"joinable":true,"location":"Room 1","organizer":"Lab A"}]}}"#,
    r#"{"id":35,"ok":{"kind":"calendar_event","id":"AAMk-synthetic","subject":"","start":1,"end":2,"all_day":true,"cancelled":true,"online":false,"joinable":false}}"#,
    r#"{"id":36,"ok":{"kind":"activity","items":[{"id":"1700000000001","kind":"reaction","title":"Lab B +1 reacted to your message","preview":"hello","place":"In chat with you","chat":"19:x@unq.gbl.spaces","message":"1700000000000","at":1700000000002,"read":false},{"id":"2","kind":"missed-call","title":"Missed call from Lab C","at":2,"read":true}]}}"#,
    r#"{"id":37,"ok":{"kind":"ui_state","view":"chat","window":"shown","call_window":false,"activity":true}}"#,
];

#[test]
fn every_response_round_trips_through_its_line() {
    let mut seen = BTreeSet::new();
    for text in RESPONSE_LINES {
        let line: ResponseLine = round_trip(text);
        let Outcome::Ok(response) = &line.outcome else {
            panic!("{text}");
        };
        seen.insert(response_index(response));
    }
    assert_eq!(seen, (0..RESPONSES).collect(), "a response kind without a sample");
    let hello: ResponseLine = serde_json::from_str(RESPONSE_LINES[0]).unwrap();
    assert_eq!(
        hello.outcome,
        Outcome::Ok(Response::Hello(Hello {
            protocol: 1,
            app: "0.1.0".to_owned(),
        }))
    );
}

const CODES: [ErrorCode; 18] = [
    ErrorCode::UnknownAccount,
    ErrorCode::UnknownChat,
    ErrorCode::AmbiguousChat,
    ErrorCode::Busy,
    ErrorCode::NoCalling,
    ErrorCode::NoCall,
    ErrorCode::NotRinging,
    ErrorCode::NoVideo,
    ErrorCode::NoMeeting,
    ErrorCode::NoTray,
    ErrorCode::NoWindow,
    ErrorCode::UnknownAction,
    ErrorCode::RefusedReadOnly,
    ErrorCode::Stopped,
    ErrorCode::Timeout,
    ErrorCode::BadRequest,
    ErrorCode::VersionMismatch,
    ErrorCode::Engine,
];

#[test]
fn every_error_code_round_trips_and_spells_as_the_wire_does() {
    let mut spelled = BTreeSet::new();
    for code in CODES {
        let error = CtlError::new(code, "why");
        let line = ResponseLine {
            id: 1,
            outcome: Outcome::Error(error.clone()),
        };
        let value = serde_json::to_value(&line).unwrap();
        assert_eq!(
            value,
            json!({"id": 1, "error": {"code": code.as_str(), "message": "why"}})
        );
        assert_eq!(serde_json::from_value::<ResponseLine>(value).unwrap(), line);
        assert_eq!(error.to_string(), format!("{}: why", code.as_str()));
        spelled.insert(code.as_str());
    }
    assert_eq!(spelled.len(), CODES.len());
    let ambiguous = CtlError {
        candidates: vec![
            ChatRef {
                id: "19:a@thread.v2".to_owned(),
                title: "Lab".to_owned(),
            },
            ChatRef {
                id: "19:b@thread.v2".to_owned(),
                title: "Lab".to_owned(),
            },
        ],
        ..CtlError::new(ErrorCode::AmbiguousChat, "2 chats are called Lab")
    };
    let line: ResponseLine = round_trip(&format!(
        r#"{{"id":2,"error":{}}}"#,
        serde_json::to_string(&ambiguous).unwrap()
    ));
    assert_eq!(line.outcome, Outcome::Error(ambiguous));
}

/// One line of each event kind (and of each value of their enums).
const EVENT_LINES: &[&str] = &[
    r#"{"account":"a@lab.invalid","at":1,"event":"message","chat":"19:x@thread.v2","message":"1","sender":"8:orgid:b","sender_name":"Lab B","text":"ping","from_me":false,"mentions_me":true,"source":"push"}"#,
    r#"{"account":"a@lab.invalid","at":1,"event":"message","chat":"19:x@thread.v2","message":"2","text":"caught","from_me":true,"mentions_me":false,"source":"catch_up"}"#,
    r#"{"account":"a@lab.invalid","at":1,"event":"message","chat":"19:x@thread.v2","message":"3","text":"again","from_me":false,"mentions_me":false,"source":"replay"}"#,
    r#"{"account":"a@lab.invalid","at":2,"event":"message_edited","chat":"19:x@thread.v2","message":"1"}"#,
    r#"{"account":"a@lab.invalid","at":2,"event":"message_deleted","chat":"19:x@thread.v2","message":"1"}"#,
    r#"{"account":"a@lab.invalid","at":3,"event":"reaction","chat":"19:x@thread.v2","message":"1"}"#,
    r#"{"account":"a@lab.invalid","at":3,"event":"reaction","chat":"19:x@thread.v2","message":"1","reactions":[{"reaction":"like","count":2,"mine":true}]}"#,
    r#"{"account":"a@lab.invalid","at":4,"event":"typing","chat":"19:x@thread.v2","who":"8:orgid:b","on":true}"#,
    r#"{"account":"a@lab.invalid","at":4,"event":"read","chat":"19:x@thread.v2"}"#,
    r#"{"account":"a@lab.invalid","at":4,"event":"chat_changed","chat":"19:x@thread.v2"}"#,
    r#"{"account":"a@lab.invalid","at":4,"event":"teams_changed"}"#,
    r#"{"account":"a@lab.invalid","at":5,"event":"unread","chat":"19:x@thread.v2","count":1,"total":4}"#,
    r#"{"account":"a@lab.invalid","at":5,"event":"unread","count":0,"total":0}"#,
    r#"{"account":"a@lab.invalid","at":5,"event":"presence","mri":"8:orgid:b","availability":"Busy"}"#,
    r#"{"account":"a@lab.invalid","at":5,"event":"own_status","availability":"Away"}"#,
    r#"{"account":"a@lab.invalid","at":6,"event":"outbox","id":3,"write":"send","chat":"19:x@thread.v2","state":"unknown","error":"timed out"}"#,
    r#"{"account":"a@lab.invalid","at":7,"event":"incoming_call","call":"call-1","caller":"8:orgid:b","caller_name":"Lab B"}"#,
    r#"{"account":"a@lab.invalid","at":7,"event":"ring_ended","call":"call-1","how":"answered elsewhere"}"#,
    r#"{"account":"a@lab.invalid","at":7,"event":"call","call":"call-1","state":"ended","reason":"hung up"}"#,
    r#"{"account":"a@lab.invalid","at":7,"event":"call_participants","call":"call-1","joined":[{"mri":"8:orgid:b"}],"left":[{"mri":"8:orgid:c","name":"Lab C"}]}"#,
    r#"{"account":"a@lab.invalid","at":7,"event":"lobby","call":"call-1","waiting":[{"mri":"8:orgid:c","name":"Lab C"}]}"#,
    r#"{"account":"a@lab.invalid","at":7,"event":"voicemail","call":"call-1"}"#,
    r#"{"account":"a@lab.invalid","at":7,"event":"call_window","open":true}"#,
    r#"{"account":"a@lab.invalid","at":8,"event":"meeting_starting","id":"AAMk-synthetic","subject":"Standup","start":1700000000000,"minutes":5,"joinable":true}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"connection","state":"connected"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"connection","state":"disconnected"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"connection","state":"stopped"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"caught_up"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"catch_up_failed","error":"offline"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"engine_failed","during":"sync","error":"refused"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"signed_out"}"#,
    r#"{"account":"b@lab.invalid","at":9,"event":"account_added"}"#,
    r#"{"account":"b@lab.invalid","at":9,"event":"account_removed"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"notification","title":"Lab B","chat":"19:x@thread.v2"}"#,
    r#"{"account":"a@lab.invalid","at":9,"event":"ui","state":{"shown":"a@lab.invalid","view":"calendar","window":"shown","call_window":false}}"#,
    r#"{"at":10,"event":"lagged"}"#,
];

#[test]
fn every_event_kind_round_trips_through_its_line() {
    let mut seen = BTreeSet::new();
    for text in EVENT_LINES {
        let line: EventLine = round_trip(text);
        let kind = line.event.kind();
        assert_eq!(serde_json::from_str::<Value>(text).unwrap()["event"], Value::from(kind));
        seen.insert(kind);
    }
    assert_eq!(
        seen,
        CtlEvent::KINDS.iter().copied().collect(),
        "an event kind without a sample"
    );
    assert_eq!(CtlEvent::KINDS.len(), seen.len(), "a kind listed twice");
}

#[test]
fn a_filter_lets_through_the_kinds_it_names_and_their_prefixes() {
    let every = EventFilter::default();
    assert!(CtlEvent::KINDS.iter().all(|kind| every.wants_kind(kind)));

    let some = EventFilter {
        kinds: vec!["message*".to_owned(), "unread".to_owned(), "call".to_owned()],
        ..EventFilter::default()
    };
    for kind in ["message", "message_edited", "message_deleted", "unread", "call"] {
        assert!(some.wants_kind(kind), "{kind}");
    }
    for kind in ["call_participants", "call_window", "reaction", "unreadable"] {
        assert!(!some.wants_kind(kind), "{kind}");
    }
    let star = EventFilter {
        kinds: vec!["*".to_owned()],
        ..EventFilter::default()
    };
    assert!(star.wants_kind("lagged"));
}

fn day(text: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
}

const HOUR: i64 = 3_600_000;

#[test]
fn a_calendar_window_runs_from_its_first_local_midnight_to_the_one_after_its_last_day() {
    let today = day("2026-11-07");
    let (start, end) = calendar_window(None, None, today).unwrap();
    // a day, give or take a change of clock
    assert!((23 * HOUR..=25 * HOUR).contains(&(end - start)), "{start}..{end}");
    assert_eq!(
        calendar_window(Some("2026-11-07"), None, day("2001-01-01")).unwrap(),
        (start, end)
    );
    assert_eq!(
        calendar_window(Some("2026-11-07"), Some("2026-11-07"), today).unwrap(),
        (start, end)
    );
    let (week_start, week_end) = calendar_window(Some("2026-11-02"), Some("2026-11-08"), today).unwrap();
    assert!(
        (week_end - week_start - 7 * 24 * HOUR).abs() <= HOUR,
        "{week_start}..{week_end}"
    );
    assert!(week_start < start && end < week_end, "today is inside the week");
    // without a first day, the last one counts from today
    let (from_today, _) = calendar_window(None, Some("2026-11-09"), today).unwrap();
    assert_eq!(from_today, start);
}

#[test]
fn a_calendar_window_refuses_a_date_that_does_not_read_and_days_the_wrong_way_round() {
    let today = day("2026-11-07");
    for (from, to) in [
        (Some("2026-13-01"), None),
        (None, Some("tomorrow")),
        (Some("07.11.2026"), None),
        (Some("2026-11-08"), Some("2026-11-07")),
    ] {
        let refused = calendar_window(from, to, today).unwrap_err();
        assert_eq!(refused.code, ErrorCode::BadRequest, "{from:?}..{to:?}");
    }
    let backwards = calendar_window(None, Some("2026-11-06"), today).unwrap_err();
    assert!(backwards.message.contains("before"), "{}", backwards.message);
    // no day after the last one there is
    let last = calendar_window(None, None, chrono::NaiveDate::MAX).unwrap_err();
    assert_eq!(last.code, ErrorCode::BadRequest);
}

#[test]
fn today_s_window_holds_now() {
    let (start, end) = calendar_window(None, None, local_today()).unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    assert!((start..end).contains(&now), "{start}..{end} without {now}");
}

/// `date` at `hour` UTC, in ms since the epoch.
fn utc(date: &str, hour: u32) -> i64 {
    day(date).and_hms_opt(hour, 0, 0).unwrap().and_utc().timestamp_millis()
}

#[test]
fn a_day_starts_at_its_local_midnight_or_when_the_clock_jumps_over_it() {
    use chrono_tz::{
        America::{Havana, Santiago},
        Pacific::Apia,
    };
    assert_eq!(midnight_in(&Santiago, day("2026-11-07")), utc("2026-11-07", 3));
    // the clock goes back from 01:00 to 00:00: the first of the two midnights (-04, not -05)
    assert_eq!(midnight_in(&Havana, day("2026-11-01")), utc("2026-11-01", 4));
    // the clock jumps from 00:00 to 01:00: the day starts at 01:00 (-03), as -04's midnight
    assert_eq!(midnight_in(&Santiago, day("2026-09-06")), utc("2026-09-06", 4));
    assert_eq!(
        midnight_in(&Santiago, day("2026-09-07")) - midnight_in(&Santiago, day("2026-09-06")),
        23 * HOUR
    );
    // a date skipped whole is an empty day: it starts as the next one does
    assert_eq!(midnight_in(&Apia, day("2011-12-30")), utc("2011-12-30", 10));
    assert_eq!(
        midnight_in(&Apia, day("2011-12-31")),
        midnight_in(&Apia, day("2011-12-30"))
    );
    // no day before the first one there is to take the clock's offset from: its midnight as UTC
    assert_eq!(
        midnight_in(&chrono::Utc, chrono::NaiveDate::MIN),
        chrono::NaiveDate::MIN
            .and_time(chrono::NaiveTime::MIN)
            .and_utc()
            .timestamp_millis()
    );
}
