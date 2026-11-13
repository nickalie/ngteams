//! The settings view, in place of the list and the conversation while Settings is chosen, laid out as Teams lays out
//! its own: a column of pages on the left (General, Appearance, Notifications, Privacy, Calls where the app has calls,
//! Diagnostics), the chosen page on the right, each a title over boxes of rows, a row's name and what it does leading
//! and its control trailing.
//!
//! - **General**: the account's manual status, whether its availability is reported, the idle time after which
//!   it no longer is, and the time format (following the system's locale, 12-hour or 24-hour).
//! - **Appearance**: the theme (following the system's light or dark mode with a theme for each, or one theme
//!   whatever the system says), each a tile drawn in its own colours; the font size with its steps; the font family,
//!   searchable among the system's.
//! - **Notifications**: which messages notify, whether a meeting's start does (Meetings), and the sounds ([`sounds`]): the
//!   pack, then each event's sound.
//! - **Privacy**: sending typing and read receipts.
//! - **Calls** (where the app has calls, or its calls have video): the calls' microphone, speaker and camera
//!   ("Follow the system", naming the system's default, then every device there is, a device chosen that isn't there
//!   shown as not connected), "Make a test call" under them as Teams has it (the echo service's test call, asked of
//!   the shell: [`SettingsViewEvent::MakeTestCall`]; offered while no call is up or rings on any account, the calling
//!   endpoint is registered and the app isn't read-only, else said why), the calls' video switch ("Video in
//!   calls"), and the camera's video effects (blur, a picture behind the person) with their self-preview
//!   ([`crate::effects::EffectsPicker`], made while the page shows and dropped as it goes, its preview with it),
//!   "Save live captions of every call" ([`crate::call_transcript`]), then the local AI tools: which tool and model translates live captions and which summarizes meeting recaps, each with
//!   the same control ([`ai_picker::AiPicker`]).
//! - **Diagnostics**: the diagnostic log's level and "Copy diagnostics".
//!
//! What was wrong with the settings file (or the log) is shown above every page. Each change goes to the
//! [`SettingsModel`] (and so to the file, and a device's to the calls) at once.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::IndexPath;
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::switch::Switch;
use gpui_component::{Disableable as _, Sizable as _};
use teams_core::{Choice, Reporting};
use teams_log::diagnostics::WARNING;
use teams_media::devices::catalog::{DeviceKind, DeviceList, DeviceStatus};

use crate::clock::{self, HourCycle};
use crate::effects::{EffectsPicker, PickerPlace};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::local_ai::Work;
use crate::presence;
use crate::settings::{LogLevel, ManualStatus, Notifications, Settings, SettingsModel, TimeFormat};
use crate::settings_view::ai_picker::AiPicker;
use crate::shell::shown_as;
use crate::theme::{self, Appearance, FONT_SIZE, FONT_SIZES, Palette, THEMES, ThemeSpec};
use crate::trace::ViewTrace;

pub mod ai_picker;
pub mod sounds;

/// The font picker's first row: the system's UI font (no family named).
pub const SYSTEM_FONT_LABEL: &str = "System font";

/// The idle times offered, in minutes (the one set is offered too, if it isn't one of these).
pub const IDLE_CHOICES: [u32; 6] = [1, 5, 10, 15, 30, 60];

/// The key of the device row that follows the system (its test selector `<kind>-system`).
pub const FOLLOW_SYSTEM: &str = "system";

/// The page column's width, px.
const NAV_WIDTH: f32 = 240.0;
/// The widest a page's content gets, px.
const PAGE_WIDTH: f32 = 880.0;
/// A theme tile's width, px.
const TILE_WIDTH: f32 = 96.0;
/// A theme tile's preview height, px.
const TILE_HEIGHT: f32 = 60.0;
/// The tallest a picker's menu gets before it scrolls, px: a long device list or a short window still reaches every
/// row.
const MENU_HEIGHT: f32 = 320.0;

/// What a choice's control does when it is chosen.
type Apply = Rc<dyn Fn(&mut App)>;

/// Whether the Calls page offers its test call now, as the shell tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestCallOffer {
    /// Not at all: the build shows no calls, the echo service isn't known, or the app is read-only.
    Hidden,
    /// It can start now.
    Ready,
    /// Not while this account has a call up, ringing or being set up.
    InCall,
    /// Not while another account of the app has one.
    OtherAccount,
    /// Not until the calling endpoint is registered.
    Connecting,
}

impl TestCallOffer {
    /// What its row says under its name: what the test call does, or why it can't start now.
    fn description(self) -> &'static str {
        match self {
            Self::Hidden | Self::Ready => TEST_CALL_DESCRIPTION,
            Self::InCall => "You can make a test call once your call ends.",
            Self::OtherAccount => "You can make a test call once the call on your other account ends.",
            Self::Connecting => "Calls are connecting. You can make a test call in a moment.",
        }
    }
}

/// What the test call's row says it does, as Teams' test call does it.
pub const TEST_CALL_DESCRIPTION: &str =
    "Check your microphone, speaker and camera: Echo plays a message, records yours and plays it back.";

/// What the shell tells the view of its test call, asked each time the Calls page draws.
pub type TestCallState = Rc<dyn Fn(&App) -> TestCallOffer>;

/// What the settings view asks of the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsViewEvent {
    /// The user pressed "Make a test call".
    MakeTestCall,
}

/// The name the device of `kind` the calls use goes by: the one the settings chose while it is there, else the
/// system's default; `None` while the devices aren't listed (or the system has no default).
#[must_use]
pub fn device_in_use(kind: DeviceKind, model: &SettingsModel) -> Option<String> {
    let list = model.devices()?;
    model
        .settings()
        .device(kind)
        .and_then(|name| list.find(kind, name))
        .or_else(|| list.default_of(kind))
        .map(|device| device.description.clone())
}

/// One choice of a device of a kind, as the settings' pickers and the call window's device menus offer it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceChoice {
    /// Its key in test selectors: the device's name, or [`FOLLOW_SYSTEM`].
    pub key: String,
    /// What it is called: "Follow the system" (naming the system's default), a device's description, or a device
    /// chosen that isn't there, as not connected.
    pub label: SharedString,
    /// It is the one the settings chose.
    pub chosen: bool,
    /// What choosing it sets: the device's name, `None` to follow the system.
    pub name: Option<String>,
}

