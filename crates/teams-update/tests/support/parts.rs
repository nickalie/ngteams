//! Detection, stages, checksums and the prompt's outcomes, one at a time.

use std::fs;
use std::path::Path;

use super::{archive_installation, places, tree, version};
use crate::check::{bounded, checksum};
use crate::privileged::{Prompted, classify};
use crate::{Error, Format, Installation};

#[test]
fn only_a_marked_release_tree_is_an_installation() {
    let home = tempfile::tempdir().unwrap();
    // a source build: the binary in a lib folder, no marker
    let lib = home.path().join("lib/ngteams");
    fs::create_dir_all(&lib).unwrap();
    fs::write(lib.join("ngteams"), "").unwrap();
    let error = Installation::detect(&places(lib.join("ngteams"), home.path())).unwrap_err();
    assert!(
        error.to_string().contains("wasn't installed from a release package"),
        "{error}"
    );
    // a tree without its marker
    let bare = home.path().join("bare/ngteams");
    tree(&bare, "0.1.0.5", Format::Archive).unwrap();
    fs::remove_file(bare.join(crate::MARKER)).unwrap();
    assert!(Installation::detect(&places(bare.join("usr/bin/ngteams"), home.path())).is_err());
    // an archive moved into a folder of another name
    let renamed = home.path().join("apps/teams");
    tree(&renamed, "0.1.0.5", Format::Archive).unwrap();
    let error = Installation::detect(&places(renamed.join("usr/bin/ngteams"), home.path())).unwrap_err();
    assert!(error.to_string().contains("folder named ngteams"), "{error}");
    // nothing there at all
    assert!(Installation::detect(&places(home.path().join("gone/usr/bin/ngteams"), home.path())).is_err());
}

#[test]
fn an_installation_reads_its_format_version_and_restart_program() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    assert_eq!(installation.format(), Format::Archive);
    assert_eq!(installation.installed_version(), Some(version("0.1.0.5")));
    assert_eq!(
        installation.root(),
        fs::canonicalize(home.path().join("opt/ngteams")).unwrap()
    );
    assert_eq!(installation.executable(), installation.root().join("usr/bin/ngteams"));
    assert_eq!(installation.stage_parent(), installation.root().parent().unwrap());
    for format in [Format::Deb, Format::Rpm, Format::Pacman] {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join("opt/ngteams");
        tree(&root, "0.1.0.5", format).unwrap();
        let installation = Installation::detect(&places(root.join("usr/bin/ngteams"), home.path())).unwrap();
        assert_eq!(installation.format(), format);
        assert_eq!(installation.stage_parent(), home.path().join("data/updates"));
    }
    // an AppImage's tree outside its mount (extracted by hand) is an archive tree
    let extracted = home.path().join("extracted/ngteams");
    tree(&extracted, "0.1.0.5", Format::AppImage).unwrap();
    let mut where_ = places(extracted.join("usr/bin/ngteams"), home.path());
    where_.appimage = Some(home.path().join("elsewhere.AppImage").into());
    where_.appdir = Some(home.path().join("mount").into());
    assert_eq!(Installation::detect(&where_).unwrap().format(), Format::Archive);
}

// an AppImage is Linux's
#[cfg(target_os = "linux")]
#[test]
fn an_appimage_that_is_gone_is_no_installation() {
    let home = tempfile::tempdir().unwrap();
    let mount = home.path().join("mount");
    tree(&mount, "0.1.0.5", Format::AppImage).unwrap();
    let mut where_ = places(mount.join("usr/bin/ngteams"), home.path());
    where_.appimage = Some(home.path().into());
    where_.appdir = Some(mount.into());
    let error = Installation::detect(&where_).unwrap_err();
    assert!(error.to_string().contains("isn't a file"), "{error}");
}

#[test]
fn a_package_upgraded_under_the_running_app_is_still_found() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::Deb).unwrap();
    let deleted = format!("{} (deleted)", root.join("usr/bin/ngteams").display());
    let installation = Installation::detect(&places(deleted.into(), home.path())).unwrap();
    assert_eq!(
        installation.executable(),
        fs::canonicalize(root.join("usr/bin/ngteams")).unwrap()
    );
}

#[test]
fn user_data_inside_the_installation_is_never_replaced() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::Archive).unwrap();
    let mut where_ = places(root.join("usr/bin/ngteams"), home.path());
    // a data folder that doesn't exist yet, inside it
    where_.protected = vec![root.join("data/not-yet")];
    let error = Installation::detect(&where_).unwrap_err();
    assert!(
        error.to_string().contains("inside the installation's folder"),
        "{error}"
    );
    // reached through a symbolic link
    std::os::unix::fs::symlink(&root, home.path().join("link")).unwrap();
    where_.protected = vec![home.path().join("link/settings")];
    assert!(Installation::detect(&where_).is_err());
}

