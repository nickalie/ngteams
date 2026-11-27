//! A team managed (`docs/protocol/teams.md`): what the team pages ask of the engine, each as the web client's pages do it.
//!
//! - **Writes** ([`TeamWrite`]): sealed `teams-proto` writes through the outbox's one-attempt kind
//!   ([`crate::outbox::Outbox::try_channel`]), as a new channel's: no row, nothing sent again by a later run, one more attempt only after a 401 or a short throttle. An
//!   answer that isn't a 2xx is [`EngineError::TeamNotChanged`] with Teams' code.
//! - **The account's own view** ([`OwnTeamView`]): its channels hidden or shown, which reaches nobody, sent as plain
//!   requests.
//! - **The list** ([`TeamChange`]): what a write changed goes into the stored teams at once, through the engine's loop, and
//!   is kept against reads that don't show it yet ([`crate::teams::Teams`]), since CSA's list follows a change late.
//! - **Reads** ([`TeamAsk`]): read when a page opens, kept nowhere: a team's people, settings, tags, code, channels and
//!   analytics, the invitations waiting, the teams one can join, the people one can add.

use std::sync::Arc;

use teams_proto::Mri;
use teams_proto::{
    AddablePeopleSearch, AllTeamsAnalyticsRead, AnalyticsPeriod, Client, Credentials, HttpError, HttpTransport,
    JoinableTeam, JoinableTeams, ManagedChannel, OwnTeamView, PastedFormat, PastedPicture, PendingInvite,
    PendingInvites, Request, RosterMember, ShortProfileLookup, TagsRead, TeamAnalytics, TeamAnalyticsRead,
    TeamChannels, TeamCode, TeamJoinRequests, TeamPerson, TeamPicture, TeamRoster, TeamTags, TeamThread, TeamWrite,
    Transport, team_created, team_error_code,
};
use teams_store::{Team, TeamChannel};

use crate::channels::{Channels, Finished};
use crate::engine::{EngineError, Responder};
use crate::outbox::{CallRetries, CallTried, Outbox};
use crate::pictures::PictureBytes;

/// What a write changed, for the stored teams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamChange {
    /// An existing channel renamed, archived, or deleted.
    ChannelEdited {
        team: String,
        channel: String,
        name: String,
        description: String,
    },
    ChannelArchived {
        team: String,
        channel: String,
        archived: bool,
    },
    ChannelGone {
        team: String,
        channel: String,
    },
    /// A new team.
    Created(Team),
    /// A team deleted, or left.
    Gone(String),
    /// A team's name, description and privacy.
    Edited {
        team: String,
        name: Option<String>,
        description: String,
        public: Option<bool>,
    },
    /// A team archived, or restored.
    Archived {
        team: String,
        archived: bool,
    },
    /// All a team's channels hidden, or the team shown again.
    Hidden {
        team: String,
        hidden: bool,
    },
    /// One channel shown, or hidden.
    ChannelShown {
        team: String,
        channel: String,
        shown: bool,
    },
}

impl TeamChange {
    /// Whether `teams` show it already.
    fn shown_in(&self, teams: &[Team]) -> bool {
        let of = |id: &str| teams.iter().find(|team| team.id == id);
        match self {
            Self::ChannelEdited {
                team,
                channel,
                name,
                description,
            } => of(team)
                .and_then(|team| team.channels.iter().find(|row| row.id == *channel))
                .is_some_and(|row| row.name == *name && row.about.description == *description),
            Self::ChannelArchived {
                team,
                channel,
                archived,
            } => of(team)
                .and_then(|team| team.channels.iter().find(|row| row.id == *channel))
                .is_some_and(|row| row.about.archived == *archived),
            Self::ChannelGone { team, channel } => {
                of(team).is_none_or(|team| !team.channels.iter().any(|row| row.id == *channel))
            }
            Self::Created(team) => of(&team.id).is_some(),
            Self::Gone(team) => of(team).is_none(),
            Self::Edited {
                team,
                name,
                description,
                public,
            } => of(team).is_some_and(|team| {
                name.as_ref().is_none_or(|name| *name == team.name)
                    && team.about.description == *description
                    && public.is_none_or(|public| public == team.about.public)
            }),
            Self::Archived { team, archived } => of(team).is_some_and(|team| team.archived == *archived),
            Self::Hidden { team, hidden } => of(team).is_some_and(|team| team.about.hidden == *hidden),
            Self::ChannelShown { team, channel, shown } => of(team)
                .and_then(|team| team.channels.iter().find(|listed| listed.id == *channel))
                .is_some_and(|listed| listed.shown == *shown),
        }
    }

