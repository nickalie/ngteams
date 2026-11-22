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
//!
//! QA round 1 adds:
//!
//! - **The round trip**: in every box, Tab goes three stops on and Shift+Tab comes back over the same stops, the
//!   draft kept; Shift+Tab from the text leaves the box and Tab comes back.
//! - **The message list**: Shift+Tab from the chat's text goes to the message list, as in Teams.
//! - **Expanded**: Format opens the format bar. Shift+Tab from the text goes to Discard, then to the bar (one stop,
//!   arrows in it), and Tab comes back the same way.
//! - **The row's keys**: Right, Left, Home and End move in the tools and do not wrap; Up and Down do nothing; the
//!   row keeps the last tool on both sides.
//! - **Menus**: Enter on "+" and on Emoji opens its menu with the keys in it; Escape gives them back to that tool.
//! - **A staged file** walked to the tools and back, its button's ring in Teams Light and Teams Dark.
//! - **Other editors**: edit mode, New message's "To" and its box, and the search box keep their Tab order.

use std::path::PathBuf;
use std::{fs, thread};

use gpui::{App, Entity, Window};
use serde_json::Value;

use super::{AB_CHAT, Check, SETTLE, STEP, SandboxResult, Shell, User, ensure, hidden, main_box, shown, text_of};

/// QA 721's checks of the first run.
pub(super) const FIRST: [Check; 8] = [
    ("qa721_every_box", every_box),
    ("qa721_staged_file", staged_file),
    ("qa721_round_trip", round_trip),
    ("qa721_expanded", expanded),
    ("qa721_row_keys", |user| on_default_bar(user, row_keys)),
    ("qa721_menu_keys", |user| on_default_bar(user, menu_keys)),
    ("qa721_other_editors", other_editors),
    ("qa721_shift_tab_to_list", shift_tab_to_list),
];

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
    user.ask("theme-dark")?;
    thread::sleep(SETTLE);
    user.shot("qa721-staged-tab-ring-dark")?;
    user.ask("theme-light")?;
    thread::sleep(SETTLE);
    user.press("shift+tab")?;
    user.until("Shift+Tab back in the box", |state| main_box(state)["focused"] == true)?;
    // all the file's buttons come before the tools, and Shift+Tab goes back over the same ones
    let text = keys_of(&user.state()?);
    let mut stops = Vec::new();
    for _ in 0..6 {
        let state = user.step("tab")?;
        if main_box(&state)["tools_focused"] == true {
            break;
        }
        stops.push(keys_of(&state));
    }
    ensure(main_box(&user.state()?)["tools_focused"] == true, || {
        format!("Tab never came to the tools after the file: {stops:?}")
    })?;
    ensure(!stops.is_empty(), || "no file button before the tools".to_owned())?;
    let mut back = Vec::new();
    for _ in 0..stops.len() {
        back.push(keys_of(&user.step("shift+tab")?));
    }
    back.reverse();
    ensure(back == stops, || format!("Shift+Tab went {back:?}, Tab went {stops:?}"))?;
    let state = user.step("shift+tab")?;
    ensure(keys_of(&state) == text && main_box(&state)["focused"] == true, || {
        format!("Shift+Tab from the file never came to the text: {state}")
    })?;
    user.ask("clear")?;
    user.until("the file gone", |state| main_box(state)["attachments"] == 0)?;
    Ok(())
}

/// The focus handle that has the keys, as the child names it.
fn keys_of(state: &Value) -> String {
    state["keys_on"].as_str().unwrap_or("none").to_owned()
}

