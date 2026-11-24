# A meeting's recap

How the web client's Recap tab reads what a meeting left, and how `ngteams` reads it. Seen in a lab capture of
2026-11-05 (`meeting-steps` as lab account a: two "Meet now" meetings with nobody invited, one recorded twice with
transcription, one transcribed and not recorded; the Recap, its parts, its transcript, Share's Copy link, Watch in
browser and "Download as .vtt"). No value from it is copied here; the ids below are placeholders.

## What the web client shows

- **The meeting chat's header** has tabs after the meeting's name: Chat, Shared, Recap, then the meeting's apps
  (Attendance, Breakout Rooms, Q&A, Meeting Whiteboard, "+1" for more) and "+" to add one; on the right Join, the
  participants' count, Search, chat details and "…".
- **The chat** shows, once the meeting ended, a recap card: the meeting's name and the span of its first recording,
  Share and "View recap", its contents (a Transcript chip) and the recording's thumbnail ("Saving…" while it is
  processed, "2 recordings" with two, "Expires in 120 days" with one, "Set to expire" with two). A recording's own
  message ("Recording has been saved to <organizer>'s OneDrive and will expire in 120 days. Remove expiration") and
  the start and stop lines stay in the chat, each a line after its icon. "View recap" opens the Recap tab. The span
  is the meeting's schedule for a scheduled one (the user's Teams: "6:30 PM - 7:00 PM" for a recording begun at
  6:31), its first recording's for one met now, its transcript's for one only transcribed.
- **The Recap tab**: a box with the meeting's day and span (the earliest start to the latest end of what it left) and
  its menu of the meeting's sessions, the one shown checked; "Part 1", "Part 2" when it was recorded more than once;
  Share (a dialog: recipients to share with and grant access to, a message, Copy link, Share), Watch in browser (the
  recording in Stream) and "…" (Go to Meeting Recaps, Delete recap content). Under it the recording's player, the
  Speakers chip and, for an account without Microsoft 365 Copilot, an upsell ("Upgrade to Microsoft 365 Copilot for AI
  speaker insights", Upgrade Copilot); beside it "Shared files" (the files of the chat, and the organizer's attendance
  report) and the panes: AI summary (for such an account the upsell "Upgrade your Copilot plan for AI summaries") and
  Transcript, plus Notes when notes were taken.
- **The transcript pane**: Search, Download (as .docx or .vtt) and, for the organizer, Delete; "AI-generated content
  may be incorrect", "<name> started transcription", each speaker's turn (their picture, name and time into the
  recording) with its paragraphs, and "<name> stopped transcription" for a transcript alone. While the recording plays
  the paragraph spoken is highlighted; a paragraph clicked plays from it. A search shows "1/3" with up and down
  arrows, each place its words stand counted, the one shown in orange; "Sync to video" brings the pane back to the
  playing paragraph. "Is this transcript useful?" asks for feedback.
- **A meeting transcribed and not recorded**: no player, no parts, no Watch in browser; the files and panes take the
  whole width.
- **Upgrade Copilot** opens Microsoft 365's offer page
  (`offer.m365.cloud.microsoft/launchpad/v1/launchpad.metaos.html?partnerId=teamsmetaoswebcommercial&experienceId=meetingnotes`)
  in a dialog of the client's.

## The reads

