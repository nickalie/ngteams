//! Which account and which chat a request names.
//!
//! - **An account** (`--account`): by its UPN (any case), its MRI or its index in the app's accounts (from 0, as
//!   `status` and `accounts` number them); without one, the account the window shows (the first one while it shows
//!   none). Anything else is [`ErrorCode::UnknownAccount`]; an account whose engine stopped (shut down, signed out) is
//!   [`ErrorCode::Stopped`].
//! - **A chat**: by its id (any stored conversation's), or by its title as the chat list names it
//!   ([`chat_list::chat_name`]: the topic, otherwise the other members' names; a 1:1 chat the other person's), in any
//!   case. A title several chats have is [`ErrorCode::AmbiguousChat`] with each one's id and title; none is
//!   [`ErrorCode::UnknownChat`], which says when the name is a channel's.
//! - **A channel** ([`channel_id`]): by its id, by `<team> > <channel>` (as `teams` titles it) or `<team>/<channel>`, or
//!   by its name alone when only one channel has it, in any case. [`conversation`] takes a chat or a channel, for the
//!   reads that serve both.

use std::sync::Arc;

use gpui::{App, Entity};
use teams_ctl::protocol::{ChatRef, CtlError, ErrorCode};
use teams_store::Conversation;
use teams_store::ConversationKind;
use teams_ui::chat_list::{self, ALL_CHATS};
use teams_ui::{Account, Chats, Core, Engine, Tray};

use crate::app::Running;

/// The account a request addresses, as its reads need it.
pub struct Addressed {
    pub upn: String,
    pub mri: String,
    pub core: Arc<dyn Core>,
    /// The badge's unread counts of the account (none while the window holds no tile of it).
    pub tray: Option<Entity<Tray>>,
    /// The account's chats as the engine told them: its outbox rows as last told among them.
    pub chats: Entity<Chats>,
}

/// The account `wanted` names, or the one the window shows without it (see the module docs).
///
/// # Errors
/// [`ErrorCode::UnknownAccount`] when no account is the one named (or there is none), [`ErrorCode::Stopped`] when its
/// engine stopped.
pub fn account(running: &Running, wanted: Option<&str>, cx: &App) -> Result<Addressed, CtlError> {
    let (mri, upn, bridge) = known(running, wanted, cx)?;
    let bridge = bridge.read(cx);
    let status = bridge.models().status.read(cx);
    if !bridge.running() || status.engine == Engine::Stopped || matches!(status.account, Account::SignedOut { .. }) {
        return Err(stopped(upn));
    }
    let tray = running
        .desktop()
        .read(cx)
        .accounts()
        .read(cx)
        .tile(mri)
        .map(|tile| tile.tray.clone());
    Ok(Addressed {
        upn: upn.clone(),
        mri: mri.clone(),
        core: bridge.core(),
        tray,
        chats: bridge.models().chats.clone(),
    })
}

/// The account `wanted` names, or the one the window shows without it, as `(MRI, UPN, bridge)`, whether or not its
/// engine runs (the window shows a signed-out account too).
///
/// # Errors
/// [`ErrorCode::UnknownAccount`] when no account is the one named (or there is none).
pub fn known<'a>(
    running: &'a Running,
    wanted: Option<&str>,
    cx: &App,
) -> Result<(&'a String, &'a String, &'a Entity<teams_ui::Bridge>), CtlError> {
    let bridges = running.bridges();
    let index = if let Some(wanted) = wanted.map(str::trim) {
        named(bridges, wanted)
    } else {
        let rail = running.desktop().read(cx).accounts().read(cx);
        rail.active_key()
            .and_then(|shown| bridges.iter().position(|(mri, _, _)| mri == shown))
            .or_else(|| (!bridges.is_empty()).then_some(0))
    };
    let Some((mri, upn, bridge)) = index.and_then(|index| bridges.get(index)) else {
        return Err(CtlError::new(
            ErrorCode::UnknownAccount,
            match wanted {
                Some(wanted) => format!("no account is {wanted}: `ngteamsctl accounts` lists them"),
                None => "the app runs no account".to_owned(),
            },
        ));
    };
    Ok((mri, upn, bridge))
}

