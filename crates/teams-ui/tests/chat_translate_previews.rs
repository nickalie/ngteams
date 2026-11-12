//! A translated chat's text outside its view ([`teams_ui::chat_translate::Previews`]) on GPUI's test context, against
//! a fake core keeping what the store would and a fake `claude` that logs every question (synthetic messages only):
//! the chat list's preview, read from the store or translated once with the chat as context and kept; a chat not
//! translated never asked about; the account's own message as written; an open view's translation shared rather than
//! asked twice; a notification's wait cut at its budget; a failing tool left alone; a reply's quote shown translated.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "support/fake_translator.rs"]
mod fake_translator;

use std::sync::Arc;
use std::time::Duration;

use common::{FakeCore, LAB_A, LAB_B, Rig, Written};
use fake_translator::{FAKE_CLAUDE, SIGNED_OUT, fake, questions, slow};
use futures_util::FutureExt as _;
use gpui::{AppContext as _, Entity, TestAppContext};
use teams_proto::chat::RichText;
use teams_store::{Conversation, ConversationKind, Member, Message, SentOriginal, TranslationKind};
use teams_ui::ViewTrace;
use teams_ui::chat_list::ChatList;
use teams_ui::chat_translate::{ChatTranslation, Limits, Lookup, Previews};
use teams_ui::compose::Quoted;
use teams_ui::local_ai::{Backend, Tools};
use teams_ui::settings::{Settings, SettingsModel};

const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const OTHER: &str = "19:synthetic-untranslated@thread.v2";

fn message(conversation: &str, id: &str, at: i64, sender: &str, text: &str) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: at,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "Text".to_owned(),
        content: text.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn quick() -> Limits {
    Limits {
        turns: 100,
        chars: 1_000_000,
        first: Duration::from_secs(20),
        turn: Duration::from_secs(20),
    }
}

/// The chat's history in the store, the newest first: b, a, b.
fn history(core: &FakeCore) -> Vec<Message> {
    let messages = vec![
        message(CHAT, "3", 3, LAB_B, "See you soon"),
        message(CHAT, "2", 2, LAB_A, "Lunch?"),
        message(CHAT, "1", 1, LAB_B, "Hello"),
    ];
    core.messages.lock().unwrap().insert(CHAT.to_owned(), messages.clone());
    messages
}

struct Rigged {
    app: TestAppContext,
    core: Arc<FakeCore>,
    shared: Arc<dyn teams_ui::Core>,
    settings: Entity<SettingsModel>,
    dir: tempfile::TempDir,
}

