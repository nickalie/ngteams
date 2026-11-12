//! Lab paths, command-line modes and build markers with synthetic paths and no real services.

#![cfg(test)]

use std::ffi::OsStr;
#[cfg(unix)]
use std::path::Path;

use clap::Parser as _;
use ngteams::accounts_file::{PersonalLocations, PersonalPaths};
use ngteams::cli::{Cli, Command, LabMode};
use ngteams::lab;
use ngteams::refusal::build_refusal;

fn parse(args: &[&str]) -> Cli {
    Cli::try_parse_from(std::iter::once("ngteams").chain(args.iter().copied())).unwrap()
}

#[cfg(unix)]
#[test]
fn lab_files_follow_the_xdg_data_home_and_use_one_alias_directory() {
    let mine = PersonalLocations::resolve(None, None, Some("/synthetic/home".into())).unwrap();
    let paths = lab::paths_for("a", &mine).unwrap();
    let root = Path::new("/synthetic/home/.local/share/ngteams-lab/a");
    assert_eq!(paths.data_dir(), root);
    assert_eq!(paths.config_dir(), root);
    assert_eq!(paths.accounts_file(), root.join("accounts.toml"));
    assert_eq!(lab::LAB_KEYRING_APPLICATION, "ngteams-lab");
    let mine = PersonalLocations::resolve(Some("/c".into()), Some("/d".into()), None).unwrap();
    assert_eq!(
        lab::paths_for("b_2-test", &mine).unwrap().data_dir(),
        Path::new("/d/ngteams-lab/b_2-test")
    );
    assert_ne!(lab::paths_for("b", &mine).unwrap(), lab::paths_for("c", &mine).unwrap());
}

#[cfg(windows)]
#[test]
fn lab_files_use_one_alias_beside_the_personal_local_appdata_directory() {
    let dir = tempfile::tempdir().unwrap();
    let mine = PersonalLocations::windows(Some(dir.path().into()), None).unwrap();
    let paths = lab::paths_for("a", &mine).unwrap();
    let root = dir.path().join("ngteams-lab/a");
    assert_eq!(paths.data_dir(), root);
    assert_eq!(paths.config_dir(), root);
    assert_eq!(paths.accounts_file(), root.join("accounts.toml"));
    assert_eq!(lab::LAB_KEYRING_APPLICATION, "ngteams-lab");
    assert_ne!(lab::paths_for("b", &mine).unwrap(), lab::paths_for("c", &mine).unwrap());
}

#[test]
fn aliases_cannot_escape_the_alias_directory() {
    for bad in ["", ".", "..", "../a", "a/b", "a\\b", "/a", "a b", "a@b", "a\n"] {
        assert!(lab::alias(bad).is_err(), "{bad:?}");
    }
    for good in ["a", "B", "a-2", "lab_bot"] {
        assert_eq!(lab::alias(good).unwrap(), good);
    }
}

#[test]
fn an_unreadable_lab_account_file_is_reported_without_replacing_its_contents() {
    let dir = tempfile::tempdir().unwrap();
    let paths = PersonalPaths::new(dir.path().to_owned(), dir.path().to_owned());
    let invalid = "[[accounts\nsynthetic invalid account file";
    std::fs::write(paths.accounts_file(), invalid).unwrap();
    assert!(lab::account(&paths).is_err());
    assert_eq!(std::fs::read_to_string(paths.accounts_file()).unwrap(), invalid);
}

#[test]
fn lab_paths_require_a_resolvable_data_home_parent() {
    let mine = PersonalLocations {
        config_dir: "/synthetic/config/ngteams".into(),
        data_dir: "/".into(),
    };
    assert!(lab::paths_for("a", &mine).unwrap_err().contains("XDG data home"));
}

