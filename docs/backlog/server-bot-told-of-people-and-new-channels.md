---
worth: yes
where: crates/ngteams-server/src/bots/activity.rs, crates/ngteams-server/src/chatsvc/member_repo.rs, crates/ngteams-server/src/teams/repo.rs
added: 2026-11-27
---
# Own server: a bot is not told of people or new channels

A bot gets `conversationUpdate` only for its own add and removal (`apps/installs.rs`). Teams also sends it one when
people join or leave a chat or team that has the bot (`membersAdded` or `membersRemoved` of `29:` accounts, and for
a team `teamMemberAdded` or `teamMemberRemoved`), and when a channel is made in the bot's team
(`channelData.eventType` `channelCreated`, with `channelData.channel`). Bot SDK handlers such as `onMembersAdded` for
people and `onTeamsChannelCreated` do not fire on the own server for this reason.

To do: queue the activity in the outbox in the same transaction as the roster change (group chat roster in
`chatsvc/member_repo.rs`, team roster and `create_channel` in `teams/repo.rs`) for each bot in the chat or the
team's channels, with `bots::activity::members_changed` and a new `channelCreated` builder. Add the e2e steps to
`tests/e2e/bots/installs.rs`, then remove the "not told yet" sentence in `docs/server.md` "What the bot gets".