impl Rigged {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let program = fake(dir.path(), script);
        let mut app = TestAppContext::single();
        app.update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, program)])));
        let settings = app.new(|_| SettingsModel::in_memory(Settings::default()));
        let core = Arc::new(FakeCore::default());
        let shared: Arc<dyn teams_ui::Core> = core.clone();
        Self {
            app,
            core,
            shared,
            settings,
            dir,
        }
    }

    fn translate(&mut self, chat: &str, language: Option<&'static str>) {
        self.settings.update(&mut self.app, |model, cx| {
            model.change(|settings| settings.set_chat_language(LAB_A, chat, language), cx);
        });
    }

    fn previews(&mut self) -> Entity<Previews> {
        let (core, settings) = (Arc::clone(&self.shared), self.settings.clone());
        let previews = self.app.update(|cx| Previews::of(&core, &settings, cx));
        previews.update(&mut self.app, |previews, _| previews.set_limits(quick()));
        previews
    }

    fn lookup(&mut self, previews: &Entity<Previews>, message: &Message) -> Lookup {
        previews.read_with(&self.app, |previews, cx| previews.lookup(message, cx))
    }

    fn want(&mut self, previews: &Entity<Previews>, messages: Vec<Message>) {
        previews.update(&mut self.app, |previews, cx| previews.want(messages, cx));
    }

    /// Runs until `message` is known (the fake tool answering on its own thread meanwhile), the gathering's wait
    /// passed.
    fn until_known(&mut self, previews: &Entity<Previews>, message: &Message) -> Lookup {
        for _ in 0..1500 {
            self.app
                .executor()
                .advance_clock(teams_ui::chat_translate::previews::GATHER);
            self.app.run_until_parked();
            let lookup = self.lookup(previews, message);
            if lookup != Lookup::Unknown {
                return lookup;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never known");
    }

    /// What `task` gives, run until it is done.
    fn wait<T: 'static>(&mut self, task: gpui::Task<T>) -> T {
        let mut task = Box::pin(task);
        for _ in 0..1500 {
            self.app.run_until_parked();
            if let Some(done) = (&mut task).now_or_never() {
                return done;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never done");
    }
}

fn text_of(lookup: &Lookup) -> Option<String> {
    lookup.text().map(|text| text.plain_text())
}

/// The chat's newest message, translated once after the chat was read in German: the previews and the message.
fn translated_once(rig: &mut Rigged) -> (Entity<Previews>, Message) {
    let messages = history(&rig.core);
    let newest = messages[0].clone();
    let previews = rig.previews();
    // not translated: as written, nothing asked
    assert_eq!(rig.lookup(&previews, &newest), Lookup::Off);
    rig.want(&previews, vec![newest.clone()]);
    rig.app.executor().advance_clock(Duration::from_secs(1));
    rig.app.run_until_parked();
    assert!(questions(rig.dir.path()).is_empty());

    rig.translate(CHAT, Some("de"));
    assert_eq!(rig.lookup(&previews, &newest), Lookup::Unknown);
    rig.want(&previews, vec![newest.clone()]);
    // asked again while it is on its way: still one question
    rig.want(&previews, vec![newest.clone()]);
    let shown = rig.until_known(&previews, &newest);
    assert_eq!(text_of(&shown).as_deref(), Some("DE See you soon"));
    (previews, newest)
}

#[test]
fn a_translated_chat_s_preview_is_translated_once_with_its_chat_as_context_and_kept() {
    let mut rig = Rigged::new(FAKE_CLAUDE);
    let (previews, _) = translated_once(&mut rig);
    let asked = questions(rig.dir.path());
    assert_eq!(asked.len(), 1, "one batch");
    assert!(asked[0].contains("<context>"), "the messages before it go as context");
    assert!(asked[0].contains("Lunch?") && asked[0].contains("Hello"));
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.batches()), 1);
    let kept = rig.core.translations.lock().unwrap().clone();
    assert!(
        kept.iter()
            .any(|row| row.message_id == "3" && row.language == "de" && row.kind == TranslationKind::Translated),
        "kept in the store"
    );
    // the tool's process closes once idle
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.running_tools()), 1);
    rig.app
        .executor()
        .advance_clock(teams_ui::chat_translate::previews::IDLE);
    rig.app.run_until_parked();
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.running_tools()), 0);
}

#[test]
fn a_later_run_reads_the_translation_from_the_store_and_another_chat_is_never_asked_about() {
    let mut rig = Rigged::new(FAKE_CLAUDE);
    let (previews, newest) = translated_once(&mut rig);
    drop(previews);
    rig.app.run_until_parked();
    let previews = rig.previews();
    rig.want(&previews, vec![newest.clone()]);
    let shown = rig.until_known(&previews, &newest);
    assert_eq!(text_of(&shown).as_deref(), Some("DE See you soon"));
    assert_eq!(questions(rig.dir.path()).len(), 1);
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.batches()), 0);

    let elsewhere = message(OTHER, "9", 9, LAB_B, "Unrelated");
    rig.want(&previews, vec![elsewhere.clone()]);
    rig.app.executor().advance_clock(Duration::from_secs(1));
    rig.app.run_until_parked();
    assert_eq!(rig.lookup(&previews, &elsewhere), Lookup::Off);
    assert_eq!(questions(rig.dir.path()).len(), 1);
}

