//! The chat list drawn on the real platform, in the UI sandbox (`harness = false`: GPUI runs on the process's main
//! thread), for its screenshots: the app's window over the fake engine (`common`), its store seeded with made-up
//! chats as Teams lists them: the account's own notes under "Favorites" with its presence, then 1:1 chats with their
//! presence badges (available, away, busy, do not disturb, offline, one unknown), group chats split between two
//! members (one with a topic, whose message starts with a mention of everyone in another language, one named by its
//! members' first names), meetings' chats with their calendar, and a bot's
//! chat named "Bot". The test runs itself again inside the sandbox once per theme (Teams Light, Teams Dark); the parent
//! screenshots each window into `target/ui-shots/chat-list-*.png`.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
#[path = "../../common/mod.rs"]
mod common;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents, LAB_A};
use gpui::{App, AppContext as _, Window};
use teams_core::{Availability, ChatListing, Event};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::chat_list::NOTES;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::ShellParts;
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in: the theme's key.
const CHILD_ENV: &str = "TEAMS_UI_CHAT_LIST_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-chat-list-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 30;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(theme) => child(&theme),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("chat list window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("teams-ui chat list window")? else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        shot(&sandbox, theme, name).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    }
    say("teams-ui chat list window: ok");
    Ok(())
}

/// Runs the child in `theme` and screenshots its window as `chat-list-<name>.png`.
fn shot(sandbox: &Sandbox, theme: &str, name: &str) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, theme)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("chat-list-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots_dir()?.join(&file);
    fs::copy(&png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    Ok(())
}

// --- the child: the app's window over the fake engine ---

const DANA: &str = "8:orgid:00000000-0000-4000-8000-00000000d001";
const ELI: &str = "8:orgid:00000000-0000-4000-8000-00000000d002";
const FRAN: &str = "8:orgid:00000000-0000-4000-8000-00000000d003";
const GUS: &str = "8:orgid:00000000-0000-4000-8000-00000000d004";
const HANA: &str = "8:orgid:00000000-0000-4000-8000-00000000d005";
const IVO: &str = "8:orgid:00000000-0000-4000-8000-00000000d006";
const BOT: &str = "28:00000000-0000-4000-8000-0000000000bb";

/// The 1:1 chat of a and `other` (by their object ids, as the service names it).
fn one_to_one(other: &str) -> String {
    let oid = |mri: &str| mri.rsplit(':').next().unwrap_or_default().to_owned();
    format!("19:{}_{}@unq.gbl.spaces", oid(LAB_A), oid(other))
}

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn text(conversation: &str, sender: &str, content: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: arrival.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: format!("<p>{content}</p>"),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

/// A seeded chat: its id, kind and topic, its members besides a, and its one message's sender and text (none when
/// empty).
struct Seeded {
    id: String,
    kind: ConversationKind,
    topic: Option<&'static str>,
    people: &'static [&'static str],
    said: (&'static str, &'static str),
}

const QUIET: &str = "19:meeting_000000000000000000000000quiet@thread.v2";

/// The group chat whose message starts with a mention of everyone in a paragraph of its own, labeled in its sender's
/// language (a made-up "Все"): its row previews it as "Everyone", then the text.
const DESIGN_CREW: &str = "19:0000000000000000000000000000team@thread.v2";

/// `message` (Dana's in [`DESIGN_CREW`]) starting with that mention of everyone.
fn mentioning_everyone(message: &mut Message) {
    message.content = format!(
        "<p><readonly class=\"skipProofing\" itemtype=\"http://schema.skype.com/Mention\" spellcheck=\"false\"><span \
         itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">Все</span></readonly></p><p>&nbsp;</p>{}",
        message.content
    );
    message.properties = Some(
        serde_json::json!({"mentions": [{"itemid": "0", "mri": DESIGN_CREW, "mentionType": "everyone",
            "displayName": "Все"}]})
        .to_string(),
    );
}

/// A 1:1 chat of a with `other`, the last said by `sender`.
fn pair(other: &'static str, sender: &'static str, said: &'static str) -> Seeded {
    let people: &'static [&'static str] = match other {
        DANA => &[DANA],
        ELI => &[ELI],
        FRAN => &[FRAN],
        GUS => &[GUS],
        HANA => &[HANA],
        IVO => &[IVO],
        _ => &[BOT],
    };
    Seeded {
        id: one_to_one(other),
        kind: ConversationKind::OneToOne,
        topic: None,
        people,
        said: (sender, said),
    }
}

