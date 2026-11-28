//! A draft corrected by the AI before it is sent, as the compose box of a chat does it, on GPUI's test context over the
//! fake engine (`common`) and a fake `claude` (synthetic messages only): what the tool reads, the proposal shown and
//! the draft untouched until the user takes it, and every way it can end without taking it.

#![cfg(test)]

#[path = "support/fake_tool.rs"]
#[expect(dead_code, reason = "the shared fake CLI helper serves several test harnesses")]
mod fake_tool;

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{LAB_B, Rig};
use gpui::{Context, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::Action;
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::{Compose, CorrectionStatus, Editor, Quoted};
use teams_ui::local_ai::{AiChoice, Backend, ToolChoice, Tools};
use teams_ui::message_correct::Limits;
use teams_ui::settings::{ComposeTool, Settings, SettingsModel};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const OTHER: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";

/// A fake Claude Code: "teh" corrected to "the" and anything with "tidy" in it left alone, each question logged, and
/// no answer while a `hold` file lies beside it.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$dir/received.log"
  printf '%s' "$$" > "$dir/pid"
  while [ -e "$dir/hold" ]; do sleep 0.02; done
  t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
  case "$t" in
    *tidy*) ans='<unchanged/>';;
    *) ans="<corrected>$(printf '%s' "$t" | sed 's/teh/the/g')</corrected>";;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

/// A fake Claude Code that isn't signed in.
const SIGNED_OUT: &str = "#!/bin/bash\necho 'Invalid API key. Please run /login' >&2\nexit 1\n";

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new(rig: &Rig) -> Self {
        let fake = Self::with(FAKE_CLAUDE);
        fake.install(rig);
        fake
    }

    fn with(body: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fake_tool::write(dir.path(), "claude", body);
        Self { dir }
    }

    fn program(&self) -> PathBuf {
        fake_tool::program(self.dir.path(), "claude")
    }

    fn install(&self, rig: &Rig) {
        let program = self.program();
        rig.cx
            .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, program)])));
    }

    fn hold(&self) {
        fs::write(self.dir.path().join("hold"), "").unwrap();
    }

    fn release(&self) {
        fs::remove_file(self.dir.path().join("hold")).unwrap();
    }

    fn questions(&self) -> String {
        read_log(self.dir.path())
    }

    fn running_pid(&self, cx: &mut VisualTestContext) -> u32 {
        until(cx, |_| self.pid().is_some_and(process_running));
        self.pid().unwrap()
    }

    fn pid(&self) -> Option<u32> {
        fs::read_to_string(self.dir.path().join("pid")).ok()?.parse().ok()
    }

    fn assert_stopped(&self, pid: u32, cx: &mut VisualTestContext) {
        assert!(self.dir.path().join("hold").exists());
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while process_running(pid) && std::time::Instant::now() < deadline {
            draw(cx);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!process_running(pid), "tool {pid} survived cancellation");
    }
}

fn process_running(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

fn read_log(dir: &Path) -> String {
    fs::read_to_string(dir.join("received.log")).unwrap_or_default()
}

fn message(id: &str, at: i64, sender: &str, text: &str) -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: id.to_owned(),
        version: at,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some("Lab Bee".to_owned()),
        message_type: "Text".to_owned(),
        content: text.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
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

fn open_chat(rig: &Rig, conversation: &str, settings: Settings) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let conversation = conversation.to_owned();
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, conversation, window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    (view, cx)
}

/// The a–b chat in a window of its own, b's message naming a term the tool must see, its box focused.
fn chat_with(rig: &Rig, settings: Settings) -> (Entity<Compose>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![message("1001", 1_000, LAB_B, "The Quuxify build passed")],
    );
    let (view, cx) = open_chat(rig, AB, settings);
    until(cx, |cx| view.read_with(cx, |view, _| !view.items().is_empty()));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    (compose, cx)
}

fn chat(rig: &Rig) -> (Entity<Compose>, &'static mut VisualTestContext) {
    chat_with(rig, Settings::default())
}

