//! The account's teams and their channels (M2 Task 16), read-only: CSA's one read ([`CsaTeams`], `GET`, on the
//! transport's CSA base) in the background, kept in the store and read again by age; a channel's posts and their reply
//! chains are the chat service's, synced like a chat's ([`crate::sync`]: each channel message's post recorded as it is
//! stored).
//!
//! - **Asked for by a view** ([`EngineRequests::teams`](crate::EngineRequests::teams)): answered from the store at once
//!   (`None` before the first read settled the list), and a read started when the stored list is missing or older
//!   than [`TEAMS_MAX_AGE`], unless one is under way or one failed less than the engine's
//!   [`EngineOptions::teams_retry`](crate::EngineOptions::teams_retry) ago. Its result is stored whole; a list that
//!   changed is told as [`Event::Teams`](crate::Event::Teams).
//! - **Read again when a team appears**: a team's thread a read or a delivery brings that no stored team names (the
//!   account was just added to a team: its first channel joins its conversations, and its posts come) reads the teams
//!   at once, whatever the stored list's age ([`Teams::note_conversation`]), once per such thread a run, so a team the
//!   account was added to shows in its list (and its channels' meetings are found) without waiting out
//!   [`TEAMS_MAX_AGE`]. So does a team's activity delivered on its thread (`ThreadActivity/…`: people added or gone, a
//!   channel made, renamed or deleted, [`Teams::note_activity`]), so a new channel shows too.
//! - **And the favourites (chats and channels)**: the same answer's `Favorites` folder (the chats and channels Teams
//!   lists under "Favorites" above its chats, in the user's order), stored with the teams when it could be read
//!   ([`TeamsRead::favorites`]; a read that couldn't keeps the stored ones), a change told as
//!   [`Event::Teams`](crate::Event::Teams) too. The chat list asks for them
//!   ([`EngineRequests::chat_listing`](crate::EngineRequests::chat_listing)), which starts a read as asking for the
//!   teams does.
//! - **Writes**: creating and managing channels uses [`crate::channels`]. This module only reads and stores teams
//!   and channel posts; a channel's posts, replies and reactions are the outbox's writes, as a chat's.

use std::collections::HashSet;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use teams_proto::{ChannelType, Client, Credentials, CsaAnswerError, CsaTeams, HttpError, HttpTransport};
use teams_store::{Applied, ChannelKind, Store, StoreError, Team, TeamAbout, TeamChannel};

use crate::channels::Created;
use crate::team_admin::{TeamChange, keep_changes};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// How old the stored teams may get before a view's asking reads them again: an hour.
pub const TEAMS_MAX_AGE: Duration = Duration::from_hours(1);

/// How long after a read that failed the teams are read again, at the soonest: five minutes.
pub const TEAMS_RETRY: Duration = Duration::from_mins(5);

/// What a failed read of the teams is told as (`Event::Failed`'s `during`): the Teams list says the teams can't be
/// read while none was ever stored.
pub const READING_TEAMS: &str = "reading the teams";

/// Why the teams couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum TeamsReadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Answer(#[from] CsaAnswerError),
    /// The read's task ended without an answer (it panicked or was cancelled).
    #[error("the read ended without an answer")]
    Lost,
}

/// What the chat list shows besides the stored conversations ([`EngineRequests::chat_listing`](crate::EngineRequests::chat_listing)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatListing {
    /// The favourites' ids (chats and channels), in the order Teams lists them under "Favorites" (`48:notes` is the
    /// account's own notes, its chat with itself); `None` before a read could read them.
    pub favorites: Option<Vec<String>>,
    /// The conversations the chat service last called empty (`isemptyconversation`): Teams lists none of them.
    pub empty: Vec<String>,
}

/// What one read of the teams gave: the teams, and the favourites (chats and channels) when the answer's folders could
/// be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TeamsRead {
    pub(crate) teams: Vec<Team>,
    pub(crate) favorites: Option<Vec<String>>,
}

/// How long a channel just made is kept in the list when a read doesn't name it yet.
const CREATED_KEPT: Duration = Duration::from_mins(10);

