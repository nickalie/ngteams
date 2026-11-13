# The personal build: its rules

`ngteams` is the user's Teams client. It may implement whatever the Teams client lets its user do, to any target;
the lab-contact rule does not constrain the user's own account. The app sends the user's actions, enabled settings
and the protocol follow-ups those actions require. The outbox, transport and call-state rules below still apply.

The same client also runs isolated lab accounts through `scripts/run-lab.sh`. Personal mode never reads
`lab.toml`, and no separate policy or decision log is required for a send. Current agent authorization is in
[AGENTS.md](../AGENTS.md); the unchanged lab reach rule and per-run check are in [lab-rules.md](lab-rules.md).
Earlier approvals and milestone implementations remain in [completed plans](plans/completed/) and acceptance
records; obsolete proposal mechanisms are not instructions for the current client.

## The binary and its data

| | user's normal app | native lab alias |
|---|---|---|
| entry point | installed `ngteams` | `scripts/run-lab.sh <alias>` starts `ngteams --lab <alias>` |
| config | `$XDG_CONFIG_HOME/ngteams/accounts.toml` | `$XDG_DATA_HOME/ngteams-lab/<alias>/accounts.toml` |
| data | `$XDG_DATA_HOME/ngteams/` | `$XDG_DATA_HOME/ngteams-lab/<alias>/` |
| credentials | Secret Service application `ngteams`, used in-process | `ngteams-lab`, one account's writable broker outside the sandbox |
| window, notifications, tray | user's desktop/session bus | private UI sandbox/session bus |
| call devices | desktop PipeWire and camera portal | sandbox PipeWire and fake camera portal when `--pipewire` is given |
| live lock | none, the user's own app | shared fixed `~/.local/share/teams-lab/live.lock`, exclusive per-alias lock |

Absolute XDG homes override the defaults `~/.config` and `~/.local/share`. Both app modes use private `0700`
directories and `0600` account/data files. The native lab alias's data and config share one root, independent of the
browser harness's `data_dir`. Lab paths resolving into personal data/config are refused.

`accounts.toml` holds `[[account]]` entries, not tokens: UPN, verified object/tenant IDs (`oid`, `tid`), optional
display name and manually named identity-provider hosts (`idp_hosts`). Sign-in verifies the account and tenant;
an existing identity is not replaced by a different account. Under the data root:

- `store/<oid>.sqlite`: the account's SQLite/WAL store, with adjacent engine locks and picture cache.
- `regions/<oid>.json`: region metadata from authz, without a token.
- `settings.toml`: the app's UI/privacy/device/diagnostic settings.
- `logs/teams-app.log`: the retained diagnostic filename, private and rotated.
- `banner-pictures/` (Windows and macOS): the picture of each desktop banner shown, a PNG named by the banner's own
  key, removed with its banner, at the quit and at the next start. Linux hands the pixels on the bus instead.

A legacy single `account.toml` and `region.json` migrate to the multi-account layout at startup. Removing an account
deletes its credential items, account entry and region; its store remains for explicit deletion by the user. Engine
locks prevent a concurrent forget while that account is running.

## Several accounts

The user may add several work accounts and run them together in one window. Rail avatars switch accounts, "+" adds
one, and the account menu offers "Sign out and remove". Every account notifies, without naming the account (clicking
one opens its chat in its account), and the tray badge counts all accounts' unread chats. Each account has its own
engine, store, tokens and region; one engine opens each store.

A newly typed account is pending until sign-in succeeds. If sign-in stores nothing, a bare entry containing only its
UPN is removed; entries with manual identity-provider hosts or a refused provider remain pending for correction.
An existing provisioned lab identity is different: canceling or failing its first sign-in retains it so the alias
can retry. A lab alias selects its one provisioned account directly and cannot add a second one, but can forget it.

## Requests, outbox and diagnostics

Writes are ordinary typed requests sent through the outbox. Durable rows keep their `clientmessageid`; an unknown
outcome is checked rather than blindly resent, and no row from a previous run is resent without `Store::resend`.
Transient and call/calendar/team operations retain their explicit retry rules. The transport makes no hidden retry
of a typed write; eligible plain reads retain their bounded retry. Every token goes only to its own service, over
HTTPS, with no proxy or redirect. The [endpoint inventory](protocol/endpoints.md) records the implemented shapes.

The app's diagnostic log at `info` contains kinds, IDs, request shapes, statuses and latency, without payloads.
At `debug` it includes every API request and response in full, including token exchange/refresh and authz, and every
Trouter frame both ways. The logger redacts credentials itself, including auth codes, PKCE verifier, refresh/access/
ID/skype tokens, Authorization/cookies and known sent credentials echoed in error text. The rest of the payload
remains visible. `TEAMS_APP_LOG=<level>` overrides the settings; `trace` is treated as `debug`.

Debug logs still contain message text, names and service answers: never share or commit them, even sanitized.
The diagnostic file rotates at 8 MiB, keeping four older files. Copy diagnostics includes the app/build/log details
and recent diagnostic lines, and marks any debug content unsafe to paste. A failed log open is reported and the
app can continue without a file.

### The settings that write on their own

