//! A dialog over Manage apps: what it holds, what it asks the [`AppAdmin`] and how it goes from one step to the next
//! (a bot registered shows its secret, then its SDK settings; an app's details lead to who may use it, a new secret,
//! the settings or its deletion, and back). How each step is drawn is in `forms.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, PathPromptOptions, SharedString,
    Subscription, Task, WeakEntity, Window,
};
use gpui_component::input::{InputEvent, InputState};
use teams_auth::admin::{AdminApp, AdminUser, AppChanges, BotAuthMode, MAX_PACKAGE, MadeApp, TestActivity};

use super::sdk::Sdk;
use super::{AppAdmin, AppProblem, BotConnection, BotForm, MAX_BOT_NAME};
use crate::trace::{TraceLine, ViewTrace};

/// How long the people search waits after the typing.
const PAUSE: Duration = Duration::from_millis(250);
/// The most pages of accounts read to name the people an app's list holds.
const NAMING_PAGES: usize = 20;

/// What a dialog shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    /// The form that registers a bot.
    Register,
    /// A Teams app package chosen and uploaded.
    Upload,
    /// What was just made, its app id and its bot's secret, shown once.
    Made,
    /// An app: on or off, its bot's endpoint and mode, and its other actions.
    Details,
    /// Who may use an app.
    Access,
    /// A new secret for an app's bot, confirmed first.
    NewSecret,
    /// An app's deletion, confirmed first.
    Delete,
    /// The settings each SDK takes for the bot.
    Settings,
}

impl DialogKind {
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Upload => "upload",
            Self::Made => "made",
            Self::Details => "details",
            Self::Access => "access",
            Self::NewSecret => "new-secret",
            Self::Delete => "delete",
            Self::Settings => "settings",
        }
    }
}

/// What a dialog tells the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppDialogEvent {
    /// The dialog closed: the list is read again.
    Closed,
    /// Something changed on the server while the dialog stays: the list behind it is read again.
    Changed,
}

/// What made the secret the dialog shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Made {
    Registered,
    Uploaded,
    NewSecret,
}

/// What the details and access steps hold of an app before they are saved.
#[derive(Clone, Copy, Debug)]
pub(super) struct Choices {
    /// People can use the app.
    pub(super) enabled: bool,
    /// Every account may use it, not only the app's list.
    pub(super) everyone: bool,
}

/// A chosen package: its file's name and its bytes.
pub(super) struct Package {
    pub(super) name: String,
    pub(super) bytes: Vec<u8>,
}

/// A dialog over the page (see the module docs).
pub struct AppDialog {
    pub(super) admin: Arc<dyn AppAdmin>,
    pub(super) trace: Arc<ViewTrace>,
    pub(super) kind: DialogKind,
    /// Where Cancel goes back to, when the dialog came from an app's details.
    pub(super) back: Option<DialogKind>,
    /// The app it is about; none for a new one until it is made.
    pub(super) app: Option<AdminApp>,
    pub(super) name: Entity<InputState>,
    pub(super) description: Entity<InputState>,
    pub(super) endpoint: Entity<InputState>,
    pub(super) people_search: Entity<InputState>,
    pub(super) mode: BotAuthMode,
    pub(super) choices: Choices,
    pub(super) users: Vec<String>,
    /// The accounts the dialog knows by id: those the app lists and those the search found.
    pub(super) known: Vec<AdminUser>,
    /// What the people search found.
    pub(super) found: Vec<AdminUser>,
    pub(super) package: Option<Package>,
    /// The secret just made, shown once, and what made it.
    pub(super) secret: Option<String>,
    pub(super) made: Option<Made>,
    pub(super) sdk: Sdk,
    pub(super) connection: Option<Result<BotConnection, AppProblem>>,
    /// How the bot answered the last test activity of this dialog.
    pub(super) test: Option<Result<TestActivity, AppProblem>>,
    /// The test activity under way.
    pub(super) testing: Option<Task<()>>,
    pub(super) busy: bool,
    pub(super) problem: Option<SharedString>,
    /// A confirmation takes the keyboard to its first button (Cancel) once it is drawn.
    pub(super) focus_first_button: bool,
    /// The dialog itself takes the keyboard at the next draw (the field that had it is gone).
    pub(super) focus_dialog: bool,
    pub(super) working: Option<Task<()>>,
    /// The people search, read again as the search changes.
    pub(super) searching: Option<Task<()>>,
    /// The names of the app's people, or the server's addresses: apart from the search, which must not stop them.
    pub(super) reading: Option<Task<()>>,
    pub(super) focus: FocusHandle,
    _inputs: Vec<Subscription>,
}

