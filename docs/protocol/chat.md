# The chat service: conversations, messages and what happens to them

Status: 2026-10-21, M1 Task 7; images (sending, denied) 2026-10-21, M1 Task 20; the shapes the UI needs (every quick
reaction, typing, lines and links, a group mention) 2026-10-21, M2 Task 10; an image's object created once and read
2026-10-22, M2 Task 21; files (sending, denied) and received attachments 2026-10-22, M2 Task 23; a mention of everyone
in a group chat (as sent) 2026-10-29. Written from what the
lab saw, in our own words, plus the web bundle where nothing was captured (marked so). The chat service's base,
credential, IDs, member reads and typed native write path are in [chat-service.md](chat-service.md); its token
in [auth.md](auth.md).

Dated restrictions and denials below describe the capture setup used then. The current native client has no write
fence; explicit browser holds retain shape-only experiments without changing what those old captures proved.

Sources (all outside the repository, under the lab's data dir; fixtures are written from them by hand, synthetic):

- **probe**: Task 3's read-only `proto-lab probe-chat` of accounts a and b (`reference/chat/probe-conversations-a.jsonl`,
  `…-b.jsonl`): the conversation list.
- **ab-chat-full**: `proto-lab run ab-chat-full` of 2026-10-20 (accounts a and b, the a–b 1:1 chat, captures
  `captures/ab-chat-full/20261020T194153Z` for a and `…194156Z` for b, reference copies
  `reference/chat/ab-chat-full-a.jsonl`, `…-b.jsonl`), and the confirming run recorded in the M1 plan's Task 7
  (`captures/ab-chat-full/20261020T205155Z` and `…205156Z`, read locally): a sent, edited, reacted and took the
  reaction back, replied with a quote, reacted to the reply, sent and deleted a message; b opened the chat (reading
  its history) and reacted.
- **mention**: `proto-lab run ab-chat-full` of 2026-10-21, once the fence let the mention picker's profile lookups of
  lab accounts through (chat-service.md, "Reads by POST"): a mentioned b, picked from the picker (captures
  `captures/ab-chat-full/20261020T223250Z` for a and `…223251Z` for b, reference copies
  `reference/chat/ab-chat-full-mention-a.jsonl`, `…-b.jsonl`).
