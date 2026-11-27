//! An own server's apps and bots as its administrator manages them (`teams_auth::admin`'s app routes) against
//! `wiremock`: the requests the server gets, what its answers and refusals become, and that a bot's secret is kept
//! only in what made it, never in a `Debug`. Every value is made up; the answers have the shapes `docs/server.md` gives.

use serde_json::{Value, json};
use teams_auth::admin::{
    AdminClient, AdminError, AppChanges, AppSource, BotAuthMode, BotHealth, HealthState, NewBot, Refusal, TestActivity,
    apps_not_served,
};
use teams_auth::secrets::Secret;
use wiremock::matchers::{body_bytes, body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "made-up-access-token";
const APP: &str = "00000000-0000-4000-8000-0000000000b1";
const PERSON: &str = "00000000-0000-4000-8000-0000000000d1";
const SECRET: &str = "made-up-bot-secret";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn app(enabled: bool) -> Value {
    json!({
        "id": APP,
        "version": "1.0.0",
        "name": "Made-up Notes",
        "full_name": "Made-up Notes",
        "short_description": "Takes made-up notes",
        "full_description": "",
        "developer": "Made-up Maker",
        "accent_color": "#4f52b2",
        "source": "form",
        "enabled": enabled,
        "everyone": false,
        "users": [PERSON],
        "manifest": {"id": APP},
        "bot": {
            "id": APP,
            "mri": format!("28:{APP}"),
            "endpoint": "https://bots.example.test/api/messages",
            "auth_mode": "signed",
            "scopes": ["personal"],
        },
        "created_at": 10,
        "updated_at": 20,
    })
}

fn refusal(status: u16, code: &str, message: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({"error": {"code": code, "message": message}}))
}

async fn server() -> TestResult<(MockServer, AdminClient)> {
    let server = MockServer::start().await;
    let client = AdminClient::with_loopback(&server.uri())?;
    Ok((server, client))
}

#[tokio::test]
async fn the_apps_are_listed_with_the_accounts_token() -> TestResult {
    let (server, client) = server().await?;
    let without_bot = json!({"id": "00000000-0000-4000-8000-0000000000b2", "name": "Made-up Tab", "enabled": true,
                             "everyone": true, "source": "package", "bot": null, "accent_color": ""});
    Mock::given(method("GET"))
        .and(path("/api/admin/v1/apps"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"apps": [app(true), without_bot]})))
        .expect(1)
        .mount(&server)
        .await;
    let apps = client.apps(&Secret::new(TOKEN)).await?;
    assert_eq!(apps.len(), 2);
    let notes = &apps[0];
    assert_eq!(notes.id, APP);
    assert_eq!(notes.name, "Made-up Notes");
    assert_eq!(notes.developer, "Made-up Maker");
    assert_eq!(notes.accent_color.as_deref(), Some("#4f52b2"));
    assert_eq!(notes.source, AppSource::Form);
    assert!(notes.enabled && !notes.everyone);
    assert_eq!(notes.users, [PERSON]);
    assert_eq!((notes.created_at, notes.updated_at), (10, 20));
    let bot = notes.bot.as_ref().ok_or("a bot")?;
    assert_eq!(bot.mri, format!("28:{APP}"));
    assert_eq!(bot.endpoint.as_deref(), Some("https://bots.example.test/api/messages"));
    assert_eq!(bot.auth_mode, BotAuthMode::Signed);
    assert_eq!(bot.scopes, ["personal"]);
    let tab = &apps[1];
    assert_eq!(tab.source, AppSource::Package);
    assert!(tab.bot.is_none() && tab.accent_color.is_none() && tab.users.is_empty());
    Ok(())
}

#[tokio::test]
async fn an_app_the_client_cant_read_is_refused() -> TestResult {
    let (server, client) = server().await?;
    let mut odd_bot = app(true);
    odd_bot["bot"]["auth_mode"] = json!("sideways");
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"apps": [odd_bot]})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"list": []})))
        .mount(&server)
        .await;
    let token = Secret::new(TOKEN);
    for _ in 0..2 {
        assert!(matches!(
            client.apps(&token).await,
            Err(AdminError::UnexpectedResponse { status: 200 })
        ));
    }
    Ok(())
}

#[tokio::test]
async fn a_server_without_apps_says_so_by_its_404() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let error = client.apps(&Secret::new(TOKEN)).await.err().ok_or("refused")?;
    assert!(apps_not_served(&error), "{error:?}");
    assert!(!apps_not_served(&AdminError::Unauthorized));
    Ok(())
}