impl EventEmitter<AppDialogEvent> for AppDialog {}

impl AppDialog {
    /// The dialog of `kind` about `app`.
    pub fn new(
        (admin, trace): (Arc<dyn AppAdmin>, Arc<ViewTrace>),
        kind: DialogKind,
        app: Option<AdminApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let bot = app.as_ref().and_then(|app| app.bot.as_ref());
        let endpoint_value = bot.and_then(|bot| bot.endpoint.clone()).unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Made-up Helper"));
        let description = cx.new(|cx| InputState::new(window, cx).placeholder("What the bot does"));
        let endpoint = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://bot.example.com/api/messages")
                .default_value(endpoint_value)
        });
        let people_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search people"));
        let mut inputs: Vec<Subscription> = [&name, &description, &endpoint]
            .map(|field| {
                cx.subscribe(field, |this: &mut Self, _, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    InputEvent::Change if this.problem.is_some() => {
                        this.problem = None;
                        cx.notify();
                    }
                    _ => {}
                })
            })
            .into();
        inputs.push(
            cx.subscribe(&people_search, |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search_people(PAUSE, cx);
                }
            }),
        );
        let mut dialog = Self {
            admin,
            trace,
            kind,
            back: None,
            mode: bot.map_or(BotAuthMode::Signed, |bot| bot.auth_mode),
            choices: Choices {
                enabled: app.as_ref().is_none_or(|app| app.enabled),
                everyone: app.as_ref().is_none_or(|app| app.everyone),
            },
            users: app.as_ref().map(|app| app.users.clone()).unwrap_or_default(),
            app,
            name,
            description,
            endpoint,
            people_search,
            known: Vec::new(),
            found: Vec::new(),
            package: None,
            secret: None,
            made: None,
            sdk: Sdk::DotNet,
            connection: None,
            test: None,
            testing: None,
            busy: false,
            problem: None,
            focus_first_button: false,
            focus_dialog: false,
            working: None,
            searching: None,
            reading: None,
            focus: cx.focus_handle(),
            _inputs: inputs,
        };
        dialog.go(kind, window, cx);
        dialog
    }

    #[must_use]
    pub fn kind(&self) -> DialogKind {
        self.kind
    }

    /// What the dialog says went wrong, if anything.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// The app it is about, as the server last answered it.
    #[must_use]
    pub fn app(&self) -> Option<&AdminApp> {
        self.app.as_ref()
    }

    /// Whether the secret just made shows.
    #[must_use]
    pub fn shows_secret(&self) -> bool {
        self.secret.is_some() && self.kind == DialogKind::Made
    }

    /// The SDK whose settings show.
    #[must_use]
    pub fn sdk(&self) -> Sdk {
        self.sdk
    }

    /// The account ids the app's list holds as the dialog has it.
    #[must_use]
    pub fn users(&self) -> &[String] {
        &self.users
    }

    /// What the people search found.
    #[must_use]
    pub fn found(&self) -> &[AdminUser] {
        &self.found
    }

    /// The settings of the chosen SDK, once the server's addresses were read.
    #[must_use]
    pub fn settings(&self) -> Option<super::sdk::SdkSettings> {
        let connection = self.connection.as_ref()?.as_ref().ok()?;
        let app = self.app.as_ref()?;
        let bot = app.bot.as_ref()?;
        let values = super::sdk::BotValues {
            app_id: &bot.id,
            secret: self.secret.as_deref(),
            mode: bot.auth_mode,
        };
        Some(super::sdk::settings(self.sdk, connection, values))
    }

    /// Shows the step `kind`, its field taking the keyboard.
    pub fn go(&mut self, kind: DialogKind, window: &mut Window, cx: &mut Context<Self>) {
        self.back = match (self.kind, kind) {
            (
                DialogKind::Details,
                DialogKind::Access | DialogKind::NewSecret | DialogKind::Delete | DialogKind::Settings,
            ) => Some(DialogKind::Details),
            (DialogKind::Made, DialogKind::Settings) => Some(DialogKind::Made),
            (_, DialogKind::Details | DialogKind::Made | DialogKind::Register | DialogKind::Upload) => None,
            (_, _) => self.back,
        };
        self.kind = kind;
        self.problem = None;
        self.trace
            .record(&TraceLine::new("app-admin", "dialog").kind(kind.key()));
        match kind {
            DialogKind::Register => self.name.update(cx, |field, cx| field.focus(window, cx)),
            DialogKind::Details if self.app.as_ref().is_some_and(|app| app.bot.is_some()) => {
                self.endpoint.update(cx, |field, cx| field.focus(window, cx));
            }
            DialogKind::Access => {
                self.people_search.update(cx, |field, cx| field.focus(window, cx));
                self.name_people(cx);
            }
            DialogKind::Settings => {
                self.focus.focus(window, cx);
                self.read_connection(cx);
            }
            DialogKind::NewSecret | DialogKind::Delete => {
                self.focus.focus(window, cx);
                self.focus_first_button = true;
            }
            _ => self.focus.focus(window, cx),
        }
        cx.notify();
    }

    /// Cancel: back to the app's details when the step came from there, else the dialog closes.
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.back {
            Some(back) => {
                if back == DialogKind::Details {
                    self.reset_to_app();
                }
                self.go(back, window, cx);
            }
            None => cx.emit(AppDialogEvent::Closed),
        }
    }

    /// The choices of the details and the access steps as the app has them, after a Cancel.
    fn reset_to_app(&mut self) {
        if let Some(app) = &self.app {
            self.choices.enabled = app.enabled;
            self.choices.everyone = app.everyone;
            self.users.clone_from(&app.users);
            self.mode = app.bot.as_ref().map_or(BotAuthMode::Signed, |bot| bot.auth_mode);
        }
    }

    pub fn set_mode(&mut self, mode: BotAuthMode, cx: &mut Context<Self>) {
        self.mode = mode;
        self.problem = None;
        cx.notify();
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.choices.enabled = enabled;
        cx.notify();
    }

    pub fn set_everyone(&mut self, everyone: bool, cx: &mut Context<Self>) {
        self.choices.everyone = everyone;
        self.problem = None;
        cx.notify();
    }

    pub fn set_sdk(&mut self, sdk: Sdk, cx: &mut Context<Self>) {
        self.sdk = sdk;
        cx.notify();
    }

    /// Adds the account to the app's list.
    pub fn add_person(&mut self, person: &AdminUser, cx: &mut Context<Self>) {
        if !self.users.contains(&person.id) {
            self.users.push(person.id.clone());
        }
        if !self.known.iter().any(|known| known.id == person.id) {
            self.known.push(person.clone());
        }
        self.problem = None;
        cx.notify();
    }

    /// Takes the account `id` off the app's list.
    pub fn remove_person(&mut self, id: &str, cx: &mut Context<Self>) {
        self.users.retain(|user| user != id);
        cx.notify();
    }

    /// Fills the register form's fields.
    pub fn fill(&mut self, name: &str, description: &str, endpoint: &str, window: &mut Window, cx: &mut Context<Self>) {
        for (field, value) in [
            (&self.name, name),
            (&self.description, description),
            (&self.endpoint, endpoint),
        ] {
            let value = value.to_owned();
            field.update(cx, |field, cx| field.set_value(value, window, cx));
        }
        // a value set from code sends no change: the problem it answered goes here
        self.problem = None;
        cx.notify();
    }

    /// Types `query` into the people search.
    pub fn search_for(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.to_owned();
        self.people_search
            .update(cx, |field, cx| field.set_value(query, window, cx));
        self.search_people(Duration::ZERO, cx);
    }

    /// "Choose file": a Teams app package picked from the disk.
    pub fn browse(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
            ..PathPromptOptions::default()
        });
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let path: Option<PathBuf> = picked
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            let Some(path) = path else {
                return;
            };
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let read = cx.background_spawn(async move { read_package(&path) }).await;
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                match read {
                    Ok(bytes) => this.choose_package(name, bytes, cx),
                    Err(problem) => this.say_problem(&problem, cx),
                }
            });
        }));
    }

    /// The package `bytes` of the file `name` chosen for the upload.
    pub fn choose_package(&mut self, name: String, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.len() > MAX_PACKAGE {
            self.say_problem(&AppProblem::PackageTooLarge, cx);
            return;
        }
        self.package = Some(Package { name, bytes });
        self.problem = None;
        cx.notify();
    }

    fn say_problem(&mut self, problem: &AppProblem, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("app-admin", "failed").kind(problem.kind()));
        self.problem = Some(problem.words());
        cx.notify();
    }

    fn value(field: &Entity<InputState>, cx: &App) -> String {
        field.read(cx).value().trim().to_owned()
    }

    /// What changed of the app on its details step.
    pub(super) fn changes(&self, cx: &App) -> AppChanges {
        let Some(app) = &self.app else {
            return AppChanges::default();
        };
        let mut changes = AppChanges {
            enabled: (self.choices.enabled != app.enabled).then_some(self.choices.enabled),
            ..AppChanges::default()
        };
        if let Some(bot) = &app.bot {
            let endpoint = Self::value(&self.endpoint, cx);
            changes.endpoint = (endpoint != bot.endpoint.clone().unwrap_or_default()).then_some(endpoint);
            changes.auth_mode = (self.mode != bot.auth_mode).then_some(self.mode);
        }
        changes
    }

    /// What changed of who may use the app on its access step.
    pub(super) fn access_changes(&self) -> AppChanges {
        let Some(app) = &self.app else {
            return AppChanges::default();
        };
        let users_changed = !self.choices.everyone && self.users != app.users;
        AppChanges {
            everyone: (self.choices.everyone != app.everyone).then_some(self.choices.everyone),
            users: users_changed.then(|| self.users.clone()),
            ..AppChanges::default()
        }
    }

    /// What keeps the step from being sent, said under it, and its name in the trace.
    fn form_problem(&self, cx: &App) -> Option<(SharedString, &'static str)> {
        match self.kind {
            DialogKind::Register => {
                let name = Self::value(&self.name, cx);
                let endpoint = Self::value(&self.endpoint, cx);
                if name.is_empty() || name.chars().count() > MAX_BOT_NAME {
                    Some((AppProblem::InvalidName.words(), "invalid-name"))
                } else if !endpoint.is_empty() && !endpoint.starts_with("https://") && !endpoint.starts_with("http://")
                {
                    Some((AppProblem::InvalidEndpoint.words(), "invalid-endpoint"))
                } else {
                    None
                }
            }
            DialogKind::Upload if self.package.is_none() => {
                Some(("Choose a Teams app package (.zip) first.".into(), "no-package"))
            }
            DialogKind::Access if !self.choices.everyone && self.users.is_empty() => Some((
                "Choose at least one person, or let everyone use the app.".into(),
                "no-people",
            )),
            _ => None,
        }
    }

    /// Whether the confirming button is on.
    #[must_use]
    pub fn can_confirm(&self, cx: &App) -> bool {
        !self.busy
            && match self.kind {
                DialogKind::Details => !self.changes(cx).is_empty(),
                DialogKind::Access => !self.access_changes().is_empty(),
                DialogKind::Upload => self.package.is_some(),
                _ => true,
            }
    }

    /// The confirming button, or Enter in a field: the step's change goes to the server.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) {
            return;
        }
        if let Some((words, kind)) = self.form_problem(cx) {
            self.trace.record(&TraceLine::new("app-admin", "failed").kind(kind));
            self.problem = Some(words);
            cx.notify();
            return;
        }
        let Some(asked) = self.ask(cx) else {
            // a step that only shows something: its button closes the dialog
            cx.emit(AppDialogEvent::Closed);
            return;
        };
        self.busy = true;
        self.problem = None;
        cx.notify();
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = asked.await;
            let _ = this.update(cx, |this, cx| this.answered(answer, cx));
        }));
    }

    /// The step's request, off the foreground; none for a step that only shows something.
    fn ask(&mut self, cx: &mut Context<Self>) -> Option<Task<Result<Answer, AppProblem>>> {
        let admin = Arc::clone(&self.admin);
        let id = self.app.as_ref().map(|app| app.id.clone()).unwrap_or_default();
        Some(match self.kind {
            DialogKind::Register => {
                let endpoint = Self::value(&self.endpoint, cx);
                let form = BotForm {
                    name: Self::value(&self.name, cx),
                    description: Self::value(&self.description, cx),
                    endpoint: (!endpoint.is_empty()).then_some(endpoint),
                    auth_mode: self.mode,
                };
                cx.background_spawn(async move { admin.register(&form).map(Answer::Made) })
            }
            DialogKind::Upload => {
                let bytes = self
                    .package
                    .as_ref()
                    .map(|package| package.bytes.clone())
                    .unwrap_or_default();
                cx.background_spawn(async move { admin.upload(bytes).map(Answer::Made) })
            }
            DialogKind::Details | DialogKind::Access => {
                let changes = if self.kind == DialogKind::Details {
                    self.changes(cx)
                } else {
                    self.access_changes()
                };
                cx.background_spawn(async move { admin.change(&id, &changes).map(Answer::Changed) })
            }
            DialogKind::NewSecret => cx.background_spawn(async move { admin.new_secret(&id).map(Answer::Secret) }),
            DialogKind::Delete => cx.background_spawn(async move { admin.delete(&id).map(|()| Answer::Deleted) }),
            DialogKind::Made | DialogKind::Settings => return None,
        })
    }

    fn answered(&mut self, answer: Result<Answer, AppProblem>, cx: &mut Context<Self>) {
        self.working = None;
        self.busy = false;
        let kind = self.kind;
        match answer {
            Ok(answer) => {
                self.trace.record(&TraceLine::new("app-admin", "done").kind(kind.key()));
                self.took(answer, cx);
            }
            Err(problem) => {
                if problem == AppProblem::NotFound && kind != DialogKind::Register {
                    // gone meanwhile: the list behind says so as it is read again
                    cx.emit(AppDialogEvent::Changed);
                }
                self.say_problem(&problem, cx);
            }
        }
    }

    fn took(&mut self, answer: Answer, cx: &mut Context<Self>) {
        match answer {
            Answer::Made(made) => {
                self.made = Some(if self.kind == DialogKind::Upload {
                    Made::Uploaded
                } else {
                    Made::Registered
                });
                self.secret = made.secret;
                self.mode = made.app.bot.as_ref().map_or(BotAuthMode::Signed, |bot| bot.auth_mode);
                self.app = Some(made.app);
                self.show_made(cx);
            }
            Answer::Secret(secret) => {
                self.made = Some(Made::NewSecret);
                self.secret = Some(secret);
                self.show_made(cx);
            }
            Answer::Changed(app) if self.kind == DialogKind::Access => {
                // back to the app's details, which show the new list
                self.app = Some(app);
                self.reset_to_app();
                self.kind = DialogKind::Details;
                self.back = None;
                self.focus_dialog = true;
                cx.emit(AppDialogEvent::Changed);
                cx.notify();
            }
            Answer::Changed(app) => {
                self.app = Some(app);
                cx.emit(AppDialogEvent::Closed);
            }
            Answer::Deleted => cx.emit(AppDialogEvent::Closed),
        }
    }

    /// The step that shows what was just made, the list behind read again.
    fn show_made(&mut self, cx: &mut Context<Self>) {
        self.kind = DialogKind::Made;
        self.back = None;
        self.focus_dialog = true;
        self.trace
            .record(&TraceLine::new("app-admin", "dialog").kind(DialogKind::Made.key()));
        cx.emit(AppDialogEvent::Changed);
        cx.notify();
    }

    /// How the bot answered the last test activity, once it answered.
    #[must_use]
    pub fn test_result(&self) -> Option<&Result<TestActivity, AppProblem>> {
        self.test.as_ref()
    }

    /// Whether a test activity is under way.
    #[must_use]
    pub fn testing(&self) -> bool {
        self.testing.is_some()
    }

    /// Sends the app's bot a test activity; how the bot answered shows in the details.
    pub fn send_test(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self
            .app
            .as_ref()
            .filter(|app| app.bot.is_some())
            .map(|app| app.id.clone())
        else {
            return;
        };
        if self.testing.is_some() {
            return;
        }
        self.test = None;
        self.trace.record(&TraceLine::new("app-admin", "test"));
        let admin = Arc::clone(&self.admin);
        self.testing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let went = cx.background_spawn(async move { admin.test_activity(&id) }).await;
            let _ = this.update(cx, |this, cx| {
                let kind = match &went {
                    Ok(test) if test.delivered => "delivered",
                    Ok(_) => "not-delivered",
                    Err(problem) => problem.kind(),
                };
                this.trace.record(&TraceLine::new("app-admin", "tested").kind(kind));
                this.test = Some(went);
                this.testing = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Reads the server's addresses a bot's settings name, once.
    fn read_connection(&mut self, cx: &mut Context<Self>) {
        if self.connection.as_ref().is_some_and(Result::is_ok) {
            return;
        }
        self.connection = None;
        let admin = Arc::clone(&self.admin);
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let read = cx.background_spawn(async move { admin.connection() }).await;
            let _ = this.update(cx, |this, cx| {
                this.connection = Some(read);
                cx.notify();
            });
        }));
    }

    /// Names the people the app's list holds: the server's accounts, read page by page until each is found.
    fn name_people(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<String> = self
            .users
            .iter()
            .filter(|id| !self.known.iter().any(|known| &known.id == *id))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        let admin = Arc::clone(&self.admin);
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let named = cx
                .background_spawn(async move { named_among(admin.as_ref(), &wanted) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.known.extend(named);
                cx.notify();
            });
        }));
    }

    /// Reads the people the search holds after `pause`; an empty search finds no one.
    fn search_people(&mut self, pause: Duration, cx: &mut Context<Self>) {
        let query = Self::value(&self.people_search, cx);
        if query.is_empty() {
            self.found.clear();
            self.searching = None;
            cx.notify();
            return;
        }
        let admin = Arc::clone(&self.admin);
        self.searching = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(pause).await;
            let page = cx.background_spawn(async move { admin.people(&query, None) }).await;
            let _ = this.update(cx, |this, cx| {
                match page {
                    Ok(page) => this.found = page.users,
                    Err(problem) => this.say_problem(&problem, cx),
                }
                cx.notify();
            });
        }));
    }

    /// The name the account `id` is shown by: its display name, its user name, or the id itself.
    pub(super) fn person_name(&self, id: &str) -> (String, Option<String>) {
        self.known.iter().find(|known| known.id == id).map_or_else(
            || (id.to_owned(), None),
            |known| {
                let name = if known.display_name.is_empty() {
                    known.upn.clone()
                } else {
                    known.display_name.clone()
                };
                (name, Some(known.upn.clone()))
            },
        )
    }
}

