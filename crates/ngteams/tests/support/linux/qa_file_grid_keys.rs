//! QA of task 849 (round 1), in the same walk: the keyboard reaches each file card in a chat.
//!
//! The chat has a document alone, then an image preview and a video in one message, then a call's recording with its
//! recap card. From the box: Escape (the keys go to the list), Enter (into it), then Tab.
//!
//! - **The order**: Tab rings the document, the image, the video and the recap's thumbnail in the order of the
//!   messages, and Shift+Tab rings them again from the last to the first. A ring is found on the screen: a 2 px line
//!   in the text's colour inside the edge. Teams Light and Dark shots of each one ringed.
//! - **Enter and Space**: on the document and the image, each opens the file's link; on the video, each plays it in its
//!   card with the keys in the player; on the recap's thumbnail, Enter shows the Recap tab and plays the recording.
//! - **Escape**: in the player, it pauses and the ring comes back to the video's card; a second Escape gives the keys to
//!   the list. A menu of the player closes first, then full screen ends.
//! - **The player's controls**: Tab goes through Play, Back 10 seconds, Forward 10 seconds, Mute, More options, Full
//!   screen and Open in browser in that order. Space and Enter on Play play the video, on More options open a menu, on
//!   Full screen fill the window, and on Open in browser open the link and nothing else; on each other control they do
//!   not play the video, fill the window or open a link.
//! - **The image's More options** shows while the keys are on the image, and Enter on it opens its menu.
//! - **The pointer**: a press on a card leaves the keys in the box; a click on the image still opens it.
//!
//! Every name, id, link and byte is made up.

use std::io;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use teams_mock::{AMS_BASE, ChatServiceMock, LAB_A, LAB_C};
use ui_sandbox::input::keys;
use ui_sandbox::{SandboxResult, wait_until};

use super::{
    CARD_WIDTH, CLIP_A, DARK, DOC, HEADER, LIGHT, Look, MENU_REACH, MadeUpFile, Posted, QUIET, Rect, SETTLE, WAIT,
    Walk, at, cards_in, changed_box, changed_in, entries, files_value, load, made_up_frame, note, post_files, runs,
    share_url, step, video,
};

/// The image c shares with its preview (task 849).
const SHOWN_PHOTO: MadeUpFile = MadeUpFile {
    name: "made-up-keys-photo.png",
    extension: "png",
    kind: 'i',
    share: "MadeUpShareLinkKeysPhoto0113",
    frame: None,
};

/// The AMS object of the image's preview.
const PREVIEW_OBJECT: &str = "0-made-up-qa849-photo";

/// Where the conversation pane starts: right of the chat list, under the title bar.
const PANE: (u32, u32) = (460, 50);

/// The most Tab presses the walk gives to pass every card of the chat.
const STEPS: u32 = 40;

/// The controls of a card's player in the order Tab reaches them, as the walk's app names them ("More options" is the
/// kit's button, which it calls `player`).
const PLAYER_CONTROLS: [&str; 7] = ["play", "back", "forward", "mute", "player", "fullscreen", "open"];

/// The Tab presses from the player's picture to "More options" (`player` in [`PLAYER_CONTROLS`]).
const MORE_AT: u32 = 5;
const _: () = assert!(matches!(PLAYER_CONTROLS[MORE_AT as usize - 1].as_bytes(), b"player"));

/// The pause after a key.
const KEY_GAP: Duration = Duration::from_millis(250);

/// The cards in the order of the messages, with what a screen reader would call them.
const KINDS: [&str; 4] = ["document", "image", "video", "recap"];

/// A card the keyboard reached: after how many Tab presses, and its ring on the screen.
#[derive(Clone, Copy, Debug)]
struct Stop {
    presses: u32,
    ring: Rect,
}

// --- the service ---

