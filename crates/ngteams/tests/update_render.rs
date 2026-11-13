//! The release updater registered against a real desktop, rendered through GPUI's entity leasing path.
//! All account state, releases and installation files are synthetic; no platform GUI or network service is used.

// Test-only helpers, including the shared fake core.
#![cfg(test)]
// The fake feed's release trees are Linux's. A separate attribute keeps the literal `cfg(test)` clippy's test
// allowances look for.
#![cfg(unix)]

#[path = "../../teams-ui/tests/common/mod.rs"]
pub mod common;
#[path = "../../teams-ui/tests/common/parts.rs"]
mod parts;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AppContext as _, VisualTestContext};
use ngteams::update::{self, Started, Updater};
use teams_ui::ViewTrace;
use teams_ui::desktop::{Desktop, DesktopParts};
use teams_ui::settings::Settings;
use teams_ui::update::{self as button, UpdateStatus};
use teams_update::fakes::{Feed, tree};
use teams_update::{Format, Installation, Places, Source, Version};

#[test]
fn a_release_updater_renders_without_reading_the_shell_it_is_rendering() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::Archive).unwrap();
    let installation = Installation::detect(&Places {
        executable: root.join("usr/bin/ngteams"),
        appimage: None,
        appdir: None,
        stage_dir: home.path().join("updates"),
        protected: Vec::new(),
    })
    .unwrap();
    let feed = Feed::start().unwrap();
    let rig = common::Rig::start();
    let shell = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let desktop = rig.cx.update(|cx| {
        cx.new(|cx| {
            Desktop::new(
                DesktopParts {
                    app_id: "ngteams-test",
                    app_name: "Teams (test)".into(),
                    title: "Teams (test)".into(),
                    shell,
                    account_name: "Lab A".into(),
                    account_detail: "synthetic account".into(),
                    can_add: false,
                    ringer: None,
                    pictures_dir: None,
                },
                cx,
            )
        })
    });
    let reads = Rc::new(Cell::new(0));
    let counted = Rc::clone(&reads);
    let holds = update::holds(desktop.downgrade());
    rig.cx.update(|cx| {
        update::start(
            Started {
                updater: Updater {
                    current: Version::parse("0.1.0.5").unwrap(),
                    installation,
                    source: Source::local(&feed.base),
                },
                holds: Rc::new(move |cx| {
                    counted.set(counted.get() + 1);
                    holds(cx)
                }),
                restart: Rc::default(),
                quit: Rc::new(|_| panic!("rendering must not quit")),
                first_check: None,
            },
            cx,
        );
    });
    let window = rig.cx.update(|cx| desktop.update(cx, Desktop::open_window)).unwrap();
    let mut visual = VisualTestContext::from_window(window.into(), &rig.cx);
    for status in [
        UpdateStatus::None,
        UpdateStatus::Available("0.1.0.7".into()),
        UpdateStatus::Ready("0.1.0.7".into()),
    ] {
        let visible = status != UpdateStatus::None;
        visual.update(|window, cx| {
            button::set_status(status, cx);
            window.refresh();
            let _ = window.draw(cx);
        });
        assert_eq!(visual.debug_bounds("title-update").is_some(), visible);
    }
    assert_eq!(reads.get(), 0, "drawing never traverses the desktop's shells");
    rig.cx.update(|cx| assert_eq!(button::waiting(cx), None));
    assert_eq!(reads.get(), 1, "the live holds callback remains registered");
}
