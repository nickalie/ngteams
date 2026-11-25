//! A screen that holds still, shared in a 1:1 call, as a user sees it in the far end's window (task #577: "when I
//! started second screen share in the same call nobody saw it"), in the UI sandbox, on the mock (`harness = false`:
//! GPUI runs on the process's main thread).
//!
//! The parent runs the mock chat service with its calling service, its Trouter and its media (a loopback TURN
//! server), and two UI sandboxes. In each sandbox this test runs again as lab account a's or b's app
//! (`ngteams::app::launch` over an engine on the mock, its calls' media on the sandbox's PipeWire). b's screen is a
//! still source (`teams_media::devices::file::StillSource`): it gives its first frames, then no more, as a desktop's
//! screen cast does while nothing on the screen changes. A marker file in b's sandbox dir makes b's next share a
//! "window" instead: another still source of another shape. b's keyboard opens Share; b's pointer clicks Stop sharing,
//! found in screenshots as a user's eye finds it (`call_pixels`). a only looks.
//!
//! 1. a calls b, b answers. b shares its screen: a's window shows it. b stops: a's window drops it.
//! 2. b shares the same screen again: a's window shows it. b stops.
//! 3. b shares a window: a's window shows it.
//! 4. While b shares, the service sets b's call media up again, as it does when a person (c) is added to a 1:1 call:
//!    a mixer of its own offers b a new session (`newOffer`, `fromMixer`). The share goes on: b's toolbar keeps Stop
//!    sharing, and b describes the new session's sharing line `sendonly`. b stops: the line is described `recvonly`.
//!    The mock itself cannot do this step: its `add` only records the person (c has no calling endpoint, so the add
//!    fails with 580), and it never gives a 1:1 call a new session. So the test plays the service's offer, as the
//!    teams-core test `a_share_going_while_the_media_is_set_up_again_goes_on_the_new_session` does. The mock's mixer
//!    passes no shared frames on, so a cannot see this share.
//!
//! For each share, the time from b's pick to a's window showing the picture (read in a's screenshots) is logged and
//! must stay below [`FIRST_FRAME`]. a's GPU must then present [`MORE_FRAMES`] more frames of it, where b's source gave
//! only [`STILL_FRAMES`]: the picture goes again while it holds still. Screenshots of a's stage at each step, under
//! `target/ui-shots/qa-share-still-1to1-*.png` (and `$QA_SHARE_STILL_EVIDENCE`, if set).
//!
//! Without weston 15, niri, `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::SocketAddr;
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
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::account::web_client_info;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, Identity, Outbox, Relay};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera, StillSource};
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, DeviceReady, Devices, VideoSource};
use teams_media::processing::ProcessingConfig;
use teams_media::relay::RelayServer;
use teams_media::sdp::NgcSdp;
use teams_media::session::SessionConfig;
use teams_media::tone::Tone;
use teams_mock::mixer::MixerPeer;
use teams_mock::{AB_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, TENANT, TrouterMock};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::settings::Settings;
use teams_ui::sounds::SoundBoard;
use teams_ui::{Ringer, SettingsFile, VideoFeed};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys, parse_key};
use ui_sandbox::pipewire::PipeWireOptions;
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

