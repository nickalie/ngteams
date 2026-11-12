//! Which installation this process runs from, and whether the app may replace it.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{Error, Format, IDENTITY, MARKER, Version, error};

/// What [`Installation::detect`] reads: the process's executable and its AppImage environment, where a package's
/// download waits, and the user's directories an update must never replace.
#[derive(Clone, Debug)]
pub struct Places {
    /// This process's executable (`std::env::current_exe`).
    pub executable: PathBuf,
    /// `$APPIMAGE`: the AppImage file the AppImage runtime started.
    pub appimage: Option<OsString>,
    /// `$APPDIR`: where that runtime mounted it.
    pub appdir: Option<OsString>,
    /// Where a system package's download waits for the package manager (its own folder isn't the user's to write), and
    /// the Windows installer's for the app to quit and run it.
    pub stage_dir: PathBuf,
    /// The app's data and config directories: an update that would replace one of them is refused.
    pub protected: Vec<PathBuf>,
}

impl Places {
    /// This process's places, with `stage_dir` and the `protected` directories of the caller.
    ///
    /// # Errors
    /// The executable's path can't be read.
    pub fn from_process(stage_dir: PathBuf, protected: Vec<PathBuf>) -> std::io::Result<Self> {
        Ok(Self {
            executable: std::env::current_exe()?,
            appimage: std::env::var_os("APPIMAGE"),
            appdir: std::env::var_os("APPDIR"),
            stage_dir,
            protected,
        })
    }
}

/// A release installation the app can replace with a newer release ([`crate::Source::prepare`]).
#[derive(Clone, Debug)]
pub struct Installation {
    /// What an update replaces: the archive's folder, the AppImage file, a package's `/opt/ngteams`, or the Windows
    /// installer's folder (holding `bin\ngteams.exe`).
    pub(crate) root: PathBuf,
    /// What a restart runs once the update is in.
    pub(crate) executable: PathBuf,
    pub(crate) format: Format,
    pub(crate) stage_dir: PathBuf,
    pub(crate) protected: Vec<PathBuf>,
    /// The password prompt that runs the package manager: `pkexec`, a test's own in tests.
    #[cfg(unix)]
    pub(crate) prompter: PathBuf,
}

const NOT_A_RELEASE: &str = "This copy of ngteams wasn't installed from a release package, so it can't update itself. \
Update it the way it was installed.";

impl Installation {
    /// The installation `places` describe, when a release package made it (its [`MARKER`]).
    ///
    /// # Errors
    /// Why this copy can't update itself: no marker (a source build), a moved or renamed archive folder, a user
    /// directory inside it, or a path that can't be read.
    #[cfg(unix)]
    pub fn detect(places: &Places) -> Result<Self, Error> {
        let executable = fs::canonicalize(executable_on_disk(places.executable.clone()))?;
        if let (Some(image), Some(appdir)) = (&places.appimage, &places.appdir)
            && executable.starts_with(Path::new(appdir))
        {
            let root = fs::canonicalize(image)?;
            if !root.is_file() {
                return Err(error("The AppImage this copy runs from isn't a file any more."));
            }
            return Self::made(places, root.clone(), root, Format::AppImage);
        }
        #[cfg(target_os = "macos")]
        if let Some(bundle) = app_bundle(&executable) {
            fs::read_to_string(bundle.join(MARKER))
                .ok()
                .and_then(|marker| Format::from_marker(&marker))
                .filter(|format| *format == Format::MacApp)
                .ok_or_else(|| error(NOT_A_RELEASE))?;
            return Self::made(places, bundle, executable, Format::MacApp);
        }
        let prefix = executable
            .parent()
            .filter(|bin| bin.ends_with("usr/bin"))
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or_else(|| error(NOT_A_RELEASE))?;
        let format = fs::read_to_string(prefix.join(MARKER))
            .ok()
            .and_then(|marker| Format::from_marker(&marker))
            .ok_or_else(|| error(NOT_A_RELEASE))?;
        if format == Format::Archive && prefix.file_name().is_none_or(|name| name != crate::FOLDER) {
            return Err(error(
                "To update itself, the unpacked release has to stay in its own folder named ngteams. Rename its \
                 folder back, or install the update by hand.",
            ));
        }
        let root = prefix.to_path_buf();
        Self::made(places, root, executable, format)
    }

    /// The installation `places` describe, when the Windows installer made it: `bin\ngteams.exe` in a folder whose
    /// [`MARKER`] says so.
    ///
    /// # Errors
    /// Why this copy can't update itself: no marker (a zip or source build), a user directory inside it, or a path
    /// that can't be read.
    #[cfg(windows)]
    pub fn detect(places: &Places) -> Result<Self, Error> {
        let executable = plain(fs::canonicalize(&places.executable)?);
        let root = executable
            .parent()
            .filter(|bin| bin.file_name().is_some_and(|name| name.eq_ignore_ascii_case("bin")))
            .and_then(Path::parent)
            .ok_or_else(|| error(NOT_A_RELEASE))?;
        let format = fs::read_to_string(root.join(MARKER))
            .ok()
            .and_then(|marker| Format::from_marker(&marker))
            .filter(|format| *format == Format::WindowsSetup)
            .ok_or_else(|| error(NOT_A_RELEASE))?;
        let root = root.to_path_buf();
        Self::made(places, root, executable, format)
    }

