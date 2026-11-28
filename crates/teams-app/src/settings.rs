//! The app's settings and their TOML file in the app's own data directory, without a UI: the desktop's GPUI model
//! (`teams-ui`'s `settings::SettingsModel`) and the phone app read and write the same file through this module.
//!
//! - **What**: the chat shown last ([`Settings::selected_chat`]), whether typing, read receipts and the account's own
//!   availability are sent, which messages notify ([`Notifications`]) and whether a meeting's start does
//!   ([`Settings::meeting_started`]), the idle time after which the account's availability stops being reported, the
//!   account's manual status ([`ManualStatus`]), whether calls carry video ([`Settings::video_calls`]), the calls'
//!   microphone, speaker and camera ([`Settings::microphone`] and the others: a device's name, or the system's default
//!   when none is named), the camera's background in calls ([`Settings::video_effect`]), how the app looks
//!   ([`Settings::appearance`]: the theme, following the system's light or dark mode or not, the font size and
//!   family), the clock times are shown on ([`Settings::time_format`]), the sounds ([`sounds`]), the noise
//!   suppression of each account's calls ([`noise`]), the local AI choices, and whether the first run's screen of the
//!   writes that happen on their own was confirmed ([`Settings::first_run`]).
//! - **Forgiving to read**: a missing file gives the defaults; a file that can't be read or isn't TOML gives the
//!   defaults and is reported, and is never written over: changes hold in memory until the app quits, so the file
//!   is there to mend by hand; a value that is bad (of the wrong type, out of range, a name this build doesn't know)
//!   is reported and that one setting keeps its default, the others are taken; a key that names no setting is
//!   reported and ignored. Every problem is a [`Problem`], shown by the settings view.
//! - **Private to write**: the file is `0600` and its directory `0700`, written whole to a temporary file beside it
//!   and renamed over it, so a crash leaves the old file or the new one. A file found readable by others is tightened
//!   when it is read.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use teams_core::Choice;
pub use teams_log::Level as LogLevel;
use teams_media_types::{DeviceKind, NoiseSuppression, stock_background};
use teams_proto::ManualAvailability;
use teams_theme::Appearance;
use toml::{Table, Value};

use crate::ai::providers::{self, Provider};
use crate::ai::{AiChoice, Model, ToolChoice, Work};
use crate::languages;

pub mod captions_height;
mod message_bar;
pub mod noise;
pub mod sounds;
mod toolbar;

pub use message_bar::{BarChoice, BarItem};
use sounds::SoundSettings;
pub use toolbar::ComposeTool;

/// The idle times the settings take, in minutes.
pub const IDLE_MINUTES: RangeInclusive<u32> = 1..=240;
/// The idle time by default, in minutes.
pub const DEFAULT_IDLE_MINUTES: u32 = 5;
/// The longest chat id kept as the selected chat.
pub const MAX_CHAT_ID: usize = 512;
/// The largest settings file read; a larger one is unreadable.
pub const MAX_FILE: u64 = 64 * 1024;
/// The longest device name kept as a choice.
pub const MAX_DEVICE_NAME: usize = 256;
/// The longest name of a background picture the user added, kept as a choice.
pub const MAX_PICTURE_NAME: usize = 64;
/// The directory beside the settings file the user's background pictures are kept in.
pub const BACKGROUNDS_DIR: &str = "backgrounds";
/// The directory beside the settings file the sound packs the user imported are kept in.
pub const SOUND_PACKS_DIR: &str = "sound-packs";

/// The camera's background in calls, as Teams' video effects offer it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VideoEffect {
    /// The camera's picture as it is.
    #[default]
    None,
    /// The background blurred.
    Blur,
    /// One of the app's pictures, by its key ([`teams_media_types::STOCK_BACKGROUNDS`]).
    Stock(&'static str),
    /// A picture the user added, by its file's name in the [`BACKGROUNDS_DIR`] beside the settings file.
    Added(String),
}

impl VideoEffect {
    /// Its name in the file: `none`, `blur`, `stock/<key>` or `added/<file name>`.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::None => "none".to_owned(),
            Self::Blur => "blur".to_owned(),
            Self::Stock(key) => format!("stock/{key}"),
            Self::Added(name) => format!("added/{name}"),
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "none" => Some(Self::None),
            "blur" => Some(Self::Blur),
            _ => {
                if let Some(stock) = key.strip_prefix("stock/") {
                    return stock_background(stock).map(Self::Stock);
                }
                key.strip_prefix("added/")
                    .filter(|name| is_picture_name(name))
                    .map(|name| Self::Added(name.to_owned()))
            }
        }
    }
}

/// Whether `name` is a picture's file name as the settings keep one: 1 to [`MAX_PICTURE_NAME`] ASCII letters, digits,
/// dots, dashes and underscores, not starting with a dot.
#[must_use]
pub fn is_picture_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_PICTURE_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Which messages notify.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Notifications {
    /// Every message from someone else in a chat that isn't shown.
    #[default]
    All,
    /// Only the messages that mention the account.
    Mentions,
    Off,
}

impl Notifications {
    pub const ALL: [Self; 3] = [Self::All, Self::Mentions, Self::Off];

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mentions => "mentions",
            Self::Off => "off",
        }
    }

    /// What the settings view calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All messages",
            Self::Mentions => "Mentions only",
            Self::Off => "Off",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.key() == key)
    }
}

/// How a time of day is shown (`clock`): on the clock the system's locale uses, or on one the user picks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeFormat {
    /// The system's locale decides (`clock::system_cycle`).
    #[default]
    System,
    /// "3:30 PM".
    Twelve,
    /// "15:30".
    TwentyFour,
}

impl TimeFormat {
    pub const ALL: [Self; 3] = [Self::System, Self::Twelve, Self::TwentyFour];

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Twelve => "12-hour",
            Self::TwentyFour => "24-hour",
        }
    }

    /// What the settings view calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Follow the system",
            Self::Twelve => "12-hour",
            Self::TwentyFour => "24-hour",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|format| format.key() == key)
    }
}

/// The account's status as the user sets it (`own_presence`, M2 Task 20): [`ManualStatus::Automatic`] follows
/// the app; the others are the manual statuses a capture showed (M2 Task 18), each sent as it is chosen and overriding
/// the app's until Automatic is chosen again (the reset). "Be right back" and "Appear offline" were never captured, so
/// they aren't offered: a file naming one is reported and Automatic kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ManualStatus {
    /// Available while the window is used; nothing reported once it hasn't been for the idle time.
    #[default]
    Automatic,
    Available,
    Busy,
    DoNotDisturb,
    Away,
}

impl ManualStatus {
    pub const ALL: [Self; 5] = [
        Self::Automatic,
        Self::Available,
        Self::Busy,
        Self::DoNotDisturb,
        Self::Away,
    ];

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Available => "available",
            Self::Busy => "busy",
            Self::DoNotDisturb => "do-not-disturb",
            Self::Away => "away",
        }
    }

    /// What the settings view and the account menu call it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Available => "Available",
            Self::Busy => "Busy",
            Self::DoNotDisturb => "Do not disturb",
            Self::Away => "Appear away",
        }
    }

    /// The own-presence policy's choice it is.
    #[must_use]
    pub fn choice(self) -> Choice {
        match self {
            Self::Automatic => Choice::Automatic,
            Self::Available => Choice::Manual(ManualAvailability::Available),
            Self::Busy => Choice::Manual(ManualAvailability::Busy),
            Self::DoNotDisturb => Choice::Manual(ManualAvailability::DoNotDisturb),
            Self::Away => Choice::Manual(ManualAvailability::Away),
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.key() == key)
    }
}

