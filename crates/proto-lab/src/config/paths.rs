//! Path and chat-id shapes used by the harness config.

use std::path::{Component, Path, PathBuf};

/// The personal data-directory name, distinct from any keyring application.
pub(super) const PERSONAL_DIR: &str = "ngteams";

/// Whether `one` and `other` are the same directory, or one lies inside the other, after resolving symlinks: each path
/// is resolved component by component as the filesystem would ([`resolved`]), so a `..` after a symlink leaves the
/// symlink's target, and a directory not made yet is compared where it would be made.
#[must_use]
pub(crate) fn overlapping(one: &Path, other: &Path) -> bool {
    let (one, other) = (resolved(one), resolved(other));
    // Windows and macOS volumes ignore case by default: two spellings of one directory overlap
    #[cfg(any(windows, target_os = "macos"))]
    let (one, other) = (
        PathBuf::from(one.to_string_lossy().to_lowercase()),
        PathBuf::from(other.to_string_lossy().to_lowercase()),
    );
    one.starts_with(&other) || other.starts_with(&one)
}

/// `path` resolved one component at a time: while the path so far exists it is canonicalized after each component
/// (its symlinks followed, as the filesystem does before it takes the next `..`); past the last existing component the
/// rest is appended with `.` dropped and `..` taking one component off.
fn resolved(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
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
    out
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::overlapping;

    #[test]
    fn a_parent_after_a_symlink_leaves_the_symlinks_target() {
        let dir = tempfile::tempdir().unwrap();
        let personal = dir.path().join("data/ngteams");
        std::fs::create_dir_all(personal.join("sub")).unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let link = scratch.join("link");
        std::os::unix::fs::symlink(personal.join("sub"), &link).unwrap();
        assert!(overlapping(&link.join("../lab"), &personal));
        assert!(!overlapping(&scratch.join("lab"), &personal));
        assert!(overlapping(&link.join("../not/yet/../lab"), &personal));
    }
}
