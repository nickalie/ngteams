//! `ui-sandbox-run`: one app in a UI sandbox of its own, for as long as it runs, driven from outside through a control
//! socket. `scripts/run-personal-read-only.sh` runs the personal build's read-only mode with it.
//!
//! - `ui-sandbox-run serve --dir <run> --broker <program> [--broker-arg A]... --app <program> [--app-arg A]...`:
//!   starts a sandbox (no PipeWire; `--pipewire` gives it its private one, with its tone, recorded speaker and test
//!   pattern camera, for a lab app's calls), a socket pair, the broker **outside** the sandbox (this process's environment,
//!   the user's bus included, so it reaches the Secret Service) with one end, and the app **inside** it
//!   (`ui_sandbox::clean_command`: the sandbox's compositor and bus only) with the other end and a view trace file
//!   (`<run>/trace.jsonl`). In the arguments, `{broker-fd}`, `{secrets-fd}` and `{trace-fd}` are replaced by those
//!   descriptors' numbers; each process gets only its own (`ui_sandbox::spawn::pass_only`), and this one keeps no copy
//!   of either end. The app's output goes to `<run>/app.log`, the broker's to `<run>/broker.log`, its own id to
//!   `<run>/holder.pid`. It listens on
//!   `<run>/control.sock` and runs until the app exits, `stop` is asked, or it gets SIGTERM or SIGINT; then it stops
//!   the app, waits for the broker (its socket closed) and stops the sandbox.
//! - `ui-sandbox-run <command> --dir <run> [args]`: one command to a running `serve`, its answer printed (`ok …` or
//!   `error …`, the exit status saying which): `shot <name>` (the window, to `<run>/shots/<name>.png`), `screen <name>`
//!   (the whole output), `key <spec>` (`ctrl+k`, `Down`, `Enter`, `a`: `ui_sandbox::input::parse_key`), `type <text>`,
//!   `click <x> <y>` (output pixels), `scroll <x> <y> <notches>` (the wheel there, positive down),
//!   `clipboard <mime> <path> [<mime> <path> ...]` (the files, read now, on the sandbox's clipboard together, each
//!   under its MIME type: `ui_sandbox::clipboard`; paths absolute, without whitespace), `windows` (niri's list),
//!   `stop`.

use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ui_sandbox::clipboard;
use ui_sandbox::input::{VirtualInput, keys, parse_key};
use ui_sandbox::pipewire::PipeWireOptions;
use ui_sandbox::{NIRI_FULL_WINDOW, Sandbox, SandboxOptions, SandboxResult, Tools, clean_command, spawn};

const USAGE: &str = "usage: ui-sandbox-run serve --dir <run> [--pipewire] --broker <program> [--broker-arg A]... --app \
                     <program> [--app-arg A]...\n       ui-sandbox-run shot|screen <name> | key <spec> | type <text> | click \
                     <x> <y> | scroll <x> <y> <notches> | clipboard <mime> <path> [<mime> <path>]... | windows | \
                     stop  --dir <run>";

/// How long the app gets to quit on SIGTERM before it is killed, and the broker to see its socket close.
const GRACE: Duration = Duration::from_secs(5);
/// How long a command waits for `serve`'s answer.
const ANSWER_WAIT: Duration = Duration::from_secs(30);

