//! Account-file parsing, persistence, permissions and legacy migration, using synthetic identities.

use std::fs;
use teams_app::accounts::{PersonalAccount, PersonalError, PersonalPaths};
#[path = "../../teams-platform/tests/support/privacy.rs"]
mod privacy;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const TENANT: &str = "00000000-0000-4000-8000-000000000001";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GUID_Y: &str = "00000000-0000-4000-8000-0000000000fe";
fn paths_in(dir: &tempfile::TempDir) -> PersonalPaths {
    PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("share/ngteams"))
}
/// A made-up account `upn` a sign-in named, with the object id `oid`.
fn signed_in(upn: &str, oid: &str) -> TestResult<PersonalAccount> {
    Ok(PersonalAccount::new(upn, oid, TENANT)?)
}

/// The UPNs of `paths`' accounts, in their order.
fn upns(paths: &PersonalPaths) -> TestResult<Vec<String>> {
    Ok(PersonalAccount::load_all(paths)?
        .into_iter()
        .map(|account| account.upn)
        .collect())
}

#[test]
fn accounts_toml_keeps_the_accounts_in_the_order_added_and_saves_by_upn() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    assert!(PersonalAccount::load_all(&paths)?.is_empty(), "none before the first");
    PersonalAccount::pending("one@example.test")?.save(&paths)?;
    signed_in("two@example.test", GUID_X)?.save(&paths)?;
    PersonalAccount::pending("three@example.test")?.save(&paths)?;
    assert_eq!(
        upns(&paths)?,
        ["one@example.test", "two@example.test", "three@example.test"]
    );
    // a sign-in of the first, its UPN in another case: saved in its place
    let one = signed_in("ONE@example.test", GUID_Y)?;
    one.save(&paths)?;
    assert_eq!(
        upns(&paths)?,
        ["ONE@example.test", "two@example.test", "three@example.test"]
    );
    assert_eq!(PersonalAccount::find(&paths, "one@EXAMPLE.test")?, Some(one));
    assert_eq!(PersonalAccount::find(&paths, "four@example.test")?, None);
    // another account with an object id one has already: refused, nothing written
    let before = fs::read_to_string(paths.accounts_file())?;
    assert!(matches!(
        signed_in("four@example.test", GUID_X)?.save(&paths),
        Err(PersonalError::Account(_))
    ));
    assert_eq!(fs::read_to_string(paths.accounts_file())?, before);
    // removed by UPN (case aside), the others kept in their order
    let removed = PersonalAccount::remove(&paths, "Two@Example.Test")?;
    assert_eq!(removed.map(|account| account.oid), Some(Some(GUID_X.to_owned())));
    assert_eq!(upns(&paths)?, ["ONE@example.test", "three@example.test"]);
    assert_eq!(PersonalAccount::remove(&paths, "two@example.test")?, None);
    // the file and its directory are the user's alone, and no staging file is left
    privacy::assert_private(&paths.accounts_file(), false);
    privacy::assert_private(paths.config_dir(), false);
    let names: Vec<_> = fs::read_dir(paths.config_dir())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, ["accounts.toml"]);
    Ok(())
}

#[test]
fn accounts_toml_refuses_an_account_given_twice_or_one_that_isn_t_valid() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    fs::create_dir_all(paths.config_dir())?;
    let table = |upn: &str, oid: &str| format!("[[account]]\nupn = \"{upn}\"\noid = \"{oid}\"\ntid = \"{TENANT}\"\n");
    let good = format!(
        "{}\n{}",
        table("one@example.test", GUID_X),
        table("two@example.test", GUID_Y)
    );
    fs::write(paths.accounts_file(), &good)?;
    assert_eq!(upns(&paths)?, ["one@example.test", "two@example.test"]);
    for bad in [
        // the same UPN, case aside
        format!(
            "{}\n{}",
            table("one@example.test", GUID_X),
            table("ONE@example.test", GUID_Y)
        ),
        // the same object id, case aside
        format!(
            "{}\n{}",
            table("one@example.test", GUID_X),
            table("two@example.test", &GUID_X.to_ascii_uppercase())
        ),
        // an account that isn't valid, an unknown key in one, an unknown table
        format!("{good}\n{}", table("three", GUID_C)),
        format!("{good}token = \"x\"\n"),
        format!("{good}\n[settings]\nx = 1\n"),
    ] {
        fs::write(paths.accounts_file(), &bad)?;
        assert!(
            matches!(PersonalAccount::load_all(&paths), Err(PersonalError::Account(_))),
            "{bad}"
        );
        // nothing is saved over a file that doesn't read
        assert!(PersonalAccount::pending("four@example.test")?.save(&paths).is_err());
        assert_eq!(fs::read_to_string(paths.accounts_file())?, bad);
    }
    // a file of no account is none
    fs::write(paths.accounts_file(), "# no account yet\n")?;
    assert!(PersonalAccount::load_all(&paths)?.is_empty());
    Ok(())
}

