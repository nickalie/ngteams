//! The AI command-line tools the app runs on this computer, shared by everything that asks one: live captions
//! translated ([`crate::translate`]), a meeting's recap summarized ([`crate::recap_summary`]), a chat translated
//! ([`crate::chat_translate`]), a draft proofread ([`crate::message_correct`]) and a draft translated in its box
//! ([`crate::draft_translate`]). Claude Code and the Codex CLI, each with the model the user picked for that work
//! ([`AiChoice`]: the one recommended for the work, the tool's own default, or any model the tool takes, named),
//! started as a child process with nothing of the user's setup that could act on what it is given:
//!
//! - **Claude Code** runs as `claude -p --input-format stream-json --output-format stream-json`: one user message per
//!   question in one conversation, its `result` line the answer (a chat's translation also reads its text as it
//!   streams, `--include-partial-messages`). No tools, MCP servers, slash commands, settings files (so no hooks) or
//!   saved session.
//! - **Codex** runs as `codex app-server` (JSON-RPC over stdio): `initialize`, one ephemeral read-only thread whose
//!   base instructions are ours, then one `turn/start` per question, its last agent message the answer (its deltas
//!   the text so far). Hooks, apps,
//!   plugins, MCP servers, tools and project docs are turned off for it.
//!
//! The models each tool offers are listed from what it reports itself ([`catalog`]), never from a list of ours.
//!
//! [`Tool`] is one such process: started, asked on its stdin and read back from its stdout, each wait bounded and
//! stopped as soon as its caller drops the work. **Failures** ([`Failure`]) say what the user can do: the tool missing,
//! signed out, too slow, or stopped in its own words. Nothing here logs what is asked or answered, or the tool's output.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};
use teams_platform::command::Tree;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod catalog;
#[cfg(windows)]
mod windows_prompt;

pub use catalog::{Catalog, Listed};

/// How long a wait on the tool polls before it looks at whether its work was dropped.
const POLL: Duration = Duration::from_millis(100);
/// The most of the tool's stderr kept, to tell a sign-in failure: its tail.
const STDERR_TAIL: usize = 4096;
/// What a tool's refusal says when it isn't signed in.
const SIGN_IN: [&str; 9] = [
    "login",
    "log in",
    "sign in",
    "signed in",
    "logged in",
    "authenticat",
    "api key",
    "unauthorized",
    "credential",
];

/// An AI command-line tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Claude Code (`claude`).
    Claude,
    /// The Codex CLI (`codex`).
    Codex,
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
    /// A draft translated in its box, when the user asks ([`crate::draft_translate`]): the stronger model, as the
    /// user sends what comes back, with little reasoning, as the user waits on it.
    Draft,
}

impl Backend {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    /// Its name in the settings file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The command it runs.
    #[must_use]
    pub fn program(self) -> &'static str {
        self.key()
    }

    /// The model recommended for `work` ([`Model::Recommended`]). Codex has no model named plain `luna`: it refuses it
    /// for a `ChatGPT` account, so the newest Luna it lists is asked for captions, and its mid-sized Terra for a summary.
    #[must_use]
    pub fn model(self, work: Work) -> &'static str {
        match (self, work) {
            (Self::Claude, Work::Captions | Work::Chat) => "haiku",
            (Self::Claude, Work::Summary | Work::Correction | Work::Draft) => "sonnet",
            (Self::Codex, Work::Captions | Work::Chat) => "gpt-6-luna",
            (Self::Codex, Work::Summary | Work::Correction | Work::Draft) => "gpt-6.1-terra",
        }
    }

    /// How hard Codex reasons for `work` (Claude Code takes no such setting).
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
            Model::Recommended => Some(self.model(work).to_owned()),
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

    /// Its name in a label: "Claude Code", "Codex".
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// What the views call it doing `work` with its recommended model.
    #[must_use]
    pub fn label(self, work: Work) -> &'static str {
        match (self, work) {
            (Self::Claude, Work::Captions | Work::Chat) => "Claude Haiku",
            (Self::Claude, Work::Summary | Work::Correction | Work::Draft) => "Claude Sonnet",
            (Self::Codex, Work::Captions | Work::Chat) => "Codex GPT-6-Luna",
            (Self::Codex, Work::Summary | Work::Correction | Work::Draft) => "Codex GPT-6.1-Terra",
        }
    }

    /// The tool's name, as a notice says it.
    #[must_use]
    pub fn tool(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "The Codex CLI",
        }
    }

    /// Who the tool sends what it is given to.
    #[must_use]
    pub fn provider(self) -> &'static str {
        match self {
            Self::Claude => "Anthropic",
            Self::Codex => "OpenAI",
        }
    }

    /// What signs the tool in.
    #[must_use]
    pub fn sign_in(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex login",
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

/// Which tool does a work, as the settings keep it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToolChoice {
    /// Claude Code when it is installed, else Codex.
    #[default]
    Automatic,
    Only(Backend),
}