    fn apply_channel(&self, teams: &mut [Team]) {
        match self {
            Self::ChannelEdited {
                team,
                channel,
                name,
                description,
            } => {
                if let Some(row) = teams
                    .iter_mut()
                    .find(|row| row.id == *team)
                    .and_then(|team| team.channels.iter_mut().find(|row| row.id == *channel))
                {
                    row.name.clone_from(name);
                    row.about.description.clone_from(description);
                }
            }
            Self::ChannelArchived {
                team,
                channel,
                archived,
            } => {
                if let Some(row) = teams
                    .iter_mut()
                    .find(|row| row.id == *team)
                    .and_then(|team| team.channels.iter_mut().find(|row| row.id == *channel))
                {
                    row.about.archived = *archived;
                }
            }
            Self::ChannelGone { team, channel } => {
                if let Some(team) = teams.iter_mut().find(|row| row.id == *team) {
                    team.channels.retain(|row| row.id != *channel);
                }
            }
            _ => {}
        }
    }

    /// Puts it into `teams`.
    pub(crate) fn apply(&self, teams: &mut Vec<Team>) {
        match self {
            Self::ChannelEdited { .. } | Self::ChannelArchived { .. } | Self::ChannelGone { .. } => {
                self.apply_channel(teams);
            }
            Self::Created(team) => {
                if !teams.iter().any(|listed| listed.id == team.id) {
                    teams.push(team.clone());
                }
            }
            Self::Gone(team) => teams.retain(|listed| listed.id != *team),
            Self::Edited {
                team,
                name,
                description,
                public,
            } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    if let Some(name) = name {
                        listed.name.clone_from(name);
                    }
                    listed.about.description.clone_from(description);
                    if let Some(public) = public {
                        listed.about.public = *public;
                    }
                }
            }
            Self::Archived { team, archived } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    listed.archived = *archived;
                }
            }
            Self::Hidden { team, hidden } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    listed.about.hidden = *hidden;
                    if *hidden {
                        listed.channels.iter_mut().for_each(|channel| channel.shown = false);
                    } else if let Some(general) = listed.channels.iter_mut().find(|channel| channel.general) {
                        general.shown = true;
                    }
                }
            }
            Self::ChannelShown { team, channel, shown } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    if let Some(entry) = listed.channels.iter_mut().find(|entry| entry.id == *channel) {
                        entry.shown = *shown;
                    }
                    if *shown {
                        listed.about.hidden = false;
                    } else if listed.channels.iter().all(|entry: &TeamChannel| !entry.shown) {
                        listed.about.hidden = true;
                    }
                }
            }
        }
    }
}

/// Puts into `teams` each change they don't show yet; one they show, or kept long enough, is let go.
pub(crate) fn keep_changes(changes: &mut Vec<(TeamChange, tokio::time::Instant)>, teams: &mut Vec<Team>) {
    let now = tokio::time::Instant::now();
    changes.retain(|(change, until)| {
        if *until <= now || change.shown_in(teams) {
            return false;
        }
        change.apply(teams);
        true
    });
}

/// A write's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamWritten {
    /// A new team's thread and group.
    pub created: Option<(String, Option<String>)>,
    /// The people the answer says weren't added.
    pub not_added: Vec<String>,
    /// A new outgoing webhook and its token, which no later read shows.
    pub created_webhook: Option<teams_proto::CreatedWebhook>,
}

/// Someone on a team, as Manage team lists them. `Debug` leaves out everything but the MRI.
#[derive(Clone, PartialEq, Eq)]
pub struct TeamMember {
    pub mri: String,
    pub name: String,
    pub email: Option<String>,
    pub title: Option<String>,
    pub location: Option<String>,
    pub owner: bool,
    pub guest: bool,
    pub moderator: bool,
}

impl std::fmt::Debug for TeamMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamMember")
            .field("mri", &self.mri)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// What the team pages ask (see the module docs).
