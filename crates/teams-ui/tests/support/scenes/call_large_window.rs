//! Calls of ten people and more, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread), for their screenshots: the call window over the fake engine (`common`), a group call of
//! 10, 16, 25 and 50 made-up participants, some with their camera on (a test pattern), the rest showing their
//! initials; in the gallery, in the speaker view and with someone's screen shared; on a small, a laptop's and a full
//! HD screen. The test runs itself again inside the sandbox per scene; the parent screenshots each window into
//! `target/ui-shots/call-large-<scene>.png`.
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
use teams_core::{CallId, CallParticipant, CallStatus, CallView, Event, RegistrationReport, VideoOutput};
use teams_media::devices::pattern::TestPattern;
use teams_store::{Conversation, ConversationKind, Member};
use teams_ui::call_stage::{CallStage, Layout};
use teams_ui::call_window::CallWindows;
use teams_ui::calls::{self, Ask};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::ShellParts;
use teams_ui::theme::{Appearance, LIGHT_KEY};
use teams_ui::video::VideoFeed;
use teams_ui::window::KitRoot;
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "the shared fixture's unsized sandbox serves the other scenes")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sized_sandbox_or_skip};

/// Which scene the child draws: its name.
const CHILD_ENV: &str = "TEAMS_UI_CALL_LARGE_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-call-large-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 30;

/// The screens the scenes are drawn on: their name and size, px.
const SCREENS: [(&str, (u32, u32)); 3] = [("small", (800, 560)), ("laptop", (1280, 800)), ("full", (1920, 1080))];

/// The scenes, each screenshotted on each screen as `call-large-<scene>-<screen>.png`: how the stage is laid out and
/// how many are in the call.
const SCENES: [&str; 8] = [
    "gallery-10",
    "gallery-16",
    "gallery-25",
    "gallery-50",
    "speaker-25",
    "share-10",
    "share-25",
    "share-50",
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(scene) => child(&scene),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("call large window test: {error}");
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
    for (screen, size) in SCREENS {
        let Some(sandbox) = sized_sandbox_or_skip("teams-ui call large window", Some(size))? else {
            return Ok(());
        };
        for scene in SCENES {
            shot(&sandbox, scene, screen).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    say("teams-ui call large window: ok");
    Ok(())
}

/// Runs the child drawing `scene` and screenshots its call window as `call-large-<scene>-<screen>.png`.
fn shot(sandbox: &Sandbox, scene: &str, screen: &str) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, scene)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("call-large-{scene}-{screen}.png");
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

const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const GROUP: &str = "19:0000000000000000000000000000crew@thread.v2";

/// Made-up first and last names the participants are named from.
const FIRST: [&str; 10] = ["Ann", "Ben", "Cleo", "Dan", "Eva", "Finn", "Gus", "Hana", "Ivo", "Jun"];
const LAST: [&str; 5] = ["Able", "Bright", "Sea", "Dee", "Eastwood"];

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The `n`th participant's MRI, from 1.
fn mri(n: usize) -> String {
    format!("8:orgid:00000000-0000-4000-8000-0000000002{n:02}")
}

/// What the scene's name says: its layout and how many others are in the call.
fn parse(scene: &str) -> (&str, usize) {
    let (layout, count) = scene.rsplit_once('-').unwrap_or((scene, "10"));
    (layout, count.parse().unwrap_or(10))
}

/// The group chat and its members.
fn seed(core: &FakeCore, count: usize) {
    locked(&core.conversations).push(Conversation {
        id: GROUP.to_owned(),
        kind: ConversationKind::Group,
        topic: Some("All hands".to_owned()),
        version: 1,
        last_activity: Some(1),
    });
    locked(&core.members).insert(
        GROUP.to_owned(),
        std::iter::once(LAB_A.to_owned())
            .chain((1..=count).map(mri))
            .map(|mri| Member::new(&mri, None))
            .collect(),
    );
}

/// The roster of `count` others: the first nine with their camera on, each on a line of its own, one speaking, a few
/// muted, one with a hand up, the one sharing while `sharing`.
fn roster(count: usize, sharing: bool) -> CallView {
    CallView {
        participants: (1..=count)
            .map(|n| CallParticipant {
                mri: mri(n),
                name: Some(format!(
                    "{} {}",
                    FIRST[n % FIRST.len()],
                    LAST[n / FIRST.len() % LAST.len()]
                )),
                video_on: n % 2 == 1 && n < 19,
                line: (n % 2 == 1 && n < 19).then_some(n / 2 + 1),
                speaking: n == 3,
                muted: n % 3 == 0,
                hand_raised: n == 5,
                organizer: n == 1,
                joined: None,
                sharing: sharing && n == 2,
            })
            .collect(),
        speaking: false,
        hearing: true,
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
        open(&scene, cx);
    });
    Ok(())
}

/// The cameras' pictures on their lines, again and again so they never go stale.
fn film(feed: VideoFeed, cx: &mut App) {
    let pattern = TestPattern::new(320, 180);
    cx.spawn(async move |cx| {
        for frame in 1..u32::MAX {
            let output: &dyn VideoOutput = &feed;
            for line in 0..=teams_core::GROUP_SLOTS {
                let _ = output.remote(line).show(&pattern.frame(frame));
            }
            let _ = output
                .remote(teams_media::session::SHARING_LINE)
                .show(&TestPattern::new(1280, 720).frame(frame));
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
    })
    .detach();
}

/// The call window of `scene`'s call, once it is up as the scene has it.
fn open(scene: &str, cx: &mut App) {
    let (layout, count) = parse(scene);
    let (layout, sharing) = (layout.to_owned(), layout == "share");
    let core = Arc::new(FakeCore::default());
    seed(&core, count);
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let feed = VideoFeed::new();
    let models = Models::with_video_calls(Some(teams_proto::Mri::new(ECHO)), feed.clone(), cx);
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let appearance = Appearance {
        follow_system: false,
        theme: LIGHT_KEY,
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
    let core = parts.core.clone();
    let calls_model = models.calls.clone();
    let send = events.clone();
    film(feed, cx);
    // the registration drained first, then the call asked for and told up
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(300)).await;
        cx.update(|cx| {
            calls::ask(&calls_model, &core, &trace, Ask::Call { chat: GROUP.to_owned() }, cx);
        });
        cx.background_executor().timer(Duration::from_millis(300)).await;
        let call = CallId::for_tests(1);
        let _ = send.send(Event::Call {
            call,
            status: CallStatus::Connected,
        });
        let _ = send.send(Event::CallParticipants {
            call,
            view: roster(count, sharing),
        });
        if sharing {
            let _ = send.send(Event::CallRemoteShare { call, on: true });
        }
        cx.background_executor().timer(Duration::from_millis(500)).await;
        cx.update(|cx| ready(&windows, &layout, cx));
        // the bridge, the windows and the event channel live as long as the app
        mem::forget((bridge, send));
    })
    .detach();
}

/// The call window laid out as `layout` has it, then `ready` once it settled.
fn ready(windows: &gpui::Entity<CallWindows>, layout: &str, cx: &mut App) {
    let Some(window) = windows.read(cx).window() else {
        eprintln!("no call window opened");
        cx.quit();
        return;
    };
    let speaker = layout == "speaker";
    let _ = window.update(cx, move |kit: &mut KitRoot, window, cx| {
        if speaker && let Ok(stage) = kit.view().clone().downcast::<CallStage>() {
            stage.update(cx, |stage, cx| stage.set_layout(Layout::Speaker, cx));
        }
        window.refresh();
        after_frames(window, READY_FRAMES, |_, _| say("ready"));
    });
}
