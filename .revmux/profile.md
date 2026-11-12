# Review profile: ms-teams

## What it is

- A native Microsoft Teams client for Linux (Rust + GPUI) written against Teams' internal APIs, with the web client
  as the reference. Personal project, one maintainer, most code written by agents.
- Done: the protocol lab (Chrome for Testing under an armed fence), the headless chat core, and the GPUI client the
  user drives daily. In progress: calls (the plan in `docs/plans/` that isn't under `completed/`).
- It runs live against a **real company tenant** through three dedicated lab accounts (a, b, c). Its writes reach
  only those accounts and the echo service; the fence (`teams-fence`, the browser fence) enforces that, and its
  design is settled.
- The lab accounts are known bot accounts. Nobody outside the lab calls, messages or invites them. A failure that
  needs an outsider to contact a lab account is not a realistic case.

## What a real failure looks like

- **A fence escape (always critical).** A contacting write (AGENTS.md's list: a message, edit, delete, reaction, call
  start, join or answer, chat created, member added or removed, invite) actually leaves without passing the fence,
  on a path the change makes reachable. Also critical: anything that mints a `WritePermit` outside `Fence::check`;
  an `Unknown` treated as allowed; a chat admitted without its fresh, complete member read.
- **A request outside the sealed policy.** The native client sends reads, plain requests `teams_fence::contacting`
  calls `None` (logged before they go), `Authorized<_>` writes, and, for a call, typed delivery responses through
  `FrameSink::respond`; the personal build only its sealed lists. A raw request, a hidden retry, a redirect or
  proxy, a host outside the allowlist is a defect.
- **A leak.** A token, cookie or payload reaching a log, stdout, an error, `Debug` or the repo. Captures and
  tokens never enter the repo, not even sanitized; committed fixtures are synthetic, marked `_synthetic`.
- **A message sent twice or lost.** The outbox resends without its arrival check; a row of another run is sent
  without an explicit `Store::resend`; an older version overwrites a newer one; a field in a form no capture showed
  is read as absent, 0 or newer instead of kept as `Unknown`.
- **A token trusted past its expiry**, or sent where Teams itself doesn't send it: on another user action, to a service
  it wasn't issued for, or to any non-Microsoft service (a third-party bot, an app backend). Microsoft's recording bot
  getting the recorder-scoped token when the user starts a recording, transcription or live captions is what Teams
  does, not a finding.
- **A lab browser target resumed unarmed.** Arming order and close-or-kill containment are in `AGENTS.md`. A breach
  is never a flake.
- **The feature doesn't work.** A call that doesn't connect, audio that doesn't flow, a view that shows the wrong
  state, a crash, a hang. This is the task's actual job and weighs as much as the fence.
- **A test that only covers the mock's form of a field.** Live bugs have come from this. A fixture must hold every
  form the captures show.

## What is not a major finding

- A failure that needs someone outside the lab to call, message or invite a lab account.
- A gap in a defence-in-depth layer (a scenario's own click guard, a harness check, a log, a second check beside the
  fence) while the fence still decides and blocks the write.
- A guard that could be stricter, a check that could be added, or a rule's wording the code could follow more
  literally, with no reachable write escaping and nothing broken.
- New fence tightening, new denials or new guards the task didn't ask for. Don't propose them as fixes.

These are minor at most, and only worth reporting when the fix is small.

## Blast radius

- Fence, policy, auth and secrets code: the user's employer tenant and colleagues. A defect here is material when it
  has a reachable failing sequence; doubt alone isn't a finding.
- Calling, media, store, outbox and sync: whether calls and chats work and what the client shows.
- `proto-lab` scenarios, spikes, `scripts/` and `tools/`: lab harness only. A defect matters when it lets a write past
  the fence, misjudges a run (a breach recorded as a pass, a failed call recorded as passed) or leaks; ergonomics and
  extra guards are minor.

## The reporting bar

- Material, not merely true. Report a defect with a concrete, realistic failing sequence, or a test that can't catch
  what it claims to.
- Noise here: style preferences rustfmt and clippy already settle; asking for more comments; suggesting a
  dependency or an abstraction for its own sake; "consider handling X" with no reachable X; hardening for a case
  that needs an outsider to contact the lab.
- Severity: critical for a fence escape, a request outside the sealed policy, or a leak; major for loss,
  duplication, a trusted stale token, or a defect that breaks what the task delivers; minor for the rest, a deviation
  from a written rule with no reachable consequence included.
- Finding nothing is a valid answer.

## Where the rules live

- `AGENTS.md`: the hard rules (fence, allowances, native request policy, calls, captures and fixtures, live runs),
  the patterns, the lint rules and the gate.
- `docs/protocol/*.md`: protocol notes in our own words. `endpoints.md` must gain or update a row in the same change
  that captures, maps, allowlists, uses or re-treats an endpoint.
- `docs/backlog/`: accepted, known gaps (one file each). Don't re-report one as new; do report a change that widens
  it.
- `docs/plans/`: historical records of decisions. They aren't a description of the current system and aren't
  updated to match code.
- `clippy.toml`, `scripts/lint-report.sh`, the workspace `[lints]`.

## Conventions that are deliberate

- Fail closed in the fence: unknown, partial, paginated or unreadable means denied, never guessed. A value no
  capture showed is kept as `Field::Unknown(raw)`.
- No hidden retries: `reqwest` retries are off, the transport retries a read once, the outbox owns the write retry
  table, and a call write is never resent after an unknown outcome. An extra retry is a defect, not robustness.
- Trouter deliveries are answered only by a call's typed response (`FrameSink::respond`); own availability goes only
  as its sealed writes; an incoming call is answered only by its `CallAction` (in the lab also
  `teams-headless watch --answer-lab-calls`, for an invitation the fence offered).
- `#[allow]` and inner `#![allow]`/`#![expect]` are refused. The only exception is an outer
  `#[expect(clippy::…, reason = "…")]` on one item. Over-limit functions are split. `clippy.toml` thresholds only go
  down.
- A released SQLite migration is never edited; a change adds a new one.
- `teams-trouter` caps `log` at `debug` at compile time; a new dependency logging at `debug` or below gets the same
  check.
- `test-support` features exist only under `[dev-dependencies]`. A lab binary refuses to run with them, and live
  runs use `cargo build -p`.
- Comments follow AGENTS.md's Code comments rule, which wins over this profile; long doc comments already in the tree
  aren't a finding.
- Our own CDP client, not `chromiumoxide`, since that one resumes held targets by itself.

## Languages a change may touch

- Rust for almost everything.
- JavaScript only in `crates/proto-lab/assets/` (`ws_guard.js`, `rtc_hooks.js`, injected into lab pages),
  `.claude/skills/task-dispatch/dispatch-status/assets/index.html` (the status page's own script) and `tools/`
  (interim Node tools). Never Microsoft JS.
- SQL migrations in `crates/teams-store/src/migrations/`, shell in `scripts/`, Markdown in `docs/`.
