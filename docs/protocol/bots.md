# Bots: invokes, card actions and their answers

How a Teams client asks an app's bot for something: a card's button, a message extension's commands, its dialog
and its submit. The client never reaches a bot or an app's backend itself. It posts one request to its own chat
service, and the service reaches the bot. The same request serves a Microsoft account and an own-server account;
only the token rule below differs.

What was seen: the Workflows app's message extension in the lab's runs (lab account a, `team-manage`, 2026-11-05):
`composeExtension/fetchCommands`, `fetchTask` and `submitAction`, with their answers; and the Approvals app's request
from the compose box's "+" (a to b in their 1:1 chat, 2026-11-16): `submitAction` from the compose box and the
just-in-time install below. Everything marked **unseen**
below follows the public Teams and Bot Framework documentation and waits for a capture: no bot in the lab tenant
shows those parts within the lab rule (`docs/lab-rules.md`). The own server follows this page, so a later capture
that differs changes the client and the server together.

The native client: [`teams_proto::bot`](../../crates/teams-proto/src/bot.rs) (the body and the answers),
[`teams_core::bots`](../../crates/teams-core/src/bots.rs) (one attempt through the outbox),
[`teams_proto::chat::card`](../../crates/teams-proto/src/chat/card.rs) (what a card's buttons and inputs carry).
Synthetic fixtures: `tests/fixtures/bots/`.

## The request

`POST <chat service>/v1/agents/28:<bot id>/invoke`, with the chat service's own headers and bearer (on Microsoft the
`https://ic3.teams.office.com` token, `X-MS-Migration: True`, `BehaviorOverride: redirectAs404`) and a JSON body:

