//! QA of task 844 (round 1), in the UI sandbox: the noise suppression level as a user meets it. The real app runs as a
//! child in the sandbox, as lab account a or b on `teams-mock` (its chat service, its calling service, its Trouter and
//! its loopback media peer), its store and settings in one directory, so a later run is a restart of the same app. The
//! parent presses the sandbox's keys and moves its pointer. It finds each control by the focus ring that Tab draws
//! on it, and the open menus by what changed on the screen. It reads the level from the child (its settings model and
//! the control its calls read) and what the far end heard from the mock's peer. Every name, id and sound is made up.
//!
//! - **settings** (a): Settings > Calls and devices shows "Noise suppression" under the microphone and the speaker,
//!   at Auto by default. Tab reaches the picker; Enter opens it; Down and Enter choose each level, which shows Teams'
//!   order (Auto, Low, High, Off). A click on the picker and on a row chooses too. Shots in Teams Light and Dark, the
//!   list open too.
//! - **restart** (a): the level chosen survives a restart, and the calls' control has it.
//! - **other** (b, on the same settings file): b has Auto, and b's own choice leaves a's level as it was.
//! - **call** (a, its microphone the sandbox's tone with the speaker's echo looped back): the mock's b calls a, and a
//!   answers with the keyboard while the model loads; the first frame reaches the far end within a second of
//!   connected. The call's audio menu changes the level at once, by its keys and by a click on a row, and the frames
//!   keep passing at each level; the far end hears a's tone at Off. Settings shows the menu's level, and the menu
//!   checks the level chosen in Settings. The echo of b's tone is logged, mute silences a, and the CPU of the app at
//!   High and Off is measured. Shots of the audio menu in Teams Light and Dark.
//! - **noise** (a, its microphone a made-up voice with bursts of noise from a file): Off keeps the file's voice and
//!   noise, Low keeps the voice, and the far end hears less noise at High and Auto than at Off. DeepFilterNet also
//!   removes the made-up voice, which is no speech to it, so the voice is not judged at High and Auto.
//!
//! `QA844_EVIDENCE` names a directory that gets the screenshots and the results too. `QA844_ONLY` runs only the
//! named runs (comma-separated). Screenshots also go to `target/ui-shots/qa844-*.png`. Without weston 15, niri,
//! `dbus-daemon`, PipeWire or WirePlumber it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
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
use teams_media::devices::Devices;
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::pipewire::{PipeWireDevices, Reach};
use teams_media::devices::{SAMPLE_RATE, wav};
use teams_media::processing::{NoiseControl, NoiseSuppression, ProcessingConfig};
use teams_media::relay::RelayServer;
use teams_media::session::SessionConfig;
use teams_media::test_signal::{Noise, voice};
use teams_media::tone::{Tone, amplitude_at};
use teams_mock::{CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, TENANT, TrouterMock};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use teams_ui::settings_view::Page;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{SettingsFile, SettingsModel};
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::pipewire::{Echo, PipeWireOptions};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

#[path = "../window.rs"]
mod support;
use support::{Alias, display_name};

const CHILD_ENV: &str = "QA844_CHILD";
const EVIDENCE_ENV: &str = "QA844_EVIDENCE";
const ONLY_ENV: &str = "QA844_ONLY";
const TRACE_FD_ENV: &str = "QA844_TRACE_FD";
const ORIGIN_ENV: &str = "QA844_ORIGIN";
const TROUTER_ENV: &str = "QA844_TROUTER";
const RELAY_ENV: &str = "QA844_RELAY";
const IC3_ENV: &str = "QA844_IC3";
const SKYPE_ENV: &str = "QA844_SKYPE";
const DIR_ENV: &str = "QA844_DIR";
/// The microphone of the child's calls: `tone` (the sandbox's PipeWire) or the path of a WAV file.
const MIC_ENV: &str = "QA844_MIC";
/// The file in the shared dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa844-ask";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
const STEP: Duration = Duration::from_millis(300);
const SETTLE: Duration = Duration::from_millis(900);
/// a's microphone tone in the sandbox.
const A_TONE: Tone = Tone {
    hz: 440.0,
    amplitude: 0.3,
};
/// What the mock's peer (b) plays into the call.
const PEER_TONE: Tone = Tone {
    hz: 880.0,
    amplitude: 0.5,
};
/// The sandbox's loopback of a's speaker into a's microphone.
const ECHO_LOOP: Echo = Echo {
    delay: Duration::from_millis(100),
    attenuation_db: 6.0,
};
/// How long a level is held in a call before what the far end heard is read.
const HOLD: Duration = Duration::from_secs(5);
/// The stretch at the end of a hold that is judged.
const JUDGED: Duration = Duration::from_secs(3);
/// How long the CPU is measured at a level.
const CPU_STRETCH: Duration = Duration::from_secs(10);
/// The length of the made-up voice with noise.
const NOISY_SECONDS: usize = 150;
/// The levels in Teams' order, as the lists show them.
const ORDER: [&str; 4] = ["auto", "low", "high", "off"];
/// A tone at least this loud is heard.
const HEARD: f32 = 0.02;
/// A tone quieter than this is not there.
const QUIET: f32 = 0.005;
/// The least frames the far end has from a in a [`HOLD`]: half of 20 ms packets.
const HOLD_FRAMES: u64 = 125;
/// The longest after connected that the far end may wait for a's first frame, though the model still loads.
const FIRST_FRAME: Duration = Duration::from_secs(1);
/// The most the call's audio menu reaches left of its button's right edge and down from its foot.
const AUDIO_MENU_REACH: (u32, u32) = (320, 420);
/// The tallest a highlighted row of the call's audio menu can be.
const MAX_ROW_HEIGHT: u32 = 40;
/// The x span of the check marks, from the audio menu's left edge (its shadow included), left of the rows' text.
const CHECK_COLUMN: (u32, u32) = (8, 22);
/// How far under Off a level may lower the made-up voice and still keep it, in dB.
const VOICE_KEPT_DB: f32 = 6.0;
/// How far under the microphone's file Off may hear the voice and the noise: the codec and the gain control move the
/// level, but a suppressor takes more out.
const OFF_SLACK_DB: f32 = 12.0;
/// The widest a Tab stop's ring can be that is still a narrow menu button (the toolbar's tools are wider).
const NARROW: u32 = 32;
/// The call window's toolbar strip at its top.
const TOOLBAR_HEIGHT: u32 = 56;
/// The least width of a settings picker's ring (the picker is at least 220 px wide).
const PICKER_WIDTH: u32 = 200;

