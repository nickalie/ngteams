//! The chat switcher (Ctrl+K): the UI kit's command palette over the chat list's rows, matched fuzzily on their names
//! as the user types ([`fuzzy_score`]): Enter opens the chat highlighted, Escape closes the switcher whatever was typed
//! (while the focus is in it: an Escape elsewhere goes where the focus is), and so does Ctrl+K again.
//! It is the live test's way into a chat: the keyboard, no screen coordinates.
//!
//! It reads the chat list's rows (one model for both), so a chat that changes while it is open shows changed. Under
//! the chats come the teams' channels the Teams list shows ("<team> › <channel>", matched the same way): one chosen
//! opens read-only, as from the Teams list. Messages are found by the title bar's search ([`crate::global_search`]),
//! the app's one search, not here. Last come the app's commands the query matches ([`COMMANDS`]: "See more chats", the
//! "Chats" section's See more from the keyboard, "Copy diagnostics", and "Set status: …" for each status the account
//! menu offers, so the account's status is set from the keyboard too; "Create team" and "Join team", and for a typed query
//! "Add channel: <team>", "Add member: <team>" and "Manage team: <team>" for each team whose "..." menu offers them,
//! [`TeamCommand`]), and in a build that shows calls the call commands that fit the moment ([`CallCommand`]: "Call" the
//! chat shown, "Make a test call", "Join call", "Answer call", "Decline call", "Leave call", "Mute or unmute", M3 Task 17;
//! "Camera on or off", Task 18).

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::IndexPath;
use gpui_component::command::{Command, CommandGroup, CommandItem, CommandState};

use crate::calls::Calls;
use crate::chat_list::{ChatList, RowKind};
use crate::settings::ManualStatus;
use crate::team_menu::{self, TeamChoice};
use crate::teams_list::TeamsList;
use crate::theme;

/// The most matches the switcher lists.
pub const MAX_MATCHES: usize = 50;
/// The switcher's width, px.
pub const WIDTH: f32 = 520.0;

/// How well `query` matches `name` (higher is better), or `None` when it doesn't: every character of the query
/// (spaces left out, any case) must appear in the name in order. Characters that follow each other, ones at the start
/// of a word and a match at the name's start count more.
#[must_use]
pub fn fuzzy_score(query: &str, name: &str) -> Option<u32> {
    let wanted: Vec<char> = query
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if wanted.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    // the best score of the query so far with its last character matched at each place of the name
    let mut best: Vec<Option<u32>> = chars
        .iter()
        .enumerate()
        .map(|(at, &ch)| (ch == wanted[0]).then(|| 1 + word_start(&chars, at)))
        .collect();
    for &want in &wanted[1..] {
        best = (0..chars.len())
            .map(|at| (chars[at] == want).then(|| after(&best, at)).flatten())
            .map(|score| score.map(|(score, at)| score + 1 + word_start(&chars, at)))
            .collect();
    }
    best.into_iter().flatten().max()
}

/// What a match at `at` gains for starting a word.
fn word_start(chars: &[char], at: usize) -> u32 {
    if at == 0 || !chars[at - 1].is_alphanumeric() {
        3
    } else {
        0
    }
}

/// The best score of a match before `at`, with what following it right away gains, and `at`.
fn after(best: &[Option<u32>], at: usize) -> Option<(u32, usize)> {
    best[..at]
        .iter()
        .enumerate()
        .filter_map(|(before, score)| score.map(|score| score + if before + 1 == at { 4 } else { 0 }))
        .max()
        .map(|score| (score, at))
}

/// The rows of `rows` (`(id, name)`, the list's order) that `query` matches, the best first; equal ones keep the
/// list's order. At most [`MAX_MATCHES`].
#[must_use]
pub fn matches(query: &str, rows: &[(String, SharedString)]) -> Vec<(String, SharedString)> {
    let mut scored: Vec<(u32, usize)> = rows
        .iter()
        .enumerate()
        .filter_map(|(at, (_, name))| fuzzy_score(query, name).map(|score| (score, at)))
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored
        .into_iter()
        .take(MAX_MATCHES)
        .map(|(_, at)| rows[at].clone())
        .collect()
}

