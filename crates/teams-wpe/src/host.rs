//! [`Host`]: the handle the consumer drives the engine with, on a thread of this process or in a worker process
//! ([`Backend`], crate `worker`). Every call is a command posted to the engine and returns at once; what follows comes
//! back as [`Event`]s.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use async_channel::{Receiver, Sender};

use crate::config::HostConfig;
use crate::engine;
use crate::event::Event;
use crate::spec::ViewId;
use crate::worker::Client;

pub use teams_wpe_client::host::{Command, Evaluation, Load, StartError, ViewCommand};

static STARTED: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// How long a shutdown waits for the engine to close its pages.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Ends the process with `code` at once, after flushing stdout and stderr, without running its exit handlers. A
/// process that started the engine on a thread of its own ends through this, after [`Host::shutdown`]: `WebKit`
/// registers static destructors that unreference its objects and assert they run on its main thread, which is the
/// engine thread (it never ends), so `exit` (and returning from `main`) runs them on the calling thread, where they
/// abort.
pub fn exit_process(code: i32) -> ! {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` ends the process without touching any state of it; nothing after it runs.
    unsafe { libc::_exit(code) }
}

/// Where the engine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// On a thread of this process.
    InProcess,
    /// In a worker process of its own (crate `worker`).
    Worker,
}

/// The engine on a thread of this process: the commands it takes, and the word that it stopped.
#[derive(Debug)]
pub(crate) struct Local {
    commands: Sender<Command>,
    /// Told once the engine has closed everything; `None` after a stop.
    stopped: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Local {
    /// Starts the engine thread (the caller made sure it is the process's one start).
    pub(crate) fn start(config: HostConfig) -> Result<(Self, Receiver<Event>), StartError> {
        let (commands, command_queue) = async_channel::unbounded();
        let (events, event_queue) = async_channel::unbounded();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let (stop, stopped) = std::sync::mpsc::sync_channel(1);
        // never joined: the engine thread outlives the host (type docs)
        std::thread::Builder::new()
            .name("wpe-engine".to_owned())
            .spawn(move || engine::run(&config, command_queue, events, &ready, stop))
            .map_err(StartError::Thread)?;
        let local = Self {
            commands,
            stopped: std::sync::Mutex::new(Some(stopped)),
        };
        match started.recv() {
            Ok(Ok(())) => Ok((local, event_queue)),
            Ok(Err(message)) => Err(StartError::Display(message)),
            Err(_) => Err(StartError::Ended),
        }
    }

    pub(crate) fn post(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            log::debug!("the WPE engine has ended: command dropped");
        }
    }

    pub(crate) fn evaluate(&self, view: ViewId, script: String, world: Option<String>) -> Receiver<Evaluation> {
        let (reply, result) = async_channel::bounded(1);
        self.post(Command::View(view, ViewCommand::Evaluate { script, world, reply }));
        result
    }

    /// Closes every page and stops the engine, waiting a bounded time; once.
    pub(crate) fn stop(&self) {
        let taken = self
            .stopped
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(stopped) = taken else { return };
        self.post(Command::Quit);
        match stopped.recv_timeout(STOP_TIMEOUT) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => {
                log::warn!("the WPE engine did not stop within {STOP_TIMEOUT:?}; leaving it");
            }
        }
    }
}

#[derive(Debug)]
enum Engine {
    Local(Local),
    Worker(Client),
}

/// The engine: WPE `WebKit` rendering pages offscreen, on a thread of its own with its own `GMainContext`, in this
/// process or in a worker process ([`Backend`]).
///
/// One in-process engine per process (a worker engine is a process of its own). Dropping it shuts the engine down, as
/// [`Host::shutdown`] does. In process, the thread itself lives on, parked with its main loop stopped, until the
/// process ends, since it is `WebKit`'s main thread, which must never end (its thread-local teardown waits forever on
/// the run loop it tears down); the process then ends through [`exit_process`]. A worker ends with its engine.
///
/// `Send` and `Sync`: every method posts a command, so a renderer thread may report a frame presented.
#[derive(Debug)]
pub struct Host {
    engine: Engine,
}

impl Host {
    /// Starts the engine on a thread of this process. Events arrive on the returned receiver until the engine ends.
    ///
    /// # Errors
    ///
    /// An invalid `config`, a second start in this process, a thread or a worker that cannot be started, or a
    /// display that does not connect (a render node `WebKit` cannot use).
    pub fn start(config: HostConfig) -> Result<(Self, Receiver<Event>), StartError> {
        Self::start_in(config, Backend::InProcess)
    }

    /// Starts the engine in a worker process of its own (crate `worker`): the image `teams_wpe_client::worker::pin_worker`
    /// pinned, so a process may start one after another.
    ///
    /// # Errors
    ///
    /// As [`Host::start`].
    pub fn start_worker(config: HostConfig) -> Result<(Self, Receiver<Event>), StartError> {
        Self::start_in(config, Backend::Worker)
    }

    fn start_in(config: HostConfig, backend: Backend) -> Result<(Self, Receiver<Event>), StartError> {
        config.validate()?;
        let (engine, events) = match backend {
            Backend::InProcess => {
                // a worker is a process of its own, so only the in-process engine is once per process
                if STARTED.swap(true, Ordering::SeqCst) {
                    return Err(StartError::AlreadyStarted);
                }
                let (local, events) = Local::start(config)?;
                (Engine::Local(local), events)
            }
            Backend::Worker => {
                let (client, events) = Client::start(&config, &teams_wpe_client::EngineSource::System)?;
                (Engine::Worker(client), events)
            }
        };
        Ok((Self { engine }, events))
    }

    /// Where the engine runs.
    #[must_use]
    pub fn backend(&self) -> Backend {
        match self.engine {
            Engine::Local(_) => Backend::InProcess,
            Engine::Worker(_) => Backend::Worker,
        }
    }

    /// The worker's pid, when the engine runs in one.
    #[must_use]
    pub fn worker_pid(&self) -> Option<u32> {
        match &self.engine {
            Engine::Local(_) => None,
            Engine::Worker(client) => Some(client.pid()),
        }
    }

    fn send(&self, command: Command) {
        match &self.engine {
            Engine::Local(local) => local.post(command),
            Engine::Worker(client) => client.send(command),
        }
    }

    /// Runs `script` in the page's main frame, in `world` (`None`: the page's own); the result arrives on the
    /// returned channel (an error when the engine ended first).
    #[must_use]
    pub fn evaluate(&self, view: ViewId, script: String, world: Option<String>) -> Receiver<Evaluation> {
        match &self.engine {
            Engine::Local(local) => local.evaluate(view, script, world),
            Engine::Worker(client) => client.evaluate(view, script, world),
        }
    }

    /// Closes every page, ends every web process and stops the engine, waiting (a bounded time) until it has; a worker
    /// is then reaped. The engine cannot start again in this process; commands after it are dropped. Once is enough:
    /// a second call (or the drop after it) returns at once.
    pub fn shutdown(&self) {
        match &self.engine {
            Engine::Local(local) => local.stop(),
            Engine::Worker(client) => client.shutdown(),
        }
    }
}

teams_wpe_client::host_commands!(Host, next_id);

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}