- **ab-image**: `proto-lab run ab-image` of 2026-10-21 (M1 Task 20; accounts a and b, the a–b chat, captures
  `captures/ab-image/20261020T225251Z` and `20261021T010819Z` for a, read locally, nothing exported): a pasted a
  made-up image and sent it with every write to an AMS object denied ([Images](#images-sending-denied-ab-image)).
- **ab-image-object**: two `proto-lab run ab-image-object` runs of 2026-10-22 (M2 Task 21; as ab-image, with the
  object's creation mapped once for the a–b chat; captures `captures/ab-image-object/20261022T014327Z` for a (breached:
  cut by a system suspend, see [below](#the-object-created-once-ab-image-object)) and `…/20261022T020426Z` for a (clean),
  read locally), and `proto-lab probe object` of the first run's object as a, b and c and of the second's as a
  (reference copies `reference/ams/object-a-run1.jsonl` and `…/object-a-run2.jsonl`; b's and c's reads printed only):
  the creation's answer and the object's read ([The object created once](#the-object-created-once-ab-image-object)).
- **chat-shapes**: three `proto-lab run chat-shapes` runs of 2026-10-21 (M2 Task 10; accounts a and b under the
  messaging classifier, so b's push channel was up; captures `captures/chat-shapes/20261021T165600Z`, `…170305Z` and
  `…171041Z` for a, `…165601Z`, `…170308Z` and `…171041Z-2` for b, read locally, nothing exported): a set and took back
  each quick reaction, typed while b's open chat recorded its typing indicator, sent a message of three lines with a
  link, and mentioned b in the lab's group chat ([The shapes the UI needs](#the-shapes-the-ui-needs-chat-shapes)).
- **abc-everyone**: `proto-lab run abc-everyone` of 2026-10-29 (accounts a and b, the lab's group chat; capture
  `captures/abc-everyone/20261029T115612Z` for a, read locally, nothing exported): a mentioned everyone, picked from
  the mention picker, its post held back by a forced deny and read as the browser held it ([A mention of
  everyone](#a-mention-of-everyone-abc-everyone)). The run was **breached** (a shared worker attached while the browser
  went down at the end, held and never resumed, its close lost with the connection), and its real post was denied (the
  live member read timed out under load). Three later runs on a loaded machine never reached the mention: one whose
  page didn't send the probe, and two **breached** (an iframe whose resume went unanswered, the browser killed; the
  fence denied and let through nothing in either). No read-back of a mention of everyone was captured.
- **ab-file**: `proto-lab run ab-file` of 2026-10-22 (M2 Task 23; accounts a and b, the a–b chat, captures
  `captures/ab-file/20261022T013754Z`, `…015634Z`, `…020205Z` and `…021606Z` for a, read locally, nothing exported):
  a tried to send a made-up text file with every write of it denied ([Files](#files-sending-denied-ab-file)).
- **bundle**: the web client `26260.1717.5229.7049`, for what no run showed.

Every path below is relative to the chat service base (`…/api/chatsvc/<region>/`), and every id in a path is
percent-encoded as one segment.

## The conversation list (probe)

`GET v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1&targetType=All` answers 200 with
`{"conversations": [...], "_metadata": {...}}`. The web client never sent it: it reads its chat list from CSA
(`chatsAndChannelsByIds`, a POST whose answer was not captured in those early runs). Fixture: `tests/fixtures/chatsvc/conversations.json`.

- `_metadata`: `totalCount` (5 for a lab account), `forwardLink` (empty: one page held everything) and `syncState`, a
  full URL for the next incremental read: `v1/users/<the account's MRI>/conversations?view=msnp24Equivalent&syncState=
  <opaque>&pageSize=200`. The path names the account's MRI, not `ME`. A non-empty `forwardLink` (the next page) was
  never seen; the client follows it as a full URL.
- Each entry: `id`, `type` `Conversation`, `version` (ms), `targetLink` (the thread, `v1/threads/<id>`), `messages`
  (the message list's URL), `lastUpdatedMessageId` and `lastUpdatedMessageVersion` (numbers; 0 for an empty chat),
  `lastMessage` (a message as below, without `properties`; `{}` for an empty chat), `memberProperties` (the account's
  own `role`, `interest`, `isReader`, `memberExpirationTime`, `relationshipState`), `properties` (`addedBy`,
  `addedByTenantId`, `consumptionhorizon` when the account has read anything, `isemptyconversation` as the strings
  `True`/`False`) and `threadProperties`.
- What kind of conversation it is comes from `threadProperties`: `threadType` `chat` with `productThreadType`
  `OneToOneChat` (a 1:1 chat, `uniquerosterthread` `True`), `threadType` `space` with `TeamsTeam` (a team; its
  channels in `topics`, a JSON string), `streamofnotifications` (`48:notifications`). The rest: `createdat`,
  `isCreator`, `lastjoinat`, `lastSequenceId`, `rosterVersion` (a number), `version` (a string),
  `originalThreadId`, `gapDetectionEnabled`. Booleans in `properties` and `threadProperties` are mostly strings.
  A group chat in the list wasn't seen (the lab had none when the probe ran). A named group chat's or a meeting
  chat's name is `threadProperties.topic` (a meeting's chat has `threadType` `meeting` and is titled by the meeting);
  the web bundle titles a chat by it, and the thread read carries it as `properties.topic`. Read from the web bundle
  (the `meeting` thread type from the personal build's store), not from a lab capture.

### Threads in the list the account can't read

The full list (`startTime=1`) still names threads the account can no longer read. A personal build's debug log of
2026-11-04 showed 15 of them on one account, team channel threads (`@thread.tacv2`) and a meeting chat among them:

- **Left or removed**: reading its messages answers **403**, the service's code `AclCheckFailed` (the account isn't on
  the thread's roster).
- **Deleted for good**: **404**, the code `ThreadNotFound` (the thread was hard-deleted). An ordinary 404 for a
  conversation the service doesn't know names `RequestHandlerException` instead, and the account's own notes
  (`48:notes`) answer 404 until a first note is written.

An error answer is JSON. Its top-level `errorCode` is a number, either the status or the service's own number (209 for
`AclCheckFailed`). The code as a word is in `standardizedError.errorSubCodeString`, and the `message` may hold a second
answer as a JSON string with a `subErrorCode`. `teams_proto::chat::ChatReadError::service_code` reads the word from
either place, and from a plain `message` that starts with `<code>:`.

Neither kind gets better on a second read. The native catch-up (`teams-core` `sync`, `Unreadable`) therefore doesn't
keep such a conversation to be synced again. If the account is added back, the incremental list brings the
conversation at a newer version and it syncs as any other. A `ThreadNotFound` thread is also removed from the store
(`Store::forget_conversation`), but notes never are. The full list keeps naming a deleted thread, so the store keeps
its version and last activity after forgetting it, and a later listing adds it back and reads it again only at a newer
version or a later last message. Each such thread gets one `info` line naming the status and the
code, and the catch-up summary counts them as "skipped (not a member or deleted)". The error bodies seen are in our
own words here; none was captured from the web client, which never reads this list.

## Favorites, empty chats and the chat list

What Teams v2's list shows above and in its "Chats" section, as the native client reads it (the lab's captures of
2026-10-24 and a personal build's debug log and store of 2026-10-26/27):

- **Favorites** live in CSA's teams read (`GET <CSA base>/api/v3/teams/users/me`, the read the Teams list already
  makes; [channels.md](channels.md#csas-teams-read)), in its top-level `conversationFolders`: `isPartialResponse`
  (`false` in every answer), `folderHierarchyVersion`, `conversationFolderOrder` (the folders' ids) and
  `conversationFolders`, seven folders whose `folderType` is `Favorites`, `RecentChats`, `TeamsAndChannels`,
  `QuickViews`, `MutedChats`, `MeetingChats` or `EngageCommunities`, each with `id` (`<tenant>~<oid>~<type>`),
  `sortType` (`UserDefinedCustomOrder` for Favorites and QuickViews, `MostRecent`, `NameAlphabetical`), `name`,
  `isExpanded`, `isDeleted`, `version`, `createdTime` and `conversationFolderItems`. A conversation item is
  `conversationId`, `createdTime`, `lastUpdatedTime` and `threadType`; QuickViews' items are slices
  (`slice-saved`, …, `itemType` `Slice`). The Favorites folder lists the chats in the user's order; its first item
  was `48:notes` (`threadType` `streamofnotes`) in the lab's accounts and one personal account: the account's own
  notes, its chat with itself, which Teams titles "<name> (You)" (the web bundle's `chat_title_self_chat_name`,
  `{{name}} (You)`) with the account's own picture and presence. Another personal account's folder held two team
  channels and no notes (items with `threadType` `topic`, ids `19:…@thread.tacv2`, channels of the answer's `teams`
  and in none of its `chats`): Teams lists a favourite channel there by its name, with its team's picture. The native
  client shows one by the channel's name with its team's initial (no team picture is read), opened as the Teams list
  opens it, as a channel; a favourite neither stored as a chat nor a channel of the stored teams is left out. `MutedChats` and `MeetingChats` were empty in every answer: the folders don't say which
  chats are muted. The middle tier's user properties (`favorites`, `{"48:notes":1}`, and `selfChatSettings`) carry an
  older form of the same; the native client reads only the folder. Pinning, unpinning and reordering are CSA writes
  no capture shows; the native client sends none. Fixture: `tests/fixtures/csa/folders.json`.
- **Muted chats**: CSA's `chats` (the same answer) carry `isMuted` and `alerts` only on some meeting chats
  (`false`/`true`); a chat Teams draws muted had neither, and neither had any chat that isn't a meeting's. No
  readable field says a chat is muted, so the native client shows none as muted.
- **Empty chats**: the conversation list's `properties.isemptyconversation` (`"True"`/`"False"`) is `True` for a chat
  nobody wrote in: the 1:1 chat a test call creates with the echo service (its call events don't count), and a
  meeting's chat nobody wrote in. Teams lists neither; CSA's `chats[].isEmptyConversation` agrees. Its `hidden` is no
  guide: it was `true` on 1:1 chats Teams lists. A group chat a call's Add participants made, holding only the call
  and its member events, is empty too, and browser Teams didn't list the four such a–b–c chats either (2026-11-06);
  its search box's suggestions and New message's To: found them all the same, and one opened from there was listed at
  the list's foot from then on ([chat-service.md](chat-service.md#new-message-and-a-new-chat)).
- **Meeting chats** (`threadType` `meeting`, ids `19:meeting_<base64>@thread.v2`) each belong to one meeting: two
  with the same topic are two meetings (a recurring series' occurrences, or two meetings named alike), and Teams lists
  each with its calendar avatar.

## The chat list's order and its See more

Read from the web bundle `26260.1717.5229.7049` (its left rail, its resolver of the rail's folders and its production
config defaults), the lab's service config and a personal account's CSA answer; compared with a screenshot of the user's
own Teams, whose 20 rows are the personal store's 20 latest chats in the same order.

- **The section is a folder of `conversationFolders`, `folderType` `RecentChats`**, `sortType` `MostRecent` in every
  answer seen. The service config's `simpleCollab.conversationFolderSortTypes.recentChatsFolder.sortTypes` offers
  `MostRecent` alone in the lab's tenant, and `MostRecent` and `UnreadOnly` by the bundle's default, so unread chats
  aren't sorted first in either: the order is the last message's arrival, latest first, a chat that has none counted by
  when it was joined or made. The bundle also has `UnreadThenMostRecent` (unread, then mentions, then time), which no
  folder of `RecentChats` was seen with.
- **A chat's unread message puts it on top only by being new.** An older unread chat stays in its place. Past the
  rows shown it shows on the See more row, from the folder's `activityStatusOnRemainingPages`: `hasUnread` draws it as an
  unread item is drawn (bold), and mentions, drafts and a badge count have fields of their own.
- **The rows shown**: `recentChatsInitialFetchCountOverrideWithMeetingChatSectionEnabled` (20 by default) while the
  meeting or muted chats' section is on or unset, else `recentChatsInitialFetchCountOverride` (25). See more shows
  `recentChatsSeeMoreFetchCountOverride` more (25), is listed while the folder's `pageInfo.hasNextPage` holds, and has no
  counterpart: `chatList_see_less_flyout_label` belongs to the flyout on the chat button. Its label is the shared
  `seeMoreButtonLabel` in the brand colour, with a chevron shown only on hover.
- **A new message** sorts the chat's edge into the rail's loaded page: the rows already loaded stay, so the page grows
  by one (read from the rail's connection updates; not watched on a list long enough to show it).
- **A chat opened outside the rows shown** is added to the loaded page (the query's `selectedIdHint`, the rail's
  "hoisting", `enableSimpleCollabRailSelectionHoisting`), which sorts it after them, and stays there until a refetch.

## Which chats are unread

Read from the web bundle `26260.1717.5229.7049` (its read-state helpers, the chat badge's publisher and the rail's
folder activity), and checked against a personal account's stores and CSA answer of 2026-11-02, whose real rail shows
no chat bold and whose See more isn't bold. Teams keeps one yes or no per chat, from the chat's last message; the native
client counts messages, and calls a chat unread when the count isn't 0, by the same rules:

- **The last message and the horizon.** A chat is read when its last message arrived no later than the horizon. CSA's
  `lastMessage` isn't the newest thing in the chat: in 54 of the 265 chats it was an older message than the newest stored
  row, an `Event/Call` or a member or topic event. All of them were `isRead`, some with that row past their horizon, so a
  call or a thread event makes no chat unread. The native client counts `Text` and `RichText…`.
- **A meeting's chat** (its id holds `meeting`) isn't unread for what the meeting left in it: the bundle's list is
  `ThreadActivity/AddMember`, `Event/Call`, `RichText/Media_CallRecording`, `RichText/Media_LocalRecording` and
  `RichText/Media_CallTranscript`, and thread events beside them. The chat counts as read up to its last content message
  (`lastContentMessageTime`, which the client works out; no answer carries it). The personal account's 8 meeting chats
  that the native client called unread held only recordings and transcripts past their horizon, one of them from 2022. In
  any other chat a recording counts as a message.
- **Your own last message** makes a chat read, whatever its horizon says: CSA's `isRead` is true on a meeting's chat
  with `isLastMessageFromMe` and no `consumptionHorizon` at all.
- **Mark all as read** is account-wide: the account's own property `appConsumptionHorizon`, `{"chat", "channel",
  "engage_community"}` in ms, as an object or as its JSON text, read with the others (`GET v1/users/ME/properties`, 404
  for an account that has none) and delivered on change (`CustomUserProperties`, the changed properties only). What
  arrived up to the time for the chat's kind is read, unless a chat with no horizon of its own was joined after it. The
  form is the bundle's: no capture shows one, since no lab account marked all read and the personal build hadn't read
  the property. The native client doesn't apply the "joined after it" exception: such a chat's messages from before
  that time count as read, and any newer one still counts.
- **Mark as unread** is the conversation's `consumptionHorizonBookmark`: while its arrival isn't 0, the chat is unread
  from the message after that arrival on, whatever its horizon and its newest message say ([Mark as
  unread](#mark-as-unread)). The 8 chats that carry one in the CSA answer above all have arrival 0, so none is marked.
- **Notifications** don't follow from any of this: only `Text`, `RichText/Html` and `RichText/Media_Card` notify.

## Mark as unread

Seen in browser Teams (`proto-lab run team-manage`, lab account a, 2026-11-19) in a's 1:1 chat with lab account b and
in a lab team's General channel. Each write is the account's own property: nobody else is told.

- **A message's "More options"** has "Mark as unread" (glasses with a slash, key U) after a hairline under Forward, Copy
  link, Save this message, Delete (the account's own) and Pin, before Share to Outlook and Translation. It is on the
  account's own messages too, and on a channel's post.
- **The write**: `PUT v1/users/ME/conversations/<conversation>/properties?name=consumptionHorizonBookmark` with
  `{"consumptionHorizonBookmark": "<message id - 1>;<now>;<clientmessageid>"}`, answered 200. The arrival is the
  marked message's id minus 1 and the `clientmessageid` is the marked message's own. On a channel's post, the message is
  the newest of the post's thread (its newest reply), not the post. The read horizon (`consumptionhorizon`) doesn't
  move.
- **At once**: the chat list's row is bold with its dot, the rail's count of unread chats goes up by one, and a "Last
  read" line stands before the marked message, its sender's name and time shown again over it. A channel's post gets
  the "Last read" line over it and "New" at its header's right end. The chat stays open and stays unread: a click in it
  sends nothing.
- **The chat list's row** has "More chat options" ("..." on hover). Its menu starts with "Mark as unread" (glasses with
  a slash) on a read chat and "Mark as read" (glasses) on an unread one, then Move to section, Mute, Notify when
  available, Manage apps, Set workflows, Copy link, Remove chat history and Hide. "Mark as unread" there puts the
  bookmark before the chat's newest message, the account's own too, and the chat is unread with it. "Mark as read" puts
  the horizon at the newest message (`consumptionhorizon`), then the bookmark `0;<now>;0`.
- **Opened again** (another pane, then the chat again): the web client put `consumptionHorizonBookmark` `0;<now>;0` and,
  with the horizon at the newest message already, no `consumptionhorizon`. The line stayed where the mark was.
- The native client keeps the bookmark's arrival in the store and counts a marked chat's messages from others after it
  (at least 1) unless it marked all read later. It ends the mark when the chat is shown after it opened marked, or shown
  again after it was marked there, and sends no read receipt while the chat stays shown after a mark made there. A
  mark (or its end) that gets no answer (the network down, a 5xx or a throttle, also of the token service) stays queued and goes out once the service answers:
  set again, the bookmark does no harm. A newer mark of the chat replaces an older one still queued, so only the newest
  goes out.
- Not seen: the push delivery of a bookmark to the account's other endpoints (the own server sends a
  `ConversationUpdate` naming `consumptionHorizonBookmark`; the native client stores a bookmark from any
  `ConversationUpdate`), and a channel row's or a thread's own mark.

## The account's own notes

`48:notes` (`threadType` `streamofnotes`), the account's chat with itself, which Teams lists first under Favorites
(above), is in no conversation list: neither the lab's captures nor a personal build's store of 2026-10-27 held it
there, while the other `48:` streams (`48:notifications`, `48:mentions`, `48:calllogs`) were listed and synced. The web
bundle builds every conversation's messages path the same way (`v1/users/ME/conversations/<id>/messages`) and names
the notes by their fixed id and thread type, so the native client reads them as any chat's messages, by `GET`
(newest page, then its sync link), from the catch-up when the notes are a favourite or already stored, and when the
chat view opens them. Not captured: no lab account and no personal run opened its notes, so the messages read on
`48:notes` is unverified live. What was captured: the conversation's properties read (`GET
v1/users/ME/conversations/48:notes?view=msnp24Equivalent`, made by the web client on load) answered 404 with
`errorCode` 404, "Conversation was not found" (`RequestHandlerException`) for lab accounts that never wrote a note, so
an account's notes exist only once something was written there; the native client takes a 404 on them as no notes.
Notes aren't a thread: no thread or member read is made for them. Each note's sender is the account itself. Whether
Trouter delivers a note written on another device wasn't seen; a delivery naming `48:notes` would be stored like any
chat's. Writing is off: no write type is mapped to a `48:` stream, and the chat view shows the notes read-only.

## Message pages (ab-chat-full, b)

Opening the chat, b's client made two reads. Fixture: `tests/fixtures/chatsvc/messages.json`.

1. **Sync**: `GET v1/users/ME/conversations/<chat>/messages?view=msnp24Equivalent|supportsMessageProperties&
   pageSize=200&syncState=<opaque>`, from a sync state the client had kept from an earlier session. It answered with
   the messages changed since, newest first, and `_metadata` `{lastCompleteSegmentStartTime, lastCompleteSegmentEndTime,
   syncState}`, where `syncState` is the full URL of the next sync read (`…/messages?startTime=<ms>&syncState=
   <opaque>&pageSize=200&view=msnp24Equivalent`). The top level also has `tenantId`.
2. **Around a time**: `GET v1/users/ME/conversations/<chat>/messages/epochTimeStamp/<ms>?pageSize=40&direction=
   BIDIRECTIONAL&includeMetadata=true&locationType=Primary`, the time being the thread's `lastjoinat`. It answered with
   every message of the (small) chat, newest first, `_metadata` with both segment times 0 and no link.

Further pages: the bundle pages a message list through `_metadata.backwardLink` (older) and `forwardLink` (newer), full
URLs. A `backwardLink` was seen since on sync answers longer than a page ([below](#sync-answers)); a `forwardLink` on a
message page never was. A first read without a kept sync state wasn't seen either.

## Sync answers

Read from the lab's web client captures (`ab-chat`, 2026-10-21: a sync answer longer than a page in the lab chat) and
from a personal build's store and debug log (2026-10-28: an account with about 250 conversations, a busy team channel
and a busy meeting chat), locally; no value from either is copied here.

- **What a sync read answers.** `GET …/messages?…&syncState=<opaque>` brings what changed since the end of the segment
  the sync state was handed out with: every message whose `version` is newer than it, newest first by id. A message
  that arrived earlier and was edited, deleted or reacted to since is in it too, at its own id's place (so at the end
  of the answer). Nothing changed: no messages, the same end.
- **The segment times**, on every page: `lastCompleteSegmentEndTime` is the newest version the conversation is
  complete to (the newest version on the answer, or the old end when nothing changed); `lastCompleteSegmentStartTime`
  is how far back the complete segment reaches: the old start when the answer fits in a page, the oldest message's id
  on the page while a longer answer isn't read through. The opaque `syncState` (and a `backwardLink`'s) moves with
  them; the native client never reads into it.
- **An answer longer than a page** has a `backwardLink` on its first page. In the lab chat, whose segment started at
  the beginning (its whole history was read), following it brought the rest of what changed, down to the old end, and
  that last page reported the merged segment (the old start, the new end) and no `backwardLink`: the service closed
  the gap itself.
- **A segment that doesn't start at the beginning** (a first read of a busy conversation: one page, more history
  behind it) is answered otherwise. A cut answer's `backwardLink` doesn't stop at the old end: each page's
  `lastCompleteSegmentStartTime` is its own oldest message's id, the end stays, and the link pages back through the
  conversation's whole history. For the busy channel and meeting chat it reached two and a half years back; a
  channel's page of 200 posts was about 0.8 MB and took 2 to 25 s, and one over 30 s timed out.
- **So the gap is bounded by the old end.** Every message that arrived after the end of the segment the sync state came
  with has a later id, and a page holds its messages newest first by id: once a page's `lastCompleteSegmentStartTime`
  (or its oldest message's id) is at or before the old end, every message that arrived since is on it or on a page
  before it, and what its `backwardLink` leads to is history the client had, or older. The native client keeps each
  sync state's `lastCompleteSegmentEndTime` beside it and stops a sync answer's walk at that page; older history is
  read only from the first read's own `backwardLink`, as the user scrolls. An edit or reaction of a message older than
  the old end, on a page past that one, isn't read by the sync (a Trouter delivery brings it while the socket is up).
- **The account's drafts** (`48:drafts`) are in the conversation list, but `GET …/conversations/48:drafts/messages`
  answers 400 with `errorCode` 201, "Invalid threadId, cannot be 48:drafts". The native client keeps the list's entry
  and never reads its messages.

## A message as read

Every message (`type` `Message`) has `id` (its arrival time in ms, as a string), `sequenceId` (1, 2, … in the chat),
`version` (ms, a string), `conversationid`, `conversationLink` (a URL), `messagetype`, `content`, `from` (a URL ending
`…/contacts/<MRI>`: take the sender's MRI from its last segment), `composetime` and `originalarrivaltime` (ISO times
with seven fraction digits). A user's message adds `clientmessageid` (the sender's, kept), `contenttype` `Text`,
`imdisplayname` (the name the sender's client gave: "Unknown User" in the lab's runs of 2026-10-20, when the fence denied
every profile lookup; the account's display name since the lookups of lab accounts pass),
`fromDisplayNameInToken` (the name from the sender's token), `amsreferences` (`[]`) and `properties`:
`mentions`, `cards`, `links` and `files` as JSON strings (`"[]"`), `formatVariant` `TEAMS`, `languageStamp`.

- **The chat's first event**: `messagetype` `ThreadActivity/AddMember`, `from` the thread's own contact URL, no
  `clientmessageid`, and an XML `content`: `<addmember>` with `eventtime`, `initiator`, `rosterVersion` and per member a
  `target` and `detailedtargetinfo`.
- **Edited** (fixture `edit.json`): same `id` and `clientmessageid`, the new `content`, `version` raised and
  `properties.edittime` (ms, set by the service: it differs from the one the edit sent). No `skypeeditedid` on an
  edited message. The edit was a `PUT v1/users/ME/conversations/<chat>/messages/<id>` of the whole message again (the
  post's fields with `id` the message's, `from` `worker/<MRI>`, `conversationLink` `conversation/<chat>;messageid=<id>`,
  `version` the edit's ISO time and `properties.edittime`), answered 200 with an empty body.
  The native text-edit path preserves only the original raw `properties.files` and `properties.cards` unchanged,
  including unknown inner fields and JSON representation. It rebuilds mentions and other text-derived properties
  from the edited content. The durable edit snapshot survives retries and manual resend. A legacy queued edit
  without one loads a safe original once before its attempt; an unavailable or unsafe original fails locally
  without HTTP. This cannot recover file metadata already erased by an earlier successful edit.
- **Deleted** (fixture `delete.json`): `DELETE v1/users/ME/conversations/<chat>/messages/<id>?behavior=softDelete`, no
  body, answered 200 with the text `null`. Read back: same `id` and `clientmessageid`, `content` empty, `properties`
  only `{deletetime}`, `version` the delete's time, no `amsreferences`, and **`skypeeditedid` set to the message's
  `clientmessageid`**: the one place that key showed up.
- **Reactions** (fixture `reaction.json`): `PUT v1/users/ME/conversations/<chat>/messages/<id>/properties?name=emotions`
  with `{"emotions": {"key": "like", "value": <ms>}}`, answered 200 with the text `200`; taken back with `DELETE` on the
  same URL and `{"emotions": {"key": "like"}}`, answered 200 with the text `null`. Read back, `properties.emotions` is
  a list of `{key, users}`, one per reaction key; a reaction taken back leaves its key with `users: []`. A reaction
  change raises the message's `version`, and a message that had one carries `annotationsSummary`: `{}` once none is
  left, `{"emotions": {"like": 1}}` (a count per key) while one stands. A standing reaction lists each user as
  `{"mri", "time", "value"}`: `time` the service's (ms), `value` the one the client sent, as a string.
- **Quoted reply** (fixture `reply.json`): an ordinary post whose `content` starts with
  `<blockquote itemscope itemtype="http://schema.skype.com/Reply" itemid="<quoted id>">`, holding
  `<strong itemprop="mri" itemid="<sender MRI>">name</strong>`, `<span itemprop="time" itemid="<quoted id>">` and
  `<p itemprop="preview">` with the quoted text, followed by the reply's own paragraphs. `properties.qtdMsgs` names the
  quoted message: sent as a JSON string `[{"messageId": "<id>", "sender": "<MRI>", "time": <ms>}]`, read back as a list
  of objects (`messageId` a number, plus `message` null, `validationResult` `Valid`, `sharedRefId`, `replyChainId`) and
  `hasValidMsgReferences` true. A 1:1 chat has no reply chains: a quote is all a "reply" is.
- **Mentions** (fixture `mention.json`): [below](#mentions-mention).
- **Received attachments**: not seen in the lab; every captured message had `amsreferences` `[]` and `files` and
  `cards` `"[]"`. A received file's shape was read since from a message of the user's own account
  ([Files: received](#files-received)): an entry in that shape is drawn as a file card (`Message::files`,
  `teams_proto::chat::SharedFiles`). Any other file entry, a card and an AMS object are kept as they came
  (`teams_proto::chat::Unshown`), and the native client shows the message's text with a line saying it has content
  that can't be shown here and a button that opens the chat in the web client (`l/chat/<chat>/0`, the id encoded as
  `abc-chat` encodes it), never a guess at it (M2 Task 23). A message
  of a type the chat view doesn't show gets the same button under its placeholder. In a channel the button opens the
  message's own link, naming its post (`l/message/<channel>/<id>?parentMessageId=<post>`, the link a workflow run on
  it carries: `teams_proto::MessageRun::link_to`).

## System messages

Seen in the lab's call runs (`ab-call`, `abc-call`: `Event/Call`) and in the personal build's stores, read locally
(the rest); no value from either is copied here. Which words the web client draws for each was read from its bundle
(its message renderer and its chat list's preview), and the native client follows it (`teams_proto::chat::chat_event`
reads them, `teams_ui::system_message` words them). A kind or a shape not listed here keeps the placeholder.

- **Who sends them.** A thread's own events (`ThreadActivity/…`) come `from` the thread's contact URL, so the sender
  read from `from` is the thread's id (`19:…@thread.v2`, a meeting chat's `19:meeting_…@thread.v2`). That is no
  person: the chat view draws them centred with no author row, and never names the thread as a sender. A call's events
  and a recording's come from a person (the call's caller, the meeting's organizer), and are drawn the same way.
- **A meeting chat** is told by its id, as the web client tells it: the id contains `meeting_`.
- **`Event/Call`**: XML. A start is a `<partlist alt ="">` (empty) followed, in later shapes, by `<meetingDetails>`
  (organizer, meeting type, scheduled times), `<callEventType>callStarted</callEventType>`, the conversation's start
  time and URL and the call id; the oldest shape is the part list alone. An end begins `<ended/>`, then
  `<partlist alt="" count="n">` with a `<part identity>` per participant (`<name>` the MRI, `<displayName>`,
  `<duration>` in whole seconds), then the same details with `callEnded`. A group chat's meeting (its "huddle")
  carries `<conversationType>"huddleGroupCall"</conversationType>`, and its start names the starter in a
  `<partlist type="started">`. The web client takes the length from the last participant's `<duration>` and draws
  nothing for an end without one (an end with `count="0"` was seen). In a meeting chat it shows "Meeting started" and
  "Meeting ended: <length>" (the length in bold), each a line after its icon and time, the end followed by the
  meeting's recap card ([recap.md](recap.md#what-the-web-client-shows)); a start followed at once by its own end is
  left out (a lab capture of 2026-11-05: the meeting transcribed only showed its end alone). In a 1:1 or
  group chat "Call started", "Call ended <length>", or "Call missed" for someone else's call with fewer than two
  participants; a huddle "<name> started a meeting." and "Meeting ended <length>". A length reads `1h 2m 3s`, its
  zero parts left out.
- **`RichText/Media_CallRecording`**: a `<URIObject type="Video.2/CallRecording.1">` with
  `<RecordingStatus status code>`, `<Title>` (the meeting's), `<a href>` (the player's link, empty until ready),
  `<OriginalName v>` (the file's name), `<MeetingOrganizerId value>` and `<MeetingOrganizerTenantId value>`,
  `<RecordingInitiatorId value isAutoRecording>`, `<RecordingTerminatorId value>` (once someone stopped it),
  `<Identifiers>` (`<Id type="callId" value>`: the call its meeting's end names), and
  `<RecordingContent contentTypes timestamp duration canVideoExpire>` (`Recording`, `Transcript` or
  `Recording+Transcript`; none in a channel's; `timestamp` when it began, UTC with 7 decimals; the length a .NET time
  span, `h:mm:ss.fffffff`), which once ready holds `<item type="onedriveForBusinessVideo" uri driveId driveItemId>`
  (the file in the organizer's OneDrive) and `<item type="onedriveForBusinessTranscript" uri id driveId
  driveItemId>`. Three states were seen, each a message of its own: `Initial`/`0` (started: "<name> started
  recording." or "… started recording and transcription"), `ChunkFinished`/`202` (stopped: "<name> stopped
  recording." with a terminator, else "Recording stopped.", or "Recording and transcription stopped") and
  `Success`/`200` (ready: in a meeting chat "Recording has been saved to <organizer>'s OneDrive and will expire in
  <n> days." with "Remove expiration" for the organizer, the expiry read from the file, [recap.md](recap.md); in a
  1:1 or group chat the recording's card, its name and length, opening Teams on the web). A meeting chat names
  everyone by name, the account too (the lab account's own name before "started recording.", and the user's own name in
  their Teams). The `Transcript` half of a recording is drawn as nothing, as the web client does. Another state or
  contents is no event.
- **`RichText/Media_CallTranscript`**: JSON with every quote escaped (`{\"callId\":…}`): `callId`, `scopeId`,
  `iCalUid`, `meetingOrganizerId`, `isDeleted` (always `false` here), `isExportedToOdsp`. In a meeting chat it draws
  nothing of its own: the meeting's recap card holds its Transcript chip; elsewhere the transcript's card
  ("Transcript is available"). Read in the meeting's Recap tab ([recap.md](recap.md)).
- **`ThreadActivity/MemberJoined` and `MemberLeft`**: JSON `{eventtime, initiator, members: [{id, friendlyname,
  meetingMemberType?}]}`; `meetingMemberType` was only ever `temp` (joined for the meeting). A meeting chat: "<names>
  was/were invited to the meeting.", "<names> left the chat."; a temporary member "has temporarily joined the chat.",
  "no longer has access to the chat."; another chat "joined the conversation.". Names: one, "A and B", "A and n
  others"; a member's `friendlyname` names them when no profile does (a visitor's). Teams draws its unverified mark
  after the name of someone who isn't verified (the user's Teams showed it for a note-taking bot invited to a
  meeting); the native client draws it for a Teams visitor (`8:teamsvisitor:`), the one such member a store showed.
- **`ThreadActivity/TopicUpdate`** (`<topicupdate>` with `initiator` and `value`): "<name> named the meeting <value>."
  in a meeting chat, "<name> changed the group name to <value>." elsewhere, "<name> removed the group name for this
  conversation." without one (an empty `<value />`, as a name taken off sent it on 2026-11-20). A group chat's rename
  ([chat-service.md](chat-service.md#naming-a-group-chat)) also brings the thread's `ThreadUpdate`, whose
  `properties.topic` is the new name, left out once the name is off.
- **`ThreadActivity/DeleteMember`** (`<deletemember>` with `initiator` and `target`s): the initiator alone "left the
  chat.", else "<name> removed <names> from the chat.", or "<names> was/were removed from the chat." when the thread
  removed them.
- **`ThreadActivity/AddMember`** ([above](#a-message-as-read)), also with `<sharehistorytime>` per target (`0`: all of
  it): "<name> added <names> to the chat" and "… and shared all chat history." for `0` (or "from the past n days"
  for a share time at least a day before a known event time; no clause when the event doesn't say it plainly); an
  initiator adding only themself "joined the conversation."; a lone bot added, nothing; one naming no initiator keeps
  the placeholder.
- **`ThreadActivity/MeetingPolicyUpdated`** (`value` `ChatEnabled` seen): "Chat has been turned on for this meeting."
  and the web client's other values' lines.
- **Seen, not drawn**: `ThreadActivity/RoleUpdate`, `PinnedItemsUpdate`, `FileAdd`, `ModalityUpdate`, the app and
  channel events of a team (`AddCustomApp`, `SpaceThreadChannelAdded`, `TopicTopicUpdated`, …), `RichText/Media_Card`
  (an adaptive card), `RichText/Media_CallLogRecording` and `Media_CallLogTranscript` (the account's call log stream).
  They keep the placeholder (a thread's centred, without an author). In a channel no thread event is drawn at all
  ([channels.md](channels.md#the-channels-own-events)).
- **A channel renamed** (`ThreadActivity/TopicTopicUpdated`) and **its description changed**
  (`TopicDescriptionUpdated`): JSON content, not XML: `oldValue`, `newValue` (strings, or `null` for no description)
  and `user` (the initiator's MRI). A personal build's channel rename brought two identical `TopicTopicUpdated`
  events a fraction of a second apart and a `TopicDescriptionUpdated` with both values `null`, each sent by the
  channel's thread (`19:…`).
- **Notifications**: the web client's chat notifications take only some types (its toast filter, read from the
  bundle): `Text`, `RichText/Html`, `RichText/Media_Card`, `RichText/Media_CallRecording` and
  `ThreadActivity/AddMember` (plus the Skype interop types). A recording is notified only when its upload failed (a
  state with a code other than `0` or `202` that isn't a successful export; none was seen), and an `AddMember` only
  for a pending chat request from outside the organization (otherwise its sender, the thread, has no profile and it is
  dropped). A call's start or end, a transcript and every other thread event are never notified. The native client
  notifies a text message and a card only, and never one the thread itself sent.
- **The chat list's preview** of a meeting chat says what the web client's list says: "Meeting started", "Meeting
  ended: <length>", "Recording has started", "Recording has stopped", "Recording is ready", "Transcript is
  available"; the transcript's half of a recording, a 1:1 or group call and a thread's events leave the line to the
  message before them, and so does a call's media message (`RichText/Media_Call…`) the native client doesn't read (a
  deleted transcript, a recording in another state, the call log's): its content is a record with ids in it, never
  shown as text, and never indexed for search or found by it.

## A message as sent (ab-chat-full, a)

`POST v1/users/ME/conversations/<chat>/messages` as in chat-service.md: the message's fields with `id` `-1`,
`version` `"0"`, `clientmessageid` a random numeric string (18 and 19 digits seen), `properties` with the empty JSON-string lists and
`formatVariant`. It answers 201 with `{"OriginalArrivalTime": <ms>}`, and the new message's `id` is that time. Typing is
the same post with `messagetype` `Control/Typing`, also answered with an arrival time.

## Images: sending, denied (ab-image)

What the web client does to send an image, with every write to an AMS object force-denied by the scenario (M1 Task 20).
a typed a caption, pasted a made-up 48×48 PNG into the compose box of the a–b chat (a clipboard paste the editor took;
it showed the image inline) and pressed Enter.

- **The object's creation** went to the **async gateway**, not to the GTM `ams` host (`us-api.asm.skype.com`, which only
  saw its token exchange): `POST https://us-prod.asyncgw.teams.microsoft.com/v1/objects/`, after an `OPTIONS`
  preflight to the same URL. Its credential is the chat service's: `Authorization: Bearer` with the
  `https://ic3.teams.office.com` token and `X-MS-Migration: True`; also `x-ams-post-sharing-mode: Inline`,
  `x-ms-client-version`, `ms-cv` and `content-type: application/json`. The body, exactly:
  `{"type": "pish/image", "permissions": {"<the a–b chat id>": ["read"]}, "sharingMode": "Inline", "filename":
  "MicrosoftTeams-image.png"}`: the conversation that may read the object is the one key of `permissions`, and the
  client renamed the pasted file.
- **Denied**, the client tried the creation again with a growing pause (seven times over about five minutes in the
  run of 22:52Z), then kept the message in the chat as being sent: a bubble with the image and a pending mark. It
  **sent no upload** (that needs the creation's answer) and **no message post**, neither the caption nor
  `amsreferences`. While the image was pending the page asked before being left (a `beforeunload` dialog), which in a headless browser shows to nobody and
  holds any navigation; the scenario's navigations now accept it. After the page was left and loaded again the image was
  gone and its creation wasn't tried again (the run of 01:08Z, 30 s watched).
- **Bundle only** (never seen): the creation's answer is read as `{id}`; the content is then put with
  `PUT v1/objects/<id>/content/imgpsh` (`application/octet-stream`; a ranged, resumable form with `Content-Range` too),
  which names the object alone; `POST v1/objects/<id>/forward` with `{"permissions": {"<another conversation>":
  ["read"]}}` lets another conversation read an existing object; `GET v1/objects/<id>/` reads the object (its answer's
  shape unknown); the views are `v1/objects/<id>/views/<name>` (`imgo`, `imgpsh_fullsize_anim`, …) and their
  `…/status`. The creation's `permissions` may carry further keys (`@:composer`), and its `type` is `sharing/file` or
  `sharing/videototranscode` for other media.

The M1 scope stopped at these shapes while every write to an object was denied. With the lab's write fence gone,
the native client now sends pictures ([Pictures sent](#pictures-sent-team-manage-2026-11-06)).

### The object created once (ab-image-object)

M2 Task 21 took the backlog item's first step: the creation mapped in exactly its captured shape (the async gateway,
the captured headers with `x-ams-post-sharing-mode: Inline`, `X-MS-Migration: True`, `x-ms-test-user: False`,
`content-type: application/json` pinned, the body above with one `permissions` key, the a–b chat, `["read"]`) to a
write on that chat admitted by the capture-era policy (members a and b), let out once per run by a selective hold
that spared the first creation sent after the image was in the compose box; the upload, a forward, every later
creation and every other media write stayed force-denied. The mapping was the experiment's only (`ab-image-object`)
and was taken out again in the same task once the read below was in: the code is in the task's first commit, the
tree has none of it.

- **The creation's answer**: `201` with `{"id": "<object>"}` (the id like `0-eus-d15-<32 hex>`). The fence logged it
  allowed (`media_object` on the a–b chat, once per pause: the page's and the browser's).
- **What the client did next**: the upload, `PUT v1/objects/<id>/content/imgpsh` on the same host, no query,
  `content-type: application/octet-stream`, the `ic3` bearer token, `X-MS-Migration: True` and `x-ms-test-user:
  False`, the image's bytes as the body (denied); then three more creations within 12 s (denied; the run let only the
  first out), and no message post: neither the caption nor `amsreferences` went out while the upload failed. After a
  reload the client tried nothing again within 30 s, but its message actions for the unsent image showed no delete.
- **The object's read** (`GET v1/objects/<id>/`, `proto-lab probe object`, the same with and without
  `X-MS-Migration`): as a (its creator) and as b (the a–b chat's other member) `200` with eight keys, `contents` (an
  empty object: nothing uploaded), `creation_date`, `expiry_date` (25 days after the creation), `filename`
  (`MicrosoftTeams-image.png`), `geo` (`amer`), `id`, `tenant_id` and `type` (`pish/image`); as c (a lab account the
  object wasn't shared with) `403` with a problem-details body (`detail`, `instance`, `message`, `status`, `title`,
  `traceId`). The second run's object answered the same.

**The read is not authoritative** (decided with Codex, transcript `discussions/task-21-ams-object-read/` of the M2
run): it names no reader at all, no conversation, MRI or permission, so it can't say who may read the object now. The
200s and the 403 show that the service checks readers, and only for the accounts the lab holds; they can never show
that nobody else can read it. Even with a complete reader list the race between that read and the upload (a `forward`
from any client) would need evidence of its own. That question belonged to the fence, which is gone; pictures are
sent now ([Pictures sent](#pictures-sent-team-manage-2026-11-06)). Showing a received picture is a read
of its own ([Pictures in messages](#pictures-in-messages)).

The first run was **breached**: 15 s after the creation the laptop's lid closed and the machine suspended for eight
minutes; on resume the fence found its connection to both browsers gone and killed them. `fence.log` has nothing of
that run after 01:46:05Z and its captures end there too, so nothing shows what, if anything, the browsers sent between
the resume and the kill. The second run (`…/20261022T020426Z`) came out clean (`ab-image-object: ok`).

## Pictures sent (team-manage, 2026-11-06)

Browser Teams as a, in the a–b chat, with nothing held (`team-manage` with its writes let through, its steps pasting a
made-up PNG into the box as a clipboard paste, then Enter):

- **In the box** the pasted picture shows at once, in a paragraph of its own after the text, in Teams' picture frame
  (a small one centred in the 18rem square), the editor's element an `InlineImage` holding a `blob:` URL. **Nothing
  goes out while it sits in the box**: no object is made before Send.
- **On Send** the object is made (`POST /v1/objects/` on the async gateway, the body and `x-ams-post-sharing-mode:
  Inline` exactly as [above](#images-sending-denied-ab-image); answered `201` `{"id": "0-eus-d10-<32 hex>"}`), then
  the bytes go in (`PUT /v1/objects/<id>/content/imgpsh`, `content-type: application/octet-stream`, the `ic3` bearer
  token, `x-ms-migration: True`, `x-ms-test-user: False`): answered `201`. A later attempt adds `content-range: bytes
  0-<last>/<length>` and `x-ams-resumable-upload-supported: true`. Once the bytes are in, the page read the object's
  `imgo` view back (`GET /v1/<own object id>/objects/<id>/views/imgo?v=1`, `200`). The message shows a pending mark
  until its post goes out; messages go out in order, so an earlier one still uploading holds the later ones.
- **The post itself wasn't captured**: the capture's request interception stalls the browser's upload of the larger
  picture (71 KB; a 3 KB one went through), and the posts queued behind it. A received pasted picture
  ([Pictures in messages](#pictures-in-messages)) shows what the post carries: the `<img>` with `itemtype` AMSImage,
  `src` the object's `imgo` view on `us-api.asm.skype.com`, `width` and `height`, `id` `x_<object>`, `itemid` the
  object and `alt` `image`, in a paragraph of its own, and the object's id in the message's `amsreferences`.

The native client sends a pasted picture the same way (`teams_proto::CreatePicture`, `UploadPicture`, each through the outbox,
again only as `CallRetries` allows: a `401`, a short throttle; `teams_core::attachments`), but **at paste time**, while the picture is in the box, as
the file attachments are uploaded when chosen; Send waits for it. Its post writes the `<img>` above in the received
form's attribute order (`teams_proto::sent_picture`, `MessageText`), without the received ones' `itemscope="png"`,
whose value for another format no capture showed, and the object in `amsreferences`. An object made for a picture removed
before Send stays empty of readers but the chat's, unreferenced, and expires with the media service's 25 days.

Live (lab a to b, 2026-11-06): the creation answered `400` "The 'User-Agent' header is missing or invalid" ("the
platform id could not be created") without a `User-Agent`, and again with a browser's `User-Agent` alone; with the
web client's build in `x-ms-client-version` too (`1415/…`) it answered `201`, the upload `201` with an empty body,
the post `201`, and b's client showed the picture inline from the object's `imgo` view.

## Pictures in messages

How a received message carries a pasted picture and an emoticon, from the lab's captures (channel posts in `ab-chat`,
`abc-chat` and `load`) and the user's own chats as the personal build stored them, read for their shape only; and
how the web client draws them, from its bundle.

- **A pasted picture** is an `<img>` with `itemtype="http://schema.skype.com/AMSImage"`, its `src` a view of an AMS
  object: `https://us-api.asm.skype.com/v1/objects/<id>/views/imgo` in most, `…/views/imgpsh_fullsize_anim` for an
  animated one, and (in a signature pasted from mail) `https://us-prod.asyncgw.teams.microsoft.com/v1/objects/<id>/views/
  imgpsh_fullsize` without the `itemtype`. The object id is like `0-eus-d9-<32 hex>`. The size it is shown at comes as
  `width` and `height` attributes (a fraction too: `130.64889918887602`) or as `style="width:<n>px; height:<n>px"`
  (photos at their full size, `2082px` by `2776px`); `itemid` repeats the object id, `id` is `x_<object id>`, `alt` is
  usually `image`. Each sits in a paragraph of its own (`<p>`, or `<div>` joined by `<br>`).
- **Where the web client reads it**: whatever host the message names, it reads the same path from its own AMS base,
  the region's GTM `amsV2` (the async gateway, `https://us-prod.asyncgw.teams.microsoft.com` for the lab and the user),
  and draws it in an `<img>` authorized by an image cookie the page gets beforehand. The bundle's own requests to AMS
  carry `Authorization: skype_token <skypetoken>`. The native client reads the view with the `ic3` bearer token and
  `x-ms-migration: True` instead, the form the async gateway answered the object read in (`proto-lab probe object`);
  that form's answer to a view read hasn't been seen live.
- **How Teams draws it** (the bundle's inline image styles, its `rem` 10 px, and the user's screenshot of Teams beside
  ours): sorted by the size the message gives it into a `data-image-type`, then sized by it, never upscaled, its aspect
  ratio kept, in a frame of `colorNeutralStroke3` (a 1 px border and the background behind it, `borderRadiusLarge`
  corners) at the start of its line. *Tall* (at least twice as high as wide, `812 / 406`): fitted (`object-fit:
  contain`) into an `18rem` square. *Super-small* (lower than `18rem`, not long): an `18rem` square, the picture at its
  own size in it (`scale-down`). *Standard*: `18rem` high, as wide as its aspect ratio makes it (`max-width: 100%`).
  *Thin* and *ultra-wide* (at least twice as wide as high, lower or not than `18rem`): their own size, or once wider
  than `62rem` or higher than `48rem` scaled to at most `62rem` wide and `18rem` high (`--calculatedWideImageWidth`),
  never higher than `25rem` nor wider than the bubble. A narrow layout (side chat) uses `9rem` for `18rem`. A message
  of pictures alone has no bubble: the frames sit on the background. A picture pasted from a dense screen gives half
  its pixels' size (`width="198" height="53"` for 396 by 106), so it is drawn at that size.
- **An emoticon** is an `<img>` with `itemtype="http://schema.skype.com/Emoji"`, `itemid` its id in Teams' emoticon
  catalog (`speechless`, `smile`, `1f44b_wavinghand`), `alt` its emoji and a `src` on the CDN
  (`…/emoticons/<id>/default/20_f.png`, sometimes `100_f.png?v=<n>`), wrapped in a `<span>` whose `class` is
  `animated-emoticon-<size>-<id>` with `title` and `type` naming it. One inside text is styled `20px` square; one sent
  alone in a message `58px` (`animated-emoticon-58-…`). Teams draws one inside text at the text's height and a message
  of emoticons alone large, animated and without a bubble. The native client draws both from the emoticon's animation
  strip on the CDN ([Reaction pictures](#reaction-pictures)): the first frame inline, the whole animation large.
- **Stickers** are an `<img>` of `itemtype="http://schema.skype.com/Sticker"` on Teams' sticker CDN: see
  [Stickers](#stickers-team-manage-2026-11-19).
- **GIFs** picked from Teams' GIF picker are `<img>`s on Giphy's media CDN:
  `https://media<n>.giphy.com/media/v1.<token>/<id>/giphy.gif` (the shard `media0` to `media4`, the token Giphy's
  base64url note of where it was picked). The common form has no `itemtype`, its size as `width` and `height`
  attributes, `alt` `"<title> (GIF Image)"` and `style="height:auto; margin-top:4px; max-width:100%"`, alone between
  two `<p>&nbsp;</p>`; an older one has `itemtype="http://schema.skype.com/Giphy"`, its size in its `style`
  (`width:400px; height:300px`) and sits in a `<div>` after text and a `<br>`. The native client sizes it as a pasted
  picture, playing. The native client reads it from `https://media.giphy.com/media/<id>/
  <file>` with no token, leaving Giphy's token, the shard and any query out (Giphy serves the same bytes without them;
  [endpoints.md](endpoints.md#giphy-mediagiphycom-mediangiphycom)), and plays it as a pasted GIF is played.

## Stickers (team-manage, 2026-11-19)

Browser Teams as a, in the a–b 1:1 chat, with nothing held (`team-manage`, its writes let through). Then b's page
read the same chat. The captures are `team-manage/20261119T163307Z` (a) and `20261119T164448Z` (b) in the lab's data
dir. The fixture is `tests/fixtures/chatsvc/sticker.json`.

**The picker.** The compose box's "Emoji, GIFs and Stickers" button opens a panel with the tabs All, Emoji, GIFs and
Stickers and a "Find something fun" search box.

- All has the sections Emoji, GIFs and Stickers, each with "See all".
- Stickers has "Recent" (8 stickers) and "Sticker Packs" with "See all". "See all" shows each pack as a tile with its
  name and its header picture, in a scrolled grid of two columns, with a back arrow.
- A pack shows its stickers in a grid of three columns. A sticker that has a caption shows a pencil mark. Its button's
  name ends in "Editable." (`High five. Editable.`).
- The search finds stickers by their words ("hello" gave six, from several packs).
- No request lists the packs or the stickers. The previews come from
  `https://teams.public.onecdn.static.microsoft/evergreen-assets/funstuff/stickers/<locale>/assets/<Pack>/Preview/<name>.png?v=5`.

**A plain sticker.** A click on a sticker puts it in the compose box, after the text. It does not send it. The box
shows it at 180 by 180 in a frame. Send posts it:

- The `content` is `<p>&nbsp;</p>`, then a `readonly` element with `aria-label` and `title` (the sticker's words, for
  example `Clippy says &quot;Hello there&quot;`), `contenteditable="false"` and
  `itemtype="http://schema.skype.com/Sticker"`, then `<p>&nbsp;</p>`.
- In the element is one `<img>`: `src` is the sticker's GIF,
  `https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/<pack>-250x250/<locale>/<Name>.gif?v=5`,
  then `height="180"`, `alt` (the words again), `width="180"`, `data-image-type="standard"`,
  `data-image-mode="single"` and `itemtype="http://schema.skype.com/Sticker"`, in that order.
- Nothing goes to AMS: `amsreferences` is `[]`. The other fields are a text message's.

**As read back** (b's page, and a's own list), the service removed the `readonly` element and the `data-image-*`
attributes: `<p>&nbsp;</p>\r\n<img src="…" height="180" alt="…" width="180"
itemtype="http://schema.skype.com/Sticker">\r\n<p>&nbsp;</p>`. Thus the `img`'s `itemtype` is the mark of a sticker
that is read, and the `readonly` element is the mark of a sticker that is sent.

**How Teams shows it.** The message has no bubble. The sticker shows at 180 by 180 in a frame with a thin border and
round corners, under the sender and the time, as a pasted picture alone does. The GIF plays when the message comes,
then Teams shows its still preview (the `Preview/<name>.png` above) with a round play button over it. A click plays it
(the button changes to pause). A click does not open the picture viewer.

**A sticker with a caption** ("Edit sticker"). A click on a sticker with a pencil mark opens an editor in the panel:
the picture large, a "Caption" box with the sticker's own words in it, and Cancel and Done. The picture shows the
typed words in the place of the old words. Done puts in the compose box a PNG that the page drew (`blob:`, 240 by 180
for "High five"), in the same `readonly` element, labelled by the caption. Its `<img>` has `itemscope="image/png"`,
`itemtype="http://schema.skype.com/CachedImage"`, `alt="Sticker image, <caption>"` and `data-cache-key`.

- Send makes an AMS object (`POST /v1/objects/` on the async gateway, `type` `pish/image`, `sharingMode` `Inline`,
  `filename` `MicrosoftTeams-image.png`, `read` permission for the chat), answered `201`, then
  `PUT /v1/objects/<id>/content/imgpsh` with the PNG.
- **The capture did not show the post.** The capture's request interception stalls the browser's upload, as it did
  for a larger pasted picture ([Pictures sent](#pictures-sent-team-manage-2026-11-06)). The browser cancelled the
  `PUT` each time and made a new object. Thus the `<img>` that a captioned sticker sends, and its read-back form, are
  not known.

**The native client** reads a sticker in both forms (`teams_proto::chat::Sticker`): an `<img>` of itemtype Sticker,
or the `readonly` element of that itemtype around a picture, whose `aria-label` (else `title`, else the picture's
`alt`) is its label. Another `readonly` element stays unknown markup. It reads the GIF from Teams' sticker CDN with
no token and no query ([endpoints.md](endpoints.md#teams-sticker-cdn-teamspubliconecdnstaticmicrosoft)). It draws a
message of a sticker alone without a bubble, at the size the message gives, and the GIF plays. It does not draw the
still preview and the play button yet. The builder (`Sticker::of_teams`, `sticker_message`) writes the captured
`content` byte for byte. `Sticker::of_picture` puts a pasted picture's `<img>` (AMSImage, its object in
`amsreferences`) in the same element. That form is the best guess for a captioned or imported sticker until a capture
shows the real one.

**Stickers the user adds** (the native client only; Teams has no import). The compose box's picker has the tabs All,
Emoji and Stickers (no GIFs tab: [backlog](../backlog/picker-gifs-tab.md)). The user imports a folder or a `.zip` of
PNG, WebP or GIF pictures as a pack, kept for all the app's accounts beside the settings file
(`teams_ui::stickers`). A pick puts the sticker in the compose box, as Teams does. The box sends the picture to AMS at
once, as it sends a pasted picture (a still sticker as a PNG, an animated one as a GIF), and Send posts it in the
`readonly` element of itemtype Sticker around the AMSImage `<img>` (`Sticker::of_picture`'s form), between two
`<p>&nbsp;</p>` when the sticker is alone. An own server takes it with no change: its AMS rows keep the picture, and it
keeps the message's HTML as it comes.

## Link previews

How a received message carries the preview of a link, from the user's own chats as the personal build stored them,
read for their shape only (`tests/fixtures/chatsvc/link-preview.json`), and how Teams draws it, from the user's
screenshot of Teams.

- **The shape**: the link is an anchor in the text (`<a href="http://<site>" target="_blank" rel="noreferrer
  noopener">`), and `properties.links` is a JSON string of a list with one entry per link: `@type`
  `http://schema.skype.com/HyperLink`, `itemid` (`"0"`), `url`, `previewenabled` (`true`; a channel post's link had it
  `false`) and `preview`: `title`, `description`, `previewurl` and `previewMetadata` (`width` and `height` of the
  picture as numbers, `320.0` by `167.0`). `previewurl` is the async gateway's URL preview thumbnail of the page's
  picture: `https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image/Thumbnail?url=<the page's picture,
  percent-encoded>`. A link the compose box sent before its preview was read carries only `"preview": {"previewurl":
  ""}` ([Lines and links](#lines-and-links-chat-shapes)).
- **How Teams draws it**: a card under the message's text, in the bubble: the picture on the left (cropped to fill),
  beside it the title (semibold, one line), the description (two lines at most) and the site (the link's host,
  muted), on a surface a shade off the bubble's with rounded corners; a click opens the link. No card for a link whose
  preview is off or has no title.
- **Where the picture is read**: the web client asks the URL preview service on the async gateway with the `ic3`
  bearer token and `x-ms-migration: True` (its `urlp/v1/url/info` read in `chat-shapes`); the native client reads the
  thumbnail the entry names in that form (`teams_proto::LinkPreviewPicture` on `Service::Ams`), the page's picture
  link the only thing it sends. That read's answer hasn't been seen live; a card whose picture can't be read is drawn
  without it.
- **Another form**: a link that an app card stands for (a YouTube link) has `previewenabled: false` and a `previewurl`
  in the gateway's `urlp/v1/url/content?url=<picture>` form (seen live 2026-11-16, [App cards](#app-cards)).

## App cards

How a link's app card travels and how Teams draws it, **seen live** on 2026-11-16: lab account a posted YouTube links and
a Vimeo link in its 1:1 chat with b in the web client, and the capture kept the compose box's reads, the sent message
and its Trouter delivery (fixture `app-card.json` is written from it, with made-up values).

- **Where it comes from**: when the compose box sees a link that an app unfurls, the web client asks the app for a
  card with an invoke on the chat service: `POST v1/agents/28:<the app's bot>/invoke`, `name`
  `composeExtension/anonymousQueryLink` (the second link of the session: `composeExtension/queryLink`), the app's id
  (`com.microsoft.teamspace.tab.youtube`) and `value.url` the link. The answer (`composeExtension.attachments`) holds
  the Adaptive Card and a hero `preview` of it. The compose box shows the card under the text, with a close mark,
  and, the first time, a line "By using YouTube, you agree to the privacy policy, terms of use, and permissions." The
  sent message does not carry that line, and Teams does not show it after a reload.
- **The message**: a plain `RichText/Html` message. Its `content` is the link's paragraph, then a
  `<span itemscope itemtype="http://schema.skype.com/InputExtension" itemid="<cardClientId>">` with an empty
  `<span itemprop="cardId">`. `properties.cards` is a JSON string of a list, one entry for each card: `appId`,
  `appName` (`YouTube`), `appIcon`, `cardClientId` (`app-preview-card-<hex>`), `contentType`
  (`application/vnd.microsoft.card.adaptive`) and `content`. In the sent message `appIcon` is the app's icon on
  `res.cdn.office.net`. In the delivered message the service changed it to the async gateway's copy,
  `urlp/v1/url/content?url=<the icon>`, and the elements had no `separator: false`. `properties.links` has the link
  with `previewenabled: false` and a `preview` (`title`, the counts as `description`, `previewurl` the gateway's
  `urlp/v1/url/content?url=` of YouTube's `mqdefault.jpg`), so no link preview is drawn.
- **YouTube's card**: Adaptive Card `version` 1.6 (`fallbackText` says it needs 1.6): a `Media` element (`sources`, one
  `video/mp4` source whose `url` is the watch link, no `poster`); the title (`size` `large`, `weight` `bolder`,
  `maxLines` 4); the channel and "<n> views · <age>" (`isSubtle`, one line each, `spacing` `Small` and `None`); an
  `ActionSet` (`spacing` `Medium`) with an "Open" `Action.Submit`. Its `data.msteams` is an `invoke` whose `value` is a
  `tab/tabInfoAction`, with `tabInfo` (`contentUrl` the app's stage page with `{chat.id}`-style placeholders,
  `websiteUrl` the watch link, `entityId` a GUID). The card's `selectAction` is the same action.
- **How Teams draws it**: under the message's text, in the bubble, a card 448 px wide with a 16 px padding: the app's
  20 px icon and its name (semibold) in a row, then the video's picture across the card at 16:9 with a white round
  play mark (56 px) in its middle, then the title, the channel, the counts and an outlined "Open" button. The web
  client reads the picture itself from `img.youtube.com/vi/<id>/maxresdefault.jpg`. When that picture is missing (an
  old video), it shows a grey stand-in with YouTube's logo. A click on the picture plays the video in the card
  (YouTube's embedded player). "Open" shows the YouTube app's player in a Teams dialog and sends no request. Dark mode
  only changes the surfaces.
- **Other sites**: a Vimeo link got no app card. It got an ordinary link preview (`previewenabled: true`, the
  `urlp/v1/url/image/Thumbnail` picture, [Link previews](#link-previews)).
- **A message extension's card** (behind `NGTEAMS_BOTS=1`): the native client sends a card chosen from an app's
  search or dialog in the same form (`teams_proto::chat::AppCard`, `MessageText::with_cards`): the span after the
  text, `\n` before it as the read-back has it, and the entry in `properties.cards`; a message without one sends `[]`
  as before. Unseen for a message extension: `docs/protocol/bots.md`, "Message extensions".
- **The native client** reads `properties.cards` with `teams_proto::chat::card::message_cards`: an entry whose card it
  draws is not content it can't show; any other entry still is. The video's picture is YouTube's `hqdefault.jpg` of
  the video, which exists for every video. The client reads it through the async gateway's thumbnail, as a card's
  pictures are read, and does not reach YouTube itself. It reads the app's icon in the same way. A click on the
  picture or on "Open" opens the watch link in the browser, because the client has no web player. Not seen live:
  the gateway's thumbnail of these two pictures.

## Cards

What a bot's or a workflow's card message is and how Teams draws it, seen 2026-11-05 in the web client as lab account
a (the Workflows webhook template's post in a team's channel and the Workflows bot's chat), its stored message read
for its shape only.

- **The shape**: `messagetype` `RichText/Media_Card`, sent by the bot (`28:<app id>`, the Workflows bot's
  `imdisplayname` "Workflows"). `content` is `<URIObject type="SWIFT.1" url_thumbnail="…">` whose text is a fallback
  ("Card - access it on <link>.", `<Title>Card</Title>`) and whose `<Swift b64="…">` is the base64 JSON of a bot
  activity: `type` `message/card`, `attachments` (each a `contentType` and a `content`), `channelData`, `entities`,
  `timestamp`. The Workflows bot's attachments are Adaptive Cards (`application/vnd.microsoft.card.adaptive`,
  `version` 1.0 to 1.5): `TextBlock`, `Image`, `ColumnSet`/`Column`, `Container` (`style` `emphasis`, `showBorder`,
  `roundedCorners`, a `selectAction`), `isVisible: false` elements, `Action.OpenUrl` buttons (`iconUrl: icon:Open`), and
  date functions in text (`{{TIME(<ISO time>)}}`). A post a workflow makes on someone's behalf carries
  `properties.onbehalfof` (a list of `mri`, `displayName`, `mentionType: person`) and `properties.botMetadata` (the
  workflow's id); the webhook template's own card ends in a separated container whose small, lighter text says who
  used the template, with a "Get template" link (`teams.microsoft.com/l/task/<app id>?url=…`).
- **How Teams draws it**: the sender's name ("Workflows") and time, the app's icon on a rounded tile with a border
  as its avatar, and the card on its own surface (white in the light theme, bordered, rounded, a faint shadow, about
  448 px wide), padded 16 px: a `TextBlock` at 14/20 px (`small` 12/16), `bolder` semibold, `lighter` light,
  `isSubtle` muted; 12 px between elements, a separator a hairline across that gap; a date function written in the
  reader's time zone ("9:15 PM"); an emphasis container grey with a border; an `Action.OpenUrl` an outlined button with
  its icon. The chat list's preview line of a card is "Sent a card".
- **Pictures**: the web client reads a card's picture straight from its link (a Teams CDN's here). The native client
  asks the async gateway's thumbnail of it instead, as for a link preview's (`LinkPreviewPicture::card_picture_src`),
  so it never reaches the picture's site; that read of a card picture hasn't been seen live.
- **Buttons that send to the bot** (`Action.Submit`, `Action.Execute`, a hero card's `imBack`/`messageBack`): the web
  client runs them as invokes on its own chat service, `POST v1/agents/28:<bot>/invoke` ([bots.md](bots.md)); a
  Microsoft account's invoke carries its Teams service token for that service, never a token for the app. The
  native client reads what each button carries (its `data`, `verb`, `msteams` kind) and a card's inputs and
  `refresh`. With the bots shown (`NGTEAMS_BOTS=1`, until the bots work is done) it draws the inputs and its buttons
  ask the bot, as [bots.md](bots.md) says. In read-only mode the inputs are drawn but take nothing, and a button opens
  the message in Teams on the web. Without the bots shown, a button opens the message in Teams on the web, and a card
  with an input says it has content it can't show.

## Forwarded messages

How a received forwarded message reads, from the personal build's debug log of 2026-10-28 (a sync page's and a
Trouter delivery's copy of the same messages, read for their shape only; fixture `forward.json` is written from it).

- **Content**: one `<blockquote itemtype="http://schema.skype.com/Forward">` (no `itemscope`, no `itemid`) holding the
  original's content as it was: paragraphs, links, a pasted picture (its `<img>` as in a message of its own,
  [Pictures in messages](#pictures-in-messages), the same AMS object), a code block. The paragraphs are joined by CRLFs,
  as a read-back message's are. Nothing names the original in the markup.
- **`properties`**: `forwardTemplateId` `basic_forward_message_template`, and `originalMessageContext`, an object (not a
  JSON string, unlike `mentions`), as read back and as delivered: `originalSender` (the original's sender's MRI),
  `originalSentTime` (an ISO time, three fraction digits), `messageId` (the original's id, a number), `originalThreadId`
  (the conversation it was sent in: a 1:1 chat or a group chat), `threadType` (`chat`) and `messageType`
  (`RichText/Html`). `amsreferences` stays `[]` with a picture in the content; `mentions`, `cards` and `files` are the
  forward's own (`"[]"` in every one seen).
- **How Teams draws it**: the content in a bordered card (a thicker left edge) under a header of the forward arrow,
  the original sender's avatar and name, and the original's time in full ("10/28/2026 2:49 AM") as a link to the
  original message; the content as a message's own, its pictures drawn. The native client draws it so
  (`teams_ui::forwarded`); the link opens the original in the app when the account has its chat and the store holds the
  message, else in the web client (`l/message/<chat>/<id>`).

## Code blocks

How a received message carries a block of code, from the user's own chats as the personal build stored them, read
for their shape only, and how Teams draws one, from its window.

- **Teams' code block editor** sends a paragraph of its own, then the code:
  `<p itemtype="http://schema.skype.com/CodeBlockEditor" id="x_codeBlockEditor-<guid>">` with a line feed and `&nbsp;`
  in it, then `<pre class="language-<id>[ language-was-manually-selected | language-was-auto-selected]"
  itemid="codeBlockEditor-<guid>"><code>…</code></pre>`, often between two empty `<p></p>`. In the `code` the lines are
  split by `<br>` (a raw space after each), the spaces are `&nbsp;`, and the tokens the editor highlighted are
  `<span class="hljs-<kind>">`: `string`, `function` (a JSON key, a shell command's word), `keyword` (`null`, `set`),
  `comment` and `default` seen. The language ids seen: `plaintext` (most), `bash`, `json`, `shell`, `csharp`, `css`,
  `html`, `javascript`, `markdown`, `vhdl`.
- **Others**: an older or pasted block is `<pre class="language-plaintext"><code>…</code></pre>` with no `itemid` and
  no editor paragraph, or a bare `<pre>` (with a `style`, or inside pasted markup with spans of no class); their lines
  `<br>` or line feeds.
- **How Teams draws it**: a card with a border and rounded corners, in the flow between the lines around it; its header
  a code icon, the language's name (`JSON`) and a copy button on the right, over a hairline; then the
  lines, numbered in a muted column, in a monospace font, the tokens coloured (in the dark theme a key red, a string
  green, `null` in the brand colour). The editor's paragraph shows nothing.
- **The native client** reads every `pre` as a block of code (`teams_proto::chat::Code`): the language's id, the text
  (a `br` a line feed, a no-break space a space, any element in it giving its text) and each run's token kind from its
  `hljs-` class, nothing else of the markup; the editor's paragraph is dropped. It draws Teams' card
  (`teams-ui/src/code.rs`), the language named as Teams names it, else its id, `Plain text` without one; the copy
  button copies the code's lines as they came (a tab drawn as four spaces, copied as a tab). The compose box's
  editor writes one (Ctrl+Shift+B, or the format bar's code button over more than a line) as
  `<pre class="language-plaintext"><code>…</code></pre>`, without the editor's paragraph and the token spans, and
  loads one back for an edit.

## Formatted text

How Teams' message box formats text, from its web client's bundle (its editor is CKEditor 5 with Teams' own
configuration and hotkeys) and from the shapes of formatted messages in the user's own chats as the personal build
stored them, read for their markup only (tags, attributes and style properties counted; no text kept).

- **Marks**: bold is `<strong>` (`<b>` too in older and pasted messages), italic `<i>`, underline `<u>`,
  strikethrough `<s>`, inline code `<code>`; nested italic outermost (`<i><strong>…</strong></i>`).
- **Colour, highlight and size**: one `span` per run of the same ones, its `style` holding them in the order
  `background-color`, `color`, `font-size`, around the marks: `<span style="background-color:#EBD3E1;
  color:#1E53A3; font-size:inherit"><strong>…</strong></span>`. The editor wraps most text in
  `<span style="font-size:inherit">` (its Medium): no format. The sizes are Small `font-size:xx-small`, Medium
  `inherit` and Large `font-size:x-large` (a hidden size, `0`, also exists). The font colours (the web client's
  palette, named as its menu names them): Red `#B6424C`, Tuscan orange `#CD5937`, Sunglow `#FDC030`, Pear `#BDCB4C`,
  Eucalyptus `#2B9B62`, Faded jade `#37797B`, Fun blue `#1E53A3`, Tyrian purple `#A5397A`, and Automatic (none). The
  highlights: Cranberry `#DF9299`, Pumpkin `#F4A593`, Marigold `#FDD472`, Forest `#E5F18F`, Seafoam `#82CDA8`, Light
  teal `#9DD9DB`, Blue `#C7D4E8`, Lilac `#EBD3E1` (the web client draws them through its theme's palette tokens and
  sends these hex fallbacks).
- **Blocks**: a paragraph `p` (an empty line `<p>&nbsp;</p>`), headings `h1`–`h3`, a quote
  `<blockquote><p>…</p></blockquote>`, lists `ul`/`ol` of `li` holding their text directly, a nested list inside
  the item before it (`<li>a<ul><li>b</li></ul></li>`), an indented paragraph `<p style="margin-left:40px">` (40 px
  a step), a horizontal rule `<hr>`, tables `<table><tbody><tr><td>` (seen pasted). Paragraphs often carry
  `margin-left:0` and margins of the editor's own, which say nothing.
- **Hotkeys** in the box (the web client's compose hotkeys): Ctrl+Enter sends from the expanded box, Shift+Enter is a
  new line, Ctrl+Shift+X expands and collapses the box ("ComposeFull"), Ctrl+B, Ctrl+I, Ctrl+U, Ctrl+Alt+X
  strikethrough (Ctrl+Shift+X is taken by the expand), Ctrl+K a link, Ctrl+Shift+C inline code, Ctrl+Shift+B a block
  of code, Ctrl+Shift+7 and Ctrl+Shift+8 numbered and bulleted lists, Ctrl+Alt+4 a quote, Ctrl+Alt+1 to 3 headings,
  Ctrl+Alt+P a paragraph, Ctrl+Z undo.
- **The native client** reads `span` colours (`#RGB`, `#RRGGBB`, `rgb()`, `rgba()`; one fully transparent, as a
  browser's copy of plain text carries, is none), highlights and sizes (keywords, and a length under 12 px small, over
  18 px large) into `teams_proto::chat::Node::Span`, headings, indented paragraphs and rules into nodes of their own,
  and draws them (a heading larger and semibold, a paragraph at the size most of its text has, since GPUI lays a line
  out at one size; a colour that wouldn't read on the theme moved toward the text colour, a highlight at a quarter
  strength on a dark theme, three quarters of the way to the surface). The compose box is its own rich-text editor
  (`teams-ui/src/editor`), which writes exactly the shapes above: `strong` for bold, a link as the web client's anchor
  (`rel="noreferrer noopener" title="<url>" target="_blank"`, `properties.links` left `"[]"`), a space after a space or
  at a line's edge as `&nbsp;`. Its keys are the web client's, but Ctrl+K stays the app's chat switcher (the format bar
  inserts a link) and Ctrl+Shift+C inserts inline code in the box, as in Teams. Writing tables isn't supported yet: the
  "More" menu shows the entry disabled.

## Files: sending, denied (ab-file)

### Chat-file upload and specific-recipient sharing

A fresh armed reference on 2026-11-03 supersedes the old unavailable-storage finding below: the lab account now offers
"Upload from this device". Picking a synthetic text file starts a content `PUT` to its personal SharePoint site's
`_api/v2.0/drive/root:/Microsoft Teams Chat Files/<name>:/content`, with `@name.conflictBehavior=rename` and
`$select=*,sharepointIds,webDavUrl`. A forced-deny probe came first. The next upload, after the folder's current grants
were read, answered 201 with its item id, name, size, web URL, parent drive and SharePoint item identifiers. The folder
did not exist before this upload; its new contents inherit the drive root's grants. The file stays in the composer
until Send. Send first attempts `POST _api/v2.0/sites/root/items/<item GUID>/driveItem/invite`, body with `recipients`
(one `email` per recipient) and `roles` containing `write`; the initial attempt was denied before any sharing.

The implicit body is unsafe in this tenant: a later reference attempt returned an anonymous edit link despite naming
one specific recipient. That run is a fence breach; its evidence is preserved separately. The final chat post was
forced-denied. The native uploader never uses that implicit request. The anonymous permission was revoked; a fresh owner read showed only the owner, and the old unauthenticated link returned “This link has been removed” without file contents. Native sharing requires explicit `requireSignIn:true` and
`sendInvitation:false`, the documented direct-grant semantics, and confirms the resulting effective grants before
posting a message. A separate armed direct-grant request, followed by a fresh read, showed only the owner and the
named lab recipient, with no sharing link. Revoking the unintended anonymous permission uses the documented
revoke-only `DELETE .../driveItem/permissions/<permission GUID>` endpoint; it grants nobody access and creates no
invitation. The original breach is not replaced by this later clean evidence.

The native transport verifies the authenticated Graph actor with `/me` and independently checks current effective
grants as the item's owner. It reads the drive's owner and requires that id to match the active account; then it reads
the folder and its permissions, or root permissions if the folder is absent. A staged upload requires an owner-only
private folder. Renaming on conflict prevents replacing an existing shared file.

For an explicit share, the native client reads the selected thread's complete membership and leaves bots out of the
recipient set. The intended recipients must be exactly the other people in that chat. Each recipient's Graph object
id must match, and the intended email must match the current `userPrincipalName` or `mail`, ignoring case. The share
reads the item's effective grants before sending and confirms them again before queuing its file-reference message.
Plain message attempts do not repeat those membership or grant reads. The owner receives all effective grants,
including inherited permissions, according to
[Microsoft's permission-list contract](https://learn.microsoft.com/en-us/graph/api/driveitem-list-permissions?view=graph-rest-1.0).
A non-owner, a page, an unknown grant, a group or a broad sharing link fails closed. Every check reads again; grants
from an earlier upload are never reused for a share. These actor, ownership and direct-recipient checks apply to every
build; they carry no lab roster. The agent applies the lab-contact check before a live run.

What the web client does to send a file, with every write of it force-denied by the scenario (M2 Task 23): writes to
the file services (SharePoint and OneDrive hosts, their media hosts, Graph), to AMS objects, and any `POST` or `PUT`
whose body names a lab file (the file's name and the caption both carry the marker). a typed a caption, then handed
the compose box of the a–b chat a made-up text file.

- **A drop or a paste of the file attaches nothing.** The editor takes a pasted image
  ([Images](#images-sending-denied-ab-image)), but a `text/plain` file dropped on it or pasted into it showed no
  attachment. Pressing Enter then (the first run, whose attachment test took the typed caption for the file) posted
  the caption alone, `properties.files` `"[]"`, tried five times within the minute watched, denied each time; the
  client kept it as being sent and, after a reload, a deleted it from its actions without a request (it had no id on
  the service).
- **The attach button asks the account's own file storage first.** The compose box's controls are "Insert link",
  "More options", "Discard", "Show Formatting options", "Emoji, GIFs and Stickers", "Attach files", "Actions and
  apps" and "Send". "Attach files", clicked with the page's file pickers made to hand the file over without a dialog,
  opened no menu and asked for no file; the page said "Sorry, you need OneDrive to share files. See your admin about
  getting a license." Before that the client had asked `POST <middle tier>/beta/me/files/availability` (no body, the
  `api.spaces.skype.com` token, `x-ms-client-caller: IsFluidSiteProvisioned`), which it sends on every load and the
  fence of the time denied (it passes under the lab rule). Its answer was never seen, so whether the lab accounts have
  a OneDrive at all is open: the notice may be the client's reading of the denied check.
- **Nothing of a file went out**: no write to SharePoint, OneDrive, Graph or an AMS object, and no message post with
  a file. The plain message before it (no marker) was sent and b showed it; b never showed the file's name (the
  first run; the later ones stopped at the notice).
- **Bundle only** (never seen): a chat file is uploaded into the account's own OneDrive, folder "Microsoft Teams
  Chat Files" (`…/drive/root:/<folder>/<name>:/createUploadSession` on Graph or SharePoint's `_api/v2.0`, or a quick
  upload), shared with the chat's members (`shareDriveItem`), and named in the message's `properties.files` (a JSON
  string of a list; an entry has `fileName`, `fileInfo` with `fileUrl` and `shareUrl`, `@type`
  `http://schema.skype.com/File`, `objectUrl`, `itemid`, `title`, `version`).

File sending remains a separate implementation task (`docs/backlog/file-sending.md`): storage availability,
upload into the account's drive, sharing with the intended recipients, then posting the file reference. Those wire
shapes and error paths need implementation and tests. Before any lab experiment the agent must include the share's
recipients and the destination chat in its reach check; there is no participant permit in the transport.

## Files: received

Seen in a message the user's own account received (two videos shared from the sender's OneDrive), read locally from
the personal build's store and never copied: the fixture `tests/fixtures/chatsvc/files.json` is written synthetic in
its shape (two files, made-up names and links). No lab capture holds a received file.

- **The text** is the caption, one `<p>` whose lines are broken by `<br>` each followed by a line feed (`<br>\n`), a
  blank line being two of them. The line feed is markup whitespace: `teams-proto`'s reader sets whitespace as a
  browser does (a run of it is one space, none at a line's start or end, a `pre` keeping its own), so the lines read
  one under the other with one blank line between paragraphs, as the web client shows them.
- **`properties.files`** is a JSON string of a list, one entry per file, in the order the cards show them. An entry:
  `@type` `http://schema.skype.com/File`; `itemid` and `id` (the file's list item id); `fileName` and `title` (its
  name); `fileType` and `type` (its extension, `mp4`); `fileInfo` with `fileUrl` (the file's address in the sender's
  "Microsoft Teams Chat Files" folder, its own name for the folder in the sender's language), `siteUrl`, an empty
  `serverRelativeUrl`, `shareUrl` (the sharing link, `https://<tenant>-my.sharepoint.com/:v:/g/personal/<user>/<id>`;
  `:v:` is a video's kind) and `shareId`; `fileChicletState` (`serviceName` `p2p`, `state` `active`); `version` `2`;
  `baseUrl`; `objectUrl` (as `fileUrl`); `openUrl` `null`; `state` `active`; `permissionScope` (`Anyone` for an
  anyone link); `filePreview` with an empty `previewUrl` and sizes `0` (a video has none); `sharepointIds` (`listId`,
  `listItemUniqueId`, `siteId`, `siteUrl`, `webId`, `driveId` `null`); `publication`; `site`. The message also carried
  `properties.atp`, the protected links of the files.
- **The web client draws** each file as a card under the text, side by side: a header with the file type's icon, the
  name cut with an ellipsis, Share and "More options", then a picture of the file filling the card (a video's frame).
  A click opens the file at its sharing link (Stream's player for a video).
- **The native client** reads an entry as a file in this shape (`teams_proto::chat::shared_files`: the file's
  `@type`, another `…File` type or none, a name, `state` `active` when given). It keeps only a SharePoint `https`
  sharing link or object URL. A file with no such link is drawn by its name with no link and no preview, and is also
  kept as it came ("Several files in one message" below). Any other entry is kept as it came, and the message says it
  has content it can't show. It draws the card from the name, the extension and the
  links, opens the sharing link in the browser on a click, offers "Open in browser", "Download" (the sharing link with
  `download=1`) and "Copy link", and reads a video's or an image's picture from Graph by the sharing link
  ([graph.md](graph.md#a-shared-files-thumbnail)). A video is played in its card (by GStreamer),
  its bytes read by ranges at the download link Graph hands out ([graph.md](graph.md#a-shared-files-content)). Share
  offers only "Copy link": sharing with people from the app would be a contacting write.
- **An image file** (seen in a received message, an image shared alone with no caption: the message's content empty)
  carries the same entry with `fileType` `jpg` and a `filePreview` naming a picture of it on AMS: `previewUrl`
  `https://<region>-api.asm.skype.com/v1/objects/<id>/views/imgo`, `previewWidth` and `previewHeight` (501 by 500
  there). The web client draws it as that picture alone, with no card or header, about half the preview's size (253
  by 252 there), its corners rounded, and the message with no bubble. The native client reads the preview only when
  its link is a view of an AMS object as a pasted picture's is (`teams_proto::chat::FilePreview`, through
  `MessagePicture::of`), reads it as it reads a pasted picture (the account's AMS, its skypetoken), draws it scaled
  down into 360 by 252, and draws the card (with Graph's thumbnail) when the preview can't be had.

### Several files in one message (lab, 2026-11-17)

Lab account a sent one message to b in their 1:1 chat from Teams on the web: a zip and four short videos, no text.
The fixture `tests/fixtures/chatsvc/files-grid.json` is written synthetic in the shape of b's read.

- **The send**: each file is uploaded to a's "Microsoft Teams Chat Files" folder (`…/drive/root:/Microsoft Teams Chat
  Files/<name>:/content`, one `PUT` for each file). The compose box shows a card for each file, "Anyone with the link"
  under its name (the tenant's default link). The page shows the message at once with "Uploading..." on each card
  that is not uploaded, and posts it only when all uploads are done. A headless run that attaches five files in one
  pick can leave some uploads open; one file in each pick does not.
- **The message**: `content` is empty (`""`), `amsreferences` `[]`, `cards` and `links` `"[]"`. `properties.files` has
  one entry for each file, in the order of the grid, in the shape above, with one more key, `altText` (`null`). b's
  read added `properties.atp`. a's push frames were a `NewMessage`, then a `MessageUpdate` with the same entries and
  `atp`.
- **What Teams draws**: a grid of cards in the message's bubble, two in a row. Each card has a header (the type's
  icon, the name cut with an ellipsis, and, on a received file's card in the user's Teams, `personal > <owner>`
  under it), then the picture: a video's frame, or the large icon of its type for the zip. Share is on the video
  cards only; "More options" is on each card.
- **Not reproduced**: a message in the user's own chat with a zip and four videos showed only "This message has
  content that can't be shown here" in the native client. The lab message in the shape above reads in full, so that
  message had a part that the lab cannot make. The native client now draws each file it can name: a `files` string
  with a lone surrogate escape (a browser reads it, `serde_json` refuses it) is mended, a list encoded twice or one
  entry alone is read, an entry with no `@type` or another `…File` type is a file, and a file with no SharePoint
  link is drawn by its name with no link kept (the message still offers Teams on the web for it).

### Files: shared on (lab, 2026-11-18)

Lab account b opened Share on a video card of a's message in the a–b chat, in Teams on the web. b picked c (their 1:1
chat) as the recipient, typed a message and pressed Share. c read the result in the b–c chat. The fixture
`tests/fixtures/chatsvc/file-shared-on.json` is written synthetic in the shape of c's read.

- **The dialog**: the title "Share this file", the line "You can share to any chat or channel.", a "More actions"
  (…) button with "Share outside Teams" (SharePoint's own share page), and Close. "Add recipients" (marked as
  necessary) is a box with the placeholder "Enter name, email, group or tag". "Message preview" is a box with "Add a
  message (optional)", the file's card (its icon, its name, and under it what its link lets people do, "Anyone with
  the link can edit"), then the file's picture, and the formatting and emoji buttons. At the bottom are "Copy link"
  and Share. Share is off until a recipient is in the box.
- **The recipient picker** finds people (a round picture, the name, the name again under it, and "(You)" after the
  account's own), chats by their names (a group chat with its people's pictures), and channels (a square picture of
  the team, the channel's name over the team's name in bold). Each pick is a chip with the picture, the name and a
  remove mark. A query with no match shows "We couldn't find any matches." Escape clears what is typed.
- **The reads**: when the dialog opens, the client reads the link's permission on the file's site,
  `GET https://<tenant>-my.sharepoint.com/_api/v2.0/shares/u!<the sharing link, base64url>/permission` (the
  site's SharePoint token). The answer has `roles`, and `link` with `scope` (`anonymous`), `type` (`edit`) and
  `localizedDescription` ("Anyone with the link can edit"), which the card shows. It also reads the item
  (`shares/u!<link>/?expand=site,driveItem(…)`) and its thumbnail.
- **The send**: one `POST v1/users/ME/conversations/<recipient chat>/messages` for each recipient, an ordinary message
  post (201). `content` is the message as one `<p>`. `properties.files` is a JSON string of a list with the file's
  entry from the original message, with these changes: `fileInfo.itemId`, `fileChicletState.state`, `state`,
  `altText`, `progressComplete`, `sharepointIds`, `publication` and `site` are null, `isUploadError` is false, and
  `permissionScope` is the link's scope (`anonymous`). Nothing else is written: no grant, no new link (the file's
  link reaches anyone).
- **After the send** the dialog closes and a toast at the bottom left says "You've shared <file name>" with a check
  mark and a close button. The page stays in the chat it was in.
- **Copy link** closes the dialog and opens SharePoint's "Link created" box: the link in a field, Copy, the link's
  permission words, and Settings.
- **Share outside Teams** opens SharePoint's share page in a frame: a form `POST` of
  `<site>/_layouts/15/sharedialog.aspx?clientId=teams&…` whose `context` holds a SharePoint token, the item's address,
  `uniqueId` and `shareId`. The same page opened by its address with the browser's own sign-in (`listId` and
  `listItemId`, or `uniqueId`) answers "Unable to share right now" (401, access denied), for the file's owner too.
  The file's sharing link opens Stream's player with Download only. OneDrive's page of the file
  (`<site>/_layouts/15/onedrive.aspx?id=<its path>&parent=<its folder>`) gives the owner SharePoint's Share menu
  (Share, Copy link, Manage access).
- **The recipient's read** has the entry as it was sent (`state` null) and the message's `atp`. Teams draws the
  message's text over the file's card.
- **The native client** offers the same dialog (`crates/teams-ui/src/share_file.rs`) from Share on a file card and
  from "More options > Share". It reads the permission with `HttpTransport::sharing_link_permission` on the file's
  personal site (`<site>_api/v2.0/shares/u!<link>/permission`, the same SharePoint token and service), builds the entry
  with `teams_proto::chat::share_entry`, and sends one `Action::Send` for each recipient with the entry
  (`MessageText::with_shared`): the outbox's ordinary post. A person with no 1:1 chat yet gets one first
  (`Action::CreateChat`, as New message does). The message is the compose box's rich-text editor, with its format bar
  and emoji picker, and is sent as the compose box sends a message. An entry with `state` null reads as a file. Copy
  link puts the link on the clipboard and shows "Link created" (the link, Copy, the permission words, Settings).
  "Share outside Teams" and Settings open OneDrive's page of the file in the browser
  (`teams_proto::chat::onedrive_page`): no token can go to a browser, so the app can't open Teams' framed page.
- **Restricted Microsoft links**: the capture covers only `anonymous` links. The native dialog refuses other scopes
  and a permission read that fails. It directs the user to Share outside Teams to set access. It sends no message
  and shows no success toast in these cases.
- **The own server** takes the same post. A message that names a file of the server, sent by someone with access to
  the file, gives each member of the chat access to it (`docs/server.md`). The message and its grants commit in one
  transaction. A grant error rolls both back. The server checks at most ten file entries per post. It answers the link's permission on the
  file's site for anyone with access (`scope` `users`). It has no web page for a file, so the dialog offers no "Share
  outside Teams" and "Link created" no Settings for its files.

## Messages a policy blocked

Seen in the user's own debug log (2026-11-13, a 1:1 message with a `.deb` file, blocked by the tenant's file type
check; structure only, nothing copied) and read from the web client's message renderer; the synthetic shapes are in
`tests/fixtures/chatsvc/policy-blocked.json`. Lab accounts can't trigger a tenant's policy.

- **The post succeeds** (201) as any other. About a second later the sender's endpoints get, in this order: a
  `ThreadUpdate` and a `ConversationUpdate` of `48:generic` (`threadType` `streamofgeneric`, the sender its only member,
  `threadProperties.originalThreadId` `19:teamsstream_generic_<sender's object id>@thread.v2`), then a `NewMessage` in
  `48:generic`: the **copy**. It has its own id and a new `clientmessageid`, the original `content` and
  `properties.files`, `threadtype` `streamofgeneric`, and nothing that names the original chat or message. Reading
  `48:generic` as a thread answers 400 ("The Thread Id is invalid").
- **The original message is then updated** (`MessageUpdate`, same id and `clientmessageid`): `content` empty, no
  `files`, `mentions` and `cards` `"[]"`, `properties.edittime` equal to its new `version`, and
  `properties.policyViolation`, an object: `state` (0 none, 1 blocked, 2 warning), `violationType` (`dlp`, `zap`
  after-delivery purge, `wft` file type, `mu` link reputation, `pd` phishing, `cc`), `verdictDetails` (flags: 1 may
  report a mistake, 2 may override, 4 may override with a reason), `dlpAction` (flags: 1 notify the sender, 2 block
  everyone, 4 block outsiders), `userAction` (flags: 1 overridden, 2 reported as a mistake), `justificationText`
  (seen: the check's own name) and `genericStreamItemId`, the copy's id as a number. A second update a little later adds
  `properties.atp` (the file's Safe Links data) at a newer version.
- **What the web client shows**: the sender's message as written, its text and files taken from the copy whose id the
  verdict names (it fetches `48:generic`'s message by that id when it lacks it), with the verdict over it; everyone else
  sees the verdict alone. No "Edited". By verdict: file type blocked, "This message was blocked because it contains a
  file type that is not permitted." with "Learn about file and link safety"; purge, "This message was blocked due to
  potentially harmful content." with "Learn more."; link warning, "This message contains a link that might be
  harmful."; phishing warning, "This message could be a phishing attempt." (no link); any other block, "This message
  was blocked." to the sender ("Blocked. You've reported this to your admin." once reported) with "Learn more.", and
  "This message was blocked due to organization policy." with "What's this?" to the others; any other warning,
  "This message was flagged." to the sender alone. The file type, link and phishing lines and the sender's are red,
  the others' block muted. The sender's "What can I do?" (override and report) needs a policy tip from the compliance
  service, which no chat service shape carries.
- **The native client** keeps each copy of the account's own (`blocked_copies`, schema 25) and puts its text and files
  back into the blocked message as stored, whichever comes first; without a copy it keeps the text the store had.
  A matching copy replaces that fallback, including its files and mentions, and updates the search index in the same
  transaction. For the sender's own blocked messages read from history or push, a missing copy is fetched with
  `GET v1/users/ME/conversations/48:generic/messages/<copy>?view=msnp24Equivalent|supportsMessageProperties`.
  The saved web transport bundle (`precompiled-web-worker-inner~services_i`, message request builder) shows that
  request; the message resolver's generic-stream branch uses its get-message workflow directly. A 404 leaves the
  fallback and notice in place. Missing or rejected copies are tried once per account session; other read failures
  are logged at debug and leave the chat's sync running. Each later catch-up revisits stored messages with missing
  copies, even when no conversation changed; a conversation sync does so for that chat. Syncs queue the stored
  missing copies before their normal reads. The engine retries them after publishing messages, outbox arrivals,
  replies and catch-up completion events, including when the normal reads fail. A page that brings a queued message
  again leaves its copy queued. Foreground chat sync and older-history reads run beside deferred copy reads through
  the catch-up's priority lane. Recovery errors
  do not change the normal sync result. Failed reads are retried once per sync, including after a restart, without
  replaying messages or holding back the sync cursor. The client never
  adds `48:generic` as a conversation (deliveries and the conversation list alike), keeps the stored edit time
  through a verdict's update, and words the notice as above (`crates/teams-ui/src/policy_notice.rs`).

## Mentions (mention)

Typing `@` in the compose box opens the mention picker (`AutocompletePopup-Mentions`); in the 1:1 chat its highlighted
suggestion was the chat's other member, by name, once the client could read that member's profile (the middle tier's
`users/fetchShortProfile` and `users/fetch`; Substrate's search stayed denied and wasn't needed). Typing the name after
`@` kept it highlighted, Enter put the mention in, and the post was an ordinary message post. Fixture:
`tests/fixtures/chatsvc/mention.json`.

- **As sent**: `content` holds the mention as `<readonly class="skipProofing" itemtype="http://schema.skype.com/Mention"
  spellcheck="false"><span itemtype="http://schema.skype.com/Mention" itemscope itemid="0">name</span></readonly>`
  followed by `&nbsp;` and the text, all in one `<p>`; `itemid` is the mention's index in the message.
  `properties.mentions` is a JSON string of a list with one entry per mention: `{"@type":
  "http://schema.skype.com/Mention", "itemid": "0", "mri": "8:orgid:<oid>", "mentionType": "person", "displayName":
  "<name>"}`, `itemid` a string. The rest of the post is as any message (above), answered 201.
- **As read from the chat** (b's message page): the `<readonly>` wrapper is gone, the span is
  `<span itemtype="http://schema.skype.com/Mention" itemscope="" itemid="0">name</span>`, and `properties.mentions` is
  still a JSON string, its entries with the same keys but `itemid` a number (`0`).
- **The mentioned account's own streams**: b's `48:mentions` stream (`GET v1/users/ME/conversations/48%3Amentions/
  messages?view=msnp24Equivalent|supportsMessageProperties&pageSize=200&startTime=1`) got an entry of its own: a message
  with its own `id`, `from` b's own contact URL, `imdisplayname` empty, `clumpId` the chat's id, `secondaryReferenceId`
  `T_<chat id>_M_<message id>`, the content without the `<p>` and with a plain space, `properties.mentions` without
  `@type`, and `properties.activity` (`activityType` `mentions`, `activitySubtype` `mention`, the source chat, message
  and sender, the target user, a text preview, `activityContext`), `views` (a JSON string naming the `ActivityFeed` view
  on `48:notifications` and the `Mentions` view on `48:mentions`), `messageUpdatePolicyValue`, `issaved`,
  `s2spartnername` `skypespaces`. The same activity reached b's `48:notifications` stream. b's client then marked the
  `48:mentions` item read with `PUT …/48%3Amentions/messages/<id>/properties?name=isread` and `{"isread":"true"}`, as it
  does for its activity feed (below); the fence denied it.
- CSA's update read (`GET api/csa/<region>/api/v3/teams/users/me/updates`) carried the message too, in its own
  camel-cased form (`messageType`, `imDisplayName`, `containerId`), with the span but no `properties`.

The bundle names more `mentionType`s than `person` (`bot` with an `appId`, `everyone`, `tag`, `channel`, `team`,
`community`, `nearby`, `share-contact`) and a reader that accepts `mentions` as a string or a list; of those only
`everyone` was seen ([A mention of everyone](#a-mention-of-everyone-abc-everyone)).

**A bot's mention** (from the bundle and the Bot Framework's documentation, **unseen**: no bot in the lab tenant can
be mentioned within the lab rule, `docs/protocol/bots.md`): the span as a person's, and the entry `{"@type":
"http://schema.skype.com/Mention", "itemid": "0", "mri": "28:<bot id>", "mentionType": "bot", "displayName": "<the
app's name>", "appId": "<the app's id>"}`. The native client sends it in that form behind `NGTEAMS_BOTS=1`
(`MessageText::new_with_bots`): its picker offers, after a group chat's members, the bots of the apps added to the chat
(`chats/<chat>/apps/chatentitlements`, those `Installed`) or to a channel's team (`teams/apps/aggregatedEntitlements`)
whose definition gives the bot the `groupChat` or `team` scope and doesn't make it notification-only, by the app's name
and icon. A capture that differs changes the client and the own server together.

Mention wire entries name an MRI. A person mention reaches that person; an `everyone` entry naming the destination
group chat reaches its members; a `bot` entry reaches its `28:` bot, one of the chat's participants. The native
composer builds the captured person and `everyone` forms, and the unseen bot form only behind `NGTEAMS_BOTS=1`; the
agent's lab check must include these recipients. There is no membership classifier on the outgoing request.

## A mention of everyone (abc-everyone)

Fixture: `tests/fixtures/chatsvc/mention-everyone.json`.

- **The picker**: in the lab's group chat (three members) typing `@` listed the chat's other members first (picture,
  name, address), then **Everyone** with an outlined people icon and "Notify everyone in the chat" under it, then
  "Add someone to the chat" and "Share someone's contact info"; the first member was highlighted. The bundle offers
  Everyone in a group chat behind a tenant setting and up to a roster size ("Not available for chats over 1,000
  users"), in a meeting chat behind settings of its own, and never in a 1:1 chat; its suggestion is keyed by the
  chat's id.
- **As sent**: the post is a group mention's ([A mention in the group chat](#a-mention-in-the-group-chat-chat-shapes)),
  key for key: the span inside the `<readonly>` element holds the label `Everyone`, and the one `properties.mentions`
  entry is `{"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": "19:<chat>@thread.v2", "mentionType":
  "everyone", "displayName": "Everyone"}`: its `mri` the chat's own id.
- **As read**: not captured. Per the bundle, a reader shows its own label for an `everyone` entry whatever name it
  carries, and marks it as mentioning the reader unless the reader sent it.

The native client sends it in that form (`MessageText` takes the chat's id as the mention's MRI, writes
`mentionType` `everyone`, and adds nobody to the write's target; a post or an edit with one naming another chat has
no wire form). Its compose box offers Everyone after the members in a group chat of at most 1,000 members (never a 1:1
or meeting chat). A received one shows `Everyone`, is the account's own mention in someone else's message (the "@"
mark, a notification at the "mentions only" level), and not in the account's own.

A sender whose Teams is in another language writes its own label in the span (for example `Все`); the entry still
has `mentionType` `everyone`. The bundle's chat list preview (its last message's sanitizer for previews) keeps only
`div`, `img` and `video`, so a paragraph's text joins the next and a `<br>` becomes a space: the whole message on one
line, cut at 85 characters, the mention's text as the sender wrote it. Its message view shows its own label instead.
The native client shows the reader's `Everyone` in every summary of the message (the chat list's row, a desktop
notification, the call window's chat bubble, a reply's quote, the local search's text), as its message view does,
and puts the message on one line with a space at each paragraph and line break. The Activity feed's
`messagePreview` is the service's own text and is shown as it comes.

## The native client's writes (M1 Task 12)

`teams_proto` sends each chat write in the form above, key for key (its tests compare the bodies with `mention.json`,
`reply.json`, `edit.json`, `delete.json`, `reaction.json` and the `writes.json` cases): a post with every key the web
client sent (`id` `-1`, `version` `"0"`, the `conversationLink` `blah/<chat>` it sends, the empty JSON-string lists,
`onbehalfof` and `policyViolation` null, `crossPostChannels` `[]`), the edit as the whole message again, the soft
delete, the reaction and its take-back, the read horizon, typing (`Control/Typing` only: the web client never sent
typing stopped, [Typing](#typing-chat-shapes)) and a new chat (`fixedRoster` and `uniquerosterthread` for a 1:1 chat, neither for a group). A message's
text is a typed rich-text value with the MRI of each mention, from which `content`, `properties.mentions` and the
write's target are all built; raw HTML can't be sent. Times go out as the web client writes them
(`2026-01-01T00:00:00.000Z`), an edit's `version` and `properties.edittime` being one instant.

**A mention is sent in the captured form** ([above](#mentions-mention)): the span inside the `<readonly>` element,
built in one place only, `mention_span` and `mention_entry` in `crates/teams-proto/src/chat/compose.rs` (written from
the bundle in Task 12, brought to the capture when the capture reached the M1 branch), and a no-break space written
`&nbsp;` as the web client writes the one after a mention. Its tests compare the post with `mention.json` and read the
mention back from both the post and the chat's copy.

## The shapes the UI needs (chat-shapes)

What M2's chat view and compose box need beyond M1's captures. Every write went to the a–b chat or the lab's group
chat, mapped and judged by the fence with members read live; the three runs exited 0 with no breach.

### Reactions (chat-shapes)

Fixture: `tests/fixtures/chatsvc/reactions.json`.

- The hover bar a message shows under the pointer offered four quick reactions, the same in every run: **`like`,
  `heart`, `laugh`, `surprised`** (`data-tid` `message-actions-<key>`, marked `data-inp="message-hover-reactions"`).
  The bundle takes the list from the account's settings or the service's config, with that list as its fallback, and
  turns `like` into `yes-tone<n>` for an account that chose a skin tone (bundle only; the lab accounts chose none).
- Each is set and taken back exactly as the like (`PUT`/`DELETE …/messages/<id>/properties?name=emotions`, the key in
  the body, answered 200 with the texts `200` and `null`). The keys stand side by side: setting one never took another
  back, so one account can hold all four on one message.
- Read back (b's `MessageUpdate` deliveries, each carrying the whole message), `properties.emotions` lists the keys in
  the order they were first set, each with its users; a key taken back stays with `users: []`, and
  `annotationsSummary.emotions` counts only the keys that still have a user (`{}` once none has). Each change raised the
  message's `version`.

### Reaction pictures

Not a capture: the web bundle's config and Teams' public CDN, read with no token.

- **The keys.** Besides the four quick ones, every reaction Teams offers is a key of its emoticon catalog
  (`…/personal-expressions/v1/metadata/<emoticonAssetVersion>/default.json` on `statics.teams.cdn.office.net`, the
  version the bundle's config names): nine categories, about 1 650 ids, each with its code points, whether it takes a
  skin tone (`diverse`), a description and its animation's frame rate (24 fps). An id is either an older name (`like`,
  `cool`, `hearteyes`, `fistbump`) or the code point and a name (`1f440_eyes`, the form the user's own chats showed
  stored). A reaction that takes a tone is sent with the tone appended, `-tone1` to `-tone5` (the bundle builds
  `<id>-tone<n>`; `like` becomes `yes-tone<n>`). The key goes in the same body as the quick ones'; the native client
  sends any key of the catalog in that form, and shows any other key it receives without sending it.
- **The pictures.** Each id's still picture and animation are on the CDN under
  `…/personal-expressions/v2/assets/emoticons/<id>/default/`: `50_f.png` (50×50) and `<size>_anim_f.png` (30, 50 or
  100 px frames stacked top to bottom, one strip per animation). The app draws its still pictures from Fluent Emoji
  3D (Microsoft's, MIT), packed into the app, and reads only the 50 px strip of the reaction under the pointer.

### Typing (chat-shapes)

- **What a sends**: the post M1 saw and the native client sends, key for key (`{"content": "", "contenttype":
  "Application/Message", "messagetype": "Control/Typing"}`), answered 201. **No `Control/ClearTyping`** went out in
  any run: not when a stopped typing, not when it emptied its compose box (15–18 s watched after), not when it sent.
- **When**: sparse. With a pressing a key twice a second for 45 s (keys pressed in one run, text inserted in
  another), no typing post went out while it typed; one went 0.2–1.6 s after the presses stopped, and one or two
  within a few seconds of a's sending a message (3.2 s and 7.2 s after it in one run). The closest two posts were
  4.0 s apart, the next 16.9 s. The bundle sends it through a throttle on the editor's changes, leading edge, at most one
  per `typingIndicator.notificationFrequency` (6 000 ms unless the service's config says otherwise), only in a chat of
  2 to 20 members (`maxRosterSizeInGroupChatChatServiceLimit`, default 20), never while a message is being edited and
  never in a reply chain (bundle only). Why the steady presses of the headless run brought none is not known.
- **What b shows**: b's indicator (`data-tid="typing-indicator"`) showed within 0.3 s of each typing delivery and went
  **about 25 s after the last one** (24.7–24.9 s after its delivery; 25.6–26.3 s after a's post) while a's text was still
  in the box, in all three runs; it went at once when a message from a arrived (1.0 and 1.2 s after a's post, as it was
  delivered). A typing delivery is a `NewMessage` of `messagetype` `Control/Typing` with
  `X-Trouter-Delivery-Control: …; resilience=nostorenoloss` ([trouter.md](trouter.md)).
- For the native client: send `Control/Typing` on input at most once every 6 s and nothing when typing stops; show
  another's typing for 25 s after their last `Control/Typing`, or until a message from them arrives.

### Lines and links (chat-shapes)

Fixture: `tests/fixtures/chatsvc/lines.json`.

- Shift+Enter in the compose box starts a **new paragraph**: three lines went out as `<p>…</p><p>…</p><p>…</p>`, no
  `<br>`. Read back, the service had joined the paragraphs with CRLF (`</p>\r\n<p>`); that is markup, not text, and
  `teams-proto`'s reader drops whitespace beside a paragraph or a quote (the quoted reply's CRLF after its quote too).
- A link typed and followed by a space became an anchor: `<a href="<url>" rel="noreferrer noopener" title="<url>"
  target="_blank"><url></a>`, and `properties.links` (a JSON string) held `[{"@type":
  "http://schema.skype.com/HyperLink", "itemid": "0", "url": "<url>", "previewenabled": true, "preview":
  {"previewurl": ""}}]`. Inserted with its text in one go, the link stayed plain text and `links` stayed `"[]"`.
- With a link in the compose box the client asked the async gateway for its preview, `GET
  /urlp/v1/url/info?url=<url>` (a read: the gateway fetched the URL and answered its title, status and favicon;
  [endpoints.md](endpoints.md)). The native client doesn't ask for previews.

### A mention in the group chat (chat-shapes)

Fixture: `tests/fixtures/chatsvc/mention-group.json`.

- The post is the 1:1 chat's ([Mentions](#mentions-mention)) to the group chat, key for key; the fence took b as its
  added participant and let it out, b being a lab account. The picker offered b by name in the group chat as it does in
  the 1:1 chat.
- b's delivery of the message carries `mentiontypes` `["person"]` beside the resource (`[]` on other messages); the
  message reads back as the 1:1 mention does (no `<readonly>`, `itemid` a number).
- b's `48:notifications` stream got an entry of `activityType` **`mentionInChat`**, subtype `person` (the activity feed's
  form for a group chat), and its `48:mentions` stream one of `activityType` `mentions`, reason and subtype `mention`,
  each pointing at the message (`T_<chat>_M_<message>`) as in the 1:1 chat.

## The activity feed

Teams' Activity pane reads the account's own stream `48:notifications` (`threadType` `streamofnotifications`), which
the conversation list brings and the catch-up and Trouter store as any conversation's messages. Seen with lab account a
in browser Teams (team-manage, 2026-11-12), and in a lab store's synced feed.

- **An item** is a message of `messagetype` `Text`, empty `content`, `from` the account itself, and in `properties`:
  `activity` (`activityType`, `activitySubtype`, `activityTimestamp`, `activityId`, `sourceThreadId`,
  `sourceMessageId`, `sourceReplyChainId`, `sourceUserId`, `sourceUserImDisplayName`, `messagePreview`,
  `sourceThreadType`, `sourceThreadTopic`, `activityContext`), `isread` (a boolean), `messageUpdatePolicyValue`. A
  `sourceMessageId` of `1` names no message (a missed call's, an app's). Kinds seen: `mentionInChat`/`person` and
  `everyone` (a group chat), `mentions`/`mention` (a 1:1 chat), `call`/`missedCall` and `voicemail`,
  `teamMembershipChange`/`addedToTeam` (with `sourceThreadTopic`, the team), `msGraph`/`customNotification` (an app:
  its title in `activityContext.templateParameters`, a JSON string, `customMessage`) and
  `msGraph`/`privateMeetingUpdated`. The bundle's `MTMAHubFeedItemActivityType` names more (`reply`, `replyToReply`,
  `like`, `likeInChat`, `reaction`, `reactionInChat`, …) and the Meet app's manifest the meeting titles
  ("{actor} invited you", "{actor} updated", "{actor} canceled"); none of those was captured.
- **The pane**: header "Activity" with "More options" ("Mark all as read", Shift+Esc; "Notification settings") and
  "Filter" (a box, "Filter by name or group name", filtering the loaded items with no request); chips "Unread"
  (Ctrl+Alt+U), "@Mentions" (Ctrl+Alt+M), "Tag mentions" (the bundle's filter types: unread, mention, reactions,
  missed call, reply, voicemail, apps). An item's "More options": "Mark as unread" (or read), "Remove", "Notification
  settings", "Turn off <app>". A missed call shows Call and Chat. Removing shows the toast "Removed".
- **Opening the pane** puts the feed's own horizon: `PUT …/48:notifications/properties?name=consumptionhorizon` with
  `{"consumptionhorizon": "<newest item's activityTimestamp, ms>;<now>;<its activityId>"}`. What arrived after it is
  new: the web client's rail showed no count while every unread item was older.
- **Opening an item** marks it read: `PUT …/48:notifications/messages/<id>/properties?name=isread` with
  `{"isread": "true"}`; "Mark as unread" sends `{"isread": "false"}`. Each answered 200; the item's `version` moved to
  the time of the change, and the next sync brought it with `isread` changed.
- **Mark all as read** sends `PUT …/48:notifications/properties?name=consumptionHorizonBookmark` with
  `{"consumptionHorizonBookmark": "0;<now>;0"}`, then the horizon as above, and nothing per item: every item then
  showed read. The conversation list carries the bookmark in the feed's `properties`. An item marked unread after it
  showed unread: an item reads as read when its `isread` is true or its `version` is no later than the bookmark.
- **Remove**: `DELETE …/48:notifications/messages/<id>?behavior=softDelete`, answered 200.
- **A meeting item** (`msGraph`/`privateMeetingUpdated`, seen live in lab account c's feed, 2026-11-06) comes from the
  Calendar app (`sourceUserId` the app's `28:app:…`, `sourceAppDisplayName` "Calendar", `sourceThreadId` empty) and
  names the person behind it in `activityContext.attributedToActorId` (and `attributedToActor`, JSON text with their
  name). Its `activityContext.location` is JSON text whose `Custom` holds
  `<DateTimeRange><StartDateTime>…Z</StartDateTime><EndDateTime>…Z</EndDateTime></DateTimeRange>`: the meeting's time,
  which the pane's third line shows ("Nov 11, 4:30 PM - 5:00 PM"). Its `activity.customTapAction` is the meeting's
  details link (`l/meeting/details/?eventId=…`, the id percent-encoded; every item carries a `customTapAction`, a
  message's being its `l/message/…` link). Which id form `eventId` takes wasn't captured: the native client matches it
  against the calendar's `objectId`, then the meeting's time, then an occurrence's `seriesMasterId`.
- **App items** carry `activityContext.teamsAppId`. The pane draws them with the app's own icon and no kind badge
  (Workflows' tile; Teams' own for "A team owner added you to …"); the app definitions Teams reads
  (`aggregatedEntitlements`, `batchedDefinitions`) carry `largeImageUrl`, a picture on `res.cdn.office.net`
  (`teamsappdata/evergreen-assets/apps/<id>_largeImage.png?v=<version>`, `teamsappdata/app-assets/<ring>/<id>/…`,
  `teamsappdata/ais_prod_v1/evergreen-assets/…/<id>_largeimage.png`), or for older definitions on
  `statics.teams.cdn.office.net` (`app-assets/…`, `evergreen-assets/apps/…`): a public 192×192 PNG, its picture inside
  a transparent margin, which the CDN answers as `application/octet-stream` and reads with no credential (checked
  once by hand on Workflows' tile). The pane draws it at the avatar's size in a rounded square with a hairline border
  (a white one in Teams Light). The native client reads it the same way (`AppPicture`) and draws the app's initials
  with its badge while it has none.
- **An item's second menu group** (team-manage, 2026-11-12, account a): "Notification settings" opens the settings at
  "Notifications and activity" and sends nothing. "Turn off <app>" (an app item's, in red) sent `PUT
  v1/users/ME/properties?name=userGraphSettings` with `{"userGraphSettings": "<JSON text>"}`: `{"<teamsAppId>":
  {"activityGenerationSettings": {<each activity type of the app>: false, "systemDefault": false}, "emailSettings": {},
  "toastSettings": {…the same…}}}`, Workflows' types being `messageNotify` and `customNotification` (the app
  manifest's `activities.activityTypes`). An unheld run on 2026-11-12 (task 27, account a) answered 200: the menu
  closed, existing app items stayed, and a toast said "Successfully turned-off setting" with Undo; no dialog asks
  first. In Notifications and activity → Apps, all three Workflows categories then read Off. The run restored the
  app with its switch: its declared activity types and `systemDefault` were true under generation settings, and
  toast settings stayed false (Show in Activity). All three setting writes answered 200. The native row is offered only when `activityContext.teamsAppId` is present, outside
  read-only mode (Ctrl+Alt+Shift+O acts on the selected item). It reads that id's definition (cached per account) and the current own properties (`GET
  v1/users/ME/properties`), then merges the declared types and `systemDefault: false` into the existing app's
  generation and toast settings. Other apps, email settings and unknown fields survive. Teams' preferences sync
  reads the own-properties endpoint and caches the categories, with later user-property deltas updating that cache
  (client preferences manager and preferences-sync job, inspected 2026-11-12). The native write reads it fresh:
  an absent property, or 404 with error code 404 and standardized error code 404/subcode 1, starts an empty setting;
  other errors or a malformed setting send nothing. The outbox retains the app and declared
  types, rereads preferences before each permitted attempt, and never automatically resends an unknown outcome.
  On confirmation the native pane keeps the items and shows the captured toast with Undo; a failure shows the
  existing error toast. Undo restores this app's exact saved preference (including absence) into a fresh whole
  setting, preserving changes to other apps since Turn off. The snapshot lasts for the engine's run; the toast is
  transient.
- **An app item's banner**: the web client (its cached scripts, read 2026-11-13) decides an app activity's banner per
  activity type from `userGraphSettings`: `<teamsAppId>.toastSettings.<activitySubtype>` and the same type under
  `activityGenerationSettings`, each `true` when the app saved no value for it. The service's per-app
  `activityCustomizations.defaultAppSettings` can make an app's default "Show in Activity" (`OnlyFeed`); with none,
  the default is "Show in Activity and banner". The Apps settings' dropdowns save "Show in Activity and banner" as
  toast and generation `true`, "Show in Activity" as toast `false`, "Off" as both `false`. Its banner carries the
  activity's title and text. The native client shows a new app item (one delivered, or a catch-up's unread one after
  the notifier's cutoff) as a banner while the "notifications" setting is all and the app's preferences, read fresh
  then, keep its type's banner on; one whose preferences can't be read shows none. Its title is the item's (the app's
  own, else the app's name), its body the preview; a click opens the Activity pane at the item, as a click there does,
  not at the newest item a plain open picks.
  How browser Teams draws the banner (icon, header) was not captured; the native one takes the chat banner's form.
- **Opening the pane opens its newest item** (team-manage, 2026-11-13, account a): each time the rail's Activity is
  chosen from elsewhere (Chat, say), the chip lets go and the newest item is selected (`aria-selected`) and opened
  beside the pane, whichever item was chosen last time; a chip chosen opens nothing. The newest was a missed call: the
  Calls page opened with its History filtered to Missed, which put `48:calllogs`'s `consumptionhorizon` (the call
  log's own horizon), besides the feed's. A missed call's menu offered "Call back", "Chat" and "Remove", no read
  mark. Whether this open marks an unread newest item read wasn't seen (the newest was read, and a missed call can't
  be marked unread).
- **A chat on screen**: an item whose chat was open when it arrived was marked read at once (see "Seen beside it");
  the native client marks unread items on entry and new arrivals while that conversation is shown in the selected
  account's active window. A manual unread mark survives reloads until the conversation is opened again.

The native client: [`teams_core::activity`](../../crates/teams-core/src/activity.rs).

## Read horizons

- The account's own horizon is the conversation property `consumptionhorizon`, `<arrival id>;<ms>;<clientmessageid>`:
  the last message read (its id and its `clientmessageid`) and when the horizon was set. a's client set it with `PUT
  v1/users/ME/conversations/<chat>/properties?name=consumptionhorizon` (`{"consumptionhorizon": "…"}`, answered 200
  with an empty body) after its own posts; the conversation list carries it in `properties`.
- A receipt names a message's `clientmessageid`, and every message that counts as unread has one. In the lab
  accounts' synced stores (read 2026-11-17 for shape only), each bot message (`28:` senders, the Workflows bot's
  cards included) carries a random numeric id of 17 to 20 digits. Only thread events (`ThreadActivity/…`) have none.
  Thus a bot chat is marked read as any chat is.
- `GET v1/threads/<chat>/consumptionhorizons` answers `{id, version, consumptionhorizons: []}`: for the lab chat the
  list was empty both times it was read (the members' horizons that others may see, when read receipts are on).
- A page sends its receipt only while it is in use. In the first run b's page sent none while it showed a's messages;
  in the confirming run it sent none within 30 s of showing them and focusing its compose box, and sent one during its
  reaction, once the pointer was over a message. a's page sent its receipts as it typed and posted.

## Seen beside it

- Own properties the client puts, denied by the fence of the time (a carve-out under the lab rule): `messageDensity`
  (hundreds of retries a run), `favorites`, `locale`, `firstLoginInformation`, and `readReceiptsEnabled`, new in this run.
- Whenever an editor opens (the compose box, the mention picker, an edit box), the client asks its message
  extensions' bots for their commands: `POST v1/agents/28:<id>/invoke` with `name` `composeExtension/fetchCommands`, to
  two bots ([bots.md](bots.md)). The fence of the time denied them as unknown.
- Once b's reaction reached a's activity feed, a's client marked the feed item read: `PUT
  v1/users/ME/conversations/48%3Anotifications/messages/<id>/properties?name=isread` with `{"isread": "true"}`, retried
  nine times while denied. A carve-out under the lab rule (the account's own feed): it passes.
- `GET v1/users/ME/properties` answers 404, and so does `v1/users/ME/conversations/48:calllogs` for the lab accounts.

## The call log

Read from a lab account's synced store of 2026-11-16 (84 call entries from the lab's call runs, besides recording
and transcript records); no value is copied here. `48:calllogs` (`threadType` `streamofcalllogs`) holds one `Text`
message per call the account was in or rang for. Its sender is the call's caller (its `from`), its content reads
"Call Logs for Call <call id>", its arrival is the call's end (within a few seconds), and its `properties` are
`call-log` (the call, a JSON object as text) and `s2spartnername` `concore_gvc`. A meeting's call leaves no entry:
one lab account's log, which holds every call around them, has none for the three meetings (`19:meeting_…` chats)
whose calls it joined from 2026-11-06 to 2026-11-11. The call:

- `callId`, `startTime`, `connectTime` (null for a call the account never took), `endTime` (ISO times, 7 digits).
- `callDirection` `outgoing` (the account placed it) or `incoming`; `callType` `twoParty` or `multiParty`;
  `callState` `accepted`, `declined` (only incoming ones seen) or `missed` (the caller's unanswered calls too: no
  outgoing entry was `declined`, though the lab's runs had declines); `sessionType` `default`.
- `originator` (the caller's MRI) and `originatorParticipant`, `target` and `targetParticipant` (the callee in a 1:1
  call, the account in most group entries, null in some): each participant `{id, type, displayName, tenantId,
  alternateId, applicationType}`, `type` `default` (a voicemail's `voicemail`), only the originator's
  `displayName` set.
- `participants` (MRIs) and `participantList` (participants) for a group call, null for a 1:1 call; `threadId` and
  `messageId` (the group chat and, it seems, its call event) on some group entries.
- `userParticipantId` (a GUID of the entry's own), `sharedCorrelationId` (a GUID, not the call id),
  `forwardingInfo` (`{forwardingType: "unanswered", forwardingDestinationType: "voicemail", acceptedBy}` when the
  call went to voicemail), `transferInfo` (a park or a transfer), `callAttributes` (null in all).

A missed call's item in the activity feed (the same store): `activityType` `call`, `activitySubtype` `missedCall`,
the caller as `sourceUserId` and `sourceUserImDisplayName`, `sourceThreadId` empty, `sourceMessageId` `1`, an empty
`messagePreview` with `messagePreviewTemplateOption` `previewUnavailable`, and an `activityContext` with
`callModalities` (`Audio`), the Calls clump (`ClumpId` `nr:system:clump:Calls`, `ClumpType` and `ClumpTitle`
`Calls`), `partnerAppId`, `activityVersion` and `activityProcessingLatency`; empty content, `isread` false, the
`skypespaces` partner. The native client reads it as "Missed call from <caller>" (`teams_core::activity`) and reads
nothing of the call log's entries yet.
