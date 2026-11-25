---
worth: later
status: open
where: crates/teams-ui/src/sticker_editor.rs, crates/teams-ui/src/stickers/caption.rs
added: 2026-11-25
---
# Edit sticker draws each keystroke in full

Each change of the caption starts one background draw. Each draw waits for the shared font lock, decodes the
template PNG again and draws all of the caption. A later draw only discards an earlier result after it is complete.
Fast typing thus queues many full draws, which hold background threads. Done also decodes the PNG it encoded, only to
read its size.

To do: keep the decoded template in the editor. Let a draw stop before it starts when a newer draw is queued (a shared
counter of the latest draw). Let `render` give the size of the picture. The review of task 650 (round 1) found this.