#[tokio::test]
async fn a_bot_registered_from_the_form_comes_with_its_secret_once() -> TestResult {
    let (server, client) = server().await?;
    let mut made = app(true);
    made["secret"] = json!(SECRET);
    Mock::given(method("POST"))
        .and(path("/api/admin/v1/apps"))
        .and(body_json(json!({
            "name": "Made-up Notes",
            "description": "Takes made-up notes",
            "endpoint": "https://bots.example.test/api/messages",
            "auth_mode": "unsigned",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(made))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/admin/v1/apps"))
        .and(body_json(
            json!({"name": "No Endpoint", "description": "", "auth_mode": "signed"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(app(true)))
        .expect(1)
        .mount(&server)
        .await;
    let token = Secret::new(TOKEN);
    let made = client
        .register_bot(
            &token,
            NewBot {
                name: "Made-up Notes",
                description: "Takes made-up notes",
                endpoint: Some("https://bots.example.test/api/messages"),
                auth_mode: BotAuthMode::Unsigned,
            },
        )
        .await?;
    assert_eq!(made.app.id, APP);
    assert_eq!(made.secret.as_deref(), Some(SECRET));
    assert!(!format!("{made:?}").contains(SECRET));
    let plain = client
        .register_bot(
            &token,
            NewBot {
                name: "No Endpoint",
                description: "",
                endpoint: None,
                auth_mode: BotAuthMode::default(),
            },
        )
        .await?;
    assert!(plain.secret.is_none());
    Ok(())
}

#[tokio::test]
async fn the_servers_refusals_name_what_is_wrong() -> TestResult {
    let (server, client) = server().await?;
    let token = Secret::new(TOKEN);
    let refusals = [
        (400, "InvalidName", Refusal::InvalidName),
        (400, "InvalidEndpoint", Refusal::InvalidEndpoint),
        (400, "UnsignedNotAllowed", Refusal::UnsignedNotAllowed),
        (400, "UnknownUser", Refusal::UnknownUser),
        (400, "NoBot", Refusal::NoBot),
        (400, "InvalidPackage", Refusal::InvalidPackage),
        (400, "BotChanged", Refusal::BotChanged),
        (400, "InvalidRequest", Refusal::InvalidRequest),
        (409, "AppExists", Refusal::AppExists),
        (409, "BotExists", Refusal::BotExists),
        (413, "PackageTooLarge", Refusal::PackageTooLarge),
        (400, "EndpointNotAllowed", Refusal::EndpointNotAllowed),
        (400, "EndpointMetadata", Refusal::EndpointMetadata),
        (400, "NoEndpoint", Refusal::NoEndpoint),
        (409, "AppDisabled", Refusal::AppDisabled),
    ];
    for (status, code, expected) in refusals {
        server.reset().await;
        Mock::given(method("PATCH"))
            .respond_with(refusal(status, code, "Made-up words."))
            .mount(&server)
            .await;
        let error = client
            .update_app(&token, APP, &AppChanges::default())
            .await
            .err()
            .ok_or("refused")?;
        assert_eq!(error.refusal(), Some(expected), "{code}");
    }
    Ok(())
}

#[tokio::test]
async fn a_package_is_uploaded_as_a_zip_and_a_new_one_replaces_it() -> TestResult {
    let (server, client) = server().await?;
    let package = b"PK\x03\x04 made-up package".to_vec();
    let mut made = app(true);
    made["source"] = json!("package");
    made["secret"] = json!(SECRET);
    Mock::given(method("POST"))
        .and(path("/api/admin/v1/apps/package"))
        .and(header("content-type", "application/zip"))
        .and(body_bytes(package.clone()))
        .respond_with(ResponseTemplate::new(201).set_body_json(made))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/api/admin/v1/apps/{APP}/package")))
        .and(header("content-type", "application/zip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(app(true)))
        .expect(1)
        .mount(&server)
        .await;
    let token = Secret::new(TOKEN);
    let made = client.upload_package(&token, package.clone()).await?;
    assert_eq!(made.app.source, AppSource::Package);
    assert_eq!(made.secret.as_deref(), Some(SECRET));
    let replaced = client.replace_package(&token, APP, package).await?;
    assert_eq!(replaced.id, APP);
    Ok(())
}

#[tokio::test]
async fn a_package_over_the_body_limit_is_said_too_large() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(413).set_body_string("length limit exceeded"))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let token = Secret::new(TOKEN);
    let error = client.upload_package(&token, vec![0; 8]).await.err().ok_or("refused")?;
    assert_eq!(error.refusal(), Some(Refusal::PackageTooLarge));
    let error = client
        .replace_package(&token, APP, vec![0; 8])
        .await
        .err()
        .ok_or("refused")?;
    assert!(
        matches!(error, AdminError::UnexpectedResponse { status: 500 }),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_change_sends_only_what_changed() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/admin/v1/apps/{APP}")))
        .and(body_json(json!({
            "enabled": false,
            "everyone": false,
            "users": [PERSON],
            "endpoint": "https://bots.example.test/v2",
            "auth_mode": "signed",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(app(false)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path(format!("/api/admin/v1/apps/{APP}")))
        .and(body_json(json!({"enabled": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(app(true)))
        .expect(1)
        .mount(&server)
        .await;
    let token = Secret::new(TOKEN);
    let all = AppChanges {
        enabled: Some(false),
        everyone: Some(false),
        users: Some(vec![PERSON.to_owned()]),
        endpoint: Some("https://bots.example.test/v2".to_owned()),
        auth_mode: Some(BotAuthMode::Signed),
    };
    assert!(!all.is_empty() && AppChanges::default().is_empty());
    assert!(!client.update_app(&token, APP, &all).await?.enabled);
    let on = AppChanges {
        enabled: Some(true),
        ..AppChanges::default()
    };
    assert!(client.update_app(&token, APP, &on).await?.enabled);
    Ok(())
}

#[tokio::test]
async fn a_new_secret_is_kept_secret_and_a_delete_answers_nothing() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("POST"))
        .and(path(format!("/api/admin/v1/apps/{APP}/secret")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"secret": SECRET})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path(format!("/api/admin/v1/apps/{APP}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let token = Secret::new(TOKEN);
    let secret = client.rotate_secret(&token, APP).await?;
    assert_eq!(secret, SECRET);
    client.delete_app(&token, APP).await?;
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    assert!(matches!(
        client.rotate_secret(&token, APP).await,
        Err(AdminError::UnexpectedResponse { status: 200 })
    ));
    Ok(())
}

#[tokio::test]
async fn the_bots_metadata_is_asked_without_a_token() -> TestResult {
    let (server, client) = server().await?;
    Mock::given(method("GET"))
        .and(path("/bot/v1/.well-known/openidconfiguration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": "https://teams.example.test/bot",
            "jwks_uri": "https://teams.example.test/bot/v1/.well-known/keys",
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"issuer": ""})))
        .mount(&server)
        .await;
    let metadata = client.bot_metadata().await?;
    assert_eq!(metadata.issuer, "https://teams.example.test/bot");
    let asked = &server.received_requests().await.ok_or("requests")?[0];
    assert!(!asked.headers.contains_key("authorization"));
    assert!(matches!(
        client.bot_metadata().await,
        Err(AdminError::UnexpectedResponse { status: 200 })
    ));
    assert_eq!(client.origin(), server.uri());
    Ok(())
}

#[tokio::test]
async fn each_bots_delivery_health_is_read_with_its_app() -> TestResult {
    let (server, client) = server().await?;
    let mut failing = app(true);
    failing["health"] = json!({"state": "failing", "failures": 3, "paused_until": null,
        "last_error": "the bot answered 500", "last_success": 1_700_000_000_000_i64,
        "outbox": {"pending": 1, "sent": 7, "failed": 2}});
    let mut paused = app(true);
    paused["id"] = json!("00000000-0000-4000-8000-0000000000b3");
    paused["health"] = json!({"state": "paused", "failures": 0, "paused_until": 1_800_000_000_000_i64,
        "last_error": "the request timed out", "last_success": null, "outbox": {"pending": 4, "sent": 0, "failed": 20}});
    let mut odd = app(true);
    odd["id"] = json!("00000000-0000-4000-8000-0000000000b4");
    odd["health"] = json!({"state": "resting"});
    let older = app(true);
    Mock::given(method("GET"))
        .and(path("/api/admin/v1/apps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"apps": [failing, paused, odd, older]})))
        .mount(&server)
        .await;
    let apps = client.apps(&Secret::new(TOKEN)).await?;
    assert_eq!(
        apps[0].health,
        Some(BotHealth {
            state: HealthState::Failing,
            failures: 3,
            paused_until: None,
            last_error: Some("the bot answered 500".to_owned()),
            last_success: Some(1_700_000_000_000),
            pending: 1,
            sent: 7,
            failed: 2,
        })
    );
    let paused = apps[1].health.as_ref().ok_or("health")?;
    assert_eq!(
        (paused.state, paused.paused_until, paused.failed),
        (HealthState::Paused, Some(1_800_000_000_000), 20)
    );
    assert_eq!(
        apps[2].health.as_ref().map(|health| health.state),
        Some(HealthState::Other)
    );
    // a server from before the health says none
    assert_eq!(apps[3].health, None);
    Ok(())
}

#[tokio::test]
async fn a_test_activity_says_how_it_went() -> TestResult {
    let (server, client) = server().await?;
    let token = Secret::new(TOKEN);
    Mock::given(method("POST"))
        .and(path(format!("/api/admin/v1/apps/{APP}/test")))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"delivered": false, "status": 500, "error": "the bot answered 500", "elapsed_ms": 42}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let went = client.test_activity(&token, APP).await?;
    assert_eq!(
        went,
        TestActivity {
            delivered: false,
            status: Some(500),
            error: Some("the bot answered 500".to_owned()),
            elapsed_ms: 42,
        }
    );
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "2")
                .set_body_json(json!({"error": {"code": "TooManyRequests", "message": "Made-up words."}})),
        )
        .mount(&server)
        .await;
    let held = client.test_activity(&token, APP).await.err().ok_or("held")?;
    assert!(matches!(held, AdminError::Throttled { .. }), "{held:?}");
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": 200})))
        .mount(&server)
        .await;
    let unread = client.test_activity(&token, APP).await.err().ok_or("unread")?;
    assert!(
        matches!(unread, AdminError::UnexpectedResponse { status: 200 }),
        "{unread:?}"
    );
    Ok(())
}
