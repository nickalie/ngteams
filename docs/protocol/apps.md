# The store and the account's apps

Status: 2026-11-16. From `proto-lab run team-manage` with lab account a, every add and remove held in the browser by
step holds (`POST`/`DELETE` on `/users/apps/entitlements` and `/chats/…/apps/entitlements`), so the shapes are seen and
nothing was changed. The chat add (201) was seen live on 2026-11-16 by part 1's Approvals run in the a–b chat. The
native client: reads in `crates/teams-proto/src/apps.rs` and `team_apps.rs`, writes in `write/team.rs`
(`TeamWrite`), the engine's part in `teams-core`'s `team_apps`, the views in `teams-ui`'s `apps_page` (behind
`NGTEAMS_BOTS=1`). A team's apps and Manage team's Apps tab are in [teams.md](teams.md#manage-team).

## The store

The rail's item is **Store** (the plus in a square). The page: "Store" with Home, Featured, Categories, Devices; a gear
for "Manage your apps"; "Do more in Teams" / "Discover agents and apps that can help you tackle any task." over a
search box ("Search agents and apps"); "Added by your org" / "Made available by your organization's technology team"
with its tiles and "See all" (on each section, whatever its count; the native page's "See all" shows every app of
the section, its row on the front page shows 24); then curated rows ("Agents we love", topic rows) whose lists come from a service the
middle tier doesn't carry and the native client doesn't read.

- **Its apps**: `POST beta/users/apps/store` on the middle tier (the body as `AppStore` sends it), answered
  `{"appCategoriesDictionary": {<category>: {"apps": [{"id", "version", "ranking", "rating": {"count", "average"},
  "metaCapabilities"}], "totalAppsCount", "blockedAppsCount"?}}, "appMetaCapabilitiesDictionary", "frequentCollaborators",
  …}`. "Added by your org" is the `adminPreInstalledApps` category; `popularApps` and `microsoft` are among about forty
  more (`all`, `personalApps`, `productivity`, `iN_<industry>`, …), each a hundred apps at most. Names, makers, lines and
  pictures come from `POST beta/users/apps/batchedDefinitions?includeblockedapps=true` with the ids.
- **A tile**: the app's picture (52 px, a check at its bottom right once the account added it), its name, maker, a line
  (`shortDescription`, two lines at most) and its rating, "★ 4.0 (881 ratings)" or "No ratings yet" (`1.1K`, `74K` for
  larger counts). A tile's "..." holds About, Share and, for an added app, Uninstall.
- **Search**: Enter shows "Search results for "<words>"" and "Agents and apps (N)" in tiles. The browser's search goes to
  a service the middle tier doesn't carry (not seen here); the native page searches the store's listed apps by name,
  maker and line instead.

## An app's details

A dialog: the picture, the name, the maker, **Add** (or **Open** once added) and a share button; a row of facts (the
rating and how many rated, installs in the last month, "Works in Teams", "Free"; the native row shows "No ratings yet"
for an app nobody rated, as its tile does); screenshots; the short and the long
description; "Capabilities in Teams" (Bot "Chat using prompts to complete simple tasks.", Messaging "Insert app content
in messages to send more than just text.", Personal app "Use the app privately outside of a chat, channel, or
meeting.", Tab "Collaborate in a chat or channel by adding a tab for the app."); Reviews (from AppSource); "Permissions
overview" ("Permission to read and access:" Information related to you; Information in your chats, channels, and
meetings. "Permission to perform these actions:" Send and receive messages and notifications); Details (Created by,
Version, Pricing, Languages); "More like this"; and at the foot "By using <app>, you agree to the privacy policy, terms
of use, and permissions." with the definition's `privacyUrl` and `termsOfUseUrl`.

Read from the definition: a capability shows when the definition has `bots` (Bot), `inputExtensions` (Messaging), a
`staticTabs` entry with no scopes or the `personal` one (Personal app: Polls' `staticTabs` scoped to `GroupChat` and
`Team` showed none) and `configurableTabs` or `galleryTabs` (Tab). The permission lines are our reading of the
definition (a web application or the `Identity` permission; a bot or a resource-specific permission; a bot, or
activities alone), not a field.

**Add** sends `POST beta/users/apps/entitlements` with the app's definition as the body, as given by the catalogue
(201, `{"value": {"id", "state": "Installed", …}}`, seen earlier). Held, the dialog said "App wasn't added because you're
offline" / "Check your connection, then try adding the app again." with Close. An added app's details show a "Let's go"
step with Open. Where the definition's scopes allow a chat or a team, Teams then asks for the place ("Choose a place to
use the app", seen from Manage team, [teams.md](teams.md#manage-team)); the native dialog offers "Add to a chat" and
"Add to a team" beside Add, which open that picker.

## Manage your apps

The gear opens "Manage your apps" (tab Apps): "Get more apps", "Upload an app", Sort, "Find by name", and a row per app
the account has: its picture, name and maker, a chevron and a "..." (View details, Copy link). The chevron opens the
row to where the app is added ("Recently used"; "Personal app") with a trash button, **Remove**, which asks "Remove
<app>" / "Are you sure you want to remove <app>?" (Cancel, Remove). Remove sends
`DELETE beta/users/apps/entitlements/<app id>`, no body (held here; a held one made the dialog say "There was an issue
while trying to remove <app>. Please try again.").

The account's apps: `POST beta/users/apps/aggregatedEntitlements?appbarview=userpinned` with a list of team threads
(`[]` for none), answered `{"value": {"userEntitlements": {<own object id>: [{"id", "state", "installedVersion"?,
"installerIdentity"?, "isAppBarPinned"?, …}]}, "teamsEntitlements"?, "definitions": {<app id>: <definition>},
"userEntitlementsHash"}}`. `Installed` are the ones the account added; `InstalledAndPermanent` and
`InstalledAndDeprecated` are Teams' own, `Featured` the store's offers. The browser's list showed the `PreConsented`
ones too (Approvals first, `PreConsented` only, in the lab capture), and its Remove sends the same `DELETE`: the native
list shows `Installed` and `PreConsented`, by name.

## A chat's apps

In a chat, the header's "Add a tab" opens a dialog of tab apps and "Add a new app" (store tiles with ratings), "Manage
your apps", Close and "Get more apps". "Manage your apps" there lists the chat's apps in the chat's pane: "Get more
apps", Sort, "Find by name", and a row per app with a "..." of Copy link and **Remove**, which asks as above and sends
`DELETE beta/chats/<chat>/apps/entitlements/<app id>`, no body (held). The 1:1 chat's details pane had no apps.

- **Its apps**: `GET beta/chats/<chat>/apps/chatentitlements`, answered `{"value": {"entitlements": [{"id", "state",
  "installedBy"?, "authorization"?, …}], "chatEntitlementsHash", "definitions": [<definition>], "appDefinitionsHash"}}`
  (definitions as a list). `Installed` are the chat's added apps (Approvals, and Teams' newly preinstalled Power BI and
  Updates in the lab chat); the rest are `InstalledAndPermanent`, `InstalledAndDeprecated`, `PreConsented` and
  `HiddenFromAppStore`.
- **Add**: `POST beta/chats/<chat>/apps/entitlements` with the definition as the body, 201, `{"value": {"id", "state":
  "Installed", "installedVersion", "createdTime", "installedBy", "installerIdentity", "botMetadata"}}`. The chat then
  showed "<name> added <app> here. Try an action below to see how it works."

## A bot's app

A bot's chat with the account (`19:<account>_<bot id>@unq.gbl.spaces`, the bot by its bare id as a person is) and a
bot's messages (`28:<bot id>`) are shown by the app whose definition names the bot in `bots[].id`, as Teams shows them:
the app's name and its picture on a tile, the app's details under the chat's About. The native engine keeps every
definition its reads bring (the store, the account's apps, a chat's and a team's) by its bots' ids for the run; a bot
none of them names is looked for in the account's apps (`aggregatedEntitlements`, every definition in its answer, Teams'
own and the store's offers too), then, for an id known to be a bot's (a `28:` member or sender of the chat), by its id as
an app's in `batchedDefinitions` until a read of it is answered (many apps' bot has the app's id); asks at once for
one bot share one answered read, and an ask that waited on a read that failed sends its own. An id that may be a
person's is never read as an app's. A 1:1 chat whose other one is neither a stored `28:` member, nor a `28:` sender,
nor a bot of a known app is with a person.

## Own servers

An own-server account's definitions name their pictures on the server's own origin under `/teamsappdata/` (the CDN's
folder): the native client reads them from there with no token (`AppPicture::of_in`), and never reads a tile off that
origin for such an account. The store, entitlement reads and writes go to the account's own middle tier, as every
middle-tier request does; the server serves them ([server.md](../server.md#the-app-catalogue-and-installs)). The client offers the Store, an app's details and
a chat's apps only while the server names `appsService` (`teams_core::Capabilities`), as Manage team's Apps tab.
