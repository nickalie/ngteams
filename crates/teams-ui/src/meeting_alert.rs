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
//! **The calling push** ([`FeedEvent::MeetingPushed`], `teams_call::meeting_push`): a meeting's start the calling
//! service pushes to its members, whether or not they opened its chat, shows the same banner for a meeting chat's or a
//! channel's meeting, titled with the push's subject; "<name> is waiting in the lobby. Join the meeting to let them
//! in." for someone waiting in its lobby. A group chat's Meet now shows from the chat's start only, since its Join
//! needs the conversation that start names. The calendar must hold the meeting about now, as above, unless it can't be
//! read: then it shows, as Teams does. One call (its id) shows one banner, from the push or the chat's start, whichever
//! came first.
//!
//! **The window's card**: each banner shows in the window as well ([`MeetingAlert::cards`], drawn by the shell), for
//! [`CARD_TIME`], or until it is acted on or closed; without a notification server (or one that refused it) only the
//! card shows. A banner shown before the server was handed in (as the app starts) gets its desktop notification once
//! it is.
//!
//! It shows its starter's picture as the engine has it stored, as a message's banner shows its sender's
//! ([`crate::notify::picture`]); none is waited for from the network.
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
//! a name or a title; each decision is logged at debug level by the chat's id ("meeting banner: …").

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDateTime;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Subscription};
use teams_core::{CalendarEvent, MeetingStartKind, MeetingStartPush, Source};
use teams_proto::chat::{ChatEvent, chat_event, meeting_times};
use teams_proto::{MeetingInfo, is_group_chat_id, is_meeting_chat_id, is_team_thread_id};
use teams_store::{Message, StoredPicture};

use crate::bridge::Core;
use crate::calls::Calls;
use crate::models::{FeedEvent, Models};
use crate::notify::{
    self, BannerPicture, DEFAULT_ACTION, JOIN_ACTION, Notice, NoticeKind, NotificationServer, SOMEONE,
};
use crate::people::Shown;
use crate::settings::{ManualStatus, Notifications, Settings, SettingsModel};
use crate::sounds::SoundEvent;
use crate::trace::{TraceLine, ViewTrace};

/// How long after it arrived a start a catch-up or a replay brought still shows a banner, in ms.
pub const FRESH_MS: i64 = 5 * 60 * 1000;
/// How long before a meeting's scheduled start its start shows a banner, in ms.
pub const EARLY_MS: i64 = 15 * 60 * 1000;
/// How long after a meeting's scheduled end its start still shows a banner, in ms (Teams'
/// `meetingStartedNotificationV2Settings.minutesAfter` default).
pub const LATE_MS: i64 = 5 * 60 * 1000;
/// How long the window's card of a started meeting shows (Teams' banner time).
pub const CARD_TIME: std::time::Duration = std::time::Duration::from_secs(20);
/// The ids of banners only the window's card shows start here, apart from a notification server's.
pub const LOCAL_IDS: u32 = 0xF000_0000;
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
/// no such meeting then. A channel's start must belong to the scheduled meeting's `post`.
#[must_use]
pub fn scheduled_now<'e>(
    events: &'e [CalendarEvent],
    conversation: &str,
    post: Option<&str>,
    now: NaiveDateTime,
) -> Option<&'e CalendarEvent> {
    let (early, late) = (
        chrono::Duration::milliseconds(EARLY_MS),
        chrono::Duration::milliseconds(LATE_MS),
    );
    events.iter().find(|event| {
        let of_it = if is_team_thread_id(conversation) {
            post.is_some()
                && event
                    .channel_meeting
                    .as_ref()
                    .is_some_and(|channel| channel.channel == conversation && channel.post.as_deref() == post)
        } else {
            event.meeting_chat.as_deref() == Some(conversation)
        };
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
    /// A calendar meeting's ([`crate::meeting_reminder`]): its pre-join as the calendar's Join opens it, by its chat and
    /// organizer, else by its chat, else its link in the browser.
    Calendar {
        chat: Option<String>,
        meeting: Option<MeetingInfo>,
        title: String,
        link: Option<String>,
    },
}

/// A started meeting's banner as the window shows it, a card with Join, its chat (but for a channel's) and Dismiss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartedCard {
    /// The banner's id: its actions go to [`MeetingAlert::activated`] with it.
    pub id: u32,
    pub title: String,
    pub body: String,
    pub opens_chat: bool,
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
    title: String,
    body: String,
    /// Whether the window still shows its card.
    card: bool,
    /// Its place in the order the banners came, which a desktop notification's id that came later doesn't change.
    seq: u64,
    /// Its desktop notification, while none was shown because no server was handed in yet (a banner as the app
    /// starts): it is shown once one is.
    unsent: Option<Notice>,
}

