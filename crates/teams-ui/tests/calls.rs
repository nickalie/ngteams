//! Calls in the app (M3 Task 17) on GPUI's test context, over the fake engine (`common`): the chat header's Call (once
//! the calling endpoint is registered) and Join (where an invitation named a running call); the switcher's call
//! commands as they fit the moment, "Make a test call" among them; the incoming call's banner, ringtone and notification with
//! Answer and Decline, only for an invitation the engine offered, its caller named as the invitation names them, all
//! gone once its ring ends or the account signs out; the in-call view (who it is with, mute, hang up, the duration, the
//! call's state, how it ended, an ask refused while it is up or after, voicemail); one start at a time, its media's
//! setup included; the keys for each; the trace lines for each, never with a name; and a run that shows no call (an
//! engine without calling) showing none of it, its call keys included. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[path = "support/fake_tool.rs"]
#[expect(dead_code, reason = "the shared fake CLI helper serves several test harnesses")]
mod fake_tool;

pub mod common;
/// The faces a call shows: pictures, else initials.
#[path = "calls/faces.rs"]
mod faces;
/// The Call health pane, from More → Settings.
#[path = "calls/health.rs"]
mod health;
/// Calls of ten people and more: the stage keeps clear of the call's controls.
#[path = "calls/large_call.rs"]
mod large_call;
#[path = "common/notices.rs"]
mod notices;
/// What a call sounds like as it goes along.
#[path = "calls/sounds.rs"]
mod sounds;
/// A recording's notice that fits its card, and Controls' checks a lagging read doesn't flip back.
#[path = "calls/stage_fit.rs"]
mod stage_fit;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use notices::{FakeServer, chat, message};
use teams_core::VideoOutput;
use teams_core::{
    CallAction, CallActionError, CallId, CallParticipant, CallStatus, CallView, End, EngineError, Event, IncomingCall,
    MessageChange, RegistrationReport, RemoteEnd, Source,
};
use teams_media::devices::catalog::{DeviceDirectory as _, DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_media::devices::pattern::TestPattern;
use teams_proto::Mri;
use teams_store::{Applied, ConversationKind, Member};
use teams_ui::call_alert::{CallAlert, notice_of};
use teams_ui::call_stage::AUTO_CLOSE;
use teams_ui::call_view;
use teams_ui::call_window::{CallWindows, CallWindowsEvent};
use teams_ui::calls::{Ask, CallKind, EndReason, Ended, Verdict};
use teams_ui::local_ai::Tools as Translators;
use teams_ui::notify::NoticeKind;
use teams_ui::ringtone::{Ringer, Ringtone};
use teams_ui::settings::{Settings, SettingsModel, attach_devices};
use teams_ui::settings_view::{Page, TestCallOffer};
use teams_ui::shell::{Main, Shell, ShellParts};
use teams_ui::switcher::CallCommand;
use teams_ui::translate::Backend;
use teams_ui::video::{Tile, VideoFeed};
use teams_ui::{Calls, ViewTrace};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
/// The caller's name as an invitation gives it.
const CALLER_NAME: &str = "Bee Caller";

/// A ringer that counts the ringtones it started and the ones stopped.
#[derive(Default)]
struct FakeRinger {
    quiet: bool,
    contexts: Mutex<Vec<(String, bool)>>,
    started: AtomicUsize,
    stopped: Arc<AtomicUsize>,
}

impl FakeRinger {
    fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    fn stopped(&self) -> usize {
        self.stopped.load(Ordering::SeqCst)
    }
}

impl Ringer for FakeRinger {
    fn audible(&self, account: &str, in_call: bool) -> bool {
        self.contexts.lock().unwrap().push((account.to_owned(), in_call));
        !self.quiet
    }

    fn start(&self) -> Ringtone {
        self.started.fetch_add(1, Ordering::SeqCst);
        let stopped = Arc::clone(&self.stopped);
        Ringtone::new(move || {
            stopped.fetch_add(1, Ordering::SeqCst);
        })
    }
}

/// a's store: the a–b chat (b named) and the trio, each with its members.
fn seed(rig: &Rig) {
    rig.core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None),
        chat(TRIO, ConversationKind::Group, Some("Lab trio")),
    ]);
    rig.core.members.lock().unwrap().extend([
        (AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]),
        (
            TRIO.to_owned(),
            vec![
                Member::new(LAB_A, None),
                Member::new(LAB_B, None),
                Member::new(LAB_C, None),
            ],
        ),
    ]);
    rig.core.names.lock().unwrap().extend([
        (LAB_B.to_owned(), "Lab Bee".to_owned()),
        (LAB_C.to_owned(), "Lab Sea".to_owned()),
    ]);
}

/// The rig of a build that shows calls, a's store seeded, the kit set up.
fn calling() -> Rig {
    let rig = Rig::start_with(|cx| teams_ui::Models::with_calls(Some(Mri::new(ECHO)), cx));
    seed(&rig);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
}

/// The shell in a window of its own, a's settings opening `chat`.
fn shell_window(
    rig: &Rig,
    chat: Option<&str>,
    trace: Arc<ViewTrace>,
) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let settings = Settings {
        selected_chat: chat.map(str::to_owned),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace,
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    draw(cx);
    (shell, cx)
}

/// The call windows over `shell`'s account, as the desktop keeps them, traced to `trace`; with what they asked.
fn call_windows(
    rig: &Rig,
    shell: &Entity<Shell>,
    trace: &Arc<ViewTrace>,
) -> (Entity<CallWindows>, Arc<Mutex<Vec<CallWindowsEvent>>>) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.read(|cx| shell.read(cx).settings().clone()),
        trace: Arc::clone(trace),
    };
    let asked = Arc::new(Mutex::new(Vec::new()));
    let told = Arc::clone(&asked);
    let windows = rig.cx.update(|cx| {
        let windows = cx.new(|cx| CallWindows::new("ngteams-test", cx));
        windows.update(cx, |windows, cx| windows.add(parts, cx));
        cx.subscribe(&windows, move |_, event: &CallWindowsEvent, _| {
            told.lock().unwrap().push(event.clone());
        })
        .detach();
        windows
    });
    (windows, asked)
}

/// The call window `windows` have open, to draw and click in.
fn stage(rig: &Rig, windows: &Entity<CallWindows>) -> &'static mut VisualTestContext {
    rig.cx.run_until_parked();
    let window = rig
        .cx
        .read(|cx| windows.read(cx).window())
        .expect("the call window is open");
    let cx = Box::leak(Box::new(VisualTestContext::from_window(window.into(), &rig.cx)));
    draw(cx);
    cx
}

/// Whether `windows` have a window open.
fn window_open(rig: &Rig, windows: &Entity<CallWindows>) -> bool {
    rig.cx.run_until_parked();
    rig.cx.read(|cx| windows.read(cx).window().is_some())
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn drawn(cx: &mut VisualTestContext, selector: &str) -> bool {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    draw(cx);
    let found = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn call(number: u64) -> CallId {
    CallId::for_tests(number)
}

fn invitation(number: u64, caller_name: Option<&str>, group_chat: Option<&str>) -> IncomingCall {
    IncomingCall {
        call: call(number),
        caller: Mri::new(LAB_B),
        caller_name: caller_name.map(str::to_owned),
        group_chat: group_chat.map(str::to_owned),
    }
}

fn status(rig: &Rig, number: u64, status: CallStatus) {
    rig.send(Event::Call {
        call: call(number),
        status,
    });
}

fn registered(rig: &Rig) {
    rig.send(Event::CallingRegistration(RegistrationReport::Registered));
}

fn actions(rig: &Rig) -> Vec<CallAction> {
    rig.core.call_actions.lock().unwrap().clone()
}

fn lines(written: &Written, view: &str, change: &str) -> Vec<serde_json::Value> {
    written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == view && line["change"] == change)
        .collect()
}

fn kinds(line: &serde_json::Value) -> Vec<String> {
    line["kinds"]
        .as_array()
        .map(|kinds| {
            kinds
                .iter()
                .filter_map(|kind| kind.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn calls_of(rig: &Rig) -> Entity<Calls> {
    rig.models.calls.clone()
}

/// The incoming call's alert over the rig's calls, ringing on `ringer`, notifying on `server`.
fn alert(rig: &Rig, trace: &Arc<ViewTrace>, ringer: &Arc<FakeRinger>, server: &Arc<FakeServer>) -> Entity<CallAlert> {
    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
    let (trace, ringer, server) = (Arc::clone(trace), Arc::clone(ringer), Arc::clone(server));
    rig.cx.update(|cx| {
        cx.new(|cx| {
            let mut alert = CallAlert::new(core, calls_of(rig), trace, Some(ringer), cx);
            alert.set_server(server, cx);
            alert
        })
    })
}

fn no_name_in(written: &Written) {
    let text = written.text();
    for name in [CALLER_NAME, "Lab Bee", "Lab Sea", "Lab trio"] {
        assert!(!text.contains(name), "{name} reached the trace");
    }
}

#[test]
fn a_staged_group_chat_offers_call_only_after_its_members_arrive() {
    let rig = calling();
    let members = rig.core.members.lock().unwrap().remove(TRIO).unwrap();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "none");
    assert!(!drawn(cx, "chat-call"));
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    assert!(actions(&rig).is_empty());
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().unwrap().clone());
    cx.simulate_input("Call");
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| switcher.read(cx).commands()), ["Make a test call"]);

    rig.core.members.lock().unwrap().insert(TRIO.to_owned(), members);
    rig.send(Event::ConversationChanged {
        conversation: TRIO.into(),
    });
    assert_eq!(
        cx.update(|_, cx| switcher.read(cx).commands()),
        ["Call", "Make a test call"]
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "call");
    assert!(drawn(cx, "chat-call"));
    click(cx, "chat-call");
    assert_eq!(actions(&rig), [CallAction::Call { chat: TRIO.into() }]);
}

#[test]
fn a_staged_group_call_tells_why_it_is_refused_and_keeps_the_busy_refusal() {
    let rig = calling();
    rig.core.members.lock().unwrap().remove(TRIO);
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    assert!(actions(&rig).is_empty());
    assert_eq!(kinds(&lines(&written, "call", "asked")[0]), ["call"]);
    assert_eq!(kinds(&lines(&written, "call", "refused")[0]), ["call", "no-roster"]);
    let told = rig.cx.read(|cx| calls_of(&rig).read(cx).refused().cloned()).unwrap();
    assert_eq!(
        told.text,
        "this chat's members are still loading: try again in a moment"
    );
    assert!(drawn(cx, "call-after"));

    status(&rig, 1, CallStatus::Connected);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    assert!(actions(&rig).is_empty());
    assert_eq!(
        kinds(lines(&written, "call", "refused").last().unwrap()),
        ["call", "busy"]
    );
}

#[test]
fn notes_and_self_only_chats_keep_the_engines_permanent_call_refusal() {
    for chat in [teams_ui::chat_list::NOTES, TRIO, "19:unknown@thread.v2"] {
        let rig = calling();
        rig.core.listing.lock().unwrap().favorites = Some(vec![teams_ui::chat_list::NOTES.to_owned()]);
        rig.core
            .members
            .lock()
            .unwrap()
            .insert(TRIO.to_owned(), vec![Member::new(LAB_A, None)]);
        rig.core
            .call_answers
            .lock()
            .unwrap()
            .push_back(Err(EngineError::Call(CallActionError::NotAChat)));
        let (trace, written) = trace();
        let (_shell, cx) = shell_window(&rig, Some(chat), trace);
        registered(&rig);
        assert!(!drawn(cx, "chat-call"));
        cx.simulate_keystrokes("ctrl-shift-c");
        cx.run_until_parked();
        assert_eq!(actions(&rig), [CallAction::Call { chat: chat.into() }]);
        assert_eq!(kinds(&lines(&written, "call", "asked")[0]), ["call"]);
        assert_eq!(kinds(&lines(&written, "call", "refused")[0]), ["call", "not-a-chat"]);
        let told = rig.cx.read(|cx| calls_of(&rig).read(cx).refused().cloned()).unwrap();
        assert_eq!(told.text, EngineError::Call(CallActionError::NotAChat).to_string());
        assert!(drawn(cx, "call-after"));
    }
}

#[test]
fn a_staged_meeting_call_waits_for_its_members() {
    let rig = calling();
    rig.core
        .conversations
        .lock()
        .unwrap()
        .iter_mut()
        .find(|chat| chat.id == TRIO)
        .unwrap()
        .kind = ConversationKind::Meeting;
    rig.core.members.lock().unwrap().remove(TRIO);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::Call { chat: TRIO.into() }, cx)));
    cx.run_until_parked();
    assert!(actions(&rig).is_empty());
    assert_eq!(kinds(&lines(&written, "call", "refused")[0]), ["call", "no-roster"]);
}

#[test]
fn roster_arrival_clears_the_switcher_highlight_before_enter_can_call() {
    let rig = calling();
    let members = rig.core.members.lock().unwrap().remove(TRIO).unwrap();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("Call");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");

    rig.core.members.lock().unwrap().insert(TRIO.to_owned(), members);
    rig.send(Event::ConversationChanged {
        conversation: TRIO.into(),
    });
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(actions(&rig).is_empty(), "a shifted command needs a new selection");
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_some()));
    cx.simulate_keystrokes("down down enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::TestCall { echo: Mri::new(ECHO) }]);
}

#[test]
fn a_call_state_change_clears_the_switcher_highlight_before_enter_can_answer() {
    let rig = calling();
    let (trace, _) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("Call");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    rig.send(Event::IncomingCall(invitation(1, Some(CALLER_NAME), None)));
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(actions(&rig).is_empty(), "a changed command needs a new selection");
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::Answer { call: call(1) }]);
}

#[test]
fn unchanged_call_commands_keep_the_switcher_highlight() {
    let rig = calling();
    let (trace, _) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("Call");
    cx.run_until_parked();
    cx.simulate_keystrokes("down");
    rig.send(Event::ConversationChanged {
        conversation: AB.into(),
    });
    registered(&rig);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::TestCall { echo: Mri::new(ECHO) }]);
}

#[test]
fn a_one_to_one_chat_can_call_before_its_members_arrive() {
    let rig = calling();
    rig.core.members.lock().unwrap().clear();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "call");
    click(cx, "chat-call");
    assert_eq!(actions(&rig), [CallAction::Call { chat: AB.into() }]);
}

#[test]
fn a_chat_s_header_has_call_once_calling_is_registered_and_the_call_shows_until_it_ends() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    assert!(
        !drawn(cx, "chat-call"),
        "no Call before the calling endpoint is registered"
    );
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "none");

    registered(&rig);
    assert!(drawn(cx, "chat-call"));
    assert!(!drawn(cx, "chat-join"));
    click(cx, "chat-call");
    assert_eq!(actions(&rig), [CallAction::Call { chat: AB.into() }]);
    let asked = lines(&written, "call", "asked");
    assert_eq!(
        (asked[0]["conversation"].as_str(), kinds(&asked[0])),
        (Some(AB), vec!["call".to_owned()])
    );

    the_call_shows_as_it_goes(&rig, &shell, cx, &written);
    hang_up_and_dismiss(&rig, cx, &written);
    no_name_in(&written);
}

/// The engine tells the call as it goes: the in-call bar shows it, with who it is with; the header's Call is gone.
fn the_call_shows_as_it_goes(rig: &Rig, shell: &Entity<Shell>, cx: &mut VisualTestContext, written: &Written) {
    status(rig, 1, CallStatus::Placing);
    assert!(drawn(cx, "in-call"));
    assert!(!drawn(cx, "chat-call"), "one call at a time");
    status(rig, 1, CallStatus::Ringing);
    status(rig, 1, CallStatus::Connecting);
    status(rig, 1, CallStatus::Connected);
    let bar = cx.update(|_, cx| shell.read(cx).call_bar().clone());
    let participants = cx.update(|_, cx| bar.read(cx).participants().to_vec());
    assert_eq!(
        participants,
        [(LAB_B.to_owned(), "Lab Bee".into())],
        "who it is with, by name"
    );
    let active = cx.read(|cx| calls_of(rig).read(cx).call().cloned()).expect("up");
    assert!(
        active.connected_at.is_some(),
        "the duration counts from the media's connect"
    );
    let shown: Vec<Vec<String>> = lines(written, "in-call", "shown").iter().map(kinds).collect();
    for state in ["placing", "ringing", "connecting", "connected"] {
        assert!(
            shown
                .iter()
                .any(|kinds| kinds == &["chat", state, "unmuted", "camera-off"]),
            "{state}: {shown:?}"
        );
    }
    let connected = lines(written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(connected["call"], 1);
    assert_eq!(connected["conversation"], AB);
    assert_eq!(connected["people"], serde_json::json!([LAB_B]));
}

/// Hang up: the engine's hang-up; its end shown until dismissed, Call back in the header.
fn hang_up_and_dismiss(rig: &Rig, cx: &mut VisualTestContext, written: &Written) {
    click(cx, "call-hang-up");
    assert_eq!(actions(rig)[1], CallAction::HangUp { call: call(1) });
    status(rig, 1, CallStatus::Leaving);
    status(rig, 1, CallStatus::Ended(End::HungUp));
    assert!(!drawn(cx, "in-call"));
    assert!(drawn(cx, "call-after"));
    assert!(drawn(cx, "chat-call"));
    let ended = lines(written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&ended), ["chat", "ended", "hung-up", "unmuted", "camera-off"]);
    click(cx, "call-dismiss");
    assert!(!drawn(cx, "call-after"));
    assert_eq!(lines(written, "in-call", "hidden").len(), 1);
    let buttons: Vec<Vec<String>> = lines(written, "call-button", "shown").iter().map(kinds).collect();
    assert_eq!(buttons, [["none"], ["call"], ["none"], ["call"]]);
}

#[test]
fn join_shows_where_an_invitation_named_a_running_call_and_is_forgotten_once_refused() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "call");

    // a group call's invitation rings and runs out: the call runs on, to be joined
    rig.send(Event::IncomingCall(invitation(4, Some(CALLER_NAME), Some(TRIO))));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).call_button(cx)),
        "none",
        "while it rings"
    );
    rig.send(Event::RingEnded {
        call: call(4),
        end: RemoteEnd::default(),
    });
    assert!(drawn(cx, "chat-join"));
    assert!(!drawn(cx, "chat-call"));
    click(cx, "chat-join");
    assert_eq!(actions(&rig), [CallAction::Join { chat: TRIO.into() }]);
    assert!(!drawn(cx, "chat-join"), "no second join while the first one goes");
    status(&rig, 1, CallStatus::Joining);
    let joined = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!((joined.kind, joined.chat.as_deref()), (CallKind::Join, Some(TRIO)));
    status(&rig, 1, CallStatus::Ended(End::Remote(RemoteEnd::default())));

    // the engine knows no running call there any more: refused, said, and Join gone
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::Call(
            teams_core::CallError::NotJoinable,
        ))));
    cx.simulate_keystrokes("ctrl-shift-j");
    cx.run_until_parked();
    assert_eq!(actions(&rig).len(), 2, "the key asks the same");
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(&refused[0]), ["join", "not-joinable"]);
    assert!(drawn(cx, "call-after"), "why, shown");
    assert!(drawn(cx, "chat-call"), "Call again");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.into())
    );
    no_name_in(&written);
}

#[test]
fn the_incoming_call_rings_shows_its_banner_and_notification_and_is_answered_from_either() {
    let rig = calling();
    let (trace, written) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let alert = alert(&rig, &trace, &ringer, &server);
    let (_shell, cx) = shell_window(&rig, None, trace);
    registered(&rig);
    let rang = Rang {
        rig: &rig,
        ringer: &ringer,
        server: &server,
        written: &written,
    };
    rang.answered_from_the_notification(&alert, cx);
    rang.declined_from_the_banner(cx);
    rang.ended_by_itself(cx);
    no_name_in(&written);
}

