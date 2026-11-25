//! QA of task 650 (round 1), in the same app window as the checks of tasks 653 and 654: Teams' "Edit sticker". The
//! mock's sticker CDN gives each caption template as a made-up picture of 334 by 250 px: a cream ground with a teal
//! disc at the right. A caption's ink is every pixel of the sent picture that is neither of the two. Every name,
//! text and picture is made up, except the ids, names and caption data of Teams' own catalogue.

use std::collections::BTreeSet;

use image::RgbaImage;
use teams_ui::stickers::caption::{self, Caption, Words};
use teams_ui::stickers::teams;

use super::{
    EMOJI_BUTTON, LAB_A, LAB_B, PICKER, Rect, Rgb, SEND_BUTTON, SandboxResult, TAB_STICKERS, User, Value, blobs,
    face_of, faults_to_result, has_item, items, json, near, picker, picker_open, pictures, pixel, say, thread,
};
use teams_mock::ChatServiceMock;

/// The made-up template: its ground, its disc and its size.
const GROUND: Rgb = [236, 224, 178];
const DISC: Rgb = [40, 150, 150];
const TEMPLATE: (u32, u32) = (334, 250);
/// The ink tolerance: the edge of a letter blends with the ground.
const INK: u8 = 40;
/// Teams Light's foreground: the colour of the pencil mark's disc.
const MARK: Rgb = [0x24, 0x24, 0x24];
const BRAND: Rgb = [0x5b, 0x5f, 0xc7];

const SO_COOL: &str = "OfficeDrama_SoCool";
const MEME: &str = "grumpycat";
const UNCAPTIONED_PACK: &str = "teamsquatch";
const EDITABLE: [&str; 5] = ["officedrama", "meme", "designers", "devs", "legal"];
const CDN: &str = "https://teams.public.onecdn.static.microsoft/";

/// The paths of every caption template on the mock's CDN, with no query.
fn template_paths() -> &'static BTreeSet<String> {
    static PATHS: std::sync::OnceLock<BTreeSet<String>> = std::sync::OnceLock::new();
    PATHS.get_or_init(|| {
        let catalog = teams::catalog();
        catalog
            .packs()
            .iter()
            .flat_map(|pack| catalog.stickers_of(&pack.id))
            .filter_map(|sticker| catalog.template(sticker))
            .map(|address| path_of(&address))
            .collect()
    })
}

/// The path of a CDN `address`, with no host, leading slash or query.
fn path_of(address: &str) -> String {
    let path = address.strip_prefix(CDN).unwrap_or(address);
    path.split('?')
        .next()
        .unwrap_or(path)
        .trim_start_matches('/')
        .to_owned()
}

/// The made-up template for `path`, when it is the path of a caption template.
pub(super) fn template_picture(path: &str) -> Option<Vec<u8>> {
    if !template_paths().contains(path.trim_start_matches('/')) {
        return None;
    }
    let picture = RgbaImage::from_fn(TEMPLATE.0, TEMPLATE.1, |x, y| {
        let (dx, dy) = (i64::from(x) - 285, i64::from(y) - 125);
        let colour = if dx * dx + dy * dy <= 40 * 40 { DISC } else { GROUND };
        image::Rgba([colour[0], colour[1], colour[2], 255])
    });
    let mut bytes = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .ok()?;
    Some(bytes)
}

/// The mock's chat of a and b for the captioned stickers.
pub(super) fn seed(mock: &ChatServiceMock) -> SandboxResult<String> {
    let chat = mock
        .add_group("Lab picker captions", &[LAB_A, LAB_B])
        .ok_or("no chat made")?;
    mock.post_as(LAB_B, &chat, "<p>Made-up words before the captioned stickers</p>")
        .ok_or("b's words refused")?;
    Ok(chat)
}

fn caption_of(id: &str) -> SandboxResult<Caption> {
    teams::catalog()
        .sticker(id)
        .and_then(|sticker| sticker.caption.clone())
        .ok_or_else(|| format!("no caption for {id}").into())
}

fn region_of(caption: &Caption) -> Option<[u32; 4]> {
    match &caption.words {
        Words::Block { region, .. } => Some(*region),
        Words::Meme => None,
    }
}

fn editor(state: &Value) -> &Value {
    &picker(state)["editor"]
}

fn editor_open(state: &Value) -> bool {
    !editor(state).is_null()
}

fn selector_of(id: &str) -> String {
    format!("sticker-teams-{}", id.to_lowercase())
}

