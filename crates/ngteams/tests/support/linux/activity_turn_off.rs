//! An app item's "Turn off <app>" (#27) as a user meets it: the red row after Notification settings in an app item's
//! menu (none on a person's item, none in the read-only mode), the success card with Undo and Dismiss, Undo putting the
//! app's earlier preferences back on the mock, a refused write showing the error and no success, Ctrl+Alt+Shift+O on
//! the selected item, a banner for a new item of the app while it is on (its click opening the item, also in a window
//! closed into the tray and opened again by the click), one again after Undo, and none for a later item of the app
//! turned off.
//!
//! The mock starts with a's `userGraphSettings` holding a made-up preference for the app (an unknown key among it)
//! and one for another app; the child answers `prefs` with what the mock holds and every `PUT` it got.

use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use ui_sandbox::input::{Modifier, keys};
use ui_sandbox::{SandboxResult, wait_until};

use super::{AB_CHAT, ABC_CHAT, Qa, WAIT, ensure, first, items, text};

/// The app of the feed's app item (Workflows' note), as the mock's catalogue has it.
pub(super) const APP: &str = "00000000-0000-4000-8000-00000000a002";
/// Another app the account has a preference for.
const OTHER_APP: &str = "00000000-0000-4000-8000-00000000a0ff";
/// The activity types the mock's definition of [`APP`] declares.
const APP_TYPES: [&str; 2] = ["messageNotify", "customNotification"];
/// What the success card says.
const TURNED_OFF: &str = "Successfully turned-off setting";
/// The item menu's "Turn off <app>", below Notification settings.
const ITEM_MENU_TURN_OFF: (u32, u32) = (337, 323);
/// Where that row is drawn (left, top, right, bottom): its red words and icon.
const TURN_OFF_BAND: (u32, u32, u32, u32) = (248, 311, 438, 337);
/// The success card's Undo and its Dismiss (×), at the window's bottom left.
const CARD_UNDO: (u32, u32) = (235, 758);
const CARD_DISMISS: (u32, u32) = (350, 727);
/// How long the views get after a key or click that should send nothing, before the mock is asked.
const QUIET: Duration = Duration::from_millis(2500);
/// Longer than the notifier gathers messages before it shows them.
const BANNER_WAIT: Duration = Duration::from_secs(5);

/// a's app preferences on the mock before the run: made up.
pub(super) fn seeded_prefs() -> Value {
    json!({
        APP: {
            "activityGenerationSettings": {"messageNotify": true},
            "toastSettings": {"systemDefault": true},
            "emailSettings": {},
            "madeUpKeptKey": "kept as it was",
        },
        OTHER_APP: {"toastSettings": {"systemDefault": false}},
    })
}

/// The flows, after the pane's own (each leaves the window as the next expects it).
pub(super) const CHECKS: [super::Check; 8] = [
    ("app_item_menu_offers_turn_off_in_red", app_menu_offers_turn_off),
    ("person_item_offers_no_turn_off", person_item_offers_none),
    ("turn_off_writes_the_app_off_and_undo_restores", turn_off_and_undo),
    ("app_item_after_undo_shows_a_banner_again", banner_after_undo),
    ("refused_turn_off_shows_the_error_and_no_success", refused_turn_off),
    ("ctrl_alt_shift_o_turns_off_and_dismiss_closes", keyboard_and_dismiss),
    ("later_item_of_the_app_turned_off_shows_no_banner", no_banner_after),
    ("dark_theme_turn_off_menu_and_card", dark_menu_and_card),
];