fn type_in(compose: &Entity<Compose>, cx: &mut VisualTestContext, text: &str) {
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, |editor, cx| editor.insert(text, cx));
        });
    });
}

fn in_editor(
    compose: &Entity<Compose>,
    cx: &mut VisualTestContext,
    change: impl FnOnce(&mut Editor, &mut Context<Editor>),
) {
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, change);
        });
    });
}

fn ask(compose: &Entity<Compose>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.correct_draft(window, cx)));
}

fn status(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> CorrectionStatus {
    compose.read_with(cx, |compose, _| compose.correction_status())
}

fn value(compose: &Entity<Compose>, cx: &mut VisualTestContext) -> String {
    cx.read(|cx| compose.read(cx).value(cx).to_string())
}

fn until_status(compose: &Entity<Compose>, cx: &mut VisualTestContext, wanted: &CorrectionStatus) {
    until(cx, |cx| status(compose, cx) == *wanted);
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

/// "+" opened, then its Correct row clicked: Correct is there until the user shows it on the toolbar.
fn correct_by_menu(cx: &mut VisualTestContext) {
    click(cx, "compose-apps");
    assert!(cx.debug_bounds("compose-apps-menu").is_some(), "\"+\" opens its menu");
    click(cx, "compose-correct-row");
}

fn sent_texts(rig: &Rig) -> Vec<String> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Send { text, .. } => Some(text.rich_text().plain_text().trim().to_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_correction_says_it_is_running_and_reads_the_draft_and_the_chat() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Running);
    assert!(cx.debug_bounds("compose-correcting").is_some());
    fake.release();
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    let question = fake.questions();
    assert!(question.contains("<x0>teh report</x0>"), "{question}");
    assert!(question.contains("The Quuxify build passed"), "{question}");
}

#[test]
fn a_correction_is_shown_beside_the_draft_and_changes_nothing_by_itself() {
    let rig = Rig::start();
    let _fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    assert_eq!(value(&compose, cx), "teh report");
    assert!(sent_texts(&rig).is_empty());
    assert!(cx.debug_bounds("compose-correction-text").is_some());
    let preview = compose.read_with(cx, |compose, _| compose.correction_proposal().unwrap().preview.clone());
    assert_eq!(preview.changes, "tehthe report");
    assert_eq!(preview.result, "the report");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.correction_preview_text().unwrap().to_owned()),
        preview.changes
    );
    click(cx, "compose-correction-view");
    assert_eq!(status(&compose, cx), CorrectionStatus::Proposed);
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.correction_preview_text().unwrap().to_owned()),
        preview.result
    );
    click(cx, "compose-correction-view");
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.correction_preview_text().unwrap().to_owned()),
        preview.changes
    );
}

#[test]
fn a_correction_goes_in_the_box_as_one_edit_when_used() {
    let rig = Rig::start();
    let _fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    click(cx, "compose-correction-use");
    assert_eq!(value(&compose, cx), "the report");
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    assert!(cx.debug_bounds("compose-correction").is_none());
    assert!(sent_texts(&rig).is_empty(), "using a correction sends nothing");
    in_editor(&compose, cx, Editor::undo);
    assert_eq!(value(&compose, cx), "teh report");
}

#[test]
fn the_keys_of_a_proposal_use_it_send_it_or_leave_it() {
    let rig = Rig::start();
    let _fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);

    // Enter uses it, and the box has the keyboard again
    type_in(&compose, cx, "teh one");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    // Correct again deliberately gives the preview the keyboard.
    cx.simulate_keystrokes("ctrl-shift-g");
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(value(&compose, cx), "the one");
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    assert!(sent_texts(&rig).is_empty());
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));

    // Escape keeps the draft
    type_in(&compose, cx, " teh two");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    cx.simulate_keystrokes("ctrl-shift-g");
    draw(cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(value(&compose, cx), "the one teh two");
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    assert!(sent_texts(&rig).is_empty());

    // Ctrl+Enter sends the correction
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    cx.simulate_keystrokes("ctrl-shift-g");
    draw(cx);
    cx.simulate_keystrokes("ctrl-enter");
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["the one the two"]);
    assert_eq!(value(&compose, cx), "");
}

