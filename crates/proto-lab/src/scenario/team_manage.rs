//! Drives team-management UI through a control socket and captures its requests. The default capture holds team writes; explicitly enabled writes go through directly.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::capture_hold::Hold;
use crate::config::{Alias, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{load, save_screenshot_as};
use super::ab_trouter::size;
use super::channel_create::click_named;
use super::dsl::{Driver, Key, SeenRequest, Selector};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The label of a capture hold a `hold` step added.
pub const HELD_BY_A_STEP: &str = "held back by a step";

/// The label of the capture hold of every write on a team, a group or a channel.
pub const TEAM_WRITE: &str = "a team's write";

/// What makes a write one on a team, a group or a channel, on any host.
pub const TEAM_URL: &str = r"(?i)/teams?(/|\?|$)|/groups?(/|\?|$)|/channels?(warmup)?([/?]|$)|/standardchannel|/standalonechannels|/tags?(/|\?|$)|/teamwork";

/// The methods denied on [`TEAM_URL`]: every one but a read.
const WRITE_METHODS: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

/// The services `seen` looks at: the middle tier, the provisioning service, CSA and the chat service on a Teams origin.
const TEAM_SERVICES: [&str; 4] = [
    "teams.cloud.microsoft/api/mt/",
    "teams.cloud.microsoft/fabric/",
    "teams.cloud.microsoft/api/csa/",
    "teams.cloud.microsoft/api/chatsvc/",
];

/// What makes `seen` leave a body out: tokens never reach stdout or a socket's answer.
const TOKEN_WORDS: [&str; 3] = ["token", "bearer", "eyj"];

/// The rules the run forces when it holds writes back.
///
/// # Errors
/// Never: the patterns are fixed.
pub fn team_rules() -> Result<Vec<Hold>, regex::Error> {
    WRITE_METHODS
        .iter()
        .map(|method| Ok(Hold::new(method, TEAM_URL)?.labelled(TEAM_WRITE)))
        .collect()
}

fn account() -> Alias {
    match std::env::var("PROTO_LAB_ACCOUNT").as_deref() {
        Ok("b") => Alias::B,
        Ok("c") => Alias::C,
        _ => Alias::A,
    }
}

/// Runs `team-manage` (see the module docs).
///
/// # Errors
/// When the run can't start: no Chrome, a browser whose capture setup fails, no socket named.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let writes = std::env::var("PROTO_LAB_TEAM_WRITES").as_deref() == Ok("1");
    let session_options = SessionOptions {
        capture: Some(ScenarioName::TeamManage.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,

        ..SessionOptions::default()
    };
    let alias = account();
    let mut page = LabSession::open_with(paths, alias, session_options).await?;
    let socket: PathBuf = std::env::var_os("PROTO_LAB_STEPS_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| ScenarioError::Setup("PROTO_LAB_STEPS_SOCKET names no socket".to_owned()))?;
    let mut report = ScenarioReport::new(ScenarioName::TeamManage);
    let mut drive = page.driver();
    match if writes { Ok(Vec::new()) } else { team_rules() } {
        Ok(rules) => rules.into_iter().for_each(|rule| page.hold(rule)),
        Err(error) => report.failures.push(error.to_string()),
    }
    if report.failures.is_empty()
        && let Err(error) = steps(alias, &mut page, &mut drive, &mut report, &socket, options).await
    {
        report.failures.push(error.to_string());
    }
    report.close_account(alias, page).await;
    let _ = std::fs::remove_file(&socket);
    Ok(report)
}

async fn steps(
    alias: Alias,
    page: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    socket: &std::path::Path,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    load(alias, page, options.load_timeout).await?;
    size(page).await?;
    serve(alias, page, drive, report, socket, &TEAM_SERVICES).await
}

/// Takes steps on `socket` until one says `quit`; `seen` looks at the requests to `services`.
pub(crate) async fn serve(
    alias: Alias,
    page: &mut LabSession,
    drive: &mut Driver,
    report: &mut ScenarioReport,
    socket: &std::path::Path,
    services: &[&str],
) -> Result<(), ScenarioError> {
    let mut pages = [StepPage::new(alias, page, drive)];
    serve_pages(&mut pages, report, socket, services).await
}

/// A page steps can drive: its account, its session, its driver and how far its `seen` looked.
pub(crate) struct StepPage<'a> {
    alias: Alias,
    page: &'a LabSession,
    drive: &'a mut Driver,
    seen: usize,
}

impl<'a> StepPage<'a> {
    pub(crate) fn new(alias: Alias, page: &'a LabSession, drive: &'a mut Driver) -> Self {
        let seen = drive.seen_so_far();
        Self {
            alias,
            page,
            drive,
            seen,
        }
    }
}

/// Takes steps on `socket` for `pages` until one says `quit`: a step goes to the first page unless it starts with
/// `@<alias> ` (`@b shot b-share.png`), which sends it to that account's page; `seen` looks at the requests to
/// `services` of the page it goes to.
pub(crate) async fn serve_pages(
    pages: &mut [StepPage<'_>],
    report: &mut ScenarioReport,
    socket: &std::path::Path,
    services: &[&str],
) -> Result<(), ScenarioError> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).map_err(|error| ScenarioError::Setup(error.to_string()))?;
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| ScenarioError::Setup(error.to_string()))?;
        if connection(pages, report, stream, services).await {
            return Ok(());
        }
    }
}