impl ToolChoice {
    pub const ALL: [Self; 3] = [Self::Automatic, Self::Only(Backend::Claude), Self::Only(Backend::Codex)];

    /// Its name in the settings file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Only(backend) => backend.key(),
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|choice| choice.key() == key)
    }

    /// Its segment's words in the settings.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Only(Backend::Claude) => "Claude",
            Self::Only(Backend::Codex) => "Codex",
        }
    }
}

/// The model a tool is asked for, as the settings keep it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Model {
    /// The one this app recommends for the work ([`Backend::model`]).
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
/// and the settings show each with the same control ([`crate::settings_view`]'s AI picker).
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

/// The tools found on this computer: an app global, looked for on the `PATH` once (and again on Try again:
/// [`Tools::look_again`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    pub(crate) found: Vec<(Backend, PathBuf)>,
}

impl Global for Tools {}

impl Tools {
    /// Looks for each tool on the `PATH`.
    #[must_use]
    pub fn detect() -> Self {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = Backend::ALL
            .into_iter()
            .filter_map(|backend| {
                std::env::split_paths(&path)
                    .flat_map(|dir| program_paths(&dir, backend.program()))
                    .find(|candidate| executable(candidate))
                    .map(|program| (backend, program))
            })
            .collect();
        Self { found }
    }

    /// These tools, at these paths.
    #[must_use]
    pub fn with(found: Vec<(Backend, PathBuf)>) -> Self {
        Self { found }
    }

    /// The app's, looked for on first use.
    pub fn of(cx: &mut App) -> Self {
        if !cx.has_global::<Self>() {
            cx.set_global(Self::detect());
        }
        cx.global::<Self>().clone()
    }

    /// Looks on the `PATH` again (Try again after a tool was missing); when what is found changed, the models the
    /// tools offer are listed again too.
    pub fn look_again(cx: &mut App) {
        Self::found_again(Self::detect(), cx);
    }

    /// The tools a look on the `PATH` found: the app's from now on, and the models listed again when they changed.
    pub fn found_again(found: Self, cx: &mut App) {
        if cx.try_global::<Self>() == Some(&found) {
            return;
        }
        cx.set_global(found);
        if cx.has_global::<Catalog>() {
            Catalog::relist(cx);
        }
    }

    /// The tools installed, Claude Code first.
    pub fn installed(&self) -> impl Iterator<Item = Backend> + '_ {
        Backend::ALL
            .into_iter()
            .filter(|backend| self.found.iter().any(|(found, _)| found == backend))
    }

    /// The tool `choice` comes to, and its program.
    ///
    /// # Errors
    /// It isn't installed (for [`ToolChoice::Automatic`], neither is).
    pub fn pick(&self, choice: ToolChoice) -> Result<(Backend, PathBuf), Failure> {
        let program = |backend: Backend| {
            self.found
                .iter()
                .find(|(found, _)| *found == backend)
                .map(|(_, program)| (backend, program.clone()))
        };
        match choice {
            ToolChoice::Automatic => Backend::ALL
                .into_iter()
                .find_map(program)
                .ok_or(Failure::NotInstalled(None)),
            ToolChoice::Only(backend) => program(backend).ok_or(Failure::NotInstalled(Some(backend))),
        }
    }
}

