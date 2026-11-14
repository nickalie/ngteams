//! What the nodes share beside the database: the fan-out (in-process, or Redis between nodes), the file store, and
//! the skypetoken check a service runs on a request.

use std::time::Duration;

use axum::Router;
use axum::routing::get;
use bytes::Bytes;
use ngteams_server::app::App;
use ngteams_server::auth::extract::Skype;
use ngteams_server::fanout::{Fanout, Local, Missed, Redis};
use ngteams_server::storage::{ObjectStorage, Storage, StorageError};

use crate::support::{Backend, TestServer, redis_url};

const WAIT: Duration = Duration::from_secs(5);

async fn next(subscription: &mut ngteams_server::fanout::Subscription) -> Bytes {
    tokio::time::timeout(WAIT, subscription.recv()).await.unwrap().unwrap()
}

#[tokio::test]
async fn the_local_fanout_delivers_to_each_subscriber_of_a_topic_only() {
    let fanout = Local::new();
    fanout.ready().await.unwrap();
    fanout
        .publish("push:nobody", Bytes::from_static(b"lost"))
        .await
        .unwrap();
    let mut first = fanout.subscribe("push:a").await.unwrap();
    let mut second = fanout.subscribe("push:a").await.unwrap();
    let mut other = fanout.subscribe("push:b").await.unwrap();
    fanout.publish("push:a", Bytes::from_static(b"hello")).await.unwrap();
    assert_eq!(next(&mut first).await, "hello");
    assert_eq!(next(&mut second).await, "hello");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), other.recv())
            .await
            .is_err()
    );
    drop((first, second));
    fanout
        .publish("push:a", Bytes::from_static(b"to nobody"))
        .await
        .unwrap();
    let mut again = fanout.subscribe("push:a").await.unwrap();
    fanout.publish("push:a", Bytes::from_static(b"back")).await.unwrap();
    assert_eq!(next(&mut again).await, "back");
    assert!(format!("{again:?}").contains("Subscription"));
}

#[tokio::test]
async fn a_subscriber_that_falls_behind_learns_how_much_it_missed() {
    let fanout = Local::new();
    let mut slow = fanout.subscribe("push:slow").await.unwrap();
    for n in 0..(ngteams_server::fanout::BACKLOG + 3) {
        fanout.publish("push:slow", Bytes::from(n.to_string())).await.unwrap();
    }
    assert_eq!(slow.recv().await, Err(Missed::Lagged(3)));
    assert_eq!(next(&mut slow).await, "3");
}

