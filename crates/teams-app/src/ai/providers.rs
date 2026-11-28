//! The AI providers the user set, as the settings file keeps them ([`FILE_KEY`]): the command-line tools on this
//! computer (Claude Code, the Codex CLI) and the APIs that the app asks with a key (Anthropic, OpenAI, and any
//! OpenAI-compatible server), in the user's order, each with a name and an on/off switch. The file keeps only that a
//! key is in the keyring ([`Provider::has_key`]), never the key.

use toml::{Table, Value};

use super::{Backend, Model};

/// The longest provider id kept, in bytes.
const MAX_ID: usize = 64;
/// The longest provider name kept, in bytes.
pub const MAX_NAME: usize = 100;
/// The settings file's key of the list: an array of tables.
pub const FILE_KEY: &str = "ai_providers";
/// What a provider's `key` says when its key is in the keyring: the only thing the file keeps of a key.
const IN_KEYRING: &str = "keyring";

/// One AI provider of the settings.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Provider {
    /// Its id in the settings: a command-line tool's is its kind's key (`claude`, `codex`), an API's is made when it
    /// is added ([`Provider::new_api`]).
    pub id: String,
    pub kind: Backend,
    /// The name the user gave it.
    pub name: String,
    /// The user turned it on.
    pub enabled: bool,
    /// An API's base URL; `None`: its kind's own ([`default_base_url`]).
    pub base_url: Option<String>,
    /// An API's own default model: what "its default" asks for, and for an OpenAI-compatible server also what
    /// "Recommended" asks for.
    pub model: Option<String>,
    /// A key for it is in the keyring.
    pub has_key: bool,
}

impl Provider {
    /// The command-line tool `backend`, turned on.
    #[must_use]
    pub fn cli(backend: Backend) -> Self {
        Self {
            id: backend.key().to_owned(),
            kind: backend,
            name: backend.name().to_owned(),
            enabled: true,
            base_url: None,
            model: None,
            has_key: false,
        }
    }

    /// A new API provider of `kind` with an id that none in `list` has, turned on.
    #[must_use]
    pub fn new_api(kind: Backend, list: &[Self]) -> Self {
        // one of these is free: the list has fewer ids
        let id = (1..=list.len() + 1)
            .map(|at| format!("api-{at}"))
            .find(|id| !list.iter().any(|provider| provider.id == *id))
            .unwrap_or_default();
        Self {
            id,
            kind,
            name: kind.name().to_owned(),
            enabled: true,
            base_url: None,
            model: None,
            has_key: false,
        }
    }

    /// The base URL its requests go to.
    #[must_use]
    pub fn base_url(&self) -> String {
        self.base_url
            .clone()
            .unwrap_or_else(|| default_base_url(self.kind).to_owned())
    }
}

/// The base URL an API kind uses when the user gives none.
#[must_use]
pub fn default_base_url(kind: Backend) -> &'static str {
    match kind {
        Backend::Anthropic => "https://api.anthropic.com",
        Backend::OpenAi => "https://api.openai.com/v1",
        Backend::Claude | Backend::Codex | Backend::Compatible => "",
    }
}

/// Whether `id` has the shape of a provider's id: letters, digits, `-` and `_`, at most [`MAX_ID`] bytes.
#[must_use]
pub fn id_shaped(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Whether `url` can be an API's base URL: `http` or `https` with a host, at most 2048 bytes.
#[must_use]
pub fn base_url_shaped(url: &str) -> bool {
    url.len() <= 2048
        && url::Url::parse(url)
            .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some())
}

/// The providers of the settings file's `value` (see [`FILE_KEY`]), and whether some could not be read (those are
/// left out, as is a second provider of one id or one command-line tool).
#[must_use]
pub fn from_value(value: &Value) -> (Vec<Provider>, bool) {
    let Some(entries) = value.as_array() else {
        return (Vec::new(), true);
    };
    let mut list: Vec<Provider> = Vec::new();
    let mut bad = false;
    for entry in entries {
        match entry.as_table().and_then(provider_of) {
            Some(provider)
                if !list
                    .iter()
                    .any(|held| held.id == provider.id || (provider.kind.is_cli() && held.kind == provider.kind)) =>
            {
                list.push(provider);
            }
            _ => bad = true,
        }
    }
    (list, bad)
}

fn provider_of(table: &Table) -> Option<Provider> {
    let text = |key: &str| table.get(key).and_then(Value::as_str);
    let id = text("id").filter(|id| id_shaped(id))?.to_owned();
    let kind = Backend::from_key(text("kind")?)?;
    if kind.is_cli() && id != kind.key() {
        return None;
    }
    let name = text("name")
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= MAX_NAME)?;
    let enabled = table.get("enabled").map_or(Some(true), Value::as_bool)?;
    let base_url = match text("base_url") {
        Some(url) if base_url_shaped(url) && !kind.is_cli() => Some(url.to_owned()),
        Some(_) => return None,
        None => None,
    };
    let model = match text("model") {
        Some(model) => match Model::from_key(model) {
            Some(Model::Named(model)) if !kind.is_cli() => Some(model),
            _ => return None,
        },
        None => None,
    };
    let has_key = match text("key") {
        Some(IN_KEYRING) => !kind.is_cli(),
        Some(_) => return None,
        None => false,
    };
    Some(Provider {
        id,
        kind,
        name: name.to_owned(),
        enabled,
        base_url,
        model,
        has_key,
    })
}

