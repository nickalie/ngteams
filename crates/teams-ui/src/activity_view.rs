//! The Activity pane, as Teams shows it in the list pane when the rail's Activity is chosen
//! ([`teams_core::activity`] tells where its items come from and when they read as read).
//!
//! - **Header**: "Activity", then "More options" (its menu: "Mark all as read", Shift+Esc as in Teams) and "Filter"
//!   (the search's box, "Filter by name or group name", which keeps the items whose title, preview or place has every
//!   word typed; Esc closes it).
//! - **Filter chips** ([`ActivityFilter::CHIPS`]): Unread, @Mentions, Reactions, Missed calls, Replies, Voicemail and
//!   Apps; one is chosen at a time, chosen again it lets go. Ctrl+Alt+U and Ctrl+Alt+M toggle the first two, as Teams'.
//! - **An item**: the person's avatar (their picture, else initials; an app's tile) with a badge for its kind (the
//!   reaction's emoji, "@" for a mention, people for a group mention or a team, a missed call's phone, a meeting's
//!   calendar), its title (semibold while unread, a dot at the left edge), its date (the time today), the preview and
//!   where it happened (a channel's team and name, a chat's name, "In chat with you" for a 1:1 chat). Hovered, the
//!   date gives way to "More options": Mark as read or unread, and Remove (a toast says "Removed"). A missed call
//!   offers Call and Chat with the caller.
//! - **Opening an item** (a click, or Enter on the item the keyboard is on) marks it read and opens its conversation
//!   at its message ([`ActivityViewEvent::Open`]).
//! - **Seen**: while the pane is shown, the feed's horizon follows its newest item ([`Action::ActivitySeen`]), which
//!   clears the rail's count ([`ActivityView::unseen`]).
//! - **Live**: the pane reads the feed again whenever the engine tells a change to it (a new item, a read state that
//!   synced, the horizon), shown or not, so the rail's count stays current.
//! - **Read-only** ([`Core::read_only`]): nothing is marked, removed or seen; items still open.
//!
//! Traced: `activity opened` (the item's kind), `activity filter` (the chip's key), `activity marked` (read or
//! unread), `activity removed`, `activity all-read`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Datelike as _, TimeZone};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, ParentElement as _, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, actions,
    deferred, div, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Selectable as _, Sizable as _};
use teams_core::activity::{ActivityFeed, ActivityFilter, ActivityItem, ActivityKind, FEED};
use teams_core::{Action, EngineError};

use crate::bridge::Core;
use crate::chat_list::ChatList;
use crate::clock::HourCycle;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::models::Models;
use crate::people::{self, Picture};
use crate::search::HitTarget;
use crate::teams_list::TeamsList;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

actions!(
    activity,
    [
        /// Teams' Shift+Esc: every item marked read.
        MarkAllRead,
        /// Teams' Ctrl+Alt+U: the Unread chip.
        ToggleUnread,
        /// Teams' Ctrl+Alt+M: the @Mentions chip.
        ToggleMentions
    ]
);

/// The pane's key context.
pub const CONTEXT: &str = "Activity";
/// The header's title.
pub const TITLE: &str = "Activity";
/// The search box's hint, Teams' words.
pub const FILTER_HINT: &str = "Filter by name or group name";
/// What the toast says once an item is removed.
pub const REMOVED: &str = "Removed";
/// A 1:1 chat's place, Teams' words.
pub const IN_CHAT_WITH_YOU: &str = "In chat with you";
/// An item's avatar, px.
const AVATAR: f32 = 32.0;
/// Its kind's badge, px.
const BADGE: f32 = 16.0;

/// Binds the pane's keys, once per app.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("shift-escape", MarkAllRead, Some(CONTEXT)),
        KeyBinding::new("ctrl-alt-u", ToggleUnread, Some(CONTEXT)),
        KeyBinding::new("ctrl-alt-m", ToggleMentions, Some(CONTEXT)),
    ]);
}

/// What the pane asks its shell for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityViewEvent {
    /// Open a conversation at a message.
    Open(HitTarget),
    /// Open the 1:1 chat with someone (a missed call's Chat).
    ChatWith(String),
    /// Call someone back (a missed call's Call).
    CallBack(String),
    /// Show a short note (a toast).
    Toast(SharedString),
}