#[test]
fn stale_stages_go_and_live_ones_stay() {
    let parent = tempfile::tempdir().unwrap();
    let stage = |name: &str, owner: Option<String>, recovery: bool| {
        let dir = parent.path().join(format!("{}{name}", crate::stage::PREFIX));
        fs::create_dir_all(dir.join("previous")).unwrap();
        if let Some(owner) = owner {
            fs::write(dir.join("owner"), owner).unwrap();
        }
        if recovery {
            fs::write(dir.join(crate::stage::RECOVERY), "").unwrap();
        }
        dir
    };
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let mine = stage("mine", Some(std::process::id().to_string()), false);
    let gone = stage("gone", Some(dead.to_string()), false);
    let live = stage("live", Some("1".into()), false);
    let kept = stage("kept", Some(dead.to_string()), true);
    let unowned = stage("unowned", None, false);
    let other = parent.path().join("unrelated");
    fs::create_dir(&other).unwrap();
    assert_eq!(crate::stage::cleanup(parent.path()), 2);
    assert!(!mine.exists() && !gone.exists());
    assert!(live.exists() && kept.exists() && unowned.exists() && other.exists());
    assert_eq!(crate::stage::cleanup(&parent.path().join("missing")), 0);
    assert!(crate::stage::alive(0));
    assert!(crate::stage::alive(u32::MAX));
}

#[test]
fn checksums_are_read_line_by_line_and_exactly() {
    let hash = "A".repeat(64);
    let manifest = format!("{hash}  asset.tar.gz\n{}  other\n", "b".repeat(64));
    assert_eq!(checksum(&manifest, "asset.tar.gz").unwrap(), "a".repeat(64));
    assert_eq!(checksum(&format!("{hash} *asset"), "asset").unwrap(), "a".repeat(64));
    for (manifest, said) in [
        (String::new(), "no single line"),
        (format!("{hash}  asset\n{hash}  asset\n"), "no single line"),
        ("abc  asset".to_owned(), "isn't a SHA-256"),
        (format!("{}  asset", "g".repeat(64)), "isn't a SHA-256"),
    ] {
        let error = checksum(&manifest, "asset").unwrap_err();
        assert!(error.to_string().contains(said), "{error}");
    }
    assert_eq!(bounded(&b"four"[..], 4, "x").unwrap(), b"four");
    assert!(
        bounded(&b"five!"[..], 4, "The thing")
            .unwrap_err()
            .to_string()
            .starts_with("The thing is larger")
    );
}

#[test]
fn the_prompts_outcomes_are_told_apart() {
    assert_eq!(classify(Some(0), "").unwrap(), Prompted::Installed);
    assert_eq!(classify(Some(126), "").unwrap(), Prompted::Cancelled);
    for said in [
        "No authentication agent found",
        "Error getting authority",
        "pkexec must be setuid root",
    ] {
        assert_eq!(classify(Some(127), said).unwrap(), Prompted::Unavailable);
    }
    let error = classify(Some(100), "line 1\n\nline 2\nline 3\nline 4\n").unwrap_err();
    assert!(error.to_string().ends_with("line 2\nline 3\nline 4"), "{error}");
    assert!(classify(None, "").is_err());
}

#[test]
fn full_disks_and_unwritable_places_are_said_in_words() {
    let parent = Path::new("/somewhere");
    let full = crate::prepare::unwritable(std::io::Error::from_raw_os_error(libc::ENOSPC), parent);
    assert!(full.to_string().contains("enough free space in /somewhere"), "{full}");
    for code in [libc::EACCES, libc::EPERM, libc::EROFS] {
        let error = crate::prepare::unwritable(std::io::Error::from_raw_os_error(code), parent);
        assert!(error.to_string().contains("can't write to /somewhere"), "{error}");
    }
    assert!(matches!(
        crate::prepare::unwritable(std::io::Error::from_raw_os_error(libc::EIO), parent),
        Error::Io(_)
    ));
}

#[test]
fn a_download_that_fills_the_disk_says_how_much_it_needs() {
    let parent = Path::new("/somewhere");
    let full = crate::prepare::short_of_space(
        Error::Io(std::io::Error::from_raw_os_error(libc::ENOSPC)),
        3 << 20,
        parent,
    );
    assert!(full.to_string().contains("it needs about 3 MB"), "{full}");
    let other = crate::prepare::short_of_space(crate::error("as it was"), 1, parent);
    assert_eq!(other.to_string(), "as it was");
}
