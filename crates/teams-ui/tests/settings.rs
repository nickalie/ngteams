//! The settings through the GPUI model: each change written at once, a failed save reported, a file that couldn't be
//! read never written over, the devices attached and the backgrounds looked for beside the file. `teams-app`'s
//! `tests/settings.rs` holds the file's own checks. Every file here is in a test's own temporary directory.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::PathBuf;

use gpui::{AppContext as _, TestAppContext};
use teams_media::devices::catalog::{DeviceDirectory as _, DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_media::video::effects::{Backdrop, Background};
use teams_ui::settings::{
    BarItem, ComposeTool, FirstRunState, Notifications, Problem, Settings, SettingsFile, SettingsModel, VideoEffect,
    attach_devices, parse, render,
};

const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// The settings file of lab account a under `dir`, as the lab app keeps it.
fn file_in(dir: &tempfile::TempDir) -> (SettingsFile, PathBuf) {
    let path = dir.path().join("ui").join("a.toml");
    (SettingsFile::new(&path), path)
}

#[test]
fn a_picture_the_user_added_is_looked_for_beside_the_settings() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (file, path) = file_in(&dir);
    let added = Settings {
        video_effect: VideoEffect::Added("0a1b.jpg".to_owned()),
        ..Settings::default()
    };
    file.save(&added).expect("saved");
    let model = SettingsModel::load(file);
    let backgrounds = path.parent().expect("a directory").join("backgrounds");
    assert_eq!(model.backgrounds_dir(), Some(backgrounds.clone()));
    assert_eq!(
        model.background(),
        Background::Picture(Backdrop::File(backgrounds.join("0a1b.jpg")))
    );
    let in_memory = SettingsModel::in_memory(added);
    assert_eq!(in_memory.backgrounds_dir(), None);
    assert_eq!(
        in_memory.background(),
        Background::Blur,
        "nothing to look in: blurred, never as it is"
    );
}

#[test]
fn the_calls_video_is_on_by_default_and_its_switch_shown_only_where_the_calls_have_it() {
    assert!(Settings::default().video_calls, "video on until the user turns it off");
    let (read, problems) = parse("video_calls = false").expect("TOML");
    assert!(!read.video_calls);
    assert!(problems.is_empty());
    let (read, problems) = parse("video_calls = \"no\"").expect("TOML");
    assert!(read.video_calls, "a bad value keeps the default");
    assert_eq!(problems.len(), 1);
    assert!(render(&read).contains("video_calls = true"));
    assert!(!SettingsModel::in_memory(Settings::default()).shows_video_switch());
    assert!(
        SettingsModel::in_memory(Settings::default())
            .with_video_switch()
            .shows_video_switch()
    );
}

#[gpui::test]
fn the_model_writes_each_change_at_once_and_only_a_change(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    let model = cx.new(|_| SettingsModel::load(file));
    assert!(!path.exists());

    let changes = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = std::rc::Rc::clone(&changes);
    let _observing = cx.update(|cx| cx.observe(&model, move |_, _| seen.set(seen.get() + 1)));
    // the same value again changes nothing: nothing is written, nobody is told
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.send_typing = true, cx)));
    assert!(!path.exists());
    assert_eq!(changes.get(), 0);

    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.notifications = Notifications::Off, cx);
        });
    });
    assert_eq!(changes.get(), 1);
    assert_eq!(
        SettingsFile::new(&path).load().settings.notifications,
        Notifications::Off
    );
}

