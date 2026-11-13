//! Check, download, verify and install, end to end against the fake feed.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use super::{archive_installation, places, version};
use crate::fakes::{Feed, Reply, archive_bytes, program, publish, release_json, sha256, tree};
use crate::{Error, Format, IDENTITY, Installation, Source};

/// A change made to a release tree before it is packed.
type Edit = fn(&std::path::Path);

/// No credential of any kind went to the feed (GitHub).
fn anonymous(feed: &Feed) {
    let asked = feed.requests().join("\n").to_ascii_lowercase();
    assert!(asked.contains("user-agent: ngteams/"));
    for credential in ["authorization", "cookie", "token"] {
        assert!(!asked.contains(credential), "{credential} in {asked}");
    }
}

#[test]
fn a_newer_release_is_offered_downloaded_verified_and_swapped_in() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let feed = Feed::start().unwrap();
    let asset = Format::Archive.asset(version("0.1.0.7"));
    publish(&feed, "v0.1.0.7", &asset, &archive_bytes("0.1.0.7", |_| {}).unwrap());
    let source = Source::local(&feed.base);
    let release = source.check(&installation, version("0.1.0.5")).unwrap().unwrap();
    assert_eq!(release.version, version("0.1.0.7"));
    assert_eq!(release.asset(), asset);
    anonymous(&feed);
    let mut heard = Vec::new();
    let mut prepared = source
        .prepare(&installation, &release, |received, total| heard.push((received, total)))
        .unwrap();
    assert_eq!(prepared.version(), version("0.1.0.7"));
    assert_eq!(heard.first(), Some(&(0, release.size())));
    assert_eq!(heard.last(), Some(&(release.size(), release.size())));
    // nothing is installed until asked
    assert_eq!(
        fs::read_to_string(installation.root().join(IDENTITY)).unwrap(),
        "0.1.0.5\n"
    );
    let restart = prepared.install().unwrap();
    assert_eq!(restart.program, installation.root().join("usr/bin/ngteams"));
    assert_eq!(
        fs::read_to_string(installation.root().join(IDENTITY)).unwrap(),
        "0.1.0.7\n"
    );
    assert_eq!(
        fs::read_to_string(installation.root().join("usr/bin/ngteams-web")).unwrap(),
        "usr/bin/ngteams-web 0.1.0.7"
    );
    // the previous version waits in the stage for a later start, which removes it
    let stage = home.path().join("opt");
    let stages: Vec<_> = fs::read_dir(&stage)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".ngteams-update-"))
        .collect();
    assert_eq!(stages.len(), 1);
    assert_eq!(
        fs::read_to_string(stages[0].path().join("previous").join(IDENTITY)).unwrap(),
        "0.1.0.5\n"
    );
    assert!(prepared.install().is_err(), "an update goes in once");
    anonymous(&feed);
    assert_eq!(installation.cleanup_stale(), 1);
    assert!(!stages[0].path().exists());
}

