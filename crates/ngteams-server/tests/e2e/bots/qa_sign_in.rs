//! QA of task 233 on each backend, as a person and an administrator see sign-in cards: the person signs out, and the
//! bot's next card signs them in again through the engine; a code of another person, a used code, an expired code or
//! link, and wrong codes are refused, and the bot's refusal of a code reaches the engine as the client reads it; an
//! administrator's changes to a connection are audited without the secret. Accounts, apps, providers and tokens are
//! synthetic.

use reqwest::Method;
use serde_json::{Value, json};
use teams_proto::bot::{InvokeAnswer, InvokeKind};

use super::connector::Setup;
use super::endpoint::{ORIGIN, Scripted};
use super::idp::{self, CLIENT_SECRET, Idp};
use super::registry::audit_actions;
use super::sign_in::{
    CONNECTION, add_connection, codes_held, connection_body, connection_path, engine_token, get_token, has_token,
    holds, link_for, prompt, sign_in_config, sign_out, started, synced_link, token_service, wrong_code,
};
use super::{app_path, error_code};
use crate::support::backends;

/// The bot answers `signin/verifyState` as an SDK does: 200 for `right`, else `refusal`.
fn verifies(setup: &Setup, right: &str, refusal: u16) {
    let right = right.to_owned();
    setup.bot.script(move |activity| match activity["name"].as_str() {
        Some("signin/verifyState") if activity["value"]["state"] == right.as_str() => Scripted::empty(200),
        Some("signin/verifyState") => Scripted::empty(refusal),
        _ => Scripted::empty(404),
    });
}

/// Ada's engine sends `code`; the bot gets it with no token of Ada's; the engine's answer.
async fn ada_sends(setup: &Setup, code: &str) -> InvokeAnswer {
    let answer = setup
        .invoke(Some(&setup.chat()), InvokeKind::VerifyState { code: code.to_owned() })
        .await;
    let got = setup.bot.next_of("invoke").await;
    assert_eq!(got.activity["name"], "signin/verifyState");
    assert_eq!(got.activity["value"], json!({"state": code}));
    assert!(!holds(&got.activity, &engine_token(setup).await), "{}", got.activity);
    assert!(
        !got.activity.to_string().contains("idp-access"),
        "no provider token in the invoke: {}",
        got.activity
    );
    answer
}