/// c's document alone, then its image and video together, then a recording of a call: the keyboard's chat. The two
/// messages with files.
pub(super) fn post(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult<Vec<Posted>> {
    let chat = mock
        .add_group("Made-up keyboard walk", &[LAB_C, LAB_A])
        .ok_or("no group made")?;
    let doc = post_files(mock, &chat, "", files_value(&[&DOC]))?;
    let mut list = entries(&[&SHOWN_PHOTO, &CLIP_A]);
    list[0]["filePreview"] = json!({
        "previewUrl": format!("{AMS_BASE}/v1/objects/{PREVIEW_OBJECT}/views/imgo"),
        "previewWidth": 640,
        "previewHeight": 400,
    });
    let media = post_files(mock, &chat, "", Value::String(list.to_string()))?;
    let mut png = Vec::new();
    made_up_frame([200, 60, 40], [250, 220, 120]).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{PREVIEW_OBJECT}/views/imgo"), "image/png", png));
    mock.add_message(&chat, (LAB_C, "Lab Cee"), "RichText/Media_CallRecording", &recording())
        .ok_or("the recording's message refused")?;
    Ok(vec![
        Posted {
            key: "keys-doc",
            chat: chat.clone(),
            id: doc,
            cards: 1,
        },
        Posted {
            key: "keys-media",
            chat,
            id: media,
            cards: 2,
        },
    ])
}

/// The recording's message, ready, in c's OneDrive, its link the one the recap of task 793 plays.
fn recording() -> String {
    let link = video::RECORDING;
    let (drive, item) = (video::RECORDING_DRIVE, video::RECORDING_ITEM);
    let owner = LAB_C.trim_start_matches("8:orgid:");
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"Success\" code=\"200\" />\
         <Title>Made-up keyboard call</Title><a href=\"{link}\">Play</a>\
         <OriginalName v=\"Made-up keyboard call-20260101_120100-Meeting Recording.mp4\" />\
         <MeetingOrganizerId value=\"\" /><RecordingInitiatorId value=\"{LAB_C}\" isAutoRecording=\"False\" />\
         <RecordingStorage type=\"RecordingInitiatorOneDrive\" userId=\"{owner}\" />\
         <Identifiers><Id type=\"callId\" value=\"made-up-call-849\" /></Identifiers>\
         <RecordingContent contentTypes=\"Recording\" timestamp=\"2026-01-01T12:01:00.0000000Z\" duration=\"0:00:03.0\">\
         <item type=\"onedriveForBusinessVideo\" uri=\"{link}\" driveId=\"{drive}\" driveItemId=\"{item}\" />\
         </RecordingContent></URIObject>"
    )
}

// --- the walk ---

