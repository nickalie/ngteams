//! The control socket's event stream (`ctl::events`): each account's bridge tap, the hub, `subscribe` and its filters,
//! through the real protocol and server. The engine's events are told to the account's bridge as its drain tells them
//! (`Bridge::tell`); the mapping alone is checked through `ctl::events::map`. Every id, name and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ctl_support::{ME, UPN, WAIT, connect, launched_with_others, pump_until, pumped, quit, socket_in};
use gpui::{AppContext as _, Entity, TestAppContext};
use ngteams::ctl::events::{self, outbox_row};
use serde_json::json;
use teams_core::{
    Availability, CLOCK_SKEW_MS, CallId, CallParticipant, CallStatus, CallView, End, EngineError, Event, IncomingCall,
    LobbyPerson, MessageChange, RemoteEnd, Source,
};
use teams_ctl::ClientError;
use teams_ctl::protocol::{
    Connection, CtlEvent, ErrorCode, EventFilter, EventLine, MessageSource, OutboxState, PersonRow, ReactionRow,
    UiState, View, WindowState,
};
use teams_proto::Mri;
use teams_store::{
    Applied, Conversation, ConversationKind, Member, Message, NameSource, OutboxEntry, OutboxKind, PictureState,
    Profile, ReactionChange, Store,
};
use teams_ui::{Bridge, Calls, Desktop, Root, Shell};

use ngteams::accounts_file::{PersonalAccount, PersonalPaths};

const BEE: &str = "8:orgid:00000000-0000-4000-8000-0000000000b1";
const ONE_TO_ONE: &str = "19:00000000-0000-4000-8000-0000000000b1_00000000-0000-4000-8000-0000000000ff@unq.gbl.spaces";
const PROJECT: &str = "19:00000000000000000000000000project@thread.v2";
/// The second account, started after the first.
const OTHER_UPN: &str = "other@example.test";
const OTHER: &str = "8:orgid:00000000-0000-4000-8000-0000000000fe";
/// A message of Project's with a reaction stored.
const REACTED: &str = "5000";

fn now_ms() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

fn conversation(store: &mut Store, id: &str, kind: ConversationKind, topic: Option<&str>, members: &[&str]) {
    store
        .upsert_conversation(&Conversation {
            id: id.to_owned(),
            kind,
            topic: topic.map(str::to_owned),
            version: 1,
            last_activity: None,
        })
        .unwrap();
    let members: Vec<Member> = members.iter().map(|mri| Member::new(mri, None)).collect();
    store.replace_members(id, 1, &members).unwrap();
}

/// A 1:1 chat with Bee, the group chat "Project" with a message Bee reacted to; everyone's profile fresh, so nobody is
/// looked up (no token to look with: the engine would find the account signed out and stop).
fn seed(store: &mut Store) {
    for (mri, name) in [(ME, "Someone"), (OTHER, "Other"), (BEE, "Bee Example")] {
        store
            .put_profile(&Profile {
                mri: mri.to_owned(),
                name: Some(name.to_owned()),
                name_source: Some(NameSource::Graph),
                picture: PictureState::None,
                read_at: now_ms(),
            })
            .unwrap();
    }
    conversation(store, ONE_TO_ONE, ConversationKind::OneToOne, None, &[ME, BEE]);
    conversation(store, PROJECT, ConversationKind::Group, Some("Project"), &[ME, BEE]);
    store
        .upsert_message(&message(PROJECT, REACTED, ME, 5000, "<p>Budget</p>"), None)
        .unwrap();
    for (user, key) in [(BEE, "like"), (ME, "like"), (BEE, "heart")] {
        store
            .apply_reaction(&ReactionChange::add(PROJECT, REACTED, key, user, 6000))
            .unwrap();
    }
}

