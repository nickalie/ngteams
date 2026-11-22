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
use teams_ui::chat_view::{ChatView, Item, Place};
use teams_ui::local_ai::{Backend, Tools};
use teams_ui::settings::{ChatLanguage, Settings, SettingsModel};

const CHANNEL: &str = "19:made-up-channel@thread.tacv2";
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
            language: "de",
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

/// A channel translated into German, its posts' box in a window of its own, with a draft and one file in the
/// channel's folder, sent: the send is held while the fake translates it (a second).
fn held_channel_post(rig: &Rig, dir: &Path) -> (Entity<teams_ui::compose::Compose>, &'static mut VisualTestContext) {
    let slow = FAKE_CLAUDE.replace("*'<outgoing'*)\n", "*'<outgoing'*)\n      sleep 1\n");
    let fake = tool(dir, "claude", &slow);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let mut hello = message("1001", 1_000, LAB_B, "Hello");
    hello.conversation_id = CHANNEL.to_owned();
    rig.core
        .messages
        .lock()
        .unwrap()
        .insert(CHANNEL.to_owned(), vec![hello]);
    rig.cx.update(teams_ui::window::init);
    let settings = Settings {
        chat_languages: vec![ChatLanguage {
            account: LAB_A.to_owned(),
            chat: CHANNEL.to_owned(),
            language: "de",
        }],
        ..Settings::default()
    };
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| {
        let compose = ChatView::channel_box(&parts, CHANNEL, &Place::Channel, "Post in channel".into(), window, cx);
        ChatView::in_place(parts, CHANNEL.to_owned(), Place::Channel, compose, window, cx)
    });
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    let file = dir.join("made-up-notes.txt");
    fs::write(&file, "synthetic").unwrap();
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.attach_files(std::slice::from_ref(&file), window, cx)
        });
    });
    draw(cx);
    send(&view, cx, "Hallo");
    cx.update(|_, cx| assert!(compose.read(cx).translating()));
    (compose, cx)
}

#[test]
fn discard_during_a_held_channel_post_sends_nothing_and_recycles_its_file() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let (compose, cx) = held_channel_post(&rig, dir.path());

    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.discard(window, cx)));
    cx.update(|_, cx| assert!(!compose.read(cx).translating()));
    std::thread::sleep(Duration::from_millis(1_500));
    draw(cx);
    assert!(message_writes(&rig).is_empty(), "{:?}", message_writes(&rig));
    assert_eq!(rig.core.discarded_files.lock().unwrap().len(), 1);
}

#[test]
fn a_file_of_a_held_channel_post_is_not_removed_and_goes_with_it() {
    use teams_ui::compose::TRANSLATING_FIRST;

    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let (compose, cx) = held_channel_post(&rig, dir.path());
    let id = cx.update(|_, cx| compose.read(cx).attachments()[0].id);
    cx.update(|_, cx| compose.update(cx, |compose, cx| compose.remove_attachment(id, cx)));
    draw(cx);
    cx.update(|_, cx| {
        assert_eq!(compose.read(cx).problem().cloned(), Some(TRANSLATING_FIRST.into()));
        assert_eq!(compose.read(cx).attachments().len(), 1);
    });
    assert!(rig.core.discarded_files.lock().unwrap().is_empty());
    until(cx, |_| !message_writes(&rig).is_empty());
    match &message_writes(&rig)[..] {
        [Action::Send { text, .. }] => assert_eq!(text.files().len(), 1),
        other => panic!("one post with its file: {other:?}"),
    }
    assert!(rig.core.discarded_files.lock().unwrap().is_empty());
}

/// The header's language picker on its own.
struct Header(Entity<ChatTranslation>);