/// `list` as the settings file keeps it: never a key, only that one is in the keyring.
#[must_use]
pub fn to_value(list: &[Provider]) -> Value {
    Value::Array(
        list.iter()
            .map(|provider| {
                let mut table = Table::new();
                table.insert("id".to_owned(), Value::String(provider.id.clone()));
                table.insert("kind".to_owned(), Value::String(provider.kind.key().to_owned()));
                table.insert("name".to_owned(), Value::String(provider.name.clone()));
                table.insert("enabled".to_owned(), Value::Boolean(provider.enabled));
                if let Some(url) = &provider.base_url {
                    table.insert("base_url".to_owned(), Value::String(url.clone()));
                }
                if let Some(model) = &provider.model {
                    table.insert("model".to_owned(), Value::String(model.clone()));
                }
                if provider.has_key {
                    table.insert("key".to_owned(), Value::String(IN_KEYRING.to_owned()));
                }
                Value::Table(table)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ToolChoice;

    fn api(id: &str, kind: Backend, has_key: bool) -> Provider {
        Provider {
            id: id.to_owned(),
            kind,
            name: format!("Synthetic {id}"),
            enabled: true,
            base_url: (kind == Backend::Compatible).then(|| "http://127.0.0.1:11434/v1".to_owned()),
            model: None,
            has_key,
        }
    }

    #[test]
    fn a_new_api_takes_an_id_none_has() {
        let list = vec![
            api("api-1", Backend::OpenAi, false),
            api("api-3", Backend::OpenAi, false),
        ];
        let added = Provider::new_api(Backend::Compatible, &list);
        assert_eq!(added.id, "api-2");
        assert_eq!(added.name, "OpenAI-compatible API");
        assert_eq!(added.base_url(), "");
        assert_eq!(
            Provider::new_api(Backend::Anthropic, &[]).base_url(),
            "https://api.anthropic.com"
        );
        assert!(id_shaped("api-1") && !id_shaped("") && !id_shaped("a b") && !id_shaped(&"a".repeat(65)));
        assert!(base_url_shaped("https://openrouter.ai/api/v1") && base_url_shaped("http://localhost:1234/v1"));
        assert!(!base_url_shaped("ftp://example.test") && !base_url_shaped("not a url"));
    }

    #[test]
    fn the_file_keeps_the_list_and_only_that_a_key_is_in_the_keyring() {
        let mut compatible = api("api-2", Backend::Compatible, true);
        compatible.model = Some("llama-synthetic".to_owned());
        let mut codex = Provider::cli(Backend::Codex);
        codex.enabled = false;
        let list = vec![api("api-1", Backend::Anthropic, true), codex, compatible];
        let value = to_value(&list);
        let text = toml::to_string(&toml::Table::from_iter([(FILE_KEY.to_owned(), value.clone())])).unwrap();
        assert!(text.contains("key = \"keyring\""), "{text}");
        assert!(!text.contains("sk-"), "{text}");
        assert_eq!(from_value(&value), (list, false));
    }

    #[test]
    fn entries_that_cannot_be_read_are_left_out() {
        let text = r#"
            ai_providers = [
                { id = "claude", kind = "claude", name = "Claude Code" },
                { id = "claude", kind = "claude", name = "Again" },
                { id = "api-1", kind = "openai", name = "Mine", enabled = false, key = "keyring", model = "gpt-x" },
                { id = "api-2", kind = "anthropic", name = "Key in file", key = "sk-ant-synthetic" },
                { id = "api-3", kind = "openai-compatible", name = "Bad URL", base_url = "ftp://x" },
                { id = "codex-2", kind = "codex", name = "Codex under another id" },
                { id = "api-4", kind = "nonsense", name = "Unknown" },
                { id = "api-5", kind = "openai", name = "" },
            ]
        "#;
        let table: toml::Table = text.parse().unwrap();
        let (list, bad) = from_value(&table[FILE_KEY]);
        assert!(bad);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], Provider::cli(Backend::Claude));
        assert_eq!(
            list[1],
            Provider {
                id: "api-1".to_owned(),
                kind: Backend::OpenAi,
                name: "Mine".to_owned(),
                enabled: false,
                base_url: None,
                model: Some("gpt-x".to_owned()),
                has_key: true,
            }
        );
        assert_eq!(from_value(&toml::Value::Integer(1)), (Vec::new(), true));
    }

    #[test]
    fn the_choices_of_an_earlier_build_name_the_same_tools() {
        assert_eq!(ToolChoice::from_key("claude"), Some(ToolChoice::cli(Backend::Claude)));
        assert_eq!(ToolChoice::from_key("codex"), Some(ToolChoice::cli(Backend::Codex)));
        assert_eq!(ToolChoice::from_key("automatic"), Some(ToolChoice::Automatic));
        assert_eq!(
            ToolChoice::from_key("api-1"),
            Some(ToolChoice::Only("api-1".to_owned()))
        );
        assert_eq!(ToolChoice::from_key("not an id"), None);
        assert_eq!(ToolChoice::cli(Backend::Codex).key(), "codex");
        assert_eq!(Model::from_key("recommended"), Some(Model::Recommended));
    }
}