#[test]
fn a_single_account_s_files_are_read_until_moved_to_the_files_of_several() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    paths.make_dirs()?;
    let legacy =
        format!("upn = \"someone@example.test\"\noid = \"{GUID_X}\"\ntid = \"{TENANT}\"\nname = \"Made-Up\"\n");
    fs::write(paths.legacy_account_file(), &legacy)?;
    fs::write(paths.legacy_region_file(), "{\"region\":\"amer\"}")?;
    let account = PersonalAccount::parse(&legacy)?;
    // not migrated yet: the one account is read from account.toml
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![account.clone()]);
    assert_eq!(
        PersonalAccount::find(&paths, "SOMEONE@example.test")?,
        Some(account.clone())
    );
    paths.migrate_single_account()?;
    assert!(!paths.legacy_account_file().exists());
    assert!(!paths.legacy_region_file().exists());
    let region = paths
        .region_file(&account)
        .ok_or("a signed-in account has a region file")?;
    assert_eq!(region, paths.data_dir().join("regions").join(format!("{GUID_X}.json")));
    assert_eq!(fs::read_to_string(&region)?, "{\"region\":\"amer\"}");
    privacy::assert_private(region.parent().ok_or("dir")?, false);
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![account.clone()]);
    privacy::assert_private(&paths.accounts_file(), false);
    // a second time: nothing to do
    let migrated = fs::read_to_string(paths.accounts_file())?;
    paths.migrate_single_account()?;
    assert_eq!(fs::read_to_string(paths.accounts_file())?, migrated);
    // an account.toml beside an accounts.toml (an older build run since) is left alone
    fs::write(paths.legacy_account_file(), "upn = \"other@example.test\"\n")?;
    fs::write(paths.legacy_region_file(), "{}")?;
    paths.migrate_single_account()?;
    assert!(paths.legacy_account_file().exists() && paths.legacy_region_file().exists());
    assert_eq!(fs::read_to_string(paths.accounts_file())?, migrated);
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![account]);
    Ok(())
}

#[test]
fn a_single_account_no_sign_in_named_moves_without_its_region() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    // nothing to move: no file is made
    paths.migrate_single_account()?;
    assert!(!paths.accounts_file().exists());
    paths.make_dirs()?;
    fs::write(paths.legacy_account_file(), "upn = \"someone@example.test\"\n")?;
    fs::write(paths.legacy_region_file(), "{}")?;
    paths.migrate_single_account()?;
    let pending = PersonalAccount::pending("someone@example.test")?;
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![pending.clone()]);
    assert_eq!(paths.region_file(&pending), None);
    assert!(!paths.legacy_account_file().exists());
    assert!(
        paths.legacy_region_file().exists(),
        "the next sign-in writes its region"
    );
    // an account.toml that doesn't read isn't moved
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    paths.make_dirs()?;
    fs::write(paths.legacy_account_file(), "upn = \"someone\"\n")?;
    assert!(paths.migrate_single_account().is_err());
    assert!(paths.legacy_account_file().exists() && !paths.accounts_file().exists());
    Ok(())
}