/// The pixels of the caption's ink in `picture`: neither the ground nor the disc.
fn ink(picture: &RgbaImage) -> Vec<(u32, u32, [u8; 3])> {
    picture
        .enumerate_pixels()
        .filter_map(|(x, y, pixel)| {
            let [r, g, b, _] = pixel.0;
            let colour = [r, g, b];
            (!near(colour, GROUND, INK) && !near(colour, DISC, INK)).then_some((x, y, colour))
        })
        .collect()
}

/// The rows of `picture` with ink, as runs: the caption's lines.
fn ink_lines(points: &[(u32, u32, [u8; 3])]) -> usize {
    let rows: BTreeSet<u32> = points.iter().map(|(_, y, _)| *y).collect();
    let mut lines = 0;
    let mut last = None;
    for row in rows {
        if last.is_none_or(|before: u32| row > before + 1) {
            lines += 1;
        }
        last = Some(row);
    }
    lines
}

/// A block caption drawn in `picture`: some ink, all of it in `region` (2 px of anti-aliasing round it).
fn block_faults(picture: &RgbaImage, region: [u32; 4], what: &str) -> Vec<String> {
    let points = ink(picture);
    let mut faults = Vec::new();
    if points.len() < 50 {
        faults.push(format!(
            "{what}: the picture holds no caption ({} ink px)",
            points.len()
        ));
    }
    let [left, top, right, bottom] = region;
    let outside: Vec<(u32, u32)> = points
        .iter()
        .filter(|(x, y, _)| *x + 2 < left || *x > right + 2 || *y + 2 < top || *y > bottom + 2)
        .map(|(x, y, _)| (*x, *y))
        .collect();
    if !outside.is_empty() {
        faults.push(format!(
            "{what}: {} ink px outside the region {region:?}, the first {:?}",
            outside.len(),
            &outside[..outside.len().min(5)]
        ));
    }
    faults
}

impl User<'_> {
    /// The media service answers the next picture send as object `object`, and keeps its bytes for the check.
    fn expect_upload(&mut self) -> String {
        self.uploads += 1;
        let object = format!("0-eus-d1-{:032x}", 0x0065_0000 + self.uploads);
        let mock = self.world.mock;
        self.world.runtime.block_on(async {
            mock.answer_next("POST", "/v1/objects/", 201, &json!({ "id": object }))
                .await;
            mock.refuse_next("PUT", &format!("/v1/objects/{object}/content/imgpsh"), 201)
                .await;
        });
        object
    }

    /// The bytes the app put into `object`; the media service then serves them, as it gives the picture to b.
    fn uploaded(&mut self, object: &str) -> SandboxResult<RgbaImage> {
        let path = format!("/v1/objects/{object}/content/imgpsh");
        let bytes = super::wait_until(super::WAIT, || {
            self.world
                .runtime
                .block_on(self.world.mock.received_requests())
                .into_iter()
                .find(|request| request.method.as_str() == "PUT" && request.url.path().ends_with(&path))
                .map(|request| request.body)
        })
        .ok_or_else(|| format!("the app put no bytes into {object}"))?;
        let mock = self.world.mock;
        let served = bytes.clone();
        self.world.runtime.block_on(async {
            mock.serve_bytes(&format!("/v1/objects/{object}/views/imgo"), "image/png", served)
                .await;
        });
        ensure_png(&bytes)?;
        Ok(image::load_from_memory(&bytes)?.to_rgba8())
    }

    /// The Stickers tab of the box's picker, opened by the emoji button and the tab's click.
    fn open_stickers_by_pointer(&mut self) -> SandboxResult<Value> {
        self.ask("focus-box")?;
        self.click(EMOJI_BUTTON)?;
        self.until("the picker opened by its button", picker_open)?;
        self.click(TAB_STICKERS)?;
        self.until("the Stickers tab", |state| picker(state)["tab"] == "Stickers")
    }

    /// Opens Teams' pack `id` from the Stickers tab by the keyboard.
    fn open_teams_pack(&mut self, id: &str) -> SandboxResult<Value> {
        self.key_pick("sticker-packs-see-all")?;
        self.until("See all", |state| has_item(state, &format!("teams-pack-{id}")))?;
        self.key_pick(&format!("teams-pack-{id}"))?;
        self.until(&format!("pack {id} open"), |state| {
            picker(state)["view"]
                .as_str()
                .is_some_and(|view| view.to_lowercase().contains(&format!("teamspack {{ id: \"{id}\"")))
        })
    }

    /// The editor of Teams' sticker `id`, opened by the keyboard from the Stickers tab (the search finds it).
    fn open_editor_by_keys(&mut self, id: &str, query: &str) -> SandboxResult<Value> {
        if picker_open(&self.state()?) {
            self.press("escape")?;
            self.until("the picker closed", |state| !picker_open(state))?;
        }
        self.open_picker_by_keys()?;
        self.type_text(query)?;
        let selector = selector_of(id);
        self.until(&format!("the search for {query:?}"), |state| has_item(state, &selector))?;
        self.key_pick(&selector)?;
        self.editor_ready(id)
    }

    /// The editor of `id` shows, its template read and drawn.
    fn editor_ready(&mut self, id: &str) -> SandboxResult<Value> {
        self.until(&format!("the editor of {id}, drawn"), |state| {
            editor(state)["sticker"] == id && !editor(state)["drawn"].is_null()
        })
    }

    /// The editor closed back to the stickers, with the picker still open and the focus in it.
    fn editor_closed(&mut self, what: &str) -> SandboxResult<Value> {
        self.until(what, |state| {
            picker_open(state) && !editor_open(state) && picker(state)["focus"] == true
        })
    }

    /// The editor's Done button on the screen: the brand-filled button below the picture.
    fn done_button(&mut self, name: &str) -> SandboxResult<Rect> {
        let image = self.image(name)?;
        let lower = Rect {
            y: PICKER.y + PICKER.h / 2,
            h: PICKER.h / 2,
            ..PICKER
        };
        blobs(&image, BRAND, lower, 800)
            .into_iter()
            .max_by_key(|blob| blob.w * blob.h)
            .ok_or_else(|| format!("{name}: no Done button drawn").into())
    }

    /// The editor's picture on the screen: the template's ground.
    fn shown_template(&mut self, name: &str) -> SandboxResult<(Rect, image::RgbImage)> {
        let image = self.image(name)?;
        let ground = face_of(&image, GROUND, PICKER).ok_or_else(|| format!("{name}: the editor draws no picture"))?;
        Ok((ground, image))
    }
}

