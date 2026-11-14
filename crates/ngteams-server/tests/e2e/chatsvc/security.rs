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
