//! The sign-in window's rules (`teams_app::signin_bound::classify`): reads, the Entra token endpoint with its captured queries,
//! Teams' authz exchange without a query and Entra's sign-in form; everything else, lookalikes among them, unknown.

use teams_app::signin_bound::classify::{Classification, Classifier, OwnKind, OwnRule, QueryRule, ValueRule};

/// The token endpoint with the query the web client sends most: its request id alone.
const TOKEN: &str = "https://login.microsoftonline.com/0f0e0d0c-0b0a-0908-0706-050403020100/oauth2/v2.0/token\
                     ?client-request-id=0a1b2c3d-4e5f-4061-8293-a4b5c6d7e8f9";
/// The token endpoint's path, with no query.
const TOKEN_PATH: &str = "https://login.microsoftonline.com/0f0e0d0c-0b0a-0908-0706-050403020100/oauth2/v2.0/token";
/// A request id as the web client makes one.
const REQUEST_ID: &str = "0a1b2c3d-4e5f-4061-8293-a4b5c6d7e8f9";
/// The broker's parameters the web client's nested app adds before its request id (the captured values).
const BROKER: &str =
    "client_id=f4060917-6abe-40d7-baa6-f634c0eda4ac&brk_client_id=5e3ce6c0-2b1f-4285-8d4b-75ee78787346";
const CHAT: &str = "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations/19:lab@thread.v2/messages";

/// The seeded rules' verdict on `method` `url`.
fn classify(method: &str, url: &str) -> Classification {
    Classifier::seeded().classify(method, url)
}

#[test]
fn reading_methods_are_reads_whatever_the_url() {
    for method in ["GET", "HEAD", "OPTIONS", "get", "Options"] {
        assert_eq!(classify(method, CHAT), Classification::Read, "{method}");
        assert_eq!(classify(method, "not a url"), Classification::Read, "{method}");
    }
}

#[test]
fn every_other_method_is_mutating_and_unknown_outside_the_own_list() {
    for method in [
        "POST", "PUT", "PATCH", "DELETE", "post", "PROPFIND", "CONNECT", "TRACE", "",
    ] {
        assert_eq!(classify(method, CHAT), Classification::Unknown, "{method}");
    }
}

/// The two query forms the captures show for the token endpoint, every value form among them.
fn captured_token_queries() -> Vec<String> {
    let redirect = |origin: &str| format!("https%3A%2F%2F{origin}%2Fv2%2Fauthv2");
    vec![
        format!("client-request-id={REQUEST_ID}"),
        format!("client-request-id=Core-{REQUEST_ID}"),
        format!(
            "{BROKER}&brk_redirect_uri={}&client-request-id={REQUEST_ID}",
            redirect("teams.cloud.microsoft")
        ),
        format!(
            "{BROKER}&brk_redirect_uri={}&client-request-id={REQUEST_ID}",
            redirect("teams.microsoft.com")
        ),
        // a set is a set: its order plays no part
        format!(
            "client-request-id={REQUEST_ID}&brk_redirect_uri={}&{BROKER}",
            redirect("teams.microsoft.com")
        ),
    ]
}

#[test]
fn the_entra_token_endpoint_of_any_tenant_is_own_with_a_captured_query() {
    for tenant in [
        "0f0e0d0c-0b0a-0908-0706-050403020100",
        "common",
        "organizations",
        "contoso.example",
    ] {
        for query in captured_token_queries() {
            let url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token?{query}");
            assert_eq!(
                classify("POST", &url),
                Classification::Own(OwnKind::EntraToken),
                "{url}"
            );
        }
    }
    assert_eq!(classify("post", TOKEN), Classification::Own(OwnKind::EntraToken));
    assert_eq!(
        classify(
            "POST",
            &format!("https://login.microsoftonline.com:443/common/oauth2/v2.0/token?client-request-id={REQUEST_ID}"),
        ),
        Classification::Own(OwnKind::EntraToken),
        "the default port is the same origin"
    );
}

