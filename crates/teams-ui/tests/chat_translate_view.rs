//! A translated chat as its views draw it, on GPUI's test context over the fake engine (`common`) and a fake `claude`
//! (synthetic messages only): the header's language picker sets the chat's language; the chat's messages drawn
//! translated with Teams' translation mark, a click on it showing the message as written; a message sent from the box
//! translated before it goes; and a tool that fails saying why in the chat and in the box, which sends nothing until
//! Send without translating.

#![cfg(test)]

#[path = "support/fake_tool.rs"]
#[expect(dead_code, reason = "the shared fake CLI helper serves several test harnesses")]
mod fake_tool;

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{LAB_A, LAB_B, Rig};
use gpui::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px, size,
};
use teams_core::Action;
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_translate::{ChatTranslation, header_button};
use teams_ui::chat_view::{ChatView, Item};
use teams_ui::local_ai::{Backend, Tools};
use teams_ui::settings::{ChatLanguage, Settings, SettingsModel};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// A fake Claude Code: each message of a batch as `DE <text>`, an outgoing message as `EN <text>`.
const FAKE_CLAUDE: &str = r#"#!/bin/bash
while IFS= read -r line; do
  ans=""
  case "$line" in
    *'<translate>'*)
      while IFS= read -r m; do
        n=$(printf '%s' "$m" | sed 's/^<m id=\\"\([0-9]*\)\\".*/\1/')
        t=$(printf '%s' "$m" | sed 's/^.*\\">\(.*\)<\/m>$/\1/')
        ans="$ans<m id=\\\"$n\\\" lang=\\\"en\\\">DE $t</m>"
      done < <(printf '%s' "$line" | grep -o '<m id=\\"[0-9]*\\" from=\\"[^\\]*\\">[^<]*</m>')
      ;;
    *'<outgoing'*)
      t=$(printf '%s' "$line" | sed 's/.*<outgoing to=\\"[a-z]*\\">\(.*\)<\/outgoing>.*/\1/')
      ans="<out lang=\\\"en\\\">EN $t</out>"
      ;;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

/// A fake Claude Code that isn't signed in.
const SIGNED_OUT: &str = "#!/bin/bash\necho 'Invalid API key. Please run /login' >&2\nexit 1\n";

fn tool(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    fake_tool::write(dir, name, body)
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

/// Draws until `done` holds, the fake tool answering on its own thread meanwhile.
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

/// The a–b chat (two of b's messages, one of a's) translated into German, in a window of its own.
fn translated_chat(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![
            message("1003", 3_000, LAB_B, "See you"),
            message("1002", 2_000, LAB_A, "Lunch?"),
            message("1001", 1_000, LAB_B, "Hello"),
        ],
    );
    rig.cx.update(teams_ui::window::init);
    let settings = Settings {
        chat_languages: vec![ChatLanguage {
            account: LAB_A.to_owned(),
            chat: AB.to_owned(),
            language: Some("de"),
            every_message: true,
        }],
        ..Settings::default()
    };
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    (view, cx)
}

fn mark_of(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) -> Option<teams_ui::chat_translate::Mark> {
    cx.update(|_, cx| {
        view.read(cx).items().iter().find_map(|item| match item {
            Item::Message(message) if message.id == id => message.translation.clone(),
            _ => None,
        })
    })
}

fn translation(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<ChatTranslation> {
    cx.update(|_, cx| view.read(cx).translation().clone())
}

fn send(view: &Entity<ChatView>, cx: &mut VisualTestContext, text: &str) {
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.focus(window, cx);
            compose.editor().update(cx, |editor, cx| editor.insert(text, cx));
            compose.send(window, cx);
        });
    });
}

fn sent_texts(rig: &Rig) -> Vec<String> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Send { text, .. } => Some(
                text.rich_text()
                    .plain_text()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        })
        .collect()
}

