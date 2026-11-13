//! An app item's tile across runs of the app (#28), as a user meets it. a's feed holds people's items and items of
//! apps whose definitions name: a tile the CDN serves (on its app host and, older, on Teams' static host), none, one
//! the CDN refuses (404), one that isn't a picture, one that doesn't decode, one too large, and URLs off the CDN's app
//! folders (another folder, another host, plain http; the mock serves a picture at each of those paths too, so only
//! the URL check keeps it out). The app runs three times on one data dir: the first reads each allowed tile once and
//! draws the two the CDN serves, the rest keep their initials and badge with nothing said; the next shows Made-up
//! Updates' tile from the cache with no read; the third, Made-up Updates' definition naming a new version, reads and
//! shows the new picture, then a new item of it brings a banner with no new read. The CDN's answers come from the mock's request log, read in the app's child.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use std::{fs, io};

use serde_json::{Value, json};
use teams_mock::{AB_CHAT, ABC_CHAT, ChatServiceMock, LAB_A, LAB_B, LAB_C};
use ui_sandbox::{Sandbox, SandboxResult, wait_until};

use super::{AWAY, CTL_SOCKET, Check, FIRST_AVATAR, Probe, Qa, activity, app_note, ensure, first, items, say, text};

/// The data dir the tile runs share, passed to the app's child.
const DATA_ENV: &str = "NGTEAMS_ACTIVITY_TILE_DATA";
/// Made-up Updates (its tile on the CDN's app host) and Made-up Pages (no tile), as the mock's own definitions.
const UPDATES: &str = "00000000-0000-4000-8000-00000000a002";
const PAGES: &str = "00000000-0000-4000-8000-00000000a003";
/// The CDN's app folder, and the same on the mock.
const CDN: &str = "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps";
const CDN_PATH: &str = "/teamsappdata/evergreen-assets/apps";
/// The items of the tile runs' feed.
const FEED_ITEMS: usize = 14;
/// More than the definition reads a run makes (one per app).
const DEFINITION_READS: usize = 24;
/// How long the pane gets to read an app's definition and tile before an item is taken to have none.
const QUIET: Duration = Duration::from_millis(2500);
/// Around the second row's avatar (left, top, right, bottom).
const SECOND_AVATAR: (u32, u32, u32, u32) = (80, 240, 150, 290);
/// How long a banner gets to come.
const BANNER_WAIT: Duration = Duration::from_secs(5);
/// The new version's tile, and the tiles of the other apps the mock serves.
const GREEN: [u8; 3] = [0x10, 0xb0, 0x40];
const ORANGE: [u8; 3] = [0xf0, 0x80, 0x10];

/// The runs, in order, on one data dir.
const RUNS: [(&str, &[Check]); 3] = [
    (
        "tiles-v1",
        &[
            ("app_tiles_first_run_reads_each_allowed_picture_once", first_run),
            (
                "app_tiles_failed_or_refused_keep_initials_and_badge_quietly_light_and_dark",
                fallbacks,
            ),
            ("people_items_keep_their_round_avatars_beside_app_tiles", people),
        ],
    ),
    (
        "tiles-v1",
        &[("app_tile_shows_from_the_cache_after_a_restart_with_no_read", cached)],
    ),
    // last, as its new item and the chat it leaves open are the mock's for this run only
    (
        "tiles-v2",
        &[
            ("app_tile_of_a_new_version_is_read_and_shown", new_version),
            ("new_app_item_banner_and_its_tile_with_no_new_read", banner),
        ],
    ),
];

/// The flows that passed, and those that failed with why.
type Tally = (Vec<&'static str>, Vec<(&'static str, String)>);

