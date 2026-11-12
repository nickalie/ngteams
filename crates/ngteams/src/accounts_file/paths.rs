//! Native account locations and directory overlap resolution for ngteams.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The personal build's directory name under `$XDG_DATA_HOME` and `$XDG_CONFIG_HOME`.
pub const PERSONAL_DIR: &str = "ngteams";

/// Where the personal build keeps its data and its config by default: `$XDG_DATA_HOME/ngteams`
/// (`~/.local/share/ngteams`) and `$XDG_CONFIG_HOME/ngteams` (`~/.config/ngteams`), by the XDG rules
/// (an empty or relative XDG directory is ignored in favour of the
/// `HOME` default).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalLocations {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl PersonalLocations {
    /// From `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `HOME`; `None` when a fallback to `HOME` is needed and it is unset
    /// or relative.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        #[cfg(windows)]
        {
            Self::windows(std::env::var_os("LOCALAPPDATA"), std::env::var_os("USERPROFILE"))
        }
        #[cfg(target_os = "macos")]
        {
            Self::macos(std::env::var_os("HOME"))
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        Self::resolve(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// Windows keeps both private account directories under Local `AppData`, never roaming the store or cookies.
    /// Absolute Local `AppData` wins; an absolute user profile supplies the conventional fallback.
    #[must_use]
    pub fn windows(local_app_data: Option<OsString>, profile: Option<OsString>) -> Option<Self> {
        let base = local_app_data
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                profile
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join("AppData/Local"))
            })?
            .join(PERSONAL_DIR);
        Some(Self {
            config_dir: base.join("config"),
            data_dir: base.join("data"),
        })
    }

    /// macOS keeps both under `~/Library/Application Support/ngteams`, as Windows does under Local `AppData`: the
    /// store, cookies and logs are this Mac's, never synced. `None` when `HOME` is unset or relative.
    #[must_use]
    pub fn macos(home: Option<OsString>) -> Option<Self> {
        let base = home
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())?
            .join("Library/Application Support")
            .join(PERSONAL_DIR);
        Some(Self {
            config_dir: base.join("config"),
            data_dir: base.join("data"),
        })
    }

    /// The XDG rules on explicit values.
    #[must_use]
    pub fn resolve(config_home: Option<OsString>, data_home: Option<OsString>, home: Option<OsString>) -> Option<Self> {
        let home = home.map(PathBuf::from).filter(|home| home.is_absolute());
        let base = |xdg: Option<OsString>, fallback: &str| {
            xdg.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
        };
        Some(Self {
            config_dir: base(config_home, ".config")?.join(PERSONAL_DIR),
            data_dir: base(data_home, ".local/share")?.join(PERSONAL_DIR),
        })
    }
}

/// Whether `one` and `other` are the same directory, or one lies inside the other, after resolving symlinks: each path
/// is resolved component by component as the filesystem would ([`resolved`]), so a `..` after a symlink leaves the
/// symlink's target, and a directory not made yet is compared where it would be made.
#[must_use]
pub fn overlapping(one: &Path, other: &Path) -> bool {
    let (Some(one), Some(other)) = (resolved(one), resolved(other)) else {
        // An unresolved drive-relative path or unreadable current directory cannot prove separation.
        return true;
    };
    contains(&one, &other) || contains(&other, &one)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn contains(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

/// macOS volumes are case-insensitive by default and a path not made yet keeps the spelling it was given, so names
/// are compared ignoring case: two spellings of one directory count as overlapping (the safe answer).
#[cfg(target_os = "macos")]
fn contains(path: &Path, parent: &Path) -> bool {
    let mut path = path.components();
    parent.components().all(|component| {
        path.next().is_some_and(|next| {
            next.as_os_str().to_string_lossy().to_lowercase() == component.as_os_str().to_string_lossy().to_lowercase()
        })
    })
}

#[cfg(windows)]
fn contains(path: &Path, parent: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
    let mut path = path.components();
    parent.components().all(|component| {
        let Some(next) = path.next() else {
            return false;
        };
        // Canonical paths use verbatim prefixes; absent paths may still have ordinary drive/UNC prefixes.
        let units = |value: Component<'_>| {
            let value = match value {
                Component::Prefix(prefix) => match prefix.kind() {
                    std::path::Prefix::VerbatimDisk(drive) | std::path::Prefix::Disk(drive) => {
                        return vec![u16::from(drive), u16::from(b':')];
                    }
                    std::path::Prefix::VerbatimUNC(server, share) | std::path::Prefix::UNC(server, share) => {
                        return server
                            .encode_wide()
                            .chain([u16::from(b'\\')])
                            .chain(share.encode_wide())
                            .collect();
                    }
                    _ => prefix.as_os_str(),
                },
                _ => value.as_os_str(),
            };
            let mut units: Vec<u16> = value.encode_wide().collect();
            // Win32 normally strips these suffixes. Conservatively treat verbatim spellings as aliases too.
            while matches!(units.last(), Some(32 | 46)) {
                units.pop();
            }
            units
        };
        let (left, right) = (units(next), units(component));
        let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len())) else {
            return false;
        };
        // SAFETY: both UTF-16 buffers stay alive; lengths exclude any terminator, including unpaired surrogates.
        unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) == CSTR_EQUAL }
    })
}

