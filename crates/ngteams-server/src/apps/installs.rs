//! Where an app is added (`docs/protocol/apps.md`): the middle tier's entitlement writes for the account itself, a
//! group chat and a team, as the client sends them (the app's definition as the body, `201`; a `DELETE` by its id,
//! `200`), and the events its bot gets of each. An add or a removal is a row of `app_installs`, written with the
//! bot's `installationUpdate` and `conversationUpdate` in one transaction, so the bot never misses one. Added to a
//! group chat, the bot is one of the chat's people; added to a team, it is in the team's standard channels (and in
//! each one made later). There it gets only what mentions it ([`crate::bots`]); taken off, it gets nothing more.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, post};
use axum::{Json, Router};
use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Value, json};

use super::catalogue::{Answer, Refusal, usable_app};
use super::repo::Record;
use super::schema::{app_installs, apps};
use crate::app::App;
use crate::auth::extract::Spaces;
use crate::bots::activity::{InChannel, Place, Recipient, Sender, installation, members_changed};
use crate::bots::outbox::Delivery;
use crate::bots::schema::bot_deliveries;
use crate::chatsvc::BotSeat;
use crate::db::users::User;
use crate::db::{Db, DbError, now, with_conn};
use crate::teams::Actor;

type Me = Actor<Spaces>;

/// Where an app is added.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// For an account itself.
    Personal,
    /// To a group chat.
    Chat,
    /// To a team.
    Team,
}

impl Scope {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Chat => "chat",
            Self::Team => "team",
        }
    }

    /// The bot scope a manifest names for it.
    const fn bot_scope(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Chat => "groupChat",
            Self::Team => "team",
        }
    }
}

/// An app added somewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Install {
    pub app: String,
    /// The app's version now.
    pub version: String,
    pub by: Option<String>,
    /// Unix seconds.
    pub at: i64,
}

/// The apps added at `target` in `scope`, oldest first.
pub(crate) async fn of(db: &Db, scope: Scope, target: &str) -> Result<Vec<Install>, DbError> {
    let rows: Vec<(String, String, Option<String>, i64)> = with_conn!(db, |conn| app_installs::table
        .inner_join(apps::table.on(apps::id.eq(app_installs::app_id)))
        .filter(app_installs::scope.eq(scope.as_str()))
        .filter(app_installs::target.eq(target))
        .order((app_installs::installed_at.asc(), app_installs::app_id.asc()))
        .select((
            app_installs::app_id,
            apps::version,
            app_installs::installed_by,
            app_installs::installed_at
        ))
        .load(conn)
        .await?);
    Ok(rows
        .into_iter()
        .map(|(app, version, by, at)| Install { app, version, by, at })
        .collect())
}

pub(super) fn routes(mt: &str) -> Router<Arc<App>> {
    Router::new()
        .route(&format!("{mt}/beta/users/apps/entitlements"), post(add_personal))
        .route(
            &format!("{mt}/beta/users/apps/entitlements/{{app}}"),
            delete(remove_personal),
        )
        .route(
            &format!("{mt}/beta/chats/{{chat}}/apps/entitlements"),
            post(add_to_chat),
        )
        .route(
            &format!("{mt}/beta/chats/{{chat}}/apps/entitlements/{{app}}"),
            delete(remove_from_chat),
        )
        .route(
            &format!("{mt}/beta/teams/{{team}}/apps/entitlements"),
            post(add_to_team),
        )
        .route(
            &format!("{mt}/beta/teams/{{team}}/apps/entitlements/{{app}}"),
            delete(remove_from_team),
        )
}

/// The app a write's body (its definition) names, when the account may use it.
async fn named(app: &App, me: &Me, body: &[u8]) -> Answer<Record> {
    let definition: Value = serde_json::from_slice(body).map_err(|_| Refusal::BAD)?;
    let id = definition["id"].as_str().ok_or(Refusal::BAD)?;
    usable_app(&app.db, &me.id, id).await?.ok_or(Refusal::MISSING)
}

