//! Manage team, as Teams has it (`docs/protocol/teams.md`): a team's header with its tabs Channels, Members, Pending
//! requests and Settings (an owner's), Analytics, Apps and Tags.
//!
//! - **Channels**: each with Show or Hide (the account's own list), the filters, the sort, Search channels and Add channel.
//! - **Members**: Owners and Members and guests, folding; Add member and Search members; an owner changes roles and removes
//!   people.
//! - **Pending requests**: who asks to join, Accept and Deny.
//! - **Settings**: Team details with Edit and its picture; Member permissions, Guest permissions, @mentions, Team code, Fun
//!   stuff (with its Giphy rating) and Tags, folding as Teams folds them, each box writing the whole of the team's
//!   settings as the web client does; Change picture.
//! - **Analytics**: a channel or all of them, the time period's summary, engagement and active users by day.
//! - **Apps**: the team's apps, Find by name, Get more apps and Remove (`apps`).
//! - **Tags**: its tags, Create tag, Edit and Delete.
//!
//! What a tab shows is read when it opens, through [`Core`]; nothing here sends anything but through it, on the user's own
//! press. The read-only mode shows it all and offers no write. Traced: `manage-team opened`, `tab` with its key.

use std::collections::HashSet;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, ClickEvent, ClipboardItem, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use serde_json::Value;
use teams_core::{TeamChange, TeamMember};
use teams_proto::Mri;
use teams_proto::{
    AnalyticsPeriod, InstalledApp, ManagedChannel, MemberUpdate, OwnTeamView, TagsRead, TeamAnalytics, TeamTag,
    TeamWrite,
};
use teams_store::Team;

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard::{self, Handles, PopupFocus};
use crate::shell::menu_row;
use crate::team_menu::TeamChoice;
use crate::teams_list::TeamsList;
use crate::teams_page::{centered, chip_base, popup_menu, tab_button, with_popup};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

mod apps;
mod webhooks;

pub(crate) use apps::app_tile;
pub use apps::{GIPHY_RATINGS, giphy_rating, with_giphy_rating};

/// The page's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ManageTab {
    #[default]
    Channels,
    Members,
    PendingRequests,
    Settings,
    Analytics,
    Apps,
    Tags,
}

impl ManageTab {
    /// Its test selector's and the trace's name.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Channels => "channels",
            Self::Members => "members",
            Self::PendingRequests => "pending-requests",
            Self::Settings => "settings",
            Self::Analytics => "analytics",
            Self::Apps => "apps",
            Self::Tags => "tags",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Channels => "Channels",
            Self::Members => "Members",
            Self::PendingRequests => "Pending requests",
            Self::Settings => "Settings",
            Self::Analytics => "Analytics",
            Self::Apps => "Apps",
            Self::Tags => "Tags",
        }
    }
}

/// The tabs `team` offers the account: an owner's as well as everyone's.
#[must_use]
pub fn tabs_of(team: &Team) -> Vec<ManageTab> {
    let mut tabs = vec![ManageTab::Channels, ManageTab::Members];
    if team.owner {
        tabs.extend([ManageTab::PendingRequests, ManageTab::Settings]);
    }
    tabs.extend([ManageTab::Analytics, ManageTab::Apps, ManageTab::Tags]);
    tabs
}

/// What the page tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManageTeamEvent {
    /// Something a dialog does: an entry of the team's menu.
    Chosen(Team, TeamChoice),
    /// The picture's Change picture.
    EditDetails(Team),
    /// Create tag, or a tag's Edit.
    EditTag(String, Option<TeamTag>),
    /// A channel to open.
    OpenChannel(String),
    /// A toast.
    Toast(&'static str),
    /// Team details' Change picture.
    ChangePicture(Team),
    /// The Apps tab's "Get more apps".
    AddApp(Team),
    /// The Apps tab's "Create an outgoing webhook", or a webhook's Edit.
    Webhook(Team, Option<Box<teams_proto::OutgoingWebhook>>),
}

impl EventEmitter<ManageTeamEvent> for ManageTeam {}

/// Which channels the Channels tab lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChannelFilter {
    #[default]
    All,
    Shown,
    Hidden,
    Private,
    Archived,
}

impl ChannelFilter {
    fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Shown => "shown",
            Self::Hidden => "hidden",
            Self::Private => "private",
            Self::Archived => "archived",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Shown => "Shown for you",
            Self::Hidden => "Hidden for you",
            Self::Private => "Private",
            Self::Archived => "Archived",
        }
    }
}

/// A settings box: its section, what it says, and the keys of the team's settings it stands for.
struct SettingBox {
    selector: &'static str,
    words: &'static str,
    guests: bool,
    keys: &'static [&'static str],
}

/// The settings' sections that are boxes of the team's settings, as Teams lists them.
const MEMBER_BOXES: [SettingBox; 9] = [
    SettingBox {
        selector: "create-update-channel",
        words: "Allow members to create and update channels",
        guests: false,
        keys: &["createTopic", "updateTopic"],
    },
    SettingBox {
        selector: "create-private-channel",
        words: "Allow members to create private channels",
        guests: false,
        keys: &["createPrivateSpace"],
    },
    SettingBox {
        selector: "delete-channel",
        words: "Allow members to delete and restore channels",
        guests: false,
        keys: &["deleteTopic"],
    },
    SettingBox {
        selector: "install-uninstall-apps",
        words: "Allow members to add and remove apps",
        guests: false,
        keys: &["installApp", "uninstallApp"],
    },
    SettingBox {
        selector: "upload-custom-app",
        words: "Allow members to upload custom apps",
        guests: false,
        keys: &["uploadCustomApp"],
    },
    SettingBox {
        selector: "create-delete-tab",
        words: "Allow members to create, update, and remove tabs",
        guests: false,
        keys: &["createTab", "deleteTab"],
    },
    SettingBox {
        selector: "update-connectors",
        words: "Allow members to create, update, and remove connectors",
        guests: false,
        keys: &["createIntegration", "updateIntegration", "deleteIntegration"],
    },
    SettingBox {
        selector: "delete-messages-enabled",
        words: "Give members the option to delete their messages",
        guests: false,
        keys: &["deleteEnabled"],
    },
    SettingBox {
        selector: "edit-messages-enabled",
        words: "Give members the option to edit their messages",
        guests: false,
        keys: &["editEnabled"],
    },
];

const GUEST_BOXES: [SettingBox; 2] = [
    SettingBox {
        selector: "create-update-channel-guest-permission",
        words: "Allow guests to create and update channels",
        guests: true,
        keys: &["createTopic", "updateTopic"],
    },
    SettingBox {
        selector: "delete-channel-guest-permission",
        words: "Allow guests to delete channels",
        guests: true,
        keys: &["deleteTopic"],
    },
];

const MENTION_BOXES: [SettingBox; 2] = [
    SettingBox {
        selector: "mention-settings-team-mention",
        words: "Show members the option to @team or @[team name] (this will send a notification to everyone on the team)",
        guests: false,
        keys: &["teamMention"],
    },
    SettingBox {
        selector: "mention-settings-channel-mention",
        words: "Give members the option to @channel or @[channel name]. This will notify everyone who's shown the \
                mentioned channel in their channel lists.",
        guests: false,
        keys: &["channelMention"],
    },
];

