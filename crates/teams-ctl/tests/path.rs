//! Each mode's socket path, the override, and the private directory it is bound in.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]
#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use teams_ctl::path::{ENV_SOCKET, Env, Mode, PathError, SOCKET_NAME, prepare_dir, socket_path};

#[expect(
    clippy::unnecessary_wraps,
    reason = "the value of an `Env` field, beside its `None`s"
)]
fn os(text: &str) -> Option<OsString> {
    Some(OsString::from(text))
}

fn env() -> Env {
    Env {
        socket: None,
        runtime_dir: os("/run/user/1000"),
        state_home: os("/home/u/.state"),
        home: os("/home/u"),
    }
}

fn lab(alias: &str) -> Mode {
    Mode::Lab(alias.to_owned())
}

#[test]
fn each_mode_has_its_own_path() {
    let env = env();
    assert_eq!(
        env.socket_path(&Mode::Personal).unwrap(),
        PathBuf::from("/run/user/1000/ngteams/ctl.sock")
    );
    assert_eq!(
        env.socket_path(&lab("a")).unwrap(),
        PathBuf::from("/home/u/.state/ngteams-lab/a/ctl.sock")
    );
    assert_eq!(
        env.socket_path(&lab("lab_b-2")).unwrap(),
        PathBuf::from("/home/u/.state/ngteams-lab/lab_b-2/ctl.sock")
    );
    assert_eq!(
        env.socket_path(&Mode::ReadOnly).unwrap(),
        PathBuf::from("/home/u/.state/ngteams-read-only/run/ctl.sock")
    );
    assert_eq!(SOCKET_NAME, "ctl.sock");
}

#[test]
fn the_state_home_falls_back_to_home_as_the_runners_do() {
    for state_home in [None, os(""), os("relative/state")] {
        let env = Env { state_home, ..env() };
        assert_eq!(
            env.socket_path(&lab("c")).unwrap(),
            PathBuf::from("/home/u/.local/state/ngteams-lab/c/ctl.sock")
        );
        assert_eq!(
            env.socket_path(&Mode::ReadOnly).unwrap(),
            PathBuf::from("/home/u/.local/state/ngteams-read-only/run/ctl.sock")
        );
    }
    let homeless = Env {
        state_home: None,
        home: os("relative"),
        ..env()
    };
    assert_eq!(homeless.socket_path(&lab("a")), Err(PathError::NoStateHome));
    assert_eq!(homeless.socket_path(&Mode::ReadOnly), Err(PathError::NoStateHome));
    // the personal path needs no state home
    assert!(homeless.socket_path(&Mode::Personal).is_ok());
}

#[test]
fn the_personal_path_needs_an_absolute_runtime_dir() {
    for runtime_dir in [None, os(""), os("run/user")] {
        let env = Env { runtime_dir, ..env() };
        assert_eq!(env.socket_path(&Mode::Personal), Err(PathError::NoRuntimeDir));
        assert!(env.socket_path(&lab("a")).is_ok());
    }
}

#[test]
fn a_lab_alias_is_a_plain_name() {
    for alias in ["", "..", "a/b", "a b", "ä", "a.b"] {
        assert_eq!(
            env().socket_path(&lab(alias)),
            Err(PathError::BadAlias(alias.to_owned())),
            "{alias:?}"
        );
    }
}

#[test]
fn the_override_wins_for_a_client_but_not_for_the_modes_own_path() {
    let env = Env {
        socket: os("/tmp/elsewhere.sock"),
        runtime_dir: None,
        ..env()
    };
    for mode in [Mode::Personal, lab("a"), Mode::ReadOnly, lab("../bad")] {
        assert_eq!(env.socket_path(&mode).unwrap(), PathBuf::from("/tmp/elsewhere.sock"));
    }
    assert_eq!(
        env.mode_path(&lab("a")).unwrap(),
        PathBuf::from("/home/u/.state/ngteams-lab/a/ctl.sock")
    );
    assert_eq!(env.mode_path(&Mode::Personal), Err(PathError::NoRuntimeDir));

    let empty = Env {
        socket: os(""),
        ..self::env()
    };
    assert_eq!(
        empty.socket_path(&Mode::Personal).unwrap(),
        PathBuf::from("/run/user/1000/ngteams/ctl.sock")
    );
}

#[test]
fn the_process_environment_is_read_as_its_variables_say() {
    let env = Env::from_process();
    assert_eq!(env.socket, std::env::var_os(ENV_SOCKET));
    assert_eq!(env.home, std::env::var_os("HOME"));
    #[cfg(not(target_os = "macos"))]
    {
        assert_eq!(env.runtime_dir, std::env::var_os("XDG_RUNTIME_DIR"));
        assert_eq!(env.state_home, std::env::var_os("XDG_STATE_HOME"));
    }
    // macOS: the per-user temporary dir and Application Support
    #[cfg(target_os = "macos")]
    {
        assert_eq!(env.runtime_dir, std::env::var_os("TMPDIR"));
        let home = std::path::Path::new(env.home.as_ref().unwrap());
        assert_eq!(
            env.state_home,
            Some(home.join("Library/Application Support").into_os_string())
        );
    }
    assert_eq!(socket_path(&Mode::ReadOnly), env.socket_path(&Mode::ReadOnly));
}

#[test]
fn prepare_dir_makes_each_missing_directory_private() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("one/two/ctl.sock");
    prepare_dir(&socket).unwrap();
    for dir in ["one", "one/two"] {
        let mode = fs::metadata(root.path().join(dir)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{dir}");
    }
    // again: there already, still fine
    prepare_dir(&socket).unwrap();
}

#[test]
fn prepare_dir_leaves_a_directory_already_there_as_it_is() {
    let root = tempfile::tempdir().unwrap();
    let shared = root.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    prepare_dir(&shared.join("ctl.sock")).unwrap();
    assert_eq!(fs::metadata(&shared).unwrap().permissions().mode() & 0o777, 0o755);
}

#[test]
fn prepare_dir_refuses_a_file_where_the_directory_goes() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    fs::write(&file, b"").unwrap();
    assert!(prepare_dir(&file.join("ctl.sock")).is_err());
}

#[test]
fn prepare_dir_of_a_bare_name_is_the_current_directory() {
    prepare_dir(std::path::Path::new("ctl.sock")).unwrap();
}