/// What the switcher tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitcherEvent {
    /// Open this chat.
    Open(String),
    /// Open this team's channel.
    OpenChannel(String),
    /// Close the switcher.
    Dismissed,
    /// Put the diagnostics on the clipboard (the "Copy diagnostics" command).
    CopyDiagnostics,
    /// Set the account's status, as the account menu does (a "Set status: …" command).
    SetStatus(ManualStatus),
    /// A call command.
    Call(CallCommand),
    /// Show more of the chats (the "See more chats" command, the "Chats" section's See more).
    SeeMoreChats,
    SeeAllTeams,
    /// "Create team", as Create team on "Your teams and channels".
    CreateTeam,
    /// "Join team", as its dropdown's Join team.
    JoinTeam,
    /// An entry of the team's "..." menu (a [`TeamCommand`]), by the team's id.
    Team(String, TeamChoice),
}

/// A command on one team, the entry of its "..." menu it stands for, offered when the menu offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamCommand {
    pub team: String,
    pub choice: TeamChoice,
    /// What the palette calls it: "<entry>: <team>".
    pub name: SharedString,
}

/// The menu's entries the palette offers on each team.
const TEAM_CHOICES: [TeamChoice; 3] = [TeamChoice::AddChannel, TeamChoice::AddMember, TeamChoice::Manage];

/// The team commands on `teams`: each of [`TEAM_CHOICES`] its "..." menu offers (as [`team_menu::entries`] decides in
/// the read-only mode or not), in the teams' order.
#[must_use]
pub fn team_commands(teams: &[teams_store::Team], read_only: bool) -> Vec<TeamCommand> {
    teams
        .iter()
        .flat_map(|team| {
            let offered = team_menu::entries(team, read_only);
            TEAM_CHOICES
                .into_iter()
                .filter(move |choice| offered.contains(choice))
                .map(move |choice| TeamCommand {
                    team: team.id.clone(),
                    choice,
                    name: format!("{}: {}", choice.label(), team.name).into(),
                })
        })
        .collect()
}

/// A call command the palette offers (in a build that shows calls), when it fits the moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallCommand {
    /// Call the chat shown.
    CallChat,
    TestCall,
    /// Join the running call of the chat shown.
    Join,
    Answer,
    Decline,
    HangUp,
    ToggleMute,
    ToggleCamera,
    ToggleCaptions,
}

impl CallCommand {
    /// What the palette calls it, and matches the query on.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::CallChat => "Call",
            Self::TestCall => "Make a test call",
            Self::Join => "Join call",
            Self::Answer => "Answer call",
            Self::Decline => "Decline call",
            Self::HangUp => "Leave call",
            Self::ToggleMute => "Mute or unmute",
            Self::ToggleCamera => "Turn camera on or off",
            Self::ToggleCaptions => "Show or hide live captions",
        }
    }

    /// The commands that fit `calls` now, `chat` the chat shown.
    #[must_use]
    pub fn fitting(calls: &Calls, chat: Option<&str>) -> Vec<Self> {
        if !calls.shows() {
            return Vec::new();
        }
        if calls.ringing().is_some() {
            return vec![Self::Answer, Self::Decline];
        }
        if calls.call().is_some() {
            let mut up = vec![Self::HangUp, Self::ToggleMute];
            if calls.call().is_some_and(|call| {
                call.status == teams_core::CallStatus::Connected && call.kind != crate::calls::CallKind::TestCall
            }) {
                up.push(Self::ToggleCaptions);
            }
            if calls.video().is_some() {
                up.push(Self::ToggleCamera);
            }
            return up;
        }
        let mut fitting = Vec::new();
        if let Some(chat) = chat {
            fitting.push(if calls.joinable(chat) {
                Self::Join
            } else {
                Self::CallChat
            });
        }
        if calls.has_test_call() {
            fitting.push(Self::TestCall);
        }
        fitting
    }
}