/// The tile runs (see the module docs).
pub(super) fn run(sandbox: &Sandbox) -> SandboxResult<Tally> {
    let data = tempfile::tempdir()?;
    let dir = data.path().to_string_lossy().into_owned();
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    for (mode, checks) in RUNS {
        let _ = fs::remove_file(sandbox.path(CTL_SOCKET));
        let mut qa = Qa::start_in(sandbox, mode, &[(DATA_ENV, &dir)])?;
        for (name, check) in checks {
            qa.check(name, *check);
        }
        passed.append(&mut qa.passed.clone());
        failed.append(&mut qa.failed.clone());
        qa.quit()?;
    }
    Ok((passed, failed))
}

/// A tile run as the app's child sees it: the version Made-up Updates' definition names, and the kept data dir.
pub(super) struct Run {
    version: u32,
    pub(super) dir: PathBuf,
}

impl Run {
    /// The run of `mode` (`tiles-v1`, `tiles-v2`), or `None` for another mode.
    pub(super) fn of_mode(mode: &str) -> SandboxResult<Option<Self>> {
        let version = match mode {
            "tiles-v1" => 1,
            "tiles-v2" => 2,
            _ => return Ok(None),
        };
        let dir = PathBuf::from(std::env::var_os(DATA_ENV).ok_or("no tile data dir")?);
        Ok(Some(Self { version, dir }))
    }
}

/// What the CDN (the mock) answers at an app's tile path.
#[derive(Clone, Copy)]
enum Answer {
    /// The mock's own route: Made-up Updates' blue-to-violet tile.
    Mock,
    /// Nothing served there.
    Nothing,
    /// A made-up tile of one colour.
    Picture([u8; 3]),
    /// 404.
    Missing,
    /// An error page's text.
    NotAPicture,
    /// A PNG cut short after its header.
    Broken,
    /// A PNG wider and taller than a tile is ever decoded from.
    Huge,
}

/// An app of the tile runs' feed.
struct TileApp {
    id: &'static str,
    name: &'static str,
    /// Its definition's `largeImageUrl`, if any.
    url: Option<String>,
    /// The path the mock answers it at.
    path: String,
    answer: Answer,
    /// Whether the app reads it (an allowed URL), and draws it.
    read: bool,
    shown: bool,
}

fn tile_app(id: &'static str, name: &'static str, url: &str, path: &str, answer: Answer) -> TileApp {
    TileApp {
        id,
        name,
        url: Some(url.to_owned()),
        path: path.to_owned(),
        answer,
        read: true,
        shown: false,
    }
}

/// The apps, Made-up Updates' definition naming `version`.
fn apps(version: u32) -> Vec<TileApp> {
    let cdn = |file: &str| (format!("{CDN}/{file}"), format!("{CDN_PATH}/{file}"));
    let (updates, updates_path) = cdn("made-up-updates_largeImage.png");
    let refused = |app: TileApp| TileApp { read: false, ..app };
    let shown = |app: TileApp| TileApp { shown: true, ..app };
    let file = |name: &str| format!("{CDN_PATH}/{name}");
    vec![
        shown(tile_app(
            UPDATES,
            "Made-up Updates",
            &format!("{updates}?v={version}"),
            &updates_path,
            if version == 1 {
                Answer::Mock
            } else {
                Answer::Picture(GREEN)
            },
        )),
        shown(tile_app(
            "00000000-0000-4000-8000-00000000a101",
            "Made-up Classic",
            "https://statics.teams.cdn.office.net/evergreen-assets/apps/made-up-classic_largeImage.png",
            "/evergreen-assets/apps/made-up-classic_largeImage.png",
            Answer::Picture(ORANGE),
        )),
        TileApp {
            url: None,
            read: false,
            ..tile_app(PAGES, "Made-up Pages", "", "", Answer::Nothing)
        },
        tile_app(
            "00000000-0000-4000-8000-00000000a102",
            "Made-up Missing",
            &format!("{CDN}/made-up-missing_largeImage.png?v=1"),
            &file("made-up-missing_largeImage.png"),
            Answer::Missing,
        ),
        tile_app(
            "00000000-0000-4000-8000-00000000a103",
            "Made-up Garbled",
            &format!("{CDN}/made-up-garbled_largeImage.png"),
            &file("made-up-garbled_largeImage.png"),
            Answer::NotAPicture,
        ),
        tile_app(
            "00000000-0000-4000-8000-00000000a104",
            "Made-up Broken",
            &format!("{CDN}/made-up-broken_largeImage.png"),
            &file("made-up-broken_largeImage.png"),
            Answer::Broken,
        ),
        tile_app(
            "00000000-0000-4000-8000-00000000a105",
            "Made-up Huge",
            &format!("{CDN}/made-up-huge_largeImage.png"),
            &file("made-up-huge_largeImage.png"),
            Answer::Huge,
        ),
        refused(tile_app(
            "00000000-0000-4000-8000-00000000a106",
            "Made-up Elsewhere",
            "https://statics.teams.cdn.office.net/made-up-other/made-up-elsewhere.png",
            "/made-up-other/made-up-elsewhere.png",
            Answer::Picture(ORANGE),
        )),
        refused(tile_app(
            "00000000-0000-4000-8000-00000000a107",
            "Made-up Foreign",
            "https://made-up-apps.example.com/teamsappdata/evergreen-assets/apps/made-up-foreign_largeImage.png",
            &file("made-up-foreign_largeImage.png"),
            Answer::Picture(ORANGE),
        )),
        refused(tile_app(
            "00000000-0000-4000-8000-00000000a108",
            "Made-up Plain",
            "http://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-plain_largeImage.png",
            &file("made-up-plain_largeImage.png"),
            Answer::Picture(ORANGE),
        )),
    ]
}