pub(crate) enum TeamAsk {
    ChannelSection {
        action: teams_proto::ChannelSectionAction,
        reply: Responder<()>,
    },
    ChannelRead {
        query: teams_proto::ChannelRead,
        reply: Responder<teams_proto::ChannelData>,
    },
    Write {
        write: TeamWrite,
        change: Option<TeamChange>,
        reply: Responder<TeamWritten>,
    },
    OwnView {
        requests: Vec<OwnTeamView>,
        change: Option<TeamChange>,
        reply: Responder<()>,
    },
    Roster {
        team: String,
        reply: Responder<Vec<TeamMember>>,
    },
    Settings {
        team: String,
        reply: Responder<Option<String>>,
    },
    Tags {
        team: String,
        reply: Responder<TagsRead>,
    },
    Code {
        team: String,
        reply: Responder<Option<String>>,
    },
    Channels {
        group: String,
        reply: Responder<Vec<ManagedChannel>>,
    },
    Invites {
        reply: Responder<Vec<PendingInvite>>,
    },
    Analytics {
        group: String,
        period: AnalyticsPeriod,
        reply: Responder<Option<TeamAnalytics>>,
    },
    AllAnalytics {
        period: AnalyticsPeriod,
        reply: Responder<Vec<TeamAnalytics>>,
    },
    Joinable {
        keywords: Option<String>,
        reply: Responder<Vec<JoinableTeam>>,
    },
    Addable {
        keyword: String,
        reply: Responder<Vec<TeamPerson>>,
    },
    JoinRequests {
        group: String,
        reply: Responder<Vec<TeamMember>>,
    },
    Picture {
        team: TeamPicture,
        reply: Responder<Option<PictureBytes>>,
    },
}

impl<C, T> Channels<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + 'static,
{
    /// Answers `ask`.
    pub(crate) fn team_ask(&mut self, ask: TeamAsk) {
        match ask {
            TeamAsk::ChannelSection { action, reply } => self.channel_section(action, reply),
            TeamAsk::ChannelRead { query, reply } => self.channel_read(query, reply),
            TeamAsk::Write { write, change, reply } => self.team_write(write, change, reply),
            TeamAsk::OwnView {
                requests,
                change,
                reply,
            } => self.own_view(requests, change, reply),
            TeamAsk::Roster { team, reply } => self.spawn_read(reply, async move |client| roster(client, team).await),
            TeamAsk::Settings { team, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamThread { team })
                    .await
                    .and_then(|body| TeamThread::read_settings(&body))
            }),
            TeamAsk::Tags { team, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamTags { team })
                    .await
                    .map_or_else(TagsRead::default, |body| TeamTags::read_answer(&body))
            }),
            TeamAsk::Code { team, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamCode { team })
                    .await
                    .and_then(|body| TeamCode::read_answer(&body))
            }),
            TeamAsk::Channels { group, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamChannels { group })
                    .await
                    .map_or_else(Vec::new, |body| TeamChannels::read_answer(&body))
            }),
            TeamAsk::Invites { reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &PendingInvites)
                    .await
                    .map_or_else(Vec::new, |body| PendingInvites::read_answer(&body))
            }),
            TeamAsk::Analytics { group, period, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamAnalyticsRead { group, period })
                    .await
                    .and_then(|body| TeamAnalyticsRead::read_answer(&body))
            }),
            TeamAsk::AllAnalytics { period, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &AllTeamsAnalyticsRead { period })
                    .await
                    .map_or_else(Vec::new, |body| AllTeamsAnalyticsRead::read_answer(&body))
            }),
            TeamAsk::Joinable { keywords, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &JoinableTeams { keywords })
                    .await
                    .map_or_else(Vec::new, |body| JoinableTeams::read_answer(&body))
            }),
            TeamAsk::Addable { keyword, reply } => self.spawn_read(reply, async move |client| {
                send_body(client, &AddablePeopleSearch { keyword })
                    .await
                    .map_or_else(Vec::new, |body| AddablePeopleSearch::read_answer(&body))
            }),
            TeamAsk::JoinRequests { group, reply } => self.spawn_read(reply, async move |client| {
                let asking = send_body(client, &TeamJoinRequests { group })
                    .await
                    .map_or_else(Vec::new, |body| TeamJoinRequests::read_answer(&body));
                named(client, asking.into_iter().map(|mri| (mri, false, false)).collect()).await
            }),
            TeamAsk::Picture { team, reply } => self.spawn_read(reply, async move |client| {
                let answer = client.read(&team).await.ok()?;
                let format = picture_format(answer.body())?;
                Some(Arc::new(PastedPicture {
                    format,
                    bytes: answer.body().to_vec(),
                }))
            }),
        }
    }

    /// Runs `read` beside the engine's loop and answers `reply` with what it found.
    pub(crate) fn spawn_read<V, F>(&self, reply: Responder<V>, read: F)
    where
        V: Send + 'static,
        F: AsyncFnOnce(&Client<HttpTransport<C>>) -> V + 'static,
    {
        spawn_read(Arc::clone(&self.client), reply, read);
    }

    fn channel_read(&self, query: teams_proto::ChannelRead, reply: Responder<teams_proto::ChannelData>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.read(&query).await {
                Ok(answer) => query
                    .read_answer(answer.body())
                    .ok_or(EngineError::TeamNotChanged { code: None }),
                Err(_) => Err(EngineError::TeamNotChanged { code: None }),
            };
            let _ = reply.send(answer);
        });
    }

    /// A move or a new section uses a complete, fresh hierarchy and never retries a stale version.
    fn channel_section(&mut self, action: teams_proto::ChannelSectionAction, reply: Responder<()>) {
        let client = Arc::clone(&self.client);
        self.running.spawn_local(async move {
            let answer = section_change(&client, &action).await;
            Finished {
                created: None,
                team: None,
                reread: answer.is_ok(),
                reply: Box::new(move || {
                    let _ = reply.send(answer);
                }),
            }
        });
    }

    fn team_write(&mut self, write: TeamWrite, change: Option<TeamChange>, reply: Responder<TeamWritten>) {
        let outbox = Arc::clone(&self.outbox);
        let client = Arc::clone(&self.client);
        self.running.spawn_local(async move {
            let sent = if let TeamWrite::Channel(channel) = &write {
                match crate::channel_admin::context(&client, channel).await {
                    Ok(crate::channel_admin::Decision::Send) => send(&outbox, &write).await,
                    Ok(crate::channel_admin::Decision::AlreadyApplied) => Ok(Vec::new()),
                    Err(error) => Err(error),
                }
            } else {
                send(&outbox, &write).await
            };
            let answer = sent.and_then(|body| {
                if matches!(write, TeamWrite::Channel(_)) && channel_partial_failure(&body) {
                    Err(EngineError::TeamNotChanged {
                        code: Some("MemberChangeRejected".to_owned()),
                    })
                } else {
                    Ok(written(&write, &body))
                }
            });
            let change = change_of(answer.as_ref().ok(), change);
            Finished {
                created: None,
                team: answer.is_ok().then_some(change).flatten().map(Box::new),
                reread: answer.is_ok(),
                reply: Box::new(move || {
                    let _ = reply.send(answer);
                }),
            }
        });
    }

    fn own_view(&mut self, requests: Vec<OwnTeamView>, change: Option<TeamChange>, reply: Responder<()>) {
        let client = Arc::clone(&self.client);
        self.running.spawn_local(async move {
            let mut failed = None;
            for request in &requests {
                match client.transport().channel_own(request).await {
                    Ok(answer) if (200..300).contains(&answer.status()) => {}
                    Ok(answer) => failed = Some(format!("{}", answer.status())),
                    Err(error) => failed = Some(error.to_string()),
                }
            }
            if failed.is_none()
                && let Some(TeamChange::ChannelShown {
                    channel, shown: false, ..
                }) = &change
            {
                let action = teams_proto::ChannelSectionAction::Remove {
                    channel: channel.clone(),
                };
                if section_change(&client, &action).await.is_err() {
                    failed = Some("section membership wasn't changed".to_owned());
                }
            }
            let answer = match failed {
                Some(why) => {
                    log::info!("the account's view of a team wasn't changed: {why}");
                    Err(EngineError::TeamNotChanged { code: None })
                }
                None => Ok(()),
            };
            Finished {
                created: None,
                team: answer.is_ok().then_some(change).flatten().map(Box::new),
                reread: true,
                reply: Box::new(move || {
                    let _ = reply.send(answer);
                }),
            }
        });
    }
}

