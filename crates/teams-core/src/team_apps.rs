//! What Manage team's Apps tab, Change picture, Analytics' "Select a channel" and "More create team options" read
//! (`docs/protocol/teams.md`), and what the Apps page reads (`docs/protocol/apps.md`), each read when the page asks: a
//! team's installed apps, the store's apps with their definitions, the store's sections, the apps the account added and
//! a chat's apps, a channel's analytics, Teams' templates and one template's channels and apps, how many
//! Microsoft 365 groups the account could make a team of, and the picture Teams makes for a team from its name.
//! App definitions are cached per account by id for Activity preferences and icons. The writes these pages send go
//! as every team write does ([`crate::team_admin`]).
//!
//! **Bots by their app** ([`BotIndex`]): every definition these reads bring is kept for the run by the ids of the bots
//! it brings, so a bot's chat, its messages and the mention picker show the app's name and icon, as Teams does. A bot
//! the reads haven't brought is looked for in the account's apps (all of them, Teams' own and the store's offers too,
//! read at most once a minute), then, for a bot known to be one (a `28:` member or sender), by its id as an app's id
//! (many apps' bot has the app's id), until a read of it is answered.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use teams_proto::{
    AnalyticsPeriod, AppDefinitions, AppStore, ChannelAnalyticsRead, ChatApps, Credentials, DEFINITIONS_PAGE,
    DefaultTeamPicture, HttpError, InstalledApp, OwnedGroups, StoreApp, StoreSections, TeamAnalytics, TeamApps,
    TeamTemplateCard, TeamTemplateDetails, TeamTemplateRead, TeamTemplates, Transport, UserApps, UserGraphSettingsRead,
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
    /// The definition only when this run has it already: nothing is read.
    CachedDefinition {
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
    Listing {
        reply: Responder<AppListing>,
    },
    UserApps {
        reply: Responder<Vec<StoreApp>>,
    },
    /// The account's apps with a message extension, and the store's offers that have one.
    MessageExtensions {
        reply: Responder<Vec<teams_proto::ExtensionApp>>,
    },
    ChatApps {
        chat: String,
        reply: Responder<Vec<StoreApp>>,
    },
    /// A bot's app ([`BotIndex`]); `known` only: never read by the bot's id as an app's.
    BotApp {
        bot: String,
        known: bool,
        reply: Responder<Option<StoreApp>>,
    },
    /// The apps added to a chat (`team` false) or a team whose bot people can mention there.
    MentionableBots {
        place: String,
        team: bool,
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
    /// A team's outgoing webhooks, kept as bots ([`BotIndex`]).
    OutgoingWebhooks {
        team: String,
        reply: Responder<Vec<teams_proto::OutgoingWebhook>>,
    },
    OutgoingWebhookPicture {
        picture: teams_proto::OutgoingWebhookPicture,
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
        self.app_reads().answer(ask);
    }

    /// What answers the apps asks, sharing this run's definitions and bots ([`AppReads`]).
    pub(crate) fn app_reads(&self) -> AppReads<C> {
        AppReads {
            client: Arc::clone(&self.client),
            app_definitions: Arc::clone(&self.app_definitions),
            bots: Arc::clone(&self.bots),
        }
    }
}

/// What answers the apps asks ([`TeamAppsAsk`]): the account's client and the definitions and bots this run read. An
/// ask reads the service only, never the store, so the engine's loop and its inbox both answer one at once: a chat
/// list's bot lookup does not wait for a catch-up read (`engine/inbox.rs`).
pub(crate) struct AppReads<C> {
    client: Arc<teams_proto::Client<teams_proto::HttpTransport<C>>>,
    app_definitions: Arc<AppDefinitionsCache>,
    bots: Arc<BotIndex>,
}

impl<C: Credentials + 'static> AppReads<C> {
    /// Runs `read` beside the engine's loop and answers `reply` with what it found.
    fn spawn_read<V, F>(&self, reply: Responder<V>, read: F)
    where
        V: Send + 'static,
        F: AsyncFnOnce(&teams_proto::Client<teams_proto::HttpTransport<C>>) -> V + 'static,
    {
        crate::team_admin::spawn_read(Arc::clone(&self.client), reply, read);
    }

    /// Answers a team's outgoing webhooks' asks: their list, each kept as a bot ([`BotIndex`]), and a picture.
    fn answer_webhooks(&self, ask: TeamAppsAsk) {
        match ask {
            TeamAppsAsk::OutgoingWebhooks { team, reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| {
                    let read = teams_proto::OutgoingWebhooks { team };
                    let hooks = read_body(client, &read)
                        .await
                        .map_or_else(Vec::new, |body| teams_proto::OutgoingWebhook::list_answer(&body));
                    bots.keep(&hooks.iter().map(teams_proto::OutgoingWebhook::app).collect::<Vec<_>>());
                    hooks
                });
            }
            TeamAppsAsk::OutgoingWebhookPicture { picture, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &picture).await.filter(|bytes| !bytes.is_empty())
            }),
            _ => {}
        }
    }

    /// Answers `ask`.
    pub(crate) fn answer(&self, ask: TeamAppsAsk) {
        match ask {
            TeamAppsAsk::NotificationSettings { reply } => self.spawn_read(reply, notification_settings),
            TeamAppsAsk::Definition { id, reply } => {
                let cache = Arc::clone(&self.app_definitions);
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| definition(client, &cache, &bots, &id).await);
            }
            TeamAppsAsk::BotApp { bot, known, reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| bots.app_of(client, &bot, known).await);
            }
            TeamAppsAsk::MentionableBots { place, team, reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| mentionable(client, &bots, place, team).await);
            }
            TeamAppsAsk::CachedDefinition { id, reply } => {
                // the cache is never held across a read, so this answers at once
                let known = self.app_definitions.cached(&id);
                let _ = reply.send(Ok(known));
            }
            TeamAppsAsk::Apps { team, reply } => self.spawn_read(reply, async move |client| {
                let read = TeamApps { team };
                send_body(client, &read)
                    .await
                    .map_or_else(Vec::new, |body| read.read_answer(&body))
            }),
            TeamAppsAsk::Store { reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| store(client, &bots).await);
            }
            TeamAppsAsk::Listing { reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| listing(client, &bots).await);
            }
            TeamAppsAsk::UserApps { reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| user_apps(client, &bots).await);
            }
            TeamAppsAsk::MessageExtensions { reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| message_extensions(client, &bots).await);
            }
            TeamAppsAsk::ChatApps { chat, reply } => {
                let bots = Arc::clone(&self.bots);
                self.spawn_read(reply, async move |client| chat_apps(client, &bots, chat).await);
            }
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
            ask @ (TeamAppsAsk::OutgoingWebhooks { .. } | TeamAppsAsk::OutgoingWebhookPicture { .. }) => {
                self.answer_webhooks(ask);
            }
        }
    }
}

