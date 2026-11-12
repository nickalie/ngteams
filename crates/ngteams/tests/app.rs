//! The personal app's sign-out on GPUI's test context: once the session can't renew its tokens the window shows
//! "signed out", the engine is stopped (nothing more is sent: every request answers that it stopped) and the sign-in
//! window is opened again, once; the settings ask for the first run's screen. The personal build's window shows calls
//! when its devices opened (its test call to the web bundle's echo service, an invitation ringing by the name it
//! gives), and none without them; these tests select only sandbox PipeWire. The engine is the personal
//! build's own (`ngteams::engine::start`) over a `MemorySecretStore` holding no refresh token, so no request leaves the
//! process; the calls themselves run in `tests/calls.rs`. The rail's "+" opens the window asking for the account, and
//! the account it names reaches the app as it runs (an account it runs already said on that window).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::cell::Cell;
use std::fs;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{TestAppContext, WindowHandle};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch, SIGNED_OUT_HINT, SignInAgain};
use ngteams::calling::PersonalCalls;
use ngteams::setup::Setup;
use teams_auth::secrets::MemorySecretStore;
use teams_core::CallFloor;
use teams_core::{CallAction, CallActionError, CallId, EngineError, Event, IncomingCall, RegistrationReport};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_mock::CHAT_BASE;
use teams_ui::bus::BusError;
use teams_ui::window::KitRoot;
use teams_ui::{Account, AccountPrompt, AccountPromptEvent, DesktopEvent, Engine, SettingsFile, ViewTrace};

const WAIT: Duration = Duration::from_secs(30);

/// A personal build signed in once (its account and region saved) under `dir`, with no refresh token anywhere.
fn signed_in_setup(dir: &tempfile::TempDir) -> (Setup, PersonalAccount) {
    let paths = PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"));
    let setup = Setup::new(paths).unwrap();
    let account = PersonalAccount::new(
        "someone@example.test",
        "00000000-0000-4000-8000-0000000000ff",
        "00000000-0000-4000-8000-000000000001",
    )
    .unwrap();
    account.save(&setup.paths).unwrap();
    setup.paths.make_dirs().unwrap();
    let region = setup.paths.region_file(&account).unwrap();
    fs::create_dir_all(region.parent().unwrap()).unwrap();
    fs::write(
        region,
        format!(r#"{{"region": "amer", "partition": null, "regionGtms": {{"chatServiceAfd": "{CHAT_BASE}"}}}}"#),
    )
    .unwrap();
    (setup, account)
}

/// Steps GPUI's test context until `done` holds: the engine answers on a thread of its own.
fn settle(cx: &TestAppContext, mut done: impl FnMut(&TestAppContext) -> bool) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(start.elapsed() < WAIT, "not settled in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn signed_out_shows_it_stops_the_engine_and_opens_the_sign_in_window_again() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);

    let engine = ngteams::engine::start(
        &setup.paths,
        &account,
        teams_core::CallFloor::new(),
        MemorySecretStore::new(),
        false,
        None,
    )
    .unwrap();
    let core = engine.handle.requests();
    let cx = TestAppContext::single();
    let (models, bridge) = cx.update(|cx| app::bridge(engine.handle, None, cx));
    let asked = Rc::new(Cell::new(0));
    let counted = Rc::clone(&asked);
    let again: SignInAgain = Rc::new(move |_| counted.set(counted.get() + 1));
    cx.update(|cx| app::watch_sign_out(engine.session.watch_signed_out(), bridge.downgrade(), again, cx));

    // the first request that needs a token finds the account signed out
    assert!(core.sync().blocking_wait().is_err());
    settle(&cx, |_| asked.get() > 0);
    let status = cx.read(|cx| models.status.read(cx).clone());
    assert_eq!(
        status.account,
        Account::SignedOut {
            hint: SIGNED_OUT_HINT.into()
        }
    );
    assert_eq!(
        status.engine,
        Engine::Stopped,
        "the window opens again only once the engine is down"
    );
    assert!(!cx.read(|cx| bridge.read(cx).running()));
    // nothing more is sent: every request answers that the engine stopped
    let after = core.sync().blocking_wait();
    assert!(matches!(after, Err(EngineError::Stopped)), "{after:?}");
    cx.run_until_parked();
    assert_eq!(asked.get(), 1, "the sign-in window is opened again once");
}

/// What the models show of a call once the engine registered the calling endpoint and an invitation rang: whether
/// calls are shown, registered, ringing, idle, and the test call can be asked for.
fn shown_after_a_ring(calls: Option<&PersonalCalls>) -> (bool, bool, bool, bool, bool) {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);

    // the engine without calling: the models' side alone is under test here (tests/calls.rs runs the calls)
    let engine = ngteams::engine::start(
        &setup.paths,
        &account,
        teams_core::CallFloor::new(),
        MemorySecretStore::new(),
        false,
        None,
    )
    .unwrap();
    let cx = TestAppContext::single();
    let (models, _bridge) = cx.update(|cx| app::bridge(engine.handle, calls, cx));
    cx.update(|cx| {
        models.apply(Event::CallingRegistration(RegistrationReport::Registered), cx);
        models.apply(
            Event::IncomingCall(IncomingCall {
                call: CallId::for_tests(1),
                caller: teams_proto::Mri::new("8:orgid:00000000-0000-4000-8000-00000000000b"),
                caller_name: Some("Someone Else".to_owned()),
                group_chat: None,
            }),
            cx,
        );
    });
    cx.read(|cx| {
        let calls = models.calls.read(cx);
        (
            calls.shows(),
            calls.registered(),
            calls.ringing().is_some(),
            calls.idle(),
            calls.has_test_call(),
        )
    })
}