impl Render for Header {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_4().children(header_button(&self.0, window, cx))
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

#[test]
fn a_translated_send_keeps_the_apps_cards_of_the_box() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = translated_chat(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    let card = teams_proto::chat::AppCard::new(
        "00000000-0000-4000-8000-00000000e001".to_owned(),
        "Made-up Finder".to_owned(),
        None,
        "application/vnd.microsoft.card.adaptive".to_owned(),
        serde_json::json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Made up"}]}),
    );
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.add_app_card(card.clone(), window, cx)));
    send(&view, cx, "Hallo");
    // a card that comes in while the text is translated is not sent: it stays for the next message
    let later = teams_proto::chat::AppCard::new(
        "00000000-0000-4000-8000-00000000e001".to_owned(),
        "Made-up Finder".to_owned(),
        None,
        "application/vnd.microsoft.card.adaptive".to_owned(),
        serde_json::json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Made up later"}]}),
    );
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.add_app_card(later.clone(), window, cx)));
    until(cx, |_| !sent_texts(&rig).is_empty());
    assert_eq!(sent_texts(&rig), ["EN Hallo"]);
    let cards: Vec<usize> = rig
        .core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Send { text, .. } => Some(text.cards().len()),
            _ => None,
        })
        .collect();
    assert_eq!(cards, [1], "the card goes with the translated text");
    assert_eq!(cx.update(|_, cx| compose.read(cx).app_cards().to_vec()), [later]);
}

/// The a–b chat of [`translated_chat`], not translated, in a window of its own.
fn plain_chat(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let (view, _, cx) = plain_chat_and_settings(rig);
    (view, cx)
}

/// [`plain_chat`] with the settings its views share.
fn plain_chat_and_settings(rig: &Rig) -> (Entity<ChatView>, Entity<SettingsModel>, &'static mut VisualTestContext) {
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![
            message("1003", 3_000, LAB_B, "See you"),
            message("1002", 2_000, LAB_A, "Lunch?"),
            message("1001", 1_000, LAB_B, "Hello"),
        ],
    );
    rig.cx.update(teams_ui::window::init);
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let settings = parts.settings.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    (view, settings, cx)
}

/// Points at the message `id`, opens its more-options menu and its Translation list, and clicks the list's first row.
fn translate_from_menu(cx: &mut VisualTestContext, id: &str) {
    let row = cx
        .debug_bounds(Box::leak(format!("message-{id}").into_boxed_str()))
        .unwrap();
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
    let more = cx.debug_bounds("message-more").unwrap();
    cx.simulate_click(more.center(), Modifiers::none());
    draw(cx);
    let list = cx
        .debug_bounds("action-translation")
        .expect("the menu offers Translation");
    cx.simulate_click(list.center(), Modifiers::none());
    draw(cx);
    let entry = cx
        .debug_bounds("action-translate")
        .expect("its list offers the translation");
    cx.simulate_click(entry.center(), Modifiers::none());
    draw(cx);
}

fn offer_of(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) -> Option<teams_ui::chat_translate::Offer> {
    cx.update(|_, cx| {
        view.read(cx).items().iter().find_map(|item| match item {
            Item::Message(message) if message.id == id => message.translate,
            _ => None,
        })
    })
}

