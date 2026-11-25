---
worth: later
status: open
where: crates/teams-ui/src/call_stage/transcript.rs, crates/teams-ui/src/calls.rs
added: 2026-11-26
---
# The call's Transcript pane in a long meeting

The live transcript of a call (`Calls::spoken`) keeps each final utterance for the whole call, with no limit. The
Transcript pane makes a row for each line on each render, and the translation gets all lines while the pane shows.
In a meeting of some hours, this can make the call's window slow. Nobody measured it.

To do: measure a synthetic transcript of some thousand lines in the UI sandbox. If it is slow, show the pane in a
virtualized list (rows only for the visible turns), or keep the built entries until the spoken lines change. The
review of task 868 (round 1) found it.
