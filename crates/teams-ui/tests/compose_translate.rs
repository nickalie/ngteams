//! A draft translated in its box, as the compose box of a chat or a channel does it, on GPUI's test context over the
//! fake engine (`common`) and a fake translating engine (synthetic messages only; no AI tool ever runs): the
//! translation in place as one edit Ctrl+Z takes back, the draft's mentions and line breaks kept, the language one
//! setting every box shares, progress and cancel, and failures that keep the draft.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{LAB_B, Rig};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::Action;
use teams_ui::ViewTrace;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::{Compose, DraftTranslationStatus, Editor, Writes};
use teams_ui::draft_translate::menu::Row;
use teams_ui::draft_translate::{self, Engine, Limits, Session};
use teams_ui::local_ai::{AiChoice, Backend, Failure, Launch, ToolChoice, Tools};
use teams_ui::settings::Settings;

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const OTHER: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";

/// A fake translating engine: "hello" becomes "hallo" and "morning" "Morgen", every marker kept; it records what it
/// was asked and with which tool, holds its answer while held, fails while `failing`, and notes each session closed.
#[derive(Default)]
struct Fake {
    asked: Mutex<Vec<String>>,
    launched: Mutex<Vec<Backend>>,
    held: AtomicBool,
    failing: Mutex<Option<Failure>>,
    closed: AtomicUsize,
}

struct FakeSession(Arc<Fake>);

struct FakeEngine(Arc<Fake>);

impl Engine for FakeEngine {
    fn start(&self, launch: &Launch, _: &AtomicBool) -> Result<Box<dyn Session>, Failure> {
        self.0.launched.lock().unwrap().push(launch.backend);
        Ok(Box::new(FakeSession(Arc::clone(&self.0))))
    }
}

impl Session for FakeSession {
    fn ask(&mut self, message: &str, wait: Duration, stop: &AtomicBool) -> Result<String, Failure> {
        self.0.asked.lock().unwrap().push(message.to_owned());
        let deadline = std::time::Instant::now() + wait;
        while self.0.held.load(Ordering::Relaxed) {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped(Backend::Claude, None));
            }
            if std::time::Instant::now() > deadline {
                return Err(Failure::TimedOut(Backend::Claude));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(failure) = self.0.failing.lock().unwrap().clone() {
            return Err(failure);
        }
        let body = message.split("<message>\n").nth(1).unwrap_or_default();
        let body = body.strip_suffix("\n</message>").unwrap_or_default();
        Ok(format!(
            "<translated>{}</translated>",
            body.replace("hello", "hallo").replace("morning", "Morgen")
        ))
    }

    fn close(self: Box<Self>) {
        self.0.closed.fetch_add(1, Ordering::Relaxed);
    }
}

impl Fake {
    fn install(rig: &Rig) -> Arc<Self> {
        let fake = Arc::new(Self::default());
        let engine: Arc<dyn Engine> = Arc::new(FakeEngine(Arc::clone(&fake)));
        rig.cx.update(|cx| {
            // a tool "installed" at a path nothing runs: the fake engine answers in its place
            cx.set_global(Tools::with(vec![(Backend::Claude, "/made-up/claude".into())]));
            draft_translate::set_engine(cx, engine);
        });
        fake
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn until(cx: &mut VisualTestContext, done: impl Fn(&mut VisualTestContext) -> bool) {
    for _ in 0..1500 {
        draw(cx);
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("never happened");
}

fn open_chat(
    rig: &Rig,
    conversation: &str,
    settings: &Entity<teams_ui::settings::SettingsModel>,
) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let mut parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    parts.settings = settings.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let conversation = conversation.to_owned();
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, conversation, window, cx));
    cx.simulate_resize(size(px(900.0), px(600.0)));
    draw(cx);
    (view, cx)
}

fn settings(rig: &Rig) -> Entity<teams_ui::settings::SettingsModel> {
    parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off())).settings
}

