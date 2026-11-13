//! A meeting that started, on the desktop: a banner with Join, as Teams shows one (its "meeting started
//! notification", `docs/protocol/calling.md`, "When a meeting starts").
//!
//! **What shows one.** A call's start (`Event/Call` read as [`ChatEvent::CallStarted`]) newly stored in a chat, someone
//! else's (its starter, else its sender, isn't the account), delivered over the push channel (or brought by a catch-up
//! or a replay within [`FRESH_MS`] of its arrival):
//! - a group chat's meeting (Meet now, `huddleGroupCall`): "<name> started a meeting", titled with the chat's name;
//! - a meeting's chat's or a channel's, only while the account's calendar holds that meeting from [`EARLY_MS`] before
//!   its start to [`LATE_MS`] after its end, as Teams checks the calendar: "<name> started the meeting", titled with
//!   the meeting's subject. Teams shows none for a channel's ad-hoc Meet now or a meeting not on the calendar.
//!
//! A group chat's plain group call and a 1:1 call ring instead ([`crate::call_alert`]). None shows while the "Meeting
//! started notification" switch is off ([`Settings::meeting_started`]), notifications are off, the account's status
//! is Do not disturb, a call's Mute notifications holds them back, the channel is muted, or the account is in that
//! call already; nor where its Join can't go (a build that shows no calls, the read-only mode). Each call shows one
//! once, and none for a call whose end came first (a catch-up brings a page's messages in no set order).
//!
//! **Its actions.** Join goes in as the chat's or the calendar's Join does ([`MeetingJoin`]); its body opens the chat
//! or the channel. It closes once activated, once its call ends (its `Event/Call` end, however late it came, or an
//! end naming no call), and once the account is in that call; another call's end in the chat leaves it. One whose
//! call ended while it was being decided or shown is withdrawn, and none shows for a start whose end the store holds
//! already (one that came before a restart).
//!
//! **A group chat's running call.** Whether a group chat's call runs is read from its stored call events, which the
//! engine joins by (`Core::call_runs`, `teams_proto::running_call`), not from the order the events came in: at start
//! for every stored group chat, and again after each start or end that isn't older history. [`Calls`] is told
//! ([`Calls::posted_running`]) and the chat's header offers Join while it runs, as Teams' does. Only a read after an
//! end clears it: a start whose read says otherwise leaves what an invitation named.
//!
//! What it does is traced (`meeting-notification shown`/`closed`/`joined`/`opened`, with the chat and the kind), never
//! a name or a title.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDateTime;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Subscription};
use teams_core::CalendarEvent;
use teams_core::Source;
use teams_proto::chat::{ChatEvent, chat_event, meeting_times};
use teams_proto::{MeetingInfo, is_group_chat_id, is_meeting_chat_id, is_team_thread_id};
use teams_store::Message;

use crate::bridge::Core;
use crate::calls::Calls;
use crate::models::{FeedEvent, Models};
use crate::notify::{self, DEFAULT_ACTION, JOIN_ACTION, Notice, NoticeKind, NotificationServer, SOMEONE};
use crate::people::Shown;
use crate::settings::{ManualStatus, Notifications, Settings, SettingsModel};
use crate::sounds::SoundEvent;
use crate::trace::{TraceLine, ViewTrace};

/// How long after it arrived a start a catch-up or a replay brought still shows a banner, in ms.
pub const FRESH_MS: i64 = 5 * 60 * 1000;
/// How long before a meeting's scheduled start its start shows a banner, in ms.
pub const EARLY_MS: i64 = 15 * 60 * 1000;
/// How long after a meeting's scheduled end its start still shows a banner, in ms.
pub const LATE_MS: i64 = 15 * 60 * 1000;
/// What a meeting without a subject is called.
pub const NO_TITLE: &str = "Meeting";

/// Where a call started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartedIn {
    /// A group chat: its meeting (Meet now) when `huddle`, else its group call (which rings).
    GroupChat { huddle: bool },
    /// A meeting's chat.
    MeetingChat,
    /// A team's channel.
    Channel,
}

