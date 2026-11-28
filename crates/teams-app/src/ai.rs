//! The AI choices of the settings, without the tools that answer: the kinds of providers ([`Backend`]: Claude Code,
//! the Codex CLI and the APIs, with the command line each is started with and the model it is asked for), the works
//! they are asked for ([`Work`]), and a setting's pick of a provider and a model ([`AiChoice`]). The list of the
//! providers the user set, as the settings file keeps it, is [`providers`]. The desktop (`teams-ui`'s `local_ai`)
//! starts the tools and asks the APIs.

use serde::{Deserialize, Serialize};

pub mod providers;

/// The kind of an AI provider: a command-line tool on this computer, or an API that the app asks with a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Claude Code (`claude`).
    Claude,
    /// The Codex CLI (`codex`).
    Codex,
    /// The Anthropic Messages API.
    Anthropic,
    /// The OpenAI Chat Completions API.
    OpenAi,
    /// An API that takes OpenAI's Chat Completions at its own base URL (OpenRouter, Ollama, LM Studio, Azure).
    Compatible,
}

/// What a tool is asked to do: each work has the model it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Work {
    /// Live captions, one at a time: the fastest model.
    Captions,
    /// A meeting's transcript summarized: one long job, a stronger model.
    Summary,
    /// A chat's messages translated, a batch at a time with the chat as context, and the account's own before they
    /// are sent: the fast model, as someone waits on a send.
    Chat,
    /// A draft proofread before it is sent, when the user asks: the stronger model, as the better word is the point.
    Correction,
    /// A draft translated in its box, when the user asks (the desktop's `draft_translate`): the stronger model, as the
    /// user sends what comes back, with little reasoning, as the user waits on it.
    Draft,
}

impl Backend {
    pub const ALL: [Self; 5] = [
        Self::Claude,
        Self::Codex,
        Self::Anthropic,
        Self::OpenAi,
        Self::Compatible,
    ];
    /// The command-line tools, in the order Automatic tries them when the user did not order the providers.
    pub const CLI: [Self; 2] = [Self::Claude, Self::Codex];

    /// Its name in the settings file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Compatible => "openai-compatible",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|backend| backend.key() == key)
    }

    /// Whether it is a command-line tool on this computer (else an API asked with a key).
    #[must_use]
    pub fn is_cli(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }

    /// The command it runs.
    #[must_use]
    pub fn program(self) -> &'static str {
        self.key()
    }

    /// The model recommended for `work` ([`Model::Recommended`]); `None`: this app knows none (an OpenAI-compatible
    /// API serves the models of its own server). Codex has no model named plain `luna`: it refuses it for a `ChatGPT`
    /// account, so the newest Luna it lists is asked for captions, and its mid-sized Terra for a summary.
    #[must_use]
    pub fn recommended(self, work: Work) -> Option<&'static str> {
        let fast = matches!(work, Work::Captions | Work::Chat);
        match self {
            Self::Claude => Some(if fast { "haiku" } else { "sonnet" }),
            Self::Codex | Self::OpenAi => Some(if fast { "gpt-6-luna" } else { "gpt-6.1-terra" }),
            Self::Anthropic => Some(if fast { "claude-haiku-5-5" } else { "claude-sonnet-5-5" }),
            Self::Compatible => None,
        }
    }

    /// How hard the model reasons for `work`: Codex's effort, Anthropic's `output_config.effort`, OpenAI's
    /// `reasoning_effort` (Claude Code and OpenAI-compatible servers take none).
    #[must_use]
    pub fn effort(self, work: Work) -> &'static str {
        match work {
            Work::Captions | Work::Chat | Work::Draft => "low",
            Work::Summary | Work::Correction => "medium",
        }
    }

    /// The model `model` asks it for doing `work`: `None` leaves it to the tool's own default.
    #[must_use]
    pub fn model_of(self, work: Work, model: &Model) -> Option<String> {
        match model {
            Model::Recommended => self.recommended(work).map(str::to_owned),
            Model::ToolDefault => None,
            Model::Named(name) => Some(name.clone()),
        }
    }

    /// What the views call it doing `work` with `model`: "Claude Haiku" (recommended), "Claude Code" (its default),
    /// "Claude Code (opus)" (named).
    #[must_use]
    pub fn label_of(self, work: Work, model: &Model) -> String {
        match model {
            Model::Recommended => self.label(work).to_owned(),
            Model::ToolDefault => self.name().to_owned(),
            Model::Named(name) => format!("{} ({name})", self.name()),
        }
    }

    /// Its name in a label: "Claude Code", "Codex", "Anthropic API".
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Anthropic => "Anthropic API",
            Self::OpenAi => "OpenAI API",
            Self::Compatible => "OpenAI-compatible API",
        }
    }

    /// What the views call it doing `work` with its recommended model.
    #[must_use]
    pub fn label(self, work: Work) -> &'static str {
        let fast = matches!(work, Work::Captions | Work::Chat);
        match self {
            Self::Claude | Self::Anthropic => {
                if fast {
                    "Claude Haiku"
                } else {
                    "Claude Sonnet"
                }
            }
            Self::Codex => {
                if fast {
                    "Codex GPT-6-Luna"
                } else {
                    "Codex GPT-6.1-Terra"
                }
            }
            Self::OpenAi => {
                if fast {
                    "GPT-6-Luna"
                } else {
                    "GPT-6.1-Terra"
                }
            }
            Self::Compatible => self.name(),
        }
    }

    /// The tool's name, as a notice says it.
    #[must_use]
    pub fn tool(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "The Codex CLI",
            Self::Anthropic => "The Anthropic API",
            Self::OpenAi => "The OpenAI API",
            Self::Compatible => "The OpenAI-compatible API",
        }
    }

    /// Who the tool sends what it is given to.
    #[must_use]
    pub fn provider(self) -> &'static str {
        match self {
            Self::Claude | Self::Anthropic => "Anthropic",
            Self::Codex | Self::OpenAi => "OpenAI",
            Self::Compatible => "the API's server",
        }
    }

    /// What signs the tool in.
    #[must_use]
    pub fn sign_in(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex login",
            Self::Anthropic | Self::OpenAi | Self::Compatible => "",
        }
    }

    /// Its command line for `work` with `model`, `instructions` being the system prompt (Codex takes the model and the
    /// instructions in its thread instead).
    #[must_use]
    pub fn args(self, work: Work, model: &Model, instructions: &str) -> Vec<String> {
        match self {
            Self::Claude => {
                let mut args = vec!["-p".to_owned()];
                if let Some(model) = self.model_of(work, model) {
                    args.extend(["--model".to_owned(), model]);
                }
                // a chat's translations show as each message's comes, not once the whole batch is done
                if work == Work::Chat {
                    args.push("--include-partial-messages".to_owned());
                }
                args.extend(
                    [
                        "--input-format",
                        "stream-json",
                        "--output-format",
                        "stream-json",
                        "--verbose",
                        "--tools",
                        "",
                        "--no-session-persistence",
                        "--setting-sources",
                        "",
                        "--strict-mcp-config",
                        "--disable-slash-commands",
                        "--system-prompt",
                        instructions,
                    ]
                    .map(str::to_owned),
                );
                args
            }
            Self::Anthropic | Self::OpenAi | Self::Compatible => Vec::new(),
            Self::Codex => {
                let mut args = vec![
                    "app-server".to_owned(),
                    "-c".to_owned(),
                    format!("model_reasoning_effort=\"{}\"", self.effort(work)),
                    "-c".to_owned(),
                    "mcp_servers={}".to_owned(),
                    "-c".to_owned(),
                    "project_doc_max_bytes=0".to_owned(),
                ];
                for feature in CODEX_OFF {
                    args.extend(["-c".to_owned(), format!("features.{feature}=false")]);
                }
                args
            }
        }
    }
}