fn ensure_png(bytes: &[u8]) -> SandboxResult {
    super::ensure(bytes.starts_with(b"\x89PNG"), || {
        "the uploaded bytes are not a PNG".to_owned()
    })
}

/// The pencil marks drawn in `area`: small dark discs.
fn marks(image: &image::RgbImage, area: Rect) -> Vec<Rect> {
    blobs(image, MARK, area, 80)
        .into_iter()
        .filter(|blob| (12..=19).contains(&blob.w) && (12..=19).contains(&blob.h))
        .collect()
}

/// The pixels that differ in `area` of two shots.
fn changed(before: &image::RgbImage, after: &image::RgbImage, area: Rect) -> usize {
    let mut count = 0;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let (Some(one), Some(other)) = (pixel(before, x, y), pixel(after, x, y))
                && !near(one, other, 24)
            {
                count += 1;
            }
        }
    }
    count
}

/// The mock's templates are only the templates: no preview, and no picture of a sticker that is not a meme, shares a
/// path with one.
pub(super) fn templates_apart() -> SandboxResult {
    let catalog = teams::catalog();
    let templates = template_paths();
    let shared: Vec<String> = catalog
        .packs()
        .iter()
        .flat_map(|pack| catalog.stickers_of(&pack.id))
        // a meme is sent only as its drawn template, so its picture's address is the template's
        .flat_map(|sticker| {
            let meme = sticker.caption.as_ref().is_some_and(Caption::is_meme);
            [
                (!meme).then(|| catalog.picture(sticker)),
                Some(catalog.preview(sticker)),
            ]
        })
        .flatten()
        .map(|address| path_of(&address))
        .filter(|path| templates.contains(path))
        .collect();
    super::ensure(shared.is_empty(), || {
        format!("a template is also a sticker's picture: {shared:?}")
    })
}