#[cfg(unix)]
pub(crate) fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
pub(crate) fn executable(path: &Path) -> bool {
    path.is_file()
        && path.extension().is_some_and(|extension| {
            ["exe", "com", "cmd", "bat"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

fn program_paths(dir: &Path, program: &str) -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        vec![dir.join(program)]
    }
    #[cfg(windows)]
    {
        ["exe", "com", "cmd", "bat"]
            .map(|extension| dir.join(format!("{program}.{extension}")))
            .to_vec()
    }
}

/// Why a tool's work ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The tool asked for isn't on the `PATH`; `None`: neither is.
    NotInstalled(Option<Backend>),
    SignedOut(Backend),
    TimedOut(Backend),
    /// The tool stopped or refused, in its own words when it gave some.
    Stopped(Backend, Option<String>),
}

impl Failure {
    /// What a draft's failure line adds when the tool isn't installed: install the one named, or one of the two.
    #[must_use]
    pub fn install_hint(&self) -> &'static str {
        match self {
            Self::NotInstalled(None) => " Install one, or choose another tool in Settings under General.",
            Self::NotInstalled(Some(_)) => " Install it, or choose another tool in Settings under General.",
            _ => "",
        }
    }

    /// What a view says of it.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotInstalled(None) => {
                "Neither Claude Code (claude) nor the Codex CLI (codex) is installed.".to_owned()
            }
            Self::NotInstalled(Some(backend)) => {
                format!("{} ({}) isn't installed.", backend.tool(), backend.program())
            }
            Self::SignedOut(backend) => format!(
                "{} isn't signed in. Run {} in a terminal, then try again.",
                backend.tool(),
                backend.sign_in()
            ),
            Self::TimedOut(backend) => format!("{} took too long to answer.", backend.tool()),
            Self::Stopped(backend, Some(why)) => format!("{} stopped: {why}", backend.tool()),
            Self::Stopped(backend, None) => format!("{} stopped unexpectedly.", backend.tool()),
        }
    }
}

/// What a [`Tool`] runs: the tool, where it is, its work, model and instructions, and how long it may take to start.
#[derive(Clone, Debug)]
pub struct Launch {
    pub backend: Backend,
    pub program: PathBuf,
    pub work: Work,
    pub model: Model,
    pub instructions: String,
    /// How long the process may take to start (Codex answers its thread's start).
    pub first: Duration,
}

