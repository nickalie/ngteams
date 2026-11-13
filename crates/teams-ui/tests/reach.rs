//! What the app crates (`teams-ui` and `ngteams`) can reach: every request goes through `teams-core`, so neither
//! depends directly on an HTTP or WebSocket client (read from `cargo metadata`, resolved for the host); GPUI's HTTP
//! client stays the null one, so nothing in their sources hands GPUI another; nothing in them reads a token's value
//! out of its `Secret`; and the search is the store's, never the service's (no client crate names Substrate).
//!
//! The media's PipeWire, Opus and audio processing bindings are held to the same audit (no network client, licences,
//! no logger of their own).
//!
//! The UI kit's audit (M2 Task 7), what can be checked of it: `gpui-component`'s whole tree for the host holds no
//! HTTP or WebSocket client, and every package in it is under a licence an Apache-2.0/MIT project can take; the
//! default features the kit turns on in `gpui-pre` give GPUI's Linux platform no X11 backend; and the app crates never
//! start the kit's theme directory watcher nor install a `log` logger or a `tracing` subscriber of their own: the only
//! logger is the diagnostic log's (`teams-log`, M2 Task 24a), installed by `ngteams` through its `install`, and it
//! takes only our crates' records, so the kit's log lines (and its `tracing` events, forwarded to `log`) go nowhere.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The crates that make the app.
const APP_CRATES: [&str; 2] = ["teams-ui", "ngteams"];

/// HTTP and WebSocket clients, by crate name.
const NETWORK_CLIENTS: [&str; 16] = [
    "reqwest",
    "hyper",
    "hyper-util",
    "h2",
    "ureq",
    "isahc",
    "curl",
    "surf",
    "attohttpc",
    "minreq",
    "awc",
    "tungstenite",
    "tokio-tungstenite",
    "async-tungstenite",
    "websocket",
    "gpui-pre-reqwest",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn host_triple() -> String {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output().expect("rustc runs");
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc names its host")
        .to_owned()
}

/// `cargo metadata` of the workspace, resolved for the host only.
fn metadata() -> Value {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace_root())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--filter-platform",
        ])
        .arg(host_triple())
        .output()
        .expect("cargo metadata runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).expect("JSON")
}

/// The package `id` names.
fn package<'a>(metadata: &'a Value, id: &Value) -> &'a Value {
    metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|candidate| &candidate["id"] == id)
        .expect("a package of the resolve")
}

/// The id of the package named `name` (the one version the workspace resolves).
fn package_id(metadata: &Value, name: &str) -> Value {
    metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .find(|candidate| candidate["name"] == name)
        .map_or_else(|| panic!("{name} is resolved"), |found| found["id"].clone())
}

/// The resolve's node of the package `id`.
fn node<'a>(metadata: &'a Value, id: &Value) -> &'a Value {
    metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve")
        .iter()
        .find(|node| &node["id"] == id)
        .expect("resolved")
}

/// The ids of the package `id`'s direct normal and build dependencies, as resolved.
fn dependency_ids(metadata: &Value, id: &Value) -> Vec<Value> {
    node(metadata, id)["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .filter(|dep| {
            dep["dep_kinds"].as_array().is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|kind| kind["kind"].is_null() || kind["kind"] == "build")
            })
        })
        .map(|dep| dep["pkg"].clone())
        .collect()
}

/// The names of `package`'s direct normal and build dependencies, as resolved.
fn direct_dependencies(metadata: &Value, package: &str) -> Vec<String> {
    dependency_ids(metadata, &package_id(metadata, package))
        .iter()
        .map(|id| self::package(metadata, id)["name"].as_str().expect("named").to_owned())
        .collect()
}

/// Every package `name` builds on through normal and build dependencies (proc macros and build scripts included),
/// itself among them.
fn closure<'a>(metadata: &'a Value, name: &str) -> Vec<&'a Value> {
    closure_from(metadata, package_id(metadata, name))
}

fn closure_from(metadata: &Value, root: Value) -> Vec<&Value> {
    let mut seen: Vec<Value> = Vec::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if seen.contains(&id) {
            continue;
        }
        pending.extend(dependency_ids(metadata, &id));
        seen.push(id);
    }
    seen.iter().map(|id| package(metadata, id)).collect()
}

