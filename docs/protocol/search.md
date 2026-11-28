# Search

What Teams' title-bar search asks of the service, as the lab's `search` scenario recorded it on 2026-10-31
(`proto-lab run search`: account a focuses the box, types, enters the query, opens each results tab and a message
found; `PROTO_LAB_SEARCH_QUERY` picks the query). All of it goes to Substrate's search on `substrate.office.com`, with
the account's token for `https://outlook.office.com/search` (its `aud` an application id, [auth.md](auth.md)). Every
request here is a search read, independent of any participant-policy classification.

The native client's side is `teams_proto::SubstrateSearch` (the two requests, built whole), sent only by
`HttpTransport::search` (debug wire logged, available in read-only mode too, since it writes nothing),
read by `teams_core::service_search` and shown by `teams_ui::global_search`.

## The box's two modes

- **Ctrl+Alt+G** opens the box in a "go to" mode, marked `/goto` before the text: what is typed matches the account's
  chats and the teams' channels by name, listed at once with the first one highlighted; Enter opens it. The web
  client asks the service nothing for it: the matching is done on what it already holds.
- **Ctrl+E** (or a click) opens it to search. As the user types, it asks for suggestions; Enter (with no suggestion
  picked) shows the results across the main area, with tabs: All, Messages, Files, People, Channels, Group chats and
  more behind a `+n`. Picking a tab adds `is:<Kind>` to the box's text.

## The suggestions

`POST search/api/v1/suggestions?scenario=powerbar&setflight=…`, once per character typed (an earlier one in flight is
cancelled). Its body lists one entity request per kind, each with the query (`QueryString`, `NormalizedQueryString`,
for people also `DisplayQueryString`), a kind (`EntityType`) and a count (`Size`): `People` (5, with the fields asked
for, a filter on the person's type and subtype and `NonHidden`, from the mailbox and the directory), `File` (3), `Chat`
(5), `Team` (3), `Channel` (4, standard and shared ones), `Text` (2), `Message` (11), `Event` (3), `App` (5). Beside
them the scenario (`Name: powerbar` with a `QueryType` dimension), a correlation id (`Cvid`) and a logical id (fresh
GUIDs that name nothing of the account), and `AppName`.

The answer is `Groups`, each with a `Type` (`People`, `Chat`, `Team`, `Channel`, `File`, `Message`, `Event`, `Text`)
and its `Suggestions`:

- a person: `DisplayName`, `MRI`, `UserPrincipalName`, `EmailAddresses`, `GivenName`, `Surname`, `JobTitle` and
  `Department` when known, `ExternalDirectoryObjectId`;
- a group chat: `ThreadId`, the members the query matched (`MatchingMembers`) and some others (`ChatMembers`), each
  with `DisplayName` and `MRI`, the member count and the last message's time; no name of its own unless it has a topic;
- a team: `Name`, `ThreadId` (its General channel's) and `GroupId`;
- an event: the meeting's subject, organizer, attendees, times and links;
- `Text`: a suggested query ("Messages from …").

The web client shows them under the box with chips for the kinds above them and "Press Enter to view all results"
at the foot.

## The results

`POST searchservice/api/v2/query`, one per tab shown (All when the query is entered, then each tab opened). Its body's
`EntityRequests` hold one request per kind, each with its kind (`entityType`), where to look (`contentSources`:
`Teams` for messages, chats and channels, `Exchange` for people, OneDrive, SharePoint and Exchange for files), the
query (`queryString`, `displayQueryString`), a page (`from`, `size`: 3 to 8 of each for All, 25 for one tab) and, by
kind, the fields wanted and the text around the match (`HitHighlight`). The scenario names the tab
(`QueryType`: `All`, `Messages`, `People`, `Files`, `GroupChats`) and the form factor. The All tab's message request
also excludes messages deleted on the client (`NOT (isClientSoftDeleted:TRUE) AND <query>`).

The answer is `EntitySets`, each with `ResultSets` (with `Total` and `MoreResultsAvailable`), each with `Results`; a
result has its `Type`, `HitHighlightedSummary` (the text around the match, the matched terms between `<c0>` and
`</c0>`, a cut as `<ddd/>`, links as HTML) and the item found as `Source`:

- a message: `ClientThreadId` (its chat or channel), `InternetMessageId` (its id in the chat service: the arrival
  time in ms), `DateTimeSent`, `Sender` (name and address), `DisplayTo` (the chat's other members, by name), `Preview`,
  and `Extensions` with the sender's MRI, the thread type and, in a channel, the thread's root message id;
- a person: as a suggested one;
- a file: `FileName`, `Title`, `FileType`, `Path`, `LinkingUrl`, `LastModifiedTime`, `ModifiedBy`, `SiteTitle` and
  the site's and parent folder's links;
- a group chat: `ThreadId` and `Members` (names);
- a team: `Name` and `ThreadId`.

Opening a message found opens its chat beside the results, at the message.

`ngteamsctl people`, `person` and `presence` send the People tab's request. `person` and `presence` use it to find
the MRI of a person named by address or name.

## What else the web client sends

- `GET search/api/v2/init?scenario=powerbar&cvid=…` and `GET search/api/v1/suggestions?scenario=powerbar&query=&…`
  as the box opens, and an empty `PowerbarContextual` query (answered 400).
- `POST search/api/v1/events` (and `…/events/`): its own telemetry of the search (what was asked, how long it took,
  what was shown). It reaches the search service alone.
- At load, the people and files caches: `suggestions?scenario=peoplecache` and `?scenario=filescache` with an empty
  query and a size of 500 (the lab tenant's files cache answers 500 at times).

The native client sends only the suggestions and the results.
