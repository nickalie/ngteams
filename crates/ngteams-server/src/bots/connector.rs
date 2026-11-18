//! The Bot Connector API a bot calls at its `serviceUrl` (`/bot/…`, or an unsigned bot's
//! `/bot/u/<bot id>.<credential>/…`): send, reply, update and delete its messages, typing, the roster, and a 1:1 chat
//! it starts. The SDKs join URLs differently, so a doubled slash and any letter case in the fixed segments are taken.
//! Answers and refusals take the Bot Framework's shapes (`ResourceResponse`, `ErrorResponse`).
//!
//! A bot acts only as itself and only in a chat it is in; it starts a chat only with a person who may use its app.
//! Its credential is checked first, then its app's rate (`bots.connector_rate`, [`super::limits`]).

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{Extension, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_governor::GovernorLayer;

use super::Bot;
use super::activity::{self, Refused};
use crate::app::App;
use crate::chatsvc::service::{self as chats, BotChange, BotPost};
use crate::db::DbError;

/// The largest request a bot may send: an activity, or a picture in base64 (`super::pictures`).
const MAX_BODY: usize = 4 * 1024 * 1024;

pub(super) fn router(app: &Arc<App>) -> Router<Arc<App>> {
    let routes = Router::new().route("/bot/{*rest}", any(dispatch));
    let limited = match super::limits::connector(&app.config.bots) {
        Some(limit) => {
            routes.route_layer(GovernorLayer::new(limit).error_handler(|error| super::limits::refused(&error)))
        }
        None => routes,
    };
    // the last layer runs first: the credential, then the app's rate; only on these routes, not on the merged
    // router's fallback
    limited
        .route_layer(middleware::from_fn_with_state(Arc::clone(app), authenticate))
        .layer(DefaultBodyLimit::max(MAX_BODY))
}

/// The bot a connector request's credential proved, and how many of its path's segments name the credential
/// (`u/<bot id>.<credential>/` of an unsigned bot).
#[derive(Clone, Debug)]
struct Authenticated {
    bot: Bot,
    skip: usize,
}

/// Checks a request's credential before its app's rate: the bot it proves goes with the request.
async fn authenticate(State(app): State<Arc<App>>, mut request: Request, next: Next) -> Response {
    let path = request.uri().path().strip_prefix("/bot").unwrap_or_default().to_owned();
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    match authenticated(&app, request.headers(), &segments).await {
        Ok(found) => {
            request
                .extensions_mut()
                .insert(super::limits::AppKey(found.bot.app_id.clone()));
            request.extensions_mut().insert(found);
            next.run(request).await
        }
        Err(refusal) => refusal.into_response(),
    }
}

async fn authenticated(app: &App, headers: &HeaderMap, segments: &[&str]) -> Result<Authenticated, Refusal> {
    if is(segments.first(), "u") {
        let credential = segments.get(1).ok_or_else(Refusal::missing)?;
        Ok(Authenticated {
            bot: unsigned(app, credential).await?,
            skip: 2,
        })
    } else {
        Ok(Authenticated {
            bot: signed(app, headers).await?,
            skip: 0,
        })
    }
}

/// A refusal in the Bot Framework's `ErrorResponse` shape.
#[derive(Debug)]
struct Refusal {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl Refusal {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self { status, code, message }
    }

    const fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The bot's credential is missing or invalid.",
        )
    }

    const fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "Forbidden", "The bot may not do this.")
    }

    const fn missing() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "NotFound",
            "No such conversation, activity or member.",
        )
    }

    const fn bad(message: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "BadArgument", message)
    }
}

impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
        tracing::error!(%error, "bot connector database failure");
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ServiceError",
            "The service can't answer now.",
        )
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let body = json!({"error": {"code": self.code, "message": self.message}});
        (self.status, Json(body)).into_response()
    }
}

/// What a bot's own token says.
#[derive(Deserialize)]
struct BotClaims {
    sub: String,
    appid: String,
    idtyp: String,
}

