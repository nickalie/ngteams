//! Account lifecycle controllers on a mock platform with empty in-memory credentials.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::accounts_file::PersonalPaths;
use gpui::TestAppContext;
use teams_auth::secrets::MemorySecretStore;
use teams_core::CallFloor;

use super::*;
use crate::setup::Setup;

pub(crate) fn setup(root: &Path, suffix: u8) -> (Setup, PersonalAccount) {
    let paths = PersonalPaths::new(root.join("config/ngteams"), root.join("data/ngteams"));
    let setup = Setup::new(paths).unwrap();
    let account = PersonalAccount::new(
        &format!("synthetic-{suffix}@example.test"),
        &format!("00000000-0000-4000-8000-{suffix:012x}"),
        "00000000-0000-4000-8000-000000000001",
    )
    .unwrap();
    account.save(&setup.paths).unwrap();
    let region = setup.paths.region_file(&account).unwrap();
    std::fs::create_dir_all(region.parent().unwrap()).unwrap();
    std::fs::write(
        region,
        format!(
            r#"{{"_synthetic":true,"region":"amer","partition":null,"regionGtms":{{"chatServiceAfd":"{}"}}}}"#,
            teams_mock::CHAT_BASE
        ),
    )
    .unwrap();
    (setup, account)
}

fn started(setup: &Setup, account: PersonalAccount) -> Started<MemorySecretStore> {
    let engine = crate::engine::start(
        &setup.paths,
        &account,
        teams_core::CallFloor::new(),
        MemorySecretStore::new(),
        false,
        None,
    )
    .unwrap();
    Started {
        account,
        engine,
        calls: None,
    }
}

fn open(cx: &TestAppContext, setup: &Setup, account: PersonalAccount) -> Entity<Running> {
    let launch = Launch {
        first: started(setup, account),
        others: Vec::new(),
        settings: settings(SettingsFile::new(setup.paths.settings_file()), None),
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
    assert!(cx.update(|cx| super::launch(cx, launch)).is_ok());
    cx.run_until_parked();
    cx.read(|cx| cx.global::<Held>().running.clone())
}

fn wait(cx: &TestAppContext, mut done: impl FnMut(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.read(&mut done) {
            return;
        }
        assert!(Instant::now() < deadline, "account lifecycle did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn adding_focuses_one_prompt_then_selects_the_started_account_and_closes_it() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, first) = setup(dir.path(), 1);
    let (_, second) = self::setup(dir.path(), 2);
    let cx = TestAppContext::single();
    let running = open(&cx, &setup, first);
    cx.update(|cx| running.update(cx, Running::ask_account));
    let prompt = cx.read(|cx| running.read(cx).adding.unwrap());
    cx.update(|cx| running.update(cx, Running::ask_account));
    assert_eq!(cx.read(|cx| running.read(cx).adding.unwrap()), prompt);
    let added = started(&setup, second.clone());
    cx.update(|cx| running.update(cx, |running, cx| running.added(added, cx)));
    assert!(cx.read(|cx| running.read(cx).adding.is_none()));
    assert_eq!(cx.read(|cx| running.read(cx).bridges.len()), 2);
    assert!(cx.update(|cx| prompt.read(cx).is_err()));
    cx.update(quit);
    cx.run_until_parked();
}

#[test]
fn a_lab_alias_keeps_sending_enabled_without_opening_an_add_account_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, first) = setup(dir.path(), 1);
    let (socket, peer) = teams_auth::broker::private_pair().unwrap();
    let setup = setup.with_secrets(crate::secrets::AppSecrets::lab(socket).unwrap());
    let cx = TestAppContext::single();
    let running = open(&cx, &setup, first);
    assert!(!cx.read(|cx| running.read(cx).read_only));
    assert!(!cx.read(|cx| running.read(cx).can_add_account()));
    let before = cx.update(|cx| cx.windows().len());
    let desktop = cx.read(|cx| running.read(cx).desktop.clone());
    cx.read(|cx| {
        let rail = desktop.read(cx).accounts().read(cx);
        assert!(!rail.can_add());
        assert!(rail.can_remove(), "lab forget remains available");
    });
    cx.update(|cx| desktop.update(cx, |_, cx| cx.emit(DesktopEvent::AddAccount)));
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), before);
    assert!(cx.read(|cx| running.read(cx).adding.is_none()));
    cx.update(quit);
    cx.run_until_parked();
    drop((setup, peer));
}

#[test]
fn removal_waits_for_shutdown_and_the_last_account_quits_even_when_forget_fails() {
    for broken in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (setup, first) = setup(dir.path(), 1);
        let (_, second) = self::setup(dir.path(), 2);
        let cx = TestAppContext::single();
        let running = open(&cx, &setup, first);
        let added = started(&setup, second);
        cx.update(|cx| running.update(cx, |running, cx| running.added(added, cx)));
        // Neither forget may ever reach the real Secret Service. Missing/corrupt metadata returns before any lookup.
        std::fs::remove_file(setup.paths.accounts_file()).unwrap();
        if broken {
            std::fs::write(setup.paths.accounts_file(), b"invalid accounts").unwrap();
        }
        let keys = cx.read(|cx| {
            running
                .read(cx)
                .bridges
                .iter()
                .map(|(key, _, _)| key.clone())
                .collect::<Vec<_>>()
        });
        cx.update(|cx| running.update(cx, |running, cx| running.remove("unknown-account", cx)));
        assert_eq!(cx.read(|cx| running.read(cx).bridges.len()), 2);
        cx.update(|cx| running.update(cx, |running, cx| running.remove(&keys[0], cx)));
        wait(&cx, |cx| running.read(cx).bridges.len() == 1);
        cx.update(|cx| running.update(cx, |running, cx| running.remove(&keys[1], cx)));
        wait(&cx, |cx| running.read(cx).quitting);
        cx.update(|cx| running.update(cx, Running::quit));
        cx.run_until_parked();
    }
}

