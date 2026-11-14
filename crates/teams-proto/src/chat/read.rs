//! The chat service's reads (`docs/protocol/chat.md`), each a [`Read`] (a `GET` under the chat base) sent through the
//! native transport and read into the model.
//!
//! - **Links**: the service pages and syncs through links it hands out, full URLs (`syncState`, `backwardLink`,
//!   `forwardLink`). One is followed only when it is on the transport's own chat base (scheme, host and base path
//!   exactly, as [`crate::chat_service_path`] reads a chat service URL) and names the read it was handed out
//!   for (the same conversation's messages, or the conversation list); anything else is [`ChatReadError::Link`] and
//!   nothing is sent. The link's path and query are sent as they came, decoded and encoded again by the transport.
//! - **A region hop**: a 404 with a `Location` (never seen) is [`ChatReadError::RegionMoved`], carrying its status
//!   only; the `Location` isn't kept or followed (the transport follows no redirect) until a capture shows its shape.

use std::iter;

use crate::{Cloud, chat_service_path_in};
use url::Url;

use crate::chat::model::{self, AppConsumptionHorizon, ConversationList, Malformed, MemberList, MessagePage, Thread};
use crate::client::{Client, Response};
use crate::endpoint::{Endpoint, Service};
use crate::http::{Credentials, HttpError, HttpTransport};
use crate::read::Read;

/// The largest page a read asks for: the web client's `pageSize`.
pub const MAX_PAGE_SIZE: u32 = 200;

/// The `view` of a message read, as the web client's sync read named it.
const MESSAGES_VIEW: &str = "msnp24Equivalent|supportsMessageProperties";

/// Why a chat read gave no answer.
#[derive(Debug, thiserror::Error)]
pub enum ChatReadError {
    /// The transport's error, a status other than a success included.
    #[error(transparent)]
    Http(HttpError),
    /// A 404 with a `Location`: a region hop, not followed. Only the status is kept.
    #[error("the chat service answered HTTP {status} with a Location (a region hop), which isn't followed")]
    RegionMoved { status: u16 },
    /// A link that isn't on the transport's chat base, or not of the read it was followed as; nothing was sent.
    #[error("not a link to {expected} on this chat base, so it isn't followed")]
    Link { expected: &'static str },
    /// The answer isn't the shape of the read's model.
    #[error(transparent)]
    Malformed(#[from] Malformed),
}

/// The chat service's code for a thread it no longer has: a 404 whose answer names it is a thread hard-deleted, not one
/// the account merely can't see.
const THREAD_NOT_FOUND: &str = "ThreadNotFound";

impl ChatReadError {
    /// The status of the service's answer when it answered with one other than a success (a region hop's included).
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http(HttpError::Status(answer)) => Some(answer.status()),
            Self::RegionMoved { status } => Some(*status),
            _ => None,
        }
    }

    /// The chat service's own code for why it refused the read (`AclCheckFailed` for an account no longer on the
    /// thread's roster, `ThreadNotFound` for a thread hard-deleted, …), read from its answer's body (see
    /// [`service_code`]); `None` for an error that isn't the service's answer, or one whose body names none.
    #[must_use]
    pub fn service_code(&self) -> Option<String> {
        match self {
            Self::Http(HttpError::Status(answer)) => service_code(answer.body()),
            _ => None,
        }
    }

    /// Whether the service answered that the thread is gone: a 404 (with no `Location`, which would be a region hop)
    /// naming `ThreadNotFound`. A 404 that names nothing, or another code ("Conversation was not found"), isn't.
    #[must_use]
    pub fn is_thread_gone(&self) -> bool {
        self.status() == Some(404)
            && matches!(self, Self::Http(_))
            && self.service_code().as_deref() == Some(THREAD_NOT_FOUND)
    }
}

/// The code a chat service error answer `body` names, the first of: `standardizedError.errorSubCodeString`,
/// `subErrorCode`, a textual `errorCode`, then the same of the JSON object its `message` may hold (the service nests
/// one answer in another), and last the word a plain `message` starts with before a `:` (`AclCheckFailed: …`). Only
/// a single word of letters and digits is taken, never the message's text: a body may name people or threads.
fn service_code(body: &[u8]) -> Option<String> {
    let answer: serde_json::Value = serde_json::from_slice(body).ok()?;
    code_of(&answer, 2)
}