#[test]
fn the_token_endpoint_with_any_other_query_is_unknown() {
    let redirect = "brk_redirect_uri=https%3A%2F%2Fteams.microsoft.com%2Fv2%2Fauthv2";
    let other_queries = [
        // no query, or an empty one
        String::new(),
        "?".to_owned(),
        // a request id in a form no capture showed
        "?client-request-id=1".to_owned(),
        format!("?client-request-id={}", REQUEST_ID.to_uppercase()),
        format!("?client-request-id={{{REQUEST_ID}}}"),
        format!("?client-request-id=Other-{REQUEST_ID}"),
        // the `Core-` form only ever came alone
        format!("?{BROKER}&{redirect}&client-request-id=Core-{REQUEST_ID}"),
        // a key twice, in any spelling that decodes to it, or a key in another case
        format!("?client-request-id={REQUEST_ID}&client-request-id={REQUEST_ID}"),
        format!("?client-request-id={REQUEST_ID}&client%2Drequest-id={REQUEST_ID}"),
        format!("?Client-Request-Id={REQUEST_ID}"),
        format!("?{BROKER}&{redirect}&client-request-id={REQUEST_ID}&client_id=f4060917-6abe-40d7-baa6-f634c0eda4ac"),
        // a key no capture showed, alone or beside a captured set
        format!("?client-request-id={REQUEST_ID}&prompt=none"),
        format!("?{BROKER}&{redirect}&client-request-id={REQUEST_ID}&x=1"),
        "?x=1".to_owned(),
        // a captured set with a key missing
        format!("?{BROKER}&client-request-id={REQUEST_ID}"),
        format!("?{redirect}&client-request-id={REQUEST_ID}"),
        // a broker value no capture showed
        format!(
            "?client_id=00000000-0000-4000-8000-000000000001&brk_client_id=5e3ce6c0-2b1f-4285-8d4b-75ee78787346&{redirect}\
             &client-request-id={REQUEST_ID}"
        ),
        format!(
            "?{BROKER}&brk_redirect_uri=https%3A%2F%2Fattacker.example%2Fv2%2Fauthv2&client-request-id={REQUEST_ID}"
        ),
        // pairs that can't be read with certainty
        format!("?client-request-id={REQUEST_ID}&"),
        format!("?&client-request-id={REQUEST_ID}"),
        format!("?client-request-id={REQUEST_ID}&&x=1"),
        format!("?client-request-id={REQUEST_ID}&flag"),
    ];
    for query in other_queries {
        let url = format!("{TOKEN_PATH}{query}");
        assert_eq!(classify("POST", &url), Classification::Unknown, "{url}");
        assert_eq!(
            Classifier::sign_in().classify("POST", &url),
            Classification::Unknown,
            "{url}"
        );
    }
}

const AUTHZ_URLS: [&str; 4] = [
    "https://teams.microsoft.com/api/authsvc/v1.0/authz",
    "https://teams.microsoft.com/api/authsvc/v1.0/authzcore",
    "https://authsvc.teams.microsoft.com/v1.0/authz",
    "https://authsvc.teams.microsoft.com/v1.0/authzcore",
];

#[test]
fn the_authz_endpoints_are_own_only_without_a_query() {
    for url in AUTHZ_URLS {
        assert_eq!(classify("POST", url), Classification::Own(OwnKind::Authz), "{url}");
        for query in ["?", "?x=1", "?client-request-id=0a1b2c3d-4e5f-4061-8293-a4b5c6d7e8f9"] {
            let with_query = format!("{url}{query}");
            assert_eq!(classify("POST", &with_query), Classification::Unknown, "{with_query}");
        }
    }
}