#[test]
fn translate_in_a_message_s_menu_shows_it_translated_and_see_original_switches_back() {
    use teams_ui::chat_translate::{Mark, Offer};
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = plain_chat(&rig);
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::Translate));
    assert_eq!(mark_of(&view, cx, "1001"), None);

    translate_from_menu(cx, "1001");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    assert_eq!(
        mark_of(&view, cx, "1003"),
        None,
        "only the message asked for is translated"
    );
    assert!(cx.debug_bounds("chat-translate-notice").is_none());
    let kept = rig.core.translations.lock().unwrap().clone();
    assert!(
        kept.iter()
            .any(|row| row.message_id == "1001" && row.language == "en" && row.content.contains("DE Hello")),
        "kept in the store in the Translate to language"
    );

    assert_eq!(rig.core.translated_messages.lock().unwrap().len(), 1);

    // See original message shows it as written, unmarked; Translate shows it again from the store, asking nothing
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::SeeOriginal(Some("English"))));
    let model = translation(&view, cx);
    let asked = cx.update(|_, cx| model.read(cx).batches());
    translate_from_menu(cx, "1001");
    assert_eq!(mark_of(&view, cx, "1001"), None);
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::Translate));
    assert!(rig.core.translated_messages.lock().unwrap().is_empty());
    translate_from_menu(cx, "1001");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    assert_eq!(cx.update(|_, cx| model.read(cx).batches()), asked);

    // the account's own message too, by its key
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.select(Some("1002".to_owned()), cx));
        view.read(cx).focus_handle().clone().focus(window, cx);
    });
    cx.simulate_keystrokes("t");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1002"), Some(Mark::Translated { .. }))
    });

    // the chat opened again shows both translated at once, from the store
    let parts = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (again, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    until(cx, |cx| {
        ["1001", "1002"]
            .iter()
            .all(|id| matches!(mark_of(&again, cx, id), Some(Mark::Translated { .. })))
    });
    assert_eq!(mark_of(&again, cx, "1003"), None);
    let model = translation(&again, cx);
    assert_eq!(cx.update(|_, cx| model.read(cx).batches()), 0);
}

#[test]
fn with_the_ai_features_off_a_message_offers_no_translation_and_t_does_nothing() {
    use teams_ui::chat_translate::{Mark, Offer};
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, settings, cx) = plain_chat_and_settings(&rig);
    translate_from_menu(cx, "1001");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    let model = translation(&view, cx);
    let asked = cx.update(|_, cx| model.read(cx).batches());
    let use_ai = |on: bool, cx: &mut VisualTestContext| {
        cx.update(|_, cx| settings.update(cx, |model, cx| model.set_ai_features(on, cx)));
        draw(cx);
    };

    use_ai(false, cx);
    // the translation shown and its mark go; no message offers one
    assert_eq!(mark_of(&view, cx, "1001"), None);
    assert!(cx.debug_bounds("translation-mark-1001").is_none());
    for id in ["1001", "1002", "1003"] {
        assert_eq!(offer_of(&view, cx, id), None, "{id}");
    }
    // the menu has no Translation row
    let row = cx.debug_bounds("message-1003").unwrap();
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
    let more = cx.debug_bounds("message-more").unwrap();
    cx.simulate_click(more.center(), Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("message-menu").is_some());
    assert!(cx.debug_bounds("action-translation").is_none());
    cx.simulate_keystrokes("escape");
    draw(cx);
    // T on a selected message translates nothing
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.select(Some("1002".to_owned()), cx));
        view.read(cx).focus_handle().clone().focus(window, cx);
    });
    cx.simulate_keystrokes("t");
    draw(cx);
    std::thread::sleep(Duration::from_millis(200));
    draw(cx);
    assert_eq!(mark_of(&view, cx, "1002"), None);
    assert_eq!(
        cx.update(|_, cx| model.read(cx).batches()),
        asked,
        "nothing asked of the tool"
    );

    // on again: the message translated before shows translated again, and the menu offers it
    use_ai(true, cx);
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    assert_eq!(offer_of(&view, cx, "1002"), Some(Offer::Translate));
}

#[test]
fn a_message_translated_alone_says_why_it_failed_and_tries_again() {
    use teams_ui::chat_translate::{Mark, Offer};
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", SIGNED_OUT);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = plain_chat(&rig);
    translate_from_menu(cx, "1001");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Stopped { .. }))
    });
    let Some(Mark::Stopped { why }) = mark_of(&view, cx, "1001") else {
        unreachable!()
    };
    assert!(why.contains("isn't signed in"), "{why}");
    assert!(
        cx.debug_bounds("chat-translate-notice").is_none(),
        "the chat itself isn't translated"
    );
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::Translate));
    // a click on the mark tries again (the tool is asked once more), and fails the same way
    let model = translation(&view, cx);
    let asked = cx.update(|_, cx| model.read(cx).batches());
    assert!(asked > 0, "the first try went to the tool");
    let mark = cx.debug_bounds("translation-mark-1001").unwrap();
    cx.simulate_click(mark.center(), Modifiers::none());
    draw(cx);
    until(cx, |cx| {
        cx.update(|_, cx| model.read(cx).batches()) > asked
            && matches!(mark_of(&view, cx, "1001"), Some(Mark::Stopped { .. }))
    });
}

