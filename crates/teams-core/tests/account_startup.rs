//! The concrete account assembler with an unsigned synthetic account: the empty in-memory secret store prevents
//! every credential acquisition before an HTTP request or socket can open. No user keyring is used.

#![cfg(test)]

use std::path::Path;
use std::sync::Arc;

use teams_auth::authz::{CHAT_BASE_GTM, Region};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::token::TokenError;
use teams_core::account::{self, Account, AccountCalls, AccountEngine, AccountError};
use teams_core::{EngineError, Relay};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_proto::{BuildError, IC3_AUDIENCE};

const MRI: &str = "8:orgid:00000000-0000-0000-0000-00000000000a";

fn region() -> Region {
    Region {
        name: Some("amer".into()),
        partition: Some("amer-03".into()),
        endpoints: [(
            CHAT_BASE_GTM.into(),
            "https://teams.microsoft.com/api/chatsvc/amer".into(),
        )]
        .into(),
    }
}

fn calls() -> AccountCalls {
    account::account_calls(
        Arc::new(FileDevices {
            input: AudioInput::Silence,
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        }),
        None,
    )
}

fn account(path: &Path) -> Account<MemorySecretStore> {
    Account {
        upn: "synthetic-unsigned@example.test".into(),
        mri: MRI.into(),
        display_name: "Synthetic Person".into(),
        store_file: path.into(),
        region: region(),
        secrets: MemorySecretStore::new(),
        push: false,
        known_names: vec![(MRI.into(), "Synthetic Person".into())],
        calls: None,
        read_only: false,
    }
}

fn start(account: Account<MemorySecretStore>) -> Result<AccountEngine<MemorySecretStore>, AccountError> {
    account::start(account, teams_core::CallFloor::new())
}

#[test]
fn account_call_configuration_keeps_the_web_identity_and_opens_no_file_devices() {
    let calls = calls();
    assert_eq!(calls.client, account::web_client_info());
    assert_eq!(calls.language_id.as_deref(), Some(account::CALL_LANGUAGE));
    assert_eq!(calls.media.relay, Relay::Teams);
    assert!(calls.media.video.is_none());
    let (push, suppressed) = account::listening(true, true, Some(calls.clone()));
    assert!(!push && suppressed.is_none());
    let (push, kept) = account::listening(false, true, Some(calls));
    assert!(push && kept.is_some());
}

#[test]
fn missing_chat_base_invalid_transport_and_unopenable_store_stop_account_assembly() {
    let dir = tempfile::tempdir().unwrap();
    let mut no_base = account(&dir.path().join("no-base.sqlite"));
    no_base.region.endpoints.clear();
    assert!(matches!(start(no_base), Err(AccountError::NoChatBase)));
    let mut bad_base = account(&dir.path().join("bad-base.sqlite"));
    bad_base
        .region
        .endpoints
        .insert(CHAT_BASE_GTM.into(), "http://127.0.0.1:9".into());
    assert!(matches!(
        start(bad_base),
        Err(AccountError::Transport(BuildError::ChatBase))
    ));
    let blocker = dir.path().join("blocked");
    std::fs::write(&blocker, "synthetic non-directory").unwrap();
    assert!(matches!(
        start(account(&blocker.join("account.sqlite"))),
        Err(AccountError::Store(_))
    ));
}