#[tokio::test(flavor = "multi_thread")]
async fn redis_carries_a_topic_between_two_nodes() {
    let Some(url) = redis_url() else { return };
    let node_a = Redis::connect(&url).await.unwrap();
    let node_b = Redis::connect(&url).await.unwrap();
    node_a.ready().await.unwrap();
    let topic = format!("push:{}", uuid::Uuid::new_v4());
    let mut on_b = node_b.subscribe(&topic).await.unwrap();
    let mut also_b = node_b.subscribe(&topic).await.unwrap();
    node_a.publish(&topic, Bytes::from_static(b"across")).await.unwrap();
    assert_eq!(next(&mut on_b).await, "across");
    assert_eq!(next(&mut also_b).await, "across");
    drop((on_b, also_b));
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut later = node_b.subscribe(&topic).await.unwrap();
    node_a.publish(&topic, Bytes::from_static(b"again")).await.unwrap();
    assert_eq!(next(&mut later).await, "again");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_on_one_database_and_redis_share_sign_ins_and_login_holds() {
    let Some(url) = redis_url() else { return };
    for backend in crate::support::backends() {
        let redis = url.clone();
        let node_a = TestServer::start_with(backend, |config| {
            config.redis_url = Some(redis);
            config.login.max_failures = 2;
        })
        .await;
        let (shared, redis) = (node_a.app.config.clone(), url.clone());
        let node_b = TestServer::start_with(backend, |config| {
            config.database_url = shared.database_url;
            config.public_origin = shared.public_origin;
            config.redis_url = Some(redis);
            config.login.max_failures = 2;
        })
        .await;
        let upn = format!("{}@own.test", uuid::Uuid::new_v4().simple());
        node_a.add_user(&upn, "Ada").await;
        let session = node_b.session(&upn).await;
        session.skypetoken().await.unwrap();
        let scope = teams_auth::token::Scope::spaces();
        for _ in 0..2 {
            assert_eq!(node_a.password_grant(&upn, "wrong password", &scope).await.0, 400);
        }
        assert_eq!(
            node_b.password_grant(&upn, crate::support::PASSWORD, &scope).await.0,
            429
        );
        let mut subscription = node_b.app.fanout.subscribe("push:test").await.unwrap();
        node_a
            .app
            .fanout
            .publish("push:test", Bytes::from_static(b"x"))
            .await
            .unwrap();
        assert_eq!(next(&mut subscription).await, "x");
        let ready = reqwest::get(node_b.url("/health/ready")).await.unwrap();
        assert_eq!(ready.status(), 204, "{backend:?}");
        node_b.stop().await;
        node_a.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_success_clears_the_failures_counted_in_redis() {
    let Some(url) = redis_url() else { return };
    let server = TestServer::start_with(Backend::Sqlite, |config| {
        config.redis_url = Some(url);
        config.login.max_failures = 2;
    })
    .await;
    let upn = format!("{}@own.test", uuid::Uuid::new_v4().simple());
    server.add_user(&upn, "Ada").await;
    let scope = teams_auth::token::Scope::spaces();
    assert_eq!(server.password_grant(&upn, "wrong password", &scope).await.0, 400);
    assert_eq!(
        server.password_grant(&upn, crate::support::PASSWORD, &scope).await.0,
        200
    );
    assert_eq!(server.password_grant(&upn, "wrong password", &scope).await.0, 400);
    assert_eq!(
        server.password_grant(&upn, crate::support::PASSWORD, &scope).await.0,
        200
    );
    server.stop().await;
}

#[tokio::test]
async fn an_unreachable_redis_stops_the_node_from_starting() {
    assert!(Redis::connect("redis://127.0.0.1:1").await.is_err());
    assert!(Redis::connect("not a url").await.is_err());
}

#[tokio::test]
async fn the_file_store_keeps_replaces_and_removes_by_key() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}", dir.path().join("objects").display());
    for url in [url.as_str(), "memory:///"] {
        let storage = ObjectStorage::open(url, &std::collections::BTreeMap::new()).unwrap();
        assert_eq!(storage.get("pictures/a.png").await.unwrap(), None);
        storage.put("pictures/a.png", Bytes::from_static(b"one")).await.unwrap();
        storage.put("pictures/a.png", Bytes::from_static(b"two")).await.unwrap();
        assert_eq!(storage.get("pictures/a.png").await.unwrap().unwrap(), "two");
        storage.delete("pictures/a.png").await.unwrap();
        storage.delete("pictures/a.png").await.unwrap();
        assert_eq!(storage.get("pictures/a.png").await.unwrap(), None);
        for key in ["", "../escape", "a/../b"] {
            assert!(
                matches!(storage.put(key, Bytes::new()).await, Err(StorageError::Key)),
                "{key}"
            );
        }
    }
    assert!(matches!(
        ObjectStorage::open("nonsense", &std::collections::BTreeMap::new()),
        Err(StorageError::Url(_))
    ));
    assert!(matches!(
        ObjectStorage::open("gopher://x/y", &std::collections::BTreeMap::new()),
        Err(StorageError::Url(_))
    ));
}

/// A service route that takes a skypetoken, as the later services do.
async fn whoami(skype: Skype) -> String {
    skype.0.mri()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_takes_the_skypetoken_in_each_form_teams_sends_and_refuses_anything_else() {
    let server = TestServer::start(Backend::Sqlite).await;
    let oid = server.add_user("ada@own.test", "Ada").await;
    let session = server.session("ada@own.test").await;
    let skypetoken = session.skypetoken().await.unwrap();
    let bearer = session.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
    let app: std::sync::Arc<App> = std::sync::Arc::clone(&server.app);
    let router: Router = Router::new().route("/whoami", get(whoami)).with_state(app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let http = reqwest::Client::new();
    let url = format!("http://{address}/whoami");
    let token = skypetoken.token.expose();
    for (name, value) in [
        ("x-skypetoken", token.to_owned()),
        ("authentication", format!("skypetoken={token}")),
        ("authorization", format!("skype_token {token}")),
    ] {
        let answer = http.get(&url).header(name, value).send().await.unwrap();
        assert_eq!(answer.status(), 200, "{name}");
        assert_eq!(answer.text().await.unwrap(), format!("8:orgid:{oid}"));
    }
    for (name, value) in [
        ("x-skypetoken", bearer.token.expose().to_owned()),
        ("authorization", format!("Bearer {token}")),
        ("x-skypetoken", "garbage".to_owned()),
    ] {
        let answer = http.get(&url).header(name, value).send().await.unwrap();
        assert_eq!(answer.status(), 401, "{name}");
    }
    assert_eq!(http.get(&url).send().await.unwrap().status(), 401);
    server.stop().await;
}