#[test]
fn nothing_newer_is_nothing_offered() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.7");
    let feed = Feed::start().unwrap();
    let source = Source::local(&feed.base);
    // no release at all
    assert_eq!(source.check(&installation, version("0.1.0.7")).unwrap(), None);
    let bytes = archive_bytes("0.1.0.7", |_| {}).unwrap();
    for tag in ["v0.1.0.7", "v0.1.0.6", "web-engine-abc", "v0.1.0"] {
        publish(&feed, tag, &Format::Archive.asset(version("0.1.0.7")), &bytes);
        assert_eq!(source.check(&installation, version("0.1.0.7")).unwrap(), None, "{tag}");
    }
    // the files say a newer version is in already (updated under the running app)
    publish(&feed, "v0.1.0.8", &Format::Archive.asset(version("0.1.0.8")), &bytes);
    fs::write(installation.root().join(IDENTITY), "0.1.0.8\n").unwrap();
    assert_eq!(source.check(&installation, version("0.1.0.7")).unwrap(), None);
    for (from, flag) in [
        (r#""draft":false"#, r#""draft":true"#),
        (r#""prerelease":false"#, r#""prerelease":true"#),
    ] {
        let json = release_json(&feed.base, "v0.1.0.9", &[("x", 1)]).replace(from, flag);
        feed.serve("/latest", Reply::ok(json));
        assert_eq!(source.check(&installation, version("0.1.0.7")).unwrap(), None, "{flag}");
    }
}

#[test]
fn rate_limits_offline_and_bad_answers_are_told_apart() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let feed = Feed::start().unwrap();
    let source = Source::local(&feed.base);
    for status in ["403 Forbidden", "429 Too Many Requests"] {
        feed.serve("/latest", Reply::status(status));
        assert!(matches!(
            source.check(&installation, version("0.1.0.5")),
            Err(Error::RateLimited)
        ));
    }
    feed.serve("/latest", Reply::status("500 Internal Server Error"));
    let error = source.check(&installation, version("0.1.0.5")).unwrap_err();
    assert_eq!(error.to_string(), "GitHub answered 500. Try again later.");
    feed.serve("/latest", Reply::ok("not json"));
    let error = source.check(&installation, version("0.1.0.5")).unwrap_err();
    assert!(error.to_string().contains("couldn't be read"), "{error}");
    feed.serve("/latest", Reply::ok(vec![b' '; 3 * 1024 * 1024]));
    let error = source.check(&installation, version("0.1.0.5")).unwrap_err();
    assert!(error.to_string().contains("larger than any release"), "{error}");
    // nobody listening there
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    assert!(matches!(
        Source::local(&base).check(&installation, version("0.1.0.5")),
        Err(Error::Offline)
    ));
}

#[test]
fn a_release_missing_or_misplacing_its_files_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let feed = Feed::start().unwrap();
    let source = Source::local(&feed.base);
    let asset = Format::Archive.asset(version("0.1.0.6"));
    let cases = [
        (vec![("SHA256SUMS", 10)], "has no ngteams-0.1.0.6-linux-x86_64.tar.gz"),
        (vec![(asset.as_str(), 10)], "has no SHA256SUMS"),
        (
            vec![(asset.as_str(), 10), (asset.as_str(), 10), ("SHA256SUMS", 10)],
            "has no ngteams",
        ),
        (vec![(asset.as_str(), 0), ("SHA256SUMS", 10)], "sizes no release has"),
        (
            vec![(asset.as_str(), 2 << 30), ("SHA256SUMS", 10)],
            "sizes no release has",
        ),
        (
            vec![(asset.as_str(), 10), ("SHA256SUMS", 1 << 20)],
            "sizes no release has",
        ),
    ];
    for (assets, said) in cases {
        feed.serve("/latest", Reply::ok(release_json(&feed.base, "v0.1.0.6", &assets)));
        let error = source.check(&installation, version("0.1.0.5")).unwrap_err();
        assert!(error.to_string().contains(said), "{error} for {said}");
    }
    let elsewhere = release_json(
        "https://example.com",
        "v0.1.0.6",
        &[(asset.as_str(), 10), ("SHA256SUMS", 10)],
    );
    feed.serve("/latest", Reply::ok(elsewhere));
    let error = source.check(&installation, version("0.1.0.5")).unwrap_err();
    assert!(error.to_string().contains("isn't where GitHub keeps"), "{error}");
}

/// A release whose asset `serve` gives, prepared against `installation`: the error, with the stage checked gone and
/// the installation unchanged.
fn refused(installation: &Installation, serve: impl FnOnce(&Feed, &str)) -> Error {
    let feed = Feed::start().unwrap();
    let asset = installation.format().asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, &archive_bytes("0.1.0.6", |_| {}).unwrap());
    serve(&feed, &asset);
    let source = Source::local(&feed.base);
    let release = source.check(installation, version("0.1.0.5")).unwrap().unwrap();
    let before = fs::read(installation.root().join(IDENTITY)).ok();
    let error = source.prepare(installation, &release, |_, _| {}).unwrap_err();
    assert_eq!(fs::read(installation.root().join(IDENTITY)).ok(), before);
    let parent = installation.root().parent().unwrap();
    let stages = fs::read_dir(parent)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".ngteams-update-"))
        .count();
    assert_eq!(stages, 0, "the stage is removed");
    error
}

