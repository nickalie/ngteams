//! A team's outgoing webhooks on the middle tier, made up and kept (`docs/protocol/bots.md`, "Outgoing webhooks"):
//! `GET`/`POST beta/teams/<team>/outgoingWebhooks`, `PUT`/`DELETE …/<id>` and `GET …/<id>/picture`, in the shapes the
//! own server answers. The account manages every webhook it made and those a test adds as manageable; the address
//! shows only to a manager. A name the team has, in any letter case, is refused with `409`.

use base64::Engine as _;
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::json;
use crate::world::State;

/// One webhook as the mock keeps it.
#[derive(Clone, Debug)]
pub(crate) struct Hook {
    id: String,
    team: String,
    name: String,
    description: String,
    callback_url: String,
    can_manage: bool,
    picture: Option<(Vec<u8>, u64)>,
}

/// The webhooks of every team, in the order they were made, and how many were made.
#[derive(Debug, Default)]
pub(crate) struct Hooks {
    listed: Vec<Hook>,
    made: u64,
}

/// The made-up token every created webhook shows: base64, and no real key.
pub const MADE_UP_TOKEN: &str = "TWFkZVVwV2ViaG9va1Rva2VuTm90QVJlYWxLZXk=";

/// The answer to `request` when it is one of these; `None` for any other path.
pub(crate) fn serve(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let hooks = &mut state.outgoing_webhooks;
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", [.., "teams", team, "outgoingWebhooks"]) => Some(json(200, &list(hooks, team))),
        ("POST", [.., "teams", team, "outgoingWebhooks"]) => Some(create(hooks, team, &request.body)),
        ("PUT", [.., "teams", team, "outgoingWebhooks", id]) => Some(change(hooks, team, id, &request.body)),
        ("DELETE", [.., "teams", team, "outgoingWebhooks", id]) => Some(remove(hooks, team, id)),
        ("GET", [.., "teams", team, "outgoingWebhooks", id, "picture"]) => Some(picture(hooks, team, id)),
        _ => None,
    }
}

fn refusal(status: u16, code: &str) -> ResponseTemplate {
    json(status, &json!({"errorCode": code, "message": code}))
}

fn shown(hook: &Hook) -> Value {
    let mut value = json!({
        "id": hook.id, "mri": format!("28:{}", hook.id), "teamId": hook.team, "name": hook.name,
        "description": hook.description, "canManage": hook.can_manage,
        "createdBy": crate::LAB_A, "createdTime": "2026-11-27T10:00:00.000Z",
    });
    if hook.can_manage {
        value["callbackUrl"] = json!(hook.callback_url);
    }
    if let Some((_, version)) = &hook.picture {
        value["pictureUrl"] = json!(format!(
            "{}/beta/teams/{}/outgoingWebhooks/{}/picture?v={version}",
            crate::MIDDLE_TIER_BASE,
            hook.team,
            hook.id
        ));
    }
    value
}

fn list(hooks: &Hooks, team: &str) -> Value {
    let mut listed: Vec<&Hook> = hooks.listed.iter().filter(|hook| hook.team == team).collect();
    listed.sort_by_key(|hook| hook.name.to_lowercase());
    json!({"value": listed.into_iter().map(shown).collect::<Vec<_>>()})
}

/// What a body's picture asks.
enum Picture {
    Keep,
    Remove,
    Set(Vec<u8>),
}

fn picture_of(body: &Value) -> Result<Picture, ()> {
    match body.get("picture") {
        None => Ok(Picture::Keep),
        Some(Value::Null) => Ok(Picture::Remove),
        Some(Value::String(encoded)) => base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map(Picture::Set)
            .map_err(|_| ()),
        Some(_) => Err(()),
    }
}

fn text(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(Value::as_str).map(|text| text.trim().to_owned())
}

fn name_taken(hooks: &Hooks, team: &str, name: &str, other_than: Option<&str>) -> bool {
    hooks
        .listed
        .iter()
        .any(|hook| hook.team == team && Some(hook.id.as_str()) != other_than && hook.name.eq_ignore_ascii_case(name))
}

