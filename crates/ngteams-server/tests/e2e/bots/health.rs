//! What an administrator reads and does for a bot's delivery on the Manage apps page: its health (failures in a row,
//! paused until, the last error, the outbox's counts) in the list and in the app, and a test activity sent to its
//! endpoint through the address guard. Only an administrator may. Bots and messages are synthetic.

use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};

use super::connector::Setup;
use super::endpoint::TestBot;
use super::{APPS, app_path, call, error_code};
use crate::support::{Backend, backends};

/// The app's health once `check` holds for it (or after `within`): a send's row and the bot's health are two writes.
async fn health_when(setup: &Setup, check: impl Fn(&Value) -> bool, within: Duration) -> Value {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let (status, app) = setup.registry.call(Method::GET, &app_path(&setup.bot_id), None).await;
        assert_eq!(status, 200, "{app}");
        let health = app["health"].clone();
        if check(&health) || tokio::time::Instant::now() > deadline {
            return health;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn listed_health(list: &Value, id: &str) -> Value {
    list["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|app| app["id"] == id)
        .unwrap()["health"]
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_reads_a_bots_delivery_health_in_the_list_and_the_app() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| config.bots.retry_delays = vec![1, 1]).await;
        let fresh = health_when(&setup, |_| true, Duration::ZERO).await;
        assert_eq!(fresh["state"], "new", "{fresh}");
        assert_eq!(fresh["failures"], 0);
        assert_eq!(fresh["outbox"], json!({"pending": 0, "sent": 0, "failed": 0}));

        setup.bot.answer(500);
        let chat = setup.chat();
        setup.send(&chat, "to a failing bot").await;
        let failing = health_when(
            &setup,
            |health| health["outbox"]["failed"] == 1 && health["failures"] == 3,
            Duration::from_secs(15),
        )
        .await;
        assert_eq!(failing["state"], "failing", "{failing}");
        assert_eq!(failing["failures"], 3, "{failing}");
        assert_eq!(failing["paused_until"], Value::Null);
        assert!(failing["last_error"].as_str().unwrap().contains("500"), "{failing}");
        assert_eq!(failing["last_success"], Value::Null);
        assert_eq!(failing["outbox"], json!({"pending": 0, "sent": 0, "failed": 1}));
        let (_, list) = setup.registry.call(Method::GET, APPS, None).await;
        assert_eq!(listed_health(&list, &setup.bot_id), failing);

        setup.bot.answer(200);
        setup.send(&chat, "to a bot that works again").await;
        let healthy = health_when(
            &setup,
            |health| health["outbox"]["sent"] == 1 && health["state"] == "healthy",
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(healthy["state"], "healthy", "{healthy}");
        assert_eq!(healthy["failures"], 0);
        assert_eq!(healthy["last_error"], Value::Null);
        assert!(healthy["last_success"].as_i64().unwrap() > 0, "{healthy}");
        assert_eq!(healthy["outbox"], json!({"pending": 0, "sent": 1, "failed": 1}));

        // an app without a bot has no health
        let (status, form) = setup
            .registry
            .call(Method::POST, APPS, Some(json!({"name": "No endpoint (synthetic)"})))
            .await;
        assert_eq!(status, 201, "{form}");
        let (_, list) = setup.registry.call(Method::GET, APPS, None).await;
        let quiet = listed_health(&list, form["id"].as_str().unwrap());
        assert_eq!(quiet["state"], "new", "{quiet}");
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn twenty_failures_in_a_row_show_the_bot_paused() {
    let setup = Setup::open_with(Backend::Sqlite, |config| config.bots.retry_delays = Vec::new()).await;
    setup.bot.answer(503);
    let chat = setup.chat();
    for index in 0..20 {
        setup.send(&chat, &format!("failing {index}")).await;
    }
    let paused = health_when(&setup, |health| health["state"] == "paused", Duration::from_secs(20)).await;
    assert_eq!(paused["state"], "paused", "{paused}");
    let until = paused["paused_until"].as_i64().unwrap();
    assert!(until > ngteams_server::db::now() * 1000, "{paused}");
    assert_eq!(paused["outbox"]["failed"], 20, "{paused}");
    setup.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_sends_a_test_activity_through_the_guard_and_a_person_may_not() {
    let setup = Setup::open(Backend::Sqlite).await;
    let path = format!("{}/test", app_path(&setup.bot_id));
    let (status, answer) = setup.registry.call(Method::POST, &path, None).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["delivered"], true, "{answer}");
    assert_eq!(answer["status"], 200);
    assert_eq!(answer["error"], Value::Null);
    assert!(answer["elapsed_ms"].as_u64().is_some(), "{answer}");

    let got = setup.bot.next_of("event").await;
    let activity = &got.activity;
    assert_eq!(activity["name"], "ngteams/testActivity", "{activity}");
    assert_eq!(activity["channelId"], "msteams");
    assert_eq!(activity["recipient"]["id"], format!("28:{}", setup.bot_id));
    assert!(
        activity["from"]["id"].as_str().unwrap().starts_with("29:"),
        "{activity}"
    );
    let claims = got.token_claims().unwrap();
    assert_eq!(claims["aud"], setup.bot_id);
    assert_eq!(claims["serviceurl"], activity["serviceUrl"]);
    // the test activity carries no person's token: the only token is the server's own, in the header
    assert!(!activity.to_string().contains("eyJ"), "{activity}");
    // it is not a chat event: no delivery row, no health change
    let rows = ngteams_server::bots::deliveries(&setup.registry.server.app.db, &setup.bot_id)
        .await
        .unwrap();
    assert!(rows.is_empty(), "{rows:?}");

    // a bot that answers 500 is reported
    setup.bot.answer(500);
    let (status, answer) = setup.registry.call(Method::POST, &path, None).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(
        (&answer["delivered"], &answer["status"]),
        (&json!(false), &json!(500)),
        "{answer}"
    );

    // an app that is off, or has no bot endpoint, isn't tested
    let (_, form) = setup
        .registry
        .call(Method::POST, APPS, Some(json!({"name": "Silent (synthetic)"})))
        .await;
    let silent = format!("{}/test", app_path(form["id"].as_str().unwrap()));
    let (status, answer) = setup.registry.call(Method::POST, &silent, None).await;
    assert_eq!((status, error_code(&answer)), (400, "NoEndpoint"), "{answer}");
    let (status, _) = setup
        .registry
        .call(Method::PATCH, &app_path(&setup.bot_id), Some(json!({"enabled": false})))
        .await;
    assert_eq!(status, 200);
    let (status, answer) = setup.registry.call(Method::POST, &path, None).await;
    assert_eq!((status, error_code(&answer)), (409, "AppDisabled"), "{answer}");
    let (status, _) = setup
        .registry
        .call(Method::POST, &format!("{APPS}/{}/test", uuid::Uuid::new_v4()), None)
        .await;
    assert_eq!(status, 404);

    // a person who isn't an administrator reads no health and sends nothing
    setup.registry.server.add_user("cora@own.test", "Cora Synthetic").await;
    let session = setup.registry.server.session("cora@own.test").await;
    let token = session
        .access_token(teams_auth::token::SPACES_RESOURCE)
        .await
        .unwrap()
        .token
        .expose()
        .to_owned();
    let server = &setup.registry.server;
    let (status, _) = call(server, Method::POST, &path, Some(&token), None).await;
    assert_eq!(status, 403);
    let (status, _) = call(server, Method::GET, &app_path(&setup.bot_id), Some(&token), None).await;
    assert_eq!(status, 403);
    let (status, _) = call(server, Method::POST, &path, None, None).await;
    assert_eq!(status, 401);
    setup.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_activities_keep_to_the_bots_send_rate() {
    let setup = Setup::open_with(Backend::Sqlite, |config| {
        config.bots.send_rate = 1;
        config.bots.send_burst = 1;
    })
    .await;
    let path = format!("{}/test", app_path(&setup.bot_id));
    let token = setup.registry.token().await;
    let server = &setup.registry.server;
    let (status, _) = call(server, Method::POST, &path, Some(&token), None).await;
    assert_eq!(status, 200);
    let response = reqwest::Client::new()
        .post(server.url(&path))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    let wait: u64 = response.headers()[reqwest::header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=2).contains(&wait), "{wait}");
    let answer: Value = response.json().await.unwrap();
    assert_eq!(error_code(&answer), "TooManyRequests", "{answer}");
    setup.stop().await;
}

/// A name passes when saved, as it may resolve elsewhere by the time of a send; the guard refuses where it resolves
/// then, and the test activity tells why.
#[tokio::test(flavor = "multi_thread")]
async fn a_test_activity_to_a_name_on_a_closed_network_is_refused_by_the_guard() {
    let registry = super::Registry::open_with(Backend::Sqlite, |config| {
        super::connector::loopback(config);
        config.bots.allowed_networks.clear();
    })
    .await;
    let closed = TestBot::start().await;
    let by_name = closed.url.replace("127.0.0.1", "localhost");
    let made = registry.register("Closed (synthetic)", &by_name).await;
    let path = format!("{}/test", app_path(made["id"].as_str().unwrap()));
    let (status, answer) = registry.call(Method::POST, &path, None).await;
    assert_eq!(status, 200, "{answer}");
    assert_eq!(answer["delivered"], false, "{answer}");
    assert_eq!(answer["status"], Value::Null);
    assert!(answer["error"].as_str().unwrap().contains("network"), "{answer}");
    assert_eq!(closed.hits(), 0);
    registry.server.stop().await;
}
