//! Manage team's Apps tab, Fun stuff's Giphy rating and Analytics' "Select a channel" (`docs/protocol/teams.md`):
//!
//! - **Apps**: "Get more apps", Find by name, and a row per app installed on the team ([`Core::team_apps`]) with its
//!   maker and a "..." whose Remove asks first, in Teams' words, then takes the app off the team. Only an owner adds or
//!   removes.
//! - **Giphy rating**: under "Enable Giphy for this team", G, PG or PG-13 (`giphyRating` 0, 1, 2 of the team's settings).
//! - **Select a channel**: All Channels, or one of the team's channels, whose analytics the tab reads instead.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, Sizable as _};
use serde_json::{Value, json};
use teams_proto::{InstalledApp, TeamWrite};
use teams_store::Team;

use super::{FUN_BOXES, ManageTeam, ManageTeamEvent, setting_on};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard;
use crate::people::team_initials;
use crate::shell::menu_row;
use crate::teams_page::{centered, popup_menu, with_popup};
use crate::theme::Palette;

/// The Giphy ratings Teams offers, by their `giphyRating`, as the web client's page named and sent them.
pub const GIPHY_RATINGS: [(u64, &str); 3] = [
    (0, "G (based on the GIPHY Content Rating)"),
    (1, "PG (based on the GIPHY Content Rating)"),
    (2, "PG-13 (based on the GIPHY Content Rating)"),
];

/// The rating a team without one has: PG, as a new team's settings hold it.
const DEFAULT_GIPHY_RATING: u64 = 1;

/// The members' Giphy rating in `settings`.
#[must_use]
pub fn giphy_rating(settings: &Value) -> u64 {
    settings
        .pointer("/userSettings/giphyRating")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_GIPHY_RATING)
}

/// `settings` with the members' Giphy rating `rating`, everything else as it came.
#[must_use]
pub fn with_giphy_rating(mut settings: Value, rating: u64) -> Value {
    if !settings.is_object() {
        settings = json!({});
    }
    if !settings.get("userSettings").is_some_and(Value::is_object) {
        settings["userSettings"] = json!({});
    }
    settings["userSettings"]["giphyRating"] = json!(rating);
    settings
}

