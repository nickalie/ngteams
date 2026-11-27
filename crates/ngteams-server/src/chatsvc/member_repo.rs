//! A group chat's roster changes. Each is one transaction under the clock: it checks the actor is in the chat then,
//! writes the thread's own event at the change's version, and reads who is in the chat afterwards, so what is told
//! of the change goes to exactly the people the change left in it.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::activity;
use super::message_repo::Message;
use super::repo::{Chat, Member, advance};
use super::schema::{chat_members, chat_messages, chats};
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};

pub(super) enum Outcome {
    /// The actor isn't in the chat (any more).
    Denied,
    /// A 1:1 chat, a channel or a stream: its people aren't changed here.
    NotAGroup,
    /// Nobody to change: the one to remove isn't in the chat.
    Missing,
    /// Someone to add is no account that can sign in.
    Unknown,
    /// Everyone to add is in the chat already.
    Unchanged,
    Done(Box<Done>),
}

/// A roster change as made.
pub(super) struct Done {
    /// The thread's event of the change.
    pub event: Message,
    /// Who may read the event.
    pub readers: Vec<String>,
    pub chat: Chat,
    /// The people in the chat after the change.
    pub members: Vec<Member>,
    /// Whether a bot in the chat was told of the change.
    pub bots: bool,
}

/// One person to add, and the first message they may read (the `shareHistoryTime` sent for them).
pub(super) struct Invite {
    pub user: String,
    pub share: i64,
}

/// What an event's body is made of, by its version and the people it names with the start of their history.
pub(super) type Event<'a> = &'a (dyn Fn(i64, &[(String, i64)]) -> String + Sync);

/// The group chat `$chat` for `$actor`, one of its people now; else the [`Outcome`] that refuses the change.
macro_rules! group {
    ($conn:expr, $chat:expr, $actor:expr) => {{
        use $crate::chatsvc::member_repo::Outcome;
        use $crate::chatsvc::repo::Chat;
        use $crate::chatsvc::schema::{chat_members, chats};
        let chat: Option<Chat> = chats::table
            .find($chat)
            .select(Chat::as_select())
            .first($conn)
            .await
            .optional()?;
        let Some(chat) = chat else {
            return Ok(Outcome::Missing);
        };
        let member: bool = diesel::select(diesel::dsl::exists(
            chat_members::table
                .filter(chat_members::chat_id.eq($chat))
                .filter(chat_members::user_id.eq($actor))
                .filter(chat_members::left_at.is_null()),
        ))
        .get_result($conn)
        .await?;
        if !member {
            return Ok(Outcome::Denied);
        }
        let channel: bool = diesel::select(diesel::dsl::exists($crate::teams::schema::channels::table.find($chat)))
            .get_result($conn)
            .await?;
        if chat.unique_roster || channel || $crate::chatsvc::streams::name_of($chat).is_some() {
            return Ok(Outcome::NotAGroup);
        }
        chat
    }};
}
pub(super) use group;

/// Writes the event and moves the chat and its roster to `$version`; the [`Done`].
macro_rules! finish {
    ($conn:expr, $chat:expr, $actor:expr, $version:expr, $body:expr, $also:expr) => {{
        let event = Message {
            chat_id: $chat.id.clone(),
            id: $version,
            owner_id: $actor.to_owned(),
            client_id: None,
            version: $version,
            body: $body,
            root_id: None,
        };
        diesel::insert_into(chat_messages::table)
            .values(&event)
            .execute($conn)
            .await?;
        diesel::update(chats::table.find(&$chat.id))
            .set((chats::version.eq($version), chats::roster_version.eq($version)))
            .execute($conn)
            .await?;
        let scene = activity::scene!($conn, &$chat.id);
        let mut readers = scene.readers($version);
        readers.extend($also);
        let members: Vec<Member> = chat_members::table
            .filter(chat_members::chat_id.eq(&$chat.id))
            .filter(chat_members::left_at.is_null())
            .order(chat_members::position)
            .select(Member::as_select())
            .load($conn)
            .await?;
        Done {
            event,
            readers,
            chat: scene.chat,
            members,
            bots: false,
        }
    }};
}

/// Whose seat a roster add or removal changes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Joining {
    /// People (an add takes only accounts in use).
    People,
    /// The bot of an app added to the chat: an app's account, which never signs in.
    Bot,
}