const CHILD_ENV: &str = "NGTEAMS_QA_STILL_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_QA_STILL_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_STILL_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_QA_STILL_TROUTER";
const RELAY_ENV: &str = "NGTEAMS_QA_STILL_RELAY";
const IC3_ENV: &str = "NGTEAMS_QA_STILL_IC3";
const SKYPE_ENV: &str = "NGTEAMS_QA_STILL_SKYPE";
const DIR_ENV: &str = "NGTEAMS_QA_STILL_DIR";
/// Where the screenshots are copied besides `target/ui-shots/`, if set (the QA run's evidence).
const EVIDENCE_ENV: &str = "QA_SHARE_STILL_EVIDENCE";
/// The file in the sandbox dir the parent writes to have the child quit.
const QUIT_TRIGGER: &str = "quit";
/// While this file is in b's sandbox dir, b's next share is a window, not its screen.
const SHARE_WINDOW: &str = "share-window";
/// The screenshots' name prefix.
const SHOTS: &str = "qa-share-still-1to1";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(120);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(60);
/// The longest time from b's pick to a's first frame of the share that the test accepts (under load).
const FIRST_FRAME: Duration = Duration::from_secs(10);
/// The frames of a share a's GPU must present: 30 at b's repeat of 5 a second take 6 s, three times the tile's stale
/// time.
const MORE_FRAMES: u64 = 30;
/// The frames a still source gives before it holds still.
const STILL_FRAMES: u32 = 3;
/// How long the window gets to draw what a click or a key changed.
const SETTLE: Duration = Duration::from_millis(700);
/// The call window's toolbar lies within this many px of its top.
const TOOLBAR_BAND: i32 = 58;
/// The mixer's stream ids start here (well above the 1:1 call's).
const MIXER_BASE: u32 = 80_000;
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
const MIXER_TONE: Tone = Tone {
    hz: 880.0,
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
            eprintln!("qa share still window test: {error}");
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
            ..PipeWireOptions::default()
        }),
        ..SandboxOptions::default()
    }
}

fn parent() -> SandboxResult {
    let Some(a_box) = sandbox_or_skip("ngteams qa share still", &sandbox_options(A_TONE))? else {
        return Ok(());
    };
    let Some(b_box) = sandbox_or_skip("ngteams qa share still", &sandbox_options(B_TONE))? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_group_video(dir.path(), MIXER_TONE)?;
    let lab = MockLab {
        origin: mock.origin(),
        trouter: trouter.origin().to_owned(),
        relay: relay.to_string(),
        dir: dir.path().to_owned(),
    };
    let mut a = AppUnderTest::start(&a_box, &lab, &mock, Alias::A)?;
    let mut b = AppUnderTest::start(&b_box, &lab, &mock, Alias::B)?;
    let service = Service {
        mock: &mock,
        trouter: &trouter,
        runtime: &runtime,
        relay,
    };
    let outcome = User::new(&a_box, &b_box).and_then(|mut user| {
        let flown = flow(&mut user, &service, &mut a, &mut b);
        if flown.is_err() {
            user.shot_a(&format!("{SHOTS}-failed-a.png"))?;
            user.shot_b(&format!("{SHOTS}-failed-b.png"))?;
        }
        flown
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
    say("ngteams qa share still window: ok");
    Ok(())
}

/// What b shares: its screen, or a window (another still source, of another shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Screen,
    Window,
}

/// The whole flow in one 1:1 call: share, stop, share again, a window, then the call's media set up again.
fn flow(user: &mut User<'_>, service: &Service<'_>, a: &mut AppUnderTest, b: &mut AppUnderTest) -> SandboxResult {
    call_up(user, a, b)?;
    let refused = (a.trace.refused(), b.trace.refused());
    let mut times = Vec::new();

    times.push(user.share_seen(a, b, Source::Screen, "1-screen")?);
    user.stop_seen(b, "1-stopped")?;
    times.push(user.share_seen(a, b, Source::Screen, "2-screen-again")?);
    user.stop_seen(b, "2-stopped")?;
    times.push(user.share_seen(a, b, Source::Window, "3-window")?);
    let mixer = set_up_again(user, service, b)?;

    if (a.trace.refused(), b.trace.refused()) != refused {
        return Err(format!(
            "a call action was refused: a {:?}, b {:?}",
            a.trace.refusals(),
            b.trace.refusals()
        )
        .into());
    }
    for (step, time) in &times {
        say(&format!("first frame: share {step}: {} ms", time.as_millis()));
    }
    hang_up(&mut user.b_input, b)?;
    a.in_call(&["ended", "remote"])?;
    drop(mixer);
    Ok(())
}

/// a calls b in their chat, b answers from the notification; b's call window found.
fn call_up(user: &mut User<'_>, a: &mut AppUnderTest, b: &mut AppUnderTest) -> SandboxResult {
    a.trace.wait_for("call-button", "shown")?;
    key(&mut user.a_input, "ctrl+shift+c")?;
    a.in_call(&["chat", "ringing"])?;
    b.trace.wait_for("call-notification", "shown")?;
    let ringing = call_notification(user.b_box)?;
    user.b_box.bus().invoke_action(ringing, "answer")?;
    b.in_call(&["incoming", "connected"])?;
    a.in_call(&["chat", "connected"])?;
    user.find_call_window()?;
    say("call: a called b, b answered");
    Ok(())
}

/// While b shares the window, the service sets b's call media up again (a person added): a mixer offers b a new
/// session. The share goes on: Stop sharing stays, and b describes the new sharing line `sendonly`. b stops: the line
/// is described `recvonly`. The mixer, kept until the call ends.
fn set_up_again(user: &mut User<'_>, service: &Service<'_>, b: &mut AppUnderTest) -> SandboxResult<MixerPeer> {
    let described = service.descriptions().len();
    let mixer = service.new_session_for_b()?;
    let line = service.described(described, "sendonly")?;
    say(&format!(
        "4 person added: b's new session describes its sharing line sendonly ({})",
        line["negotiationTag"]
    ));
    thread::sleep(Duration::from_secs(2));
    user.share_button(true)
        .map_err(|error| format!("4 person added: b's toolbar lost Stop sharing: {error}"))?;
    user.shot_a(&format!("{SHOTS}-4-person-added-a.png"))?;
    user.shot_b(&format!("{SHOTS}-4-person-added-b.png"))?;

    let described = service.descriptions().len();
    user.click_stop_sharing(b, "4 person added")?;
    service.described(described, "recvonly")?;
    say("4 person added: b stopped; its new session's sharing line described recvonly");
    user.shot_b(&format!("{SHOTS}-4-stopped-b.png"))?;
    Ok(mixer)
}

/// The mock and what the test plays of the service besides it.
struct Service<'a> {
    mock: &'a ChatServiceMock,
    trouter: &'a TrouterMock,
    runtime: &'a tokio::runtime::Runtime,
    relay: SocketAddr,
}

