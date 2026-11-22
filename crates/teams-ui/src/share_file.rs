//! "Share this file", as Teams opens it from a file card's Share and from its "More options > Share" (seen with lab
//! accounts on 2026-11-18, `docs/protocol/chat.md`, "Files: shared on"): the file sent on to any chat or channel, with
//! an optional message.
//!
//! - **The dialog**: the title, "You can share to any chat or channel.", "More actions" (…) and Close; "Add recipients"
//!   (marked as necessary) with its box; "Message preview": a box with "Add a message (optional)" over the file's card
//!   (its type's icon, its name, what its link lets people do as the file's site says it, and its picture), and Teams'
//!   format and emoji buttons; then Copy link and Share. Share stays off until a recipient is picked, and while the
//!   file is being shared.
//! - **More actions** offers "Share outside Teams": SharePoint's own sharing of the file, its OneDrive page opened in
//!   the browser ([`teams_proto::chat::onedrive_page`]). An own server's file has no such page: the menu isn't drawn.
//! - **The message** is the app's rich-text editor, as the compose box's ([`message`]): Format shows the compose box's
//!   format bar over it, Emoji the app's emoji picker; it is sent as the compose box sends a message
//!   ([`crate::compose::message_of`]).
//! - **The recipients** are found as Teams' picker finds them, as the box's text changes: people (the account's 1:1
//!   chats' partners, then the people the directory finds, never the account itself), group and meeting chats by
//!   their names and their people's, and the teams' channels by their names and their team's, each with its own face.
//!   A query with no match says "We couldn't find any matches." A pick is a chip with its face, its name and a remove
//!   mark; Backspace in the empty box takes the last one off.
//! - **Share** sends the file to each recipient as Teams does: one ordinary message to the recipient's chat or channel,
//!   its text the message, its `properties.files` the file's entry again ([`teams_proto::chat::share_entry_any`],
//!   [`MessageText::with_shared`]). A person with no 1:1 chat gets one through the engine, as New message does.
//!   When all sends succeed, the dialog closes and the shell says "You've shared <name>".
//!   A recipient that the send cannot reach stays picked. The dialog removes successful recipients and shows an error.
//! - **Access**: Microsoft links must have the `anonymous` scope. Other scopes and unknown permissions show an
//!   access error. An own server grants access to a personal file with the message, and never to a channel's file
//!   (`/sp/sites/`): such a file shows an access error.
//! - **Retries** read each recipient's current outbox row and use [`Core::resend`]. An unknown outcome shows a warning first.
//!   The message stays read-only while a share runs or a recipient keeps a Send row for retry.
//!   Close, Escape and Copy link cannot stop an active share.
//! - **Copy link** puts the file's sharing link on the clipboard, and the dialog becomes Teams' "Link created" box
//!   ([`link`]): the link in its field, Copy, what the link lets people do, and Settings (the file's OneDrive page,
//!   where SharePoint sets its links), then Close.
//! - **Keys**: the box has the keyboard when the dialog opens; Up and Down move in the suggestions, Enter takes the
//!   one highlighted (else the first); Escape closes an open menu or picker, then the suggestions (clearing the box),
//!   else the dialog; Enter in the message (Shift+Enter: a new line) shares; Tab goes from the box to the message,
//!   then to the buttons.
//!
//! Nothing here writes but through [`Core`]; the read-only app offers no Share. Traced: `share-file opened`,
//! `link copied`, `outside`, `shared`, `closed`; never a conversation, a name or a text.

mod link;
mod message;
mod send;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, ObjectFit, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task, WeakEntity,
    Window, actions, div, img, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Backspace, Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::{Disableable as _, FocusTrapElement as _, Size};
use teams_core::SearchScope;
use teams_proto::chat::{LinkPermission, MessageText, RichText, SharedFile, share_entry_any};
use teams_store::{OutboxEntry, OutboxKind, OutboxState};

use crate::bridge::Core;
use crate::chat_list::{self, ChatList, ChatRow, RowKind};
use crate::editor::toolbar::FormatBar;
use crate::editor::{Editor, EditorEvent};
use crate::files::{PICTURE_RATIO, header_colour, kind_icon, no_picture, picture_colour};
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::inline::{PictureSource, PictureState, thumbnail};
use crate::new_chat::searchable_name;
use crate::people::{self, Picture};
use crate::reaction_picker::ReactionPicker;
use crate::sending::Again;
use crate::switcher::matches;
use crate::teams_list::TeamsList;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

