//! The settings and their file: defaults for a missing or unreadable file, a bad value reported with its default kept,
//! the file `0600` in a `0700` directory, written whole; the model writing each change at once. Every file here is in
//! a test's own temporary directory.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
#[path = "../../teams-platform/tests/support/privacy.rs"]
mod privacy;
use std::path::{Path, PathBuf};

use gpui::{AppContext as _, TestAppContext};
use teams_media::devices::catalog::{DeviceDirectory as _, DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_media::video::effects::{Backdrop, Background};
use teams_ui::local_ai::ToolChoice as TranslatorChoice;
use teams_ui::local_ai::{AiChoice, Model};
use teams_ui::settings::{
    AccountSettings, DEFAULT_IDLE_MINUTES, FirstRunState, LogLevel, ManualStatus, Notifications, Problem, Settings,
    SettingsFile, SettingsModel, TimeFormat, VideoEffect, attach_devices, parse, render,
};
use teams_ui::theme::{self, Appearance, THEMES};
use teams_ui::translate::Backend;

const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// Give an unrelated user access so the application's load/save must tighten the native descriptor.
fn loosen(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(windows)]
    {
        // Change only the DACL: round-tripping Get-Acl's full descriptor can try to write the mandatory label
        // and require SeSecurityPrivilege, unrelated to this synthetic access-grant contract.
        let result = std::process::Command::new("icacls.exe")
            .arg(path)
            .args(["/grant", "*S-1-1-0:(F)", "/q"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    }
}

/// The settings file of lab account a under `dir`, as the lab app keeps it.
fn file_in(dir: &tempfile::TempDir) -> (SettingsFile, PathBuf) {
    let path = dir.path().join("ui").join("a.toml");
    (SettingsFile::new(&path), path)
}

fn changed() -> Settings {
    Settings {
        selected_chat: Some(CHAT.to_owned()),
        send_typing: false,
        send_read_receipts: false,
        send_presence: false,
        first_run: FirstRunState::Confirmed,
        notifications: Notifications::Mentions,
        meeting_started: false,
        idle_minutes: 15,
        manual_status: ManualStatus::DoNotDisturb,
        log_level: LogLevel::Debug,
        video_calls: false,
        save_call_captions: true,
        microphone: Some("alsa_input.usb-headset".to_owned()),
        speaker: None,
        camera: Some("v4l2_camera".to_owned()),
        video_effect: VideoEffect::Stock("waves"),
        appearance: Appearance {
            follow_system: false,
            theme: "nord",
            light_theme: "solarized-light",
            dark_theme: "high-contrast",
            font_size: 17,
            font_family: Some("Noto Sans".to_owned()),
            animations: false,
        },
        time_format: TimeFormat::Twelve,
        caption_translate_to: Some("de"),
        caption_translator: AiChoice {
            tool: TranslatorChoice::Only(Backend::Codex),
            model: Model::Named("gpt-6.1-sol".to_owned()),
        },
        recap_summarizer: AiChoice {
            tool: TranslatorChoice::Only(Backend::Claude),
            model: Model::ToolDefault,
        },
        accounts: [(
            "8:orgid:00000000-0000-4000-8000-00000000000b".to_owned(),
            AccountSettings {
                selected_chat: Some(CHAT.to_owned()),
                manual_status: ManualStatus::Busy,
            },
        )]
        .into(),
        first_account: None,
        people_calendars: vec![teams_ui::settings::PersonCalendar {
            account: "8:orgid:00000000-0000-4000-8000-00000000000a".to_owned(),
            address: "avery@example.com".to_owned(),
            name: "Avery Example".to_owned(),
        }],
        chat_languages: vec![teams_ui::settings::ChatLanguage {
            account: "8:orgid:00000000-0000-4000-8000-00000000000a".to_owned(),
            chat: CHAT.to_owned(),
            language: "de",
        }],
        sounds: teams_ui::sounds::SoundSettings::default(),
        chat_translator: AiChoice {
            tool: TranslatorChoice::Only(Backend::Codex),
            model: teams_ui::local_ai::Model::Named("my-team-model".to_owned()),
        },
        message_corrector: AiChoice {
            tool: TranslatorChoice::Only(Backend::Claude),
            model: teams_ui::local_ai::Model::ToolDefault,
        },
        message_translate_to: "Klingon".to_owned(),
        message_translator: AiChoice {
            tool: TranslatorChoice::Only(Backend::Codex),
            model: teams_ui::local_ai::Model::Recommended,
        },
    }
}

#[test]
fn the_video_effect_is_none_by_default_and_kept_by_its_key() {
    assert_eq!(Settings::default().video_effect, VideoEffect::None);
    for (text, effect) in [
        ("video_effect = \"blur\"", VideoEffect::Blur),
        ("video_effect = \"stock/forest\"", VideoEffect::Stock("forest")),
        (
            "video_effect = \"added/0a1b2c3d.jpg\"",
            VideoEffect::Added("0a1b2c3d.jpg".to_owned()),
        ),
        ("video_effect = \"none\"", VideoEffect::None),
    ] {
        let (read, problems) = parse(text).expect("TOML");
        assert_eq!(read.video_effect, effect, "{text}");
        assert!(problems.is_empty(), "{text}");
        assert_eq!(
            parse(&render(&read)).expect("TOML").0.video_effect,
            effect,
            "{text} round-trips"
        );
    }
    for bad in [
        "stock/nothing",
        "added/../settings.toml",
        "added/.hidden",
        "sparkles",
        "added/a/b.jpg",
    ] {
        let (read, problems) = parse(&format!("video_effect = \"{bad}\"")).expect("TOML");
        assert_eq!(read.video_effect, VideoEffect::None, "{bad} keeps the default");
        assert_eq!(problems.len(), 1, "{bad} is reported");
    }
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

#[test]
fn the_log_level_is_info_by_default_and_read_from_the_file() {
    assert_eq!(Settings::default().log_level, LogLevel::Info);
    for level in LogLevel::ALL {
        let settings = Settings {
            log_level: level,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains(&format!("log_level = \"{}\"", level.key())), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty());
    }
    // `trace` is `debug`; any other name is reported and the default kept
    let (read, problems) = parse("log_level = \"trace\"").expect("TOML");
    assert_eq!(read.log_level, LogLevel::Debug);
    assert!(problems.is_empty());
    let (read, problems) = parse("log_level = \"loud\"").expect("TOML");
    assert_eq!(read.log_level, LogLevel::Info);
    assert!(matches!(&problems[..], [Problem::BadValue { key: "log_level", .. }]));
}

#[test]
fn a_missing_file_gives_the_defaults_and_no_problem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    let loaded = file.load();
    assert_eq!(loaded.settings, Settings::default());
    assert!(loaded.problems.is_empty());
    assert!(!path.exists(), "reading writes nothing");
    let defaults = Settings::default();
    assert!(defaults.send_typing && defaults.send_read_receipts);
    assert_eq!(defaults.notifications, Notifications::All);
    assert_eq!(defaults.idle_minutes, DEFAULT_IDLE_MINUTES);
    assert_eq!(defaults.manual_status, ManualStatus::Automatic);
    assert_eq!(defaults.selected_chat, None);
}

#[test]
fn saved_settings_are_read_back_from_a_private_file_in_a_private_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    file.save(&changed()).expect("saved");
    privacy::assert_private(&path, false);
    privacy::assert_private(path.parent().expect("dir"), false);
    let loaded = file.load();
    assert_eq!(loaded.settings, changed());
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    // nothing but the file is left in its directory
    let names: Vec<_> = fs::read_dir(path.parent().expect("dir"))
        .expect("listed")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names, ["a.toml"]);
}

