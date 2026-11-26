---
worth: later
status: open
where: crates/teams-ui/src/call_window.rs (follow), crates/teams-ui/src/calls.rs (cancel_prejoin, next_choice), crates/teams-ui/src/call_stage.rs
added: 2026-11-26
---
# Cancel after Join now shows the call window for a short time

If the user cancels a pre-join after its Join now and before the call is taken, the call window opens for the
meeting's call. Then the call's stage sends the hang-up, and the window closes. The user sees a call window for a call
that they cancelled. This occurs with the answer of a meeting ring (Cancel stays enabled while it joins), and also
with the join from a calendar pre-join before the call is told.

To do: the call window must not open for a call that a Cancel took back. Today only the call's stage asks the hang-up
(`Calls::next_choice`), so first move that hang-up out of the call window (for example into `Calls` or the
`CallWindows` follow), and then make `CallWindows::follow` skip that call. The review of task 831 (loop 891, round 1)
found it.
