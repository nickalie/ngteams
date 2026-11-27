//! The endpoint inventory (`docs/protocol/endpoints.md`) against the own server: every request the native client
//! sends (a row whose Native column names an entry) is served by the server, hidden by the client for an own-server
//! account (`teams_core::Capabilities`, from the GTMs the server's authz names), fetched from a public host without
//! credentials, or a known gap that names why.

use ngteams_server::config::Config;
use teams_core::Capabilities;

const INVENTORY: &str = include_str!("../../../../docs/protocol/endpoints.md");

/// What the own server does with one request the client sends.
#[derive(Debug, PartialEq, Eq)]
enum Status {
    Served,
    Hidden(String),
    PublicHost,
    NotServed(String),
}

/// One row of the inventory the native client sends: its line, its method and path, and its own-server status.
#[derive(Debug)]
struct Row {
    line: usize,
    what: String,
    status: Option<Status>,
}

/// The text in the parentheses that `note` starts with after `marker`.
fn named(note: &str, marker: &str) -> Option<String> {
    let rest = note.strip_prefix(marker)?;
    Some(rest[..rest.find(')')?].trim().to_owned())
}

/// The status the row's first `own server:` note starts with; `None` for a row without one, or whose note starts
/// with anything else (a typo, a refusal or a to-do is no status).
fn status(row: &str) -> Option<Status> {
    const MARKER: &str = "own server:";
    let row = row.to_lowercase();
    let note = row[row.find(MARKER)? + MARKER.len()..].trim_start();
    if let Some(capability) = named(note, "hidden (") {
        return Some(Status::Hidden(capability));
    }
    if let Some(reason) = named(note, "not served (") {
        return Some(Status::NotServed(reason));
    }
    if note.starts_with("public host") {
        return Some(Status::PublicHost);
    }
    let served = note
        .strip_prefix("served")
        .is_some_and(|rest| !rest.starts_with(char::is_alphanumeric));
    served.then_some(Status::Served)
}

/// The rows of the inventory's tables whose Native column names a native entry.
fn native_rows() -> Vec<Row> {
    INVENTORY
        .lines()
        .enumerate()
        .filter(|(_, line)| line.starts_with("| `"))
        .filter_map(|(at, line)| {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
            let native = cells.get(3)?;
            if cells.len() < 6 || native.starts_with("not used") || native.starts_with("no service") {
                return None;
            }
            Some(Row {
                line: at + 1,
                what: format!("{} {}", cells[0], cells[1]),
                status: status(line),
            })
        })
        .collect()
}

/// Whether a server of `config` leaves the client's `capability` off.
fn left_off(config: &Config, capability: &str) -> Option<bool> {
    let gtms = ngteams_server::services::region_gtms(config);
    let own = teams_proto::Cloud::own("https://own.example.test").unwrap();
    let capabilities = Capabilities::of(&own, |gtm| gtms.contains_key(gtm));
    let on = match capability {
        "workflows" => capabilities.workflows,
        "recap" => capabilities.recap,
        "captions" => capabilities.captions,
        "recording" => capabilities.recording,
        "apps" => capabilities.apps,
        "copilot" => capabilities.copilot,
        _ => return None,
    };
    Some(!on)
}

#[test]
fn every_request_the_client_sends_is_served_hidden_public_or_a_named_gap() {
    let rows = native_rows();
    assert!(rows.len() > 150, "the inventory's tables are read: {} rows", rows.len());
    let unmarked: Vec<String> = rows
        .iter()
        .filter(|row| row.status.is_none())
        .map(|row| format!("line {}: {}", row.line, row.what))
        .collect();
    assert!(
        unmarked.is_empty(),
        "rows the client sends whose note does not start with an own-server status (`own server: served`, \
         `own server: hidden (<capability>)`, `own server: public host`, `own server: not served (<why>)`):\n{}",
        unmarked.join("\n")
    );
    let mut off = Config::default();
    off.workflows.enabled = false;
    for row in &rows {
        match &row.status {
            Some(Status::Hidden(capability)) => assert_eq!(
                left_off(&off, capability),
                Some(true),
                "line {}: `{capability}` is no capability the client hides without its GTM",
                row.line
            ),
            Some(Status::NotServed(reason)) => {
                assert!(!reason.is_empty(), "line {}: a gap names why", row.line);
            }
            _ => {}
        }
    }
}

#[test]
fn only_a_note_that_starts_with_a_status_has_one() {
    assert_eq!(status("| x | own server: served |"), Some(Status::Served));
    assert_eq!(
        status("| x | Own server: served: the caller's own stream |"),
        Some(Status::Served)
    );
    assert_eq!(
        status("| x | own server: served ([server.md](x)) |"),
        Some(Status::Served)
    );
    assert_eq!(
        status("| x | own server: hidden (apps) |"),
        Some(Status::Hidden("apps".into()))
    );
    assert_eq!(
        status("| x | own server: not served (no store) |"),
        Some(Status::NotServed("no store".into()))
    );
    assert_eq!(status("| x | own server: public host |"), Some(Status::PublicHost));
    for row in [
        "| x | own server: hiden (apps) |",
        "| x | own server: 400 (only emotions) |",
        "| x | own server: TODO |",
        "| x | own server: the caller's own stream |",
        "| x | own server: serves it |",
        "| x | served |",
    ] {
        assert_eq!(status(row), None, "{row}");
    }
}

#[test]
fn a_default_server_names_workflows_and_apps_and_leaves_every_other_optional_service_off() {
    let mut config = Config::default();
    assert_eq!(left_off(&config, "workflows"), Some(false));
    assert_eq!(left_off(&config, "apps"), Some(false));
    for capability in ["recap", "captions", "recording", "copilot"] {
        assert_eq!(left_off(&config, capability), Some(true), "{capability}");
    }
    config.apps.enabled = false;
    assert_eq!(left_off(&config, "apps"), Some(true));
    assert_eq!(left_off(&config, "telepathy"), None);
}