#[test]
fn every_choice_goes_through_the_file() {
    for notifications in Notifications::ALL {
        for manual_status in ManualStatus::ALL {
            let settings = Settings {
                notifications,
                manual_status,
                ..Settings::default()
            };
            let (read, problems) = parse(&render(&settings)).expect("TOML");
            assert_eq!(read, settings);
            assert!(problems.is_empty());
        }
    }
}

#[test]
fn a_file_that_isn_t_toml_or_can_t_be_read_gives_the_defaults_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    fs::create_dir_all(path.parent().expect("dir")).expect("dir");
    fs::write(&path, "send_typing = [unclosed").expect("written");
    let loaded = file.load();
    assert_eq!(loaded.settings, Settings::default());
    assert!(
        matches!(&loaded.problems[..], [Problem::Unreadable { why, .. }] if why.contains("TOML")),
        "{:?}",
        loaded.problems
    );

    fs::remove_file(&path).expect("removed");
    fs::create_dir(&path).expect("a directory where the file goes");
    let loaded = file.load();
    assert_eq!(loaded.settings, Settings::default());
    assert!(matches!(&loaded.problems[..], [Problem::Unreadable { .. }]));
}

#[test]
fn a_bad_value_is_reported_and_its_default_kept_the_rest_taken() {
    let text = format!(
        "selected_chat = {CHAT:?}\nsend_typing = \"yes\"\nsend_read_receipts = false\nnotifications = \"loud\"\n\
         idle_minutes = 0\nmanual_status = \"busy\"\ncolour = \"purple\"\n"
    );
    let (settings, problems) = parse(&text).expect("TOML");
    assert_eq!(
        settings,
        Settings {
            selected_chat: Some(CHAT.to_owned()),
            send_read_receipts: false,
            manual_status: ManualStatus::Busy,
            ..Settings::default()
        }
    );
    let keys: Vec<String> = problems
        .iter()
        .map(|problem| match problem {
            Problem::BadValue { key, .. } => (*key).to_owned(),
            Problem::UnknownKey { key } => format!("unknown {key}"),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(keys, ["unknown colour", "idle_minutes", "notifications", "send_typing"]);
    for problem in &problems {
        assert!(problem.to_string().contains("setting") || problem.to_string().contains("default"));
    }

    for bad in [
        "idle_minutes = 241",
        "idle_minutes = -5",
        "idle_minutes = 2.5",
        "selected_chat = \"\"",
        "selected_chat = \"has space\"",
        "selected_chat = 7",
        "manual_status = \"Busy\"",
    ] {
        let (settings, problems) = parse(bad).expect("TOML");
        assert_eq!(settings, Settings::default(), "{bad}");
        assert_eq!(problems.len(), 1, "{bad}");
    }
}

#[test]
fn a_file_readable_by_others_is_tightened_when_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    file.save(&changed()).expect("saved");
    loosen(&path);
    let loaded = file.load();
    assert_eq!(loaded.settings, changed());
    privacy::assert_private(&path, false);
}

#[test]
fn a_save_tightens_a_directory_open_to_others() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (file, path) = file_in(&dir);
    let parent = path.parent().expect("dir");
    fs::create_dir_all(parent).expect("dir");
    loosen(parent);
    file.save(&Settings::default()).expect("saved");
    privacy::assert_private(parent, false);
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

#[test]
fn only_the_captured_manual_statuses_are_kept_and_each_is_the_policy_s_choice() {
    use teams_core::Choice;
    use teams_proto::ManualAvailability;
    // "Be right back" and "Appear offline" were never captured (M2 Task 18): reported, Automatic kept
    for uncaptured in ["be-right-back", "appear-offline"] {
        let (read, problems) = parse(&format!("manual_status = \"{uncaptured}\"")).expect("TOML");
        assert_eq!(read.manual_status, ManualStatus::Automatic, "{uncaptured}");
        assert!(
            matches!(
                &problems[..],
                [Problem::BadValue {
                    key: "manual_status",
                    ..
                }]
            ),
            "{uncaptured}: {problems:?}"
        );
    }
    let choices: Vec<Choice> = ManualStatus::ALL.iter().map(|status| status.choice()).collect();
    assert_eq!(
        choices,
        [
            Choice::Automatic,
            Choice::Manual(ManualAvailability::Available),
            Choice::Manual(ManualAvailability::Busy),
            Choice::Manual(ManualAvailability::DoNotDisturb),
            Choice::Manual(ManualAvailability::Away),
        ]
    );
}

#[test]
fn presence_is_sent_and_the_first_run_waits_by_default_and_both_go_through_the_file() {
    let defaults = Settings::default();
    assert!(defaults.send_presence);
    assert_eq!(defaults.first_run, FirstRunState::Waiting);
    let (read, problems) = parse("send_presence = false\nfirst_run = \"confirmed\"").expect("TOML");
    assert!(!read.send_presence);
    assert_eq!(read.first_run, FirstRunState::Confirmed);
    assert!(problems.is_empty());
    let (read, problems) = parse("first_run = \"skipped\"").expect("TOML");
    assert_eq!(read.first_run, FirstRunState::Waiting);
    assert!(matches!(&problems[..], [Problem::BadValue { key: "first_run", .. }]));
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

#[test]
fn the_calls_devices_follow_the_system_by_default_and_a_chosen_one_is_kept_by_name() {
    let defaults = Settings::default();
    for kind in DeviceKind::ALL {
        assert_eq!(defaults.device(kind), None, "{kind:?} follows the system");
    }
    let text = render(&defaults);
    for key in ["microphone", "speaker", "camera"] {
        assert!(!text.contains(key), "nothing written for the system's: {text}");
    }
    let (read, problems) =
        parse("microphone = \"alsa_input.usb-Headset-00.mono-fallback\"\ncamera = \"v4l2 cam\"").expect("TOML");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        read.device(DeviceKind::Microphone),
        Some("alsa_input.usb-Headset-00.mono-fallback")
    );
    assert_eq!(read.device(DeviceKind::Speaker), None);
    assert_eq!(read.device(DeviceKind::Camera), Some("v4l2 cam"));
    assert_eq!(parse(&render(&read)).expect("TOML").0, read, "read back as written");

    for bad in ["speaker = 3", "speaker = \"\"", "speaker = \"a\\nb\""] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read.speaker, None, "{bad}: the system's kept");
        assert_eq!(
            problems,
            [Problem::BadValue {
                key: "speaker",
                why: "not a device's name"
            }],
            "{bad}"
        );
    }
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