/// The store's apps with their definitions, kept in `bots`.
async fn store<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    bots: &BotIndex,
) -> Vec<StoreApp> {
    let ids = send_body(client, &AppStore)
        .await
        .map_or_else(Vec::new, |body| AppStore::read_answer(&body));
    bots.kept(definitions_of(client, &ids).await)
}

/// The store as the Apps page shows it, its apps kept in `bots`.
async fn listing<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    bots: &BotIndex,
) -> AppListing {
    let sections = send_body(client, &AppStore)
        .await
        .map_or_else(StoreSections::default, |body| StoreSections::read(&body));
    let apps = bots.kept(definitions_of(client, &sections.ids()).await);
    AppListing { sections, apps }
}

/// The account's apps; every app the answer defines is kept in `bots`.
async fn user_apps<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    bots: &BotIndex,
) -> Vec<StoreApp> {
    let Some(body) = send_body(client, &UserApps).await else {
        return Vec::new();
    };
    bots.keep(&UserApps::defined(&body));
    UserApps::read_answer(&body)
}

/// The account's apps with a message extension, read as the account's apps are, every app the answer defines kept in
/// `bots`.
async fn message_extensions<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    bots: &BotIndex,
) -> Vec<teams_proto::ExtensionApp> {
    let Some(body) = send_body(client, &UserApps).await else {
        return Vec::new();
    };
    bots.keep(&UserApps::defined(&body));
    teams_proto::extension_apps(&body)
}

