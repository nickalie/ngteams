//! The registry's database work. Each change is one transaction with its audit entry (`admin_audit`, as the
//! account changes write it), so a refused or failed change leaves neither. A bot's row in `users` is made with
//! its app, and stays when the app is deleted (deleted, keeping its name), so what it posted keeps an author; the same
//! bot id may be registered again later.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Map, Value, json};

use super::schema::{app_users, apps, bots};
use super::{AppError, Changes};
use crate::admin::Actor;
use crate::db::schema::{admin_audit, users};
use crate::db::users::{APP, PERSON};
use crate::db::{Db, DbError, conflict, now, with_conn};

/// An app as the database keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = apps)]
pub struct AppRow {
    pub id: String,
    pub version: String,
    pub name: String,
    pub full_name: String,
    pub short_description: String,
    pub full_description: String,
    pub developer: String,
    pub accent_color: String,
    /// The Teams manifest, JSON.
    pub manifest: String,
    /// `form` or `package`.
    pub source: String,
    /// Whether its icons are in the file store.
    pub icons: bool,
    /// Whether every account may use it, or only its listed users.
    pub everyone: bool,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A bot as the database keeps it.
#[derive(Clone, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = bots)]
pub struct BotRow {
    pub id: String,
    pub app_id: String,
    pub endpoint: Option<String>,
    /// The secret's SHA-256, base64url.
    pub secret_hash: String,
    /// `signed` or `unsigned`.
    pub auth_mode: String,
    /// The scopes it serves, a JSON array.
    pub scopes: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl std::fmt::Debug for BotRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotRow")
            .field("id", &self.id)
            .field("app_id", &self.app_id)
            .field("auth_mode", &self.auth_mode)
            .finish_non_exhaustive()
    }
}

/// An app with its bot and its listed users.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub app: AppRow,
    pub bot: Option<BotRow>,
    pub users: Vec<String>,
}

impl Record {
    /// The scopes its bot serves.
    #[must_use]
    pub fn scopes(&self) -> Vec<String> {
        self.bot
            .as_ref()
            .and_then(|bot| serde_json::from_str(&bot.scopes).ok())
            .unwrap_or_default()
    }
}

fn by_name(rows: &mut [AppRow]) {
    rows.sort_by_cached_key(|app| (app.name.to_lowercase(), app.id.clone()));
}

/// What a bot's client credentials are checked against.
#[derive(Clone)]
pub struct Credentials {
    pub secret_hash: String,
    /// Whether its app is enabled.
    pub enabled: bool,
    /// Whether it authenticates (`signed`); an `unsigned` bot takes no token.
    pub signed: bool,
}

/// An `admin_audit` row: an administrator's change to an app.
#[derive(Insertable)]
#[diesel(table_name = admin_audit)]
pub(crate) struct Entry<'a> {
    at: i64,
    actor_id: Option<&'a str>,
    actor: &'a str,
    action: &'a str,
    target_id: Option<&'a str>,
    target: &'a str,
    detail: String,
}

impl<'a> Entry<'a> {
    pub(crate) fn new(actor: Actor<'a>, action: &'a str, app: &'a AppRow, detail: &Value) -> Self {
        Self {
            at: now(),
            actor_id: actor.id,
            actor: actor.name,
            action,
            target_id: Some(&app.id),
            target: &app.name,
            detail: detail.to_string(),
        }
    }
}

/// Inserts the audit [`Entry`] `$entry` in the transaction `$conn`.
macro_rules! record {
    ($conn:ident, $entry:expr) => {
        diesel::insert_into($crate::db::schema::admin_audit::table)
            .values($entry)
            .execute($conn)
            .await?
    };
}
pub(crate) use record;

/// The app row `$id`, if there is one.
macro_rules! app_row {
    ($conn:ident, $id:expr) => {{
        let app: Option<AppRow> = apps::table
            .find($id)
            .select(AppRow::as_select())
            .first($conn)
            .await
            .optional()?;
        app
    }};
}

/// The app row `$app` with its bot and users.
macro_rules! filled {
    ($conn:ident, $app:expr) => {{
        let app: AppRow = $app;
        let bot: Option<BotRow> = bots::table
            .filter(bots::app_id.eq(&app.id))
            .select(BotRow::as_select())
            .first($conn)
            .await
            .optional()?;
        let users: Vec<String> = app_users::table
            .filter(app_users::app_id.eq(&app.id))
            .order(app_users::user_id.asc())
            .select(app_users::user_id)
            .load($conn)
            .await?;
        Record { app, bot, users }
    }};
}

/// The app `$id` with its bot and users, or [`AppError::NoSuchApp`].
macro_rules! load {
    ($conn:ident, $id:expr) => {{
        let app = app_row!($conn, $id).ok_or(AppError::NoSuchApp)?;
        filled!($conn, app)
    }};
}