pub(super) fn main() -> ExitCode {
    let outcome = if std::env::var_os(CHILD_ENV).is_some() {
        child()
    } else {
        parent()
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_noise_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
    result(what);
}

/// What the walk saw wrong that does not stop it: each is said at the end, and the test fails.
static FINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

fn expect(ok: bool, finding: impl FnOnce() -> String) {
    if !ok {
        note(finding());
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// Appends `line` to the evidence's `results.txt`, when the evidence is kept.
fn result(line: &str) {
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        let evidence = PathBuf::from(evidence);
        let _ = fs::create_dir_all(&evidence);
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(evidence.join("results.txt"))
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

fn wanted(run: &str) -> bool {
    let only = std::env::var(ONLY_ENV).unwrap_or_default();
    only.is_empty() || only.split(',').any(|name| name.trim() == run)
}

// --- the parent ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        pipewire: Some(PipeWireOptions {
            tone: A_TONE,
            echo: Some(ECHO_LOOP),
            ..PipeWireOptions::default()
        }),
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams noise suppression", &options)? else {
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mock = runtime.block_on(ChatServiceMock::start())?;
    let trouter = mock.start_trouter()?;
    let dir = tempfile::tempdir()?;
    let relay = mock.start_media(dir.path(), PEER_TONE)?;
    let service = Service {
        mock,
        trouter,
        relay: relay.to_string(),
    };
    let noisy = dir.path().join("noisy-voice.wav");
    write_noisy_voice(&noisy)?;
    if let Err(error) = runs(&sandbox, &service, dir.path(), &noisy) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams noise suppression: ok");
    Ok(())
}

/// The mock and where the children reach it.
struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    relay: String,
}

impl Service {
    fn env(&self, mri: &str) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.mock.origin()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (RELAY_ENV, self.relay.clone()),
            (IC3_ENV, self.mock.token(mri).unwrap_or_default()),
            (SKYPE_ENV, self.mock.skypetoken(mri).unwrap_or_default()),
        ]
    }
}

/// The runs on one store and one settings file.
fn runs(sandbox: &Sandbox, service: &Service, dir: &Path, noisy: &Path) -> SandboxResult {
    let noisy = noisy.to_string_lossy().into_owned();
    for (name, alias, mic) in [
        ("settings", Alias::A, "tone"),
        ("restart", Alias::A, "tone"),
        ("other", Alias::B, "tone"),
        ("call", Alias::A, "tone"),
        ("noise", Alias::A, noisy.as_str()),
    ] {
        if !wanted(name) {
            continue;
        }
        step(&format!("--- run {name} ---"));
        let mri = if alias == Alias::B { LAB_B } else { LAB_A };
        let mut run = AppRun::launch(sandbox, &service.env(mri), dir, alias, mic)?;
        let outcome = {
            let mut walk = Walk {
                run: &mut run,
                sandbox,
                service,
            };
            match name {
                "settings" => walk.settings(),
                "restart" => walk.restart(),
                "other" => walk.other_account(),
                "call" => walk.call(),
                _ => walk.noise(),
            }
        };
        if let Err(error) = outcome {
            return Err(run.fail(sandbox, &*error));
        }
        run.quit()?;
    }
    Ok(())
}

/// The made-up voice (`teams_media::test_signal::voice`, a tone that moves and stops as voiced speech does) with
/// bursts of white noise under it that come and go as keys or steps do, written to `path` at 48 kHz.
fn write_noisy_voice(path: &Path) -> SandboxResult {
    let mut writer = wav::WavWriter::create(path)?;
    writer.write(&noisy_voice())?;
    writer.finish()?;
    Ok(())
}

/// The samples of [`write_noisy_voice`].
fn noisy_voice() -> Vec<f32> {
    let frames = NOISY_SECONDS * 100;
    let mut near = voice(frames, 0.3);
    let mut noise = Noise::new(5);
    let mut levels = Noise::new(11);
    for burst in near.chunks_mut(4) {
        let level = 0.12 * levels.next_sample().max(0.0);
        noise.add_to(burst, level);
    }
    teams_media::test_signal::samples(&near)
}

/// A box on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn center(self) -> (u32, u32) {
        (u32::midpoint(self.x0, self.x1), u32::midpoint(self.y0, self.y1))
    }

    fn width(self) -> u32 {
        self.x1 - self.x0
    }

    fn height(self) -> u32 {
        self.y1 - self.y0
    }
}

/// What one run works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    service: &'a Service,
}