/// The choices of a device of `kind`: "Follow the system", then every device of the kind there is, then a device
/// chosen that isn't there.
#[must_use]
pub fn device_choices(kind: DeviceKind, settings: &Settings, list: &DeviceList) -> Vec<DeviceChoice> {
    let chosen = settings.device(kind).map(str::to_owned);
    let system = match list.default_of(kind) {
        Some(device) => format!("Follow the system ({})", device.description),
        None => "Follow the system".to_owned(),
    };
    let mut choices = vec![DeviceChoice {
        key: FOLLOW_SYSTEM.to_owned(),
        label: system.into(),
        chosen: chosen.is_none(),
        name: None,
    }];
    let mut names: Vec<(String, String)> = list
        .of(kind)
        .map(|device| (device.name.clone(), device.description.clone()))
        .collect();
    if let Some(missing) = chosen.as_ref().filter(|name| list.find(kind, name).is_none()) {
        names.push((missing.clone(), format!("{missing} (not connected)")));
    }
    choices.extend(names.into_iter().map(|(name, label)| DeviceChoice {
        key: name.clone(),
        label: label.into(),
        chosen: chosen.as_deref() == Some(name.as_str()),
        name: Some(name),
    }));
    choices
}

/// What the settings call each kind of device.
#[must_use]
pub fn device_label(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Microphone => "Microphone",
        DeviceKind::Speaker => "Speaker",
        DeviceKind::Camera => "Camera",
    }
}

/// A status's mark, as the account menu and the settings show it: its presence's dot, or for Automatic the
/// "follows the window's use" arrows.
#[must_use]
pub fn status_mark(status: ManualStatus, palette: Palette) -> AnyElement {
    match status.choice() {
        Choice::Manual(manual) => div()
            .w(px(16.0))
            .flex()
            .justify_center()
            .child(presence::dot(&shown_as(Reporting::Manual(manual)), palette))
            .into_any_element(),
        Choice::Automatic => Glyph::REFRESH
            .icon()
            .size(px(14.0))
            .text_color(rgb(palette.muted_foreground))
            .into_any_element(),
    }
}

/// A page of the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    General,
    Appearance,
    Notifications,
    Privacy,
    Calls,
    Diagnostics,
}

impl Page {
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Appearance,
        Self::Notifications,
        Self::Privacy,
        Self::Calls,
        Self::Diagnostics,
    ];

    /// Its name in its nav row's test selector, `settings-page-<key>`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Notifications => "notifications",
            Self::Privacy => "privacy",
            Self::Calls => "calls",
            Self::Diagnostics => "diagnostics",
        }
    }

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Notifications => "Notifications",
            Self::Privacy => "Privacy",
            Self::Calls => "Calls and devices",
            Self::Diagnostics => "Diagnostics",
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Self::General => Glyph::USER,
            Self::Appearance => Glyph::PALETTE,
            Self::Notifications => Glyph::BELL,
            Self::Privacy => Glyph::SHIELD,
            Self::Calls => Glyph::HEADPHONES,
            Self::Diagnostics => Glyph::BUG,
        }
    }
}

/// The font picker's state.
type FontSelect = SelectState<SearchableVec<SharedString>>;

/// The font picker, its rows (the system's font first, then the families) and the view's subscription to it.
struct Fonts {
    select: Entity<FontSelect>,
    names: Vec<SharedString>,
    _confirmed: Subscription,
}

impl Fonts {
    /// Shows the family `appearance` holds when the picker shows another (another account's view, or the file,
    /// changed it); a family the system doesn't have is offered too, as at the start.
    fn show(&mut self, appearance: &Appearance, window: &mut Window, cx: &mut App) {
        let shown: SharedString = appearance
            .font_family
            .clone()
            .map_or_else(|| SYSTEM_FONT_LABEL.into(), Into::into);
        if self.select.read(cx).selected_value() == Some(&shown) {
            return;
        }
        let items = (!self.names.contains(&shown)).then(|| {
            self.names.push(shown.clone());
            SearchableVec::new(self.names.clone())
        });
        self.select.update(cx, |select, cx| {
            if let Some(items) = items {
                select.set_items(items, window, cx);
            }
            select.set_selected_value(&shown, window, cx);
        });
    }
}

/// One choice of a picker or a segmented control: its key (its test selector is `<group>-<key>`), its words, a
/// status's mark, whether it is the one set, and what choosing it changes.
struct Pick {
    key: String,
    label: SharedString,
    status: Option<ManualStatus>,
    chosen: bool,
    apply: Apply,
}

/// The settings view (see the module docs).
pub struct SettingsView {
    model: Entity<SettingsModel>,
    /// The app's calls have video: its switch is shown.
    video_switch: bool,
    /// The account whose own settings (its manual status) the view shows ([`SettingsView::for_account`]); `None`: the
    /// first account's.
    account: Option<String>,
    /// The page shown.
    page: Page,
    /// The page column's focus: Up, Down, Home and End move between the pages while it has it.
    focus: FocusHandle,
    /// The font picker, made the first time the view draws it (it needs the window).
    fonts: Option<Fonts>,
    /// Whether the test call is offered now, as the shell tells it ([`SettingsView::with_test_call`]); `None`: never.
    test_call: Option<TestCallState>,
    /// The video effects' picker while the Calls page shows.
    effects: Option<Entity<EffectsPicker>>,
    /// The local AI choices' pickers (captions, then recaps), made the first time the Calls page shows.
    ai: Option<[Entity<AiPicker>; 2]>,
    /// The chats' translator's picker, once the General page showed it.
    chat_ai: Option<Entity<AiPicker>>,
    correction_ai: Option<Entity<AiPicker>>,
    message_translation_ai: Option<Entity<AiPicker>>,
    /// The page's scrolling.
    scroll: ScrollHandle,
    /// A sound pack or file being added.
    adding: Option<Task<()>>,
    /// Why the last one couldn't be.
    adding_problem: Option<SharedString>,
    _observing: Subscription,
}

impl EventEmitter<SettingsViewEvent> for SettingsView {}

