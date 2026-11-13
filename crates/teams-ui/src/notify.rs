//! Desktop notifications for messages that come (M2 Task 14).
//!
//! **Which messages notify.** The [`Notifier`] listens to the models' [`Feed`](crate::models::Feed): a message newly
//! stored, from someone else, a text message not deleted, in a chat (not one of the account's own streams). Then, as
//! the "notifications" setting says ([`Notifications`]: all, mentions only, off), and only when the chat isn't the
//! one shown in a focused window:
//! - one delivered over the push channel notifies; one the store already had (not newly stored), older history
//!   ([`Source::History`]) and an edit, a delete or a reaction never do;
//! - one a catch-up brought ([`Source::CatchUp`]), or a replayed delivery ([`Source::Replay`]: the service pushing
//!   again on a reconnect what it pushed before, newly stored when the earlier push never reached the app), notifies
//!   only if it is unread (after the account's read horizon) and arrived after the cutoff ([`Notifier::cutoff`]): the
//!   app's start until the push channel first goes down, then the moment it was last known to deliver before it last
//!   went down (when its socket last heard from the service, as the engine tells it: a socket gone silent in a suspend
//!   is noticed only later), less [`CLOCK_SKEW_MS`], since the service's times are compared with this machine's;
//! - in a team's channel only a mention of the account notifies (channels are the sidebar's teams section's).
//!
//! A chat read in another language ([`crate::chat_translate`]) notifies in that language: its message's translation is
//! waited for at most [`NOTIFY_BUDGET`] (the store's, else the tool's, [`Previews`]); one not ready by then shows as
//! written, and nothing is notified again when it comes.
//!
//! A mention of the account is marked as such ("… mentioned you"). Messages are gathered for [`GATHER`] before they
//! are shown, so a catch-up's are seen together: up to [`SINGLES`] of one chat are shown one by one; more than that
//! collapse into one notification of the chat ("N new messages"), which the ones shown before give way to (closed)
//! and later ones replace; it says "one or more mentioning you" while any message it stands for (one shown before it
//! included) mentioned the account. A chat's notifications are closed once it is read (its read horizon moves, or a
//! read of the service changes the chat, and nothing in it is unread), opened from one, or shown in a focused window
//! when the next message comes. Whether a chat is shown in a focused window is asked again just before its
//! notifications are shown, and a chat opened or read while its messages were being looked at shows none of them.
//!
//! **An app's Activity item** (a new item of the account's own feed, `48:notifications`, that carries
//! `activityContext.teamsAppId`) shows a banner as Teams' does for an app's activity: its title (the app's own, else
//! the app's name) and the item's preview, while the "notifications" setting is all, and only when the app's
//! preferences, read then ([`teams_proto::UserGraphSettings::banner`]), keep its activity type's banner on: an app
//! with no saved choice shows one, an app turned off (or set to "Show in Activity") none, nor one whose preferences
//! couldn't be read. A catch-up's or a replayed one notifies only when unread and after the cutoff, as a chat's.
//! Activating it opens the Activity pane at the item ([`NotifierEvent::OpenActivity`]).
//!
//! **Pictures** ([`picture`]): a message's or a mention's banner shows its sender's picture, a collapsed one in a 1:1
//! chat the other person's (a group's or a channel's is about the chat, and shows none), an app item's the app's tile,
//! each only when the engine already has it: a banner never waits on a network read. An app tile the engine doesn't
//! have yet is read after its banner shows, so the app's next banner has it.
//!
//! **Activating one** (its body, the `default` action) opens its chat: [`NotifierEvent::Open`], or for a team's
//! channel (the stored conversation's kind when it was shown) [`NotifierEvent::OpenChannel`], which the
//! [`Desktop`](crate::desktop::Desktop) acts on. Each one shown is told too ([`NotifierEvent::Shown`]: its chat and
//! title, for the control socket's event stream).
//!
//! **The path.** GPUI's own system notifications reach the bus, but can't replace or close a notification (their tag
//! replaces nothing on Linux), and each opens a connection of its own with `zbus`'s session-bus constructor, which
//! falls back to `$XDG_RUNTIME_DIR/bus` without an address (`tests/gpui_notification.rs`). So the notifications go
//! through a
//! [`NotificationServer`]: in the app [`BusNotifications`], `org.freedesktop.Notifications` over `zbus` on the
//! connection to the bus the app is given ([`crate::bus`]); Windows uses `WinRT` toasts; in a view test a fake. What a notification says (a name,
//! the message's first line) goes only to that bus, never to the view trace, which records the conversation id and
//! the kind.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::future::BoxFuture;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, SharedString, Subscription, WeakEntity};
use teams_core::activity::{ActivityFeed, ActivityItem};
use teams_core::{CLOCK_SKEW_MS, PictureBytes, Source};
use teams_proto::ACTIVITY_FEED;
use teams_proto::chat::RichText;
use teams_store::{Conversation, ConversationKind, Message, StoredPicture};

