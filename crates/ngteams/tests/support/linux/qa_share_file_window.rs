//! QA of task 543 (rounds 1 to 3), in the UI sandbox: "Share this file" on a file card, as a user opens and uses it
//! (`harness = false`: GPUI runs on the process's main thread).
//!
//! The parent runs the mock (`teams-mock`). Lab account c posts a message with a video and a document to a chat with
//! a, and the mock serves the video's thumbnail and what each file's link lets people do. The parent runs a's app as a
//! child in the sandbox with a control socket. It uses the app as a user does: it clicks with the sandbox's pointer
//! and presses keys. It finds the file cards and the dialog on the screen by their borders and surfaces. It reads the
//! posts the mock got, the links the sandbox's URI opener got (it opens nothing), the sandbox's clipboard, and the
//! dialog's and the views' state from the child.
//!
//! - **Open**: Share on the video's card and "More options > Share" on the document's card open "Share this file"
//!   for that file, with what its link lets people do. Share is off with no recipient: Enter and a click do nothing.
//! - **Keys**: Escape closes "More actions", then the emoji picker, then clears the box, then closes the dialog. A
//!   query with no match. Up, Down and Enter pick a suggestion. Backspace in the empty box takes the last chip off.
//!   Tab goes from the box to the message. Enter in the message shares, and Enter in the empty box shares.
//! - **Suggestions over the dialog** (round 2): "Made-up" finds more than the list shows. The list is over "Message
//!   preview" in both themes, and the wheel scrolls it. A click on the last row, a click on the third row, and Down
//!   and Enter pick the row the user sees. In a narrow window the list stays over the dialog, a click picks, and a
//!   query with no match says so.
//! - **Chip faces** (round 3): the face of a named group chat's chip and of a person's chip, in both themes. The
//!   initials are in the middle of the circle, and all of them are inside it.
//! - **Faces in other views** (round 3): the chat list's faces (48 px), and New message's suggestions (32 px) for
//!   people, for group chats of two words and for one of one word, in both themes. The initials are in the middle of
//!   the circle and inside it, on one line, at the size Teams gives the face, with one or two letters. All the
//!   faces of one list have one size of initials. "Start a group chat" from a chat's people, with "lab" typed: its
//!   people and chats (32 px) have the same initials.
//! - **Share**: one and three people, one and three group chats, one and two channels (the mock's teams have two
//!   live channels). Each target gets one post with the file's entry and the message, and the chat shows the file's
//!   card. A bold word and an emoji go out as rich text. The toast says "You've shared <file>".
//! - **Copy link**: the link on the clipboard and "Link created", its Copy and its Settings (the file's OneDrive
//!   page); "More actions > Share outside Teams" opens the same page.
//! - **A send error**: the service refuses the post. The dialog stays with the recipient and the message, and says
//!   why. Share again sends the file once.
//!
//! Every name, id and text is made up. Screenshots go to `target/ui-shots/qa543-*.png`, and also to the directory
//! `QA_SHARE_FILE_EVIDENCE` names, in Teams Light and Teams Dark. Without weston 15, niri or `dbus-daemon` it says so
//! and passes, unless `GPUI_SMOKE_REQUIRE=1`.

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
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_ctl::protocol::{OpenTarget, Request};
use teams_ctl::{Client, Timeouts};
use teams_mock::{
    AB_CHAT, AC_CHAT, AMS_BASE, AX_CHAT, CHANNEL, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, OUTSIDER,
    TEAM_CHANNEL, TrouterMock,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_people::Picked;
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::new_chat::Suggested;
use teams_ui::rich_text::Block;
use teams_ui::share_file::{ShareFileDialog, Target};
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

const CHILD_ENV: &str = "QA_SHARE_FILE_CHILD";
const TRACE_FD_ENV: &str = "QA_SHARE_FILE_TRACE_FD";
const ORIGIN_ENV: &str = "QA_SHARE_FILE_ORIGIN";
const TROUTER_ENV: &str = "QA_SHARE_FILE_TROUTER";
const TOKEN_ENV: &str = "QA_SHARE_FILE_TOKEN";
const DIR_ENV: &str = "QA_SHARE_FILE_DIR";
/// A directory that gets a copy of each screenshot.
const EVIDENCE_ENV: &str = "QA_SHARE_FILE_EVIDENCE";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa543-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);
/// How long a key or a click that must do nothing gets to do it anyway.
const QUIET: Duration = Duration::from_millis(1500);
/// The screen, as the UI tour's.
const SCREEN: (u32, u32) = (1280, 800);
/// niri's layout: each window takes the whole screen, and no hot corner.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";

/// A card's width and header, as Teams draws them (`teams_ui::files`).
const CARD_WIDTH: u32 = 316;
/// The card of a message's only file: two columns wide, its 16:9 picture under the header (`teams_ui::files`).
const LONE_WIDTH: u32 = 2 * CARD_WIDTH;
const LONE_HEIGHT: u32 = 460;
const HEADER: u32 = 64;

/// What the toast says after a share, before the file's name.
const SHARED: &str = "You've shared ";
/// What the file's link lets people do, as the mock's site says it.
const ANYONE_CAN_EDIT: &str = "Anyone with the link can edit";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_share_file_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

/// What the walk saw wrong that does not stop it: each is said at the end, and the test fails.
static FINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn parent() -> SandboxResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let options = SandboxOptions {
        width: SCREEN.0,
        height: SCREEN.1,
        niri_extra: NIRI_CONFIG,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams share file", &options)? else {
        return Ok(());
    };
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    if let Err(error) = walk(&sandbox, &service, &runtime, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams share file: ok");
    Ok(())
}

fn walk(sandbox: &Sandbox, service: &Service, runtime: &tokio::runtime::Runtime, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir)?;
    let outcome = (|| -> SandboxResult {
        let mut ctx = Walk {
            run: &mut run,
            sandbox,
            service,
            runtime,
            cards: None,
            doc_share_failed: false,
        };
        ctx.open_and_keys()?;
        ctx.many_matches()?;
        ctx.narrow()?;
        ctx.chip_faces()?;
        ctx.one_person()?;
        ctx.three_people()?;
        ctx.groups()?;
        ctx.channels()?;
        ctx.copy_link()?;
        ctx.outside()?;
        ctx.send_error()?;
        ctx.list_faces()?;
        ctx.new_message_faces()?;
        ctx.start_group_faces()
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    service: &'a Service,
    runtime: &'a tokio::runtime::Runtime,
    /// The video's and the document's cards on the screen, once found.
    cards: Option<(Rect, Rect)>,
    /// More options > Share on the document's card opened no dialog once: said once.
    doc_share_failed: bool,
}

/// A target of a share: what to type, the suggestion's kind and name, and the conversation its post goes to.
#[derive(Clone, Copy, Debug)]
struct To {
    query: &'static str,
    kind: &'static str,
    name: &'static str,
    chat: &'static str,
}

const PERSON_B: To = To {
    query: "Lab B",
    kind: "person",
    name: "Lab B",
    chat: AB_CHAT,
};
const PERSON_C: To = To {
    query: "Lab C",
    kind: "person",
    name: "Lab C",
    chat: AC_CHAT,
};
const PERSON_X: To = To {
    query: "Outsider",
    kind: "person",
    name: "Outsider X",
    chat: AX_CHAT,
};
const GENERAL: To = To {
    query: "General",
    kind: "channel",
    name: "General",
    chat: CHANNEL,
};
const MADE_UP_CHANNEL: To = To {
    query: "made-up channel",
    kind: "channel",
    name: "A made-up channel",
    chat: TEAM_CHANNEL,
};

impl Walk<'_> {
    /// Opens the source chat and finds the two cards (light): the video's, then the document's.
    fn source(&mut self) -> SandboxResult<(Rect, Rect)> {
        if self.run.theme_now() != "light" {
            self.run.theme(false)?;
        }
        let chat = self.service.source.clone();
        self.run.open(&chat)?;
        let id = self.service.source_post.clone();
        self.run.until("chat", "the source message's two cards", |state| {
            state.item(&id).is_some_and(|item| item.files.len() == 2)
        })?;
        thread::sleep(SETTLE * 2);
        self.run.shot(self.sandbox, "qa543-source-light.png")?;
        let cards = cards_in(&self.run.last, LIGHT);
        let [video, doc] = cards.as_slice() else {
            return Err(format!("the two cards are not on the screen: {cards:?}").into());
        };
        self.cards = Some((*video, *doc));
        Ok((*video, *doc))
    }

    /// Share on the video's card; the dialog, once it is open.
    fn open_from_video(&mut self) -> SandboxResult<Dialog> {
        let (video, _) = self.source()?;
        let (x, y) = share_at(video);
        self.run.click(x, y)?;
        let dialog = self.run.dialog_until("the dialog open from the video's Share", |d| {
            d.open && d.permission.is_some()
        })?;
        thread::sleep(SETTLE);
        Ok(dialog)
    }

    /// "More options > Share" on the document's card; the dialog, once it is open.
    fn open_from_document(&mut self) -> SandboxResult<Dialog> {
        let (_, doc) = self.source()?;
        let (mx, my) = more_at(doc);
        self.run.click(mx, my)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-doc-more-light.png")?;
        // the menu opens under "…": Open in browser, Download, Share, Copy link, 28 px each
        let (x, y) = doc_menu_share(more_at(doc));
        step(&format!("the document's More options: Share at ({x}, {y})"));
        self.run.click(x, y)?;
        let opened = self
            .run
            .dialog_until("the dialog open from the document's More options", |d| {
                d.open && d.permission.is_some()
            });
        let Ok(dialog) = opened else {
            if !self.doc_share_failed {
                note("More options > Share on the document's card opens no dialog".to_owned());
                self.doc_share_failed = true;
            }
            self.run.shot(self.sandbox, "qa543-doc-share-missing-light.png")?;
            self.run.input.key(keys::ESC)?;
            return self.open_from_video();
        };
        thread::sleep(SETTLE);
        Ok(dialog)
    }

    /// Types `to.query` in the box, then Down to its suggestion and Enter; once its chip shows.
    fn pick(&mut self, to: To) -> SandboxResult {
        self.run.input.type_text(to.query)?;
        let wanted = |s: &Suggestion| s.kind == to.kind && s.name == to.name;
        // the directory's answer comes 200 ms after the typing stops, and can add people above the chats
        thread::sleep(Duration::from_millis(700));
        let dialog = self
            .run
            .dialog_until(&format!("{} suggested for {:?}", to.name, to.query), |d| {
                d.suggestions.iter().any(wanted)
            })
            .inspect_err(|_| note(format!("{} is not suggested for {:?}", to.name, to.query)))?;
        step(&format!("{:?} suggests {:?}", to.query, dialog.suggestions));
        let at = dialog.suggestions.iter().position(wanted).unwrap_or_default();
        for _ in 0..=at {
            self.run.input.key(keys::DOWN)?;
            thread::sleep(Duration::from_millis(80));
        }
        self.run.input.key(keys::ENTER)?;
        self.run.dialog_until(&format!("{} picked", to.name), |d| {
            d.picked.last().map(String::as_str) == Some(to.name)
        })?;
        Ok(())
    }

    /// The posts to `chat` the mock took.
    fn posts_to(&self, chat: &str) -> Vec<Value> {
        posts(&self.runtime.block_on(self.service.mock.received_requests()))
            .into_iter()
            .filter(|(to, _)| to == chat)
            .map(|(_, body)| body)
            .collect()
    }

    /// The count of every post the mock took.
    fn all_posts(&self) -> usize {
        posts(&self.runtime.block_on(self.service.mock.received_requests())).len()
    }

    /// Waits until each of `targets` got one more post than in `before`; checks each holds `file` and `words`.
    fn check_posts(&self, targets: &[To], before: &[usize], file: &MadeUpFile, words: &str, what: &str) {
        let got = wait_until(WAIT, || {
            targets
                .iter()
                .zip(before)
                .all(|(to, before)| self.posts_to(to.chat).len() > *before)
                .then_some(())
        });
        if got.is_none() {
            let counts: Vec<usize> = targets.iter().map(|to| self.posts_to(to.chat).len()).collect();
            note(format!(
                "{what}: not every target got a post (before {before:?}, now {counts:?})"
            ));
            return;
        }
        thread::sleep(QUIET);
        for (to, before) in targets.iter().zip(before) {
            let sent = self.posts_to(to.chat);
            if sent.len() != before + 1 {
                note(format!(
                    "{what}: {} got {} posts, expected one",
                    to.name,
                    sent.len() - before
                ));
            }
            let Some(body) = sent.last() else { continue };
            let content = body["content"].as_str().unwrap_or_default();
            step(&format!("{what}: the post to {}: {content}", to.name));
            if !content.contains(words) {
                note(format!(
                    "{what}: the post to {} says {content:?}, not {words:?}",
                    to.name
                ));
            }
            let entries = files_of(body);
            let names: Vec<&str> = entries.iter().filter_map(|entry| entry["fileName"].as_str()).collect();
            if names != [file.name] {
                note(format!("{what}: the post to {} carries the files {names:?}", to.name));
            }
            if let Some(entry) = entries.first()
                && entry["fileInfo"]["shareUrl"].as_str() != Some(share_url(file).as_str())
            {
                note(format!(
                    "{what}: the post to {} has the link {:?}",
                    to.name, entry["fileInfo"]["shareUrl"]
                ));
            }
        }
    }

    /// Waits for the toast after a share of `file`.
    fn check_toast(&mut self, file: &MadeUpFile, what: &str) -> SandboxResult {
        let wanted = format!("{SHARED}{}", file.name);
        let toast = wait_until(Duration::from_secs(5), || {
            self.run.question("toast").ok().filter(|toast| toast.contains(&wanted))
        });
        if toast.is_none() {
            let now = self.run.question("toast")?;
            note(format!("{what}: no toast {wanted:?} (it says {now})"));
        }
        Ok(())
    }

    /// Opens `to`'s chat or channel and waits for a row with `file`'s card and `words`.
    fn check_shown(&mut self, to: To, file: &MadeUpFile, words: &str, shot: &str) -> SandboxResult {
        let view = if to.kind == "channel" {
            self.run.open_channel(to.chat)?;
            "posts"
        } else {
            self.run.open(to.chat)?;
            "chat"
        };
        let shown = self
            .run
            .until(view, &format!("{}'s card in {}", file.name, to.name), |state| {
                state
                    .items
                    .iter()
                    .any(|item| item.files.iter().any(|name| name == file.name) && item.text.contains(words))
            });
        match shown {
            Ok(state) => {
                if let Some(item) = state
                    .items
                    .iter()
                    .rev()
                    .find(|item| item.files.iter().any(|n| n == file.name))
                    && item.unshown
                {
                    note(format!(
                        "{}: the shared file shows \"content that can't be shown\"",
                        to.name
                    ));
                }
            }
            Err(error) => note(format!("{}: {error}", to.name)),
        }
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, shot)?;
        Ok(())
    }

    /// Shots of the screen as it is now in Teams Dark and Teams Light, `qa543-<name>-<theme>.png`; then Light again.
    fn both_themes(&mut self, name: &str) -> SandboxResult {
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa543-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa543-{name}-light.png"))?;
        Ok(())
    }

    /// The dialog's box on the screen now.
    fn dialog_box(&mut self, name: &str) -> SandboxResult<Rect> {
        self.run.shot(self.sandbox, name)?;
        surface_box(&self.run.last, DIALOG_SEED).ok_or_else(|| format!("no dialog on the screen ({name})").into())
    }
}

