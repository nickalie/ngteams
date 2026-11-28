"""The Bot Framework SDK for Python sample bot of an own ngteams server, on aiohttp: its signed setup is
configuration alone (`config.py`)."""

from aiohttp import web
from botbuilder.integration.aiohttp import CloudAdapter, ConfigurationBotFrameworkAuthentication
from botbuilder.core.integration import aiohttp_error_middleware

from bot import SampleBot
from config import DefaultConfig

CONFIG = DefaultConfig()
ADAPTER = CloudAdapter(ConfigurationBotFrameworkAuthentication(CONFIG))
BOT = SampleBot(ADAPTER, CONFIG.APP_ID)


async def messages(request: web.Request) -> web.Response:
    return await ADAPTER.process(request, BOT)


APP = web.Application(middlewares=[aiohttp_error_middleware])
APP.router.add_post("/api/messages", messages)

if __name__ == "__main__":
    web.run_app(APP, host="0.0.0.0", port=CONFIG.PORT)
