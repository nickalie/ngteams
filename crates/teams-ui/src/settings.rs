//! The app's settings, kept in a TOML file in its own data directory: the data and the file are `teams-app`'s
//! (`teams_app::settings`, re-exported here), the GPUI model the views share is this module's.
//!
//! - **What**: the chat shown last ([`Settings::selected_chat`]), whether typing, read receipts and the account's own
//!   availability are sent, which messages notify ([`Notifications`]) and whether a meeting's start does
//!   ([`Settings::meeting_started`]), the idle time after which the account's
//!   availability stops being reported, the account's manual status ([`ManualStatus`]), whether calls carry
//!   video ([`Settings::video_calls`], its switch shown only where the app's calls have video), the calls'
//!   microphone, speaker and camera ([`Settings::microphone`] and the others: a device's name, or the system's default
//!   when none is named), the camera's background in calls ([`Settings::video_effect`]: none, blurred, one of the
//!   app's pictures or one the user added, kept in [`SettingsModel::backgrounds_dir`]), how the app looks ([`Settings::appearance`]: the theme, following the system's light or dark
//!   mode or not, the font size and family), the clock times are shown on ([`Settings::time_format`]: the system's,
//!   12-hour or 24-hour), and whether the first run's screen of the writes that happen on their
//!   own was confirmed.
//!   Tasks 13, 14 and 20 act on the ones Task 8 only kept.
//! - **The sounds** ([`crate::sounds`], [`attach_sounds`]): which sound each event plays, how loud and the packs they
//!   come from, the app's for every account; what plays them follows every change at once.
//! - **The devices** (M3 Task 20, [`attach_devices`]): with the calls' devices attached, the model keeps the list of
//!   devices there are (each change of it shown as it comes), hands the device directory the choice of each kind at
//!   once and on every change, so a call that runs moves to the device chosen; the settings view lists them.
//! - **The noise suppression** ([`noise`]): the level of each account's calls (Auto, High, Low, Off), which a call
//!   follows at once ([`noise::follow`]).
//! - **The first run** (the personal build, M2 Task 27, [`SettingsModel::requiring_first_run`]): typing, read receipts
//!   and the account's availability are the writes that happen without a per-item action; until the user confirms the
//!   first-run screen ([`Settings::first_run`]) none of them is sent, whatever its switch says
//!   ([`SettingsModel::sends_typing`], [`SettingsModel::sends_read_receipts`], [`SettingsModel::sends_presence`]). The
//!   lab app doesn't ask: its settings send as their switches say.
//! - **The AI notice** ([`SettingsModel::awaiting_ai_notice`]): an app that shows the first run's screen also asks
//!   once if the user wants the AI features ([`Settings::ai_features`]), after that screen, or at the next start after
//!   an upgrade. No AI feature runs before the answer. The answer and each later change go to the app's switch
//!   ([`crate::local_ai::AiSwitch`]), which every AI entry point asks.
//! - **Forgiving to read**: a missing file gives the defaults; a file that can't be read or isn't TOML gives the
//!   defaults and is reported, and is never written over: changes hold in memory until the app quits, so the file
//!   is there to mend by hand; a value that is bad (of the wrong type, out of range, a name this build doesn't know)
//!   is reported and that one setting keeps its default, the others are taken; a key that names no setting is
//!   reported and ignored. Every problem is a [`Problem`], shown by the settings view.
//! - **Private to write**: the file is `0600` and its directory `0700`, written whole to a temporary file beside it
//!   and renamed over it, so a crash leaves the old file or the new one. A file found readable by others is tightened
//!   when it is read.
//!
//! [`SettingsModel`] is the GPUI entity the views share: a change goes to the file at once (unless it couldn't be
//! read).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{App, AppContext as _, AsyncApp, ClipboardItem, Context, Entity, Task, WeakEntity};
use teams_log::diagnostics::{Report, Source as DiagnosticsSource};
use teams_media::devices::catalog::{DeviceDirectory, DeviceKind, DeviceList, DeviceStatus};
use teams_media::video::effects::{self, Backdrop, Background};

