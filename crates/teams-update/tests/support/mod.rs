//! The updater against a local fake release feed and disposable installations in temporary folders: nothing here
//! reaches GitHub or touches an installed copy.

#[cfg(unix)]
mod flows;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod parts;
mod setup;

use std::path::{Path, PathBuf};

#[cfg(unix)]
use crate::Installation;
#[cfg(unix)]
pub(super) use crate::fakes::tree;
use crate::{Format, Places, Version};

pub(super) fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// What a test's installation is detected from, with its stages and data in `home`.
pub(super) fn places(executable: PathBuf, home: &Path) -> Places {
    Places {
        executable,
        appimage: None,
        appdir: None,
        stage_dir: home.join("data/updates"),
        protected: vec![home.join("data"), home.join("config")],
    }
}

/// An unpacked archive of `version` at `<home>/opt/ngteams`, detected.
#[cfg(unix)]
pub(super) fn archive_installation(home: &Path, version: &str) -> Installation {
    let root = home.join("opt/ngteams");
    tree(&root, version, Format::Archive).unwrap();
    Installation::detect(&places(root.join("usr/bin/ngteams"), home)).unwrap()
}

#[test]
fn versions_are_four_numbers_compared_in_order() {
    assert_eq!(version("0.1.0.12").to_string(), "0.1.0.12");
    assert!(version("0.1.0.13") > version("0.1.0.12"));
    assert!(version("0.1.0.100") > version("0.1.0.99"));
    assert!(version("0.2.0.1") > version("0.1.9.999"));
    for bad in [
        "",
        "0.1.0",
        "0.1.0.1.2",
        "0.1.0.x",
        "0.1..1",
        "v0.1.0.1",
        "0.1.0.-1",
        "0.1.0.+1",
        " 0.1.0.1",
    ] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
    // the build here is no release
    assert_eq!(Version::current(), None);
    // on wherever a workspace-wide build unifies in a dependent's `test-support` (ngteams' dev-dependency)
    assert_eq!(crate::test_support_enabled(), cfg!(feature = "test-support"));
}

#[test]
fn each_format_takes_the_asset_the_release_workflow_publishes() {
    let release = version("0.1.0.12");
    // the names of release v0.1.0.12's assets
    assert_eq!(Format::Archive.asset(release), "ngteams-0.1.0.12-linux-x86_64.tar.gz");
    assert_eq!(
        Format::AppImage.asset(release),
        "ngteams-0.1.0.12-linux-x86_64.AppImage"
    );
    assert_eq!(Format::Deb.asset(release), "ngteams_0.1.0.12-1_amd64.deb");
    assert_eq!(Format::Rpm.asset(release), "ngteams-0.1.0.12-1.x86_64.rpm");
    assert_eq!(Format::Pacman.asset(release), "ngteams-0.1.0.12-1-x86_64.pkg.tar.zst");
    for format in [
        Format::Archive,
        Format::Deb,
        Format::Rpm,
        Format::Pacman,
        Format::WindowsSetup,
        Format::MacApp,
    ] {
        assert_eq!(Format::from_marker(&format!("{}\n", format.marker())), Some(format));
    }
    // an AppImage's tree found unpacked is an archive
    assert_eq!(Format::from_marker("linux-appimage"), Some(Format::Archive));
    assert_eq!(Format::from_marker("windows-msi"), None);
    assert_eq!(Format::Archive.system_package(), None);
    assert_eq!(Format::AppImage.system_package(), None);
    assert_eq!(Format::Deb.system_package(), Some("deb"));
    assert_eq!(Format::Rpm.system_package(), Some("rpm"));
    assert_eq!(Format::Pacman.system_package(), Some("pacman"));
}

#[test]
fn a_stage_goes_beside_the_installation_only_where_an_update_renames_it_in() {
    assert_eq!(
        Format::WindowsSetup.asset(version("0.1.0.12")),
        "ngteams-0.1.0.12-windows-x86_64-setup.exe"
    );
    assert_eq!(Format::WindowsSetup.system_package(), None);
    assert_eq!(
        Format::MacApp.asset(version("0.1.0.12")),
        "ngteams-0.1.0.12-macos-arm64.zip"
    );
    assert_eq!(Format::MacApp.system_package(), None);
    for (format, own_folder) in [
        (Format::Archive, false),
        (Format::AppImage, false),
        (Format::Deb, true),
        (Format::WindowsSetup, true),
        (Format::MacApp, false),
    ] {
        assert_eq!(format.stages_in_app_folder(), own_folder, "{format:?}");
    }
}

#[test]
fn network_failures_read_as_offline_and_the_rest_says_what_happened() {
    use crate::Error;
    for offline in [
        ureq::Error::HostNotFound,
        ureq::Error::ConnectionFailed,
        ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
        ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::NetworkUnreachable)),
    ] {
        assert!(matches!(Error::from(offline), Error::Offline));
    }
    let other = Error::from(ureq::Error::TooManyRedirects);
    assert!(
        other.to_string().starts_with("GitHub's answer couldn't be read"),
        "{other}"
    );
    assert!(Error::Offline.to_string().contains("can't be reached"));
    assert!(Error::RateLimited.to_string().contains("limiting"));
    assert!(Error::Cancelled.to_string().contains("password prompt was closed"));
}