#[tokio::test]
async fn an_unsigned_account_assembles_optional_services_and_the_engine_lock_prevents_a_second_engine() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("account.sqlite");
    let mut configured = account(&path);
    configured.region.endpoints.extend([
        (
            account::MIDDLE_TIER_GTM.into(),
            "https://teams.microsoft.com/api/mt/part/amer-03".into(),
        ),
        (
            account::CSA_GTM.into(),
            "https://teams.microsoft.com/api/csa/amer".into(),
        ),
        (
            account::PRESENCE_GTM.into(),
            "https://teams.microsoft.com/ups/noam".into(),
        ),
        (
            account::AMS_GTM.into(),
            "https://amer-0.asyncgw.teams.microsoft.com".into(),
        ),
        (
            account::PROVISIONING_GTM.into(),
            "https://teams.microsoft.com/fabric/amer/templates/api".into(),
        ),
        (
            account::USER_PROFILE_GTM.into(),
            "https://teams.microsoft.com/api/userprofilesvc/amer".into(),
        ),
        (
            account::MCPS_GTM.into(),
            "https://teams.microsoft.com/api/mcps/amer".into(),
        ),
    ]);
    let engine = start(configured).unwrap();
    assert!(!engine.handle.requests().read_only());
    assert_eq!(
        engine.handle.person(MRI).await.unwrap().name.as_deref(),
        Some("Synthetic Person")
    );
    assert!(matches!(
        engine.session.access_token(IC3_AUDIENCE).await,
        Err(TokenError::NotSignedIn { .. })
    ));
    assert!(matches!(
        start(account(&path)),
        Err(AccountError::Engine(EngineError::AlreadyRunning { .. }))
    ));
    engine.handle.shutdown();
    let restarted = start(account(&path)).unwrap();
    restarted.handle.shutdown();
    assert!(path.is_file());
}

#[tokio::test]
async fn read_only_suppresses_requested_sockets_and_calling_and_invalid_optional_bases_leave_the_engine_usable() {
    for read_only in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let mut configured = account(&dir.path().join("account.sqlite"));
        configured.read_only = read_only;
        configured.push = true;
        configured.calls = Some(calls());
        for service in [
            account::MIDDLE_TIER_GTM,
            account::CSA_GTM,
            account::PRESENCE_GTM,
            account::AMS_GTM,
            account::PROVISIONING_GTM,
            account::USER_PROFILE_GTM,
            account::MCPS_GTM,
        ] {
            configured
                .region
                .endpoints
                .insert(service.into(), "invalid synthetic base".into());
        }
        let engine = start(configured).unwrap();
        assert_eq!(engine.handle.requests().read_only(), read_only);
        assert!(matches!(
            engine.session.access_token(IC3_AUDIENCE).await,
            Err(TokenError::NotSignedIn { .. })
        ));
        assert_eq!(
            engine.handle.person(MRI).await.unwrap().name.as_deref(),
            Some("Synthetic Person")
        );
        let description = format!("{engine:?}");
        assert!(!description.contains("refresh_token"));
        engine.handle.shutdown();
    }
}

#[tokio::test]
async fn own_account_assembly_accepts_its_bases_and_refuses_a_microsoft_chat_base() {
    let cloud = teams_proto::Cloud::own("https://chat.example:8443").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut configured = account(&dir.path().join("own.sqlite"));
    assert!(matches!(
        account::start_for_cloud(configured, teams_core::CallFloor::new(), &cloud),
        Err(AccountError::Transport(BuildError::ChatBase))
    ));
    configured = account(&dir.path().join("own.sqlite"));
    for (gtm, path) in [
        (CHAT_BASE_GTM, "/api/chatsvc/amer"),
        (account::MIDDLE_TIER_GTM, "/api/mt/part/amer-03"),
        (account::CSA_GTM, "/api/csa/amer"),
        (account::PRESENCE_GTM, "/ups/noam"),
        (account::AMS_GTM, "/ams"),
        (account::PROVISIONING_GTM, "/fabric/amer/templates/api"),
        (account::USER_PROFILE_GTM, "/api/userprofilesvc/amer"),
        (account::MCPS_GTM, "/api/mcps/amer"),
    ] {
        configured
            .region
            .endpoints
            .insert(gtm.into(), format!("https://chat.example:8443{path}"));
    }
    configured.push = true;
    configured.calls = Some(calls());
    let engine = account::start_for_cloud(configured, teams_core::CallFloor::new(), &cloud).unwrap();
    assert!(matches!(
        engine.session.access_token(IC3_AUDIENCE).await,
        Err(TokenError::NotSignedIn { .. })
    ));
    engine.handle.shutdown();
}