async fn section_change<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    action: &teams_proto::ChannelSectionAction,
) -> Result<(), EngineError> {
    let read = client
        .read(&teams_proto::ChannelRead::Sections)
        .await
        .map_err(|_| EngineError::TeamNotChanged { code: None })?;
    let data = teams_proto::ChannelData::read(read.body()).ok_or(EngineError::TeamNotChanged { code: None })?;
    let sections = teams_proto::ChannelSections::read(&data.0).ok_or(EngineError::TeamNotChanged { code: None })?;
    if let teams_proto::ChannelSectionAction::Remove { channel } = action
        && !sections.sections.iter().any(|section| section.items.contains(channel))
    {
        return Ok(());
    }
    let write = teams_proto::ChannelSectionWrite::new(action, &sections).ok_or(EngineError::TeamNotChanged {
        code: Some("SectionChanged".to_owned()),
    })?;
    match client.transport().channel_own(&write).await {
        Ok(answer) if (200..300).contains(&answer.status()) => Ok(()),
        _ => Err(EngineError::TeamNotChanged { code: None }),
    }
}

/// The change for the stored teams: a new team's, with the thread and group its answer names, once it has them.
fn change_of(written: Option<&TeamWritten>, change: Option<TeamChange>) -> Option<TeamChange> {
    let (
        Some(TeamWritten {
            created: Some((thread, group)),
            ..
        }),
        Some(TeamChange::Created(team)),
    ) = (written, &change)
    else {
        return change;
    };
    let mut team = team.clone();
    team.id.clone_from(thread);
    team.group_id.clone_from(group);
    if let Some(general) = team.channels.iter_mut().find(|channel| channel.general) {
        general.id.clone_from(thread);
    }
    Some(TeamChange::Created(team))
}