- **What the meeting left**: `GET <mcpService>/contents/?threadId=<the meeting's chat>` (the trailing slash as sent;
  the chat's recap card's read adds `&organizerId=<oid>`), the base the region's GTM `mcpService`
  (`https://teams.microsoft.com/api/mcps/<region>`), with a token for the meeting-artifacts app
  `6bc3b958-689b-49f5-9006-36d165f30e00`. The answer is `{changeKey, resources, invalidResources, deletedResources,
  countOfFilteredContentsByChatHistory, paginationInfo: {hasNextPage}}`; a resource is
  `{id, type, location, metadata, subType, addedBy, additionDateTime}`. `type` was `Recording` (its `location` the
  recording's sharing link, `https://<tenant>-my.sharepoint.com/:v:/g/personal/<organizer>/<token>`) and
  `TranscriptV2` (its `location`
  `https://<tenant>-my.sharepoint.com/personal/<organizer>/_api/v2.1/drives/<drive>/items/<item>/versions/current/media/transcripts/<id>/content`,
  and its `metadata.webUrl` the item's sharing link). `metadata` holds `threadId`, `callId`, `driveId`, `driveItemId`,
  `startTime` and `endTime` (.NET ticks), `organizerId`, `ownerTenantId`, `meetingJoinUrl`, `transcriptId` and more.
  A recording and its transcript share the item; a meeting transcribed alone has a transcript on an item of its own
  and no recording. A recording still processing isn't listed yet.
- **The recording** is played by the web client in SharePoint's embedded player (Graph `POST
  drives/<drive>/items/<item>/preview`, the site's `_api/v2.0`/`v2.1` item reads with a SharePoint token,
  `streamembed.aspx`), which streams it in DASH parts and never downloads it ([below](#streaming-a-recording)).
- **The transcript's words**: the player reads `…/_api_cached/v2.1/drives/<drive>/items/<item>/cdnmedia/transcripts?…`
  (signed query, a compressed JSON); "Download as .vtt" reads
  `GET https://<tenant>-my.sharepoint.com/personal/<organizer>/_api/v2.1/drives/<drive>/items/<item>/media/transcripts/<id>/streamContent?is=1&applymediaedits=false`
  with a SharePoint token (audience the site's origin; the token's `aud` SharePoint's
  first-party application id, not the URL), answered `text/vtt`. Its cues are named `<utterance>-<n>`: the cues of one
  utterance are one paragraph of the pane, a speaker's utterances in a row one turn (`<v Name>` names the speaker).
- **The transcript's language**: the player reads the item's transcripts,
  `GET <site>/_api/v2.1/drives/<drive>/items/<item>?select=media/transcripts&$expand=media/transcripts` with the
  site's SharePoint token, answered `{media: {transcripts: [{id, displayName, size, languageTag, transcriptType,
  isVisible, isAutoGenerated, isDefault, temporaryDownloadUrl, source, downloadCode, …}]}}`; `languageTag` (`en-US`)
  names its captions ("English (United States)").
- **What the account may do with the recording**: the player reads the item,
  `GET <site>/_api/v2.1/drives/<drive>/items/<item>?$select=accessViewpoint,sharepointIds/siteUrl,media,source,webUrl,title,accessViewpoint,highlightsInfo,@content.downloadUrl,sharepointIds,expiration`
  with the site's SharePoint token, answered `{@content.downloadUrl, webUrl, title, sharepointIds, expiration:
  {expirationDateTime, isExpirationModifiable}, media: {…, recordingStartDateTime, recordingEndDateTime, mediaSource:
  {contentCategory: "Meeting", contentType: "Recording"}, viewpoint: {…}}, source: {application: "Teams", threadId,
  meetingOrganizerId, …}, accessViewpoint: {canRead, canEdit, canDelete, canComment, canDownload,
  canManagePermissions}}`. `accessViewpoint.canDownload` was true for the organizer. Every answer from the personal
  site carries `x-ms-invokeapp: 1; RequireReadOnly`, the organizer's too, so that header doesn't tell a blocked
  download; the attendee's refused download link ([below](#streaming-a-recording)) does.
- **Downloading the recording**: Teams' Recap offers no Download (its "…" holds Go to Meeting Recaps and Delete recap
  content, its player's gear Caption settings, Quality and Keyboard shortcuts; the chat's recap card and the Shared
  tab's recording links none either). Watch in browser opens the recording in Stream, whose command bar has Download
  (a download arrow and the word): a "Download video" dialog ("The download is video only and won't include
  Clipchamp content such as Transcript, Chapters or Comments.", Don't show this again, Download, Cancel), then the
  site's `_layouts/15/download.aspx?UniqueId=<listItemUniqueId>` (cookies), answered `video/mp4` with
  `content-disposition: attachment; filename="<meeting>-<yyyymmdd_hhmmss>-Meeting Recording.mp4"` (the item's name).
  Seen in a lab capture of 2026-11-06 (`team-manage` as lab account a, its own "Meeting with AI" recap).
- **A recording's expiry**: the chat's recap card reads the recording's item,
  `GET https://<tenant>-my.sharepoint.com/_api/v2.1/drives/<drive>/items/<item>?$select=expiration,…` (with
  `accessViewpoint`, `sharepointIds/siteUrl` and more) with the site's SharePoint token, answered
  `{expiration: {expirationDateTime: "<RFC 3339 with an offset>", isExpirationModifiable: true}, …}`: 120 days after
  the recording was saved, as the tenant set it ("Expires in 120 days", the days counted up).
