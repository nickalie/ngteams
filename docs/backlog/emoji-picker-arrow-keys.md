---
worth: later
status: open
where: crates/teams-ui/src/reaction_picker.rs, crates/teams-ui/src/fun_picker.rs
added: 2026-11-20
---
# The arrow keys do not move in the emoji list

In the emoji list (`ReactionPicker`: "More reactions", the Emoji tab of the compose box's picker, and the emoji
pickers of a shared file's message and an event's description), the keyboard picks only by a search and Enter. The
arrow keys do not move from the search field into the list, and not among the emoji. The All and Stickers tabs of
the compose box's picker have this navigation (`FunPicker::items_key`).

To do: compare with Teams' emoji picker in the browser (lab account, screenshots). Then let Down from the search field
go into the list, the arrows move among the cells and the section tabs, and Enter or Space pick, in each place that
shows the list. The review of task 653 (round 1) found that the Emoji tab does not have it.