/// The account behind `me`.
async fn account(app: &App, me: &Me) -> Answer<User> {
    crate::db::users::by_id(&app.db, &me.id)
        .await?
        .ok_or(Refusal::FORBIDDEN)
}

/// Whether the app may be added in `scope`: an app with a bot only where its bot serves.
fn fits(record: &Record, scope: Scope) -> bool {
    record.bot.is_none()
        || record
            .scopes()
            .iter()
            .any(|listed| listed.eq_ignore_ascii_case(scope.bot_scope()))
}

/// The add's answer: the app's entitlement as the listings show it, with its bot.
async fn added(app: &App, record: &Record, scope: Scope, target: &str) -> Answer {
    let installs = of(&app.db, scope, target).await?;
    let install = installs.iter().find(|install| install.app == record.app.id);
    let mut value = super::catalogue::entitlement(&record.app.id, "Installed", install);
    value["botMetadata"] = record.bot.as_ref().map(|bot| json!({"botId": bot.id})).into();
    Ok((StatusCode::CREATED, Json(json!({ "value": value }))).into_response())
}

/// An add or removal to record: where, and what the app's bot gets of it.
struct Change<'a> {
    record: &'a Record,
    scope: Scope,
    target: &'a str,
    by: &'a User,
    /// Where the bot is told.
    place: Place<'a>,
    added: bool,
}

impl Change<'_> {
    /// The conversation the bot's events are queued in, for the outbox's order: a team's general channel is the
    /// team's own thread.
    const fn target_chat(&self) -> &str {
        match self.place {
            Place::Personal(chat) | Place::Group(chat) => chat,
            Place::Channel(_) => self.target,
        }
    }
}

/// Records `change` with its bot's events in one transaction; a repeat changes nothing and queues nothing.
async fn write_change(app: &App, change: &Change<'_>) -> Result<(), DbError> {
    let at = now();
    let ms = crate::chatsvc::now();
    let activities: Vec<(String, Value)> = match &change.record.bot {
        None => Vec::new(),
        Some(bot) => {
            let name = bot_name(&app.db, &bot.id, &change.record.app.name).await?;
            let from = Sender {
                id: &change.by.id,
                name: &change.by.display_name,
            };
            let to = Recipient {
                id: &bot.id,
                name: &name,
            };
            let install = installation(&change.place, &from, &to, change.added, ms);
            let members = members_changed(&change.place, &from, &to, &[format!("28:{}", bot.id)], change.added, ms);
            let ordered = if change.added {
                [install, members]
            } else {
                [members, install]
            };
            ordered.into_iter().map(|activity| (bot.id.clone(), activity)).collect()
        }
    };
    let changed = with_conn!(&app.db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let key = (change.record.app.id.as_str(), change.scope.as_str(), change.target);
            let changed = if change.added {
                diesel::insert_into(app_installs::table)
                    .values((
                        app_installs::app_id.eq(key.0),
                        app_installs::scope.eq(key.1),
                        app_installs::target.eq(key.2),
                        app_installs::installed_by.eq(Some(&change.by.id)),
                        app_installs::installed_at.eq(at),
                    ))
                    .on_conflict_do_nothing()
                    .execute(conn)
                    .await?
            } else {
                diesel::delete(app_installs::table.find(key)).execute(conn).await?
            };
            if changed > 0 {
                for (seq, (bot, activity)) in (ms..).zip(&activities) {
                    let row = Delivery::new(bot, change.target_chat(), seq, activity, false);
                    diesel::insert_into(bot_deliveries::table)
                        .values(&row)
                        .execute(conn)
                        .await?;
                }
            }
            Ok(changed > 0)
        })
        .await)?;
    if changed && !activities.is_empty() {
        crate::bots::wake(app).await;
    }
    Ok(())
}

async fn bot_name(db: &Db, bot: &str, app_name: &str) -> Result<String, DbError> {
    Ok(crate::bots::bot(db, bot)
        .await?
        .map_or_else(|| app_name.to_owned(), |bot| bot.name))
}