pub(crate) use teams_app::settings::write_private;
pub use teams_app::settings::{
    AccountSettings, BACKGROUNDS_DIR, BarChoice, BarItem, ChatLanguage, ComposeTool, DEFAULT_IDLE_MINUTES,
    FirstRunState, IDLE_MINUTES, Loaded, LogLevel, MAX_CHAT_ID, MAX_DEVICE_NAME, MAX_FILE, MAX_PICTURE_NAME,
    ManualStatus, Notifications, PersonCalendar, Problem, SOUND_PACKS_DIR, Settings, SettingsFile, TimeFormat,
    VideoEffect, captions_height, is_picture_name, keys, parse, render,
};

use crate::effects::CameraPreview;
use crate::sounds::{self, PackInfo, SoundPlayer, SoundSource};

pub mod noise;

/// The background the media applies for `effect`; a picture the user added is looked for in `dir`, and blurred when
/// there is no `dir` (settings kept in memory) to look in.
fn video_background(effect: &VideoEffect, dir: Option<&Path>) -> Background {
    match effect {
        VideoEffect::None => Background::None,
        VideoEffect::Blur => Background::Blur,
        VideoEffect::Stock(key) => {
            effects::stock(key).map_or(Background::Blur, |stock| Background::Picture(Backdrop::Stock(stock)))
        }
        VideoEffect::Added(name) => dir.map_or(Background::Blur, |dir| {
            Background::Picture(Backdrop::File(dir.join(name)))
        }),
    }
}

/// What the last "copy diagnostics" put on the clipboard, as the settings view tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Copied {
    /// It holds a `debug` line: full requests and responses, not safe to paste.
    pub has_debug: bool,
}

/// The settings as the views share them: a GPUI entity. A change goes to the file at once ([`SettingsModel::change`]);
/// a change of the log's level goes to the log at once too (`teams_log::apply_setting`). It also keeps where the
/// "copy diagnostics" text comes from ([`SettingsModel::copy_diagnostics`]).
pub struct SettingsModel {
    settings: Settings,
    file: Option<SettingsFile>,
    problems: Vec<Problem>,
    /// The file couldn't be read: it is left as it is, and changes hold in memory.
    held: bool,
    diagnostics: Option<DiagnosticsSource>,
    copied: Option<Copied>,
    /// The "copy diagnostics" being collected.
    copying: Option<Task<()>>,
    /// When typing, read receipts and presence may go: as their switches say, after the first run's screen
    /// ([`SettingsModel::requiring_first_run`]), or never ([`SettingsModel::read_only`]).
    signals: Signals,
    /// The app's calls have video: the view shows its switch ([`SettingsModel::with_video_switch`]).
    video_switch: bool,
    /// The calls' devices, once attached ([`attach_devices`]): where each choice goes, and the devices there are.
    devices: Option<Devices>,
    /// The camera's preview of the video effects, once attached ([`attach_camera_preview`]).
    preview: Option<CameraPreview>,
    /// What plays the app's sounds, once attached ([`attach_sounds`]).
    sounds: Option<Arc<dyn SoundPlayer>>,
    /// The sound packs the user imported, as last read ([`SettingsModel::reload_sound_packs`]).
    sound_packs: Vec<PackInfo>,
}

impl std::fmt::Debug for SettingsModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsModel")
            .field("settings", &self.settings)
            .field("file", &self.file)
            .field("problems", &self.problems)
            .finish_non_exhaustive()
    }
}

/// When the settings let typing, read receipts and presence go ([`SettingsModel::sends_typing`] and the others).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signals {
    /// As their switches say.
    AsSwitched,
    /// As their switches say, once the first run's screen was confirmed (the personal build).
    AfterFirstRun,
    /// Never: the app's read-only mode.
    Never,
}

/// The calls' devices as the settings know them.
struct Devices {
    directory: Arc<dyn DeviceDirectory>,
    list: DeviceList,
}

impl std::fmt::Debug for Devices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Devices")
            .field("list", &self.list)
            .finish_non_exhaustive()
    }
}

impl SettingsModel {
    /// The settings in `file`, with what was wrong with it.
    #[must_use]
    pub fn load(file: SettingsFile) -> Self {
        let Loaded { settings, problems } = file.load();
        let held = problems
            .iter()
            .any(|problem| matches!(problem, Problem::Unreadable { .. }));
        Self {
            settings,
            file: Some(file),
            problems,
            held,
            diagnostics: None,
            copied: None,
            copying: None,
            signals: Signals::AsSwitched,
            video_switch: false,
            devices: None,
            preview: None,
            sounds: None,
            sound_packs: Vec::new(),
        }
    }

