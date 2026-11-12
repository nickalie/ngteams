//! The lab browser: launch arguments per profile, the debugging endpoint, the pinned Chrome for Testing install and its
//! checksum, the CDP message codec, and (with Chrome installed) a new worker target held paused until we resume it.
//!
//! The Chrome test runs headless on a throwaway user-data-dir and a local page. It finds Chrome at
//! `$PROTO_LAB_CHROME`, else at the pinned build in the lab's chrome dir, found as `proto-lab chrome install` finds it
//! (lab.toml's `data_dir` when set); without either it says so on stderr and passes, unless
//! `PROTO_LAB_REQUIRE_CHROME=1`.

// Shared CDP and local-browser contracts also run on Windows; Unix process/install tests stay scoped below.

mod common;
#[cfg(unix)]
#[path = "common/process.rs"]
mod process;

use std::ffi::OsString;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt as _;
use proto_lab::browser::{BASE_ARGS, Browser, BrowserError, DevToolsEndpoint, Display, LaunchOptions, Media};
use proto_lab::cdp::{Cdp, CdpError, Event, Events, Incoming, RemoteError, SessionId, encode_command, parse_incoming};
#[cfg(unix)]
use proto_lab::commands::chrome::{self, ChecksumError, ChromeBuild, InstallError, Installed};
use proto_lab::config::{Alias, LabPaths};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Helpers return errors; the tests unwrap them.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
/// The same, for what a spawned task returns.
type SendResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn strings(args: &[OsString]) -> Vec<String> {
    args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect()
}

// --- launch arguments ---

#[test]
fn a_profile_launches_headless_on_an_automatic_port_in_its_own_user_data_dir() {
    let paths = LabPaths::new(PathBuf::from("/lab/data"));
    let options = LaunchOptions::for_profile(&paths, Alias::B);
    let args = strings(&options.args());

    assert_eq!(options.user_data_dir, PathBuf::from("/lab/data/profiles/b"));
    assert!(args.contains(&format!("--user-data-dir={}", options.user_data_dir.display())));
    assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
    assert!(args.contains(&"--headless=new".to_owned()));
    assert!(!args.iter().any(|arg| arg.contains("fake")));
    for base in BASE_ARGS {
        assert!(args.contains(&(*base).to_owned()), "{base} missing");
    }
    assert_eq!(args.last().map(String::as_str), Some("about:blank"));
}

#[test]
fn every_lab_browser_keeps_off_the_keyring_and_google_background_traffic() {
    for flag in [
        "--password-store=basic",
        "--use-mock-keychain",
        "--disable-background-networking",
        "--disable-sync",
        "--no-first-run",
    ] {
        assert!(BASE_ARGS.contains(&flag), "{flag} missing");
    }
}

#[test]
fn every_lab_browser_plays_nothing_aloud_and_opens_no_audio_device() {
    for media in [
        Media::System,
        Media::Fake { audio: None },
        Media::Fake {
            audio: Some(PathBuf::from("/lab/tone.wav")),
        },
    ] {
        let mut options = LaunchOptions::new(PathBuf::from("/lab/p"));
        options.media = media;
        let args = strings(&options.args());
        assert!(args.contains(&"--mute-audio".to_owned()), "{args:?}");
        assert!(args.contains(&"--disable-audio-output".to_owned()), "{args:?}");
    }
}

#[test]
fn every_lab_browser_has_report_uploads_off_in_its_only_disable_features() {
    let options = LaunchOptions::new(PathBuf::from("/lab/p"));
    let disabled: Vec<String> = strings(&options.args())
        .into_iter()
        .filter(|arg| arg.starts_with("--disable-features="))
        .collect();
    // Chrome keeps only the last --disable-features, so there must be exactly one
    assert_eq!(disabled, ["--disable-features=Reporting,NetworkErrorLogging"]);
}

#[test]
fn headed_with_fake_media_still_leaves_the_port_to_chrome() {
    let options = LaunchOptions {
        user_data_dir: PathBuf::from("/tmp/profile with space"),
        display: Display::Headed,
        media: Media::Fake { audio: None },
        user_agent: None,
    };
    let args = strings(&options.args());

    assert!(args.contains(&"--user-data-dir=/tmp/profile with space".to_owned()));
    // Chrome writes DevToolsActivePort only for port 0
    assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
    assert!(!args.iter().any(|arg| arg.starts_with("--headless")));
    assert!(args.contains(&"--use-fake-ui-for-media-stream".to_owned()));
    assert!(args.contains(&"--use-fake-device-for-media-stream".to_owned()));
    assert!(
        !args
            .iter()
            .any(|arg| arg.starts_with("--use-file-for-fake-audio-capture"))
    );
}

#[test]
fn fake_media_can_play_a_wav_file_as_the_microphone() {
    let mut options = LaunchOptions::new(PathBuf::from("/p"));
    options.media = Media::Fake {
        audio: Some(PathBuf::from("/lab/tone.wav")),
    };
    let args = strings(&options.args());

    assert!(args.contains(&"--use-fake-device-for-media-stream".to_owned()));
    assert!(args.contains(&"--use-file-for-fake-audio-capture=/lab/tone.wav".to_owned()));
    // the audio tap reads a received track without waiting for a click; the output stays muted
    assert!(args.contains(&"--autoplay-policy=no-user-gesture-required".to_owned()));
    assert!(args.contains(&"--mute-audio".to_owned()));
}

#[test]
fn only_fake_media_lets_audio_start_without_a_click() {
    let args = strings(&LaunchOptions::new(PathBuf::from("/p")).args());
    assert!(!args.iter().any(|arg| arg.starts_with("--autoplay-policy")));
}

