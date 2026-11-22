---
worth: soon
status: open
where: crates/teams-ui/src/channel_view.rs, crates/teams-ui/src/chat_view/channel.rs, crates/teams-ui/src/compose.rs, crates/teams-core/src/engine.rs (`command`), docs/protocol/channels.md (Posting, replying and reacting in a channel)
added: 2026-11-05
---
# A channel's compose box and threads: what Teams has that the app doesn't yet

A team's channel now opens as Teams' threaded channel: its posts as a chat's messages with each post's replies summed
up, "Message in <channel>" posting to the channel, a post's thread beside it whose "Reply in thread" box posts to the
post's chain, and reactions, edits, deletes, copying and workflows as in a chat. What is left, each needing a capture
or a read nobody has made yet:

- **The web client's shapes of the writes.** The native writes ran live from a lab client on a lab team whose only
  person is that lab account: a post (201), a reply posted to the post's chain (201, read back under its post), a
  reaction on each (200) and an edit of the reply (200). The web client's own requests weren't captured: a
  `team-manage` run on such a team should post, reply in a thread and react in the web client, looking for anything it
  adds to a channel post (`subject`, `properties.announceViaEmail`, the chain's `conversationid`) that the chat's
  `SendMessage` leaves out.
- **Files in a channel: what is left.** Attach, drop and paste upload into the channel's folder on the team's site,
  as Teams does ([channels.md](../protocol/channels.md#files-in-a-channel)). Teams' copy notice also offers "Reshare
  older file instead", and its Attach menu "Attach cloud files" and "Browse Teams and Channels"; none of these was
  captured. The toast "You've shared <file>" after the post isn't shown. Also left:
  - An `addStub` whose answer was lost (`CallTried::Uncertain`) can leave an empty file in the channel's folder. A fix
    reads the tried name's path in the folder and recycles that file; no capture showed a read of a file by its path.
  - On the own server, a file keeps the folder it was added in after its channel is renamed, but the client's
    preflight accepts only a file in the channel's folder now. So Remove of a draft file after a rename is refused
    and the file stays. Decide: the server moves a channel's files with its folder (as SharePoint renames the
    folder), or the client recycles in the folder the upload answered.
  - `UploadedFile` holds an own-drive file and a channel file, told apart by `channel()`. An enum of the two kinds
    would keep the own-drive accessors (`site`, `item_url`) off a channel file.
- **Mentions in a channel.** The box's picker offers a chat's stored members; a channel stores none, so it offers
  nobody. Teams offers the team's people and the channel and the team themselves (`@channel`, `@team`).
- **"Send to: thread and channel".** Teams' thread box can also post the reply to the channel; the app says "Send to:
  thread only" and sends only to the thread until that write is captured.
- **Typing and the read horizon.** A channel's box sends no typing and its view no read receipt (no capture showed a
  channel's); Teams shows typing in a channel and moves the channel's horizon, which the channels' unread marks follow.
- **The classic "Posts" layout.** Some channels still use Teams' Posts layout ("Post in channel", "Reply in thread"
  under each post); the app draws every channel in the threaded layout. Which layout a channel has (CSA's `modality`?)
  isn't read yet.
- **The channel header.** Teams' header has the team's tile, the channel's name and its tabs (Conversation, Shared,
  Page, Notes, +); the app's has the tile and the name, no tabs. The thread pane's header in Teams is
  "Threads › <thread>" with a threads list behind "Threads" and a follow button; the app's says "Thread" with Close.
- **A new reply under an old post.** A post keeps its place, so a reply to a post older than the posts loaded (a page
  of 50) shows nowhere until the posts are scrolled back to it; Teams' threads list (above) and its activity feed are
  where such a reply is seen.
