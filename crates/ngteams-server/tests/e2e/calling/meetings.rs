//! Meetings on the group call controller: the client's engines (`teams-core`, `teams-media` on the file device
//! backend) join a meeting scheduled on the server's calendar, are pushed its start, wait in its lobby until someone
//! lets them in, raise hands, react, are muted by the organizer and see the meeting ended for all; a channel's meeting
//! is posted to its channel, titled and joined the same way. Every account and meeting is synthetic.

use reqwest::Method;
use serde_json::json;
use teams_core::engine::Event;
use teams_core::{CallAction, CallId, CallStatus, End, EventWrite, MeetingStartKind};
use teams_media::tone::Tone;
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, MIDDLE_TIER_AUDIENCE, MeetingInfo, Mri};

use super::engines::{A_TONE, B_TONE, Engine, Screen};
use super::group_media::{C_TONE, Mixed};
use crate::calendar::meetings::invited;
use crate::calendar::{MONDAY, draft};
use crate::support::backends;
use crate::trouter::Account;

/// An account of the server with its engine: what an invitation names it by.
pub(super) struct Person {
    pub(super) engine: Engine,
    pub(super) upn: String,
    pub(super) name: String,
}

impl Person {
    pub(super) async fn new(mixed: &Mixed, name: &str, tone: Tone) -> Self {
        Self::sharing(mixed, name, tone, Screen::Moving).await
    }

    /// [`Person::new`] whose engine shares `screen`.
    pub(super) async fn sharing(mixed: &Mixed, name: &str, tone: Tone, screen: Screen) -> Self {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let upn = format!("{}-{tag}@own.test", name.to_lowercase());
        let id = mixed.server().add_user(&upn, name).await;
        let account = Account::sign_in(mixed.server(), &upn, &id).await;
        let nodes = (mixed.server(), mixed.server());
        let engine = Engine::start_served_sharing(&account, name, nodes, (tone, mixed.dir(), screen)).await;
        Self {
            engine,
            upn,
            name: name.to_owned(),
        }
    }

    /// The account's own id (its MRI's object id).
    pub(super) fn id(&self) -> &str {
        self.engine.mri.trim_start_matches("8:orgid:")
    }
}

/// A Teams meeting `organizer` schedules on its calendar, inviting `guests`: its event (its chat and meeting).
pub(super) async fn schedule(organizer: &Person, guests: &[&Person]) -> CalendarEvent {
    let mut meeting = draft("Planning", MONDAY + 9 * crate::calendar::HOUR, 30);
    meeting.online = true;
    meeting.attendees = guests
        .iter()
        .map(|guest| invited(&guest.upn, &guest.name, None, true))
        .collect();
    let written = organizer
        .engine
        .handle
        .write_event(EventWrite::Create(meeting))
        .await
        .unwrap();
    let event = written.created.unwrap();
    for person in std::iter::once(organizer).chain(guests.iter().copied()) {
        assert!(person.engine.handle.sync().await.unwrap().failed.is_empty());
    }
    event
}

/// What joining the meeting `event` takes: its chat and its organizer.
pub(super) fn join_of(event: &CalendarEvent) -> CallAction {
    CallAction::JoinMeeting {
        chat: event.meeting_chat.clone().expect("a Teams meeting has a chat"),
        meeting: event.meeting.clone().expect("a Teams meeting names its organizer"),
    }
}

/// `person` joins the meeting by `join` and is in it with its media up.
pub(super) async fn joined(person: &mut Person, join: &CallAction) -> CallId {
    let call = person.engine.call(join.clone()).await;
    person.engine.status(call, &CallStatus::Connected).await;
    call
}

/// The `Event/Call` messages of `chat`, newest first, as `reader` reads them.
pub(super) async fn call_events(mixed: &Mixed, reader: &Person, chat: &str) -> Vec<String> {
    let client = reader.engine.account.client(mixed.server());
    let page = ChatReads::new(&client).messages(chat, 20).await.unwrap();
    page.messages
        .iter()
        .filter(|message| message.message_type.as_str() == "Event/Call")
        .map(|message| message.content.clone())
        .collect()
}

