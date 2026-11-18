// An echo-and-cards bot on the Microsoft 365 Agents SDK (JavaScript) for ngteams-server's container tests. It checks
// the server's activity tokens by configuration alone: the authority and tenant point the SDK at the server's keys and
// `issuers` names the server's issuer. Its msal-node takes no known authorities and asks Microsoft about any other
// authority, so the bot gets its own token from a provider of a few lines, msal-node told the server's host. All
// names and texts are synthetic.
const { ConfidentialClientApplication } = require('@azure/msal-node');
const { AgentApplication, CloudAdapter, MemoryStorage } = require('@microsoft/agents-hosting');
const { startServer } = require('@microsoft/agents-hosting-express');

function env(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
}

const origin = env('ORIGIN');
const name = 'agents-sdk';
const authConfig = {
  clientId: env('BOT_ID'),
  clientSecret: env('BOT_SECRET'),
  tenantId: env('TENANT'),
  authorityEndpoint: origin,
  issuers: [`${origin}/bot`],
  scope: `${env('SCOPE')}/.default`,
};

const msal = new ConfidentialClientApplication({
  auth: {
    clientId: authConfig.clientId,
    clientSecret: authConfig.clientSecret,
    authority: `${origin}/${authConfig.tenantId}`,
    knownAuthorities: [new URL(origin).host],
  },
});
// every token the bot needs is for the server's bot resource
const tokens = {
  async getAccessToken() {
    const result = await msal.acquireTokenByClientCredential({ scopes: [authConfig.scope] });
    return result.accessToken;
  },
};

const adapter = new CloudAdapter(authConfig);
adapter.connectionManager.getTokenProvider = () => tokens;
adapter.connectionManager.getTokenProviderFromActivity = () => tokens;
const app = new AgentApplication({ adapter, storage: new MemoryStorage() });

const card = {
  type: 'AdaptiveCard',
  version: '1.5',
  body: [
    { type: 'TextBlock', text: `Pick one (${name}, synthetic)` },
    { type: 'Input.Text', id: 'comment', placeholder: 'Why?' },
  ],
  actions: [
    { type: 'Action.Submit', title: 'Vote yes', data: { choice: 'yes', sdk: name } },
    { type: 'Action.OpenUrl', title: 'Docs', url: 'https://docs.example.test/sdk' },
  ],
};

function sorted(value) {
  return Object.keys(value)
    .sort()
    .map((key) => `${key}=${String(value[key])}`)
    .join(' ');
}

function connector(context) {
  return context.turnState.get(adapter.ConnectorClientKey);
}

app.onActivity('message', async (context) => {
  const activity = context.activity;
  const conversation = activity.conversation.id;
  const text = (activity.text || '').trim();
  if (activity.value && typeof activity.value === 'object' && !text) {
    await context.sendActivity(`submitted from ${name}: ${sorted(activity.value)}`);
    return;
  }
  switch (text) {
    case 'card':
      await context.sendActivity({
        type: 'message',
        attachments: [{ contentType: 'application/vnd.microsoft.card.adaptive', content: card }],
      });
      return;
    case 'markdown':
      await context.sendActivity(`**bold** from ${name}`);
      return;
    case 'edit': {
      const draft = await context.sendActivity(`draft from ${name}`);
      await connector(context).updateActivity(conversation, draft.id, {
        type: 'message',
        id: draft.id,
        text: `final from ${name}`,
      });
      return;
    }
    case 'delete': {
      const doomed = await context.sendActivity(`gone soon from ${name}`);
      await context.deleteActivity(doomed.id);
      await context.sendActivity(`deleted from ${name}`);
      return;
    }
    case 'typing':
      await context.sendActivity({ type: 'typing' });
      await context.sendActivity(`typed from ${name}`);
      return;
    case 'member': {
      const member = await connector(context).getConversationMember(activity.from.id, conversation);
      await context.sendActivity(`member from ${name}: ${member.name}`);
      return;
    }
    case 'proactive': {
      const reference = activity.getConversationReference();
      setTimeout(() => {
        adapter
          .continueConversation(authConfig.clientId, reference, async (later) => {
            await later.sendActivity(`proactive from ${name}`);
          })
          .catch((error) => console.error(error));
      }, 200);
      return;
    }
    default:
      await context.sendActivity(`echo from ${name}: ${text}`);
  }
});

startServer(app, { authConfig, port: Number(process.env.PORT || 3978) });