/// A call's start or end newly stored in a chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallNews {
    /// It started: where, and who started it (its starter, else the message's sender).
    Started { place: StartedIn, starter: Option<String> },
    /// It ended.
    Ended,
}

/// What `message`, newly stored in `conversation`, tells of the chat's call: `None` when it isn't a call's start or
/// end, or the place is none a banner or a join is about (a 1:1 chat, the account's own streams).
#[must_use]
pub fn call_news(conversation: &str, message: &Message) -> Option<CallNews> {
    let place = if is_team_thread_id(conversation) {
        StartedIn::Channel
    } else if is_meeting_chat_id(conversation) {
        StartedIn::MeetingChat
    } else if is_group_chat_id(conversation) {
        StartedIn::GroupChat { huddle: false }
    } else {
        return None;
    };
    match chat_event(&message.message_type, &message.content)? {
        ChatEvent::CallStarted(start) => Some(CallNews::Started {
            place: match place {
                StartedIn::GroupChat { .. } => StartedIn::GroupChat { huddle: start.huddle },
                other => other,
            },
            starter: start.initiator.or_else(|| message.sender.clone()),
        }),
        ChatEvent::CallEnded(_) => Some(CallNews::Ended),
        _ => None,
    }
}

/// Whether a call's event brought by `source` and arrived at `arrival` (ms) tells of the call now (`now`, ms): one
/// delivered, or one a catch-up or a replay brought within [`FRESH_MS`]; never older history.
#[must_use]
pub fn fresh(source: Source, arrival: i64, now: i64) -> bool {
    match source {
        Source::Delivery => true,
        Source::CatchUp | Source::Replay => now.saturating_sub(arrival) <= FRESH_MS,
        Source::History => false,
    }
}

/// Whether a fresh start ([`fresh`]) in `place` by `starter` may show `me` a banner: someone else's meeting, not a
/// group chat's call (which rings).
#[must_use]
pub fn may_show(me: &str, place: StartedIn, starter: Option<&str>) -> bool {
    starter != Some(me) && place != StartedIn::GroupChat { huddle: false }
}

/// Whether the settings let a meeting's banner show for `me` now (`held`: a call's Mute notifications holds them).
#[must_use]
pub fn banners_on(settings: &Settings, me: &str, held: bool) -> bool {
    settings.meeting_started
        && settings.notifications != Notifications::Off
        && settings.manual_status_of(me) != ManualStatus::DoNotDisturb
        && !held
}

/// The account's calendar event of the meeting in `conversation` (a meeting's chat, or a channel) that runs about
/// `now` (local time): from [`EARLY_MS`] before its start to [`LATE_MS`] after its end. `None` when the calendar holds
/// no such meeting then.
#[must_use]
pub fn scheduled_now<'e>(
    events: &'e [CalendarEvent],
    conversation: &str,
    now: NaiveDateTime,
) -> Option<&'e CalendarEvent> {
    let (early, late) = (
        chrono::Duration::milliseconds(EARLY_MS),
        chrono::Duration::milliseconds(LATE_MS),
    );
    events.iter().find(|event| {
        let of_it = event.meeting_chat.as_deref() == Some(conversation)
            || event
                .channel_meeting
                .as_ref()
                .is_some_and(|channel| channel.channel == conversation);
        let start = crate::calendar::local(event.start);
        let end = crate::calendar::local(event.end);
        of_it
            && !event.cancelled
            && start
                .zip(end)
                .is_some_and(|(start, end)| start - early <= now && now <= end + late)
    })
}

/// What a banner's Join goes into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeetingJoin {
    /// A group chat's running meeting, joined as the chat's header joins it.
    GroupChat { chat: String },
    /// A meeting's chat's meeting: its pre-join, as the meeting chat's Join opens it.
    MeetingChat { chat: String, title: String },
    /// A channel's meeting: its pre-join with the meeting as the calendar names it, else the channel (its header's
    /// Join).
    Channel {
        channel: String,
        title: String,
        meeting: Option<MeetingInfo>,
    },
}

