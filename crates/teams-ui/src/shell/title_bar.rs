//! The title bar across the top of the window, as Teams draws it: on the left the app launcher (the grid over the
//! rail) and the list pane's toggle, then Back and Forward at the list pane's right edge; the search box
//! ([`GlobalSearch`]) from there; on the right "Settings and more" (…) and the account's avatar with its presence,
//! which opens the account menu under it. Its empty parts move the window (a double click maximizes it), as a title bar
//! does.
//!
//! - **Back and Forward** (Alt+Left, Alt+Right) go through the places the shell showed ([`History`]): a chat, a
//!   channel, the calendar, the settings, the search's results. A place shown again after Back drops what was ahead.
//! - **The list pane's toggle** hides the list of chats and teams and shows it again.
//! - **The launcher** lists the app's own places (Chat, Calendar, Settings), as Teams' lists its apps.
//! - **Settings and more**: the settings, the zoom (the base font size: Ctrl+=, Ctrl+-, Ctrl+0) and "Copy
//!   diagnostics".
//! - **The app's update** ([`crate::update`]), right of the search while there is one: its download's progress,
//!   Restart to update, or Update failed.
//!
//! Traced: `title-bar back` and `forward` (the place gone to, by kind), `list-pane hidden`/`shown`.

use gpui::{
    AnyElement, App, ClickEvent, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Size;
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable as _, Selectable as _};

use super::{FontLarger, FontReset, FontSmaller, LIST_WIDTH, Main, RAIL_WIDTH, Shell, shown_as};
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::people;
use crate::presence;
use crate::settings_view::Page;
use crate::theme::{self, Palette};
use crate::trace::TraceLine;

gpui::actions!(teams_ui, [GoBack, GoForward, FocusSearch, ToggleListPane]);

/// The title bar's height, px.
pub const TITLE_HEIGHT: f32 = 48.0;
/// The widest the search box gets, px.
pub const SEARCH_MAX: f32 = 784.0;
/// The right part's width (Settings and more, the account), px: the search's results and suggestions line up with it.
pub const RIGHT_WIDTH: f32 = 104.0;
/// The most places Back goes through.
const HISTORY_MAX: usize = 50;
/// The account's avatar in the title bar, px.
const AVATAR: f32 = 32.0;

/// The places the shell showed, for Back and Forward (see the module docs).
#[derive(Debug, Default)]
pub struct History {
    back: Vec<Main>,
    forward: Vec<Main>,
    current: Option<Main>,
}

impl History {
    /// `main` is shown: the place shown before goes on the back list, and what was ahead is dropped. "No chat" isn't a
    /// place to go back to.
    pub fn note(&mut self, main: &Main) {
        match self.current.replace(main.clone()) {
            Some(left) if left != *main => {
                if left != Main::Nothing {
                    self.back.retain(|place| *place != left);
                    self.back.push(left);
                    if self.back.len() > HISTORY_MAX {
                        self.back.remove(0);
                    }
                }
                self.forward.clear();
            }
            _ => {}
        }
    }

    /// The place Back goes to, taken off the back list (the place shown goes ahead); `None` at the start.
    pub fn back(&mut self, shown: &Main) -> Option<Main> {
        let to = self.back.pop()?;
        if *shown != Main::Nothing {
            self.forward.push(shown.clone());
        }
        self.current = Some(to.clone());
        Some(to)
    }

    /// The place Forward goes to, taken off the forward list (the place shown goes back); `None` when nothing is ahead.
    pub fn forward(&mut self, shown: &Main) -> Option<Main> {
        let to = self.forward.pop()?;
        if *shown != Main::Nothing {
            self.back.push(shown.clone());
        }
        self.current = Some(to.clone());
        Some(to)
    }

    /// Takes `place` off both lists (a place that can't be shown again: the search results once left).
    pub fn forget(&mut self, place: &Main) {
        self.back.retain(|kept| kept != place);
        self.forward.retain(|kept| kept != place);
        if self.current.as_ref() == Some(place) {
            self.current = None;
        }
    }

    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

/// A place's kind, for the trace.
fn kind(main: &Main) -> &'static str {
    match main {
        Main::Nothing => "nothing",
        Main::Chat(_) => "chat",
        Main::Channel(_) => "channel",
        Main::Settings => "settings",
        Main::Calendar => "calendar",
        Main::Search => "search",
        Main::TeamsPage => "teams-page",
        Main::ManageTeam(_) => "manage-team",
        Main::JoinTeam => "join-team",
        Main::CreateTeamMore => "create-team-more",
        Main::Workflows => "workflows",
        Main::Apps => "apps",
        Main::NewChat => "new-chat",
    }
}