impl SettingsView {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&model, |_, _, cx| cx.notify());
        let video_switch = model.read(cx).shows_video_switch();
        Self {
            model,
            video_switch,
            account: None,
            page: Page::General,
            focus: cx.focus_handle(),
            fonts: None,
            test_call: None,
            effects: None,
            ai: None,
            chat_ai: None,
            correction_ai: None,
            message_translation_ai: None,
            scroll: ScrollHandle::new(),
            adding: None,
            adding_problem: None,
            _observing: observing,
        }
    }

    /// The view offering the test call on its Calls page as `state` tells it each time the page draws (the shell's:
    /// its account's calls and the app's other accounts).
    #[must_use]
    pub fn with_test_call(mut self, state: TestCallState) -> Self {
        self.test_call = Some(state);
        self
    }

    /// Whether the test call is offered now.
    #[must_use]
    pub fn test_call_offer(&self, cx: &App) -> TestCallOffer {
        self.test_call.as_ref().map_or(TestCallOffer::Hidden, |state| state(cx))
    }

    /// "Make a test call" pressed: asked of the shell, which asks the calls (and refuses it as any start is refused
    /// while a call is up on any account).
    pub fn make_test_call(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsViewEvent::MakeTestCall);
    }

    /// The view showing `account`'s own settings (its MRI): an app of several accounts has a view per account.
    #[must_use]
    pub fn for_account(mut self, account: String) -> Self {
        self.account = Some(account);
        self
    }

    /// The family the font picker shows, once the view has drawn it.
    #[must_use]
    pub fn shown_font(&self, cx: &App) -> Option<SharedString> {
        self.fonts.as_ref()?.select.read(cx).selected_value().cloned()
    }

    /// The page shown.
    #[must_use]
    pub fn page(&self) -> Page {
        self.page
    }

    /// Shows `page`.
    pub fn show_page(&mut self, page: Page, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            cx.notify();
        }
    }

    /// The settings went out of sight: the video effects' picker goes, and its preview with it.
    pub fn put_away(&mut self) {
        self.effects = None;
    }

    /// The video effects' picker, while the Calls page shows it.
    #[must_use]
    pub fn effects_picker(&self) -> Option<&Entity<EffectsPicker>> {
        self.effects.as_ref()
    }

    /// Scrolls the page to its end.
    pub fn scroll_to_end(&self) {
        self.scroll.scroll_to_bottom();
    }

    /// Whether the Calls page offers the video effects: the app's calls have video, or there is a camera to preview.
    fn shows_effects(&self, cx: &App) -> bool {
        let model = self.model.read(cx);
        model.media_status() == DeviceStatus::Ready && (self.video_switch || model.camera_preview().is_some())
    }

    /// Gives the page column the keyboard, as Settings opens.
    pub fn focus_pages(&self, window: &mut Window, cx: &mut App) {
        self.focus.focus(window, cx);
    }

    /// The pages the column lists: Calls only where it has something.
    fn pages(&self, cx: &App) -> Vec<Page> {
        let calls = self.has_calls(cx);
        Page::ALL
            .into_iter()
            .filter(|page| *page != Page::Calls || calls)
            .collect()
    }

    /// Up, Down, Home and End on the page column: the page before, after, first or last.
    fn page_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let pages = self.pages(cx);
        let at = pages.iter().position(|page| *page == self.page).unwrap_or(0);
        let to = match keystroke.key.as_str() {
            "up" => at.saturating_sub(1),
            "down" => (at + 1).min(pages.len() - 1),
            "home" => 0,
            "end" => pages.len() - 1,
            _ => return,
        };
        cx.stop_propagation();
        self.show_page(pages[to], cx);
    }

    /// Whether the Calls page has anything to show: the calls' devices, the test call, or the video switch.
    fn has_calls(&self, cx: &App) -> bool {
        self.shows_effects(cx)
            || self.model.read(cx).devices().is_some()
            || self.test_call_offer(cx) != TestCallOffer::Hidden
    }

    /// A change of the settings, as a control's handler makes it.
    fn changer(&self, change: impl Fn(&mut Settings) + 'static) -> Apply {
        let model = self.model.clone();
        Rc::new(move |cx| model.update(cx, |model, cx| model.change(&change, cx)))
    }

    /// A switch, its test selector its id; `set` writes the value asked for.
    fn switch(&self, id: &'static str, checked: bool, set: fn(&mut Settings, bool)) -> AnyElement {
        let model = self.model.clone();
        selected_by(
            id,
            Switch::new(id).checked(checked).on_click(move |checked: &bool, _, cx| {
                let checked = *checked;
                model.update(cx, |model, cx| model.change(|settings| set(settings, checked), cx));
            }),
        )
        .into_any_element()
    }

    // --- the pages ---

    fn general(&self, settings: &Settings, palette: Palette) -> Vec<AnyElement> {
        let account = self
            .account
            .clone()
            .or_else(|| settings.first_account.clone())
            .unwrap_or_default();
        let set = settings.manual_status_of(&account);
        let statuses = ManualStatus::ALL
            .into_iter()
            .map(|status| {
                let chosen = account.clone();
                Pick {
                    key: status.key().to_owned(),
                    label: status.label().into(),
                    status: Some(status),
                    chosen: set == status,
                    apply: self.changer(move |settings| settings.set_manual_status(&chosen, status)),
                }
            })
            .collect();
        vec![
            group(
                Some("Status"),
                vec![row(
                    "Your status",
                    Some("What others see. Automatic shows you available while you use the app."),
                    picker("status", statuses, palette),
                    palette,
                )],
                palette,
            ),
            group(
                Some("Availability"),
                vec![
                    row(
                        "Report my availability",
                        Some("Off: this app never tells Teams you are available."),
                        self.switch("send-presence", settings.send_presence, |settings, on| {
                            settings.send_presence = on;
                        }),
                        palette,
                    ),
                    row(
                        "Stop reporting me available after",
                        Some("How long the app may go unused before you no longer show as available."),
                        picker("idle", self.idle_picks(settings), palette),
                        palette,
                    ),
                ],
                palette,
            ),
            self.date_and_time(settings, palette),
        ]
    }

    /// The time format: following the system's locale (the description says which clock that is), 12-hour or 24-hour;
    /// each segment's test selector is `time-format-<key>`.
    fn date_and_time(&self, settings: &Settings, palette: Palette) -> AnyElement {
        let choices = TimeFormat::ALL
            .into_iter()
            .map(|format| Pick {
                key: format.key().to_owned(),
                label: format.label().into(),
                status: None,
                chosen: settings.time_format == format,
                apply: self.changer(move |settings| settings.time_format = format),
            })
            .collect();
        let description = match clock::system_cycle() {
            HourCycle::Twelve => "Times like 3:30 PM or 15:30. Your system's region uses 12-hour.",
            HourCycle::TwentyFour => "Times like 3:30 PM or 15:30. Your system's region uses 24-hour.",
        };
        group(
            Some("Date and time"),
            vec![row(
                "Time format",
                Some(description),
                segmented("time-format", choices, palette),
                palette,
            )],
            palette,
        )
    }

    /// The idle times, and the one set if it isn't one of them.
    fn idle_picks(&self, settings: &Settings) -> Vec<Pick> {
        let mut idle = IDLE_CHOICES.to_vec();
        if !idle.contains(&settings.idle_minutes) {
            idle.push(settings.idle_minutes);
            idle.sort_unstable();
        }
        idle.into_iter()
            .map(|minutes| Pick {
                key: minutes.to_string(),
                label: minutes_label(minutes).into(),
                status: None,
                chosen: settings.idle_minutes == minutes,
                apply: self.changer(move |settings| settings.idle_minutes = minutes),
            })
            .collect()
    }

    fn appearance(
        &mut self,
        appearance: &Appearance,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let select = self.font_select(appearance, window, cx);
        let font = selected_by(
            "font-family",
            div()
                .w(px(240.0))
                .child(Select::new(&select).search_placeholder("Search fonts")),
        );
        vec![
            group(Some("Theme"), self.themes(appearance, palette), palette),
            group(
                Some("Text"),
                vec![
                    row(
                        "Font size",
                        Some(concat!(
                            ctrl!(),
                            "+= and ",
                            ctrl!(),
                            "+- change it anywhere in the app, ",
                            ctrl!(),
                            "+0 resets it."
                        )),
                        self.font_size(appearance, palette),
                        palette,
                    ),
                    row("Font", None, font, palette),
                ],
                palette,
            ),
            group(
                Some("Motion"),
                vec![row(
                    "Play animations",
                    Some(
                        "Fade chats in as they open and let new messages rise. Off, everything holds still, animated pictures too.",
                    ),
                    self.switch("animations", appearance.animations, |settings, on| {
                        settings.appearance.animations = on;
                    }),
                    palette,
                )],
                palette,
            ),
        ]
    }

    /// The theme's rows: following the system, then the tiles of each of its modes, or of the one theme. Each tile's
    /// test selector is `<side>-<key>`: `light-theme`, `dark-theme` or `theme`, and the theme's key.
    fn themes(&self, appearance: &Appearance, palette: Palette) -> Vec<AnyElement> {
        let follow = row(
            "Match the system",
            Some("Use one theme while the system is light and another while it is dark."),
            self.switch("follow-system-theme", appearance.follow_system, |settings, on| {
                settings.appearance.follow_system = on;
            }),
            palette,
        );
        let mut rows = vec![follow];
        if appearance.follow_system {
            rows.push(self.theme_side(
                "light-theme",
                Some("When the system is light"),
                appearance.light_theme,
                |appearance, key| appearance.light_theme = key,
                palette,
            ));
            rows.push(self.theme_side(
                "dark-theme",
                Some("When the system is dark"),
                appearance.dark_theme,
                |appearance, key| appearance.dark_theme = key,
                palette,
            ));
        } else {
            rows.push(self.theme_side(
                "theme",
                None,
                appearance.theme,
                |appearance, key| appearance.theme = key,
                palette,
            ));
        }
        rows
    }

    /// A row of every theme's tile, `chosen` marked; picking one `set`s it.
    fn theme_side(
        &self,
        group: &'static str,
        title: Option<&'static str>,
        chosen: &'static str,
        set: fn(&mut Appearance, &'static str),
        palette: Palette,
    ) -> AnyElement {
        let tiles = THEMES.iter().map(|spec| {
            let key = spec.key;
            let apply = self.changer(move |settings| set(&mut settings.appearance, key));
            theme_tile(group, spec, chosen == key, palette, apply)
        });
        div()
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .children(title.map(|title| div().text_sm().text_color(rgb(palette.muted_foreground)).child(title)))
            .child(div().flex().flex_wrap().gap_3().children(tiles))
            .into_any_element()
    }

    /// The font size: a step smaller, the size, a step larger and back to the default (Ctrl+-, Ctrl+= and Ctrl+0 do
    /// the same anywhere in the window).
    fn font_size(&self, appearance: &Appearance, palette: Palette) -> AnyElement {
        let size = appearance.font_size;
        let step = |id: &'static str, glyph: Glyph, tip: String, to: u16, disabled: bool| {
            let apply = self.changer(move |settings| settings.appearance.font_size = to);
            selected_by(
                id,
                icon_button(id, glyph, tip)
                    .small()
                    .disabled(disabled)
                    .on_click(move |_, _, cx| apply(cx)),
            )
        };
        let stepper = div()
            .flex()
            .items_center()
            .p_0p5()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .child(step(
                "font-smaller",
                Glyph::MINUS,
                concat!("Smaller (", ctrl!(), "+-)").to_owned(),
                appearance.stepped_font_size(-1),
                size <= *FONT_SIZES.start(),
            ))
            .child(
                div()
                    .debug_selector(|| "font-size".to_owned())
                    .min_w(px(52.0))
                    .flex()
                    .justify_center()
                    .child(SharedString::from(format!("{size} px"))),
            )
            .child(step(
                "font-larger",
                Glyph::PLUS,
                concat!("Larger (", ctrl!(), "+=)").to_owned(),
                appearance.stepped_font_size(1),
                size >= *FONT_SIZES.end(),
            ));
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(stepper)
            .child(step(
                "font-reset",
                Glyph::RESET,
                format!(concat!("Reset to {} px (", ctrl!(), "+0)"), FONT_SIZE),
                FONT_SIZE,
                size == FONT_SIZE,
            ))
            .into_any_element()
    }

    /// The font picker, made over the system's font families (and a family chosen that isn't there) the first time,
    /// and showing the family the settings hold each time: another account's view, or the file, may have changed it.
    fn font_select(
        &mut self,
        appearance: &Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<FontSelect> {
        if let Some(fonts) = &mut self.fonts {
            fonts.show(appearance, window, cx);
            return fonts.select.clone();
        }
        let chosen = appearance.font_family.clone();
        let mut names: Vec<SharedString> = vec![SYSTEM_FONT_LABEL.into()];
        let families = cx.text_system().all_font_names();
        names.extend(
            families
                .iter()
                .filter(|family| !family.starts_with('.'))
                .map(|family| SharedString::from(family.clone())),
        );
        if let Some(missing) = chosen.as_ref().filter(|family| !families.contains(family)) {
            names.push(missing.clone().into());
        }
        let selected = chosen
            .as_deref()
            .map_or(Some(0), |family| names.iter().position(|name| name.as_ref() == family))
            .map(IndexPath::new);
        let select =
            cx.new(|cx| SelectState::new(SearchableVec::new(names.clone()), selected, window, cx).searchable(true));
        let model = self.model.clone();
        let subscription = cx.subscribe(
            &select,
            move |_, _, event: &SelectEvent<SearchableVec<SharedString>>, cx| {
                let SelectEvent::Confirm(Some(name)) = event else {
                    return;
                };
                let family = (name.as_ref() != SYSTEM_FONT_LABEL).then(|| name.to_string());
                model.update(cx, |model, cx| {
                    model.change(|settings| settings.appearance.font_family = family, cx);
                });
            },
        );
        self.fonts = Some(Fonts {
            select: select.clone(),
            names,
            _confirmed: subscription,
        });
        select
    }

    fn notifications(&self, settings: &Settings, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let choices = Notifications::ALL
            .into_iter()
            .map(|level| Pick {
                key: level.key().to_owned(),
                label: level.label().into(),
                status: None,
                chosen: settings.notifications == level,
                apply: self.changer(move |settings| settings.notifications = level),
            })
            .collect();
        let mut boxes = vec![
            group(
                None,
                vec![row(
                    "Notify me about",
                    Some("A desktop notification for every new message, only for those that mention you, or none."),
                    segmented("notifications", choices, palette),
                    palette,
                )],
                palette,
            ),
            group(
                Some("Meetings"),
                vec![row(
                    "Meeting started notification",
                    Some("A notification with Join when a meeting you're invited to, or one in a group chat, starts."),
                    self.switch("meeting-started", settings.meeting_started, |settings, on| {
                        settings.meeting_started = on;
                    }),
                    palette,
                )],
                palette,
            ),
        ];
        boxes.extend(self.sound_groups(settings, palette, cx));
        boxes
    }

    fn privacy(&self, settings: &Settings, palette: Palette) -> Vec<AnyElement> {
        vec![group(
            None,
            vec![
                row(
                    "Send typing indicators",
                    Some("Others in a chat see when you are typing."),
                    self.switch("send-typing", settings.send_typing, |settings, on| {
                        settings.send_typing = on;
                    }),
                    palette,
                ),
                row(
                    "Send read receipts",
                    Some("Others in a chat see when you have read their messages."),
                    self.switch("send-read-receipts", settings.send_read_receipts, |settings, on| {
                        settings.send_read_receipts = on;
                    }),
                    palette,
                ),
            ],
            palette,
        )]
    }

    fn calls(&mut self, settings: &Settings, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut groups = Vec::new();
        if self.model.read(cx).media_status() == DeviceStatus::Unavailable {
            groups.push(media_unavailable());
        }
        let rows = self.call_device_rows(settings, palette, cx);
        if !rows.is_empty() {
            groups.push(group(Some("Devices"), rows, palette));
        }
        groups.push(self.ringtone_group(settings, palette, cx));
        if self.video_switch {
            groups.push(group(
                Some("Video"),
                vec![row(
                    "Video in calls",
                    Some("When off, calls are audio only"),
                    self.switch("video-calls", settings.video_calls, |settings, on| {
                        settings.video_calls = on;
                    }),
                    palette,
                )],
                palette,
            ));
        }
        if self.shows_effects(cx) {
            groups.push(self.video_effects(palette, cx));
        }
        groups.push(group(
            Some("Live captions"),
            vec![row(
                "Save live captions of every call",
                Some(SAVE_CAPTIONS_NOTE),
                self.switch("save-call-captions", settings.save_call_captions, |settings, on| {
                    settings.save_call_captions = on;
                }),
                palette,
            )],
            palette,
        ));
        groups.extend(self.ai_pickers(cx).into_iter().map(IntoElement::into_any_element));
        groups
    }

    fn call_device_rows(&self, settings: &Settings, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = self
            .model
            .read(cx)
            .devices()
            .map(|list| self.devices(settings, list, palette))
            .unwrap_or_default();
        let status = self.model.read(cx).media_status();
        match status {
            DeviceStatus::Ready => {}
            DeviceStatus::Preparing => {
                rows.clear();
                rows.push(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .debug_selector(|| "media-status".to_owned())
                        .child("Preparing media…")
                        .into_any_element(),
                );
            }
            // said in a warning box over the page instead ([`media_unavailable`])
            DeviceStatus::Unavailable => return Vec::new(),
        }
        let offer = self.test_call_offer(cx);
        if offer != TestCallOffer::Hidden {
            rows.push(Self::test_call_row(offer, palette, cx));
        }
        rows
    }

    /// The chats' translator's picker ([`crate::chat_translate`]), on the General page beside the chats' other
    /// settings.
    pub fn chat_ai_picker(&mut self, cx: &mut Context<Self>) -> Entity<AiPicker> {
        let model = self.model.clone();
        self.chat_ai
            .get_or_insert_with(|| cx.new(|cx| AiPicker::new(model, Work::Chat, cx)))
            .clone()
    }

    /// The drafts' proofreader's picker ([`crate::message_correct`]), on the General page after the chats' translator.
    pub fn correction_ai_picker(&mut self, cx: &mut Context<Self>) -> Entity<AiPicker> {
        let model = self.model.clone();
        self.correction_ai
            .get_or_insert_with(|| cx.new(|cx| AiPicker::new(model, Work::Correction, cx)))
            .clone()
    }

    /// The drafts' translator's picker with their language ([`crate::draft_translate`]), on the General page after the
    /// drafts' proofreader.
    pub fn message_translation_ai_picker(&mut self, cx: &mut Context<Self>) -> Entity<AiPicker> {
        let model = self.model.clone();
        self.message_translation_ai
            .get_or_insert_with(|| cx.new(|cx| AiPicker::new(model, Work::Draft, cx)))
            .clone()
    }

    /// The local AI choices' pickers: the captions' translator, then the recaps' summarizer.
    pub fn ai_pickers(&mut self, cx: &mut Context<Self>) -> [Entity<AiPicker>; 2] {
        let model = self.model.clone();
        self.ai
            .get_or_insert_with(|| {
                [Work::Captions, Work::Summary].map(|work| cx.new(|cx| AiPicker::new(model.clone(), work, cx)))
            })
            .clone()
    }

    /// The camera's video effects: what they do, the self-preview and the backgrounds.
    fn video_effects(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.clone();
        let picker = self
            .effects
            .get_or_insert_with(|| {
                cx.new(|cx| EffectsPicker::new(model, Arc::new(ViewTrace::off()), PickerPlace::Settings, cx))
            })
            .clone();
        group(
            Some("Video effects"),
            vec![
                div()
                    .debug_selector(|| "video-effects".to_owned())
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(
                        "Blur your background or put a picture behind you in calls. Your choice is kept for \
                                 every call, and a call with your camera on changes at once.",
                    ))
                    .child(picker)
                    .into_any_element(),
            ],
            palette,
        )
    }

    /// The calls' devices: for each kind, a picker of "Follow the system" (naming the system's default), then every
    /// device there is, and a device chosen that isn't there, as not connected. Each picker's test selector is its
    /// kind's key, each of its rows' `<kind>-<key>`: the device's name, or [`FOLLOW_SYSTEM`].
    fn devices(&self, settings: &Settings, list: &DeviceList, palette: Palette) -> Vec<AnyElement> {
        DeviceKind::ALL
            .map(|kind| {
                let picks = self.device_picks(kind, settings, list);
                row(device_label(kind), None, picker(kind.key(), picks, palette), palette)
            })
            .into_iter()
            .collect()
    }

    /// "Make a test call" under the devices, as Teams has it: what it does, or why it can't start now with the button
    /// disabled. The button's test selector is `make-test-call`.
    fn test_call_row(offer: TestCallOffer, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let button = Button::new("make-test-call")
            .icon(Glyph::PHONE.icon())
            .label("Make a test call")
            .outline()
            .small()
            .disabled(offer != TestCallOffer::Ready)
            .on_click(cx.listener(|this, _, _, cx| this.make_test_call(cx)));
        row(
            "Test call",
            Some(offer.description()),
            selected_by("make-test-call", button),
            palette,
        )
    }

    fn device_picks(&self, kind: DeviceKind, settings: &Settings, list: &DeviceList) -> Vec<Pick> {
        device_choices(kind, settings, list)
            .into_iter()
            .map(|choice| {
                let DeviceChoice {
                    key,
                    label,
                    chosen,
                    name,
                } = choice;
                Pick {
                    key,
                    label,
                    status: None,
                    chosen,
                    apply: self.changer(move |settings| settings.set_device(kind, name.clone())),
                }
            })
            .collect()
    }

    /// The diagnostic log: its level (changed live; `TEAMS_APP_LOG` overrides it, said here), "Copy diagnostics", and
    /// what the last copy held: said plainly when it has full requests and responses.
    fn diagnostics(&self, settings: &Settings, model: &SettingsModel, palette: Palette) -> Vec<AnyElement> {
        let levels = LogLevel::ALL
            .into_iter()
            .map(|level| Pick {
                key: level.key().to_owned(),
                label: level_label(level).into(),
                status: None,
                chosen: settings.log_level == level,
                apply: self.changer(move |settings| settings.log_level = level),
            })
            .collect();
        let overridden = teams_log::installed()
            .and_then(teams_log::FileLog::overridden)
            .map(|level| {
                caption("log-level-overridden", palette.muted_foreground)
                    .child(SharedString::from(format!(
                        "TEAMS_APP_LOG sets the level to {} for this run.",
                        level.key()
                    )))
                    .into_any_element()
            });
        let mut log = vec![row(
            "Log level",
            Some("Debug writes full requests and responses to the log; credentials are always left out."),
            segmented("log-level", levels, palette),
            palette,
        )];
        log.extend(overridden);
        let mut report = vec![row(
            "Copy diagnostics",
            Some("The app's diagnostic log, to paste into a bug report."),
            self.copy_button(),
            palette,
        )];
        report.extend(copied_note(model, palette));
        vec![
            group(Some("Log"), log, palette),
            group(Some("Report a problem"), report, palette),
        ]
    }

    fn copy_button(&self) -> AnyElement {
        let model = self.model.clone();
        selected_by(
            "copy-diagnostics",
            Button::new("copy-diagnostics")
                .label("Copy diagnostics")
                .outline()
                .small()
                .on_click(move |_, _, cx| model.update(cx, SettingsModel::copy_diagnostics)),
        )
        .into_any_element()
    }

    // --- the frame ---

    /// The page column: its title, then a row per page (Calls only where it has something), the shown one marked.
    fn nav(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.focus.is_focused(window);
        let rows = self.pages(cx).into_iter().map(|page| {
            let shown = page == self.page;
            let id: SharedString = format!("settings-page-{}", page.key()).into();
            let selector = id.to_string();
            let mark = page_mark(&id, shown, palette, cx);
            div()
                .debug_selector(move || selector.clone())
                .id(id)
                .relative()
                .h(px(36.0))
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .cursor_pointer()
                .text_color(rgb(if shown { palette.brand } else { palette.foreground }))
                .when(shown, |row| {
                    row.bg(theme::gradient(
                        palette.selected,
                        theme::mix(palette.selected, palette.surface, 0.6),
                        90.0,
                    ))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                })
                .child(mark)
                // where the keyboard is: the page shown, ringed while the column has the focus
                .border_1()
                .border_color(rgb(if shown && focused {
                    palette.brand
                } else {
                    palette.surface
                }))
                .when(!shown, |row| row.hover(move |row| row.bg(rgb(palette.hover))))
                .child(page.glyph().icon().size(px(18.0)))
                .child(page.title())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.focus.focus(window, cx);
                    this.show_page(page, cx);
                }))
        });
        div()
            .debug_selector(|| "settings-nav".to_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::page_key))
            .w(px(NAV_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap_0p5()
            .px_2()
            .bg(rgb(palette.surface))
            .border_r_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .h(px(48.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Settings"),
            )
            .children(rows)
            .into_any_element()
    }

    /// The shown page's boxes.
    fn page_content(
        &mut self,
        settings: &Settings,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        match self.page {
            Page::General => {
                let mut boxes = self.general(settings, palette);
                boxes.push(self.chat_ai_picker(cx).into_any_element());
                boxes.push(self.correction_ai_picker(cx).into_any_element());
                boxes.push(self.message_translation_ai_picker(cx).into_any_element());
                boxes
            }
            Page::Appearance => self.appearance(&settings.appearance, palette, window, cx),
            Page::Notifications => self.notifications(settings, palette, cx),
            Page::Privacy => self.privacy(settings, palette),
            Page::Calls => self.calls(settings, palette, cx),
            Page::Diagnostics => {
                let model = self.model.clone();
                self.diagnostics(settings, model.read(cx), palette)
            }
        }
    }
}

/// `minutes` in words.
fn minutes_label(minutes: u32) -> String {
    match minutes {
        1 => "1 minute".to_owned(),
        60 => "1 hour".to_owned(),
        _ => format!("{minutes} minutes"),
    }
}

/// A log level's short name, for its segment.
fn level_label(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "Errors",
        LogLevel::Warn => "Warnings",
        LogLevel::Info => "Info",
        LogLevel::Debug => "Debug",
    }
}