use crate::bridge::Core;
use crate::calls::Calls;
use crate::chat_list::{self, LIST_LIMIT};
use crate::chat_translate::Previews;
use crate::chat_translate::previews::NOTIFY_BUDGET;
use crate::models::{FeedEvent, Models};
use crate::people::Shown;
use crate::rich_text::Mentions;
use crate::settings::{Notifications, SettingsModel};
use crate::sounds::SoundEvent;
use crate::system_message;
use crate::trace::{TraceLine, ViewTrace};

pub mod picture;
pub use picture::{BannerFiles, BannerPicture, PictureShape};

/// How long messages are gathered before they are shown.
pub const GATHER: Duration = Duration::from_millis(250);
/// The most notifications of one chat shown one by one; more collapse into one.
pub const SINGLES: usize = 3;
/// The notification server's bus name and object path, and its interface.
pub const SERVER_NAME: &str = "org.freedesktop.Notifications";
pub const SERVER_PATH: &str = "/org/freedesktop/Notifications";
/// The action of a notification's body.
pub const DEFAULT_ACTION: &str = "default";
/// An incoming call's notification's actions (M3 Task 17): answer it, decline it.
pub const ANSWER_ACTION: &str = "answer";
pub const DECLINE_ACTION: &str = "decline";
/// What a sender without a name is called.
pub const SOMEONE: &str = "Someone";

/// What a notification is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// One message.
    Message,
    /// One message that mentions the account.
    Mention,
    /// Several messages of one chat, in one.
    Collapsed,
    /// An incoming call, with Answer and Decline ([`crate::call_alert`]).
    Call,
    /// An app's Activity item.
    Activity,
}

impl NoticeKind {
    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Mention => "mention",
            Self::Collapsed => "collapsed",
            Self::Call => "call",
            Self::Activity => "activity",
        }
    }
}

/// One desktop notification, as the notifier asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub conversation: String,
    pub kind: NoticeKind,
    pub summary: String,
    pub body: String,
    /// The person's or the app's picture it shows; `None` shows the app's icon alone.
    pub picture: Option<BannerPicture>,
}

/// The desktop's notification server, as the notifier sees it.
pub trait NotificationServer: Send + Sync + 'static {
    /// Shows `notice`, in place of notification `replaces` when that isn't 0; answers the server's id for it.
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>>;
    /// Closes notification `id`; one already gone is no matter.
    fn close(&self, id: u32) -> BoxFuture<'static, ()>;
}

/// What the notifier tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifierEvent {
    /// A notification was activated: open its chat.
    Open(String),
    /// A notification of a team's channel was activated: open the channel (never in a chat view).
    OpenChannel(String),
    /// An app's Activity item's notification was activated: open the Activity pane at the item (its feed id).
    OpenActivity(String),
    /// A notification was shown (the control socket's `notification`): its chat and its title.
    Shown { conversation: String, title: String },
}

/// The chat shown in a focused window, if one is: no notification is shown for it.
pub type Viewing = Box<dyn Fn(&mut App) -> Option<String>>;

/// A message that may notify, as it came.
#[derive(Clone, Debug)]
struct Candidate {
    conversation: String,
    message: Message,
    source: Source,
}

/// What is shown for one chat.
#[derive(Debug, Default)]
struct ChatNotices {
    /// The notifications shown one by one, by the server's id.
    singles: Vec<u32>,
    /// The one the chat's messages collapsed into.
    collapsed: Option<u32>,
    /// The messages notified since the chat was last read.
    count: usize,
    /// One of them mentioned the account.
    mentioned: bool,
}

impl ChatNotices {
    fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.singles.iter().copied().chain(self.collapsed)
    }
}

/// What one gathering hands its processing: the messages and what decides about them, as they stood when it began.
struct Batch {
    candidates: Vec<Candidate>,
    level: Notifications,
    viewing: Option<String>,
    cutoff: i64,
    /// The notifier's count of forgotten chats when the batch was taken: a chat forgotten after it shows nothing of it.
    taken: u64,
    core: Arc<dyn Core>,
    server: Arc<dyn NotificationServer>,
}