impl User<'_> {
    /// `spec` pressed, then the state when the app has seen it.
    fn step(&mut self, spec: &str) -> SandboxResult<Value> {
        self.press(spec)?;
        thread::sleep(STEP);
        self.state()
    }

    /// `count` presses of `spec`, the keys after each.
    fn walk(&mut self, spec: &str, count: usize) -> SandboxResult<Vec<String>> {
        let mut stops = Vec::new();
        for _ in 0..count {
            stops.push(keys_of(&self.step(spec)?));
        }
        Ok(stops)
    }

    /// The same screen in Teams Light and Teams Dark, the keys kept; Teams Light after.
    fn ring_shots(&mut self, name: &str) -> SandboxResult {
        self.park()?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-light"))?;
        self.ask("theme-dark")?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-dark"))?;
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

/// In every box: Tab three stops on, Shift+Tab back over the same three, the draft kept; Shift+Tab from the chat's
/// text goes to the message list and Tab comes back.
fn round_trip(user: &mut User<'_>) -> SandboxResult {
    for key in BOXES {
        user.open_box(key)?;
        let state_key = state_key(key);
        user.type_text("a round draft")?;
        let text = keys_of(&user.until(&format!("the draft in the {key} box"), |state| {
            text_of(&state[state_key]) == "a round draft"
        })?);
        if key == "chat" {
            user.ring_shots("qa721-r1-chat-text-ring")?;
        }
        let ahead = user.walk("tab", 3)?;
        let mut back = user.walk("shift+tab", 3)?;
        back.reverse();
        let state = user.state()?;
        let expected = [text.clone(), ahead[0].clone(), ahead[1].clone()];
        ensure(back == expected, || {
            format!("{key}: Tab went {ahead:?} from {text}, Shift+Tab came back over {back:?}")
        })?;
        ensure(
            state[state_key]["focused"] == true && text_of(&state[state_key]) == "a round draft",
            || format!("{key}: the text or the draft is lost after the round trip: {state}"),
        )?;
        let out = user.step("shift+tab")?;
        ensure(
            out[state_key]["focused"] == false && out[state_key]["tools_focused"] == false,
            || format!("{key}: Shift+Tab from the text stays in the box: {out}"),
        )?;
        let again = user.step("tab")?;
        ensure(
            again[state_key]["focused"] == true && text_of(&again[state_key]) == "a round draft",
            || format!("{key}: Tab from before the text does not come back to it: {again}"),
        )?;
        user.ask("clear")?;
    }
    user.leave_boxes()
}

/// The chat box's tools: Tab to them, then Home and Enter on Format. Returns the state after.
fn press_format(user: &mut User<'_>) -> SandboxResult<Value> {
    user.press("tab")?;
    user.until("Tab to the tools", |state| main_box(state)["tools_focused"] == true)?;
    user.press("home")?;
    user.step("enter")
}

/// The expanded box: Format by the keys opens the format bar; Shift+Tab from the text goes to Discard and then to the
/// bar, one stop with arrows in it; Tab comes back the same way; the bar keeps its last tool.
fn expanded(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    press_format(user)?;
    let state = user.until("the box expanded by Format", |state| {
        main_box(state)["expanded"] == true
    })?;
    if main_box(&state)["focused"] != true {
        user.press("shift+tab")?;
        user.until("the text after Format", |state| main_box(state)["focused"] == true)?;
    }
    user.type_text("an expanded draft")?;
    let text = keys_of(&user.until("the draft in the expanded box", |state| {
        text_of(main_box(state)) == "an expanded draft"
    })?);
    let discard = user.step("shift+tab")?;
    ensure(
        main_box(&discard)["focused"] == false && main_box(&discard)["tools_focused"] == false,
        || format!("Shift+Tab from the expanded text is not on Discard: {discard}"),
    )?;
    let discard = keys_of(&discard);
    user.ring_shots("qa721-r1-expanded-discard-ring")?;
    let bold = keys_of(&user.step("shift+tab")?);
    ensure(bold != discard && bold != text, || {
        format!("Shift+Tab from Discard is not on the format bar: {bold}")
    })?;
    user.ring_shots("qa721-r1-expanded-bar-ring")?;
    let italic = keys_of(&user.step("right")?);
    ensure(italic != bold, || "Right does not move in the format bar".to_owned())?;
    let left = keys_of(&user.step("left")?);
    ensure(left == bold, || {
        format!("Left from the bar's second tool is not on its first: {left}")
    })?;
    user.press("right")?;
    let ahead = user.walk("tab", 2)?;
    ensure(ahead == [discard.clone(), text.clone()], || {
        format!("Tab from the bar went {ahead:?}, not Discard {discard} then the text {text}")
    })?;
    let behind = user.walk("shift+tab", 2)?;
    ensure(behind == [discard.clone(), italic.clone()], || {
        format!("Shift+Tab from the text went {behind:?}: the bar does not keep its last tool {italic}")
    })?;
    user.walk("tab", 2)?;
    let after_text = user.walk("tab", 2)?;
    let mut back = user.walk("shift+tab", 2)?;
    back.reverse();
    ensure(back == [text.clone(), after_text[0].clone()], || {
        format!("expanded: Tab went {after_text:?}, Shift+Tab came back over {back:?}")
    })?;
    let state = user.state()?;
    ensure(
        main_box(&state)["focused"] == true && text_of(main_box(&state)) == "an expanded draft",
        || format!("the expanded draft is lost: {state}"),
    )?;
    user.press("ctrl+a")?;
    user.press("backspace")?;
    user.until("the expanded box empty", |state| text_of(main_box(state)).is_empty())?;
    press_format(user)?;
    user.until("the box back to one line", |state| main_box(state)["expanded"] == false)?;
    user.main_chat().map(|_| ())
}

/// Shift+Tab from the chat's text goes to the message list, as in Teams; Tab comes back to the text.
fn shift_tab_to_list(user: &mut User<'_>) -> SandboxResult {
    for key in ["chat", "narrow"] {
        user.open_box(key)?;
        let out = user.step("shift+tab")?;
        if out["list_focused"] == true {
            user.ring_shots(&format!("qa721-{key}-list-ring"))?;
        }
        let back = user.step("tab")?;
        let refocused = main_box(&back)["focused"] == true;
        if !refocused {
            user.main_chat()?;
        }
        ensure(out["list_focused"] == true, || {
            format!("{key}: Shift+Tab from the text is not on the message list: {out}")
        })?;
        ensure(refocused, || {
            format!("{key}: Tab from the list does not come back to the text: {back}")
        })?;
    }
    user.main_chat().map(|_| ())
}

/// `check` on Teams' default bar (nothing hidden, no AI tool shown); the buttons that earlier checks hid or showed
/// for the restart come back after it.
fn on_default_bar(user: &mut User<'_>, check: fn(&mut User<'_>) -> SandboxResult) -> SandboxResult {
    let state = user.state()?;
    let (was_hidden, was_shown) = (hidden(&state), shown(&state));
    user.hide(&[])?;
    user.show_ai(&[])?;
    let checked = check(user);
    let was_hidden: Vec<&str> = was_hidden.iter().map(String::as_str).collect();
    let was_shown: Vec<&str> = was_shown.iter().map(String::as_str).collect();
    user.hide(&was_hidden)?;
    user.show_ai(&was_shown)?;
    checked
}

