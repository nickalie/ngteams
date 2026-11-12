//! Starting the actual runner with an in-memory credential store and synthetic account metadata.

use super::*;
use teams_auth::secrets::MemorySecretStore;

#[test]
fn the_runner_holds_one_engine_per_account_and_releases_it_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = super::super::app::tests::setup(dir.path(), 1);
    let runner = Runner {
        setup,
        floor: CallFloor::new(),
        devices: Some(crate::calling::CallDevices::open(teams_media::devices::Reach::Sandbox)),
    };
    let started = runner.start_on(&account, MemorySecretStore::new()).unwrap();
    assert!(format!("{started:?}").contains(&account.upn));
    assert!(started.calls.as_ref().unwrap().video.is_some());
    let error = runner.start_on(&account, MemorySecretStore::new()).unwrap_err();
    assert!(
        error.message.contains(&account.upn) && error.message.contains("another engine runs"),
        "{error}"
    );
    assert!(error.running, "another engine holds the account");
    started.engine.handle.shutdown();
    runner
        .start_on(&account, MemorySecretStore::new())
        .unwrap()
        .engine
        .handle
        .shutdown();
}
