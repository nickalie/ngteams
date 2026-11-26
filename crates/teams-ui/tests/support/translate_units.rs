//! Caption translation's languages, tools and model, the model driven against a fake `claude` on GPUI's test context
//! (synthetic captions only).

use crate::fake_tool;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use gpui::AppContext as _;

use super::*;
use crate::local_ai::{ToolChoice as TranslatorChoice, Tools as Translators, executable};
use crate::settings::Settings;

#[test]
fn a_track_is_in_the_target_language_by_its_language_script_or_country() {
    assert!(same_language("en", "en-us"));
    assert!(same_language("de", "DE-de"));
    assert!(!same_language("de", "en-gb"));
    assert!(same_language("zh-hans", "zh-cn"));
    assert!(!same_language("zh-hans", "zh-tw"));
    assert!(same_language("zh-hant", "zh-hk"));
    assert!(same_language("pt-br", "pt-br"));
    assert!(!same_language("pt-pt", "pt-br"));
    assert!(same_language("nb", "nn-no"));
    assert_eq!(language("lt"), Some(("lt", "Lithuanian")));
    assert_eq!(language("xx"), None);
    assert!(instructions("German").contains("into German"));
    assert!(instructions("German").contains(session::GLOSSARY_REQUEST));
}

#[test]
fn the_tools_are_chosen_claude_first() {
    for choice in [
        TranslatorChoice::Automatic,
        TranslatorChoice::cli(Backend::Claude),
        TranslatorChoice::cli(Backend::Codex),
    ] {
        assert_eq!(TranslatorChoice::from_key(choice.key()), Some(choice));
    }
    assert_eq!(
        TranslatorChoice::from_key("luna"),
        Some(TranslatorChoice::Only("luna".to_owned()))
    );
    let pick = |tools: &Translators, choice: &TranslatorChoice| {
        crate::local_ai::providers::pick_from(
            &crate::local_ai::providers::effective(&[], tools),
            tools,
            &crate::local_ai::AiKeys::default(),
            choice,
        )
    };
    let both = Translators::with(vec![
        (Backend::Codex, "/synthetic/codex".into()),
        (Backend::Claude, "/synthetic/claude".into()),
    ]);
    assert_eq!(both.installed().collect::<Vec<_>>(), Backend::CLI);
    assert_eq!(
        pick(&both, &TranslatorChoice::Automatic).unwrap().backend,
        Backend::Claude
    );
    assert!(matches!(
        pick(&both, &TranslatorChoice::cli(Backend::Codex)).unwrap().target,
        crate::local_ai::Target::Program(path) if path == std::path::Path::new("/synthetic/codex")
    ));
    let codex = Translators::with(vec![(Backend::Codex, "/synthetic/codex".into())]);
    assert_eq!(
        pick(&codex, &TranslatorChoice::Automatic).unwrap().backend,
        Backend::Codex
    );
    assert_eq!(
        pick(&codex, &TranslatorChoice::cli(Backend::Claude)).err(),
        Some(Failure::NotInstalled(Some(Backend::Claude)))
    );
    assert_eq!(
        pick(&Translators::default(), &TranslatorChoice::Automatic).err(),
        Some(Failure::NotInstalled(None))
    );
}

#[test]
fn every_kind_has_its_names() {
    for backend in Backend::CLI {
        assert_eq!(backend.program(), backend.key());
        assert!(!backend.label(Work::Captions).is_empty() && backend.recommended(Work::Captions).is_some());
    }
    for backend in Backend::ALL {
        assert_eq!(Backend::from_key(backend.key()), Some(backend));
        assert!(!backend.name().is_empty() && !backend.tool().is_empty() && !backend.provider().is_empty());
    }
}

#[test]
fn only_an_executable_file_is_a_tool() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("claude");
    fs::write(&file, "").unwrap();
    assert!(!executable(&file));
    let program = fake_tool::write(dir.path(), "claude", "exit 0");
    assert!(executable(&program));
    assert!(!executable(dir.path()));
    // the PATH as the tests run with it: whatever it finds is an executable of the tool's name
    for (backend, program) in &Translators::detect().found {
        assert!(program.file_stem().unwrap() == backend.program() && executable(program));
    }
}

/// A fake Claude Code answering each caption with its text after `DE `.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
while IFS= read -r line; do
  text=$(printf '%s' "$line" | sed -n 's/.*<caption[^>]*>\(.*\)<\/caption>.*/\1/p')
  printf '{"type":"result","is_error":false,"result":"DE %s"}\n' "$text"
done
"#;

fn caption(stamp: u64, text: &str, final_result: bool) -> Caption {
    Caption {
        id: format!("synthetic/{stamp}"),
        speaker: Some("8:orgid:synthetic-a".into()),
        display_name: Some("Lab Speaker".into()),
        text: text.into(),
        final_result,
        spoken_language: "en-us".into(),
        track_language: "en-us".into(),
        audio_timestamp: stamp,
        duration: 10,
    }
}

