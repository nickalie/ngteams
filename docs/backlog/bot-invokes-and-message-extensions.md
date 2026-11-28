---
worth: yes
where: crates/teams-proto/src/bot.rs, crates/teams-proto/src/chat/card.rs, crates/teams-core/src/bots.rs, docs/protocol/bots.md
added: 2026-10-21
---
# Bot invokes and message extensions: the shapes still unseen

The client side of bots is built (cards' inputs and buttons, bot chats and mentions, the Store, message extensions
and dialogs, sign-in cards). A bot invoke is `POST v1/agents/28:<bot>/invoke` on the account's own chat service
([bots.md](../protocol/bots.md)); the native client has the typed invoke (`teams_proto::BotInvoke`), its answers,
what a card's buttons and inputs carry, and one attempt through the outbox (`EngineHandle::bot_invoke`). With the
flag, a card's buttons, its `refresh` and a 1:1 chat's suggested actions send it (`teams_ui::bot_card`).

The token question that kept invokes out is settled by what the captures show: `userAadToken` is the account's
token for the Teams service (`https://api.spaces.skype.com`), posted to Microsoft's chat service, the service it was
issued for, not a token for the app. An own-server account sends none.

What stays open, because no capture shows it (each is in `bots.md` as unseen or own design):

- **A card's `Action.Execute`** (`adaptiveCard/action`): where Teams names the card's message in the request (the
  native body says `messageId`), and whether the chat service wraps the bot's `{statusCode, type, value}` in its
  `responseType` envelope. The Approvals request tried on 2026-11-16 (lab a to b) gave a card with no action and no
  `refresh`, which stayed "Requesting", so the lab tenant has no card with one.
- **A plain `Action.Submit` and `messageBack`**: which request Teams sends for them (the native body is an invoke
  named `message`).
- **A search command** (`composeExtension/query`, `selectItem`), `task/fetch` and `task/submit` with a card,
  suggested actions and a bot mention: no bot in the lab tenant offers them. A test bot of our own in the lab tenant
  (custom app upload and one bot registration, the user's decision) would show them all.

When a capture differs from `bots.md`, the client and the own server change together, as `bots.md` is their one
source.
