# Continuous integration

The [Coverage workflow](../.github/workflows/coverage.yml) and
[Windows portability workflow](../.github/workflows/windows.yml) skip pull requests and pushes to `main` whose
changes are entirely Markdown (`**/*.md`) or under `docs/`. This includes `README.md`, `AGENTS.md` and skill prose.
Files under any `tests/fixtures/` directory and `vendor/gpui-pre/README.md` remain included: tests consume fixtures
and GPUI compiles its README with `include_str!`. Licenses remain included because packaging reads them. A mixed
documentation/code change runs both workflows. Merge queues and manual dispatch remain unfiltered, as does
Coverage's reusable `workflow_call` entry point used by releases.

The small [Fixture and documentation guard workflow](../.github/workflows/fixtures.yml) still runs
`cargo test --locked -p fixture-guard` when `docs/`, any `tests/fixtures/`, the guard crate, the root manifest/lockfile,
the toolchain, Cargo configuration or its workflow changes. This preserves secret scanning of documentation and
fixtures and synthetic-marker checks without compiling the native client. Root Markdown and skill prose alone
run no workflow; a change confined to `docs/` runs only this guard. No other check reads those documentation paths:
`scripts/lint-report.sh` reads Rust sources; packaging tests read generated documentation and repository licenses.

Main currently has no branch protection or applicable rulesets requiring these checks. Before requiring a
path-filtered check, replace workflow filtering with an always-triggered workflow and job-level changed-file
conditions: [skipped workflows leave required checks pending](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow#using-filters-to-target-specific-paths-for-pull-request-or-push-events).

Coverage's `Coverage ≥90%` job runs `scripts/coverage.sh --require-target` over all runtime crates
selected by `coverage-report.py list`. A crate
below 90% in **any** of LLVM's lines, functions or regions fails the job, even if the workspace average or that
crate's saved standalone baseline is higher or lower. Exactly 90% passes. Failed tests, missing dependencies and
missing source mappings also fail; the workflow does not skip packages or tolerate a failed coverage step.
The hardware DMA subcase is excluded in GitHub Actions, as described below; its runtime source still counts.


The workspace has one client, `ngteams`; its UI tests run against synthetic services in `ui-sandbox`.
`proto-lab` tests browser capture/hold behavior against local pages. Typed-construction compile contracts live in
`teams-proto`; production-only mock-origin/registrar contracts live in `teams-trouter`. No tenant account or native
lab session is part of coverage. The lab runner is checked separately by `scripts/test_run_lab.py` using synthetic
commands, locks and holders.

The hosted `Coverage gate tests` job checks the reporter and the wrapper's failure propagation, including 89.99%
in each metric and a second crate whose coverage is lower than the first one's. This fast job does not replace the
full measurement. The coverage artifact contains only `summary.json`, including per-crate counts and gaps. Raw
profiles, screenshots, captures and runtime logs are not uploaded.
The job summary displays each crate's measured lines, functions and regions. If tests fail, the wrapper attempts
to export the current profiles for diagnosis and labels those results as partial; it still exits with the original
test failure. A failed build or export cannot reuse a previous measurement's summary.

## Hosted CPU runner

Both jobs use GitHub-hosted Ubuntu runners. The full measurement runs in the
[Arch test image](../.github/ci/Dockerfile), which supplies WPE WebKit ≥2.54, Weston ≥15, niri, PipeWire, the private
keyring tools and Mesa's software OpenGL/Vulkan drivers. Tests run as a non-root user. The container is privileged
so WebKit can create its own sandbox namespaces, and `/dev/dri` is hidden explicitly. It runs on a disposable
hosted VM, with only the checkout mounted; no self-hosted runner, personal files or tenant credentials are used.
The entrypoint also creates a fresh device filesystem: privileged container runtimes can otherwise repopulate DRM
nodes after mounting `/dev/dri`. Only the container's shared-memory mount is carried into that device filesystem.
Ubuntu 24.04 also restricts unprivileged user namespaces on the host, including those requested from within the
container. The workflow disables that restriction only for the disposable VM's current boot, using the setting
documented in [Ubuntu's release notes](https://documentation.ubuntu.com/release-notes/24.04/#unprivileged-user-namespace-restrictions).
Chrome's and WebKit's internal sandboxes remain enabled. Before compiling Rust, a preflight checks user-ID mapping
and bubblewrap; after installing Chrome, it renders a synthetic page with no network or desktop access. These
checks fail early with diagnostics if the hosted environment cannot support the browser tests.

`TEAMS_WPE_TEST_SKIP_DMA=1` excludes only the native WPE test's real DMA-buffer subcase. Its worker and in-process
SHM cases still run, as do the other native GUI, media and browser tests. Local runs leave this variable unset and
continue requiring actual DMA frames. This flag excludes a hardware test, not source files: the 90% gate still
measures all runtime crates and keeps the DMA code in the denominator.

CI selects `WGPU_BACKEND=vulkan` so GPUI uses Mesa's Lavapipe software adapter without probing unavailable GLES
devices. The sandbox forwards this graphics setting alongside its existing small environment allowlist.

The image installs the repository's pinned Rust toolchain and LLVM tools plus `cargo-llvm-cov` 0.9.1. The entry
script downloads checksum-verified Chrome using the normal lab CLI, then executes the full strict coverage gate.
Before measurement it also runs the patched Wayland clipboard's ownership regressions; these vendor-library tests
are separate from the runtime-crate selection. The native UI test still exercises copy, cut and paste end to end.
The image sets `TZ=Etc/UTC` so calendar fixtures do not depend on the host's `/etc/localtime` configuration.
Tool installation uses the network; test traffic stays synthetic and offline from the tenant. No lab configuration
or secret is required. The workflow frees unused hosted SDKs to make space for the instrumented native binaries.
The one-shot build disables incremental caches and DWARF debug information while retaining LLVM coverage maps and
debug assertions. It also removes the temporary Chrome-installer build before compiling instrumented tests.
A historical instrumented build with these settings measured about 23 GiB locally, before temporary test outputs;
actual size depends on the selected sources and dependencies.

To block merges on coverage failures, first apply the required-check pattern above, then make `Coverage ≥90%`
a required status check in branch rules; the hosted reporter tests alone are not a coverage measurement.

## Windows portability

The [Windows portability workflow](../.github/workflows/windows.yml) checks, lints and runs the offline tests of
`teams-platform`, `teams-log`, `teams-store`, `teams-auth`, `teams-ctl` and `teams-runner` on Windows Server 2025
(`windows-2025`) and Linux; local verification also runs on the Windows 11 VM.
These are the foundational-library gates. The complete native app has separate VM contracts and a hosted Windows
release job. These portability jobs do not build or launch `ngteams` or access a tenant.

`teams-platform::private` creates files and directories with Unix modes or native protected Windows ACLs.
Windows tests inspect actual ACLs for files, log rotations, databases, WAL/SHM, run locks, picture caches and
snapshots; the shared SQLite/outbox tests also run natively. Only Unix mode-bit, symlink and Linux `/proc` lock
inspection tests are platform gated. Invalid Unicode tests use unpaired UTF-16 on Windows and invalid UTF-8 on Unix.
Windows filenames cannot contain `?`, so the portable snapshot path test covers the legal URI characters `%` and
`#`; source URI paths normalize Windows directory separators before SQLite opens them.

SQLite's Windows side files use the creating process token's default owner (the Administrators group under an
elevated token); their inherited DACL still grants only the current user and SYSTEM access. The Windows test checks
that owner against the actual process token and verifies both grants. Files created through `teams-platform`
explicitly name the current user as owner. See [Windows object ownership](https://learn.microsoft.com/en-us/windows/win32/secauthz/owner-of-a-new-object).

The Linux coverage inventory records the Windows-only filesystem source with the existing hash-checked
`unmapped_sources` mechanism: it has no Linux LLVM mapping. Native Windows tests exercise that source in the VM
and hosted job; this entry neither removes mapped counts nor changes any existing coverage floor.

Native exports use the same source inventory and saved per-crate floors. When inspecting a VM export on another
machine, run `python3 scripts/coverage-report.py report <export.json> --output <summary.json> --packages proto-lab
--platform windows --source-root C:/src/ngteams-windows-port-3` (as one command). The explicit source root rebases
only that checkout's filenames; counts stay unchanged. `unmapped_sources_windows` records individually hashed
Unix-only lab sources and their cfg/module roots. These reviews apply only to the Windows audit: Linux still
requires those sources, an edited source or module root invalidates its review, and any supplied LLVM mapping
always counts, including zero-hit code. Missing compiled sources still fail the audit. The restricted Windows
lab CLI does not compile the Unix capture/scenario command set; it retains only sign-in, provisioning and reach
evidence plus their browser driver.
Authentication uses Secret Service on Unix and Windows Credential Manager on Windows. The Windows backend selects
its native store explicitly, persists only on this machine, and hashes length-delimited application/account/audience
keys to avoid Credential Manager's case-insensitive-name collisions. Tests use unique synthetic namespaces and exact
entry operations, never enumeration. Third-party credential errors can contain secret bytes, so the adapter reduces
them to fixed error categories and zeroes secret buffers. The keyring-core debug records contain entry metadata,
not values; the app logger ignores this third-party target. Interprocess is used only in byte mode (its message-mode
receive diagnostic is unused and prints a mode number only).

Windows Credential Manager limits an individual blob to 2,560 bytes; an authz skypetoken/region record can exceed
that limit. The adapter retains legacy raw-item reads and stores new values as bounded pieces, under the same
hashed application/account/audience namespace. One root write commits a random generation and a SHA-256 digest;
readers never combine generations. A small pending journal recovers interrupted staging without enumerating any
credentials, and the committed manifest retains the prior generation until cleanup can succeed. A per-key native
file lock serializes distinct store instances/processes; its file contains no credential data. Synthetic tests cover
native large/multibyte values, reopen, replacement, concurrent instances, corruption, missing pieces and interruption
at every mutation of an update. The VM runs the native store tests only under its console token.

Broker scope, get-only behavior, framing, closure and session contracts run unchanged over Unix sockets or Windows
named pipes. The caller supplies the established stream and is responsible for endpoint permissions.
The Windows runner now supervises broker handles and the complete app process tree. The normal lab app builds;
signed-in validation remains a separate gate. Named pipes have no half-close, so malformed-request tests close the
complete synthetic peer when testing truncation.

The VM's SSH token cannot access Credential Manager (Windows error 1312); native credential tests and coverage run
through a temporary scheduled task in its existing interactive console session. No user credential is read or used.
Hosted Windows tests likewise require the runner's interactive user profile. Linux uses a private D-Bus and temporary
keyring, with the job installing the test daemon dependencies. The Windows-only credential adapter has a hash-checked
unmapped-source entry for Linux measurement and is measured natively in Windows.

The per-crate Linux gate includes `teams-platform` at 90%. The Windows job additionally measures that crate natively
with `cargo-llvm-cov` 0.9.1 and requires 90% in lines, functions and regions, including its Win32 implementation.
It also measures `teams-auth` natively and enforces that crate's existing floors: 96% lines, 96% functions and
94% regions, including the Windows credential backend.

Space-limited native measurements can build test targets individually without PDB files. Set
`CARGO_TARGET_DIR` to the instrumented directory before applying `cargo llvm-cov show-env --pwsh`; its output is
PowerShell code, with escaped environment values. Discover every enabled library, binary and integration test
harness for the selected crate from `cargo metadata`. Build each using `cargo rustc --profile test` with `--lib`,
`--bin` or `--test` and the final Rust arguments `-- -C link-arg=/DEBUG:NONE`. Accept only Cargo artifacts with
`profile.test = true`, run every discovered harness, and retain their executables and profiles for the report.
This changes only the test link's debug symbols: LLVM's coverage mappings remain in the executable. A native
comparison exported byte-identical coverage summaries with and without the same test's PDB.

Keep the usual workspace-artifact cleanup before a fresh measurement, the complete source audit and the saved
per-crate floors. A resumed measurement must preserve successful harness results and profiles, run every remaining
target, and reject a failed build or test even if its partial coverage is high enough. This technique does not
build a runnable client or replace the lab runner.

Control IPC uses Unix sockets or local-only byte pipes on Windows. Pipes are created with a protected DACL granting
only the exact current user and SYSTEM full access; a mandatory label refuses lower-integrity access. Clients verify
the server process's user before sending their first byte. Names hash the user's SID and the logical endpoint's
UTF-16 path, preserving mode and profile separation without a lossy Unicode conversion. Windows client paths use
`LOCALAPPDATA` for state/runtime and `USERPROFILE` for the home fallback; `NGTEAMS_CTL_SOCKET` still overrides the
logical endpoint. The pipe name is never accepted from an untrusted remote hostname.

Native tests inspect a live pipe's owner, grants and integrity label, exercise bounded connects/reads/writes,
cloned shutdown, large transfers, protocol/CLI contracts and events. The GPUI-independent **actual app listener**
is compiled directly by `teams-ctl/tests/app_listener.rs` and tested on both platforms, including subscription
cleanup. This does not build or launch the GUI. Windows uses nonblocking byte operations and monotonic deadlines;
writes are capped to the queried outgoing pipe quota so an oversized NOWAIT write cannot stall an empty pipe.
`FlushFileBuffers` is never used in the control protocol, because it waits for the peer to read without a deadline.
The native Windows gate also enforces the control crate's unchanged floors from `coverage.toml` (99% lines,
100% functions, 98% regions). Platform-only sources use the same hash-checked Linux inventory mechanism above.


The native offline gate also compiles the app's actual descriptor adoption and restart modules through
`teams-auth/tests/app_descriptors.rs`, and its actual location/overlap module through
`teams-platform/tests/app_paths.rs`. Broker pairs use protected, local-only byte pipes whose temporary listener is
removed before a child launches. The pair verifies both peers are the creating process. A native transfer set owns
fresh inheritable duplicates and passes an explicit handle whitelist to `CreateProcessW`; originals and working
clones remain noninheritable. Synthetic subprocesses exercise quoted arguments, unwanted-handle exclusion, writable
trace validation, failed restarts and successful broker/trace transfer across a restart. Windows restarts create the
successor before exiting, so the native runner tracks the entire app job, including every successor.

Windows account locations resolve under `LOCALAPPDATA/ngteams/{config,data}` with an absolute
`USERPROFILE/AppData/Local` fallback; lab aliases use the sibling `LOCALAPPDATA/ngteams-lab/<alias>` root. Snapshot
separation compares Windows components with native ordinal case-insensitivity, recognizes ordinary/verbatim drive
and UNC spellings, and conservatively treats trailing dots/spaces as aliases. Relative paths resolve from the current
working directory before interpreting `..`; an unresolved Windows drive-relative path cannot prove separation.
The existing native coverage floors remain unchanged. Windows-only sources use the hash-checked Linux inventory.
The explicit VM-console guard also checks the active interactive session; its tests open no window. These boundaries
supply the Windows runner. Native WebView2, the complete media runtime and the application have VM contracts;
signed-in feature validation is separate from offline checks. The release workflow packages the Windows binaries.

The app and UI all-target checks include real native handle, byte-pipe and file-privacy contracts, rather than
compiling Unix-only test helpers on Windows. `scripts/test-windows-app.ps1` runs the synthetic application/startup,
IPC, account, settings and local-tool model/view suites in the authorized VM console; Git for Windows interprets
only the fake CLI protocols behind native `.cmd` shims. Shared screenshot scenes also run against native Windows
GPUI through a console/child-verified capture boundary.
Wayland input, portal/D-Bus and Linux runner harnesses and the separate native Windows desktop/shell/video fixtures
remain platform-specific. No coverage floor is changed.


The native runner gate measures its Windows CLI, locks, snapshot cleanup, deadlines, broker/app failures and restart
supervision with synthetic executables. Its non-console smoke opens no GUI or live client. The additional console
scheduling smoke runs in the authorized win11 VM, also with a synthetic executable. `teams-platform` tests prove
atomic job assignment, descendant retention after the original process exits, explicit termination and kill-on-close.
Windows-only runner/job sources have hash-checked Linux inventory entries and are measured by the native gate.


## macOS portability

The [macOS portability workflow](../.github/workflows/macos.yml) runs on `macos-latest` (Apple silicon) for pushes to
`main`, pull requests and merge groups. It installs GStreamer's pinned official macOS runtime into the runner's temp
dir (`scripts/macos-gstreamer.sh`, no admin), then formats, checks and lints every workspace target (the Linux-only
crates compile to nothing there) and runs the offline tests of the crates with macOS code of their own: files, logs,
stores, authentication with the Keychain store's round trip against the runner's unlocked login keychain
(`NGTEAMS_KEYCHAIN_TESTS=1`), the control socket and CLI, the runner, updates, the sign-in guard and the native device
backend's catalog, and the macOS notifications' tags and actions (`teams-ui`'s `desktop::macos`, no notification
center reached). Builds use `CARGO_INCREMENTAL=0` and no debug info to fit the runner's disk.

Coverage is measured natively as on Windows: `teams-platform` at 90% in lines, functions and regions, `teams-auth` and
`teams-ctl` at their `coverage.toml` floors with their macOS code included. The macOS-only sources (the Keychain store,
the sign-in worker, the notifications and Dock badge, the menu bar and the delegate's terminate) have no Linux LLVM
mapping and are listed under `unmapped_sources` with the reason; the sign-in
window itself and the app's windows run on the Mac console through `scripts/run-lab-macos.py`
([development guide](development.md#macos-host-runner)).

## Releases

The [release workflow](../.github/workflows/release.yml) runs on a push to `main` that changes what ends up in a
package (its `paths` list: code, dependencies, packaging and release scripts, not docs, skills or CI settings) and on
manual dispatch. It runs the full suite (the Coverage workflow above, called as its `tests` job, plus every
`scripts/test_*.py`) and publishes only if that passes. A new push cancels any release still building or waiting, so
only the newest commit is published. A successful `main` run makes a GitHub release tagged
`v<workspace version>.<run number>` (`Cargo.toml`'s `[workspace.package] version` and `github.run_number`, monotonic
for package upgrades), marked latest, targeting the pushed commit. Linux x86_64 builds on `ubuntu-24.04` produce
`.deb`, `.rpm`, Arch `.pkg.tar.zst`, `.AppImage` and a portable `.tar.gz`; `windows-2025` produces
`ngteams-<version>-windows-x86_64.zip` and the per-user installer `ngteams-<version>-windows-x86_64-setup.exe`, both
from the same staged, hash-checked folder; `macos-latest` produces `ngteams.app` for Apple silicon as
`ngteams-<version>-macos-arm64.zip` and `.dmg` (`scripts/package-macos.sh`, GStreamer's runtime inside). `SHA256SUMS` and GitHub build-provenance attestations cover every asset,
including the runtime below. Publishing requires the tests and both native build jobs and runs only on `main`; a
manual branch dispatch builds artifacts without publishing. The `macos` job checks the zip's app before it uploads
it: its code signature, its release marker and version, no link to the build's GStreamer prefix, and the display-free
`--build-identity` and `ngteamsctl --help`.

Packages carry no web engine: `ngteams-web` picks the system's WPE WebKit (2.54 or newer) or a downloaded runtime when
it starts. Only `ngteams-web` and the sign-in extension link the engine; `scripts/check-linux-package.py` fails a tree
where `ngteams` or `ngteamsctl` needs one of its libraries or a host library not on its list, any shipped file has an
RPATH or RUNPATH, or the tree holds a library at all. The Arch package depends on the system's engine, deb and rpm
recommend it, and the runtime is the fallback, so the `.deb` and `.rpm` work where the distribution has none.

The runtime is `ngteams-web-engine-<key>-linux-x86_64.tar.zst` (MANIFEST with `key`, `arch` and `wpe_version`,
`lib/`, `lib/wpe-webkit-2.0/`, `lib/gio/modules`, `lib/gstreamer-1.0`, `libexec/gstreamer-1.0/gst-plugin-scanner`).
It is built from ngterm's portable WPE prefix (`scripts/release-wpe-pull.sh`: pulled from GHCR by digest with pinned,
checksum-verified ORAS, unpacked only after `gh attestation verify` ties it to ngterm's `release-wpe-cache.yml` on
`main`; WPE is never built here) plus Ubuntu's GStreamer plugins, GIO modules and library closure (`scripts/web-engine-closure.py`).
Its key hashes the pinned digest, the exact Ubuntu package versions copied and the packaging inputs
(`packaging/linux/gstreamer-plugins.txt`, `runtime-libraries.txt`, and the packaging scripts). It is published once,
on its own `web-engine-<key>` prerelease (never latest), and reused while the key is unchanged: a release compiles
that exact file's URL, size and SHA-256 into the app, so a published archive is never replaced.

The build compiles the release's version into the app (`NGTEAMS_RELEASE_VERSION`), and each package carries
`usr/share/ngteams/release` (that version) and `usr/share/ngteams/update-format` (how it installs: `linux-archive`,
`linux-appimage`, `linux-deb`, `linux-rpm`, `linux-pacman`). The installed app's updater (`crates/teams-update`) reads
them to pick the asset of a newer release that replaces it. On Windows the installer writes `share/ngteams/release`
and `share/ngteams/update-format` (`windows-setup`) under its folder; the zip carries neither, so a copy installed
from it doesn't update itself. The macOS bundle carries `Contents/Resources/ngteams/release` and `update-format`
(`macos-app`): its update unpacks the next zip with `ditto` beside the installed bundle, checks its marker, version,
programs and code signature (`codesign --verify --deep --strict`), and swaps the bundles by two renames.

### The Windows installer

`packaging/windows/ngteams.iss` is an [Inno Setup](https://jrsoftware.org/isinfo.php) 7 script, compiled by
`scripts/package-release-windows.ps1 -InnoSetup <ISCC.exe>` after the zip's stage has passed `windows-release.py
check`. The compiler is installed per user from the hash-pinned release in `packaging/windows/installer-pins.json`
(`scripts/release-innosetup.ps1`); to update it, change the version, URL and SHA-256 there together. Inno Setup was
chosen over an MSI because it installs per user without elevation natively, compares all four parts of our
`MAJOR.MINOR.PATCH.RUN` version (an MSI ignores the fourth, the one every release bumps), upgrades in place by
running the newer installer, and makes one compressed `.exe` from the ~2,000-file media runtime with no per-file
component authoring.

It installs under `%LOCALAPPDATA%\Programs\ngteams` with a Start menu shortcut carrying the app's notification
identity, an Add/Remove programs entry and the `msteams:` handler offered under Default apps, all in `HKCU`. An upgrade
first moves the previous version's program folders aside into `previous-version` inside the installation (only in a
folder an earlier ngteams installer marked; the packaging script refuses a stage with a top-level name the script's
`ProgramItems` doesn't list), removes them once the new version is in, and puts them back when it fails; it stops
before moving anything while a process runs from the installation. The next run finishes an upgrade cut short, or a
removal of the previous version that failed on a held file, from where the marker is, and refuses a folder holding a
`previous-version` that neither it nor the installation marks. An older installer is refused unless run with
`/ALLOWDOWNGRADE`, and one installer runs at a time. Uninstall removes the
program, the app's own notification registration, and the shortcut and `msteams:` handler unless they now open
another copy (a zip's), and keeps `%LOCALAPPDATA%\ngteams` and Credential Manager. The app's updater runs the installer
with `/VERYSILENT /UPDATEPID=<pid> /DIR=<folder>`: it waits for the app to exit, upgrades, and starts it again, the previous
version when the upgrade fails. The `windows` job installs and uninstalls it on the
runner with `scripts/test-windows-installer.ps1` (never starting the app) and checks every installed program file
against the zip's `PACKAGE-MANIFEST.json`.

### Windows code signing

Nothing is signed yet, so SmartScreen may warn on the installer and the app (the README says how to proceed). The
hook is `scripts/package-release-windows.ps1 -SignCommand '<command>'`: an Inno Setup sign tool command line, `$f`
for the quoted file and `$q` for a quote, for example `signtool.exe sign /fd sha256 /td sha256 /tr <timestamp URL>
/f <certificate> $f`. It signs `ngteams.exe`, `ngteams-webview.exe` and `ngteamsctl.exe` before they are staged (so
the manifest hashes the signed files), and Inno Setup signs the installer and its uninstaller (`SignTool`,
`SignedUninstaller`). To turn it on, add a step that provides the certificate (an Azure Trusted Signing or HSM-backed
`signtool`, say) from a secret available only to `main`, and pass `-SignCommand` in the workflow's packaging step.

### macOS signing

Without secrets the `macos` job signs the app ad hoc: macOS asks before it first opens it (right-click → **Open**),
and the Keychain asks to allow access again after each update, because an ad hoc signature changes with every build.
With a Developer ID the app is signed with the hardened runtime, a secure timestamp and
`packaging/macos/ngteams.entitlements` (camera, microphone), then notarized and stapled. The job takes these
repository secrets; any left unset leaves the ad hoc path:

- `MACOS_DEVELOPER_ID_P12`: the Developer ID Application certificate and its private key, as a base64 `.p12`;
  `MACOS_DEVELOPER_ID_PASSWORD`: its password. They go into a keychain of the job's own, deleted when it ends, and
  `scripts/package-macos.sh` signs with that identity (`NGTEAMS_MACOS_SIGN_IDENTITY`).
- `MACOS_NOTARY_KEY_P8` (the App Store Connect API key's `.p8` text), `MACOS_NOTARY_KEY_ID` and
  `MACOS_NOTARY_ISSUER`: `xcrun notarytool` submits the app and then the dmg, and `xcrun stapler` staples each ticket.
  Notarization runs only with a Developer ID.

`build` runs the tests of the packaging scripts, builds, packages and smoke-tests: the build identity in the archive,
the AppImage and the installed `.deb`, `--install-web-engine` into a private `XDG_DATA_HOME`, `ngteams-web --probe`
on that installed runtime with the build's WPE prefix moved away, and `apt` installing the `.deb`. It holds only a
read token; artifacts move to `publish`, which alone may write.

### Updating the pinned WPE prefix

ngterm's cache workflow publishes a new prefix when its WPE build inputs change. To move to it, take its tag:
`ubuntu-24.04-` followed by the output of `scripts/build-release-wpe.sh --print-key` in ngterm. Resolve its manifest
digest (the tag's anonymous registry answer's `Docker-Content-Digest`), and put the tag and digest in [`pins.env`](../packaging/release/pins.env), keeping
`RELEASE_WPE_PREFIX` as the path it was built at. The runtime's key changes with it, so the next release publishes a new runtime.

### Local checks

`scripts/test_web_engine_closure.py` and `scripts/test_check_linux_package.py` run under
`python3 -B -m unittest discover -s scripts -p 'test_*.py'`; they need `cc`, `ldd`, `patchelf` and `zstd`.

`scripts/test_windows_media_closure.py` builds tiny real PE fixtures with clang and Rust's LLD, and reads them with
LLVM's `llvm-readobj` (`llvm-tools-preview`). It checks delay/transitive imports, Windows' case-insensitive DLL
names, unresolved and ambiguous dependencies, architecture, and missing/corrupt/extra packaged files. The native
runtime gate is `scripts/test-windows-media.ps1`, using the samples made by `scripts/make-media-samples.py` and the
same manifest/plugin/DLL/license closure as packaging. It runs only in the authorized Windows VM console, with no
account or physical capture. Signed-in flows and physical devices remain separate gates.


The Windows media backend uses GStreamer's WASAPI2 and Media Foundation providers and `windows-capture`'s system
picker for Windows Graphics Capture. Synthetic GStreamer sources exercise capture, playback queues, catalog
changes, fallback and restoration without opening hardware or reaching a tenant. Hardware runs still need the
explicit VM console sandbox; the Windows picker/D3D capture source cannot execute on Linux. Linux retains PipeWire
and portal capture. The complete application and release package have their own checks below.

The pinned Windows libwebrtc archive uses the static MSVC runtime (`/MT`). `.cargo/config.toml` selects
`target-feature=+crt-static` for Windows x64 so Rust and cc-rs' C++ bridge use that same ABI; mixing `/MD` bridge
objects with the archive fails the linker's runtime checks. An explicit replacement `RUSTFLAGS` must retain that
setting. GStreamer continues using its DLLs and their own allocation/free APIs.


Windows' native file-device relay contracts cover audio, camera video and screen sharing without a tenant. The
`test-support` TURN fixture retains its loopback server; a Windows-only UDP adapter accepts only traffic from its
own host address because Winsock rejects libwebrtc's interface-bound socket sending directly to 127/8 (10049).
No production transport or ICE policy changes. The ignored system-picker contract requires a solid dark-blue fixture window and an explicit picker selection.
The automated fixture/driver is still missing ([backlog](backlog/windows-screen-picker-contract.md)); the ordinary
native media gate does not execute that ignored case or establish BGRA/idle-cleanup coverage for the picker.


The Windows portability workflow separately compiles/lints every `teams-signin` target and runs its display-free
classifier, host-gate and private-worker rejection contracts. Its WebView2/STA callbacks require the authorized VM
console and are exercised by `scripts/test-windows-signin.ps1`, using synthetic HTTPS and no account. Windows-only
sources are hash-audited in the Linux inventory; native whole-application coverage remains part of the final port
gates. Wry's optional tracing features are disabled, the app logger filters third-party targets, and the worker's
stderr goes to NUL. Only fixed Chromium error codes are forwarded; guard decisions are logged in the parent.

Native sign-in coverage runs the real offline window as well as synthetic private-worker conversations and COM
event objects. Registry tests write only below a disposable application key, never the actual Edge policy roots.
In a coverage build, the offline page requests one LLVM profile dump after its guard probes finish and waits for
the worker's acknowledgement before redirecting. The parent intentionally kills that worker tree after catching
the redirect, so waiting until process exit would lose its counters. This hook lives only in the test fixture;
it neither changes the shipped worker nor resets counters, and a failed dump fails the fixture. Hash-reviewed
Unix-only sign-in sources remain required in the Linux inventory; every mapped Windows gap still counts.

Windows desktop shell contracts require visible message/call toasts and actual body/Answer/Decline COM activation,
reject stale/foreign arguments and use real WinRT history (replacement, withdrawal and shutdown cleanup),
Shell_NotifyIcon and ITaskbarList3 in the authorized VM console. The native transport sources are hash-audited in
the Linux coverage inventory because those APIs have no Linux mapping; native whole-application coverage remains
a final Windows-port gate. Linux still measures and exercises the freedesktop notification/tray transports and the
shared multi-account notification and unread models, with unchanged coverage floors.

`scripts/test-windows-shell.ps1` adds the real GPUI Desktop over synthetic accounts in the authorized VM console:
minimize/activation, native tray/menu input, close/reopen, selected account/theme, settings/chat routing and unread
taskbar overlays. A fixture-owned icon is removed and restored through a synchronous `TaskbarCreated` message;
no real Explorer restart or account is needed. The captures contain the verified native controls and synthetic
window, including the app's taskbar button before/after the unread count changes and its window is recreated.


The release workflow builds Windows and Linux artifacts under read-only tokens, and its main-only publish job
checksums and attests both platforms before creating the release. Windows packaging uses the same verified media
closure as the lab runner, with all three executables beside its DLLs and a full-package hash manifest. Real PE
package tests and an unpacked-archive identity/manifest check run without an account or GUI. Local VM packaging
uses lab binaries only; install-marked builds remain exclusive to the hosted release job and explicit personal
install requests. A branch dispatch cannot publish a release.