/// The apps' definitions, as the store's definitions read answers them.
fn definitions(apps: &[TileApp]) -> Value {
    let listed: Vec<Value> = apps
        .iter()
        .map(|app| {
            let mut definition = json!({
                "id": app.id, "name": app.name, "developerName": "Made-up Maker",
                "shortDescription": "Made up", "accentColor": "#5B5FC7",
            });
            if let Some(url) = &app.url {
                definition["largeImageUrl"] = json!(url);
            }
            if app.id == UPDATES {
                definition["activities"] =
                    json!({"activityTypes": [{"type": "messageNotify"}, {"type": "customNotification"}]});
                definition["bots"] = json!([{ "id": "00000000-0000-4000-8000-00000000b002", "scopes": ["Team"] }]);
            }
            definition
        })
        .collect();
    json!(listed)
}

/// The mock for a tile run: the apps' definitions, their tiles, and a's feed (oldest first).
pub(super) async fn seed(mock: &ChatServiceMock, run: &Run) -> SandboxResult {
    let apps = apps(run.version);
    let answer = definitions(&apps);
    for _ in 0..DEFINITION_READS {
        mock.answer_next("POST", "users/apps/batchedDefinitions", 200, &answer)
            .await;
    }
    for app in &apps {
        serve(mock, app).await?;
    }
    let mine: u64 = mock
        .post_as(LAB_A, AB_CHAT, "<p>The made-up plan is ready</p>")
        .ok_or("a's post refused")?
        .parse()?;
    let theirs: u64 = mock
        .post_as(LAB_C, ABC_CHAT, "<p>Lab A can you look at the made-up plan?</p>")
        .ok_or("c's post refused")?
        .parse()?;
    let app = |index: usize| &apps[index];
    let b = (LAB_B, "Lab B");
    let feed = [
        note(app(0), 300, "an older update"),
        note(app(9), 301, ""),
        note(app(8), 302, ""),
        note(app(7), 303, ""),
        activity("call", "missedCall", b, ("", 1), ""),
        note(app(6), 304, ""),
        note(app(5), 305, ""),
        note(app(4), 306, ""),
        note(app(3), 307, ""),
        note(app(2), 308, ""),
        activity(
            "mentionInChat",
            "person",
            (LAB_C, "Lab C"),
            (ABC_CHAT, theirs),
            "Lab A can you look at the made-up plan?",
        ),
        note(app(1), 309, ""),
        activity(
            "reactionInChat",
            "like",
            b,
            (AB_CHAT, mine),
            "The made-up plan is ready",
        ),
        note(app(0), 310, "the newest update"),
    ];
    for (index, item) in feed.iter().enumerate() {
        mock.add_activity(LAB_A, item, index < 10).ok_or("no feed")?;
    }
    Ok(())
}