actions!(share_file, [ToMessage]);

/// The recipients' box's key context.
const TO_CONTEXT: &str = "ShareFileTo";

/// Binds the dialog's keys, once per app: Tab in the recipients' box goes to the message, as in Teams; the kit's Tab
/// would stop on the box's own controls first.
pub fn init(cx: &mut gpui::App) {
    cx.bind_keys([KeyBinding::new("tab", ToMessage, Some(TO_CONTEXT))]);
}

/// The dialog's words, as Teams has them.
pub const TITLE: &str = "Share this file";
pub const SUBTITLE: &str = "You can share to any chat or channel.";
pub const RECIPIENTS: &str = "Add recipients";
pub const RECIPIENTS_HINT: &str = "Enter name, email, group or tag";
pub const PREVIEW: &str = "Message preview";
pub const MESSAGE_HINT: &str = "Add a message (optional)";
pub const NO_MATCH: &str = "We couldn't find any matches.";
pub const COPY_LINK: &str = "Copy link";
pub const SHARE: &str = "Share";

/// The dialog's width, px, as Teams draws it.
const WIDTH: f32 = 600.0;
/// The file card's width in the preview, px.
const CARD: f32 = 320.0;
/// A suggestion's face, px.
const FACE: f32 = 32.0;
/// The most people, chats and channels suggested, each.
const MOST: usize = 6;
/// How long typing rests before the directory is asked.
const PAUSE: Duration = Duration::from_millis(200);
/// The suggestions' most height before they scroll, px.
const SUGGESTIONS_MAX: f32 = 300.0;

/// What the dialog tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareFileEvent {
    /// Closed with nothing shared.
    Closed,
    /// The file (by its name) went to every recipient.
    Shared(SharedString),
}

/// A recipient, picked or suggested.
#[derive(Clone, Debug)]
pub enum Target {
    /// Someone: their 1:1 chat, made when there is none.
    Person {
        mri: String,
        name: SharedString,
        detail: SharedString,
        picture: Option<Picture>,
    },
    /// A group or meeting chat.
    Chat(Box<ChatRow>),
    /// A team's channel: a new post in it.
    Channel {
        id: String,
        name: SharedString,
        team: SharedString,
        team_id: String,
    },
}

impl Target {
    /// What tells it from any other.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Person { mri, .. } => mri,
            Self::Chat(row) => &row.id,
            Self::Channel { id, .. } => id,
        }
    }

    /// Its name, as its chip shows it.
    #[must_use]
    pub fn name(&self) -> SharedString {
        match self {
            Self::Person { name, .. } | Self::Channel { name, .. } => name.clone(),
            Self::Chat(row) => row.name.clone(),
        }
    }
}

/// Where the dialog is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Ready,
    Sharing,
    /// Some recipients weren't reached: what to tell.
    Failed(SharedString),
    ConfirmRetry(SharedString),
    /// Copy link pressed: Teams' "Link created" box in the dialog's place.
    LinkCreated,
}

/// "Share this file" (see the module docs).
pub struct ShareFileDialog {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    file: SharedFile,
    source: PictureSource,
    permission: Option<LinkPermission>,
    to: Entity<InputState>,
    message: Entity<Editor>,
    /// The compose box's format bar over the message, while Format is on.
    format_bar: Entity<FormatBar>,
    formatting: bool,
    /// The emoji picker over the message, while it is open, and where its button was last drawn.
    emoji: Option<(Entity<ReactionPicker>, Subscription)>,
    emoji_at: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// "More actions" is open.
    more_open: bool,
    picked: Vec<Target>,
    suggestions: Vec<Target>,
    suggesting: bool,
    highlighted: Option<usize>,
    /// The directory's people for what the box said when they were asked.
    found_people: Vec<Target>,
    asking: Option<Task<()>>,
    step: Step,
    retries: HashMap<String, OutboxEntry>,
    tasks: Vec<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ShareFileEvent> for ShareFileDialog {}

impl ShareFileDialog {
    pub fn new(
        (core, trace): (Arc<dyn Core>, Arc<ViewTrace>),
        (list, teams): (Entity<ChatList>, Entity<TeamsList>),
        file: SharedFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        trace.record(&TraceLine::new("share-file", "opened"));
        let to = cx.new(|cx| InputState::new(window, cx).placeholder(RECIPIENTS_HINT));
        let message = cx.new(|cx| Editor::new(window, cx).placeholder(MESSAGE_HINT));
        let format_bar = cx.new(|cx| FormatBar::new(message.clone(), cx).without_discard());
        let subscriptions = vec![
            cx.subscribe_in(&to, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
                this.on_to(event, window, cx);
            }),
            cx.subscribe_in(
                &message,
                window,
                |this: &mut Self, _, event: &EditorEvent, window, cx| {
                    if *event == EditorEvent::Submit {
                        this.share(window, cx);
                    }
                },
            ),
            cx.observe(&list, |this: &mut Self, _, cx| this.refresh_suggestions(cx)),
        ];
        let mut dialog = Self {
            source: PictureSource::new(Arc::clone(&core)),
            core,
            trace,
            list,
            teams,
            file,
            permission: None,
            to,
            message,
            format_bar,
            formatting: false,
            emoji: None,
            emoji_at: Rc::default(),
            more_open: false,
            picked: Vec::new(),
            suggestions: Vec::new(),
            suggesting: false,
            highlighted: None,
            found_people: Vec::new(),
            asking: None,
            step: Step::Ready,
            retries: HashMap::new(),
            tasks: Vec::new(),
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        dialog.read_permission(cx);
        dialog
    }

