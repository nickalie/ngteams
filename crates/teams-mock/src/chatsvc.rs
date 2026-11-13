//! The chat service's front: every request's path under the chat base, its credential and headers (M1 Task 3), and
//! the route it takes to a read ([`crate::reads`]) or a write ([`crate::writes`]).

use std::sync::{Arc, Mutex, PoisonError};

use percent_encoding::percent_decode_str;
use serde_json::Value;
use wiremock::{Request, Respond, ResponseTemplate};

use crate::world::State;
use crate::{REGION, reads, writes};

/// The responder the mock's server answers every request with.
pub(crate) struct ChatService {
    state: Arc<Mutex<State>>,
}

impl ChatService {
    pub(crate) fn new(state: Arc<Mutex<State>>) -> Self {
        Self { state }
    }
}

impl Respond for ChatService {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        handle(&mut state, request)
    }
}

/// A request the mock models. Ids are percent-decoded.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// `GET v1/users/<owner>/conversations`: `ME`, or the caller's own MRI (the sync link names it).
    Conversations {
        owner: String,
    },
    Messages {
        chat: String,
    },
    Around {
        chat: String,
    },
    Post {
        chat: String,
    },
    Edit {
        chat: String,
        message: String,
    },
    Delete {
        chat: String,
        message: String,
    },
    Reaction {
        chat: String,
        message: String,
        remove: bool,
    },
    ConversationProperty {
        chat: String,
    },
    OwnConversation {
        chat: String,
    },
    Thread {
        id: String,
    },
    Members {
        id: String,
    },
    Filtered {
        id: String,
    },
    UserProperties {
        write: bool,
    },
    CreateChat,
    AddMembers {
        id: String,
    },
}

impl Route {
    /// The route of `method` on `path` (the segments after `v1`), if the mock models it.
    fn of(method: &str, path: &[String]) -> Option<Self> {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        let text = |text: &str| text.to_owned();
        let route = match (method, path.as_slice()) {
            ("GET" | "PUT", ["users", "ME", "properties"]) => Self::UserProperties { write: method == "PUT" },
            ("GET", ["users", owner, "conversations"]) => Self::Conversations { owner: text(owner) },
            ("GET", ["threads", id]) => Self::Thread { id: text(id) },
            ("GET", ["threads", id, "members"]) => Self::Members { id: text(id) },
            ("GET", ["threads", id, "filtered"]) => Self::Filtered { id: text(id) },
            ("POST", ["threads"]) => Self::CreateChat,
            ("PUT", ["threads", id, "members"]) => Self::AddMembers { id: text(id) },
            (method, ["users", "ME", "conversations", chat, rest @ ..]) => return Self::of_chat(method, chat, rest),
            _ => return None,
        };
        Some(route)
    }

    /// The route of `method` on `v1/users/ME/conversations/<chat>/<rest>`.
    fn of_chat(method: &str, chat: &str, rest: &[&str]) -> Option<Self> {
        let chat = chat.to_owned();
        let route = match (method, rest) {
            ("GET", []) => Self::OwnConversation { chat },
            ("GET", ["messages"]) => Self::Messages { chat },
            ("GET", ["messages", "epochTimeStamp", _]) => Self::Around { chat },
            ("POST", ["messages"]) => Self::Post { chat },
            ("PUT", ["messages", message]) => Self::Edit {
                chat,
                message: (*message).to_owned(),
            },
            ("DELETE", ["messages", message]) => Self::Delete {
                chat,
                message: (*message).to_owned(),
            },
            ("PUT" | "DELETE", ["messages", message, "properties"]) => Self::Reaction {
                chat,
                message: (*message).to_owned(),
                remove: method == "DELETE",
            },
            ("PUT", ["properties"]) => Self::ConversationProperty { chat },
            _ => return None,
        };
        Some(route)
    }
}

/// What a handler gets: the caller's MRI, the query and the body.
pub(crate) struct Call<'a> {
    pub(crate) caller: String,
    query: Vec<(String, String)>,
    pub(crate) body: &'a [u8],
}

impl Call<'_> {
    /// The first value of the query parameter `name`.
    pub(crate) fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The body as a JSON object.
    pub(crate) fn object(&self) -> Option<serde_json::Map<String, Value>> {
        match serde_json::from_slice(self.body) {
            Ok(Value::Object(map)) => Some(map),
            _ => None,
        }
    }
}

fn handle(state: &mut State, request: &Request) -> ResponseTemplate {
    if let Some(answer) = crate::trouter::registrar(state, request) {
        return answer;
    }
    if let Some(answer) = crate::csa::teams(state, request) {
        return answer;
    }
    if let Some(answer) = crate::team_pages::serve(request) {
        return answer;
    }
    if let Some(answer) = crate::channels::serve(state, request) {
        return answer;
    }
    if let Some(answer) = crate::meeting_options::serve(state, request) {
        return answer;
    }
    if let Some(answer) = crate::calendar::calendar_view(state, request) {
        return answer;
    }
    if let Some(answer) = crate::ups::own_availability(state, request) {
        return answer;
    }
    if let Some(answer) = crate::calling::relay_tokens(state, request) {
        return answer;
    }
    if let Some(answer) = crate::calling::serve(state, request) {
        return answer;
    }
    let route = chat_path(request).and_then(|path| Route::of(request.method.as_str(), &path));
    let thread_read = matches!(route, Some(Route::Thread { .. }));
    let Some(caller) = authenticate(state, request, thread_read) else {
        return status(401);
    };
    let Some(route) = route else {
        return status(404);
    };
    let call = Call {
        caller,
        query: request.url.query_pairs().into_owned().collect(),
        body: &request.body,
    };
    dispatch(state, route, &call)
}