#[test]
fn a_download_that_doesnt_match_its_checksum_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let tampered = refused(&installation, |feed, asset| {
        let mut bytes = archive_bytes("0.1.0.6", |_| {}).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        feed.serve(&format!("/download/v0.1.0.6/{asset}"), Reply::ok(bytes));
    });
    assert!(
        tampered.to_string().contains("doesn't match the release's SHA256SUMS"),
        "{tampered}"
    );
    let longer = refused(&installation, |feed, asset| {
        let mut bytes = archive_bytes("0.1.0.6", |_| {}).unwrap();
        bytes.extend(b"more");
        feed.serve(&format!("/download/v0.1.0.6/{asset}"), Reply::ok(bytes));
    });
    assert!(longer.to_string().contains("larger than the release says"), "{longer}");
    let unlisted = refused(&installation, |feed, _| {
        feed.serve(
            "/download/v0.1.0.6/SHA256SUMS",
            Reply::ok(format!("{}  other\n", "0".repeat(64))),
        );
    });
    assert!(unlisted.to_string().contains("no single line"), "{unlisted}");
    let gone = refused(&installation, |feed, asset| {
        feed.serve(&format!("/download/v0.1.0.6/{asset}"), Reply::status("404 Not Found"));
    });
    assert!(gone.to_string().contains("GitHub answered 404"), "{gone}");
    let limited = refused(&installation, |feed, _| {
        feed.serve("/download/v0.1.0.6/SHA256SUMS", Reply::status("429 Too Many Requests"));
    });
    assert!(matches!(limited, Error::RateLimited));
    let binary = refused(&installation, |feed, _| {
        feed.serve("/download/v0.1.0.6/SHA256SUMS", Reply::ok(vec![0xff, 0xfe]));
    });
    assert!(binary.to_string().contains("isn't text"), "{binary}");
}

#[test]
fn an_archive_of_another_release_or_missing_a_program_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let cases: [(&str, Edit); 3] = [
        ("isn't release 0.1.0.6's archive", |root| {
            fs::write(root.join(IDENTITY), "0.1.0.4\n").unwrap();
        }),
        ("isn't release 0.1.0.6's archive", |root| {
            fs::write(root.join(crate::MARKER), "linux-deb\n").unwrap();
        }),
        ("has no usr/bin/ngteams-web", |root| {
            fs::remove_file(root.join("usr/bin/ngteams-web")).unwrap();
        }),
    ];
    for (said, edit) in cases {
        let error = refused(&installation, |feed, asset| {
            let bytes = archive_bytes("0.1.0.6", edit).unwrap();
            publish(feed, "v0.1.0.6", asset, &bytes);
        });
        assert!(error.to_string().contains(said), "{error} for {said}");
    }
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_copy_marked_as_the_macos_app_isnt_updated_elsewhere() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("opt/ngteams");
    tree(&root, "0.1.0.5", Format::MacApp).unwrap();
    let installation = Installation::detect(&places(root.join("usr/bin/ngteams"), home.path())).unwrap();
    assert_eq!(installation.format(), Format::MacApp);
    let error = refused(&installation, |_, _| {});
    assert!(error.to_string().contains("updates only on macOS"), "{error}");
}

#[test]
fn a_folder_the_user_cant_write_is_said_and_left_alone() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let feed = Feed::start().unwrap();
    let asset = Format::Archive.asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, &archive_bytes("0.1.0.6", |_| {}).unwrap());
    let source = Source::local(&feed.base);
    let release = source.check(&installation, version("0.1.0.5")).unwrap().unwrap();
    let parent = home.path().join("opt");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let error = source.prepare(&installation, &release, |_, _| {}).unwrap_err();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(error.to_string().contains("can't write to"), "{error}");
    assert!(error.to_string().contains(crate::RELEASES_URL), "{error}");
    assert_eq!(
        fs::read_to_string(installation.root().join(IDENTITY)).unwrap(),
        "0.1.0.5\n"
    );
}