#[test]
fn lab_refuses_overlapping_personal_config_or_data_even_through_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let mine = PersonalPaths::new(dir.path().join("config/ngteams"), dir.path().join("data/ngteams"));
    #[cfg(windows)]
    let mine = PersonalPaths::new(dir.path().join("ngteams/config"), dir.path().join("ngteams/data"));
    std::fs::create_dir_all(mine.config_dir()).unwrap();
    std::fs::create_dir_all(mine.data_dir()).unwrap();
    for own in [mine.config_dir(), mine.data_dir()] {
        for overlap in [
            own.to_path_buf(),
            own.join("child"),
            own.parent().unwrap().to_path_buf(),
        ] {
            let lab = PersonalPaths::new(dir.path().join("elsewhere"), overlap.clone());
            assert!(lab::check_apart(&lab, &mine).is_err());
            let lab = PersonalPaths::new(overlap, dir.path().join("elsewhere"));
            assert!(lab::check_apart(&lab, &mine).is_err());
        }
    }
    #[cfg(unix)]
    let link = dir.path().join("data/ngteams-lab");
    #[cfg(windows)]
    let link = dir.path().join("ngteams-lab");
    #[cfg(unix)]
    std::os::unix::fs::symlink(mine.data_dir(), &link).unwrap();
    #[cfg(windows)]
    {
        // A junction exercises native reparse-point resolution without requiring symlink privileges or a policy change.
        let result = std::process::Command::new("powershell.exe").env_remove("PSModulePath")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:NGTEAMS_LINK -Target $env:NGTEAMS_TARGET | Out-Null"])
            .env("NGTEAMS_LINK", &link).env("NGTEAMS_TARGET", mine.data_dir()).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    }
    let locations = PersonalLocations {
        config_dir: mine.config_dir().to_owned(),
        data_dir: mine.data_dir().to_owned(),
    };
    assert!(lab::paths_for("a", &locations).unwrap_err().contains("overlaps"));
    let apart = PersonalPaths::new(dir.path().join("other/config"), dir.path().join("other/data"));
    lab::check_apart(&apart, &mine).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn a_lab_window_requires_the_sandbox_without_x11_fallback() {
    let socket = OsStr::new("/synthetic/sandbox/wayland-1");
    let sandbox = OsStr::new("/synthetic/sandbox");
    lab::window_allowed(Some(socket), Some(sandbox), None).unwrap();
    for (wayland, root, display) in [
        (None, Some(sandbox), None),
        (Some(OsStr::new("wayland-1")), Some(sandbox), None),
        (Some(OsStr::new("/desktop/wayland-1")), Some(sandbox), None),
        (Some(socket), None, None),
        (Some(socket), Some(sandbox), Some(OsStr::new(":0"))),
    ] {
        let error = lab::window_allowed(wayland, root, display).unwrap_err();
        assert!(error.starts_with("lab:"), "{error}");
        assert!(error.contains("scripts/run-lab.sh"));
    }
}

#[test]
fn the_lab_cli_requires_its_broker_and_rejects_snapshot_and_read_only_modes() {
    let cli = parse(&["--lab", "a", "--secrets-fd", "3", "--trace-fd", "4"]);
    assert!(cli.lab_requested());
    assert_eq!(
        cli.lab_mode().unwrap(),
        Some(LabMode {
            alias: "a".into(),
            secrets_fd: 3,
            trace_fd: Some(4),
            ctl_socket: None,
        })
    );
    // the control socket only where its runner names it
    let told = parse(&[
        "--lab",
        "a",
        "--secrets-fd",
        "3",
        "--ctl-socket",
        "/state/ngteams-lab/a/ctl.sock",
    ]);
    assert_eq!(
        told.lab_mode().unwrap().unwrap().ctl_socket,
        Some("/state/ngteams-lab/a/ctl.sock".into())
    );
    assert_eq!(cli.read_only_mode(None).unwrap(), None);
    assert!(cli.read_only_mode(Some(OsStr::new("1"))).is_err());
    assert!(parse(&["--lab", "a"]).lab_mode().is_err());
    assert!(
        parse(&["--lab", "a", "--data", "/snapshot", "--secrets-fd", "3"])
            .lab_mode()
            .is_err()
    );
    assert!(
        parse(&["--lab", "a", "--secrets-fd", "3", "--trace-fd", "3"])
            .lab_mode()
            .is_err()
    );
    assert!(Cli::try_parse_from(["ngteams", "--lab", "a", "--read-only"]).is_err());
    assert!(Cli::try_parse_from(["ngteams", "--lab", "../a"]).is_err());
    let ordinary = parse(&[]);
    assert_eq!(ordinary.lab_mode().unwrap(), None);
    assert!(!ordinary.lab_requested());
}

#[test]
fn broker_cli_requires_exactly_one_personal_account_or_lab_alias() {
    let lab = parse(&["secrets", "--lab", "a", "--fd", "3"]);
    assert!(lab.lab_requested());
    assert!(matches!(lab.command, Some(Command::Secrets { account: None, lab: Some(alias), fd: 3 }) if alias == "a"));
    let personal = parse(&["secrets", "--account", "someone@example.test", "--fd", "3"]);
    assert!(!personal.lab_requested());
    assert!(matches!(
        personal.command,
        Some(Command::Secrets {
            account: Some(_),
            lab: None,
            fd: 3
        })
    ));
    for args in [
        vec!["ngteams", "secrets", "--fd", "3"],
        vec![
            "ngteams",
            "secrets",
            "--lab",
            "a",
            "--account",
            "someone@example.test",
            "--fd",
            "3",
        ],
        vec!["ngteams", "secrets", "--lab", "../a", "--fd", "3"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn the_build_marker_matrix_keeps_lab_builds_in_lab_and_all_test_support_builds_out() {
    for installed in [None, Some(""), Some("1")] {
        for lab_build in [None, Some(""), Some("1")] {
            for lab in [false, true] {
                assert!(
                    build_refusal(true, installed, lab_build, lab)
                        .unwrap()
                        .contains("test-support")
                );
                let expected = if lab_build == Some("1") {
                    lab
                } else {
                    installed == Some("1")
                };
                assert_eq!(build_refusal(false, installed, lab_build, lab).is_none(), expected);
            }
        }
    }
    assert!(
        build_refusal(false, None, Some("1"), false)
            .unwrap()
            .contains("only with --lab")
    );
    assert!(
        build_refusal(false, Some("1"), None, true).is_none(),
        "installed builds may run --lab too"
    );
}