fn compose_of(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<Compose> {
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    compose
}

/// The a–b chat in a window of its own, its box focused, the app's settings `model`.
fn chat_in(
    rig: &Rig,
    model: &Entity<teams_ui::settings::SettingsModel>,
) -> (Entity<Compose>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    let (view, cx) = open_chat(rig, AB, model);
    let compose = compose_of(&view, cx);
    (compose, cx)
}

fn chat(rig: &Rig) -> (Entity<Compose>, &'static mut VisualTestContext) {
    chat_in(rig, &settings(rig))
}

fn type_in(compose: &Entity<Compose>, cx: &mut VisualTestContext, text: &str) {
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, |editor, cx| editor.insert(text, cx));
        });
    });
}

fn translate(compose: &Entity<Compose>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.translate_draft(window, cx)));
}

fn status(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> DraftTranslationStatus {
    compose.read_with(cx, |compose, _| compose.draft_translation_status())
}

fn value(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> String {
    cx.read(|cx| compose.read(cx).value(cx).to_string())
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} isn't shown"))
        .center();
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
}

fn sent(rig: &Rig) -> usize {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter(|action| matches!(action, Action::Send { .. }))
        .count()
}

#[test]
fn translate_replaces_the_draft_as_one_edit_and_undo_brings_it_back() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "hello there");
    click(cx, "compose-translate");
    until(cx, |cx| value(&compose, cx) == "hallo there");
    assert_eq!(status(&compose, cx), DraftTranslationStatus::Idle);
    assert_eq!(sent(&rig), 0, "translating sends nothing");
    assert_eq!(
        fake.asked(),
        vec!["<language>English</language>\n<message>\n<x0>hello there</x0>\n</message>".to_owned()],
        "only the language and the draft go to the tool"
    );
    cx.update(|_, cx| compose.update(cx, |compose, cx| compose.editor().update(cx, Editor::undo)));
    assert_eq!(value(&compose, cx), "hello there");
}

#[test]
fn a_mention_a_link_line_breaks_and_paragraphs_stay_as_they_were() {
    use teams_ui::editor::doc::{Attrs, Block, Doc, Kind, Run};
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    let (compose, cx) = chat(&rig);
    let mention = Run::new(
        "Lab Bee",
        Attrs {
            mention: Some(LAB_B.to_owned()),
            ..Attrs::default()
        },
    );
    let link = Run::new(
        "https://example.test/plan",
        Attrs {
            link: Some("https://example.test/plan".to_owned()),
            ..Attrs::default()
        },
    );
    let draft = Doc::new(vec![
        Block::new(
            Kind::Paragraph,
            0,
            vec![
                mention.clone(),
                Run::new(" hello 👋\ngood morning ", Attrs::default()),
                link.clone(),
            ],
        ),
        Block::paragraph("morning"),
    ]);
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, |editor, cx| editor.set_doc(draft, cx));
        });
    });
    translate(&compose, cx);
    until(cx, |cx| value(&compose, cx).contains("Morgen"));
    let question = fake.asked().join("\n");
    for private in ["Lab Bee", LAB_B, "example.test"] {
        assert!(!question.contains(private), "{private}: {question}");
    }
    let doc = cx.read(|cx| compose.read(cx).editor().read(cx).doc().clone());
    let expected = Doc::new(vec![
        Block::new(
            Kind::Paragraph,
            0,
            vec![mention, Run::new(" hallo 👋\ngood Morgen ", Attrs::default()), link],
        ),
        Block::paragraph("Morgen"),
    ]);
    assert_eq!(doc, expected);
}