    /// Gives the recipients' box the keyboard, as the dialog opens.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.to.update(cx, |to, cx| to.focus(window, cx));
    }

    /// The file it shares.
    #[must_use]
    pub fn file(&self) -> &SharedFile {
        &self.file
    }

    /// The recipients picked, in order.
    #[must_use]
    pub fn picked(&self) -> &[Target] {
        &self.picked
    }

    /// The suggestions shown for what the box says.
    #[must_use]
    pub fn suggestions(&self) -> &[Target] {
        &self.suggestions
    }

    /// What the link lets people do, once it was read.
    #[must_use]
    pub fn permission(&self) -> Option<&LinkPermission> {
        self.permission.as_ref()
    }

    /// Whether Share is on: a recipient picked, nothing being shared.
    #[must_use]
    pub fn can_share(&self) -> bool {
        !self.picked.is_empty() && self.step != Step::Sharing
    }

    /// What the dialog says went wrong, while it says it.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        match &self.step {
            Step::Failed(words) | Step::ConfirmRetry(words) => Some(words),
            _ => None,
        }
    }

    /// Reads what the file's link lets people do, for its card's line and its entry's scope.
    fn read_permission(&mut self, cx: &mut Context<Self>) {
        let (Some(object), Some(share)) = (self.file.object_url.clone(), self.file.share_url.clone()) else {
            return;
        };
        let reply = self.core.link_permission(object, share);
        self.tasks.push(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let permission = reply.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.permission = permission;
                cx.notify();
            });
        }));
    }

    // --- the recipients ---

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
                if self.query(cx).is_empty() {
                    self.share(window, cx);
                } else {
                    self.choose(self.highlighted.unwrap_or(0), window, cx);
                }
            }
        }
    }

    /// What the box says, trimmed.
    fn query(&self, cx: &gpui::App) -> String {
        self.to.read(cx).value().trim().to_owned()
    }

    /// Puts `text` in the box as if it were typed, its suggestions open.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.to.update(cx, |to, cx| to.set_value(text.to_owned(), window, cx));
        self.highlighted = None;
        self.suggesting = true;
        self.ask_directory(cx);
        self.refresh_suggestions(cx);
    }

    /// Puts `text` in the message box, a paragraph per line.
    pub fn set_message(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.message_locked() {
            return;
        }
        let doc = crate::editor::doc::Doc::new(text.lines().map(crate::editor::doc::Block::paragraph).collect());
        self.message.update(cx, |message, cx| message.set_doc(doc, cx));
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
                this.asking = None;
                this.found_people = answer
                    .people
                    .into_iter()
                    .filter_map(|person| {
                        let mri = person.mri.filter(|mri| mri.starts_with("8:orgid:"))?;
                        let detail = person.job_title.or(person.email).unwrap_or_default();
                        Some(Target::Person {
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

    /// The suggestions again from what the box says (see the module docs).
    fn refresh_suggestions(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.suggestions = if query.is_empty() {
            Vec::new()
        } else {
            let mut found = self.people(&query, cx);
            found.extend(self.chats(&query, cx));
            found.extend(self.channels(&query, cx));
            found
        };
        if self.highlighted.is_some_and(|at| at >= self.suggestions.len()) {
            self.highlighted = None;
        }
        cx.notify();
    }

    /// Whether `key` is already picked.
    fn is_picked(&self, key: &str) -> bool {
        self.picked.iter().any(|target| target.key() == key)
    }

    /// The people `query` finds: the 1:1 chats' partners, then the directory's, never the account or one picked.
    fn people(&self, query: &str, cx: &gpui::App) -> Vec<Target> {
        let me = self.core.me();
        let list = self.list.read(cx);
        let partners: Vec<&ChatRow> = list
            .findable_rows()
            .filter(|row| row.kind == RowKind::OneToOne && row.named)
            .filter(|row| row.partner.as_deref().is_some_and(|mri| mri != me))
            .collect();
        let named: Vec<(String, SharedString)> =
            partners.iter().map(|row| (row.id.clone(), row.name.clone())).collect();
        let mut people: Vec<Target> = matches(query, &named)
            .into_iter()
            .filter_map(|(id, _)| partners.iter().find(|row| row.id == id))
            .map(|row| Target::Person {
                mri: row.partner.clone().unwrap_or_default(),
                name: row.name.clone(),
                detail: SharedString::default(),
                picture: row.picture.clone(),
            })
            .collect();
        for person in &self.found_people {
            if person.key() != me && !people.iter().any(|known| known.key() == person.key()) {
                people.push(person.clone());
            }
        }
        people.retain(|person| !self.is_picked(person.key()));
        people.truncate(MOST);
        people
    }

    /// The group and meeting chats `query` finds by their names and their people's.
    fn chats(&self, query: &str, cx: &gpui::App) -> Vec<Target> {
        let list = self.list.read(cx);
        let chats: Vec<&ChatRow> = list
            .findable_rows()
            .filter(|row| matches!(row.kind, RowKind::Group | RowKind::Meeting))
            .filter(|row| !self.is_picked(&row.id))
            .collect();
        let named: Vec<(String, SharedString)> =
            chats.iter().map(|row| (row.id.clone(), searchable_name(row))).collect();
        matches(query, &named)
            .into_iter()
            .filter_map(|(id, _)| chats.iter().find(|row| row.id == id))
            .take(MOST)
            .map(|row| Target::Chat(Box::new((*row).clone())))
            .collect()
    }

    /// The channels `query` finds by their names and their team's.
    fn channels(&self, query: &str, cx: &gpui::App) -> Vec<Target> {
        let teams = self.teams.read(cx).teams().unwrap_or_default();
        let channels: Vec<(Target, (String, SharedString))> = teams
            .iter()
            .filter(|team| !team.archived)
            .flat_map(|team| {
                team.channels.iter().map(|channel| {
                    let target = Target::Channel {
                        id: channel.id.clone(),
                        name: channel.name.clone().into(),
                        team: team.name.clone().into(),
                        team_id: team.id.clone(),
                    };
                    let named = (channel.id.clone(), format!("{} {}", channel.name, team.name).into());
                    (target, named)
                })
            })
            .filter(|(channel, _)| !self.is_picked(channel.key()))
            .collect();
        let named: Vec<(String, SharedString)> = channels.iter().map(|(_, named)| named.clone()).collect();
        matches(query, &named)
            .into_iter()
            .filter_map(|(id, _)| channels.iter().find(|(channel, _)| channel.key() == id))
            .take(MOST)
            .map(|(channel, _)| channel.clone())
            .collect()
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

    /// Picks the suggestion at `at`; nothing while the file is being shared.
    pub fn choose(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.step == Step::Sharing {
            return;
        }
        let Some(chosen) = self.suggestions.get(at).cloned() else {
            return;
        };
        self.picked.push(chosen);
        if matches!(self.step, Step::ConfirmRetry(_)) {
            self.step = Step::Ready;
        }
        self.to.update(cx, |to, cx| to.set_value("", window, cx));
        self.found_people.clear();
        self.highlighted = None;
        self.refresh_suggestions(cx);
    }

    /// Takes the recipient at `at` off; nobody while the file is being shared.
    pub fn remove(&mut self, at: usize, cx: &mut Context<Self>) {
        if at < self.picked.len() && self.step != Step::Sharing {
            self.picked.remove(at);
            if matches!(self.step, Step::ConfirmRetry(_)) {
                self.step = Step::Ready;
            }
            self.refresh_suggestions(cx);
        }
    }

    /// Backspace in the empty box: the last recipient comes off.
    fn backspace(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.to.read(cx).value().is_empty() || self.picked.is_empty() {
            return false;
        }
        self.remove(self.picked.len() - 1, cx);
        true
    }

    /// Escape: an open menu or picker closes, then the suggestions close and the box is cleared, else the dialog
    /// closes.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.step == Step::Sharing {
            return;
        }
        if self.more_open || self.emoji.is_some() {
            self.more_open = false;
            self.emoji = None;
            cx.notify();
            return;
        }
        if self.suggesting && !self.to.read(cx).value().is_empty() {
            self.to.update(cx, |to, cx| to.set_value("", window, cx));
            self.suggesting = false;
            self.refresh_suggestions(cx);
            return;
        }
        self.close(cx);
    }

    /// Closes the dialog with nothing more shared.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.step == Step::Sharing {
            return;
        }
        self.trace.record(&TraceLine::new("share-file", "closed"));
        cx.emit(ShareFileEvent::Closed);
    }

    /// Copy link: the file's sharing link on the clipboard, and "Link created" in the dialog's place ([`link`]).
    pub fn copy_link(&mut self, cx: &mut Context<Self>) {
        if self.step == Step::Sharing {
            return;
        }
        let Some(link) = self.file.open_link() else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(link.to_owned()));
        self.trace.record(&TraceLine::new("share-file", "link copied"));
        self.step = Step::LinkCreated;
        self.more_open = false;
        self.emoji = None;
        self.suggesting = false;
        cx.notify();
    }

    /// Whether "Link created" shows.
    #[must_use]
    pub fn link_created(&self) -> bool {
        self.step == Step::LinkCreated
    }

    // --- sharing ---

    /// The message as it is sent: the editor's text as the compose box sends it, none when it says nothing; `None`
    /// when it can't be sent (too long) or the file has no entry to share.
    fn text(&self, cx: &gpui::App) -> Option<MessageText> {
        let text = match crate::compose::message_of(self.message.read(cx).doc(), &[]) {
            Ok(Some(text)) => text,
            Ok(None) => MessageText::new(RichText::default(), []).ok()?,
            Err(_) => return None,
        };
        let entry = share_entry_any(
            &self.file,
            self.permission.as_ref().map(|permission| permission.scope.as_str()),
        )?;
        Some(text.with_shared(vec![entry]))
    }

    /// Share: the file sent to each recipient (see the module docs).
    pub fn share(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.can_share() || self.core.read_only() {
            return;
        }
        if let Some(problem) = self.access_problem() {
            self.step = Step::Failed(problem.into());
            cx.notify();
            return;
        }
        let Some(text) = self.text(cx) else {
            self.step = Step::Failed("This message or file can't be shared. Shorten the message and try again.".into());
            cx.notify();
            return;
        };
        let confirmed = matches!(self.step, Step::ConfirmRetry(_));
        self.step = Step::Sharing;
        self.lock_message(cx);
        self.emoji = None;
        self.suggesting = false;
        let targets = self.picked.clone();
        let mut retries = self.retries.clone();
        let core = Arc::clone(&self.core);
        let chats: Vec<(String, String)> = self
            .list
            .read(cx)
            .findable_rows()
            .filter(|row| row.kind == RowKind::OneToOne)
            .filter_map(|row| Some((row.partner.clone()?, row.id.clone())))
            .collect();
        self.tasks
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let refreshed = send::refresh(core.as_ref(), &targets, &mut retries).await;
                let proceed = this.update(cx, |this, cx| {
                    this.retries.clone_from(&retries);
                    this.ready_to_retry(refreshed, confirmed, cx)
                });
                if !proceed.unwrap_or(false) {
                    return;
                }
                let mut missed = Vec::new();
                for target in targets {
                    let previous = retries.get(target.key()).cloned();
                    let row = send::send_to(&core, &target, &chats, text.clone(), previous, cx).await;
                    if row.as_ref().is_some_and(|row| row.state == OutboxState::Sent) {
                        retries.remove(target.key());
                        continue;
                    }
                    retries.extend(row.map(|row| (target.key().to_owned(), row)));
                    missed.push(target);
                }
                let _ = this.update(cx, |this, cx| this.shared(missed, retries, cx));
            }));
        cx.notify();
    }

    /// Why the recipients may not open the file, when they may not.
    fn access_problem(&self) -> Option<&'static str> {
        let link = self.file.open_link()?;
        if teams_proto::chat::is_file_link(link) {
            return self
                .permission
                .as_ref()
                .is_none_or(|permission| permission.scope != "anonymous")
                .then_some("These recipients may not have access. Use Share outside Teams to give them access first.");
        }
        // An own server grants nothing on a post that names a channel's file (docs/server.md).
        url::Url::parse(link)
            .is_ok_and(|url| url.path().starts_with("/sp/sites/"))
            .then_some("These recipients may not have access. Only the people in the file's channel can open it.")
    }

    fn ready_to_retry(&mut self, refreshed: bool, confirmed: bool, cx: &mut Context<Self>) -> bool {
        if !refreshed {
            self.step = Step::Failed("We could not check the share. Try again.".into());
        } else if confirmed || !self.warn_before_retry() {
            return true;
        }
        self.lock_message(cx);
        cx.notify();
        false
    }

    fn warn_before_retry(&mut self) -> bool {
        let (post, chat) = self
            .picked
            .iter()
            .filter_map(|target| self.retries.get(target.key()))
            .filter(|row| teams_core::outcome_unknown(row) || Again::of(row, false) == Again::AfterWarning)
            .fold((false, false), |(post, chat), row| {
                (
                    post || row.kind == OutboxKind::Send,
                    chat || row.kind == OutboxKind::CreateChat,
                )
            });
        let chat_question = "The chat may already exist. Try to create it again?";
        let question = match (post, chat) {
            (false, false) => return false,
            (true, false) => crate::chat_view::MAY_HAVE_ARRIVED.into(),
            (false, true) => chat_question.into(),
            (true, true) => format!("{} {chat_question}", crate::chat_view::MAY_HAVE_ARRIVED).into(),
        };
        self.step = Step::ConfirmRetry(question);
        true
    }

    /// The share's end: every recipient reached closes the dialog, else those missed stay picked.
    fn shared(&mut self, missed: Vec<Target>, retries: HashMap<String, OutboxEntry>, cx: &mut Context<Self>) {
        self.retries = retries;
        self.step = Step::Ready;
        self.lock_message(cx);
        if missed.is_empty() {
            self.trace.record(&TraceLine::new("share-file", "shared"));
            cx.emit(ShareFileEvent::Shared(self.file.name.clone().into()));
            return;
        }
        let names: Vec<String> = missed.iter().map(|target| target.name().to_string()).collect();
        self.step = Step::Failed(format!("We couldn't share the file with {}. Try again.", names.join(", ")).into());
        self.picked = missed;
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.escape(window, cx);
        }
    }
}