fn dispatch(state: &mut State, route: Route, call: &Call<'_>) -> ResponseTemplate {
    match route {
        Route::Conversations { owner } => reads::conversations(state, call, &owner),
        Route::Messages { chat } => reads::messages(state, call, &chat),
        Route::Around { chat } => reads::around(state, call, &chat),
        Route::Thread { id } => reads::thread(state, call, &id),
        Route::Members { id } => reads::members(state, call, &id),
        Route::Filtered { id } => reads::filtered(state, call, &id),
        Route::Post { chat } => writes::post(state, call, &chat),
        Route::Edit { chat, message } => writes::edit(state, call, &chat, &message),
        Route::Delete { chat, message } => writes::delete(state, call, &chat, &message),
        Route::Reaction {
            chat,
            message,
            remove: false,
        } if call.query("name") == Some("isread") => writes::activity_read(state, call, &chat, &message),
        Route::Reaction { chat, message, remove } => writes::reaction(state, call, &chat, &message, remove),
        Route::ConversationProperty { chat } => match call.query("name") {
            Some("channelNotificationSettings" | "followAllRc" | "alerts" | "consumptionHorizonBookmark") => {
                writes::own_property(state, call, &chat)
            }
            _ => writes::read_horizon(state, call, &chat),
        },
        Route::OwnConversation { chat } => reads::own_conversation(state, call, &chat),
        Route::UserProperties { write } => user_properties(state, call, write),
        Route::CreateChat => writes::create_chat(state, call),
        Route::AddMembers { id } => writes::add_members(state, call, &id),
    }
}

fn user_properties(state: &mut State, call: &Call<'_>, write: bool) -> ResponseTemplate {
    if !write {
        return state.user_graph_settings.get(&call.caller).map_or_else(
            || {
                json(
                    404,
                    &serde_json::json!({"errorCode": 404, "standardizedError": {"errorCode": 404, "errorSubCode": 1}}),
                )
            },
            |settings| json(200, &serde_json::json!({"userGraphSettings": settings})),
        );
    }
    let Some(body) = call.object().filter(|body| body.len() == 1) else {
        return status(400);
    };
    if call.query("name") != Some("userGraphSettings") {
        return status(400);
    }
    let Some(settings) = body.get("userGraphSettings").and_then(Value::as_str) else {
        return status(400);
    };
    if serde_json::from_str::<serde_json::Map<String, Value>>(settings).is_err() {
        return status(400);
    }
    state
        .user_graph_settings
        .insert(call.caller.clone(), settings.to_owned());
    status(200)
}

/// The path's segments after `/api/chatsvc/<region>/v1/`, percent-decoded; `None` for any other path.
fn chat_path(request: &Request) -> Option<Vec<String>> {
    let segments = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<Vec<String>>>()?;
    match segments.as_slice() {
        [api, chatsvc, region, v1, rest @ ..]
            if api == "api" && chatsvc == "chatsvc" && region == REGION && v1 == "v1" =>
        {
            Some(rest.to_vec())
        }
        _ => None,
    }
}

/// The caller's MRI: the account whose token is the `Authorization: Bearer` header's, with `X-MS-Migration: True` and
/// `BehaviorOverride: redirectAs404` beside it unless `thread_read` (the token alone is enough there).
fn authenticate(state: &State, request: &Request, thread_read: bool) -> Option<String> {
    let header = |name: &str| request.headers.get(name).and_then(|value| value.to_str().ok());
    let token = header("authorization").and_then(|value| {
        let (scheme, token) = value.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then_some(token)
    })?;
    let account = state.account_by_token(token)?;
    let migrated = header("x-ms-migration") == Some("True") && header("behavioroverride") == Some("redirectAs404");
    (thread_read || migrated).then(|| account.mri.clone())
}

/// An answer with `code` and no body.
pub(crate) fn status(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code)
}

/// An answer with `code` and `body` as JSON.
pub(crate) fn json(code: u16, body: &Value) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(body)
}

#[cfg(test)]
mod tests {
    use super::Route;

    fn route(method: &str, path: &str) -> Option<Route> {
        let path: Vec<String> = path.split('/').map(str::to_owned).collect();
        Route::of(method, &path)
    }

    #[test]
    fn requests_take_the_route_their_method_and_path_name() {
        let chat = "19:x@thread.v2".to_owned();
        assert_eq!(
            route("PUT", "users/ME/conversations/19:x@thread.v2/messages/1/properties"),
            Some(Route::Reaction {
                chat: chat.clone(),
                message: "1".to_owned(),
                remove: false
            })
        );
        assert_eq!(
            route("GET", "users/ME/conversations/19:x@thread.v2/messages/epochTimeStamp/5"),
            Some(Route::Around { chat: chat.clone() })
        );
        assert_eq!(
            route("GET", "users/ME/conversations/19:x@thread.v2"),
            Some(Route::OwnConversation { chat: chat.clone() })
        );
        assert_eq!(
            route("PUT", "users/ME/conversations/19:x@thread.v2/properties"),
            Some(Route::ConversationProperty { chat })
        );
        assert_eq!(route("POST", "threads"), Some(Route::CreateChat));
        assert_eq!(
            route("PUT", "threads/19:x@thread.v2/members"),
            Some(Route::AddMembers {
                id: "19:x@thread.v2".to_owned()
            })
        );
        for (method, path) in [
            ("PATCH", "users/ME/conversations/19:x@thread.v2/messages/1"),
            ("POST", "users/someone/conversations/19:x@thread.v2/messages"),
            ("POST", "agents/28:bot/invoke"),
        ] {
            assert_eq!(route(method, path), None, "{method} {path}");
        }
    }
}
