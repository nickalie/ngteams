//! The faces a call shows of people whose camera is off, drawn on the real platform in the UI sandbox (`harness =
//! false`: GPUI runs on the process's main thread), for their screenshots: the call window over the fake engine
//! (`common`), the people's profile pictures made-up portraits. Each scene is a call of its own: a 1:1 call with and
//! without pictures (the other one's tile and the account's self view), ringing with the other one's picture, a group
//! call whose participants have a picture or not (one speaking, one muted with a hand up), the same with People open,
//! the group call in Teams Dark, and three tiles with two hands raised in turn (Teams Light and Dark, and with the
//! account's own hand). Then a meeting whose chat invited three others with People open: the account alone in it
//! (nobody's tile, the three under "Others invited"), and once Ben joined. The test runs itself again inside the
//! sandbox per scene; the parent screenshots
//! each window into `target/ui-shots/call-faces-*.png`.
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

use common::{FakeCore, FakeEvents, LAB_A, LAB_B};
use gpui::{App, AppContext as _, Window};
use teams_core::{CallId, CallParticipant, CallStatus, CallView, Event, NameFrom, Person, RegistrationReport};
use teams_store::{Conversation, ConversationKind, Member, StoredPicture};
use teams_ui::call_stage::CallStage;
use teams_ui::call_window::CallWindows;
use teams_ui::calls::{self, Ask};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::ShellParts;
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::window::KitRoot;
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

/// Which scene the child draws: its name.
const CHILD_ENV: &str = "TEAMS_UI_CALL_FACES_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-call-faces-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 30;

/// The scenes, each screenshotted as `call-faces-<name>.png`.
const SCENES: [&str; 12] = [
    "one-to-one-pictures",
    "one-to-one-initials",
    "ringing-picture",
    "group",
    "group-people",
    "group-dark",
    "group-speaker",
    "group-hands",
    "group-hands-dark",
    "group-hands-self",
    "meeting-alone",
    "meeting-joined",
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(scene) => child(&scene),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("call faces window test: {error}");
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
    let Some(sandbox) = sandbox_or_skip("teams-ui call faces window")? else {
        return Ok(());
    };
    for scene in SCENES {
        shot(&sandbox, scene).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    }
    say("teams-ui call faces window: ok");
    Ok(())
}

/// Runs the child drawing `scene` and screenshots its call window as `call-faces-<scene>.png`.
fn shot(sandbox: &Sandbox, scene: &str) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, scene)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("call-faces-{scene}.png");
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

// --- the child: the call window over the fake engine ---

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const LAB_D: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const GROUP: &str = "19:0000000000000000000000000000trio@thread.v2";
const MEETING: &str = "19:meeting_MadeUpMeeting0001@thread.v2";

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A made-up portrait in `colours` (background, face, shirt): a head and shoulders, as a profile photo is framed.
fn portrait((ground, face, shirt): ([u8; 3], [u8; 3], [u8; 3])) -> StoredPicture {
    const SIDE: u32 = 240;
    let image = image::RgbImage::from_fn(SIDE, SIDE, |px, py| {
        let (x, y) = (f64::from(px), f64::from(py));
        let head = ((x - 120.0) / 46.0).powi(2) + ((y - 100.0) / 56.0).powi(2) <= 1.0;
        let shoulders = y > 160.0 && ((x - 120.0) / 100.0).powi(2) + ((y - 250.0) / 90.0).powi(2) <= 1.0;
        let shade = u8::try_from(240_u32.saturating_sub((px + py) / 4)).unwrap_or(255);
        let tint = |c: u8| u8::try_from(u32::from(c) * u32::from(shade) / 255).unwrap_or(c);
        let colour = if head {
            face
        } else if shoulders {
            shirt
        } else {
            [tint(ground[0]), tint(ground[1]), tint(ground[2])]
        };
        image::Rgb(colour)
    });
    let mut bytes = Vec::new();
    if image
        .write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .is_err()
    {
        bytes.clear();
    }
    StoredPicture {
        media_type: "image/jpeg".to_owned(),
        bytes,
    }
}