/// The notifications (see the module docs). A GPUI entity.
pub struct Notifier {
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    server: Option<Arc<dyn NotificationServer>>,
    viewing: Viewing,
    /// A catch-up's message notifies only when it arrived after this, in milliseconds ([`Notifier::cutoff`]).
    cutoff: i64,
    /// Messages gathered, not yet processed.
    pending: Vec<Candidate>,
    /// A gathering is under way (it processes what is pending until nothing is).
    gathering: bool,
    shown: HashMap<String, ChatNotices>,
    /// Every notification shown, by the server's id: its chat.
    ids: HashMap<u32, String>,
    /// The conversations shown that were stored as a team's channel.
    channels: HashSet<String>,
    /// The app items' notifications shown, by the server's id: the item's feed id.
    app_items: HashMap<u32, String>,
    /// How many times a chat's notifications were forgotten (closed, its count started again), and per chat that
    /// count when it last was.
    forgets: u64,
    forgotten: HashMap<String, u64>,
    /// The account's calls: whether a sound counts as "in a call" ([`crate::sounds`]).
    calls: Entity<Calls>,
    /// The account's translations: a translated chat's message is shown in its language when its translation comes
    /// within [`NOTIFY_BUDGET`].
    previews: Entity<Previews>,
    _feed: Subscription,
}

impl EventEmitter<NotifierEvent> for Notifier {}

/// Milliseconds since the epoch of `time` (0 before it).
fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// Whether `message` (newly stored in `conversation`, brought by `source`) may notify `me` at all: someone else's
/// message of a type Teams notifies for ([`system_message::notifies`]: a text message or a card, never a call's, a
/// recording's, a transcript's or a thread's event), never one the thread itself sent. The rest is decided when it is
/// processed.
#[must_use]
pub fn may_notify(me: &str, conversation: &str, message: &Message, source: Source) -> bool {
    matches!(source, Source::Delivery | Source::Replay | Source::CatchUp)
        && message
            .sender
            .as_deref()
            .is_some_and(|sender| sender != me && !system_message::is_thread(sender))
        && system_message::notifies(&message.message_type)
        && message.delete_time.is_none()
        && !conversation.starts_with("48:")
}