/// An incoming call's test: the rig, the fake ringer and server, the trace.
struct Rang<'a> {
    rig: &'a Rig,
    ringer: &'a FakeRinger,
    server: &'a FakeServer,
    written: &'a Written,
}

impl Rang<'_> {
    /// It rings with its banner, ringtone and notification; Answer on the notification answers it.
    fn answered_from_the_notification(&self, alert: &Entity<CallAlert>, cx: &mut VisualTestContext) {
        let Self {
            rig,
            ringer,
            server,
            written,
        } = *self;
        rig.send(Event::IncomingCall(invitation(7, Some(CALLER_NAME), None)));
        assert!(drawn(cx, "call-banner"));
        assert!(drawn(cx, "call-answer") && drawn(cx, "call-decline"));
        assert_eq!((ringer.started(), ringer.stopped()), (1, 0), "the ringtone plays");
        let shown = server.shown();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].kind, NoticeKind::Call);
        assert_eq!(
            shown[0].summary, "Bee Caller is calling",
            "the caller's name as the invitation gives it"
        );
        let banner = lines(written, "call-banner", "shown");
        assert_eq!(banner[0]["call"], 7);
        assert_eq!(kinds(&banner[0]), ["one-to-one", "named"]);
        assert_eq!(banner[0]["people"], serde_json::json!([LAB_B]));
        self.answer_on_the_notification(alert, cx);
    }

    /// Answer on the notification: the engine's answer of that invitation; the ring stops once it is told.
    fn answer_on_the_notification(&self, alert: &Entity<CallAlert>, cx: &mut VisualTestContext) {
        let Self {
            rig,
            ringer,
            server,
            written,
        } = *self;
        let answered = rig
            .cx
            .update(|cx| alert.update(cx, |alert, cx| alert.activated(1, "answer", cx)));
        assert!(answered);
        rig.cx.run_until_parked();
        assert_eq!(actions(rig), [CallAction::Answer { call: call(7) }]);
        status(rig, 7, CallStatus::Answering);
        assert_eq!(ringer.stopped(), 1);
        assert_eq!(server.closed(), [1]);
        assert!(!drawn(cx, "call-banner"));
        assert!(drawn(cx, "in-call"));
        let active = rig.cx.read(|cx| calls_of(rig).read(cx).call().cloned()).unwrap();
        assert_eq!(active.caller_name.as_deref(), Some(CALLER_NAME));
        status(rig, 7, CallStatus::Connected);
        for (view, change) in [
            ("ringtone", "started"),
            ("ringtone", "stopped"),
            ("call-notification", "shown"),
            ("call-notification", "answered"),
            ("call-notification", "closed"),
            ("call-banner", "hidden"),
        ] {
            assert_eq!(lines(written, view, change).len(), 1, "{view} {change}");
        }
        let up = lines(written, "in-call", "shown").last().cloned().unwrap();
        assert_eq!(kinds(&up), ["incoming", "connected", "unmuted", "camera-off"]);
        status(rig, 7, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    }

    /// The next one: Decline on the banner declines it.
    fn declined_from_the_banner(&self, cx: &mut VisualTestContext) {
        let Self {
            rig,
            ringer,
            server,
            written,
        } = *self;
        rig.send(Event::IncomingCall(invitation(8, None, None)));
        assert_eq!(server.shown()[1].summary, "Someone is calling", "no name given");
        click(cx, "call-decline");
        assert_eq!(actions(rig)[1], CallAction::Decline { call: call(8) });
        status(rig, 8, CallStatus::Declining);
        status(rig, 8, CallStatus::Ended(End::Declined));
        assert_eq!((ringer.started(), ringer.stopped()), (2, 2));
        let declined = lines(written, "in-call", "shown").last().cloned().unwrap();
        assert_eq!(
            kinds(&declined),
            ["incoming", "ended", "declined", "unmuted", "camera-off"]
        );
    }

    /// One whose ring ends before anyone acts: all of it gone, nothing asked.
    fn ended_by_itself(&self, cx: &mut VisualTestContext) {
        let Self {
            rig, ringer, server, ..
        } = *self;
        rig.send(Event::IncomingCall(invitation(9, Some(CALLER_NAME), Some(TRIO))));
        assert_eq!(server.shown()[2].body, "Incoming group call");
        rig.send(Event::RingEnded {
            call: call(9),
            end: RemoteEnd::default(),
        });
        assert_eq!((ringer.started(), ringer.stopped()), (3, 3));
        assert!(server.closed().contains(&3));
        assert!(!drawn(cx, "call-banner"));
        assert_eq!(actions(rig).len(), 2, "nothing is answered or declined by itself");
    }
}

#[test]
fn a_quiet_ringtone_still_shows_the_invitation_and_checks_whether_already_in_a_call() {
    for in_call in [false, true] {
        let rig = calling();
        if in_call {
            let (_shell, cx) = shell_window(&rig, Some(AB), Arc::new(ViewTrace::off()));
            registered(&rig);
            click(cx, "chat-call");
            status(&rig, 1, CallStatus::Connected);
        } else {
            registered(&rig);
        }
        let ringer = Arc::new(FakeRinger {
            quiet: true,
            ..FakeRinger::default()
        });
        let server = Arc::new(FakeServer::default());
        let alert = alert(&rig, &Arc::new(ViewTrace::off()), &ringer, &server);
        rig.send(Event::IncomingCall(invitation(7, Some(CALLER_NAME), None)));
        assert_eq!(ringer.started(), 0);
        assert_eq!(server.shown().len(), 1);
        assert!(!rig.cx.read(|cx| alert.read(cx).ringing_aloud()));
        assert_eq!(*ringer.contexts.lock().unwrap(), [(LAB_A.to_owned(), in_call)]);
    }
}

#[test]
fn nothing_rings_but_an_invitation_the_engine_offered() {
    let rig = calling();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &Arc::new(ViewTrace::off()), &ringer, &server);
    registered(&rig);
    // a ring's end, a call's status and a voicemail of calls the engine never offered here ring nothing
    rig.send(Event::RingEnded {
        call: call(3),
        end: RemoteEnd::default(),
    });
    status(&rig, 3, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    rig.send(Event::Voicemail { call: call(5) });
    // nor does a message
    rig.send(Event::Message {
        conversation: AB.to_owned(),
        id: "6000".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message(AB, "6000", LAB_B, "<p>call me</p>", 6_000))),
        source: Source::Delivery,
    });
    assert_eq!(ringer.started(), 0);
    assert!(server.shown().is_empty());
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).ringing().is_none()));
    assert!(actions(&rig).is_empty());
    assert!(!rig.core.asked().contains(&"call"), "nothing asked of the engine");
    assert_eq!(rig.shutdowns(), 0);
}

#[test]
fn the_keys_call_answer_decline_mute_and_hang_up() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::Call { chat: AB.into() }]);
    status(&rig, 1, CallStatus::Connected);
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig)[1],
        CallAction::Mute {
            call: call(1),
            muted: true
        }
    );
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().muted));
    let muted = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&muted), ["chat", "connected", "muted", "camera-off"]);
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig)[2],
        CallAction::Mute {
            call: call(1),
            muted: false
        }
    );
    cx.simulate_keystrokes("ctrl-shift-h");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[3], CallAction::HangUp { call: call(1) });
    status(&rig, 1, CallStatus::Ended(End::HungUp));

    rig.send(Event::IncomingCall(invitation(2, Some(CALLER_NAME), None)));
    cx.simulate_keystrokes("ctrl-shift-s");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[4], CallAction::Answer { call: call(2) });
    status(&rig, 2, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    rig.send(Event::IncomingCall(invitation(3, Some(CALLER_NAME), None)));
    cx.simulate_keystrokes("ctrl-shift-d");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[5], CallAction::Decline { call: call(3) });

    // a key that fits nothing asks nothing of the engine: refused here, said
    rig.send(Event::RingEnded {
        call: call(3),
        end: RemoteEnd::default(),
    });
    cx.simulate_keystrokes("ctrl-shift-h");
    cx.run_until_parked();
    assert_eq!(actions(&rig).len(), 6);
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["hang-up", "no-call"]);
}

#[test]
fn the_switcher_offers_the_call_commands_that_fit_the_moment() {
    let rig = calling();
    let (trace, _written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    let commands = |cx: &mut VisualTestContext| {
        let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
        cx.update(|_, cx| switcher.read(cx).commands())
    };
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("call");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Call", "Make a test call"]);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("test call");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Make a test call"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::TestCall { echo: Mri::new(ECHO) }]);
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()), "closed");

    // while it is up: Leave call and Mute
    status(&rig, 1, CallStatus::Connected);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("leave");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Leave call"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[1], CallAction::HangUp { call: call(1) });
    status(&rig, 1, CallStatus::Ended(End::HungUp));

    // while one rings: Answer and Decline
    rig.send(Event::IncomingCall(invitation(2, Some(CALLER_NAME), None)));
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("call");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Answer call", "Decline call"]);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("answer");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Answer call"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[2], CallAction::Answer { call: call(2) });
}

#[test]
fn the_in_call_view_tells_a_refusal_a_failure_and_voicemail() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::Media("no relay".to_owned()))));
    click(cx, "chat-call");
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(&refused[0]), ["call", "media"]);
    let told = rig.cx.read(|cx| calls_of(&rig).read(cx).refused().cloned()).unwrap();
    assert!(told.text.contains("no relay"), "{told:?}");
    assert!(drawn(cx, "call-after"));

    // the next: the callee's voicemail takes it (declined there), and a's call says so
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Ringing);
    assert!(!drawn(cx, "call-after"), "a new call clears what the last left");
    status(&rig, 1, CallStatus::Connecting);
    rig.send(Event::Voicemail { call: call(1) });
    let voicemail = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(
        kinds(&voicemail),
        ["chat", "connecting", "unmuted", "camera-off", "voicemail"]
    );
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!(
        teams_ui::calls::status_text(&active),
        "Declined or not answered: voicemail"
    );
    status(&rig, 1, CallStatus::Ended(End::HungUp));

    // the next fails on its media
    click(cx, "chat-call");
    status(&rig, 2, CallStatus::Ringing);
    status(&rig, 2, CallStatus::Ended(End::Failed("media")));
    let failed = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(
        kinds(&failed),
        ["chat", "ended", "failed", "media", "unmuted", "camera-off"]
    );
    let ended = rig.cx.read(|cx| calls_of(&rig).read(cx).ended().cloned()).unwrap();
    assert_eq!(teams_ui::calls::end_text(&ended.end, ended.voicemail), "Call failed");
}

#[test]
fn a_build_that_shows_no_call_shows_rings_and_offers_none_of_it() {
    // the models of a run whose engine has no calling (the personal build's when its devices couldn't be opened)
    let rig = Rig::start();
    seed(&rig);
    rig.cx.update(teams_ui::window::init);
    let (trace, written) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &trace, &ringer, &server);
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    rig.send(Event::IncomingCall(invitation(1, Some(CALLER_NAME), None)));
    assert!(!drawn(cx, "chat-call") && !drawn(cx, "call-banner") && !drawn(cx, "in-call"));
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "none");
    assert_eq!(ringer.started(), 0);
    assert!(server.shown().is_empty());
    for key in [
        "ctrl-shift-c",
        "ctrl-shift-j",
        "ctrl-shift-s",
        "ctrl-shift-d",
        "ctrl-shift-h",
        "ctrl-shift-m",
    ] {
        cx.simulate_keystrokes(key);
    }
    cx.run_until_parked();
    assert!(actions(&rig).is_empty(), "nothing asked of the engine");
    assert!(!drawn(cx, "call-after"), "no refusal shown either");
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).refused().is_none()));
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("call");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    assert!(cx.update(|_, cx| switcher.read(cx).commands()).is_empty());
    let traced = written.text();
    for view in [
        "call",
        "call-button",
        "call-banner",
        "in-call",
        "ringtone",
        "call-notification",
    ] {
        assert!(!traced.contains(&format!("\"view\":\"{view}\"")), "{view}: {traced}");
    }
}

#[test]
fn a_start_asked_while_one_sets_up_is_refused_and_the_first_keeps_its_chat() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    // the engine answers a start once its media is set up
    *rig.core.hold_calls.lock().unwrap() = true;
    click(cx, "chat-call");
    assert!(!drawn(cx, "chat-call"), "no Call while the first one sets up");
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig),
        [CallAction::Call { chat: AB.into() }],
        "the second waits"
    );
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["call", "busy"]);

    // the first's media is set up and its call told: still the call of its chat
    let held = std::mem::take(&mut *rig.core.held_calls.lock().unwrap());
    assert_eq!(held.len(), 1);
    for answer in held {
        answer.send(Ok(call(1)));
    }
    rig.cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!((active.kind, active.chat.as_deref()), (CallKind::Chat, Some(AB)));
    let shown = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(shown["conversation"], AB);
}

#[test]
fn a_start_the_engine_never_tells_is_given_up_and_said_and_call_comes_back() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    *rig.core.hold_calls.lock().unwrap() = true;
    click(cx, "chat-call");
    assert!(!drawn(cx, "chat-call"), "no Call while the start is asked");
    rig.cx.executor().advance_clock(teams_ui::calls::START_TOLD_WITHIN / 2);
    assert!(!drawn(cx, "chat-call") && !drawn(cx, "call-after"), "not given up yet");
    rig.cx.executor().advance_clock(teams_ui::calls::START_TOLD_WITHIN);
    cx.run_until_parked();
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["call", "not-told"]);
    assert!(drawn(cx, "call-after"), "the user is told");
    assert!(drawn(cx, "chat-call"), "Call is offered again");

    // asked again; the engine then refuses the first as late (it was said already): the second stays asked
    click(cx, "chat-call");
    assert!(!drawn(cx, "chat-call"), "the second start is asked");
    let held = std::mem::take(&mut *rig.core.held_calls.lock().unwrap());
    assert_eq!(held.len(), 2);
    let mut held = held.into_iter();
    held.next().unwrap().send(Err(EngineError::Call(CallActionError::Late)));
    cx.run_until_parked();
    assert!(!drawn(cx, "chat-call"), "the second start is still asked");
    let told = rig.cx.read(|cx| calls_of(&rig).read(cx).refused().cloned());
    assert!(
        told.is_none_or(|told| !told.text.contains("too long")),
        "the late refusal isn't told again"
    );
}

#[test]
fn a_late_refusal_of_any_start_given_up_on_is_not_told_again() {
    let rig = calling();
    let (trace, _written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    *rig.core.hold_calls.lock().unwrap() = true;
    // two starts, each given up on in turn
    for _ in 0..2 {
        click(cx, "chat-call");
        rig.cx
            .executor()
            .advance_clock(teams_ui::calls::START_TOLD_WITHIN + std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert!(drawn(cx, "chat-call"), "given up, Call is offered again");
    }
    // the engine refuses both as late, the older first
    let held = std::mem::take(&mut *rig.core.held_calls.lock().unwrap());
    assert_eq!(held.len(), 2);
    for answer in held {
        answer.send(Err(EngineError::Call(CallActionError::Late)));
        cx.run_until_parked();
        let told = rig.cx.read(|cx| calls_of(&rig).read(cx).refused().cloned());
        assert!(
            told.is_none_or(|told| !told.text.contains("too long")),
            "the late refusal isn't told again"
        );
    }
}

#[test]
fn a_start_told_at_once_whose_media_fails_keeps_why_with_its_end_either_way_round() {
    for reply_first in [false, true] {
        let rig = calling();
        let (trace, _written) = trace();
        let (_shell, cx) = shell_window(&rig, Some(AB), trace);
        registered(&rig);
        *rig.core.hold_calls.lock().unwrap() = true;
        click(cx, "chat-call");
        // the engine tells the call as its media setup begins
        status(&rig, 1, CallStatus::Placing);
        let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
        assert_eq!((active.kind, active.chat.as_deref()), (CallKind::Chat, Some(AB)));
        let held = std::mem::take(&mut *rig.core.held_calls.lock().unwrap());
        let refuse = || {
            for answer in held {
                answer.send(Err(EngineError::Call(CallActionError::Media("no relay".to_owned()))));
            }
            rig.cx.run_until_parked();
        };
        if reply_first {
            refuse();
            status(&rig, 1, CallStatus::Ended(End::Failed("setup")));
        } else {
            status(&rig, 1, CallStatus::Ended(End::Failed("setup")));
            refuse();
        }
        rig.cx.read(|cx| {
            let calls = calls_of(&rig).read(cx);
            assert!(calls.ended().is_some_and(|ended| ended.end == End::Failed("setup")));
            let told = calls.refused().expect("why it failed is kept");
            assert!(told.text.contains("no relay"), "{told:?}");
        });
        assert!(drawn(cx, "chat-call"), "Call is offered again");
    }
}

/// The account signs out (its tokens can't be renewed): the engine is shut down, so no call event comes any more.
fn sign_out(rig: &Rig) {
    rig.cx.update(|cx| {
        rig.bridge
            .update(cx, |bridge, cx| bridge.sign_out("signed out".into(), cx))
            .detach();
    });
    rig.cx.run_until_parked();
}

#[test]
fn a_sign_out_stops_the_ring() {
    let rig = calling();
    let (trace, _written) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &trace, &ringer, &server);
    let (_shell, cx) = shell_window(&rig, None, trace);
    registered(&rig);
    rig.send(Event::IncomingCall(invitation(7, Some(CALLER_NAME), None)));
    assert_eq!((ringer.started(), ringer.stopped()), (1, 0));
    assert_eq!(server.shown().len(), 1);
    sign_out(&rig);
    assert_eq!(ringer.stopped(), 1, "the ringtone stops");
    assert_eq!(server.closed(), [1], "the notification closes");
    assert!(!drawn(cx, "call-banner"));
    assert!(actions(&rig).is_empty(), "nothing answered or declined");
}

#[test]
fn a_sign_out_shows_the_call_up_ended() {
    let rig = calling();
    let (trace, _written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    sign_out(&rig);
    assert!(!drawn(cx, "in-call"));
    assert!(drawn(cx, "call-after"));
    let ended = rig.cx.read(|cx| calls_of(&rig).read(cx).ended().cloned()).unwrap();
    assert_eq!((ended.id, ended.end), (call(1), End::HungUp));
}

#[test]
fn a_refusal_during_a_call_shows_in_its_bar_and_its_end_replaces_it() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    assert!(!drawn(cx, "in-call-refused"));
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::NoMedia)));
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert!(
        drawn(cx, "in-call") && drawn(cx, "in-call-refused"),
        "told while the call is up"
    );
    cx.simulate_keystrokes("ctrl-shift-j");
    cx.run_until_parked();
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["join", "busy"]);
    assert!(drawn(cx, "in-call-refused"));

    // once it ends, how it ended is what is told
    status(&rig, 1, CallStatus::Ended(End::Declined));
    assert!(drawn(cx, "call-after"));
    let calls = rig.cx.read(|cx| {
        let calls = calls_of(&rig).read(cx);
        (calls.refused().cloned(), calls.ended().map(|ended| ended.end.clone()))
    });
    assert_eq!(calls, (None, Some(End::Declined)));
}

#[test]
fn an_unmute_a_newer_mute_overtook_leaves_the_microphone_shown_muted_and_tells_nothing() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().muted));
    // the engine skipped the unmute (its microphone reopened after a newer mute): capture stays muted
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::Superseded)));
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Mute {
            call: call(1),
            muted: false
        })
    );
    assert!(
        rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().muted),
        "shown muted, as capture is"
    );
    assert!(!drawn(cx, "in-call-refused"), "nothing went wrong to tell");
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["mute", "superseded"]);
}

// --- video (M3 Task 18) ---

