//! A team channel (M2 Task 16), as Teams' channel shows it: its posts in a [`ChatView`] of the channel
//! ([`Place::Channel`]: a post's replies summed up under it, its compose box "Message in <channel>" posting to the
//! channel), and once a post's "Reply in thread" or its replies' summary is clicked, the post's thread in a pane beside
//! them, a [`ChatView`] of the thread ([`Place::Thread`]: the post and its replies, its box "Reply in thread" posting to
//! the post's chain) under Teams' pane header (its title, and Close). Reactions, edits, deletes, copying and workflows
//! are the chat view's own, in both.
//!
//! - **Synced each time it is opened** ([`ChannelView::sync`], [`Core::sync_conversation`]): a channel other than a
//!   team's general one is in no conversation list, so no catch-up reads it, and a sync that failed is tried again
//!   only so. What it stores comes as the models' change, which the views read again on.
//! - **Opened at a message** ([`ChannelView::reveal`], a search's hit): the posts at the message's thread's post, and
//!   a reply's thread opened at it.
//! - **Drafts are kept** as a chat's are: a thread's box holding text, a reply or an upload is kept when its thread is
//!   closed or another opens, and given back when that thread opens again; the shell keeps the channel's and its
//!   threads' boxes ([`ChannelView::drafts`]) while another channel is shown and hands them back to its next view.
//! - **What it tells** ([`ChannelViewEvent`]): a forward's original, a meeting card's Edit, and a workflow run or made
//!   on a message, for the shell to carry out.
//!
//! What the views show is traced as theirs (`channel-view shown`, `thread-view shown`), and a thread's opening and
//! closing (`channel-thread opened`/`closed`, its post's id), never text or a name.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use gpui_component::{Sizable as _, Size};

use crate::chat_view::{ChatView, ChatViewEvent, Place};
use crate::compose::{self, Compose};
use crate::forwarded::Original;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::shell::ShellParts;
use crate::theme::{self, Palette};
use crate::trace::TraceLine;

/// The thread pane's width, px: Teams' pane beside the channel.
pub const THREAD_WIDTH: f32 = 440.0;
/// The thread pane's title.
pub const THREAD_TITLE: &str = "Thread";

/// What the channel view tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelViewEvent {
    /// A forwarded message's original time was clicked: the original is opened in the app when the account has its
    /// chat, else in Teams on the web (the shell's to decide, as for a chat's forward).
    OpenOriginal(Original),
    /// A meeting card's Edit: the meeting's details, read from the team's calendar by its id (`ical_uid`), opened to
    /// change or cancel it (the shell's to open, as it knows the channel's team).
    EditMeeting { ical_uid: String },
    /// A running meeting's Join on its post: the pre-join of the meeting (`meeting`) of the post `post` in the channel
    /// `channel`, titled `title` (the shell's to open, as for the channel header's Join). The join goes into the meeting
    /// of that post, not into another meeting of the channel.
    JoinMeeting {
        channel: String,
        post: String,
        meeting: teams_proto::MeetingInfo,
        title: String,
    },
    /// A workflow was chosen in a post's or a reply's "More options" (or "More actions"): it runs on that message
    /// ([`crate::workflow_run`]); the shell names its team.
    RunWorkflow {
        flow: crate::workflow_dialog::FlowRef,
        message: crate::workflow_run::MessageRef,
    },
    /// "More actions" → "Create workflows" on a post or a reply: the Workflows dialog for it.
    CreateWorkflows(crate::workflow_run::MessageRef),
    /// An app's dialog opens over the window ([`crate::app_dialog`]): a post's or a reply's "More actions", or a
    /// card's `task/fetch` button.
    AppDialog(Box<crate::app_dialog::DialogSeed>),
    /// An app the account hasn't added was chosen in "More actions": its details open, to add it.
    AppDetails(Box<teams_proto::StoreApp>),
    /// A post's "Translation" → "Settings".
    TranslationSettings,
    /// A file card's Share: "Share this file" for that file, as a chat's ([`ChatViewEvent::ShareFile`]).
    ShareFile(Box<teams_proto::chat::SharedFile>),
}

/// A post's thread, open beside the posts.
struct Thread {
    post: String,
    view: Entity<ChatView>,
    _events: Subscription,
}