/// A message of `chat` from `sender`, arrived at `at`; one whose HTML has an `<at>` mentions the account.
fn message(chat: &str, id: &str, sender: &str, at: i64, html: &str) -> Message {
    let properties = html.contains("<at").then(|| {
        let mentions = json!([{
            "@type": "http://schema.skype.com/Mention",
            "itemid": 0,
            "mri": ME,
            "mentionType": "person",
            "displayName": "Someone",
        }]);
        json!({ "mentions": mentions.to_string() }).to_string()
    });
    Message {
        conversation_id: chat.to_owned(),
        id: id.to_owned(),
        version: at,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some(if sender == BEE { "Bee Example" } else { "Someone" }.to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: html.to_owned(),
        properties,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

/// The engine's event of `message` stored new, as `source` brought it.
fn arrived(message: Message, applied: Applied, source: Source) -> Event {
    Event::Message {
        conversation: message.conversation_id.clone(),
        id: message.id.clone(),
        change: MessageChange::New,
        applied,
        message: Some(Box::new(message)),
        source,
    }
}

/// A change to a message the engine stored (an edit, a delete, a reaction).
fn changed(chat: &str, id: &str, change: MessageChange) -> Event {
    Event::Message {
        conversation: chat.to_owned(),
        id: id.to_owned(),
        change,
        applied: Applied::Updated,
        message: None,
        source: Source::Delivery,
    }
}

fn typing(chat: &str, on: bool) -> Event {
    Event::Typing {
        conversation: chat.to_owned(),
        from: Some(BEE.to_owned()),
        typing: on,
    }
}

fn presence(mri: &str, availability: Availability) -> Event {
    Event::Presence {
        mri: mri.to_owned(),
        availability,
    }
}

fn kinds(kinds: &[&str]) -> EventFilter {
    EventFilter {
        kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
        ..EventFilter::default()
    }
}

/// A subscriber's events as its client read them, on a thread of its own; `None` once the stream ended.
struct Subscriber {
    lines: mpsc::Receiver<Option<EventLine>>,
    /// Lets a subscriber made not to read start reading.
    gate: Option<mpsc::Sender<()>>,
    thread: JoinHandle<()>,
}

struct App {
    cx: TestAppContext,
    socket: PathBuf,
    desktop: Entity<Desktop>,
    dir: tempfile::TempDir,
}

impl App {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        let cx = TestAppContext::single();
        let others = [(OTHER_UPN, "00000000-0000-4000-8000-0000000000fe")];
        let (launched, _) = launched_with_others(&cx, dir.path(), &socket, &Rc::new(Cell::new(0)), seed, &others);
        Self {
            cx,
            socket,
            desktop: launched.desktop,
            dir,
        }
    }

    /// The first account's store, opened beside its engine as the service's writes would change it.
    fn store(&self) -> Store {
        let dir = self.dir.path();
        let paths = PersonalPaths::new(dir.join("config/ngteams"), dir.join("share/ngteams"));
        let account = PersonalAccount::new(
            UPN,
            "00000000-0000-4000-8000-0000000000ff",
            "00000000-0000-4000-8000-000000000001",
        )
        .unwrap();
        Store::open(&paths.store_file(&account).unwrap()).unwrap()
    }

    /// [`App::read_until`], GPUI's clock advanced by `step` each time (its timers fire).
    fn read_advancing(
        &self,
        subscriber: &Subscriber,
        step: Duration,
        mut last: impl FnMut(&EventLine) -> bool,
    ) -> Vec<EventLine> {
        let start = Instant::now();
        let mut read = Vec::new();
        loop {
            self.cx.executor().advance_clock(step);
            self.cx.run_until_parked();
            while let Ok(line) = subscriber.lines.try_recv() {
                let Some(line) = line else { return read };
                let done = last(&line);
                read.push(line);
                if done {
                    return read;
                }
            }
            assert!(start.elapsed() < WAIT, "the events didn't come in time: {read:?}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// The shell the window shows.
    fn shell(&self) -> Entity<Shell> {
        let window = self.cx.read(|cx| self.desktop.read(cx).window().unwrap());
        self.cx.update(|cx| {
            window
                .update(cx, |kit, _, cx| {
                    let root = kit.view().clone().downcast::<Root>().unwrap();
                    root.read(cx).shell_in(cx).cloned().unwrap()
                })
                .unwrap()
        })
    }

    /// The bridge of the account at `index`.
    fn bridge(&self, index: usize) -> Entity<Bridge> {
        self.cx.read(ngteams::app::bridges)[index].clone()
    }

    /// `event` told to the account at `index` as its engine would tell it.
    fn tell(&self, index: usize, event: Event) {
        let bridge = self.bridge(index);
        self.cx
            .update(|cx| bridge.update(cx, |bridge, cx| bridge.tell(event, cx)));
    }

    /// A subscriber of `filter` for `account`; one made `gated` reads nothing until its gate opens.
    fn subscribe(&self, account: Option<&str>, filter: EventFilter, gated: bool) -> Result<Subscriber, ClientError> {
        let (socket, account) = (self.socket.clone(), account.map(str::to_owned));
        let (ready, subscribed) = mpsc::channel();
        let (lines, read) = mpsc::channel();
        let (gate, opened) = mpsc::channel::<()>();
        let thread = thread::spawn(move || {
            let events = connect(&socket).and_then(|client| client.subscribe(account.as_deref(), filter));
            let events = match events {
                Ok(events) => {
                    ready.send(Ok(())).unwrap();
                    events
                }
                Err(error) => return ready.send(Err(error)).unwrap(),
            };
            if gated {
                let _ = opened.recv();
            }
            for line in events {
                if lines.send(Some(line.unwrap())).is_err() {
                    return;
                }
            }
            let _ = lines.send(None);
        });
        let start = Instant::now();
        loop {
            self.cx.run_until_parked();
            match subscribed.try_recv() {
                Ok(result) => break result?,
                Err(mpsc::TryRecvError::Empty) => assert!(start.elapsed() < WAIT, "not subscribed in time"),
                Err(mpsc::TryRecvError::Disconnected) => panic!("the subscriber's thread ended"),
            }
            thread::sleep(Duration::from_millis(2));
        }
        Ok(Subscriber {
            lines: read,
            gate: gated.then_some(gate),
            thread,
        })
    }

    /// The events `subscriber` reads up to the first `last` lets through (that one included), or to the stream's end.
    fn read_until(&self, subscriber: &Subscriber, mut last: impl FnMut(&EventLine) -> bool) -> Vec<EventLine> {
        let start = Instant::now();
        let mut read = Vec::new();
        loop {
            self.cx.run_until_parked();
            match subscriber.lines.try_recv() {
                Ok(Some(line)) => {
                    let done = last(&line);
                    read.push(line);
                    if done {
                        return read;
                    }
                }
                Ok(None) | Err(mpsc::TryRecvError::Disconnected) => return read,
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(start.elapsed() < WAIT, "the events didn't come in time: {read:?}");
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// `subscriber`'s events up to the presence of `marker`, which every test's filter lets through; that one left out.
    fn read_to_marker(&self, subscriber: &Subscriber, marker: &str) -> Vec<CtlEvent> {
        let mut read = self.read_until(subscriber, |line| is_marker(line, marker));
        assert!(
            read.pop().is_some_and(|line| is_marker(&line, marker)),
            "the marker came"
        );
        read.into_iter().map(|line| line.event).collect()
    }

    fn mark(&self, index: usize, marker: &str) {
        self.tell(index, presence(marker, Availability::Offline));
    }
}

fn is_marker(line: &EventLine, marker: &str) -> bool {
    matches!(&line.event, CtlEvent::Presence { mri, .. } if mri == marker)
}

fn outbox_entry() -> OutboxEntry {
    OutboxEntry {
        id: 7,
        run_id: 1,
        client_message_id: "1234".to_owned(),
        conversation_id: Some(PROJECT.to_owned()),
        kind: OutboxKind::Send,
        payload: String::new(),
        state: teams_store::OutboxState::Pending,
        attempts: 0,
        attempts_at_resend: 0,
        failed_checks: 0,
        not_before: None,
        last_error: None,
        message_id: None,
        created_at: 1,
        updated_at: 1,
        unreadable_seen_at: None,
    }
}

fn participant(mri: &str, name: &str) -> CallParticipant {
    CallParticipant {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        video_on: false,
        line: None,
        speaking: false,
        muted: false,
        hand_raised: false,
        organizer: false,
        joined: None,
        sharing: false,
    }
}

fn person(mri: &str, name: &str) -> PersonRow {
    PersonRow {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
    }
}

/// One engine event of each kind a script follows, and one it doesn't, about the call `call`.
fn engine_events(call: CallId) -> Vec<Event> {
    vec![
        presence(BEE, Availability::Busy),
        presence(BEE, Availability::Other("InAMeeting".to_owned())),
        typing(ONE_TO_ONE, true),
        Event::ReadHorizon {
            conversation: ONE_TO_ONE.to_owned(),
        },
        Event::ConversationChanged {
            conversation: PROJECT.to_owned(),
        },
        Event::Teams,
        Event::Queued(outbox_entry()),
        changed(PROJECT, REACTED, MessageChange::Edit),
        changed(PROJECT, REACTED, MessageChange::Delete),
        changed(PROJECT, REACTED, MessageChange::Reaction),
        Event::Connected {
            endpoint: "made-up".to_owned(),
        },
        Event::Disconnected {
            down: teams_trouter::Down::Closed,
            heard: None,
        },
        Event::PushStopped(teams_trouter::Stop::Requested),
        Event::CaughtUp {
            cause: teams_core::Cause::Connected,
            report: teams_core::CatchUpReport::default(),
        },
        Event::CatchUpFailed {
            cause: teams_core::Cause::Retry,
            error: teams_core::CatchUpError::TooManyPages,
        },
        Event::Failed {
            during: "reading the teams",
            error: EngineError::NoEndpoint,
        },
        Event::IncomingCall(IncomingCall {
            call,
            caller: Mri::new(BEE),
            caller_name: Some("Bee Example".to_owned()),
            group_chat: Some(PROJECT.to_owned()),
        }),
        Event::RingEnded {
            call,
            end: RemoteEnd {
                phrase: Some("CallEndReasonLocalUserInitiated".to_owned()),
                ..RemoteEnd::default()
            },
        },
        Event::Call {
            call,
            status: CallStatus::Connected,
        },
        Event::Lobby {
            call,
            waiting: vec![LobbyPerson {
                mri: Mri::new(BEE),
                name: Some("Bee Example".to_owned()),
            }],
        },
        Event::Voicemail { call },
        Event::Call {
            call,
            status: CallStatus::Ended(End::HungUp),
        },
        // nothing a script follows: no event
        Event::Profile { mri: BEE.to_owned() },
    ]
}

/// The events [`engine_events`] are told as, its call's id `call`.
fn told_as(call: &str) -> Vec<CtlEvent> {
    let call = call.to_owned();
    let chat = |chat: &str| chat.to_owned();
    vec![
        CtlEvent::Presence {
            mri: BEE.to_owned(),
            availability: "busy".to_owned(),
        },
        CtlEvent::Presence {
            mri: BEE.to_owned(),
            availability: "InAMeeting".to_owned(),
        },
        CtlEvent::Typing {
            chat: chat(ONE_TO_ONE),
            who: Some(BEE.to_owned()),
            on: true,
        },
        CtlEvent::Read { chat: chat(ONE_TO_ONE) },
        CtlEvent::ChatChanged { chat: chat(PROJECT) },
        CtlEvent::TeamsChanged,
        CtlEvent::Outbox(outbox_row(&outbox_entry())),
        CtlEvent::MessageEdited {
            chat: chat(PROJECT),
            message: REACTED.to_owned(),
        },
        CtlEvent::MessageDeleted {
            chat: chat(PROJECT),
            message: REACTED.to_owned(),
        },
        CtlEvent::Reaction {
            chat: chat(PROJECT),
            message: REACTED.to_owned(),
            reactions: None,
        },
        CtlEvent::Connection {
            state: Connection::Connected,
        },
        CtlEvent::Connection {
            state: Connection::Disconnected,
        },
        CtlEvent::Connection {
            state: Connection::Stopped,
        },
        CtlEvent::CaughtUp,
        CtlEvent::CatchUpFailed {
            error: teams_core::CatchUpError::TooManyPages.to_string(),
        },
        CtlEvent::EngineFailed {
            during: "reading the teams".to_owned(),
            error: EngineError::NoEndpoint.to_string(),
        },
        CtlEvent::IncomingCall {
            call: call.clone(),
            caller: BEE.to_owned(),
            caller_name: Some("Bee Example".to_owned()),
            chat: Some(chat(PROJECT)),
        },
        CtlEvent::RingEnded {
            call: call.clone(),
            how: "CallEndReasonLocalUserInitiated".to_owned(),
        },
        CtlEvent::Call {
            call: call.clone(),
            state: "connected".to_owned(),
            reason: None,
        },
        CtlEvent::Lobby {
            call: call.clone(),
            waiting: vec![person(BEE, "Bee Example")],
        },
        CtlEvent::Voicemail { call: call.clone() },
        CtlEvent::Call {
            call,
            state: "ended".to_owned(),
            reason: Some("hung-up".to_owned()),
        },
    ]
}

/// A ring's end without its phrase is told by its code, and without either as `ended`.
#[test]
fn a_ring_end_without_its_phrase_is_told_by_its_code_else_as_ended() {
    let call = CallId::for_tests(4);
    let told = |end: RemoteEnd| events::map(&Event::RingEnded { call, end }, ME, 0, None);
    let how = |how: &str| {
        Some(CtlEvent::RingEnded {
            call: call.get().to_string(),
            how: how.to_owned(),
        })
    };
    let coded = RemoteEnd {
        code: Some(487),
        ..RemoteEnd::default()
    };
    assert_eq!(told(coded), how("487"));
    assert_eq!(told(RemoteEnd::default()), how("ended"));
}

#[test]
fn each_engine_event_is_told_as_its_kind() {
    let app = App::start();
    let all = app.subscribe(None, EventFilter::default(), false).unwrap();
    let call = CallId::for_tests(3);
    for event in engine_events(call) {
        app.tell(0, event);
    }
    app.mark(0, "marker");
    let expected = told_as(&call.get().to_string());
    let read = app.read_to_marker(&all, "marker");
    // the engine may tell something of its own meanwhile (a read without a token that failed): only ours are checked
    let ours: Vec<CtlEvent> = read.into_iter().filter(|event| expected.contains(event)).collect();
    assert_eq!(ours, expected);
    // the one a script doesn't follow (filtered out above whatever it became) is told as nothing
    let profile = Event::Profile { mri: BEE.to_owned() };
    assert_eq!(events::map(&profile, ME, 0, None), None);
    // the quit ends the stream
    quit(&app.cx);
    app.read_until(&all, |_| false);
    all.thread.join().unwrap();
}

#[test]
fn a_new_message_is_told_once_and_never_older_history_or_a_catch_up_from_before_the_cutoff() {
    let app = App::start();
    let messages = app.subscribe(None, kinds(&["message", "presence"]), false).unwrap();
    let (now, mention) = (now_ms(), r#"<p><at id="0">Someone</at> look</p>"#);
    let (new, stored) = (Applied::Inserted, Applied::Updated);
    let before_drop = [
        // a push: told, whenever it arrived; the account's own, sent elsewhere, too
        (ONE_TO_ONE, "1", BEE, 1000, mention, new, Source::Delivery),
        (ONE_TO_ONE, "2", ME, now, "<p>Mine</p>", new, Source::Delivery),
        // stored already: nothing new
        (ONE_TO_ONE, "3", BEE, now, "<p>Again</p>", stored, Source::Delivery),
        // older history, read on scrolling back
        (ONE_TO_ONE, "4", BEE, now + 1000, "<p>Old</p>", new, Source::History),
        // a catch-up's and a replay's from before the app started
        (PROJECT, "5", BEE, 2000, "<p>Before</p>", new, Source::CatchUp),
        (PROJECT, "6", BEE, 3000, "<p>Before</p>", new, Source::Replay),
        // one that arrived since: told
        (PROJECT, "7", BEE, now + 1000, "<p>Since</p>", new, Source::CatchUp),
    ];
    // the push channel was last heard at 100 s: what arrived after that, less the clock's skew, is new
    let after_drop = [
        (PROJECT, "8", BEE, 2000, "<p>Missed</p>", new, Source::Replay),
        (
            PROJECT,
            "9",
            BEE,
            100_000 - CLOCK_SKEW_MS,
            "<p>At it</p>",
            new,
            Source::Replay,
        ),
    ];
    let tell = |rows: &[(&str, &str, &str, i64, &str, Applied, Source)]| {
        for (chat, id, sender, at, html, applied, source) in rows {
            app.tell(0, arrived(message(chat, id, sender, *at, html), *applied, *source));
        }
    };
    tell(&before_drop);
    app.tell(
        0,
        Event::Disconnected {
            down: teams_trouter::Down::Closed,
            heard: Some(UNIX_EPOCH + Duration::from_secs(100)),
        },
    );
    tell(&after_drop);
    app.mark(0, "marker");
    let (bee, me) = ((BEE, "Bee Example"), (ME, "Someone"));
    let expected = [
        (ONE_TO_ONE, "1", bee, "Someone look", (false, true), MessageSource::Push),
        (ONE_TO_ONE, "2", me, "Mine", (true, false), MessageSource::Push),
        (PROJECT, "7", bee, "Since", (false, false), MessageSource::CatchUp),
        (PROJECT, "8", bee, "Missed", (false, false), MessageSource::Replay),
    ];
    let expected: Vec<CtlEvent> = expected
        .into_iter()
        .map(
            |(chat, id, (sender, name), text, (from_me, mentions_me), source)| CtlEvent::Message {
                chat: chat.to_owned(),
                message: id.to_owned(),
                sender: Some(sender.to_owned()),
                sender_name: Some(name.to_owned()),
                text: text.to_owned(),
                from_me,
                mentions_me,
                source,
            },
        )
        .collect();
    assert_eq!(app.read_to_marker(&messages, "marker"), expected);
    quit(&app.cx);
}

#[test]
fn each_filter_narrows_the_stream_and_two_subscribers_get_their_own() {
    let app = App::start();
    let prefixed = app.subscribe(None, kinds(&["message*", "presence"]), false).unwrap();
    let project = EventFilter {
        chat: Some("project".to_owned()),
        ..kinds(&["message", "typing", "presence"])
    };
    // the chat of a filter is any event's chat: the marker has none, so it comes on a subscriber of its own
    let in_project = app.subscribe(None, project, false).unwrap();
    let from = |from: &str| EventFilter {
        from: Some(from.to_owned()),
        ..EventFilter::default()
    };
    // by the name a message carries (a typing carries none), and by MRI
    let named_bee = app.subscribe(None, from("bee example"), false).unwrap();
    let from_bee = app.subscribe(None, from(BEE), false).unwrap();
    let mentions_me = EventFilter {
        mentions_me: true,
        ..EventFilter::default()
    };
    let mentioning = app.subscribe(None, mentions_me, false).unwrap();
    let (now, mention) = (now_ms(), r#"<p><at id="0">Someone</at> look</p>"#);
    for (chat, id, sender, html) in [
        (PROJECT, "1", BEE, "<p>Hi</p>"),
        (ONE_TO_ONE, "2", BEE, mention),
        (PROJECT, "3", ME, "<p>Me</p>"),
    ] {
        let message = message(chat, id, sender, now, html);
        app.tell(0, arrived(message, Applied::Inserted, Source::Delivery));
    }
    for event in [
        changed(PROJECT, "1", MessageChange::Edit),
        typing(PROJECT, true),
        typing(ONE_TO_ONE, false),
        Event::Teams,
    ] {
        app.tell(0, event);
    }
    app.mark(0, "marker");
    let read = app.read_to_marker(&prefixed, "marker");
    assert_eq!(ids(read), ["message 1", "message 2", "message 3", "edited 1"]);
    // the marker isn't Project's: the Project subscriber reads to its last event
    let read = app.read_until(&in_project, |line| matches!(line.event, CtlEvent::Typing { .. }));
    assert_eq!(ids(events(read)), ["message 1", "message 3", "typing 19:00000"]);
    let second = |line: &EventLine| matches!(&line.event, CtlEvent::Message { message, .. } if message == "2");
    let read = app.read_until(&named_bee, second);
    assert_eq!(ids(events(read)), ["message 1", "message 2"]);
    let read = app.read_until(&from_bee, |line| {
        matches!(line.event, CtlEvent::Typing { on: false, .. })
    });
    let typed = ["message 1", "message 2", "typing 19:00000", "typing 19:00000"];
    assert_eq!(ids(events(read)), typed);
    let read = app.read_until(&mentioning, |_| true);
    assert_eq!(ids(events(read)), ["message 2"]);
    quit(&app.cx);
}

fn events(lines: Vec<EventLine>) -> Vec<CtlEvent> {
    lines.into_iter().map(|line| line.event).collect()
}

/// Events by what tells them apart in these tests: a message's id, a typing's chat's start, any other's kind.
fn ids(events: impl IntoIterator<Item = CtlEvent>) -> Vec<String> {
    events
        .into_iter()
        .map(|event| match event {
            CtlEvent::Message { message, .. } => format!("message {message}"),
            CtlEvent::MessageEdited { message, .. } => format!("edited {message}"),
            CtlEvent::Typing { chat, .. } => format!("typing {}", &chat[..8]),
            other => other.kind().to_owned(),
        })
        .collect()
}

#[test]
fn a_filter_naming_no_account_no_chat_or_someone_else_s_upn_is_refused() {
    let app = App::start();
    let refused = |account: Option<&str>, filter: EventFilter| match app.subscribe(account, filter, false) {
        Err(ClientError::Server(error)) => error.code,
        Ok(_) => panic!("subscribed"),
        Err(other) => panic!("{other}"),
    };
    let nobody = EventFilter {
        account: Some("nobody@example.test".to_owned()),
        ..EventFilter::default()
    };
    assert_eq!(refused(None, nobody), ErrorCode::UnknownAccount);
    assert_eq!(refused(Some("7"), EventFilter::default()), ErrorCode::UnknownAccount);
    let no_chat = EventFilter {
        chat: Some("No such chat".to_owned()),
        ..EventFilter::default()
    };
    assert_eq!(refused(None, no_chat), ErrorCode::UnknownChat);
    // a sender by UPN only when the UPN is one of the app's accounts: no read maps anyone else's to an MRI
    let from = |from: &str| EventFilter {
        from: Some(from.to_owned()),
        ..EventFilter::default()
    };
    assert_eq!(refused(None, from("colleague@example.test")), ErrorCode::BadRequest);
    assert!(app.subscribe(None, from(&OTHER_UPN.to_uppercase()), false).is_ok());
    quit(&app.cx);
}

#[test]
fn a_reaction_carries_the_message_s_reactions_for_a_subscriber_asking_for_detail() {
    let app = App::start();
    let detailed = EventFilter {
        detail: true,
        ..kinds(&["reaction"])
    };
    let detailed = app.subscribe(None, detailed, false).unwrap();
    let plain = app.subscribe(None, kinds(&["reaction"]), false).unwrap();
    app.tell(0, changed(PROJECT, REACTED, MessageChange::Reaction));
    let read = app.read_until(&detailed, |_| true);
    let rows = |rows: &[(&str, u64, bool)]| {
        rows.iter()
            .map(|(reaction, count, mine)| ReactionRow {
                reaction: (*reaction).to_owned(),
                count: *count,
                mine: *mine,
            })
            .collect::<Vec<_>>()
    };
    let CtlEvent::Reaction { reactions, .. } = &read[0].event else {
        panic!("{read:?}");
    };
    let mut reactions = reactions.clone().unwrap();
    reactions.sort_by(|a, b| a.reaction.cmp(&b.reaction));
    assert_eq!(reactions, rows(&[("heart", 1, false), ("like", 2, true)]));
    let read = app.read_until(&plain, |_| true);
    assert!(matches!(read[0].event, CtlEvent::Reaction { reactions: None, .. }));
    quit(&app.cx);
}

#[test]
fn a_slow_subscriber_is_dropped_with_lagged_while_the_others_and_the_drain_go_on() {
    let app = App::start();
    let slow = app.subscribe(None, kinds(&["typing"]), true).unwrap();
    let keeping_up = app.subscribe(None, kinds(&["typing"]), false).unwrap();
    // far more than the slow one's buffer and its socket's take, told in steps the other one reads in full
    let (steps, each) = (10, events::BUFFERED);
    for _ in 0..steps {
        for _ in 0..each {
            app.tell(0, typing(PROJECT, true));
        }
        let read = app.read_until(&keeping_up, {
            let mut count = 0;
            move |_| {
                count += 1;
                count == each
            }
        });
        assert_eq!(read.len(), each, "the one that keeps up gets every event");
    }
    // the drain went on: the models applied what came after
    app.tell(0, presence(BEE, Availability::Away));
    let bridge = app.bridge(0);
    let shown = app.cx.read(|cx| bridge.read(cx).models().presence.read(cx).of(BEE));
    assert_eq!(shown, Availability::Away);

    slow.gate.as_ref().unwrap().send(()).unwrap();
    let read = app.read_until(&slow, |_| false);
    assert!(read.len() < steps * each, "events were dropped: {}", read.len());
    assert_eq!(read.last().unwrap().event, CtlEvent::Lagged, "its last line says so");
    assert_eq!(read.last().unwrap().account, None);
    slow.thread.join().unwrap();
    quit(&app.cx);
}

#[test]
fn an_account_started_after_the_first_is_tapped_and_filtered_by_account() {
    let app = App::start();
    let second = app.bridge(1);
    assert_eq!(app.cx.read(|cx| second.read(cx).core().me().to_owned()), OTHER);
    let everyone = app.subscribe(None, kinds(&["presence"]), false).unwrap();
    let other = EventFilter {
        account: Some(OTHER_UPN.to_uppercase()),
        ..kinds(&["presence"])
    };
    let other = app.subscribe(None, other, false).unwrap();
    let by_index = app.subscribe(Some("1"), kinds(&["presence"]), false).unwrap();
    app.tell(0, presence(BEE, Availability::Busy));
    app.tell(1, presence(BEE, Availability::Away));
    let read = app.read_until(&everyone, |line| line.account.as_deref() == Some(OTHER_UPN));
    let accounts: Vec<Option<&str>> = read.iter().map(|line| line.account.as_deref()).collect();
    assert_eq!(accounts, [Some(UPN), Some(OTHER_UPN)]);
    for subscriber in [&other, &by_index] {
        let read = app.read_until(subscriber, |_| true);
        assert_eq!(read[0].account.as_deref(), Some(OTHER_UPN));
        assert_eq!(
            read[0].event,
            CtlEvent::Presence {
                mri: BEE.to_owned(),
                availability: "away".to_owned()
            }
        );
    }
    // the stream ends with the app
    quit(&app.cx);
    for subscriber in [everyone, other, by_index] {
        assert!(app.read_until(&subscriber, |_| false).is_empty(), "nothing more came");
        subscriber.thread.join().unwrap();
    }
}

#[test]
fn call_participants_tell_who_joined_and_who_left_since_the_view_before() {
    let cx = TestAppContext::single();
    let calls = cx.update(|cx| cx.new(|_| Calls::shown(None)));
    let call = CallId::for_tests(9);
    let apply = |event: &Event| cx.update(|cx| calls.update(cx, |calls, cx| calls.apply(event, cx)));
    let view = |people: &[(&str, &str)]| Event::CallParticipants {
        call,
        view: CallView {
            participants: people.iter().map(|(mri, name)| participant(mri, name)).collect(),
            ..CallView::default()
        },
    };
    let mapped = |event: &Event| cx.read(|cx| events::map(event, ME, 0, calls.read(cx).call()));
    apply(&Event::Call {
        call,
        status: CallStatus::Connected,
    });
    let first = view(&[(BEE, "Bee Example")]);
    assert_eq!(
        mapped(&first),
        Some(CtlEvent::CallParticipants {
            call: call.get().to_string(),
            joined: vec![person(BEE, "Bee Example")],
            left: Vec::new(),
        })
    );
    apply(&first);
    let cee = "8:orgid:00000000-0000-4000-8000-0000000000c1";
    let second = view(&[(cee, "Cee Example")]);
    assert_eq!(
        mapped(&second),
        Some(CtlEvent::CallParticipants {
            call: call.get().to_string(),
            joined: vec![person(cee, "Cee Example")],
            left: vec![person(BEE, "Bee Example")],
        })
    );
    // another call's view: everyone in it joined, nobody left
    let other = CallId::for_tests(10);
    let others = Event::CallParticipants {
        call: other,
        view: CallView {
            participants: vec![participant(BEE, "Bee Example")],
            ..CallView::default()
        },
    };
    assert_eq!(
        mapped(&others),
        Some(CtlEvent::CallParticipants {
            call: other.get().to_string(),
            joined: vec![person(BEE, "Bee Example")],
            left: Vec::new(),
        })
    );
}

#[test]
fn a_subscriber_whose_client_hung_up_stops_listening_though_no_event_passes_its_filter() {
    let app = App::start();
    let socket = app.socket.clone();
    // a filter nothing told here passes
    let filter = EventFilter {
        chat: Some("project".to_owned()),
        ..kinds(&["incoming_call"])
    };
    let events = pumped(&app.cx, move || connect(&socket)?.subscribe(None, filter)).unwrap();
    let listening = || app.cx.read(ngteams::app::ctl_listening);
    assert!(listening(), "subscribed");
    // events its filter refuses don't drop a subscriber still there
    app.tell(0, typing(PROJECT, true));
    app.mark(0, "marker");
    app.cx.run_until_parked();
    assert!(listening(), "still there");

    drop(events);
    pump_until(&app.cx, Duration::from_millis(1), |cx| !ngteams::app::ctl_listening(cx));
    quit(&app.cx);
}

#[test]
fn an_outbox_row_is_told_as_where_it_stands() {
    use teams_store::OutboxState as Stored;

    let state = |state, may_have_arrived: bool| {
        let entry = OutboxEntry {
            state,
            unreadable_seen_at: may_have_arrived.then_some(1),
            ..outbox_entry()
        };
        outbox_row(&entry).state
    };
    assert_eq!(state(Stored::Pending, false), OutboxState::Queued);
    assert_eq!(state(Stored::Sending, false), OutboxState::Unknown);
    assert_eq!(state(Stored::Sent, false), OutboxState::Sent);
    assert_eq!(state(Stored::Denied, false), OutboxState::Failed);
    assert_eq!(state(Stored::Failed, false), OutboxState::Failed);
    assert_eq!(
        state(Stored::Pending, true),
        OutboxState::Unknown,
        "it may have arrived"
    );
    assert_eq!(state(Stored::Sent, true), OutboxState::Sent);
    let row = outbox_row(&outbox_entry());
    assert_eq!(
        (row.id, row.write.as_str(), row.chat.as_deref()),
        (7, "send", Some(PROJECT))
    );
}

fn unread(chat: Option<&str>, count: u64, total: u64) -> CtlEvent {
    CtlEvent::Unread {
        chat: chat.map(str::to_owned),
        count,
        total,
    }
}

/// Bee's messages `ids` of `chat`, stored as the service's delivery would store them, and the account's engine telling
/// the chat changed.
fn delivered(app: &App, store: &mut Store, chat: &str, ids: &[&str]) {
    for id in ids {
        let at = now_ms();
        store
            .upsert_message(&message(chat, id, BEE, at, "<p>Hey</p>"), None)
            .unwrap();
    }
    app.tell(
        0,
        Event::ConversationChanged {
            conversation: chat.to_owned(),
        },
    );
}

const STEP: Duration = Duration::from_millis(100);

#[test]
fn unread_is_told_as_the_badge_counts_each_chat_up_and_down_with_the_total() {
    let app = App::start();
    let tray = app.cx.read(|cx| {
        let rail = app.desktop.read(cx).accounts().read(cx);
        rail.tile(ME).unwrap().tray.clone()
    });
    // the badge's first count, before anyone listens: nothing is unread
    pump_until(&app.cx, STEP, |cx| tray.read(cx).total() == Some(0));
    let counts = app.subscribe(None, kinds(&["unread"]), false).unwrap();
    let mut store = app.store();

    delivered(&app, &mut store, ONE_TO_ONE, &["11"]);
    let read = app.read_advancing(&counts, STEP, |_| true);
    assert_eq!(events(read), [unread(Some(ONE_TO_ONE), 1, 1)]);
    delivered(&app, &mut store, PROJECT, &["12", "13"]);
    let read = app.read_advancing(&counts, STEP, |_| true);
    assert_eq!(events(read), [unread(Some(PROJECT), 2, 2)]);
    // a chat's count up, the total as it was
    delivered(&app, &mut store, ONE_TO_ONE, &["14"]);
    let read = app.read_advancing(&counts, STEP, |_| true);
    assert_eq!(events(read), [unread(Some(ONE_TO_ONE), 2, 2)]);
    // the 1:1 chat read: down to nothing, the total with it
    let horizon = teams_store::ReadHorizon {
        conversation_id: ONE_TO_ONE.to_owned(),
        user: ME.to_owned(),
        last_read_arrival: now_ms(),
        updated: now_ms(),
        raw: None,
    };
    store.set_read_horizon(&horizon).unwrap();
    app.tell(
        0,
        Event::ReadHorizon {
            conversation: ONE_TO_ONE.to_owned(),
        },
    );
    let read = app.read_advancing(&counts, STEP, |_| true);
    assert_eq!(read[0].account.as_deref(), Some(UPN));
    assert_eq!(events(read), [unread(Some(ONE_TO_ONE), 0, 1)]);
    quit(&app.cx);
}

#[test]
fn unread_changes_name_each_chat_that_changed_and_a_first_count_alone() {
    use ngteams::ctl::watch::{Counts, unread_changes};

    let counts = |chats: &[(&str, u64)], total: Option<u64>| Counts {
        chats: chats.iter().map(|(chat, count)| ((*chat).to_owned(), *count)).collect(),
        total,
    };
    let none = counts(&[], None);
    assert_eq!(unread_changes(&none, &counts(&[], Some(0))), [unread(None, 0, 0)]);
    let one = counts(&[("a", 1)], Some(1));
    let two = counts(&[("a", 3), ("b", 1)], Some(2));
    assert_eq!(
        unread_changes(&one, &two),
        [unread(Some("a"), 3, 2), unread(Some("b"), 1, 2)]
    );
    assert_eq!(
        unread_changes(&two, &counts(&[("b", 1)], Some(1))),
        [unread(Some("a"), 0, 1)]
    );
    assert!(unread_changes(&two, &two).is_empty());
}

fn statuses(lines: Vec<EventLine>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| match line.event {
            CtlEvent::OwnStatus { availability } => availability,
            other => panic!("not an own status: {other:?}"),
        })
        .collect()
}

#[test]
fn own_status_is_told_as_the_idle_policy_and_the_status_menu_set_it() {
    let app = App::start();
    let settings = app.cx.read(|cx| app.desktop.read(cx).settings().clone());
    app.cx
        .update(|cx| settings.update(cx, teams_ui::SettingsModel::confirm_first_run));
    let own = app.subscribe(None, kinds(&["own_status"]), false).unwrap();
    let registered = || app.tell(0, Event::Registration(teams_core::RegistrationReport::Registered));

    // automatic, the window used since the start: the idle policy reports available
    registered();
    assert_eq!(statuses(app.read_advancing(&own, STEP, |_| true)), ["available"]);
    // the engine has no endpoint in fact (no push channel here): it refuses, and the next tick says so
    let next = |app: &App| statuses(app.read_advancing(&own, Duration::from_secs(1), |_| true));
    assert_eq!(next(&app), ["connecting"]);
    // the status menu's busy, reported once registered again
    let shell = app.shell();
    app.cx.update(|cx| {
        shell.update(cx, |shell, cx| {
            shell.choose_status(teams_ui::settings::ManualStatus::Busy, cx);
        });
    });
    registered();
    assert_eq!(statuses(app.read_advancing(&own, STEP, |_| true)), ["busy"]);
    assert_eq!(next(&app), ["connecting"]);
    quit(&app.cx);
}

#[test]
fn reporting_is_named_as_the_wire_tells_it() {
    use ngteams::ctl::watch::reporting_name;
    use teams_core::{Choice, Reporting};
    use teams_proto::ManualAvailability;

    let named = [
        (Reporting::Connecting, "connecting"),
        (Reporting::Available, "available"),
        (Reporting::Manual(ManualAvailability::Available), "available"),
        (Reporting::Manual(ManualAvailability::Busy), "busy"),
        (Reporting::Manual(ManualAvailability::DoNotDisturb), "do-not-disturb"),
        (Reporting::Manual(ManualAvailability::Away), "away"),
        (Reporting::NotSet(Choice::Automatic), "not-set"),
        (Reporting::Inactive, "inactive"),
        (Reporting::Halted, "halted"),
        (Reporting::Quit, "stopped"),
    ];
    for (reporting, name) in named {
        assert_eq!(reporting_name(reporting), name);
    }
}

/// A notification server that shows everything, each with an id of its own.
#[derive(Default)]
struct Shows(std::sync::atomic::AtomicU32);

impl teams_ui::NotificationServer for Shows {
    fn show(
        &self,
        _: &teams_ui::Notice,
        _: u32,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<u32, String>> + Send>> {
        let id = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Box::pin(async move { Ok(id) })
    }

    fn close(&self, _: u32) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

#[test]
fn a_notification_shown_is_told_with_its_title_and_chat() {
    let app = App::start();
    app.cx.update(|cx| {
        app.desktop
            .update(cx, |desktop, cx| desktop.use_server(Arc::new(Shows::default()), cx));
    });
    let shown = app.subscribe(None, kinds(&["notification"]), false).unwrap();
    let lunch = message(PROJECT, "21", BEE, now_ms(), "<p>Lunch?</p>");
    app.tell(0, arrived(lunch, Applied::Inserted, Source::Delivery));
    let read = app.read_advancing(&shown, Duration::from_millis(50), |_| true);
    assert_eq!(read[0].account.as_deref(), Some(UPN));
    let CtlEvent::Notification { title, chat } = &read[0].event else {
        panic!("a notification: {read:?}");
    };
    assert_eq!(
        (title.as_str(), chat.as_deref()),
        ("Bee Example in Project", Some(PROJECT))
    );
    quit(&app.cx);
}

fn shown(lines: &[EventLine]) -> Vec<(Option<String>, UiState)> {
    lines
        .iter()
        .map(|line| match &line.event {
            CtlEvent::Ui { state } => (line.account.clone(), state.clone()),
            other => panic!("not a ui event: {other:?}"),
        })
        .collect()
}

#[test]
fn navigation_is_told_as_what_the_window_shows() {
    let app = App::start();
    let ui = app.subscribe(None, kinds(&["ui"]), false).unwrap();
    let in_view =
        |view: View| move |line: &EventLine| matches!(&line.event, CtlEvent::Ui { state } if state.view == view);
    // the chat list loaded, which names the chat
    let shell = app.shell();
    let list = app.cx.read(|cx| shell.read(cx).list().clone());
    pump_until(&app.cx, STEP, |cx| list.read(cx).row(PROJECT).is_some());

    app.cx.update(|cx| {
        app.desktop
            .update(cx, |desktop, cx| desktop.open_chat_of(ME, PROJECT.to_owned(), cx));
    });
    let read = app.read_until(&ui, in_view(View::Chat));
    let project = UiState {
        shown: Some(UPN.to_owned()),
        view: View::Chat,
        chat: Some(PROJECT.to_owned()),
        chat_title: Some("Project".to_owned()),
        window: WindowState::Shown,
        call_window: false,
        activity: false,
    };
    assert_eq!(shown(&read).last(), Some(&(Some(UPN.to_owned()), project.clone())));

    // the chat renamed: its list names it anew, the shell left as it was
    let mut store = app.store();
    store
        .upsert_conversation(&Conversation {
            id: PROJECT.to_owned(),
            kind: ConversationKind::Group,
            topic: Some("Budget".to_owned()),
            version: 2,
            last_activity: None,
        })
        .unwrap();
    app.tell(
        0,
        Event::ConversationChanged {
            conversation: PROJECT.to_owned(),
        },
    );
    let read = app.read_until(
        &ui,
        |line| matches!(&line.event, CtlEvent::Ui { state } if state.chat_title.as_deref() == Some("Budget")),
    );
    let budget = UiState {
        chat_title: Some("Budget".to_owned()),
        ..project
    };
    assert_eq!(shown(&read).last(), Some(&(Some(UPN.to_owned()), budget)));

    let window = app.cx.read(|cx| app.desktop.read(cx).window().unwrap());
    let shell = app.shell();
    app.cx.update(|cx| {
        window
            .update(cx, |_, window, cx| {
                shell.update(cx, |shell, cx| {
                    shell.open_settings_page(teams_ui::settings_view::Page::General, window, cx);
                });
            })
            .unwrap();
    });
    let read = app.read_until(&ui, in_view(View::Settings));
    let (account, state) = shown(&read).pop().unwrap();
    assert_eq!((account.as_deref(), state.chat), (Some(UPN), None));

    // the other account shown: told as its own
    app.cx
        .update(|cx| app.desktop.update(cx, |desktop, cx| desktop.switch(OTHER, cx)));
    let read = app.read_until(&ui, |line| line.account.as_deref() == Some(OTHER_UPN));
    let (_, state) = shown(&read).pop().unwrap();
    assert_eq!(state.shown.as_deref(), Some(OTHER_UPN));
    quit(&app.cx);
}

#[test]
fn a_subscriber_wanting_meetings_starts_the_ticker_and_is_told_a_stored_meeting() {
    let app = App::start();
    let now = app.cx.read(ngteams::ctl::ticker::wall_ms);
    let start = now + 5 * 60_000;
    let at = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let entry = json!({
        "objectId": "stored-standup",
        "subject": "Stored standup",
        "startTime": at(start),
        "endTime": at(start + 30 * 60_000),
    });
    app.store()
        .keep_calendar_events(&[teams_store::StoredEvent {
            id: "stored-standup".to_owned(),
            start_ms: start,
            end_ms: start + 30 * 60_000,
            entry: entry.to_string(),
        }])
        .unwrap();
    // a subscriber that wants no meeting starts no ticker: the next one does
    let typing = app.subscribe(None, kinds(&["typing"]), false).unwrap();
    let meetings = app.subscribe(None, kinds(&["meeting_*"]), false).unwrap();
    let read = app.read_until(&meetings, |line| matches!(line.event, CtlEvent::MeetingStarting { .. }));
    let Some(CtlEvent::MeetingStarting { id, minutes, .. }) = read.last().map(|line| &line.event) else {
        panic!("no meeting told: {read:?}");
    };
    assert_eq!((id.as_str(), *minutes), ("stored-standup", 5));
    drop((typing, meetings));
    quit(&app.cx);
}

#[test]
fn a_sign_out_is_told_as_the_account_s() {
    let app = App::start();
    let out = app.subscribe(None, kinds(&["signed_out", "presence"]), false).unwrap();
    let bridge = app.bridge(1);
    app.cx.update(|cx| {
        let stopped = bridge.update(cx, |bridge, cx| bridge.sign_out("Signed out".into(), cx));
        stopped.detach();
    });
    app.mark(0, "marker");
    let read = app.read_until(&out, |line| is_marker(line, "marker"));
    let told: Vec<(Option<&str>, &CtlEvent)> = read
        .iter()
        .filter(|line| !is_marker(line, "marker"))
        .map(|line| (line.account.as_deref(), &line.event))
        .collect();
    assert_eq!(told, [(Some(OTHER_UPN), &CtlEvent::SignedOut)]);
    quit(&app.cx);
}

/// The ticker over a calendar of its own (a core that answers the calendar alone), with the hub as `subscribe` fills it.
mod ticking {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ngteams::ctl::events::{Accounts, Hub, Origin, Wanted};
    use ngteams::ctl::ticker::{self, TICK};
    use teams_core::sync::{CatchUpReport, Synced};
    use teams_core::{
        Action, Answer, CalendarEvent, FileStream, OwnStatus, Person, PictureBytes, Reply, Strip, Transient,
        TransientSent,
    };
    use teams_proto::EventTime;
    use teams_store::{MessagePage, PageCursor, Reaction, ReadHorizon};
    use teams_ui::Core;

    use super::*;

    /// The account's calendar: what the service gives and the store keeps are the same here. One `held` never answers
    /// a read from the service (as the engine holds it through a catch-up).
    struct Calendar {
        events: Mutex<Vec<CalendarEvent>>,
        reads: AtomicUsize,
        held: Option<Mutex<Vec<Answer<Vec<CalendarEvent>>>>>,
    }

    impl Calendar {
        fn new(events: Vec<CalendarEvent>, held: bool) -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(events),
                reads: AtomicUsize::new(0),
                held: held.then(|| Mutex::new(Vec::new())),
            })
        }

        fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    fn stopped<T>() -> Reply<T> {
        Reply::ready(Err(EngineError::Stopped))
    }

    macro_rules! stopped {
        ($($name:ident($($arg:ty),*) -> $out:ty;)*) => {
            $(fn $name(&self, $(_: $arg),*) -> Reply<$out> {
                stopped()
            })*
        };
    }

    impl Core for Calendar {
        fn me(&self) -> &str {
            ME
        }

        fn calendar(&self, _: i64, _: i64) -> Reply<Vec<CalendarEvent>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            match &self.held {
                Some(held) => {
                    let (answer, reply) = Reply::later();
                    held.lock().unwrap().push(answer);
                    reply
                }
                None => Reply::ready(Ok(self.events.lock().unwrap().clone())),
            }
        }

        fn stored_calendar(&self, start: i64, end: i64) -> Reply<Vec<CalendarEvent>> {
            let events = self.events.lock().unwrap();
            let within = |event: &&CalendarEvent| {
                let at = teams_core::calendar::instant(event.start);
                (start..end).contains(&at)
            };
            Reply::ready(Ok(events.iter().filter(within).cloned().collect()))
        }

        stopped! {
            write(Action) -> OutboxEntry;
            resend(i64) -> OutboxEntry;
            transient(Transient) -> TransientSent;
            own_availability(OwnStatus) -> TransientSent;
            sync() -> CatchUpReport;
            load_older(String) -> Option<Synced>;
            conversations(usize) -> Vec<Conversation>;
            messages(String, Option<PageCursor>, usize) -> MessagePage;
            message(String, String) -> Option<Message>;
            reactions(String, String) -> Vec<Reaction>;
            members(String) -> Vec<Member>;
            unread_count(String) -> u64;
            person(String) -> Person;
            presence(String) -> Availability;
            reaction_animation(String) -> Option<Strip>;
            message_picture(String) -> Option<PictureBytes>;
            file_thumbnail(String) -> Option<PictureBytes>;
            file_stream(String, u64, u64) -> FileStream;
            file_size(String) -> u64;
            recent_reactions(usize) -> Vec<String>;
            frequent_reactions(usize) -> Vec<String>;
            read_horizon(String) -> Option<ReadHorizon>;
            history_complete(String) -> Option<bool>;
            outbox_entry(i64) -> Option<OutboxEntry>;
            outbox_unsettled(String) -> Vec<OutboxEntry>;
            left_by_earlier_runs() -> Vec<OutboxEntry>;
            search(String, Vec<String>, usize) -> Vec<teams_store::SearchHit>;
            teams() -> Option<Vec<teams_store::Team>>;
            chat_listing() -> teams_core::ChatListing;
            sync_conversation(String) -> Synced;
            reply_roots(String) -> Vec<(String, String)>;
            call(teams_core::CallAction) -> CallId;
        }
    }

    /// An event `id` starting `minutes` after `now`, a Teams meeting when `online`.
    fn event(id: &str, now: i64, minutes: i64, online: bool) -> CalendarEvent {
        let entry = json!({
            "objectId": id,
            "subject": format!("Subject {id}"),
            "startTime": "2025-07-01T00:00:00Z",
            "endTime": "2025-07-01T00:30:00Z",
            "isOnlineMeeting": online,
        });
        let mut event = CalendarEvent::stored(&entry.to_string()).unwrap();
        event.start = EventTime::Instant(now + minutes * 60_000);
        event.end = EventTime::Instant(now + (minutes + 30) * 60_000);
        event
    }

    fn told(stream: &events::Stream) -> Vec<(String, i64)> {
        std::iter::from_fn(|| stream.try_recv().ok())
            .map(|line| match line.event {
                CtlEvent::MeetingStarting {
                    id,
                    minutes,
                    subject,
                    joinable,
                    ..
                } => {
                    assert_eq!(subject, format!("Subject {id}"));
                    assert_eq!(joinable, id == "standup", "{id}");
                    (id, minutes)
                }
                other => panic!("not a meeting: {other:?}"),
            })
            .collect()
    }

    fn wanting(leads: &[u32]) -> Wanted {
        Wanted {
            filter: EventFilter {
                leads: leads.to_vec(),
                ..kinds(&["meeting_starting"])
            },
            ..Wanted::default()
        }
    }

    #[test]
    fn a_meeting_is_told_once_at_each_lead_and_the_ticker_runs_only_while_wanted() {
        let cx = TestAppContext::single();
        let now = cx.read(ticker::wall_ms);
        let mut cancelled = event("cancelled", now, 6, true);
        cancelled.cancelled = true;
        let mut all_day = event("all-day", now, 6, false);
        all_day.all_day = true;
        let calendar = Calendar::new(
            vec![
                event("standup", now, 7, true),
                event("lunch", now, 9, false),
                cancelled,
                all_day,
            ],
            false,
        );
        let accounts = accounts(&[(ME, &calendar)]);
        let hub = Rc::new(Hub::default());

        // nobody wants meetings: the ticker stops at once, reading nothing, and stays stopped
        cx.update(|cx| ticker::start(&hub, accounts.clone(), cx));
        cx.run_until_parked();
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        assert_eq!(calendar.reads(), 0);

        let defaults = hub.subscribe(wanting(&[]));
        let six = hub.subscribe(wanting(&[6]));
        // a lead whose moment passed before it subscribed: never told late
        let ten = hub.subscribe(wanting(&[10]));
        cx.update(|cx| ticker::start(&hub, accounts.clone(), cx));
        cx.run_until_parked();
        assert_eq!(calendar.reads(), 1, "it runs: the calendar read at its start");
        // a second start while it runs starts nothing more: its reads stay one ticker's
        cx.update(|cx| ticker::start(&hub, accounts.clone(), cx));
        cx.run_until_parked();
        assert_eq!(calendar.reads(), 1);
        for _ in 0..(10 * 60 / TICK.as_secs()) {
            cx.executor().advance_clock(TICK);
            cx.run_until_parked();
        }
        assert_eq!(
            told(&defaults),
            [
                ("standup".to_owned(), 5),
                ("lunch".to_owned(), 5),
                ("standup".to_owned(), 0),
                ("lunch".to_owned(), 0),
            ]
        );
        assert_eq!(told(&six), [("standup".to_owned(), 6), ("lunch".to_owned(), 6)]);
        assert!(told(&ten).is_empty());
        // read from the service at the start and every five minutes since
        assert_eq!(calendar.reads(), 3);

        // everyone gone: the ticker stops at its next tick, reading nothing more however long it is left
        drop((defaults, six, ten));
        for _ in 0..(10 * 60 / TICK.as_secs()) {
            cx.executor().advance_clock(TICK);
            cx.run_until_parked();
        }
        assert_eq!(calendar.reads(), 3);
        // stopped, it starts again for the next subscriber that wants meetings
        let again = hub.subscribe(wanting(&[]));
        cx.update(|cx| ticker::start(&hub, accounts.clone(), cx));
        cx.run_until_parked();
        assert_eq!(calendar.reads(), 4);
        drop(again);
    }

    /// The accounts the ticker reads: each MRI's calendar.
    fn accounts(calendars: &[(&str, &Arc<Calendar>)]) -> Accounts {
        let origins: Vec<Origin> = calendars
            .iter()
            .map(|(mri, calendar)| Origin {
                upn: format!("{mri}@example.test"),
                mri: (*mri).to_owned(),
                core: Arc::clone(calendar) as Arc<dyn Core>,
            })
            .collect();
        Rc::new(move |_| origins.clone())
    }

    #[test]
    fn a_read_from_the_service_that_never_answers_holds_up_no_meeting() {
        let cx = TestAppContext::single();
        let now = cx.read(ticker::wall_ms);
        // the first account's read is held (a catch-up the engine is in), the other's answers
        let held = Calendar::new(vec![event("standup", now, 7, true)], true);
        let answered = Calendar::new(vec![event("lunch", now, 9, false)], false);
        let hub = Rc::new(Hub::default());
        let defaults = hub.subscribe(wanting(&[]));
        cx.update(|cx| ticker::start(&hub, accounts(&[(ME, &held), (OTHER, &answered)]), cx));
        cx.run_until_parked();
        for _ in 0..(10 * 60 / TICK.as_secs()) {
            cx.executor().advance_clock(TICK);
            cx.run_until_parked();
        }
        // the held account's stored meeting and the other's both told on time
        assert_eq!(
            told(&defaults),
            [
                ("standup".to_owned(), 5),
                ("lunch".to_owned(), 5),
                ("standup".to_owned(), 0),
                ("lunch".to_owned(), 0),
            ]
        );
        // one read at a time: the held one is never read again while it runs
        assert_eq!((held.reads(), answered.reads()), (1, 3));

        // a subscriber looking further ahead than the last read reached: read again at the next tick
        let hour = hub.subscribe(wanting(&[60]));
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        assert_eq!((held.reads(), answered.reads()), (1, 4));
        drop((defaults, hour));
    }
}