/// What a write's answer `body` says.
fn written(write: &TeamWrite, body: &[u8]) -> TeamWritten {
    match write {
        TeamWrite::Create(_) => TeamWritten {
            created: team_created(body),
            ..TeamWritten::default()
        },
        TeamWrite::AddMembers { .. } => TeamWritten {
            not_added: not_added(body),
            ..TeamWritten::default()
        },
        TeamWrite::CreateOutgoingWebhook { .. } => TeamWritten {
            created_webhook: teams_proto::CreatedWebhook::read_answer(body),
            ..TeamWritten::default()
        },
        _ => TeamWritten::default(),
    }
}

/// Provisioning uses a 2xx with per-person failures for membership and moderator operations.
fn channel_partial_failure(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    ["multipleStatus", "failedMembers", "updatedMembersError"]
        .iter()
        .any(|key| {
            value
                .get(*key)
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| !items.is_empty())
        })
        || value
            .get("results")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.get("status").and_then(serde_json::Value::as_str) == Some("Failed"))
            })
}

/// The people an answer adding them says weren't added: `value.failedMembers`, by the bundle's reading of it, or
/// `updatedMembersError`.
fn not_added(body: &[u8]) -> Vec<String> {
    let Ok(answer) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let value = answer.get("value").unwrap_or(&answer);
    ["failedMembers", "updatedMembersError", "errors"]
        .iter()
        .filter_map(|key| value.get(*key))
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .filter_map(|failed| {
            failed
                .as_str()
                .or_else(|| failed.get("id").and_then(serde_json::Value::as_str))
                .or_else(|| failed.get("mri").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        })
        .collect()
}

/// One write, again only as [`CallRetries`] says: its answer's body on a 2xx.
async fn send<T, R>(outbox: &Outbox<T>, write: &R) -> Result<Vec<u8>, EngineError>
where
    T: Transport<Error = HttpError>,
    R: Request + Clone + Send,
{
    let mut retries = CallRetries::default();
    loop {
        let tried = outbox.try_channel(write.clone()).await;
        if let Some(wait) = retries.again(&tried) {
            tokio::time::sleep(wait).await;
            continue;
        }
        return match tried {
            CallTried::Answered(answer) if (200..300).contains(&answer.status()) => Ok(answer.body().to_vec()),
            CallTried::Answered(answer) => {
                let code = team_error_code(answer.body());
                log::info!(
                    "a team's write was refused: {} {}",
                    answer.status(),
                    code.as_deref().unwrap_or("no code")
                );
                Err(EngineError::TeamNotChanged { code })
            }
            CallTried::Denied(reason) => {
                log::info!("a team's write was refused: {reason}");
                Err(EngineError::TeamNotChanged { code: None })
            }
            CallTried::NotSent(why) | CallTried::Uncertain(why) => {
                log::info!("a team's write didn't go: {why}");
                Err(EngineError::TeamNotChanged { code: None })
            }
            CallTried::Throttled(_) => Err(EngineError::TeamNotChanged { code: None }),
        };
    }
}

pub(crate) async fn read_body<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    read: &(impl teams_proto::Read + Sync),
) -> Option<Vec<u8>> {
    match client.read(read).await {
        Ok(answer) => Some(answer.body().to_vec()),
        Err(error) => {
            log::info!("a team page's read failed: {error}");
            None
        }
    }
}

pub(crate) async fn send_body<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    request: &(impl teams_proto::Request + Sync),
) -> Option<Vec<u8>> {
    match client.transport().send(request).await {
        Ok(answer) if (200..300).contains(&answer.status()) => Some(answer.body().to_vec()),
        Ok(answer) => {
            log::info!("a team page's read answered {}", answer.status());
            None
        }
        Err(error) => {
            log::info!("a team page's read failed: {error}");
            None
        }
    }
}