/// The channel view (see the module docs).
pub struct ChannelView {
    parts: ShellParts,
    channel: String,
    /// The channel's name, as its compose box says it ("Message in <channel>").
    name: SharedString,
    posts: Entity<ChatView>,
    thread: Option<Thread>,
    /// Threads' boxes put away with a draft in them (their thread closed, or another opened), by the post's chain they
    /// write to: given back when the thread opens again.
    drafts: HashMap<String, Entity<Compose>>,
    /// The team's and the channel's names, for a meeting card's line and a channel meeting's call.
    place: Option<(SharedString, SharedString)>,
    /// The sync asked for on open, while it runs.
    syncing: Option<Task<()>>,
    /// A search's hit being looked up to open at.
    revealing: Option<Task<()>>,
    _posts: Subscription,
}

impl EventEmitter<ChannelViewEvent> for ChannelView {}

impl ChannelView {
    /// The view of `channel`, named `name` (empty when the list doesn't know it yet): synced from the service at once,
    /// read from the store. `drafts` are its boxes an earlier view of it kept ([`Self::drafts`]), by the conversation
    /// they write to: the channel's posts' box, and its threads' by their posts' chains.
    pub fn new(
        parts: ShellParts,
        channel: String,
        name: SharedString,
        mut drafts: HashMap<String, Entity<Compose>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let compose = drafts.remove(&channel).unwrap_or_else(|| {
            let placeholder = compose::channel_placeholder(&name).into();
            ChatView::channel_box(&parts, &channel, &Place::Channel, placeholder, window, cx)
        });
        compose.update(cx, |compose, cx| compose.name_channel(name.clone(), cx));
        let posts = {
            let (parts, channel) = (parts.clone(), channel.clone());
            cx.new(|cx| ChatView::in_place(parts, channel, Place::Channel, compose, window, cx))
        };
        let events = Self::follow(&posts, window, cx);
        let mut view = Self {
            parts,
            channel,
            name,
            posts,
            thread: None,
            drafts,
            place: None,
            syncing: None,
            revealing: None,
            _posts: events,
        };
        view.sync(cx);
        view
    }

    /// Follows what a view of the channel tells: a post's thread opened here, the rest handed on.
    fn follow(view: &Entity<ChatView>, window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe_in(view, window, |this: &mut Self, _, event, window, cx| match event {
            ChatViewEvent::OpenThread { post } => this.open_thread(post.clone(), window, cx),
            ChatViewEvent::OpenOriginal(original) => cx.emit(ChannelViewEvent::OpenOriginal(original.clone())),
            ChatViewEvent::EditMeeting { ical_uid } => cx.emit(ChannelViewEvent::EditMeeting {
                ical_uid: ical_uid.clone(),
            }),
            ChatViewEvent::JoinMeeting { post, meeting, title } => cx.emit(ChannelViewEvent::JoinMeeting {
                channel: this.channel().to_owned(),
                post: post.clone(),
                meeting: meeting.clone(),
                title: title.clone(),
            }),
            ChatViewEvent::RunWorkflow { flow, message } => cx.emit(ChannelViewEvent::RunWorkflow {
                flow: flow.clone(),
                message: message.clone(),
            }),
            ChatViewEvent::CreateWorkflows(message) => cx.emit(ChannelViewEvent::CreateWorkflows(message.clone())),
            ChatViewEvent::ShareFile(file) => cx.emit(ChannelViewEvent::ShareFile(file.clone())),
            ChatViewEvent::AppDialog(seed) => cx.emit(ChannelViewEvent::AppDialog(seed.clone())),
            ChatViewEvent::AppDetails(store) => cx.emit(ChannelViewEvent::AppDetails(store.clone())),
            ChatViewEvent::TranslationSettings => cx.emit(ChannelViewEvent::TranslationSettings),
            ChatViewEvent::Action { .. }
            | ChatViewEvent::OpenRecap { .. }
            | ChatViewEvent::OpenSavedTranscript(_)
            | ChatViewEvent::ShowStore => {}
        })
    }

    /// The team's and the channel's names, as a meeting card's line shows them; the compose box says the channel's.
    pub fn set_place(&mut self, team: SharedString, channel: SharedString, cx: &mut Context<Self>) {
        let place = Some((team, channel)).filter(|(team, channel)| !team.is_empty() && !channel.is_empty());
        if self.place == place {
            return;
        }
        self.place.clone_from(&place);
        if let Some((team, channel)) = place {
            if self.name != channel {
                self.name = channel.clone();
                let compose = self.posts.read(cx).compose().clone();
                let placeholder = compose::channel_placeholder(&channel);
                compose.update(cx, |compose, cx| {
                    compose.in_channel(compose::Writes::Channel, placeholder, cx);
                });
                for view in self.views() {
                    let compose = view.read(cx).compose().clone();
                    compose.update(cx, |compose, cx| compose.name_channel(channel.clone(), cx));
                }
            }
            for view in self.views() {
                let (team, channel) = (team.clone(), channel.clone());
                view.update(cx, |view, cx| view.set_channel_place(team, channel, cx));
            }
        }
        cx.notify();
    }