#[test]
fn a_proposal_arriving_never_changes_the_editors_send_key() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, cx) = chat(&rig);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_expanded(window, cx)));
    type_in(&compose, cx, "teh original");
    ask(&compose, cx);
    fake.release();
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
    assert!(sent_texts(&rig).is_empty());
    cx.simulate_keystrokes("ctrl-enter");
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["teh original"]);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
}

#[test]
fn the_buttons_send_and_keep_too() {
    let rig = Rig::start();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        apps: false,
        ..teams_core::Capabilities::ALL
    });
    let _fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh one");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    click(cx, "compose-correction-keep");
    assert_eq!(value(&compose, cx), "teh one");
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);

    correct_by_menu(cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    // the button again gives the proposal the keyboard, nothing more
    ask(&compose, cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Proposed);
    click(cx, "compose-correction-send");
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["the one"]);
}

#[test]
fn a_message_with_nothing_to_correct_says_so_and_the_line_goes_with_a_change() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "a tidy message");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::NothingToCorrect);
    assert!(cx.debug_bounds("compose-correction-clean").is_some());
    assert_eq!(value(&compose, cx), "a tidy message");
    type_in(&compose, cx, "!");
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);

    // nothing but emoji: nothing for the tool to read, and nothing asked of it
    in_editor(&compose, cx, Editor::clear);
    let asked = fake.questions().lines().count();
    type_in(&compose, cx, "🎉");
    ask(&compose, cx);
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::NothingToCorrect);
    assert_eq!(fake.questions().lines().count(), asked);
    click(cx, "compose-correction-dismiss");
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
}

#[test]
fn an_empty_box_has_nothing_to_correct() {
    let rig = Rig::start();
    let _fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    ask(&compose, cx);
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
}

#[test]
fn a_failure_says_why_keeps_the_draft_and_can_be_tried_again() {
    let rig = Rig::start();
    let signed_out = Fake::with(SIGNED_OUT);
    signed_out.install(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    until(cx, |cx| matches!(status(&compose, cx), CorrectionStatus::Failed(_)));
    let CorrectionStatus::Failed(why) = status(&compose, cx) else {
        panic!("not failed");
    };
    assert!(why.contains("isn't signed in"), "{why}");
    assert!(cx.debug_bounds("compose-correction-failed").is_some());
    assert_eq!(value(&compose, cx), "teh report");

    let working = Fake::new(&rig);
    click(cx, "compose-correction-retry");
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    assert!(!working.questions().is_empty());
}

#[test]
fn a_missing_tool_says_which_and_where_to_choose_another() {
    let rig = Rig::start();
    rig.cx.update(|cx| cx.set_global(Tools::default()));
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    draw(cx);
    let CorrectionStatus::Failed(why) = status(&compose, cx) else {
        panic!("not failed");
    };
    assert!(why.contains("No AI provider can do this"), "{why}");
    assert!(why.contains("Settings"), "{why}");
    assert_eq!(value(&compose, cx), "teh report");
}

#[test]
fn the_tool_chosen_for_this_work_is_the_one_asked_for() {
    let rig = Rig::start();
    let _fake = Fake::new(&rig);
    let settings = Settings {
        message_corrector: AiChoice::of(ToolChoice::cli(Backend::Codex)),
        ..Settings::default()
    };
    let (compose, cx) = chat_with(&rig, settings);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    draw(cx);
    let CorrectionStatus::Failed(why) = status(&compose, cx) else {
        panic!("not failed");
    };
    assert!(why.contains("Codex CLI"), "{why}");
}

#[test]
fn a_tool_that_takes_too_long_is_given_up_on() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, cx) = chat(&rig);
    compose.update(cx, |compose, _| {
        compose.set_correction_limits(Limits {
            start: Duration::from_millis(50),
            answer: Duration::from_millis(100),
        });
    });
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    until(cx, |cx| matches!(status(&compose, cx), CorrectionStatus::Failed(_)));
    let CorrectionStatus::Failed(why) = status(&compose, cx) else {
        panic!("not failed");
    };
    assert!(why.contains("too long"), "{why}");
    assert_eq!(value(&compose, cx), "teh report");
    fake.release();
}