impl Walk<'_> {
    /// The settings' level and the calls' control, as keys.
    fn levels(&mut self) -> SandboxResult<(String, String)> {
        let state = self.run.state()?;
        Ok((
            state["level"].as_str().unwrap_or_default().to_owned(),
            state["control"].as_str().unwrap_or_default().to_owned(),
        ))
    }

    /// Waits until the settings and the control are at `level`.
    fn at_level(&mut self, level: &str, what: &str) -> SandboxResult<bool> {
        let start = Instant::now();
        loop {
            let (set, control) = self.levels()?;
            if set == level && control == level {
                step(&format!("{what}: the level is {level}, the calls' control too"));
                return Ok(true);
            }
            if start.elapsed() > Duration::from_secs(5) {
                note(format!(
                    "{what}: the level is {set}, the control {control}; wanted {level}"
                ));
                return Ok(false);
            }
            thread::sleep(STEP);
        }
    }

    /// Opens Settings > Calls and devices, as the rail's Settings and the page's row open it: the keys on its pages.
    fn open_calls_page(&mut self) -> SandboxResult {
        let answer = self.run.question("settings")?;
        if answer != "ok" {
            return Err(format!("the settings didn't open: {answer}").into());
        }
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Tabs from the settings' page column to the noise suppression picker; its ring. The Tab stops in the page are
    /// found by their focus rings; the picker is the third wide one (the microphone, the speaker, then it).
    fn tab_to_picker(&mut self) -> SandboxResult<Rect> {
        let base = screen(self.sandbox, "qa844-tab-base.png")?;
        let region = content_region(&base);
        let mut pickers = Vec::new();
        for tab in 1..=12 {
            self.run.press("Tab")?;
            let now = screen(self.sandbox, "qa844-tab.png")?;
            let Some(ring) = changed_box(&base, &now, region) else {
                step(&format!("Tab {tab}: no ring in the page"));
                continue;
            };
            step(&format!("Tab {tab}: a ring at {ring:?}"));
            if ring.width() >= PICKER_WIDTH && ring.height() < 60 {
                pickers.push(ring);
            }
            if let [microphone, speaker, noise] = pickers[..] {
                expect(noise.y0 > microphone.y0 && noise.y0 > speaker.y0, || {
                    format!("the noise picker {noise:?} isn't under the microphone and the speaker {pickers:?}")
                });
                return Ok(noise);
            }
        }
        Err(format!("Tab didn't reach a third picker: {pickers:?}").into())
    }

    /// The open menu: what changed on the screen once it opened, from `from_y` down.
    fn menu_box(&mut self, before: &image::RgbImage, name: &str, from_y: u32) -> SandboxResult<Rect> {
        let after = screen(self.sandbox, name)?;
        let below = Rect {
            x0: 0,
            y0: from_y,
            x1: after.width(),
            y1: after.height(),
        };
        let changed = changed_box(before, &after, below).ok_or("nothing opened")?;
        if changed.height() < 80 {
            return Err(format!("no menu opened: only {changed:?} changed").into());
        }
        Ok(changed)
    }

    /// Chooses `level` in the settings picker by its keys: Enter opens it, Down to the row, Enter.
    fn choose_by_keys(&mut self, level: &str) -> SandboxResult<bool> {
        let index = ORDER.iter().position(|key| *key == level).ok_or("no level")?;
        self.run.press("Enter")?;
        thread::sleep(STEP);
        for _ in 0..=index {
            self.run.press("Down")?;
        }
        self.run.press("Enter")?;
        self.at_level(level, &format!("Enter, Down ×{}, Enter", index + 1))
    }

    fn settings(&mut self) -> SandboxResult {
        self.open_calls_page()?;
        let (set, control) = self.levels()?;
        expect(set == "auto" && control == "auto", || {
            format!("a new account's level is {set} (control {control}), not Auto")
        });
        shot(self.sandbox, "qa844-settings-auto-light.png")?;
        let picker = self.tab_to_picker()?;
        step(&format!("the noise suppression picker at {picker:?}"));
        shot(self.sandbox, "qa844-settings-picker-focused-light.png")?;
        // the list open by Enter, then each level by its keys: the order is Teams'
        let before = screen(self.sandbox, "qa844-before-open.png")?;
        self.run.press("Enter")?;
        let menu = self.menu_box(&before, "qa844-settings-list-open-light.png", picker.y1 + 2)?;
        step(&format!("the list opened by Enter at {menu:?}"));
        keep(
            &self.sandbox.screenshot("qa844-settings-list-open-light.png", false)?,
            "qa844-settings-list-open-light.png",
        )?;
        self.run.press("Escape")?;
        thread::sleep(STEP);
        for level in ["high", "low", "off", "auto"] {
            self.choose_by_keys(level)?;
            thread::sleep(STEP);
        }
        // the pointer: the picker, then High's row
        self.pointer_choice(picker, "high")?;
        shot(self.sandbox, "qa844-settings-high-light.png")?;
        self.themed_settings_shots(picker, "dark")?;
        self.themed_settings_shots(picker, "light")?;
        let file = self.run.state()?["file"].clone();
        expect(file == json!({LAB_A: "high"}), || {
            format!("the settings file keeps {file}, not a's High")
        });
        Ok(())
    }

    /// Clicks the picker at `picker`, then the row of `level` in its list.
    fn pointer_choice(&mut self, picker: Rect, level: &str) -> SandboxResult<bool> {
        let index = ORDER.iter().position(|key| *key == level).ok_or("no level")?;
        let before = screen(self.sandbox, "qa844-before-click.png")?;
        let (x, y) = picker.center();
        self.run.click(x, y)?;
        let menu = self.menu_box(&before, "qa844-settings-list-clicked.png", picker.y1 + 2)?;
        // four rows, between the list's padding
        let pitch = menu.height().saturating_sub(8) / 4;
        let row_y = menu.y0 + 4 + pitch * u32::try_from(index)? + pitch / 2;
        step(&format!("the list at {menu:?}: {level}'s row at y {row_y}"));
        self.run.click(menu.center().0, row_y)?;
        self.at_level(level, &format!("a click on the picker and on {level}"))
    }

    /// The picker closed and its list open, in theme `theme`.
    fn themed_settings_shots(&mut self, picker: Rect, theme: &str) -> SandboxResult {
        self.run.theme(theme)?;
        shot(self.sandbox, &format!("qa844-settings-high-{theme}.png"))?;
        let (x, y) = picker.center();
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        shot(self.sandbox, &format!("qa844-settings-list-open-{theme}.png"))?;
        self.run.press("Escape")?;
        thread::sleep(STEP);
        Ok(())
    }

    fn restart(&mut self) -> SandboxResult {
        self.at_level("high", "a's restart")?;
        self.open_calls_page()?;
        shot(self.sandbox, "qa844-restart-high-light.png")?;
        Ok(())
    }

    fn other_account(&mut self) -> SandboxResult {
        self.at_level("auto", "b on a's settings file")?;
        self.open_calls_page()?;
        self.tab_to_picker()?;
        self.choose_by_keys("low")?;
        let file = self.run.state()?["file"].clone();
        expect(file == json!({LAB_A: "high", LAB_B: "low"}), || {
            format!("the settings file keeps {file}, not a's High and b's Low")
        });
        Ok(())
    }

    /// The mock's b calls a; a answers with its key. The call's id, once a's call is connected.
    fn answered_call(&mut self) -> SandboxResult<String> {
        let id = self.service.mock.ring(LAB_B, LAB_A);
        self.run.trace.next_where("the call's notification", |line| {
            line["view"] == "call-banner" && line["change"] == "shown"
        })?;
        let answered = Instant::now();
        self.run.press("ctrl+shift+s")?;
        self.run.in_call("connected")?;
        let connected = Instant::now();
        step(&format!(
            "a answered, connected after {} ms",
            answered.elapsed().as_millis()
        ));
        // the frames pass from the start, while the model loads
        let first = wait_until(Duration::from_secs(5), || {
            self.service
                .mock
                .peer_stats(&id, LAB_A)
                .filter(|stats| stats.received > 0)
                .map(|stats| (stats.received, answered.elapsed(), connected.elapsed()))
        });
        match first {
            Some((frames, after, since_connected)) => {
                step(&format!(
                    "the far end had {frames} frames {} ms after the answer, {} ms after connected",
                    after.as_millis(),
                    since_connected.as_millis()
                ));
                expect(since_connected <= FIRST_FRAME, || {
                    format!(
                        "the first frame came {} ms after connected",
                        since_connected.as_millis()
                    )
                });
            }
            None => note("the far end had no frame 5 s after the answer".to_owned()),
        }
        thread::sleep(SETTLE);
        Ok(id)
    }

    /// The recording of a by the mock's peer of call `id`.
    fn heard(&self, id: &str) -> SandboxResult<Vec<f32>> {
        let path = self.service.mock.peer_recording(id, LAB_A).ok_or("no peer recording")?;
        Ok(wav::read(&path)?)
    }

    /// The call window's Tab stops, by their rings: the audio menu's button is the narrow one beside the first.
    fn audio_button(&mut self) -> SandboxResult<Rect> {
        let base = screen(self.sandbox, "qa844-call-base.png")?;
        // the toolbar's strip, right of the title and the call's clock, which change by themselves
        let toolbar = Rect {
            x0: base.width() / 2,
            y0: 0,
            x1: base.width(),
            y1: TOOLBAR_HEIGHT,
        };
        let mut rings: Vec<Rect> = Vec::new();
        for tab in 1..=20 {
            self.run.press("Tab")?;
            let now = screen(self.sandbox, "qa844-call-tab.png")?;
            let Some(ring) = changed_box(&base, &now, toolbar) else {
                step(&format!("call Tab {tab}: no ring in the toolbar"));
                continue;
            };
            step(&format!("call Tab {tab}: a ring at {ring:?}"));
            if ring.width() <= NARROW {
                return Ok(ring);
            }
            rings.push(ring);
        }
        Err(format!("Tab reached no narrow menu button in the toolbar: {rings:?}").into())
    }

    /// Opens the call's audio menu by a click on `button`; the menu's box, within [`audio_menu_region`], away from the
    /// call's clock and the stage, which change by themselves.
    fn open_audio_menu(&mut self, button: Rect, name: &str) -> SandboxResult<Rect> {
        let before = screen(self.sandbox, "qa844-call-before-menu.png")?;
        let (x, y) = button.center();
        self.run.click(x, y)?;
        thread::sleep(SETTLE);
        let after = screen(self.sandbox, name)?;
        let menu = changed_box(&before, &after, audio_menu_region(button)).ok_or("the audio menu did not open")?;
        if menu.height() < 80 {
            return Err(format!("no audio menu opened: only {menu:?} changed").into());
        }
        keep(&self.sandbox.screenshot(name, false)?, name)?;
        Ok(menu)
    }

    /// Chooses `level` in the open audio menu by its keys: Up from the foot ("More audio settings"), then Enter.
    fn audio_menu_keys(&mut self, level: &str) -> SandboxResult<bool> {
        let index = ORDER.iter().position(|key| *key == level).ok_or("no level")?;
        for _ in 0..=(ORDER.len() - index) {
            self.run.press("Up")?;
        }
        self.run.press("Enter")?;
        self.at_level(level, &format!("the call's audio menu, {level} by its keys"))
    }

    /// The row of `level` in the audio menu open at `menu`, by the highlight its keys move to it: Up from the foot, as
    /// [`Self::audio_menu_keys`] does. The menu shows no highlight before the first key.
    fn audio_menu_row(&mut self, menu: Rect, level: &str) -> SandboxResult<Rect> {
        let index = ORDER.iter().position(|key| *key == level).ok_or("no level")?;
        let before = screen(self.sandbox, "qa844-call-menu-before-keys.png")?;
        for _ in 0..=(ORDER.len() - index) {
            self.run.press("Up")?;
        }
        let after = screen(self.sandbox, "qa844-call-menu-keys.png")?;
        let row = changed_box(&before, &after, menu).ok_or("the keys highlighted no row")?;
        if row.height() > MAX_ROW_HEIGHT {
            return Err(format!("the highlight of {level} is not one row: {row:?}").into());
        }
        Ok(row)
    }

    /// Chooses `level` in the audio menu open at `menu` by a click on its row.
    fn audio_menu_click(&mut self, menu: Rect, level: &str) -> SandboxResult<bool> {
        let row = self.audio_menu_row(menu, level)?;
        step(&format!("the audio menu's {level} row at {row:?}"));
        let (x, y) = row.center();
        self.run.click(x, y)?;
        self.at_level(level, &format!("the call's audio menu, a click on {level}"))
    }

    /// The far end's last [`JUDGED`] of a's tone and of the echo of b's, in the recording of call `id`.
    fn tones(&self, id: &str) -> SandboxResult<(f32, f32)> {
        let heard = self.heard(id)?;
        let tail = tail(&heard, JUDGED)?;
        Ok((amplitude_at(tail, A_TONE.hz), amplitude_at(tail, PEER_TONE.hz)))
    }

    fn call(&mut self) -> SandboxResult {
        self.at_level("high", "a's call run starts")?;
        let id = self.answered_call()?;
        let button = self.audio_button()?;
        step(&format!("the audio menu's button at {button:?}"));
        let mut menu = self.open_audio_menu(button, "qa844-call-audio-menu-light.png")?;
        // each level from the menu: it applies at once; the frames keep passing to the far end
        let mut tones = Vec::new();
        for level in ["off", "low", "auto", "high"] {
            if level != "off" {
                menu = self.open_audio_menu(button, "qa844-call-audio-menu-reopen.png")?;
            }
            if level == "low" {
                self.audio_menu_click(menu, level)?;
            } else {
                self.audio_menu_keys(level)?;
            }
            let before = self.frames(&id);
            thread::sleep(HOLD);
            let frames = self.frames(&id);
            let (own, echo) = self.tones(&id)?;
            step(&format!(
                "at {level}: the far end hears a's 440 Hz at {own:.4}, the echo of its 880 Hz at {echo:.4}, {} frames in the hold",
                frames.saturating_sub(before)
            ));
            result(&format!(
                "call level {level}: a {own:.4}, echo {echo:.4}, frames {before} to {frames}"
            ));
            expect(frames >= before + HOLD_FRAMES, || {
                format!(
                    "at {level} the far end had only {} frames in {} s",
                    frames.saturating_sub(before),
                    HOLD.as_secs()
                )
            });
            tones.push((level, own, echo));
        }
        // the echo of a steady tone is only logged: the canceller can't tell one period of it from the next, so it
        // reads from 0.0001 to 0.1 between runs; teams-media's processing tests judge the echo on a speech-like far end
        for (level, own, _) in &tones {
            if *level == "off" {
                expect(*own > HEARD, || {
                    format!("at Off the far end hears a's tone at {own:.4} only")
                });
            }
        }
        self.settings_follow_the_call(button)?;
        self.mute(&id)?;
        self.cpu(button)?;
        self.run.theme("dark")?;
        self.open_audio_menu(button, "qa844-call-audio-menu-dark.png")?;
        self.run.press("Escape")?;
        self.run.theme("light")?;
        self.run.press("ctrl+shift+h")?;
        self.run.in_call("ended")?;
        Ok(())
    }

    /// The frames the far end of call `id` has had from a.
    fn frames(&self, id: &str) -> u64 {
        self.service
            .mock
            .peer_stats(id, LAB_A)
            .map_or(0, |stats| stats.received)
    }

    /// The level from the call's menu shows in Settings; one chosen in Settings shows checked in the call's menu.
    fn settings_follow_the_call(&mut self, button: Rect) -> SandboxResult {
        let state = self.run.state()?;
        expect(state["level"] == "high", || {
            format!("Settings shows {}, not the call's High", state["level"])
        });
        let answer = self.run.question("settings-level low")?;
        expect(answer == "ok", || format!("the settings model refused Low: {answer}"));
        self.at_level("low", "Settings' Low during the call")?;
        let name = "qa844-call-audio-menu-low-light.png";
        let menu = self.open_audio_menu(button, name)?;
        let open = screen(self.sandbox, name)?;
        let low = self.audio_menu_row(menu, "low")?;
        // the rows touch, so a row's height is the step to the next one
        let checks: Vec<(&str, bool)> = ORDER
            .iter()
            .zip(-1_i64..)
            .map(|(level, offset)| (*level, checked(&open, menu, low, offset)))
            .collect();
        step(&format!("the call's menu after Settings' Low: checked {checks:?}"));
        expect(checks.iter().all(|(level, check)| *check == (*level == "low")), || {
            format!("after Settings' Low the call's menu checks {checks:?}")
        });
        self.run.press("Escape")?;
        thread::sleep(STEP);
        self.open_audio_menu(button, "qa844-call-audio-menu-reopen.png")?;
        self.audio_menu_keys("high")?;
        Ok(())
    }

    /// Mute silences a at the far end; unmute brings a back. At Off, which passes a's steady tone.
    fn mute(&mut self, id: &str) -> SandboxResult {
        let answer = self.run.question("settings-level off")?;
        expect(answer == "ok", || format!("the settings model refused Off: {answer}"));
        self.at_level("off", "Off for the mute")?;
        thread::sleep(HOLD);
        self.run.press("ctrl+shift+m")?;
        self.run.in_call("muted")?;
        thread::sleep(HOLD);
        let (muted, _) = self.tones(id)?;
        self.run.press("ctrl+shift+m")?;
        thread::sleep(HOLD);
        let (back, _) = self.tones(id)?;
        step(&format!(
            "muted the far end hears a at {muted:.4}, unmuted at {back:.4}"
        ));
        result(&format!("mute: {muted:.4}, unmuted: {back:.4}"));
        expect(muted < QUIET, || {
            format!("muted, the far end still hears a at {muted:.4}")
        });
        expect(back > HEARD, || {
            format!("unmuted, the far end hears a at {back:.4} only")
        });
        Ok(())
    }

    /// The app's CPU at High and at Off, over [`CPU_STRETCH`] each.
    fn cpu(&mut self, button: Rect) -> SandboxResult {
        for level in ["high", "off"] {
            self.open_audio_menu(button, "qa844-call-audio-menu-cpu.png")?;
            self.audio_menu_keys(level)?;
            thread::sleep(SETTLE);
            let before = self.run.cpu()?;
            thread::sleep(CPU_STRETCH);
            let after = self.run.cpu()?;
            #[expect(clippy::cast_precision_loss, reason = "ms of CPU time")]
            let share = (after - before) as f64 / CPU_STRETCH.as_millis() as f64 * 100.0;
            step(&format!(
                "CPU at {level}: {} ms in {} s, {share:.1}% of one core",
                after - before,
                CPU_STRETCH.as_secs()
            ));
            result(&format!("cpu {level}: {share:.1}% of one core"));
        }
        Ok(())
    }

    fn noise(&mut self) -> SandboxResult {
        let id = self.answered_call()?;
        let button = self.audio_button()?;
        let mut levels = Vec::new();
        for level in ["off", "high", "auto", "low", "off"] {
            self.open_audio_menu(button, "qa844-noise-menu.png")?;
            self.audio_menu_keys(level)?;
            thread::sleep(HOLD);
            let heard = self.heard(&id)?;
            let (voice, noise) = voice_and_noise(tail(&heard, JUDGED)?);
            step(&format!("noise run at {level}: voice {voice:.4}, noise {noise:.4}"));
            levels.push((level, voice, noise));
        }
        let source = noisy_voice();
        let (source_voice, source_noise) = voice_and_noise(tail(&source, JUDGED)?);
        step(&format!(
            "the microphone's file: voice {source_voice:.4}, noise {source_noise:.4}"
        ));
        // Off is read twice, first and last: the gain control moves the level between them
        let off = levels[0].2.min(levels[4].2);
        let off_voice = levels[0].1.min(levels[4].1);
        for (level, voice, noise) in &levels {
            let lowered = decibels_under(off, *noise);
            let line = format!("noise level {level}: voice {voice:.4}, noise {noise:.4}, {lowered:.1} dB under Off");
            step(&line);
            result(&line);
            match *level {
                "high" => expect(lowered >= 10.0, || {
                    format!("High lowers the noise only {lowered:.1} dB")
                }),
                "auto" => expect(lowered >= 6.0, || format!("Auto lowers the noise only {lowered:.1} dB")),
                "low" => expect(decibels_under(off_voice, *voice) <= VOICE_KEPT_DB, || {
                    format!("Low lowers the voice from {off_voice:.4} to {voice:.4}")
                }),
                _ => {
                    let (voice_under, noise_under) = (
                        decibels_under(source_voice, *voice),
                        decibels_under(source_noise, *noise),
                    );
                    expect(voice_under <= OFF_SLACK_DB && noise_under <= OFF_SLACK_DB, || {
                        format!(
                            "Off is {voice_under:.1} dB under the file's voice, {noise_under:.1} dB under its noise"
                        )
                    });
                }
            }
        }
        self.run.press("ctrl+shift+h")?;
        self.run.in_call("ended")?;
        Ok(())
    }
}