#[test]
fn no_app_crate_depends_directly_on_an_http_or_websocket_client() {
    let metadata = metadata();
    for package in APP_CRATES {
        let dependencies = direct_dependencies(&metadata, package);
        assert!(
            dependencies.iter().any(|name| name == "teams-core"),
            "{package}: {dependencies:?}"
        );
        let clients: Vec<&String> = dependencies
            .iter()
            .filter(|name| NETWORK_CLIENTS.contains(&name.as_str()))
            .collect();
        assert!(clients.is_empty(), "{package} depends directly on {clients:?}");
    }
}

/// The licences (SPDX ids) a package in the kit's tree may be under: permissive ones, and MPL-2.0, whose copyleft
/// stops at its own files (`option-ext`, used by a build-time macro of the kit's assets crate).
const LICENCES: [&str; 14] = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "0BSD",
    "CC0-1.0",
    "Unlicense",
    "BSL-1.0",
    "MPL-2.0",
    "Unicode-DFS-2016",
];

/// Packages that give their licence as a file rather than an SPDX id, read as BSD-3-Clause (none in the tree today;
/// the check stays for the next one).
const BSD_3_CLAUSE_FILES: [&str; 0] = [];

/// What a licence file must say to be read as BSD-3-Clause: its grant, its third clause and its disclaimer.
const BSD_3_CLAUSE_TEXT: [&str; 3] = [
    "Redistribution and use in source and binary forms, with or without",
    "Neither the name of",
    "THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS",
];

/// A package's licence as an SPDX expression: its `license`, or for a package of [`BSD_3_CLAUSE_FILES`] whose
/// `license-file` holds that licence's text, BSD-3-Clause. `None` for anything else.
fn licence_of(found: &Value) -> Option<String> {
    if let Some(expression) = found["license"].as_str() {
        return Some(expression.to_owned());
    }
    let name = found["name"].as_str()?;
    if !BSD_3_CLAUSE_FILES.contains(&name) {
        return None;
    }
    let manifest = Path::new(found["manifest_path"].as_str()?);
    let text = fs::read_to_string(manifest.parent()?.join(found["license_file"].as_str()?)).ok()?;
    BSD_3_CLAUSE_TEXT
        .iter()
        .all(|phrase| text.contains(phrase))
        .then(|| "BSD-3-Clause".to_owned())
}

/// Whether `found`'s licence ([`licence_of`]) is one we can take.
fn licensed_for_us(found: &Value) -> bool {
    licence_of(found).is_some_and(|expression| licence_allowed(&expression))
}

/// Whether an SPDX expression (`A OR B`, `A/B`, `(A OR B) AND C`) allows use under one of [`LICENCES`]: some
/// alternative whose every part is allowed.
fn licence_allowed(expression: &str) -> bool {
    let plain = expression.replace(['(', ')'], "");
    let part_allowed = |part: &str| LICENCES.contains(&part.trim());
    // AND binds tighter than OR: `(A OR B) AND C` needs C and one of A or B
    plain.split(" AND ").all(|conjunct| {
        conjunct
            .split(" OR ")
            .flat_map(|alternative| alternative.split('/'))
            .any(part_allowed)
    })
}

#[test]
fn the_licence_check_reads_spdx_expressions() {
    for allowed in [
        "MIT OR Apache-2.0",
        "Apache-2.0/MIT",
        "(MIT OR Apache-2.0) AND Unicode-3.0",
        "Zlib",
        "MPL-2.0",
    ] {
        assert!(licence_allowed(allowed), "{allowed}");
    }
    for refused in [
        "GPL-3.0-only",
        "AGPL-3.0 OR GPL-2.0",
        "MIT AND GPL-3.0-only",
        "LGPL-2.1-or-later",
        "",
    ] {
        assert!(!licence_allowed(refused), "{refused}");
    }
}