#[test]
fn a_draft_too_long_is_not_asked_about() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, &"word ".repeat(6_000));
    ask(&compose, cx);
    draw(cx);
    let CorrectionStatus::Failed(why) = status(&compose, cx) else {
        panic!("not failed");
    };
    assert!(why.contains("too long"), "{why}");
    assert_eq!(fake.questions(), "");
}

#[test]
fn it_can_be_stopped_from_the_button_or_with_escape() {
    let rig = Rig::start();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        apps: false,
        ..teams_core::Capabilities::ALL
    });
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    let pid = fake.running_pid(cx);
    correct_by_menu(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    fake.assert_stopped(pid, cx);

    ask(&compose, cx);
    let pid = fake.running_pid(cx);
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Running);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    fake.assert_stopped(pid, cx);

    ask(&compose, cx);
    let pid = fake.running_pid(cx);
    click(cx, "compose-correction-dismiss");
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    fake.assert_stopped(pid, cx);

    fake.release();
    std::thread::sleep(Duration::from_millis(100));
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    assert_eq!(value(&compose, cx), "teh report");
}

#[test]
fn editing_the_draft_while_it_runs_drops_the_result_and_check_again_reads_the_new_text() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    draw(cx);
    type_in(&compose, cx, " and teh rest");
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::DraftChanged);
    assert!(cx.debug_bounds("compose-correction-changed").is_some());
    fake.release();
    std::thread::sleep(Duration::from_millis(150));
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::DraftChanged);
    assert_eq!(value(&compose, cx), "teh report and teh rest");

    click(cx, "compose-correction-retry");
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    let preview = compose.read_with(cx, |compose, _| compose.correction_proposal().unwrap().preview.clone());
    assert_eq!(preview.result, "the report and the rest");
}

#[test]
fn editing_the_draft_under_a_proposal_drops_it_and_nothing_can_overwrite_the_new_text() {
    let rig = Rig::start();
    let _fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    // written straight into the document, as a load does it (no change event)
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, |editor, cx| {
                let doc = teams_ui::compose::plain_doc("something else");
                editor.set_doc(doc, cx);
            });
        });
    });
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.use_correction(window, cx)));
    assert_eq!(value(&compose, cx), "something else");
    assert_eq!(status(&compose, cx), CorrectionStatus::DraftChanged);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send_correction(window, cx)));
    draw(cx);
    assert!(sent_texts(&rig).is_empty());
    // emptied, there is nothing left to say about it
    in_editor(&compose, cx, Editor::clear);
    draw(cx);
    assert_eq!(value(&compose, cx), "");
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    assert!(cx.debug_bounds("compose-correction-changed").is_none());
    type_in(&compose, cx, "a new draft");
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
}

#[test]
fn sending_the_draft_while_it_runs_ends_the_correction() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    let pid = fake.running_pid(cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    fake.assert_stopped(pid, cx);
    fake.release();
    std::thread::sleep(Duration::from_millis(150));
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    assert_eq!(sent_texts(&rig), ["teh report"]);
}

#[test]
fn a_correction_stays_with_its_own_box_and_doesn_t_take_the_keyboard_from_another() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, cx) = chat(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    draw(cx);
    let (other_view, other_cx) = open_chat(&rig, OTHER, Settings::default());
    let other = other_view.read_with(other_cx, |view, _| view.compose().clone());
    // the user has left this box
    cx.update(gpui::Window::blur);
    fake.release();
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    assert!(!cx.update(|window, cx| compose.read(cx).focused(window, cx)));
    assert_eq!(
        other.read_with(other_cx, |compose, _| compose.correction_status()),
        CorrectionStatus::Idle
    );
    assert_eq!(value(&other, other_cx), "");
}