impl Walk<'_> {
    pub(super) fn card_keys(&mut self) -> SandboxResult {
        self.run.theme(false)?;
        let stops = self.tab_order()?;
        let Ok(stops) = <[Stop; 4]>::try_from(stops.as_slice()) else {
            return Ok(());
        };
        self.back_order(&stops)?;
        self.ring_shots(&stops)?;
        self.doc_keys(stops[0])?;
        self.image_keys(stops[1])?;
        self.video_keys(stops[2])?;
        self.player_controls(stops[2])?;
        self.escape_order(stops[2])?;
        self.recap_keys(stops[3])?;
        self.pointer(&stops)?;
        self.run.theme(false)
    }

    /// The keyboard's chat afresh (another chat first), then Escape (the box gives the keys to the list) and Enter
    /// (into the list).
    fn fresh(&mut self) -> SandboxResult {
        self.open_message("one")?;
        self.open_message("keys-media")?;
        self.key(keys::ESC)?;
        self.key(keys::ENTER)
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.run.input.key(key)?;
        thread::sleep(KEY_GAP);
        Ok(())
    }

    fn back_key(&mut self) -> SandboxResult {
        self.run
            .input
            .key_with(&[ui_sandbox::input::Modifier::Shift], keys::TAB)?;
        thread::sleep(KEY_GAP);
        Ok(())
    }

    fn tabs(&mut self, presses: u32) -> SandboxResult {
        for _ in 0..presses {
            self.key(keys::TAB)?;
        }
        Ok(())
    }

    /// The keyboard's chat afresh with the keys on `stop`; whether its ring shows (a finding when not).
    fn keys_on(&mut self, stop: Stop, what: &str) -> SandboxResult<bool> {
        let mut ringed = false;
        // a second try: a loaded host can drop a Tab press while the chat opens
        for _ in 0..2 {
            self.fresh()?;
            self.tabs(stop.presses)?;
            thread::sleep(SETTLE);
            self.run.shot(self.sandbox, &format!("qa849-{what}-on-light.png"))?;
            ringed = has_ring(&self.run.last, LIGHT, stop.ring);
            if ringed {
                break;
            }
        }
        if !ringed {
            note(format!(
                "{what}: after {} Tab presses no ring on the card at {:?}",
                stop.presses, stop.ring
            ));
        }
        Ok(ringed)
    }

    /// Where the keys are: `list`, `box`, `other` or `none`.
    fn focus(&mut self) -> SandboxResult<String> {
        Ok(self.run.question("focus")?.trim_matches('"').to_owned())
    }

    /// Tab from the list's first stop: the cards ringed, each once, in the order the keys came to them.
    fn tab_order(&mut self) -> SandboxResult<Vec<Stop>> {
        self.fresh()?;
        let mut stops: Vec<Stop> = Vec::new();
        let mut seen = Vec::new();
        for presses in 0..=STEPS {
            if presses > 0 {
                self.key(keys::TAB)?;
            }
            self.run.shot(self.sandbox, "qa849-tab-scan.png")?;
            let rings = rings(&self.run.last, LIGHT);
            seen.push(format!("{presses}: {rings:?}"));
            if let [ring] = rings.as_slice()
                && stops.last().is_none_or(|last| !same(last.ring, *ring))
            {
                let kind = KINDS.get(stops.len()).copied().unwrap_or("more");
                self.run
                    .shot(self.sandbox, &format!("qa849-tab-{}-{kind}-light.png", stops.len()))?;
                stops.push(Stop { presses, ring: *ring });
                if stops.len() == KINDS.len() {
                    break;
                }
            }
        }
        step(&format!("Tab rings: {stops:?}"));
        if stops.len() != KINDS.len() {
            note(format!(
                "Tab from the list's first stop rings {} cards, not the document, the image, the video and the recap \
                 ({stops:?}); each press: {seen:?}",
                stops.len()
            ));
            return Ok(stops);
        }
        let mut reading = stops.clone();
        reading.sort_by_key(|stop| (stop.ring.y0 / 8, stop.ring.x0));
        if reading
            .iter()
            .zip(&stops)
            .any(|(read, came)| !same(read.ring, came.ring))
        {
            note(format!(
                "Tab rings the cards out of the order of the messages: {stops:?}"
            ));
        }
        let thumbnail = stops[3].ring;
        if thumbnail.width().abs_diff(142) > 12 || thumbnail.height().abs_diff(80) > 12 {
            note(format!(
                "the fourth ring is not the recap's thumbnail (142 by 80): {thumbnail:?}"
            ));
        }
        if stops[2].ring.width().abs_diff(CARD_WIDTH) > 12 {
            note(format!(
                "the third ring is not the video's card ({CARD_WIDTH} wide): {:?}",
                stops[2].ring
            ));
        }
        Ok(stops)
    }

    /// Shift+Tab from the recap's thumbnail rings the video, the image and the document, in that order.
    fn back_order(&mut self, stops: &[Stop; 4]) -> SandboxResult {
        self.fresh()?;
        self.tabs(stops[3].presses)?;
        let mut back: Vec<Rect> = Vec::new();
        for _ in 0..=(stops[3].presses + 2) {
            self.back_key()?;
            self.run.shot(self.sandbox, "qa849-back-scan.png")?;
            if let [ring] = rings(&self.run.last, LIGHT).as_slice()
                && back.last().is_none_or(|last| !same(*last, *ring))
            {
                back.push(*ring);
                if back.len() == 3 {
                    break;
                }
            }
        }
        let wanted: Vec<Rect> = stops[..3].iter().rev().map(|stop| stop.ring).collect();
        let matches = back.len() == wanted.len() && back.iter().zip(&wanted).all(|(got, want)| same(*got, *want));
        if !matches {
            note(format!(
                "Shift+Tab from the recap rings {back:?}, not the video, the image and the document {wanted:?}"
            ));
        }
        step(&format!("Shift+Tab rings: {back:?}"));
        Ok(())
    }

    /// Each card ringed, in Teams Light and Teams Dark.
    fn ring_shots(&mut self, stops: &[Stop; 4]) -> SandboxResult {
        for (stop, kind) in stops.iter().zip(KINDS) {
            if !self.keys_on(*stop, kind)? {
                continue;
            }
            for look in [LIGHT, DARK] {
                self.run.theme(look.dark)?;
                thread::sleep(SETTLE);
                self.run
                    .shot(self.sandbox, &format!("qa849-focused-{kind}-{}.png", look.name))?;
                if !has_ring(&self.run.last, look, stop.ring) {
                    note(format!(
                        "{}: the {kind} card's ring does not show ({:?})",
                        look.name, stop.ring
                    ));
                }
            }
            self.run.theme(false)?;
        }
        step("each card ringed in Teams Light and Dark");
        Ok(())
    }

    /// `key` on the ringed card: the link it opened, if any in [`QUIET`].
    fn key_opens(&mut self, key: u32) -> SandboxResult<Option<String>> {
        let before = self.sandbox.opened()?.len();
        self.key(key)?;
        let opened = wait_until(QUIET * 2, || self.sandbox.opened().ok()?.get(before).cloned());
        Ok(opened.and_then(|opened| opened.args.last().cloned()))
    }

    /// Space and Enter on the document open its link.
    fn doc_keys(&mut self, stop: Stop) -> SandboxResult {
        for (key, name) in [(keys::SPACE, "Space"), (keys::ENTER, "Enter")] {
            if !self.keys_on(stop, "document")? {
                continue;
            }
            let opened = self.key_opens(key)?;
            if opened.as_deref() != Some(share_url(&DOC).as_str()) {
                note(format!("{name} on the document's card opens {opened:?}, not its link"));
            }
            step(&format!("{name} on the document opens {opened:?}"));
        }
        Ok(())
    }

    /// Enter and Space on the image open its link; its More options shows while the keys are on it, and Enter opens
    /// its menu, which Escape closes.
    fn image_keys(&mut self, stop: Stop) -> SandboxResult {
        for (key, name) in [(keys::ENTER, "Enter"), (keys::SPACE, "Space")] {
            if !self.keys_on(stop, "image")? {
                continue;
            }
            let opened = self.key_opens(key)?;
            if opened.as_deref() != Some(share_url(&SHOWN_PHOTO).as_str()) {
                note(format!("{name} on the image's card opens {opened:?}, not its link"));
            }
            step(&format!("{name} on the image opens {opened:?}"));
        }
        // the same screen with the keys on the box: no More options
        self.open_message("one")?;
        self.open_message("keys-media")?;
        let unfocused = load(&self.run.shot(self.sandbox, "qa849-image-unfocused-light.png")?)?;
        if !self.keys_on(stop, "image-more")? {
            return Ok(());
        }
        let corner = Rect {
            x0: stop.ring.x1.saturating_sub(48),
            y0: stop.ring.y0 + 3,
            x1: stop.ring.x1 - 3,
            y1: stop.ring.y0 + 44,
        };
        let shown = changed_in(&unfocused, &self.run.last, corner);
        if shown < 40 {
            note(format!(
                "the image's More options does not show while the keys are on the image ({shown} pixels in {corner:?})"
            ));
        }
        self.key(keys::TAB)?;
        thread::sleep(SETTLE);
        let on_more = load(&self.run.shot(self.sandbox, "qa849-image-more-focused-light.png")?)?;
        self.key(keys::ENTER)?;
        thread::sleep(SETTLE);
        let opened = load(&self.run.shot(self.sandbox, "qa849-image-more-open-light.png")?)?;
        let menu = changed_box(&on_more, &opened, corner.grown(MENU_REACH), corner)
            .filter(|menu| menu.height() > 20 && menu.width() > 60);
        if menu.is_none() {
            note("Tab to the image's More options, then Enter: no menu opens".to_owned());
        }
        self.key(keys::ESC)?;
        let closed = load(&self.run.shot(self.sandbox, "qa849-image-more-closed-light.png")?)?;
        if let Some(menu) = menu
            && changed_in(&on_more, &closed, menu) > 200
        {
            note(format!("Escape does not close the image's menu ({menu:?})"));
        }
        step(&format!(
            "the image's More options: {shown} pixels shown; Enter opens {menu:?}"
        ));
        Ok(())
    }

    /// The state of the video's player in the keyboard's chat.
    fn video_state(&mut self) -> SandboxResult<Option<String>> {
        let id = format!("{}-1 ", self.posted("keys-media")?.id);
        let states = self.run.players_until("the players", |_| true)?;
        Ok(states.into_iter().find(|state| state.starts_with(&id)))
    }

    /// Waits for the video's player in a state `wanted` takes; its state then.
    fn video_until(&mut self, wanted: impl Fn(&str) -> bool) -> SandboxResult<Option<String>> {
        let id = format!("{}-1 ", self.posted("keys-media")?.id);
        let found = self.run.players_until("the video's player", |states| {
            states.iter().any(|state| state.starts_with(&id) && wanted(state))
        });
        match found {
            Ok(states) => Ok(states.into_iter().find(|state| state.starts_with(&id))),
            Err(_) => self.video_state(),
        }
    }

    /// Space and Enter on the video play it in its card, the keys in the player; Escape pauses it and rings the card
    /// again; a second Escape gives the keys to the list.
    fn video_keys(&mut self, stop: Stop) -> SandboxResult {
        for (key, name) in [(keys::SPACE, "Space"), (keys::ENTER, "Enter")] {
            if !self.keys_on(stop, "video")? {
                continue;
            }
            let before = self.video_state()?;
            self.key(key)?;
            let started = self.video_until(|state| ["Playing", "Ended"].iter().any(|seen| state.contains(seen)))?;
            let focus = self.focus()?;
            self.run
                .shot(self.sandbox, &format!("qa849-video-{name}-playing-light.png"))?;
            if started == before
                || !started
                    .as_deref()
                    .is_some_and(|state| state.contains("Playing") || state.contains("Ended"))
            {
                note(format!(
                    "{name} on the video's card plays nothing: {before:?} -> {started:?}"
                ));
                continue;
            }
            if focus != "other" || has_ring(&self.run.last, LIGHT, stop.ring) {
                note(format!(
                    "{name} on the video's card: the keys are not in the player (focus {focus})"
                ));
            }
            self.key(keys::ESC)?;
            let after = self.video_state()?;
            self.run
                .shot(self.sandbox, &format!("qa849-video-{name}-escape-light.png"))?;
            if after.as_deref().is_some_and(|state| state.contains("Playing")) {
                note(format!("Escape in the playing video does not pause it: {after:?}"));
            }
            if !has_ring(&self.run.last, LIGHT, stop.ring) {
                note("Escape in the video's player: no ring on the video's card".to_owned());
            }
            self.key(keys::ESC)?;
            let second = self.focus()?;
            self.run
                .shot(self.sandbox, &format!("qa849-video-{name}-escape-twice-light.png"))?;
            if second != "list" {
                note(format!(
                    "a second Escape after the player gives the keys to {second}, not the list"
                ));
            }
            step(&format!(
                "{name} on the video: {before:?} -> {started:?}, focus {focus}; Escape: {after:?}; then {second}"
            ));
        }
        Ok(())
    }

    /// Enter and Space on each control of the player after the video ended, Tab bringing the keys to it in the order
    /// of the bar: each presses only its own control (only Play plays the video).
    fn player_controls(&mut self, stop: Stop) -> SandboxResult {
        let mut pressed = Vec::new();
        for (presses, name) in (1..).zip(PLAYER_CONTROLS) {
            for (key, key_name) in [(keys::SPACE, "Space"), (keys::ENTER, "Enter")] {
                if !self.keys_on(stop, "video-controls")? {
                    continue;
                }
                self.key(keys::ENTER)?;
                let ended = self.video_until(|state| state.contains("Ended") || state.contains("Paused"))?;
                self.tabs(presses)?;
                let control = self.control()?;
                if control != name {
                    note(format!(
                        "Tab {presses} times into the player reaches {control}, not {name}"
                    ));
                    continue;
                }
                let shot = format!("qa849-control-{presses}-{name}");
                if key == keys::SPACE && matches!(name, "play" | "fullscreen") {
                    self.dark_shot(&format!("{shot}-focused-dark.png"))?;
                }
                let before = load(&self.run.shot(self.sandbox, &format!("{shot}-focused-light.png"))?)?;
                let opened_before = self.sandbox.opened()?.len();
                self.key(key)?;
                thread::sleep(QUIET);
                let after = self.video_state()?;
                let opened = self
                    .sandbox
                    .opened()?
                    .get(opened_before)
                    .and_then(|opened| opened.args.last().cloned());
                let pressed_shot = load(&self.run.shot(self.sandbox, &format!("{shot}-{key_name}-light.png"))?)?;
                let whole = Rect {
                    x0: 0,
                    y0: PANE.1,
                    x1: pressed_shot.width(),
                    y1: pressed_shot.height(),
                };
                let changed = changed_in(&before, &pressed_shot, whole);
                let area = usize::try_from(whole.width() * whole.height()).unwrap_or(usize::MAX);
                let played = after
                    .as_deref()
                    .is_some_and(|state| state.contains("Playing") || state.contains("Loading"));
                let full = changed * 2 > area;
                let menu = (5_000..area / 10).contains(&changed);
                let wanted = match name {
                    "play" => played && !full,
                    "player" => menu && !played && !full,
                    "fullscreen" => full && !played,
                    "open" => opened.as_deref() == Some(share_url(&CLIP_A).as_str()) && !played && !full && !menu,
                    // a skip changes the picture, as a menu does: only what it must not do is checked
                    _ => !played && !full && opened.is_none(),
                };
                if !wanted {
                    note(format!(
                        "{key_name} on the player's {name}: {ended:?} -> {after:?}, {changed} of {area} pixels \
                         changed, opened {opened:?}"
                    ));
                }
                pressed.push(format!("{name} {key_name} {}", if wanted { "ok" } else { "wrong" }));
                for _ in 0..3 {
                    self.key(keys::ESC)?;
                }
                thread::sleep(SETTLE);
            }
        }
        step(&format!("the player's controls: {pressed:?}"));
        Ok(())
    }

    /// A shot `name` in Teams Dark, then Teams Light again.
    fn dark_shot(&mut self, name: &str) -> SandboxResult {
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, name)?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Which control of a player has the keys: its name, `player` for another place in it, else `outside`.
    fn control(&mut self) -> SandboxResult<String> {
        Ok(self.run.question("control")?.trim_matches('"').to_owned())
    }

    /// In the video's player: its menu (Tab to it, Enter), then full screen from the pointer, with the keys on More
    /// options, which full screen does not draw again; then Escape closes the menu, ends full screen, pauses and rings
    /// the card, then gives the keys to the list.
    fn escape_order(&mut self, stop: Stop) -> SandboxResult {
        if !self.keys_on(stop, "video-escape")? {
            return Ok(());
        }
        self.key(keys::ENTER)?;
        let _ = self.video_until(|state| state.contains("Ended") || state.contains("Paused"))?;
        self.tabs(MORE_AT)?;
        thread::sleep(SETTLE);
        let before_menu = load(&self.run.shot(self.sandbox, "qa849-escape-0-more-focused-light.png")?)?;
        self.key(keys::ENTER)?;
        let menu = load(&self.run.shot(self.sandbox, "qa849-escape-1-menu-light.png")?)?;
        let card = stop.ring.grown(MENU_REACH);
        let opened = changed_box(
            &before_menu,
            &menu,
            card,
            Rect {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            },
        )
        .filter(|menu| menu.height() > 20 && menu.width() > 60);
        self.key(keys::ESC)?;
        let closed = load(&self.run.shot(self.sandbox, "qa849-escape-2-menu-closed-light.png")?)?;
        let focus_after_menu = self.focus()?;
        if let Some(opened) = opened
            && changed_in(&before_menu, &closed, opened) > 200
        {
            note(format!(
                "Escape in the player does not close its More options menu first ({opened:?})"
            ));
        }
        if focus_after_menu != "other" || has_ring(&self.run.last, LIGHT, stop.ring) {
            note(format!(
                "Escape on the player's menu: the keys leave the player (focus {focus_after_menu})"
            ));
        }
        let focus_after_full = self.full_screen_escape(stop)?;
        self.key(keys::ESC)?;
        self.run.shot(self.sandbox, "qa849-escape-6-card-light.png")?;
        if !has_ring(&self.run.last, LIGHT, stop.ring) {
            note("the third Escape (menu, full screen, then the player): no ring on the video's card".to_owned());
        }
        self.key(keys::ESC)?;
        let last = self.focus()?;
        if last != "list" {
            note(format!("the fourth Escape gives the keys to {last}, not the list"));
        }
        step(&format!(
            "Escape order: menu {opened:?} closed (focus {focus_after_menu}), full screen ended (focus \
             {focus_after_full}), ring on the card, then {last}"
        ));
        Ok(())
    }

    /// With the keys in the player: a click on its full screen button shows the video full screen, and Escape ends it
    /// with the keys left in the player. Where the keys are then.
    fn full_screen_escape(&mut self, stop: Stop) -> SandboxResult<String> {
        let windowed = load(
            &self
                .run
                .shot(self.sandbox, "qa849-escape-3-fullscreen-focused-light.png")?,
        )?;
        // the player's full screen button: second from the right in its bar
        self.run.click(stop.ring.x1 - 56, stop.ring.y1 - 18)?;
        thread::sleep(SETTLE);
        let full = load(&self.run.shot(self.sandbox, "qa849-escape-4-fullscreen-light.png")?)?;
        let whole = Rect {
            x0: 0,
            y0: PANE.1,
            x1: full.width(),
            y1: full.height(),
        };
        let area = usize::try_from(whole.width() * whole.height()).unwrap_or(usize::MAX);
        if changed_in(&windowed, &full, whole) * 2 < area {
            note("a click on the player's full screen button: the video is not full screen".to_owned());
        }
        self.key(keys::ESC)?;
        let back = load(&self.run.shot(self.sandbox, "qa849-escape-5-windowed-light.png")?)?;
        let focus_after_full = self.focus()?;
        if changed_in(&windowed, &back, whole) * 4 > area {
            note("Escape in full screen does not end full screen".to_owned());
        }
        if focus_after_full != "other" || has_ring(&back, LIGHT, stop.ring) {
            note(format!(
                "Escape in full screen: the keys leave the player (focus {focus_after_full})"
            ));
        }
        Ok(focus_after_full)
    }

    /// With the keys in the box, a press on a card leaves them there; a click on the image opens it.
    fn pointer(&mut self, stops: &[Stop; 4]) -> SandboxResult {
        self.open_message("one")?;
        self.open_message("keys-media")?;
        let start = self.focus()?;
        self.run.shot(self.sandbox, "qa849-pointer-start-light.png")?;
        // the list as it opens is scrolled to its end: the cards as Tab found them, moved by as much as the document
        let Some(now) = cards_in(&self.run.last, LIGHT)
            .into_iter()
            .find(|card| card.x0.abs_diff(stops[0].ring.x0) <= 6 && card.width().abs_diff(stops[0].ring.width()) <= 6)
        else {
            note("the pointer: the document's card is not found on the screen".to_owned());
            return Ok(());
        };
        let moved = |ring: Rect| Rect {
            y0: (ring.y0 + now.y0).saturating_sub(stops[0].ring.y0),
            y1: (ring.y1 + now.y0).saturating_sub(stops[0].ring.y0),
            ..ring
        };
        let doc = moved(stops[0].ring);
        // the empty space under the document's icon: no link, no button
        let (x, y) = (doc.x0 + 40, doc.y0 + HEADER + 40);
        let opened = self.run.click_opens_quietly(self.sandbox, x, y)?;
        let after = self.focus()?;
        if start != "box" || after != "box" {
            note(format!(
                "a press on the document's card at ({x}, {y}) moves the keys from {start} to {after}"
            ));
        }
        let image = moved(stops[1].ring);
        let (cx, cy) = (u32::midpoint(image.x0, image.x1), u32::midpoint(image.y0, image.y1));
        let link = self.run.click_opens(self.sandbox, cx, cy)?;
        if link != share_url(&SHOWN_PHOTO) {
            note(format!("a click on the image opens {link}, not its link"));
        }
        let ringed = self.run.shot(self.sandbox, "qa849-pointer-light.png")?;
        let rings_now = rings(&load(&ringed)?, LIGHT);
        if !rings_now.is_empty() {
            note(format!("after the pointer's clicks, rings show: {rings_now:?}"));
        }
        step(&format!(
            "the pointer: keys {start} -> {after} after a press on the document ({opened:?}); the image opens {link}"
        ));
        Ok(())
    }

    /// Enter on the recap's thumbnail shows the Recap tab and plays the recording.
    fn recap_keys(&mut self, stop: Stop) -> SandboxResult {
        if !self.keys_on(stop, "recap")? {
            return Ok(());
        }
        self.key(keys::ENTER)?;
        let tab = wait_until(WAIT, || self.run.question("tab").ok().filter(|tab| tab == "\"Recap\""));
        if tab.is_none() {
            note(format!(
                "Enter on the recap's thumbnail: the tab is {}",
                self.run.question("tab")?
            ));
            return Ok(());
        }
        let playing = self.run.players_until("the recap's player", |states| {
            states
                .iter()
                .any(|state| state.starts_with("recap-") && (state.contains("Playing") || state.contains("Failed")))
        });
        let recap = playing
            .ok()
            .and_then(|states| states.into_iter().find(|state| state.starts_with("recap-")));
        self.run.shot(self.sandbox, "qa849-recap-enter-light.png")?;
        if !recap.as_deref().is_some_and(|state| state.contains("Playing")) {
            note(format!(
                "Enter on the recap's thumbnail shows the Recap tab but plays nothing: {recap:?}"
            ));
        }
        step(&format!(
            "Enter on the recap's thumbnail: the Recap tab, its player {recap:?}"
        ));
        Ok(())
    }
}