impl Notifier {
    /// The notifier over the models; it shows nothing until it is given a server ([`Notifier::set_server`]).
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
            FeedEvent::Read { conversation } | FeedEvent::Changed { conversation } => this.read(conversation, cx),
            FeedEvent::Down { heard } => this.went_down(*heard),
            // the account's own presence's, nothing to notify
            FeedEvent::Registered | FeedEvent::PushStopped | FeedEvent::OwnReport { .. } => {}
        });
        let previews = Previews::of(&core, &settings, cx);
        Self {
            core,
            settings,
            trace,
            server: None,
            viewing: Box::new(|_| None),
            cutoff: millis(SystemTime::now()),
            pending: Vec::new(),
            gathering: false,
            shown: HashMap::new(),
            ids: HashMap::new(),
            channels: HashSet::new(),
            app_items: HashMap::new(),
            forgets: 0,
            forgotten: HashMap::new(),
            calls: models.calls.clone(),
            previews,
            _feed: feed,
        }
    }

    /// From now on, notifications go to `server`.
    pub fn set_server(&mut self, server: Arc<dyn NotificationServer>) {
        self.server = Some(server);
    }

    /// Whether a server was given.
    #[must_use]
    pub fn has_server(&self) -> bool {
        self.server.is_some()
    }

    /// How to tell the chat shown in a focused window.
    pub fn set_viewing(&mut self, viewing: Viewing) {
        self.viewing = viewing;
    }

    /// A catch-up's message (or a replayed delivery newly stored) notifies only when it arrived after this, in
    /// milliseconds since the epoch: the app's start until the push channel first goes down, then the moment it was
    /// last known to deliver before its last drop less [`CLOCK_SKEW_MS`] (see the module docs).
    #[must_use]
    pub fn cutoff(&self) -> i64 {
        self.cutoff
    }

    /// The push channel went down, last heard at `heard`: the cutoff moves there. One that never connected moves
    /// nothing (what it would have delivered is still missed since the last drop, or the start).
    fn went_down(&mut self, heard: Option<SystemTime>) {
        if let Some(heard) = heard {
            self.cutoff = millis(heard).saturating_sub(CLOCK_SKEW_MS);
        }
    }

    /// The notifications shown now, by the server's id, with their chats.
    #[must_use]
    pub fn shown(&self) -> Vec<(u32, String)> {
        let mut shown: Vec<(u32, String)> = self.ids.iter().map(|(id, chat)| (*id, chat.clone())).collect();
        shown.sort_unstable();
        shown
    }

    /// Notification `id` was activated (its body): its chat opens, and the chat's notifications close.
    pub fn activated(&mut self, id: u32, cx: &mut Context<Self>) {
        if let Some(item) = self.app_items.remove(&id) {
            self.trace
                .record(&TraceLine::new("notification", "opened").conversation(ACTIVITY_FEED));
            cx.emit(NotifierEvent::OpenActivity(item));
            return;
        }
        let Some(conversation) = self.ids.get(&id).cloned() else {
            return;
        };
        self.trace
            .record(&TraceLine::new("notification", "opened").conversation(&conversation));
        cx.emit(if self.channels.contains(&conversation) {
            NotifierEvent::OpenChannel(conversation.clone())
        } else {
            NotifierEvent::Open(conversation.clone())
        });
        self.forget(&conversation, cx);
    }

    fn arrived(&mut self, conversation: &str, message: &Message, source: Source, cx: &mut Context<Self>) {
        if conversation == ACTIVITY_FEED {
            self.app_item_arrived(message, source, cx);
            return;
        }
        if self.settings.read(cx).settings().notifications == Notifications::Off
            || muted_in_call(cx)
            || !may_notify(self.core.me(), conversation, message, source)
        {
            return;
        }
        self.pending.push(Candidate {
            conversation: conversation.to_owned(),
            message: message.clone(),
            source,
        });
        if !self.gathering {
            self.gathering = true;
            cx.spawn(async move |this, cx: &mut AsyncApp| gather(this, cx).await)
                .detach();
        }
    }

    /// Whether an app item's banner may be shown now: notifications on for all messages, not muted in a call, a server.
    fn app_banners_on(&self, cx: &App) -> Option<Arc<dyn NotificationServer>> {
        let on = self.settings.read(cx).settings().notifications == Notifications::All && !muted_in_call(cx);
        self.server.clone().filter(|_| on)
    }

    /// A feed message newly stored: an app's item shows its banner when its app's preferences keep it on (see the
    /// module docs).
    fn app_item_arrived(&mut self, message: &Message, source: Source, cx: &mut Context<Self>) {
        let Some(item) = app_item(message, source, self.cutoff) else {
            return;
        };
        if self.app_banners_on(cx).is_none() {
            return;
        }
        let settings = self.core.app_notification_settings();
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let app = item.teams_app_id.clone().unwrap_or_default();
            let on = settings
                .await
                .ok()
                .flatten()
                .is_some_and(|settings| settings.banner(&app, item.subtype.as_deref().unwrap_or_default()));
            if !on {
                return;
            }
            let picture = if let Some(tile) = cached_app_tile(&*core, &app).await {
                cx.background_executor()
                    .spawn(async move { BannerPicture::of_app(&tile) })
                    .await
            } else {
                read_app_tile(&core, app, cx);
                None
            };
            let Ok(Some(server)) = this.update(cx, |this, cx| this.app_banners_on(cx)) else {
                return;
            };
            let notice = Notice {
                picture,
                ..app_notice(&item)
            };
            if let Ok(id) = server.show(&notice, 0).await {
                let _ = this.update(cx, |this, cx| this.shown_app_item(&notice, id, item.id.clone(), cx));
            }
        })
        .detach();
    }

    /// `notice`, an app item's, shown as notification `id`.
    fn shown_app_item(&mut self, notice: &Notice, id: u32, item: String, cx: &mut Context<Self>) {
        self.app_items.insert(id, item);
        self.trace.record(
            &TraceLine::new("notification", "shown")
                .conversation(ACTIVITY_FEED)
                .kind(NoticeKind::Activity.name()),
        );
        self.sound(notice, false, cx);
        Self::tell_shown(notice, cx);
    }

    /// The account's read horizon in `conversation` moved: once nothing in it is unread, its notifications close.
    fn read(&mut self, conversation: &str, cx: &mut Context<Self>) {
        if !self.shown.contains_key(conversation) {
            return;
        }
        let unread = self.core.unread_count(conversation.to_owned());
        let conversation = conversation.to_owned();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            if unread.await.is_ok_and(|unread| unread == 0) {
                // a notifier that is gone has nothing left to close
                let _ = this.update(cx, |this, cx| this.forget(&conversation, cx));
            }
        })
        .detach();
    }

    /// Closes `conversation`'s notifications and starts its count again; a batch taken before shows none of it.
    fn forget(&mut self, conversation: &str, cx: &mut Context<Self>) {
        self.forgets += 1;
        self.forgotten.insert(conversation.to_owned(), self.forgets);
        let Some(notices) = self.shown.remove(conversation) else {
            return;
        };
        let ids: Vec<u32> = notices.ids().collect();
        for id in &ids {
            self.ids.remove(id);
        }
        self.trace
            .record(&TraceLine::new("notifications", "closed").conversation(conversation));
        if let Some(server) = self.server.clone() {
            cx.background_spawn(async move {
                for id in ids {
                    server.close(id).await;
                }
            })
            .detach();
        }
    }

    /// The messages gathered and what decides about them; `None` (and the gathering over) when none are left.
    fn take_batch(&mut self, cx: &mut Context<Self>) -> Option<Batch> {
        let server = self.server.clone().filter(|_| !self.pending.is_empty());
        let Some(server) = server else {
            // nothing left, or nowhere to show it yet
            self.pending.clear();
            self.gathering = false;
            return None;
        };
        let candidates = std::mem::take(&mut self.pending);
        Some(Batch {
            candidates,
            level: self.settings.read(cx).settings().notifications,
            viewing: (self.viewing)(cx),
            cutoff: self.cutoff,
            taken: self.forgets,
            core: Arc::clone(&self.core),
            server,
        })
    }

    /// `notice`, shown one by one as notification `id`.
    fn shown_single(&mut self, notice: &Notice, id: u32, cx: &mut Context<Self>) {
        let (conversation, kind) = (notice.conversation.as_str(), notice.kind);
        let notices = self.shown.entry(conversation.to_owned()).or_default();
        notices.singles.push(id);
        notices.count += 1;
        notices.mentioned |= kind == NoticeKind::Mention;
        self.ids.insert(id, conversation.to_owned());
        self.trace.record(
            &TraceLine::new("notification", "shown")
                .conversation(conversation)
                .kind(kind.name()),
        );
        self.sound(notice, notice.kind == NoticeKind::Mention, cx);
        Self::tell_shown(notice, cx);
    }

    /// Plays the sound of `notice`, a notification just shown.
    fn sound(&self, notice: &Notice, mentioned: bool, cx: &App) {
        let event = if mentioned {
            SoundEvent::Mention
        } else if self.channels.contains(&notice.conversation) {
            SoundEvent::ChannelMessage
        } else {
            SoundEvent::Message
        };
        if let Some(player) = self.settings.read(cx).sound_player() {
            player.play(event, self.core.me(), self.calls.read(cx).call().is_some());
        }
    }

    /// Tells the observers `notice` was shown ([`NotifierEvent::Shown`]).
    fn tell_shown(notice: &Notice, cx: &mut Context<Self>) {
        cx.emit(NotifierEvent::Shown {
            conversation: notice.conversation.clone(),
            title: notice.summary.clone(),
        });
    }

    /// Its chat's notifications collapsed into `notice`, shown as `id`, for `count` messages (`mentioned`: one
    /// mentioned the account); the singles it replaced are gone.
    fn shown_collapsed(&mut self, notice: &Notice, id: u32, count: usize, mentioned: bool, cx: &mut Context<Self>) {
        let conversation = notice.conversation.as_str();
        let notices = self.shown.entry(conversation.to_owned()).or_default();
        for single in notices.singles.drain(..) {
            self.ids.remove(&single);
        }
        if let Some(old) = notices.collapsed.replace(id) {
            self.ids.remove(&old);
        }
        notices.count = count;
        notices.mentioned = mentioned;
        self.ids.insert(id, conversation.to_owned());
        self.trace.record(
            &TraceLine::new("notification", "shown")
                .conversation(conversation)
                .kind(NoticeKind::Collapsed.name()),
        );
        Self::tell_shown(notice, cx);
    }
}