/// [`service_code`] of one answer object, looking `depth` answers deep into a nested `message`.
fn code_of(answer: &serde_json::Value, depth: u8) -> Option<String> {
    let word = |value: Option<&serde_json::Value>| value.and_then(serde_json::Value::as_str).and_then(code_word);
    let named = word(answer.pointer("/standardizedError/errorSubCodeString"))
        .or_else(|| word(answer.get("subErrorCode")))
        .or_else(|| word(answer.get("errorCode")));
    if named.is_some() {
        return named;
    }
    let message = answer.get("message")?.as_str()?;
    match serde_json::from_str::<serde_json::Value>(message) {
        Ok(inner) if inner.is_object() => depth.checked_sub(1).and_then(|depth| code_of(&inner, depth)),
        _ => message.split_once(':').and_then(|(word, _)| code_word(word)),
    }
}

/// `text` when it is one word of ASCII letters and digits starting with a letter, at most 64 long: a code, not prose.
fn code_word(text: &str) -> Option<String> {
    let starts = text.chars().next().is_some_and(|first| first.is_ascii_alphabetic());
    (starts && text.len() <= 64 && text.chars().all(|c| c.is_ascii_alphanumeric())).then(|| text.to_owned())
}

/// The chat service's reads over a client's native transport.
#[derive(Debug)]
pub struct ChatReads<'c, C> {
    client: &'c Client<HttpTransport<C>>,
}

impl<'c, C: Credentials> ChatReads<'c, C> {
    #[must_use]
    pub fn new(client: &'c Client<HttpTransport<C>>) -> Self {
        Self { client }
    }

    /// The conversation list's first read, as Task 3's probe sent it:
    /// `GET v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1&targetType=All`.
    ///
    /// # Errors
    /// See [`ChatReadError`].
    pub async fn conversations(&self) -> Result<ConversationList, ChatReadError> {
        let endpoint = Endpoint::new(["v1", "users", "ME", "conversations"])
            .with_query("view", "msnp24Equivalent")
            .with_query("pageSize", MAX_PAGE_SIZE.to_string())
            .with_query("startTime", "1")
            .with_query("targetType", "All");
        Ok(ConversationList::from_body(self.get(endpoint).await?.body())?)
    }