#[test]
fn a_user_agent_replaces_the_browser_s_own() {
    let mut options = LaunchOptions::new(PathBuf::from("/p"));
    assert!(
        !strings(&options.args())
            .iter()
            .any(|arg| arg.starts_with("--user-agent"))
    );
    options.user_agent = Some("Mozilla/5.0 (X11; Linux x86_64) Chrome/1.0".to_owned());
    let args = strings(&options.args());

    assert!(args.contains(&"--user-agent=Mozilla/5.0 (X11; Linux x86_64) Chrome/1.0".to_owned()));
    // the starting page stays last
    assert_eq!(args.last().map(String::as_str), Some("about:blank"));
}

// --- DevToolsActivePort ---

#[test]
fn the_active_port_file_gives_the_browser_websocket() {
    let endpoint = DevToolsEndpoint::parse("40123\n/devtools/browser/0b7c-11\n").unwrap();

    assert_eq!(endpoint.port, 40123);
    assert_eq!(endpoint.ws_url(), "ws://127.0.0.1:40123/devtools/browser/0b7c-11");
}

#[test]
fn a_partial_or_malformed_active_port_file_is_refused() {
    for text in [
        "",
        "40123",
        "40123\n",
        "0\n/devtools/browser/x",
        "port\n/devtools/browser/x",
        "1\n/json",
    ] {
        assert!(
            matches!(DevToolsEndpoint::parse(text), Err(BrowserError::BadActivePort(_))),
            "{text:?} accepted"
        );
    }
}

#[tokio::test]
#[cfg(unix)]
async fn a_missing_executable_is_reported() {
    let profile = tempfile::tempdir().unwrap();
    let missing = profile.path().join("no-chrome");
    let result = Browser::launch(
        &missing,
        &LaunchOptions::new(profile.path().join("p")),
        Duration::from_secs(5),
    )
    .await;

    assert!(matches!(result, Err(BrowserError::Spawn { .. })));
}

#[tokio::test]
#[cfg(unix)]
async fn a_browser_that_exits_early_is_reported() {
    let profile = tempfile::tempdir().unwrap();
    let fake = profile.path().join("fake-chrome");
    std::fs::write(&fake, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let result = Browser::launch(
        &fake,
        &LaunchOptions::new(profile.path().join("p")),
        Duration::from_secs(10),
    )
    .await;

    assert!(matches!(result, Err(BrowserError::Exited(status)) if status.code() == Some(3)));
}

#[tokio::test]
#[cfg(unix)]
async fn a_stale_active_port_file_is_not_trusted() {
    let profile = tempfile::tempdir().unwrap();
    let dir = profile.path().join("p");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("DevToolsActivePort"), "40123\n/devtools/browser/stale\n").unwrap();
    let fake = profile.path().join("silent-chrome");
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let result = Browser::launch(&fake, &LaunchOptions::new(dir), Duration::from_millis(500)).await;

    assert!(matches!(result, Err(BrowserError::Timeout(_))));
}

/// A stand-in browser: starts a helper process (as Chrome starts its crash handler), writes `DevToolsActivePort`,
/// then waits.
#[cfg(unix)]
const FAKE_BROWSER: &str = r#"#!/bin/sh
for arg; do case "$arg" in --user-data-dir=*) dir="${arg#--user-data-dir=}";; esac; done
sleep 300 &
echo $! > "$dir/helper.pid"
printf '40123\n/devtools/browser/fake\n' > "$dir/DevToolsActivePort"
exec sleep 300
"#;

#[cfg(unix)]
async fn launch_fake(root: &Path) -> TestResult<(Browser, u32)> {
    let fake = root.join("fake-chrome");
    std::fs::write(&fake, FAKE_BROWSER)?;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    let dir = root.join("p");
    let browser = Browser::launch(&fake, &LaunchOptions::new(dir.clone()), Duration::from_secs(10)).await?;
    let helper = std::fs::read_to_string(dir.join("helper.pid"))?.trim().parse()?;
    Ok((browser, helper))
}

#[tokio::test]
#[cfg(unix)]
async fn the_endpoint_is_read_from_the_profile_and_dropping_the_browser_kills_its_whole_group() {
    let root = tempfile::tempdir().unwrap();
    let (browser, helper) = launch_fake(root.path()).await.unwrap();

    assert_eq!(
        browser.endpoint().ws_url(),
        "ws://127.0.0.1:40123/devtools/browser/fake"
    );
    assert!(process::running(helper));
    drop(browser);

    assert!(process::gone(helper).await, "the helper outlived the browser");
}

#[tokio::test]
#[cfg(unix)]
async fn a_browser_that_does_not_exit_gets_sigterm_and_its_group_goes_with_it() {
    use std::os::unix::process::ExitStatusExt as _;

    let root = tempfile::tempdir().unwrap();
    let (browser, helper) = launch_fake(root.path()).await.unwrap();

    let status = browser.wait_or_kill(Duration::from_millis(100)).await.unwrap();

    assert_eq!(status.signal(), Some(15));
    assert!(process::gone(helper).await, "the helper outlived the browser");
}

// --- checksum and install ---