async fn dispatch(
    State(app): State<Arc<App>>,
    Extension(found): Extension<Authenticated>,
    method: Method,
    Path(rest): Path<String>,
    body: Bytes,
) -> Response {
    let segments: Vec<&str> = rest.split('/').filter(|segment| !segment.is_empty()).collect();
    let path = &segments[found.skip.min(segments.len())..];
    match route(&app, &method, &found.bot, path, &body).await {
        Ok(response) => response,
        Err(refusal) => refusal.into_response(),
    }
}

/// Whether `segment` is the fixed segment `name`, in any letter case.
fn is(segment: Option<&&str>, name: &str) -> bool {
    segment.is_some_and(|segment| segment.eq_ignore_ascii_case(name))
}

async fn route(app: &App, method: &Method, bot: &Bot, path: &[&str], body: &[u8]) -> Result<Response, Refusal> {
    if !bot.enabled {
        return Err(Refusal::forbidden());
    }
    let caller = Caller { app, bot };
    if !is(path.first(), "v3") {
        return Err(unserved(path));
    }
    if is(path.get(1), "conversations") {
        return conversations(&caller, method, &path[2..], body).await;
    }
    if is(path.get(1), "attachments") && method == Method::GET {
        return super::pictures::read(app, bot, &path[2..])
            .await
            .map_err(|refused| picture_refused(&refused));
    }
    Err(unserved(path))
}

/// What isn't served: the Bot Framework's "not implemented" for its known routes, the token service's "no token",
/// and not found otherwise.
fn unserved(path: &[&str]) -> Refusal {
    let known = is(path.get(1), "batch")
        || (is(path.first(), "v1") && is(path.get(1), "meetings"))
        || path.iter().any(|segment| segment.eq_ignore_ascii_case("history"))
        || is(path.get(1), "teams");
    if known {
        Refusal::new(
            StatusCode::NOT_IMPLEMENTED,
            "NotImplemented",
            "This server doesn't serve this operation.",
        )
    } else {
        Refusal::missing()
    }
}

/// A signed bot: its own token for the bots' resource.
async fn signed(app: &App, headers: &HeaderMap) -> Result<Bot, Refusal> {
    let token = crate::auth::extract::bearer(headers).ok_or_else(Refusal::unauthorized)?;
    let claims: BotClaims = app
        .issuer
        .verify_app(token, &app.config.bots.scope)
        .await
        .map_err(|_| Refusal::unauthorized())?;
    if claims.idtyp != "app" || claims.sub != claims.appid {
        return Err(Refusal::unauthorized());
    }
    let bot = super::bot(&app.db, &claims.sub)
        .await?
        .ok_or_else(Refusal::unauthorized)?;
    if !bot.signed {
        return Err(Refusal::unauthorized());
    }
    Ok(bot)
}

/// An unsigned bot: the credential in its own `serviceUrl` (`<bot>.<credential>`).
async fn unsigned(app: &App, credential: &str) -> Result<Bot, Refusal> {
    let (id, given) = credential.split_once('.').ok_or_else(Refusal::unauthorized)?;
    let bot = super::bot(&app.db, id).await?.ok_or_else(Refusal::unauthorized)?;
    let expected = super::unsigned_credential(&bot);
    let proven = aws_lc_rs::constant_time::verify_slices_are_equal(expected.as_bytes(), given.as_bytes()).is_ok();
    if bot.signed || !proven {
        return Err(Refusal::unauthorized());
    }
    Ok(bot)
}

struct Caller<'a> {
    app: &'a App,
    bot: &'a Bot,
}

