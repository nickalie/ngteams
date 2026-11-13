//! A team's new channel (`docs/protocol/channels.md`, "Creating a channel"): made when the "Create a channel" dialog
//! asks, the people a private channel starts with added, its thread waited for, and the people on a team a name finds.
//!
//! - **Writes** ([`Channels::create`], [`Channels::add_members`]): sealed `teams-proto` writes through the outbox's
//!   one-attempt kind ([`Outbox::try_channel`]), as the calendar's: no row, nothing sent again by a later run; one more attempt only after a 401 or a short throttle
//!   ([`CallRetries`]), never after an unknown outcome (a channel made twice is worse than a failed save). A creation
//!   that wasn't answered with a 2xx is [`EngineError::ChannelNotCreated`], saying which of Teams' problems it was
//!   ([`ChannelProblem`]).
//! - **The list** ([`Created`]): a channel made is stored at once, through the engine's loop, so the sidebar shows it and
//!   the shell can open it before CSA's list names it; [`crate::teams::Teams`] keeps it for a while against a read that
//!   doesn't name it yet, and reads the list again.
//! - **A private channel** comes as a group first (`OID:<id>@<tenant>`) and its thread later ([`Channels::thread`]: its
//!   status read every [`THREAD_POLL`], up to [`THREAD_WAIT`]). Its members go in by the group, so the dialog's member
//!   step doesn't wait for the thread.

use std::sync::Arc;
use std::time::Duration;

use teams_proto::Mri;
use teams_proto::{
    AddChannelMembers, ChannelCreated, ChannelCreationStatus, Client, CreateChannel, Credentials, HttpError,
    HttpTransport, NewChannel, NewChannelKind, Request, ShowChannelByDefault, TeamPeopleSearch, TeamPerson, Transport,
    channel_error_code,
};
use teams_store::{ChannelKind, TeamChannel};
use tokio::task::JoinSet;

use crate::engine::{EngineError, Responder};
use crate::outbox::{CallRetries, CallTried, Outbox};

/// How often a private channel's status is read while it has no thread.
const THREAD_POLL: Duration = Duration::from_secs(4);

/// How long a private channel's thread is waited for.
const THREAD_WAIT: Duration = Duration::from_secs(120);

/// What the dialog asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelDraft {
    /// The channel as it goes to the service.
    pub channel: NewChannel,
    /// Show it in everyone's channel list (an owner's choice for a standard channel).
    pub show_by_default: bool,
}

/// A channel made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelMade {
    /// Its thread; `None` for a private channel that hasn't one yet.
    pub thread: Option<String>,
    /// A private channel's group (`OID:<id>@<tenant>`).
    pub group: Option<String>,
    /// It couldn't be shown in everyone's channel list, as asked.
    pub not_shown_by_default: bool,
}

/// Why a channel wasn't made, as Teams tells them apart (its error codes, `docs/protocol/channels.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelProblem {
    /// `ChannelNameAlreadyExist`, or a 409 naming nothing.
    NameTaken,
    /// `MaxPrivateChannelLimitExceeded`.
    TooManyPrivateChannels,
    /// `MaxChannelLimitExceeded`, `Max1kChannelLimitExceeded`.
    TooManyChannels,
    /// `UnableToFetchTeamOwner`.
    NoTeamOwner,
    /// `NoPermissionToCreateChannel`, `UserNotAllowedToCreateChannel`.
    NoPermission,
    /// `CannotCreatePrivateChannelDueToUserPolicy`.
    PrivateNotAllowed,
    /// `OperationNotAllowedOnArchivedTeam`.
    TeamArchived,
    /// `InvalidUsersInCreateChannelRequest`.
    InvalidUsers,
    /// `ThreadDescriptionLimitExceeded`.
    DescriptionTooLong,
    /// Anything else: not sent, denied, no answer, an answer that names nothing known. Worth trying again.
    Other,
}

impl ChannelProblem {
    /// The problem an error `code` names; `status` for an answer naming none.
    #[must_use]
    pub fn of(code: Option<&str>, status: u16) -> Self {
        match code {
            Some("ChannelNameAlreadyExist") => Self::NameTaken,
            Some("MaxPrivateChannelLimitExceeded") => Self::TooManyPrivateChannels,
            Some("MaxChannelLimitExceeded" | "Max1kChannelLimitExceeded") => Self::TooManyChannels,
            Some("UnableToFetchTeamOwner") => Self::NoTeamOwner,
            Some("NoPermissionToCreateChannel" | "UserNotAllowedToCreateChannel") => Self::NoPermission,
            Some("CannotCreatePrivateChannelDueToUserPolicy") => Self::PrivateNotAllowed,
            Some("OperationNotAllowedOnArchivedTeam") => Self::TeamArchived,
            Some("InvalidUsersInCreateChannelRequest") => Self::InvalidUsers,
            Some("ThreadDescriptionLimitExceeded") => Self::DescriptionTooLong,
            None if status == 409 => Self::NameTaken,
            _ => Self::Other,
        }
    }
}

