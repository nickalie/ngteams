---
worth: later
status: open
where: crates/teams-ui/src/compose.rs
added: 2026-11-22
---
# The compose box's problem notice does not scroll with the keyboard

The compose box's problem notice (`problem_view`) has a height limit (`PROBLEM_MAX`) and scrolls. A long list of
files that a channel recycle could not remove overflows it. The notice scrolls only with a pointer: it has no focus
target and no keyboard scroll, so a keyboard-only user cannot read the clipped file names.

To do: compare with how Teams shows a long error in its compose box (lab account, screenshots). Then make the notice
focusable when it overflows, and let the arrow and page keys scroll it. The review of task 706 (round 6) found it.