/// The index of the account `wanted` names: its MRI, its name in any case (its UPN; an own server's account is
/// `<server>/<UPN>`, or its UPN alone while no other account has it), or its index.
fn named(bridges: &[(String, String, Entity<teams_ui::Bridge>)], wanted: &str) -> Option<usize> {
    bridges
        .iter()
        .position(|(mri, upn, _)| mri == wanted || upn.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            let mut own = bridges.iter().enumerate().filter(|(_, (_, name, _))| {
                name.rsplit_once('/')
                    .is_some_and(|(_, upn)| upn.eq_ignore_ascii_case(wanted))
            });
            match (own.next(), own.next()) {
                (Some((index, _)), None) => Some(index),
                _ => None,
            }
        })
        .or_else(|| wanted.parse::<usize>().ok().filter(|index| *index < bridges.len()))
}

/// The error of an account whose engine stopped.
fn stopped(upn: &str) -> CtlError {
    CtlError::new(
        ErrorCode::Stopped,
        format!("{upn}'s engine stopped: sign it in again or start the app again"),
    )
}

/// The error an engine's request failed with: [`ErrorCode::Stopped`] once it stopped, otherwise
/// [`ErrorCode::Engine`] with its message.
#[must_use]
pub fn engine_error(error: &teams_core::EngineError) -> CtlError {
    match error {
        teams_core::EngineError::Stopped => CtlError::new(ErrorCode::Stopped, "the account's engine stopped"),
        other => CtlError::new(ErrorCode::Engine, other.to_string()),
    }
}

/// The account's stored conversations the chat list lists, the latest activity first.
///
/// # Errors
/// The engine's ([`engine_error`]).
pub async fn listed(core: &dyn Core) -> Result<Vec<Conversation>, CtlError> {
    let conversations = core
        .conversations(ALL_CHATS)
        .await
        .map_err(|error| engine_error(&error))?;
    Ok(conversations.into_iter().filter(chat_list::listed).collect())
}

/// The chat `wanted` names (see the module docs).
///
/// # Errors
/// [`ErrorCode::UnknownChat`], [`ErrorCode::AmbiguousChat`] with the candidates, or the engine's.
pub async fn chat(core: &dyn Core, wanted: &str) -> Result<Conversation, CtlError> {
    match find_chat(core, wanted).await? {
        Some(found) => Ok(found),
        None => Err(not_a_chat(core, wanted).await),
    }
}

/// The chat `wanted` names, `None` when no chat has the name.
async fn find_chat(core: &dyn Core, wanted: &str) -> Result<Option<Conversation>, CtlError> {
    let conversations = core
        .conversations(ALL_CHATS)
        .await
        .map_err(|error| engine_error(&error))?;
    let wanted = wanted.trim();
    if let Some(found) = conversations.iter().find(|conversation| conversation.id == wanted) {
        return Ok(Some(found.clone()));
    }
    let mut found: Vec<(Conversation, String)> = Vec::new();
    for conversation in conversations.into_iter().filter(chat_list::listed) {
        let title = chat_list::chat_name(core, &conversation).await.to_string();
        if title.trim().to_lowercase() == wanted.to_lowercase() {
            found.push((conversation, title));
        }
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.swap_remove(0).0)),
        many => Err(CtlError {
            code: ErrorCode::AmbiguousChat,
            message: format!("{many} chats are called {wanted}: name one by its id"),
            candidates: found
                .into_iter()
                .map(|(conversation, title)| ChatRef {
                    id: conversation.id,
                    title,
                })
                .collect(),
        }),
    }
}

/// The error of a name that no chat has: it points to `teams` when a listed channel has the name.
async fn not_a_chat(core: &dyn Core, wanted: &str) -> CtlError {
    if matches!(find_channel(core, wanted).await, Ok(Channel::Listed(_))) {
        return CtlError::new(
            ErrorCode::UnknownChat,
            format!(
                "{wanted} is a channel, not a chat: `ngteamsctl teams` lists the channels with their ids, and \
                 `ngteamsctl messages` reads a channel"
            ),
        );
    }
    no_chat(wanted)
}