#[test]
fn the_personal_build_shows_its_calls_and_an_invitation_rings_by_the_name_it_gives() {
    // file devices: no PipeWire, no portal
    let devices = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
    let (engine_calls, calls) = ngteams::calling::on_devices(devices, false);
    assert_eq!(
        calls.echo.as_str(),
        teams_proto::TEAMS_ECHO_BOT_MRI,
        "the web bundle's echo service"
    );
    assert!(calls.ringer.is_some() && calls.video.is_none());
    assert!(engine_calls.media.video.is_none(), "no video asked, none");
    assert_eq!(
        shown_after_a_ring(Some(&calls)),
        (true, true, true, false, true),
        "shown, registered, ringing (so not idle), with the test call"
    );
}

#[test]
fn without_its_devices_the_personal_build_shows_no_call() {
    assert_eq!(
        shown_after_a_ring(None),
        (false, false, false, false, false),
        "nothing of a call is kept, shown or offered"
    );
}

#[test]
fn sandbox_devices_are_refused_outside_the_sandbox_and_the_engine_without_them_places_no_call() {
    // Sandbox reach refuses PipeWire outside the UI sandbox, before any desktop connection.
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);
    assert!(
        ngteams::calling::open(teams_media::devices::Reach::Sandbox)
            .0
            .media
            .devices
            .microphone()
            .is_err()
            || std::env::var_os("UI_SANDBOX_DIR").is_some()
    );

    let engine = ngteams::engine::start(
        &setup.paths,
        &account,
        teams_core::CallFloor::new(),
        MemorySecretStore::new(),
        false,
        None,
    )
    .unwrap();
    let refused = engine
        .handle
        .requests()
        .call(CallAction::TestCall {
            echo: ngteams::calling::echo(),
        })
        .blocking_wait();
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::NoCalling))),
        "no calling: {refused:?}"
    );
    engine.handle.shutdown();
}

#[test]
fn the_settings_ask_for_the_first_run_s_screen() {
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    let settings = app::settings(file, None);
    assert!(settings.awaiting_first_run());
    assert!(!settings.sends_typing() && !settings.sends_read_receipts() && !settings.sends_presence());
}

/// The window asking for an account the rail's "+" opened: the app's window other than `main`.
fn the_prompt(cx: &TestAppContext, main: WindowHandle<KitRoot>) -> gpui::Entity<AccountPrompt> {
    cx.update(|cx| {
        let prompts: Vec<gpui::Entity<AccountPrompt>> = cx
            .windows()
            .into_iter()
            .filter(|window| window.window_id() != main.window_id())
            .filter_map(|window| window.downcast::<KitRoot>())
            .filter_map(|window| window.read(cx).ok()?.view().clone().downcast::<AccountPrompt>().ok())
            .collect();
        assert_eq!(prompts.len(), 1, "one window asks for the account");
        prompts.into_iter().next().unwrap()
    })
}

