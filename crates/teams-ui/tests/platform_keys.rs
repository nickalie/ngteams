//! The shortcuts on each platform's own key, as a user presses them: Cmd (⌘) on macOS, Ctrl on Linux and Windows.
//! The message box's formatting, select-all, copy, paste, undo and send, the shell's switcher, new chat, panes and
//! font size, Activity with its Unread and @Mentions chips, and the Translate and Turn off keys all answer to the
//! platform's key and not to the other one; the hints name it ("Cmd" or "Ctrl"); on macOS the message box also takes
//! the system's Option and Command arrow keys. GPUI's test context, the fake engine (`common`); every id and text is
//! made up. These run on macOS too (the other view tests press Ctrl itself).

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig};
use gpui::{
    Action, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px,
};
use teams_core::activity::ActivityFilter;
use teams_proto::chat::Marks;
use teams_store::{Conversation, ConversationKind, Member};
use teams_ui::ViewTrace;
use teams_ui::activity_view::TurnOffApp;
use teams_ui::compose::TranslateDraft;
use teams_ui::editor::{Editor, EditorEvent};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};

/// The platform's shortcut key, as GPUI's keystrokes name it, and the other one, which must do nothing.
#[cfg(target_os = "macos")]
const KEY: &str = "cmd";
#[cfg(target_os = "macos")]
const OTHER: &str = "ctrl";
#[cfg(not(target_os = "macos"))]
const KEY: &str = "ctrl";
#[cfg(not(target_os = "macos"))]
const OTHER: &str = "cmd";
/// The word a hint names that key with.
#[cfg(target_os = "macos")]
const HINT: &str = "Cmd";
#[cfg(not(target_os = "macos"))]
const HINT: &str = "Ctrl";

/// `keys` with every `@` replaced by the platform's key (`@-b` is ⌘B on a Mac, Ctrl+B elsewhere).
fn on(keys: &str) -> String {
    keys.replace('@', KEY)
}

/// `keys` with every `@` replaced by the key the platform doesn't use for shortcuts.
fn off(keys: &str) -> String {
    keys.replace('@', OTHER)
}

// --- the message box ---

/// The editor in a window, as its owner holds it.
struct Host {
    editor: Entity<Editor>,
    submitted: usize,
    _events: gpui::Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(500.0)).child(self.editor.clone())
    }
}

fn editor_window() -> (Entity<Host>, &'static mut VisualTestContext) {
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    app.update(|cx| {
        gpui_component::init(cx);
        teams_ui::theme::init(cx);
    });
    let (host, cx) = app.add_window_view(|window, cx| {
        let editor = cx.new(|cx| Editor::new(window, cx).placeholder("Type a message"));
        let events = cx.subscribe(&editor, |host: &mut Host, _, event, _| {
            if *event == EditorEvent::Submit {
                host.submitted += 1;
            }
        });
        Host {
            editor,
            submitted: 0,
            _events: events,
        }
    });
    let editor = cx.update(|_, cx| host.read(cx).editor.clone());
    cx.update(|window, cx| {
        let focus = editor.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    });
    draw(cx);
    (host, cx)
}

/// Draws the window a few times, as the views' tests do, so the input handler is in place.
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn text_of(host: &Entity<Host>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| host.read(cx).editor.read(cx).text())
}

fn selected_of(host: &Entity<Host>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| host.read(cx).editor.read(cx).selected_text())
}

fn bold_runs(host: &Entity<Host>, cx: &mut VisualTestContext) -> Vec<(String, bool)> {
    cx.update(|_, cx| {
        host.read(cx).editor.read(cx).doc().blocks[0]
            .runs
            .iter()
            .map(|run| (run.text.clone(), run.attrs.marks.contains(Marks::BOLD)))
            .collect()
    })
}

#[test]
fn the_platform_key_formats_and_the_other_key_does_not() {
    let (host, cx) = editor_window();
    cx.simulate_input("plain ");
    cx.simulate_keystrokes(&off("@-b"));
    cx.simulate_input("still ");
    cx.simulate_keystrokes(&on("@-b"));
    cx.simulate_input("bold");
    cx.simulate_keystrokes(&on("@-b"));
    cx.simulate_input(" end");
    assert_eq!(
        bold_runs(&host, cx),
        [
            ("plain still ".to_owned(), false),
            ("bold".to_owned(), true),
            (" end".to_owned(), false)
        ],
        "{KEY}+B formats, {OTHER}+B does nothing"
    );
    cx.simulate_keystrokes(&on("shift-left shift-left shift-left @-i @-u"));
    let marks = cx.update(|_, cx| {
        host.read(cx).editor.read(cx).doc().blocks[0]
            .runs
            .last()
            .unwrap()
            .attrs
            .marks
    });
    assert!(
        marks.contains(Marks::ITALIC) && marks.contains(Marks::UNDERLINE),
        "{KEY}+I and {KEY}+U"
    );
    cx.simulate_keystrokes(&on("@-z @-z"));
    assert_eq!(bold_runs(&host, cx).len(), 3, "{KEY}+Z undid them");
}