/// Adds `invites` (all `joining`) to the group chat for `actor`; `event` makes the body of the thread's event.
pub(super) async fn add(
    db: &Db,
    chat: &str,
    actor: &str,
    (invites, joining): (&[Invite], Joining),
    event: Event<'_>,
) -> Result<Outcome, DbError> {
    let at = super::wire::now();
    let ids: Vec<&str> = invites.iter().map(|invite| invite.user.as_str()).collect();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let found = group!(conn, chat, actor);
            let known: i64 = match joining {
                Joining::People => {
                    users::table
                        .filter(users::id.eq_any(&ids))
                        .filter(users::disabled.eq(false))
                        .filter(crate::db::users::person())
                        .count()
                        .get_result(conn)
                        .await?
                }
                Joining::Bot => {
                    users::table
                        .filter(users::id.eq_any(&ids))
                        .filter(users::kind.eq(crate::db::users::APP))
                        .filter(users::deleted_at.is_null())
                        .count()
                        .get_result(conn)
                        .await?
                }
            };
            if usize::try_from(known).ok() != Some(ids.len()) {
                return Ok(Outcome::Unknown);
            }
            let seats: Vec<(String, Option<i64>, i32)> = chat_members::table
                .filter(chat_members::chat_id.eq(chat))
                .select((chat_members::user_id, chat_members::left_at, chat_members::position))
                .load(conn)
                .await?;
            let mut position = seats.iter().map(|seat| seat.2).max().unwrap_or(0);
            let mut added: Vec<(String, i64)> = Vec::new();
            for invite in invites {
                let from = match invite.share {
                    0 => 0,
                    share if share < 0 => version,
                    share => share.min(version),
                };
                match seats.iter().find(|seat| seat.0 == invite.user) {
                    Some((_, None, _)) => continue,
                    Some(_) => {
                        diesel::update(chat_members::table.find((chat, &invite.user)))
                            .set((
                                chat_members::left_at.eq(None::<i64>),
                                chat_members::history_from.eq(from),
                                chat_members::joined_at.eq(version),
                            ))
                            .execute(conn)
                            .await?;
                    }
                    None => {
                        position = position.saturating_add(1);
                        diesel::insert_into(chat_members::table)
                            .values((
                                chat_members::chat_id.eq(chat),
                                chat_members::user_id.eq(&invite.user),
                                chat_members::position.eq(position),
                                chat_members::joined_at.eq(version),
                                chat_members::history_from.eq(from),
                            ))
                            .execute(conn)
                            .await?;
                    }
                }
                added.push((invite.user.clone(), from));
            }
            if added.is_empty() {
                return Ok(Outcome::Unchanged);
            }
            let mut done = finish!(conn, found, actor, version, event(version, &added), None::<String>);
            if joining == Joining::People {
                let people: Vec<&str> = added.iter().map(|(user, _)| user.as_str()).collect();
                done.bots = crate::bots::outbox::enqueue_members!(conn, chat, actor, &people, true, version);
            }
            Ok(Outcome::Done(Box::new(done)))
        })
        .await)
}

/// Takes `target` (a person, or for [`Joining::Bot`] an app's bot) out of the group chat for `actor` (themself: they
/// leave). They keep what they could read until now, the event of their going included, and are told of it once. A
/// bot leaves only with its app's removal, so a person's roster change never names one.
pub(super) async fn remove(
    db: &Db,
    chat: &str,
    actor: &str,
    (target, leaving): (&str, Joining),
    event: Event<'_>,
) -> Result<Outcome, DbError> {
    let at = super::wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let found = group!(conn, chat, actor);
            let kind_fits: bool = match leaving {
                Joining::People => {
                    diesel::select(diesel::dsl::exists(
                        users::table.find(target).filter(crate::db::users::person()),
                    ))
                    .get_result(conn)
                    .await?
                }
                Joining::Bot => true,
            };
            if !kind_fits {
                return Ok(Outcome::Missing);
            }
            let gone = diesel::update(
                chat_members::table
                    .find((chat, target))
                    .filter(chat_members::left_at.is_null()),
            )
            .set(chat_members::left_at.eq(Some(version)))
            .execute(conn)
            .await?;
            if gone == 0 {
                return Ok(Outcome::Missing);
            }
            let body = event(version, &[(target.to_owned(), 0)]);
            let mut done = finish!(conn, found, actor, version, body, Some(target.to_owned()));
            if leaving == Joining::People {
                done.bots = crate::bots::outbox::enqueue_members!(conn, chat, actor, &[target], false, version);
            }
            Ok(Outcome::Done(Box::new(done)))
        })
        .await)
}
