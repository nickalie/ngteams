---
worth: later
status: open
where: crates/teams-ui/src/files.rs
added: 2026-11-25
---
# The keyboard does not reach a file card

A file card in a chat (an image, a video or another file in the file grid) is not a tab stop. Tab and Shift+Tab do
not go to it, and Enter cannot open it or play a video. Only the mouse opens a card.

To do: compare with Teams in the browser (lab account, screenshots): which cards take focus, how the focus ring
shows, and which keys open or play. Then give the cards that focus. The QA walk of task 793
(`crates/ngteams/tests/support/linux/qa_file_grid_video.rs`, `keyboard`) logs this as a known gap; make it fail
again when the cards take focus.