// --- the rings on the screen ---

/// Whether a pixel is in the text's colour of `look`: the ring's.
const fn inked(pixel: [u8; 3], look: Look) -> bool {
    let [red, green, blue] = pixel;
    if look.dark {
        red >= 200 && green >= 200 && blue >= 200
    } else {
        red <= 90 && green <= 90 && blue <= 90
    }
}

/// The rings on the conversation pane: boxes drawn with a 2 px line in the text's colour.
fn rings(shot: &image::RgbImage, look: Look) -> Vec<Rect> {
    let edges = edges(shot, look);
    let mut found: Vec<Rect> = Vec::new();
    for (at, &(top, from, to)) in edges.iter().enumerate() {
        let bottom = edges[at + 1..].iter().find(|&&(y, start, end)| {
            y > top + 16
                && start.abs_diff(from) <= 4
                && end.abs_diff(to) <= 4
                && sides(shot, look, top, y, from, to).is_some()
        });
        if let Some(&(y, _, _)) = bottom
            && let Some((left, right)) = sides(shot, look, top, y, from, to)
        {
            let ring = Rect {
                x0: left,
                y0: top,
                x1: right + 1,
                y1: y + 2,
            };
            if !found.iter().any(|known| known.overlaps(&ring)) {
                found.push(ring);
            }
        }
    }
    found
}