#[test]
fn account_toml_is_strict() {
    let good = format!("upn = \"someone@example.test\"\noid = \"{GUID_X}\"\ntid = \"{TENANT}\"\n");
    assert!(PersonalAccount::parse(&good).is_ok());
    let upper = good.replace(GUID_X, &GUID_X.to_ascii_uppercase());
    assert_eq!(
        PersonalAccount::parse(&upper).map(|account| account.oid).ok(),
        Some(Some(GUID_X.to_owned()))
    );
    // before the first sign-in: the UPN alone
    let pending = PersonalAccount::parse("upn = \"someone@example.test\"\n").unwrap();
    assert!(!pending.is_signed_in() && pending.mri().is_none());
    for bad in [
        good.replace("someone@example.test", "someone"),
        good.replace(GUID_X, "not-a-guid"),
        good.replace(&format!("tid = \"{TENANT}\"\n"), ""),
        format!("{good}token = \"x\"\n"),
        format!("{good}idp_hosts = [\"Bad Host\"]\n"),
        format!("{good}idp_hosts = [\"localhost\"]\n"),
    ] {
        assert!(PersonalAccount::parse(&bad).is_err(), "{bad}");
    }
    for bad_name in ["", " padded", "line\\nbreak"] {
        let bad = format!("{good}name = \"{bad_name}\"\n");
        assert!(PersonalAccount::parse(&bad).is_err(), "{bad}");
    }
    let hosts = format!("{good}idp_hosts = [\"sso.example.test\"]\n");
    assert_eq!(
        PersonalAccount::parse(&hosts).map(|account| account.idp_hosts).ok(),
        Some(vec!["sso.example.test".to_owned()])
    );
}

#[test]
fn a_sign_in_s_display_name_is_kept_in_accounts_toml() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    let pending = PersonalAccount::pending("someone@example.test")?;
    let named = pending.signed_in_as(GUID_X, TENANT, Some("Made-Up Person"))?;
    assert_eq!(named.name.as_deref(), Some("Made-Up Person"));
    named.save(&paths)?;
    assert_eq!(PersonalAccount::load_all(&paths)?, vec![named.clone()]);
    // a later sign-in without one, or with one a post can't carry, keeps it
    let again = named.signed_in_as(GUID_X, TENANT, None)?;
    assert_eq!(again.name.as_deref(), Some("Made-Up Person"));
    let again = named.signed_in_as(GUID_X, TENANT, Some("bad\u{7}name"))?;
    assert_eq!(again.name.as_deref(), Some("Made-Up Person"));
    // the store is the account's own
    assert_eq!(
        paths.store_file(&named),
        Some(paths.data_dir().join("store").join(format!("{GUID_X}.sqlite")))
    );
    assert_eq!(paths.store_file(&pending), None);
    Ok(())
}

#[test]
fn an_unwritable_config_reports_the_target_and_preserves_published_accounts() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    PersonalAccount::pending("first@example.test")?.save(&paths)?;
    let before = fs::read(paths.accounts_file())?;
    #[cfg(unix)]
    let restore = {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(paths.config_dir())?.permissions();
        fs::set_permissions(paths.config_dir(), fs::Permissions::from_mode(0o555))?;
        || fs::set_permissions(paths.config_dir(), mode)
    };
    // Windows' read-only directory flag does not deny file creation. A reader without delete sharing prevents
    // atomic replacement instead, exercising the native publication failure and preservation contract.
    #[cfg(windows)]
    let restore = {
        use std::os::windows::fs::OpenOptionsExt as _;
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(paths.accounts_file())?;
        move || {
            drop(held);
            Ok::<_, std::io::Error>(())
        }
    };
    let result = PersonalAccount::pending("second@example.test")?.save(&paths);
    let after = fs::read(paths.accounts_file());
    // Restore before assertions, including on the expected refusal, so the private fixture cleans up normally.
    restore()?;
    assert!(matches!(result, Err(PersonalError::Write { path, .. }) if path == paths.accounts_file()));
    assert_eq!(after?, before);
    assert_eq!(upns(&paths)?, ["first@example.test"]);
    Ok(())
}