/// `text` cut to at most `max` bytes, on a character's boundary.
#[must_use]
pub fn cap(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A running tool and where its conversation stands.
pub struct Tool {
    backend: Backend,
    work: Work,
    /// The model asked for, `None`: the tool's default.
    model: Option<String>,
    child: Tree,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    /// Codex's thread.
    thread: Option<String>,
    next_id: u64,
    /// Questions asked, and their characters.
    turns: usize,
    chars: usize,
    /// It answered once: later answers may get a shorter wait.
    answered: bool,
    // Dropped after the process tree: a tool may read its prompt after startup.
    #[cfg(windows)]
    _prompt: Option<tempfile::TempDir>,
}

impl Tool {
    /// Starts `launch`'s tool (Codex's thread started too), giving up once `stop` is set.
    ///
    /// # Errors
    /// The tool couldn't start, refused, or took longer than [`Launch::first`].
    pub fn start(launch: &Launch, stop: &AtomicBool) -> Result<Self, Failure> {
        let backend = launch.backend;
        let args = backend.args(launch.work, &launch.model, &launch.instructions);
        #[cfg(windows)]
        let (args, prompt) = windows_prompt::prepare(launch, args)
            .map_err(|error| Failure::Stopped(backend, Some(error.to_string())))?;
        let mut command = Command::new(&launch.program);
        command
            .args(args)
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Tree::spawn(&mut command).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Failure::NotInstalled(Some(backend)),
            _ => Failure::Stopped(backend, Some(error.to_string())),
        })?;
        let stdin = child.stdin.take();
        let lines = child.stdout.take().map_or_else(
            || mpsc::channel().1,
            |stdout| {
                let (sender, lines) = mpsc::channel();
                thread::spawn(move || read_lines(stdout, &sender));
                lines
            },
        );
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(pipe) = child.stderr.take() {
            let tail = Arc::clone(&stderr);
            thread::spawn(move || keep_tail(pipe, &tail));
        }
        let mut tool = Self {
            backend,
            work: launch.work,
            model: backend.model_of(launch.work, &launch.model),
            child,
            stdin,
            lines,
            stderr,
            thread: None,
            next_id: 1,
            turns: 0,
            chars: 0,
            answered: false,
            #[cfg(windows)]
            _prompt: prompt,
        };
        if backend == Backend::Codex
            && let Err(failure) = tool.codex_thread(&launch.instructions, launch.first, stop)
        {
            tool.close();
            return Err(failure);
        }
        Ok(tool)
    }

    /// Questions asked so far.
    #[must_use]
    pub fn turns(&self) -> usize {
        self.turns
    }

    /// Characters asked so far.
    #[must_use]
    pub fn chars(&self) -> usize {
        self.chars
    }

    /// Whether it answered once.
    #[must_use]
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// Codex: initializes the app server and starts the thread the questions go to.
    fn codex_thread(&mut self, instructions: &str, wait: Duration, stop: &AtomicBool) -> Result<(), Failure> {
        let deadline = Instant::now() + wait;
        let initialize = self.request(
            "initialize",
            &json!({"clientInfo": {"name": "ngteams", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        self.answer_of(initialize, deadline, stop)?;
        self.write(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let mut params = json!({
            "cwd": cwd,
            "approvalPolicy": "never",
            "sandbox": "read-only",
            "ephemeral": true,
            "baseInstructions": instructions,
        });
        // none named: the model the user's Codex is set to
        if let (Some(model), Some(params)) = (&self.model, params.as_object_mut()) {
            params.insert("model".to_owned(), Value::String(model.clone()));
        }
        let start = self.request("thread/start", &params)?;
        let started = self.answer_of(start, deadline, stop)?;
        let thread = started
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or(Failure::Stopped(self.backend, None))?;
        self.thread = Some(thread.to_owned());
        Ok(())
    }

    /// Sends JSON-RPC request `method`; its id.
    fn request(&mut self, method: &str, params: &Value) -> Result<u64, Failure> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    /// The result of JSON-RPC request `id`, answering any request the server makes meanwhile.
    fn answer_of(&mut self, id: u64, deadline: Instant, stop: &AtomicBool) -> Result<Value, Failure> {
        loop {
            let message = self.next_message(deadline, stop)?;
            self.refuse_request(&message)?;
            if message.get("id").and_then(Value::as_u64) == Some(id) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(self.refusal(error.get("message").and_then(Value::as_str)));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// The server asking something of the client (an approval): refused, as nothing here may be approved.
    fn refuse_request(&mut self, message: &Value) -> Result<(), Failure> {
        if let (Some(id), Some(_)) = (message.get("id"), message.get("method")) {
            self.write(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "not supported"}}))?;
        }
        Ok(())
    }

    /// Sends `message` and waits up to `wait` for the answer, trimmed.
    ///
    /// # Errors
    /// The tool refused, stopped, took longer than `wait`, or `stop` was set.
    pub fn ask(&mut self, message: &str, wait: Duration, stop: &AtomicBool) -> Result<String, Failure> {
        self.ask_streaming(message, wait, stop, &mut |_| {})
    }

    /// [`Self::ask`], telling `partial` the answer's text so far each time more of it comes (when the tool streams it:
    /// Claude Code started for [`Work::Chat`], and Codex), before the whole answer is given back.
    ///
    /// # Errors
    /// As [`Self::ask`].
    pub fn ask_streaming(
        &mut self,
        message: &str,
        wait: Duration,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        let deadline = Instant::now() + wait;
        self.turns += 1;
        self.chars += message.len();
        let text = match self.backend {
            Backend::Claude => self.ask_claude(message, deadline, stop, partial),
            Backend::Codex => self.ask_codex(message, deadline, stop, partial),
        }?;
        self.answered = true;
        Ok(text.trim().to_owned())
    }

    fn ask_claude(
        &mut self,
        message: &str,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        self.write(&json!({"type": "user", "message": {"role": "user", "content": message}}))?;
        let mut so_far = String::new();
        loop {
            let line = self.next_message(deadline, stop)?;
            match line.get("type").and_then(Value::as_str) {
                Some("stream_event") => {
                    stream_event(&line, &mut so_far, partial);
                    continue;
                }
                Some("result") => {}
                _ => continue,
            }
            let said = line.get("result").and_then(Value::as_str);
            if line.get("is_error").and_then(Value::as_bool) == Some(true) {
                return Err(self.refusal(said));
            }
            return Ok(said.unwrap_or_default().to_owned());
        }
    }

    fn ask_codex(
        &mut self,
        message: &str,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        let thread = self.thread.clone().unwrap_or_default();
        let id = self.request(
            "turn/start",
            &json!({
                "threadId": thread,
                "input": [{"type": "text", "text": message}],
                "effort": Backend::Codex.effort(self.work),
            }),
        )?;
        let (mut text, mut error) = (None::<String>, None::<String>);
        // the agent message being written: its item and its text so far
        let (mut item, mut so_far) = (String::new(), String::new());
        loop {
            let line = self.next_message(deadline, stop)?;
            self.refuse_request(&line)?;
            if line.get("id").and_then(Value::as_u64) == Some(id)
                && let Some(refused) = line.get("error")
            {
                return Err(self.refusal(refused.get("message").and_then(Value::as_str)));
            }
            let params = line.get("params").unwrap_or(&Value::Null);
            match line.get("method").and_then(Value::as_str) {
                Some("item/agentMessage/delta") => {
                    let of = params.get("itemId").and_then(Value::as_str).unwrap_or_default();
                    if of != item {
                        of.clone_into(&mut item);
                        so_far.clear();
                    }
                    so_far.push_str(params.get("delta").and_then(Value::as_str).unwrap_or_default());
                    partial(&so_far);
                }
                Some("item/completed") if params.pointer("/item/type") == Some(&json!("agentMessage")) => {
                    text = params.pointer("/item/text").and_then(Value::as_str).map(str::to_owned);
                }
                Some("error") if params.get("willRetry") != Some(&json!(true)) => {
                    error = params
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                Some("turn/completed") => {
                    let failed = params.pointer("/turn/error/message").and_then(Value::as_str);
                    return match (error.or_else(|| failed.map(str::to_owned)), text) {
                        (Some(why), _) => Err(self.refusal(Some(&why))),
                        (None, Some(text)) => Ok(text),
                        (None, None) => Err(Failure::Stopped(self.backend, Some("it gave no answer.".to_owned()))),
                    };
                }
                _ => {}
            }
        }
    }

    fn write(&mut self, message: &Value) -> Result<(), Failure> {
        let backend = self.backend;
        let stdin = self.stdin.as_mut().ok_or(Failure::Stopped(backend, None))?;
        let mut line = message.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|_| self.exited())
    }

    /// The tool's next JSON line (other lines skipped), by `deadline`.
    fn next_message(&mut self, deadline: Instant, stop: &AtomicBool) -> Result<Value, Failure> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped(self.backend, None));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::TimedOut(self.backend));
            }
            match self.lines.recv_timeout(left.min(POLL)) {
                Ok(line) => {
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        return Ok(value);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(self.exited()),
            }
        }
    }

    /// Why the process ended: signed out when its last words say so, else in those words.
    fn exited(&mut self) -> Failure {
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        // the stderr reader may still be taking its last bytes
        thread::sleep(Duration::from_millis(50));
        let tail = self.stderr.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let last = tail.lines().rev().find(|line| !line.trim().is_empty());
        self.refusal(last)
    }

    /// A refusal in the tool's words: a sign-in one as such.
    fn refusal(&self, said: Option<&str>) -> Failure {
        let Some(said) = said.map(str::trim).filter(|said| !said.is_empty()) else {
            return Failure::Stopped(self.backend, None);
        };
        let lower = said.to_lowercase();
        if SIGN_IN.iter().any(|word| lower.contains(word)) {
            return Failure::SignedOut(self.backend);
        }
        let line = said.lines().next().unwrap_or(said);
        let mut why = cap(line, 160);
        if why.len() < line.len() {
            why.push('…');
        }
        Failure::Stopped(self.backend, Some(why))
    }

    /// Ends the process: its stdin closed (both tools leave then), and after a moment its whole group killed.
    pub fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        self.child.stop();
    }
}

