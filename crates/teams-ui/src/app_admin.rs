//! Manage apps: the settings page where an own server's administrator registers and manages its apps and bots
//! (`docs/server.md`, "Apps and bots"), behind `NGTEAMS_BOTS=1` ([`crate::bots_flag`]). It lists the server's apps
//! with a search, registers a bot from a form, uploads a Teams app package, and opens an app to turn it on or off,
//! change its bot's endpoint and mode and who may use it, give its bot a new secret, show the settings each SDK takes,
//! or delete it after a confirmation. A bot's delivery health shows as a mark on its row when it fails or is paused,
//! and in its details with a "Send a test activity" button that says how the bot answered. A bot's app id and secret
//! are shown once, with Copy, when the bot is made or gets a new secret. Teams itself has no counterpart (Microsoft's apps are managed in a web admin centre), so the
//! page is drawn as Manage users ([`crate::user_admin`]) is: the settings' own boxes and the team dialogs' surfaces.
//!
//! The page asks nothing itself: every read and change goes through the [`AppAdmin`] the app attached for the
//! account, off the foreground, and only an account whose server says it is an administrator and serves apps gets
//! one. What the server refuses is said in the user's words ([`AppProblem::words`]).
//!
//! **The keyboard**: Tab walks the search, "Register a bot", "Upload an app" and the list; in the list Up, Down, Home
//! and End move, Enter or Space opens the app. A dialog traps Tab, Enter in a field confirms, Escape closes (it waits
//! while a change is under way); a confirmation starts on Cancel. The search has the keyboard again once a dialog
//! closes.
//!
//! Traced: `app-admin shown`, `listed` (the count as kinds), `dialog`, `done` and `failed`, each with a kind, never a
//! name, an id or a secret.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::spinner::Spinner;
use gpui_component::{Sizable as _, Size};
use teams_auth::admin::{
    AdminApp, AdminError, AppChanges, BotAuthMode, HealthState, MadeApp, Refusal, TestActivity, UserPage,
    apps_not_served,
};

use crate::icons::{Glyph, selected_by};
use crate::keyboard;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::user_admin::{AdminProblem, badge, short};

mod dialog;
mod forms;
pub mod health;
pub mod sdk;

pub use dialog::{AppDialog, AppDialogEvent, DialogKind};
pub use forms::shown_secret;

/// What the search asks for when it is empty.
pub const SEARCH_PLACEHOLDER: &str = "Search apps";
/// The longest bot name the server takes, in characters.
pub const MAX_BOT_NAME: usize = 30;

/// A bot registered from the form, as the administrator fills it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotForm {
    pub name: String,
    pub description: String,
    pub endpoint: Option<String>,
    pub auth_mode: BotAuthMode,
}

/// What a bot's settings name of the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotConnection {
    /// `https://host[:port]`.
    pub origin: String,
    /// The issuer of the server's tokens to bots.
    pub issuer: String,
}

/// Why the server didn't do what the administrator asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppProblem {
    InvalidName,
    InvalidEndpoint,
    /// The address in the endpoint is on a network the server doesn't send to.
    EndpointNotAllowed,
    /// A test activity to a bot that has no endpoint.
    NoEndpoint,
    /// A test activity to an app that is off.
    AppDisabled,
    UnsignedNotAllowed,
    InvalidPackage(String),
    PackageTooLarge,
    /// The package's app is registered already.
    AppExists,
    /// The package's bot is another app's.
    BotExists,
    UnknownUser,
    /// The app is gone (another administrator deleted it).
    NotFound,
    /// The package file couldn't be read.
    File(String),
    /// What the account management says of the account and the server (not an administrator, signed out, …).
    Account(AdminProblem),
}