#[gpui::test]
fn a_save_that_fails_is_reported_and_the_change_holds(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    // No file yet (defaults, no problem). A plain file then obstructs creation of the settings directory.
    let (file, path) = file_in(&dir);
    let model = cx.new(|_| SettingsModel::load(file));
    assert!(cx.read(|cx| model.read(cx).problems().is_empty()));
    fs::write(path.parent().unwrap(), "synthetic obstruction").unwrap();
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.idle_minutes = 30, cx)));
    let unsaved = |model: &SettingsModel| {
        model
            .problems()
            .iter()
            .filter(|problem| matches!(problem, Problem::Unsaved { .. }))
            .count()
    };
    cx.read(|cx| {
        let model = model.read(cx);
        assert_eq!(model.settings().idle_minutes, 30);
        assert_eq!(unsaved(model), 1, "{:?}", model.problems());
    });
    // a later failure replaces it: one at a time
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.idle_minutes = 60, cx)));
    assert_eq!(cx.read(|cx| unsaved(model.read(cx))), 1);
    assert!(!path.exists());
    fs::remove_file(path.parent().unwrap()).unwrap();
}

#[gpui::test]
fn a_file_that_couldn_t_be_read_is_never_written_over(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    // the user's settings, with a slip that makes the file not TOML
    let text = "notifications = \"mentions\"\nidle_minutes = 30\n[broken\n";
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(&path, text).expect("written");
    let model = cx.new(|_| SettingsModel::load(file));
    cx.read(|cx| {
        let model = model.read(cx);
        assert_eq!(model.settings(), &Settings::default());
        assert!(matches!(model.problems(), [Problem::Unreadable { .. }]));
    });

    // a chat opened (as the shell keeps it) and a setting changed: both hold in memory, the file stays as it was
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.selected_chat = Some(CHAT.to_owned()), cx);
            model.change(|settings| settings.send_typing = false, cx);
        });
    });
    assert_eq!(fs::read_to_string(&path).expect("read"), text);
    cx.read(|cx| {
        let model = model.read(cx);
        assert_eq!(model.settings().selected_chat.as_deref(), Some(CHAT));
        assert!(!model.settings().send_typing);
        let problems = model.problems();
        assert!(matches!(problems[0], Problem::Unreadable { .. }), "{problems:?}");
        assert!(
            matches!(&problems[1..], [Problem::Unsaved { why, .. }] if why.contains("couldn't be read")),
            "{problems:?}"
        );
    });
}

#[gpui::test]
fn nothing_writes_on_its_own_until_the_first_run_is_confirmed(cx: &mut TestAppContext) {
    // the lab app doesn't ask: each switch says
    let lab = SettingsModel::in_memory(Settings::default());
    assert!(!lab.awaiting_first_run());
    assert!(lab.sends_typing() && lab.sends_read_receipts() && lab.sends_presence());
    // the personal build asks: nothing until it is confirmed, whatever the switches say
    let model = cx.new(|_| SettingsModel::in_memory(Settings::default()).requiring_first_run());
    cx.read(|cx| {
        let model = model.read(cx);
        assert!(model.awaiting_first_run());
        assert!(!model.sends_typing() && !model.sends_read_receipts() && !model.sends_presence());
    });
    model.update(cx, |model, cx| {
        model.change(|settings| settings.send_read_receipts = false, cx);
        model.confirm_first_run(cx);
    });
    cx.read(|cx| {
        let model = model.read(cx);
        assert!(!model.awaiting_first_run());
        assert!(model.sends_typing() && !model.sends_read_receipts() && model.sends_presence());
        assert_eq!(model.settings().first_run, FirstRunState::Confirmed);
    });
    // confirmed once, kept in the file
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, _) = file_in(&dir);
    let saved = cx.new(|_| SettingsModel::load(file.clone()).requiring_first_run());
    saved.update(cx, SettingsModel::confirm_first_run);
    assert!(!SettingsModel::load(file).requiring_first_run().awaiting_first_run());
}