/// The app item `message` (newly stored in the account's feed, brought by `source`) tells, when it may show a banner:
/// one delivered, or a catch-up's or a replayed one unread that arrived after `cutoff`.
fn app_item(message: &Message, source: Source, cutoff: i64) -> Option<ActivityItem> {
    let item = ActivityItem::from_message(message).filter(|item| item.teams_app_id.is_some())?;
    let due = match source {
        Source::Delivery => true,
        Source::CatchUp | Source::Replay => {
            message.arrival_time > cutoff
                && !ActivityFeed::new(std::slice::from_ref(message), None, None).is_read(&item)
        }
        Source::History => false,
    };
    due.then_some(item)
}

/// What an app item's banner says: its title, then its preview (else where it came from, the app).
fn app_notice(item: &ActivityItem) -> Notice {
    let summary = item.title();
    let app = item.app_name.clone().unwrap_or_default();
    let body = if item.preview.is_empty() && app != summary {
        app
    } else {
        item.preview.clone()
    };
    Notice {
        conversation: ACTIVITY_FEED.to_owned(),
        kind: NoticeKind::Activity,
        summary,
        body,
        picture: None,
    }
}

/// App `app`'s tile, only when the engine keeps or caches it already (its definition known this run, its picture
/// kept or on disk): nothing is read.
async fn cached_app_tile(core: &dyn Core, app: &str) -> Option<PictureBytes> {
    let definition = core.cached_app_definition(app.to_owned()).await.ok().flatten()?;
    let url = definition.large_image_url()?.to_owned();
    core.cached_app_picture(url).await.ok().flatten()
}