/// The apps added to `chat`, kept in `bots`.
async fn chat_apps<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    bots: &BotIndex,
    chat: String,
) -> Vec<StoreApp> {
    bots.kept(
        read_body(client, &ChatApps { chat })
            .await
            .map_or_else(Vec::new, |body| ChatApps::read_answer(&body)),
    )
}

/// The store as the Apps page shows it: its sections, and the definitions of the apps they list (an app without a
/// readable one is left out of [`AppListing::apps`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppListing {
    pub sections: StoreSections,
    pub apps: Vec<StoreApp>,
}

impl AppListing {
    /// The app with `id`, when its definition was read.
    #[must_use]
    pub fn app(&self, id: &str) -> Option<&StoreApp> {
        self.apps.iter().find(|store| store.app.id == id)
    }
}

/// The definitions of `ids`, read a page at a time; a page that fails is left out.
async fn definitions_of<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    ids: &[String],
) -> Vec<StoreApp> {
    let mut apps = Vec::new();
    for page in ids.chunks(DEFINITIONS_PAGE) {
        let read = AppDefinitions { ids: page.to_vec() };
        if let Some(body) = send_body(client, &read).await {
            apps.extend(AppDefinitions::read_answer(&body));
        }
    }
    apps
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

/// Account-scoped definitions shared by Activity writes and readers such as app icons. `known` is locked only to look
/// up or insert, never across a read, so a cache-only ask for one app never waits on another app's read; `reading`
/// holds one lock per app across its read, so concurrent asks for one app send one request.
#[derive(Default)]
pub(crate) struct AppDefinitionsCache {
    known: std::sync::Mutex<BTreeMap<String, teams_proto::AppDefinition>>,
    reading: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl AppDefinitionsCache {
    /// `id`'s definition when the cache has it.
    fn cached(&self, id: &str) -> Option<teams_proto::AppDefinition> {
        self.known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    fn reading(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.reading
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(id.to_owned())
                .or_default(),
        )
    }

    fn insert(&self, id: &str, definition: teams_proto::AppDefinition) {
        self.known
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_owned(), definition);
    }
}

pub(crate) async fn definition<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    cache: &AppDefinitionsCache,
    bots: &BotIndex,
    id: &str,
) -> Option<teams_proto::AppDefinition> {
    if let Some(definition) = cache.cached(id) {
        return Some(definition);
    }
    let reading = cache.reading(id);
    let _reading = reading.lock().await;
    if let Some(definition) = cache.cached(id) {
        return Some(definition);
    }
    let body = send_body(
        client,
        &AppDefinitions {
            ids: vec![id.to_owned()],
        },
    )
    .await?;
    let app = AppDefinitions::read_answer(&body)
        .into_iter()
        .find(|app| app.definition.id() == id)?;
    bots.keep(std::slice::from_ref(&app));
    cache.insert(id, app.definition.clone());
    Some(app.definition)
}

/// How long after an unanswered read of the account's apps [`BotIndex`] reads them again.
const ACCOUNT_APPS_AGAIN: Duration = Duration::from_secs(60);

