//! A team's outgoing webhooks in Manage team's Apps tab, over a fake core (`docs/protocol/bots.md`, "Outgoing
//! webhooks"): "Create an outgoing webhook" for an owner while the bots are shown and the cloud serves webhooks; the
//! create dialog, its checks and the token shown once with Copy; a webhook's row with its picture, its Edit that sends
//! only what changed, and its Remove that asks first. Every id, name and token is made up.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::TeamWritten;
use teams_proto::{CreatedWebhook, OutgoingWebhook, TeamWrite, WebhookForm, WebhookPicture};
use teams_store::{ChannelKind, Team, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::bots_flag::BotsFlag;
use teams_ui::manage_team::ManageTab;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Shell, ShellParts};
use teams_ui::webhook_dialog::{NAME_TAKEN, NOT_HTTPS};

const TEAM: &str = "19:team0001@thread.tacv2";
const HOOK: &str = "00000000-0000-4000-8000-0000000b0001";
/// A made-up token: base64, no real key.
const TOKEN: &str = "TWFkZVVwVG9rZW4=";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\nmade-up";

fn team(owner: bool) -> Team {
    Team {
        id: TEAM.to_owned(),
        name: "Alpha".to_owned(),
        group_id: Some("00000000-0000-4000-8000-0000000000aa".to_owned()),
        owner,
        channels: vec![TeamChannel {
            id: TEAM.to_owned(),
            name: "General".to_owned(),
            general: true,
            kind: ChannelKind::Standard,
            about: teams_store::ChannelAbout::default(),
            shown: true,
        }],
        ..Team::default()
    }
}

fn hook(name: &str, can_manage: bool, picture: bool) -> OutgoingWebhook {
    let body = serde_json::json!({"value": [{
        "id": HOOK, "teamId": TEAM, "name": name, "description": "Tells the state of a build.",
        "canManage": can_manage, "callbackUrl": "https://hooks.example.test/build",
        "pictureUrl": picture.then(|| format!("https://mt.example.test/beta/teams/{TEAM}/outgoingWebhooks/{HOOK}/picture?v=1")),
    }]});
    OutgoingWebhook::list_answer(body.to_string().as_bytes()).remove(0)
}

fn window(rig: &Rig, bots: bool, owner: bool) -> (Entity<Shell>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(|cx| cx.set_global(BotsFlag(bots)));
    *rig.core.teams.lock().unwrap() = Some(vec![team(owner)]);
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_manage_team(TEAM.to_owned(), ManageTab::Apps, window, cx);
        });
    });
    cx.run_until_parked();
    (shell, cx)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_mouse_move(found.center(), None, Modifiers::none());
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn type_in(cx: &mut VisualTestContext, selector: &str, text: &str) {
    click(cx, selector);
    cx.simulate_input(text);
    cx.run_until_parked();
}

fn writes(rig: &Rig) -> Vec<TeamWrite> {
    rig.core
        .team_writes
        .lock()
        .unwrap()
        .iter()
        .map(|(write, _)| write.clone())
        .collect()
}

#[test]
fn an_owner_is_offered_create_only_while_the_bots_are_shown_and_the_cloud_serves_webhooks() {
    let rig = Rig::start();
    let (_shell, cx) = window(&rig, true, true);
    assert!(bounds(cx, "manage-create-webhook").is_some());

    let rig = Rig::start();
    let (_shell, cx) = window(&rig, false, true);
    assert!(bounds(cx, "manage-create-webhook").is_none(), "behind NGTEAMS_BOTS");

    let rig = Rig::start();
    let (_shell, cx) = window(&rig, true, false);
    assert!(bounds(cx, "manage-create-webhook").is_none(), "a member makes none");

    let rig = Rig::start();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        outgoing_webhooks: false,
        ..teams_core::Capabilities::ALL
    });
    let (_shell, cx) = window(&rig, true, true);
    assert!(
        bounds(cx, "manage-create-webhook").is_none(),
        "an own server that doesn't serve them"
    );

    // an own server that serves webhooks but no app catalogue: the tab shows for the webhooks alone
    let rig = Rig::start();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        apps: false,
        ..teams_core::Capabilities::ALL
    });
    rig.core
        .outgoing_webhooks
        .lock()
        .unwrap()
        .insert(TEAM.to_owned(), vec![hook("Build bot", true, false)]);
    let (_shell, cx) = window(&rig, true, true);
    assert!(bounds(cx, "manage-tab-apps").is_some());
    assert!(bounds(cx, "manage-create-webhook").is_some());
    assert!(bounds(cx, "manage-get-apps").is_none(), "no store to get apps from");
    assert!(bounds(cx, "manage-webhook-0").is_some());
}

