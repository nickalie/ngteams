//! `PUT v1/threads/<chat>/properties?name=topic`: one of a group chat's people names it, renames it or takes the name
//! off (`""`), as Teams' "Name group chat" does. One transaction under the clock checks the caller is in the group
//! chat then, sets the name and writes the thread's `ThreadActivity/TopicUpdate` event, so what is told of the change
//! goes to exactly the people in the chat at that moment. A 1:1 chat, a meeting's chat, a channel and a stream take no
//! name here (400).

use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::Value;

use super::member_repo::{Done, Outcome, group};
use super::message_repo::Message;
use super::repo::{Member, advance};
use super::schema::{chat_members, chat_messages, chats};
use super::{Caller, Error, activity, call_events, members, wire};
use crate::app::App;
use crate::calendar::schema::meetings;
use crate::db::{Db, DbError, with_conn};

/// The longest name Teams' "Group name" box takes, in characters.
const MOST: usize = 250;

/// Names the stored thread `chat` (the caller is in it) by the `topic` of `body`.
pub(super) async fn set(app: &App, caller: &Caller, chat: &str, body: &Value) -> Result<StatusCode, Error> {
    let topic = body["topic"].as_str().ok_or(Error::BadRequest)?.trim();
    if topic.chars().count() > MOST {
        return Err(Error::BadRequest);
    }
    let base = wire::base(app);
    let initiator = caller.mri();
    let event = |version: i64| {
        let content = content(&initiator, version, topic);
        members::event_body(&base, chat, version, "ThreadActivity/TopicUpdate", &content)
    };
    let outcome = rename(&app.db, chat, &caller.id, topic, &event).await?;
    members::finish(app, caller, outcome).await
}

/// The event's content, as Teams writes it: an empty `<value />` when the name is taken off.
fn content(initiator: &str, version: i64, topic: &str) -> String {
    let value = if topic.is_empty() {
        "<value />".to_owned()
    } else {
        format!("<value>{}</value>", call_events::escaped(topic))
    };
    format!("<topicupdate><eventtime>{version}</eventtime><initiator>{initiator}</initiator>{value}</topicupdate>")
}

/// Names the group chat `chat` `topic` for `actor` (empty: no name) and writes the event `event` makes of the
/// change's version. The same name again is no change.
async fn rename(
    db: &Db,
    chat: &str,
    actor: &str,
    topic: &str,
    event: &(dyn Fn(i64) -> String + Sync),
) -> Result<Outcome, DbError> {
    let at = wire::now();
    let name = Some(topic).filter(|topic| !topic.is_empty());
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let found = group!(conn, chat, actor);
            let meeting: bool = diesel::select(diesel::dsl::exists(meetings::table.find(chat)))
                .get_result(conn)
                .await?;
            if meeting {
                return Ok(Outcome::NotAGroup);
            }
            if found.topic.as_deref() == name {
                return Ok(Outcome::Unchanged);
            }
            let event = Message {
                chat_id: chat.to_owned(),
                id: version,
                owner_id: actor.to_owned(),
                client_id: None,
                version,
                body: event(version),
                root_id: None,
            };
            diesel::insert_into(chat_messages::table)
                .values(&event)
                .execute(conn)
                .await?;
            diesel::update(chats::table.find(chat))
                .set((chats::topic.eq(name), chats::version.eq(version)))
                .execute(conn)
                .await?;
            let scene = activity::scene!(conn, chat);
            let readers = scene.readers(version);
            let members: Vec<Member> = chat_members::table
                .filter(chat_members::chat_id.eq(chat))
                .filter(chat_members::left_at.is_null())
                .order(chat_members::position)
                .select(Member::as_select())
                .load(conn)
                .await?;
            Ok(Outcome::Done(Box::new(Done {
                event,
                readers,
                chat: scene.chat,
                members,
            })))
        })
        .await)
}