/// Whether the first run's screen was confirmed ([`Settings::first_run`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FirstRunState {
    /// Not yet: where the app asks for the screen, typing, read receipts and presence aren't sent.
    #[default]
    Waiting,
    Confirmed,
}

impl FirstRunState {
    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Confirmed => "confirmed",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        [Self::Waiting, Self::Confirmed]
            .into_iter()
            .find(|state| state.key() == key)
    }
}

/// The settings (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a switch of its own the user turns in the settings, stored under its own key"
)]
pub struct Settings {
    /// The chat shown last, opened again at the start.
    pub selected_chat: Option<String>,
    /// Typing is sent while the user types (Task 13).
    pub send_typing: bool,
    /// Read receipts are sent as a chat is read (Task 13).
    pub send_read_receipts: bool,
    /// The account's own availability is reported, following the app or a manual status (Task 20).
    pub send_presence: bool,
    /// Whether the first run's screen of the writes that happen on their own (typing, read receipts, presence) was
    /// confirmed; read only where the app asks for it (`SettingsModel::requiring_first_run`).
    pub first_run: FirstRunState,
    pub notifications: Notifications,
    /// A meeting someone else started where the account is invited, or a group chat's meeting, shows a banner with
    /// Join (Teams' "Meeting started notification", on by default; `meeting_alert`).
    pub meeting_started: bool,
    /// A calendar meeting shows a reminder before it starts, at its reminder time, and again as it starts
    /// (`meeting_reminder`); on by default.
    pub meeting_reminders: bool,
    /// Minutes without use after which the account's availability stops being reported (Task 20,
    /// `own_presence`), within [`IDLE_MINUTES`].
    pub idle_minutes: u32,
    pub manual_status: ManualStatus,
    /// The diagnostic log's level (`teams-log`), changed live; `TEAMS_APP_LOG` overrides it.
    pub log_level: LogLevel,
    /// Calls carry video ("Video in calls"); off, a call's media starts no video and the codec isn't used.
    pub video_calls: bool,
    /// Every call's live captions are turned on and saved on this computer as its transcript ("Save live captions of
    /// every call", `call_transcript`); off by default. Teams and the call's other people see nothing of it.
    pub save_call_captions: bool,
    /// The calls' microphone by its device name; `None` follows the system's default.
    pub microphone: Option<String>,
    /// The calls' speaker (and the ringtone's); `None` follows the system's default.
    pub speaker: Option<String>,
    /// The calls' camera; `None` follows the system's default.
    pub camera: Option<String>,
    /// The camera's background in calls, the app's for every account.
    pub video_effect: VideoEffect,
    /// How the app looks: its theme, font size and font family, the app's for every account.
    pub appearance: Appearance,
    /// The clock times are shown on, the app's for every account.
    pub time_format: TimeFormat,
    /// The language live captions are translated into on this computer (`translate`), by its code in
    /// [`crate::languages::TARGET_LANGUAGES`]; `None`: not translated (the default: nothing goes to a tool until the
    /// user picks one).
    pub caption_translate_to: Option<&'static str>,
    /// Which tool, and model, translates them.
    pub caption_translator: AiChoice,
    /// Which tool, and model, makes a meeting recap's AI summary (`recap_summary`), when the user asks for one.
    pub recap_summarizer: AiChoice,
    /// The other accounts' own settings, by the account's MRI: the first account's are [`Settings::selected_chat`]
    /// and [`Settings::manual_status`] themselves (see [`Settings::first_account`]).
    pub accounts: BTreeMap<String, AccountSettings>,
    /// The app's first account, whose own settings are the top-level ones (an app of one account: its only one), as
    /// the app last set it (`SettingsModel::set_first_account`) and the file keeps it. `None` (a file of a build of
    /// one account): every account is taken for the first, and the first one the app sets takes the top-level ones.
    pub first_account: Option<String>,
    /// Other people's calendars each account added to its calendar (its Add calendar), in the order added.
    pub people_calendars: Vec<PersonCalendar>,
    /// The language each account reads a chat in (`chat_translate`), for the chats it chose one for.
    pub chat_languages: Vec<ChatLanguage>,
    /// Which tool, and model, translates chats.
    pub chat_translator: AiChoice,
    /// Which tool, and model, proofreads a draft when the user asks (`message_correct`).
    pub message_corrector: AiChoice,
    /// The language a message is translated into for reading (a message's "Translate", Teams' "Translate messages into
    /// this language", `chat_translate`): the app's for every chat and account, by its name (one of
    /// [`crate::languages::TARGET_LANGUAGES`]' or any the user typed). A chat read in a language of its own
    /// ([`Settings::chat_languages`]) uses that one.
    pub message_translate_to: String,
    /// The language a draft is translated into in its box before it is sent (`draft_translate`): the app's
    /// for every box and account, by its name. It is apart from [`Settings::message_translate_to`], so that the user
    /// can read in one language and write in another.
    pub draft_translate_to: String,
    /// Which tool, and model, translates a draft in its box.
    pub message_translator: AiChoice,
    /// The AI providers the user set, in the user's order (`ai::providers`); the command-line tools
    /// found on this computer that it does not hold come after them.
    pub ai_providers: Vec<Provider>,
    /// "Use AI features": `Some(false)` turns every AI feature off (`local_ai::enabled`). `None`: the user
    /// was not asked yet, and the app shows the AI notice first (`SettingsModel::awaiting_ai_notice`).
    pub ai_features: Option<bool>,
    /// Teams' own buttons that the user hid from the message box's toolbar, in the toolbar's order ([`toolbar`]).
    pub compose_hidden: Vec<ComposeTool>,
    /// The AI tools that the user shows on the message box's toolbar, in the toolbar's order ([`toolbar`]).
    pub compose_shown: Vec<ComposeTool>,
    /// The buttons each account chose for a message's hover bar, by the account's MRI ([`message_bar`]); an account
    /// with none shows the bar of Teams.
    pub message_bar: BTreeMap<String, BarChoice>,
    /// The noise suppression level of each account's calls, by the account's MRI ([`noise`]); an account that isn't
    /// here has Auto.
    pub noise_suppression: BTreeMap<String, NoiseSuppression>,
    /// The height of each account's live captions area in calls, px, by the account's MRI ([`captions_height`]); an
    /// account that isn't here has [`captions_height::DEFAULT`].
    pub caption_heights: BTreeMap<String, u32>,
    /// Which sound plays for each event, how loud, and the pack they come from ([`sounds`]).
    pub sounds: SoundSettings,
}

/// The language an account reads a chat in, translated on this computer (`chat_translate`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLanguage {
    /// The account, by its MRI.
    pub account: String,
    /// The chat (or channel, or a channel post's thread), by its id.
    pub chat: String,
    /// The language, by its code in [`crate::languages::TARGET_LANGUAGES`].
    pub language: &'static str,
}

/// Another person's calendar an account added to its calendar (Add calendar, "People's calendars").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonCalendar {
    /// The account that added it, by its MRI.
    pub account: String,
    /// The person's address.
    pub address: String,
    /// Their name, as the directory gave it.
    pub name: String,
}

