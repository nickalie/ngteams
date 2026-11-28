# Bot Framework SDK for .NET sample bot

A bot for an own ngteams server (`ngteams-server`) on the Bot Framework SDK for .NET, the one its container tests
(`binary::bots::containers`) run. It answers, in its 1:1 chat: any text with `echo from botbuilder-dotnet: <text>`;
`markdown`, `card` (an Adaptive Card with an input, an `Action.Submit` and a link), `execute` (a card whose
`Action.Execute` `approve` the bot replaces with `approved by <name> from botbuilder-dotnet`), `edit`, `delete`,
`typing`, `member` and `proactive`. A card's submit comes back as `submitted from botbuilder-dotnet: <data>`. Its
message extension has a search command `search` (two results with the query in their titles) and an action command
`order` (a dialog with an `item` input; its submit answers a result card `ordered <item> from botbuilder-dotnet`). In a
group chat or a channel, where a bot gets only the messages that @mention it, it answers `mentioned in
<groupChat|channel> from botbuilder-dotnet: <text>`. Every name in it is synthetic.

Pinned: `Microsoft.Bot.Builder.Integration.AspNet.Core` 4.23.1 (`packages.lock.json`, restored in locked mode), .NET 8.

## Its setup for an own server

By configuration alone: the SDK's own keys, read by `ConfigurationBotFrameworkAuthentication` (`appsettings.json` shows
them; `run.sh` sets them in the environment from the variables below): `MicrosoftAppType`, `MicrosoftAppId`,
`MicrosoftAppPassword`, `MicrosoftAppTenantId`, `ChannelService` any name but Microsoft's clouds', `ValidateAuthority =
false`, `ToChannelFromBotLoginUrl = "<origin>/<tenant>"`, `ToChannelFromBotOAuthScope`, `ToBotFromChannelTokenIssuer =
"<origin>/bot"`, `ToBotFromChannelOpenIdMetadataUrl` and `ToBotFromEmulatorOpenIdMetadataUrl` =
`"<origin>/bot/v1/.well-known/openidconfiguration"`, `OAuthUrl = "<origin>"`. .NET reads the CA from `SSL_CERT_FILE`,
which the image's entrypoint points at the system's CAs and `BOT_CA_PEM`.

## Run it against an own server

1. Make the app package: `../package.sh botbuilder-dotnet` writes `botbuilder-dotnet.zip` with new GUIDs as the app's
   and the bot's id (set `APP_ID` and `BOT_ID` to keep ids you have). Upload it in ngteams (**Settings → Manage apps**)
   or with `ngteams-server app add --package botbuilder-dotnet.zip`; copy the bot's secret from the answer.
2. Give the app its bot's messaging endpoint, `https://<the bot's host>/api/messages`, in Manage apps (or `PATCH
   /api/admin/v1/apps/<app id>` with `{"endpoint": …}`). A bot on a private network needs its range in
   `bots.allowed_networks`, and a plain `http` endpoint `bots.allow_http_endpoints`.
3. Build the image from `packaging/server/bots`: `NGTEAMS_SERVER_BOT_IMAGES=botbuilder-dotnet ./build.sh`, then run it:

   ```sh
   podman run --rm -p 3978:3978 -e ORIGIN=https://teams.example.com -e TENANT=<tenant id> \
     -e BOT_ID=<bot id> -e BOT_SECRET=<secret> -e BOT_CA_PEM="$(cat ca.pem)" localhost/ngteams-bot-botbuilder-dotnet:1
   ```

4. Press **Send a test activity** in Manage apps: the bot answers `200` when it took the server's token. Then add the
   app from the Store and chat with it.

| Variable | What |
|---|---|
| `ORIGIN` | the server's public origin (`https`: every MSAL refuses another authority) |
| `TENANT` | the server's tenant id (Manage apps shows it) |
| `BOT_ID`, `BOT_SECRET` | the bot's id and secret |
| `SCOPE` | the server's `bots.scope`, default `https://api.botframework.com` |
| `PORT` | where the bot listens, default `3978` |
| `BOT_CA_PEM` | the CA of the server's certificate, when the system doesn't trust it |