/// Pencil marks on Popular's captioned stickers and on the five editable packs, none on Teamsquatch; a click on a
/// captioned sticker opens the editor with its own words; the typed caption redraws the picture; Done puts it in the
/// box at 240 by 180, sent to the media service; Send posts one sticker message; Teams Light and Dark.
pub(super) fn caption_pointer_send(user: &mut User<'_>) -> SandboxResult {
    templates_apart()?;
    let chat = user.world.captions_chat.to_owned();
    user.open(&chat)?;
    let mut faults = Vec::new();
    user.open_stickers_by_pointer()?;
    user.key_pick("sticker-packs-see-all")?;
    user.until("See all", |state| has_item(state, "teams-pack-officedrama"))?;
    let image = user.image("caption-see-all-marks")?;
    let shown = marks(&image, PICKER).len();
    say(&format!("  pencil marks in See all: {shown}"));
    if shown == 0 {
        faults.push("See all draws no pencil mark on an editable pack".to_owned());
    }
    let editable: Vec<String> = teams::catalog()
        .packs()
        .iter()
        .filter(|pack| teams::catalog().editable(&pack.id))
        .map(|pack| pack.id.to_lowercase())
        .collect();
    if editable != EDITABLE {
        faults.push(format!("the editable packs are {editable:?}, expected {EDITABLE:?}"));
    }
    user.press("escape")?;
    user.ask("clear")?;
    user.open_stickers_by_keys()?;
    user.open_teams_pack(UNCAPTIONED_PACK)?;
    let image = user.image("caption-teamsquatch-no-marks")?;
    let shown = marks(&image, PICKER).len();
    if shown != 0 {
        faults.push(format!("Teamsquatch (no caption) draws {shown} pencil marks"));
    }
    user.press("escape")?;
    user.ask("clear")?;
    user.open_stickers_by_pointer()?;
    user.key_pick("teams-pack-popular")?;
    user.until("Popular open", |state| {
        picker(state)["view"]
            .as_str()
            .is_some_and(|view| view.contains("Popular"))
    })?;
    let image = user.image("caption-popular-marks")?;
    let tiles: Vec<Rect> = blobs(&image, super::teams::FACE, PICKER, 150)
        .into_iter()
        .filter(|tile| tile.w.abs_diff(tile.h) <= 4 && tile.w > 40)
        .collect();
    let shown = marks(&image, PICKER).len();
    say(&format!("  Popular: {} whole tiles, {shown} pencil marks", tiles.len()));
    if tiles.is_empty() || shown < tiles.len() {
        faults.push(format!("Popular: {} whole tiles but {shown} pencil marks", tiles.len()));
    }
    let first = tiles
        .iter()
        .min_by_key(|tile| (tile.y / 20, tile.x))
        .copied()
        .ok_or("no Popular tile to click")?;
    user.click(first.centre())?;
    let state = user.until("the editor opened by a click", editor_open)?;
    let id = editor(&state)["sticker"].as_str().unwrap_or_default().to_owned();
    let words = caption_of(&id)?.first_text();
    if editor(&state)["typed"] != json!(words) {
        faults.push(format!(
            "the editor of {id} starts with {}, expected {words:?}",
            editor(&state)["typed"]
        ));
    }
    user.press("escape")?;
    user.editor_closed("the editor closed by Escape")?;
    user.press("escape")?;
    user.ask("clear")?;
    faults.extend(type_done_send(user, &chat)?);
    faults_to_result(&faults)
}

/// So cool: the typed caption redraws the picture; Done by the pointer puts it in the box, Send by the pointer posts it.
fn type_done_send(user: &mut User<'_>, chat: &str) -> SandboxResult<Vec<String>> {
    let mut faults = Vec::new();
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    let (before_area, before) = user.shown_template("caption-editor-first")?;
    say(&format!("  the editor's picture: {before_area:?}"));
    user.press("ctrl+a")?;
    user.type_text("Go team qa")?;
    let state = user.until("the typed caption", |state| {
        editor(state)["typed"] == json!(["Go team qa"])
    })?;
    if editor(&state)["can_finish"] != true {
        faults.push(format!("Done is off with a caption: {}", editor(&state)));
    }
    thread::sleep(super::SETTLE);
    let (_, after) = user.shown_template("caption-editor-typed")?;
    let redrawn = changed(&before, &after, before_area);
    say(&format!("  px of the picture redrawn: {redrawn}"));
    if redrawn < 100 {
        faults.push(format!("the picture did not change with the caption ({redrawn} px)"));
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.image(&format!("caption-editor-{theme}"))?;
    }
    user.ask("theme-light")?;
    let object = user.expect_upload();
    let done = user.done_button("caption-editor-done")?;
    user.click(done.centre())?;
    let state = user.until("the captioned sticker in the box, sent", |state| {
        let pictures = pictures(state);
        !picker_open(state) && pictures.len() == 1 && pictures[0]["state"] == "sent"
    })?;
    let picture = &pictures(&state)[0];
    if picture["sticker"] != "Go team qa" || picture["given"] != json!([240, 180]) {
        faults.push(format!("the captioned sticker in the box: {picture}"));
    }
    if picture["alt"] != "Sticker image, Go team qa" {
        faults.push(format!("the box's picture words: {}", picture["alt"]));
    }
    let drawn = user.uploaded(&object)?;
    if drawn.dimensions() != TEMPLATE {
        faults.push(format!(
            "the uploaded picture is {:?}, not the template's {TEMPLATE:?}",
            drawn.dimensions()
        ));
    }
    let region = region_of(&caption_of(SO_COOL)?).ok_or("So cool has no region")?;
    faults.extend(block_faults(&drawn, region, "Go team qa"));
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.image(&format!("caption-in-box-{theme}"))?;
    }
    user.ask("theme-light")?;
    user.click(SEND_BUTTON)?;
    let body = user.wait_sent("Sticker image, Go team qa")?;
    user.until("the box empty after Send", |state| pictures(state).is_empty())?;
    faults.extend(sent_faults(user, &body, &object, "Go team qa", chat));
    user.until("a's captioned sticker in the chat", |state| {
        items(state)
            .iter()
            .any(|item| item["own"] == true && item_names(item, "Go team qa"))
    })?;
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.image(&format!("caption-sent-a-{theme}"))?;
    }
    user.ask("theme-light")?;
    Ok(faults)
}

