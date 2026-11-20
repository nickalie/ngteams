# A group chat's rename moves the chat up the chat list

When a group chat gets a new name, browser Teams keeps the chat's row in the same place in the chat list (seen on
2026-11-20 in a lab group chat, `docs/protocol/chat-service.md#naming-a-group-chat`). ngteams moves the row to the top.
The message sync stores the `ThreadActivity/TopicUpdate` event, and the store's message write moves the conversation's
`last_activity` forward for each message, whatever its type (`crates/teams-store/src/messages.rs`). Other members get
the same event, so their rows move too. This existed before the rename action, but the rename action makes it easy to
see.

Do not change only the message write. The `last_activity` value also comes from the listing's `lastMessage`
(`conversation_row` in `crates/teams-core/src/sync.rs`) and from the conversation previews. First find out, in browser
Teams with lab accounts only, which event types move a row: TopicUpdate, AddMember, DeleteMember, a picture change,
call events. Also find out what the conversation list's `lastMessage` is after a rename. Then apply one rule on all
the paths that set `last_activity`, and record the rule in `docs/protocol/chat.md`.

Tests: a store test that a TopicUpdate does not move `last_activity`, and an engine test on the mock that renames an
older group chat and checks that the order of the chat list does not change, for the account that renamed it and for
another member.
