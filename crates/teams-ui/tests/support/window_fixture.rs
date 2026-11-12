//! Real window fixtures: private Linux compositor or the explicitly authorized Windows VM console.
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::Duration;

pub type SandboxResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
#[cfg(target_os = "linux")]
pub use ui_sandbox::Running;
#[cfg(windows)]
#[path = "window_running.rs"]
mod process;
#[cfg(windows)]
pub use process::Running;

pub fn application() -> gpui::Application {
    #[cfg(target_os = "linux")]
    {
        gpui_platform::linux(gpui::WindowingModes::WAYLAND)
    }
    #[cfg(windows)]
    {
        gpui_platform::application()
    }
}

pub fn window_allowed() -> SandboxResult {
    #[cfg(target_os = "linux")]
    ui_sandbox::window_allowed()?;
    #[cfg(windows)]
    teams_platform::windows_sandbox::window_allowed_here()?;
    Ok(())
}

/// Activate each synthetic child before it opens menus: a later title-bar click would dismiss them. Scheduled
/// console tasks do not necessarily give a newly created window foreground permission. Use GPUI's native window
/// activation, then wait for the actual foreground HWND rather than its thread-local active-window state.
#[cfg(windows)]
pub fn scene_activated(window: &gpui::Window) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::sync::atomic::{AtomicBool, Ordering};
    static REQUESTED: AtomicBool = AtomicBool::new(false);
    if !REQUESTED.swap(true, Ordering::Relaxed) {
        window.activate_window();
    }
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return false;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return false;
    };
    // SAFETY: read-only query; the live GPUI window owns the compared HWND.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() }.addr()
        == handle.hwnd.get().cast_unsigned()
}

pub struct Sandbox {
    #[cfg(target_os = "linux")]
    inner: ui_sandbox::Sandbox,
    #[cfg(windows)]
    directory: PathBuf,
    #[cfg(windows)]
    child: std::cell::Cell<Option<u32>>,
}

pub fn sandbox_or_skip(name: &str) -> SandboxResult<Option<Sandbox>> {
    sized_sandbox_or_skip(name, None)
}

/// A sandbox whose window fills a screen of `screen` (width, height) px; the console's own size on Windows.
pub fn sized_sandbox_or_skip(name: &str, screen: Option<(u32, u32)>) -> SandboxResult<Option<Sandbox>> {
    #[cfg(windows)]
    let _ = screen;
    #[cfg(target_os = "linux")]
    {
        let mut options = ui_sandbox::SandboxOptions {
            niri_extra: ui_sandbox::NIRI_FULL_WINDOW,
            ..Default::default()
        };
        if let Some((width, height)) = screen {
            options.width = width;
            options.height = height;
            options.fill_output = true;
        }
        Ok(ui_sandbox::sandbox_or_skip(name, &options)?.map(|inner| Sandbox { inner }))
    }
    #[cfg(windows)]
    {
        if let Err(error) = window_allowed() {
            if std::env::var_os("GPUI_SMOKE_REQUIRE").is_some_and(|value| value == "1") {
                return Err(error);
            }
            eprintln!("{name}: requires the authorized win11 VM console ({error})");
            return Ok(None);
        }
        let directory = tempfile::Builder::new().prefix("ngteams-scene-").tempdir()?;
        teams_platform::private::restrict_dir(directory.path())?;
        Ok(Some(Sandbox {
            directory: directory.keep(),
            child: std::cell::Cell::new(None),
        }))
    }
}

impl Sandbox {
    pub fn logs(&self) -> String {
        #[cfg(target_os = "linux")]
        {
            self.inner.logs()
        }
        #[cfg(windows)]
        {
            format!("\nfixture evidence: {}", self.directory.as_path().display())
        }
    }
    #[cfg(target_os = "linux")]
    pub fn spawn(&self, executable: &Path, args: &[&str], env: &[(&str, &str)]) -> SandboxResult<std::process::Child> {
        self.inner.spawn(executable, args, env)
    }
    #[cfg(windows)]
    pub fn spawn(
        &self,
        executable: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> SandboxResult<teams_platform::command::Tree> {
        use std::os::windows::process::CommandExt as _;
        use std::process::{Command, Stdio};
        let mut command = Command::new(executable);
        command
            .args(args)
            .envs(env.iter().copied())
            .env("UI_SANDBOX_DIR", self.directory.as_path())
            .creation_flags(0x0800_0000)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = teams_platform::command::Tree::spawn(&mut command)?;
        self.child.set(Some(child.id()));
        Ok(child)
    }
    pub fn screenshot(&self, file: &str, with_pointer: bool) -> SandboxResult<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            self.inner.screenshot(file, with_pointer)
        }
        #[cfg(windows)]
        {
            let _ = with_pointer;
            let path = self.directory.as_path().join(file);
            self.native_action("Capture", Some(&path))?;
            Ok(path)
        }
    }
    pub fn close_window(&self) -> SandboxResult {
        #[cfg(target_os = "linux")]
        self.inner.niri_msg(&["action", "close-window"])?;
        #[cfg(windows)]
        self.native_action("Close", None)?;
        Ok(())
    }
    #[cfg(windows)]
    fn native_action(&self, action: &str, output: Option<&Path>) -> SandboxResult {
        use std::os::windows::process::CommandExt as _;
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/lib/windows-scene-fixture.ps1");
        let mut command = std::process::Command::new("pwsh.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-File"])
            .arg(script)
            .args([
                "-Action",
                action,
                "-ChildPid",
                &self.child.get().ok_or("no fixture child")?.to_string(),
            ])
            .arg("-Executable")
            .arg(std::env::current_exe()?);
        if let Some(output) = output {
            command.arg("-OutputFile").arg(output);
        }
        command
            .creation_flags(0x0800_0000)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = teams_platform::command::Tree::spawn(&mut command)?;
        let running = Running::new(child)?;
        let (status, stderr) = running.finish(Duration::from_secs(20))?;
        if !status.success() {
            return Err(format!("scene {action}: {status}: {stderr}").into());
        }
        Ok(())
    }
}