// --- open, Share off, and the keys ---

impl Walk<'_> {
    fn open_and_keys(&mut self) -> SandboxResult {
        let before = self.all_posts();
        let dialog = self.open_from_video()?;
        if dialog.file != CLIP.name {
            note(format!("the video's Share opens the dialog for {:?}", dialog.file));
        }
        if dialog.permission.as_deref() != Some(ANYONE_CAN_EDIT) {
            note(format!("the dialog's card says {:?} about the link", dialog.permission));
        }
        if dialog.can_share {
            note("Share is on with no recipient".to_owned());
        }
        if dialog.outside.is_none() {
            note("the dialog has no OneDrive page for a Microsoft file".to_owned());
        }
        self.both_themes("empty")?;
        let found = self.dialog_box("qa543-empty-measured-light.png")?;
        step(&format!("the dialog is at {found:?}"));
        if found.width().abs_diff(600) > 4 {
            note(format!("the dialog is {} px wide, Teams' is 600", found.width()));
        }
        // Share off: Enter in the empty box, and a click on Share, do nothing
        self.run.input.key(keys::ENTER)?;
        let (x, y) = share_button(found);
        self.run.click(x, y)?;
        thread::sleep(QUIET);
        if self.all_posts() != before {
            note("Enter or Share with no recipient sent something".to_owned());
        }
        if !self.run.dialog()?.open {
            return Err("Enter or Share with no recipient closed the dialog".into());
        }
        step("Share is off with no recipient");
        self.escapes(found)?;
        self.no_match()?;
        let dialog = self.open_from_document()?;
        if dialog.file != DOC.name {
            note(format!(
                "the document's More options > Share opens the dialog for {:?}",
                dialog.file
            ));
        }
        self.run.input.key(keys::ESC)?;
        self.run.dialog_until("the dialog closed by Escape", |d| !d.open)?;
        step("More options > Share opens the dialog for the document");
        Ok(())
    }

    /// "Lab" in the box: its suggestions in both themes, and a click on the first one.
    fn suggestions(&mut self, found: Rect) -> SandboxResult {
        self.run.input.type_text("Lab")?;
        thread::sleep(Duration::from_millis(700));
        self.run
            .dialog_until("suggestions for \"Lab\"", |d| !d.suggestions.is_empty())?;
        let dialog = self.run.dialog()?;
        step(&format!("\"Lab\" suggests {:?}", dialog.suggestions));
        self.run.shot(self.sandbox, "qa543-suggestions-light.png")?;
        self.check_suggestions_shown(found, dialog.suggestions.len(), "qa543-suggestions-light.png");
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-suggestions-dark.png")?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        // a click on the first suggestion, as a user picks it with the pointer
        let (x, y) = first_suggestion(found);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        let after = self.run.dialog()?;
        if after.picked.is_empty() {
            note(format!(
                "a click on the first suggestion ({x}, {y}) picks nothing: the list is drawn under \"Message preview\""
            ));
        } else {
            step(&format!("a click on the first suggestion picks {:?}", after.picked));
            self.run.input.key(keys::BACKSPACE)?;
            self.run.input.type_text("Lab")?;
            thread::sleep(Duration::from_millis(700));
        }
        Ok(())
    }

    /// Escape: "More actions", the emoji picker, the box's text, then the dialog.
    fn escapes(&mut self, found: Rect) -> SandboxResult {
        self.suggestions(found)?;
        let (x, y) = more_button(found);
        self.run.click(x, y)?;
        let dialog = self.run.dialog_until("More actions open", |d| d.more_open)?;
        self.run.shot(self.sandbox, "qa543-more-light.png")?;
        if dialog.outside.is_none() {
            note("More actions has no Share outside Teams".to_owned());
        }
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let dialog = self.run.dialog()?;
        if dialog.more_open || !dialog.open {
            note(format!(
                "the first Escape: More actions open {}, the dialog open {}",
                dialog.more_open, dialog.open
            ));
        }
        let (x, y) = emoji_button(found);
        self.run.click(x, y)?;
        if self.run.dialog_until("the emoji picker open", |d| d.emoji_open).is_ok() {
            self.run.shot(self.sandbox, "qa543-emoji-light.png")?;
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
            let dialog = self.run.dialog()?;
            if dialog.emoji_open || !dialog.open {
                note(format!(
                    "Escape in the emoji picker: the picker open {}, the dialog open {}",
                    dialog.emoji_open, dialog.open
                ));
            }
        } else {
            note("a click on the emoji button opens no picker".to_owned());
        }
        // the box keeps its text through both: focus it again (a click on the box) and Escape clears it
        let (x, y) = box_at(found);
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        if self.run.dialog()?.suggestions.is_empty() {
            note("the box lost \"Lab\" after the menu and the picker closed".to_owned());
        }
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let dialog = self.run.dialog()?;
        if !dialog.open || !dialog.suggestions.is_empty() {
            note(format!(
                "Escape with \"Lab\" in the box: the dialog open {}, {} suggestions",
                dialog.open,
                dialog.suggestions.len()
            ));
        }
        self.run.input.key(keys::ESC)?;
        if self
            .run
            .dialog_until("the dialog closed by the last Escape", |d| !d.open)
            .is_err()
        {
            note("Escape with an empty box does not close the dialog".to_owned());
            self.close_dialog()?;
        }
        step("Escape: More actions, the emoji picker, the box, the dialog");
        Ok(())
    }

    /// The rows under the first of `count` suggestions show on the list's surface (white in Teams Light), on the right
    /// of the rows where no word is drawn: the list is over everything under the box, as Teams draws it.
    fn check_suggestions_shown(&mut self, found: Rect, count: usize, shot_name: &str) {
        let rows = u32::try_from(count.saturating_sub(1)).unwrap_or_default().min(4);
        if rows == 0 {
            return;
        }
        let region = Rect {
            x0: found.x1 - 220,
            y0: found.y0 + 215,
            x1: found.x1 - 40,
            y1: found.y0 + 215 + 52 * rows,
        };
        let shot = &self.run.last;
        let total = region.width() * region.height();
        let on_list = (region.y0..region.y1)
            .flat_map(|y| (region.x0..region.x1).map(move |x| (x, y)))
            .filter(|&(x, y)| near(at(shot, x, y), LIGHT.surface, 3))
            .count();
        step(&format!(
            "the suggestions' rows: {on_list} of {total} pixels on the list's surface"
        ));
        if u32::try_from(on_list).unwrap_or_default() * 10 < total * 9 {
            note(format!(
                "the {count} suggestions are drawn under \"Message preview\": only {on_list} of {total} pixels where \
                 their rows go show the list ({shot_name})"
            ));
        }
    }

    /// A query that matches nothing says so.
    fn no_match(&mut self) -> SandboxResult {
        self.open_from_video()?;
        self.run.input.type_text("zzqxw")?;
        thread::sleep(Duration::from_millis(900));
        let dialog = self.run.dialog()?;
        if !dialog.suggestions.is_empty() {
            note(format!("\"zzqxw\" finds {:?}", dialog.suggestions));
        }
        self.run.shot(self.sandbox, "qa543-no-match-light.png")?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-no-match-dark.png")?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        self.run.input.key(keys::ESC)?;
        self.run.input.key(keys::ESC)?;
        self.run.dialog_until("the dialog closed", |d| !d.open)?;
        step("a query with no match says so");
        Ok(())
    }

    /// Closes the dialog by its Close (X), when Escape did not.
    fn close_dialog(&mut self) -> SandboxResult {
        let found = self.dialog_box("qa543-close-light.png")?;
        let (x, y) = close_button(found);
        self.run.click(x, y)?;
        self.run.dialog_until("the dialog closed by Close", |d| !d.open)?;
        Ok(())
    }
}

// --- round 2: the suggestions over the dialog, many of them, and in a narrow window ---

/// What finds more suggestions than the list shows without a scroll: the made-up chats, team and channel.
const MANY: &str = "Made-up";
/// The fewest suggestions [`MANY`] must find: more than the list's 300 px show.
const MANY_AT_LEAST: usize = 6;
/// The narrow main window's width: under the dialog's 600 px, so the dialog takes the window's width.
const NARROW: u32 = 560;

