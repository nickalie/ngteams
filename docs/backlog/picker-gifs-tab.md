---
worth: later
status: open
where: crates/teams-ui/src/fun_picker.rs, docs/protocol/chat.md (Stickers)
added: 2026-11-20
---
# The compose box's picker has no GIFs tab

Teams' "Emoji, GIFs and Stickers" picker has four tabs: All, Emoji, GIFs and Stickers. All also has a GIFs section with
"See all" and the line "GIFs powered by GIPHY View Terms" (lab screenshot `team-manage/20261119T163307Z/s03-picker-all.png`).
The app's picker (`crates/teams-ui/src/fun_picker.rs`) has All, Emoji and Stickers. The user asked for stickers, not
for GIFs, so the sticker task (582, subtask 653) left the GIFs tab out.

To add it:

- Capture the GIFs tab and its search in browser Teams with a lab account. The plan of 582 names the request it saw:
  `POST api/mt/part/<region>/beta/commands/com.microsoft.teamspace.inputextension.giphy/execute`.
- A picked GIF is a message's picture (the app already shows Giphy GIFs that others send). Find how Teams sends it.
- Add the tab between Emoji and Stickers, the GIFs section on All, and the GIPHY line.
- An own-server account has no Giphy: the tab is not offered there.