/// The palette's sections in order (an empty one is hidden, and keeps its number).
const CHATS: usize = 0;
const CHANNELS: usize = 1;
const COMMAND_SECTION: usize = 2;

/// One of the app's commands the palette offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteCommand {
    SeeMoreChats,
    SeeAllTeams,
    CreateTeam,
    JoinTeam,
    CopyDiagnostics,
    SetStatus(ManualStatus),
    Call(CallCommand),
}

impl PaletteCommand {
    /// What the palette calls it, and matches the query on.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::SeeMoreChats => "See more chats",
            Self::SeeAllTeams => "See all your teams",
            Self::CreateTeam => "Create team",
            Self::JoinTeam => "Join team",
            Self::CopyDiagnostics => "Copy diagnostics",
            Self::SetStatus(ManualStatus::Automatic) => "Set status: Automatic",
            Self::SetStatus(ManualStatus::Available) => "Set status: Available",
            Self::SetStatus(ManualStatus::Busy) => "Set status: Busy",
            Self::SetStatus(ManualStatus::DoNotDisturb) => "Set status: Do not disturb",
            Self::SetStatus(ManualStatus::Away) => "Set status: Appear away",
            Self::Call(command) => command.name(),
        }
    }

    fn event(self) -> SwitcherEvent {
        match self {
            Self::SeeMoreChats => SwitcherEvent::SeeMoreChats,
            Self::SeeAllTeams => SwitcherEvent::SeeAllTeams,
            Self::CreateTeam => SwitcherEvent::CreateTeam,
            Self::JoinTeam => SwitcherEvent::JoinTeam,
            Self::CopyDiagnostics => SwitcherEvent::CopyDiagnostics,
            Self::SetStatus(status) => SwitcherEvent::SetStatus(status),
            Self::Call(command) => SwitcherEvent::Call(command),
        }
    }
}

/// The commands the palette offers, in order, matched on their names as the chats are.
pub const COMMANDS: [PaletteCommand; 10] = [
    PaletteCommand::SeeMoreChats,
    PaletteCommand::SeeAllTeams,
    PaletteCommand::CreateTeam,
    PaletteCommand::JoinTeam,
    PaletteCommand::CopyDiagnostics,
    PaletteCommand::SetStatus(ManualStatus::Automatic),
    PaletteCommand::SetStatus(ManualStatus::Available),
    PaletteCommand::SetStatus(ManualStatus::Busy),
    PaletteCommand::SetStatus(ManualStatus::DoNotDisturb),
    PaletteCommand::SetStatus(ManualStatus::Away),
];

/// The switcher (see the module docs).
pub struct Switcher {
    state: Entity<CommandState>,
    list: Entity<ChatList>,
    teams: Entity<TeamsList>,
    query: String,
    matches: Vec<(String, SharedString)>,
    /// The channels the query matches, the best first: `(id, "<team> › <channel>")`.
    channels: Vec<(String, SharedString)>,
    /// The call commands that fit the moment, first of the commands.
    call_commands: Vec<CallCommand>,
    clear_selection: bool,
    /// The team commands the query matches, the best first, after the app's.
    team_commands: Vec<TeamCommand>,
    /// The read-only mode: nothing that writes is offered.
    read_only: bool,
    _observing: [Subscription; 3],
    /// Tracked on the dialog's root: the focus is in it while its query field or the palette's list has it.
    focus: FocusHandle,
    /// Escape, before the palette's own binding (which first clears the query).
    _escape: Subscription,
}

impl EventEmitter<SwitcherEvent> for Switcher {}

