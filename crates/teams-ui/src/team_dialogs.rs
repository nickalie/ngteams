//! The dialogs of a team's menu and pages, as Teams has them (`docs/protocol/teams.md`), in Teams' words:
//!
//! - [`CreateTeamDialog`]: "Create a team", from scratch, the name, the description, the team type on a step of its own and
//!   the first channel's name; then the team's [`AddMembersDialog`], with Skip.
//! - [`AddMembersDialog`]: a box that finds people in the organization ([`Core::addable_people`]), those picked each with a
//!   role, Add; for a member of a private team, a request to the owners.
//! - [`EditTeamDialog`]: a team's name, description and privacy.
//! - [`ConfirmDialog`]: Hide all channels, Archive, Leave team and Delete team; deleting asks for a tick first.
//! - [`TagDialog`]: a tag's name, description and people.
//!
//! A dialog tells the shell by a [`TeamDialogEvent`]. Nothing here sends anything but through [`Core`], and only on its own
//! confirming button. Traced: `team-dialog opened`, `done`, with the team by id and a `kind`, never a name.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, ClickEvent, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::spinner::Spinner;
use gpui_component::{Disableable as _, FocusTrapElement as _, Sizable as _};
use teams_core::{TeamChange, TeamWritten};
use teams_proto::Mri;
use teams_proto::{MemberUpdate, NewTeam, OwnTeamView, TeamPerson, TeamRole, TeamTag, TeamWrite};
use teams_store::{ChannelKind, Team, TeamAbout, TeamChannel};

use crate::bridge::Core;
use crate::channel_dialog::name_problem;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard::{self, Handles, PopupFocus};
use crate::shell::menu_row;
use crate::teams_page::{popup_menu, with_popup};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The longest a team's name may be, as the web client's box.
pub const TEAM_NAME_LIMIT: usize = 256;
/// The longest a team's description may be.
pub const TEAM_DESCRIPTION_LIMIT: usize = 1024;
/// The longest a tag's name may be.
pub const TAG_NAME_LIMIT: usize = 40;
/// How long the people box waits after the typing.
const PAUSE: Duration = Duration::from_millis(250);
/// How many people the box lists.
const MOST: usize = 8;

/// What a dialog tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamDialogEvent {
    /// It closed with nothing more to do.
    Closed,
    /// A new team: its Add members step comes next.
    Created(Team),
    /// It closed, and something changed that a page showing the team reads again; `words` go in a toast.
    Changed { team: String, words: &'static str },
    /// The team is gone for the account: a page on it closes.
    Gone { team: String, words: &'static str },
    /// "More create team options": a team from a template, another team or a group.
    MoreOptions,
    /// A channel's notification settings' "Go to Settings": the settings' Notifications page opens.
    NotificationSettings,
    /// The channel's Workflows asked for the Workflows app in the main window: its Home ("Workflows Home"), or its
    /// "Your workflows" (`see_all`, the list's "See all").
    Workflows { see_all: bool },
    /// A workflow run from a message was chosen to run on the message the Workflows dialog was opened from: its run
    /// dialog opens in the dialog's place.
    RunWorkflow {
        flow: crate::workflow_dialog::FlowRef,
        message: crate::workflow_run::MessageRef,
    },
    /// An app was added or removed: the store reads the account's apps again; `words` go in a toast.
    AppsChanged { words: &'static str },
    /// An app's "Add to a chat" (`team` false) or "Add to a team": the place's picker opens in the dialog's place;
    /// `changed` when the details' Add went through first, so the store reads the account's apps again.
    AddAppTo {
        app: Box<teams_proto::StoreApp>,
        team: bool,
        changed: bool,
    },
    /// A chat opens (an app's Open: its bot's chat).
    OpenChat(String),
    /// "Get more apps": the store opens.
    ShowStore,
    /// An app's dialog gave a result: its card goes into the compose box of `conversation`, as Teams puts it there.
    AppCard {
        conversation: String,
        card: Box<teams_proto::chat::AppCard>,
    },
}

/// Whether `text` holds a link, as the bundle tells it for a team's name and description.
#[must_use]
pub fn holds_link(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let word = word.to_lowercase();
        word.starts_with("http://") || word.starts_with("https://") || word.starts_with("www.")
    })
}

/// What's wrong with a team's name, in Teams' words; `None` when it reads.
#[must_use]
pub fn team_name_problem(name: &str) -> Option<&'static str> {
    if holds_link(name) {
        Some("Links are not allowed in the team name")
    } else if name.chars().count() > TEAM_NAME_LIMIT {
        Some("Character limit reached")
    } else {
        None
    }
}

/// What's wrong with a team's description, in Teams' words; `None` when it reads.
#[must_use]
pub fn team_description_problem(description: &str) -> Option<&'static str> {
    if holds_link(description) {
        Some("Links are not allowed in the team description")
    } else if description.chars().count() > TEAM_DESCRIPTION_LIMIT {
        Some("Character limit reached")
    } else {
        None
    }
}

/// A dialog's surface.
pub(crate) fn surface(id: &'static str, palette: Palette, window: &Window) -> gpui::Stateful<gpui::Div> {
    div()
        .debug_selector(move || id.to_owned())
        .id(id)
        .occlude()
        .w(px(600.0))
        .max_w_full()
        .max_h(window.viewport_size().height - px(48.0))
        .p_6()
        .flex()
        .flex_col()
        .gap_3()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_color(rgb(palette.foreground))
        .shadow_lg()
        .on_click(|_, _, cx: &mut App| cx.stop_propagation())
}

pub(crate) fn title(words: impl Into<SharedString>) -> gpui::Div {
    div().text_xl().font_weight(gpui::FontWeight::BOLD).child(words.into())
}

pub(crate) fn label(words: &'static str, required: bool, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .gap_1()
        .text_sm()
        .child(words)
        .when(required, |label| {
            label.child(div().text_color(rgb(theme::DANGER)).child("*"))
        })
        .text_color(rgb(palette.foreground))
}

pub(crate) fn problem(selector: &'static str, words: impl Into<SharedString>) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .text_xs()
        .text_color(rgb(theme::DANGER))
        .child(words.into())
}