#[test]
fn video_effects_and_participants_share_one_call_pane_without_turning_the_camera_on() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    let before = actions(&rig).len();
    click(window, "call-more");
    click(window, "call-more-video-effects");
    assert!(drawn(window, "call-effects-pane"));
    assert!(drawn(window, "effects-picker"));
    assert!(!drawn(window, "call-roster-pane"));
    click(window, "effect-blur");
    let settings = rig.cx.read(|cx| shell.read(cx).settings().clone());
    assert_eq!(
        rig.cx.read(|cx| settings.read(cx).settings().video_effect.clone()),
        teams_ui::settings::VideoEffect::Blur
    );
    assert_eq!(actions(&rig).len(), before, "choosing an effect starts no call action");
    click(window, "call-roster");
    assert!(drawn(window, "call-roster-pane"));
    assert!(!drawn(window, "call-effects-pane"));
    click(window, "call-more");
    click(window, "call-more-video-effects");
    assert!(drawn(window, "call-effects-pane"));
    assert!(!drawn(window, "call-roster-pane"));
    click(window, "call-effects-close");
    assert!(!drawn(window, "call-effects-pane"));
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    no_name_in(&written);
}

/// The rig of a build that shows calls with their video, on `feed`.
fn video_calling(feed: &VideoFeed) -> Rig {
    let feed = feed.clone();
    let rig = Rig::start_with(move |cx| teams_ui::Models::with_video_calls(Some(Mri::new(ECHO)), feed, cx));
    seed(&rig);
    rig.cx.update(teams_ui::window::init);
    rig
}

#[test]
fn the_camera_is_off_when_a_call_starts_and_goes_on_and_off_by_its_key_its_button_and_the_switcher() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    assert!(drawn(cx, "call-camera"), "the camera's button");
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let up = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&up), ["chat", "connected", "unmuted", "camera-off"]);

    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig)[1],
        CallAction::Camera {
            call: call(1),
            on: true
        }
    );
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let on = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&on), ["chat", "connected", "unmuted", "camera-on"]);
    click(cx, "call-camera");
    assert_eq!(
        actions(&rig)[2],
        CallAction::Camera {
            call: call(1),
            on: false
        }
    );
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let offered = rig
        .cx
        .read(|cx| CallCommand::fitting(calls_of(&rig).read(cx), Some(AB)));
    assert_eq!(
        offered,
        [
            CallCommand::HangUp,
            CallCommand::ToggleMute,
            CallCommand::ToggleCaptions,
            CallCommand::ToggleCamera
        ]
    );

    // a camera that went off by itself (its device gone, its encoder failed) is shown off
    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    rig.send(Event::CameraOff { call: call(1) });
    cx.run_until_parked();
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let off = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&off), ["chat", "connected", "unmuted", "camera-off"]);

    // the engine's refusal (a call without video) is told, and the camera stays as it was
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::NoVideo)));
    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    assert_eq!(
        kinds(lines(&written, "call", "refused").last().unwrap()),
        ["camera", "no-video"]
    );
    no_name_in(&written);
}

#[test]
fn the_test_call_shows_as_a_call_with_echo_its_camera_and_its_self_view() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("test call");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::TestCall { echo: Mri::new(ECHO) }]);
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);

    // Echo is the other participant, as a 1:1 call's other member is
    let bar = cx.update(|_, cx| shell.read(cx).call_bar().clone());
    let participants = cx.update(|_, cx| bar.read(cx).participants().to_vec());
    assert_eq!(participants, [(ECHO.to_owned(), call_view::ECHO_IN_CALL.into())]);
    let up = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&up), ["test-call", "connected", "unmuted", "camera-off"]);
    assert_eq!(up["people"], serde_json::json!([ECHO]));
    for control in [
        "call-return",
        "call-mute",
        "call-camera",
        "call-hang-up",
        "in-call-participants",
    ] {
        assert!(drawn(cx, control), "{control}");
    }

    // the roster names the echo without a name: a cell of its own; the camera turns on and the self view shows
    rig.send(Event::CallParticipants {
        call: call(1),
        view: CallView {
            participants: vec![CallParticipant {
                mri: ECHO.to_owned(),
                name: None,
                video_on: false,
                line: None,
                speaking: true,
                muted: false,
                hand_raised: false,
                organizer: false,
                joined: None,
                sharing: false,
            }],
            speaking: false,
            hearing: false,
        },
    });
    click(cx, "call-camera");
    assert_eq!(
        actions(&rig)[1],
        CallAction::Camera {
            call: call(1),
            on: true
        }
    );
    let pattern = TestPattern::new(320, 180);
    let output: &dyn VideoOutput = &feed;
    output.preview().unwrap().show(&pattern.frame(1)).unwrap();
    cx.run_until_parked();
    let window = stage(&rig, &windows);
    for shown in [
        "call-grid",
        "call-participant-0",
        "call-participant-self",
        "video-tile-self",
        "call-mute",
        "call-camera",
        "call-hang-up",
    ] {
        assert!(drawn(window, shown), "{shown}");
    }
    let cells = lines(&written, "call-participant", "shown");
    assert_eq!(cells.last().unwrap()["people"], serde_json::json!([ECHO]));
    no_name_in(&written);
}

#[test]
fn the_feed_s_frames_draw_as_tiles_while_the_call_is_up_and_go_with_it() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    assert!(drawn(window, "call-person-0"), "b's tile, before the roster");
    assert!(!drawn(window, "video-tile-remote"), "no frame yet");

    // the call's media shows the far end's frame and the camera's on the feed (I420, the test pattern)
    let pattern = TestPattern::new(320, 180);
    let output: &dyn VideoOutput = &feed;
    output.remote(0).show(&pattern.frame(1)).unwrap();
    output.preview().unwrap().show(&pattern.frame(2)).unwrap();
    assert!(drawn(window, "video-tile-remote") && drawn(window, "video-tile-self"));
    let shown: Vec<Vec<String>> = lines(&written, "video-tile", "shown").iter().map(kinds).collect();
    assert_eq!(shown, [["remote"], ["self"]]);
    let remote = feed.picture(Tile::REMOTE).unwrap();
    assert_eq!(
        (remote.width, remote.height, remote.bgra.len()),
        (320, 180, 320 * 180 * 4)
    );

    // a frame not in I420 isn't taken
    let mut wrong = pattern.frame(3);
    wrong.format = teams_media::devices::PixelFormat::Rgb;
    assert!(output.remote(0).show(&wrong).is_err());

    status(&rig, 1, CallStatus::Ended(End::HungUp));
    assert!(!window_open(&rig, &windows), "a call's window closes with it");
    let hidden: Vec<Vec<String>> = lines(&written, "video-tile", "hidden").iter().map(kinds).collect();
    assert_eq!(hidden, [["remote"], ["self"]]);
    assert!(feed.picture(Tile::REMOTE).is_none(), "the call's frames go with it");
}

/// The view of a group call: `who`, each `(MRI, roster name, camera on, line, speaking)`, and whether the account
/// speaks.
fn view(who: &[(&str, &str, bool, Option<usize>, bool)], speaking: bool) -> CallView {
    CallView {
        participants: who
            .iter()
            .map(|(mri, name, video_on, line, speaking)| CallParticipant {
                mri: (*mri).to_owned(),
                name: Some((*name).to_owned()),
                video_on: *video_on,
                line: *line,
                speaking: *speaking,
                muted: false,
                hand_raised: false,
                organizer: false,
                joined: None,
                sharing: false,
            })
            .collect(),
        speaking,
        hearing: speaking,
    }
}

#[test]
fn a_group_call_s_grid_shows_each_participant_its_video_and_who_speaks() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(TRIO), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let cx = stage(&rig, &windows);
    assert!(!drawn(cx, "call-grid"), "no roster told yet");

    // the roster tells b (camera on, line 1) and c (camera off, line 2, speaking): a cell each and the account's own
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(
            &[
                (LAB_B, "Lab Bee", true, Some(1), false),
                (LAB_C, "Lab Sea", false, Some(2), true),
            ],
            false,
        ),
    });
    let pattern = TestPattern::new(320, 180);
    let output: &dyn VideoOutput = &feed;
    output.remote(1).show(&pattern.frame(1)).unwrap();
    cx.run_until_parked();
    for cell in [
        "call-grid",
        "call-participant-0",
        "call-participant-1",
        "call-participant-self",
    ] {
        assert!(drawn(cx, cell), "{cell}");
    }
    assert!(drawn(cx, "video-tile-line-1"), "b's video on its line");
    assert!(!drawn(cx, "video-tile-line-2"), "no frame of c's yet");
    let cells = lines(&written, "call-participant", "shown");
    assert_eq!(
        cells.iter().map(kinds).collect::<Vec<_>>(),
        [["camera-on", "line-1", "quiet"], ["camera-off", "line-2", "speaking"]]
    );
    assert_eq!(cells[0]["people"], serde_json::json!([LAB_B]));
    assert_eq!(
        kinds(lines(&written, "call-participant", "self").last().unwrap()),
        ["quiet", "mic-silent"]
    );

    // c leaves and the account speaks: the grid follows
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&[(LAB_B, "Lab Bee", true, Some(1), false)], true),
    });
    cx.run_until_parked();
    draw(cx);
    assert!(!drawn(cx, "call-participant-1"));
    assert_eq!(
        kinds(lines(&written, "call-participant", "self").last().unwrap()),
        ["speaking", "mic-sound"]
    );
    no_name_in(&written);
}

#[test]
fn nine_participants_each_draw_on_a_line_of_their_own() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, _written) = trace();
    let (shell, cx) = shell_window(&rig, Some(TRIO), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let cx = stage(&rig, &windows);
    let mris: Vec<String> = (1..=9)
        .map(|n| format!("8:orgid:00000000-0000-4000-8000-0000000001{n:02}"))
        .collect();
    let who: Vec<(&str, &str, bool, Option<usize>, bool)> = mris
        .iter()
        .zip(1..)
        .map(|(mri, line)| (mri.as_str(), "Someone", true, Some(line), false))
        .collect();
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&who, false),
    });
    let pattern = TestPattern::new(320, 180);
    let output: &dyn VideoOutput = &feed;
    for line in 1..=9 {
        output.remote(line).show(&pattern.frame(1)).unwrap();
    }
    // a line past the ninth has no tile: its frames are dropped
    output.remote(10).show(&pattern.frame(1)).unwrap();
    cx.run_until_parked();
    for line in 1..=9 {
        // a frame of its own just before the look: a loaded machine draws a grid slower than a frame goes stale
        output.remote(line).show(&pattern.frame(2)).unwrap();
        assert!(drawn(cx, &format!("video-tile-line-{line}")), "line {line}");
    }
    assert!(feed.picture(Tile::Line(10)).is_none());
}

#[test]
fn a_build_without_video_has_no_camera_and_its_key_asks_nothing() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    assert!(!drawn(cx, "call-camera"));
    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert_eq!(actions(&rig).len(), 1, "only the call");
    assert_eq!(
        kinds(lines(&written, "call", "refused").last().unwrap()),
        ["camera", "no-video"]
    );
    let offered = rig
        .cx
        .read(|cx| CallCommand::fitting(calls_of(&rig).read(cx), Some(AB)));
    assert_eq!(
        offered,
        [
            CallCommand::HangUp,
            CallCommand::ToggleMute,
            CallCommand::ToggleCaptions
        ]
    );
}

#[test]
fn a_notification_says_who_calls_as_the_invitation_names_them() {
    let one = notice_of(&invitation(1, Some(CALLER_NAME), None));
    assert_eq!(
        (
            one.kind,
            one.summary.as_str(),
            one.body.as_str(),
            one.conversation.as_str()
        ),
        (NoticeKind::Call, "Bee Caller is calling", "Incoming call", "")
    );
    let group = notice_of(&invitation(2, None, Some(TRIO)));
    assert_eq!(
        (group.summary.as_str(), group.body.as_str(), group.conversation.as_str()),
        ("Someone is calling", "Incoming group call", TRIO)
    );
}

/// The settings shown on their Calls page.
fn calls_page(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    cx.update(|_, cx| {
        shell.update(cx, Shell::show_settings);
        let view = shell.read(cx).settings_view().clone();
        view.update(cx, |view, cx| view.show_page(Page::Calls, cx));
    });
    draw(cx);
}

fn offer(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> TestCallOffer {
    cx.update(|_, cx| shell.read(cx).test_call_offer(cx))
}

#[test]
fn the_settings_calls_page_makes_a_test_call_as_teams_does() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    // the Calls page is there for the test call alone (no devices listed, no video)
    calls_page(&shell, cx);
    assert!(drawn(cx, "settings-page-calls"));
    assert_eq!(offer(&shell, cx), TestCallOffer::Connecting);
    click(cx, "make-test-call");
    assert!(
        actions(&rig).is_empty(),
        "disabled until the calling endpoint is registered"
    );

    registered(&rig);
    assert_eq!(offer(&shell, cx), TestCallOffer::Ready);
    click(cx, "make-test-call");
    assert_eq!(actions(&rig), [CallAction::TestCall { echo: Mri::new(ECHO) }]);
    let asked = lines(&written, "call", "asked");
    assert_eq!(kinds(asked.last().unwrap()), ["test-call"]);
    // the call shows in the call bar over the settings, which stay
    status(&rig, 1, CallStatus::Placing);
    assert!(drawn(cx, "call-hang-up"));
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);

    // while it is up the button is disabled, and a press asks nothing
    status(&rig, 1, CallStatus::Connected);
    assert_eq!(offer(&shell, cx), TestCallOffer::InCall);
    click(cx, "make-test-call");
    assert_eq!(actions(&rig).len(), 1);
    no_name_in(&written);
}

/// The echo's voice playing, the microphone hearing the account.
fn echo_heard(rig: &Rig, number: u64) {
    rig.send(Event::CallParticipants {
        call: call(number),
        view: CallView {
            participants: vec![CallParticipant {
                mri: ECHO.to_owned(),
                name: None,
                video_on: false,
                line: None,
                speaking: true,
                muted: false,
                hand_raised: false,
                organizer: false,
                joined: None,
                sharing: false,
            }],
            speaking: false,
            hearing: true,
        },
    });
}

#[test]
fn a_test_call_ends_in_its_results_and_they_lead_to_the_device_settings() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, asked) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    echo_heard(&rig, 1);
    // the echo left once it played the recording back: the call ended by itself
    status(&rig, 1, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    let ended = cx.update(|_, cx| calls_of(&rig).read(cx).ended().cloned()).unwrap();
    let check = ended.check.expect("a test call's results");
    assert!(check.connected && check.heard && check.played && !check.camera);
    assert!(check.passed());
    // its window stays, on its results, as Teams' test call's does; the main window's strip says nothing of them
    let window = stage(&rig, &windows);
    for shown in [
        "test-call-results",
        "test-call-check-microphone",
        "test-call-check-speaker",
        "test-call-check-camera",
        "test-call-check-network",
        "test-call-device-settings",
        "test-call-close",
    ] {
        assert!(drawn(window, shown), "{shown}");
    }
    assert!(!drawn(cx, "call-after"));
    let results = lines(&written, "test-call", "results");
    assert_eq!(results.len(), 1);
    assert_eq!(
        kinds(&results[0]),
        ["connected", "mic-heard", "speaker-played", "camera-untried"]
    );

    click(window, "test-call-device-settings");
    let me = rig.cx.read(|cx| shell.read(cx).account().to_owned());
    assert_eq!(*asked.lock().unwrap(), [CallWindowsEvent::DeviceSettings(me)]);
    // the next test call can start
    assert_eq!(offer(&shell, cx), TestCallOffer::Ready);
    click(window, "test-call-close");
    assert!(!window_open(&rig, &windows));
    assert!(
        rig.cx.read(|cx| calls_of(&rig).read(cx).ended().is_none()),
        "its results dismissed"
    );
    no_name_in(&written);
}

#[test]
fn a_test_call_that_never_connected_says_so_in_its_results() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Ended(End::Failed("media")));
    let check = cx
        .update(|_, cx| calls_of(&rig).read(cx).ended().cloned())
        .and_then(|ended| ended.check)
        .unwrap();
    assert!(!check.passed());
    assert_eq!(check.network(), (Verdict::Problem, "Couldn't connect"));
    assert_eq!(check.microphone().0, Verdict::Untried);
    assert!(drawn(stage(&rig, &windows), "test-call-results"));
    assert_eq!(
        kinds(&lines(&written, "test-call", "results")[0]),
        ["not-connected", "mic-silent", "speaker-silent", "camera-untried"]
    );
    // a call from a chat ends as before, with no results
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::Call { chat: AB.to_owned() }, cx)));
    cx.run_until_parked();
    status(&rig, 2, CallStatus::Placing);
    status(&rig, 2, CallStatus::Ended(End::HungUp));
    let ended = cx.update(|_, cx| calls_of(&rig).read(cx).ended().cloned()).unwrap();
    assert_eq!(ended.kind, CallKind::Chat);
    assert!(ended.check.is_none());
    assert!(
        !window_open(&rig, &windows),
        "the new call took the results' window, and closed with it"
    );
    assert!(!drawn(cx, "call-after"), "the main window says nothing of it");
}

#[test]
fn a_camera_turned_on_in_the_test_call_is_checked() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, _written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    click(cx, "call-camera");
    let up = cx.update(|_, cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert!(up.camera && up.check.camera);
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    let check = cx
        .update(|_, cx| calls_of(&rig).read(cx).ended().cloned())
        .and_then(|ended| ended.check)
        .unwrap();
    assert_eq!(check.camera(), (Verdict::Fine, "Turned on"));
}

#[test]
fn a_camera_that_stopped_by_itself_in_the_test_call_failed_its_check() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    echo_heard(&rig, 1);
    click(cx, "call-camera");
    // its device gone: the camera stops by itself
    rig.send(Event::CameraOff { call: call(1) });
    status(&rig, 1, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    let check = cx
        .update(|_, cx| calls_of(&rig).read(cx).ended().cloned())
        .and_then(|ended| ended.check)
        .unwrap();
    assert_eq!(check.camera(), (Verdict::Problem, "Didn't work"));
    assert!(!check.passed());
    assert_eq!(
        kinds(&lines(&written, "test-call", "results")[0]),
        ["connected", "mic-heard", "speaker-played", "camera-failed"]
    );
}

#[test]
fn a_test_call_that_failed_after_it_connected_doesn_t_pass() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    echo_heard(&rig, 1);
    // its media lost for good: the engine fails the call
    status(&rig, 1, CallStatus::Ended(End::Failed("media")));
    let check = cx
        .update(|_, cx| calls_of(&rig).read(cx).ended().cloned())
        .and_then(|ended| ended.check)
        .unwrap();
    assert_eq!(check.network(), (Verdict::Problem, "The call dropped"));
    assert!(!check.passed());
    assert_eq!(
        kinds(&lines(&written, "test-call", "results")[0]),
        ["dropped", "mic-heard", "speaker-played", "camera-untried"]
    );
}

#[test]
fn a_microphone_or_speaker_that_went_away_in_the_test_call_failed_its_check() {
    for microphone in [true, false] {
        let rig = calling();
        let (trace, written) = trace();
        let (shell, cx) = shell_window(&rig, Some(AB), trace);
        registered(&rig);
        cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
        cx.run_until_parked();
        status(&rig, 1, CallStatus::Placing);
        status(&rig, 1, CallStatus::Connected);
        echo_heard(&rig, 1);
        // it worked, then its stream ended for good (the sound server restarted)
        rig.send(Event::AudioClosed {
            call: call(1),
            microphone,
        });
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| calls_of(&rig).read(cx).call().unwrap().muted),
            microphone
        );
        status(&rig, 1, CallStatus::Ended(End::Remote(RemoteEnd::default())));
        let check = cx
            .update(|_, cx| calls_of(&rig).read(cx).ended().cloned())
            .and_then(|ended| ended.check)
            .unwrap();
        let (lost, kept) = if microphone {
            (check.microphone(), check.speaker())
        } else {
            (check.speaker(), check.microphone())
        };
        assert_eq!(lost, (Verdict::Problem, "Stopped during the call"));
        assert_eq!(kept.0, Verdict::Fine);
        assert!(!check.passed());
        let expected = if microphone {
            ["connected", "mic-lost", "speaker-played", "camera-untried"]
        } else {
            ["connected", "mic-heard", "speaker-lost", "camera-untried"]
        };
        assert_eq!(kinds(&lines(&written, "test-call", "results")[0]), expected);
    }
}

