//! What Manage team's Apps tab and "More create team options" read (`docs/protocol/teams.md`), made up: a team's
//! installed apps, the store's apps and their definitions, Teams' templates and one template's channels and apps, and no
//! Microsoft 365 group to make a team of; and the tile Made-up Updates' definition names, on the CDN's path (Activity
//! draws it for the app's items; the other apps name none). Requests not implemented by these fixtures remain
//! unmatched.

use serde_json::{Value, json};
use wiremock::ResponseTemplate;

use crate::chatsvc::json;

/// The answer to `method` on `segments` when it is one of these; `None` for any other path.
pub(crate) fn serve(method: &str, segments: &[&str], body: &[u8]) -> Option<ResponseTemplate> {
    match (method, segments) {
        ("POST", [.., "teams", "apps", "aggregatedEntitlements"]) => Some(json(200, &entitlements())),
        ("POST", [.., "users", "apps", "store"]) => Some(json(200, &store())),
        ("POST", [.., "users", "apps", "batchedDefinitions"]) => Some(json(200, &json!(requested_definitions(body)))),
        ("GET", [.., "teamtemplates", "v1.0", "en-us"]) => Some(json(200, &templates())),
        ("GET", [.., "teamtemplates", "v1.0", _, "Public", _]) => Some(json(200, &template())),
        ("GET", [.., "teams", "usergroups"]) => Some(json(200, &json!([]))),
        (
            "GET",
            [
                "teamsappdata",
                "evergreen-assets",
                "apps",
                "made-up-updates_largeImage.png",
            ],
        ) => Some(tile()),
        _ => None,
    }
}

/// Made-up Updates' tile as the CDN answers one: a 192×192 PNG, its picture (a blue-to-violet rounded square) inside
/// a transparent margin, typed `application/octet-stream`.
fn tile() -> ResponseTemplate {
    const SIDE: u32 = 192;
    const MARGIN: u32 = 36;
    const CORNER: u32 = 28;
    let inside = |x: u32, y: u32| {
        let (left, right) = (MARGIN, SIDE - MARGIN - 1);
        let near = |v: u32, low: u32, high: u32| {
            if v < low + CORNER {
                Some(low + CORNER - v)
            } else if v > high - CORNER {
                Some(v - (high - CORNER))
            } else {
                None
            }
        };
        (left..=right).contains(&x)
            && (left..=right).contains(&y)
            && match (near(x, left, right), near(y, left, right)) {
                (Some(dx), Some(dy)) => dx * dx + dy * dy <= CORNER * CORNER,
                _ => true,
            }
    };
    let picture = image::RgbaImage::from_fn(SIDE, SIDE, |x, y| {
        if !inside(x, y) {
            return image::Rgba([0, 0, 0, 0]);
        }
        // from Teams' blue at the top left to its violet at the bottom right
        let t = (x + y) * 255 / (2 * (SIDE - 1));
        let mix = |from: u32, to: u32| u8::try_from((from * (255 - t) + to * t) / 255).unwrap_or(u8::MAX);
        image::Rgba([mix(0x36, 0x5b), mix(0x9b, 0x5f), mix(0xf0, 0xc7), 255])
    });
    let mut png = Vec::new();
    match picture.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png) {
        Ok(()) => ResponseTemplate::new(200).set_body_raw(png, "application/octet-stream"),
        Err(_) => ResponseTemplate::new(500),
    }
}

fn requested_definitions(body: &[u8]) -> Vec<Value> {
    let ids: Vec<String> = serde_json::from_slice(body).unwrap_or_default();
    definitions()
        .into_iter()
        .filter(|definition| {
            definition
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| ids.iter().any(|asked| asked == id))
        })
        .collect()
}

/// Made-up apps: a tab app, one with a bot, one Teams keeps for itself.
fn definitions() -> Vec<Value> {
    vec![
        json!({ "id": "00000000-0000-4000-8000-00000000a001", "name": "Made-up Planner", "developerName": "Made-up Maker",
                "shortDescription": "Plans made up", "accentColor": "#6264A7", "staticTabs": [] }),
        json!({ "id": "00000000-0000-4000-8000-00000000a002", "name": "Made-up Updates", "developerName": "Made-up Maker",
                "shortDescription": "Updates made up", "accentColor": "#5B5FC7",
                "largeImageUrl": "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps/made-up-updates_largeImage.png?v=1",
                "activities": {"activityTypes": [{"type": "messageNotify"}, {"type": "customNotification"}]},
                "bots": [{ "id": "00000000-0000-4000-8000-00000000b002", "scopes": ["Team"] }] }),
        json!({ "id": "00000000-0000-4000-8000-00000000a003", "name": "Made-up Pages", "developerName": "Made-up Maker",
                "shortDescription": "Pages made up", "accentColor": "#133a3c" }),
    ]
}

fn entitlements() -> Value {
    let team = json!([
        { "id": "00000000-0000-4000-8000-00000000a001", "state": "Installed" },
        { "id": "00000000-0000-4000-8000-00000000a002", "state": "Installed", "isNewGlobalPreInstalledApp": true },
        { "id": "00000000-0000-4000-8000-00000000a003", "state": "InstalledAndPermanent" },
    ]);
    let definitions: serde_json::Map<String, Value> = definitions()
        .into_iter()
        .filter_map(|definition| Some((definition.get("id")?.as_str()?.to_owned(), definition)))
        .collect();
    json!({ "value": {
        "teamsEntitlements": { "19:channel0001@thread.tacv2": team },
        "definitions": definitions,
    }})
}

fn store() -> Value {
    json!({ "appCategoriesDictionary": {
        "popularApps": { "apps": [{ "id": "00000000-0000-4000-8000-00000000a001" }], "totalAppsCount": 1 },
        "microsoft": { "apps": [
            { "id": "00000000-0000-4000-8000-00000000a002" }, { "id": "00000000-0000-4000-8000-00000000a003" },
        ], "totalAppsCount": 2 },
    }})
}

fn templates() -> Value {
    json!({ "value": [
        { "id": "made.up.template.project", "name": "Manage a Made-up Project", "shortDescription": "Coordinate made-up work.",
          "description": "A made-up template", "categories": ["General"], "channelCount": 2, "appCount": 1 },
        { "id": "made.up.template.event", "name": "Manage a Made-up Event", "shortDescription": "Run a made-up event.",
          "description": "Another made-up template", "categories": ["General"], "channelCount": 3, "appCount": 2 },
    ]})
}

fn template() -> Value {
    json!({ "value": { "teamDefinition": {
        "channels": [
            { "id": "General", "displayName": "General" },
            { "id": "made.up.template.project.channel1", "displayName": "Made-up Planning" },
        ],
        "apps": [{ "id": "00000000-0000-4000-8000-00000000a001" }],
    }}})
}
