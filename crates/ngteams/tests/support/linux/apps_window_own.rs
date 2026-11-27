//! The Store's own-server side: an own server's account on the real `ngteams-server` binary (`NGTEAMS_QA_SERVER_BIN`,
//! else the one `cargo build -p ngteams-server` left in the target directory), run as an operator runs it, its user
//! made with `user add` and one synthetic app registered with `app add`. The server names `appsService` in its authz
//! (`apps.enabled`, on by default), so the rail's Store opens and lists that app. Without the binary it says so and
//! skips.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::{fs, thread};

use gpui::{App, WindowingModes};
use ngteams::accounts_file::{self, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::setup::Setup;
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::CallFloor;
use ui_sandbox::{Sandbox, SandboxResult};

use super::own_server_binary::{self, BinaryServer, PASSWORD};
use super::{ASK, EVIDENCE_ENV, Qa, READY_FRAMES, after_frames, answer, ensure, say};

/// The child's mode.
pub const MODE: &str = "own";
const ORIGIN_ENV: &str = "NGTEAMS_APPS_QA_ORIGIN";
const CA_ENV: &str = "NGTEAMS_APPS_QA_CA";
const DIR_ENV: &str = "NGTEAMS_APPS_QA_DIR";
const UPN: &str = "dana@own.example.test";
const NAME: &str = "Dana Made-Up";
/// The synthetic app's bot endpoint: nothing is sent there in this run.
const ENDPOINT: &str = "https://bots.example.test/api/messages";

type Outcome = (Vec<&'static str>, Vec<(&'static str, String)>);

/// The own server's checks; none when there is no server binary.
pub fn run(sandbox: &Sandbox) -> SandboxResult<Outcome> {
    let Some(binary) = own_server_binary::binary()? else {
        say("skipped own-server checks: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok((Vec::new(), Vec::new()));
    };
    let server = BinaryServer::start(&binary)?;
    server.add_user(UPN, NAME)?;
    let added = server.add_app(
        "Made-up Notes (synthetic)",
        ENDPOINT,
        "A made-up app an admin registered",
    )?;
    say(&format!("own server: {}", added.lines().next().unwrap_or_default()));
    let dir = tempfile::tempdir()?;
    let ca = server.ca_file.to_string_lossy().into_owned();
    let data = dir.path().to_string_lossy().into_owned();
    let mut qa = Qa::start(
        sandbox,
        MODE,
        &[(ORIGIN_ENV, &server.origin), (CA_ENV, &ca), (DIR_ENV, &data)],
    )?;
    qa.check("own_server_store_lists_the_registered_app", store_lists_the_app);
    let outcome = (qa.passed.clone(), qa.failed.clone());
    qa.quit()?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::write(PathBuf::from(evidence).join("own-server.log"), server.log());
    }
    Ok(outcome)
}

fn store_lists_the_app(qa: &mut Qa<'_>) -> SandboxResult {
    qa.state_until("the window drawn", |state| state["main"].is_string())?;
    thread::sleep(super::ANSWERED);
    qa.shot("own-window")?;
    qa.act("store")?;
    let state = qa.state_until("the own server's store read", |state| {
        state["main"] == "Apps" && state["store"]["loaded"] == true
    })?;
    qa.shots("own-store")?;
    let store = &state["store"];
    let org = store["org"].as_array().cloned().unwrap_or_default();
    ensure(org.len() == 1 && store["found"].as_array() == Some(&org), || {
        format!("the own server's store does not list exactly the registered app: {store}")
    })?;
    ensure(store["mine"].as_array().is_some_and(Vec::is_empty), || {
        format!("an app nobody added shows as added: {store}")
    })
}

// --- the child: the app on the own server ---

fn env(name: &str) -> SandboxResult<String> {
    std::env::var(name).map_err(|_| format!("{name} unset").into())
}

pub fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let sandbox_dir = PathBuf::from(std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let paths = PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = Setup::new(paths)?;
    let server = accounts_file::OwnServer::parse(&env(ORIGIN_ENV)?, &env(CA_ENV)?)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        UPN,
        &server,
        &Secret::new(PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, None)?;
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    let runner = super::support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
                answer(launched.desktop, None, sandbox_dir.join(ASK), cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    drop(runtime);
    Ok(())
}
