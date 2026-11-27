//! QA of task 524 on each backend: a person writes to a bot faster than the server may send to it, and the bot
//! gets every message once, in order, at its rate; a bot that answers faster than the connector takes gets 429,
//! waits the `Retry-After` and sends again, and the person reads each answer once, in order. Bots and messages are
//! synthetic.

use std::time::{Duration, Instant};

use ngteams_server::bots::{PENDING, SENT, deliveries};
use reqwest::Method;

use super::app_path;
use super::connector::Setup;
use super::endpoint::Connector;
use crate::support::{Backend, backends};

const SENDS: usize = 8;

#[tokio::test(flavor = "multi_thread")]
async fn a_person_who_writes_fast_reaches_the_bot_in_order_at_its_rate_on_each_backend() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| {
            config.bots.send_rate = 2;
            config.bots.send_burst = 2;
        })
        .await;
        let chat = setup.chat();
        let started = Instant::now();
        for index in 0..SENDS {
            setup.send(&chat, &format!("fast {index}")).await;
        }
        let mut arrived = Vec::new();
        for index in 0..SENDS {
            let got = setup.bot.next_of("message").await;
            assert_eq!(got.activity["text"], format!("fast {index}"), "{backend:?}");
            arrived.push(started.elapsed());
        }
        // two at once, then two a second: the last of eight no sooner than three seconds after the first
        assert!(
            arrived[SENDS - 1] >= Duration::from_millis(2_800),
            "{backend:?} {arrived:?}"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        let rows = loop {
            let rows = deliveries(&setup.registry.server.app.db, &setup.bot_id).await.unwrap();
            if rows.iter().all(|row| row.state != PENDING) || Instant::now() > deadline {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let messages: Vec<_> = rows.iter().filter(|row| row.activity.contains("\"message\"")).collect();
        assert_eq!(messages.len(), SENDS, "{backend:?} {rows:?}");
        for row in messages {
            assert_eq!((row.state.as_str(), row.attempts), (SENT, 1), "{backend:?} {row:?}");
        }

        // the administrator sees a healthy bot: a wait is no failure
        let deadline = Instant::now() + Duration::from_secs(10);
        let health = loop {
            let (status, app) = setup.registry.call(Method::GET, &app_path(&setup.bot_id), None).await;
            assert_eq!(status, 200, "{app}");
            let health = app["health"].clone();
            if health["outbox"]["sent"].as_u64() >= Some(SENDS as u64) || Instant::now() > deadline {
                break health;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(health["state"], "healthy", "{backend:?} {health}");
        assert_eq!(health["failures"], 0, "{backend:?} {health}");
        assert_eq!(health["outbox"]["failed"], 0, "{backend:?} {health}");
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_that_answers_too_fast_waits_its_retry_after_and_the_person_reads_each_answer_once_on_each_backend() {
    for backend in backends() {
        let setup = Setup::open_with(backend, |config| {
            config.bots.connector_rate = 2;
            config.bots.connector_burst = 2;
        })
        .await;
        let chat = setup.chat();
        setup.send(&chat, "please reply six times").await;
        let got = setup.bot.next_of("message").await;
        let service_url = got.activity["serviceUrl"].as_str().unwrap().to_owned();
        let connector = setup.connector(&service_url).await;
        let path = format!("v3/conversations/{}/activities", super::endpoint::encode(&chat));

        let mut refusals = 0;
        for index in 0..6 {
            refusals += post_until_taken(&connector, &path, &format!("answer {index}"), backend).await;
        }
        assert!(refusals > 0, "{backend:?}: six answers at once were all taken");

        let messages = setup.synced(&chat).await;
        let answers: Vec<_> = messages
            .iter()
            .filter(|message| message.content.contains("answer "))
            .map(|message| message.content.clone())
            .collect();
        let expected: Vec<_> = (0..6).map(|index| format!("answer {index}")).collect();
        assert_eq!(answers.len(), 6, "{backend:?} {answers:#?}");
        for (answer, expected) in answers.iter().zip(&expected) {
            assert!(answer.contains(expected.as_str()), "{backend:?} {answers:#?}");
        }
        setup.stop().await;
    }
}

/// Posts `text` as the bot, and once more after the `Retry-After` of a 429, which must then be taken; the refusals.
async fn post_until_taken(connector: &Connector, path: &str, text: &str, backend: Backend) -> u32 {
    let mut refusals = 0;
    loop {
        let (status, answer, headers) = connector
            .call_with_headers(Method::POST, path, Some(Connector::text(text)))
            .await;
        if status == 200 {
            return refusals;
        }
        assert_eq!(status, 429, "{backend:?} {answer}");
        assert_eq!(answer["error"]["code"], "TooManyRequests", "{answer}");
        let wait: u64 = headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no Retry-After: {headers:?}"));
        assert!((1..=2).contains(&wait), "{backend:?} Retry-After {wait}");
        refusals += 1;
        assert_eq!(
            refusals, 1,
            "{backend:?}: refused again after the Retry-After of {wait}s"
        );
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}
