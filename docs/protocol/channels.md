# Teams and channels

Status: 2026-10-22, M2 Task 16. Written from `proto-lab probe teams a`, run live once as lab account a (its own
`chatsvcagg` and `ic3` tokens, `GET`s only, under the live lock), which printed statuses, header names, content types,
sizes, the answers' key paths with their JSON types, counts, and two protocol enums' plain values (a thread's
`threadType`, a message's `messagetype`), never a name, a text or an id, and kept nothing; and from the web client's
captures of the same reads (`ab-chat`, `abc-chat`, `load`: method, path pattern, query names and flag values, header
names, statuses and key shapes only). The lab accounts are members of two teams of the tenant that people outside the
lab are in too, so nothing of them is written here. The native client's read is in `crates/teams-proto/src/read.rs`
(`CsaTeams`), the transport's check in `crates/teams-proto/src/http.rs` (`Service::Csa`), its use in
`crates/teams-core/src/teams.rs`, and the views in `crates/teams-ui/src/{teams_list.rs,channel_view.rs}`.

## Where the web client gets them

The web client lists the account's teams and channels from CSA, the chat service aggregator, at the region's GTM
`chatSvcAggAfd` (`https://<AFD host>/api/csa/<region>`, `amer` for the lab):

- `GET api/v3/teams/users/me` with `isPrefetch=false`, `enableMembershipSummary=true`,
  `supportsAdditionalSystemGeneratedFolders=true`, `supportsSliceItems=true`, `enableEngageCommunities=false`, in
  that order, in every capture of the full read: the account's teams with their channels, its chats and more;
- `GET api/v3/teams/users/me/updates` with the same flags (and `migratePinnedToFavorites` in some), what changed since a
  sync token (`x-ms-synctoken`);
- a channel's posts through CSA too: `GET api/v1/containers/<channel>/posts` (`modality`, `pageSize`, `teamId`,
  `includeRcMetadata`, `filterSystemMessage`, `shouldIncludeSharedToL1Rc`, or `threadedPostsOnly`), answered 207 with
  `posts` (each a post with its `replies.messages` and `totalCount`), `hasMoreForward`, `hasMoreBackward`; and
  `…/teams/<team>/channels/<channel>/pins`, `…/systemmessages`.