    /// Makes the settings' AI providers and "Use AI features" the app's ([`crate::local_ai::Providers`],
    /// [`crate::local_ai::AiSwitch`]): once, when the app starts; each change after that does it by itself.
    pub fn publish_ai(&self, cx: &mut App) {
        crate::local_ai::Providers::publish(&self.settings.ai_providers, cx);
        crate::local_ai::AiSwitch::publish(self.uses_ai(), cx);
    }

    /// Settings kept in memory only: nothing is read or written.
    #[must_use]
    pub fn in_memory(settings: Settings) -> Self {
        Self {
            settings,
            file: None,
            problems: Vec::new(),
            held: false,
            diagnostics: None,
            copied: None,
            copying: None,
            signals: Signals::AsSwitched,
            video_switch: false,
            devices: None,
            preview: None,
            sounds: None,
            sound_packs: Vec::new(),
        }
    }

    /// The settings with the calls' video switch shown ([`Settings::video_calls`]): an app whose calls have video.
    #[must_use]
    pub fn with_video_switch(mut self) -> Self {
        self.video_switch = true;
        self
    }

    /// Whether the view shows the calls' video switch.
    #[must_use]
    pub fn shows_video_switch(&self) -> bool {
        self.video_switch
    }

    /// The devices there are, once the calls' devices are attached ([`attach_devices`]); `None` without (no calls).
    #[must_use]
    pub fn devices(&self) -> Option<&DeviceList> {
        self.devices.as_ref().map(|devices| &devices.list)
    }

    /// Whether call devices are still being discovered, ready, or unavailable for this run.
    #[must_use]
    pub fn media_status(&self) -> DeviceStatus {
        self.devices
            .as_ref()
            .map_or(DeviceStatus::Ready, |devices| devices.directory.status())
    }

    /// Where the background pictures the user added are kept: [`BACKGROUNDS_DIR`] beside the settings file; `None` for
    /// settings kept in memory (nothing can be added).
    #[must_use]
    pub fn backgrounds_dir(&self) -> Option<PathBuf> {
        let file = self.file.as_ref()?;
        Some(file.path().parent()?.join(BACKGROUNDS_DIR))
    }

    /// Where the pictures the user chose for the accounts are kept: [`crate::account_picture::DIR`] beside the settings
    /// file; `None` for settings kept in memory (no picture can be chosen).
    #[must_use]
    pub fn account_pictures_dir(&self) -> Option<PathBuf> {
        let file = self.file.as_ref()?;
        Some(file.path().parent()?.join(crate::account_picture::DIR))
    }

    /// Where the recaps' AI summaries are kept: [`crate::recap_summary::SUMMARY_DIR`] beside the settings file; `None`
    /// for settings kept in memory (summaries kept for the run only).
    #[must_use]
    pub fn summaries_dir(&self) -> Option<PathBuf> {
        let file = self.file.as_ref()?;
        Some(file.path().parent()?.join(crate::recap_summary::SUMMARY_DIR))
    }

    /// Where the sound packs the user imported are kept: [`SOUND_PACKS_DIR`] beside the settings file; `None` for
    /// settings kept in memory (nothing can be imported).
    #[must_use]
    pub fn sound_packs_dir(&self) -> Option<PathBuf> {
        let file = self.file.as_ref()?;
        Some(file.path().parent()?.join(SOUND_PACKS_DIR))
    }

    /// Where the sticker packs the user imported are kept, for every account: [`crate::stickers::store::DIR`] beside
    /// the settings file; `None` for settings kept in memory (nothing can be imported).
    #[must_use]
    pub fn stickers_dir(&self) -> Option<PathBuf> {
        let file = self.file.as_ref()?;
        Some(file.path().parent()?.join(crate::stickers::store::DIR))
    }

    /// What plays the app's sounds, once attached ([`attach_sounds`]); `None` without (no speaker).
    #[must_use]
    pub fn sound_player(&self) -> Option<&Arc<dyn SoundPlayer>> {
        self.sounds.as_ref()
    }