fn trace(actor: Actor<'_>, action: &str, target: &str) {
    tracing::info!(actor = actor.name, actor_id = actor.id, action, target, "admin action");
}

/// A taken app id as [`AppError::AppExists`].
fn taken(error: diesel::result::Error) -> AppError {
    match conflict(error) {
        DbError::Conflict => AppError::AppExists,
        error => AppError::Db(error),
    }
}

/// Adds an app and its bot, with the bot's account row: a new one, or the row of a deleted bot with that id again.
///
/// # Errors
/// [`AppError::AppExists`], [`AppError::BotExists`] for an id a person, a live bot or the server's own app has;
/// a database failure.
pub async fn insert(db: &Db, actor: Actor<'_>, app: AppRow, bot: Option<BotRow>) -> Result<Record, AppError> {
    let detail = json!({"source": app.source, "version": app.version, "bot": bot.as_ref().map(|bot| &bot.id)});
    let record = with_conn!(db, |conn| conn
        .transaction::<_, AppError, _>(async |conn| {
            diesel::insert_into(apps::table)
                .values(&app)
                .execute(conn)
                .await
                .map_err(taken)?;
            if let Some(bot) = &bot {
                let at = now();
                let existing: Option<(String, Option<i64>)> = users::table
                    .find(&bot.id)
                    .select((users::kind, users::deleted_at))
                    .first(conn)
                    .await
                    .optional()?;
                match existing {
                    None => {
                        diesel::insert_into(users::table)
                            .values((
                                users::id.eq(&bot.id),
                                users::upn.eq(format!("app:{}", bot.id)),
                                users::display_name.eq(&app.name),
                                users::password_hash.eq(""),
                                users::disabled.eq(true),
                                users::created_at.eq(at),
                                users::updated_at.eq(at),
                                users::kind.eq(APP),
                            ))
                            .execute(conn)
                            .await
                            .map_err(|error| match conflict(error) {
                                DbError::Conflict => AppError::BotExists,
                                error => AppError::Db(error),
                            })?;
                    }
                    Some((kind, Some(_))) if kind == APP && bot.id != crate::workflows::WORKFLOWS_BOT => {
                        diesel::update(users::table.find(&bot.id))
                            .set((
                                users::display_name.eq(&app.name),
                                users::deleted_at.eq(None::<i64>),
                                users::updated_at.eq(at),
                            ))
                            .execute(conn)
                            .await?;
                    }
                    Some(_) => return Err(AppError::BotExists),
                }
                diesel::insert_into(bots::table)
                    .values(bot)
                    .execute(conn)
                    .await
                    .map_err(|error| match conflict(error) {
                        DbError::Conflict => AppError::BotExists,
                        error => AppError::Db(error),
                    })?;
            }
            record!(conn, &Entry::new(actor, "app.add", &app, &detail));
            Ok(Record {
                app: app.clone(),
                bot: bot.clone(),
                users: Vec::new(),
            })
        })
        .await)?;
    trace(actor, "app.add", &record.app.name);
    Ok(record)
}

/// Replaces an app's package fields (all but its state, users and times of creation), and its bot's scopes and
/// name.
///
/// # Errors
/// [`AppError::NoSuchApp`]; a database failure.
pub async fn replace(db: &Db, actor: Actor<'_>, new: AppRow, scopes: Option<String>) -> Result<Record, AppError> {
    let id = new.id.clone();
    let record = with_conn!(db, |conn| conn
        .transaction::<_, AppError, _>(async |conn| {
            let current = load!(conn, &id);
            let at = now();
            diesel::update(apps::table.find(&id))
                .set((
                    apps::version.eq(&new.version),
                    apps::name.eq(&new.name),
                    apps::full_name.eq(&new.full_name),
                    apps::short_description.eq(&new.short_description),
                    apps::full_description.eq(&new.full_description),
                    apps::developer.eq(&new.developer),
                    apps::accent_color.eq(&new.accent_color),
                    apps::manifest.eq(&new.manifest),
                    apps::icons.eq(new.icons),
                    apps::updated_at.eq(at),
                ))
                .execute(conn)
                .await?;
            if let (Some(bot), Some(scopes)) = (&current.bot, &scopes) {
                diesel::update(bots::table.find(&bot.id))
                    .set((bots::scopes.eq(scopes), bots::updated_at.eq(at)))
                    .execute(conn)
                    .await?;
                diesel::update(users::table.find(&bot.id))
                    .set((users::display_name.eq(&new.name), users::updated_at.eq(at)))
                    .execute(conn)
                    .await?;
            }
            let detail = json!({"version": new.version});
            record!(conn, &Entry::new(actor, "app.package", &new, &detail));
            Ok(load!(conn, &id))
        })
        .await)?;
    trace(actor, "app.package", &record.app.name);
    Ok(record)
}