/// The background read of the teams.
pub(crate) struct Teams<C> {
    client: Arc<Client<HttpTransport<C>>>,
    running: JoinSet<Result<TeamsRead, TeamsReadError>>,
    /// After a failed read: not before then.
    retry_at: Option<Instant>,
    retry: Duration,
    /// The channels made here that no read has named yet, until when they are kept.
    created: Vec<(Created, Instant)>,
    /// A channel was made while a read ran: another read follows it, which may name it.
    read_again: bool,
    /// What the team pages changed that no read has shown yet, until when it is kept ([`crate::team_admin`]).
    changes: Vec<(TeamChange, Instant)>,
    /// The stored teams' channels, as last looked at ([`Self::note_conversation`]); `None` once a read stored others.
    known: Option<HashSet<String>>,
    /// The team threads the stored list didn't name that read the teams again: each reads them once a run.
    asked: HashSet<String>,
}

impl<C: Credentials + 'static> Teams<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, retry: Duration) -> Self {
        Self {
            client,
            running: JoinSet::new(),
            retry_at: None,
            retry,
            created: Vec::new(),
            read_again: false,
            changes: Vec::new(),
            known: None,
            asked: HashSet::new(),
        }
    }

    /// A team's activity was delivered (its thread's `ThreadActivity/…`: people added or gone, a channel made, renamed
    /// or deleted): the teams are read again now, whatever the stored list's age.
    pub(crate) fn note_activity(&mut self) {
        log::info!("a team's activity came: reading the teams again");
        self.read_now();
    }

    /// A conversation a read or delivery brought (see the module docs): a team's thread that no stored team names (the
    /// account was just added to the team, or a channel was made in it) reads the teams now, whatever the stored list's
    /// age, once per thread a run; the teams stored before the first read don't count (that read is due anyway).
    pub(crate) fn note_conversation(&mut self, store: &Store, id: &str) -> Result<(), StoreError> {
        if self.asked.contains(id) || store.teams_read_at()?.is_none() {
            return Ok(());
        }
        let known = match &mut self.known {
            Some(known) => known,
            empty @ None => empty.insert(
                store
                    .teams()?
                    .into_iter()
                    .flat_map(|team| team.channels.into_iter().map(|channel| channel.id))
                    .collect(),
            ),
        };
        if !known.contains(id) {
            log::info!("a team's thread the stored teams don't name: reading the teams again");
            self.asked.insert(id.to_owned());
            self.read_now();
        }
        Ok(())
    }

    /// Puts what a team's write changed in the stored list, and reads the list again: the service's own list follows it
    /// late, so the change is kept against reads that don't show it ([`CREATED_KEPT`]). Whether the stored list changed.
    pub(crate) fn note_change(&mut self, store: &mut Store, change: Option<TeamChange>) -> Result<bool, StoreError> {
        let Some(change) = change else {
            self.read_now();
            return Ok(false);
        };
        let mut teams = store.teams()?;
        note_into(&mut self.changes, &mut teams, change);
        let read_at = store.teams_read_at()?.unwrap_or_else(now_ms);
        let applied = store.replace_teams(&teams, read_at)?;
        self.read_now();
        Ok(applied != Applied::Ignored)
    }

    /// Stores a channel just made in its team's list, and reads the list again: the service's own list may not name it
    /// yet, so it is kept against reads that don't ([`CREATED_KEPT`]). Whether the stored list changed.
    pub(crate) fn note_created(&mut self, store: &mut Store, created: Created) -> Result<bool, StoreError> {
        let mut teams = store.teams()?;
        created_into(&mut self.created, &mut teams, created);
        let read_at = store.teams_read_at()?.unwrap_or_else(now_ms);
        let applied = store.replace_teams(&teams, read_at)?;
        self.read_now();
        Ok(applied != Applied::Ignored)
    }

    /// Starts a read now, whatever the stored list's age, or one more after the running one.
    fn read_now(&mut self) {
        if self.running.is_empty() {
            let client = Arc::clone(&self.client);
            self.running.spawn(async move { read(&client).await });
        } else {
            self.read_again = true;
        }
    }

    /// What the chat list needs besides the conversations: the stored favourite chats and channels (`None` before a
    /// read could read them) and the conversations the chat service calls empty; a read starts when the teams are due
    /// one, as [`Self::teams`] starts it.
    pub(crate) fn chat_listing(&mut self, store: &Store) -> Result<ChatListing, StoreError> {
        self.read_if_due(store)?;
        stored_listing(store)
    }

    /// The stored teams (`None` before the first read settled them); a read starts when they are due one (see the
    /// module docs).
    pub(crate) fn teams(&mut self, store: &Store) -> Result<Option<Vec<Team>>, StoreError> {
        self.read_if_due(store)?;
        stored_teams(store)
    }

    /// Starts a read when the stored teams are missing or old, unless one runs or a failed one's wait does.
    fn read_if_due(&mut self, store: &Store) -> Result<(), StoreError> {
        let old = store.teams_read_at()?.is_none_or(|read_at| {
            Duration::from_millis(u64::try_from(now_ms().saturating_sub(read_at)).unwrap_or(0)) > TEAMS_MAX_AGE
        });
        let waiting = self.retry_at.is_some_and(|at| Instant::now() < at);
        if old && !waiting && self.running.is_empty() {
            let client = Arc::clone(&self.client);
            self.running.spawn(async move { read(&client).await });
        }
        Ok(())
    }

    /// Whether a read runs.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    #[cfg(test)]
    async fn next(&mut self) -> Option<Result<TeamsRead, TeamsReadError>> {
        std::future::poll_fn(|cx| self.poll_next(cx)).await
    }

    pub(crate) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<TeamsRead, TeamsReadError>>> {
        self.running
            .poll_join_next(cx)
            .map(|joined| joined.map(|read| read.unwrap_or(Err(TeamsReadError::Lost))))
    }

    /// Stores a read's teams and favourite chats and channels (these only when it could read them); whether either
    /// changed. A failed read waits [`Self::retry`] before the next.
    pub(crate) fn store(
        &mut self,
        store: &mut Store,
        read: Result<TeamsRead, TeamsReadError>,
    ) -> Result<Result<bool, TeamsReadError>, StoreError> {
        match read {
            Ok(read) => {
                self.retry_at = None;
                let now = now_ms();
                let mut listed = read.teams;
                keep_created(&mut self.created, &mut listed);
                keep_changes(&mut self.changes, &mut listed);
                let teams = store.replace_teams(&listed, now)?;
                self.known = None;
                let favorites = match &read.favorites {
                    Some(favorites) => store.replace_favorites(favorites, now)?,
                    None => Applied::Ignored,
                };
                if std::mem::take(&mut self.read_again) {
                    self.read_now();
                }
                Ok(Ok(teams != Applied::Ignored || favorites != Applied::Ignored))
            }
            Err(error) => {
                self.retry_at = Some(Instant::now() + self.retry);
                Ok(Err(error))
            }
        }
    }
}