    /// The sound packs the user imported, as last read.
    #[must_use]
    pub fn sound_packs(&self) -> &[PackInfo] {
        &self.sound_packs
    }

    /// Reads the imported sound packs again, off the window's thread; the views are told.
    pub fn reload_sound_packs(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.sound_packs_dir() else {
            return;
        };
        let listing = cx.background_spawn(async move { sounds::pack::list(&dir) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let packs = listing.await;
            let _ = this.update(cx, |model, cx| {
                model.sound_packs = packs;
                cx.notify();
            });
        })
        .detach();
    }

    /// Removes imported pack `id`: forgotten by the settings first, so nothing plays it, its files after.
    pub fn remove_sound_pack(&mut self, id: &str, cx: &mut Context<Self>) {
        let source = SoundSource::Imported(id.to_owned());
        self.change(|settings| settings.sounds.forget_pack(&source), cx);
        self.sound_packs.retain(|pack| pack.id != id);
        cx.notify();
        let Some(dir) = self.sound_packs_dir() else {
            return;
        };
        let id = id.to_owned();
        cx.background_spawn(async move {
            if let Err(error) = sounds::pack::remove(&dir, &id) {
                log::warn!("a sound pack can't be removed: {error}");
            }
        })
        .detach();
    }

    /// Tells the sounds the settings as they are now.
    pub(crate) fn apply_sounds(&self) {
        if let Some(player) = &self.sounds {
            player.apply(&self.settings, self.sound_packs_dir());
        }
    }

    /// The background the camera is sent with, as the settings choose it ([`Settings::video_effect`]).
    #[must_use]
    pub fn background(&self) -> Background {
        video_background(&self.settings.video_effect, self.backgrounds_dir().as_deref())
    }

    /// The camera's preview of the video effects, once attached ([`attach_camera_preview`]); `None` without (no
    /// camera to preview).
    #[must_use]
    pub fn camera_preview(&self) -> Option<&CameraPreview> {
        self.preview.as_ref()
    }

    /// Hands the device directory the choice of each kind the settings hold.
    fn apply_devices(&self) {
        if let Some(devices) = &self.devices {
            for kind in DeviceKind::ALL {
                devices.directory.choose(kind, self.settings.device(kind));
            }
        }
    }

    /// Makes `account` the app's first account, whose own settings are the top-level ones ([`Settings::make_first`]);
    /// an app of several accounts sets it before its views read an account's settings.
    pub fn set_first_account(&mut self, account: &str, cx: &mut Context<Self>) {
        let account = account.to_owned();
        self.change(move |settings| settings.make_first(&account), cx);
    }

    /// Forgets `account`'s own settings, `next` the first account when it was ([`Settings::forget_account`]), and
    /// deletes the picture the user chose for it.
    pub fn forget_account(&mut self, account: &str, next: Option<&str>, cx: &mut Context<Self>) {
        if let Some(dir) = self.account_pictures_dir()
            && let Err(error) = crate::account_picture::AccountPictures::new(dir).reset(account)
        {
            log::warn!("the picture of an account removed can't be deleted: {error}");
        }
        let (account, next) = (account.to_owned(), next.map(str::to_owned));
        self.change(move |settings| settings.forget_account(&account, next.as_deref()), cx);
    }

    /// The settings with the first run's screen asked for (the personal build): until [`Settings::first_run`] is confirmed,
    /// none of typing, read receipts and presence is sent.
    #[must_use]
    pub fn requiring_first_run(mut self) -> Self {
        if self.signals == Signals::AsSwitched {
            self.signals = Signals::AfterFirstRun;
        }
        self
    }

    /// The settings of the app's read-only mode (the personal build's `ngteams --read-only`): typing, read receipts
    /// and presence are never sent whatever their switches say, and no first run's screen is shown (it asks only about
    /// those).
    #[must_use]
    pub fn read_only(mut self) -> Self {
        self.signals = Signals::Never;
        self
    }

