//! Send limits: a bot's calls to the connector over its app's rate get the Bot Framework's 429 with `Retry-After`,
//! and the server's sends to one bot keep to that bot's rate without counting a wait as a failure. Bots and messages
//! are synthetic.

use std::time::{Duration, Instant};

use ngteams_server::bots::{PENDING, SENT, deliveries};
use reqwest::Method;

use super::connector::{Setup, personal_chat};
use super::endpoint::{Connector, TestBot};
use crate::support::Backend;

fn retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("no Retry-After in seconds: {headers:?}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_over_its_connector_rate_gets_429_with_retry_after_and_another_bot_does_not() {
    let setup = Setup::open_with(Backend::Sqlite, |config| {
        config.bots.connector_rate = 1;
        config.bots.connector_burst = 3;
    })
    .await;
    let chat = setup.chat();
    setup.send(&chat, "start").await;
    let got = setup.bot.next_of("message").await;
    let service_url = got.activity["serviceUrl"].as_str().unwrap().to_owned();
    let connector = setup.connector(&service_url).await;

    let mut statuses = Vec::new();
    let mut refused = None;
    for index in 0..5 {
        let path = format!("v3/conversations/{}/activities", super::endpoint::encode(&chat));
        let (status, answer, headers) = connector
            .call_with_headers(Method::POST, &path, Some(Connector::text(&format!("burst {index}"))))
            .await;
        statuses.push(status);
        if status == 429 {
            assert_eq!(super::error_code(&answer), "TooManyRequests", "{answer}");
            refused.get_or_insert(retry_after(&headers));
        }
    }
    assert_eq!(statuses, [200, 200, 200, 429, 429]);
    let wait = refused.unwrap();
    assert!((1..=2).contains(&wait), "Retry-After {wait}");

    // the limit is per app: another bot of the same server isn't held
    let other = TestBot::start().await;
    let answer = setup.registry.register("Other (synthetic)", &other.url).await;
    let other_id = answer["id"].as_str().unwrap();
    let other_secret = answer["secret"].as_str().unwrap();
    let other_connector = Connector::signed(&setup.registry.server, &service_url, other_id, other_secret).await;
    let to_ada = personal_chat(&setup.ada, other_id);
    setup.send(&to_ada, "hello other").await;
    other.next_of("message").await;
    let (status, answer) = other_connector.send(&to_ada, Connector::text("other answers")).await;
    assert_eq!(status, 200, "{answer}");

    // the person syncs only what was taken
    let messages = setup.synced(&chat).await;
    let from_bot: Vec<_> = messages
        .iter()
        .filter(|message| message.content.contains("burst"))
        .collect();
    assert_eq!(from_bot.len(), 3, "{messages:#?}");

    tokio::time::sleep(Duration::from_secs(wait)).await;
    let (status, answer) = connector.send(&chat, Connector::text("after the wait")).await;
    assert_eq!(status, 200, "{answer}");
    setup.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_sends_to_a_bot_at_its_rate_and_a_wait_is_no_failed_attempt() {
    let setup = Setup::open_with(Backend::Sqlite, |config| {
        config.bots.send_rate = 1;
        config.bots.send_burst = 1;
    })
    .await;
    let chat = setup.chat();
    let started = Instant::now();
    for index in 0..3 {
        setup.send(&chat, &format!("quick {index}")).await;
    }
    let mut arrived = Vec::new();
    for index in 0..3 {
        let got = setup.bot.next_of("message").await;
        assert_eq!(got.activity["text"], format!("quick {index}"));
        arrived.push(started.elapsed());
    }
    // one a second: the third no sooner than two seconds after the first
    assert!(
        arrived[2].saturating_sub(arrived[0]) >= Duration::from_millis(1800),
        "{arrived:?}"
    );
    // each send is recorded after the bot answered it
    let deadline = Instant::now() + Duration::from_secs(5);
    let rows = loop {
        let rows = deliveries(&setup.registry.server.app.db, &setup.bot_id).await.unwrap();
        if rows.iter().all(|row| row.state != PENDING) || Instant::now() > deadline {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let messages: Vec<_> = rows.iter().filter(|row| row.activity.contains("\"message\"")).collect();
    assert_eq!(messages.len(), 3, "{rows:?}");
    for row in messages {
        assert_eq!((row.state.as_str(), row.attempts), (SENT, 1), "{row:?}");
    }
    setup.stop().await;
}
