//! The models each AI tool on this computer offers, as the tool reports them itself, so the settings list what it
//! takes today rather than a list of ours. Nothing here asks a model anything:
//!
//! - **Claude Code** keeps the catalog its own model picker shows in `~/.claude/cache/model-catalog/` (`$CLAUDE_CONFIG_DIR`
//!   when set): its aliases that follow the newest model of a family (`opus`, `sonnet`, …) and each model by its full
//!   name. Without that file, the aliases its `--help` names for `--model` are listed.
//! - **The Codex CLI** prints its model catalog with `codex debug models`; when that fails, the copy it keeps in
//!   `~/.codex/models_cache.json` (`$CODEX_HOME` when set) is read. Models it hides from its own picker are left out.
//!
//! The settings offer these, and also take any other name the user types ([`super::Model::Named`]). The catalog is an
//! app global, listed in the background on first use ([`Catalog::of`]) and again when the tools found change
//! ([`super::Tools::look_again`]).

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp, Global};
use serde_json::Value;

use super::{Backend, Tools};

/// How long a tool may take to list its models.
const LISTING: Duration = Duration::from_secs(15);
/// The most of a tool's listing read, in bytes.
const MAX_LISTING: u64 = 4 << 20;

/// A model a tool offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// What its command line takes (`opus`, `claude-opus-5-5`, `gpt-6.1-sol`).
    pub id: String,
    /// Its name as the tool shows it (`Opus 5.5`, `GPT-6.1-Sol`).
    pub name: String,
    /// An alias that follows the newest model of its family.
    pub latest: bool,
}

impl Listed {
    /// Its row's words in the settings: "Opus (latest)", "Opus 5.5".
    #[must_use]
    pub fn label(&self) -> String {
        if self.latest {
            format!("{} (latest)", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The models the tools offer (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    claude: Vec<Listed>,
    codex: Vec<Listed>,
    /// Listed (or set): not still being looked for.
    listed: bool,
}

impl Global for Catalog {}

impl Catalog {
    /// These models (tests, the UI tour).
    #[must_use]
    pub fn with(claude: Vec<Listed>, codex: Vec<Listed>) -> Self {
        Self {
            claude,
            codex,
            listed: true,
        }
    }

    /// The models `backend` offers, as it orders them.
    #[must_use]
    pub fn models(&self, backend: Backend) -> &[Listed] {
        match backend {
            Backend::Claude => &self.claude,
            Backend::Codex => &self.codex,
        }
    }

    /// Whether the tools were asked yet.
    #[must_use]
    pub fn is_listed(&self) -> bool {
        self.listed
    }

    /// The app's: empty while the installed tools are being asked, which starts on first use.
    pub fn of(cx: &mut App) -> Self {
        if let Some(catalog) = cx.try_global::<Self>() {
            return catalog.clone();
        }
        Self::relist(cx);
        Self::default()
    }

    /// Asks the app's tools for their models again (they changed), in the background; empty until they answer.
    pub fn relist(cx: &mut App) {
        cx.set_global(Self::default());
        let tools = Tools::of(cx);
        let asked = tools.clone();
        // on the background executor (a pool thread; the test scheduler's own in tests)
        let listing = cx.background_executor().spawn(async move { Self::list(&asked) });
        cx.spawn(async move |cx: &mut AsyncApp| {
            let catalog = listing.await;
            cx.update(|cx| {
                // a listing of tools since found again is stale
                if cx.try_global::<Tools>() == Some(&tools) {
                    cx.set_global(catalog);
                }
            });
        })
        .detach();
    }

    /// Asks each tool in `tools` for its models (blocking: a tool runs).
    #[must_use]
    pub fn list(tools: &Tools) -> Self {
        let mut catalog = Self {
            listed: true,
            ..Self::default()
        };
        for (backend, program) in &tools.found {
            match backend {
                Backend::Claude => catalog.claude = list_claude(program, &claude_home()),
                Backend::Codex => catalog.codex = list_codex(program, &codex_home()),
            }
        }
        catalog
    }
}

/// Claude Code's models: its cached catalog under `home`, else the aliases its `--help` names.
fn list_claude(program: &Path, home: &Path) -> Vec<Listed> {
    let models = newest_claude_catalog(home)
        .map(|text| claude_catalog(&text))
        .unwrap_or_default();
    if !models.is_empty() {
        return models;
    }
    run(program, &["--help"]).map_or_else(Vec::new, |help| claude_help(&help))
}

/// The Codex CLI's models: `codex debug models`, else its cache under `home`.
fn list_codex(program: &Path, home: &Path) -> Vec<Listed> {
    let models = run(program, &["debug", "models"])
        .map(|text| codex_catalog(&text))
        .unwrap_or_default();
    if !models.is_empty() {
        return models;
    }
    std::fs::read_to_string(home.join("models_cache.json"))
        .map(|text| codex_catalog(&text))
        .unwrap_or_default()
}

/// Claude Code's folder: `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
fn claude_home() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR").map_or_else(|| home().join(".claude"), PathBuf::from)
}

/// The Codex CLI's folder: `$CODEX_HOME`, else `~/.codex`.
fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME").map_or_else(|| home().join(".codex"), PathBuf::from)
}

fn home() -> PathBuf {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable).map(PathBuf::from).unwrap_or_default()
}

/// The text of the newest catalog Claude Code fetched for itself (`*-cc.json`, by its `fetchedAt`).
fn newest_claude_catalog(home: &Path) -> Option<String> {
    let dir = std::fs::read_dir(home.join("cache").join("model-catalog")).ok()?;
    dir.filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.to_string_lossy().ends_with("-cc.json"))
        .filter_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let fetched = serde_json::from_str::<Value>(&text)
                .ok()?
                .get("fetchedAt")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            Some((fetched, text))
        })
        .max_by_key(|(fetched, _)| *fetched)
        .map(|(_, text)| text)
}