/// The tools: Right, Left, Home and End move and do not wrap; Up and Down do nothing; the row keeps the last tool
/// from both sides.
fn row_keys(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.type_text("a row draft")?;
    user.press("tab")?;
    user.until("Tab to the tools", |state| main_box(state)["tools_focused"] == true)?;
    let first = keys_of(&user.step("home")?);
    user.ring_shots("qa721-r1-tools-ring")?;
    let mut tools = vec![first.clone()];
    for _ in 0..8 {
        let on = keys_of(&user.step("right")?);
        if tools.last() == Some(&on) {
            break;
        }
        tools.push(on);
    }
    // Loop is shown disabled (not in this app yet): the arrows pass it
    ensure(tools.len() == 4, || {
        format!(
            "the row has {} tools, not Format, Emoji, Attach and +: {tools:?}",
            tools.len()
        )
    })?;
    let last = tools.last().cloned().unwrap_or_default();
    let past_end = keys_of(&user.step("right")?);
    ensure(past_end == last, || "Right on the last tool wraps".to_owned())?;
    let home = keys_of(&user.step("home")?);
    ensure(home == first, || "Home is not on the first tool".to_owned())?;
    let before_first = keys_of(&user.step("left")?);
    ensure(before_first == first, || "Left on the first tool wraps".to_owned())?;
    for key in ["up", "down"] {
        let state = user.step(key)?;
        ensure(keys_of(&state) == first && main_box(&state)["menu"].is_null(), || {
            format!("{key} on Format does something: {state}")
        })?;
    }
    let end = keys_of(&user.step("end")?);
    ensure(end == last, || "End is not on the last tool".to_owned())?;
    let kept = keys_of(&user.step("left")?);
    ensure(kept == tools[2], || "Left from + is not on Attach".to_owned())?;
    user.press("tab")?;
    let back = keys_of(&user.step("shift+tab")?);
    ensure(back == kept, || {
        "Shift+Tab from Send does not come back to the last tool".to_owned()
    })?;
    user.press("shift+tab")?;
    user.until("the text", |state| main_box(state)["focused"] == true)?;
    let ahead = keys_of(&user.step("tab")?);
    ensure(ahead == kept, || {
        "Tab from the text does not come back to the last tool".to_owned()
    })?;
    user.press("shift+tab")?;
    let state = user.until("the text", |state| main_box(state)["focused"] == true)?;
    ensure(text_of(main_box(&state)) == "a row draft", || {
        format!("the draft is lost: {state}")
    })?;
    user.ask("clear")?;
    Ok(())
}