#[gpui::test]
fn the_ai_notice_waits_after_the_first_run_and_its_answer_is_kept(cx: &mut TestAppContext) {
    // the views of tests do not ask: the AI features run
    let lab = SettingsModel::in_memory(Settings::default());
    assert!(!lab.awaiting_ai_notice());
    assert!(lab.uses_ai());
    // the app asks after the first run's screen, and no AI feature runs before the answer
    let model = cx.new(|_| SettingsModel::in_memory(Settings::default()).requiring_first_run());
    cx.read(|cx| {
        let model = model.read(cx);
        assert!(model.awaiting_first_run() && !model.awaiting_ai_notice());
        assert!(!model.uses_ai());
    });
    model.update(cx, SettingsModel::confirm_first_run);
    cx.read(|cx| assert!(model.read(cx).awaiting_ai_notice() && !model.read(cx).uses_ai()));
    model.update(cx, |model, cx| model.set_ai_features(false, cx));
    cx.read(|cx| {
        let model = model.read(cx);
        assert!(!model.awaiting_ai_notice() && !model.uses_ai());
        assert!(
            !teams_ui::local_ai::enabled(cx),
            "the answer is the app's switch at once"
        );
    });
    model.update(cx, |model, cx| model.set_ai_features(true, cx));
    cx.read(|cx| assert!(model.read(cx).uses_ai() && teams_ui::local_ai::enabled(cx)));
    // an upgrade from a build without it: the first run was confirmed, the notice still asks once
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    fs::create_dir_all(path.parent().expect("a directory")).expect("made");
    fs::write(&path, "first_run = \"confirmed\"\n").expect("written");
    let upgraded = cx.new(|_| SettingsModel::load(file.clone()).requiring_first_run());
    cx.read(|cx| assert!(upgraded.read(cx).awaiting_ai_notice()));
    upgraded.update(cx, |model, cx| model.set_ai_features(false, cx));
    let again = SettingsModel::load(file.clone()).requiring_first_run();
    assert!(!again.awaiting_ai_notice(), "answered once, kept in the file");
    assert!(!again.uses_ai());
    // the read-only mode never asks, and runs no AI feature before an answer
    let read_only = SettingsModel::in_memory(Settings {
        first_run: FirstRunState::Confirmed,
        ..Settings::default()
    })
    .requiring_first_run()
    .read_only();
    assert!(!read_only.awaiting_ai_notice() && !read_only.uses_ai());
}

fn device(kind: DeviceKind, name: &str, description: &str) -> DeviceInfo {
    DeviceInfo {
        kind,
        name: name.to_owned(),
        description: description.to_owned(),
    }
}

#[gpui::test]
fn attached_devices_take_each_choice_at_once_and_on_every_change_and_their_list_follows(cx: &mut TestAppContext) {
    let directory = FixedDirectory::new(DeviceList::new(vec![device(
        DeviceKind::Microphone,
        "mic",
        "Microphone",
    )]));
    let settings = Settings {
        camera: Some("cam".to_owned()),
        ..Settings::default()
    };
    let model = cx.new(|_| SettingsModel::in_memory(settings));
    assert!(
        model.read_with(cx, |model, _| model.devices().is_none()),
        "no devices before they are attached"
    );
    cx.update(|cx| attach_devices(&model, std::sync::Arc::new(directory.clone()), cx));
    assert_eq!(
        directory.choices_made(),
        [
            (DeviceKind::Microphone, None),
            (DeviceKind::Speaker, None),
            (DeviceKind::Camera, Some("cam".to_owned()))
        ],
        "every kind's choice handed over at once"
    );
    assert_eq!(
        model.read_with(cx, |model, _| model.devices().map(|list| list.all().len())),
        Some(1)
    );

    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(
                |settings| settings.set_device(DeviceKind::Microphone, Some("mic".to_owned())),
                cx,
            );
        });
    });
    assert_eq!(directory.chosen(DeviceKind::Microphone).as_deref(), Some("mic"));
    let made = directory.choices_made().len();
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.send_typing = false, cx);
        });
    });
    assert_eq!(
        directory.choices_made().len(),
        made,
        "a change of another setting moves no device"
    );

    // a device plugged in: the list the settings show follows
    directory.set_devices(DeviceList::new(vec![
        device(DeviceKind::Microphone, "mic", "Microphone"),
        device(DeviceKind::Speaker, "usb", "USB headset"),
    ]));
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |model, _| model.devices().map(|list| list.all().len())),
        Some(2)
    );
}