/// What a request under `v3/conversations/{id}/` asks for.
enum Operation<'a> {
    Send,
    Update(&'a str),
    Delete(&'a str),
    Roster,
    Paged,
    Member(&'a str),
    Upload,
}

fn operation<'a>(method: &Method, rest: &[&'a str]) -> Option<Operation<'a>> {
    let named = |segment: &str, name: &str| segment.eq_ignore_ascii_case(name);
    let operation = match (method.as_str(), rest) {
        ("POST", [activities] | [activities, _]) if named(activities, "activities") => Operation::Send,
        ("PUT", [activities, id]) if named(activities, "activities") => Operation::Update(id),
        ("DELETE", [activities, id]) if named(activities, "activities") => Operation::Delete(id),
        ("GET", [members]) if named(members, "members") => Operation::Roster,
        ("GET", [activities, _, members]) if named(activities, "activities") && named(members, "members") => {
            Operation::Roster
        }
        ("GET", [paged]) if named(paged, "pagedmembers") => Operation::Paged,
        ("POST", [attachments]) if named(attachments, "attachments") => Operation::Upload,
        ("GET", [members, member]) if named(members, "members") => Operation::Member(member),
        _ => return None,
    };
    Some(operation)
}

async fn conversations(caller: &Caller<'_>, method: &Method, path: &[&str], body: &[u8]) -> Result<Response, Refusal> {
    let Some(conversation) = path.first().copied() else {
        return match *method {
            Method::POST => create(caller, body).await,
            _ => Err(unserved(&["v3", "conversations"])),
        };
    };
    let rest = &path[1..];
    let Some(operation) = operation(method, rest) else {
        return Err(unserved(&[&["v3", "conversations", conversation], rest].concat()));
    };
    match operation {
        Operation::Send => send(caller, conversation, body).await,
        Operation::Update(id) => update(caller, conversation, id, body).await,
        Operation::Delete(id) => delete(caller, conversation, id).await,
        Operation::Roster => Ok(Json(roster(caller, conversation).await?).into_response()),
        Operation::Paged => {
            let members = roster(caller, conversation).await?;
            Ok(Json(json!({"members": members})).into_response())
        }
        Operation::Member(member) => one_member(caller, conversation, member).await,
        Operation::Upload => super::pictures::upload(caller.app, caller.bot, conversation, body)
            .await
            .map_err(|refused| picture_refused(&refused)),
    }
}

fn parse(body: &[u8]) -> Result<Value, Refusal> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| Refusal::bad("The body isn't an activity."))
}

fn refused(refused: &Refused) -> Refusal {
    match refused {
        Refused::Empty => Refusal::bad("The activity has neither text nor an attachment."),
        Refused::Attachment => Refusal::bad("An attachment isn't a card this server carries."),
    }
}

fn picture_refused(refused: &super::pictures::Refused) -> Refusal {
    use super::pictures::Refused;
    match refused {
        Refused::NotAPicture => Refusal::bad("An attachment isn't a picture this server keeps."),
        Refused::Denied => Refusal::forbidden(),
        Refused::Missing => Refusal::missing(),
        Refused::Quota => Refusal::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "InsufficientStorage",
            "The bot's storage is full.",
        ),
        Refused::Failed => Refusal::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ServiceError",
            "The service can't answer now.",
        ),
    }
}

struct Prepared {
    messages: Vec<activity::Made>,
    pictures: super::pictures::Taken,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    One,
    Any,
}

async fn made(caller: &Caller<'_>, conversation: &str, activity: &Value, expect: Expect) -> Result<Prepared, Refusal> {
    let mut activity = activity.clone();
    let attachments = super::pictures::extract(&mut activity);
    let origin = caller.app.config.origin();
    let mut messages =
        activity::messages(&activity, &origin, !attachments.is_empty()).map_err(|error| refused(&error))?;
    if expect == Expect::One && messages.len() != 1 {
        return Err(Refusal::bad("An update must contain one message."));
    }
    let pictures = super::pictures::take(caller.app, caller.bot, conversation, &attachments)
        .await
        .map_err(|refused| picture_refused(&refused))?;
    if let Some(message) = messages.iter_mut().find(|message| message.kind == activity::HTML) {
        message.content.extend(pictures.html.iter().map(String::as_str));
    }
    Ok(Prepared { messages, pictures })
}

