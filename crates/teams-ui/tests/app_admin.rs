//! Manage apps (`teams_ui::app_admin`) in the settings, on GPUI's test context over a made-up server: the page shows
//! only for an account that has the service and only with `NGTEAMS_BOTS=1`; its list and search; registering a bot
//! (its id and secret shown once, then its SDK settings), uploading a package, turning an app off, changing its
//! endpoint and mode and who may use it, a new secret and a deletion, what each asks of the service and what a refusal
//! says; the trace; the page going once the account is no administrator any more.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use teams_auth::admin::{
    AdminApp, AdminBot, AdminUser, AppChanges, AppSource, BotAuthMode, BotHealth, HealthState, MadeApp, TestActivity,
    UserPage,
};
use teams_ui::app_admin::sdk::Sdk;
use teams_ui::app_admin::{AppAdmin, AppDialog, AppProblem, BotConnection, BotForm, DialogKind, ManageApps};
use teams_ui::bots_flag::BotsFlag;
use teams_ui::settings::{SettingsFile, SettingsModel};
use teams_ui::settings_view::{Page, SettingsView};
use teams_ui::user_admin::AdminProblem;
use teams_ui::{ViewTrace, theme};

const ME: &str = "8:orgid:00000000-0000-4000-8000-0000000000a1";
const NOTES: &str = "00000000-0000-4000-8000-0000000000b1";
const SECRET: &str = "made-up-secret-1";
const DANA: &str = "00000000-0000-4000-8000-0000000000a2";

fn app(id: &str, name: &str, bot: bool) -> AdminApp {
    AdminApp {
        id: id.to_owned(),
        version: "1.0.0".to_owned(),
        name: name.to_owned(),
        short_description: "A made-up app".to_owned(),
        developer: "Made-up Maker".to_owned(),
        accent_color: Some("#4f52b2".to_owned()),
        source: AppSource::Form,
        enabled: true,
        everyone: true,
        users: Vec::new(),
        bot: bot.then(|| AdminBot {
            id: id.to_owned(),
            mri: format!("28:{id}"),
            endpoint: Some("https://bots.example.test/api/messages".to_owned()),
            auth_mode: BotAuthMode::Signed,
            scopes: vec!["personal".to_owned()],
        }),
        health: None,
        created_at: 1,
        updated_at: 2,
    }
}

fn person(id: &str, upn: &str, name: &str) -> AdminUser {
    AdminUser {
        id: id.to_owned(),
        upn: upn.to_owned(),
        display_name: name.to_owned(),
        enabled: true,
        admin: false,
        must_change_password: false,
    }
}

/// A made-up server: its apps, its people, what it was asked, and a refusal for the next change.
#[derive(Default)]
struct Server {
    apps: Mutex<Vec<AdminApp>>,
    asked: Mutex<Vec<String>>,
    refuse: Mutex<Option<AppProblem>>,
    made: Mutex<u32>,
    /// How the bot answers the next test activity; it takes it unless set.
    test: Mutex<Option<TestActivity>>,
}