/// Set by SIGTERM or SIGINT.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_asked(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub(super) fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let result = if command == "serve" {
        Serve::parse(rest).and_then(Serve::run)
    } else {
        ask(command, rest)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ui-sandbox-run: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `serve`'s arguments.
#[derive(Debug, Default)]
struct Serve {
    dir: PathBuf,
    broker: PathBuf,
    broker_args: Vec<String>,
    app: PathBuf,
    app_args: Vec<String>,
    /// The sandbox's private PipeWire, for calls.
    pipewire: bool,
}

impl Serve {
    fn parse(args: &[String]) -> SandboxResult<Self> {
        let mut serve = Self::default();
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            if flag == "--pipewire" {
                serve.pipewire = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?
                .clone();
            match flag.as_str() {
                "--dir" => serve.dir = PathBuf::from(value),
                "--broker" => serve.broker = PathBuf::from(value),
                "--broker-arg" => serve.broker_args.push(value),
                "--app" => serve.app = PathBuf::from(value),
                "--app-arg" => serve.app_args.push(value),
                other => return Err(format!("unknown argument {other}\n{USAGE}").into()),
            }
        }
        if [&serve.dir, &serve.broker, &serve.app]
            .iter()
            .any(|path| path.as_os_str().is_empty())
        {
            return Err(USAGE.into());
        }
        Ok(serve)
    }

    fn run(self) -> SandboxResult {
        let handler = stop_asked as extern "C" fn(libc::c_int) as *const () as libc::sighandler_t;
        for signal in [libc::SIGTERM, libc::SIGINT] {
            // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
            unsafe {
                libc::signal(signal, handler);
            }
        }
        fs::create_dir_all(self.dir.join("shots"))?;
        // its own process id, for a script to stop it by (`setsid` may fork before it runs)
        fs::write(self.dir.join("holder.pid"), format!("{}\n", std::process::id()))?;
        // the control socket first: nothing is started when it can't be had (its path too long, say)
        let control = self.dir.join("control.sock");
        let _ = fs::remove_file(&control);
        let listener = UnixListener::bind(&control)?;
        listener.set_nonblocking(true)?;
        let tools = Tools::find()?;
        let sandbox = Sandbox::start(
            &tools,
            &SandboxOptions {
                niri_extra: NIRI_FULL_WINDOW,
                pipewire: self.pipewire.then(PipeWireOptions::default),
                ..SandboxOptions::default()
            },
        )?;
        let (app_end, broker_end) = UnixStream::pair()?;
        let trace = File::create(self.dir.join("trace.jsonl"))?;
        let fds = [
            ("{broker-fd}", broker_end.as_raw_fd()),
            ("{secrets-fd}", app_end.as_raw_fd()),
            ("{trace-fd}", trace.as_raw_fd()),
        ];
        let filled = |args: &[String]| -> Vec<String> {
            args.iter()
                .map(|arg| {
                    fds.iter()
                        .fold(arg.clone(), |arg, (name, fd)| arg.replace(name, &fd.to_string()))
                })
                .collect()
        };
        // the broker outside the sandbox, with this process's environment (the user's bus)
        let mut broker = Command::new(&self.broker);
        broker
            .args(filled(&self.broker_args))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(self.dir.join("broker.log"))?);
        spawn::pass_only(&mut broker, &[broker_end.as_fd()]);
        let mut broker = broker.spawn()?;
        drop(broker_end);
        // the app inside it
        let log = File::create(self.dir.join("app.log"))?;
        let mut app = clean_command(&self.app);
        app.args(filled(&self.app_args))
            .envs(sandbox.client_env())
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        spawn::pass_only(&mut app, &[app_end.as_fd(), trace.as_fd()]);
        let app = app.spawn();
        drop((app_end, trace));
        let mut app = match app {
            Ok(app) => app,
            Err(error) => {
                stop_child(&mut broker);
                return Err(error.into());
            }
        };
        println!("ready sandbox={} run={}", sandbox.dir().display(), self.dir.display());
        let mut controls = Controls {
            sandbox: &sandbox,
            dir: &self.dir,
            input: None,
        };
        let ended = controls.serve(&listener, &mut app);
        drop(controls);
        stop_child(&mut app);
        // the broker exits once the app's end is closed
        if wait_for(&mut broker, GRACE).is_none() {
            stop_child(&mut broker);
        }
        let _ = fs::remove_file(&control);
        println!("stopped: {ended}");
        Ok(())
    }
}

/// What the control socket drives.
struct Controls<'a> {
    sandbox: &'a Sandbox,
    dir: &'a Path,
    input: Option<VirtualInput>,
}