/// A channel that came, for the engine to store and tell: its team, and the channel's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Created {
    pub(crate) team: String,
    pub(crate) channel: TeamChannel,
}

/// A task's end: what to store, and the reply to send once it is stored.
pub(crate) struct Finished {
    pub(crate) created: Option<Created>,
    /// What a team's write changed in the list.
    pub(crate) team: Option<Box<crate::team_admin::TeamChange>>,
    /// Whether the list is to be read again.
    pub(crate) reread: bool,
    pub(crate) reply: Box<dyn FnOnce() + Send>,
}

/// The channel writes and reads under way.
pub(crate) struct Channels<C, T> {
    pub(crate) client: Arc<Client<HttpTransport<C>>>,
    pub(crate) outbox: Arc<Outbox<T>>,
    pub(crate) running: JoinSet<Finished>,
    pub(crate) app_definitions: Arc<crate::team_apps::AppDefinitionsCache>,
}

impl<C, T> Channels<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + 'static,
{
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, outbox: Arc<Outbox<T>>) -> Self {
        Self {
            client,
            outbox,
            running: JoinSet::new(),
            app_definitions: Arc::default(),
        }
    }

    /// Whether something runs.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// What ended next; never while nothing runs ([`Self::busy`]).
    pub(crate) async fn next(&mut self) -> Option<Finished> {
        self.running.join_next().await?.ok()
    }

    /// Makes `draft`'s channel and answers `reply`, once the channel is stored when it has its thread.
    pub(crate) fn create(&mut self, draft: ChannelDraft, reply: Responder<ChannelMade>) {
        let outbox = Arc::clone(&self.outbox);
        self.running.spawn_local(async move {
            let name = draft.channel.name.clone();
            let team = draft.channel.team.clone();
            let kind = draft.channel.kind;
            let made = make(&outbox, &draft).await;
            let created = made.as_ref().ok().and_then(|made| {
                let thread = made.thread.clone()?;
                Some(Created {
                    team: team.clone(),
                    channel: row(thread, name.clone(), kind),
                })
            });
            Finished {
                created,
                team: None,
                reread: false,
                reply: Box::new(move || {
                    let _ = reply.send(made);
                }),
            }
        });
    }

    /// Adds `members` to the private channel `channel_group` of `team_group`; answers `reply` with those who weren't.
    pub(crate) fn add_members(
        &mut self,
        team_group: String,
        channel_group: String,
        members: Vec<String>,
        reply: Responder<Vec<String>>,
    ) {
        let outbox = Arc::clone(&self.outbox);
        self.running.spawn_local(async move {
            let everyone: Vec<Mri> = members.iter().map(Mri::new).collect();
            let answer = match AddChannelMembers::new(team_group, channel_group, &everyone) {
                Some(write) => match send(&outbox, &write).await {
                    Ok(body) => Ok(write.failed(&body).iter().map(ToString::to_string).collect()),
                    Err(_) => Ok(members),
                },
                None => Ok(members),
            };
            Finished {
                created: None,
                team: None,
                reread: false,
                reply: Box::new(move || {
                    let _ = reply.send(answer);
                }),
            }
        });
    }

    /// Waits for the private channel `channel_group`'s thread (its status read every [`THREAD_POLL`], up to
    /// [`THREAD_WAIT`]) and answers `reply` with it, once it is stored; `None` when it didn't come in time.
    pub(crate) fn thread(
        &mut self,
        team: String,
        team_group: String,
        channel_group: String,
        name: String,
        reply: Responder<Option<String>>,
    ) {
        let client = Arc::clone(&self.client);
        self.running.spawn_local(async move {
            let status = ChannelCreationStatus {
                team_group,
                channel_group,
            };
            let thread = wait_for_thread(&client, &status).await;
            let created = thread.clone().map(|thread| Created {
                team,
                channel: row(thread, name, NewChannelKind::Private),
            });
            Finished {
                created,
                team: None,
                reread: false,
                reply: Box::new(move || {
                    let _ = reply.send(Ok(thread));
                }),
            }
        });
    }

    /// The people on `team` that `keyword` finds, read now; nobody when the read fails.
    pub(crate) fn people(&self, team: String, keyword: String, reply: Responder<Vec<TeamPerson>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let search = TeamPeopleSearch { team, keyword };
            let found = match client.transport().send(&search).await {
                Ok(answer) if (200..300).contains(&answer.status()) => TeamPeopleSearch::read_answer(answer.body()),
                Ok(answer) => {
                    log::info!("a team's people search answered {}", answer.status());
                    Vec::new()
                }
                Err(error) => {
                    log::info!("a team's people search failed: {error}");
                    Vec::new()
                }
            };
            let _ = reply.send(Ok(found));
        });
    }
}