/// A banner decided on, to show: its notice, without its starter's `picture` until that is decoded.
struct Banner {
    notice: Notice,
    join: MeetingJoin,
    picture: Option<StoredPicture>,
}

/// The start a banner is being decided for: its chat, its message, its call, and how many of the chat's ends (and of
/// its ends naming no call) had come then.
struct Pending {
    conversation: String,
    start: String,
    call: Option<String>,
    ends: Ends,
}

/// What told of a start: the chat service's call start in the chat, or the calling service's push.
enum Origin {
    Chat,
    Push(Box<MeetingStartPush>),
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
    /// The calls a banner was decided for, by their id: the chat service's start and the calling push of one call
    /// show one banner.
    started_calls: HashSet<String>,
    /// How many call ends each chat had.
    ends: HashMap<String, Ends>,
    /// The calls that ended, by their chat and call id.
    ended: HashSet<(String, String)>,
    /// The reads of the group chats' running calls.
    running: HashMap<String, RunningRead>,
    /// The last id given to a banner only the window's card shows.
    next_local: u32,
    /// The last banner's place in their order.
    next_seq: u64,
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
    /// The alert over the models; without a server ([`MeetingAlert::set_server`]) its banners show only as the
    /// window's cards.
    pub fn new(
        core: Arc<dyn Core>,
        models: &Models,
        settings: Entity<SettingsModel>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let feed = cx.subscribe(&models.feed, |this: &mut Self, _, event, cx| match event {
            FeedEvent::Arrived {
                conversation,
                message,
                source,
            } => this.arrived(conversation, message, *source, cx),
            FeedEvent::MeetingPushed(start) => this.pushed(start, cx),
            _ => {}
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
            started_calls: HashSet::new(),
            ends: HashMap::new(),
            ended: HashSet::new(),
            running: HashMap::new(),
            next_local: LOCAL_IDS,
            next_seq: 0,
            _feed: feed,
            _calls: calls,
        };
        this.read_all_running(cx);
        this
    }

    /// From now on, banners go to `server`; a banner the window shows already without one (one due as the app
    /// started, before the server came) goes there now.
    pub fn set_server(&mut self, server: Arc<dyn NotificationServer>, cx: &mut Context<Self>) {
        self.server = Some(server);
        let unsent: Vec<u32> = self
            .shown
            .iter()
            .filter(|(_, shown)| shown.unsent.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in unsent {
            self.send_late(id, cx);
        }
    }

    /// Shows banner `id`'s desktop notification, which no server showed when it came; the banner takes the server's id.
    fn send_late(&mut self, id: u32, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let Some((notice, seq)) = self
            .shown
            .get_mut(&id)
            .and_then(|shown| Some((shown.unsent.take()?, shown.seq)))
        else {
            return;
        };
        log::debug!(
            "meeting banner: a desktop notification late for {}",
            notice.conversation
        );
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Ok(given) = server.show(&notice, 0).await else {
                log::debug!("meeting banner: no desktop notification for {}", notice.conversation);
                return;
            };
            // a banner closed while the server answered takes its notification away again
            let kept = this.update(cx, |this, cx| {
                let local = this
                    .shown
                    .iter()
                    .find_map(|(id, shown)| (shown.seq == seq).then_some(*id))?;
                let shown = this.shown.remove(&local)?;
                this.shown.insert(given, shown);
                // the drawn card's actions hold the old id: draw it again with the new one
                cx.notify();
                Some(())
            });
            if kept.ok().flatten().is_none() {
                server.close(given).await;
            }
        })
        .detach();
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
        cx.notify();
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
                let fresh = fresh(source, message.arrival_time, now_ms());
                let theirs = may_show(self.core.me(), place, starter.as_deref());
                let due = fresh
                    && theirs
                    && !self.ended_since(&pending)
                    && self.seen.insert((conversation.to_owned(), message.id.clone()))
                    && pending
                        .call
                        .as_ref()
                        .is_none_or(|call| self.started_calls.insert(call.clone()));
                let on = self.on_now(cx);
                let in_call = self.in_call(conversation, cx);
                log::debug!(
                    "meeting banner: a call start in {conversation} ({place:?}, {source:?}): fresh={fresh} \
                     someone_else's={theirs} due={due} banners_on={on} in_call={in_call}"
                );
                if due && on && !in_call {
                    self.decide(pending, place, starter, Origin::Chat, cx);
                }
            }
        }
        self.read_running(conversation, ended, cx);
    }

    /// The calling service pushed a meeting's start: a meeting's (its chat's or a channel's), or someone waiting in its
    /// lobby. Each call shows once, whichever of this and the chat's start came first. A group chat's Meet now is left
    /// to the chat's start: its Join goes where that start says the call runs, which the push doesn't say.
    fn pushed(&mut self, start: &MeetingStartPush, cx: &mut Context<Self>) {
        let conversation = start.thread.as_str();
        if start.kind == MeetingStartKind::Huddle && is_group_chat_id(conversation) {
            log::debug!(
                "meeting banner: a pushed Meet now in {conversation} (call {}): left to the chat's start",
                start.call_id
            );
            return;
        }
        let place = if is_team_thread_id(conversation) {
            StartedIn::Channel
        } else {
            StartedIn::MeetingChat
        };
        let theirs = may_show(self.core.me(), place, start.starter.as_deref());
        let pending = Pending {
            ends: self.ends_in(conversation),
            conversation: conversation.to_owned(),
            start: format!("push:{}", start.call_id),
            call: Some(start.call_id.clone()),
        };
        let due = theirs && !self.ended_since(&pending) && self.started_calls.insert(start.call_id.clone());
        let on = self.on_now(cx);
        let in_call = self.in_call(conversation, cx);
        log::debug!(
            "meeting banner: a pushed start in {conversation} ({:?}, call {}): someone_else's={theirs} due={due} \
             banners_on={on} in_call={in_call}",
            start.kind,
            start.call_id
        );
        if due && on && !in_call {
            let starter = start.starter.clone();
            self.decide(pending, place, starter, Origin::Push(Box::new(start.clone())), cx);
        }
    }

    /// A start of `pending`'s call showed no banner: a later start of that call (the push, or the chat's) may.
    fn forget_call(&mut self, pending: &Pending) {
        if let Some(call) = &pending.call {
            self.started_calls.remove(call);
        }
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

    /// Whether a banner may show now: its Join works here (a build that shows calls, not the read-only mode) and the
    /// settings let it.
    fn on_now(&self, cx: &App) -> bool {
        let joins = self.calls.read(cx).shows() && !self.core.read_only();
        joins
            && banners_on(
                self.settings.read(cx).settings(),
                self.core.me(),
                notify::muted_in_call(cx),
            )
    }

    /// Whether the account is in the call of `conversation` (or going into it) now.
    fn in_call(&self, conversation: &str, cx: &App) -> bool {
        let calls = self.calls.read(cx);
        calls.call().and_then(|call| call.chat.as_deref()) == Some(conversation)
            || calls.prejoin().and_then(|prejoin| prejoin.to_join.chat()) == Some(conversation)
    }

    /// Reads what the banner needs (the chat's name, the calendar, the channel's preferences), then shows it if it is
    /// still due; one whose call ended while it was being shown is closed again.
    fn decide(
        &self,
        pending: Pending,
        place: StartedIn,
        starter: Option<String>,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Some(mut banner) = banner(&*core, &pending, place, starter, &origin).await else {
                log::debug!(
                    "meeting banner: none for {}: not due by its reads",
                    pending.conversation
                );
                let _ = this.update(cx, |this, _| this.forget_call(&pending));
                return;
            };
            // an end that came before a restart is in the store only
            let stored_end = core
                .call_ended(pending.conversation.clone(), pending.start.clone())
                .await;
            if stored_end.unwrap_or(false) {
                log::debug!("meeting banner: none for {}: its call ended", pending.conversation);
                let _ = this.update(cx, |this, _| this.forget_call(&pending));
                return;
            }
            let server = this
                .update(cx, |this, cx| {
                    let due =
                        this.on_now(cx) && !this.in_call(&pending.conversation, cx) && !this.ended_since(&pending);
                    due.then(|| this.server.clone())
                })
                .ok()
                .flatten();
            let Some(server) = server else {
                log::debug!("meeting banner: none for {}: no longer due", pending.conversation);
                return;
            };
            if let Some(picture) = banner.picture.take() {
                banner.notice.picture = cx
                    .background_executor()
                    .spawn(async move { BannerPicture::of_person(&picture) })
                    .await;
            }
            // the window's card shows it even when the desktop has no notification server or it refused one
            let shown = match &server {
                Some(server) => server.show(&banner.notice, 0).await.ok(),
                None => None,
            };
            if shown.is_none() {
                log::debug!("meeting banner: no desktop notification for {}", pending.conversation);
            }
            let no_server = server.is_none();
            let kept = this.update(cx, |this, cx| {
                let due = !this.ended_since(&pending) && !this.in_call(&pending.conversation, cx);
                if due {
                    let id = shown.unwrap_or_else(|| this.local_id());
                    log::debug!("meeting banner: shown for {}", pending.conversation);
                    this.shown_banner(id, banner, pending.call.clone(), no_server, cx);
                }
                due
            });
            if let (false, Some(server), Some(id)) = (kept.unwrap_or(false), server, shown) {
                server.close(id).await;
            }
        })
        .detach();
    }

    /// Banner `id` shows; `unsent` when no server was handed in to show its desktop notification.
    fn shown_banner(&mut self, id: u32, banner: Banner, call: Option<String>, unsent: bool, cx: &mut Context<Self>) {
        let conversation = banner.notice.conversation.clone();
        let kind = match banner.join {
            MeetingJoin::GroupChat { .. } => "group-chat",
            MeetingJoin::MeetingChat { .. } => "meeting-chat",
            MeetingJoin::Channel { .. } => "channel",
            MeetingJoin::Calendar { .. } => "calendar",
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
        self.next_seq += 1;
        let seq = self.next_seq;
        let unsent = unsent.then(|| banner.notice.clone());
        self.shown.insert(
            id,
            ShownBanner {
                conversation,
                call,
                join: banner.join,
                title: banner.notice.summary,
                body: banner.notice.body,
                card: true,
                seq,
                unsent,
            },
        );
        // the server came while this banner was being decided
        if self.server.is_some() {
            self.send_late(id, cx);
        }
        // the window's card goes after Teams' banner time; the desktop's stays as its server keeps it
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(CARD_TIME).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(shown) = this.shown.values_mut().find(|shown| shown.seq == seq) {
                    shown.card = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// An id for a banner only the window's card shows (no desktop notification): from [`LOCAL_IDS`] up, apart from
    /// a notification server's.
    fn local_id(&mut self) -> u32 {
        self.next_local = self.next_local.max(LOCAL_IDS).wrapping_add(1).max(LOCAL_IDS);
        self.next_local
    }

    /// The banners the window shows as cards now, the newest last.
    #[must_use]
    pub fn cards(&self) -> Vec<StartedCard> {
        let mut cards: Vec<(u64, StartedCard)> = self
            .shown
            .iter()
            .filter(|(_, shown)| shown.card)
            .map(|(id, shown)| {
                let card = StartedCard {
                    id: *id,
                    title: shown.title.clone(),
                    body: shown.body.clone(),
                    opens_chat: !is_team_thread_id(&shown.conversation),
                };
                (shown.seq, card)
            })
            .collect();
        cards.sort_by_key(|(seq, _)| *seq);
        cards.into_iter().map(|(_, card)| card).collect()
    }

    /// The card of banner `id` was dismissed: the banner closes, the desktop's too.
    pub fn dismiss(&mut self, id: u32, cx: &mut Context<Self>) {
        if let Some(shown) = self.shown.remove(&id) {
            self.trace
                .record(&TraceLine::new("meeting-notification", "closed").conversation(&shown.conversation));
            self.close(id, cx);
            cx.notify();
        }
    }

    /// Closes the banners of `conversation` whose call ended (`call`, the call its end named): that call's, or every
    /// one when the end named no call; a banner whose start named no call closes at any end.
    fn close_ended(&mut self, conversation: &str, call: Option<&str>, cx: &mut Context<Self>) {
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
    fn close_in(&mut self, conversation: &str, cx: &mut Context<Self>) {
        let ids: Vec<u32> = self
            .shown
            .iter()
            .filter(|(_, shown)| shown.conversation == conversation)
            .map(|(id, _)| *id)
            .collect();
        self.close_ids(conversation, ids, cx);
    }

    fn close_ids(&mut self, conversation: &str, ids: Vec<u32>, cx: &mut Context<Self>) {
        if !ids.is_empty() {
            cx.notify();
        }
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
        if let Some(server) = self.server.clone().filter(|_| id < LOCAL_IDS) {
            cx.background_spawn(server.close(id)).detach();
        }
    }
}

/// The banner of `pending`'s start (`place`, by `starter`), as the reads say; `None` when none is due: a
/// meeting the calendar doesn't hold now, a muted channel, reads that failed.
async fn banner(
    core: &dyn Core,
    pending: &Pending,
    place: StartedIn,
    starter: Option<String>,
    origin: &Origin,
) -> Option<Banner> {
    let conversation = &pending.conversation;
    let pushed = match origin {
        Origin::Push(start) => Some(&**start),
        Origin::Chat => None,
    };
    let (mut name, picture) = match starter {
        Some(starter) => person(core, starter).await,
        None => (SOMEONE.to_owned(), None),
    };
    if name == SOMEONE
        && let Some(given) = pushed.and_then(|start| start.starter_name.clone())
    {
        name = given;
    }
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
            scheduled_banner(core, pending, place == StartedIn::Channel, &name, pushed).await?
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
        picture,
    })
}

/// A meeting chat's or a channel's (`channel`) banner, by `name`: its title, its line and its Join; `None` for a muted
/// channel, or a meeting the calendar doesn't hold about now (a push's, only when the calendar can be read).
async fn scheduled_banner(
    core: &dyn Core,
    pending: &Pending,
    channel: bool,
    name: &str,
    pushed: Option<&MeetingStartPush>,
) -> Option<(String, String, MeetingJoin)> {
    let conversation = &pending.conversation;
    if channel && channel_muted(core, conversation).await {
        log::debug!("meeting banner: none for {conversation}: the channel is muted");
        return None;
    }
    let found = if let Some(start) = pushed {
        // a push shows when the calendar can't be read, as Teams does
        calendar_lookup(core, conversation, start.message.as_deref()).await
    } else {
        Ok(calendar_meeting(core, conversation, &pending.start).await)
    };
    if matches!(found, Ok(None)) {
        log::debug!("meeting banner: none for {conversation}: not on the calendar about now");
        return None;
    }
    let event = found.ok().flatten();
    let subject = pushed
        .and_then(|start| start.subject.clone())
        .or_else(|| event.as_ref().map(|event| event.subject.clone()))
        .filter(|subject| !subject.trim().is_empty());
    let title = match subject {
        Some(subject) => subject,
        None => chat_name(core, conversation).await,
    };
    let join = if channel {
        let meeting = event.and_then(|event| event.meeting).or_else(|| {
            pushed.and_then(|start| {
                Some(MeetingInfo {
                    organizer_id: start.organizer.clone()?,
                    tenant_id: start.tenant.clone()?,
                })
            })
        });
        MeetingJoin::Channel {
            channel: conversation.to_owned(),
            title: title.clone(),
            meeting,
        }
    } else {
        MeetingJoin::MeetingChat {
            chat: conversation.to_owned(),
            title: title.clone(),
        }
    };
    let body = if pushed.is_some_and(|start| start.kind == MeetingStartKind::Lobby) {
        format!("{name} is waiting in the lobby. Join the meeting to let them in.")
    } else {
        format!("{name} started the meeting")
    };
    Some((title, body, join))
}

/// The account's calendar event of the meeting in `conversation` running now ([`scheduled_now`]): the stored calendar
/// first, else the service's.
async fn calendar_meeting(core: &dyn Core, conversation: &str, start: &str) -> Option<CalendarEvent> {
    let post = if is_team_thread_id(conversation) {
        Some(
            core.reply_roots(conversation.to_owned())
                .await
                .ok()?
                .into_iter()
                .find_map(|(message, root)| (message == start).then_some(root))?,
        )
    } else {
        None
    };
    calendar_lookup(core, conversation, post.as_deref())
        .await
        .ok()
        .flatten()
}

/// The account's calendar event of the meeting in `conversation` (a channel's: its `post`) running now
/// ([`scheduled_now`]): the stored calendar first, else the service's. `Err` when neither holds it and the service's
/// can't be read.
async fn calendar_lookup(core: &dyn Core, conversation: &str, post: Option<&str>) -> Result<Option<CalendarEvent>, ()> {
    let now = now_ms();
    let local = chrono::Local::now().naive_local();
    let window = (now - 24 * 60 * 60 * 1000, now + 24 * 60 * 60 * 1000);
    let stored = core.stored_calendar(window.0, window.1).await.unwrap_or_default();
    if let Some(event) = scheduled_now(&stored, conversation, post, local) {
        return Ok(Some(event.clone()));
    }
    let read = core.calendar(window.0, window.1).await.map_err(|_| ())?;
    Ok(scheduled_now(&read, conversation, post, local).cloned())
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
pub(crate) async fn chat_name(core: &dyn Core, conversation: &str) -> String {
    let conversations = core
        .conversations(crate::chat_list::LIST_LIMIT)
        .await
        .unwrap_or_default();
    match conversations.iter().find(|stored| stored.id == conversation) {
        Some(stored) => crate::chat_list::chat_name(core, stored).await.to_string(),
        None => NO_TITLE.to_owned(),
    }
}

/// A person's name as the engine has it, else [`SOMEONE`], and their picture as it has it stored.
async fn person(core: &dyn Core, mri: String) -> (String, Option<StoredPicture>) {
    match core.person(mri).await {
        Ok(person) => {
            let picture = person.picture.clone();
            let name = Shown::of(person)
                .name
                .map_or_else(|| SOMEONE.to_owned(), |name| name.to_string());
            (name, picture)
        }
        Err(_) => (SOMEONE.to_owned(), None),
    }
}

#[cfg(test)]
mod tests;