/// What each account of an app of several keeps of its own: the chat shown last and its manual status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountSettings {
    pub selected_chat: Option<String>,
    pub manual_status: ManualStatus,
}

impl Settings {
    /// Whether the message box's toolbar shows `tool`.
    #[must_use]
    pub fn shows_compose_tool(&self, tool: ComposeTool) -> bool {
        if tool.shown_by_default() {
            !self.compose_hidden.contains(&tool)
        } else {
            self.compose_shown.contains(&tool)
        }
    }

    /// Shows `tool` on the message box's toolbar, or hides it there.
    pub fn show_compose_tool(&mut self, tool: ComposeTool, shown: bool) {
        let (list, kept) = if tool.shown_by_default() {
            (&mut self.compose_hidden, !shown)
        } else {
            (&mut self.compose_shown, shown)
        };
        list.retain(|other| *other != tool);
        if kept {
            list.push(tool);
            toolbar::tidy(list);
        }
    }

    /// Whether `account`'s message hover bar shows `item`.
    #[must_use]
    pub fn shows_bar_item(&self, account: &str, item: BarItem) -> bool {
        self.message_bar
            .get(account)
            .map_or_else(|| item.shown_by_default(), |choice| choice.shows(item))
    }

    /// Shows `item` on `account`'s message hover bar, or hides it in "More options".
    pub fn show_bar_item(&mut self, account: &str, item: BarItem, shown: bool) {
        let choice = self.message_bar.entry(account.to_owned()).or_default();
        choice.show(item, shown);
        if *choice == BarChoice::default() {
            self.message_bar.remove(account);
        }
    }

    /// The noise suppression level of `account`'s calls.
    #[must_use]
    pub fn noise_suppression_of(&self, account: &str) -> NoiseSuppression {
        self.noise_suppression.get(account).copied().unwrap_or_default()
    }

    /// Sets the noise suppression level of `account`'s calls.
    pub fn set_noise_suppression(&mut self, account: &str, level: NoiseSuppression) {
        if level == NoiseSuppression::default() {
            self.noise_suppression.remove(account);
        } else {
            self.noise_suppression.insert(account.to_owned(), level);
        }
    }

    /// The height of `account`'s live captions area in calls, px.
    #[must_use]
    pub fn caption_height_of(&self, account: &str) -> u32 {
        self.caption_heights
            .get(account)
            .copied()
            .unwrap_or(captions_height::DEFAULT)
    }

    /// Sets the height of `account`'s live captions area in calls, kept inside its limits.
    pub fn set_caption_height(&mut self, account: &str, height: u32) {
        let height = captions_height::clamp(height);
        if height == captions_height::DEFAULT {
            self.caption_heights.remove(account);
        } else {
            self.caption_heights.insert(account.to_owned(), height);
        }
    }

    /// Whether `account` is the first account, whose own settings are the top-level ones.
    fn is_first(&self, account: &str) -> bool {
        self.first_account.as_deref().is_none_or(|first| first == account)
    }

    /// The chat `account` showed last.
    #[must_use]
    pub fn selected_chat_of(&self, account: &str) -> Option<&str> {
        if self.is_first(account) {
            self.selected_chat.as_deref()
        } else {
            self.accounts.get(account)?.selected_chat.as_deref()
        }
    }

    /// Keeps `chat` as the chat `account` showed last; `None` keeps none.
    pub fn set_selected_chat(&mut self, account: &str, chat: Option<String>) {
        if self.is_first(account) {
            self.selected_chat = chat;
        } else {
            self.accounts.entry(account.to_owned()).or_default().selected_chat = chat;
            self.accounts.retain(|_, kept| *kept != AccountSettings::default());
        }
    }

    /// `account`'s manual status.
    #[must_use]
    pub fn manual_status_of(&self, account: &str) -> ManualStatus {
        if self.is_first(account) {
            self.manual_status
        } else {
            self.accounts
                .get(account)
                .map_or_else(ManualStatus::default, |kept| kept.manual_status)
        }
    }

    /// Sets `account`'s manual status.
    pub fn set_manual_status(&mut self, account: &str, status: ManualStatus) {
        if self.is_first(account) {
            self.manual_status = status;
        } else {
            self.accounts.entry(account.to_owned()).or_default().manual_status = status;
            self.accounts.retain(|_, kept| *kept != AccountSettings::default());
        }
    }

    /// Makes `account` the first account: its own settings become the top-level ones, and the first account's before
    /// it are kept under its MRI. With no first account yet (a file of one account), the top-level ones are taken for
    /// `account`'s.
    pub fn make_first(&mut self, account: &str) {
        if self.first_account.as_deref() == Some(account) {
            return;
        }
        // a file of one account: its top-level ones are this account's
        if self.first_account.is_none() {
            self.accounts.remove(account);
            self.first_account = Some(account.to_owned());
            return;
        }
        let own = self.accounts.remove(account).unwrap_or_default();
        let before = AccountSettings {
            selected_chat: std::mem::replace(&mut self.selected_chat, own.selected_chat),
            manual_status: std::mem::replace(&mut self.manual_status, own.manual_status),
        };
        if let Some(first) = self.first_account.replace(account.to_owned())
            && before != AccountSettings::default()
        {
            self.accounts.insert(first, before);
        }
    }

    /// The language `account` reads `chat` in; `None`: as it is written.
    #[must_use]
    pub fn chat_language_of(&self, account: &str, chat: &str) -> Option<&'static str> {
        self.chat_languages
            .iter()
            .find(|kept| kept.account == account && kept.chat == chat)
            .map(|kept| kept.language)
    }

    /// Sets the language `account` reads `chat` in; `None` reads it as it is written.
    pub fn set_chat_language(&mut self, account: &str, chat: &str, language: Option<&'static str>) {
        self.chat_languages
            .retain(|kept| !(kept.account == account && kept.chat == chat));
        if let Some(language) = language {
            self.chat_languages.push(ChatLanguage {
                account: account.to_owned(),
                chat: chat.to_owned(),
                language,
            });
        }
    }

    /// Forgets `account`'s own settings (an account removed): when it was the first, `next` (the app's next account)
    /// takes the first's place first, so nothing of it stays at the top level.
    pub fn forget_account(&mut self, account: &str, next: Option<&str>) {
        if self.first_account.as_deref() == Some(account) {
            if let Some(next) = next {
                self.make_first(next);
            } else {
                self.selected_chat = None;
                self.manual_status = ManualStatus::default();
                self.first_account = None;
            }
        }
        self.accounts.remove(account);
        self.noise_suppression.remove(account);
        self.caption_heights.remove(account);
        self.chat_languages.retain(|kept| kept.account != account);
    }

    /// The device chosen for `kind`; `None` follows the system.
    #[must_use]
    pub fn device(&self, kind: DeviceKind) -> Option<&str> {
        match kind {
            DeviceKind::Microphone => self.microphone.as_deref(),
            DeviceKind::Speaker => self.speaker.as_deref(),
            DeviceKind::Camera => self.camera.as_deref(),
        }
    }

    /// Chooses `name` for `kind`; `None` follows the system.
    pub fn set_device(&mut self, kind: DeviceKind, name: Option<String>) {
        let slot = match kind {
            DeviceKind::Microphone => &mut self.microphone,
            DeviceKind::Speaker => &mut self.speaker,
            DeviceKind::Camera => &mut self.camera,
        };
        *slot = name;
    }
}

