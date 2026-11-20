//! A chat's people, as browser Teams shows them from its header (seen 2026-11-10 with lab accounts): the header's
//! "View and add participants" button opens a flyout listing the chat's people, and from it people are added.
//!
//! - **The flyout**: "People" over a 1:1 chat's two, "People (<n>)" over a group chat's; each with their picture or
//!   initials, their presence on its corner and the account's own marked "You" under its name. Under a rule, a 1:1
//!   chat offers "Start a group chat", a group chat (and a new group chat not made yet) "Add people". The read-only
//!   mode offers neither: it writes nothing.
//! - **The picker** ("Start a group chat", or "Add" for a group chat): "Enter a name, email, or tag" finds the people
//!   the account has a 1:1 chat with and the people the directory finds (the service's suggestions, a moment after
//!   the typing stops), then the group chats whose names hold what was typed; none while nothing is typed. Up and
//!   Down move in them, Enter takes the one highlighted (else the first), Backspace in the empty box takes the last
//!   chip off. Each one taken is a chip with its face and a remove button; a group chat taken stands for its people.
//!   Nobody already in the chat, nor the account, is offered. Cancel closes it; Create or Add is lit once someone is
//!   taken and the chat's people are read (a 1:1 chat's partner known), as [`ChatPeople::can_confirm`] says.
//! - **A group chat's history** (its "Add" only): "Don't include chat history" (Teams' default), "Include history
//!   from the past number of days:" with its number (1 to start; digits only, and Add is off while it is empty or
//!   0), or "Include all chat history" ([`ChatHistory`]).
//! - **What a confirm does**: a 1:1 chat's Create hands the shell the people, the partner last
//!   ([`ChatPeopleEvent::StartGroup`]: the unnamed group chat of exactly them opens if there is one, else a new group
//!   chat that its first message makes, as Teams does; the 1:1 chat stays as it was). A group chat's Add goes out
//!   through the outbox ([`Action::AddMembers`]), "Adding..." until the outbox settles it; then the picker closes, or
//!   says what went wrong and keeps the people. A new group chat not made yet takes them into its people
//!   ([`ChatPeopleEvent::AddToDraft`]).
//!
//! Escape or a press outside closes the picker, then the flyout. Traced: `chat-people opened`, `picked` (a count),
//! `added` (the chat), never a name.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncWindowContext, Bounds, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Backspace, Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::radio::Radio;
use gpui_component::{Disableable as _, Sizable as _, Size};
use teams_core::{Action, ChatHistory, SearchScope};
use teams_proto::Mri;
use teams_store::OutboxState;

use crate::bridge::Core;
use crate::chat_list::{self, ChatList, ChatRow, RowKind};
use crate::icons::{Glyph, selected_by};
use crate::models::Chats;
use crate::new_chat::Recipient;
use crate::people::{self, Picture, Shown};
use crate::presence::Presences;
use crate::profile_card::{self, CardAsks, CardOf};
use crate::sending::settled_row;
use crate::switcher::matches;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The header button's tooltip and accessible name, as Teams words it.
pub const PEOPLE_TIP: &str = "View and add participants";
/// A 1:1 chat's flyout's action, and its picker's title.
pub const START_GROUP: &str = "Start a group chat";
/// A group chat's flyout's action.
pub const ADD_PEOPLE: &str = "Add people";
/// A group chat's picker's title.
pub const ADD_TITLE: &str = "Add";
/// The picker's box's hint.
pub const QUERY_HINT: &str = "Enter a name, email, or tag";
/// The history choices, as Teams words them.
pub const NO_HISTORY: &str = "Don't include chat history";
pub const SOME_HISTORY: &str = "Include history from the past number of days:";
pub const ALL_HISTORY: &str = "Include all chat history";
/// What the account's own row says under its name.
pub const YOU: &str = "You";
/// What the picker says while its people are being added.
pub const ADDING: &str = "Adding...";
/// What it says when they couldn't be.
pub const NOT_ADDED: &str = "We couldn't add them. Check your connection and try again.";

/// How long typing rests before the directory is asked.
const PAUSE: Duration = Duration::from_millis(200);
/// The most people, and the most group chats, suggested.
const MOST_PEOPLE: usize = 6;
const MOST_CHATS: usize = 4;
/// The flyout's and the picker's widths, px, as Teams draws them.
const FLYOUT_WIDTH: f32 = 252.0;
const PICKER_WIDTH: f32 = 402.0;
/// A face in the flyout and in the suggestions, px.
const FACE: f32 = 32.0;
/// Where the suggestions hang from the picker's top, the most they take and one's height, px.
const PANEL_TOP: f32 = 84.0;
const PANEL_MOST: f32 = 260.0;
const SUGGESTION_HEIGHT: f32 = 52.0;
/// How far the picker reaches over the flyout from its left, px, as Teams lays it.
const PICKER_OVER: f32 = 98.0;
/// The most days of history a number takes (Teams takes any; this keeps the time sane).
const MOST_DAYS: u32 = 36_500;

/// Whose people the flyout shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeopleOf {
    /// A 1:1 chat: "Start a group chat".
    OneToOne,
    /// A group chat: "Add people", with the history choices.
    Group,
    /// A new group chat not made yet: "Add people" takes them into it.
    Draft,
}