/// Where the call's audio menu opens: under its button, right-aligned with it.
fn audio_menu_region(button: Rect) -> Rect {
    Rect {
        x0: button.x1.saturating_sub(AUDIO_MENU_REACH.0),
        y0: button.y1,
        x1: button.x1,
        y1: button.y1 + AUDIO_MENU_REACH.1,
    }
}

/// How many dB `level` is under `reference`.
fn decibels_under(reference: f32, level: f32) -> f32 {
    20.0 * (reference.max(1e-6) / level.max(1e-6)).log10()
}

/// Whether the audio menu open at `menu` in `shot` draws a check mark beside the row `offset` rows from `row`.
fn checked(shot: &image::RgbImage, menu: Rect, row: Rect, offset: i64) -> bool {
    let step = i64::from(row.height()) * offset;
    let Ok(top) = u32::try_from(i64::from(row.y0) + step) else {
        return false;
    };
    let background = at(shot, menu.x0 + CHECK_COLUMN.0 / 2, top + row.height() / 2);
    (top + 4..top + row.height().saturating_sub(4))
        .any(|y| (menu.x0 + CHECK_COLUMN.0..menu.x0 + CHECK_COLUMN.1).any(|x| !near(at(shot, x, y), background, 60)))
}

/// The last `stretch` of `samples`.
fn tail(samples: &[f32], stretch: Duration) -> SandboxResult<&[f32]> {
    let wanted = usize::try_from(stretch.as_millis())? * usize::try_from(SAMPLE_RATE / 1_000)?;
    if samples.len() < wanted {
        return Err(format!("the recording holds {} samples", samples.len()).into());
    }
    Ok(&samples[samples.len() - wanted..])
}

