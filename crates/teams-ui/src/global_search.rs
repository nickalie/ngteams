//! The app's one search, in the title bar as Teams has it (`docs/protocol/search.md`).
//!
//! - **The box** ([`GlobalSearch`]'s own view): Ctrl+E (and Ctrl+Shift+F) focuses it to search; Ctrl+Alt+G focuses it
//!   in its "go to" mode (a `/goto` mark before the text), which goes right to a chat or a channel by its name. A click
//!   on it searches too.
//! - **The suggestions** drop down under it as the user types ([`GlobalSearch::suggestions`]): with nothing typed, the
//!   recent chats; else the people (the 1:1 chats' partners the list knows, at once, and the people the service
//!   suggests once it answers), the chats and the teams' channels the lists know (matched as the switcher matches them,
//!   instantly) and the group chats the service suggests, the files it suggests, then "Press Enter to view all
//!   results". Above them, while searching, chips go straight to the results of one kind. Up and Down move through
//!   them, Enter opens the one highlighted (a chat, a channel, a person's 1:1 chat, a file in the browser), or with
//!   none highlighted shows the results; the go-to mode highlights the first and has no results.
//! - **The results** ([`GlobalSearch::results`]) take the place of the list and the conversation, as Teams' do, with
//!   its tabs ([`Tab`]): All (a few of each kind), Messages (the service's and the store's, the newest first), People,
//!   Files, Group chats, Teams and channels. A message opens its chat at the message; a person their 1:1 chat
//!   (created through the outbox when needed); a file in the browser (GPUI's `open_url`); a chat or a channel itself.
//! - **Escape** in the box closes the suggestions and gives the keyboard back; on the results it leaves them.
//! - **A translated chat's message** ([`crate::chat_translate`]) shows in the chat's language on its card, once its
//!   translation is known ([`Previews`]: the cards shown ask for it), the query's words marked where they are in it.
//! - Nothing typed is ever sent anywhere but the account's own search service (a read: `teams_core::service_search`)
//!   and its store. What the search shows is traced: `search opened` (its mode), `search suggested` (the chats,
//!   channels and people suggested), `search results` (the tab), `search shown` (the messages the results show, with
//!   their conversations), `search closed`; never a query or a text.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    FontWeight, HighlightStyle, InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task, Window,
    actions, div, prelude::FluentBuilder as _, px, rgb, rgba,
};
use gpui_component::Size;
use gpui_component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::kbd::Kbd;
use teams_core::{Action, SearchAnswer, SearchScope};
use teams_proto::Mri;
use teams_proto::{FoundFile, FoundMessage, FoundPerson, SearchKind};
use teams_store::{Message, OutboxState};

use crate::bridge::Core;
use crate::chat_list::{ChatList, RowKind};
use crate::chat_translate::{Lookup, Previews};
use crate::clock::{self, HourCycle};
use crate::icons::Glyph;
use crate::people::{self, Picture};
use crate::profile_card::{self, CardAsks, CardOf, Click};
use crate::search::{self, Hit, HitTarget};
use crate::sending::settled_row;
use crate::switcher::matches;
use crate::teams_list::TeamsList;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

actions!(teams_ui, [LeaveSearchResults]);

/// The box's hint, as Teams' says it.
pub const PLACEHOLDER: &str = concat!("Press ", ctrl!(), "+Alt+G to go right to a chat or channel");
/// The box's hint in its go-to mode.
pub const GOTO_PLACEHOLDER: &str = "Go to a chat or channel";
/// The results page's key context (Escape leaves it).
pub const RESULTS_CONTEXT: &str = "SearchResults";
/// How long typing rests before the service is asked for suggestions.
pub const SUGGEST_DELAY: Duration = Duration::from_millis(200);
/// The most of each kind the suggestions show.
const SUGGESTED_PEOPLE: usize = 4;
const SUGGESTED_CHATS: usize = 4;
const SUGGESTED_CHANNELS: usize = 3;
const SUGGESTED_FILES: usize = 3;
/// The recent chats shown with nothing typed, and the most the go-to mode lists.
const RECENT: usize = 6;
const GOTO_MAX: usize = 8;
/// The most of a kind the All tab shows.
const ALL_EACH: usize = 3;
/// The All tab's messages.
const ALL_MESSAGES: usize = 5;

/// Binds the results page's Escape, once per app (with the shell's keys).
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", LeaveSearchResults, Some(RESULTS_CONTEXT))]);
}

/// Searching, or going right to a chat or a channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Search,
    GoTo,
}

/// A tab of the results, as Teams names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    All,
    Messages,
    People,
    Files,
    GroupChats,
    Channels,
}

impl Tab {
    pub const ALL: [Self; 6] = [
        Self::All,
        Self::Messages,
        Self::People,
        Self::Files,
        Self::GroupChats,
        Self::Channels,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Messages => "Messages",
            Self::People => "People",
            Self::Files => "Files",
            Self::GroupChats => "Group chats",
            Self::Channels => "Teams and channels",
        }
    }

    /// Its name in selectors and the trace.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Messages => "messages",
            Self::People => "people",
            Self::Files => "files",
            Self::GroupChats => "group-chats",
            Self::Channels => "channels",
        }
    }

    /// What it asks of the service.
    #[must_use]
    pub fn scope(self) -> SearchScope {
        match self {
            Self::All => SearchScope::All,
            Self::Messages => SearchScope::Only(SearchKind::Messages),
            Self::People => SearchScope::Only(SearchKind::People),
            Self::Files => SearchScope::Only(SearchKind::Files),
            Self::GroupChats => SearchScope::Only(SearchKind::Chats),
            Self::Channels => SearchScope::Only(SearchKind::Channels),
        }
    }

    /// The tab whose answer it shows: its own, but the Teams and channels tab shows the All tab's channels (no capture
    /// shows the web client's own request for that tab, so it isn't asked in a guessed form).
    #[must_use]
    pub fn asked(self) -> Self {
        match self {
            Self::Channels => Self::All,
            tab => tab,
        }
    }
}

/// One row of the suggestions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Suggestion {
    Chat {
        id: String,
        name: SharedString,
        picture: Option<Picture>,
        named: bool,
    },
    Channel {
        id: String,
        name: SharedString,
        team: SharedString,
    },
    Person {
        mri: Option<String>,
        name: SharedString,
        detail: SharedString,
        /// Their 1:1 chat, when the list has one.
        chat: Option<String>,
        picture: Option<Picture>,
    },
    File {
        name: SharedString,
        url: String,
        location: SharedString,
    },
    /// "Press Enter to view all results".
    Results,
}

impl Suggestion {
    /// The section it is listed under.
    fn section(&self) -> &'static str {
        match self {
            Self::Person { .. } => "People",
            Self::Chat { .. } => "Chats",
            Self::Channel { .. } => "Teams and channels",
            Self::File { .. } => "Files",
            Self::Results => "",
        }
    }

    /// Its selector's suffix: its kind and its place.
    fn kind(&self) -> &'static str {
        match self {
            Self::Person { .. } => "person",
            Self::Chat { .. } => "chat",
            Self::Channel { .. } => "channel",
            Self::File { .. } => "file",
            Self::Results => "results",
        }
    }
}