#[gpui::test]
fn a_drafts_translation_language_set_is_there_after_a_restart(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (file, _) = file_in(&dir);
    let model = cx.update(|cx| cx.new(|_| SettingsModel::load(file)));
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.draft_translate_to = "Klingon".to_owned(), cx);
        });
    });
    let (file, _) = file_in(&dir);
    let restarted = SettingsModel::load(file);
    assert_eq!(restarted.settings().draft_translate_to, "Klingon");
    assert_eq!(restarted.settings().message_translate_to, "English");
}

#[gpui::test]
fn settings_follow_media_preparation_and_failure_even_when_the_list_stays_empty(cx: &mut TestAppContext) {
    use teams_media::devices::catalog::DeviceStatus;
    let directory = FixedDirectory::default();
    directory.set_status(DeviceStatus::Preparing);
    let model = cx.new(|_| SettingsModel::in_memory(Settings::default()));
    assert_eq!(
        model.read_with(cx, |model, _| model.media_status()),
        DeviceStatus::Ready
    );
    cx.update(|cx| attach_devices(&model, std::sync::Arc::new(directory.clone()), cx));
    assert_eq!(
        model.read_with(cx, |model, _| model.media_status()),
        DeviceStatus::Preparing
    );
    directory.set_status(DeviceStatus::Unavailable);
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |model, _| model.media_status()),
        DeviceStatus::Unavailable
    );
    directory.set_status(DeviceStatus::Ready);
    cx.run_until_parked();
    assert_eq!(
        model.read_with(cx, |model, _| model.media_status()),
        DeviceStatus::Ready
    );
}

#[gpui::test]
fn a_save_after_one_bad_hover_bar_table_keeps_the_other_account_s_choice(cx: &mut TestAppContext) {
    const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    fs::create_dir_all(path.parent().expect("dir")).expect("dir");
    fs::write(
        &path,
        format!("[message_bar.\"{A}\"]\nhidden = [\"reply\"]\n[message_bar.\"{B}\"]\nhidden = \"reply\"\n"),
    )
    .expect("written");
    let model = cx.new(|_| SettingsModel::load(file));
    cx.update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.idle_minutes = 30, cx)));
    let saved = SettingsFile::new(&path).load().settings;
    assert_eq!(saved.idle_minutes, 30, "the change is saved");
    assert!(!saved.shows_bar_item(A, BarItem::Reply), "{A} keeps its choice");
}

#[gpui::test]
fn a_shown_ai_tool_is_still_shown_after_a_restart(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (file, _) = file_in(&dir);
    let model = cx.update(|cx| cx.new(|_| SettingsModel::load(file)));
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.show_compose_tool(ComposeTool::Correct, true), cx);
        });
    });
    let (file, _) = file_in(&dir);
    let restarted = SettingsModel::load(file);
    assert_eq!(restarted.settings().compose_shown, [ComposeTool::Correct]);
    assert!(restarted.settings().shows_compose_tool(ComposeTool::Correct));
    assert!(!restarted.settings().shows_compose_tool(ComposeTool::Translate));
}

#[gpui::test]
fn a_hidden_toolbar_button_is_still_hidden_after_a_restart(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (file, _) = file_in(&dir);
    let model = cx.update(|cx| cx.new(|_| SettingsModel::load(file)));
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.show_compose_tool(ComposeTool::Attach, false), cx);
        });
    });
    let (file, _) = file_in(&dir);
    let restarted = SettingsModel::load(file);
    assert_eq!(restarted.settings().compose_hidden, [ComposeTool::Attach]);
    assert!(!restarted.settings().shows_compose_tool(ComposeTool::Attach));
}