#[test]
fn each_account_keeps_its_own_chat_and_status_and_the_first_one_the_top_level_ones() {
    const FIRST: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const OTHER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
    // a file of one account: its values are the first account's
    let (mut settings, problems) = parse("selected_chat = \"19:first@thread.v2\"\nmanual_status = \"busy\"\n").unwrap();
    assert_eq!(problems, []);
    settings.make_first(FIRST);
    assert_eq!(settings.selected_chat_of(FIRST), Some("19:first@thread.v2"));
    assert_eq!(settings.manual_status_of(FIRST), ManualStatus::Busy);
    assert_eq!(settings.selected_chat_of(OTHER), None);
    assert_eq!(settings.manual_status_of(OTHER), ManualStatus::default());
    settings.set_selected_chat(OTHER, Some("19:other@thread.v2".to_owned()));
    settings.set_manual_status(OTHER, ManualStatus::DoNotDisturb);
    assert_eq!(settings.selected_chat, Some("19:first@thread.v2".to_owned()));

    // written and read back: the other account's under its MRI
    let text = render(&settings);
    let (mut read, problems) = parse(&text).unwrap();
    assert_eq!(problems, []);
    assert_eq!(read, settings);
    read.make_first(FIRST);
    assert_eq!(read, settings);
    assert_eq!(read.selected_chat_of(OTHER), Some("19:other@thread.v2"));
    assert_eq!(read.manual_status_of(OTHER), ManualStatus::DoNotDisturb);
}