/// The history choice as the picker shows it; the number of days is its box's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryChoice {
    #[default]
    None,
    Days,
    All,
}

/// Someone in the flyout or the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub mri: String,
    pub name: SharedString,
    /// Whether `name` is a name (else it says the person is unknown, and the avatar shows no initials).
    pub named: bool,
    pub picture: Option<Picture>,
    /// What the directory says of them (a suggestion's second line).
    pub detail: SharedString,
}

impl Person {
    fn recipient(&self) -> Recipient {
        Recipient {
            mri: self.mri.clone(),
            name: self.name.clone(),
            picture: self.picture.clone(),
        }
    }

    fn of(mri: &str, shown: Shown) -> Self {
        let named = shown.name.is_some();
        Self {
            mri: mri.to_owned(),
            name: shown
                .name
                .unwrap_or_else(|| SharedString::from(chat_list::unnamed(mri))),
            named,
            picture: shown.picture,
            detail: SharedString::default(),
        }
    }
}

/// One taken in the picker, or offered: a person, or a group chat standing for its people.
#[derive(Clone, Debug)]
pub enum Picked {
    Person(Person),
    Chat(ChatRow),
}

impl Picked {
    fn name(&self) -> SharedString {
        match self {
            Self::Person(person) => person.name.clone(),
            Self::Chat(row) => row.name.clone(),
        }
    }
}

/// What the flyout tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatPeopleEvent {
    /// Closed: by Escape, a press outside, or once its work is done.
    Close,
    /// A 1:1 chat's "Start a group chat": these people (not the account), the chat's partner last.
    StartGroup(Vec<Recipient>),
    /// A new group chat's "Add people": these join its people.
    AddToDraft(Vec<Recipient>),
    /// A person's row clicked: the chat with them alone, as Teams opens it.
    OpenChat(Recipient),
}

/// The flyout and its picker (see the module docs).
pub struct ChatPeople {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    list: Entity<ChatList>,
    presence: Entity<Presences>,
    cards: Entity<CardAsks>,
    /// The chat (empty for a new group chat not made yet).
    chat: String,
    of: PeopleOf,
    /// The account first, then the others as the store lists them (a 1:1 chat's partner when it lists nobody else).
    members: Vec<Person>,
    /// The chat's change ([`Chats::changed_at`]) its people were last read at: a later one reads them again, so the
    /// open flyout follows people added or gone as Teams' does.
    read_at: u64,
    /// A 1:1 chat's partner, as the chat itself names them: always the last of a group chat started from it.
    partner: Option<Recipient>,
    /// The picker is open.
    picking: bool,
    query: Entity<InputState>,
    days: Entity<InputState>,
    picked: Vec<Picked>,
    suggestions: Vec<Picked>,
    highlighted: Option<usize>,
    /// The directory's people for what the box said when they were asked.
    found_people: Vec<Person>,
    history: HistoryChoice,
    problem: Option<SharedString>,
    asking: Option<Task<()>>,
    loading: Option<Task<()>>,
    /// The chat's people were read once: a later read (a change of the chat) keeps them shown and Add lit meanwhile.
    read: bool,
    adding: Option<Task<()>>,
    /// Where the picker, and its suggestions hanging under it, were last drawn: a press there isn't one outside.
    picker_at: Placed,
    panel_at: Placed,
    /// Where the button that opens the flyout was last drawn: its own click toggles it.
    button_at: Placed,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// Where an element was last drawn, kept by whoever draws it ([`Placed::keep`]).
#[derive(Clone, Debug, Default)]
pub struct Placed(Rc<Cell<Option<Bounds<Pixels>>>>);

impl Placed {
    /// `element`, its bounds kept here each time it is drawn.
    pub fn keep(&self, element: impl IntoElement) -> gpui::Div {
        let placed = self.0.clone();
        div()
            .on_children_prepainted(move |children, _, _| placed.set(children.first().copied()))
            .child(element)
    }

    pub(crate) fn holds(&self, point: gpui::Point<Pixels>) -> bool {
        self.0.get().is_some_and(|bounds| bounds.contains(&point))
    }

    fn set(&self, bounds: Option<Bounds<Pixels>>) {
        self.0.set(bounds);
    }