impl Settings {
    /// The local AI choice of `work`: the captions' translator, the recaps' summarizer, the chats' translator, the
    /// drafts' proofreader or their translator.
    #[must_use]
    pub fn ai(&self, work: Work) -> &AiChoice {
        match work {
            Work::Captions => &self.caption_translator,
            Work::Summary => &self.recap_summarizer,
            Work::Chat => &self.chat_translator,
            Work::Correction => &self.message_corrector,
            Work::Draft => &self.message_translator,
        }
    }

    /// Sets the local AI choice of `work`.
    pub fn set_ai(&mut self, work: Work, choice: AiChoice) {
        match work {
            Work::Captions => self.caption_translator = choice,
            Work::Summary => self.recap_summarizer = choice,
            Work::Chat => self.chat_translator = choice,
            Work::Correction => self.message_corrector = choice,
            Work::Draft => self.message_translator = choice,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            selected_chat: None,
            send_typing: true,
            send_read_receipts: true,
            send_presence: true,
            first_run: FirstRunState::Waiting,
            notifications: Notifications::default(),
            meeting_started: true,
            meeting_reminders: true,
            idle_minutes: DEFAULT_IDLE_MINUTES,
            manual_status: ManualStatus::default(),
            log_level: LogLevel::default(),
            video_calls: true,
            save_call_captions: false,
            microphone: None,
            speaker: None,
            camera: None,
            video_effect: VideoEffect::None,
            appearance: Appearance::default(),
            time_format: TimeFormat::default(),
            caption_translate_to: None,
            caption_translator: AiChoice::default(),
            recap_summarizer: AiChoice::default(),
            accounts: BTreeMap::new(),
            first_account: None,
            people_calendars: Vec::new(),
            chat_languages: Vec::new(),
            chat_translator: AiChoice::default(),
            message_corrector: AiChoice::default(),
            message_translate_to: languages::DEFAULT_LANGUAGE.to_owned(),
            draft_translate_to: languages::DEFAULT_LANGUAGE.to_owned(),
            message_translator: AiChoice::default(),
            ai_providers: Vec::new(),
            ai_features: None,
            compose_hidden: Vec::new(),
            compose_shown: Vec::new(),
            message_bar: BTreeMap::new(),
            noise_suppression: BTreeMap::new(),
            caption_heights: BTreeMap::new(),
            sounds: SoundSettings::default(),
        }
    }
}

/// The settings' keys in the file.
pub mod keys {
    pub const SELECTED_CHAT: &str = "selected_chat";
    pub const SEND_TYPING: &str = "send_typing";
    pub const SEND_READ_RECEIPTS: &str = "send_read_receipts";
    pub const SEND_PRESENCE: &str = "send_presence";
    pub const FIRST_RUN: &str = "first_run";
    /// "Use AI features", `true` or `false`; missing: not asked yet.
    pub const AI_FEATURES: &str = "ai_features";
    pub const NOTIFICATIONS: &str = "notifications";
    pub const MEETING_STARTED: &str = "meeting_started_notification";
    pub const MEETING_REMINDERS: &str = "meeting_reminders";
    pub const IDLE_MINUTES: &str = "idle_minutes";
    pub const MANUAL_STATUS: &str = "manual_status";
    pub const LOG_LEVEL: &str = "log_level";
    pub const VIDEO_CALLS: &str = "video_calls";
    pub const SAVE_CALL_CAPTIONS: &str = "save_call_captions";
    pub const MICROPHONE: &str = "microphone";
    pub const SPEAKER: &str = "speaker";
    pub const CAMERA: &str = "camera";
    pub const VIDEO_EFFECT: &str = "video_effect";
    pub const FOLLOW_SYSTEM_THEME: &str = "follow_system_theme";
    pub const THEME: &str = "theme";
    pub const LIGHT_THEME: &str = "light_theme";
    pub const DARK_THEME: &str = "dark_theme";
    pub const FONT_SIZE: &str = "font_size";
    pub const FONT_FAMILY: &str = "font_family";
    pub const ANIMATIONS: &str = "animations";
    pub const TIME_FORMAT: &str = "time_format";
    pub const CAPTION_TRANSLATE_TO: &str = "caption_translate_to";
    /// A local AI choice: a table of `tool` (`automatic`, `claude`, `codex`) and `model` (`recommended`, `default` or
    /// a model's name); a file of an earlier build holds the tool alone, as a string, its model the recommended one.
    pub const CAPTION_TRANSLATOR: &str = "caption_translator";
    pub const RECAP_SUMMARIZER: &str = "recap_summarizer";
    /// A local AI choice's tool and model, in its table.
    pub const AI_TOOL: &str = "tool";
    pub const AI_MODEL: &str = "model";
    /// The other accounts' own settings: a table of tables, by the account's MRI.
    pub const ACCOUNTS: &str = "accounts";
    /// The account whose own settings are the top-level ones, by its MRI.
    pub const FIRST_ACCOUNT: &str = "first_account";
    /// Other people's calendars the accounts added: an array of tables (`account`, `address`, `name`).
    pub const PEOPLE_CALENDARS: &str = "people_calendars";
    /// The languages chats are read in: an array of tables (`account`, `chat`, `language`).
    pub const CHAT_LANGUAGES: &str = "chat_languages";
    pub const CHAT_TRANSLATOR: &str = "chat_translator";
    pub const MESSAGE_CORRECTOR: &str = "message_corrector";
    /// The language a message is translated into for reading, by its name. A file of an earlier build had only this
    /// one language, for drafts too: a file without [`DRAFT_TRANSLATE_TO`] takes it for drafts as well.
    pub const MESSAGE_TRANSLATE_TO: &str = "message_translate_to";
    /// The language a draft is translated into in its box, by its name.
    pub const DRAFT_TRANSLATE_TO: &str = "draft_translate_to";
    pub const MESSAGE_TRANSLATOR: &str = "message_translator";
    /// Teams' own buttons hidden from the message box's toolbar: an array of their names.
    pub const COMPOSE_HIDDEN: &str = "compose_hidden_tools";
    /// The AI tools shown on the message box's toolbar: an array of their names.
    pub const COMPOSE_SHOWN: &str = "compose_shown_tools";
    /// The buttons each account hid from, and showed on, a message's hover bar: a table of tables, by the account's
    /// MRI, each with `hidden` and `shown` arrays of names.
    pub const MESSAGE_BAR: &str = "message_bar";
    /// The noise suppression level of each account's calls: a table of level names, by the account's MRI.
    pub const NOISE_SUPPRESSION: &str = "noise_suppression";
    /// The height of each account's live captions area: a table of px, by the account's MRI.
    pub const CAPTION_HEIGHT: &str = "caption_height";
    /// The sound pack chosen as a whole (`builtin/<key>` or `added/<id>`), "Play sounds when I'm busy", every sound's
    /// volume, and each event's sound: a table of tables, by the event's name.
    pub const SOUND_PACK: &str = "sound_pack";
    pub const SOUNDS_WHILE_BUSY: &str = "sounds_while_busy";
    pub const SOUNDS_VOLUME: &str = "sounds_volume";
    pub const SOUNDS: &str = "sounds";
}