#[cfg(unix)]
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[cfg(unix)]
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> TestResult<PathBuf> {
    let path = dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

#[cfg(unix)]
fn sha256_of(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes).iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

#[test]
#[cfg(unix)]
fn a_matching_checksum_passes_in_any_case() {
    let dir = tempfile::tempdir().unwrap();
    let empty = write_file(dir.path(), "empty", b"").unwrap();
    let abc = write_file(dir.path(), "abc", b"abc").unwrap();

    chrome::verify_sha256(&empty, EMPTY_SHA256).unwrap();
    chrome::verify_sha256(&abc, "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD").unwrap();
}

#[test]
#[cfg(unix)]
fn a_mismatched_checksum_names_both_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let abc = write_file(dir.path(), "abc", b"abc").unwrap();

    let error = chrome::verify_sha256(&abc, EMPTY_SHA256).unwrap_err();

    let ChecksumError::Mismatch { expected, actual } = &error else {
        panic!("{error}");
    };
    assert_eq!(expected, EMPTY_SHA256);
    assert_eq!(
        actual,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(error.to_string().contains(EMPTY_SHA256));
}

#[test]
#[cfg(unix)]
fn a_malformed_expected_hash_or_missing_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let abc = write_file(dir.path(), "abc", b"abc").unwrap();

    for bad in ["", "abc", &"g".repeat(64), &"a".repeat(63)] {
        assert!(matches!(
            chrome::verify_sha256(&abc, bad),
            Err(ChecksumError::BadExpected(_))
        ));
    }
    assert!(matches!(
        chrome::verify_sha256(&dir.path().join("missing"), EMPTY_SHA256),
        Err(ChecksumError::Read { .. })
    ));
}

#[test]
fn the_pinned_builds_are_chrome_for_testing_zips_no_older_than_the_profiles() {
    let chrome_dir = Path::new("/lab/data/chrome");
    for (build, platform, binary) in [
        (ChromeBuild::linux64(), "linux64", "chrome-linux64/chrome"),
        (
            ChromeBuild::mac_arm64(),
            "mac-arm64",
            "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ),
    ] {
        assert!(
            build
                .url
                .starts_with("https://storage.googleapis.com/chrome-for-testing-public/")
        );
        assert!(
            build
                .url
                .ends_with(&format!("/{}/{platform}/chrome-{platform}.zip", build.version))
        );
        assert_eq!(build.sha256.len(), 64);
        let major: u32 = build.version.split('.').next().unwrap().parse().unwrap();
        // the lab profiles were created by Chrome for Testing 151 (Playwright 1.62.1)
        assert!(major >= 151);
        assert_eq!(
            build.executable(chrome_dir),
            chrome_dir.join(&build.version).join(binary)
        );
    }
    assert_ne!(ChromeBuild::linux64().sha256, ChromeBuild::mac_arm64().sha256);
    let here = if cfg!(target_os = "macos") {
        ChromeBuild::mac_arm64()
    } else {
        ChromeBuild::linux64()
    };
    assert_eq!(ChromeBuild::pinned(), here);
}

/// A zip shaped like Chrome for Testing's: `chrome-linux64/chrome` (executable) and one data file.
#[cfg(unix)]
fn fake_chrome_zip(with_binary: bool) -> TestResult<Vec<u8>> {
    let mut bytes = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut bytes);
    let executable = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
    let data = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    if with_binary {
        zip.start_file("chrome-linux64/chrome", executable)?;
        zip.write_all(b"#!/bin/sh\necho fake chrome\n")?;
    }
    zip.start_file("chrome-linux64/resources.pak", data)?;
    zip.write_all(b"data")?;
    zip.finish()?;
    Ok(bytes.into_inner())
}

#[cfg(unix)]
async fn serve_zip(body: Vec<u8>, expected_downloads: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cft/chrome-linux64.zip"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .expect(expected_downloads)
        .mount(&server)
        .await;
    server
}

#[cfg(unix)]
fn build_at(server: &MockServer, sha256: String) -> ChromeBuild {
    ChromeBuild {
        version: "999.0.0.1".to_owned(),
        url: format!("{}/cft/chrome-linux64.zip", server.uri()),
        sha256,
        binary: "chrome-linux64/chrome".to_owned(),
    }
}

#[cfg(unix)]
fn entries(dir: &Path) -> TestResult<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[tokio::test]
#[cfg(unix)]
async fn install_downloads_verifies_and_unpacks_once() {
    let zip = fake_chrome_zip(true).unwrap();
    let server = serve_zip(zip.clone(), 1).await;
    let build = build_at(&server, sha256_of(&zip));
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");
    let client = reqwest::Client::new();

    let first = chrome::install(&build, &chrome_dir, &client).await.unwrap();
    let second = chrome::install(&build, &chrome_dir, &client).await.unwrap();

    let executable = build.executable(&chrome_dir);
    assert_eq!(first, Installed::Fresh(executable.clone()));
    assert_eq!(second, Installed::Present(executable.clone()));
    let mode = std::fs::metadata(&executable).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the browser binary is executable");
    // nothing but the finished install is left behind
    assert_eq!(entries(&chrome_dir).unwrap(), vec!["999.0.0.1".to_owned()]);
}

#[tokio::test]
#[cfg(unix)]
async fn a_download_with_the_wrong_checksum_installs_nothing() {
    let server = serve_zip(fake_chrome_zip(true).unwrap(), 1).await;
    let build = build_at(&server, EMPTY_SHA256.to_owned());
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");

    let error = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap_err();

    assert!(
        matches!(error, InstallError::Checksum(ChecksumError::Mismatch { .. })),
        "{error}"
    );
    assert!(
        entries(&chrome_dir).unwrap().is_empty(),
        "left behind: {:?}",
        entries(&chrome_dir).unwrap()
    );
}

