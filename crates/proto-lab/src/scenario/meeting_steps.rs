//! Drives a lab meeting page through the scenario control socket and records its UI/network steps. The hold control installs explicit capture holds.
//! `PROTO_LAB_FAKE_AUDIO` may name a WAV file the fake microphone plays instead of the burst tone.
//! `PROTO_LAB_STEPS_BYSTANDERS` (`b`, or `b,c`) also loads those accounts' pages with their calling stack up, for a step
//! that rings them (Request to join); a step that starts `@b ` goes to b's page.

use std::path::PathBuf;

use crate::browser::Media;
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{CHAT_OPEN, COMPOSE, chat_link, load, one_to_one_id, setup};
use super::ab_meeting::{cancel_scheduled, created_chat, created_join_link, schedule};
use super::ab_trouter::size;
use super::echo_call::{USER_AGENT, write_burst_tone};
use super::team_manage::{StepPage, serve_pages};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The services `seen` looks at: the middle tier, CSA, the chat service, and the call services behind the flight proxy.
const MEETING_SERVICES: [&str; 8] = [
    "/api/mt/",
    "/api/csa/",
    "/api/chatsvc/",
    "api.flightproxy.teams.microsoft.com/",
    ".conv.skype.com/",
    ".cc.skype.com/",
    "/meetingOptions/",
    "/api/v1/meetings",
];

fn alias_of(name: &str, default: Alias) -> Alias {
    match std::env::var(name).as_deref() {
        Ok("a") => Alias::A,
        Ok("b") => Alias::B,
        Ok("c") => Alias::C,
        _ => default,
    }
}

/// Runs `meeting-steps` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, a browser whose capture setup fails, no socket named.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let alias = alias_of("PROTO_LAB_ACCOUNT", Alias::A);
    let peer = alias_of("PROTO_LAB_PEER", if alias == Alias::B { Alias::A } else { Alias::B });
    // `PROTO_LAB_FAKE_AUDIO` names a WAV the microphone plays instead of the tone (speech, for a transcript to hold)
    // (one that names no file stops the run: a meeting with no speech would leave an empty transcript, unexplained)
    let tone = match std::env::var_os("PROTO_LAB_FAKE_AUDIO").map(PathBuf::from) {
        Some(wav) if wav.is_file() => wav,
        Some(wav) => return Err(setup(format!("PROTO_LAB_FAKE_AUDIO names no file: {}", wav.display()))),
        None => write_burst_tone(paths).map_err(|error| setup(format!("can't write the tone: {error}")))?,
    };
    let session_options = SessionOptions {
        capture: Some(ScenarioName::MeetingSteps.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::Fake { audio: Some(tone) },
        user_agent: Some(USER_AGENT.to_owned()),
    };
    let mri = |alias| config.account(alias).mri.clone().unwrap_or_default();
    let (me, them) = (mri(alias), mri(peer));
    let peer_upn = config.account(peer).upn.clone();
    let mut page = LabSession::open_with(paths, alias, session_options.clone()).await?;
    let socket: PathBuf = std::env::var_os("PROTO_LAB_STEPS_SOCKET")
        .map(PathBuf::from)
        .ok_or_else(|| ScenarioError::Setup("PROTO_LAB_STEPS_SOCKET names no socket".to_owned()))?;
    let mut report = ScenarioReport::new(ScenarioName::MeetingSteps);
    let mut bystanders = Vec::new();
    for other in super::ab_call::bystanders(super::ab_call::CallStep::Steps)
        .into_iter()
        .filter(|other| *other != alias)
    {
        match LabSession::open_with(paths, other, session_options.clone()).await {
            Ok(session) => bystanders.push((other, session)),
            Err(error) => report.failures.push(format!("{other}: {error}")),
        }
    }
    let who = Who {
        alias,
        me: &me,
        them: &them,
        peer_upn: &peer_upn,
    };
    let result = match stand_up(&mut bystanders, &mut report, options).await {
        Ok(()) => steps(&who, &mut page, &mut bystanders, &mut report, &socket, options).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        report.failures.push(error.to_string());
    }
    report.close_account(alias, page).await;
    for (other, session) in bystanders {
        report.close_account(other, session).await;
    }
    let _ = std::fs::remove_file(&socket);
    Ok(report)
}

/// The account and its peer.
struct Who<'a> {
    alias: Alias,
    me: &'a str,
    them: &'a str,
    peer_upn: &'a str,
}