#[test]
fn the_kit_brings_no_network_client_and_only_licences_we_can_take() {
    let metadata = metadata();
    assert!(
        direct_dependencies(&metadata, "teams-ui")
            .iter()
            .any(|name| name == "gpui-component"),
        "teams-ui takes the kit"
    );
    let tree = closure(&metadata, "gpui-component");
    assert!(tree.len() > 50, "the kit's tree was read ({} packages)", tree.len());
    let clients: Vec<&str> = tree
        .iter()
        .filter_map(|found| found["name"].as_str())
        .filter(|name| NETWORK_CLIENTS.contains(name))
        .collect();
    assert!(clients.is_empty(), "the kit's tree holds {clients:?}");
    let refused: Vec<String> = tree
        .iter()
        .filter(|found| !licensed_for_us(found))
        .map(|found| format!("{} {} ({})", found["name"], found["version"], found["license"]))
        .collect();
    assert!(refused.is_empty(), "under a licence we can't take: {refused:?}");
    // one GPUI: the kit resolves to the vendored, patched copy the workspace patches in
    let gpui: Vec<&&Value> = tree.iter().filter(|found| found["name"] == "gpui-pre").collect();
    assert_eq!(gpui.len(), 1);
    assert!(
        gpui[0]["source"].is_null(),
        "gpui-pre is the vendored path crate: {}",
        gpui[0]["id"]
    );
}

/// The PipeWire binding the device seam's backend takes (`teams-media`): the crate and the crates it binds through.
#[cfg(target_os = "linux")]
const PIPEWIRE_BINDING: [&str; 4] = ["pipewire", "libspa", "pipewire-sys", "libspa-sys"];

/// The calls' media engine (`teams-media`): the safe binding of libwebrtc and the crate that binds and links the
/// prebuilt static libwebrtc (`LiveKit`'s).
const LIBWEBRTC_BINDING: [&str; 2] = ["libwebrtc", "webrtc-sys"];

/// The one crate allowed an HTTP client in libwebrtc's tree, and only as a build dependency: `webrtc-sys`'s build
/// helper, which downloads `LiveKit`'s prebuilt libwebrtc from GitHub on a clean build (`LK_CUSTOM_WEBRTC` names a local
/// one instead). Its `reqwest` runs in the build script and links into no binary.
const LIBWEBRTC_BUILD_HELPER: &str = "webrtc-sys-build";

/// The `GStreamer` bindings the inline video player takes (`teams-media`'s `player`): the core binding and the `GLib`
/// it binds through.
const GSTREAMER_BINDING: [&str; 5] = ["gstreamer", "gstreamer-sys", "glib", "glib-sys", "gobject-sys"];

/// The `GStreamer` app elements' binding the player's `appsrc` and appsinks take, and the base classes it binds
/// through.
const GSTREAMER_APP_BINDING: [&str; 4] = [
    "gstreamer-app",
    "gstreamer-app-sys",
    "gstreamer-base",
    "gstreamer-base-sys",
];

/// A native binding `teams-media` takes (its first crate the direct dependency), held to the kit's audit before it
/// reaches the app: no HTTP or WebSocket client in its tree, only licences we can take, and no logger in the binding
/// itself, so nothing of it can bypass `teams-log` or the compile-time cap.
fn audit_media_binding(binding: &[&str]) {
    let metadata = metadata();
    let root = binding[0];
    // Select the dependency edge, since the workspace's spikes can resolve another version of the same binding.
    let id = dependency_ids(&metadata, &package_id(&metadata, "teams-media"))
        .into_iter()
        .find(|id| package(&metadata, id)["name"] == root)
        .unwrap_or_else(|| panic!("teams-media takes {root}"));
    let tree = closure_from(&metadata, id);
    for name in binding {
        assert!(
            tree.iter().any(|found| found["name"] == *name),
            "{name} is in the binding's tree"
        );
    }
    let clients: Vec<&str> = tree
        .iter()
        .filter_map(|found| found["name"].as_str())
        .filter(|name| NETWORK_CLIENTS.contains(name))
        .collect();
    assert!(clients.is_empty(), "the binding's tree holds {clients:?}");
    let refused: Vec<String> = tree
        .iter()
        .filter(|found| !licensed_for_us(found))
        .map(|found| format!("{} {} ({})", found["name"], found["version"], found["license"]))
        .collect();
    assert!(refused.is_empty(), "under a licence we can't take: {refused:?}");
    for found in tree
        .iter()
        .filter(|found| found["name"].as_str().is_some_and(|name| binding.contains(&name)))
    {
        let name = &found["name"];
        let loggers: Vec<&str> = dependency_ids(&metadata, &found["id"])
            .into_iter()
            .filter_map(|id| package(&metadata, &id)["name"].as_str())
            .filter(|dependency| ["log", "tracing"].contains(dependency))
            .collect();
        assert!(loggers.is_empty(), "{name} logs through {loggers:?}");
    }
}