#[test]
fn account_file_obstructions_are_read_errors_instead_of_empty_account_lists() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    fs::create_dir_all(paths.accounts_file())?;
    assert!(
        matches!(PersonalAccount::load_all(&paths), Err(PersonalError::Read { path, .. }) if path == paths.accounts_file())
    );
    fs::remove_dir(paths.accounts_file())?;
    fs::create_dir(paths.legacy_account_file())?;
    assert!(
        matches!(PersonalAccount::load_all(&paths), Err(PersonalError::Read { path, .. }) if path == paths.legacy_account_file())
    );
    Ok(())
}

const SERVER: &str = "https://chat.example.test:8443";
const GUID_S: &str = "00000000-0000-4000-8000-0000000000a1";
const GUID_T: &str = "00000000-0000-4000-8000-0000000000a2";

#[test]
fn an_own_servers_account_sits_beside_the_microsoft_one_of_the_same_upn() -> TestResult {
    use teams_app::accounts::OwnServer;
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    let upn = "someone@example.test";
    // an entry written before own servers reads as Microsoft 365, and saving beside it leaves its lines as they were
    let old = format!("[[account]]\nupn = \"{upn}\"\noid = \"{GUID_X}\"\ntid = \"{TENANT}\"\nidp_hosts = []\n");
    fs::create_dir_all(paths.config_dir())?;
    fs::write(paths.accounts_file(), &old)?;
    let microsoft = PersonalAccount::find(&paths, upn)?.ok_or("the old entry")?;
    assert_eq!((microsoft.server.as_deref(), microsoft.id().as_str()), (None, upn));
    assert!(microsoft.cloud()?.is_microsoft());

    let server = OwnServer::parse(" chat.example.test:8443/ ", "")?;
    assert_eq!(server.origin, SERVER);
    let own = PersonalAccount::pending_on(upn, &server)?.signed_in_as(GUID_S, TENANT, Some("Someone"))?;
    own.save(&paths)?;
    let other = OwnServer::parse("https://other.example.test", "")?;
    PersonalAccount::pending_on(upn, &other)?
        .signed_in_as(GUID_T, TENANT, None)?
        .save(&paths)?;
    let text = fs::read_to_string(paths.accounts_file())?;
    assert!(text.contains(&old), "{text}");
    assert!(text.contains(&format!("server = \"{SERVER}\"\n")), "{text}");

    let accounts = PersonalAccount::load_all(&paths)?;
    assert_eq!(accounts.len(), 3);
    assert_eq!(own.id(), format!("{SERVER}/{upn}"));
    assert_eq!(
        own.cloud()?.origin().map(ToString::to_string).as_deref(),
        Some("https://chat.example.test:8443/")
    );
    assert_eq!(own.server_host().as_deref(), Some("chat.example.test:8443"));
    // a bare UPN names the Microsoft account; an own server's is named by its id, case aside
    assert_eq!(PersonalAccount::find(&paths, upn)?, Some(microsoft));
    assert_eq!(
        PersonalAccount::find(&paths, &own.id().to_uppercase())?,
        Some(own.clone())
    );
    kept_apart_when_saved_and_removed(&paths, &own, &text)
}