#[test]
fn an_edit_of_an_own_message_translated_alone_starts_from_the_text_as_sent() {
    use teams_ui::chat_translate::Mark;
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = plain_chat(&rig);
    let select = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.select(Some("1002".to_owned()), cx));
            view.read(cx).focus_handle().clone().focus(window, cx);
        });
    };
    select(cx);
    cx.simulate_keystrokes("t");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1002"), Some(Mark::Translated { .. }))
    });
    // the chat isn't translated: the edit goes out as written, so it starts from what was sent, not the translation
    select(cx);
    cx.simulate_keystrokes("e");
    draw(cx);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    let text = cx.read(|cx| compose.read(cx).editor().read(cx).doc().plain_text());
    assert_eq!(text, "Lunch?");
}

#[test]
fn a_message_translated_alone_shows_translated_again_once_the_chat_is_read_as_written() {
    use teams_ui::chat_translate::Mark;
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = plain_chat(&rig);
    translate_from_menu(cx, "1001");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    let model = translation(&view, cx);
    cx.update(|_, cx| model.update(cx, |model, cx| model.set_language(Some("de"), cx)));
    until(cx, |cx| {
        ["1001", "1003"]
            .iter()
            .all(|id| matches!(mark_of(&view, cx, id), Some(Mark::Translated { .. })))
    });
    // no message comes meanwhile: the store is asked again at once
    cx.update(|_, cx| model.update(cx, |model, cx| model.set_language(None, cx)));
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. })) && mark_of(&view, cx, "1003").is_none()
    });
}

/// b's two messages a minute apart, the second a follow-up under the first's name and time, not translated.
fn follow_up_chat(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![
            message("1004", 61_000, LAB_B, "Bye"),
            message("1001", 1_000, LAB_B, "Hello"),
        ],
    );
    rig.cx.update(teams_ui::window::init);
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    (view, cx)
}

#[test]
fn a_follow_up_s_translation_mark_stands_over_its_bubble() {
    use teams_ui::chat_translate::Mark;
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = follow_up_chat(&rig);
    translate_from_menu(cx, "1004");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1004"), Some(Mark::Translated { .. }))
    });
    let mark = cx.debug_bounds("translation-mark-1004").unwrap();
    let bubble = cx.debug_bounds("bubble-1004").unwrap();
    assert!(
        mark.bottom() <= bubble.top(),
        "the mark is over the bubble, as Teams draws it: {mark:?} {bubble:?}"
    );
    assert!(
        (mark.left() - bubble.left()).abs() < px(8.0),
        "at the bubble's left edge: {mark:?} {bubble:?}"
    );
    // the pointer on the message: its hover bar keeps clear of the mark, which a click still reaches
    cx.simulate_mouse_move(bubble.center(), None, Modifiers::none());
    draw(cx);
    let bar = cx.debug_bounds("message-actions").expect("the hover bar shows");
    assert!(
        bar.intersect(&mark).is_empty(),
        "the hover bar covers the mark: {bar:?} {mark:?}"
    );
}

#[test]
fn a_follow_up_s_failure_mark_stands_over_its_bubble() {
    use teams_ui::chat_translate::Mark;
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", SIGNED_OUT);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = follow_up_chat(&rig);
    translate_from_menu(cx, "1004");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1004"), Some(Mark::Stopped { .. }))
    });
    let mark = cx.debug_bounds("translation-mark-1004").unwrap();
    let bubble = cx.debug_bounds("bubble-1004").unwrap();
    assert!(mark.bottom() <= bubble.top(), "{mark:?} {bubble:?}");
}