impl AppProblem {
    /// The problem in the user's words.
    #[must_use]
    pub fn words(&self) -> SharedString {
        match self {
            Self::InvalidName => "Enter a name of 1 to 30 characters.".into(),
            Self::InvalidEndpoint => {
                "Enter the bot's messaging endpoint as a full https address, like https://bot.example.com/api/messages."
                    .into()
            }
            Self::EndpointNotAllowed => "The server doesn't send to this address. Use a public address, or ask the \
                 server's operator to allow the bot's private network."
                .into(),
            Self::NoEndpoint => "Set the bot's messaging endpoint first.".into(),
            Self::AppDisabled => "Turn the app on first.".into(),
            Self::UnsignedNotAllowed => {
                "This server takes no unsigned bots. Choose Signed, or ask the server's operator to allow them.".into()
            }
            Self::InvalidPackage(said) if said.is_empty() => "This isn't a Teams app package the server takes.".into(),
            Self::InvalidPackage(said) => format!("This app package can't be added: {said}").into(),
            Self::PackageTooLarge => "This app package is too large. A package can be at most 5 MB.".into(),
            Self::AppExists => "This app is on the server already.".into(),
            Self::BotExists => "This package's bot belongs to another app on the server.".into(),
            Self::UnknownUser => "One of the people chosen is no longer on the server. Choose again.".into(),
            Self::NotFound => "This app no longer exists.".into(),
            Self::File(said) => format!("The file can't be read: {said}").into(),
            Self::Account(AdminProblem::SignedOut) => {
                "You're signed out of this server. Sign in again to manage apps.".into()
            }
            Self::Account(problem) => problem.words(),
        }
    }

    /// Its name in the trace.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidName => "invalid-name",
            Self::InvalidEndpoint => "invalid-endpoint",
            Self::EndpointNotAllowed => "endpoint-not-allowed",
            Self::NoEndpoint => "no-endpoint",
            Self::AppDisabled => "app-disabled",
            Self::UnsignedNotAllowed => "unsigned-not-allowed",
            Self::InvalidPackage(_) => "invalid-package",
            Self::PackageTooLarge => "package-too-large",
            Self::AppExists => "app-exists",
            Self::BotExists => "bot-exists",
            Self::UnknownUser => "unknown-user",
            Self::NotFound => "not-found",
            Self::File(_) => "file",
            Self::Account(problem) => problem.kind(),
        }
    }

    /// Whether the server said the account is no administrator any more.
    #[must_use]
    pub fn not_admin(&self) -> bool {
        *self == Self::Account(AdminProblem::NotAdmin)
    }
}

impl std::fmt::Display for AppProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words())
    }
}

impl std::error::Error for AppProblem {}

impl From<AdminProblem> for AppProblem {
    fn from(problem: AdminProblem) -> Self {
        match problem {
            AdminProblem::NotFound => Self::NotFound,
            AdminProblem::InvalidName => Self::InvalidName,
            other => Self::Account(other),
        }
    }
}

impl From<AdminError> for AppProblem {
    fn from(error: AdminError) -> Self {
        if apps_not_served(&error) {
            return Self::Account(AdminProblem::Other("This server doesn't serve apps.".to_owned()));
        }
        let AdminError::Refused { refusal, message, .. } = &error else {
            return AdminProblem::from(error).into();
        };
        match refusal {
            Refusal::InvalidEndpoint => Self::InvalidEndpoint,
            Refusal::EndpointNotAllowed => Self::EndpointNotAllowed,
            Refusal::NoEndpoint => Self::NoEndpoint,
            Refusal::AppDisabled => Self::AppDisabled,
            Refusal::UnsignedNotAllowed => Self::UnsignedNotAllowed,
            Refusal::InvalidPackage | Refusal::BotChanged => Self::InvalidPackage(message.clone()),
            Refusal::PackageTooLarge => Self::PackageTooLarge,
            Refusal::AppExists => Self::AppExists,
            Refusal::BotExists => Self::BotExists,
            Refusal::UnknownUser => Self::UnknownUser,
            _ => AdminProblem::from(error).into(),
        }
    }
}

