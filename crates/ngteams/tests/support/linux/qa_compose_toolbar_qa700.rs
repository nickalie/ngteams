//! QA of task 700, round 1, in the same window and run as the checks of task 584: the AI tools under "+" and the two
//! languages, as a user reaches them.
//!
//! - **"+" rows**: Teams' hidden tools, then Correct, Translate and "Translation language", then "Customize toolbar",
//!   found by the action that each row starts; with "Use AI features" off, only Teams' rows and "Customize toolbar".
//! - **Shown from "+"**: "Customize toolbar" in "+" shows Correct and Translate by the keys; Ctrl+Shift+G and
//!   Ctrl+Shift+Y with the tools on the bar; the right-click list hides them again.
//! - **Settings** > Appearance: the switches of Correct and Translate show and hide the buttons.
//! - **Other boxes**: the notes chat (the chat with yourself), a channel's post and a thread's reply show the tools.
//! - **A narrow window**: the shown tools go into "More options", and they work from there.
//! - **Each font size** from 10 to 24 px: the bar and "+" with the tools shown, in Teams Light and Dark.
//! - **A restart** keeps the shown Translate; **two languages**: Settings > General sets the reading language and the
//!   draft language apart; a received message translates into the one, the draft into the other.
//! - **An old settings file** with only `message_translate_to` gives both languages that value.

use super::*;

use teams_mock::NOTES;
use teams_ui::chat_translate::Mark;
use teams_ui::chat_view::{Body, Item};
use teams_ui::rich_text::Block;

/// QA 700's checks of the first run, after the checks of task 584 and before the hide for the restart.
pub(super) const FIRST: [Check; 7] = [
    ("qa700_plus_rows", plus_rows),
    ("qa700_shown_from_plus", shown_from_plus),
    ("qa700_settings_switches", settings_switches),
    ("qa700_other_boxes", other_boxes),
    ("qa700_narrow_more", narrow_more),
    ("qa700_font_sizes", font_sizes),
    ("qa700_show_for_restart", show_for_restart),
];

/// The first check of the second run, before the checks of task 584 there.
pub(super) const RESTART_FIRST: Check = ("qa700_kept_after_restart", kept_after_restart);

/// The last check of the second run.
pub(super) const RESTART_LAST: Check = ("qa700_two_languages", two_languages);

/// The third run, over an old settings file.
pub(super) const OLD_FILE: [Check; 1] = [("qa700_old_file", old_file)];

/// The language a user read in before task 700, the only one an old settings file names.
const OLD_LANGUAGE: &str = "Spanish";

/// The settings file as an app before task 700 left it: one language, `message_translate_to`, for both. With no
/// earlier run (a filtered run), the file holds only that language.
pub(super) fn write_old_file(file: &Path) -> SandboxResult {
    if !file.exists() {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(file, format!("message_translate_to = \"{OLD_LANGUAGE}\"\n"))?;
        return Ok(());
    }
    let text = fs::read_to_string(file)?;
    let mut old = String::new();
    for line in text.lines() {
        if line.starts_with("draft_translate_to") {
            continue;
        }
        if line.starts_with("message_translate_to") {
            let _ = writeln!(old, "message_translate_to = \"{OLD_LANGUAGE}\"");
        } else {
            let _ = writeln!(old, "{line}");
        }
    }
    ensure(old.contains("message_translate_to"), || {
        format!("the file names no message_translate_to: {text}")
    })?;
    fs::write(file, old)?;
    Ok(())
}