/// Codex's features turned off: no hooks of the user's run on what it is given, and no tools.
const CODEX_OFF: [&str; 13] = [
    "hooks",
    "apps",
    "plugins",
    "multi_agent",
    "shell_tool",
    "unified_exec",
    "image_generation",
    "goals",
    "view_image",
    "browser_use",
    "computer_use",
    "tool_suggest",
    "skill_search",
];

/// Which provider does a work, as the settings keep it ([`providers`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToolChoice {
    /// The first provider in the user's order that is on and can work (the desktop's `providers::pick`).
    #[default]
    Automatic,
    /// The provider of this id ([`providers::Provider::id`]): the command-line tools keep their kind's key (`claude`,
    /// `codex`), so a choice of an earlier build names the same tool.
    Only(String),
}

impl ToolChoice {
    /// The command-line tool `backend`.
    #[must_use]
    pub fn cli(backend: Backend) -> Self {
        Self::Only(backend.key().to_owned())
    }

    /// Its name in the settings file.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Automatic => "automatic",
            Self::Only(id) => id,
        }
    }

    /// The choice `key` names: `automatic`, or a provider's id ([`providers::id_shaped`]).
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "automatic" => Some(Self::Automatic),
            _ => providers::id_shaped(key).then(|| Self::Only(key.to_owned())),
        }
    }
}

/// The model a tool is asked for, as the settings keep it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Model {
    /// The one this app recommends for the work ([`Backend::recommended`]).
    #[default]
    Recommended,
    /// None asked for: the tool uses its own default.
    ToolDefault,
    /// Any model the tool takes, by the name its command line takes (an alias or a full name).
    Named(String),
}

/// The longest model name kept, in bytes.
pub const MAX_MODEL_NAME: usize = 100;

impl Model {
    /// Its name in the settings file: `recommended`, `default`, or the model's name.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Recommended => "recommended",
            Self::ToolDefault => "default",
            Self::Named(name) => name,
        }
    }

    /// The model `key` names (as the settings file or the user types it, trimmed): `recommended`, `default` (Claude
    /// Code's alias for its default, so the same thing), or a model's name: visible characters without spaces, at
    /// most [`MAX_MODEL_NAME`] bytes, not starting with `-` (it goes on a command line).
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        let key = key.trim();
        match key {
            "recommended" => Some(Self::Recommended),
            "default" => Some(Self::ToolDefault),
            _ => {
                let shaped = !key.is_empty()
                    && key.len() <= MAX_MODEL_NAME
                    && !key.starts_with('-')
                    && key.chars().all(|c| !c.is_whitespace() && !c.is_control());
                shaped.then(|| Self::Named(key.to_owned()))
            }
        }
    }
}