pub(crate) fn waiting(selector: &'static str, words: &'static str, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .h(px(320.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .text_color(rgb(palette.muted_foreground))
        .child(Spinner::new())
        .child(words)
}

pub(crate) fn buttons() -> gpui::Div {
    div().flex().justify_end().gap_2().pt_2()
}

/// Whether a team's row in the store is made of a new team's form (its thread and group come with the answer).
fn new_team_row(name: &str, description: &str, first_channel: &str, public: bool) -> Team {
    Team {
        id: String::new(),
        name: name.to_owned(),
        archived: false,
        group_id: None,
        mailbox: None,
        owner: true,
        can_add_channel: true,
        can_add_private_channel: true,
        about: TeamAbout {
            description: description.to_owned(),
            public,
            people: 1,
            ..TeamAbout::default()
        },
        channels: vec![TeamChannel {
            id: String::new(),
            name: first_channel.to_owned(),
            general: true,
            kind: ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }],
    }
}

/// Where [`CreateTeamDialog`] is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreateStep {
    Form,
    /// "What kind of team will this be?"
    Kind,
    Creating,
    Failed,
}

/// "Create a team" (see the module docs).
pub struct CreateTeamDialog {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    first_channel: Entity<InputState>,
    public: bool,
    step: CreateStep,
    general_menu: bool,
    /// The keyboard on the "..." of the first channel's name's menu.
    general_popup: PopupFocus,
    /// The kind cards' focus.
    kinds: Handles,
    /// The team type's button, where the keyboard goes back to from the kind step.
    kind_focus: FocusHandle,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _inputs: [Subscription; 3],
}

impl EventEmitter<TeamDialogEvent> for CreateTeamDialog {}

impl CreateTeamDialog {
    /// The dialog, its name box focused.
    pub fn new(core: Arc<dyn Core>, trace: Arc<ViewTrace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Give your team a name"));
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Let people know what this team is all about")
                .soft_wrap(true)
                .auto_grow(3, 3)
        });
        let first_channel = cx.new(|cx| InputState::new(window, cx).placeholder("Name your channel"));
        let touched = [
            cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&description, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&first_channel, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        name.update(cx, |name, cx| name.focus(window, cx));
        trace.record(&TraceLine::new("team-dialog", "opened").kind("create"));
        Self {
            core,
            trace,
            name,
            description,
            first_channel,
            public: false,
            step: CreateStep::Form,
            general_menu: false,
            general_popup: PopupFocus::new(cx),
            kinds: Handles::default(),
            kind_focus: cx.focus_handle(),
            working: None,
            focus: cx.focus_handle(),
            _inputs: touched,
        }
    }

    /// Where it is.
    #[must_use]
    pub fn step(&self) -> &CreateStep {
        &self.step
    }

    /// Whether the new team is to be public.
    #[must_use]
    pub fn public(&self) -> bool {
        self.public
    }

    /// Fills the form.
    pub fn fill(
        &mut self,
        name: &str,
        description: &str,
        first_channel: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name
            .update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
        self.description
            .update(cx, |input, cx| input.set_value(description.to_owned(), window, cx));
        self.first_channel
            .update(cx, |input, cx| input.set_value(first_channel.to_owned(), window, cx));
        cx.notify();
    }

    fn values(&self, cx: &App) -> (String, String, String) {
        (
            self.name.read(cx).value().trim().to_owned(),
            self.description.read(cx).value().trim().to_owned(),
            self.first_channel.read(cx).value().trim().to_owned(),
        )
    }

    fn channel_problem(first_channel: &str) -> Option<String> {
        if first_channel.is_empty() || first_channel.eq_ignore_ascii_case("general") {
            None
        } else {
            name_problem(first_channel, &[])
        }
    }

    /// Whether Create is on.
    #[must_use]
    pub fn can_create(&self, cx: &App) -> bool {
        let (name, description, first_channel) = self.values(cx);
        !name.is_empty()
            && !first_channel.is_empty()
            && team_name_problem(&name).is_none()
            && team_description_problem(&description).is_none()
            && Self::channel_problem(&first_channel).is_none()
    }

    /// Picks who can join, and goes back to the form.
    pub fn pick_kind(&mut self, public: bool, cx: &mut Context<Self>) {
        self.public = public;
        self.step = CreateStep::Form;
        cx.notify();
    }

    /// The team type's button: the step that picks it.
    pub fn show_kind(&mut self, cx: &mut Context<Self>) {
        self.step = CreateStep::Kind;
        cx.notify();
    }

    /// Cancel.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(TeamDialogEvent::Closed);
    }

    /// Create: the team goes out; once it is made its Add members step comes.
    pub fn create(&mut self, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }
        let (name, description, first_channel) = self.values(cx);
        let row = new_team_row(&name, &description, &first_channel, self.public);
        let write = TeamWrite::Create(NewTeam {
            name,
            description,
            first_channel,
            public: self.public,
        });
        self.step = CreateStep::Creating;
        cx.notify();
        let reply = self.core.team_write(write, Some(TeamChange::Created(row.clone())));
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                if let Ok(written) = answer {
                    this.created(row, written, cx);
                } else {
                    this.step = CreateStep::Failed;
                    cx.notify();
                }
            });
        }));
    }

    fn created(&mut self, mut team: Team, written: TeamWritten, cx: &mut Context<Self>) {
        let Some((thread, group)) = written.created else {
            // made, by its answer's 2xx, without an answer that names it: it comes with the next read of the teams
            cx.emit(TeamDialogEvent::Changed {
                team: String::new(),
                words: "Nice work! You successfully created a team.",
            });
            return;
        };
        team.id.clone_from(&thread);
        team.group_id = group;
        if let Some(general) = team.channels.first_mut() {
            general.id = thread;
        }
        self.trace.record(
            &TraceLine::new("team-dialog", "done")
                .kind("create")
                .conversation(&team.id),
        );
        cx.emit(TeamDialogEvent::Created(team));
    }

    /// The form.
    /// "You're creating a team from scratch." and its link to the other ways.
    fn scratch_line(palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .text_sm()
            .child("You're creating a team from scratch.")
            .child(keyboard::stop(
                "create-team-more",
                div()
                    .id("create-team-more")
                    .debug_selector(|| "create-team-more".to_owned())
                    .text_color(rgb(palette.brand))
                    .underline()
                    .cursor_pointer()
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::MoreOptions)))
                    .child("More create team options"),
                palette,
            ))
    }

    fn form(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let (name, description, first_channel) = self.values(cx);
        let can_create = self.can_create(cx);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(title("Create a team"))
            .child(Self::scratch_line(palette, cx))
            .child(label("Team name", true, palette))
            .child(selected_by_full("create-team-name", Input::new(&self.name)))
            .children(team_name_problem(&name).map(|words| problem("create-team-name-problem", words)))
            .child(label("Description", false, palette))
            .child(selected_by_full(
                "create-team-description",
                Textarea::new(&self.description),
            ))
            .children(
                team_description_problem(&description).map(|words| problem("create-team-description-problem", words)),
            )
            .child(label("Team type", false, palette))
            .child(
                div().flex().child(selected_by(
                    "create-team-kind",
                    // an outline button of our own, its focus kept for the kind step to give back
                    keyboard::stop(
                        "create-team-kind",
                        div()
                            .id("create-team-kind")
                            .h_8()
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(palette.border))
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(palette.hover)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.show_kind(cx);
                                this.kinds.get(kind_key(this.public), cx).focus(window, cx);
                            }))
                            .child(
                                if self.public { Glyph::GLOBE } else { Glyph::LOCK }
                                    .icon()
                                    .size(px(16.0)),
                            )
                            .child(if self.public { "Public" } else { "Private" }),
                        palette,
                    )
                    .edge()
                    .handle(&self.kind_focus),
                )),
            )
            .child(label("First channel name", true, palette))
            .child(self.first_channel_row(palette, cx))
            .children(
                Self::channel_problem(&first_channel).map(|words| problem("create-team-first-channel-problem", words)),
            )
            .child(
                buttons()
                    .child(selected_by(
                        "create-team-cancel",
                        Button::new("create-team-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    ))
                    .child(selected_by(
                        "create-team-create",
                        Button::new("create-team-create")
                            .primary()
                            .label("Create")
                            .disabled(!can_create)
                            .tab_stop(can_create)
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx))),
                    )),
            )
    }

    /// "First channel name" with its "..." and that one's menu.
    fn first_channel_row(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let general_menu = self.general_menu.then(|| {
            let weak = cx.entity().downgrade();
            popup_menu(
                palette,
                cx.listener(|this, _, _, cx| {
                    this.general_menu = false;
                    cx.notify();
                }),
            )
            .keyed(
                self.general_popup.root(),
                keyboard::menu_keys(self.general_popup.root().clone(), move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.general_menu = false;
                        this.general_popup.closed(window, cx);
                        cx.notify();
                    });
                }),
            )
            .row(
                "create-team-general",
                menu_row("create-team-general", palette)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.general_menu = false;
                        this.general_popup.closed(window, cx);
                        this.first_channel
                            .update(cx, |input, cx| input.set_value("General".to_owned(), window, cx));
                        cx.notify();
                    }))
                    .child("Set name to General"),
                palette,
            )
        });
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().flex_1().child(selected_by_full(
                "create-team-first-channel",
                Input::new(&self.first_channel),
            )))
            .child(with_popup(
                selected_by(
                    "create-team-more-naming",
                    icon_button("create-team-more-naming", Glyph::ELLIPSIS, "More naming options").on_click(
                        cx.listener(|this, _, window, cx| {
                            this.general_menu = !this.general_menu;
                            if this.general_menu {
                                this.general_popup.opened(window, cx);
                            } else {
                                this.general_popup.closed(window, cx);
                            }
                            cx.notify();
                        }),
                    ),
                ),
                general_menu,
            ))
    }

    /// "What kind of team will this be?": a card per kind, Up and Down moving between them, Enter picking one.
    fn kind_step(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let card = |id: &'static str,
                    glyph: Glyph,
                    name: &'static str,
                    about: &'static str,
                    public: bool,
                    cx: &mut Context<Self>| {
            let card = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .p_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .cursor_pointer()
                .hover(move |card| card.bg(rgb(palette.hover)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.pick_kind(public, cx);
                    this.kind_focus.focus(window, cx);
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    if let Some(to) = keyboard::step(event, 2, usize::from(public)) {
                        cx.stop_propagation();
                        this.kinds.get(kind_key(to == 1), cx).focus(window, cx);
                    }
                }))
                .child(glyph.icon().size(px(28.0)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                        .child(div().text_sm().child(about)),
                );
            keyboard::stop(id, card, palette)
                .edge()
                .handle(&self.kinds.get(kind_key(public), cx))
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(title("What kind of team will this be?"))
            .child(div().text_sm().child("Privacy"))
            .child(card(
                "create-team-private",
                Glyph::LOCK,
                "Private",
                "People need permission to join",
                false,
                cx,
            ))
            .child(card(
                "create-team-public",
                Glyph::GLOBE,
                "Public",
                "Anyone in your org can join",
                true,
                cx,
            ))
            .child(
                div().pt_2().flex().child(selected_by(
                    "create-team-back",
                    Button::new("create-team-back")
                        .ghost()
                        .icon(Glyph::CHEVRON_LEFT.icon())
                        .label("Back")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.step = CreateStep::Form;
                            this.kind_focus.focus(window, cx);
                            cx.notify();
                        })),
                )),
            )
    }

    fn failed(palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .debug_selector(|| "create-team-failed".to_owned())
            .h(px(320.0))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("We ran into a problem creating your team"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Please try again later."),
            )
            .child(
                buttons()
                    .child(selected_by(
                        "create-team-close",
                        Button::new("create-team-close")
                            .outline()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    ))
                    .child(selected_by(
                        "create-team-retry",
                        Button::new("create-team-retry")
                            .primary()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step = CreateStep::Form;
                                cx.notify();
                            })),
                    )),
            )
    }
}

