//! The app registry through the admin API and the command line: a bot from the form, an app from a Teams package,
//! who may use it, its secret, enable, disable, delete, and the audit of each.

use reqwest::Method;
use serde_json::{Value, json};

use super::{APPS, Registry, app_path, call, error_code, good_package, manifest, package, png};
use crate::admin::cli;
use crate::support::{Backend, backends};

const ECHO_ENDPOINT: &str = "https://echo.example.test/api/messages";

async fn audit_actions(registry: &Registry) -> Vec<(String, String, Value)> {
    let (status, audit) = registry.call(Method::GET, "/api/admin/v1/audit", None).await;
    assert_eq!(status, 200, "{audit}");
    audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["action"].as_str().unwrap().to_owned(),
                entry["target"].as_str().unwrap().to_owned(),
                entry["detail"].clone(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_registers_a_bot_from_the_form_and_sees_its_secret_once() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let added = registry.register("Echo (synthetic)", ECHO_ENDPOINT).await;
        let id = added["id"].as_str().unwrap();
        uuid::Uuid::parse_str(id).unwrap();
        let secret = added["secret"].as_str().unwrap();
        assert!(secret.len() >= 40, "{secret}");
        assert_eq!(added["name"], "Echo (synthetic)");
        assert_eq!(added["source"], "form");
        assert_eq!(added["enabled"], true);
        assert_eq!(added["everyone"], true);
        assert_eq!(added["users"], json!([]));
        assert_eq!(added["bot"]["id"], id);
        assert_eq!(added["bot"]["mri"], format!("28:{id}"));
        assert_eq!(added["bot"]["endpoint"], ECHO_ENDPOINT);
        assert_eq!(added["bot"]["auth_mode"], "signed");
        assert_eq!(added["bot"]["scopes"], json!(["personal", "team", "groupChat"]));

        // the secret is shown once: never read back, never in the audit
        let (status, read) = registry.call(Method::GET, &app_path(id), None).await;
        assert_eq!(status, 200, "{read}");
        assert!(read.get("secret").is_none(), "{read}");
        assert_eq!(read["bot"], added["bot"]);
        let (status, listed) = registry.call(Method::GET, APPS, None).await;
        assert_eq!(status, 200, "{listed}");
        let ids: Vec<&str> = listed["apps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|app| app["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, [id]);
        assert!(!listed.to_string().contains(secret));
        let audit = audit_actions(&registry).await;
        assert_eq!(audit[0].0, "app.add");
        assert_eq!(audit[0].1, "Echo (synthetic)");
        assert!(!format!("{audit:?}").contains(secret));
        if backend == Backend::Sqlite {
            let file = registry
                .server
                .app
                .config
                .database_url
                .trim_start_matches("sqlite://")
                .to_owned();
            let mut stored = std::fs::read(&file).unwrap();
            stored.extend(std::fs::read(format!("{file}-wal")).unwrap_or_default());
            let found = stored.windows(secret.len()).any(|window| window == secret.as_bytes());
            assert!(!found, "the secret is kept in clear");
        }
        registry.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_form_refuses_what_a_bot_cant_be_and_only_an_admin_may_use_it() {
    let registry = Registry::open(Backend::Sqlite).await;
    let refused = |body: Value| {
        let registry = &registry;
        async move { registry.call(Method::POST, APPS, Some(body)).await }
    };
    let cases = [
        (json!({"name": " ", "endpoint": ECHO_ENDPOINT}), 400, "InvalidName"),
        (
            json!({"name": "x".repeat(300), "endpoint": ECHO_ENDPOINT}),
            400,
            "InvalidName",
        ),
        (
            json!({"name": "Echo", "endpoint": "http://echo.example.test/api"}),
            400,
            "InvalidEndpoint",
        ),
        (
            json!({"name": "Echo", "endpoint": "ftp://echo.example.test/api"}),
            400,
            "InvalidEndpoint",
        ),
        (
            json!({"name": "Echo", "endpoint": "https://user:pw@echo.example.test/"}),
            400,
            "InvalidEndpoint",
        ),
        (json!({"name": "Echo", "endpoint": "not a url"}), 400, "InvalidEndpoint"),
        (
            json!({"name": "Echo", "endpoint": ECHO_ENDPOINT, "auth_mode": "unsigned"}),
            400,
            "UnsignedNotAllowed",
        ),
        (
            json!({"name": "Echo", "endpoint": ECHO_ENDPOINT, "auth_mode": "magic"}),
            400,
            "InvalidRequest",
        ),
    ];
    for (body, status, code) in cases {
        let (got, answer) = refused(body.clone()).await;
        assert_eq!((got, error_code(&answer)), (status, code), "{body} → {answer}");
    }
    // a form without an endpoint registers a bot that has none yet
    let (status, bare) = refused(json!({"name": "Notify only"})).await;
    assert_eq!(status, 201, "{bare}");
    assert_eq!(bare["bot"]["endpoint"], Value::Null);

    // no token, and an account that isn't an admin
    let body = json!({"name": "Echo", "endpoint": ECHO_ENDPOINT});
    let (status, _) = call(&registry.server, Method::POST, APPS, None, Some(body.clone())).await;
    assert_eq!(status, 401);
    registry.server.add_user("bob@own.test", "Bob").await;
    let bob = registry.server.session("bob@own.test").await;
    let token = bob.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    let (status, answer) = call(
        &registry.server,
        Method::POST,
        APPS,
        Some(token.token.expose()),
        Some(body),
    )
    .await;
    assert_eq!((status, error_code(&answer)), (403, "NotAdmin"));
    let (status, _) = call(&registry.server, Method::GET, APPS, Some(token.token.expose()), None).await;
    assert_eq!(status, 403);
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unsigned_bots_and_plain_http_endpoints_take_the_servers_consent() {
    let registry = Registry::open_with(Backend::Sqlite, |config| {
        config.bots.allow_unsigned = true;
        config.bots.allow_http_endpoints = true;
        config.bots.allowed_networks = vec!["127.0.0.0/8".parse().unwrap()];
    })
    .await;
    let body = json!({"name": "Local", "endpoint": "http://127.0.0.1:3978/api/messages", "auth_mode": "unsigned"});
    let (status, added) = registry.call(Method::POST, APPS, Some(body)).await;
    assert_eq!(status, 201, "{added}");
    assert_eq!(added["bot"]["auth_mode"], "unsigned");
    assert_eq!(added["bot"]["endpoint"], "http://127.0.0.1:3978/api/messages");
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_registers_its_app_and_bot_and_a_new_version_replaces_it() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let (app_id, bot_id) = (uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string());
        let upload = format!("{APPS}/package");
        let (status, added) = registry
            .upload(&upload, Method::POST, good_package(&app_id, &bot_id, "1.0.0"))
            .await;
        assert_eq!(status, 201, "{added}");
        assert_eq!(added["id"], app_id);
        assert_eq!(added["source"], "package");
        assert_eq!(added["version"], "1.0.0");
        assert_eq!(added["name"], "Polls (synthetic)");
        assert_eq!(added["full_name"], "Synthetic Polls for teams");
        assert_eq!(added["short_description"], "Quick polls");
        assert_eq!(added["developer"], "Synthetic Tools");
        assert_eq!(added["accent_color"], "#4F6BED");
        assert_eq!(added["bot"]["id"], bot_id);
        assert_eq!(added["bot"]["mri"], format!("28:{bot_id}"));
        assert_eq!(added["bot"]["endpoint"], Value::Null);
        assert!(added["secret"].as_str().unwrap().len() >= 40);
        // the manifest is kept whole and the icons go to the file store
        assert_eq!(added["manifest"], manifest(&app_id, &bot_id, "1.0.0"));
        let storage = registry.server.app.storage.as_ref();
        let color = storage.get(&format!("apps/{app_id}/color.png")).await.unwrap().unwrap();
        assert_eq!(color.as_ref(), png(192, 192).as_slice());
        assert!(
            storage
                .get(&format!("apps/{app_id}/outline.png"))
                .await
                .unwrap()
                .is_some()
        );

        // its endpoint is set afterwards
        let endpoint = json!({"endpoint": "https://polls.example.test/api/messages"});
        let (status, changed) = registry.call(Method::PATCH, &app_path(&app_id), Some(endpoint)).await;
        assert_eq!(status, 200, "{changed}");
        assert_eq!(changed["bot"]["endpoint"], "https://polls.example.test/api/messages");

        // the same app again is a conflict; a new version replaces it and keeps the bot's endpoint and secret
        let (status, again) = registry
            .upload(&upload, Method::POST, good_package(&app_id, &bot_id, "1.0.0"))
            .await;
        assert_eq!((status, error_code(&again)), (409, "AppExists"));
        let replace = format!("{}/package", app_path(&app_id));
        let mut newer = manifest(&app_id, &bot_id, "1.1.0");
        newer["name"]["short"] = "Polls 2 (synthetic)".into();
        let (status, replaced) = registry
            .upload(
                &replace,
                Method::PUT,
                package(&newer, &png(192, 192), &png(32, 32), &[]),
            )
            .await;
        assert_eq!(status, 200, "{replaced}");
        assert_eq!(replaced["version"], "1.1.0");
        assert_eq!(replaced["name"], "Polls 2 (synthetic)");
        assert_eq!(replaced["bot"]["endpoint"], "https://polls.example.test/api/messages");
        assert!(replaced.get("secret").is_none());
        // a replacement names the same app and bot
        let other_bot = good_package(&app_id, &uuid::Uuid::new_v4().to_string(), "1.2.0");
        let (status, answer) = registry.upload(&replace, Method::PUT, other_bot).await;
        assert_eq!((status, error_code(&answer)), (400, "BotChanged"), "{answer}");
        let other_app = good_package(&uuid::Uuid::new_v4().to_string(), &bot_id, "1.2.0");
        let (status, answer) = registry.upload(&replace, Method::PUT, other_app).await;
        assert_eq!((status, error_code(&answer)), (400, "InvalidPackage"), "{answer}");
        // a second app can't take a bot another app has
        let thief = good_package(&uuid::Uuid::new_v4().to_string(), &bot_id, "1.0.0");
        let (status, answer) = registry.upload(&upload, Method::POST, thief).await;
        assert_eq!((status, error_code(&answer)), (409, "BotExists"), "{answer}");

        let actions: Vec<String> = audit_actions(&registry)
            .await
            .into_iter()
            .map(|entry| entry.0)
            .collect();
        assert_eq!(actions[..3], ["app.package", "app.update", "app.add"]);
        registry.server.stop().await;
    }
}

/// Packages Teams refuses, each with what is wrong with it.
fn refused_packages(good: &Value) -> Vec<(&'static str, bytes::Bytes)> {
    let with = |change: fn(&mut Value)| {
        let mut manifest = good.clone();
        change(&mut manifest);
        package(&manifest, &png(192, 192), &png(32, 32), &[])
    };
    vec![
        ("not a zip", bytes::Bytes::from_static(b"PK not really")),
        ("no manifest", {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file("color.png", zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, &png(192, 192)).unwrap();
            bytes::Bytes::from(zip.finish().unwrap().into_inner())
        }),
        ("manifest not json", {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file("manifest.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, b"{ nope").unwrap();
            bytes::Bytes::from(zip.finish().unwrap().into_inner())
        }),
        ("id not a GUID", with(|m| m["id"] = "polls".into())),
        (
            "bot id not a GUID",
            with(|m| m["bots"][0]["botId"] = "polls-bot".into()),
        ),
        ("no short name", with(|m| m["name"]["short"] = "".into())),
        (
            "two bots",
            with(|m| {
                let bot = m["bots"][0].clone();
                m["bots"].as_array_mut().unwrap().push(bot);
            }),
        ),
        (
            "unknown bot scope",
            with(|m| m["bots"][0]["scopes"] = json!(["everywhere"])),
        ),
        (
            "icon file missing",
            with(|m| m["icons"]["color"] = "missing.png".into()),
        ),
        (
            "color icon of the wrong size",
            package(good, &png(64, 64), &png(32, 32), &[]),
        ),
        (
            "outline icon of the wrong size",
            package(good, &png(192, 192), &png(48, 48), &[]),
        ),
        (
            "icon that isn't a picture",
            package(good, b"GIF? no", &png(32, 32), &[]),
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_that_teams_would_refuse_is_refused() {
    let registry = Registry::open(Backend::Sqlite).await;
    let upload = format!("{APPS}/package");
    let (app, bot) = (uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string());
    let good = manifest(&app, &bot, "1.0.0");
    let cases = refused_packages(&good);
    for (case, bytes) in cases {
        let (status, answer) = registry.upload(&upload, Method::POST, bytes).await;
        assert_eq!(
            (status, error_code(&answer)),
            (400, "InvalidPackage"),
            "{case}: {answer}"
        );
        assert!(!answer["error"]["message"].as_str().unwrap().is_empty());
    }
    // a package bigger than the limit (of bytes that don't compress)
    let mut state = 0x2545_f491_u32;
    let noise: Vec<u8> = (0..6 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect();
    let huge = package(&good, &png(192, 192), &png(32, 32), &[("big.bin", &noise)]);
    // the server may answer before it read the body, and close: the client then sees the connection go
    let sent = reqwest::Client::new()
        .post(registry.server.url(&upload))
        .bearer_auth(registry.token().await)
        .header("content-type", "application/zip")
        .body(huge)
        .send()
        .await;
    if let Ok(response) = sent {
        assert_eq!(response.status().as_u16(), 413);
    }
    let (_, listed) = registry.call(Method::GET, APPS, None).await;
    assert_eq!(listed["apps"], json!([]));
    // an app without a bot is an app all the same
    let mut tab_only = good.clone();
    tab_only.as_object_mut().unwrap().remove("bots");
    tab_only.as_object_mut().unwrap().remove("composeExtensions");
    let (status, added) = registry
        .upload(
            &upload,
            Method::POST,
            package(&tab_only, &png(192, 192), &png(32, 32), &[]),
        )
        .await;
    assert_eq!(status, 201, "{added}");
    assert_eq!(added["bot"], Value::Null);
    assert!(added.get("secret").is_none());
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_names_who_may_use_an_app_and_deletes_it() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let bob = registry.server.add_user("bob@own.test", "Bob").await;
        let added = registry.register("Helpdesk (synthetic)", ECHO_ENDPOINT).await;
        let id = added["id"].as_str().unwrap();
        let path = app_path(id);

        let listed = json!({"everyone": false, "users": [bob]});
        let (status, changed) = registry.call(Method::PATCH, &path, Some(listed)).await;
        assert_eq!(status, 200, "{changed}");
        assert_eq!(changed["everyone"], false);
        assert_eq!(changed["users"], json!([bob]));
        // nobody unknown, and no bot, is a user of an app
        let workflows = "358f0194-6b0e-4dd3-af35-c24fe8a9ec87";
        for stranger in [uuid::Uuid::new_v4().to_string(), id.to_owned(), workflows.to_owned()] {
            let (status, answer) = registry
                .call(Method::PATCH, &path, Some(json!({"users": [stranger]})))
                .await;
            assert_eq!((status, error_code(&answer)), (400, "UnknownUser"), "{answer}");
        }
        let (status, answer) = registry
            .call(Method::PATCH, &path, Some(json!({"colour": "red"})))
            .await;
        assert_eq!(status, 400, "{answer}");

        let (status, off) = registry
            .call(Method::PATCH, &path, Some(json!({"enabled": false})))
            .await;
        assert_eq!(status, 200, "{off}");
        assert_eq!(off["enabled"], false);

        let (status, rotated) = registry.call(Method::POST, &format!("{path}/secret"), None).await;
        assert_eq!(status, 200, "{rotated}");
        let secret = rotated["secret"].as_str().unwrap();
        assert_ne!(secret, added["secret"].as_str().unwrap());

        let (status, _) = registry.call(Method::DELETE, &path, None).await;
        assert_eq!(status, 204);
        for method in [Method::GET, Method::DELETE, Method::PATCH] {
            let body = (method == Method::PATCH).then(|| json!({"enabled": true}));
            let (status, answer) = registry.call(method, &path, body).await;
            assert_eq!((status, error_code(&answer)), (404, "NotFound"));
        }
        let (status, answer) = registry.call(Method::POST, &format!("{path}/secret"), None).await;
        assert_eq!((status, error_code(&answer)), (404, "NotFound"));
        let (_, listed) = registry.call(Method::GET, APPS, None).await;
        assert_eq!(listed["apps"], json!([]));

        let audit = audit_actions(&registry).await;
        let actions: Vec<&str> = audit.iter().map(|entry| entry.0.as_str()).collect();
        assert_eq!(
            actions[..5],
            ["app.delete", "app.secret", "app.update", "app.update", "app.add"]
        );
        assert!(!format!("{audit:?}").contains(secret));
        assert_eq!(audit[2].2, json!({"enabled": false}));
        registry.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_bots_id_can_come_back_with_a_new_package() {
    let registry = Registry::open(Backend::Sqlite).await;
    let (app_id, bot_id) = (uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string());
    let upload = format!("{APPS}/package");
    let (status, _) = registry
        .upload(&upload, Method::POST, good_package(&app_id, &bot_id, "1.0.0"))
        .await;
    assert_eq!(status, 201);
    let (status, _) = registry.call(Method::DELETE, &app_path(&app_id), None).await;
    assert_eq!(status, 204);
    let (status, again) = registry
        .upload(&upload, Method::POST, good_package(&app_id, &bot_id, "2.0.0"))
        .await;
    assert_eq!(status, 201, "{again}");
    assert_eq!(again["bot"]["id"], bot_id);
    assert_eq!(again["version"], "2.0.0");
    // neither a person's id nor the Workflows bot's is a new bot's
    let person = registry.server.add_user("carol@own.test", "Carol").await;
    for taken in [person.as_str(), "358f0194-6b0e-4dd3-af35-c24fe8a9ec87"] {
        let (status, answer) = registry
            .upload(
                &upload,
                Method::POST,
                good_package(&uuid::Uuid::new_v4().to_string(), taken, "1.0.0"),
            )
            .await;
        assert_eq!((status, error_code(&answer)), (409, "BotExists"), "{answer}");
    }
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn apps_turned_off_serve_no_registry() {
    let registry = Registry::open_with(Backend::Sqlite, |config| config.apps.enabled = false).await;
    let body = json!({"name": "Echo", "endpoint": ECHO_ENDPOINT});
    let (status, _) = registry.call(Method::POST, APPS, Some(body)).await;
    assert_eq!(status, 404);
    let (status, _) = registry.call(Method::GET, APPS, None).await;
    assert_eq!(status, 404);
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_manages_apps_on_the_command_line() {
    let registry = Registry::open(Backend::Sqlite).await;
    let server = &registry.server;
    let added = cli(
        server,
        &[
            "app",
            "add",
            "--name",
            "Ops bot (synthetic)",
            "--endpoint",
            ECHO_ENDPOINT,
        ],
        "",
    )
    .await
    .unwrap();
    let mut lines = added.lines();
    let id = lines
        .next()
        .unwrap()
        .strip_prefix("added ")
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_owned();
    let secret = lines.next().unwrap().strip_prefix("secret ").unwrap().to_owned();
    assert!(secret.len() >= 40, "{added}");

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("polls.zip");
    let (app_id, bot_id) = (uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string());
    std::fs::write(&file, good_package(&app_id, &bot_id, "1.0.0")).unwrap();
    let uploaded = cli(server, &["app", "add", "--package", file.to_str().unwrap()], "")
        .await
        .unwrap();
    assert!(uploaded.starts_with(&format!("added {app_id} ")), "{uploaded}");
    assert!(uploaded.contains("\nsecret "), "{uploaded}");

    let listed = cli(server, &["app", "list"], "").await.unwrap();
    assert!(
        listed.contains(&format!("{id}\tenabled\tOps bot (synthetic)\t28:{id}")),
        "{listed}"
    );
    assert!(
        listed.contains(&format!("{app_id}\tenabled\tPolls (synthetic)\t28:{bot_id}")),
        "{listed}"
    );
    assert!(!listed.contains(&secret));

    cli(server, &["app", "disable", &id], "").await.unwrap();
    let (_, read) = registry.call(Method::GET, &app_path(&id), None).await;
    assert_eq!(read["enabled"], false);
    cli(server, &["app", "enable", &id], "").await.unwrap();
    let (_, read) = registry.call(Method::GET, &app_path(&id), None).await;
    assert_eq!(read["enabled"], true);
    let rotated = cli(server, &["app", "rotate-secret", &id], "").await.unwrap();
    let new_secret = rotated.trim().strip_prefix("secret ").unwrap();
    assert_ne!(new_secret, secret);
    cli(server, &["app", "remove", &app_id], "").await.unwrap();
    let (status, _) = registry.call(Method::GET, &app_path(&app_id), None).await;
    assert_eq!(status, 404);
    assert!(cli(server, &["app", "remove", &app_id], "").await.is_err());

    let audit = audit_actions(&registry).await;
    assert_eq!(audit[0].0, "app.delete");
    assert!(!format!("{audit:?}").contains(new_secret));
    server.app.db.ping().await.unwrap();
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_that_changes_nothing_is_not_audited_and_bot_settings_need_a_bot() {
    let registry = Registry::open_with(Backend::Sqlite, |config| config.bots.allow_unsigned = true).await;
    let added = registry.register("Echo (synthetic)", ECHO_ENDPOINT).await;
    let path = app_path(added["id"].as_str().unwrap());
    let before = audit_actions(&registry).await.len();
    let same =
        json!({"enabled": true, "everyone": true, "users": [], "endpoint": ECHO_ENDPOINT, "auth_mode": "signed"});
    let (status, unchanged) = registry.call(Method::PATCH, &path, Some(same)).await;
    assert_eq!(status, 200, "{unchanged}");
    assert_eq!(unchanged["updated_at"], added["updated_at"]);
    assert_eq!(audit_actions(&registry).await.len(), before);
    let (status, unsigned) = registry
        .call(Method::PATCH, &path, Some(json!({"auth_mode": "unsigned"})))
        .await;
    assert_eq!(status, 200, "{unsigned}");
    assert_eq!(unsigned["bot"]["auth_mode"], "unsigned");
    assert_eq!(audit_actions(&registry).await[0].2, json!({"auth_mode": "unsigned"}));

    // an app without a bot has no bot settings and no secret, and goes without its icons
    let (app, bot) = (uuid::Uuid::new_v4().to_string(), uuid::Uuid::new_v4().to_string());
    let mut tab_only = manifest(&app, &bot, "1.0.0");
    tab_only.as_object_mut().unwrap().remove("bots");
    tab_only["name"].as_object_mut().unwrap().remove("full");
    let upload = format!("{APPS}/package");
    let (status, added) = registry
        .upload(
            &upload,
            Method::POST,
            package(&tab_only, &png(192, 192), &png(32, 32), &[]),
        )
        .await;
    assert_eq!(status, 201, "{added}");
    assert_eq!(added["full_name"], added["name"]);
    let tab = app_path(&app);
    for body in [json!({"endpoint": ECHO_ENDPOINT}), json!({"auth_mode": "signed"})] {
        let (status, answer) = registry.call(Method::PATCH, &tab, Some(body)).await;
        assert_eq!((status, error_code(&answer)), (400, "NoBot"));
    }
    let (status, answer) = registry.call(Method::POST, &format!("{tab}/secret"), None).await;
    assert_eq!((status, error_code(&answer)), (400, "NoBot"));
    let (status, _) = registry.call(Method::DELETE, &tab, None).await;
    assert_eq!(status, 204);
    let storage = registry.server.app.storage.as_ref();
    assert!(storage.get(&format!("apps/{app}/color.png")).await.unwrap().is_none());

    // a colour that isn't one, and a manifest too big to read
    let mut odd = manifest(&uuid::Uuid::new_v4().to_string(), &bot, "1.0.0");
    odd["accentColor"] = "blue".into();
    let (status, answer) = registry
        .upload(&upload, Method::POST, package(&odd, &png(192, 192), &png(32, 32), &[]))
        .await;
    assert_eq!((status, error_code(&answer)), (400, "InvalidPackage"));
    assert!(answer["error"]["message"].as_str().unwrap().contains("accentColor"));
    let mut padded = manifest(&uuid::Uuid::new_v4().to_string(), &bot, "1.0.0");
    padded["padding"] = "x".repeat(1024 * 1024 + 1).into();
    let (status, answer) = registry
        .upload(
            &upload,
            Method::POST,
            package(&padded, &png(192, 192), &png(32, 32), &[]),
        )
        .await;
    assert_eq!((status, error_code(&answer)), (400, "InvalidPackage"), "{answer}");
    registry.server.stop().await;
}
