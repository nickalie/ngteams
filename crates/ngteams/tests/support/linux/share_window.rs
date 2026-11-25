//! Sharing the screen in a call, from the call window's Share, as a user on niri does it (task #139: "When I choose
//! what to share, right away I see Screen Sharing Stopped"), in the UI sandbox, on the mock (`harness = false`: GPUI
//! runs on the process's main thread).
//!
//! The parent runs the mock chat service with its calling service, its Trouter and its media (a loopback TURN server
//! and, for a group call, a mixer behind each party), and two UI sandboxes, each with its private PipeWire. a's
//! screen casts as niri casts one (linear DMA-BUFs only, the modifier mandatory: `ui_sandbox::pipewire::ScreenCast`),
//! and a's bus has the fake screen-cast portal that picks it. In each sandbox this test runs again as lab account a's
//! or b's app (`ngteams::app::launch` over an engine on the mock with its calling, its calls' media on the sandbox's
//! PipeWire). The sandbox's pointer clicks a's toolbar (Share, its "Screen, window, or tab", Stop sharing), found in
//! screenshots as a user's eye finds them (`call_pixels`); its keyboard and the notifications' actions do the rest.
//!
//! - **1:1 call**: a calls b, b answers. a shares: the portal's session started and its remote handed over, b's window
//!   presents a's screen, and for [`HOLD`] the share keeps going (a's screen is read, b presents more of it, the portal
//!   session stays open). a clicks Stop sharing: the portal session is closed, a's screen is no longer read, and b's
//!   window no longer presents or draws it. a shares twice more: new portal sessions, b presents each again; a stops each time.
//! - **Group call**: the mock's c rings a and b in the a–b–c chat, both answer. a shares: a new portal session, the
//!   sharing line described `sendonly` to the service, a's screen read for [`HOLD`], the session open; a stops: the
//!   session closed, the line described `recvonly`; repeated three times. (The mock's mixer doesn't pass a's frames on to b.)
//! - **Renegotiation (1:1)**: four calls a makes to b. a's camera opens slowly here (a USB camera can take seconds),
//!   so a's camera switch holds its renegotiation open meanwhile. a's toolbar shows each switch at once all the same:
//!   the camera on, Stop sharing in Share's place. a turns the camera on and shares at once: the share waits, no
//!   refusal is told, and once the camera's offer settled the share's goes; b presents a's camera, then a's screen. a
//!   stops sharing while the camera's renegotiation is open: the stop waits, then goes, and b presents a's camera and
//!   no longer the screen. a shares and stops while it is open: the later switch decides, nothing is shared. a turns
//!   the camera on, off and on again: b presents a's camera again. a's offers go one at a time.
//! - Screenshots of a's window while it shares and of b's window showing a's screen, under `target/ui-shots/` (and
//!   `$SHARE_EVIDENCE`, if set).
//!
//! Without weston 15, niri, `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, WindowingModes};
use image::{Rgb, RgbImage};
use ngteams::app::{self, Launch};
use ngteams::calling::PersonalCalls;
use ngteams::descriptors::adopt_trace;
use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::web_client_info;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, DeviceReady, Devices, VideoSource};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::{AB_CHAT, ABC_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TENANT};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::settings::Settings;
use teams_ui::sounds::SoundBoard;
use teams_ui::{Ringer, SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys, parse_key};
use ui_sandbox::pipewire::{PipeWireOptions, ScreenCast};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};
#[expect(
    dead_code,
    reason = "the shared screenshot reader exposes the other call-window measurements too"
)]
#[path = "call_pixels.rs"]
mod pixels;
use pixels::Rect;