#[test]
fn the_rail_s_plus_asks_for_an_account_and_says_one_the_app_runs_already() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);

    let engine = ngteams::engine::start(
        &setup.paths,
        &account,
        teams_core::CallFloor::new(),
        MemorySecretStore::new(),
        false,
        None,
    )
    .unwrap();
    let cx = TestAppContext::single();
    let launch = Launch {
        first: Started {
            account: account.clone(),
            engine,
            calls: None,
        },
        others: Vec::new(),
        settings: app::settings(SettingsFile::new(dir.path().join("settings.toml")), None),
        bus: Err(BusError::NotGiven),
        sign_in_again: Rc::new(|_| Rc::new(|_| {})),
        runner: Runner {
            setup: setup.clone(),
            floor: CallFloor::new(),
            devices: None,
        },
        read_only: false,
        trace: Arc::new(ViewTrace::off()),
        ctl: None,
    };
    let Ok(launched) = cx.update(|cx| app::launch(cx, launch)) else {
        panic!("the window opens");
    };
    cx.run_until_parked();

    // the rail's "+"
    cx.update(|cx| launched.desktop.update(cx, |_, cx| cx.emit(DesktopEvent::AddAccount)));
    cx.run_until_parked();
    let prompt = the_prompt(&cx, launched.window);

    // the account named there reaches the app as it runs (it used to update the app inside its own update: a panic
    // that quit it): one it runs already, case aside, is said under the field, and no sign-in window opens for it
    let named = account.upn.to_uppercase();
    cx.update(|cx| prompt.update(cx, |_, cx| cx.emit(AccountPromptEvent::SignIn(named.clone()))));
    cx.run_until_parked();
    let problem = cx.read(|cx| prompt.read(cx).problem().map(ToString::to_string));
    assert_eq!(problem, Some(format!("{named} is added already")));
    assert!(
        !cx.read(|cx| prompt.read(cx).is_signing_in()),
        "the field is open again"
    );

    // a second "+" brings the same window forward, not another one
    cx.update(|cx| launched.desktop.update(cx, |_, cx| cx.emit(DesktopEvent::AddAccount)));
    cx.run_until_parked();
    assert_eq!(the_prompt(&cx, launched.window), prompt);

    cx.update(app::quit);
    cx.run_until_parked();
}

#[test]
fn the_read_only_mode_adds_removes_and_signs_in_no_account_and_says_signed_out_only() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);

    let engine = ngteams::engine::start_read_only(
        &setup.paths,
        &account,
        teams_core::CallFloor::new(),
        MemorySecretStore::new(),
    )
    .unwrap();
    let core = engine.handle.requests();
    assert!(core.read_only(), "the views see the read-only mode");
    let signed_out = engine.session.watch_signed_out();
    let cx = TestAppContext::single();
    let asked = Rc::new(Cell::new(0));
    let counted = Rc::clone(&asked);
    let launch = Launch {
        first: Started {
            account: account.clone(),
            engine,
            calls: None,
        },
        others: Vec::new(),
        settings: app::settings(SettingsFile::new(dir.path().join("settings.toml")), None).read_only(),
        bus: Err(BusError::NotGiven),
        sign_in_again: Rc::new(move |_| {
            let counted = Rc::clone(&counted);
            Rc::new(move |_| counted.set(counted.get() + 1))
        }),
        runner: Runner {
            setup: setup.clone(),
            floor: CallFloor::new(),
            devices: None,
        },
        read_only: true,
        trace: Arc::new(ViewTrace::off()),
        ctl: None,
    };
    let Ok(launched) = cx.update(|cx| app::launch(cx, launch)) else {
        panic!("the window opens");
    };
    cx.run_until_parked();
    let windows = || cx.update(|cx| cx.windows().len());
    assert_eq!(windows(), 1);

    // no "+": asked anyway, no window asks for an account
    cx.update(|cx| launched.desktop.update(cx, |_, cx| cx.emit(DesktopEvent::AddAccount)));
    cx.run_until_parked();
    assert_eq!(windows(), 1, "no window asks for an account");
    // nor "Sign out and remove": asked anyway, the account and its files stay
    cx.update(|cx| {
        launched
            .desktop
            .update(cx, |_, cx| cx.emit(DesktopEvent::RemoveAccount(core.me().to_owned())));
    });
    cx.run_until_parked();
    assert!(PersonalAccount::find(&setup.paths, &account.upn).unwrap().is_some());
    assert!(setup.paths.region_file(&account).unwrap().is_file());

    // signed out (no refresh token in the store): the engine stops, and no sign-in window opens
    assert!(core.sync().blocking_wait().is_err());
    let mut watching = signed_out;
    settle(&cx, |_| watching.borrow_and_update().is_some());
    settle(&cx, |_| {
        matches!(core.sync().blocking_wait(), Err(EngineError::Stopped))
    });
    cx.run_until_parked();
    let (can_add, read_only) = cx.read(|cx| {
        let desktop = launched.desktop.read(cx);
        (
            desktop.accounts().read(cx).can_add(),
            desktop.settings().read(cx).is_read_only(),
        )
    });
    assert!(!can_add, "the rail offers no \"+\" and no \"Sign out and remove\"");
    assert!(read_only, "no typing, read receipt or presence");
    assert_eq!(asked.get(), 0, "no sign-in window");
    cx.update(app::quit);
    cx.run_until_parked();
}