#[test]
fn select_all_copy_paste_and_send_use_the_platform_key() {
    let (host, cx) = editor_window();
    cx.simulate_input("hello there");
    cx.simulate_keystrokes(&off("@-a"));
    assert_eq!(selected_of(&host, cx), "", "{OTHER}+A selects nothing");
    cx.simulate_keystrokes(&on("@-a"));
    assert_eq!(selected_of(&host, cx), "hello there");
    cx.simulate_keystrokes(&on("@-c right shift-enter @-v"));
    assert_eq!(text_of(&host, cx), "hello there\nhello there", "{KEY}+C then {KEY}+V");
    cx.simulate_keystrokes(&on("@-a @-x"));
    assert_eq!(text_of(&host, cx), "", "{KEY}+X cut it all");
    cx.simulate_keystrokes(&on("@-v"));
    assert_eq!(text_of(&host, cx), "hello there\nhello there");
    cx.simulate_keystrokes(&off("@-enter"));
    assert_eq!(
        cx.update(|_, cx| host.read(cx).submitted),
        0,
        "{OTHER}+Enter sends nothing"
    );
    cx.simulate_keystrokes(&on("@-enter"));
    assert_eq!(cx.update(|_, cx| host.read(cx).submitted), 1, "{KEY}+Enter sends");
}

/// macOS's own text keys: Option moves and deletes by words, Command goes to the line's and the text's ends.
#[cfg(target_os = "macos")]
#[test]
fn option_and_command_arrows_move_as_in_macos_text_fields() {
    let (host, cx) = editor_window();
    cx.simulate_input("one two three");
    cx.simulate_keystrokes("alt-left");
    cx.simulate_input("X");
    assert_eq!(text_of(&host, cx), "one two Xthree", "⌥← to the word's start");
    cx.simulate_keystrokes("cmd-left");
    cx.simulate_input("S");
    assert_eq!(text_of(&host, cx), "Sone two Xthree", "⌘← to the line's start");
    cx.simulate_keystrokes("alt-right alt-right");
    cx.simulate_input("Y");
    assert_eq!(text_of(&host, cx), "Sone twoY Xthree", "⌥→ to each word's end");
    cx.simulate_keystrokes("cmd-right");
    cx.simulate_input("E");
    assert_eq!(text_of(&host, cx), "Sone twoY XthreeE", "⌘→ to the line's end");
    cx.simulate_keystrokes("alt-backspace");
    assert_eq!(text_of(&host, cx), "Sone twoY ", "⌥⌫ deletes the word");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("second line");
    cx.simulate_keystrokes("cmd-up");
    cx.simulate_input("T");
    assert_eq!(text_of(&host, cx), "TSone twoY \nsecond line", "⌘↑ to the text's start");
    cx.simulate_keystrokes("cmd-down");
    cx.simulate_input("B");
    assert_eq!(text_of(&host, cx), "TSone twoY \nsecond lineB", "⌘↓ to the text's end");
    cx.simulate_keystrokes("alt-shift-left");
    assert_eq!(selected_of(&host, cx), "lineB", "⌥⇧← selects the word");
    cx.simulate_keystrokes("cmd-shift-left");
    assert_eq!(
        selected_of(&host, cx),
        "second lineB",
        "⌘⇧← selects to the line's start"
    );
    cx.simulate_keystrokes("cmd-up alt-delete");
    assert_eq!(text_of(&host, cx), " twoY \nsecond lineB", "⌥⌦ deletes the word after");
}

/// Linux and Windows keep Ctrl's word and text keys; Option's are macOS's.
#[cfg(not(target_os = "macos"))]
#[test]
fn ctrl_arrows_still_move_by_words_and_to_the_text_ends() {
    let (host, cx) = editor_window();
    cx.simulate_input("one two three");
    cx.simulate_keystrokes("ctrl-left");
    cx.simulate_input("X");
    assert_eq!(text_of(&host, cx), "one two Xthree");
    cx.simulate_keystrokes("ctrl-home");
    cx.simulate_input("S");
    assert_eq!(text_of(&host, cx), "Sone two Xthree");
    cx.simulate_keystrokes("ctrl-end ctrl-backspace");
    assert_eq!(
        text_of(&host, cx),
        "Sone two ",
        "Ctrl+End, then Ctrl+Backspace deletes the word"
    );
}

// --- the shell ---

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

fn shell_window() -> (Entity<Shell>, &'static mut VisualTestContext) {
    let rig = Rig::start();
    rig.core.conversations.lock().unwrap().push(Conversation {
        id: AB.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(3_000),
    });
    rig.core
        .members
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    rig.cx.update(teams_ui::window::init);
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    // the rig's engine side stays alive for the test's length, as the shell's tests keep it
    Box::leak(Box::new(rig));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    draw(cx);
    (shell, cx)
}