/// What the search tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlobalSearchEvent {
    OpenChat(String),
    /// Feedback for a person whose chat could not be opened.
    Toast(&'static str),
    /// A team's channel.
    OpenChannel(String),
    /// A chat at a message found.
    OpenMessage(HitTarget),
    /// Show the results across the panes.
    ShowResults,
    /// Leave the results for where the user was.
    LeaveResults,
    /// The box let go of the keyboard (Escape, or a suggestion chosen): the shell takes it back.
    Done,
}

/// The results of one query.
pub struct Results {
    query: String,
    tab: Tab,
    /// The store's messages found, the newest first.
    local: Vec<Hit>,
    /// The service's answers, by the tab that asked.
    answers: HashMap<Tab, SearchAnswer>,
    loading: HashSet<Tab>,
    failed: HashSet<Tab>,
    /// The reads under way for these results, dropped (and so stopped) with them.
    asks: Vec<Task<()>>,
}

impl Results {
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// Whether the service hasn't answered the tab shown yet.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading.contains(&self.tab.asked())
    }

    /// The answer the tab shown reads: its own, else the All tab's.
    fn answer(&self, tab: Tab) -> Option<&SearchAnswer> {
        self.answers.get(&tab).or_else(|| self.answers.get(&Tab::All))
    }
}

