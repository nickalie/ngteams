//! A bot's endpoint as an administrator saves it: an address written in it that the server would never send to (a
//! cloud metadata address always; a private, loopback or link-local one unless `bots.allowed_networks` names it) is
//! refused at once with a 400 the Manage apps page shows, on the form and on a change. A name passes, as it may
//! resolve elsewhere by the time of a send, and the guard checks every send. Bots here are synthetic.

use reqwest::Method;
use serde_json::{Value, json};

use super::connector::loopback;
use super::{APPS, Registry, app_path, error_code};
use crate::support::Backend;

async fn add(registry: &Registry, endpoint: &str) -> (u16, Value) {
    let body = json!({"name": "Guarded (synthetic)", "endpoint": endpoint});
    registry.call(Method::POST, APPS, Some(body)).await
}

fn refused(answer: &Value, endpoint: &str) {
    assert_eq!(error_code(answer), "EndpointNotAllowed", "{endpoint}: {answer}");
    let message = answer["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("bots.allowed_networks"), "{endpoint}: {message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_on_a_network_the_server_never_reaches_is_refused_when_saved() {
    // loopback (127.0.0.0/8) is open, as for a bot on the same host
    let registry = Registry::open_with(Backend::Sqlite, loopback).await;
    for endpoint in [
        "https://169.254.169.254/latest/meta-data",
        "https://[::ffff:169.254.169.254]/latest",
        "https://[fd00:ec2::254]/latest",
        "http://100.100.100.200/latest",
        "http://10.1.2.3/api/messages",
        "https://192.168.1.10/api/messages",
        "https://169.254.10.10/api/messages",
        "http://[::1]:3978/api/messages",
        "https://[fe80::1]/api/messages",
        "https://0.0.0.0/api/messages",
    ] {
        let (status, answer) = add(&registry, endpoint).await;
        assert_eq!(status, 400, "{endpoint}: {answer}");
        refused(&answer, endpoint);
    }
    for endpoint in [
        "http://127.0.0.1:3978/api/messages",
        "http://localhost:3978/api/messages",
        "https://bots.example.test/api/messages",
        "https://93.184.216.34/api/messages",
    ] {
        let (status, answer) = add(&registry, endpoint).await;
        assert_eq!(status, 201, "{endpoint}: {answer}");
    }

    // a change to such an address is refused too, and the endpoint stays
    let (_, made) = add(&registry, "https://bots.example.test/api/messages").await;
    let path = app_path(made["id"].as_str().unwrap());
    let (status, answer) = registry
        .call(
            Method::PATCH,
            &path,
            Some(json!({"endpoint": "https://169.254.169.254/latest"})),
        )
        .await;
    assert_eq!(status, 400, "{answer}");
    refused(&answer, "a change");
    let (_, app) = registry.call(Method::GET, &path, None).await;
    assert_eq!(app["bot"]["endpoint"], "https://bots.example.test/api/messages");
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn allowed_networks_open_a_private_address_but_never_metadata() {
    let registry = Registry::open_with(Backend::Sqlite, |config| {
        config.bots.allow_http_endpoints = true;
        config.bots.allowed_networks = vec!["10.0.0.0/8".parse().unwrap(), "169.254.0.0/16".parse().unwrap()];
    })
    .await;
    let (status, answer) = add(&registry, "http://10.1.2.3/api/messages").await;
    assert_eq!(status, 201, "{answer}");
    let (status, answer) = add(&registry, "http://169.254.10.10/api/messages").await;
    assert_eq!(status, 201, "{answer}");
    let (status, answer) = add(&registry, "http://169.254.169.254/latest").await;
    assert_eq!(status, 400, "{answer}");
    refused(&answer, "metadata");
    let (status, answer) = add(&registry, "http://127.0.0.1:3978/api/messages").await;
    assert_eq!(status, 400, "{answer}");
    refused(&answer, "loopback");
    registry.server.stop().await;
}
