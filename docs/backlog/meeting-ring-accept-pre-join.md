# Accept on a meeting's Request to join has no pre-join

In browser Teams, Accept on a meeting's Request to join ring opens the meeting's pre-join, and Join now answers the
ring. ngteams answers at once (`Ask::Answer` is `CallAction::Answer`), as it answers a group call's ring.

What was seen (task #810, QA round 1, on 2026-11-24): the browser shots `b-b2-ring.png` and `b-b3-accepted.png`.

To do: open the meeting's pre-join on Accept, with the ring held, and send the `accept` at Join now. Then make the
`invitee_ring` check (`crates/ngteams/tests/support/linux/qa_meeting_request.rs`) require the pre-join again, and
change the "no pre-join step" sentence in `docs/protocol/calling.md`.