impl Qa<'_> {
    /// What the mock holds of a's app preferences: `status` of its read, `settings` (parsed), and `puts`, every
    /// `PUT` of them it got (refused ones too), each as the setting it carried.
    fn prefs(&self) -> SandboxResult<Value> {
        let prefs = self.ask("prefs")?;
        ensure(prefs.get("error").is_none(), || format!("the mock's prefs: {prefs}"))?;
        Ok(prefs)
    }

    fn puts(&self) -> SandboxResult<usize> {
        Ok(self.prefs()?["puts"].as_array().map_or(0, Vec::len))
    }

    /// Waits for the mock's preferences to take what `wanted` takes.
    fn prefs_until(&self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let mut last = Value::Null;
        wait_until(WAIT, || match self.prefs() {
            Ok(prefs) if wanted(&prefs) => Some(prefs),
            Ok(prefs) => {
                last = prefs;
                thread::sleep(Duration::from_millis(200));
                None
            }
            Err(_) => None,
        })
        .ok_or_else(|| format!("{what}: the mock holds {last}").into())
    }

    /// The pane with the Apps chip on, its first row an app item (the newest); that row.
    fn apps_pane(&mut self) -> SandboxResult<Value> {
        self.plain_pane()?;
        self.click_chip("apps")?;
        let state = self.state_until("the Apps chip on", |state| state["filter"] == "apps")?;
        let row = first(&state)?;
        ensure(text(&row, "kind") == "app", || format!("an app item first: {row}"))?;
        Ok(row)
    }

    /// The first row selected from the keyboard (as `keyboard_walks_and_opens` walks to it).
    fn select_first_by_keys(&mut self) -> SandboxResult {
        self.key(keys::HOME)?;
        self.key(keys::DOWN)?;
        self.key(keys::UP)
    }

    fn turn_off_key(&mut self) -> SandboxResult {
        self.chord(&[Modifier::Ctrl, Modifier::Alt, Modifier::Shift], 'o')
    }

    /// Waits for the success card.
    fn card(&self) -> SandboxResult<Value> {
        self.state_until("the success card", |state| state["toast"] == TURNED_OFF)
    }
}

/// How many pixels in `band` of the screenshot are drawn in Teams' red (the danger colour, anti-aliased).
fn red_pixels(shot: &Path, (left, top, right, bottom): (u32, u32, u32, u32)) -> SandboxResult<usize> {
    let image = image::open(shot)?.to_rgb8();
    Ok((top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let [r, g, b] = image.get_pixel(x, y).0;
            r > 150 && r.saturating_sub(g) > 70 && r.saturating_sub(b) > 50
        })
        .count())
}

/// Whether `prefs`' setting has [`APP`] turned off as Teams turns it off: each declared type and the default off in
/// what Activity makes and what it toasts, its other keys and the other app's preference kept.
fn app_off(prefs: &Value) -> bool {
    let app = &prefs["settings"][APP];
    let off = |category: &str| {
        APP_TYPES
            .iter()
            .chain(std::iter::once(&"systemDefault"))
            .all(|kind| app[category][*kind] == false)
    };
    off("activityGenerationSettings")
        && off("toastSettings")
        && app["madeUpKeptKey"] == "kept as it was"
        && prefs["settings"][OTHER_APP] == seeded_prefs()[OTHER_APP]
}

/// The app item's menu: "Turn off Workflows" in red, last, after Notification settings; its tooltip names the action
/// and Ctrl+Alt+Shift+O.
fn app_menu_offers_turn_off(qa: &mut Qa<'_>) -> SandboxResult {
    qa.apps_pane()?;
    qa.open_row_menu()?;
    let shot = qa.shot("app-menu-light")?;
    let red = red_pixels(&shot, TURN_OFF_BAND)?;
    ensure(red > 40, || {
        format!("no red row after Notification settings ({red} red pixels)")
    })?;
    qa.hover(ITEM_MENU_TURN_OFF)?;
    thread::sleep(Duration::from_millis(1500));
    qa.shot("app-menu-tooltip-light")?;
    qa.key(keys::ESC)?;
    Ok(())
}

/// A person's item (a reaction) has no "Turn off" row, and Ctrl+Alt+Shift+O on it changes nothing.
fn person_item_offers_none(qa: &mut Qa<'_>) -> SandboxResult {
    qa.plain_pane()?;
    qa.click_chip("reactions")?;
    let state = qa.state_until("the Reactions chip on", |state| state["filter"] == "reactions")?;
    let row = first(&state)?;
    ensure(text(&row, "kind") == "reaction", || format!("a reaction first: {row}"))?;
    let puts = qa.puts()?;
    qa.open_row_menu()?;
    let shot = qa.shot("person-menu-light")?;
    let red = red_pixels(&shot, TURN_OFF_BAND)?;
    ensure(red < 10, || format!("a red row on a person's item ({red} red pixels)"))?;
    qa.key(keys::ESC)?;
    qa.select_first_by_keys()?;
    qa.turn_off_key()?;
    thread::sleep(QUIET);
    let state = qa.state()?;
    ensure(state["toast"].is_null(), || format!("a toast: {}", state["toast"]))?;
    ensure(qa.puts()? == puts, || "the key sent a preference".to_owned())?;
    Ok(())
}

