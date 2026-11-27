//! Teams and channels, as the client's team pages and channel dialogs ask for them: CSA (the account's teams,
//! channel sections, tags), the middle tier (teams, their people, standard channels) and the provisioning service
//! (private channels, archiving, the account's own view). Every channel is a thread of the chat service, so its
//! messages need nothing here.
//!
//! Who may do what is decided from the database on every request: a team's owners manage it, its members do what
//! its settings allow, and nobody else learns that it exists. A change names its target by ids the caller must
//! already be allowed to see; the request body never grants a role.

mod csa;
mod folders;
mod mt;
mod people;
mod prov;
pub(crate) mod repo;
pub(crate) mod schema;
pub(crate) mod wire;

use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRequestParts};
use axum::http::{StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::auth::extract::{Audience, Bearer};
use crate::config::Config;
use crate::db::{Db, DbError, users};
use repo::{Channel, Team, TeamMember};

/// The largest JSON body a team or channel request takes; the calendar's routes, merged in below, set their own.
const BODY_LIMIT: usize = 64 * 1024;

/// CSA's audience.
pub(crate) struct Csa;
impl Audience for Csa {
    const AUDIENCE: &'static str = "https://chatsvcagg.teams.microsoft.com";
}

/// The provisioning service's audience.
pub(crate) struct Templates;
impl Audience for Templates {
    const AUDIENCE: &'static str = "https://templates.teams.microsoft.com";
}

pub(crate) fn router(config: &Config) -> Router<Arc<App>> {
    Router::new()
        .nest(&crate::services::csa_prefix(config), csa::routes())
        .nest(
            &crate::services::middle_tier_prefix(config),
            mt::routes().merge(crate::calendar::routes()),
        )
        .nest(&crate::services::provisioning_prefix(config), prov::routes())
        .nest(&crate::services::user_profile_prefix(config), people::routes())
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// The enabled account a request of audience `A` comes from.
pub(crate) struct Actor<A> {
    pub id: String,
    pub tenant: String,
    audience: PhantomData<A>,
}

impl<A: Audience> FromRequestParts<Arc<App>> for Actor<A> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let bearer = Bearer::<A>::from_request_parts(parts, app)
            .await
            .map_err(IntoResponse::into_response)?;
        let user = users::by_id(&app.db, &bearer.claims.oid)
            .await
            .map_err(|error| Error::from(error).into_response())?;
        if user.is_none_or(|user| user.disabled) {
            return Err(Error::FORBIDDEN.into_response());
        }
        Ok(Self {
            id: bearer.claims.oid,
            tenant: bearer.claims.tid,
            audience: PhantomData,
        })
    }
}

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// The object id an MRI of a work account names.
fn object_id(mri: &str) -> Option<&str> {
    mri.strip_prefix("8:orgid:")
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
}

/// A refusal, with the error code the client's dialogs read.
#[derive(Debug)]
pub(crate) enum Error {
    Code(StatusCode, &'static str),
    Database(DbError),
}

impl Error {
    const BAD_REQUEST: Self = Self::Code(StatusCode::BAD_REQUEST, "BadRequest");
    const FORBIDDEN: Self = Self::Code(StatusCode::FORBIDDEN, "Forbidden");
    const NOT_FOUND: Self = Self::Code(StatusCode::NOT_FOUND, "NotFound");
    const ARCHIVED: Self = Self::Code(StatusCode::FORBIDDEN, "OperationNotAllowedOnArchivedTeam");
    const NAME_TAKEN: Self = Self::Code(StatusCode::CONFLICT, "ChannelNameAlreadyExist");
    const LAST_OWNER: Self = Self::Code(StatusCode::FORBIDDEN, "LastOwnerCannotBeRemoved");
}

impl From<DbError> for Error {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Code(status, code) => (status, Json(json!({"errorCode": code, "message": code}))).into_response(),
            Self::Database(error) => {
                tracing::error!(%error, "teams database failure");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
    }
}

type Answer<T = Response> = Result<T, Error>;

/// A JSON body, or [`Error::BAD_REQUEST`].
fn body(body: Result<Json<Value>, axum::extract::rejection::JsonRejection>) -> Answer<Value> {
    body.map(|Json(body)| body).map_err(|_| Error::BAD_REQUEST)
}

/// A team name (1–256 characters), a channel name (1–50) or a description (at most 1024), trimmed.
fn text(value: &Value, max: usize, required: bool) -> Answer<String> {
    let text = match value {
        Value::Null if !required => "",
        Value::String(text) => text.trim(),
        _ => return Err(Error::BAD_REQUEST),
    };
    let length = text.chars().count();
    if length > max || (required && length == 0) {
        return Err(Error::BAD_REQUEST);
    }
    Ok(text.to_owned())
}

/// The caller's place on a team.
pub(crate) struct Seat {
    pub team: Team,
    pub member: TeamMember,
}

impl Seat {
    /// The caller's seat on the team of this thread; someone not on it learns only that it isn't there for them.
    async fn of(db: &Db, user: &str, team: &str) -> Answer<Self> {
        let found = repo::team(db, team).await?.ok_or(Error::NOT_FOUND)?;
        Self::on(db, user, found).await
    }