/// Runs `app` until `done` holds, the fake tool answering on its own thread meanwhile.
fn until(app: &mut gpui::TestAppContext, done: impl Fn(&mut gpui::TestAppContext) -> bool) {
    for _ in 0..1000 {
        // the debounce's timer runs on the test's clock
        app.executor().advance_clock(Duration::from_millis(50));
        app.run_until_parked();
        if done(app) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("never happened");
}

fn translation(
    app: &mut gpui::TestAppContext,
    found: Vec<(Backend, PathBuf)>,
) -> (Entity<SettingsModel>, Entity<CaptionTranslation>) {
    app.update(|cx| cx.set_global(Translators::with(found)));
    let settings = app.new(|_| SettingsModel::in_memory(Settings::default()));
    let model = settings.clone();
    let translation = app.new(|cx| CaptionTranslation::new(model, cx));
    (settings, translation)
}

fn translate_to(app: &mut gpui::TestAppContext, settings: &Entity<SettingsModel>, code: Option<&'static str>) {
    settings.update(app, |settings, cx| {
        settings.change(|settings| settings.caption_translate_to = code, cx);
    });
}

#[test]
fn captions_are_translated_only_once_a_language_is_picked_and_while_they_show() {
    let dir = tempfile::tempdir().unwrap();
    let program = fake_tool::write(dir.path(), "claude", FAKE_CLAUDE);
    let mut app = gpui::TestAppContext::single();
    let (settings, translation) = translation(&mut app, vec![(Backend::Claude, program)]);
    let lines = vec![caption(1, "Synthetic hello.", true)];
    translation.update(&mut app, |translation, cx| {
        translation.follow(&lines, true, "Title: Synthetic".into(), cx);
    });
    app.run_until_parked();
    // off by default: shown as spoken, nothing started
    translation.read_with(&app, |translation, _| {
        assert_eq!(translation.status(), &Status::Off);
        assert_eq!(translation.shown(&lines[0]), Shown::Spoken("Synthetic hello."));
    });
    translate_to(&mut app, &settings, Some("de"));
    translation.read_with(&app, |translation, _| {
        assert_eq!(
            translation.status(),
            &Status::Translating {
                backend: Backend::Claude,
                language: "German",
                by: "Claude Haiku".to_owned(),
            }
        );
    });
    until(&mut app, |app| {
        translation.read_with(app, |translation, _| {
            translation.shown(&lines[0]) == Shown::Translated("DE Synthetic hello.")
        })
    });
    // an interim line waits, dimmed, then comes translated
    let more = vec![lines[0].clone(), caption(2, "Still speaking", false)];
    translation.update(&mut app, |translation, cx| {
        translation.follow(&more, true, "Title: Synthetic".into(), cx);
    });
    translation.read_with(&app, |translation, _| {
        assert_eq!(translation.shown(&more[1]), Shown::Waiting("Still speaking"));
    });
    until(&mut app, |app| {
        translation.read_with(app, |translation, _| {
            translation.shown(&more[1]) == Shown::Translated("DE Still speaking")
        })
    });
    // hidden: stopped
    translation.update(&mut app, |translation, cx| {
        translation.follow(&[], false, String::new(), cx);
    });
    translation.read_with(&app, |translation, _| assert_eq!(translation.status(), &Status::Off));
    // shown again: what was translated shows at once
    translation.update(&mut app, |translation, cx| {
        translation.follow(&lines, true, "Title: Synthetic".into(), cx);
    });
    translation.read_with(&app, |translation, _| {
        assert_eq!(translation.shown(&lines[0]), Shown::Translated("DE Synthetic hello."));
    });
    translate_to(&mut app, &settings, None);
}

#[test]
fn a_tool_missing_shows_the_captions_as_spoken_and_try_again_looks_again() {
    let mut app = gpui::TestAppContext::single();
    let (settings, translation) = translation(&mut app, Vec::new());
    translate_to(&mut app, &settings, Some("fr"));
    let lines = vec![caption(1, "Synthetic hello.", true)];
    translation.update(&mut app, |translation, cx| {
        translation.follow(&lines, true, String::new(), cx);
    });
    translation.read_with(&app, |translation, _| {
        assert_eq!(
            translation.status(),
            &Status::Failed {
                failure: Failure::NotInstalled(None),
                language: "French"
            }
        );
        assert_eq!(translation.shown(&lines[0]), Shown::Spoken("Synthetic hello."));
    });
    // the tool installed meanwhile, but it fails as it starts: still as spoken, with why
    let dir = tempfile::tempdir().unwrap();
    let program = fake_tool::write(dir.path(), "codex", "echo 'synthetic failure' >&2\nexit 1\n");
    app.update(|cx| cx.set_global(Translators::with(vec![(Backend::Codex, program)])));
    translation.update(&mut app, CaptionTranslation::retry);
    until(&mut app, |app| {
        translation.read_with(app, |translation, _| {
            translation.status()
                == &Status::Failed {
                    failure: Failure::Stopped(Backend::Codex, Some("synthetic failure".into())),
                    language: "French",
                }
        })
    });
}