/// The title bar's search (see the module docs).
pub struct GlobalSearch {
    core: Arc<dyn Core>,
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    trace: Arc<ViewTrace>,
    input: Entity<InputState>,
    mode: Mode,
    /// The suggestions are shown.
    open: bool,
    suggested: Vec<Suggestion>,
    highlighted: Option<usize>,
    /// The service's suggestions, and the query they are for.
    service: Option<(String, SearchAnswer)>,
    suggesting: Option<Task<()>>,
    /// One chat creation at a time; repeated activation never sends a second write.
    opening_person: Option<Task<()>>,
    /// Navigation may finish after the user moved elsewhere; the write still settles without taking them back.
    navigation: u64,
    results: Option<Results>,
    /// The results page's, where its Escape is heard.
    results_focus: FocusHandle,
    /// The box's text when it last changed while it had the keyboard.
    typed: String,
    /// The account's translations: a message of a translated chat shows in its language ([`Self::set_previews`]).
    previews: Option<(Entity<Previews>, Subscription)>,
    /// The stored messages of the hits in translated chats, by conversation and id, and the ones being read.
    found_messages: HashMap<(String, String), Message>,
    reading_messages: HashSet<(String, String)>,
    /// Where a person found asks for their profile card ([`Self::set_cards`]).
    cards: Option<Entity<CardAsks>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GlobalSearchEvent> for GlobalSearch {}

impl GlobalSearch {
    pub fn new(
        core: Arc<dyn Core>,
        list: Entity<ChatList>,
        teams: Entity<TeamsList>,
        trace: Arc<ViewTrace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(PLACEHOLDER));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_input(event, window, cx);
            }),
            // the lists change under the suggestions: they follow
            cx.observe(&list, |this: &mut Self, _, cx| this.refresh(cx)),
            cx.observe(&teams, |this: &mut Self, _, cx| this.refresh(cx)),
        ];
        Self {
            core,
            list,
            teams,
            trace,
            input,
            mode: Mode::Search,
            open: false,
            suggested: Vec::new(),
            highlighted: None,
            service: None,
            suggesting: None,
            opening_person: None,
            navigation: 0,
            results: None,
            results_focus: cx.focus_handle(),
            typed: String::new(),
            previews: None,
            found_messages: HashMap::new(),
            reading_messages: HashSet::new(),
            cards: None,
            _subscriptions: subscriptions,
        }
    }

    /// A person found opens their compact card when the pointer rests on their face, asked of `cards`.
    pub fn set_cards(&mut self, cards: Entity<CardAsks>) {
        self.cards = Some(cards);
    }

    /// From now on a message found in a translated chat shows in the chat's language, through `previews`.
    pub fn set_previews(&mut self, previews: Entity<Previews>, cx: &mut Context<Self>) {
        let observing = cx.observe(&previews, |_: &mut Self, _, cx| cx.notify());
        self.previews = Some((previews, observing));
        self.translate_hits(cx);
    }

    /// Asks for the translations of the hits shown that are in translated chats: their messages read from the store,
    /// then handed to the account's [`Previews`].
    fn translate_hits(&mut self, cx: &mut Context<Self>) {
        let Some(previews) = self.previews.as_ref().map(|(previews, _)| previews.clone()) else {
            return;
        };
        let hits = self.message_hits(cx);
        let translated: Vec<(String, String)> = hits
            .iter()
            .filter(|hit| previews.read(cx).language_of(&hit.target.conversation, cx).is_some())
            .map(|hit| (hit.target.conversation.clone(), hit.target.message.clone()))
            .collect();
        let known: Vec<Message> = translated
            .iter()
            .filter_map(|key| self.found_messages.get(key).cloned())
            .collect();
        if !known.is_empty() {
            previews.update(cx, |previews, cx| previews.want(known, cx));
        }
        let unread: Vec<(String, String)> = translated
            .into_iter()
            .filter(|key| !self.found_messages.contains_key(key) && self.reading_messages.insert(key.clone()))
            .collect();
        let Some(results) = self.results.as_mut().filter(|_| !unread.is_empty()) else {
            return;
        };
        let core = Arc::clone(&self.core);
        results.asks.push(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut messages = Vec::new();
            for (conversation, id) in unread {
                if let Ok(Some(message)) = core.message(conversation, id).await {
                    messages.push(message);
                }
            }
            let _ = this.update(cx, |this, cx| {
                for message in &messages {
                    this.found_messages
                        .insert((message.conversation_id.clone(), message.id.clone()), message.clone());
                }
                previews.update(cx, |previews, cx| previews.want(messages, cx));
                cx.notify();
            });
        }));
    }

    /// `hit` as its chat's language shows it, when its chat is translated and its translation is known: the text
    /// shown in place of its snippet (the query's words marked where they are in it).
    fn translated_hit(&self, hit: Hit, query: &str, cx: &App) -> Hit {
        let Some((previews, _)) = &self.previews else {
            return hit;
        };
        let key = (hit.target.conversation.clone(), hit.target.message.clone());
        let Some(message) = self.found_messages.get(&key) else {
            return hit;
        };
        let Lookup::Text(text) = previews.read(cx).lookup(message, cx) else {
            return hit;
        };
        let snippet = search::snippet_line(&text.plain_text());
        let matched = search::marked_terms(&snippet, query);
        Hit {
            snippet: snippet.into(),
            matched,
            ..hit
        }
    }

    /// The query as typed.
    #[must_use]
    pub fn query(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether the suggestions are shown.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The suggestions shown, in order.
    #[must_use]
    pub fn suggestions(&self) -> &[Suggestion] {
        &self.suggested
    }

    #[must_use]
    pub fn highlighted(&self) -> Option<usize> {
        self.highlighted
    }

    /// The results, once a query was entered.
    #[must_use]
    pub fn results(&self) -> Option<&Results> {
        self.results.as_ref()
    }

    /// The results page's focus handle.
    #[must_use]
    pub fn results_focus(&self) -> &FocusHandle {
        &self.results_focus
    }

    /// Whether the keyboard is in the box. Read from the last frame, as every focus here.
    #[must_use]
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).contains_focused(window, cx)
    }

    /// Ctrl+E, Ctrl+Alt+G, a click: the box takes the keyboard in `mode` (the go-to mode starts empty), the
    /// suggestions shown.
    pub fn focus_box(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if mode != self.mode {
            self.mode = mode;
            let placeholder = match mode {
                Mode::Search => PLACEHOLDER,
                Mode::GoTo => GOTO_PLACEHOLDER,
            };
            self.input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
                if mode == Mode::GoTo {
                    input.set_value("", window, cx);
                }
            });
        }
        self.input.update(cx, |input, cx| input.focus(window, cx));
        self.show_suggestions(cx);
    }

    /// Puts `query` in the box, as typed: the suggestions follow it.
    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation = self.navigation.wrapping_add(1);
        self.input
            .update(cx, |input, cx| input.set_value(query.to_owned(), window, cx));
        query.clone_into(&mut self.typed);
        self.open = true;
        self.highlighted = None;
        self.refresh(cx);
        self.ask_suggestions(cx);
    }

    fn show_suggestions(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            self.open = true;
            let kind = match self.mode {
                Mode::Search => "search",
                Mode::GoTo => "go-to",
            };
            self.trace.record(&TraceLine::new("search", "opened").kind(kind));
        }
        self.refresh(cx);
    }

    /// Closes the suggestions (the results stay).
    pub fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        self.navigation = self.navigation.wrapping_add(1);
        if self.open {
            self.open = false;
            self.suggesting = None;
            if self.results.is_none() {
                self.trace.record(&TraceLine::new("search", "closed"));
            }
            cx.notify();
        }
    }

    fn on_input(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            // a change the app made while the box doesn't have the keyboard (cleared as it let go) opens nothing
            InputEvent::Change if !self.holds_focus(window, cx) => {}
            // the same text again (the box told of a change that changed nothing) opens nothing
            InputEvent::Change if self.query(cx) == self.typed => {}
            InputEvent::Change => {
                self.navigation = self.navigation.wrapping_add(1);
                self.typed = self.query(cx);
                self.open = true;
                self.highlighted = None;
                self.refresh(cx);
                self.ask_suggestions(cx);
            }
            InputEvent::Focus => self.show_suggestions(cx),
            InputEvent::Blur => self.close_suggestions(cx),
            InputEvent::PressEnter { .. } => self.enter(window, cx),
        }
    }

    /// Asks the service for its suggestions once typing rests; a later query drops the earlier ask.
    fn ask_suggestions(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx).trim().to_owned();
        if self.mode == Mode::GoTo || !search::searchable(&query) {
            self.suggesting = None;
            return;
        }
        let core = Arc::clone(&self.core);
        self.suggesting = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(SUGGEST_DELAY).await;
            let reply = core.search_service(query.clone(), SearchScope::Suggestions);
            let answer = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                // an answer to what the box no longer says is dropped
                if this.query(cx).trim() == query {
                    this.service = Some((query, answer));
                    this.suggesting = None;
                    this.refresh(cx);
                }
            });
        }));
    }

    /// Builds the suggestions again from the lists and the service's last answer.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.open {
            return;
        }
        let query = self.query(cx).trim().to_owned();
        let service = self
            .service
            .as_ref()
            .filter(|(asked, _)| *asked == query)
            .map(|(_, answer)| answer);
        let suggested = suggest(self.mode, &query, &self.list, &self.teams, service, cx);
        if suggested != self.suggested {
            trace_suggested(&self.trace, &suggested);
            self.suggested = suggested;
        }
        if self.mode == Mode::GoTo {
            self.highlighted = (!self.suggested.is_empty()).then_some(0);
        }
        self.highlighted = self.highlighted.filter(|at| *at < self.suggested.len());
        cx.notify();
    }

    /// Up (-1) or Down (1) through the suggestions, round.
    pub fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.suggested.len();
        if count == 0 {
            return;
        }
        self.open = true;
        let count = isize::try_from(count).unwrap_or(isize::MAX);
        let next = match self.highlighted {
            None if step > 0 => 0,
            None => count - 1,
            Some(at) => (isize::try_from(at).unwrap_or(0) + step).rem_euclid(count),
        };
        self.highlighted = usize::try_from(next).ok();
        cx.notify();
    }

    /// Enter: the suggestion highlighted, else (searching) the results of what is typed.
    pub fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(at) = self.highlighted {
            self.choose(at, window, cx);
            return;
        }
        if self.mode == Mode::Search && !self.query(cx).trim().is_empty() {
            self.show_results(Tab::All, cx);
        }
    }

    /// Escape in the box: the suggestions close, the go-to mode ends, and the keyboard goes back.
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::GoTo {
            self.mode = Mode::Search;
            self.input
                .update(cx, |input, cx| input.set_placeholder(PLACEHOLDER, window, cx));
            self.clear_box(window, cx);
        }
        self.close_suggestions(cx);
        cx.emit(GlobalSearchEvent::Done);
    }

    /// Opens the suggestion at `at`.
    pub fn choose(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chosen) = self.suggested.get(at).cloned() else {
            return;
        };
        let event = match chosen {
            Suggestion::Chat { id, .. } => Some(GlobalSearchEvent::OpenChat(id)),
            Suggestion::Channel { id, .. } => Some(GlobalSearchEvent::OpenChannel(id)),
            Suggestion::Person { chat: Some(chat), .. } => Some(GlobalSearchEvent::OpenChat(chat)),
            Suggestion::Person { mri, .. } => {
                self.open_person(mri.as_deref(), cx);
                None
            }
            Suggestion::File { url, .. } => {
                cx.open_url(&url);
                self.close_suggestions(cx);
                None
            }
            Suggestion::Results => {
                self.show_results(Tab::All, cx);
                None
            }
        };
        if let Some(event) = event {
            self.close_suggestions(cx);
            if self.mode == Mode::GoTo {
                self.escape(window, cx);
            } else {
                self.clear_box(window, cx);
            }
            cx.emit(event);
            cx.emit(GlobalSearchEvent::Done);
        }
    }

    /// Shows the results of what is typed on `tab`: the store's messages and the service's answer asked now.
    pub fn show_results(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let query = self.query(cx).trim().to_owned();
        if query.is_empty() {
            return;
        }
        let same = self.results.as_ref().is_some_and(|results| results.query == query);
        if !same {
            self.found_messages.clear();
            self.reading_messages.clear();
            let mut results = Results {
                query: query.clone(),
                tab,
                local: Vec::new(),
                answers: HashMap::new(),
                loading: HashSet::new(),
                failed: HashSet::new(),
                asks: Vec::new(),
            };
            results.asks.push(self.find_local(query, cx));
            self.results = Some(results);
        }
        self.open = false;
        self.suggesting = None;
        self.set_tab(tab, cx);
        cx.emit(GlobalSearchEvent::ShowResults);
    }

    /// The store's messages of the query, in the chats the list shows.
    fn find_local(&self, query: String, cx: &mut Context<Self>) -> Task<()> {
        let core = Arc::clone(&self.core);
        let chats = search::listed_chats(&self.list, cx);
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let hits = search::find(&*core, query.clone(), chats).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(results) = this.results.as_mut().filter(|results| results.query == query) {
                    results.local = hits;
                    this.trace_shown(cx);
                    this.translate_hits(cx);
                    cx.notify();
                }
            });
        })
    }

    /// Shows `tab` of the results, asking the service for it when it hasn't yet.
    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let Some(results) = self.results.as_mut() else {
            return;
        };
        results.tab = tab;
        self.trace.record(&TraceLine::new("search", "results").kind(tab.key()));
        let tab = tab.asked();
        let asked = results.answers.contains_key(&tab) || results.loading.contains(&tab);
        if !asked {
            results.loading.insert(tab);
            results.failed.remove(&tab);
            let reply = self.core.search_service(results.query.clone(), tab.scope());
            let query = results.query.clone();
            results.asks.push(cx.spawn(async move |this, cx: &mut AsyncApp| {
                let answer = reply.await.ok();
                let _ = this.update(cx, |this, cx| this.answered(&query, tab, answer, cx));
            }));
        }
        self.trace_shown(cx);
        self.translate_hits(cx);
        cx.notify();
    }

    /// The service's `answer` to `tab` of `query`'s results (none when it failed), kept while those results show.
    fn answered(&mut self, query: &str, tab: Tab, answer: Option<SearchAnswer>, cx: &mut Context<Self>) {
        let Some(results) = self.results.as_mut().filter(|results| results.query == query) else {
            return;
        };
        results.loading.remove(&tab);
        match answer {
            Some(answer) => {
                results.answers.insert(tab, answer);
            }
            None => {
                results.failed.insert(tab);
            }
        }
        self.trace_shown(cx);
        self.translate_hits(cx);
        cx.notify();
    }

    /// Leaves the results (Escape on them): forgotten, and the box emptied.
    pub fn clear_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.navigation = self.navigation.wrapping_add(1);
        if self.results.take().is_some() {
            self.trace.record(&TraceLine::new("search", "closed"));
            self.clear_box(window, cx);
            cx.notify();
        }
    }

    /// Empties the box, as Teams' once its search is done with (a suggestion opened, the results left).
    fn clear_box(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // the change this makes is the app's: it opens nothing
        self.typed.clear();
        self.input.update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// Traces the messages the results show now.
    fn trace_shown(&self, cx: &App) {
        let hits = self.message_hits(cx);
        search::trace_hits(&self.trace, "search", &hits);
    }

    /// The messages the results show on their tab: the service's and the store's, once each, the newest first.
    #[must_use]
    pub fn message_hits(&self, cx: &App) -> Vec<Hit> {
        let Some(results) = &self.results else {
            return Vec::new();
        };
        if !matches!(results.tab, Tab::All | Tab::Messages) {
            return Vec::new();
        }
        let found = results
            .answer(results.tab)
            .map(|answer| answer.messages.as_slice())
            .unwrap_or_default();
        let list = self.list.read(cx);
        let mut seen = HashSet::new();
        let mut hits: Vec<Hit> = found
            .iter()
            .map(|message| service_hit(message, list, self.core.me()))
            .chain(results.local.iter().cloned())
            // a message is its chat's and its id (ids are arrival times: two chats may share one)
            .filter(|hit| seen.insert((hit.target.conversation.clone(), hit.target.message.clone())))
            .collect();
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.target.arrival_time));
        if results.tab == Tab::All {
            hits.truncate(ALL_MESSAGES);
        }
        hits.into_iter()
            .map(|hit| self.translated_hit(hit, &results.query, cx))
            .collect()
    }

    /// The people the results show on their tab.
    #[must_use]
    pub fn people(&self) -> Vec<FoundPerson> {
        self.found(|answer| answer.people.clone(), Tab::People)
    }

    /// The files the results show on their tab.
    #[must_use]
    pub fn files(&self) -> Vec<FoundFile> {
        self.found(|answer| answer.files.clone(), Tab::Files)
    }

    fn found<T>(&self, of: impl Fn(&SearchAnswer) -> Vec<T>, tab: Tab) -> Vec<T> {
        let Some(results) = &self.results else {
            return Vec::new();
        };
        match results.tab {
            Tab::All => results
                .answer(Tab::All)
                .map(|answer| of(answer).into_iter().take(ALL_EACH).collect())
                .unwrap_or_default(),
            shown if shown == tab => results.answers.get(&tab).map(&of).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// The group chats the results show: the list's matched by name, then the service's the list doesn't have.
    #[must_use]
    pub fn group_chats(&self, cx: &App) -> Vec<Suggestion> {
        let Some(results) = &self.results else {
            return Vec::new();
        };
        let list = self.list.read(cx);
        let rows: Vec<(String, SharedString)> = list
            .findable_rows()
            .filter(|row| matches!(row.kind, RowKind::Group | RowKind::Meeting))
            .map(|row| (row.id.clone(), row.name.clone()))
            .collect();
        let mut chats: Vec<Suggestion> = matches(&results.query, &rows)
            .into_iter()
            .map(|(id, name)| chat_suggestion(list, id, name))
            .collect();
        let service = results
            .answer(Tab::GroupChats)
            .map(|answer| answer.chats.clone())
            .unwrap_or_default();
        for chat in service {
            if !chats
                .iter()
                .any(|known| matches!(known, Suggestion::Chat { id, .. } if *id == chat.id))
            {
                let name = list
                    .row(&chat.id)
                    .map_or_else(|| SharedString::from(chat.name.clone()), |row| row.name.clone());
                chats.push(chat_suggestion(list, chat.id, name));
            }
        }
        if results.tab == Tab::All {
            chats.truncate(ALL_EACH);
        }
        chats
    }

    /// The teams' channels the results show: the Teams list's matched by name, then the service's.
    #[must_use]
    pub fn channels(&self, cx: &App) -> Vec<Suggestion> {
        let Some(results) = &self.results else {
            return Vec::new();
        };
        let mut channels = local_channels(&results.query, &self.teams, cx);
        let service = results
            .answer(Tab::Channels)
            .map(|answer| answer.channels.clone())
            .unwrap_or_default();
        for found in service {
            if !channels
                .iter()
                .any(|known| matches!(known, Suggestion::Channel { id, .. } if *id == found.id))
            {
                channels.push(Suggestion::Channel {
                    id: found.id,
                    name: found.name.into(),
                    team: found.team.unwrap_or_default().into(),
                });
            }
        }
        if results.tab == Tab::All {
            channels.truncate(ALL_EACH);
        }
        channels
    }

    /// Opens a person's existing 1:1 chat, or creates it through the engine's persistent outbox.
    pub fn open_person(&mut self, mri: Option<&str>, cx: &mut Context<Self>) {
        let Some(mri) = mri.filter(|mri| !mri.is_empty()) else {
            cx.emit(GlobalSearchEvent::Toast("This account has no chat address."));
            return;
        };
        if let Some(chat) = one_to_one_of(&self.list, mri, cx) {
            cx.emit(GlobalSearchEvent::OpenChat(chat));
            return;
        }
        if self.core.read_only() {
            cx.emit(GlobalSearchEvent::Toast(
                "A new chat can't be opened in read-only mode.",
            ));
            return;
        }
        if self.opening_person.is_some() {
            cx.emit(GlobalSearchEvent::Toast("Opening a chat…"));
            return;
        }
        let reply = self.core.write(Action::CreateChat {
            others: vec![Mri::new(mri)],
            topic: None,
        });
        let core = Arc::clone(&self.core);
        let navigation = self.navigation;
        cx.emit(GlobalSearchEvent::Toast("Opening a chat…"));
        self.opening_person = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            // a throttled or uncertain write already owns an outbox row: it is watched, never submitted again
            let chat = settled_row(core.as_ref(), reply, cx.background_executor())
                .await
                .filter(|row| row.state == OutboxState::Sent)
                .and_then(|row| row.message_id);
            let _ = this.update(cx, |this, cx| {
                this.opening_person = None;
                if this.navigation != navigation {
                    return;
                }
                match chat {
                    Some(chat) => cx.emit(GlobalSearchEvent::OpenChat(chat)),
                    None => cx.emit(GlobalSearchEvent::Toast(
                        "We couldn't open the chat. Check your connection and try again.",
                    )),
                }
            });
        }));
    }

    /// The service failed the tab shown.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.results
            .as_ref()
            .is_some_and(|results| results.failed.contains(&results.tab.asked()))
    }
}