const CHILD_ENV: &str = "NGTEAMS_SHARE_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_SHARE_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_SHARE_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_SHARE_TROUTER";
const RELAY_ENV: &str = "NGTEAMS_SHARE_RELAY";
const IC3_ENV: &str = "NGTEAMS_SHARE_IC3";
const SKYPE_ENV: &str = "NGTEAMS_SHARE_SKYPE";
const DIR_ENV: &str = "NGTEAMS_SHARE_DIR";
/// Where the screenshots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "SHARE_EVIDENCE";
/// The steps to run, by name (`one-to-one`, `group`, `renegotiation`), comma-separated; all of them when unset.
const STEPS_ENV: &str = "SHARE_STEPS";
/// Set to `dark` for a's Teams Dark screenshots; b keeps the screenshot reader's light palette.
const THEME_ENV: &str = "SHARE_THEME";
/// Set to `dark` for b's Teams Dark screenshots in the `renegotiation` step (the other steps read b's window as light).
const THEME_B_ENV: &str = "SHARE_THEME_B";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
/// While this file is in the sandbox dir, the camera opens only once it is gone (a slow camera).
const HOLD_CAMERA: &str = "hold-camera";
/// The file the child writes in the sandbox dir when its camera was asked for and is held.
const CAMERA_HELD: &str = "camera-held";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(120);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(60);
/// How long a share is held before it is checked still going: well past the moment niri's ended it.
const HOLD: Duration = Duration::from_secs(10);
/// How long the window gets to draw what a click or a key changed.
const SETTLE: Duration = Duration::from_millis(700);
/// The frames of a's screen (15 a second) that must be read while a share is held.
const HELD_FRAMES: u64 = 75;
/// The frames of a's screen b must have presented (traced at the first and every 30th).
const PRESENTED: u64 = 30;
/// The call window's toolbar lies within this many px of its top.
const TOOLBAR_BAND: i32 = 58;
/// niri's layout: each window takes the whole screen, and no hot corner opens the overview under the pointer.
const NIRI_CONFIG: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\ngestures {\n    hot-corners {\n        off\n    }\n}\n";
const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.5,
};
const B_TONE: Tone = Tone {
    hz: 660.0,
    amplitude: 0.5,
};

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("a") => child(Alias::A),
        Ok("b") => child(Alias::B),
        Ok(other) => Err(format!("unknown account {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("share window test: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn sandbox_options(tone: Tone) -> SandboxOptions {
    SandboxOptions {
        niri_extra: NIRI_CONFIG,
        pipewire: Some(PipeWireOptions {
            tone,
            screen: ScreenCast::Niri,
            ..PipeWireOptions::default()
        }),
        ..SandboxOptions::default()
    }
}

fn parent() -> SandboxResult {
    let Some(a_box) = sandbox_or_skip("ngteams share", &sandbox_options(A_TONE))? else {
        return Ok(());
    };
    let Some(b_box) = sandbox_or_skip("ngteams share", &sandbox_options(B_TONE))? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(
        dir.path(),
        Tone {
            hz: 880.0,
            amplitude: 0.5,
        },
    )?;
    let lab = MockLab {
        origin: mock.origin(),
        trouter: trouter.origin().to_owned(),
        relay: relay.to_string(),
        dir: dir.path().to_owned(),
    };
    let mut a = AppUnderTest::start(&a_box, &lab, &mock, Alias::A)?;
    let mut b = AppUnderTest::start(&b_box, &lab, &mock, Alias::B)?;
    // some steps alone (`SHARE_STEPS=group`), all of them when unset
    let only = std::env::var(STEPS_ENV).ok();
    let wanted = |name: &str| {
        only.as_deref()
            .is_none_or(|only| only.split(',').any(|step| step == name))
    };
    let outcome = User::new(&a_box, &b_box, &mock, &runtime).and_then(|mut user| {
        if wanted("one-to-one") {
            say("step one-to-one");
            one_to_one(&mut user, &mut a, &mut b)?;
        }
        if wanted("group") {
            say("step group");
            group_call(&mut user, &mut a, &mut b)?;
        }
        if wanted("renegotiation") {
            say("step renegotiation");
            if let Err(error) = renegotiation(&mut user, &mut a, &mut b) {
                user.shot_a("share-reneg-failed-a.png")?;
                user.shot_b("share-reneg-failed-b.png")?;
                return Err(error);
            }
        }
        Ok(())
    });
    keep_logs(&lab.dir);
    let outcome = outcome.map_err(|error| {
        format!(
            "{error}\n--- a: {}\n--- b: {}\n--- a's sandbox{}\n--- b's sandbox{}",
            a.running.fail(&"killed"),
            b.running.fail(&"killed"),
            a_box.logs(),
            b_box.logs()
        )
    });
    outcome?;
    a.quit(&a_box)?;
    b.quit(&b_box)?;
    drop(trouter);
    say("ngteams share window: ok");
    Ok(())
}

/// a calls b; b answers; a shares, holds, stops, shares again and stops; a hangs up.
fn one_to_one(user: &mut User<'_>, a: &mut AppUnderTest, b: &mut AppUnderTest) -> SandboxResult {
    a.trace.wait_for("call-button", "shown")?;
    user.a_keys("ctrl+shift+c")?;
    a.in_call(&["chat", "ringing"])?;
    b.trace.wait_for("call-notification", "shown")?;
    let ringing = call_notification(user.b_box, 0)?;
    user.b_box.bus().invoke_action(ringing, "answer")?;
    b.in_call(&["incoming", "connected"])?;
    a.in_call(&["chat", "connected"])?;
    user.find_call_window()?;

    for round in 1..=3 {
        let presented_before = b.trace.presented_share();
        user.share(a, round)?;
        let first = b.trace.next_where("b: a's screen presented", |line| {
            is_share_presented(line) && frames_of(line) >= presented_before + PRESENTED
        });
        let first = match first {
            Ok(first) => first,
            Err(error) => {
                user.shot_a(&format!("share-1to1-{round}-unseen-a.png"))?;
                user.shot_b(&format!("share-1to1-{round}-unseen-b.png"))?;
                return Err(format!("1:1 share {round}: {error}").into());
            }
        };
        say(&format!(
            "1:1 share {round}: b presented a's screen ({})",
            first["frames"]
        ));
        user.shot_a(&format!("share-1to1-{round}-sharing.png"))?;
        user.shot_b(&format!("share-1to1-{round}-far-end.png"))?;
        user.holds(&format!("1:1 share {round}"), round)?;
        let presented = b.trace.presented_share();
        say(&format!(
            "1:1 share {round}: b presented {presented} of a's frames by the hold's end"
        ));
        if presented < frames_of(&first) + PRESENTED {
            return Err(format!(
                "1:1 share {round}: b presented only {presented} of a's frames by the hold's end (from {})",
                first["frames"]
            )
            .into());
        }
        if !user.b_shows_a_screen()? {
            return Err(format!("1:1 share {round}: b's window shows no shared screen").into());
        }
        user.stop_sharing(a, round)?;
        // b's stage drops the share as its renegotiation comes: no more of it presented, nothing of it drawn
        thread::sleep(Duration::from_secs(2));
        let stopped_at = b.trace.presented_share();
        thread::sleep(Duration::from_secs(6));
        if b.trace.presented_share() > stopped_at {
            return Err(format!("1:1 share {round}: b still presents a's screen after a stopped").into());
        }
        if round == 1 {
            user.shot_a("share-1to1-stopped.png")?;
            user.shot_b("share-1to1-far-end-stopped.png")?;
        }
        let gone = wait_until(WAIT / 2, || (!user.b_shows_a_screen().unwrap_or(true)).then_some(()));
        if gone.is_none() {
            user.shot_b(&format!("share-1to1-far-end-stopped-{round}-still.png"))?;
            return Err(format!("1:1 share {round}: b's window still shows a's screen after a stopped").into());
        }
    }

    hang_up(&mut user.a_input, a)?;
    b.in_call(&["ended", "remote"])?;
    Ok(())
}

/// The mock's c rings a and b in the a–b–c chat; both answer; a shares, holds and stops; both leave.
fn group_call(user: &mut User<'_>, a: &mut AppUnderTest, b: &mut AppUnderTest) -> SandboxResult {
    thread::sleep(Duration::from_secs(3));
    let seen = (ringing_seen(user.a_box), ringing_seen(user.b_box));
    let _call = user.mock.ring_group(LAB_C, ABC_CHAT, &[LAB_A, LAB_B]);
    let a_ringing = call_notification(user.a_box, seen.0)?;
    let b_ringing = call_notification(user.b_box, seen.1)?;
    user.a_box.bus().invoke_action(a_ringing, "answer")?;
    user.b_box.bus().invoke_action(b_ringing, "answer")?;
    a.in_call(&["incoming", "connected"])?;
    b.in_call(&["incoming", "connected"])?;
    user.find_call_window()?;

    // the portal's sessions so far: the 1:1 call's, when it ran first
    for share in 1..=3 {
        let round = user.calls("ScreenCast.OpenPipeWireRemote") + 1;
        let described = user.descriptions().len();
        user.share(a, round)?;
        user.described(described, "sendonly")?;
        user.holds(&format!("group share {share}"), round)?;
        user.shot_a(&format!("share-group-{share}-sharing.png"))?;
        user.shot_b(&format!("share-group-{share}-other.png"))?;
        let described = user.descriptions().len();
        user.stop_sharing(a, round)?;
        user.described(described, "recvonly")?;
    }

    hang_up(&mut user.a_input, a)?;
    hang_up(&mut user.b_input, b)?;
    Ok(())
}

/// Four 1:1 calls a makes to b. a's camera opens slowly, so when a turns it on, its renegotiation stays open until
/// the test lets the camera open: a shares, stops sharing, or shares and stops in that time. Then a turns the camera
/// on, off and on again. Each case has a call of its own.
fn renegotiation(user: &mut User<'_>, a: &mut AppUnderTest, b: &mut AppUnderTest) -> SandboxResult {
    // 1. the camera and Share at once: the share waits for the camera's renegotiation, then goes after it
    let refused = call_up(user, a, b)?;
    let round = user.calls("ScreenCast.OpenPipeWireRemote") + 1;
    user.camera_on_held(a)?;
    user.share(a, round)?;
    user.waits(a, b, refused, (0, true), "share while the camera turns on")?;
    user.share_button(true)?;
    user.shot_a("share-reneg-1-waits.png")?;
    user.release_camera()?;
    b.trace
        .next_where("b: a's camera presented", |line| is_presented(line, "remote"))?;
    b.presents_share("camera and share")?;
    user.holds("camera and share", round)?;
    user.settled(2)?;
    a.logged("sharing its screen true once its open renegotiation settled")?;
    user.shot_a("share-reneg-1-sharing.png")?;
    user.shot_b("share-reneg-1-far-end.png")?;
    say("renegotiation 1: the camera's offer, then the share's; b presents both");
    user.stop_sharing(a, round)?;
    user.settled(3)?;
    leave(user, a, b, refused)?;

    // 2. Stop sharing while the camera's renegotiation is open: the stop waits, then goes
    let refused = call_up(user, a, b)?;
    let round = round + 1;
    user.share(a, round)?;
    user.settled(1)?;
    b.presents_share("share before the camera")?;
    user.camera_on_held(a)?;
    user.click_stop_sharing(a, round)?;
    let read = user.screen_frames()?;
    user.waits(a, b, refused, (1, false), "stop sharing while the camera turns on")?;
    user.share_button(false)?;
    if user.calls("ScreenCast.Close") >= round {
        return Err("stop sharing while the camera turns on: the portal session closed before the stop went".into());
    }
    if user.screen_frames()? <= read {
        return Err("stop sharing while the camera turns on: a's screen is no longer read while the stop waits".into());
    }
    user.shot_a("share-reneg-2-stop-waits.png")?;
    user.release_camera()?;
    user.stopped(round, "stop sharing while the camera turns on")?;
    b.no_more_share("stop sharing while the camera turns on")?;
    user.settled(3)?;
    a.logged("sharing its screen false once its open renegotiation settled")?;
    user.shot_a("share-reneg-2-stopped.png")?;
    user.shot_b("share-reneg-2-far-end-stopped.png")?;
    say("renegotiation 2: the stop went after the camera's offer; b no longer presents a's screen");
    leave(user, a, b, refused)?;

    // 3. Share, then Stop sharing while the camera's renegotiation is open: the later switch decides
    let refused = call_up(user, a, b)?;
    let round = round + 1;
    user.camera_on_held(a)?;
    user.share(a, round)?;
    user.click_stop_sharing(a, round)?;
    user.waits(a, b, refused, (0, true), "share and stop while the camera turns on")?;
    user.share_button(false)?;
    user.shot_a("share-reneg-3-taken-back.png")?;
    user.release_camera()?;
    user.stopped(round, "share and stop while the camera turns on")?;
    user.settled(1)?;
    let mark = b.trace.mark();
    thread::sleep(Duration::from_secs(3));
    if b.trace.since(mark, is_share_presented) || user.offers()?.0 != 1 {
        return Err("share and stop while the camera turns on: a's screen went out".into());
    }
    say("renegotiation 3: share and stop while the camera turned on; nothing was shared");
    leave(user, a, b, refused)?;

    // 4. the camera on, off and on again: b presents a's camera again
    let refused = call_up(user, a, b)?;
    for (turn, on) in [true, false, true].into_iter().enumerate() {
        // what b presented before this switch is not looked at
        b.trace.mark();
        user.a_keys("ctrl+shift+o")?;
        user.settled(turn + 1)?;
        if on {
            b.presents_camera(&format!("camera on {}", turn / 2 + 1))?;
        } else {
            b.no_camera("camera off")?;
        }
    }
    user.shot_a("share-reneg-4-camera-again.png")?;
    user.shot_b("share-reneg-4-far-end-camera-again.png")?;
    say("renegotiation 4: the camera on, off and on again; b presents a's camera again");
    leave(user, a, b, refused)
}

/// a calls b in their chat, b answers; the refusals each app told so far.
fn call_up(user: &mut User<'_>, a: &mut AppUnderTest, b: &mut AppUnderTest) -> SandboxResult<(usize, usize)> {
    thread::sleep(Duration::from_secs(3));
    let seen = ringing_seen(user.b_box);
    user.a_keys("ctrl+shift+c")?;
    a.in_call(&["chat", "ringing"])?;
    let ringing = call_notification(user.b_box, seen)?;
    user.b_box.bus().invoke_action(ringing, "answer")?;
    b.in_call(&["incoming", "connected"])?;
    a.in_call(&["chat", "connected"])?;
    user.find_call_window()?;
    Ok((a.trace.refused(), b.trace.refused()))
}

/// a hangs up; neither app told a refusal since `refused`.
fn leave(user: &mut User<'_>, a: &mut AppUnderTest, b: &mut AppUnderTest, refused: (usize, usize)) -> SandboxResult {
    if (a.trace.refused(), b.trace.refused()) != refused {
        return Err(format!(
            "a call action was refused: a {:?}, b {:?}",
            a.trace.refusals(),
            b.trace.refusals()
        )
        .into());
    }
    hang_up(&mut user.a_input, a)?;
    b.in_call(&["ended", "remote"])?;
    Ok(())
}

/// The user at the two sandboxes: a's pointer and keyboard and b's keyboard, a's call window, and the mock.
struct User<'a> {
    a_box: &'a Sandbox,
    b_box: &'a Sandbox,
    mock: &'a ChatServiceMock,
    runtime: &'a tokio::runtime::Runtime,
    a_input: VirtualInput,
    b_input: VirtualInput,
    /// a's call window on a's screen, as last found.
    frame: Option<Rect>,
    shots: usize,
}

impl<'a> User<'a> {
    fn new(
        a_box: &'a Sandbox,
        b_box: &'a Sandbox,
        mock: &'a ChatServiceMock,
        runtime: &'a tokio::runtime::Runtime,
    ) -> SandboxResult<Self> {
        Ok(Self {
            a_box,
            b_box,
            mock,
            runtime,
            a_input: a_box.input()?,
            b_input: b_box.input()?,
            frame: None,
            shots: 0,
        })
    }

    fn a_keys(&mut self, spec: &str) -> SandboxResult {
        key(&mut self.a_input, spec)
    }

    /// a's screen as it is now.
    fn screen(&mut self) -> SandboxResult<RgbImage> {
        self.shots += 1;
        let name = format!("screen-{}.png", self.shots);
        let png = self.a_box.screenshot(&name, false)?;
        let image = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        Ok(image)
    }

    /// Finds a's call window (it has the keyboard once it opens): where niri's shot of it alone matches the screen's.
    fn find_call_window(&mut self) -> SandboxResult {
        thread::sleep(SETTLE * 2);
        let deadline = Instant::now() + WAIT;
        loop {
            self.shots += 1;
            let name = format!("window-{}.png", self.shots);
            let png = self.a_box.screenshot(&name, true)?;
            let alone = image::open(&png)?.to_rgb8();
            let _ = fs::remove_file(png);
            let screen = self.screen()?;
            if let Some(frame) = pixels::find(&screen, &alone) {
                self.frame = Some(frame);
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("a's call window isn't on the screen".into());
            }
            thread::sleep(SETTLE);
        }
    }

    fn window(&self) -> SandboxResult<Rect> {
        self.frame.ok_or_else(|| "a's call window wasn't found".into())
    }

    /// The pointer rests on the window's frame, off every control.
    fn park(&mut self) -> SandboxResult {
        let window = self.window()?;
        self.a_input
            .move_to(u32_of(window.x + 2), u32_of(window.bottom() - 2))?;
        thread::sleep(Duration::from_millis(150));
        Ok(())
    }

    /// A left click in the middle of `target`, then the pointer parked.
    fn click(&mut self, target: Rect) -> SandboxResult {
        let (x, y) = target.center();
        self.a_input.move_to(u32_of(x), u32_of(y))?;
        thread::sleep(Duration::from_millis(60));
        self.a_input.click(u32_of(x), u32_of(y), keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(60));
        self.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The toolbar's band of a's call window, under the title's two lines, off the window's edge.
    fn band(&self) -> SandboxResult<Rect> {
        let window = self.window()?;
        Ok(Rect {
            x: window.x + 4,
            y: window.y + 6,
            w: window.w - 8,
            h: TOOLBAR_BAND - 12,
        })
    }

    /// Share, and while a shares, Stop sharing: the button right before Leave (Teams' red, the toolbar's largest).
    fn share_button(&mut self, sharing: bool) -> SandboxResult<Rect> {
        self.park()?;
        let band = self.band()?;
        let shot = self.screen()?;
        let reds = red_blobs(&shot, band);
        let leave = reds
            .iter()
            .max_by_key(|blob| blob.area())
            .copied()
            .ok_or("no Leave in a's toolbar")?;
        if sharing {
            // Stop sharing's red mark, left of Leave
            return reds
                .iter()
                .filter(|blob| blob.right() <= leave.x && blob.area() < leave.area())
                .max_by_key(|blob| blob.x)
                .copied()
                .ok_or_else(|| format!("no Stop sharing left of Leave {leave:?}: {reds:?}").into());
        }
        let groups = pixels::white_groups(&shot, band);
        groups
            .iter()
            .filter(|group| group.right() <= leave.x)
            .max_by_key(|group| group.x)
            .copied()
            .ok_or_else(|| format!("no Share left of Leave {leave:?}: {groups:?}").into())
    }

    /// a clicks Share, then its "Screen, window, or tab"; the portal picks a's screen (session `round`).
    fn share(&mut self, a: &mut AppUnderTest, round: usize) -> SandboxResult {
        let asked = a.trace.asked("share");
        if round == 2 {
            self.a_keys("ctrl+shift+e")?;
        } else {
            let share = self.share_button(false)?;
            self.click(share)?;
        }
        if round <= 2 {
            self.shot_a(&format!("share-menu-{round}.png"))?;
        }
        self.a_keys("down")?;
        self.a_keys("enter")?;
        let what = format!("share {round}");
        a.trace.next_where(&format!("{what}: Share asked"), |line| {
            line["view"] == "call"
                && line["change"] == "asked"
                && strings(line, "kinds").iter().any(|kind| kind == "share")
        })?;
        if a.trace.asked("share") <= asked {
            return Err(format!("{what}: Share wasn't asked").into());
        }
        wait_until(WAIT, || {
            (self.calls("ScreenCast.OpenPipeWireRemote") >= round).then_some(())
        })
        .ok_or_else(|| format!("{what}: the portal handed over no remote: {:?}", self.portal()))?;
        if self.calls("ScreenCast.Close") >= round {
            return Err(format!("{what}: the portal session closed at once: {:?}", self.portal()).into());
        }
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The share goes on for [`HOLD`]: a's screen read all along, the portal's session `round` never closed.
    fn holds(&mut self, what: &str, round: usize) -> SandboxResult {
        let before = self.screen_frames()?;
        thread::sleep(HOLD);
        let read = self.screen_frames()? - before;
        say(&format!("{what}: {read} frames of a's screen read in {HOLD:?}"));
        if read < HELD_FRAMES {
            return Err(format!("{what}: only {read} frames of a's screen read in {HOLD:?}").into());
        }
        if self.calls("ScreenCast.Close") >= round {
            return Err(format!("{what}: the portal session closed while sharing: {:?}", self.portal()).into());
        }
        // Share's place shows Stop sharing
        self.share_button(true)?;
        Ok(())
    }

    /// a clicks Stop sharing: the portal's session `round` closed, a's screen no longer read, Share back.
    fn stop_sharing(&mut self, a: &mut AppUnderTest, round: usize) -> SandboxResult {
        self.click_stop_sharing(a, round)?;
        self.stopped(round, &format!("stop sharing {round}"))
    }

    /// a clicks Stop sharing, and the app asks for it.
    fn click_stop_sharing(&mut self, a: &mut AppUnderTest, round: usize) -> SandboxResult {
        let stop = self.share_button(true)?;
        let asked = a.trace.asked("share");
        self.click(stop)?;
        let what = format!("stop sharing {round}");
        a.trace.next_where(&format!("{what}: asked"), |line| {
            line["view"] == "call"
                && line["change"] == "asked"
                && strings(line, "kinds").iter().any(|kind| kind == "share")
        })?;
        if a.trace.asked("share") <= asked {
            return Err(format!("{what}: Stop sharing wasn't asked").into());
        }
        Ok(())
    }

    /// The share stopped: the portal's session `round` closed, a's screen no longer read, Share back.
    fn stopped(&mut self, round: usize, what: &str) -> SandboxResult {
        wait_until(WAIT, || (self.calls("ScreenCast.Close") >= round).then_some(()))
            .ok_or_else(|| format!("{what}: the portal session wasn't closed: {:?}", self.portal()))?;
        thread::sleep(Duration::from_secs(1));
        let before = self.screen_frames()?;
        thread::sleep(Duration::from_secs(2));
        let read = self.screen_frames()? - before;
        if read > 2 {
            return Err(format!("{what}: a's screen still read ({read} frames in 2 s)").into());
        }
        self.share_button(false)?;
        Ok(())
    }

    /// a turns its camera on, and the camera holds until [`Self::release_camera`]: the camera's renegotiation open.
    fn camera_on_held(&mut self, a: &mut AppUnderTest) -> SandboxResult {
        let _ = fs::remove_file(self.a_box.path(CAMERA_HELD));
        fs::write(self.a_box.path(HOLD_CAMERA), "")?;
        let asked = a.trace.asked("camera");
        let mark = a.trace.mark();
        self.a_keys("ctrl+shift+o")?;
        wait_until(WAIT, || self.a_box.path(CAMERA_HELD).exists().then_some(()))
            .ok_or("a's camera wasn't asked for")?;
        if a.trace.asked("camera") <= asked {
            return Err("a's camera wasn't asked".into());
        }
        // shown on at once, while it opens
        let camera_on = |line: &Value| {
            line["view"] == "in-call"
                && line["change"] == "shown"
                && strings(line, "kinds").iter().any(|kind| kind == "camera-on")
        };
        if !a.trace.since(mark, camera_on) {
            a.trace.next_where("a's camera shown on while it opens", camera_on)?;
        }
        Ok(())
    }

    /// a's held camera opens.
    fn release_camera(&self) -> SandboxResult {
        fs::remove_file(self.a_box.path(HOLD_CAMERA))?;
        Ok(())
    }

    /// While a's camera holds, `what` waits: a sent no offer past its `offers` before, neither app told a refusal, and
    /// for a share `starting`, b presents none of it.
    fn waits(
        &mut self,
        a: &mut AppUnderTest,
        b: &mut AppUnderTest,
        refused: (usize, usize),
        (offers, starting): (usize, bool),
        what: &str,
    ) -> SandboxResult {
        let mark = b.trace.mark();
        thread::sleep(Duration::from_secs(3));
        let (sent, _) = self.offers()?;
        if sent != offers {
            return Err(format!(
                "{what}: a sent {sent} offers while its camera's renegotiation was open (before: {offers})"
            )
            .into());
        }
        if (a.trace.refused(), b.trace.refused()) != refused {
            return Err(format!(
                "{what}: a call action was refused: a {:?}, b {:?}",
                a.trace.refusals(),
                b.trace.refusals()
            )
            .into());
        }
        if starting && b.trace.since(mark, is_share_presented) {
            return Err(format!("{what}: b presents a's screen before the share went").into());
        }
        say(&format!("{what}: waits, no refusal"));
        Ok(())
    }

    /// a's offers (`renegotiate`) on the newest call, and whether one waits for its answer (`acknowledge`): an error
    /// when one went while another waited.
    fn offers(&self) -> SandboxResult<(usize, bool)> {
        let calls = self.mock.calls();
        let call = calls.last().ok_or("the mock has no call")?;
        let (mut sent, mut open) = (0, false);
        for (op, account) in &call.requests {
            if account != LAB_A {
                continue;
            }
            match op.as_str() {
                "renegotiate" if open => {
                    return Err(format!("a sent two offers at once: {:?}", call.requests).into());
                }
                "renegotiate" => {
                    open = true;
                    sent += 1;
                }
                "acknowledge" => open = false,
                _ => {}
            }
        }
        Ok((sent, open))
    }

    /// Waits until a sent `offers` offers, each answered, one at a time.
    fn settled(&self, offers: usize) -> SandboxResult {
        let deadline = Instant::now() + WAIT;
        loop {
            let (sent, open) = self.offers()?;
            if sent == offers && !open {
                return Ok(());
            }
            if sent > offers || Instant::now() > deadline {
                return Err(format!("a sent {sent} offers (open: {open}); {offers} were expected").into());
            }
            thread::sleep(Duration::from_millis(200));
        }
    }

    fn screen_frames(&self) -> SandboxResult<u64> {
        let pipewire = self.a_box.pipewire().ok_or("no PipeWire in a's sandbox")?;
        Ok(pipewire.screen().frames.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn portal(&self) -> Vec<(&'static str, &'static str)> {
        self.a_box
            .bus()
            .portal_calls()
            .iter()
            .filter(|call| call.method.starts_with("ScreenCast."))
            .map(|call| (call.method, call.answer))
            .collect()
    }

    fn calls(&self, method: &str) -> usize {
        self.portal().iter().filter(|(asked, _)| *asked == method).count()
    }

    /// The media descriptions every `updateMediaDescriptions` the mock got carries, in order.
    fn descriptions(&self) -> Vec<Value> {
        let requests = self.runtime.block_on(self.mock.received_requests());
        requests
            .into_iter()
            .filter(|request| request.url.path().ends_with("/updateMediaDescriptions"))
            .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
            .map(|body| body["UpdateMediaDescriptions"]["mediaDescriptions"].clone())
            .collect()
    }

    /// Waits until an `updateMediaDescriptions` after the first `after` describes the sharing line `direction`.
    fn described(&self, after: usize, direction: &str) -> SandboxResult {
        let found = wait_until(WAIT, || {
            let descriptions = self.descriptions();
            descriptions
                .iter()
                .skip(after)
                .any(|sent| {
                    sent["descriptions"]
                        .as_array()
                        .and_then(|lines| lines.last())
                        .is_some_and(|line| line["direction"] == direction)
                })
                .then_some(())
        });
        found.ok_or_else(|| {
            format!(
                "the sharing line wasn't described {direction}: {:?}",
                self.descriptions()
            )
            .into()
        })
    }

    /// Whether b's window shows a's screen, as a user sees it: the pattern's dark greys fill a fifth of the window
    /// below its toolbar (an avatar's letters and icons are far fewer pixels).
    fn b_shows_a_screen(&mut self) -> SandboxResult<bool> {
        self.shots += 1;
        let name = format!("b-window-{}.png", self.shots);
        let png = self.b_box.screenshot(&name, true)?;
        let image = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        let (w, h) = (i32::try_from(image.width())?, i32::try_from(image.height())?);
        let stage = Rect {
            x: 0,
            y: TOOLBAR_BAND,
            w,
            h: h - TOOLBAR_BAND,
        };
        let dark: i64 = pixels::blobs(stage, |x, y| {
            let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
                return false;
            };
            image
                .get_pixel_checked(x, y)
                .is_some_and(|&Rgb([r, g, b])| u32::from(r) + u32::from(g) + u32::from(b) < 270)
        })
        .iter()
        .map(|blob| blob.pixels)
        .sum();
        Ok(dark * 5 >= stage.area())
    }

    fn shot_a(&mut self, name: &str) -> SandboxResult {
        self.park()?;
        keep_shot(self.a_box, name)
    }

    fn shot_b(&self, name: &str) -> SandboxResult {
        keep_shot(self.b_box, name)
    }
}

/// Screenshots the focused window of `sandbox` into `name` under `target/ui-shots/` (and the evidence dir).
fn keep_shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    thread::sleep(Duration::from_millis(500));
    let png = sandbox.screenshot(name, true)?;
    fs::copy(&png, dir.join(name))?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV).map(PathBuf::from) {
        fs::create_dir_all(&evidence)?;
        fs::copy(&png, evidence.join(name))?;
    }
    Ok(())
}

/// Copies a's and b's app logs (info: no payloads) into the evidence dir, if it is set.
fn keep_logs(dir: &Path) {
    let Some(evidence) = std::env::var_os(EVIDENCE_ENV).map(PathBuf::from) else {
        return;
    };
    for alias in [Alias::A, Alias::B] {
        let Ok(files) = fs::read_dir(dir.join(alias.to_string()).join("log")) else {
            continue;
        };
        for file in files.filter_map(Result::ok) {
            let name = format!("{alias}-{}", file.file_name().to_string_lossy());
            let _ = fs::create_dir_all(&evidence);
            let _ = fs::copy(file.path(), evidence.join(name));
        }
    }
}

/// The blobs of Teams' red in `within` (Leave, Stop sharing's mark).
fn red_blobs(image: &RgbImage, within: Rect) -> Vec<Rect> {
    pixels::blobs(within, |x, y| {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return false;
        };
        image
            .get_pixel_checked(x, y)
            .is_some_and(|&Rgb([r, g, b])| r > 170 && g < 90 && b < 110)
    })
    .into_iter()
    .filter(|blob| blob.pixels >= 40)
    .map(|blob| blob.bounds)
    .collect()
}

fn key(input: &mut VirtualInput, spec: &str) -> SandboxResult {
    let (modifiers, code) = parse_key(spec, input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
    input.key_with(&modifiers, code)
}

fn hang_up(input: &mut VirtualInput, app: &mut AppUnderTest) -> SandboxResult {
    let before = app.trace.asked("hang-up");
    key(input, "ctrl+shift+h")?;
    app.in_call(&["ended", "hung-up"])?;
    thread::sleep(Duration::from_secs(1));
    let asked = app.trace.asked("hang-up") - before;
    if asked != 1 {
        return Err(format!("one hang-up key press produced {asked} hang-up actions").into());
    }
    say("hang-up: one key press produced one action after the call window closed");
    Ok(())
}

fn u32_of(value: i32) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(0)
}

/// The id of the notification of an incoming call on `sandbox`'s bus that wasn't there before (`seen` of them).
fn call_notification(sandbox: &Sandbox, seen: usize) -> SandboxResult<u32> {
    wait_until(WAIT, || {
        sandbox
            .bus()
            .notifications()
            .into_iter()
            .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
            .nth(seen)
    })
    .map(|notification| notification.id)
    .ok_or_else(|| format!("no incoming call's notification: {:?}", sandbox.bus().notifications()).into())
}

/// How many notifications of an incoming call `sandbox`'s bus has had.
fn ringing_seen(sandbox: &Sandbox) -> usize {
    sandbox
        .bus()
        .notifications()
        .iter()
        .filter(|notification| notification.actions.iter().any(|action| action == "answer"))
        .count()
}

fn is_share_presented(line: &Value) -> bool {
    is_presented(line, "share")
}

fn is_presented(line: &Value, tile: &str) -> bool {
    line["view"] == "video-tile" && line["change"] == "presented" && strings(line, "kinds") == [tile]
}

fn frames_of(line: &Value) -> u64 {
    line["frames"].as_u64().unwrap_or(0)
}

fn strings(line: &Value, key: &str) -> Vec<String> {
    line[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Where the children reach the mock.
struct MockLab {
    origin: String,
    trouter: String,
    relay: String,
    dir: PathBuf,
}

/// A child app in its sandbox: its process, its trace.
struct AppUnderTest {
    alias: Alias,
    running: Running,
    trace: TraceReader,
    /// The app's log (info: no payloads).
    log: PathBuf,
}

impl AppUnderTest {
    fn start(sandbox: &Sandbox, lab: &MockLab, mock: &ChatServiceMock, alias: Alias) -> SandboxResult<Self> {
        let mri = mri_of(alias);
        let dir = lab.dir.join(alias.to_string());
        fs::create_dir_all(&dir)?;
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let ic3 = mock.token(mri).ok_or("the mock's token")?;
        let skype = mock.skypetoken(mri).ok_or("the mock's skypetoken")?;
        let account = alias.to_string();
        let own = dir.to_string_lossy().into_owned();
        let theme = std::env::var(THEME_ENV).unwrap_or_default();
        let theme_b = std::env::var(THEME_B_ENV).unwrap_or_default();
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, account.as_str()),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &lab.origin),
                (TROUTER_ENV, &lab.trouter),
                (RELAY_ENV, &lab.relay),
                (IC3_ENV, &ic3),
                (SKYPE_ENV, &skype),
                (DIR_ENV, &own),
                (THEME_ENV, &theme),
                (THEME_B_ENV, &theme_b),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let running = Running::new(child)?;
        let mut app = Self {
            alias,
            running,
            trace: TraceReader::start(reader),
            log: dir.join("log"),
        };
        app.running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| app.running.fail(&format!("{alias}: no ready line: {error:?}")))?;
        app.trace.wait_for("status", "connected")?;
        app.trace.wait_for("calling", "registered")?;
        Ok(app)
    }

    /// Waits for an `in-call shown` line whose kinds hold every one of `kinds`.
    fn in_call(&mut self, kinds: &[&str]) -> SandboxResult<Value> {
        let what = format!("{}: in-call shown {kinds:?}", self.alias);
        self.trace.next_where(&what, |line| {
            line["view"] == "in-call"
                && line["change"] == "shown"
                && kinds
                    .iter()
                    .all(|kind| strings(line, "kinds").iter().any(|shown| shown == kind))
        })
    }

    /// Waits until the app's log has a line with `text`.
    fn logged(&self, text: &str) -> SandboxResult {
        wait_until(WAIT, || {
            let lines = fs::read_dir(&self.log).ok()?;
            lines
                .filter_map(Result::ok)
                .any(|file| fs::read_to_string(file.path()).is_ok_and(|log| log.contains(text)))
                .then_some(())
        })
        .ok_or_else(|| format!("{}: no log line {text:?} in {}", self.alias, self.log.display()).into())
    }

    /// The far end's window presents a's screen anew: a first frame of it, then [`PRESENTED`] more.
    fn presents_share(&mut self, what: &str) -> SandboxResult {
        let first = self
            .trace
            .next_where(&format!("{what}: a's screen presented"), is_share_presented)?;
        let more = frames_of(&first) + PRESENTED;
        self.trace
            .next_where(&format!("{what}: {more} frames of a's screen presented"), |line| {
                is_share_presented(line) && frames_of(line) >= more
            })?;
        Ok(())
    }

    /// The far end's window presents no more of a's screen (no new frame of it), and presents a's camera again.
    fn no_more_share(&mut self, what: &str) -> SandboxResult {
        let mark = self.trace.mark();
        thread::sleep(Duration::from_secs(2));
        let stopped_at = self.trace.presented_share();
        thread::sleep(Duration::from_secs(4));
        if self.trace.presented_share() > stopped_at {
            return Err(format!("{what}: {} still presents a's screen", self.alias).into());
        }
        if !self.trace.since(mark, |line| is_presented(line, "remote")) {
            self.trace
                .next_where(&format!("{what}: a's camera presented"), |line| {
                    is_presented(line, "remote")
                })?;
        }
        Ok(())
    }

    /// The far end's window presents a's camera anew: a frame of it after now.
    fn presents_camera(&mut self, what: &str) -> SandboxResult {
        self.trace
            .next_where(&format!("{what}: a's camera presented"), |line| {
                is_presented(line, "remote")
            })?;
        Ok(())
    }

    /// The far end's window presents no more of a's camera.
    fn no_camera(&mut self, what: &str) -> SandboxResult {
        thread::sleep(Duration::from_secs(2));
        let mark = self.trace.mark();
        thread::sleep(Duration::from_secs(3));
        if self.trace.since(mark, |line| is_presented(line, "remote")) {
            return Err(format!("{what}: {} still presents a's camera", self.alias).into());
        }
        Ok(())
    }

    /// Asks the app to quit and waits for it to exit cleanly.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        fs::write(sandbox.path(QUIT_TRIGGER), "")?;
        self.running
            .line_where(WAIT, |line| line == "quit")
            .map_err(|error| self.running.fail(&format!("{}: didn't quit: {error:?}", self.alias)))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("{}: exited {status}: {stderr}", self.alias).into());
        }
        Ok(())
    }
}