fn main_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Main {
    cx.update(|_, cx| shell.read(cx).main().clone())
}

fn font_size(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> u16 {
    cx.update(|_, cx| shell.read(cx).settings().read(cx).settings().appearance.font_size)
}

#[test]
fn the_switcher_new_chat_and_the_panes_open_with_the_platform_key() {
    let (shell, cx) = shell_window();
    cx.simulate_keystrokes(&off("@-k"));
    draw(cx);
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_none()),
        "{OTHER}+K opens nothing"
    );
    cx.simulate_keystrokes(&on("@-k"));
    draw(cx);
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_some()),
        "{KEY}+K opens the switcher"
    );
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_none()),
        "Escape closes it"
    );

    cx.simulate_keystrokes(&off("@-n"));
    draw(cx);
    assert_ne!(main_of(&shell, cx), Main::NewChat, "{OTHER}+N opens nothing");
    cx.simulate_keystrokes(&on("@-n"));
    draw(cx);
    assert_eq!(main_of(&shell, cx), Main::NewChat, "{KEY}+N starts a new chat");

    cx.simulate_keystrokes(&on("@-4"));
    draw(cx);
    assert_eq!(main_of(&shell, cx), Main::Calendar, "{KEY}+4 opens the calendar");
    cx.simulate_keystrokes(&on("@-2"));
    draw(cx);
    assert_ne!(main_of(&shell, cx), Main::Calendar, "{KEY}+2 goes back to the chats");
}

#[test]
fn the_font_size_keys_use_the_platform_key() {
    let (shell, cx) = shell_window();
    let start = font_size(&shell, cx);
    cx.simulate_keystrokes(&off("@-="));
    assert_eq!(font_size(&shell, cx), start, "{OTHER}+= changes nothing");
    cx.simulate_keystrokes(&on("@-= @-="));
    assert_eq!(font_size(&shell, cx), start + 2, "{KEY}+= twice");
    cx.simulate_keystrokes(&on("@--"));
    assert_eq!(font_size(&shell, cx), start + 1, "{KEY}+-");
    cx.simulate_keystrokes(&on("@-0"));
    assert_eq!(font_size(&shell, cx), teams_ui::theme::FONT_SIZE, "{KEY}+0 resets");
}

#[test]
fn hints_name_the_platform_key() {
    assert_eq!(
        teams_ui::global_search::PLACEHOLDER,
        format!("Press {HINT}+Alt+G to go right to a chat or channel")
    );
}

/// The Activity shown (`shown`) and its chip.
fn activity_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> (bool, ActivityFilter) {
    cx.update(|_, cx| {
        let view = shell.read(cx).activity_view().read(cx);
        (view.shown(), view.filter())
    })
}

#[test]
fn activity_and_its_chips_open_with_the_platform_key() {
    let (shell, cx) = shell_window();
    cx.simulate_keystrokes(&off("@-1"));
    draw(cx);
    assert!(!activity_of(&shell, cx).0, "{OTHER}+1 opens nothing");
    cx.simulate_keystrokes(&on("@-1"));
    draw(cx);
    assert_eq!(
        activity_of(&shell, cx),
        (true, ActivityFilter::All),
        "{KEY}+1 opens Activity"
    );

    cx.simulate_keystrokes(&off("@-alt-u"));
    draw(cx);
    assert_eq!(
        activity_of(&shell, cx).1,
        ActivityFilter::All,
        "{OTHER}+Alt+U changes nothing"
    );
    cx.simulate_keystrokes(&on("@-alt-u"));
    draw(cx);
    assert_eq!(activity_of(&shell, cx).1, ActivityFilter::Unread, "{KEY}+Alt+U: Unread");
    cx.simulate_keystrokes(&on("@-alt-m"));
    draw(cx);
    assert_eq!(
        activity_of(&shell, cx).1,
        ActivityFilter::Mentions,
        "{KEY}+Alt+M: @Mentions"
    );
}

/// Whether every key bound to `action` is held with the platform's key and none with the other one.
fn on_the_platform_key(action: &dyn Action, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let mut bindings = keymap.bindings_for_action(action).peekable();
        bindings.peek().is_some()
            && bindings.all(|binding| {
                binding.keystrokes().iter().all(|keystroke| {
                    let modifiers = keystroke.modifiers();
                    let other = if cfg!(target_os = "macos") {
                        modifiers.control
                    } else {
                        modifiers.platform
                    };
                    modifiers.secondary() && !other
                })
            })
    })
}

#[test]
fn translate_and_turn_off_are_bound_to_the_platform_key() {
    let (_shell, cx) = shell_window();
    assert!(on_the_platform_key(&TranslateDraft, cx), "Translate is {KEY}+Shift+Y");
    assert!(on_the_platform_key(&TurnOffApp, cx), "Turn off is {KEY}+Alt+Shift+O");
}
