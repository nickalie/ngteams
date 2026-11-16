//! The updates' flow in the app, end to end against a local fake release feed and a disposable installation in a
//! temporary folder: the check, the button's states, the download, the install and the restart asked of `main`.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::TestAppContext;
use teams_ui::update::{self as button, UpdateStatus, Waiting};
use teams_update::fakes::{Feed, Reply, archive_bytes, program, publish, tree};
use teams_update::{Format, IDENTITY, Installation, Places, Restart, Source, Version};

use super::{Started, Updater, check, percent, start, updating_build};

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn places(executable: PathBuf, home: &Path) -> Places {
    Places {
        executable,
        appimage: None,
        appdir: None,
        stage_dir: home.join("data/updates"),
        protected: vec![home.join("data"), home.join("config")],
    }
}

/// A test's app: the updater over `installation` and `feed`, what holds its restart up in `holds`, and what it asked
/// of `main` and of the quit.
struct App {
    restart: Rc<Cell<Option<Restart>>>,
    quits: Rc<Cell<u32>>,
    holds: Rc<Cell<Option<Waiting>>>,
}

fn started(cx: &mut TestAppContext, installation: Installation, feed: &Feed) -> App {
    let app = App {
        restart: Rc::default(),
        quits: Rc::default(),
        holds: Rc::default(),
    };
    let (restart, quits, holds) = (Rc::clone(&app.restart), Rc::clone(&app.quits), Rc::clone(&app.holds));
    let source = Source::local(&feed.base);
    cx.update(|cx| {
        start(
            Started {
                updater: Updater {
                    current: version("0.1.0.5"),
                    installation,
                    source,
                },
                holds: Rc::new(move |_| holds.get()),
                restart,
                quit: Rc::new(move |_| quits.set(quits.get() + 1)),
                first_check: None,
            },
            cx,
        );
    });
    app
}

fn checked(cx: &mut TestAppContext) {
    let checking = cx.update(check);
    assert!(checking.is_some(), "a check runs");
    cx.run_until_parked();
}

fn pressed(cx: &mut TestAppContext) -> Option<Waiting> {
    let waiting = cx.update(button::press);
    cx.run_until_parked();
    waiting
}

fn status(cx: &mut TestAppContext) -> UpdateStatus {
    cx.update(|cx| button::status(cx))
}

/// An unpacked release of 0.1.0.5 in `home`, and a feed offering `asset_version`'s archive (its bytes).
fn archive_case(home: &Path, asset_version: &str) -> (Installation, Feed, Vec<u8>) {
    let root = home.join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::Archive).unwrap();
    let installation = Installation::detect(&places(root.join("usr/bin/ngteams"), home)).unwrap();
    let feed = Feed::start().unwrap();
    let asset = Format::Archive.asset(version(asset_version));
    let bytes = archive_bytes(asset_version, |_| {}).unwrap();
    publish(&feed, &format!("v{asset_version}"), &asset, &bytes);
    (installation, feed, bytes)
}

#[gpui::test]
fn a_newer_release_downloads_by_itself_and_the_press_restarts_into_it(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let (installation, feed, _) = archive_case(home.path(), "0.1.0.7");
    let root = installation.root().to_path_buf();
    let app = started(cx, installation, &feed);
    assert_eq!(status(cx), UpdateStatus::None, "nothing before a check");
    checked(cx);
    // downloaded and verified after the check, with no press: only the restart is offered
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.7".into()));
    assert_eq!(
        fs::read_to_string(root.join(IDENTITY)).unwrap(),
        "0.1.0.5\n",
        "nothing installed before the press"
    );
    assert_eq!(app.quits.get(), 0);
    assert!(cx.update(check).is_none(), "no check while a download waits");
    assert_eq!(pressed(cx), None);
    assert_eq!(fs::read_to_string(root.join(IDENTITY)).unwrap(), "0.1.0.7\n");
    assert_eq!(
        app.restart.take().map(|restart| restart.program),
        Some(root.join("usr/bin/ngteams"))
    );
    assert_eq!(app.quits.get(), 1, "the app quits for its restart");
    assert_eq!(status(cx), UpdateStatus::Installing("0.1.0.7".into()));
    // no check while the update is in
    assert!(cx.update(check).is_none());
}

#[gpui::test]
fn a_call_or_an_unsent_message_holds_the_restart_until_the_press(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let (installation, feed, _) = archive_case(home.path(), "0.1.0.7");
    let root = installation.root().to_path_buf();
    let app = started(cx, installation, &feed);
    app.holds.set(Some(Waiting::Call));
    checked(cx);
    // the download goes on during a call
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.7".into()));
    assert_eq!(
        fs::read_to_string(root.join(IDENTITY)).unwrap(),
        "0.1.0.5\n",
        "nothing installed yet"
    );
    assert!(cx.update(check).is_none(), "no check while a download waits");
    // pressed while the call is up: it says so, and nothing happens
    assert_eq!(pressed(cx), Some(Waiting::Call));
    app.holds.set(Some(Waiting::Unsent));
    assert_eq!(pressed(cx), Some(Waiting::Unsent));
    assert_eq!(app.quits.get(), 0);
    app.holds.set(None);
    assert_eq!(pressed(cx), None);
    assert_eq!(fs::read_to_string(root.join(IDENTITY)).unwrap(), "0.1.0.7\n");
    assert_eq!(app.quits.get(), 1);
    assert!(app.restart.take().is_some());
}