impl Walk<'_> {
    /// "Made-up" in the box: more suggestions than fit, in both themes, over "Message preview". The wheel scrolls the
    /// list to its end. A click on the last row, a click on the third row, and Down and Enter pick the row shown.
    fn many_matches(&mut self) -> SandboxResult {
        self.open_from_video()?;
        let found = self.dialog_box("qa543-many-open-light.png")?;
        let shown = self.type_many()?;
        self.run.shot(self.sandbox, "qa543-many-light.png")?;
        self.check_suggestions_shown(found, shown.len(), "qa543-many-light.png");
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-many-dark.png")?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        let (x, y) = nth_suggestion(found, 2);
        self.run.input.scroll(x, y, 10)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-many-scrolled-light.png")?;
        let last = shown.last().map(|s| s.name.clone()).unwrap_or_default();
        self.click_picks(last_suggestion(found), &last, "the last suggestion, scrolled to")?;
        self.type_many()?;
        let third = shown.get(2).map(|s| s.name.clone()).unwrap_or_default();
        self.click_picks(nth_suggestion(found, 2), &third, "the third suggestion")?;
        self.type_many()?;
        for _ in 0..4 {
            self.run.input.key(keys::DOWN)?;
            thread::sleep(Duration::from_millis(80));
        }
        self.run.input.key(keys::ENTER)?;
        thread::sleep(SETTLE);
        let fourth = shown.get(3).map(|s| s.name.clone()).unwrap_or_default();
        let picked = self.run.dialog()?.picked;
        if picked.last() == Some(&fourth) {
            step(&format!("Down four times and Enter pick {fourth:?}"));
        } else {
            note(format!("Down four times and Enter pick {picked:?}, not {fourth:?}"));
        }
        self.run.shot(self.sandbox, "qa543-many-picked-light.png")?;
        self.unpick()?;
        self.run.input.key(keys::ESC)?;
        self.run.dialog_until("the dialog closed", |d| !d.open)?;
        step("many suggestions: shown over the dialog, scrolled, picked by the pointer and the keys");
        Ok(())
    }

    /// A narrow window with the dialog open: the suggestions over it in both themes, a click on the second row, and
    /// "We couldn't find any matches."; then the window wide again.
    fn narrow(&mut self) -> SandboxResult {
        self.open_from_video()?;
        self.resize(NARROW)?;
        let found = self.dialog_in("qa543-narrow-open-light.png")?;
        step(&format!("in the narrow window the dialog is at {found:?}"));
        if found.x1 > NARROW {
            note(format!(
                "the dialog {found:?} reaches past the narrow window's right edge at {NARROW}"
            ));
        }
        let shown = self.type_many()?;
        self.run.shot(self.sandbox, "qa543-narrow-many-light.png")?;
        self.check_suggestions_shown(found, shown.len(), "qa543-narrow-many-light.png");
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-narrow-many-dark.png")?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        let second = shown.get(1).map(|s| s.name.clone()).unwrap_or_default();
        self.click_picks(
            nth_suggestion(found, 1),
            &second,
            "the second suggestion in the narrow window",
        )?;
        self.run.input.type_text("zzqxw")?;
        thread::sleep(Duration::from_millis(900));
        let dialog = self.run.dialog()?;
        if !dialog.suggestions.is_empty() {
            note(format!("\"zzqxw\" finds {:?} in the narrow window", dialog.suggestions));
        }
        self.run.shot(self.sandbox, "qa543-narrow-no-match-light.png")?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-narrow-no-match-dark.png")?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        self.run.input.key(keys::ESC)?;
        self.run.input.key(keys::ESC)?;
        self.run.dialog_until("the dialog closed", |d| !d.open)?;
        self.resize(SCREEN.0)?;
        step("a narrow window: the suggestions over the dialog, a click picks, no match says so");
        Ok(())
    }

    /// Types [`MANY`] in the empty box; the suggestions, once at least [`MANY_AT_LEAST`] show.
    fn type_many(&mut self) -> SandboxResult<Vec<Suggestion>> {
        self.run.input.type_text(MANY)?;
        thread::sleep(Duration::from_millis(700));
        let dialog = self
            .run
            .dialog_until(&format!("{MANY_AT_LEAST} suggestions for {MANY:?}"), |d| {
                d.suggestions.len() >= MANY_AT_LEAST
            })?;
        step(&format!("{MANY:?} suggests {:?}", dialog.suggestions));
        Ok(dialog.suggestions)
    }

    /// A click at `(x, y)` picks `name`; then Backspace takes its chip off again.
    fn click_picks(&mut self, (x, y): (u32, u32), name: &str, what: &str) -> SandboxResult {
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        let picked = self.run.dialog()?.picked;
        if picked.last().map(String::as_str) == Some(name) {
            step(&format!("a click on {what} ({x}, {y}) picks {name:?}"));
        } else {
            note(format!("a click on {what} ({x}, {y}) picks {picked:?}, not {name:?}"));
        }
        self.unpick()
    }

    /// The box is empty after a pick: Backspace takes the chip off. With no pick, Escape clears the box.
    fn unpick(&mut self) -> SandboxResult {
        if self.run.dialog()?.picked.is_empty() {
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
            return Ok(());
        }
        self.run.input.key(keys::BACKSPACE)?;
        self.run.dialog_until("the chip taken off", |d| d.picked.is_empty())?;
        Ok(())
    }

    /// The compositor sizes the main window to `width` wide, as a user's drag does.
    fn resize(&self, width: u32) -> SandboxResult {
        let sandbox = self.sandbox;
        sandbox.niri_msg(&["action", "set-column-width", &width.to_string()])?;
        wait_until(WAIT, || {
            let text = sandbox.niri_msg(&["-j", "windows"]).ok()?;
            let windows: Value = serde_json::from_str(&text).ok()?;
            windows
                .as_array()?
                .iter()
                .any(|window| window["layout"]["window_size"][0].as_f64() == Some(f64::from(width)))
                .then_some(())
        })
        .ok_or_else(|| format!("the window never took {width} px"))?;
        thread::sleep(SETTLE * 2);
        Ok(())
    }

    /// The dialog's box wherever the window is: the first light surface on the screen's middle row that is a box of
    /// a dialog's size.
    fn dialog_in(&mut self, name: &str) -> SandboxResult<Rect> {
        self.run.shot(self.sandbox, name)?;
        let shot = &self.run.last;
        let y = SCREEN.1 / 2;
        (0..shot.width())
            .filter(|&x| near(at(shot, x, y), LIGHT.surface, 3))
            .find_map(|x| surface_box(shot, (x, y)).filter(|b| b.width() > 300 && b.height() > 300))
            .ok_or_else(|| format!("no dialog on the screen ({name})").into())
    }
}

// --- round 3: the faces of the chips ---

/// A chip's face across, px: a named group chat's (the dialog's own face) and a person's (the kit's small avatar).
const GROUP_FACE: u32 = 20;
const PERSON_FACE: u32 = 24;
/// How far the middle of the initials can be from the middle of the face, px.
const OFF_MIDDLE: u32 = 2;
/// The part of the screen a zoomed shot of a chip shows, and how many times larger it is.
const CHIP_CROP: (u32, u32) = (200, 40);
const ZOOM: u32 = 5;

impl Walk<'_> {
    /// The face of a named group chat's chip and of a person's chip, in both themes: the initials in the middle of the
    /// circle, all of them inside it, as Teams draws them.
    fn chip_faces(&mut self) -> SandboxResult {
        let east = self
            .service
            .groups
            .iter()
            .copied()
            .find(|to| to.name == "Made-up crew east")
            .ok_or("no group \"Made-up crew east\"")?;
        for (to, side, kind) in [(east, GROUP_FACE, "group"), (PERSON_B, PERSON_FACE, "person")] {
            self.open_from_video()?;
            self.pick(to)?;
            for dark in [false, true] {
                self.run.theme(dark)?;
                thread::sleep(SETTLE);
                let name = format!("qa543-chip-{kind}-{}.png", if dark { "dark" } else { "light" });
                let found = self.dialog_box(&name)?;
                self.check_chip_face(found, (side, dark), &name)?;
            }
            self.run.theme(false)?;
            self.unpick()?;
            self.run.input.key(keys::ESC)?;
            self.run.dialog_until("the dialog closed", |d| !d.open)?;
        }
        step("chip faces: the initials in the middle of the circle");
        Ok(())
    }

    /// The initials on the face of the first chip (`side` px across, in Teams Dark when `dark`) are in the middle of
    /// the circle and inside it. A zoomed shot of the chip goes next to the shot.
    fn check_chip_face(&self, found: Rect, (side, dark): (u32, bool), shot_name: &str) -> SandboxResult {
        let shot = &self.run.last;
        let (_, middle) = box_at(found);
        keep_zoomed(shot, (found.x0 + 30, middle - CHIP_CROP.1 / 2), shot_name)?;
        let Some(face) = chip_face(shot, found, side, dark) else {
            note(format!(
                "no chip with a face of {side} px in the recipients' box ({shot_name})"
            ));
            return Ok(());
        };
        let Some(ink) = initials_ink(shot, face, dark) else {
            note(format!("the chip's face {face:?} draws no initials ({shot_name})"));
            return Ok(());
        };
        step(&format!("the chip's face {face:?}, its initials {ink:?} ({shot_name})"));
        // the middles are doubled, so they stay whole numbers
        let off_x = (ink.x0 + ink.x1).abs_diff(face.x0 + face.x1);
        let off_y = (ink.y0 + ink.y1).abs_diff(face.y0 + face.y1);
        let inside = ink.x0 >= face.x0 + 2 && ink.y0 >= face.y0 + 2 && ink.x1 + 2 <= face.x1 && ink.y1 + 2 <= face.y1;
        if off_x > 2 * OFF_MIDDLE || off_y > 2 * OFF_MIDDLE || !inside {
            note(format!(
                "the initials {ink:?} on the chip's face {face:?} are not in the middle of the circle, or the circle \
                 cuts them ({shot_name})"
            ));
        }
        Ok(())
    }
}

/// The face of the first chip in the recipients' box of the dialog at `found`, `side` px across. The chip's surface
/// starts on the box's middle row. The face is 2 px in from it, in the middle of the chip's height.
fn chip_face(shot: &image::RgbImage, found: Rect, side: u32, dark: bool) -> Option<Rect> {
    let surface = if dark { DARK.surface } else { LIGHT.surface };
    let on_chip = |x: u32, y: u32| near(at(shot, x, y), surface, 3);
    let (_, middle) = box_at(found);
    let left = (found.x0 + 30..found.x0 + 90).find(|&x| on_chip(x, middle))?;
    // a column in the gap between the face and the name: only the chip's surface
    let gap = left + 2 + side + 3;
    let top = (middle - 20..=middle).rev().take_while(|&y| on_chip(gap, y)).last()?;
    let bottom = (middle..=middle + 20).take_while(|&y| on_chip(gap, y)).last()?;
    let x0 = left + 2;
    let y0 = (top + bottom + 1).checked_sub(side)? / 2;
    Some(Rect {
        x0,
        y0,
        x1: x0 + side,
        y1: y0 + side,
    })
}

/// The box round the ink of the initials on `face`: darker than the face's tint in Teams Light, lighter in Teams
/// Dark. `None` when there are too few such pixels for a letter.
fn initials_ink(shot: &image::RgbImage, face: Rect, dark: bool) -> Option<Rect> {
    let ink = |pixel: [u8; 3]| {
        let light = pixel.iter().map(|&c| u32::from(c)).sum::<u32>() / 3;
        if dark { light > 120 } else { light < 170 }
    };
    let pixels: Vec<(u32, u32)> = (face.y0..face.y1)
        .flat_map(|y| (face.x0..face.x1).map(move |x| (x, y)))
        .filter(|&(x, y)| ink(at(shot, x, y)))
        .collect();
    if pixels.len() < 8 {
        return None;
    }
    Some(Rect {
        x0: pixels.iter().map(|&(x, _)| x).min()?,
        y0: pixels.iter().map(|&(_, y)| y).min()?,
        x1: pixels.iter().map(|&(x, _)| x).max()? + 1,
        y1: pixels.iter().map(|&(_, y)| y).max()? + 1,
    })
}

