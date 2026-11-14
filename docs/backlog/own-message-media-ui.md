---
worth: yes
where: crates/teams-ui/src/editor/pictures.rs, crates/teams-ui/src/card_view.rs, crates/teams-ui/src/chat_view.rs, crates/teams-ui/src/rich_text.rs, crates/teams-ui/src/recap.rs
added: 2026-11-14
---
# Pass the selected account's cloud through message media views

Task 193 adds own-origin protocol and core support for message pictures, preview pictures and shared files.
Production Own account selection and its UI wiring are a separate subtask. Before enabling that selection,
thread the account's `Cloud` through the composer and message views so they construct and parse the same URLs
that the engine accepts.

Use `sent_picture_in` for pasted pictures, `card_picture_src_in` for card pictures, `InlinePicture::of_in` for
inline pictures, and `shared_files_in` / `Message::files_in` for file chiclets and the chat's shared-items list
in `recap.rs`. Use `unshown_parts_in` / `Message::unshown_in` with the same cloud so valid own-origin files do not
appear as unread content.
Keep the existing Microsoft defaults for Microsoft accounts.

Check the browser reference and sandbox UI under the teams-ui skill when implementing this. Exercise an Own
account pasting, sending and viewing a picture, opening a card picture, showing a file chiclet, and listing that
file in the chat's Shared tab; use synthetic services and verify every request stays on the selected origin.
Retain the Microsoft UI regression checks.