#[gpui::test]
fn a_bad_download_says_update_failed_and_the_next_check_or_a_press_tries_again(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let (installation, feed, bytes) = archive_case(home.path(), "0.1.0.7");
    let root = installation.root().to_path_buf();
    let asset = Format::Archive.asset(version("0.1.0.7"));
    let path = format!("/download/v0.1.0.7/{asset}");
    feed.serve(&path, Reply::ok(vec![0; 10]));
    let app = started(cx, installation, &feed);
    checked(cx);
    let UpdateStatus::Failed { version, reason } = status(cx) else {
        panic!("{:?}", status(cx));
    };
    assert_eq!(version, "0.1.0.7");
    assert!(reason.contains("SHA256SUMS"), "{reason}");
    assert_eq!(fs::read_to_string(root.join(IDENTITY)).unwrap(), "0.1.0.5\n");
    assert_eq!(app.quits.get(), 0);
    // the press tries again at once: still bad, still said
    assert_eq!(pressed(cx), None);
    assert!(matches!(status(cx), UpdateStatus::Failed { .. }));
    // the release's file mended: the next check downloads it again by itself
    feed.serve(&path, Reply::ok(bytes));
    checked(cx);
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.7".into()));
    assert_eq!(app.quits.get(), 0);
    assert_eq!(pressed(cx), None);
    assert_eq!(fs::read_to_string(root.join(IDENTITY)).unwrap(), "0.1.0.7\n");
    assert_eq!(app.quits.get(), 1);
}

#[gpui::test]
fn a_download_cut_by_the_network_or_a_rate_limit_shows_nothing_and_the_next_check_tries_again(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let (installation, feed, bytes) = archive_case(home.path(), "0.1.0.7");
    let asset = Format::Archive.asset(version("0.1.0.7"));
    let path = format!("/download/v0.1.0.7/{asset}");
    let app = started(cx, installation, &feed);
    for answer in ["429 Too Many Requests", "403 Forbidden"] {
        feed.serve(&path, Reply::status(answer));
        checked(cx);
        assert_eq!(status(cx), UpdateStatus::None, "{answer}");
    }
    feed.serve(&path, Reply::ok(bytes));
    checked(cx);
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.7".into()));
    assert_eq!(app.quits.get(), 0);
}

#[gpui::test]
fn offline_rate_limited_or_nothing_newer_shows_nothing(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let (installation, feed, _) = archive_case(home.path(), "0.1.0.5");
    let app = started(cx, installation, &feed);
    checked(cx);
    assert_eq!(status(cx), UpdateStatus::None, "the running version");
    for answer in ["429 Too Many Requests", "403 Forbidden", "500 Internal Server Error"] {
        feed.serve("/latest", Reply::status(answer));
        checked(cx);
        assert_eq!(status(cx), UpdateStatus::None, "{answer}");
    }
    drop(feed);
    checked(cx);
    assert_eq!(status(cx), UpdateStatus::None);
    // nothing offered: a press does nothing
    assert_eq!(pressed(cx), None);
    assert_eq!(app.quits.get(), 0);
}

#[gpui::test]
fn a_closed_password_prompt_keeps_the_download_for_the_next_press(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::Deb).unwrap();
    let prompter = home.path().join("pkexec");
    program(&prompter, "#!/bin/sh\nexit 126\n").unwrap();
    let installation = Installation::detect(&places(root.join("usr/bin/ngteams"), home.path()))
        .unwrap()
        .with_prompter(prompter.clone());
    let feed = Feed::start().unwrap();
    publish(&feed, "v0.1.0.6", &Format::Deb.asset(version("0.1.0.6")), b"package");
    let app = started(cx, installation, &feed);
    checked(cx);
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.6".into()));
    // the password prompt comes only with the press
    assert_eq!(pressed(cx), None);
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.6".into()));
    assert_eq!(app.quits.get(), 0);
    // the prompt answered this time (the package manager faked away): installed without another download
    feed.serve(
        "/download/v0.1.0.6/ngteams_0.1.0.6-1_amd64.deb",
        Reply::status("404 Not Found"),
    );
    program(&prompter, "#!/bin/sh\nexit 0\n").unwrap();
    assert_eq!(pressed(cx), None);
    assert_eq!(app.quits.get(), 1);
    assert_eq!(
        app.restart.take().map(|restart| restart.program),
        Some(fs::canonicalize(root.join("usr/bin/ngteams")).unwrap())
    );
    // the package manager failing: said, nothing kept
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::Pacman).unwrap();
    let prompter = home.path().join("pkexec-failing");
    program(
        &prompter,
        "#!/bin/sh\necho 'error: failed to commit transaction' >&2\nexit 1\n",
    )
    .unwrap();
    let installation = Installation::detect(&places(root.join("usr/bin/ngteams"), home.path()))
        .unwrap()
        .with_prompter(prompter);
    publish(&feed, "v0.1.0.6", &Format::Pacman.asset(version("0.1.0.6")), b"package");
    let failing = started(cx, installation, &feed);
    checked(cx);
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.6".into()));
    assert_eq!(pressed(cx), None);
    let UpdateStatus::Failed { reason, .. } = status(cx) else {
        panic!("{:?}", status(cx));
    };
    assert!(reason.contains("failed to commit transaction"), "{reason}");
    assert_eq!(failing.quits.get(), 0);
}