#[test]
fn an_installation_updated_meanwhile_isnt_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let feed = Feed::start().unwrap();
    let asset = Format::Archive.asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, &archive_bytes("0.1.0.6", |_| {}).unwrap());
    let source = Source::local(&feed.base);
    let release = source.check(&installation, version("0.1.0.5")).unwrap().unwrap();
    let mut prepared = source.prepare(&installation, &release, |_, _| {}).unwrap();
    fs::write(installation.root().join(IDENTITY), "0.1.0.9\n").unwrap();
    let error = prepared.install().unwrap_err();
    assert!(error.to_string().contains("updated by something else"), "{error}");
    assert_eq!(
        fs::read_to_string(installation.root().join(IDENTITY)).unwrap(),
        "0.1.0.9\n"
    );
    // and a try again doesn't download an older release over it
    let again = source.prepare(&installation, &release, |_, _| {}).unwrap_err();
    assert!(again.to_string().contains("already"), "{again}");
    // the installation gone altogether
    fs::remove_dir_all(installation.root()).unwrap();
    assert!(prepared.install().is_err());
}

#[test]
fn a_swap_that_cant_finish_puts_the_installed_version_back() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let feed = Feed::start().unwrap();
    let asset = Format::Archive.asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, &archive_bytes("0.1.0.6", |_| {}).unwrap());
    let source = Source::local(&feed.base);
    let release = source.check(&installation, version("0.1.0.5")).unwrap().unwrap();
    let mut prepared = source.prepare(&installation, &release, |_, _| {}).unwrap();
    // the unpacked release gone from its stage: the second rename fails
    fs::remove_dir_all(&prepared.candidate).unwrap();
    let error = prepared.install().unwrap_err();
    assert!(error.to_string().contains("was put back"), "{error}");
    assert_eq!(
        fs::read_to_string(installation.root().join(IDENTITY)).unwrap(),
        "0.1.0.5\n"
    );
}

// an AppImage is Linux's
#[cfg(target_os = "linux")]
#[test]
fn an_appimage_is_replaced_by_its_newer_file() {
    let home = tempfile::tempdir().unwrap();
    let image = home.path().join("Apps/ngteams.AppImage");
    fs::create_dir_all(image.parent().unwrap()).unwrap();
    fs::write(&image, "old image").unwrap();
    let mount = home.path().join("mount");
    tree(&mount, "0.1.0.5", Format::AppImage).unwrap();
    let mut where_ = places(mount.join("usr/bin/ngteams"), home.path());
    where_.appimage = Some(image.clone().into());
    where_.appdir = Some(mount.clone().into());
    let installation = Installation::detect(&where_).unwrap();
    assert_eq!(installation.format(), Format::AppImage);
    assert_eq!(installation.installed_version(), None);
    let feed = Feed::start().unwrap();
    let asset = Format::AppImage.asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, b"new image");
    let source = Source::local(&feed.base);
    let release = source.check(&installation, version("0.1.0.5")).unwrap().unwrap();
    let mut prepared = source.prepare(&installation, &release, |_, _| {}).unwrap();
    let restart = prepared.install().unwrap();
    assert_eq!(restart.program, fs::canonicalize(&image).unwrap());
    assert_eq!(fs::read(&image).unwrap(), b"new image");
    assert_eq!(fs::metadata(&image).unwrap().permissions().mode() & 0o777, 0o755);
    // replaced meanwhile: its identity (inode, size, time) differs
    let mut again = source.prepare(&installation, &release, |_, _| {}).unwrap();
    fs::write(home.path().join("other"), "another image").unwrap();
    fs::rename(home.path().join("other"), &image).unwrap();
    assert!(
        again
            .install()
            .unwrap_err()
            .to_string()
            .contains("updated by something else")
    );
}

/// pkexec's part, faked: the root script run as this user, staging in `<home>/var-tmp` rather than `/var/tmp`, which a
/// test can't count on writing (CI's container has it read-only).
fn as_this_user(home: &std::path::Path) -> String {
    assert!(crate::privileged::ROOT_SCRIPT.contains("mktemp -d /var/tmp/"));
    let stage = home.join("var-tmp");
    fs::create_dir_all(&stage).unwrap();
    format!(
        "shift; shell=$1; flag=$2; script=$(printf '%s\\n' \"$3\" | sed 's|/var/tmp/|{}/|'); shift 3; \
         exec \"$shell\" \"$flag\" \"$script\" \"$@\"",
        stage.display()
    )
}