#[test]
fn a_camera_that_couldn_t_turn_on_in_the_test_call_failed_its_check() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, _written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.ask_call(Ask::TestCall, cx)));
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    echo_heard(&rig, 1);
    // the media can't open the camera: the ask is refused, and the camera failed
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::Media("no camera".to_owned()))));
    click(cx, "call-camera");
    status(&rig, 1, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    let check = cx
        .update(|_, cx| calls_of(&rig).read(cx).ended().cloned())
        .and_then(|ended| ended.check)
        .unwrap();
    assert_eq!(check.camera(), (Verdict::Problem, "Didn't work"));
    assert!(!check.passed());
}

#[test]
fn a_build_without_calls_offers_no_test_call_in_the_settings() {
    let rig = Rig::start();
    seed(&rig);
    rig.cx.update(teams_ui::window::init);
    let (trace, _written) = trace();
    let (shell, cx) = shell_window(&rig, None, trace);
    registered(&rig);
    assert_eq!(offer(&shell, cx), TestCallOffer::Hidden);
    calls_page(&shell, cx);
    assert!(!drawn(cx, "settings-page-calls"));
    assert!(!drawn(cx, "make-test-call"));
}

// --- the call's own window ---

#[test]
fn a_call_opens_a_window_of_its_own_beside_the_chats_and_it_closes_with_the_call() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "chat-call");
    assert!(!window_open(&rig, &windows), "not before the engine told the call");
    status(&rig, 1, CallStatus::Placing);
    let window = stage(&rig, &windows);
    assert!(drawn(window, "call-calling"), "who it rings, while nobody is in it");
    for control in ["call-roster", "call-more", "call-mute", "call-hang-up"] {
        assert!(drawn(window, control), "{control}");
    }
    assert!(!drawn(window, "call-camera"), "no camera without the codec");
    let opened = lines(&written, "call-window", "opened");
    assert_eq!((opened.len(), kinds(&opened[0])), (1, vec!["chat".to_owned()]));
    // the main window keeps its chat, with the way back to the call above it
    assert!(drawn(cx, "in-call") && drawn(cx, "call-return"));
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Chat(AB.into()));

    the_window_s_own_controls_ask(&rig, window, cx, &written);

    click(window, "call-hang-up");
    assert_eq!(actions(&rig)[2], CallAction::HangUp { call: call(1) });
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    assert!(!window_open(&rig, &windows));
    let closed = lines(&written, "call-window", "closed");
    assert_eq!((closed.len(), kinds(&closed[0])), (1, Vec::<String>::new()));
    assert!(
        !drawn(cx, "call-after"),
        "the main window says nothing of a hang-up of the user's"
    );
    no_name_in(&written);
}

/// Connected: b's tile; the window's own keys and buttons ask as the main window's do, People opens and closes the
/// participants' pane, and the main window's "Return to call" raises the window.
fn the_window_s_own_controls_ask(
    rig: &Rig,
    window: &mut VisualTestContext,
    cx: &mut VisualTestContext,
    written: &Written,
) {
    status(rig, 1, CallStatus::Connected);
    assert!(drawn(window, "call-person-0"));
    window.simulate_keystrokes("ctrl-shift-m");
    window.run_until_parked();
    assert_eq!(
        actions(rig)[1],
        CallAction::Mute {
            call: call(1),
            muted: true
        }
    );
    click(window, "call-roster");
    for row in ["call-roster-pane", "call-roster-self", "call-roster-0"] {
        assert!(drawn(window, row), "{row}");
    }
    click(window, "call-roster-close");
    assert!(!drawn(window, "call-roster-pane"));
    click(cx, "call-return");
    assert_eq!(lines(written, "call-window", "raised").len(), 1);
}

#[test]
fn closing_the_call_window_leaves_the_call_and_return_to_call_brings_it_back() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, asked) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    assert!(window.simulate_close(), "the window closes");
    rig.cx.run_until_parked();
    assert_eq!(actions(&rig).last(), Some(&CallAction::HangUp { call: call(1) }));
    assert_eq!(kinds(&lines(&written, "call-window", "closed")[0]), ["left"]);
    assert_eq!(*asked.lock().unwrap(), [CallWindowsEvent::Closed]);
    // its leave not through yet (or refused): the call is still up, its window not opened again by itself
    status(&rig, 1, CallStatus::Connected);
    assert!(!window_open(&rig, &windows));
    assert!(drawn(cx, "in-call"));
    // "Return to call" opens it again
    click(cx, "call-return");
    assert!(window_open(&rig, &windows));
    assert_eq!(lines(&written, "call-window", "opened").len(), 2);
}

#[test]
fn the_call_window_s_device_menus_choose_the_settings_devices() {
    let rig = calling();
    let (trace, _written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    let device = |kind, name: &str, description: &str| DeviceInfo {
        kind,
        name: name.to_owned(),
        description: description.to_owned(),
    };
    let directory = FixedDirectory::new(
        DeviceList::new(vec![
            device(DeviceKind::Microphone, "built-in-mic", "Built-in microphone"),
            device(DeviceKind::Microphone, "usb-mic", "USB headset"),
            device(DeviceKind::Speaker, "built-in-speaker", "Speakers"),
        ])
        .with_default(DeviceKind::Microphone, Some("built-in-mic")),
    );
    let model = rig.cx.read(|cx| shell.read(cx).settings().clone());
    rig.cx
        .update(|cx| attach_devices(&model, Arc::new(directory.clone()), cx));
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    click(window, "call-audio-devices");
    for row in [
        "call-microphone-system",
        "call-microphone-built-in-mic",
        "call-microphone-usb-mic",
        "call-speaker-system",
        "call-speaker-built-in-speaker",
    ] {
        assert!(drawn(window, row), "{row}");
    }
    click(window, "call-microphone-usb-mic");
    let chosen = rig.cx.read(|cx| model.read(cx).settings().microphone.clone());
    assert_eq!(chosen.as_deref(), Some("usb-mic"));
    assert_eq!(
        directory.chosen(DeviceKind::Microphone).as_deref(),
        Some("usb-mic"),
        "the call's microphone moves to it"
    );
    // More's "Device settings"
    click(window, "call-more");
    assert!(drawn(window, "call-more-device-settings"));
}

#[test]
fn an_answered_invitation_opens_the_call_window_and_a_declined_one_none() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    // declined: the invitation's banner goes, and no window opens for the decline
    rig.send(Event::IncomingCall(invitation(3, Some(CALLER_NAME), None)));
    assert!(
        !window_open(&rig, &windows),
        "a ringing invitation has its banner, not a window"
    );
    click(cx, "call-decline");
    status(&rig, 3, CallStatus::Declining);
    assert!(!window_open(&rig, &windows), "a decline opens no window");
    status(&rig, 3, CallStatus::Ended(End::Declined));
    assert!(!window_open(&rig, &windows));
    assert!(lines(&written, "call-window", "opened").is_empty());

    // answered: the window opens on the answer, the caller's tile once connected
    rig.send(Event::IncomingCall(invitation(4, Some(CALLER_NAME), None)));
    click(cx, "call-answer");
    status(&rig, 4, CallStatus::Answering);
    let window = stage(&rig, &windows);
    assert_eq!(
        rig.cx.read(|cx| windows.read(cx).showing().map(|(_, call)| call)),
        Some(call(4))
    );
    assert!(drawn(window, "call-window"));
    status(&rig, 4, CallStatus::Connected);
    assert!(drawn(window, "call-person-0"), "the caller's tile");
    let opened = lines(&written, "call-window", "opened");
    assert_eq!((opened.len(), kinds(&opened[0])), (1, vec!["incoming".to_owned()]));
    no_name_in(&written);
}

// --- how a call ended, in its window ---

/// The far end's hang-up, in the captured shape: its own end ("local" to its sender), naming its sender.
fn hung_up_by(mri: &str, name: &str) -> End {
    End::Remote(RemoteEnd {
        code: Some(0),
        sub_code: Some(0),
        phrase: Some("CallEndReasonLocalUserInitiated".to_owned()),
        by: Some(Mri::new(mri)),
        by_name: Some(name.to_owned()),
        categories: vec!["Success".to_owned()],
        ..RemoteEnd::default()
    })
}

/// How call 1, connected for `connected` or never, ended with `end`.
fn ended_with(end: End, chat: Option<&str>, connected: bool) -> Ended {
    Ended {
        id: call(1),
        kind: CallKind::Chat,
        chat: chat.map(str::to_owned),
        caller: None,
        caller_name: None,
        end,
        voicemail: false,
        duration: connected.then(|| std::time::Duration::from_secs(63)),
        check: None,
    }
}

#[test]
fn an_end_is_told_by_its_cause_in_plain_words_never_by_its_code() {
    let reason = |end: End, connected: bool| ended_with(end, Some(AB), connected).reason(LAB_A);
    let ended_by_b = reason(hung_up_by(LAB_B, "Lab Bee"), true);
    assert_eq!(ended_by_b, EndReason::EndedBy(Some("Lab Bee".to_owned())));
    assert_eq!(ended_by_b.text("B").as_deref(), Some("Lab Bee ended the call"));
    // the same end sent by the account's own other device
    assert_eq!(reason(hung_up_by(LAB_A, "Lab A"), true), EndReason::EndedElsewhere);
    // the user's own hang-up and decline close the window
    assert_eq!(reason(End::HungUp, true), EndReason::Own);
    assert!(!ended_with(End::HungUp, Some(AB), true).stays_in_window());
    assert!(!ended_with(End::Declined, Some(AB), false).stays_in_window());
    assert!(ended_with(hung_up_by(LAB_B, "Lab Bee"), Some(AB), true).stays_in_window());
    // a failure: dropped once connected, else never connected
    let gave_up = End::Remote(RemoteEnd {
        phrase: Some("SdpParsingFailure".to_owned()),
        categories: vec!["JoinFailure".to_owned(), "MediaNegotiationFailure".to_owned()],
        ..RemoteEnd::default()
    });
    assert_eq!(reason(gave_up.clone(), true), EndReason::Dropped);
    assert_eq!(reason(gave_up, false), EndReason::NotConnected);
    assert_eq!(reason(End::Failed("media"), true), EndReason::Dropped);
    assert_eq!(reason(End::Failed("start"), false), EndReason::NotConnected);
    assert_eq!(reason(End::Remote(RemoteEnd::default()), true), EndReason::Ended);
    let mut voicemail = ended_with(End::Remote(RemoteEnd::default()), Some(AB), true);
    voicemail.voicemail = true;
    assert_eq!(voicemail.reason(LAB_A), EndReason::Voicemail);
}

#[test]
fn a_conversation_s_end_that_names_nobody_is_told_by_its_sub_code() {
    let reason = |end: End, connected: bool| ended_with(end, Some(AB), connected).reason(LAB_A);
    let conversation_end = |sub_code| {
        End::Remote(RemoteEnd {
            code: Some(0),
            sub_code: Some(sub_code),
            phrase: Some("This conversation has ended as no one else has joined the group call.".to_owned()),
            categories: vec!["Success".to_owned()],
            ..RemoteEnd::default()
        })
    };
    assert_eq!(reason(conversation_end(5013), true), EndReason::NobodyJoined);
    assert_eq!(reason(conversation_end(5002), true), EndReason::EveryoneLeft);
    // as captured, the conversation's end names the last to leave as its sender: still everyone else left
    let mut last_left = conversation_end(5002);
    if let End::Remote(remote) = &mut last_left {
        remote.by = Some(Mri::new(LAB_B));
        remote.by_name = Some("Lab Bee".to_owned());
    }
    assert_eq!(reason(last_left, true), EndReason::EveryoneLeft);
    // no reason's words carry a code or a phrase the service sent
    for reason in [
        EndReason::EndedBy(None),
        EndReason::EndedElsewhere,
        EndReason::Voicemail,
        EndReason::NobodyJoined,
        EndReason::EveryoneLeft,
        EndReason::ChatHasCall,
        EndReason::Dropped,
        EndReason::NotConnected,
    ] {
        let text = reason.text("Lab Bee").unwrap();
        assert!(!text.contains("CallEndReason") && !text.contains('('), "{text}");
    }
}

#[test]
fn a_call_the_far_end_hung_up_keeps_its_window_on_who_ended_it_with_call_back() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    status(&rig, 1, CallStatus::Ended(hung_up_by(LAB_B, "Lab Bee")));
    assert!(window_open(&rig, &windows), "the window stays on how the call ended");
    for part in [
        "call-end",
        "call-end-heading",
        "call-end-reason",
        "call-end-duration",
        "call-end-call-back",
        "call-end-close",
    ] {
        assert!(drawn(window, part), "{part}");
    }
    assert!(!drawn(window, "call-hang-up"), "no call controls once it ended");
    assert!(
        !drawn(cx, "call-after") && !drawn(cx, "in-call"),
        "the main window says nothing of it"
    );
    let ended = rig.cx.read(|cx| calls_of(&rig).read(cx).ended().cloned()).unwrap();
    assert_eq!(ended.reason(LAB_A), EndReason::EndedBy(Some("Lab Bee".to_owned())));
    let told = lines(&written, "call-window", "ended");
    assert_eq!(
        (told.len(), kinds(&told[0])),
        (1, vec!["ended-by-other".to_owned(), "call-back".to_owned()])
    );

    // Call back: a new call to the same chat, the user's ask
    click(window, "call-end-call-back");
    assert_eq!(actions(&rig).last(), Some(&CallAction::Call { chat: AB.into() }));
    assert!(!drawn(window, "call-end-call-back"), "not asked twice while it sets up");
    status(&rig, 2, CallStatus::Placing);
    assert_eq!(
        rig.cx.read(|cx| windows.read(cx).showing().map(|(_, call)| call)),
        Some(call(2)),
        "the new call takes the window"
    );
    no_name_in(&written);
}

#[test]
fn a_call_s_end_closes_by_itself_or_by_close() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, asked) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    // dropped once connected: said, then gone by itself
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Placing);
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    status(&rig, 1, CallStatus::Ended(End::Failed("media")));
    assert!(drawn(window, "call-end") && drawn(window, "call-end-call-back"));
    let told = lines(&written, "call-window", "ended");
    assert_eq!(kinds(&told[0]), ["dropped", "call-back"]);
    rig.cx.executor().advance_clock(AUTO_CLOSE / 2);
    assert!(window_open(&rig, &windows), "not before its time");
    rig.cx.executor().advance_clock(AUTO_CLOSE);
    assert!(!window_open(&rig, &windows), "closed by itself");
    assert!(
        rig.cx.read(|cx| calls_of(&rig).read(cx).ended().is_none()),
        "and dismissed"
    );
    assert_eq!(asked.lock().unwrap().last(), Some(&CallWindowsEvent::Closed));

    // never connected: Close
    click(cx, "chat-call");
    status(&rig, 2, CallStatus::Placing);
    let window = stage(&rig, &windows);
    status(&rig, 2, CallStatus::Ended(End::Failed("start")));
    assert!(drawn(window, "call-end"));
    assert!(!drawn(window, "call-end-duration"), "it never lasted");
    assert_eq!(
        kinds(&lines(&written, "call-window", "ended")[1]),
        ["not-connected", "call-back"]
    );
    click(window, "call-end-close");
    assert!(!window_open(&rig, &windows));
    assert_eq!(actions(&rig).len(), 2, "nothing asked of the engine but the two calls");
}

#[test]
fn a_dropped_group_call_offers_rejoin_and_a_one_to_one_invitation_call_back_by_their_keys() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    // a group call answered, then dropped: it runs on without the account, to be joined again
    rig.send(Event::IncomingCall(invitation(4, Some(CALLER_NAME), Some(TRIO))));
    click(cx, "call-answer");
    status(&rig, 4, CallStatus::Answering);
    status(&rig, 4, CallStatus::Connected);
    let window = stage(&rig, &windows);
    status(&rig, 4, CallStatus::Ended(End::Failed("media")));
    assert!(drawn(window, "call-end-rejoin") && !drawn(window, "call-end-call-back"));
    window.simulate_keystrokes("ctrl-shift-j");
    window.run_until_parked();
    assert_eq!(actions(&rig).last(), Some(&CallAction::Join { chat: TRIO.into() }));
    status(&rig, 5, CallStatus::Joining);
    status(&rig, 5, CallStatus::Ended(End::HungUp));
    assert!(!window_open(&rig, &windows), "the user's own hang-up closes it at once");

    // a 1:1 invitation answered, the caller hanging up: Call back goes to the two's chat
    rig.send(Event::IncomingCall(invitation(6, Some(CALLER_NAME), None)));
    click(cx, "call-answer");
    status(&rig, 6, CallStatus::Connected);
    let window = stage(&rig, &windows);
    status(&rig, 6, CallStatus::Ended(hung_up_by(LAB_B, CALLER_NAME)));
    assert!(drawn(window, "call-end-call-back"));
    window.simulate_keystrokes("ctrl-shift-c");
    window.run_until_parked();
    assert_eq!(actions(&rig).last(), Some(&CallAction::Call { chat: AB.into() }));
    let told: Vec<Vec<String>> = lines(&written, "call-window", "ended").iter().map(kinds).collect();
    assert_eq!(told, [["dropped", "rejoin"], ["ended-by-other", "call-back"]]);
    no_name_in(&written);
}

// --- a meeting's pre-join ---

const MEETING_CHAT: &str = "19:meeting_MadeUpMeeting0001@thread.v2";

fn meeting_to_join() -> teams_ui::calls::MeetingToJoin {
    teams_ui::calls::MeetingToJoin::Chat {
        chat: MEETING_CHAT.to_owned(),
        meeting: teams_core::MeetingInfo {
            organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
            tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
        },
        title: "Synthetic meeting".to_owned(),
    }
}

fn join_meeting_action() -> CallAction {
    let to_join = meeting_to_join();
    CallAction::JoinMeeting {
        chat: to_join.chat().unwrap().to_owned(),
        meeting: to_join.meeting().unwrap().clone(),
    }
}

/// The pre-join's window `windows` have open, to draw and click in.
fn prejoin_window(rig: &Rig, windows: &Entity<CallWindows>) -> &'static mut VisualTestContext {
    rig.cx.run_until_parked();
    let window = rig
        .cx
        .read(|cx| windows.read(cx).prejoin_window())
        .expect("the pre-join's window is open");
    let cx = Box::leak(Box::new(VisualTestContext::from_window(window.into(), &rig.cx)));
    draw(cx);
    cx
}

#[test]
fn a_meetings_pre_join_sends_nothing_until_join_now_and_gives_way_to_its_call() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let window = prejoin_window(&rig, &windows);
    for shown in [
        "prejoin",
        "prejoin-title",
        "prejoin-audio",
        "prejoin-microphone",
        "prejoin-cancel",
        "prejoin-join",
    ] {
        assert!(drawn(window, shown), "{shown}");
    }
    assert!(actions(&rig).is_empty(), "nothing goes out while it shows");

    click(window, "prejoin-join");
    assert_eq!(actions(&rig), [join_meeting_action()]);
    click(window, "prejoin-join");
    assert_eq!(actions(&rig).len(), 1, "one join while it goes");

    status(&rig, 1, CallStatus::Joining);
    assert!(
        rig.cx.read(|cx| windows.read(cx).prejoin_window().is_none()),
        "the call's window takes over"
    );
    assert!(window_open(&rig, &windows));
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!(active.kind, CallKind::Meeting);
    assert_eq!(active.chat.as_deref(), Some(MEETING_CHAT));
    assert_eq!(active.title.as_deref(), Some("Synthetic meeting"));
    assert!(teams_ui::calls::status_text(&active).contains("Joining"));
    no_name_in(&written);
}

