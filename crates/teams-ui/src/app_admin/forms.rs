//! How a Manage apps dialog draws each step (`dialog.rs` holds what it does).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClipboardItem, Context, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::Input;
use gpui_component::spinner::Spinner;
use gpui_component::{Disableable as _, FocusTrapElement as _};
use teams_auth::admin::{AdminApp, AdminUser, BotAuthMode};

use super::dialog::{AppDialog, AppDialogEvent, DialogKind, Made};
use super::sdk::Sdk;
use super::{short, tile};
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::keyboard;
use crate::team_dialogs::{buttons, problem, surface, title};
use crate::theme::{self, Palette};

/// What an unsigned bot's mode says of itself wherever it can be chosen.
pub const UNSIGNED_WARNING: &str = "Unsigned: no token either way. Anyone who can reach the bot's endpoint can post to it. Use it only for \
     development or on a private network.";

/// One choice of a segmented control: its key, its words, whether it is the one set.
struct Segment<T> {
    key: &'static str,
    label: &'static str,
    value: T,
    chosen: bool,
}

fn label(words: &'static str) -> gpui::Div {
    div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(words)
}

fn muted(words: impl Into<SharedString>, palette: Palette) -> gpui::Div {
    div()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .child(words.into())
}

/// A value shown in a monospaced box, with Copy beside it; the box's test selector is `selector`.
fn copyable(selector: &'static str, value: &str, tip: &'static str, palette: Palette) -> gpui::Div {
    let copied = value.to_owned();
    let copy_id = format!("{selector}-copy");
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .debug_selector(move || selector.to_owned())
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.background))
                .font_family("monospace")
                .text_sm()
                .truncate()
                .child(value.to_owned()),
        )
        .child(selected_by(
            copy_id.clone(),
            icon_button(SharedString::from(copy_id), Glyph::COPY, tip).on_click(move |_, _, cx: &mut App| {
                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
            }),
        ))
}

