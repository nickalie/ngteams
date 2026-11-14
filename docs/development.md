# Developing ngteams

The technical guide to the project; the [README](../README.md) is the introduction for people using the app.

A native Microsoft Teams client for Linux in Rust + GPUI, for work/school accounts. It uses Teams' internal APIs,
with the web client as the protocol reference. `ngteams` is the only client binary: installed for the user's own
accounts, or run with isolated lab accounts through `scripts/run-lab.sh`. `proto-lab` remains the browser capture,
scenario and bundle-analysis tool.

The app has chats, rich-text compose, reactions, search, teams and channels, notifications and tray support,
audio/video calls, and the calendar and meeting flows. Earlier milestone outcomes and measurements remain in
[the acceptance records](m3-acceptance.md) and [completed plans](plans/completed/); they describe the code
and live runs at those milestones, rather than today's commands. Shared rules for Claude Code and Codex are in
[AGENTS.md](../AGENTS.md).

The client also stages dropped files as private attachments, pastes pictures into the compose box (a screenshot, a
browser's copied image, picture files a file manager copied) and sends them inline as Teams does, retains compose
drafts while sends are pending,
opens people search results as chats, starts chats with one person or several from New message (Ctrl+N: a To: line
of people, an optional group name, the chat those people already have shown under it), opens a person's profile card
from their picture or name (a message's sender in a chat or a channel, a 1:1 chat's header, a chat's people, people
search results: the compact card on a rest or a name's click, the full card with its Overview, Profile and
Organization tabs on a picture's click; Chat, a quick message through the outbox and 1:1 audio and video calls from it;
`crates/teams-ui/src/profile_card.rs`), and keeps unread badges in sync with read receipts. Editing message text
keeps its existing file references and cards. Channel menus provide management, notification and section
preferences, email and workflow actions. Calls and meetings support Teams
live captions and spoken-language controls (Alt+Shift+C); caption text stays in memory and clears on hide or call
end, unless Settings → Calls → "Save live captions of every call" is on: then every call's captions are turned on
(their pane stays as the user has it) and saved in the store as the call's transcript, with nothing sent to Teams
or shown to the others. A saved transcript is linked from its call's end row in the chat (a row of the app's own for a 1:1
call, which Teams posts none for; "Saved on this computer")
and opens in the chat's Recap tab with search, .vtt download, the AI summary and Delete transcript; Teams' own
transcript of the same call wins. Captions can be translated on this computer ("Translate to" in the caption settings) by Claude Code or the Codex
CLI, with any model the tool lists or one typed (by default Claude Haiku or GPT-6-Luna; Settings → Calls → Captions);
the caption text then goes to that tool and its provider. A chat or channel can be read and written in another
language (the header's translation button): its messages are shown translated by the same tools (Settings → General
→ Chat translation), the account's own are sent in the chat's language and shown as written, and the translations
are kept in the store, so a chat opened again is not translated twice. Its last message in the chat list, its new
messages' desktop notifications, its messages found by the search and a reply's quote read in that language too (a
notification waits at most a few seconds for its translation); the language menu filters as you type. See [channel management](protocol/channels.md#managing-an-existing-channel),
[workflows](protocol/workflows.md), and [live captions](protocol/live-captions.md).

## Layout

| path | contents |
|---|---|
| `crates/ngteams` | sole client, account files and startup, selected credential store, lab/read-only modes, build markers and sign-in integration |
| `crates/teams-auth` | PKCE, code exchange/refresh, identity checks, authz/skypetokens, sessions and scoped credential brokers |
| `crates/teams-proto` | IDs and service constants, typed API requests and delivery responses, HTTP transport, chat/rich-text models and shared URL/path log shaping |
| `crates/teams-store` | per-account SQLite/WAL stores, migrations, durable outbox and engine locks |
| `crates/teams-trouter` | messaging and calling push sockets, registration, reconnect and typed connection-bound call replies |
| `crates/teams-call` | call state machine and tracking: offers, redelivery/ended rings, acceptance and media links, one call per account |
| `crates/teams-core` | account engines, sync, outbox and retries, calendar/team operations and the cross-account call floor |
| `crates/teams-media` | libwebrtc, relay-only ICE, audio processing, video effects, file/PipeWire devices, GStreamer playback and validated SDP adapters |
| `crates/teams-sdp` | Teams' SDP dialect and group media control plane, without a media engine |
| `crates/teams-ui` | GPUI views/models over `Core`, rich-text editor/rendering, accounts, calendar, notifications and call windows |
| `crates/teams-ctl` | `ngteamsctl` and the control socket's protocol, socket paths, line codec and client (no GPUI); the app's server is `crates/ngteams/src/ctl/` |
| `crates/teams-log` | private rotated diagnostic files, full debug HTTP/frame records and logger-owned credential redaction |
| `crates/teams-platform` | native private-file creation/permissions, Windows identity, selective process-handle transfer and VM-console guard |
| `crates/teams-signin`, `crates/teams-signin-extension` | in-process host-gated WPE sign-in window and its web-process request guard |
| `crates/teams-wpe`, `crates/wpe-*-sys` | the WPE WebKit engine, its `ngteams-web` worker and generated bindings |
| `crates/teams-wpe-client`, `crates/teams-web-engine` | the app's side of that worker (no WebKit linked) and the choice or download of the engine |
| `crates/teams-mock` | synthetic HTTP/Trouter/calling/media services for tests, dev-dependencies only |
| `crates/proto-lab` | lab config, persistent browser profiles, native token provisioning, captures/holds, scenarios, probes, export and bundle search |
| `crates/ui-sandbox` | private headless Wayland compositor, bus, input, URI opener, optional PipeWire/camera portal and `ui-sandbox-run` controls |
| `crates/fixture-guard` | synthetic-fixture markers and credential/private-data scans |
| `scripts/run-lab.sh`, `scripts/run-personal-read-only.sh` | isolated sending lab sessions and snapshot-based read-only personal sessions |
| `scripts/install.sh`, `packaging/` | user-requested personal release install, desktop entry and icons |
| `docs/protocol/` | protocol observations in our own words and the [endpoint inventory](protocol/endpoints.md) |
| `docs/personal-build.md`, `docs/lab-rules.md` | personal account authorization and the agent's lab run check |
| `spikes/gpui-video`, `tools/`, `vendor/` | standalone experiments, supporting asset tools and patched dependencies |

The native transport uses HTTPS only, no proxy or redirects, and sends each token only to its own service.
Writes are typed requests through the outbox; unknown outcomes are checked and rows are never resent across runs
without `Store::resend`. Trouter replies are typed and bound to the delivery's connection. Call tracking and the
cross-account floor are app behavior, independent of lab configuration.

### WPE WebKit

The sign-in window renders Microsoft's page with WPE WebKit 2.54 or newer. Building the WPE crates needs a development
copy: the distribution's `wpewebkit` (`sudo pacman -S wpewebkit`) locally, ngterm's portable prefix in the release job. The build finds it through the
system's pkg-config (`wpe-webkit-2.0`, `wpe-web-process-extension-2.0`), the binaries load `libWPEWebKit-2.0.so` from
the loader's own path, and the engine starts the package's helper processes (`WPEWebProcess`, `WPENetworkProcess`,
`WPEGPUProcess`) from `/usr/lib/wpe-webkit-2.0/`, where their path is compiled into the library; GStreamer loads its
plugins at run time. `scripts/install.sh` refuses to build without it.

Only the `ngteams-web` worker (`crates/teams-wpe`) links WebKit. `ngteams` uses `crates/teams-wpe-client`, so it starts
without an engine and picks one before each sign-in (`crates/teams-signin/src/engine.rs`): the system's if `ngteams-web
--probe` loads 2.54 or newer of the 2.x series, then a runtime already downloaded to `<data dir>/web-engine/<key>/`,
then a download (`crates/teams-web-engine`). A release build compiles in the runtime's `NGTEAMS_WEB_ENGINE_KEY`, `_URL`,
`_SIZE` and `_SHA256`, and a download is checked against those; setting `NGTEAMS_WEB_ENGINE_URL` when starting the app
replaces the URL only. Builds without pins (development, lab) never download. `ngteams --install-web-engine <archive>`
installs the runtime from a file, under the same checks. Only the worker's command gets a runtime's library, GIO,
GStreamer and injected-bundle paths (`EngineSource`), so the app and what it starts never see them.

A test program that opens a sign-in window serves as its own worker: it calls `teams_wpe::worker::serve_if_asked` first
and pins itself with `teams_wpe_client::worker::pin_worker_path`.

## Third-party UI code

The views are built on [`gpui-component`](https://crates.io/crates/gpui-component) 0.8 (Longbridge's GPUI Kit,
Apache-2.0), the UI kit for `gpui-pre` 0.3.9; it resolves to the workspace's vendored GPUI patch. Its audit (M2 Task 7,
`docs/plans/completed/20261021-native-teams-m2-gpui-daily-driver.md`) found, and `crates/teams-ui/tests/reach.rs` keeps
checking:

- It takes `gpui-pre` with its default features (`x11`, `font-kit`, `windows-manifest`), so the workspace's GPUI has
  them on. In `gpui-pre` 0.3.9 `x11` only lets GPUI name an X11 display it could use and `font-kit` switches nothing;
  the X11 client is `gpui-pre-linux`'s, which stays Wayland-only (`crates/teams-ui/tests/reach.rs` checks it), and
  `ngteams` lab and read-only modes refuse a set `DISPLAY` all the same. `windows-manifest` acts on Windows builds only.
- Its tree for the host holds no HTTP or WebSocket client (`gpui-pre-reqwest` is for wasm only) and only licences an
  Apache-2.0/MIT project can take: MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, 0BSD, CC0, and MPL-2.0 for
  `option-ext`, which a build-time macro of the kit's assets uses (`reach.rs` checks both).
- It starts no thread or process of its own and touches no file in our use: its theme directory watcher
  (`ThemeRegistry::watch_dir`, `notify`) is never called. It reads the reduced-motion setting from the settings portal
  on the bus the app is given (the sandbox's private bus, which has none). The app installs its diagnostic logger
  before startup; third-party payload logging is excluded by the compile-time log cap and the logger's target filter (`reach.rs` checks the app and dependency boundary).
- We don't use its Markdown/HTML text view: message text is rendered from `teams-proto`'s `RichText` only.

No Zed code is in the repo: Zed's UI crates are GPL-3.0 and served as a design reference only. The rest of the UI is our
own, on the kit's widgets and gpui-pre's examples (Apache-2.0).

The kit's `Textarea` is tested on GPUI's test context (`crates/teams-ui/tests/kit.rs`: typing, selection, the
clipboard with plain text only, undo, soft wrap, IME composition, Enter and Shift-Enter) and on the real platform in
the UI sandbox (`crates/teams-ui/tests/kit_window.rs`, which also switches the theme live through a fake settings
portal on the sandbox's bus, and clicks a message's link drawn by our rich text renderer, which goes to the sandbox's
no-op opener). The compose box is our own rich-text editor (`crates/teams-ui/src/editor`, tested in
`crates/teams-ui/tests/editor.rs`), drawn with GPUI's text elements as the renderer draws messages.

## The lab

Three dedicated accounts, a, b and c, live in the company tenant. Before **each live run**, the agent lists every
chat, call and person the run can reach and verifies that all are lab accounts, the approved echo service or the
user's own account. This includes automatic UI actions and scenario follow-ups. The agent applies
[the lab rule](lab-rules.md); the application does not read `lab.toml` or enforce a lab roster.

The browser/capture harness keeps its own configuration, outside the repository:

```toml
# ~/.config/teams-lab/lab.toml (0600); passwords never belong here
# data_dir = "/absolute/lab-data"  # optional; default $XDG_DATA_HOME/teams-lab
[[accounts]]
alias = "a"                      # also b and c, each exactly once
upn = "bot-a@lab.example.com"     # all three in one tenant domain
# mri = "8:orgid:<object-id>"     # recorded after login

# Repeat [[accounts]] for b and c.
[[service_participants]]
name = "echo"
# mri = "28:<captured-bot-id>"

[[contacts]]
name = "me"                     # only the user's own account
mri = "8:orgid:<object-id>"
# upn = "me@example.com"         # optional, for calendar attendee flows
```

| location | contents |
|---|---|
| `$XDG_CONFIG_HOME/teams-lab/lab.toml` | harness accounts, MRIs, service participants, contacts and optional browser data root |
| `<browser data>/chrome/`, `profiles/{a,b,c}/` | checksum-verified pinned Chrome and persistent signed-in profiles |
| `<browser data>/captures/<scenario>/<timestamp>/events.jsonl` | raw recordings; never committed |
| `<browser data>/reference/`, `bundles/`, `chats.toml` | local reference copies, unpacked/indexed bundles and recorded lab chat IDs |
| `<browser data>/auth/<alias>.json` | region metadata for native HTTP probes, never tokens |
| `$XDG_CONFIG_HOME/teams-lab/accounts.json` | password input used by `proto-lab login`, mode `0600`, never committed |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/accounts.toml` | the alias's one provisioned native account, including identity/name/optional `idp_hosts` |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/regions/<oid>.json` | native region metadata written by `proto-lab auth` |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/store/<oid>.sqlite` | native account store and adjacent engine-lock/picture-cache files |
| `$XDG_DATA_HOME/ngteams-lab/<alias>/settings.toml`, `logs/` | app settings and diagnostic logs |
| `$XDG_STATE_HOME/ngteams-lab/<alias>/` | runner holder/broker/app logs, trace and screenshots; retained after stop |
| Secret Service application `ngteams-lab` | native refresh tokens/skypetokens, scoped by account and audience |
| `$HOME/.local/share/teams-lab/live.lock` | fixed shared runner lifetime lock, excluding exclusively locked browser/capture runs |

Absolute XDG homes override their defaults (`~/.config`, `~/.local/share`, `~/.local/state`). Native config and data
for each alias share its `ngteams-lab/<alias>/` directory. Browser `data_dir` does not relocate native files. Account
and store files are private (`0600` in `0700` directories). `accounts.json` uses the shape
`{"accounts": [{"profile": "a", "email": "bot-a@lab.example.com", "password": "<password>"}]}` (repeat for b/c);
real values stay outside the repository.

After the per-run check, provision native lab tokens from a signed-in browser profile with `proto-lab auth a`.
It verifies identity, writes tokens under `ngteams-lab`, and creates the account/region layout above. A provisioning
file failure can occur after tokens are stored; its error is reported. `--no-skypetoken` omits authz and therefore
its region. Passwords are handled by `proto-lab login`, never typed through the runner's control socket.

## Building

The Windows client includes native startup, WebView2 sign-in, Credential Manager storage, private IPC and restart,
WASAPI/Media Foundation devices, Windows Graphics Capture, and toast/tray/taskbar integration. The release workflow
builds and packages the native x64 binaries alongside Linux. The [Windows portability checks](ci.md#windows-portability)
exercise the foundational libraries on Linux and Windows. In a Windows checkout with the repository's Rust toolchain and MSVC C++
build tools:

```powershell
cargo check --locked -p teams-platform -p teams-log -p teams-store -p teams-auth -p teams-ctl --all-targets
cargo test --locked -p teams-platform -p teams-log -p teams-store -p teams-auth -p teams-ctl
```

For native Windows debugging, the dedicated `win11` VM's console is the Windows UI sandbox, as authorized for the
port; lab/test GUIs must stay there, with isolated lab identities and the [Windows runner](#windows-vm-runner).
The native runner isolates lab identities in that console. Linux builds and runs through the runners below.

Agents build runnable `ngteams` only through the lab runner:

```sh
scripts/run-lab.sh --build-only  # normal debug build in target/lab; opens no account, broker or window
cargo build -p proto-lab --target-dir target/lab  # browser/protocol tool, not the client
```

The runner sets `TEAMS_LAB_BUILD=1` and removes `TEAMS_PERSONAL_INSTALL` from that build environment. A lab-marked
binary runs only in explicit lab mode; an installed build may also select lab mode. The install marker remains
required for other app/broker modes. Binaries built with `test-support` refuse to run. Cargo unifies dev features
for tests/`--all-targets`/`--all-features`, so use the normal runner build for live work, not a test executable.
The workspace's default members exclude `ngteams`; never use `cargo build -p ngteams` as an agent shortcut.

The personal release is built and installed with `scripts/install.sh` **only when the user requests it**.
It installs under `~/.local`, starts nothing and requires the user's own first sign-in. See
[the personal-build guide](personal-build.md).

`teams-media`'s calls run on libwebrtc, through LiveKit's `libwebrtc` bindings: a clean build downloads LiveKit's
prebuilt static libwebrtc (about 150 MB, `webrtc-linux-x64-release`) from GitHub, or takes the one `LK_CUSTOM_WEBRTC`
names. A call's H.264 is encoded in software (OpenH264). NVIDIA's NVENC and NVDEC stay out:
the bindings build them only when `CUDA_HOME` (or `/usr/local/cuda`) holds CUDA's headers, so leave it unset for this
build. NVIDIA's decoder library hangs the process at exit while a decoder is still alive.
Its PipeWire binding needs libpipewire's headers and `clang` for `bindgen`.
PipeWire reach is selected at run time: desktop for the user's normal app, sandbox for lab/test runs.
Read-only mode opens no media devices. Native call-device discovery runs on its own thread, so it cannot delay
account engines or the first window. Settings → Calls and devices shows "Preparing media…" until discovery finishes;
a call started meanwhile waits before its media setup and signaling. If discovery fails, calls stay unavailable for
that app run and chats keep working.

A video a message carries is played in its card by the system's GStreamer (`teams-media`'s `player`: `playbin` on an
`appsrc` the engine feeds, its frames and sound to appsinks, the sound on the device seam's speaker). The build needs
GStreamer's development files (`gstreamer`, `gst-plugins-base`, through `pkg-config`); playing needs the plugins for
the video's format at run time: `gst-plugins-base` and `gst-plugins-good` (MP4, playbin's converters) and
`gst-libav` (H.264, AAC and most other codecs). Only software video decoders are used (making a hardware one probes
the GPU's drivers, which can take seconds). A video whose picture no installed plugin decodes says so in its card and
offers the browser; one whose sound none decodes plays silently.

## `proto-lab`

The protocol tool uses recorded browser sessions and explicit capture holds. A hold pauses and fails only its
matching request/frame so a scenario can record the shape without sending it. Current and legacy queued held
messages stay held on later sessions. There is no browser write policy or membership gate; the agent's per-run
lab check is required before navigation or interaction.

`login`, `status` and `auth` run headless; agents also give `run` its `--headless` flag. Use only one browser per
profile and hold the fixed live lock **exclusively** around browser/capture work, with an outer timeout. A browser
run and native aliases must not share that lock concurrently. `chrome`, `bundles` and `export` can work without a
lab config; an invalid config is reported.

```sh
proto-lab chrome install
proto-lab chrome path
proto-lab login a [--timeout 300]
proto-lab status [a b c] [--timeout 120]
proto-lab auth a [--audiences] [--no-skypetoken] [--timeout 120]
proto-lab probe chat a [--with b] [--save <name>]
proto-lab probe thread a [--save <name>]
proto-lab probe profile a
proto-lab probe teams a
proto-lab probe object a <id> [--save <name>]
proto-lab run <scenario> --headless [--timeout 180] [--call <step>]
proto-lab export <capture> <name> [--strict]
proto-lab bundles import <msix>
proto-lab bundles search <regex> [--kind url|host|route|method] [--version <v>]
proto-lab service-mri echo <mri>
```

`login` uses the password input outside the repository, records its steps in `status-<alias>.json`, and records the
account's MRI. `auth` uses the profile's existing sign-in, catches Entra's native redirect, then exchanges and
refreshes tokens in Rust; it never types a password. A profile needing interaction is reported for `login`.
Probes make plain HTTP GETs with the account's tokens and print outlines/claims, never response bodies or tokens.
`run` exits non-zero when its goal is not reached or capture setup fails. Captures are raw private lab data.

| scenarios | purpose |
|---|---|
| `load`, `search` | web load and title-bar suggestions/results |
| `ab-chat`, `ab-chat-full`, `abc-chat`, `chat-shapes`, `abc-everyone` | messages, edits, reactions, quotes, typing, group creation and mentions; explicit holds where shapes are captured |
| `ab-image`, `ab-file` | synthetic uploads held for shape capture |
| `ab-trouter`, `ab-trouter-quiet` | push capture; quiet variant holds `user.activity` frames |
| `echo-call`, `ab-call`, `abc-call` | browser echo, 1:1 and group call captures; `--call` chooses the step (`--call steps`: once the call is up, the caller's page is driven over `PROTO_LAB_STEPS_SOCKET` with `team-manage`'s steps, a callee's by a step that starts `@b ` or `@c `; `PROTO_LAB_STEPS_BYSTANDERS=c` also loads c's page, not called, for steps such as a consult or an add) |
| `ab-meeting`, `ab-meeting-join`, `meeting-steps` | scheduling, pre-join and step-controlled meeting captures |
| `channel-create`, `team-manage` | held channel/team writes; team sending requires explicit scenario configuration. `team-manage` is also how an agent looks at a feature in browser Teams before building it: steps over `PROTO_LAB_STEPS_SOCKET` (`go`, `click`, `point`, `xy`, `wheel`, `type`, `insert`, `press`, `js`, `shot`, `seen`, `hold`, `quit`) |
| `own-presence-activity`, `own-presence-endpoint`, `own-presence-both`, `own-presence-manual-held`, `own-presence-manual` | compare activity/presence paths, with the requested capture holds |

Protocol details and scenario controls are in [the protocol notes](protocol/).

## `ngteams`

### Lab runs (`scripts/run-lab.sh`)

After the per-run lab check and native provisioning, start each alias through the runner. These are live tenant
sessions, so run only the accounts and interactions already checked:

```sh
scripts/run-lab.sh a --pipewire --timeout 1800
scripts/run-lab.sh b --pipewire --timeout 1800
scripts/run-lab.sh a --shot chats
scripts/run-lab.sh a --key ctrl+k
scripts/run-lab.sh a --type '<lab chat name>'
scripts/run-lab.sh a --click 300 200
scripts/run-lab.sh a --clipboard image/png shot.png text/plain note.txt
scripts/run-lab.sh a --windows
scripts/run-lab.sh a --status
scripts/run-lab.sh a --stop
scripts/run-lab.sh b --stop
```

The broker runs outside each sandbox and serves only its one configured account in `ngteams-lab` with `get`, `put`
and `delete`. The app runs inside the private compositor/bus; `--pipewire` adds private audio/video devices.
An alias selects one account directly, allows normal sending and sign-out/forget, and offers no add-account prompt.
Lab windows require the sandbox compositor with `DISPLAY` unset; lab paths overlapping personal data/config are refused.

Each alias has an exclusive lock in `<state>/ngteams-lab/.locks/`; live holders share the fixed legacy live lock so
several aliases can coexist. The build happens before acquiring the shared live lock. The detached holder keeps
both lifetime locks through cleanup. The deadline defaults to 1800 seconds (`--timeout` overrides it) and requests
ordered TERM shutdown. A stuck holder keeps its locks and diagnostic state until cleanup finishes. Stop retains
account files, stores and logs. `--shot` captures a window, `--screen <name>` the whole output; `--key`, `--type`,
`--click`, `--windows` and `--status` address only that alias. Never type credentials through `--type`.

### Personal accounts

The user installs and runs their own app:

```sh
scripts/install.sh             # only on the user's explicit request
ngteams                                 # ask for the work account, then sign in
ngteams --account you@example.com        # sign in this account, adding it if new
ngteams --forget you@example.com         # remove credentials/config/region; quit its engine first; store stays
```

Several accounts run in one window, switched by the rail's avatars. The personal app uses
`~/.config/ngteams/accounts.toml`, data under `~/.local/share/ngteams/` and Secret Service namespace `ngteams`.
It does not read `lab.toml`. Its normal calls use desktop PipeWire and the camera portal. Agents may read personal
data/config directories, but may run this account themselves only through the read-only runner below; no personal
sign-in, sending-mode testing or access to its keyring items/tokens. See [the full rules](personal-build.md).

### The read-only mode (`scripts/run-personal-read-only.sh`)

```sh
scripts/run-personal-read-only.sh you@example.com
scripts/run-personal-read-only.sh --shot chats
scripts/run-personal-read-only.sh --key ctrl+k
scripts/run-personal-read-only.sh --click 300 200
scripts/run-personal-read-only.sh --windows
scripts/run-personal-read-only.sh --status
scripts/run-personal-read-only.sh --stop       # --keep retains the run instead of deleting it
```

It runs the installed app over a consistent snapshot in `~/.local/state/ngteams-read-only/run/snapshot/`, in its own
UI sandbox. The outside broker serves `get` only; refresh rotations live in memory. No personal source file or
stored token is changed. Transports refuse writes, registrations and call replies below the views, recording
`refused_read_only` with method, host and path shape in `snapshot/data/logs/` (also see `app.log`). It opens no push
channel, calls, PipeWire or camera, catches up by reads every minute and never opens sign-in. The UI offers no
writes, add/remove account or tray item. It does not disturb the user's running instance. The script builds only
`ui-sandbox-run`; it requires an installed binary with read-only support (`TEAMS_BIN` can select its path).

### `ngteamsctl`

`ngteamsctl` drives the running app over its control socket: reads, the window and its views, writes, own status,
calls, the calendar, and a stream of what happens. Nothing in the app has to be turned on, and it asks no
confirmation: the socket is `0600` in a `0700` directory, so only the user's own processes reach it. The server is
one more caller of what the window calls (`Core`, the outbox, the call buttons' path, the status menu's), so it
builds no request of its own. `scripts/install.sh` installs it beside `ngteams` with bash, fish and zsh
completions; `ngteamsctl completions <shell>` prints one.

Each app serves one socket, and `ngteamsctl` picks it by mode. `--socket <path>`, else `$NGTEAMS_CTL_SOCKET`,
overrides any of them:

| app | socket | `ngteamsctl` |
|---|---|---|
| installed `ngteams` | `$XDG_RUNTIME_DIR/ngteams/ctl.sock`, or the app's `--ctl-socket <path>` | (default) |
| lab alias | `$XDG_STATE_HOME/ngteams-lab/<alias>/ctl.sock`, handed in by `scripts/run-lab.sh` | `--lab <alias>` |
| read-only | `$XDG_STATE_HOME/ngteams-read-only/run/ctl.sock`, handed in by `scripts/run-personal-read-only.sh` | `--read-only` |

`XDG_STATE_HOME` defaults to `~/.local/state`. The sandbox clears a lab or read-only app's environment, so it
serves only where its runner's `--ctl-socket` says. On bind, a socket file left by an app that didn't quit is
replaced and one with a live app behind it is refused. The read-only app refuses every write, `status set` and
joins with `refused_read_only`. A lab alias's writes and calls are live lab runs, under the lab-contact check.

```sh
ngteamsctl status                           # version, accounts and their engines, the window, the call up
ngteamsctl accounts
ngteamsctl chats [--unread] [--limit N]
ngteamsctl messages <chat> [--limit N] [--before <message-id>]
ngteamsctl unread                           # the badge's counts, per chat and total
ngteamsctl search <words…> [--limit N]
ngteamsctl members <chat>
ngteamsctl activity [--filter unread|mentions|reactions|missed-calls|replies|voicemail|apps] [--search <words>] [--limit N]
ngteamsctl calendar [--today | --from YYYY-MM-DD [--to YYYY-MM-DD]]
ngteamsctl calendar show <event-id>

ngteamsctl window show|hide|toggle|focus    # hide needs a tray item (no_tray)
ngteamsctl activate
ngteamsctl open chat <chat> | channel <channel> | calendar | activity | settings [page] | search <words…>
ngteamsctl account switch <account>
ngteamsctl ui state
ngteamsctl action list
ngteamsctl action ShowCalendar              # any registered teams_ui:: action, full or unique short name

ngteamsctl send <chat> '<text>' [--wait]    # '-' reads the text from stdin; each line a paragraph
ngteamsctl edit <chat> <message-id> '<text>' [--wait]
ngteamsctl delete <chat> <message-id> [--wait]
ngteamsctl react|unreact <chat> <message-id> like|heart|laugh|surprised|sad|angry [--wait]
ngteamsctl read <chat> [--wait]             # mark read up to the newest message
ngteamsctl new-chat <mri|upn>… [--wait]     # a 1:1 chat for one person, a group chat for more
ngteamsctl resend <row> [--wait]
ngteamsctl status get
ngteamsctl status set available|busy|dnd|away|reset
ngteamsctl sync

ngteamsctl calls
ngteamsctl call <chat>
ngteamsctl answer | decline | hangup
ngteamsctl join <chat>
ngteamsctl join meeting <event-id> [--now]
ngteamsctl mute on|off|toggle
ngteamsctl camera on|off|toggle

ngteamsctl events [--follow] [--kind K,…] [--chat C] [--from WHO] [--mentions] [--detail] [--lead MIN,…]
ngteamsctl events kinds
ngteamsctl wait <kind> [--match REGEX] [--timeout SECONDS] [--chat C] [--from WHO] [--mentions]
```

- **Addressing**: `--account <upn|mri|index>` (index as `accounts` numbers them, from 0); by default the account the
  window shows, and for call commands the account ringing or in the call. A chat is its id or its title as the chat
  list shows it (the topic, otherwise the other members' names), in any case; a title several chats share is
  `ambiguous_chat`, listing their ids. A channel is its id, name or `<team>/<channel>`; a settings page its key or
  title.
- **Writes** go through the outbox as the window's do. Each answers with its outbox row as its first attempt left it;
  `--wait` waits up to a minute more for it to settle (`sent`, `failed`, or `unknown`: it may have arrived, and only
  `resend` sends it again). A write the app didn't answer in time may still go out: check
  `events --follow --kind outbox` or `messages <chat>` before writing it again. `send` takes plain text; markdown and
  mentions aren't parsed. `status set` chooses as the status menu does, kept in the settings; `reset` hands the
  status back to the app.
- **Calls** are asked as the call buttons ask them (the one-call rule, the call window and devices apply) and answer
  with the call's row once the engine took it. `mute` and `camera` with `on`/`off` change only what differs.
  `join meeting` opens the pre-join as the calendar's Join does; `--now` joins at once (microphone on, camera off).
- **Output** is tables and one line per event; `--json` prints each response, error or event as the JSON line the
  app sent. The wire is a JSON line per request (`{"id":N,"cmd":"…","account":"…"}`), answered
  `{"id":N,"ok":{"kind":"…",…}}` or `{"id":N,"error":{"code":"…","message":"…"}}`, after a `hello` that checks the
  protocol version; the types are in `crates/teams-ctl/src/protocol.rs`. Error codes: `unknown_account`,
  `unknown_chat`, `ambiguous_chat`, `busy`, `no_calling`, `no_call`, `not_ringing`, `no_video`, `no_meeting`,
  `no_tray`, `no_window`, `unknown_action`, `refused_read_only`, `stopped`, `timeout`, `bad_request`,
  `version_mismatch`, `engine`.
- **Exit codes**: 0 success; 1 an error answer, a write whose row failed (with `--wait`, one not sent), or a stream
  that ended early or lagged; 2 no app on the socket, no socket path, or a usage error; 3 `wait` timed out.

**Events.** `events --follow` prints every event the filters let through until the app closes the stream; without
`--follow` it prints the first one. `wait <kind>` prints the first event of that kind whose JSON line matches
`--match` and exits 0, or 3 past `--timeout` (which the connect counts towards; without it, it waits on). `--kind`
and `wait` take `message*`-style prefixes. A subscriber that falls behind gets a last `lagged` line and the stream
ends. Each line is `{"event":"<kind>","account":"<upn>","at":<ms>,…}` and carries only what its source holds:

| group | kinds |
|---|---|
| messages | `message` (chat, message, sender, plain text, `from_me`, `mentions_me`, `source`: push, catch_up or replay; new messages only, never older history), `message_edited`, `message_deleted`, `reaction` (with `--detail`, the message's current reactions), `typing`, `read` (the account's read horizon moved) |
| chats | `chat_changed`, `teams_changed`, `unread` (a chat's count and the account's total, as the badge counts) |
| people | `presence` (MRI, availability), `own_status` (the account's reported status, whoever changed it) |
| outbox | `outbox` (row id, write, chat, state: queued, sent, failed, unknown) |
| calls | `incoming_call`, `ring_ended`, `call` (state, and the reason once ended), `call_participants` (joined, left), `lobby`, `voicemail`, `call_window` (open or closed) |
| calendar | `meeting_starting` (event id, subject, start, minutes to it, whether it is joinable), at each `--lead` (default 5 and 0 minutes) |
| app | `connection` (connected, disconnected, stopped), `caught_up`, `catch_up_failed`, `engine_failed`, `signed_out`, `account_added`, `account_removed`, `notification` (a desktop notification shown: title, chat), `ui` (the shown account, view, chat or window changed) |

**Hand-off.** A second `ngteams` on the personal build, with no flags or with one Teams link, doesn't start a second
app: before taking any lock it asks the app on its socket (`--ctl-socket` when given, else the personal one) to
`activate`, or to open the link, and exits 0. A meeting join link (`msteams:/l/meetup-join/…` or
`https://teams.microsoft.com/l/meetup-join/…`) opens that meeting's pre-join; any other link shows the window and
says it wasn't opened. With nobody behind the socket (no file, or a stale one) the launch starts the app, which
opens a given link once up. An app that takes the connection but doesn't answer in time (a second to connect, five
to answer, a link's up to 95), or answers an error, is said and the launch exits 1 without starting another app;
one still starting (holding its accounts before it serves) is asked again for 15 seconds. `--forget`, `--account`,
the lab and the read-only mode never hand off. The installed desktop entry runs `ngteams %u` and handles
`x-scheme-handler/msteams`, so a meeting link clicked in the browser comes here.

### UI and diagnostics

The Activity pane opens with Ctrl+1. Shift+Esc marks all activity as read; Ctrl+Alt+U toggles Unread and
Ctrl+Alt+M toggles @Mentions.

The three-pane window provides chats, teams/channels, a rich-text compose box, search and calendar. Ctrl+K opens
the chat/command switcher, Ctrl+E searches, Ctrl+2 focuses the chat list, and Ctrl+4 opens Calendar. Message-list
keys include `r` reply, `a` react, `e` edit own message, Delete own message, Ctrl+C copy and Escape clear selection.
The compose editor supports formatting, emoji, mentions and quotes; Enter sends, Shift+Enter inserts a line break,
and expanded formatting mode uses Ctrl+Enter to send. A meeting chat's header has Teams' Chat, Shared and Recap tabs:
Recap plays the meeting's recordings with their transcripts (search, download), as a recording's "View recap" opens
it, and its More options downloads a recording the account may download
([docs/protocol/recap.md](protocol/recap.md)). Its AI summary is made on this computer when asked for, laid out
as Fathom's call summaries (purpose, key takeaways, topics with their moments, next steps with owners, open
questions), by Claude Code or the Codex CLI with any model it takes (by default Claude Sonnet or GPT-6.1-Terra;
Settings → Calls → Meeting recaps, the same tool-and-model control as the captions'): the transcript then goes to that
tool and its provider. It is kept per recording, copied as Markdown, or sent to the
meeting's chat after a preview. Settings control appearance, notifications, privacy,
calls/devices and diagnostics. Native sandbox tests in `crates/ngteams/tests/{window,accounts_window,call_window,ui_tour}.rs`
exercise these views against the mock.
The focused Linux calendar layout check is `GPUI_SMOKE_REQUIRE=1 cargo test -p teams-ui --test calendar_window`;
it saves synthetic day/work-week shots at 1024px and 1280px in both themes under `target/ui-shots/calendar-*.png`.

Calls open a separate window; closing it leaves the call. Its toolbar follows Teams': Transfer and Hold (1:1 calls;
Resume while held), Chat (the call's chat beside the stage), People, Raise and React (group calls and meetings), View (Gallery or
Speaker, Focus on content while a share shows, Show videos first, Max gallery size; More options: Turn off other
videos, Hide me, Full screen), More, camera, microphone, Share (any call with video: "Share content", the screen or window picked at the
desktop's screen-cast portal; Stop sharing while shared) and Leave (End meeting, after a confirmation, for a meeting's
organizer). A 1:1 call the far end holds shows its avatar over "On hold"; the far end's shared screen (in a group call,
anyone's) takes the stage, the people beside it. The gallery shows equal 16:9 tiles, the account's own among them in a
group call, as many to a page as Max gallery size (4, 9, 16 or 49) and the window allow, the rest on pages turned
with `<` `>`; the strips beside a share and under the speaker page the same way, and nothing on the stage covers the
toolbar. `GPUI_SMOKE_REQUIRE=1 cargo test -p teams-ui --test call_large_window` saves calls of 10 to 50 people on
three screen sizes under `target/ui-shots/call-large-*.png`. People's "Invite someone" adds someone to a chat's call (a 1:1 call becomes a group call in a new
chat of the three); who doesn't answer shows "No response" with Call again. Transfer (1:1 calls) holds the call and
transfers it to someone picked; a call the far end transfers calls the target at once. More's Record and transcribe starts a recording with its transcript after asking the spoken language (video
and audio, or the audio alone), or a transcription on its own; Stop recording and Stop transcription, after asking,
stop anyone's. Show transcript opens the Transcript pane with the transcription's starts and stops (it opens by itself
for its starter); the recording lands in the chat as Teams saves it. More → Settings → Call health shows the call's
network, audio, video and screen sharing measures as its media reads them (nothing goes to Teams). The main window
keeps an in-call strip.
Ctrl+Shift+M mutes, Ctrl+Shift+O toggles camera, Ctrl+Shift+K raises or lowers the hand and Ctrl+Shift+H leaves.
Camera starts off; only the user's action turns it on.
Notifications, tray and device access use the session selected at startup: desktop for the user's normal app,
private sandbox for lab/tests. One call or ring is admitted at a time across the app's accounts.

The diagnostic file is `<data>/logs/teams-app.log` (the retained filename), mode `0600` in a `0700` directory,
rotated at 8 MiB with four older files. `TEAMS_APP_LOG=<error|warn|info|debug>` overrides the settings' level;
`trace` is treated as `debug`. Info stays payload-free. Debug includes every API request/response, including token
auth and authz calls, and both directions of every Trouter frame, with credentials redacted by `teams-log`.
It still holds message text, names and service answers: **never share a debug log or commit it, even sanitized**.
Copy diagnostics includes app/build/log details and recent diagnostic lines; any debug content is labelled unsafe
to paste. There is no separate write-decision log.

#### Translating the message you write

Every message box (a chat's, a channel's post and reply boxes, editing a message, New message) has Translate beside
Correct (Ctrl+Shift+Y), with the language button next to it. Translate replaces the draft with its translation, in
place and as one edit, so Ctrl+Z brings the draft back; mentions, links that show their address, inline code, blocks
of code, pictures and line breaks never go to the tool and come back as they were, and formatted words stay formatted.
While it runs, a line over the box shows the language and the tool with Cancel (Escape or the button again cancel
too); a failure keeps the draft and says why, with Try again, and with no tool installed it offers the settings. The
language button opens a searchable list of common languages, and any other language can be typed. The language is one
setting for every box and account in the app, kept across restarts; it and the tool are in Settings → General →
Message translation (the tool: Automatic, which is Claude Code when installed and else Codex, or either one; by
default Claude Sonnet or GPT-6.1-Terra). Only the draft's words and the language go to the tool and its provider,
never the chat, names or tokens (`crates/teams-ui/src/draft_translate.rs`).

## Captures and fixtures

A recorded run writes `captures/<scenario>/<ts>/events.jsonl` under the lab's data dir, never into the repo. Captures
never enter the repo, not even sanitized.

`export` writes a sanitized **reference copy**. It always refuses any destination inside a git work tree (checked with
`..` and symlinks resolved), a destination that is a symlink, and a `..` after a directory not yet created. The copy
never enters the repo, so what export's checks find in it is printed as warnings (kinds, placeholders, event and line
numbers, never a value) and the whole copy is written: a word (3 characters or more) of a name found in the capture
left in the result at any layer of encoding (an ordinary word that is also a name's word counts too; a name never seen
under a name field, or a shorter word, is outside that check), a line too deeply encoded to check, and anything the
fixture guard finds. A word of a name is never replaced inside a host, so the copy keeps every host intact: a public
Microsoft protocol host such as `teams.microsoft.com` is never taken for a name, and a word of a name that is also a
label of any other host is left there and counted in the warnings (it is still replaced everywhere else). Only a real
host counts: a URL's authority (as written or encoded), a `Host`-style header's value, a cookie's `Domain=`; a dotted
name (`Mei.Li`, `jun.hao.li`), a file name (`quill.notes.md`) or prose gets the name replaced, and that is counted in
the warnings too, since such a run may be a host nothing around it gave away. `--strict` turns every one of these
warnings back into a refusal that writes nothing. Teams meeting codes and join passcodes (of letters only too) become
`<meeting:N>` and `<token:N>` wherever they show, a host included, invitations read in every language the web client
has locale strings for, and the fixture guard flags a join link, a meeting id or a passcode left in the clear. The
sanitizer is defence in depth, with known gaps (`docs/backlog/sanitizer-defence-in-depth-known-gaps.md`).

Committed fixtures are written **synthetic-from-capture**: an agent reads the reference copy locally and writes a
fixture of the same shape with made-up values. A call's fixture holds made-up network data: addresses in the
documentation ranges (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`, `2001:db8::/32`), placeholders or values
that say they are made up (`SyntheticUfrag`) for ICE and TURN credentials, and a placeholder or one byte repeated
(`AA:AA:…`) for a DTLS fingerprint; the guard flags anything else. Fixtures live in any `tests/fixtures/` of the workspace: the top-level
one, a crate's, a spike's (`spikes/*/tests/fixtures/`). Every fixture declares itself synthetic: a JSON file has a
top-level `"_synthetic"` note (what it is shaped like, that the values are made up), a JSONL file has one on its first
line (a header line of its own, or on every line), and any other text file starts with a `_synthetic: …` line, bare or
after its own comment opener. Only an empty `.gitkeep` is not a fixture. `cargo test -p fixture-guard` fails on an
unmarked fixture (`committed_fixtures_are_marked_synthetic`) and on anything the guard finds in any `tests/fixtures/`
or `docs/` (`committed_fixtures_and_docs_are_clean`). The only directories exempt from the marker are test inputs that
are not shaped like anything Teams sends, listed in `fixture_guard::synthetic::TEST_INPUTS` (proto-lab's test pages
and mini bundle); the guard still scans them.

## Tests

Choose checks for what the change can break; [AGENTS.md](../AGENTS.md) defines the scope. Typical checks:

```sh
cargo fmt --all --check
cargo check -p <crate> --all-targets
cargo clippy -p <crate> --all-targets -- -D warnings
scripts/lint-report.sh
cargo test -p <crate> [--test <name>] [<filter>]
cargo test -p fixture-guard
python3 -B -m unittest discover -s scripts -p test_run_lab.py
```

The UI tour accepts `NGTEAMS_TOUR_FILTER=<comma-separated-step-names>` for a focused set of screenshots; include
any steps that set up the selected views. The `window` sandbox test accepts `NGTEAMS_STARTUP_DEVICES=blocking` or
`deferred` to time eager versus background native device discovery around the same mock-engine startup and first
window. Each run supplies a fresh empty `GST_REGISTRY` file; append `-slow` to either mode to add a synthetic
three-second discovery delay.

The widest local gates are `cargo clippy --workspace --all-targets -- -D warnings` and
`PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace`. Ordinary tests use synthetic mocks and
throwaway browser profiles. No live tenant write may come from a `cargo test` process; native live tests must use
the normally built lab app through `run-lab.sh` after the agent's check.

### Coverage

Install `cargo-llvm-cov` (`cargo install cargo-llvm-cov --locked`) and the pinned Rust toolchain's LLVM tools
(`rustup component add llvm-tools-preview`). The reporting helper requires Python 3.11 or newer; the wrapper uses
Bash and `flock`. Run a selected crate's tests and local coverage gate with:

```sh
scripts/coverage.sh -p teams-store
scripts/coverage.sh -p teams-store --require-target   # require 90% on every measured metric
scripts/coverage.sh -p teams-log --no-gate            # measure a crate before establishing its baseline
scripts/coverage.sh -p teams-log --test-threads 3     # explicitly change test-harness concurrency
scripts/coverage.sh -p teams-log --profile-mode ordinary  # diagnostic exit-time profiling
scripts/coverage.sh --webrtc-cache /absolute/path/to/linux-x64-release  # reuse a matching SDK
python3 -B -m unittest discover -s scripts -p test_coverage_report.py
```

Coverage runs every selected test with `--test-threads 1` by default. Overlapping harness cases can fork while
another case still holds a generated fake-browser script open for writing, causing `ExecutableFileBusy` when it
starts. Serial execution avoids that overlap in coverage runs; ordinary `cargo test` keeps its configured harness
concurrency. `--test-threads N` accepts a positive integer to change the harness concurrency; concurrency exercised
inside a test stays part of that test.

`coverage.toml` holds the 90% target and measured standalone baseline floors for all 17 runtime crates. Raise floors
as tests improve; never lower them or exclude runnable code to pass. Baselines are measured with each crate
selected alone. Some standalone floors remain below 90%; the full workspace also exercises libraries through their consumers' integration suites and can cover more code.
`--require-target` always enforces 90% for every selected crate, independently of those baseline floors.
The [Coverage CI workflow](ci.md) requires the 90% target on pull requests and `main`, using hosted CPU runners
and software graphics; only the hardware DMA test is excluded there. Running the script without
`-p` selects all 17 crates with runtime code, including the lab and test-support tooling. Generated WPE bindings and
the standalone video spike are excluded. Retained typed-construction compile checks run in `teams-proto` and
production-only mock-origin/registrar checks run in `teams-trouter` as part of the normal test gate.
Third-party code and tests outside `src/` do not enter the totals.

Baseline floors are measured and validated with one `-p <crate>` alone. Combined runs also count hits from dependent
crates' tests, so use them to find gaps; their higher totals cannot establish or validate an individual crate's floor.

On Linux the wrapper uses LLVM continuous profiling with counter relocation and `%p-%m%c.profraw`, verified on the
pinned Rust toolchain. Counters stay in a mapped profile when a sandbox worker is killed, so its executed code still
counts. Existing `RUSTFLAGS` or the higher-precedence `CARGO_ENCODED_RUSTFLAGS` are preserved when adding relocation.
See [LLVM's continuous profiling documentation](https://clang.llvm.org/docs/SourceBasedCodeCoverage.html#running-the-instrumented-program).
`--profile-mode ordinary` selects exit-time profiling for diagnostics; killed workers can lose their counters in that
mode. Other platforms default to ordinary mode; continuous mode is verified and accepted only on Linux.

Continuous reports go to `target/coverage/continuous/<selection>/llvm.json` and `summary.json`, with instrumented builds
in `target/coverage-continuous-build`. Ordinary mode keeps separate reports in `target/coverage/<selection>/` and builds
in `target/coverage-build`. One crate uses its name, the full workspace uses `all`, and long partial selections use a
short hash. The summary records every selected package and ranks source files by uncovered lines, functions and regions.
Measurements serialize on one lock across both modes. Before each run, the wrapper clears workspace artifacts from
the instrumentation build so LLVM cannot count obsolete code embedded in an unselected consumer; third-party builds
remain cached. `--webrtc-cache DIR` passes an existing SDK with `include/` and
`lib/libwebrtc.a` to the upstream build as `LK_CUSTOM_WEBRTC`; relative paths resolve from the caller's directory.
Choose the SDK version and platform matching the locked dependency. The wrapper does not select or download an SDK
itself, and preserves an existing `LK_CUSTOM_WEBRTC` when this option is absent. `--report-only` rechecks the selected
mode's existing export without running tests; use a fresh measurement for change validation. Every executable selected
`src/**/*.rs` must appear in the LLVM export, including
zero-hit files: missing files fail even with `--no-gate`, so omitted feature-gated code cannot silently inflate
coverage. `coverage.toml` lists individually reviewed declaration/type-only files and separate unit-test modules that
LLVM does not map because `cargo-llvm-cov` filters test filenames such as `tests.rs`. Each entry records its SHA-256
and reason; test modules also pin the file that includes them under `cfg(test)`. A macro invocation whose executable
bodies are mapped at its definition pins that definition's file, which remains required in the report. A content
change invalidates the review and fails the report until inspected again. The summary lists these audited unmapped
files explicitly. If LLVM supplies a mapping for one, its counts still contribute to the totals; the review never
discards reported executable code.

Stable Rust supplies line, function and region coverage here; regions are not statements or branches. Inline unit
test functions in `src/` also appear in LLVM's source totals. Measurement uses default features and the
features enabled by development dependencies, not a merged feature matrix. Code behind `test-support`
or other optional features still needs appropriate mock-backed variant measurements before claiming full coverage.
The wrapper never enables `live` and removes live-test and external TURN-server environment variables. Browser and
GUI tests need the local dependencies below; the wrapper sets `PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1`
so missing dependencies fail rather than skip tests. Never run tenant activity for coverage.

Coverage builds also make the profiling output directory writable inside WebKit's subprocess sandbox; otherwise the
extension cannot persist its profiles in the host's output directory. Ordinary builds do not add this
directory. The extension's standalone suite shares the sign-in window's offline sandbox scenarios to exercise its real
WebKit callbacks, alongside socket, protocol and GLib boundary tests.

The automatic `teams-auth` private-keyring test requires `/usr/bin/dbus-run-session`, `/usr/bin/gnome-keyring-daemon`
and `/usr/bin/kill`. It starts a private bus and keyring service with temporary HOME/XDG directories, without using
the desktop keyring. The separate manual user-keyring test remains ignored by default.

`cargo test --workspace` is offline: native APIs and calls use `teams-mock`, browser captures/holds use local
pages in Chrome, and GUI/media tests use their private sandbox. No personal files, profiles, credentials or keyring
are needed. The automatic auth keyring test uses a private bus/keyring under temporary HOME/XDG; the separate
user-keyring test is ignored by default.

| variable / flag | effect |
|---|---|
| `PROTO_LAB_CHROME=<binary>` | browser for offline tests; defaults to the harness's pinned install |
| `PROTO_LAB_REQUIRE_CHROME=1` | missing Chrome fails instead of skipping |
| `GPUI_SMOKE_REQUIRE=1` | missing Weston/niri/private bus/PipeWire dependencies fail instead of skipping |
| `TEAMS_WPE_TEST_SKIP_DMA=1` | CPU CI omits only native WPE's real DMA subcase; runtime source remains counted; unset locally |
| `WGPU_BACKEND=vulkan` | CPU CI uses Mesa Lavapipe; sandbox forwards this graphics setting |
| `--features live` and `PROTO_LAB_LIVE=1` | explicitly enable proto-lab's optional auth live test; use the live lock and outer timeout |
| `TEAMS_APP_LOG=<level>` | diagnostic level for a client run; debug logs remain private |
| `TEAMS_MEDIA_TURN_SERVER`, `TEAMS_MEDIA_TURN_USER`, `TEAMS_MEDIA_TURN_PASSWORD` | optional explicit real TURN relay tests, outside the ordinary offline gate |
| `GPUI_BENCH_GPU`, `GPUI_BENCH_SECONDS`, `GPUI_BENCH_UPLOAD`, `GPUI_BENCH_OUT` | standalone video-grid benchmark settings |

Every GUI test uses its own headless Weston/niri compositor, private bus and no-op URI opener. PipeWire tests use
private synthetic microphones/speakers/cameras; host-session canaries check isolation. The native WPE tests use
synthetic pages in worker/in-process SHM and real DMA modes. DMA needs a working render node/EGL driver; no external
page or network service is used. Coverage explicitly requires browser and sandbox dependencies.

### Known flakes under load

Chrome startup and `browser::a_browser_that_exits_early_is_reported` can fail on timing under load. Rerun the failed
test alone to establish its cause; do not treat a lab-contact violation as a timing flake.


### Windows VM runner

Windows lab builds use `pwsh -NoProfile -File scripts/run-lab.ps1 -BuildOnly`; a normal build always sets only the
lab marker, never the personal install marker. The runner also builds the sibling `ngteams-webview.exe` sign-in worker.
Prerequisites are the repository Rust toolchain, MSVC C++/Windows
SDK and the native media development libraries. The display-free `teams-runner` helper has its own bounded build
in `target/runner`; the normally built app goes to `target/lab`.

The media SDK is the official GStreamer MSVC x64 build pinned in
`packaging/windows/media-pins.json`. Install it with `/CURRENTUSER /VERYSILENT /NORESTART /TYPE=devel` into
`windows-deps/gstreamer`, checking the pin's SHA-256 first. Also install the repository toolchain's
`llvm-tools-preview` component and Python 3. The lab build then runs `scripts/package-windows-media.ps1` to assemble
`target/windows-media-runtime`; an existing runtime is verified, never silently replaced. To rebuild it after a
policy/SDK change, retain the old directory for diagnosis and package to a new output directory.

`packaging/windows/gstreamer-plugins.txt` records the production plugin roots. The packager reads normal **and delay**
PE imports with LLVM, copies every non-OS dependency recursively, and refuses anything unresolved. Only the declared
Windows 11 components/API sets remain external; the MSVC CRT comes from VS's x64 redistributable directory. SDK
licenses, exact component versions, the hash-verified Microsoft runtime license and a per-file SHA-256 manifest are
included. The standard `bin`, `lib/gstreamer-1.0` and `libexec/gstreamer-1.0` layout also works when the SDK is absent.
Lab launches use that private runtime with a per-alias registry. The lab-contact rule still applies to live runs.

The Windows decoder equivalents are libav for AAC/HEVC and dav1d/libav for AV1; WASAPI2 and Media Foundation provide
the native devices. Hardware video decoding remains unranked by the player on both platforms. The Linux WPE
manifest additionally serves WebKit's Web Audio/WebCodecs/MediaRecorder; Windows sign-in uses WebView2. The client's
inline player is for video attachments and recordings. Call RTP, screen capture and background segmentation use
libwebrtc, Windows Graphics Capture and the embedded Rust CPU/wgpu model respectively, with no extra GStreamer
device/effects runtime to install.

`python scripts/make-media-samples.py <directory>` creates 21 disposable FFmpeg test-pattern/tone samples, with no
captured input. Copy that directory into the VM and run
`pwsh -NoProfile -File scripts/test-windows-media.ps1 -Samples <directory>`. This offline console gate checks the
default runtime layout without SDK paths, a fresh complete plugin scan/blacklist, each plugin's actual path, native
device and decoder factories, every generated container/codec, and the real player's MP4/fragmented-recording and
background-effects tests. It does not capture a physical microphone/camera or reach a tenant. Each process has a
deadline and the build/run stops below 1.5 GiB free; diagnostics remain in the private `target/windows-media-smoke-*`
directory. Targets link sequentially using `cargo rustc --profile test`; only their final links omit the large
native PDBs. Dependencies retain their normal debug settings and compiled/static caches. Run
`python -B scripts/test_windows_media_closure.py` for the display-free PE packaging contracts.

The only Windows UI sandbox authorized for agents is the **win11 VM's interactive console**. After the lab-contact
check, `scripts/run-lab.ps1 a` builds then schedules a Limited InteractiveToken action for the already logged-in
user, without a password. The native console guard refuses an SSH/session-zero window. Provision exactly one lab
account in `%LOCALAPPDATA%/ngteams-lab/a/accounts.toml` first, as on Linux. `-Status` and `-Stop` inspect/stop that
alias; `-TimeoutSeconds 1800` sets its deadline. The VM's normal screenshot/input tools operate its console; never
enter credentials with a typing helper. Use only the authorized VM, never a physical Windows desktop.

`pwsh scripts/provision-lab.ps1 -Command Browser` checks the installed system Edge's Microsoft signature and product
identity. The Windows `proto-lab` command set contains only lab browser verification, native login/provisioning and
read-only reach evidence; Linux keeps its full capture/scenario CLI. Both reuse our CDP connection, login decisions,
redirect catcher, PKCE, identity checks and token publication. Windows launches Edge headless in a separate protected
`%LOCALAPPDATA%/teams-lab/profiles/<a|b|c>` profile, in an atomically assigned kill-on-close job. It never enables
password/debugging input in the shipped WebView2 worker.

After the lab-contact check, prepare `%LOCALAPPDATA%/teams-lab` privately (`teams-runner prepare --directory <path>`),
place the approved roster in `lab.toml`, and transfer only the required lab account entries as `accounts.json` over
scp, never as command text or a password argument. Protect the input file's owner and DACL for the console user and
SYSTEM before transfer; the loader checks the opened handle and refuses broad grants, a different owner or an
unmatched roster email without repairing it. Remove that password input from the VM when validation ends.
`pwsh scripts/provision-lab.ps1 -Command Login -Alias a -NativeAlias <runner-alias>` runs the dedicated one-password
state machine on Microsoft's native authorization page, stops for MFA/rejection, checks the redirect/state/ID token,
and publishes native account/region files and tokens in the VM's `ngteams-lab` Credential Manager namespace. It
never opens the Teams web client. `-Command Auth` instead reuses an already authenticated browser profile without
typing a password. `-Command Refresh` instead completes authz and region publication from a stored refresh token,
after matching the previously verified native identity against the roster; it opens no browser. Tokens remain in the VM.
The scheduled console action is bounded, uses the existing interactive user, owns its browser descendants and holds the same exclusive live lock used by Windows capture tools.

Before starting a signed-in engine, run `-Command Reach -Alias a -NativeAlias <runner-alias> -Chat <checked-chat-id>` under a fresh lab-contact
check. It follows every conversation page and reads actual member lists through the production GET transport;
`reach-a.json` privately records roster completeness and allowed actor labels separately. Only explicitly selected
targets gate the run: incomplete rosters or unlisted people on a selected target fail the operator check. A visible
channel outside the allowed set remains inventory, never a target to open or act in. Review that evidence and any
intended meeting/call participants before
using `run-lab.ps1`; the installed client has the ordinary Teams feature policy.

The runner retains a kernel sharing lock per alias and a shared `%LOCALAPPDATA%/teams-lab/live.lock` for its entire
lifetime and cleanup. Windows capture tools must open the latter with read/write access and no sharing for their
exclusive lock; Linux `flock` and Windows file-sharing locks are separate OS domains. Global alias paths make
worktrees contend for the same lock. It creates the broker pair itself, transfers only the required endpoint and
trace into each child, and keeps its original endpoints alive across restarts. App and broker have private unnamed
kill-on-close Job Objects. Job assignment is atomic with process creation; app restart images and WebView workers
remain in the app job. Cleanup waits until its jobs contain no processes before releasing locks. `holder.log` and
`trace.jsonl` are private and retained; they are never repository artifacts.

For an **already installed** personal Windows executable, `scripts/run-personal-read-only.ps1 <upn> -App C:/path/to/ngteams/bin/ngteams.exe` uses the same VM console, the app's consistent snapshot and its get-only broker. It does
not build, install or sign in the personal client. Snapshot data is under `%LOCALAPPDATA%/ngteams-read-only/run/`,
removed after orderly completion or `-Stop`; a failed/crashed run retains diagnostic evidence until stopped.
It uses the installed package's adjacent media runtime and manifest.

Offline lifecycle tests use a tiny synthetic native executable, no GUI, network or credentials:
`cargo test -p teams-runner` and `scripts/test-windows-runner.ps1 -Runner <absolute-helper.exe>`. The latter adds
actual CLI dispatch and console scheduling in the VM; `-SkipConsole` checks only the display-free paths on hosted CI.


### Windows sign-in checks

`teams-signin` selects native WebView2 on Windows; Linux retains the offscreen WPE worker. The native worker uses
private standard-stream pipes to the same in-process guard, starts in a private kill-on-close job, and receives
neither the broker nor view-trace handles. Its private, InPrivate browser profile is deleted after every browser
process exits, including readonly Chromium cache metadata. Environment and registry overrides that could change
the browser or proxy are refused. Request filters cover every document/worker source; response interception adds
CSP restrictions, and document-created scripts remove unguarded network constructors in every frame. Proxy
settings remove Chromium's implicit loopback bypass. TLS errors, ambient HTTP authentication, client certificates,
external protocols, permissions, popups and downloads are refused in production.

The Windows lab runner still requires the authorized VM console and the usual contact check for a live app run.
For an offline native sign-in check, run `pwsh -NoProfile -File scripts/test-windows-signin.ps1` in that VM. It
builds the test harness, then schedules its GUI in the existing Limited InteractiveToken console. `-HoldForShot`
keeps the synthetic page visible longer for a VM screenshot. It uses only a self-signed numerical-loopback HTTPS
fixture: the test-support worker permits that certificate alone, without changing Windows trust. The production
proxy configuration is used even for loopback. Assertions cover per-request logging, allowed and denied POSTs,
synchronous/POST redirects, main/frame/immediate-about-blank constructor restrictions, immutable restrictions,
proxy tunnel counts, redirect-code redaction and profile/process cleanup. Synthetic evidence stays in the ignored
`target/windows-signin-smoke-*` directories. No account, credential or tenant is used.

Display-free Windows checks use `cargo test -p teams-signin --lib --test classify --test gate --test native_worker`.
The normal lab application builds and opens the native Microsoft sign-in page. Signed-in feature validation is separate from these offline synthetic contracts.

### Windows application contracts

`pwsh -NoProfile -File scripts/test-windows-app.ps1` builds and executes the synthetic app and UI contracts in the
authorized win11 console. `-Group app`, `-Group ui` or `-Group scenes` selects app contracts, shared model/view contracts
or real rendered scenes. `-Only` accepts exact inventory keys such as `ngteams-test-descriptors` or
`teams-ui-scene-font_styles` to rerun failed targets. The app and UI groups discover their shared integration tests
from each crate's test directory, including calls, calendar and control events; Linux compositor/bus harnesses and
the separately driven native Windows fixtures are excluded from that discovery. It checks actual native descriptor
ownership/inheritance and restart, account/sign-in persistence, read-only and lab broker behavior, startup, control
IPC and handoff, settings privacy and the translation/correction/recap models and views. All identities and services
are synthetic; no installed AI tool or tenant is reached. Its evidence and executable inventory stay under a private
`target/windows-app-smoke-*` directory. Builds retain dependency debugging and omit only each selected test image's
final PDB; deadlines, the 1.5 GiB free-space guard and scheduled-task cleanup cover each run.

Fake CLI protocols use the same scripts on both platforms. Windows launches them through native `.cmd` shims, with
Git for Windows' `Program Files/Git/bin/bash.exe` interpreting the synthetic protocol. This exercises the app's actual
prompt-file adaptation and process-tree cancellation; process liveness is checked in Bash's PID namespace before
and after cancellation. The Trouter production-dependency contract uses Git's `usr/bin/timeout.exe` for the same
300-second child deadline as Linux. Windows privacy assertions inspect actual ACL grants, and publication-failure tests use
native file-sharing rules. Chat list, notes, meetings, recap, channels, workflows and font-style scenes share their
real rendering code in `tests/support/scenes/`; their small fixture boundary selects the private compositor or native Windows window capture
and close. Windows snapshots verify the child executable and console session, refuse an overlaid secure desktop,
use physical DPI coordinates and reject client areas clipped by the work area or covered by another input window.
They crop only the foreground client area and retain its process tree until cleanup. `RECAP_PAGES=settings,caption-settings,speed`
selects those recap scene pages on both platforms; the Windows driver passes that selection into its console task.
Font checks use native installed
families, one family per short Windows
window to keep shell overlays outside the measured pixels. The Wayland drag, portal, D-Bus and Linux app-runner
harnesses stay in `tests/support/linux/`; Windows has native desktop/shell/video fixtures. Shared model/view tests
remain available on both platforms. These offline gates do not establish signed-in feature behavior or physical devices.

### Windows video and local tools

`pwsh -NoProfile -File scripts/test-windows-video.ps1` runs a synthetic GPUI video fixture in the authorized win11
console. It uses numerical BGRA quadrants without an account, device or network. It checks that each replacement
uploads one image, repainting reuses it, fully clipped frames submit nothing, and both replacement and hiding evict the previous atlas entry. `-HoldForShot`
keeps the final synthetic frame visible briefly and saves its foreground client area as `frame.png` in the private run directory. The pinned Windows GPUI crate has a test-support atlas inspection patch, verified by `scripts/vendor-crates.sh --check`, so these checks query the actual DirectX atlas. Linux retains its persistent external
surface and GPU completion trace; Windows records `video-tile submitted` after a successful image paint because
GPUI's image API has no GPU-completion callback. Neither means a remote participant received a frame.

Local AI tools are discovered as native executables or Windows npm `.cmd`/`.bat` shims. Their arguments use the
standard library's native escaping. Each command starts suspended, enters a private non-breakaway job, then resumes;
closing the owner terminates its descendants even after the first process exits. Model caches use `USERPROFILE`
on Windows. Claude batch shims receive multiline instructions through its documented `--system-prompt-file` option
in a private temporary directory, removed after its process tree exits. Settings, recap summaries and print pages use the platform's private file creation/restriction helpers;
print links use encoded file URLs, including Windows drive paths.

Both GUI executables embed the application icon, share an explicit Windows taskbar identity and use the Windows
GUI subsystem. The complete debug application exceeds MSVC's PDB debug-record limit (LNK1140) with the full media/AI
and UI dependencies even with line tables only. Its Windows final link omits the PDB; runtime diagnostic logging
is unchanged. Linux and dependency/test debugging retain their usual settings. Release builds already omit debug
information. Signed-in application flows remain a separate gate; release packaging is described below.

The initial sign-in installs the same private diagnostic logger before opening its page. Chromium rejects an
explicitly empty response phrase when a paused response's headers are continued; HTTP/2 can supply that empty
field. The response policy omits it and lets Chromium supply the status's standard phrase, preserving the original
status and headers plus the additional restrictions. The native HTTPS contract includes an empty-phrase response.

Windows desktop integration uses WinRT toasts and the native notification area rather than a session bus.
The same notifier handles mentions, collapsed messages, translations, focused-chat suppression and notification
settings on both platforms. Notification IDs are shared across accounts; activations are handed to the existing account/chat/channel and
explicit Answer/Decline handlers. Windows receives fixed action names, opaque per-run/per-notice keys and numeric
tags, with escaped plain display text; notification content never enters the view trace. The unpackaged app registers
its name/icon, matching Start-menu shortcut and COM activation class under its per-user AppUserModelID. COM callbacks
queue only arguments; the notification thread validates the identity, notice generation and fixed action before
routing it. Replaced, withdrawn, foreign and unknown activations are ignored.
Lab and read-only processes use distinct temporary identities and shortcuts, so concurrent runners do not share a
COM endpoint or alter the installed client's shell registration. Those temporary registrations are removed on
shutdown. The same selected identity groups the main window and its native sign-in worker, including the first
sign-in before a main window exists. Message sounds follow Windows notification preferences; the app's ringer owns call audio.
The running app owns its toasts and withdraws them on normal shutdown. A fresh run never accepts an old call action.
Windows’ COM `-Embedding` startup marker is filtered before argument parsing; other arguments, including all lab and
read-only runner requirements, still pass through the ordinary command line checks.

The Windows tray offers Open Teams and Quit. Its icon and taskbar overlay follow the combined unread-chat count,
including a main window reopened from the tray; the icon re-registers after Explorer restarts.
The main HWND also forwards `TaskbarButtonCreated` to the tray thread, which reapplies the current unread overlay
when Explorer has recreated that button. Without a tray item,
closing the main window retains the existing quit behavior. Read-only mode creates no tray item. Native shell work
runs on dedicated threads and returns actions to GPUI; it builds no Teams requests.

`scripts/test-windows-desktop.ps1` runs account-free toast/history, native toast action, tray callback and taskbar contracts in the
explicitly authorized win11 console. It never signs in or opens a microphone or camera. Like the other native fixture
runners, it creates only a Limited InteractiveToken task and removes that task when the bounded run ends.

The shell fixture requires visible synthetic message and incoming-call toasts: body, Answer and Decline each invoke
the native shell control and must return the exact notification ID/action through COM. UI Automation examines only
immediate desktop children to locate toast roots, then searches their trees for the exact synthetic title. It waits
for visible controls and stable geometry before the console helper captures that verified rectangle. A history entry
alone cannot pass. The fixture also tests stale/foreign COM arguments, history replacement and withdrawal, tray
callbacks and taskbar APIs.

`scripts/test-windows-shell.ps1` exercises the production GPUI Desktop over two synthetic accounts in that console.
It checks real window activation after minimizing, close to tray, the visible native tray/menu's Open Teams action,
window recreation with its selected account and theme, account-specific settings/chat routing, and combined unread
counts. It withdraws only its own tray icon and sends `TaskbarCreated` synchronously to its own HWND, requiring the
icon to return before it can open the menu. Explorer itself is never restarted. The window procedure queues sent
shell callbacks/broadcasts as well as posted ones: `GetMessage` dispatches sent messages without returning them to
the outer loop. The fixture saves the visible tray/menu rectangles, app taskbar button before/after unread changes
and recreation, and the foreground synthetic client in Light/Dark. UI Automation uses bounded provider calls and
known shell HWNDs, with a unique per-run synthetic icon label; capture includes Explorer's layered windows.
Pass `-Executable <absolute-native_shell-test.exe>` to rerun an already-built fixture. Its GUI subsystem prevents a
driver console from taking focus away from the popup. Signed-in application flows remain a separate Windows gate.


### Windows release packaging

The Windows job in `.github/workflows/release.yml` builds the install-marked app, `ngteams-webview.exe` and
`ngteamsctl.exe` on `windows-2025`, using the same workspace-version/run-number and source commit as Linux.
Both build jobs have read-only permissions. The publish job requires both artifacts and runs only for
`refs/heads/main`; a branch dispatch builds artifacts without publishing. The Windows zip joins the Linux assets
in the release's checksums and build-provenance attestation. No agent builds the install-marked client locally.

`scripts/release-windows-deps.ps1` installs the hash-pinned GStreamer development SDK. After the hosted binaries
are built, `scripts/package-release-windows.ps1 -Binaries target/release -Version <major.minor.patch.run> -Commit
<full-commit>` assembles the verified media closure, collects notices from the resolved Windows Cargo graph and
libwebrtc, and stages all three executables under `bin/`. `scripts/windows-release.py` checks every PE's ordinary
and delay imports against that private tree, and that `ngteams.exe` reserves 8 MiB for its main (GPUI) thread's
stack, as Linux gives it, instead of MSVC's 1 MiB default. `PACKAGE-MANIFEST.json` hashes every shipped file, including the
unchanged media manifest. By default the media closure uses a unique system temporary directory, removed after
packaging, outside Cargo's `target/` tree. A restored rust-cache may contain an empty old `target/windows-media-release`
directory; the packager never consults it. An explicit `-Runtime` remains a verified caller-owned input.
Missing/extra/changed files, unresolved DLLs and stale plugin roots fail packaging;
staging and the archive must fit above the existing 1.5 GiB disk guard.

The zip contains Windows PowerShell 5.1-compatible `Install.ps1` and `Unregister.ps1`. Setup verifies the extracted
tree, copies it to a versioned per-user program directory, creates a Start shortcut and registers an available
MSTeams handler through Windows Default Apps capabilities. It does not replace the current default or start the
client. The first normal startup adds the shortcut's notification properties and COM registration. Setup detects
WebView2 using Microsoft's documented per-user/per-machine registry values; if missing it downloads the official
Evergreen bootstrapper, checks its Microsoft Authenticode signature and runs its bounded per-user installation.
Unregister removes only metadata still pointing at that package version and retains account files/credentials.
See the [Microsoft deployment guide](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution).

`python -B scripts/test_windows_release.py` exercises real x64 PE delayed/transitive dependencies, archive contents,
metadata, corruption, missing binaries, existing-output protection and the disk guard. The workflow also unpacks
and verifies its zip, checks build identity and CLI help, and runs `Install.ps1 -VerifyOnly`. No GUI, account,
installer execution or tenant access is needed for these checks. Agents can package normally built lab binaries
with the same scripts and `-Runtime target/windows-media-runtime`; they never install or start them as a personal
client. The signed-in VM checks and physical device/GPU limits remain separate from package integrity.

### macOS host runner

The macOS port (Apple silicon, macOS 14 or later) is debugged on a Mac over SSH; its logged-in console is this port's
GUI place, as the win11 VM's console is Windows', only through the lab runner below ([AGENTS.md](../AGENTS.md)'s
sandbox rule): lab-marked builds; lab accounts within the [lab-contact rule](lab-rules.md) (or a synthetic alias);
never the user's own account or its Keychain items; short runs, every window and process closed after. Prerequisites
there: Xcode's command line tools (the `security`, `codesign`,
`iconutil` and `hdiutil` tools; GPUI compiles its Metal shaders at run time, so no Metal Toolchain), rustup with the
repository toolchain, Python 3, and GStreamer's official macOS runtime in a private prefix:

```sh
scripts/macos-gstreamer.sh ~/ngteams-agent/gstreamer      # pinned in packaging/macos/media-pins.json, no admin
export PKG_CONFIG_PATH=~/ngteams-agent/gstreamer/lib/pkgconfig
```

The runtime package has no pkg-config files (GStreamer's devel package, which has them, hard-codes
`/Library/Frameworks`), so the script writes the few the Rust bindings ask for. Its libraries are named `@rpath/...`:
a development binary or test finds them with `DYLD_FALLBACK_LIBRARY_PATH=<prefix>/lib` (the lab runner sets it),
`ngteams.app` through its own rpath; GStreamer finds its plugins beside its library. Bring the branch over as for the Windows VM: a
`git bundle` copied with `scp` into a directory of your own on the Mac (or `git fetch` from it over SSH), never a push
to GitHub. Mac disks are small: use one `CARGO_TARGET_DIR`, `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0`.
A lab build needs ~7.5 GB free at its peak (with less, its last link or strip fails and leaves a broken program, which
`run-lab-macos.py` refuses and removes for the next build). Each test command below needs up to ~8 GB in a fresh
target dir, and several sharing one dir pass 9 GB: run them one at a time and delete the target dir between.

```sh
cargo check --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked -p teams-platform -p teams-log -p teams-store -p teams-auth -p teams-ctl -p teams-runner \
    -p teams-update -- --test-threads=1
NGTEAMS_KEYCHAIN_TESTS=1 cargo test -p teams-auth --lib keychain   # needs an unlocked login keychain (not over SSH)
scripts/run-lab-macos.py --build-only                     # lab-marked ngteams and ngteams-webview in target/lab
```

What macOS has of its own: data and config under `~/Library/Application Support/ngteams/{data,config}` (the lab's
aliases in `ngteams-lab/<alias>/` beside it), the control socket under `$TMPDIR/ngteams/`, credentials as Keychain
generic passwords (service `<namespace>: <audience>`, account the UPN; `crates/teams-auth/src/secrets/macos.rs`), the
sign-in window in the `ngteams-webview` worker (`WKWebView` on a non-persistent store whose proxy is the host gate;
`crates/teams-signin/src/macos/`), devices through GStreamer's `osxaudio` and `avfvideosrc` on the Windows backend
(`crates/teams-media/src/devices/native.rs`), video through GPUI's image path, and the background-effects model on
Metal. Linux-only crates (WPE, the UI sandbox) compile to nothing there. The desktop (`crates/teams-ui/src/desktop/
macos.rs`, `crates/ngteams/src/mac_app.rs`): notifications the app posts to the `UNUserNotificationCenter` with the
banner's picture as an attachment, while GPUI's binding hands back responses and dismissals (only for the app started
from `ngteams.app`; a bare binary gets none), the unread total on the Dock badge, the Dock in the tray's place
(closing the main window keeps the app, a Dock click shows it again), the menu bar (⌘Q, ⌘H, ⌘M, ⌘W) and
`AppKit`'s terminate (the Dock's Quit, a logout) routed through the app's own quit. Shortcuts bind `secondary-`
(⌘ on macOS, Ctrl elsewhere); the message box adds macOS's ⌥/⌘ arrow keys.

`scripts/run-lab-macos.py <alias>` runs a lab alias on the console: lab-marked builds only, its broker and app on a
private socket pair, `--shot <name>` saving the run's own windows (`screencapture -l`, which needs the Screen
Recording permission for the SSH session's process), `--status` and `--stop`; the app serves its control socket where
`ngteamsctl --lab <alias>` looks for it; runs are short and every process stops with the run. State the lab-contact check before a run of a lab account; a synthetic alias (an `example.invalid`
account, no tokens) stops at the sign-in window and reaches nobody. Lab devices stay refused on macOS (no device
sandbox). With `--app`, the runner takes only a lab bundle (below) and takes it out of LaunchServices after the run.
On a locked console (the usual state over SSH) `AppKit`'s close animation never ends, so the window server keeps
listing a window the app has closed as on screen (CGWindowList), a few points larger; an `--app` run turns the lab
bundle's window animations off while it runs, so check windows opening and closing on such a run.

A lab account is signed in by the lab tooling, as on Linux; nobody types a password into the app:

```sh
cargo build --locked -p proto-lab --target-dir target/lab
# ~/.config/teams-lab/lab.toml as on Linux; accounts.json (0600) with only the accounts to sign in
proto-lab chrome install       # the pinned Chrome for Testing for mac-arm64, under ~/.local/share/teams-lab/chrome
proto-lab login a              # headless: the lab password from accounts.json, typed into Microsoft's page
rm ~/.config/teams-lab/accounts.json
proto-lab auth a               # tokens, accounts.toml and region where the Mac lab build reads them
python3 scripts/run-lab-macos.py a --timeout 600
```

`proto-lab auth` on macOS writes the alias to `~/Library/Application Support/ngteams-lab/<alias>/` and its tokens to
the lab keychain, `~/Library/Application Support/ngteams-lab/ngteams-lab.keychain-db`
(`crates/teams-auth/src/secrets/macos_lab.rs`): the `ngteams-lab` namespace never uses the login keychain, which is
locked over SSH. The lab keychain opens with a fixed password, isn't in the user's keychain search list, and its items
are readable by any of the lab's programs (`proto-lab` writes them, the lab build's broker reads them). The lab
browser uses a mock keychain (`--use-mock-keychain`), never the login keychain.

After the run, `--stop` it, then remove what the sign-in made: the alias
(`rm -rf ~/Library/Application\ Support/ngteams-lab/<alias>`), its tokens (the lab keychain file, or one alias's
items with `security unlock-keychain -p ngteams-lab <keychain>` and `security delete-generic-password -a <upn>
<keychain>` until none is left), the browser profile, captures and Chrome under `~/.local/share/teams-lab/`, and
`~/.config/teams-lab/`.

`scripts/package-macos.sh --binaries <dir> --gstreamer <prefix> --version <v> --out <dir>` assembles
`ngteams.app` (the three binaries, the runtime's libraries and the plugins `packaging/macos/gstreamer-plugins.txt`
names under `Contents/Frameworks/gstreamer` (a plain folder: codesign takes a `.framework` only in its bundle form), the Teams-derived icon, `Info.plist` with the camera and
microphone usage descriptions and the `msteams:` scheme, the release marker the updater reads), signs it and packs
a `.zip` and a `.dmg`: ad hoc, or with `NGTEAMS_MACOS_SIGN_IDENTITY` a Developer ID with the hardened runtime, and
with the `NGTEAMS_NOTARY_*` key notarized and stapled ([CI guide](ci.md#macos-signing)). Ad hoc, a user opens the app
the first time with right-click → Open. Build its binaries first (`run-lab-macos.py --build-only`, then
`cargo build -p teams-ctl --bin ngteamsctl` into the same target dir). Lab-marked binaries (`ngteams --build-marker`
says `lab`) make a lab bundle: id `com.nickalie.ngteams.lab` and no URL scheme, so LaunchServices never hands it a
Teams link or the personal app's notifications. Running a bundle registers it with LaunchServices: take a copy out
with `lsregister -u <ngteams.app>` before deleting it.