impl Controls<'_> {
    /// Answers commands until the app exits, `stop` is asked or a signal comes; says which.
    fn serve(&mut self, listener: &UnixListener, app: &mut Child) -> String {
        loop {
            if STOP.load(Ordering::SeqCst) {
                return "a signal".to_owned();
            }
            if let Ok(Some(status)) = app.try_wait() {
                return format!("the app exited ({status})");
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    if self.answer(stream) {
                        return "stop asked".to_owned();
                    }
                }
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// One command on `stream`, answered; whether it was `stop`.
    fn answer(&mut self, stream: UnixStream) -> bool {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(ANSWER_WAIT));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return false;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let stop = command == "stop";
        let answer = if stop {
            Ok("stopping".to_owned())
        } else {
            self.run(command, rest)
        };
        let text = match answer {
            Ok(detail) => format!("ok {detail}\n"),
            Err(error) => format!("error {}\n", error.to_string().replace('\n', " ")),
        };
        let mut stream = stream;
        let _ = stream.write_all(text.as_bytes());
        stop
    }

    fn run(&mut self, command: &str, rest: &str) -> SandboxResult<String> {
        match command {
            "shot" | "screen" => self.shot(rest, command == "shot"),
            "key" => {
                let input = self.input()?;
                let (modifiers, key) =
                    parse_key(rest, input.keymap()).ok_or_else(|| format!("no such key: {rest:?}"))?;
                input.key_with(&modifiers, key)?;
                Ok(String::new())
            }
            "type" => {
                self.input()?.type_text(rest)?;
                Ok(String::new())
            }
            "scroll" => {
                let mut numbers = rest.split_whitespace();
                let (Some(Ok(x)), Some(Ok(y)), Some(Ok(steps)), None) = (
                    numbers.next().map(str::parse::<u32>),
                    numbers.next().map(str::parse::<u32>),
                    numbers.next().map(str::parse::<i32>),
                    numbers.next(),
                ) else {
                    return Err("scroll <x> <y> <notches>".into());
                };
                self.input()?.scroll(x, y, steps)?;
                Ok(String::new())
            }
            "click" => {
                let mut numbers = rest.split_whitespace().map(str::parse::<u32>);
                let (Some(Ok(x)), Some(Ok(y)), None) = (numbers.next(), numbers.next(), numbers.next()) else {
                    return Err("click <x> <y>".into());
                };
                self.input()?.click(x, y, keys::BTN_LEFT)?;
                Ok(String::new())
            }
            "clipboard" => {
                let words: Vec<&str> = rest.split_whitespace().collect();
                let offers = clipboard::offers_from_files(&words)?;
                let count = offers.len();
                self.sandbox.set_clipboard(offers)?;
                Ok(format!("{count} types"))
            }
            "windows" => Ok(self.sandbox.niri_msg(&["windows"])?.replace('\n', " | ")),
            other => Err(format!("unknown command {other:?}").into()),
        }
    }

    fn input(&mut self) -> SandboxResult<&mut VirtualInput> {
        if self.input.is_none() {
            self.input = Some(self.sandbox.input()?);
        }
        self.input.as_mut().ok_or_else(|| "no input".into())
    }

    /// A screenshot named `name` (letters, digits, `-` and `_`), of the focused window or the whole output.
    fn shot(&self, name: &str, window: bool) -> SandboxResult<String> {
        let safe = !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_');
        if !safe {
            return Err("a shot's name is letters, digits, - and _".into());
        }
        let taken = self.sandbox.screenshot(&format!("{name}.png"), window)?;
        let kept = self.dir.join("shots").join(format!("{name}.png"));
        fs::copy(&taken, &kept)?;
        let _ = fs::remove_file(&taken);
        Ok(kept.display().to_string())
    }
}

/// `child` stopped: SIGTERM, then killed after [`GRACE`].
fn stop_child(child: &mut Child) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `kill` only sends a signal to the process of our own child, which hasn't been reaped yet.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    if wait_for(child, GRACE).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn wait_for(child: &mut Child, deadline: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

/// One command (`command` and its arguments, `--dir <run>` among them) to the `serve` of that run, its answer printed.
fn ask(command: &str, args: &[String]) -> SandboxResult {
    let mut dir = None;
    let mut words = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--dir" {
            dir = args.next().map(PathBuf::from);
        } else {
            words.push(arg.as_str());
        }
    }
    let dir = dir.ok_or_else(|| format!("--dir <run> is needed\n{USAGE}"))?;
    let mut stream = UnixStream::connect(dir.join("control.sock"))
        .map_err(|error| format!("no ui-sandbox-run serve at {}: {error}", dir.display()))?;
    stream.set_read_timeout(Some(ANSWER_WAIT))?;
    let line = std::iter::once(command).chain(words).collect::<Vec<_>>().join(" ");
    if line.contains('\n') {
        return Err("a command is one line".into());
    }
    stream.write_all(format!("{line}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer)?;
    let answer = answer.trim_end();
    println!("{answer}");
    if answer.starts_with("ok") {
        Ok(())
    } else {
        Err(format!("{command} failed").into())
    }
}