/// What went wrong with the settings file: the defaults are used, or the one setting's default.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Problem {
    #[error("the settings file {path} can't be read ({why}): the defaults are used")]
    Unreadable { path: String, why: String },
    #[error("the settings file's `{key}` is {why}: its default is kept")]
    BadValue { key: &'static str, why: &'static str },
    #[error("the settings file's `{key}` isn't a setting: it is ignored")]
    UnknownKey { key: String },
    #[error("the settings can't be saved to {path} ({why}): they hold until the app quits")]
    Unsaved { path: String, why: String },
    /// The diagnostic log couldn't be opened: the app runs without it.
    #[error("{why}")]
    LogUnavailable { why: String },
}

/// Reads settings from a file's text (see the module docs): every problem found, each one's setting at its default.
///
/// # Errors
/// The text isn't TOML: why, in a few words.
pub fn parse(text: &str) -> Result<(Settings, Vec<Problem>), &'static str> {
    let table: Table = text.parse().map_err(|_| "it isn't valid TOML")?;
    let mut settings = Settings::default();
    let mut problems = Vec::new();
    for (key, value) in &table {
        if let Err(problem) = take(&mut settings, key, value) {
            problems.push(problem);
        }
    }
    if !table.contains_key(keys::DRAFT_TRANSLATE_TO) {
        settings.draft_translate_to.clone_from(&settings.message_translate_to);
    }
    Ok((settings, problems))
}

/// An account's own setting of its calls: its noise suppression or its captions' height.
fn take_calls(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    if key == keys::CAPTION_HEIGHT {
        captions_height::take(&mut settings.caption_heights, value)
    } else {
        noise::take(&mut settings.noise_suppression, value)
    }
}

/// Takes one key's value into `settings`.
fn take(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    let bad = |key: &'static str, why: &'static str| Problem::BadValue { key, why };
    match key {
        keys::SELECTED_CHAT => {
            settings.selected_chat = Some(chat_id(value).ok_or(bad(keys::SELECTED_CHAT, "not a chat id"))?);
        }
        keys::SEND_TYPING => {
            settings.send_typing = value.as_bool().ok_or(bad(keys::SEND_TYPING, "not true or false"))?;
        }
        keys::SEND_READ_RECEIPTS => {
            settings.send_read_receipts = value
                .as_bool()
                .ok_or(bad(keys::SEND_READ_RECEIPTS, "not true or false"))?;
        }
        keys::SEND_PRESENCE => {
            settings.send_presence = value.as_bool().ok_or(bad(keys::SEND_PRESENCE, "not true or false"))?;
        }
        keys::FIRST_RUN => {
            settings.first_run = value
                .as_str()
                .and_then(FirstRunState::from_key)
                .ok_or(bad(keys::FIRST_RUN, "not one of waiting, confirmed"))?;
        }
        keys::NOTIFICATIONS => {
            settings.notifications = value
                .as_str()
                .and_then(Notifications::from_key)
                .ok_or(bad(keys::NOTIFICATIONS, "not one of all, mentions, off"))?;
        }
        keys::IDLE_MINUTES => {
            settings.idle_minutes = value
                .as_integer()
                .and_then(|minutes| u32::try_from(minutes).ok())
                .filter(|minutes| IDLE_MINUTES.contains(minutes))
                .ok_or(bad(keys::IDLE_MINUTES, "not a whole number of minutes from 1 to 240"))?;
        }
        keys::MANUAL_STATUS => {
            settings.manual_status = value
                .as_str()
                .and_then(ManualStatus::from_key)
                .ok_or(bad(keys::MANUAL_STATUS, "not a status this app knows"))?;
        }
        keys::LOG_LEVEL => {
            settings.log_level = value
                .as_str()
                .and_then(LogLevel::parse)
                .ok_or(bad(keys::LOG_LEVEL, "not one of error, warn, info, debug"))?;
        }
        key if SWITCHES.contains(&key) => take_switch(settings, key, value)?,
        keys::MICROPHONE | keys::SPEAKER | keys::CAMERA => take_device(settings, key, value)?,
        keys::VIDEO_EFFECT => take_video_effect(settings, value)?,
        keys::FOLLOW_SYSTEM_THEME
        | keys::THEME
        | keys::LIGHT_THEME
        | keys::DARK_THEME
        | keys::FONT_SIZE
        | keys::FONT_FAMILY
        | keys::ANIMATIONS => take_appearance(&mut settings.appearance, key, value)?,
        keys::TIME_FORMAT => settings.time_format = take_time_format(value)?,
        keys::CAPTION_TRANSLATE_TO | keys::CAPTION_TRANSLATOR | keys::RECAP_SUMMARIZER => {
            take_captions(settings, key, value)?;
        }
        providers::FILE_KEY => take_providers(settings, value)?,
        keys::SOUND_PACK | keys::SOUNDS_WHILE_BUSY | keys::SOUNDS_VOLUME | keys::SOUNDS => {
            sounds::take(&mut settings.sounds, key, value)?;
        }
        keys::ACCOUNTS => take_accounts(settings, value)?,
        keys::PEOPLE_CALENDARS => take_people_calendars(settings, value)?,
        key if TRANSLATIONS.contains(&key) => take_translation(settings, key, value)?,
        keys::COMPOSE_HIDDEN => settings.compose_hidden = toolbar::take(keys::COMPOSE_HIDDEN, false, value)?,
        keys::COMPOSE_SHOWN => settings.compose_shown = toolbar::take(keys::COMPOSE_SHOWN, true, value)?,
        keys::MESSAGE_BAR => message_bar::take(&mut settings.message_bar, value)?,
        keys::NOISE_SUPPRESSION | keys::CAPTION_HEIGHT => take_calls(settings, key, value)?,
        keys::FIRST_ACCOUNT => {
            settings.first_account = Some(chat_id(value).ok_or(bad(keys::FIRST_ACCOUNT, "not an account's MRI"))?);
        }
        other => {
            return Err(Problem::UnknownKey { key: other.to_owned() });
        }
    }
    Ok(())
}

fn take_time_format(value: &Value) -> Result<TimeFormat, Problem> {
    value.as_str().and_then(TimeFormat::from_key).ok_or(Problem::BadValue {
        key: keys::TIME_FORMAT,
        why: "not one of system, 12-hour, 24-hour",
    })
}

/// Takes the people's calendars the accounts added: an array of tables each naming the account (an MRI), the person's
/// address and their name. One that is bad is left out, the others are taken.
fn take_people_calendars(settings: &mut Settings, value: &Value) -> Result<(), Problem> {
    let bad = Problem::BadValue {
        key: keys::PEOPLE_CALENDARS,
        why: "not a list of calendars, each with an account, an address and a name",
    };
    let entries = value.as_array().ok_or(bad.clone())?;
    let mut failed = false;
    for entry in entries {
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let taken = text("account")
            .and_then(|account| chat_id(&Value::String(account)))
            .zip(text("address").filter(|address| address.contains('@')))
            .map(|(account, address)| PersonCalendar {
                account,
                name: text("name").unwrap_or_else(|| address.clone()),
                address,
            });
        match taken {
            Some(calendar) => settings.people_calendars.push(calendar),
            None => failed = true,
        }
    }
    if failed { Err(bad) } else { Ok(()) }
}