impl Service<'_> {
    /// A mixer of its own offers b's call a new session, at the callback b's answer handed out; b answers it, and the
    /// mixer takes that answer.
    fn new_session_for_b(&self) -> SandboxResult<MixerPeer> {
        let mixer = MixerPeer::start_at(self.relay, MIXER_BASE, MIXER_TONE)?;
        let offer = mixer.offer()?;
        let callback = self.callback("/callAcceptance/links/mediaRenegotiation")?;
        let call = self.mock.calls().last().ok_or("the mock has no call")?.id.clone();
        let answer_path = format!("/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/negotiations/{call}/1/t/9/answer");
        self.runtime
            .block_on(self.mock.answer_next("POST", &answer_path, 200, &json!({})));
        let negotiation = json!({"mediaNegotiation": {
            "mediaContent": {"blob": offer.as_str(), "contentType": offer.content_type(), "newOffer": true,
                "fromMixer": true},
            "links": {"mediaAnswer": format!("https://api.flightproxy.teams.microsoft.com{answer_path}?i=10-0-0-1")}
        }});
        let _ = self.trouter.deliver_call(LAB_B, &callback, &negotiation);
        let answer = wait_until(WAIT, || self.answer_at(&answer_path)).ok_or("b didn't answer the new session")?;
        let content = &answer["mediaContent"];
        let sdp = NgcSdp::new(
            content["contentType"].as_str().unwrap_or_default(),
            content["blob"].as_str().unwrap_or_default(),
        )?;
        mixer.accept_answer(&sdp)?;
        Ok(mixer)
    }

    /// The callback path of b's answer (`pointer` in its body), as its Trouter registration names it.
    fn callback(&self, pointer: &str) -> SandboxResult<String> {
        let requests = self.runtime.block_on(self.mock.received_requests());
        let url = requests
            .iter()
            .filter_map(|request| {
                serde_json::from_slice::<Value>(&request.body)
                    .ok()?
                    .pointer(pointer)?
                    .as_str()
                    .map(str::to_owned)
            })
            .next_back()
            .ok_or("b's answer named no media renegotiation callback")?;
        let path = url
            .split_once("/v4/f/")
            .and_then(|(_, rest)| rest.split_once('/'))
            .map(|(_, path)| path.to_owned())
            .ok_or_else(|| format!("no callback path in {url}"))?;
        Ok(path)
    }

    /// b's `mediaAnswer` to the new session, once it came.
    fn answer_at(&self, path: &str) -> Option<Value> {
        let requests = self.runtime.block_on(self.mock.received_requests());
        requests.into_iter().find_map(|request| {
            if request.url.path() != path {
                return None;
            }
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            body.get("mediaAnswer").cloned()
        })
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

    /// Waits until an `updateMediaDescriptions` after the first `after` describes the sharing line `direction`: that
    /// description.
    fn described(&self, after: usize, direction: &str) -> SandboxResult<Value> {
        let found = wait_until(WAIT, || {
            self.descriptions().into_iter().skip(after).find(|sent| {
                sent["descriptions"]
                    .as_array()
                    .and_then(|lines| lines.last())
                    .is_some_and(|line| line["direction"] == direction)
            })
        });
        found.ok_or_else(|| {
            format!(
                "the sharing line wasn't described {direction}: {:?}",
                self.descriptions()
            )
            .into()
        })
    }
}

