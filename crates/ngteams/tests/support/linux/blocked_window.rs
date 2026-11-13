//! Messages a tenant's policy blocked, as a user sees them in the app's window in the UI sandbox (`harness = false`:
//! GPUI runs on the process's main thread).
//!
//! The parent runs the mock (`teams-mock`) and drives what the chat service tells a's push channel in the synthetic
//! shapes of `tests/fixtures/chatsvc/policy-blocked.json`: a blocked message's update (empty `content`, no files,
//! `properties.policyViolation`), the service's copy of the account's own in its stream `48:generic`, that stream's
//! `ConversationUpdate`, and the later update that adds `atp`. It runs the app (a's engine on that mock, as `ngteams`
//! hands its own to the window) as a child in the sandbox with a control socket, and reads it as a user would see it:
//! the chat view the child describes on request, `ngteamsctl`'s chats, the sandbox's notifications and URI opener,
//! the sandbox's keyboard and pointer, and screenshots kept under `target/ui-shots/blocked-*.png`.
//!
//! - **The account's own, live.** A message of a's with a `.deb` file in the a–b chat, blocked: the update first (its
//!   text and file kept from the store, the copy's read finds none), then the copy, then the `atp` update. It shows its
//!   text and file under Teams' red notice ("…a file type that is not permitted.") and "Learn about file and link
//!   safety", never "Edited", in Teams Light and Dark; the link, clicked, opens Microsoft's page. A message a types and
//!   sends in a group chat of a and b is then blocked the other way round (the copy first) and shows the same, its
//!   chat's preview its text. (The a–b chat's fixtures are newer than the mock's clock, so its preview is theirs.)
//! - **Someone else's.** b's messages in a group chat: a policy's block shows only "…blocked due to organization
//!   policy." with "What's this?" (opening Microsoft's page), a file type's its line and link, never their text; a
//!   harmful link and phishing show their warnings over the text; a flag shows to its sender only ("This message was
//!   flagged." on a's own, nothing on b's). A reply to the withheld message quotes the notice's line and previews its
//!   own text.
//! - **Withheld before it came.** b's message that comes withheld to a chat not open notifies, and the chat list
//!   previews it, with the notice's line.
//! - **A restart between the copy and the block** (in that group chat). A copy kept before the app quit puts the text
//!   back into a message
//!   that came blocked after the restart; a message whose copy the service still has gets it from the copy's read; one
//!   whose copy the service has lost shows the notice alone (the chat list previewing its line) until the copy comes.
//! - **`48:generic` is no chat.** It is never listed, never read as a thread, and nothing but its copies is read from
//!   it.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_ctl::protocol::{ChatRow, OpenTarget, Request, Response};
use teams_ctl::{Client, Timeouts};
use teams_mock::{
    AB_CHAT, ABC_CHAT, CHAT_BASE, CSA_BASE, ChatEvent, ChatServiceMock, EventKind, LAB_A, LAB_B, TrouterMock,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView};
use teams_ui::compose::Mode;
use teams_ui::policy_notice;
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "NGTEAMS_BLOCKED_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_BLOCKED_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_BLOCKED_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_BLOCKED_TROUTER";
const TOKEN_ENV: &str = "NGTEAMS_BLOCKED_TOKEN";
const DIR_ENV: &str = "NGTEAMS_BLOCKED_DIR";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "blocked-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// How long a state that should stay is watched.
const QUIET: Duration = Duration::from_millis(1500);
/// Where the conversation pane starts, right of the rail and the chat list, in output pixels.
const PANE_X: u32 = 420;

// Teams' words, as the web client shows them.
const FILE_TYPE_LINE: &str = "This message was blocked because it contains a file type that is not permitted.";
const FILE_TYPE_LINK: &str = "Learn about file and link safety";
const OTHERS_LINE: &str = "This message was blocked due to organization policy.";
const OTHERS_LINK: &str = "What's this?";
const HARMFUL_LINK_LINE: &str = "This message contains a link that might be harmful.";
const PHISHING_LINE: &str = "This message could be a phishing attempt.";
const FLAGGED_LINE: &str = "This message was flagged.";
/// Microsoft's pages the links open.
const FILE_SAFETY_PAGE: &str = "https://go.microsoft.com/fwlink/?linkid=2293545";
const POLICY_PAGE: &str = "https://go.microsoft.com/fwlink/?LinkId=2132837";

/// The file every blocked message of a's carries.
const PACKAGE: &str = "made-up-tool_0.1.0_amd64.deb";
const FILED_HTML: &str = "<p>sudo apt install ./made-up-tool_0.1.0_amd64.deb</p>\r\n<p>sudo made-up-tool --setup</p>";
const FILED_TEXT: &str = "sudo apt install ./made-up-tool_0.1.0_amd64.deb\nsudo made-up-tool --setup";
/// What a types and sends.
const TYPED: &str = "sudo dpkg -i made-up-tool_0.1.0_amd64.deb";
const REPLY: &str = "Made-up reply";
const R1_HTML: &str = "<p>Made-up package before the restart</p>";
const R2_HTML: &str = "<p>Made-up package the service lost</p>";
const R3_HTML: &str = "<p>Made-up package read back</p>";