impl AppDialog {
    fn segmented<T: Copy + 'static>(
        &self,
        group: &'static str,
        segments: Vec<Segment<T>>,
        set: fn(&mut Self, T, &mut Context<Self>),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let busy = self.busy;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_0p5()
            .p_0p5()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .children(segments.into_iter().map(|segment| {
                let id = SharedString::from(format!("{group}-{}", segment.key));
                let selector = id.to_string();
                let value = segment.value;
                div()
                    .debug_selector(move || selector.clone())
                    .id(id)
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .text_sm()
                    .cursor_pointer()
                    .when(segment.chosen, |segment| {
                        segment
                            .bg(rgb(palette.brand))
                            .text_color(rgb(palette.on_brand))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .when(!segment.chosen, |segment| {
                        segment.hover(move |segment| segment.bg(rgb(palette.hover)))
                    })
                    .child(segment.label)
                    .when(!busy, |segment| {
                        segment.on_click(cx.listener(move |this, _, _, cx| set(this, value, cx)))
                    })
            }))
    }

    fn field(selector: &'static str, input: &gpui::Entity<gpui_component::input::InputState>, busy: bool) -> gpui::Div {
        div()
            .debug_selector(move || selector.to_owned())
            .child(Input::new(input).disabled(busy))
    }

    fn mode_choice(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let segments = vec![
            Segment {
                key: "signed",
                label: "Signed",
                value: BotAuthMode::Signed,
                chosen: self.mode == BotAuthMode::Signed,
            },
            Segment {
                key: "unsigned",
                label: "Unsigned (development)",
                value: BotAuthMode::Unsigned,
                chosen: self.mode == BotAuthMode::Unsigned,
            },
        ];
        let mut rows = vec![
            label("Security"),
            div()
                .flex()
                .child(self.segmented("app-dialog-mode", segments, Self::set_mode, palette, cx)),
        ];
        rows.push(muted(
            if self.mode == BotAuthMode::Unsigned {
                UNSIGNED_WARNING
            } else {
                "Signed: the bot and the server check each other's tokens. The bot needs its app id and secret."
            },
            palette,
        ));
        rows
    }

    fn register_form(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let mut form = vec![
            muted(
                "The bot gets an app id and a secret. People find it under its name once it's on.",
                palette,
            ),
            label("Name"),
            Self::field("app-dialog-name", &self.name, self.busy),
            label("Description"),
            Self::field("app-dialog-description", &self.description, self.busy),
            label("Messaging endpoint"),
            Self::field("app-dialog-endpoint", &self.endpoint, self.busy),
            muted(
                "Where the server sends the bot's messages. You can add it later.",
                palette,
            ),
        ];
        form.extend(self.mode_choice(palette, cx));
        form
    }

    fn upload_form(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let chosen = self.package.as_ref().map_or_else(
            || "No file chosen".to_owned(),
            |package| format!("{} ({} KB)", package.name, package.bytes.len().div_ceil(1024)),
        );
        vec![
            muted(
                "Choose a Teams app package: a .zip with manifest.json and the app's two icons, at most 5 MB. Its \
                 bot gets a secret.",
                palette,
            ),
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(selected_by(
                    "app-dialog-browse",
                    Button::new("app-dialog-browse")
                        .outline()
                        .icon(Glyph::FOLDER.icon())
                        .label("Choose file")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, _, cx| this.browse(cx))),
                ))
                .child(
                    div()
                        .debug_selector(|| "app-dialog-package".to_owned())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(chosen),
                ),
        ]
    }

    fn made_form(&self, palette: Palette) -> Vec<gpui::Div> {
        let Some(app) = &self.app else {
            return Vec::new();
        };
        let mut form = Vec::new();
        match (&self.secret, &app.bot) {
            (Some(secret), Some(bot)) => {
                form.push(muted(
                    "Put the app id and the secret in the bot's settings. The secret isn't shown again: copy it now.",
                    palette,
                ));
                form.push(label("App id"));
                form.push(copyable("app-dialog-app-id", &bot.id, "Copy app id", palette));
                form.push(label("Secret"));
                form.push(copyable("app-dialog-secret", secret, "Copy secret", palette));
            }
            (None, Some(bot)) if bot.auth_mode == BotAuthMode::Unsigned => {
                form.push(muted("An unsigned bot needs no secret.", palette));
                form.push(label("App id"));
                form.push(copyable("app-dialog-app-id", &bot.id, "Copy app id", palette));
            }
            _ => {
                form.push(muted("This app has no bot.", palette));
                form.push(label("App id"));
                form.push(copyable("app-dialog-app-id", &app.id, "Copy app id", palette));
            }
        }
        form
    }

    fn details_header(app: &AdminApp, palette: Palette) -> gpui::Div {
        let source = match app.source {
            teams_auth::admin::AppSource::Package => "From an app package",
            _ => "Registered here",
        };
        let line = [app.developer.as_str(), source]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let version = (!app.version.is_empty()).then(|| format!("Version {}", app.version));
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(tile(app, 40.0, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(muted(line, palette).truncate())
                    .children(version.map(|version| muted(version, palette))),
            )
    }

    fn details_form(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let Some(app) = self.app.clone() else {
            return Vec::new();
        };
        let mut form = vec![Self::details_header(&app, palette)];
        if !app.short_description.is_empty() {
            form.push(div().text_sm().line_clamp(3).child(app.short_description.clone()));
        }
        form.push(label("App id"));
        form.push(copyable("app-dialog-app-id", &app.id, "Copy app id", palette));
        form.push(selected_by(
            "app-dialog-enabled",
            Checkbox::new("app-dialog-enabled")
                .label("On: people can use this app")
                .checked(self.choices.enabled)
                .disabled(self.busy)
                .on_click(cx.listener(|this, checked: &bool, _, cx| this.set_enabled(*checked, cx))),
        ));
        if let Some(bot) = &app.bot {
            if bot.id != app.id {
                form.push(label("Bot id"));
                form.push(copyable("app-dialog-bot-id", &bot.id, "Copy bot id", palette));
            }
            form.push(label("Messaging endpoint"));
            form.push(Self::field("app-dialog-endpoint", &self.endpoint, self.busy));
            form.extend(self.mode_choice(palette, cx));
            form.push(Self::delivery(&app, palette));
            form.push(self.test_row(&app, palette, cx));
        }
        form.push(self.access_summary(&app, palette, cx));
        form
    }

    /// How the server's deliveries to the app's bot go.
    fn delivery(app: &AdminApp, palette: Palette) -> gpui::Div {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX));
        let (summary, attention) = super::health::summary(app.health.as_ref(), now);
        let counts = super::health::counts(app.health.as_ref());
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(label("Delivery"))
            .child(
                div()
                    .debug_selector(|| "app-dialog-health".to_owned())
                    .flex()
                    .items_start()
                    .gap_1p5()
                    .text_sm()
                    .text_color(rgb(if attention { theme::DANGER } else { palette.foreground }))
                    .when(attention, |line| {
                        line.child(Glyph::CIRCLE_ALERT.icon().size(px(14.0)).flex_none().mt_0p5())
                    })
                    .child(div().flex_1().min_w_0().child(summary)),
            )
            .children(
                counts.map(|counts| muted(counts, palette).debug_selector(|| "app-dialog-health-counts".to_owned())),
            )
    }

    /// "Send a test activity" and how the bot answered the last one.
    fn test_row(&self, app: &AdminApp, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let ready = super::dialog::ready_to_test(app);
        let testing = self.testing.is_some();
        let said = match &self.test {
            Some(result) => {
                let (words, took) = super::health::went(result);
                let color = if took { theme::SUCCESS } else { theme::DANGER };
                Some(
                    div()
                        .debug_selector(|| "app-dialog-test-result".to_owned())
                        .text_sm()
                        .text_color(rgb(color))
                        .child(words),
                )
            }
            None if !ready => Some(muted("Turn the app on and set its endpoint to test it.", palette)),
            None => None,
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(label("Test the bot"))
                    .child(muted("Sends the bot a test event and shows how it answers.", palette))
                    .children(said),
            )
            .child(selected_by(
                "app-dialog-test",
                Button::new("app-dialog-test")
                    .outline()
                    .label("Send a test activity")
                    .spinning(testing)
                    .disabled(!self.can_test())
                    .on_click(cx.listener(|this, _, _, cx| this.send_test(cx))),
            ))
    }

    fn access_summary(&self, app: &AdminApp, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let summary = if app.everyone {
            "Everyone on this server".to_owned()
        } else {
            match app.users.len() {
                0 => "No one yet".to_owned(),
                1 => "1 person".to_owned(),
                count => format!("{count} people"),
            }
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(label("Who can use it"))
                    .child(muted(summary, palette).debug_selector(|| "app-dialog-access-summary".to_owned())),
            )
            .child(selected_by(
                "app-dialog-access",
                Button::new("app-dialog-access")
                    .outline()
                    .label("Change")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.go(DialogKind::Access, window, cx))),
            ))
    }

    fn person_row(
        (id, name, upn): (&str, String, Option<String>),
        add: Option<AdminUser>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let key = upn.clone().unwrap_or_else(|| id.to_owned());
        let shown: SharedString = name.clone().into();
        let button = if let Some(person) = add {
            let button_id = SharedString::from(format!("app-dialog-add-{key}"));
            selected_by(
                button_id.to_string(),
                icon_button(button_id, Glyph::PLUS, "Add")
                    .on_click(cx.listener(move |this, _, _, cx| this.add_person(&person, cx))),
            )
        } else {
            let button_id = SharedString::from(format!("app-dialog-remove-{key}"));
            let id = id.to_owned();
            selected_by(
                button_id.to_string(),
                icon_button(button_id, Glyph::X, "Remove")
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_person(&id, cx))),
            )
        };
        div()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_3()
            .child(crate::people::avatar(Some(&shown), None, gpui_component::Size::Small))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_sm().truncate().child(name))
                    .children(upn.map(|upn| muted(upn, palette).text_xs().truncate())),
            )
            .child(button)
    }

    fn access_form(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let segments = vec![
            Segment {
                key: "everyone",
                label: "Everyone",
                value: true,
                chosen: self.choices.everyone,
            },
            Segment {
                key: "specific",
                label: "Specific people",
                value: false,
                chosen: !self.choices.everyone,
            },
        ];
        let mut form =
            vec![
                div()
                    .flex()
                    .child(self.segmented("app-dialog-who", segments, Self::set_everyone, palette, cx)),
            ];
        if self.choices.everyone {
            form.push(muted("Every account on this server can add and use the app.", palette));
            return form;
        }
        let chosen: Vec<gpui::Div> = self
            .users
            .iter()
            .map(|id| {
                let (name, upn) = self.person_name(id);
                Self::person_row((id, name, upn), None, palette, cx)
            })
            .collect();
        form.push(label("People who can use it"));
        form.push(if chosen.is_empty() {
            muted("No one yet. Search for people to add them.", palette)
        } else {
            div()
                .debug_selector(|| "app-dialog-people".to_owned())
                .flex()
                .flex_col()
                .children(chosen)
        });
        form.push(Self::field("app-dialog-people-search", &self.people_search, self.busy));
        let found: Vec<gpui::Div> = self
            .found
            .iter()
            .filter(|person| !self.users.contains(&person.id))
            .map(|person| {
                let name = if person.display_name.is_empty() {
                    person.upn.clone()
                } else {
                    person.display_name.clone()
                };
                Self::person_row(
                    (&person.id, name, Some(person.upn.clone())),
                    Some(person.clone()),
                    palette,
                    cx,
                )
            })
            .collect();
        if !found.is_empty() {
            form.push(
                div()
                    .debug_selector(|| "app-dialog-found".to_owned())
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .children(found),
            );
        }
        form
    }

    fn settings_form(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let segments = Sdk::ALL
            .into_iter()
            .map(|sdk| Segment {
                key: sdk.key(),
                label: sdk.label(),
                value: sdk,
                chosen: self.sdk == sdk,
            })
            .collect();
        let mut form = vec![
            div()
                .flex()
                .child(self.segmented("app-dialog-sdk", segments, Self::set_sdk, palette, cx)),
        ];
        match (&self.connection, self.settings()) {
            (Some(Err(problem)), _) => form.push(muted(problem.words(), palette)),
            (_, Some(settings)) => {
                form.push(muted(format!("Put these in {}.", settings.place), palette));
                if let Some(note) = settings.note {
                    form.push(muted(note, palette).debug_selector(|| "app-dialog-sdk-note".to_owned()));
                }
                if self.secret.is_none()
                    && self
                        .app
                        .as_ref()
                        .and_then(|app| app.bot.as_ref())
                        .is_some_and(|bot| bot.auth_mode == BotAuthMode::Signed)
                {
                    form.push(muted(
                        "Use the secret you got when the bot was made, or give it a new secret.",
                        palette,
                    ));
                }
                form.push(Self::code_block(&settings.text, palette));
            }
            _ => form.push(muted("Reading the server's addresses…", palette).child(Spinner::new())),
        }
        form
    }

    fn code_block(text: &str, palette: Palette) -> gpui::Div {
        let copied = text.to_owned();
        div()
            .relative()
            .child(
                div()
                    .debug_selector(|| "app-dialog-sdk-text".to_owned())
                    .p_3()
                    .pr_10()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .font_family("monospace")
                    .text_xs()
                    .children(text.lines().map(|line| div().child(line.to_owned()))),
            )
            .child(div().absolute().top_1().right_1().child(selected_by(
                "app-dialog-sdk-copy",
                icon_button("app-dialog-sdk-copy", Glyph::COPY, "Copy settings").on_click(move |_, _, cx: &mut App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                }),
            )))
    }

    /// The dialog's title and its confirming button's word.
    fn words(&self) -> (String, &'static str) {
        let name = self.app.as_ref().map(|app| short(&app.name)).unwrap_or_default();
        match self.kind {
            DialogKind::Register => ("Register a bot".to_owned(), "Register"),
            DialogKind::Upload => ("Upload an app".to_owned(), "Upload"),
            DialogKind::Made => match self.made {
                Some(Made::NewSecret) => (format!("New secret for {name}"), "Done"),
                Some(Made::Uploaded) => (format!("{name} added"), "Done"),
                _ => (format!("{name} registered"), "Done"),
            },
            DialogKind::Details => (name, "Save"),
            DialogKind::Access => (format!("Who can use {name}"), "Save"),
            DialogKind::NewSecret => (format!("Give {name} a new secret?"), "New secret"),
            DialogKind::Delete => (format!("Delete {name}?"), "Delete"),
            DialogKind::Settings => (format!("Bot settings for {name}"), "Done"),
        }
    }

    /// An app's other actions, at the left of the buttons: on its details, and after its bot was made.
    fn more_actions(&self, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let has_bot = self.app.as_ref().is_some_and(|app| app.bot.is_some());
        let settings = selected_by(
            "app-dialog-sdk-settings",
            Button::new("app-dialog-sdk-settings")
                .outline()
                .icon(Glyph::CODE.icon())
                .label("Bot settings")
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, window, cx| this.go(DialogKind::Settings, window, cx))),
        );
        match self.kind {
            DialogKind::Made if has_bot => vec![settings],
            DialogKind::Details => {
                let mut actions = Vec::new();
                if has_bot {
                    actions.push(settings);
                    actions.push(selected_by(
                        "app-dialog-new-secret",
                        Button::new("app-dialog-new-secret")
                            .outline()
                            .label("New secret")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.go(DialogKind::NewSecret, window, cx))),
                    ));
                }
                actions.push(selected_by(
                    "app-dialog-delete",
                    Button::new("app-dialog-delete")
                        .danger()
                        .label("Delete app")
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.go(DialogKind::Delete, window, cx))),
                ));
                actions
            }
            _ => Vec::new(),
        }
    }

    fn footer(&self, confirm: &'static str, cx: &mut Context<Self>) -> gpui::Div {
        let can_confirm = self.can_confirm(cx);
        let confirming = Button::new("app-dialog-confirm")
            .label(confirm)
            .disabled(!can_confirm)
            .tab_stop(can_confirm)
            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)));
        let confirming = if matches!(self.kind, DialogKind::Delete | DialogKind::NewSecret) {
            confirming.danger()
        } else {
            confirming.primary()
        };
        let cancel = self.kind != DialogKind::Made;
        let cancel_word = if self.back.is_some() { "Back" } else { "Cancel" };
        buttons()
            .flex_wrap()
            .children(self.more_actions(cx))
            .child(div().flex_1())
            .when(cancel, |row| {
                row.child(selected_by(
                    "app-dialog-cancel",
                    Button::new("app-dialog-cancel")
                        .outline()
                        .label(cancel_word)
                        .disabled(self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                ))
            })
            .child(selected_by("app-dialog-confirm", confirming))
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key.as_str() == "escape" {
            cx.stop_propagation();
            // the write under way is waited for: its answer still reaches the page
            if !self.busy {
                cx.emit(AppDialogEvent::Closed);
            }
        } else if event.keystroke.key.as_str() == "enter"
            && self.kind == DialogKind::Made
            && self.focus.is_focused(window)
        {
            // only on the dialog itself: Enter on a focused button (Copy secret) does what that button does
            cx.stop_propagation();
            cx.emit(AppDialogEvent::Closed);
        }
    }

    fn form(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        match self.kind {
            DialogKind::Register => self.register_form(palette, cx),
            DialogKind::Upload => self.upload_form(palette, cx),
            DialogKind::Made => self.made_form(palette),
            DialogKind::Details => self.details_form(palette, cx),
            DialogKind::Access => self.access_form(palette, cx),
            DialogKind::Settings => self.settings_form(palette, cx),
            DialogKind::NewSecret => vec![
                div()
                    .text_sm()
                    .child("The bot's current secret stops working at once. Put the new one in the bot's settings."),
            ],
            DialogKind::Delete => vec![div().text_sm().child(
                "People can't use it any more and its bot gets no more messages. What it posted stays. This can't \
                 be undone.",
            )],
        }
    }

    fn problem_box(words: SharedString) -> gpui::Div {
        div()
            .debug_selector(|| "app-dialog-problem".to_owned())
            .w_full()
            .flex()
            .items_start()
            .gap_2()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme::DANGER))
            .child(
                Glyph::CIRCLE_ALERT
                    .icon()
                    .size(px(16.0))
                    .flex_none()
                    .text_color(rgb(theme::DANGER)),
            )
            .child(problem("app-dialog-problem-words", words).text_sm().flex_1().min_w_0())
    }
}