fn no_chat(wanted: &str) -> CtlError {
    CtlError::new(
        ErrorCode::UnknownChat,
        format!("no chat is {wanted}: `ngteamsctl chats` lists the chats, `ngteamsctl teams` the channels"),
    )
}

/// A conversation that a read names: a chat or a team's channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    /// A team's channel: its rows tell a post from a reply, and a read syncs it first.
    pub channel: bool,
}

/// The chat or the channel `wanted` names: a stored conversation by its id, a chat by its title, else a channel
/// ([`channel_id`]).
///
/// # Errors
/// As [`chat`] when no channel has the name either; [`ErrorCode::AmbiguousChat`] for a channel name that several
/// channels share; the engine's.
pub async fn conversation(core: &dyn Core, wanted: &str) -> Result<Target, CtlError> {
    if let Some(found) = find_chat(core, wanted).await? {
        return Ok(Target {
            channel: found.kind == ConversationKind::Channel,
            id: found.id,
        });
    }
    match find_channel(core, wanted).await {
        Ok(Channel::Listed(id) | Channel::Bare(id)) => Ok(Target { id, channel: true }),
        Err(error) if error.code == ErrorCode::UnknownChat => Err(no_chat(wanted)),
        Err(error) => Err(error),
    }
}

/// The id of the channel `wanted` names among the account's teams (see the module docs). Before the teams were ever
/// read, a thread's id (`19:…`) is taken as it is.
///
/// # Errors
/// [`ErrorCode::UnknownChat`], [`ErrorCode::AmbiguousChat`] with the candidates, or the engine's.
pub async fn channel_id(core: &dyn Core, wanted: &str) -> Result<String, CtlError> {
    match find_channel(core, wanted).await? {
        Channel::Listed(id) | Channel::Bare(id) => Ok(id),
    }
}

/// A channel [`find_channel`] found.
enum Channel {
    /// One of the stored teams' channels.
    Listed(String),
    /// A thread's id taken as it is, before the teams were ever read.
    Bare(String),
}

async fn find_channel(core: &dyn Core, wanted: &str) -> Result<Channel, CtlError> {
    let wanted = wanted.trim();
    let Some(teams) = core.teams().await.map_err(|error| engine_error(&error))? else {
        if wanted.starts_with("19:") {
            return Ok(Channel::Bare(wanted.to_owned()));
        }
        return Err(CtlError::new(
            ErrorCode::UnknownChat,
            format!("the teams aren't read yet: name the channel {wanted} by its id"),
        ));
    };
    let mut found: Vec<ChatRef> = Vec::new();
    for team in &teams {
        for channel in &team.channels {
            if channel.id == wanted {
                return Ok(Channel::Listed(channel.id.clone()));
            }
            if names(wanted, &team.name, &channel.name) {
                found.push(ChatRef {
                    id: channel.id.clone(),
                    title: channel_title(&team.name, &channel.name),
                });
            }
        }
    }
    match found.len() {
        0 => Err(CtlError::new(
            ErrorCode::UnknownChat,
            format!(
                "no channel is {wanted}: `ngteamsctl teams` lists them; name one by its id, <team> > <channel> or its \
                 name"
            ),
        )),
        1 => Ok(Channel::Listed(found.swap_remove(0).id)),
        many => Err(CtlError {
            code: ErrorCode::AmbiguousChat,
            message: format!("{many} channels are called {wanted}: name one by <team> > <channel> or its id"),
            candidates: found,
        }),
    }
}

/// A channel's title as `teams` gives it: `<team> > <channel>`.
#[must_use]
pub fn channel_title(team: &str, channel: &str) -> String {
    format!("{} > {}", team.trim(), channel.trim())
}

/// Whether `wanted` names the channel `channel` of the team `team`: its name, `<team> > <channel>` (the spaces
/// around `>` optional) or `<team>/<channel>`, in any case.
fn names(wanted: &str, team: &str, channel: &str) -> bool {
    let same = |one: &str, other: &str| one.trim().eq_ignore_ascii_case(other.trim());
    same(wanted, channel)
        || same(wanted, &format!("{team}/{channel}"))
        || wanted
            .match_indices('>')
            .any(|(at, _)| same(&wanted[..at], team) && same(&wanted[at + 1..], channel))
}