impl Render for CreateTeamDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let content = match self.step {
            CreateStep::Form => self.form(palette, cx),
            CreateStep::Kind => self.kind_step(palette, cx),
            CreateStep::Creating => waiting("create-team-creating", "Creating the team...", palette),
            CreateStep::Failed => Self::failed(palette, cx),
        };
        surface("create-team-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.cancel(cx);
                }
            }))
            .child(content)
            .focus_trap("create-team-dialog-trap", &self.focus)
    }
}

/// A kind card's key.
fn kind_key(public: bool) -> &'static str {
    if public { "public" } else { "private" }
}

/// `element` with `selector` as its test selector, as wide as it can be.
pub(crate) fn selected_by_full(selector: &'static str, element: impl IntoElement) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .child(element)
}

/// Someone picked in [`AddMembersDialog`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub person: TeamPerson,
    pub role: TeamRole,
}

/// Where [`AddMembersDialog`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddStep {
    Picking,
    Adding,
    /// `added` of `asked` went in.
    Result {
        added: usize,
        asked: usize,
    },
}

/// "Add members to …" (see the module docs).
pub struct AddMembersDialog {
    core: Arc<dyn Core>,
    team: Team,
    /// It follows a new team: Skip instead of Cancel.
    after_create: bool,
    people_box: Entity<InputState>,
    picked: Vec<Picked>,
    found: Vec<TeamPerson>,
    found_for: String,
    role_menu: Option<usize>,
    /// The keyboard on a role's menu.
    role_popup: PopupFocus,
    /// The people found: Up and Down move through them.
    found_focus: FocusHandle,
    step: AddStep,
    searching: Option<Task<()>>,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _typed: Subscription,
}