    fn forget(&self) {
        self.0.set(None);
    }
}

/// A 1:1 chat's partner as the chat names them: the list row's (its name, its picture), else the one its id names.
fn partner_of(list: &Entity<ChatList>, chat: &str, me: &str, cx: &gpui::App) -> Option<Recipient> {
    let row = list.read(cx).row(chat);
    // a bot's chat is with no person
    if row.is_some_and(|row| row.bot.is_some()) {
        return None;
    }
    let mri = row
        .and_then(|row| row.partner.clone())
        .or_else(|| chat_list::one_to_one_partner(chat, me))
        .filter(|mri| mri != me)?;
    let named = row.filter(|row| row.named);
    Some(Recipient {
        name: named.map_or_else(|| SharedString::from(chat_list::unnamed(&mri)), |row| row.name.clone()),
        picture: named.and_then(|row| row.picture.clone()),
        mri,
    })
}

impl EventEmitter<ChatPeopleEvent> for ChatPeople {}

/// What the flyout is made with.
pub struct ChatPeopleParts {
    pub core: Arc<dyn Core>,
    pub trace: Arc<ViewTrace>,
    pub list: Entity<ChatList>,
    /// What changed in the account's chats: a change of this chat reads its people again.
    pub chats: Entity<Chats>,
    pub presence: Entity<Presences>,
    /// Where a person's face asks for their profile card.
    pub cards: Entity<CardAsks>,
    /// Where the button that opens it is drawn.
    pub button: Placed,
}

impl ChatPeople {
    /// The flyout of `chat`'s people; for a new group chat not made yet (`PeopleOf::Draft`), `draft` are its people
    /// besides the account.
    pub fn new(
        parts: ChatPeopleParts,
        chat: String,
        of: PeopleOf,
        draft: Vec<Recipient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(QUERY_HINT));
        let days = cx.new(|cx| InputState::new(window, cx).default_value("1"));
        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_query(event, window, cx);
            }),
            cx.subscribe_in(&days, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.keep_digits(window, cx);
                }
            }),
            cx.observe(&parts.presence, |_, _, cx| cx.notify()),
            cx.observe(&parts.chats, |this: &mut Self, chats, cx| this.on_chats(&chats, cx)),
        ];
        let read_at = parts.chats.read(cx).changed_at(&chat);
        parts
            .trace
            .record(&TraceLine::new("chat-people", "opened").conversation(&chat));
        let partner = (of == PeopleOf::OneToOne)
            .then(|| partner_of(&parts.list, &chat, parts.core.me(), cx))
            .flatten();
        let mut this = Self {
            core: parts.core,
            trace: parts.trace,
            list: parts.list,
            presence: parts.presence,
            cards: parts.cards,
            chat,
            of,
            members: Vec::new(),
            read_at,
            partner,
            picking: false,
            query,
            days,
            picked: Vec::new(),
            suggestions: Vec::new(),
            highlighted: None,
            found_people: Vec::new(),
            history: HistoryChoice::None,
            problem: None,
            asking: None,
            loading: None,
            read: false,
            adding: None,
            picker_at: Placed::default(),
            panel_at: Placed::default(),
            button_at: parts.button,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.load(draft, cx);
        this
    }

    /// The account's chats changed: a change of this chat (people added or gone among what it may be) reads its people
    /// again. A new group chat not made yet has no people to read.
    fn on_chats(&mut self, chats: &Entity<Chats>, cx: &mut Context<Self>) {
        if self.of == PeopleOf::Draft {
            return;
        }
        let at = chats.read(cx).changed_at(&self.chat);
        if at > self.read_at {
            self.read_at = at;
            self.load(Vec::new(), cx);
        }
    }

    /// Reads the chat's people (a new group chat's are given), the account first, and asks their presence. A 1:1
    /// chat the store lists nobody else in has the partner its id names, as the chat list shows it.
    fn load(&mut self, mut draft: Vec<Recipient>, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        let me = core.me().to_owned();
        let (chat, of) = (self.chat.clone(), self.of);
        let partner = self.partner.clone();
        // a read again asks only the presence of the people it didn't show before
        let watched: Vec<String> = self.members.iter().map(|person| person.mri.clone()).collect();
        self.loading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let mut mris: Vec<String> = if of == PeopleOf::Draft {
                draft.iter().map(|person| person.mri.clone()).collect()
            } else {
                core.members(chat)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|member| member.mri)
                    .collect()
            };
            mris.retain(|mri| *mri != me);
            if let Some(partner) = partner.filter(|_| mris.is_empty()) {
                mris.push(partner.mri.clone());
                draft.push(partner);
            }
            mris.insert(0, me);
            let mut members = Vec::with_capacity(mris.len());
            for mri in &mris {
                let shown = core.person(mri.clone()).await.map(Shown::of);
                let given = draft.iter().find(|person| person.mri == *mri);
                let person = match (shown, given) {
                    (Ok(shown), _) if shown.name.is_some() => Person::of(mri, shown),
                    (_, Some(given)) => Person {
                        mri: mri.clone(),
                        name: given.name.clone(),
                        named: true,
                        picture: given.picture.clone(),
                        detail: SharedString::default(),
                    },
                    (shown, None) => Person::of(
                        mri,
                        shown.unwrap_or(Shown {
                            name: None,
                            from: None,
                            picture: None,
                        }),
                    ),
                };
                members.push(person);
            }
            crate::presence::watch(core.as_ref(), mris.iter().skip(1).filter(|mri| !watched.contains(mri)));
            let _ = this.update(cx, |this, cx| {
                this.members = members;
                this.loading = None;
                this.read = true;
                this.refresh_suggestions(cx);
                cx.notify();
            });
        }));
    }

    /// The chat whose people these are (empty for a new group chat not made yet).
    #[must_use]
    pub fn chat(&self) -> &str {
        &self.chat
    }

    /// The chat's people as read, the account first.
    #[must_use]
    pub fn members(&self) -> &[Person] {
        &self.members
    }

    /// Whose people these are.
    #[must_use]
    pub fn of(&self) -> PeopleOf {
        self.of
    }

    /// Whether the picker is open.
    #[must_use]
    pub fn picking(&self) -> bool {
        self.picking
    }

    /// The people and chats taken in the picker.
    #[must_use]
    pub fn picked(&self) -> &[Picked] {
        &self.picked
    }

    /// The picker's suggestions as they stand.
    #[must_use]
    pub fn suggestions(&self) -> &[Picked] {
        &self.suggestions
    }

    /// The history choice.
    #[must_use]
    pub fn history(&self) -> HistoryChoice {
        self.history
    }

    /// What went wrong with the last add, while it is shown.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// Whether an add is going out.
    #[must_use]
    pub fn is_adding(&self) -> bool {
        self.adding.is_some()
    }

    /// The flyout takes the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    /// Opens the picker, its box taking the keyboard; nothing in the read-only mode.
    pub fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        self.picking = true;
        self.problem = None;
        self.query.update(cx, |query, cx| query.focus(window, cx));
        self.refresh_suggestions(cx);
        cx.notify();
    }

    /// Closes the picker (its people dropped), or the flyout when no picker is open.
    pub fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picking && self.adding.is_none() {
            self.close_picker(window, cx);
        } else if !self.picking {
            cx.emit(ChatPeopleEvent::Close);
        }
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picking = false;
        self.picked.clear();
        self.found_people.clear();
        self.suggestions.clear();
        self.highlighted = None;
        self.history = HistoryChoice::None;
        self.problem = None;
        self.query.update(cx, |query, cx| query.set_value("", window, cx));
        self.days.update(cx, |days, cx| days.set_value("1", window, cx));
        self.picker_at.forget();
        self.panel_at.forget();
        self.focus(window, cx);
        cx.notify();
    }

    // --- the box ---

    fn on_query(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.highlighted = None;
                self.ask_directory(cx);
                self.refresh_suggestions(cx);
            }
            InputEvent::PressEnter { .. } => {
                if self.suggestions.is_empty() {
                    if self.query_text(cx).is_empty() {
                        self.confirm(window, cx);
                    }
                } else {
                    let at = self.highlighted.unwrap_or(0);
                    self.take(at, window, cx);
                }
            }
            InputEvent::Focus | InputEvent::Blur => cx.notify(),
        }
    }

    fn query_text(&self, cx: &gpui::App) -> String {
        self.query.read(cx).value().trim().to_owned()
    }

    /// Puts `text` in the box as if it were typed.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |query, cx| query.set_value(text.to_owned(), window, cx));
        self.highlighted = None;
        self.ask_directory(cx);
        self.refresh_suggestions(cx);
    }

    /// Asks the directory for the people the box names, a moment after the typing stops; an answer to what the box
    /// no longer says is dropped.
    fn ask_directory(&mut self, cx: &mut Context<Self>) {
        let query = self.query_text(cx);
        self.found_people.clear();
        if query.is_empty() {
            self.asking = None;
            return;
        }
        let core = Arc::clone(&self.core);
        self.asking = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            cx.background_executor().timer(PAUSE).await;
            let answer = core
                .search_service(query.clone(), SearchScope::Suggestions)
                .await
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.query_text(cx) != query {
                    return;
                }
                this.found_people = answer
                    .people
                    .into_iter()
                    .filter_map(|person| {
                        let mri = person.mri.filter(|mri| mri.starts_with("8:orgid:"))?;
                        let detail = person.job_title.or(person.email).unwrap_or_default();
                        Some(Person {
                            mri,
                            name: person.name.into(),
                            named: true,
                            picture: None,
                            detail: detail.into(),
                        })
                    })
                    .collect();
                this.refresh_suggestions(cx);
            });
        }));
    }

    /// The MRIs nobody may be offered: the account, the chat's people, those taken.
    fn taken(&self) -> Vec<String> {
        let mut taken: Vec<String> = self.members.iter().map(|person| person.mri.clone()).collect();
        taken.push(self.core.me().to_owned());
        for picked in &self.picked {
            if let Picked::Person(person) = picked {
                taken.push(person.mri.clone());
            }
        }
        taken
    }

    /// The suggestions again from what the box says (see the module docs).
    fn refresh_suggestions(&mut self, cx: &mut Context<Self>) {
        let query = self.query_text(cx);
        if query.is_empty() {
            self.suggestions.clear();
            self.highlighted = None;
            cx.notify();
            return;
        }
        let taken = self.taken();
        let list = self.list.read(cx);
        let partners: Vec<&ChatRow> = list
            .findable_rows()
            .filter(|row| row.kind == RowKind::OneToOne && row.named)
            .filter(|row| {
                row.partner
                    .as_deref()
                    .is_some_and(|mri| !taken.iter().any(|t| t == mri))
            })
            .collect();
        let named: Vec<(String, SharedString)> =
            partners.iter().map(|row| (row.id.clone(), row.name.clone())).collect();
        let mut suggestions: Vec<Picked> = matches(&query, &named)
            .into_iter()
            .filter_map(|(id, _)| partners.iter().find(|row| row.id == id))
            .map(|row| {
                Picked::Person(Person {
                    mri: row.partner.clone().unwrap_or_default(),
                    name: row.name.clone(),
                    named: true,
                    picture: row.picture.clone(),
                    detail: SharedString::default(),
                })
            })
            .collect();
        for person in &self.found_people {
            let known = suggestions
                .iter()
                .any(|known| matches!(known, Picked::Person(known) if known.mri == person.mri));
            if !known && !taken.contains(&person.mri) {
                suggestions.push(Picked::Person(person.clone()));
            }
        }
        suggestions.truncate(MOST_PEOPLE);
        let groups: Vec<&ChatRow> = list
            .findable_rows()
            .filter(|row| row.kind == RowKind::Group && row.id != self.chat)
            .filter(|row| {
                !self
                    .picked
                    .iter()
                    .any(|picked| matches!(picked, Picked::Chat(chat) if chat.id == row.id))
            })
            .collect();
        let named: Vec<(String, SharedString)> = groups.iter().map(|row| (row.id.clone(), row.name.clone())).collect();
        suggestions.extend(
            matches(&query, &named)
                .into_iter()
                .take(MOST_CHATS)
                .filter_map(|(id, _)| groups.iter().find(|row| row.id == id))
                .map(|row| Picked::Chat((*row).clone())),
        );
        self.suggestions = suggestions;
        if self.highlighted.is_some_and(|at| at >= self.suggestions.len()) {
            self.highlighted = None;
        }
        cx.notify();
    }

    fn move_highlight(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.suggestions.len();
        if count == 0 {
            return;
        }
        let next = match self.highlighted {
            None if by > 0 => 0,
            None => count - 1,
            Some(at) => (at.cast_signed() + by).rem_euclid(count.cast_signed()).cast_unsigned(),
        };
        self.highlighted = Some(next);
        cx.notify();
    }

    /// Takes the suggestion at `at` as a chip.
    pub fn take(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(taken) = self.suggestions.get(at).cloned() else {
            return;
        };
        let kind = if matches!(taken, Picked::Chat(_)) {
            "chat"
        } else {
            "person"
        };
        self.picked.push(taken);
        self.problem = None;
        self.query.update(cx, |query, cx| query.set_value("", window, cx));
        self.found_people.clear();
        self.highlighted = None;
        self.trace.record(&TraceLine::new("chat-people", "picked").kind(kind));
        self.refresh_suggestions(cx);
    }

    /// Takes the chip at `at` off.
    pub fn remove(&mut self, at: usize, cx: &mut Context<Self>) {
        if at < self.picked.len() && self.adding.is_none() {
            self.picked.remove(at);
            self.refresh_suggestions(cx);
        }
    }

    fn backspace(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.query.read(cx).value().is_empty() || self.picked.is_empty() {
            return false;
        }
        self.remove(self.picked.len() - 1, cx);
        true
    }

    // --- the history ---

    /// Chooses how much history the people added see.
    pub fn choose_history(&mut self, choice: HistoryChoice, cx: &mut Context<Self>) {
        self.history = choice;
        cx.notify();
    }

    /// Puts `text` in the number of days' box as if it were typed.
    pub fn set_days(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.days
            .update(cx, |days, cx| days.set_value(text.to_owned(), window, cx));
        self.keep_digits(window, cx);
    }

    /// The number of days' box keeps only digits, as Teams' does.
    fn keep_digits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.days.read(cx).value().to_string();
        let digits: String = value.chars().filter(char::is_ascii_digit).take(5).collect();
        if digits != value {
            self.days.update(cx, |days, cx| days.set_value(digits, window, cx));
        }
        cx.notify();
    }

    /// The number of days typed, when it is one Add takes (1 or more).
    fn days_typed(&self, cx: &gpui::App) -> Option<u32> {
        self.days
            .read(cx)
            .value()
            .parse::<u32>()
            .ok()
            .filter(|days| *days > 0)
            .map(|days| days.min(MOST_DAYS))
    }

    /// The history the Add sends, when its choice can be sent.
    fn chat_history(&self, cx: &gpui::App) -> Option<ChatHistory> {
        match self.history {
            HistoryChoice::None => Some(ChatHistory::None),
            HistoryChoice::Days => self.days_typed(cx).map(ChatHistory::Days),
            HistoryChoice::All => Some(ChatHistory::All),
        }
    }

    /// Whether Create or Add is lit: someone taken, the chat's people read, nothing going out, a 1:1 chat's partner
    /// known and a group chat's history choice one that can be sent.
    #[must_use]
    pub fn can_confirm(&self, cx: &gpui::App) -> bool {
        !self.picked.is_empty()
            && self.adding.is_none()
            && self.read
            && match self.of {
                PeopleOf::OneToOne => self.partner.is_some(),
                PeopleOf::Group => self.chat_history(cx).is_some(),
                PeopleOf::Draft => true,
            }
    }

    // --- confirming ---

    /// Create or Add (see the module docs).
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) || self.core.read_only() {
            return;
        }
        let history = self.chat_history(cx).unwrap_or_default();
        let picked = self.picked.clone();
        let core = Arc::clone(&self.core);
        let me = core.me().to_owned();
        let members: Vec<String> = self.members.iter().map(|person| person.mri.clone()).collect();
        // the partner as the chat names them (their name as read when it was)
        let partner = self.partner.clone().map(|partner| {
            self.members
                .iter()
                .find(|person| person.mri == partner.mri && person.named)
                .map_or(partner, Person::recipient)
        });
        let (of, chat) = (self.of, self.chat.clone());
        self.problem = None;
        self.adding = Some(cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let mut people = expand(&core, &picked).await;
            people.retain(|person| {
                person.mri != me
                    && !members.contains(&person.mri)
                    && partner.as_ref().is_none_or(|partner| partner.mri != person.mri)
            });
            let added = match (of, partner) {
                // nobody new: nothing to start or add
                (PeopleOf::OneToOne | PeopleOf::Group, _) if people.is_empty() => Some(ChatPeopleEvent::Close),
                (PeopleOf::OneToOne, Some(partner)) => {
                    people.push(partner);
                    Some(ChatPeopleEvent::StartGroup(people))
                }
                (PeopleOf::OneToOne, None) => None,
                (PeopleOf::Draft, _) => Some(ChatPeopleEvent::AddToDraft(people)),
                (PeopleOf::Group, _) => {
                    let action = Action::AddMembers {
                        conversation: chat.clone(),
                        members: people.iter().map(|person| Mri::new(&person.mri)).collect(),
                        history,
                    };
                    settled(&core, core.write(action), cx)
                        .await
                        .then_some(ChatPeopleEvent::Close)
                }
            };
            let _ = this.update_in(cx, |this, window, cx| this.confirmed(added, window, cx));
        }));
        cx.notify();
    }

    /// What a confirm came to: `added` told to the shell and the picker closed, or none and why said.
    fn confirmed(&mut self, added: Option<ChatPeopleEvent>, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = None;
        let Some(event) = added else {
            self.problem = Some(NOT_ADDED.into());
            cx.notify();
            return;
        };
        if self.of == PeopleOf::Group {
            self.trace
                .record(&TraceLine::new("chat-people", "added").conversation(&self.chat));
        }
        self.close_picker(window, cx);
        cx.emit(event);
    }

    // --- drawing ---

    fn member_row(&self, at: usize, person: &Person, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let me = person.mri == self.core.me();
        let availability = self.presence.read(cx).of(&person.mri);
        let badge = crate::presence_badge::badge(&availability, 12.0, palette)
            .map(|badge| div().absolute().right(px(-2.0)).bottom(px(-2.0)).child(badge));
        let face = div()
            .relative()
            .flex_none()
            .child(people::avatar(
                person.named.then_some(&person.name),
                person.picture.as_ref(),
                Size::Size(px(FACE)),
            ))
            .children(badge);
        // a rest on the face opens their compact card, as Teams' does
        let of = CardOf::new(person.mri.clone(), person.named.then(|| person.name.clone()))
            .with_picture(person.picture.clone());
        let face = profile_card::anchor(
            format!("profile-card-from-people-{at}"),
            of,
            profile_card::Click::Hover,
            &self.cards,
            face,
        );
        let recipient = person.recipient();
        div()
            .id(("chat-people-member", at))
            .debug_selector(move || format!("chat-people-member-{at}"))
            .w_full()
            .min_h(px(48.0))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            // another's row opens the chat with them alone
            .when(!me, |row| {
                row.cursor_pointer()
                    .hover(move |row| row.bg(rgb(palette.hover)))
                    .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(ChatPeopleEvent::OpenChat(recipient.clone()));
                    }))
            })
            .child(face)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_sm().truncate().child(person.name.clone()))
                    .when(me, |lines| {
                        lines.child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(YOU))
                    }),
            )
            .into_any_element()
    }

    fn action_row(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.core.read_only() {
            return None;
        }
        let (id, label) = match self.of {
            PeopleOf::OneToOne => ("chat-people-start-group", START_GROUP),
            PeopleOf::Group | PeopleOf::Draft => ("chat-people-add", ADD_PEOPLE),
        };
        Some(
            crate::shell::menu_row(id, palette)
                .when(self.picking, |row| row.bg(rgb(palette.hover)))
                .child(Glyph::USER_PLUS.icon().size(px(16.0)))
                .child(div().flex_1().text_sm().child(label))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_picker(window, cx)))
                .into_any_element(),
        )
    }

    /// The flyout; the picker, while it is open, hangs from under its action ([`hung_under`]).
    fn flyout(&self, picker: Option<AnyElement>, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let title = match self.of {
            PeopleOf::OneToOne => SharedString::from("People"),
            PeopleOf::Group | PeopleOf::Draft => format!("People ({})", self.members.len()).into(),
        };
        let rows: Vec<AnyElement> = self
            .members
            .iter()
            .enumerate()
            .map(|(at, person)| self.member_row(at, person, palette, cx))
            .collect();
        let action = self.action_row(palette, cx);
        div()
            .id("chat-people")
            .debug_selector(|| "chat-people".to_owned())
            .occlude()
            .w(px(FLYOUT_WIDTH))
            .max_h(px(440.0))
            .overflow_y_scroll()
            .p_2()
            .flex()
            .flex_col()
            .gap_0p5()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .child(
                div()
                    .px_2()
                    .pt_1()
                    .pb_1()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .children(rows)
            .when_some(action, |flyout, action| {
                flyout
                    .child(div().mx_2().my_1().h(px(1.0)).bg(rgb(palette.border)))
                    .child(action)
                    .children(picker.map(hung_under))
            })
            .into_any_element()
    }

    fn chip(at: usize, picked: &Picked, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let face = match picked {
            Picked::Person(person) => {
                people::avatar(Some(&person.name), person.picture.as_ref(), Size::Small).into_any_element()
            }
            Picked::Chat(row) => chat_list::chat_avatar(row, palette, Size::Size(px(22.0)), "chat-people-avatar"),
        };
        let remove = div()
            .id(SharedString::from(format!("chat-people-chip-remove-{at}")))
            .debug_selector(move || format!("chat-people-chip-remove-{at}"))
            .flex_none()
            .p_0p5()
            .rounded_sm()
            .cursor_pointer()
            .text_color(rgb(palette.muted_foreground))
            .hover(move |button| button.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.remove(at, cx)))
            .child(Glyph::X.icon().size(px(12.0)));
        div()
            .debug_selector(move || format!("chat-people-chip-{at}"))
            .flex_none()
            .h(px(28.0))
            .pl_0p5()
            .pr_1p5()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_full()
            .bg(rgb(palette.surface))
            .child(face)
            .child(div().text_sm().child(picked.name()))
            .child(remove)
            .into_any_element()
    }

    fn suggestion(&self, at: usize, suggested: &Picked, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let highlighted = self.highlighted == Some(at);
        let (face, detail) = match suggested {
            Picked::Person(person) => (
                people::avatar(Some(&person.name), person.picture.as_ref(), Size::Size(px(FACE))).into_any_element(),
                person.detail.clone(),
            ),
            Picked::Chat(row) => (
                chat_list::chat_avatar(row, palette, Size::Size(px(FACE)), "chat-people-avatar"),
                SharedString::default(),
            ),
        };
        div()
            .id(SharedString::from(format!("chat-people-suggestion-{at}")))
            .debug_selector(move || format!("chat-people-suggestion-{at}"))
            .w_full()
            .min_h(px(SUGGESTION_HEIGHT))
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .when(highlighted, |row| row.bg(rgb(palette.selected)))
            .hover(move |row| row.bg(rgb(palette.hover)))
            // a press would take the keyboard from the box before the click lands
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.take(at, window, cx)))
            .child(face)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_sm().truncate().child(suggested.name()))
                    .when(!detail.is_empty(), |lines| {
                        lines.child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette.muted_foreground))
                                .truncate()
                                .child(detail),
                        )
                    }),
            )
            .into_any_element()
    }

    fn query_line(&self, focused: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chips: Vec<AnyElement> = self
            .picked
            .iter()
            .enumerate()
            .map(|(at, picked)| Self::chip(at, picked, palette, cx))
            .collect();
        div()
            .id("chat-people-query-line")
            .debug_selector(|| "chat-people-query".to_owned())
            .w_full()
            .min_h(px(36.0))
            .px_2()
            .py_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .rounded_t_md()
            .bg(rgb(palette.hover))
            .border_b_2()
            .border_color(rgb(if focused { palette.brand } else { palette.border }))
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                if !this.suggestions.is_empty() {
                    this.move_highlight(-1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                if !this.suggestions.is_empty() {
                    this.move_highlight(1, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.escape(window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &Backspace, _, cx| {
                if this.backspace(cx) {
                    cx.stop_propagation();
                }
            }))
            .children(chips)
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.0))
                    .child(Input::new(&self.query).appearance(false).aria_label(QUERY_HINT)),
            )
            .into_any_element()
    }

    fn history_choices(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let radio = |id: &'static str, choice: HistoryChoice, label: &'static str, cx: &mut Context<Self>| {
            let chosen = self.history == choice;
            selected_by(
                id,
                Radio::new(id)
                    .label(label)
                    .checked(chosen)
                    .small()
                    .on_click(cx.listener(move |this, _: &bool, _, cx| this.choose_history(choice, cx))),
            )
        };
        let days = div()
            .debug_selector(|| "chat-people-days".to_owned())
            .w(px(44.0))
            .child(Input::new(&self.days).small().aria_label("Number of days"));
        div()
            .debug_selector(|| "chat-people-history".to_owned())
            .flex()
            .flex_col()
            .gap_1()
            .text_sm()
            .child(radio("chat-people-history-none", HistoryChoice::None, NO_HISTORY, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(radio("chat-people-history-days", HistoryChoice::Days, SOME_HISTORY, cx))
                    .child(days),
            )
            .child(radio("chat-people-history-all", HistoryChoice::All, ALL_HISTORY, cx))
            .text_color(rgb(palette.foreground))
            .into_any_element()
    }

    fn picker(&self, window: &Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.query.read(cx).focus_handle(cx).is_focused(window);
        let title = match self.of {
            PeopleOf::OneToOne => START_GROUP,
            PeopleOf::Group | PeopleOf::Draft => ADD_TITLE,
        };
        let confirm_label = if self.of == PeopleOf::OneToOne { "Create" } else { "Add" };
        let busy = self.adding.is_some();
        let shown = self.suggestions.len();
        let panel = self.suggestions_panel(palette, cx);
        let history = (self.of == PeopleOf::Group).then(|| self.history_choices(palette, cx));
        let problem = self.problem.clone().map(|problem| {
            div()
                .debug_selector(|| "chat-people-problem".to_owned())
                .text_xs()
                .text_color(rgb(theme::DANGER))
                .child(problem)
        });
        let cancel = selected_by(
            "chat-people-cancel",
            Button::new("chat-people-cancel")
                .label("Cancel")
                .outline()
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.close_picker(window, cx))),
        );
        let confirm = selected_by(
            "chat-people-confirm",
            Button::new("chat-people-confirm")
                .label(if busy { ADDING } else { confirm_label })
                .primary()
                .disabled(!self.can_confirm(cx))
                .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
        );
        // the suggestions hang out of the picker's bounds: where they are drawn is kept too
        let panel_at = self.panel_at.clone();
        let picker = div()
            .on_children_prepainted(move |children, _, _| {
                panel_at.set((shown > 0).then(|| children.last().copied()).flatten());
            })
            .id("chat-people-picker")
            .debug_selector(|| "chat-people-picker".to_owned())
            .occlude()
            .relative()
            .w(px(PICKER_WIDTH))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_lg()
            .shadow_lg()
            .child(div().text_sm().child(title))
            .child(self.query_line(focused, palette, cx))
            .children(history)
            .children(problem)
            .child(div().flex().justify_end().gap_2().child(cancel).child(confirm))
            .children(panel);
        with_reach(self.picker_at.keep(picker), shown)
    }

    /// The suggestions, hanging under the picker's box over what follows it, while there are any.
    fn suggestions_panel(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.suggestions.is_empty() {
            return None;
        }
        let suggestions: Vec<AnyElement> = self
            .suggestions
            .iter()
            .enumerate()
            .map(|(at, suggested)| self.suggestion(at, suggested, palette, cx))
            .collect();
        Some(
            div()
                .id("chat-people-suggestions")
                .debug_selector(|| "chat-people-suggestions".to_owned())
                .occlude()
                .absolute()
                .top(px(PANEL_TOP))
                .left(px(16.0))
                .right(px(16.0))
                .max_h(px(PANEL_MOST))
                .overflow_y_scroll()
                .p_1()
                .bg(rgb(palette.surface))
                .border_1()
                .border_color(rgb(palette.border))
                .rounded_lg()
                .shadow_lg()
                .children(suggestions)
                .into_any_element(),
        )
    }
}