/// A message the service found, as a hit: its chat named as the list names it (else as the service does).
fn service_hit(message: &FoundMessage, list: &ChatList, me: &str) -> Hit {
    let chat = list.row(&message.conversation).map_or_else(
        || SharedString::from(message.to.clone().unwrap_or_else(|| "Chat".to_owned())),
        |row| row.name.clone(),
    );
    let sender = match (&message.sender_mri, &message.sender_name) {
        (Some(mri), _) if mri == me => SharedString::from(search::YOU),
        (_, Some(name)) => name.clone().into(),
        _ => SharedString::default(),
    };
    Hit {
        target: HitTarget {
            conversation: message.conversation.clone(),
            message: message.id.clone(),
            arrival_time: message.sent_ms,
            thread: message.parent.clone(),
        },
        chat,
        sender,
        snippet: message.snippet.clone().into(),
        matched: message.matched.clone(),
    }
}

/// The 1:1 chat the list has with `mri`.
fn one_to_one_of(list: &Entity<ChatList>, mri: &str, cx: &App) -> Option<String> {
    list.read(cx)
        .findable_rows()
        .find(|row| row.kind == RowKind::OneToOne && row.partner.as_deref() == Some(mri))
        .map(|row| row.id.clone())
}

fn chat_suggestion(list: &ChatList, id: String, name: SharedString) -> Suggestion {
    let row = list.row(&id);
    Suggestion::Chat {
        picture: row.and_then(|row| row.picture.clone()),
        named: row.is_none_or(|row| row.named),
        id,
        name,
    }
}