/// The frequencies the noise is read at: between the voice's tones and narrowband's 4 kHz, so a band that Opus narrows
/// under load does not read as less noise.
const NOISE_HZ: [f32; 6] = [1_200.0, 1_500.0, 1_900.0, 2_300.0, 2_700.0, 3_100.0];
/// The windows the noise is read in: 20 ms, a whole number of periods of each of [`NOISE_HZ`].
const NOISE_WINDOW: usize = 960;

/// The voice (the mean of its four tones) and the noise (the mean amplitude at [`NOISE_HZ`] over 20 ms windows, where
/// the voice has none) of `samples`.
fn voice_and_noise(samples: &[f32]) -> (f32, f32) {
    let voice = teams_media::test_signal::VOICE_HZ
        .iter()
        .map(|hz| amplitude_at(samples, *hz))
        .sum::<f32>()
        / 4.0;
    let readings: Vec<f32> = samples
        .as_chunks::<NOISE_WINDOW>()
        .0
        .iter()
        .flat_map(|window| NOISE_HZ.iter().map(|hz| amplitude_at(window, *hz)))
        .collect();
    #[expect(clippy::cast_precision_loss, reason = "a count of readings")]
    let noise = readings.iter().sum::<f32>() / readings.len().max(1) as f32;
    (voice, noise)
}

