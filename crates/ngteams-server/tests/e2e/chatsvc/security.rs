//! Authorization and request validation use actual accounts and the client's typed requests.

use teams_auth::token::Scope;
use teams_proto::{CreateChat, Mri};

use super::client;
use crate::support::{Backend, PASSWORD, TestServer};

#[tokio::test(flavor = "multi_thread")]
async fn chat_creation_binds_the_actor_and_refuses_unknown_people_and_wrong_credentials() {
    let server = TestServer::start(Backend::Sqlite).await;
    let a = server.add_user("a@own.test", "Ada").await;
    let b = server.add_user("b@own.test", "Bert").await;
    let c = server.add_user("c@own.test", "Cora").await;
    let client = client(&server, "a@own.test", &a).await;
    let other = |id: &str| Mri::new(format!("8:orgid:{id}"));
    let absent_actor = CreateChat::one_to_one(other(&b), other(&c)).unwrap();
    assert_eq!(client.write(absent_actor).await.unwrap().status(), 403);
    let unknown = CreateChat::one_to_one(other(&a), other("00000000-0000-4000-8000-000000000001")).unwrap();
    assert_eq!(client.write(unknown).await.unwrap().status(), 400);
    let url = server.url("/api/chatsvc/own/v1/threads");
    assert_eq!(
        reqwest::Client::new()
            .post(&url)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let (_, grant) = server.password_grant("a@own.test", PASSWORD, &Scope::spaces()).await;
    assert_eq!(
        reqwest::Client::new()
            .post(&url)
            .bearer_auth(grant["access_token"].as_str().unwrap())
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    ngteams_server::admin::set_disabled(&server.app.db, "a@own.test", true)
        .await
        .unwrap();
    let request = CreateChat::one_to_one(other(&a), other(&b)).unwrap();
    assert_eq!(client.write(request).await.unwrap().status(), 403);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_rosters_and_cursors_never_create_or_expose_a_chat() {
    let server = TestServer::start(Backend::Sqlite).await;
    let a = server.add_user("a@own.test", "Ada").await;
    let b = server.add_user("b@own.test", "Bert").await;
    let (_, grant) = server
        .password_grant("a@own.test", PASSWORD, &Scope::for_resource(teams_proto::IC3_AUDIENCE))
        .await;
    let http = reqwest::Client::new();
    let token = grant["access_token"].as_str().unwrap();
    let url = server.url("/api/chatsvc/own/v1/threads");
    let bodies = malformed_rosters(&a, &b);
    for body in bodies {
        assert_eq!(
            http.post(&url)
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    let list = server.url("/api/chatsvc/own/v1/users/ME/conversations");
    for query in [
        "pageSize=bad",
        "syncState=-1",
        "syncState=1000000000000000",
        "after=7&until=0",
    ] {
        assert_eq!(
            http.get(format!("{list}?{query}"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            400,
            "{query}"
        );
    }
    for (path, status) in [
        ("/api/chatsvc/other/v1/threads/missing", 404),
        ("/api/chatsvc/own/v1/threads/missing", 404),
        ("/api/chatsvc/own/v1/users/someone/conversations", 403),
    ] {
        assert_eq!(
            http.get(server.url(path))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            status
        );
    }
    assert!(
        teams_proto::chat::ChatReads::new(&client(&server, "a@own.test", &a).await)
            .conversations()
            .await
            .unwrap()
            .conversations
            .is_empty()
    );
    server.stop().await;
}

fn malformed_rosters(a: &str, b: &str) -> Vec<serde_json::Value> {
    let base = serde_json::json!({"members": [{"id": format!("8:orgid:{a}"), "role": "Admin"}, {"id": format!("8:orgid:{b}"), "role": "Admin"}], "properties": {"threadType": "chat"}});
    let mut bodies = vec![serde_json::json!({})];
    for (path, value) in [
        ("/members/0/id", serde_json::json!("not-an-mri")),
        ("/members/0/role", serde_json::json!("User")),
        ("/members/1/id", serde_json::json!(format!("8:orgid:{a}"))),
        ("/properties/threadType", serde_json::json!("space")),
    ] {
        let mut body = base.clone();
        *body.pointer_mut(path).unwrap() = value;
        bodies.push(body);
    }
    let mut mismatched = base.clone();
    mismatched["properties"]["fixedRoster"] = serde_json::json!(true);
    bodies.push(mismatched);
    let mut empty = base.clone();
    empty["members"] = serde_json::json!([]);
    bodies.push(empty);
    bodies
}