#[test]
fn a_translated_chat_draws_its_marks_and_sends_in_the_chat_s_language() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    until(cx, |cx| {
        matches!(
            mark_of(&view, cx, "1001"),
            Some(teams_ui::chat_translate::Mark::Translated { .. })
        )
    });
    assert!(cx.debug_bounds("translation-mark-1001").is_some());
    assert!(cx.debug_bounds("chat-translate-notice").is_none());
    // the mark shows the message as written, and back
    let mark = cx.debug_bounds("translation-mark-1001").unwrap();
    cx.simulate_click(mark.center(), Modifiers::none());
    draw(cx);
    assert!(matches!(
        mark_of(&view, cx, "1001"),
        Some(teams_ui::chat_translate::Mark::Original { .. })
    ));

    // written in German, sent in the chat's language (English), after the box said it was translating
    send(&view, cx, "Hallo");
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["EN Hallo"]);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|_, cx| assert!(!compose.read(cx).translating() && compose.read(cx).untranslated().is_none()));
}

#[test]
fn a_tool_that_fails_says_so_and_a_send_goes_only_as_written_when_asked() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", SIGNED_OUT);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    until(cx, |cx| cx.debug_bounds("chat-translate-notice").is_some());
    assert!(cx.debug_bounds("chat-translate-retry").is_some());

    send(&view, cx, "Hallo");
    until(cx, |cx| cx.debug_bounds("compose-untranslated").is_some());
    assert!(sent_texts(&rig).is_empty(), "nothing is sent untranslated on its own");
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|_, cx| {
        let said = compose.read(cx).untranslated().cloned().unwrap_or_default();
        assert!(said.contains("isn't signed in"), "{said}");
    });
    // Try again fails the same way; Send without translating sends it as written
    let again = cx.debug_bounds("compose-translate-retry").unwrap();
    cx.simulate_click(again.center(), Modifiers::none());
    until(cx, |cx| cx.debug_bounds("compose-untranslated").is_some());
    assert!(sent_texts(&rig).is_empty());
    let as_written = cx.debug_bounds("compose-send-as-written").unwrap();
    cx.simulate_click(as_written.center(), Modifiers::none());
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["Hallo"]);
    assert!(cx.debug_bounds("compose-untranslated").is_none());

    // the chat's Try again
    let retry = cx.debug_bounds("chat-translate-retry").unwrap();
    cx.simulate_click(retry.center(), Modifiers::none());
    until(cx, |cx| cx.debug_bounds("chat-translate-notice").is_some());
    // turned off: the notice goes
    let model = translation(&view, cx);
    cx.update(|_, cx| model.update(cx, |model, cx| model.set_language(None, cx)));
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-notice").is_none());
    assert_eq!(mark_of(&view, cx, "1001"), None);
}

/// The writes `rig` was asked for that send or edit a message.
fn message_writes(rig: &Rig) -> Vec<Action> {
    rig.core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter(|action| matches!(action, Action::Send { .. } | Action::Edit { .. }))
        .cloned()
        .collect()
}