/// The rows 2 px thick in the text's colour, at least 50 px long, as (`y`, `from`, `to`).
fn edges(shot: &image::RgbImage, look: Look) -> Vec<(u32, u32, u32)> {
    let ink = |pixel| inked(pixel, look);
    let share = |y: u32, from: u32, to: u32| (from..to).filter(|&x| ink(at(shot, x, y))).count();
    let mut edges = Vec::new();
    for y in PANE.1 + 1..shot.height().saturating_sub(3) {
        for (from, to) in runs(shot, y, PANE.0, shot.width(), ink) {
            let long = (to - from) as usize;
            if long < 50 {
                continue;
            }
            let thin = share(y - 1, from, to) * 2 < long
                && share(y + 1, from, to) * 10 >= long * 9
                && share(y + 2, from, to) * 2 < long;
            if thin {
                edges.push((y, from, to));
            }
        }
    }
    edges
}

/// The left and right sides of a ring between edges at `top` and `bottom`: a column in the text's colour within 14 px
/// out of each edge's ends (its rounded corners), all the way down.
fn sides(shot: &image::RgbImage, look: Look, top: u32, bottom: u32, from: u32, to: u32) -> Option<(u32, u32)> {
    let ink = |pixel| inked(pixel, look);
    let height = bottom - top;
    let down = |x: u32| (1..4).all(|quarter| ink(at(shot, x, top + height * quarter / 4)));
    let left = (from.saturating_sub(14)..=from + 2).find(|&x| down(x))?;
    let right = (to.saturating_sub(3)..to + 14).rev().find(|&x| down(x))?;
    Some((left, right))
}

/// Whether `ring` (as Tab found it) shows on `shot`.
fn has_ring(shot: &image::RgbImage, look: Look, ring: Rect) -> bool {
    rings(shot, look).iter().any(|found| same_shape(*found, ring))
}

/// A box of the same size at the same left and right, give or take a few pixels: the list may scroll it up or down.
const fn same_shape(one: Rect, other: Rect) -> bool {
    one.x0.abs_diff(other.x0) <= 6 && one.x1.abs_diff(other.x1) <= 6 && one.height().abs_diff(other.height()) <= 6
}

/// The same box, give or take a few pixels.
const fn same(one: Rect, other: Rect) -> bool {
    one.x0.abs_diff(other.x0) <= 6
        && one.y0.abs_diff(other.y0) <= 6
        && one.x1.abs_diff(other.x1) <= 6
        && one.y1.abs_diff(other.y1) <= 6
}