/// A team's people, owners first, with their names; bots left out, as the web client's list does.
async fn roster<C: Credentials + 'static>(client: &Client<HttpTransport<C>>, team: String) -> Vec<TeamMember> {
    let mut listed: Vec<RosterMember> = Vec::new();
    for owners in [true, false] {
        let read = TeamRoster {
            team: team.clone(),
            owners,
        };
        if let Some(body) = read_body(client, &read).await {
            listed.extend(TeamRoster::read_answer(&body).into_iter().filter(|member| !member.bot));
        }
    }
    let moderators = listed
        .iter()
        .filter(|member| member.moderator)
        .map(|member| member.mri.clone())
        .collect::<Vec<_>>();
    let mut members = named(
        client,
        listed
            .into_iter()
            .map(|member| (member.mri, member.owner, member.guest))
            .collect(),
    )
    .await;
    for member in &mut members {
        member.moderator = moderators.contains(&member.mri);
    }
    members
}

/// `people` (MRI, owner, guest) with their names, from their profiles read ([`ShortProfileLookup`], a read by `POST`).
async fn named<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    people: Vec<(String, bool, bool)>,
) -> Vec<TeamMember> {
    let mut profiles = Vec::new();
    for page in people.chunks(100) {
        if let Some(lookup) = ShortProfileLookup::new(page.iter().map(|(mri, _, _)| Mri::new(mri.as_str()))) {
            match client.lookup(lookup.clone()).await {
                Ok(answer) => profiles.extend(lookup.read_answer(answer.body()).unwrap_or_default()),
                Err(error) => log::info!("a team's people's profiles couldn't be read: {error}"),
            }
        }
    }
    people
        .into_iter()
        .map(|(mri, owner, guest)| {
            let profile = profiles.iter().find(|profile| profile.mri.as_str() == mri);
            TeamMember {
                name: profile
                    .and_then(|profile| profile.display_name.clone())
                    .unwrap_or_else(|| "Unknown user".to_owned()),
                email: profile
                    .and_then(|profile| profile.email.clone().or_else(|| profile.user_principal_name.clone())),
                title: profile.and_then(|profile| profile.job_title.clone()),
                location: profile.and_then(|profile| profile.office_location.clone()),
                mri,
                owner,
                guest,
                moderator: false,
            }
        })
        .collect()
}