/// A line under a row, in `color`, its test selector `selector`.
/// What "Save live captions of every call" does, under it.
pub const SAVE_CAPTIONS_NOTE: &str = "Turns captions on in every call and saves them on this computer as the call's \
                                      transcript, with its AI summary. Teams and the others in the call see nothing.";

fn caption(selector: &'static str, color: u32) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .px_4()
        .pb_3()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(rgb(color))
}

/// What the last "copy diagnostics" held, under its row: said plainly when it has full requests and responses.
fn copied_note(model: &SettingsModel, palette: Palette) -> Option<AnyElement> {
    let copied = model.copied()?;
    let (text, color) = if copied.has_debug {
        (format!("Copied. It {WARNING}."), theme::DANGER)
    } else {
        ("Copied to the clipboard.".to_owned(), palette.muted_foreground)
    };
    Some(
        caption("diagnostics-copied", color)
            .child(Glyph::CHECK.icon().size(px(14.0)))
            .child(SharedString::from(text))
            .into_any_element(),
    )
}

/// The page shown's mark in the page column: a bar in the brand gradient at its row's left edge, growing in as the page
/// is chosen.
fn page_mark(id: &SharedString, shown: bool, palette: Palette, cx: &App) -> AnyElement {
    let mark = div()
        .absolute()
        .left_0()
        .top(px(18.0))
        .w(px(3.0))
        .rounded_r_sm()
        .bg(theme::brand_gradient(palette, 180.0));
    crate::motion::toggle(mark, SharedString::from(format!("{id}-mark")), shown, cx, |mark, t| {
        let height = 20.0 * t.max(0.0);
        mark.h(px(height)).mt(px(-height / 2.0))
    })
}

