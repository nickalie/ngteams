//! New message (the list header's New message, Ctrl+N), as browser Teams' is (seen 2026-11-06 with lab accounts):
//! the conversation pane under a To: line, and a "New message" row selected at the top of the chats.
//!
//! - **To:** takes people one by one, each a chip with its picture and name and a remove button; Backspace in the empty
//!   box takes the last one off. What is typed is matched as Teams' picker matches it: the people the account has a
//!   1:1 chat with and the people the directory finds (the service's suggestions, a moment after the typing stops),
//!   then the group chats whose names hold it, the empty ones the list leaves out among them
//!   ([`ChatList::findable_rows`]); with nothing typed, the people of the latest 1:1 chats. Up and Down move in them,
//!   Enter takes the one highlighted (else the first), Escape closes them. A group chat chosen there is opened.
//! - **Add group name** (the chevron at the To: line's end, "Group name:" over it): a named chat is always a new group
//!   chat, as Teams' tooltip says ("Add group name and create a new group chat"); the chevron takes the line away
//!   again while nothing is typed in it.
//! - **The chat it is**, as Teams finds it while the people change: one person's 1:1 chat when the account has one,
//!   or the unnamed group chat of exactly those people ([`Core::group_chat_with`]: the one whose roster came last, as
//!   Teams picked the newest of five a–b–c chats over the one with the latest message, and kept to it once a named
//!   a–b–c chat was made after it) unless a group name is typed. That
//!   chat is shown under the line, with its own compose box, and a message sent there opens it. Otherwise the pane
//!   says "You're starting a new conversation" over New message's own box.
//! - **The first message** of a new chat makes it through the outbox ([`Action::CreateChat`]: the people, and the
//!   group name as its topic, the request Teams sent), waits for the outbox to settle that row, then sends the message
//!   to the chat made ([`Action::Send`]) and opens it. A chat that couldn't be made gives the message back to the box
//!   with what went wrong. A named chat needs at least two other people, as Teams sends none with one. While the
//!   chat is being made its people stay as they were sent (none taken or added, on To: or from a new group chat's
//!   people), so nobody is shown in a chat made without them. Leaving New
//!   message meanwhile doesn't call it off: the shell keeps it, out of sight, until the message is sent (the chat made
//!   is then not opened over the place shown) or given back, which New message opened again shows
//!   ([`NewChat::keeps_going`]).
//!
//! - **A new group chat from a 1:1 chat** ([`NewChat::start_draft`]: its header's "Start a group chat" when the
//!   account has no unnamed group chat of exactly those people) is New message with its people fixed, as Teams draws
//!   it: no To: line, but a chat's header (the group's face, the people's first names, the people button with their
//!   count, whose "Add people" adds to them: [`crate::chat_people`]), and its row in the list named after them
//!   ([`NewChat::draft_row`]); its first message makes the chat as above, the people in the order Teams sends them
//!   (those added, the 1:1 chat's partner last).
//!
//! Nothing is written in the read-only mode: the shell offers no New message there. Traced: `new-chat opened`,
//! `new-chat found` (the chat it is, or none), `new-chat started` (the chat made), never a name or a text.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncWindowContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    Focusable as _, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::Selectable as _;
use gpui_component::input::{Backspace, Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::{Sizable as _, Size};
use teams_core::{Action, SearchScope};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_store::OutboxState;

use crate::bridge::Core;
use crate::chat_list::{self, ChatList, ChatRow, RowKind};
use crate::chat_people::{ChatPeople, ChatPeopleEvent, ChatPeopleParts, PEOPLE_TIP, PeopleOf, Placed};
use crate::chat_view::ChatView;
use crate::compose::{Compose, ComposeEvent, ComposeParts};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::people::{self, Picture};
use crate::sending::settled_row;
use crate::shell::ShellParts;
use crate::switcher::matches;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The To: box's hint.
pub const TO_HINT: &str = "Enter name or chat";
/// What the pane says while no chat is found for the people, as Teams says it.
pub const STARTING: &str = "You're starting a new conversation";
/// And under it.
pub const STARTING_HINT: &str = "Type your first message below.";
/// The group name's label, and its box's hint.
pub const GROUP_NAME: &str = "Group name:";
/// The chevron's tooltip, as Teams words it.
pub const ADD_GROUP_NAME: &str = "Add group name";
/// The chevron's tooltip while the group name is shown.
pub const REMOVE_GROUP_NAME: &str = "Remove group name";
/// The row New message adds at the top of the chats while it is open, as Teams names it.
pub const ROW_NAME: &str = "New message";
/// What the box says when the message names nobody.
pub const NOBODY: &str = "Add someone to send this message to.";
/// What it says when a named chat has one other person.
pub const NAMED_NEEDS_TWO: &str = "Add at least two people to a named group chat.";
/// What it says when the chat couldn't be made.
pub const NOT_STARTED: &str = "We couldn't start the chat. Check your connection and try again.";

/// How long typing rests before the directory is asked.
const PAUSE: Duration = Duration::from_millis(200);
/// The most people, and the most group chats, the suggestions list.
const MOST_PEOPLE: usize = 6;
const MOST_CHATS: usize = 6;
/// The people of the latest 1:1 chats offered with nothing typed.
const RECENT_PEOPLE: usize = 5;
/// A suggestion's face, px.
const FACE: f32 = 32.0;
/// The suggestions' most height before they scroll, px.
const SUGGESTIONS_MAX: f32 = 320.0;

/// Someone the message goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipient {
    pub mri: String,
    pub name: SharedString,
    pub picture: Option<Picture>,
}