/// One connection's steps; whether a `quit` ended the run.
async fn connection(
    pages: &mut [StepPage<'_>],
    report: &mut ScenarioReport,
    stream: UnixStream,
    services: &[&str],
) -> bool {
    let (reading, mut writing) = stream.into_split();
    let mut lines = BufReader::new(reading).lines();
    let mut quit = false;
    while let Ok(Some(line)) = lines.next_line().await {
        let answer = if line.trim() == "quit" {
            quit = true;
            "bye".to_owned()
        } else {
            let (target, line) = addressed(pages, line.trim());
            match target {
                Some(page) => step(page, report, line, services)
                    .await
                    .unwrap_or_else(|error| format!("failed: {error}")),
                None => "failed: no such page".to_owned(),
            }
        };
        if writing.write_all(format!("{answer}\n").as_bytes()).await.is_err() {
            break;
        }
    }
    let _ = writing.shutdown().await;
    quit
}

/// The page `line` goes to (`@<alias> <step>`, else the first) and its step.
fn addressed<'p, 'a, 'l>(pages: &'p mut [StepPage<'a>], line: &'l str) -> (Option<&'p mut StepPage<'a>>, &'l str) {
    if let Some(rest) = line.strip_prefix('@') {
        let (alias, step) = rest.split_once(' ').unwrap_or((rest, ""));
        let alias = alias.parse::<Alias>().ok();
        return (pages.iter_mut().find(|page| Some(page.alias) == alias), step.trim());
    }
    (pages.first_mut(), line)
}

/// One step's answer.
async fn step(
    target: &mut StepPage<'_>,
    report: &mut ScenarioReport,
    line: &str,
    services: &[&str],
) -> Result<String, ScenarioError> {
    let (alias, page, drive) = (target.alias, target.page, &mut *target.drive);
    let (verb, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    match verb {
        "wait" => {
            tokio::time::sleep(Duration::from_secs(rest.parse().unwrap_or(1))).await;
            Ok("waited".to_owned())
        }
        "shot" => Ok(save_screenshot_as(alias, page, rest)
            .await
            .unwrap_or_else(|| "no screenshot".to_owned())),
        "press" => {
            let key = match rest {
                "enter" => Key::ENTER,
                "backspace" => Key::BACKSPACE,
                "space" => Key::SPACE,
                "tab" => Key::TAB,
                "shift+tab" => Key::SHIFT_TAB,
                _ => Key::ESCAPE,
            };
            drive.press("a key", key).await?;
            Ok("pressed".to_owned())
        }
        "go" => {
            drive.navigate("a step's navigation", rest).await?;
            Ok("loaded".to_owned())
        }
        "js" => Ok(drive.evaluate("the page", rest).await?.to_string()),
        "click" => {
            let (css, name) = rest.split_once(' ').unwrap_or((rest, "."));
            Ok(drive.evaluate("a click", &click_named(css, name)).await?.to_string())
        }
        "point" => {
            drive.pointer_click("a pointer's click", Selector::Css(rest)).await?;
            Ok("clicked".to_owned())
        }
        "xy" | "wheel" => at_point(drive, verb, rest).await,
        "hover" => {
            drive.hover("a hover", Selector::Css(rest)).await?;
            Ok("hovered".to_owned())
        }
        "type" => {
            let (css, text) = rest.split_once(' ').unwrap_or((rest, ""));
            drive.type_into("typing", Selector::Css(css), text).await?;
            Ok("typed".to_owned())
        }
        "insert" => {
            drive.insert_text("typing", rest).await?;
            Ok("inserted".to_owned())
        }
        "seen" => Ok(writes_seen(drive, report, &mut target.seen, services).await),
        "hold" => hold(page, rest),
        other => Ok(format!("unknown step {other}")),
    }
}

/// An `xy <x> <y>` click or a `wheel <x> <y> <dy>` turn at a viewport point: what a page selector can't reach, such
/// as a cross-origin frame's controls. A missing or unreadable number is 0.
async fn at_point(drive: &mut Driver, verb: &str, rest: &str) -> Result<String, ScenarioError> {
    let mut words = rest.split_whitespace().map(|word| word.parse().unwrap_or(0.0));
    let (x, y, dy) = (
        words.next().unwrap_or(0.0),
        words.next().unwrap_or(0.0),
        words.next().unwrap_or(0.0),
    );
    if verb == "wheel" {
        drive.wheel_at("a wheel turn", x, y, dy).await?;
        return Ok("scrolled".to_owned());
    }
    drive.pointer_click_at("a pointer's click", x, y).await?;
    Ok("clicked".to_owned())
}

/// A `hold` step: from now on every request of its method whose URL its pattern finds a match in (and whose body holds,
/// or lacks, its text) is held for capture.
fn hold(page: &LabSession, rest: &str) -> Result<String, ScenarioError> {
    let (method, rest) = rest.split_once(' ').unwrap_or(("POST", rest));
    let (pattern, body) = rest.split_once(' ').unwrap_or((rest, ""));
    let mut rule = Hold::new(method, pattern).map_err(|error| ScenarioError::Setup(error.to_string()))?;
    if let Some(text) = body.strip_prefix('+') {
        rule = rule.with_body_containing(text);
    } else if let Some(text) = body.strip_prefix('-') {
        rule = rule.with_body_lacking(text);
    }
    page.hold(rule.labelled(HELD_BY_A_STEP));
    Ok("held".to_owned())
}

/// The requests since `seen` on the services team management uses (never another host, whose bodies can carry a token),
/// each as `<method> <url> -> <status or failure> from <target>`, and the body of one that isn't a plain read unless it
/// holds anything like a token. The target tells a page's request from its worker's: the web client makes its chat
/// and middle-tier calls from a dedicated worker.
async fn writes_seen(drive: &mut Driver, report: &mut ScenarioReport, seen: &mut usize, services: &[&str]) -> String {
    let now = drive.seen_so_far();
    let requests: Vec<_> = drive.requests().get(*seen..now).unwrap_or_default().to_vec();
    *seen = now;
    let mut lines = Vec::new();
    for request in requests {
        let on_a_service = services.iter().any(|service| request.url.contains(service));
        if !on_a_service || matches!(request.method.as_str(), "OPTIONS" | "HEAD") {
            continue;
        }
        if request.method == "GET" {
            lines.push(format!(
                "GET {} -> {} from {}",
                crate::protocol::log_url(&request.url),
                outcome(&request),
                drive.origin(&request)
            ));
            continue;
        }
        let read = drive.read_body("a body", &request).await.unwrap_or(request);
        let body = read.body.as_deref().unwrap_or("");
        let body = if TOKEN_WORDS.iter().any(|word| body.to_ascii_lowercase().contains(word)) {
            "(a body naming a token, left out)"
        } else {
            body
        };
        let url = crate::protocol::log_url(&read.url);
        let origin = drive.origin(&read);
        report.notes.push(format!("seen: {} {url} from {origin}", read.method));
        lines.push(format!(
            "{} {url} -> {} from {origin} {body}",
            read.method,
            outcome(&read)
        ));
    }
    lines.join("\n")
}

/// A seen request's status, or why it failed (a held one: `net::ERR_BLOCKED_BY_CLIENT`), or `pending`.
fn outcome(request: &SeenRequest) -> String {
    match (&request.failed, request.status) {
        (Some(error), _) => format!("failed {error}"),
        (None, Some(status)) => status.to_string(),
        (None, None) => "pending".to_owned(),
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/team_manage_driver_tests.rs"]
mod driver_tests;