/// "+" with Format hidden: Format, then Correct, Translate and the language, then "Customize toolbar"; the keys find
/// each row by the action that it starts. With AI off: Format, then "Customize toolbar".
fn plus_rows(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.ask("language German")?;
    user.hide(&["format"])?;
    user.reopened_shots("qa700-plus-format-hidden", "chat", |user| {
        user.click_menu(0, "Plus").map(|_| ())
    })?;
    user.type_text("hello teh rows")?;
    // the second row: Correct
    user.click_menu(0, "Plus")?;
    user.press("down")?;
    user.press("down")?;
    user.settled_shot("qa700-plus-second-row")?;
    user.press("enter")?;
    user.until("Correct by the second row", |state| {
        main_box(state)["correction"] == "Proposed"
    })?;
    // the third row: Translate, which drops the proposal
    user.click_menu(0, "Plus")?;
    for _ in 0..3 {
        user.press("down")?;
    }
    user.press("enter")?;
    user.until("Translate by the third row", |state| {
        text_of(main_box(state)) == "hallo teh rows"
    })?;
    // the fourth row: the language menu
    user.click_menu(0, "Plus")?;
    for _ in 0..4 {
        user.press("down")?;
    }
    user.press("enter")?;
    user.until("the language menu by the fourth row", |state| {
        main_box(state)["language_open"] == true
    })?;
    user.press("escape")?;
    user.close_menus("chat")?;
    // the fifth row: Customize toolbar
    user.click_menu(0, "Plus")?;
    for _ in 0..5 {
        user.press("down")?;
    }
    user.press("enter")?;
    user.until("the list by the fifth row", |state| {
        main_box(state)["customize"] == true
    })?;
    user.close_menus("chat")?;
    // AI off: the second row is Customize toolbar
    user.ask("ai off")?;
    user.until("AI off", |state| state["ai"] == false)?;
    user.reopened_shots("qa700-plus-ai-off", "chat", |user| {
        user.click_menu(0, "Plus").map(|_| ())
    })?;
    user.click_menu(0, "Plus")?;
    user.press("down")?;
    user.press("down")?;
    user.press("enter")?;
    user.until("the list by the second row with AI off", |state| {
        main_box(state)["customize"] == true
    })?;
    user.close_menus("chat")?;
    user.ask("ai on")?;
    user.until("AI on", |state| state["ai"] == true)?;
    user.ask("clear")?;
    user.hide(&[])?;
    Ok(())
}

/// "Customize toolbar" in "+" shows Translate, then Correct, by the keys; the keys of both tools work while they
/// show; the right-click list hides both again.
fn shown_from_plus(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.ask("language German")?;
    user.show_ai(&[])?;
    user.click_menu(0, "Plus")?;
    user.press("up")?;
    user.press("enter")?;
    user.until("the list from \"+\"", |state| main_box(state)["customize"] == true)?;
    user.take_row_by_up(1)?;
    user.until("Translate shown from \"+\"", |state| shown_is(state, &["translate"]))?;
    user.press("up")?;
    user.press("enter")?;
    user.until("Correct shown from \"+\"", |state| {
        shown_is(state, &["correct", "translate"])
    })?;
    user.settled_shot("qa700-list-from-plus-both-shown")?;
    user.close_menus("chat")?;
    user.shots_in_both_themes("qa700-bar-both-shown")?;
    // the keys while both tools show
    user.click(MAIN_TEXT)?;
    user.type_text("hello teh shown")?;
    user.tool.hold(true)?;
    user.press("ctrl+shift+g")?;
    user.until("Correct by its key while shown", |state| {
        main_box(state)["correction"] == "Running"
    })?;
    user.settled_shot("qa700-correct-running-shown")?;
    user.press("ctrl+shift+g")?;
    user.until("Correct stopped by its key while shown", |state| {
        main_box(state)["correction"] != "Running"
    })?;
    user.press("ctrl+shift+y")?;
    user.until("Translate by its key while shown", |state| {
        main_box(state)["translation"] == "Running"
    })?;
    user.settled_shot("qa700-translate-running-shown")?;
    user.press("ctrl+shift+y")?;
    user.until("Translate stopped by its key while shown", |state| {
        main_box(state)["translation"] != "Running"
    })?;
    user.tool.hold(false)?;
    user.press("ctrl+shift+g")?;
    user.until("the correction by its key while shown", |state| {
        main_box(state)["correction"] == "Proposed"
    })?;
    user.press("ctrl+shift+y")?;
    user.until("the translation by its key while shown", |state| {
        text_of(main_box(state)) == "hallo teh shown"
    })?;
    // the right-click list: Translate, then Correct, hidden again
    user.right_click(MAIN_SEND)?;
    user.until("the list by a right click", |state| {
        main_box(state)["customize"] == true
    })?;
    user.take_row_by_up(1)?;
    user.until("Translate hidden by the list", |state| shown_is(state, &["correct"]))?;
    user.press("up")?;
    user.press("enter")?;
    user.until("Correct hidden by the list", |state| shown(state).is_empty())?;
    user.close_menus("chat")?;
    user.settled_shot("qa700-both-hidden-again")?;
    user.ask("clear")?;
    Ok(())
}