/// Reads app `app`'s definition and tile in the background, for its next banner: no banner waits for it.
fn read_app_tile(core: &Arc<dyn Core>, app: String, cx: &AsyncApp) {
    let core = Arc::clone(core);
    let definition = core.app_definition(app);
    cx.background_executor()
        .spawn(async move {
            if let Ok(Some(definition)) = definition.await
                && let Some(url) = definition.large_image_url()
            {
                let _ = core.app_picture(url.to_owned()).await;
            }
        })
        .detach();
}

/// Waits [`GATHER`], then processes what was gathered until nothing is left.
async fn gather(this: WeakEntity<Notifier>, cx: &mut AsyncApp) {
    cx.background_executor().timer(GATHER).await;
    while let Ok(Some(batch)) = this.update(cx, Notifier::take_batch) {
        process(&this, batch, cx).await;
    }
}

/// The candidates by chat, each chat's in the order they came, the chats in the order of their first.
fn by_chat(candidates: Vec<Candidate>) -> Vec<(String, Vec<Candidate>)> {
    let mut chats: Vec<(String, Vec<Candidate>)> = Vec::new();
    for candidate in candidates {
        match chats.iter_mut().find(|(chat, _)| *chat == candidate.conversation) {
            Some((_, list)) => list.push(candidate),
            None => chats.push((candidate.conversation.clone(), vec![candidate])),
        }
    }
    chats
}

/// Decides about a batch and shows what notifies (see the module docs).
async fn process(this: &WeakEntity<Notifier>, batch: Batch, cx: &mut AsyncApp) {
    let Batch {
        candidates,
        level,
        viewing,
        cutoff,
        taken,
        core,
        server,
    } = batch;
    if level == Notifications::Off || candidates.is_empty() {
        return;
    }
    let conversations = core.conversations(LIST_LIMIT).await.unwrap_or_default();
    for (conversation, candidates) in by_chat(candidates) {
        if viewing.as_deref() == Some(conversation.as_str()) {
            // read as it comes: what was shown for it is done with
            let _ = this.update(cx, |this, cx| this.forget(&conversation, cx));
            continue;
        }
        let stored = conversations.iter().find(|stored| stored.id == conversation);
        let chosen = choose(&*core, stored, candidates, level, cutoff).await;
        if chosen.is_empty() {
            continue;
        }
        let translated = translations(this, &chosen, cx).await;
        let (chat, notices, pictures) = describe(&*core, &conversation, stored, chosen, translated).await;
        let notices = with_pictures(notices, pictures, cx).await;
        // a 1:1 chat's collapsed banner is about the other person; a group's or a channel's about the chat
        let one_to_one = stored.is_none_or(|stored| stored.kind == ConversationKind::OneToOne);
        let collapsed_picture = notices
            .last()
            .and_then(|notice| notice.picture.clone())
            .filter(|_| one_to_one);
        // asked again after the reads above, right before what is shown is read (nothing waits in between)
        let channel = stored.is_some_and(|stored| stored.kind == ConversationKind::Channel);
        let due = this.update(cx, |this, cx| {
            let due = still_due(this, &conversation, taken, cx);
            if due && channel {
                this.channels.insert(conversation.clone());
            }
            due
        });
        if due.is_ok_and(|due| due) {
            let chat = ChatBanner {
                conversation: &conversation,
                name: &chat,
                picture: collapsed_picture,
            };
            show(this, &*server, chat, notices, cx).await;
        }
    }
}

/// Whether `conversation`'s notices of a batch taken at `taken` may still be shown: it isn't shown in a focused window
/// now (if it is, what was shown for it is done with), and it wasn't opened or read since the batch was taken.
fn still_due(this: &mut Notifier, conversation: &str, taken: u64, cx: &mut Context<Notifier>) -> bool {
    if (this.viewing)(cx).as_deref() == Some(conversation) {
        this.forget(conversation, cx);
        return false;
    }
    this.forgotten.get(conversation).is_none_or(|at| *at <= taken)
}