#[test]
fn several_new_messages_of_a_chat_go_in_one_batch() {
    let mut rig = Rigged::new(FAKE_CLAUDE);
    let messages = history(&rig.core);
    rig.translate(CHAT, Some("de"));
    let previews = rig.previews();
    let theirs = vec![messages[0].clone(), messages[2].clone()];
    rig.want(&previews, theirs.clone());
    rig.until_known(&previews, &theirs[0]);
    let first = rig.until_known(&previews, &theirs[1]);
    assert_eq!(text_of(&first).as_deref(), Some("DE Hello"));
    assert_eq!(questions(rig.dir.path()).len(), 1);
}

#[test]
fn the_account_s_own_message_shows_as_it_was_written() {
    let mut rig = Rigged::new(FAKE_CLAUDE);
    history(&rig.core);
    rig.translate(CHAT, Some("de"));
    // sent translated: its original kept by its clientmessageid
    let mut sent = message(CHAT, "4", 4, LAB_A, "EN Hallo zusammen");
    sent.client_message_id = Some("client-4".to_owned());
    rig.core.sent_originals.lock().unwrap().push(SentOriginal {
        conversation_id: CHAT.to_owned(),
        key: "client-4".to_owned(),
        language: "de".to_owned(),
        content: serde_json::to_string(&RichText::plain("Hallo zusammen")).unwrap(),
        sent_text: "EN Hallo zusammen".to_owned(),
        created_at: 4,
    });
    // written as it went (another client, or sent as written)
    let mine = message(CHAT, "5", 5, LAB_A, "Back in five");
    let previews = rig.previews();
    rig.want(&previews, vec![sent.clone(), mine.clone()]);
    let shown = rig.until_known(&previews, &sent);
    assert_eq!(text_of(&shown).as_deref(), Some("Hallo zusammen"));
    assert_eq!(rig.until_known(&previews, &mine), Lookup::AsWritten);
    assert!(
        questions(rig.dir.path()).is_empty(),
        "the account's own never go to the tool from here"
    );
    assert!(
        rig.core
            .translations
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.message_id == "4" && row.kind == TranslationKind::Original),
        "bound to its message"
    );
}

#[test]
fn an_open_view_s_translation_is_shared_not_asked_twice() {
    let mut rig = Rigged::new(FAKE_CLAUDE);
    let messages = history(&rig.core);
    rig.translate(CHAT, Some("de"));
    let (core, settings) = (Arc::clone(&rig.shared), rig.settings.clone());
    let view = rig
        .app
        .new(|cx| ChatTranslation::new(core, settings, CHAT, cx).with_limits(quick()));
    let previews = rig.previews();
    // the list asks for the newest first: the view holds it, so it waits for the view
    rig.want(&previews, vec![messages[0].clone()]);
    rig.app.executor().advance_clock(Duration::from_secs(1));
    rig.app.run_until_parked();
    let held: Vec<Message> = messages.iter().rev().cloned().collect();
    view.update(&mut rig.app, |view, cx| {
        let held: Vec<&Message> = held.iter().collect();
        view.follow(&held, &|message| format!("Name of {}", message.id), cx);
    });
    let shown = rig.until_known(&previews, &messages[0]);
    assert_eq!(text_of(&shown).as_deref(), Some("DE See you soon"));
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.batches()), 0);
    assert_eq!(view.read_with(&rig.app, |view, _| view.batches()), 1);
    assert_eq!(questions(rig.dir.path()).len(), 1);

    // the view closed: a new message it never held is asked of the tool
    drop(view);
    rig.app.run_until_parked();
    let new = message(CHAT, "6", 6, LAB_B, "Bye");
    rig.core
        .messages
        .lock()
        .unwrap()
        .get_mut(CHAT)
        .unwrap()
        .insert(0, new.clone());
    rig.want(&previews, vec![new.clone()]);
    assert_eq!(text_of(&rig.until_known(&previews, &new)).as_deref(), Some("DE Bye"));
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.batches()), 1);
}