/// Opens the more-options menu of `id` with a click, the focus left in the compose box.
fn menu_by_click(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) {
    cx.update(|window, cx| {
        let compose = view.read(cx).compose().clone();
        compose.update(cx, |compose, cx| compose.focus(window, cx));
    });
    let row = cx
        .debug_bounds(Box::leak(format!("message-{id}").into_boxed_str()))
        .unwrap();
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    draw(cx);
    let more = cx.debug_bounds("message-more").unwrap();
    cx.simulate_click(more.center(), Modifiers::none());
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).options().map(str::to_owned)),
        Some(id.to_owned())
    );
}

#[test]
fn one_escape_closes_a_menu_opened_with_a_click() {
    let rig = Rig::start();
    let (view, cx) = plain_chat(&rig);
    menu_by_click(&view, cx, "1001");
    let compose_focused =
        |cx: &mut VisualTestContext| cx.update(|window, cx| view.read(cx).compose().read(cx).focused(window, cx));
    assert!(compose_focused(cx), "a click leaves the focus in the box");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).options().map(str::to_owned)), None);
    assert!(compose_focused(cx), "the box keeps the focus");
    // its Translation list open too: one Escape closes both
    menu_by_click(&view, cx, "1001");
    let list = cx.debug_bounds("action-translation").unwrap();
    cx.simulate_click(list.center(), Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("message-translation-menu").is_some());
    assert!(compose_focused(cx), "a click in the menu leaves the focus in the box");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).options().map(str::to_owned)), None);
    assert!(cx.debug_bounds("message-translation-menu").is_none());
}

/// Presses and lets go of `key`, as a keyboard does: a focused row clicks when the key comes up.
fn press(cx: &mut VisualTestContext, key: &str) {
    let keystroke = gpui::Keystroke::parse(key).unwrap();
    cx.simulate_keystrokes(key);
    cx.simulate_event(gpui::KeyUpEvent { keystroke });
    draw(cx);
}

/// Selects `id` with the keys: Escape out of the box, then Up until it is selected.
fn select_by_keys(view: &Entity<ChatView>, cx: &mut VisualTestContext, id: &str) {
    cx.update(|window, cx| {
        let compose = view.read(cx).compose().clone();
        compose.update(cx, |compose, cx| compose.focus(window, cx));
    });
    cx.simulate_keystrokes("escape");
    draw(cx);
    for _ in 0..5 {
        cx.simulate_keystrokes("up");
        draw(cx);
        if cx
            .update(|_, cx| view.read(cx).selected().map(str::to_owned))
            .as_deref()
            == Some(id)
        {
            return;
        }
    }
    panic!("Up never selected {id}");
}

/// The plain chat with the fake translator, `1001` selected with the keys.
fn keyed_chat(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, fake)])));
    let (view, cx) = plain_chat(rig);
    select_by_keys(&view, cx, "1001");
    (view, cx)
}

fn options_of(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| view.read(cx).options().map(str::to_owned))
}

fn list_focused(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| view.read(cx).focus_handle().is_focused(window))
}

fn keys(cx: &mut VisualTestContext, keys: &[&str]) {
    for key in keys {
        cx.simulate_keystrokes(key);
        draw(cx);
    }
}

#[test]
fn the_menu_key_opens_the_selected_message_s_menu_and_escape_gives_the_keys_back() {
    let rig = Rig::start();
    let (view, cx) = keyed_chat(&rig);
    keys(cx, &["menu"]);
    assert_eq!(options_of(&view, cx).as_deref(), Some("1001"));
    assert!(!list_focused(&view, cx), "the keys are in the menu");
    keys(cx, &["escape"]);
    assert_eq!(options_of(&view, cx), None);
    assert!(list_focused(&view, cx), "the keys are back on the message list");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected().map(str::to_owned))
            .as_deref(),
        Some("1001")
    );
}