fn mri_of(alias: Alias) -> &'static str {
    match alias {
        Alias::A => LAB_A,
        Alias::B => LAB_B,
        Alias::C => LAB_C,
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

    fn catch_up(&mut self) -> &[Value] {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        &self.seen
    }

    /// The `call asked` lines naming `ask` so far.
    fn asked(&mut self, ask: &str) -> usize {
        self.catch_up()
            .iter()
            .filter(|line| {
                line["view"] == "call"
                    && line["change"] == "asked"
                    && strings(line, "kinds").iter().any(|kind| kind == ask)
            })
            .count()
    }

    /// The most frames of a shared screen the trace has told presented so far.
    fn presented_share(&mut self) -> u64 {
        self.catch_up()
            .iter()
            .filter(|line| is_share_presented(line))
            .map(frames_of)
            .max()
            .unwrap_or(0)
    }

    /// Where the trace is now, for [`Self::since`].
    fn mark(&mut self) -> usize {
        self.catch_up().len()
    }

    /// Whether a line after `mark` is `found`.
    fn since(&mut self, mark: usize, found: impl Fn(&Value) -> bool) -> bool {
        self.catch_up().iter().skip(mark).any(found)
    }

    /// The `call refused` lines so far.
    fn refused(&mut self) -> usize {
        self.refusals().len()
    }

    fn refusals(&mut self) -> Vec<Value> {
        self.catch_up()
            .iter()
            .filter(|line| line["view"] == "call" && line["change"] == "refused")
            .cloned()
            .collect()
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult<Value> {
        if let Some(line) = self
            .catch_up()
            .iter()
            .find(|line| line["view"] == view && line["change"] == change)
        {
            return Ok(line.clone());
        }
        self.next_where(&format!("{view} {change}"), |line| {
            line["view"] == view && line["change"] == change
        })
    }

    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; seen {:?}", self.seen))?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

// --- the children ---

/// One account's mock tokens: its `ic3` token and its skypetoken; they never run out in a test.
struct Tokens {
    ic3: String,
    skypetoken: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.ic3.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skypetoken.clone()))))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.ic3.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