impl Shell {
    /// Back (Alt+Left, the title bar's arrow): the place shown before.
    pub fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.main.clone();
        if let Some(to) = self.history.back(&shown) {
            self.trace.record(&TraceLine::new("title-bar", "back").kind(kind(&to)));
            self.go(to, window, cx);
        }
    }

    /// Forward (Alt+Right): the place Back left.
    pub fn go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.main.clone();
        if let Some(to) = self.history.forward(&shown) {
            self.trace
                .record(&TraceLine::new("title-bar", "forward").kind(kind(&to)));
            self.go(to, window, cx);
        }
    }

    /// Shows `to` again, as it was opened.
    fn go(&mut self, to: Main, window: &mut Window, cx: &mut Context<Self>) {
        match to {
            Main::Chat(id) => self.open_chat(id, window, cx),
            Main::Channel(id) => self.open_channel_id(id, window, cx),
            Main::Settings => self.open_settings(window, cx),
            Main::Calendar => self.show_calendar(window, cx),
            Main::Search => self.show_search_results(window, cx),
            Main::TeamsPage => self.open_teams_page(window, cx),
            Main::ManageTeam(team) => {
                self.open_manage_team(team, crate::manage_team::ManageTab::default(), window, cx);
            }
            Main::JoinTeam => self.open_join_team(window, cx),
            Main::CreateTeamMore => self.open_create_team_more(window, cx),
            Main::Workflows => self.resume_workflows(window, cx),
            Main::Apps => self.open_apps(window, cx),
            Main::NewChat => self.resume_new_chat(window, cx),
            Main::Nothing => {}
        }
        // what was shown is the history's place now, whatever the opening made of it
        self.history.current = Some(self.main.clone());
    }

    /// The places Back and Forward go through.
    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Whether the list pane (the chats and the teams) is shown.
    #[must_use]
    pub fn list_shown(&self) -> bool {
        self.list_shown
    }

    /// Hides the list pane, or shows it again.
    pub fn toggle_list_pane(&mut self, cx: &mut Context<Self>) {
        self.list_shown = !self.list_shown;
        let change = if self.list_shown { "shown" } else { "hidden" };
        self.trace.record(&TraceLine::new("list-pane", change));
        cx.notify();
    }

    /// The list pane's width now (its resizable panel's), px.
    fn list_width(&self, cx: &App) -> f32 {
        self.panes
            .read(cx)
            .sizes()
            .first()
            .map_or(LIST_WIDTH, |width| f32::from(*width))
    }

    /// The left part's width: over the rail and the list pane.
    pub(super) fn title_left_width(&self, cx: &App) -> f32 {
        RAIL_WIDTH + self.list_width(cx)
    }

    /// The title bar (see the module docs).
    pub(super) fn title_bar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let left = div()
            .w(px(self.title_left_width(cx)))
            // a narrow window draws the list pane narrower than its width: the right part must keep its place
            .flex_shrink(1.0)
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .child(
                div()
                    .w(px(RAIL_WIDTH))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(Self::launcher(palette, cx)),
            )
            .child(self.list_toggle(palette, cx))
            .child(drag_area())
            .child(self.history_buttons(palette, cx))
            .pr_1();
        let center = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .px_2()
            // the box as wide as Teams' when there is room, narrower when there isn't; the rest moves the window
            .child(
                div()
                    .w(px(SEARCH_MAX))
                    .flex_shrink(1.0)
                    .min_w_0()
                    .child(self.search_box.clone()),
            )
            .child(drag_area().min_w(px(8.0)))
            .children(Self::update_button(palette, cx));
        let right = div()
            .w(px(RIGHT_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .pr_3()
            .child(self.more_menu(palette, cx))
            .child(self.title_avatar(palette, cx));
        div()
            .debug_selector(|| "title-bar".to_owned())
            .h(px(TITLE_HEIGHT))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            // the frame's own: the shell draws its gradient behind the title bar and the rail
            .text_color(rgb(palette.on_frame))
            .child(left)
            .child(center)
            .child(right)
            .into_any_element()
    }

    /// The app's update, while there is one to show ([`crate::update`]): a press the restart waits on says why.
    fn update_button(palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shell = cx.weak_entity();
        crate::update::button(
            palette,
            move |_, _, cx| {
                if let Some(said) = crate::update::press(cx) {
                    let _ = shell.update(cx, |shell, cx| shell.show_toast_text(said, cx));
                }
            },
            cx,
        )
    }

    /// Whether a box of this account holds a message not sent yet (a chat's, a channel's or a thread's, New
    /// message's): an update's restart waits for it ([`crate::update::Waiting::Unsent`]).
    #[must_use]
    pub fn has_unsent(&self, cx: &App) -> bool {
        let kept = self.drafts.values().any(|compose| compose.read(cx).has_draft(cx));
        let chat = self
            .chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).compose().read(cx).has_draft(cx));
        let channel = self.channel.as_ref().is_some_and(|(view, _)| {
            view.read(cx)
                .drafts(cx)
                .iter()
                .any(|(_, compose)| compose.read(cx).has_draft(cx))
        });
        let new_chat = self.new_chat.iter().chain(&self.kept_new).any(|(view, _)| {
            let view = view.read(cx);
            view.is_starting() || view.compose(cx).read(cx).has_draft(cx)
        });
        kept || chat || channel || new_chat
    }

    /// The app launcher: the app's own places, as Teams lists its apps.
    fn launcher(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let shell = cx.weak_entity();
        let button = on_frame(icon_button("title-launcher", Glyph::GRIP, "Apps"), palette, true).dropdown_menu(
            move |menu, _, _| {
                let place = |label: &'static str, glyph: Glyph, page: Option<Main>| {
                    let shell = shell.clone();
                    PopupMenuItem::new(label)
                        .icon(glyph.icon())
                        .on_click(move |_, window, cx: &mut App| {
                            let _ = shell.update(cx, |shell, cx| match &page {
                                Some(Main::Calendar) => shell.show_calendar(window, cx),
                                Some(_) => shell.open_settings_page(Page::General, window, cx),
                                None => shell.show_chats(window, cx),
                            });
                        })
                };
                menu.min_w(px(200.0))
                    .label("Apps")
                    .item(place("Chat", Glyph::MESSAGE, None))
                    .item(place("Calendar", Glyph::CALENDAR, Some(Main::Calendar)))
                    .item(place("Settings", Glyph::SETTINGS, Some(Main::Settings)))
            },
        );
        selected_by("title-launcher", button).into_any_element()
    }

    /// The list pane's toggle.
    fn list_toggle(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let tip = if self.list_shown {
            "Hide the list"
        } else {
            "Show the list"
        };
        let button = icon_button_with_key("title-list-toggle", Glyph::PANEL_LEFT, tip, &ToggleListPane);
        // hidden, it is chosen: the kit's chosen chip, its icon in the chip's own colour
        let button = (if self.list_shown {
            on_frame(button, palette, true)
        } else {
            button
        })
        .selected(!self.list_shown)
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_list_pane(cx)));
        selected_by("title-list-toggle", button).into_any_element()
    }

    /// Back and Forward.
    fn history_buttons(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let (can_back, can_forward) = (self.history.can_go_back(), self.history.can_go_forward());
        let back = icon_button_with_key("title-back", Glyph::CHEVRON_LEFT, "Back", &GoBack);
        let back = on_frame(back, palette, can_back)
            .disabled(!can_back)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go_back(window, cx)));
        let forward = icon_button_with_key("title-forward", Glyph::CHEVRON_RIGHT, "Forward", &GoForward);
        let forward = on_frame(forward, palette, can_forward)
            .disabled(!can_forward)
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.go_forward(window, cx)));
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by("title-back", back))
            .child(selected_by("title-forward", forward))
            .into_any_element()
    }

    /// "Settings and more": the settings, the zoom and the diagnostics.
    fn more_menu(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let shell = cx.weak_entity();
        let size = self.settings.read(cx).settings().appearance.font_size;
        let zoom = u32::from(size) * 100 / u32::from(theme::FONT_SIZE);
        let button = on_frame(
            icon_button("title-more", Glyph::MORE, "Settings and more"),
            palette,
            true,
        )
        .dropdown_menu(move |menu, _, _| {
            let act = |label: SharedString, glyph: Glyph, run: fn(&mut Shell, &mut Window, &mut Context<Shell>)| {
                let shell = shell.clone();
                PopupMenuItem::new(label)
                    .icon(glyph.icon())
                    .on_click(move |_, window, cx: &mut App| {
                        let _ = shell.update(cx, |shell, cx| run(shell, window, cx));
                    })
            };
            menu.min_w(px(240.0))
                .item(act("Settings".into(), Glyph::SETTINGS, |shell, window, cx| {
                    shell.open_settings_page(Page::General, window, cx);
                }))
                .separator()
                .label(SharedString::from(format!("Zoom: {zoom}%")))
                .item(
                    act("Zoom in".into(), Glyph::ZOOM_IN, |shell, _, cx| {
                        shell.step_font(Some(1), cx);
                    })
                    .action(Box::new(FontLarger)),
                )
                .item(
                    act("Zoom out".into(), Glyph::ZOOM_OUT, |shell, _, cx| {
                        shell.step_font(Some(-1), cx);
                    })
                    .action(Box::new(FontSmaller)),
                )
                .item(
                    act("Reset zoom".into(), Glyph::RESET, |shell, _, cx| {
                        shell.step_font(None, cx);
                    })
                    .action(Box::new(FontReset)),
                )
                .separator()
                .item(act("Copy diagnostics".into(), Glyph::COPY, Shell::copy_diagnostics))
        });
        selected_by("title-more", button).into_any_element()
    }

    /// The account's avatar with its own presence: opens the account menu under it.
    fn title_avatar(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let tile = self.accounts.as_ref().and_then(|(accounts, _)| {
            let accounts = accounts.read(cx);
            let tile = accounts.tile(self.core.me())?;
            Some((tile.name.clone(), tile.detail.clone(), tile.face().cloned()))
        });
        let reporting = self.own_reporting(cx);
        let failed = self.own_presence().is_some_and(|own| own.read(cx).failure().is_some());
        let face: AnyElement = match &tile {
            Some((name, _, picture)) => people::avatar(Some(name), picture.as_ref(), Size::Small).into_any_element(),
            None => div()
                .size(px(AVATAR))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(palette.hover))
                .text_color(rgb(palette.muted_foreground))
                .child(Glyph::USER.icon().size(px(18.0)))
                .into_any_element(),
        };
        let dot = reporting.map(|reporting| {
            presence::dot(&shown_as(reporting), palette)
                .debug_selector(|| "title-account-dot".to_owned())
                .absolute()
                .right(px(-2.0))
                .bottom(px(-2.0))
        });
        let tip: SharedString = match (&tile, failed) {
            (Some((name, detail, _)), false) => format!("{name}\n{detail}").into(),
            (Some((name, detail, _)), true) => format!("{name}\n{detail}\nThe last status report failed").into(),
            (None, false) => "Your account and status".into(),
            (None, true) => "Your account and status (the last report failed)".into(),
        };
        let open = self.status_menu && self.status_anchor == super::MenuAnchor::TitleBar;
        let avatar = div()
            .debug_selector(|| "title-account".to_owned())
            .id("title-account")
            .relative()
            .flex_none()
            .p(px(2.0))
            .rounded_full()
            .cursor_pointer()
            .hover(move |avatar| avatar.bg(rgb(palette.on_frame).opacity(0.2)))
            .when(open, |avatar| avatar.bg(rgb(palette.on_frame).opacity(0.3)))
            .when(failed, |avatar| avatar.border_1().border_color(rgb(theme::DANGER)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.status_avatar_clicked(super::MenuAnchor::TitleBar, window, cx);
            }))
            .child(face)
            .children(dot)
            .into_any_element();
        self.status_opener(super::MenuAnchor::TitleBar, avatar)
    }
}

/// An empty stretch of the title bar: dragged, it moves the window; double-clicked, it maximizes it.
fn drag_area() -> gpui::Stateful<gpui::Div> {
    div()
        .id("title-drag")
        .flex_1()
        .h_full()
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count >= 2 {
                window.zoom_window();
            } else {
                window.start_window_move();
            }
        })
}

/// A title bar button drawn on the frame: its icon in the frame's own colour, faded while it can't be used. (The kit
/// draws a ghost button's icon in the text colour of the panes, which vanishes on a dark frame; its hover and chosen
/// faces stay the kit's, a light chip.)
fn on_frame(button: Button, palette: Palette, usable: bool) -> Button {
    let color = rgb(palette.on_frame);
    button.text_color(if usable {
        color.opacity(0.92)
    } else {
        color.opacity(0.35)
    })
}