/// Puts into `teams` each channel just made that they don't name, in its team's list by name after the general one;
/// one a read names now, or kept long enough, is let go.
fn keep_created(created: &mut Vec<(Created, Instant)>, teams: &mut [Team]) {
    let now = Instant::now();
    created.retain(|(made, until)| {
        let Some(team) = teams.iter_mut().find(|team| team.id == made.team) else {
            return *until > now;
        };
        if team.channels.iter().any(|channel| channel.id == made.channel.id) {
            return false;
        }
        if *until <= now {
            return false;
        }
        insert_channel(made, team);
        true
    });
}

/// Puts `change` into the stored list `teams` and keeps it against reads that don't show it yet. Only this change goes
/// in: the stored list holds the earlier ones already, so it would show them and retire their guards before any read
/// did (only a read's list retires a change, [`keep_changes`]).
fn note_into(changes: &mut Vec<(TeamChange, Instant)>, teams: &mut Vec<Team>, change: TeamChange) {
    change.apply(teams);
    changes.push((change, Instant::now() + CREATED_KEPT));
}

/// Puts a channel just made into the stored list `teams` and keeps it against reads that don't name it yet; only this
/// one goes in, as in [`note_into`].
fn created_into(created: &mut Vec<(Created, Instant)>, teams: &mut [Team], made: Created) {
    insert_created(&made, teams);
    created.push((made, Instant::now() + CREATED_KEPT));
}

/// Puts a channel just made into its team's list in `teams`, unless the list names it already.
fn insert_created(made: &Created, teams: &mut [Team]) {
    if let Some(team) = teams.iter_mut().find(|team| team.id == made.team)
        && !team.channels.iter().any(|channel| channel.id == made.channel.id)
    {
        insert_channel(made, team);
    }
}