#[test]
fn a_pre_join_cancelled_after_join_now_hangs_its_call_up_and_never_turns_the_camera_on() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let _window = prejoin_window(&rig, &windows);
    rig.cx
        .update(|cx| calls_of(&rig).update(cx, |calls, cx| calls.set_prejoin_camera(true, cx)));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    rig.cx
        .update(|cx| teams_ui::calls::join_now(&calls_of(&rig), &core, &trace, cx));
    assert_eq!(actions(&rig), [join_meeting_action()]);
    // the user cancels while the join's media is still being set up
    rig.cx
        .update(|cx| calls_of(&rig).update(cx, teams_ui::Calls::cancel_prejoin));
    // another meeting's Join meanwhile is refused as busy, and takes nothing back from the cancel
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    rig.cx.run_until_parked();
    assert_eq!(actions(&rig), [join_meeting_action()]);
    status(&rig, 1, CallStatus::Joining);
    let _stage = stage(&rig, &windows);
    status(&rig, 1, CallStatus::Connected);
    rig.cx.run_until_parked();
    let sent = actions(&rig);
    assert!(sent.contains(&CallAction::HangUp { call: call(1) }), "{sent:?}");
    assert!(
        !sent.iter().any(|action| matches!(action, CallAction::Camera { .. })),
        "{sent:?}"
    );
}

#[test]
fn dont_use_audio_joins_the_meeting_muted_and_phone_audio_is_greyed_out() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let window = prejoin_window(&rig, &windows);
    for shown in [
        "prejoin-audio-computer",
        "prejoin-audio-phone",
        "prejoin-audio-none",
        "prejoin-microphone",
    ] {
        assert!(drawn(window, shown), "{shown}");
    }
    // Phone audio takes no click
    click(window, "prejoin-audio-phone");
    assert!(rig.cx.read(|cx| {
        calls_of(&rig)
            .read(cx)
            .prejoin()
            .is_some_and(|prejoin| prejoin.audio == teams_ui::calls::PrejoinAudio::Computer)
    }));
    click(window, "prejoin-audio-none");
    assert!(rig.cx.read(|cx| {
        calls_of(&rig)
            .read(cx)
            .prejoin()
            .is_some_and(|prejoin| prejoin.audio == teams_ui::calls::PrejoinAudio::None)
    }));
    assert!(
        !drawn(window, "prejoin-microphone"),
        "no devices without the computer's audio"
    );
    assert!(actions(&rig).is_empty(), "nothing goes out while it shows");
    click(window, "prejoin-join");
    assert_eq!(actions(&rig), [join_meeting_action()]);
    status(&rig, 1, CallStatus::Joining);
    let _stage = stage(&rig, &windows);
    rig.cx.run_until_parked();
    assert!(
        actions(&rig).contains(&CallAction::Mute {
            call: call(1),
            muted: true
        }),
        "joined muted: {:?}",
        actions(&rig)
    );
}

#[test]
fn a_meet_nows_subject_is_edited_in_its_pre_join_and_titles_the_new_meeting() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, _cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let channel = "19:MadeUpChannel0001@thread.tacv2";
    let to_join = teams_ui::calls::MeetingToJoin::Chat {
        chat: channel.to_owned(),
        meeting: meeting_to_join().meeting().unwrap().clone(),
        title: "Meeting in \"General\"".to_owned(),
    };
    rig.cx
        .update(|cx| calls_of(&rig).update(cx, |calls, cx| calls.open_meet_now(to_join, cx)));
    let window = prejoin_window(&rig, &windows);
    assert!(drawn(window, "prejoin-title"), "Teams' heading");
    assert!(drawn(window, "prejoin-subject"), "the subject's box");
    click(window, "prejoin-subject");
    window.simulate_input(" and more");
    window.run_until_parked();
    assert!(actions(&rig).is_empty(), "an edited subject sends nothing");
    click(window, "prejoin-join");
    assert_eq!(
        actions(&rig),
        [CallAction::MeetNow {
            channel: channel.to_owned()
        }]
    );
    let titles = rig.core.calendar.lock().unwrap().titles.clone();
    assert_eq!(
        titles,
        [(channel.to_owned(), "Meeting in \"General\" and more".to_owned())]
    );
    no_name_in(&written);
}

#[test]
fn a_meeting_chats_header_joins_the_meeting_by_its_chat() {
    let rig = calling();
    let (trace, _) = trace();
    rig.core
        .meetings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            MEETING_CHAT.to_owned(),
            teams_core::MeetingDetails {
                meeting: meeting_to_join().meeting().unwrap().clone(),
                title: Some("Synthetic meeting".to_owned()),
                join_url: None,
            },
        );
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.join_meeting_chat(MEETING_CHAT.to_owned(), None, None, cx);
        });
    });
    let window = prejoin_window(&rig, &windows);
    assert!(drawn(window, "prejoin-title"));
    let prejoin = rig
        .cx
        .read(|cx| calls_of(&rig).read(cx).prejoin().cloned())
        .expect("the pre-join opened");
    assert_eq!(prejoin.to_join, meeting_to_join());
    assert!(actions(&rig).is_empty(), "nothing goes out while it shows");
}

/// a in the made-up meeting's call: its pre-join's Join now, the call up.
fn in_meeting(rig: &Rig, trace: &Arc<ViewTrace>) -> (Entity<CallWindows>, &'static mut VisualTestContext) {
    let (shell, cx) = shell_window(rig, Some(AB), Arc::clone(trace));
    let (windows, _) = call_windows(rig, &shell, trace);
    registered(rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let window = prejoin_window(rig, &windows);
    click(window, "prejoin-join");
    status(rig, 1, CallStatus::Joining);
    status(rig, 1, CallStatus::Connected);
    let stage = stage(rig, &windows);
    (windows, stage)
}

#[test]
fn whoever_waits_in_the_meetings_lobby_is_admitted_or_denied_from_the_stage() {
    let rig = calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_meeting(&rig, &trace);
    assert!(!drawn(stage, "call-lobby"), "nobody waits");
    rig.send(Event::Lobby {
        call: call(1),
        waiting: vec![teams_core::LobbyPerson {
            mri: Mri::new(LAB_B),
            name: Some("Lab Bee".to_owned()),
        }],
    });
    rig.cx.run_until_parked();
    draw(stage);
    assert!(drawn(stage, "call-lobby"));
    assert!(drawn(stage, "call-lobby-person-0"));
    click(stage, "call-lobby-admit-0");
    click(stage, "call-lobby-deny-0");
    let asked: Vec<CallAction> = actions(&rig)
        .into_iter()
        .filter(|action| matches!(action, CallAction::Lobby { .. }))
        .collect();
    assert_eq!(
        asked,
        [
            CallAction::Lobby {
                call: call(1),
                waiting: Mri::new(LAB_B),
                admit: true
            },
            CallAction::Lobby {
                call: call(1),
                waiting: Mri::new(LAB_B),
                admit: false
            }
        ]
    );
    // let in or turned away: nobody waits, and the card goes
    rig.send(Event::Lobby {
        call: call(1),
        waiting: Vec::new(),
    });
    rig.cx.run_until_parked();
    draw(stage);
    assert!(!drawn(stage, "call-lobby"));
}

#[test]
fn waiting_in_the_lobby_the_account_is_told_someone_should_let_it_in() {
    let rig = calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_meeting(&rig, &trace);
    status(&rig, 1, CallStatus::Lobby);
    rig.cx.run_until_parked();
    draw(stage);
    assert!(drawn(stage, "call-calling"));
    let text = rig
        .cx
        .read(|cx| calls_of(&rig).read(cx).call().map(teams_ui::calls::status_text));
    assert_eq!(text.as_deref(), Some(teams_ui::calls::LOBBY_WAIT));
}

/// The made-up meeting's ID and passcode, as its short join link carries them.
const MEETING_LINK: &str = "https://teams.microsoft.com/meet/123456789012?p=MadeUp";

/// [`MEETING_CHAT`] in a's chat list, its thread naming the meeting with `link`.
fn meeting_chat_with(rig: &Rig, link: Option<&str>) {
    rig.core.conversations.lock().unwrap().push(chat(
        MEETING_CHAT,
        ConversationKind::Meeting,
        Some("Synthetic meeting"),
    ));
    rig.core
        .meetings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            MEETING_CHAT.to_owned(),
            teams_core::MeetingDetails {
                meeting: meeting_to_join().meeting().unwrap().clone(),
                title: Some("Synthetic meeting".to_owned()),
                join_url: link.map(str::to_owned),
            },
        );
}

#[test]
fn join_with_an_id_uses_the_invitation_when_the_thread_has_a_long_link() {
    let rig = calling();
    let (trace, written) = trace();
    // Synthetic: an inactive meeting's thread has no numeric ID or passcode, only a long join link.
    meeting_chat_with(
        &rig,
        Some("https://teams.microsoft.com/l/meetup-join/19%3ameeting_Synthetic%40thread.v2/0"),
    );
    rig.core.listing.lock().unwrap().empty.push(MEETING_CHAT.to_owned());
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let code = teams_core::MeetingCode::typed("1234 5678 9012", "KeepPrivate29").expect("a code");
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.join_meeting_code(code.clone(), cx)));
    let window = prejoin_window(&rig, &windows);
    assert!(drawn(window, "prejoin-title"));
    let prejoin = rig.cx.read(|cx| calls_of(&rig).read(cx).prejoin().cloned()).unwrap();
    assert_eq!(prejoin.to_join, teams_ui::calls::MeetingToJoin::Code(code.clone()));
    assert!(actions(&rig).is_empty(), "opening pre-join never enters a meeting");
    click(window, "prejoin-join");
    assert_eq!(actions(&rig), vec![CallAction::JoinMeetingCode(code.clone())]);
    status(&rig, 1, CallStatus::Joining);
    status(&rig, 1, CallStatus::Connected);
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    rig.cx.read(|cx| {
        let calls = calls_of(&rig).read(cx);
        assert_eq!(
            calls.rejoin_of(calls.ended().unwrap()),
            Some(Ask::JoinMeetingCode(code.clone()))
        );
    });
    let trace_text = String::from_utf8(written.0.lock().unwrap().clone()).unwrap();
    assert!(!trace_text.contains(&code.id) && !trace_text.contains(&code.passcode));
}

#[test]
fn join_with_an_id_keeps_the_resolved_meeting_for_controls_and_rejoin() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let code = teams_core::MeetingCode::typed("123456789012", "MadeUp").unwrap();
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.join_meeting_code(code, cx)));
    click(prejoin_window(&rig, &windows), "prejoin-join");
    status(&rig, 1, CallStatus::Joining);
    let known = meeting_to_join();
    let info = known.meeting().unwrap();
    let reference = format!(
        "https://teams.microsoft.com/l/meetup-join/{MEETING_CHAT}/0?context={{\"Tid\":\"{}\",\"Oid\":\"{}\"}}",
        info.tenant_id, info.organizer_id
    );
    rig.send(Event::CallMeeting {
        call: call(1),
        state: teams_core::MeetingState {
            join: Some(teams_proto::MeetingJoinInfo {
                reference_link: Some(reference),
                ..Default::default()
            }),
            ..Default::default()
        },
    });
    rig.send(Event::CallGroupChat {
        call: call(1),
        chat: MEETING_CHAT.to_owned(),
    });
    rig.cx.read(|cx| {
        let calls = calls_of(&rig).read(cx);
        let resolved = calls.meeting_to_join().unwrap();
        assert_eq!(resolved.chat(), Some(MEETING_CHAT));
        assert_eq!(resolved.meeting(), known.meeting());
    });
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    rig.cx.read(|cx| {
        let calls = calls_of(&rig).read(cx);
        assert_eq!(
            calls.rejoin_of(calls.ended().unwrap()),
            Some(Ask::JoinMeeting {
                chat: MEETING_CHAT.to_owned(),
                meeting: info.clone()
            })
        );
    });
}

#[test]
fn join_with_an_id_without_a_local_chat_can_be_cancelled_without_a_write() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    let code = teams_core::MeetingCode::typed("123456789012", "MadeUp").expect("a code");
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.join_meeting_code(code, cx)));
    let window = prejoin_window(&rig, &windows);
    click(window, "prejoin-cancel");
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).prejoin().is_none()));
    assert!(actions(&rig).is_empty());
}

#[test]
fn a_chat_naming_no_meeting_is_refused_rather_than_joined() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| {
        shell.update(cx, |shell, cx| {
            shell.join_meeting_chat(MEETING_CHAT.to_owned(), None, None, cx);
        });
    });
    rig.cx.run_until_parked();
    assert!(rig.cx.read(|cx| windows.read(cx).prejoin_window().is_none()));
    let refused = rig
        .cx
        .read(|cx| calls_of(&rig).read(cx).refused().cloned())
        .expect("refused");
    assert_eq!(refused.why, "no-meeting");
    assert!(actions(&rig).is_empty());
}

#[test]
fn a_meetings_pre_join_is_cancelled_with_nothing_sent() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let window = prejoin_window(&rig, &windows);
    click(window, "prejoin-cancel");
    rig.cx.run_until_parked();
    assert!(rig.cx.read(|cx| windows.read(cx).prejoin_window().is_none()));
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).prejoin().is_none()));
    assert!(actions(&rig).is_empty());
}

#[test]
fn a_meetings_pre_join_keeps_its_choices_for_the_call_and_they_are_the_users_only() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let window = prejoin_window(&rig, &windows);
    rig.cx.update(|cx| {
        calls_of(&rig).update(cx, |calls, cx| calls.set_prejoin_muted(true, cx));
    });
    click(window, "prejoin-join");
    status(&rig, 1, CallStatus::Joining);
    let _stage = stage(&rig, &windows);
    rig.cx.run_until_parked();
    assert!(
        actions(&rig).contains(&CallAction::Mute {
            call: call(1),
            muted: true
        }),
        "the microphone muted as the switch said: {:?}",
        actions(&rig)
    );
    status(&rig, 1, CallStatus::Connected);
    rig.cx.run_until_parked();
    assert!(
        !actions(&rig)
            .iter()
            .any(|action| matches!(action, CallAction::Camera { .. })),
        "the camera stays off: its switch was never turned on"
    );
    let asked = actions(&rig).len();
    status(&rig, 1, CallStatus::Reconnecting);
    status(&rig, 1, CallStatus::Connected);
    rig.cx.run_until_parked();
    assert_eq!(actions(&rig).len(), asked, "each choice is asked once");
}

#[test]
fn a_meeting_is_not_offered_while_a_call_is_up_and_its_end_offers_rejoin() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let window = prejoin_window(&rig, &windows);
    click(window, "prejoin-join");
    status(&rig, 1, CallStatus::Connected);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    rig.cx.run_until_parked();
    assert!(
        rig.cx.read(|cx| calls_of(&rig).read(cx).prejoin().is_none()),
        "a call is up: it is refused, not shown"
    );
    let window = stage(&rig, &windows);
    status(&rig, 1, CallStatus::Ended(End::Failed("media")));
    assert!(drawn(window, "call-end-rejoin"), "a meeting can be gone into again");
    click(window, "call-end-rejoin");
    assert_eq!(actions(&rig).last(), Some(&join_meeting_action()));
}

#[test]
fn caption_shortcut_pane_hide_and_call_end_keep_speech_ephemeral() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    window.simulate_keystrokes("alt-shift-c");
    window.run_until_parked();
    assert!(
        rig.core
            .call_actions
            .lock()
            .unwrap()
            .iter()
            .any(|action| matches!(action, CallAction::Captions { call: id, enabled:true } if *id == call(1)))
    );
    let captions = teams_core::CallCaptions {
        phase: teams_core::CaptionPhase::On,
        spoken_language: "en-us".into(),
        error: None,
        lines: vec![teams_core::Caption {
            id: "synthetic/1".into(),
            speaker: Some(LAB_A.into()),
            display_name: Some("Lab Speaker".into()),
            text: "Private synthetic caption words.".into(),
            final_result: true,
            spoken_language: "en-us".into(),
            track_language: "en-us".into(),
            audio_timestamp: 123,
            duration: 10,
        }],
    };
    rig.send(Event::CallCaptions {
        call: call(1),
        captions,
    });
    assert!(drawn(window, "call-captions"));
    assert!(drawn(window, "call-caption-text"));
    // A stale other call cannot replace these lines.
    rig.send(Event::CallCaptions {
        call: call(99),
        captions: teams_core::CallCaptions::default(),
    });
    assert!(drawn(window, "call-captions"));
    window.simulate_keystrokes("alt-shift-c");
    window.run_until_parked();
    assert!(
        rig.core
            .call_actions
            .lock()
            .unwrap()
            .iter()
            .any(|action| matches!(action, CallAction::Captions { call: id, enabled:false } if *id == call(1)))
    );
    rig.send(Event::CallCaptions {
        call: call(1),
        captions: teams_core::CallCaptions::default(),
    });
    assert!(!drawn(window, "call-captions"));
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    assert!(rig.cx.read(|cx| rig.models.calls.read(cx).captions().lines.is_empty()));
    assert!(
        !String::from_utf8(written.0.lock().unwrap().clone())
            .unwrap()
            .contains("Private synthetic caption words")
    );
}

/// A fake Claude Code answering each caption with its text after `AR `.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
while IFS= read -r line; do
  text=$(printf '%s' "$line" | sed -n 's/.*<caption[^>]*>\(.*\)<\/caption>.*/\1/p')
  printf '{"type":"result","is_error":false,"result":"AR %s"}\n' "$text"
done
"#;

/// The caption settings' "Translate to" open.
fn translate_menu(window: &mut VisualTestContext) {
    click(window, "call-caption-settings");
    window.simulate_keystrokes("down down left");
    draw(window);
    if !drawn(window, "call-caption-translate-off") {
        window.simulate_keystrokes("right");
    }
    assert!(drawn(window, "call-caption-translate-off"));
}

#[test]
fn captions_translate_on_this_computer_and_say_why_when_they_cannot() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    rig.cx.update(|cx| cx.set_global(Translators::default()));
    caption_state(&rig, teams_core::CaptionPhase::On, None);
    rig.send(Event::CallCaptions {
        call: call(1),
        captions: teams_core::CallCaptions {
            phase: teams_core::CaptionPhase::On,
            spoken_language: "en-us".into(),
            error: None,
            lines: vec![teams_core::Caption {
                id: "synthetic/1".into(),
                speaker: Some(LAB_B.into()),
                display_name: Some("Lab Speaker".into()),
                text: "Private synthetic caption words.".into(),
                final_result: true,
                spoken_language: "en-us".into(),
                track_language: "en-us".into(),
                audio_timestamp: 123,
                duration: 10,
            }],
        },
    });
    assert!(drawn(window, "call-caption-text"));
    assert!(
        !drawn(window, "call-caption-translated"),
        "off until a language is picked"
    );
    // Arabic picked, no tool installed: the captions as spoken, and why
    translate_menu(window);
    click(window, "call-caption-translate-ar");
    let settings = rig.cx.read(|cx| shell.read(cx).settings().clone());
    assert_eq!(
        rig.cx.read(|cx| settings.read(cx).settings().caption_translate_to),
        Some("ar")
    );
    assert!(drawn(window, "call-caption-translate-failed"));
    assert!(drawn(window, "call-caption-text"));
    // the tool installed meanwhile: Try again translates
    let dir = tempfile::tempdir().unwrap();
    let program = fake_tool::write(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Translators::with(vec![(Backend::Claude, program)])));
    click(window, "call-caption-translate-retry");
    for _ in 0..500 {
        if !drawn(window, "call-caption-translate-failed") && drawn(window, "call-caption-translated") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(drawn(window, "call-caption-translated"));
    assert!(!drawn(window, "call-caption-translate-failed"));
    // Off: no longer translated
    translate_menu(window);
    click(window, "call-caption-translate-off");
    assert!(!drawn(window, "call-caption-translated"));
    assert_eq!(
        rig.cx.read(|cx| settings.read(cx).settings().caption_translate_to),
        None
    );
    // translating is local: the meeting is never asked anything, and no caption reaches the trace
    assert!(
        !actions(&rig)
            .iter()
            .any(|action| matches!(action, CallAction::CaptionLanguage { .. }))
    );
    assert!(
        !String::from_utf8(written.0.lock().unwrap().clone())
            .unwrap()
            .contains("Private synthetic caption words")
    );
}