impl EventEmitter<TeamDialogEvent> for AddMembersDialog {}

impl AddMembersDialog {
    /// The dialog on `team`, its box focused.
    pub fn new(
        core: Arc<dyn Core>,
        trace: &ViewTrace,
        team: Team,
        after_create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let people_box = cx.new(|cx| InputState::new(window, cx).placeholder("Type a name or email"));
        let typed = cx.subscribe(&people_box, |this: &mut Self, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().trim().to_owned();
                this.search(text, cx);
            }
        });
        people_box.update(cx, |input, cx| input.focus(window, cx));
        trace.record(
            &TraceLine::new("team-dialog", "opened")
                .kind("add-members")
                .conversation(&team.id),
        );
        Self {
            core,
            team,
            after_create,
            people_box,
            picked: Vec::new(),
            found: Vec::new(),
            found_for: String::new(),
            role_menu: None,
            role_popup: PopupFocus::new(cx),
            found_focus: cx.focus_handle(),
            step: AddStep::Picking,
            searching: None,
            working: None,
            focus: cx.focus_handle(),
            _typed: typed,
        }
    }

    /// Whether a request goes to the owners instead of people going in: a member of a private team.
    #[must_use]
    pub fn sends_request(&self) -> bool {
        !self.team.owner && !self.team.about.public
    }

    /// Where it is.
    #[must_use]
    pub fn step(&self) -> AddStep {
        self.step
    }

    /// The people picked.
    #[must_use]
    pub fn picked(&self) -> &[Picked] {
        &self.picked
    }

    /// Asks for the people `text` finds, a moment after the typing stops.
    pub fn search(&mut self, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            self.found.clear();
            self.found_for.clear();
            self.searching = None;
            cx.notify();
            return;
        }
        let core = Arc::clone(&self.core);
        self.searching = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(PAUSE).await;
            let found = core.addable_people(text.clone()).await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.found = found.into_iter().take(MOST).collect();
                this.found_for = text;
                cx.notify();
            });
        }));
    }

    /// Picks someone found, clearing the box.
    pub fn pick(&mut self, person: TeamPerson, window: &mut Window, cx: &mut Context<Self>) {
        if !self.picked.iter().any(|picked| picked.person.mri == person.mri) {
            self.picked.push(Picked {
                person,
                role: TeamRole::Member,
            });
        }
        self.found.clear();
        self.found_for.clear();
        self.people_box.update(cx, |input, cx| {
            input.set_value(String::new(), window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Takes someone picked back.
    pub fn unpick(&mut self, mri: &str, cx: &mut Context<Self>) {
        self.picked.retain(|picked| picked.person.mri != mri);
        cx.notify();
    }

    /// Sets the role of the one picked at `at`.
    pub fn set_role(&mut self, at: usize, role: TeamRole, cx: &mut Context<Self>) {
        if let Some(picked) = self.picked.get_mut(at) {
            picked.role = role;
        }
        self.role_menu = None;
        cx.notify();
    }

    /// Add (or Send request).
    pub fn add(&mut self, cx: &mut Context<Self>) {
        let Some(group) = self.team.group_id.clone() else {
            return;
        };
        if self.picked.is_empty() {
            return;
        }
        let asked = self.picked.len();
        let members: Vec<(Mri, TeamRole)> = self
            .picked
            .iter()
            .map(|picked| (Mri::new(picked.person.mri.as_str()), picked.role))
            .collect();
        let write = if self.sends_request() {
            TeamWrite::RequestMembers {
                group,
                members: members.into_iter().map(|(member, _)| member).collect(),
            }
        } else {
            TeamWrite::AddMembers {
                team: self.team.id.clone(),
                group,
                members,
            }
        };
        self.step = AddStep::Adding;
        cx.notify();
        let reply = self.core.team_write(write, None);
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| this.added(answer.ok(), asked, cx));
        }));
    }

    /// The people went in, some of them, or none (`None`: the write didn't go).
    fn added(&mut self, written: Option<TeamWritten>, asked: usize, cx: &mut Context<Self>) {
        self.working = None;
        let Some(written) = written else {
            self.step = AddStep::Result { added: 0, asked };
            cx.notify();
            return;
        };
        if written.not_added.is_empty() {
            let words = if self.sends_request() {
                "Request sent"
            } else {
                "Members added"
            };
            cx.emit(TeamDialogEvent::Changed {
                team: self.team.id.clone(),
                words,
            });
            return;
        }
        let left = written.not_added;
        self.picked.retain(|picked| left.contains(&picked.person.mri));
        self.step = AddStep::Result {
            added: asked - left.len().min(asked),
            asked,
        };
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let (team, words) = (self.team.id.clone(), "");
        cx.emit(TeamDialogEvent::Changed { team, words });
    }

    /// The role's menu of the person picked at `at`: Member, Owner.
    fn role_menu_of(&self, at: usize, palette: Palette, cx: &mut Context<Self>) -> crate::teams_page::PopupMenu {
        let weak = cx.entity().downgrade();
        let mut menu = popup_menu(
            palette,
            cx.listener(|this, _, _, cx| {
                this.role_menu = None;
                cx.notify();
            }),
        )
        .keyed(
            self.role_popup.root(),
            keyboard::menu_keys(self.role_popup.root().clone(), move |window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.role_menu = None;
                    this.role_popup.closed(window, cx);
                    cx.notify();
                });
            }),
        );
        for (role, words, key) in [
            (TeamRole::Member, "Member", "member"),
            (TeamRole::Owner, "Owner", "owner"),
        ] {
            let key = format!("add-members-role-{at}-{key}");
            menu = menu.row(
                key.clone(),
                menu_row(key, palette)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_role(at, role, cx);
                        this.role_popup.closed(window, cx);
                    }))
                    .child(words),
                palette,
            );
        }
        menu
    }

    fn picked_row(&self, at: usize, picked: &Picked, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let mri = picked.person.mri.clone();
        let open = self.role_menu == Some(at);
        let menu = open.then(|| self.role_menu_of(at, palette, cx));
        let role_words = match picked.role {
            TeamRole::Member => "Member",
            TeamRole::Owner => "Owner",
        };
        let role = (!self.sends_request()).then(|| {
            with_popup(
                selected_by(
                    format!("add-members-role-{at}"),
                    Button::new(SharedString::from(format!("add-members-role-{at}")))
                        .ghost()
                        .small()
                        .label(role_words)
                        .dropdown_caret(true)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.role_menu = if this.role_menu == Some(at) { None } else { Some(at) };
                            if this.role_menu.is_some() {
                                this.role_popup.opened(window, cx);
                            } else {
                                this.role_popup.closed(window, cx);
                            }
                            cx.notify();
                        })),
                ),
                menu,
            )
        });
        div()
            .debug_selector(move || format!("add-members-picked-{at}"))
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().child(picked.person.name.clone()))
                    .children(picked.person.email.clone().map(|email| {
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(rgb(palette.muted_foreground))
                            .child(email)
                    })),
            )
            .children(role)
            .child(
                icon_button(
                    SharedString::from(format!("add-members-remove-{at}")),
                    Glyph::X,
                    format!("Remove {}", picked.person.name),
                )
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.unpick(&mri, cx))),
            )
            .into_any_element()
    }

    fn found_row(at: usize, person: TeamPerson, palette: Palette, cx: &mut Context<Self>) -> keyboard::Stop {
        let shown = person.clone();
        let row = div()
            .debug_selector(move || format!("add-members-found-{at}"))
            .id(SharedString::from(format!("add-members-found-{at}")))
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.pick(person.clone(), window, cx)))
            .child(shown.name)
            .children(
                shown
                    .email
                    .map(|email| div().text_xs().text_color(rgb(palette.muted_foreground)).child(email)),
            );
        keyboard::stop(SharedString::from(format!("add-members-found-{at}")), row, palette)
    }

    fn picking(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let (heading, about): (String, &'static str) = if self.sends_request() {
            (
                format!("Send a request to add people to the {} team", self.team.name),
                "Start typing a name or email.",
            )
        } else {
            (
                format!("Add members to {}", self.team.name),
                "Start typing a name, distribution list, or security group to add to your team. You can also add \
                 people outside your organization as guests by typing their email addresses. People outside your org \
                 will get an email letting them know they've been added.",
            )
        };
        let found: Vec<_> = self
            .found
            .iter()
            .cloned()
            .enumerate()
            .map(|(at, person)| Self::found_row(at, person, palette, cx))
            .collect();
        let picked: Vec<AnyElement> = self
            .picked
            .iter()
            .enumerate()
            .map(|(at, picked)| self.picked_row(at, picked, palette, cx))
            .collect();
        let none_found = !self.found_for.is_empty() && self.found.is_empty();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(title(heading))
            .child(div().text_sm().child(about))
            .child(selected_by_full("add-members-box", Input::new(&self.people_box)))
            .child(
                div()
                    .id("add-members-found")
                    .track_focus(&self.found_focus)
                    .on_key_down(keyboard::list_keys(self.found_focus.clone()))
                    .max_h(px(180.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(found)
                    .when(none_found, |list| {
                        list.child(
                            div()
                                .debug_selector(|| "add-members-none".to_owned())
                                .text_sm()
                                .text_color(rgb(palette.muted_foreground))
                                .child("We didn't find any matches"),
                        )
                    }),
            )
            .child(div().flex().flex_col().children(picked))
            .child(
                buttons()
                    .child(selected_by(
                        "add-members-cancel",
                        Button::new("add-members-cancel")
                            .outline()
                            .label(if self.after_create { "Skip" } else { "Cancel" })
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ))
                    .child(selected_by(
                        "add-members-add",
                        Button::new("add-members-add")
                            .primary()
                            .label(if self.sends_request() { "Send request" } else { "Add" })
                            .disabled(self.picked.is_empty())
                            .tab_stop(!self.picked.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                    )),
            )
    }

    /// Not everyone went in: Teams' count, and Try again.
    fn result(&self, added: usize, asked: usize, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let left: Vec<AnyElement> = self
            .picked
            .iter()
            .map(|picked| {
                div()
                    .flex()
                    .flex_col()
                    .child(picked.person.name.clone())
                    .children(
                        picked
                            .person
                            .email
                            .clone()
                            .map(|email| div().text_xs().text_color(rgb(palette.muted_foreground)).child(email)),
                    )
                    .into_any_element()
            })
            .collect();
        let remaining = asked - added;
        div()
            .debug_selector(|| "add-members-result".to_owned())
            .flex()
            .flex_col()
            .gap_3()
            .child(title(format!("{added} of {asked} members were added")))
            .child(div().text_sm().child("Looks like there was an issue."))
            .child(div().text_sm().child(format!(
                "Try adding the remaining {remaining} people. If it doesn't work, ask your IT admin to add them."
            )))
            .children(left)
            .child(
                buttons()
                    .child(selected_by(
                        "add-members-close",
                        Button::new("add-members-close")
                            .outline()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ))
                    .child(selected_by(
                        "add-members-retry",
                        Button::new("add-members-retry")
                            .primary()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                    )),
            )
    }
}

impl Render for AddMembersDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let content = match self.step {
            AddStep::Picking => self.picking(palette, cx),
            AddStep::Adding => waiting("add-members-adding", "Adding members...", palette),
            AddStep::Result { added, asked } => self.result(added, asked, palette, cx),
        };
        surface("add-members-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(cx);
                }
            }))
            .child(content)
            .focus_trap("add-members-dialog-trap", &self.focus)
    }
}