/// Puts a channel just made into `team`'s list by name, after the general one.
fn insert_channel(made: &Created, team: &mut Team) {
    let name = made.channel.name.to_lowercase();
    let at = team
        .channels
        .iter()
        .position(|channel| !channel.general && channel.name.to_lowercase() > name)
        .unwrap_or(team.channels.len());
    team.channels.insert(at, made.channel.clone());
}

/// The stored teams (`None` before the first read settled them); nothing is read.
pub(crate) fn stored_teams(store: &Store) -> Result<Option<Vec<Team>>, StoreError> {
    match store.teams_read_at()? {
        Some(_) => store.teams().map(Some),
        None => Ok(None),
    }
}

/// What the chat list needs besides the conversations, as stored ([`ChatListing`]); nothing is read.
pub(crate) fn stored_listing(store: &Store) -> Result<ChatListing, StoreError> {
    Ok(ChatListing {
        favorites: store.favorites()?,
        empty: store.empty_conversations()?,
    })
}

/// CSA's teams read, as the store keeps its teams and favourites (chats and channels).
async fn read<C: Credentials>(client: &Client<HttpTransport<C>>) -> Result<TeamsRead, TeamsReadError> {
    let answer = client.read(&CsaTeams).await?;
    let read = CsaTeams::read_answer(answer.body())?;
    let teams = read
        .teams
        .into_iter()
        .map(|team| Team {
            id: team.id,
            name: team.name,
            archived: team.archived,
            group_id: team.group_id,
            mailbox: team.mailbox,
            owner: team.owner,
            can_add_channel: team.can_add_channel,
            can_add_private_channel: team.can_add_private_channel,
            about: TeamAbout {
                tenant: team.about.tenant,
                description: team.about.description,
                public: team.about.public,
                people: team.about.people,
                guest: team.about.guest,
                dynamic_membership: team.about.dynamic_membership,
                tenant_wide: team.about.tenant_wide,
                channel_only: team.about.channel_only,
                hidden: team.about.hidden,
                picture_etag: team.about.picture_etag,
                site: team.about.site,
            },
            channels: team
                .channels
                .into_iter()
                .map(|channel| TeamChannel {
                    id: channel.id,
                    name: channel.name,
                    general: channel.general,
                    kind: match channel.kind {
                        ChannelType::Private => ChannelKind::Private,
                        ChannelType::Shared => ChannelKind::Shared,
                        ChannelType::Standard | ChannelType::Unknown => ChannelKind::Standard,
                    },
                    about: teams_store::ChannelAbout {
                        description: channel.description,
                        archived: channel.archived,
                        owner: channel.owner,
                        can_edit: channel.can_edit,
                        can_delete: channel.can_delete,
                        group: channel.group,
                    },
                    shown: channel.shown,
                })
                .collect(),
        })
        .collect();
    Ok(TeamsRead {
        teams,
        favorites: read.favorites,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use teams_store::{ChannelKind, Team, TeamChannel};

    use super::{created_into, keep_created, note_into};
    use crate::channels::Created;
    use crate::team_admin::{TeamChange, keep_changes};

    fn team(id: &str) -> Team {
        Team {
            id: id.to_owned(),
            name: id.to_owned(),
            channels: vec![channel(id, "General", true)],
            ..Team::default()
        }
    }

    fn channel(id: &str, name: &str, general: bool) -> TeamChannel {
        TeamChannel {
            id: id.to_owned(),
            name: name.to_owned(),
            general,
            kind: ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }
    }

    #[test]
    fn a_second_change_noted_before_a_read_shows_the_first_keeps_both() {
        let stale = vec![team("19:a@thread.tacv2"), team("19:b@thread.tacv2")];
        let mut stored = stale.clone();
        let mut changes = Vec::new();
        let hidden = TeamChange::Hidden {
            team: "19:a@thread.tacv2".to_owned(),
            hidden: true,
        };
        let archived = TeamChange::Archived {
            team: "19:b@thread.tacv2".to_owned(),
            archived: true,
        };
        note_into(&mut changes, &mut stored, hidden);
        note_into(&mut changes, &mut stored, archived);
        assert_eq!(changes.len(), 2, "the stored list retires nothing");
        // the read that ran meanwhile answers with the list from before both
        let mut listed = stale;
        keep_changes(&mut changes, &mut listed);
        assert!(listed[0].about.hidden && listed[1].archived);
        assert_eq!(listed, stored);
    }

    #[test]
    fn a_second_channel_made_before_a_read_names_the_first_keeps_both() {
        let stale = vec![team("19:a@thread.tacv2")];
        let mut stored = stale.clone();
        let mut created = Vec::new();
        for (id, name) in [("19:one@thread.tacv2", "One"), ("19:two@thread.tacv2", "Two")] {
            let made = Created {
                team: "19:a@thread.tacv2".to_owned(),
                channel: channel(id, name, false),
            };
            created_into(&mut created, &mut stored, made);
        }
        assert_eq!(created.len(), 2, "the stored list retires nothing");
        let mut listed = stale;
        keep_created(&mut created, &mut listed);
        assert_eq!(listed[0].channels.len(), 3);
        assert_eq!(listed, stored);
    }

    /// No token: the client below has no CSA base, so its reads fail at once, sending nothing.
    struct NoTokens;

    impl teams_proto::Credentials for NoTokens {
        fn bearer(
            &self,
            _: &str,
        ) -> impl Future<Output = Result<teams_auth::secrets::Secret, teams_auth::token::TokenError>> + Send {
            std::future::ready(Ok(teams_auth::secrets::Secret::new("synthetic-teams-token")))
        }

        fn bearer_rejected(
            &self,
            audience: &str,
            _: &teams_auth::secrets::Secret,
        ) -> impl Future<Output = Result<teams_auth::secrets::Secret, teams_auth::token::TokenError>> + Send {
            self.bearer(audience)
        }
    }

    fn teams_without_csa() -> super::Teams<NoTokens> {
        let transport = teams_proto::HttpTransport::with_mock_origin(
            std::sync::Arc::new(NoTokens),
            "https://teams.microsoft.com/api/chatsvc/amer",
            "http://127.0.0.1:9",
        )
        .expect("a transport");
        super::Teams::new(
            std::sync::Arc::new(teams_proto::Client::new(transport)),
            std::time::Duration::from_secs(60),
        )
    }

    #[tokio::test]
    async fn a_team_thread_the_stored_teams_don_t_name_reads_them_again_once_a_run() {
        let dir = tempfile::tempdir().expect("a directory");
        let mut store = teams_store::Store::open(&dir.path().join("teams.sqlite")).expect("a store");
        let old = teams_auth::secrets::Secret::new("synthetic-old-token");
        assert!(
            teams_proto::Credentials::bearer_rejected(&NoTokens, "csa", &old)
                .await
                .is_ok()
        );
        let mut teams = teams_without_csa();
        teams.note_conversation(&store, "19:new@thread.tacv2").expect("noted");
        assert!(!teams.busy(), "before the first read nothing counts");

        store
            .replace_teams(&[team("19:a@thread.tacv2")], super::now_ms())
            .expect("stored");
        teams.note_conversation(&store, "19:a@thread.tacv2").expect("noted");
        assert!(!teams.busy(), "a stored team's thread reads nothing");
        teams.note_conversation(&store, "19:new@thread.tacv2").expect("noted");
        assert!(teams.busy(), "one no stored team names reads the teams");
        teams.note_activity();
        assert!(teams.read_again, "a read runs: another follows it");

        let read = teams.next().await.expect("a read ran");
        assert!(read.is_err(), "no CSA base");
        assert!(matches!(teams.store(&mut store, read), Ok(Err(_))));
        teams.note_conversation(&store, "19:new@thread.tacv2").expect("noted");
        assert!(!teams.busy(), "once a run");

        teams.read_again = true;
        let listed = super::TeamsRead {
            teams: vec![team("19:a@thread.tacv2")],
            favorites: Some(vec!["19:a@thread.tacv2".to_owned()]),
        };
        assert!(matches!(teams.store(&mut store, Ok(listed)), Ok(Ok(true))));
        assert!(teams.busy(), "the read owed starts");
        let _ = teams.next().await;
        assert!(!teams.note_change(&mut store, None).expect("noted"), "nothing to store");
        assert!(teams.busy(), "a write with no change reads the teams again");
        assert!(matches!(teams.next().await, Some(Err(_))));
    }
}