Its requests carry many `x-ms-…` headers (`x-ms-client-type`, `x-ms-client-version`, `x-ms-partition`,
`x-ms-region`, `x-ms-session-id`, `x-ms-request-priority`, …) and the account's token for
`https://chatsvcagg.teams.microsoft.com` ([auth.md](auth.md#audience--service-table)).

## What the probe read

### The token

The native client (`1fec8e78-…`) gets a `chatsvcagg` token from the account's refresh token like any other audience:
`aud https://chatsvcagg.teams.microsoft.com`, scope `user_impersonation`. CSA took it with nothing else: the probe sent
`Authorization: Bearer <token>` and `Accept: application/json`, no `x-ms-…` header.

### CSA's teams read

`GET <chatSvcAggAfd>/api/v3/teams/users/me` with the web client's query, as above: **200**, `application/json`, about
32 KB; header names `cache-control`, `content-length`, `content-type`, `date`, `pragma`, `request-context`,
`timing-allow-origin`, `x-cache`, `x-content-type-options`, `x-msedge-ref`.

- Top level: `teams`, `chats`, `users`, `privateFeeds`, `engageCommunities` (arrays), `conversationFolders` (an object:
  folders, their order, slices metadata) and `metadata` (`syncToken`, `forwardSyncToken` `null`, `isPartialData`,
  `hasMoreChats`, `coldBootTime`).
- A team: `id` (a `19:…@thread.tacv2` id, its general channel's), `displayName`, `description`, `channels`,
  `isArchived`, `isDeleted`, `isFavorite`, `isCollapsed`, `isTenantWide`, `isGeneralChannelFavorite` and other flags,
  `memberRole`/`memberRoleString`, `membershipSummary` (counts), `rosterVersion` and `membershipVersion` (numbers),
  `threadVersion` and `threadSchemaVersion` (strings), `smtpAddress`, `tenantId`, `teamSettings`, `teamGuestSettings`,
  `teamSiteInformation`, `teamStatus`, `extensionDefinition`, … .
- A channel: `id`, `displayName`, `description`, `parentTeamId`, `threadType`, `threadSubType`, `channelType` (a
  number), `modality`, `isGeneral`, `isArchived`, `isDeleted`, `isMember`, `isFavorite`, `isPinned`, `isShared`,
  `isMuted` and other flags, `lastMessage` (an object), `version`, `threadVersion`, `membershipVersion` (numbers),
  `memberRole`, `creator`, `creationTime`, `lastJoinAt`, `tabs`, `connectorProfiles`, `defaultFileSettings`, … .
- Counts: 2 teams, 10 channels; each team's general channel is the one whose id is the team's, with `isGeneral`
  `true` (2); one team and one channel archived; none deleted.

### The channels on the chat service

A channel is a conversation of the chat service, read with the same reads as a chat:

- **The conversation list** (`v1/users/ME/conversations`) held 2 of the 10 channels: the teams' general channels
  (`threadType` `space`, [chat.md](chat.md#the-conversation-list-probe)). A team's other channels are in no list, so
  no catch-up reaches them; they are read by their id.
- **A channel's thread** (`GET v1/threads/<channel>?view=msnp24Equivalent`): 200, `threadType` `space` for a general
  channel and **`topic`** for another (no `productThreadType` in either), one entry in `members`, `rosterSummary`
  counts, and many `properties` (`spaceId`, `topic`, `topicThreadTopic`, `groupId`, `sharepointChannelDocsFolder`, …).
- **Its messages** (`GET v1/users/ME/conversations/<channel>/messages?view=msnp24Equivalent|supportsMessageProperties&
  pageSize=200`): 200, `messages`, `tenantId` and `_metadata` (`lastCompleteSegmentStartTime`,
  `lastCompleteSegmentEndTime`, `syncState`), as a chat's page. The general channel's page held 48 messages
  (`RichText/Html` 14, `Text` 4, and thread events: `ThreadActivity/AddMember` 12, `DeleteMember` 12, `RoleUpdate` 2,
  `TopicTopicUpdated` 2, `ModalityUpdate` 1, `SpaceThreadChannelAdded` 1); a standard channel's 28 (`RichText/Html` 21,
  `Text` 5, `ModalityUpdate` 1, `UpdateFavDefault` 1). A message has a chat message's keys
  ([chat.md](chat.md#a-message-as-read)) and `s2spartnername`, `fromGivenNameInToken`, `fromFamilyNameInToken`,
  `origincontextid`, and on some `rootMessageId` (a string); `properties` adds `announceViaEmail`, `atp`, `blurHash`,
  `deltaEmotions`, `pinned`, `skipfanouttobots`.
- **Reply chains**: every message's `conversationid` was the channel's, and every message's `conversationLink` ended
  in `;messageid=<post>`: the post a reply belongs to, a post's own id for a post (an event too). No message had
  `properties.parentmessageid`. The channel's page holds posts and replies together, newest first.
- **One post's chain** (`GET v1/users/ME/conversations/<channel>;messageid=<post>/messages`, the same query): 200, the
  post and its replies (24 messages for one post, 1 for an event), each with that link.

## The channel's own events

A channel's thread carries its own events among its posts (`ThreadActivity/…`: people added or gone, the channel
renamed or its description changed, apps and tabs). The web client never draws them in the post list: every captured
read of a channel's posts asked CSA for `filterSystemMessage=true`, and the bundle words them only in the channel's
info pane (its "channel update" renderer): "<name> changed channel name from <old> to <new>." ("Channel name was
changed from <old> to <new>." when the system did it) and "<name> changed the channel description.". The native
client reads a channel from the chat service, which holds them, and draws none of them (`channel_view`); it has no
info pane.

## A post that came by email

An email sent to a channel's address arrives as a channel post on the chat service. The web client's reader
(its message resolvers, read in a stored bundle, 2026-11-17) takes these keys of the post's `properties`:

- `subject`: the post's subject, as text. The web client draws it bold over the text of any post that has one.
- `emaildetails`: a JSON string of an object. A post with it is an email post (`EmailPostMessage`). Its keys:
  `from` and `sender` (each `{name, smtp}`), `to` and `cc` (lists of them), `emailLink` and `totalRecipients`.
  The reader keeps the first 3 recipients, the `to` list first, then `cc`. Without `totalRecipients`, it counts
  `to` and `cc`.
- The sender's name is "<name> via email" (`tmp_message_via_email`).
- "View original email" shows only when `emailLink` is present. A click asks the files service to redeem the link
  (`redeemFile`, a download link of the SharePoint file), then downloads the file (`downloadFile`). The file is the
  original `.eml` in the team's SharePoint.

No received email post was captured. The fixture `tests/fixtures/chatsvc/email-post.json` is synthetic and follows
the reader's shape. The native client reads both keys in `teams_proto::chat` (`subject`, `EmailDetails`). It keeps
the whole email link, at most 4096 bytes, only when the link is an `https` link on a SharePoint host, or an `https`
link to a file service path (`/sp/`). A SharePoint link opens in the browser with `download=1`, so the browser
downloads the file. Another link is saved through the account's engine, which refuses a link that is not on its own
server's file service. A line under the row shows the progress of that download, and its failure.

## What the native client reads

- **The teams**: `GET <CSA base>/api/v3/teams/users/me` with the web client's query exactly, the account's
  `chatsvcagg` token and `Accept: application/json`, nothing else (`Service::Csa`, `CsaTeams`). The transport takes a
  CSA base only of the GTM's form (`https://<AFD host>/api/csa/<region>`, given by `HttpTransport::with_csa`) and
  refuses any other CSA endpoint or query (the update read, the posts read, discover, pins, …) and anything but a
  `GET` before it asks for a token. Of the answer it reads only the teams: a team's and a channel's `id`,
  `displayName`, `isDeleted` (left out), `isArchived` (a team's) and `isGeneral` (or the channel whose id is the
  team's), a channel's `channelType`, and what a new channel needs of a team (`teamSiteInformation.groupId`,
  `smtpAddress`, `memberRoleString`, `teamSettings`, `teamGuestSettings`: [Creating a channel](#creating-a-channel)); an entry without a string id and name is left out; the general channel first. The answer is taken when
  `metadata.isPartialData` is `false`, as in every lab capture, or when it is `true` with `metadata.hasMoreChats`
  `true`: a personal build's account with a few hundred chats was answered so every time (12 teams, its `chats`
  264 of them, `conversationFolders` with `isPartialResponse` `false`), and the web bundle ties the flag to the chats
  alone (`partialDataStartupChatsLimit`, and `shouldSkipGroupChatSyncBasedOnIsPartialData`: the group chats are read
  from the chat service after a partial answer), so its teams and folders are whole. One that says it is partial
  otherwise, or doesn't say (no `metadata`, no flag, a flag that isn't a boolean), is refused as malformed, since the
  store replaces its list with what a read gives; the stored list and its read time stay, and the read is tried again
  after the engine's retry wait. `chats`, `users` and the rest aren't read: the chat list stays the chat service's;
  of `conversationFolders` only the favourites are ([chat.md](chat.md#favorites-empty-chats-and-the-chat-list)).
  `teams-core` reads it in the background when a view asks and the stored list is missing or older than an hour
  (`TEAMS_MAX_AGE`), keeps it in the store whole and tells a change as `Event::Teams`. It reads it again at once when
  a read or a delivery brings a team's thread no stored team names (the account was added to a team; once per thread a
  run), or a team's activity is delivered on its thread (`ThreadActivity/…`: a channel made comes to everyone on the
  team as `SpaceThreadChannelAdded`, seen 2026-11-03): without it a team the account was just added to, or its new
  channel, stayed missing for up to an hour.
- **A channel's posts**: the chat service's message reads, as a chat's (`ChatReads::messages` and its links), on demand
  when the channel is opened (`EngineRequests::sync_conversation`), older pages on "Load older posts". A channel the
  store doesn't know is read as a thread first (`topic` and `space` are both a channel). Each stored
  message's post is kept beside it from its `conversationLink` (`Message::reply_root`, only when the link names that
  channel and the post is digits); a message without one is shown as a post of its own. The store's schema 9 started
  the general channels' syncs again, so their messages stored before get their posts.
- **Not used**: one post's chain read (the channel's page holds the replies), the thread's members (no member is
  stored for a channel), CSA's posts read and its update read.

## Creating a channel

From `proto-lab run channel-create` (2026-11-02): account a in the web client's "Create a channel" dialog of a team of
the tenant, every write that could make a channel held back by a forced deny, so nothing was ever made; the answers below
are the web bundle's (`26260.1717.5229.7049`) reading of them, since at the time no lab team could be written to.
`channel-create`'s writing mode (`PROTO_LAB_CHANNEL_WRITES=1`) makes channels on a lab team after the agent's reach check. The native client's writes are
`crates/teams-proto/src/write/channel.rs`, its reads `channel_read.rs`, the engine's part `teams-core`'s `channels`, the
dialog `teams-ui`'s `channel_dialog`.

**The team's "..." menu** offered a member of a team "Hide all channels", "Add channel", "Manage team", "Manage tags" and
"Copy link"; "Add channel" isn't there for an archived team. The web client decides as `CsaTeams` reads it: an owner
(`memberRoleString` `admin`) always, a member by `teamSettings.createTopic`, a guest by `teamGuestSettings.createTopic`;
a private channel for an owner, or a member with `teamSettings.createTopic` and
`teamSettings.isPrivateChannelCreationEnabled`. The channel types offered were Standard and Private
(`isSharedChannelCreationEnabled` `false`, and shared channels aren't offered here).

**The dialog's rules** (the bundle's, its words in `channel_dialog.rs`): a name of at most 50 characters, trimmed;
refused when empty, starting with `_` or `.`, ending with `.`, holding any of `~ # % & * { } / \ : < > ? + | ' "` or
`..`, one of the reserved words (`forms`, `.lock`, `con`, `prn`, `aux`, `nul`, `_vti_`, `desktop.ini`, `com1`-`com8`,
`lpt1`-`lpt9`), `General` in any case, a name the team's channels have in any case, or a link. A description of at most
1024 characters without a link. A problem shows once the name or the description was touched; Create is on when both
read, a type is chosen and something was touched. Threads is chosen at first. An owner choosing Standard also gets
"Automatically show this channel in everyone's channel list". The Layout cards name `chatModalityType`.

**A standard channel** goes on the middle tier, its token and headers its own:

```
POST <middleTier>/beta/teams/<team thread>/channels
{"displayName": …, "description": …, "groupId": <team group>, "channelType": "Standard",
 "chatModalityType": "Conversational" | "PostReply"}
```

**Seen live** (`channel-create` writing on the lab's own team, 2026-11-03), for Threads and Posts alike: 201 with
`{"value": {"objectId": <channel thread>, "createdDateTime", "displayName", "description", "groupId": <team group>,
"channelState": "Active", "isFavoriteByDefault": false, "chatModalityType"}, "type": …}`.

**A private channel** goes on the teams' provisioning service, the GTM `teamsAndChannelsProvisioningService`
(`https://<AFD host>/fabric/<region>/templates/api`), with a token for `https://templates.teams.microsoft.com` (the
native client gets one, an opaque token) and `x-anchormailbox: <the team's smtpAddress>`:

```
POST <provisioning>/teams/<team group>/channels?forceSync=false
{"spaceThreadId": <team thread>, "displayName": …, "description": …, "channelType": "Private",
 "sharedWithParentTeam": true, "sessionKey": <uuid>, "chatModalityType": …}
```

Before it, as the type is picked, the web client sent `POST <provisioning>/teams/<team group>/channelsWarmup` with no body
and the same key in `x-routingparameter-sessionkey`. The bundle attaches the key only behind a flag; the native client
sends neither. **Seen live**: 200 with `{"groupId": "OID:<channel group>@<tenant>", "displayName", "description",
"hostTeamId": <team group>, "hostTenantId", "createdTime", "chatModalityType"}`, no `threadId`: the thread can
come later, from `GET <provisioning>/teams/<team group>/channels/OID:<channel group>@<tenant>` (`threadId`), read every 4 s.

**The step after a private channel** is the add-people dialog, in the create flow with Skip: "Add members to the
<name> channel", "Start typing a name of a person you want to add to this private channel.", a box "Type a name or email"
scoped to the team, and Add. People go in by the channel's group, before its thread exists:

```
POST <provisioning>/teams/<team group>/channels/OID:<channel group>@<tenant>/users?forceSync=false
{"users": [{"id": "OID:<object id>@<tenant>", "role": 1}]}
```

**Seen live** adding b: 200 with `{"multipleStatus": [], "asyncOperations": []}`; the bundle reads `multipleStatus` as those
that failed (`id`, `statusSubCode`, `message`). No error code was seen live: every write here succeeded. After each creation
the web client sets the new channel a favourite of the account's (`PUT v1/users/ME/conversations/<channel>/properties?name=favorite&requirePermanentJoinedState=true`,
`{"favorite": "true"}`, its own list alone) and marks it read (`consumptionhorizon`). The box's search is a read sent as
a `POST` on the user profile service (GTM `userProfileService`, `https://<AFD host>/api/userprofilesvc/<region>`, the
middle tier's token): `api/v1.0/teams/<team thread>/users/profilesearch?top=15&includeDLs=false&includeBots=false&
enableGuest=true` with `{"keyword": …}`, answered with a list of people (`mri`, `displayName`, `email`, `jobTitle`,
`tenantId`, `objectId`, …): a mention in one of the team's channels asked it so in the capture, the bundle's add-people box
scoping its search to the team the same way. Sent once from Rust as account a (2026-11-02): it answered 200 with the
capture's `x-ms-client-type`, `x-ms-migration`, `x-ms-object-id`, `x-ms-test-user` and `x-client-ui-language`, and 400
without them.

**Shown to everyone**: `PUT <middleTier>/beta/teams/<channel thread>/setDefaultChannel`, no body (`DELETE` takes it back).

**What goes wrong**: the middle tier names an error code (`errorCode`, or `errors[0].errorCode`); the bundle's dialog
replaces the form with its words for each (`problem_words`): `ChannelNameAlreadyExist`,
`MaxPrivateChannelLimitExceeded`, `MaxChannelLimitExceeded`, `UnableToFetchTeamOwner`, `NoPermissionToCreateChannel`,
`UserNotAllowedToCreateChannel`, `CannotCreatePrivateChannelDueToUserPolicy`, `OperationNotAllowedOnArchivedTeam`,
`InvalidUsersInCreateChannelRequest`, `ThreadDescriptionLimitExceeded`. Anything else says "We ran into a problem creating
your channel" with "Try again" (the capture's forced deny showed it).

**The list**: a channel made comes from CSA's next read, which may not name it yet; the native client stores it at once
under its team, by name, and keeps it against reads that don't name it for ten minutes. A private channel's row carries
a lock (`channelType` 1; 0 is standard, 2 shared).

A channel's creation and membership changes reach the team/channel's people. For lab tests the agent must include
them in its reach check. Native typed channel requests use the outbox; a browser capture hold can record the matching
request without creating a channel. Neither path has a participant-policy classifier.

## Posting, replying and reacting in a channel

A channel is written as a chat is, through the outbox's typed chat writes (`teams-core`'s `Command`), from the channel's
compose boxes (`teams-ui`'s `chat_view` in its channel places, `channel_view`):

- **A new post**: `SendMessage` to the channel, `POST v1/users/ME/conversations/<channel>/messages`, the message as a
  chat's. The service names a post as its own chain's post (`conversationLink` `…;messageid=<its id>`), so it reads back
  as a post.
- **A reply in a post's thread**: `SendMessage` to the post's chain, `<channel>;messageid=<post>` (the id the web client
  joins, [chat-service.md](chat-service.md#ids); `teams_proto::reply_chain_id`), `POST …/conversations/<chain>/messages`.
  The engine takes one only under a stored post that isn't deleted. The reply comes back in the channel's messages
  (`conversationid` the channel's, `conversationLink` naming the post), so the outbox looks for it, checks an unknown
  outcome and syncs after the write in the channel (`teams_proto::stored_conversation`), never the chain.
- **Edits, deletes and reactions** go to the message in the channel (`…/conversations/<channel>/messages/<id>…`), a
  post's and a reply's alike, as a chat's.
- **Files**: uploaded into the channel's folder on its team's site, then named in the post or the reply
  ([Files in a channel](#files-in-a-channel)).
- **Not written**: typing (no capture showed a channel's), the read horizon (a channel's view sends no read receipt)
  and Teams' "Send to: thread and channel" (a reply also posted to the channel, a write no capture showed: the
  thread's box says "Send to: thread only").

The native writes ran live on a lab team with one person (the lab account itself): the post answered 201 and read back
naming itself as its chain's post; the reply posted to `<channel>;messageid=<post>/messages` answered 201 and read
back in the channel's messages under its post; a reaction on each and the reply's edit, on the channel's message
paths, answered 200. The web client's own shapes for these writes weren't captured yet (extra keys it may send are in
the backlog); the mock (`teams-mock`) takes a channel's posts and stores a reply posted to a post's chain with the
channel's messages, as the service did.

## Files in a channel

Seen 2026-11-22 in Teams on the web (`proto-lab run team-manage`, lab account a, a lab team whose only member is a,
its General channel in the classic Posts layout). Lab account a attached a made-up text file to a new post and to a
reply, dropped and pasted made-up files, attached a file whose name the folder had already, and removed one.

**The folder.** A channel's files go into the channel's own folder in the document library of its team's SharePoint
site, not into the account's OneDrive. The channel's thread on the chat service names the folder (`GET
v1/threads/<channel>?view=msnp24Equivalent`, and the same keys in `childThreads`): `properties.sharepointSiteUrl`
(`https://<tenant>.sharepoint.com/sites/<site>`), `sharepointRootLibrary` (`Shared Documents`),
`sharepointChannelDocsFolder` (`General`, the folder's name) and `channelDocsFolderRelativeUrl` (the folder's
server-relative path, `/sites/<site>/Shared Documents/General`). The provisioning service's channel list also gives
the folder's address (`sharepointUrl`).

**The upload.** Three `POST`s on the team's site, with a token for the site's origin (`https://<tenant>.sharepoint.com`)
and the headers `accept: application/json`, `application: Teams_Web`, `scenario: ShareUploadFile`, `scenariotype: AUO`:

1. `<site>/_api/web/GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)?@v='<folder path>'&@fn='<name>'` (no body):
   an empty file of the name. 200 with the file (`SP.File`: `UniqueId`, `Name`, `ServerRelativeUrl`, `Length` `"0"`,
   `TimeCreated`, …). When the folder has the name, 400 with `odata.error.code` `-2130575257,
   Microsoft.SharePoint.SPException` ("A file with the name … already exists"); the web client then tries
   `<name> 1.<extension>`, so a file never replaces another. The `OData` strings are in quotes and the query values are
   percent-encoded (`%27%2Fsites%2F…%20Documents%2FGeneral%27`).
2. `<site>/_api/web/GetFileById('<UniqueId>')/StartUpload(uploadId=guid'<a fresh GUID>')?@folder='<folder
   path>'&@file='<name>'`, `content-type: application/octet-stream`, the file's bytes: 200, `{"value":"<bytes
   taken>"}`.
3. `<site>/_api/web/GetFileById('<UniqueId>')/FinishUpload(uploadId=guid'<the same GUID>',fileOffset=<length>)`, the
   same `@folder` and `@file`, no body: 200 with the file, its `Length` now the file's.

Then the web client reads the file's thumbnail from the site (`_api/v2.1/shares/u!<the file's address>/thumbnails/…`).
Nothing is shared and no grant is written: the team reaches the file through its site.

**The post.** The ordinary `POST v1/users/ME/conversations/<channel>/messages` (a reply: `<channel>;messageid=<post>`,
the body with `replyChainId`), 201. `properties.files` is a JSON string of a list with one entry for each file:
`itemid` and `id` the file's `UniqueId`; `fileName`, `title`, `fileType`, `type`; `fileInfo` with `itemId` `null`,
`fileUrl` (the site's origin and the server-relative path, the spaces not encoded), `siteUrl` (the site, no `/` at
the end), an empty `serverRelativeUrl`, `shareUrl` and `shareId` `null`; `fileChicletState` `serviceName` `teams`,
`state` `active`; `baseUrl` (the site) and `objectUrl` (as `fileUrl`); `permissionScope`, `sharepointIds`,
`publication`, `site`, `altText`, `isUploadError`, `progressComplete` `null`; `version` `2`; an empty `filePreview`.
A new post also had `postType` `Standard`, `subject` `""` and `crossPostChannels` `[]`. The push frames of the post
added `properties.atp` (the file's address, encoded).

**What the web client shows.**

- "Attach files" (a paper clip, in a new post's box and in the reply box) opens a menu: "Attach cloud files", "Browse
  Teams and Channels", "Upload from this device".
- The box shows a card for each file: the type's icon, the name in bold and a remove mark (X) in its header, then a
  preview (a text file's first lines). While the file uploads, a thin line under the header shows the progress.
- After the post, a toast at the bottom left says "You've shared <file name>". The post shows the card with Share and
  "More options".
- A file dragged over a channel's box highlights the box alone (not the whole channel): the drop picture and "Share
  in <channel name>" ("Share in General").
- A file dropped with no file data (a made-up drop) gave the red line "Some files couldn't be uploaded because they're
  missing necessary data" over the box, with a close mark.
- A pasted file that isn't a picture attaches nothing, as in a chat.
- A file whose name the folder had got the yellow line "Uploading a copy because a file with the same name is shared
  in this channel." with "Reshare older file instead" and a close mark; the card had the numbered name.
- Remove (X) on a card sends the uploaded file to the site's recycle bin: `DELETE
  <site>/_api/web/GetFileById('<UniqueId>')/recycle`, 200.

**The native client** (`teams_proto::channel_file`, `teams-core`'s `channel_files`) uploads the same way: it reads the
folder from the channel's thread, takes only a tenant site (`/sites/` or `/teams/`, never a personal site) and a folder
inside it, and sends `addStub` (numbered as the web client numbers it while the name is taken), `StartUpload` with all
the bytes and `FinishUpload`, each once through the outbox. Before each write the transport reads the channel's thread
again and refuses a write whose folder isn't the channel's now; before a write that names a file, it reads the file by
its id and refuses one that isn't right in that folder. A failed step after `addStub` recycles the empty file. Remove
recycles the file. The engine takes a channel file only in a post or a reply of its own channel, and an own-drive file
only in a chat. It shows Teams' copy notice without "Reshare older file instead" (that action wasn't captured).

## Managing an existing channel

The channel ellipsis menu follows the supplied reference: notifications; a Move to section submenu (Favorites,
custom sections and New section); Hide/Show; Edit; Manage; Copy link; Get email address; Workflows; Archive/Restore;
and Delete. The same channel row is used in the team tree, Favorites and custom sections. Right click and
Shift+F10 open the same keyboard-navigable, scrolling menu.

A fresh CSA team/channel snapshot precedes a shared channel change. The owning team, Microsoft 365 group, channel
privacy and current permissions must still agree with the chosen target. Already-applied name/description,
archive-state or deletion changes reconcile without another request. General cannot be archived/deleted. Standard
channel administration uses the team-owner role; private/shared administration uses the channel-owner role, and
private membership never becomes team membership. Per-person provisioning failures carried inside a 2xx response
remain errors. An uncertain write is not automatically resent.

Standard edits/deletes use the middle tier's `beta/teams/<team>/channels/<channel>`. Private/shared edits/deletes
use provisioning's `teams/<team group>/channels/OID:<channel group>@<tenant>`; membership operations append `users`
and, for a removal, the person's `OID:<object ID>@<tenant>`. Member role is 1, owner role 2. Moderator changes use
provisioning's `channels/<channel>/members` with work-account object IDs. The dialog keeps the last owner until
another owner is present. Archive and restore use provisioning's owning-group/channel-thread routes.

Manage channel separates members from settings. Standard channels inherit the team's people and let channel owners
manage moderators and posting/reply/pinning/bot/connector permissions. General's posting policy lives in the
team's `spaceAdminSettings.userSettings.generalChannelSetting`; other standard channels use their own
`channelSettings` thread property. Both keep fields this build does not expose. Private/shared channels expose
their own membership rather than standard-channel moderation controls.

Channel notification settings are the account's own conversation properties, read with the channel's conversation
(`v1/users/ME/conversations/<channel>?view=msnp24Equivalent`). Seen 2026-11-06 with `proto-lab run team-manage`
steps, lab account a in its lab-only team's General channel (a the only person): the channel's "..." → Channel
notifications opens "Channel notification settings", with "<team> › <channel>" under the title; "Notify me for all
new messages" is a dropdown of "Show in Activity and banner", "Show in Activity" and "Off"; "Include thread replies"
is a checkbox, unchecked and unavailable while Off; under a rule, "Follow all new threads" with "Threads you
participate in or are @mentioned in are auto-followed. Choose where you get notified in settings." and a "Go to
Settings" link (Settings' Notifications), and an On/Off switch. Save stays unavailable until something changes.

What Save sends, each a `PUT …/properties?name=<property>` answered 200, and only for what changed (the follow switch
first):

| Choice | Property and value |
|---|---|
| Show in Activity and banner | `channelNotificationSettings`: `{"allNewPosts":"On","dskNotif":"On","includeReplies":<bool>}` |
| Show in Activity | `channelNotificationSettings`: `{"allNewPosts":"On","includeReplies":<bool>}` (no `dskNotif`) |
| Off | `channelNotificationSettings`: `{"allNewPosts":"Off","includeReplies":false}` |
| Follow all new threads on / off | `followAllRc`: the time it was turned on in epoch milliseconds, as text / empty text |

Reopened, the dialog shows what was saved. Teams' dialog writes no `alerts` property and no `tag` or `channelTeam`
keys; the classic client's `alerts` `false` (a muted channel) still keeps the desktop notifier quiet. The native
dialog keeps keys of `channelNotificationSettings` it doesn't know. The notifier shows a post that doesn't mention the
account only for "Show in Activity and banner", and such a reply only when replies are included or new threads are
followed. A failed read leaves the dialog unable to save, so a default never overwrites unread settings.

Channel email uses `beta/channels/<channel>/email`: a read returns `emailAddressDetails.emailId` and
`allowedSenders` (`allowedSenderType`, `allowedDomains`). `NotFound` starts members-only address creation; other
errors do not. Owners can change allowed senders/domains or remove the address; copy uses the desktop clipboard.

Sections are the account's CSA `conversationFolders`, with `folderHierarchyVersion`, `conversationFolderOrder`
and each folder's `conversationFolderItems`. Every operation reads a whole hierarchy first, rejects partial data
and stale/missing destinations, and submits that version. Creating a folder uses `AddFolder`; moving an item uses
`AddItem` and, when necessary, `RemoveItem`. Existing item order and section order stay server-owned. Expanded
state uses `batchUpdateExpandedState`, with an optimistic view rollback if saving fails.

Evidence: reference web-client bundles inspected outside the repository, synthetic protocol/context/store/UI tests,
and a normally built native read of the owned disposable channel's preferences (200, exact matching id). The
original reference team's full roster included people outside the lab and was never mutated. A new actor-only
private team automatically acquired three bot members; independent full-roster checks refused shared admin writes
on it. A second private team made through the native provisioning template route with explicit `apps: []`
also returned 201 and acquired the same three bots (fresh full roster read 200). No shared administration or
cleanup request was sent to either resource. These are historical implementation observations under the then-current policy. Current lab runs follow the
agent reach check in `docs/lab-rules.md`; no new live shared-write verification was performed during integration.

Native channel operations independently retain fresh CSA target/type/role checks and authenticated actor
binding. The installed client has no global lab-roster admission. The agent applies the lab-contact check
before a live run, including changes to a team's or channel's people.

Account-only changes use the typed `ChannelOwn` route for `OwnTeamView` and `ChannelSectionWrite`. It verifies
the authenticated actor (including an interest update's named own account), refuses read-only mutations and
sends once. The previously recorded native own-alerts round trip returned 200 for both updates and confirming
reads, with the original effective state restored. Synthetic transport tests retain wrong-actor, another-account
interest, read-only, section conflict and no-hidden-retry coverage.