#[test]
fn another_account_made_first_trades_places_with_the_first() {
    const FIRST: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const OTHER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
    let (mut read, problems) = parse(
        "selected_chat = \"19:first@thread.v2\"\nmanual_status = \"busy\"\n\
         first_account = \"8:orgid:00000000-0000-4000-8000-00000000000a\"\n\
         [accounts.\"8:orgid:00000000-0000-4000-8000-00000000000b\"]\nselected_chat = \"19:other@thread.v2\"\n",
    )
    .unwrap();
    assert_eq!(problems, []);
    assert_eq!(read.first_account.as_deref(), Some(FIRST));
    read.make_first(OTHER);
    assert_eq!(read.first_account.as_deref(), Some(OTHER));
    assert_eq!(read.selected_chat, Some("19:other@thread.v2".to_owned()));
    assert_eq!(read.selected_chat_of(FIRST), Some("19:first@thread.v2"));
    assert_eq!(read.manual_status_of(FIRST), ManualStatus::Busy);
}

#[test]
fn an_account_s_table_that_is_bad_is_reported_and_left_out() {
    let (bad, problems) = parse("[accounts.\"8:orgid:x\"]\nmanual_status = \"sleeping\"\n").unwrap();
    assert!(bad.accounts.is_empty());
    assert_eq!(problems.len(), 1);
}