const FUN_BOXES: [SettingBox; 3] = [
    SettingBox {
        selector: "giphy-enabled",
        words: "Enable Giphy for this team",
        guests: false,
        keys: &["giphyEnabled"],
    },
    SettingBox {
        selector: "stickers-enabled",
        words: "Enable stickers and memes",
        guests: false,
        keys: &["stickersEnabled"],
    },
    SettingBox {
        selector: "custom-memes-enabled",
        words: "Allow memes to be uploaded",
        guests: false,
        keys: &["customMemesEnabled"],
    },
];

/// Whether `settings` has every one of `keys` on, under `userSettings` or `guestSettings`.
#[must_use]
pub fn setting_on(settings: &Value, guests: bool, keys: &[&str]) -> bool {
    let holder = settings.get(if guests { "guestSettings" } else { "userSettings" });
    keys.iter().all(|key| {
        holder
            .and_then(|holder| holder.get(*key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
}

/// `settings` with `keys` set to `on`.
#[must_use]
pub fn with_setting(mut settings: Value, guests: bool, keys: &[&str], on: bool) -> Value {
    let holder = if guests { "guestSettings" } else { "userSettings" };
    if !settings.is_object() {
        settings = Value::Object(serde_json::Map::new());
    }
    if settings.get(holder).is_none_or(|holder| !holder.is_object()) {
        settings[holder] = Value::Object(serde_json::Map::new());
    }
    for key in keys {
        settings[holder][*key] = Value::Bool(on);
    }
    settings
}

/// Manage team (see the module docs).
#[expect(
    clippy::struct_excessive_bools,
    reason = "a page's folds, popups and what it has read: each stands alone, no state machine"
)]
pub struct ManageTeam {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    teams: Entity<TeamsList>,
    team: String,
    tab: ManageTab,
    // Channels
    channels: Option<Vec<ManagedChannel>>,
    channel_filter: ChannelFilter,
    channel_more: bool,
    channel_z_to_a: bool,
    channel_search: Option<Entity<InputState>>,
    channel_query: String,
    // Members
    members: Option<Vec<TeamMember>>,
    member_search: Option<Entity<InputState>>,
    member_query: String,
    owners_open: bool,
    others_open: bool,
    role_menu: Option<String>,
    // Requests
    requests: Option<Vec<TeamMember>>,
    // Settings
    settings: Option<Value>,
    /// A read of the settings is under way: none is changed until it answered ([`Self::set_setting`]).
    settings_reading: bool,
    open_sections: HashSet<&'static str>,
    code: Option<String>,
    code_read: bool,
    // Analytics
    analytics: Option<TeamAnalytics>,
    analytics_read: bool,
    period: AnalyticsPeriod,
    period_open: bool,
    // Apps
    apps: Option<Vec<InstalledApp>>,
    app_find: Option<Entity<InputState>>,
    app_query: String,
    app_menu: Option<String>,
    removing_app: Option<InstalledApp>,
    // The team's outgoing webhooks, each with its face
    webhooks: Option<Vec<webhooks::ShownWebhook>>,
    webhook_menu: Option<String>,
    removing_webhook: Option<teams_proto::OutgoingWebhook>,
    // Analytics of one channel
    analytics_channel: Option<(String, String)>,
    channel_menu_open: bool,
    giphy_open: bool,
    // Tags
    tags: Option<TagsRead>,
    /// What went wrong with the last write, in Teams' words.
    problem: Option<&'static str>,
    /// The tabs, Left and Right moving through them.
    tabs: Handles,
    /// The keyboard on the open popup ("N more", a role, the time period).
    popup: PopupFocus,
    reading: Vec<Task<()>>,
    writing: Option<Task<()>>,
    observing_: Vec<Subscription>,
}

impl ManageTeam {
    /// The page on the team `team`, on `tab`.
    pub fn new(
        core: Arc<dyn Core>,
        trace: Arc<ViewTrace>,
        teams: Entity<TeamsList>,
        team: String,
        tab: ManageTab,
        cx: &mut Context<Self>,
    ) -> Self {
        let observing = vec![cx.observe(&teams, |_, _, cx| cx.notify())];
        trace.record(&TraceLine::new("manage-team", "opened").conversation(&team));
        let mut page = Self {
            core,
            trace,
            teams,
            team,
            tab,
            channels: None,
            channel_filter: ChannelFilter::default(),
            channel_more: false,
            channel_z_to_a: false,
            channel_search: None,
            channel_query: String::new(),
            members: None,
            member_search: None,
            member_query: String::new(),
            owners_open: true,
            others_open: true,
            role_menu: None,
            requests: None,
            settings: None,
            settings_reading: false,
            open_sections: HashSet::from(["details"]),
            code: None,
            code_read: false,
            analytics: None,
            analytics_read: false,
            period: AnalyticsPeriod::default(),
            period_open: false,
            apps: None,
            app_find: None,
            app_query: String::new(),
            app_menu: None,
            removing_app: None,
            webhooks: None,
            webhook_menu: None,
            removing_webhook: None,
            analytics_channel: None,
            channel_menu_open: false,
            giphy_open: false,
            tags: None,
            problem: None,
            tabs: Handles::default(),
            popup: PopupFocus::new(cx),
            reading: Vec::new(),
            writing: None,
            observing_: observing,
        };
        page.read_tab(cx);
        page
    }

    /// The team it is on.
    #[must_use]
    pub fn team_id(&self) -> &str {
        &self.team
    }

    /// The tab shown.
    #[must_use]
    pub fn tab(&self) -> ManageTab {
        self.tab
    }

    fn team_of(&self, cx: &App) -> Option<Team> {
        self.teams
            .read(cx)
            .teams()?
            .iter()
            .find(|team| team.id == self.team)
            .cloned()
    }

    /// Shows `tab`, reading what it shows.
    pub fn show_tab(&mut self, tab: ManageTab, cx: &mut Context<Self>) {
        if tab == ManageTab::Apps && !self.apps_shown_at_all() {
            return;
        }
        self.tab = tab;
        self.problem = None;
        self.trace.record(&TraceLine::new("manage-team", "tab").kind(tab.key()));
        self.read_tab(cx);
        cx.notify();
    }

    /// Reads again what the tab shows: a dialog changed something.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.read_tab(cx);
        cx.notify();
    }

    fn read_tab(&mut self, cx: &mut Context<Self>) {
        let group = self.team_of(cx).and_then(|team| team.group_id).unwrap_or_default();
        let team = self.team.clone();
        match self.tab {
            ManageTab::Channels => {
                let reply = self.core.team_channels(group);
                self.reading
                    .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                        let channels = reply.await.unwrap_or_default();
                        let _ = this.update(cx, |this, cx| {
                            this.channels = Some(channels);
                            cx.notify();
                        });
                    }));
            }
            ManageTab::Members => {
                let reply = self.core.team_roster(team);
                self.reading
                    .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                        let members = reply.await.unwrap_or_default();
                        let _ = this.update(cx, |this, cx| {
                            this.members = Some(members);
                            cx.notify();
                        });
                    }));
            }
            ManageTab::PendingRequests => {
                let reply = self.core.team_join_requests(group);
                self.reading
                    .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                        let requests = reply.await.unwrap_or_default();
                        let _ = this.update(cx, |this, cx| {
                            this.requests = Some(requests);
                            cx.notify();
                        });
                    }));
            }
            ManageTab::Settings => self.read_settings(cx),
            ManageTab::Analytics => self.read_analytics(cx),
            ManageTab::Apps => {
                self.read_apps(cx);
                self.read_webhooks(cx);
            }
            ManageTab::Tags => self.read_tags(cx),
        }
    }

    fn read_settings(&mut self, cx: &mut Context<Self>) {
        let settings = self.core.team_settings(self.team.clone());
        self.settings_reading = true;
        self.reading
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let settings = settings.await.ok().flatten();
                let _ = this.update(cx, |this, cx| {
                    this.settings_reading = false;
                    let read = settings.and_then(|settings| serde_json::from_str::<Value>(&settings).ok());
                    if !read.as_ref().is_some_and(Value::is_object) {
                        // shown unchecked, and never written back as a snapshot of one box ([`Self::set_setting`])
                        this.problem = Some("We couldn't load your team's settings. Please try again.");
                    }
                    this.settings = Some(read.unwrap_or(Value::Null));
                    cx.notify();
                });
            }));
        let code = self.core.team_code(self.team.clone());
        self.reading
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let code = code.await.ok().flatten();
                let _ = this.update(cx, |this, cx| {
                    this.code = code;
                    this.code_read = true;
                    cx.notify();
                });
            }));
        self.read_tags(cx);
    }

    fn read_analytics(&mut self, cx: &mut Context<Self>) {
        let group = self.team_of(cx).and_then(|team| team.group_id).unwrap_or_default();
        self.analytics_read = false;
        let reply = match self.analytics_channel.clone() {
            Some((channel, _)) => self.core.channel_analytics(group, channel, self.period),
            None => self.core.team_analytics(group, self.period),
        };
        self.reading
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let analytics = reply.await.ok().flatten();
                let _ = this.update(cx, |this, cx| {
                    this.analytics = analytics;
                    this.analytics_read = true;
                    cx.notify();
                });
            }));
    }

    fn read_tags(&mut self, cx: &mut Context<Self>) {
        let reply = self.core.team_tags(self.team.clone());
        self.reading
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let tags = reply.await.unwrap_or_default();
                let _ = this.update(cx, |this, cx| {
                    this.tags = Some(tags);
                    cx.notify();
                });
            }));
    }

    /// Sends `write`, then reads the tab again; what goes wrong shows on the page, in `words`.
    fn write(&mut self, write: TeamWrite, words: &'static str, cx: &mut Context<Self>) {
        self.problem = None;
        let reply = self.core.team_write(write, None);
        self.writing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.writing = None;
                if answer.is_err() {
                    this.problem = Some(words);
                }
                this.read_tab(cx);
                cx.notify();
            });
        }));
    }

    /// A box of the team's settings: the whole of them goes out, as the web client sends them. Since they replace the
    /// team's settings whole, nothing goes while another write or a read of them is under way (an older snapshot
    /// answered late would undo a newer one) or when they couldn't be read (a snapshot of only this box would wipe the
    /// rest).
    pub fn set_setting(&mut self, guests: bool, keys: &'static [&'static str], on: bool, cx: &mut Context<Self>) {
        if !self.settings_writable() {
            return;
        }
        let Some(settings) = self.settings.clone() else {
            return;
        };
        let changed = with_setting(settings, guests, keys, on);
        self.settings = Some(changed.clone());
        cx.notify();
        self.write(
            TeamWrite::Settings {
                team: self.team.clone(),
                settings: changed.to_string(),
            },
            "We couldn't update your team's settings. Please try again.",
            cx,
        );
    }

    /// Who may change tags, and whether apps apply them.
    pub fn set_tag_settings(&mut self, owners_only: bool, automatic: bool, cx: &mut Context<Self>) {
        if let Some(tags) = &mut self.tags {
            tags.owners_only = owners_only;
            tags.automatic = automatic;
        }
        cx.notify();
        self.write(
            TeamWrite::TagSettings {
                team: self.team.clone(),
                owners_only,
                automatic,
            },
            "We ran into a problem getting tags setting",
            cx,
        );
    }

    /// Shows or hides a channel in the account's own list.
    pub fn set_channel_shown(&mut self, channel: &str, general: bool, shown: bool, cx: &mut Context<Self>) {
        let requests = vec![
            OwnTeamView::ChannelInterest {
                channel: channel.to_owned(),
                own: self.core.me().to_owned(),
                shown,
            },
            OwnTeamView::ChannelFavorite {
                channel: channel.to_owned(),
                general,
                shown,
            },
        ];
        let change = TeamChange::ChannelShown {
            team: self.team.clone(),
            channel: channel.to_owned(),
            shown,
        };
        let reply = self.core.own_team_view(requests, Some(change));
        self.writing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.writing = None;
                if answer.is_err() {
                    this.problem = Some("We couldn't update your channel list. Please try again.");
                }
                cx.notify();
            });
        }));
    }

    fn update_member(&mut self, mri: &str, update: MemberUpdate, cx: &mut Context<Self>) {
        let Some(group) = self.team_of(cx).and_then(|team| team.group_id) else {
            return;
        };
        self.role_menu = None;
        if let Some(members) = &mut self.members {
            match update {
                MemberUpdate::Removed => members.retain(|member| member.mri != mri),
                MemberUpdate::Promoted | MemberUpdate::Demoted => {
                    if let Some(member) = members.iter_mut().find(|member| member.mri == mri) {
                        member.owner = update == MemberUpdate::Promoted;
                    }
                }
                MemberUpdate::Left | MemberUpdate::DemotedAndLeft => {}
            }
        }
        self.write(
            TeamWrite::UpdateMember {
                team: self.team.clone(),
                group,
                member: Mri::new(mri),
                update,
            },
            "We couldn't change that person's role. Please try again.",
            cx,
        );
    }

    fn answer_request(&mut self, mri: &str, accept: bool, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        let Some(group) = self.team_of(cx).and_then(|team| team.group_id) else {
            return;
        };
        if let Some(requests) = &mut self.requests {
            requests.retain(|request| request.mri != mri);
        }
        self.write(
            TeamWrite::AnswerJoinRequest {
                group,
                member: Mri::new(mri),
                accept,
            },
            "We couldn't answer that request. Please try again.",
            cx,
        );
    }

    fn write_code(&mut self, remove: bool, cx: &mut Context<Self>) {
        let team = self.team.clone();
        let write = if remove {
            self.code = None;
            TeamWrite::RemoveCode { team }
        } else {
            TeamWrite::ResetCode { team }
        };
        self.write(write, "Couldn't generate a code right now. Try again.", cx);
    }

    fn channel_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(search) = &self.channel_search {
            return search.clone();
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search channels"));
        let typed = cx.subscribe(&search, |this: &mut Self, search, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.channel_query = search.read(cx).value().to_lowercase();
                cx.notify();
            }
        });
        self.observing_.push(typed);
        self.channel_search = Some(search.clone());
        search
    }

    fn member_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(search) = &self.member_search {
            return search.clone();
        }
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search members"));
        let typed = cx.subscribe(&search, |this: &mut Self, search, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.member_query = search.read(cx).value().to_lowercase();
                cx.notify();
            }
        });
        self.observing_.push(typed);
        self.member_search = Some(search.clone());
        search
    }

    fn header(&self, team: &Team, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let avatar = self.teams.read(cx).avatar(team, 32.0, false, palette);
        let mut shown = tabs_of(team);
        if !self.apps_shown_at_all() {
            shown.retain(|tab| *tab != ManageTab::Apps);
        }
        let tabs: Vec<keyboard::Stop> = shown
            .iter()
            .copied()
            .map(|tab| {
                let key = format!("manage-tab-{}", tab.key());
                let chosen = self.tab == tab;
                let order = shown.clone();
                keyboard::stop(
                    key.clone(),
                    tab_button(key.clone(), tab.label(), chosen, palette, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.show_tab(tab, cx)))
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            this.tab_key(&order, tab, event, window, cx);
                        })),
                    palette,
                )
                .handle(&self.tabs.get(&key, cx))
                .tab_stop(chosen)
            })
            .collect();
        div()
            .h(px(60.0))
            .flex_none()
            .px_5()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(avatar)
            .child(
                div()
                    .debug_selector(|| "manage-team-name".to_owned())
                    .flex_none()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(team.name.clone()),
            )
            .children(tabs)
    }

    /// Left and Right on a tab: the one next to it is shown, the keyboard on it, as Teams' tabs take them.
    fn tab_key(
        &mut self,
        order: &[ManageTab],
        tab: ManageTab,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = order.iter().position(|shown| *shown == tab).unwrap_or(0);
        let Some(to) = keyboard::step(event, order.len(), at) else {
            return;
        };
        cx.stop_propagation();
        let tab = order[to];
        self.show_tab(tab, cx);
        self.tabs
            .get(&format!("manage-tab-{}", tab.key()), cx)
            .focus(window, cx);
    }

    /// Opens or closes a popup, the keyboard going to it or back to what opened it.
    fn popup_toggled(&self, open: bool, window: &mut Window, cx: &mut App) {
        if open {
            self.popup.opened(window, cx);
        } else {
            self.popup.closed(window, cx);
        }
    }

    /// Closes whichever popup is open, the keyboard back on what opened it.
    fn close_popups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.channel_more = false;
        self.role_menu = None;
        self.period_open = false;
        self.app_menu = None;
        self.channel_menu_open = false;
        self.giphy_open = false;
        self.popup.closed(window, cx);
        cx.notify();
    }

    /// The open popup's keys.
    fn popup_keys(&self, cx: &mut Context<Self>) -> impl Fn(&KeyDownEvent, &mut Window, &mut App) + 'static {
        let weak = cx.entity().downgrade();
        keyboard::menu_keys(self.popup.root().clone(), move |window, cx| {
            let _ = weak.update(cx, |this, cx| this.close_popups(window, cx));
        })
    }

    /// The Channels tab's filter chips, the words of its "N more" and that one's menu.
    fn channel_chips(
        &self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<keyboard::Stop>, String, Option<crate::teams_page::PopupMenu>) {
        let others = [ChannelFilter::Hidden, ChannelFilter::Private, ChannelFilter::Archived];
        let left: Vec<ChannelFilter> = others
            .into_iter()
            .filter(|filter| *filter != self.channel_filter)
            .collect();
        let chip = |filter: ChannelFilter, this: &Self, cx: &mut Context<Self>| {
            let key = format!("manage-channels-{}", filter.key());
            keyboard::stop(
                key.clone(),
                chip_base(key, this.channel_filter == filter, palette)
                    .child(filter.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.channel_filter = filter;
                        this.channel_more = false;
                        cx.notify();
                    })),
                palette,
            )
            .edge()
        };
        let mut chips = vec![chip(ChannelFilter::All, self, cx), chip(ChannelFilter::Shown, self, cx)];
        if others.contains(&self.channel_filter) {
            chips.push(chip(self.channel_filter, self, cx));
        }
        let more_menu = self.channel_more.then(|| {
            let mut menu = popup_menu(
                palette,
                cx.listener(|this, _, _, cx| {
                    this.channel_more = false;
                    cx.notify();
                }),
            )
            .keyed(self.popup.root(), self.popup_keys(cx));
            for filter in left.iter().copied() {
                let key = format!("manage-channels-more-{}", filter.key());
                menu = menu.row(
                    key.clone(),
                    menu_row(key, palette)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.channel_filter = filter;
                            this.close_popups(window, cx);
                        }))
                        .child(filter.label()),
                    palette,
                );
            }
            menu
        });
        (chips, format!("{} more", left.len()), more_menu)
    }

    /// The Channels tab's sort, A-Z or Z-A.
    fn channel_sort(&self, palette: Palette, cx: &mut Context<Self>) -> keyboard::Stop {
        keyboard::stop(
            "manage-channels-sort",
            div()
                .id("manage-channels-sort")
                .debug_selector(|| "manage-channels-sort".to_owned())
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_1()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.channel_z_to_a = !this.channel_z_to_a;
                    cx.notify();
                }))
                .child(if self.channel_z_to_a { "Sort: Z-A" } else { "Sort: A-Z" })
                .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
            palette,
        )
    }

    /// The Channels tab.
    fn channels_tab(
        &mut self,
        team: &Team,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let search = self.channel_box(window, cx);
        let read_only = self.core.read_only();
        let (chips, more_words, more_menu) = self.channel_chips(palette, cx);
        let toolbar = div()
            .flex_none()
            .px_6()
            .pt_6()
            .pb_3()
            .flex()
            .items_center()
            .gap_3()
            .children(chips)
            .child(with_popup(
                keyboard::stop(
                    "manage-channels-more",
                    chip_base("manage-channels-more".to_owned(), false, palette)
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(more_words)
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0)))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.channel_more = !this.channel_more;
                            this.popup_toggled(this.channel_more, window, cx);
                            cx.notify();
                        })),
                    palette,
                )
                .edge(),
                more_menu,
            ))
            .child(self.channel_sort(palette, cx))
            .child(div().flex_1())
            .when(!read_only && team.can_add_channel, |bar| {
                let id = team.id.clone();
                bar.child(selected_by(
                    "manage-add-channel",
                    Button::new("manage-add-channel")
                        .ghost()
                        .icon(Glyph::PLUS.icon())
                        .label("Add channel")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.teams.update(cx, |teams, cx| teams.add_channel(&id, cx));
                        })),
                ))
            })
            .child(
                div()
                    .w(px(280.0))
                    .min_w(px(120.0))
                    .flex_shrink(1.0)
                    .child(Input::new(&search)),
            );
        let rows = self.channel_rows(team, read_only, palette, cx);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(rows)
            .into_any_element()
    }

    /// The channels the tab lists: as the provisioning service gives them, or as stored while it hasn't, each with how it
    /// stands in the account's list.
    fn channel_rows(&self, team: &Team, read_only: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(channels) = &self.channels else {
            return centered("Loading…", palette);
        };
        let mut rows: Vec<(String, String, String, String, bool, bool, bool)> = channels
            .iter()
            .map(|channel| {
                let stored = team.channels.iter().find(|stored| stored.id == channel.id);
                (
                    channel.id.clone(),
                    channel.name.clone(),
                    channel.description.clone(),
                    channel.kind.clone(),
                    channel.archived,
                    stored.is_some_and(|stored| stored.general) || channel.id == team.id,
                    stored.is_none_or(|stored| stored.shown),
                )
            })
            .collect();
        if rows.is_empty() {
            rows = team
                .channels
                .iter()
                .map(|channel| {
                    (
                        channel.id.clone(),
                        channel.name.clone(),
                        String::new(),
                        channel.kind.key().to_owned(),
                        false,
                        channel.general,
                        channel.shown,
                    )
                })
                .collect();
        }
        rows.retain(|(_, name, _, kind, archived, _, shown)| {
            let by_filter = match self.channel_filter {
                ChannelFilter::All => true,
                ChannelFilter::Shown => *shown,
                ChannelFilter::Hidden => !*shown,
                ChannelFilter::Private => kind.eq_ignore_ascii_case("private"),
                ChannelFilter::Archived => *archived,
            };
            by_filter && (self.channel_query.is_empty() || name.to_lowercase().contains(&self.channel_query))
        });
        rows.sort_by_key(|row| row.1.to_lowercase());
        if self.channel_z_to_a {
            rows.reverse();
        }
        let shown: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(at, line)| Self::channel_line(at, line, read_only, palette, cx))
            .collect();
        div()
            .id("manage-channels")
            .debug_selector(|| "manage-channels".to_owned())
            .flex_1()
            .min_h_0()
            .px_6()
            .overflow_y_scroll()
            .children(shown)
            .into_any_element()
    }

    /// A channel's line, as the Channels tab lists it: `line` is its id, name, description, kind, whether it is archived,
    /// whether it is the general one and whether it is shown.
    fn channel_line(
        at: usize,
        line: (String, String, String, String, bool, bool, bool),
        read_only: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (id, name, description, kind, _, general, shown) = line;
        let (toggled, opened) = (id.clone(), id);
        let private = kind.eq_ignore_ascii_case("private");
        let toggle = (!read_only).then(|| {
            let button = Button::new(SharedString::from(format!("manage-channel-toggle-{at}")))
                .small()
                .label(if shown { "Hide" } else { "Show" })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_channel_shown(&toggled, general, !shown, cx);
                }));
            selected_by(
                format!("manage-channel-toggle-{at}"),
                if shown { button.ghost() } else { button.outline() },
            )
        });
        let line = div()
            .debug_selector(move || format!("manage-channel-{at}"))
            .id(SharedString::from(format!("manage-channel-{at}")))
            .h(px(60.0))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.emit(ManageTeamEvent::OpenChannel(opened.clone()));
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).truncate().child(name))
                            .when(general, |row| row.child(Glyph::HOUSE.icon().size(px(14.0))))
                            .when(private, |row| row.child(Glyph::LOCK.icon().size(px(14.0)))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(rgb(palette.muted_foreground))
                            .child(description),
                    ),
            )
            .children(toggle);
        keyboard::stop(SharedString::from(format!("manage-channel-{at}")), line, palette).into_any_element()
    }

    /// The Members tab's Add member and Search members.
    fn member_toolbar(&self, team: &Team, search: &Entity<InputState>, cx: &mut Context<Self>) -> gpui::Div {
        let can_add = !self.core.read_only() && team.can_add_member();
        div()
            .flex_none()
            .px_5()
            .py_4()
            .flex()
            .items_center()
            .justify_between()
            .child(div().children(can_add.then(|| {
                let id = team.id.clone();
                selected_by(
                    "manage-add-member",
                    Button::new("manage-add-member")
                        .outline()
                        .icon(Glyph::PLUS.icon())
                        .label("Add member")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.teams
                                .update(cx, |teams, cx| teams.choose(&id, TeamChoice::AddMember, cx));
                        })),
                )
            })))
            .child(
                div()
                    .w(px(280.0))
                    .min_w(px(120.0))
                    .flex_shrink(1.0)
                    .child(Input::new(search)),
            )
    }

    /// The Members tab.
    fn members_tab(
        &mut self,
        team: &Team,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let search = self.member_box(window, cx);
        let toolbar = self.member_toolbar(team, &search, cx);
        let Some(members) = self.members.clone() else {
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(toolbar)
                .child(centered("Loading…", palette))
                .into_any_element();
        };
        let query = self.member_query.clone();
        let matched: Vec<TeamMember> = members
            .into_iter()
            .filter(|member| query.is_empty() || member.name.to_lowercase().contains(&query))
            .collect();
        let (owners, others): (Vec<TeamMember>, Vec<TeamMember>) = matched.into_iter().partition(|member| member.owner);
        let can_change = !self.core.read_only() && team.owner;
        let (owners_open, others_open) = (self.owners_open, self.others_open);
        let owners_title = Self::fold_title(
            "manage-owners",
            format!("Owners ({})", owners.len()),
            owners_open,
            true,
            palette,
            cx,
        );
        let others_title = Self::fold_title(
            "manage-others",
            format!("Members and guests ({})", others.len()),
            others_open,
            false,
            palette,
            cx,
        );
        let mut rows: Vec<AnyElement> = vec![owners_title];
        if owners_open {
            rows.push(member_columns(palette));
            rows.extend(
                owners
                    .iter()
                    .map(|member| self.member_row(member, can_change, palette, cx)),
            );
        }
        rows.push(others_title);
        if others_open {
            rows.push(member_columns(palette));
            rows.extend(
                others
                    .iter()
                    .map(|member| self.member_row(member, can_change, palette, cx)),
            );
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .id("manage-members")
                    .debug_selector(|| "manage-members".to_owned())
                    .flex_1()
                    .min_h_0()
                    .px_5()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    fn fold_title(
        id: &'static str,
        words: String,
        open: bool,
        owners: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .h(px(44.0))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if owners {
                    this.owners_open = !this.owners_open;
                } else {
                    this.others_open = !this.others_open;
                }
                cx.notify();
            }))
            .child(
                if open {
                    Glyph::CHEVRON_DOWN
                } else {
                    Glyph::CHEVRON_RIGHT
                }
                .icon()
                .size(px(16.0)),
            )
            .child(words);
        keyboard::stop(id, title, palette).into_any_element()
    }

    /// A member's Team role: a dropdown for an owner to change it.
    fn role_cell(&self, member: &TeamMember, can_change: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let mri = member.mri.clone();
        let (role_id, promote, demote) = (mri.clone(), mri.clone(), mri.clone());
        let role_words = if member.owner {
            "Owner"
        } else if member.guest {
            "Guest"
        } else {
            "Member"
        };
        let open = self.role_menu.as_deref() == Some(mri.as_str());
        let menu = open.then(|| {
            popup_menu(
                palette,
                cx.listener(|this, _, _, cx| {
                    this.role_menu = None;
                    cx.notify();
                }),
            )
            .keyed(self.popup.root(), self.popup_keys(cx))
            .row(
                "manage-role-owner",
                menu_row("manage-role-owner", palette)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.update_member(&promote, MemberUpdate::Promoted, cx);
                        this.popup.closed(window, cx);
                    }))
                    .child("Owner"),
                palette,
            )
            .row(
                "manage-role-member",
                menu_row("manage-role-member", palette)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.update_member(&demote, MemberUpdate::Demoted, cx);
                        this.popup.closed(window, cx);
                    }))
                    .child("Member"),
                palette,
            )
        });
        if can_change && !member.guest {
            with_popup(
                selected_by(
                    format!("manage-role-{mri}"),
                    Button::new(SharedString::from(format!("manage-role-{mri}")))
                        .ghost()
                        .small()
                        .label(role_words)
                        .dropdown_caret(true)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.role_menu = if this.role_menu.as_deref() == Some(role_id.as_str()) {
                                None
                            } else {
                                Some(role_id.clone())
                            };
                            this.popup_toggled(this.role_menu.is_some(), window, cx);
                            cx.notify();
                        })),
                ),
                menu,
            )
            .into_any_element()
        } else {
            div().child(role_words).into_any_element()
        }
    }

    fn member_row(
        &self,
        member: &TeamMember,
        can_change: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mri = member.mri.clone();
        let remove = mri.clone();
        let name: SharedString = member.name.clone().into();
        let role = self.role_cell(member, can_change, palette, cx);
        let selector = format!("manage-member-{mri}");
        div()
            .debug_selector(move || selector.clone())
            .h(px(52.0))
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(crate::people::avatar(Some(&name), None, gpui_component::Size::Small))
                    .child(div().min_w_0().truncate().child(name.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(member.title.clone().unwrap_or_default()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(member.location.clone().unwrap_or_default()),
            )
            .child(div().w(px(140.0)).flex_none().child(role))
            .child(div().w(px(32.0)).flex_none().children(can_change.then(|| {
                icon_button(
                    SharedString::from(format!("manage-remove-{mri}")),
                    Glyph::X,
                    format!("Remove {name}"),
                )
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.update_member(&remove, MemberUpdate::Removed, cx)))
            })))
            .into_any_element()
    }

    /// The Pending requests tab.
    fn requests_tab(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(requests) = &self.requests else {
            return centered("Loading…", palette);
        };
        if requests.is_empty() {
            return centered("There are no pending requests to join this team.", palette);
        }
        let rows: Vec<AnyElement> = requests
            .iter()
            .enumerate()
            .map(|(at, request)| {
                let (accepted, denied) = (request.mri.clone(), request.mri.clone());
                div()
                    .debug_selector(move || format!("manage-request-{at}"))
                    .h(px(52.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(div().flex_1().min_w_0().truncate().child(request.name.clone()))
                    .child(selected_by(
                        format!("manage-request-accept-{at}"),
                        Button::new(SharedString::from(format!("manage-request-accept-{at}")))
                            .primary()
                            .small()
                            .label("Accept")
                            .disabled(self.core.read_only())
                            .on_click(cx.listener(move |this, _, _, cx| this.answer_request(&accepted, true, cx))),
                    ))
                    .child(selected_by(
                        format!("manage-request-deny-{at}"),
                        Button::new(SharedString::from(format!("manage-request-deny-{at}")))
                            .outline()
                            .small()
                            .label("Deny")
                            .disabled(self.core.read_only())
                            .on_click(cx.listener(move |this, _, _, cx| this.answer_request(&denied, false, cx))),
                    ))
                    .into_any_element()
            })
            .collect();
        div()
            .id("manage-requests")
            .flex_1()
            .min_h_0()
            .p_5()
            .overflow_y_scroll()
            .children(rows)
            .into_any_element()
    }

    fn section(
        &self,
        key: &'static str,
        (heading, about): (&'static str, &'static str),
        content: AnyElement,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.open_sections.contains(key);
        let selector = format!("manage-section-{key}");
        div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(keyboard::stop(
                SharedString::from(selector.clone()),
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .min_h(px(44.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.open_sections.remove(key) {
                            this.open_sections.insert(key);
                        }
                        cx.notify();
                    }))
                    .child(
                        if open {
                            Glyph::CHEVRON_DOWN
                        } else {
                            Glyph::CHEVRON_RIGHT
                        }
                        .icon()
                        .size(px(16.0)),
                    )
                    .child(
                        div()
                            .w(px(200.0))
                            .flex_none()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(heading),
                    )
                    .child(div().flex_1().text_sm().child(about)),
                palette,
            ))
            .when(open, |section| {
                section.child(div().pl_6().pb_4().flex().flex_col().gap_2().child(content))
            })
            .into_any_element()
    }

    /// Whether a box of the team's settings may be changed now ([`Self::set_setting`]): the settings were read, and
    /// neither a write nor a read of them is under way (a read answered after a change would put back what it replaced,
    /// and the next change would send that).
    fn settings_writable(&self) -> bool {
        !self.core.read_only()
            && self.writing.is_none()
            && !self.settings_reading
            && self.settings.as_ref().is_some_and(Value::is_object)
    }

    fn boxes(&self, boxes: &'static [SettingBox], settings: &Value, cx: &mut Context<Self>) -> AnyElement {
        let can_write = self.settings_writable();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(boxes.iter().map(|setting| {
                let (guests, keys) = (setting.guests, setting.keys);
                let on = setting_on(settings, guests, keys);
                selected_by(
                    setting.selector,
                    Checkbox::new(setting.selector)
                        .label(setting.words)
                        .checked(on)
                        .disabled(!can_write)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_setting(guests, keys, *checked, cx);
                        })),
                )
            }))
            .into_any_element()
    }

    /// Member permissions' boxes.
    fn member_boxes(
        &self,
        settings: &Value,
        owners_only: bool,
        automatic: bool,
        can_write: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.boxes(&MEMBER_BOXES, settings, cx))
            .child(selected_by(
                "member-manage-tags-enabled",
                Checkbox::new("member-manage-tags-enabled")
                    .label("Allow members to create, edit, and delete tags")
                    .checked(!owners_only)
                    .disabled(!can_write)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_tag_settings(!*checked, automatic, cx);
                    })),
            ))
            .into_any_element()
    }

    /// @mentions' boxes.
    fn mention_boxes(
        &self,
        settings: &Value,
        owners_only: bool,
        automatic: bool,
        can_write: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.boxes(&MENTION_BOXES, settings, cx))
            .child(selected_by(
                "mention-settings-automatic-tags",
                Checkbox::new("mention-settings-automatic-tags")
                    .label(
                        "Show members the option to @mention groups based on job title or department. This will notify \
                         everyone in that group.",
                    )
                    .checked(automatic)
                    .disabled(!can_write)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_tag_settings(owners_only, *checked, cx);
                    })),
            ))
            .into_any_element()
    }

    /// The Settings tab.
    fn settings_tab(&self, team: &Team, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(settings) = self.settings.clone() else {
            return centered("Loading…", palette);
        };
        let tags = self.tags.clone().unwrap_or_default();
        let can_write = !self.core.read_only();
        let details = self.details_section(team, can_write, palette, cx);
        let (owners_only, automatic) = (tags.owners_only, tags.automatic);
        let members = self.member_boxes(&settings, owners_only, automatic, can_write, cx);
        let guests = self.boxes(&GUEST_BOXES, &settings, cx);
        let mentions = self.mention_boxes(&settings, owners_only, automatic, can_write, cx);
        let code = self.code_section(can_write, palette, cx);
        let fun = self.fun_boxes(&settings, palette, cx);
        let tag_toggle = selected_by(
            "tag-settings-source-toggle",
            Checkbox::new("tag-settings-source-toggle")
                .label("Automatically apply tags from other apps. Shifts")
                .checked(automatic)
                .disabled(!can_write)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.set_tag_settings(owners_only, *checked, cx);
                })),
        )
        .into_any_element();
        let sections = vec![
            self.section("details", ("Team details", "Edit team name, description, privacy, and picture"), details,
                palette,
                cx,
            ),
            self.section("members", ("Member permissions", "Enable channel creation, adding apps, and more"), members,
                palette,
                cx,
            ),
            self.section("guests", ("Guest permissions", "Enable channel creation"), guests,
                palette,
                cx,
            ),
            self.section("mentions", ("@mentions", "Choose who can use team, channel, and group @mentions"), mentions,
                palette,
                cx,
            ),
            self.section("code", ("Team code", "Share this code so people can join the team directly - you won't get join requests for public teams"), code,
                palette,
                cx,
            ),
            self.section("fun", ("Fun stuff", "Allow memes, GIFs, or stickers"), fun, palette, cx),
            self.section("tags", ("Tags", "Automatically apply tags from other apps"), tag_toggle,
                palette,
                cx,
            ),
        ];
        div()
            .id("manage-settings")
            .debug_selector(|| "manage-settings".to_owned())
            .flex_1()
            .min_h_0()
            .px_6()
            .py_4()
            .overflow_y_scroll()
            .children(sections)
            .into_any_element()
    }

    fn details_section(&self, team: &Team, can_write: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let edited = team.clone();
        let avatar = self.teams.read(cx).avatar(team, 64.0, false, palette);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(team.name.clone()),
            )
            .child(div().text_sm().child(team.about.description.clone()))
            .child(
                div().flex().text_color(rgb(palette.muted_foreground)).child(
                    if team.about.public { Glyph::GLOBE } else { Glyph::LOCK }
                        .icon()
                        .size(px(16.0)),
                ),
            )
            .when(can_write, |details| {
                details.child(
                    div().flex().child(selected_by(
                        "manage-edit-details",
                        Button::new("manage-edit-details")
                            .primary()
                            .label("Edit")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ManageTeamEvent::EditDetails(edited.clone()));
                            })),
                    )),
                )
            })
            .child(
                div()
                    .pt_2()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Team picture"),
            )
            .child(avatar)
            .when(can_write && team.owner && team.group_id.is_some(), |details| {
                let pictured = team.clone();
                details.child(
                    div().flex().child(selected_by(
                        "manage-change-picture",
                        Button::new("manage-change-picture")
                            .ghost()
                            .icon(Glyph::UPLOAD.icon())
                            .label("Change picture")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ManageTeamEvent::ChangePicture(pictured.clone()));
                            })),
                    )),
                )
            })
            .into_any_element()
    }

    fn code_section(&self, can_write: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let code = self.code.clone();
        let mut section = div().flex().flex_col().gap_2();
        if let Some(code) = code {
            let copied = code.clone();
            section = section
                .child(
                    div()
                        .debug_selector(|| "manage-team-code".to_owned())
                        .text_xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(code),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(selected_by(
                            "manage-code-copy",
                            Button::new("manage-code-copy")
                                .outline()
                                .label("Copy")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                                    cx.emit(ManageTeamEvent::Toast("Copied to clipboard"));
                                })),
                        ))
                        .when(can_write, |buttons| {
                            buttons
                                .child(selected_by(
                                    "manage-code-reset",
                                    Button::new("manage-code-reset")
                                        .outline()
                                        .label("Reset")
                                        .on_click(cx.listener(|this, _, _, cx| this.write_code(false, cx))),
                                ))
                                .child(selected_by(
                                    "manage-code-remove",
                                    Button::new("manage-code-remove")
                                        .outline()
                                        .label("Remove")
                                        .on_click(cx.listener(|this, _, _, cx| this.write_code(true, cx))),
                                ))
                        }),
                );
        } else if can_write && self.code_read {
            section = section.child(
                div().flex().child(selected_by(
                    "manage-code-generate",
                    Button::new("manage-code-generate")
                        .primary()
                        .label("Generate")
                        .on_click(cx.listener(|this, _, _, cx| this.write_code(false, cx))),
                )),
            );
        }
        section
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Note: guests won't be able to join with a team code"),
            )
            .into_any_element()
    }

    /// The time period's menu.
    fn period_menu(&self, palette: Palette, cx: &mut Context<Self>) -> crate::teams_page::PopupMenu {
        let current = self.period;
        let mut menu = popup_menu(
            palette,
            cx.listener(|this, _, _, cx| {
                this.period_open = false;
                cx.notify();
            }),
        )
        .keyed(self.popup.root(), self.popup_keys(cx));
        for period in [
            AnalyticsPeriod::LastSevenDays,
            AnalyticsPeriod::LastThirtyDays,
            AnalyticsPeriod::LastNinetyDays,
        ] {
            let key = format!("manage-period-{}", period.wire());
            menu = menu.row(
                key.clone(),
                menu_row(key, palette)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.period = period;
                        this.close_popups(window, cx);
                        this.read_analytics(cx);
                    }))
                    .child(div().flex_1().child(period.label()))
                    .when(period == current, |row| row.child(Glyph::CHECK.icon().size(px(14.0)))),
                palette,
            );
        }
        menu
    }

    /// The Analytics tab.
    fn analytics_tab(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let current = self.period;
        let menu = self.period_open.then(|| self.period_menu(palette, cx));
        let channels = self.channel_picker(palette, cx);
        let period = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child("Time period"))
            .child(with_popup(
                keyboard::stop(
                    "manage-period",
                    div()
                        .id("manage-period")
                        .debug_selector(|| "manage-period".to_owned())
                        .w(px(240.0))
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
                            this.period_open = !this.period_open;
                            this.popup_toggled(this.period_open, window, cx);
                            cx.notify();
                        }))
                        .child(current.label())
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
                    palette,
                )
                .edge(),
                menu,
            ));
        let picker = div().flex().gap_3().child(channels).child(period);
        let content: AnyElement = match &self.analytics {
            _ if !self.analytics_read => centered("Loading…", palette),
            None => centered("There's nothing to show for this time period yet.", palette),
            Some(numbers) => analytics_boxes(numbers, palette),
        };
        div()
            .id("manage-analytics")
            .debug_selector(|| "manage-analytics".to_owned())
            .flex_1()
            .min_h_0()
            .p_6()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .child(picker)
            .child(content)
            .into_any_element()
    }

    /// What the Tags tab says with no tags.
    fn tags_empty(create: Option<gpui::Div>, can_write: bool, palette: Palette) -> AnyElement {
        div()
            .debug_selector(|| "manage-tags-empty".to_owned())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .p_4()
                    .rounded_full()
                    .bg(rgb(palette.hover))
                    .child(Glyph::TAG.icon().size(px(32.0))),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("This team doesn't have any tags yet"),
            )
            .child(div().text_sm().child(if can_write {
                "Create some so you can notify a group of people all at once by @mentioning a tag."
            } else {
                "Contact a team owner to add a tag."
            }))
            .children(create)
            .into_any_element()
    }

    /// A tag's line, with Edit and Delete for who may.
    fn tag_row(
        at: usize,
        tag: &TeamTag,
        team_id: &str,
        can_write: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (edited, team_edited) = (tag.clone(), team_id.to_owned());
        let (deleted, team_deleted) = (tag.id.clone(), team_id.to_owned());
        div()
            .debug_selector(move || format!("manage-tag-{at}"))
            .h(px(56.0))
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(Glyph::TAG.icon().size(px(16.0)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(tag.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(rgb(palette.muted_foreground))
                            .child(tag.description.clone()),
                    ),
            )
            .child(div().text_sm().child(format!("{} people", tag.members)))
            .when(can_write, |row| {
                row.child(
                    icon_button(
                        SharedString::from(format!("manage-tag-edit-{at}")),
                        Glyph::PENCIL,
                        "Edit tag",
                    )
                    .xsmall()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ManageTeamEvent::EditTag(team_edited.clone(), Some(edited.clone())));
                    })),
                )
                .child(
                    icon_button(
                        SharedString::from(format!("manage-tag-delete-{at}")),
                        Glyph::TRASH,
                        "Delete tag",
                    )
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.write(
                            TeamWrite::DeleteTag {
                                team: team_deleted.clone(),
                                tag: deleted.clone(),
                            },
                            "We had trouble applying your changes. Please try again later.",
                            cx,
                        );
                    })),
                )
            })
            .into_any_element()
    }

    /// The Tags tab.
    fn tags_tab(&self, team: &Team, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(tags) = self.tags.clone() else {
            return centered("Loading…", palette);
        };
        let can_write = !self.core.read_only() && !team.archived && (team.owner || !tags.owners_only);
        let team_id = team.id.clone();
        let create = can_write.then(|| {
            let team_id = team_id.clone();
            selected_by(
                "manage-create-tag",
                Button::new("manage-create-tag")
                    .primary()
                    .label("Create tag")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ManageTeamEvent::EditTag(team_id.clone(), None));
                    })),
            )
        });
        if tags.tags.is_empty() {
            return Self::tags_empty(create, can_write, palette);
        }
        let rows: Vec<AnyElement> = tags
            .tags
            .iter()
            .enumerate()
            .map(|(at, tag)| Self::tag_row(at, tag, &team_id, can_write, palette, cx))
            .collect();
        div()
            .id("manage-tags")
            .debug_selector(|| "manage-tags".to_owned())
            .flex_1()
            .min_h_0()
            .p_6()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(div().pb_3().flex().justify_end().children(create))
            .children(rows)
            .into_any_element()
    }
}