#[test]
fn a_send_being_translated_goes_as_it_was_sent_and_a_cancelled_edit_s_goes_not_at_all() {
    use teams_ui::compose::{Loaded, Mode, TRANSLATING_FIRST};
    use teams_ui::editor::doc::{Block, Doc};

    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    // the fake, a second slower to translate a message sent
    let slow = FAKE_CLAUDE.replace("*'<outgoing'*)\n", "*'<outgoing'*)\n      sleep 1\n");
    let fake = tool(dir.path(), "claude", &slow);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    let lunch = || Loaded {
        doc: Doc::new(vec![Block::paragraph("Lunch?")]),
        quotes: Vec::new(),
    };

    // while a new message is translated: no edit starts (the message stays in the box as it was sent, to go or to be
    // sent again), a file attached stays, and the message goes as the post it was sent as, leaving the box
    send(&view, cx, "Hallo");
    cx.update(|_, cx| assert!(compose.read(cx).translating()));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1002".to_owned(), lunch(), window, cx)));
    cx.update(|_, cx| {
        assert_eq!(compose.read(cx).problem().cloned(), Some(TRANSLATING_FIRST.into()));
        assert_eq!(*compose.read(cx).mode(), Mode::New);
    });
    let file = dir.path().join("made-up-notes.txt");
    fs::write(&file, "synthetic").unwrap();
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_files(std::slice::from_ref(&file), window, cx);
        });
    });
    until(cx, |_| !message_writes(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["EN Hallo"]);
    assert_eq!(message_writes(&rig).len(), 1);
    cx.update(|_, cx| {
        assert_eq!(compose.read(cx).value(cx).trim(), "", "what was sent left the box");
        assert_eq!(
            compose.read(cx).attachments().len(),
            1,
            "the file attached meanwhile stays"
        );
    });
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.discard(window, cx)));

    // an edit sent, then cancelled while it is translated: nothing of it goes
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1002".to_owned(), lunch(), window, cx)));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    cx.update(|_, cx| assert!(compose.read(cx).translating()));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.cancel(window, cx)));
    cx.update(|_, cx| assert!(!compose.read(cx).translating()));
    std::thread::sleep(Duration::from_millis(1_500));
    draw(cx);
    assert_eq!(message_writes(&rig).len(), 1, "{:?}", message_writes(&rig));
}

#[test]
fn a_translated_send_refused_comes_back_beside_a_file_attached_meanwhile() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let slow = FAKE_CLAUDE.replace("*'<outgoing'*)\n", "*'<outgoing'*)\n      sleep 1\n");
    let fake = tool(dir.path(), "claude", &slow);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    send(&view, cx, "Hallo");
    let file = dir.path().join("made-up-notes.txt");
    fs::write(&file, "synthetic").unwrap();
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_files(std::slice::from_ref(&file), window, cx);
        });
    });
    // the engine refuses the write when the translation came
    *rig.core.stopped.lock().unwrap() = true;
    until(cx, |_| !message_writes(&rig).is_empty());
    until(cx, |cx| {
        cx.update(|_, cx| !compose.read(cx).translating() && compose.read(cx).value(cx).trim() == "Hallo")
    });
    assert_eq!(message_writes(&rig).len(), 1, "asked once");
    cx.update(|_, cx| assert_eq!(compose.read(cx).attachments().len(), 1, "the file stays beside it"));
}

/// The header's language picker on its own.
struct Header(Entity<ChatTranslation>);

impl Render for Header {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().child(header_button(&self.0, window, cx))
    }
}

#[test]
fn the_header_s_picker_sets_the_chat_s_language() {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(|cx| cx.set_global(Tools::with(Vec::new())));
    let settings = rig
        .cx
        .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default())));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let shared = settings.clone();
    let model = rig
        .cx
        .update(|cx| cx.new(|cx| ChatTranslation::new(core, shared, AB, cx)));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let picker = model.clone();
    let (_, cx) = app.add_window_view(|_, _| Header(picker));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    let button = cx.debug_bounds("chat-translate").unwrap();
    cx.simulate_click(button.center(), Modifiers::none());
    draw(cx);
    assert!(
        cx.debug_bounds("chat-translate-off").is_some(),
        "the menu lists Show as written"
    );
    let catalan = cx.debug_bounds("chat-translate-ca").unwrap();
    cx.simulate_click(catalan.center(), Modifiers::none());
    draw(cx);
    cx.update(|_, cx| {
        assert_eq!(model.read(cx).language(), Some("ca"));
        assert_eq!(settings.read(cx).settings().chat_language_of(LAB_A, AB), Some("ca"));
    });
    assert!(
        cx.debug_bounds("chat-translate-menu").is_none(),
        "picking closes the menu"
    );

    // the long list filters as the user types in its search field, which has the keyboard; Enter picks the first
    cx.simulate_click(button.center(), Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-search").is_some());
    cx.simulate_input("germ");
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-de").is_some());
    assert!(cx.debug_bounds("chat-translate-ca").is_none());
    assert!(cx.debug_bounds("chat-translate-off").is_none());
    cx.simulate_keystrokes("enter");
    draw(cx);
    cx.update(|_, cx| assert_eq!(model.read(cx).language(), Some("de")));
    assert!(cx.debug_bounds("chat-translate-menu").is_none());

    // nothing matches: it says so; Down and Up move through what is left; Escape closes it, picking nothing
    cx.simulate_click(button.center(), Modifiers::none());
    draw(cx);
    cx.simulate_input("zzz");
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-none").is_some());
    cx.simulate_keystrokes("enter");
    draw(cx);
    cx.update(|_, cx| assert_eq!(model.read(cx).language(), Some("de")));
    for _ in 0..3 {
        cx.simulate_keystrokes("backspace");
    }
    cx.simulate_input("fr");
    draw(cx);
    cx.simulate_keystrokes("down down up");
    draw(cx);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-menu").is_none());
    cx.update(|_, cx| assert_eq!(model.read(cx).language(), Some("de")));

    // "Show as written" is a row like the languages
    cx.simulate_click(button.center(), Modifiers::none());
    draw(cx);
    cx.simulate_keystrokes("down enter");
    draw(cx);
    cx.update(|_, cx| assert_eq!(model.read(cx).language(), None));
}

