---
worth: later
status: open
where: crates/teams-proto/src/http.rs (request_timeout, with_body), crates/teams-core/src/attachments.rs (Uploads)
added: 2026-11-23
---
# A file upload has only a total time limit, and no stall check

Not seen in a run. It is a conclusion from the code. A request with a body gets one total time limit from its size
(`request_timeout`: 30 s and 1 s for each 64 KiB). Two problems stay:

- `Uploads` lets two uploads run at the same time. On an uplink of approximately 1 Mbit/s, each upload goes at
  approximately 64 KiB/s or slower. Then both can time out, and the user sees "The file service didn't confirm the
  upload or share".
- If the connection stops during a large upload, nothing finds it before the full limit (approximately 67 minutes for
  250 MB). During that time the file shows as pending and holds one upload slot.

To do: give an upload a large total limit, and stop it when its body makes no progress for some time, as the file
reader does (`STALLED`, `crates/teams-core/src/file_content.rs`). This needs a body stream that records its progress.
An upload that stopped is still `CallTried::Uncertain`: the outbox checks it and does not send it again blindly.