/// A titled box of rows, a hairline between each.
fn group(title: Option<&'static str>, rows: Vec<AnyElement>, palette: Palette) -> AnyElement {
    let rows = rows.into_iter().enumerate().map(|(index, row)| {
        div()
            .when(index > 0, |row| row.border_t_1().border_color(rgb(palette.border)))
            .child(row)
    });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(title.map(|title| div().font_weight(gpui::FontWeight::SEMIBOLD).child(title)))
        .child(
            div()
                .flex()
                .flex_col()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                // raised a little off the page
                .shadow(theme::glow(palette, 0x0000_0000, 0.06, 10.0, 2.0))
                .children(rows),
        )
        .into_any_element()
}

/// One row: its name and what it does leading, its control trailing.
fn row(
    title: &'static str,
    description: Option<&'static str>,
    control: impl IntoElement,
    palette: Palette,
) -> AnyElement {
    row_with(title.into(), description.map(SharedString::from), control, palette)
}

/// [`row`], its words made as it draws.
fn row_with(
    title: SharedString,
    description: Option<SharedString>,
    control: impl IntoElement,
    palette: Palette,
) -> AnyElement {
    div()
        .min_h(px(56.0))
        .px_4()
        .py_2p5()
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(title)
                .children(description.map(|description| {
                    div()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child(description)
                })),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// A few choices side by side, the one set filled; each segment's test selector is `<group>-<key>`.
fn segmented(group: &'static str, choices: Vec<Pick>, palette: Palette) -> AnyElement {
    let segments = choices.into_iter().map(|pick| {
        let id: SharedString = format!("{group}-{}", pick.key).into();
        let selector = id.to_string();
        let apply = pick.apply;
        div()
            .debug_selector(move || selector.clone())
            .id(id)
            .h(px(28.0))
            .px_3()
            .flex()
            .items_center()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(pick.chosen, |segment| {
                segment
                    .bg(rgb(palette.brand))
                    .text_color(rgb(palette.on_brand))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
            })
            .when(!pick.chosen, |segment| {
                segment.hover(move |segment| segment.bg(rgb(palette.hover)))
            })
            .child(pick.label)
            .on_click(move |_, _, cx| apply(cx))
    });
    div()
        .flex()
        .items_center()
        .gap_0p5()
        .p_0p5()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.background))
        .children(segments)
        .into_any_element()
}