/// `path` resolved one component at a time: while the path so far exists it is canonicalized after each component
/// (its symlinks followed, as the filesystem does before it takes the next `..`); past the last existing component the
/// rest is appended with `.` dropped and `..` taking one component off.
fn resolved(path: &Path) -> Option<PathBuf> {
    // Seed relative paths before interpreting `..`: popping an empty buffer would resolve against the wrong dir.
    #[cfg(windows)]
    if !path.is_absolute() && matches!(path.components().next(), Some(Component::Prefix(_))) {
        return None;
    }
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().ok()?
    };
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => {
                out.push(other.as_os_str());
                if let Ok(canonical) = out.canonicalize() {
                    out = canonical;
                }
            }
        }
    }
    Some(out)
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_personal_dirs_follow_the_xdg_rules() {
        let locations =
            PersonalLocations::resolve(None, Some("relative".into()), Some("/home/someone".into())).unwrap();
        assert_eq!(locations.data_dir, Path::new("/home/someone/.local/share/ngteams"));
        assert_eq!(locations.config_dir, Path::new("/home/someone/.config/ngteams"));
        let locations = PersonalLocations::resolve(Some("/c".into()), Some("/d".into()), None).unwrap();
        assert_eq!(locations.data_dir, Path::new("/d/ngteams"));
        assert_eq!(locations.config_dir, Path::new("/c/ngteams"));
        assert!(PersonalLocations::resolve(None, None, Some("relative".into())).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_dir_overlaps_itself_its_parents_and_its_children_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(overlapping(&real, &real));
        assert!(overlapping(&real.join(PERSONAL_DIR), &real));
        assert!(overlapping(&real, &real.join("not/yet")));
        assert!(overlapping(&link.join(PERSONAL_DIR), &real.join(PERSONAL_DIR)));
        assert!(overlapping(&real.join("a/../teams"), &link.join("teams/store")));
        assert!(!overlapping(&real.join("teams-lab"), &real.join(PERSONAL_DIR)));
        assert!(!overlapping(&dir.path().join("other"), &real));
    }

    #[cfg(unix)]
    #[test]
    fn a_parent_after_a_symlink_leaves_the_symlinks_target() {
        let dir = tempfile::tempdir().unwrap();
        let personal = dir.path().join("data/teams");
        std::fs::create_dir_all(personal.join("sub")).unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let link = scratch.join("link");
        std::os::unix::fs::symlink(personal.join("sub"), &link).unwrap();
        // `scratch/link/..` is `data/teams` on the filesystem, not `scratch`
        assert!(overlapping(&link.join("../lab"), &personal));
        assert!(!overlapping(&scratch.join("lab"), &personal));
        assert!(overlapping(&link.join("../not/yet/../lab"), &personal));
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::ffi::OsStringExt as _;
    #[test]
    fn local_account_directories_do_not_roam_or_use_xdg() {
        let locations = PersonalLocations::windows(Some(r"C:\Local".into()), None).unwrap();
        assert_eq!(locations.config_dir, Path::new(r"C:\Local\ngteams\config"));
        assert_eq!(locations.data_dir, Path::new(r"C:\Local\ngteams\data"));
        let locations =
            PersonalLocations::windows(Some("relative".into()), Some(r"C:\Users\synthetic".into())).unwrap();
        assert_eq!(
            locations.data_dir,
            Path::new(r"C:\Users\synthetic\AppData\Local\ngteams\data")
        );
        assert!(PersonalLocations::windows(None, Some("relative".into())).is_none());
        assert!(PersonalLocations::windows(Some("".into()), None).is_none());
    }
    #[test]
    fn case_and_verbatim_spellings_cannot_bypass_snapshot_separation() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("Personal");
        std::fs::create_dir(&real).unwrap();
        assert!(overlapping(&real, &real.join("not/yet")));
        assert!(overlapping(&real.join("MISSING"), &real.join("missing/sub")));
        assert!(overlapping(&real.join("missing. "), &real.join("MISSING/sub")));
        assert!(overlapping(
            &real.canonicalize().unwrap().join("MISSING"),
            &real.join("missing")
        ));
        assert!(overlapping(
            Path::new(r"C:\ABSENT\Personal"),
            Path::new(r"\\?\C:\absent\personal\sub")
        ));
        // Check UNC spelling normalization without asking the filesystem to open a network share.
        assert!(contains(
            Path::new(r"\\?\UNC\SERVER\SHARE\absent\sub"),
            Path::new(r"\\server\share\ABSENT")
        ));
        assert!(!overlapping(&real, &dir.path().join("Personal-lab")));
        assert!(!overlapping(Path::new(r"C:\personal"), Path::new(r"D:\personal")));
        assert!(overlapping(Path::new(r"C:relative"), &real));
        let unusual = dir.path().join(OsString::from_wide(&[0xd800, 120]));
        std::fs::create_dir(&unusual).unwrap();
        assert!(overlapping(&unusual, &unusual.canonicalize().unwrap().join("child")));
    }
}

#[cfg(test)]
mod relative_tests {
    use super::*;
    #[test]
    fn a_relative_parent_and_missing_relative_components_resolve_against_the_current_directory() {
        let here = std::env::current_dir().unwrap();
        assert!(overlapping(Path::new(".."), here.parent().unwrap()));
        let missing = "ngteams-synthetic-nonexistent-path/sub";
        assert!(overlapping(Path::new(missing), &here.join(missing)));
        assert!(overlapping(
            &Path::new("..").join(here.file_name().unwrap()).join(missing),
            &here.join(missing)
        ));
    }
    #[test]
    fn explicit_native_locations_follow_the_supplied_local_data_root() {
        let directory = tempfile::tempdir().unwrap();
        let locations = PersonalLocations::windows(Some(directory.path().into()), None).unwrap();
        assert_eq!(locations.data_dir, directory.path().join("ngteams/data"));
        let locations = PersonalLocations::windows(None, Some(directory.path().into())).unwrap();
        assert_eq!(
            locations.config_dir,
            directory.path().join("AppData/Local/ngteams/config")
        );
        assert!(PersonalLocations::windows(None, None).is_none());
    }
}