/// Whether a chat item is a sticker named by `words`.
fn item_names(item: &Value, words: &str) -> bool {
    item["label"].as_str().is_some_and(|label| label.contains(words))
}

/// The posted message: one sticker message, its picture the media service object, at 240 by 180, named by the words.
fn sent_faults(user: &User<'_>, body: &str, object: &str, words: &str, chat: &str) -> Vec<String> {
    let mut faults = Vec::new();
    let posts = user.sent_with("POST", &format!("Sticker image, {words}"));
    if posts.len() != 1 {
        faults.push(format!(
            "{} messages posted with the caption {words:?}, not 1",
            posts.len()
        ));
    }
    let Ok(message) = serde_json::from_str::<Value>(body) else {
        return vec![format!("the posted message is not JSON: {body}")];
    };
    let content = message["content"].as_str().unwrap_or_default();
    for part in [
        "schema.skype.com/Sticker",
        "width=\"240\"",
        "height=\"180\"",
        &format!("aria-label=\"{words}\""),
        object,
    ] {
        if !content.contains(part) {
            faults.push(format!("the posted sticker lacks {part}: {content}"));
        }
    }
    if !message["amsreferences"].to_string().contains(object) {
        faults.push(format!(
            "amsreferences is {}, not the object {object}",
            message["amsreferences"]
        ));
    }
    let sent_to = user.sent_with("POST", object).len();
    if sent_to == 0 {
        faults.push(format!("no message of {chat} names {object}"));
    }
    faults
}

/// b sees a's captioned sticker alone with no bubble, its picture the drawn caption, in Teams Light and Dark.
pub(super) fn caption_b_sees(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.captions_chat.to_owned();
    user.switch("b")?;
    user.open(&chat)?;
    let state = user.until("a's captioned sticker in b's chat", |state| {
        items(state).iter().any(|item| item_names(item, "Go team qa"))
    })?;
    let mut faults = Vec::new();
    match items(&state).iter().rev().find(|item| item_names(item, "Go team qa")) {
        Some(item) if item["own"] == false && item["sticker_alone"] == true && item["bare"] == true => {}
        other => faults.push(format!("b's item of a's captioned sticker: {other:?}")),
    }
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        faults.extend(
            user.drawn(&format!("caption-b-sees-{theme}"), super::MESSAGES, |image| {
                match face_of(image, GROUND, super::MESSAGES) {
                    Some(ground) => {
                        let mut faults = picture_bare_faults(image, ground, &format!("{theme} b's captioned sticker"));
                        // the ground is the picture less its disc and caption: about 240 by 180
                        if !(200..=242).contains(&ground.w) || !(150..=182).contains(&ground.h) {
                            faults.push(format!("{theme}: b's captioned sticker is drawn at {ground:?}"));
                        }
                        faults
                    }
                    None => vec![format!("{theme}: b does not see the captioned sticker")],
                }
            })?,
        );
    }
    user.ask("theme-light")?;
    faults_to_result(&faults)
}

/// The drawn picture `ground` is bare: right outside its sides, the chat's ground shows (as on the same row far to
/// the right), not a bubble's fill.
fn picture_bare_faults(image: &image::RgbImage, ground: Rect, what: &str) -> Vec<String> {
    let (cx, cy) = ground.centre();
    let outside = [
        (ground.x - 6, cy),
        (ground.right() + 6, cy),
        (cx, ground.y - 6),
        (cx, ground.bottom() + 6),
        (ground.x - 6, ground.y - 6),
        (ground.right() + 6, ground.bottom() + 6),
    ];
    let behind: Vec<((i32, i32), [u8; 3])> = outside
        .into_iter()
        .filter_map(|(x, y)| {
            let chat = pixel(image, ground.right() + 260, y)?;
            pixel(image, x, y)
                .filter(|here| !near(*here, chat, 8))
                .map(|here| ((x, y), here))
        })
        .collect();
    if behind.is_empty() {
        Vec::new()
    } else {
        vec![format!("{what}: {behind:?} round the picture: a frame or a bubble")]
    }
}