fn caption_stage() -> (Rig, Entity<CallWindows>, &'static mut VisualTestContext) {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    (rig, windows, window)
}
fn caption_state(rig: &Rig, phase: teams_core::CaptionPhase, error: Option<&str>) {
    rig.send(Event::CallCaptions {
        call: call(1),
        captions: teams_core::CallCaptions {
            phase,
            spoken_language: "en-us".into(),
            error: error.map(str::to_owned),
            lines: Vec::new(),
        },
    });
}
fn caption_language_menu(window: &mut VisualTestContext) {
    click(window, "call-caption-settings");
    window.simulate_keystrokes("down left");
    draw(window);
    if !drawn(window, "call-caption-language-sq-al") {
        window.simulate_keystrokes("right");
    }
    assert!(drawn(window, "call-caption-language-sq-al"));
}

// These kit Buttons have no debug-selector wrapper. Target their visible controls within the pane's stable bounds.
fn click_caption_retry(window: &mut VisualTestContext) {
    draw(window);
    let pane = window.debug_bounds("call-captions").unwrap();
    window.simulate_click(
        gpui::point(pane.center().x, pane.center().y + gpui::px(36.0)),
        Modifiers::none(),
    );
    window.run_until_parked();
}
fn click_caption_confirmation(window: &mut VisualTestContext, update: bool) {
    draw(window);
    let panel = window.debug_bounds("call-caption-language-confirm").unwrap();
    window.simulate_click(
        gpui::point(
            panel.origin.x + panel.size.width - gpui::px(if update { 52.0 } else { 128.0 }),
            panel.origin.y + panel.size.height - gpui::px(32.0),
        ),
        Modifiers::none(),
    );
    window.run_until_parked();
}

#[test]
fn caption_service_states_offer_explicit_retry_and_hide_without_touching_the_voice_call() {
    let (rig, _windows, window) = caption_stage();
    caption_state(&rig, teams_core::CaptionPhase::Starting, None);
    assert!(drawn(window, "call-captions"));
    assert!(!drawn(window, "call-caption-text"));
    caption_state(
        &rig,
        teams_core::CaptionPhase::Failed,
        Some("Synthetic caption service is busy. Try again."),
    );
    click_caption_retry(window);
    assert!(matches!(
        actions(&rig).last(),
        Some(CallAction::Captions { enabled: true, .. })
    ));
    caption_state(
        &rig,
        teams_core::CaptionPhase::On,
        Some("Synthetic spoken language change failed."),
    );
    assert!(drawn(window, "call-captions"));
    assert!(!drawn(window, "call-caption-text"));
    click(window, "call-caption-hide");
    assert!(matches!(
        actions(&rig).last(),
        Some(CallAction::Captions { enabled: false, .. })
    ));
    assert!(
        !actions(&rig)
            .iter()
            .any(|action| matches!(action, CallAction::HangUp { .. }))
    );
    caption_state(&rig, teams_core::CaptionPhase::Off, None);
    assert!(!drawn(window, "call-captions"));
}

#[test]
fn caption_language_selection_requires_confirmation_and_cancel_has_no_shared_effect() {
    let (rig, _windows, window) = caption_stage();
    caption_state(&rig, teams_core::CaptionPhase::On, None);
    caption_language_menu(window);
    click(window, "call-caption-language-sq-al");
    assert!(drawn(window, "call-caption-language-confirm"));
    let before = actions(&rig).len();
    click_caption_confirmation(window, false);
    assert_eq!(actions(&rig).len(), before);
    assert!(!drawn(window, "call-caption-language-confirm"));
    caption_language_menu(window);
    click(window, "call-caption-language-sq-al");
    click_caption_confirmation(window, true);
    assert!(matches!(actions(&rig).last(), Some(CallAction::CaptionLanguage { language, .. }) if language == "sq-al"));
    assert!(!drawn(window, "call-caption-language-confirm"));
}

#[test]
fn caption_font_and_position_choices_are_local_and_preserve_the_ephemeral_lines() {
    let (rig, _windows, window) = caption_stage();
    rig.send(Event::CallCaptions {
        call: call(1),
        captions: teams_core::CallCaptions {
            phase: teams_core::CaptionPhase::On,
            spoken_language: "unknown".into(),
            error: None,
            lines: vec![teams_core::Caption {
                id: "synthetic/local".into(),
                speaker: None,
                display_name: None,
                text: "Synthetic local caption".into(),
                final_result: false,
                spoken_language: "en-us".into(),
                track_language: "en-us".into(),
                audio_timestamp: 7,
                duration: 1,
            }],
        },
    });
    assert!(drawn(window, "call-caption-text"));
    let before_actions = actions(&rig).len();
    let before = window.debug_bounds("call-captions").unwrap();
    click(window, "call-caption-settings");
    window.simulate_keystrokes("down down down left down down enter");
    draw(window);
    assert!(drawn(window, "call-caption-text"));
    click(window, "call-caption-settings");
    window.simulate_keystrokes("down down down down left down enter");
    draw(window);
    let after = window.debug_bounds("call-captions").unwrap();
    assert!(
        after.origin.y < before.origin.y,
        "At top moves the caption pane above the stage"
    );
    assert_eq!(
        actions(&rig).len(),
        before_actions,
        "local styles never change the meeting"
    );
    assert_eq!(rig.cx.read(|cx| rig.models.calls.read(cx).captions().lines.len()), 1);
}

#[test]
fn language_and_speech_menu_exposes_caption_toggle_through_the_actual_more_popup() {
    let (rig, _windows, window) = caption_stage();
    open_language_and_speech(window);
    click(window, "call-toggle-captions");
    assert!(matches!(
        actions(&rig).last(),
        Some(CallAction::Captions { enabled: true, .. })
    ));
    caption_state(&rig, teams_core::CaptionPhase::On, None);
    open_language_and_speech(window);
    click(window, "call-toggle-captions");
    assert!(matches!(
        actions(&rig).last(),
        Some(CallAction::Captions { enabled: false, .. })
    ));
}

/// More's Language and speech opened by the keyboard, as Teams lists it after the video effects and audio settings.
fn open_language_and_speech(window: &mut VisualTestContext) {
    for downs in 1..=4 {
        click(window, "call-more");
        let keys = format!("{} right", vec!["down"; downs].join(" "));
        window.simulate_keystrokes(&keys);
        if drawn(window, "call-toggle-captions") {
            return;
        }
        window.simulate_keystrokes("escape escape");
        draw(window);
    }
    panic!("Language and speech isn't in More");
}

/// Where the element `selector` starts from the window's left.
fn left_of(window: &mut VisualTestContext, selector: &'static str) -> gpui::Pixels {
    draw(window);
    window.debug_bounds(selector).expect(selector).origin.x
}

/// Opens View's More options (a submenu, reached by the keyboard as a user's arrows reach it).
fn open_view_more_options(window: &mut VisualTestContext) {
    for downs in 1..=7 {
        click(window, "call-view");
        let keys = format!("{} right", vec!["down"; downs].join(" "));
        window.simulate_keystrokes(&keys);
        if drawn(window, "call-view-hide-me") {
            return;
        }
        window.simulate_keystrokes("escape escape");
        draw(window);
    }
    panic!("More options isn't in View");
}

/// a's group call in the trio chat, up, b muted with its hand raised and c speaking: its window.
fn in_group_call(rig: &Rig, trace: &Arc<ViewTrace>) -> (Entity<CallWindows>, &'static mut VisualTestContext) {
    let (shell, cx) = shell_window(rig, Some(TRIO), Arc::clone(trace));
    let (windows, _) = call_windows(rig, &shell, trace);
    registered(rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(rig, 1, CallStatus::Connected);
    let mut told = view(
        &[
            (LAB_B, "Lab Bee", false, None, false),
            (LAB_C, "Lab Sea", false, None, true),
        ],
        false,
    );
    told.participants[0].muted = true;
    told.participants[0].hand_raised = true;
    rig.send(Event::CallParticipants {
        call: call(1),
        view: told,
    });
    let stage = stage(rig, &windows);
    (windows, stage)
}

/// The call actions asked so far that `wanted` takes.
fn asked(rig: &Rig, wanted: impl Fn(&CallAction) -> bool) -> Vec<CallAction> {
    actions(rig).into_iter().filter(|action| wanted(action)).collect()
}

#[test]
fn a_group_call_offers_teams_controls_raise_and_react_and_shows_who_is_muted_or_has_a_hand_up() {
    let rig = calling();
    let (trace, written) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    for control in [
        "call-chat",
        "call-roster",
        // b's hand up asks for attention: People's red badge
        "call-roster-attention",
        "call-raise-hand",
        "call-react",
        "call-view",
        "call-more",
        "call-mute",
        "call-hang-up",
    ] {
        assert!(drawn(stage, control), "{control}");
    }
    assert!(
        !drawn(stage, "call-leave-options"),
        "only a meeting's organizer ends it for everyone"
    );
    click(stage, "call-raise-hand");
    stage.simulate_keystrokes("ctrl-shift-k");
    click(stage, "call-react");
    click(stage, "call-react-heart");
    let hands = asked(&rig, |action| {
        matches!(action, CallAction::RaiseHand { .. } | CallAction::React { .. })
    });
    assert_eq!(
        hands,
        [
            CallAction::RaiseHand {
                call: call(1),
                raised: true
            },
            // shown raised at once: its key lowers it again
            CallAction::RaiseHand {
                call: call(1),
                raised: false
            },
            CallAction::React {
                call: call(1),
                reaction: "heart".to_owned()
            }
        ]
    );
    // the account's hand up, as the roster tells it: Raise lowers it
    rig.send(Event::CallOwn {
        call: call(1),
        hand_raised: true,
        organizer: false,
    });
    rig.cx.run_until_parked();
    click(stage, "call-raise-hand");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::RaiseHand {
            call: call(1),
            raised: false
        })
    );
    // the People pane: everyone with their signs, the account first
    click(stage, "call-roster");
    for row in ["call-roster-pane", "call-roster-self", "call-roster-0", "call-roster-1"] {
        assert!(drawn(stage, row), "{row}");
    }
    // b's hand, the first up, is numbered as Teams numbers them
    assert!(drawn(stage, "call-roster-hand-1"));
    assert!(!drawn(stage, "call-roster-hand-2"), "the account's hand is down again");
    // a live reaction floats up from the stage
    rig.send(Event::CallReaction {
        call: call(1),
        from: Mri::new(LAB_C),
        name: Some("Lab Sea".to_owned()),
        reaction: "applause".to_owned(),
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-reactions"));
    no_name_in(&written);
}

#[test]
fn view_lays_the_stage_out_as_gallery_or_speaker_pins_a_tile_and_hides_the_self_view() {
    let rig = calling();
    let (trace, written) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    assert!(drawn(stage, "call-grid"));
    assert!(!drawn(stage, "call-speaker-main"), "the gallery first");
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    assert!(drawn(stage, "call-speaker-main"));
    assert!(drawn(stage, "call-speaker-strip"));
    click(stage, "call-view");
    click(stage, "call-view-gallery");
    assert!(!drawn(stage, "call-speaker-main"));
    // a tile's More options, shown while the pointer is over it, pins it for the account alone
    let tile = stage.debug_bounds("call-participant-1").expect("c's tile").center();
    stage.simulate_mouse_move(tile, None, Modifiers::none());
    click(stage, "call-participant-1-more");
    click(stage, "call-participant-1-pin");
    assert!(drawn(stage, "call-speaker-main"), "the pinned tile large");
    let tile = stage.debug_bounds("call-participant-1").expect("c's tile").center();
    stage.simulate_mouse_move(tile, None, Modifiers::none());
    click(stage, "call-participant-1-more");
    click(stage, "call-participant-1-pin");
    assert!(!drawn(stage, "call-speaker-main"), "unpinned");
    open_view_more_options(stage);
    click(stage, "call-view-hide-me");
    assert!(!drawn(stage, "call-participant-self"));
    open_view_more_options(stage);
    click(stage, "call-view-hide-me");
    assert!(drawn(stage, "call-participant-self"));
    let traced: Vec<Vec<String>> = lines(&written, "call-window", "layout").iter().map(kinds).collect();
    assert_eq!(traced, [["speaker"], ["gallery"]]);
    assert_eq!(lines(&written, "call-window", "pinned").len(), 2);
}

#[test]
fn view_turns_the_others_videos_off_and_shows_videos_first() {
    let rig = calling();
    let (trace, written) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    // Turn off other videos, and on again
    open_view_more_options(stage);
    click(stage, "call-view-other-videos");
    open_view_more_options(stage);
    assert!(drawn(stage, "call-view-other-videos"));
    click(stage, "call-view-other-videos");
    let others: Vec<Vec<String>> = lines(&written, "call-window", "other-videos")
        .iter()
        .map(kinds)
        .collect();
    assert_eq!(others, [["off"], ["on"]]);
    // Show videos first leads with the cameras on: c's tile (the second in the roster) first
    let told = view(
        &[
            (LAB_B, "Lab Bee", false, None, false),
            (LAB_C, "Lab Sea", true, Some(1), false),
        ],
        false,
    );
    rig.send(Event::CallParticipants {
        call: call(1),
        view: told,
    });
    rig.cx.run_until_parked();
    assert!(left_of(stage, "call-participant-0") < left_of(stage, "call-participant-1"));
    click(stage, "call-view");
    click(stage, "call-view-videos-first");
    assert!(left_of(stage, "call-participant-1") < left_of(stage, "call-participant-0"));
}

#[test]
fn a_one_to_one_call_offers_no_raise_or_react_and_its_chat_opens_beside_the_stage() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    assert!(drawn(stage, "call-chat"));
    assert!(!drawn(stage, "call-raise-hand"), "a 1:1 call has no Raise");
    assert!(!drawn(stage, "call-react"), "nor React");
    stage.simulate_keystrokes("ctrl-shift-k");
    assert!(
        asked(&rig, |action| matches!(action, CallAction::RaiseHand { .. })).is_empty(),
        "its key asks nothing either"
    );
    click(stage, "call-chat");
    assert!(drawn(stage, "call-chat-pane"));
    // one pane at a time: People takes its place, and Chat brings it back
    click(stage, "call-roster");
    assert!(drawn(stage, "call-roster-pane"));
    assert!(!drawn(stage, "call-chat-pane"));
    click(stage, "call-chat");
    assert!(drawn(stage, "call-chat-pane"));
    click(stage, "call-chat-close");
    assert!(!drawn(stage, "call-chat-pane"));
}

#[test]
fn a_one_to_one_call_goes_on_hold_and_resumes_as_teams_shows_it() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    click(stage, "call-hold");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Hold {
            call: call(1),
            held: true
        })
    );
    assert!(!drawn(stage, "call-resume"), "held once the hold was taken");
    rig.send(Event::CallHeld {
        call: call(1),
        held: true,
    });
    rig.cx.run_until_parked();
    // Resume in Hold's place, the camera and the microphone greyed out, and "On hold" on the stage
    assert!(drawn(stage, "call-resume"));
    assert!(!drawn(stage, "call-hold"));
    assert!(drawn(stage, "call-held-controls"));
    assert!(drawn(stage, "call-calling"));
    click(stage, "call-resume");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Hold {
            call: call(1),
            held: false
        })
    );
    rig.send(Event::CallHeld {
        call: call(1),
        held: false,
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-hold"));
    assert!(!drawn(stage, "call-held-controls"));
}

/// A 1:1 call to b, up, in a build that shows video on `feed`: the rig and the call window.
fn one_to_one_video_call(feed: &VideoFeed) -> (Rig, &'static mut VisualTestContext) {
    let rig = video_calling(feed);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    (rig, stage)
}

#[test]
fn a_one_to_one_call_shares_its_screen_from_share_content_and_stops_as_teams_shows_it() {
    let feed = VideoFeed::new();
    let (rig, stage) = one_to_one_video_call(&feed);
    // Share's flyout: "Share content", its screen, window or tab
    click(stage, "call-share");
    assert!(drawn(stage, "call-share-content"));
    click(stage, "call-share-screen");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Share {
            call: call(1),
            on: true
        })
    );
    assert!(!drawn(stage, "call-stop-sharing"), "sharing once the engine says so");
    rig.send(Event::CallSharing {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    // the red Stop sharing in Share's place
    assert!(drawn(stage, "call-stop-sharing"));
    assert!(!drawn(stage, "call-share"));
    click(stage, "call-stop-sharing");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Share {
            call: call(1),
            on: false
        })
    );
    rig.send(Event::CallSharing {
        call: call(1),
        on: false,
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-share"));
    // on hold, Share greys out with the camera and the microphone
    rig.send(Event::CallHeld {
        call: call(1),
        held: true,
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-held-controls") && drawn(stage, "call-share"));
}

/// The sharer's name sits on the shared picture's bottom left, where the 16:9 picture is drawn letterboxed in the
/// share's room, not on the room's.
fn share_named_on_its_picture(stage: &mut VisualTestContext) {
    for window in [(1280.0, 800.0), (800.0, 560.0)] {
        stage.simulate_resize(gpui::size(gpui::px(window.0), gpui::px(window.1)));
        draw(stage);
        draw(stage);
        let room = stage.debug_bounds("call-share").expect("the share");
        let picture = stage.debug_bounds("call-share-picture").expect("the picture's corner");
        let ratio = f32::from(picture.size.width) / f32::from(picture.size.height);
        assert!((ratio - 16.0 / 9.0).abs() < 0.02, "{window:?}: {picture:?}");
        let (dx, dy) = (
            picture.center().x - room.center().x,
            picture.center().y - room.center().y,
        );
        assert!(
            dx.abs() < gpui::px(1.0) && dy.abs() < gpui::px(1.0),
            "{window:?}: {picture:?} in {room:?}"
        );
        let fills_width = (picture.size.width - room.size.width).abs() < gpui::px(1.0);
        let fills_height = (picture.size.height - room.size.height).abs() < gpui::px(1.0);
        assert!(fills_width || fills_height, "{window:?}: {picture:?} in {room:?}");
    }
}

#[test]
fn the_far_end_s_share_takes_the_stage_with_the_people_beside_it_and_view_focuses_on_it() {
    let feed = VideoFeed::new();
    let (rig, stage) = one_to_one_video_call(&feed);
    assert!(drawn(stage, "call-participant-self"), "the self view floats");
    click(stage, "call-view");
    assert!(!drawn(stage, "call-view-focus"), "nothing shared: no Focus on content");
    stage.simulate_keystrokes("escape");
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-share-stage") && drawn(stage, "call-share"));
    assert!(drawn(stage, "call-share-strip"), "the people beside the share");
    assert!(drawn(stage, "call-person-0") && drawn(stage, "call-participant-self"));
    // the share's frames on its own tile
    let output: &dyn VideoOutput = &feed;
    output
        .remote(teams_media::session::SHARING_LINE)
        .show(&TestPattern::new(320, 180).frame(1))
        .unwrap();
    assert!(drawn(stage, "video-tile-share"));
    assert!(feed.picture(Tile::REMOTE).is_none(), "no camera's frame");
    share_named_on_its_picture(stage);
    // View's Focus on content: the share alone
    click(stage, "call-view");
    click(stage, "call-view-focus");
    assert!(drawn(stage, "call-share-focus"));
    share_named_on_its_picture(stage);
    assert!(!drawn(stage, "call-share-strip") && !drawn(stage, "call-participant-self"));
    // the share ends: the call as it was
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: false,
    });
    rig.cx.run_until_parked();
    assert!(!drawn(stage, "call-share-stage") && !drawn(stage, "call-share-focus"));
    assert!(drawn(stage, "call-person-0") && drawn(stage, "call-participant-self"));
}

#[test]
fn a_group_call_shares_its_screen_from_share_content_beside_the_people_as_teams_shows_it() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, _) = trace();
    let (_windows, group) = in_group_call(&rig, &trace);
    click(group, "call-share");
    assert!(drawn(group, "call-share-content"));
    click(group, "call-share-screen");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Share {
            call: call(1),
            on: true
        })
    );
    rig.send(Event::CallSharing {
        call: call(1),
        on: true,
    });
    rig.cx.run_until_parked();
    // the sharer sees the people as before, with the red Stop sharing in Share's place (its own screen isn't shown)
    assert!(drawn(group, "call-stop-sharing") && !drawn(group, "call-share"));
    assert!(drawn(group, "call-participant-0") && drawn(group, "call-participant-1"));
    assert!(!drawn(group, "call-share-stage"));
    click(group, "call-stop-sharing");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Share {
            call: call(1),
            on: false
        })
    );
    rig.send(Event::CallSharing {
        call: call(1),
        on: false,
    });
    rig.cx.run_until_parked();
    assert!(drawn(group, "call-share"));
}