/// A dropdown of `picks`, showing the one set; its test selector is `group`, each of its menu's rows'
/// `<group>-<key>`.
fn picker(group: &'static str, picks: Vec<Pick>, palette: Palette) -> AnyElement {
    let shown = picks.iter().find(|pick| pick.chosen);
    let face = div()
        .w_full()
        .flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .children(
            shown
                .and_then(|pick| pick.status)
                .map(|status| status_mark(status, palette)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(shown.map_or_else(|| SharedString::from("Choose…"), |pick| pick.label.clone())),
        )
        .child(
            Glyph::CHEVRON_DOWN
                .icon()
                .size(px(14.0))
                .text_color(rgb(palette.muted_foreground)),
        );
    let picks = Rc::new(picks);
    selected_by(
        group,
        Button::new(group)
            .outline()
            .min_w(px(220.0))
            .max_w(px(320.0))
            .child(face)
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let menu = menu.min_w(px(220.0)).scrollable(true).max_h(px(MENU_HEIGHT));
                picks
                    .iter()
                    .fold(menu, |menu, pick| menu.item(pick_item(group, pick, palette)))
            }),
    )
    .into_any_element()
}

/// A picker's menu row.
fn pick_item(group: &'static str, pick: &Pick, palette: Palette) -> PopupMenuItem {
    let selector = format!("{group}-{}", pick.key);
    let (label, status, apply) = (pick.label.clone(), pick.status, Rc::clone(&pick.apply));
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap_2()
            .children(status.map(|status| status_mark(status, palette)))
            .child(label.clone())
    })
    .checked(pick.chosen)
    .on_click(move |_, _, cx| apply(cx))
}