/// An own server's apps and bots as the page asks them. Every call blocks until the server answered: the page calls
/// them off the foreground.
pub trait AppAdmin: Send + Sync + 'static {
    /// Every app on the server, by name.
    ///
    /// # Errors
    /// [`AppProblem`], as for every call here.
    fn apps(&self) -> Result<Vec<AdminApp>, AppProblem>;
    /// Registers a bot from the form; its secret comes with it.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn register(&self, form: &BotForm) -> Result<MadeApp, AppProblem>;
    /// Adds the app a Teams app package holds; its bot's secret comes with it.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn upload(&self, package: Vec<u8>) -> Result<MadeApp, AppProblem>;
    /// Changes the app `id`.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn change(&self, id: &str, changes: &AppChanges) -> Result<AdminApp, AppProblem>;
    /// Gives the bot of the app `id` a new secret; the old one stops working.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn new_secret(&self, id: &str) -> Result<String, AppProblem>;
    /// Deletes the app `id`.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn delete(&self, id: &str) -> Result<(), AppProblem>;
    /// A page of the server's accounts whose name or user name holds `query`, after the user name `after`: who may
    /// use an app is chosen among them.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn people(&self, query: &str, after: Option<&str>) -> Result<UserPage, AppProblem>;
    /// What a bot's settings name of the server.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn connection(&self) -> Result<BotConnection, AppProblem>;
    /// Sends the bot of the app `id` a test activity; how the bot answered.
    ///
    /// # Errors
    /// [`AppProblem`].
    fn test_activity(&self, id: &str) -> Result<TestActivity, AppProblem>;
}

/// The app's tile: its first letter on its accent colour (the brand's when it has none).
pub(crate) fn tile(app: &AdminApp, size: f32, palette: Palette) -> gpui::Div {
    let color = app
        .accent_color
        .as_deref()
        .and_then(|color| u32::from_str_radix(color.trim_start_matches('#'), 16).ok())
        .filter(|_| app.accent_color.as_deref().is_some_and(|color| color.len() == 7))
        .unwrap_or(palette.brand);
    let letter: String = app
        .name
        .chars()
        .find(|c| c.is_alphanumeric())
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();
    div()
        .size(px(size))
        .flex_none()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(color))
        .text_color(rgb(0x00ff_ffff))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .when(app.bot.is_some() && letter.is_empty(), |tile| {
            tile.child(Glyph::BOT.icon().size(px(size * 0.5)))
        })
        .child(letter)
}

/// What marks an app in its row, beside its name.
fn marks(app: &AdminApp) -> Vec<(&'static str, Option<u32>)> {
    let mut marks = Vec::new();
    if !app.enabled {
        marks.push(("Off", Some(theme::DANGER)));
    }
    if let Some(bot) = &app.bot {
        if bot.endpoint.is_none() {
            marks.push(("No endpoint", Some(theme::DANGER)));
        }
        if bot.auth_mode == BotAuthMode::Unsigned {
            marks.push(("Unsigned", None));
        }
    }
    match app.health.as_ref().map(|health| health.state) {
        Some(HealthState::Failing) => marks.push(("Not delivering", Some(theme::DANGER))),
        Some(HealthState::Paused) => marks.push(("Paused", Some(theme::DANGER))),
        _ => {}
    }
    if !app.everyone {
        marks.push(("Specific people", None));
    }
    marks
}

/// The Manage apps page (see the module docs).
pub struct ManageApps {
    admin: Arc<dyn AppAdmin>,
    trace: Arc<ViewTrace>,
    search: Entity<InputState>,
    apps: Vec<AdminApp>,
    loading: bool,
    problem: Option<AppProblem>,
    /// The list's cursor row while the list has the keyboard.
    at: usize,
    list_focus: FocusHandle,
    scroll: ScrollHandle,
    dialog: Option<(Entity<AppDialog>, Subscription)>,
    /// Counts the reads, so an older one's answer is dropped.
    read: u64,
    reading: Option<Task<()>>,
    shown: bool,
    _search: Subscription,
}