/// The Members tab's column titles.
fn member_columns(palette: Palette) -> AnyElement {
    div()
        .h(px(32.0))
        .flex()
        .items_center()
        .gap_3()
        .text_xs()
        .text_color(rgb(palette.muted_foreground))
        .child(div().flex_1().child("Name"))
        .child(div().flex_1().child("Title"))
        .child(div().flex_1().child("Location"))
        .child(div().w(px(140.0)).flex_none().child("Team role"))
        .child(div().w(px(32.0)).flex_none())
        .into_any_element()
}

/// Active users by day, as bars.
fn active_users_bars(numbers: &TeamAnalytics, palette: Palette) -> gpui::Div {
    let most = numbers
        .active_users_by_day
        .iter()
        .map(|(_, value)| *value)
        .max()
        .unwrap_or(1)
        .max(1);
    let bars = numbers.active_users_by_day.iter().map(|(day, value)| {
        let height = u16::try_from(value * 140 / most).unwrap_or(140);
        let shown_day: String = day.chars().skip(5).take(5).collect();
        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .justify_end()
            .items_center()
            .gap_1()
            .child(div().text_xs().child(value.to_string()))
            .child(
                div()
                    .w(px(24.0))
                    .h(px(f32::from(height).max(2.0)))
                    .rounded_sm()
                    .bg(rgb(palette.brand)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(shown_day),
            )
    });
    div().h(px(190.0)).flex().items_end().gap_2().children(bars)
}

/// The Analytics tab's boxes: Summary, Engagement, and active users by day as bars.
fn analytics_boxes(numbers: &TeamAnalytics, palette: Palette) -> AnyElement {
    let tile = |value: u64, words: &'static str| {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .child(div().text_2xl().child(value.to_string()))
            .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(words))
    };
    let boxed = |heading: &'static str, tiles: Vec<gpui::Div>| {
        div()
            .flex_1()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(heading))
            .child(div().flex().gap_3().children(tiles))
    };
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .gap_3()
                .child(boxed(
                    "Summary",
                    vec![
                        tile(numbers.active_users, "Active users"),
                        tile(numbers.guests, "Active guests"),
                        tile(numbers.meetings, "Meetings"),
                    ],
                ))
                .child(boxed(
                    "Engagement",
                    vec![
                        tile(numbers.posts, "Posts"),
                        tile(numbers.replies, "Replies"),
                        tile(numbers.reactions, "Reactions"),
                        tile(numbers.mentions, "Mentions"),
                    ],
                )),
        )
        .child(
            div()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Active users"))
                .child(active_users_bars(numbers, palette)),
        )
        .into_any_element()
}