/// The Teams list's channels `query` matches, the best first.
fn local_channels(query: &str, teams: &Entity<TeamsList>, cx: &App) -> Vec<Suggestion> {
    let teams = teams.read(cx).teams().unwrap_or_default();
    let named: Vec<(String, SharedString)> = teams
        .iter()
        .flat_map(|team| {
            team.channels
                .iter()
                .map(move |channel| (channel.id.clone(), format!("{} › {}", team.name, channel.name).into()))
        })
        .collect();
    matches(query, &named)
        .into_iter()
        .filter_map(|(id, _)| {
            teams.iter().find_map(|team| {
                team.channels
                    .iter()
                    .find(|channel| channel.id == id)
                    .map(|channel| Suggestion::Channel {
                        id: channel.id.clone(),
                        name: channel.name.clone().into(),
                        team: team.name.clone().into(),
                    })
            })
        })
        .collect()
}

/// The suggestions for `query` in `mode` (see the module docs).
fn suggest(
    mode: Mode,
    query: &str,
    list: &Entity<ChatList>,
    teams: &Entity<TeamsList>,
    service: Option<&SearchAnswer>,
    cx: &App,
) -> Vec<Suggestion> {
    let chat_list = list.read(cx);
    let rows: Vec<(String, SharedString)> = chat_list
        .findable_rows()
        .filter(|row| row.kind != RowKind::Channel)
        .map(|row| (row.id.clone(), row.name.clone()))
        .collect();
    if query.is_empty() {
        let recent = rows
            .into_iter()
            .take(RECENT)
            .map(|(id, name)| chat_suggestion(chat_list, id, name));
        return recent.collect();
    }
    let chats: Vec<Suggestion> = matches(query, &rows)
        .into_iter()
        .map(|(id, name)| chat_suggestion(chat_list, id, name))
        .collect();
    let channels = local_channels(query, teams, cx);
    if mode == Mode::GoTo {
        return chats.into_iter().chain(channels).take(GOTO_MAX).collect();
    }
    let mut out = people(query, chat_list, service);
    out.extend(chats.into_iter().take(SUGGESTED_CHATS));
    for chat in service.map(|answer| answer.chats.as_slice()).unwrap_or_default() {
        let listed = out
            .iter()
            .filter(|known| matches!(known, Suggestion::Chat { .. }))
            .count();
        if listed < SUGGESTED_CHATS
            && !out
                .iter()
                .any(|known| matches!(known, Suggestion::Chat { id, .. } if *id == chat.id))
        {
            let name = chat_list
                .row(&chat.id)
                .map_or_else(|| SharedString::from(chat.name.clone()), |row| row.name.clone());
            out.push(chat_suggestion(chat_list, chat.id.clone(), name));
        }
    }
    let mut channels = channels;
    for found in service.map(|answer| answer.channels.as_slice()).unwrap_or_default() {
        if !channels
            .iter()
            .any(|known| matches!(known, Suggestion::Channel { id, .. } if *id == found.id))
        {
            channels.push(Suggestion::Channel {
                id: found.id.clone(),
                name: found.name.clone().into(),
                team: found.team.clone().unwrap_or_default().into(),
            });
        }
    }
    out.extend(channels.into_iter().take(SUGGESTED_CHANNELS));
    let files = service.map(|answer| answer.files.as_slice()).unwrap_or_default();
    out.extend(files.iter().take(SUGGESTED_FILES).map(|file| Suggestion::File {
        name: file.name.clone().into(),
        url: file.url.clone(),
        location: file.location.clone().unwrap_or_default().into(),
    }));
    out.push(Suggestion::Results);
    out
}

/// The people suggested: the list's 1:1 partners `query` matches, then the service's (each once, by MRI).
fn people(query: &str, list: &ChatList, service: Option<&SearchAnswer>) -> Vec<Suggestion> {
    let partners: Vec<(String, SharedString)> = list
        .findable_rows()
        .filter(|row| row.kind == RowKind::OneToOne && row.named)
        .filter_map(|row| row.partner.clone().map(|partner| (partner, row.name.clone())))
        .collect();
    let mut people: Vec<Suggestion> = matches(query, &partners)
        .into_iter()
        .map(|(mri, name)| person_suggestion(list, Some(mri), name, SharedString::default()))
        .collect();
    for person in service.map(|answer| answer.people.as_slice()).unwrap_or_default() {
        let known = people.iter_mut().find(
            |known| matches!(known, Suggestion::Person { mri: Some(mri), .. } if Some(mri) == person.mri.as_ref()),
        );
        let detail = person_detail(person);
        match known {
            Some(Suggestion::Person { detail: shown, .. }) => *shown = detail,
            _ => people.push(person_suggestion(
                list,
                person.mri.clone(),
                person.name.clone().into(),
                detail,
            )),
        }
    }
    people.truncate(SUGGESTED_PEOPLE);
    people
}