/// The part of `shot` at `(x, y)` that [`CHIP_CROP`] gives, [`ZOOM`] times larger, kept as `<shot>-zoom.png` where
/// the shots go.
fn keep_zoomed(shot: &image::RgbImage, (x, y): (u32, u32), shot_name: &str) -> SandboxResult {
    let (width, height) = CHIP_CROP;
    let crop = image::imageops::crop_imm(shot, x, y, width, height).to_image();
    let zoomed = image::imageops::resize(&crop, width * ZOOM, height * ZOOM, image::imageops::FilterType::Nearest);
    let name = format!("{}-zoom.png", shot_name.trim_end_matches(".png"));
    zoomed.save(shots_dir()?.join(&name))?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        zoomed.save(PathBuf::from(evidence).join(&name))?;
    }
    Ok(())
}

// --- round 3: the faces of the chat list and of New message ---

/// The faces across, px: the chat list's (`teams_ui::chat_list::AVATAR`) and New message's suggestions'.
const LIST_FACE: u32 = 48;
const SUGGESTION_FACE: u32 = 32;
/// The chat list on the screen, from the rail to the conversation, under its header.
const LIST: Rect = Rect {
    x0: 78,
    y0: 96,
    x1: 450,
    y1: SCREEN.1,
};
/// The conversation's side, where New message shows its suggestions, under the title bar.
const CONVERSATION: Rect = Rect {
    x0: 456,
    y0: 48,
    x1: SCREEN.0,
    y1: SCREEN.1,
};
/// A group chat of one word: its face has one letter.
const ONE_WORD: &str = "Harbourcrew";
/// A named group chat that "lab" finds, with a round face.
const LAB_GROUP: &str = "Lab alerts";
/// How many times larger a zoomed shot of a face is, and the margin round the face it keeps.
const FACE_ZOOM: u32 = 6;
const FACE_MARGIN: u32 = 6;
/// The chat header's "View and add participants", and its flyout's "Start a group chat" under the 1:1 chat's two
/// people.
const PEOPLE_BUTTON: (u32, u32) = (1243, 71);
const START_GROUP: (u32, u32) = (1098, 258);
/// The people flyout's rows of the chat's members, under its title, over "Start a group chat".
const PEOPLE_MEMBERS: Rect = Rect {
    x0: 1012,
    y0: 128,
    x1: 1256,
    y1: 232,
};
/// The suggestions of "Start a group chat", inside the picker's border, under its box.
const GROUP_PICKER: Rect = Rect {
    x0: 726,
    y0: 362,
    x1: 1086,
    y1: 700,
};