/// A team's name, description and privacy (see the module docs).
#[expect(
    clippy::struct_excessive_bools,
    reason = "its privacy, its open picker and its saving and failed states each stand alone, no state machine"
)]
pub struct EditTeamDialog {
    core: Arc<dyn Core>,
    team: Team,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    public: bool,
    privacy_open: bool,
    /// The keyboard on the privacy's menu.
    privacy_popup: PopupFocus,
    saving: bool,
    failed: bool,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _inputs: [Subscription; 2],
}

impl EventEmitter<TeamDialogEvent> for EditTeamDialog {}

impl EditTeamDialog {
    /// The dialog on `team`.
    pub fn new(core: Arc<dyn Core>, team: Team, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (shown_name, shown_description) = (team.name.clone(), team.about.description.clone());
        let name = cx.new(|cx| InputState::new(window, cx).default_value(shown_name));
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .default_value(shown_description)
                .soft_wrap(true)
                .auto_grow(3, 3)
        });
        let touched = [
            cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&description, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        name.update(cx, |name, cx| name.focus(window, cx));
        Self {
            core,
            public: team.about.public,
            team,
            name,
            description,
            privacy_open: false,
            privacy_popup: PopupFocus::new(cx),
            saving: false,
            failed: false,
            working: None,
            focus: cx.focus_handle(),
            _inputs: touched,
        }
    }

    fn values(&self, cx: &App) -> (String, String) {
        (
            self.name.read(cx).value().trim().to_owned(),
            self.description.read(cx).value().trim().to_owned(),
        )
    }

    /// Whether Done is on: something changed and it all reads.
    #[must_use]
    pub fn can_save(&self, cx: &App) -> bool {
        let (name, description) = self.values(cx);
        let changed = name != self.team.name
            || description != self.team.about.description
            || self.public != self.team.about.public;
        changed
            && !name.is_empty()
            && team_name_problem(&name).is_none()
            && team_description_problem(&description).is_none()
    }

    /// Sets the form's words.
    pub fn fill(&mut self, name: &str, description: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name
            .update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
        self.description
            .update(cx, |input, cx| input.set_value(description.to_owned(), window, cx));
        cx.notify();
    }

    /// Picks who can join.
    pub fn set_public(&mut self, public: bool, cx: &mut Context<Self>) {
        self.public = public;
        self.privacy_open = false;
        cx.notify();
    }

    /// Done: what changed goes out.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }
        let (name, description) = self.values(cx);
        let new_name = (name != self.team.name).then_some(name);
        let public = (self.public != self.team.about.public).then_some(self.public);
        let team = self.team.id.clone();
        let write = TeamWrite::Edit {
            team: team.clone(),
            name: new_name.clone(),
            description: description.clone(),
            public,
        };
        let change = TeamChange::Edited {
            team: team.clone(),
            name: new_name,
            description,
            public,
        };
        self.saving = true;
        self.failed = false;
        cx.notify();
        let reply = self.core.team_write(write, Some(change));
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                this.saving = false;
                if answer.is_ok() {
                    cx.emit(TeamDialogEvent::Changed { team, words: "" });
                } else {
                    this.failed = true;
                    cx.notify();
                }
            });
        }));
    }

    fn privacy(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        const PRIVATE: &str = "Private - Only team owners can add members";
        const PUBLIC: &str = "Public - Anyone in your organization can join";
        let menu = self.privacy_open.then(|| {
            let weak = cx.entity().downgrade();
            popup_menu(
                palette,
                cx.listener(|this, _, _, cx| {
                    this.privacy_open = false;
                    cx.notify();
                }),
            )
            .keyed(
                self.privacy_popup.root(),
                keyboard::menu_keys(self.privacy_popup.root().clone(), move |window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.privacy_open = false;
                        this.privacy_popup.closed(window, cx);
                        cx.notify();
                    });
                }),
            )
            .row(
                "edit-team-private",
                menu_row("edit-team-private", palette)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_public(false, cx);
                        this.privacy_popup.closed(window, cx);
                    }))
                    .child(PRIVATE),
                palette,
            )
            .row(
                "edit-team-public",
                menu_row("edit-team-public", palette)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_public(true, cx);
                        this.privacy_popup.closed(window, cx);
                    }))
                    .child(PUBLIC),
                palette,
            )
        });
        with_popup(
            keyboard::stop(
                "edit-team-privacy",
                div()
                    .id("edit-team-privacy")
                    .debug_selector(|| "edit-team-privacy".to_owned())
                    .px_3()
                    .py_1p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.privacy_open = !this.privacy_open;
                        if this.privacy_open {
                            this.privacy_popup.opened(window, cx);
                        } else {
                            this.privacy_popup.closed(window, cx);
                        }
                        cx.notify();
                    }))
                    .child(if self.public { PUBLIC } else { PRIVATE })
                    .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
                palette,
            )
            .edge(),
            menu,
        )
        .into_any_element()
    }
}