#[test]
fn lookalikes_of_own_endpoints_are_unknown() {
    let lookalikes = [
        ("PUT", TOKEN_PATH),
        ("DELETE", "https://teams.microsoft.com/api/authsvc/v1.0/authz"),
        ("POST", "http://login.microsoftonline.com/common/oauth2/v2.0/token"),
        (
            "POST",
            "https://login.microsoftonline.com:8443/common/oauth2/v2.0/token",
        ),
        (
            "POST",
            "https://login.microsoftonline.com.attacker.example/common/oauth2/v2.0/token",
        ),
        (
            "POST",
            "https://attacker.example/login.microsoftonline.com/common/oauth2/v2.0/token",
        ),
        ("POST", "https://login.microsoftonline.com//oauth2/v2.0/token"),
        ("POST", "https://login.microsoftonline.com/oauth2/v2.0/token"),
        ("POST", "https://login.microsoftonline.com/common/oauth2/v2.0/token/"),
        (
            "POST",
            "https://login.microsoftonline.com/common/oauth2/v2.0/token/more",
        ),
        ("POST", "https://login.microsoftonline.com/a/b/oauth2/v2.0/token"),
        ("POST", "https://login.microsoftonline.com/common/oauth2/token"),
        ("POST", "https://login.microsoftonline.com/common/oauth2/v2.0/%74oken"),
        ("POST", "https://teams.microsoft.com/api/authsvc/v1.0/authz/extra"),
        ("POST", "https://teams.microsoft.com/api/authsvc/v1.0/users/region"),
        ("POST", "https://teams.live.com/api/auth/v2.0/authz/consumer"),
        ("POST", "not a url"),
        ("POST", ""),
    ];
    for (method, url) in lookalikes {
        assert_eq!(classify(method, url), Classification::Unknown, "{method} {url}");
        // with the token endpoint's captured query too, so only the method, origin and path are what's off
        let with_query = format!("{url}?client-request-id={REQUEST_ID}");
        assert_eq!(
            classify(method, &with_query),
            Classification::Unknown,
            "{method} {with_query}"
        );
    }
}

#[test]
fn a_local_rule_stands_in_for_an_own_endpoint_without_widening_the_seeded_list() {
    let classifier = Classifier::seeded().with_own(OwnRule::new(
        OwnKind::Authz,
        "post",
        "http://127.0.0.1:4000",
        "/own/authz",
    ));
    assert_eq!(
        classifier.classify("POST", "http://127.0.0.1:4000/own/authz"),
        Classification::Own(OwnKind::Authz)
    );
    assert_eq!(
        classifier.classify("POST", "http://127.0.0.1:4001/own/authz"),
        Classification::Unknown
    );
    assert_eq!(
        classify("POST", "http://127.0.0.1:4000/own/authz"),
        Classification::Unknown,
        "the seeded classifier doesn't know it"
    );
    assert_eq!(
        classifier.classify("POST", TOKEN),
        Classification::Own(OwnKind::EntraToken)
    );
    // a local rule takes no query unless it is given the one it takes
    assert_eq!(
        classifier.classify("POST", "http://127.0.0.1:4000/own/authz?x=1"),
        Classification::Unknown
    );
    let with_query = Classifier::seeded().with_own(
        OwnRule::new(OwnKind::Authz, "POST", "http://127.0.0.1:4000", "/own/authz")
            .with_query(QueryRule::OneOf(&[&[("x", ValueRule::Exact(&["1"]))]])),
    );
    assert_eq!(
        with_query.classify("POST", "http://127.0.0.1:4000/own/authz?x=1"),
        Classification::Own(OwnKind::Authz)
    );
    for url in [
        "http://127.0.0.1:4000/own/authz",
        "http://127.0.0.1:4000/own/authz?x=2",
        "http://127.0.0.1:4000/own/authz?x=1&x=1",
    ] {
        assert_eq!(with_query.classify("POST", url), Classification::Unknown, "{url}");
    }
}

#[test]
fn the_sign_in_form_still_takes_any_query_and_only_with_login() {
    for url in [
        "https://login.microsoftonline.com/common/login",
        "https://login.microsoftonline.com/common/login?ctx=1&x=y",
        "https://login.microsoftonline.com/common/GetCredentialType?mkt=en-US&mkt=de-DE",
    ] {
        assert_eq!(
            Classifier::sign_in().classify("POST", url),
            Classification::Own(OwnKind::EntraSignIn),
            "{url}"
        );
        assert_eq!(classify("POST", url), Classification::Unknown, "{url}");
    }
    // the seeded rules keep their captured queries under login too
    assert_eq!(
        Classifier::sign_in().classify("POST", &format!("{TOKEN_PATH}?x=1")),
        Classification::Unknown
    );
}

#[test]
fn own_kinds_are_named_for_the_log() {
    assert_eq!(OwnKind::EntraToken.as_str(), "entra_token");
    assert_eq!(OwnKind::Authz.as_str(), "authz");
    assert_eq!(OwnKind::EntraSignIn.as_str(), "entra_sign_in");
}