fn bot_post<'a>(
    caller: &'a Caller<'_>,
    conversation: &'a str,
    prepared: &'a Prepared,
    message: &'a activity::Made,
) -> BotPost<'a> {
    BotPost {
        thread: conversation,
        bot: &caller.bot.id,
        name: &caller.bot.name,
        kind: message.kind,
        content: &message.content,
        pictures: if message.kind == activity::HTML {
            &prepared.pictures.ids
        } else {
            &[]
        },
    }
}

fn resource(id: i64) -> Response {
    Json(json!({ "id": id.to_string() })).into_response()
}

/// `POST …/activities[/{replyTo}]`: a message or typing, as the bot.
async fn send(caller: &Caller<'_>, conversation: &str, body: &[u8]) -> Result<Response, Refusal> {
    let activity = parse(body)?;
    match activity["type"].as_str().unwrap_or("message") {
        "message" => {
            let id = post(caller, conversation, &activity).await?;
            Ok(resource(id))
        }
        "typing" => {
            if chats::bot_typing(caller.app, conversation, &caller.bot.id, &caller.bot.name).await? {
                Ok(Json(json!({})).into_response())
            } else {
                Err(Refusal::forbidden())
            }
        }
        _ => Err(Refusal::bad("This server takes message and typing activities.")),
    }
}

/// Posts each message `activity` makes; the last one's id.
async fn post(caller: &Caller<'_>, conversation: &str, activity: &Value) -> Result<i64, Refusal> {
    let prepared = made(caller, conversation, activity, Expect::Any).await?;
    let result = post_messages(caller, conversation, &prepared).await;
    prepared.pictures.release(caller.app, caller.bot).await;
    result
}

async fn post_messages(caller: &Caller<'_>, conversation: &str, prepared: &Prepared) -> Result<i64, Refusal> {
    let mut last = None;
    for message in &prepared.messages {
        let posted = match chats::bot_post(caller.app, bot_post(caller, conversation, prepared, message)).await {
            Err(DbError::Conflict) => return Err(Refusal::missing()),
            other => other?,
        };
        last = Some(posted.ok_or_else(Refusal::forbidden)?);
    }
    last.ok_or_else(|| refused(&Refused::Empty))
}

fn message_id(id: &str) -> Result<i64, Refusal> {
    id.parse::<i64>().ok().filter(|id| *id > 0).ok_or_else(Refusal::missing)
}

fn changed(change: BotChange) -> Result<(), Refusal> {
    match change {
        BotChange::Done => Ok(()),
        BotChange::Missing => Err(Refusal::missing()),
        BotChange::Denied => Err(Refusal::forbidden()),
    }
}

/// `PUT …/activities/{id}`: the bot's own message, its text or cards now the activity's.
async fn update(caller: &Caller<'_>, conversation: &str, id: &str, body: &[u8]) -> Result<Response, Refusal> {
    let id = message_id(id)?;
    let activity = parse(body)?;
    let prepared = made(caller, conversation, &activity, Expect::One).await?;
    let result = async {
        let message = prepared.messages.first().ok_or_else(|| refused(&Refused::Empty))?;
        changed(chats::bot_edit(caller.app, bot_post(caller, conversation, &prepared, message), id).await?)?;
        Ok(resource(id))
    }
    .await;
    prepared.pictures.release(caller.app, caller.bot).await;
    result
}

/// `DELETE …/activities/{id}`: the bot's own message, soft-deleted.
async fn delete(caller: &Caller<'_>, conversation: &str, id: &str) -> Result<Response, Refusal> {
    let id = message_id(id)?;
    changed(chats::bot_delete(caller.app, conversation, &caller.bot.id, id).await?)?;
    Ok(StatusCode::OK.into_response())
}

