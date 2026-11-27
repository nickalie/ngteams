//! A team's outgoing webhooks on the middle tier (`docs/protocol/bots.md`, "Outgoing webhooks"): the list and the
//! create answer read from the synthetic fixtures `tests/fixtures/mt/outgoing-webhook*.json`, and each request's
//! wire. JSON is compared as parsed values.

use serde_json::{Value, json};
use teams_proto::{
    CreatedWebhook, Method, OutgoingWebhook, OutgoingWebhookPicture, OutgoingWebhooks, Read, Request, Service,
    TeamWrite, WebhookForm, WebhookPicture,
};

const LIST: &str = include_str!("../../../tests/fixtures/mt/outgoing-webhooks.json");
const CREATED: &str = include_str!("../../../tests/fixtures/mt/outgoing-webhook-created.json");
const TEAM: &str = "19:synthetic-team@thread.tacv2";
const HOOK: &str = "made-up-hook-1";

#[test]
fn the_list_reads_each_webhook_with_a_name_and_an_id() {
    let hooks = OutgoingWebhook::list_answer(LIST.as_bytes());
    assert_eq!(hooks.len(), 2, "entries without an id or a name are left out");
    let build = &hooks[0];
    assert_eq!(build.id, HOOK);
    assert_eq!(build.mri, format!("28:{HOOK}"));
    assert_eq!(build.team, TEAM);
    assert_eq!(build.name, "Build bot");
    assert_eq!(build.description, "Tells the team the state of a build.");
    assert!(build.can_manage);
    assert_eq!(build.created_by, "8:orgid:made-up-a");
    assert_eq!(build.callback_url.as_deref(), Some("https://hooks.example.test/build"));
    assert_eq!(build.picture_version(), Some("made-up-picture-1"));
}

#[test]
fn a_member_sees_no_address_and_a_body_without_a_list_is_none() {
    let hooks = OutgoingWebhook::list_answer(LIST.as_bytes());
    let lunch = &hooks[1];
    assert!(!lunch.can_manage);
    assert_eq!(lunch.callback_url, None, "only a manager sees the address");
    assert_eq!(lunch.picture_version(), None);
    assert!(OutgoingWebhook::list_answer(b"not json").is_empty());
    assert!(OutgoingWebhook::list_answer(b"{}").is_empty());
}

#[test]
fn a_webhooks_debug_leaves_its_address_out() {
    let hooks = OutgoingWebhook::list_answer(LIST.as_bytes());
    let shown = format!("{:?}", hooks[0]);
    assert!(!shown.contains("hooks.example.test"), "{shown}");
}

#[test]
fn the_create_answer_gives_the_webhook_and_its_token_once() {
    let created = CreatedWebhook::read_answer(CREATED.as_bytes()).expect("a created webhook");
    assert_eq!(created.webhook.name, "Release notes");
    assert_eq!(created.webhook.mri, "28:made-up-hook-3");
    assert_eq!(created.token.as_str(), "U3ludGhldGljVG9rZW5Ob3RBUmVhbEtleQ==");
    let shown = format!("{created:?}");
    assert!(!shown.contains("U3ludGhl"), "the token never reaches a log: {shown}");
    let mut without: Value = serde_json::from_str(CREATED).unwrap_or_default();
    without["value"]
        .as_object_mut()
        .map(|value| value.remove("securityToken"));
    assert!(CreatedWebhook::read_answer(without.to_string().as_bytes()).is_none());
    assert!(CreatedWebhook::read_answer(b"[]").is_none());
}

#[test]
fn the_reads_go_to_the_middle_tier() {
    let list = OutgoingWebhooks { team: TEAM.to_owned() };
    assert_eq!(Read::service(&list), Service::MiddleTier);
    assert_eq!(
        Read::endpoint(&list).segments(),
        ["beta", "teams", TEAM, "outgoingWebhooks"]
    );
    let picture = OutgoingWebhookPicture {
        team: TEAM.to_owned(),
        webhook: HOOK.to_owned(),
        version: Some("v1".to_owned()),
    };
    assert_eq!(Read::service(&picture), Service::MiddleTier);
    let endpoint = Read::endpoint(&picture);
    assert_eq!(
        endpoint.segments(),
        ["beta", "teams", TEAM, "outgoingWebhooks", HOOK, "picture"]
    );
    assert_eq!(endpoint.query(), [("v".to_owned(), "v1".to_owned())]);
}