#[tokio::test]
#[cfg(unix)]
async fn a_zip_without_the_browser_binary_installs_nothing() {
    let zip = fake_chrome_zip(false).unwrap();
    let server = serve_zip(zip.clone(), 1).await;
    let build = build_at(&server, sha256_of(&zip));
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");

    let error = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap_err();

    assert!(matches!(error, InstallError::MissingExecutable(_)), "{error}");
    assert!(
        entries(&chrome_dir).unwrap().is_empty(),
        "left behind: {:?}",
        entries(&chrome_dir).unwrap()
    );
}

#[tokio::test]
#[cfg(unix)]
async fn an_http_error_installs_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let build = build_at(&server, EMPTY_SHA256.to_owned());
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");

    let error = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap_err();

    assert!(matches!(error, InstallError::Download(_)), "{error}");
    assert!(entries(&chrome_dir).unwrap().is_empty());
}

#[tokio::test]
#[cfg(unix)]
async fn an_unfinished_install_is_replaced() {
    let zip = fake_chrome_zip(true).unwrap();
    let server = serve_zip(zip.clone(), 1).await;
    let build = build_at(&server, sha256_of(&zip));
    let lab = tempfile::tempdir().unwrap();
    let chrome_dir = lab.path().join("chrome");
    // an earlier run died after unpacking, before the marker
    std::fs::create_dir_all(build.install_dir(&chrome_dir).join("chrome-linux64")).unwrap();
    std::fs::write(build.executable(&chrome_dir), "truncated").unwrap();

    let installed = chrome::install(&build, &chrome_dir, &reqwest::Client::new())
        .await
        .unwrap();

    assert!(matches!(installed, Installed::Fresh(_)));
    let binary = std::fs::read_to_string(build.executable(&chrome_dir)).unwrap();
    assert!(binary.contains("fake chrome"));
}

// --- CDP codec ---

#[test]
fn a_command_carries_its_session_and_empty_params_become_an_object() {
    let session = SessionId::new("S1");
    let browser: Value = serde_json::from_str(&encode_command(1, None, "Target.getTargets", Value::Null)).unwrap();
    let target: Value = serde_json::from_str(&encode_command(
        2,
        Some(&session),
        "Fetch.enable",
        json!({"patterns": []}),
    ))
    .unwrap();

    assert_eq!(browser, json!({"id": 1, "method": "Target.getTargets", "params": {}}));
    assert_eq!(
        target,
        json!({"id": 2, "method": "Fetch.enable", "params": {"patterns": []}, "sessionId": "S1"})
    );
}

#[test]
fn responses_errors_and_session_events_are_decoded() {
    let ok = parse_incoming(r#"{"id":3,"result":{"targetId":"T"}}"#).unwrap();
    let failed = parse_incoming(r#"{"id":4,"error":{"code":-32601,"message":"'X.y' wasn't found"}}"#).unwrap();
    let event =
        parse_incoming(r#"{"method":"Target.attachedToTarget","params":{"waitingForDebugger":true},"sessionId":"S2"}"#)
            .unwrap();
    let browser_event = parse_incoming(r#"{"method":"Target.targetCreated","params":{}}"#).unwrap();

    assert_eq!(
        ok,
        Incoming::Response {
            id: 3,
            outcome: Ok(json!({"targetId": "T"}))
        }
    );
    assert_eq!(
        failed,
        Incoming::Response {
            id: 4,
            outcome: Err(RemoteError {
                code: -32601,
                message: "'X.y' wasn't found".to_owned()
            })
        }
    );
    assert_eq!(
        event,
        Incoming::Event(Event {
            session_id: Some(SessionId::new("S2")),
            method: "Target.attachedToTarget".to_owned(),
            params: json!({"waitingForDebugger": true}),
        })
    );
    assert!(matches!(browser_event, Incoming::Event(Event { session_id: None, .. })));
}

#[test]
fn a_message_that_is_neither_response_nor_event_is_malformed() {
    for text in ["", "[]", "{}", r#"{"params":{}}"#, r#"{"id":"x"}"#] {
        assert!(
            matches!(parse_incoming(text), Err(CdpError::Malformed(_))),
            "{text:?} accepted"
        );
    }
}

#[tokio::test]
async fn connecting_to_nothing_fails() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let result = Cdp::connect(&format!("ws://127.0.0.1:{port}/devtools/browser/x")).await;

    assert!(matches!(result, Err(CdpError::Connect(_))));
}

/// A one-client websocket server that reads until the client goes away; true when the client sent a close frame.
async fn close_watching_server() -> TestResult<(String, tokio::task::JoinHandle<SendResult<bool>>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("ws://{}/devtools/browser/x", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        let mut closed = false;
        // reading on after the close frame sends the server's answer, which ends the client's reader
        while let Some(message) = socket.next().await {
            closed |= message?.is_close();
        }
        Ok(closed)
    });
    Ok((url, server))
}

#[tokio::test]
async fn dropping_every_handle_closes_the_connection() {
    let (url, server) = close_watching_server().await.unwrap();
    let (cdp, events) = Cdp::connect(&url).await.unwrap();
    let session = cdp.session(SessionId::new("S1"));

    drop((cdp, session, events));

    let closed = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the connection stayed open after every handle was dropped");
    assert!(closed.unwrap().unwrap(), "no close frame");
}

#[tokio::test]
async fn events_end_once_the_last_command_handle_is_dropped() {
    let (url, server) = close_watching_server().await.unwrap();
    let (cdp, mut events) = Cdp::connect(&url).await.unwrap();

    drop(cdp);

    let end = tokio::time::timeout(Duration::from_secs(5), events.recv()).await;
    assert!(matches!(end, Ok(None)), "the events went on");
    assert!(server.await.unwrap().unwrap(), "no close frame");
}

// --- a real browser: a new worker target is held until we resume it ---

const PAGE: &str = r"<!doctype html>
<title>worker</title>
<script>
  window.workerSaid = [];
  const worker = new Worker('/worker.js');
  worker.onmessage = (event) => { window.workerSaid.push(event.data); };
</script>";

// The first line is something Fetch can't hold: if the worker ran before we resumed it, the page would hear 'started'.
const WORKER: &str = r"postMessage('started');
fetch('/from-worker')
  .then((response) => response.text())
  .then((text) => postMessage('ran:' + text), () => postMessage('fetch failed'));";

const HOLD: Duration = Duration::from_millis(700);

const STEP: Duration = Duration::from_secs(15);

fn is_worker_request(event: &Event) -> bool {
    event.method == "Fetch.requestPaused"
        && event.params["request"]["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/from-worker"))
}

/// Takes every event for `window`, letting paused requests through, and fails if the worker's request turns up: a held
/// worker sends nothing.
async fn hold_for(cdp: &Cdp, events: &mut Events, window: Duration) -> TestResult {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let Ok(next) = tokio::time::timeout_at(deadline, events.recv()).await else {
            return Ok(());
        };
        let event = next.ok_or("the connection closed while the worker was held")?;
        if is_worker_request(&event) {
            return Err("the worker's request went out before runIfWaitingForDebugger".into());
        }
        if event.method == "Fetch.requestPaused" {
            let request = json!({"requestId": event.params["requestId"]});
            cdp.call(event.session_id.as_ref(), "Fetch.continueRequest", request)
                .await?;
        }
    }
}