#[test]
fn a_build_without_video_offers_no_share() {
    let rig = calling();
    let (trace, _) = trace();
    let (_windows, group) = in_group_call(&rig, &trace);
    assert!(!drawn(group, "call-share"), "no video: nothing to share with");
    let rig = calling();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    assert!(
        !drawn(stage(&rig, &windows), "call-share"),
        "no video: nothing to share with"
    );
}

#[test]
fn a_one_to_one_call_held_by_its_far_end_shows_on_hold_its_controls_as_they_were() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    rig.send(Event::CallHeldByFarEnd {
        call: call(1),
        held: true,
    });
    rig.cx.run_until_parked();
    // the far end's avatar over "On hold"; the account's own Hold and its devices as they were
    assert!(drawn(stage, "call-held-by-far-end"));
    assert!(drawn(stage, "call-hold"));
    assert!(!drawn(stage, "call-held-controls"));
    rig.send(Event::CallHeldByFarEnd {
        call: call(1),
        held: false,
    });
    rig.cx.run_until_parked();
    assert!(!drawn(stage, "call-held-by-far-end"));
}

#[test]
fn a_group_call_offers_no_hold() {
    let rig = calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    assert!(drawn(stage, "call-raise-hand"));
    assert!(!drawn(stage, "call-hold"), "Teams offers Hold in a 1:1 call only");
}

#[test]
fn a_meeting_s_organizer_ends_it_for_everyone_and_shares_its_link_from_the_people_pane() {
    let rig = calling();
    let (trace, _) = trace();
    meeting_chat_with(&rig, Some(MEETING_LINK));
    let (_windows, stage) = in_meeting(&rig, &trace);
    assert!(!drawn(stage, "call-leave-options"), "not told it organizes");
    rig.send(Event::CallOwn {
        call: call(1),
        hand_raised: false,
        organizer: true,
    });
    rig.cx.run_until_parked();
    // End meeting asks first: Cancel leaves the meeting running, End ends it for everyone
    click(stage, "call-leave-options");
    click(stage, "call-end-meeting");
    assert!(drawn(stage, "call-end-meeting-confirm"));
    click(stage, "call-end-meeting-cancel");
    assert!(!drawn(stage, "call-end-meeting-confirm"));
    assert!(asked(&rig, |action| matches!(action, CallAction::EndMeeting { .. })).is_empty());
    click(stage, "call-leave-options");
    click(stage, "call-end-meeting");
    click(stage, "call-end-meeting-end");
    assert_eq!(actions(&rig).last(), Some(&CallAction::EndMeeting { call: call(1) }));
    click(stage, "call-leave-options");
    click(stage, "call-leave");
    assert_eq!(actions(&rig).last(), Some(&CallAction::HangUp { call: call(1) }));
    // People: Share invite copies the meeting's link
    status(&rig, 1, CallStatus::Connected);
    click(stage, "call-roster");
    click(stage, "call-share-invite");
    click(stage, "call-copy-link");
    assert!(drawn(stage, "call-link-copied"));
    // the account organizes it: "Organizer" under its own row
    assert!(drawn(stage, "call-roster-organizer"));
    let copied = stage.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some(MEETING_LINK));
    // and whoever waits in the lobby is let in at once by Admit all
    rig.send(Event::Lobby {
        call: call(1),
        waiting: vec![
            teams_core::LobbyPerson {
                mri: Mri::new(LAB_B),
                name: Some("Lab Bee".to_owned()),
            },
            teams_core::LobbyPerson {
                mri: Mri::new(LAB_C),
                name: None,
            },
        ],
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-roster-lobby"));
    click(stage, "call-lobby-admit-all");
    click(stage, "call-roster-deny-1");
    let decided: Vec<(String, bool)> = asked(&rig, |action| matches!(action, CallAction::Lobby { .. }))
        .into_iter()
        .filter_map(|action| match action {
            CallAction::Lobby { waiting, admit, .. } => Some((waiting.as_str().to_owned(), admit)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decided,
        [
            (LAB_B.to_owned(), true),
            (LAB_C.to_owned(), true),
            (LAB_C.to_owned(), false)
        ]
    );
}

#[test]
fn a_reconnecting_call_says_so_over_its_stage() {
    let rig = calling();
    let (trace, _) = trace();
    let (_windows, stage) = in_group_call(&rig, &trace);
    assert!(
        !drawn(stage, "call-reconnecting"),
        "a connection that is fine says nothing"
    );
    status(&rig, 1, CallStatus::Reconnecting);
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-reconnecting"));
    status(&rig, 1, CallStatus::Connected);
    rig.cx.run_until_parked();
    assert!(!drawn(stage, "call-reconnecting"));
}

/// A group call in the trio chat, connected, its roster telling b and c: the call's window.
fn group_call_up(rig: &Rig) -> &'static mut VisualTestContext {
    let (trace, _) = trace();
    let (shell, cx) = shell_window(rig, Some(TRIO), Arc::clone(&trace));
    let (windows, _) = call_windows(rig, &shell, &trace);
    registered(rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(rig, 1, CallStatus::Connected);
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(
            &[
                (LAB_B, "Lab Bee", false, Some(1), false),
                (LAB_C, "Lab Sea", false, Some(2), false),
            ],
            false,
        ),
    });
    rig.cx.run_until_parked();
    let stage = stage(rig, &windows);
    // the windows live as long as the test
    std::mem::forget(windows);
    stage
}

/// The meeting's state `state`, told for call 1.
fn meeting(rig: &Rig, state: teams_core::MeetingState) {
    rig.send(Event::CallMeeting { call: call(1), state });
    rig.cx.run_until_parked();
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[test]
fn a_group_call_s_more_ends_with_help_which_opens_teams_help_in_the_browser() {
    let rig = calling();
    let stage = group_call_up(&rig);
    let before = actions(&rig).len();
    click(stage, "call-more");
    click(stage, "call-more-help");
    assert_eq!(
        stage.opened_url().as_deref(),
        Some("https://go.microsoft.com/fwlink/?linkid=2163160")
    );
    assert_eq!(actions(&rig).len(), before, "Help acts on no call");
}

#[test]
fn a_group_call_s_timer_shows_to_everyone_and_each_change_goes_to_the_call() {
    use teams_proto::{MeetingTimer, TimerState};
    let rig = calling();
    let stage = group_call_up(&rig);
    assert!(!drawn(stage, "call-timer"));
    click(stage, "call-more");
    click(stage, "call-more-timer");
    // More → Timer: five minutes, paused, until it is sent
    assert!(drawn(stage, "call-timer"));
    click(stage, "call-timer-toggle");
    let Some(CallAction::Timer { timer, .. }) = actions(&rig).last().cloned() else {
        panic!("{:?}", actions(&rig));
    };
    assert_eq!(
        (timer.state, timer.remaining_ms, timer.previous),
        (TimerState::InProgress, 300_000, None)
    );
    // the conversation tells the timer running: Pause pauses it where it is
    let running = MeetingTimer {
        state: TimerState::InProgress,
        remaining_ms: 300_000,
        original_ms: 300_000,
        edited_ms: Some(now_ms() - 61_000),
        previous: None,
    };
    meeting(
        &rig,
        teams_core::MeetingState {
            timer: Some(running),
            ..teams_core::MeetingState::default()
        },
    );
    click(stage, "call-timer-toggle");
    let Some(CallAction::Timer { timer, .. }) = actions(&rig).last().cloned() else {
        panic!("{:?}", actions(&rig));
    };
    assert_eq!(timer.state, TimerState::Paused);
    assert_eq!(timer.previous, Some(TimerState::InProgress));
    assert!(
        (238_000..=240_000).contains(&timer.remaining_ms),
        "{}",
        timer.remaining_ms
    );
    // its time opens the editor: +5 min and Start
    click(stage, "call-timer-time");
    assert!(drawn(stage, "call-timer-editor"));
    click(stage, "call-timer-add-5");
    click(stage, "call-timer-start");
    let Some(CallAction::Timer { timer, .. }) = actions(&rig).last().cloned() else {
        panic!("{:?}", actions(&rig));
    };
    assert_eq!(
        (timer.state, timer.remaining_ms, timer.original_ms),
        (TimerState::InProgress, 600_000, 600_000)
    );
    assert!(!drawn(stage, "call-timer-editor"));
    // cancelled for everyone: no bar
    meeting(&rig, teams_core::MeetingState::default());
    assert!(!drawn(stage, "call-timer"));
}

#[test]
fn a_recording_someone_runs_and_the_lock_s_change_show_their_notices() {
    let rig = calling();
    let stage = group_call_up(&rig);
    meeting(
        &rig,
        teams_core::MeetingState {
            recording: Some(teams_core::Recording {
                by: Some(Mri::new(LAB_B)),
                audio_only: false,
            }),
            ..teams_core::MeetingState::default()
        },
    );
    assert!(drawn(stage, "call-recording-notice"));
    assert!(drawn(stage, "call-recording-dot"));
    click(stage, "call-recording-notice-close");
    assert!(!drawn(stage, "call-recording-notice"));
    assert!(drawn(stage, "call-recording-dot"), "the dot stays while it records");
    meeting(
        &rig,
        teams_core::MeetingState {
            locked: true,
            ..teams_core::MeetingState::default()
        },
    );
    assert!(drawn(stage, "call-lock-notice"));
    assert!(!drawn(stage, "call-recording-dot"), "the recording stopped");
}

/// More's Record and transcribe opened by the keyboard (it leads More, as in Teams), its `item` drawn.
fn open_record_menu(stage: &mut VisualTestContext, item: &'static str) {
    for downs in 1..=3 {
        click(stage, "call-more");
        let keys = format!("{} right", vec!["down"; downs].join(" "));
        stage.simulate_keystrokes(&keys);
        if drawn(stage, item) {
            return;
        }
        stage.simulate_keystrokes("escape escape");
        draw(stage);
    }
    panic!("Record and transcribe doesn't offer {item}");
}

fn recording_by(by: &str) -> teams_core::MeetingState {
    teams_core::MeetingState {
        recording: Some(teams_core::Recording {
            by: Some(Mri::new(by)),
            audio_only: false,
        }),
        ..teams_core::MeetingState::default()
    }
}

/// The roster tells `by`'s transcription, with their recording of the audio alone (`audio`) or none.
fn transcribing_by(by: &str, audio: bool) -> teams_core::MeetingState {
    teams_core::MeetingState {
        recording: audio.then(|| teams_core::Recording {
            by: Some(Mri::new(by)),
            audio_only: true,
        }),
        transcribing: Some(teams_core::Transcription { by: Some(Mri::new(by)) }),
        ..teams_core::MeetingState::default()
    }
}

#[test]
fn the_account_transcribes_after_asking_the_language_and_its_transcript_opens() {
    let rig = calling();
    let stage = group_call_up(&rig);
    open_record_menu(stage, "call-transcribe-start");
    assert!(!drawn(stage, "call-transcript-toggle"), "no transcript before one ran");
    click(stage, "call-transcribe-start");
    assert!(
        drawn(stage, "call-transcribe-dialog"),
        "Start transcription asks the language first"
    );
    let before = actions(&rig).len();
    click(stage, "call-transcribe-cancel");
    assert_eq!(actions(&rig).len(), before, "Cancel sends nothing");
    open_record_menu(stage, "call-transcribe-start");
    click(stage, "call-transcribe-start");
    click(stage, "call-transcribe-confirm");
    let Some(CallAction::Transcribe { title, language, .. }) = actions(&rig).last().cloned() else {
        panic!("{:?}", actions(&rig));
    };
    assert_eq!(language, "en-us");
    assert!(title.starts_with("Call with "), "{title}");
    // the roster tells it: its notice, the header's mark, and the transcript open by itself for its starter
    meeting(&rig, transcribing_by(LAB_A, false));
    assert!(drawn(stage, "call-transcription-notice"));
    assert!(drawn(stage, "call-recording-privacy"));
    assert!(drawn(stage, "call-transcribing"));
    assert!(drawn(stage, "call-transcript-pane"));
    assert!(drawn(stage, "call-transcript-event"));
    // Switch to live captions shows them instead
    click(stage, "call-transcript-captions");
    assert!(!drawn(stage, "call-transcript-pane"));
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Captions {
            call: call(1),
            enabled: true
        })
    );
    open_record_menu(stage, "call-transcript-toggle");
    click(stage, "call-transcript-toggle");
    assert!(drawn(stage, "call-transcript-pane"), "Show transcript shows it again");
    click(stage, "call-transcript-tip-close");
    assert!(!drawn(stage, "call-transcript-tip"));
    record_audio_beside_the_transcription(&rig, stage);
}

/// While the account's transcription runs: Start recording asks what to record, the audio alone records beside it, and
/// Stop recording stops both after asking.
fn record_audio_beside_the_transcription(rig: &Rig, stage: &mut VisualTestContext) {
    open_record_menu(stage, "call-record-start");
    click(stage, "call-record-start");
    assert!(drawn(stage, "call-record-what"), "it asks what to record");
    assert!(
        !drawn(stage, "call-record-language"),
        "the transcription has its language"
    );
    click(stage, "call-record-what");
    click(stage, "call-record-what-audio");
    click(stage, "call-record-confirm");
    let Some(CallAction::Record { audio_only, .. }) = actions(rig).last().cloned() else {
        panic!("{:?}", actions(rig));
    };
    assert!(audio_only);
    meeting(rig, transcribing_by(LAB_A, true));
    assert!(drawn(stage, "call-recording-notice"));
    assert!(drawn(stage, "call-recording-audio"), "the microphone's mark");
    assert!(!drawn(stage, "call-recording-dot"));
    assert!(
        !drawn(stage, "call-transcription-notice"),
        "the recording's notice says it"
    );
    open_record_menu(stage, "call-record-stop");
    click(stage, "call-record-stop");
    assert!(drawn(stage, "call-record-stop-dialog"));
    click(stage, "call-record-stop-confirm");
    assert_eq!(actions(rig).last(), Some(&CallAction::StopRecording { call: call(1) }));
    meeting(rig, teams_core::MeetingState::default());
    assert!(drawn(stage, "call-recording-stopped"));
    assert!(
        !drawn(stage, "call-transcription-stopped"),
        "the recording's stop says it"
    );
    assert!(!drawn(stage, "call-transcribing"));
    open_record_menu(stage, "call-transcript-toggle");
}

#[test]
fn someone_else_s_transcription_is_stopped_after_asking_and_its_stop_is_told() {
    let rig = calling();
    let stage = group_call_up(&rig);
    meeting(&rig, transcribing_by(LAB_B, false));
    assert!(drawn(stage, "call-transcription-notice"));
    assert!(
        drawn(stage, "call-recording-policy"),
        "everyone else is pointed at the policy"
    );
    assert!(!drawn(stage, "call-recording-privacy"));
    assert!(
        !drawn(stage, "call-transcript-pane"),
        "only its starter's opens by itself"
    );
    open_record_menu(stage, "call-transcribe-stop");
    click(stage, "call-transcribe-stop");
    assert!(drawn(stage, "call-transcribe-stop-dialog"));
    let before = actions(&rig).len();
    click(stage, "call-transcribe-stop-cancel");
    assert_eq!(actions(&rig).len(), before, "Cancel sends nothing");
    open_record_menu(stage, "call-transcribe-stop");
    click(stage, "call-transcribe-stop");
    click(stage, "call-transcribe-stop-confirm");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::StopTranscription { call: call(1) })
    );
    meeting(&rig, teams_core::MeetingState::default());
    assert!(drawn(stage, "call-transcription-stopped"));
    click(stage, "call-transcription-stopped-close");
    assert!(!drawn(stage, "call-transcription-stopped"));
    // the transcript lists its start and stop
    open_record_menu(stage, "call-transcript-toggle");
    click(stage, "call-transcript-toggle");
    assert!(drawn(stage, "call-transcript-pane"));
    click(stage, "call-transcript-close");
    assert!(!drawn(stage, "call-transcript-pane"));
}

#[test]
fn a_recording_and_transcription_records_the_audio_alone_from_more_options() {
    let rig = calling();
    let stage = group_call_up(&rig);
    open_record_menu(stage, "call-record-start");
    click(stage, "call-record-start");
    assert!(drawn(stage, "call-record-language"));
    assert!(
        !drawn(stage, "call-record-what"),
        "what to record is under More options"
    );
    click(stage, "call-record-more");
    click(stage, "call-record-what");
    click(stage, "call-record-what-audio");
    click(stage, "call-record-confirm");
    let Some(CallAction::Record {
        audio_only, language, ..
    }) = actions(&rig).last().cloned()
    else {
        panic!("{:?}", actions(&rig));
    };
    assert!(audio_only);
    assert_eq!(language, "en-us");
}

#[test]
fn the_account_starts_a_recording_after_asking_its_language_and_stops_it_after_asking() {
    let rig = calling();
    let stage = group_call_up(&rig);
    open_record_menu(stage, "call-record-start");
    click(stage, "call-record-start");
    assert!(drawn(stage, "call-record-dialog"), "Start recording asks first");
    let before = actions(&rig).len();
    click(stage, "call-record-cancel");
    assert!(!drawn(stage, "call-record-dialog"));
    assert_eq!(actions(&rig).len(), before, "Cancel sends nothing");
    open_record_menu(stage, "call-record-start");
    click(stage, "call-record-start");
    click(stage, "call-record-language");
    // one the scrolled list shows at once
    click(stage, "call-record-language-ar-sa");
    click(stage, "call-record-confirm");
    let Some(CallAction::Record {
        title,
        organizer,
        language,
        ..
    }) = actions(&rig).last().cloned()
    else {
        panic!("{:?}", actions(&rig));
    };
    assert_eq!(language, "ar-sa");
    assert!(title.starts_with("Call with "), "{title}");
    assert_eq!(organizer, None, "a call names no organizer");
    assert!(!drawn(stage, "call-record-dialog"));
    stop_own_recording(&rig, stage);
}