/// The settings' content: right of their page column, under the title bar.
fn content_region(shot: &image::RgbImage) -> Rect {
    Rect {
        x0: shot.width() * 2 / 5,
        y0: 60,
        x1: shot.width(),
        y1: shot.height(),
    }
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

/// The bounds of the pixels that changed in `region`.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in region.y0..region.y1.min(before.height()).min(after.height()) {
        for x in region.x0..region.x1.min(before.width()).min(after.width()) {
            if !near(at(before, x, y), at(after, x, y), 8) {
                bounds = Some(bounds.map_or(
                    Rect {
                        x0: x,
                        y0: y,
                        x1: x + 1,
                        y1: y + 1,
                    },
                    |known| Rect {
                        x0: known.x0.min(x),
                        y0: known.y0.min(y),
                        x1: known.x1.max(x + 1),
                        y1: known.y1.max(y + 1),
                    },
                ));
            }
        }
    }
    bounds
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Keeps the screenshot `png` as `name` under `target/ui-shots/`, and in `QA844_EVIDENCE` when it is set.
fn keep(png: &Path, name: &str) -> SandboxResult {
    fs::copy(png, shots_dir()?.join(name))?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        let evidence = PathBuf::from(evidence);
        fs::create_dir_all(&evidence)?;
        fs::copy(png, evidence.join(name))?;
    }
    Ok(())
}