impl Walk<'_> {
    /// The faces with initials in the chat list (48 px), in both themes: the initials in the middle of the circle,
    /// inside it, on one line, at Teams' size for the face.
    fn list_faces(&mut self) -> SandboxResult {
        self.run.open(PERSON_B.chat)?;
        for dark in [false, true] {
            self.run.theme(dark)?;
            thread::sleep(SETTLE);
            let name = format!("qa543-list-faces-{}.png", theme_name(dark));
            self.run.shot(self.sandbox, &name)?;
            self.check_faces((LIST, LIST_FACE), (3, None), &name)?;
        }
        self.run.theme(false)?;
        step("the chat list's faces: the initials in the middle of the circle");
        Ok(())
    }

    /// New message (Ctrl+N), its To: line's suggestions (32 px faces) for people, for group chats of two words and for
    /// a group chat of one word, in both themes: as [`Self::list_faces`] checks them, with one or two letters.
    fn new_message_faces(&mut self) -> SandboxResult {
        self.ctrl('n')?;
        self.new_message_until("New message open", |open, _| open)?;
        thread::sleep(SETTLE);
        for (query, at_least, letters) in [("Lab", 2, 2), (MANY, 3, 2), (ONE_WORD, 1, 1)] {
            self.run.input.type_text(query)?;
            let found = self.new_message_until(&format!("suggestions for {query:?}"), |_, names| {
                names.iter().any(|name| name.contains(query))
            })?;
            step(&format!("New message: {query:?} suggests {found:?}"));
            for dark in [false, true] {
                self.run.theme(dark)?;
                thread::sleep(SETTLE);
                let slug = query.to_lowercase().replace(' ', "-");
                let name = format!("qa543-new-message-{slug}-{}.png", theme_name(dark));
                self.run.shot(self.sandbox, &name)?;
                self.check_faces((CONVERSATION, SUGGESTION_FACE), (at_least, Some(letters)), &name)?;
            }
            self.run.theme(false)?;
            for _ in query.chars() {
                self.run.input.key(keys::BACKSPACE)?;
            }
            // the empty line suggests the people of the latest chats again
            self.new_message_until("the To: line empty again", |_, names| names != found.as_slice())?;
        }
        self.run.open(PERSON_B.chat)?;
        step("New message's suggestions: the initials in the middle of the circle");
        Ok(())
    }

    /// The chat's people (the header's button), "Start a group chat" and "lab" typed: the flyout's members and the
    /// picker's suggestions (people and chats, 32 px faces) in both themes, as [`Self::list_faces`] checks them.
    fn start_group_faces(&mut self) -> SandboxResult {
        self.run.open(PERSON_B.chat)?;
        thread::sleep(SETTLE);
        self.run.click(PEOPLE_BUTTON.0, PEOPLE_BUTTON.1)?;
        self.people_until("the people flyout open", |open, _, _| open)?;
        self.run.click(START_GROUP.0, START_GROUP.1)?;
        self.people_until("the picker open", |_, picking, _| picking)?;
        self.run.input.type_text("lab")?;
        let found = self.people_until("suggestions for \"lab\"", |_, _, names| {
            names.iter().any(|name| name == "Lab C") && names.iter().any(|name| name == LAB_GROUP)
        })?;
        step(&format!("Start a group chat: \"lab\" suggests {found:?}"));
        for dark in [false, true] {
            self.run.theme(dark)?;
            thread::sleep(SETTLE);
            let name = format!("qa543-start-group-lab-{}.png", theme_name(dark));
            self.run.shot(self.sandbox, &name)?;
            // its own name keeps the members' zoomed faces apart from the suggestions'
            let members = name.replace(".png", "-members.png");
            self.check_faces((PEOPLE_MEMBERS, SUGGESTION_FACE), (2, Some(2)), &members)?;
            self.check_faces((GROUP_PICKER, SUGGESTION_FACE), (2, Some(2)), &name)?;
        }
        self.run.theme(false)?;
        self.run.input.key(keys::ESC)?;
        self.run.input.key(keys::ESC)?;
        self.people_until("the people flyout closed", |open, _, _| !open)?;
        step("the people flyout and Start a group chat: one size of initials for members, people and chats");
        Ok(())
    }

    /// Waits up to [`WAIT`] for the chat's people flyout as `wanted` takes it (open, its picker open, the picker's
    /// suggestions' names); the names.
    fn people_until(
        &mut self,
        what: &str,
        wanted: impl Fn(bool, bool, &[String]) -> bool,
    ) -> SandboxResult<Vec<String>> {
        let start = Instant::now();
        loop {
            let answer = self.run.question("people")?;
            let read: Value = serde_json::from_str(&answer)?;
            let names: Vec<String> = read["suggestions"]
                .as_array()
                .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            let open = read["open"].as_bool().unwrap_or_default();
            if wanted(open, read["picking"].as_bool().unwrap_or_default(), &names) {
                return Ok(names);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {answer}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Waits up to [`WAIT`] for New message as `wanted` takes it (open, its suggestions' names); the names.
    fn new_message_until(
        &mut self,
        what: &str,
        wanted: impl Fn(bool, &[String]) -> bool,
    ) -> SandboxResult<Vec<String>> {
        let start = Instant::now();
        loop {
            let answer = self.run.question("new_message")?;
            let read: Value = serde_json::from_str(&answer)?;
            let open = read["open"].as_bool().unwrap_or_default();
            let names: Vec<String> = read["suggestions"]
                .as_array()
                .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            if wanted(open, &names) {
                return Ok(names);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {answer}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The round faces `side` px across in `area` of the last shot: at least `at_least` of them, each with initials
    /// (of `letters` letters, when given) in the middle of the circle and inside it, on one line, at Teams' size,
    /// all of one size. A zoomed shot of each face goes next to the shot.
    fn check_faces(
        &self,
        (area, side): (Rect, u32),
        (at_least, letters): (usize, Option<usize>),
        shot_name: &str,
    ) -> SandboxResult {
        let shot = &self.run.last;
        let faces = round_faces(shot, area, side);
        if faces.len() < at_least {
            note(format!(
                "{} round faces of {side} px in {area:?}, not at least {at_least} ({shot_name})",
                faces.len()
            ));
        }
        let mut heights = Vec::new();
        for (at, face) in faces.iter().enumerate() {
            keep_face_zoomed(shot, *face, &format!("{}-face{at}", shot_name.trim_end_matches(".png")))?;
            let Some((ink, drawn)) = face_ink(shot, *face) else {
                note(format!("the face {face:?} draws no initials ({shot_name})"));
                continue;
            };
            step(&format!(
                "a face {face:?}: initials {ink:?}, {drawn} letter(s), {} px high ({shot_name})",
                ink.height()
            ));
            heights.push(ink.height());
            // the middles are doubled, so they stay whole numbers
            let off_x = (ink.x0 + ink.x1).abs_diff(face.x0 + face.x1);
            let off_y = (ink.y0 + ink.y1).abs_diff(face.y0 + face.y1);
            if off_x > 2 * OFF_MIDDLE || off_y > 2 * OFF_MIDDLE {
                note(format!(
                    "the initials {ink:?} are not in the middle of the face {face:?} ({shot_name})"
                ));
            }
            if !ink_inside(shot, *face, ink) {
                note(format!(
                    "the circle of the face {face:?} cuts its initials {ink:?} ({shot_name})"
                ));
            }
            // capitals stand about 0.7 to 0.8 of the text's size: on one line, at the size Teams gives the face
            let text = teams_initials_size(side);
            if ink.height() * 10 < text * 7 || ink.height() * 10 > text * 9 {
                note(format!(
                    "the initials {ink:?} on the face {face:?} are {} px high, not those of Teams' {text} px text on \
                     one line ({shot_name})",
                    ink.height()
                ));
            }
            if letters.is_some_and(|letters| letters != drawn) {
                note(format!(
                    "the face {face:?} shows {drawn} letter(s), not {letters:?} ({shot_name})"
                ));
            }
        }
        // a round letter rises 1 px over a flat one
        if let (Some(low), Some(high)) = (heights.iter().min(), heights.iter().max())
            && high - low > 1
        {
            note(format!(
                "the initials on the faces of {side} px are {heights:?} px high, not one size ({shot_name})"
            ));
        }
        Ok(())
    }
}

fn theme_name(dark: bool) -> &'static str {
    if dark { "dark" } else { "light" }
}

/// The size of the initials Teams (Fluent's avatar) gives a face `side` px across, px.
const fn teams_initials_size(side: u32) -> u32 {
    match side {
        ..=24 => 10,
        25..=28 => 12,
        29..=40 => 14,
        41..=56 => 16,
        57..=96 => 20,
        _ => 24,
    }
}

/// The round faces `side` px across in `area` of `shot`: each a patch off its row's background (the row's most
/// common colour in `area`) that fills a square of the side, the square's corners on the background. A presence
/// badge at its corner may make the patch up to 3 px larger. A split group face and a team's square are not taken.
fn round_faces(shot: &image::RgbImage, area: Rect, side: u32) -> Vec<Rect> {
    let backgrounds: Vec<[u8; 3]> = (area.y0..area.y1).map(|y| row_background(shot, area, y)).collect();
    let off = |x: u32, y: u32| {
        y.checked_sub(area.y0)
            .and_then(|row| backgrounds.get(row as usize))
            .is_some_and(|&background| !near(at(shot, x, y), background, 12))
    };
    let mut seen = vec![false; area.width() as usize * area.height() as usize];
    let mut faces = Vec::new();
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            if seen[area.index(x, y)] || !off(x, y) {
                continue;
            }
            let patch = patch_at((x, y), area, &off, &mut seen);
            let fits = |across: u32| (side..=side + 3).contains(&across);
            // a circle leaves the square's corner bare; a team's square, rounded less, does not
            let corner = side / 10;
            let round =
                !off(patch.x0 + corner, patch.y0 + corner) && !off(patch.x0 + side - 1 - corner, patch.y0 + corner);
            if fits(patch.width()) && fits(patch.height()) && round {
                faces.push(Rect {
                    x0: patch.x0,
                    y0: patch.y0,
                    x1: patch.x0 + side,
                    y1: patch.y0 + side,
                });
            }
        }
    }
    faces
}

/// The box round the patch of pixels that `off` takes, joined side by side, from `start` in `area`; each is marked in
/// `seen` (a flag a pixel of `area`, row by row).
fn patch_at(start: (u32, u32), area: Rect, off: &impl Fn(u32, u32) -> bool, seen: &mut [bool]) -> Rect {
    seen[area.index(start.0, start.1)] = true;
    let mut patch = Rect {
        x0: start.0,
        y0: start.1,
        x1: start.0 + 1,
        y1: start.1 + 1,
    };
    let mut stack = vec![start];
    while let Some((px, py)) = stack.pop() {
        patch = Rect {
            x0: patch.x0.min(px),
            y0: patch.y0.min(py),
            x1: patch.x1.max(px + 1),
            y1: patch.y1.max(py + 1),
        };
        let next = [
            (px.wrapping_sub(1), py),
            (px + 1, py),
            (px, py.wrapping_sub(1)),
            (px, py + 1),
        ];
        for (nx, ny) in next {
            let inside = (area.x0..area.x1).contains(&nx) && (area.y0..area.y1).contains(&ny);
            if inside && !seen[area.index(nx, ny)] && off(nx, ny) {
                seen[area.index(nx, ny)] = true;
                stack.push((nx, ny));
            }
        }
    }
    patch
}

/// The most common colour of row `y` in `area`.
fn row_background(shot: &image::RgbImage, area: Rect, y: u32) -> [u8; 3] {
    let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    for x in area.x0..area.x1 {
        *counts.entry(at(shot, x, y)).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|&(_, count)| count)
        .map_or([0, 0, 0], |(colour, _)| colour)
}

/// Whether a pixel `(x, y)` is at most `within` px inside the edge of the circle of `face` (the middles doubled).
fn in_circle(face: Rect, (x, y): (u32, u32), within: u32) -> bool {
    let dx = i64::from(2 * x + 1) - i64::from(face.x0 + face.x1);
    let dy = i64::from(2 * y + 1) - i64::from(face.y0 + face.y1);
    let radius = i64::from(face.width()) - i64::from(2 * within);
    dx * dx + dy * dy <= radius * radius
}

/// The box round the ink of the initials on `face`, and how many letters it has (by its width): the
/// pixels in the circle that differ much from the face's tint, read above the letters. `None` when there are too
/// few for a letter.
fn face_ink(shot: &image::RgbImage, face: Rect) -> Option<(Rect, usize)> {
    let tint = at(shot, face.x0 + face.width() / 2, face.y0 + face.height() / 8);
    let ink = |x: u32, y: u32| {
        let pixel = at(shot, x, y);
        let apart: u32 = pixel
            .iter()
            .zip(tint)
            .map(|(&got, want)| u32::from(got.abs_diff(want)))
            .sum();
        apart > 60 && in_circle(face, (x, y), 2)
    };
    let pixels: Vec<(u32, u32)> = (face.y0..face.y1)
        .flat_map(|y| (face.x0..face.x1).map(move |x| (x, y)))
        .filter(|&(x, y)| ink(x, y))
        .collect();
    if pixels.len() < 8 {
        return None;
    }
    let found = Rect {
        x0: pixels.iter().map(|&(x, _)| x).min()?,
        y0: pixels.iter().map(|&(_, y)| y).min()?,
        x1: pixels.iter().map(|&(x, _)| x).max()? + 1,
        y1: pixels.iter().map(|&(_, y)| y).max()? + 1,
    };
    // a capital is at most about 1.3 times as wide as high; two side by side are wider (the gap between them can
    // close in the smoothing, so the runs of inked columns do not count them)
    let letters = if found.width() * 20 > found.height() * 27 { 2 } else { 1 };
    Some((found, letters))
}

/// Whether the initials' ink keeps clear of the circle's edge: no inked pixel in the last 3 px of the circle.
fn ink_inside(shot: &image::RgbImage, face: Rect, ink: Rect) -> bool {
    let tint = at(shot, face.x0 + face.width() / 2, face.y0 + face.height() / 8);
    (ink.y0..ink.y1)
        .flat_map(|y| (ink.x0..ink.x1).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let apart: u32 = at(shot, x, y)
                .iter()
                .zip(tint)
                .map(|(&got, want)| u32::from(got.abs_diff(want)))
                .sum();
            apart > 60
        })
        .all(|point| in_circle(face, point, 3))
}

/// `face` with a margin, [`FACE_ZOOM`] times larger, kept as `<name>-zoom.png` where the shots go.
fn keep_face_zoomed(shot: &image::RgbImage, face: Rect, name: &str) -> SandboxResult {
    let x = face.x0.saturating_sub(FACE_MARGIN);
    let y = face.y0.saturating_sub(FACE_MARGIN);
    let across = face.width() + 2 * FACE_MARGIN;
    let crop = image::imageops::crop_imm(shot, x, y, across, across).to_image();
    let zoomed = image::imageops::resize(
        &crop,
        across * FACE_ZOOM,
        across * FACE_ZOOM,
        image::imageops::FilterType::Nearest,
    );
    let name = format!("{name}-zoom.png");
    zoomed.save(shots_dir()?.join(&name))?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        zoomed.save(PathBuf::from(evidence).join(&name))?;
    }
    Ok(())
}

// --- the shares ---

impl Walk<'_> {
    /// One person, from the video's Share: Tab to the message, then a click on Share.
    fn one_person(&mut self) -> SandboxResult {
        let words = "Made-up note for one person";
        let before = [self.posts_to(PERSON_B.chat).len()];
        self.open_from_video()?;
        self.pick(PERSON_B)?;
        self.run.input.key(keys::TAB)?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.type_text(words)?;
        let dialog = self
            .run
            .dialog_until("the message typed after Tab", |d| d.message.contains(words))?;
        if !dialog.can_share {
            note("Share is off with one recipient".to_owned());
        }
        let found = self.dialog_box("qa543-one-person-light.png")?;
        let (x, y) = share_button(found);
        self.run.click(x, y)?;
        self.run.dialog_until("the dialog closed after Share", |d| !d.open)?;
        self.run.shot(self.sandbox, "qa543-sent-light.png")?;
        self.check_toast(&CLIP, "one person")?;
        self.check_posts(&[PERSON_B], &before, &CLIP, words, "one person");
        self.check_shown(PERSON_B, &CLIP, words, "qa543-shown-person-light.png")?;
        step("one person: shared");
        Ok(())
    }

    /// Three people, from the document's More options: Backspace takes the last chip off; a bold word and an emoji;
    /// Enter in the message shares.
    fn three_people(&mut self) -> SandboxResult {
        let targets = [PERSON_B, PERSON_C, PERSON_X];
        let before: Vec<usize> = targets.iter().map(|to| self.posts_to(to.chat).len()).collect();
        let file = file_named(&self.open_from_document()?.file);
        for to in targets {
            self.pick(to)?;
        }
        self.run.input.key(keys::BACKSPACE)?;
        let dialog = self
            .run
            .dialog_until("Backspace took the last chip off", |d| d.picked.len() == 2)?;
        if dialog.picked != ["Lab B", "Lab C"] {
            note(format!("after Backspace the chips are {:?}", dialog.picked));
        }
        self.pick(PERSON_X)?;
        self.rich_message()?;
        // the message has the keyboard after the format bar's button
        self.run.input.key(keys::ENTER)?;
        if self
            .run
            .dialog_until("the dialog closed by Enter in the message", |d| !d.open)
            .is_err()
        {
            note("Enter in the message does not share".to_owned());
            let found = self.dialog_box("qa543-enter-failed-light.png")?;
            let (x, y) = share_button(found);
            self.run.click(x, y)?;
            self.run.dialog_until("the dialog closed after Share", |d| !d.open)?;
        }
        self.check_toast(file, "three people")?;
        self.check_posts(&targets, &before, file, "Made-up ", "three people");
        self.check_rich(&targets);
        self.check_shown(PERSON_X, file, "Made-up", "qa543-shown-third-person-light.png")?;
        step("three people: shared with a bold word and an emoji");
        Ok(())
    }

    /// Tab to the message; "Made-up **bold** note" and a heart from the emoji picker; Format shows its bar and hides
    /// it again. Shots of the format bar and of the dialog ready, in both themes.
    fn rich_message(&mut self) -> SandboxResult {
        self.run.input.key(keys::TAB)?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.type_text("Made-up ")?;
        self.ctrl('b')?;
        self.run.input.type_text("bold")?;
        self.ctrl('b')?;
        self.run.input.type_text(" note ")?;
        let found = self.dialog_box("qa543-three-typed-light.png")?;
        let (x, y) = emoji_button(found);
        self.run.click(x, y)?;
        self.run.dialog_until("the emoji picker open", |d| d.emoji_open)?;
        thread::sleep(SETTLE);
        self.run.input.type_text("heart")?;
        thread::sleep(Duration::from_millis(500));
        self.run.input.key(keys::ENTER)?;
        let dialog = self.run.dialog_until("the emoji in the message", |d| !d.emoji_open)?;
        step(&format!("the message: {:?}", dialog.message));
        if !dialog.message.contains("Made-up bold note") || dialog.message.trim_end() == "Made-up bold note" {
            note(format!("the message with an emoji reads {:?}", dialog.message));
        }
        let found = self.dialog_box("qa543-ready-measured-light.png")?;
        let (x, y) = format_button(found);
        self.run.click(x, y)?;
        if self.run.dialog_until("the format bar shown", |d| d.formatting).is_err() {
            note("a click on Format shows no format bar".to_owned());
        }
        thread::sleep(SETTLE);
        self.both_themes("format")?;
        let found = self.dialog_box("qa543-format-measured-light.png")?;
        let (x, y) = format_button(found);
        self.run.click(x, y)?;
        self.run.dialog_until("the format bar hidden", |d| !d.formatting)?;
        thread::sleep(SETTLE);
        self.both_themes("ready")
    }

    /// The last post to each of `targets` has the bold word and the emoji.
    fn check_rich(&self, targets: &[To]) {
        for to in targets {
            let sent = self.posts_to(to.chat);
            let content = sent
                .last()
                .and_then(|body| body["content"].as_str())
                .unwrap_or_default();
            if !(content.contains("<strong>bold</strong>") || content.contains("<b>bold</b>")) {
                note(format!(
                    "three people: the post to {} has no bold word: {content:?}",
                    to.name
                ));
            }
            if !(content.contains('\u{2764}') || content.contains("emoji")) {
                note(format!(
                    "three people: the post to {} has no emoji: {content:?}",
                    to.name
                ));
            }
        }
    }

    fn ctrl(&mut self, typed: char) -> SandboxResult {
        let (key, _) = self
            .run
            .input
            .keymap()
            .key_for(typed)
            .ok_or_else(|| format!("{typed:?} isn't on the keymap"))?;
        self.run.input.key_with(&[Modifier::Ctrl], key)
    }

    /// One group chat (Enter in the empty box shares), then three in Teams Dark.
    fn groups(&mut self) -> SandboxResult {
        let groups = self.service.groups.clone();
        let one = [groups[0]];
        let before = [self.posts_to(one[0].chat).len()];
        self.open_from_video()?;
        self.pick(one[0])?;
        self.run.input.key(keys::ENTER)?;
        if self
            .run
            .dialog_until("the dialog closed by Enter in the box", |d| !d.open)
            .is_err()
        {
            note("Enter in the empty box with a recipient does not share".to_owned());
            self.close_dialog()?;
        }
        self.check_toast(&CLIP, "one group")?;
        self.check_posts(&one, &before, &CLIP, "", "one group");
        self.check_shown(one[0], &CLIP, "", "qa543-shown-group-light.png")?;

        let words = "Made-up cut for three crews";
        let before: Vec<usize> = groups.iter().map(|to| self.posts_to(to.chat).len()).collect();
        self.open_from_video()?;
        self.run.theme(true)?;
        for to in &groups {
            self.pick(*to)?;
        }
        self.run.input.key(keys::TAB)?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.type_text(words)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-groups-ready-dark.png")?;
        let found = self.dialog_box("qa543-groups-measured-dark.png")?;
        let (x, y) = share_button(found);
        self.run.click(x, y)?;
        self.run.dialog_until("the dialog closed after Share", |d| !d.open)?;
        self.run.shot(self.sandbox, "qa543-sent-dark.png")?;
        self.check_toast(&CLIP, "three groups")?;
        self.run.theme(false)?;
        self.check_posts(&groups, &before, &CLIP, words, "three groups");
        step("one and three group chats: shared");
        Ok(())
    }

    /// One channel, then the two live channels of the mock's teams.
    fn channels(&mut self) -> SandboxResult {
        let words = "Made-up cut for the channel";
        let before = [self.posts_to(GENERAL.chat).len()];
        self.open_from_video()?;
        self.pick(GENERAL)?;
        self.run.input.key(keys::TAB)?;
        self.run.input.type_text(words)?;
        self.run.input.key(keys::ENTER)?;
        self.run.dialog_until("the dialog closed by Enter", |d| !d.open)?;
        self.check_toast(&CLIP, "one channel")?;
        self.check_posts(&[GENERAL], &before, &CLIP, words, "one channel");

        let words = "Made-up cut for two channels";
        let targets = [GENERAL, MADE_UP_CHANNEL];
        let before: Vec<usize> = targets.iter().map(|to| self.posts_to(to.chat).len()).collect();
        let file = file_named(&self.open_from_document()?.file);
        for to in targets {
            self.pick(to)?;
        }
        self.run.input.key(keys::TAB)?;
        self.run.input.type_text(words)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa543-channels-ready-light.png")?;
        self.run.input.key(keys::ENTER)?;
        self.run.dialog_until("the dialog closed by Enter", |d| !d.open)?;
        self.check_toast(file, "two channels")?;
        self.check_posts(&targets, &before, file, words, "two channels");
        self.check_shown(MADE_UP_CHANNEL, file, words, "qa543-shown-channel-light.png")?;
        step("one and two channels: shared");
        Ok(())
    }
}

