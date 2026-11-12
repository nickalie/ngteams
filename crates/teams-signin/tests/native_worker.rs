//! Native worker entry point (`WebView2` on Windows, `WKWebView` on macOS) rejects invocations without a private parent
//! protocol, before opening a browser.
use std::process::{Command, Stdio};

#[test]
fn native_worker_rejects_an_unconnected_invocation_without_opening_a_browser() {
    let output = Command::new(env!("CARGO_BIN_EXE_ngteams-webview"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    // Linux has no such worker (WPE's is `ngteams-web`): the program only says what it is
    #[cfg(target_os = "linux")]
    {
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("the Windows and macOS sign-in worker"));
    }
    // WebView2's and WKWebView's worker answer on their protocol's stdout
    #[cfg(any(windows, target_os = "macos"))]
    {
        assert!(output.stderr.is_empty());
        let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(failure["Failed"].as_str().unwrap().contains("started by ngteams"));
    }
}

#[cfg(windows)]
#[test]
fn native_worker_rejects_a_closed_private_pipe_before_opening_a_browser() {
    let output = Command::new(env!("CARGO_BIN_EXE_ngteams-webview"))
        .arg("--guarded-signin")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.stderr.is_empty());
    let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(failure["Failed"].as_str().unwrap().contains("private pipe failed"));
}

#[cfg(windows)]
#[test]
fn native_worker_rejects_version_mismatches_and_environment_overrides_before_opening_a_browser() {
    use std::io::Write as _;

    let directory = tempfile::tempdir().unwrap();
    let identity = format!(
        "{}:webview2-v1:{}:test-support={}",
        env!("CARGO_PKG_VERSION"),
        option_env!("NGTEAMS_BUILD_COMMIT").unwrap_or(""),
        teams_signin::test_support_enabled()
    );
    for (identity, reason) in [
        ("synthetic-wrong-identity", "versions differ"),
        (identity.as_str(), "overrides"),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ngteams-webview"))
            .arg("--guarded-signin")
            .env("WEBVIEW2_SYNTHETIC_OPTIONS", "synthetic")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(
            child.stdin.take().unwrap(),
            "{}",
            serde_json::json!({
                "identity":identity,"app_id":"com.nickalie.ngteams.signin.synthetic",
                "authorize":"https://127.0.0.1/unused","proxy":"http://127.0.0.1:1",
                "directory":directory.path(),
            })
        )
        .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(failure["Failed"].as_str().unwrap().contains(reason));
    }
}
