//! Manage users (`teams_ui::user_admin`) in the settings, on GPUI's test context over a made-up server: the page shows
//! only for an account that has the service; its list, search and "Show more"; the dialogs that add, edit, reset and
//! delete, what each asks of the service and what a refusal says; the keyboard on the list; the trace; the page going
//! once the account is no administrator any more.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext};
use teams_auth::admin::{AdminUser, UserChanges, UserPage};
use teams_auth::secrets::Secret;
use teams_ui::settings::{SettingsFile, SettingsModel};
use teams_ui::settings_view::{Page, SettingsView};
use teams_ui::user_admin::{AdminProblem, DialogKind, ManageUsers, NewAccount, UserAdmin, UserDialog};
use teams_ui::{ViewTrace, theme};

const ME: &str = "8:orgid:00000000-0000-4000-8000-0000000000a1";

fn user(n: u8, upn: &str, name: &str, admin: bool) -> AdminUser {
    AdminUser {
        id: format!("00000000-0000-4000-8000-0000000000a{n}"),
        upn: upn.to_owned(),
        display_name: name.to_owned(),
        enabled: true,
        admin,
        must_change_password: false,
    }
}

/// A made-up server: its accounts, a page size, what it was asked, and a refusal for the next change.
#[derive(Default)]
struct Server {
    users: Mutex<Vec<AdminUser>>,
    page: usize,
    asked: Mutex<Vec<String>>,
    refuse: Mutex<Option<AdminProblem>>,
}