#[test]
fn an_account_removed_leaves_nothing_of_its_own_and_the_next_takes_the_first_s_place() {
    const FIRST: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const OTHER: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
    let mut settings = Settings::default();
    settings.make_first(FIRST);
    settings.set_selected_chat(FIRST, Some("19:first@thread.v2".to_owned()));
    settings.set_manual_status(FIRST, ManualStatus::Busy);
    settings.set_selected_chat(OTHER, Some("19:other@thread.v2".to_owned()));
    settings.set_manual_status(OTHER, ManualStatus::DoNotDisturb);
    // the other one removed: its table goes
    let mut removed = settings.clone();
    removed.forget_account(OTHER, None);
    assert!(removed.accounts.is_empty());
    assert_eq!(removed.selected_chat_of(FIRST), Some("19:first@thread.v2"));
    // the first one removed: the other takes the top level, and nothing of the first stays
    settings.forget_account(FIRST, Some(OTHER));
    assert_eq!(settings.first_account.as_deref(), Some(OTHER));
    assert_eq!(settings.selected_chat.as_deref(), Some("19:other@thread.v2"));
    assert_eq!(settings.manual_status, ManualStatus::DoNotDisturb);
    assert!(settings.accounts.is_empty());
    // the last one removed: the top level back to its defaults
    settings.forget_account(OTHER, None);
    assert_eq!((settings.selected_chat, settings.first_account), (None, None));
    assert_eq!(settings.manual_status, ManualStatus::default());
}

#[test]
fn the_theme_follows_the_system_by_default_and_every_choice_of_how_the_app_looks_goes_through_the_file() {
    let defaults = Settings::default().appearance;
    assert!(defaults.follow_system);
    assert_eq!(
        (defaults.light_theme, defaults.dark_theme),
        (theme::LIGHT_KEY, theme::DARK_KEY)
    );
    assert_eq!(defaults.font_size, theme::FONT_SIZE);
    assert_eq!(defaults.font_family, None, "the system's UI font");
    assert!(
        !render(&Settings::default()).contains("font_family"),
        "nothing written for the system's"
    );

    for spec in &THEMES {
        for follow_system in [false, true] {
            let settings = Settings {
                appearance: Appearance {
                    follow_system,
                    theme: spec.key,
                    light_theme: spec.key,
                    dark_theme: spec.key,
                    font_size: *theme::FONT_SIZES.end(),
                    font_family: Some("DejaVu Sans".to_owned()),
                    animations: !follow_system,
                },
                ..Settings::default()
            };
            let (read, problems) = parse(&render(&settings)).expect("TOML");
            assert_eq!(read, settings);
            assert!(problems.is_empty(), "{problems:?}");
        }
    }
}

#[test]
fn the_time_format_follows_the_system_by_default_and_each_choice_goes_through_the_file() {
    assert_eq!(Settings::default().time_format, TimeFormat::System);
    for format in TimeFormat::ALL {
        let settings = Settings {
            time_format: format,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains(&format!("time_format = \"{}\"", format.key())), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    for bad in ["time_format = \"am-pm\"", "time_format = 12", "time_format = \"12h\""] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read, Settings::default(), "{bad}");
        assert!(
            matches!(&problems[..], [Problem::BadValue { key: "time_format", .. }]),
            "{bad}: {problems:?}"
        );
    }
}

/// Every local AI choice a setting can hold: each tool, with its recommended model, its own default, or a model named.
fn ai_choices() -> Vec<AiChoice> {
    let models = [
        Model::Recommended,
        Model::ToolDefault,
        Model::Named("claude-opus-5-5".to_owned()),
        Model::Named("sonnet[1m]".to_owned()),
    ];
    TranslatorChoice::ALL
        .into_iter()
        .flat_map(|tool| models.clone().map(|model| AiChoice { tool, model }))
        .collect()
}