- **The license**: the web client reads Graph's `users/<oid>/licenseDetails` to choose the Copilot upsell; both
  accounts it was seen with had no Copilot plan.
- **Share → Copy link** copies
  `https://teams.microsoft.com/l/meetingrecap?driveId&driveItemId&sitePath=<sharing link>&fileUrl=<the file's address>?web=1&threadId&organizerId&tenantId&callId&threadType=meeting&meetingType&subType=RecapSharingLink_RecapCore&recapType=RecordingAndTranscript`.
  **Watch in browser** opens the file's address with `?web=1&referrer=Teams.TEAMS-WEB&referrerScenario=RecapOpenInStreamButton.view`.

## The player's controls

Seen in a lab capture of 2026-11-05 (`meeting-steps` as lab account a, its "Meeting with AI" recap): under the seek bar,
play, back 10 s, forward 10 s, mute (a volume slider), the time ("0:47 / 1:52"); on the right CC, the speed ("1x"),
the settings gear, full screen and pop-out. Each of CC, the speed and the gear opens a dark menu over the picture, its
button underlined while it is open; a click outside or Esc closes it.

- **CC**: Off (checked by default) and the transcript's language ("English (United States)"). On, the cue spoken is
  drawn over the bottom of the picture above the controls, centred, without the speaker's name, white on black in
  its default style.
- **The speed**: 2x, 1.8x, 1.5x, 1.2x, 1x (checked) and 0.8x; the button says the one chosen ("1.5x"), the sound's
  pitch kept.
- **The gear**: Caption settings (Size: Small, Medium, Large; Color: Standard, Standard reverse, Purple, Purple reverse,
  each with a swatch; Background transparency, a switch, the words then drawn without their background; Revert to
  default settings), Quality ("1080p", a submenu holding the presentation's one representation, "1080p HD", checked)
  and Keyboard shortcuts (a dialog listing the player's Alt keys: Alt+K plays or pauses, Alt+J and Alt+L skip 10 s,
  Alt+M mutes, Alt+C toggles caption languages, Alt+Z and Alt+X slow down and speed up, …). A submenu takes its
  menu's place under a "<" header.

## Streaming a recording

A meeting recording's download is blocked for those who may only watch it (an attendee's view of another's meeting):
its item's download link (`download.aspx`, the one Graph's `shares/…/driveItem` hands out) answers 403 "Access
denied" with `x-ms-invokeapp: 1; RequireReadOnly`, whatever the token. The player streams it instead, as below; seen as
the organizer in a lab capture of 2026-11-05 and a lab probe the same day (the organizer's own recording, read in the
browser), the user's own account's debug log showing the download refused for an attendee.