impl Render for EditTeamDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let (name, description) = self.values(cx);
        let can_save = self.can_save(cx) && !self.saving;
        surface("edit-team-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(TeamDialogEvent::Closed);
                }
            }))
            .child(title(format!("Edit {} team details", self.team.name)))
            .child(div().text_sm().child(
                "Collaborate closely with a group of people inside your organization based on project, initiative, or \
                 common interest.",
            ))
            .child(label("Team name", true, palette))
            .child(selected_by_full("edit-team-name", Input::new(&self.name)))
            .children(team_name_problem(&name).map(|words| problem("edit-team-name-problem", words)))
            .child(label("Description", false, palette))
            .child(selected_by_full(
                "edit-team-description",
                Textarea::new(&self.description),
            ))
            .children(
                team_description_problem(&description).map(|words| problem("edit-team-description-problem", words)),
            )
            .child(label("Privacy", false, palette))
            .child(self.privacy(palette, cx))
            .when(self.failed, |dialog| {
                dialog.child(problem(
                    "edit-team-failed",
                    "We're having trouble updating your team. Please try again.",
                ))
            })
            .child(
                buttons()
                    .child(selected_by(
                        "edit-team-cancel",
                        Button::new("edit-team-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
                    ))
                    .child(selected_by(
                        "edit-team-done",
                        Button::new("edit-team-done")
                            .primary()
                            .label("Done")
                            .disabled(!can_save)
                            .tab_stop(can_save)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )),
            )
            .focus_trap("edit-team-dialog-trap", &self.focus)
    }
}

/// What [`ConfirmDialog`] asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    HideAll,
    Archive,
    Leave,
    Delete,
}

/// "Are you sure?" for a team (see the module docs).
pub struct ConfirmDialog {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    team: Team,
    kind: Confirm,
    /// Archive's "Make the SharePoint site read-only", Delete's "I understand".
    ticked: bool,
    saving: bool,
    failed: bool,
    working: Option<Task<()>>,
    focus: FocusHandle,
}

impl EventEmitter<TeamDialogEvent> for ConfirmDialog {}

impl ConfirmDialog {
    /// The dialog on `team`.
    pub fn new(
        core: Arc<dyn Core>,
        trace: Arc<ViewTrace>,
        team: Team,
        kind: Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        trace.record(
            &TraceLine::new("team-dialog", "opened")
                .kind("confirm")
                .conversation(&team.id),
        );
        Self {
            core,
            trace,
            team,
            kind,
            ticked: false,
            saving: false,
            failed: false,
            working: None,
            focus,
        }
    }

    /// What it asks about.
    #[must_use]
    pub fn kind(&self) -> Confirm {
        self.kind
    }

    /// Ticks or unticks its box.
    pub fn set_ticked(&mut self, ticked: bool, cx: &mut Context<Self>) {
        self.ticked = ticked;
        cx.notify();
    }