#[test]
fn the_message_corrector_goes_through_the_file() {
    let defaults = Settings::default();
    assert_eq!(defaults.message_corrector.tool, TranslatorChoice::Automatic);
    assert_eq!(defaults.message_corrector.model, Model::Recommended);
    for choice in ai_choices() {
        let settings = Settings {
            message_corrector: choice,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains("[message_corrector]"), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    let (read, problems) = parse("message_corrector = 3").expect("TOML");
    assert_eq!(read.message_corrector, AiChoice::default());
    assert!(
        problems
            .iter()
            .any(|problem| problem.to_string().contains("message_corrector")),
        "{problems:?}"
    );
}

#[test]
fn a_drafts_translation_language_and_tool_go_through_the_file() {
    let defaults = Settings::default();
    assert_eq!(defaults.message_translate_to, "English");
    assert_eq!(defaults.message_translator, AiChoice::default());
    for (choice, language) in ai_choices().into_iter().zip(
        ["German", "Klingon", "Portuguese (Brazil)", "Scots Gaelic"]
            .into_iter()
            .cycle(),
    ) {
        let settings = Settings {
            message_translator: choice,
            message_translate_to: language.to_owned(),
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains("[message_translator]"), "{text}");
        assert!(
            text.contains(&format!("message_translate_to = \"{language}\"")),
            "{text}"
        );
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    let (read, problems) = parse("message_translate_to = \"german\"").expect("TOML");
    assert_eq!(
        read.message_translate_to, "German",
        "a listed language is kept by its own name"
    );
    assert!(problems.is_empty(), "{problems:?}");
    for bad in [
        "message_translate_to = 3",
        "message_translate_to = \"<x0/>\"",
        "message_translator = 3",
    ] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read.message_translate_to, "English", "{bad}");
        assert_eq!(read.message_translator, AiChoice::default(), "{bad}");
        assert!(
            problems
                .iter()
                .any(|problem| problem.to_string().contains("message_translat")),
            "{bad}: {problems:?}"
        );
    }
}

#[gpui::test]
fn a_drafts_translation_language_set_is_there_after_a_restart(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (file, _) = file_in(&dir);
    let model = cx.update(|cx| cx.new(|_| SettingsModel::load(file)));
    cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.message_translate_to = "Klingon".to_owned(), cx);
        });
    });
    let (file, _) = file_in(&dir);
    let restarted = SettingsModel::load(file);
    assert_eq!(restarted.settings().message_translate_to, "Klingon");
}

#[test]
fn caption_translation_is_off_by_default_and_its_language_and_tool_go_through_the_file() {
    let defaults = Settings::default();
    assert_eq!(defaults.caption_translate_to, None);
    assert_eq!(defaults.caption_translator.tool, TranslatorChoice::Automatic);
    assert_eq!(defaults.caption_translator.model, Model::Recommended);
    assert!(!render(&defaults).contains("caption_translate_to"));
    for choice in ai_choices() {
        let settings = Settings {
            caption_translate_to: Some("pt-br"),
            caption_translator: choice,
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains("caption_translate_to = \"pt-br\""), "{text}");
        assert!(text.contains("[caption_translator]"), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    for (bad, key) in [
        ("caption_translate_to = \"klingon\"", "caption_translate_to"),
        ("caption_translate_to = 3", "caption_translate_to"),
        ("caption_translator = \"luna\"", "caption_translator"),
        ("caption_translator = 3", "caption_translator"),
        ("caption_translator = { tool = \"gemini\" }", "caption_translator"),
        ("caption_translator = { tool = 1 }", "caption_translator"),
        (
            "caption_translator = { tool = \"claude\", model = 3 }",
            "caption_translator",
        ),
        (
            "caption_translator = { tool = \"claude\", model = \"--dangerously\" }",
            "caption_translator",
        ),
        (
            "caption_translator = { tool = \"claude\", model = \"two words\" }",
            "caption_translator",
        ),
        (
            "caption_translator = { tool = \"claude\", model = \"\" }",
            "caption_translator",
        ),
    ] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read, Settings::default(), "{bad}");
        assert!(
            matches!(&problems[..], [Problem::BadValue { key: found, .. }] if *found == key),
            "{bad}: {problems:?}"
        );
    }
}