    /// Whether these are the read-only mode's settings ([`SettingsModel::read_only`]).
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.signals == Signals::Never
    }

    /// Whether the first run's screen is waiting for the user: asked for, and not confirmed yet (never in the read-only
    /// mode).
    #[must_use]
    pub fn awaiting_first_run(&self) -> bool {
        self.signals == Signals::AfterFirstRun && self.settings.first_run == FirstRunState::Waiting
    }

    /// Whether the AI notice waits for the user: the app asks for the first run's screen, that screen is confirmed, and
    /// the user did not answer "Use AI features" yet (an upgrade from a build without it asks too).
    #[must_use]
    pub fn awaiting_ai_notice(&self) -> bool {
        self.signals == Signals::AfterFirstRun && !self.awaiting_first_run() && self.settings.ai_features.is_none()
    }

    /// Whether the AI features may run now: as "Use AI features" says. Before the user answers it, they run only where
    /// the app does not ask (the views of tests); an app that asks, or the read-only mode, keeps them off.
    #[must_use]
    pub fn uses_ai(&self) -> bool {
        self.settings.ai_features.unwrap_or(self.signals == Signals::AsSwitched)
    }

    /// Turns every AI feature on or off: the AI notice's answer, and the switch in Settings.
    pub fn set_ai_features(&mut self, on: bool, cx: &mut Context<Self>) {
        self.change(|settings| settings.ai_features = Some(on), cx);
    }

    /// Whether typing is sent now: its switch is on, no first run waits, and the app isn't read-only.
    #[must_use]
    pub fn sends_typing(&self) -> bool {
        !self.is_read_only() && !self.awaiting_first_run() && self.settings.send_typing
    }

    /// Whether read receipts are sent now: their switch is on, no first run waits, and the app isn't read-only.
    #[must_use]
    pub fn sends_read_receipts(&self) -> bool {
        !self.is_read_only() && !self.awaiting_first_run() && self.settings.send_read_receipts
    }

    /// Whether the account's own availability is reported now: its switch is on, no first run waits, and the app isn't
    /// read-only.
    #[must_use]
    pub fn sends_presence(&self) -> bool {
        !self.is_read_only() && !self.awaiting_first_run() && self.settings.send_presence
    }

    /// The first run's screen confirmed with the switches as they stand: from now on each is sent as its switch says.
    pub fn confirm_first_run(&mut self, cx: &mut Context<Self>) {
        self.change(|settings| settings.first_run = FirstRunState::Confirmed, cx);
    }

    /// The settings with where "copy diagnostics" reads from; a log that couldn't be opened
    /// ([`DiagnosticsSource::log_problem`]) is shown as a problem.
    #[must_use]
    pub fn with_diagnostics(mut self, source: DiagnosticsSource) -> Self {
        if let Some(why) = &source.log_problem {
            self.problems.push(Problem::LogUnavailable { why: why.clone() });
        }
        self.diagnostics = Some(source);
        self
    }

    /// Where the "copy diagnostics" text comes from, and the level it names when no log is installed.
    fn diagnostics_source(&self) -> (DiagnosticsSource, LogLevel) {
        let source = self.diagnostics.clone().unwrap_or_else(|| DiagnosticsSource {
            app: format!("teams {}", env!("CARGO_PKG_VERSION")),
            log_file: None,
            log_problem: None,
        });
        (source, self.settings.log_level)
    }

    /// The "copy diagnostics" text as it would be copied now (see `teams_log::diagnostics`). It waits for the log's
    /// writer and reads the files: never on GPUI's foreground ([`SettingsModel::copy_diagnostics`] collects it on a
    /// background task).
    #[must_use]
    pub fn diagnostics(&self) -> Report {
        let (source, setting) = self.diagnostics_source();
        source.collect(setting)
    }

    /// Puts the "copy diagnostics" text on the clipboard and keeps what it was for the settings view to tell
    /// ([`SettingsModel::copied`]). The text is collected on a background task (the flush of the log's writer, up to
    /// `teams_log::file::FLUSH_WAIT`, and the files' reads never hold the window); the clipboard is written once it is
    /// ready. A copy asked while one is collected takes its place.
    pub fn copy_diagnostics(&mut self, cx: &mut Context<Self>) {
        let (source, setting) = self.diagnostics_source();
        let collecting = cx.background_spawn(async move { source.collect(setting) });
        self.copying = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let report = collecting.await;
            let _ = this.update(cx, |model, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(report.text));
                model.copied = Some(Copied {
                    has_debug: report.has_debug,
                });
                model.copying = None;
                cx.notify();
            });
        }));
    }

    /// What the last "copy diagnostics" copied; `None` before one.
    #[must_use]
    pub fn copied(&self) -> Option<Copied> {
        self.copied
    }

    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// What was wrong with the file when it was read, and a save that failed since.
    #[must_use]
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// The file the settings are kept in; `None` in memory.
    #[must_use]
    pub fn file(&self) -> Option<&SettingsFile> {
        self.file.as_ref()
    }

    /// Changes the settings and, when they changed, writes them to the file and tells the views. A save that fails, or
    /// one held back because the file couldn't be read (it would replace what the user wrote), is kept as a problem
    /// (the latest one only) and the change holds in memory.
    pub fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let before = self.settings.clone();
        change(&mut self.settings);
        if self.settings == before {
            return;
        }
        // the same value changes nothing; a model that never published (a view test) publishes it now
        crate::local_ai::AiSwitch::publish(self.uses_ai(), cx);
        if self.settings.log_level != before.log_level {
            // no log installed (a view test): nothing to tell
            let _ = teams_log::apply_setting(self.settings.log_level);
        }
        if DeviceKind::ALL
            .iter()
            .any(|kind| self.settings.device(*kind) != before.device(*kind))
        {
            self.apply_devices();
        }
        if self.settings.video_effect != before.video_effect
            && let Some(preview) = &self.preview
        {
            preview.set_background(self.background());
        }
        self.apply_sounds();
        if self.settings.ai_providers != before.ai_providers {
            crate::local_ai::Providers::publish(&self.settings.ai_providers, cx);
        }
        self.problems
            .retain(|problem| !matches!(problem, Problem::Unsaved { .. }));
        if let Some(file) = &self.file {
            let saved = if self.held {
                Err(Problem::Unsaved {
                    path: file.shown(),
                    why: "it couldn't be read, so it is left as it is".to_owned(),
                })
            } else {
                file.save(&self.settings)
            };
            if let Err(problem) = saved {
                self.problems.push(problem);
            }
        }
        cx.notify();
    }
}

