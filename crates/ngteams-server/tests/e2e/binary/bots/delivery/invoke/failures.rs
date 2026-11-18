use std::time::{Duration, Instant};

use serde_json::json;
use teams_proto::bot::InvokeAnswer;

use super::super::{backends, tls_parent};
use super::endpoint::Reply;
use super::support::{Case, fetch};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bot_errors_bad_json_and_large_answers_do_not_break_the_next_invoke() {
    if tls_parent(
        "binary::bots::delivery::invoke::failures::bot_errors_bad_json_and_large_answers_do_not_break_the_next_invoke",
    ) {
        return;
    }
    for backend in backends() {
        let case = Case::open(backend, "errors").await;
        let replies = [
            (Reply::Json(500, json!({"error": "synthetic failure"})), 500),
            (Reply::Json(401, json!({"error": "bot credentials"})), 502),
            (Reply::Json(403, json!({"error": "bot access"})), 502),
            (Reply::Json(407, json!({"error": "bot proxy"})), 502),
            (Reply::Text, 502),
            (Reply::Huge, 502),
        ];
        for (reply, status) in replies {
            case.bot.reply(reply);
            assert_eq!(case.invoke(&case.ada, fetch()).await, InvokeAnswer::Refused(status));
            case.got(&case.ada, "composeExtension/fetchCommands").await;
            case.bot.quiet().await;
            case.bot.reply(Reply::Normal);
            assert!(matches!(
                case.invoke(&case.ada, fetch()).await,
                InvokeAnswer::Commands(_)
            ));
            case.got(&case.ada, "composeExtension/fetchCommands").await;
        }
        case.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_twelve_second_bot_times_out_at_ten_seconds_without_a_retry() {
    if tls_parent(
        "binary::bots::delivery::invoke::failures::a_twelve_second_bot_times_out_at_ten_seconds_without_a_retry",
    ) {
        return;
    }
    for backend in backends() {
        let case = Case::open(backend, "timeout").await;
        case.bot.reply(Reply::Slow);
        let started = Instant::now();
        let pending = case.invoke(&case.ada, fetch());
        let independent = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let before = Instant::now();
            case.ada.sync().await;
            assert!(before.elapsed() < Duration::from_secs(3));
        };
        let (answer, ()) = tokio::join!(pending, independent);
        let elapsed = started.elapsed();
        eprintln!("the 12 s bot returned {answer:?} after {elapsed:?}");
        assert_eq!(answer, InvokeAnswer::Refused(504));
        assert!((Duration::from_secs(9)..Duration::from_secs(12)).contains(&elapsed));
        case.got(&case.ada, "composeExtension/fetchCommands").await;
        case.bot.reply(Reply::Normal);
        assert!(matches!(
            case.invoke(&case.ada, fetch()).await,
            InvokeAnswer::Commands(_)
        ));
        case.got(&case.ada, "composeExtension/fetchCommands").await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        case.bot.quiet().await;
        case.close().await;
    }
}