impl Render for AppDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        if std::mem::take(&mut self.focus_dialog) {
            self.focus.focus(window, cx);
        }
        if std::mem::take(&mut self.focus_first_button) {
            // the buttons' own focus exists once they are drawn: the safe choice, Cancel, comes first in the trap
            let root = self.focus.clone();
            window.defer(cx, move |window, cx| keyboard::move_within(&root, true, window, cx));
        }
        let (heading, confirm) = self.words();
        let form: Vec<AnyElement> = self
            .form(palette, cx)
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect();
        let footer = self.footer(confirm, cx);
        let key = self.kind.key();
        surface("app-dialog", palette, window)
            .w(px(560.0))
            .key_context("AppDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .child(
                title(heading)
                    .debug_selector(|| "app-dialog-title".to_owned())
                    .line_clamp(2)
                    .text_ellipsis(),
            )
            .child(
                div()
                    .id("app-dialog-body")
                    .debug_selector(move || format!("app-dialog-{key}"))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(form),
            )
            .children(self.problem.clone().map(Self::problem_box))
            .child(footer)
            .focus_trap("app-dialog-trap", &self.focus)
    }
}

/// The secret a dialog shows, for a test to compare with what the server made.
#[must_use]
pub fn shown_secret(dialog: &AppDialog) -> Option<&str> {
    dialog.secret.as_deref().filter(|_| dialog.kind == DialogKind::Made)
}