/// Enter on "+" and on Emoji opens the menu with the keys in it; Escape gives them back to that tool.
fn menu_keys(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.press("tab")?;
    user.until("Tab to the tools", |state| main_box(state)["tools_focused"] == true)?;
    let plus = keys_of(&user.step("end")?);
    user.press("enter")?;
    let open = user.until("+ open by Enter", |state| main_box(state)["menu"] == "Plus")?;
    ensure(keys_of(&open) != plus && main_box(&open)["focused"] == false, || {
        format!("the keys are not in + after Enter: {open}")
    })?;
    let down = user.step("down")?;
    ensure(
        main_box(&down)["menu"] == "Plus" && main_box(&down)["focused"] == false,
        || format!("Down in + leaves it: {down}"),
    )?;
    user.settled_shot("qa721-r1-plus-by-enter")?;
    user.press("escape")?;
    let closed = user.until("+ closed by Escape", |state| main_box(state)["menu"].is_null())?;
    ensure(keys_of(&closed) == plus, || {
        format!("Escape from + does not give the keys back to +: {closed}")
    })?;
    user.ring_shots("qa721-r1-plus-after-escape-ring")?;
    // the keys act in +: Up selects its last row, Customize toolbar, and Enter opens it
    user.press("enter")?;
    user.until("+ open by Enter again", |state| main_box(state)["menu"] == "Plus")?;
    user.press("up")?;
    user.press("enter")?;
    user.until("Customize toolbar by the keys of +", |state| {
        main_box(state)["customize"] == true
    })?;
    user.settled_shot("qa721-r1-customize-by-keys")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.press("tab")?;
    user.until("Tab to the tools after the list", |state| {
        main_box(state)["tools_focused"] == true
    })?;
    user.press("home")?;
    let emoji = keys_of(&user.step("right")?);
    user.press("enter")?;
    let open = user.until("Emoji open by Enter", |state| main_box(state)["emoji_open"] == true)?;
    ensure(keys_of(&open) != emoji && main_box(&open)["focused"] == false, || {
        format!("the keys are not in Emoji after Enter: {open}")
    })?;
    user.settled_shot("qa721-r1-emoji-by-enter")?;
    user.press("escape")?;
    let closed = user.until("Emoji closed by Escape", |state| main_box(state)["emoji_open"] == false)?;
    ensure(keys_of(&closed) == emoji, || {
        format!("Escape from Emoji does not give the keys back to Emoji: {closed}")
    })?;
    user.press("shift+tab")?;
    user.until("the text", |state| main_box(state)["focused"] == true)?;
    Ok(())
}