/// What the mock answers at `app`'s tile path.
async fn serve(mock: &ChatServiceMock, app: &TileApp) -> SandboxResult {
    let octets = "application/octet-stream";
    match app.answer {
        Answer::Mock | Answer::Nothing => {}
        Answer::Picture(colour) => mock.serve_bytes(&app.path, octets, tile(colour)?).await,
        Answer::Missing => {
            for _ in 0..3 {
                mock.refuse_next("GET", &app.path, 404).await;
            }
        }
        Answer::NotAPicture => {
            let page = b"<html><body>Made-up error page</body></html>".to_vec();
            mock.serve_bytes(&app.path, octets, page).await;
        }
        Answer::Broken => {
            let mut cut = tile(ORANGE)?;
            cut.truncate(64);
            mock.serve_bytes(&app.path, octets, cut).await;
        }
        Answer::Huge => mock.serve_bytes(&app.path, octets, huge()?).await,
    }
    Ok(())
}

/// A note of `app` in the team's channel, its `id`.
fn note(app: &TileApp, id: u64, words: &str) -> Value {
    let mut item = app_note();
    item["activityId"] = json!(id);
    item["sourceAppDisplayName"] = json!(app.name);
    item["sourceUserImDisplayName"] = json!(app.name);
    item["messagePreview"] = json!(format!("Made-up note from {} {words}", app.name).trim().to_owned());
    item["activityContext"]["teamsAppId"] = json!(app.id);
    item
}