    /// The account's mark-all-read horizon, from its own properties (`GET v1/users/ME/properties`). `None` when it has
    /// none, an account without any properties (the answer is 404) among them.
    ///
    /// # Errors
    /// See [`ChatReadError`].
    pub async fn app_consumption_horizon(&self) -> Result<Option<AppConsumptionHorizon>, ChatReadError> {
        let endpoint = Endpoint::new(["v1", "users", "ME", "properties"]);
        match self.get(endpoint).await {
            Ok(answer) => Ok(serde_json::from_slice(answer.body())
                .ok()
                .and_then(|properties| AppConsumptionHorizon::from_properties(&properties))),
            Err(ChatReadError::Http(HttpError::Status(answer))) if answer.status() == 404 => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The conversation list from a link it handed out (its `syncState` or `forwardLink`).
    ///
    /// # Errors
    /// [`ChatReadError::Link`] for a link that isn't the conversation list's on this chat base; otherwise as
    /// [`Self::conversations`].
    pub async fn conversations_from(&self, link: &str) -> Result<ConversationList, ChatReadError> {
        const EXPECTED: &str = "the conversation list";
        let endpoint = self
            .link(link, |rest| matches!(rest, [users, _, conversations] if users == "users" && conversations == "conversations"))
            .ok_or(ChatReadError::Link { expected: EXPECTED })?;
        Ok(ConversationList::from_body(self.get(endpoint).await?.body())?)
    }

    /// The newest page of `conversation`'s messages, `page_size` at most ([`MAX_PAGE_SIZE`]):
    /// `GET v1/users/ME/conversations/<id>/messages?view=msnp24Equivalent|supportsMessageProperties&pageSize=<n>`.
    /// A first read without a kept sync state wasn't captured; this is the sync read's form without its `syncState`.
    ///
    /// # Errors
    /// See [`ChatReadError`].
    pub async fn messages(&self, conversation: &str, page_size: u32) -> Result<MessagePage, ChatReadError> {
        let endpoint = Endpoint::new(["v1", "users", "ME", "conversations", conversation, "messages"])
            .with_query("view", MESSAGES_VIEW)
            .with_query("pageSize", page(page_size));
        Ok(MessagePage::from_body(self.get(endpoint).await?.body())?)
    }

    /// The sender's copy named by a blocked message's verdict, as the web client's message read fetches it.
    /// `None` when the service no longer has it (404); the stream is never read as a thread.
    ///
    /// # Errors
    /// See [`ChatReadError`]. A response naming another message is malformed.
    pub async fn blocked_copy(&self, id: &str) -> Result<Option<model::Message>, ChatReadError> {
        let endpoint = Endpoint::new([
            "v1",
            "users",
            "ME",
            "conversations",
            super::GENERIC_STREAM,
            "messages",
            id,
        ])
        .with_query("view", MESSAGES_VIEW);
        let answer = match self.get(endpoint).await {
            Ok(answer) => answer,
            Err(error) if error.status() == Some(404) && matches!(error, ChatReadError::Http(_)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let message: model::Message = model::object(answer.body(), "a blocked message's copy")?;
        if message.id != id {
            return Err(model::Malformed("a blocked message's copy with the requested id").into());
        }
        Ok(Some(message))
    }

    /// `conversation`'s messages from a link a page of them handed out (`syncState`, `backwardLink`, `forwardLink`).
    ///
    /// # Errors
    /// [`ChatReadError::Link`] for a link that isn't to `conversation`'s messages on this chat base; otherwise as
    /// [`Self::messages`].
    pub async fn messages_from(&self, conversation: &str, link: &str) -> Result<MessagePage, ChatReadError> {
        const EXPECTED: &str = "the conversation's messages";
        let endpoint = self
            .link(link, |rest| {
                matches!(rest, [users, _, conversations, id, messages]
                    if users == "users" && conversations == "conversations" && id == conversation && messages == "messages")
            })
            .ok_or(ChatReadError::Link { expected: EXPECTED })?;
        Ok(MessagePage::from_body(self.get(endpoint).await?.body())?)
    }

    /// The page around a time (ms), both ways, as the web client read it around the time its account joined:
    /// `GET …/messages/epochTimeStamp/<ms>?pageSize=<n>&direction=BIDIRECTIONAL&includeMetadata=true&locationType=Primary`.
    ///
    /// # Errors
    /// See [`ChatReadError`].
    pub async fn messages_around(
        &self,
        conversation: &str,
        time_ms: u64,
        page_size: u32,
    ) -> Result<MessagePage, ChatReadError> {
        let time = time_ms.to_string();
        let segments = [
            "v1",
            "users",
            "ME",
            "conversations",
            conversation,
            "messages",
            "epochTimeStamp",
            &time,
        ];
        let endpoint = Endpoint::new(segments)
            .with_query("pageSize", page(page_size))
            .with_query("direction", "BIDIRECTIONAL")
            .with_query("includeMetadata", "true")
            .with_query("locationType", "Primary");
        Ok(MessagePage::from_body(self.get(endpoint).await?.body())?)
    }

    /// The thread read: `GET v1/threads/<id>?view=msnp24Equivalent`.
    ///
    /// # Errors
    /// See [`ChatReadError`].
    pub async fn thread(&self, id: &str) -> Result<Thread, ChatReadError> {
        let endpoint = Endpoint::new(["v1", "threads", id]).with_query("view", "msnp24Equivalent");
        Ok(model::object(self.get(endpoint).await?.body(), "a thread")?)
    }

    /// The member list: `GET v1/threads/<id>/members?view=msnp24Equivalent&pageSize=100`, as Task 8's probe sent it.
    /// Only an unpaged answer was ever seen; any paging key stays in [`MemberList::extra`].
    ///
    /// # Errors
    /// See [`ChatReadError`].
    pub async fn members(&self, id: &str) -> Result<MemberList, ChatReadError> {
        let endpoint = Endpoint::new(["v1", "threads", id, "members"])
            .with_query("view", "msnp24Equivalent")
            .with_query("pageSize", "100");
        Ok(model::object(self.get(endpoint).await?.body(), "a member list")?)
    }

    /// `link` as an endpoint, if it is on the transport's chat base and its path after `v1` is one `expected` takes.
    fn link(&self, link: &str, expected: impl Fn(&[String]) -> bool) -> Option<Endpoint> {
        link_endpoint_in(
            self.client.transport().cloud(),
            self.client.transport().chat_base(),
            link,
        )
        .filter(|endpoint| {
            endpoint
                .segments()
                .split_first()
                .is_some_and(|(_, rest)| expected(rest))
        })
    }

    async fn get(&self, endpoint: Endpoint) -> Result<Response, ChatReadError> {
        match self.client.read(&ChatRead(endpoint)).await {
            Ok(answer) => Ok(answer),
            Err(HttpError::Status(answer)) if answer.status() == 404 && answer.location().is_some() => {
                Err(ChatReadError::RegionMoved { status: 404 })
            }
            Err(error) => Err(ChatReadError::Http(error)),
        }
    }
}

/// A read of the chat service.
struct ChatRead(Endpoint);

impl Read for ChatRead {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn endpoint(&self) -> Endpoint {
        self.0.clone()
    }
}

fn page(page_size: u32) -> String {
    page_size.clamp(1, MAX_PAGE_SIZE).to_string()
}

/// A link the service handed out, as an endpoint on `chat_base` (`HttpTransport::chat_base`): `None` unless it is a
/// chat service URL ([`crate::chat_service_path`]) with no fragment whose scheme, host, port and base path are
/// exactly the base's. Its query comes along, pair by pair.
#[must_use]
pub fn link_endpoint(chat_base: &str, link: &str) -> Option<Endpoint> {
    link_endpoint_in(&Cloud::Microsoft, chat_base, link)
}

/// A service-issued link on this account's exact chat base and cloud, including its TLS port.
#[must_use]
pub fn link_endpoint_in(cloud: &Cloud, chat_base: &str, link: &str) -> Option<Endpoint> {
    let url = Url::parse(link).ok()?;
    let base = Url::parse(&format!("{}/v1", chat_base.trim_end_matches('/'))).ok()?;
    let on_base = url.scheme() == base.scheme()
        && url.host_str() == base.host_str()
        && url.port() == base.port()
        && url.fragment().is_none()
        && url
            .path()
            .strip_prefix(base.path())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
    if !on_base {
        return None;
    }
    let rest = chat_service_path_in(cloud, &url)?;
    let endpoint = Endpoint::new(iter::once("v1".to_owned()).chain(rest));
    Some(
        url.query_pairs()
            .fold(endpoint, |endpoint, (name, value)| endpoint.with_query(name, value)),
    )
}

#[cfg(test)]
mod tests {
    //! The answers here are synthetic: the chat service's error shapes with made-up text and ids.

    use super::*;
    use crate::client::Response;
    use serde_json::json;

    fn refused(status: u16, body: &serde_json::Value) -> ChatReadError {
        ChatReadError::Http(HttpError::Status(Box::new(Response::new(status, body.to_string()))))
    }

    /// The service's nested form: an answer whose `message` holds another, beside its standardized error.
    fn nested(status: u16, code: &str, text: &str) -> serde_json::Value {
        json!({
            "errorCode": status,
            "message": json!({"errorCode": status, "message": text, "subErrorCode": code}).to_string(),
            "standardizedError": {"errorCode": status, "errorSubCode": 1, "errorSubCodeString": code},
        })
    }

    #[test]
    fn a_hard_deleted_thread_is_gone_and_a_missing_conversation_is_not() {
        let gone = refused(404, &nested(404, "ThreadNotFound", "Synthetic thread hard-deleted."));
        assert_eq!(gone.service_code().as_deref(), Some("ThreadNotFound"));
        assert!(gone.is_thread_gone());

        let missing = refused(404, &nested(404, "RequestHandlerException", "Synthetic not found"));
        assert_eq!(missing.service_code().as_deref(), Some("RequestHandlerException"));
        assert!(!missing.is_thread_gone());

        // the code alone where the nested answer holds it, and a plain 404 naming nothing
        let inner_only = json!({"errorCode": 404, "message": json!({"subErrorCode": "ThreadNotFound"}).to_string()});
        assert!(refused(404, &inner_only).is_thread_gone());
        assert!(!refused(404, &json!({})).is_thread_gone());
        assert!(
            !refused(403, &nested(403, "ThreadNotFound", "x")).is_thread_gone(),
            "a 404 only"
        );
        assert!(
            !ChatReadError::RegionMoved { status: 404 }.is_thread_gone(),
            "a region hop isn't"
        );
    }

    #[test]
    fn the_code_is_one_word_never_the_message() {
        let acl = json!({"errorCode": 209, "message": "AclCheckFailed: initiator 8:orgid:synthetic isn't on 92:x"});
        assert_eq!(refused(403, &acl).service_code().as_deref(), Some("AclCheckFailed"));
        assert_eq!(refused(403, &acl).status(), Some(403));
        let textual = json!({"errorCode": "ThreadNotFound", "message": "Synthetic"});
        assert_eq!(refused(404, &textual).service_code().as_deref(), Some("ThreadNotFound"));
        for body in [
            json!({"message": "Synthetic prose with no code"}),
            json!({"message": "two words: synthetic"}),
            json!({"errorCode": 403, "message": "8:orgid:synthetic"}),
            json!([1, 2]),
        ] {
            assert_eq!(refused(403, &body).service_code(), None, "{body}");
        }
        assert_eq!(service_code(b"not json"), None);
        assert_eq!(ChatReadError::Link { expected: "x" }.status(), None);
        assert_eq!(ChatReadError::Link { expected: "x" }.service_code(), None);
    }
}