/// libpipewire's own warnings go to stderr, not through `log`.
#[cfg(target_os = "linux")]
#[test]
fn the_media_s_pipewire_binding_brings_no_network_client_no_logger_and_only_licences_we_can_take() {
    audit_media_binding(&PIPEWIRE_BINDING);
}

#[cfg(windows)]
#[test]
fn the_media_s_windows_capture_binding_brings_no_network_client_no_logger_and_only_licences_we_can_take() {
    audit_media_binding(&["windows-capture", "windows", "windows-core", "windows-future"]);
}

/// The ids of the package `id`'s direct normal dependencies (what links into a binary), as resolved.
fn runtime_dependency_ids(metadata: &Value, id: &Value) -> Vec<Value> {
    node(metadata, id)["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .filter(|dep| {
            dep["dep_kinds"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind["kind"].is_null()))
        })
        .map(|dep| dep["pkg"].clone())
        .collect()
}

/// Every package `name` links in through normal dependencies, itself among them.
fn runtime_closure<'a>(metadata: &'a Value, name: &str) -> Vec<&'a Value> {
    let mut seen: Vec<Value> = Vec::new();
    let mut pending = vec![package_id(metadata, name)];
    while let Some(id) = pending.pop() {
        if seen.contains(&id) {
            continue;
        }
        pending.extend(runtime_dependency_ids(metadata, &id));
        seen.push(id);
    }
    seen.iter().map(|id| package(metadata, id)).collect()
}

/// libwebrtc is held to the media bindings' audit with two stated exceptions, both outside what runs:
/// - **its build downloads**: `webrtc-sys`'s build helper ([`LIBWEBRTC_BUILD_HELPER`]) fetches the prebuilt library
///   with `reqwest`; no HTTP client is in what links into a binary, and the media's network is libwebrtc's own sockets
///   to the call's relay selected by its signaling setup.
/// - **its logger**: the binding forwards libwebrtc's own log lines to `log` at `debug` under the target
///   `libwebrtc`; `teams-log` writes only our crates' targets (`teams_…`), so none of them reaches the log file.
///
/// The prebuilt itself (BSD-3-Clause libwebrtc with `FFmpeg`'s H.264 decoder, LGPL, linked statically) is no crate of
/// the tree: the personal build is the user's own and never distributed.
#[test]
fn the_media_s_libwebrtc_binding_downloads_only_at_build_time_and_logs_nowhere_we_write() {
    let metadata = metadata();
    assert!(
        direct_dependencies(&metadata, "teams-media")
            .iter()
            .any(|name| name == LIBWEBRTC_BINDING[0]),
        "teams-media takes libwebrtc"
    );
    let linked = runtime_closure(&metadata, LIBWEBRTC_BINDING[0]);
    for name in LIBWEBRTC_BINDING {
        assert!(linked.iter().any(|found| found["name"] == name), "{name} links in");
    }
    let clients: Vec<&str> = linked
        .iter()
        .filter_map(|found| found["name"].as_str())
        .filter(|name| NETWORK_CLIENTS.contains(name))
        .collect();
    assert!(clients.is_empty(), "libwebrtc links in {clients:?}");
    // the build's tree: a network client only under the build helper
    let built = closure(&metadata, LIBWEBRTC_BINDING[0]);
    let helper = closure(&metadata, LIBWEBRTC_BUILD_HELPER);
    let outside: Vec<&str> = built
        .iter()
        .filter(|found| !helper.iter().any(|ours| ours["id"] == found["id"]))
        .filter_map(|found| found["name"].as_str())
        .filter(|name| NETWORK_CLIENTS.contains(name))
        .collect();
    assert!(
        outside.is_empty(),
        "a network client outside the build helper: {outside:?}"
    );
    let refused: Vec<String> = linked
        .iter()
        .filter(|found| !licensed_for_us(found))
        .map(|found| format!("{} {} ({})", found["name"], found["version"], found["license"]))
        .collect();
    assert!(refused.is_empty(), "under a licence we can't take: {refused:?}");
    assert!(
        !teams_log::ours(&log::Metadata::builder().target("libwebrtc").build()),
        "teams-log keeps only our crates' lines"
    );
}