/// Takes the chats' local AI settings: the tool proofreading drafts, the tool translating chats, or the language each
/// account reads a chat in (an array of tables naming the account, the chat and the language; one that is bad is left
/// out, the others are taken).
fn take_chat_translation(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    if key == keys::MESSAGE_CORRECTOR {
        settings.message_corrector = ai_choice(value).ok_or(Problem::BadValue {
            key: keys::MESSAGE_CORRECTOR,
            why: AI_CHOICE_SHAPE,
        })?;
        return Ok(());
    }
    if key == keys::CHAT_TRANSLATOR {
        settings.chat_translator = ai_choice(value).ok_or(Problem::BadValue {
            key: keys::CHAT_TRANSLATOR,
            why: AI_CHOICE_SHAPE,
        })?;
        return Ok(());
    }
    let bad = Problem::BadValue {
        key: keys::CHAT_LANGUAGES,
        why: "not a list of chats, each with an account, a chat id and a language chats are translated into",
    };
    let entries = value.as_array().ok_or(bad.clone())?;
    let mut failed = false;
    for entry in entries {
        let id = |key: &str| entry.get(key).and_then(chat_id);
        let language = entry
            .get("language")
            .and_then(Value::as_str)
            .and_then(languages::language)
            .map(|(code, _)| code);
        match (id("account"), id("chat"), language) {
            (Some(account), Some(chat), Some(language)) => settings.set_chat_language(&account, &chat, Some(language)),
            _ => failed = true,
        }
    }
    if failed { Err(bad) } else { Ok(()) }
}

/// Takes a message's translation: the language messages are read in, the language drafts are written in (any name
/// `languages::language_name` takes), or the tool that translates a draft.
fn take_message_translation(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    if key == keys::MESSAGE_TRANSLATOR {
        settings.message_translator = ai_choice(value).ok_or(Problem::BadValue {
            key: keys::MESSAGE_TRANSLATOR,
            why: AI_CHOICE_SHAPE,
        })?;
        return Ok(());
    }
    let (key, kept) = if key == keys::DRAFT_TRANSLATE_TO {
        (keys::DRAFT_TRANSLATE_TO, &mut settings.draft_translate_to)
    } else {
        (keys::MESSAGE_TRANSLATE_TO, &mut settings.message_translate_to)
    };
    *kept = value
        .as_str()
        .and_then(languages::language_name)
        .ok_or(Problem::BadValue {
            key,
            why: "not a language's name",
        })?;
    Ok(())
}

/// Takes the other accounts' own settings: each a table under the account's MRI (an id as a chat id is kept) holding
/// `selected_chat` and `manual_status`. One that is bad leaves out that account's, the others are taken.
fn take_accounts(settings: &mut Settings, value: &Value) -> Result<(), Problem> {
    let bad = Problem::BadValue {
        key: keys::ACCOUNTS,
        why: "not a table of accounts, each with a chat id and a status this app knows",
    };
    let table = value.as_table().ok_or(bad.clone())?;
    let mut failed = false;
    for (account, kept) in table {
        let taken = chat_id(&Value::String(account.clone()))
            .zip(kept.as_table())
            .and_then(|(account, kept)| {
                let mut own = AccountSettings::default();
                for (key, value) in kept {
                    match key.as_str() {
                        keys::SELECTED_CHAT => own.selected_chat = Some(chat_id(value)?),
                        keys::MANUAL_STATUS => own.manual_status = value.as_str().and_then(ManualStatus::from_key)?,
                        _ => return None,
                    }
                }
                Some((account, own))
            });
        match taken {
            Some((account, own)) => {
                settings.accounts.insert(account, own);
            }
            None => failed = true,
        }
    }
    if failed { Err(bad) } else { Ok(()) }
}

/// Takes one of how the app looks: whether the theme follows the system, a theme's key of the catalog, the font size
/// within [`teams_theme::FONT_SIZES`] or the font family's name.
fn take_appearance(appearance: &mut Appearance, key: &str, value: &Value) -> Result<(), Problem> {
    let theme_key = |key: &'static str| {
        value
            .as_str()
            .and_then(teams_theme::find)
            .map(|spec| spec.key)
            .ok_or(Problem::BadValue {
                key,
                why: "not a theme this app knows",
            })
    };
    match key {
        keys::FOLLOW_SYSTEM_THEME => {
            appearance.follow_system = value.as_bool().ok_or(Problem::BadValue {
                key: keys::FOLLOW_SYSTEM_THEME,
                why: "not true or false",
            })?;
        }
        keys::ANIMATIONS => {
            appearance.animations = value.as_bool().ok_or(Problem::BadValue {
                key: keys::ANIMATIONS,
                why: "not true or false",
            })?;
        }
        keys::THEME => appearance.theme = theme_key(keys::THEME)?,
        keys::LIGHT_THEME => appearance.light_theme = theme_key(keys::LIGHT_THEME)?,
        keys::DARK_THEME => appearance.dark_theme = theme_key(keys::DARK_THEME)?,
        keys::FONT_SIZE => {
            appearance.font_size = value
                .as_integer()
                .and_then(|size| u16::try_from(size).ok())
                .filter(|size| teams_theme::FONT_SIZES.contains(size))
                .ok_or(Problem::BadValue {
                    key: keys::FONT_SIZE,
                    why: "not a whole number of pixels from 10 to 24",
                })?;
        }
        _ => {
            appearance.font_family = Some(device_name(value).ok_or(Problem::BadValue {
                key: keys::FONT_FAMILY,
                why: "not a font's name",
            })?);
        }
    }
    Ok(())
}

/// The chats' and messages' translation and correction settings, which [`take_translation`] takes.
const TRANSLATIONS: [&str; 6] = [
    keys::CHAT_LANGUAGES,
    keys::CHAT_TRANSLATOR,
    keys::MESSAGE_CORRECTOR,
    keys::MESSAGE_TRANSLATE_TO,
    keys::DRAFT_TRANSLATE_TO,
    keys::MESSAGE_TRANSLATOR,
];

/// Takes one of [`TRANSLATIONS`].
fn take_translation(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    match key {
        keys::CHAT_LANGUAGES | keys::CHAT_TRANSLATOR | keys::MESSAGE_CORRECTOR => {
            take_chat_translation(settings, key, value)
        }
        _ => take_message_translation(settings, key, value),
    }
}

/// The settings that [`take_switch`] takes.
const SWITCHES: [&str; 5] = [
    keys::VIDEO_CALLS,
    keys::SAVE_CALL_CAPTIONS,
    keys::MEETING_STARTED,
    keys::MEETING_REMINDERS,
    keys::AI_FEATURES,
];

/// Takes an on/off switch: video in calls, saving every call's captions, the meeting started notification, the meeting
/// reminders, or "Use AI features".
fn take_switch(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    if key == keys::AI_FEATURES {
        let on = value.as_bool().ok_or(Problem::BadValue {
            key: keys::AI_FEATURES,
            why: "not true or false",
        })?;
        settings.ai_features = Some(on);
        return Ok(());
    }
    let (key, switch) = match key {
        keys::VIDEO_CALLS => (keys::VIDEO_CALLS, &mut settings.video_calls),
        keys::MEETING_STARTED => (keys::MEETING_STARTED, &mut settings.meeting_started),
        keys::MEETING_REMINDERS => (keys::MEETING_REMINDERS, &mut settings.meeting_reminders),
        _ => (keys::SAVE_CALL_CAPTIONS, &mut settings.save_call_captions),
    };
    *switch = value.as_bool().ok_or(Problem::BadValue {
        key,
        why: "not true or false",
    })?;
    Ok(())
}