/// The user at the two sandboxes: a's keyboard, b's pointer and keyboard, b's call window.
struct User<'a> {
    a_box: &'a Sandbox,
    b_box: &'a Sandbox,
    a_input: VirtualInput,
    b_input: VirtualInput,
    /// b's call window on b's screen, as last found.
    frame: Option<Rect>,
    shots: usize,
}

impl<'a> User<'a> {
    fn new(a_box: &'a Sandbox, b_box: &'a Sandbox) -> SandboxResult<Self> {
        Ok(Self {
            a_box,
            b_box,
            a_input: a_box.input()?,
            b_input: b_box.input()?,
            frame: None,
            shots: 0,
        })
    }

    fn b_keys(&mut self, spec: &str) -> SandboxResult {
        key(&mut self.b_input, spec)
    }

    /// b shares `source`: b's keyboard opens Share and picks "Screen, window, or tab". a's window shows it within
    /// [`FIRST_FRAME`], and a's GPU presents far more frames of it than the still source gave: the picture goes again
    /// while it holds still. The step and the time to a's first frame.
    fn share_seen(
        &mut self,
        a: &mut AppUnderTest,
        b: &mut AppUnderTest,
        source: Source,
        step: &str,
    ) -> SandboxResult<(String, Duration)> {
        let window = self.b_box.path(SHARE_WINDOW);
        if source == Source::Window {
            fs::write(&window, "")?;
        } else if window.exists() {
            fs::remove_file(&window)?;
        }
        let mark = a.trace.mark();
        let before = a.trace.presented_share();
        let asked = b.trace.asked("share");
        self.b_keys("ctrl+shift+e")?;
        thread::sleep(SETTLE);
        self.b_keys("down")?;
        let started = Instant::now();
        self.b_keys("enter")?;
        // a's window shows the picture: read in a's screenshots, as a user's eye sees it
        let seen = wait_until(FIRST_FRAME + WAIT, || {
            self.a_shows_share().unwrap_or(false).then(Instant::now)
        })
        .ok_or_else(|| format!("{step}: a's window never showed b's share"))?;
        let first = seen.saturating_duration_since(started);
        if b.trace.asked("share") <= asked {
            return Err(format!("{step}: b's Share wasn't asked").into());
        }
        let traced = a
            .trace
            .first_since(mark, |line| is_tile(line, "shown", "share"))
            .map_or_else(
                || "no new share-tile line".to_owned(),
                |at| {
                    format!(
                        "share tile traced shown at {} ms",
                        at.saturating_duration_since(started).as_millis()
                    )
                },
            );
        say(&format!(
            "share {step} ({source:?}): a's window showed b's still picture {} ms after b's pick ({traced})",
            first.as_millis()
        ));
        if first > FIRST_FRAME {
            return Err(
                format!("{step}: a's first frame came {first:?} after b's pick (bound {FIRST_FRAME:?})").into(),
            );
        }
        a.presents_more(mark, before, (started, step))?;
        if !self.a_shows_share()? {
            return Err(format!("{step}: a's window no longer shows b's still picture").into());
        }
        self.share_button(true)
            .map_err(|error| format!("{step}: b's toolbar shows no Stop sharing: {error}"))?;
        self.shot_a(&format!("{SHOTS}-{step}.png"))?;
        Ok((step.to_owned(), first))
    }