#[test]
fn shift_f10_opens_the_menu_and_its_keys_reach_translate() {
    use teams_ui::chat_translate::Mark;
    let rig = Rig::start();
    let (view, cx) = keyed_chat(&rig);
    // Copy first, Down past Mark as unread to Translation, Right opens its list, Left closes it, Right again, Enter
    // translates
    keys(cx, &["shift-f10", "down", "down", "right"]);
    assert_eq!(options_of(&view, cx).as_deref(), Some("1001"));
    assert!(
        cx.debug_bounds("message-translation-menu").is_some(),
        "Right opens the Translation list"
    );
    keys(cx, &["left"]);
    assert!(cx.debug_bounds("message-translation-menu").is_none(), "Left closes it");
    assert_eq!(options_of(&view, cx).as_deref(), Some("1001"), "the menu stays");
    keys(cx, &["right"]);
    assert!(cx.debug_bounds("message-translation-menu").is_some());
    press(cx, "enter");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    assert_eq!(options_of(&view, cx), None);
    assert!(list_focused(&view, cx), "the keys are back on the message list");
}

#[test]
fn escape_in_the_translation_list_closes_it_alone_and_its_settings_row_takes_enter() {
    let rig = Rig::start();
    let (view, cx) = keyed_chat(&rig);
    keys(cx, &["shift-f10", "down", "down"]);
    press(cx, "enter");
    assert!(
        cx.debug_bounds("message-translation-menu").is_some(),
        "Enter on Translation opens its list"
    );
    keys(cx, &["escape"]);
    assert!(cx.debug_bounds("message-translation-menu").is_none());
    assert_eq!(options_of(&view, cx).as_deref(), Some("1001"));
    keys(cx, &["escape"]);
    assert_eq!(options_of(&view, cx), None);

    let settings = std::rc::Rc::new(std::cell::Cell::new(false));
    let told = settings.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &teams_ui::chat_view::ChatViewEvent, _| {
            if matches!(event, teams_ui::chat_view::ChatViewEvent::TranslationSettings) {
                told.set(true);
            }
        })
        .detach();
    });
    keys(cx, &["shift-f10", "down", "down", "right", "down"]);
    press(cx, "enter");
    assert!(settings.get(), "Settings opens the settings' page");
    assert_eq!(options_of(&view, cx), None);
}

/// The plain chat in the kit's root, as the app's window holds it (the root binds Tab and Shift+Tab), `1001` selected
/// with the keys.
fn rooted_keyed_chat(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    rig.core.messages.lock().unwrap().insert(
        AB.to_owned(),
        vec![
            message("1002", 2_000, LAB_A, "Lunch?"),
            message("1001", 1_000, LAB_B, "Hello"),
        ],
    );
    rig.cx.update(teams_ui::window::init);
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
        teams_ui::window::KitRoot::new(view, window, cx)
    });
    let view = cx.update(|_, cx| root.read(cx).view().clone().downcast::<ChatView>().unwrap());
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    select_by_keys(&view, cx, "1001");
    (view, cx)
}

#[test]
fn tab_and_shift_tab_in_the_menu_close_it_and_give_the_keys_back() {
    let rig = Rig::start();
    let (view, cx) = rooted_keyed_chat(&rig);
    for tab in ["tab", "shift-tab"] {
        keys(cx, &["shift-f10"]);
        assert_eq!(options_of(&view, cx).as_deref(), Some("1001"));
        assert!(!list_focused(&view, cx), "the keys are in the menu");
        keys(cx, &[tab]);
        assert_eq!(options_of(&view, cx), None, "{tab} closes the menu");
        assert!(list_focused(&view, cx), "{tab}: the keys are back on the message list");
    }
    // with Translation's list open, Tab closes the menu and the list
    keys(cx, &["shift-f10", "down", "down", "right"]);
    assert!(cx.debug_bounds("message-translation-menu").is_some());
    keys(cx, &["tab"]);
    assert_eq!(options_of(&view, cx), None);
    assert!(cx.debug_bounds("message-translation-menu").is_none());
    assert!(list_focused(&view, cx), "the keys are back on the message list");
}