/// Opens the account's chat with `them` and waits for its compose box.
async fn open_chat(drive: &mut super::dsl::Driver, them: &str, options: RunOptions) -> Result<(), ScenarioError> {
    drive
        .navigate("the account opens its chat with its peer", &chat_link(them))
        .await?;
    drive
        .wait_for("the compose box", COMPOSE, options.load_timeout.max(CHAT_OPEN))
        .await?;
    Ok(())
}

/// Loads each bystander's page and starts its calling stack, so a ring reaches it.
async fn stand_up(
    bystanders: &mut [(Alias, LabSession)],
    report: &mut ScenarioReport,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    for (other, session) in bystanders.iter_mut() {
        load(*other, session, options.load_timeout).await?;
        size(session).await?;
        let pauses = super::ab_call::CallPauses::default();
        super::ab_call::stand_up_calling_with(*other, &mut session.driver(), report, pauses).await?;
        super::ab_call::wait_calling_socket(*other, session, report).await?;
    }
    Ok(())
}

async fn steps(
    who: &Who<'_>,
    page: &mut LabSession,
    bystanders: &mut [(Alias, LabSession)],
    report: &mut ScenarioReport,
    socket: &std::path::Path,
    options: RunOptions,
) -> Result<(), ScenarioError> {
    let (alias, me, them) = (who.alias, who.me, who.them);
    // Both MRIs are required to open the account's 1:1 chat.
    one_to_one_id(me, them)
        .ok_or_else(|| setup("lab.toml has no MRI for the account or its peer: run `proto-lab login`".to_owned()))?;
    load(alias, page, options.load_timeout).await?;
    size(page).await?;
    let mut drive = page.driver();
    open_chat(&mut drive, them, options).await?;

    let made = match std::env::var("PROTO_LAB_SCHEDULE") {
        Ok(title) if !title.is_empty() => {
            Some(made_meeting(page, &mut drive, report, &title, who.peer_upn, options).await?)
        }
        _ => None,
    };
    // `seen` starts after the initial chat and optional meeting setup
    let served = serve_with(alias, page, bystanders, report, socket).await;
    let to_cancel = made.or_else(|| std::env::var("PROTO_LAB_CANCEL").ok().filter(|chat| !chat.is_empty()));
    if let Some(chat) = to_cancel {
        let mut drive = page.driver();
        let cancelled = cancel_scheduled(page, &mut drive, report, &chat, options).await;
        report.notes.push(match &cancelled {
            Ok(()) => format!("the meeting {chat} was cancelled"),
            Err(error) => format!("the meeting {chat} wasn't cancelled: {error}"),
        });
        cancelled?;
    }
    served
}

/// Takes steps for the account's page and its bystanders' until a `quit`.
async fn serve_with(
    alias: Alias,
    page: &LabSession,
    bystanders: &[(Alias, LabSession)],
    report: &mut ScenarioReport,
    socket: &std::path::Path,
) -> Result<(), ScenarioError> {
    let mut drivers: Vec<_> = std::iter::once((alias, page))
        .chain(bystanders.iter().map(|(other, session)| (*other, session)))
        .map(|(alias, session)| (alias, session, session.driver()))
        .collect();
    let mut pages: Vec<_> = drivers
        .iter_mut()
        .map(|(alias, session, drive)| StepPage::new(*alias, session, drive))
        .collect();
    serve_pages(&mut pages, report, socket, &MEETING_SERVICES).await
}

/// Schedules a meeting of the account's with its peer (`PROTO_LAB_SCHEDULE` its title) and writes its chat and join
/// link to `meeting.txt` in the capture dir, for the steps to go on with; its chat, which the run cancels as it ends.
async fn made_meeting(
    page: &mut LabSession,
    drive: &mut super::dsl::Driver,
    report: &mut ScenarioReport,
    title: &str,
    upn: &str,
    options: RunOptions,
) -> Result<String, ScenarioError> {
    let body = schedule(page, drive, report, title, upn, options).await?;
    let link = created_join_link(body.as_deref()).ok_or_else(|| setup("the creation named no join link".to_owned()))?;
    let chat = created_chat(body.as_deref()).ok_or_else(|| setup("the creation named no chat".to_owned()))?;
    if let Some(dir) = page.capture_dir() {
        std::fs::write(dir.join("meeting.txt"), format!("{chat}\n{link}\n"))
            .map_err(|error| setup(format!("can't write meeting.txt: {error}")))?;
    }
    report.notes.push(format!("scheduled the meeting {chat}"));
    Ok(chat)
}

#[cfg(test)]
#[path = "../../tests/support/scenario/meeting_steps_driver_tests.rs"]
mod driver_tests;