/// Applies `changes` to `record`; what changed, as the audit keeps it.
fn apply(record: &mut Record, changes: &Changes) -> Result<Map<String, Value>, AppError> {
    let mut detail = Map::new();
    if let Some(enabled) = changes.enabled.filter(|enabled| *enabled != record.app.enabled) {
        detail.insert("enabled".to_owned(), json!(enabled));
        record.app.enabled = enabled;
    }
    if let Some(everyone) = changes.everyone.filter(|everyone| *everyone != record.app.everyone) {
        detail.insert("everyone".to_owned(), json!(everyone));
        record.app.everyone = everyone;
    }
    if let Some(users) = &changes.users {
        let mut users = users.clone();
        users.sort();
        users.dedup();
        if users != record.users {
            detail.insert("users".to_owned(), json!(users));
            record.users = users;
        }
    }
    if changes.endpoint.is_some() || changes.auth_mode.is_some() {
        let bot = record.bot.as_mut().ok_or(AppError::NoBot)?;
        if let Some(endpoint) = changes
            .endpoint
            .as_ref()
            .filter(|endpoint| bot.endpoint.as_ref() != Some(*endpoint))
        {
            detail.insert("endpoint".to_owned(), json!(endpoint));
            bot.endpoint = Some(endpoint.clone());
        }
        if let Some(mode) = changes.auth_mode.filter(|mode| mode.as_str() != bot.auth_mode) {
            detail.insert("auth_mode".to_owned(), json!(mode.as_str()));
            mode.as_str().clone_into(&mut bot.auth_mode);
        }
    }
    Ok(detail)
}

/// Changes an app's state, users, or its bot's endpoint or mode. A change that changes nothing is not audited.
///
/// # Errors
/// [`AppError::NoSuchApp`], [`AppError::NoBot`], [`AppError::UnknownUser`] for a user who isn't a person's
/// account; a database failure.
pub async fn update(db: &Db, actor: Actor<'_>, id: &str, changes: &Changes) -> Result<Record, AppError> {
    let (record, audited) = with_conn!(db, |conn| conn
        .transaction::<_, AppError, _>(async |conn| {
            let mut record = load!(conn, id);
            let detail = apply(&mut record, changes)?;
            if detail.is_empty() {
                return Ok((record, false));
            }
            let at = now();
            record.app.updated_at = at;
            diesel::update(apps::table.find(id))
                .set((
                    apps::enabled.eq(record.app.enabled),
                    apps::everyone.eq(record.app.everyone),
                    apps::updated_at.eq(at),
                ))
                .execute(conn)
                .await?;
            if detail.contains_key("users") {
                let people: i64 = users::table
                    .filter(users::id.eq_any(&record.users))
                    .filter(users::kind.eq(PERSON))
                    .filter(users::deleted_at.is_null())
                    .count()
                    .get_result(conn)
                    .await?;
                if usize::try_from(people).ok() != Some(record.users.len()) {
                    return Err(AppError::UnknownUser);
                }
                diesel::delete(app_users::table.filter(app_users::app_id.eq(id)))
                    .execute(conn)
                    .await?;
                for user in &record.users {
                    diesel::insert_into(app_users::table)
                        .values((app_users::app_id.eq(id), app_users::user_id.eq(user)))
                        .execute(conn)
                        .await?;
                }
            }
            if let Some(bot) = &record.bot {
                diesel::update(bots::table.find(&bot.id))
                    .set((
                        bots::endpoint.eq(&bot.endpoint),
                        bots::auth_mode.eq(&bot.auth_mode),
                        bots::updated_at.eq(at),
                    ))
                    .execute(conn)
                    .await?;
            }
            record!(
                conn,
                &Entry::new(actor, "app.update", &record.app, &Value::Object(detail))
            );
            Ok((record, true))
        })
        .await)?;
    if audited {
        trace(actor, "app.update", &record.app.name);
    }
    Ok(record)
}

/// Replaces the digest of an app's bot's secret.
///
/// # Errors
/// [`AppError::NoSuchApp`], [`AppError::NoBot`]; a database failure.
pub async fn set_secret(db: &Db, actor: Actor<'_>, id: &str, digest: &str) -> Result<(), AppError> {
    let name = with_conn!(db, |conn| conn
        .transaction::<_, AppError, _>(async |conn| {
            let record = load!(conn, id);
            let bot = record.bot.as_ref().ok_or(AppError::NoBot)?;
            diesel::update(bots::table.find(&bot.id))
                .set((bots::secret_hash.eq(digest), bots::updated_at.eq(now())))
                .execute(conn)
                .await?;
            record!(conn, &Entry::new(actor, "app.secret", &record.app, &json!({})));
            Ok(record.app.name)
        })
        .await)?;
    trace(actor, "app.secret", &name);
    Ok(())
}