/// `POST beta/users/apps/entitlements`: the app for the account itself. Its bot's 1:1 chat with the account is made,
/// and the bot told there.
async fn add_personal(State(app): State<Arc<App>>, me: Me, body: Bytes) -> Answer {
    let record = named(&app, &me, &body).await?;
    if !fits(&record, Scope::Personal) {
        return Err(Refusal::BAD);
    }
    let by = account(&app, &me).await?;
    let chat = match &record.bot {
        Some(bot) => crate::chatsvc::service::bot_chat(&app, &me.id, &bot.id)
            .await?
            .ok_or(Refusal::FORBIDDEN)?,
        None => String::new(),
    };
    let change = Change {
        record: &record,
        scope: Scope::Personal,
        target: &me.id,
        by: &by,
        place: Place::Personal(&chat),
        added: true,
    };
    if let Some(bot) = &record.bot {
        crate::chatsvc::service::seat_personal_bot(&app.db, &chat, &bot.id, true).await?;
    }
    write_change(&app, &change).await?;
    added(&app, &record, Scope::Personal, &me.id).await
}

/// `DELETE beta/users/apps/entitlements/<app>`: the account's own app taken off; its bot is told, then leaves their
/// 1:1 chat, so it gets nothing more there and posts nothing.
async fn remove_personal(State(app): State<Arc<App>>, me: Me, Path(id): Path<String>) -> Answer {
    let record = super::repo::get(&app.db, &id).await?.ok_or(Refusal::MISSING)?;
    let by = account(&app, &me).await?;
    let chat = record
        .bot
        .as_ref()
        .map(|bot| crate::bots::personal_chat(&me.id, &bot.id))
        .unwrap_or_default();
    let change = Change {
        record: &record,
        scope: Scope::Personal,
        target: &me.id,
        by: &by,
        place: Place::Personal(&chat),
        added: false,
    };
    write_change(&app, &change).await?;
    // Not gated on the row: a retry must still unseat the bot after a failed first unseat.
    if let Some(bot) = &record.bot {
        crate::chatsvc::service::seat_personal_bot(&app.db, &chat, &bot.id, false).await?;
    }
    Ok(StatusCode::OK.into_response())
}

/// The group chat `chat` when the account is one of its people.
async fn group_chat(app: &App, me: &Me, chat: &str) -> Answer<()> {
    let members = crate::chatsvc::service::members(&app.db, chat).await?;
    if !members.contains(&me.id) {
        return Err(Refusal::FORBIDDEN);
    }
    if crate::chatsvc::service::is_group(&app.db, chat).await? {
        Ok(())
    } else {
        Err(Refusal::BAD)
    }
}

fn seated(seat: BotSeat) -> Answer<()> {
    match seat {
        BotSeat::Done | BotSeat::Unchanged => Ok(()),
        BotSeat::Denied => Err(Refusal::FORBIDDEN),
        BotSeat::NotAGroup => Err(Refusal::BAD),
        BotSeat::Missing => Err(Refusal::MISSING),
    }
}

/// `POST beta/chats/<chat>/apps/entitlements`: the app added to a group chat the account is in; its bot joins the
/// chat as Teams adds it, and is told.
async fn add_to_chat(State(app): State<Arc<App>>, me: Me, Path(chat): Path<String>, body: Bytes) -> Answer {
    let record = named(&app, &me, &body).await?;
    group_chat(&app, &me, &chat).await?;
    if !fits(&record, Scope::Chat) {
        return Err(Refusal::BAD);
    }
    let by = account(&app, &me).await?;
    if let Some(bot) = &record.bot {
        seated(crate::chatsvc::seat_bot(&app, &chat, &by, &bot.id, true).await?)?;
    }
    let change = Change {
        record: &record,
        scope: Scope::Chat,
        target: &chat,
        by: &by,
        place: Place::Group(&chat),
        added: true,
    };
    write_change(&app, &change).await?;
    added(&app, &record, Scope::Chat, &chat).await
}