/// `GStreamer` itself (LGPL, the system's, linked dynamically) is no crate of the tree; its bindings are MIT or
/// Apache-2.0, take `log` only behind a feature left off, and bring no network client: the player feeds `playbin` from
/// an `appsrc`, so `GStreamer` reads nothing from the network.
#[test]
fn the_media_s_gstreamer_bindings_bring_no_network_client_no_logger_and_only_licences_we_can_take() {
    audit_media_binding(&GSTREAMER_BINDING);
    audit_media_binding(&GSTREAMER_APP_BINDING);
}

#[cfg(target_os = "linux")]
#[test]
fn the_kit_s_default_gpui_features_give_linux_no_x11_backend() {
    let metadata = metadata();
    // the kit takes gpui-pre with its defaults, `x11` among them: in gpui-pre itself that only names a compositor
    // GPUI could guess; the X11 client lives in gpui-pre-linux, whose features nobody turns `x11` on for
    let features = |name: &str| -> Vec<String> {
        node(&metadata, &package_id(&metadata, name))["features"]
            .as_array()
            .expect("features")
            .iter()
            .filter_map(|feature| feature.as_str().map(str::to_owned))
            .collect()
    };
    let linux = features("gpui-pre-linux");
    assert!(linux.iter().any(|feature| feature == "wayland"), "{linux:?}");
    assert!(
        !linux.iter().any(|feature| feature == "x11"),
        "gpui-pre-linux: {linux:?}"
    );
    let platform = features("gpui-pre-platform");
    assert!(
        !platform.iter().any(|feature| feature == "x11"),
        "gpui-pre-platform: {platform:?}"
    );
}

#[cfg(windows)]
#[test]
fn the_kit_resolves_the_native_windows_backend_without_the_linux_backend() {
    let metadata = metadata();
    let dependencies = direct_dependencies(&metadata, "gpui-pre-platform");
    assert!(
        dependencies.iter().any(|name| name == "gpui-pre-windows"),
        "{dependencies:?}"
    );
    assert!(
        dependencies.iter().all(|name| name != "gpui-pre-linux"),
        "{dependencies:?}"
    );
    let tree = closure(&metadata, "gpui-component");
    assert!(tree.iter().all(|found| found["name"] != "gpui-pre-linux"));
}

/// Every `.rs` file under `dir`, with its text.
fn sources(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut dirs = vec![dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).expect("readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = fs::read_to_string(&path).expect("UTF-8");
                found.push((path, text));
            }
        }
    }
    found
}

/// What the app crates' sources never name: another HTTP client for GPUI, a token's value, the kit's theme directory
/// watcher, a `log` logger or a `tracing` subscriber of their own (the only logger is `teams-log`'s, installed through
/// its `install`).
const FORBIDDEN: [&str; 8] = [
    "set_http_client",
    "with_http_client",
    ".expose()",
    "watch_dir(",
    "set_logger",
    "set_boxed_logger",
    "set_global_default",
    "tracing_subscriber",
];

/// What sets a `log` logger or a `tracing` subscriber.
const LOGGER_SETUP: [&str; 4] = [
    "set_logger",
    "set_boxed_logger",
    "set_global_default",
    "tracing_subscriber",
];

/// The crates the app is built from besides the app crates: none of them sets a logger either.
const LINKED_CRATES: [&str; 5] = [
    "teams-proto",
    "teams-core",
    "teams-trouter",
    "teams-store",
    "teams-auth",
];