/// The pane (see the module docs).
pub struct ActivityView {
    core: Arc<dyn Core>,
    models: Models,
    chats: Entity<ChatList>,
    teams: Entity<TeamsList>,
    trace: Arc<ViewTrace>,
    feed: ActivityFeed,
    /// The feed's change the shown items were read at.
    read_at: Option<u64>,
    loading: Option<Task<()>>,
    dirty: bool,
    loads: u64,
    filter: ActivityFilter,
    search: Entity<InputState>,
    searching: bool,
    /// The item the keyboard or the last click is on.
    selected: Option<String>,
    /// Items marked here whose change hasn't come back yet: the read state asked for, at the version then shown.
    pending: HashMap<String, (bool, i64)>,
    /// Items removed here, hidden until the store drops them.
    removed: Vec<String>,
    /// People's pictures, by MRI: `None` while asked or when they have none.
    pictures: HashMap<String, Option<Picture>>,
    /// An item's "More options" menu, while open.
    item_menu: Option<String>,
    /// The header's "More options" menu, while open.
    header_menu: bool,
    shown: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ActivityViewEvent> for ActivityView {}

impl Focusable for ActivityView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// What the pane needs to be made.
pub struct ActivityParts {
    pub core: Arc<dyn Core>,
    pub models: Models,
    pub chats: Entity<ChatList>,
    pub teams: Entity<TeamsList>,
    pub trace: Arc<ViewTrace>,
}

impl ActivityView {
    pub fn new(parts: ActivityParts, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(FILTER_HINT));
        let searched = cx.subscribe_in(&search, window, |this: &mut Self, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.selected = None;
                cx.notify();
            }
        });
        let changed = cx.observe(&parts.models.chats, |this: &mut Self, chats, cx| {
            let at = chats.read(cx).changed_at(FEED);
            if this.read_at != Some(at) {
                this.reload(cx);
            }
        });
        // a chat's or a channel's name, as the places show them
        let named = cx.observe(&parts.chats, |_, _, cx| cx.notify());
        let teamed = cx.observe(&parts.teams, |_, _, cx| cx.notify());
        let mut view = Self {
            core: parts.core,
            models: parts.models,
            chats: parts.chats,
            teams: parts.teams,
            trace: parts.trace,
            feed: ActivityFeed::default(),
            read_at: None,
            loading: None,
            dirty: false,
            loads: 0,
            filter: ActivityFilter::All,
            search,
            searching: false,
            selected: None,
            pending: HashMap::new(),
            removed: Vec::new(),
            pictures: HashMap::new(),
            item_menu: None,
            header_menu: false,
            shown: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _subscriptions: vec![searched, changed, named, teamed],
        };
        view.reload(cx);
        view
    }

    /// The feed as last read.
    #[must_use]
    pub fn feed(&self) -> &ActivityFeed {
        &self.feed
    }

    /// Reads the feed done so far.
    #[must_use]
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// How many unread items came since the pane was last shown: the rail's count.
    #[must_use]
    pub fn unseen(&self) -> usize {
        if self.shown {
            return 0;
        }
        self.feed.unseen()
    }

    /// The filter chosen.
    #[must_use]
    pub fn filter(&self) -> ActivityFilter {
        self.filter
    }

    /// Whether `item` reads as read here: as marked here until the change comes back, else as the feed says.
    #[must_use]
    pub fn is_read(&self, item: &ActivityItem) -> bool {
        match self.pending.get(&item.id) {
            Some((read, version)) if *version == item.version => *read,
            _ => self.feed.is_read(item),
        }
    }

    /// The items shown: the filter's and the search's, without those removed here.
    #[must_use]
    pub fn shown_items(&self, cx: &App) -> Vec<&ActivityItem> {
        let words = if self.searching {
            self.search.read(cx).value().to_string()
        } else {
            String::new()
        };
        self.feed
            .items
            .iter()
            .filter(|item| !self.removed.contains(&item.id))
            .filter(|item| match self.filter {
                ActivityFilter::Unread => !self.is_read(item),
                filter => self.feed.keeps(filter, item),
            })
            .filter(|item| item.matches(&words) || place_matches(&self.place(item, cx), &words))
            .collect()
    }

    /// Reads the feed again (a read under way reads once more after it).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        self.read_at = Some(self.models.chats.read(cx).changed_at(FEED));
        let reply = self.core.activity();
        self.loading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let feed = reply.await;
            let _ = this.update(cx, |view, cx| view.loaded(feed, cx));
        }));
    }

    fn loaded(&mut self, feed: Result<ActivityFeed, EngineError>, cx: &mut Context<Self>) {
        self.loading = None;
        self.loads += 1;
        if let Ok(feed) = feed {
            // a change that came back replaces what was marked here
            self.pending
                .retain(|id, (_, version)| feed.item(id).is_some_and(|item| item.version == *version));
            self.removed.retain(|id| feed.item(id).is_some());
            self.feed = feed;
            self.ask_pictures(cx);
            if self.shown {
                self.mark_seen(cx);
            }
        }
        cx.notify();
        if self.dirty {
            self.reload(cx);
        }
    }

    /// The pane shown (the rail's Activity) or left.
    pub fn set_shown(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        self.item_menu = None;
        self.header_menu = false;
        if shown {
            self.mark_seen(cx);
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Whether the pane is shown.
    #[must_use]
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// Moves the feed's horizon to its newest item, when it isn't there.
    fn mark_seen(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() || self.feed.seen_horizon().is_none() {
            return;
        }
        // shown at once; the store's horizon follows as the engine queues it
        self.feed.seen_ms = self.feed.items.first().map(|newest| newest.at_ms);
        self.write(Action::ActivitySeen, cx);
    }

    fn write(&self, action: Action, cx: &mut Context<Self>) {
        let reply = self.core.write(action);
        cx.background_spawn(async move {
            let _ = reply.await;
        })
        .detach();
    }

    /// Chooses a chip; the chosen one again lets it go.
    pub fn choose_filter(&mut self, filter: ActivityFilter, cx: &mut Context<Self>) {
        self.filter = if self.filter == filter {
            ActivityFilter::All
        } else {
            filter
        };
        self.selected = None;
        self.trace
            .record(&TraceLine::new("activity", "filter").kind(self.filter.key()));
        cx.notify();
    }

    /// Opens or closes the search's box (closing clears it).
    pub fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.searching = !self.searching;
        if self.searching {
            self.search.update(cx, |search, cx| search.focus(window, cx));
        } else {
            self.search.update(cx, |search, cx| search.set_value("", window, cx));
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Opens an item's "More options" menu (`None` closes it), as its button does.
    pub fn show_item_menu(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.item_menu = id;
        self.header_menu = false;
        cx.notify();
    }

    /// Opens or closes the header's "More options" menu, as its button does.
    pub fn show_header_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        self.header_menu = open;
        self.item_menu = None;
        cx.notify();
    }

    /// Whether the header's menu is open, and the item whose menu is.
    #[must_use]
    pub fn menus(&self) -> (bool, Option<&str>) {
        (self.header_menu, self.item_menu.as_deref())
    }

    /// The search's box opened with `words` in it (closed, and cleared, for none).
    pub fn search_for(&mut self, words: &str, window: &mut Window, cx: &mut Context<Self>) {
        if words.is_empty() == self.searching {
            self.toggle_search(window, cx);
        }
        if self.searching {
            self.search
                .update(cx, |search, cx| search.set_value(words.to_owned(), window, cx));
        }
        cx.notify();
    }

    /// Marks an item read or unread (every feed message a folded item stands for).
    pub fn mark(&mut self, id: &str, read: bool, cx: &mut Context<Self>) {
        self.item_menu = None;
        let Some(item) = self.feed.item(id).cloned() else {
            return;
        };
        if self.core.read_only() {
            return;
        }
        self.pending.insert(item.id.clone(), (read, item.version));
        self.trace
            .record(&TraceLine::new("activity", "marked").kind(if read { "read" } else { "unread" }));
        for own in item.ids {
            self.write(Action::ActivityRead { item: own, read }, cx);
        }
        cx.notify();
    }

    /// Removes an item from the feed, as Teams' Remove does.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        self.item_menu = None;
        let Some(item) = self.feed.item(id).cloned() else {
            return;
        };
        if self.core.read_only() {
            return;
        }
        self.removed.push(item.id.clone());
        self.trace.record(&TraceLine::new("activity", "removed"));
        for own in item.ids {
            self.write(
                Action::Delete {
                    conversation: FEED.to_owned(),
                    message_id: own,
                },
                cx,
            );
        }
        cx.emit(ActivityViewEvent::Toast(REMOVED.into()));
        cx.notify();
    }

    /// Every item marked read ("Mark all as read").
    pub fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        self.header_menu = false;
        if self.core.read_only() {
            cx.notify();
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        self.feed.bookmark_ms = Some(self.feed.bookmark_ms.map_or(now, |bookmark| bookmark.max(now)));
        self.pending.clear();
        self.trace.record(&TraceLine::new("activity", "all-read"));
        self.write(Action::ActivityAllRead, cx);
        cx.notify();
    }

    /// Opens an item: marked read, its conversation opened at its message.
    pub fn open(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(item) = self.feed.item(id).cloned() else {
            return;
        };
        self.selected = Some(item.id.clone());
        self.item_menu = None;
        self.trace
            .record(&TraceLine::new("activity", "opened").kind(kind_key(&item.kind)));
        if !self.is_read(&item) {
            self.mark(&item.id, true, cx);
        }
        if let Some(target) = target(&item) {
            cx.emit(ActivityViewEvent::Open(target));
        } else if let (ActivityKind::MissedCall | ActivityKind::Voicemail, Some(actor)) = (&item.kind, &item.actor) {
            cx.emit(ActivityViewEvent::ChatWith(actor.clone()));
        }
        cx.notify();
    }

    /// Where an item happened, as its third line says it.
    #[must_use]
    pub fn place(&self, item: &ActivityItem, cx: &App) -> String {
        if matches!(item.kind, ActivityKind::MissedCall | ActivityKind::Voicemail) {
            return "Teams call".to_owned();
        }
        let Some(thread) = item.thread.as_deref() else {
            return item.topic.clone().unwrap_or_default();
        };
        if item.in_channel() {
            if let Some(chosen) = self.teams.read(cx).chosen(thread) {
                return if chosen.team.is_empty() || chosen.team == chosen.name {
                    chosen.name.to_string()
                } else {
                    format!("{} › {}", chosen.team, chosen.name)
                };
            }
            return item.topic.clone().unwrap_or_default();
        }
        if thread.ends_with("@unq.gbl.spaces") {
            return IN_CHAT_WITH_YOU.to_owned();
        }
        self.chats
            .read(cx)
            .row(thread)
            .map(|row| row.name.to_string())
            .or_else(|| item.topic.clone())
            .unwrap_or_default()
    }

    /// Asks for the pictures of the people the items name, once each.
    fn ask_pictures(&mut self, cx: &mut Context<Self>) {
        let asked: Vec<String> = self
            .feed
            .items
            .iter()
            .filter_map(|item| item.actor.clone())
            .filter(|actor| actor.starts_with("8:") && !self.pictures.contains_key(actor))
            .collect();
        for mri in asked {
            self.pictures.insert(mri.clone(), None);
            let reply = self.core.person(mri.clone());
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let Ok(person) = reply.await else { return };
                let picture = person.picture.as_ref().and_then(people::image_of);
                let _ = this.update(cx, |view, cx| {
                    view.pictures.insert(mri, picture);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// The keyboard's keys over the items: Up, Down, Home, End and Enter.
    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.shown_items(cx).iter().map(|item| item.id.clone()).collect();
        let at = self
            .selected
            .as_ref()
            .and_then(|selected| ids.iter().position(|id| id == selected));
        let next = match event.keystroke.key.as_str() {
            "down" => at.map_or(0, |at| (at + 1).min(ids.len().saturating_sub(1))),
            "up" => at.map_or(0, |at| at.saturating_sub(1)),
            "home" => 0,
            "end" => ids.len().saturating_sub(1),
            "enter" => {
                if let Some(id) = self.selected.clone() {
                    self.open(&id, cx);
                }
                return;
            }
            _ => return,
        };
        if let Some(id) = ids.get(next) {
            self.selected = Some(id.clone());
            self.scroll.scroll_to_item(next);
            cx.stop_propagation();
            cx.notify();
        }
    }
}

/// Whether a place's name holds the search's words, case and spaces folded.
fn place_matches(place: &str, words: &str) -> bool {
    let fold = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let words = fold(words);
    !words.is_empty() && fold(place).contains(&words)
}

/// Where an item opens: its message in its thread (a channel's at its post's thread); `None` when it names none.
fn target(item: &ActivityItem) -> Option<HitTarget> {
    let conversation = item.thread.clone()?;
    let message = item.message.clone().or_else(|| item.reply_chain.clone())?;
    let arrival_time = message.parse().unwrap_or(item.at_ms);
    Some(HitTarget {
        conversation,
        message,
        arrival_time,
        thread: item.reply_chain.clone(),
    })
}

/// A kind's key, for the trace.
fn kind_key(kind: &ActivityKind) -> &'static str {
    match kind {
        ActivityKind::Mention(_) => "mention",
        ActivityKind::Reaction(_) => "reaction",
        ActivityKind::Reply => "reply",
        ActivityKind::MissedCall => "missed-call",
        ActivityKind::Voicemail => "voicemail",
        ActivityKind::AddedToTeam => "added-to-team",
        ActivityKind::Meeting(_) => "meeting",
        ActivityKind::App => "app",
        ActivityKind::Other => "other",
    }
}

/// A reaction's emoji as text, by Teams' keys (another key shows a like's): where a picture can't go, such as a
/// line of `ngteamsctl`.
#[must_use]
pub fn reaction_emoji(key: &str) -> &'static str {
    match key {
        "heart" => "❤️",
        "laugh" => "😆",
        "surprised" => "😮",
        "sad" => "😢",
        "angry" => "😡",
        _ => "👍",
    }
}

/// An item's date: the time of day today, else the month and day (and the year, if not this one), as Teams' feed.
#[must_use]
pub fn activity_time<Tz: TimeZone>(ms: i64, now: &DateTime<Tz>, cycle: HourCycle) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(at) = DateTime::from_timestamp_millis(ms) else {
        return String::new();
    };
    let at = at.with_timezone(&now.timezone());
    let format = if at.date_naive() == now.date_naive() {
        cycle.time_of_day()
    } else if at.year() == now.year() {
        "%-m/%-d"
    } else {
        "%-m/%-d/%y"
    };
    at.format(format).to_string()
}

