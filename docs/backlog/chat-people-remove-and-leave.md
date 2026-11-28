---
worth: yes
where: crates/teams-ui/src/chat_people.rs, crates/teams-proto/src/write/chat.rs, crates/teams-core/src/outbox.rs
added: 2026-11-10
---
# A chat's people: Remove, Leave and Add agents

The chat header's people flyout (`crate::chat_people`) lists a chat's people and adds people, as browser Teams does
(`docs/protocol/chat-service.md`, "Adding people to a chat"). Three of Teams' entries in the same flyout are not built
yet:

- **Remove** (a group chat's member, the × on hover): "Remove <name> from the conversation? They'll still have access
  to the chat history." with Cancel and Remove, then `DELETE v1/threads/<chat>/members/<mri>` with no body (seen
  2026-11-10, held back). It needs a typed write and an outbox kind of its own, the flyout's hover × and its confirm,
  and the roster read again after it went through, as an add does.
- **Leave** (a group chat): "Leave this chat?" — "This chat will stay in your list but you can hide it anytime or
  remove chat history now." — with a "Remove chat history" box ("This will remove the chat from your list and search.
  It affects you only.") and Cancel and Leave. Its requests were not captured: presumably the same `DELETE` on the
  account's own MRI, and for the box something more (hiding the conversation, removing its history for the account).
  Capture both, held back, before building it; the store then has to keep a chat the account left (its reads answer
  403) as the conversation list already treats one (`docs/protocol/chat.md`).
- **Add agents** (both kinds of chat): adds a bot or a Copilot agent to the chat. Apps and bots in chats are built
  (`docs/protocol/bots.md`); look at it with that.