fn env(name: &str) -> SandboxResult<String> {
    Ok(std::env::var(name).map_err(|_| format!("{name} isn't set"))?)
}

/// The account's engine on the mock, with its calls' media on `devices`, relayed through the mock TURN server.
fn engine(
    alias: Alias,
    devices: Arc<dyn Devices + Send + Sync>,
    feed: VideoFeed,
    dir: &Path,
) -> SandboxResult<EngineHandle> {
    let mri = mri_of(alias);
    let tokens = Arc::new(Tokens {
        ic3: env(IC3_ENV)?,
        skypetoken: env(SKYPE_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let tracker = Arc::new(teams_call::Tracker::new(Mri::new(mri)));
    let origin = env(ORIGIN_ENV)?;
    let trouter_origin = env(TROUTER_ENV)?;
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
    let options = Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(200),
        backoff_jitter_percent: 0,
        ..Options::default()
    };
    let trouter = || -> SandboxResult<_> {
        Ok(
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), &trouter_origin)?
                .with_options(options),
        )
    };
    let calling = trouter()?.for_calls(CallStack::new(Arc::clone(&tracker)));
    let name = display_name(&format!("bot-{alias}@lab.example.test")).to_owned();
    let identity = Identity {
        mri: Mri::new(mri),
        display_name: Some(name.clone()),
        endpoint_id: String::new(),
        language_id: Some("en-us".into()),
        tenant_id: TENANT.into(),
        client: web_client_info(),
        region: Some("amer".into()),
        partition: Some("amer01".into()),
    };
    let media = CallMedia {
        devices,
        video: Some(teams_core::account::call_video(Arc::new(feed))),
        relay: Relay::Fixed(vec![RelayServer::udp(env(RELAY_ENV)?.parse()?)]),
        session: SessionConfig {
            processing: ProcessingConfig::OFF,
            ..Default::default()
        },
    };
    let push = Push::new(trouter()?);
    let handle = engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(Client::new(http()?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(mri), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?;
    Ok(handle)
}

/// The app's log (info: no payloads) in `dir`.
fn app_log(dir: &Path) -> SandboxResult {
    teams_log::install(&teams_log::Config {
        dir: dir.join("log"),
        setting: teams_log::Level::Info,
        env: None,
        rotation: teams_log::file::Rotation::default(),
    })
    .map_err(|error| format!("the app's log: {error}"))?;
    Ok(())
}

/// The account's settings: a's with the chat with b open, in [`THEME_ENV`]'s theme; b's in [`THEME_B_ENV`]'s.
fn settings(alias: Alias, dir: &Path) -> SandboxResult<SettingsFile> {
    let settings = SettingsFile::new(dir.join("settings.toml"));
    if alias == Alias::A {
        let opened = Settings {
            selected_chat: Some(AB_CHAT.to_owned()),
            appearance: teams_ui::theme::Appearance {
                follow_system: false,
                theme: if std::env::var(THEME_ENV).as_deref() == Ok("dark") {
                    teams_ui::theme::DARK_KEY
                } else {
                    teams_ui::theme::LIGHT_KEY
                },
                ..teams_ui::theme::Appearance::default()
            },
            ..Settings::default()
        };
        settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    } else if std::env::var(THEME_B_ENV).as_deref() == Ok("dark") {
        let mut opened = Settings::default();
        opened.appearance.follow_system = false;
        opened.appearance.theme = teams_ui::theme::DARK_KEY;
        settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    }
    Ok(settings)
}

fn child(alias: Alias) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    app_log(&dir)?;
    let pipewire = Arc::new(PipeWireDevices::connect(Reach::Sandbox)?);
    let devices: Arc<dyn Devices + Send + Sync> = pipewire.clone();
    let slow: Arc<dyn Devices + Send + Sync> = Arc::new(SlowCamera {
        devices: pipewire.clone(),
        sandbox: PathBuf::from(env(ui_sandbox::SANDBOX_ENV)?),
    });
    let feed = VideoFeed::new();
    let engine = engine(alias, slow, feed.clone(), &dir)?;
    let first = support::started(alias, engine)?;
    let runner = support::runner(&dir)?;
    let settings = settings(alias, &dir)?;
    let ringer: Arc<dyn Ringer> = Arc::new(SoundBoard::new(devices));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started {
                    calls: Some(PersonalCalls {
                        ringer: Some(ringer),
                        sounds: None,
                        echo: Mri::new(ECHO),
                        video: Some(feed),
                        camera: Some(pipewire.clone()),
                        devices: Some(pipewire),
                        noise: teams_media::processing::NoiseControl::default(),
                    }),
                    ..first
                },
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(_) => {
                quit_when_asked(cx);
                say("ready");
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
    say("quit");
    Ok(())
}

/// The sandbox's devices, with a camera that opens slowly while the parent asks it to (a USB camera can take
/// seconds): while [`HOLD_CAMERA`] is in the sandbox dir, the camera opens only once it is gone.
struct SlowCamera {
    devices: Arc<PipeWireDevices>,
    sandbox: PathBuf,
}

impl Devices for SlowCamera {
    fn ready(&self) -> DeviceReady<'_> {
        self.devices.ready()
    }

    fn preparing(&self) -> bool {
        self.devices.preparing()
    }

    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.devices.microphone()
    }

    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.devices.speaker()
    }

    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        let hold = self.sandbox.join(HOLD_CAMERA);
        if hold.exists() {
            let _ = fs::write(self.sandbox.join(CAMERA_HELD), "");
            while hold.exists() {
                thread::sleep(Duration::from_millis(50));
            }
        }
        self.devices.camera()
    }

    fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.devices.screen()
    }
}

/// Once the parent writes [`QUIT_TRIGGER`] in the sandbox dir: the engine shut down, then the app quits.
fn quit_when_asked(cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let trigger = PathBuf::from(dir).join(QUIT_TRIGGER);
    cx.spawn(async move |cx: &mut AsyncApp| {
        while !trigger.exists() {
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        cx.update(|cx| {
            app::quit(cx);
        });
    })
    .detach();
}
