//! The admin command line against a server's database, and the client signing in with what it set.

use clap::Parser as _;
use ngteams_server::cli::{Cli, run};
use teams_auth::token::Scope;

use crate::support::{Backend, PASSWORD, TestServer};

async fn cli(server: &TestServer, args: &[&str], input: &str) -> Result<String, ngteams_server::cli::CliError> {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("server.toml");
    std::fs::write(
        &config,
        format!(
            "database_url = \"{}\"\npublic_origin = \"{}\"\n",
            server.app.config.database_url, server.origin
        ),
    )
    .unwrap();
    let mut argv = vec!["ngteams-server", "--config", config.to_str().unwrap()];
    argv.extend_from_slice(args);
    let mut output = Vec::new();
    run(Cli::parse_from(argv), &mut input.as_bytes(), &mut output, std::future::pending()).await?;
    Ok(String::from_utf8(output).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn accounts_added_and_changed_on_the_command_line_sign_in_as_set() {
    let server = TestServer::start(Backend::Sqlite).await;
    let added = cli(&server, &["user", "add", "Grace@Own.Test", "--name", "Grace Hopper"], &format!("{PASSWORD}\n"))
        .await
        .unwrap();
    assert!(added.starts_with("added grace@own.test ("), "{added}");
    let session = server.session("grace@own.test").await;
    session.skypetoken().await.unwrap();

    let listed = cli(&server, &["user", "list"], "").await.unwrap();
    assert!(listed.starts_with("grace@own.test\t") && listed.contains("\tenabled\tGrace Hopper"), "{listed}");

    cli(&server, &["user", "disable", "grace@own.test"], "").await.unwrap();
    assert!(cli(&server, &["user", "list"], "").await.unwrap().contains("\tdisabled\t"));
    assert_eq!(server.password_grant("grace@own.test", PASSWORD, &Scope::spaces()).await.0, 400);
    cli(&server, &["user", "enable", "grace@own.test"], "").await.unwrap();

    cli(&server, &["user", "reset-password", "grace@own.test"], "another password\r\n")
        .await
        .unwrap();
    assert_eq!(server.password_grant("grace@own.test", PASSWORD, &Scope::spaces()).await.0, 400);
    assert_eq!(
        server.password_grant("grace@own.test", "another password", &Scope::spaces()).await.0,
        200
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_line_refuses_bad_accounts_and_passwords() {
    let server = TestServer::start(Backend::Sqlite).await;
    server.add_user("ada@own.test", "Ada").await;
    for (args, input) in [
        (&["user", "add", "ada@own.test", "--name", "Ada"][..], "a long password\n"),
        (&["user", "add", "not-an-upn", "--name", "X"][..], "a long password\n"),
        (&["user", "add", "eve@own.test", "--name", "Eve"][..], "short\n"),
        (&["user", "disable", "nobody@own.test"][..], ""),
        (&["user", "reset-password", "nobody@own.test"][..], "a long password\n"),
    ] {
        assert!(cli(&server, args, input).await.is_err(), "{args:?}");
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_rotated_on_the_command_line_reaches_every_node_and_old_tokens_still_verify() {
    let node_a = TestServer::start(Backend::Sqlite).await;
    node_a.add_user("ada@own.test", "Ada").await;
    let before = node_a.session("ada@own.test").await;
    let old = before.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    let rotated = cli(&node_a, &["keys", "rotate"], "").await.unwrap();
    assert!(rotated.starts_with("signing with key "), "{rotated}");
    let shared = node_a.app.config.clone();
    // one deployment: the same database and the same public origin (the tokens' issuer)
    let node_b = TestServer::start_with(Backend::Sqlite, |config| {
        config.database_url = shared.database_url;
        config.public_origin = shared.public_origin;
    })
    .await;
    let on_b = node_b.session("ada@own.test").await;
    let new = on_b.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    for token in [&old.token, &new.token] {
        node_a
            .authz_client()
            .exchange(token, teams_auth::authz::AuthzKind::TokenRefresh)
            .await
            .unwrap();
    }
    node_b.stop().await;
    node_a.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn migrate_brings_a_new_database_up_and_a_bad_config_is_refused() {
    let server = TestServer::start(Backend::Sqlite).await;
    cli(&server, &["migrate"], "").await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("bad.toml");
    std::fs::write(&config, "public_origin = \"ftp://example.com\"\n").unwrap();
    let parsed = Cli::parse_from(["ngteams-server", "--config", config.to_str().unwrap(), "migrate"]);
    assert!(matches!(
        run(parsed, &mut "".as_bytes(), &mut Vec::new(), std::future::pending()).await,
        Err(ngteams_server::cli::CliError::Config(_))
    ));
    server.stop().await;
}