    /// b clicks Stop sharing: a's window no longer shows the picture.
    fn stop_seen(&mut self, b: &mut AppUnderTest, step: &str) -> SandboxResult {
        self.click_stop_sharing(b, step)?;
        let gone = wait_until(WAIT / 2, || (!self.a_shows_share().unwrap_or(true)).then_some(()));
        if gone.is_none() {
            return Err(format!("{step}: a's window still shows b's screen").into());
        }
        self.share_button(false)
            .map_err(|error| format!("{step}: b's toolbar shows no Share: {error}"))?;
        self.shot_a(&format!("{SHOTS}-{step}.png"))?;
        say(&format!("{step}: b stopped; a's window dropped the share"));
        // a user looks a moment before they share again
        thread::sleep(Duration::from_secs(2));
        Ok(())
    }

    /// b clicks Stop sharing, and b's app asks for it.
    fn click_stop_sharing(&mut self, b: &mut AppUnderTest, step: &str) -> SandboxResult {
        let button = self.share_button(true)?;
        let asked = b.trace.asked("share");
        self.click(button)?;
        b.trace.next_where(&format!("{step}: b's Stop sharing asked"), |line| {
            is_asked(line, "share")
        })?;
        if b.trace.asked("share") <= asked {
            return Err(format!("{step}: b's Stop sharing wasn't asked").into());
        }
        Ok(())
    }

    /// b's screen as it is now.
    fn screen(&mut self) -> SandboxResult<RgbImage> {
        self.shots += 1;
        let name = format!("screen-{}.png", self.shots);
        let png = self.b_box.screenshot(&name, false)?;
        let image = image::open(&png)?.to_rgb8();
        let _ = fs::remove_file(png);
        Ok(image)
    }

    /// Finds b's call window (it has the keyboard once it opens): where niri's shot of it alone matches the screen's.
    fn find_call_window(&mut self) -> SandboxResult {
        thread::sleep(SETTLE * 2);
        let deadline = Instant::now() + WAIT;
        loop {
            self.shots += 1;
            let name = format!("window-{}.png", self.shots);
            let png = self.b_box.screenshot(&name, true)?;
            let alone = image::open(&png)?.to_rgb8();
            let _ = fs::remove_file(png);
            let screen = self.screen()?;
            if let Some(frame) = pixels::find(&screen, &alone) {
                self.frame = Some(frame);
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err("b's call window isn't on the screen".into());
            }
            thread::sleep(SETTLE);
        }
    }

    fn window(&self) -> SandboxResult<Rect> {
        self.frame.ok_or_else(|| "b's call window wasn't found".into())
    }

    /// b's pointer rests on the window's frame, off every control.
    fn park(&mut self) -> SandboxResult {
        let window = self.window()?;
        self.b_input
            .move_to(u32_of(window.x + 2), u32_of(window.bottom() - 2))?;
        thread::sleep(Duration::from_millis(150));
        Ok(())
    }