    /// Asks the engine to sync the channel from the service, unless a sync it asked for still runs. Each opening of
    /// the channel asks it, the view's first and any later one.
    pub fn sync(&mut self, cx: &mut Context<Self>) {
        if self.syncing.is_some() {
            return;
        }
        let (core, channel) = (Arc::clone(&self.parts.core), self.channel.clone());
        self.syncing = Some(cx.spawn(async move |this, cx| {
            // what it stores comes as the models' change too; a failed sync leaves what the store holds shown
            let _ = core.sync_conversation(channel).await;
            let _ = this.update(cx, |view, cx| {
                view.syncing = None;
                cx.notify();
            });
        }));
    }

    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    /// Whether the sync asked for on open still runs.
    #[must_use]
    pub fn syncing(&self) -> bool {
        self.syncing.is_some()
    }

    /// The channel's posts.
    #[must_use]
    pub fn posts(&self) -> &Entity<ChatView> {
        &self.posts
    }

    /// The thread open beside the posts: its post's id and its view.
    #[must_use]
    pub fn thread(&self) -> Option<(&str, &Entity<ChatView>)> {
        self.thread.as_ref().map(|thread| (thread.post.as_str(), &thread.view))
    }

    /// The views shown: the posts, and the thread when one is open.
    fn views(&self) -> Vec<Entity<ChatView>> {
        std::iter::once(self.posts.clone())
            .chain(self.thread.as_ref().map(|thread| thread.view.clone()))
            .collect()
    }

    /// Opens post `post`'s thread beside the posts (in place of one open), its compose box focused, as Teams does; a
    /// thread open already stays as it is.
    pub fn open_thread(&mut self, post: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.as_ref().is_some_and(|thread| thread.post == post) {
            return;
        }
        self.parts.trace.record(
            &TraceLine::new("channel-thread", "opened")
                .conversation(&self.channel)
                .messages([post.as_str()]),
        );
        self.put_thread_away(cx);
        let place = Place::Thread { post: post.clone() };
        let compose = self
            .drafts
            .remove(&crate::chat_view::writes_to(&self.channel, &place))
            .unwrap_or_else(|| {
                let placeholder = compose::THREAD_PLACEHOLDER.into();
                ChatView::channel_box(&self.parts, &self.channel, &place, placeholder, window, cx)
            });
        let view = {
            let (parts, channel) = (self.parts.clone(), self.channel.clone());
            cx.new(|cx| ChatView::in_place(parts, channel, place, compose, window, cx))
        };
        if let Some((team, channel)) = self.place.clone() {
            view.update(cx, |view, cx| view.set_channel_place(team, channel, cx));
        }
        let compose = view.read(cx).compose().clone();
        compose.update(cx, |compose, cx| compose.name_channel(self.name.clone(), cx));
        compose.update(cx, |compose, cx| compose.focus(window, cx));
        let events = Self::follow(&view, window, cx);
        self.thread = Some(Thread {
            post,
            view,
            _events: events,
        });
        cx.notify();
    }

    /// An app dialog's result card goes into the box that writes to `conversation`: the posts' box for the channel, a
    /// post's thread's box for its chain (the thread opens, as a reply's box takes it). `false` when `conversation` is
    /// not this channel's.
    pub fn add_app_card(
        &mut self,
        conversation: &str,
        card: teams_proto::chat::AppCard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let view = if conversation == self.channel {
            self.posts.clone()
        } else {
            match teams_proto::reply_chain(conversation) {
                Some((channel, post)) if channel == self.channel => {
                    self.open_thread(post.to_owned(), window, cx);
                    match &self.thread {
                        Some(thread) => thread.view.clone(),
                        None => return false,
                    }
                }
                _ => return false,
            }
        };
        view.update(cx, |view, cx| view.add_app_card(card, window, cx));
        true
    }