/// What the alert asks of the desktop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeetingAlertEvent {
    /// The banner's body: open the chat, or the channel.
    Open { conversation: String, channel: bool },
    /// The banner's Join.
    Join(MeetingJoin),
}

/// One banner shown: its chat, its call (as its start named it), and where its Join goes.
#[derive(Clone, Debug)]
struct ShownBanner {
    conversation: String,
    call: Option<String>,
    join: MeetingJoin,
}

/// A banner decided on, to show.
struct Banner {
    notice: Notice,
    join: MeetingJoin,
}

/// The start a banner is being decided for: its chat, its message, its call, and how many of the chat's ends (and of
/// its ends naming no call) had come then.
struct Pending {
    conversation: String,
    start: String,
    call: Option<String>,
    ends: Ends,
}

/// How many call ends a chat had: all of them, and those naming no call (which end whatever call ran).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ends {
    all: u64,
    bare: u64,
}

/// A read of whether a group chat's call runs, asked for: the newest one asked is the one applied.
#[derive(Default)]
struct RunningRead {
    asked: u64,
    /// Whether a read since the last one applied followed an end, so may clear the running call.
    may_clear: bool,
}

/// The started meetings' banners (see the module docs). A GPUI entity the desktop holds, one per account.
pub struct MeetingAlert {
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    calls: Entity<Calls>,
    trace: Arc<ViewTrace>,
    server: Option<Arc<dyn NotificationServer>>,
    /// The banners shown, by the server's id.
    shown: HashMap<u32, ShownBanner>,
    /// The starts a banner was decided for (their chat and message), so each shows once.
    seen: HashSet<(String, String)>,
    /// How many call ends each chat had.
    ends: HashMap<String, Ends>,
    /// The calls that ended, by their chat and call id.
    ended: HashSet<(String, String)>,
    /// The reads of the group chats' running calls.
    running: HashMap<String, RunningRead>,
    _feed: Subscription,
    _calls: Subscription,
}

impl EventEmitter<MeetingAlertEvent> for MeetingAlert {}

/// Milliseconds since the epoch, now.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

impl MeetingAlert {
    /// The alert over the models; it shows nothing until it is given a server ([`MeetingAlert::set_server`]).
    pub fn new(
        core: Arc<dyn Core>,
        models: &Models,
        settings: Entity<SettingsModel>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let feed = cx.subscribe(&models.feed, |this: &mut Self, _, event, cx| {
            if let FeedEvent::Arrived {
                conversation,
                message,
                source,
            } = event
            {
                this.arrived(conversation, message, *source, cx);
            }
        });
        let calls = cx.observe(&models.calls, |this: &mut Self, _, cx| this.close_joined(cx));
        let this = Self {
            core,
            settings,
            calls: models.calls.clone(),
            trace,
            server: None,
            shown: HashMap::new(),
            seen: HashSet::new(),
            ends: HashMap::new(),
            ended: HashSet::new(),
            running: HashMap::new(),
            _feed: feed,
            _calls: calls,
        };
        this.read_all_running(cx);
        this
    }

    /// From now on, banners go to `server`.
    pub fn set_server(&mut self, server: Arc<dyn NotificationServer>) {
        self.server = Some(server);
    }

    /// Notification `id` had `action` invoked: whether it is one of this alert's (its Join joins, its body opens the
    /// chat; either closes it).
    pub fn activated(&mut self, id: u32, action: &str, cx: &mut Context<Self>) -> bool {
        let Some(shown) = self.shown.remove(&id) else {
            return false;
        };
        let event = match action {
            JOIN_ACTION => MeetingAlertEvent::Join(shown.join.clone()),
            DEFAULT_ACTION => MeetingAlertEvent::Open {
                channel: is_team_thread_id(&shown.conversation),
                conversation: shown.conversation.clone(),
            },
            _ => {
                self.shown.insert(id, shown);
                return true;
            }
        };
        let done = if action == JOIN_ACTION { "joined" } else { "opened" };
        self.trace
            .record(&TraceLine::new("meeting-notification", done).conversation(&shown.conversation));
        self.close(id, cx);
        cx.emit(event);
        true
    }