/// Turn off: the mock has the app's types off (the other app's and unknown keys kept), every item stays, and the
/// success card shows; Undo puts the app's earlier preference back and the card goes.
fn turn_off_and_undo(qa: &mut Qa<'_>) -> SandboxResult {
    ensure(qa.prefs()?["settings"] == seeded_prefs(), || {
        "the seeded preferences".to_owned()
    })?;
    let total = qa.ctl_items(&[])?.len();
    let row = qa.apps_pane()?;
    qa.open_row_menu()?;
    qa.click(ITEM_MENU_TURN_OFF)?;
    let state = qa.card()?;
    ensure(state["item_menu"].is_null(), || "the menu stayed open".to_owned())?;
    let prefs = qa.prefs()?;
    ensure(app_off(&prefs), || format!("the app turned off on the mock: {prefs}"))?;
    qa.shot("app-off-light")?;
    // Undo before the card's four seconds run out; the rest is checked after
    qa.click(CARD_UNDO)?;
    qa.state_until("the card gone after Undo", |state| state["toast"].is_null())?;
    qa.prefs_until("the earlier preference back", |prefs| {
        prefs["settings"] == seeded_prefs()
    })?;
    let state = qa.state()?;
    ensure(items(&state).iter().any(|item| item["id"] == row["id"]), || {
        "the app item is gone".to_owned()
    })?;
    ensure(qa.ctl_items(&[])?.len() == total, || "the feed lost items".to_owned())?;
    qa.hover((900, 500))?;
    qa.shot("app-undone-light")?;
    Ok(())
}

/// A refused write (403), an answer whose arrival is unknown (500) and a refused read of the preferences (500): each
/// shows "Couldn't update activity. Try again.", never the success card, and the mock keeps the earlier preference.
fn refused_turn_off(qa: &mut Qa<'_>) -> SandboxResult {
    for (refusal, shot) in [
        ("PUT 403 1", "app-off-refused-light"),
        ("PUT 500 1", "app-off-unknown-light"),
        ("GET 500 1", "app-off-unread-light"),
    ] {
        qa.ask(&format!("refuse {refusal}"))?;
        qa.apps_pane()?;
        qa.open_row_menu()?;
        qa.click(ITEM_MENU_TURN_OFF)?;
        let seen = std::cell::RefCell::new(Vec::new());
        let state = qa.state_until(&format!("the error after {refusal}"), |state| {
            if !state["toast"].is_null() {
                seen.borrow_mut().push(state["toast"].clone());
            }
            state["toast"] == teams_ui::activity_view::WRITE_FAILED
        });
        let seen = seen.into_inner();
        let state = state.map_err(|error| format!("{error}; toasts seen: {seen:?}"))?;
        ensure(!seen.iter().any(|toast| toast == TURNED_OFF), || {
            format!("success shown after {refusal}")
        })?;
        qa.shot(shot)?;
        ensure(qa.prefs()?["settings"] == seeded_prefs(), || {
            format!("the mock changed after {refusal}")
        })?;
        say_state(refusal, &state);
        thread::sleep(Duration::from_secs(4));
    }
    Ok(())
}

fn say_state(refusal: &str, state: &Value) {
    super::say(&format!("turn off after {refusal}: toast {}", state["toast"]));
}

/// Ctrl+Alt+Shift+O on the selected app item turns its app off with the card; Dismiss closes the card and the
/// preference stays off.
fn keyboard_and_dismiss(qa: &mut Qa<'_>) -> SandboxResult {
    let puts = qa.puts()?;
    qa.apps_pane()?;
    qa.select_first_by_keys()?;
    qa.turn_off_key()?;
    qa.card()?;
    qa.click(CARD_DISMISS)?;
    qa.state_until("the card gone after Dismiss", |state| state["toast"].is_null())?;
    let prefs = qa.prefs()?;
    ensure(app_off(&prefs), || format!("the app off on the mock: {prefs}"))?;
    ensure(prefs["puts"].as_array().map_or(0, Vec::len) == puts + 1, || {
        format!("one write for the key: {prefs}")
    })?;
    Ok(())
}