/// The people of a chat the bot is in, as `TeamsChannelAccount`s.
async fn roster(caller: &Caller<'_>, conversation: &str) -> Result<Vec<Value>, Refusal> {
    let people = chats::bot_roster(caller.app, conversation, &caller.bot.id)
        .await?
        .ok_or_else(Refusal::forbidden)?;
    let tenant = caller.app.issuer.tenant();
    Ok(people.iter().map(|person| account(person, tenant)).collect())
}

fn account(person: &crate::db::users::User, tenant: &str) -> Value {
    let (given, surname) = person
        .display_name
        .split_once(' ')
        .map_or((person.display_name.as_str(), ""), |(given, surname)| (given, surname));
    json!({
        "id": format!("29:{}", person.id), "name": person.display_name,
        "aadObjectId": person.id, "objectId": person.id,
        "givenName": given, "surname": surname,
        "email": person.upn, "userPrincipalName": person.upn,
        "tenantId": tenant, "userRole": "user",
    })
}

/// `GET …/members/{id}`: one of them, by `29:`, MRI, object id or address.
async fn one_member(caller: &Caller<'_>, conversation: &str, member: &str) -> Result<Response, Refusal> {
    let wanted = person_id(member).to_lowercase();
    let people = chats::bot_roster(caller.app, conversation, &caller.bot.id)
        .await?
        .ok_or_else(Refusal::forbidden)?;
    let person = people
        .iter()
        .find(|person| person.id == wanted || person.upn == wanted)
        .ok_or_else(Refusal::missing)?;
    Ok(Json(account(person, caller.app.issuer.tenant())).into_response())
}

/// An account id from the forms a bot names a person in.
fn person_id(id: &str) -> &str {
    id.strip_prefix("29:")
        .or_else(|| id.strip_prefix("8:orgid:"))
        .unwrap_or(id)
}

#[derive(Deserialize)]
struct Create {
    #[serde(default)]
    members: Vec<CreateMember>,
    #[serde(default, rename = "isGroup")]
    is_group: bool,
    #[serde(default)]
    activity: Option<Value>,
    #[serde(default, rename = "channelData")]
    channel_data: Option<Value>,
    #[serde(default, rename = "tenantId")]
    tenant_id: Option<String>,
}

#[derive(Deserialize)]
struct CreateMember {
    #[serde(default)]
    id: String,
    #[serde(default, rename = "aadObjectId")]
    aad_object_id: Option<String>,
}

/// `POST v3/conversations`: the bot's 1:1 chat with a person who may use its app, made when it isn't there, with
/// the activity posted in it when one is given.
async fn create(caller: &Caller<'_>, body: &[u8]) -> Result<Response, Refusal> {
    let create: Create = serde_json::from_slice(body).map_err(|_| Refusal::bad("The body isn't a conversation."))?;
    let tenant = caller.app.issuer.tenant();
    let named = create
        .tenant_id
        .as_deref()
        .or_else(|| create.channel_data.as_ref()?.pointer("/tenant/id")?.as_str());
    if named.is_some_and(|named| !named.eq_ignore_ascii_case(tenant)) {
        return Err(Refusal::forbidden());
    }
    let [member] = create.members.as_slice() else {
        return Err(Refusal::bad("This server starts 1:1 chats with one person."));
    };
    if create.is_group {
        return Err(Refusal::bad("This server starts 1:1 chats with one person."));
    }
    let person = member
        .aad_object_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| person_id(&member.id))
        .to_lowercase();
    if uuid::Uuid::parse_str(&person).is_err() || !super::may_use(&caller.app.db, caller.bot, &person).await? {
        return Err(Refusal::forbidden());
    }
    let chat = chats::bot_chat(caller.app, &person, &caller.bot.id)
        .await?
        .ok_or_else(Refusal::forbidden)?;
    let mut answer = json!({"id": chat, "serviceUrl": super::service_url(caller.app, caller.bot)});
    if let Some(activity) = create.activity.filter(Value::is_object) {
        let id = post(caller, &chat, &activity).await?;
        answer["activityId"] = id.to_string().into();
    }
    Ok((StatusCode::CREATED, Json(answer)).into_response())
}