// --- Copy link, Share outside Teams ---

impl Walk<'_> {
    fn copy_link(&mut self) -> SandboxResult {
        let link = share_url(&CLIP);
        let page = onedrive_page(&CLIP);
        self.open_from_video()?;
        let found = self.dialog_box("qa543-before-copy-light.png")?;
        let copied = self.run.click_copies(self.sandbox, copy_button(found))?;
        if copied != link {
            note(format!("Copy link copies {copied:?}, expected {link}"));
        }
        if self.run.dialog_until("\"Link created\"", |d| d.link_created).is_err() {
            note("Copy link does not show \"Link created\"".to_owned());
            return self.close_dialog();
        }
        thread::sleep(SETTLE);
        self.both_themes("link")?;
        self.run.shot(self.sandbox, "qa543-link-measured-light.png")?;
        let Some(created) = surface_box(&self.run.last, LINK_SEED) else {
            note("no \"Link created\" box on the screen".to_owned());
            return Ok(());
        };
        step(&format!("\"Link created\" is at {created:?}"));
        let copied = self.run.click_copies(self.sandbox, link_copy(created))?;
        if copied != link {
            note(format!("\"Link created\"'s Copy copies {copied:?}, expected {link}"));
        }
        let (x, y) = link_settings(created);
        let opened = self.run.click_opens(self.sandbox, x, y)?;
        if opened != page {
            note(format!("\"Link created\"'s Settings opens {opened}, expected {page}"));
        }
        let (x, y) = link_close(created);
        self.run.click(x, y)?;
        if self.run.dialog_until("\"Link created\" closed", |d| !d.open).is_err() {
            note("Close on \"Link created\" does not close it".to_owned());
            self.run.input.key(keys::ESC)?;
            self.run
                .dialog_until("\"Link created\" closed by Escape", |d| !d.open)?;
        }
        step("Copy link, \"Link created\", Copy, Settings and Close");
        Ok(())
    }

    fn outside(&mut self) -> SandboxResult {
        for from_video in [false, true] {
            let opened = if from_video {
                self.open_from_video()?
            } else {
                self.open_from_document()?
            };
            let file = file_named(&opened.file);
            let page = onedrive_page(file);
            let found = self.dialog_box("qa543-before-more-light.png")?;
            let (x, y) = more_button(found);
            self.run.click(x, y)?;
            self.run.dialog_until("More actions open", |d| d.more_open)?;
            thread::sleep(SETTLE);
            if !from_video {
                self.both_themes("more")?;
            }
            let (x, y) = outside_row(found);
            let opened = self.run.click_opens(self.sandbox, x, y)?;
            if opened != page {
                note(format!(
                    "Share outside Teams for {} opens {opened}, expected {page}",
                    file.name
                ));
            }
            thread::sleep(SETTLE);
            if self.run.dialog()?.open {
                self.run.input.key(keys::ESC)?;
                self.run.input.key(keys::ESC)?;
                self.run.dialog_until("the dialog closed", |d| !d.open)?;
            }
        }
        step("Share outside Teams opens the file's OneDrive page");
        Ok(())
    }
}

// --- a send error ---

impl Walk<'_> {
    fn send_error(&mut self) -> SandboxResult {
        let words = "Made-up note that the service refuses first";
        let before = [self.posts_to(PERSON_C.chat).len()];
        self.runtime
            .block_on(self.service.mock.refuse_next("POST", "/messages", 403));
        self.open_from_video()?;
        self.pick(PERSON_C)?;
        self.run.input.key(keys::TAB)?;
        self.run.input.type_text(words)?;
        self.run.input.key(keys::ENTER)?;
        let failed = self
            .run
            .dialog_until("the dialog says why", |d| d.problem.is_some() && !d.sharing);
        let Ok(dialog) = failed else {
            note("a refused share shows no error in the dialog".to_owned());
            if self.run.dialog()?.open {
                self.close_dialog()?;
            }
            return Ok(());
        };
        step(&format!("the refused share says {:?}", dialog.problem));
        if !dialog.open || dialog.picked != ["Lab C"] || !dialog.message.contains(words) {
            note(format!(
                "after the refused share: open {}, chips {:?}, message {:?}",
                dialog.open, dialog.picked, dialog.message
            ));
        }
        if !dialog.problem.as_deref().unwrap_or_default().contains("Lab C") {
            note(format!("the error does not name Lab C: {:?}", dialog.problem));
        }
        if self.run.question("toast")?.contains(SHARED) {
            note("a toast says the refused file was shared".to_owned());
        }
        self.both_themes("error")?;
        let mut problems = Vec::new();
        for _ in 0..3 {
            let found = self.dialog_box("qa543-error-measured-light.png")?;
            let (x, y) = share_button(found);
            self.run.click(x, y)?;
            let next = self
                .run
                .dialog_until("the share again", |d| !d.open || (d.problem.is_some() && !d.sharing));
            match next {
                Ok(dialog) if dialog.open => {
                    self.run.shot(self.sandbox, "qa543-error-again-light.png")?;
                    problems.push(dialog.problem.unwrap_or_default());
                }
                Ok(_) => break,
                Err(error) => {
                    note(format!("Share again after the error: {error}"));
                    break;
                }
            }
        }
        step(&format!("the share again said {problems:?}"));
        if self.run.dialog()?.open {
            note(format!("the share again never went: {problems:?}"));
            self.close_dialog()?;
            return Ok(());
        }
        self.check_toast(&CLIP, "the share again")?;
        // the refused post and the one that went
        let sent = self.posts_to(PERSON_C.chat).len() - before[0];
        if sent != 2 {
            note(format!(
                "the share again: {sent} posts to Lab C, expected the refused one and one more"
            ));
        }
        self.check_shown(PERSON_C, &CLIP, words, "qa543-shown-after-error-light.png")?;
        thread::sleep(QUIET);
        let shown = self.shown_in(PERSON_C, words)?;
        if shown != 1 {
            note(format!(
                "the share again: Lab C's chat shows the message {shown} times, expected once"
            ));
        }
        step("a refused share keeps the dialog and says why; Share again sends it once");
        Ok(())
    }
}

impl Walk<'_> {
    /// How many of the rows of `to`'s chat say `words`, as the app shows them; the chat is opened.
    fn shown_in(&mut self, to: To, words: &str) -> SandboxResult<usize> {
        self.run.open(to.chat)?;
        thread::sleep(SETTLE);
        let state = self.run.state_in("chat")?;
        Ok(state.items.iter().filter(|item| item.text.contains(words)).count())
    }
}

/// The posts in `requests`: each conversation and the post's body.
fn posts(requests: &[wiremock::Request]) -> Vec<(String, Value)> {
    requests
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .filter_map(|request| {
            let path = request.url.path();
            let rest = path.strip_suffix("/messages")?;
            let (_, chat) = rest.rsplit_once("/conversations/")?;
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            Some((decoded(chat), body))
        })
        .collect()
}