impl ManageTeam {
    /// Reads the team's apps; none while the account's cloud serves no apps.
    pub(super) fn read_apps(&mut self, cx: &mut Context<Self>) {
        if !self.core.capabilities().apps {
            self.apps = Some(Vec::new());
            return;
        }
        let reply = self.core.team_apps(self.team.clone());
        self.reading
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let apps = reply.await.unwrap_or_default();
                let _ = this.update(cx, |this, cx| {
                    this.apps = Some(apps);
                    cx.notify();
                });
            }));
    }

    /// The apps the tab lists: installed, their names holding what Find by name holds.
    #[must_use]
    pub fn apps_shown(&self) -> Vec<InstalledApp> {
        let query = self.app_query.to_lowercase();
        self.apps
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|app| query.is_empty() || app.name.to_lowercase().contains(&query))
            .collect()
    }

    /// Opens a section of the Settings tab by its key (`details`, `members`, `fun`, …).
    pub fn open_section(&mut self, key: &'static str, cx: &mut Context<Self>) {
        self.open_sections.insert(key);
        cx.notify();
    }

    /// Opens or closes the Giphy rating's menu.
    pub fn toggle_giphy_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.giphy_open = !self.giphy_open;
        self.popup_toggled(self.giphy_open, window, cx);
        cx.notify();
    }

    /// Opens or closes "Select a channel"'s menu.
    pub fn toggle_channel_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.channel_menu_open = !self.channel_menu_open;
        self.popup_toggled(self.channel_menu_open, window, cx);
        cx.notify();
    }

    /// The ids of the apps on the team, as last read.
    #[must_use]
    pub fn installed_apps(&self) -> Vec<String> {
        self.apps.iter().flatten().map(|app| app.id.clone()).collect()
    }

    /// An app's Remove: asks first.
    pub fn ask_remove_app(&mut self, app: InstalledApp, cx: &mut Context<Self>) {
        self.app_menu = None;
        self.removing_app = Some(app);
        cx.notify();
    }

    /// The question's Remove: the app off the team, gone from the list at once.
    pub fn remove_app(&mut self, cx: &mut Context<Self>) {
        let Some(app) = self.removing_app.take() else {
            return;
        };
        if let Some(apps) = &mut self.apps {
            apps.retain(|listed| listed.id != app.id);
        }
        self.write(
            TeamWrite::RemoveApp {
                team: self.team.clone(),
                app: app.id,
            },
            "We couldn't remove the app. Please try again.",
            cx,
        );
        cx.notify();
    }

    /// The question's Cancel.
    pub fn keep_app(&mut self, cx: &mut Context<Self>) {
        self.removing_app = None;
        cx.notify();
    }

    fn app_find_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(found) = &self.app_find {
            return found.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find by name"));
        let typed = cx.subscribe(&input, |this: &mut Self, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                input.read(cx).value().trim().clone_into(&mut this.app_query);
                cx.notify();
            }
        });
        self.observing_.push(typed);
        self.app_find = Some(input.clone());
        input
    }

    fn app_menu_of(
        &self,
        app: &InstalledApp,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> crate::teams_page::PopupMenu {
        let removed = app.clone();
        popup_menu(
            palette,
            cx.listener(|this, _, _, cx| {
                this.app_menu = None;
                cx.notify();
            }),
        )
        .keyed(self.popup.root(), self.popup_keys(cx))
        .row(
            "manage-app-remove",
            menu_row("manage-app-remove", palette)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.popup.closed(window, cx);
                    this.ask_remove_app(removed.clone(), cx);
                }))
                .child(Glyph::TRASH.icon().size(px(14.0)))
                .child("Remove"),
            palette,
        )
    }

    fn app_row(
        &self,
        at: usize,
        app: &InstalledApp,
        can_write: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let menu_open = self.app_menu.as_deref() == Some(app.id.as_str());
        let id = app.id.clone();
        let menu = menu_open.then(|| self.app_menu_of(app, palette, cx));
        div()
            .debug_selector(move || format!("manage-app-{at}"))
            .px_4()
            .py_2p5()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(app_tile(app, 36.0, palette))
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
                            .child(app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(app.developer.clone()),
                    ),
            )
            .when(can_write, |row| {
                row.child(with_popup(
                    selected_by(
                        format!("manage-app-more-{at}"),
                        icon_button(
                            SharedString::from(format!("manage-app-more-{at}")),
                            Glyph::MORE,
                            "More options",
                        )
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let open = this.app_menu.as_deref() != Some(id.as_str());
                            this.app_menu = open.then(|| id.clone());
                            this.popup_toggled(open, window, cx);
                            cx.notify();
                        })),
                    ),
                    menu,
                ))
            })
            .into_any_element()
    }

    /// The question before an app goes, in Teams' words.
    fn remove_question(app: &InstalledApp, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .debug_selector(|| "manage-app-remove-question".to_owned())
            .mx_6()
            .my_2()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(format!("Remove {}", app.name)),
            )
            .child(
                div()
                    .text_sm()
                    .child(format!("Are you sure you want to remove {}?", app.name)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by(
                        "manage-app-remove-cancel",
                        Button::new("manage-app-remove-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.keep_app(cx))),
                    ))
                    .child(selected_by(
                        "manage-app-remove-confirm",
                        Button::new("manage-app-remove-confirm")
                            .primary()
                            .label("Remove")
                            .on_click(cx.listener(|this, _, _, cx| this.remove_app(cx))),
                    )),
            )
    }

    /// The Apps tab.
    pub(super) fn apps_tab(
        &mut self,
        team: &Team,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let find = self.app_find_box(window, cx);
        let can_write = team.owner && !team.archived && !self.core.read_only();
        let shown = self.apps_shown();
        let mut rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(at, app)| self.app_row(at, app, can_write, palette, cx))
            .collect();
        rows.extend(self.webhook_rows(team, palette, cx));
        let added = team.clone();
        let store = self.core.capabilities().apps;
        let toolbar = div()
            .px_6()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .when(can_write && store, |bar| {
                bar.child(selected_by(
                    "manage-get-apps",
                    Button::new("manage-get-apps")
                        .outline()
                        .icon(Glyph::PLUS.icon())
                        .label("Get more apps")
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(ManageTeamEvent::AddApp(added.clone())))),
                ))
            })
            .children(self.create_webhook_button(team, cx))
            .child(div().flex_1())
            .child(
                div()
                    .w(px(240.0))
                    .debug_selector(|| "manage-app-find".to_owned())
                    .child(Input::new(&find)),
            );
        let question = self
            .removing_app
            .clone()
            .map(|app| Self::remove_question(&app, palette, cx))
            .or_else(|| self.webhook_question(palette, cx));
        let none_at_all = self.webhooks.as_ref().is_none_or(Vec::is_empty);
        let list: AnyElement = match &self.apps {
            None => centered("Loading…", palette),
            Some(apps) if apps.is_empty() && none_at_all => {
                centered("No apps have been added to this team yet.", palette)
            }
            Some(_) if rows.is_empty() => centered("No apps match your search.", palette),
            Some(_) => div().flex().flex_col().px_2().children(rows).into_any_element(),
        };
        div()
            .id("manage-apps")
            .debug_selector(|| "manage-apps".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(toolbar)
            .children(question)
            .child(list)
            .into_any_element()
    }

    /// Fun stuff's boxes, with the Giphy rating under "Enable Giphy for this team" while it is on.
    pub(super) fn fun_boxes(&self, settings: &Value, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let can_write = !self.core.read_only();
        let giphy_on = setting_on(settings, false, FUN_BOXES[0].keys);
        let rating = giphy_rating(settings);
        let words = GIPHY_RATINGS
            .iter()
            .find(|(value, _)| *value == rating)
            .map_or(GIPHY_RATINGS[1].1, |(_, words)| *words);
        let menu = (self.giphy_open && can_write).then(|| self.giphy_menu(rating, palette, cx));
        let boxes: Vec<gpui::Div> = FUN_BOXES
            .iter()
            .map(|setting| {
                let (guests, keys) = (setting.guests, setting.keys);
                selected_by(
                    setting.selector,
                    Checkbox::new(setting.selector)
                        .label(setting.words)
                        .checked(setting_on(settings, guests, keys))
                        .disabled(!can_write)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_setting(guests, keys, *checked, cx);
                        })),
                )
            })
            .collect();
        let mut boxes = boxes.into_iter();
        let picker = giphy_on.then(|| {
            div()
                .pl_6()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .child("Filter out inappropriate content using one of the settings below:"),
                )
                .child(with_popup(
                    keyboard::stop(
                        "giphy-rating",
                        picker_face("giphy-rating", words, can_write, palette).on_click(cx.listener(
                            |this, _, window, cx| {
                                this.giphy_open = !this.giphy_open;
                                this.popup_toggled(this.giphy_open, window, cx);
                                cx.notify();
                            },
                        )),
                        palette,
                    )
                    .edge(),
                    menu,
                ))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(boxes.next())
            .children(picker)
            .children(boxes)
            .into_any_element()
    }

    fn giphy_menu(&self, rating: u64, palette: Palette, cx: &mut Context<Self>) -> crate::teams_page::PopupMenu {
        let mut menu = popup_menu(
            palette,
            cx.listener(|this, _, _, cx| {
                this.giphy_open = false;
                cx.notify();
            }),
        )
        .keyed(self.popup.root(), self.popup_keys(cx));
        for (value, words) in GIPHY_RATINGS {
            let key = format!("giphy-rating-{value}");
            menu = menu.row(
                key.clone(),
                menu_row(key, palette)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_popups(window, cx);
                        this.set_giphy_rating(value, cx);
                    }))
                    .child(div().flex_1().child(words))
                    .when(value == rating, |row| row.child(Glyph::CHECK.icon().size(px(14.0)))),
                palette,
            );
        }
        menu
    }

    /// The members' Giphy rating set to `rating`: the team's whole settings written, as every box writes them.
    pub fn set_giphy_rating(&mut self, rating: u64, cx: &mut Context<Self>) {
        let Some(settings) = self.settings.clone() else {
            return;
        };
        let changed = with_giphy_rating(settings, rating);
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

    /// Analytics' "Select a channel": All Channels, or one channel's.
    pub fn pick_analytics_channel(&mut self, channel: Option<(String, String)>, cx: &mut Context<Self>) {
        self.analytics_channel = channel;
        self.read_analytics(cx);
        cx.notify();
    }

    /// The channel Analytics shows, when one.
    #[must_use]
    pub fn analytics_channel(&self) -> Option<&str> {
        self.analytics_channel.as_ref().map(|(channel, _)| channel.as_str())
    }

    fn channel_menu(&self, team: &Team, palette: Palette, cx: &mut Context<Self>) -> crate::teams_page::PopupMenu {
        let current = self.analytics_channel.as_ref().map(|(channel, _)| channel.clone());
        let mut menu = popup_menu(
            palette,
            cx.listener(|this, _, _, cx| {
                this.channel_menu_open = false;
                cx.notify();
            }),
        )
        .keyed(self.popup.root(), self.popup_keys(cx));
        let mut entries = vec![(None, "All Channels".to_owned())];
        entries.extend(
            team.channels
                .iter()
                .map(|channel| (Some(channel.id.clone()), channel.name.clone())),
        );
        for (at, (channel, name)) in entries.into_iter().enumerate() {
            let key = format!("manage-analytics-channel-{at}");
            let chosen = channel == current;
            let picked = channel.map(|channel| (channel, name.clone()));
            menu = menu.row(
                key.clone(),
                menu_row(key, palette)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_popups(window, cx);
                        this.pick_analytics_channel(picked.clone(), cx);
                    }))
                    .child(div().flex_1().truncate().child(name))
                    .when(chosen, |row| row.child(Glyph::CHECK.icon().size(px(14.0)))),
                palette,
            );
        }
        menu
    }

    /// "Select a channel", over the analytics.
    pub(super) fn channel_picker(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let team = self.team_of(cx);
        let words = self
            .analytics_channel
            .as_ref()
            .map_or_else(|| "All Channels".to_owned(), |(_, name)| name.clone());
        let menu = self
            .channel_menu_open
            .then(|| team.map(|team| self.channel_menu(&team, palette, cx)))
            .flatten();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child("Select a channel"))
            .child(with_popup(
                keyboard::stop(
                    "manage-analytics-channel",
                    picker_face("manage-analytics-channel", words, true, palette).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.channel_menu_open = !this.channel_menu_open;
                            this.popup_toggled(this.channel_menu_open, window, cx);
                            cx.notify();
                        },
                    )),
                    palette,
                )
                .edge(),
                menu,
            ))
    }
}

