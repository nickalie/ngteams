//! QA of task 721, in the same window and run as the checks of task 584: Tab and Shift+Tab between a box and its
//! controls, as a user presses them.
//!
//! - **Every box**: the chat's, a channel's post, a thread's reply and the stacked box of a 520 px window. Tab takes
//!   the keys from the text to the tools; Shift+Tab gives them back to the text, the draft kept. The tools are one
//!   stop, as in Teams: Right moves to the next tool, Tab goes on to Send, and Shift+Tab from Send comes back to the
//!   tool the keys were on, then to the text.
//! - **A staged file**: Tab goes from the text to the file's button, not to the tools; Shift+Tab comes back to the
//!   text.
//! - **The ring**: screenshots of the ring after Tab, in Teams Light and Teams Dark.

use std::path::PathBuf;
use std::{fs, thread};

use gpui::{App, Entity, Window};

use super::{Check, SETTLE, SandboxResult, Shell, User, main_box, text_of};

/// QA 721's checks of the first run.
pub(super) const FIRST: [Check; 2] = [("qa721_every_box", every_box), ("qa721_staged_file", staged_file)];

/// The made-up file that the staged-file check puts in the box.
const STAGED: &str = "qa721-notes.txt";

/// The boxes, by their key in the child's state.
const BOXES: [&str; 4] = ["chat", "post", "thread", "narrow"];

fn every_box(user: &mut User<'_>) -> SandboxResult {
    for key in BOXES {
        user.open_box(key)?;
        let state_key = state_key(key);
        user.type_text("a tab draft")?;
        user.press("tab")?;
        user.until(&format!("Tab to the tools of the {key} box"), |state| {
            state[state_key]["focused"] == false && state[state_key]["tools_focused"] == true
        })?;
        if key == "chat" {
            user.shot("qa721-chat-tab-ring-light")?;
        }
        user.press("shift+tab")?;
        user.until(&format!("Shift+Tab back in the {key} box"), |state| {
            state[state_key]["focused"] == true && text_of(&state[state_key]) == "a tab draft"
        })?;
        user.press("tab")?;
        let before = user.until(&format!("Tab to the tools of the {key} box again"), |state| {
            state[state_key]["tools_focused"] == true
        })?[state_key]["keys_on"]
            .clone();
        user.press("right")?;
        let second = user.until(&format!("Right to the next tool of the {key} box"), |state| {
            state[state_key]["tools_focused"] == true && state[state_key]["keys_on"] != before
        })?[state_key]["keys_on"]
            .clone();
        user.press("tab")?;
        user.until(&format!("Tab from the tools to Send in the {key} box"), |state| {
            state[state_key]["focused"] == false && state[state_key]["tools_focused"] == false
        })?;
        user.press("shift+tab")?;
        user.until(
            &format!("Shift+Tab from Send to the same tool of the {key} box"),
            |state| state[state_key]["tools_focused"] == true && state[state_key]["keys_on"] == second,
        )?;
        user.press("shift+tab")?;
        user.until(
            &format!("Shift+Tab from the tools to the text of the {key} box"),
            |state| state[state_key]["focused"] == true,
        )?;
        user.ask("clear")?;
    }
    user.ring_in_dark()?;
    user.leave_boxes()
}

impl User<'_> {
    /// The ring in Teams Dark on the chat box's tool that the keys were last on, Teams Light after.
    fn ring_in_dark(&mut self) -> SandboxResult {
        self.open_box("chat")?;
        self.ask("theme-dark")?;
        thread::sleep(SETTLE);
        self.click_text("chat")?;
        self.press("tab")?;
        self.until("Tab leaves the box in Teams Dark", |state| {
            main_box(state)["focused"] == false
        })?;
        self.shot("qa721-chat-tab-ring-dark")?;
        self.press("shift+tab")?;
        self.until("Shift+Tab back in Teams Dark", |state| {
            main_box(state)["focused"] == true
        })?;
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The box `key` open and focused by a click in its text, empty, Teams Light.
    fn open_box(&mut self, key: &str) -> SandboxResult {
        match key {
            "chat" => {
                self.main_chat()?;
            }
            "narrow" => {
                self.main_chat()?;
                self.resize_focused(520)?;
                self.until("the stacked box", |state| main_box(state)["stacked"] == true)?;
                self.click_text("chat")?;
            }
            "post" => {
                self.main_chat()?;
                self.ask("open-channel")?;
                self.until("the channel", |state| !state["post"].is_null())?;
                self.click_text("post")?;
            }
            _ => {
                self.main_chat()?;
                self.ask("open-channel")?;
                self.until("the channel", |state| !state["post"].is_null())?;
                self.ask("open-thread")?;
                self.until("the thread", |state| !state["thread"].is_null())?;
                self.click_text("thread")?;
            }
        }
        Ok(())
    }

    /// A click in the text of the box `key`, in its left part, near its top: in a thread, under the line that says
    /// where the reply goes.
    fn click_text(&mut self, key: &str) -> SandboxResult {
        thread::sleep(SETTLE);
        let state = self.state()?;
        let bounds = state[key]["bounds"].clone();
        let at = |index: usize| bounds[index].as_f64().unwrap_or_default();
        let y = if key == "thread" {
            at(1) + 45.0
        } else {
            at(1) + (at(3) / 2.0).min(20.0)
        };
        #[expect(clippy::cast_possible_truncation, reason = "a point on a 1280x800 screen")]
        let point = ((at(0) + 40.0) as i32, y as i32);
        self.click(point)?;
        self.until(&format!("the {key} box focused"), |state| state[key]["focused"] == true)
            .map(|_| ())
    }

    /// The thread closed, the window wide again.
    fn leave_boxes(&mut self) -> SandboxResult {
        self.ask("close-thread")?;
        self.main_chat().map(|_| ())
    }
}

/// The key of the box in the child's state: the narrow box is the chat's.
fn state_key(key: &str) -> &str {
    if key == "narrow" { "chat" } else { key }
}

/// A file staged in the chat box: Tab goes to its button, Shift+Tab back to the text.
fn staged_file(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    let file = user.sandbox.path(STAGED);
    fs::write(&file, "made-up notes for QA 721\n")?;
    user.ask(&format!("stage-file {}", file.display()))?;
    user.until("the staged file", |state| main_box(state)["attachments"] == 1)?;
    // the file is under the text: a click at the box's top
    user.click_text("chat")?;
    user.press("tab")?;
    user.until("Tab leaves the box for the file", |state| {
        main_box(state)["focused"] == false && main_box(state)["tools_focused"] == false
    })?;
    user.shot("qa721-staged-tab-ring-light")?;
    user.press("shift+tab")?;
    user.until("Shift+Tab back in the box", |state| main_box(state)["focused"] == true)?;
    user.ask("clear")?;
    user.until("the file gone", |state| main_box(state)["attachments"] == 0)?;
    Ok(())
}

// --- the child ---

/// A verb of QA 721: `stage-file <path>` puts the file in the chat box, as a pick in the file chooser does.
pub(super) fn on_verb(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> Option<String> {
    let path = verb.strip_prefix("stage-file ")?;
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return Some("no chat".to_owned());
    };
    let compose = chat.read(cx).compose().clone();
    compose.update(cx, |compose, cx| {
        compose.attach_files(&[PathBuf::from(path)], window, cx);
    });
    Some("ok".to_owned())
}