/// The candidates of one chat that notify, each with whether it mentions the account.
async fn choose(
    core: &dyn Core,
    stored: Option<&Conversation>,
    candidates: Vec<Candidate>,
    level: Notifications,
    cutoff: i64,
) -> Vec<(Candidate, bool)> {
    let channel = stored.is_some_and(|stored| stored.kind == ConversationKind::Channel);
    let channel_preferences = if channel {
        let id = candidates
            .first()
            .map(|candidate| candidate.conversation.clone())
            .unwrap_or_default();
        let read = core
            .channel_data(teams_proto::ChannelRead::Preferences { channel: id })
            .await;
        match read {
            Ok(data) => Some(crate::channel_management::ChannelNotifications::read(&data.0)),
            // An unverified preference must not produce a banner while the channel may be muted elsewhere.
            Err(_) => return Vec::new(),
        }
    } else {
        None
    };
    if channel_preferences.as_ref().is_some_and(|settings| settings.muted) {
        return Vec::new();
    }
    let roots = if channel_preferences
        .as_ref()
        .is_some_and(|settings| settings.banners_posts() || settings.banners_replies())
    {
        let id = candidates
            .first()
            .map(|candidate| candidate.conversation.clone())
            .unwrap_or_default();
        core.reply_roots(id).await.unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut horizon = None;
    let mut chosen = Vec::new();
    for candidate in candidates {
        let mention = Mentions::of_message(&candidate.message, core.me()).names_me();
        let is_reply = roots.iter().any(|(id, root)| id == &candidate.message.id && id != root);
        let channel_all = channel_preferences.as_ref().is_some_and(|settings| {
            if is_reply {
                settings.banners_replies()
            } else {
                settings.banners_posts()
            }
        });
        if (level == Notifications::Mentions || channel && !channel_all) && !mention {
            continue;
        }
        if matches!(candidate.source, Source::CatchUp | Source::Replay) {
            let arrival = candidate.message.arrival_time;
            if arrival <= cutoff {
                continue;
            }
            if horizon.is_none() {
                let read = core.read_horizon(candidate.conversation.clone()).await;
                horizon = Some(read.ok().flatten().map(|horizon| horizon.last_read_arrival));
            }
            if horizon.flatten().is_some_and(|read| arrival <= read) {
                continue;
            }
        }
        chosen.push((candidate, mention));
    }
    chosen
}

/// The chosen messages' text in their chat's language, when it is translated: each waited for at most
/// [`NOTIFY_BUDGET`], all at once; `None` for one shown as written (its chat isn't translated, or its translation
/// didn't come in time: it is shown once, as written, and never again).
async fn translations(
    this: &WeakEntity<Notifier>,
    chosen: &[(Candidate, bool)],
    cx: &mut AsyncApp,
) -> Vec<Option<Arc<RichText>>> {
    let tasks = this.update(cx, |this, cx| {
        this.previews.update(cx, |previews, cx| {
            chosen
                .iter()
                .map(|(candidate, _)| previews.translated_within(candidate.message.clone(), NOTIFY_BUDGET, cx))
                .collect::<Vec<_>>()
        })
    });
    match tasks {
        Ok(tasks) => futures_util::future::join_all(tasks).await,
        Err(_) => vec![None; chosen.len()],
    }
}

/// The chat's name and the notices for its messages that notify, each with its text in the chat's language when
/// `translated` has it, and their senders' stored pictures (the notices have none yet: [`with_pictures`]).
async fn describe(
    core: &dyn Core,
    conversation: &str,
    stored: Option<&Conversation>,
    chosen: Vec<(Candidate, bool)>,
    translated: Vec<Option<Arc<RichText>>>,
) -> (SharedString, Vec<Notice>, Vec<Option<StoredPicture>>) {
    let one_to_one = stored.is_none_or(|stored| stored.kind == ConversationKind::OneToOne);
    let chat: SharedString = match stored {
        Some(stored) => chat_list::chat_name(core, stored).await,
        None => "Chat".into(),
    };
    let mut notices = Vec::with_capacity(chosen.len());
    let mut pictures = Vec::with_capacity(chosen.len());
    let mut translated = translated.into_iter();
    for (candidate, mention) in chosen {
        let text = translated.next().flatten();
        let (sender, picture) = sender_of(core, &candidate.message).await;
        pictures.push(picture);
        let summary = match (one_to_one, mention) {
            (true, false) => sender,
            (true, true) => format!("{sender} mentioned you"),
            (false, false) => format!("{sender} in {chat}"),
            (false, true) => format!("{sender} mentioned you in {chat}"),
        };
        notices.push(Notice {
            conversation: conversation.to_owned(),
            kind: if mention {
                NoticeKind::Mention
            } else {
                NoticeKind::Message
            },
            summary,
            body: text.map_or_else(
                || chat_list::message_line(&candidate.message),
                |text| chat_list::text_line(&text),
            ),
            picture: None,
        });
    }
    (chat, notices, pictures)
}

/// `notices` with the pictures of `pictures` (one each, in order), decoded off the main thread.
async fn with_pictures(
    mut notices: Vec<Notice>,
    pictures: Vec<Option<StoredPicture>>,
    cx: &mut AsyncApp,
) -> Vec<Notice> {
    if pictures.iter().all(Option::is_none) {
        return notices;
    }
    let decoded = cx
        .background_executor()
        .spawn(async move {
            pictures
                .iter()
                .map(|picture| picture.as_ref().and_then(BannerPicture::of_person))
                .collect::<Vec<_>>()
        })
        .await;
    for (notice, picture) in notices.iter_mut().zip(decoded) {
        notice.picture = picture;
    }
    notices
}

/// A sender's name (as the engine has it, else as the message carried it, else [`SOMEONE`]) and their picture as the
/// engine has it stored.
async fn sender_of(core: &dyn Core, message: &Message) -> (String, Option<StoredPicture>) {
    let carried = || {
        message
            .sender_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(SOMEONE)
            .to_owned()
    };
    let Some(sender) = message.sender.clone() else {
        return (SOMEONE.to_owned(), None);
    };
    let Ok(person) = core.person(sender).await else {
        return (carried(), None);
    };
    let picture = person.picture.clone();
    let name = Shown::of(person).name.map_or_else(carried, |name| name.to_string());
    (name, picture)
}

/// What a collapsed notification of `count` messages says.
#[must_use]
pub fn collapsed_body(count: usize, mention: bool) -> String {
    if mention {
        format!("{count} new messages, one or more mentioning you")
    } else {
        format!("{count} new messages")
    }
}

/// A chat as its collapsed banner shows it: its id, its name and the picture it shows.
struct ChatBanner<'a> {
    conversation: &'a str,
    name: &'a str,
    picture: Option<BannerPicture>,
}

