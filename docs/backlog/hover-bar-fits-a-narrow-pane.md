---
worth: later
status: open
where: crates/teams-ui/src/chat_view.rs (hover_bar, bar_buttons, hidden_entries)
added: 2026-11-22
---
# The hover bar does not fit a narrow message pane

The message hover bar puts every button that the user shows in one row, and the row has no width limit. In a narrow
pane (a channel's thread open beside the posts at the default window width), the default bar only just fits. Each
button that the user adds makes the bar approximately 30 px wider. The message list clips the part that goes past
its edge: on the account's own message the first quick reactions, on someone else's message the buttons at the end,
"More options" too. The keys of the clipped buttons still work.

To do: compare with Teams in the browser in a narrow pane (lab account, screenshots). Then measure the space that the
bar has, and move the buttons that do not fit, from the end of the row, into "More options", so that the bar and the
menu agree on what each one holds. The review of task 712 (round 1) found it.