/// The apps this run read, by the ids of the bots they bring (lower case), and what it looked for in vain (see the
/// module docs).
#[derive(Default)]
pub(crate) struct BotIndex {
    apps: std::sync::Mutex<BTreeMap<String, StoreApp>>,
    /// When the account's apps were last read, and whether that read was answered; held across the read, so asks at
    /// once send one.
    account: tokio::sync::Mutex<Option<(Instant, bool)>>,
    /// The bots whose id was read as an app's and named none; a read that failed is no miss.
    missed: std::sync::Mutex<BTreeSet<String>>,
    /// One lock per bot across the read by its id, so asks at once for one bot share one answered read; an ask that
    /// waited on a read that failed sends its own.
    reading: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

/// A bot's id as the index keys it: without its `28:`, in lower case.
fn bot_key(bot: &str) -> String {
    bot.strip_prefix("28:").unwrap_or(bot).to_ascii_lowercase()
}

impl BotIndex {
    /// Keeps `apps` by their bots' ids.
    pub(crate) fn keep(&self, apps: &[StoreApp]) {
        let mut known = self.apps.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for app in apps {
            for bot in app.definition.bots() {
                known.insert(bot_key(bot.as_str()), app.clone());
            }
        }
    }

    /// [`Self::keep`], handing `apps` back.
    fn kept(&self, apps: Vec<StoreApp>) -> Vec<StoreApp> {
        self.keep(&apps);
        apps
    }

    /// The app of `bot` when this run has it.
    pub(crate) fn known(&self, bot: &str) -> Option<StoreApp> {
        self.apps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&bot_key(bot))
            .cloned()
    }

    /// The app of `bot` (`28:<id>`, or the id alone): as kept, else after reading the account's apps, else (unless
    /// `known` only) after reading the bot's id as an app's.
    async fn app_of<C: Credentials + 'static>(
        &self,
        client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
        bot: &str,
        known: bool,
    ) -> Option<StoreApp> {
        let key = bot_key(bot);
        if key.is_empty() {
            return None;
        }
        if let Some(app) = self.known(&key) {
            return Some(app);
        }
        self.read_account(client).await;
        if let Some(app) = self.known(&key) {
            return Some(app);
        }
        if known || self.missed(&key) {
            return None;
        }
        let reading = Arc::clone(
            self.reading
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(key.clone())
                .or_default(),
        );
        let _reading = reading.lock().await;
        // an ask that waited on another's read takes its answer
        if let Some(app) = self.known(&key) {
            return Some(app);
        }
        if self.missed(&key) {
            return None;
        }
        let body = send_body(client, &AppDefinitions { ids: vec![key.clone()] }).await?;
        self.keep(&AppDefinitions::read_answer(&body));
        let app = self.known(&key);
        if app.is_none() {
            self.missed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key);
        }
        app
    }

    /// Whether `key`'s id was read as an app's and named none.
    fn missed(&self, key: &str) -> bool {
        self.missed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(key)
    }

    /// Reads the account's apps into the index, unless a read was answered this run or one was sent less than
    /// [`ACCOUNT_APPS_AGAIN`] ago.
    async fn read_account<C: Credentials + 'static>(
        &self,
        client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    ) {
        let mut account = self.account.lock().await;
        if account.is_some_and(|(at, answered)| answered || at.elapsed() < ACCOUNT_APPS_AGAIN) {
            return;
        }
        let body = send_body(client, &UserApps).await;
        if let Some(body) = &body {
            self.keep(&UserApps::defined(body));
        }
        *account = Some((Instant::now(), body.is_some()));
    }
}

/// The apps added to `place` (a chat, or a team when `team`) whose bot people can mention there, kept in the index.
async fn mentionable<C: Credentials + 'static>(
    client: &teams_proto::Client<teams_proto::HttpTransport<C>>,
    bots: &BotIndex,
    place: String,
    team: bool,
) -> Vec<StoreApp> {
    let apps = if team {
        let read = TeamApps { team: place };
        bots.kept(
            send_body(client, &read)
                .await
                .map_or_else(Vec::new, |body| read.read_defined(&body)),
        )
    } else {
        chat_apps(client, bots, place).await
    };
    apps.into_iter()
        .filter(|app| app.definition.mentionable_bot(team).is_some())
        .collect()
}