#[test]
fn the_recap_summarizer_is_automatic_by_default_and_goes_through_the_file() {
    assert_eq!(Settings::default().recap_summarizer, AiChoice::default());
    for choice in ai_choices() {
        let settings = Settings {
            recap_summarizer: choice.clone(),
            ..Settings::default()
        };
        let text = render(&settings);
        assert!(text.contains("[recap_summarizer]"), "{text}");
        assert!(text.contains(&format!("tool = \"{}\"", choice.tool.key())), "{text}");
        assert!(text.contains(&format!("model = \"{}\"", choice.model.key())), "{text}");
        let (read, problems) = parse(&text).expect("TOML");
        assert_eq!(read, settings);
        assert!(problems.is_empty(), "{problems:?}");
    }
    let (read, problems) = parse("recap_summarizer = \"opus\"").expect("TOML");
    assert_eq!(read, Settings::default());
    assert!(
        matches!(
            &problems[..],
            [Problem::BadValue {
                key: "recap_summarizer",
                ..
            }]
        ),
        "{problems:?}"
    );
}

#[test]
fn an_earlier_builds_tool_alone_keeps_its_tool_with_the_recommended_model_and_is_written_anew() {
    for tool in TranslatorChoice::ALL {
        let earlier = format!(
            "caption_translator = \"{key}\"\nrecap_summarizer = \"{key}\"\n",
            key = tool.key()
        );
        let (read, problems) = parse(&earlier).expect("TOML");
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(read.caption_translator, AiChoice::of(tool));
        assert_eq!(read.recap_summarizer, AiChoice::of(tool));
        let text = render(&read);
        assert!(
            text.contains(&format!(
                "[caption_translator]\nmodel = \"recommended\"\ntool = \"{}\"",
                tool.key()
            )),
            "{text}"
        );
        assert_eq!(parse(&text).expect("TOML").0, read);
    }
    // a table missing a part keeps that part's default
    let (read, problems) =
        parse("[caption_translator]\ntool = \"codex\"\n[recap_summarizer]\nmodel = \"opus\"\n").expect("TOML");
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        read.caption_translator,
        AiChoice::of(TranslatorChoice::Only(Backend::Codex))
    );
    assert_eq!(
        read.recap_summarizer,
        AiChoice {
            tool: TranslatorChoice::Automatic,
            model: Model::Named("opus".to_owned()),
        }
    );
}

#[test]
fn a_model_s_name_is_any_the_tool_takes_on_its_command_line() {
    assert_eq!(Model::from_key("recommended"), Some(Model::Recommended));
    assert_eq!(Model::from_key(" default "), Some(Model::ToolDefault));
    for name in [
        "opus",
        "claude-opus-5-5",
        "sonnet[1m]",
        "gpt-6.1-sol",
        "us.anthropic.claude-x:0",
    ] {
        assert_eq!(Model::from_key(name), Some(Model::Named(name.to_owned())), "{name}");
        assert_eq!(Model::Named(name.to_owned()).key(), name);
    }
    let long = "m".repeat(101);
    for bad in ["", "  ", "-p", "two words", "tab\tbed", long.as_str()] {
        assert_eq!(Model::from_key(bad), None, "{bad:?}");
    }
    assert_eq!(Model::from_key(&"m".repeat(100)), Some(Model::Named("m".repeat(100))));
}

#[test]
fn a_theme_this_app_doesn_t_know_or_a_font_size_out_of_range_keeps_its_default() {
    for (bad, key) in [
        ("theme = \"purple\"", "theme"),
        ("light_theme = \"Teams Light\"", "light_theme"),
        ("dark_theme = 3", "dark_theme"),
        ("follow_system_theme = \"yes\"", "follow_system_theme"),
        ("font_size = 9", "font_size"),
        ("font_size = 25", "font_size"),
        ("font_size = 14.5", "font_size"),
        ("font_family = \"\"", "font_family"),
        ("font_family = \"a\\nb\"", "font_family"),
        ("animations = \"off\"", "animations"),
    ] {
        let (read, problems) = parse(bad).expect("TOML");
        assert_eq!(read, Settings::default(), "{bad}");
        assert!(
            matches!(&problems[..], [Problem::BadValue { key: found, .. }] if *found == key),
            "{bad}: {problems:?}"
        );
    }
}

#[test]
fn the_font_size_steps_within_its_range() {
    let mut appearance = Appearance::default();
    assert_eq!(appearance.stepped_font_size(1), theme::FONT_SIZE + 1);
    appearance.font_size = *theme::FONT_SIZES.end();
    assert_eq!(appearance.stepped_font_size(1), *theme::FONT_SIZES.end());
    appearance.font_size = *theme::FONT_SIZES.start();
    assert_eq!(appearance.stepped_font_size(-1), *theme::FONT_SIZES.start());
}

