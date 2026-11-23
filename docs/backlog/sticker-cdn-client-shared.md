---
worth: later
status: open
where: crates/teams-core/src/signal.rs, crates/teams-core/src/telegram.rs
added: 2026-11-23
---
# The Telegram and the Signal sticker clients have two copies of one transport

`SignalCdn` and `BotApi` each build their own `reqwest` client (https only, no proxy, no redirect, no retry, the same
timeouts). Each has its own loopback check for a mock origin, its own error category and its own capped body reader.
Only the error type is different. A later fix to one copy does not go to the other copy.

To do: move the client builder and a capped reader with a small shared error into one private `teams-core` module.
Let each service map that error into `BotError` or `SignalError`. Keep the tests of the transport policy for both
services. The review of task 652 (round 1) found the copies.