impl Switcher {
    /// The switcher over the chat list's rows and the Teams list's channels; with the call commands that fit `calls`
    /// now, `chat` the chat shown.
    pub fn new(
        list: Entity<ChatList>,
        teams: Entity<TeamsList>,
        (calls, chat): (Entity<Calls>, Option<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let fitting = move |calls: &Calls, list: &ChatList| {
            CallCommand::fitting(calls, chat.as_deref())
                .into_iter()
                .filter(|command| {
                    *command != CallCommand::CallChat || chat.as_ref().is_some_and(|id| list.can_call(id))
                })
                .collect()
        };
        let call_commands = fitting(calls.read(cx), list.read(cx));
        let list_fitting = fitting.clone();
        let list_calls = calls.clone();
        let calls_list = list.clone();
        let observing = [
            cx.observe(&list, move |this: &mut Self, list, cx| {
                this.set_call_commands(list_fitting(list_calls.read(cx), list.read(cx)));
                this.refilter(cx);
            }),
            cx.observe(&teams, |this: &mut Self, _, cx| this.refilter(cx)),
            cx.observe(&calls, move |this: &mut Self, calls, cx| {
                this.set_call_commands(fitting(calls.read(cx), calls_list.read(cx)));
                this.refilter(cx);
            }),
        ];
        let this = cx.weak_entity();
        // the interceptor sees every keystroke of the app: an Escape is the switcher's only while the focus is in it (a
        // switcher left open in a shell not shown, another account's, or with the focus moved out of it, never takes
        // the Escape meant for where the focus is)
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key != "escape" || keystroke.modifiers.number_of_modifiers() != 0 {
                return;
            }
            let dismissed = this.update(cx, |this, cx| {
                let held = this.holds_focus(window, cx);
                if held {
                    cx.emit(SwitcherEvent::Dismissed);
                }
                held
            });
            if matches!(dismissed, Ok(true)) {
                cx.stop_propagation();
            }
        });
        let read_only = list.read(cx).read_only();
        let mut switcher = Self {
            state,
            list,
            teams,
            query: String::new(),
            matches: Vec::new(),
            channels: Vec::new(),
            call_commands,
            clear_selection: false,
            team_commands: Vec::new(),
            read_only,
            _observing: observing,
            focus: cx.focus_handle(),
            _escape: escape,
        };
        switcher.refilter(cx);
        switcher
    }

    /// Moves the keyboard focus to the query field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.state.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Whether the focus is in the switcher (its query field, or its list once clicked): the focus may have moved
    /// elsewhere since it opened. Read from the last frame, which GPUI draws before it dispatches a key; one not drawn
    /// (in a shell not shown) never holds it.
    #[must_use]
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// The chats listed now, the best match first: `(id, name)`.
    #[must_use]
    pub fn matches(&self) -> &[(String, SharedString)] {
        &self.matches
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The channels listed now, the best match first: `(id, "<team> › <channel>")`.
    #[must_use]
    pub fn channels(&self) -> &[(String, SharedString)] {
        &self.channels
    }

    /// The names of the commands the query matches ([`COMMANDS`], then the team commands), listed last.
    #[must_use]
    pub fn commands(&self) -> Vec<SharedString> {
        self.matching_commands()
            .into_iter()
            .map(|command| SharedString::from(command.name()))
            .chain(self.team_commands.iter().map(|command| command.name.clone()))
            .collect()
    }

    fn matching_commands(&self) -> Vec<PaletteCommand> {
        // the read-only mode creates and joins no team
        let writes = !self.read_only;
        self.call_commands
            .iter()
            .copied()
            .map(PaletteCommand::Call)
            .chain(COMMANDS)
            .filter(|command| writes || !matches!(command, PaletteCommand::CreateTeam | PaletteCommand::JoinTeam))
            .filter(|command| fuzzy_score(&self.query, command.name()).is_some())
            .collect()
    }

    fn set_call_commands(&mut self, commands: Vec<CallCommand>) {
        if self.call_commands != commands {
            self.call_commands = commands;
            self.clear_selection = true;
        }
    }

    fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        query.clone_into(&mut self.query);
        self.refilter(cx);
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<(String, SharedString)> = self
            .list
            .read(cx)
            .findable_rows()
            // a favourite channel is offered with the other channels, below
            .filter(|row| row.kind != RowKind::Channel)
            .map(|row| (row.id.clone(), row.name.clone()))
            .collect();
        self.matches = matches(&self.query, &rows);
        let channels: Vec<(String, SharedString)> = self
            .teams
            .read(cx)
            .teams()
            .unwrap_or_default()
            .iter()
            .flat_map(|team| {
                team.channels
                    .iter()
                    .map(move |channel| (channel.id.clone(), format!("{} › {}", team.name, channel.name).into()))
            })
            .collect();
        self.channels = matches(&self.query, &channels);
        // a command per team and entry: offered once something is typed, so an empty query isn't all of them
        self.team_commands = if self.query.trim().is_empty() {
            Vec::new()
        } else {
            let all = team_commands(self.teams.read(cx).teams().unwrap_or_default(), self.read_only);
            let named: Vec<(String, SharedString)> = all
                .iter()
                .enumerate()
                .map(|(at, command)| (at.to_string(), command.name.clone()))
                .collect();
            matches(&self.query, &named)
                .into_iter()
                .filter_map(|(at, _)| at.parse::<usize>().ok().and_then(|at| all.get(at).cloned()))
                .collect()
        };
        cx.notify();
    }

    fn confirm(&mut self, index: IndexPath, cx: &mut Context<Self>) {
        let event = match index.section {
            CHATS => self
                .matches
                .get(index.row)
                .map(|(id, _)| SwitcherEvent::Open(id.clone())),
            CHANNELS => self
                .channels
                .get(index.row)
                .map(|(id, _)| SwitcherEvent::OpenChannel(id.clone())),
            COMMAND_SECTION => {
                let app = self.matching_commands();
                app.get(index.row).map(|command| command.event()).or_else(|| {
                    self.team_commands
                        .get(index.row - app.len())
                        .map(|command| SwitcherEvent::Team(command.team.clone(), command.choice))
                })
            }
            _ => None,
        };
        if let Some(event) = event {
            cx.emit(event);
        }
    }
}