// --- drawing ---

impl ShareFileDialog {
    fn header(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let close = selected_by(
            "share-file-close",
            icon_button("share-file-close", Glyph::X, "Close")
                .disabled(self.step == Step::Sharing)
                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
        );
        div()
            .flex()
            .items_start()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .debug_selector(|| "share-file-title".to_owned())
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(TITLE),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .child(SUBTITLE),
                    ),
            )
            .children(self.more_button(cx))
            .child(close)
            .into_any_element()
    }

    /// A field's label: its words, and Teams' red mark when it is necessary.
    fn label(words: &'static str, necessary: bool, palette: Palette) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .text_color(rgb(palette.foreground))
            .child(words)
            .when(necessary, |label| {
                label.child(div().text_color(rgb(theme::DANGER)).child("*"))
            })
            .into_any_element()
    }

    /// A target's face, `size` across: a person's picture or initials, a chat's avatar, a channel's team's.
    fn face(&self, target: &Target, size: f32, palette: Palette, cx: &gpui::App) -> AnyElement {
        match target {
            Target::Person { name, picture, .. } => {
                people::avatar(Some(name), picture.as_ref(), Size::Size(px(size))).into_any_element()
            }
            Target::Chat(row) => chat_list::chat_avatar(row, palette, Size::Size(px(size)), "share-file-face"),
            Target::Channel { team_id, team, .. } => {
                let teams = self.teams.read(cx);
                match teams
                    .teams()
                    .unwrap_or_default()
                    .iter()
                    .find(|known| known.id == *team_id)
                {
                    Some(known) => teams.avatar(known, size, false, palette),
                    None => people::avatar(Some(team), None, Size::Size(px(size))).into_any_element(),
                }
            }
        }
    }

    fn chip(&self, at: usize, target: &Target, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let remove = div()
            .id(SharedString::from(format!("share-file-chip-remove-{at}")))
            .debug_selector(move || format!("share-file-chip-remove-{at}"))
            .flex_none()
            .p_0p5()
            .rounded_sm()
            .cursor_pointer()
            .text_color(rgb(palette.muted_foreground))
            .hover(move |button| button.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.remove(at, cx)))
            .child(Glyph::X.icon().size(px(12.0)));
        div()
            .debug_selector(move || format!("share-file-chip-{at}"))
            .flex_none()
            .h(px(26.0))
            .pl_0p5()
            .pr_1p5()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_full()
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .child(match target {
                // the kit's small avatar, as New message's chips draw a person: its initials fit
                Target::Person { name, picture, .. } => {
                    people::avatar(Some(name), picture.as_ref(), Size::Small).into_any_element()
                }
                _ => self.face(target, 20.0, palette, cx),
            })
            .child(div().text_sm().child(target.name()))
            .child(remove)
            .into_any_element()
    }

    /// The recipients' box: the chips, then the box typed in, Teams' tinted field with the brand's line while it has
    /// the keyboard.
    fn recipients(&self, window: &Window, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let chips: Vec<AnyElement> = self
            .picked
            .iter()
            .enumerate()
            .map(|(at, target)| self.chip(at, target, palette, cx))
            .collect();
        let focused = self.to.read(cx).focus_handle(cx).is_focused(window);
        div()
            .id("share-file-to")
            .debug_selector(|| "share-file-to".to_owned())
            .key_context(TO_CONTEXT)
            .on_action(cx.listener(|this, _: &ToMessage, window, cx| {
                this.suggesting = false;
                let focus = this.message.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                cx.notify();
            }))
            .min_h(px(34.0))
            .px_3()
            .py_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .rounded_md()
            .bg(rgb(field(palette)))
            .border_b_2()
            .border_color(rgb(if focused { palette.brand } else { field(palette) }))
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
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                cx.stop_propagation();
                this.escape(window, cx);
            }))
            .capture_action(cx.listener(|this, _: &Backspace, _, cx| {
                if this.backspace(cx) {
                    cx.stop_propagation();
                }
            }))
            .children(chips)
            .child(
                div()
                    .debug_selector(|| "share-file-to-input".to_owned())
                    .flex_1()
                    .min_w(px(160.0))
                    .child(Input::new(&self.to).appearance(false).aria_label(RECIPIENTS)),
            )
            .into_any_element()
    }

    fn suggestion(&self, at: usize, target: &Target, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let highlighted = self.highlighted == Some(at);
        let (name, detail, bold_detail) = match target {
            Target::Person { name, detail, .. } => (name.clone(), detail.clone(), false),
            Target::Chat(row) => (row.name.clone(), SharedString::default(), false),
            Target::Channel { name, team, .. } => (name.clone(), team.clone(), true),
        };
        div()
            .id(SharedString::from(format!("share-file-suggestion-{at}")))
            .debug_selector(move || format!("share-file-suggestion-{at}"))
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
            .child(self.face(target, FACE, palette, cx))
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
                                .when(bold_detail, |line| line.font_weight(FontWeight::SEMIBOLD))
                                .truncate()
                                .child(detail),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The suggestions under the box while it has the keyboard and says something: "We couldn't find any matches."
    /// once nothing matches and the directory answered.
    fn suggestions_panel(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.suggesting || self.query(cx).is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = if self.suggestions.is_empty() {
            if self.asking.is_some() {
                return None;
            }
            vec![
                div()
                    .debug_selector(|| "share-file-no-match".to_owned())
                    .px_3()
                    .py_2()
                    .text_sm()
                    .child(NO_MATCH)
                    .into_any_element(),
            ]
        } else {
            self.suggestions
                .iter()
                .enumerate()
                .map(|(at, target)| self.suggestion(at, target, palette, cx))
                .collect()
        };
        Some(
            div()
                .id("share-file-suggestions")
                .debug_selector(|| "share-file-suggestions".to_owned())
                .absolute()
                .occlude()
                .top(px(2.0))
                .left_0()
                .right_0()
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

    /// The file's card in the preview: its type's icon, its name and what its link lets people do, over its picture.
    fn file_card(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let kind = self.file.kind();
        let permission = self
            .permission
            .as_ref()
            .and_then(|permission| permission.description.clone());
        let state = match self.file.share_url.as_deref() {
            Some(share_url) if kind.has_thumbnail() => thumbnail(share_url, &self.source, cx),
            _ => PictureState::Missing,
        };
        let picture = match state {
            PictureState::Shown(image, _) => img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            PictureState::Reading => div().size_full().into_any_element(),
            PictureState::Missing => no_picture(kind, "share-file-picture-icon"),
        };
        div()
            .debug_selector(|| "share-file-card".to_owned())
            .w(px(CARD))
            .max_w_full()
            .flex()
            .flex_col()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(header_colour(palette)))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(kind_icon(kind, 28.0, "share-file-icon".to_owned()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_sm()
                                    .truncate()
                                    .child(self.file.name.clone()),
                            )
                            .children(permission.map(|words| {
                                div()
                                    .debug_selector(|| "share-file-permission".to_owned())
                                    .text_xs()
                                    .truncate()
                                    .child(words)
                            })),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .aspect_ratio(PICTURE_RATIO)
                    .bg(rgb(picture_colour(palette)))
                    .child(picture),
            )
            .into_any_element()
    }

    /// "Message preview": the message's box (the format bar over it while Format is on) over the file's card, and the
    /// box's format and emoji buttons.
    fn preview(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        div()
            .debug_selector(|| "share-file-preview".to_owned())
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(field(palette)))
            .children(self.format_line(palette))
            .child(
                div()
                    .id("share-file-message")
                    .debug_selector(|| "share-file-message".to_owned())
                    .max_h(px(160.0))
                    .overflow_y_scroll()
                    .child(self.message.clone()),
            )
            .child(self.file_card(palette, cx))
            .child(self.message_tools(cx))
            .into_any_element()
    }

    fn buttons(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let sharing = self.step == Step::Sharing;
        let copy = selected_by(
            "share-file-copy-link",
            Button::new("share-file-copy-link")
                .outline()
                .label(COPY_LINK)
                .disabled(sharing || self.file.open_link().is_none())
                .on_click(cx.listener(|this, _, _, cx| this.copy_link(cx))),
        );
        let share = selected_by(
            "share-file-share",
            Button::new("share-file-share")
                .primary()
                .label(if matches!(self.step, Step::ConfirmRetry(_)) {
                    "Send again"
                } else {
                    SHARE
                })
                .spinning(sharing)
                .disabled(!self.can_share())
                .on_click(cx.listener(|this, _, window, cx| this.share(window, cx))),
        );
        let problem = self.problem().cloned().map(|words| {
            div()
                .debug_selector(|| "share-file-problem".to_owned())
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(rgb(theme::DANGER))
                .child(words)
        });
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .text_color(rgb(palette.foreground))
            .children(problem)
            .child(copy)
            .child(share)
            .into_any_element()
    }
}