/// The accounts of `wanted` among the server's, read page by page until each is found (at most [`NAMING_PAGES`]).
fn named_among(admin: &dyn AppAdmin, wanted: &[String]) -> Vec<AdminUser> {
    let mut named = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..NAMING_PAGES {
        let Ok(page) = admin.people("", after.as_deref()) else {
            break;
        };
        named.extend(page.users.into_iter().filter(|user| wanted.contains(&user.id)));
        after = page.next;
        if after.is_none() || named.len() == wanted.len() {
            break;
        }
    }
    named
}

/// What the server answered a step.
enum Answer {
    Made(MadeApp),
    Secret(String),
    Changed(AdminApp),
    Deleted,
}

/// A package file's bytes, refused when it is over the server's limit.
fn read_package(path: &std::path::Path) -> Result<Vec<u8>, AppProblem> {
    let size = std::fs::metadata(path)
        .map_err(|error| AppProblem::File(error.kind().to_string()))?
        .len();
    if usize::try_from(size).map_or(true, |size| size > MAX_PACKAGE) {
        return Err(AppProblem::PackageTooLarge);
    }
    std::fs::read(path).map_err(|error| AppProblem::File(error.kind().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_file_is_read_unless_it_is_too_large_or_gone() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.zip");
        std::fs::write(&small, b"PK made-up").unwrap();
        assert_eq!(read_package(&small).unwrap(), b"PK made-up");
        let large = dir.path().join("large.zip");
        let file = std::fs::File::create(&large).unwrap();
        file.set_len(u64::try_from(MAX_PACKAGE).unwrap() + 1).unwrap();
        assert_eq!(read_package(&large), Err(AppProblem::PackageTooLarge));
        assert!(matches!(
            read_package(&dir.path().join("gone.zip")),
            Err(AppProblem::File(_))
        ));
    }
}