impl Server {
    fn with(apps: Vec<AdminApp>) -> Arc<Self> {
        Arc::new(Self {
            apps: Mutex::new(apps),
            ..Self::default()
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn changes(&self) -> Vec<String> {
        self.asked()
            .into_iter()
            .filter(|asked| asked != "apps" && !asked.starts_with("people") && asked != "connection")
            .collect()
    }

    fn refuse_next(&self, problem: AppProblem) {
        *self.refuse.lock().unwrap() = Some(problem);
    }

    fn note(&self, asked: String) -> Result<(), AppProblem> {
        self.asked.lock().unwrap().push(asked);
        self.refuse.lock().unwrap().take().map_or(Ok(()), Err)
    }

    fn app(&self, id: &str) -> Option<AdminApp> {
        self.apps.lock().unwrap().iter().find(|app| app.id == id).cloned()
    }

    fn made(&self, mut made: AdminApp) -> MadeApp {
        let mut count = self.made.lock().unwrap();
        *count += 1;
        made.id = format!("00000000-0000-4000-8000-00000000c00{count}");
        if let Some(bot) = &mut made.bot {
            bot.id.clone_from(&made.id);
        }
        self.apps.lock().unwrap().push(made.clone());
        MadeApp {
            secret: made.bot.as_ref().map(|_| format!("made-up-secret-{count}")),
            app: made,
        }
    }
}

impl AppAdmin for Server {
    fn apps(&self) -> Result<Vec<AdminApp>, AppProblem> {
        self.note("apps".to_owned())?;
        Ok(self.apps.lock().unwrap().clone())
    }

    fn register(&self, form: &BotForm) -> Result<MadeApp, AppProblem> {
        self.note(format!(
            "register {} desc={} endpoint={} mode={}",
            form.name,
            form.description,
            form.endpoint.as_deref().unwrap_or("-"),
            form.auth_mode.wire()
        ))?;
        let mut made = app("", &form.name, true);
        if let Some(bot) = &mut made.bot {
            bot.endpoint.clone_from(&form.endpoint);
            bot.auth_mode = form.auth_mode;
        }
        Ok(self.made(made))
    }

    fn upload(&self, package: Vec<u8>) -> Result<MadeApp, AppProblem> {
        self.note(format!("upload {} bytes", package.len()))?;
        let mut made = app("", "Made-up Package", true);
        made.source = AppSource::Package;
        Ok(self.made(made))
    }

    fn change(&self, id: &str, changes: &AppChanges) -> Result<AdminApp, AppProblem> {
        self.note(format!("change {id} {changes:?}"))?;
        let mut apps = self.apps.lock().unwrap();
        let app = apps.iter_mut().find(|app| app.id == id).ok_or(AppProblem::NotFound)?;
        if let Some(enabled) = changes.enabled {
            app.enabled = enabled;
        }
        if let Some(everyone) = changes.everyone {
            app.everyone = everyone;
        }
        if let Some(users) = &changes.users {
            app.users.clone_from(users);
        }
        if let Some(bot) = &mut app.bot {
            if let Some(endpoint) = &changes.endpoint {
                bot.endpoint = Some(endpoint.clone());
            }
            if let Some(mode) = changes.auth_mode {
                bot.auth_mode = mode;
            }
        }
        Ok(app.clone())
    }

    fn new_secret(&self, id: &str) -> Result<String, AppProblem> {
        self.note(format!("new-secret {id}"))?;
        Ok("made-up-rotated-secret".to_owned())
    }

    fn delete(&self, id: &str) -> Result<(), AppProblem> {
        self.note(format!("delete {id}"))?;
        self.apps.lock().unwrap().retain(|app| app.id != id);
        Ok(())
    }

    fn people(&self, query: &str, after: Option<&str>) -> Result<UserPage, AppProblem> {
        self.note(format!("people q={query} after={}", after.unwrap_or("-")))?;
        let everyone = [
            person(DANA, "dana@example.test", "Dana Doe"),
            person("00000000-0000-4000-8000-0000000000a3", "evan@example.test", "Evan Roe"),
        ];
        Ok(UserPage {
            users: everyone
                .into_iter()
                .filter(|person| person.upn.contains(query) || person.display_name.contains(query))
                .collect(),
            next: None,
        })
    }

    fn test_activity(&self, id: &str) -> Result<TestActivity, AppProblem> {
        self.note(format!("test {id}"))?;
        Ok(self.test.lock().unwrap().take().unwrap_or(TestActivity {
            delivered: true,
            status: Some(200),
            error: None,
            elapsed_ms: 12,
        }))
    }

    fn connection(&self) -> Result<BotConnection, AppProblem> {
        self.note("connection".to_owned())?;
        Ok(BotConnection {
            origin: "https://teams.example.test".to_owned(),
            issuer: "https://teams.example.test/bot".to_owned(),
        })
    }
}

/// What the trace wrote.
#[derive(Clone, Default)]
struct Written(Arc<Mutex<Vec<u8>>>);

impl Write for Written {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Written {
    /// Each `app-admin` line's change and kinds, and the whole text.
    fn lines(&self) -> (Vec<(String, Vec<String>)>, String) {
        let text = String::from_utf8(self.0.lock().unwrap().clone()).unwrap();
        let lines = text
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|line| line["view"] == "app-admin")
            .map(|line| {
                let kinds = line["kinds"]
                    .as_array()
                    .map(|kinds| kinds.iter().map(|kind| kind.as_str().unwrap().to_owned()).collect())
                    .unwrap_or_default();
                (line["change"].as_str().unwrap().to_owned(), kinds)
            })
            .collect();
        (lines, text)
    }
}

struct Opened {
    view: Entity<SettingsView>,
    written: Written,
    cx: VisualTestContext,
    _dir: tempfile::TempDir,
}

impl Opened {
    fn settle(&mut self) {
        self.cx.executor().advance_clock(Duration::from_millis(300));
        self.cx.run_until_parked();
    }

    fn click(&mut self, selector: &'static str) {
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("no {selector}"));
        self.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        self.settle();
    }

    fn click_owned(&mut self, selector: &str) {
        let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
        self.click(selector);
    }

    fn shows(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    fn apps(&mut self) -> Entity<ManageApps> {
        let view = self.view.clone();
        self.cx.read(|cx| view.read(cx).apps().cloned()).expect("the page")
    }

    fn listed(&mut self) -> Vec<String> {
        let apps = self.apps();
        self.cx.read(|cx| {
            apps.read(cx)
                .shown_apps(cx)
                .iter()
                .map(|app| app.name.clone())
                .collect()
        })
    }

    fn dialog(&mut self) -> Option<Entity<AppDialog>> {
        let apps = self.apps();
        self.cx.read(|cx| apps.read(cx).dialog().cloned())
    }

    fn kind(&mut self) -> Option<DialogKind> {
        let dialog = self.dialog()?;
        Some(self.cx.read(|cx| dialog.read(cx).kind()))
    }

    fn dialog_problem(&mut self) -> Option<String> {
        let dialog = self.dialog()?;
        self.cx.read(|cx| dialog.read(cx).problem().map(ToString::to_string))
    }

    fn with_dialog(&mut self, change: impl FnOnce(&mut AppDialog, &mut gpui::Window, &mut gpui::Context<AppDialog>)) {
        let dialog = self.dialog().expect("a dialog");
        self.cx
            .update(|window, cx| dialog.update(cx, |dialog, cx| change(dialog, window, cx)));
        self.settle();
    }

    fn keys(&mut self, keys: &str) {
        self.cx.simulate_keystrokes(keys);
        self.settle();
    }

    /// `key` pressed and let go: a focused button takes Enter or Space as it is let go.
    fn press(&mut self, key: &str) {
        let keystroke = gpui::Keystroke::parse(key).unwrap();
        self.cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        self.cx.simulate_event(gpui::KeyUpEvent { keystroke });
        self.settle();
    }

    fn page(&mut self) -> Page {
        let view = self.view.clone();
        self.cx.read(|cx| view.read(cx).page())
    }
}

/// The account's service as the settings read it: the app hands it in or takes it away (`Accounts::set_app_admin`).
type Held = Rc<RefCell<Option<Arc<dyn AppAdmin>>>>;

/// The settings of an account whose service is what `held` holds, with the bots shown or not.
fn opened_with(held: &Held, bots: bool, cx: &mut TestAppContext) -> Opened {
    cx.update(teams_ui::window::init);
    cx.update(theme::init);
    cx.update(|cx| cx.set_global(BotsFlag(bots)));
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    let written = Written::default();
    let trace = Arc::new(ViewTrace::new(written.clone()));
    let mut made = None;
    let window = cx.add_window(|window, cx| {
        let model = cx.new(|_| SettingsModel::load(file));
        let held = Rc::clone(held);
        let view = cx.new(|cx| {
            SettingsView::new(model, cx)
                .for_account(ME.to_owned())
                .with_app_admin(Rc::new(move |_| held.borrow().clone()), trace)
        });
        made = Some(view.clone());
        teams_ui::window::KitRoot::new(view, window, cx)
    });
    let mut opened = Opened {
        view: made.unwrap(),
        written,
        cx: VisualTestContext::from_window(window.into(), cx),
        _dir: dir,
    };
    opened.settle();
    opened
}

fn held(server: &Arc<Server>) -> Held {
    let admin: Arc<dyn AppAdmin> = Arc::clone(server) as Arc<dyn AppAdmin>;
    Rc::new(RefCell::new(Some(admin)))
}

/// The page shown for a server of two apps.
fn page_opened(cx: &mut TestAppContext) -> (Arc<Server>, Opened) {
    let server = Server::with(vec![
        app(NOTES, "Made-up Notes", true),
        app("00000000-0000-4000-8000-0000000000b2", "Made-up Tab", false),
    ]);
    let mut opened = opened_with(&held(&server), true, cx);
    opened.click("settings-page-apps");
    (server, opened)
}

#[gpui::test]
fn the_page_is_offered_only_with_the_service_and_the_bots_shown(cx: &mut TestAppContext) {
    let none: Held = Rc::new(RefCell::new(None));
    let mut microsoft = opened_with(&none, true, cx);
    assert!(!microsoft.shows("settings-page-apps"));
    let view = microsoft.view.clone();
    microsoft
        .cx
        .update(|_, cx| view.update(cx, |view, cx| view.show_page(Page::Apps, cx)));
    microsoft.settle();
    assert_eq!(microsoft.page(), Page::General);
    assert!(microsoft.cx.read(|cx| !view.read(cx).offers(Page::Apps, cx)));
}

#[gpui::test]
fn the_page_is_hidden_while_the_bots_are(cx: &mut TestAppContext) {
    let server = Server::with(Vec::new());
    let mut hidden = opened_with(&held(&server), false, cx);
    assert!(!hidden.shows("settings-page-apps"));
    assert!(server.asked().is_empty());
}

#[gpui::test]
fn the_list_shows_the_apps_and_the_search_narrows_it(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    assert!(opened.shows("manage-apps"));
    assert_eq!(opened.listed(), ["Made-up Notes", "Made-up Tab"]);
    assert!(opened.shows("apps-list"));
    let search = {
        let apps = opened.apps();
        opened.cx.read(|cx| apps.read(cx).search().clone())
    };
    opened
        .cx
        .update(|window, cx| search.update(cx, |search, cx| search.set_value("tab", window, cx)));
    opened.settle();
    assert_eq!(opened.listed(), ["Made-up Tab"]);
    opened
        .cx
        .update(|window, cx| search.update(cx, |search, cx| search.set_value("nothing", window, cx)));
    opened.settle();
    assert!(opened.listed().is_empty());
    assert!(opened.shows("apps-none"));
    assert_eq!(server.asked(), ["apps"]);
    let (lines, _) = opened.written.lines();
    assert_eq!(lines[0].0, "shown");
    assert_eq!(lines[1], ("listed".to_owned(), vec!["some".to_owned()]));
}

#[gpui::test]
fn an_empty_server_says_how_to_start_and_a_failed_read_can_be_tried_again(cx: &mut TestAppContext) {
    let server = Server::with(Vec::new());
    server.refuse_next(AppProblem::Account(AdminProblem::Unreachable));
    let mut opened = opened_with(&held(&server), true, cx);
    opened.click("settings-page-apps");
    assert!(opened.shows("apps-problem"));
    opened.click("apps-retry");
    assert!(opened.shows("apps-empty"));
    assert_eq!(server.asked(), ["apps", "apps"]);
}

/// The SDK settings of the bot just made: its secret in them, each SDK's own, Copy.
fn sdk_settings_shown(opened: &mut Opened, dialog: &Entity<AppDialog>) {
    opened.click("app-dialog-sdk-settings");
    assert_eq!(opened.kind(), Some(DialogKind::Settings));
    assert!(opened.shows("app-dialog-sdk-text"));
    let settings = opened.cx.read(|cx| dialog.read(cx).settings()).unwrap();
    assert!(settings.text.contains(SECRET) && settings.text.contains("https://teams.example.test/botframework.com"));
    opened.click("app-dialog-sdk-python");
    assert_eq!(opened.cx.read(|cx| dialog.read(cx).sdk()), Sdk::Python);
    opened.click("app-dialog-sdk-javascript");
    assert!(opened.shows("app-dialog-sdk-note"));
    opened.click("app-dialog-sdk-copy");
    assert!(
        opened
            .cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| text.contains("MicrosoftAppId="))
    );
}

#[gpui::test]
fn a_bot_registered_shows_its_id_and_secret_once_then_its_settings(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click("apps-register");
    assert_eq!(opened.kind(), Some(DialogKind::Register));
    // a name is needed: said, nothing asked
    opened.click("app-dialog-confirm");
    assert!(opened.dialog_problem().is_some_and(|words| words.contains("1 to 30")));
    opened.with_dialog(|dialog, window, cx| {
        dialog.fill(
            "Made-up Helper",
            "Helps",
            "https://bots.example.test/helper",
            window,
            cx,
        );
    });
    assert!(opened.dialog_problem().is_none());
    opened.click("app-dialog-mode-unsigned");
    opened.click("app-dialog-mode-signed");
    opened.click("app-dialog-confirm");
    assert_eq!(
        server.changes(),
        ["register Made-up Helper desc=Helps endpoint=https://bots.example.test/helper mode=signed"]
    );
    assert_eq!(opened.kind(), Some(DialogKind::Made));
    assert!(opened.shows("app-dialog-secret") && opened.shows("app-dialog-app-id"));
    let dialog = opened.dialog().unwrap();
    let shown = opened
        .cx
        .read(|cx| teams_ui::app_admin::shown_secret(dialog.read(cx)).map(str::to_owned));
    assert_eq!(shown.as_deref(), Some(SECRET));
    // from the keyboard: Enter on the focused Copy secret button (after Copy app id) copies, and the dialog stays
    opened.keys("tab tab");
    opened.press("enter");
    assert_eq!(opened.kind(), Some(DialogKind::Made));
    assert_eq!(
        opened.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some(SECRET)
    );
    // the list behind has it already
    assert!(opened.listed().contains(&"Made-up Helper".to_owned()));
    sdk_settings_shown(&mut opened, &dialog);
    // back to the secret, then done: it isn't shown again
    opened.click("app-dialog-cancel");
    assert_eq!(opened.kind(), Some(DialogKind::Made));
    opened.click("app-dialog-confirm");
    assert!(opened.dialog().is_none());
    let (lines, text) = opened.written.lines();
    assert!(!text.contains(SECRET) && !text.contains("Made-up Helper"));
    assert!(lines.contains(&("done".to_owned(), vec!["register".to_owned()])));
}

#[gpui::test]
fn a_refused_registration_says_why_and_keeps_the_form(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click("apps-register");
    opened.with_dialog(|dialog, window, cx| dialog.fill("Unsigned Helper", "", "", window, cx));
    opened.click("app-dialog-mode-unsigned");
    server.refuse_next(AppProblem::UnsignedNotAllowed);
    opened.click("app-dialog-confirm");
    assert_eq!(opened.kind(), Some(DialogKind::Register));
    assert!(
        opened
            .dialog_problem()
            .is_some_and(|words| words.contains("no unsigned bots"))
    );
    assert_eq!(
        server.changes(),
        ["register Unsigned Helper desc= endpoint=- mode=unsigned"]
    );
    // a typed endpoint that isn't an address is said before anything is asked
    opened.with_dialog(|dialog, window, cx| dialog.fill("Unsigned Helper", "", "bots.example.test", window, cx));
    opened.click("app-dialog-confirm");
    assert!(opened.dialog_problem().is_some_and(|words| words.contains("https")));
    assert_eq!(server.changes().len(), 1);
    opened.keys("escape");
    assert!(opened.dialog().is_none());
}

#[gpui::test]
fn a_package_is_chosen_and_uploaded_and_too_large_one_is_refused(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click("apps-upload");
    assert_eq!(opened.kind(), Some(DialogKind::Upload));
    let can = |opened: &mut Opened| {
        let dialog = opened.dialog().unwrap();
        opened.cx.read(|cx| dialog.read(cx).can_confirm(cx))
    };
    assert!(!can(&mut opened));
    opened.with_dialog(|dialog, _, cx| {
        dialog.choose_package("huge.zip".to_owned(), vec![0; teams_auth::admin::MAX_PACKAGE + 1], cx);
    });
    assert!(opened.dialog_problem().is_some_and(|words| words.contains("5 MB")));
    assert!(!can(&mut opened));
    opened.with_dialog(|dialog, _, cx| dialog.choose_package("notes.zip".to_owned(), b"PK made-up".to_vec(), cx));
    assert!(opened.shows("app-dialog-package") && can(&mut opened));
    opened.click("app-dialog-confirm");
    assert_eq!(server.changes(), ["upload 10 bytes"]);
    assert_eq!(opened.kind(), Some(DialogKind::Made));
    assert!(opened.shows("app-dialog-secret"));
    assert!(opened.listed().contains(&"Made-up Package".to_owned()));
}

#[gpui::test]
fn an_app_is_turned_off_and_its_endpoint_and_mode_change(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click_owned(&format!("app-row-{NOTES}"));
    assert_eq!(opened.kind(), Some(DialogKind::Details));
    let can = |opened: &mut Opened| {
        let dialog = opened.dialog().unwrap();
        opened.cx.read(|cx| dialog.read(cx).can_confirm(cx))
    };
    // nothing changed yet: nothing to save
    assert!(!can(&mut opened));
    opened.click("app-dialog-enabled");
    opened.click("app-dialog-mode-unsigned");
    opened.with_dialog(|dialog, window, cx| dialog.fill("", "", "https://bots.example.test/v2", window, cx));
    opened.click("app-dialog-confirm");
    assert_eq!(server.changes().len(), 1);
    assert!(server.changes()[0].contains("enabled: Some(false)"));
    assert!(server.changes()[0].contains("endpoint: Some(\"https://bots.example.test/v2\")"));
    assert!(server.changes()[0].contains("auth_mode: Some(Unsigned)"));
    assert!(opened.dialog().is_none());
    let notes = server.app(NOTES).unwrap();
    assert!(!notes.enabled);
    assert!(opened.shows("apps-list"));
}

#[gpui::test]
fn who_may_use_an_app_is_chosen_among_the_servers_people(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click_owned(&format!("app-row-{NOTES}"));
    opened.click("app-dialog-access");
    assert_eq!(opened.kind(), Some(DialogKind::Access));
    opened.click("app-dialog-who-specific");
    // no one chosen: said before anything is asked
    opened.click("app-dialog-confirm");
    assert!(
        opened
            .dialog_problem()
            .is_some_and(|words| words.contains("at least one"))
    );
    opened.with_dialog(|dialog, window, cx| dialog.search_for("dana", window, cx));
    assert!(opened.shows("app-dialog-found"));
    opened.click("app-dialog-add-dana@example.test");
    assert!(opened.shows("app-dialog-people"));
    let dialog = opened.dialog().unwrap();
    assert_eq!(opened.cx.read(|cx| dialog.read(cx).users().to_vec()), [DANA]);
    opened.click("app-dialog-confirm");
    // back on the details, which say so
    assert_eq!(opened.kind(), Some(DialogKind::Details));
    let changes = server.changes();
    assert!(
        changes[0].contains("everyone: Some(false)") && changes[0].contains(DANA),
        "{changes:?}"
    );
    // the dialog opened again names the person from the server's accounts, also when a search starts before the
    // naming is complete: the search does not stop the naming
    opened.keys("escape");
    opened.click_owned(&format!("app-row-{NOTES}"));
    opened.with_dialog(|dialog, window, cx| {
        dialog.go(DialogKind::Access, window, cx);
        dialog.search_for("nobody", window, cx);
    });
    assert!(opened.shows("app-dialog-people"));
    assert!(opened.shows("app-dialog-remove-dana@example.test"));
    opened.click("app-dialog-remove-dana@example.test");
    opened.click("app-dialog-who-everyone");
    opened.click("app-dialog-confirm");
    assert!(server.changes()[1].contains("everyone: Some(true)"));
    assert!(server.app(NOTES).unwrap().everyone);
}

#[gpui::test]
fn a_new_secret_is_confirmed_and_shown_once(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click_owned(&format!("app-row-{NOTES}"));
    opened.click("app-dialog-new-secret");
    assert_eq!(opened.kind(), Some(DialogKind::NewSecret));
    // Back keeps the old secret
    opened.click("app-dialog-cancel");
    assert_eq!(opened.kind(), Some(DialogKind::Details));
    assert!(server.changes().is_empty());
    opened.click("app-dialog-new-secret");
    opened.click("app-dialog-confirm");
    assert_eq!(server.changes(), [format!("new-secret {NOTES}")]);
    assert_eq!(opened.kind(), Some(DialogKind::Made));
    let dialog = opened.dialog().unwrap();
    assert!(opened.cx.read(|cx| dialog.read(cx).shows_secret()));
    opened.keys("enter");
    assert!(opened.dialog().is_none());
}

#[gpui::test]
fn an_app_is_deleted_after_a_confirmation_and_one_gone_meanwhile_says_so(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click_owned(&format!("app-row-{NOTES}"));
    opened.click("app-dialog-delete");
    assert_eq!(opened.kind(), Some(DialogKind::Delete));
    server.refuse_next(AppProblem::NotFound);
    opened.click("app-dialog-confirm");
    assert!(
        opened
            .dialog_problem()
            .is_some_and(|words| words.contains("no longer exists"))
    );
    opened.click("app-dialog-confirm");
    assert_eq!(server.changes(), [format!("delete {NOTES}"), format!("delete {NOTES}")]);
    assert!(opened.dialog().is_none());
    assert_eq!(opened.listed(), ["Made-up Tab"]);
}

#[gpui::test]
fn an_app_without_a_bot_has_no_secret_or_settings(cx: &mut TestAppContext) {
    let (_, mut opened) = page_opened(cx);
    opened.click("app-row-00000000-0000-4000-8000-0000000000b2");
    assert_eq!(opened.kind(), Some(DialogKind::Details));
    assert!(!opened.shows("app-dialog-new-secret") && !opened.shows("app-dialog-sdk-settings"));
    assert!(!opened.shows("app-dialog-endpoint"));
    assert!(opened.shows("app-dialog-delete"));
}

#[gpui::test]
fn the_keyboard_walks_the_list_and_opens_an_app(cx: &mut TestAppContext) {
    let (_, mut opened) = page_opened(cx);
    // the search, Register, Upload, then the list
    opened.keys("tab tab tab tab");
    opened.keys("down enter");
    let dialog = opened.dialog().expect("a dialog");
    let name = opened.cx.read(|cx| dialog.read(cx).app().map(|app| app.name.clone()));
    assert_eq!(name.as_deref(), Some("Made-up Tab"));
}

#[gpui::test]
fn the_page_goes_once_the_account_is_no_administrator(cx: &mut TestAppContext) {
    let server = Server::with(vec![app(NOTES, "Made-up Notes", true)]);
    let held = held(&server);
    let mut opened = opened_with(&held, true, cx);
    opened.click("settings-page-apps");
    assert_eq!(opened.page(), Page::Apps);
    held.borrow_mut().take();
    let view = opened.view.clone();
    opened.cx.update(|_, cx| view.update(cx, |_, cx| cx.notify()));
    opened.settle();
    assert_eq!(opened.page(), Page::General);
    assert!(opened.shows("settings-apps-gone"));
    assert!(opened.cx.read(|cx| view.read(cx).apps().is_none()));
    opened.click("settings-page-appearance");
    opened.click("settings-page-general");
    assert!(!opened.shows("settings-apps-gone"));
}

#[gpui::test]
fn the_list_is_read_again_each_time_the_page_opens_and_keeps_the_cursor(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.keys("tab tab tab tab");
    opened.keys("down");
    opened.click("settings-page-general");
    // another administrator adds an app while the page is out of sight
    server
        .apps
        .lock()
        .unwrap()
        .insert(0, app("00000000-0000-4000-8000-0000000000b0", "Made-up Alpha", true));
    let view = opened.view.clone();
    opened
        .cx
        .update(|_, cx| view.update(cx, |view, cx| view.show_page(Page::Apps, cx)));
    // the list read before shows while it is read again: no "Loading apps…" in its place
    assert!(opened.shows("apps-list"));
    assert!(!opened.shows("apps-loading"));
    opened.settle();
    assert_eq!(opened.listed(), ["Made-up Alpha", "Made-up Notes", "Made-up Tab"]);
    assert_eq!(server.asked(), ["apps", "apps"]);
    // the cursor stays on the app it was on
    opened.keys("tab tab tab tab");
    opened.keys("enter");
    let dialog = opened.dialog().expect("a dialog");
    let name = opened.cx.read(|cx| dialog.read(cx).app().map(|app| app.name.clone()));
    assert_eq!(name.as_deref(), Some("Made-up Tab"));
}

#[gpui::test]
fn the_list_is_read_again_when_the_settings_open_again(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    let view = opened.view.clone();
    // the app shows a chat, then the settings again, on the same page
    opened.cx.update(|_, cx| view.update(cx, |view, cx| view.put_away(cx)));
    server
        .apps
        .lock()
        .unwrap()
        .push(app("00000000-0000-4000-8000-0000000000b3", "Made-up Zed", false));
    opened.cx.update(|_, cx| view.update(cx, |_, cx| cx.notify()));
    opened.settle();
    assert_eq!(opened.listed(), ["Made-up Notes", "Made-up Tab", "Made-up Zed"]);
    assert_eq!(server.asked(), ["apps", "apps"]);
}

fn failing() -> BotHealth {
    BotHealth {
        state: HealthState::Failing,
        failures: 3,
        paused_until: None,
        last_error: Some("the bot answered 500".to_owned()),
        last_success: None,
        pending: 0,
        sent: 4,
        failed: 1,
    }
}

#[gpui::test]
fn a_bot_that_fails_is_marked_and_its_details_say_why(cx: &mut TestAppContext) {
    let mut notes = app(NOTES, "Made-up Notes", true);
    notes.health = Some(failing());
    let mut quiet = app("00000000-0000-4000-8000-0000000000b3", "Made-up Quiet", true);
    quiet.health = Some(BotHealth {
        state: HealthState::Healthy,
        failures: 0,
        last_error: None,
        ..failing()
    });
    let server = Server::with(vec![notes, quiet]);
    let mut opened = opened_with(&held(&server), true, cx);
    opened.click("settings-page-apps");
    // the row's mark is the unit test's (`marks`); here the details
    opened.click_owned(&format!("app-row-{NOTES}"));
    assert!(opened.shows("app-dialog-health"));
    assert!(opened.shows("app-dialog-health-counts"));
    assert!(opened.shows("app-dialog-test"));
    let dialog = opened.dialog().unwrap();
    let health = opened
        .cx
        .read(|cx| dialog.read(cx).app().and_then(|app| app.health.clone()));
    let (words, attention) = teams_ui::app_admin::health::summary(health.as_ref(), 0);
    assert!(attention && words.contains("3 failed in a row"), "{words}");
}

#[gpui::test]
fn a_test_activity_is_sent_and_how_the_bot_answered_shows(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    let (_, before) = opened.written.lines();
    assert!(!before.contains("tested"));
    opened.click_owned(&format!("app-row-{NOTES}"));
    assert!(!opened.shows("app-dialog-test-result"));
    opened.click("app-dialog-test");
    assert!(
        server.asked().contains(&format!("test {NOTES}")),
        "{:?}",
        server.asked()
    );
    assert!(opened.shows("app-dialog-test-result"));
    let result = |opened: &mut Opened| {
        let dialog = opened.dialog().unwrap();
        opened
            .cx
            .read(|cx| dialog.read(cx).test_result().cloned())
            .map(|result| teams_ui::app_admin::health::went(&result))
    };
    assert_eq!(
        result(&mut opened),
        Some(("The bot took it (200) in 12 ms.".to_owned(), true))
    );

    *server.test.lock().unwrap() = Some(TestActivity {
        delivered: false,
        status: None,
        error: Some("the address is on a network the server doesn't reach".to_owned()),
        elapsed_ms: 1,
    });
    opened.click("app-dialog-test");
    let (words, took) = result(&mut opened).unwrap();
    assert!(!took && words.contains("network"), "{words}");

    server.refuse_next(AppProblem::AppDisabled);
    opened.click("app-dialog-test");
    assert_eq!(result(&mut opened), Some(("Turn the app on first.".to_owned(), false)));
    // the details stay open and nothing else was asked
    assert_eq!(opened.kind(), Some(DialogKind::Details));
    assert!(
        server.changes().iter().all(|asked| asked.starts_with("test ")),
        "{:?}",
        server.changes()
    );
    let (lines, text) = opened.written.lines();
    let tested: Vec<_> = lines.iter().filter(|(change, _)| change == "tested").collect();
    assert_eq!(tested.len(), 3, "{lines:?}");
    assert!(!text.contains(NOTES), "the trace names no app");
}

#[gpui::test]
fn an_app_that_is_off_offers_no_test(cx: &mut TestAppContext) {
    let mut notes = app(NOTES, "Made-up Notes", true);
    notes.enabled = false;
    let server = Server::with(vec![notes]);
    let mut opened = opened_with(&held(&server), true, cx);
    opened.click("settings-page-apps");
    opened.click_owned(&format!("app-row-{NOTES}"));
    opened.click("app-dialog-test");
    assert!(
        !server.asked().iter().any(|asked| asked.starts_with("test ")),
        "{:?}",
        server.asked()
    );
}

#[gpui::test]
fn an_endpoint_on_a_closed_network_is_said_in_the_details(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(cx);
    opened.click_owned(&format!("app-row-{NOTES}"));
    server.refuse_next(AppProblem::EndpointNotAllowed);
    opened.with_dialog(|dialog, window, cx| dialog.fill("", "", "https://169.254.169.254/latest", window, cx));
    opened.click("app-dialog-confirm");
    let said = opened.dialog_problem().unwrap();
    assert!(said.contains("doesn't send to this address"), "{said}");
    assert_eq!(opened.kind(), Some(DialogKind::Details));
}
