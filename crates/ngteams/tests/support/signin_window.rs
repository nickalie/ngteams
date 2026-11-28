//! Bound windows refuse what no page could answer (a foreign authorize URL, an unusable redirect URI, a data dir that
//! can't be made) before resolving a page engine or reaching the foreground service.

use gpui::TestAppContext;

use super::*;

fn request(authorize: &str) -> SignInRequest {
    SignInRequest {
        authorize: authorize.to_owned(),
        redirect_uri: "invalid-redirect".to_owned(),
    }
}

fn window(setup: &Setup, windows: &Windows) -> Window {
    bound_window(
        setup,
        Arc::new(SignInHosts::entra_with(&[]).unwrap()),
        windows,
        "/synthetic/no-module.so".into(),
    )
}

#[test]
fn bound_windows_refuse_foreign_pages_and_report_data_failures_before_the_service() {
    let root = tempfile::tempdir().unwrap();
    let (setup, _) = crate::app::tests::setup(root.path(), 1);
    let cx = TestAppContext::single();
    let windows = cx.update(|cx| super::windows_for(&setup, cx));
    let again = cx.update(|cx| super::windows_for(&setup, cx));
    let refused = window(&setup, &windows)(request("https://example.test/authorize"));
    assert!(matches!(refused, SignInAnswer::Failed(reason) if reason.contains("isn't on the sign-in window's hosts")));
    let file = root.path().join("not-a-directory");
    std::fs::write(&file, b"_synthetic obstruction").unwrap();
    let broken = setup
        .clone()
        .with_paths(crate::accounts_file::PersonalPaths::new(file.clone(), file));
    let refused = window(&broken, &again)(request("https://login.microsoftonline.com/authorize"));
    assert!(matches!(refused, SignInAnswer::Failed(reason) if reason.contains("personal data dir")));
    assert_eq!(cx.update(|cx| cx.windows().len()), 0);
}

#[test]
fn a_bound_request_with_an_unusable_redirect_is_refused_before_any_engine() {
    let root = tempfile::tempdir().unwrap();
    let (setup, _) = crate::app::tests::setup(root.path(), 2);
    let cx = TestAppContext::single();
    let windows = cx.update(|cx| super::windows_for(&setup, cx));
    // no worker beside the test binary: resolving an engine would fail on that instead
    let refused = window(&setup, &windows)(request("https://login.microsoftonline.com/authorize"));
    assert!(matches!(refused, SignInAnswer::Failed(reason) if reason.contains("redirect URI")));
    assert_eq!(cx.update(|cx| cx.windows().len()), 0);
}

#[test]
fn a_bound_valid_request_without_a_foreground_service_fails_without_opening_a_page() {
    let root = tempfile::tempdir().unwrap();
    let (setup, _) = crate::app::tests::setup(root.path(), 2);
    let cx = TestAppContext::single();
    let windows = cx.update(|cx| super::windows_for(&setup, cx));
    let ask = window(&setup, &windows);
    drop(cx);
    let answer = ask(SignInRequest {
        authorize: "https://login.microsoftonline.com/authorize".into(),
        redirect_uri: teams_auth::token::NATIVE_CLIENT_REDIRECT.into(),
    });
    assert!(matches!(answer, SignInAnswer::Failed(_)), "{answer:?}");
}