/// Edit mode, New message's "To" and its box, and the search box keep their Tab order.
fn other_editors(user: &mut User<'_>) -> SandboxResult {
    edit_mode(user)?;
    new_message(user)?;
    search_box(user)
}

/// Edit of a message: Tab to the tools and Shift+Tab back keep the edit and its text.
fn edit_mode(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    let mark = user.mark();
    user.type_text(&format!("qa721 edit {mark}"))?;
    user.press("enter")?;
    user.wait_sent("POST", &mark)?;
    user.until("the box empty", |state| text_of(main_box(state)).is_empty())?;
    thread::sleep(SETTLE);
    user.press("up")?;
    let edit = user.until("the edit", |state| main_box(state)["mode"] == "edit")?;
    let edited = text_of(main_box(&edit));
    user.press("tab")?;
    user.until("Tab to the tools in edit mode", |state| {
        main_box(state)["tools_focused"] == true
    })?;
    user.press("shift+tab")?;
    let state = user.until("Shift+Tab back to the edit", |state| main_box(state)["focused"] == true)?;
    ensure(
        main_box(&state)["mode"] == "edit" && text_of(main_box(&state)) == edited,
        || format!("the edit or its text is lost: {state}"),
    )?;
    user.press("escape")?;
    user.until("the edit ended", |state| main_box(state)["mode"] == "new")?;
    Ok(())
}

/// New message: Tab from "To" comes to the box, and Shift+Tab goes back over the same stops to "To".
fn new_message(user: &mut User<'_>) -> SandboxResult {
    user.press("ctrl+n")?;
    let start = user.until("New message", |state| {
        state["main"] == "NewChat" && !state["new_chat"].is_null()
    })?;
    thread::sleep(SETTLE);
    let to = keys_of(&user.state()?);
    ensure(start["new_chat"]["focused"] == false, || {
        format!("New message opens in its box: {start}")
    })?;
    let mut stops = Vec::new();
    for _ in 0..6 {
        let state = user.step("tab")?;
        stops.push(keys_of(&state));
        if state["new_chat"]["focused"] == true {
            break;
        }
    }
    ensure(user.state()?["new_chat"]["focused"] == true, || {
        format!("Tab from To never came to the box: {stops:?}")
    })?;
    user.ring_shots("qa721-r1-new-message-box-ring")?;
    let mut back = user.walk("shift+tab", stops.len())?;
    back.reverse();
    let mut expected = vec![to];
    expected.extend(stops.iter().take(stops.len() - 1).cloned());
    ensure(back == expected, || {
        format!("New message: Tab went {stops:?}, Shift+Tab came back over {back:?}")
    })?;
    user.press("escape")?;
    user.ask(&format!("open {AB_CHAT}"))?;
    Ok(())
}

/// The search box: Tab goes on from it and Shift+Tab comes back to it.
fn search_box(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    let search = keys_of(&user.step("ctrl+e")?);
    ensure(main_box(&user.state()?)["focused"] == false, || {
        "Ctrl+E leaves the keys in the box".to_owned()
    })?;
    let on = keys_of(&user.step("tab")?);
    ensure(on != search, || "Tab does not leave the search box".to_owned())?;
    let back = keys_of(&user.step("shift+tab")?);
    ensure(back == search, || {
        format!("Shift+Tab does not come back to the search box: {back}")
    })?;
    user.press("escape")?;
    user.main_chat()?;
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