#[test]
fn create_sends_the_form_then_shows_the_token_once_with_copy() {
    let rig = Rig::start();
    let (_shell, cx) = window(&rig, true, true);
    click(cx, "manage-create-webhook");
    assert!(bounds(cx, "webhook-dialog").is_some());
    assert!(bounds(cx, "webhook-send").is_some());
    type_in(cx, "webhook-name", "Build bot");
    type_in(cx, "webhook-url", "http://hooks.example.test/build");
    click(cx, "webhook-send");
    assert!(writes(&rig).is_empty(), "an address that isn't HTTPS is not sent");
    assert!(bounds(cx, "webhook-problem").is_some(), "{NOT_HTTPS}");

    click(cx, "webhook-url");
    cx.simulate_keystrokes("ctrl-a backspace");
    cx.simulate_input("https://hooks.example.test/build");
    type_in(cx, "webhook-description", "Builds");
    let created = serde_json::json!({"value": {"id": HOOK, "teamId": TEAM, "name": "Build bot", "canManage": true,
        "securityToken": TOKEN}});
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten {
        created_webhook: CreatedWebhook::read_answer(created.to_string().as_bytes()),
        ..TeamWritten::default()
    });
    click(cx, "webhook-send");
    assert_eq!(
        writes(&rig),
        [TeamWrite::CreateOutgoingWebhook {
            team: TEAM.to_owned(),
            form: WebhookForm {
                name: Some("Build bot".to_owned()),
                callback_url: Some("https://hooks.example.test/build".to_owned()),
                description: Some("Builds".to_owned()),
                picture: WebhookPicture::Keep,
            },
        }]
    );
    assert!(bounds(cx, "webhook-token").is_some(), "the token shows");
    click(cx, "webhook-token-copy");
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some(TOKEN));
    // the list the page reads again once the dialog closes
    rig.core
        .outgoing_webhooks
        .lock()
        .unwrap()
        .insert(TEAM.to_owned(), vec![hook("Build bot", true, false)]);
    click(cx, "webhook-close");
    assert!(bounds(cx, "webhook-dialog").is_none());
    assert!(bounds(cx, "manage-webhook-0").is_some(), "listed once created");
}

#[test]
fn a_name_the_team_has_is_told_in_teams_words() {
    let rig = Rig::start();
    *rig.core.team_refusal.lock().unwrap() = Some("OutgoingWebhookNameAlreadyExists".to_owned());
    let (_shell, cx) = window(&rig, true, true);
    click(cx, "manage-create-webhook");
    type_in(cx, "webhook-name", "Build bot");
    type_in(cx, "webhook-url", "https://hooks.example.test/build");
    click(cx, "webhook-send");
    assert_eq!(writes(&rig).len(), 1);
    assert!(bounds(cx, "webhook-problem").is_some(), "{NAME_TAKEN}");
    assert!(bounds(cx, "webhook-token").is_none());
}

#[test]
fn a_webhooks_row_shows_its_picture_edit_sends_what_changed_and_remove_asks_first() {
    let rig = Rig::start();
    rig.core
        .outgoing_webhooks
        .lock()
        .unwrap()
        .insert(TEAM.to_owned(), vec![hook("Build bot", true, true)]);
    rig.core
        .webhook_pictures
        .lock()
        .unwrap()
        .insert(HOOK.to_owned(), PNG.to_vec());
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (_shell, cx) = window(&rig, true, true);
    assert!(bounds(cx, "manage-webhook-0").is_some());

    click(cx, "manage-webhook-more-0");
    click(cx, "manage-webhook-edit");
    assert!(bounds(cx, "webhook-dialog").is_some());
    assert!(bounds(cx, "webhook-picture-remove").is_some(), "it has a picture");
    type_in(cx, "webhook-name", "s");
    click(cx, "webhook-picture-remove");
    click(cx, "webhook-send");
    assert_eq!(
        writes(&rig),
        [TeamWrite::ChangeOutgoingWebhook {
            team: TEAM.to_owned(),
            webhook: HOOK.to_owned(),
            form: WebhookForm {
                name: Some("Build bots".to_owned()),
                picture: WebhookPicture::Remove,
                ..WebhookForm::default()
            },
        }],
        "only what changed"
    );
    assert!(bounds(cx, "webhook-dialog").is_none(), "Save closes it");

    rig.core.team_writes.lock().unwrap().clear();
    click(cx, "manage-webhook-more-0");
    click(cx, "manage-webhook-remove");
    assert!(writes(&rig).is_empty(), "it asks first");
    rig.core.outgoing_webhooks.lock().unwrap().clear();
    click(cx, "manage-webhook-remove-confirm");
    assert_eq!(
        writes(&rig),
        [TeamWrite::RemoveOutgoingWebhook {
            team: TEAM.to_owned(),
            webhook: HOOK.to_owned(),
        }]
    );
    assert!(bounds(cx, "manage-webhook-0").is_none());
}