/// `text` with its `%XX` escapes decoded.
fn decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && let Some(hex) = text.get(at + 1..at + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The entries of a post's `properties.files` (a JSON string of their list).
fn files_of(body: &Value) -> Vec<Value> {
    let files = &body["properties"]["files"];
    let list = files.as_str().and_then(|text| serde_json::from_str::<Value>(text).ok());
    list.or_else(|| files.is_array().then(|| files.clone()))
        .and_then(|list| list.as_array().cloned())
        .unwrap_or_default()
}

// --- where things are ---

/// A point inside the share dialog's left padding, and inside "Link created"'s, at the screen's middle row.
const DIALOG_SEED: (u32, u32) = (SCREEN.0 / 2 - 295, SCREEN.1 / 2);
const LINK_SEED: (u32, u32) = (SCREEN.0 / 2 - 235, SCREEN.1 / 2);

/// The places in the dialog, from its box's edges, as the UI tour's shots of it show them.
const fn share_button(d: Rect) -> (u32, u32) {
    (d.x1 - 55, d.y1 - 42)
}
const fn copy_button(d: Rect) -> (u32, u32) {
    (d.x1 - 126, d.y1 - 42)
}
const fn more_button(d: Rect) -> (u32, u32) {
    (d.x1 - 78, d.y0 + 42)
}
const fn close_button(d: Rect) -> (u32, u32) {
    (d.x1 - 43, d.y0 + 42)
}
const fn outside_row(d: Rect) -> (u32, u32) {
    (d.x1 - 150, d.y0 + 89)
}
const fn format_button(d: Rect) -> (u32, u32) {
    (d.x1 - 75, d.y1 - 94)
}
const fn emoji_button(d: Rect) -> (u32, u32) {
    (d.x1 - 51, d.y1 - 94)
}
/// The first suggestion's middle: the list opens 2 px under the box, its rows 52 px high.
const fn first_suggestion(d: Rect) -> (u32, u32) {
    (d.x0 + 150, d.y0 + 185)
}
/// The middle of the suggestion at `n` (from 0), with the list not scrolled.
const fn nth_suggestion(d: Rect, n: u32) -> (u32, u32) {
    (d.x0 + 150, d.y0 + 185 + 52 * n)
}
/// The last suggestion's middle with the list scrolled to its end: the list is at most 300 px high.
const fn last_suggestion(d: Rect) -> (u32, u32) {
    (d.x0 + 150, d.y0 + 423)
}
const fn box_at(d: Rect) -> (u32, u32) {
    (d.x1 - 200, d.y0 + 136)
}
const fn link_copy(l: Rect) -> (u32, u32) {
    (l.x1 - 48, l.y0 + 77)
}
const fn link_settings(l: Rect) -> (u32, u32) {
    (l.x1 - 54, l.y0 + 116)
}
const fn link_close(l: Rect) -> (u32, u32) {
    (l.x1 - 33, l.y0 + 35)
}

/// "More options" (…) at a card's header's right end.
const fn more_at(card: Rect) -> (u32, u32) {
    (card.x1 - 25, card.y0 + HEADER / 2)
}

/// Share, left of "More options".
const fn share_at(card: Rect) -> (u32, u32) {
    (card.x1 - 56, card.y0 + HEADER / 2)
}

/// Share in a document card's "More options", from the button's middle: the third row of the menu under it.
const fn doc_menu_share((x, y): (u32, u32)) -> (u32, u32) {
    (x + 30, y + 89)
}

/// The box of a dialog's surface round `seed` (a point in its padding): up and down from it while the surface goes on,
/// then left and right along a row near its top. `None` when the seed is not on a theme's surface.
fn surface_box(shot: &image::RgbImage, seed: (u32, u32)) -> Option<Rect> {
    let surface = at(shot, seed.0, seed.1);
    if !near(surface, LIGHT.surface, 3) && !near(surface, DARK.surface, 3) {
        return None;
    }
    let same = |x: u32, y: u32| near(at(shot, x, y), surface, 4);
    let mut top = seed.1;
    while top > 0 && same(seed.0, top - 1) {
        top -= 1;
    }
    let mut bottom = seed.1;
    while bottom + 1 < shot.height() && same(seed.0, bottom + 1) {
        bottom += 1;
    }
    let row = top + 6;
    let mut left = seed.0;
    while left > 0 && same(left - 1, row) {
        left -= 1;
    }
    let mut right = seed.0;
    while right + 1 < shot.width() && same(right + 1, row) {
        right += 1;
    }
    // the box's border is one pixel round its surface
    Some(Rect {
        x0: left - 1,
        y0: top - 1,
        x1: right + 2,
        y1: bottom + 2,
    })
}

// --- the screen ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    const fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    const fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    /// Where the pixel `(x, y)` in the box is, row by row from its top left.
    const fn index(&self, x: u32, y: u32) -> usize {
        (y - self.y0) as usize * self.width() as usize + (x - self.x0) as usize
    }

    const fn overlaps(&self, other: &Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
}

/// A theme's colours a card is found by: the bubble's surface and a card's border.
#[derive(Clone, Copy, Debug)]
struct Look {
    surface: [u8; 3],
    border: [u8; 3],
}

const LIGHT: Look = Look {
    surface: [255, 255, 255],
    border: [224, 224, 224],
};

const DARK: Look = Look {
    surface: [41, 41, 41],
    border: [61, 61, 61],
};

fn load(shot: &Path) -> SandboxResult<image::RgbImage> {
    Ok(image::open(shot)?.to_rgb8())
}

fn near(pixel: [u8; 3], wanted: [u8; 3], within: u8) -> bool {
    pixel.iter().zip(wanted).all(|(got, want)| got.abs_diff(want) <= within)
}

fn at(shot: &image::RgbImage, x: u32, y: u32) -> [u8; 3] {
    if x < shot.width() && y < shot.height() {
        shot.get_pixel(x, y).0
    } else {
        [0, 0, 0]
    }
}

/// The file cards on the screen: a border round a box of a card's size over the bubble's surface.
fn cards_in(shot: &image::RgbImage, look: Look) -> Vec<Rect> {
    let mut found: Vec<Rect> = Vec::new();
    for y in 1..shot.height().saturating_sub(HEADER) {
        for (from, to) in runs(shot, y, 0, shot.width(), |pixel| near(pixel, look.border, 2)) {
            if to - from < 100 {
                continue;
            }
            let Some(card) = card_from(shot, look, y, (from, to)) else {
                continue;
            };
            if (150..=LONE_WIDTH + 4).contains(&card.width())
                && (60..=LONE_HEIGHT).contains(&card.height())
                && !found.iter().any(|known| known.overlaps(&card))
            {
                found.push(card);
            }
        }
    }
    found.sort_by_key(|card| (card.y0, card.x0));
    found
}

/// The card whose top edge is the run `from`..`to` of row `y`, when it is one.
fn card_from(shot: &image::RgbImage, look: Look, y: u32, (from, to): (u32, u32)) -> Option<Rect> {
    let mid = from + (to - from) / 2;
    let below = at(shot, mid, y + 1);
    if !near(at(shot, mid, y - 1), look.surface, 3) || near(below, look.surface, 3) || near(below, look.border, 2) {
        return None;
    }
    let probe = y + 12;
    let left = (from.saturating_sub(12)..=from).find(|&x| near(at(shot, x, probe), look.border, 2))?;
    let right = (to.saturating_sub(1)..to + 12)
        .rev()
        .find(|&x| near(at(shot, x, probe), look.border, 2))?;
    let mut down = probe;
    while down < shot.height() && near(at(shot, left, down), look.border, 2) {
        down += 1;
    }
    let bottom = (down.saturating_sub(2)..(down + 12).min(shot.height() - 1))
        .find(|&row| near(at(shot, mid, row), look.border, 2) && near(at(shot, mid, row + 1), look.surface, 3))?;
    Some(Rect {
        x0: left,
        y0: y,
        x1: right + 1,
        y1: bottom + 1,
    })
}

/// The runs of row `y` from `from` to `to` whose pixels `taken` takes.
fn runs(shot: &image::RgbImage, y: u32, from: u32, to: u32, taken: impl Fn([u8; 3]) -> bool) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    let mut x = from;
    let to = to.min(shot.width());
    while x < to {
        if !taken(at(shot, x, y)) {
            x += 1;
            continue;
        }
        let start = x;
        while x < to && taken(at(shot, x, y)) {
            x += 1;
        }
        found.push((start, x));
    }
    found
}

// --- the made-up files ---

/// A made-up file c shares: its name, its extension, its sharing link's kind and id, and for a video the colours its
/// made-up thumbnail is drawn in.
struct MadeUpFile {
    name: &'static str,
    extension: &'static str,
    kind: char,
    share: &'static str,
    frame: Option<([u8; 3], [u8; 3])>,
}

const CLIP: MadeUpFile = MadeUpFile {
    name: "01-made-up-share-walk.mp4",
    extension: "mp4",
    kind: 'v',
    share: "MadeUpShareLinkShareWalk0201",
    frame: Some(([24, 120, 40], [170, 230, 120])),
};
const DOC: MadeUpFile = MadeUpFile {
    name: "made-up-share-notes.docx",
    extension: "docx",
    kind: 'w',
    share: "MadeUpShareLinkShareNotes0202",
    frame: None,
};

/// The made-up file named `name`: the document's, else the video's.
fn file_named(name: &str) -> &'static MadeUpFile {
    if name == DOC.name { &DOC } else { &CLIP }
}

const SITE: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";
const FOLDER: &str = "/personal/made_up_user_contoso_test/Documents/Microsoft Teams Chat Files";

/// A file's entry in `properties.files`, in the shape `tests/fixtures/chatsvc/files.json` has (made up).
fn file_entry(file: &MadeUpFile, at: usize) -> Value {
    let object = format!("{SITE}Documents/Microsoft Teams Chat Files/{}", file.name);
    json!({
        "itemid": format!("made-up-item-{at}"),
        "fileName": file.name,
        "fileType": file.extension,
        "fileInfo": {"fileUrl": object, "siteUrl": SITE, "shareUrl": share_url(file)},
        "@type": "http://schema.skype.com/File",
        "version": 2,
        "objectUrl": object,
        "type": file.extension,
        "title": file.name,
        "state": "active",
        "filePreview": {"previewUrl": "", "previewHeight": 0, "previewWidth": 0},
    })
}

fn share_url(file: &MadeUpFile) -> String {
    format!(
        "https://contoso-my.sharepoint.com/:{}:/g/personal/made_up_user_contoso_test/{}",
        file.kind, file.share
    )
}

/// The file's OneDrive page, as OneDrive writes it: its path and its folder, each character but a letter, a digit,
/// `-`, `_` and `.` escaped (a space as `%20`).
fn onedrive_page(file: &MadeUpFile) -> String {
    let escaped = |text: &str| {
        text.bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                    char::from(byte).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect::<String>()
    };
    format!(
        "{SITE}_layouts/15/onedrive.aspx?id={}&parent={}",
        escaped(&format!("{FOLDER}/{}", file.name)),
        escaped(FOLDER)
    )
}

// --- the service ---

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    /// The chat of c's message with the two files, and the message's id.
    source: String,
    source_post: String,
    /// Three group chats of a's.
    groups: Vec<To>,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let source = mock
            .add_group("Made-up share source", &[LAB_C, LAB_A])
            .ok_or("no group made")?;
        let files = Value::String(Value::Array(vec![file_entry(&CLIP, 0), file_entry(&DOC, 1)]).to_string());
        let source_post = post_files(&mock, &source, "<p>Made-up files to pass on</p>", files)?;
        let mut groups = Vec::new();
        for (topic, query, members) in [
            ("Made-up crew north", "crew north", &[LAB_A, LAB_B, LAB_C][..]),
            ("Made-up crew south", "crew south", &[LAB_A, LAB_B][..]),
            ("Made-up crew east", "crew east", &[LAB_A, LAB_C][..]),
        ] {
            let chat = mock.add_group(topic, members).ok_or("no group made")?;
            groups.push(To {
                query,
                kind: "chat",
                name: topic,
                chat: Box::leak(chat.into_boxed_str()),
            });
        }
        // more chats that "Made-up" finds than the list shows without a scroll (round 2)
        for (topic, members) in [
            ("Made-up squad one", &[LAB_A, LAB_B][..]),
            ("Made-up squad two", &[LAB_A, LAB_C][..]),
            ("Made-up squad three", &[LAB_A, LAB_B, LAB_C][..]),
        ] {
            mock.add_group(topic, members).ok_or("no group made")?;
        }
        // a group chat of one word, so New message shows a face of one letter (round 3)
        mock.add_group(ONE_WORD, &[LAB_A, LAB_B]).ok_or("no group made")?;
        // a named group chat that "lab" finds next to a person, so "Start a group chat" shows both faces (round 3)
        mock.add_group(LAB_GROUP, &[LAB_A, LAB_B]).ok_or("no group made")?;
        // b and the outsider say something in their 1:1 chats with a, so the chats are named after them
        for (chat, who) in [(AB_CHAT, (LAB_B, "Lab B")), (AX_CHAT, (OUTSIDER, "Outsider X"))] {
            mock.add_message(chat, who, "RichText/Html", "<p>Made-up hello</p>")
                .ok_or("a hello refused")?;
        }
        serve_file(&mock, runtime, &CLIP, 0)?;
        for (at, file) in [&CLIP, &DOC].into_iter().enumerate() {
            // what its link lets people do, as "Share this file" reads it on the sharer's site
            let token = teams_proto::chat::share_token(&share_url(file));
            let permission = json!({"id": format!("made-up-permission-{at}"), "roles": ["write"],
                "link": {"scope": "anonymous", "type": "edit", "localizedDescription": ANYONE_CAN_EDIT}});
            runtime.block_on(mock.serve_bytes(
                &format!("/personal/made_up_user_contoso_test/_api/v2.0/shares/{token}/permission"),
                "application/json",
                permission.to_string().into_bytes(),
            ));
        }
        let _ = mock.take_events(LAB_A);
        let _ = mock.take_events(LAB_B);
        let _ = mock.take_events(LAB_C);
        Ok(Self {
            mock,
            trouter,
            source,
            source_post,
            groups,
        })
    }
}

/// c's post of `html` in `chat`, `properties.files` set to `files`; its id.
fn post_files(mock: &ChatServiceMock, chat: &str, html: &str, files: Value) -> SandboxResult<String> {
    let id = mock.post_as(LAB_C, chat, html).ok_or("c's post refused")?;
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(chat, &client_message_id, |message| {
        message["properties"]["files"] = files;
    });
    changed.then_some(id).ok_or_else(|| "c's post isn't in the mock".into())
}