#[test]
fn a_mention_survives_and_a_reply_s_quote_is_part_of_what_the_tool_reads() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    let (compose, cx) = chat(&rig);
    let quoted = Quoted::of(&message("1001", 1_000, LAB_B, "The Quuxify build passed"), "Lab Bee").unwrap();
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.reply_to(quoted, window, cx)));
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.insert_mention(LAB_B, "Lab Bee", window, cx);
        });
    });
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    let question = fake.questions();
    assert!(
        question.matches("The Quuxify build passed").count() >= 2,
        "the chat's line and the quote: {question}"
    );
    assert!(question.contains("<x1/>"), "{question}");
    click(cx, "compose-correction-use");
    let runs = cx.read(|cx| compose.read(cx).editor().read(cx).doc().blocks[0].runs.clone());
    assert_eq!(runs[0].attrs.mention.as_deref(), Some(LAB_B));
    assert_eq!(runs[0].text, "Lab Bee");
    assert_eq!(runs[1].text, " the report");
}

/// The a–b chat as [`chat_with`] opens it, with the settings model its views share.
fn chat_and_settings(rig: &Rig) -> (Entity<Compose>, Entity<SettingsModel>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![message("1001", 1_000, LAB_B, "The Quuxify build passed")],
    );
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let settings = parts.settings.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    until(cx, |cx| view.read_with(cx, |view, _| !view.items().is_empty()));
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    (compose, settings, cx)
}

fn use_ai(settings: &Entity<SettingsModel>, on: bool, cx: &mut VisualTestContext) {
    cx.update(|_, cx| settings.update(cx, |model, cx| model.set_ai_features(on, cx)));
    draw(cx);
}

#[test]
fn with_the_ai_features_off_the_box_offers_no_ai_and_no_tool_runs() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    let (compose, settings, cx) = chat_and_settings(&rig);
    cx.update(|_, cx| {
        settings.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.show_compose_tool(ComposeTool::Correct, true);
                    settings.show_compose_tool(ComposeTool::Translate, true);
                },
                cx,
            );
        });
    });
    draw(cx);
    let tools = ["compose-correct", "compose-translate", "compose-language"];
    for tool in tools {
        assert!(cx.debug_bounds(tool).is_some(), "{tool}");
    }
    use_ai(&settings, false, cx);
    for tool in tools {
        assert!(cx.debug_bounds(tool).is_none(), "{tool} while off");
    }
    type_in(&compose, cx, "teh report");
    // neither the keys nor a direct call starts anything
    cx.simulate_keystrokes("ctrl-shift-g");
    cx.simulate_keystrokes("ctrl-shift-y");
    ask(&compose, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.translate_draft(window, cx)));
    draw(cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    std::thread::sleep(Duration::from_millis(200));
    draw(cx);
    assert!(fake.questions().is_empty(), "the tool was asked while off");
    assert_eq!(value(&compose, cx), "teh report");
    // on again: the box offers them, and they run as before
    use_ai(&settings, true, cx);
    for tool in tools {
        assert!(cx.debug_bounds(tool).is_some(), "{tool} on again");
    }
    ask(&compose, cx);
    until_status(&compose, cx, &CorrectionStatus::Proposed);
    assert!(fake.questions().contains("teh report"));
}

#[test]
fn turning_the_ai_features_off_stops_a_correction_that_runs() {
    let rig = Rig::start();
    let fake = Fake::new(&rig);
    fake.hold();
    let (compose, settings, cx) = chat_and_settings(&rig);
    type_in(&compose, cx, "teh report");
    ask(&compose, cx);
    let pid = fake.running_pid(cx);
    use_ai(&settings, false, cx);
    assert_eq!(status(&compose, cx), CorrectionStatus::Idle);
    assert!(cx.debug_bounds("compose-correction").is_none());
    fake.assert_stopped(pid, cx);
    assert_eq!(value(&compose, cx), "teh report", "the draft stays as written");
}
