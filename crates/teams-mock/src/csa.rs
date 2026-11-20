//! CSA's teams read (M2 Task 16): `GET /api/csa/<region>/api/v3/teams/users/me` with the web client's query exactly,
//! answered with `csa/teams.json`'s body (the same teams for every account: the mock's one team, [`crate::CHANNEL`],
//! with its other channel, [`crate::TEAM_CHANNEL`], that channel every account's one favourite, after the account's
//! own notes when a test lists them, [`crate::ChatServiceMock::list_notes_in_favorites`]). It takes the account's
//! token alone, as the probe sent it; a request without a token the mock handed out is a 401, the read with any other
//! query a 404 (the service's answers to those weren't captured).

use percent_encoding::percent_decode_str;
use wiremock::{Request, ResponseTemplate};

use crate::REGION;
use crate::chatsvc::{json, status};
use crate::world::State;

/// The read's query, in order (`teams_proto::CSA_TEAMS_QUERY`, which the mock doesn't depend on).
const QUERY: [(&str, &str); 5] = [
    ("isPrefetch", "false"),
    ("enableMembershipSummary", "true"),
    ("supportsAdditionalSystemGeneratedFolders", "true"),
    ("supportsSliceItems", "true"),
    ("enableEngageCommunities", "false"),
];

/// The answer to `request` when it is under the CSA base; `None` for any other path.
pub(crate) fn teams(state: &State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let rest = match segments.as_slice() {
        [api, csa, region, rest @ ..] if api == "api" && csa == "csa" && region == REGION => rest,
        _ => return None,
    };
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token);
    let Some(caller) = token.and_then(|token| state.account_by_token(token)) else {
        return Some(status(401));
    };
    let notes_listed = state.notes_in_favorites.contains(&caller.mri);
    let query: Vec<(String, String)> = request.url.query_pairs().into_owned().collect();
    if request.method.as_str() == "GET" && rest == ["api", "v1", "teams", "users", "me", "conversationFolders"] {
        // Synthetic empty whole account hierarchy, used by own-visibility and section reads.
        return Some(json(
            200,
            &serde_json::json!({"isPartialResponse":false,"folderHierarchyVersion":1,"conversationFolderOrder":[],"conversationFolders":[]}),
        ));
    }
    let the_read = request.method.as_str() == "GET"
        && rest == ["api", "v3", "teams", "users", "me"]
        && query
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .eq(QUERY);
    Some(if the_read {
        let mut answer = with_made_channels(&state.fixtures.teams, state);
        if notes_listed {
            list_notes_first(&mut answer);
        }
        json(200, &answer)
    } else {
        status(404)
    })
}

/// The answer with the account's own notes as the first item of its Favorites folder, as Teams listed them
/// (`docs/protocol/chat.md`, "Favorites").
fn list_notes_first(answer: &mut serde_json::Value) {
    let folders = answer
        .pointer_mut("/conversationFolders/conversationFolders")
        .and_then(serde_json::Value::as_array_mut);
    let favorites = folders.and_then(|folders| folders.iter_mut().find(|folder| folder["folderType"] == "Favorites"));
    if let Some(items) = favorites
        .and_then(|folder| folder.get_mut("conversationFolderItems"))
        .and_then(serde_json::Value::as_array_mut)
    {
        items.insert(
            0,
            serde_json::json!({
                "conversationId": crate::NOTES,
                "createdTime": 1_760_000_000_050_u64,
                "lastUpdatedTime": 1_760_000_000_050_u64,
                "threadType": "streamofnotes",
            }),
        );
    }
}

/// The fixture's answer with the channels made since (`channels.rs`) in their teams.
fn with_made_channels(answer: &serde_json::Value, state: &State) -> serde_json::Value {
    let mut answer = answer.clone();
    if let Some(teams) = answer.get_mut("teams").and_then(serde_json::Value::as_array_mut) {
        for made in &state.channels.made {
            let team = teams.iter_mut().find(|team| team["id"] == made.team.as_str());
            if let Some(channels) = team.and_then(|team| team["channels"].as_array_mut()) {
                channels.push(made.entry.clone());
            }
        }
    }
    answer
}
