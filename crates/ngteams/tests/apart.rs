//! Build, credential namespace and dependency boundaries of ngteams.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use toml::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn manifest(path: &Path) -> Value {
    Value::Table(toml::from_str(&fs::read_to_string(path).unwrap()).unwrap())
}

fn workspace() -> Value {
    manifest(&root().join("Cargo.toml"))
}

fn list(value: &Value, key: &str) -> Vec<String> {
    value["workspace"][key]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member.as_str().unwrap().to_owned())
        .collect()
}

/// Every dependency table of a manifest, by its name (`dependencies`, `dev-dependencies`, `build-dependencies`, a
/// target's), with its entries.
fn dependency_tables(manifest: &Value) -> Vec<(String, &toml::Table)> {
    let mut tables = Vec::new();
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(kind).and_then(Value::as_table) {
            tables.push((kind.to_owned(), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (target, value) in targets {
            for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = value.get(kind).and_then(Value::as_table) {
                    tables.push((format!("target.{target}.{kind}"), table));
                }
            }
        }
    }
    tables
}

#[test]
fn runtime_crates_enable_test_support_only_through_dev_dependencies() {
    for member in list(&workspace(), "members") {
        let manifest = manifest(&root().join(&member).join("Cargo.toml"));
        // The dev-only mock's optional media peer itself needs test-support. Its own dependency audit proves
        // that no runtime crate takes teams-mock as a normal/build dependency, including renamed entries.
        if manifest["package"]["name"].as_str() == Some("teams-mock") {
            continue;
        }
        for (kind, table) in dependency_tables(&manifest) {
            if kind.ends_with("dev-dependencies") {
                continue;
            }
            for (name, entry) in table {
                let mut features = entry.get("features").and_then(Value::as_array).into_iter().flatten();
                assert!(
                    !features.any(|feature| feature.as_str() == Some("test-support")),
                    "{member} {kind} {name} enables test-support"
                );
            }
        }
    }
}

/// `cargo tree` of `package` alone, its normal and build dependencies with their features.
fn features_of(package: &str) -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(root())
        .args([
            "tree",
            "--offline",
            "--locked",
            "-p",
            package,
            "-e",
            "normal,build,features",
            "--prefix",
            "none",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn normal_builds_leave_test_support_off_and_no_protocol_lab_is_linked() {
    let tree = features_of("ngteams");
    assert!(!tree.contains("proto-lab"), "{tree}");
    assert!(!tree.contains(r#"feature "test-support""#), "{tree}");
}

#[test]
fn a_plain_build_at_the_root_leaves_the_personal_build_out() {
    let workspace = workspace();
    let members = list(&workspace, "members");
    let defaults = list(&workspace, "default-members");
    assert!(members.contains(&"crates/ngteams".to_owned()));
    assert!(!defaults.contains(&"crates/ngteams".to_owned()));
    let mut rest: Vec<String> = members
        .into_iter()
        .filter(|member| member != "crates/ngteams")
        .collect();
    rest.sort();
    let mut sorted = defaults;
    sorted.sort();
    assert_eq!(sorted, rest, "every other member is a default one");
}

/// Every Rust, manifest and script file under `dirs` (relative to the root), with its text; `target` dirs skipped.
fn sources(dirs: &[&str]) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.iter().map(|dir| root().join(dir)).collect();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "target") {
                    stack.push(path);
                }
                continue;
            }
            let wanted = path.extension().is_some_and(|extension| {
                ["rs", "toml", "sh", "ps1", "yml", "yaml"]
                    .iter()
                    .any(|wanted| extension == *wanted)
            });
            if let (true, Ok(text)) = (wanted, fs::read_to_string(&path)) {
                found.push((path, text));
            }
        }
    }
    found
}

/// The files under `dirs` holding `needle`, relative to the root.
fn holding(dirs: &[&str], needle: &str) -> Vec<String> {
    let base = root().canonicalize().unwrap();
    let mut found: Vec<String> = sources(dirs)
        .into_iter()
        .filter(|(_, text)| text.contains(needle))
        .map(|(path, _)| {
            let path = path.canonicalize().unwrap();
            path.strip_prefix(&base).unwrap().to_string_lossy().replace('\\', "/")
        })
        .collect();
    found.sort();
    found
}

