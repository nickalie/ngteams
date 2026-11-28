"""The sample bot's turns, the same in every SDK's sample: text, Markdown and cards, an edit, a delete, typing, a
member read, a proactive message, a card's submit and `Action.Execute`, a search and an action command, and a mention
in a group chat or a channel. All names and texts are synthetic."""

import asyncio
import re

from botbuilder.core import CardFactory, MessageFactory, TurnContext
from botbuilder.core.teams import TeamsActivityHandler, TeamsInfo
from botbuilder.schema import (
    Activity,
    ActivityTypes,
    AdaptiveCardInvokeResponse,
    AdaptiveCardInvokeValue,
    Attachment,
)
from botbuilder.schema.teams import (
    MessagingExtensionAction,
    MessagingExtensionActionResponse,
    MessagingExtensionAttachment,
    MessagingExtensionQuery,
    MessagingExtensionResponse,
    MessagingExtensionResult,
    TaskModuleContinueResponse,
    TaskModuleTaskInfo,
)

NAME = "botbuilder-python"
ADAPTIVE = "application/vnd.microsoft.card.adaptive"

CARD = {
    "type": "AdaptiveCard",
    "version": "1.5",
    "body": [
        {"type": "TextBlock", "text": f"Pick one ({NAME}, synthetic)"},
        {"type": "Input.Text", "id": "comment", "placeholder": "Why?"},
    ],
    "actions": [
        {"type": "Action.Submit", "title": "Vote yes", "data": {"choice": "yes", "sdk": NAME}},
        {"type": "Action.OpenUrl", "title": "Docs", "url": "https://docs.example.test/sdk"},
    ],
}

APPROVAL = {
    "type": "AdaptiveCard",
    "version": "1.5",
    "body": [{"type": "TextBlock", "text": f"Approve the order? ({NAME}, synthetic)"}],
    "actions": [{"type": "Action.Execute", "title": "Approve", "verb": "approve", "data": {"sdk": NAME}}],
}

ORDER_FORM = {
    "type": "AdaptiveCard",
    "version": "1.5",
    "body": [
        {"type": "TextBlock", "text": f"What to order from {NAME}?"},
        {"type": "Input.Text", "id": "item", "placeholder": "Item"},
    ],
    "actions": [{"type": "Action.Submit", "title": "Order"}],
}


def hero(title: str, preview: bool = False) -> MessagingExtensionAttachment:
    return MessagingExtensionAttachment(
        content_type="application/vnd.microsoft.card.hero",
        content={"title": title},
        preview=Attachment(content_type="application/vnd.microsoft.card.thumbnail", content={"title": title})
        if preview
        else None,
    )


def results(attachments: list) -> MessagingExtensionResult:
    return MessagingExtensionResult(type="result", attachment_layout="list", attachments=attachments)


def sorted_pairs(value: dict) -> str:
    return " ".join(f"{key}={value[key]}" for key in sorted(value))


class SampleBot(TeamsActivityHandler):
    def __init__(self, adapter, app_id: str):
        self._adapter = adapter
        self._app_id = app_id
        self._later = set()

    async def on_message_activity(self, turn_context: TurnContext):
        activity = turn_context.activity
        text = (activity.text or "").strip()
        kind = activity.conversation.conversation_type
        if kind in ("groupChat", "channel"):
            words = re.sub(r"<at>[^<]*</at>", "", text).strip()
            await turn_context.send_activity(f"mentioned in {kind} from {NAME}: {words}")
            return
        if isinstance(activity.value, dict) and not text:
            await turn_context.send_activity(f"submitted from {NAME}: {sorted_pairs(activity.value)}")
            return
        await self._word(turn_context, text)

    async def _word(self, turn_context: TurnContext, text: str):
        send = turn_context.send_activity
        if text == "card":
            await send(MessageFactory.attachment(CardFactory.adaptive_card(CARD)))
        elif text == "execute":
            await send(MessageFactory.attachment(CardFactory.adaptive_card(APPROVAL)))
        elif text == "markdown":
            await send(f"**bold** from {NAME}")
        elif text == "edit":
            draft = await send(f"draft from {NAME}")
            await turn_context.update_activity(Activity(id=draft.id, type="message", text=f"final from {NAME}"))
        elif text == "delete":
            doomed = await send(f"gone soon from {NAME}")
            await turn_context.delete_activity(doomed.id)
            await send(f"deleted from {NAME}")
        elif text == "typing":
            await send(Activity(type=ActivityTypes.typing))
            await send(f"typed from {NAME}")
        elif text == "member":
            member = await TeamsInfo.get_member(turn_context, turn_context.activity.from_property.id)
            await send(f"member from {NAME}: {member.name}")
        elif text == "proactive":
            reference = TurnContext.get_conversation_reference(turn_context.activity)
            task = asyncio.create_task(self._proactive(reference))
            self._later.add(task)
            task.add_done_callback(self._later.discard)
        else:
            await send(f"echo from {NAME}: {text}")

    async def _proactive(self, reference):
        await asyncio.sleep(0.2)

        async def later(context: TurnContext):
            await context.send_activity(f"proactive from {NAME}")

        await self._adapter.continue_conversation(reference, later, self._app_id)

    async def on_teams_messaging_extension_query(
        self, turn_context: TurnContext, query: MessagingExtensionQuery
    ) -> MessagingExtensionResponse:
        words = str(query.parameters[0].value) if query.parameters else ""
        found = [hero(f"{words} {n} from {NAME}", preview=True) for n in ("one", "two")]
        return MessagingExtensionResponse(compose_extension=results(found))

    async def on_teams_messaging_extension_fetch_task(
        self, turn_context: TurnContext, action: MessagingExtensionAction
    ) -> MessagingExtensionActionResponse:
        form = TaskModuleTaskInfo(title=f"Order ({NAME})", card=CardFactory.adaptive_card(ORDER_FORM))
        return MessagingExtensionActionResponse(task=TaskModuleContinueResponse(value=form))

    async def on_teams_messaging_extension_submit_action(
        self, turn_context: TurnContext, action: MessagingExtensionAction
    ) -> MessagingExtensionActionResponse:
        item = (action.data or {}).get("item")
        return MessagingExtensionActionResponse(compose_extension=results([hero(f"ordered {item} from {NAME}")]))

    async def on_adaptive_card_invoke(
        self, turn_context: TurnContext, invoke_value: AdaptiveCardInvokeValue
    ) -> AdaptiveCardInvokeResponse:
        name = turn_context.activity.from_property.name
        done = {
            "type": "AdaptiveCard",
            "version": "1.5",
            "body": [{"type": "TextBlock", "text": f"approved by {name} from {NAME}"}],
        }
        return AdaptiveCardInvokeResponse(status_code=200, type=ADAPTIVE, value=done)