#[test]
fn the_picker_s_filter_matches_names_and_codes() {
    use teams_ui::chat_translate::picker::{AS_WRITTEN, matching};
    assert_eq!(matching("").len(), teams_ui::translate::TARGET_LANGUAGES.len() + 1);
    assert_eq!(matching("")[0], (None, AS_WRITTEN));
    let names: Vec<&str> = matching(" GERM ").into_iter().map(|(_, name)| name).collect();
    assert_eq!(names, ["German"]);
    assert!(
        matching("pt")
            .iter()
            .all(|(code, _)| code.is_some_and(|code| code.starts_with("pt")))
    );
    assert!(matching("chinese").len() >= 2);
    assert!(matching("written").contains(&(None, AS_WRITTEN)));
    assert!(matching("zzz").is_empty());
}

/// A fake Claude Code that answers nothing until a `go` file is beside it, then each message of a batch as `DE <n>`.
const HOLDING: &str = r#"#!/bin/bash
dir="$(dirname "$0")"
while IFS= read -r line; do
  for _ in $(seq 400); do [ -e "$dir/go" ] && break; sleep 0.05; done
  ans=""
  for n in $(printf '%s' "$line" | grep -o '<m id=\\"[0-9]*\\"' | grep -o '[0-9]*'); do
    ans="$ans<m id=\\\"$n\\\" lang=\\\"en\\\">DE $n</m>"
  done
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

#[test]
fn a_chat_being_translated_spins_its_button_and_says_translating_on_each_message() {
    use teams_ui::chat_translate::Mark;
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", HOLDING);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    let model = translation(&view, cx);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let header = model.clone();
    let (_, header_cx) = app.add_window_view(|_, _| Header(header));
    header_cx.simulate_resize(size(px(400.0), px(300.0)));
    draw(cx);
    draw(header_cx);
    // under way: the button spins (and still opens its menu), each message says so in place
    assert!(header_cx.debug_bounds("chat-translate-busy").is_some());
    assert_eq!(mark_of(&view, cx, "1001"), Some(Mark::Waiting));
    assert!(cx.debug_bounds("translation-mark-1001").is_some());
    cx.update(|_, cx| {
        let translation = model.read(cx);
        assert!(translation.busy());
        assert_eq!(
            teams_ui::chat_translate::picker::tip_of(translation),
            "Translating into German… 0 of 3"
        );
    });
    let button = header_cx.debug_bounds("chat-translate").unwrap();
    header_cx.simulate_click(button.center(), Modifiers::none());
    draw(header_cx);
    assert!(
        header_cx.debug_bounds("chat-translate-off").is_some(),
        "the menu still opens"
    );
    header_cx.simulate_keystrokes("escape");
    draw(header_cx);

    fs::write(dir.path().join("go"), "").unwrap();
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    draw(header_cx);
    assert!(header_cx.debug_bounds("chat-translate-busy").is_none());
    assert!(header_cx.debug_bounds("chat-translate").is_some());
}

