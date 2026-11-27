---
worth: yes
where: crates/ngteams-server/src/bots/activity.rs, crates/ngteams-server/src/teams/repo.rs
added: 2026-11-27
---
# Own server: a bot is not told of new channels

A bot gets no `conversationUpdate` when a channel is made in its team. Teams sends it one, with
`channelData.eventType` `channelCreated` and `channelData.channel`. The Bot SDK handler `onTeamsChannelCreated` does
not fire on the own server for this reason.

To do: queue the activity in the outbox in the same transaction as `create_channel` in `teams/repo.rs` for each bot
in the team, with a new `channelCreated` builder in `bots::activity`. Add the e2e steps to
`tests/e2e/bots/member_updates.rs`, then remove the "not told yet" sentence in `docs/server.md` "What the bot gets".