/// Takes the local AI tools' settings: the language captions are translated into, the tool translating them, or the
/// tool summarizing recaps.
fn take_captions(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    if key == keys::CAPTION_TRANSLATE_TO {
        settings.caption_translate_to = Some(
            value
                .as_str()
                .and_then(languages::language)
                .map(|(code, _)| code)
                .ok_or(Problem::BadValue {
                    key: keys::CAPTION_TRANSLATE_TO,
                    why: "not a language captions are translated into",
                })?,
        );
    } else if key == keys::RECAP_SUMMARIZER {
        settings.recap_summarizer = ai_choice(value).ok_or(Problem::BadValue {
            key: keys::RECAP_SUMMARIZER,
            why: AI_CHOICE_SHAPE,
        })?;
    } else {
        settings.caption_translator = ai_choice(value).ok_or(Problem::BadValue {
            key: keys::CAPTION_TRANSLATOR,
            why: AI_CHOICE_SHAPE,
        })?;
    }
    Ok(())
}

/// What a local AI choice that can't be read isn't.
const AI_CHOICE_SHAPE: &str =
    "not a provider (automatic, claude, codex or an AI provider's id) or a table of a provider and a model";

/// Takes the AI providers: those that can be read (the others are left out, and said).
fn take_providers(settings: &mut Settings, value: &Value) -> Result<(), Problem> {
    let (list, bad) = providers::from_value(value);
    settings.ai_providers = list;
    if bad {
        return Err(Problem::BadValue {
            key: providers::FILE_KEY,
            why: "an entry is not a provider (id, kind, name, enabled, base_url, model, key) or repeats one",
        });
    }
    Ok(())
}

/// A local AI choice as the file keeps it: a table of its tool and model, or (a file of an earlier build) the tool's
/// name alone, its model the recommended one.
fn ai_choice(value: &Value) -> Option<AiChoice> {
    if let Some(tool) = value.as_str() {
        return ToolChoice::from_key(tool).map(AiChoice::of);
    }
    let table = value.as_table()?;
    let tool = match table.get(keys::AI_TOOL) {
        Some(tool) => ToolChoice::from_key(tool.as_str()?)?,
        None => ToolChoice::default(),
    };
    let model = match table.get(keys::AI_MODEL) {
        Some(model) => Model::from_key(model.as_str()?)?,
        None => Model::default(),
    };
    Some(AiChoice { tool, model })
}

/// A local AI choice, as the file keeps it.
fn ai_choice_value(choice: &AiChoice) -> Value {
    let mut table = Table::new();
    table.insert(keys::AI_TOOL.to_owned(), Value::String(choice.tool.key().to_owned()));
    table.insert(keys::AI_MODEL.to_owned(), Value::String(choice.model.key().to_owned()));
    Value::Table(table)
}

/// Takes the camera's background in calls.
fn take_video_effect(settings: &mut Settings, value: &Value) -> Result<(), Problem> {
    settings.video_effect = value
        .as_str()
        .and_then(VideoEffect::from_key)
        .ok_or(Problem::BadValue {
            key: keys::VIDEO_EFFECT,
            why: "not one of none, blur, stock/<a picture of the app's>, added/<a picture's file name>",
        })?;
    Ok(())
}

/// Takes a device's choice: a device name, as the settings keep one.
fn take_device(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    let (kind, key) = match key {
        keys::MICROPHONE => (DeviceKind::Microphone, keys::MICROPHONE),
        keys::SPEAKER => (DeviceKind::Speaker, keys::SPEAKER),
        _ => (DeviceKind::Camera, keys::CAMERA),
    };
    let name = device_name(value).ok_or(Problem::BadValue {
        key,
        why: "not a device's name",
    })?;
    settings.set_device(kind, Some(name));
    Ok(())
}

/// A device name (or a font family's) as the settings keep one: a non-empty string without control characters, at
/// most [`MAX_DEVICE_NAME`] bytes.
fn device_name(value: &Value) -> Option<String> {
    let name = value.as_str()?;
    let shaped = !name.is_empty() && name.len() <= MAX_DEVICE_NAME && !name.chars().any(char::is_control);
    shaped.then(|| name.to_owned())
}

/// A chat id as the settings keep one: a string of visible ASCII, at most [`MAX_CHAT_ID`] bytes.
fn chat_id(value: &Value) -> Option<String> {
    let id = value.as_str()?;
    let shaped = !id.is_empty() && id.len() <= MAX_CHAT_ID && id.bytes().all(|byte| byte.is_ascii_graphic());
    shaped.then(|| id.to_owned())
}

/// The file's text for `settings`.
#[must_use]
pub fn render(settings: &Settings) -> String {
    let mut table = Table::new();
    if let Some(chat) = &settings.selected_chat {
        table.insert(keys::SELECTED_CHAT.to_owned(), Value::String(chat.clone()));
    }
    for (key, on) in [
        (keys::SEND_TYPING, settings.send_typing),
        (keys::SEND_READ_RECEIPTS, settings.send_read_receipts),
        (keys::SEND_PRESENCE, settings.send_presence),
        (keys::VIDEO_CALLS, settings.video_calls),
        (keys::SAVE_CALL_CAPTIONS, settings.save_call_captions),
        (keys::MEETING_STARTED, settings.meeting_started),
        (keys::MEETING_REMINDERS, settings.meeting_reminders),
    ] {
        table.insert(key.to_owned(), Value::Boolean(on));
    }
    table.insert(
        keys::FIRST_RUN.to_owned(),
        Value::String(settings.first_run.key().to_owned()),
    );
    if let Some(on) = settings.ai_features {
        table.insert(keys::AI_FEATURES.to_owned(), Value::Boolean(on));
    }
    table.insert(
        keys::NOTIFICATIONS.to_owned(),
        Value::String(settings.notifications.key().to_owned()),
    );
    table.insert(
        keys::IDLE_MINUTES.to_owned(),
        Value::Integer(i64::from(settings.idle_minutes)),
    );
    table.insert(
        keys::MANUAL_STATUS.to_owned(),
        Value::String(settings.manual_status.key().to_owned()),
    );
    table.insert(
        keys::LOG_LEVEL.to_owned(),
        Value::String(settings.log_level.key().to_owned()),
    );
    for (key, kind) in [
        (keys::MICROPHONE, DeviceKind::Microphone),
        (keys::SPEAKER, DeviceKind::Speaker),
        (keys::CAMERA, DeviceKind::Camera),
    ] {
        if let Some(name) = settings.device(kind) {
            table.insert(key.to_owned(), Value::String(name.to_owned()));
        }
    }
    table.insert(
        keys::VIDEO_EFFECT.to_owned(),
        Value::String(settings.video_effect.key()),
    );
    render_appearance(&settings.appearance, &mut table);
    table.insert(
        keys::TIME_FORMAT.to_owned(),
        Value::String(settings.time_format.key().to_owned()),
    );
    render_captions(settings, &mut table);
    if !settings.ai_providers.is_empty() {
        table.insert(
            providers::FILE_KEY.to_owned(),
            providers::to_value(&settings.ai_providers),
        );
    }
    table.insert(
        keys::RECAP_SUMMARIZER.to_owned(),
        ai_choice_value(&settings.recap_summarizer),
    );
    render_accounts(settings, &mut table);
    if let Some(first) = &settings.first_account {
        table.insert(keys::FIRST_ACCOUNT.to_owned(), Value::String(first.clone()));
    }
    render_people(settings, &mut table);
    render_chat_translation(settings, &mut table);
    toolbar::render(&settings.compose_hidden, &settings.compose_shown, &mut table);
    message_bar::render(&settings.message_bar, &mut table);
    noise::render(&settings.noise_suppression, &mut table);
    captions_height::render(&settings.caption_heights, &mut table);
    sounds::render(&settings.sounds, &mut table);
    table.to_string()
}