/// Deletes an app with its bot, users and installs; the bot's account row stays, deleted. What was deleted.
///
/// # Errors
/// [`AppError::NoSuchApp`]; a database failure.
pub async fn delete(db: &Db, actor: Actor<'_>, id: &str) -> Result<Record, AppError> {
    let record = with_conn!(db, |conn| conn
        .transaction::<_, AppError, _>(async |conn| {
            let record = load!(conn, id);
            diesel::delete(apps::table.find(id)).execute(conn).await?;
            if let Some(bot) = &record.bot {
                diesel::update(users::table.find(&bot.id))
                    .set((users::deleted_at.eq(Some(now())), users::updated_at.eq(now())))
                    .execute(conn)
                    .await?;
            }
            record!(conn, &Entry::new(actor, "app.delete", &record.app, &json!({})));
            Ok(record)
        })
        .await)?;
    trace(actor, "app.delete", &record.app.name);
    Ok(record)
}

/// The app `id`.
///
/// # Errors
/// A database failure.
pub async fn get(db: &Db, id: &str) -> Result<Option<Record>, DbError> {
    with_conn!(db, |conn| {
        let Some(app) = app_row!(conn, id) else {
            return Ok(None);
        };
        Ok(Some(filled!(conn, app)))
    })
}

/// Every app, by name in any case, then by id. Sorted here, not by the database, so both backends order names alike
/// whatever their collation.
///
/// # Errors
/// A database failure.
pub async fn list(db: &Db) -> Result<Vec<Record>, DbError> {
    let (mut rows, bots, users): (Vec<AppRow>, Vec<BotRow>, Vec<(String, String)>) = with_conn!(db, |conn| {
        let rows = apps::table.select(AppRow::as_select()).load(conn).await?;
        let bots = bots::table.select(BotRow::as_select()).load(conn).await?;
        let users = app_users::table
            .order(app_users::user_id.asc())
            .select((app_users::app_id, app_users::user_id))
            .load(conn)
            .await?;
        (rows, bots, users)
    });
    by_name(&mut rows);
    Ok(rows
        .into_iter()
        .map(|app| Record {
            bot: bots.iter().find(|bot| bot.app_id == app.id).cloned(),
            users: users
                .iter()
                .filter(|(app_id, _)| *app_id == app.id)
                .map(|(_, user)| user.clone())
                .collect(),
            app,
        })
        .collect())
}

/// What a bot's client credentials are checked against, when the bot is registered.
///
/// # Errors
/// A database failure.
pub async fn credentials(db: &Db, bot: &str) -> Result<Option<Credentials>, DbError> {
    let found: Option<(String, bool, String)> = with_conn!(db, |conn| bots::table
        .inner_join(apps::table)
        .filter(bots::id.eq(bot))
        .select((bots::secret_hash, apps::enabled, bots::auth_mode))
        .first(conn)
        .await
        .optional()?);
    Ok(found.map(|(secret_hash, enabled, mode)| Credentials {
        secret_hash,
        enabled,
        signed: mode == super::AuthMode::Signed.as_str(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bots_debug_form_never_shows_its_secrets_digest() {
        let bot = BotRow {
            id: "bot".to_owned(),
            app_id: "app".to_owned(),
            endpoint: None,
            secret_hash: "digest-of-the-secret".to_owned(),
            auth_mode: "signed".to_owned(),
            scopes: "[]".to_owned(),
            created_at: 0,
            updated_at: 0,
        };
        let shown = format!("{bot:?}");
        assert!(
            shown.contains("bot") && !shown.contains("digest-of-the-secret"),
            "{shown}"
        );
        let record = Record {
            app: app("app", "App"),
            bot: Some(bot),
            users: Vec::new(),
        };
        assert!(!format!("{record:?}").contains("digest-of-the-secret"));
        assert!(record.scopes().is_empty());
    }

    #[test]
    fn apps_are_listed_by_name_in_any_case_then_by_id() {
        let mut rows = vec![
            app("5", "Zz last"),
            app("4", "padded"),
            app("3", "O'Brien"),
            app("2", "echo"),
            app("1", "Echo"),
            app("0", "a"),
        ];
        by_name(&mut rows);
        let order: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(order, ["0", "1", "2", "3", "4", "5"]);
    }

    fn app(id: &str, name: &str) -> AppRow {
        AppRow {
            id: id.to_owned(),
            version: "1".to_owned(),
            name: name.to_owned(),
            full_name: name.to_owned(),
            short_description: String::new(),
            full_description: String::new(),
            developer: String::new(),
            accent_color: "#000000".to_owned(),
            manifest: "{}".to_owned(),
            source: "form".to_owned(),
            icons: false,
            everyone: true,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }
}