/// The models in Claude Code's catalog `text`: first its aliases (each following its family's newest model), then each
/// model, those its picker shows first before the older ones.
#[must_use]
pub fn claude_catalog(text: &str) -> Vec<Listed> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let config = value.pointer("/catalog/config").unwrap_or(&Value::Null);
    let models: Vec<&Value> = config
        .get("models")
        .and_then(Value::as_array)
        .map(|models| models.iter().collect())
        .unwrap_or_default();
    let field = |model: &Value, key: &str| model.get(key).and_then(Value::as_str).map(str::to_owned);
    let aliases = config.get("provider_alias_targets").and_then(Value::as_object);
    let mut listed = Vec::new();
    // the aliases, in the order their families first come in the catalog
    let mut families: Vec<String> = models
        .iter()
        .filter_map(|model| model.pointer("/runtime/family").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    families.extend(aliases.into_iter().flat_map(|aliases| aliases.keys().cloned()));
    for family in families {
        if !aliases.is_some_and(|aliases| aliases.contains_key(&family))
            || listed.iter().any(|seen: &Listed| seen.id == family)
        {
            continue;
        }
        let name = models
            .iter()
            .find(|model| model.pointer("/runtime/family").and_then(Value::as_str) == Some(&family))
            .and_then(|model| field(model, "short_name"))
            .unwrap_or_else(|| capitalized(&family));
        listed.push(Listed {
            id: family,
            name,
            latest: true,
        });
    }
    let main = |model: &Value| model.get("section").and_then(Value::as_str) == Some("main");
    let ordered = models
        .iter()
        .filter(|model| main(model))
        .chain(models.iter().filter(|model| !main(model)));
    for model in ordered {
        let Some(id) = field(model, "id") else { continue };
        let name = field(model, "name").unwrap_or_else(|| id.clone());
        listed.push(Listed {
            id,
            name,
            latest: false,
        });
    }
    listed
}

/// The aliases Claude Code's `--help` names for `--model` (its words in quotes there: `'opus'`).
#[must_use]
pub fn claude_help(help: &str) -> Vec<Listed> {
    let Some(at) = help.find("--model ") else {
        return Vec::new();
    };
    // the option's description runs to the next option
    let rest = &help[at + "--model ".len()..];
    let end = rest.find("\n  -").unwrap_or(rest.len());
    let mut listed: Vec<Listed> = Vec::new();
    for (index, word) in rest[..end].split('\'').enumerate() {
        let word = word.trim();
        let shaped = index % 2 == 1
            && !word.is_empty()
            && word
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '[' | ']'));
        if shaped && !listed.iter().any(|seen| seen.id == word) {
            listed.push(Listed {
                id: word.to_owned(),
                name: capitalized(word),
                latest: true,
            });
        }
    }
    listed
}

/// The models in the Codex CLI's catalog `text` (`codex debug models`, or its cache): those its picker lists, by their
/// priority.
#[must_use]
pub fn codex_catalog(text: &str) -> Vec<Listed> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut models: Vec<(i64, Listed)> = value
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|model| {
            model
                .get("visibility")
                .and_then(Value::as_str)
                .is_none_or(|shown| shown == "list")
        })
        .filter_map(|model| {
            let id = model.get("slug").and_then(Value::as_str)?.to_owned();
            let name = model
                .get("display_name")
                .and_then(Value::as_str)
                .map_or_else(|| id.clone(), str::to_owned);
            let priority = model.get("priority").and_then(Value::as_i64).unwrap_or(i64::MAX);
            Some((
                priority,
                Listed {
                    id,
                    name,
                    latest: false,
                },
            ))
        })
        .collect();
    models.sort_by_key(|(priority, _)| *priority);
    models.into_iter().map(|(_, listed)| listed).collect()
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What `program args` prints, when it ends well within [`LISTING`].
fn run(program: &Path, args: &[&str]) -> Option<String> {
    run_within(program, args, LISTING)
}

/// What `program args` prints, when it ends well within `wait`.
fn run_within(program: &Path, args: &[&str], wait: Duration) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = teams_platform::command::Tree::spawn(&mut command).ok()?;
    let stdout = child.stdout.take()?;
    let reading = thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.take(MAX_LISTING).read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + wait;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                child.stop();
                break None;
            }
        }
    };
    child.stop();
    let text = reading.join().ok()?;
    status.filter(std::process::ExitStatus::success).map(|_| text)
}

#[cfg(test)]
#[path = "../../tests/support/local_ai_catalog_units.rs"]
mod tests;