    /// Whether its button is on.
    #[must_use]
    pub fn can_confirm(&self) -> bool {
        !self.saving && (self.kind != Confirm::Delete || self.ticked)
    }

    /// The button: what it asked about goes out.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_confirm() {
            return;
        }
        self.saving = true;
        self.failed = false;
        cx.notify();
        let id = self.team.id.clone();
        let group = self.team.group_id.clone().unwrap_or_default();
        let reply = match self.kind {
            Confirm::HideAll => self.core.own_team_view(
                vec![OwnTeamView::HideAll { team: id.clone() }],
                Some(TeamChange::Hidden {
                    team: id.clone(),
                    hidden: true,
                }),
            ),
            kind => {
                let (write, change) = self.write_of(kind, &id, group);
                let reply = self.core.team_write(write, Some(change));
                self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let answer = reply.await;
                    let _ = this.update(cx, |this, cx| this.done(answer.is_ok(), cx));
                }));
                return;
            }
        };
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| this.done(answer.is_ok(), cx));
        }));
    }

    fn write_of(&self, kind: Confirm, id: &str, group: String) -> (TeamWrite, TeamChange) {
        let team = id.to_owned();
        match kind {
            Confirm::Archive => (
                TeamWrite::Archive {
                    group,
                    site_read_only: self.ticked,
                },
                TeamChange::Archived { team, archived: true },
            ),
            Confirm::Leave => (
                TeamWrite::UpdateMember {
                    team: team.clone(),
                    group,
                    member: Mri::new(self.core.me()),
                    update: if self.team.owner {
                        MemberUpdate::DemotedAndLeft
                    } else {
                        MemberUpdate::Left
                    },
                },
                TeamChange::Gone(team),
            ),
            Confirm::Delete | Confirm::HideAll => (TeamWrite::Delete { team: team.clone() }, TeamChange::Gone(team)),
        }
    }

    fn done(&mut self, went: bool, cx: &mut Context<Self>) {
        self.working = None;
        self.saving = false;
        if !went {
            self.failed = true;
            cx.notify();
            return;
        }
        self.trace.record(
            &TraceLine::new("team-dialog", "done")
                .kind("confirm")
                .conversation(&self.team.id),
        );
        let team = self.team.id.clone();
        cx.emit(match self.kind {
            Confirm::HideAll => TeamDialogEvent::Changed {
                team,
                words: "We are hiding all channels",
            },
            Confirm::Archive => TeamDialogEvent::Changed {
                team,
                words: "Your team is archived",
            },
            Confirm::Leave => TeamDialogEvent::Gone {
                team,
                words: "You left the team",
            },
            Confirm::Delete => TeamDialogEvent::Gone {
                team,
                words: "The team was deleted",
            },
        });
    }

    /// Its title, its sentence, its box, its buttons' words.
    fn words(&self) -> (String, Option<String>, Option<&'static str>, &'static str, &'static str) {
        let name = &self.team.name;
        match self.kind {
            Confirm::HideAll => (
                format!("Hide all channels in {name}?"),
                Some(
                    "Channels will be removed from custom sections they're in, and if they're shared channels, hidden \
                     from teams they're shared with."
                        .to_owned(),
                ),
                None,
                "Cancel",
                "Hide",
            ),
            Confirm::Archive => (
                format!("Want to archive the {name} team?"),
                Some(
                    "This will freeze all team activity, but you'll still be able to remove members and update roles. \
                     Go to Manage teams to restore the team."
                        .to_owned(),
                ),
                Some("Make the SharePoint site read-only for team members"),
                "Cancel",
                "Archive",
            ),
            Confirm::Leave => (
                format!("Are you sure you want to leave the {name} team?"),
                None,
                None,
                "No",
                "Yes",
            ),
            Confirm::Delete => (
                format!("Delete the {name} team"),
                Some(format!(
                    "Are you sure you want to delete the {name} team? All hosted channels, conversations, files, and \
                     the Microsoft 365 group for this team will be deleted. You'll lose access to any shared channels \
                     that have been shared with the team."
                )),
                Some("I understand that everything will be deleted"),
                "Cancel",
                "Delete team",
            ),
        }
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let (heading, about, tick, cancel, confirm) = self.words();
        let danger = self.kind == Confirm::Delete;
        let ticked = self.ticked;
        surface("team-confirm-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(TeamDialogEvent::Closed);
                }
            }))
            .child(title(heading))
            .children(about.map(|about| div().text_sm().child(about)))
            .children(tick.map(|tick| {
                selected_by_full(
                    "team-confirm-tick",
                    Checkbox::new("team-confirm-tick")
                        .label(tick)
                        .checked(ticked)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| this.set_ticked(*checked, cx))),
                )
            }))
            .when(self.failed, |dialog| {
                dialog.child(problem(
                    "team-confirm-failed",
                    "We ran into a problem. Please try again.",
                ))
            })
            .child(
                buttons()
                    .child(selected_by(
                        "team-confirm-cancel",
                        Button::new("team-confirm-cancel")
                            .outline()
                            .label(cancel)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
                    ))
                    .child(selected_by("team-confirm-ok", {
                        let button = Button::new("team-confirm-ok")
                            .label(confirm)
                            .disabled(!self.can_confirm())
                            .tab_stop(self.can_confirm())
                            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)));
                        if danger { button.danger() } else { button.primary() }
                    })),
            )
            .focus_trap("team-confirm-dialog-trap", &self.focus)
    }
}

/// A tag's name, description and people (see the module docs).
pub struct TagDialog {
    core: Arc<dyn Core>,
    team: String,
    /// The tag edited; a new one otherwise.
    tag: Option<TeamTag>,
    /// Everyone on the team the tag can be given to, as (MRI, name).
    people: Vec<(String, String)>,
    picked: Vec<(String, String)>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    member_box: Entity<InputState>,
    /// The people found: Up and Down move through them.
    found_focus: FocusHandle,
    saving: bool,
    failed: bool,
    _reading: Task<()>,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _inputs: [Subscription; 3],
}

impl EventEmitter<TeamDialogEvent> for TagDialog {}