fn person_suggestion(list: &ChatList, mri: Option<String>, name: SharedString, detail: SharedString) -> Suggestion {
    let row = mri.as_deref().and_then(|mri| {
        list.findable_rows()
            .find(|row| row.kind == RowKind::OneToOne && row.partner.as_deref() == Some(mri))
    });
    Suggestion::Person {
        chat: row.map(|row| row.id.clone()),
        picture: row.and_then(|row| row.picture.clone()),
        mri,
        name,
        detail,
    }
}

/// What tells a person apart: their job, else their address.
#[must_use]
pub fn person_detail(person: &FoundPerson) -> SharedString {
    match (&person.job_title, &person.department, &person.email) {
        (Some(job), Some(department), _) => format!("{job} · {department}").into(),
        (Some(job), None, _) => job.clone().into(),
        (None, _, Some(email)) => email.clone().into(),
        _ => SharedString::default(),
    }
}

/// Traces the suggestions: the chats', channels' and people's ids, never a name.
fn trace_suggested(trace: &ViewTrace, suggested: &[Suggestion]) {
    let conversations = suggested.iter().filter_map(|suggestion| match suggestion {
        Suggestion::Chat { id, .. } | Suggestion::Channel { id, .. } => Some(id.as_str()),
        _ => None,
    });
    let people = suggested.iter().filter_map(|suggestion| match suggestion {
        Suggestion::Person { mri, .. } => mri.as_deref(),
        _ => None,
    });
    trace.record(
        &TraceLine::new("search", "suggested")
            .conversations(conversations)
            .people(people),
    );
}

impl Render for GlobalSearch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let focused = self.holds_focus(window, cx);
        let lead = match self.mode {
            Mode::GoTo => div()
                .flex_none()
                .px_1p5()
                .rounded_sm()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child("/goto")
                .into_any_element(),
            Mode::Search => Glyph::SEARCH
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.muted_foreground))
                .into_any_element(),
        };
        let typed = !self.input.read(cx).value().is_empty();
        let clear = typed.then(|| {
            div()
                .id("title-search-clear")
                .debug_selector(|| "title-search-clear".to_owned())
                .flex_none()
                .p_1()
                .rounded_sm()
                .cursor_pointer()
                .text_color(rgb(palette.muted_foreground))
                .hover(move |button| button.bg(rgb(palette.hover)))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.input.update(cx, |input, cx| input.set_value("", window, cx));
                    this.input.update(cx, |input, cx| input.focus(window, cx));
                }))
                .child(Glyph::X.icon().size(px(14.0)))
        });
        div()
            .debug_selector(|| "title-search".to_owned())
            .id("title-search")
            .w_full()
            .h(px(32.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .map(|search| on_frame(search, focused, palette))
            .cursor_text()
            // Up, Down and Escape are the suggestions' while the box has the keyboard
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.move_highlight(-1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.move_highlight(1, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.escape(window, cx);
                cx.stop_propagation();
            }))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                if !this.holds_focus(window, cx) {
                    this.focus_box(Mode::Search, window, cx);
                }
            }))
            .child(lead)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.input).appearance(false).aria_label("Search")),
            )
            .children(clear)
    }
}

/// The search box's face on the title bar's frame: nearly opaque so its text reads (in the panes' text colour, not
/// the frame's), lit up in the brand colour while it has the keyboard.
fn on_frame(search: gpui::Stateful<gpui::Div>, focused: bool, palette: Palette) -> gpui::Stateful<gpui::Div> {
    search
        .rounded_md()
        .border_1()
        .border_color(if focused {
            rgb(palette.brand)
        } else {
            rgb(palette.on_frame).opacity(0.3)
        })
        .bg(rgb(palette.surface).opacity(if focused { 1.0 } else { 0.9 }))
        .text_color(rgb(palette.foreground))
        .when(focused, |search| {
            search.shadow(theme::glow(palette, palette.accent, 0.45, 14.0, 0.0))
        })
}

/// A row's frame in the suggestions: highlighted when the keyboard is on it, its own clicks.
fn row_frame(id: String, highlighted: bool, palette: Palette) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    div()
        .debug_selector(move || selector.clone())
        .id(SharedString::from(id))
        .w_full()
        .min_h(px(40.0))
        .px_3()
        .py_1()
        .flex()
        .items_center()
        .gap_3()
        .rounded_md()
        .cursor_pointer()
        .when(highlighted, |row| row.bg(rgb(palette.selected)))
        .hover(move |row| row.bg(rgb(palette.hover)))
}

/// Two lines: a name, and a muted detail under it when there is one.
fn two_lines(name: SharedString, detail: SharedString, palette: Palette) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(div().text_sm().truncate().child(name))
        .when(!detail.is_empty(), |lines| {
            lines.child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .truncate()
                    .child(detail),
            )
        })
}

/// A glyph in a tinted square, where a row has no picture.
fn glyph_face(glyph: Glyph, palette: Palette) -> gpui::Div {
    div()
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(rgb(palette.selected))
        .text_color(rgb(palette.brand))
        .child(glyph.icon().size(px(16.0)))
}

/// A suggestion's look: its face and its lines.
fn suggestion_content(suggestion: &Suggestion, query: &str, palette: Palette) -> Vec<AnyElement> {
    match suggestion {
        Suggestion::Chat {
            name, picture, named, ..
        } => vec![
            people::avatar(named.then_some(name), picture.as_ref(), Size::Small).into_any_element(),
            two_lines(name.clone(), SharedString::default(), palette).into_any_element(),
        ],
        Suggestion::Channel { name, team, .. } => vec![
            glyph_face(Glyph::HASH, palette).into_any_element(),
            two_lines(name.clone(), team.clone(), palette).into_any_element(),
        ],
        Suggestion::Person {
            name, detail, picture, ..
        } => vec![
            people::avatar(Some(name), picture.as_ref(), Size::Small).into_any_element(),
            two_lines(name.clone(), detail.clone(), palette).into_any_element(),
        ],
        Suggestion::File { name, location, .. } => vec![
            glyph_face(Glyph::FILE, palette).into_any_element(),
            two_lines(name.clone(), location.clone(), palette).into_any_element(),
        ],
        Suggestion::Results => vec![
            Glyph::SEARCH
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.brand))
                .into_any_element(),
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap_1()
                .text_sm()
                .text_color(rgb(palette.brand))
                .child(if query.is_empty() {
                    "Search messages, files and more".to_owned()
                } else {
                    format!("Search for \u{201c}{query}\u{201d}")
                })
                .child(div().flex_1())
                .child("Press")
                .child(Kbd::new(gpui::Keystroke::parse("enter").unwrap_or_default()).outline())
                .child("to view all results")
                .into_any_element(),
        ],
    }
}

