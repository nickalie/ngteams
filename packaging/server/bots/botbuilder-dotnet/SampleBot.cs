// The sample bot's turns, the same in every SDK's sample: text, Markdown and cards, an edit, a delete, typing, a
// member read, a proactive message, a card's submit and Action.Execute, a search and an action command, and a mention
// in a group chat or a channel. All names and texts are synthetic.
using System.Text.RegularExpressions;
using Microsoft.Bot.Builder;
using Microsoft.Bot.Builder.Integration.AspNet.Core;
using Microsoft.Bot.Builder.Teams;
using Microsoft.Bot.Schema;
using Microsoft.Bot.Schema.Teams;
using Newtonsoft.Json.Linq;

public class SampleBot(CloudAdapter adapter, IConfiguration configuration) : TeamsActivityHandler
{
    private const string Name = "botbuilder-dotnet";
    private const string Adaptive = "application/vnd.microsoft.card.adaptive";

    private static readonly JObject Card = JObject.FromObject(new
    {
        type = "AdaptiveCard",
        version = "1.5",
        body = new object[]
        {
            new { type = "TextBlock", text = $"Pick one ({Name}, synthetic)" },
            new { type = "Input.Text", id = "comment", placeholder = "Why?" },
        },
        actions = new object[]
        {
            new { type = "Action.Submit", title = "Vote yes", data = new { choice = "yes", sdk = Name } },
            new { type = "Action.OpenUrl", title = "Docs", url = "https://docs.example.test/sdk" },
        },
    });

    private static readonly JObject Approval = JObject.FromObject(new
    {
        type = "AdaptiveCard",
        version = "1.5",
        body = new object[] { new { type = "TextBlock", text = $"Approve the order? ({Name}, synthetic)" } },
        actions = new object[]
        {
            new { type = "Action.Execute", title = "Approve", verb = "approve", data = new { sdk = Name } },
        },
    });

    private static readonly JObject OrderForm = JObject.FromObject(new
    {
        type = "AdaptiveCard",
        version = "1.5",
        body = new object[]
        {
            new { type = "TextBlock", text = $"What to order from {Name}?" },
            new { type = "Input.Text", id = "item", placeholder = "Item" },
        },
        actions = new object[] { new { type = "Action.Submit", title = "Order" } },
    });

    private static Attachment AdaptiveCard(JObject card) => new() { ContentType = Adaptive, Content = card };

    private static MessagingExtensionAttachment Hero(string title, bool preview = false) => new()
    {
        ContentType = HeroCard.ContentType,
        Content = new HeroCard { Title = title },
        Preview = preview ? new Attachment { ContentType = ThumbnailCard.ContentType, Content = new ThumbnailCard { Title = title } } : null,
    };

    private static MessagingExtensionResult Results(params MessagingExtensionAttachment[] attachments) => new()
    {
        Type = "result",
        AttachmentLayout = "list",
        Attachments = attachments,
    };

    protected override async Task OnMessageActivityAsync(ITurnContext<IMessageActivity> turnContext, CancellationToken cancellationToken)
    {
        var activity = turnContext.Activity;
        var text = (activity.Text ?? string.Empty).Trim();
        var kind = activity.Conversation.ConversationType;
        if (kind is "groupChat" or "channel")
        {
            var words = Regex.Replace(text, "<at>[^<]*</at>", string.Empty).Trim();
            await turnContext.SendActivityAsync($"mentioned in {kind} from {Name}: {words}", cancellationToken: cancellationToken);
            return;
        }
        if (activity.Value is JObject value && text.Length == 0)
        {
            var pairs = value.Properties().OrderBy(p => p.Name, StringComparer.Ordinal).Select(p => $"{p.Name}={p.Value}");
            await turnContext.SendActivityAsync($"submitted from {Name}: {string.Join(' ', pairs)}", cancellationToken: cancellationToken);
            return;
        }
        await WordAsync(turnContext, text, cancellationToken);
    }

