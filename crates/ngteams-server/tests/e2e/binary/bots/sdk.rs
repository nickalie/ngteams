//! Real bot libraries against the real binary over TLS, in podman (when `NGTS_QA_BOT_SDKS` is set; the images build
//! from `sdk/`, pinned): msal-python under `/adfs`, msal-node under the tenant and `botframework.com`, the Bot
//! Framework SDK's own credentials and its own checks of an activity's token against the server's published keys.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;

use super::super::support::{Deployment, Node, TLS_DIR};
use super::support::{SCOPE, activity_token, app_path, bot_token, call, first_admin, register, tenant};
use crate::support::Backend;

const SDKS: &str = "NGTS_QA_BOT_SDKS";
const PYTHON: &str = "localhost/ngts-qa-bot-python:1";
const NODE: &str = "localhost/ngts-qa-bot-node:1";

fn sdk_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/binary/bots/sdk")
}

fn build(image: &str, file: &str) {
    let status = Command::new("podman")
        .args(["build", "-q", "-t", image, "-f", file, "."])
        .current_dir(sdk_dir())
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "podman build {image}");
}

/// What a bot library in `image` printed for `args`, with `env` (secrets passed through podman's environment, not its
/// arguments) and `input` on its standard input.
fn run(image: &str, args: &[&str], env: &[(&str, &str)], input: &str) -> Value {
    let ca = PathBuf::from(std::env::var_os(TLS_DIR).unwrap()).join("ca.pem");
    let mut command = Command::new("podman");
    command.args(["run", "--rm", "-i", "--network", "host"]);
    command.arg("-v").arg(format!("{}:/ca.pem:ro", ca.display()));
    command.arg("-v").arg(format!("{}:/sdk:ro", sdk_dir().display()));
    for (key, value) in [
        ("REQUESTS_CA_BUNDLE", "/ca.pem"),
        ("SSL_CERT_FILE", "/ca.pem"),
        ("NODE_EXTRA_CA_CERTS", "/ca.pem"),
        ("NODE_PATH", "/bot/node_modules"),
    ]
    .iter()
    .chain(env)
    {
        command.arg("-e").arg(key);
        command.env(key, value);
    }
    command.arg(image).args(args);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().last().unwrap_or_default();
    serde_json::from_str(last).unwrap_or_else(|_| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(8).collect();
        panic!("{image} {args:?} printed no JSON: {tail:?}")
    })
}

/// Runs `test` again in a child with its own CA, as the harness's `tls_parent` does, but with a CA and a leaf of
/// different names: OpenSSL (Python's `ssl`) takes a leaf named as its issuer for a self-signed one.
pub(super) fn tls_parent(test: &str) -> bool {
    if std::env::var_os(TLS_DIR).is_some() {
        return false;
    }
    let dir = tempfile::tempdir().unwrap();
    write_ca(dir.path());
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env(TLS_DIR, dir.path())
        .env("SSL_CERT_FILE", dir.path().join("ca.pem"))
        .env("SSL_CERT_DIR", dir.path().join("empty"))
        .status()
        .unwrap();
    assert!(status.success(), "TLS child failed");
    true
}

fn write_ca(dir: &Path) {
    use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "ngteams QA bots CA (synthetic)");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(params, ca_key);
    let key = KeyPair::generate().unwrap();
    let mut leaf = CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    leaf.distinguished_name.push(DnType::CommonName, "127.0.0.1");
    let leaf = leaf.signed_by(&key, &issuer).unwrap();
    std::fs::write(dir.join("ca.pem"), ca.pem()).unwrap();
    std::fs::write(dir.join("leaf.pem"), leaf.pem()).unwrap();
    std::fs::write(dir.join("key.pem"), key.serialize_pem()).unwrap();
    std::fs::create_dir(dir.join("empty")).unwrap();
}

struct Bot<'a> {
    node: &'a Node,
    id: String,
    secret: String,
    tenant: String,
}