/// A package installation at `<home>/opt/ngteams`, its prompt the script `prompter` (a test's pkexec).
fn package_installation(home: &std::path::Path, format: Format, prompter: &str) -> Installation {
    let root = home.join("opt/ngteams");
    tree(&root, "0.1.0.5", format).unwrap();
    let script = home.join("prompter");
    program(&script, &format!("#!/bin/sh\n{prompter}\n")).unwrap();
    Installation::detect(&places(root.join("usr/bin/ngteams"), home))
        .unwrap()
        .with_prompter(script)
}

/// `installation`'s package of 0.1.0.6, prepared from the feed.
fn package_prepared(installation: &Installation) -> (crate::Prepared, Feed) {
    let feed = Feed::start().unwrap();
    let asset = installation.format().asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, b"package bytes");
    let source = Source::local(&feed.base);
    let release = source.check(installation, version("0.1.0.5")).unwrap().unwrap();
    (source.prepare(installation, &release, |_, _| {}).unwrap(), feed)
}

#[test]
fn a_package_goes_to_the_package_manager_behind_the_password_prompt() {
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let log = home.path().join("installed");
    // the package managers, faked: each says what it was asked to install
    for manager in ["apt-get", "pacman", "dnf"] {
        let fake = bin.join(manager);
        program(
            &fake,
            &format!("#!/bin/sh\necho {manager} \"$@\" >> '{}'\n", log.display()),
        )
        .unwrap();
    }
    // the package managers above found first by the root script
    let path = format!("PATH='{}':\"$PATH\"; export PATH", bin.display());
    for (format, manager) in [
        (Format::Deb, "apt-get"),
        (Format::Pacman, "pacman"),
        (Format::Rpm, "dnf"),
    ] {
        let case = tempfile::tempdir().unwrap();
        let pkexec = format!("{path}; {}", as_this_user(case.path()));
        let installation = package_installation(case.path(), format, &pkexec);
        // a package's download waits in the app's own folder
        let (mut prepared, _feed) = package_prepared(&installation);
        assert!(prepared.candidate.starts_with(case.path().join("data/updates")));
        let restart = prepared.install().unwrap();
        assert_eq!(restart.program, installation.root().join("usr/bin/ngteams"));
        let installed = fs::read_to_string(&log).unwrap();
        let line = installed.lines().last().unwrap();
        assert!(line.starts_with(manager), "{line}");
        assert!(line.ends_with(&format.asset(version("0.1.0.6"))), "{line}");
    }
}

#[test]
fn a_closed_prompt_or_none_at_all_changes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let cancelled = package_installation(home.path(), Format::Deb, "exit 126");
    let (mut prepared, _feed) = package_prepared(&cancelled);
    assert!(matches!(prepared.install(), Err(Error::Cancelled)));
    let home = tempfile::tempdir().unwrap();
    let agentless = package_installation(home.path(), Format::Deb, "echo 'Error getting authority' >&2; exit 127");
    let (mut prepared, _feed) = package_prepared(&agentless);
    let error = prepared.install().unwrap_err();
    assert!(error.to_string().contains("no password prompt"), "{error}");
    assert!(error.to_string().contains(crate::RELEASES_URL), "{error}");
    assert!(error.to_string().contains("ngteams_0.1.0.6-1_amd64.deb"), "{error}");
    let home = tempfile::tempdir().unwrap();
    let missing =
        package_installation(home.path(), Format::Rpm, "true").with_prompter(home.path().join("no-such-pkexec"));
    let (mut prepared, _feed) = package_prepared(&missing);
    assert!(
        prepared
            .install()
            .unwrap_err()
            .to_string()
            .contains("no password prompt")
    );
    let home = tempfile::tempdir().unwrap();
    let failing = package_installation(
        home.path(),
        Format::Pacman,
        "echo 'error: conflicting files' >&2; exit 1",
    );
    let (mut prepared, _feed) = package_prepared(&failing);
    let error = prepared.install().unwrap_err();
    assert!(error.to_string().contains("conflicting files"), "{error}");
    assert_eq!(fs::read_to_string(failing.root().join(IDENTITY)).unwrap(), "0.1.0.5\n");
}