/// A dropdown's face: its words and a chevron, in a bordered box the keyboard reaches.
fn picker_face(
    id: &'static str,
    words: impl Into<SharedString>,
    enabled: bool,
    palette: Palette,
) -> gpui::Stateful<gpui::Div> {
    let words = words.into();
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w(px(280.0))
        .px_3()
        .py_1p5()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .when(enabled, gpui::Styled::cursor_pointer)
        .when(!enabled, |face| face.opacity(0.6))
        .child(div().truncate().child(words))
        .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0)))
}

/// An app's face: its colour with its initials, as Teams shows a tile when its icon isn't drawn.
pub(crate) fn app_tile(app: &InstalledApp, size: f32, palette: Palette) -> gpui::Div {
    let colour = app.accent.unwrap_or(palette.brand);
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(rgb(colour))
        .text_color(rgb(0x00ff_ffff))
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(team_initials(&app.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_giphy_rating_is_the_members_and_writes_back_with_the_rest() {
        let settings =
            json!({"userSettings": {"giphyEnabled": true, "giphyRating": 2}, "guestSettings": {"giphyRating": 0}});
        assert_eq!(giphy_rating(&settings), 2);
        assert_eq!(giphy_rating(&Value::Null), DEFAULT_GIPHY_RATING);
        let changed = with_giphy_rating(settings, 0);
        assert_eq!(changed["userSettings"]["giphyRating"], 0);
        assert_eq!(changed["userSettings"]["giphyEnabled"], true);
        assert_eq!(changed["guestSettings"]["giphyRating"], 0);
        assert_eq!(with_giphy_rating(Value::Null, 1)["userSettings"]["giphyRating"], 1);
    }
}