/// The ids of the service's copies in `48:generic`.
const COPY_FILED: &str = "1760000500901";
const COPY_TYPED: &str = "1760000500902";
const COPY_R1: &str = "1760000500903";
const COPY_R2: &str = "1760000500904";
const COPY_R3: &str = "1760000500905";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("blocked_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started, for a failure's log.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams blocked messages", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    if let Err(error) = walk(&sandbox, &runtime, &service, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    say("ngteams blocked messages: ok");
    Ok(())
}

/// The whole walk: the first run, the app quit, the service's changes while it is down, the second run.
fn walk(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir)?;
    let first = (|| -> SandboxResult {
        own_blocked_update_first(&mut run, sandbox, service)?;
        typed_and_blocked_copy_first(&mut run, service)?;
        someone_else_s(&mut run, sandbox, service)?;
        reply_quotes_the_notice(&mut run, sandbox, service)?;
        withheld_notifies(&mut run, sandbox, service)?;
        // the copy of a message a sends from another client while this app is down, kept before it quits
        service.push_copy(COPY_R1, R1_HTML, true);
        thread::sleep(QUIET);
        generic_is_no_chat(&mut run, runtime, service)
    })();
    if let Err(error) = first {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)?;
    step("quit");
    let offline = service.while_down(runtime)?;
    let mut run = AppRun::launch(sandbox, service, dir)?;
    let second = (|| -> SandboxResult {
        after_the_restart(&mut run, sandbox, service, &offline)?;
        generic_is_no_chat(&mut run, runtime, service)
    })();
    if let Err(error) = second {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// a's own message with its file, blocked live: the update before the copy (see the module docs).
fn own_blocked_update_first(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    run.open(AB_CHAT)?;
    let filed = &service.filed;
    run.until("a's message as sent", |state| {
        state
            .item(&filed.id)
            .is_some_and(|item| item.text == FILED_TEXT && item.files == [PACKAGE] && item.notice.is_none())
    })?;
    let version = filed.at + 1_500;
    let blocked = service.block(AB_CHAT, filed, &verdict("wft", 1, Some(COPY_FILED)), version, false)?;
    service.deliver(AB_CHAT, LAB_A, EventKind::Edit, blocked);
    let own_blocked = |item: &Item| {
        item.text == FILED_TEXT && item.files == [PACKAGE] && item.has_notice(FILE_TYPE_LINE, FILE_TYPE_LINK, true)
    };
    run.until("a's blocked message with its text kept", |state| {
        state.item(&filed.id).is_some_and(own_blocked)
    })?;
    service.push_copy(COPY_FILED, FILED_HTML, true);
    let atp = service.add_atp(AB_CHAT, filed, version + 2_000)?;
    service.deliver(AB_CHAT, LAB_A, EventKind::Edit, atp);
    let state = run.stays("a's blocked message after its copy and atp", |state| {
        state.item(&filed.id).is_some_and(own_blocked)
    })?;
    no_edited(&state, &filed.id)?;
    step("a's own blocked message shows its text, file and notice");
    run.shot(sandbox, "blocked-own-light.png")?;
    run.theme(true)?;
    run.shot(sandbox, "blocked-own-dark.png")?;
    run.theme(false)?;
    let page = click_link(run, sandbox, teams_ui::theme::LIGHT_PALETTE.brand, 150)?;
    if page != FILE_SAFETY_PAGE {
        return Err(format!("the notice's link opened {page}").into());
    }
    step("the notice's link opens Microsoft's page");
    let row = run.ctl_message(AB_CHAT, &filed.id)?;
    if !row.0.contains("sudo made-up-tool --setup") || row.1 {
        return Err(format!("ngteamsctl reads a's blocked message as {row:?}").into());
    }
    Ok(())
}

/// What a types and sends, blocked with the copy first.
fn typed_and_blocked_copy_first(run: &mut AppRun, service: &Service) -> SandboxResult {
    let release = service.release.as_str();
    run.open(release)?;
    let state = run.until("the compose box focused", |state| state.composing)?;
    let before: Vec<String> = state.items.iter().map(|item| item.id.clone()).collect();
    run.input.type_text(TYPED)?;
    run.input.key(keys::ENTER)?;
    let state = run.until("a's typed message sent", |state| {
        state
            .items
            .iter()
            .any(|item| !before.contains(&item.id) && item.own && item.text == TYPED)
    })?;
    let id = state
        .items
        .iter()
        .find(|item| !before.contains(&item.id) && item.text == TYPED)
        .map(|item| item.id.clone())
        .ok_or("no typed message")?;
    let sent = service.sent_message(release, TYPED, &id)?;
    service.push_copy(COPY_TYPED, &format!("<p>{TYPED}</p>"), false);
    let blocked = service.block(
        release,
        &sent,
        &verdict("wft", 1, Some(COPY_TYPED)),
        sent.at + 1_500,
        false,
    )?;
    service.deliver(release, LAB_A, EventKind::Edit, blocked);
    let state = run.stays("a's typed message blocked after its copy", |state| {
        state.item(&id).is_some_and(|item| {
            item.text == TYPED && item.files.is_empty() && item.has_notice(FILE_TYPE_LINE, FILE_TYPE_LINK, true)
        })
    })?;
    no_edited(&state, &id)?;
    let preview = run.preview(release)?;
    if preview.as_deref() != Some(&format!("You: {TYPED}")) {
        return Err(format!("the chat list previews a's blocked message as {preview:?}").into());
    }
    step("a's typed message, blocked with the copy first, keeps its text");
    Ok(())
}

/// b's messages in the group chat, blocked, warned of and flagged; a's own flagged.
fn someone_else_s(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    let group = &service.group;
    run.open(&group.chat)?;
    run.until("b's messages as sent", |state| {
        state
            .item(&group.dlp.id)
            .is_some_and(|item| item.text == group::DLP_TEXT && item.notice.is_none())
    })?;
    // the policy's block first, alone: its link is the only one on the screen
    let blocked = service.block(
        &group.chat,
        &group.dlp,
        &verdict("dlp", 1, None),
        group.dlp.at + 1_500,
        false,
    )?;
    service.deliver(&group.chat, LAB_B, EventKind::Edit, blocked);
    run.until("b's message blocked by a policy", |state| {
        state.item(&group.dlp.id).is_some_and(|item| {
            item.text.is_empty() && item.files.is_empty() && item.has_notice(OTHERS_LINE, OTHERS_LINK, false)
        })
    })?;
    let page = click_link(run, sandbox, teams_ui::theme::LIGHT_PALETTE.brand, 40)?;
    if page != POLICY_PAGE {
        return Err(format!("\"What's this?\" opened {page}").into());
    }
    step("someone else's blocked message shows the notice alone; its link opens Microsoft's page");
    let changes = [
        (&group.wft, verdict("wft", 1, None), false),
        (&group.link, verdict("mu", 2, None), true),
        (&group.phishing, verdict("pd", 2, None), true),
        (&group.flagged, verdict("dlp", 2, None), true),
        (&group.own_flagged, verdict("dlp", 2, None), true),
    ];
    for (message, verdict, keep) in changes {
        let update = service.block(&group.chat, message, &verdict, message.at + 1_500, keep)?;
        service.deliver(&group.chat, &message.from, EventKind::Edit, update);
    }
    let state = run.stays("the group's verdicts", |state| {
        let item = |id: &str| state.item(id);
        item(&group.wft.id).is_some_and(|item| {
            item.text.is_empty() && item.files.is_empty() && item.has_notice(FILE_TYPE_LINE, FILE_TYPE_LINK, true)
        }) && item(&group.link.id).is_some_and(|item| {
            item.text == group::LINK_TEXT && item.has_notice(HARMFUL_LINK_LINE, FILE_TYPE_LINK, true)
        }) && item(&group.phishing.id).is_some_and(|item| {
            item.text == group::PHISHING_TEXT
                && item
                    .notice
                    .as_ref()
                    .is_some_and(|notice| notice.line == PHISHING_LINE && notice.link.is_none() && notice.red)
        }) && item(&group.flagged.id).is_some_and(|item| item.text == group::FLAGGED_TEXT && item.notice.is_none())
            && item(&group.own_flagged.id).is_some_and(|item| {
                item.text == group::OWN_FLAGGED_TEXT
                    && item
                        .notice
                        .as_ref()
                        .is_some_and(|notice| notice.line == FLAGGED_LINE && notice.red)
            })
    })?;
    for message in [&group.dlp, &group.wft, &group.link, &group.phishing, &group.own_flagged] {
        no_edited(&state, &message.id)?;
    }
    step("warnings, phishing and the sender-only flag show as Teams shows them");
    run.shot(sandbox, "blocked-others-light.png")?;
    run.theme(true)?;
    run.shot(sandbox, "blocked-others-dark.png")?;
    run.theme(false)
}

/// a replies to b's withheld message: the reply's quote reads the notice's line, in the box and as sent.
fn reply_quotes_the_notice(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    let group = &service.group;
    run.until("the compose box focused", |state| state.composing)?;
    run.input.key(keys::ESC)?;
    let mut selected = false;
    for _ in 0..12 {
        run.input.key(keys::UP)?;
        thread::sleep(Duration::from_millis(150));
        if run.state()?.selected.as_deref() == Some(group.dlp.id.as_str()) {
            selected = true;
            break;
        }
    }
    if !selected {
        return Err(format!("Up never reached b's withheld message: {:?}", run.state()?.selected).into());
    }
    run.input.type_text("r")?;
    run.until("the reply's quote", |state| state.reply.as_deref() == Some(OTHERS_LINE))?;
    run.shot(sandbox, "blocked-reply.png")?;
    run.input.type_text(REPLY)?;
    run.input.key(keys::ENTER)?;
    let state = run.until("a's reply", |state| {
        state.items.iter().any(|item| item.own && item.text == REPLY)
    })?;
    let reply = state
        .items
        .iter()
        .find(|item| item.own && item.text == REPLY)
        .ok_or("no reply")?;
    if reply.quote != OTHERS_LINE {
        return Err(format!("a's reply quotes {:?}", reply.quote).into());
    }
    let sent = service.sent_content(&group.chat, REPLY)?;
    if !sent.contains(OTHERS_LINE) || sent.contains(group::DLP_TEXT) {
        return Err(format!("the reply as sent: {sent}").into());
    }
    let expected = format!("You: {REPLY}");
    let preview = wait_until(WAIT, || {
        run.preview(&group.chat)
            .ok()
            .flatten()
            .filter(|preview| preview == &expected)
    });
    if preview.is_none() {
        return Err(format!("the chat list previews a's reply as {:?}", run.preview(&group.chat)?).into());
    }
    run.shot(sandbox, "blocked-reply-sent.png")?;
    run.theme(true)?;
    run.shot(sandbox, "blocked-reply-sent-dark.png")?;
    run.theme(false)?;
    step("a reply quotes the notice and previews its own text");
    Ok(())
}

/// b's message that comes withheld to a chat not open: its notification and its chat's preview read the notice.
fn withheld_notifies(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    let resource = service.withheld_in_abc()?;
    service.deliver(ABC_CHAT, LAB_B, EventKind::Message, resource);
    let notification = wait_until(WAIT, || {
        sandbox
            .bus()
            .notifications()
            .into_iter()
            .find(|notification| notification.body == OTHERS_LINE)
    })
    .ok_or_else(|| format!("no notification read the notice: {:?}", sandbox.bus().notifications()))?;
    if notification.body.contains(group::HELLO_TEXT) {
        return Err(format!("the notification: {notification:?}").into());
    }
    let preview = wait_until(WAIT, || {
        run.preview(ABC_CHAT)
            .ok()
            .flatten()
            .filter(|preview| preview == OTHERS_LINE)
    });
    if preview.is_none() {
        return Err(format!(
            "the chat list previews the withheld message as {:?}",
            run.preview(ABC_CHAT)?
        )
        .into());
    }
    step("a withheld message notifies and previews with the notice's line");
    Ok(())
}

/// What the service's stream of copies left: no chat, no thread read, nothing read but copies.
fn generic_is_no_chat(run: &mut AppRun, runtime: &tokio::runtime::Runtime, service: &Service) -> SandboxResult {
    const STREAM: [&str; 2] = ["48:generic", "48%3Ageneric"];
    let listed: Vec<String> = run.chats()?.into_iter().map(|row| row.id).collect();
    if listed.iter().any(|id| STREAM.contains(&id.as_str())) {
        return Err(format!("ngteamsctl chats lists the copies' stream: {listed:?}").into());
    }
    run.trace.drain();
    let shown = run.trace.seen.iter().find(|line| {
        line["view"] == "chat-list"
            && line["conversations"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id.as_str().is_some_and(|id| STREAM.contains(&id))))
    });
    if let Some(line) = shown {
        return Err(format!("the chat list showed the copies' stream: {line}").into());
    }
    let requests = runtime.block_on(service.mock.received_requests());
    let stray: Vec<String> = requests
        .iter()
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .filter(|line| STREAM.iter().any(|stream| line.contains(stream)))
        .filter(|line| {
            let copy_read = line.starts_with("GET ")
                && [COPY_FILED, COPY_TYPED, COPY_R1, COPY_R2, COPY_R3]
                    .iter()
                    .any(|copy| line.ends_with(&format!("/messages/{copy}")));
            !copy_read
        })
        .collect();
    if !stray.is_empty() {
        return Err(format!("the copies' stream was reached as a chat: {stray:?}").into());
    }
    step("48:generic is no chat and was never read as a thread");
    Ok(())
}

/// After the restart: the copy kept before it, the copy's read, and a copy lost, then pushed.
fn after_the_restart(run: &mut AppRun, sandbox: &Sandbox, service: &Service, offline: &Offline) -> SandboxResult {
    let release = service.release.as_str();
    run.open(release)?;
    let restored = |item: &Item, text: &str| {
        item.text == text && item.files == [PACKAGE] && item.has_notice(FILE_TYPE_LINE, FILE_TYPE_LINK, true)
    };
    let state = run.until("the blocked messages after the restart", |state| {
        state
            .item(&offline.kept.id)
            .is_some_and(|item| restored(item, "Made-up package before the restart"))
            && state
                .item(&offline.read.id)
                .is_some_and(|item| restored(item, "Made-up package read back"))
            && state.item(&offline.lost.id).is_some_and(|item| {
                item.text.is_empty() && item.files.is_empty() && item.has_notice(FILE_TYPE_LINE, FILE_TYPE_LINK, true)
            })
    })?;
    for message in [&offline.kept, &offline.read, &offline.lost] {
        no_edited(&state, &message.id)?;
    }
    let preview = run.preview(release)?;
    if preview.as_deref() != Some(&format!("You: {FILE_TYPE_LINE}")) {
        return Err(format!("the chat list previews the message whose copy is lost as {preview:?}").into());
    }
    run.shot(sandbox, "blocked-restart.png")?;
    step("after the restart: the kept copy and the copy's read put the text back; a lost one shows the notice");
    service.push_copy(COPY_R2, R2_HTML, true);
    run.until("the lost copy, pushed", |state| {
        state
            .item(&offline.lost.id)
            .is_some_and(|item| restored(item, "Made-up package the service lost"))
    })?;
    let preview = wait_until(WAIT, || {
        run.preview(release)
            .ok()
            .flatten()
            .filter(|preview| preview == "You: Made-up package the service lost")
    });
    if preview.is_none() {
        return Err(format!(
            "the chat list previews the restored message as {:?}",
            run.preview(release)?
        )
        .into());
    }
    run.shot(sandbox, "blocked-restart-restored.png")?;
    step("a copy that comes after the restart puts its text back");
    Ok(())
}

fn no_edited(state: &State, id: &str) -> SandboxResult {
    match state.item(id) {
        Some(item) if item.edited => Err(format!("message {id} shows Edited").into()),
        Some(_) => Ok(()),
        None => Err(format!("message {id} isn't shown").into()),
    }
}

/// Clicks the lowest link drawn in `colour` in the conversation pane (its underline at least `min` px long): the
/// newest message's, the one the caller just blocked; the page the app asked the opener for.
fn click_link(run: &mut AppRun, sandbox: &Sandbox, colour: u32, min: u32) -> SandboxResult<String> {
    let shot = run.shot(sandbox, "blocked-link.png")?;
    let lines = underlines(&shot, colour, min)?;
    let Some(&(x0, x1, y)) = lines.iter().max_by_key(|(_, _, y)| *y) else {
        return Err(format!("no link underlined in the pane ({})", shot.display()).into());
    };
    let before = sandbox.opened()?.len();
    run.input.click(x0.midpoint(x1), y.saturating_sub(6), keys::BTN_LEFT)?;
    let opened = wait_until(WAIT, || {
        let opened = sandbox.opened().ok()?;
        opened.get(before).cloned()
    })
    .ok_or("the link opened nothing")?;
    run.input.move_to(900, 24)?;
    opened
        .args
        .last()
        .cloned()
        .ok_or_else(|| format!("the opener got no address: {opened:?}").into())
}

/// The horizontal runs of `colour` (within a few steps on each channel) at least `min` px long right of [`PANE_X`]:
/// `(x0, x1, y)` each, merged across neighbouring rows (an underline may be two rows thick).
fn underlines(png: &Path, colour: u32, min: u32) -> SandboxResult<Vec<(u32, u32, u32)>> {
    let wanted = colour.to_be_bytes();
    let shot = image::open(png)?.to_rgb8();
    let near = |x: u32, y: u32| {
        shot.get_pixel(x, y)
            .0
            .iter()
            .zip(&wanted[1..])
            .all(|(got, want)| got.abs_diff(*want) <= 12)
    };
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    // below the conversation's header and its tabs, above the compose box
    for y in 100..shot.height().saturating_sub(90) {
        let mut x = PANE_X;
        while x < shot.width() {
            if !near(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < shot.width() && near(x, y) {
                x += 1;
            }
            if x - start >= min {
                let merged = runs
                    .iter_mut()
                    .find(|(x0, x1, y0)| y - *y0 <= 2 && x0.abs_diff(start) <= 2 && x1.abs_diff(x) <= 2);
                if merged.is_none() {
                    runs.push((start, x, y));
                }
            }
        }
    }
    Ok(runs)
}

// --- the service ---

/// A message on the mock: its id, client message id, sender and arrival (ms).
#[derive(Clone, Debug)]
struct Posted {
    id: String,
    cmid: String,
    from: String,
    at: u64,
}

/// The group chat of a and b with b's messages the verdicts change.
struct Group {
    chat: String,
    link: Posted,
    phishing: Posted,
    flagged: Posted,
    own_flagged: Posted,
    wft: Posted,
    dlp: Posted,
    hello: Posted,
}

mod group {
    pub const LINK_HTML: &str =
        "<p>Made-up notes at <a href=\"https://example.test/made-up\">https://example.test/made-up</a></p>";
    pub const LINK_TEXT: &str = "Made-up notes at https://example.test/made-up";
    pub const PHISHING_TEXT: &str = "Made-up sign-in check";
    pub const FLAGGED_TEXT: &str = "Made-up figures, flagged";
    pub const OWN_FLAGGED_TEXT: &str = "Made-up budget line";
    pub const WFT_TEXT: &str = "Made-up build attached";
    pub const DLP_TEXT: &str = "Made-up figures for the quarter";
    pub const HELLO_TEXT: &str = "Made-up hello";
}

/// What the service holds once the app quit: a's messages sent from another client meanwhile, already blocked.
struct Offline {
    /// Its copy was kept before the quit.
    kept: Posted,
    /// Its copy the service answers a read of.
    read: Posted,
    /// Its copy the service no longer has, pushed later.
    lost: Posted,
}

struct Service {
    runtime: tokio::runtime::Handle,
    mock: ChatServiceMock,
    trouter: TrouterMock,
    /// The `.deb`'s `properties.files`, as a message carries it.
    files: String,
    /// a's message with its file in the a–b chat.
    filed: Posted,
    /// A group chat of a and b where a's newest messages are those the test sends and blocks (the a–b chat's
    /// fixtures are newer than the mock's clock).
    release: String,
    group: Group,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let files = package_files();
        let own = post(&mock, LAB_A, AB_CHAT, FILED_HTML)?;
        let attached = mock.change_message(AB_CHAT, &own.cmid, |message| {
            message["properties"]["files"] = json!(files);
        });
        if !attached {
            return Err("a's message isn't in the mock".into());
        }
        let release = mock
            .add_group("Made-up release", &[LAB_A, LAB_B])
            .ok_or("no group made")?;
        post(&mock, LAB_B, &release, "<p>Made-up release starts here</p>")?;
        let chat = mock
            .add_group("Made-up policy checks", &[LAB_A, LAB_B])
            .ok_or("no group made")?;
        let group = Group {
            link: post(&mock, LAB_B, &chat, group::LINK_HTML)?,
            phishing: post(&mock, LAB_B, &chat, &format!("<p>{}</p>", group::PHISHING_TEXT))?,
            flagged: post(&mock, LAB_B, &chat, &format!("<p>{}</p>", group::FLAGGED_TEXT))?,
            own_flagged: post(&mock, LAB_A, &chat, &format!("<p>{}</p>", group::OWN_FLAGGED_TEXT))?,
            wft: post(&mock, LAB_B, &chat, &format!("<p>{}</p>", group::WFT_TEXT))?,
            dlp: post(&mock, LAB_B, &chat, &format!("<p>{}</p>", group::DLP_TEXT))?,
            hello: post(&mock, LAB_B, ABC_CHAT, &format!("<p>{}</p>", group::HELLO_TEXT))?,
            chat,
        };
        Ok(Self {
            runtime: runtime.handle().clone(),
            mock,
            trouter,
            files,
            filed: own,
            release,
            group,
        })
    }

    /// Sets `verdict` on `message` as its update gives it at `version` (its text and files taken out unless `keep`);
    /// the update.
    fn block(&self, chat: &str, message: &Posted, verdict: &Value, version: u64, keep: bool) -> SandboxResult<Value> {
        let mut changed = None;
        let found = self.mock.change_message(chat, &message.cmid, |stored| {
            if !keep {
                stored["content"] = json!("");
                if let Some(properties) = stored["properties"].as_object_mut() {
                    properties.remove("files");
                }
            }
            stored["properties"]["mentions"] = json!("[]");
            stored["properties"]["cards"] = json!("[]");
            stored["properties"]["policyViolation"] = verdict.clone();
            stored["properties"]["edittime"] = json!(version.to_string());
            stored["version"] = json!(version.to_string());
            changed = Some(stored.clone());
        });
        changed
            .filter(|_| found)
            .ok_or_else(|| format!("message {} isn't in the mock", message.id).into())
    }

    /// The later update of a blocked message, its `atp` added at `version` (its edit time kept).
    fn add_atp(&self, chat: &str, message: &Posted, version: u64) -> SandboxResult<Value> {
        let mut changed = None;
        let found = self.mock.change_message(chat, &message.cmid, |stored| {
            let atp = json!([{"URL": format!("https://contoso-my.sharepoint.com/personal/made_up/{PACKAGE}"),
                "Xsdata": "made-up", "Sdata": "made-up"}]);
            stored["properties"]["atp"] = json!(atp.to_string());
            stored["version"] = json!(version.to_string());
            changed = Some(stored.clone());
        });
        changed
            .filter(|_| found)
            .ok_or_else(|| format!("message {} isn't in the mock", message.id).into())
    }

    /// Tells a's push channel `resource` changed in `chat`, by `from`.
    fn deliver(&self, chat: &str, from: &str, kind: EventKind, resource: Value) {
        self.mock.queue_event(
            LAB_A,
            ChatEvent {
                conversation: chat.to_owned(),
                from: from.to_owned(),
                kind,
                resource,
            },
        );
    }

    /// The service's copy of a's blocked message `html` (with the `.deb` when `filed`), as a's push channel gets it:
    /// the stream's `ConversationUpdate`, then the copy's `NewMessage`.
    fn push_copy(&self, id: &str, html: &str, filed: bool) {
        let stream = json!({
            "id": "48:generic", "type": "Conversation", "version": 1_760_000_100_800_u64, "lastMessage": {},
            "properties": {},
            "threadProperties": {
                "originalThreadId": "19:teamsstream_generic_00000000-0000-4000-8000-00000000000a@thread.v2",
                "threadType": "streamofgeneric", "isCreator": true,
            },
        });
        self.deliver("48:generic", LAB_A, EventKind::ConversationUpdate, stream);
        let copy = copy_of(id, html, filed.then_some(self.files.as_str()));
        self.deliver("48:generic", LAB_A, EventKind::Message, copy);
    }

    /// The message a sent to `chat` with `text`: its id as the app shows it, its client message id from the post.
    fn sent_message(&self, chat: &str, text: &str, id: &str) -> SandboxResult<Posted> {
        let cmid = self.sent(chat, text)?["clientmessageid"]
            .as_str()
            .ok_or("the post carried no client message id")?
            .to_owned();
        Ok(Posted {
            id: id.to_owned(),
            cmid,
            from: LAB_A.to_owned(),
            at: id.parse()?,
        })
    }

    /// The content a sent to `chat` in its post holding `text`.
    fn sent_content(&self, chat: &str, text: &str) -> SandboxResult<String> {
        Ok(self.sent(chat, text)?["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned())
    }

    /// The body of a's newest post to `chat` holding `text`.
    fn sent(&self, chat: &str, text: &str) -> SandboxResult<Value> {
        let segment = chat.replace(':', "%3A").replace('@', "%40");
        let requests = self.runtime.block_on(self.mock.received_requests());
        requests
            .iter()
            .rev()
            .filter(|request| request.method.as_str() == "POST")
            .filter(|request| {
                let path = request.url.path();
                path.ends_with(&format!("/conversations/{segment}/messages"))
                    || path.ends_with(&format!("/conversations/{chat}/messages"))
            })
            .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
            .find(|body| body["content"].as_str().is_some_and(|content| content.contains(text)))
            .ok_or_else(|| format!("a never posted {text:?} to {chat}").into())
    }

    /// b's message to the a–b–c chat, withheld before it reached a: stored as its newest and returned to deliver.
    fn withheld_in_abc(&self) -> SandboxResult<Value> {
        let hello = &self.group.hello;
        let at = hello.at + 100_000;
        let mut made = None;
        let copied = self.mock.copy_message(ABC_CHAT, &hello.cmid, |message| {
            let time = iso(at);
            message["id"] = json!(at.to_string());
            message["version"] = json!(at.to_string());
            message["clientmessageid"] = json!((9_100_000_000_000_000_000_u64 + at).to_string());
            message["composetime"] = json!(time);
            message["originalarrivaltime"] = json!(time);
            message["content"] = json!("");
            message["properties"]["policyViolation"] = verdict("dlp", 1, None);
            made = Some(message.clone());
        });
        made.filter(|_| copied)
            .ok_or_else(|| "b's hello isn't in the mock".into())
    }

    /// While the app is down, a sends three messages with the `.deb` from another client, each blocked at once; the
    /// service answers a read of one's copy (see [`Offline`]).
    fn while_down(&self, runtime: &tokio::runtime::Runtime) -> SandboxResult<Offline> {
        let add = |copy: &str| -> SandboxResult<Posted> {
            let id = self
                .mock
                .add_message(&self.release, (LAB_A, "bot-a"), "RichText/Html", "")
                .ok_or("a's message wasn't added")?;
            let at: u64 = id.parse()?;
            let message = Posted {
                cmid: (9_000_000_000_000_000_000_u64 + at).to_string(),
                id,
                from: LAB_A.to_owned(),
                at,
            };
            self.block(
                &self.release,
                &message,
                &verdict("wft", 1, Some(copy)),
                at + 1_500,
                false,
            )?;
            Ok(message)
        };
        let kept = add(COPY_R1)?;
        let read = add(COPY_R3)?;
        let lost = add(COPY_R2)?;
        let copy = copy_of(COPY_R3, R3_HTML, Some(&self.files));
        runtime.block_on(
            self.mock
                .answer_next("GET", &format!("/messages/{COPY_R3}"), 200, &copy),
        );
        Ok(Offline { kept, read, lost })
    }
}

fn post(mock: &ChatServiceMock, from: &str, chat: &str, html: &str) -> SandboxResult<Posted> {
    let id = mock.post_as(from, chat, html).ok_or("a post refused")?;
    let at: u64 = id.parse()?;
    Ok(Posted {
        cmid: (9_000_000_000_000_000_000_u64 + at).to_string(),
        id,
        from: from.to_owned(),
        at,
    })
}

/// A verdict of `kind` in `state` (1 blocked, 2 a warning), naming the copy `copy`.
fn verdict(kind: &str, state: u64, copy: Option<&str>) -> Value {
    let mut verdict = json!({
        "verdictDetails": 0, "dlpAction": 2, "state": state, "violationType": kind,
        "justificationText": "MadeUpHandler",
    });
    if let Some(copy) = copy {
        verdict["genericStreamItemId"] = json!(copy.parse::<u64>().unwrap_or_default());
    }
    verdict
}

/// The service's copy `id` of a's blocked message `html` in `48:generic`, with `files` when it had any.
fn copy_of(id: &str, html: &str, files: Option<&str>) -> Value {
    let mut properties = json!({"importance": "", "subject": "", "languageStamp": "languages=en:100;length=30;"});
    if let Some(files) = files {
        properties["files"] = json!(files);
    }
    let time = iso(id.parse().unwrap_or_default());
    json!({
        "id": id, "version": id, "clientmessageid": format!("71{id}"), "messagetype": "RichText/Html",
        "contenttype": "Text", "type": "Message", "content": html,
        "from": format!("{CHAT_BASE}/v1/users/ME/contacts/{LAB_A}"), "imdisplayname": null,
        "composetime": time, "originalarrivaltime": time, "sequenceId": 1,
        "conversationLink": format!("{CHAT_BASE}/v1/users/ME/conversations/48:generic"),
        "to": "48:generic", "threadtype": "streamofgeneric", "isactive": true, "properties": properties,
    })
}

/// `ms` as the service writes a time.
fn iso(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(i64::try_from(ms).unwrap_or_default())
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3f0000Z")
        .to_string()
}

/// The `.deb`'s entry in `properties.files`, as `tests/fixtures/chatsvc/policy-blocked.json` has it, filled in.
fn package_files() -> String {
    let base = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";
    let object = format!("{base}Documents/Microsoft Teams Chat Files/{PACKAGE}");
    let id = "00000000-0000-4000-8000-0000000000f1";
    json!([{
        "itemid": id, "fileName": PACKAGE, "fileType": "deb",
        "fileInfo": {
            "itemId": null, "fileUrl": object, "siteUrl": base, "serverRelativeUrl": "",
            "shareUrl": "https://contoso-my.sharepoint.com/:u:/g/personal/made_up_user_contoso_test/MadeUpShareLinkForTheToolPackage01",
            "shareId": "00000000-0000-4000-8000-0000000000f2",
        },
        "fileChicletState": {"serviceName": "p2p", "state": "active"},
        "@type": "http://schema.skype.com/File", "version": 2, "id": id, "baseUrl": base, "objectUrl": object,
        "openUrl": null, "type": "deb", "title": PACKAGE, "state": "active", "chicletBreadcrumbs": null,
        "providerData": "", "botFileProperties": {}, "isUploadError": null, "progressComplete": null,
        "permissionScope": "Anyone", "filePreview": {"previewUrl": "", "previewHeight": 0, "previewWidth": 0},
    }])
    .to_string()
}

// --- what the app shows ---

/// A message's row as the child tells it.
#[derive(Debug, serde::Deserialize)]
struct Item {
    id: String,
    own: bool,
    edited: bool,
    notice: Option<Notice>,
    text: String,
    files: Vec<String>,
    quote: String,
}

#[derive(Debug, serde::Deserialize)]
struct Notice {
    line: String,
    link: Option<String>,
    page: Option<String>,
    red: bool,
}

impl Item {
    fn has_notice(&self, words: &str, label: &str, red: bool) -> bool {
        self.notice.as_ref().is_some_and(|notice| {
            notice.line == words && notice.link.as_deref() == Some(label) && notice.red == red && notice.page.is_some()
        })
    }
}

/// The open chat as the child tells it.
#[derive(Debug, serde::Deserialize)]
struct State {
    selected: Option<String>,
    reply: Option<String>,
    composing: bool,
    items: Vec<Item>,
}

impl State {
    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<Value>,
    seen: Vec<Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line, not seen before, that `found` takes, named `what` in the error.
    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; the last seen {:?}", self.seen.last()))?;
            self.seen.push(line.clone());
            if found(&line) {
                return Ok(line);
            }
        }
    }

    /// Takes in the lines that came.
    fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }
}