/// The screen now, as an image (kept in the sandbox as `name`).
fn screen(sandbox: &Sandbox, name: &str) -> SandboxResult<image::RgbImage> {
    thread::sleep(STEP);
    let png = sandbox.screenshot(name, false)?;
    Ok(image::open(&png)?.to_rgb8())
}

/// A screenshot of the screen, kept as `target/ui-shots/<name>` and in the evidence.
fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
    thread::sleep(SETTLE);
    let png = sandbox.screenshot(name, false)?;
    keep(&png, name)?;
    Ok(png)
}

// --- the app in the sandbox ---

/// The app running in the sandbox: its process, its trace, its directory and the user's pointer and keys.
struct AppRun {
    running: Running,
    trace: TraceReader,
    dir: PathBuf,
    input: VirtualInput,
    rest: (u32, u32),
    asked: u32,
}

impl AppRun {
    fn launch(
        sandbox: &Sandbox,
        env: &[(&'static str, String)],
        dir: &Path,
        alias: Alias,
        mic: &str,
    ) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let dir_text = dir.to_string_lossy().into_owned();
        let account = alias.to_string();
        let mut vars: Vec<(&str, &str)> = vec![
            (CHILD_ENV, account.as_str()),
            (DIR_ENV, &dir_text),
            (TRACE_FD_ENV, &fd),
            (MIC_ENV, mic),
        ];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        let child = sandbox.spawn_with_fds(&std::env::current_exe()?, &[], &vars, &[writer.as_fd()])?;
        drop(writer);
        let mut running = Running::new(child)?;
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            return Err(running.fail(&format!("no ready line: {error:?}")));
        }
        let mut run = Self {
            running,
            trace: TraceReader::start(reader),
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (640, 12),
            asked: 0,
        };
        run.trace.wait_for("calling", "registered")?;
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

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

    fn state(&mut self) -> SandboxResult<Value> {
        let answer = self.question("state")?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// The app's CPU time so far, in ms.
    fn cpu(&mut self) -> SandboxResult<u64> {
        Ok(self.question("cpu")?.parse()?)
    }

    fn theme(&mut self, theme: &str) -> SandboxResult {
        let answer = self.question(&format!("theme {theme}"))?;
        if answer != "ok" {
            return Err(format!("theme {theme}: {answer}").into());
        }
        thread::sleep(SETTLE);
        Ok(())
    }

    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.move_to(x, y)?;
        thread::sleep(STEP);
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(STEP);
        Ok(())
    }

    /// Waits for an `in-call shown` line whose kinds hold `kind`.
    fn in_call(&mut self, kind: &str) -> SandboxResult<Value> {
        self.trace.next_where(&format!("in-call {kind}"), |line| {
            line["view"] == "in-call"
                && line["change"] == "shown"
                && line["kinds"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|shown| shown == kind))
        })
    }

    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot =
            shot(sandbox, "qa844-failed-run.png").map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    fn quit(mut self) -> SandboxResult {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} quit", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
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

    fn wait_for(&mut self, view: &str, change: &str) -> SandboxResult<Value> {
        if let Some(line) = self
            .seen
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
            let line = self.lines.recv_timeout(left).map_err(|_| {
                let last: Vec<&Value> = self.seen.iter().rev().take(20).collect();
                format!("no trace line {what}; the last seen {last:?}")
            })?;
            let hit = found(&line);
            self.seen.push(line.clone());
            if hit {
                return Ok(line);
            }
        }
    }
}

