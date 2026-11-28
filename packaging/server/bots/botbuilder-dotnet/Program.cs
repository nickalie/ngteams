// The Bot Framework SDK for .NET sample bot of an own ngteams server, on ASP.NET Core. Its signed setup is
// configuration alone: the SDK's own keys (appsettings.json, or the environment as run.sh sets it).
using Microsoft.Bot.Builder;
using Microsoft.Bot.Builder.Integration.AspNet.Core;
using Microsoft.Bot.Connector.Authentication;

var builder = WebApplication.CreateBuilder(args);
builder.Services.AddHttpClient();
builder.Services.AddSingleton<BotFrameworkAuthentication, ConfigurationBotFrameworkAuthentication>();
builder.Services.AddSingleton(services => new CloudAdapter(
    services.GetRequiredService<BotFrameworkAuthentication>(),
    services.GetRequiredService<ILogger<CloudAdapter>>()));
builder.Services.AddSingleton<IBotFrameworkHttpAdapter>(services => services.GetRequiredService<CloudAdapter>());
builder.Services.AddTransient<IBot, SampleBot>();

var app = builder.Build();
app.MapPost(
    "/api/messages",
    (HttpRequest request, HttpResponse response, IBotFrameworkHttpAdapter adapter, IBot bot) =>
        adapter.ProcessAsync(request, response, bot));
app.Run();