/// A local AI choice of the settings: which tool, then which model. Every setting that picks a local AI keeps one,
/// and the settings show each with the same control (the desktop settings view's AI picker).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AiChoice {
    pub tool: ToolChoice,
    pub model: Model,
}

impl AiChoice {
    /// `tool` with its recommended model.
    #[must_use]
    pub fn of(tool: ToolChoice) -> Self {
        Self {
            tool,
            model: Model::Recommended,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKS: [Work; 5] = [Work::Captions, Work::Summary, Work::Chat, Work::Correction, Work::Draft];

    #[test]
    fn every_kind_has_its_names() {
        for backend in Backend::CLI {
            assert_eq!(backend.program(), backend.key());
            assert!(backend.is_cli() && !backend.sign_in().is_empty());
        }
        for backend in Backend::ALL {
            assert_eq!(Backend::from_key(backend.key()), Some(backend));
            assert!(!backend.name().is_empty() && !backend.tool().is_empty() && !backend.provider().is_empty());
            for work in WORKS {
                assert!(!backend.label(work).is_empty() && !backend.effort(work).is_empty());
                assert_eq!(backend.recommended(work).is_none(), backend == Backend::Compatible);
            }
        }
        assert_eq!(Backend::from_key("gemini"), None);
        assert!(!Backend::OpenAi.is_cli() && Backend::OpenAi.sign_in().is_empty());
    }

    #[test]
    fn a_model_is_asked_and_named_as_chosen() {
        let named = Model::Named("opus".to_owned());
        assert_eq!(
            Backend::Claude.model_of(Work::Captions, &Model::Recommended).as_deref(),
            Some("haiku")
        );
        assert_eq!(Backend::Claude.model_of(Work::Summary, &Model::ToolDefault), None);
        assert_eq!(Backend::Claude.model_of(Work::Summary, &named).as_deref(), Some("opus"));
        assert_eq!(
            Backend::Claude.label_of(Work::Chat, &Model::Recommended),
            "Claude Haiku"
        );
        assert_eq!(Backend::Claude.label_of(Work::Chat, &Model::ToolDefault), "Claude Code");
        assert_eq!(Backend::Claude.label_of(Work::Chat, &named), "Claude Code (opus)");
        assert_eq!(Backend::Codex.label(Work::Summary), "Codex GPT-6.1-Terra");
        assert_eq!(Backend::OpenAi.label(Work::Captions), "GPT-6-Luna");
        assert_eq!(Backend::Compatible.label(Work::Captions), Backend::Compatible.name());
    }

    #[test]
    fn each_tool_runs_with_its_own_command_line() {
        let claude = Backend::Claude.args(Work::Chat, &Model::Recommended, "Synthetic instructions.");
        assert_eq!(claude[..3], ["-p", "--model", "haiku"]);
        assert!(claude.contains(&"--include-partial-messages".to_owned()));
        assert_eq!(claude.last().map(String::as_str), Some("Synthetic instructions."));
        let default = Backend::Claude.args(Work::Summary, &Model::ToolDefault, "x");
        assert!(
            !default.contains(&"--model".to_owned()) && !default.contains(&"--include-partial-messages".to_owned())
        );
        let codex = Backend::Codex.args(Work::Summary, &Model::Recommended, "unused");
        assert_eq!(codex[0], "app-server");
        assert!(codex.contains(&"model_reasoning_effort=\"medium\"".to_owned()));
        assert!(codex.contains(&"features.hooks=false".to_owned()));
        assert!(Backend::Anthropic.args(Work::Chat, &Model::Recommended, "x").is_empty());
    }

    #[test]
    fn a_choice_goes_through_its_keys() {
        assert_eq!(ToolChoice::from_key("automatic"), Some(ToolChoice::Automatic));
        assert_eq!(ToolChoice::Automatic.key(), "automatic");
        assert_eq!(ToolChoice::cli(Backend::Codex).key(), "codex");
        assert_eq!(ToolChoice::from_key("not an id"), None);
        assert_eq!(Model::from_key(" default "), Some(Model::ToolDefault));
        assert_eq!(Model::from_key("recommended"), Some(Model::Recommended));
        assert_eq!(Model::from_key("gpt-x"), Some(Model::Named("gpt-x".to_owned())));
        for bad in ["", "-rm", "two words", &"m".repeat(MAX_MODEL_NAME + 1)] {
            assert_eq!(Model::from_key(bad), None, "{bad}");
        }
        for model in [
            Model::Recommended,
            Model::ToolDefault,
            Model::Named("sonnet".to_owned()),
        ] {
            assert_eq!(Model::from_key(model.key()), Some(model));
        }
        assert_eq!(
            AiChoice::of(ToolChoice::Automatic),
            AiChoice {
                tool: ToolChoice::Automatic,
                model: Model::Recommended
            }
        );
    }
}