/// The rest of the test above: `own` saved again, an account given twice refused, and a bare UPN's meaning as
/// accounts are removed.
fn kept_apart_when_saved_and_removed(paths: &PersonalPaths, own: &PersonalAccount, text: &str) -> TestResult {
    let upn = own.upn.as_str();
    // saved again in its own place, never over the account of the same UPN elsewhere
    own.signed_in_as(GUID_S, TENANT, Some("Renamed"))?.save(paths)?;
    let names: Vec<_> = PersonalAccount::load_all(paths)?
        .into_iter()
        .map(|account| account.name)
        .collect();
    assert_eq!(names, [None, Some("Renamed".to_owned()), None]);
    // the same server and UPN twice is refused
    let twice = format!("{text}\n[[account]]\nupn = \"{upn}\"\nserver = \"{SERVER}\"\n");
    fs::write(paths.accounts_file(), twice)?;
    assert!(matches!(
        PersonalAccount::load_all(paths),
        Err(PersonalError::Account(_))
    ));
    // a server written with a trailing `/` is the same server: the same id, so the same entry and tokens
    let slashed = format!("{text}\n[[account]]\nupn = \"{upn}\"\nserver = \"{SERVER}/\"\n");
    fs::write(paths.accounts_file(), slashed)?;
    assert!(matches!(
        PersonalAccount::load_all(paths),
        Err(PersonalError::Account(_))
    ));
    let written = PersonalAccount::parse(&format!("upn = \"{upn}\"\nserver = \"{SERVER}/\"\n"))?;
    assert_eq!(written.server.as_deref(), Some(SERVER));
    assert!(written.is(own), "{}", written.id());
    fs::write(paths.accounts_file(), text)?;
    // without the Microsoft account, a bare UPN names an own server's only while one alone has it
    PersonalAccount::remove(paths, upn)?;
    assert_eq!(PersonalAccount::find(paths, upn)?, None);
    PersonalAccount::remove(paths, &format!("https://other.example.test/{upn}"))?;
    assert_eq!(
        PersonalAccount::find(paths, upn)?.map(|account| account.id()),
        Some(own.id())
    );
    Ok(())
}

#[test]
fn an_own_server_is_an_https_origin_with_an_absolute_ca_file() -> TestResult {
    use teams_app::accounts::OwnServer;
    let good = format!("upn = \"someone@example.test\"\nserver = \"{SERVER}\"\n");
    assert!(PersonalAccount::parse(&good).is_ok());
    for bad in [
        good.replace("https://", "http://"),
        good.replace(SERVER, "https://chat.example.test/path"),
        good.replace(SERVER, "https://user@chat.example.test"),
        format!("{good}ca_file = \"relative/ca.pem\"\n"),
        format!("{good}idp_hosts = [\"sso.example.test\"]\n"),
        "upn = \"someone@example.test\"\nca_file = \"/tmp/ca.pem\"\n".to_owned(),
    ] {
        assert!(PersonalAccount::parse(&bad).is_err(), "{bad}");
    }
    for bad in [
        "",
        "http://chat.example.test",
        "chat.example.test/path",
        "ftp://chat.example.test",
    ] {
        assert!(OwnServer::parse(bad, "").is_err(), "{bad}");
    }
    assert!(OwnServer::parse(SERVER, "ca.pem").is_err());

    // the CA file is read when the account's cloud is made: one missing or holding no certificate is said
    let dir = tempfile::tempdir()?;
    let ca = dir.path().join("ca.pem");
    let account = PersonalAccount::pending_on(
        "someone@example.test",
        &OwnServer::parse(SERVER, &ca.display().to_string())?,
    )?;
    assert!(matches!(account.cloud(), Err(PersonalError::Read { .. })));
    fs::write(&ca, "not a certificate\n")?;
    assert!(matches!(account.cloud(), Err(PersonalError::Account(_))));
    let authority = rcgen::generate_simple_self_signed(vec!["chat.example.test".to_owned()])?;
    fs::write(&ca, authority.cert.pem())?;
    assert!(account.cloud()?.extra_roots().is_some());
    let saved = tempfile::tempdir()?;
    let paths = paths_in(&saved);
    account.save(&paths)?;
    assert_eq!(PersonalAccount::load_all(&paths)?, [account]);
    Ok(())
}