#[test]
fn the_language_is_one_setting_every_box_shares() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    let model = settings(&rig);
    let (compose, cx) = chat_in(&rig, &model);
    let (other_view, other_cx) = open_chat(&rig, OTHER, &model);
    let other = compose_of(&other_view, other_cx);

    // the language button shows the language and opens the menu; any language can be typed
    assert!(cx.debug_bounds("compose-language").is_some());
    click(cx, "compose-language");
    let menu = compose.read_with(cx, |compose, _| compose.language_menu().unwrap());
    assert!(menu.read_with(cx, |menu, _| menu.is_open()));
    cx.update(|window, cx| menu.update(cx, |menu, cx| menu.search("Klingon", window, cx)));
    click(cx, "compose-language-custom");
    assert!(!menu.read_with(cx, |menu, _| menu.is_open()));
    assert_eq!(
        model.read_with(cx, |model, _| model.settings().message_translate_to.clone()),
        "Klingon"
    );

    // the other chat's box translates into it too
    type_in(&other, other_cx, "hello");
    translate(&other, other_cx);
    until(other_cx, |cx| value(&other, cx) == "hallo");
    assert!(
        fake.asked()[0].starts_with("<language>Klingon</language>"),
        "{:?}",
        fake.asked()
    );

    // a listed one by the keyboard; the menu opens with the language chosen first
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.open_language_menu(window, cx)));
    let rows = cx.read(|cx| menu.read(cx).rows(cx));
    assert_eq!(rows[0], Row::Typed("Klingon".to_owned()));
    assert_eq!(rows.len(), draft_translate::COMMON_LANGUAGES.len() + 1);
    cx.update(|window, cx| menu.update(cx, |menu, cx| menu.search("germ", window, cx)));
    cx.update(|window, cx| menu.update(cx, |menu, cx| menu.enter(window, cx)));
    assert_eq!(
        model.read_with(cx, |model, _| model.settings().message_translate_to.clone()),
        "German"
    );
}

#[test]
fn a_translation_shows_its_progress_and_cancel_keeps_the_draft_and_ends_the_tool() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    fake.held.store(true, Ordering::Relaxed);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "hello");
    translate(&compose, cx);
    draw(cx);
    assert_eq!(status(&compose, cx), DraftTranslationStatus::Running);
    assert!(cx.debug_bounds("compose-draft-translating").is_some());
    until(cx, |_| !fake.asked().is_empty());
    click(cx, "compose-draft-translation-cancel");
    assert_eq!(status(&compose, cx), DraftTranslationStatus::Idle);
    assert!(cx.debug_bounds("compose-draft-translating").is_none());
    until(cx, |_| fake.closed.load(Ordering::Relaxed) == 1);
    fake.held.store(false, Ordering::Relaxed);
    draw(cx);
    assert_eq!(value(&compose, cx), "hello", "a cancelled translation never lands");

    // the button again and Escape cancel too
    fake.held.store(true, Ordering::Relaxed);
    translate(&compose, cx);
    draw(cx);
    translate(&compose, cx);
    assert_eq!(status(&compose, cx), DraftTranslationStatus::Idle);
    translate(&compose, cx);
    draw(cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(status(&compose, cx), DraftTranslationStatus::Idle);
    until(cx, |_| fake.closed.load(Ordering::Relaxed) == 3);
    assert_eq!(value(&compose, cx), "hello");
}

#[test]
fn a_failure_says_why_keeps_the_draft_and_can_be_tried_again() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    *fake.failing.lock().unwrap() = Some(Failure::SignedOut(Backend::Claude));
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "hello");
    translate(&compose, cx);
    until(cx, |cx| {
        matches!(status(&compose, cx), DraftTranslationStatus::Failed(_))
    });
    let DraftTranslationStatus::Failed(why) = status(&compose, cx) else {
        unreachable!()
    };
    assert!(why.contains("isn't signed in"), "{why}");
    assert_eq!(value(&compose, cx), "hello");
    assert!(cx.debug_bounds("compose-draft-translation-failed").is_some());
    assert!(cx.debug_bounds("compose-draft-translation-settings").is_none());
    *fake.failing.lock().unwrap() = None;
    click(cx, "compose-draft-translation-retry");
    until(cx, |cx| value(&compose, cx) == "hallo");
}