/// Waits for an event, letting through every paused request it passes over (Fetch is on for the page).
async fn next_event(cdp: &Cdp, events: &mut Events, what: &str, wanted: impl Fn(&Event) -> bool) -> TestResult<Event> {
    let wait = async {
        loop {
            let Some(event) = events.recv().await else {
                return Err(format!("the connection closed while waiting for {what}").into());
            };
            if wanted(&event) {
                return Ok(event);
            }
            if event.method == "Fetch.requestPaused" {
                let request = json!({"requestId": event.params["requestId"]});
                cdp.call(event.session_id.as_ref(), "Fetch.continueRequest", request)
                    .await?;
            }
        }
    };
    tokio::time::timeout(STEP, wait)
        .await
        .map_err(|_| format!("no {what} within {STEP:?}"))?
}

async fn page_value(cdp: &Cdp, page: &SessionId, expression: &str) -> TestResult<Value> {
    let params = json!({"expression": expression, "returnByValue": true});
    let result = cdp.call(Some(page), "Runtime.evaluate", params).await?;
    Ok(result["result"]["value"].clone())
}

fn session_of(value: &Value) -> TestResult<SessionId> {
    let id = value["sessionId"].as_str().ok_or("no sessionId")?;
    Ok(SessionId::new(id))
}

async fn served(server: &MockServer, wanted: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == wanted)
        .count()
}

async fn serve_worker_page() -> MockServer {
    let server = MockServer::start().await;
    for (route, body, content_type) in [
        ("/worker.html", PAGE, "text/html"),
        ("/worker.js", WORKER, "text/javascript"),
        ("/from-worker", "hello", "text/plain"),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
            .mount(&server)
            .await;
    }
    server
}

/// Opens a blank page target in flat mode, with auto-attach holding every new child target.
async fn open_held_page(cdp: &Cdp) -> TestResult<SessionId> {
    let target = cdp
        .call(None, "Target.createTarget", json!({"url": "about:blank"}))
        .await?;
    let attach = json!({"targetId": target["targetId"], "flatten": true});
    let page = session_of(&cdp.call(None, "Target.attachToTarget", attach).await?)?;
    let hold = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
    cdp.call(Some(&page), "Target.setAutoAttach", hold).await?;
    Ok(page)
}

/// Arms the page (Fetch on every request), loads it, and returns the session of the worker it starts, still held.
async fn load_with_held_worker(
    cdp: &Cdp,
    events: &mut Events,
    page: &SessionId,
    server: &MockServer,
) -> TestResult<SessionId> {
    // armed as the fence will be: Fetch on the page before anything loads
    let intercept_all = json!({"patterns": [{"urlPattern": "*"}]});
    cdp.call(Some(page), "Fetch.enable", intercept_all).await?;
    // Page.navigate answers once the navigation commits, and the page request waits on Fetch: drive events meanwhile
    let navigate = tokio::spawn({
        let (cdp, page, url) = (cdp.clone(), page.clone(), format!("{}/worker.html", server.uri()));
        async move { cdp.call(Some(&page), "Page.navigate", json!({ "url": url })).await }
    });
    let attached = next_event(cdp, events, "the worker's attach", |event| {
        event.method == "Target.attachedToTarget"
            && event.session_id.as_ref() == Some(page)
            && event.params["targetInfo"]["type"] == "worker"
    })
    .await?;
    navigate.await??;
    assert_eq!(attached.params["waitingForDebugger"], json!(true));
    session_of(&attached.params)
}