impl Bot<'_> {
    fn env<'b>(&'b self, authority: &'b str, secret: &'b str) -> Vec<(&'b str, &'b str)> {
        vec![
            ("BOT_ID", self.id.as_str()),
            ("BOT_SECRET", secret),
            ("AUTHORITY", authority),
            ("SCOPE", SCOPE),
            ("ORIGIN", self.node.endpoint.as_str()),
        ]
    }

    fn python(&self, action: &str, authority: &str, secret: &str) -> Value {
        let issuer = format!("{}/bot", self.node.endpoint);
        let mut env = self.env(authority, secret);
        env.push(("ISSUER", &issuer));
        run(PYTHON, &["python", "/sdk/bot.py", action], &env, "")
    }

    fn node_msal(&self, authority: &str, secret: &str) -> Value {
        run(NODE, &["node", "/sdk/msal.js"], &self.env(authority, secret), "")
    }
}

fn assert_token(got: &Value, bot: &str, what: &str) {
    assert_eq!(got["ok"], true, "{what}: {got}");
    assert_eq!(got["claims"]["aud"], SCOPE, "{what}");
    assert_eq!(got["claims"]["appid"], bot, "{what}");
    assert_eq!(got["claims"]["idtyp"], "app", "{what}");
}

impl Bot<'_> {
    fn adfs(&self) -> String {
        format!("{}/adfs", self.node.endpoint)
    }

    fn at_tenant(&self) -> String {
        format!("{}/{}", self.node.endpoint, self.tenant)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bot_libraries_get_tokens_and_check_activities_over_tls() {
    if std::env::var_os(SDKS).is_none() {
        eprintln!("skipped: {SDKS} is not set");
        return;
    }
    if tls_parent("binary::bots::sdk::bot_libraries_get_tokens_and_check_activities_over_tls") {
        return;
    }
    build(PYTHON, "python.Containerfile");
    build(NODE, "node.Containerfile");
    let deployment = Deployment::new(Backend::Sqlite, true, None).await;
    let mut node = deployment.node("a");
    node.cli(&["migrate"], "");
    node.start().await;
    let root = first_admin(&node).await;
    let (id, secret) = register(&node, &root, "SDK Echo (synthetic)").await;
    let bot = Bot {
        node: &node,
        id,
        secret,
        tenant: tenant(&root),
    };
    tokens(&bot);
    sdk_checks(&bot).await;
    disabled_rotated_deleted(&bot, &root).await;
    let (held, held_secret) = register(&node, &root, "SDK Held (synthetic)").await;
    let held = Bot {
        node: &node,
        id: held,
        secret: held_secret,
        tenant: bot.tenant.clone(),
    };
    hold(&held);
    node.stop().await;
}

fn tokens(bot: &Bot<'_>) {
    eprintln!("sdk: msal-python under /adfs, msal-node under the tenant and botframework.com");
    let (adfs, at_tenant) = (bot.adfs(), bot.at_tenant());
    let framework = format!("{}/botframework.com", bot.node.endpoint);
    assert_token(&bot.python("msal", &adfs, &bot.secret), &bot.id, "msal-python");
    assert_token(&bot.node_msal(&at_tenant, &bot.secret), &bot.id, "msal-node tenant");
    assert_token(
        &bot.node_msal(&framework, &bot.secret),
        &bot.id,
        "msal-node botframework.com",
    );
    for wrong in [
        bot.python("msal", &adfs, "synthetic-wrong"),
        bot.node_msal(&at_tenant, "synthetic-wrong"),
    ] {
        assert_eq!(
            (&wrong["ok"], &wrong["error"]),
            (&json!(false), &json!("invalid_client")),
            "{wrong}"
        );
    }
    eprintln!("sdk: the Bot Framework SDK's own credentials");
    assert_token(
        &bot.python("sdk-token", &adfs, &bot.secret),
        &bot.id,
        "botframework-connector",
    );
}

async fn sdk_checks(bot: &Bot<'_>) {
    eprintln!("sdk: the Bot Framework SDK checks an activity's token");
    let node = bot.node;
    let service_url = format!("{}/bot/", node.endpoint);
    let token = activity_token(node, &bot.id, &service_url).await;
    let own = bot_token(node, &bot.tenant, &bot.id, &bot.secret).await.answer["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = uuid::Uuid::new_v4().to_string();
    let other_token = activity_token(node, &other, &service_url).await;
    let mut forged = token.rsplit_once('.').unwrap().0.to_owned();
    forged.push_str(".c3ludGhldGljIHNpZ25hdHVyZQ");
    let elsewhere = "https://elsewhere.example.test/bot/";
    let cases = json!([
        {"name": "the activity's token", "token": token, "bot": bot.id, "service_url": service_url},
        {"name": "another bot's token", "token": other_token, "bot": bot.id, "service_url": service_url},
        {"name": "for another bot", "token": token, "bot": other, "service_url": service_url},
        {"name": "another service URL", "token": token, "bot": bot.id, "service_url": elsewhere},
        {"name": "the bot's own EdDSA token", "token": own, "bot": bot.id, "service_url": service_url},
        {"name": "a forged signature", "token": forged, "bot": bot.id, "service_url": service_url},
    ]);
    let issuer = format!("{}/bot", node.endpoint);
    let adfs = bot.adfs();
    let mut env = bot.env(&adfs, "unused");
    env.push(("ISSUER", &issuer));
    let checked = run(PYTHON, &["python", "/sdk/bot.py", "verify"], &env, &cases.to_string());
    eprintln!("  the SDK's verdicts: {checked}");
    let results = checked.as_array().unwrap();
    assert_eq!(results[0]["ok"], true, "{checked}");
    assert_eq!(results[0]["audience"], SCOPE, "{checked}");
    for refused in &results[1..] {
        assert_eq!(refused["ok"], false, "{refused}");
    }
}

async fn disabled_rotated_deleted(bot: &Bot<'_>, root: &Secret) {
    eprintln!("sdk: disabled, rotated and deleted");
    let (node, adfs, at_tenant) = (bot.node, bot.adfs(), bot.at_tenant());
    let path = app_path(&bot.id);
    call(node, Method::PATCH, &path, Some(root), Some(json!({"enabled": false}))).await;
    for off in [
        bot.python("msal", &adfs, &bot.secret),
        bot.node_msal(&at_tenant, &bot.secret),
    ] {
        assert_eq!(off["error"], "unauthorized_client", "{off}");
    }
    call(node, Method::PATCH, &path, Some(root), Some(json!({"enabled": true}))).await;
    let (_, rotated) = call(node, Method::POST, &format!("{path}/secret"), Some(root), None).await;
    let new = rotated["secret"].as_str().unwrap();
    assert_eq!(bot.python("sdk-token", &adfs, &bot.secret)["ok"], false);
    assert_token(&bot.python("sdk-token", &adfs, new), &bot.id, "after the rotation");
    call(node, Method::DELETE, &path, Some(root), None).await;
    let gone = bot.node_msal(&at_tenant, new);
    assert_eq!(gone["error"], "invalid_client", "{gone}");
}

fn hold(bot: &Bot<'_>) {
    eprintln!("sdk: msal-node meets the hold");
    let at_tenant = bot.at_tenant();
    for _ in 0..3 {
        assert_eq!(bot.node_msal(&at_tenant, "synthetic-wrong")["error"], "invalid_client");
    }
    let throttled = bot.node_msal(&at_tenant, "synthetic-wrong");
    eprintln!("  msal-node after 3 wrong secrets: {throttled}");
    assert_eq!(throttled["ok"], false, "{throttled}");
    assert_eq!(throttled["error"], "temporarily_unavailable", "held off: {throttled}");
    assert_token(
        &bot.node_msal(&at_tenant, &bot.secret),
        &bot.id,
        "the right secret during the hold",
    );
}