#[test]
fn a_notification_waits_only_its_budget_for_the_translation() {
    let mut rig = Rigged::new(&slow());
    let messages = history(&rig.core);
    rig.translate(CHAT, Some("de"));
    let previews = rig.previews();
    let newest = messages[0].clone();
    let budget = Duration::from_millis(300);
    let waited = previews.update(&mut rig.app, |previews, cx| {
        previews.translated_within(newest.clone(), budget, cx)
    });
    rig.app.run_until_parked();
    rig.app.executor().advance_clock(budget);
    assert_eq!(rig.wait(waited), None, "shown as written once the budget is spent");
    // it still comes, for the list and a later notification
    let shown = rig.until_known(&previews, &newest);
    assert_eq!(text_of(&shown).as_deref(), Some("DE See you soon"));
    let again = previews.update(&mut rig.app, |previews, cx| {
        previews.translated_within(newest.clone(), budget, cx)
    });
    assert_eq!(
        rig.wait(again).map(|text| text.plain_text()).as_deref(),
        Some("DE See you soon")
    );
    // a chat not translated waits for nothing
    let elsewhere = message(OTHER, "9", 9, LAB_B, "Unrelated");
    let none = previews.update(&mut rig.app, |previews, cx| {
        previews.translated_within(elsewhere, budget, cx)
    });
    assert_eq!(rig.wait(none), None);
    assert_eq!(questions(rig.dir.path()).len(), 1);
}

#[test]
fn a_failing_tool_is_left_alone_and_the_text_shows_as_written() {
    let mut rig = Rigged::new(SIGNED_OUT);
    let messages = history(&rig.core);
    rig.translate(CHAT, Some("de"));
    let previews = rig.previews();
    rig.want(&previews, vec![messages[0].clone()]);
    assert_eq!(rig.until_known(&previews, &messages[0]), Lookup::AsWritten);
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.batches()), 1);
    // paused: the next message isn't asked
    rig.want(&previews, vec![messages[2].clone()]);
    assert_eq!(rig.until_known(&previews, &messages[2]), Lookup::AsWritten);
    assert_eq!(previews.read_with(&rig.app, |previews, _| previews.batches()), 1);
    assert!(rig.core.translations.lock().unwrap().is_empty(), "nothing kept");
}

#[test]
fn the_chat_list_shows_a_translated_chat_s_preview_in_its_language() {
    let rig = Rig::start();
    let dir = tempfile::tempdir().unwrap();
    let program = fake(dir.path(), FAKE_CLAUDE);
    rig.cx
        .update(|cx| cx.set_global(Tools::with(vec![(Backend::Claude, program)])));
    rig.core.conversations.lock().unwrap().push(Conversation {
        id: CHAT.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(3),
    });
    rig.core.members.lock().unwrap().insert(
        CHAT.to_owned(),
        vec![Member::new(LAB_A, None), Member::new(LAB_B, None)],
    );
    history(&rig.core);
    let settings = rig
        .cx
        .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default())));
    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
    let models = rig.models.clone();
    let trace = Arc::new(ViewTrace::new(Written::default()));
    let (shared, kept) = (Arc::clone(&core), settings.clone());
    let list = rig.cx.update(|cx| {
        let previews = Previews::of(&shared, &kept, cx);
        cx.new(|cx| {
            let mut list = ChatList::new(core, models, trace, None, cx);
            list.set_previews(previews, cx);
            list
        })
    });
    rig.cx.run_until_parked();
    let shown = |rig: &Rig| {
        let mut cx = rig.cx.clone();
        list.update(&mut cx, |list, cx| {
            let row = list.row(CHAT).cloned().unwrap();
            list.preview_shown(&row, cx).to_string()
        })
    };
    assert_eq!(shown(&rig), "See you soon");
    settings.update(&mut rig.cx.clone(), |model, cx| {
        model.change(|settings| settings.set_chat_language(LAB_A, CHAT, Some("de")), cx);
    });
    let mut seen = String::new();
    for _ in 0..1500 {
        seen = shown(&rig);
        if seen.starts_with("DE") {
            break;
        }
        rig.cx
            .executor()
            .advance_clock(teams_ui::chat_translate::previews::GATHER);
        rig.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(seen, "DE See you soon");
    assert_eq!(questions(dir.path()).len(), 1);
}

