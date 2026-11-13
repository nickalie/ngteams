//! The control socket's tests' harness: the app launched on GPUI's test context with `Launch::ctl` over the personal
//! build's engine (no token in its store, so no request leaves the process; what it reads is seeded into its store
//! before it starts), and `teams_ctl`'s client on a thread of its own while the test thread steps GPUI (the requests
//! are answered on its foreground).

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gpui::{App, TestAppContext};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch, Launched};
use ngteams::setup::Setup;
use teams_auth::secrets::{MemorySecretStore, SecretStore};
use teams_core::{CallFloor, EngineRequests};
use teams_ctl::protocol::{Request, Response};
use teams_ctl::{Client, ClientError, Timeouts};
use teams_mock::CHAT_BASE;
use teams_store::Store;
use teams_ui::bus::BusError;
use teams_ui::{SettingsFile, ViewTrace};

pub const WAIT: Duration = Duration::from_secs(30);
pub const UPN: &str = "someone@example.test";
/// The account's MRI, its object id's.
pub const ME: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";

/// A personal build signed in once (its account and region saved) under `dir`, with no refresh token anywhere.
pub fn signed_in_setup(dir: &Path) -> (Setup, PersonalAccount) {
    let paths = PersonalPaths::new(dir.join("config/ngteams"), dir.join("share/ngteams"));
    let setup = Setup::new(paths).unwrap();
    let account = signed_in(&setup, UPN, "00000000-0000-4000-8000-0000000000ff");
    (setup, account)
}

/// The account `upn` (object id `oid`) signed in once under `setup`: saved with its region, no refresh token.
pub fn signed_in(setup: &Setup, upn: &str, oid: &str) -> PersonalAccount {
    let account = PersonalAccount::new(upn, oid, "00000000-0000-4000-8000-000000000001").unwrap();
    account.save(&setup.paths).unwrap();
    setup.paths.make_dirs().unwrap();
    let region = setup.paths.region_file(&account).unwrap();
    fs::create_dir_all(region.parent().unwrap()).unwrap();
    fs::write(
        region,
        format!(
            r#"{{"_synthetic":true,"region":"amer","partition":null,"regionGtms":{{"chatServiceAfd":"{CHAT_BASE}"}}}}"#
        ),
    )
    .unwrap();
    account
}

/// The app launched under `dir`, serving its control socket at `socket`.
pub fn launched(cx: &TestAppContext, dir: &Path, socket: &Path) -> Launched {
    launched_with(cx, dir, socket, &Rc::new(Cell::new(0)), |_| {}).0
}

/// [`launched`] over a store `seed` filled first, with its engine's requests and `asked` counting each time the
/// window asks to sign in again.
pub fn launched_with(
    cx: &TestAppContext,
    dir: &Path,
    socket: &Path,
    asked: &Rc<Cell<u32>>,
    seed: impl Fn(&mut Store),
) -> (Launched, EngineRequests) {
    launched_with_others(cx, dir, socket, asked, seed, &[])
}

/// [`launched_with`], with more accounts after the first, each `(upn, object id)`, started as the app starts them;
/// each account's store filled by `seed` first.
pub fn launched_with_others(
    cx: &TestAppContext,
    dir: &Path,
    socket: &Path,
    asked: &Rc<Cell<u32>>,
    seed: impl Fn(&mut Store),
    others: &[(&str, &str)],
) -> (Launched, EngineRequests) {
    launched_with_others_on(
        cx,
        dir,
        socket,
        asked,
        |setup, account| started(setup, account, &seed, MemorySecretStore::new()),
        others,
    )
}

pub fn launched_with_others_on<S: SecretStore + 'static>(
    cx: &TestAppContext,
    dir: &Path,
    socket: &Path,
    asked: &Rc<Cell<u32>>,
    start: impl Fn(&Setup, PersonalAccount) -> Started<S>,
    others: &[(&str, &str)],
) -> (Launched, EngineRequests) {
    let (setup, account) = signed_in_setup(dir);
    let first = start(&setup, account);
    let requests = first.engine.handle.requests();
    let others = others
        .iter()
        .map(|(upn, oid)| start(&setup, signed_in(&setup, upn, oid)))
        .collect();
    let asked = Rc::clone(asked);
    let launch = Launch {
        first,
        others,
        settings: app::settings(SettingsFile::new(dir.join("settings.toml")), None),
        bus: Err(BusError::NotGiven),
        sign_in_again: Rc::new(move |_| {
            let asked = Rc::clone(&asked);
            Rc::new(move |_| asked.set(asked.get() + 1))
        }),
        runner: Runner {
            setup,
            floor: CallFloor::new(),
            devices: None,
        },
        read_only: false,
        trace: Arc::new(ViewTrace::off()),
        ctl: Some(socket.to_owned()),
    };
    let Ok(launched) = cx.update(|cx| app::launch(cx, launch)) else {
        panic!("the window opens");
    };
    cx.run_until_parked();
    (launched, requests)
}

/// `account`'s engine started over a store `seed` filled, without a push channel or calling.
pub fn started<S: SecretStore + 'static>(
    setup: &Setup,
    account: PersonalAccount,
    seed: &impl Fn(&mut Store),
    secrets: S,
) -> Started<S> {
    let store_file = setup.paths.store_file(&account).unwrap();
    fs::create_dir_all(store_file.parent().unwrap()).unwrap();
    seed(&mut Store::open(&store_file).unwrap());
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), secrets, false, None).unwrap();
    Started {
        account,
        engine,
        calls: None,
    }
}

/// `work` run on a thread of its own while the test thread steps GPUI, which answers its requests.
pub fn pumped<T: Send + 'static>(cx: &TestAppContext, work: impl FnOnce() -> T + Send + 'static) -> T {
    let thread = thread::spawn(work);
    pump_until_done(cx, &thread);
    thread.join().unwrap()
}

pub fn pump_until_done<T>(cx: &TestAppContext, thread: &JoinHandle<T>) {
    let start = Instant::now();
    while !thread.is_finished() {
        cx.run_until_parked();
        assert!(start.elapsed() < WAIT, "the client wasn't answered in time");
        thread::sleep(Duration::from_millis(2));
    }
}

/// GPUI stepped (its timers too, by `step` each time) until `done`.
pub fn pump_until(cx: &TestAppContext, step: Duration, mut done: impl FnMut(&App) -> bool) {
    let start = Instant::now();
    loop {
        cx.executor().advance_clock(step);
        cx.run_until_parked();
        if cx.read(&mut done) {
            return;
        }
        assert!(start.elapsed() < WAIT, "the app didn't settle in time");
        thread::sleep(Duration::from_millis(5));
    }
}

pub fn timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(5),
        read: WAIT,
        write: WAIT,
    }
}

pub fn connect(socket: &Path) -> Result<Client, ClientError> {
    Client::connect(socket, timeouts())
}

/// `request` for `account` (the shown one without it), asked by a client of its own.
pub fn ask(
    cx: &TestAppContext,
    socket: &Path,
    account: Option<&str>,
    request: Request,
) -> Result<Response, ClientError> {
    let socket = socket.to_owned();
    let account = account.map(str::to_owned);
    pumped(cx, move || connect(&socket)?.request(account.as_deref(), request))
}

pub fn socket_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("run/ngteams/ctl.sock")
}

pub fn quit(cx: &TestAppContext) {
    cx.update(app::quit);
    cx.run_until_parked();
}
