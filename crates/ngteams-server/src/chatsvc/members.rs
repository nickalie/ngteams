//! `PUT v1/threads/<chat>/members` and `DELETE v1/threads/<chat>/members/<mri>`: a group chat's people add others
//! with as much of its history as they give, take others out, and leave. A 1:1 chat keeps its two people, and a
//! team's channel takes and loses people through its team.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State, rejection::JsonRejection};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;

use super::member_repo::{self, Done, Invite, Joining, Outcome};
use super::{Caller, Error, events, member_of, reads, wire};
use crate::app::App;
use crate::db::DbError;
use crate::db::users::User;

type Params = HashMap<String, String>;

#[derive(Deserialize)]
pub(super) struct Add {
    members: Vec<Added>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Added {
    id: String,
    #[serde(default)]
    role: Option<String>,
    share_history_time: i64,
}

/// An account's id from its MRI.
fn account(mri: &str) -> Result<&str, Error> {
    mri.strip_prefix("8:orgid:")
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .ok_or(Error::BadRequest)
}

impl Add {
    fn invites(&self) -> Result<Vec<Invite>, Error> {
        let mut invites: Vec<Invite> = Vec::new();
        for member in &self.members {
            let user = account(&member.id)?;
            let admin = member.role.as_deref().is_none_or(|role| role == "Admin");
            if !admin || invites.iter().any(|invite| invite.user == user) {
                return Err(Error::BadRequest);
            }
            invites.push(Invite {
                user: user.to_owned(),
                share: member.share_history_time,
            });
        }
        if invites.is_empty() {
            return Err(Error::BadRequest);
        }
        Ok(invites)
    }
}

pub(super) async fn add(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Result<Json<Add>, JsonRejection>,
) -> Result<StatusCode, Error> {
    let chat = reads::target(&path)?;
    let (found, _) = member_of(&app, &caller, chat).await?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let invites = body.invites()?;
    let base = wire::base(&app);
    let event = |version: i64, added: &[(String, i64)]| {
        let content = event_content("addmember", &caller.mri(), version, added, true, person_mri);
        event_body(&base, chat, version, "ThreadActivity/AddMember", &content)
    };
    let outcome = member_repo::add(&app.db, &found.id, &caller.id, (&invites, Joining::People), &event).await?;
    finish(&app, &caller, outcome).await
}

pub(super) async fn remove(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
) -> Result<StatusCode, Error> {
    let chat = reads::target(&path)?;
    let target = account(path.get("member").map_or("", String::as_str))?;
    let (found, _) = member_of(&app, &caller, chat).await?;
    let base = wire::base(&app);
    let event = |version: i64, gone: &[(String, i64)]| {
        let content = event_content("deletemember", &caller.mri(), version, gone, false, person_mri);
        event_body(&base, chat, version, "ThreadActivity/DeleteMember", &content)
    };
    let outcome = member_repo::remove(&app.db, &found.id, &caller.id, (target, Joining::People), &event).await?;
    finish(&app, &caller, outcome).await
}

/// Answers a roster change or a rename and tells its people: the event to everyone who may read it, the thread as it is now to
/// the people in it. Someone taken out hears the event of their going and nothing of the thread after it.
pub(super) async fn finish(app: &App, caller: &Caller, outcome: Outcome) -> Result<StatusCode, Error> {
    let done: Box<Done> = match outcome {
        Outcome::Denied => return Err(Error::Denied),
        Outcome::NotAGroup | Outcome::Unknown => return Err(Error::BadRequest),
        Outcome::Missing => return Err(Error::Missing),
        Outcome::Unchanged => return Ok(StatusCode::OK),
        Outcome::Done(done) => done,
    };
    events::tell(app, "NewMessage", &done.event.value()?, &done.readers, &[]).await;
    let thread = wire::thread(app, caller, &done.chat, &done.members);
    events::publish(app, &done.members, "ThreadUpdate", &thread).await;
    if done.bots {
        crate::bots::wake(app).await;
    }
    Ok(StatusCode::OK)
}

/// A roster event's content, as Teams writes it: who did it, when, and each person it names. An added person's
/// `sharehistorytime` is the first message they may read (`0`: all; the event's own time: none before it).
fn event_content(
    tag: &str,
    initiator: &str,
    version: i64,
    people: &[(String, i64)],
    shares: bool,
    mri: fn(&str) -> String,
) -> String {
    let mut content = format!(
        "<{tag}><eventtime>{version}</eventtime><initiator>{initiator}</initiator>\
         <rosterVersion>{version}</rosterVersion>"
    );
    for (user, from) in people {
        let target = mri(user);
        let _ = write!(
            content,
            "<target>{target}</target><detailedtargetinfo><id>{target}</id>"
        );
        if shares {
            let _ = write!(content, "<sharehistorytime>{from}</sharehistorytime>");
        }
        content.push_str("</detailedtargetinfo>");
    }
    let _ = write!(content, "</{tag}>");
    content
}

fn person_mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

fn bot_mri(id: &str) -> String {
    format!("28:{id}")
}

/// What a bot's add to or removal from a group chat came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BotSeat {
    Done,
    /// It was in the chat already (added), or not in it (removed).
    Unchanged,
    /// The one asking isn't in the chat.
    Denied,
    /// A 1:1 chat, a channel or a stream.
    NotAGroup,
    /// No such chat.
    Missing,
}