- **The presentation**: `GET <site>/_api/v2.1/drives/<drive>/items/<item>/content?format=dash` with the site's
  SharePoint token, `prefer: noredirect` (the MPD itself rather than a redirect to it), `applymediaedits: true` and
  `enablecdn`. With `enablecdn: 1` (the web player's) the MPD's parts are on SharePoint's CDN
  (`_api_cached/v2.1/drives/<drive>/items/<item>/oneDrive.transcode?…`, read with `X-Authorization: Bearer <site
  token>`), each part encrypted whole in AES-128-CBC (`ContentProtection` `urn:mpeg:dash:sea:2012`), its key from
  `https://<region>-mediap.svc.ms/transform/VideoProtectionKey?…` or, as the web player takes it, from the
  `EncKey` of the `g_fileInfo` the embed page (`streamembed.aspx`, posted the site token as `access_token`) holds.
  With `enablecdn: 0` the MPD has no `ContentProtection` and its parts are the transform service's own, in the clear.
- **The MPD**: static, one period, `BaseURL` `https://<region>-mediap.svc.ms/transform/`; an audio adaptation set
  (`mp4a.40.2`, a representation `audcopy`) and a video one (`avc1`, `vcopy`: the recording's own streams, remuxed),
  each a `SegmentTemplate` with a timescale (16000 for both in the capture), `initialization` and `media` addresses
  (`videotranscode?provider=Spo&docId=<the item's address with a signature and its expiry>&…&part=initsegment` or
  `part=mediasegment&segmentTime=$Time$`, `track=audio|video`, `quality=$RepresentationID$`) and a `SegmentTimeline`
  of 6 s segments; `mediaPresentationDuration` the recording's length.
- **A part**: `GET` of its address, no token or cookie (its `docId` signs it): fragmented MP4, an `ftyp` and `moov`
  for an initialization segment, a `moof` (`tfhd` with the defaults, `tfdt`, `trun` of sizes) and its `mdat` for a
  media segment.

### An attendee's view (lab, 2026-11-24)

Lab account b opened the Recap tab of its 1:1 chat with a (`team-manage`), whose call recordings are in a's OneDrive,
and played one. Teams read MCPS as above, then the recording's item on the tenant's root path
(`https://<tenant>-my.sharepoint.com/_api/v2.1/drives/<drive>/items/<item>?$select=accessViewpoint,…`), Graph's
`POST drives/<drive>/items/<item>/preview`, `xplatplugins.aspx` and `streamembed.aspx` on a's site, and the
presentation on the root path, `…/_api/v2.1/drives/<drive>/items/<item>/content?format=dash` with `enablecdn: 1`,
`prefer: noredirect` and `applymediaedits: true`, answered `application/dash+xml`. On Play, the embedded player read
the media service's `transform/videomanifest?…&docid=<the item's signed address>&part=index&format=dash` (with the
site's `X-SPOPacToken`), an MPD whose parts are on the site's `_api_cached/v2.1/…` and encrypted
(`urn:mpeg:dash:sea:2012`), as with `enablecdn: 1` above. All of it worked for b, who isn't the recording's owner:
the drive and item come from MCPS's `metadata`.

A recap whose `metadata` names no drive or item can't be streamed by them. `ngteams` then reads the recording's item on
Graph by its sharing link (`shares/u!<link>/driveItem`, `Prefer: redeemSharingLink`, as a shared file's,
[graph.md](graph.md#a-link-for-specific-people)) and streams it by the item's `parentReference.driveId` and `id`. A
recording's link in the short form of a link for specific people (`/:v:/p/<owner>/<token>`) names the same personal
site as `/:v:/g/personal/<owner>/…`.

## A 1:1 or group call's recording

Seen in a lab capture of 2026-11-06 (`team-manage` as lab account a, its 1:1 chat with lab account b holding three call
recordings and two calls transcribed without recording; the chat, each card's View recap, the sessions menu, the
Transcript pane and "…").

- **The chat** has the same tabs as a meeting's (Chat, Shared, Notes, Recap, Storyline). A recording's states are the
  same lines as a meeting's ("<name> started recording.", "<name> stopped recording.", "Recording has been saved to
  <name>'s OneDrive and will expire in 120 days. Remove expiration", the account named by its own name there too), and
  under the saved line the same recap card as a meeting's, of that recording alone: titled by the recording ("Call
  with <name>"), its day and its own span, Share, View recap, the Transcript chip when it was transcribed, the
  thumbnail with its length and "Expires in 120 days". A call transcribed and not recorded has a card of its own (no
  thumbnail); a transcript a recording of its call holds has none.
- **The recording's message** names no organizer (`MeetingOrganizerId` empty) and no transcript item; it names where
  it is kept instead, `<RecordingStorage type="RecordingInitiatorOneDrive" userId="<oid>">`, the one who started it.
- **The Recap tab** reads what the chat's calls left with the meeting's read, the chat's own id as `threadId`:
  `contents/?threadId=<1:1 chat>` lists each call's recording under `resources` as a meeting's (its sharing link, call,
  drive, item and times, `organizerId` the one whose OneDrive holds it). The calls' `TranscriptV2` resources come only
  under `invalidResources`, with no `location`; the recording's item lists no transcripts (`media` empty in the
  `select=media/transcripts` read). Choosing a session reads the same list narrowed by `&recapCallId=<call>`. The
  sessions menu lists the recordings alone, each call a session by its own day and span ("Friday, November 6, 2026
  5:29 PM - 5:30 PM"), the latest first and shown when the tab opens; its box is as wide as its line up to about
  390 px, a longer line (a meeting's too) cut with an ellipsis at its end, and the menu's rows as wide as the box, cut
  the same way. The player and "…" (Go
  to Meeting Recaps) are a meeting's, and the Transcript pane says "No transcript available" ("The transcript for this
  meeting was never created, has expired, or has been deleted."). The recording is played as a meeting's (the item's
  `content?format=dash`, `streamembed.aspx`).
- **Share → Copy link** (a later capture the same day, the same chat: the card's Share and the tab's, Copy link alone)
  opens the meeting's "Share Recap" dialog, its preview "Recap: Call with <name> <weekday>, <month> <day> | Meeting |
  Microsoft Teams", and copies, with no request, the `meetingrecap` link (as plain text, and as HTML, an anchor titled
  so) in a 1:1 chat's shape: `threadType=OneOnOneChat&meetingType=Unknown`. From the Recap tab it is
  `driveId&driveItemId&sitePath=<sharing link>&fileUrl=<the file's address>?web=1&threadId&organizerId&tenantId&callId&threadType&meetingType&subType=RecapSharingLink_RecapCore&recapType`,
  `recapType=Recording` (its transcript is invalid); from the card it is
  `driveId&driveItemId&sitePath=<sharing link>&fileUrl=<the same sharing link>&threadId&callId&threadType&meetingType&subType=RecapSharingLink_RecapChiclet&recapType`,
  no organizer or tenant, `recapType=RecordingAndTranscript` (as the message's `contentTypes` say). The link's origin
  is the web client's own (`teams.cloud.microsoft` there).
- **Names**: the 1:1 chat's lines name the account by its own name ("AI started recording.", "AI stopped
  recording.", "saved to AI's OneDrive"), as the meeting chat's do; no other line of the 1:1 chat names anyone. No
  group chat of lab accounts holds a recording, so a group chat's lines weren't seen.
- **A call transcribed and not recorded** (a lab capture of 2026-11-06, `team-manage` as lab account a, the a–b chat's
  call transcribed alone by a's native client; its card "Call with <name>", the call's day and span, Share, View recap,
  the Transcript chip, no thumbnail). The bot saves the transcript as a file of its own in the starter's OneDrive
  (`Recordings/<title>-<local stamp>-Meeting Transcript.mp4`, its last `Media_CallRecording` state naming its sharing
  link, drive and item, `contentTypes="Transcript"`). MCPS lists it only under `invalidResources` (a `TranscriptV2`
  with its call, times, item, `webUrl`, organizer and tenant, no `location`), so the Recap tab opened by itself doesn't
  list the call: its sessions are the recordings. The card's View recap (and its Transcript chip, the Transcript pane
  shown) opens the call's own recap: the call's day and span as a plain line (no box, no menu), Share and "…" (Go to
  Meeting Recaps), no player and no Watch in browser, "Shared files" and the panes (AI summary, Transcript) across the
  whole width; the Transcript pane says "No transcript available" ("The transcript for this meeting was never created,
  has expired, or has been deleted.") with "Is this transcript useful?". Teams reads MCPS again, then the transcript
  file's item on the personal site (`$select=accessViewpoint,…,media,…,expiration`: `media.mediaSource.contentType:
  "Transcript"`, the call's `recordingStartDateTime` and `recordingEndDateTime`, the account allowed everything), its
  Graph `preview`, and its `select=media/transcripts,audioTracks&$expand=…`, answered `"media": {}`: the file lists no
  transcript, so Teams shows none. Leaving the tab and coming back shows the latest recording again. In an earlier
  capture of the same chat, a call recorded between two transcribed stretches had a transcript card for each stretch;
  one's View recap showed that call's session with "Part 1" (the transcribed stretch, a transcript glyph, "This part
  of the meeting was transcribed only." in a bar with a Close under the top row) and "Part 2" (the recording); opened
  by itself the same session showed the recording alone, no parts. Share → Copy link copies the tab's shape with the
  transcript file's address as `fileUrl` and `recapType=ODSPTranscript`; the card's Share the card's shape, also
  `recapType=ODSPTranscript`.

## What `ngteams` does

`teams_proto::recap` reads MCPS's answer (`MeetingContents`, `Service::Mcps`, the GTM `mcpService`) and the
transcript's address (`TranscriptLocation`: only a personal site's, read with that site's token, as the chat-file
upload's reads are); `teams_proto::transcript` reads the `WebVTT`. `teams_core::recap` makes both reads when the Recap
tab asks; nothing is stored. The tab (`teams_ui::recap_view`) shows the parts, plays the recording, and draws the
transcript beside it (no panes' pills: the transcript is the only pane). The recording is streamed
(`teams_proto::recording`, `teams_core::recap`): the presentation with `enablecdn: 0`, an MPD with `ContentProtection`
or parts off the transform service refused, each part read as the player (`teams_media::player::stream`) asks for it,
demultiplexed there and decoded by `GStreamer` (a part refused, as one whose `docId` expired is, asked for once more at
its address in the presentation read again); a recording that can't be streamed is played from its download link as
any shared video ([graph.md](graph.md#a-shared-files-content)) before the player says it can't be opened. Nothing of
it is stored. Share copies the recap link (without `fileUrl`, which only the item's own read gives), Watch in browser
opens the recording's sharing link (Stream's player). **Download** (the recap's More options, Stream's Download brought
into the app) is offered only when the recording's item, read as the player reads it with a narrower `$select`
(`name,size,accessViewpoint`: `teams_proto::recording::RecordingFile`), says `accessViewpoint.canDownload`; it saves the
recording where the user chooses (a save dialog in the downloads folder, under the item's name), its bytes read at the
download link Graph hands out for the sharing link as a shared file's are ([graph.md](graph.md#a-shared-files-content):
64 MiB streams, a link that expired read again, a stream that broke off opened again where the file stands). The
recap shows its progress with Cancel, then Show in folder or what failed with Try again; it runs apart from the tab, so
leaving the chat doesn't stop it. Stream's "Download video" note is left out (the app's download is the video file
alone, as Stream's is; the transcript has its own Download). The player's bar is the Recap player's
([above](#the-players-controls)): CC (the transcript's cues as captions, named by its language as its item's
transcripts give it), the speed (a seek at the same place with the new rate, the sound through `GStreamer`'s
`scaletempo`), and the gear's caption settings (kept for the run, for every recording), quality (the picture's height
played) and keyboard shortcuts (the app's own keys: Space, Left/Right, M, C, Z/X, Esc). Left out, as no read backs them or they write: Share's
recipients (a grant and a post), Delete recap content and the transcript's Delete, "Go to Meeting Recaps" (a Teams
app), the attendance report (the organizer's reports), Notes (Loop), feedback, Download as .docx, and everything
Copilot: the AI summary, the Speakers chip's speaker insights and their "Upgrade Copilot" upsells (left out by the
user's choice). The AI summary pane is the app's own instead: made on this computer by Claude Code or the Codex CLI
from the transcript the recap already read (`teams_ui::recap_summary`), no Teams request involved; sending it to the
chat is an ordinary message post through the outbox.

The chat's recap card (`teams_ui::meeting_recap`) is worked out from the chat's own messages (each recording's call,
file, start and length, the transcript's call); it reads only the recording's thumbnail (Graph, as a shared file's) and
its expiry (`RecordingExpiration`: the item read above with `$select=expiration` alone, on the personal site's own
path). View recap and the Transcript chip open the Recap tab at the card's first recording (the chip its transcript from
the top), the thumbnail plays it there, and Share → Copy link copies the same recap link as the tab's Share. Share's
recipients and "Remove expiration" (writes) are left out.

A 1:1 or group chat's calls ([above](#a-11-or-group-calls-recording)) are shown the same way: each recording saved is
the saved line (its owner the `RecordingStorage` user when no organizer is named) with its recap card under it, a
transcript no recording of its call holds a card of its own (`teams_ui::meeting_recap::call_cards`), and the chat has
the tabs while it holds one. Its Recap tab makes the meeting's read with the chat's id, each call a session of its menu
(its parts beside it when it was recorded more than once), the latest shown first; a recording with no transcript
to read keeps the Transcript pane, saying there is none as Teams does, and the chat's captions saved on this computer
stay sessions of their own (Teams' transcript of the same time would win, as a meeting's). The transcripts MCPS lists
under `invalidResources` are read too (`ContentKind::UnreadTranscript`; one on a recording's item is told by the
recording): a call transcribed alone isn't a session of the menu, and its card's View recap or Transcript chip shows it
as Teams' does above (`teams_ui::recap_view`'s `transcribed`: the plain line, the files and panes across the width, the
Transcript saying there is none; a stretch transcribed alone of a recorded call is a part of its session, with the
note), the part the card stands for (its file, call and span, from the chat's messages) shown in its place when the
read doesn't list it. Its AI summary has no transcript to work from either; captions saved on this computer of that
call are a session of their own with theirs. The transcript file's item reads are left out: they found no transcript. A recording's start and
stop name the account by its name, as Teams' 1:1 chat does. Share → Copy link copies a 1:1 call's link in Teams'
shapes above (the tab's without `fileUrl`, which only the item's own read gives; a call transcribed alone's
`recapType=ODSPTranscript`); a group chat's keeps the meeting's shape (`threadType=meeting`), its own not captured.