/// Sets every sign-in of the server as if its 15 minutes ran out.
async fn age_sign_ins(setup: &Setup) {
    use diesel_async::SimpleAsyncConnection as _;
    use ngteams_server::db::with_conn;
    async {
        with_conn!(&setup.registry.server.app.db, |conn| conn
            .batch_execute("UPDATE bot_sign_ins SET expires_at = 1")
            .await?);
        Ok::<_, ngteams_server::db::DbError>(())
    }
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_who_signed_out_signs_in_again_from_the_bots_next_card_on_each_backend() {
    for backend in backends() {
        let setup = Setup::open_with(backend, sign_in_config).await;
        let idp = Idp::start().await;
        add_connection(&setup, &idp).await;
        let connector = started(&setup).await;

        let first = prompt(&setup, &connector).await;
        assert_eq!(synced_link(&setup).await, first);
        let (code, _) = idp::sign_in_in_browser(&first, ORIGIN, &setup.registry.server.origin).await;
        verifies(&setup, &code, 412);
        assert_eq!(ada_sends(&setup, &code).await, InvokeAnswer::Done, "{backend:?}");
        let (status, token) = token_service(&setup, &connector, Method::GET, &get_token(&setup.ada, Some(&code))).await;
        assert_eq!((status, &token["token"]), (200, &json!("idp-access-1")), "{token}");

        sign_out(&setup, &connector).await;
        assert!(!has_token(&setup, &connector).await, "signed out");
        let status = idp::link_status(&first, ORIGIN, &setup.registry.server.origin).await;
        assert_eq!(status, 404, "the first card's link doesn't open again");

        // the bot finds no token and sends a new card; Ada signs in from it
        let second = prompt(&setup, &connector).await;
        assert_ne!(second, first);
        assert_eq!(synced_link(&setup).await, second, "Ada reads the new card's link");
        let (again, _) = idp::sign_in_in_browser(&second, ORIGIN, &setup.registry.server.origin).await;
        verifies(&setup, &again, 412);
        assert_eq!(ada_sends(&setup, &again).await, InvokeAnswer::Done);
        let (status, token) =
            token_service(&setup, &connector, Method::GET, &get_token(&setup.ada, Some(&again))).await;
        assert_eq!((status, &token["token"]), (200, &json!("idp-access-2")), "{token}");
        assert!(has_token(&setup, &connector).await);
        assert_eq!(idp.issued(), (2, 0));
        setup.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn codes_of_another_person_used_expired_or_wrong_are_refused_on_each_backend() {
    for backend in backends() {
        let setup = Setup::open_with(backend, sign_in_config).await;
        let idp = Idp::start().await;
        add_connection(&setup, &idp).await;
        let bob = setup.registry.server.add_user("bob@own.test", "Bob Byte").await;
        let connector = started(&setup).await;
        let link = prompt(&setup, &connector).await;
        let (code, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
        let get = |user: &str, code: Option<&str>| get_token(user, code);

        // the bot refuses a wrong code as an SDK does, and the engine gets that refusal for the code field
        let wrong = wrong_code(&code);
        for refusal in [412, 404] {
            verifies(&setup, &code, refusal);
            assert_eq!(
                ada_sends(&setup, wrong).await,
                InvokeAnswer::Refused(refusal),
                "{backend:?}"
            );
        }

        // Bob's tries with Ada's code sign no one in and do not end Ada's sign-in
        for _ in 0..5 {
            let (status, _) = token_service(&setup, &connector, Method::GET, &get(&bob, Some(&code))).await;
            assert_eq!(status, 404, "Ada's code doesn't sign Bob in");
        }
        // four wrong codes at the token service keep Ada's sign-in
        for _ in 0..4 {
            let (status, _) = token_service(&setup, &connector, Method::GET, &get(&setup.ada, Some(wrong))).await;
            assert_eq!(status, 404);
        }
        let (status, token) = token_service(&setup, &connector, Method::GET, &get(&setup.ada, Some(&code))).await;
        assert_eq!((status, &token["token"]), (200, &json!("idp-access-1")), "{token}");
        assert_eq!(codes_held(&setup).await, 0, "a used code is gone before the sign-out");
        let (status, _) = token_service(&setup, &connector, Method::GET, &get(&bob, None)).await;
        assert_eq!(status, 404, "Bob has no token");

        // after the sign-out, the code signs in no one
        sign_out(&setup, &connector).await;
        let (status, _) = token_service(&setup, &connector, Method::GET, &get(&setup.ada, Some(&code))).await;
        assert_eq!(status, 404, "after the sign-out, no code signs in");

        // a code whose 15 minutes ran out, and a link whose 15 minutes ran out
        let link = link_for(&setup, &connector).await;
        let (late, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
        let unopened = link_for(&setup, &connector).await;
        age_sign_ins(&setup).await;
        let (status, _) = token_service(&setup, &connector, Method::GET, &get(&setup.ada, Some(&late))).await;
        assert_eq!(status, 404, "an expired code");
        let status = idp::link_status(&unopened, ORIGIN, &setup.registry.server.origin).await;
        assert_eq!(status, 404, "an expired link");

        // five wrong codes end the sign-in
        let link = link_for(&setup, &connector).await;
        let (fresh, _) = idp::sign_in_in_browser(&link, ORIGIN, &setup.registry.server.origin).await;
        let wrong = wrong_code(&fresh);
        for _ in 0..5 {
            let (status, _) = token_service(&setup, &connector, Method::GET, &get(&setup.ada, Some(wrong))).await;
            assert_eq!(status, 404);
        }
        let (status, _) = token_service(&setup, &connector, Method::GET, &get(&setup.ada, Some(&fresh))).await;
        assert_eq!(status, 404, "the sign-in ended after five wrong codes");
        assert!(!has_token(&setup, &connector).await);
        setup.stop().await;
    }
}

/// The audit's newest entries, which never hold the secret.
async fn audit(setup: &Setup) -> Vec<(String, String, Value)> {
    let (status, audit) = setup.registry.call(Method::GET, "/api/admin/v1/audit", None).await;
    assert_eq!(status, 200, "{audit}");
    assert!(!holds(&audit, CLIENT_SECRET), "{audit}");
    audit_actions(&setup.registry).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_administrators_connection_changes_are_audited_without_the_secret_on_each_backend() {
    for backend in backends() {
        let setup = Setup::open_with(backend, sign_in_config).await;
        let idp = Idp::start().await;
        let path = connection_path(&setup.bot_id, CONNECTION);
        add_connection(&setup, &idp).await;
        let mut changed = connection_body(&idp);
        changed["scopes"] = json!("repo");
        let (status, answer) = setup.registry.call(Method::PUT, &path, Some(changed)).await;
        assert_eq!(status, 200, "{answer}");
        assert!(!holds(&answer, CLIENT_SECRET), "{answer}");
        let (status, _) = setup.registry.call(Method::DELETE, &path, None).await;
        assert_eq!(status, 204);

        let entries = audit(&setup).await;
        let actions: Vec<&str> = entries.iter().take(3).map(|entry| entry.0.as_str()).collect();
        assert_eq!(
            actions,
            ["app.connection.delete", "app.connection.change", "app.connection.add"],
            "{backend:?}: {entries:?}"
        );
        for (_, _, detail) in entries.iter().take(3) {
            assert_eq!(detail["connection"], CONNECTION, "{entries:?}");
        }

        // a connection that isn't there: not found, and no audit entry
        let (status, answer) = setup.registry.call(Method::DELETE, &path, None).await;
        assert_eq!(status, 404, "{answer}");
        assert_eq!(audit(&setup).await.len(), entries.len());
        let list = format!("{}/connections", app_path(&setup.bot_id));
        let (status, answer) = setup.registry.call(Method::GET, &list, None).await;
        assert_eq!((status, &answer["value"]), (200, &json!([])));
        setup.stop().await;

        // without `bots.token_key`, a change is refused too
        let keyless = Setup::open(backend).await;
        let mut change = connection_body(&idp);
        change.as_object_mut().unwrap().remove("client_secret");
        let path = connection_path(&keyless.bot_id, CONNECTION);
        let (status, answer) = keyless.registry.call(Method::PUT, &path, Some(change)).await;
        assert_eq!((status, error_code(&answer)), (409, "SignInNotConfigured"), "{answer}");
        keyless.stop().await;
    }
}