/// A group or meeting chat.
fn thread(
    id: &str,
    kind: ConversationKind,
    topic: Option<&'static str>,
    people: &'static [&'static str],
    said: (&'static str, &'static str),
) -> Seeded {
    Seeded {
        id: id.to_owned(),
        kind,
        topic,
        people,
        said,
    }
}

/// The made-up chats (see the module docs), the newest first.
fn chats() -> Vec<Seeded> {
    let meeting = || ConversationKind::Other("thread_type:meeting".to_owned());
    vec![
        thread(
            DESIGN_CREW,
            ConversationKind::Group,
            Some("Design crew"),
            &[DANA, ELI, FRAN],
            (DANA, "Setup is ready, try it"),
        ),
        pair(DANA, LAB_A, "Here is the new build"),
        pair(ELI, ELI, "Looks good to me"),
        pair(FRAN, FRAN, "On my way"),
        thread(
            "19:meeting_0000000000000000000000standup@thread.v2",
            meeting(),
            Some("Daily stand-up"),
            &[DANA, ELI],
            (DANA, "Cancelled for today"),
        ),
        pair(GUS, LAB_A, "Play"),
        thread(
            "19:meeting_00000000000000000000000review@thread.v2",
            meeting(),
            Some("Sprint review"),
            &[ELI, GUS],
            (GUS, "Slides are in the folder"),
        ),
        pair(HANA, HANA, "Accepted"),
        pair(BOT, BOT, "Your report is ready"),
        thread(
            "19:00000000000000000000000000unnamed@thread.v2",
            ConversationKind::Group,
            None,
            &[ELI, DANA],
            (DANA, "Thank you!"),
        ),
        pair(IVO, IVO, "See you tomorrow"),
        thread(QUIET, meeting(), Some("Nobody wrote here"), &[ELI], (ELI, "")),
    ]
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The made-up store: [`chats`] a day apart in steps of seven hours, Eli's with three unread, everyone named, a's
/// notes its one favourite and the quiet meeting empty.
fn seed(core: &FakeCore) {
    let now = chrono::Utc::now().timestamp_millis();
    let hour = 3_600_000;
    for (at, seeded) in (0_i64..).zip(chats()) {
        let last = now - at * 7 * hour;
        let (sender, said) = seeded.said;
        locked(&core.conversations).push(chat(&seeded.id, seeded.kind, seeded.topic, last));
        if !said.is_empty() {
            let mut message = text(&seeded.id, sender, said, last);
            if seeded.id == DESIGN_CREW {
                mentioning_everyone(&mut message);
            }
            locked(&core.messages).insert(seeded.id.clone(), vec![message]);
        }
        let mut stored = vec![Member::new(LAB_A, None)];
        stored.extend(seeded.people.iter().map(|mri| Member::new(mri, None)));
        locked(&core.members).insert(seeded.id, stored);
    }
    locked(&core.unread).insert(one_to_one(ELI), 3);
    for (mri, name) in [
        (LAB_A, "Ann Able"),
        (DANA, "Dana Fox"),
        (ELI, "Eli Stone"),
        (FRAN, "Fran Lake"),
        (GUS, "Gus Moor"),
        (HANA, "Hana Reed"),
        (IVO, "Ivo Park"),
    ] {
        locked(&core.names).insert(mri.to_owned(), name.to_owned());
    }
    *locked(&core.listing) = ChatListing {
        favorites: Some(vec![NOTES.to_owned()]),
        empty: vec![QUIET.to_owned()],
    };
}

/// The presences the engine tells: one of each kind Teams badges, Ivo's not known.
fn presences() -> Vec<(&'static str, Availability)> {
    vec![
        (LAB_A, Availability::Available),
        (DANA, Availability::Away),
        (ELI, Availability::Available),
        (FRAN, Availability::Busy),
        (GUS, Availability::DoNotDisturb),
        (HANA, Availability::Offline),
    ]
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        #[cfg(windows)]
        if !fixture::scene_activated(window) {
            after_frames(window, frames, done);
            window.refresh();
            return;
        }
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn child(theme: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

fn open(theme: &'static str, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    seed(&core);
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let shared: Arc<dyn Core> = core;
    let drained = models.clone();
    let bridge = cx.new(|cx| Bridge::start(source, shared, drained, cx));
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, APP_ID, "Chat list".into(), parts)?;
    for (mri, availability) in presences() {
        let _ = events.send(Event::Presence {
            mri: mri.to_owned(),
            availability,
        });
    }
    // the bridge and the event channel live as long as the app
    mem::forget((bridge, events));
    window.update(cx, |_, window, _| {
        after_frames(window, READY_FRAMES, |_, _| say("ready"));
    })?;
    Ok(())
}
