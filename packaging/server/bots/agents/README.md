# Microsoft 365 Agents SDK (JavaScript) sample bot

A bot for an own ngteams server (`ngteams-server`) on the Microsoft 365 Agents SDK (JavaScript), the one its container
tests (`binary::bots::containers`) run. It answers, in its 1:1 chat: any text with `echo from agents-sdk: <text>`;
`markdown`, `card` (an Adaptive Card with an input, an `Action.Submit` and a link), `execute` (a card whose
`Action.Execute` `approve` the bot replaces with `approved by <name> from agents-sdk`), `edit`, `delete`, `typing`,
`member` and `proactive`. A card's submit comes back as `submitted from agents-sdk: <data>`. Its message extension has a
search command `search` (two results with the query in their titles) and an action command `order` (a dialog with an
`item` input; its submit answers a result card `ordered <item> from agents-sdk`). In a group chat or a channel, where a
bot gets only the messages that @mention it, it answers `mentioned in <groupChat|channel> from agents-sdk: <text>`.
Every name in it is synthetic.

Pinned: `@microsoft/agents-hosting` and `@microsoft/agents-hosting-express` 1.10.1, `@azure/msal-node` 5.6.0
(`package-lock.json`), Node 22.

## Its setup for an own server

The check of the server's activity tokens is configuration: `authorityEndpoint: "<origin>"`, `tenantId` and `issuers:
["<origin>/bot"]`. Getting the bot's own token is a few lines of code: the SDK's msal-node asks Microsoft about any
authority it doesn't know, so `bot.js` sets a token provider (msal-node with authority `<origin>/<tenant>` and
`knownAuthorities: ["<origin host>"]`) as the adapter's `connectionManager.getTokenProvider` and
`getTokenProviderFromActivity`. Invokes are answered from `onActivity('invoke')` with an `invokeResponse`.

## Run it against an own server

1. Make the app package: `../package.sh agents` writes `agents.zip` with new GUIDs as the app's and the bot's id (set
   `APP_ID` and `BOT_ID` to keep ids you have). Upload it in ngteams (**Settings → Manage apps**) or with
   `ngteams-server app add --package agents.zip`; copy the bot's secret from the answer.
2. Give the app its bot's messaging endpoint, `https://<the bot's host>/api/messages`, in Manage apps (or `PATCH
   /api/admin/v1/apps/<app id>` with `{"endpoint": …}`). A bot on a private network needs its range in
   `bots.allowed_networks`, and a plain `http` endpoint `bots.allow_http_endpoints`.
3. Build the image from `packaging/server/bots`: `NGTEAMS_SERVER_BOT_IMAGES=agents ./build.sh`, then run it:

   ```sh
   podman run --rm -p 3978:3978 -e ORIGIN=https://teams.example.com -e TENANT=<tenant id> \
     -e BOT_ID=<bot id> -e BOT_SECRET=<secret> -e BOT_CA_PEM="$(cat ca.pem)" localhost/ngteams-bot-agents:1
   ```

4. Press **Send a test activity** in Manage apps: the bot answers with a 2xx status when it took the server's
   token. Then add the app from the Store and chat with it.

| Variable | What |
|---|---|
| `ORIGIN` | the server's public origin (`https`: every MSAL refuses another authority) |
| `TENANT` | the server's tenant id: the `<id>` in the `issuer` (`<origin>/<id>/v2.0`) of `<origin>/botframework.com/v2.0/.well-known/openid-configuration` |
| `BOT_ID`, `BOT_SECRET` | the bot's id and secret |
| `SCOPE` | the server's `bots.scope`, default `https://api.botframework.com` |
| `PORT` | where the bot listens, default `3978` |
| `BOT_CA_PEM` | the CA of the server's certificate, when the system doesn't trust it |