/// A new channel's row in the list.
fn row(id: String, name: String, kind: NewChannelKind) -> TeamChannel {
    TeamChannel {
        id,
        name,
        general: false,
        kind: match kind {
            NewChannelKind::Standard => ChannelKind::Standard,
            NewChannelKind::Private => ChannelKind::Private,
        },
        about: teams_store::ChannelAbout::default(),
        shown: true,
    }
}

/// The creation, then what the draft asks of the channel made.
async fn make<T>(outbox: &Outbox<T>, draft: &ChannelDraft) -> Result<ChannelMade, EngineError>
where
    T: Transport<Error = HttpError>,
{
    let body = send(outbox, &CreateChannel::new(draft.channel.clone())).await?;
    let ChannelCreated { thread, group } = CreateChannel::read_answer(&body).unwrap_or(ChannelCreated {
        thread: None,
        group: None,
    });
    let mut not_shown_by_default = false;
    if draft.show_by_default
        && let Some(thread) = &thread
    {
        not_shown_by_default = send(outbox, &ShowChannelByDefault::new(thread.clone())).await.is_err();
    }
    Ok(ChannelMade {
        thread,
        group,
        not_shown_by_default,
    })
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
                let code = channel_error_code(answer.body());
                log::info!(
                    "a channel's write was refused: {} {}",
                    answer.status(),
                    code.as_deref().unwrap_or("no code")
                );
                Err(not_created(ChannelProblem::of(code.as_deref(), answer.status())))
            }
            CallTried::Denied(reason) => {
                log::info!("a channel's write was refused: {reason}");
                Err(not_created(ChannelProblem::Other))
            }
            CallTried::NotSent(why) | CallTried::Uncertain(why) => {
                log::info!("a channel's write didn't go: {why}");
                Err(not_created(ChannelProblem::Other))
            }
            CallTried::Throttled(_) => Err(not_created(ChannelProblem::Other)),
        };
    }
}

fn not_created(why: ChannelProblem) -> EngineError {
    EngineError::ChannelNotCreated { why }
}

/// Reads `status` until it names the thread, or [`THREAD_WAIT`] is over.
async fn wait_for_thread<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    status: &ChannelCreationStatus,
) -> Option<String> {
    let deadline = tokio::time::Instant::now() + THREAD_WAIT;
    loop {
        match client.read(status).await {
            Ok(answer) => {
                if let Some(thread) = ChannelCreationStatus::read_answer(answer.body()) {
                    return Some(thread);
                }
            }
            Err(error) => log::info!("a channel's status couldn't be read: {error}"),
        }
        if tokio::time::Instant::now() + THREAD_POLL > deadline {
            return None;
        }
        tokio::time::sleep(THREAD_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::ChannelProblem;

    #[test]
    fn each_code_teams_answers_is_told_apart() {
        for (code, problem) in [
            ("ChannelNameAlreadyExist", ChannelProblem::NameTaken),
            ("MaxPrivateChannelLimitExceeded", ChannelProblem::TooManyPrivateChannels),
            ("MaxChannelLimitExceeded", ChannelProblem::TooManyChannels),
            ("Max1kChannelLimitExceeded", ChannelProblem::TooManyChannels),
            ("UnableToFetchTeamOwner", ChannelProblem::NoTeamOwner),
            ("NoPermissionToCreateChannel", ChannelProblem::NoPermission),
            ("UserNotAllowedToCreateChannel", ChannelProblem::NoPermission),
            (
                "CannotCreatePrivateChannelDueToUserPolicy",
                ChannelProblem::PrivateNotAllowed,
            ),
            ("OperationNotAllowedOnArchivedTeam", ChannelProblem::TeamArchived),
            ("InvalidUsersInCreateChannelRequest", ChannelProblem::InvalidUsers),
            ("ThreadDescriptionLimitExceeded", ChannelProblem::DescriptionTooLong),
            ("SomethingMadeUp", ChannelProblem::Other),
        ] {
            assert_eq!(ChannelProblem::of(Some(code), 400), problem, "{code}");
        }
        assert_eq!(
            ChannelProblem::of(None, 409),
            ChannelProblem::NameTaken,
            "a conflict without a code"
        );
        assert_eq!(ChannelProblem::of(None, 500), ChannelProblem::Other);
    }
}