/// Attaches the camera's preview of the video effects to the settings: the views offer it, and it applies the
/// background the settings choose, now and on every change.
pub fn attach_camera_preview(model: &Entity<SettingsModel>, preview: CameraPreview, cx: &mut App) {
    model.update(cx, |model, cx| {
        preview.set_background(model.background());
        model.preview = Some(preview);
        cx.notify();
    });
}

/// Attaches what plays the app's sounds to the settings: it follows them now and on every change, and the views offer
/// a preview, told when one ends.
pub fn attach_sounds(model: &Entity<SettingsModel>, player: Arc<dyn SoundPlayer>, cx: &mut App) {
    let (told, ended) = async_channel::unbounded::<()>();
    player.watch_previewing(Box::new(move || {
        // the settings gone: nobody to tell
        let _ = told.try_send(());
    }));
    model.update(cx, |model, cx| {
        model.sounds = Some(player);
        model.apply_sounds();
        model.reload_sound_packs(cx);
        cx.notify();
    });
    let weak = model.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while ended.recv().await.is_ok() {
            if weak.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }
    })
    .detach();
}

/// Attaches the calls' devices to the settings (see the module docs): the list there is now, each change of it as it
/// comes, and each kind's choice handed to `directory` now and on every change.
pub fn attach_devices(model: &Entity<SettingsModel>, directory: Arc<dyn DeviceDirectory>, cx: &mut App) {
    let (told, lists) = async_channel::unbounded::<DeviceList>();
    directory.watch(Box::new(move |list: &DeviceList| {
        // the settings gone: nobody to tell
        let _ = told.try_send(list.clone());
    }));
    let list = directory.devices();
    model.update(cx, |model, cx| {
        model.devices = Some(Devices { directory, list });
        model.apply_devices();
        cx.notify();
    });
    let weak = model.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(list) = lists.recv().await {
            let kept = weak.update(cx, |model, cx| {
                if let Some(devices) = &mut model.devices {
                    devices.list = list;
                    cx.notify();
                }
            });
            if kept.is_err() {
                break;
            }
        }
    })
    .detach();
}