/// A theme's preview: a small window in its own colours (the frame with the rail's chosen mark, and on it the sheet
/// of the list with its chosen row and two messages).
fn theme_preview(spec: &ThemeSpec) -> gpui::Div {
    let colors = spec.palette;
    let bar = |width: f32| {
        div()
            .h(px(4.0))
            .w(px(width))
            .rounded_full()
            .bg(rgb(colors.muted_foreground))
    };
    let list = div()
        .w(px(38.0))
        .h_full()
        .flex()
        .flex_col()
        .gap_1p5()
        .p_1p5()
        .bg(rgb(colors.surface))
        .child(div().h(px(6.0)).rounded_sm().bg(rgb(colors.selected)))
        .child(bar(24.0))
        .child(bar(18.0));
    let chat = div()
        .flex_1()
        .flex()
        .flex_col()
        .justify_end()
        .gap_1()
        .p_1p5()
        .child(div().h(px(8.0)).w(px(36.0)).rounded_sm().bg(rgb(colors.surface)))
        .child(
            div().flex().justify_end().child(
                div()
                    .h(px(8.0))
                    .w(px(30.0))
                    .rounded_sm()
                    .bg(theme::brand_gradient(colors, 90.0)),
            ),
        );
    let sheet = div()
        .flex_1()
        .h_full()
        .flex()
        .overflow_hidden()
        .bg(rgb(colors.background))
        .when(colors.depth, |sheet| sheet.rounded_tl(px(4.0)))
        .child(list)
        .child(chat);
    div()
        .h(px(TILE_HEIGHT))
        .w_full()
        .flex()
        .overflow_hidden()
        .rounded_md()
        .pt(px(8.0))
        .bg(theme::frame(colors))
        .child(
            div()
                .w(px(12.0))
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .pt_1()
                .child(div().size(px(5.0)).rounded_full().bg(rgb(colors.on_frame))),
        )
        .child(sheet)
}