/// The account's own recording told, then stopped after asking.
fn stop_own_recording(rig: &Rig, stage: &mut VisualTestContext) {
    // the roster tells the account's recording: its own notice, with the privacy policy
    meeting(rig, recording_by(LAB_A));
    assert!(drawn(stage, "call-recording-notice"));
    assert!(drawn(stage, "call-recording-privacy"));
    assert!(drawn(stage, "call-recording-dot"));
    // Stop recording asks first
    open_record_menu(stage, "call-record-stop");
    click(stage, "call-record-stop");
    assert!(drawn(stage, "call-record-stop-dialog"));
    let before = actions(rig).len();
    click(stage, "call-record-stop-cancel");
    assert_eq!(actions(rig).len(), before, "Cancel sends nothing");
    open_record_menu(stage, "call-record-stop");
    click(stage, "call-record-stop");
    click(stage, "call-record-stop-confirm");
    assert_eq!(actions(rig).last(), Some(&CallAction::StopRecording { call: call(1) }));
    meeting(rig, teams_core::MeetingState::default());
    assert!(drawn(stage, "call-recording-stopped"), "it says it stopped");
    assert!(!drawn(stage, "call-recording-dot"));
    click(stage, "call-recording-stopped-close");
    assert!(!drawn(stage, "call-recording-stopped"));
}

#[test]
fn a_refused_recording_says_why_and_someone_else_s_shows_who_started_it() {
    let rig = calling();
    let stage = group_call_up(&rig);
    rig.send(Event::CallRecording {
        call: call(1),
        recording: teams_core::CallRecording {
            phase: teams_core::RecordingPhase::Idle,
            problem: Some(teams_core::RecordingProblem {
                heading: "Couldn't start recording",
                detail: "Recording not started. Your IT admin has disabled Recording.",
            }),
        },
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-recording-problem"));
    click(stage, "call-recording-problem-close");
    assert!(!drawn(stage, "call-recording-problem"));
    meeting(&rig, recording_by(LAB_B));
    assert!(drawn(stage, "call-recording-notice"));
    assert!(
        !drawn(stage, "call-recording-privacy"),
        "only its starter is told to let everyone know"
    );
    // anyone in the call may stop it
    open_record_menu(stage, "call-record-stop");
}

/// The account's own start, told `phase` by the engine (and a failure, when it failed).
fn own_start(rig: &Rig, phase: teams_core::RecordingPhase, problem: Option<teams_core::RecordingProblem>) {
    rig.send(Event::CallRecording {
        call: call(1),
        recording: teams_core::CallRecording { phase, problem },
    });
    rig.cx.run_until_parked();
}

#[test]
fn a_pending_start_says_it_is_starting_until_the_roster_tells_it_or_it_fails() {
    let rig = calling();
    let stage = group_call_up(&rig);
    // a transcription: Teams' "Starting transcription…", no recording's mark
    open_record_menu(stage, "call-transcribe-start");
    click(stage, "call-transcribe-start");
    click(stage, "call-transcribe-confirm");
    own_start(&rig, teams_core::RecordingPhase::Starting, None);
    assert!(drawn(stage, "call-record-pending"));
    assert!(!drawn(stage, "call-recording-pending"));
    click(stage, "call-record-pending-close");
    assert!(!drawn(stage, "call-record-pending"), "Close puts it away");
    own_start(&rig, teams_core::RecordingPhase::Idle, None);
    meeting(&rig, transcribing_by(LAB_A, false));
    assert!(drawn(stage, "call-transcription-notice"));
    meeting(&rig, teams_core::MeetingState::default());
    // a recording: the notice again, the record mark greyed by the duration, a spinner atop the open transcript
    open_record_menu(stage, "call-record-start");
    click(stage, "call-record-start");
    click(stage, "call-record-confirm");
    own_start(&rig, teams_core::RecordingPhase::Starting, None);
    assert!(drawn(stage, "call-record-pending"), "a new start shows it again");
    assert!(drawn(stage, "call-recording-pending"));
    assert!(drawn(stage, "call-transcript-pending"));
    // it took longer than Teams waits: Teams' words, and the pending marks go
    own_start(
        &rig,
        teams_core::RecordingPhase::Idle,
        Some(teams_core::RecordingProblem {
            heading: "Couldn't start recording and transcription.",
            detail: "Please try again. Contact your IT admin if this keeps happening.",
        }),
    );
    assert!(drawn(stage, "call-recording-problem"));
    for gone in [
        "call-record-pending",
        "call-recording-pending",
        "call-transcript-pending",
    ] {
        assert!(!drawn(stage, gone), "{gone} goes");
    }
}

#[test]
fn a_meeting_s_organizer_locks_it_after_asking_and_its_info_shows_how_to_join() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.open_prejoin(meeting_to_join(), cx)));
    let window = prejoin_window(&rig, &windows);
    click(window, "prejoin-join");
    status(&rig, 1, CallStatus::Joining);
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    assert!(!drawn(stage, "call-controls"), "not told it organizes");
    rig.send(Event::CallOwn {
        call: call(1),
        hand_raised: false,
        organizer: true,
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-controls"));
    click(stage, "call-roster");
    click(stage, "call-roster-more");
    assert!(drawn(stage, "call-roster-attendance"));
    click(stage, "call-roster-lock");
    assert!(drawn(stage, "call-lock-dialog"), "Lock asks first");
    let before = actions(&rig).len();
    click(stage, "call-lock-cancel");
    assert_eq!(actions(&rig).len(), before, "Cancel sends nothing");
    click(stage, "call-roster-more");
    click(stage, "call-roster-lock");
    click(stage, "call-lock-confirm");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::LockMeeting {
            call: call(1),
            locked: true
        })
    );
    // More → Meeting info: how to join, as the invitation says
    click(stage, "call-more");
    click(stage, "call-more-meeting-info");
    assert!(drawn(stage, "call-meeting-info-pane"));
    assert!(!drawn(stage, "call-meeting-passcode"), "not told yet");
    meeting(
        &rig,
        teams_core::MeetingState {
            locked: true,
            join: Some(teams_proto::MeetingJoinInfo {
                join_link: Some("https://teams.microsoft.com/meet/1112223334445?p=MadeUp".to_owned()),
                meeting_id: Some("111 222 333 444 5".to_owned()),
                passcode: Some("MadeUp1".to_owned()),
                help_link: None,
                reference_link: None,
            }),
            ..teams_core::MeetingState::default()
        },
    );
    for shown in [
        "call-meeting-join-link",
        "call-meeting-id",
        "call-meeting-passcode",
        "call-lock-notice",
    ] {
        assert!(drawn(stage, shown), "{shown}");
    }
}

#[test]
fn the_attendance_list_is_teams_utf16_tab_separated_file() {
    let joined = chrono::DateTime::parse_from_rfc3339("2026-01-02T15:04:05Z")
        .unwrap()
        .with_timezone(&chrono::Local);
    let bytes = teams_ui::call_stage::attendance_list(&[("Lab Bee".to_owned(), joined)]);
    assert_eq!(&bytes[..2], [0xff, 0xfe], "its BOM");
    let units: Vec<u16> = bytes[2..]
        .chunks(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = String::from_utf16(&units).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("Full Name\tUser Action\tTimestamp"));
    let row = lines.next().unwrap();
    assert!(row.starts_with("Lab Bee\tJoined\t"), "{row}");
    assert!(
        row.ends_with(&joined.format("%-m/%-d/%y, %-I:%M:%S %p").to_string()),
        "{row}"
    );
}

#[test]
fn the_timer_reads_and_changes_as_teams_does() {
    use teams_proto::{MeetingTimer, TimerState};
    use teams_ui::call_stage::{TimerChange, timer_after, timer_text};
    assert_eq!(timer_text(300_000), "05:00");
    assert_eq!(timer_text(59_001), "01:00");
    assert_eq!(timer_text(0), "00:00");
    let paused = MeetingTimer {
        state: TimerState::Paused,
        remaining_ms: 90_000,
        original_ms: 120_000,
        edited_ms: None,
        previous: None,
    };
    let played = timer_after(Some(&paused), TimerChange::Toggle, 0, 0).unwrap();
    assert_eq!((played.state, played.remaining_ms), (TimerState::InProgress, 90_000));
    let reset = timer_after(Some(&paused), TimerChange::Reset, 0, 0).unwrap();
    assert_eq!((reset.state, reset.remaining_ms), (TimerState::Paused, 120_000));
    let cancelled = timer_after(Some(&paused), TimerChange::Cancel, 0, 0).unwrap();
    assert_eq!(cancelled.state, TimerState::Cancelled);
    assert_eq!(timer_after(None, TimerChange::Reset, 0, 0), None, "no timer to reset");
}

/// The lab's third account, as People's box and Transfer find it.
const LAB_SEA: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";

/// The people the fake core knows: the lab's third account.
fn knows_sea(rig: &Rig) {
    rig.core.calendar.lock().unwrap().people = vec![teams_core::Invitable {
        mri: LAB_SEA.to_owned(),
        name: "Lab Sea".to_owned(),
        address: "sea@example.com".to_owned(),
    }];
}

/// `typed` in the box at `selector`, and what it found shown.
fn find(stage: &mut VisualTestContext, selector: &str, typed: &str) {
    click(stage, selector);
    stage.simulate_input(typed);
    stage.executor().advance_clock(std::time::Duration::from_millis(400));
    draw(stage);
}

#[test]
fn people_adds_someone_to_a_one_to_one_call_which_becomes_a_group_call_and_lists_who_doesn_t_answer() {
    let rig = calling();
    knows_sea(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    click(stage, "call-roster");
    find(stage, "call-invite", "Lab");
    click(stage, "call-invite-found-0");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Add {
            call: call(1),
            invitee: Mri::new(LAB_SEA)
        })
    );
    // listed ringing at once, the call still a 1:1 call's until the add was taken
    assert!(drawn(stage, "call-roster-invited-0"));
    assert!(drawn(stage, "call-hold"));
    rig.send(Event::CallGroupChat {
        call: call(1),
        chat: "19:00000000000000000000000000010001@thread.v2".to_owned(),
    });
    rig.cx.run_until_parked();
    // a group call's toolbar: Raise, no Hold or Transfer
    assert!(drawn(stage, "call-raise-hand"));
    assert!(!drawn(stage, "call-hold"));
    assert!(!drawn(stage, "call-transfer"));
    rig.send(Event::CallAddFailed {
        call: call(1),
        who: Mri::new(LAB_SEA),
    });
    rig.cx.run_until_parked();
    click(stage, "call-roster-call-again-0");
    let adds = actions(&rig)
        .into_iter()
        .filter(|action| matches!(action, CallAction::Add { .. }))
        .count();
    assert_eq!(adds, 2, "called again");
}

#[test]
fn transfer_holds_the_call_asks_whom_and_transfers_it_and_cancel_leaves_it_held() {
    let rig = calling();
    knows_sea(&rig);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let stage = stage(&rig, &windows);
    click(stage, "call-transfer");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Hold {
            call: call(1),
            held: true
        })
    );
    assert!(drawn(stage, "call-transfer-dialog"));
    click(stage, "call-transfer-cancel");
    assert!(!drawn(stage, "call-transfer-dialog"));
    assert!(
        !actions(&rig)
            .iter()
            .any(|action| matches!(action, CallAction::Hold { held: false, .. })),
        "Cancel leaves the call held"
    );
    rig.send(Event::CallHeld {
        call: call(1),
        held: true,
    });
    rig.cx.run_until_parked();
    click(stage, "call-transfer");
    assert!(drawn(stage, "call-transfer-dialog"));
    stage.simulate_input("Lab");
    stage.executor().advance_clock(std::time::Duration::from_millis(400));
    click(stage, "call-transfer-found-0");
    assert!(drawn(stage, "call-transfer-picked"));
    assert!(drawn(stage, "call-transfer-how"));
    click(stage, "call-transfer-go");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Transfer {
            call: call(1),
            target: Mri::new(LAB_SEA)
        })
    );
    assert!(!drawn(stage, "call-transfer-dialog"));
    // one not taken: the call stays held, and its bar says so
    rig.send(Event::CallTransferFailed { call: call(1) });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-resume"));
}

/// The group call up, the account told it organizes it.
fn organized_group_call(rig: &Rig) -> &'static mut VisualTestContext {
    let stage = group_call_up(rig);
    click(stage, "call-roster");
    assert!(!drawn(stage, "call-mute-all"), "not told it organizes");
    assert!(!drawn(stage, "call-controls"));
    rig.send(Event::CallOwn {
        call: call(1),
        hand_raised: false,
        organizer: true,
    });
    rig.cx.run_until_parked();
    stage
}

#[test]
fn the_organizer_mutes_everyone_after_asking() {
    let rig = calling();
    let stage = organized_group_call(&rig);
    assert!(
        drawn(stage, "call-roster-organizer"),
        "a group call names its organizer"
    );
    // Mute all asks first
    click(stage, "call-mute-all");
    assert!(drawn(stage, "call-mute-all-confirm"));
    let before = actions(&rig).len();
    click(stage, "call-mute-all-cancel");
    assert_eq!(actions(&rig).len(), before, "Cancel sends nothing");
    click(stage, "call-mute-all");
    click(stage, "call-mute-all-mute");
    assert_eq!(actions(&rig).last(), Some(&CallAction::MuteAll { call: call(1) }));
    // everyone else muted: Mute all goes
    let mut muted = view(
        &[
            (LAB_B, "Lab Bee", false, Some(1), false),
            (LAB_C, "Lab Sea", false, Some(2), false),
        ],
        false,
    );
    for participant in &mut muted.participants {
        participant.muted = true;
    }
    rig.send(Event::CallParticipants {
        call: call(1),
        view: muted,
    });
    rig.cx.run_until_parked();
    assert!(!drawn(stage, "call-mute-all"));
}

#[test]
fn controls_change_the_meeting_options() {
    use teams_proto::MeetingOption;
    let rig = calling();
    let stage = organized_group_call(&rig);
    // Controls: each option on as Teams starts them, the restrictions greyed while everyone presents
    click(stage, "call-controls");
    assert!(drawn(stage, "call-controls-menu"));
    for on in [
        "call-controls-lobby-on",
        "call-controls-present-on",
        "call-controls-chat-on",
        "call-controls-unmute-on",
        "call-controls-cameras-on",
    ] {
        assert!(drawn(stage, on), "{on}");
    }
    let before = actions(&rig).len();
    click(stage, "call-controls-unmute");
    assert_eq!(actions(&rig).len(), before, "greyed while everyone presents");
    click(stage, "call-controls-present");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::MeetingOption {
            call: call(1),
            option: MeetingOption::Present(false),
        })
    );
    assert!(!drawn(stage, "call-controls-present-on"), "shown at once");
    click(stage, "call-controls-unmute");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::MeetingOption {
            call: call(1),
            option: MeetingOption::Attendees {
                unmute: false,
                camera: true,
            },
        })
    );
    click(stage, "call-controls-lobby");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::MeetingOption {
            call: call(1),
            option: MeetingOption::Lobby(false),
        })
    );
    // the conversation tells the lobby back on, and the menu follows it
    meeting(
        &rig,
        teams_core::MeetingState {
            options: teams_proto::MeetingOptions {
                lobby: Some(true),
                ..teams_proto::MeetingOptions::default()
            },
            ..teams_core::MeetingState::default()
        },
    );
    assert!(drawn(stage, "call-controls-lobby-on"));
    click(stage, "call-controls-lock");
    assert!(!drawn(stage, "call-controls-menu"), "Lock closes the menu");
    assert!(drawn(stage, "call-lock-dialog"), "and asks first");
}

#[test]
fn people_added_to_the_chat_are_listed_with_request_to_join() {
    let rig = calling();
    let stage = group_call_up(&rig);
    click(stage, "call-roster");
    assert!(!drawn(stage, "call-roster-added"), "everyone is in the call");
    // Lab Sea left
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&[(LAB_B, "Lab Bee", false, Some(1), false)], false),
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-roster-added"));
    assert!(
        !drawn(stage, "call-roster-request-0"),
        "only while the pointer is on the row"
    );
    let row = stage.debug_bounds("call-roster-added-0").unwrap();
    stage.simulate_mouse_move(row.center(), None, Modifiers::none());
    click(stage, "call-roster-request-0");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Add {
            call: call(1),
            invitee: Mri::new(LAB_C),
        })
    );
}

#[test]
fn the_organizers_mute_shows_its_card_and_an_attendee_s_kept_controls_are_greyed() {
    let rig = calling();
    let stage = group_call_up(&rig);
    let muted_by = |version: u64| teams_core::MeetingState {
        muted_by_organizer: Some(version),
        ..teams_core::MeetingState::default()
    };
    meeting(&rig, muted_by(3));
    assert!(drawn(stage, "call-muted-toast"));
    // Unmute unmutes, and the card goes
    click(stage, "call-muted-unmute");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::Mute {
            call: call(1),
            muted: false,
        })
    );
    assert!(!drawn(stage, "call-muted-toast"));
    // a new mute shows it again; its close takes it away
    meeting(&rig, muted_by(5));
    assert!(drawn(stage, "call-muted-toast"));
    click(stage, "call-muted-close");
    assert!(!drawn(stage, "call-muted-toast"));
    // an attendee kept from unmuting: the microphone greyed, its key refused
    meeting(
        &rig,
        teams_core::MeetingState {
            muted_by_organizer: Some(5),
            attendee: true,
            options: teams_proto::MeetingOptions {
                present: Some(false),
                unmute: Some(false),
                camera: Some(false),
                ..teams_proto::MeetingOptions::default()
            },
            ..teams_core::MeetingState::default()
        },
    );
    assert!(drawn(stage, "call-mute-blocked"));
    let before = actions(&rig).len();
    stage.simulate_keystrokes("ctrl-shift-m");
    stage.run_until_parked();
    assert_eq!(actions(&rig).len(), before, "the key asks nothing");
}

#[gpui::test]
fn failed_background_media_setup_hides_calls_and_drops_an_incoming_ring(cx: &mut TestAppContext) {
    use teams_media::devices::catalog::DeviceStatus;
    let directory = Arc::new(FixedDirectory::default());
    directory.set_status(DeviceStatus::Preparing);
    let calls = cx.new(|_| Calls::shown(Some(Mri::new(ECHO))));
    cx.update(|cx| teams_ui::calls::follow_devices(&calls, directory.clone(), cx));
    assert!(calls.read_with(cx, |calls, _| calls.shows()));
    calls.update(cx, |calls, cx| {
        calls.apply(
            &Event::IncomingCall(IncomingCall {
                call: CallId::for_tests(1),
                caller: Mri::new(LAB_B),
                caller_name: None,
                group_chat: None,
            }),
            cx,
        );
    });
    assert!(calls.read_with(cx, |calls, _| calls.ringing().is_some()));
    directory.set_status(DeviceStatus::Unavailable);
    cx.run_until_parked();
    assert!(!calls.read_with(cx, |calls, _| calls.shows()));
    assert!(calls.read_with(cx, |calls, _| calls.ringing().is_none()));
}

#[gpui::test]
fn a_call_waiting_for_discovery_shows_preparing_then_calling(cx: &mut TestAppContext) {
    let calls = cx.new(|_| Calls::shown(Some(Mri::new(ECHO))));
    for (status, text) in [(CallStatus::Preparing, "Preparing…"), (CallStatus::Placing, "Calling…")] {
        calls.update(cx, |calls, cx| {
            calls.apply(
                &Event::Call {
                    call: CallId::for_tests(1),
                    status,
                },
                cx,
            );
        });
        assert_eq!(
            calls.read_with(cx, |calls, _| teams_ui::calls::status_text(calls.call().unwrap())),
            text
        );
    }
    assert_eq!(teams_ui::calls::status_name(&CallStatus::Preparing), "preparing");
}