/// The switches of Correct and Translate at the foot of Settings > Appearance, scrolled to its foot.
const SETTINGS_CORRECT: (i32, i32) = (1135, 667);
const SETTINGS_TRANSLATE: (i32, i32) = (1135, 731);

/// Settings > Appearance: a click on the switch of Correct, then of Translate, shows each; the bar shows them; a
/// second click hides them.
fn settings_switches(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.show_ai(&[])?;
    user.ask("settings appearance")?;
    user.until("the settings", |state| state["main"] == "Settings")?;
    user.input.scroll(800, 450, 10)?;
    thread::sleep(SETTLE);
    user.click(SETTINGS_CORRECT)?;
    user.until("Correct shown by its switch", |state| shown_is(state, &["correct"]))?;
    user.click(SETTINGS_TRANSLATE)?;
    user.until("Translate shown by its switch", |state| {
        shown_is(state, &["correct", "translate"])
    })?;
    user.shots_in_both_themes("qa700-settings-ai-on")?;
    user.main_chat()?;
    user.settled_shot("qa700-settings-then-bar")?;
    user.ask("settings appearance")?;
    user.until("the settings", |state| state["main"] == "Settings")?;
    user.input.scroll(800, 450, 10)?;
    thread::sleep(SETTLE);
    user.click(SETTINGS_CORRECT)?;
    user.click(SETTINGS_TRANSLATE)?;
    user.until("both hidden by their switches", |state| shown(state).is_empty())?;
    user.settled_shot("qa700-settings-ai-off")?;
    Ok(())
}

/// The notes chat: the tools show, and their keys work there; a channel's post and a thread's reply show them, and
/// their "+" has no AI rows.
fn other_boxes(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.ask("language German")?;
    user.show_ai(&["correct", "translate"])?;
    user.ask(&format!("open {NOTES}"))?;
    user.until("the notes chat", |state| {
        state["main"] == format!("chat:{NOTES}") && !main_box(state).is_null()
    })?;
    thread::sleep(SETTLE);
    user.click(MAIN_TEXT)?;
    user.until("the notes box focused", |state| main_box(state)["focused"] == true)?;
    user.shots_in_both_themes("qa700-notes-shown")?;
    user.type_text("hello teh notes")?;
    user.click(MAIN_CORRECT)?;
    user.until("Correct by its button in the notes", |state| {
        main_box(state)["correction"] == "Proposed"
    })?;
    user.press("ctrl+shift+y")?;
    user.until("Translate by its key in the notes", |state| {
        text_of(main_box(state)) == "hallo teh notes"
    })?;
    user.click(MAIN_CHEVRON)?;
    user.until("the language menu by the chevron in the notes", |state| {
        main_box(state)["language_open"] == true
    })?;
    user.settled_shot("qa700-notes-language")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.ask("clear")?;
    // a channel's post and a thread's reply
    user.ask("open-channel")?;
    user.until("the post box shows the tools on its bar alone", |state| {
        shows_ai_on_bar(state, "post")
    })?;
    user.ask("open-thread")?;
    // with Attach and Loop of task 706, the thread's reply box is stacked too: its AI tools are in More options
    user.until(
        "the thread box shows the tools on its bar or in More options alone",
        |state| shows_ai_alone(state, "thread"),
    )?;
    user.shots_in_both_themes("qa700-channel-thread-shown")?;
    for key in ["post", "thread"] {
        user.ask(&format!("box-menu {key} plus"))?;
        user.until(&format!("+ of the {key} box"), |state| state[key]["menu"] == "Plus")?;
        user.settled_shot(&format!("qa700-{key}-plus-shown"))?;
        user.press("escape")?;
        user.close_menus(key)?;
    }
    user.ask("close-thread")?;
    user.main_chat()?;
    user.show_ai(&[])?;
    Ok(())
}

/// Whether the box `key` shows Correct and Translate at the end of its bar, and its "+" holds only "Customize toolbar".
/// The post box is checked before the thread opens: beside the thread it is stacked, and its AI tools go to More
/// options. A box keeps the width it was last drawn at, so a check waits for the box to be drawn again.
fn shows_ai_on_bar(state: &Value, key: &str) -> bool {
    state[key]["row_tools"]
        .as_array()
        .is_some_and(|row| row.ends_with(&[json!("compose-correct"), json!("compose-translate")]))
        && state[key]["plus_rows"] == json!(["compose-customize"])
}