    private async Task WordAsync(ITurnContext<IMessageActivity> turnContext, string text, CancellationToken cancellationToken)
    {
        Task<ResourceResponse> Send(string words) => turnContext.SendActivityAsync(words, cancellationToken: cancellationToken);
        switch (text)
        {
            case "card":
                await turnContext.SendActivityAsync(MessageFactory.Attachment(AdaptiveCard(Card)), cancellationToken);
                break;
            case "execute":
                await turnContext.SendActivityAsync(MessageFactory.Attachment(AdaptiveCard(Approval)), cancellationToken);
                break;
            case "markdown":
                await Send($"**bold** from {Name}");
                break;
            case "edit":
                var draft = await Send($"draft from {Name}");
                var final = MessageFactory.Text($"final from {Name}");
                final.Id = draft.Id;
                await turnContext.UpdateActivityAsync(final, cancellationToken);
                break;
            case "delete":
                var doomed = await Send($"gone soon from {Name}");
                await turnContext.DeleteActivityAsync(doomed.Id, cancellationToken);
                await Send($"deleted from {Name}");
                break;
            case "typing":
                await turnContext.SendActivityAsync(new Activity(ActivityTypes.Typing), cancellationToken);
                await Send($"typed from {Name}");
                break;
            case "member":
                var member = await TeamsInfo.GetMemberAsync(turnContext, turnContext.Activity.From.Id, cancellationToken);
                await Send($"member from {Name}: {member.Name}");
                break;
            case "proactive":
                var reference = turnContext.Activity.GetConversationReference();
                _ = Task.Run(() => ProactiveAsync(reference));
                break;
            default:
                await Send($"echo from {Name}: {text}");
                break;
        }
    }

    private async Task ProactiveAsync(ConversationReference reference)
    {
        await Task.Delay(200);
        await adapter.ContinueConversationAsync(
            configuration["MicrosoftAppId"],
            reference,
            (context, token) => context.SendActivityAsync($"proactive from {Name}", cancellationToken: token),
            CancellationToken.None);
    }

    protected override Task<MessagingExtensionResponse> OnTeamsMessagingExtensionQueryAsync(ITurnContext<IInvokeActivity> turnContext, MessagingExtensionQuery query, CancellationToken cancellationToken)
    {
        var words = query.Parameters?.FirstOrDefault()?.Value?.ToString() ?? string.Empty;
        var found = new[] { "one", "two" }.Select(n => Hero($"{words} {n} from {Name}", preview: true)).ToArray();
        return Task.FromResult(new MessagingExtensionResponse { ComposeExtension = Results(found) });
    }

    protected override Task<MessagingExtensionActionResponse> OnTeamsMessagingExtensionFetchTaskAsync(ITurnContext<IInvokeActivity> turnContext, MessagingExtensionAction action, CancellationToken cancellationToken)
    {
        var form = new TaskModuleTaskInfo { Title = $"Order ({Name})", Card = AdaptiveCard(OrderForm) };
        return Task.FromResult(new MessagingExtensionActionResponse { Task = new TaskModuleContinueResponse { Value = form } });
    }

    protected override Task<MessagingExtensionActionResponse> OnTeamsMessagingExtensionSubmitActionAsync(ITurnContext<IInvokeActivity> turnContext, MessagingExtensionAction action, CancellationToken cancellationToken)
    {
        var item = (action.Data as JObject)?["item"]?.ToString();
        return Task.FromResult(new MessagingExtensionActionResponse { ComposeExtension = Results(Hero($"ordered {item} from {Name}")) });
    }

    protected override Task<AdaptiveCardInvokeResponse> OnAdaptiveCardInvokeAsync(ITurnContext<IInvokeActivity> turnContext, AdaptiveCardInvokeValue invokeValue, CancellationToken cancellationToken)
    {
        var done = JObject.FromObject(new
        {
            type = "AdaptiveCard",
            version = "1.5",
            body = new object[] { new { type = "TextBlock", text = $"approved by {turnContext.Activity.From.Name} from {Name}" } },
        });
        return Task.FromResult(new AdaptiveCardInvokeResponse { StatusCode = 200, Type = Adaptive, Value = done });
    }
}