/// `picker` beside a zero-wide strut as tall as its `shown` suggestions reach, so it is kept in the window with them.
fn with_reach(picker: gpui::Div, shown: usize) -> AnyElement {
    let reach = if shown == 0 {
        0.0
    } else {
        let rows = f32::from(u16::try_from(shown).unwrap_or(u16::MAX));
        PANEL_TOP + (10.0 + SUGGESTION_HEIGHT * rows).min(PANEL_MOST)
    };
    div()
        .flex()
        .items_start()
        .child(picker)
        .child(div().w_0().h(px(reach)))
        .into_any_element()
}

/// The people `picked` stand for, in order, each once: a person, or a group chat's people (read from the store).
async fn expand(core: &Arc<dyn Core>, picked: &[Picked]) -> Vec<Recipient> {
    let mut people: Vec<Recipient> = Vec::new();
    for taken in picked {
        let more: Vec<Recipient> = match taken {
            Picked::Person(person) => vec![person.recipient()],
            Picked::Chat(row) => {
                let mut members = Vec::new();
                for member in core.members(row.id.clone()).await.unwrap_or_default() {
                    let shown = core.person(member.mri.clone()).await.map(Shown::of).ok();
                    let name = shown
                        .as_ref()
                        .and_then(|shown| shown.name.clone())
                        .unwrap_or_else(|| SharedString::from(chat_list::unnamed(&member.mri)));
                    members.push(Recipient {
                        mri: member.mri,
                        name,
                        picture: shown.and_then(|shown| shown.picture),
                    });
                }
                members
            }
        };
        for person in more {
            if !people.iter().any(|known| known.mri == person.mri) {
                people.push(person);
            }
        }
    }
    people
}