impl ManageApps {
    pub fn new(admin: Arc<dyn AppAdmin>, trace: Arc<ViewTrace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(SEARCH_PLACEHOLDER));
        let searching = cx.subscribe(&search, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.at = 0;
                cx.notify();
            }
        });
        Self {
            admin,
            trace,
            search,
            apps: Vec::new(),
            loading: false,
            problem: None,
            at: 0,
            list_focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            dialog: None,
            read: 0,
            reading: None,
            shown: false,
            _search: searching,
        }
    }

    /// The apps the server has, by name.
    #[must_use]
    pub fn apps(&self) -> &[AdminApp] {
        &self.apps
    }

    /// The apps the search lets through, in the list's order.
    #[must_use]
    pub fn shown_apps(&self, cx: &App) -> Vec<&AdminApp> {
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.apps
            .iter()
            .filter(|app| {
                query.is_empty()
                    || app.name.to_lowercase().contains(&query)
                    || app.developer.to_lowercase().contains(&query)
            })
            .collect()
    }

    /// What kept the list from being read, if anything.
    #[must_use]
    pub fn problem(&self) -> Option<&AppProblem> {
        self.problem.as_ref()
    }

    /// Whether the list is being read.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading
    }

    /// The dialog open over the page.
    #[must_use]
    pub fn dialog(&self) -> Option<&Entity<AppDialog>> {
        self.dialog.as_ref().map(|(dialog, _)| dialog)
    }

    /// The search's field.
    #[must_use]
    pub fn search(&self) -> &Entity<InputState> {
        &self.search
    }

    /// The page shows: its list is read again each time it comes into sight, as Teams' admin pages do. The list
    /// read before stays until the answer replaces it.
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.shown, true) {
            self.trace.record(&TraceLine::new("app-admin", "shown"));
            self.reload(cx);
        }
    }

    /// The page went out of sight: it reads its list again when it shows again.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.shown, false) {
            cx.notify();
        }
    }

    /// Reads the list again.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.read += 1;
        let read = self.read;
        self.loading = true;
        cx.notify();
        let admin = Arc::clone(&self.admin);
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let apps = cx.background_spawn(async move { admin.apps() }).await;
            let _ = this.update(cx, |this, cx| {
                if this.read == read {
                    this.listed(apps, cx);
                }
            });
        }));
    }

    fn listed(&mut self, apps: Result<Vec<AdminApp>, AppProblem>, cx: &mut Context<Self>) {
        self.loading = false;
        match apps {
            Ok(apps) => {
                self.trace
                    .record(&TraceLine::new("app-admin", "listed").kind(if apps.is_empty() { "none" } else { "some" }));
                let cursor = self.shown_apps(cx).get(self.at).map(|app| app.id.clone());
                self.apps = apps;
                self.problem = None;
                let shown = self.shown_apps(cx);
                self.at = cursor
                    .and_then(|id| shown.iter().position(|app| app.id == id))
                    .unwrap_or(self.at)
                    .min(shown.len().saturating_sub(1));
            }
            Err(problem) => {
                self.trace
                    .record(&TraceLine::new("app-admin", "failed").kind(problem.kind()));
                self.problem = Some(problem);
            }
        }
        cx.notify();
    }

    /// Opens the dialog of `kind` about `app` (none for a new one).
    pub fn open(&mut self, kind: DialogKind, app: Option<AdminApp>, window: &mut Window, cx: &mut Context<Self>) {
        let parts = (Arc::clone(&self.admin), Arc::clone(&self.trace));
        let dialog = cx.new(|cx| AppDialog::new(parts, kind, app, window, cx));
        let events = cx.subscribe_in(
            &dialog,
            window,
            move |this: &mut Self, _, event: &AppDialogEvent, window, cx| match event {
                AppDialogEvent::Changed => this.reload(cx),
                AppDialogEvent::Closed => {
                    this.dialog = None;
                    this.search.update(cx, |search, cx| search.focus(window, cx));
                    this.reload(cx);
                }
            },
        );
        self.dialog = Some((dialog, events));
        cx.notify();
    }

    /// Opens the app at `at` of the list as it shows.
    pub fn open_row(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.shown_apps(cx).get(at).map(|app| (*app).clone());
        if let Some(app) = app {
            self.at = at;
            self.open(DialogKind::Details, Some(app), window, cx);
        }
    }

    fn list_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.shown_apps(cx).len();
        if let Some(to) = keyboard::list_step(event, count, Some(self.at)) {
            cx.stop_propagation();
            self.at = to;
            cx.notify();
        } else if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.open_row(self.at, window, cx);
        }
    }
}

