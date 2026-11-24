# Microsoft Graph: people's names and pictures

Status: 2026-10-21, M2 Task 12. Written from `proto-lab probe profile` run live as lab accounts a and b (each with its
own Graph token, reading a's, b's and c's records), which printed statuses, header names, content types, sizes and
key names with their JSON types, never a value, and kept nothing. The native client's Graph reads are in
`crates/teams-proto/src/read.rs` (`GraphUser`, `GraphPicture`), the transport's check of them in
`crates/teams-proto/src/http.rs` (`Service::Graph`), and their use in `crates/teams-core/src/profiles.rs`.

## Graph is our own reach, not the web client's

The web client never asks Graph for a person. It names people through the middle tier's profile lookups
(`beta/users/fetchShortProfile` and `beta/users/fetch`, reads sent as `POST`, [chat-service.md](chat-service.md#reads-by-post))
and draws their pictures from the middle tier's `profilepicturev2`, authenticated by a cookie it first gets from
`beta/users/<oid>/cookiev2` (its answer was not captured in those early runs). The only Graph request in the lab's captures is the
tenant's domain list (`v1.0/domains`, [endpoints.md](endpoints.md#microsoft-365-around-teams)).

So no capture shows the reads below. They are reads (`GET`, no body) of our own choosing, and the probe's answers are
the reviewed shape: the transport sends nothing to Graph but these two reads, in exactly the form the probe sent.

## The token

The native client (`1fec8e78-…`) gets a Graph token from the account's refresh token like any other audience
([auth.md](auth.md#open-questions), question 1): the scope `https://graph.microsoft.com/.default`. The lab accounts'
tokens carried `User.ReadBasic.All` among their scopes, which is what reading another account's name and picture
needs. No Conditional Access block, for either account.

## What the probe read

Each with `Authorization: Bearer <the Graph token>` and `Accept: application/json`, nothing else (no `X-MS-…`
header, no cookie). `<oid>` is the account's object id, the GUID its MRI `8:orgid:<oid>` names.

| Read | Status | Answer |
|---|---|---|
| `GET https://graph.microsoft.com/v1.0/users/<oid>` | 200 for a, b and c | `application/json`, about 370 bytes: `@odata.context`, `id`, `displayName`, `userPrincipalName`, `mail` (strings), `businessPhones` (an array), `givenName` and `surname` (strings, or `null` for the account without them), `jobTitle`, `mobilePhone`, `officeLocation`, `preferredLanguage` (`null`) |
| `GET …/v1.0/users/<oid>?$select=id,displayName` | 200 for a, b and c | `application/json`, about 160 bytes: `@odata.context`, `id`, `displayName`, all strings |
| `GET …/v1.0/users/<oid>/photo/$value` | 200 for a; 404 for b and c | a: `image/jpeg`, about 230 KB (the largest size Graph has); b, c: `application/json`, `{"error": {…}}` with the code `ImageNotFound` |
| `GET …/v1.0/users/<oid>/photos/96x96/$value` | 200 for a; 404 for b and c | a: `image/jpeg`, about 7 KB; b, c: as above |

The answers' headers were `cache-control`, `client-request-id`, `content-type`, `date`, `odata-version` (the user
reads), `etag` (a picture), `request-id`, `strict-transport-security`, `transfer-encoding`, `x-ms-ags-diagnostic` and
`x-ms-resource-unit` (the user reads). No redirect, no throttling.

## What the native client reads

- **A name**: `GET https://graph.microsoft.com/v1.0/users/<oid>?$select=id,displayName`. The `$select` form asks for
  nothing of the person's record but the name shown (no address, phone or title), and is read only when its `id` is
  the one asked for (any case); a missing, `null` or blank `displayName` is no name.
- **A picture**: `GET https://graph.microsoft.com/v1.0/users/<oid>/photos/96x96/$value`, the size an avatar needs
  (the probe's 96×96 picture was a thirtieth of the largest). Kept only as `image/jpeg` (what the probe saw) or
  `image/png`, up to 256 KB; a 404 is no picture, and the view draws initials.
- **Only for an MRI `8:orgid:<GUID>`**, and with a shared file's thumbnails (below) the only Graph reads the native
  client builds; the transport refuses any request to Graph that isn't a read.

## A shared file's thumbnail

A file a message carries (`properties.files`, [chat.md](chat.md#files-received)) is drawn as a card, a video's or an
image's with a picture of it. The web client draws that picture from SharePoint's own API with a SharePoint token;
the native client asks for no SharePoint token and reads it through Graph instead, by the file's sharing link, in
the form Microsoft documents. Like the person reads above, no capture or probe shows it: it is our own reach, two
reads, and a file the account can't read is just a card without its picture.

- **The list**: `GET https://graph.microsoft.com/v1.0/shares/u!<sharing link>/driveItem/thumbnails` with the
  account's Graph token and `Prefer: redeemSharingLink` ([below](#a-link-for-specific-people)), `<sharing link>` the message's `fileInfo.shareUrl` in base64url without padding (Graph's
  sharing token), only for an `https` link on a SharePoint host. The answer is documented as `value`, a list of
  thumbnail sets, each with an `id` and a `small`, `medium` and `large` thumbnail (`width`, `height`, `url`); an empty
  list for a file without one. The fixture `tests/fixtures/graph/thumbnails.json` is written from that documentation.
- **The picture**: the first set's `large` thumbnail (up to 800 pixels on its longer side; else its `medium` one),
  read at its `url` as Graph handed it out (`teams_proto::ThumbnailLink`): a pre-authenticated link that carries its
  own credential in its query, on SharePoint's media host (`https://<name>.svc.ms/transform/thumbnail?…`) or a
  SharePoint host, and nothing else. It goes with no token of the account's (`Service::FileThumbnail`); the log
  writes it at `info` by host and path alone, and at `debug` with its query's credentials redacted (a `tempauth`, an
  `access_token` in a link nested in `docid`, any JWT-shaped value: `teams-log`'s redactor); and it is kept only when
  its bytes are a PNG, JPEG, GIF or WebP picture.
- **Kept for the run** in the engine's memory (`teams-core`'s `thumbnails.rs`), once per file: an answer that isn't a
  success (403, 404), an empty list or bytes that aren't a picture are no picture, and the card shows its placeholder;
  a read that failed on the way is asked again later. The bytes reach the card only from the engine.
- **Cached across runs** in the account's picture cache beside its store (`teams_store::PictureCache`, by the
  SHA-256 of the file's sharing token, never the link Graph handed out): a picture read is shown by a later run
  without either read for a week, then read again (the file may have changed); no picture is never cached.

## A shared file's content

A video a message carries is played in its card, as Teams plays it. The web client plays it through SharePoint's own
media API (Stream's player, a SharePoint token); the native client asks for no SharePoint token and reads the file's
bytes through Graph's documented download link instead. No capture or probe shows these reads either: they are our own
reach, and a file the account can't read says so in its card and offers the browser.

- **The item**: `GET https://graph.microsoft.com/v1.0/shares/u!<sharing link>/driveItem` with the account's Graph
  token and `Prefer: redeemSharingLink`, the sharing token as the thumbnails' (`teams_proto::FileDownload`). The answer is documented as the file's
  `driveItem`; the native client reads its `size` and its `@microsoft.graph.downloadUrl`, "a URL that can be used to
  download this file's content", pre-authenticated (a `tempauth` in its query) and short-lived (about an hour). The
  link is taken only on a SharePoint host (`teams_proto::FileContentLink`).
- **The bytes**: `GET <download link>` with `Range: bytes=<first>-<last>` and no token of the account's
  (`Service::FileContent`), up to 2 MiB a read, answered `206` with `Content-Range: bytes <first>-<last>/<size>`, the
  part exactly as asked or shorter only at the file's end (a `200` with the whole file is cut to what was asked, but
  only up to the transport's 4 MiB body: a host that ignores `Range` on a larger file can't be played). The log writes it at `info` by host and path alone, and at
  `debug` with its `tempauth` redacted and its body by size only.
- **Streamed** (what a playing video reads by, `HttpTransport::file_stream`): the same `GET` with
  `Range: bytes=<first>-<last>` for up to 64 MiB (16 MiB as the player asks), its body handed on as it arrives instead
  of read whole, answered `206` as above (or a `200` with the whole file to a read from its start). Dropping it resets
  its HTTP/2 stream, so a seek stops the old place's read at once; its timeout is 10 minutes end to end, and the engine
  fails a body silent for 20 s. On a link measured from a laptop (about 170 ms to the file's host, about 400 KB/s on
  one connection), a 1 MiB part took 3 to 10 s whole, so reading by whole parts kept a video's first frame and a seek
  waiting seconds on bytes it didn't need yet; streamed, the first bytes come one round trip after the headers.
- **Kept for the run** (`teams-core`'s `file_content.rs`): the link per file for up to 40 minutes, read as soon as a
  video's card is drawn (its size asked for), so Play doesn't wait for Graph; a 401 or 403 from the file's host (an
  expired link) asks Graph for a new one, once per read. Any other 4xx (but a throttle) from Graph or the file's host,
  an item without a link, or a part that isn't the one asked for means the file can't be had; a 5xx, a throttle or a
  network failure fails that read only, and the player streams from there again. The bytes aren't kept by the
  engine: the player keeps the last 128 MiB it read (`teams-media`'s `player::source`), streaming up to 8 MiB ahead of
  what it plays, at most three streams at once (a stream nobody read for half a second gives way), a seek's new place
  first; an MP4 whose index (`moov`) follows its media has the index streamed as soon as the file's first box headers
  came, alongside GStreamer's setup and type finding. The bytes reach the
  player only from the engine, and the player hands them to GStreamer's `playbin` through an `appsrc`: GStreamer reads
  nothing from the network itself.

## A link for specific people

Seen in a lab capture of 2026-11-24 (`team-manage`: lab account a sent b a video from its OneDrive in their 1:1 chat,
then b opened the chat). A file a colleague shares in a chat often has a link that works only for the people it
names: its short form is `https://<tenant>-my.sharepoint.com/:<kind>:/p/<owner>/<token>` (no `?email=`). Graph
refuses such a link to the very people it names unless the read asks Graph to redeem it: `403` with "The sharing link
no longer exists, or you do not have permission to access it.", an `@error.fixItUrl` to the site's
`guestaccess.aspx?share=<token>` and `SharesAccessDeniedVroomException` (seen in the user's own log, a video from
another person of the same tenant).

- **What Teams sends**: the recipient's web client reads a received video's card with
  `POST https://graph.microsoft.com/v1.0/shares/u!<sharing link>/driveItem/preview` and the header
  `Prefer: redeemSharingLink`, the account's Graph token. The answer's `getUrl` is SharePoint's embed page of the item
  (`<owner's site>/_layouts/15/embed.aspx?uniqueId=<item>&access_token=…`), which the card shows in a frame (Stream's
  player). The sender's client sends the same read with the file's own address (`objectUrl`) as the sharing token.
- **Thumbnails**: the web client reads the card's picture on the owner's site with its SharePoint token,
  `GET <owner's site>/_api/v2.1/shares/u!<sharing link>/thumbnails/0/c960x540/content?prefer=noredirect,closestavailablesize`.
- **When the message is sent**, the sender's web client also grants each person of the chat access to the file
  (`POST <owner's site>/_api/v2.0/sites/root/items/<item>/driveItem/invite`, `{"recipients": [{"email"}], "roles":
  ["write"]}`), whatever the link's kind.
- **What `ngteams` does**: its two Graph reads of a shared file (the thumbnails and the item) go with
  `Prefer: redeemSharingLink`, as Teams' read does (`teams_proto::REDEEM_SHARING_LINK`). The header asks Graph to
  redeem the link for the account, as opening the link in a browser does; it gives nothing to anyone else. The
  account's Graph token still goes only to Graph, and the bytes are read at the download link Graph hands out, with no
  token, as above.
- **Not seen**: the link-settings dialog ("People currently in this chat", "People you choose") didn't apply in the
  headless browser, so the lab message has a link for anyone. b's read above is the same whatever the link's kind;
  the refusal without the header is the user's log's.

## Where a name comes from

`teams-core`'s engine (`profiles.rs`) asks, first that answers: Graph's name; the middle tier's profile lookup
(`users/fetchShortProfile`, a typed `PostRead`); the name the person's newest stored message carried
(`imdisplayname`); a name supplied by the account startup, when available. The picture is Graph's alone.
What a read found is kept in the store and read again after a day; a read that failed on the way (the network, a 5xx,
a refused or throttled token) keeps the last one and is tried again after five minutes; one person is looked up once
at a time. The bytes of a picture reach a view only from the engine.