/// One of the To: line's suggestions.
#[derive(Clone, Debug)]
pub enum Suggested {
    /// Someone to add: their name, and what the directory says of them.
    Person {
        mri: String,
        name: SharedString,
        detail: SharedString,
        picture: Option<Picture>,
    },
    /// A group chat to open.
    Chat(ChatRow),
}

/// What New message tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewChatEvent {
    /// A chat to open in its place: one chosen from the suggestions, one a message was just sent to.
    Open(String),
    /// A new group chat's people changed: its row in the list follows them.
    DraftChanged,
}

/// What New message is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// New message: its To: line.
    NewMessage,
    /// A new group chat from a 1:1 chat ([`NewChat::start_draft`]).
    GroupDraft,
}

/// The chat the people are, while one is found, shown under the line.
struct Found {
    id: String,
    view: Entity<ChatView>,
    _sent: Subscription,
}

/// New message (see the module docs).
pub struct NewChat {
    parts: ShellParts,
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    list: Entity<ChatList>,
    to: Entity<InputState>,
    name: Entity<InputState>,
    /// The group name's line is shown.
    naming: bool,
    people: Vec<Recipient>,
    suggestions: Vec<Suggested>,
    /// The suggestions are open (the To: box has the keyboard).
    suggesting: bool,
    highlighted: Option<usize>,
    /// The directory's people for what the box said when they were asked.
    found_people: Vec<Suggested>,
    asking: Option<Task<()>>,
    /// Which chat the people are, being looked up.
    resolving: Option<Task<()>>,
    found: Option<Found>,
    /// New message's own box, for a chat not made yet.
    draft: Entity<Compose>,
    starting: Option<Task<()>>,
    /// A first message given back since New message was last shown.
    given_back: bool,
    /// New message's own, or a new group chat from a 1:1 chat (its people fixed, its header drawn in place of To:).
    kind: Kind,
    /// Its people flyout, while it is open.
    people_flyout: Option<(Entity<ChatPeople>, Subscription)>,
    /// Where its header's people button was last drawn.
    people_button: Placed,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NewChatEvent> for NewChat {}

impl NewChat {
    pub fn new(parts: ShellParts, list: Entity<ChatList>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        parts.trace.record(&TraceLine::new("new-chat", "opened"));
        let to = cx.new(|cx| InputState::new(window, cx).placeholder(TO_HINT));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(""));
        let compose_parts = ComposeParts {
            core: Arc::clone(&parts.core),
            settings: parts.settings.clone(),
            trace: Arc::clone(&parts.trace),
            presence: parts.models.presence.clone(),
        };
        let draft = cx.new(|cx| {
            let mut compose = Compose::new(compose_parts, String::new(), window, cx);
            compose.for_new_chat(cx);
            compose
        });
        let subscriptions = vec![
            cx.subscribe_in(&to, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_to(event, window, cx);
            }),
            cx.subscribe_in(&name, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.resolve(window, cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &draft,
                window,
                |this: &mut Self, _, event: &ComposeEvent, window, cx| {
                    if let ComposeEvent::Start(text) = event {
                        this.start(text.as_ref().clone(), window, cx);
                    }
                },
            ),
            // the list's rows change under the suggestions: they follow
            cx.observe(&list, |this: &mut Self, _, cx| this.refresh_suggestions(cx)),
        ];
        Self {
            core: Arc::clone(&parts.core),
            trace: Arc::clone(&parts.trace),
            parts,
            list,
            to,
            name,
            naming: false,
            people: Vec::new(),
            suggestions: Vec::new(),
            suggesting: false,
            highlighted: None,
            found_people: Vec::new(),
            asking: None,
            resolving: None,
            found: None,
            draft,
            starting: None,
            given_back: false,
            kind: Kind::NewMessage,
            people_flyout: None,
            people_button: Placed::default(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Gives the To: box the keyboard, its suggestions open.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.given_back = false;
        self.to.update(cx, |to, cx| to.focus(window, cx));
        self.suggesting = true;
        self.refresh_suggestions(cx);
    }

    /// The people the message goes to, by MRI, in the order they were added.
    #[must_use]
    pub fn people(&self) -> Vec<&str> {
        self.people.iter().map(|person| person.mri.as_str()).collect()
    }

    /// The chat the people are, while one is found.
    #[must_use]
    pub fn found(&self) -> Option<&str> {
        self.found.as_ref().map(|found| found.id.as_str())
    }

    /// The suggestions under the To: line, as they stand.
    #[must_use]
    pub fn suggestions(&self) -> &[Suggested] {
        &self.suggestions
    }

    /// Whether the group name's line is shown.
    #[must_use]
    pub fn naming(&self) -> bool {
        self.naming
    }

    /// The box the next message is written in: the found chat's, else New message's own.
    #[must_use]
    pub fn compose(&self, cx: &gpui::App) -> Entity<Compose> {
        self.found
            .as_ref()
            .map_or_else(|| self.draft.clone(), |found| found.view.read(cx).compose().clone())
    }

    /// The group name typed, trimmed; empty while the line is hidden.
    fn group_name(&self, cx: &gpui::App) -> String {
        if self.naming {
            self.name.read(cx).value().trim().to_owned()
        } else {
            String::new()
        }
    }

    // --- a new group chat from a 1:1 chat ---

    /// Makes this a new group chat of `people` (see the module docs), its box taking the keyboard.
    pub fn start_draft(&mut self, people: Vec<Recipient>, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = Kind::GroupDraft;
        self.naming = false;
        self.suggesting = false;
        self.people = people;
        self.to.update(cx, |to, cx| to.set_value("", window, cx));
        self.trace.record(&TraceLine::new("new-chat", "drafted"));
        self.resolve(window, cx);
        let compose = self.compose(cx);
        compose.update(cx, |compose, cx| compose.focus(window, cx));
        cx.emit(NewChatEvent::DraftChanged);
        cx.notify();
    }

    /// New message to `person` alone, as if they were taken on the To: line (a profile card's Chat with someone the
    /// account has no 1:1 chat with): the chat they are, else New message's own box, which takes the keyboard.
    pub fn start_with(&mut self, person: Recipient, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = Kind::NewMessage;
        self.naming = false;
        self.suggesting = false;
        self.people = vec![person];
        self.to.update(cx, |to, cx| to.set_value("", window, cx));
        self.found_people.clear();
        self.highlighted = None;
        self.resolve(window, cx);
        let compose = self.compose(cx);
        compose.update(cx, |compose, cx| compose.focus(window, cx));
        cx.emit(NewChatEvent::DraftChanged);
        cx.notify();
    }

    /// Whether this is a new group chat from a 1:1 chat.
    #[must_use]
    pub fn drafting(&self) -> bool {
        self.kind == Kind::GroupDraft
    }

    /// A new group chat's row in the list, as Teams names it: the people's first names joined.
    #[must_use]
    pub fn draft_row(&self) -> Option<ChatRow> {
        if self.kind != Kind::GroupDraft {
            return None;
        }
        let names: Vec<String> = self
            .people
            .iter()
            .map(|person| chat_list::first_name(&person.name).to_owned())
            .collect();
        let faces = self
            .people
            .iter()
            .take(2)
            .map(|person| chat_list::HalfFace {
                name: Some(person.name.clone()),
                picture: person.picture.clone(),
            })
            .collect();
        Some(ChatRow {
            id: String::new(),
            kind: RowKind::Group,
            name: chat_list::joined(&names, 0).into(),
            named: true,
            picture: None,
            partner: None,
            faces,
            members: self.people.len() + 1,
            chat_with: None,
            team: None,
            preview: SharedString::default(),
            previewed: None,
            last_activity: None,
            unread: 0,
        })
    }

    /// Opens the new group chat's people, or closes them; they don't open while its first message makes the chat.
    pub fn toggle_people(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.people_flyout.take().is_some() {
            cx.notify();
            return;
        }
        if self.is_starting() {
            return;
        }
        let parts = ChatPeopleParts {
            core: Arc::clone(&self.core),
            trace: Arc::clone(&self.trace),
            list: self.list.clone(),
            presence: self.parts.models.presence.clone(),
            cards: self.parts.models.cards.clone(),
            button: self.people_button.clone(),
        };
        let people = self.people.clone();
        let view = cx.new(|cx| ChatPeople::new(parts, String::new(), PeopleOf::Draft, people, window, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, _, event: &ChatPeopleEvent, window, cx| {
                match event {
                    ChatPeopleEvent::AddToDraft(more) => this.add_to_draft(more, window, cx),
                    // a person's row: their 1:1 chat, else New message to them alone
                    ChatPeopleEvent::OpenChat(person) => {
                        let me = this.core.me().to_owned();
                        match crate::profile_card::chat_with(this.list.read(cx), &me, &person.mri) {
                            Some(chat) => cx.emit(NewChatEvent::Open(chat)),
                            None => this.start_with(person.clone(), window, cx),
                        }
                    }
                    ChatPeopleEvent::Close | ChatPeopleEvent::StartGroup(_) => {}
                }
                this.people_flyout = None;
                cx.notify();
            },
        );
        view.update(cx, |view, cx| view.focus(window, cx));
        self.people_flyout = Some((view, events));
        cx.notify();
    }

    /// `more` join the new group chat's people, each once; none while its first message makes the chat.
    fn add_to_draft(&mut self, more: &[Recipient], window: &mut Window, cx: &mut Context<Self>) {
        if self.is_starting() {
            return;
        }
        for person in more {
            if !self.people.iter().any(|known| known.mri == person.mri) {
                self.people.push(person.clone());
            }
        }
        self.resolve(window, cx);
        cx.emit(NewChatEvent::DraftChanged);
    }

    /// The new group chat's people flyout, while it is open.
    #[must_use]
    pub fn people_flyout(&self) -> Option<&Entity<ChatPeople>> {
        self.people_flyout.as_ref().map(|(view, _)| view)
    }

    /// The new group chat's header in place of To: (see the module docs).
    fn draft_header(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let row = self.draft_row();
        let face = row.as_ref().map(|row| chat_list::group_face(row, palette, 32.0));
        let title: Vec<&str> = self
            .people
            .iter()
            .map(|person| chat_list::first_name(&person.name))
            .collect();
        let button = icon_button("chat-header-people", Glyph::USER_PLUS, PEOPLE_TIP)
            .selected(self.people_flyout.is_some())
            .disabled(self.is_starting())
            .label((self.people.len() + 1).to_string())
            .on_click(cx.listener(|this, _, window, cx| this.toggle_people(window, cx)));
        crate::shell::pane_header_led(face, title.join(", "), palette)
            .debug_selector(|| "new-chat-draft-header".to_owned())
            .child(div().flex_1())
            .child(self.people_button.keep(selected_by("chat-header-people", button)))
            .into_any_element()
    }

    // --- the To: line ---

    fn on_to(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                self.highlighted = None;
                self.suggesting = true;
                self.ask_directory(cx);
                self.refresh_suggestions(cx);
            }
            InputEvent::Focus => {
                self.suggesting = true;
                self.refresh_suggestions(cx);
            }
            InputEvent::Blur => {
                self.suggesting = false;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                let at = self.highlighted.unwrap_or(0);
                self.choose(at, window, cx);
            }
        }
    }

