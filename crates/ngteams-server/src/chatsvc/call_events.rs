//! A call's events in its chat (`Event/Call`, `docs/protocol/chat.md`, "System messages"): its start and its end,
//! posted by the call controller as the caller's, to the people in the chat. A 1:1 call's chat is the two parties'
//! unique one, when they have it.

use std::fmt::Write as _;

use serde_json::{Map, json};

use super::service::Post;
use super::{Caller, events, message_repo, repo, wire};
use crate::app::App;
use crate::db::DbError;
use crate::db::users::User;

/// One participant of an ended call, as its end lists it.
pub(crate) struct Part<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// Seconds in the call.
    pub seconds: i64,
}

/// The unique chat of `a` and `b`, when they have one.
pub(crate) async fn chat_of(app: &App, a: &str, b: &str) -> Result<Option<String>, DbError> {
    let mut ids = [a, b];
    ids.sort_unstable();
    let id = format!("19:{}@unq.gbl.spaces", ids.join("_"));
    Ok(repo::chat(&app.db, &id).await?.map(|chat| chat.id))
}

pub(super) fn escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The ids of the people in the chat `chat` now; `None` when there is no such chat.
pub(crate) async fn members(app: &App, chat: &str) -> Result<Option<Vec<String>>, DbError> {
    if repo::chat(&app.db, chat).await?.is_none() {
        return Ok(None);
    }
    let members = repo::members(&app.db, chat).await?;
    Ok(Some(members.into_iter().map(|member| member.user_id).collect()))
}

/// The call `call` began, placed by `caller` (its id and name); a group call's start names its conversation, where
/// a member who wasn't rung joins it.
pub(crate) async fn started(
    app: &App,
    chat: &str,
    (id, name): (&str, &str),
    (call, conversation): (&str, Option<&str>),
) -> Result<(), DbError> {
    let url = conversation
        .map(|url| format!("<conversationUrl>{}</conversationUrl>", escaped(url)))
        .unwrap_or_default();
    let content = format!(
        "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType>{url}<callId>{call}</callId>"
    );
    post(app, (chat, None), (id, name), &content).await
}

/// A channel's meeting `call` began, organized by `organizer`, at `conversation`: its post in the channel, which
/// names the meeting (`properties.meeting`: its organizer and tenant, its title once its organizer titles it) and
/// where it runs, as the service posts a channel's Meet now. The post's id, when it was posted.
pub(crate) async fn meeting_started(
    app: &App,
    channel: &str,
    organizer: &User,
    (call, conversation): (&str, &str),
) -> Result<Option<i64>, DbError> {
    let content = format!(
        "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType>\
         <conversationUrl>{}</conversationUrl><callId>{call}</callId>",
        escaped(conversation)
    );
    let meeting = json!({
        "@type": "http://schema.skype.com/ScheduledMeetingCreated", "meetingtitle": null,
        "organizerId": organizer.id, "tenantId": app.issuer.tenant(),
    });
    let mut properties = Map::new();
    properties.insert("meeting".into(), meeting.to_string().into());
    let post = Post {
        thread: channel,
        author: organizer,
        kind: "Event/Call",
        content: &content,
        properties,
        under: None,
    };
    super::service::post(app, post).await
}

/// The call `call` ended with `parts` in it: one part (its caller alone) reads as a missed call. In `chat`, or in the
/// chain of its post `under` (a channel meeting's).
pub(crate) async fn ended(
    app: &App,
    (chat, under): (&str, Option<i64>),
    caller: (&str, &str),
    call: &str,
    parts: &[Part<'_>],
) -> Result<(), DbError> {
    let mut listed = String::new();
    for part in parts {
        let _ = write!(
            listed,
            "<part identity=\"{id}\"><name>8:orgid:{id}</name><displayName>{}</displayName><duration>{}</duration></part>",
            escaped(part.name),
            part.seconds,
            id = part.id,
        );
    }
    let content = format!(
        "<ended/><partlist alt=\"\" count=\"{}\">{listed}</partlist><callEventType>callEnded</callEventType><callId>{call}</callId>",
        parts.len()
    );
    post(app, (chat, under), caller, &content).await
}

async fn post(
    app: &App,
    (chat, under): (&str, Option<i64>),
    (id, name): (&str, &str),
    content: &str,
) -> Result<(), DbError> {
    let caller = Caller {
        id: id.to_owned(),
        tenant: String::new(),
        name: name.to_owned(),
    };
    let body = json!({
        "type": "Message", "conversationid": chat,
        "conversationLink": format!("{}/v1/users/ME/conversations/{chat}", wire::base(app)),
        "from": format!("{}/v1/users/ME/contacts/{}", wire::base(app), caller.mri()),
        "fromDisplayNameInToken": name, "imdisplayname": name,
        "messagetype": "Event/Call", "contenttype": "text", "content": content, "properties": {},
    });
    let draft = message_repo::Message {
        chat_id: chat.to_owned(),
        id: 0,
        owner_id: id.to_owned(),
        client_id: None,
        version: 0,
        body: body.to_string(),
        root_id: super::channels::root(&app.db, chat, under).await?,
    };
    let Some(posted) = message_repo::insert(&app.db, &draft, &wire::base(app)).await? else {
        return Ok(());
    };
    events::tell(app, "NewMessage", &posted.row.value()?, &posted.readers, &posted.told).await;
    Ok(())
}