/// A made-up tile as the CDN serves one: 192×192, `colour` inside a transparent margin.
fn tile(colour: [u8; 3]) -> SandboxResult<Vec<u8>> {
    const SIDE: u32 = 192;
    const MARGIN: u32 = 36;
    let inside = MARGIN..SIDE - MARGIN;
    let picture = image::RgbaImage::from_fn(SIDE, SIDE, |x, y| {
        if inside.contains(&x) && inside.contains(&y) {
            image::Rgba([colour[0], colour[1], colour[2], 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    });
    png(&image::DynamicImage::ImageRgba8(picture))
}

/// A grey PNG past the tile's largest source side (4096) both ways.
fn huge() -> SandboxResult<Vec<u8>> {
    png(&image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(
        4200,
        4200,
        image::Luma([0x80]),
    )))
}

fn png(picture: &image::DynamicImage) -> SandboxResult<Vec<u8>> {
    let mut bytes = Vec::new();
    picture.write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

/// The child's answer to `tile-reads`: every picture `GET` the mock got this run, by path, each read's query.
pub(super) fn reads(probe: &Probe) -> Value {
    let requests = probe.block(probe.mock.received_requests()).unwrap_or_default();
    let mut reads: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for request in requests.iter().filter(|request| request.method.as_str() == "GET") {
        let path = request.url.path();
        let lower = path.to_ascii_lowercase();
        if [".png", ".jpg", ".jpeg"].iter().any(|end| lower.ends_with(end)) {
            reads
                .entry(path.to_owned())
                .or_default()
                .push(request.url.query().unwrap_or_default().to_owned());
        }
    }
    json!(reads)
}

/// The child's answer to `tile-push`: a new note of Made-up Updates, unread, as the feed then tells a.
pub(super) fn push_update(mock: &ChatServiceMock) -> Value {
    let Some(app) = apps(1).into_iter().find(|app| app.id == UPDATES) else {
        return json!({"error": "no such app"});
    };
    mock.add_activity(LAB_A, &note(&app, 320, "that just came"), false)
        .map_or_else(|| json!({"error": "no feed"}), |id| json!({"id": id}))
}

/// Waits until a tile run's feed is listed (a later run lists the earlier runs' kept items too).
pub(super) fn settled(qa: &Qa<'_>) -> SandboxResult {
    qa.state_until("the tile run's feed listed", |state| items(state).len() >= FEED_ITEMS)?;
    Ok(())
}

/// The picture reads this run, by path.
fn tile_reads(qa: &Qa<'_>) -> SandboxResult<BTreeMap<String, Vec<String>>> {
    let reads = qa.ask("tile-reads")?;
    say(&format!("tile reads: {reads}"));
    Ok(serde_json::from_value(reads)?)
}

fn reads_of<'a>(reads: &'a BTreeMap<String, Vec<String>>, path: &str) -> &'a [String] {
    reads.get(path).map_or(&[], Vec::as_slice)
}

/// Whether each app item is drawn with its tile exactly when `apps` say, people's items never.
fn tiles_as_expected(state: &Value, apps: &[TileApp]) -> Result<(), String> {
    for item in items(state) {
        let app = text(&item, "app");
        let expected = apps
            .iter()
            .find(|known| known.id == app)
            .is_some_and(|known| known.shown);
        if item["tile"] != expected {
            return Err(format!(
                "{} ({}) drawn {} its tile",
                text(&item, "title"),
                if app.is_empty() { "a person's" } else { app.as_str() },
                if expected { "without" } else { "with" }
            ));
        }
    }
    Ok(())
}

/// The pane with every tile drawn that the run's apps show, and after [`QUIET`] still only those; no toast.
fn tiles_settle(qa: &mut Qa<'_>, version: u32) -> SandboxResult<Value> {
    let apps = apps(version);
    qa.plain_pane()?;
    qa.state_until("the apps' tiles drawn", |state| tiles_as_expected(state, &apps).is_ok())?;
    thread::sleep(QUIET);
    let state = qa.state()?;
    tiles_as_expected(&state, &apps)?;
    ensure(state["toast"].is_null(), || format!("a toast: {}", state["toast"]))?;
    ensure(text(&first(&state)?, "app") == UPDATES, || {
        format!("Made-up Updates' newest note first: {state}")
    })?;
    Ok(state)
}

/// First run: Made-up Updates' and Made-up Classic's tiles drawn, each allowed tile read once (Updates' at its
/// version, once for its two items), the refused URLs never; no toast.
fn first_run(qa: &mut Qa<'_>) -> SandboxResult {
    tiles_settle(qa, 1)?;
    qa.hover(AWAY)?;
    let shot = qa.shot("tiles-pane-light")?;
    let blue = tile_pixels(&shot, is_mock_tile)?;
    ensure(blue > 150, || {
        format!("Made-up Updates' tile not drawn on the first row ({blue} pixels)")
    })?;
    let reads = tile_reads(qa)?;
    for app in apps(1).iter().filter(|app| !app.path.is_empty()) {
        let made = reads_of(&reads, &app.path);
        let wanted = usize::from(app.read);
        ensure(made.len() == wanted, || {
            format!("{} ({}) read {} times, not {wanted}", app.name, app.path, made.len())
        })?;
    }
    let updates = reads_of(&reads, &format!("{CDN_PATH}/made-up-updates_largeImage.png"));
    ensure(updates == ["v=1"], || {
        format!("Made-up Updates' tile read as {updates:?}")
    })?;
    Ok(())
}

/// Under Apps: the apps whose tile is missing, not a picture, broken, too large, off the CDN's folders or not named
/// keep their initials and badge, nothing said; in Teams Light and Teams Dark.
fn fallbacks(qa: &mut Qa<'_>) -> SandboxResult {
    let apps = apps(1);
    qa.plain_pane()?;
    qa.click_chip("apps")?;
    let state = qa.state_until("the Apps chip on", |state| state["filter"] == "apps")?;
    let listed = items(&state);
    ensure(
        listed.len() == 11 && listed.iter().all(|item| !text(item, "app").is_empty()),
        || format!("the Apps chip lists {} items: {state}", listed.len()),
    )?;
    tiles_as_expected(&state, &apps)?;
    ensure(state["toast"].is_null(), || format!("a toast: {}", state["toast"]))?;
    qa.hover(AWAY)?;
    qa.shot("tiles-apps-light")?;
    qa.ask("theme dark")?;
    let dark = dark_shots(qa, &apps);
    qa.ask("theme light")?;
    dark
}

/// The Apps chip's list and the whole pane in Teams Dark, the tiles as in Teams Light.
fn dark_shots(qa: &mut Qa<'_>, apps: &[TileApp]) -> SandboxResult {
    let shot = qa.shot("tiles-apps-dark")?;
    let blue = tile_pixels(&shot, is_mock_tile)?;
    ensure(blue > 150, || {
        format!("no tile on the first row in Teams Dark ({blue} pixels)")
    })?;
    qa.click_chip("apps")?;
    let state = qa.state_until("the Apps chip off", |state| state["filter"] == "all")?;
    tiles_as_expected(&state, apps)?;
    qa.hover(AWAY)?;
    qa.shot("tiles-pane-dark")?;
    Ok(())
}

/// People's items (a reaction, a mention, a missed call) beside the app tiles keep their round avatars: never drawn
/// with a tile, and the second row's avatar (b's reaction) round, its corners the pane's own background.
fn people(qa: &mut Qa<'_>) -> SandboxResult {
    let state = qa.plain_pane()?;
    let listed = items(&state);
    let people: Vec<&Value> = listed.iter().filter(|item| text(item, "app").is_empty()).collect();
    ensure(
        people.len() == 3 && people.iter().all(|item| item["tile"] == false),
        || format!("people's items: {people:?}"),
    )?;
    let second = listed.get(1).ok_or("no second row")?;
    ensure(text(second, "kind") == "reaction", || {
        format!("b's reaction second: {second}")
    })?;
    qa.hover(AWAY)?;
    let shot = qa.shot("tiles-people-light")?;
    ensure(second_avatar_round(&shot)?, || {
        "the second row's avatar has square corners".to_owned()
    })?;
    Ok(())
}

/// A new item of Made-up Updates while a chat is on screen: a banner comes, the item is drawn with the tile already
/// read, and the tile isn't read again. Whether the banner carries a picture is logged.
fn banner(qa: &mut Qa<'_>) -> SandboxResult {
    qa.ctl_json(&["open", "chat", ABC_CHAT])?;
    let before = tile_reads(qa)?;
    let shown = qa.sandbox.bus().notifications().len();
    let pushed = qa.ask("tile-push")?;
    let id = text(&pushed, "id");
    ensure(!id.is_empty(), || format!("the push: {pushed}"))?;
    qa.state_until("the new item listed with its tile", |state| {
        items(state)
            .iter()
            .any(|item| text(item, "id") == id && item["tile"] == true)
    })?;
    let banner = wait_until(BANNER_WAIT, || qa.sandbox.bus().notifications().get(shown).cloned())
        .ok_or("no banner for the new app item")?;
    // the icon names the app's desktop entry; a picture of the item would be a path or an image hint
    let picture = banner.app_icon.contains('/')
        || banner
            .hints
            .iter()
            .any(|hint| ["image-data", "image_data", "image-path", "image_path", "icon_data"].contains(&hint.as_str()));
    say(&format!(
        "the new app item's banner: {:?}: {:?}; icon {:?}, hints {:?}; carries a picture: {picture}",
        banner.summary, banner.body, banner.app_icon, banner.hints
    ));
    let after = tile_reads(qa)?;
    let path = format!("{CDN_PATH}/made-up-updates_largeImage.png");
    ensure(!reads_of(&before, &path).is_empty(), || {
        "Made-up Updates' tile never read this run".to_owned()
    })?;
    ensure(reads_of(&after, &path) == reads_of(&before, &path), || {
        format!("Made-up Updates' tile read again: {:?}", reads_of(&after, &path))
    })?;
    qa.plain_pane()?;
    qa.hover(AWAY)?;
    qa.shot("tiles-new-item-light")?;
    Ok(())
}

/// Second run, same data: Made-up Updates' and Made-up Classic's tiles drawn with no read of either.
fn cached(qa: &mut Qa<'_>) -> SandboxResult {
    tiles_settle(qa, 1)?;
    qa.hover(AWAY)?;
    let shot = qa.shot("tiles-cached-light")?;
    let blue = tile_pixels(&shot, is_mock_tile)?;
    ensure(blue > 150, || {
        format!("no cached tile on the first row ({blue} pixels)")
    })?;
    let reads = tile_reads(qa)?;
    for app in apps(1).iter().filter(|app| app.shown) {
        let made = reads_of(&reads, &app.path);
        ensure(made.is_empty(), || format!("{} read again: {made:?}", app.name))?;
    }
    Ok(())
}

/// Third run, Made-up Updates' definition naming `v=2`: its tile read once at the new version and the new picture
/// drawn; Made-up Classic's still from the cache.
fn new_version(qa: &mut Qa<'_>) -> SandboxResult {
    let apps = apps(2);
    qa.plain_pane()?;
    qa.state_until("the apps' tiles drawn", |state| tiles_as_expected(state, &apps).is_ok())?;
    let shot = wait_until(Duration::from_secs(10), || {
        let shot = qa.shot("tiles-new-version-light").ok()?;
        (tile_pixels(&shot, is_green).ok()? > 150).then_some(shot)
    })
    .ok_or("the new version's picture not drawn on the first row")?;
    let blue = tile_pixels(&shot, is_mock_tile)?;
    ensure(blue < 20, || format!("the old picture still drawn ({blue} pixels)"))?;
    let reads = tile_reads(qa)?;
    let updates = reads_of(&reads, &format!("{CDN_PATH}/made-up-updates_largeImage.png"));
    ensure(updates == ["v=2"], || {
        format!("Made-up Updates' tile read as {updates:?}")
    })?;
    let classic = reads_of(&reads, "/evergreen-assets/apps/made-up-classic_largeImage.png");
    ensure(classic.is_empty(), || {
        format!("Made-up Classic read again: {classic:?}")
    })?;
    Ok(())
}

/// How many pixels around the first row's avatar `wanted` takes.
fn tile_pixels(shot: &Path, wanted: fn([u8; 3]) -> bool) -> SandboxResult<usize> {
    let (left, top, right, bottom) = FIRST_AVATAR;
    let image = image::open(shot)?.to_rgb8();
    Ok((top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|&(x, y)| wanted(image.get_pixel(x, y).0))
        .count())
}

/// The mock's own tile's blue-to-violet (`teams-mock`'s `team_apps.rs`).
fn is_mock_tile([r, g, b]: [u8; 3]) -> bool {
    b > 180 && (0x30..=0x60).contains(&r) && (0x5a..=0xa0).contains(&g)
}

fn is_green([r, g, b]: [u8; 3]) -> bool {
    g > 150 && r < 0x50 && b < 0x80
}

/// Whether the second row's avatar is round: the corner of the box its drawn pixels span (1 px in) is the pane's
/// background, as a circle leaves it.
fn second_avatar_round(shot: &Path) -> SandboxResult<bool> {
    let image = image::open(shot)?.to_rgb8();
    let (left, top, right, bottom) = SECOND_AVATAR;
    let background = image.get_pixel(left, top).0;
    let differs = |x: u32, y: u32| {
        let pixel = image.get_pixel(x, y).0;
        pixel.iter().zip(background).any(|(a, b)| a.abs_diff(b) > 24)
    };
    let (x0, y0) = (top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|&(x, y)| differs(x, y))
        .fold((u32::MAX, u32::MAX), |(x, y), (px, py)| (x.min(px), y.min(py)));
    if x0 == u32::MAX {
        return Err("no avatar on the second row".into());
    }
    say(&format!("the second row's avatar starts at ({x0}, {y0})"));
    Ok(!differs(x0 + 1, y0 + 1))
}
