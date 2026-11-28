---
worth: later
status: open
where: crates/ngteams/src/ctl/reads.rs (`teams_read`), crates/ngteams/src/ctl/address.rs (`conversation`, `Channel::Bare`), crates/teams-ui/src/bridge.rs (`Core::teams`)
added: 2026-11-28
---
# ngteamsctl: two edges of the channel reads before the teams are read

- **`teams` after a failed read whose retry is due.** `teams_read` answers "the teams can't be read right now" at
  once when the last read failed and no teams are stored. When `TEAMS_RETRY` has passed, its own `core.teams()` call
  starts the retry, but the answer does not wait for it: the next `ngteamsctl teams` gets the teams. A fix gives the
  ctl through `Core` whether a teams read runs or is due, and waits for that read within `TEAMS_ANSWER`.
- **A bare `19:` id before the teams are read.** `messages` and `thread` accept each unknown `19:` id as a channel
  while the teams are not read (`Channel::Bare`), also after a failed read. A typo then looks like an empty channel
  that did not sync (`synced: false`, `end: store`). A fix answers `no_chat` for a bare id after a failed teams read,
  or marks the answer as not verified.
