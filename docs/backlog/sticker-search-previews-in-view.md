---
worth: later
status: open
where: crates/teams-ui/src/fun_picker.rs
added: 2026-11-23
---
# The Stickers tab's search reads the preview of each sticker it finds at once

A search on the Stickers tab lists each of Teams' stickers whose name or words hold the typed text. One letter finds
most of the 406 stickers. The picker draws every row, and each drawn cell asks the engine for its preview from Teams'
sticker CDN. Thus one key can start some hundred reads at once, and each preview stays in memory for the session. The
reads are public and need no credential. The All tab caps its search at 8 stickers, but no capture shows a cap on
Teams' Stickers tab, so a cap is not the fix.

To fix it:

- Draw the rows of the Stickers tab as a list that builds only the rows in view.
- Ask for a preview only when its row is in view.