impl Render for Switcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.clear_selection) {
            self.state
                .update(cx, |state, cx| state.set_selected_index(None, window, cx));
        }
        let palette = theme::current(cx);
        let on_query = cx.weak_entity();
        let on_confirm = cx.weak_entity();
        let chats = self
            .matches
            .iter()
            .map(|(_, name)| CommandItem::new().label(name.clone()));
        // the sections in order (an empty one is hidden, and keeps its number)
        let command = Command::new(&self.state)
            .filterable(false)
            .placeholder("Go to a chat or a channel, or run a command")
            .group(CommandGroup::new().label("Chats").items(chats))
            .group(
                CommandGroup::new().label("Channels").items(
                    self.channels
                        .iter()
                        .map(|(_, name)| CommandItem::new().label(name.clone())),
                ),
            )
            .group(
                CommandGroup::new()
                    .label("Commands")
                    .items(self.commands().into_iter().map(|name| CommandItem::new().label(name))),
            )
            .on_query(move |query, _, cx| {
                // a switcher already closed has nothing to filter
                let _ = on_query.update(cx, |this, cx| this.set_query(query, cx));
            })
            .on_confirm(move |index, _, cx| {
                let _ = on_confirm.update(cx, |this, cx| this.confirm(index, cx));
            })
            .empty(|_, _, _| div().p_2().child("No chat, channel or command matches"));
        div()
            .debug_selector(|| "switcher".to_owned())
            .track_focus(&self.focus)
            .w(px(WIDTH))
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(command)
    }
}