/// The format of a picture's bytes, by its first ones.
fn picture_format(bytes: &[u8]) -> Option<PastedFormat> {
    match bytes {
        [0xff, 0xd8, ..] => Some(PastedFormat::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(PastedFormat::Png),
        _ => None,
    }
}

/// Runs `read` through `client` beside the engine's loop and answers `reply` with what it found.
pub(crate) fn spawn_read<C, V, F>(client: Arc<Client<HttpTransport<C>>>, reply: Responder<V>, read: F)
where
    C: Credentials + 'static,
    V: Send + 'static,
    F: AsyncFnOnce(&Client<HttpTransport<C>>) -> V + 'static,
{
    tokio::task::spawn_local(async move {
        let found = read(&client).await;
        let _ = reply.send(Ok(found));
    });
}

#[cfg(test)]
mod tests {
    use teams_proto::NewTeam;
    use teams_store::ChannelKind;

    use super::*;

    fn channel(id: &str, general: bool, shown: bool) -> TeamChannel {
        TeamChannel {
            id: id.to_owned(),
            name: id.to_owned(),
            general,
            kind: ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown,
        }
    }

    fn team(id: &str) -> Team {
        Team {
            id: id.to_owned(),
            name: "Made-up".to_owned(),
            channels: vec![channel(id, true, true), channel("19:other@thread.tacv2", false, true)],
            ..Team::default()
        }
    }

    #[test]
    fn a_new_team_is_put_in_once_and_is_shown_by_its_id() {
        let mut teams = Vec::new();
        let created = TeamChange::Created(team("t"));
        assert!(!created.shown_in(&teams));
        created.apply(&mut teams);
        created.apply(&mut teams);
        assert_eq!(teams.len(), 1);
        assert!(created.shown_in(&teams));
        let gone = TeamChange::Gone("t".to_owned());
        assert!(!gone.shown_in(&teams));
        gone.apply(&mut teams);
        assert!(teams.is_empty());
        assert!(gone.shown_in(&teams));
    }

    #[test]
    fn an_edit_changes_what_it_names_and_keeps_what_it_leaves_out() {
        let mut teams = vec![team("t")];
        teams[0].about.public = true;
        let kept = TeamChange::Edited {
            team: "t".to_owned(),
            name: None,
            description: "new words".to_owned(),
            public: None,
        };
        assert!(!kept.shown_in(&teams));
        kept.apply(&mut teams);
        assert!(kept.shown_in(&teams));
        assert_eq!(teams[0].name, "Made-up");
        assert!(teams[0].about.public);
        let changed = TeamChange::Edited {
            team: "t".to_owned(),
            name: Some("Renamed".to_owned()),
            description: "new words".to_owned(),
            public: Some(false),
        };
        assert!(!changed.shown_in(&teams));
        changed.apply(&mut teams);
        assert!(changed.shown_in(&teams));
        assert_eq!(teams[0].name, "Renamed");
        assert!(!teams[0].about.public);
        // a team not listed is left alone, and isn't shown
        let elsewhere = TeamChange::Edited {
            team: "x".to_owned(),
            name: None,
            description: String::new(),
            public: None,
        };
        elsewhere.apply(&mut teams);
        assert!(!elsewhere.shown_in(&teams));
    }

    #[test]
    fn archiving_hiding_and_showing_a_channel_change_the_listed_team() {
        let mut teams = vec![team("t")];
        let archived = TeamChange::Archived {
            team: "t".to_owned(),
            archived: true,
        };
        assert!(!archived.shown_in(&teams));
        archived.apply(&mut teams);
        assert!(teams[0].archived && archived.shown_in(&teams));

        let hidden = TeamChange::Hidden {
            team: "t".to_owned(),
            hidden: true,
        };
        hidden.apply(&mut teams);
        assert!(hidden.shown_in(&teams));
        assert!(teams[0].channels.iter().all(|channel| !channel.shown));
        let shown = TeamChange::Hidden {
            team: "t".to_owned(),
            hidden: false,
        };
        shown.apply(&mut teams);
        assert!(!teams[0].about.hidden);
        assert_eq!(
            teams[0]
                .channels
                .iter()
                .map(|channel| channel.shown)
                .collect::<Vec<_>>(),
            [true, false],
            "the general channel comes back"
        );

        let other = TeamChange::ChannelShown {
            team: "t".to_owned(),
            channel: "19:other@thread.tacv2".to_owned(),
            shown: true,
        };
        assert!(!other.shown_in(&teams));
        other.apply(&mut teams);
        assert!(other.shown_in(&teams));
        for id in ["t", "19:other@thread.tacv2"] {
            TeamChange::ChannelShown {
                team: "t".to_owned(),
                channel: id.to_owned(),
                shown: false,
            }
            .apply(&mut teams);
        }
        assert!(teams[0].about.hidden, "a team with no channel shown is hidden");
        other.apply(&mut teams);
        assert!(!teams[0].about.hidden);
        let missing = TeamChange::ChannelShown {
            team: "t".to_owned(),
            channel: "19:missing@thread.tacv2".to_owned(),
            shown: true,
        };
        assert!(!missing.shown_in(&teams));
        for change in [archived, hidden, missing] {
            let mut none = Vec::new();
            change.apply(&mut none);
            assert!(none.is_empty());
        }
    }

    #[tokio::test]
    async fn changes_are_kept_until_the_read_shows_them_or_their_time_is_up() {
        let mut teams = vec![team("t")];
        let later = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut changes = vec![
            (
                TeamChange::Archived {
                    team: "t".to_owned(),
                    archived: true,
                },
                later,
            ),
            (TeamChange::Created(team("n")), tokio::time::Instant::now()),
            (TeamChange::Gone("x".to_owned()), later),
        ];
        keep_changes(&mut changes, &mut teams);
        assert_eq!(changes.len(), 1, "only the archive the read doesn't show is kept");
        assert!(teams[0].archived);
        assert_eq!(teams.len(), 1, "an expired change isn't put in");
        // the next read shows it: let go
        keep_changes(&mut changes, &mut teams);
        assert!(changes.is_empty());
    }

    #[test]
    fn a_new_team_takes_the_thread_and_group_its_answer_names() {
        let written = TeamWritten {
            created: Some(("19:made@thread.tacv2".to_owned(), Some("group".to_owned()))),
            ..TeamWritten::default()
        };
        let made = change_of(Some(&written), Some(TeamChange::Created(team("draft"))));
        let Some(TeamChange::Created(made)) = made else {
            unreachable!("a created team: {made:?}");
        };
        assert_eq!(made.id, "19:made@thread.tacv2");
        assert_eq!(made.group_id.as_deref(), Some("group"));
        assert_eq!(made.channels[0].id, "19:made@thread.tacv2");
        assert_eq!(made.channels[1].id, "19:other@thread.tacv2");
        let gone = Some(TeamChange::Gone("t".to_owned()));
        assert_eq!(change_of(Some(&written), gone.clone()), gone);
        assert_eq!(change_of(None, None), None);
    }

    #[test]
    fn a_write_s_answer_is_read_for_what_it_made_or_who_it_left_out() {
        let create = TeamWrite::Create(NewTeam {
            name: "n".to_owned(),
            description: String::new(),
            first_channel: "General".to_owned(),
            public: false,
        });
        let body = serde_json::json!({"value": {"objectId": "19:t@thread.tacv2", "siteInfo": {"groupId": "g"}}});
        assert_eq!(
            written(&create, body.to_string().as_bytes()).created,
            Some(("19:t@thread.tacv2".to_owned(), Some("g".to_owned())))
        );
        let add = TeamWrite::AddMembers {
            team: "t".to_owned(),
            group: "g".to_owned(),
            members: Vec::new(),
        };
        let body = serde_json::json!({"value": {
            "failedMembers": ["8:orgid:one", {"id": "8:orgid:two"}, {"mri": "8:orgid:three"}, {"other": 1}],
            "updatedMembersError": [{"id": "8:orgid:four"}],
        }});
        assert_eq!(
            written(&add, body.to_string().as_bytes()).not_added,
            ["8:orgid:one", "8:orgid:two", "8:orgid:three", "8:orgid:four"]
        );
        let bare = serde_json::json!({"errors": ["8:orgid:five"]});
        assert_eq!(not_added(bare.to_string().as_bytes()), ["8:orgid:five"]);
        assert!(not_added(b"not json").is_empty());
        let delete = TeamWrite::Delete { team: "t".to_owned() };
        assert_eq!(written(&delete, b"{}"), TeamWritten::default());
    }

    #[test]
    fn a_picture_s_format_is_told_by_its_first_bytes() {
        assert_eq!(picture_format(&[0xff, 0xd8, 0xff]), Some(PastedFormat::Jpeg));
        assert_eq!(picture_format(b"\x89PNG\r\n"), Some(PastedFormat::Png));
        assert_eq!(picture_format(b"GIF89a"), None);
        assert_eq!(picture_format(&[]), None);
    }

    #[test]
    fn a_member_s_debug_shows_only_the_mri_and_role() {
        let member = TeamMember {
            mri: "8:orgid:made-up".to_owned(),
            name: "Secret Name".to_owned(),
            email: Some("secret@example.com".to_owned()),
            title: None,
            location: None,
            owner: true,
            guest: false,
            moderator: false,
        };
        let shown = format!("{member:?}");
        assert!(shown.contains("8:orgid:made-up") && shown.contains("owner"));
        assert!(!shown.contains("Secret"));
    }
    #[test]
    fn optimistic_channel_changes_survive_stale_reads_and_settle_when_confirmed() {
        let id = "19:other@thread.tacv2";
        let mut listed = vec![team("t")];
        let edit = TeamChange::ChannelEdited {
            team: "t".into(),
            channel: id.into(),
            name: "Renamed".into(),
            description: "New description".into(),
        };
        assert!(!edit.shown_in(&listed));
        edit.apply(&mut listed);
        assert!(edit.shown_in(&listed));
        assert_eq!(listed[0].channels[1].about.description, "New description");
        let archive = TeamChange::ChannelArchived {
            team: "t".into(),
            channel: id.into(),
            archived: true,
        };
        assert!(!archive.shown_in(&listed));
        archive.apply(&mut listed);
        assert!(archive.shown_in(&listed));
        let gone = TeamChange::ChannelGone {
            team: "t".into(),
            channel: id.into(),
        };
        assert!(!gone.shown_in(&listed));
        gone.apply(&mut listed);
        assert!(gone.shown_in(&listed));
        assert_eq!(listed[0].channels.len(), 1);
        assert!(listed[0].channels[0].general);
    }
    #[test]
    fn missing_team_or_channel_is_not_replaced_by_optimistic_channel_changes() {
        for (team_id, channel) in [
            ("missing", "missing"),
            ("missing", "19:other@thread.tacv2"),
            ("t", "missing"),
        ] {
            let mut listed = vec![team("t")];
            for change in [
                TeamChange::ChannelEdited {
                    team: team_id.into(),
                    channel: channel.into(),
                    name: "After".into(),
                    description: "After".into(),
                },
                TeamChange::ChannelArchived {
                    team: team_id.into(),
                    channel: channel.into(),
                    archived: true,
                },
                TeamChange::ChannelGone {
                    team: team_id.into(),
                    channel: channel.into(),
                },
            ] {
                let mut empty = Vec::new();
                change.apply(&mut empty);
                assert!(empty.is_empty());
                let before = listed.clone();
                change.apply(&mut listed);
                assert_eq!(before, listed);
                assert_eq!(
                    change.shown_in(&listed),
                    matches!(change, TeamChange::ChannelGone { .. })
                );
            }
        }
    }
}