#[test]
fn a_chat_s_language_is_kept_per_account_and_chat_and_a_bad_one_left_out() {
    let (read, problems) = parse(
        "chat_translator = \"claude\"\n\
         [[chat_languages]]\naccount = \"8:orgid:a\"\nchat = \"19:one@thread.v2\"\nlanguage = \"de\"\n\
         [[chat_languages]]\naccount = \"8:orgid:a\"\nchat = \"19:two@thread.v2\"\nlanguage = \"klingon\"\n\
         [[chat_languages]]\naccount = \"8:orgid:b\"\nchat = \"19:one@thread.v2\"\nlanguage = \"pt-br\"\n",
    )
    .unwrap();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(
        read.chat_translator,
        AiChoice::of(TranslatorChoice::Only(Backend::Claude))
    );
    assert_eq!(read.chat_language_of("8:orgid:a", "19:one@thread.v2"), Some("de"));
    assert_eq!(read.chat_language_of("8:orgid:a", "19:two@thread.v2"), None);
    assert_eq!(read.chat_language_of("8:orgid:b", "19:one@thread.v2"), Some("pt-br"));
    // kept across a restart: written and read again
    let again = parse(&render(&read)).unwrap().0;
    assert_eq!(again.chat_languages, read.chat_languages);
    assert_eq!(again.chat_translator, read.chat_translator);
}

#[test]
fn a_chat_s_language_is_changed_turned_off_and_forgotten_with_its_account() {
    let (mut settings, _) = parse(
        "[[chat_languages]]\naccount = \"8:orgid:a\"\nchat = \"19:one@thread.v2\"\nlanguage = \"de\"\n\
         [[chat_languages]]\naccount = \"8:orgid:b\"\nchat = \"19:one@thread.v2\"\nlanguage = \"pt-br\"\n",
    )
    .unwrap();
    settings.set_chat_language("8:orgid:a", "19:one@thread.v2", Some("fr"));
    assert_eq!(settings.chat_language_of("8:orgid:a", "19:one@thread.v2"), Some("fr"));
    assert_eq!(settings.chat_languages.len(), 2);
    settings.set_chat_language("8:orgid:a", "19:one@thread.v2", None);
    assert_eq!(settings.chat_language_of("8:orgid:a", "19:one@thread.v2"), None);
    settings.forget_account("8:orgid:b", None);
    assert!(settings.chat_languages.is_empty());
    // off by default; a bad tool or list is reported
    assert!(Settings::default().chat_languages.is_empty());
    assert_eq!(Settings::default().chat_translator, AiChoice::default());
    assert_eq!(parse("chat_translator = \"luna\"").unwrap().1.len(), 1);
    assert_eq!(parse("chat_languages = 3").unwrap().1.len(), 1);
}

#[test]
fn people_s_calendars_are_kept_by_account_and_a_bad_one_left_out() {
    let (read, problems) = parse(
        "[[people_calendars]]\naccount = \"8:orgid:a\"\naddress = \"avery@example.com\"\nname = \"Avery\"\n\
         [[people_calendars]]\naccount = \"8:orgid:a\"\naddress = \"no-address\"\n\
         [[people_calendars]]\naccount = \"8:orgid:b\"\naddress = \"riley@example.com\"\n",
    )
    .unwrap();
    assert_eq!(problems.len(), 1);
    assert_eq!(read.people_calendars.len(), 2);
    assert_eq!(read.people_calendars[0].name, "Avery");
    // a calendar without a name is named by its address
    assert_eq!(read.people_calendars[1].name, "riley@example.com");
    assert_eq!(parse(&render(&read)).unwrap().0.people_calendars, read.people_calendars);
    assert_eq!(parse("people_calendars = 3").unwrap().1.len(), 1);
}

#[cfg(unix)]
#[test]
fn reading_preserves_unix_modes_that_already_exclude_other_users() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let (file, path) = file_in(&dir);
    file.save(&changed()).unwrap();
    for mode in [0o400, 0o600, 0o700] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(file.load().settings, changed());
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, mode);
    }
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