/// A fake draft translator: the draft's "privet" in English, the language it was asked for kept.
struct DraftFake(std::sync::Mutex<Vec<String>>);

struct DraftEngine(Arc<DraftFake>);

struct DraftSession(Arc<DraftFake>);

impl teams_ui::draft_translate::Engine for DraftEngine {
    fn start(
        &self,
        _: &teams_ui::local_ai::Launch,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<Box<dyn teams_ui::draft_translate::Session>, teams_ui::local_ai::Failure> {
        Ok(Box::new(DraftSession(Arc::clone(&self.0))))
    }
}

impl teams_ui::draft_translate::Session for DraftSession {
    fn ask(
        &mut self,
        message: &str,
        _: Duration,
        _: &std::sync::atomic::AtomicBool,
    ) -> Result<String, teams_ui::local_ai::Failure> {
        self.0.0.lock().unwrap().push(message.to_owned());
        let body = message.split("<message>\n").nth(1).unwrap_or_default();
        let body = body.strip_suffix("\n</message>").unwrap_or_default();
        Ok(format!("<translated>{}</translated>", body.replace("privet", "hello")))
    }

    fn close(self: Box<Self>) {}
}

#[test]
fn a_user_reads_in_russian_and_sends_in_english() {
    use teams_ui::chat_translate::{Mark, Offer};
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let fake = tool(dir.path(), "claude", FAKE_CLAUDE);
    let drafts = Arc::new(DraftFake(std::sync::Mutex::new(Vec::new())));
    let engine: Arc<dyn teams_ui::draft_translate::Engine> = Arc::new(DraftEngine(Arc::clone(&drafts)));
    rig.cx.update(|cx| {
        cx.set_global(Tools::with(vec![(Backend::Claude, fake)]));
        teams_ui::draft_translate::set_engine(cx, engine);
    });
    let (view, settings, cx) = plain_chat_and_settings(&rig);
    cx.update(|_, cx| {
        settings.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.message_translate_to = "Russian".to_owned();
                    settings.draft_translate_to = "English".to_owned();
                },
                cx,
            );
        });
    });
    draw(cx);

    // a message of the other person, read in Russian
    translate_from_menu(cx, "1001");
    until(cx, |cx| {
        matches!(mark_of(&view, cx, "1001"), Some(Mark::Translated { .. }))
    });
    let kept = rig.core.translations.lock().unwrap().clone();
    assert!(
        kept.iter().any(|row| row.message_id == "1001" && row.language == "ru"),
        "read in Russian: {kept:?}"
    );
    assert_eq!(offer_of(&view, cx, "1001"), Some(Offer::SeeOriginal(Some("English"))));

    // the user's draft, translated into English and sent
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.focus(window, cx);
            compose.editor().update(cx, |editor, cx| editor.insert("privet", cx));
        });
    });
    draw(cx);
    cx.simulate_keystrokes("ctrl-shift-y");
    until(cx, |cx| {
        cx.update(|_, cx| compose.read(cx).value(cx).to_string()) == "hello"
    });
    let asked = drafts.0.lock().unwrap().clone();
    assert!(
        asked
            .first()
            .is_some_and(|asked| asked.starts_with("<language>English</language>")),
        "{asked:?}"
    );
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    until(cx, |_| sent_texts(&rig) == ["hello"]);

    // the two languages stay apart
    let languages = cx.update(|_, cx| {
        let settings = settings.read(cx).settings();
        (
            settings.message_translate_to.clone(),
            settings.draft_translate_to.clone(),
        )
    });
    assert_eq!(languages, ("Russian".to_owned(), "English".to_owned()));
}