#[test]
fn the_root_script_refuses_a_package_or_installation_changed_since_the_check() {
    let home = tempfile::tempdir().unwrap();
    let pkexec = as_this_user(home.path());
    let installation = package_installation(home.path(), Format::Deb, &pkexec);
    let (mut prepared, _feed) = package_prepared(&installation);
    // the identity file changes once the app's own check has passed: the prompt's script still refuses
    let script = home.path().join("prompter-changing");
    let identity = installation.root().join(IDENTITY);
    program(
        &script,
        &format!("#!/bin/sh\necho changed > '{}'\n{pkexec}\n", identity.display()),
    )
    .unwrap();
    prepared.installation.prompter = script;
    let error = prepared.install().unwrap_err();
    assert!(error.to_string().contains("updated by something else"), "{error}");
    // the package changes after it was verified
    fs::write(&identity, "0.1.0.5\n").unwrap();
    let script = home.path().join("prompter-tampering");
    program(
        &script,
        &format!(
            "#!/bin/sh\necho tampered > '{}'\n{pkexec}\n",
            prepared.candidate.display()
        ),
    )
    .unwrap();
    prepared.installation.prompter = script;
    let before = sha256(&fs::read(&prepared.candidate).unwrap());
    let error = prepared.install().unwrap_err();
    assert!(
        error.to_string().contains("changed after it was verified"),
        "{error} ({before})"
    );
}

#[test]
fn a_failed_swap_and_rollback_keep_the_previous_install_after_prepared_is_dropped() {
    let home = tempfile::tempdir().unwrap();
    let installation = archive_installation(home.path(), "0.1.0.5");
    let feed = Feed::start().unwrap();
    let asset = Format::Archive.asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, &archive_bytes("0.1.0.6", |_| {}).unwrap());
    let source = Source::local(&feed.base);
    let release = source.check(&installation, version("0.1.0.5")).unwrap().unwrap();
    let mut prepared = source.prepare(&installation, &release, |_, _| {}).unwrap();
    let stage = prepared.work.as_ref().unwrap().path().to_path_buf();
    let mut renames = 0;
    let error = prepared
        .install_with_rename(|from, to| {
            renames += 1;
            if renames == 1 {
                fs::rename(from, to)
            } else {
                Err(std::io::Error::other("injected rename failure"))
            }
        })
        .unwrap_err();
    assert_eq!(renames, 3, "both the swap and rollback were attempted");
    assert!(
        error
            .to_string()
            .contains(&stage.join("previous").display().to_string())
    );
    assert!(!installation.root().exists());
    drop(prepared);
    assert_eq!(
        fs::read_to_string(stage.join("previous").join(IDENTITY)).unwrap(),
        "0.1.0.5\n"
    );
    assert!(stage.join(crate::stage::RECOVERY).is_file());
    assert_eq!(
        installation.cleanup_stale(),
        0,
        "startup must preserve recovery files too"
    );
    assert!(stage.join("previous/usr/bin/ngteams").is_file());
}

#[test]
fn a_package_changed_before_the_prompt_is_refused_against_the_verified_digest() {
    let home = tempfile::tempdir().unwrap();
    // The actual privileged script runs as this user and must refuse before any package manager runs.
    let installation = package_installation(home.path(), Format::Deb, &as_this_user(home.path()));
    let (mut prepared, _feed) = package_prepared(&installation);
    fs::write(&prepared.candidate, "replaced while waiting for a restart").unwrap();
    let error = prepared.install().unwrap_err();
    assert!(error.to_string().contains("changed after it was verified"), "{error}");
    assert_eq!(
        fs::read_to_string(installation.root().join(IDENTITY)).unwrap(),
        "0.1.0.5\n"
    );
}