    /// Closes the thread beside the posts; its box's draft is kept for the thread's next opening.
    pub fn close_thread(&mut self, cx: &mut Context<Self>) {
        if let Some(post) = self.put_thread_away(cx) {
            self.parts.trace.record(
                &TraceLine::new("channel-thread", "closed")
                    .conversation(&self.channel)
                    .messages([post.as_str()]),
            );
            cx.notify();
        }
    }

    /// Takes the open thread away, keeping its box when it holds a draft: the thread's post, if one was open.
    fn put_thread_away(&mut self, cx: &App) -> Option<String> {
        self.drafts.retain(|_, compose| compose.read(cx).has_draft(cx));
        let thread = self.thread.take()?;
        let compose = thread.view.read(cx).compose().clone();
        if compose.read(cx).has_draft(cx) {
            let place = Place::Thread {
                post: thread.post.clone(),
            };
            self.drafts
                .insert(crate::chat_view::writes_to(&self.channel, &place), compose);
        }
        Some(thread.post)
    }

    /// The boxes holding a draft, by the conversation each writes to (the channel's posts' box by the channel, a
    /// thread's by its post's chain): the shell keeps them while another channel is shown, for the channel's next view
    /// ([`Self::new`]).
    #[must_use]
    pub fn drafts(&self, cx: &App) -> Vec<(String, Entity<Compose>)> {
        let posts = (self.channel.clone(), self.posts.read(cx).compose().clone());
        let open = self.thread.as_ref().map(|thread| {
            let place = Place::Thread {
                post: thread.post.clone(),
            };
            let target = crate::chat_view::writes_to(&self.channel, &place);
            (target, thread.view.read(cx).compose().clone())
        });
        std::iter::once(posts)
            .chain(open)
            .chain(
                self.drafts
                    .iter()
                    .map(|(target, compose)| (target.clone(), compose.clone())),
            )
            .filter(|(_, compose)| compose.read(cx).has_draft(cx))
            .collect()
    }

    /// Opens the view at `message` (which arrived at `arrival_time`) in the thread of post `thread` (a search's hit):
    /// the posts at the post (read back through the service's older pages when the store doesn't hold it), and a
    /// reply's thread opened at the reply.
    pub fn reveal(
        &mut self,
        thread: String,
        message: String,
        arrival_time: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if message != thread {
            self.open_thread(thread.clone(), window, cx);
            if let Some(open) = &self.thread {
                let message = message.clone();
                open.view.update(cx, |view, cx| view.reveal(arrival_time, message, cx));
            }
        }
        // a post is where its arrival puts it; a reply's post came before it, at a time the store tells, or, for a post
        // older than the store holds, its id does (a message's id is its arrival time, ms: docs/protocol/chat.md)
        let (core, channel) = (Arc::clone(&self.parts.core), self.channel.clone());
        self.revealing = Some(cx.spawn(async move |this, cx| {
            let at = if message == thread {
                Some(arrival_time)
            } else {
                core.message(channel, thread.clone())
                    .await
                    .ok()
                    .flatten()
                    .map(|post| post.arrival_time)
                    .or_else(|| thread.parse().ok())
            };
            let _ = this.update(cx, |this, cx| {
                this.revealing = None;
                if let Some(at) = at {
                    this.posts.update(cx, |posts, cx| posts.reveal(at, thread, cx));
                }
            });
        }));
    }

    /// The thread's pane: Teams' header (the title, and Close) over the thread's view.
    fn thread_pane(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread = self.thread.as_ref()?;
        let close = selected_by(
            "channel-thread-close",
            icon_button("channel-thread-close", Glyph::X, "Close thread")
                .with_size(Size::Size(px(28.0)))
                .on_click(cx.listener(|this, _, _, cx| this.close_thread(cx))),
        );
        let header = crate::shell::pane_header(None, THREAD_TITLE, palette)
            .child(div().flex_1())
            .child(close)
            .debug_selector(|| "channel-thread-header".to_owned());
        Some(
            div()
                .debug_selector(|| "channel-thread".to_owned())
                .w(px(THREAD_WIDTH))
                .flex_none()
                .h_full()
                .flex()
                .flex_col()
                .border_l_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.background))
                .child(header)
                .child(div().flex_1().min_h_0().child(thread.view.clone()))
                .into_any_element(),
        )
    }
}

impl Render for ChannelView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        div()
            .debug_selector(|| "channel-view".to_owned())
            .size_full()
            .flex()
            .bg(rgb(palette.background))
            .child(div().flex_1().min_w_0().h_full().child(self.posts.clone()))
            .children(self.thread_pane(palette, cx))
    }
}