#[test]
fn a_tool_that_takes_too_long_is_given_up_on_and_the_draft_kept() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    fake.held.store(true, Ordering::Relaxed);
    let (compose, cx) = chat(&rig);
    compose.update(cx, |compose, _| {
        compose.set_draft_translation_limits(Limits {
            start: Duration::from_millis(10),
            answer: Duration::from_millis(50),
        });
    });
    type_in(&compose, cx, "hello");
    translate(&compose, cx);
    until(cx, |cx| {
        matches!(status(&compose, cx), DraftTranslationStatus::Failed(_))
    });
    assert_eq!(value(&compose, cx), "hello");
}

#[test]
fn with_no_tool_installed_it_says_so_and_offers_the_setting() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    rig.cx.update(|cx| cx.set_global(Tools::default()));
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "hello");
    translate(&compose, cx);
    draw(cx);
    let DraftTranslationStatus::Failed(why) = status(&compose, cx) else {
        panic!("{:?}", status(&compose, cx))
    };
    assert!(why.contains("Neither Claude Code"), "{why}");
    assert!(cx.debug_bounds("compose-draft-translation-settings").is_some());
    assert!(fake.asked().is_empty());
    assert_eq!(value(&compose, cx), "hello");
}

#[test]
fn the_tool_chosen_for_translating_messages_is_the_one_started() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    rig.cx.update(|cx| {
        cx.set_global(Tools::with(vec![
            (Backend::Claude, "/made-up/claude".into()),
            (Backend::Codex, "/made-up/codex".into()),
        ]));
    });
    let model = settings(&rig);
    rig.cx.update(|cx| {
        model.update(cx, |model, cx| {
            model.change(
                |settings| settings.message_translator = AiChoice::of(ToolChoice::Only(Backend::Codex)),
                cx,
            );
        });
    });
    let (compose, cx) = chat_in(&rig, &model);
    type_in(&compose, cx, "hello");
    translate(&compose, cx);
    until(cx, |cx| value(&compose, cx) == "hallo");
    assert_eq!(*fake.launched.lock().unwrap(), vec![Backend::Codex]);
}

#[test]
fn editing_the_draft_while_it_runs_drops_the_translation() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    fake.held.store(true, Ordering::Relaxed);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "hello");
    translate(&compose, cx);
    draw(cx);
    type_in(&compose, cx, " again");
    draw(cx);
    assert_eq!(status(&compose, cx), DraftTranslationStatus::DraftChanged);
    fake.held.store(false, Ordering::Relaxed);
    draw(cx);
    assert_eq!(value(&compose, cx), "hello again");
    click(cx, "compose-draft-translation-retry");
    until(cx, |cx| value(&compose, cx) == "hallo again");
}

#[test]
fn an_empty_box_has_nothing_to_translate_and_one_without_words_says_so() {
    let rig = Rig::start();
    let fake = Fake::install(&rig);
    let (compose, cx) = chat(&rig);
    translate(&compose, cx);
    assert_eq!(status(&compose, cx), DraftTranslationStatus::Idle);
    type_in(&compose, cx, "👍 🎉");
    translate(&compose, cx);
    assert_eq!(status(&compose, cx), DraftTranslationStatus::NothingToTranslate);
    assert!(fake.asked().is_empty());
}

#[test]
fn a_channels_box_translates_too() {
    let rig = Rig::start();
    let _fake = Fake::install(&rig);
    let (compose, cx) = chat(&rig);
    compose.update(cx, |compose, cx| {
        compose.in_channel(Writes::Channel, "Message in General", cx);
    });
    draw(cx);
    assert!(cx.debug_bounds("compose-translate").is_some());
    assert!(cx.debug_bounds("compose-language").is_some());
    type_in(&compose, cx, "hello");
    click(cx, "compose-translate");
    until(cx, |cx| value(&compose, cx) == "hallo");
}