/// A long caption is cut at 150 characters and drawn in several lines inside the region; an empty one keeps Done off:
/// its click does nothing and Tab skips it.
pub(super) fn caption_long_and_empty(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.captions_chat.to_owned();
    user.open(&chat)?;
    let mut faults = Vec::new();
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    let long = "made up words for a very long caption ".repeat(5);
    user.press("ctrl+a")?;
    let typed = type_long(user, &long, &mut faults)?;
    user.image("caption-long-editor")?;
    let object = user.expect_upload();
    let done = user.done_button("caption-long-done")?;
    user.click(done.centre())?;
    user.until("the long caption in the box", |state| {
        let pictures = pictures(state);
        pictures.len() == 1 && pictures[0]["state"] == "sent"
    })?;
    let drawn = user.uploaded(&object)?;
    let region = region_of(&caption_of(SO_COOL)?).ok_or("So cool has no region")?;
    faults.extend(block_faults(&drawn, region, "the long caption"));
    let lines = ink_lines(&ink(&drawn));
    say(&format!("  the long caption's lines of ink: {lines}"));
    if lines < 3 {
        faults.push(format!("the long caption is drawn in {lines} lines, not wrapped"));
    }
    let label = caption::Caption::label(&caption_of(SO_COOL)?, "So cool", &[typed]);
    if pictures(&user.state()?)[0]["sticker"] != label.as_str() {
        faults.push(format!("the long caption's label is not {label:?}"));
    }
    user.ask("clear")?;
    faults.extend(empty_faults(user)?);
    faults_to_result(&faults)
}

/// Types `long` in the editor's caption at a fast typist's pace; what the caption holds after it. A stall of the
/// app and a caption that is not the first 150 characters typed are faults.
fn type_long(user: &mut User<'_>, long: &str, faults: &mut Vec<String>) -> SandboxResult<String> {
    // a fast typist's pace: five characters, then a pause
    let chars: Vec<char> = long.chars().collect();
    for chunk in chars.chunks(5) {
        user.type_text(&chunk.iter().collect::<String>())?;
    }
    let typed_at = std::time::Instant::now();
    // a stalled app answers no question in time: ask again until it answers with the whole caption
    let mut answered = false;
    while typed_at.elapsed() < std::time::Duration::from_secs(240) {
        if let Ok(state) = user.state()
            && editor(&state)["typed"][0]
                .as_str()
                .is_some_and(|text| text.chars().count() >= 150)
        {
            answered = true;
            break;
        }
        thread::sleep(super::POLL);
    }
    let late = typed_at.elapsed();
    say(&format!(
        "  the editor held 150 characters {} ms after the last key",
        late.as_millis()
    ));
    if late > std::time::Duration::from_secs(5) {
        faults.push(format!(
            "the app stalled: it showed the typed caption {} s after the last key",
            late.as_secs()
        ));
    }
    super::ensure(answered, || "the editor never held the long caption".to_owned())?;
    thread::sleep(super::SETTLE);
    let typed = editor(&user.state()?)["typed"][0]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let kept: String = long.chars().take(150).collect();
    if typed != kept {
        faults.push(format!(
            "past 150 characters the caption is {typed:?} ({} characters), expected the first 150 typed: {kept:?}",
            typed.chars().count()
        ));
    }
    Ok(typed)
}

/// An empty caption keeps Done off: Tab from the field skips it, and its click does nothing.
fn empty_faults(user: &mut User<'_>) -> SandboxResult<Vec<String>> {
    let mut faults = Vec::new();
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    let done = user.done_button("caption-empty-before")?;
    user.press("ctrl+a")?;
    user.press("backspace")?;
    let state = user.until("the caption empty", |state| editor(state)["typed"] == json!([""]))?;
    if editor(&state)["can_finish"] != false {
        faults.push(format!("Done is on with an empty caption: {}", editor(&state)));
    }
    user.image("caption-empty-light")?;
    user.ask("theme-dark")?;
    user.image("caption-empty-dark")?;
    user.ask("theme-light")?;
    // Tab from the field skips Done to Cancel: Enter there closes the editor
    user.press("tab")?;
    user.press("enter")?;
    let state = user.editor_closed("Tab then Enter with no caption: Cancel")?;
    if !pictures(&state).is_empty() {
        faults.push("Tab then Enter with no caption put a picture in the box".to_owned());
    }
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    user.press("ctrl+a")?;
    user.press("backspace")?;
    user.until("the caption empty again", |state| editor(state)["typed"] == json!([""]))?;
    user.click(done.centre())?;
    thread::sleep(super::SETTLE);
    let state = user.state()?;
    if !editor_open(&state) || !pictures(&state).is_empty() {
        faults.push(format!("a click on Done with no caption acted: {}", picker(&state)));
    }
    Ok(faults)
}