/// A theme's tile: its preview, its name under it, the one set ringed in the brand colour. Its test selector is
/// `<group>-<key>`.
fn theme_tile(group: &'static str, spec: &ThemeSpec, chosen: bool, palette: Palette, apply: Apply) -> AnyElement {
    let id: SharedString = format!("{group}-{}", spec.key).into();
    let selector = id.to_string();
    let check = chosen.then(|| Glyph::CHECK.icon().size(px(14.0)).text_color(rgb(palette.brand)));
    div()
        .debug_selector(move || selector.clone())
        .id(id)
        .w(px(TILE_WIDTH))
        .flex()
        .flex_col()
        .gap_1p5()
        .cursor_pointer()
        .child(
            div()
                .p_0p5()
                .rounded_lg()
                .border_2()
                .border_color(rgb(if chosen { palette.brand } else { palette.border }))
                .when(!chosen, |frame| {
                    frame.hover(move |frame| frame.border_color(rgb(palette.muted_foreground)))
                })
                .child(theme_preview(spec)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_sm()
                .when(chosen, |name| name.font_weight(gpui::FontWeight::SEMIBOLD))
                .child(spec.name)
                .children(check),
        )
        .on_click(move |_, _, cx| apply(cx))
        .into_any_element()
}

/// What was wrong with the settings file, one line each, in a warning box; nothing when all was well.
fn problems(model: &SettingsModel) -> Option<AnyElement> {
    let problems = model.problems();
    if problems.is_empty() {
        return None;
    }
    let lines = problems
        .iter()
        .map(|problem| div().child(SharedString::from(problem.to_string())).into_any_element());
    Some(warning_box("settings-problems", lines))
}

/// The call devices couldn't be set up this run: what that means and what to do, in a warning box.
fn media_unavailable() -> AnyElement {
    warning_box(
        "media-status",
        [
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Calls aren't available")
                .into_any_element(),
            div().child(MEDIA_UNAVAILABLE).into_any_element(),
        ],
    )
}

/// What the Calls page says when the call devices couldn't be set up.
pub const MEDIA_UNAVAILABLE: &str = "The app couldn't set up your microphone, speaker and camera, so you can't make or \
                                     answer calls. Restart the app to try again. Chats work as usual.";

/// A problem, said in a box with Teams' red edge and a mark, a line per element.
fn warning_box(selector: &'static str, lines: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .flex()
        .gap_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(rgb(theme::DANGER))
        .text_sm()
        .child(Glyph::INFO.icon().size(px(16.0)).text_color(rgb(theme::DANGER)))
        .child(div().flex_1().min_w_0().flex().flex_col().gap_1().children(lines))
        .into_any_element()
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        if self.page == Page::Calls && !self.has_calls(cx) {
            self.page = Page::General;
        }
        if self.page != Page::Calls {
            self.put_away();
        }
        let settings = self.model.read(cx).settings().clone();
        let content = self.page_content(&settings, palette, window, cx);
        let problems = problems(self.model.read(cx));
        let nav = self.nav(palette, window, cx);
        div()
            .debug_selector(|| "settings-view".to_owned())
            .size_full()
            .flex()
            .bg(rgb(palette.background))
            .child(nav)
            .child(
                // scrolled: a page outgrows the pane at a large font size or in a small window
                div()
                    .id("settings")
                    .debug_selector(|| "settings".to_owned())
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child({
                        let page = div()
                            .w_full()
                            .max_w(px(PAGE_WIDTH))
                            .px_8()
                            .pt_6()
                            .pb_8()
                            .flex()
                            .flex_col()
                            .gap_6()
                            .child(
                                div()
                                    .text_size(px(22.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(self.page.title()),
                            )
                            .children(problems)
                            .children(content);
                        // another page fades in from a little below
                        crate::motion::rise(
                            page,
                            SharedString::from(format!("settings-in-{}", self.page.key())),
                            8.0,
                            cx,
                        )
                    }),
            )
    }
}