/// The suggestions under the box, while they are shown (see the module docs).
#[must_use]
pub fn dropdown(search: &Entity<GlobalSearch>, palette: Palette, window: &Window, cx: &App) -> Option<AnyElement> {
    let this = search.read(cx);
    // only while the box has the keyboard (an inactive window tells it of no blur)
    if !this.open || !this.holds_focus(window, cx) {
        return None;
    }
    let query = this.query(cx).trim().to_owned();
    let mut body: Vec<AnyElement> = Vec::new();
    if this.mode == Mode::Search && !query.is_empty() {
        body.push(chips(search, palette).into_any_element());
    }
    let mut section = "";
    for (at, suggestion) in this.suggested.iter().enumerate() {
        let label = if query.is_empty() {
            "Recent"
        } else {
            suggestion.section()
        };
        if label != section && !label.is_empty() {
            section = label;
            body.push(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_foreground))
                    .child(label)
                    .into_any_element(),
            );
        }
        let chosen = search.clone();
        let row = row_frame(
            format!("title-search-{}-{at}", suggestion.kind()),
            this.highlighted == Some(at),
            palette,
        )
        .on_click(move |_, window, cx| chosen.update(cx, |this, cx| this.choose(at, window, cx)))
        .children(suggestion_content(suggestion, &query, palette));
        body.push(row.into_any_element());
    }
    if body.is_empty() {
        body.push(
            div()
                .px_3()
                .py_3()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(if query.is_empty() {
                    "Type a name or words to search for"
                } else {
                    "No chat or channel goes by that name"
                })
                .into_any_element(),
        );
    }
    Some(
        div()
            .debug_selector(|| "title-search-suggestions".to_owned())
            .id("title-search-suggestions")
            .w_full()
            .max_h(px(520.0))
            .overflow_y_scroll()
            .p_1()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            // Underlying views must not take mouse-down focus before a suggestion receives its click.
            .occlude()
            // a click in here keeps the box's keyboard: the suggestions stay until one is chosen
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .children(body)
            .into_any_element(),
    )
}

/// The chips over the suggestions: straight to the results of one kind.
fn chips(search: &Entity<GlobalSearch>, palette: Palette) -> gpui::Div {
    let chips = Tab::ALL[1..].iter().map(|tab| {
        let tab = *tab;
        let search = search.clone();
        let selector = format!("title-search-chip-{}", tab.key());
        div()
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .id(SharedString::from(selector))
            .px_3()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(rgb(palette.border))
            .text_sm()
            .cursor_pointer()
            .hover(move |chip| chip.bg(rgb(palette.hover)))
            .on_click(move |_, _, cx| search.update(cx, |this, cx| this.show_results(tab, cx)))
            .child(tab.label())
    });
    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .px_2()
        .py_2()
        .border_b_1()
        .border_color(rgb(palette.border))
        .children(chips)
}

/// The results' group chats and teams' channels, on All and on their own tabs.
fn place_sections(
    this: &GlobalSearch,
    tab: Tab,
    search: &Entity<GlobalSearch>,
    palette: Palette,
    cx: &App,
) -> Vec<AnyElement> {
    let mut sections = Vec::new();
    if matches!(tab, Tab::All | Tab::GroupChats) {
        let chats = this.group_chats(cx);
        if !chats.is_empty() {
            sections.push(section("Group chats", Tab::GroupChats, tab, search, palette).into_any_element());
            sections.extend(
                chats
                    .iter()
                    .enumerate()
                    .map(|(at, chat)| place_row("chat", at, chat, search, palette)),
            );
        }
    }
    if matches!(tab, Tab::All | Tab::Channels) {
        let channels = this.channels(cx);
        if !channels.is_empty() {
            sections.push(section("Teams and channels", Tab::Channels, tab, search, palette).into_any_element());
            sections.extend(
                channels
                    .iter()
                    .enumerate()
                    .map(|(at, channel)| place_row("channel", at, channel, search, palette)),
            );
        }
    }
    sections
}

/// The results page (see the module docs): the query, the tabs, the results of the tab shown.
#[must_use]
pub fn results_page(search: &Entity<GlobalSearch>, palette: Palette, cx: &App) -> AnyElement {
    let this = search.read(cx);
    let Some(results) = &this.results else {
        return div().into_any_element();
    };
    let tab = results.tab;
    let cycle = clock::cycle(cx);
    let mut sections: Vec<AnyElement> = Vec::new();
    let hits = this.message_hits(cx);
    if matches!(tab, Tab::All | Tab::Messages) && !hits.is_empty() {
        sections.push(section("Messages", Tab::Messages, tab, search, palette).into_any_element());
        sections.extend(hits.iter().map(|hit| message_card(hit, search, palette, cycle)));
    }
    let people = this.people();
    if !people.is_empty() {
        sections.push(section("People", Tab::People, tab, search, palette).into_any_element());
        sections.extend(
            people
                .iter()
                .enumerate()
                .map(|(at, person)| person_row(at, person, (search, this.cards.as_ref()), palette)),
        );
    }
    let files = this.files();
    if !files.is_empty() {
        sections.push(section("Files", Tab::Files, tab, search, palette).into_any_element());
        sections.extend(files.iter().enumerate().map(|(at, file)| file_row(at, file, palette)));
    }
    sections.extend(place_sections(this, tab, search, palette, cx));
    if sections.is_empty() {
        sections.push(empty_results(results.loading(), this.failed(), palette).into_any_element());
    }
    let leave = search.clone();
    div()
        .debug_selector(|| "search-results".to_owned())
        .key_context(RESULTS_CONTEXT)
        .track_focus(&this.results_focus)
        .on_action(move |_: &LeaveSearchResults, _, cx| {
            leave.update(cx, |_, cx| cx.emit(GlobalSearchEvent::LeaveResults));
        })
        .size_full()
        .flex()
        .flex_col()
        .bg(rgb(palette.background))
        .child(tabs(search, tab, palette))
        .child(
            div()
                .id("search-results-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    div()
                        .w_full()
                        .max_w(px(760.0))
                        .mx_auto()
                        .px_4()
                        .py_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(sections),
                ),
        )
        .into_any_element()
}

/// The tabs, the one shown filled as Teams' pills are.
fn tabs(search: &Entity<GlobalSearch>, shown: Tab, palette: Palette) -> gpui::Div {
    let pills = Tab::ALL.iter().map(|tab| {
        let tab = *tab;
        let search = search.clone();
        let selector = format!("search-tab-{}", tab.key());
        let chosen = tab == shown;
        div()
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .id(SharedString::from(selector))
            .px_4()
            .py_1()
            .rounded_full()
            .border_1()
            .text_sm()
            .cursor_pointer()
            .map(|pill| {
                if chosen {
                    pill.border_color(rgb(palette.brand))
                        .bg(rgb(palette.selected))
                        .text_color(rgb(palette.brand))
                        .font_weight(FontWeight::SEMIBOLD)
                } else {
                    pill.border_color(rgb(palette.border))
                        .hover(move |pill| pill.bg(rgb(palette.hover)))
                }
            })
            .on_click(move |_, _, cx| search.update(cx, |this, cx| this.set_tab(tab, cx)))
            .child(tab.label())
    });
    div()
        .flex_none()
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .flex_wrap()
        .justify_center()
        .gap_2()
        .border_b_1()
        .border_color(rgb(palette.border))
        .children(pills)
}