/// The `Event/Call` messages of `chat` once the newest tells the call's end, which the chat gets just after the
/// parties hear it (or as they are when that takes too long).
pub(super) async fn ended_events(mixed: &Mixed, reader: &Person, chat: &str) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let events = call_events(mixed, reader, chat).await;
        let ended = events.first().is_some_and(|newest| newest.contains("<ended/>"));
        if ended || tokio::time::Instant::now() >= deadline {
            return events;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Each person hangs up its call and waits for its end.
pub(super) async fn leave_all(people: &mut [&mut Person], calls: &[CallId]) {
    for (person, call) in people.iter_mut().zip(calls) {
        person.engine.call(CallAction::HangUp { call: *call }).await;
        person.engine.ended(*call).await;
    }
}

pub(super) async fn shut_down(people: Vec<Person>, mixed: Mixed) {
    for person in people {
        person.engine.shut_down().await;
    }
    mixed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_join_a_scheduled_meeting_nobody_rings_and_its_chat_tells_its_start_and_end() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        assert_eq!(
            event.meeting,
            Some(MeetingInfo {
                organizer_id: ada.id().to_owned(),
                tenant_id: event.meeting.clone().unwrap().tenant_id,
            })
        );
        // a guest may start the meeting's call before its organizer comes
        let b_call = joined(&mut bert, &join).await;
        let chat = event.meeting_chat.clone().unwrap();
        told_started(&mut [&mut ada, &mut cora], &chat, &bert.engine.mri.clone()).await;
        let a_call = joined(&mut ada, &join).await;
        let c_call = joined(&mut cora, &join).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        // each wait passes only on a view with the parties that came later, so not on a view still in the queue from
        // before: bert was in before ada and cora came, ada before cora
        let (organizer, guest, last) = (ada.engine.mri.clone(), bert.engine.mri.clone(), cora.engine.mri.clone());
        bert.engine
            .view(b_call, |view| {
                view.participants
                    .iter()
                    .any(|party| party.mri == organizer && party.organizer)
            })
            .await;
        ada.engine
            .view(a_call, |view| {
                let shown = |mri: &str| view.participants.iter().find(|party| party.mri == mri);
                shown(&last).is_some() && shown(&guest).is_some_and(|party| !party.organizer)
            })
            .await;
        leave_all(&mut [&mut bert, &mut ada, &mut cora], &[b_call, a_call, c_call]).await;
        let events = ended_events(&mixed, &cora, &chat).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events[1].contains("callStarted"), "{events:?}");
        assert!(
            events[0].contains("<ended/>") && events[0].contains("count=\"3\""),
            "{events:?}"
        );
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

/// Until each of `people` is pushed the start of the meeting in `chat` by `starter` (`evt` 128), as Teams pushes it to
/// its members.
async fn told_started(people: &mut [&mut Person], chat: &str, starter: &str) {
    told(people, MeetingStartKind::Meeting, chat, starter).await;
}

/// `person` is pushed no meeting's start within [`QUIET`] (pushes already sent come within it).
async fn untold(person: &mut Person) {
    let deadline = tokio::time::Instant::now() + QUIET;
    while let Ok(event) = tokio::time::timeout_at(deadline, person.engine.handle.next_event()).await {
        let event = event.expect("the engine runs");
        assert!(!matches!(event, Event::MeetingStarted(_)), "{}: {event:?}", person.name);
    }
}

/// How long [`untold`] waits for a push that must not come.
const QUIET: std::time::Duration = std::time::Duration::from_secs(2);

/// Until each of `people` is pushed the start `kind` of the meeting in `chat` by `starter` (`evt` 128).
async fn told(people: &mut [&mut Person], kind: MeetingStartKind, chat: &str, starter: &str) {
    for person in people.iter_mut() {
        let start = person
            .engine
            .next(|event| match event {
                Event::MeetingStarted(start) => Some(start.clone()),
                _ => None,
            })
            .await;
        assert_eq!(
            (start.kind, start.thread.as_str(), start.starter.as_deref()),
            (kind, chat, Some(starter))
        );
        assert_eq!(start.subject.as_deref(), Some("Planning"));
    }
}

/// The meeting's lobby option (`AutoAdmittedUsers`) saved as `value` by its organizer, as Teams' meeting options page
/// saves it (the native client opens that page in a browser).
pub(super) async fn admit_automatically(mixed: &Mixed, organizer: &Person, event: &CalendarEvent, value: &str) {
    let tenant = &event.meeting.as_ref().unwrap().tenant_id;
    let chat = event.meeting_chat.as_ref().unwrap();
    let path = format!(
        "/api/mt/part/own-01/beta/meetings/v1/options/{tenant}/{}/{chat}/0/0",
        organizer.id()
    );
    let token = crate::teams::security::token(mixed.server(), &organizer.upn, MIDDLE_TIER_AUDIENCE).await;
    let options = json!({"options": [{"name": "AutoAdmittedUsers", "currentValue": value}]});
    let (status, answer) =
        crate::teams::security::call(mixed.server(), &token, Method::POST, &path, Some(options)).await;
    assert_eq!(status, 200, "{answer}");
}

/// Until `person`'s lobby of `call` is told as `waiting` (by MRI).
pub(super) async fn lobby_is(person: &mut Person, call: CallId, waiting: &[&str]) {
    person
        .engine
        .next(|event| match event {
            Event::Lobby {
                call: of,
                waiting: told,
            } if *of == call => {
                let told: Vec<&str> = told.iter().map(|person| person.mri.as_str()).collect();
                (told == waiting).then_some(())
            }
            _ => None,
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outsider_waits_in_the_lobby_until_the_organizer_admits_it_and_one_turned_away_never_comes_in() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut dan = Person::new(&mixed, "Dan", C_TONE).await;
        let mut eve = Person::new(&mixed, "Eve", C_TONE).await;
        let event = schedule(&ada, &[&bert]).await;
        admit_automatically(&mixed, &ada, &event, "InvitedUsers").await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        // an invited guest comes straight in
        let b_call = joined(&mut bert, &join).await;
        // someone the meeting doesn't invite waits, with no media, until let in
        let d_call = dan.engine.call(join.clone()).await;
        dan.engine.status(d_call, &CallStatus::Lobby).await;
        let waiting = dan.engine.mri.clone();
        lobby_is(&mut ada, a_call, &[&waiting]).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 2).await;
        let admit = CallAction::Lobby {
            call: a_call,
            waiting: Mri::new(waiting.clone()),
            admit: true,
        };
        ada.engine.call(admit).await;
        dan.engine.status(d_call, &CallStatus::Connected).await;
        lobby_is(&mut ada, a_call, &[]).await;
        mixed.connected(&id, 3).await;
        bert.engine
            .view(b_call, |view| {
                view.participants.iter().any(|party| party.mri == waiting)
            })
            .await;
        // one turned away ends its call and never comes in
        let e_call = eve.engine.call(join.clone()).await;
        eve.engine.status(e_call, &CallStatus::Lobby).await;
        let turned_away = eve.engine.mri.clone();
        lobby_is(&mut ada, a_call, &[&turned_away]).await;
        let deny = CallAction::Lobby {
            call: a_call,
            waiting: Mri::new(turned_away),
            admit: false,
        };
        ada.engine.call(deny).await;
        let end = eve.engine.ended(e_call).await;
        assert!(matches!(end, End::Remote(_)), "{end:?}");
        lobby_is(&mut ada, a_call, &[]).await;
        mixed.connected(&id, 3).await;
        leave_all(&mut [&mut dan, &mut bert, &mut ada], &[d_call, b_call, a_call]).await;
        shut_down(vec![ada, bert, dan, eve], mixed).await;
    }
}

/// Until `person`'s view of `call` shows `mri` with its hand raised (`raised`) or not.
async fn hand_shown(person: &mut Person, call: CallId, mri: &str, raised: bool) {
    person
        .engine
        .view(call, |view| {
            view.participants
                .iter()
                .any(|party| party.mri == mri && party.hand_raised == raised)
        })
        .await;
}

/// Until `person` is told `from` reacted with `reaction` in `call`.
async fn reaction_told(person: &mut Person, call: CallId, from: &str, reaction: &str) {
    person
        .engine
        .next(|event| match event {
            Event::CallReaction {
                call: of,
                from: by,
                reaction: told,
                ..
            } if *of == call && by.as_str() == from && told == reaction => Some(()),
            _ => None,
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raised_hand_and_a_reaction_reach_everyone_in_the_meeting() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        let c_call = joined(&mut cora, &join).await;
        let b = bert.engine.mri.clone();
        bert.engine
            .call(CallAction::RaiseHand {
                call: b_call,
                raised: true,
            })
            .await;
        hand_shown(&mut ada, a_call, &b, true).await;
        hand_shown(&mut cora, c_call, &b, true).await;
        bert.engine
            .next(|event| match event {
                Event::CallOwn {
                    call,
                    hand_raised: true,
                    ..
                } if *call == b_call => Some(()),
                _ => None,
            })
            .await;
        let c = cora.engine.mri.clone();
        let react = CallAction::React {
            call: c_call,
            reaction: "applause".to_owned(),
        };
        cora.engine.call(react).await;
        reaction_told(&mut ada, a_call, &c, "applause").await;
        reaction_told(&mut bert, b_call, &c, "applause").await;
        reaction_told(&mut cora, c_call, &c, "applause").await;
        bert.engine
            .call(CallAction::RaiseHand {
                call: b_call,
                raised: false,
            })
            .await;
        hand_shown(&mut ada, a_call, &b, false).await;
        hand_shown(&mut cora, c_call, &b, false).await;
        leave_all(&mut [&mut ada, &mut bert, &mut cora], &[a_call, b_call, c_call]).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

/// Until `person`'s view of `call` shows `mri` muted (`muted`) or not.
async fn muted_shown(person: &mut Person, call: CallId, mri: &str, muted: bool) {
    person
        .engine
        .view(call, |view| {
            view.participants
                .iter()
                .any(|party| party.mri == mri && party.muted == muted)
        })
        .await;
}

/// Until `person` is told the organizer muted it in `call`.
async fn muted_by_organizer(person: &mut Person, call: CallId) {
    person
        .engine
        .next(|event| match event {
            Event::CallMeeting { call: of, state } if *of == call && state.muted_by_organizer.is_some() => Some(()),
            _ => None,
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_organizer_mutes_everyone_else_who_see_it_and_may_unmute_themselves() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let b_call = joined(&mut bert, &join).await;
        let a_call = joined(&mut ada, &join).await;
        let c_call = joined(&mut cora, &join).await;
        let (b, c) = (bert.engine.mri.clone(), cora.engine.mri.clone());
        muted_shown(&mut ada, a_call, &c, false).await;
        ada.engine.call(CallAction::MuteAll { call: a_call }).await;
        muted_by_organizer(&mut bert, b_call).await;
        muted_by_organizer(&mut cora, c_call).await;
        // one roster can show both muted: one wait for both, so the first doesn't eat the second's
        ada.engine
            .view(a_call, |view| {
                [&b, &c]
                    .iter()
                    .all(|mri| view.participants.iter().any(|party| party.mri == **mri && party.muted))
            })
            .await;
        let a = ada.engine.mri.clone();
        muted_shown(&mut bert, b_call, &a, false).await;
        // a muted participant unmutes itself, as Teams' "You've been muted" card lets it
        bert.engine
            .call(CallAction::Mute {
                call: b_call,
                muted: false,
            })
            .await;
        muted_shown(&mut ada, a_call, &b, false).await;
        muted_shown(&mut cora, c_call, &b, false).await;
        leave_all(&mut [&mut ada, &mut bert, &mut cora], &[a_call, b_call, c_call]).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_organizer_ends_the_meeting_for_everyone_in_it_and_its_chat_tells_the_end() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        let c_call = joined(&mut cora, &join).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 3).await;
        ada.engine.call(CallAction::EndMeeting { call: a_call }).await;
        for (person, call) in [(&mut bert, b_call), (&mut cora, c_call)] {
            let end = person.engine.ended(call).await;
            assert!(matches!(end, End::Remote(_)), "{end:?}");
        }
        ada.engine.ended(a_call).await;
        let calls = ngteams_server::calling::open_calls(&mixed.server().app.db)
            .await
            .unwrap();
        assert!(calls.is_empty(), "{calls:?}");
        let chat = event.meeting_chat.clone().unwrap();
        let events = ended_events(&mixed, &bert, &chat).await;
        assert!(
            events[0].contains("callEnded") && events[0].contains("count=\"3\""),
            "{events:?}"
        );
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

/// What `reader`'s own streams hold of calls: its call log's entries and its activity feed's missed calls.
async fn call_traces(mixed: &Mixed, reader: &Person) -> Vec<String> {
    assert!(reader.engine.handle.sync().await.unwrap().failed.is_empty());
    let client = reader.engine.account.client(mixed.server());
    let reads = ChatReads::new(&client);
    let log = reads.messages("48:calllogs", 20).await.unwrap();
    let feed = reads.messages("48:notifications", 20).await.unwrap();
    let missed = feed.messages.into_iter().filter(|item| {
        item.properties
            .get("activity")
            .is_some_and(|activity| activity["activitySubtype"] == "missedCall")
    });
    log.messages
        .into_iter()
        .chain(missed)
        .map(|message| message.content)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_meeting_leaves_no_call_log_entry_and_no_missed_call_as_in_teams() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut eve = Person::new(&mixed, "Eve", C_TONE).await;
        let event = schedule(&ada, &[&bert]).await;
        admit_automatically(&mixed, &ada, &event, "InvitedUsers").await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        // one turned away from the lobby never took the call, but no one rang it either
        let e_call = eve.engine.call(join.clone()).await;
        eve.engine.status(e_call, &CallStatus::Lobby).await;
        let turned_away = eve.engine.mri.clone();
        lobby_is(&mut ada, a_call, &[&turned_away]).await;
        let deny = CallAction::Lobby {
            call: a_call,
            waiting: Mri::new(turned_away),
            admit: false,
        };
        ada.engine.call(deny).await;
        eve.engine.ended(e_call).await;
        ada.engine.call(CallAction::EndMeeting { call: a_call }).await;
        bert.engine.ended(b_call).await;
        ada.engine.ended(a_call).await;
        let chat = event.meeting_chat.clone().unwrap();
        assert!(ended_events(&mixed, &bert, &chat).await[0].contains("callEnded"));
        // a call's end is told in its chat just before it is written to the parties' streams
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            for person in [&ada, &bert, &eve] {
                let traces = call_traces(&mixed, person).await;
                assert!(traces.is_empty(), "{}: {traces:?}", person.name);
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        shut_down(vec![ada, bert, eve], mixed).await;
    }
}

/// A team `organizer` makes with `others` in it, and a channel of it, once each person's engine has them: the channel.
async fn channel_of(mixed: &Mixed, organizer: &Person, others: &[&Person]) -> String {
    let id = organizer.id().to_owned();
    let maker = crate::teams::Seat::of(mixed.server(), &organizer.upn, id).await;
    let mut members = Vec::new();
    for other in others {
        members.push(crate::teams::Seat::of(mixed.server(), &other.upn, other.id().to_owned()).await);
    }
    let (team, group) = maker.make_team("Rocket", false).await;
    maker.add(&team, &group, &members.iter().collect::<Vec<_>>()).await;
    let channel = maker.make_channel(&team, &group, "Launch", true).await;
    for person in std::iter::once(organizer).chain(others.iter().copied()) {
        assert!(person.engine.handle.sync().await.unwrap().failed.is_empty());
    }
    maker.stop();
    for member in members {
        member.stop();
    }
    channel
}

/// The channel's posts `reader` reads, newest first: (its kind, its text, its meeting's title).
async fn posts(mixed: &Mixed, reader: &Person, channel: &str) -> Vec<(String, String, Option<String>)> {
    let client = reader.engine.account.client(mixed.server());
    let page = ChatReads::new(&client).messages(channel, 20).await.unwrap();
    page.messages
        .iter()
        .map(|message| {
            let meeting = match message.properties.get("meeting") {
                Some(serde_json::Value::String(text)) => serde_json::from_str(text).ok(),
                other => other.cloned(),
            };
            let title = meeting
                .as_ref()
                .and_then(|meeting: &serde_json::Value| meeting.get("meetingtitle")?.as_str())
                .map(str::to_owned);
            (message.message_type.as_str().to_owned(), message.content.clone(), title)
        })
        .collect()
}

/// The channel's call start post and the post its end replies under, as the client reads the chain of each: Teams
/// writes a channel meeting's end into its post's chain, not as a post of its own.
async fn end_chain(mixed: &Mixed, reader: &Person, channel: &str) -> (Option<String>, Option<String>) {
    let client = reader.engine.account.client(mixed.server());
    let page = ChatReads::new(&client).messages(channel, 20).await.unwrap();
    let of = |event: &str| {
        page.messages
            .iter()
            .find(|message| message.message_type.as_str() == "Event/Call" && message.content.contains(event))
    };
    let started = of("callStarted").map(|message| message.id.clone());
    (started, of("callEnded").and_then(|message| message.reply_root(channel)))
}

/// A hand-made plain group start by `caller` in `chat`, ringing `to`, with its callbacks on its own calling endpoint
/// as the engine sends them: only where the call may run decides its answer.
async fn plain_start(mixed: &Mixed, caller: &Person, chat: &str, to: &[&Person]) -> serde_json::Value {
    let (_, base) = super::group_signaling::own_callbacks(mixed.server(), caller.id()).await;
    let to: Vec<_> = to.iter().map(|person| json!({"id": person.engine.mri})).collect();
    json!({
        "participants": {"from": {"id": caller.engine.mri}, "to": to},
        "groupChat": {"threadId": chat, "messageId": "0"},
        "callInvitation": {
            "mediaContent": {"blob": "v=0", "contentType": "application/sdp-ngc-1.0"},
            "links": {"acceptance": format!("{base}acceptance/"), "end": format!("{base}end/")},
        },
    })
}

/// The `<callId>` an `Event/Call` post names.
fn call_id(text: &str) -> Option<String> {
    let rest = text.split("<callId>").nth(1)?;
    Some(rest.split('<').next()?.to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channels_meet_now_is_posted_titled_joined_from_its_post_and_told_ended() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let channel = channel_of(&mixed, &ada, &[&bert]).await;
        // a hand-made plain group call in a channel isn't made: a channel's call is its Meet now
        let plain = plain_start(&mixed, &bert, &channel, &[&ada]).await;
        assert_eq!(
            raw(&mixed, &bert, Method::POST, "/flightproxy/api/v2/epconv", &plain).await,
            400
        );
        let calls = ngteams_server::calling::open_calls(&mixed.server().app.db)
            .await
            .unwrap();
        assert!(calls.is_empty(), "{calls:?}");
        let a_call = ada
            .engine
            .call(CallAction::MeetNow {
                channel: channel.clone(),
            })
            .await;
        ada.engine.status(a_call, &CallStatus::Connected).await;
        // the pre-join's title goes to the post the service makes, as Teams' web client titles it
        ada.engine
            .handle
            .requests()
            .title_meet_now(channel.clone(), "Sprint review")
            .await
            .unwrap();
        let listed = posts(&mixed, &bert, &channel).await;
        let started = listed
            .iter()
            .find(|(kind, text, _)| kind == "Event/Call" && text.contains("callStarted"))
            .unwrap_or_else(|| panic!("{listed:?}"));
        assert_eq!(started.2.as_deref(), Some("Sprint review"), "{listed:?}");
        let b_call = bert
            .engine
            .call(CallAction::JoinChannel {
                channel: channel.clone(),
                post: None,
            })
            .await;
        // the roster with the organizer can come before the join's status: waited for first, so no wait eats it
        let organizer = ada.engine.mri.clone();
        bert.engine
            .view(b_call, |view| {
                view.participants
                    .iter()
                    .any(|party| party.mri == organizer && party.organizer)
            })
            .await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 2).await;
        leave_all(&mut [&mut bert, &mut ada], &[b_call, a_call]).await;
        let listed = posts(&mixed, &bert, &channel).await;
        let (_, ended, _) = &listed[0];
        assert!(ended.contains("callEnded"), "{listed:?}");
        assert_eq!(call_id(ended), call_id(&started.1), "{listed:?}");
        let (post, under) = end_chain(&mixed, &bert, &channel).await;
        assert!(post.is_some() && under == post, "{post:?} {under:?} {listed:?}");
        shut_down(vec![ada, bert], mixed).await;
    }
}

/// Until `person` is told its meeting `call` is as `wanted` says.
async fn meeting_is(person: &mut Person, call: CallId, wanted: impl Fn(&teams_call::MeetingState) -> bool) {
    person
        .engine
        .next(|event| match event {
            Event::CallMeeting { call: of, state } if *of == call && wanted(state) => Some(()),
            _ => None,
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_organizers_controls_bind_everyone_in_the_meeting_while_it_runs() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        let c_call = joined(&mut cora, &join).await;
        let option = |option| CallAction::MeetingOption { call: a_call, option };
        // only the organizer presents: the others are attendees, told so with the option
        ada.engine
            .call(option(teams_proto::MeetingOption::Present(false)))
            .await;
        for (person, call) in [(&mut bert, b_call), (&mut cora, c_call)] {
            meeting_is(person, call, |state| {
                state.attendee && state.options.present == Some(false)
            })
            .await;
        }
        // attendees may not unmute: the service mutes them, and their microphone is blocked
        let restrict = teams_proto::MeetingOption::Attendees {
            unmute: false,
            camera: true,
        };
        ada.engine.call(option(restrict)).await;
        for (person, call) in [(&mut bert, b_call), (&mut cora, c_call)] {
            meeting_is(person, call, |state| {
                state.unmute_blocked() && state.muted_by_organizer.is_some()
            })
            .await;
        }
        let b = bert.engine.mri.clone();
        muted_shown(&mut ada, a_call, &b, true).await;
        // everyone presents again: the restriction binds nobody
        ada.engine.call(option(teams_proto::MeetingOption::Present(true))).await;
        for (person, call) in [(&mut bert, b_call), (&mut cora, c_call)] {
            meeting_is(person, call, |state| !state.attendee && !state.unmute_blocked()).await;
        }
        leave_all(&mut [&mut ada, &mut bert, &mut cora], &[a_call, b_call, c_call]).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_meeting_is_joined_by_its_id_and_passcode_and_a_wrong_passcode_finds_none() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let event = schedule(&ada, &[&bert]).await;
        let code = event
            .meeting_code
            .clone()
            .expect("a Teams meeting has an id and passcode");
        // the first one by its id starts the meeting's one call; the next by its chat joins it
        let b_call = joined(&mut bert, &CallAction::JoinMeetingCode(code.clone())).await;
        let a_call = joined(&mut ada, &join_of(&event)).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 2).await;
        let mut wrong = code.clone();
        wrong.passcode = "wrong0passcode".to_owned();
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let c_call = cora.engine.call(CallAction::JoinMeetingCode(wrong)).await;
        let end = cora.engine.ended(c_call).await;
        assert!(matches!(end, End::Failed(_)), "{end:?}");
        assert_eq!(mixed.open_call().await, id);
        leave_all(&mut [&mut ada, &mut bert], &[a_call, b_call]).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_guest_who_comes_before_the_organizer_waits_until_it_comes_and_lets_it_in() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        admit_automatically(&mixed, &ada, &event, "OrganizerOnly").await;
        let join = join_of(&event);
        // nobody is in the meeting yet: the invited guest waits all the same
        let b_call = bert.engine.call(join.clone()).await;
        bert.engine.status(b_call, &CallStatus::Lobby).await;
        // only the organizer, who can let it in, is told that it waits; the other member is told nothing
        let chat = event.meeting_chat.clone().unwrap();
        let waiting = bert.engine.mri.clone();
        told(&mut [&mut ada], MeetingStartKind::Lobby, &chat, &waiting).await;
        untold(&mut cora).await;
        let id = mixed.open_call().await;
        let a_call = joined(&mut ada, &join).await;
        assert_eq!(
            mixed.open_call().await,
            id,
            "the organizer comes into the call the guest waits in"
        );
        let admit = CallAction::Lobby {
            call: a_call,
            waiting: Mri::new(waiting.clone()),
            admit: true,
        };
        ada.engine.call(admit).await;
        bert.engine.status(b_call, &CallStatus::Connected).await;
        mixed.connected(&id, 2).await;
        // the guest who waited and was let in leaves; the meeting goes on until the organizer leaves
        leave_all(&mut [&mut bert, &mut ada], &[b_call, a_call]).await;
        let calls = ngteams_server::calling::open_calls(&mixed.server().app.db)
            .await
            .unwrap();
        assert!(calls.is_empty(), "{calls:?}");
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

/// A raw calling request with `person`'s skypetoken.
async fn raw(mixed: &Mixed, person: &Person, method: Method, path: &str, body: &serde_json::Value) -> u16 {
    let session = mixed.server().session(&person.upn).await;
    let token = session.skypetoken().await.unwrap();
    reqwest::Client::new()
        .request(method, mixed.server().url(path))
        .header("x-skypetoken", token.token.expose())
        .json(body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_a_meetings_hosts_let_in_or_mute_and_an_unknown_meeting_is_none() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let dan = Person::new(&mixed, "Dan", C_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        ada.engine
            .call(CallAction::MeetingOption {
                call: a_call,
                option: teams_proto::MeetingOption::Present(false),
            })
            .await;
        meeting_is(&mut bert, b_call, |state| state.attendee).await;
        let calls = ngteams_server::calling::open_calls(&mixed.server().app.db)
            .await
            .unwrap();
        let conversation = format!("/flightproxy/api/v2/ep/conv.own/conv/{}", calls[0].conversation);
        let at = |op: &str| format!("{conversation}/{op}");
        let to = |person: &Person| json!({"participants": {"from": {}, "to": [{"id": person.engine.mri}]}});
        // someone who isn't in it finds no call; an attendee hosts nothing
        assert_eq!(raw(&mixed, &dan, Method::POST, &at("admit"), &to(&bert)).await, 404);
        assert_eq!(raw(&mixed, &bert, Method::POST, &at("admit"), &to(&dan)).await, 403);
        assert_eq!(
            raw(&mixed, &bert, Method::POST, &at("removeParticipant"), &to(&ada)).await,
            403
        );
        let all = json!({"scope": "all", "muteParticipants": [], "mediaTypes": ["audio"]});
        assert_eq!(raw(&mixed, &bert, Method::POST, &at("mute"), &all).await, 403);
        // an attendee rings nobody into the meeting: not even an invited member of its chat who hasn't joined
        assert_eq!(raw(&mixed, &bert, Method::POST, &at("add"), &to(&cora)).await, 403);
        // the organizer's Request to join rings that member, who answers into the meeting
        let chat = event.meeting_chat.clone().unwrap();
        let asked = CallAction::Add {
            call: a_call,
            invitee: Mri::new(cora.engine.mri.clone()),
        };
        ada.engine.call(asked).await;
        let ringing = cora.engine.incoming().await;
        assert_eq!(ringing.group_chat.as_deref(), Some(chat.as_str()));
        assert_eq!(ringing.caller.as_str(), ada.engine.mri);
        let c_call = cora.engine.call(CallAction::Answer { call: ringing.call }).await;
        cora.engine.status(c_call, &CallStatus::Connected).await;
        mixed.connected(&calls[0].id, 3).await;
        ada.engine.view(a_call, |view| view.participants.len() == 2).await;
        assert_eq!(raw(&mixed, &ada, Method::POST, &at("admit"), &json!({})).await, 400);
        // the organizer mutes one person by name (Teams' client offers only Mute all)
        let one = json!({"scope": "specified", "muteParticipants": [{"id": bert.engine.mri}], "mediaTypes": ["audio"]});
        assert_eq!(raw(&mixed, &ada, Method::POST, &at("mute"), &one).await, 202);
        muted_by_organizer(&mut bert, b_call).await;
        // a meeting chat no meeting has, and an id and passcode of none
        let offer = json!({"blob": "v=0", "contentType": "application/sdp-ngc-1.0"});
        let unknown = json!({
            "participants": {"from": {"id": dan.engine.mri}, "to": []},
            "groupChat": {"threadId": "19:meeting_none@thread.v2", "messageId": "0"},
            "meetingInfo": {"organizerId": ada.id(), "tenantId": "t"},
            "callInvitation": {"mediaContent": offer},
        });
        let start = "/flightproxy/api/v2/epconv";
        assert_eq!(raw(&mixed, &dan, Method::POST, start, &unknown).await, 404);
        let no_chat = json!({
            "participants": {"from": {"id": dan.engine.mri}, "to": []},
            "meetingInfo": {"organizerId": ada.id(), "tenantId": "t"},
            "callInvitation": {"mediaContent": offer},
        });
        assert_eq!(raw(&mixed, &dan, Method::POST, start, &no_chat).await, 501);
        // an invited attendee's hand-made start in the meeting's chat is no meeting of its own: neither a Meet now
        // nor a plain group call is made there
        let meet_now = json!({
            "participants": {"from": {"id": bert.engine.mri}, "to": []},
            "groupChat": {"threadId": chat, "messageId": "0"},
            "conversationRequest": {"properties": {"enableGroupCallMeetupGeneration": true}},
        });
        assert_eq!(raw(&mixed, &bert, Method::POST, start, &meet_now).await, 400);
        let plain = plain_start(&mixed, &bert, &chat, &[]).await;
        assert_eq!(raw(&mixed, &bert, Method::POST, start, &plain).await, 400);
        let calls = ngteams_server::calling::open_calls(&mixed.server().app.db)
            .await
            .unwrap();
        assert_eq!(calls.len(), 1, "{calls:?}");
        leave_all(&mut [&mut cora, &mut ada, &mut bert], &[c_call, a_call, b_call]).await;
        shut_down(vec![ada, bert, cora, dan], mixed).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_first_joins_at_once_make_one_meeting_call_and_a_channels_one_post() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let event = schedule(&ada, &[&bert]).await;
        admit_automatically(&mixed, &ada, &event, "OrganizerOnly").await;
        let join = join_of(&event);
        // the organizer and a guest the lobby holds start the meeting's call at once
        let (a_call, b_call) = tokio::join!(ada.engine.call(join.clone()), bert.engine.call(join.clone()));
        ada.engine.status(a_call, &CallStatus::Connected).await;
        bert.engine.status(b_call, &CallStatus::Lobby).await;
        let id = mixed.open_call().await;
        // the guest waits in the organizer's call: let in, both are in it
        let admit = CallAction::Lobby {
            call: a_call,
            waiting: Mri::new(bert.engine.mri.clone()),
            admit: true,
        };
        ada.engine.call(admit).await;
        bert.engine.status(b_call, &CallStatus::Connected).await;
        mixed.connected(&id, 2).await;
        leave_all(&mut [&mut bert, &mut ada], &[b_call, a_call]).await;
        // two Meet nows in one channel at once: one meeting, posted once
        let channel = channel_of(&mixed, &ada, &[&bert]).await;
        let meet_now = CallAction::MeetNow {
            channel: channel.clone(),
        };
        let (a_call, b_call) = tokio::join!(ada.engine.call(meet_now.clone()), bert.engine.call(meet_now.clone()));
        ada.engine.status(a_call, &CallStatus::Connected).await;
        bert.engine.status(b_call, &CallStatus::Connected).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 2).await;
        let listed = posts(&mixed, &bert, &channel).await;
        let started = listed
            .iter()
            .filter(|(kind, text, _)| kind == "Event/Call" && text.contains("callStarted"))
            .count();
        assert_eq!(started, 1, "{listed:?}");
        leave_all(&mut [&mut bert, &mut ada], &[b_call, a_call]).await;
        shut_down(vec![ada, bert], mixed).await;
    }
}