impl ManageApps {
    fn row(
        (at, app): (usize, &AdminApp),
        (lit, narrow): (bool, bool),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = format!("app-row-{}", app.id);
        let detail = match (&app.bot, app.developer.is_empty()) {
            (Some(_), true) => "Bot".to_owned(),
            (Some(_), false) => format!("Bot · {}", app.developer),
            (None, _) => app.developer.clone(),
        };
        let badges = || {
            div().flex().flex_wrap().gap_2().children(
                marks(app)
                    .into_iter()
                    .map(|(words, color)| badge(words, color.unwrap_or(palette.muted_foreground))),
            )
        };
        div()
            .debug_selector(move || selector.clone())
            .id(("app-row", at))
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .border_1()
            .border_color(rgb(palette.surface))
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .map(|row| keyboard::lit(row, lit, palette))
            .child(tile(app, 36.0, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(detail),
                    )
                    .when(narrow, |column| column.child(badges().pt_1())),
            )
            .when(!narrow, |row| row.child(badges().flex_none()))
            .child(
                Glyph::CHEVRON_RIGHT
                    .icon()
                    .size(px(14.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_row(at, window, cx)))
            .into_any_element()
    }

    /// The search, "Register a bot" and "Upload an app", which go under it when they don't fit.
    fn toolbar(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .child(
                div()
                    .debug_selector(|| "apps-search".to_owned())
                    .flex_1()
                    .min_w(px(200.0))
                    .child(Input::new(&self.search).prefix(Glyph::SEARCH.icon().size(px(14.0)))),
            )
            .child(selected_by(
                "apps-register",
                Button::new("apps-register")
                    .primary()
                    .icon(Glyph::BOT.icon())
                    .label("Register a bot")
                    .on_click(cx.listener(|this, _, window, cx| this.open(DialogKind::Register, None, window, cx))),
            ))
            .child(selected_by(
                "apps-upload",
                Button::new("apps-upload")
                    .outline()
                    .icon(Glyph::UPLOAD.icon())
                    .label("Upload an app")
                    .on_click(cx.listener(|this, _, window, cx| this.open(DialogKind::Upload, None, window, cx))),
            ))
    }

    /// What stands in the list's place while it has no rows.
    fn no_rows(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let note = |words: SharedString| {
            div()
                .p_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(words)
        };
        if let Some(problem) = &self.problem {
            return note(problem.words())
                .debug_selector(|| "apps-problem".to_owned())
                .child(selected_by(
                    "apps-retry",
                    Button::new("apps-retry")
                        .outline()
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                ))
                .into_any_element();
        }
        if self.loading && self.apps.is_empty() {
            return note("Loading apps…".into())
                .debug_selector(|| "apps-loading".to_owned())
                .child(Spinner::new().with_size(Size::Medium))
                .into_any_element();
        }
        if self.apps.is_empty() {
            return note("No apps yet. Register a bot, or upload a Teams app package.".into())
                .debug_selector(|| "apps-empty".to_owned())
                .child(
                    div()
                        .size(px(40.0))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(palette.hover))
                        .child(Glyph::BOT.icon().size(px(20.0)).text_color(rgb(palette.brand))),
                )
                .into_any_element();
        }
        note("No apps match your search.".into())
            .debug_selector(|| "apps-none".to_owned())
            .into_any_element()
    }

    fn list(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let shown: Vec<AdminApp> = self.shown_apps(cx).into_iter().cloned().collect();
        if shown.is_empty() || self.problem.is_some() {
            return self.no_rows(palette, cx);
        }
        let focused = self.list_focus.is_focused(window);
        let narrow = crate::settings_view::narrow(window);
        let rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(at, app)| Self::row((at, app), (focused && at == self.at, narrow), palette, cx))
            .collect();
        div()
            .debug_selector(|| "apps-list".to_owned())
            .id("apps-list")
            .track_focus(&self.list_focus.clone().tab_stop(true))
            .on_key_down(cx.listener(Self::list_key))
            .p_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .children(rows)
            .into_any_element()
    }
}