/// A section's title on the results, with "See all" to its own tab on the All tab.
fn section(title: &'static str, tab: Tab, shown: Tab, search: &Entity<GlobalSearch>, palette: Palette) -> gpui::Div {
    let all = (shown == Tab::All).then(|| {
        let search = search.clone();
        let selector = format!("search-see-all-{}", tab.key());
        div()
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .id(SharedString::from(selector))
            .text_sm()
            .text_color(rgb(palette.brand))
            .cursor_pointer()
            .hover(gpui::Styled::underline)
            .on_click(move |_, _, cx| search.update(cx, |this, cx| this.set_tab(tab, cx)))
            .child("See all")
    });
    div()
        .pt_3()
        .flex()
        .items_center()
        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(title))
        .children(all)
}

/// A card's frame on the results.
fn card(id: String, palette: Palette) -> gpui::Stateful<gpui::Div> {
    keyed_card(id.clone(), id, palette)
}

/// A card whose element id (`key`, unique among the cards) differs from its test selector.
fn keyed_card(selector: String, key: String, palette: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .debug_selector(move || selector.clone())
        .id(SharedString::from(key))
        .w_full()
        .px_4()
        .py_3()
        .flex()
        .flex_col()
        .gap_2()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .cursor_pointer()
        .hover(move |card| card.border_color(rgb(palette.brand)))
}

/// The text around a match, the matched terms marked as Teams marks them.
fn marked(text: &SharedString, matched: &[std::ops::Range<usize>], palette: Palette) -> StyledText {
    let mark = HighlightStyle {
        background_color: Some(rgba(0xffc8_3d66).into()),
        font_weight: Some(FontWeight::SEMIBOLD),
        color: Some(rgb(palette.foreground).into()),
        ..HighlightStyle::default()
    };
    StyledText::new(text.clone()).with_highlights(
        matched
            .iter()
            .filter(|range| {
                range.end <= text.len() && text.is_char_boundary(range.start) && text.is_char_boundary(range.end)
            })
            .map(|range| (range.clone(), mark)),
    )
}

/// A message found: its chat over its sender, time and text, as Teams' cards are.
fn message_card(hit: &Hit, search: &Entity<GlobalSearch>, palette: Palette, cycle: HourCycle) -> AnyElement {
    let when = clock::list_time(hit.target.arrival_time, &Local::now(), cycle);
    let target = hit.target.clone();
    let search = search.clone();
    keyed_card(
        format!("search-message-{}", hit.target.message),
        format!("search-message-{}-{}", hit.target.conversation, hit.target.message),
        palette,
    )
    .on_click(move |_, _, cx| {
        let target = target.clone();
        search.update(cx, |_, cx| cx.emit(GlobalSearchEvent::OpenMessage(target)));
    })
    .child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .pb_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .text_xs()
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::MESSAGE.icon().size(px(14.0)))
            .child(div().truncate().child(hit.chat.clone())),
    )
    .child(
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(people::avatar(Some(&hit.sender), None, Size::XSmall))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(hit.sender.clone()),
            )
            .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(when)),
    )
    .child(div().text_sm().child(marked(&hit.snippet, &hit.matched, palette)))
    .into_any_element()
}

/// A person found: their name and job; a click opens their 1:1 chat.
fn person_row(
    at: usize,
    person: &FoundPerson,
    (search, cards): (&Entity<GlobalSearch>, Option<&Entity<CardAsks>>),
    palette: Palette,
) -> AnyElement {
    let name: SharedString = person.name.clone().into();
    let detail = person_detail(person);
    let mri = person.mri.clone();
    let search = search.clone();
    let face = people::avatar(Some(&name), None, Size::Medium);
    // a rest on the face opens their compact card, as Teams' results do; a click still opens their chat
    let face = match (cards, mri.clone()) {
        (Some(cards), Some(mri)) => profile_card::anchor(
            format!("profile-card-from-search-{at}"),
            CardOf::new(mri, Some(name.clone())),
            Click::Hover,
            cards,
            face,
        ),
        _ => face.into_any_element(),
    };
    card(format!("search-person-{at}"), palette)
        .flex_row()
        .items_center()
        .gap_3()
        .on_click(move |_, _, cx| search.update(cx, |this, cx| this.open_person(mri.as_deref(), cx)))
        .child(face)
        .child(two_lines(name, detail, palette))
        .into_any_element()
}

/// A file found: its name, where it is and who changed it when; a click opens it in the browser.
fn file_row(at: usize, file: &FoundFile, palette: Palette) -> AnyElement {
    let url = file.url.clone();
    let mut detail = file.location.clone().unwrap_or_default();
    if let Some(by) = &file.modified_by {
        let when = file
            .modified_ms
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|time| time.with_timezone(&Local).format(" on %B %-d, %Y").to_string())
            .unwrap_or_default();
        if !detail.is_empty() {
            detail.push_str(" · ");
        }
        let _ = write!(detail, "Modified by {by}{when}");
    }
    let snippet: SharedString = file.snippet.clone().into();
    card(format!("search-file-{at}"), palette)
        .on_click(move |_, _, cx| cx.open_url(&url))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(glyph_face(Glyph::FILE, palette))
                .child(two_lines(file.name.clone().into(), detail.into(), palette)),
        )
        .when(!snippet.is_empty(), |card| {
            card.child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(marked(
                &snippet,
                &file.matched,
                palette,
            )))
        })
        .into_any_element()
}

/// A group chat or a channel found: a click opens it.
fn place_row(
    kind: &'static str,
    at: usize,
    place: &Suggestion,
    search: &Entity<GlobalSearch>,
    palette: Palette,
) -> AnyElement {
    let event = match place {
        Suggestion::Chat { id, .. } => GlobalSearchEvent::OpenChat(id.clone()),
        Suggestion::Channel { id, .. } => GlobalSearchEvent::OpenChannel(id.clone()),
        _ => return div().into_any_element(),
    };
    let search = search.clone();
    card(format!("search-{kind}-{at}"), palette)
        .flex_row()
        .items_center()
        .gap_3()
        .on_click(move |_, _, cx| {
            let event = event.clone();
            search.update(cx, |_, cx| cx.emit(event));
        })
        .children(suggestion_content(place, "", palette))
        .into_any_element()
}

/// The results while the tab has none: searching, failed, or nothing found.
fn empty_results(loading: bool, failed: bool, palette: Palette) -> gpui::Div {
    let (glyph, title, hint) = if loading {
        (Glyph::LOADING, "Searching…", "")
    } else if failed {
        (
            Glyph::CIRCLE_ALERT,
            "The search didn't answer",
            "Check the connection, then try another tab or search again",
        )
    } else {
        (
            Glyph::SEARCH,
            "No results found",
            "Check for spelling or try searching for another term",
        )
    };
    div()
        .debug_selector(|| "search-results-empty".to_owned())
        .w_full()
        .pt_16()
        .flex()
        .flex_col()
        .items_center()
        .gap_3()
        .text_color(rgb(palette.muted_foreground))
        .child(
            div()
                .size(px(64.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(glyph.icon().size(px(28.0))),
        )
        .child(
            div()
                .text_lg()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(palette.foreground))
                .child(title),
        )
        .when(!hint.is_empty(), |empty| empty.child(div().text_sm().child(hint)))
}
