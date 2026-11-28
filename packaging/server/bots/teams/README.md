# Teams SDK v2 (TypeScript) sample bot

A bot for an own ngteams server (`ngteams-server`) on the Teams SDK v2 (TypeScript), the one its container tests
(`binary::bots::containers`) run. It answers, in its 1:1 chat: any text with `echo from teams-sdk: <text>`; `markdown`,
`card` (an Adaptive Card with an input, an `Action.Submit` and a link), `execute` (a card whose `Action.Execute`
`approve` the bot replaces with `approved by <name> from teams-sdk`), `edit`, `delete`, `typing`, `member` and
`proactive`, and `signin` / `signout` (the SDK's OAuth flow for the connection `synthetic-idp`, which an administrator
gives the bot with `PUT /api/admin/v1/apps/<app id>/connections/synthetic-idp`). A card's submit comes back as
`submitted from teams-sdk: <data>`. Its message extension has a search command `search` (two results with the query in
their titles) and an action command `order` (a dialog with an `item` input; its submit answers a result card `ordered
<item> from teams-sdk`). In a group chat or a channel, where a bot gets only the messages that @mention it, it answers
`mentioned in <groupChat|channel> from teams-sdk: <text>`. Every name in it is synthetic.

Pinned: `@microsoft/teams.apps` and `@microsoft/teams.api` 2.2.1, `@azure/msal-node` 7.2.2 (`package-lock.json`), Node
22.

## Its setup for an own server

Not by configuration alone. `bot.ts` passes the SDK a `cloud` (`withOverrides(PUBLIC, …)`) whose `openIdMetadataUrl` and
`tokenIssuer` are the server's, so the SDK checks the server's activity tokens, and whose `tokenServiceUrl` is the
server. The SDK's msal-node takes no known authorities, so the bot gets its own token through the `token` callback:
msal-node with authority `<origin>/<tenant>` and `knownAuthorities: ["<origin host>"]`.

## Run it against an own server

1. Make the app package: `../package.sh teams` writes `teams.zip` with new GUIDs as the app's and the bot's id (set
   `APP_ID` and `BOT_ID` to keep ids you have). Upload it in ngteams (**Settings → Manage apps**) or with
   `ngteams-server app add --package teams.zip`; copy the bot's secret from the answer.
2. Give the app its bot's messaging endpoint, `https://<the bot's host>/api/messages`, in Manage apps (or `PATCH
   /api/admin/v1/apps/<app id>` with `{"endpoint": …}`). A bot on a private network needs its range in
   `bots.allowed_networks`, and a plain `http` endpoint `bots.allow_http_endpoints`.
3. Build the image from `packaging/server/bots`: `NGTEAMS_SERVER_BOT_IMAGES=teams ./build.sh`, then run it:

   ```sh
   podman run --rm -p 3978:3978 -e ORIGIN=https://teams.example.com -e TENANT=<tenant id> \
     -e BOT_ID=<bot id> -e BOT_SECRET=<secret> -e BOT_CA_PEM="$(cat ca.pem)" localhost/ngteams-bot-teams:1
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