fn create(hooks: &mut Hooks, team: &str, body: &[u8]) -> ResponseTemplate {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return refusal(400, "BadRequest");
    };
    let (Some(name), Some(callback_url), Ok(picture)) = (
        text(&body, "name").filter(|name| !name.is_empty()),
        text(&body, "callbackUrl").filter(|url| url.starts_with("https://")),
        picture_of(&body),
    ) else {
        return refusal(400, "BadRequest");
    };
    if name_taken(hooks, team, &name, None) {
        return refusal(409, "OutgoingWebhookNameAlreadyExists");
    }
    hooks.made += 1;
    let hook = Hook {
        id: format!("00000000-0000-4000-8000-0000000b{:04x}", hooks.made),
        team: team.to_owned(),
        name,
        description: text(&body, "description").unwrap_or_default(),
        callback_url,
        can_manage: true,
        picture: match picture {
            Picture::Set(bytes) => Some((bytes, hooks.made)),
            Picture::Keep | Picture::Remove => None,
        },
    };
    let mut made = shown(&hook);
    made["securityToken"] = json!(MADE_UP_TOKEN);
    hooks.listed.push(hook);
    json(201, &json!({"value": made}))
}

fn change(hooks: &mut Hooks, team: &str, id: &str, body: &[u8]) -> ResponseTemplate {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return refusal(400, "BadRequest");
    };
    let Ok(picture) = picture_of(&body) else {
        return refusal(400, "BadRequest");
    };
    let name = text(&body, "name");
    if name
        .as_deref()
        .is_some_and(|name| name_taken(hooks, team, name, Some(id)))
    {
        return refusal(409, "OutgoingWebhookNameAlreadyExists");
    }
    hooks.made += 1;
    let version = hooks.made;
    let Some(hook) = hooks
        .listed
        .iter_mut()
        .find(|hook| hook.team == team && hook.id == id && hook.can_manage)
    else {
        return refusal(404, "NotFound");
    };
    if let Some(name) = name.filter(|name| !name.is_empty()) {
        hook.name = name;
    }
    if let Some(description) = text(&body, "description") {
        hook.description = description;
    }
    if let Some(url) = text(&body, "callbackUrl") {
        hook.callback_url = url;
    }
    match picture {
        Picture::Keep => {}
        Picture::Remove => hook.picture = None,
        Picture::Set(bytes) => hook.picture = Some((bytes, version)),
    }
    json(200, &json!({"value": shown(hook)}))
}

fn remove(hooks: &mut Hooks, team: &str, id: &str) -> ResponseTemplate {
    let before = hooks.listed.len();
    hooks
        .listed
        .retain(|hook| !(hook.team == team && hook.id == id && hook.can_manage));
    if hooks.listed.len() == before {
        return refusal(404, "NotFound");
    }
    ResponseTemplate::new(204)
}

fn picture(hooks: &Hooks, team: &str, id: &str) -> ResponseTemplate {
    match hooks
        .listed
        .iter()
        .find(|hook| hook.team == team && hook.id == id)
        .and_then(|hook| hook.picture.as_ref())
    {
        Some((bytes, _)) => ResponseTemplate::new(200)
            .insert_header("content-type", "image/png")
            .set_body_bytes(bytes.clone()),
        None => refusal(404, "NotFound"),
    }
}

/// A webhook on `team` named `name` that someone else made, which the account may change only when `can_manage`;
/// its id.
pub(crate) fn add(hooks: &mut Hooks, team: &str, name: &str, can_manage: bool) -> String {
    hooks.made += 1;
    let id = format!("00000000-0000-4000-8000-0000000b{:04x}", hooks.made);
    hooks.listed.push(Hook {
        id: id.clone(),
        team: team.to_owned(),
        name: name.to_owned(),
        description: String::new(),
        callback_url: "https://hooks.example.test/added".to_owned(),
        can_manage,
        picture: None,
    });
    id
}

/// The webhook `id`'s picture set to `picture`, under a new version.
pub(crate) fn set_picture(hooks: &mut Hooks, id: &str, picture: Vec<u8>) {
    hooks.made += 1;
    let version = hooks.made;
    if let Some(hook) = hooks.listed.iter_mut().find(|hook| hook.id == id) {
        hook.picture = Some((picture, version));
    }
}