#[test]
fn an_account_added_and_one_removed_are_told_to_the_event_streams() {
    use teams_ctl::protocol::{CtlEvent, EventFilter};

    let dir = tempfile::tempdir().unwrap();
    let (setup, first) = setup(dir.path(), 1);
    let (_, second) = self::setup(dir.path(), 2);
    let cx = TestAppContext::single();
    let running = open(&cx, &setup, first);
    let wanted = crate::ctl::events::Wanted {
        filter: EventFilter {
            kinds: vec!["account_*".to_owned()],
            ..EventFilter::default()
        },
        ..crate::ctl::events::Wanted::default()
    };
    let stream = cx.read(|cx| running.read(cx).hub().subscribe(wanted));
    let added = started(&setup, second.clone());
    cx.update(|cx| running.update(cx, |running, cx| running.added(added, cx)));
    // the forget must never reach the real Secret Service: without the accounts' file it returns before any lookup
    std::fs::remove_file(setup.paths.accounts_file()).unwrap();
    let key = cx.read(|cx| running.read(cx).bridges[1].0.clone());
    cx.update(|cx| running.update(cx, |running, cx| running.remove(&key, cx)));
    wait(&cx, |cx| running.read(cx).bridges.len() == 1);
    let told: Vec<(Option<String>, CtlEvent)> = std::iter::from_fn(|| stream.try_recv().ok())
        .map(|line| (line.account, line.event))
        .collect();
    let upn = Some(second.upn);
    assert_eq!(
        told,
        [(upn.clone(), CtlEvent::AccountAdded), (upn, CtlEvent::AccountRemoved)]
    );
    cx.update(quit);
    cx.run_until_parked();
}

#[test]
fn an_invalid_added_account_reports_the_failure_in_its_existing_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = setup(dir.path(), 1);
    let cx = TestAppContext::single();
    let running = open(&cx, &setup, account);
    cx.update(|cx| running.update(cx, Running::ask_account));
    let prompt = cx.read(|cx| {
        running
            .read(cx)
            .adding
            .unwrap()
            .read(cx)
            .unwrap()
            .view()
            .clone()
            .downcast::<AccountPrompt>()
            .unwrap()
    });
    cx.update(|cx| {
        prompt.update(cx, |_, cx| cx.emit(AccountPromptEvent::SignIn("not-an-upn".to_owned())));
    });
    wait(&cx, |cx| prompt.read(cx).problem().is_some());
    assert!(!cx.read(|cx| prompt.read(cx).is_signing_in()));
    assert_eq!(cx.read(|cx| running.read(cx).bridges.len()), 1);
    let desktop = cx.read(|cx| running.read(cx).desktop.clone());
    cx.update(|cx| desktop.update(cx, |_, cx| cx.emit(DesktopEvent::Quit)));
    cx.run_until_parked();
}

#[test]
fn account_video_and_device_settings_follow_the_shared_model_after_an_account_is_added() {
    // The unit backend is silent FileDevices + FixedDirectory; the ordinary build still requires real PipeWire.
    let devices = crate::calling::CallDevices::open(teams_media::devices::Reach::Sandbox);
    let (_, calls) = devices.for_account();
    let first_feed = calls.video.clone().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (setup, first) = setup(dir.path(), 1);
    let (_, second) = self::setup(dir.path(), 2);
    let bad_settings = SettingsFile::new(dir.path().join("invalid-settings.toml"));
    std::fs::write(dir.path().join("invalid-settings.toml"), b"invalid settings").unwrap();
    let cx = TestAppContext::single();
    let shared = cx.update(|cx| shared_settings(SettingsModel::load(bad_settings), Some(&calls), cx));
    cx.update(|cx| {
        shared.update(cx, |settings, cx| {
            settings.change(|settings| settings.video_calls = false, cx);
        });
    });
    cx.run_until_parked();
    assert!(!first_feed.enabled());
    let running = open(&cx, &setup, first);
    let mut added = started(&setup, second);
    let (_, calls) = devices.for_account();
    let added_feed = calls.video.clone().unwrap();
    added.calls = Some(calls);
    cx.update(|cx| running.update(cx, |running, cx| running.added(added, cx)));
    let model = cx.read(|cx| running.read(cx).settings.clone());
    cx.update(|cx| {
        model.update(cx, |settings, cx| {
            settings.change(|settings| settings.video_calls = false, cx);
        });
    });
    cx.run_until_parked();
    assert!(!added_feed.enabled());
    cx.update(quit);
    cx.run_until_parked();
}