    fn arrived(&mut self, conversation: &str, message: &Message, source: Source, cx: &mut Context<Self>) {
        let Some(news) = call_news(conversation, message) else {
            return;
        };
        if source == Source::History {
            return;
        }
        let call = meeting_times(&message.content).and_then(|times| times.call_id);
        let ended = news == CallNews::Ended;
        match news {
            CallNews::Ended => {
                let ends = self.ends.entry(conversation.to_owned()).or_default();
                ends.all += 1;
                ends.bare += u64::from(call.is_none());
                self.close_ended(conversation, call.as_deref(), cx);
                if let Some(call) = call {
                    self.ended.insert((conversation.to_owned(), call));
                }
            }
            CallNews::Started { place, starter } => {
                let pending = Pending {
                    ends: self.ends_in(conversation),
                    conversation: conversation.to_owned(),
                    start: message.id.clone(),
                    call,
                };
                let due = fresh(source, message.arrival_time, now_ms())
                    && may_show(self.core.me(), place, starter.as_deref())
                    && !self.ended_since(&pending)
                    && self.seen.insert((conversation.to_owned(), message.id.clone()));
                if due && self.on_now(cx).is_some() && !self.in_call(conversation, cx) {
                    self.decide(pending, place, starter, cx);
                }
            }
        }
        self.read_running(conversation, ended, cx);
    }

    fn ends_in(&self, conversation: &str) -> Ends {
        self.ends.get(conversation).copied().unwrap_or_default()
    }

    /// Whether the call of `pending` ended since: its call's end came, or an end naming no call; for a start naming no
    /// call, any end in its chat.
    fn ended_since(&self, pending: &Pending) -> bool {
        let now = self.ends_in(&pending.conversation);
        match &pending.call {
            Some(call) => {
                now.bare != pending.ends.bare || self.ended.contains(&(pending.conversation.clone(), call.clone()))
            }
            None => now.all != pending.ends.all,
        }
    }