/// Resumes the held worker; its request pauses on the page's session and goes on only when we continue it.
async fn release_worker(
    cdp: &Cdp,
    events: &mut Events,
    page: &SessionId,
    worker: &SessionId,
    server: &MockServer,
) -> TestResult {
    // a dedicated worker has no Fetch domain of its own: the page's Fetch covers it
    let no_fetch = cdp
        .call(Some(worker), "Fetch.enable", json!({"patterns": [{"urlPattern": "*"}]}))
        .await;
    assert!(matches!(no_fetch, Err(CdpError::Remote { .. })), "{no_fetch:?}");
    cdp.call(Some(worker), "Runtime.runIfWaitingForDebugger", Value::Null)
        .await?;
    let paused = next_event(cdp, events, "the worker's paused request", is_worker_request).await?;
    assert_eq!(paused.session_id.as_ref(), Some(page), "paused on the page's session");
    // the worker runs now, and its request is held by Fetch alone
    assert_eq!(worker_messages(cdp, page, 1).await?, json!(["started"]));
    assert_eq!(served(server, "/from-worker").await, 0, "held by Fetch");
    let request = json!({"requestId": paused.params["requestId"]});
    cdp.call(Some(page), "Fetch.continueRequest", request).await?;
    Ok(())
}

/// What the worker posted to the page, once it has posted at least `count` messages.
async fn worker_messages(cdp: &Cdp, page: &SessionId, count: usize) -> TestResult<Value> {
    let poll = async {
        loop {
            let said = page_value(cdp, page, "window.workerSaid").await?;
            if said.as_array().is_some_and(|messages| messages.len() >= count) {
                return Ok(said);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(STEP, poll)
        .await
        .map_err(|_| format!("the worker posted under {count} messages within {STEP:?}"))?
}

async fn held_worker(browser: &Browser, server: &MockServer) -> TestResult {
    let (cdp, mut events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    let page = open_held_page(&cdp).await?;
    let worker = load_with_held_worker(&cdp, &mut events, &page, server).await?;

    // held: the page is up, the worker's code hasn't run, not even its first line
    hold_for(&cdp, &mut events, HOLD).await?;
    assert_eq!(page_value(&cdp, &page, "window.workerSaid").await?, json!([]));
    assert_eq!(served(server, "/from-worker").await, 0);

    release_worker(&cdp, &mut events, &page, &worker, server).await?;
    assert_eq!(worker_messages(&cdp, &page, 2).await?, json!(["started", "ran:hello"]));
    assert_eq!(served(server, "/from-worker").await, 1);

    // closing is best effort: the connection may drop before the reply
    let _ = cdp.call(None, "Browser.close", Value::Null).await;
    Ok(())
}

#[tokio::test]
async fn a_new_worker_target_is_held_until_run_if_waiting_for_debugger() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let profile = tempfile::tempdir().unwrap();
    let server = serve_worker_page().await;
    let browser = Browser::launch(&chrome, &LaunchOptions::new(profile.path().join("p")), STEP)
        .await
        .unwrap();

    let outcome = tokio::time::timeout(Duration::from_secs(90), held_worker(&browser, &server)).await;
    browser.wait_or_kill(Duration::from_secs(10)).await.unwrap();

    outcome.expect("the held-worker run took over 90 s").unwrap();
}

// --- a real browser: what lets a target held by `waitForDebuggerOnStart` go ---
//
// Whether a target the fence gave up on before its resume, and couldn't close, can still run (`proto_lab::arming`).
// The held target is a cross-site iframe (an out-of-process frame: a target of its own) or a dedicated worker, whose
// script fetches `/ran/<case>` and sends `ran:<case>` over a WebSocket as soon as it runs; the servers count what
// arrives. Nothing here goes through the fence: the page's requests go straight to the servers.

/// How long a held target is watched for sending nothing.
const QUIET: Duration = Duration::from_secs(2);

/// What the test WebSocket server got, as text.
type Frames = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// A WebSocket server on 127.0.0.1 that keeps every text frame it gets.
async fn frame_server() -> TestResult<(u16, Frames)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let frames = Frames::default();
    let seen = std::sync::Arc::clone(&frames);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(keep_frames(stream, std::sync::Arc::clone(&seen)));
        }
    });
    Ok((port, frames))
}

async fn keep_frames(stream: tokio::net::TcpStream, seen: Frames) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    while let Some(Ok(message)) = socket.next().await {
        if let Ok(text) = message.to_text() {
            let mut frames = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            frames.push(text.to_owned());
        }
    }
}

async fn serve(server: &MockServer, route: &str, body: String, content_type: &str) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
        .mount(server)
        .await;
}

/// Serves `case`: `/outer/<case>.html` (the page with the cross-site iframe, `localhost` against the page's
/// `127.0.0.1`), `/inner/<case>.html` (the iframe), `/worker/<case>.html` (a page starting the worker),
/// `/worker/<case>.js` (the worker) and `/ran/<case>`.
async fn serve_case(server: &MockServer, ws_port: u16, case: &str) {
    let port = server.address().port();
    let run = format!(
        "fetch('/ran/{case}'); const socket = new WebSocket('ws://127.0.0.1:{ws_port}/{case}'); \
         socket.onopen = () => socket.send('ran:{case}');"
    );
    let outer = format!("<!doctype html><iframe src=\"http://localhost:{port}/inner/{case}.html\"></iframe>");
    serve(server, &format!("/outer/{case}.html"), outer, "text/html").await;
    let inner = format!("<!doctype html><script>{run}</script>");
    serve(server, &format!("/inner/{case}.html"), inner, "text/html").await;
    let starter = format!("<!doctype html><script>new Worker('/worker/{case}.js')</script>");
    serve(server, &format!("/worker/{case}.html"), starter, "text/html").await;
    serve(server, &format!("/worker/{case}.js"), run, "text/javascript").await;
    serve(server, &format!("/ran/{case}"), "ok".to_owned(), "text/plain").await;
}

/// A target held by its page's auto-attach.
struct Held {
    page: SessionId,
    page_target: String,
    session: SessionId,
    target: String,
}