/// With the app off, a later item of it comes into the pane without a banner; b's chat message still notifies, so
/// the banners themselves work.
fn no_banner_after(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    qa.chord(&[Modifier::Ctrl], '1')?;
    let shown = qa.sandbox.bus().notifications().len();
    qa.ask("push-chat")?;
    wait_until(WAIT, || (qa.sandbox.bus().notifications().len() > shown).then_some(()))
        .ok_or_else(|| format!("no banner for b's message in {AB_CHAT}"))?;
    thread::sleep(BANNER_WAIT);
    let shown = qa.sandbox.bus().notifications().len();
    let pushed = qa.ask("push-app later")?;
    let id = text(&pushed, "id");
    qa.state_until("the app's later item listed", |state| {
        items(state).iter().any(|item| text(item, "id") == id)
    })?;
    thread::sleep(BANNER_WAIT);
    let notifications = qa.sandbox.bus().notifications();
    ensure(notifications.len() == shown, || {
        format!("a banner for the app turned off: {:?}", &notifications[shown..])
    })?;
    qa.hover((900, 500))?;
    qa.shot("app-later-item-light")?;
    Ok(())
}

/// A new app item while its app's notifications are on (the seeded preference saves no choice for its type): one
/// banner with the item's title and preview, as Teams shows one; its click shows the Activity pane with the item
/// selected and read.
pub(super) fn banner_while_on(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    let shown = qa.sandbox.bus().notifications().len();
    let pushed = qa.ask("push-app earlier")?;
    let id = text(&pushed, "id");
    qa.state_until("the app's new item listed", |state| {
        items(state).iter().any(|item| text(item, "id") == id)
    })?;
    let banner = wait_until(BANNER_WAIT, || qa.sandbox.bus().notifications().get(shown).cloned())
        .ok_or("no banner for a new app item while its notifications are on")?;
    thread::sleep(BANNER_WAIT);
    let banners = qa.sandbox.bus().notifications().len() - shown;
    super::say(&format!(
        "banners for a new app item while its notifications are on: {banners} ({:?}: {:?})",
        banner.summary, banner.body
    ));
    ensure(banners == 1, || format!("{banners} banners for one app item"))?;
    ensure(
        banner.summary == "Follow up on a message" && banner.body == "Made-up earlier reminder",
        || format!("the banner says {:?}: {:?}", banner.summary, banner.body),
    )?;
    qa.sandbox.bus().invoke_action(banner.id, "default")?;
    let state = qa.state_until("the Activity pane at the item, selected and read", |state| {
        state["pane_shown"] == true
            && state["selected"] == id.as_str()
            && items(state)
                .iter()
                .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    super::say(&format!(
        "the banner's click: list {}, main {}",
        state["list"], state["main"]
    ));
    Ok(())
}

/// The window closed into the tray: a new app item's banner, clicked, opens the window again with the Activity pane
/// at the item, selected and read, once the new pane has read its feed; nothing else opens after it.
pub(super) fn banner_reopens_the_window(qa: &mut Qa<'_>) -> SandboxResult {
    banner_into_closed_window(qa, "tray", "banner-opened-light")
}

/// The same in Teams Dark.
pub(super) fn dark_banner_reopens_the_window(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("theme dark")?;
    let outcome = banner_into_closed_window(qa, "tray-dark", "banner-opened-dark");
    qa.ask("theme light")?;
    outcome
}

fn banner_into_closed_window(qa: &mut Qa<'_>, which: &str, shot: &str) -> SandboxResult {
    qa.sandbox.niri_msg(&["action", "close-window"])?;
    wait_until(WAIT, || qa.ask("state").ok().filter(|state| state["window"] == false))
        .ok_or("the window didn't close into the tray")?;
    let shown = qa.sandbox.bus().notifications().len();
    let pushed = qa.ask(&format!("push-app {which}"))?;
    let id = text(&pushed, "id");
    let banner = wait_until(BANNER_WAIT, || qa.sandbox.bus().notifications().get(shown).cloned())
        .ok_or("no banner for a new app item with the window closed")?;
    ensure(banner.body == format!("Made-up {which} reminder"), || {
        format!("the banner says {:?}: {:?}", banner.summary, banner.body)
    })?;
    qa.trace.drain();
    let from = qa.trace.seen.len();
    qa.sandbox.bus().invoke_action(banner.id, "default")?;
    qa.trace.next_where("the app item opened", |line| {
        line["view"] == "activity" && line["change"] == "opened" && line["kinds"] == json!(["app"])
    })?;
    // read already: the earlier Mark all read put the horizon at the wall clock, past the mock's item times
    let opened = qa.state_until("the window again, the pane at the item, selected and read", |state| {
        state["list"] == "Activity"
            && state["pane_shown"] == true
            && state["selected"] == id.as_str()
            && items(state)
                .iter()
                .any(|item| text(item, "id") == id && item["read"] == true)
    })?;
    super::say(&format!(
        "the banner's click into a closed window: list {}, main {}, chat {}, selected {}",
        opened["list"], opened["main"], opened["chat"], opened["selected"]
    ));
    thread::sleep(QUIET);
    qa.trace.drain();
    let opens = qa
        .trace
        .seen
        .iter()
        .skip(from)
        .filter(|line| line["view"] == "activity" && line["change"] == "opened")
        .count();
    let after = qa.state()?;
    ensure(
        opens == 1
            && after["selected"] == opened["selected"]
            && after["chat"] == opened["chat"]
            && after["main"] == opened["main"]
            && after["list"] == "Activity",
        || format!("something else opened after the item ({opens} opens since the click): {after}"),
    )?;
    qa.hover((900, 500))?;
    qa.shot(shot)?;
    Ok(())
}

/// After Undo the app's banners are on again: its next item shows one.
fn banner_after_undo(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    let shown = qa.sandbox.bus().notifications().len();
    let pushed = qa.ask("push-app undone")?;
    let id = text(&pushed, "id");
    qa.state_until("the app's item after Undo listed", |state| {
        items(state).iter().any(|item| text(item, "id") == id)
    })?;
    let banner = wait_until(BANNER_WAIT, || qa.sandbox.bus().notifications().get(shown).cloned())
        .ok_or("no banner for the app's item after Undo")?;
    thread::sleep(BANNER_WAIT);
    let banners = qa.sandbox.bus().notifications().len() - shown;
    super::say(&format!(
        "banners for the app's item after Undo: {banners} ({:?}: {:?})",
        banner.summary, banner.body
    ));
    ensure(banners == 1 && banner.body == "Made-up undone reminder", || {
        format!("{banners} banners, the first {:?}: {:?}", banner.summary, banner.body)
    })?;
    Ok(())
}

/// Teams Dark: the app item's menu, then the card after Turn off; Undo puts back the preference before it (off).
fn dark_menu_and_card(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ask("theme dark")?;
    let outcome = dark_shots(qa);
    qa.ask("theme light")?;
    outcome
}

fn dark_shots(qa: &mut Qa<'_>) -> SandboxResult {
    qa.apps_pane()?;
    qa.open_row_menu()?;
    let shot = qa.shot("app-menu-dark")?;
    let red = red_pixels(&shot, TURN_OFF_BAND)?;
    ensure(red > 40, || format!("no red row in Teams Dark ({red} red pixels)"))?;
    qa.click(ITEM_MENU_TURN_OFF)?;
    qa.card()?;
    qa.shot("app-off-dark")?;
    qa.click(CARD_UNDO)?;
    qa.state_until("the card gone after Undo", |state| state["toast"].is_null())?;
    let prefs = qa.prefs()?;
    ensure(app_off(&prefs), || format!("still off after Undo: {prefs}"))?;
    Ok(())
}

/// The read-only mode: the app item's menu has no "Turn off" row and Ctrl+Alt+Shift+O sends nothing.
pub(super) fn read_only_offers_none(qa: &mut Qa<'_>) -> SandboxResult {
    qa.apps_pane()?;
    qa.open_row_menu()?;
    let shot = qa.shot("app-menu-read-only-light")?;
    let red = red_pixels(&shot, TURN_OFF_BAND)?;
    ensure(red < 10, || {
        format!("a red row in the read-only mode ({red} red pixels)")
    })?;
    qa.key(keys::ESC)?;
    qa.select_first_by_keys()?;
    qa.turn_off_key()?;
    thread::sleep(QUIET);
    let prefs = qa.prefs()?;
    ensure(prefs["puts"].as_array().is_some_and(Vec::is_empty), || {
        format!("a write in the read-only mode: {prefs}")
    })?;
    ensure(prefs["settings"].is_null(), || format!("the mock's prefs: {prefs}"))?;
    let state = qa.state()?;
    ensure(state["toast"].is_null(), || format!("a toast: {}", state["toast"]))?;
    Ok(())
}