/// Only this logger (M2 Task 24a): `teams-log` is the one crate of the app that sets a logger, and only with `log`'s
/// `set_logger` (no `tracing` subscriber, so the kit's `tracing` events reach `log` and its filter); no other crate the
/// app is built from sets one; the app crates install it only through `teams_log::install`, which `ngteams` calls
/// once and `teams-ui` never.
#[test]
fn the_only_logger_is_teams_log_s_installed_by_the_app() {
    let mut read = 0;
    for package in APP_CRATES.iter().chain(&LINKED_CRATES) {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for setup in LOGGER_SETUP {
                assert!(!text.contains(setup), "{} names {setup}", path.display());
            }
            let installs = text.matches("teams_log::install(").count();
            if path.ends_with("ngteams/src/app.rs") {
                assert_eq!(installs, 1, "the app installs the log once");
            } else {
                assert_eq!(installs, 0, "{} installs the log", path.display());
            }
        }
    }
    assert!(read >= 60, "the crates' sources were read ({read} files)");
    let log = sources(&workspace_root().join("crates/teams-log/src"));
    let setters: Vec<&PathBuf> = log
        .iter()
        .filter(|(_, text)| text.contains("log::set_logger("))
        .map(|(path, _)| path)
        .collect();
    assert_eq!(setters.len(), 1, "teams-log sets the logger in one place");
    for (path, text) in &log {
        for setup in ["set_boxed_logger", "set_global_default", "tracing_subscriber"] {
            assert!(!text.contains(setup), "{} names {setup}", path.display());
        }
    }
    let manifest = fs::read_to_string(workspace_root().join("crates/teams-log/Cargo.toml")).expect("UTF-8");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|rest| rest.split("\n[").next())
        .expect("its dependencies");
    assert!(
        !dependencies
            .lines()
            .any(|line| line.trim_start().starts_with("tracing")),
        "teams-log takes no tracing: {dependencies}"
    );
}

#[test]
fn gpui_keeps_its_null_http_client_and_no_token_is_read_out() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for forbidden in FORBIDDEN {
                assert!(!text.contains(forbidden), "{} uses {forbidden}", path.display());
            }
        }
    }
    assert!(read >= 9, "the app crates' sources were read ({read} files)");
}

/// What the app crates' sources never name: the kit's markdown and HTML view (`TextView`, its `text` module, its
/// `markdown`/`html` constructors) and its chat widgets. A message's text is drawn only by our own renderer
/// (`rich_text`), from `teams-proto`'s sanitized model.
const RENDERERS: [&str; 7] = [
    "TextView",
    "gpui_component::text",
    "::markdown(",
    "::html(",
    "MessageScroller",
    "gpui_component::message",
    "gpui_component::bubble",
];

#[test]
fn no_message_html_reaches_a_markdown_or_html_view() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for renderer in RENDERERS {
                assert!(!text.contains(renderer), "{} names {renderer}", path.display());
            }
        }
    }
    assert!(read >= 11, "the app crates' sources were read ({read} files)");
    // the chat view draws a message's content through our renderer
    let chat_view = fs::read_to_string(workspace_root().join("crates/teams-ui/src/chat_view.rs")).expect("UTF-8");
    assert!(chat_view.contains("rich_text::render_selectable("));
}

/// What reaches a bus other than the one the app is given: `zbus`'s session-bus constructors (an address from the
/// environment, else `$XDG_RUNTIME_DIR/bus`), GPUI's own system notifications (which use them, a connection each),
/// and the blocking API's (its own constructors alike). macOS's desktop transport uses GPUI's notification center.
const OTHER_BUSES: [&str; 6] = [
    "Connection::session(",
    "Builder::session(",
    "Address::session(",
    "zbus::blocking",
    "show_system_notification",
    "on_system_notification_response",
];

/// The notifications and the tray item reach only the bus the app is given (M2 Task 14): no app crate's source calls
/// another way onto a bus, the one connection is made by `bus::connect` from the address alone, and the address is
/// read only by `bus::address_from_env`.
#[test]
fn the_desktop_is_reached_only_on_the_bus_given() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for other in OTHER_BUSES {
                let macos_notification = path.ends_with("teams-ui/src/desktop/macos.rs")
                    && matches!(other, "show_system_notification" | "on_system_notification_response");
                assert!(
                    macos_notification || !text.contains(other),
                    "{} names {other}",
                    path.display()
                );
            }
            let connects = text.matches("connection::Builder::").count();
            let reads_env = text.contains("bus::ADDRESS_ENV") || text.contains("var_os(ADDRESS_ENV)");
            if path.ends_with("teams-ui/src/bus.rs") {
                assert_eq!(connects, 1, "bus.rs builds one connection");
                assert!(text.contains("zbus::connection::Builder::address(address)"));
                assert!(reads_env);
            } else {
                assert_eq!(connects, 0, "{} builds a bus connection of its own", path.display());
                assert!(!reads_env, "{} reads the bus address itself", path.display());
            }
        }
    }
    assert!(read >= 15, "the app crates' sources were read ({read} files)");
}

