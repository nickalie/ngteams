//! What the team pages read (`docs/protocol/teams.md`), made up: a team's people, settings, tags, code, channels and
//! analytics, the invitations waiting, the people to add and the teams to join. Requests not implemented by these
//! fixtures remain unmatched.

use percent_encoding::percent_decode_str;
use serde_json::json;
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::json;

/// The answer to `request` when it is one of these; `None` for any other path.
pub(crate) fn serve(request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let method = request.method.as_str();
    let query = request.url.query().unwrap_or_default();
    match (method, segments.as_slice()) {
        ("GET", [.., "threads", _, "members"]) if query.contains("selectMemberRoles") => {
            Some(json(200, &roster(query.contains("Admin"))))
        }
        ("GET", [.., "teams", _, "tagCards"]) => Some(json(200, &tags())),
        ("GET", [.., "teams", _, "joinCode"]) => Some(ResponseTemplate::new(204)),
        ("GET", [.., "groups", _, "channels"]) => Some(json(200, &channels())),
        ("GET", [.., "users", "me", "invitations"]) => Some(json(200, &json!({ "invitations": [] }))),
        ("GET", [.., "analytics", "teams", _, "summary-timeseries"]) => {
            Some(json(200, &json!({ "teams": analytics() })))
        }
        ("GET", [.., "analytics", "me", "teams", "summary-timeseries"]) => {
            Some(json(200, &json!({ "teams": [analytics()] })))
        }
        ("POST", [.., "teams", "findJoinRequests"]) => Some(json(200, &json!({ "joinRequestsPerTeam": {} }))),
        ("POST", [.., "users", "searchV2"]) => Some(json(200, &people())),
        ("GET", [.., "teams", "suggested"]) => Some(json(200, &joinable())),
        ("GET", [.., "analytics", "teams", _, "channels", _, "summary-timeseries"]) => {
            Some(json(200, &json!({ "channels": analytics() })))
        }
        _ => crate::team_apps::serve(method, &segments, &request.body),
    }
}

fn roster(owners: bool) -> serde_json::Value {
    let members = if owners {
        json!([{ "id": crate::LAB_A, "role": "Admin", "hidden": false }])
    } else {
        json!([
            { "id": crate::LAB_B, "role": "User", "hidden": false },
            { "id": crate::LAB_C, "role": "User", "hidden": false },
            { "id": "28:00000000-0000-4000-8000-0000000000ee", "role": "User", "hidden": false },
        ])
    };
    json!({ "totalMemberCount": 4, "members": members, "isDeleted": false })
}

/// A team's settings as its thread keeps them: JSON, as text.
pub(crate) fn settings() -> serde_json::Value {
    let settings = json!({
        "userSettings": {
            "createTopic": true, "updateTopic": true, "deleteTopic": true, "createPrivateSpace": true,
            "installApp": true, "uninstallApp": true, "uploadCustomApp": true, "createTab": true, "deleteTab": true,
            "createIntegration": true, "updateIntegration": true, "deleteIntegration": true,
            "deleteEnabled": true, "editEnabled": true, "teamMention": true, "channelMention": true,
            "giphyEnabled": true, "stickersEnabled": true, "customMemesEnabled": true,
        },
        "guestSettings": { "createTopic": false, "updateTopic": false, "deleteTopic": false },
    });
    json!(settings.to_string())
}

fn tags() -> serde_json::Value {
    json!({
        "tagCards": [
            { "id": "tag-1", "tagName": "Release", "tagDescription": "Who ships the release", "memberCount": 2, "version": 3 },
        ],
        "teamTagSettings": { "onlyOwnersCanUpdate": true, "enableAutomaticTags": false },
    })
}

fn channels() -> serde_json::Value {
    json!({
        "channels": [
            { "threadId": "19:channel0001@thread.tacv2", "displayName": "General", "description": "Made-up team's general channel",
              "channelType": "Standard", "channelState": "Active", "archiveState": "None" },
            { "threadId": "19:channel0002@thread.tacv2", "displayName": "Made-up Channel", "description": "Made-up channel",
              "channelType": "Standard", "channelState": "Active", "archiveState": "None" },
        ],
    })
}

fn analytics() -> serde_json::Value {
    let days = [3, 0, 1, 4, 2, 5, 3];
    let series: Vec<_> = days
        .iter()
        .enumerate()
        .map(|(at, value)| json!({ "date": format!("2026-10-{:02}T00:00:00", at + 20), "value": value }))
        .collect();
    json!({
        "id": "00000000-0000-4000-8000-0000000000aa",
        "displayName": "Made-up Team",
        "metrics": {
            "activeUsers": { "value": 5, "timeSeries": series },
            "guests": { "value": 0 },
            "postMessages": { "value": 12 },
            "replyMessages": { "value": 20 },
            "reactions": { "value": 4 },
            "mentions": { "value": 7 },
            "meetingsOrganized": { "value": 1 },
        },
    })
}

fn people() -> serde_json::Value {
    json!({
        "value": [
            { "mri": crate::LAB_B, "displayName": "Lab B", "email": "lab-b@lab.example.com" },
            { "mri": crate::LAB_C, "displayName": "Lab C", "email": "lab-c@lab.example.com" },
        ],
    })
}

fn joinable() -> serde_json::Value {
    json!({
        "value": [
            { "groupId": "00000000-0000-4000-8000-0000000000ab", "displayName": "Made-up Public Team",
              "description": "A made-up team to join", "accessType": 3 },
            { "groupId": "00000000-0000-4000-8000-0000000000ac", "displayName": "Second Made-up Team",
              "description": "Another", "accessType": 3 },
        ],
    })
}
