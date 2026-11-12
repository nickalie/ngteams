//! What Manage team's Apps tab and "More create team options" read (`docs/protocol/teams.md`), made up: a team's
//! installed apps, the store's apps and their definitions, Teams' templates and one template's channels and apps, and no
//! Microsoft 365 group to make a team of. Requests not implemented by these fixtures remain unmatched.

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
        _ => None,
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