/// Opens a page whose auto-attach holds its children, loads `url` in it and waits for the held child of type `kind`.
async fn load_held(cdp: &Cdp, events: &mut Events, url: String, kind: &str) -> TestResult<Held> {
    let created = cdp
        .call(None, "Target.createTarget", json!({"url": "about:blank"}))
        .await?;
    let page_target = created["targetId"].as_str().ok_or("no targetId")?.to_owned();
    let attach = json!({"targetId": page_target, "flatten": true});
    let page = session_of(&cdp.call(None, "Target.attachToTarget", attach).await?)?;
    let hold = json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true});
    cdp.call(Some(&page), "Target.setAutoAttach", hold).await?;
    let navigate = tokio::spawn({
        let (cdp, page) = (cdp.clone(), page.clone());
        async move { cdp.call(Some(&page), "Page.navigate", json!({ "url": url })).await }
    });
    let attached = next_event(cdp, events, "the held child's attach", |event| {
        event.method == "Target.attachedToTarget"
            && event.session_id.as_ref() == Some(&page)
            && event.params["targetInfo"]["type"] == kind
    })
    .await?;
    navigate.await??;
    assert_eq!(attached.params["waitingForDebugger"], json!(true));
    let target = attached.params["targetInfo"]["targetId"]
        .as_str()
        .ok_or("no targetId")?;
    Ok(Held {
        page,
        page_target,
        session: session_of(&attached.params)?,
        target: target.to_owned(),
    })
}

/// How many `/ran/<case>` requests and `ran:<case>` frames arrived.
async fn arrivals(server: &MockServer, frames: &Frames, case: &str) -> (usize, usize) {
    let fetched = served(server, &format!("/ran/{case}")).await;
    let marker = format!("ran:{case}");
    let frames = frames.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    (fetched, frames.iter().filter(|frame| **frame == marker).count())
}

/// Fails if anything of `case` arrives within [`QUIET`].
async fn stays_quiet(server: &MockServer, frames: &Frames, case: &str) -> TestResult {
    tokio::time::sleep(QUIET).await;
    match arrivals(server, frames, case).await {
        (0, 0) => Ok(()),
        seen => Err(format!("the held {case} target ran: (requests, frames) = {seen:?}").into()),
    }
}

/// Waits until both the request and the frame of `case` have arrived.
async fn runs(server: &MockServer, frames: &Frames, case: &str) -> TestResult {
    let poll = async {
        while arrivals(server, frames, case).await != (1, 1) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(STEP, poll)
        .await
        .map_err(|_| format!("the {case} target didn't run within {STEP:?}").into())
}

/// The target's `TargetInfo` as `Target.getTargets` lists it, if it does.
async fn listed(cdp: &Cdp, target: &str) -> TestResult<Option<Value>> {
    let targets = cdp.call(None, "Target.getTargets", Value::Null).await?;
    let infos = targets["targetInfos"].as_array().ok_or("no targetInfos")?;
    Ok(infos.iter().find(|info| info["targetId"] == target).cloned())
}

/// One connection to the browser and the servers the held targets would reach.
struct Bench<'a> {
    cdp: Cdp,
    events: Events,
    server: &'a MockServer,
    frames: &'a Frames,
}

impl Bench<'_> {
    /// Loads `case` with its child of type `kind` (`iframe` or `worker`) held.
    async fn held(&mut self, case: &str, kind: &str) -> TestResult<Held> {
        let page = if kind == "worker" { "worker" } else { "outer" };
        let url = format!("{}/{page}/{case}.html", self.server.uri());
        load_held(&self.cdp, &mut self.events, url, kind).await
    }

    async fn stays_quiet(&self, case: &str) -> TestResult {
        stays_quiet(self.server, self.frames, case).await
    }
}

/// Left alone, a held target sends nothing; a held out-of-process frame isn't even listed by `Target.getTargets`, so
/// that can't tell held from gone.
async fn left_alone(bench: &mut Bench<'_>) -> TestResult {
    let held = bench.held("held", "iframe").await?;
    bench.stays_quiet("held").await?;
    assert_eq!(listed(&bench.cdp, &held.target).await?, None);
    Ok(())
}

/// Chrome refuses to close a dedicated worker, and the worker stays held.
async fn a_refused_close(bench: &mut Bench<'_>) -> TestResult {
    let worker = bench.held("worker", "worker").await?;
    let close = json!({"targetId": worker.target});
    let refused = bench.cdp.call(None, "Target.closeTarget", close).await;
    let unsupported = |error: &RemoteError| error.message.contains("doesn't support closing");
    assert!(
        matches!(&refused, Err(CdpError::Remote { error, .. }) if unsupported(error)),
        "{refused:?}"
    );
    bench.stays_quiet("worker").await?;
    let info = listed(&bench.cdp, &worker.target).await?.ok_or("the worker is gone")?;
    assert_eq!(info["attached"], json!(true), "still attached, and still held");
    Ok(())
}

/// Closing a held iframe closes its whole tab.
async fn an_iframe_closed(bench: &mut Bench<'_>) -> TestResult {
    let closed = bench.held("close", "iframe").await?;
    let close = json!({"targetId": closed.target});
    let answer = bench.cdp.call(None, "Target.closeTarget", close).await?;
    assert_eq!(answer["success"], json!(true));
    bench.stays_quiet("close").await?;
    assert_eq!(
        listed(&bench.cdp, &closed.page_target).await?,
        None,
        "the tab went with it"
    );
    Ok(())
}