    async fn of_group(db: &Db, user: &str, group: &str) -> Answer<Self> {
        let found = repo::team_by_group(db, group).await?.ok_or(Error::NOT_FOUND)?;
        Self::on(db, user, found).await
    }

    async fn on(db: &Db, user: &str, team: Team) -> Answer<Self> {
        let member = repo::membership(db, &team.id, user).await?.ok_or(Error::NOT_FOUND)?;
        Ok(Self { team, member })
    }

    fn owner(&self) -> Answer<()> {
        if self.member.owner {
            Ok(())
        } else {
            Err(Error::FORBIDDEN)
        }
    }

    fn active(&self) -> Answer<()> {
        if self.team.archived {
            Err(Error::ARCHIVED)
        } else {
            Ok(())
        }
    }

    /// Whether the team's settings let its members do `permission` (`createTopic`, …); its owners always may.
    fn may(&self, permission: &str) -> bool {
        self.member.owner || wire::members_may(&self.team, permission, false)
    }
}

/// A channel the caller may see, with the caller's seat on its team: a private channel only for its members.
async fn channel_seat(db: &Db, user: &str, channel: Channel) -> Answer<(Seat, Channel)> {
    let seat = Seat::of(db, user, &channel.team_id).await?;
    if channel.private
        && repo::joined(db, user, std::slice::from_ref(&channel.id))
            .await?
            .is_empty()
    {
        return Err(Error::NOT_FOUND);
    }
    Ok((seat, channel))
}

async fn channel_of(db: &Db, user: &str, channel: &str) -> Answer<(Seat, Channel)> {
    let channel = repo::channel(db, channel).await?.ok_or(Error::NOT_FOUND)?;
    channel_seat(db, user, channel).await
}

/// Whether a channel is in the account's channel list: as the account set it, else a general, private or
/// shown-by-default channel.
fn shown(channel: &Channel, own: Option<&Map<String, Value>>) -> bool {
    let key = if channel.general() {
        "isGeneralChannelFavorite"
    } else {
        "favorite"
    };
    match own.and_then(|own| own.get(key)).and_then(Value::as_str) {
        Some(set) => set == "true",
        None => channel.general() || channel.private || channel.shown_by_default,
    }
}

/// After the account showed or hid a channel: its team is hidden while none of its channels is listed.
pub(crate) async fn listed(db: &Db, thread: &str, user: &str) -> Result<(), DbError> {
    let Some(channel) = repo::channel(db, thread).await? else {
        return Ok(());
    };
    let channels = repo::channels_of(db, std::slice::from_ref(&channel.team_id)).await?;
    let ids: Vec<String> = channels.iter().map(|channel| channel.id.clone()).collect();
    let joined = repo::joined(db, user, &ids).await?;
    let own = repo::own_properties(db, user, &ids).await?;
    let any = channels
        .iter()
        .filter(|channel| joined.contains(&channel.id))
        .any(|channel| {
            let own = own.iter().find(|(id, _)| *id == channel.id).map(|(_, own)| own);
            shown(channel, own)
        });
    repo::mark_hidden(db, &channel.team_id, user, !any).await
}

fn thread_id() -> String {
    format!("19:{}@thread.tacv2", uuid::Uuid::new_v4().simple())
}