impl Render for ManageApps {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.show(cx);
        let column = crate::settings_view::page_column(crate::settings_view::Page::Apps.title(), window).child(
            div()
                .debug_selector(|| "manage-apps".to_owned())
                .flex()
                .flex_col()
                .gap_4()
                .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(
                    "Register the bots and apps people on this server can use, and choose who can use each one.",
                ))
                .child(self.toolbar(cx))
                .child(self.list(palette, window, cx)),
        );
        div()
            .id("apps-page")
            .debug_selector(|| "settings".to_owned())
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(crate::settings_view::page_in(
                column,
                crate::settings_view::Page::Apps,
                cx,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_problem_has_words_and_a_kind() {
        let problems = [
            AppProblem::InvalidName,
            AppProblem::InvalidEndpoint,
            AppProblem::UnsignedNotAllowed,
            AppProblem::InvalidPackage(String::new()),
            AppProblem::InvalidPackage("no manifest.json".to_owned()),
            AppProblem::PackageTooLarge,
            AppProblem::AppExists,
            AppProblem::BotExists,
            AppProblem::UnknownUser,
            AppProblem::NotFound,
            AppProblem::File("gone".to_owned()),
            AppProblem::Account(AdminProblem::SignedOut),
            AppProblem::Account(AdminProblem::NotAdmin),
            AppProblem::EndpointNotAllowed,
            AppProblem::NoEndpoint,
            AppProblem::AppDisabled,
        ];
        for problem in &problems {
            assert!(!problem.words().is_empty() && !problem.kind().is_empty(), "{problem:?}");
            assert_eq!(problem.to_string(), problem.words().to_string());
        }
        assert!(problems[12].not_admin() && !problems[11].not_admin());
        assert!(problems[4].words().contains("no manifest.json"));
    }

    #[test]
    fn the_servers_refusals_become_the_pages_problems() {
        let refused = |refusal, message: &str| AdminError::Refused {
            refusal,
            status: 400,
            message: message.to_owned(),
        };
        let cases = [
            (refused(Refusal::InvalidEndpoint, ""), AppProblem::InvalidEndpoint),
            (refused(Refusal::EndpointNotAllowed, ""), AppProblem::EndpointNotAllowed),
            (refused(Refusal::NoEndpoint, ""), AppProblem::NoEndpoint),
            (refused(Refusal::AppDisabled, ""), AppProblem::AppDisabled),
            (refused(Refusal::UnsignedNotAllowed, ""), AppProblem::UnsignedNotAllowed),
            (
                refused(Refusal::InvalidPackage, "bad"),
                AppProblem::InvalidPackage("bad".to_owned()),
            ),
            (
                refused(Refusal::BotChanged, "bot"),
                AppProblem::InvalidPackage("bot".to_owned()),
            ),
            (refused(Refusal::PackageTooLarge, ""), AppProblem::PackageTooLarge),
            (refused(Refusal::AppExists, ""), AppProblem::AppExists),
            (refused(Refusal::BotExists, ""), AppProblem::BotExists),
            (refused(Refusal::UnknownUser, ""), AppProblem::UnknownUser),
            (refused(Refusal::InvalidName, ""), AppProblem::InvalidName),
            (refused(Refusal::NotFound, ""), AppProblem::NotFound),
            (
                refused(Refusal::NotAdmin, ""),
                AppProblem::Account(AdminProblem::NotAdmin),
            ),
            (AdminError::Unauthorized, AppProblem::Account(AdminProblem::SignedOut)),
        ];
        for (error, expected) in cases {
            assert_eq!(AppProblem::from(error), expected);
        }
        let not_served = AppProblem::from(AdminError::UnexpectedResponse { status: 404 });
        assert!(not_served.words().contains("doesn't serve apps"));
    }

    #[test]
    fn an_app_is_marked_by_what_needs_attention() {
        let app = AdminApp {
            id: "a".to_owned(),
            version: String::new(),
            name: "Notes".to_owned(),
            short_description: String::new(),
            developer: String::new(),
            accent_color: Some("#4f52b2".to_owned()),
            source: teams_auth::admin::AppSource::Form,
            enabled: false,
            everyone: false,
            users: Vec::new(),
            bot: Some(teams_auth::admin::AdminBot {
                id: "a".to_owned(),
                mri: "28:a".to_owned(),
                endpoint: None,
                auth_mode: BotAuthMode::Unsigned,
                scopes: Vec::new(),
            }),
            health: Some(teams_auth::admin::BotHealth {
                state: HealthState::Failing,
                failures: 3,
                paused_until: None,
                last_error: None,
                last_success: None,
                pending: 0,
                sent: 0,
                failed: 1,
            }),
            created_at: 0,
            updated_at: 0,
        };
        let words: Vec<&str> = marks(&app).into_iter().map(|(words, _)| words).collect();
        assert_eq!(
            words,
            ["Off", "No endpoint", "Unsigned", "Not delivering", "Specific people"]
        );
        let fine = AdminApp {
            enabled: true,
            everyone: true,
            bot: None,
            health: None,
            ..app
        };
        assert!(marks(&fine).is_empty());
    }
}
