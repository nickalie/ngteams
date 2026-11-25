//! QA of task 654 (round 1), in the same app window as the checks of task 653: Teams' own sticker packs in the
//! picker. The mock serves made-up pictures for Teams' sticker CDN: a blue disc for each pack's header, a red disc
//! for Teamsquatch "squashing a bug", a green disc for Teamsquatch "drinks too much coffee" (the PNG sticker that b
//! sends from another client) and a purple disc for every other sticker. Clippy "takes a nap" has no picture that
//! can be read. Every name, id and picture is made up, except the ids and names of Teams' own catalogue.

use super::{
    AB_CHAT, Duration, LAB_A, LAB_B, PICKER, Rect, Rgb, SEND_BUTTON, SandboxResult, TAB_STICKERS, User, Value,
    bare_faults, blobs, face_of, faults_to_result, has_item, items, json, picker, picker_open, pictures, position,
    rows, say, thread,
};
use teams_mock::ChatServiceMock;

/// The made-up colours of Teams' pictures: far from the colours of the user's stickers (`files::ALL`).
pub(super) const HEADER: Rgb = [40, 90, 200];
pub(super) const BUG: Rgb = [200, 40, 60];
pub(super) const COFFEE: Rgb = [110, 240, 160];
pub(super) const FACE: Rgb = [90, 70, 140];

const BUG_LABEL: &str = "teamsquatch squashing a bug";
const BUG_ITEM: &str = "sticker-teams-teamsquatch_bugsquash";
const BUG_SRC: &str = "evergreen-assets/stickerassets/teamsquatch-250x250/Teamsquatch_BugSquash.png?v=5";
const SUN_ITEM: &str = "sticker-teams-clippy_sun";
const SUN_SRC: &str = "evergreen-assets/stickerassets/clippy-250x250/en-us/Clippy_Sun.gif?v=5";
const NAP_ITEM: &str = "sticker-teams-clippy_nap";
const COFFEE_LABEL: &str = "teamsquatch drinks too much coffee";
const CAN_NOT_READ: &str = "That sticker can't be read. Check your connection, then try again.";
const CDN: &str = "https://teams.public.onecdn.static.microsoft/";

/// Teams' 18 packs for a work account, in Teams' order (the selectors).
const PACKS: [&str; 18] = [
    "popular",
    "clippy",
    "officedrama",
    "meme",
    "designers",
    "devs",
    "legal",
    "broccoli",
    "bee",
    "teamsquatch",
    "bummerbert",
    "catsinsuits",
    "wordart",
    "monkey",
    "octocorn",
    "donutfriends",
    "uncleblob",
    "animalopolis",
];
/// What Teams' search finds for "hello", in Teams' order (`.teamlead/shots/654-teams-search-hello.png`).
const HELLO: [&str; 6] = [
    "sticker-teams-clippy_hellothere",
    "sticker-teams-officedrama_hey",
    "sticker-teams-broccoli_wave",
    "sticker-teams-bee_hi",
    "sticker-teams-wordart_hello",
    "sticker-teams-animalopolis_weird_lemur",
];
/// Teams' stickers whose name or keywords hold "sun", in the packs' order.
const SUN: [&str; 4] = [
    SUN_ITEM,
    "sticker-teams-clippy_sunglasses",
    "sticker-teams-teamsquatch_weekendride",
    "sticker-teams-catsinsuits_sleepinsun",
];
/// The first four of Teams' Popular list: All's stickers when the user has no pack and no Recent.
const POPULAR: [&str; 4] = [
    "sticker-teams-officedrama_ohyeah",
    "sticker-teams-doge",
    "sticker-teams-devs_works_on_my_machine",
    "sticker-teams-officedrama_hey",
];