impl TagDialog {
    /// The dialog on a new tag of `team`, or on `tag`.
    pub fn new(
        core: Arc<dyn Core>,
        team: String,
        tag: Option<TeamTag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (shown_name, shown_description) = tag.as_ref().map_or((String::new(), String::new()), |tag| {
            (tag.name.clone(), tag.description.clone())
        });
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Letters, numbers, spaces and emojis are allowed")
                .default_value(shown_name)
        });
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Let people know what this tag is all about")
                .default_value(shown_description)
                .soft_wrap(true)
                .auto_grow(2, 2)
        });
        let member_box = cx.new(|cx| InputState::new(window, cx).placeholder("Enter name"));
        let touched = [
            cx.subscribe(&name, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&description, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&member_box, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        name.update(cx, |name, cx| name.focus(window, cx));
        let reply = core.team_roster(team.clone());
        let reading = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let roster = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.people = roster.into_iter().map(|member| (member.mri, member.name)).collect();
                cx.notify();
            });
        });
        Self {
            core,
            team,
            tag,
            people: Vec::new(),
            picked: Vec::new(),
            name,
            description,
            member_box,
            found_focus: cx.focus_handle(),
            saving: false,
            failed: false,
            _reading: reading,
            working: None,
            focus: cx.focus_handle(),
            _inputs: touched,
        }
    }

    /// Whether the button is on.
    #[must_use]
    pub fn can_save(&self, cx: &App) -> bool {
        let name = self.name.read(cx).value();
        let name = name.trim();
        !self.saving
            && !name.is_empty()
            && name.chars().count() <= TAG_NAME_LIMIT
            && (self.tag.is_some() || !self.picked.is_empty())
    }

    /// Sets the name and the description.
    pub fn fill(&mut self, name: &str, description: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.name
            .update(cx, |input, cx| input.set_value(name.to_owned(), window, cx));
        self.description
            .update(cx, |input, cx| input.set_value(description.to_owned(), window, cx));
        cx.notify();
    }

    /// Gives the tag to someone on the team.
    pub fn pick(&mut self, mri: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.picked.iter().any(|(picked, _)| *picked == mri) {
            self.picked.push((mri, name));
        }
        self.member_box
            .update(cx, |input, cx| input.set_value(String::new(), window, cx));
        cx.notify();
    }

    /// Create (or Save).
    pub fn save(&mut self, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }
        let name = self.name.read(cx).value().trim().to_owned();
        let description = self.description.read(cx).value().trim().to_owned();
        let team = self.team.clone();
        let write = match &self.tag {
            Some(tag) => TeamWrite::EditTag {
                team: team.clone(),
                tag: tag.id.clone(),
                version: tag.version,
                name,
                description,
            },
            None => TeamWrite::CreateTag {
                team: team.clone(),
                name,
                description,
                members: self.picked.iter().map(|(mri, _)| Mri::new(mri.as_str())).collect(),
            },
        };
        self.saving = true;
        self.failed = false;
        cx.notify();
        let reply = self.core.team_write(write, None);
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.working = None;
                this.saving = false;
                if answer.is_ok() {
                    cx.emit(TeamDialogEvent::Changed { team, words: "" });
                } else {
                    this.failed = true;
                    cx.notify();
                }
            });
        }));
    }

    fn member_step(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let typed = self.member_box.read(cx).value().trim().to_lowercase();
        let found: Vec<AnyElement> = if typed.is_empty() {
            Vec::new()
        } else {
            self.people
                .iter()
                .filter(|(_, name)| name.to_lowercase().contains(&typed))
                .take(MOST)
                .enumerate()
                .map(|(at, (mri, name))| {
                    let (mri, name, shown) = (mri.clone(), name.clone(), name.clone());
                    let row = div()
                        .id(SharedString::from(format!("tag-found-{at}")))
                        .debug_selector(move || format!("tag-found-{at}"))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(move |row| row.bg(rgb(palette.hover)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.pick(mri.clone(), name.clone(), window, cx);
                        }))
                        .child(shown);
                    keyboard::stop(SharedString::from(format!("tag-found-{at}")), row, palette).into_any_element()
                })
                .collect()
        };
        let picked: Vec<AnyElement> = self
            .picked
            .iter()
            .enumerate()
            .map(|(at, (mri, name))| {
                let mri = mri.clone();
                div()
                    .debug_selector(move || format!("tag-picked-{at}"))
                    .pl_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_md()
                    .bg(rgb(palette.hover))
                    .child(name.clone())
                    .child(
                        icon_button(
                            SharedString::from(format!("tag-unpick-{at}")),
                            Glyph::X,
                            format!("Remove {name}"),
                        )
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.picked.retain(|(picked, _)| *picked != mri);
                            cx.notify();
                        })),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(label("Enter name of a member", true, palette))
            .child(selected_by_full("tag-member-box", Input::new(&self.member_box)))
            .child(
                div()
                    .track_focus(&self.found_focus)
                    .on_key_down(keyboard::list_keys(self.found_focus.clone()))
                    .flex()
                    .flex_col()
                    .children(found),
            )
            .child(div().flex().flex_wrap().gap_1().children(picked))
    }
}

impl Render for TagDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let used = self.name.read(cx).value().chars().count();
        let editing = self.tag.is_some();
        let members = (!editing).then(|| self.member_step(palette, cx));
        let can_save = self.can_save(cx);
        surface("tag-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(TeamDialogEvent::Closed);
                }
            }))
            .child(title(if editing { "Edit tag" } else { "Create a new tag" }))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(label("Tag name", true, palette))
                    .child(
                        div()
                            .debug_selector(|| "tag-name-count".to_owned())
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child(format!("{used}/{TAG_NAME_LIMIT}")),
                    ),
            )
            .child(selected_by_full("tag-name", Input::new(&self.name)))
            .child(label("Tag description", false, palette))
            .child(selected_by_full("tag-description", Textarea::new(&self.description)))
            .children(members)
            .when(self.failed, |dialog| {
                dialog.child(problem(
                    "tag-failed",
                    "We had trouble applying your changes. Please try again later.",
                ))
            })
            .child(
                buttons()
                    .child(selected_by(
                        "tag-cancel",
                        Button::new("tag-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
                    ))
                    .child(selected_by(
                        "tag-save",
                        Button::new("tag-save")
                            .primary()
                            .label(if editing { "Save" } else { "Create" })
                            .disabled(!can_save)
                            .tab_stop(can_save)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )),
            )
            .focus_trap("tag-dialog-trap", &self.focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_teams_words_are_checked_by_teams_rules() {
        assert_eq!(team_name_problem("Release planning"), None);
        assert_eq!(
            team_name_problem("see https://example.com"),
            Some("Links are not allowed in the team name")
        );
        assert_eq!(
            team_name_problem(&"x".repeat(TEAM_NAME_LIMIT + 1)),
            Some("Character limit reached")
        );
        assert_eq!(
            team_description_problem("www.example.com"),
            Some("Links are not allowed in the team description")
        );
    }
}