/// The app running in the sandbox: its process, its view trace, its control socket and the user's keyboard and
/// pointer.
struct AppRun {
    running: Running,
    trace: TraceReader,
    ctl: PathBuf,
    dir: PathBuf,
    input: VirtualInput,
    asked: u32,
}

impl AppRun {
    /// Starts the app on `service` with its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, service: &Service, dir: &Path) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let token = service.mock.token(LAB_A).ok_or("the mock's token for a")?;
        let origin = service.mock.origin();
        let dir_text = dir.to_string_lossy().into_owned();
        let _ = fs::remove_file(dir.join(THEME_FILE));
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "app"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &origin),
                (TROUTER_ENV, service.trouter.origin()),
                (TOKEN_ENV, &token),
                (DIR_ENV, &dir_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let trace = TraceReader::start(reader);
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let ctl = dir.join("ctl.sock");
        wait_until(WAIT, || Client::connect(&ctl, Timeouts::default()).ok()).ok_or("no control socket")?;
        let mut run = Self {
            running,
            trace,
            ctl,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            asked: 0,
        };
        run.input.move_to(900, 24)?;
        Ok(run)
    }

    /// `request` over the control socket, as `ngteamsctl` sends it.
    fn ask(&self, request: Request) -> SandboxResult<Response> {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        Ok(client.request(None, request)?)
    }

    /// `ngteamsctl chats --limit 1000`.
    fn chats(&self) -> SandboxResult<Vec<ChatRow>> {
        match self.ask(Request::Chats {
            unread_only: false,
            limit: Some(1000),
        })? {
            Response::Chats { chats } => Ok(chats),
            other => Err(format!("chats answered {other:?}").into()),
        }
    }

    /// The chat list's preview of `chat`, as `ngteamsctl chats` reads it.
    fn preview(&self, chat: &str) -> SandboxResult<Option<String>> {
        Ok(self
            .chats()?
            .into_iter()
            .find(|row| row.id == chat)
            .and_then(|row| row.preview))
    }

    /// `ngteamsctl messages <chat>`: message `id`'s text and whether it is called edited.
    fn ctl_message(&self, chat: &str, id: &str) -> SandboxResult<(String, bool)> {
        match self.ask(Request::Messages {
            chat: chat.to_owned(),
            limit: Some(50),
            before: None,
        })? {
            Response::Messages { messages } => messages
                .into_iter()
                .find(|message| message.id == id)
                .map(|message| (message.text, message.edited))
                .ok_or_else(|| format!("ngteamsctl messages has no {id}").into()),
            other => Err(format!("messages answered {other:?}").into()),
        }
    }

    /// `ngteamsctl open chat <chat>`, then the trace's `chat opened`.
    fn open(&mut self, chat: &str) -> SandboxResult {
        self.ask(Request::Open {
            target: OpenTarget::Chat { chat: chat.to_owned() },
        })?;
        self.trace.next_where(&format!("chat {chat} opened"), |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
        Ok(())
    }

    /// Asks the child `verb`; its answer.
    fn question(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state(&mut self) -> SandboxResult<State> {
        let answer = self.question("state")?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state `wanted` takes, named `what` in the error; that state.
    fn until(&mut self, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state:#?}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// [`Self::until`], then watches for [`QUIET`] that the state stays so.
    fn stays(&mut self, what: &str, kept: impl Fn(&State) -> bool) -> SandboxResult<State> {
        self.until(what, &kept)?;
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if !kept(&state) {
                return Err(format!("{what}: changed after {:?}: {state:#?}", start.elapsed()).into());
            }
            if start.elapsed() > QUIET {
                return Ok(state);
            }
            thread::sleep(POLL);
        }
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    /// A screenshot of the screen, kept as `target/ui-shots/<name>`; the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(900, 24)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        fs::copy(&png, shots_dir()?.join(name))?;
        Ok(png)
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "blocked-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits through the tray menu, as a user does; once the app ended and released its engine.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.running.pid());
        let host = sandbox.bus().connect()?;
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
        menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32))?;
        self.running
            .line_where(WAIT, |line| line == "engine-released")
            .map_err(|error| self.running.fail(&format!("the engine wasn't released: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the child ---

/// One account's mock token, for the transport and the push channel alike.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// a's engine on the parent's mock, its store in `dir`.
fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
    let trouter = Trouter::with_mock_origin(
        Arc::clone(&transport),
        Arc::clone(&tokens),
        &std::env::var(TROUTER_ENV)?,
    )?
    .with_options(Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(50),
        backoff_jitter_percent: 0,
        ..Options::default()
    });
    let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(http()?.with_csa(CSA_BASE)?)),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?)
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// The settings as the Appearance page leaves them once theme `key` is picked.
fn show_theme(settings: &mut teams_ui::Settings, key: &'static str) {
    settings.appearance.follow_system = false;
    settings.appearance.theme = key;
}

/// Shows the theme `file` names (`light` or `dark`) once it changes, as the user picks one in Settings, and says
/// `theme <name>` once it is applied.
fn follow_theme_file(settings: &Entity<teams_ui::SettingsModel>, file: PathBuf, cx: &mut App) {
    let settings = settings.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut applied = String::new();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(wanted) = fs::read_to_string(&file) else {
                continue;
            };
            if wanted == applied {
                continue;
            }
            let key = if wanted == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let changed =
                cx.update(|cx| settings.update(cx, |model, cx| model.change(|chosen| show_theme(chosen, key), cx)));
            if changed.is_err() {
                return;
            }
            // the frames in the new theme
            cx.background_executor().timer(Duration::from_millis(300)).await;
            say(&format!("theme {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let engine = engine_on_the_mock(&dir)?;
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let runner = support::runner(&dir)?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ctl = dir.join("ctl.sock");
    let theme_file = dir.join(THEME_FILE);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: Some(ctl),
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, theme_file.clone(), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, ask.clone(), cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    let store = Store::open(&support::store_file(&dir, Alias::A))?;
    if store.engine_lock().is_ok() {
        say("engine-released");
    }
    Ok(())
}

/// Answers the parent's questions about the open chat as they come.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = window
                .update(cx, |kit, window, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(match verb {
                        "state" => state_of(&shell, window, cx).to_string(),
                        other => format!("unknown {other}"),
                    })
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

/// The open chat's rows, selection and compose box, as [`State`] reads them.
fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let Some(chat) = shell.read(cx).chat() else {
        return json!({"selected": null, "reply": null, "composing": false, "items": []});
    };
    let view: &ChatView = chat.read(cx);
    let items: Vec<Value> = view
        .message_items()
        .map(|item| {
            let notice = item.policy.map(|notice| {
                let link = policy_notice::link(notice, item.own);
                json!({
                    "line": policy_notice::words(notice, item.own),
                    "link": link.map(|(label, _)| label),
                    "page": link.map(|(_, page)| page),
                    "red": policy_notice::is_error(notice, item.own),
                })
            });
            let (text, quote) = match &item.body {
                Body::Text(rendered) => (
                    lines(rendered.0.blocks.iter()),
                    lines(
                        rendered
                            .0
                            .blocks
                            .iter()
                            .filter_map(|block| match block {
                                Block::Quote(quote) => Some(quote.blocks.iter()),
                                _ => None,
                            })
                            .flatten(),
                    ),
                ),
                _ => (String::new(), String::new()),
            };
            json!({
                "id": item.id, "own": item.own, "edited": item.edited, "notice": notice, "text": text,
                "files": item.files.iter().map(|file| file.name.clone()).collect::<Vec<_>>(), "quote": quote,
            })
        })
        .collect();
    let compose = view.compose().read(cx);
    let reply = match compose.mode() {
        Mode::Reply(quoted) => Some(quoted.shown_preview().to_owned()),
        _ => None,
    };
    json!({
        "selected": view.selected(), "reply": reply, "composing": compose.focused(window, cx), "items": items,
    })
}

/// The blocks' lines as a reader copies them, one per line.
fn lines<'a>(blocks: impl Iterator<Item = &'a Block>) -> String {
    blocks.flat_map(Block::plain_lines).collect::<Vec<_>>().join("\n")
}