/// Whether the box `key` shows Correct and Translate, on its bar or, when it is stacked, in More options, and its "+"
/// holds only "Customize toolbar".
fn shows_ai_alone(state: &Value, key: &str) -> bool {
    let boxed = &state[key];
    if boxed["stacked"] != true {
        return shows_ai_on_bar(state, key);
    }
    let ai = [json!("compose-correct"), json!("compose-translate")];
    boxed["more_rows"]
        .as_array()
        .is_some_and(|rows| ai.iter().all(|tool| rows.contains(tool)))
        && boxed["plus_rows"] == json!(["compose-customize"])
}

/// A 520 px window with both tools shown: "More options" holds Correct, Translate and the language, and each works
/// from there; at 24 px too.
fn narrow_more(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.ask("language German")?;
    user.show_ai(&["correct", "translate"])?;
    user.resize_focused(520)?;
    let mut faults = Vec::new();
    for font in [14, 24] {
        user.ask(&format!("font {font}"))?;
        thread::sleep(SETTLE);
        let state = user.state()?;
        if main_box(&state)["stacked"] != true {
            faults.push(format!("not stacked at 520 px, font {font}: {}", main_box(&state)));
        }
        user.shots_in_both_themes(&format!("qa700-narrow-font-{font}"))?;
        user.reopened_shots(&format!("qa700-narrow-font-{font}-more"), "chat", |user| {
            user.ask("box-menu chat more")?;
            user.until("More options", |state| main_box(state)["menu"] == "More")
                .map(|_| ())
        })?;
    }
    user.ask("font 14")?;
    thread::sleep(SETTLE);
    user.ask("clear")?;
    user.click_narrow_text()?;
    user.type_text("hello teh narrow")?;
    // More options holds "+"'s rows at this width: from its foot, Customize toolbar, the language, Translate, Correct
    user.open_narrow_more()?;
    for _ in 0..4 {
        user.press("up")?;
    }
    user.settled_shot("qa700-narrow-more-correct-row")?;
    user.press("enter")?;
    if user
        .until("Correct from More options", |state| {
            main_box(state)["correction"] == "Proposed"
        })
        .is_err()
    {
        faults.push("Correct's row in More options did not correct".to_owned());
    }
    user.close_menus("chat")?;
    user.open_narrow_more()?;
    user.press("up")?;
    user.press("up")?;
    user.press("enter")?;
    if user
        .until("the language from More options", |state| {
            main_box(state)["language_open"] == true
        })
        .is_err()
    {
        faults.push("the language row of More options did not open the language menu".to_owned());
    }
    user.settled_shot("qa700-narrow-more-language")?;
    user.close_menus("chat")?;
    user.open_narrow_more()?;
    for _ in 0..3 {
        user.press("up")?;
    }
    user.press("enter")?;
    if user
        .until("Translate from More options", |state| {
            text_of(main_box(state)) == "hallo teh narrow"
        })
        .is_err()
    {
        faults.push("Translate's row in More options did not translate".to_owned());
    }
    user.close_menus("chat")?;
    user.ask("clear")?;
    user.resize_focused(1280)?;
    user.show_ai(&[])?;
    ensure(faults.is_empty(), || faults.join("; "))
}

/// Each font size from 10 to 24 px, both tools shown: the bar and "+" in Teams Light and Dark; the box stays on one
/// line at 1280 px.
fn font_sizes(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.show_ai(&["correct", "translate"])?;
    user.type_text("a draft at each size")?;
    let mut faults = Vec::new();
    for font in (10..=24).step_by(2) {
        user.ask(&format!("font {font}"))?;
        thread::sleep(SETTLE);
        let state = user.state()?;
        if main_box(&state)["stacked"] != false {
            faults.push(format!("stacked at 1280 px, font {font}"));
        }
        user.shots_in_both_themes(&format!("qa700-font-{font}"))?;
        user.reopened_shots(&format!("qa700-font-{font}-plus"), "chat", |user| {
            user.ask("box-menu chat plus")?;
            user.until("\"+\"", |state| main_box(state)["menu"] == "Plus")
                .map(|_| ())
        })?;
    }
    user.ask("font 14")?;
    user.ask("clear")?;
    user.show_ai(&[])?;
    ensure(faults.is_empty(), || faults.join("; "))
}