/// The mock's thumbnail of video `file` (a made-up picture). `at` makes its links its own.
fn serve_file(
    mock: &ChatServiceMock,
    runtime: &tokio::runtime::Runtime,
    file: &MadeUpFile,
    at: usize,
) -> SandboxResult {
    let Some((dark, light)) = file.frame else {
        return Ok(());
    };
    let read = teams_proto::FileThumbnails::of(&share_url(file)).ok_or("a made-up link no read names")?;
    let token = read.share();
    let picture = format!("/transform/thumbnail/made-up-qa543-{at}");
    let list = json!({"value": [{"id": "0", "large": {
        "height": 450, "width": 800,
        "url": format!("https://westus1-mediap.svc.ms{picture}?provider=spo&docid=made-up"),
    }}]});
    runtime.block_on(mock.serve_bytes(
        &format!("/v1.0/shares/{token}/driveItem/thumbnails"),
        "application/json",
        list.to_string().into_bytes(),
    ));
    let mut png = Vec::new();
    made_up_frame(dark, light).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&picture, "image/png", png));
    Ok(())
}

/// A made-up video frame, 800 by 450: a gradient from `dark` to `light` with a bar.
fn made_up_frame(dark: [u8; 3], light: [u8; 3]) -> image::RgbaImage {
    image::RgbaImage::from_fn(800, 450, |x, y| {
        let mix = |from: u8, to: u8| {
            let (from, to) = (u32::from(from), u32::from(to));
            let value = (from * (800 - x) + to * x) / 800;
            u8::try_from(value).unwrap_or(u8::MAX)
        };
        if (360..440).contains(&x) && y > 120 {
            image::Rgba([10, 10, 10, 255])
        } else {
            image::Rgba([
                mix(dark[0], light[0]),
                mix(dark[1], light[1]),
                mix(dark[2], light[2]),
                255,
            ])
        }
    })
}

// --- what the app shows ---

/// A message's row as the child tells it.
#[derive(Clone, Debug, serde::Deserialize)]
struct Item {
    id: String,
    text: String,
    files: Vec<String>,
    unshown: bool,
}

/// A view's rows as the child tells them.
#[derive(Debug, serde::Deserialize)]
struct State {
    items: Vec<Item>,
}

impl State {
    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// A suggestion under the box: its kind (`person`, `chat`, `channel`) and its name.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq)]
struct Suggestion {
    kind: String,
    name: String,
}

/// "Share this file" as the child tells it.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the dialog's own flags, as the child reads them, one field each"
)]
struct Dialog {
    open: bool,
    file: String,
    permission: Option<String>,
    picked: Vec<String>,
    suggestions: Vec<Suggestion>,
    can_share: bool,
    sharing: bool,
    problem: Option<String>,
    link_created: bool,
    more_open: bool,
    emoji_open: bool,
    formatting: bool,
    message: String,
    outside: Option<String>,
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
}

/// The app running in the sandbox: its process, its view trace, its control socket and the user's pointer.
struct AppRun {
    running: Running,
    trace: TraceReader,
    ctl: PathBuf,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar, away from every card and button.
    rest: (u32, u32),
    asked: u32,
    /// The last screenshot.
    last: image::RgbImage,
}

impl AppRun {
    /// Starts the app on `service` with its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, service: &Service, dir: &Path) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let token = service.mock.token(LAB_A).ok_or("the mock's token for a")?;
        let origin = service.mock.origin();
        let dir_text = dir.to_string_lossy().into_owned();
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
            rest: (200, 24),
            asked: 0,
            last: image::RgbImage::new(1, 1),
        };
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

    /// `ngteamsctl open chat <chat>`, then the trace's `chat opened`.
    fn open(&mut self, chat: &str) -> SandboxResult {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        client.request(
            None,
            Request::Open {
                target: OpenTarget::Chat { chat: chat.to_owned() },
            },
        )?;
        self.trace.next_where(&format!("chat {chat} opened"), |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
        Ok(())
    }

    /// `ngteamsctl open channel <channel>`.
    fn open_channel(&mut self, channel: &str) -> SandboxResult {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        client.request(
            None,
            Request::Open {
                target: OpenTarget::Channel {
                    channel: channel.to_owned(),
                },
            },
        )?;
        thread::sleep(SETTLE);
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

    /// The rows of `view` (`chat` or `posts`).
    fn state_in(&mut self, view: &str) -> SandboxResult<State> {
        let answer = self.question(&format!("state {view}"))?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state of `view` that `wanted` takes, named `what` in the error.
    fn until(&mut self, view: &str, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state_in(view)?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state:#?}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// "Share this file" as it is now.
    fn dialog(&mut self) -> SandboxResult<Dialog> {
        let answer = self.question("dialog")?;
        serde_json::from_str(&answer).map_err(|error| format!("the dialog {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for the dialog as `wanted` takes it, named `what` in the error.
    fn dialog_until(&mut self, what: &str, wanted: impl Fn(&Dialog) -> bool) -> SandboxResult<Dialog> {
        let start = Instant::now();
        loop {
            let dialog = self.dialog()?;
            if wanted(&dialog) {
                return Ok(dialog);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {dialog:#?}").into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// The theme shown now (`light` unless dark was asked).
    fn theme_now(&self) -> String {
        fs::read_to_string(self.dir.join(THEME_FILE)).unwrap_or_else(|_| "light".to_owned())
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        if fs::read_to_string(self.dir.join(THEME_FILE)).is_ok_and(|now| now == key) {
            return Ok(());
        }
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    /// A screenshot of the screen, kept as `target/ui-shots/<name>` (and in the evidence directory); the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        fs::copy(&png, shots_dir()?.join(name))?;
        if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
            let evidence = PathBuf::from(evidence);
            fs::create_dir_all(&evidence)?;
            fs::copy(&png, evidence.join(name))?;
        }
        self.last = load(&png)?;
        Ok(png)
    }

    /// Clicks (`x`, `y`) as a user does.
    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// Clicks (`x`, `y`) as a user does; the link the app asked the opener for.
    fn click_opens(&mut self, sandbox: &Sandbox, x: u32, y: u32) -> SandboxResult<String> {
        let before = sandbox.opened()?.len();
        self.input.click(x, y, keys::BTN_LEFT)?;
        let opened = wait_until(WAIT, || sandbox.opened().ok()?.get(before).cloned())
            .ok_or_else(|| format!("a click at ({x}, {y}) opened nothing"))?;
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(300));
        opened
            .args
            .last()
            .cloned()
            .ok_or_else(|| format!("the opener got no address: {opened:?}").into())
    }

    /// Clicks (`x`, `y`); what the clipboard holds then.
    fn click_copies(&mut self, sandbox: &Sandbox, (x, y): (u32, u32)) -> SandboxResult<String> {
        sandbox.set_clipboard(vec![(
            "text/plain;charset=utf-8".to_owned(),
            b"made-up before".to_vec(),
        )])?;
        thread::sleep(Duration::from_millis(300));
        self.click(x, y)?;
        let copied = wait_until(Duration::from_secs(5), || {
            clipboard_text(sandbox).ok().filter(|text| text != "made-up before")
        });
        self.input.move_to(self.rest.0, self.rest.1)?;
        Ok(copied.unwrap_or_else(|| "(the clipboard did not change)".to_owned()))
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa543-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits through the tray menu, as a user does; once the app ended and released its engine.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.running.pid());
        let host = sandbox.bus().connect()?;
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
        let quit = || menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32));
        if wait_until(Duration::from_secs(10), || quit().ok()).is_none() {
            step("no tray item to quit through: the app is stopped");
            let _ = self.running.fail(&"stopped by the walk");
            return Ok(());
        }
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

/// The text the sandbox's clipboard holds.
fn clipboard_text(sandbox: &Sandbox) -> SandboxResult<String> {
    let offers = sandbox.read_clipboard()?;
    offers
        .iter()
        .find(|(mime, _)| mime.starts_with("text/plain"))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        .ok_or_else(|| "no text on the clipboard".into())
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
        client: Arc::new(ChatClient::new(http()?.with_ams(AMS_BASE)?.with_csa(CSA_BASE)?)),
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

/// Answers the parent's questions as they come.
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
                .update(cx, |kit, _, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, cx: &mut App) -> String {
    let (verb, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "state" => view_of(shell, rest, cx).map_or_else(|| json!({"items": []}), |view| state_of(view.read(cx))),
        "dialog" => shell
            .read(cx)
            .share_dialog()
            .cloned()
            .map_or_else(|| json!({"open": false}), |dialog| dialog_of(&dialog, cx)),
        "toast" => json!(shell.read(cx).toast().map(ToString::to_string).unwrap_or_default()),
        "new_message" => shell.read(cx).new_chat().map_or_else(
            || json!({"open": false, "suggestions": []}),
            |view| {
                let names: Vec<String> = view
                    .read(cx)
                    .suggestions()
                    .iter()
                    .map(|suggested| match suggested {
                        Suggested::Person { name, .. } => name.to_string(),
                        Suggested::Chat(row) => row.name.to_string(),
                    })
                    .collect();
                json!({"open": true, "suggestions": names})
            },
        ),
        "people" => shell.read(cx).people().map_or_else(
            || json!({"open": false, "picking": false, "suggestions": []}),
            |view| {
                let view = view.read(cx);
                let names: Vec<String> = view
                    .suggestions()
                    .iter()
                    .map(|picked| match picked {
                        Picked::Person(person) => person.name.to_string(),
                        Picked::Chat(row) => row.name.to_string(),
                    })
                    .collect();
                json!({"open": true, "picking": view.picking(), "suggestions": names})
            },
        ),
        other => json!(format!("unknown {other}")),
    }
    .to_string()
}

/// The dialog, as [`Dialog`] reads it.
fn dialog_of(dialog: &Entity<ShareFileDialog>, cx: &App) -> Value {
    let dialog = dialog.read(cx);
    let suggestions: Vec<Value> = dialog
        .suggestions()
        .iter()
        .map(|target| {
            let kind = match target {
                Target::Person { .. } => "person",
                Target::Chat(_) => "chat",
                Target::Channel { .. } => "channel",
            };
            json!({"kind": kind, "name": target.name().to_string()})
        })
        .collect();
    let picked: Vec<String> = dialog.picked().iter().map(|target| target.name().to_string()).collect();
    let problem = dialog.problem().map(ToString::to_string);
    let sharing = !dialog.picked().is_empty() && !dialog.can_share();
    json!({
        "open": true,
        "file": dialog.file().name,
        "permission": dialog.permission().and_then(|permission| permission.description.clone()),
        "picked": picked,
        "suggestions": suggestions,
        "can_share": dialog.can_share(),
        "sharing": sharing,
        "problem": problem,
        "link_created": dialog.link_created(),
        "more_open": dialog.more_open(),
        "emoji_open": dialog.emoji_open(),
        "formatting": dialog.formatting(),
        "message": dialog.message_text(cx),
        "outside": dialog.outside_page(),
    })
}

/// The view `which` names: the channel's `posts`, else the chat open.
fn view_of(shell: &Entity<Shell>, which: &str, cx: &App) -> Option<Entity<ChatView>> {
    let shell = shell.read(cx);
    match which {
        "posts" => Some(shell.channel()?.read(cx).posts().clone()),
        _ => shell.chat().cloned(),
    }
}

/// The view's rows, as [`State`] reads them.
fn state_of(view: &ChatView) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({ "items": items })
}

fn item_of(item: &MessageItem) -> Value {
    let text = match &item.body {
        Body::Text(rendered) => rendered
            .0
            .blocks
            .iter()
            .flat_map(Block::plain_lines)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let files: Vec<&str> = item.files.iter().map(|file| file.name.as_str()).collect();
    json!({ "id": item.id, "text": text, "files": files, "unshown": item.unshown.any() })
}
