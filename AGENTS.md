# ngteams

A native Microsoft Teams client for Linux in Rust + GPUI, written against Teams' internal APIs with the web client as
the reference. `ngteams` is the only client. Agents test it with dedicated lab accounts; the installed build runs the
user's own account. `proto-lab` captures the web client and explores its protocol. Layout, commands and tests:
[docs/development.md](docs/development.md).

## Hard rules

These protect real people, the user's account and the user's data. They hold for every agent and every change.

- **Lab accounts talk and call only each other, the echo service and the user's own account**, both ways.
  Before every live run, list the chats, calls and people it reaches and confirm that all are in that set
  ([docs/lab-rules.md](docs/lab-rules.md)). Check the people on each target and anyone the run adds; if that cannot be
  established, do not run it. Apps and bots aren't people. This is the agent's check, not an application policy.
- **Agents build `ngteams` only through `scripts/run-lab.sh` (Windows: `scripts/run-lab.ps1`; macOS:
  `scripts/run-lab-macos.py`)**, including their build-only modes. Drive lab accounts freely in that runner's UI
  sandbox, within the lab-contact rule. The personal installer is used only on an explicit user request, as below.
- **No live write ever comes from a `cargo test` process**: live tests spawn the normally built lab client.
- **Captures, tokens and debug logs never enter the repo**, not even sanitized. Committed fixtures are synthetic and
  marked ([development guide](docs/development.md#captures-and-fixtures)). Credentials never reach logs or stdout; print only aud, exp,
  scopes and tenant. At `debug`, every API request and response and every Trouter frame is logged in full, with
  credentials redacted by the logger; `info` stays payload-free.
- **Tokens go only to their account's cloud and service**: Microsoft-account tokens go only where Teams sends them
  for the same user action, to the Microsoft service each was issued for, never to an own server, third-party bot or
  app backend. An own-server account's tokens go only to that account's own server.
- **Agents run the user's own account only read-only, and install it only when the user asks**
  ([details](docs/personal-build.md#what-agents-may-do-with-the-personal-build)):
  - start it only through `scripts/run-personal-read-only.sh` (Windows: `scripts/run-personal-read-only.ps1`);
    its data dirs (`~/.local/share/ngteams/`, `~/.config/ngteams/`) are readable any time;
  - run `scripts/install.sh` only on the user's explicit request, each time; afterwards start the installed
    `ngteams` detached if asked, and leave it alone;
  - never sign it in, drive, test or capture with it in sending mode, touch its keyring items or receive the user's
    tokens.
- **Every lab or test GUI runs in `crates/ui-sandbox`**, never on the user's display, bus or PipeWire. On Windows,
  the explicitly authorized `win11` VM's interactive console is the sandbox, through the PowerShell runners.
  On macOS, the Mac's logged-in console is the sandbox, only through `scripts/run-lab-macos.py`: lab-marked builds;
  lab accounts within the lab-contact rule (or a synthetic alias); never the user's own account or its Keychain
  items; short runs, every window and process closed after.
- **Protocol notes are written in our own words** in `docs/protocol/`; Microsoft JS never enters the repo.

## The client builds

- **The installed `ngteams` is the user's Teams** ([docs/personal-build.md](docs/personal-build.md)): it may do whatever
  the Teams client lets its user do, to any target. Design its features as any desktop Teams client would; the lab
  rule is the agent's responsibility during lab runs.
- The lab marker selects lab mode only: isolated alias directories, the `ngteams-lab` credential namespace and a UI
  sandbox. The install marker gates other modes; `.github/workflows/release.yml` builds with it too, on GitHub, never
  an agent's machine. A `test-support` build refuses to run.
- Mention a lab constraint only when the change touches lab code, in one sentence.

## Checks

CI requires 90% coverage per runtime crate ([CPU runner and hardware-test exception](docs/ci.md)). Choose additional
checks, waits and review from what a change can break, and stop once they cover that. A small change gets a small,
fast check (the test that exercises it, or `cargo check` of its crate), not every suite of every crate it touched; a
shared API, write-path or calling change earns more. Rerun only what failed. Say what you ran and what you left out.
Commands: [docs/development.md](docs/development.md#tests).

What the tooling enforces when it runs:

- clippy denies the workspace lints (`all`, `pedantic`, `unwrap`/`expect`/`panic`, the complexity limits in
  `clippy.toml`); `scripts/lint-report.sh` fails on an inner `#![allow]`/`#![expect]`.
- `cargo test -p fixture-guard` fails on an unmarked fixture or a secret in any `tests/fixtures/` or `docs/`.
- `scripts/coverage.sh --require-target` fails any runtime crate below 90% in lines, functions or regions; without
  that flag, the script checks the saved standalone floors in `coverage.toml`.
- A binary built with `test-support` (anything `cargo test`, `--all-targets` or `--all-features` built) refuses to run.

### Live runs

Live runs reach the real tenant, so these stay firm. Commands are in the development guide's lab section.

- State the lab-contact check before each live run. Do not widen whom the lab may reach without the user's sign-off.
- Use `scripts/run-lab.sh` for `ngteams`; it builds normally into `target/lab`, holds the live and alias locks and
  applies an outer timeout. Several aliases may run together. Stop each one when its run ends.
- Live tests build only with `--features live` and run only with `PROTO_LAB_LIVE=1`. Web captures use normally built
  `proto-lab` under the exclusive live lock and an outer timeout. Lab browsers run headless and muted, one per lab
  profile at a time, none left running.
- A scenario clicks answer, decline or join only when made for it (`ab-call`, `abc-call`), only on its own call.
- If a run reaches someone outside the allowed set, stop and retain its output. Report what happened and any later
  clean run separately. Do not raise a timeout or omit a reach check to make a run pass.

## Conventions

- Before building a feature or UI, look at how browser Teams does it and screenshot each step with a lab account
  (`proto-lab`); build from those shots, never invent it ([how](.agents/skills/teams-ui/SKILL.md#before-you-build)).
- Split a function over a `clippy.toml` limit; if it really can't be split, put an outer
  `#[expect(clippy::..., reason = "...")]` on that item. Thresholds only go down.
- Bump `rust-toolchain.toml` in a change of its own. Every crate (spikes too) has `[lints] workspace = true` and is in
  the root `members`; shared dependency versions live in `[workspace.dependencies]`.
- **Never edit a released store migration**: users' stores already ran it. Add a new one (`crates/teams-store`).
- Keep a value in a form no capture showed as unknown rather than guessing it (`teams_proto::chat::Field::Unknown`);
  a fixture covers a field's whole captured range.
- A new or changed endpoint updates its row in `docs/protocol/endpoints.md`.
- Check a new dependency that logs at `debug` or below against the compile-time log cap (`crates/teams-log`).

## Code comments

- No comments by default: the code explains itself through its names and structure.
- A comment says why, not what, in one short line, and only where the code can't say it.
- License headers, lint and tool directives, and the doc comments a public API or docs build needs are exempt, as
  are the module docs that carry a design invariant's why.

## Design invariants

Undoing one needs a new reason, not a cleanup; the why is in each path's module docs.

- The CDP client stays our own (`crates/proto-lab/src/cdp.rs`); capture holds keep their matching requests unsent.
- The transport is `https` only, no proxy or redirects, each token only to its own service. Typed writes are single
  attempts; the outbox owns the permitted retries (`crates/teams-proto/src/http.rs`).
- The outbox is the only write path; an unknown outcome is checked, never blindly resent; nothing is resent across
  runs without `Store::resend` (`crates/teams-core/src/outbox.rs`).
- One engine per account (`crates/teams-core/src/engine.rs`); nothing else in the process opens the store file.
- Deliveries are never answered except a call's typed response (`FrameSink::respond`); only a `CallAction` answers,
  declines or joins a call, and no view does it, or turns a camera on, by itself.
- No log line carries a payload except the app's debug log; the logger redacts credentials (`crates/teams-log`).
- No view builds a request: views reach the engine only through `Core` (`crates/teams-ui/src/bridge.rs`).
- Message text is drawn only from sanitized `RichText`; the UI kit is held to `crates/teams-ui/tests/reach.rs`; no Zed
  code is copied (GPL).
- Call media is libwebrtc's (`crates/teams-media/src/rtc.rs`): relay-only ICE, SDP only in Teams' dialect.
- The sign-in window (`crates/teams-signin`) is Microsoft's page in an isolated worker: WPE WebKit on Linux
  (`ngteams-web`, `crates/teams-wpe`), using the system's `wpewebkit` or a release's pinned, hash-verified runtime
  (`crates/teams-web-engine`); WebView2 on Windows (`ngteams-webview`), using Microsoft's installed runtime;
  WKWebView on macOS (`ngteams-webview`, `crates/teams-signin/src/macos`), where the guard decides navigations and the
  page's fetch/XHR and other subresource loads reach only the host gate's hosts (WKWebView has no per-request hook).
  `ngteams` itself links no WebKit. Every page request (on macOS, every navigation and fetch/XHR) is decided and
  logged by the guard, and the egress proxy is the page's only way out.

## Where things live

- [README.md](README.md): what the app is and how to install it, for end users.
- [docs/development.md](docs/development.md): layout, building, each tool's commands, the lab's files, captures, fixtures and tests.
- `docs/protocol/`: how each Teams service works, `endpoints.md`, `chat-service.md`. `docs/lab-rules.md`: the lab rule
  and per-run check. `docs/personal-build.md`: the installed client. `docs/backlog/`: deferred work. `docs/plans/`:
  plans as decided then, not the current design.
- Keep this file to rules and pointers: no dates, incidents or mechanism detail. A fix's why goes in a comment at the
  code, how a service works in `docs/protocol/`.