#[test]
fn lab_runners_clear_the_install_marker_before_building() {
    let marker = ["TEAMS_PERSONAL", "_INSTALL"].concat();
    let runner = fs::read_to_string(root().join("scripts/run-lab.sh")).unwrap();
    let unset = format!("env -u {marker} ");
    assert!(runner.contains(&unset));
    assert!(
        !runner.replace(&unset, "").contains(&marker),
        "the Linux runner only clears it"
    );
    let windows = fs::read_to_string(root().join("scripts/lib/windows-runner.ps1")).unwrap();
    let save = format!("$previousInstall = $env:{marker}");
    let clear = format!("Remove-Item Env:{marker} -ErrorAction SilentlyContinue");
    let restore = format!("$env:{marker} = $previousInstall");
    let build = windows.find("& $runner build").unwrap();
    assert!(windows.find(&save).unwrap() < windows.find(&clear).unwrap());
    assert!(windows.find(&clear).unwrap() < build && build < windows.find(&restore).unwrap());
    let remaining = windows.replace(&save, "").replace(&clear, "").replace(&restore, "");
    assert!(
        !remaining.contains(&marker),
        "the Windows runner only clears and restores it"
    );
}

#[test]
fn the_install_marker_is_confined_to_refusals_runners_and_authorized_builders() {
    let marker = ["TEAMS_PERSONAL", "_INSTALL"].concat();
    assert_eq!(
        holding(&["crates", "scripts", ".github/workflows"], &marker),
        [
            ".github/workflows/release.yml",
            "crates/ngteams/src/refusal.rs",
            "crates/teams-runner/src/windows.rs",
            "scripts/install.sh",
            "scripts/lib/windows-runner.ps1",
            "scripts/run-lab.sh",
        ]
    );
    let refusal = fs::read_to_string(root().join("crates/ngteams/src/refusal.rs")).unwrap();
    assert!(refusal.contains(&format!("option_env!(\"{marker}\")")));
    let native = fs::read_to_string(root().join("crates/teams-runner/src/windows.rs")).unwrap();
    assert_eq!(
        native.matches(&marker).count(),
        1,
        "the native runner only reads it for its build refusal"
    );
    assert!(native.contains(&format!("std::env::var_os(\"{marker}\")")));
    let script = fs::read_to_string(root().join("scripts/install.sh")).unwrap();
    assert!(script.contains(&format!("{marker}=1")) && script.contains("--release -p ngteams"));
    let release = fs::read_to_string(root().join(".github/workflows/release.yml")).unwrap();
    assert_eq!(
        release.matches(&format!("{marker}: \"1\"")).count(),
        3,
        "only the Linux, Windows and macOS release builds set it"
    );
}

#[test]
fn the_binary_refuses_first() {
    // from the first function on (the imports above it name what comes later)
    let text = fs::read_to_string(root().join("crates/ngteams/src/main.rs")).unwrap();
    let main = &text[text.find("fn main()").unwrap()..];
    let refused = main.find("ngteams::refusal()").unwrap();
    // the keyring is opened only through `ngteams::accounts`, which `main` reaches after the refusal; the sign-in
    // window's page engine is served after it too
    for later in [
        "pin_worker",
        "Cli::parse",
        "Setup::from_env",
        "platform::application",
        "accounts::",
    ] {
        assert!(
            main.find(later).is_some_and(|at| at > refused),
            "{later} before the refusal"
        );
    }
    let reason = ngteams::refusal().unwrap();
    assert!(reason.contains("test-support"), "a test build refuses: {reason}");
}

#[test]
fn only_this_crate_names_the_personal_keyring() {
    // Match a store's application argument, not a directory name.
    let application = ["\"ngteams", "\")"].concat();
    let code: Vec<String> = fs::read_dir(root().join("crates"))
        .unwrap()
        .flatten()
        .map(|entry| format!("crates/{}/src", entry.file_name().to_string_lossy()))
        .filter(|dir| root().join(dir).is_dir())
        .collect();
    let code: Vec<&str> = code.iter().map(String::as_str).collect();
    for file in holding(&code, &application) {
        assert!(file.starts_with("crates/ngteams/"), "{file} names the personal keyring");
    }
    assert_eq!(ngteams::KEYRING_APPLICATION, "ngteams");
    let secrets = fs::read_to_string(root().join("crates/ngteams/src/secrets.rs")).unwrap();
    assert!(secrets.contains("LazyKeyring::new(KEYRING_APPLICATION)"));
}

#[test]
fn the_binary_built_with_test_support_refuses_and_touches_nothing() {
    let home = tempfile::tempdir().unwrap();
    // Windows still needs the private runtime DLL path and the OS loader environment before main can refuse.
    let loader = if cfg!(windows) {
        ["PATH", "SYSTEMROOT", "WINDIR"]
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name, value)))
            .collect()
    } else {
        Vec::new()
    };
    let output = Command::new(env!("CARGO_BIN_EXE_ngteams"))
        .args(["--account", "someone@example.test"])
        .env_clear()
        .envs(loader)
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env("HOME", home.path())
        .env("LOCALAPPDATA", home.path().join("data"))
        .env("APPDATA", home.path().join("config"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("test-support"));
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0, "nothing was created");
}
