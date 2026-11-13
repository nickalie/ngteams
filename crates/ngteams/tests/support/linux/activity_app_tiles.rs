//! An app item's tile (#28) as a user meets it: Made-up Updates' items (the mock's definition names its tile on the
//! CDN's path, which the mock serves) drawn with the tile and no badge; then a note of Made-up Pages (whose definition
//! names none) keeping the initials and the badge, in Teams Light and Teams Dark.

use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use teams_mock::{ChatServiceMock, LAB_A};
use ui_sandbox::SandboxResult;

use super::turn_off::APP;
use super::{AWAY, Qa, app_note, ensure, first, items, text};

/// Made-up Pages: the mock's definition names no tile.
const OTHER_APP: &str = "00000000-0000-4000-8000-00000000a003";
/// How long the pane gets to read an app's definition and tile before an item is taken to have none.
const QUIET: Duration = Duration::from_millis(2500);

pub(super) const CHECKS: [super::Check; 2] = [
    ("app_item_shows_its_tile_without_a_badge", tile_shown),
    (
        "app_without_a_tile_keeps_initials_and_badge_light_and_dark",
        tile_fallback,
    ),
];

impl Qa<'_> {
    /// The pane with the Apps chip on.
    fn apps_chip(&mut self) -> SandboxResult<Value> {
        self.plain_pane()?;
        self.click_chip("apps")?;
        self.state_until("the Apps chip on", |state| state["filter"] == "apps")
    }
}

/// A note of [`OTHER_APP`], the newest item, as the feed then tells a.
pub(super) fn push_other_app(mock: &ChatServiceMock) -> Value {
    let mut item = app_note();
    item["activityId"] = json!(20);
    item["sourceAppDisplayName"] = json!("Made-up Pages");
    item["sourceUserImDisplayName"] = json!("Made-up Pages");
    item["messagePreview"] = json!("Made-up page changed");
    item["activityContext"]["teamsAppId"] = json!(OTHER_APP);
    mock.add_activity(LAB_A, &item, true)
        .map_or_else(|| json!({"error": "no feed"}), |id| json!({"id": id}))
}

fn of_app<'a>(state: &'a [Value], app: &str) -> Vec<&'a Value> {
    state.iter().filter(|item| item["app"] == app).collect()
}

fn tile_shown(qa: &mut Qa<'_>) -> SandboxResult {
    qa.apps_chip()?;
    let state = qa.state_until("Made-up Updates' items drawn with its tile", |state| {
        let listed = items(state);
        let apps = of_app(&listed, APP);
        !apps.is_empty() && apps.iter().all(|item| item["tile"] == true)
    })?;
    ensure(text(&first(&state)?, "app") == APP, || {
        format!("Made-up Updates first: {state}")
    })?;
    qa.hover(AWAY)?;
    let shot = qa.shot("app-tile-light")?;
    let tiled = tile_pixels(&shot)?;
    ensure(tiled > 150, || {
        format!("no tile drawn on the first row ({tiled} pixels)")
    })?;
    Ok(())
}

fn tile_fallback(qa: &mut Qa<'_>) -> SandboxResult {
    let pushed = qa.ask("push-other-app")?;
    ensure(pushed.get("error").is_none(), || format!("the push: {pushed}"))?;
    qa.apps_chip()?;
    qa.state_until("Made-up Pages' note first", |state| {
        first(state).is_ok_and(|row| text(&row, "app") == OTHER_APP)
    })?;
    // its definition names no tile: still none once the pane had time to read it
    thread::sleep(QUIET);
    let state = qa.state()?;
    let listed = items(&state);
    ensure(
        of_app(&listed, OTHER_APP).iter().all(|item| item["tile"] == false),
        || format!("Made-up Pages drawn with a tile: {state}"),
    )?;
    ensure(of_app(&listed, APP).iter().all(|item| item["tile"] == true), || {
        format!("Made-up Updates lost its tile: {state}")
    })?;
    qa.hover(AWAY)?;
    let shot = qa.shot("app-tile-fallback-light")?;
    let tiled = tile_pixels(&shot)?;
    ensure(tiled < 20, || format!("a tile on the first row ({tiled} pixels)"))?;
    qa.ask("theme dark")?;
    let outcome = qa.shot("app-tile-dark");
    qa.ask("theme light")?;
    outcome?;
    qa.click_chip("apps")?;
    Ok(())
}

/// How many pixels of the first row's avatar are the made-up tile's blue-to-violet (`teams-mock`'s `team_apps.rs`).
fn tile_pixels(shot: &Path) -> SandboxResult<usize> {
    let (left, top, right, bottom) = super::FIRST_AVATAR;
    let image = image::open(shot)?.to_rgb8();
    Ok((top..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let [r, g, b] = image.get_pixel(x, y).0;
            b > 180 && (0x30..=0x60).contains(&r) && (0x5a..=0xa0).contains(&g)
        })
        .count())
}