/// Its session detached through its parent's: no re-attach, and it stays held.
async fn its_session_detached(bench: &mut Bench<'_>) -> TestResult {
    let detached = bench.held("detach", "iframe").await?;
    let detach = json!({"sessionId": detached.session.as_str()});
    bench
        .cdp
        .call(Some(&detached.page), "Target.detachFromTarget", detach)
        .await?;
    bench.stays_quiet("detach").await
}

/// Its parent navigates away.
async fn its_parent_navigating(bench: &mut Bench<'_>) -> TestResult {
    let left = bench.held("navigate", "iframe").await?;
    let blank = json!({"url": "about:blank"});
    bench.cdp.call(Some(&left.page), "Page.navigate", blank).await?;
    bench.stays_quiet("navigate").await
}

/// What the fence can do to a target it gave up on, or what can happen around it, without letting it go.
async fn nothing_lets_it_go(browser: &Browser, server: &MockServer, frames: &Frames) -> TestResult {
    let (cdp, events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    let mut bench = Bench {
        cdp,
        events,
        server,
        frames,
    };
    left_alone(&mut bench).await?;
    a_refused_close(&mut bench).await?;
    an_iframe_closed(&mut bench).await?;
    its_session_detached(&mut bench).await?;
    its_parent_navigating(&mut bench).await?;
    // and the first one is still held after all that
    bench.stays_quiet("held").await
}

/// The two things that do let a held target go: its parent's auto-attach turned off, and its connection dropping.
async fn what_lets_it_go(browser: &Browser, server: &MockServer, frames: &Frames) -> TestResult {
    let (cdp, mut events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    let base = server.uri();
    let parent = load_held(&cdp, &mut events, format!("{base}/outer/autooff.html"), "iframe").await?;
    stays_quiet(server, frames, "autooff").await?;
    let off = json!({"autoAttach": false, "waitForDebuggerOnStart": false});
    cdp.call(Some(&parent.page), "Target.setAutoAttach", off).await?;
    runs(server, frames, "autooff").await?;

    let (other, mut other_events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    load_held(&other, &mut other_events, format!("{base}/outer/drop.html"), "iframe").await?;
    stays_quiet(server, frames, "drop").await?;
    drop((other, other_events));
    runs(server, frames, "drop").await?;

    // closing is best effort: the connection may drop before the reply
    let _ = cdp.call(None, "Browser.close", Value::Null).await;
    Ok(())
}

#[tokio::test]
async fn a_held_target_runs_only_once_its_parent_s_auto_attach_ends_or_its_connection_drops() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let profile = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (ws_port, frames) = frame_server().await.unwrap();
    for case in ["held", "worker", "close", "detach", "navigate", "autooff", "drop"] {
        serve_case(&server, ws_port, case).await;
    }
    let browser = Browser::launch(&chrome, &LaunchOptions::new(profile.path().join("p")), STEP)
        .await
        .unwrap();

    let outcome = tokio::time::timeout(Duration::from_secs(150), async {
        nothing_lets_it_go(&browser, &server, &frames).await?;
        what_lets_it_go(&browser, &server, &frames).await
    })
    .await;
    browser.wait_or_kill(Duration::from_secs(10)).await.unwrap();

    outcome.expect("the held-target run took over 150 s").unwrap();
}

// --- a real browser: a command on a target that goes away ---

/// A command still with a held out-of-process iframe when the iframe is removed: Chrome reports the session detached
/// and never answers the command, so the client fails it with the detach. (The fence's arming waits on such a command:
/// a resume sent just as the frame went away used to wait out its whole step timeout, `proto_lab::arming`.)
async fn a_command_with_a_removed_frame(browser: &Browser, server: &MockServer) -> TestResult {
    let (cdp, mut events) = Cdp::connect(&browser.endpoint().ws_url()).await?;
    let url = format!("{}/outer/vanish.html", server.uri());
    let held = load_held(&cdp, &mut events, url, "iframe").await?;
    cdp.call(Some(&held.session), "Runtime.runIfWaitingForDebugger", Value::Null)
        .await?;
    // a promise that never settles keeps the command with the frame's renderer
    let waiting = tokio::spawn({
        let (cdp, session) = (cdp.clone(), held.session.clone());
        let never = json!({"expression": "new Promise(() => {})", "awaitPromise": true});
        async move { cdp.call(Some(&session), "Runtime.evaluate", never).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "answered while the frame was there");
    page_value(&cdp, &held.page, "document.querySelector('iframe').remove()").await?;
    next_event(&cdp, &mut events, "the frame's detach", |event| {
        event.method == "Target.detachedFromTarget" && event.params["sessionId"] == held.session.as_str()
    })
    .await?;
    let answer = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .map_err(|_| "the command still waits after its session detached")??;
    assert!(
        matches!(&answer, Err(CdpError::Detached { method }) if method == "Runtime.evaluate"),
        "{answer:?}"
    );
    // closing is best effort: the connection may drop before the reply
    let _ = cdp.call(None, "Browser.close", Value::Null).await;
    Ok(())
}

#[tokio::test]
async fn a_command_waiting_on_a_frame_that_goes_away_fails_with_its_detach() {
    let Some(chrome) = common::chrome_or_skip() else {
        return;
    };
    let profile = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (ws_port, _frames) = frame_server().await.unwrap();
    serve_case(&server, ws_port, "vanish").await;
    let browser = Browser::launch(&chrome, &LaunchOptions::new(profile.path().join("p")), STEP)
        .await
        .unwrap();

    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        a_command_with_a_removed_frame(&browser, &server),
    )
    .await;
    browser.wait_or_kill(Duration::from_secs(10)).await.unwrap();

    outcome.expect("the run took over 60 s").unwrap();
}