/// Translate shown by the right-click list, for the restart.
fn show_for_restart(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.show_ai(&[])?;
    user.right_click(MAIN_SEND)?;
    user.until("the list", |state| main_box(state)["customize"] == true)?;
    user.take_row_by_up(1)?;
    user.until("Translate shown", |state| shown_is(state, &["translate"]))?;
    user.close_menus("chat")?;
    let text = user.settings_text();
    ensure(text.contains("compose_shown"), || {
        format!("the shown tool is not in the file: {text}")
    })
}

/// The second run: Translate still shows, and its chevron opens the language menu; the list hides it for the checks
/// of task 584.
fn kept_after_restart(user: &mut User<'_>) -> SandboxResult {
    let state = user.main_chat()?;
    ensure(shown_is(&state, &["translate"]), || {
        format!("the shown Translate is not kept: {state}")
    })?;
    user.shots_in_both_themes("qa700-after-restart-translate")?;
    user.click(MAIN_CHEVRON)?;
    user.until("the language menu by the chevron after the restart", |state| {
        main_box(state)["language_open"] == true
    })?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.right_click(MAIN_SEND)?;
    user.until("the list", |state| main_box(state)["customize"] == true)?;
    user.take_row_by_up(1)?;
    user.until("Translate hidden", |state| shown(state).is_empty())?;
    user.close_menus("chat")?;
    Ok(())
}

/// The language buttons of Settings > General, scrolled to its foot: the reading one at the top, the draft one in
/// "Message translation".
const GENERAL_READING: (i32, i32) = (1042, 62);
const GENERAL_DRAFT: (i32, i32) = (1042, 553);

/// Settings > General: the reading language Russian and the draft language English by their menus; b's message
/// translates into Russian, the draft into English.
fn two_languages(user: &mut User<'_>) -> SandboxResult {
    user.main_chat()?;
    user.ask("settings general")?;
    user.until("Settings > General", |state| state["main"] == "Settings")?;
    user.input.scroll(800, 450, 20)?;
    thread::sleep(SETTLE);
    user.shots_in_both_themes("qa700-general-foot")?;
    user.click(GENERAL_DRAFT)?;
    thread::sleep(SETTLE);
    user.settled_shot("qa700-general-draft-menu")?;
    user.type_text("English")?;
    user.press("enter")?;
    user.until("the draft language English", |state| state["language"] == "English")?;
    user.press("escape")?;
    user.click(GENERAL_READING)?;
    thread::sleep(SETTLE);
    user.settled_shot("qa700-general-reading-menu")?;
    user.type_text("Russian")?;
    user.press("enter")?;
    let state = user.until("the reading language Russian", |state| state["reading"] == "Russian")?;
    ensure(state["language"] == "English", || {
        format!("the reading language changed the draft language: {state}")
    })?;
    user.press("escape")?;
    user.shots_in_both_themes("qa700-general-two-languages")?;
    let text = user.settings_text();
    ensure(
        text.contains("message_translate_to = \"Russian\"") && text.contains("draft_translate_to = \"English\""),
        || format!("the file: {text}"),
    )?;
    // b's message: T on it, selected by the keys
    user.main_chat()?;
    // a word of its own: the marks start again after the restart
    let mark = "zq700evening".to_owned();
    let words = format!("Good evening {mark} team");
    user.mock
        .post_as(LAB_B, AB_CHAT, &format!("<p>{words}</p>"))
        .ok_or("the mock refused b's post")?;
    let state = user.until("b's message", |state| !row_with(state, &mark).is_null())?;
    let id = row_with(&state, &mark)["id"].as_str().unwrap_or_default().to_owned();
    user.press("escape")?;
    thread::sleep(STEP);
    let mut selected = false;
    for _ in 0..10 {
        user.press("up")?;
        if user.state()?["selected"] == id.as_str() {
            selected = true;
            break;
        }
    }
    ensure(selected, || format!("Up never selected b's message {id}"))?;
    user.press("t")?;
    let state = user.until("b's message translated", |state| {
        row_with(state, &mark)["mark"] == "translated"
    })?;
    let shown_text = row_with(&state, &mark)["text"].as_str().unwrap_or_default().to_owned();
    user.shots_in_both_themes("qa700-message-in-russian")?;
    // the made-up tool's answer may break the line after its language tag
    let shown_words = shown_text.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure(shown_words == format!("[Russian] {words}"), || {
        format!("b's message is not in the reading language: {shown_text:?}")
    })?;
    // the draft: Translate in the box
    user.click(MAIN_TEXT)?;
    user.type_text("hello the draft")?;
    user.press("ctrl+shift+y")?;
    let state = user.until("the draft translated", |state| {
        main_box(state)["translation"] != "Running" && text_of(main_box(state)) != "hello the draft"
    })?;
    user.shots_in_both_themes("qa700-draft-in-english")?;
    let draft = text_of(main_box(&state));
    ensure(draft == "[English] hello the draft", || {
        format!("the draft is not in the draft language: {draft:?}")
    })?;
    user.ask("clear")?;
    Ok(())
}