/// What builds a request for a new chat: the engine's action or the outbox's command, named or brought in by name
/// (`use …::Action::CreateChat`, a glob of either's variants, or the variant's fields after its bare name). A row's
/// kind (`OutboxKind::CreateChat`, shown by the view as it stands) builds nothing.
const CREATES_A_CHAT: [&str; 5] = [
    "Action::CreateChat",
    "Command::CreateChat",
    "Action::*",
    "Command::*",
    "CreateChat {",
];

/// Only an explicit people-search activation, New message's first message, or `ngteamsctl new-chat` creates a new chat,
/// through the engine's persistent outbox. Other views cannot create one incidentally.
#[test]
fn only_people_search_and_new_message_offer_creating_a_chat_through_the_engine() {
    let mut read = 0;
    for package in APP_CRATES {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            if creates_a_chat_as_offered(&path, &text) {
                continue;
            }
            for builds in CREATES_A_CHAT {
                assert!(!text.contains(builds), "{} names {builds}", path.display());
            }
        }
    }
    assert!(read >= 13, "the app crates' sources were read ({read} files)");
    // compose writes through the engine's actions: a post, an edit
    let compose = fs::read_to_string(workspace_root().join("crates/teams-ui/src/compose.rs")).expect("UTF-8");
    assert!(compose.contains("self.core.write(action)"));
}

/// Whether `path` is one of the sources offering a new chat, checked to create it only as offered.
fn creates_a_chat_as_offered(path: &Path, text: &str) -> bool {
    if path.ends_with("teams-ui/src/global_search.rs") {
        assert_eq!(text.matches("self.core.write(Action::CreateChat").count(), 1);
        assert!(text.contains("if self.core.read_only()"));
        return true;
    }
    if path.ends_with("teams-ui/src/new_chat.rs") {
        // made only from the box's first message, never in the read-only mode
        assert_eq!(text.matches("self.core.write(Action::CreateChat").count(), 1);
        assert!(text.contains("self.core.read_only()"));
        assert!(text.contains("ComposeEvent::Start"));
        return true;
    }
    if path.ends_with("teams-ui/src/profile_card.rs") {
        // made only by the card's quick message to someone without a 1:1 chat, never on the account's own card nor in
        // the read-only mode
        assert_eq!(text.matches("core.write(Action::CreateChat").count(), 1);
        assert!(text.contains("!self.own() && !self.core.read_only()"));
        assert!(text.contains("|| !self.writes() ||"));
        return true;
    }
    if path.ends_with("ngteams/src/ctl/writes.rs") {
        // `ngteamsctl new-chat`, the user's own explicit command: the action handed to the engine's write, never in
        // the read-only mode
        assert_eq!(text.matches("Action::CreateChat").count(), 1);
        assert!(text.contains("core.write(action)"));
        assert!(text.contains("if core.read_only()"));
        return true;
    }
    false
}

/// The crates the native client's requests are sent from or kept in, besides the app crates and `teams-proto` (where
/// every request is built).
const CLIENT_CRATES: [&str; 3] = ["teams-core", "teams-trouter", "teams-store"];

/// What names the service's search (Substrate): its host, and its search and suggestion paths.
const SERVER_SEARCH: [&str; 5] = [
    "substrate.office.com",
    "search/api/",
    "searchservice/api",
    "/suggestions",
    "userknowledgebase",
];