/// The bot `bot` added to (`add`) or taken out of the group chat `chat` by `actor`, one of its people, as a person is
/// added or taken out: the thread's event and its new roster told to its people.
pub(crate) async fn seat_bot(app: &App, chat: &str, actor: &User, bot: &str, add: bool) -> Result<BotSeat, DbError> {
    let caller = super::service::caller(app, actor);
    let base = wire::base(app);
    let outcome = if add {
        let invites = [Invite {
            user: bot.to_owned(),
            share: -1,
        }];
        let event = |version: i64, added: &[(String, i64)]| {
            let content = event_content("addmember", &caller.mri(), version, added, true, bot_mri);
            event_body(&base, chat, version, "ThreadActivity/AddMember", &content)
        };
        member_repo::add(&app.db, chat, &caller.id, (&invites, Joining::Bot), &event).await?
    } else {
        let event = |version: i64, gone: &[(String, i64)]| {
            let content = event_content("deletemember", &caller.mri(), version, gone, false, bot_mri);
            event_body(&base, chat, version, "ThreadActivity/DeleteMember", &content)
        };
        match member_repo::remove(&app.db, chat, &caller.id, (bot, Joining::Bot), &event).await? {
            Outcome::Missing => Outcome::Unchanged,
            outcome => outcome,
        }
    };
    let done = match outcome {
        Outcome::Denied => return Ok(BotSeat::Denied),
        Outcome::NotAGroup | Outcome::Unknown => return Ok(BotSeat::NotAGroup),
        Outcome::Missing => return Ok(BotSeat::Missing),
        Outcome::Unchanged => return Ok(BotSeat::Unchanged),
        Outcome::Done(done) => done,
    };
    events::tell(app, "NewMessage", &done.event.value()?, &done.readers, &[]).await;
    let thread = wire::thread(app, &caller, &done.chat, &done.members);
    events::publish(app, &done.members, "ThreadUpdate", &thread).await;
    Ok(BotSeat::Done)
}

/// A thread's own event as a message: sent by the thread itself.
pub(super) fn event_body(base: &str, chat: &str, version: i64, kind: &str, content: &str) -> String {
    json!({
        "type": "Message", "id": version.to_string(), "version": version.to_string(), "sequenceId": version,
        "conversationid": chat, "conversationLink": format!("{base}/v1/users/ME/conversations/{chat}"),
        "messagetype": kind, "content": content, "from": format!("{base}/v1/users/ME/contacts/{chat}"),
        "composetime": wire::iso(version), "originalarrivaltime": wire::iso(version),
    })
    .to_string()
}