#[test]
fn a_webhook_others_manage_has_no_menu_and_the_read_only_mode_offers_nothing() {
    let rig = Rig::start();
    rig.core
        .outgoing_webhooks
        .lock()
        .unwrap()
        .insert(TEAM.to_owned(), vec![hook("Lunch", false, false)]);
    let (_shell, cx) = window(&rig, true, true);
    assert!(bounds(cx, "manage-webhook-0").is_some());
    assert!(bounds(cx, "manage-webhook-more-0").is_none());

    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    rig.core
        .outgoing_webhooks
        .lock()
        .unwrap()
        .insert(TEAM.to_owned(), vec![hook("Build bot", true, false)]);
    let (_shell, cx) = window(&rig, true, true);
    assert!(bounds(cx, "manage-webhook-0").is_some());
    assert!(bounds(cx, "manage-webhook-more-0").is_none());
    assert!(bounds(cx, "manage-create-webhook").is_none());
}

fn message(id: i64, sender: &str, name: &str) -> teams_store::Message {
    teams_store::Message {
        conversation_id: TEAM.to_owned(),
        id: id.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some(name.to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: "<p>made up</p>".to_owned(),
        properties: None,
        arrival_time: id,
        compose_time: Some(id),
        edit_time: None,
        delete_time: None,
    }
}

fn in_channel(bots: bool) -> (Rig, Entity<Shell>, &'static mut VisualTestContext) {
    let rig = Rig::start();
    rig.core
        .outgoing_webhooks
        .lock()
        .unwrap()
        .insert(TEAM.to_owned(), vec![hook("Build bot", true, true)]);
    rig.core
        .webhook_pictures
        .lock()
        .unwrap()
        .insert(HOOK.to_owned(), PNG.to_vec());
    rig.core.messages.lock().unwrap().insert(
        TEAM.to_owned(),
        vec![
            message(100, &format!("28:{HOOK}"), "Build bot"),
            message(101, common::LAB_B, "Lab Bee"),
        ],
    );
    let (shell, cx) = window(&rig, bots, true);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_channel_id(TEAM.to_owned(), window, cx)));
    cx.run_until_parked();
    (rig, shell, cx)
}

fn posts_box(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<teams_ui::compose::Compose> {
    let channel = cx
        .update(|_, cx| shell.read(cx).channel().cloned())
        .expect("the channel");
    cx.update(|_, cx| channel.read(cx).posts().read(cx).compose().clone())
}

#[test]
fn a_webhooks_reply_carries_its_name_and_the_app_mark() {
    let (_rig, _shell, cx) = in_channel(true);
    assert!(bounds(cx, "app-mark-100").is_some(), "the webhook's message");
    assert!(bounds(cx, "app-mark-101").is_none(), "a person's");

    let (_rig, _shell, cx) = in_channel(false);
    assert!(bounds(cx, "app-mark-100").is_none(), "behind NGTEAMS_BOTS");
}

#[test]
fn the_channels_picker_offers_the_teams_webhook_and_sends_a_bots_mention() {
    let (rig, shell, cx) = in_channel(true);
    let compose = posts_box(&shell, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    cx.simulate_input("Ship it? @Bu");
    cx.run_until_parked();
    let offered: Vec<String> = cx.update(|_, cx| {
        compose
            .read(cx)
            .picker()
            .map(|picker| {
                picker
                    .matches
                    .iter()
                    .map(|candidate| candidate.name.to_string())
                    .collect()
            })
            .unwrap_or_default()
    });
    assert_eq!(offered, ["Build bot"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let actions = std::mem::take(&mut *rig.core.actions.lock().unwrap());
    let Some(teams_core::Action::Send { text, .. }) = actions.first() else {
        panic!("a post: {actions:?}");
    };
    let bot = teams_proto::Mri::new(format!("28:{HOOK}"));
    assert_eq!(
        text.bots().get(&bot).map(String::as_str),
        Some(HOOK),
        "its id as the app's"
    );
    assert!(text.participants().contains(&bot));
}