#[test]
fn a_reply_s_quote_shows_translated_and_is_sent_as_written() {
    let quoted = message(CHAT, "1", 1, LAB_B, "Hello there");
    let plain = Quoted::of(&quoted, "Lab B").unwrap();
    assert_eq!(plain.shown_preview(), "Hello there");
    let shown = plain.clone().shown_as(&RichText::plain("DE Hello there"));
    assert_eq!(shown.shown_preview(), "DE Hello there");
    assert_eq!(shown.preview, "Hello there", "the quote sent is as written");
    // the account's reply as written keeps the quote as the chat showed it
    let reply = RichText {
        nodes: vec![
            shown.node(),
            teams_proto::chat::Node::Text {
                text: "Hallo".to_owned(),
                marks: teams_proto::chat::Marks::NONE,
            },
        ],
        truncated: false,
    };
    let kept = shown.as_shown_in(&reply).plain_text();
    assert!(kept.contains("DE Hello there"), "{kept}");
    assert_eq!(plain.as_shown_in(&reply), reply);
}

/// A fake Claude Code that logs each question and answers its first message with a term after it.
const TERMING_CLAUDE: &str = r#"#!/bin/bash
log="$(dirname "$0")/received.log"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' '<m id=\"1\" lang=\"en\">Die da</m><term from=\"Abnahme\" to=\"Abnahme (sign-off)\"/>'
done
"#;

#[test]
fn a_preview_s_reply_goes_with_what_it_answers_and_its_chat_s_terms_are_kept() {
    let mut rig = Rigged::new(TERMING_CLAUDE);
    rig.core.conversations.lock().unwrap().push(Conversation {
        id: CHAT.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: Some("Synthetic topic".to_owned()),
        version: 1,
        last_activity: None,
    });
    rig.core.members.lock().unwrap().insert(
        CHAT.to_owned(),
        vec![Member::new(LAB_A, None), Member::new(LAB_B, None)],
    );
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab B".to_owned());
    // the question far back, out of the page of context before the reply
    // and longer than a history line: it goes whole
    let question = format!("Which one? {}the end", "really ".repeat(80));
    let mut held = vec![message(CHAT, "1", 1, LAB_B, &question)];
    held.extend((2..60).map(|at| message(CHAT, &at.to_string(), at, LAB_A, "Filler")));
    let mut reply = message(
        CHAT,
        "60",
        60,
        LAB_B,
        "<blockquote itemscope=\"\" itemtype=\"http://schema.skype.com/Reply\" itemid=\"1\"><strong \
         itemprop=\"mri\" itemid=\"8:orgid:synthetic-b\">Lab B</strong><p itemprop=\"preview\">Which…</p>\
         </blockquote><p>That one</p>",
    );
    reply.message_type = "RichText/Html".to_owned();
    held.push(reply.clone());
    held.reverse();
    rig.core.messages.lock().unwrap().insert(CHAT.to_owned(), held);
    let previews = rig.previews();
    rig.translate(CHAT, Some("de"));
    rig.want(&previews, vec![reply.clone()]);
    let shown = rig.until_known(&previews, &reply);
    assert!(text_of(&shown).is_some_and(|text| text.contains("Die da")), "{shown:?}");
    let asked = questions(rig.dir.path());
    assert_eq!(asked.len(), 1);
    for told in [
        "Title: Synthetic topic",
        "Members: Lab B",
        "[q1] Lab B: Which one?",
        "really the end",
        "re=\\\"q1\\\"",
    ] {
        assert!(asked[0].contains(told), "{told}: {}", asked[0]);
    }
    for _ in 0..500 {
        rig.app.run_until_parked();
        if !rig.core.glossary.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let kept = rig.core.glossary.lock().unwrap().clone();
    assert!(
        kept.iter()
            .any(|kept| kept.conversation_id == CHAT && kept.language == "de" && kept.term == "Abnahme"),
        "{kept:?}"
    );
}
