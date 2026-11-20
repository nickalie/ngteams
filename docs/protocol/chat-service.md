# Chat service: routing, IDs and reads

Protocol observations from the 2026-10-19–2026-10-27 web-client captures and native probes, retained after the write
fence was removed. Captures stay outside the repository; examples and fixtures use synthetic values. The original
capture restrictions explain missing answers, not current client permissions. See [chat.md](chat.md) for message
and attachment bodies, [calling.md](calling.md) for call tracking, and [endpoints.md](endpoints.md) for the endpoint
inventory.

## The chat service

Chats live in the chat service ("chatsvc", the IC3 messaging service). Its base URL comes from authz's region GTMs:
`chatServiceAfd`, which prod prefers, or `chatService` (see auth.md). The web client recognizes the Front Door form by
host: `teams.microsoft.com`, `teams.cloud.microsoft` (commercial), `teams.live.com` (consumer), `gov.teams.microsoft.us`
and `dod.teams.microsoft.us`, with the path `/api/chatsvc/<region>/`. Every request below is relative to that base and
starts with `v1/` (endpoint registration also with `v2/`). Conversation and thread ids are percent-encoded as one path
segment.

For the lab tenant the client uses the Front Door form with the region `amer`, on whichever Teams origin the page is
on (`teams.cloud.microsoft` after the client moves there, `teams.microsoft.com` after a deep link reloads it). It sent
no request to a `.msg.teams.microsoft.com` host. That host form showed up only as data: authz's `chatService` GTM
(`https://amer.ng.msg.teams.microsoft.com`, auth.md's Region GTMs), and the `Location` header of the 201 answers to
`POST v1/threads` and to message posts, which names the new resource on that host with a `/v1/…` path.

Credential: the web client sends the chat service an **Entra bearer token**, `Authorization: Bearer <token>` whose
audience is `https://ic3.teams.office.com` (scope `Teams.AccessAsUser.All`), on every chat service request, and no
skypetoken there. The bundle also names `Authentication: skypetoken=<skypetoken>`, which the captures never showed
([auth.md](auth.md#audience--service-table)). No `RegistrationToken` was sent in those web-client captures: the client registered no chat endpoint.

`teams_proto::chat_service_path` recognizes the native transport's chat-service URL forms: HTTPS, no port or
user info; a commercial Front Door host (`teams.microsoft.com`, `teams.cloud.microsoft`) with
`/api/chatsvc/<region>/v1/…`, or a regional `.msg.teams.microsoft.com` host with `/v1/…`. The region is 1–32 lowercase
letters, digits or `-`; regional-host labels contain letters, digits or `-`. This is URL routing, not a participant
policy. Lab reach is checked by the agent before a live run ([lab-rules.md](../lab-rules.md)).

### Ids

| Id | What |
|---|---|
| `19:<oid>_<oid>@unq.gbl.spaces` | a 1:1 chat; its id names both members' Entra object ids, lowercase and sorted |
| `19:<id>@thread.v2` | a group chat; `19:meeting_<id>@thread.v2` is a meeting's chat |
| `19:<id>@thread.tacv2`, `19:<id>@thread.skype` | a team's channel (the team's General channel has the team's id) |
| `<channel id>;messageid=<root id>` | a reply chain in a channel (the client joins the root message id to the conversation id) |
| `48:notes`, `48:…` | the user's own streams (notes, notifications, mentions, call logs, thread index) |
| `8:orgid:<oid>` | a work account's MRI; `28:<id>` a bot |

`teams_proto` owns `Mri` and the chat/team/meeting ID helpers. A chat ID is either the 1:1 form naming two distinct
object IDs or a group/meeting `19:<id>@thread.v2`, with a 1–128-character ASCII identifier made of letters, digits,
`_` or `-`. A channel and a reply chain are distinct forms.

## Reading a chat's members

The client's member reads (all `GET`, relative to the base):

| Request | apiName in the bundle | Notes |
|---|---|---|
| `v1/threads/<id>?view=msnp24Equivalent` | `thread` | the thread with its properties and a `members` array |
| `v1/threads/<id>/members?view=msnp24Equivalent&pageSize=<n>` | `thread_members` | the member list, paged; extra filters `selectMemberRoles`, `state`, `filterOnIsModerator`, `filterExplicitlyAdded`, `filterInterest`; the next page is a full URL the client re-bases |
| `v1/threads/<id>/filtered?view=msnp24Equivalent` | `filtered_thread` | a **filtered** view of the thread |
| `v1/threads?view=msnp24Equivalent&threadIds=<id>,<id>` | `threads` | several threads at once |
| `v1/threads/onetoone?view=msnp24Equivalent&userIds=<a>,<b>&threadType=<t>&isFederated=<b>` | `one_to_one_thread` | finds the 1:1 thread of two users |
| `v1/threads/<id>/members/<mri>` | `getMemberProperty` | one member |
| `v1/users/ME/conversations/<id>?view=msnp24Equivalent` | `myconversation` | the user's own view of a conversation (not a member list) |

Seen live: the client reads a chat's thread with the batch form, `GET v1/threads?view=msnp24Equivalent&threadIds=<id>`.
The answer is a JSON array with one thread object per id: `id`, `type` `Thread`, `properties` (`threadType` `chat`,
`fixedRoster`, `uniquerosterthread`, `creator`, `createdat`, `capabilities`, …), `members` (each with `id` (the
`8:orgid:` MRI), `type` `ThreadMember`, `role`, `userLink`, `capabilities`, `hidden`, `isFollowing`, …), `version`,
`rosterVersion`, `messages` (a link) and `eTag`. For the 1:1 chat it listed both members and carried no paging key and
no member count. `GET v1/threads/<id>/consumptionhorizons` gives the members' read horizons. The user's view of the
chat, `GET v1/users/ME/conversations/<id>`, answers 404 until the chat exists, and the client takes a 404 to mean the
chat is one the user isn't in.

### A group chat's reads

Seen live (M1 Task 8, 2026-10-20): the a–b–c group chat, created by a in the web client (`proto-lab run abc-chat`, its
id kept in the lab's `chats.toml`), read with each account's own `ic3` token (`proto-lab probe thread <alias>`; local
reference copies `reference/chat/thread-group-{a,b,c}.jsonl`; synthetic fixtures `tests/fixtures/chatsvc/thread-group.json`,
`thread-group-members.json`, `thread-group-filtered.json` written from them). A, b and c got the same answers:

- The id: `19:<32 lowercase hex>@thread.v2`.
- `GET v1/threads/<id>?view=msnp24Equivalent` (and the batch form, an array holding the same object): 200; `id`,
  `type` `Thread`, `members` (a, b and c, each `role` `Admin`, `hidden` false, with `capabilities`, `rangeStart`,
  `rangeEnd`, `meetingRole`, `userLink`, …), `properties` (`threadType` `chat`, `creator`, `createdat`,
  `capabilities`, `historydisclosed`, `productContext` `TEAMS`, …), `rosterVersion`, `version` and `eTag` (all three the
  creation time), `tenantId`, `messages` (a link), `fileReferences`, `encryptionType`, `copilotShareTokens`. No member
  count, no paging key, no `_metadata`.
- `GET v1/threads/<id>/members?view=msnp24Equivalent&pageSize=<n>`: 200; `members` (the same three, each with
  `interest`), `totalMemberCount` 3, `eTag` (the thread's), `rosterVersion` **0**, `isDeleted` false. `pageSize=1` got
  the same answer as `pageSize=100`: all members and no next link, so its paging form was never seen.
- `GET v1/threads/<id>/filtered?view=msnp24Equivalent`: **403**, error 209 `AclCheckFailed`: a chat's `Admin` lacks
  the `GetFilteredThread` capability.
- `GET v1/users/ME/conversations/<id>?view=msnp24Equivalent`: the account's own view, no member list:
  `memberProperties` (`role`, `interest`, `isReader`), `threadProperties` (`rosterVersion` as the thread's,
  `threadType` `chat`, `productThreadType` `Chat`, `isCreator`, …), `properties.isemptyconversation`.

## Reads by POST

The web client sends the following reads and subscriptions as POSTs. The tables describe wire shapes, independent
of the lab policy. `ngteams` uses the typed `teams-proto::PostRead` entries and checks each response against the
identities the request named; no permit or browser policy is involved.

| Request | Body |
|---|---|
| `POST https://<teams origin>/api/mt/part/<partition>/beta/users/fetchShortProfile?<the lookup query>` | a non-empty JSON list of MRIs |
| `POST https://<teams origin>/api/mt/part/<partition>/beta/users/fetch?<the lookup query>` | a non-empty JSON list of MRIs |
| `POST https://<teams origin>/ups/<geo>/v1/presence/getpresence/` (M2 Task 17) | a non-empty JSON list of `{"mri": …}`, each with at most `"source": "ups"` beside it, no MRI twice |
| `POST https://<teams origin>/ups/<geo>/v1/pubsub/subscriptions/<epid>` (2026-10-27) | exactly `trouterUri`, `shouldPurgePreviousSubscriptions`, `subscriptionsToAdd` and `subscriptionsToRemove`, each list of `{"mri": …, "source": "ups"}`: the account's own endpoint subscribed to others' presence ([trouter.md](trouter.md#the-subscription-and-its-deliveries)) |

The profile lookups, as 5869 captured ones sent them:

- `POST` on `https://teams.microsoft.com` or `https://teams.cloud.microsoft`, the partition a lowercase region, `-`
  and a number (`amer-03`);
- the query `isMailAddress=false`, `enableGuest=true`, `skypeTeamsInfo=true`, `canBeSmtpAddress=false`,
  `includeIBBarredUsers=true`, `includeDisabledAccounts=true` (one lookup by address had `isMailAddress=true` with
  addresses in the body);
- the headers `Accept`, `Cookie`, `Origin`, `Referer`, `User-Agent`, `authorization`, `content-type` and the client's
  `x-client-ui-language`, `x-ms-client-caller`, `x-ms-client-request-type`, `x-ms-client-type`,
  `x-ms-client-version`, `x-ms-migration`, `x-ms-object-id`, `x-ms-request-id`, `x-ms-session-id`, each with one
  value on every lookup where that matters: `Content-Type` `application/json;charset=UTF-8`, `x-ms-object-id` empty,
  `x-ms-migration` `True`, `x-ms-test-user` `False`, `x-ms-client-request-type` `0`, `x-ms-client-type` `cdlworker`;
  `x-ms-client-caller` names the client component that asked (dozens seen, `use-chat-hook`, `User.avatarUrl`);
- a body that is a non-empty list of MRIs.

The answers seen live name exactly the requested accounts: `{"type": …, "value": [one profile per requested MRI]}`,
each profile with its `mri`, `objectId`, `displayName`, `givenName`, `surname`, `email`, `userPrincipalName`,
`userType`, `tenantName`, `isShortProfile`, and for `users/fetch` also `alias`, `mail`, `smtpAddresses`,
`sipProxyAddress`, `phones`, `accountEnabled`, `skypeTeamsInfo`, `featureSettings` and such; no other account's id
or address.

### The presence read

UPS's `getpresence` (M2 Task 17), as the 5461 presence reads of the captures of 2026-10-19 to 2026-10-21 sent it:

- `POST` on either Teams origin, the path `/ups/<geo>/v1/presence/getpresence/` with its trailing slash, the geo
  lowercase letters (`noam`), and no query at all;
- the headers `Accept`, `Cookie`, `Origin`, `Referer`, `User-Agent`, `authorization`, `behavioroverride`,
  `content-type`, `x-ms-client-caller`, `x-ms-client-type`, `x-ms-client-user-agent`, `x-ms-client-version`,
  `x-ms-correlation-id`, `x-ms-object-id`, `x-ms-request-id`, `x-ms-session-id`, with `Content-Type`
  `application/json`, `behavioroverride` `redirectAs404`, `x-ms-client-type` `cdlworker`, `x-ms-client-user-agent`
  `Teams-V2-Web`, `x-ms-object-id` empty; on 2065 of them also `x-ms-endpoint-id`, the client's own endpoint;
- a body that is a non-empty list of objects, each `{"mri": <string>}` or `{"mri": <string>, "source": "ups"}` (the
  client's own MRI came with `source`), no MRI twice.

The first run that let it through (`ab-chat` of 2026-10-22) sent 14 of them, each naming only a, b or c; every answer
was 200 and named only the accounts asked for: a list of one item per MRI (`mri`, `presence` with `sourceNetwork`
(`Self` for the account's own, `SameEnterprise` for another), `calendarData` (`isOutOfOffice`), `capabilities`,
`availability`, `activity`, `lastActiveTime`, and `source` when asked with it), `etagMatch`, `etag`, and the item's
`status` `20000`) ([trouter.md](trouter.md#others-presence)). `PresenceRead` reads `availability`, `activity`,
`lastActiveTime` (as ms; `0001-01-01T00:00:00Z`, never active, as none) and `calendarData.isOutOfOffice`.

### The native client's reads by POST

`short_profile`, `presence` and `presence_subscription` are sealed `PostRead` entries in
`crates/teams-proto/src/post_read.rs`. `HttpTransport::post_read` verifies the method, service path, query, headers
and requested names against the typed entry and rejects responses naming another identity. The profile lookup uses
the account's middle-tier token (`api.spaces.skype.com`), while UPS uses
`https://presence.teams.microsoft.com/`. `users/fetch` is not sent; profile names come from Graph and
`short_profile`. Presence subscriptions use the current run's registered endpoint and connection push URL.

## Writes and capture holds

Chat writes are ordinary typed requests in `crates/teams-proto/src/write/chat.rs`, sent through the core outbox.
Messages, edits, deletes, reactions, typing, read receipts and chat/member operations retain the wire shapes in
[chat.md](chat.md); there is no target classifier or permission permit. The transport retains HTTPS-only,
no-proxy, no-redirect and service-specific credentials. Unknown message outcomes are checked instead of resent
blindly, and resending across runs requires `Store::resend`.

### New message and a new chat

Seen 2026-11-06 in browser Teams with lab account a (`proto-lab run team-manage` steps, the chat service's writes held
back, so nothing went out):

- **New message** (the list header's pen, Alt+Shift+N in the browser; Teams' desktop key is Ctrl+N) adds a "New
  message" row at the top of the chats and shows the conversation pane under a To: line ("Enter name, chat, channel,
  email or tag"), with "You're starting a new conversation / Type your first message below." over the compose box
  while no chat is chosen.
- **To:** suggests recent people with nothing typed; typed, the people whose names match, then the group chats whose
  name or people match (a named chat came up for one of its members' names). Every a–b–c group chat came up, those
  the list hides as empty (`isemptyconversation`: made by a call's Add participants, holding only the call) among
  them. Choosing a group chat opens it. Nothing is read from the service to find them.
- **The chat it is**: one person chosen shows their 1:1 chat's history under the line; b and c chosen showed the
  a–b–c group chat whose roster came last of five (made last, holding only a call), not the one with the latest
  message, and the first message went to it (`POST …/conversations/<that chat>/messages`). No read was made to find it.
  Once a named a–b–c chat had been made after it (by the native client, "Lab trio 39"), b and c still showed that
  unnamed chat: a named chat isn't the one New message finds for its people.
- **Add group name** (the chevron at the line's end, "Add group name and create a new group chat") puts a "Group
  name:" line over To:; with a name typed the pane shows a new conversation again, and the first message sends
  `POST v1/threads` with `{"members": [the account, then the people, each "role": "Admin"], "properties":
  {"threadType": "chat", "topic": "<the name>"}}` before the message to the chat it makes. A name with one person
  chosen sent nothing.
- **An opened empty chat** (one the list hides) is listed from then on at the list's foot, and stays there when
  another chat is opened.

### Adding people to a chat

Seen 2026-11-10 in browser Teams with lab account a (`proto-lab run team-manage` steps; every chat-service write on
`v1/threads` held back, so nothing went out; the people were lab accounts, plus the user's own account in held
requests only):

- **The header's people button** ("View and add participants"; a 1:1 chat's is an icon, a group chat's carries the
  member count) opens a flyout: "People" (a group chat: "People (<n>)"), each member with their picture and presence,
  the account first with "You" under its name; under a rule a 1:1 chat offers "Start a group chat" and "Add agents", a
  group chat "Add people", "Add agents" and "Leave". A group chat's member shows a remove × on hover.
- **The picker** is a small popover over the flyout: "Start a group chat" (a 1:1 chat) or "Add" (a group chat), a box
  "Enter a name, email, or tag" offering people and the group chats whose names match (none with nothing typed),
  each taken as a chip with a remove ×, then Cancel and Create (or Add), off until someone is taken. Nobody in the
  chat already is offered. A group chat taken stands for its members.
- **A 1:1 chat's Create sends nothing.** With an unnamed group chat of exactly those people (a, b and c had several)
  Teams opened the one whose roster came last, as New message does. Otherwise it opened a new group chat not made
  yet: its row at the top of the list named after the people ("<first>, <first>, and <first>"), its header their first
  names with the people count, "You're starting a new conversation / Type your first message below.", and a flyout
  with "Add people". The first message sent `POST v1/threads` with `{"members": [the account, the people added, the
  1:1 chat's partner], "properties": {"threadType": "chat"}}`, as [above](#new-message-and-a-new-chat). The 1:1 chat
  stayed as it was.
- **A group chat's Add** has three history choices under the box: "Don't include chat history" (the default),
  "Include history from the past number of days:" with a number box (1 to start; it takes digits only, and Add is off
  while it is empty or 0; 999 was taken), and "Include all chat history". Add sent `PUT v1/threads/<chat>/members`
  with `{"members": [{"id": <mri>, "role": "Admin", "shareHistoryTime": <n>}]}`: `-1` for none, `0` for all, and for
  the past days the time of the click less that many days in epoch milliseconds (31 days gave the time 31 days
  before). Held back, Teams said "Something went wrong. Please try again." and kept the chat as it was.
- **Remove and Leave**: the member's × asked "Remove <name> from the conversation? They'll still have access to the
  chat history." (Cancel, Remove) and sent `DELETE v1/threads/<chat>/members/<mri>` with no body; Leave asked "Leave
  this chat?" with a "Remove chat history" box (not sent). ngteams doesn't offer these yet
  ([backlog](../backlog/chat-people-remove-and-leave.md)).

ngteams follows this: `Action::AddMembers` (one outbox row, `AddChatMembers`; the history's start is worked out when
the user asks, so every attempt sends the same; an attempt whose outcome is unknown is failed, never sent twice), and
after it went through the chat's members are read again as a thread.

### Naming a group chat

Seen 2026-11-20 in browser Teams with lab account a (`proto-lab run team-manage` steps) in a group chat of a, b and c
alone; the renames were sent, and the chat's name was put back after:

- **Where**: a group chat's header shows a pencil after its name ("Name group chat"). A 1:1 chat and a meeting's chat
  have none, and neither the chat list's nor the header's "More chat options" menu, nor the chat's details panel,
  offers a rename.
- **The popup** under the name (400 by 150 px): "Group name" over a box that holds the name (empty for a chat without
  one: "Type a group name"), the keyboard in it, then Cancel and Save. Save is lit even while the box is empty.
- **The limit**: the box takes 250 characters. A paste or a key that would make it longer is refused whole, and "Your
  group name can't be longer than 250 characters." shows under the box until the next change it takes.
- **Save or Enter** closes the popup and sends `PUT v1/threads/<chat>/properties?name=topic` with `{"topic": <the
  name>}`, answered 200. The name is trimmed (`"  Lab trio 41  "` went as `"Lab trio 41"`); an empty box sends
  `{"topic": ""}`, which takes the name off: the header and the row are called by the people again.
- **What every member gets** (the account too): the thread's `ThreadUpdate` (its `properties` carry the new `topic`,
  or leave it out once the name is off), a `ThreadActivity/TopicUpdate` message from the thread
  (`<topicupdate><eventtime>…</eventtime><initiator><mri></initiator><value>Lab trio 40</value></topicupdate>`; an
  empty `<value />` for a name taken off; [chat.md](chat.md#system-messages)), and a `ConversationUpdate`. The chat
  keeps its place in the list. The line reads "AI changed the group name to Lab trio 40." or "AI removed the group
  name for this conversation."

ngteams follows this: the header's pencil and popup, `Action::RenameChat` (one outbox row, `SetChatTopic`). Once the
row went through, the account's own client calls the chat by the new name; another member's rename comes in with the
thread's `ThreadUpdate`, whose `properties.topic` the engine takes at once, with no read. A name that is the same as
before is not sent.

`proto-lab` records browser traffic without a browser fence. A scenario that needs a request's shape without its
side effect installs an explicit capture hold before navigation or the matching action. Held marker messages,
including legacy queued probes, stay held on later sessions so the browser cannot resend them then. Holds are
capture features, not participant policy. Lab participants are checked by the agent before each live run.

## Current call tracking

`teams-call::track` owns offer filtering, invitation deduplication, ended-ring handling and acceptance/media link
binding. `teams-core::CallFloor` shares the one-call-or-ring floor across accounts. Trouter hands calling deliveries
to this tracker and replies only through a typed response on that delivery's connection. A view cannot answer,
decline, join, or enable a camera by itself. See [calling.md](calling.md) and [trouter.md](trouter.md).