    /// A left click in the middle of `target`, then the pointer parked.
    fn click(&mut self, target: Rect) -> SandboxResult {
        let (x, y) = target.center();
        self.b_input.move_to(u32_of(x), u32_of(y))?;
        thread::sleep(Duration::from_millis(60));
        self.b_input.click(u32_of(x), u32_of(y), keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(60));
        self.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The toolbar's band of b's call window, under the title's two lines, off the window's edge.
    fn band(&self) -> SandboxResult<Rect> {
        let window = self.window()?;
        Ok(Rect {
            x: window.x + 4,
            y: window.y + 6,
            w: window.w - 8,
            h: TOOLBAR_BAND - 12,
        })
    }

    /// b's Share, and while b shares, its Stop sharing: the button right before Leave (Teams' red, the toolbar's
    /// largest).
    fn share_button(&mut self, sharing: bool) -> SandboxResult<Rect> {
        self.park()?;
        let band = self.band()?;
        let shot = self.screen()?;
        let reds = red_blobs(&shot, band);
        let leave = reds
            .iter()
            .max_by_key(|blob| blob.area())
            .copied()
            .ok_or("no Leave in b's toolbar")?;
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

    /// Whether a's window shows b's screen, as a user sees it: the pattern's dark greys fill a fifth of the window
    /// below its toolbar (an avatar's letters and icons are far fewer pixels).
    fn a_shows_share(&mut self) -> SandboxResult<bool> {
        self.shots += 1;
        let name = format!("a-window-{}.png", self.shots);
        let png = self.a_box.screenshot(&name, true)?;
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

    fn shot_a(&self, name: &str) -> SandboxResult {
        keep_shot(self.a_box, name)
    }

    fn shot_b(&mut self, name: &str) -> SandboxResult {
        if self.frame.is_some() {
            self.park()?;
        }
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
    key(input, "ctrl+shift+h")?;
    app.in_call(&["ended", "hung-up"])?;
    Ok(())
}

fn u32_of(value: i32) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(0)
}

/// The id of the first notification of an incoming call on `sandbox`'s bus.
fn call_notification(sandbox: &Sandbox) -> SandboxResult<u32> {
    wait_until(WAIT, || {
        sandbox
            .bus()
            .notifications()
            .into_iter()
            .find(|notification| notification.actions.iter().any(|action| action == "answer"))
    })
    .map(|notification| notification.id)
    .ok_or_else(|| format!("no incoming call's notification: {:?}", sandbox.bus().notifications()).into())
}

fn is_asked(line: &Value, ask: &str) -> bool {
    line["view"] == "call" && line["change"] == "asked" && strings(line, "kinds").iter().any(|kind| kind == ask)
}

fn is_tile(line: &Value, change: &str, tile: &str) -> bool {
    line["view"] == "video-tile" && line["change"] == change && strings(line, "kinds") == [tile]
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

    /// The app's GPU presents at least [`MORE_FRAMES`] frames of b's share after `mark` (the trace tells the count at the
    /// first frame and every 30th, `before` the last told), far more than the still source's [`STILL_FRAMES`].
    fn presents_more(&mut self, mark: usize, before: u64, (started, step): (Instant, &str)) -> SandboxResult {
        // up to 29 frames past `before` were presented untold
        let wanted = if before == 0 {
            MORE_FRAMES
        } else {
            before + MORE_FRAMES + 29
        };
        let what = format!("{step}: {} presented {wanted} frames of the share", self.alias);
        let at = self.trace.wait_since(mark, &what, |line| {
            is_tile(line, "presented", "share") && frames_of(line) >= wanted
        })?;
        say(&format!(
            "share {step}: a's GPU presented {MORE_FRAMES}+ more frames of the still picture by {} ms (b's source gave \
             {STILL_FRAMES})",
            at.saturating_duration_since(started).as_millis()
        ));
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

/// The child's view trace, line by line as it comes, each with the moment it came.
struct TraceReader {
    lines: mpsc::Receiver<(Instant, Value)>,
    seen: Vec<(Instant, Value)>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                let _ = sender.send((Instant::now(), value));
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    fn catch_up(&mut self) -> &[(Instant, Value)] {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        &self.seen
    }

    /// The `call asked` lines naming `ask` so far.
    fn asked(&mut self, ask: &str) -> usize {
        self.catch_up().iter().filter(|(_, line)| is_asked(line, ask)).count()
    }

    /// Where the trace is now, for [`Self::first_since`] and [`Self::wait_since`].
    fn mark(&mut self) -> usize {
        self.catch_up().len()
    }

    /// The most frames of a shared screen the trace has told presented so far.
    fn presented_share(&mut self) -> u64 {
        self.catch_up()
            .iter()
            .filter(|(_, line)| is_tile(line, "presented", "share"))
            .map(|(_, line)| frames_of(line))
            .max()
            .unwrap_or(0)
    }

    /// When the first line after `mark` that is `found` came, if one came yet.
    fn first_since(&mut self, mark: usize, found: impl Fn(&Value) -> bool) -> Option<Instant> {
        self.catch_up()
            .iter()
            .skip(mark)
            .find(|(_, line)| found(line))
            .map(|(at, _)| *at)
    }

    /// When the first line after `mark` that is `found` came; waits for it.
    fn wait_since(&mut self, mark: usize, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Instant> {
        if let Some(at) = self.first_since(mark, &found) {
            return Ok(at);
        }
        self.next_at(what, found).map(|(at, _)| at)
    }

    /// The `call refused` lines so far.
    fn refused(&mut self) -> usize {
        self.refusals().len()
    }

    fn refusals(&mut self) -> Vec<Value> {
        self.catch_up()
            .iter()
            .filter(|(_, line)| line["view"] == "call" && line["change"] == "refused")
            .map(|(_, line)| line.clone())
            .collect()
    }

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult<Value> {
        if let Some((_, line)) = self
            .catch_up()
            .iter()
            .find(|(_, line)| line["view"] == view && line["change"] == change)
        {
            return Ok(line.clone());
        }
        self.next_where(&format!("{view} {change}"), |line| {
            line["view"] == view && line["change"] == change
        })
    }

    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        self.next_at(what, found).map(|(_, line)| line)
    }

    fn next_at(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<(Instant, Value)> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (at, line) = self.lines.recv_timeout(left).map_err(|_| {
                let seen: Vec<&Value> = self.seen.iter().map(|(_, line)| line).collect();
                format!("no trace line {what}; seen {seen:?}")
            })?;
            let hit = found(&line);
            self.seen.push((at, line.clone()));
            if hit {
                return Ok((at, line));
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

/// The account's settings: Teams Light; a's with the chat with b open.
fn settings(alias: Alias, dir: &Path) -> SandboxResult<SettingsFile> {
    let settings = SettingsFile::new(dir.join("settings.toml"));
    let mut opened = Settings::default();
    opened.appearance.follow_system = false;
    opened.appearance.theme = teams_ui::theme::LIGHT_KEY;
    if alias == Alias::A {
        opened.selected_chat = Some(AB_CHAT.to_owned());
    }
    settings.save(&opened).map_err(|problem| format!("{problem:?}"))?;
    Ok(settings)
}

fn child(alias: Alias) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    app_log(&dir)?;
    let pipewire = Arc::new(PipeWireDevices::connect(Reach::Sandbox)?);
    let devices: Arc<dyn Devices + Send + Sync> = pipewire.clone();
    let still: Arc<dyn Devices + Send + Sync> = Arc::new(StillScreens {
        devices: pipewire.clone(),
        sandbox: PathBuf::from(env(ui_sandbox::SANDBOX_ENV)?),
    });
    let feed = VideoFeed::new();
    let engine = engine(alias, still, feed.clone(), &dir)?;
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

/// The sandbox's devices, with a shared screen that holds still: the file backend's test pattern, its first
/// [`STILL_FRAMES`] frames, then none. While [`SHARE_WINDOW`] is in the sandbox dir, a "window" is shared instead: the
/// pattern at another shape, also still.
struct StillScreens {
    devices: Arc<PipeWireDevices>,
    sandbox: PathBuf,
}

impl Devices for StillScreens {
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
        self.devices.camera()
    }

    fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        let file = FileDevices {
            input: AudioInput::Silence,
            recording: None,
            // a window's shape: 4:3, where the screen is 16:9
            camera: PatternCamera {
                width: 960,
                height: 720,
                fps: 15,
            },
            pacing: Pacing::RealTime,
        };
        let source = if self.sandbox.join(SHARE_WINDOW).exists() {
            file.camera()?
        } else {
            file.screen()?
        };
        Ok(Box::new(StillSource::new(source, STILL_FRAMES)))
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