    /// What the To: box says, trimmed.
    fn query(&self, cx: &gpui::App) -> String {
        self.to.read(cx).value().trim().to_owned()
    }

    /// Asks the directory for the people the box names, a moment after the typing stops; an answer to what the box
    /// no longer says is dropped.
    fn ask_directory(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
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
                if this.query(cx) != query {
                    return;
                }
                this.found_people = answer
                    .people
                    .into_iter()
                    .filter_map(|person| {
                        let mri = person.mri.filter(|mri| mri.starts_with("8:orgid:"))?;
                        let detail = person.job_title.or(person.email).unwrap_or_default();
                        Some(Suggested::Person {
                            mri,
                            name: person.name.into(),
                            detail: detail.into(),
                            picture: None,
                        })
                    })
                    .collect();
                this.refresh_suggestions(cx);
            });
        }));
    }

    /// The suggestions again from what the box says, the people chosen and the list's chats (see the module docs).
    fn refresh_suggestions(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        let me = self.core.me().to_owned();
        let chosen: Vec<&str> = self.people.iter().map(|person| person.mri.as_str()).collect();
        let list = self.list.read(cx);
        let partners: Vec<&ChatRow> = list
            .findable_rows()
            .filter(|row| row.kind == RowKind::OneToOne && row.named)
            .filter(|row| {
                row.partner
                    .as_deref()
                    .is_some_and(|mri| mri != me && !chosen.contains(&mri))
            })
            .collect();
        let mut suggestions: Vec<Suggested> = if query.is_empty() {
            partners.iter().take(RECENT_PEOPLE).map(|row| person_of(row)).collect()
        } else {
            let named: Vec<(String, SharedString)> =
                partners.iter().map(|row| (row.id.clone(), row.name.clone())).collect();
            matches(&query, &named)
                .into_iter()
                .filter_map(|(id, _)| partners.iter().find(|row| row.id == id).map(|row| person_of(row)))
                .collect()
        };
        for person in &self.found_people {
            let Suggested::Person { mri, .. } = person else {
                continue;
            };
            let known = suggestions
                .iter()
                .any(|known| matches!(known, Suggested::Person { mri: known, .. } if known == mri));
            if !known && !chosen.contains(&mri.as_str()) && *mri != me {
                suggestions.push(person.clone());
            }
        }
        suggestions.truncate(MOST_PEOPLE);
        if !query.is_empty() {
            let groups: Vec<&ChatRow> = list.findable_rows().filter(|row| row.kind == RowKind::Group).collect();
            let named: Vec<(String, SharedString)> = groups
                .iter()
                .map(|row| (row.id.clone(), searchable_name(row)))
                .collect();
            suggestions.extend(
                matches(&query, &named)
                    .into_iter()
                    .take(MOST_CHATS)
                    .filter_map(|(id, _)| groups.iter().find(|row| row.id == id))
                    .map(|row| Suggested::Chat((*row).clone())),
            );
        }
        self.suggestions = suggestions;
        if self.highlighted.is_some_and(|at| at >= self.suggestions.len()) {
            self.highlighted = None;
        }
        cx.notify();
    }

    /// Puts `text` in the To: box as if it were typed, its suggestions open.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.to.update(cx, |to, cx| to.set_value(text.to_owned(), window, cx));
        self.highlighted = None;
        self.suggesting = true;
        self.ask_directory(cx);
        self.refresh_suggestions(cx);
    }

    /// Shows the group name's line with `text` in it, as if it were typed there.
    pub fn set_group_name(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.naming {
            self.toggle_group_name(window, cx);
        }
        self.name
            .update(cx, |name, cx| name.set_value(text.to_owned(), window, cx));
        self.resolve(window, cx);
        cx.notify();
    }

    /// Moves the highlight `by` rows, wrapping.
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

    /// Takes the suggestion at `at`: a person is added (not while the first message makes the chat: its people are
    /// the ones it is made with), a group chat opened.
    pub fn choose(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chosen) = self.suggestions.get(at).cloned() else {
            return;
        };
        match chosen {
            Suggested::Person { .. } if self.is_starting() => {}
            Suggested::Person { mri, name, picture, .. } => {
                self.people.push(Recipient { mri, name, picture });
                self.to.update(cx, |to, cx| to.set_value("", window, cx));
                self.found_people.clear();
                self.highlighted = None;
                self.refresh_suggestions(cx);
                self.resolve(window, cx);
            }
            Suggested::Chat(row) => cx.emit(NewChatEvent::Open(row.id)),
        }
    }

    /// Takes the person at `at` off the line; nobody while the first message makes the chat.
    pub fn remove(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if at < self.people.len() && !self.is_starting() {
            self.people.remove(at);
            self.refresh_suggestions(cx);
            self.resolve(window, cx);
        }
    }

    /// Backspace in the empty To: box: the last person comes off.
    fn backspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.to.read(cx).value().is_empty() || self.people.is_empty() {
            return false;
        }
        self.remove(self.people.len() - 1, window, cx);
        true
    }

    /// Shows or hides the group name's line (see the module docs).
    pub fn toggle_group_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.naming && !self.group_name(cx).is_empty() {
            return;
        }
        self.naming = !self.naming;
        if self.naming {
            self.name.update(cx, |name, cx| name.focus(window, cx));
        } else {
            self.name.update(cx, |name, cx| name.set_value("", window, cx));
        }
        self.resolve(window, cx);
        cx.notify();
    }

    // --- the chat the people are ---

    /// Finds the chat the people are (see the module docs), and shows it, or New message's own box.
    fn resolve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = self.core.me().to_owned();
        if !self.group_name(cx).is_empty() || self.people.is_empty() {
            self.resolving = None;
            return self.show_found(None, window, cx);
        }
        if let [one] = self.people.as_slice() {
            self.resolving = None;
            let chat = self
                .list
                .read(cx)
                .findable_rows()
                .find(|row| row.kind == RowKind::OneToOne && row.partner.as_deref() == Some(one.mri.as_str()))
                .map(|row| row.id.clone());
            return self.show_found(chat, window, cx);
        }
        // the chat shown is for the people as they were: nothing is written to it while theirs is looked up
        self.show_found(None, window, cx);
        let mut members: Vec<String> = self.people.iter().map(|person| person.mri.clone()).collect();
        members.push(me);
        let asked = self.people();
        let asked: Vec<String> = asked.into_iter().map(str::to_owned).collect();
        let reply = self.core.group_chat_with(members);
        self.resolving = Some(cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let chat = reply.await.ok().flatten();
            let _ = this.update_in(cx, |this, window, cx| {
                // an answer for people no longer on the line is dropped
                if this.people() == asked && this.group_name(cx).is_empty() {
                    this.show_found(chat, window, cx);
                }
            });
        }));
    }

    /// Shows `chat` under the line (none: New message's own box), carrying what was typed across.
    fn show_found(&mut self, chat: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.found.as_ref().map(|found| &found.id) == chat.as_ref() {
            return;
        }
        let from = self.compose(cx);
        self.found = chat.map(|id| {
            let view = cx.new(|cx| ChatView::new(self.parts.clone(), id.clone(), window, cx));
            view.update(cx, |view, cx| view.set_shown(true, cx));
            let compose = view.read(cx).compose().clone();
            let opened = id.clone();
            let sent = cx.subscribe_in(&compose, window, move |_: &mut Self, _, event: &ComposeEvent, _, cx| {
                if matches!(event, ComposeEvent::Sent) {
                    cx.emit(NewChatEvent::Open(opened.clone()));
                }
            });
            Found { id, view, _sent: sent }
        });
        let to = self.compose(cx);
        from.update(cx, |from, cx| from.hand_text_to(&to, cx));
        self.trace
            .record(&TraceLine::new("new-chat", "found").conversation(self.found().unwrap_or_default()));
        cx.notify();
    }

    // --- the first message ---

    /// New message's box handed on its first message: the chat is made, then the message sent to it (see the module
    /// docs).
    fn start(&mut self, text: MessageText, window: &mut Window, cx: &mut Context<Self>) {
        self.given_back = false;
        let topic = self.group_name(cx);
        let refused = if self.people.is_empty() {
            Some(NOBODY)
        } else if !topic.is_empty() && self.people.len() < 2 {
            Some(NAMED_NEEDS_TWO)
        } else if self.core.read_only() {
            Some(crate::compose::READ_ONLY_APP)
        } else {
            None
        };
        if let Some(problem) = refused {
            let draft = self.draft.clone();
            draft.update(cx, |draft, cx| draft.started(Err(problem.into()), window, cx));
            return;
        }
        let others: Vec<Mri> = self.people.iter().map(|person| Mri::new(&person.mri)).collect();
        let topic = (!topic.is_empty()).then_some(topic);
        let core = Arc::clone(&self.core);
        let reply = self.core.write(Action::CreateChat { others, topic });
        // the people are locked from here: those it is made with are those shown
        self.people_flyout = None;
        self.starting = Some(cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let made = settled_chat(&core, reply, cx).await;
            let sent = match &made {
                Some(chat) => core
                    .write(Action::Send {
                        conversation: chat.clone(),
                        text,
                    })
                    .await
                    .is_ok(),
                None => false,
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.starting = None;
                let draft = this.draft.clone();
                if let Some(chat) = made.filter(|_| sent) {
                    this.trace
                        .record(&TraceLine::new("new-chat", "started").conversation(&chat));
                    draft.update(cx, |draft, cx| draft.started(Ok(()), window, cx));
                    cx.emit(NewChatEvent::Open(chat));
                } else {
                    this.given_back = true;
                    draft.update(cx, |draft, cx| draft.started(Err(NOT_STARTED.into()), window, cx));
                }
            });
        }));
        cx.notify();
    }

    /// Whether a new chat is being made.
    #[must_use]
    pub fn is_starting(&self) -> bool {
        self.starting.is_some()
    }

    /// Whether New message is kept when the place shown leaves it (see the module docs): its first message is still
    /// on its way, or was given back since it was last shown.
    #[must_use]
    pub fn keeps_going(&self) -> bool {
        self.starting.is_some() || self.given_back
    }

    // --- drawing ---

    fn chip(at: usize, person: &Recipient, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let remove = div()
            .id(SharedString::from(format!("new-chat-chip-remove-{at}")))
            .debug_selector(move || format!("new-chat-chip-remove-{at}"))
            .flex_none()
            .p_0p5()
            .rounded_sm()
            .cursor_pointer()
            .text_color(rgb(palette.muted_foreground))
            .hover(move |button| button.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.remove(at, window, cx)))
            .child(Glyph::X.icon().size(px(12.0)));
        div()
            .debug_selector(move || format!("new-chat-chip-{at}"))
            .flex_none()
            .h(px(30.0))
            .pl_0p5()
            .pr_1p5()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_full()
            .bg(rgb(palette.hover))
            .child(people::avatar(Some(&person.name), person.picture.as_ref(), Size::Small))
            .child(div().text_sm().child(person.name.clone()))
            .child(remove)
            .into_any_element()
    }

    fn group_name_line(&self, focused: bool, palette: Palette) -> AnyElement {
        div()
            .debug_selector(|| "new-chat-group-name".to_owned())
            .mx_3()
            .h(px(52.0))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .border_b_2()
            .border_color(rgb(if focused { palette.brand } else { palette.border }))
            .child(div().flex_none().text_sm().child(GROUP_NAME))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.name).appearance(false).aria_label("Group name")),
            )
            .into_any_element()
    }

    fn to_line(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chips: Vec<AnyElement> = self
            .people
            .iter()
            .enumerate()
            .map(|(at, person)| Self::chip(at, person, palette, cx))
            .collect();
        let named = !self.group_name(cx).is_empty();
        let (glyph, tip) = if self.naming {
            (Glyph::CHEVRON_UP, REMOVE_GROUP_NAME)
        } else {
            (Glyph::CHEVRON_DOWN, ADD_GROUP_NAME)
        };
        let toggle = selected_by(
            "new-chat-group-name-toggle",
            icon_button("new-chat-group-name-toggle", glyph, tip)
                .with_size(Size::Small)
                .disabled(named)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_group_name(window, cx))),
        );
        let line = div()
            .debug_selector(|| "new-chat-to".to_owned())
            .id("new-chat-to")
            .min_h(px(60.0))
            .px_5()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border));
        Self::line_keys(line, cx)
            .child(div().flex_none().text_sm().child("To:"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .children(chips)
                    .child(
                        div()
                            .debug_selector(|| "new-chat-to-input".to_owned())
                            .flex_1()
                            .min_w(px(160.0))
                            .child(Input::new(&self.to).appearance(false).aria_label("To")),
                    ),
            )
            .child(toggle)
            .into_any_element()
    }

    /// The To: line's pointer and keys: a press opens the suggestions (as Teams' does, even while its box already has
    /// the keyboard); Up, Down and Escape are the suggestions' while they are open; Backspace in the empty box takes
    /// the last person off.
    fn line_keys(line: gpui::Stateful<gpui::Div>, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        line.on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.suggesting = true;
                this.refresh_suggestions(cx);
            }),
        )
        .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
            if this.suggesting {
                this.move_highlight(-1, cx);
                cx.stop_propagation();
            }
        }))
        .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
            if this.suggesting {
                this.move_highlight(1, cx);
                cx.stop_propagation();
            }
        }))
        .capture_action(cx.listener(|this, _: &Escape, _, cx| {
            if this.suggesting {
                this.suggesting = false;
                cx.notify();
                cx.stop_propagation();
            }
        }))
        .capture_action(cx.listener(|this, _: &Backspace, window, cx| {
            if this.backspace(window, cx) {
                cx.stop_propagation();
            }
        }))
    }

    fn suggestion(&self, at: usize, suggested: &Suggested, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let highlighted = self.highlighted == Some(at);
        let (face, name, detail) = match suggested {
            Suggested::Person {
                name, detail, picture, ..
            } => (
                people::avatar(Some(name), picture.as_ref(), Size::Size(px(FACE))).into_any_element(),
                name.clone(),
                detail.clone(),
            ),
            Suggested::Chat(row) => (
                chat_list::group_face(row, palette, FACE),
                row.name.clone(),
                SharedString::default(),
            ),
        };
        div()
            .id(SharedString::from(format!("new-chat-suggestion-{at}")))
            .debug_selector(move || format!("new-chat-suggestion-{at}"))
            .w_full()
            .min_h(px(52.0))
            .px_4()
            .py_1p5()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .when(highlighted, |row| row.bg(rgb(palette.selected)))
            .hover(move |row| row.bg(rgb(palette.hover)))
            // a press would take the keyboard from the box, closing the suggestions before the click lands
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.choose(at, window, cx)))
            .child(face)
            .child(
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
                    }),
            )
            .into_any_element()
    }

    fn suggestions_panel(&self, top: f32, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.suggesting || self.suggestions.is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = self
            .suggestions
            .iter()
            .enumerate()
            .map(|(at, suggested)| self.suggestion(at, suggested, palette, cx))
            .collect();
        Some(
            div()
                .id("new-chat-suggestions")
                .debug_selector(|| "new-chat-suggestions".to_owned())
                .absolute()
                .top(px(top))
                .left(px(52.0))
                .right(px(72.0))
                .max_h(px(SUGGESTIONS_MAX))
                .overflow_y_scroll()
                .p_1()
                .bg(rgb(palette.surface))
                .border_1()
                .border_color(rgb(palette.border))
                .rounded_lg()
                .shadow_lg()
                .children(rows)
                .into_any_element(),
        )
    }

    fn starting_note(palette: Palette) -> AnyElement {
        div()
            .debug_selector(|| "new-chat-empty".to_owned())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .size(px(96.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(theme::mix(palette.background, palette.brand, 0.14)))
                    .text_color(rgb(palette.brand))
                    .child(Glyph::NEW_MESSAGE.icon().size(px(44.0))),
            )
            .child(div().mt_2().text_lg().font_weight(FontWeight::SEMIBOLD).child(STARTING))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(STARTING_HINT),
            )
            .into_any_element()
    }
}

