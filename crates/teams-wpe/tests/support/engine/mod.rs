//! No tenant, desktop data or network origin is used by this executable.

mod page;
mod pump;
mod scenarios;

use std::path::Path;
use std::process::ExitStatus;
use std::time::Duration;

use teams_wpe::{
    Backend, BufferFormat, DmaBufTarget, FrameMode, HostConfig, MODIFIER_INVALID, MODIFIER_LINEAR, fourcc,
};
use ui_sandbox::{Running, SandboxOptions};

pub const CHILD: &str = "TEAMS_WPE_ENGINE_TEST_CHILD";
const ROOT: &str = "TEAMS_WPE_ENGINE_TEST_ROOT";
/// CPU-only CI still exercises both real native SHM hosts; only kernel DMA-buffer export requires hardware.
const SKIP_DMA: &str = "TEAMS_WPE_TEST_SKIP_DMA";
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn entry() -> Result {
    match std::env::var(CHILD).as_deref() {
        Ok("worker") => child(Backend::Worker, FrameMode::Shm),
        Ok("local") => child(Backend::InProcess, FrameMode::Shm),
        Ok("dmabuf") => {
            let target = DmaBufTarget {
                render_node: "/dev/dri/renderD128".into(),
                formats: [b"XR24", b"AR24"]
                    .map(|name| BufferFormat {
                        fourcc: fourcc(name),
                        modifiers: vec![MODIFIER_LINEAR, MODIFIER_INVALID],
                    })
                    .to_vec(),
            };
            child(Backend::Worker, FrameMode::DmaBuf(target))
        }
        Ok(other) => Err(format!("unknown child mode {other}").into()),
        Err(_) => parent(),
    }
}

fn parent() -> Result {
    let skip_dma = match std::env::var(SKIP_DMA).as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("0") => false,
        Ok("1") => true,
        _ => return Err(format!("{SKIP_DMA} must be unset, 0 or 1").into()),
    };
    let Some(sandbox) = ui_sandbox::sandbox_or_skip("native WPE engine", &SandboxOptions::default())? else {
        return Ok(());
    };
    let modes: &[&str] = if skip_dma {
        &["worker", "local"]
    } else {
        &["worker", "local", "dmabuf"]
    };
    for &mode in modes {
        let root = tempfile::tempdir()?;
        let mut command = ui_sandbox::clean_command(std::env::current_exe()?);
        command
            .envs(sandbox.client_env())
            .env(CHILD, mode)
            .env(ROOT, root.path());
        disposable_home(&mut command, root.path())?;
        let mut running = Running::new(
            command
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?,
        )?;
        let complete = running.line_where(Duration::from_secs(180), |line| {
            line == format!("engine {mode}: passed")
        });
        if let Err(error) = complete {
            return Err(running.fail(&format!("mode {mode} did not finish: {error:?}")));
        }
        let (status, stderr) = running.finish(Duration::from_secs(20))?;
        succeeded(status, &stderr)?;
    }
    println!("native WPE hosts: {} passed", modes.join(", "));
    if skip_dma {
        println!("native WPE dmabuf: skipped because {SKIP_DMA}=1 (CPU-only test environment)");
    }
    Ok(())
}

fn disposable_home(command: &mut std::process::Command, root: &Path) -> Result {
    for (name, dir) in [
        ("HOME", "home"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let path = ui_sandbox::private_dir(root.join(dir))?;
        command.env(name, path);
    }
    Ok(())
}

fn succeeded(status: ExitStatus, stderr: &str) -> Result {
    if !status.success() {
        return Err(format!("native child exited {status}: {stderr}").into());
    }
    Ok(())
}

fn child(backend: Backend, frames: FrameMode) -> Result {
    ui_sandbox::window_allowed()?;
    let root = std::path::PathBuf::from(std::env::var_os(ROOT).ok_or("no disposable test root")?);
    let expects_dma = matches!(frames, FrameMode::DmaBuf(_));
    let config = HostConfig {
        frames,
        schemes: vec!["synthetic".into()],
        extensions: None,
    };
    let mut engine = pump::Engine::start(config.clone(), backend, root)?;
    if backend == Backend::InProcess {
        scenarios::refused_restart(config.clone());
    }
    scenarios::run(&mut engine)?;
    assert_eq!(engine.frames.values().any(|frame| frame.dma), expects_dma);
    assert!(engine.frames.values().any(|frame| frame.shm));
    let spinning = if backend == Backend::InProcess {
        Some(scenarios::spinning(&mut engine)?)
    } else {
        None
    };
    engine.shutdown();
    if let Some(pending) = spinning {
        assert!(
            matches!(pending.try_recv(), Err(async_channel::TryRecvError::Closed)),
            "an evaluation pending at the shutdown is left open"
        );
    }
    if backend == Backend::Worker {
        let config = scenarios::with_extensions(config, &engine.root)?;
        let mut next = pump::Engine::start(config, backend, engine.root.clone())?;
        scenarios::restart(&mut next)?;
        next.shutdown();
    }
    println!("engine {}: passed", std::env::var(CHILD)?);
    Ok(())
}