/// Claude Code's streamed event `line` (`--include-partial-messages`): a text delta adds to `so_far`, told to `partial`;
/// a new message starts it over.
fn stream_event(line: &Value, so_far: &mut String, partial: &mut dyn FnMut(&str)) {
    let event = line.get("event").unwrap_or(&Value::Null);
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => so_far.clear(),
        Some("content_block_delta") if event.pointer("/delta/type") == Some(&json!("text_delta")) => {
            so_far.push_str(event.pointer("/delta/text").and_then(Value::as_str).unwrap_or_default());
            partial(so_far);
        }
        _ => {}
    }
}

/// Hands each line the tool writes to its reader, until it ends.
fn read_lines(stdout: impl std::io::Read, sender: &mpsc::Sender<String>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if sender.send(line).is_err() {
            break;
        }
    }
}

/// Keeps the last [`STDERR_TAIL`] bytes the tool writes to stderr.
fn keep_tail(mut pipe: impl std::io::Read, tail: &Mutex<String>) {
    let mut chunk = [0_u8; 1024];
    while let Ok(read @ 1..) = pipe.read(&mut chunk) {
        let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
        tail.push_str(&String::from_utf8_lossy(&chunk[..read]));
        if tail.len() > STDERR_TAIL {
            let cut = tail.len() - STDERR_TAIL;
            let cut = (cut..tail.len()).find(|at| tail.is_char_boundary(*at)).unwrap_or(0);
            tail.drain(..cut);
        }
    }
}
