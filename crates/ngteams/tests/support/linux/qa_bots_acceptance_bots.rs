//! The sample bots of `packaging/server/bots/` for the bots' acceptance test: each sample's Teams app package (its
//! manifest with made-up ids, its icons) and its image run as a container on the host's network, as ngteams-server's
//! container tests run them (`podman`, or `NGTEAMS_SERVER_BOT_ENGINE`; the images `build.sh` builds). The bot's
//! secret and the server's certificate authority go through the engine's environment, never its arguments.

use std::io::{Cursor, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{fs, thread};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use ui_sandbox::SandboxResult;

const ENGINE_ENV: &str = "NGTEAMS_SERVER_BOT_ENGINE";
/// The maintained SDK's sample to run: `teams` (the Teams SDK v2, TypeScript; the default) or `agents`.
const MAINTAINED_ENV: &str = "QA_BOTS_MAINTAINED";
/// The scope a bot asks its token for: the server's `bots.scope` by default.
const SCOPE: &str = "https://api.botframework.com";
const LISTENING: Duration = Duration::from_secs(90);

/// One sample: its directory, its image, the name it signs its answers with, and the made-up ids of its app and bot.
pub struct Sample {
    pub dir: &'static str,
    image: &'static str,
    pub name: &'static str,
    pub app: &'static str,
    pub bot: &'static str,
    /// The manifest's short name: the app's name in the store, the flyout and the mention picker.
    pub short: String,
}

fn engine() -> String {
    std::env::var(ENGINE_ENV).unwrap_or_else(|_| "podman".to_owned())
}

fn samples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/server/bots")
}

/// The maintained SDK's sample and the botbuilder one, whose images are built; `Err` says what is missing.
pub fn ready() -> Result<Vec<Sample>, String> {
    let maintained = std::env::var(MAINTAINED_ENV).unwrap_or_else(|_| "teams".to_owned());
    let all = [
        (
            "teams",
            "localhost/ngteams-bot-teams:1",
            "teams-sdk",
            "00000000-0000-4000-8000-0000000ba001",
            "00000000-0000-4000-8000-0000000bb001",
        ),
        (
            "agents",
            "localhost/ngteams-bot-agents:1",
            "agents-sdk",
            "00000000-0000-4000-8000-0000000ba002",
            "00000000-0000-4000-8000-0000000bb002",
        ),
        (
            "botbuilder-python",
            "localhost/ngteams-bot-botbuilder-python:1",
            "botbuilder-python",
            "00000000-0000-4000-8000-0000000ba003",
            "00000000-0000-4000-8000-0000000bb003",
        ),
    ];
    let mut wanted = Vec::new();
    for (dir, image, name, app, bot) in all {
        if dir != maintained && dir != "botbuilder-python" {
            continue;
        }
        let exists = Command::new(engine())
            .args(["image", "exists", image])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("no container engine {}: {error}", engine()))?;
        if !exists.success() {
            return Err(format!("no image {image} (packaging/server/bots/build.sh builds it)"));
        }
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(samples().join(dir).join("manifest.json"))
                .map_err(|error| format!("the {dir} sample's manifest: {error}"))?,
        )
        .map_err(|error| format!("the {dir} sample's manifest: {error}"))?;
        let short = manifest["name"]["short"].as_str().unwrap_or_default().to_owned();
        wanted.push(Sample {
            dir,
            image,
            name,
            app,
            bot,
            short,
        });
    }
    if wanted.len() < 2 {
        return Err(format!(
            "{MAINTAINED_ENV} names no sample of a maintained SDK: {maintained}"
        ));
    }
    Ok(wanted)
}

impl Sample {
    /// Its Teams app package: the manifest with its ids, and its icons, as `package.sh` zips it.
    pub fn package(&self) -> SandboxResult<Vec<u8>> {
        let dir = samples().join(self.dir);
        let manifest = fs::read_to_string(dir.join("manifest.json"))?
            .replace("${APP_ID}", self.app)
            .replace("${BOT_ID}", self.bot);
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.json", options)?;
        zip.write_all(manifest.as_bytes())?;
        for icon in ["color.png", "outline.png"] {
            zip.start_file(icon, options)?;
            zip.write_all(&fs::read(dir.join(icon))?)?;
        }
        Ok(zip.finish()?.into_inner())
    }
}

/// The tenant the server's tokens carry, read from the token the bot's secret gets (only its `tid` is kept).
pub fn tenant(origin: &str, ca_file: &Path, bot: &str, secret: &str) -> SandboxResult<String> {
    let output = Command::new("curl")
        .args(["-sS", "--max-time", "20", "--cacert"])
        .arg(ca_file)
        .args(["--data-urlencode", "grant_type=client_credentials", "--data-urlencode"])
        .arg(format!("client_id={bot}"))
        .arg("--data-urlencode")
        .arg(format!("client_secret={secret}"))
        .arg("--data-urlencode")
        .arg(format!("scope={SCOPE}/.default"))
        .arg(format!("{origin}/botframework.com/oauth2/v2.0/token"))
        .output()?;
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
    let token = answer["access_token"]
        .as_str()
        .ok_or_else(|| format!("the bot's secret got no token: {}", answer["error"]))?;
    let payload = token.split('.').nth(1).ok_or("the bot's token is no JWT")?;
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    Ok(claims["tid"].as_str().ok_or("the bot's token has no tid")?.to_owned())
}

/// A bot's container, removed when dropped.
pub struct Container {
    name: String,
}

/// Where a bot runs: the server's origin and certificate authority, the tenant, and the bot's secret and port.
pub struct Place<'a> {
    pub origin: &'a str,
    pub ca_file: &'a Path,
    pub tenant: &'a str,
    pub secret: &'a str,
    pub port: u16,
}

impl Container {
    /// Runs `sample`'s image as its bot at `place`; once it listens.
    pub fn run(sample: &Sample, place: &Place<'_>) -> SandboxResult<Self> {
        let name = format!("qa-bots-acceptance-{}-{}", sample.name, place.port);
        let ca = fs::read_to_string(place.ca_file)?;
        let port = place.port.to_string();
        let env = [
            ("BOT_ID", sample.bot),
            ("BOT_SECRET", place.secret),
            ("TENANT", place.tenant),
            ("ORIGIN", place.origin),
            ("SCOPE", SCOPE),
            ("PORT", port.as_str()),
            ("BOT_CA_PEM", ca.as_str()),
        ];
        let mut command = Command::new(engine());
        command.args(["run", "-d", "--rm", "--name", &name, "--network", "host"]);
        for (key, value) in env {
            command.arg("-e").arg(key);
            command.env(key, value);
        }
        let output = command.arg(sample.image).stderr(Stdio::piped()).output()?;
        if !output.status.success() {
            return Err(format!(
                "{} run {}: {}",
                engine(),
                sample.image,
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        let container = Self { name };
        let start = Instant::now();
        while TcpStream::connect(("127.0.0.1", place.port)).is_err() {
            if start.elapsed() > LISTENING {
                return Err(format!("the {} bot never listened: {}", sample.name, container.logs()).into());
            }
            thread::sleep(Duration::from_millis(250));
        }
        Ok(container)
    }

    pub fn logs(&self) -> String {
        let Ok(output) = Command::new(engine()).args(["logs", &self.name]).output() else {
            return String::new();
        };
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new(engine())
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