/// The other accounts' own settings, into the file's table (when there are any).
fn render_accounts(settings: &Settings, table: &mut Table) {
    let accounts: Table = settings
        .accounts
        .iter()
        .map(|(account, own)| {
            let mut kept = Table::new();
            if let Some(chat) = &own.selected_chat {
                kept.insert(keys::SELECTED_CHAT.to_owned(), Value::String(chat.clone()));
            }
            kept.insert(
                keys::MANUAL_STATUS.to_owned(),
                Value::String(own.manual_status.key().to_owned()),
            );
            (account.clone(), Value::Table(kept))
        })
        .collect();
    if !accounts.is_empty() {
        table.insert(keys::ACCOUNTS.to_owned(), Value::Table(accounts));
    }
}

/// The people's calendars the accounts added, into the file's table (when there are any).
fn render_people(settings: &Settings, table: &mut Table) {
    if !settings.people_calendars.is_empty() {
        let people = settings
            .people_calendars
            .iter()
            .map(|calendar| {
                let mut entry = Table::new();
                entry.insert("account".to_owned(), Value::String(calendar.account.clone()));
                entry.insert("address".to_owned(), Value::String(calendar.address.clone()));
                entry.insert("name".to_owned(), Value::String(calendar.name.clone()));
                Value::Table(entry)
            })
            .collect();
        table.insert(keys::PEOPLE_CALENDARS.to_owned(), Value::Array(people));
    }
}

/// The chats' local AI settings, including draft correction and translation, and any chat languages, into the file's
/// table.
fn render_chat_translation(settings: &Settings, table: &mut Table) {
    table.insert(
        keys::CHAT_TRANSLATOR.to_owned(),
        ai_choice_value(&settings.chat_translator),
    );
    table.insert(
        keys::MESSAGE_CORRECTOR.to_owned(),
        ai_choice_value(&settings.message_corrector),
    );
    table.insert(
        keys::MESSAGE_TRANSLATE_TO.to_owned(),
        Value::String(settings.message_translate_to.clone()),
    );
    table.insert(
        keys::DRAFT_TRANSLATE_TO.to_owned(),
        Value::String(settings.draft_translate_to.clone()),
    );
    table.insert(
        keys::MESSAGE_TRANSLATOR.to_owned(),
        ai_choice_value(&settings.message_translator),
    );
    if !settings.chat_languages.is_empty() {
        let chats = settings
            .chat_languages
            .iter()
            .map(|kept| {
                let mut entry = Table::new();
                entry.insert("account".to_owned(), Value::String(kept.account.clone()));
                entry.insert("chat".to_owned(), Value::String(kept.chat.clone()));
                entry.insert("language".to_owned(), Value::String(kept.language.to_owned()));
                Value::Table(entry)
            })
            .collect();
        table.insert(keys::CHAT_LANGUAGES.to_owned(), Value::Array(chats));
    }
}

/// The captions' translation, into the file's table (the language only when one is picked).
fn render_captions(settings: &Settings, table: &mut Table) {
    if let Some(language) = settings.caption_translate_to {
        table.insert(
            keys::CAPTION_TRANSLATE_TO.to_owned(),
            Value::String(language.to_owned()),
        );
    }
    table.insert(
        keys::CAPTION_TRANSLATOR.to_owned(),
        ai_choice_value(&settings.caption_translator),
    );
}

/// How the app looks, into the file's table (the font family only when one is named).
fn render_appearance(appearance: &Appearance, table: &mut Table) {
    table.insert(
        keys::FOLLOW_SYSTEM_THEME.to_owned(),
        Value::Boolean(appearance.follow_system),
    );
    for (key, theme) in [
        (keys::THEME, appearance.theme),
        (keys::LIGHT_THEME, appearance.light_theme),
        (keys::DARK_THEME, appearance.dark_theme),
    ] {
        table.insert(key.to_owned(), Value::String(theme.to_owned()));
    }
    table.insert(
        keys::FONT_SIZE.to_owned(),
        Value::Integer(i64::from(appearance.font_size)),
    );
    table.insert(keys::ANIMATIONS.to_owned(), Value::Boolean(appearance.animations));
    if let Some(family) = &appearance.font_family {
        table.insert(keys::FONT_FAMILY.to_owned(), Value::String(family.clone()));
    }
}

/// The settings as read from their file, and what was wrong with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub settings: Settings,
    pub problems: Vec<Problem>,
}

/// The settings' file (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's path, as a problem names it.
    #[must_use]
    pub fn shown(&self) -> String {
        self.path.display().to_string()
    }

    /// The settings in the file; the defaults when there is none, or with every problem found.
    #[must_use]
    pub fn load(&self) -> Loaded {
        let unreadable = |why: String| Loaded {
            settings: Settings::default(),
            problems: vec![Problem::Unreadable {
                path: self.shown(),
                why,
            }],
        };
        let text = match read_private(&self.path) {
            Ok(Some(text)) => text,
            Ok(None) => {
                return Loaded {
                    settings: Settings::default(),
                    problems: Vec::new(),
                };
            }
            Err(why) => return unreadable(why),
        };
        match parse(&text) {
            Ok((settings, problems)) => Loaded { settings, problems },
            Err(why) => unreadable(why.to_owned()),
        }
    }

    /// Writes `settings` to the file (see the module docs).
    ///
    /// # Errors
    /// [`Problem::Unsaved`] when the directory or the file can't be written.
    pub fn save(&self, settings: &Settings) -> Result<(), Problem> {
        write_private(&self.path, render(settings).as_bytes()).map_err(|error| Problem::Unsaved {
            path: self.shown(),
            why: error.to_string(),
        })
    }
}

/// The file's text; `None` when there is no file. Access by others is removed before reading; Unix owner-only
/// modes are preserved, and Windows uses the platform's protected DACL.
fn read_private(path: &Path) -> Result<Option<String>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("it isn't a file".to_owned());
    }
    #[cfg(unix)]
    let restrict = {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o077 != 0
    };
    #[cfg(windows)]
    let restrict = true;
    if restrict {
        teams_platform::private::restrict_file(path)
            .map_err(|error| format!("the settings file can't be made private: {error}"))?;
    }
    let mut text = String::new();
    file.take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() as u64 > MAX_FILE {
        return Err("it is too large".to_owned());
    }
    Ok(Some(text))
}

/// Writes `bytes` to `path` whole: the directory made (or kept) `0700`, a temporary file beside it `0600`, then
/// renamed over it.
///
/// # Errors
/// The path has no directory, or the directory or the file can't be made, written or renamed.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the settings file has no directory"))?;
    teams_platform::private::create_dir_all(dir)?;
    teams_platform::private::restrict_dir(dir)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = (|| {
        let mut file = teams_platform::private::replace(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        // a temporary file left behind would only be overwritten by the next save
        let _ = fs::remove_file(&temporary);
    }
    written
}