/// The colour of the dialog's fields: Teams' tint of the surface toward the text.
fn field(palette: Palette) -> u32 {
    theme::mix(palette.surface, palette.foreground, 0.06)
}

impl Focusable for ShareFileDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ShareFileDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        if self.step == Step::LinkCreated {
            return self.link_created_view(palette, cx);
        }
        let suggestions = self.suggestions_panel(palette, cx);
        div()
            .debug_selector(|| "share-file-dialog".to_owned())
            .id("share-file-dialog")
            .key_context("ShareFileDialog")
            .track_focus(&self.focus)
            .occlude()
            .relative()
            .w(px(WIDTH))
            .max_w_full()
            .max_h(window.viewport_size().height - px(48.0))
            .p_8()
            .flex()
            .flex_col()
            .gap_5()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key))
            .child(self.header(palette, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Self::label(RECIPIENTS, true, palette))
                    .child(self.recipients(window, palette, cx))
                    // deferred: the list is drawn over "Message preview", which comes after it, as Teams draws it
                    .child(
                        div()
                            .relative()
                            .children(suggestions.map(|list| gpui::deferred(list).with_priority(1))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Self::label(PREVIEW, false, palette))
                    .child(self.preview(palette, cx)),
            )
            .child(self.buttons(palette, cx))
            .children(self.more_menu(palette, cx))
            .children(self.emoji_overlay())
            .focus_trap("share-file-trap", &self.focus)
            .into_any_element()
    }
}