impl Render for ManageTeam {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let Some(team) = self.team_of(cx) else {
            return div()
                .debug_selector(|| "manage-team".to_owned())
                .size_full()
                .flex()
                .flex_col()
                .bg(rgb(palette.background))
                .child(centered("This team isn't on your list anymore.", palette));
        };
        let body = match self.tab {
            ManageTab::Channels => self.channels_tab(&team, palette, window, cx),
            ManageTab::Members => self.members_tab(&team, palette, window, cx),
            ManageTab::PendingRequests => self.requests_tab(palette, cx),
            ManageTab::Settings => self.settings_tab(&team, palette, cx),
            ManageTab::Analytics => self.analytics_tab(palette, cx),
            ManageTab::Apps => self.apps_tab(&team, palette, window, cx),
            ManageTab::Tags => self.tags_tab(&team, palette, cx),
        };
        let problem = self.problem.map(|words| {
            div()
                .debug_selector(|| "manage-problem".to_owned())
                .flex_none()
                .px_6()
                .py_2()
                .text_sm()
                .text_color(rgb(theme::DANGER))
                .child(words)
        });
        div()
            .debug_selector(|| "manage-team".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(self.header(&team, palette, cx))
            .children(problem)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_box_stands_for_all_its_keys_and_writes_them_all() {
        let settings = json!({"userSettings": {"createTopic": true, "updateTopic": false, "other": 1}});
        assert!(!setting_on(&settings, false, &["createTopic", "updateTopic"]));
        assert!(setting_on(&settings, false, &["createTopic"]));
        let changed = with_setting(settings, false, &["createTopic", "updateTopic"], true);
        assert!(setting_on(&changed, false, &["createTopic", "updateTopic"]));
        assert_eq!(
            changed["userSettings"]["other"], 1,
            "the rest of the settings go back as they came"
        );
    }

    #[test]
    fn a_guests_box_goes_under_the_guests_settings() {
        let changed = with_setting(Value::Null, true, &["deleteTopic"], true);
        assert!(setting_on(&changed, true, &["deleteTopic"]));
        assert!(!setting_on(&changed, false, &["deleteTopic"]));
    }

    #[test]
    fn only_an_owner_gets_pending_requests_and_settings() {
        let owner = Team {
            owner: true,
            ..Team::default()
        };
        assert!(tabs_of(&owner).contains(&ManageTab::Settings));
        assert!(tabs_of(&owner).contains(&ManageTab::PendingRequests));
        let member = Team::default();
        assert_eq!(
            tabs_of(&member),
            [
                ManageTab::Channels,
                ManageTab::Members,
                ManageTab::Analytics,
                ManageTab::Apps,
                ManageTab::Tags
            ]
        );
    }
}