/// The made-up picture the mock's CDN gives for `path`, in the format its file name gives.
pub(super) fn teams_picture(path: &str) -> Vec<u8> {
    if let Some(template) = super::captions::template_picture(path) {
        return template;
    }
    if path.contains("/stickerassets/") && path.contains("Clippy_Nap") {
        return b"made-up bytes that are not a picture".to_vec();
    }
    let header = path.contains("_header") || path.contains("_Header");
    let colour = if header {
        HEADER
    } else if path.contains("BugSquash") {
        BUG
    } else if path.contains("CoffeeBuzz") {
        COFFEE
    } else {
        FACE
    };
    let side: u32 = if header {
        96
    } else if path.contains("/Preview/") {
        108
    } else {
        250
    };
    let centre = i64::from(side / 2);
    let radius = i64::from(side) * 5 / 12;
    let picture = image::RgbaImage::from_fn(side, side, |x, y| {
        let (dx, dy) = (i64::from(x) - centre, i64::from(y) - centre);
        if dx * dx + dy * dy <= radius * radius {
            image::Rgba([colour[0], colour[1], colour[2], 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    });
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or_default();
    let mut bytes = Vec::new();
    let mut out = std::io::Cursor::new(&mut bytes);
    let written = match extension {
        "gif" => picture.write_to(&mut out, image::ImageFormat::Gif),
        "jpg" | "jpeg" => image::DynamicImage::ImageRgba8(picture)
            .to_rgb8()
            .write_to(&mut out, image::ImageFormat::Jpeg),
        _ => picture.write_to(&mut out, image::ImageFormat::Png),
    };
    if let Err(error) = written {
        say(&format!("made-up Teams picture {path}: {error}"));
    }
    bytes
}

/// The mock serves Teams' sticker CDN, and b's other client sends a Teams PNG sticker in a new chat of a and b.
pub(super) fn seed(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult<String> {
    runtime.block_on(mock.serve_bytes_under("/evergreen-assets/funstuff/", teams_picture));
    runtime.block_on(mock.serve_bytes_under("/evergreen-assets/stickerassets/", teams_picture));
    let chat = mock
        .add_group("Lab picker teams", &[LAB_A, LAB_B])
        .ok_or("no chat made")?;
    let html = format!(
        "<p>&nbsp;</p><readonly aria-label=\"{COFFEE_LABEL}\" contenteditable=\"false\" \
         itemtype=\"http://schema.skype.com/Sticker\" title=\"{COFFEE_LABEL}\"><img \
         src=\"{CDN}evergreen-assets/stickerassets/teamsquatch-250x250/Teamsquatch_CoffeeBuzz.png?v=5\" \
         height=\"180\" alt=\"{COFFEE_LABEL}\" width=\"180\" data-image-type=\"standard\" data-image-mode=\"single\" \
         itemtype=\"http://schema.skype.com/Sticker\"></readonly><p>&nbsp;</p>"
    );
    mock.post_as(LAB_B, &chat, &html).ok_or("b's sticker refused")?;
    Ok(chat)
}

fn teams_items(row: &[String]) -> usize {
    row.iter().filter(|item| item.starts_with("sticker-teams-")).count()
}

/// The sticker items of the picker, in order.
fn sticker_order(state: &Value) -> Vec<String> {
    rows(state)
        .into_iter()
        .flatten()
        .filter(|item| item.starts_with("sticker-") && !item.starts_with("sticker-pack") && item != "sticker-back")
        .filter(|item| !item.starts_with("sticker-import") && !item.starts_with("sticker-rename"))
        .collect()
}

fn pack_items(state: &Value) -> Vec<String> {
    rows(state)
        .into_iter()
        .flatten()
        .filter(|item| item.starts_with("teams-pack-") || item.starts_with("sticker-pack-qa"))
        .collect()
}

fn shown_view(state: &Value) -> String {
    picker(state)["view"].as_str().unwrap_or_default().to_owned()
}

impl User<'_> {
    /// Types `query` in the picker's search field (the picker is open, its field focused by Ctrl+.).
    fn search(&mut self, query: &str) -> SandboxResult<Value> {
        self.open_picker_by_keys()?;
        self.type_text(query)?;
        self.until(&format!("the search for {query:?}"), |state| {
            sticker_order(state)
                .iter()
                .any(|item| item.starts_with("sticker-teams-"))
                || sticker_order(state).iter().any(|item| item.starts_with("sticker-qa"))
        })?;
        thread::sleep(Duration::from_millis(400));
        self.state()
    }

    /// The box holds one Teams sticker `label`, read from the CDN, and the picker is closed.
    fn teams_sticker_in_box(&mut self, label: &str) -> SandboxResult<Value> {
        self.until(&format!("{label} in the box"), |state| {
            let pictures = pictures(state);
            !picker_open(state)
                && pictures.len() == 1
                && pictures[0]["sticker"] == label
                && pictures[0]["state"] == "hosted"
        })
    }

    /// Send posts the box's Teams sticker: the message names it by its CDN address, with no AMS reference.
    fn send_teams_sticker(&mut self, src: &str, by_key: bool) -> SandboxResult<Vec<String>> {
        if by_key {
            self.ask("focus-box")?;
            self.press("enter")?;
        } else {
            self.click(SEND_BUTTON)?;
        }
        let body = self.wait_sent(src)?;
        self.until("the box empty after Send", |state| pictures(state).is_empty())?;
        let message: Value = serde_json::from_str(&body)?;
        let mut faults = Vec::new();
        let content = message["content"].as_str().unwrap_or_default();
        if !content.contains(&format!("src=\"{CDN}{src}\"")) || !content.contains("schema.skype.com/Sticker") {
            faults.push(format!("the sticker was not sent by its CDN address: {content}"));
        }
        if message["amsreferences"] != json!([]) {
            faults.push(format!("amsreferences is {}, not []", message["amsreferences"]));
        }
        if !content.contains("height=\"180\"") || !content.contains("width=\"180\"") {
            faults.push(format!("the sticker is not sent at 180 by 180: {content}"));
        }
        Ok(faults)
    }
}

/// Before the imports (no pack of the user's, no Recent): All shows Teams' first four Popular stickers.
pub(super) fn teams_all_before_imports(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let state = user.open_picker_by_keys()?;
    let order = sticker_order(&state);
    let picture = user.image("teams-all-no-packs")?;
    let mut faults = Vec::new();
    if order != POPULAR {
        faults.push(format!(
            "All's stickers with no pack and no Recent: {order:?}, expected {POPULAR:?}"
        ));
    }
    if blobs(&picture, FACE, PICKER, 150).len() < 4 {
        faults.push("All does not draw the four Popular previews".to_owned());
    }
    faults_to_result(&faults)
}

/// The Stickers tab: the user's packs, then Teams' 18 packs in Teams' order with their header pictures; See all; a
/// pack opened by a click on its tile, in 3 columns, in Teams Light and Dark.
pub(super) fn teams_packs_listed(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    user.ask("focus-box")?;
    user.click(super::EMOJI_BUTTON)?;
    user.until("the picker opened by its button", picker_open)?;
    user.click(TAB_STICKERS)?;
    let state = user.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")?;
    let mut faults = Vec::new();
    let home = pack_items(&state);
    say(&format!("  Stickers tab packs: {home:?}"));
    let first_teams = home.iter().position(|item| item.starts_with("teams-pack-"));
    let last_own = home.iter().rposition(|item| item.starts_with("sticker-pack-qa"));
    if first_teams.is_none() || last_own.is_none_or(|own| first_teams.is_some_and(|teams| own > teams)) {
        faults.push(format!("the user's packs do not come before Teams' packs: {home:?}"));
    }
    // with Recent, the packs are below it: the keyboard brings Teams' first pack into view
    user.key_to("teams-pack-popular")?;
    let picture = user.image("teams-stickers-tab")?;
    let headers = blobs(&picture, HEADER, PICKER, 150).len();
    if headers == 0 {
        faults.push("no header picture of Teams' packs on the Stickers tab".to_owned());
    }
    user.key_pick("sticker-packs-see-all")?;
    let state = user.until("See all", |state| has_item(state, "teams-pack-animalopolis"))?;
    let teams: Vec<String> = pack_items(&state)
        .into_iter()
        .filter_map(|item| item.strip_prefix("teams-pack-").map(str::to_owned))
        .collect();
    if teams != PACKS {
        faults.push(format!("Teams' packs in See all: {teams:?}, expected {PACKS:?}"));
    }
    let picture = user.image("teams-see-all")?;
    let headers = blobs(&picture, HEADER, PICKER, 150).len();
    say(&format!("  header pictures drawn in See all: {headers}"));
    if headers < 4 {
        faults.push(format!("See all draws {headers} header pictures"));
    }
    // each of the 18 packs, opened by the keyboard from See all and closed by Back
    for id in PACKS {
        user.key_pick(&format!("teams-pack-{id}"))?;
        let state = user.until(&format!("pack {id} open"), |state| {
            shown_view(state)
                .to_lowercase()
                .contains(&format!("teamspack {{ id: \"{id}\""))
        })?;
        faults.extend(columns_faults(&state, id));
        user.key_pick("sticker-back")?;
        user.until("See all again", |state| shown_view(state) == "Packs")?;
    }
    // a click on a header picture in view opens that pack
    let picture = user.image("teams-see-all-end")?;
    let tile = biggest(&picture, HEADER).ok_or("no header picture to click")?;
    user.click(tile.centre())?;
    let state = user.until("a Teams pack opened by a click", |state| {
        shown_view(state).contains("TeamsPack")
    })?;
    faults.extend(columns_faults(&state, &shown_view(&state)));
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        let picture = user.image(&format!("teams-pack-open-{theme}"))?;
        if blobs(&picture, FACE, PICKER, 150).len() < 3 {
            faults.push(format!("{theme}: the opened pack draws fewer than 3 previews"));
        }
    }
    user.ask("theme-light")?;
    faults_to_result(&faults)
}

/// The largest disc of `colour` on the screen.
fn biggest(image: &image::RgbImage, colour: Rgb) -> Option<Rect> {
    blobs(image, colour, PICKER, 150)
        .into_iter()
        .max_by_key(|blob| blob.w * blob.h)
}

/// A Teams pack's stickers in rows of 3 at most, the full rows of 3.
fn columns_faults(state: &Value, view: &str) -> Vec<String> {
    let sticker_rows: Vec<usize> = rows(state)
        .iter()
        .map(|row| teams_items(row))
        .filter(|count| *count > 0)
        .collect();
    let mut faults = Vec::new();
    if sticker_rows.is_empty() {
        faults.push(format!("{view}: no sticker in the pack"));
    }
    let (last, full) = sticker_rows.split_last().unwrap_or((&0, &[]));
    if full.iter().any(|count| *count != 3) || *last > 3 {
        faults.push(format!(
            "{view}: the rows hold {sticker_rows:?} stickers, not 3 columns"
        ));
    }
    faults
}

/// a opens Teamsquatch by the keyboard, clicks "squashing a bug": it goes in the box at 180; Send posts it by its
/// CDN address with no AMS reference.
pub(super) fn teams_pointer_pick_and_send(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    user.open_stickers_by_keys()?;
    user.key_pick("sticker-packs-see-all")?;
    user.until("See all", |state| has_item(state, "teams-pack-teamsquatch"))?;
    user.key_pick("teams-pack-teamsquatch")?;
    let state = user.until("Teamsquatch open", |state| shown_view(state).contains("Teamsquatch"))?;
    let mut faults = columns_faults(&state, "Teamsquatch");
    let picture = user.image("teams-teamsquatch")?;
    let bug = face_of(&picture, BUG, PICKER).ok_or("no preview of squashing a bug in the pack")?;
    user.click(bug.centre())?;
    let state = user.teams_sticker_in_box(BUG_LABEL)?;
    if pictures(&state)[0]["given"] != json!([180, 180]) {
        faults.push(format!("the Teams sticker in the box: {}", pictures(&state)[0]));
    }
    let picture = user.image("teams-bug-in-box")?;
    if face_of(&picture, BUG, Rect::whole_screen()).is_none() {
        faults.push("the box does not draw the sticker".to_owned());
    }
    faults.extend(user.send_teams_sticker(BUG_SRC, false)?);
    let state = user.until("a's sticker in the chat", |state| {
        items(state)
            .iter()
            .any(|item| item["label"] == BUG_LABEL && item["own"] == true)
    })?;
    if let Some(item) = items(&state).iter().rev().find(|item| item["label"] == BUG_LABEL)
        && (item["sticker_alone"] != true || item["bare"] != true)
    {
        faults.push(format!("a's sent Teams sticker is not alone with no bubble: {item}"));
    }
    user.image("teams-bug-sent-a")?;
    faults_to_result(&faults)
}

/// b sees a's Teams sticker alone at 180 px with no bubble, in Teams Light and Dark.
pub(super) fn teams_b_sees_sent(user: &mut User<'_>) -> SandboxResult {
    user.switch("b")?;
    user.open(AB_CHAT)?;
    let state = user.until("a's Teams sticker in b's chat", |state| {
        items(state).iter().any(|item| item["label"] == BUG_LABEL)
    })?;
    let mut faults = Vec::new();
    match items(&state).iter().rev().find(|item| item["label"] == BUG_LABEL) {
        Some(item) if item["own"] == false && item["sticker_alone"] == true && item["bare"] == true => {}
        other => faults.push(format!("b's item of a's Teams sticker: {other:?}")),
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        faults.extend(user.drawn(
            &format!("teams-b-sees-{theme}"),
            super::MESSAGES,
            |image| match face_of(image, BUG, super::MESSAGES) {
                Some(face) => bare_faults(image, face, &format!("{theme} b's Teams sticker")),
                None => vec![format!("{theme}: b does not see the Teams sticker")],
            },
        )?);
    }
    user.ask("theme-light")?;
    faults_to_result(&faults)
}

/// A PNG Teams sticker that b sent from another client shows for a, alone at 180 px with no bubble.
pub(super) fn teams_png_from_other_client(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.teams_chat.to_owned();
    let state = user.open(&chat)?;
    let state = if items(&state).iter().any(|item| item["label"] == COFFEE_LABEL) {
        state
    } else {
        user.until("b's PNG sticker in a's chat", |state| {
            items(state).iter().any(|item| item["label"] == COFFEE_LABEL)
        })?
    };
    let mut faults = Vec::new();
    match items(&state).iter().find(|item| item["label"] == COFFEE_LABEL) {
        Some(item) if item["sticker_alone"] == true && item["bare"] == true => {}
        other => faults.push(format!("a's item of b's PNG sticker: {other:?}")),
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        faults.extend(user.drawn(
            &format!("teams-png-from-b-{theme}"),
            super::MESSAGES,
            |image| match face_of(image, COFFEE, super::MESSAGES) {
                Some(face) => bare_faults(image, face, &format!("{theme} b's PNG sticker")),
                None => vec![format!("{theme}: a does not see b's PNG sticker")],
            },
        )?);
    }
    user.ask("theme-light")?;
    faults_to_result(&faults)
}

/// The search: "hello" finds Teams' six in Teams' order; "sun" finds the user's sticker first, then Teams' four;
/// the keyboard picks Clippy "gets some sun", Enter sends it by its CDN address.
pub(super) fn teams_search_and_keys(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.teams_chat.to_owned();
    user.open(&chat)?;
    let mut faults = Vec::new();
    let state = user.search("hello")?;
    let found = sticker_order(&state);
    if found != HELLO {
        faults.push(format!("the search for hello: {found:?}, expected {HELLO:?}"));
    }
    user.image("teams-search-hello")?;
    user.ask("clear")?;
    let state = user.search("sun")?;
    let found = sticker_order(&state);
    let own: Vec<&String> = found.iter().filter(|item| item.starts_with("sticker-qa")).collect();
    let teams: Vec<&String> = found.iter().filter(|item| item.starts_with("sticker-teams-")).collect();
    let first_teams = found.iter().position(|item| item.starts_with("sticker-teams-"));
    if own.is_empty() || first_teams.is_none_or(|first| first < own.len()) || teams != SUN {
        faults.push(format!(
            "the search for sun: {found:?}; the user's sticker, then {SUN:?} expected"
        ));
    }
    user.image("teams-search-sun")?;
    user.key_pick(SUN_ITEM)?;
    user.teams_sticker_in_box("Clippy gets some sun")?;
    faults.extend(user.send_teams_sticker(SUN_SRC, true)?);
    faults_to_result(&faults)
}

/// A Teams sticker whose picture cannot be read: the box says so and holds no picture; Send sends nothing.
pub(super) fn teams_unreadable_sticker(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.teams_chat.to_owned();
    user.open(&chat)?;
    user.search("nap")?;
    let posts_before = user.sent_with("POST", "Clippy_Nap").len();
    user.key_pick(NAP_ITEM)?;
    let state = user.until("the problem line", |state| !state["compose"]["problem"].is_null())?;
    let mut faults = Vec::new();
    if state["compose"]["problem"] != CAN_NOT_READ {
        faults.push(format!(
            "the box says {}, expected {CAN_NOT_READ:?}",
            state["compose"]["problem"]
        ));
    }
    if !pictures(&state).is_empty() {
        faults.push(format!("the box holds {:?}", pictures(&state)));
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.image(&format!("teams-unreadable-{theme}"))?;
    }
    user.ask("theme-light")?;
    user.click(SEND_BUTTON)?;
    thread::sleep(Duration::from_secs(1));
    if user.sent_with("POST", "Clippy_Nap").len() != posts_before {
        faults.push("Send posted the sticker that cannot be read".to_owned());
    }
    faults_to_result(&faults)
}

/// After the sends: All starts with Recent (the last Teams sticker sent first); the Stickers tab's Recent has both.
pub(super) fn teams_all_after_send(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let state = user.open_picker_by_keys()?;
    let order = sticker_order(&state);
    let mut faults = Vec::new();
    if order.first().map(String::as_str) != Some(SUN_ITEM) || order.get(1).map(String::as_str) != Some(BUG_ITEM) {
        faults.push(format!(
            "All's stickers after the sends: {order:?}, Recent first expected"
        ));
    }
    user.image("teams-all-after-send")?;
    user.press("ctrl+tab")?;
    user.press("ctrl+tab")?;
    let state = user.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")?;
    faults.extend(recent_faults(&state));
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.image(&format!("teams-stickers-recent-{theme}"))?;
    }
    user.ask("theme-light")?;
    faults_to_result(&faults)
}

/// The Stickers tab's Recent holds the two Teams stickers sent, before any pack.
fn recent_faults(state: &Value) -> Vec<String> {
    let rows = rows(state);
    let first_pack = rows.iter().position(|row| {
        row.iter()
            .any(|item| item.starts_with("teams-pack-") || item.starts_with("sticker-pack-qa"))
    });
    let mut faults = Vec::new();
    for item in [SUN_ITEM, BUG_ITEM] {
        match (position(&rows, item), first_pack) {
            (Some((row, _)), Some(pack)) if row < pack => {}
            other => faults.push(format!(
                "{item} is not in Recent before the packs: {other:?} in {rows:?}"
            )),
        }
    }
    faults
}

/// After a restart of the app: Recent still holds a's Teams stickers.
pub(super) fn teams_recent_after_restart(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let state = user.open_stickers_by_keys()?;
    user.image("teams-recent-after-restart")?;
    faults_to_result(&recent_faults(&state))
}

/// An own-server account: no Teams pack on the Stickers tab or in See all, no Teams sticker in Recent, All or a
/// search; the user's packs stay.
pub(super) fn own_server_only_own_packs(user: &mut User<'_>) -> SandboxResult {
    user.open(AB_CHAT)?;
    let mut faults = Vec::new();
    let state = user.open_picker_by_keys()?;
    let all = sticker_order(&state);
    if all.iter().any(|item| item.starts_with("sticker-teams-")) || all.is_empty() {
        faults.push(format!("All on an own server: {all:?}"));
    }
    user.press("ctrl+tab")?;
    user.press("ctrl+tab")?;
    let state = user.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")?;
    let shown: Vec<String> = rows(&state).into_iter().flatten().collect();
    if shown
        .iter()
        .any(|item| item.starts_with("teams-pack-") || item.starts_with("sticker-teams-"))
    {
        faults.push(format!(
            "the own server's Stickers tab shows Teams' stickers: {shown:?}"
        ));
    }
    if !shown.iter().any(|item| item == "sticker-pack-qa-zorblets") {
        faults.push(format!(
            "the own server's Stickers tab has no pack of the user's: {shown:?}"
        ));
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.image(&format!("own-server-stickers-{theme}"))?;
    }
    user.ask("theme-light")?;
    if has_item(&state, "sticker-packs-see-all") {
        user.key_pick("sticker-packs-see-all")?;
        thread::sleep(Duration::from_millis(500));
        let state = user.state()?;
        if pack_items(&state).iter().any(|item| item.starts_with("teams-pack-")) {
            faults.push(format!("See all on an own server: {:?}", pack_items(&state)));
        }
    }
    user.press("escape")?;
    user.ask("clear")?;
    user.open_picker_by_keys()?;
    user.type_text("sun")?;
    thread::sleep(Duration::from_secs(1));
    let state = user.state()?;
    let found = sticker_order(&state);
    if found.iter().any(|item| item.starts_with("sticker-teams-")) || found.is_empty() {
        faults.push(format!("the search for sun on an own server: {found:?}"));
    }
    user.image("own-server-search-sun")?;
    faults_to_result(&faults)
}