/// Whether an add's outbox row went out, once the outbox settled it ([`settled_row`]).
async fn settled(
    core: &Arc<dyn Core>,
    reply: teams_core::Reply<teams_store::OutboxEntry>,
    cx: &mut AsyncWindowContext,
) -> bool {
    settled_row(core.as_ref(), reply, cx.background_executor())
        .await
        .is_some_and(|row| row.state == OutboxState::Sent)
}

/// `picker` over the flyout's left, from just under its action wherever the flyout is scrolled, as Teams lays it;
/// drawn over everything and kept in the window.
fn hung_under(picker: AnyElement) -> gpui::Div {
    div().h_0().child(
        gpui::deferred(
            gpui::anchored()
                .anchor(gpui::Anchor::TopRight)
                // from the flyout's padded inside (its own left is 8 px further), over the action's foot
                .offset(gpui::point(px(PICKER_OVER - 8.0), px(-6.0)))
                .snap_to_window_with_margin(px(8.0))
                .child(picker),
        )
        .with_priority(2),
    )
}

impl Focusable for ChatPeople {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ChatPeople {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let picker = self.picking.then(|| self.picker(window, palette, cx));
        let flyout = self.flyout(picker, palette, cx);
        div()
            .id("chat-people-layer")
            .track_focus(&self.focus)
            .key_context("ChatPeople")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.escape(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                // the picker and its suggestions hang out of the flyout's bounds, and the button's own click toggles it
                let inside = [&this.picker_at, &this.panel_at, &this.button_at]
                    .iter()
                    .any(|placed| placed.holds(event.position));
                if this.adding.is_none() && !inside {
                    cx.emit(ChatPeopleEvent::Close);
                }
            }))
            .relative()
            .flex()
            .flex_col()
            .items_end()
            .child(flyout)
    }
}
