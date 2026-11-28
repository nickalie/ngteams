"""The bot's settings: the Bot Framework SDK's own keys, pointed at an own ngteams server. Only the server's origin,
its tenant and the bot's id and secret come from the environment."""

import os

ORIGIN = os.environ["ORIGIN"].rstrip("/")


class DefaultConfig:
    PORT = int(os.environ.get("PORT", "3978"))
    APP_ID = os.environ["BOT_ID"]
    APP_PASSWORD = os.environ["BOT_SECRET"]
    APP_TENANTID = os.environ["TENANT"]
    # any name but Microsoft's clouds': the SDK then takes every endpoint below and fills in none of its own
    CHANNEL_SERVICE = "ngteams"
    VALIDATE_AUTHORITY = False
    # msal-python asks Microsoft about any authority but an `adfs` one
    TO_CHANNEL_FROM_BOT_LOGIN_URL = ORIGIN + "/adfs"
    TO_CHANNEL_FROM_BOT_OAUTH_SCOPE = os.environ.get("SCOPE", "https://api.botframework.com")
    TO_BOT_FROM_CHANNEL_TOKEN_ISSUER = ORIGIN + "/bot"
    TO_BOT_FROM_CHANNEL_OPENID_METADATA_URL = ORIGIN + "/bot/v1/.well-known/openidconfiguration"
    TO_BOT_FROM_EMULATOR_OPENID_METADATA_URL = TO_BOT_FROM_CHANNEL_OPENID_METADATA_URL
    OAUTH_URL = ORIGIN
    CALLER_ID = "urn:botframework:ngteams"
