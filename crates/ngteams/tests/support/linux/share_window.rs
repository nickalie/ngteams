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
//!   window no longer presents or draws it. a shares again: a new portal session, b presents it again; a stops again.
//! - **Group call**: the mock's c rings a and b in the a–b–c chat, both answer. a shares: a new portal session, the
//!   sharing line described `sendonly` to the service, a's screen read for [`HOLD`], the session open; a stops: the
//!   session closed, the line described `recvonly`. (The mock's mixer doesn't pass a's frames on to b.)
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
use teams_media::devices::Devices;
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
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
/// The steps to run, by name (`one-to-one`, `group`), comma-separated; all of them when unset.
const STEPS_ENV: &str = "SHARE_STEPS";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
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
        Ok(())
    });
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

    for round in 1..=2 {
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
        if round == 1 {
            user.shot_a("share-1to1-sharing.png")?;
            user.shot_b("share-1to1-far-end.png")?;
        }
        user.holds(&format!("1:1 share {round}"), round)?;
        let presented = b.trace.presented_share();
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

    user.a_keys("ctrl+shift+h")?;
    a.in_call(&["ended", "hung-up"])?;
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
    let round = user.calls("ScreenCast.OpenPipeWireRemote") + 1;
    let described = user.descriptions().len();
    user.share(a, round)?;
    user.described(described, "sendonly")?;
    user.holds("group share", round)?;
    user.shot_a("share-group-sharing.png")?;
    user.shot_b("share-group-other.png")?;
    let described = user.descriptions().len();
    user.stop_sharing(a, round)?;
    user.described(described, "recvonly")?;

    user.a_keys("ctrl+shift+h")?;
    a.in_call(&["ended", "hung-up"])?;
    user.b_keys("ctrl+shift+h")?;
    b.in_call(&["ended", "hung-up"])?;
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

    fn b_keys(&mut self, spec: &str) -> SandboxResult {
        key(&mut self.b_input, spec)
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
        let share = self.share_button(false)?;
        self.click(share)?;
        if round == 1 {
            self.shot_a("share-menu.png")?;
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
    line["view"] == "video-tile" && line["change"] == "presented" && strings(line, "kinds") == ["share"]
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
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let running = Running::new(child)?;
        let mut app = Self {
            alias,
            running,
            trace: TraceReader::start(reader),
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

fn child(alias: Alias) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let pipewire = Arc::new(PipeWireDevices::connect(Reach::Sandbox)?);
    let devices: Arc<dyn Devices + Send + Sync> = pipewire.clone();
    let feed = VideoFeed::new();
    let engine = engine(alias, Arc::clone(&devices), feed.clone(), &dir)?;
    let first = support::started(alias, engine)?;
    let runner = support::runner(&dir)?;
    let settings = SettingsFile::new(dir.join("settings.toml"));
    if alias == Alias::A {
        let opened = Settings {
            selected_chat: Some(AB_CHAT.to_owned()),
            ..Settings::default()
        };
        settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    }
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