/// The search is the store's and the service's, each only through the engine: the chats' search (M2 Task 15) is
/// answered from the store, and the title bar's (people, files, messages) by Substrate, a read by `POST`. Substrate is
/// `teams-proto`'s alone: its `Service`, host and paths are named there and nowhere else of the native client (the
/// engine sends its typed request, the transport reaches only the hosts of its `Service`s), and the views ask only the
/// engine's `search` and `search_service`.
#[test]
fn the_search_is_the_store_s_and_the_service_s_only_through_the_engine() {
    let mut read = 0;
    for package in APP_CRATES.iter().chain(&CLIENT_CRATES) {
        for (path, text) in sources(&workspace_root().join("crates").join(package).join("src")) {
            read += 1;
            for named in SERVER_SEARCH {
                assert!(!text.contains(named), "{} names {named}", path.display());
            }
            assert!(
                !text.contains("SUBSTRATE_HOST"),
                "{} names Substrate's host",
                path.display()
            );
        }
    }
    assert!(read >= 100, "the client crates' sources were read ({read} files)");
    let search = fs::read_to_string(workspace_root().join("crates/teams-ui/src/search.rs")).expect("UTF-8");
    assert!(search.contains("core.search(query, within, MAX_HITS)"));
    let global = fs::read_to_string(workspace_root().join("crates/teams-ui/src/global_search.rs")).expect("UTF-8");
    assert!(global.contains("core.search_service("), "the title bar asks the engine");
    let engine = fs::read_to_string(workspace_root().join("crates/teams-core/src/engine/inbox.rs")).expect("UTF-8");
    assert!(engine.contains("Some(within) => store.search_in(&query, &within, limit),"));
    assert!(engine.contains("None => store.search(&query, limit),"));
    let services = fs::read_to_string(workspace_root().join("crates/teams-proto/src/endpoint.rs")).expect("UTF-8");
    assert!(
        services.contains("    Substrate,\n"),
        "Substrate is a Service of teams-proto's"
    );
    let proto = fs::read_to_string(workspace_root().join("crates/teams-proto/src/search.rs")).expect("UTF-8");
    assert!(proto.contains("pub const SUBSTRATE_HOST: &str = \"substrate.office.com\";"));
}

/// A channel's posts, replies and reactions are written as a chat's are: by the chat view and its compose box, through
/// Core's outbox write (`Action`), never a channel write route of their own. The channel view and the teams list
/// write nothing themselves; channel management and account preferences use domain actions through Core; views
/// neither construct protocol request builders nor send them.
#[test]
fn channel_writes_go_only_through_the_chat_view_and_views_reach_management_only_through_core() {
    let proto = workspace_root().join("crates/teams-proto/src");
    let mut read = 0;
    for (path, text) in sources(&proto) {
        read += 1;
        assert!(
            !text.contains("WriteKind::ChannelPost"),
            "{} maps a channel write",
            path.display()
        );
    }
    assert!(read >= 10, "teams-proto's sources were read ({read} files)");
    for (view, composes) in [("teams_list.rs", false), ("channel_view.rs", true)] {
        let text = fs::read_to_string(workspace_root().join("crates/teams-ui/src").join(view)).expect("UTF-8");
        let mut forbidden = vec![".write(", ".resend(", ".transient(", "MessageAction::"];
        // the channel view makes its chat views' boxes say where they write; the list has none
        if !composes {
            forbidden.extend(["Compose", "compose::"]);
        }
        for writes in forbidden {
            assert!(!text.contains(writes), "{view} names {writes}");
        }
        for (at, _) in text.match_indices("Action::") {
            let is_engine_action = text[..at]
                .chars()
                .next_back()
                .is_none_or(|before| !before.is_alphanumeric() && before != '_');
            assert!(!is_engine_action, "{view} names the engine's message Action");
        }
    }
}

// These are concrete transport/request builders, rather than domain DTOs such as ChannelSectionAction,
// OwnTeamView, TeamWrite, ChannelRead and WorkflowRequest, whose fixed operations the engine interprets.
const HTTP_BUILDERS: &[&str] = &[
    "HttpTransport",
    "BuiltRequest",
    "Endpoint",
    "Client",
    "Transport",
    "RequestBuilder",
    "Prepared",
    "ChannelSectionWrite",
    "DriveWrite",
    "CaptionWrite",
    "CallWrite",
    "EndpointRegistration",
];

#[test]
fn views_never_construct_or_send_protocol_requests() {
    for (path, text) in sources(&workspace_root().join("crates/teams-ui/src")) {
        for (at, _) in text.match_indices("CallingRegistration") {
            assert!(
                text[..at].ends_with("Event::"),
                "{} names the calling registration request builder",
                path.display()
            );
        }
        for token in text.split(|character: char| !character.is_alphanumeric() && character != '_') {
            assert!(
                !HTTP_BUILDERS.contains(&token),
                "{} names protocol builder {token}",
                path.display()
            );
        }
        for sends in [
            ".transport(",
            ".deliver(",
            ".send_as(",
            "impl Request for",
            "impl teams_proto::Request for",
        ] {
            assert!(
                !text.contains(sends),
                "{} constructs or sends a protocol request with {sends}",
                path.display()
            );
        }
    }
}
