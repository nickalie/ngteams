//! The Apps tab's outgoing webhooks (`docs/protocol/bots.md`, "Outgoing webhooks"), while the account's cloud serves
//! them ([`teams_core::Capabilities::outgoing_webhooks`]): "Create an outgoing webhook" for an owner, and a row per webhook of the team ([`Core::outgoing_webhooks`]) with its
//! picture, its name and "Outgoing webhook". For an owner, a webhook the server lets the account manage has a "..."
//! with Edit and Remove; Remove asks first.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AsyncApp, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use teams_proto::{OUTGOING_WEBHOOK_MAKER, OutgoingWebhook, TeamWrite};
use teams_store::Team;

use super::{ManageTeam, ManageTeamEvent};
use crate::bot_face::{self, BotApp};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::shell::menu_row;
use crate::teams_page::{popup_menu, with_popup};
use crate::theme::Palette;

/// A webhook as the tab lists it: its entry and its face (its picture once read).
#[derive(Clone, Debug)]
pub struct ShownWebhook {
    pub hook: OutgoingWebhook,
    pub face: BotApp,
}

impl ManageTeam {
    /// Whether the tab shows the team's outgoing webhooks.
    pub(super) fn webhooks_on(&self) -> bool {
        self.core.capabilities().outgoing_webhooks
    }

    /// Whether Manage team has an Apps tab: for the app catalogue, or for the outgoing webhooks.
    pub(super) fn apps_shown_at_all(&self) -> bool {
        self.core.capabilities().apps || self.webhooks_on()
    }