fn wire(write: &TeamWrite) -> (Service, Method, Vec<String>, Option<Value>) {
    (
        write.service(),
        write.method(),
        write.endpoint().segments().to_vec(),
        write.body(),
    )
}

#[test]
fn create_change_and_remove_go_in_the_routes_shape() {
    let create = TeamWrite::CreateOutgoingWebhook {
        team: TEAM.to_owned(),
        form: WebhookForm {
            name: Some("Build bot".to_owned()),
            callback_url: Some("https://hooks.example.test/build".to_owned()),
            description: Some("Builds".to_owned()),
            picture: WebhookPicture::Set(b"\x89PNG\r\n".to_vec()),
        },
    };
    assert!(create.is_write());
    assert_eq!(
        wire(&create),
        (
            Service::MiddleTier,
            Method::Post,
            vec!["beta".into(), "teams".into(), TEAM.into(), "outgoingWebhooks".into()],
            Some(json!({
                "name": "Build bot",
                "callbackUrl": "https://hooks.example.test/build",
                "description": "Builds",
                "picture": "iVBORw0K",
            })),
        )
    );
    let change = TeamWrite::ChangeOutgoingWebhook {
        team: TEAM.to_owned(),
        webhook: HOOK.to_owned(),
        form: WebhookForm {
            name: Some("Builds".to_owned()),
            picture: WebhookPicture::Remove,
            ..WebhookForm::default()
        },
    };
    assert_eq!(
        wire(&change),
        (
            Service::MiddleTier,
            Method::Put,
            vec![
                "beta".into(),
                "teams".into(),
                TEAM.into(),
                "outgoingWebhooks".into(),
                HOOK.into()
            ],
            Some(json!({"name": "Builds", "picture": null})),
        )
    );
    let keep = TeamWrite::ChangeOutgoingWebhook {
        team: TEAM.to_owned(),
        webhook: HOOK.to_owned(),
        form: WebhookForm {
            description: Some(String::new()),
            ..WebhookForm::default()
        },
    };
    assert_eq!(
        wire(&keep).3,
        Some(json!({"description": ""})),
        "a kept picture is not sent"
    );
    let remove = TeamWrite::RemoveOutgoingWebhook {
        team: TEAM.to_owned(),
        webhook: HOOK.to_owned(),
    };
    assert_eq!(
        wire(&remove),
        (
            Service::MiddleTier,
            Method::Delete,
            vec![
                "beta".into(),
                "teams".into(),
                TEAM.into(),
                "outgoingWebhooks".into(),
                HOOK.into()
            ],
            None,
        )
    );
}

#[test]
fn a_forms_debug_leaves_its_address_and_picture_out() {
    let form = WebhookForm {
        name: Some("Build bot".to_owned()),
        callback_url: Some("https://hooks.example.test/build".to_owned()),
        description: None,
        picture: WebhookPicture::Set(vec![0x89, 0x50]),
    };
    let shown = format!("{form:?}");
    assert!(
        !shown.contains("hooks.example.test") && !shown.contains("137"),
        "{shown}"
    );
}

#[test]
fn a_webhook_is_an_app_whose_bot_people_mention_in_the_teams_channels() {
    let hooks = OutgoingWebhook::list_answer(LIST.as_bytes());
    let app = hooks[0].app();
    assert_eq!(app.app.id, HOOK);
    assert_eq!(app.app.name, "Build bot");
    assert_eq!(app.app.developer, teams_proto::OUTGOING_WEBHOOK_MAKER);
    assert!(app.has_bot);
    assert_eq!(app.definition.id(), HOOK);
    assert_eq!(
        app.definition.mentionable_bot(true).map(|bot| bot.as_str().to_owned()),
        Some(format!("28:{HOOK}"))
    );
    assert_eq!(app.definition.mentionable_bot(false), None, "not in group chats");
    assert_eq!(app.definition.personal_bot(), None);
    assert_eq!(
        app.definition.outgoing_webhook(),
        Some(OutgoingWebhookPicture {
            team: TEAM.to_owned(),
            webhook: HOOK.to_owned(),
            version: Some("made-up-picture-1".to_owned()),
        })
    );
    assert_eq!(app.definition.large_image_url(), None);
    let no_picture = hooks[1].app();
    assert_eq!(
        no_picture.definition.outgoing_webhook().map(|picture| picture.version),
        Some(None)
    );
}
