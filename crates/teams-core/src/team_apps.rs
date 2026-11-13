//! What Manage team's Apps tab, Change picture, Analytics' "Select a channel" and "More create team options" read
//! (`docs/protocol/teams.md`), each read when the page asks: a team's installed apps, the store's apps
//! with their definitions, a channel's analytics, Teams' templates and one template's channels and apps, how many
//! Microsoft 365 groups the account could make a team of, and the picture Teams makes for a team from its name.
//! App definitions are cached per account by id for Activity preferences and icons. The writes these pages send go
//! as every team write does ([`crate::team_admin`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use teams_proto::{
    AnalyticsPeriod, AppDefinitions, AppStore, ChannelAnalyticsRead, Credentials, DEFINITIONS_PAGE, DefaultTeamPicture,
    HttpError, InstalledApp, OwnedGroups, StoreApp, TeamAnalytics, TeamApps, TeamTemplateCard, TeamTemplateDetails,
    TeamTemplateRead, TeamTemplates, Transport, UserGraphSettingsRead,
};

use crate::channels::Channels;
use crate::engine::Responder;
use crate::team_admin::{read_body, send_body};

/// What these pages ask (see the module docs).
pub(crate) enum TeamAppsAsk {
    NotificationSettings {
        reply: Responder<Option<teams_proto::UserGraphSettings>>,
    },
    Definition {
        id: String,
        reply: Responder<Option<teams_proto::AppDefinition>>,
    },
    Apps {
        team: String,
        reply: Responder<Vec<InstalledApp>>,
    },
    Store {
        reply: Responder<Vec<StoreApp>>,
    },
    ChannelAnalytics {
        group: String,
        channel: String,
        period: AnalyticsPeriod,
        reply: Responder<Option<TeamAnalytics>>,
    },
    Templates {
        reply: Responder<Vec<TeamTemplateCard>>,
    },
    Template {
        id: String,
        reply: Responder<Option<TeamTemplateDetails>>,
    },
    OwnedGroups {
        reply: Responder<Option<usize>>,
    },
    DefaultPicture {
        name: String,
        reply: Responder<Option<Vec<u8>>>,
    },
}

impl<C, T> Channels<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + 'static,
{
    /// Answers `ask`.
    pub(crate) fn team_apps_ask(&mut self, ask: TeamAppsAsk) {
        match ask {
            TeamAppsAsk::NotificationSettings { reply } => self.spawn_read(reply, notification_settings),
            TeamAppsAsk::Definition { id, reply } => {
                let cache = Arc::clone(&self.app_definitions);
                self.spawn_read(reply, async move |client| definition(client, &cache, &id).await);
            }
            TeamAppsAsk::Apps { team, reply } => self.spawn_read(reply, async move |client| {
                let read = TeamApps { team };
                send_body(client, &read)
                    .await
                    .map_or_else(Vec::new, |body| read.read_answer(&body))
            }),
            TeamAppsAsk::Store { reply } => self.spawn_read(reply, async move |client| {
                let ids = send_body(client, &AppStore)
                    .await
                    .map_or_else(Vec::new, |body| AppStore::read_answer(&body));
                let mut apps = Vec::new();
                for page in ids.chunks(DEFINITIONS_PAGE) {
                    let read = AppDefinitions { ids: page.to_vec() };
                    if let Some(body) = send_body(client, &read).await {
                        apps.extend(AppDefinitions::read_answer(&body));
                    }
                }
                apps
            }),
            TeamAppsAsk::ChannelAnalytics {
                group,
                channel,
                period,
                reply,
            } => self.spawn_read(reply, async move |client| {
                read_body(client, &ChannelAnalyticsRead { group, channel, period })
                    .await
                    .and_then(|body| ChannelAnalyticsRead::read_answer(&body))
            }),
            TeamAppsAsk::Templates { reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamTemplates)
                    .await
                    .map_or_else(Vec::new, |body| TeamTemplates::read_answer(&body))
            }),
            TeamAppsAsk::Template { id, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamTemplateRead { id })
                    .await
                    .and_then(|body| TeamTemplateRead::read_answer(&body))
            }),
            TeamAppsAsk::OwnedGroups { reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &OwnedGroups)
                    .await
                    .and_then(|body| OwnedGroups::read_answer(&body))
            }),
            TeamAppsAsk::DefaultPicture { name, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &DefaultTeamPicture { name })
                    .await
                    .filter(|bytes| !bytes.is_empty())
            }),
        }
    }
}

/// The account's app notification preferences, read now; `None` when they couldn't be read.
async fn notification_settings<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
) -> Option<teams_proto::UserGraphSettings> {
    let answer = match client.read(&UserGraphSettingsRead).await {
        Ok(answer) => answer,
        Err(HttpError::Status(answer)) => *answer,
        Err(error) => {
            log::info!("the app notification preferences' read failed: {error}");
            return None;
        }
    };
    UserGraphSettingsRead::settings_of(answer.status(), answer.body()).ok()
}

/// Account-scoped definitions shared by Activity writes and readers such as app icons.
pub(crate) type AppDefinitionsCache = Arc<tokio::sync::Mutex<BTreeMap<String, teams_proto::AppDefinition>>>;

pub(crate) async fn definition<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    cache: &AppDefinitionsCache,
    id: &str,
) -> Option<teams_proto::AppDefinition> {
    let mut definitions = cache.lock().await;
    if let Some(definition) = definitions.get(id) {
        return Some(definition.clone());
    }
    let body = send_body(
        client,
        &AppDefinitions {
            ids: vec![id.to_owned()],
        },
    )
    .await?;
    let definition = AppDefinitions::read_answer(&body)
        .into_iter()
        .find(|app| app.definition.id() == id)?
        .definition;
    definitions.insert(id.to_owned(), definition.clone());
    Some(definition)
}