/// Shows one chat's notices: one by one while the chat has at most [`SINGLES`], else collapsed into one.
async fn show(
    this: &WeakEntity<Notifier>,
    server: &dyn NotificationServer,
    chat: ChatBanner<'_>,
    notices: Vec<Notice>,
    cx: &mut AsyncApp,
) {
    let conversation = chat.conversation;
    let Ok((count, singles, collapsed, mentioned)) = this.update(cx, |this, _| {
        this.shown
            .get(conversation)
            .map_or((0, Vec::new(), None, false), |notices| {
                (
                    notices.count,
                    notices.singles.clone(),
                    notices.collapsed,
                    notices.mentioned,
                )
            })
    }) else {
        return;
    };
    let total = count + notices.len();
    if total <= SINGLES {
        for notice in notices {
            if let Ok(id) = server.show(&notice, 0).await {
                let _ = this.update(cx, |this, cx| this.shown_single(&notice, id, cx));
            }
        }
        return;
    }
    for id in singles {
        server.close(id).await;
    }
    // a mention shown before, one by one or in the collapsed one this replaces, is still among the messages
    let batch_mentions = notices.iter().any(|notice| notice.kind == NoticeKind::Mention);
    let mention = mentioned || batch_mentions;
    let notice = Notice {
        conversation: conversation.to_owned(),
        kind: NoticeKind::Collapsed,
        summary: chat.name.to_owned(),
        body: collapsed_body(total, mention),
        picture: chat.picture,
    };
    if let Ok(id) = server.show(&notice, collapsed.unwrap_or(0)).await {
        let _ = this.update(cx, |this, cx| {
            this.shown_collapsed(&notice, id, total, mention, cx);
            this.sound(&notice, batch_mentions, cx);
        });
    }
}

/// Text for a server that reads markup in a body (`body-markup`): `&`, `<` and `>` as entities.
#[must_use]
pub fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_escaped() {
        assert_eq!(escape_markup("a <b> & c"), "a &lt;b&gt; &amp; c");
    }

    #[test]
    fn a_collapsed_notification_counts_its_messages() {
        assert_eq!(collapsed_body(4, false), "4 new messages");
        assert_eq!(collapsed_body(5, true), "5 new messages, one or more mentioning you");
    }
}

/// The call window's More → Settings → Mute notifications: no message notifies while the call it was asked in runs
/// (the call window takes it back when the call ends).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MutedInCall(pub bool);

impl gpui::Global for MutedInCall {}

/// Whether a call's Mute notifications holds the notifications back now.
#[must_use]
pub fn muted_in_call(cx: &App) -> bool {
    cx.try_global::<MutedInCall>().is_some_and(|muted| muted.0)
}

/// Holds the notifications back while a call runs (`muted`), or lets them through again.
pub fn mute_in_call(muted: bool, cx: &mut App) {
    if muted_in_call(cx) != muted {
        cx.set_global(MutedInCall(muted));
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{BusNotifications, IMAGE_DATA, image_data};