| Field | What it is |
|---|---|
| `name` | the invoke's name (below) |
| `appId` | the app's id; the Workflows app's differs from its bot's id in the path |
| `value` | what the invoke asks, by name (below) |
| `imdisplayname` | the account's display name |
| `userAadToken` | a Microsoft account only: see the token rule |
| `conversation` | `{"id": …}`: the chat, or a channel post's `<channel>;messageid=<post>` chain. Seen left out of a `fetchCommands` sent while no conversation was open |
| `messageId` | the card message an action acts on: **own design, unseen** (the Bot Framework gives the bot this as `replyToId`; where Teams puts it in the request isn't known) |

## The token rule

In every captured invoke, `userAadToken` is the account's token for the Teams service itself: audience
`https://api.spaces.skype.com`, scopes `Authorization.ReadWrite Region.ReadWrite user_impersonation`, issued to the
Teams web client. It is the same token the client already sends to Teams' authz and middle tier, and it goes to
Microsoft's chat service, the service it was issued for. It is not a token for the bot or its app: nothing seen shows
the service passing it on, and the Bot Framework gives a bot no user token in an activity.

- **Microsoft account:** the native client sends `userAadToken` with the account's Teams service token, in every
  invoke, as Teams does. No other token goes into an invoke. The client never opens a connection to a bot, never
  follows a bot's `serviceUrl`, and never asks for a token for an app's audience. The transport adds the token when it
  sends; the typed request never holds it, and the debug log redacts it.
- **Own-server account:** the same path on the account's own origin (`<origin>/api/chatsvc/<region>/v1/agents/…`),
  with that account's own bearer, and **no `userAadToken`**: the bearer already names the caller. The own server
  ignores the field if a client sends it and never copies a user token into what it sends a bot.

Still out of scope under this rule: `signin/tokenExchange` (single sign-on, which hands a token for the app's own
audience to the app) and any on-behalf-of token for an app.

## The invokes

| `name` | When | `value` | Seen |
|---|---|---|---|
| `composeExtension/fetchCommands` | an editor opens (the compose box, a message's actions): the app's commands for that context | `{"commandContext": "Message"}` (capitalised; `Compose`, `CommandBox` by analogy, unseen) | seen |
| `composeExtension/fetchTask` | an action command is chosen: its dialog | `commandId`, `commandContext` (`message`, `compose`; `commandbox` unseen), `context` `{"theme": "default"}` (`dark`, `contrast`), and on a message `messagePayload` | seen |
| `composeExtension/submitAction` | the dialog's submit | the same, with `data`: the card's `Action.Submit` `data` merged with the answers by input id, each a string (a date `2026-01-03`, a time `08:00`, a switch's value); an app's own web-page dialog submits its own JSON there (Approvals: lists and numbers too) | seen |
| `composeExtension/query` | a search command's query | `commandId`, `commandContext` `compose`, `parameters` (`[{name, value}]`), `queryOptions` (`skip`, `count`), `context` | unseen |
| `composeExtension/selectItem` | a search result is chosen | the result preview's `tap` value | unseen |
| `adaptiveCard/action` | a card's `Action.Execute` is pressed, or its `refresh` runs | `{"action": {"type": "Action.Execute", "id", "verb", "data"}, "trigger": "manual" \| "automatic"}` | unseen |
| `task/fetch` | a card button whose `msteams.type` is `task/fetch` | `{"data": <the button's data>, "context"}` | unseen |
| `task/submit` | a card dialog's submit | `{"data": <its data and answers>, "context"}` | unseen |
| `signin/verifyState` | the code a bot's sign-in page showed, typed under its sign-in card once the page opened | `{"state": "<the code>"}` | unseen (the Bot Framework documents it for clients that can't finish a sign-in in a window of their own) |
| `message` | a card's plain `Action.Submit` or `messageBack`: the bot gets a message activity carrying `value` | the data merged with the answers; a `messageBack`'s `text` goes as the body's `text` | **own design**, unseen |

`messagePayload` describes the message an action command runs on: `id`, `replyToId` (a channel post's id; none in a
chat), `createdDateTime`, `deleted` false, `importance` `normal`, `locale` `en-us`, `from` (`{"user":
{"userIdentityType": "aadUser", "id": <object id>, "displayName"}}`, or `{"application": {"applicationIdentityType":
"bot", "id", "displayName"}}` for a bot's message), `body` (`contentType` `html`, `content`), empty `attachments`,
`mentions` and `reactions` lists, and `linkToMessage` (the message's Teams link, as `workflows.md` gives it).

What a card's buttons carry (`teams_proto::chat::card::BotAction`): an `Action.Submit`'s `data` as given, its
`msteams.type` (`messageBack` with `text`, `displayText` and `value`; `imBack` with its text; `invoke`; `signin`;
`task/fetch`), its `id` and `associatedInputs` (`none` sends no answers); an `Action.Execute`'s `verb`; a hero card
button's type and `value`; and a card's `refresh` (its `Action.Execute` and the `userIds` it runs for by itself).
`imBack` and `messageBack`'s `displayText` post a visible message as the user, through the ordinary message write;
`messageBack`'s `text` goes to the bot only, with its value.

The answers go as Adaptive Cards gives them: a card's `Action.Submit` or `Action.Execute` sends the answers of its own
card and of the cards a `ShowCard` opened it from, each input's by its `id`, merged into the action's `data` when that
is an object (none becomes the answers; any other `data` goes as it is, without them). An input left empty goes as no
key. Nothing goes while an answer fails its card's checks (`isRequired`, `regex`, `maxLength`, `min` and `max`); the
input shows its `errorMessage`. A typed time that can't be read as one fails too, required or not.

**Suggested actions** (own design, unseen): a bot's message offers them in its activity's
`suggestedActions.actions` (the Bot Framework's shape: `imBack`, `messageBack`, `postBack` buttons), carried in the
`<Swift>` activity of its `RichText/Media_Card` message beside its `attachments`. In a 1:1 chat the newest message's
are buttons over the compose box; `imBack` posts its `value` as the user's message, the others ask the bot as a
card's button does, but in a place of their own: no card waits for them, and the bot's line shows under them. They go
once anyone writes again.

**Just-in-time install** (seen, Approvals, 2026-11-16): an app not yet in the chat answers a `submitAction` with a
dialog card (`task` `continue`) that asks to add it, its `Action.Submit` `data` the submitted data plus
`"msteams": {"justInTimeInstall": true}`. On that button Teams adds the app to the chat (`POST
<middle tier>/beta/chats/<chat>/apps/entitlements`) and sends the `submitAction` again with that data, answered 200
with an empty body. The app then posts its card as the user ("<name> via <app>"), with `properties.onbehalfof`.

## Sign-in cards

A bot that needs the person's token for another service sends an OAuth card
(`application/vnd.microsoft.card.oauth`: `text`, `connectionName`, `buttons` with one `signin` button whose `value`
is the sign-in link) or a sign-in card (`application/vnd.microsoft.card.signin`: `text`, `buttons`). Unseen: no bot in
the lab tenant sends one within the lab rule. The card's text and buttons are drawn as a hero card's are.

The native client opens the link in the browser, then shows a code field and Verify under the
card. The page at the end of the sign-in shows a code (the Bot Framework's token service shows it when the sign-in
runs outside Teams' own window; the own server always does). Verify sends it as `signin/verifyState`, one attempt;
the bot redeems it at its token service. A `400`, `401`, `403`, `404` or `412` (or the bot's own error) says the code
didn't work; any failure keeps the field. A success takes the field away and says the person is signed in.

No token of the account is involved beyond the invoke's own (the token rule above): the sign-in happens in the
browser, between the person and the bot's identity provider. Single sign-on (`signin/tokenExchange`) is not sent.

## Message extensions

Seen in the lab's catalogue reads (lab account a, 2026-11-14) and in browser Teams' compose box and message menu:

- **Where the commands come from**: an app's definition in the account's apps (`POST beta/users/apps/aggregatedEntitlements`,
  `docs/protocol/apps.md`) lists them under `inputExtensions` (a manifest's `composeExtensions`): per extension its
  bot (`botId`), `fetchCommands` (the app gives its commands at run time, `composeExtension/fetchCommands`) and
  `commands`. A command has an `id`, a `type` (`Action`; none is a search), `context` (`Compose`, `Commandbox`,
  `Message`; none or an empty list is the compose box and the command box, as the manifest schema's default says), a
  `title` and a `description`, a search's `parameters` (`name`, `title`, `description`, `isRequired`, `value`) and
  `initialRun`, and for an action either `fetchTask` (its dialog is the app's answer to `fetchTask`) or a static
  `taskInfo` (`title`, `width`, `height`, `url`: in every app seen, a web page).
- **The compose box's "+"** (Actions and apps): a flyout with "Search for apps", Teams' own actions (Schedule message,
  Set delivery options, Schedule meeting, Record video clip), a hairline, the account's apps whose commands run in the
  compose box (`Installed`, `InstalledAndPermanent`: Clipchamp, Workflows, Approvals, Power BI, Praise, Updates in the
  lab), and "Get more apps". An app chosen opens its dialog, titled with the app's picture, name and the command's
  title ("Praise · Send praise to people"); those apps' dialogs were all web pages.
- **A message's "More actions"**: Teams' own (Share to Outlook, Translate, Immersive Reader), then the commands that run
  on a message, each with its app's picture: the account's (Create Planner task) upright, the store's offers
  (`Featured`: Azure Boards, Jira, Polly) in italics; then "Create workflows".
- **A search command** (unseen): a search field under the app's name, the results of `composeExtension/query` as a
  list (each its `preview`: a thumbnail or hero card's `title`, `text` and picture), run at once with no words when
  `initialRun`. A result chosen goes into the compose box as its card; a preview whose `tap` is an `invoke` first asks
  the app for the card (`composeExtension/selectItem`).
- **A card chosen into the compose box** is sent as a link's app card is (`chat.md`, "App cards"): a span of itemtype
  `InputExtension` after the text and the card in `properties.cards` (`appId`, `appName`, `appIcon`, `cardClientId`,
  `contentType`, `content`). Unseen for a message extension's result; a message with only a card sends no text before
  the span (own design).

The native client: `teams_proto::ExtensionApp` and `ExtensionCommand` read the definitions;
`teams_ui::apps_flyout` is the "+" (it lists no Teams action: this app has none of them for a chat),
`teams_ui::app_dialog` the dialog, and a chat's "More actions" lists the commands after the workflows (an app not
added opens its details, where it is added). A dialog that is a web page needs Teams' JavaScript host, which the native
client doesn't have, so the dialog says the app opens in Teams on the web and offers that link. Synthetic apps:
`tests/fixtures/bots/extension-apps.json`.

## The answers

The chat service answers with the bot's status and body. Seen: HTTP 200 with
`{"responseType": <kind>, <kind>: …}`, `application/json`; and HTTP 200 with an empty `text/plain` body.

| Answer | Shape | Seen |
|---|---|---|
| done | 200, empty body (a `submitAction`; a `fetchCommands` of an app with nothing for the context) | seen |
| commands | `responseType` `composeExtension`, `{"type": "command", "commands": [{id, type: action \| query, title, fetchTask, taskInfo: {width}, imageUrls}]}` | seen |
| dialog | `responseType` `task`, `{"type": "continue", "value": {width, height, title, card: {contentType, content: <Adaptive Card>}}}` | seen (a card, `width` 600) |
| dialog with a web page | the same with `value.url` in place of `card` | unseen; the native client says the app opens in Teams on the web |
| results | `composeExtension` `{"type": "result", "attachmentLayout": "list" \| "grid", "attachments": [<card with its preview>]}` | unseen |
| message | `composeExtension` `{"type": "message", "text"}`, `task` `{"type": "message", "value"}` | unseen |
| sign-in | `composeExtension` `{"type": "auth" \| "config", "suggestedActions"}` | unseen |
| `Action.Execute`'s | the bot's own `{"statusCode", "type", "value"}`: `application/vnd.microsoft.card.adaptive` (a card that takes the pressed card's place), `…activity.message` (a line of text), `application/vnd.microsoft.error` (`{code, message}`), `…activity.loginRequest` (sign in) | unseen, and whether the service wraps it in `responseType` isn't known: the native client reads both |
| an error status | 4xx or 5xx | unseen; the native client shows it and sends nothing again |

**`Action.Execute` is still unseen.** The Approvals request card of 2026-11-16 (from a to b, in their chat) carried no
action and no `refresh`, and stayed "Requesting" for the approver after a reload some minutes later: the lab
tenant's Approvals never offered Approve or Reject on it. A test bot of our own in the lab tenant is the way to see
it (`docs/backlog/bot-invokes-and-message-extensions.md`).

An answer of another shape is kept as it came (`InvokeAnswer::Unknown`), never guessed at.

## The native client

An invoke is one attempt through the outbox (`Outbox::try_bot_invoke`), with no row: it may make the app act, so an
unknown outcome is told to the asker and never sent again, and a later run sends nothing. Read-only mode sends none.
The engine names the asker (`EngineHandle::bot_invoke`); no view builds the request. A chat's
cards ask their bot through `teams_ui::bot_card`:

- a card's inputs are drawn and checked, and its buttons ask the bot (`message` for a plain submit, `messageBack`,
  `invoke` and a hero card's `postBack`; `adaptiveCard/action` for `Action.Execute`), its message in `messageId`;
- the answer shows on the card: a card in the pressed card's place (its answers start again), a message, the bot's
  error or "the app didn't respond" under it, a sign-in request or a dialog as a line that opens the message in Teams
  on the web (sign-in cards and the apps' dialogs come later);
- a card's `refresh` runs by itself (`trigger` `automatic`) once for each version of the message's cards while the
  chat is open when its `userIds` name the account (its MRI or its object id), and by hand from the message's "More options" → "Refresh card" (`manual`);
- a `task/fetch` button asks the bot for its dialog (`task/fetch`) and opens it (`teams_ui::app_dialog`): a card
  across the dialog, its Submit a `task/submit`; a message extension's action command opens the same dialog, its Submit
  a `composeExtension/submitAction`. The dialog's answer closes it (done), takes its place (a next dialog), goes into the
  compose box (a result's card; in a channel the reply box of the post's thread, which opens), or shows (a message, an error); `signin` opens its page in the browser;
- the bot's app id is the bot's own id until the engine knows each bot's app.

Without the flag, or in read-only mode, a card's bot buttons open the message in Teams on the web, as before.

## Outgoing webhooks

A team's outgoing webhook is an app that the team's people @mention in its channels. Teams sends the mention to the
webhook's service and posts the answer as the webhook's reply in the post's thread. Nothing here was seen in the
lab: a webhook made in the lab tenant would send lab channel content to an outside address, which the lab rule does
not allow (`docs/lab-rules.md`). The web client's own requests are still to be captured, with every create, change
and delete held; on 2026-11-27 the capture could not run, because the lab accounts were deleted from the tenant.
Thus each part below is marked **documented** (Microsoft's public documentation and samples), **third party** (other
services' documents) or **own design** (the own server's choice until a capture shows Teams' form).

### What Teams does (documented)

- **Make it.** A team owner opens Manage team > Apps > "Create an outgoing webhook" and gives a name (also what
  people @mention), a callback URL (HTTPS), a description and an optional picture. Teams then shows a security token
  once: an HMAC key in base64. The token does not expire.
- **Where it works.** In the team's public (standard) channels, only when a message @mentions it. Not in chats, not
  in private channels, not in another team. It sends nothing by itself, and it can't call any other API.
- **The request.** `POST <callback URL>` with a Bot Framework `message` activity as the JSON body and
  `Authorization: HMAC <base64 of HMAC-SHA256(key, the exact body bytes)>`. The key is the token's base64 decoded.
- **The answer.** Within 5 seconds, an activity: `{"type": "message", "text": …}`, or `attachments` with Adaptive or
  hero cards. A webhook's cards act only by opening a link (`openUrl`).
- **A failure (third party).** When the service fails or does not answer in 5 seconds, the webhook's reply in the
  thread says "Sorry, there was a problem encountered with your request".

### The activity (documented, from Microsoft's samples)

```json
{"type": "message", "id": "<message id>", "timestamp": "…", "localTimestamp": "…",
 "serviceUrl": "…", "channelId": "msteams",
 "from": {"id": "29:<user>", "name": "<display name>", "aadObjectId": "<object id>"},
 "conversation": {"isGroup": true, "id": "<channel>;messageid=<post>", "name": null,
                  "conversationType": "channel", "tenantId": "<tenant>"},
 "recipient": null, "textFormat": "plain", "locale": "en-US",
 "text": "<at>Webhook name</at> the words",
 "attachments": [{"contentType": "text/html", "content": "<the message's HTML>"}],
 "entities": [{"type": "clientInfo", "locale": "en-US"}],
 "channelData": {"teamsChannelId": "<channel>", "teamsTeamId": "<team>", "channel": {"id": "<channel>"},
                 "team": {"id": "<team>"}, "tenant": {"id": "<tenant>"}}}
```

`conversation.id` names the post's thread, for a post and for a reply in its thread alike. The `serviceUrl` is
Teams' own: a webhook has no credential to call it.

### The middle tier's routes (own design)

Under the account's middle tier (`<middle tier>/beta/teams/<team>/outgoingWebhooks`), with the account's token for
`https://api.spaces.skype.com`. The team is its thread id. When a capture shows Teams' own form, the client and the
own server change together.

| Method | Path | Body | Answer |
|---|---|---|---|
| `GET` | `…/outgoingWebhooks` | none | 200 `{"value": [<webhook>]}`, ordered by name; for each of the team's people |
| `POST` | `…/outgoingWebhooks` | `{"name", "callbackUrl", "description"?, "picture"?}` | 201 `{"value": <webhook> + "securityToken"}`: the only answer that shows the token |
| `PUT` | `…/outgoingWebhooks/<id>` | any of the same fields; `"picture": null` removes it | 200 `{"value": <webhook>}` |
| `DELETE` | `…/outgoingWebhooks/<id>` | none | 204 |
| `GET` | `…/outgoingWebhooks/<id>/picture` | none | the picture (PNG or JPEG) |

A webhook: `{"id", "mri": "28:<id>", "teamId", "name", "description", "canManage", "createdBy": "8:orgid:<id>",
"createdTime", "pictureUrl"?, "callbackUrl"?}`. `callbackUrl` shows only to those who may change it (`canManage`).
`picture` is a PNG or JPEG in base64. A refusal is `{"errorCode", "message"}`: 400 `BadRequest` (a form the server
doesn't take, an address it doesn't send to), 403 `Forbidden` or `OperationNotAllowedOnArchivedTeam`, 404 `NotFound`
(no such team or webhook for the caller), 409 `OutgoingWebhookNameAlreadyExists` (a name, in any letter case, that
the team already has), or 409 `OutgoingWebhookChanged` (a concurrent update changed or removed the webhook).
A delete also returns `OutgoingWebhookChanged` if a concurrent update changed the picture.

A mention of a webhook is an ordinary mention in the message's `properties.mentions`, with `mri` `28:<id>`. Its
replies come from `28:<id>`, with the webhook's name as `imdisplayname`, in the thread of the post.

### The client (own design)

ngteams shows webhooks only for an own-server account and only while its authz names
`outgoingWebhookService`. A Microsoft account sends none of these requests until a capture shows Teams' own routes
(`teams_core::Capabilities::of`). Manage team > Apps lists the team's webhooks after its apps, each with its picture,
its name and "Outgoing webhook". An owner gets "Create an outgoing webhook": Name (at most 100 characters, the own
server's limit), Callback URL (HTTPS only), Description and a picture (PNG or JPEG, at most 1 MB). The create answer's token shows once,
with Copy; the dialog stays open until the create is answered. For an owner, a webhook with `canManage` has Edit (only the changed fields are sent; `"picture": null` removes the
picture) and Remove, which asks first. A list read makes each webhook a known bot for the run, so the mention picker
of the team's standard channels offers it (not of its private or shared channels, where a webhook gets nothing). A mention of it is a bot's mention (`mentionType` `bot`) with the webhook's id as
`appId`. Its replies show its name, its picture and an "App" mark. A card in a reply shows as any bot's card.
