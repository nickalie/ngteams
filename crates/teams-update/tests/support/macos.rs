//! The macOS app bundle's update: found from its program, unpacked from its zip with `ditto`, and refused unless its
//! marker, identity, programs and code signature all hold. Real bundles signed ad hoc, as a build without a Developer
//! ID is; no network, no installation outside a temporary folder.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::version;
use crate::{Format, IDENTITY, Installation, MARKER, Places};

const PROGRAMS: [&str; 3] = ["ngteams", "ngteams-webview", "ngteamsctl"];

/// `ngteams.app` of `version` in `parent`, its programs copies of a system program, signed ad hoc.
fn bundle(parent: &Path, version: &str) -> std::path::PathBuf {
    let app = parent.join("ngteams.app");
    let programs = app.join("Contents/MacOS");
    fs::create_dir_all(&programs).unwrap();
    for program in PROGRAMS {
        fs::copy("/usr/bin/true", programs.join(program)).unwrap();
    }
    fs::write(
        app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>ngteams</string>
<key>CFBundleIdentifier</key><string>com.nickalie.ngteams.test</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
"#,
    )
    .unwrap();
    fs::create_dir_all(app.join(MARKER).parent().unwrap()).unwrap();
    fs::write(app.join(MARKER), "macos-app\n").unwrap();
    fs::write(app.join(IDENTITY), format!("{version}\n")).unwrap();
    for program in ["ngteamsctl", "ngteams-webview"] {
        sign(&programs.join(program));
    }
    sign(&app);
    app
}

fn sign(path: &Path) {
    let status = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "codesign {}", path.display());
}

/// The zip of `app`, as the release workflow makes it.
fn zipped(app: &Path, zip: &Path) {
    let status = Command::new("/usr/bin/ditto")
        .args(["-c", "-k", "--keepParent"])
        .arg(app)
        .arg(zip)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn the_bundle_a_program_runs_from_is_the_installation() {
    let home = tempfile::tempdir().unwrap();
    let app = bundle(home.path(), "0.1.0.5");
    let places = Places {
        executable: app.join("Contents/MacOS/ngteams"),
        appimage: None,
        appdir: None,
        stage_dir: home.path().join("stage"),
        protected: vec![home.path().join("data")],
    };
    let installation = Installation::detect(&places).unwrap();
    assert_eq!(installation.format(), Format::MacApp);
    assert_eq!(installation.root(), fs::canonicalize(&app).unwrap());
    assert_eq!(installation.installed_version(), Some(version("0.1.0.5")));
    // its stage goes beside the bundle, for the renames
    assert_eq!(installation.stage_parent(), fs::canonicalize(home.path()).unwrap());

    // a bundle without the release marker is a build of its own
    fs::remove_file(app.join(MARKER)).unwrap();
    assert!(Installation::detect(&places).is_err());
}

#[test]
fn a_downloaded_app_is_unpacked_and_checked_before_it_is_installed() {
    let home = tempfile::tempdir().unwrap();
    let built = home.path().join("built");
    fs::create_dir(&built).unwrap();
    let zip = home.path().join("ngteams.zip");
    zipped(&bundle(&built, "0.1.0.6"), &zip);
    let work = home.path().join("work");
    fs::create_dir(&work).unwrap();
    let candidate = crate::prepare::ready(Format::MacApp, &zip, &work, version("0.1.0.6")).unwrap();
    assert_eq!(candidate, work.join("unpacked/ngteams.app"));
    assert!(candidate.join("Contents/MacOS/ngteams").is_file());
    assert!(!zip.exists(), "the zip is dropped once unpacked");

    // another release's app is refused
    zipped(&candidate, &zip);
    let other = home.path().join("other");
    fs::create_dir(&other).unwrap();
    let refused = crate::prepare::ready(Format::MacApp, &zip, &other, version("0.1.0.7")).unwrap_err();
    assert!(refused.to_string().contains("isn't release 0.1.0.7's app"), "{refused}");
}

#[test]
fn an_app_changed_after_its_signing_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let built = home.path().join("built");
    fs::create_dir(&built).unwrap();
    let app = bundle(&built, "0.1.0.6");
    fs::write(app.join("Contents/MacOS/ngteamsctl"), "changed").unwrap();
    let zip = home.path().join("ngteams.zip");
    zipped(&app, &zip);
    let work = home.path().join("work");
    fs::create_dir(&work).unwrap();
    let refused = crate::prepare::ready(Format::MacApp, &zip, &work, version("0.1.0.6")).unwrap_err();
    assert!(refused.to_string().contains("code signature"), "{refused}");
}

#[test]
fn an_app_missing_a_program_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let built = home.path().join("built");
    fs::create_dir(&built).unwrap();
    let app = bundle(&built, "0.1.0.6");
    fs::remove_file(app.join("Contents/MacOS/ngteams-webview")).unwrap();
    let zip = home.path().join("ngteams.zip");
    zipped(&app, &zip);
    let work = home.path().join("work");
    fs::create_dir(&work).unwrap();
    let refused = crate::prepare::ready(Format::MacApp, &zip, &work, version("0.1.0.6")).unwrap_err();
    assert!(
        refused.to_string().contains("no Contents/MacOS/ngteams-webview"),
        "{refused}"
    );
}