The first-run screen explains typing indicators, read receipts and presence. Those settings send nothing until
confirmed. The app otherwise writes from an explicit user action or the protocol follow-ups it requires. Views
reach the engine through `Core`; no view builds an API request. Rich text is sanitized before it is drawn.

## Calls

The app supports 1:1, group and meeting calls through `teams-call` signaling and `teams-media` libwebrtc. The user
starts, joins, answers, declines or leaves; a view does not do those actions on its own. Incoming invitations
addressed to the account ring only when the tracker/floor can offer them. Tracking filters malformed or self offers,
deduplicates redeliveries, ignores ended rings and binds acceptance/media links.

One call or ring is admitted at a time across the app's accounts (`teams-core::CallFloor`). Each calling delivery
is answered only with its typed response on that delivery's connection. The camera starts off and only the user
turns it on. The microphone/speaker open after an admitted start/join/answer succeeds. Linux desktop mode uses
PipeWire and the camera portal; Windows uses WASAPI and Media Foundation, with its native picker for screen sharing.
Lab/tests explicitly choose sandbox reach at run time. Without a microphone, calls connect muted; receiving media
and sharing a screen remain available. The user can retry the microphone after connecting one. "Make a test call"
uses the Teams echo service constant from the web client.

Media uses relay-only ICE, Teams' captured SDP dialect, DTLS-SRTP, Opus and software H.264. The call window controls
mute, camera, devices, background effects and leave; closing it leaves the call. The main window remains usable.
The user's own real-call checklist remains in [M3 acceptance](m3-acceptance.md); that historical report is not a
request for an agent to make calls from the user's account.

### The lab accounts as test callees

An agent may freely drive lab accounts in their sandbox after the per-run check, reaching only lab accounts, the
echo service and the user's own account. The user's own account may participate when the user drives it; the agent
still does not sign in or drive that personal account in sending mode. Follow [the lab rule](lab-rules.md).

## Updates

A copy installed from a release package updates itself from GitHub's releases (`crates/teams-update`,
`crates/ngteams/src/update.rs`): an anonymous check of the latest release an hour, no token or account data sent,
and the title bar's Install update downloads the matching asset, verifies it against `SHA256SUMS`, installs it (a system
package through `pkexec` and its package manager) and restarts. The restart waits while a call is up or a message is
unsent. A source build (`scripts/install.sh`), a lab build, a `test-support` build and a read-only run never check:
only a build the release workflow made knows its release version. Agents test the updater against a local fake feed
and temporary installations only, never the user's installed copy.

## `ngteamsctl`