/// The third run over an old file: both languages are the one language that it named.
fn old_file(user: &mut User<'_>) -> SandboxResult {
    let state = user.main_chat()?;
    ensure(
        state["reading"] == OLD_LANGUAGE && state["language"] == OLD_LANGUAGE,
        || format!("the old language is not both languages: {state}"),
    )?;
    user.click_menu(0, "Plus")?;
    user.settled_shot("qa700-old-file-plus")?;
    user.press("escape")?;
    user.close_menus("chat")?;
    user.ask("settings general")?;
    user.until("Settings > General", |state| state["main"] == "Settings")?;
    user.input.scroll(800, 450, 20)?;
    thread::sleep(SETTLE);
    user.shots_in_both_themes("qa700-old-file-general")?;
    let text = user.settings_text();
    ensure(
        text.contains(&format!("draft_translate_to = \"{OLD_LANGUAGE}\"")),
        || format!("the draft language is not saved: {text}"),
    )
}

/// The message row of the open chat whose words hold `mark`, as the child tells it.
fn row_with<'s>(state: &'s Value, mark: &str) -> &'s Value {
    state["rows"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["text"].as_str().is_some_and(|text| text.contains(mark)))
        })
        .unwrap_or(&Value::Null)
}

/// "More options" on the stacked row of the main box at 520 px and font 14: the third tool.
const NARROW_MORE: (i32, i32) = (396, 749);

impl User<'_> {
    /// Opens "More options" of the narrow main box by a click on its button.
    fn open_narrow_more(&mut self) -> SandboxResult {
        self.click(NARROW_MORE)?;
        self.until("More options by its button", |state| main_box(state)["menu"] == "More")?;
        self.park()
    }

    /// A click in the text of the main box at 520 px, where the stacked box has its text over its row.
    fn click_narrow_text(&mut self) -> SandboxResult {
        let state = self.state()?;
        let bounds = main_box(&state)["bounds"].clone();
        let at = |index: usize| bounds[index].as_f64().unwrap_or_default();
        #[expect(clippy::cast_possible_truncation, reason = "a point on a 1280x800 screen")]
        let point = ((at(0) + at(2) / 2.0) as i32, (at(1) + 20.0) as i32);
        self.click(point)?;
        self.until("the narrow box focused", |state| main_box(state)["focused"] == true)
            .map(|_| ())
    }
}

// --- the child ---

/// A verb of QA 700: `settings general`.
pub(super) fn on_verb(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> Option<String> {
    (verb == "settings general").then(|| {
        shell.update(cx, |shell, cx| shell.open_settings_page(Page::General, window, cx));
        "ok".to_owned()
    })
}

/// The open chat's message rows (each its id, its words as shown and its translation mark) and the selected one.
pub(super) fn chat_rows(shell: &Shell, cx: &App) -> (Value, Value) {
    let Some(chat) = shell.chat().filter(|_| matches!(shell.main(), Main::Chat(_))) else {
        return (Value::Null, Value::Null);
    };
    let chat = chat.read(cx);
    let rows: Vec<Value> = chat
        .items()
        .iter()
        .filter_map(|item| {
            let Item::Message(message) = item else {
                return None;
            };
            let text = match &message.body {
                Body::Text(rendered) => rendered
                    .0
                    .blocks
                    .iter()
                    .flat_map(Block::plain_lines)
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            let mark = match &message.translation {
                None => "none",
                Some(Mark::Translated { .. }) => "translated",
                Some(Mark::Original { .. }) => "original",
                Some(Mark::Waiting) => "waiting",
                Some(Mark::Failed) => "failed",
                Some(Mark::Stopped { .. }) => "stopped",
            };
            Some(json!({"id": message.id, "text": text.trim(), "mark": mark}))
        })
        .collect();
    (Value::Array(rows), json!(chat.selected()))
}