impl Server {
    fn with(users: Vec<AdminUser>, page: usize) -> Arc<Self> {
        Arc::new(Self {
            users: Mutex::new(users),
            page,
            ..Self::default()
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn refuse_next(&self, problem: AdminProblem) {
        *self.refuse.lock().unwrap() = Some(problem);
    }

    fn note(&self, asked: String) -> Result<(), AdminProblem> {
        self.asked.lock().unwrap().push(asked);
        self.refuse.lock().unwrap().take().map_or(Ok(()), Err)
    }
}

impl UserAdmin for Server {
    fn list(&self, query: &str, after: Option<&str>) -> Result<UserPage, AdminProblem> {
        self.note(format!("list q={query} after={}", after.unwrap_or("-")))?;
        let users = self.users.lock().unwrap();
        let mut found: Vec<AdminUser> = users
            .iter()
            .filter(|user| user.upn.contains(query) && after.is_none_or(|after| user.upn.as_str() > after))
            .cloned()
            .collect();
        let more = found.len() > self.page;
        found.truncate(self.page);
        let next = more.then(|| found.last().map(|user| user.upn.clone())).flatten();
        Ok(UserPage { users: found, next })
    }

    fn add(&self, new: &NewAccount) -> Result<AdminUser, AdminProblem> {
        self.note(format!(
            "add {} name={} password={} admin={} must_change={}",
            new.upn,
            new.display_name,
            new.password.expose(),
            new.admin,
            new.must_change_password
        ))?;
        let added = AdminUser {
            admin: new.admin,
            must_change_password: new.must_change_password,
            ..user(9, &new.upn, &new.display_name, false)
        };
        self.users.lock().unwrap().push(added.clone());
        Ok(added)
    }

    fn change(&self, id: &str, changes: &UserChanges) -> Result<AdminUser, AdminProblem> {
        self.note(format!("change {id} {changes:?}"))?;
        let mut users = self.users.lock().unwrap();
        let user = users
            .iter_mut()
            .find(|user| user.id == id)
            .ok_or(AdminProblem::NotFound)?;
        if let Some(admin) = changes.admin {
            user.admin = admin;
        }
        if let Some(enabled) = changes.enabled {
            user.enabled = enabled;
        }
        if let Some(name) = &changes.display_name {
            user.display_name.clone_from(name);
        }
        Ok(user.clone())
    }

    fn delete(&self, id: &str) -> Result<(), AdminProblem> {
        self.note(format!("delete {id}"))?;
        self.users.lock().unwrap().retain(|user| user.id != id);
        Ok(())
    }

    fn reset_password(&self, id: &str, password: &Secret, must_change: bool) -> Result<(), AdminProblem> {
        self.note(format!(
            "reset {id} password={} must_change={must_change}",
            password.expose()
        ))
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
    /// Each line's change, kinds and people.
    fn lines(&self) -> Vec<(String, Vec<String>, usize)> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|line| line["view"] == "user-admin")
            .map(|line| {
                let kinds = line["kinds"]
                    .as_array()
                    .map(|kinds| kinds.iter().map(|kind| kind.as_str().unwrap().to_owned()).collect())
                    .unwrap_or_default();
                let people = line["people"].as_array().map_or(0, Vec::len);
                (line["change"].as_str().unwrap().to_owned(), kinds, people)
            })
            .collect()
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

    fn shows(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    fn users(&mut self) -> Entity<ManageUsers> {
        let view = self.view.clone();
        self.cx.read(|cx| view.read(cx).users().cloned()).expect("the page")
    }

    fn listed(&mut self) -> Vec<String> {
        let users = self.users();
        self.cx
            .read(|cx| users.read(cx).users().iter().map(|user| user.upn.clone()).collect())
    }

    fn dialog(&mut self) -> Option<Entity<UserDialog>> {
        let users = self.users();
        self.cx.read(|cx| users.read(cx).dialog().cloned())
    }

    fn dialog_problem(&mut self) -> Option<String> {
        let dialog = self.dialog()?;
        self.cx.read(|cx| dialog.read(cx).problem().map(ToString::to_string))
    }

    fn fill(&mut self, upn: &str, name: &str, password: &str) {
        let dialog = self.dialog().expect("a dialog");
        self.cx.update(|window, cx| {
            dialog.update(cx, |dialog, cx| dialog.fill(upn, name, password, window, cx));
        });
        self.settle();
    }

    fn keys(&mut self, keys: &str) {
        self.cx.simulate_keystrokes(keys);
        self.settle();
    }

    /// `key` pressed and let go: a focused button takes Enter or Space as it is let go.
    fn press(&mut self, key: &'static str) {
        let keystroke = gpui::Keystroke::parse(key).unwrap();
        self.cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        self.cx.simulate_event(gpui::KeyUpEvent { keystroke });
        self.settle();
    }
}

/// The account's service as the settings read it: the app hands it in or takes it away (`Accounts::set_admin`).
type Held = Rc<RefCell<Option<Arc<dyn UserAdmin>>>>;

/// The settings of an account whose service is `server` (none: a Microsoft 365 account's).
fn opened(server: Option<Arc<Server>>, cx: &mut TestAppContext) -> Opened {
    let admin: Option<Arc<dyn UserAdmin>> = server.map(|server| server as Arc<dyn UserAdmin>);
    opened_with(&Rc::new(RefCell::new(admin)), cx)
}

/// The settings of an account whose service is what `held` holds when they read it.
fn opened_with(held: &Held, cx: &mut TestAppContext) -> Opened {
    cx.update(teams_ui::window::init);
    cx.update(theme::init);
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
                .with_admin(Rc::new(move |_| held.borrow().clone()), trace)
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

fn three() -> Vec<AdminUser> {
    vec![
        user(1, "ada@example.test", "Ada Admin", true),
        user(2, "dana@example.test", "Dana Doe", false),
        user(3, "evan@example.test", "Evan Roe", false),
    ]
}

/// The page shown for a server of [`three`] users, `page` to a page.
fn page_opened(page: usize, cx: &mut TestAppContext) -> (Arc<Server>, Opened) {
    let server = Server::with(three(), page);
    let mut opened = opened(Some(Arc::clone(&server)), cx);
    opened.click("settings-page-users");
    (server, opened)
}

#[gpui::test]
fn the_page_is_offered_only_to_an_account_with_the_service(cx: &mut TestAppContext) {
    let mut microsoft = opened(None, cx);
    assert!(!microsoft.shows("settings-page-users"));
    let view = microsoft.view.clone();
    microsoft
        .cx
        .update(|_, cx| view.update(cx, |view, cx| view.show_page(Page::Users, cx)));
    microsoft.settle();
    assert_eq!(microsoft.cx.read(|cx| view.read(cx).page()), Page::General);
    assert!(microsoft.cx.read(|cx| !view.read(cx).offers(Page::Users, cx)));
    assert!(microsoft.written.lines().is_empty());
}

#[gpui::test]
fn the_page_goes_once_the_account_is_no_administrator(cx: &mut TestAppContext) {
    let server: Arc<dyn UserAdmin> = Server::with(three(), 50);
    let held: Held = Rc::new(RefCell::new(Some(server)));
    let mut opened = opened_with(&held, cx);
    opened.click("settings-page-users");
    let view = opened.view.clone();
    assert_eq!(opened.cx.read(|cx| view.read(cx).page()), Page::Users);
    assert_eq!(opened.listed().len(), 3);

    // the server said the account is no administrator any more: the app takes the service away
    held.borrow_mut().take();
    opened.cx.update(|_, cx| view.update(cx, |_, cx| cx.notify()));
    opened.settle();
    assert_eq!(opened.cx.read(|cx| view.read(cx).page()), Page::General);
    assert!(!opened.shows("settings-page-users"));
    assert!(opened.cx.read(|cx| view.read(cx).users().is_none()));
    // why the page went stays on screen until another page is chosen
    assert!(opened.shows("settings-admin-gone"));
    opened.click("settings-page-appearance");
    opened.click("settings-page-general");
    assert!(!opened.shows("settings-admin-gone"));
}

#[gpui::test]
fn the_administrator_sees_the_servers_users(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    let view = opened.view.clone();
    assert!(opened.cx.read(|cx| view.read(cx).offers(Page::Users, cx)));
    assert_eq!(
        opened.listed(),
        ["ada@example.test", "dana@example.test", "evan@example.test"]
    );
    for row in [
        "user-row-ada@example.test",
        "user-row-dana@example.test",
        "users-search",
        "users-add",
    ] {
        assert!(opened.shows(row), "{row}");
    }
    assert!(!opened.shows("users-more"));
    assert_eq!(server.asked(), ["list q= after=-"]);
    assert_eq!(
        opened.written.lines(),
        [("shown".to_owned(), vec![], 0), ("listed".to_owned(), vec![], 3)]
    );
}

#[gpui::test]
fn the_list_is_searched_and_shows_more_on_demand(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(2, cx);
    assert_eq!(opened.listed().len(), 2);
    let users = opened.users();
    assert!(opened.cx.read(|cx| users.read(cx).has_more()));
    opened.click("users-more");
    assert_eq!(opened.listed().len(), 3);
    assert!(!opened.shows("users-more"));
    assert_eq!(server.asked()[1], "list q= after=dana@example.test");

    opened.click("users-search");
    opened.cx.simulate_input("dana");
    opened.settle();
    assert_eq!(opened.listed(), ["dana@example.test"]);
    opened.cx.simulate_input("x");
    opened.settle();
    assert!(opened.listed().is_empty());
    assert!(opened.shows("users-none"));
}

#[gpui::test]
fn a_list_that_cant_be_read_says_why_and_reads_again(cx: &mut TestAppContext) {
    let server = Server::with(three(), 50);
    server.refuse_next(AdminProblem::Unreachable);
    let mut opened = opened(Some(Arc::clone(&server)), cx);
    opened.click("settings-page-users");
    assert!(opened.shows("users-problem"));
    let users = opened.users();
    assert_eq!(
        opened.cx.read(|cx| users.read(cx).problem().cloned()),
        Some(AdminProblem::Unreachable)
    );
    opened.click("users-retry");
    assert!(!opened.shows("users-problem"));
    assert_eq!(opened.listed().len(), 3);
    assert_eq!(
        opened.written.lines()[1],
        ("failed".to_owned(), vec!["unreachable".to_owned()], 0)
    );
}

/// The password given shows once, with Copy (which copies `password`, from the keyboard too), as a reset shows it;
/// Done closes.
fn shown_once_then_done(opened: &mut Opened, password: &str) {
    let dialog = opened.dialog().expect("the password given");
    assert!(opened.cx.read(|cx| dialog.read(cx).shows_given()));
    assert!(opened.shows("user-dialog-given") && opened.shows("user-dialog-copy"));
    // Enter on the focused Copy button copies, and the dialog stays
    opened.keys("tab");
    opened.press("enter");
    assert!(opened.dialog().is_some());
    assert_eq!(
        opened.cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some(password)
    );
    opened.click("user-dialog-confirm");
    assert!(opened.dialog().is_none());
}

#[gpui::test]
fn a_user_is_added_once_the_form_reads(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("users-add");
    assert!(opened.shows("user-dialog"));
    opened.keys("enter");
    assert_eq!(
        opened.dialog_problem().as_deref(),
        Some("Enter a user name like name@example.com.")
    );
    opened.fill("frank@example.test", "", "");
    opened.click("user-dialog-confirm");
    assert_eq!(opened.dialog_problem().as_deref(), Some("Enter a display name."));
    opened.fill("frank@example.test", "Frank Poe", "short");
    opened.click("user-dialog-confirm");
    assert_eq!(
        opened.dialog_problem().as_deref(),
        Some("Use a password of at least 8 characters.")
    );
    assert_eq!(server.asked().len(), 1, "nothing was asked of the server yet");

    // a user name another account has
    opened.fill("frank@example.test", "Frank Poe", "a made-up password");
    server.refuse_next(AdminProblem::UpnTaken);
    opened.click("user-dialog-confirm");
    assert_eq!(
        opened.dialog_problem().as_deref(),
        Some("Another user already has this user name.")
    );
    assert!(opened.shows("user-dialog-problem"));

    opened.click("user-dialog-admin");
    opened.click("user-dialog-confirm");
    shown_once_then_done(&mut opened, "a made-up password");
    assert_eq!(
        server.asked()[2],
        "add frank@example.test name=Frank Poe password=a made-up password admin=true must_change=true"
    );
    assert_eq!(opened.listed().len(), 4);
    let lines = opened.written.lines();
    assert!(lines.contains(&("dialog".to_owned(), vec!["add".to_owned()], 0)));
    assert!(lines.contains(&("failed".to_owned(), vec!["upn-taken".to_owned()], 0)));
    assert!(lines.contains(&("done".to_owned(), vec!["add".to_owned()], 0)));
    assert!(
        !opened
            .written
            .0
            .lock()
            .unwrap()
            .windows(4)
            .any(|bytes| bytes == b"made")
    );
}

#[gpui::test]
fn a_user_is_edited_and_the_only_administrator_stays_one(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("user-row-dana@example.test");
    let dialog = opened.dialog().expect("the edit dialog");
    assert_eq!(opened.cx.read(|cx| dialog.read(cx).kind()), DialogKind::Edit);
    assert!(
        opened.cx.read(|cx| !dialog.read(cx).can_confirm(cx)),
        "nothing changed yet"
    );
    opened.click("user-dialog-admin");
    opened.click("user-dialog-enabled");
    opened.click("user-dialog-confirm");
    assert!(opened.dialog().is_none());
    assert_eq!(
        server.asked()[1],
        "change 00000000-0000-4000-8000-0000000000a2 UserChanges { display_name: None, upn: None, enabled: \
         Some(false), admin: Some(true) }"
    );

    opened.click("user-row-ada@example.test");
    opened.click("user-dialog-admin");
    server.refuse_next(AdminProblem::LastAdmin);
    opened.click("user-dialog-confirm");
    assert_eq!(
        opened.dialog_problem().as_deref(),
        Some("This is the server's only administrator. Make another user an administrator first.")
    );
    opened.keys("escape");
    assert!(opened.dialog().is_none());
    assert!(
        opened
            .written
            .lines()
            .contains(&("failed".to_owned(), vec!["last-admin".to_owned()], 0))
    );

    // the name changed alone, with Cancel first
    opened.click("user-row-evan@example.test");
    opened.click("user-dialog-cancel");
    assert!(opened.dialog().is_none());
    opened.click("user-row-evan@example.test");
    opened.fill("evan@example.test", "Evan R.", "");
    opened.keys("enter");
    assert!(
        server
            .asked()
            .iter()
            .any(|asked| asked.contains("display_name: Some(\"Evan R.\"), upn: None")),
        "{:?}",
        server.asked()
    );
}

#[gpui::test]
fn a_password_is_reset_and_shown_once(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("user-row-dana@example.test");
    opened.click("user-dialog-reset");
    let dialog = opened.dialog().expect("the reset dialog");
    assert_eq!(opened.cx.read(|cx| dialog.read(cx).kind()), DialogKind::Reset);
    opened.click("user-dialog-confirm");
    assert_eq!(
        opened.dialog_problem().as_deref(),
        Some("Use a password of at least 8 characters.")
    );
    opened.click("user-dialog-suggest");
    assert_eq!(opened.dialog_problem(), None);
    opened.click("user-dialog-must-change");
    opened.click("user-dialog-confirm");
    let asked = server.asked().last().unwrap().clone();
    assert!(
        asked.starts_with("reset 00000000-0000-4000-8000-0000000000a2 password=")
            && asked.ends_with("must_change=false"),
        "{asked}"
    );
    assert!(opened.cx.read(|cx| dialog.read(cx).shows_given()));
    assert!(opened.shows("user-dialog-given") && opened.shows("user-dialog-copy"));
    assert!(!opened.shows("user-dialog-cancel"));
    opened.click("user-dialog-copy");
    let copied = opened.cx.read_from_clipboard().and_then(|item| item.text()).unwrap();
    assert!(asked.contains(&format!("password={copied} ")), "{copied}");
    opened.keys("escape");
    assert!(opened.dialog().is_none());
    assert_eq!(server.asked().last().unwrap(), "list q= after=-");

    // again, read and closed with Enter: the dialog has the keyboard once the password shows
    opened.click("user-row-evan@example.test");
    opened.click("user-dialog-reset");
    opened.cx.simulate_input("a made-up password");
    opened.keys("enter");
    assert!(opened.shows("user-dialog-given"));
    opened.keys("enter");
    assert!(opened.dialog().is_none());
}

#[gpui::test]
fn a_user_is_deleted_after_a_confirmation(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("user-row-evan@example.test");
    opened.click("user-dialog-delete");
    let dialog = opened.dialog().expect("the confirmation");
    assert_eq!(opened.cx.read(|cx| dialog.read(cx).kind()), DialogKind::Delete);
    assert_eq!(server.asked().len(), 1, "nothing is deleted before the confirmation");
    opened.click("user-dialog-confirm");
    assert!(opened.dialog().is_none());
    assert_eq!(server.asked()[1], "delete 00000000-0000-4000-8000-0000000000a3");
    assert_eq!(opened.listed(), ["ada@example.test", "dana@example.test"]);
    assert!(!opened.shows("user-row-evan@example.test"));
}

#[gpui::test]
fn a_dialog_stays_until_its_change_is_answered(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("user-row-evan@example.test");
    opened.click("user-dialog-delete");
    let dialog = opened.dialog().expect("the confirmation");
    // confirmed, its answer not in yet: Escape waits for it
    opened.cx.update(|_, cx| dialog.update(cx, UserDialog::confirm));
    assert!(opened.cx.read(|cx| !dialog.read(cx).can_confirm(cx)));
    opened.keys("escape");
    assert_eq!(server.asked()[1], "delete 00000000-0000-4000-8000-0000000000a3");
    // the answer reached the page: the list is read again without the account
    assert!(opened.dialog().is_none());
    assert_eq!(opened.listed(), ["ada@example.test", "dana@example.test"]);
}

#[gpui::test]
fn the_keyboard_walks_the_list_and_opens_a_user(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("users-search");
    opened.keys("tab tab");
    opened.keys("down down down up");
    opened.keys("enter");
    let dialog = opened.dialog().expect("the second row's dialog");
    assert_eq!(opened.cx.read(|cx| dialog.read(cx).kind()), DialogKind::Edit);
    opened.click("user-dialog-enabled");
    opened.click("user-dialog-confirm");
    assert!(
        server.asked()[1].starts_with("change 00000000-0000-4000-8000-0000000000a2 "),
        "{:?}",
        server.asked()
    );
    // the search has the keyboard again
    opened.cx.simulate_input("ada");
    opened.settle();
    assert_eq!(opened.listed(), ["ada@example.test"]);
    opened.keys("tab tab");
    opened.keys("end home space");
    assert!(opened.dialog().is_some());
}

/// `n` made-up users, in user name order, the first an administrator.
fn many(n: u8) -> Vec<AdminUser> {
    (0..n)
        .map(|at| {
            user(
                at,
                &format!("user-{at:02}@example.test"),
                &format!("User {at:02}"),
                at == 0,
            )
        })
        .collect()
}

/// A display name of `n` characters.
fn long_name(n: usize) -> String {
    "Bartholomew Alexander ".chars().cycle().take(n).collect()
}

#[gpui::test]
fn a_display_name_too_long_is_said_before_it_is_sent(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("users-add");
    opened.fill("frank@example.test", &long_name(300), "a made-up password");
    opened.click("user-dialog-confirm");
    assert_eq!(
        opened.dialog_problem().as_deref(),
        Some("Use a display name of at most 256 characters.")
    );
    assert_eq!(server.asked().len(), 1, "nothing was asked of the server");
    // the server's own refusal says the same
    assert_eq!(
        AdminProblem::InvalidName.words().as_ref(),
        "Enter a display name of at most 256 characters."
    );
    opened.fill("frank@example.test", &long_name(256), "a made-up password");
    opened.click("user-dialog-confirm");
    assert!(
        server.asked()[1].starts_with("add frank@example.test "),
        "{:?}",
        server.asked()
    );
}

#[gpui::test]
fn the_delete_confirmation_starts_on_cancel(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    opened.click("user-row-evan@example.test");
    opened.click("user-dialog-delete");
    assert!(opened.dialog().is_some());
    // Enter takes the safe choice the keyboard starts on
    opened.press("enter");
    assert!(opened.dialog().is_none());
    assert_eq!(server.asked().len(), 1, "nothing was deleted");
    // Tab then Enter: Delete
    opened.click("user-row-evan@example.test");
    opened.click("user-dialog-delete");
    opened.keys("tab");
    opened.press("enter");
    assert_eq!(server.asked()[1], "delete 00000000-0000-4000-8000-0000000000a3");
    // Escape closes it
    opened.click("user-row-dana@example.test");
    opened.click("user-dialog-delete");
    opened.keys("escape");
    assert!(opened.dialog().is_none());
}

#[gpui::test]
fn a_long_name_keeps_the_dialog_titles_short(cx: &mut TestAppContext) {
    let server = Server::with(vec![user(1, "long@example.test", &long_name(200), false)], 50);
    let mut opened = opened(Some(server), cx);
    opened.click("settings-page-users");
    opened.click("user-row-long@example.test");
    for open in [None, Some("user-dialog-reset")] {
        if let Some(button) = open {
            opened.click(button);
        }
        let title = opened.cx.debug_bounds("user-dialog-title").expect("the title");
        assert!(title.size.height < gpui::px(64.0), "{open:?}: {title:?}");
    }
    opened.keys("escape");
    opened.click("user-row-long@example.test");
    opened.click("user-dialog-delete");
    let title = opened.cx.debug_bounds("user-dialog-title").expect("the title");
    assert!(title.size.height < gpui::px(64.0), "delete: {title:?}");
}

#[gpui::test]
fn the_page_stays_readable_in_a_narrow_window(cx: &mut TestAppContext) {
    let mut users = three();
    users[1].enabled = false;
    let server = Server::with(users, 50);
    let mut opened = opened(Some(server), cx);
    opened.cx.simulate_resize(gpui::size(gpui::px(400.0), gpui::px(800.0)));
    opened.click("settings-page-users");
    let width = gpui::px(400.0);
    let add = opened.cx.debug_bounds("users-add").expect("Add user");
    assert!(add.right() <= width, "{add:?}");
    let search = opened.cx.debug_bounds("users-search").expect("the search");
    assert!(search.size.width >= gpui::px(160.0), "{search:?}");
    for (name, badge) in [
        ("user-name-ada@example.test", "user-badges-ada@example.test"),
        ("user-name-dana@example.test", "user-badges-dana@example.test"),
    ] {
        let name = opened.cx.debug_bounds(name).expect("a name");
        assert!(name.size.width >= gpui::px(120.0), "{name:?}");
        let badges = opened.cx.debug_bounds(badge).expect("its badges");
        assert!(badges.right() <= width, "{badges:?}");
    }
}

#[gpui::test]
fn the_focused_show_more_scrolls_into_view(cx: &mut TestAppContext) {
    let server = Server::with(many(60), 50);
    let mut opened = opened(Some(server), cx);
    opened.cx.simulate_resize(gpui::size(gpui::px(1024.0), gpui::px(600.0)));
    opened.click("settings-page-users");
    assert_eq!(opened.listed().len(), 50);
    // the search, Add user, the list, Show more
    opened.click("users-search");
    opened.keys("tab tab");
    opened.keys("end");
    let row = opened
        .cx
        .debug_bounds("user-row-user-49@example.test")
        .expect("the last row");
    assert!(
        row.bottom() <= gpui::px(600.0),
        "the list's cursor row is out of sight: {row:?}"
    );
    opened.keys("tab");
    let more = opened.cx.debug_bounds("users-more").expect("Show more");
    assert!(more.bottom() <= gpui::px(600.0), "Show more is out of sight: {more:?}");
    opened.press("enter");
    assert_eq!(opened.listed().len(), 60);
}

#[gpui::test]
fn typing_in_a_dialog_doesnt_draw_the_list_again(cx: &mut TestAppContext) {
    let server = Server::with(many(61), 61);
    let mut opened = opened(Some(server), cx);
    opened.click("settings-page-users");
    opened.click("users-add");
    let users = opened.users();
    // the first key settles the dialog's field (its caret): the page may draw once
    opened.cx.simulate_input("d");
    opened.settle();
    let before = opened.cx.read(|cx| users.read(cx).drawn());
    for key in "ana@example.test".chars() {
        opened.cx.simulate_input(&key.to_string());
        opened.settle();
    }
    let after = opened.cx.read(|cx| users.read(cx).drawn());
    assert_eq!(after, before, "the page drew again while the dialog was typed in");
}

#[gpui::test]
fn the_list_is_read_again_each_time_the_page_opens_and_keeps_the_cursor(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(2, cx);
    opened.click("users-more");
    opened.click("users-search");
    opened.keys("tab tab");
    opened.keys("down");
    opened.click("settings-page-general");
    // another administrator adds an account while the page is out of sight
    server
        .users
        .lock()
        .unwrap()
        .insert(0, user(4, "aaron@example.test", "Aaron Doe", false));
    let view = opened.view.clone();
    opened
        .cx
        .update(|_, cx| view.update(cx, |view, cx| view.show_page(Page::Users, cx)));
    // the list read before shows while it is read again: no "Loading users…" in its place
    assert!(opened.shows("users-list"));
    assert!(!opened.shows("users-loading"));
    opened.settle();
    // as many pages as showed before are read again
    assert_eq!(
        opened.listed(),
        [
            "aaron@example.test",
            "ada@example.test",
            "dana@example.test",
            "evan@example.test"
        ]
    );
    assert_eq!(
        server.asked()[2..],
        ["list q= after=-", "list q= after=ada@example.test"]
    );
    // the cursor stays on the account it was on
    opened.click("users-search");
    opened.keys("tab tab");
    opened.keys("enter");
    let dialog = opened.dialog().expect("a dialog");
    let upn = opened.cx.read(|cx| dialog.read(cx).user().map(|user| user.upn.clone()));
    assert_eq!(upn.as_deref(), Some("dana@example.test"));
}

#[gpui::test]
fn the_list_is_read_again_when_the_settings_open_again(cx: &mut TestAppContext) {
    let (server, mut opened) = page_opened(50, cx);
    let view = opened.view.clone();
    // the app shows a chat, then the settings again, on the same page
    opened.cx.update(|_, cx| view.update(cx, |view, cx| view.put_away(cx)));
    server
        .users
        .lock()
        .unwrap()
        .push(user(4, "zoe@example.test", "Zoe Roe", false));
    opened.cx.update(|_, cx| view.update(cx, |_, cx| cx.notify()));
    opened.settle();
    assert_eq!(opened.listed().len(), 4);
    assert_eq!(server.asked(), ["list q= after=-", "list q= after=-"]);
}
