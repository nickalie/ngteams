// The Teams SDK v2 (TypeScript) sample bot of an own ngteams server, which its container tests run. It runs against an own
// server by configuration and one token callback: `cloud` points the SDK's checks of the server's activity tokens at
// the server's keys and issuer; the SDK's msal-node takes no known authorities, so the bot gets its own token through
// `token`, with msal-node told the server's host. Its token service (`tokenServiceUrl`) is the server too: `signin`
// runs the SDK's OAuth flow for the connection `synthetic-idp`. All names and texts are synthetic.
import { createHash } from 'node:crypto';
import { ConfidentialClientApplication } from '@azure/msal-node';
import { PUBLIC, withOverrides } from '@microsoft/teams.api';
import { App } from '@microsoft/teams.apps';

const env = (name: string): string => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
};

const origin = env('ORIGIN');
const tenant = env('TENANT');
const scope = process.env.SCOPE || 'https://api.botframework.com';
const clientId = env('BOT_ID');
const name = 'teams-sdk';
const connection = 'synthetic-idp';
// what the bot says of a token: a digest, never the token
const digest = (token: string): string => createHash('sha256').update(token).digest('hex').slice(0, 12);

const msal = new ConfidentialClientApplication({
  auth: {
    clientId,
    clientSecret: env('BOT_SECRET'),
    authority: `${origin}/${tenant}`,
    knownAuthorities: [new URL(origin).host],
  },
});

const app = new App({
  clientId,
  tenantId: tenant,
  serviceUrl: `${origin}/bot/`,
  oauthFlows: [connection],
  cloud: withOverrides(PUBLIC, {
    // the SDK takes an issuer under its login endpoint for an Entra one: keep the two apart
    loginEndpoint: `${origin}/login`,
    loginTenant: tenant,
    botScope: `${scope}/.default`,
    tokenServiceUrl: origin,
    openIdMetadataUrl: `${origin}/bot/v1/.well-known/openidconfiguration`,
    tokenIssuer: `${origin}/bot`,
  }),
  token: async (wanted: string | string[]) => {
    const scopes = Array.isArray(wanted) ? wanted : [wanted];
    const result = await msal.acquireTokenByClientCredential({ scopes });
    if (!result) throw new Error('no token');
    return result.accessToken;
  },
});

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

const adaptive = (content: object) => ({ contentType: 'application/vnd.microsoft.card.adaptive', content });
const hero = (title: string) => ({ contentType: 'application/vnd.microsoft.card.hero', content: { title } });

const approval = {
  type: 'AdaptiveCard',
  version: '1.5',
  body: [{ type: 'TextBlock', text: `Approve the order? (${name}, synthetic)` }],
  actions: [{ type: 'Action.Execute', title: 'Approve', verb: 'approve', data: { sdk: name } }],
};

const orderForm = {
  type: 'AdaptiveCard',
  version: '1.5',
  body: [
    { type: 'TextBlock', text: `What to order from ${name}?` },
    { type: 'Input.Text', id: 'item', placeholder: 'Item' },
  ],
  actions: [{ type: 'Action.Submit', title: 'Order' }],
};

const withoutMentions = (text: string): string => text.replace(/<at>[^<]*<\/at>/g, '').trim();

const sorted = (value: Record<string, unknown>): string =>
  Object.keys(value)
    .sort()
    .map((key) => `${key}=${String(value[key])}`)
    .join(' ');

const signIn = app.getOAuthFlow(connection);
signIn.onSignInComplete(async (ctx, token) => {
  await ctx.send(`signed in to ${name}: ${digest(token.token)}`);
});

app.on('message.ext.query', async ({ activity }) => {
  const query = String(activity.value.parameters?.[0]?.value ?? '');
  const results = ['one', 'two'].map((n) => {
    const title = `${query} ${n} from ${name}`;
    return { ...hero(title), preview: { contentType: 'application/vnd.microsoft.card.thumbnail', content: { title } } };
  });
  return { composeExtension: { type: 'result', attachmentLayout: 'list', attachments: results } } as never;
});

app.on('message.ext.open', async () => {
  return { task: { type: 'continue', value: { title: `Order (${name})`, card: adaptive(orderForm) } } } as never;
});

app.on('message.ext.submit', async ({ activity }) => {
  const data = (activity.value.data ?? {}) as Record<string, unknown>;
  const result = hero(`ordered ${String(data.item)} from ${name}`);
  return { composeExtension: { type: 'result', attachmentLayout: 'list', attachments: [result] } } as never;
});

app.on('card.action', async ({ activity }) => {
  const text = `approved by ${activity.from.name} from ${name}`;
  const done = { type: 'AdaptiveCard', version: '1.5', body: [{ type: 'TextBlock', text }] };
  return { statusCode: 200, type: 'application/vnd.microsoft.card.adaptive', value: done } as never;
});

app.on('message', async (ctx) => {
  const { activity, send, api } = ctx;
  const conversation = activity.conversation.id;
  const text = (activity.text ?? '').trim();
  const kind = activity.conversation.conversationType;
  if (kind === 'groupChat' || kind === 'channel') {
    await send(`mentioned in ${kind} from ${name}: ${withoutMentions(text)}`);
    return;
  }
  if (activity.value && typeof activity.value === 'object' && !text) {
    await send(`submitted from ${name}: ${sorted(activity.value as Record<string, unknown>)}`);
    return;
  }
  switch (text) {
    case 'card':
      await send({
        type: 'message',
        attachments: [{ contentType: 'application/vnd.microsoft.card.adaptive', content: card }],
      } as never);
      return;
    case 'execute':
      await send({ type: 'message', attachments: [adaptive(approval)] } as never);
      return;
    case 'markdown':
      await send(`**bold** from ${name}`);
      return;
    case 'edit': {
      const draft = await send(`draft from ${name}`);
      await api.conversations.updateActivity(conversation, draft.id, {
        type: 'message',
        text: `final from ${name}`,
      } as never);
      return;
    }
    case 'delete': {
      const doomed = await send(`gone soon from ${name}`);
      await api.conversations.deleteActivity(conversation, doomed.id);
      await send(`deleted from ${name}`);
      return;
    }
    case 'typing':
      await send({ type: 'typing' } as never);
      await send(`typed from ${name}`);
      return;
    case 'member': {
      const member = await api.conversations.getMemberById(conversation, activity.from.id);
      await send(`member from ${name}: ${member.name}`);
      return;
    }
    case 'signin': {
      const token = await signIn.signIn(ctx);
      if (token) await send(`already signed in to ${name}: ${digest(token)}`);
      return;
    }
    case 'signout':
      await signIn.signOut(ctx);
      await send(`signed out of ${name}`);
      return;
    case 'proactive':
      setTimeout(() => {
        app.send(conversation, `proactive from ${name}`).catch((error: unknown) => console.error(error));
      }, 200);
      return;
    default:
      await send(`echo from ${name}: ${text}`);
  }
});

app.start(Number(process.env.PORT ?? 3978)).catch((error: unknown) => {
  console.error(error);
  process.exit(1);
});