/// A meme: two empty lines, Done off until a line holds words; Tab from the top line to the bottom one, Done, Enter;
/// the picture's lines in upper case, white with a black edge, at the top and the bottom.
pub(super) fn caption_meme(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.captions_chat.to_owned();
    user.open(&chat)?;
    let mut faults = Vec::new();
    let state = user.open_editor_by_keys(MEME, "grumpy")?;
    if editor(&state)["typed"] != json!(["", ""]) || editor(&state)["can_finish"] != false {
        faults.push(format!("the meme editor starts as {}", editor(&state)));
    }
    let (hints_area, hints) = user.shown_template("caption-meme-empty")?;
    user.type_text("made up top")?;
    let state = user.until("the top line typed", |state| editor(state)["typed"][0] == "made up top")?;
    if editor(&state)["can_finish"] != true {
        faults.push(format!("Done is off with a top line: {}", editor(&state)));
    }
    user.press("tab")?;
    user.type_text("made up bottom")?;
    user.until("the bottom line typed", |state| {
        editor(state)["typed"] == json!(["made up top", "made up bottom"])
    })?;
    faults.extend(meme_preview_faults(user, hints_area, &hints)?);
    for theme in ["light", "dark"] {
        user.ask(&format!("theme-{theme}"))?;
        user.image(&format!("caption-meme-{theme}"))?;
    }
    user.ask("theme-light")?;
    let object = user.expect_upload();
    user.press("tab")?;
    user.press("enter")?;
    let state = user.until("the meme in the box", |state| {
        let pictures = pictures(state);
        !picker_open(state) && pictures.len() == 1 && pictures[0]["state"] == "sent"
    })?;
    let label = "grumpycat made up top, made up bottom";
    if pictures(&state)[0]["sticker"] != label {
        faults.push(format!(
            "the meme's label is {}, not {label:?}",
            pictures(&state)[0]["sticker"]
        ));
    }
    let drawn = user.uploaded(&object)?;
    faults.extend(meme_faults(&drawn)?);
    user.image("caption-meme-in-box")?;
    user.ask("focus-box")?;
    user.press("enter")?;
    user.wait_sent(&format!("Sticker image, {label}"))?;
    user.until("the box empty after the meme's Send", |state| {
        pictures(state).is_empty()
    })?;
    user.image("caption-meme-sent")?;
    faults_to_result(&faults)
}

/// The editor's picture shows the typed lines in place of the hints within 5 s of the last key (checked for 60 s).
fn meme_preview_faults(user: &mut User<'_>, area: Rect, hints: &image::RgbImage) -> SandboxResult<Vec<String>> {
    let typed_at = std::time::Instant::now();
    let mut shots = 0;
    while typed_at.elapsed() < std::time::Duration::from_secs(60) {
        shots += 1;
        let (_, now) = user.shown_template(&format!("caption-meme-preview-{shots}"))?;
        let redrawn = changed(hints, &now, area);
        if redrawn > 400 {
            let late = typed_at.elapsed();
            say(&format!(
                "  the meme's picture showed the typed lines {} ms after the last key ({redrawn} px)",
                late.as_millis()
            ));
            return Ok(if late > std::time::Duration::from_secs(5) {
                vec![format!(
                    "the meme's picture showed the typed lines only {} s after the last key",
                    late.as_secs()
                )]
            } else {
                Vec::new()
            });
        }
        thread::sleep(std::time::Duration::from_millis(500));
    }
    Ok(vec![
        "the meme's picture still showed the hints 60 s after both lines were typed".to_owned(),
    ])
}

