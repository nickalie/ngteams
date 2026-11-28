#!/bin/sh
# The SDK's own settings from the server's origin, its tenant and the bot's id and secret; the environment takes
# precedence over appsettings.json.
set -eu
origin=${ORIGIN%/}
export MicrosoftAppId="$BOT_ID" MicrosoftAppPassword="$BOT_SECRET" MicrosoftAppTenantId="$TENANT"
export ToChannelFromBotLoginUrl="$origin/adfs" ToChannelFromBotOAuthScope="${SCOPE:-https://api.botframework.com}"
export ToBotFromChannelTokenIssuer="$origin/bot" OAuthUrl="$origin"
export ToBotFromChannelOpenIdMetadataUrl="$origin/bot/v1/.well-known/openidconfiguration"
export ToBotFromEmulatorOpenIdMetadataUrl="$ToBotFromChannelOpenIdMetadataUrl"
export ASPNETCORE_URLS="http://0.0.0.0:${PORT:-3978}"
exec dotnet SampleBot.dll