/// `DELETE beta/chats/<chat>/apps/entitlements/<app>`: the app taken off a group chat the account is in; its bot is
/// told, then leaves the chat.
async fn remove_from_chat(State(app): State<Arc<App>>, me: Me, Path((chat, id)): Path<(String, String)>) -> Answer {
    group_chat(&app, &me, &chat).await?;
    let record = super::repo::get(&app.db, &id).await?.ok_or(Refusal::MISSING)?;
    let by = account(&app, &me).await?;
    let change = Change {
        record: &record,
        scope: Scope::Chat,
        target: &chat,
        by: &by,
        place: Place::Group(&chat),
        added: false,
    };
    write_change(&app, &change).await?;
    if let Some(bot) = &record.bot {
        seated(crate::chatsvc::seat_bot(&app, &chat, &by, &bot.id, false).await?)?;
    }
    Ok(StatusCode::OK.into_response())
}

/// The team `team` and its general channel, when the account may add apps to it (`adding`) or take them off: its
/// owners, its members while the team's `installApp` (or `uninstallApp`) setting lets them, and the server's
/// administrators.
async fn team_place(app: &App, me: &Me, team: &str, adding: bool) -> Answer<InChannel> {
    let found = crate::teams::repo::team(&app.db, team).await?.ok_or(Refusal::MISSING)?;
    let account = crate::db::users::by_id(&app.db, &me.id).await?;
    let admin = account.is_some_and(|account| account.is_admin);
    let may = match crate::teams::repo::membership(&app.db, team, &me.id).await? {
        None if !admin => return Err(Refusal::MISSING),
        None => true,
        Some(member) => {
            let setting = if adding { "installApp" } else { "uninstallApp" };
            admin || member.owner || crate::teams::wire::members_may(&found, setting, true)
        }
    };
    if !may {
        return Err(Refusal::FORBIDDEN);
    }
    if found.archived {
        return Err(Refusal::Code(
            StatusCode::FORBIDDEN,
            "OperationNotAllowedOnArchivedTeam",
        ));
    }
    Ok(InChannel {
        channel: found.id.clone(),
        team: found.id,
        team_name: found.name,
        group: found.group_id,
        root: None,
    })
}

/// `POST beta/teams/<team>/apps/entitlements`: the app added to a team; its bot joins the team's standard channels
/// and is told in the general one.
async fn add_to_team(State(app): State<Arc<App>>, me: Me, Path(team): Path<String>, body: Bytes) -> Answer {
    let record = named(&app, &me, &body).await?;
    let place = team_place(&app, &me, &team, true).await?;
    if !fits(&record, Scope::Team) {
        return Err(Refusal::BAD);
    }
    let by = account(&app, &me).await?;
    if let Some(bot) = &record.bot {
        crate::teams::repo::seat_bot(&app.db, &team, &bot.id, true).await?;
    }
    let change = Change {
        record: &record,
        scope: Scope::Team,
        target: &team,
        by: &by,
        place: Place::Channel(&place),
        added: true,
    };
    write_change(&app, &change).await?;
    added(&app, &record, Scope::Team, &team).await
}

/// `DELETE beta/teams/<team>/apps/entitlements/<app>`: the app taken off a team; its bot is told, then leaves the
/// team's channels.
async fn remove_from_team(State(app): State<Arc<App>>, me: Me, Path((team, id)): Path<(String, String)>) -> Answer {
    let place = team_place(&app, &me, &team, false).await?;
    let record = super::repo::get(&app.db, &id).await?.ok_or(Refusal::MISSING)?;
    let by = account(&app, &me).await?;
    let change = Change {
        record: &record,
        scope: Scope::Team,
        target: &team,
        by: &by,
        place: Place::Channel(&place),
        added: false,
    };
    write_change(&app, &change).await?;
    if let Some(bot) = &record.bot {
        crate::teams::repo::seat_bot(&app.db, &team, &bot.id, false).await?;
    }
    Ok(StatusCode::OK.into_response())
}