// --- drawing ---

impl ActivityView {
    fn header(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let more = selected_by(
            "activity-more",
            icon_button("activity-more", Glyph::ELLIPSIS, "More options")
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.header_menu = !this.header_menu;
                    cx.notify();
                })),
        );
        let filter = selected_by(
            "activity-search",
            icon_button("activity-search", Glyph::SEARCH, "Filter")
                .small()
                .selected(self.searching)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_search(window, cx))),
        );
        div()
            .debug_selector(|| "activity-header".to_owned())
            .relative()
            .h(px(48.0))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex_1()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(TITLE),
            )
            .child(more)
            .child(filter)
            .children(self.header_menu.then(|| self.header_menu_view(palette, cx)))
    }

    fn header_menu_view(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let read_only = self.core.read_only();
        let row = crate::shell::menu_row("activity-mark-all-read", palette)
            .when(read_only, |row| row.opacity(0.5))
            .child(Glyph::EYE.icon().size(px(14.0)))
            .child(div().flex_1().child("Mark all as read"))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Shift+Esc"),
            )
            .on_click(cx.listener(|this, _, _, cx| this.mark_all_read(cx)));
        deferred(
            menu_surface("activity-header-menu", palette)
                .top(px(40.0))
                .right(px(40.0))
                .w(px(240.0))
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.header_menu = false;
                    cx.notify();
                }))
                .child(row),
        )
        .with_priority(1)
        .into_any_element()
    }

    fn search_box(&self, palette: Palette) -> Option<impl IntoElement> {
        self.searching.then(|| {
            div()
                .debug_selector(|| "activity-filter-box".to_owned())
                .mx_3()
                .mt_2()
                .px_2()
                .h(px(32.0))
                .flex()
                .items_center()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.brand))
                .bg(rgb(palette.background))
                .child(
                    Input::new(&self.search)
                        .appearance(false)
                        .cleanable(true)
                        .aria_label(FILTER_HINT),
                )
        })
    }

    fn chips(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let chips: Vec<AnyElement> = ActivityFilter::CHIPS
            .iter()
            .map(|filter| {
                let filter = *filter;
                let chosen = self.filter == filter;
                div()
                    .debug_selector(move || format!("activity-chip-{}", filter.key()))
                    .id(ElementId::Name(format!("activity-chip-{}", filter.key()).into()))
                    .px_2()
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .text_xs()
                    .cursor_pointer()
                    .when(chosen, |chip| {
                        chip.bg(rgb(palette.brand))
                            .border_color(rgb(palette.brand))
                            .text_color(rgb(palette.on_brand))
                    })
                    .when(!chosen, |chip| {
                        chip.border_color(rgb(palette.border))
                            .hover(move |chip| chip.bg(rgb(palette.hover)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_filter(filter, cx)))
                    .child(filter.label())
                    .into_any_element()
            })
            .collect();
        div()
            .debug_selector(|| "activity-chips".to_owned())
            .px_3()
            .py_2()
            .flex()
            .flex_wrap()
            .gap_1p5()
            .children(chips)
    }

    fn list(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let items: Vec<ActivityItem> = self.shown_items(cx).into_iter().cloned().collect();
        if items.is_empty() {
            return self.empty(palette).into_any_element();
        }
        let rows: Vec<AnyElement> = items.iter().map(|item| self.row(item, palette, cx)).collect();
        div()
            .debug_selector(|| "activity-list".to_owned())
            .id("activity-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px_1()
            .pb_2()
            .children(rows)
            .into_any_element()
    }

    fn empty(&self, palette: Palette) -> impl IntoElement {
        let (title, words) = match (self.filter, self.searching) {
            (_, true) => ("Nothing found", "No activity matches what you typed."),
            (ActivityFilter::All, false) => (
                "No activity yet",
                "Mentions, reactions, replies and missed calls show up here.",
            ),
            (ActivityFilter::Unread, false) => ("You're all caught up", "There's no unread activity."),
            (_, false) => ("Nothing here", "No activity of this kind yet."),
        };
        div()
            .debug_selector(|| "activity-empty".to_owned())
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .child(
                div()
                    .size(px(56.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(palette.brand).opacity(0.15))
                    .text_color(rgb(palette.brand))
                    .child(Glyph::BELL.icon().size(px(24.0))),
            )
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(
                div()
                    .text_sm()
                    .text_center()
                    .text_color(rgb(palette.muted_foreground))
                    .child(words),
            )
    }
}

impl Render for ActivityView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        div()
            .debug_selector(|| "activity-pane".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .on_action(cx.listener(|this, _: &MarkAllRead, _, cx| this.mark_all_read(cx)))
            .on_action(cx.listener(|this, _: &ToggleUnread, _, cx| this.choose_filter(ActivityFilter::Unread, cx)))
            .on_action(cx.listener(|this, _: &ToggleMentions, _, cx| {
                this.choose_filter(ActivityFilter::Mentions, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .child(self.header(palette, cx))
            .children(self.search_box(palette))
            .child(self.chips(palette, cx))
            .child(self.list(palette, cx))
    }
}

/// A raised menu's surface, placed by its caller.
fn menu_surface(id: &'static str, palette: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .debug_selector(move || id.to_owned())
        .id(id)
        .absolute()
        .p_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .bg(rgb(palette.surface))
        .border_1()
        .border_color(rgb(palette.border))
        .rounded_lg()
        .shadow_lg()
        .occlude()
}

mod row;