#[test]
fn only_a_release_build_without_a_lab_marker_or_test_support_updates() {
    let release = Some(version("0.1.0.5"));
    assert_eq!(updating_build(release, None, false), release);
    assert_eq!(updating_build(release, Some(""), false), release);
    assert_eq!(updating_build(release, Some("1"), false), None, "a lab build");
    assert_eq!(updating_build(release, None, true), None, "a test-support build");
    assert_eq!(updating_build(None, None, false), None, "a source build");
    // this test binary is none of them
    let paths = crate::accounts_file::PersonalPaths::new(
        PathBuf::from("/nonexistent/config"),
        PathBuf::from("/nonexistent/data"),
    );
    assert!(super::for_this_run(&paths).is_none());
}

#[test]
fn progress_is_whole_percent() {
    assert_eq!(percent(0, 0), 0);
    assert_eq!(percent(0, 10), 0);
    assert_eq!(percent(5, 10), 50);
    assert_eq!(percent(10, 10), 100);
    assert_eq!(percent(20, 10), 100);
    assert_eq!(percent(u64::MAX, u64::MAX), 100);
}

#[gpui::test]
fn a_hold_arriving_during_the_package_prompt_waits_for_a_restart_without_reinstalling(cx: &mut TestAppContext) {
    for hold in [Waiting::Call, Waiting::Unsent] {
        hold_arriving_during_the_package_prompt(hold, cx);
    }
}

/// One hold (`hold`) that arrives while the package prompt is open, as the test above runs it.
fn hold_arriving_during_the_package_prompt(hold: Waiting, cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::Deb).unwrap();
    let prompter = home.path().join("pkexec");
    let installed = home.path().join("installed");
    program(&prompter, &format!("#!/bin/sh\ntouch '{}'\n", installed.display())).unwrap();
    let installation = Installation::detect(&places(root.join("usr/bin/ngteams"), home.path()))
        .unwrap()
        .with_prompter(prompter.clone());
    let feed = Feed::start().unwrap();
    let asset = Format::Deb.asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, b"package");
    let app = started(cx, installation, &feed);
    let completed = installed.clone();
    let released = Rc::new(Cell::new(false));
    let resumed = Rc::clone(&released);
    // No hold before installation; one exists by the time the prompt returns.
    cx.update(|cx| {
        button::holds_with(
            Rc::new(move |_| (completed.exists() && !resumed.get()).then_some(hold)),
            cx,
        );
    });
    checked(cx);
    assert!(!installed.exists(), "the download alone asks for no password");
    assert_eq!(pressed(cx), None);
    assert!(installed.exists());
    assert_eq!(app.quits.get(), 0);
    assert!(app.restart.take().is_none(), "no restart armed while held");
    assert_eq!(status(cx), UpdateStatus::Ready("0.1.0.6".into()));
    assert_eq!(pressed(cx), Some(hold));
    assert!(cx.update(check).is_none());
    // Neither download nor installation may run again on the later press.
    feed.serve(&format!("/download/v0.1.0.6/{asset}"), Reply::status("404 Not Found"));
    fs::remove_file(prompter).unwrap();
    released.set(true);
    cx.run_until_parked();
    assert_eq!(app.quits.get(), 0, "clearing the hold never restarts automatically");
    assert_eq!(pressed(cx), None);
    assert_eq!(app.quits.get(), 1);
    assert_eq!(pressed(cx), None);
    assert_eq!(app.quits.get(), 1, "a second press while quitting does nothing");
    assert_eq!(
        app.restart.take().map(|restart| restart.program),
        Some(fs::canonicalize(root.join("usr/bin/ngteams")).unwrap())
    );
}

#[test]
fn path_lookup_failure_is_logged_with_the_reason() {
    struct Capture;
    thread_local! {
        static LINES: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            LINES.with_borrow_mut(|lines| lines.push(record.args().to_string()));
        }
        fn flush(&self) {}
    }
    log::set_logger(&Capture).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    assert!(super::for_this_copy::<()>(Err(std::io::Error::other("synthetic current_exe failure"))).is_none());
    assert_eq!(super::for_this_copy::<u8>(Ok::<_, std::io::Error>(7)), Some(7));
    LINES.with_borrow(|lines| {
        assert!(
            lines
                .iter()
                .any(|line| line == "updates: not for this copy (synthetic current_exe failure)")
        );
    });
}