    /// Reads the team's webhooks, then their pictures.
    pub(super) fn read_webhooks(&mut self, cx: &mut Context<Self>) {
        if !self.webhooks_on() {
            self.webhooks = Some(Vec::new());
            return;
        }
        let core = std::sync::Arc::clone(&self.core);
        let team = self.team.clone();
        self.reading
            .push(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let hooks = core.outgoing_webhooks(team).await.unwrap_or_default();
                let mut shown = Vec::new();
                for hook in hooks {
                    let face = bot_face::with_picture(core.as_ref(), hook.app()).await;
                    shown.push(ShownWebhook { hook, face });
                }
                let _ = this.update(cx, |this, cx| {
                    this.webhooks = Some(shown);
                    cx.notify();
                });
            }));
    }

    /// The team's webhooks, as last read.
    #[must_use]
    pub fn webhooks(&self) -> Vec<OutgoingWebhook> {
        self.webhooks.iter().flatten().map(|shown| shown.hook.clone()).collect()
    }

    fn webhooks_found(&self) -> Vec<ShownWebhook> {
        let query = self.app_query.to_lowercase();
        self.webhooks
            .iter()
            .flatten()
            .filter(|shown| query.is_empty() || shown.hook.name.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    /// "Create an outgoing webhook", for an owner of a team that isn't archived.
    pub(super) fn create_webhook_button(&self, team: &Team, cx: &mut Context<Self>) -> Option<AnyElement> {
        let can = self.webhooks_on() && team.owner && !team.archived && !self.core.read_only();
        let team = team.clone();
        can.then(|| {
            selected_by(
                "manage-create-webhook",
                Button::new("manage-create-webhook")
                    .outline()
                    .icon(Glyph::LINK.icon())
                    .label("Create an outgoing webhook")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ManageTeamEvent::Webhook(team.clone(), None));
                    })),
            )
            .into_any_element()
        })
    }

    /// A webhook's Remove: asks first.
    pub fn ask_remove_webhook(&mut self, hook: OutgoingWebhook, cx: &mut Context<Self>) {
        self.webhook_menu = None;
        self.removing_webhook = Some(hook);
        cx.notify();
    }

    /// The question's Remove: the webhook off the team, gone from the list at once.
    pub fn remove_webhook(&mut self, cx: &mut Context<Self>) {
        let Some(hook) = self.removing_webhook.take() else {
            return;
        };
        if let Some(listed) = &mut self.webhooks {
            listed.retain(|shown| shown.hook.id != hook.id);
        }
        self.write(
            TeamWrite::RemoveOutgoingWebhook {
                team: self.team.clone(),
                webhook: hook.id,
            },
            "We couldn't remove the outgoing webhook. Please try again.",
            cx,
        );
        cx.notify();
    }

    /// The question's Cancel.
    pub fn keep_webhook(&mut self, cx: &mut Context<Self>) {
        self.removing_webhook = None;
        cx.notify();
    }

    fn webhook_menu_of(
        &self,
        team: &Team,
        hook: &OutgoingWebhook,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> crate::teams_page::PopupMenu {
        let (edited, removed, team) = (hook.clone(), hook.clone(), team.clone());
        popup_menu(
            palette,
            cx.listener(|this, _, _, cx| {
                this.webhook_menu = None;
                cx.notify();
            }),
        )
        .keyed(self.popup.root(), self.popup_keys(cx))
        .row(
            "manage-webhook-edit",
            menu_row("manage-webhook-edit", palette)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.popup.closed(window, cx);
                    this.webhook_menu = None;
                    cx.emit(ManageTeamEvent::Webhook(team.clone(), Some(Box::new(edited.clone()))));
                }))
                .child(Glyph::PENCIL.icon().size(px(14.0)))
                .child("Edit"),
            palette,
        )
        .row(
            "manage-webhook-remove",
            menu_row("manage-webhook-remove", palette)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.popup.closed(window, cx);
                    this.ask_remove_webhook(removed.clone(), cx);
                }))
                .child(Glyph::TRASH.icon().size(px(14.0)))
                .child("Remove"),
            palette,
        )
    }

    fn webhook_row(
        &self,
        at: usize,
        (team, shown): (&Team, &ShownWebhook),
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hook = &shown.hook;
        // Create is an owner's, so Edit and Remove are too, also where the server lets members manage.
        let can_write = team.owner && hook.can_manage && !team.archived && !self.core.read_only();
        let menu_open = self.webhook_menu.as_deref() == Some(hook.id.as_str());
        let menu = menu_open.then(|| self.webhook_menu_of(team, hook, palette, cx));
        let id = hook.id.clone();
        let line = if hook.description.is_empty() {
            OUTGOING_WEBHOOK_MAKER.to_owned()
        } else {
            format!("{OUTGOING_WEBHOOK_MAKER} · {}", hook.description)
        };
        div()
            .debug_selector(move || format!("manage-webhook-{at}"))
            .px_4()
            .py_2p5()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(bot_face::tile(Some(&shown.face), 36.0, palette))
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
                            .child(hook.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(line),
                    ),
            )
            .when(can_write, |row| {
                row.child(with_popup(
                    selected_by(
                        format!("manage-webhook-more-{at}"),
                        icon_button(
                            SharedString::from(format!("manage-webhook-more-{at}")),
                            Glyph::MORE,
                            "More options",
                        )
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let open = this.webhook_menu.as_deref() != Some(id.as_str());
                            this.webhook_menu = open.then(|| id.clone());
                            this.popup_toggled(open, window, cx);
                            cx.notify();
                        })),
                    ),
                    menu,
                ))
            })
            .into_any_element()
    }

    /// A row per webhook whose name holds what Find by name holds.
    pub(super) fn webhook_rows(&self, team: &Team, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.webhooks_found()
            .iter()
            .enumerate()
            .map(|(at, shown)| self.webhook_row(at, (team, shown), palette, cx))
            .collect()
    }

    /// The question before a webhook goes.
    pub(super) fn webhook_question(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let hook = self.removing_webhook.as_ref()?;
        Some(
            div()
                .debug_selector(|| "manage-webhook-remove-question".to_owned())
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
                        .child(format!("Remove {}", hook.name)),
                )
                .child(div().text_sm().child(format!(
                    "Are you sure you want to remove {}? People can't mention it in this team's channels anymore.",
                    hook.name
                )))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(selected_by(
                            "manage-webhook-remove-cancel",
                            Button::new("manage-webhook-remove-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.keep_webhook(cx))),
                        ))
                        .child(selected_by(
                            "manage-webhook-remove-confirm",
                            Button::new("manage-webhook-remove-confirm")
                                .primary()
                                .label("Remove")
                                .on_click(cx.listener(|this, _, _, cx| this.remove_webhook(cx))),
                        )),
                ),
        )
    }
}