    fn made(places: &Places, root: PathBuf, executable: PathBuf, format: Format) -> Result<Self, Error> {
        let installation = Self {
            root,
            executable,
            format,
            stage_dir: places.stage_dir.clone(),
            protected: places.protected.clone(),
            #[cfg(unix)]
            prompter: PathBuf::from("pkexec"),
        };
        installation.validate()?;
        Ok(installation)
    }

    /// The installation with `prompter` in `pkexec`'s place, for tests of the package route.
    #[cfg(all(unix, any(test, feature = "test-support")))]
    #[must_use]
    pub fn with_prompter(mut self, prompter: PathBuf) -> Self {
        self.prompter = prompter;
        self
    }

    /// How this copy was installed.
    #[must_use]
    pub fn format(&self) -> Format {
        self.format
    }

    /// What an update replaces.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What a restart runs.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The release version the installed files are, when the installation says (an AppImage doesn't).
    pub(crate) fn installed_version(&self) -> Option<Version> {
        if self.format == Format::AppImage {
            return None;
        }
        let identity = fs::read_to_string(self.root.join(IDENTITY)).ok()?;
        Version::parse(identity.lines().next()?.trim())
    }

    /// What tells this installation's current contents apart: its release identity file, or the AppImage file's
    /// device, inode, size and modification time. An update compares it before replacing anything.
    pub(crate) fn identity(&self) -> std::io::Result<Vec<u8>> {
        #[cfg(unix)]
        if self.format == Format::AppImage {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = fs::metadata(&self.root)?;
            return Ok(format!(
                "{}:{}:{}:{}.{}",
                metadata.dev(),
                metadata.ino(),
                metadata.size(),
                metadata.mtime(),
                metadata.mtime_nsec()
            )
            .into_bytes());
        }
        fs::read(self.root.join(IDENTITY))
    }

    /// Where an update's stage goes: beside what it replaces (one filesystem, for the renames), or for a system
    /// package or the Windows installer the app's own directory.
    pub(crate) fn stage_parent(&self) -> PathBuf {
        if self.format.stages_in_app_folder() {
            return self.stage_dir.clone();
        }
        self.root.parent().map_or_else(|| self.root.clone(), Path::to_path_buf)
    }

    /// Refuses an installation that is gone, has changed its kind, or holds one of the user's directories.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        let metadata = fs::metadata(&self.root)?;
        let expected = if self.format == Format::AppImage {
            metadata.is_file()
        } else {
            metadata.is_dir()
        };
        if !expected {
            return Err(error(
                "The installation isn't where it was, so the update can't replace it.",
            ));
        }
        if self.protected.iter().any(|path| self.contains(path)) {
            return Err(error(
                "Your ngteams data or settings are inside the installation's folder, so replacing the folder would \
                 remove them. Move them out of it, or install the update by hand.",
            ));
        }
        Ok(())
    }

    /// Whether `path` is inside the installation; one whose location can't be resolved counts as inside.
    fn contains(&self, path: &Path) -> bool {
        match (resolved(path), resolved(&self.root)) {
            (Ok(path), Ok(root)) => path.starts_with(root),
            _ => true,
        }
    }

    /// Removes the stages an earlier run left (see [`crate::stage::cleanup`]). Run it once at startup, before any
    /// download of this process.
    #[must_use]
    pub fn cleanup_stale(&self) -> usize {
        crate::stage::cleanup(&self.stage_parent())
    }
}

/// `path` with its symbolic links resolved; a part that doesn't exist yet resolves through its nearest existing parent.
fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let absolute = std::path::absolute(path)?;
            let name = absolute.file_name().ok_or(error)?;
            let parent = absolute
                .parent()
                .ok_or_else(|| std::io::Error::other("the path has no parent"))?;
            Ok(resolved(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

/// `path` without the `\\?\` prefix `canonicalize` gives on Windows, when it has a plain drive form: the installer
/// is handed this folder, and it is shown to the user.
#[cfg(windows)]
fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') && !rest.contains('?') => PathBuf::from(rest),
        _ => path,
    }
}

/// The app bundle `executable` runs from (`<name>.app/Contents/MacOS/<program>`), if it does.
#[cfg(target_os = "macos")]
fn app_bundle(executable: &Path) -> Option<PathBuf> {
    executable
        .parent()
        .filter(|programs| programs.ends_with("Contents/MacOS"))
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|bundle| bundle.extension().is_some_and(|extension| extension == "app"))
        .map(Path::to_path_buf)
}

/// The executable's path as it is on disk: a package upgrade under a running app leaves `/proc/self/exe` saying
/// ` (deleted)`.
#[cfg(unix)]
fn executable_on_disk(executable: PathBuf) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    if !executable.exists()
        && let Some(path) = executable.as_os_str().as_bytes().strip_suffix(b" (deleted)")
    {
        return PathBuf::from(std::ffi::OsStr::from_bytes(path));
    }
    executable
}