/// `mri`, named `name`, with `picture` when given, as the engine gives them.
fn person(core: &FakeCore, mri: &str, name: &str, picture: Option<StoredPicture>) {
    locked(&core.people).insert(
        mri.to_owned(),
        Person {
            mri: mri.to_owned(),
            name: Some(name.to_owned()),
            from: Some(NameFrom::Graph),
            picture,
            looking: false,
        },
    );
}

/// The store and the people of `scene`.
fn seed(core: &FakeCore, scene: &str) {
    let pictures = scene != "one-to-one-initials";
    let photo = |colours| pictures.then(|| portrait(colours));
    person(
        core,
        LAB_A,
        "Ann Able",
        photo(([70, 110, 160], [230, 190, 160], [40, 60, 90])),
    );
    person(
        core,
        LAB_B,
        "Ben Bright",
        photo(([150, 170, 120], [200, 150, 120], [140, 40, 40])),
    );
    // c and d have no picture: their initials
    person(core, LAB_C, "Cleo Sea", None);
    person(core, LAB_D, "Dan Dee", None);
    locked(&core.conversations).extend([
        Conversation {
            id: AB.to_owned(),
            kind: ConversationKind::OneToOne,
            topic: None,
            version: 1,
            last_activity: Some(1),
        },
        Conversation {
            id: GROUP.to_owned(),
            kind: ConversationKind::Group,
            topic: Some("Release crew".to_owned()),
            version: 1,
            last_activity: Some(2),
        },
    ]);
    locked(&core.members).extend([
        (AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]),
        (
            GROUP.to_owned(),
            [LAB_A, LAB_B, LAB_C, LAB_D]
                .iter()
                .map(|mri| Member::new(mri, None))
                .collect(),
        ),
        (
            MEETING.to_owned(),
            [LAB_A, LAB_B, LAB_C, LAB_D]
                .iter()
                .map(|mri| Member::new(mri, None))
                .collect(),
        ),
    ]);
}

/// One participant of the group call's roster.
fn participant(mri: &str, name: &str, (speaking, muted, hand): (bool, bool, bool)) -> CallParticipant {
    CallParticipant {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        video_on: false,
        line: None,
        speaking,
        muted,
        hand_raised: hand,
        organizer: false,
        joined: None,
        sharing: false,
    }
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

fn child(scene: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let scene = scene.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        teams_ui::window::init(cx);
        open(scene, cx);
    });
    Ok(())
}

/// The call window of `scene`'s call, once it is up as the scene has it.
fn open(scene: String, cx: &mut App) {
    let core = Arc::new(FakeCore::default());
    seed(&core, &scene);
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::with_calls(Some(teams_proto::Mri::new(ECHO)), cx);
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let theme = if scene.ends_with("-dark") { DARK_KEY } else { LIGHT_KEY };
    let appearance = Appearance {
        follow_system: false,
        theme,
        ..Appearance::default()
    };
    teams_ui::theme::set_appearance(&appearance, cx);
    let settings = Settings {
        appearance,
        ..Settings::default()
    };
    let trace = Arc::new(ViewTrace::off());
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models: models.clone(),
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::clone(&trace),
    };
    let windows = cx.new(|cx| CallWindows::new(APP_ID, cx));
    windows.update(cx, |windows, cx| windows.add(parts.clone(), cx));
    let _ = events.send(Event::CallingRegistration(RegistrationReport::Registered));
    let group = scene.starts_with("group");
    let meeting = scene.starts_with("meeting");
    let chat = if group { GROUP } else { AB };
    let core = parts.core.clone();
    let calls_model = models.calls.clone();
    let send = events.clone();
    // the registration drained first, then the call asked for and told up
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(300)).await;
        cx.update(|cx| {
            if meeting {
                calls_model.update(cx, |calls, cx| calls.open_prejoin(meeting_to_join(), cx));
                calls::join_now(&calls_model, &core, &trace, cx);
            } else {
                calls::ask(&calls_model, &core, &trace, Ask::Call { chat: chat.to_owned() }, cx);
            }
        });
        cx.background_executor().timer(Duration::from_millis(300)).await;
        let call = CallId::for_tests(1);
        let status = if scene == "ringing-picture" {
            CallStatus::Ringing
        } else {
            CallStatus::Connected
        };
        let _ = send.send(Event::Call { call, status });
        if scene.starts_with("group-hands") {
            hands(&send, call, scene.ends_with("-self"), cx).await;
        } else if let Some(view) = roster(&scene) {
            let _ = send.send(Event::CallParticipants { call, view });
        }
        cx.background_executor().timer(Duration::from_millis(500)).await;
        cx.update(|cx| ready(&windows, &scene, cx));
        // the bridge, the windows and the event channel live as long as the app
        mem::forget((bridge, send));
    })
    .detach();
}