#[test]
fn an_account_names_its_files_its_server_s_host_and_its_mri() -> TestResult {
    use teams_app::accounts::OwnServer;
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    assert_eq!(paths.settings_file(), paths.data_dir().join("settings.toml"));
    assert_eq!(paths.log_dir(), paths.data_dir().join("logs"));

    let microsoft = signed_in("someone@example.test", GUID_X)?;
    assert_eq!(
        microsoft.mri().map(|mri| mri.as_str().to_owned()),
        Some(format!("8:orgid:{GUID_X}"))
    );
    assert_eq!(microsoft.server_host(), None);
    assert!(PersonalAccount::pending("someone@example.test")?.mri().is_none());
    let shown = format!("{microsoft:?}");
    assert!(
        shown.contains("someone@example.test") && !shown.contains(GUID_X),
        "{shown}"
    );

    let own = PersonalAccount::pending_on("someone@example.test", &OwnServer::parse(SERVER, "")?)?;
    assert_eq!(own.server_host().as_deref(), Some("chat.example.test:8443"));
    let https_port = PersonalAccount::pending_on(
        "someone@example.test",
        &OwnServer::parse("https://chat.example.test", "")?,
    )?;
    assert_eq!(https_port.server_host().as_deref(), Some("chat.example.test"));

    let accounts = [microsoft.clone(), own.clone()];
    assert_eq!(
        PersonalAccount::named(&accounts, "SOMEONE@example.test"),
        Some(&microsoft)
    );
    assert_eq!(PersonalAccount::named(&accounts, &own.id()), Some(&own));
    assert_eq!(PersonalAccount::named(&accounts, "nobody@example.test"), None);
    Ok(())
}

#[test]
fn the_account_files_take_up_an_entry_held_and_leave_one_the_user_wrote_into() -> TestResult {
    use teams_app::accounts::{AccountFiles, OwnServer};
    let dir = tempfile::tempdir()?;
    let files = AccountFiles::new(paths_in(&dir));
    let pending = files.ask_for("someone@example.test")?;
    assert_eq!(files.ask_for("SOMEONE@example.test")?, pending);
    assert_eq!(files.as_held(&pending)?, pending);
    let elsewhere = PersonalAccount::pending("nobody@example.test")?;
    assert_eq!(
        files.as_held(&elsewhere)?,
        elsewhere,
        "an account the file doesn't hold is as given"
    );
    assert_eq!(files.pending_upns()?, ["someone@example.test"]);
    assert!(files.signed_in_accounts()?.is_empty());

    let ca = dir.path().join("ca.pem");
    let server = OwnServer::parse(SERVER, "")?;
    let own = files.ask_for_on("someone@example.test", &server)?;
    assert_eq!(files.ask_for_on("someone@example.test", &server)?, own);
    let with_ca = OwnServer::parse(SERVER, &ca.display().to_string())?;
    let taken_up = files.ask_for_on("someone@example.test", &with_ca)?;
    assert_eq!(
        taken_up.ca_file.as_deref(),
        Some(ca.as_path()),
        "a CA file given now replaces the entry's"
    );
    assert_eq!(
        files.pending_upns()?,
        ["someone@example.test"],
        "an own server's account isn't listed"
    );

    // the entry the user wrote more into stays; a bare one goes
    assert!(!files.abandon(&taken_up.id())?);
    assert!(files.abandon("someone@example.test")?);
    assert!(!files.abandon("nobody@example.test")?);
    assert_eq!(files.accounts()?, [taken_up]);
    Ok(())
}

#[test]
fn an_accounts_file_that_cant_be_parsed_fails_every_change_and_read() -> TestResult {
    use teams_app::accounts::{AccountFiles, OwnServer};
    let dir = tempfile::tempdir()?;
    let files = AccountFiles::new(paths_in(&dir));
    fs::create_dir_all(files.paths.config_dir())?;
    fs::write(files.paths.accounts_file(), "not = [valid\n")?;
    let account = PersonalAccount::pending("someone@example.test")?;
    let server = OwnServer::parse(SERVER, "")?;
    assert!(files.accounts().is_err());
    assert!(files.signed_in_accounts().is_err());
    assert!(files.pending_upns().is_err());
    assert!(files.ask_for("someone@example.test").is_err());
    assert!(files.ask_for_on("someone@example.test", &server).is_err());
    assert!(files.as_held(&account).is_err());
    assert!(files.abandon("someone@example.test").is_err());
    assert!(PersonalAccount::find(&files.paths, "someone@example.test").is_err());
    assert!(PersonalAccount::remove(&files.paths, "someone@example.test").is_err());
    assert!(account.save(&files.paths).is_err());
    Ok(())
}