struct Pane(Entity<ChatView>);

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.0.read(cx).translation().clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(48.0))
                    .flex()
                    .justify_end()
                    .child(header_button(&model, window, cx)),
            )
            .child(div().flex_1().min_h_0().child(self.0.clone()))
    }
}

fn chat_pane(
    rig: &Rig,
    language: Option<&'static str>,
    every_message: bool,
) -> (Entity<ChatView>, Entity<SettingsModel>, &'static mut VisualTestContext) {
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![
            message("1003", 3_000, LAB_B, "See you"),
            message("1002", 2_000, LAB_A, "Lunch?"),
            message("1001", 1_000, LAB_B, "Hello"),
        ],
    );
    rig.cx.update(teams_ui::window::init);
    let settings = Settings {
        chat_languages: vec![ChatLanguage {
            account: LAB_A.to_owned(),
            chat: AB.to_owned(),
            language,
            every_message,
        }],
        ..Settings::default()
    };
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let settings = parts.settings.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (pane, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
        Pane(view)
    });
    cx.simulate_resize(size(px(800.0), px(600.0)));
    let view = cx.update(|_, cx| pane.read(cx).0.clone());
    draw(cx);
    (view, settings, cx)
}

fn offer_of(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) -> Option<teams_ui::chat_translate::Offer> {
    cx.update(|_, cx| {
        view.read(cx).items().iter().find_map(|item| match item {
            Item::Message(message) if message.id == id => message.translate,
            _ => None,
        })
    })
}

fn hover(cx: &mut VisualTestContext, row: &'static str) {
    let row = cx.debug_bounds(row).unwrap();
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap();
    cx.simulate_click(at.center(), Modifiers::none());
    draw(cx);
}

#[test]
fn a_chat_translated_a_message_at_a_time_translates_another_s_message_from_its_hover_bar_or_menu() {
    use teams_ui::chat_translate::{Mark, Offer};
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", HOLDING);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, _, cx) = chat_pane(&rig, Some("de"), false);
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::Translate));
    assert_eq!(offer_of(&view, cx, "1002"), None, "the account's own");
    assert_eq!(mark_of(&view, cx, "1001"), None);
    hover(cx, "message-1002");
    assert!(cx.debug_bounds("message-actions").is_some());
    assert!(cx.debug_bounds("action-translate").is_none());

    hover(cx, "message-1001");
    click(cx, "action-translate");
    assert_eq!(mark_of(&view, cx, "1001"), Some(Mark::Waiting));
    assert!(cx.debug_bounds("translation-mark-1001").is_some());
    assert!(cx.debug_bounds("action-translate").is_none(), "hidden while it waits");
    assert_eq!(mark_of(&view, cx, "1003"), None);
    fs::write(dir.path().join("go"), "").unwrap();
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::SeeOriginal));
    assert_eq!(mark_of(&view, cx, "1003"), None);
    assert_eq!(offer_of(&view, cx, "1003"), Some(Offer::Translate));
    original_and_back(&view, cx);
}

fn original_and_back(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    use teams_ui::chat_translate::{Mark, Offer};
    hover(cx, "message-1001");
    click(cx, "action-translate");
    assert!(matches!(mark_of(view, cx, "1001"), Some(Mark::Original { .. })));
    assert_eq!(offer_of(view, cx, "1001"), Some(Offer::Translate));
    hover(cx, "message-1001");
    click(cx, "message-more");
    click(cx, "menu-translate");
    assert!(matches!(mark_of(view, cx, "1001"), Some(Mark::Translated { .. })));
    assert!(cx.debug_bounds("message-menu").is_none());
}