// --- the child ---

/// One account's mock tokens.
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

/// The account's engine on the mock: its calls' media on `devices`, processed as the app's calls are, at the level
/// of `noise`, relayed through the mock's TURN server.
fn engine(
    alias: Alias,
    mri: &str,
    devices: Arc<dyn Devices + Send + Sync>,
    noise: NoiseControl,
    dir: &Path,
) -> SandboxResult<EngineHandle> {
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
        video: None,
        relay: Relay::Fixed(vec![RelayServer::udp(env(RELAY_ENV)?.parse()?)]),
        // as the app's calls run: the call's processing, at the account's level
        session: SessionConfig {
            processing: ProcessingConfig::CALL,
            noise,
        },
    };
    let push = Push::new(trouter()?);
    Ok(engine::start(Parts {
        calls: Some(CallParts::new(calling, identity, media)),
        store: Store::open(&support::store_file(dir, alias))?,
        client: Arc::new(Client::new(http()?)),
        outbox: Outbox::new(transport, Author::new(Mri::new(mri), name))
            .with_calls(Arc::clone(&tracker), teams_core::CallFloor::new()),
        push: Some(push),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?)
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(env(TRACE_FD_ENV)?.parse()?)?);
    let dir = PathBuf::from(env(DIR_ENV)?);
    let (alias, mri) = if env(CHILD_ENV)? == "b" {
        (Alias::B, LAB_B)
    } else {
        (Alias::A, LAB_A)
    };
    let pipewire = Arc::new(PipeWireDevices::connect(Reach::Sandbox)?);
    let mic = env(MIC_ENV)?;
    let devices: Arc<dyn Devices + Send + Sync> = if mic == "tone" {
        pipewire.clone()
    } else {
        Arc::new(FileDevices {
            input: AudioInput::Wav(PathBuf::from(&mic)),
            recording: None,
            camera: PatternCamera::default(),
            pacing: Pacing::RealTime,
        })
    };
    let noise = NoiseControl::default();
    let engine = engine(alias, mri, devices, noise.clone(), &dir)?;
    let first = support::started(alias, engine)?;
    let runner = support::runner(&dir.join(alias.to_string()))?;
    // one settings file for both accounts: each keeps its level under its MRI
    let settings_path = dir.join("settings.toml");
    let settings = SettingsFile::new(settings_path.clone());
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: ngteams::accounts::Started {
                    calls: Some(PersonalCalls {
                        ringer: None,
                        sounds: None,
                        echo: Mri::new(ECHO),
                        video: None,
                        camera: None,
                        devices: Some(pipewire),
                        noise: noise.clone(),
                    }),
                    ..first
                },
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: None,
                settings: SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                let context = Answering {
                    window,
                    ask: ask.clone(),
                    noise: noise.clone(),
                    file: settings_path.clone(),
                };
                answer(context, cx);
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
    Ok(())
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

/// What the child answers the parent from.
struct Answering {
    window: WindowHandle<KitRoot>,
    ask: PathBuf,
    noise: NoiseControl,
    file: PathBuf,
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(context: Answering, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(20)).await;
            let Ok(question) = fs::read_to_string(&context.ask) else {
                continue;
            };
            let _ = fs::remove_file(&context.ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(app::quit);
                return;
            }
            let reply = context
                .window
                .update(cx, |kit, window, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, &context, window, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, context: &Answering, window: &mut Window, cx: &mut App) -> String {
    let (verb, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "state" => state_of(shell, context, cx).to_string(),
        // as the rail's Settings, then the page's row, open it: the keys on the pages
        "settings" => {
            shell.update(cx, |shell, cx| shell.open_settings_page(Page::Calls, window, cx));
            "ok".to_owned()
        }
        // as the Settings picker sets it
        "settings-level" => {
            let Some(level) = NoiseSuppression::from_key(rest) else {
                return "no level".to_owned();
            };
            let account = shell.read(cx).account().to_owned();
            let model = shell.read(cx).settings().clone();
            model.update(cx, |model, cx| {
                model.change(|settings| settings.set_noise_suppression(&account, level), cx);
            });
            "ok".to_owned()
        }
        "theme" => {
            let key = if rest == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let model = shell.read(cx).settings().clone();
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
            "ok".to_owned()
        }
        "cpu" => cpu_ms().map_or_else(|| "no cpu".to_owned(), |ms| ms.to_string()),
        other => format!("unknown {other}"),
    }
}

/// The level the account's settings keep, the control its calls read, what the settings show and the file's levels.
fn state_of(shell: &Entity<Shell>, context: &Answering, cx: &App) -> Value {
    let shell = shell.read(cx);
    let account = shell.account().to_owned();
    let settings = shell.settings().read(cx).settings();
    let file = fs::read_to_string(&context.file)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|table| table.get("noise_suppression").cloned())
        .map_or(json!({}), |levels| serde_json::to_value(levels).unwrap_or_default());
    json!({
        "account": account,
        "level": settings.noise_suppression_of(&account).key(),
        "control": context.noise.level().key(),
        "settings_shown": matches!(shell.main(), Main::Settings),
        "page": format!("{:?}", shell.settings_view().read(cx).page()),
        "file": file,
    })
}

/// This process's CPU time (user and system), in ms.
fn cpu_ms() -> Option<u64> {
    let stat = fs::read_to_string("/proc/self/stat").ok()?;
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // utime and stime: the 14th and 15th fields, the 12th and 13th after the name
    let ticks: u64 = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    Some(ticks * 10)
}