    /// Reads whether each stored group chat's call runs, as the alert starts.
    fn read_all_running(&self, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let conversations = core
                .conversations(crate::chat_list::LIST_LIMIT)
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                for conversation in conversations {
                    this.read_running(&conversation.id, false, cx);
                }
            });
        })
        .detach();
    }

    /// Reads whether group chat `conversation`'s call runs and tells [`Calls`]; after an end (`ended`) the read may
    /// clear it.
    fn read_running(&mut self, conversation: &str, ended: bool, cx: &mut Context<Self>) {
        if !is_group_chat_id(conversation) || !self.calls.read(cx).shows() {
            return;
        }
        let read = self.running.entry(conversation.to_owned()).or_default();
        read.asked += 1;
        read.may_clear |= ended;
        let asked = read.asked;
        let core = Arc::clone(&self.core);
        let conversation = conversation.to_owned();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Ok(runs) = core.call_runs(conversation.clone()).await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let Some(read) = this.running.get_mut(&conversation).filter(|read| read.asked == asked) else {
                    return;
                };
                if runs || read.may_clear {
                    this.calls
                        .update(cx, |calls, cx| calls.posted_running(&conversation, runs, cx));
                }
                read.may_clear = false;
            });
        })
        .detach();
    }

    /// The server, when a banner may show now: its Join works here (a build that shows calls, not the read-only mode)
    /// and the settings let it.
    fn on_now(&self, cx: &App) -> Option<Arc<dyn NotificationServer>> {
        let joins = self.calls.read(cx).shows() && !self.core.read_only();
        let on = joins
            && banners_on(
                self.settings.read(cx).settings(),
                self.core.me(),
                notify::muted_in_call(cx),
            );
        self.server.clone().filter(|_| on)
    }

    /// Whether the account is in the call of `conversation` (or going into it) now.
    fn in_call(&self, conversation: &str, cx: &App) -> bool {
        let calls = self.calls.read(cx);
        calls.call().and_then(|call| call.chat.as_deref()) == Some(conversation)
            || calls.prejoin().and_then(|prejoin| prejoin.to_join.chat()) == Some(conversation)
    }

    /// Reads what the banner needs (the chat's name, the calendar, the channel's preferences), then shows it if it is
    /// still due; one whose call ended while it was being shown is closed again.
    fn decide(&self, pending: Pending, place: StartedIn, starter: Option<String>, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Some(banner) = banner(&*core, &pending.conversation, place, starter).await else {
                return;
            };
            // an end that came before a restart is in the store only
            let stored_end = core
                .call_ended(pending.conversation.clone(), pending.start.clone())
                .await;
            if stored_end.unwrap_or(false) {
                return;
            }
            let server = this
                .update(cx, |this, cx| {
                    this.on_now(cx)
                        .filter(|_| !this.in_call(&pending.conversation, cx) && !this.ended_since(&pending))
                })
                .ok()
                .flatten();
            let Some(server) = server else {
                return;
            };
            let Ok(id) = server.show(&banner.notice, 0).await else {
                return;
            };
            let kept = this.update(cx, |this, cx| {
                let due = !this.ended_since(&pending) && !this.in_call(&pending.conversation, cx);
                if due {
                    this.shown_banner(id, banner, pending.call.clone(), cx);
                }
                due
            });
            if !kept.unwrap_or(false) {
                server.close(id).await;
            }
        })
        .detach();
    }

    fn shown_banner(&mut self, id: u32, banner: Banner, call: Option<String>, cx: &mut Context<Self>) {
        let conversation = banner.notice.conversation.clone();
        let kind = match banner.join {
            MeetingJoin::GroupChat { .. } => "group-chat",
            MeetingJoin::MeetingChat { .. } => "meeting-chat",
            MeetingJoin::Channel { .. } => "channel",
        };
        self.trace.record(
            &TraceLine::new("meeting-notification", "shown")
                .conversation(&conversation)
                .kind(kind),
        );
        if let Some(player) = self.settings.read(cx).sound_player() {
            player.play(
                SoundEvent::Message,
                self.core.me(),
                self.calls.read(cx).call().is_some(),
            );
        }
        self.shown.insert(
            id,
            ShownBanner {
                conversation,
                call,
                join: banner.join,
            },
        );
    }

    /// Closes the banners of `conversation` whose call ended (`call`, the call its end named): that call's, or every
    /// one when the end named no call; a banner whose start named no call closes at any end.
    fn close_ended(&mut self, conversation: &str, call: Option<&str>, cx: &App) {
        let ids: Vec<u32> = self
            .shown
            .iter()
            .filter(|(_, shown)| {
                shown.conversation == conversation
                    && (call.is_none() || shown.call.is_none() || shown.call.as_deref() == call)
            })
            .map(|(id, _)| *id)
            .collect();
        self.close_ids(conversation, ids, cx);
    }

    /// Closes the banners of `conversation`: the account is in its call.
    fn close_in(&mut self, conversation: &str, cx: &App) {
        let ids: Vec<u32> = self
            .shown
            .iter()
            .filter(|(_, shown)| shown.conversation == conversation)
            .map(|(id, _)| *id)
            .collect();
        self.close_ids(conversation, ids, cx);
    }

    fn close_ids(&mut self, conversation: &str, ids: Vec<u32>, cx: &App) {
        for id in ids {
            self.shown.remove(&id);
            self.trace
                .record(&TraceLine::new("meeting-notification", "closed").conversation(conversation));
            self.close(id, cx);
        }
    }

    /// Closes the banner of the call the account is in now, if one shows.
    fn close_joined(&mut self, cx: &mut Context<Self>) {
        let joined: Vec<String> = self
            .shown
            .values()
            .filter(|shown| self.in_call(&shown.conversation, cx))
            .map(|shown| shown.conversation.clone())
            .collect();
        for conversation in joined {
            self.close_in(&conversation, cx);
        }
    }

    fn close(&self, id: u32, cx: &App) {
        if let Some(server) = self.server.clone() {
            cx.background_spawn(server.close(id)).detach();
        }
    }
}

