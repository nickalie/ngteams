# Bot Framework SDK for Python sample bot

A bot for an own ngteams server (`ngteams-server`) on the Bot Framework SDK for Python, the one its container tests
(`binary::bots::containers`) run. It answers, in its 1:1 chat: any text with `echo from botbuilder-python: <text>`;
`markdown`, `card` (an Adaptive Card with an input, an `Action.Submit` and a link), `execute` (a card whose
`Action.Execute` `approve` the bot replaces with `approved by <name> from botbuilder-python`), `edit`, `delete`,
`typing`, `member` and `proactive`. A card's submit comes back as `submitted from botbuilder-python: <data>`. Its
message extension has a search command `search` (two results with the query in their titles) and an action command
`order` (a dialog with an `item` input; its submit answers a result card `ordered <item> from botbuilder-python`). In a
group chat or a channel, where a bot gets only the messages that @mention it, it answers `mentioned in
<groupChat|channel> from botbuilder-python: <text>`. Every name in it is synthetic.

Pinned: `botbuilder-core` and `botbuilder-integration-aiohttp` 4.16.2 and every package under them (`requirements.txt`),
Python 3.12. Not 4.17.1: its aiohttp adapter serializes an invoke's answer a second time and answers every Teams invoke
with a 500.

## Its setup for an own server

By configuration alone (`config.py`, read by `ConfigurationBotFrameworkAuthentication`): `CHANNEL_SERVICE` any name but
Microsoft's clouds' (the SDK then fills in no endpoint of its own), `VALIDATE_AUTHORITY = False`,
`TO_CHANNEL_FROM_BOT_LOGIN_URL = "<origin>/adfs"` (msal-python asks Microsoft about any authority but an `adfs` one),
`TO_CHANNEL_FROM_BOT_OAUTH_SCOPE`, `TO_BOT_FROM_CHANNEL_TOKEN_ISSUER = "<origin>/bot"`,
`TO_BOT_FROM_CHANNEL_OPENID_METADATA_URL` and `TO_BOT_FROM_EMULATOR_OPENID_METADATA_URL` =
`"<origin>/bot/v1/.well-known/openidconfiguration"`, `OAUTH_URL = "<origin>"`, with `APP_ID`, `APP_PASSWORD` and
`APP_TENANTID`. Python reads the CA from `SSL_CERT_FILE` and `REQUESTS_CA_BUNDLE`, which the image's entrypoint points
at the system's CAs and `BOT_CA_PEM`.

## Run it against an own server

1. Make the app package: `../package.sh botbuilder-python` writes `botbuilder-python.zip` with new GUIDs as the app's
   and the bot's id (set `APP_ID` and `BOT_ID` to keep ids you have). Upload it in ngteams (**Settings → Manage apps**)
   or with `ngteams-server app add --package botbuilder-python.zip`; copy the bot's secret from the answer.
2. Give the app its bot's messaging endpoint, `https://<the bot's host>/api/messages`, in Manage apps (or `PATCH
   /api/admin/v1/apps/<app id>` with `{"endpoint": …}`). A bot on a private network needs its range in
   `bots.allowed_networks`, and a plain `http` endpoint `bots.allow_http_endpoints`.
3. Build the image from `packaging/server/bots`: `NGTEAMS_SERVER_BOT_IMAGES=botbuilder-python ./build.sh`, then run it:

   ```sh
   podman run --rm -p 3978:3978 -e ORIGIN=https://teams.example.com -e TENANT=<tenant id> \
     -e BOT_ID=<bot id> -e BOT_SECRET=<secret> -e BOT_CA_PEM="$(cat ca.pem)" localhost/ngteams-bot-botbuilder-python:1
   ```

4. Press **Send a test activity** in Manage apps: the bot answers `201` when it took the server's token. Then add
   the app from the Store and chat with it.

| Variable | What |
|---|---|
| `ORIGIN` | the server's public origin (`https`: every MSAL refuses another authority) |
| `TENANT` | the server's tenant id: the `<id>` in the `issuer` (`<origin>/<id>/v2.0`) of `<origin>/botframework.com/v2.0/.well-known/openid-configuration` |
| `BOT_ID`, `BOT_SECRET` | the bot's id and secret |
| `SCOPE` | the server's `bots.scope`, default `https://api.botframework.com` |
| `PORT` | where the bot listens, default `3978` |
| `BOT_CA_PEM` | the CA of the server's certificate, when the system doesn't trust it |