#[test]
fn a_chat_translated_whole_offers_no_translate_on_its_messages() {
    let rig = Rig::start();
    rig.cx.update(|cx| cx.set_global(Tools::with(Vec::new())));
    let (view, cx) = translated_chat(&rig);
    draw(cx);
    assert_eq!(offer_of(&view, cx, "1001"), None);
    hover(cx, "message-1001");
    assert!(cx.debug_bounds("message-actions").is_some());
    assert!(cx.debug_bounds("action-translate").is_none());
    click(cx, "message-more");
    assert!(cx.debug_bounds("message-menu").is_some());
    assert!(cx.debug_bounds("menu-translate").is_none());

    let model = translation(&view, cx);
    cx.update(|_, cx| model.update(cx, |model, cx| model.set_language(None, cx)));
    draw(cx);
    assert_eq!(
        offer_of(&view, cx, "1001"),
        Some(teams_ui::chat_translate::Offer::PickLanguage)
    );
    hover(cx, "message-1001");
    assert!(
        cx.debug_bounds("action-translate").is_none(),
        "no language menu drawn to open"
    );
    cx.update(|window, cx| view.update(cx, |view, cx| view.translate_message("1001", window, cx)));
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-menu").is_none());
}

#[test]
fn a_message_asked_for_whose_tool_fails_says_so_in_the_chat() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", SIGNED_OUT);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, _, cx) = chat_pane(&rig, Some("de"), false);
    assert!(cx.debug_bounds("chat-translate-notice").is_none());
    hover(cx, "message-1001");
    click(cx, "action-translate");
    until(cx, |cx| cx.debug_bounds("chat-translate-notice").is_some());
    assert_eq!(
        offer_of(&view, cx, "1001"),
        Some(teams_ui::chat_translate::Offer::Translate)
    );
}

#[test]
fn with_no_language_translate_opens_the_header_s_menu_whose_switch_keeps_a_message_at_a_time() {
    use teams_ui::chat_translate::Offer;
    let rig = Rig::start();
    rig.cx.update(|cx| cx.set_global(Tools::with(Vec::new())));
    *rig.core.read_only.lock().unwrap() = true;
    let (view, settings, cx) = chat_pane(&rig, None, true);
    let model = translation(&view, cx);
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::PickLanguage));
    hover(cx, "message-1001");
    click(cx, "action-translate");
    assert!(
        cx.debug_bounds("chat-translate-menu").is_some(),
        "the bar's Translate opens it"
    );
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-menu").is_none());
    hover(cx, "message-1001");
    click(cx, "message-more");
    click(cx, "menu-translate");
    assert!(
        cx.debug_bounds("chat-translate-menu").is_some(),
        "the menu's Translate opens it"
    );
    assert!(cx.debug_bounds("message-menu").is_none());

    click(cx, "chat-translate-every");
    assert!(cx.debug_bounds("chat-translate-menu").is_some(), "the menu stays open");
    cx.update(|_, cx| {
        let kept = settings.read(cx).settings();
        assert!(!kept.translates_every_message(LAB_A, AB));
        assert_eq!(kept.chat_language_of(LAB_A, AB), None);
    });
    cx.simulate_input("germ");
    draw(cx);
    assert!(
        cx.debug_bounds("chat-translate-every").is_some(),
        "whatever the search leaves"
    );
    cx.simulate_keystrokes("enter");
    draw(cx);
    cx.update(|_, cx| {
        assert_eq!(model.read(cx).language(), Some("de"));
        assert!(!model.read(cx).every_message());
        assert_eq!(model.read(cx).batches(), 0);
    });
    assert_eq!(mark_of(&view, cx, "1001"), None);
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::Translate));

    click(cx, "chat-translate");
    cx.simulate_keystrokes("up enter");
    draw(cx);
    cx.update(|_, cx| {
        assert!(model.read(cx).every_message());
        assert!(settings.read(cx).settings().translates_every_message(LAB_A, AB));
    });
    assert!(
        cx.debug_bounds("chat-translate-menu").is_some(),
        "the switch leaves it open"
    );
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.debug_bounds("chat-translate-menu").is_none());
}