/// Who `scene`'s roster says is in its call besides the account, unless it says nothing.
fn roster(scene: &str) -> Option<CallView> {
    let participants = match scene {
        "meeting-joined" => vec![participant(LAB_B, "Ben Bright", (false, false, false))],
        _ if scene.starts_with("meeting") => Vec::new(),
        _ if scene.starts_with("group") => vec![
            participant(LAB_B, "Ben Bright", (false, false, false)),
            participant(LAB_C, "Cleo Sea", (true, false, false)),
            participant(LAB_D, "Dan Dee", (false, true, true)),
        ],
        _ => return None,
    };
    Some(CallView {
        participants,
        speaking: false,
        hearing: true,
    })
}

/// The made-up meeting, joined by its chat.
fn meeting_to_join() -> calls::MeetingToJoin {
    calls::MeetingToJoin::Chat {
        chat: MEETING.to_owned(),
        meeting: teams_core::MeetingInfo {
            organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
            tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
        },
        title: "Weekly sync".to_owned(),
        post: None,
    }
}

/// Three tiles, the hands raised in turn: Dan's first, then Ben's (or, with `own`, the account's), each its own roster
/// delivery so the line of hands has its order.
async fn hands(send: &mpsc::UnboundedSender<Event>, call: CallId, own: bool, cx: &mut gpui::AsyncApp) {
    let roster = |ben: bool| CallView {
        participants: vec![
            participant(LAB_B, "Ben Bright", (false, false, ben)),
            participant(LAB_D, "Dan Dee", (false, true, true)),
        ],
        speaking: false,
        hearing: true,
    };
    let _ = send.send(Event::CallParticipants {
        call,
        view: roster(false),
    });
    cx.background_executor().timer(Duration::from_millis(200)).await;
    let _ = send.send(if own {
        Event::CallOwn {
            call,
            hand_raised: true,
            organizer: false,
        }
    } else {
        Event::CallParticipants {
            call,
            view: roster(true),
        }
    });
}

/// The call window as `scene` shows it, then `ready` once it settled.
fn ready(windows: &gpui::Entity<CallWindows>, scene: &str, cx: &mut App) {
    let Some(window) = windows.read(cx).window() else {
        eprintln!("no call window opened");
        cx.quit();
        return;
    };
    let scene = scene.to_owned();
    let _ = window.update(cx, move |kit: &mut KitRoot, window, cx| {
        if let Ok(stage) = kit.view().clone().downcast::<CallStage>() {
            stage.update(cx, |stage, cx| match scene.as_str() {
                "group-people" | "meeting-alone" | "meeting-joined" => stage.show_roster(true, cx),
                "group-speaker" => stage.set_layout(teams_ui::call_stage::Layout::Speaker, cx),
                _ => {}
            });
        }
        window.refresh();
        after_frames(window, READY_FRAMES, |_, _| say("ready"));
    });
}