/// A 1:1 chat's partner as a suggestion.
fn person_of(row: &ChatRow) -> Suggested {
    Suggested::Person {
        mri: row.partner.clone().unwrap_or_default(),
        name: row.name.clone(),
        detail: SharedString::default(),
        picture: row.picture.clone(),
    }
}

/// What a group chat is found by: its name, and the names of the members its avatar shows (a named chat is found by
/// its people too, as Teams finds one).
fn searchable_name(row: &ChatRow) -> SharedString {
    let mut name = row.name.to_string();
    for face in &row.faces {
        if let Some(person) = &face.name {
            name.push(' ');
            name.push_str(person);
        }
    }
    name.into()
}

/// The chat a new chat's outbox row made, once the outbox settled it ([`settled_row`]): its id when it went out (the
/// row's answer names the thread), none when it didn't.
async fn settled_chat(
    core: &Arc<dyn Core>,
    reply: teams_core::Reply<teams_store::OutboxEntry>,
    cx: &mut AsyncWindowContext,
) -> Option<String> {
    let row = settled_row(core.as_ref(), reply, cx.background_executor()).await?;
    (row.state == OutboxState::Sent).then_some(row.message_id).flatten()
}

impl gpui::Focusable for NewChat {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for NewChat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let name_focused = self.name.read(cx).focus_handle(cx).is_focused(window);
        let group = (self.naming && self.kind != Kind::GroupDraft).then(|| self.group_name_line(name_focused, palette));
        let to = if self.kind == Kind::GroupDraft {
            self.draft_header(palette, cx)
        } else {
            self.to_line(palette, cx)
        };
        let top = if self.naming { 52.0 + 60.0 } else { 60.0 };
        let panel = if self.kind == Kind::GroupDraft {
            None
        } else {
            self.suggestions_panel(top, palette, cx)
        };
        let flyout = self.people_flyout.as_ref().map(|(view, _)| {
            gpui::deferred(div().absolute().top(px(50.0)).right(px(12.0)).child(view.clone())).with_priority(1)
        });
        let body = match &self.found {
            Some(found) => div().flex_1().min_h_0().child(found.view.clone()).into_any_element(),
            None => div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(Self::starting_note(palette))
                .child(self.draft.clone())
                .into_any_element(),
        };
        div()
            .debug_selector(|| "new-chat".to_owned())
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .children(group)
            .child(to)
            .child(body)
            .children(panel)
            .children(flyout)
    }
}