/// The meme's picture: the lines drawn in upper case (the same as the upper-case words drawn by the same code),
/// white with a black edge, ink only in the top and the bottom thirds.
fn meme_faults(drawn: &RgbaImage) -> SandboxResult<Vec<String>> {
    let mut faults = Vec::new();
    let template = template_picture(&path_of(
        &teams::catalog()
            .template(teams::catalog().sticker(MEME).ok_or("no meme")?)
            .ok_or("no meme template")?,
    ))
    .ok_or("the meme template is not made up")?;
    let upper = ["MADE UP TOP".to_owned(), "MADE UP BOTTOM".to_owned()];
    let expected = caption::render(&template, &caption_of(MEME)?, &upper).ok_or("no upper-case drawing")?;
    let expected = image::load_from_memory(&expected.bytes)?.to_rgba8();
    if expected.as_raw() != drawn.as_raw() {
        faults.push("the meme's picture is not its lines in upper case".to_owned());
    }
    let points = ink(drawn);
    let height = drawn.height();
    let white = points
        .iter()
        .filter(|(_, _, colour)| near(*colour, [255, 255, 255], 30))
        .count();
    let black = points
        .iter()
        .filter(|(_, _, colour)| near(*colour, [0, 0, 0], 30))
        .count();
    let middle = points
        .iter()
        .filter(|(_, y, _)| *y > height / 3 && *y < height * 2 / 3)
        .count();
    let top = points.iter().filter(|(_, y, _)| *y <= height / 3).count();
    let bottom = points.iter().filter(|(_, y, _)| *y >= height * 2 / 3).count();
    say(&format!(
        "  meme ink: {white} white, {black} black; top {top}, middle {middle}, bottom {bottom}"
    ));
    if white < 100 || black < 100 {
        faults.push(format!(
            "the meme's letters are not white with a black edge ({white} white, {black} black)"
        ));
    }
    if top < 100 || bottom < 100 || middle > 0 {
        faults.push(format!("the meme's ink: top {top}, middle {middle}, bottom {bottom}"));
    }
    Ok(faults)
}

/// The keyboard: Shift+Tab from the field to the back arrow, Enter goes back; Tab to Done, Enter puts the sticker in
/// the box; Tab twice to Cancel, Space goes back; Tab three times round to the back arrow and a fourth to the field;
/// Escape goes back. The pointer: back and Cancel go back.
pub(super) fn caption_keyboard_and_back(user: &mut User<'_>) -> SandboxResult {
    let chat = user.world.captions_chat.to_owned();
    user.open(&chat)?;
    let mut faults = Vec::new();
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    user.press("shift+tab")?;
    user.press("enter")?;
    user.editor_closed("Shift+Tab, Enter: back")?;
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    let object = user.expect_upload();
    user.press("tab")?;
    user.press("enter")?;
    let state = user.until("Tab, Enter: Done", |state| {
        pictures(state).len() == 1 && !picker_open(state)
    })?;
    if pictures(&state)[0]["sticker"] != "THAT'S SO COOL" {
        faults.push(format!("Done by the keyboard put {} in the box", pictures(&state)[0]));
    }
    user.uploaded(&object)?;
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    user.press("tab")?;
    user.press("tab")?;
    user.press("space")?;
    user.editor_closed("Tab, Tab, Space: Cancel")?;
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    for _ in 0..4 {
        user.press("tab")?;
    }
    user.press("end")?;
    user.type_text("z")?;
    if let Err(error) = user.until("Tab four times: back in the field", |state| {
        editor(state)["typed"] == json!(["THAT'S SO COOLz"])
    }) {
        faults.push(error.to_string());
    }
    user.press("escape")?;
    let state = user.editor_closed("Escape: back")?;
    if pictures(&state).len() != 1 {
        faults.push(format!(
            "the editor's Cancel and back changed the box: {:?}",
            pictures(&state)
        ));
    }
    user.ask("clear")?;
    // the pointer: Cancel, mirrored from Done about the picker's middle, and the back arrow above the picture
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    let done = user.done_button("caption-keys-done")?;
    let (picture, _) = user.shown_template("caption-keys-picture")?;
    let centre = PICKER.x + PICKER.w / 2;
    let (done_x, done_y) = done.centre();
    user.click((2 * centre - done_x, done_y))?;
    user.editor_closed("Cancel by the pointer")?;
    user.open_editor_by_keys(SO_COOL, "so cool")?;
    user.click((PICKER.x + 20, picture.y - 24))?;
    user.editor_closed("the back arrow by the pointer")?;
    let state = user.state()?;
    if !pictures(&state).is_empty() {
        faults.push(format!(
            "Cancel or back by the pointer put a picture in the box: {:?}",
            pictures(&state)
        ));
    }
    // the other tabs still work with the editor closed
    user.press("ctrl+tab")?;
    let state = user.until("the tab after Stickers", |state| picker(state)["tab"] != "Stickers")?;
    say(&format!("  tab after Stickers: {}", picker(&state)["tab"]));
    faults_to_result(&faults)
}
