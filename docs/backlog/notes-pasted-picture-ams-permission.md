---
worth: yes
where: crates/teams-ui/src/compose.rs, crates/teams-core/src/attachments.rs, crates/teams-proto/src/write/picture.rs, docs/protocol/endpoints.md
added: 2026-11-21
---
# Capture a picture pasted in the notes, and make the AMS object the same

Task 698 lets the user paste a picture in the account's own notes (`48:notes`). The paste makes an AMS object with
`CreatePicture::new("48:notes", …)`, so its permissions are `{"48:notes": ["read"]}`. No capture shows what Teams
web sends for a picture in the notes, or if AMS accepts the `48:notes` key. The AMS row in `endpoints.md` has
captures only for chat and channel thread ids. If AMS refuses the key, a paste in the notes shows "The picture
service answered N. Try again." Text, files and the other writes of the notes continue to work.

Capture browser Teams with proto-lab in lab account a's own notes: paste a picture and send it. Record the AMS
`POST /v1/objects/` body (its `permissions`) and the answer. If Teams sends a different key, or none, make
`picture_object` do the same for the notes. Add the capture to the AMS row in `endpoints.md`, and add a teams-ui or
engine test of a picture paste in the notes.