/// The banner of a start in `conversation` (`place`, by `starter`), as the reads say; `None` when none is due: a
/// meeting the calendar doesn't hold now, a muted channel, reads that failed.
async fn banner(core: &dyn Core, conversation: &str, place: StartedIn, starter: Option<String>) -> Option<Banner> {
    let name = match starter {
        Some(starter) => person_name(core, starter).await,
        None => SOMEONE.to_owned(),
    };
    let (summary, body, join) = match place {
        StartedIn::GroupChat { huddle: false } => return None,
        StartedIn::GroupChat { huddle: true } => (
            chat_name(core, conversation).await,
            format!("{name} started a meeting"),
            MeetingJoin::GroupChat {
                chat: conversation.to_owned(),
            },
        ),
        StartedIn::MeetingChat | StartedIn::Channel => {
            if place == StartedIn::Channel && channel_muted(core, conversation).await {
                return None;
            }
            let event = calendar_meeting(core, conversation).await?;
            let title = if event.subject.trim().is_empty() {
                chat_name(core, conversation).await
            } else {
                event.subject.clone()
            };
            let join = if place == StartedIn::Channel {
                MeetingJoin::Channel {
                    channel: conversation.to_owned(),
                    title: title.clone(),
                    meeting: event.meeting.clone(),
                }
            } else {
                MeetingJoin::MeetingChat {
                    chat: conversation.to_owned(),
                    title: title.clone(),
                }
            };
            (title, format!("{name} started the meeting"), join)
        }
    };
    Some(Banner {
        notice: Notice {
            conversation: conversation.to_owned(),
            kind: NoticeKind::Meeting,
            summary,
            body,
            picture: None,
        },
        join,
    })
}

/// The account's calendar event of the meeting in `conversation` running now ([`scheduled_now`]): the stored calendar
/// first, else the service's.
async fn calendar_meeting(core: &dyn Core, conversation: &str) -> Option<CalendarEvent> {
    let now = now_ms();
    let local = chrono::Local::now().naive_local();
    let window = (now - 24 * 60 * 60 * 1000, now + 24 * 60 * 60 * 1000);
    let stored = core.stored_calendar(window.0, window.1).await.unwrap_or_default();
    if let Some(event) = scheduled_now(&stored, conversation, local) {
        return Some(event.clone());
    }
    let read = core.calendar(window.0, window.1).await.ok()?;
    scheduled_now(&read, conversation, local).cloned()
}

/// Whether the channel `channel`'s notifications are muted; one whose preferences can't be read counts as muted, as a
/// message's banner does.
async fn channel_muted(core: &dyn Core, channel: &str) -> bool {
    let read = core
        .channel_data(teams_proto::ChannelRead::Preferences {
            channel: channel.to_owned(),
        })
        .await;
    read.map_or(true, |data| {
        crate::channel_management::ChannelNotifications::read(&data.0).muted
    })
}

/// The chat's name as the list shows it, else [`NO_TITLE`].
async fn chat_name(core: &dyn Core, conversation: &str) -> String {
    let conversations = core
        .conversations(crate::chat_list::LIST_LIMIT)
        .await
        .unwrap_or_default();
    match conversations.iter().find(|stored| stored.id == conversation) {
        Some(stored) => crate::chat_list::chat_name(core, stored).await.to_string(),
        None => NO_TITLE.to_owned(),
    }
}

/// A person's name as the engine has it, else [`SOMEONE`].
async fn person_name(core: &dyn Core, mri: String) -> String {
    match core.person(mri).await {
        Ok(person) => Shown::of(person)
            .name
            .map_or_else(|| SOMEONE.to_owned(), |name| name.to_string()),
        Err(_) => SOMEONE.to_owned(),
    }
}

#[cfg(test)]
mod tests;