The installer puts `ngteamsctl` beside `ngteams` (`~/.local/lib/ngteams/`, linked from `~/.local/bin/`) with its
shell completions. It is the user's own scripting of their Teams, like a keyboard shortcut: every command works on
the installed app with no setting to turn on and no confirmation. The app serves it on
`$XDG_RUNTIME_DIR/ngteams/ctl.sock` (`0600` in a `0700` directory, so only the user's own processes reach it). Its
writes, own status and calls take the same paths as the window's (the outbox, the status menu, the call buttons), so
the outbox, transport and call rules above hold for them. A second launch, or a meeting link from the desktop entry,
hands off to the running app over the same socket. Commands, events and exit codes:
[the development guide](development.md#ngteamsctl).

## Its sign-in

`ngteams` opens a WPE WebKit sign-in window in its own app. The page engine runs in the `ngteams-web` worker beside it;
sign-in hands its typed answer back in-process to `teams-auth`. Every production window uses `SignInHosts` and the
egress host gate, whichever engine it runs on: the system's WPE WebKit 2.54 or newer, or a release's pinned runtime,
downloaded once and verified ([development guide](development.md#wpe-webkit)). Building from source needs the system's
package; the installer checks it.

### Choice C: the host bound

The retained host bound permits Entra's sign-in/static-content hosts and identity-provider hosts manually listed
in that account's `idp_hosts`, HTTPS on port 443 only. Microsoft service domains cannot be added as identity
providers; the window can sign in, but cannot load the Teams app, Graph or Office services. The network session's
only egress path is its proxy host gate. The web-process guard additionally classifies requests it sees and logs
decisions with method/host/path shapes, no bodies or credential values.

The guard's known redirect/process gaps are described in
[the sign-in guard backlog](backlog/webkit-sign-in-guard-redirects.md). The host gate limits HTTP(S) connections
even when a request is not seen by the per-page guard. This is a host bound, not a promise that the guard sees
all requests. WebSockets, beacons, service workers, WebRTC, new windows and downloads are disabled, with an
ephemeral session. A refused provider host is reported by host name only so the user can decide whether to add it;
nothing updates `idp_hosts` automatically.

The user completes supported multi-factor and federated prompts in that window. Unsupported device-bound flows,
WebAuthn/security keys or client certificates are reported, not worked around. Conditional Access is respected:
no alternate client ID, borrowed device claim, browser-cookie scraping or other bypass. The user registers new
methods in their normal browser, outside this client's host list.

Lab tokens may initially be provisioned with `proto-lab auth <alias>` from the alias's signed-in browser profile.
It uses the fixed `ngteams-lab` namespace and writes the native account/region files. Later lab sign-in, refresh and
forget use that alias's broker. Broker/trace descriptors are preserved for explicit sign-in selfexec and not
inherited by ordinary WPE children.

## The first run, and Conditional Access

These are actions for the user, never an agent's personal-account test:

1. Request or run `scripts/install.sh`, then start `ngteams` and enter the work UPN.
2. Complete sign-in in the host-gated window, including any supported multi-factor prompt. Tenant sign-in logs see
   the Teams desktop client from Linux. A tenant/device policy the app cannot satisfy is a refusal, not a workaround.
3. Verify the account and tenant shown, inspect the chat list, and review the settings explaining automatic writes.
4. Confirm those settings before using them, then send only as the user intends. Re-sign in when the tenant revokes
   or expires the refresh token.
5. Report an error code or plain description, never tokens, captures or a diagnostic log containing payloads.

## What agents may do with the personal build

- Compile/check and mock-test the client with temporary files and synthetic secret stores. Runnable lab builds are
  made only through `scripts/run-lab.sh`; the personal install script is used only on the user's explicit request.
- Read the user's personal config/data directories, including stores, settings and diagnostic logs, at any time.
- On the agent's own initiative, run the installed personal app only through `scripts/run-personal-read-only.sh`.
  Navigate, screenshot and inspect that snapshot-based window without driving the user's live instance.
- Use `ngteamsctl` only against a lab alias (`--lab <alias>`, within the lab rule) and the read-only runner's socket
  (`--read-only`), never against the installed instance's personal socket: not by default, `--socket` or
  `$NGTEAMS_CTL_SOCKET`. Nor launch `ngteams` in a way that hands off to the running app.
- Only on the user's explicit request each time, install the release or start the installed app detached for the
  user, then leave it alone.
- Never sign in the user's account, drive/test/capture its sending mode, touch its `ngteams` keyring items, receive
  its tokens, or type credentials through sandbox controls.

A plain root Cargo build excludes `ngteams` through `default-members`. Test-support builds refuse to run. An
installed binary carries `TEAMS_PERSONAL_INSTALL`, set by `scripts/install.sh`; lab builds carry
`TEAMS_LAB_BUILD`, set by `scripts/run-lab.sh`, and run only with explicit lab mode. The release workflow sets
`TEAMS_PERSONAL_INSTALL` too, for the packages it publishes. The install marker still gates
other app/broker modes. These markers prevent accidental runnable builds; they do not authorize setting them by
hand or bypassing the agent rules.

### Read-only runs

The runner makes a consistent snapshot of account/settings/region/store files using `ngteams snapshot`, starts a
get-only credential broker outside its private UI sandbox, and runs `ngteams --read-only` on that snapshot. Stored
personal tokens are never rotated or overwritten; refresh changes are kept in an in-memory overlay. No push channel
or calls/devices open, and catch-up runs by reads once a minute. Registrations, messages, edits, reactions, typing,
receipts, presence/status changes and all calling writes/replies are refused below the views and logged as
`refused_read_only` in the snapshot's `data/logs/`. The UI offers no writes or account add/remove and no tray item.
An unusable token shows signed out; this mode never opens sign-in.

```sh
scripts/run-personal-read-only.sh you@example.com
scripts/run-personal-read-only.sh --shot chats
scripts/run-personal-read-only.sh --key ctrl+k
scripts/run-personal-read-only.sh --click 300 200
scripts/run-personal-read-only.sh --windows
scripts/run-personal-read-only.sh --status
scripts/run-personal-read-only.sh --stop        # deletes snapshot/run; --keep retains them
```

The runner uses `~/.local/state/ngteams-read-only/run/` by default; its status prints the snapshot, logs, trace and
screenshots. It leaves the user's normal running app and its source files untouched. `TEAMS_BIN` may choose the
installed executable; a binary without read-only support is refused. See [the development guide](development.md#the-read-only-mode-scriptsrun-personal-read-onlysh).

## How the implementation reads it

`ngteams` resolves the selected mode before constructing `Setup`: personal startup uses fresh lazy Secret Service
handles, lab startup shares its scoped broker, and read-only startup uses a snapshot and in-memory secret overlay.
Account readiness, sign-in, refresh and forget consistently use that selected store. A successful first sign-in
restarts the app with its account entry ready; adding another personal account starts its engine without restarting
the other accounts. The account prompt and sign-in window remain separate from the protocol engines.

## How the lab and personal binaries are kept apart

They are modes of the same client, using fixed separate namespaces and paths. Native lab aliases use
`ngteams-lab/<alias>` under the data home and one-account brokers in `ngteams-lab`; personal mode uses `ngteams`
config/data and credentials. Existing symlinks are resolved when checking overlap with personal directories.
The app does not consume the browser harness's `lab.toml`. Lab windows require the UI sandbox, read-only personal
runs require their snapshot and get-only broker, and normal personal runs require the user's installed build.
Native protocol test-support is enabled through dev dependencies, absent from the normal dependency tree;
`ngteams/tests/apart.rs`, protocol compile contracts and mock-origin tests check those boundaries.
